//! Command line and on-disk paths (ARCHITECTURE sections 5.5 and 8).

use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::PathBuf;

/// Default listening address: every IPv4 interface (Wi-Fi and USB tethering), fixed port so a
/// paired tablet finds the engine again after a restart. mDNS (M1-13) advertises it too, with
/// A records only (this socket is IPv4-only). Why all interfaces is acceptable: ARCHITECTURE
/// section 8, "Listening address".
pub const DEFAULT_BIND: &str = "0.0.0.0:47800";

/// Encoder bitrate until delay-based rate control exists.
/// shortcut: fixed 50 Mbit/s, replaced by rate control from `LinkFeedback` in M1-7.
pub const DEFAULT_BITRATE_KBPS: u32 = 50_000;

pub const USAGE: &str = "\
usage: vetro-engine --no-vdd [--test-pattern moving-bars] [--bind <addr:port>]
                    [--telemetry-out <file.jsonl>] [--pair]

  --no-vdd                  synthetic source, no virtual display driver (required until M1-8)
  --test-pattern <name>     synthetic pattern: moving-bars (default)
  --bind <addr:port>        listening address (default 0.0.0.0:47800)
  --telemetry-out <file>    write one JSON line per sent frame (host clock, microseconds)
  --pair                    open a pairing window and print the pairing URI on stdout
                            (what the tray's QR code will show, M1-12); the address in it is
                            the --bind address, or the first LAN IPv4 address of this PC

The engine announces itself on the LAN as _vetro._udp (mDNS) when it has a LAN IPv4 address.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestPattern {
    MovingBars,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub bind: SocketAddr,
    pub pattern: TestPattern,
    pub telemetry_out: Option<PathBuf>,
    pub pair: bool,
    pub bitrate_kbps: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bind: DEFAULT_BIND.parse().expect("valid default address"),
            pattern: TestPattern::MovingBars,
            telemetry_out: None,
            pair: false,
            bitrate_kbps: DEFAULT_BITRATE_KBPS,
        }
    }
}

/// Parses the arguments after the program name. `Ok(None)` means `--help`.
pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Option<Config>, String> {
    let mut cfg = Config::default();
    let mut no_vdd = false;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let flag = arg
            .to_str()
            .ok_or_else(|| format!("unknown argument {arg:?}"))?;
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        match flag {
            "-h" | "--help" => return Ok(None),
            "--no-vdd" => no_vdd = true,
            "--pair" => cfg.pair = true,
            "--test-pattern" => {
                cfg.pattern = match value(flag)?.to_str() {
                    Some("moving-bars") => TestPattern::MovingBars,
                    _ => return Err("--test-pattern: unknown pattern (known: moving-bars)".into()),
                }
            }
            "--bind" => {
                let v = value(flag)?;
                cfg.bind = v
                    .to_str()
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| format!("--bind: {v:?} is not an addr:port"))?;
            }
            "--telemetry-out" => cfg.telemetry_out = Some(value(flag)?.into()),
            _ => return Err(format!("unknown argument {flag:?}")),
        }
    }
    if !no_vdd {
        return Err("the virtual display arrives in M1-8; pass --no-vdd".into());
    }
    Ok(Some(cfg))
}

/// Where the host identity (certificate + unencrypted private key) lives.
///
/// Windows: `%LOCALAPPDATA%\Vetro\identity.bin`, protected by the per-user ACL of the profile
/// (never ProgramData or a temp directory, ARCHITECTURE section 8). Elsewhere (CI, a later Linux
/// host): `$XDG_DATA_HOME/vetro/identity.bin`, else `$HOME/.local/share/vetro/identity.bin`.
/// `env` reads an environment variable; an unset or relative base is an error, never a
/// fallback to a shared directory.
pub fn identity_path(env: impl Fn(&str) -> Option<OsString>) -> Result<PathBuf, String> {
    let abs = |key: &str| env(key).map(PathBuf::from).filter(|p| p.is_absolute());
    let dir = if cfg!(windows) {
        abs("LOCALAPPDATA")
            .ok_or("%LOCALAPPDATA% is not set to an absolute path")?
            .join("Vetro")
    } else {
        abs("XDG_DATA_HOME")
            .or_else(|| abs("HOME").map(|h| h.join(".local").join("share")))
            .ok_or("neither $XDG_DATA_HOME nor $HOME is an absolute path")?
            .join("vetro")
    };
    Ok(dir.join("identity.bin"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn args(s: &str) -> Vec<OsString> {
        s.split_whitespace().map(OsString::from).collect()
    }

    #[test]
    fn acceptance_command_line_parses_to_defaults() {
        let cfg = parse(args("--no-vdd --test-pattern moving-bars")).unwrap();
        assert_eq!(cfg, Some(Config::default()));
    }

    #[test]
    fn every_flag_is_read() {
        let cfg = parse(args(
            "--pair --bind 127.0.0.1:0 --telemetry-out t.jsonl --no-vdd",
        ))
        .unwrap()
        .unwrap();
        assert_eq!(cfg.bind, "127.0.0.1:0".parse().unwrap());
        assert_eq!(cfg.telemetry_out.as_deref(), Some(Path::new("t.jsonl")));
        assert!(cfg.pair);
        assert!(parse(args("--bind [::1]:9")).unwrap_err().contains("M1-8"));
    }

    #[test]
    fn help_and_errors() {
        assert_eq!(parse(args("--help")), Ok(None));
        assert_eq!(parse(args("--no-vdd -h")), Ok(None));
        for bad in [
            "",
            "--test-pattern moving-bars",
            "--no-vdd --test-pattern stripes",
            "--no-vdd --test-pattern",
            "--no-vdd --bind",
            "--no-vdd --bind localhost",
            "--no-vdd --bind 1.2.3.4",
            "--no-vdd --telemetry-out",
            "--no-vdd --fast",
            "--no-vdd extra",
        ] {
            assert!(parse(args(bad)).is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn pair_without_bind_is_accepted() {
        // The URI gets a LAN address chosen at run time (`discovery::pair_ip`).
        let cfg = parse(args("--no-vdd --pair")).unwrap().unwrap();
        assert!(cfg.pair && cfg.bind.ip().is_unspecified());
    }

    #[test]
    fn identity_lives_in_the_per_user_data_directory() {
        let base = if cfg!(windows) {
            r"C:\Users\u\AppData\Local"
        } else {
            "/home/u/.data"
        };
        let env = |k: &str| (k == "LOCALAPPDATA" || k == "XDG_DATA_HOME").then(|| base.into());
        let expected = if cfg!(windows) {
            Path::new(base).join("Vetro").join("identity.bin")
        } else {
            Path::new(base).join("vetro").join("identity.bin")
        };
        assert_eq!(identity_path(env).unwrap(), expected);
    }

    #[test]
    fn identity_path_never_falls_back() {
        assert!(identity_path(|_| None).is_err());
        assert!(identity_path(|_| Some("relative".into())).is_err());
    }

    #[cfg(not(windows))]
    #[test]
    fn identity_path_uses_home_without_xdg() {
        let env = |k: &str| (k == "HOME").then(|| "/home/u".into());
        assert_eq!(
            identity_path(env).unwrap(),
            Path::new("/home/u/.local/share/vetro/identity.bin")
        );
    }
}
