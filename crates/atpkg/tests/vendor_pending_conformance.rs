// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1: bind the bounded pending-head model to the real watch, including
//! independent harvest and the single-head retry after an unreachable hint.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime};

use aterm_spec::derive::atpkg_vendor_pending_check_model;
use aterm_spec::wait::{MUST_HAPPEN, within};
use atpkg::activate::atomic_symlink;
use atpkg::flow::{VendorFetchError, VendorGet};
use atpkg::store::Layout;
use atpkg::vendor_direct::watch::{ConcurrentVendorGetFn, HeadWatch};

/// A store of the test's own: its prefix is a fresh directory that goes with the
/// fixture, failed or not.
struct Fixture {
    layout: Layout,
    _scratch: aterm_tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let scratch = aterm_tempfile::Builder::new()
            .prefix("atpkg-vendor-pending-conformance-")
            .tempdir()
            .expect("fixture prefix");
        Self {
            layout: Layout {
                prefix: scratch.path().to_path_buf(),
            },
            _scratch: scratch,
        }
    }

    fn install_legacy(&self, program: &str) {
        // A legacy build has no vendor record, so any well-formed newer head
        // is an offer. The real `installed` reader still traverses `current`.
        let dir = self.layout.build_dir(program, 1);
        std::fs::create_dir_all(&dir).expect("fixture build");
        atomic_symlink(&dir, &self.layout.program_current(program)).expect("fixture current");
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

/// The longest one park waits for a worker's wake. Not a budget: a finished worker
/// unparks the watch at once, and this only bounds a wake that raced the park.
const HINT_SLICE: Duration = Duration::from_millis(20);

/// Park for the workers' hints and harvest at `now`, collecting every offer into
/// `offers`, until `arrived` holds — at once if it already does. A wait for what
/// MUST happen: a hang detector at [`MUST_HAPPEN`], never a latency budget, so a
/// loaded host only slows it and a watch that never delivers is named at the minute.
fn harvest_until(
    watch: &mut HeadWatch,
    layout: &Layout,
    now: SystemTime,
    get: &Arc<ConcurrentVendorGetFn<'static>>,
    offers: &mut Vec<&'static str>,
    what: &str,
    arrived: impl Fn(&HeadWatch, &[&'static str]) -> bool,
) {
    // A zero gap: the park itself is the wait between probes.
    let done = within(MUST_HAPPEN, Duration::ZERO, || {
        if arrived(watch, offers) {
            return Some(());
        }
        watch.park_for_hint(HINT_SLICE);
        offers.extend(watch.check_pending(layout, now, get));
        arrived(watch, offers).then_some(())
    });
    assert!(
        done.is_some(),
        "timed out after {MUST_HAPPEN:?} waiting for: {what} (offers so far: {offers:?})"
    );
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
                // The test opens the gate only after a check returns, so a check
                // that waited for Claude would never see it open: this wait is
                // that defect's hang detector, never a latency budget.
                let (open, _) = ready
                    .wait_timeout_while(open, MUST_HAPPEN, |open| !*open)
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
    let mut first_offers = watch.check_pending(&fixture.layout, at(0), &get);
    assert!(model.fire("StartFirst", &mut projected));
    assert!(model.fire("StartSecond", &mut projected));
    assert!(watch.has_pending(), "the blocked Claude head remains owned");
    assert!(watch.pending_count() <= modeled_pending(&projected));

    // The first call may harvest a very fast Codex answer; otherwise await its
    // actual completion. No pass waits for the blocked Claude head.
    harvest_until(
        &mut watch,
        &fixture.layout,
        at(0),
        &get,
        &mut first_offers,
        "the Codex offer, harvested while Claude is in flight",
        |_, offers| offers.contains(&"codex"),
    );
    assert_eq!(first_offers, ["codex"]);
    assert_eq!(reached.load(Ordering::SeqCst) & 2, 2);
    assert!(model.fire("SecondReady", &mut projected));
    assert!(model.fire("OfferSecond", &mut projected));
    assert_eq!((projected["first"], projected["second"]), (1, 3));
    assert_eq!(watch.pending_count(), modeled_pending(&projected));
    assert!(
        watch.check_pending(&fixture.layout, at(0), &get).is_empty(),
        "a harvested offer is never returned twice"
    );
    assert_eq!(codex_calls.load(Ordering::SeqCst), 1);

    let (lock, ready) = &*gate;
    *lock.lock().expect("gate lock") = true;
    ready.notify_all();
    let mut second_offers = Vec::new();
    harvest_until(
        &mut watch,
        &fixture.layout,
        at(0),
        &get,
        &mut second_offers,
        "the Claude offer, once its gate opened",
        |_, offers| !offers.is_empty(),
    );
    assert_eq!(second_offers, ["claude"]);
    assert_eq!(reached.load(Ordering::SeqCst) & 3, 3);
    assert!(model.fire("FirstReady", &mut projected));
    assert!(model.fire("OfferFirst", &mut projected));
    assert_eq!((projected["completed"], projected["harvested"]), (2, 2));
    assert!(!watch.has_pending());
    assert_eq!(watch.pending_count(), modeled_pending(&projected));
    assert!(watch.check_pending(&fixture.layout, at(0), &get).is_empty());
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
    assert!(watch.check_pending(&fixture.layout, at(0), &get).is_empty());
    assert!(model.fire("StartFirst", &mut projected));
    let mut failed_offers = Vec::new();
    harvest_until(
        &mut watch,
        &fixture.layout,
        at(0),
        &get,
        &mut failed_offers,
        "the unreachable head's harvest",
        |watch, _| !watch.has_pending(),
    );
    assert!(failed_offers.is_empty(), "{failed_offers:?}");
    assert!(model.fire("FirstFailed", &mut projected));
    assert!(model.fire("RetryFirst", &mut projected));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(
        watch
            .check_pending(&fixture.layout, at(59), &get)
            .is_empty()
    );
    // A retry launched before its deadline would be pending NOW: the worker is
    // spawned inside the check, before it returns, whatever the GET then does.
    assert!(
        !watch.has_pending(),
        "the retry launched before its deadline"
    );
    let mut offers = watch.check_pending(&fixture.layout, at(60), &get);
    harvest_until(
        &mut watch,
        &fixture.layout,
        at(60),
        &get,
        &mut offers,
        "the retried head's offer",
        |_, offers| !offers.is_empty(),
    );
    assert_eq!(offers, ["claude"]);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}
