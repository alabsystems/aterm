// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1: bind the bounded pending-head model to the real watch, including
//! independent harvest and the single-head retry after an unreachable hint.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime};

use aterm_spec::derive::atpkg_vendor_pending_check_model;
use atpkg::activate::atomic_symlink;
use atpkg::flow::{VendorFetchError, VendorGet};
use atpkg::store::Layout;
use atpkg::vendor_direct::watch::{ConcurrentVendorGetFn, HeadWatch};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct Fixture(Layout);

impl Fixture {
    fn new() -> Self {
        let id = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let prefix = std::env::temp_dir().join(format!(
            "atpkg-vendor-pending-conformance-{}-{id}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&prefix);
        std::fs::create_dir_all(&prefix).expect("fixture prefix");
        Self(Layout { prefix })
    }

    fn install_legacy(&self, program: &str) {
        // A legacy build has no vendor record, so any well-formed newer head
        // is an offer. The real `installed` reader still traverses `current`.
        let dir = self.0.build_dir(program, 1);
        std::fs::create_dir_all(&dir).expect("fixture build");
        atomic_symlink(&dir, &self.0.program_current(program)).expect("fixture current");
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0.prefix).expect("remove fixture");
    }
}

fn at(secs: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_000_000 + secs)
}

fn modeled_pending(state: &BTreeMap<&'static str, i64>) -> usize {
    ["first", "second"]
        .iter()
        .filter(|slot| (1..=2).contains(&state[*slot]))
        .count()
}

fn body(program: &str, url: &str) -> VendorGet {
    let bytes = match program {
        "claude" => b"2.1.281\n".to_vec(),
        "codex" => br#"{"tag_name":"rust-v0.157.0","assets":[]}"#.to_vec(),
        other => panic!("unexpected vendor {other}"),
    };
    VendorGet::Body {
        bytes,
        etag: None,
        effective_url: url.to_string(),
    }
}

#[test]
fn ready_codex_offer_is_harvested_while_claude_is_still_in_flight() {
    let fixture = Fixture::new();
    fixture.install_legacy("claude");
    fixture.install_legacy("codex");
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let reached = Arc::new(AtomicUsize::new(0));
    let codex_calls = Arc::new(AtomicUsize::new(0));
    let get: Arc<ConcurrentVendorGetFn<'static>> = {
        let gate = Arc::clone(&gate);
        let reached = Arc::clone(&reached);
        let codex_calls = Arc::clone(&codex_calls);
        Arc::new(move |program, url, _cap, _etag| {
            if program == "claude" {
                reached.fetch_or(1, Ordering::SeqCst);
                let (lock, ready) = &*gate;
                let open = lock.lock().expect("gate lock");
                let (open, _) = ready
                    .wait_timeout_while(open, Duration::from_secs(30), |open| !*open)
                    .expect("gate wait");
                assert!(*open, "the pending check must not wait for Claude");
            } else {
                reached.fetch_or(2, Ordering::SeqCst);
                codex_calls.fetch_add(1, Ordering::SeqCst);
            }
            Ok(body(program, url))
        })
    };
    let model = atpkg_vendor_pending_check_model();
    let mut projected = model.init_state();
    let mut watch = HeadWatch::new(&[]);
    let mut first_offers = watch.check_pending(&fixture.0, at(0), &get);
    assert!(model.fire("StartFirst", &mut projected));
    assert!(model.fire("StartSecond", &mut projected));
    assert!(watch.has_pending(), "the blocked Claude head remains owned");
    assert!(watch.pending_count() <= modeled_pending(&projected));

    // The first call may harvest a very fast Codex answer; otherwise await its
    // actual completion. No pass waits for the blocked Claude head.
    for _ in 0..200 {
        if first_offers.contains(&"codex") {
            break;
        }
        watch.park_for_hint(Duration::from_millis(20));
        first_offers.extend(watch.check_pending(&fixture.0, at(0), &get));
    }
    assert_eq!(first_offers, ["codex"]);
    assert_eq!(reached.load(Ordering::SeqCst) & 2, 2);
    assert!(model.fire("SecondReady", &mut projected));
    assert!(model.fire("OfferSecond", &mut projected));
    assert_eq!((projected["first"], projected["second"]), (1, 3));
    assert_eq!(watch.pending_count(), modeled_pending(&projected));
    assert!(
        watch.check_pending(&fixture.0, at(0), &get).is_empty(),
        "a harvested offer is never returned twice"
    );
    assert_eq!(codex_calls.load(Ordering::SeqCst), 1);

    let (lock, ready) = &*gate;
    *lock.lock().expect("gate lock") = true;
    ready.notify_all();
    let mut second_offers = Vec::new();
    for _ in 0..200 {
        watch.park_for_hint(Duration::from_millis(20));
        second_offers.extend(watch.check_pending(&fixture.0, at(0), &get));
        if !second_offers.is_empty() {
            break;
        }
    }
    assert_eq!(second_offers, ["claude"]);
    assert_eq!(reached.load(Ordering::SeqCst) & 3, 3);
    assert!(model.fire("FirstReady", &mut projected));
    assert!(model.fire("OfferFirst", &mut projected));
    assert_eq!((projected["completed"], projected["harvested"]), (2, 2));
    assert!(!watch.has_pending());
    assert_eq!(watch.pending_count(), modeled_pending(&projected));
    assert!(watch.check_pending(&fixture.0, at(0), &get).is_empty());
}

#[test]
fn unreachable_single_head_keeps_its_retry_deadline() {
    let fixture = Fixture::new();
    fixture.install_legacy("claude");
    let calls = Arc::new(AtomicUsize::new(0));
    let get: Arc<ConcurrentVendorGetFn<'static>> = {
        let calls = Arc::clone(&calls);
        Arc::new(move |program, url, _cap, _etag| {
            assert_eq!(program, "claude");
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                Err(VendorFetchError::Unreachable(String::from(
                    "fixture timeout",
                )))
            } else {
                Ok(body(program, url))
            }
        })
    };
    let model = atpkg_vendor_pending_check_model();
    let mut projected = model.init_state();
    let mut watch = HeadWatch::new(&[String::from("codex")]);
    assert!(watch.check_pending(&fixture.0, at(0), &get).is_empty());
    assert!(model.fire("StartFirst", &mut projected));
    for _ in 0..200 {
        if !watch.has_pending() {
            break;
        }
        watch.park_for_hint(Duration::from_millis(20));
        assert!(watch.check_pending(&fixture.0, at(0), &get).is_empty());
    }
    assert!(!watch.has_pending());
    assert!(model.fire("FirstFailed", &mut projected));
    assert!(model.fire("RetryFirst", &mut projected));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(watch.check_pending(&fixture.0, at(59), &get).is_empty());
    let mut offers = watch.check_pending(&fixture.0, at(60), &get);
    for _ in 0..200 {
        if !offers.is_empty() {
            break;
        }
        watch.park_for_hint(Duration::from_millis(20));
        offers.extend(watch.check_pending(&fixture.0, at(60), &get));
    }
    assert_eq!(offers, ["claude"]);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}
