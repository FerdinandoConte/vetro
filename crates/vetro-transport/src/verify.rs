//! Certificate verifiers: pinning by fingerprint on both sides, no WebPKI (ADR-0008).
//!
//! The certificate is only an envelope for the public key: its issuer, names and validity dates
//! are ignored. What counts is (1) the SHA-256 of the certificate matches a pin, and (2) the
//! peer's TLS 1.3 `CertificateVerify` signature verifies against that certificate's key, which
//! proves the peer holds the private key. (2) is delegated to rustls' own implementation.

use crate::Fingerprint;
use crate::pairing::Trust;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{CertificateError, DigitallySignedStruct, DistinguishedName, SignatureScheme};
use std::sync::Arc;
use std::time::Instant;

fn rejected() -> rustls::Error {
    rustls::Error::InvalidCertificate(CertificateError::ApplicationVerificationFailure)
}

fn tls12_refused() -> rustls::Error {
    // QUIC is TLS 1.3 only and the configs allow nothing else; refuse rather than half-support.
    rustls::Error::General("TLS 1.2 is not supported".into())
}

/// Client side: accepts exactly one host certificate, the one whose fingerprint came from the QR.
pub(crate) struct HostPin {
    host: Fingerprint,
    algs: WebPkiSupportedAlgorithms,
}

impl HostPin {
    pub(crate) fn new(host: Fingerprint, provider: &CryptoProvider) -> Self {
        Self {
            host,
            algs: provider.signature_verification_algorithms,
        }
    }
}

impl core::fmt::Debug for HostPin {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "HostPin({})", self.host)
    }
}

impl ServerCertVerifier for HostPin {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if intermediates.is_empty() && Fingerprint::of(end_entity) == self.host {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rejected())
        }
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(tls12_refused())
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.algs)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algs.supported_schemes()
    }
}

/// Host side: a client certificate passes the handshake if its fingerprint is paired, or, only
/// while a pairing token is live, provisionally; the session setup then demands the token in
/// `Hello` before anything else is read. With no live token an unpaired device never gets past
/// the handshake.
pub(crate) struct ClientPins {
    trust: Arc<Trust>,
    algs: WebPkiSupportedAlgorithms,
}

impl ClientPins {
    pub(crate) fn new(trust: Arc<Trust>, provider: &CryptoProvider) -> Self {
        Self {
            trust,
            algs: provider.signature_verification_algorithms,
        }
    }
}

impl core::fmt::Debug for ClientPins {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("ClientPins")
    }
}

impl ClientCertVerifier for ClientPins {
    fn offer_client_auth(&self) -> bool {
        true
    }

    fn client_auth_mandatory(&self) -> bool {
        true
    }

    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        let fp = Fingerprint::of(end_entity);
        if intermediates.is_empty()
            && (self.trust.is_paired(&fp) || self.trust.pairing_open(Instant::now()))
        {
            Ok(ClientCertVerified::assertion())
        } else {
            Err(rejected())
        }
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(tls12_refused())
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.algs)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algs.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Identity;

    fn provider() -> CryptoProvider {
        rustls::crypto::ring::default_provider()
    }

    fn now() -> UnixTime {
        UnixTime::now()
    }

    #[test]
    fn host_pin_accepts_only_the_pinned_certificate() {
        let host = Identity::generate().unwrap();
        let other = Identity::generate().unwrap();
        let pin = HostPin::new(host.fingerprint(), &provider());
        let name = ServerName::try_from("vetro").unwrap();
        assert!(
            pin.verify_server_cert(&host.cert(), &[], &name, &[], now())
                .is_ok()
        );
        // Wrong fingerprint.
        assert!(
            pin.verify_server_cert(&other.cert(), &[], &name, &[], now())
                .is_err()
        );
        // The pinned cert with an unexpected chain.
        assert!(
            pin.verify_server_cert(&host.cert(), &[other.cert()], &name, &[], now())
                .is_err()
        );
    }

    #[test]
    fn client_pins_reject_an_unpaired_client() {
        let client = Identity::generate().unwrap();
        let pins = ClientPins::new(Arc::new(Trust::default()), &provider());
        assert!(pins.verify_client_cert(&client.cert(), &[], now()).is_err());
        assert!(pins.client_auth_mandatory());
    }

    #[test]
    fn client_pins_accept_a_paired_client() {
        let client = Identity::generate().unwrap();
        let trust = Arc::new(Trust::new([client.fingerprint()]));
        let pins = ClientPins::new(trust.clone(), &provider());
        assert!(pins.verify_client_cert(&client.cert(), &[], now()).is_ok());
        // Unpairing revokes at the next handshake.
        trust.unpair(&client.fingerprint());
        assert!(pins.verify_client_cert(&client.cert(), &[], now()).is_err());
    }

    #[test]
    fn client_pins_let_an_unknown_client_through_only_while_a_token_is_live() {
        let client = Identity::generate().unwrap();
        let trust = Arc::new(Trust::default());
        let pins = ClientPins::new(trust.clone(), &provider());
        let token = trust.start_pairing(Instant::now()).unwrap();
        assert!(pins.verify_client_cert(&client.cert(), &[], now()).is_ok());
        // Once the token is used by someone, the window is closed for everyone else.
        let somebody = Identity::generate().unwrap();
        assert!(trust.redeem(&token, somebody.fingerprint(), Instant::now()));
        assert!(pins.verify_client_cert(&client.cert(), &[], now()).is_err());
    }

    #[test]
    fn our_own_key_type_is_a_supported_scheme() {
        let host = Identity::generate().unwrap();
        let pin = HostPin::new(host.fingerprint(), &provider());
        assert!(
            pin.supported_verify_schemes()
                .contains(&SignatureScheme::ECDSA_NISTP256_SHA256)
        );
    }
}
