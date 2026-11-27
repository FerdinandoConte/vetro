//! Long-term identity of one side: a self-signed certificate and its private key (ADR-0008).

use crate::Error;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use std::fmt;
use std::io::{Read, Write};
use std::path::Path;

/// SHA-256 of a certificate's DER bytes: what pairing pins. `Display` is lowercase hex.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Fingerprint(pub [u8; 32]);

impl Fingerprint {
    pub fn of(cert_der: &[u8]) -> Self {
        let digest = ring::digest::digest(&ring::digest::SHA256, cert_der);
        let mut out = [0u8; 32];
        out.copy_from_slice(digest.as_ref());
        Self(out)
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|b| write!(f, "{b:02x}"))
    }
}

impl fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fingerprint({self})")
    }
}

/// Certificate + private key. `Debug` prints only the fingerprint, never the key.
pub struct Identity {
    cert: CertificateDer<'static>,
    key: PrivatePkcs8KeyDer<'static>,
}

impl Clone for Identity {
    fn clone(&self) -> Self {
        Self {
            cert: self.cert.clone(),
            key: self.key.clone_key(),
        }
    }
}

impl fmt::Debug for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Identity")
            .field("fingerprint", &self.fingerprint())
            .field("key", &"<redacted>")
            .finish()
    }
}

/// File header of a stored identity, version 1.
const MAGIC: &[u8; 8] = b"VETROID\x01";
/// Upper bound of each DER blob in the identity file (an ECDSA P-256 cert/key is under 1 KiB).
const MAX_DER: usize = 16 * 1024;
const MAX_FILE: usize = MAGIC.len() + 4 + 2 * MAX_DER;

impl Identity {
    /// A fresh ECDSA P-256 key pair and a self-signed certificate. Validity dates are irrelevant:
    /// peers pin the fingerprint and never check them.
    pub fn generate() -> Result<Self, Error> {
        let ck = rcgen::generate_simple_self_signed(vec!["vetro".to_owned()])
            .map_err(|_| Error::Identity("certificate generation failed"))?;
        Ok(Self {
            cert: ck.cert.der().clone(),
            key: PrivatePkcs8KeyDer::from(ck.signing_key.serialize_der()),
        })
    }

    pub fn fingerprint(&self) -> Fingerprint {
        Fingerprint::of(&self.cert)
    }

    /// Loads the identity stored at `path`, or creates one there if the file does not exist.
    ///
    /// Writes nothing but `path` (created new, owner-only on Unix; on Windows the caller picks a
    /// per-user directory). A file that exists but does not parse is an error, never replaced:
    /// replacing it would silently change the fingerprint and break every pairing.
    pub fn load_or_generate(path: &Path) -> Result<Self, Error> {
        match std::fs::File::open(path) {
            Ok(f) => {
                let mut bytes = Vec::new();
                f.take(MAX_FILE as u64 + 1).read_to_end(&mut bytes)?;
                Self::from_bytes(&bytes)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let id = Self::generate()?;
                let mut opts = std::fs::OpenOptions::new();
                opts.write(true).create_new(true);
                #[cfg(unix)]
                std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
                let mut f = opts.open(path)?;
                let written = f.write_all(&id.to_bytes()).and_then(|()| f.sync_all());
                if let Err(e) = written {
                    drop(f);
                    // Nothing was paired with this identity yet, so a half-written file can go.
                    let _ = std::fs::remove_file(path);
                    return Err(e.into());
                }
                Ok(id)
            }
            Err(e) => Err(e.into()),
        }
    }

    /// `MAGIC | cert_len u32 LE | cert DER | PKCS#8 key DER`.
    fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(MAGIC.len() + 4 + self.cert.len() + 64);
        out.extend_from_slice(MAGIC);
        // Both blobs come from rcgen and are far below MAX_DER, so the cast is lossless.
        out.extend_from_slice(&(self.cert.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.cert);
        out.extend_from_slice(self.key.secret_pkcs8_der());
        out
    }

    fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        let bad = || Error::Identity("identity file is corrupt; delete it and pair again");
        if bytes.len() > MAX_FILE {
            return Err(bad());
        }
        let rest = bytes.strip_prefix(MAGIC.as_slice()).ok_or_else(bad)?;
        let (len, rest) = rest.split_first_chunk::<4>().ok_or_else(bad)?;
        let cert_len = u32::from_le_bytes(*len) as usize;
        if cert_len == 0 || cert_len > MAX_DER || rest.len() <= cert_len {
            return Err(bad());
        }
        let (cert, key) = rest.split_at(cert_len);
        if key.len() > MAX_DER {
            return Err(bad());
        }
        let id = Self {
            cert: CertificateDer::from(cert.to_vec()),
            key: PrivatePkcs8KeyDer::from(key.to_vec()),
        };
        // The key must parse and belong to the certificate.
        let provider = rustls::crypto::ring::default_provider();
        let signer = provider
            .key_provider
            .load_private_key(id.key())
            .map_err(|_| Error::Identity("identity file holds an unusable key"))?;
        rustls::sign::CertifiedKey::new(vec![id.cert.clone()], signer)
            .keys_match()
            .map_err(|_| Error::Identity("identity file key does not match its certificate"))?;
        Ok(id)
    }

    pub(crate) fn cert(&self) -> CertificateDer<'static> {
        self.cert.clone()
    }

    pub(crate) fn key(&self) -> PrivateKeyDer<'static> {
        PrivateKeyDer::Pkcs8(self.key.clone_key())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn bytes_roundtrip_keeps_the_fingerprint() {
        let id = Identity::generate().unwrap();
        let back = Identity::from_bytes(&id.to_bytes()).unwrap();
        assert_eq!(back.fingerprint(), id.fingerprint());
        assert_eq!(back.key.secret_pkcs8_der(), id.key.secret_pkcs8_der());
    }

    #[test]
    fn debug_never_prints_the_key() {
        let id = Identity::generate().unwrap();
        let dbg = format!("{id:?}");
        assert!(dbg.contains("<redacted>") && dbg.contains(&id.fingerprint().to_string()));
        let key_hex: String = id.key.secret_pkcs8_der()[..16]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert!(!dbg.contains(&key_hex));
    }

    #[test]
    fn a_key_from_another_identity_is_rejected() {
        let a = Identity::generate().unwrap();
        let b = Identity::generate().unwrap();
        let mixed = Identity {
            cert: a.cert.clone(),
            key: b.key.clone_key(),
        };
        assert!(Identity::from_bytes(&mixed.to_bytes()).is_err());
    }

    #[test]
    fn load_or_generate_creates_once_then_loads() {
        let dir = std::env::temp_dir().join(format!("vetro-id-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("identity.bin");
        let _ = std::fs::remove_file(&path);
        let first = Identity::load_or_generate(&path).unwrap();
        let second = Identity::load_or_generate(&path).unwrap();
        assert_eq!(first.fingerprint(), second.fingerprint());
        // Only the given path was written.
        let files: Vec<_> = std::fs::read_dir(&dir).unwrap().collect();
        assert_eq!(files.len(), 1);
        std::fs::write(&path, b"garbage").unwrap();
        assert!(Identity::load_or_generate(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"garbage");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    proptest! {
        #[test]
        fn arbitrary_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..2048)) {
            let _ = Identity::from_bytes(&bytes);
        }

        #[test]
        fn arbitrary_bytes_after_the_magic_never_panic(
            len in any::<u32>(),
            body in proptest::collection::vec(any::<u8>(), 0..2048),
        ) {
            let mut bytes = MAGIC.to_vec();
            bytes.extend_from_slice(&len.to_le_bytes());
            bytes.extend_from_slice(&body);
            prop_assert!(Identity::from_bytes(&bytes).is_err());
        }

        #[test]
        fn fingerprint_display_is_64_hex_digits(bytes in any::<[u8; 32]>()) {
            let s = Fingerprint(bytes).to_string();
            prop_assert_eq!(s.len(), 64);
            prop_assert!(s.bytes().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        }
    }
}
