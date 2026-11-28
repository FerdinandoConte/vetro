//! LAN discovery of the engine: which addresses to give out, and the mDNS advertisement
//! (ARCHITECTURE section 5.5, "Discovery").
//!
//! The listening socket is IPv4-only (section 8, "Listening address"), so only A records are
//! published: an AAAA record would point the client at an address nothing listens on.

use if_addrs::IfAddr;
use mdns_sd::{IfKind, ServiceDaemon, ServiceInfo};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;
use vetro_protocol::discovery::{HostAdvert, SERVICE_TYPE};
use vetro_protocol::version::PROTOCOL_VERSION;
use vetro_transport::Fingerprint;

/// One IPv4 address of one network interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Nic {
    pub name: String,
    pub ip: Ipv4Addr,
    pub up: bool,
}

/// Name fragments of adapters a tablet cannot be on: hypervisor and container switches, VPN and
/// tunnel pseudo-interfaces. Matched case-insensitively. USB tethering (an RNDIS/NCM
/// "Ethernet N" adapter) must never match.
/// shortcut: name heuristics only; Windows' interface type (IfType) and the physical-media flag
/// would be exact but need `unsafe` FFI, which this crate forbids. Revisit if an owner's
/// machine advertises the wrong adapter first.
const VIRTUAL_NAMES: &[&str] = &[
    "vethernet",
    "virtualbox",
    "vmware",
    "vmnet",
    "vboxnet",
    "hyper-v",
    "docker",
    "wsl",
    "loopback",
    "pseudo",
    "isatap",
    "teredo",
    "tailscale",
    "zerotier",
    "vpn",
];
const VIRTUAL_PREFIXES: &[&str] = &["veth", "br-", "virbr", "tun", "tap", "utun"];

/// The addresses a tablet on the LAN could dial: IPv4, interface up, not loopback, not
/// link-local (169.254/16, no DHCP answer) and not a virtual adapter. Private ranges
/// (RFC 1918) come first, each group in the system's order.
pub fn suitable(nics: &[Nic]) -> Vec<Ipv4Addr> {
    let mut out: Vec<Ipv4Addr> = Vec::new();
    for n in nics {
        let name = n.name.to_ascii_lowercase();
        let virt = name == "lo"
            || VIRTUAL_NAMES.iter().any(|s| name.contains(s))
            || VIRTUAL_PREFIXES.iter().any(|s| name.starts_with(s));
        let ok = n.up
            && !virt
            && !n.ip.is_loopback()
            && !n.ip.is_link_local()
            && !n.ip.is_unspecified()
            && !n.ip.is_multicast()
            && !n.ip.is_broadcast();
        if ok && !out.contains(&n.ip) {
            out.push(n.ip);
        }
    }
    out.sort_by_key(|ip| !ip.is_private()); // stable
    out
}

/// The machine's interfaces now (IPv4 only).
pub fn interfaces() -> std::io::Result<Vec<Nic>> {
    Ok(if_addrs::get_if_addrs()?
        .into_iter()
        .filter_map(|i| match i.addr {
            IfAddr::V4(ref a) => Some(Nic {
                up: i.is_oper_up(),
                ip: a.ip,
                name: i.name,
            }),
            IfAddr::V6(_) => None,
        })
        .collect())
}

/// [`suitable`] over the machine's interfaces now.
pub fn lan_ipv4s() -> Vec<Ipv4Addr> {
    interfaces().map(|n| suitable(&n)).unwrap_or_default()
}

/// The addresses to advertise, and the first of them goes into the pairing URI, for a socket
/// bound at `bound`: every suitable address (`lan`) for a wildcard bind, the bound address
/// itself when it is one of them (the owner chose it), nothing otherwise (loopback, IPv6, an
/// address no real interface owns).
pub fn dial_addrs(bound: SocketAddr, lan: &[Ipv4Addr]) -> Vec<Ipv4Addr> {
    match bound.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => lan.to_vec(),
        IpAddr::V4(ip) if lan.contains(&ip) => vec![ip],
        _ => Vec::new(),
    }
}

/// The address for the pairing URI's `a=`: the bound address when the owner chose one with
/// `--bind` (even loopback, for a test client on the same machine); for a wildcard bind the
/// first of `lan`; `None` when there is none.
pub fn pair_ip(bound: SocketAddr, lan: &[Ipv4Addr]) -> Option<IpAddr> {
    if bound.ip().is_unspecified() {
        lan.first().map(|ip| IpAddr::V4(*ip))
    } else {
        Some(bound.ip())
    }
}

/// The computer's name, for the instance name shown on the tablet.
fn computer_name() -> String {
    ["COMPUTERNAME", "HOSTNAME"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.trim().is_empty()))
        .unwrap_or_else(|| "PC".into())
}

/// DNS label of a name: ASCII letters, digits and hyphens, at most 63 characters.
fn host_label(name: &str) -> String {
    let l: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .take(63)
        .collect();
    let l = l.trim_matches('-');
    if l.is_empty() {
        "vetro".into()
    } else {
        l.into()
    }
}

/// A running advertisement.
pub struct Advertiser {
    daemon: ServiceDaemon,
    fullname: String,
}

