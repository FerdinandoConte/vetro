//! Finding hosts on the LAN: browsing `_vetro._udp` (ARCHITECTURE section 6.2).
//!
//! Discovery only suggests where to connect. A connection always goes through the pinned TLS
//! handshake, so a forged advertisement can at worst waste a connection attempt. The manual
//! fallback needs no discovery at all: connect to the address typed in, or taken from the
//! pairing QR (`a=`, `p=`).
//!
//! Platform-neutral: on Android the app must hold a `WifiManager.MulticastLock` while a
//! [`Browser`] exists (section 6.1), or the radio filters the multicast replies.

use mdns_sd::{IfKind, ResolvedService, ServiceDaemon, ServiceEvent, instance_name_unescaped};
use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;
use vetro_protocol::discovery::{HostAdvert, SERVICE_TYPE};
use vetro_protocol::version::PROTOCOL_VERSION;

/// A host found by mDNS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredHost {
    /// Instance name (the PC's name), for display.
    pub name: String,
    /// SHA-256 of the host certificate, as claimed by an unauthenticated advertisement: a hint
    /// for matching the pins already stored (see [`paired_hosts`]). Never pin it for a new
    /// host: the pairing QR is the only trusted source of a fingerprint.
    pub fingerprint: [u8; 32],
    /// IPv4 addresses, sorted. Never empty. The engine listens on IPv4 only.
    pub addrs: Vec<Ipv4Addr>,
    pub port: u16,
}

impl DiscoveredHost {
    /// Addresses to try, in order.
    pub fn socket_addrs(&self) -> impl Iterator<Item = SocketAddr> + '_ {
        self.addrs
            .iter()
            .map(|ip| SocketAddr::new((*ip).into(), self.port))
    }

    /// `Some` when `resolved` is a usable host of our protocol major: a valid TXT record, at
    /// least one IPv4 address and a port.
    fn from_resolved(resolved: &ResolvedService) -> Option<Self> {
        let txt = resolved.get_properties().iter().map(|p| (p.key(), p.val()));
        let advert = HostAdvert::parse(txt).ok()?;
        let mut addrs: Vec<Ipv4Addr> = resolved.get_addresses_v4().into_iter().collect();
        addrs.sort();
        (advert.major == PROTOCOL_VERSION.major && !addrs.is_empty() && resolved.get_port() != 0)
            .then(|| Self {
                name: instance_name_unescaped(resolved.get_fullname()),
                fingerprint: advert.fingerprint,
                addrs,
                port: resolved.get_port(),
            })
    }
}

/// The hosts among `found` whose certificate is one of `paired` (the fingerprints the app
/// stored at pairing): connect to these without asking.
pub fn paired_hosts<'a>(
    found: &'a [DiscoveredHost],
    paired: &'a [[u8; 32]],
) -> impl Iterator<Item = &'a DiscoveredHost> {
    found.iter().filter(|h| paired.contains(&h.fingerprint))
}

/// What a [`Browser`] reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostEvent {
    /// A host appeared or its record changed (the same host can be reported again).
    Found(DiscoveredHost),
    /// The host named `name` said goodbye or timed out.
    Lost { name: String },
}

/// A running browse. Dropping it stops the search.
pub struct Browser {
    daemon: ServiceDaemon,
    rx: mdns_sd::Receiver<ServiceEvent>,
}

impl Browser {
    /// Starts browsing. Needs multicast: see the module docs for Android.
    pub fn start() -> Result<Self, mdns_sd::Error> {
        let daemon = ServiceDaemon::new()?;
        // The engine publishes A records only.
        daemon.disable_interface(IfKind::IPv6)?;
        let rx = daemon.browse(SERVICE_TYPE)?;
        Ok(Self { daemon, rx })
    }

    /// Waits up to `timeout` for the next event; `None` when nothing happened (or the daemon
    /// stopped). Records that are not a valid host of this protocol major are skipped.
    pub fn next_event(&self, timeout: Duration) -> Option<HostEvent> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let left = deadline.checked_duration_since(std::time::Instant::now())?;
            match self.rx.recv_timeout(left).ok()? {
                ServiceEvent::ServiceResolved(r) => {
                    if let Some(h) = DiscoveredHost::from_resolved(&r) {
                        return Some(HostEvent::Found(h));
                    }
                }
                ServiceEvent::ServiceRemoved(_, fullname) => {
                    return Some(HostEvent::Lost {
                        name: instance_name_unescaped(&fullname),
                    });
                }
                _ => {}
            }
        }
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        let _ = self.daemon.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mdns_sd::ServiceInfo;

    fn resolved(major: u16, ips: &str, port: u16, extra: &[(&str, &str)]) -> ResolvedService {
        let advert = HostAdvert {
            major,
            fingerprint: [7; 32],
        };
        let mut txt: Vec<(&str, String)> = advert.to_txt().into();
        txt.extend(extra.iter().map(|(k, v)| (*k, v.to_string())));
        ServiceInfo::new(SERVICE_TYPE, "My PC", "my-pc.local.", ips, port, &txt[..])
            .unwrap()
            .as_resolved_service()
    }

    #[test]
    fn a_valid_record_becomes_a_host() {
        let h = DiscoveredHost::from_resolved(&resolved(1, "192.168.1.9,10.0.0.2", 47800, &[]))
            .unwrap();
        assert_eq!(h.name, "My PC");
        assert_eq!(h.fingerprint, [7; 32]);
        assert_eq!(h.port, 47800);
        let addrs: Vec<String> = h.socket_addrs().map(|a| a.to_string()).collect();
        assert_eq!(addrs, ["10.0.0.2:47800", "192.168.1.9:47800"]);
    }

    #[test]
    fn unusable_records_are_skipped() {
        let skip = |r: ResolvedService| assert_eq!(DiscoveredHost::from_resolved(&r), None);
        skip(resolved(2, "192.168.1.9", 47800, &[])); // other protocol major
        skip(resolved(1, "fe80::1", 47800, &[])); // no IPv4 address
        skip(resolved(1, "192.168.1.9", 0, &[])); // no port
        let no_fp = ServiceInfo::new(
            SERVICE_TYPE,
            "x",
            "x.local.",
            "192.168.1.9",
            1,
            &[("v", "1")][..],
        )
        .unwrap()
        .as_resolved_service();
        skip(no_fp);
    }

    #[test]
    fn matches_a_paired_fingerprint() {
        let host = |fp: u8| DiscoveredHost {
            name: format!("pc{fp}"),
            fingerprint: [fp; 32],
            addrs: vec![Ipv4Addr::LOCALHOST],
            port: 1,
        };
        let found = [host(1), host(2), host(3)];
        let names: Vec<&str> = paired_hosts(&found, &[[3; 32], [1; 32], [9; 32]])
            .map(|h| h.name.as_str())
            .collect();
        assert_eq!(names, ["pc1", "pc3"]);
        assert_eq!(paired_hosts(&found, &[]).count(), 0);
    }
}
