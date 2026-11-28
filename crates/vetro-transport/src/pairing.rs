//! Host trust state: the pinned client fingerprints and the one pending pairing token (ADR-0008).

use crate::{Error, Fingerprint};
use std::collections::HashSet;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};
use vetro_protocol::messages::PairToken;

/// How long a pairing token (the QR code) stays valid.
pub const PAIR_TOKEN_TTL: Duration = Duration::from_secs(5 * 60);

#[derive(Default)]
struct State {
    paired: HashSet<Fingerprint>,
    /// At most one token at a time: the one the tray currently shows.
    pending: Option<(PairToken, Instant)>,
}

impl State {
    /// The pending token if it has not expired; drops it when it has.
    fn live_token(&mut self, now: Instant) -> Option<&PairToken> {
        if self.pending.as_ref().is_some_and(|(_, exp)| now >= *exp) {
            self.pending = None;
        }
        self.pending.as_ref().map(|(t, _)| t)
    }
}

/// Shared by the TLS verifier (handshake) and the session setup (`Hello`).
#[derive(Default)]
pub(crate) struct Trust(Mutex<State>);

impl Trust {
    pub(crate) fn new(paired: impl IntoIterator<Item = Fingerprint>) -> Self {
        Self(Mutex::new(State {
            paired: paired.into_iter().collect(),
            pending: None,
        }))
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // The state stays consistent at every await-free step, so a poisoned lock is still valid.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn is_paired(&self, fp: &Fingerprint) -> bool {
        self.lock().paired.contains(fp)
    }

    /// Whether an unknown certificate may finish the TLS handshake: only while a token is live.
    /// The token itself is checked in `Hello`, after the handshake proved key possession.
    pub(crate) fn pairing_open(&self, now: Instant) -> bool {
        self.lock().live_token(now).is_some()
    }

    /// Issues a fresh random token valid for [`PAIR_TOKEN_TTL`]; any previous token dies.
    pub(crate) fn start_pairing(&self, now: Instant) -> Result<PairToken, Error> {
        let mut bytes = [0u8; 16];
        ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut bytes)
            .map_err(|_| Error::Identity("system random generator failed"))?;
        let token = PairToken(bytes);
        self.lock().pending = Some((token.clone(), now + PAIR_TOKEN_TTL));
        Ok(token)
    }

    /// Closes the pairing window: the live token, if any, stops working.
    pub(crate) fn stop_pairing(&self) {
        self.lock().pending = None;
    }

    /// Whether `token` equals the live token (constant-time `==`), without consuming it. The
    /// host checks this first and redeems only once the rest of the session setup succeeded.
    pub(crate) fn token_matches(&self, token: &PairToken, now: Instant) -> bool {
        self.lock().live_token(now) == Some(token)
    }

    /// Single use: if `token` equals the live token (constant-time `==`), consumes it and pins
    /// `fp`. A wrong token leaves the live one in place, so a LAN peer guessing cannot cancel
    /// the owner's pairing; 128 random bits make guessing hopeless anyway.
    pub(crate) fn redeem(&self, token: &PairToken, fp: Fingerprint, now: Instant) -> bool {
        let mut state = self.lock();
        if state.live_token(now) != Some(token) {
            return false;
        }
        state.pending = None;
        state.paired.insert(fp);
        true
    }

    pub(crate) fn unpair(&self, fp: &Fingerprint) -> bool {
        self.lock().paired.remove(fp)
    }

    pub(crate) fn paired(&self) -> Vec<Fingerprint> {
        self.lock().paired.iter().copied().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: Fingerprint = Fingerprint([1; 32]);
    const B: Fingerprint = Fingerprint([2; 32]);

    #[test]
    fn a_live_token_pairs_once() {
        let t = Trust::default();
        let now = Instant::now();
        let token = t.start_pairing(now).unwrap();
        assert!(t.pairing_open(now));
        assert!(t.redeem(&token, A, now));
        assert!(t.is_paired(&A));
        // Reused token: rejected, and the window is closed.
        assert!(!t.redeem(&token, B, now));
        assert!(!t.is_paired(&B));
        assert!(!t.pairing_open(now));
    }

    #[test]
    fn an_expired_token_is_rejected() {
        let t = Trust::default();
        let now = Instant::now();
        let token = t.start_pairing(now).unwrap();
        let later = now + PAIR_TOKEN_TTL;
        assert!(!t.pairing_open(later));
        assert!(!t.redeem(&token, A, later));
        assert!(!t.is_paired(&A));
    }

    #[test]
    fn a_token_just_before_expiry_still_works() {
        let t = Trust::default();
        let now = Instant::now();
        let token = t.start_pairing(now).unwrap();
        assert!(t.redeem(&token, A, now + PAIR_TOKEN_TTL - Duration::from_millis(1)));
    }

    #[test]
    fn a_wrong_token_is_rejected_and_keeps_the_live_one() {
        let t = Trust::default();
        let now = Instant::now();
        let token = t.start_pairing(now).unwrap();
        let mut wrong = token.0;
        wrong[15] ^= 1;
        assert!(!t.redeem(&PairToken(wrong), A, now));
        assert!(t.redeem(&token, A, now));
    }

    #[test]
    fn a_new_token_replaces_the_old_one() {
        let t = Trust::default();
        let now = Instant::now();
        let old = t.start_pairing(now).unwrap();
        let new = t.start_pairing(now).unwrap();
        assert_ne!(old, new);
        assert!(!t.redeem(&old, A, now));
        assert!(t.redeem(&new, A, now));
    }

    #[test]
    fn without_a_token_nothing_pairs() {
        let t = Trust::default();
        assert!(!t.pairing_open(Instant::now()));
        assert!(!t.redeem(&PairToken([0; 16]), A, Instant::now()));
    }

    #[test]
    fn checking_a_token_does_not_consume_it() {
        let t = Trust::default();
        let now = Instant::now();
        let token = t.start_pairing(now).unwrap();
        assert!(t.token_matches(&token, now));
        assert!(t.token_matches(&token, now));
        assert!(!t.token_matches(&PairToken([0; 16]), now));
        assert!(!t.token_matches(&token, now + PAIR_TOKEN_TTL));
        let token = t.start_pairing(now).unwrap();
        assert!(t.redeem(&token, A, now));
        assert!(!t.token_matches(&token, now));
    }

    #[test]
    fn stop_pairing_closes_the_window() {
        let t = Trust::default();
        let now = Instant::now();
        let token = t.start_pairing(now).unwrap();
        t.stop_pairing();
        assert!(!t.pairing_open(now));
        assert!(!t.redeem(&token, A, now));
    }

    #[test]
    fn unpair_revokes() {
        let t = Trust::new([A]);
        assert!(t.unpair(&A));
        assert!(!t.is_paired(&A));
        assert!(t.paired().is_empty());
    }
}