impl Advertiser {
    /// Publishes `_vetro._udp.local.` on `port` with an A record per address of `addrs`,
    /// only on the interfaces that own them. TXT: the protocol major and `fingerprint`, never
    /// a secret. `addrs` must not be empty.
    /// shortcut: the address list is fixed at start; a network change (Wi-Fi roam, tethering
    /// plugged in) needs a restart until the tray re-registers on change (M1-12).
    pub fn start(
        addrs: &[Ipv4Addr],
        port: u16,
        fingerprint: Fingerprint,
    ) -> Result<Self, mdns_sd::Error> {
        let name = computer_name();
        let advert = HostAdvert {
            major: PROTOCOL_VERSION.major,
            fingerprint: fingerprint.0,
        };
        let txt: Vec<(&str, String)> = advert.to_txt().into();
        let ips: Vec<IpAddr> = addrs.iter().map(|a| IpAddr::V4(*a)).collect();
        let mut info = ServiceInfo::new(
            SERVICE_TYPE,
            &name,
            &format!("{}.local.", host_label(&name)),
            &ips[..],
            port,
            &txt[..],
        )?;
        info.set_interfaces(addrs.iter().map(|a| IfKind::Addr((*a).into())).collect());
        let fullname = info.get_fullname().to_string();
        let daemon = ServiceDaemon::new()?;
        daemon.disable_interface(IfKind::IPv6)?;
        daemon.register(info)?;
        Ok(Self { daemon, fullname })
    }

    /// Sends the goodbye and stops the daemon thread; gives up after one second each.
    pub fn stop(self) {
        let wait = Duration::from_secs(1);
        if let Ok(rx) = self.daemon.unregister(&self.fullname) {
            let _ = rx.recv_timeout(wait);
        }
        if let Ok(rx) = self.daemon.shutdown() {
            let _ = rx.recv_timeout(wait);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nic(name: &str, ip: &str, up: bool) -> Nic {
        Nic {
            name: name.into(),
            ip: ip.parse().unwrap(),
            up,
        }
    }

    #[test]
    fn filters_what_a_tablet_cannot_reach() {
        let nics = [
            nic("Loopback Pseudo-Interface 1", "127.0.0.1", true),
            nic("lo", "127.0.0.1", true),
            nic("Wi-Fi", "169.254.10.2", true), // no DHCP answer
            nic("vEthernet (WSL)", "172.28.0.1", true),
            nic("VirtualBox Host-Only Network", "192.168.56.1", true),
            nic("VMware Network Adapter VMnet8", "192.168.17.1", true),
            nic("docker0", "172.17.0.1", true),
            nic("Tailscale", "100.101.102.103", true),
            nic("Ethernet 2", "192.168.1.9", false), // cable out
            nic("Wi-Fi", "0.0.0.0", true),
            nic("Wi-Fi", "224.0.0.251", true),
        ];
        assert!(suitable(&nics).is_empty());
    }

    #[test]
    fn keeps_wifi_and_usb_tethering_private_first_without_duplicates() {
        let nics = [
            nic("Ethernet 3", "100.64.0.9", true), // CGNAT range, still a real adapter
            nic("Wi-Fi", "192.168.1.20", true),
            nic("Wi-Fi", "192.168.1.20", true),
            nic(
                "Remote NDIS based Internet Sharing Device",
                "192.168.42.129",
                true,
            ),
            nic("Local Area Connection", "10.0.0.5", true), // starts with "lo", is not "lo"
        ];
        let ips: Vec<String> = suitable(&nics).iter().map(ToString::to_string).collect();
        assert_eq!(
            ips,
            ["192.168.1.20", "192.168.42.129", "10.0.0.5", "100.64.0.9"]
        );
    }

    #[test]
    fn dial_addrs_follow_the_bind() {
        let lan: Vec<Ipv4Addr> = vec!["192.168.1.20".parse().unwrap(), "10.0.0.5".parse().unwrap()];
        let at = |s: &str| dial_addrs(s.parse().unwrap(), &lan);
        assert_eq!(at("0.0.0.0:47800"), lan);
        assert_eq!(at("10.0.0.5:47800"), vec![lan[1]]);
        assert!(at("127.0.0.1:47800").is_empty());
        assert!(at("172.16.0.9:47800").is_empty()); // not an interface address we know
        assert!(at("[::]:47800").is_empty());
        assert!(dial_addrs("0.0.0.0:1".parse().unwrap(), &[]).is_empty());
    }

    #[test]
    fn pair_uri_address_is_the_bind_or_the_first_lan_address() {
        let lan: Vec<Ipv4Addr> = vec!["192.168.1.20".parse().unwrap(), "10.0.0.5".parse().unwrap()];
        let at = |s: &str, lan: &[Ipv4Addr]| pair_ip(s.parse().unwrap(), lan);
        assert_eq!(
            at("0.0.0.0:47800", &lan),
            Some("192.168.1.20".parse().unwrap())
        );
        assert_eq!(
            at("[::]:47800", &lan),
            Some("192.168.1.20".parse().unwrap())
        );
        assert_eq!(at("10.0.0.5:1", &lan), Some("10.0.0.5".parse().unwrap()));
        assert_eq!(at("127.0.0.1:1", &lan), Some("127.0.0.1".parse().unwrap()));
        assert_eq!(at("0.0.0.0:47800", &[]), None, "no interface: refuse");
    }

    #[test]
    fn host_label_is_a_dns_label() {
        assert_eq!(host_label("My PC (2)"), "My-PC--2");
        assert_eq!(host_label("---"), "vetro");
        assert_eq!(host_label(&"a".repeat(100)).len(), 63);
    }
}
