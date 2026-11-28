//! Vetro Android client core, built as a `cdylib` for the Kotlin app and as an `rlib` for x86 tests.
//!
//! Skeleton; the session code is implemented in M1-5 (ARCHITECTURE section 6). Discovery
//! (mDNS browse, M1-13) is in [`discovery`].

pub mod discovery;
