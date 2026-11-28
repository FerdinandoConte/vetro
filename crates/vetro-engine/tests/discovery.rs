//! The engine's advertisement is found by the client's browser, and gone after the engine
//! withdraws it. Real multicast, but engine and browser are on the same host: it proves the
//! records and the code paths, not that another device on the LAN receives them. Reach from
//! the tablet (including the Windows firewall profile) belongs to the M1 manual checklist.
//! Needs a network interface with multicast: run with `--ignored`.

use std::time::{Duration, Instant};
use vetro_client::discovery::{Browser, HostEvent};
use vetro_engine::discovery::{Advertiser, lan_ipv4s};
use vetro_transport::Identity;

const WAIT: Duration = Duration::from_secs(15);

#[test]
#[ignore = "needs working multicast on a real LAN interface (CI runners and sandboxes often have none)"]
fn client_browse_finds_the_engine_and_sees_it_leave() {
    let lan = lan_ipv4s();
    assert!(
        !lan.is_empty(),
        "no LAN IPv4 interface: connect to a network"
    );
    // A port nothing listens on: discovery only reads the record.
    let port = 47_811;
    let fp = Identity::generate().unwrap().fingerprint();

    let browser = Browser::start().unwrap();
    let advertiser = Advertiser::start(&lan, port, fp).unwrap();

    let until = Instant::now() + WAIT;
    let host = loop {
        let left = until.saturating_duration_since(Instant::now());
        match browser.next_event(left) {
            // Other Vetro engines on the LAN may answer too: take ours by fingerprint.
            Some(HostEvent::Found(h)) if h.fingerprint == fp.0 => break h,
            Some(_) => {}
            None => panic!("engine not found within {WAIT:?}"),
        }
    };
    assert_eq!(host.port, port);
    assert!(
        host.addrs.iter().all(|a| lan.contains(a)),
        "advertised {:?}, expected a subset of {lan:?}",
        host.addrs
    );

    drop(advertiser); // withdraws
    let until = Instant::now() + WAIT;
    loop {
        let left = until.saturating_duration_since(Instant::now());
        match browser.next_event(left) {
            Some(HostEvent::Lost { name }) if name == host.name => break,
            Some(_) => {}
            None => panic!("withdrawal not seen within {WAIT:?}"),
        }
    }
}
