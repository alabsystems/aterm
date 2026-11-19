// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};

use super::*;
use crate::vendor_direct::{Anchor, RECORD_SCHEMA, VendorRecord, record_path, spec};

const CLAUDE_HEAD: &str = "https://downloads.claude.ai/claude-code-releases/latest";
const CODEX_HEAD: &str = "https://releases.openai.com/codex/channels/latest";

/// The hang detector for a wait that must end (a worker's answer, a test's own
/// release).
pub(super) const HANG: Duration = Duration::from_secs(60);

/// A rest `watch`'s workers ring.
pub(super) fn ringing(watch: &mut HeadWatch) -> Rest {
    let rest = Rest::inert();
    watch.bind_ring(rest.ringer());
    rest
}

/// Wait for a head worker's ring (a hang detector) and take it.
pub(super) fn rang(rest: &Rest) {
    assert!(
        rest.wait(Some(HANG)).is_some_and(|told| told.rung),
        "a head worker rang within a minute (a hang detector)"
    );
}

fn v(text: &str) -> Version {
    Version::parse(text).unwrap()
}

fn at(secs: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_000_000 + secs)
}

pub(super) fn layout(label: &str) -> crate::testdir::ScratchLayout {
    crate::testdir::layout(&format!("atpkg-watch-{label}-"))
}

#[test]
fn a_window_sit_waits_for_a_sessions_held_move_look() {
    let l = layout("sit-handoff");
    let mut session = host::HostClaim::new(Host::Session, &l, None);
    assert_eq!(session.admit().0, host::Admit::Yes);
    assert!(session.seated_session());
    let mut window = HeadWatch::hosted(&[], Host::Window);
    assert!(!window.sit(&l), "the session owns this slice's look");
    assert_eq!(
        host::watcher(&l),
        host::Watcher::Session(Some(std::process::id()))
    );
    session.end_round();
    assert!(window.sit(&l), "the next slice takes the shared claim");
    assert_eq!(host::watcher(&l), host::Watcher::Window);
    assert_eq!(session.admit().0, host::Admit::Later(CADENCE));
    drop(window);
    drop(session);
    let _ = std::fs::remove_dir_all(&l.prefix);
}

fn activate(layout: &Layout, program: &str, build: u64) -> PathBuf {
    let dir = layout.build_dir(program, build);
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    crate::activate::atomic_symlink(&dir, &layout.program_current(program)).unwrap();
    dir
}

/// A complete, recorded vendor build of `program` at `version`, active.
pub(super) fn install(layout: &Layout, program: &str, version: &str) {
    let spec = spec(program).unwrap();
    let version = v(version);
    let dir = activate(layout, program, version.build_id());
    let source_url = match spec.anchor {
        Anchor::AnthropicOpenPgp => {
            format!(
                "https://downloads.claude.ai/claude-code-releases/{version}/darwin-arm64/claude"
            )
        }
        Anchor::OpenAiTwoHost => format!(
            "https://github.com/openai/codex/releases/download/rust-v{version}/codex-package-aarch64-apple-darwin.tar.gz"
        ),
        Anchor::GoogleTwoHost => format!(
            "https://storage.googleapis.com/antigravity-public/antigravity-cli/{version}-1/darwin-arm/cli_mac_arm64.tar.gz"
        ),
        Anchor::MetaDeveloperId => format!(
            "https://lookaside.facebook.com/lookaside/muse/download/?channel=muse&version={version}&file=muse-aarch64-macos"
        ),
        Anchor::MoonshotTwoHost => {
            format!("https://cdn.kimi.com/kimi-code/binaries/{version}/kimi-code-darwin-arm64")
        }
        Anchor::XaiNpmRegistry | Anchor::XaiDeveloperId => format!(
            "https://registry.npmjs.org/@xai-official/grok-darwin-arm64/-/grok-darwin-arm64-{version}.tgz"
        ),
    };
    let record = VendorRecord {
        schema: RECORD_SCHEMA,
        program: program.to_string(),
        version,
        vendor: spec.vendor.to_string(),
        source_url,
        sha256: "a".repeat(64),
        sha512: String::new(),
        size: 1,
        tree_root: "b".repeat(64),
        apple_team: cfg!(target_os = "macos").then(|| spec.apple_team.to_string()),
        anchor: spec.anchor,
        build_date: None,
        verified_at: 1_790_000_000,
        measured_sha256: None,
        agreed_with: None,
        tree_from: None,
    };
    let bytes = record.record_bytes().unwrap();
    super::super::durable::write_durable(&record_path(&dir).unwrap(), &bytes).unwrap();
    crate::store::mark_build_ready(&dir).unwrap();
    assert_eq!(
        super::super::complete_record(&dir).map(|r| r.version),
        Some(version)
    );
}

/// What one GET answered.
#[derive(Clone)]
enum Answer {
    Body(Vec<u8>, Option<String>),
    Unreachable,
    Unserved,
}

/// The vendors, as the watch's GET sees them. A body served with an ETag answers a
/// matching `If-None-Match` with a 304.
#[derive(Default)]
struct Vendors {
    heads: RefCell<BTreeMap<&'static str, Answer>>,
    /// Every GET: `(url, if_none_match)`.
    gets: RefCell<Vec<(String, Option<String>)>>,
}

impl Vendors {
    fn head(&self, url: &'static str, body: &str, etag: Option<&str>) {
        self.heads.borrow_mut().insert(
            url,
            Answer::Body(body.as_bytes().to_vec(), etag.map(str::to_string)),
        );
    }

    fn answer(&self, url: &'static str, answer: Answer) {
        self.heads.borrow_mut().insert(url, answer);
    }

    fn codex(&self, version: &str, etag: Option<&str>) {
        self.head(
            CODEX_HEAD,
            &format!("{{\"tag_name\":\"rust-v{version}\",\"assets\":[]}}"),
            etag,
        );
    }

    fn get(
        &self,
        program: &str,
        url: &str,
        cap: u64,
        if_none_match: Option<&str>,
    ) -> Result<VendorGet, VendorFetchError> {
        assert!(
            crate::vendor::vendor_direct_url_allowed(program, url),
            "{url}"
        );
        let want = if program == "claude" { 64 } else { 1024 * 1024 };
        assert_eq!(cap, want, "{program}'s head cap");
        self.gets
            .borrow_mut()
            .push((url.to_string(), if_none_match.map(str::to_string)));
        match self.heads.borrow().get(url).cloned() {
            Some(Answer::Body(bytes, etag)) => {
                if etag.is_some() && etag.as_deref() == if_none_match {
                    return Ok(VendorGet::NotModified {
                        etag: etag.unwrap_or_default(),
                    });
                }
                Ok(VendorGet::Body {
                    bytes,
                    etag,
                    effective_url: url.to_string(),
                })
            }
            Some(Answer::Unreachable) => {
                Err(VendorFetchError::Unreachable(String::from("timed out")))
            }
            Some(Answer::Unserved) | None => {
                Err(VendorFetchError::Refused(String::from("HTTP 404")))
            }
        }
    }

    fn take_gets(&self) -> Vec<(String, Option<String>)> {
        std::mem::take(&mut self.gets.borrow_mut())
    }
}

fn check(watch: &mut HeadWatch, l: &Layout, f: &Vendors, now: SystemTime) -> Vec<&'static str> {
    watch.check(l, now, &|p, u, c, e| f.get(p, u, c, e))
}

#[test]
fn a_newer_head_triggers_and_an_equal_or_older_one_does_not() {
    let l = layout("newer");
    install(&l, "claude", "2.1.280");
    install(&l, "codex", "0.156.0");
    let f = Vendors::default();
    f.head(CLAUDE_HEAD, "2.1.281\n", Some("\"c1\""));
    f.codex("0.156.0", Some("\"x1\""));
    let mut w = HeadWatch::new(&[]);
    assert!(w.due(at(0)), "every head is due at once");
    assert_eq!(check(&mut w, &l, &f, at(0)), ["claude"]);
    assert_eq!(w.take_notes().len(), 1);
    assert_eq!(
        f.take_gets(),
        [
            (CLAUDE_HEAD.to_string(), None),
            (CODEX_HEAD.to_string(), None)
        ]
    );
    let mut w = HeadWatch::new(&[]);
    f.head(CLAUDE_HEAD, "2.1.279", None);
    f.codex("0.155.0", None);
    assert!(check(&mut w, &l, &f, at(0)).is_empty());
    assert!(w.take_notes().is_empty());
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn concurrent_watch_reaches_codex_while_claude_head_is_waiting() {
    let l = layout("concurrent-heads");
    install(&l, "claude", "2.1.280");
    install(&l, "codex", "0.156.0");
    let release_claude = Arc::new((Mutex::new(false), Condvar::new()));
    let claude_finished = Arc::new(AtomicBool::new(false));
    let gate = Arc::clone(&release_claude);
    let finished = Arc::clone(&claude_finished);
    let get: Arc<ConcurrentVendorGetFn<'static>> = Arc::new(
        move |program: &str, url: &str, cap: u64, etag: Option<&str>| {
            assert!(etag.is_none());
            let bytes = if program == "claude" {
                assert_eq!((url, cap), (CLAUDE_HEAD, 64));
                let (lock, ready) = &*gate;
                let released = lock.lock().unwrap();
                let (released, _) = ready
                    .wait_timeout_while(released, 2 * HANG, |released| !*released)
                    .unwrap();
                assert!(*released, "test release arrived before the network timeout");
                finished.store(true, Ordering::Release);
                b"2.1.281".to_vec()
            } else {
                assert_eq!((program, url, cap), ("codex", CODEX_HEAD, 1024 * 1024));
                br#"{"tag_name":"rust-v0.157.0","assets":[]}"#.to_vec()
            };
            Ok(VendorGet::Body {
                bytes,
                etag: None,
                effective_url: url.to_string(),
            })
        },
    );
    let mut watch = HeadWatch::shared(&[]);
    let rest = ringing(&mut watch);
    assert!(watch.check_pending(&l, at(0), &get).is_empty());
    assert_eq!(
        watch.pending_count(),
        2,
        "both independent hints are in flight"
    );
    rang(&rest);
    let moved = watch.check_pending(&l, at(0), &get);
    assert_eq!(moved, ["codex"], "Codex is offered before Claude finishes");
    assert!(
        !claude_finished.load(Ordering::Acquire),
        "the slow Claude head is still waiting"
    );
    assert_eq!(watch.pending_count(), 1);
    assert!(
        watch.take_notes().is_empty(),
        "notes await table-order flush"
    );
    assert!(
        watch.check_pending(&l, at(0), &get).is_empty(),
        "no duplicate offer"
    );
    let (lock, ready) = &*release_claude;
    *lock.lock().unwrap() = true;
    ready.notify_all();
    rang(&rest);
    assert_eq!(watch.check_pending(&l, at(0), &get), ["claude"]);
    assert_eq!(watch.pending_count(), 0);
    let notes = watch.take_notes();
    assert!(
        notes[0].starts_with("vendor head moved: claude"),
        "{notes:?}"
    );
    assert!(
        notes[1].starts_with("vendor head moved: codex"),
        "{notes:?}"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn one_blocked_vendor_is_owned_without_blocking_the_package_lane() {
    let l = layout("one-blocked-head");
    install(&l, "claude", "2.1.280");
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let wait_gate = Arc::clone(&gate);
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let get: Arc<ConcurrentVendorGetFn<'static>> = Arc::new(move |program, url, cap, etag| {
        assert_eq!((program, url, cap, etag), ("claude", CLAUDE_HEAD, 64, None));
        started_tx.send(()).unwrap();
        let (lock, ready) = &*wait_gate;
        let released = lock.lock().unwrap();
        let (released, _) = ready
            .wait_timeout_while(released, 2 * HANG, |released| !*released)
            .unwrap();
        assert!(*released, "the test released the simulated vendor GET");
        Ok(VendorGet::Body {
            bytes: b"2.1.281".to_vec(),
            etag: None,
            effective_url: url.to_string(),
        })
    });
    let caller_layout = l.clone();
    let caller_get = Arc::clone(&get);
    let (returned_tx, returned_rx) = std::sync::mpsc::channel();
    let rest = Rest::inert();
    let ring = rest.ringer();
    let caller = std::thread::spawn(move || {
        let mut watch = HeadWatch::shared(&[String::from("codex")]);
        watch.bind_ring(ring);
        let moved = watch.check_pending(&caller_layout, at(0), &caller_get);
        returned_tx.send((watch, moved)).unwrap();
    });
    started_rx.recv_timeout(HANG).unwrap();
    let early = returned_rx.recv_timeout(HANG);
    let returned_while_blocked = early.is_ok();
    let owned_while_blocked = early
        .as_ref()
        .is_ok_and(|(watch, moved)| moved.is_empty() && watch.pending_count() == 1);
    let (lock, ready) = &*gate;
    *lock.lock().unwrap() = true;
    ready.notify_all();
    let (mut watch, moved) = early.unwrap_or_else(|_| returned_rx.recv_timeout(HANG).unwrap());
    caller.join().unwrap();
    assert!(
        returned_while_blocked,
        "one slow vendor GET blocked the package lane"
    );
    assert!(owned_while_blocked, "the blocked GET must remain owned");
    assert!(moved.is_empty());
    rang(&rest);
    assert_eq!(watch.check_pending(&l, at(0), &get), ["claude"]);
    assert_eq!(watch.pending_count(), 0);
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn a_deferred_hint_is_skipped_if_the_full_pass_installed_it() {
    let l = layout("deferred-head-landed");
    install(&l, "claude", "2.1.280");
    let calls = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&calls);
    let get: Arc<ConcurrentVendorGetFn<'static>> = Arc::new(move |program, url, _, _| {
        assert_eq!((program, url), ("claude", CLAUDE_HEAD));
        count.fetch_add(1, Ordering::SeqCst);
        Ok(VendorGet::Body {
            bytes: b"2.1.281".to_vec(),
            etag: None,
            effective_url: url.to_string(),
        })
    });
    let mut watch = HeadWatch::new(&[String::from("codex")]);
    let moved = watch.check(&l, at(0), get.as_ref());
    assert_eq!(moved, ["claude"]);
    watch.defer_moved(&moved);
    assert!(watch.due(at(0)), "the carried hint wakes the next park");
    install(&l, "claude", "2.1.281");
    assert!(
        watch.check_pending(&l, at(0), &get).is_empty(),
        "the signed pass already landed the offered version"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1, "no duplicate GET");
    assert!(!watch.due(at(0)), "the carried hint is consumed once");
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn vendor_spawn_failure_cannot_block_an_in_flight_index_answer() {
    let model = aterm_models_atpkg::atpkg_index_pending_park_model();
    let mut state = model.init_state();
    assert!(model.fire("Start", &mut state));
    let l = layout("spawn-failed-beside-index");
    install(&l, "claude", "2.1.280");
    let calls = Arc::new(AtomicUsize::new(0));
    let get_calls = Arc::clone(&calls);
    let get: Arc<ConcurrentVendorGetFn<'static>> = Arc::new(move |program, url, _, _| {
        assert_eq!((program, url), ("claude", CLAUDE_HEAD));
        get_calls.fetch_add(1, Ordering::SeqCst);
        Ok(VendorGet::Body {
            bytes: b"2.1.281".to_vec(),
            etag: None,
            effective_url: url.to_string(),
        })
    });
    let mut watch = HeadWatch::new(&[String::from("codex")]);
    let rest = ringing(&mut watch);
    watch.fail_next_pending_spawn = true;
    assert!(
        watch
            .check_pending_with_index_in_flight(&l, at(0), &get, true)
            .is_empty()
    );
    assert!(
        rest.take().is_some_and(|told| told.rung),
        "the refused worker was its round's last, so the host is rung"
    );
    assert!(model.fire("VendorSpawnFailed", &mut state));
    assert_eq!(
        i64::try_from(calls.load(Ordering::SeqCst)).unwrap(),
        state["inline_vendor_get"]
    );
    assert_eq!(watch.pending_count(), 0);
    assert!(!watch.round_active, "a failed spawn leaves no active round");
    assert_eq!(watch.watched[0].1.next_check_at, Some(at(0) + BUSY_RETRY));
    assert!(!watch.due(at(0) + BUSY_RETRY - Duration::from_millis(1)));
    assert!(watch.due(at(0) + BUSY_RETRY));
    assert_eq!(state["vendor_retry"], 1);
    assert!(
        watch
            .take_notes()
            .iter()
            .any(|note| note.starts_with("vendor head helper unavailable: claude"))
    );

    assert!(
        watch
            .check_pending_with_index_in_flight(&l, at(0) + BUSY_RETRY, &get, true)
            .is_empty()
    );
    assert_eq!(watch.pending_count(), 1);
    rang(&rest);
    assert_eq!(
        watch.check_pending_with_index_in_flight(&l, at(0) + BUSY_RETRY, &get, true),
        ["claude"]
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(model.fire("VendorReady", &mut state));
    assert_eq!(watch.pending_count(), 0);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut old = buggy.init_state();
    assert!(buggy.fire("Start", &mut old));
    assert!(buggy.fire("VendorSpawnFailed", &mut old));
    assert_eq!(old["inline_vendor_get"], 1);
    assert!(!buggy.check_invariant("NoInlineVendorGetOnHelperFailure", &old));
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn vendor_spawn_failure_defers_even_without_an_index_probe() {
    let model = aterm_models_atpkg::atpkg_index_pending_park_model();
    let mut state = model.init_state();
    let l = layout("spawn-failed-without-index");
    install(&l, "claude", "2.1.280");
    install(&l, "codex", "0.156.0");
    let claude_calls = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&claude_calls);
    let get: Arc<ConcurrentVendorGetFn<'static>> = Arc::new(move |program, url, _, _| {
        let bytes = if program == "claude" {
            assert_eq!(url, CLAUDE_HEAD);
            count.fetch_add(1, Ordering::SeqCst);
            b"2.1.281".to_vec()
        } else {
            assert_eq!((program, url), ("codex", CODEX_HEAD));
            br#"{"tag_name":"rust-v0.156.0","assets":[]}"#.to_vec()
        };
        Ok(VendorGet::Body {
            bytes,
            etag: None,
            effective_url: url.to_string(),
        })
    });
    let mut watch = HeadWatch::new(&[]);
    let rest = ringing(&mut watch);
    watch.fail_next_pending_spawn = true;
    assert!(watch.check_pending(&l, at(0), &get).is_empty());
    assert!(model.fire("VendorSpawnFailed", &mut state));
    assert_eq!(claude_calls.load(Ordering::SeqCst), 0);
    assert_eq!(state["inline_vendor_get"], 0);
    assert_eq!(watch.pending_count(), 1, "the other vendor keeps its slot");
    rang(&rest);
    assert!(watch.check_pending(&l, at(0), &get).is_empty());
    assert_eq!(watch.pending_count(), 0);
    assert!(!watch.round_active);
    assert_eq!(watch.watched[0].1.next_check_at, Some(at(0) + BUSY_RETRY));
    assert!(!watch.due(at(0) + BUSY_RETRY - Duration::from_millis(1)));
    assert!(watch.due(at(0) + BUSY_RETRY));

    assert!(watch.check_pending(&l, at(0) + BUSY_RETRY, &get).is_empty());
    assert_eq!(watch.pending_count(), 1);
    rang(&rest);
    assert_eq!(
        watch.check_pending(&l, at(0) + BUSY_RETRY, &get),
        ["claude"]
    );
    assert_eq!(claude_calls.load(Ordering::SeqCst), 1);
    assert!(model.fire("VendorReady", &mut state));
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut old = buggy.init_state();
    assert!(buggy.fire("VendorSpawnFailed", &mut old));
    assert_eq!(old["inline_vendor_get"], 1);
    assert!(!buggy.check_invariant("NoInlineVendorGetOnHelperFailure", &old));
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn completed_hint_skips_a_pass_already_landed_by_the_whole_store_update() {
    let l = layout("head-landed-before-harvest");
    install(&l, "claude", "2.1.280");
    install(&l, "codex", "0.156.0");
    let release_claude = Arc::new((Mutex::new(false), Condvar::new()));
    let (codex_started_tx, codex_started_rx) = std::sync::mpsc::channel();
    let gate = Arc::clone(&release_claude);
    let get: Arc<ConcurrentVendorGetFn<'static>> = Arc::new(move |program, url, _, _| {
        let bytes = if program == "claude" {
            let (lock, ready) = &*gate;
            let released = lock.lock().unwrap();
            let (released, _) = ready
                .wait_timeout_while(released, 2 * HANG, |released| !*released)
                .unwrap();
            assert!(*released);
            b"2.1.281".to_vec()
        } else {
            let _ = codex_started_tx.send(());
            br#"{"tag_name":"rust-v0.157.0","assets":[]}"#.to_vec()
        };
        Ok(VendorGet::Body {
            bytes,
            etag: None,
            effective_url: url.to_string(),
        })
    });
    let mut watch = HeadWatch::new(&[]);
    let rest = ringing(&mut watch);
    assert!(watch.check_pending(&l, at(0), &get).is_empty());
    codex_started_rx.recv_timeout(HANG).unwrap();
    rang(&rest);
    install(&l, "codex", "0.157.0");
    assert!(
        watch.check_pending(&l, at(0), &get).is_empty(),
        "the whole pass reached Codex before the hint was harvested"
    );
    assert_eq!(watch.pending_count(), 1);
    let (lock, ready) = &*release_claude;
    *lock.lock().unwrap() = true;
    ready.notify_all();
    rang(&rest);
    assert_eq!(watch.check_pending(&l, at(0), &get), ["claude"]);
    let notes = watch.take_notes();
    assert!(
        notes
            .iter()
            .any(|note| note.starts_with("vendor head moved: codex")),
        "Codex did move before the full pass landed it: {notes:?}"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn a_304_is_nothing_and_costs_one_conditional_get() {
    let l = layout("304");
    install(&l, "claude", "2.1.280");
    let f = Vendors::default();
    f.head(CLAUDE_HEAD, "2.1.281", Some("\"c1\""));
    let mut w = HeadWatch::new(&[]);
    assert_eq!(check(&mut w, &l, &f, at(0)), ["claude"]);
    f.take_gets();
    install(&l, "claude", "2.1.281");
    for secs in [300, 600, 3 * 3600] {
        assert!(w.due(at(secs)));
        assert!(check(&mut w, &l, &f, at(secs)).is_empty(), "{secs}");
        assert_eq!(
            f.take_gets(),
            [(CLAUDE_HEAD.to_string(), Some(String::from("\"c1\"")))],
            "{secs}"
        );
    }
    assert_eq!(
        w.take_notes().len(),
        1,
        "the one trigger note, nothing per check"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn independent_watches_share_one_request_and_validator_per_store() {
    let l = layout("shared");
    install(&l, "claude", "2.1.280");
    let f = Vendors::default();
    f.head(CLAUDE_HEAD, "2.1.280", Some("\"c1\""));
    let excluded = [String::from("codex")];
    let mut first = HeadWatch::shared(&excluded);
    let mut second = HeadWatch::shared(&excluded);
    assert!(check(&mut first, &l, &f, at(0)).is_empty());
    let first_record = crate::vendor_direct::ProgramStamp::read(&l, "claude")
        .unwrap()
        .last_checked_at;
    assert_eq!(f.take_gets(), [(CLAUDE_HEAD.to_string(), None)]);
    assert!(
        check(&mut second, &l, &f, at(0) - Duration::from_millis(1)).is_empty(),
        "a slightly earlier sampled clock is not a rollback"
    );
    assert!(check(&mut second, &l, &f, at(5)).is_empty());
    assert!(f.take_gets().is_empty(), "another window reuses the minute");
    assert!(check(&mut second, &l, &f, at(60)).is_empty());
    assert_eq!(
        crate::vendor_direct::ProgramStamp::read(&l, "claude")
            .unwrap()
            .last_checked_at,
        first_record,
        "an unchanged 304 does not fsync the durable vendor stamp every minute"
    );
    assert_eq!(
        f.take_gets(),
        [(CLAUDE_HEAD.to_string(), Some(String::from("\"c1\"")))],
        "the other window inherits the ETag"
    );
    assert!(check(&mut first, &l, &f, at(65)).is_empty());
    assert!(f.take_gets().is_empty());
    f.head(CLAUDE_HEAD, "2.1.281", Some("\"c2\""));
    assert_eq!(check(&mut first, &l, &f, at(120)), ["claude"]);
    assert_eq!(
        f.take_gets(),
        [(CLAUDE_HEAD.to_string(), Some(String::from("\"c1\"")))],
    );
    assert!(check(&mut second, &l, &f, at(125)).is_empty());
    assert!(f.take_gets().is_empty(), "one offer across both windows");
    let mut restarted = HeadWatch::shared(&excluded);
    assert!(check(&mut restarted, &l, &f, at(180)).is_empty());
    assert_eq!(
        f.take_gets(),
        [(CLAUDE_HEAD.to_string(), Some(String::from("\"c2\"")))],
        "a new process inherits the last ETag"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn a_result_deferred_by_the_shared_lock_is_reconciled_before_early_return() {
    let l = layout("busy-result");
    install(&l, "claude", "2.1.280");
    let f = Vendors::default();
    f.head(CLAUDE_HEAD, "2.1.281", Some("\"c1\""));
    let spec = spec("claude").unwrap();
    let mut watch = HeadWatch::shared(&[String::from("codex")]);
    assert_eq!(check(&mut watch, &l, &f, at(0)), ["claude"]);
    f.take_gets();
    let SharedFile::Held(mut held) = open_shared(&l, spec) else {
        panic!("watch lock");
    };
    let mut state = read_shared(&mut held, spec);
    state.next_check_at = unix_seconds(at(355));
    write_shared(&mut held, &state).unwrap();
    watch.note_pass_result("claude", false, at(295));
    assert!(watch.watched[0].1.deferred_result.is_some());
    drop(held);
    assert!(check(&mut watch, &l, &f, at(300)).is_empty());
    assert!(f.take_gets().is_empty(), "the shared GET was not due");
    let SharedFile::Held(mut file) = open_shared(&l, spec) else {
        panic!("watch lock");
    };
    let persisted = read_shared(&mut file, spec);
    assert_eq!(persisted.short_retries, 1);
    assert!(!persisted.pending);
    assert_eq!(persisted.offered_at, unix_seconds(at(295)));
    drop(file);
    assert_eq!(check(&mut watch, &l, &f, at(595)), ["claude"]);
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn a_deferred_result_does_not_clear_a_newer_offer_of_the_same_version() {
    let l = layout("stale-result");
    install(&l, "claude", "2.1.280");
    let f = Vendors::default();
    f.head(CLAUDE_HEAD, "2.1.281", Some("\"c1\""));
    let spec = spec("claude").unwrap();
    let mut watch = HeadWatch::shared(&[String::from("codex")]);
    assert_eq!(check(&mut watch, &l, &f, at(0)), ["claude"]);
    f.take_gets();
    let SharedFile::Held(mut held) = open_shared(&l, spec) else {
        panic!("watch lock");
    };
    watch.note_pass_result("claude", false, at(295));
    let mut state = read_shared(&mut held, spec);
    state.offer_generation += 1;
    state.offered_at = unix_seconds(at(295));
    state.next_check_at = unix_seconds(at(355));
    state.short_retries = 1;
    write_shared(&mut held, &state).unwrap();
    drop(held);
    assert!(check(&mut watch, &l, &f, at(300)).is_empty());
    assert!(watch.watched[0].1.deferred_result.is_none());
    let SharedFile::Held(mut file) = open_shared(&l, spec) else {
        panic!("watch lock");
    };
    let persisted = read_shared(&mut file, spec);
    assert!(
        persisted.pending,
        "the newer offer still awaits its own pass"
    );
    assert_eq!(persisted.offer_generation, state.offer_generation);
    assert_eq!(persisted.offered_at, unix_seconds(at(295)));
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[cfg(unix)]
#[test]
fn an_unavailable_shared_file_applies_the_result_to_the_local_fallback() {
    use std::os::unix::fs::symlink;

    let l = layout("unavailable-result");
    install(&l, "claude", "2.1.280");
    let f = Vendors::default();
    f.head(CLAUDE_HEAD, "2.1.281", Some("\"c1\""));
    let mut watch = HeadWatch::shared(&[String::from("codex")]);
    assert_eq!(check(&mut watch, &l, &f, at(0)), ["claude"]);
    f.take_gets();
    let file = l.prefix.join("vendor-head-claude.watch");
    std::fs::remove_file(&file).unwrap();
    symlink("/dev/null", &file).unwrap();
    watch.note_pass_result("claude", false, at(295));
    assert!(!watch.watched[0].1.pending);
    assert_eq!(watch.watched[0].1.short_retries, 1);
    assert!(check(&mut watch, &l, &f, at(296)).is_empty());
    assert_eq!(check(&mut watch, &l, &f, at(595)), ["claude"]);
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn a_wall_clock_rollback_refreshes_the_durable_check_stamp() {
    let l = layout("clock-rollback");
    install(&l, "claude", "2.1.280");
    let f = Vendors::default();
    f.head(CLAUDE_HEAD, "2.1.280", Some("\"c1\""));
    let mut watch = HeadWatch::shared(&[String::from("codex")]);
    let before = at(10_000);
    assert!(check(&mut watch, &l, &f, before).is_empty());
    let stamp = || {
        crate::vendor_direct::ProgramStamp::read(&l, "claude")
            .unwrap()
            .last_checked_at
    };
    assert_eq!(
        stamp(),
        i64::try_from(unix_seconds(before).unwrap()).unwrap()
    );
    let after = before - Duration::from_secs(2 * 3600);
    assert!(check(&mut watch, &l, &f, after).is_empty());
    assert_eq!(
        stamp(),
        i64::try_from(unix_seconds(after).unwrap()).unwrap()
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn failed_targeted_pass_gets_one_short_retry_then_hourly_offers() {
    let l = layout("failed-pass-retry");
    install(&l, "claude", "2.1.280");
    let f = Vendors::default();
    f.head(CLAUDE_HEAD, "2.1.281", Some("\"c1\""));
    let excluded = [String::from("codex")];
    let mut first = HeadWatch::shared(&excluded);
    assert_eq!(check(&mut first, &l, &f, at(0)), ["claude"]);
    assert!(!first.head_reached(&l, "claude"));
    first.note_pass_result("claude", false, at(5));
    f.take_gets();
    let mut restarted = HeadWatch::shared(&excluded);
    assert!(check(&mut restarted, &l, &f, at(300)).is_empty());
    assert_eq!(check(&mut restarted, &l, &f, at(305)), ["claude"]);
    restarted.note_pass_result("claude", false, at(310));
    assert!(check(&mut restarted, &l, &f, at(610)).is_empty());
    assert_eq!(
        check(&mut restarted, &l, &f, at(3910)),
        ["claude"],
        "the next offer is an hour after the second failure"
    );
    install(&l, "claude", "2.1.281");
    assert!(restarted.head_reached(&l, "claude"));
    assert!(check(&mut restarted, &l, &f, at(3970)).is_empty());
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn an_unacknowledged_offer_recovers_after_five_minutes() {
    let l = layout("unacknowledged-offer");
    install(&l, "claude", "2.1.280");
    let f = Vendors::default();
    f.head(CLAUDE_HEAD, "2.1.281", Some("\"c1\""));
    let excluded = [String::from("codex")];
    let mut crashed = HeadWatch::shared(&excluded);
    assert_eq!(check(&mut crashed, &l, &f, at(0)), ["claude"]);
    f.take_gets();
    drop(crashed);
    let mut successor = HeadWatch::shared(&excluded);
    assert!(check(&mut successor, &l, &f, at(299)).is_empty());
    assert_eq!(check(&mut successor, &l, &f, at(300)), ["claude"]);
    assert_eq!(
        f.take_gets(),
        [(CLAUDE_HEAD.to_string(), Some(String::from("\"c1\"")))],
        "the successor reuses the first process's validator"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn an_unacknowledged_head_uses_two_short_leases_then_hourly_304s() {
    let l = layout("not-landed");
    install(&l, "claude", "2.1.280");
    let f = Vendors::default();
    f.head(CLAUDE_HEAD, "2.1.281", Some("\"c1\""));
    let mut w = HeadWatch::new(&[String::from("codex")]);
    assert_eq!(check(&mut w, &l, &f, at(0)), ["claude"]);
    w.take_notes();
    f.take_gets();
    let mut offered = Vec::new();
    for secs in (60..=2 * 3600).step_by(60) {
        if !check(&mut w, &l, &f, at(secs)).is_empty() {
            offered.push(secs);
        }
        assert_eq!(
            f.take_gets(),
            [(CLAUDE_HEAD.to_string(), Some(String::from("\"c1\"")))],
            "{secs}: one conditional GET"
        );
    }
    assert_eq!(offered, [300, 600, 4200], "bounded recovery, then hourly");
    assert_eq!(w.take_notes().len(), 3);
    install(&l, "claude", "2.1.281");
    assert!(check(&mut w, &l, &f, at(3 * 3600)).is_empty());
    assert!(check(&mut w, &l, &f, at(4 * 3600)).is_empty());
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn the_watch_runs_only_where_the_pass_reaches_a_vendor() {
    let dir = OsStr::new("dir:/mirror");
    assert!(admitted(true, None));
    assert!(!admitted(false, None), "no pinned root");
    assert!(
        !admitted(true, Some(dir)),
        "a dir: registry reaches no vendor"
    );
    assert!(!admitted(false, Some(dir)));
    assert!(
        admitted(true, Some(OsStr::new("https://example.invalid"))),
        "anything but dir: is the network fetcher"
    );
    assert_eq!(
        crate::source::dir_registry(Some(dir)),
        Some(PathBuf::from("/mirror")),
        "the pass's own choice"
    );
}

#[test]
fn the_cadence_is_one_minute_on_the_wall_clock() {
    let l = layout("cadence");
    install(&l, "claude", "2.1.280");
    let f = Vendors::default();
    f.head(CLAUDE_HEAD, "2.1.280", Some("\"c1\""));
    let mut w = HeadWatch::new(&[]);
    check(&mut w, &l, &f, at(0));
    assert!(!w.due(at(59)));
    assert!(w.due(at(60)));
    f.take_gets();
    assert!(check(&mut w, &l, &f, at(59)).is_empty());
    assert!(f.take_gets().is_empty());
    assert!(w.due(at(0) - Duration::from_secs(86_400)));
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// ONE WAIT IS JUDGED A WAKE BY ITS TWO CLOCKS, AND A WAKE ASKS EVERY HEAD AFTER THE GRACE: a
/// wait the wall clock outran by exactly WAKE_GAP is no wake, by a second more it is, however
/// long the wait (an hour's wait whose wall clock ran an hour and a half is one), and a wall
/// clock set back never is. woke() moves every head, and the next check of one in flight, to
/// AFTER_WAKE later. NEGATIVE CONTROL: `>=` at the gap reads the first case as a wake; a woke()
/// that skips the in-flight head leaves its wake_after unset.
#[test]
fn woke_between_judges_one_wait_and_woke_reschedules_every_head() {
    let five = Duration::from_secs(5);
    let hour = Duration::from_secs(3600);
    assert!(!woke_between(Some(five), five));
    assert!(!woke_between(Some(five + WAKE_GAP), five), "the gap itself");
    assert!(woke_between(
        Some(five + WAKE_GAP + Duration::from_secs(1)),
        five
    ));
    assert!(!woke_between(Some(hour + Duration::from_secs(29)), hour));
    assert!(woke_between(Some(hour + hour / 2), hour), "one long wait");
    assert!(!woke_between(None, five), "a clock set back");

    let l = layout("wake");
    install(&l, "claude", "2.1.280");
    install(&l, "codex", "0.156.0");
    let f = Vendors::default();
    f.head(CLAUDE_HEAD, "2.1.280", Some("\"c1\""));
    let mut w = HeadWatch::new(&[String::from("codex")]);
    check(&mut w, &l, &f, at(0));
    assert!(!w.due(at(55)), "still on the cadence");
    let (_hold, parked) = std::sync::mpsc::channel::<()>();
    let idle = std::thread::spawn(move || {
        let _ = parked.recv();
        CompletedHead {
            index: 1,
            watched: Watched::default(),
            outcome: CheckOutcome::default(),
            notes: Vec::new(),
        }
    });
    w.pending[1] = Some(PendingHead {
        handle: idle,
        done: Arc::new(AtomicBool::new(false)),
        wake_after: None,
    });
    let woke = at(8 * 3600);
    w.woke(woke);
    assert_eq!(
        w.pending[1].as_ref().and_then(|p| p.wake_after),
        Some(woke + AFTER_WAKE),
        "the head in flight is asked after the grace too"
    );
    w.pending[1] = None;
    assert!(!w.due(woke + Duration::from_secs(19)), "the network's 20 s");
    assert!(w.due(woke + AFTER_WAKE));
    assert!(check(&mut w, &l, &f, woke + AFTER_WAKE).is_empty());
    assert!(!w.due(woke + AFTER_WAKE + Duration::from_secs(59)));
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn the_same_head_is_offered_hourly_after_the_short_retry_is_spent() {
    let l = layout("reoffer");
    install(&l, "claude", "2.1.280");
    let f = Vendors::default();
    f.head(CLAUDE_HEAD, "2.1.281", None);
    let mut w = HeadWatch::new(&[]);
    assert_eq!(check(&mut w, &l, &f, at(0)), ["claude"]);
    w.note_pass_result("claude", false, at(0));
    assert_eq!(check(&mut w, &l, &f, at(300)), ["claude"]);
    w.note_pass_result("claude", false, at(300));
    for secs in [600, 3300] {
        assert!(check(&mut w, &l, &f, at(secs)).is_empty(), "{secs}");
    }
    assert_eq!(check(&mut w, &l, &f, at(3900)), ["claude"], "an hour on");
    f.head(CLAUDE_HEAD, "2.1.282", None);
    assert_eq!(check(&mut w, &l, &f, at(4200)), ["claude"]);
    install(&l, "claude", "2.1.282");
    w.note_pass_result("claude", true, at(4200));
    assert!(check(&mut w, &l, &f, at(4500)).is_empty());
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn an_unreachable_head_is_asked_every_minute_and_logged_once() {
    let l = layout("unreachable");
    install(&l, "claude", "2.1.280");
    let f = Vendors::default();
    f.answer(CLAUDE_HEAD, Answer::Unreachable);
    let mut w = HeadWatch::new(&[String::from("codex")]);
    assert!(check(&mut w, &l, &f, at(0)).is_empty());
    assert_eq!(w.take_notes().len(), 1);
    assert!(!w.due(at(59)));
    assert!(w.due(at(60)), "retried in a minute");
    assert!(check(&mut w, &l, &f, at(60)).is_empty());
    assert!(w.take_notes().is_empty(), "said once");
    assert!(!w.due(at(60 + 59)));
    assert!(w.due(at(60 + 60)), "then every minute");
    assert!(check(&mut w, &l, &f, at(120)).is_empty());
    assert!(w.take_notes().is_empty());
    f.head(CLAUDE_HEAD, "2.1.281", Some("\"c1\""));
    assert_eq!(check(&mut w, &l, &f, at(180)), ["claude"]);
    assert_eq!(w.take_notes().len(), 2);
    assert!(!w.due(at(180 + 59)));
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn a_refused_head_is_no_trigger_and_is_asked_at_the_cadence() {
    let l = layout("refused");
    install(&l, "claude", "2.1.280");
    install(&l, "codex", "0.156.0");
    let f = Vendors::default();
    f.head(CLAUDE_HEAD, "2.1.0281", None);
    f.head(CODEX_HEAD, "{\"tag_name\":\"v9.9.9\"}", None);
    let mut w = HeadWatch::new(&[]);
    assert!(check(&mut w, &l, &f, at(0)).is_empty());
    f.answer(CLAUDE_HEAD, Answer::Unserved);
    f.head(
        CODEX_HEAD,
        "{\"tag_name\":\"rust-v0.157.0\",\"tag_name\":\"rust-v0.157.0\"}",
        None,
    );
    assert!(!w.due(at(59)));
    assert!(check(&mut w, &l, &f, at(60)).is_empty());
    assert!(w.take_notes().is_empty(), "nothing to log");
    assert!(
        !w.due(at(119)),
        "the normal cadence, not the unreachable retry"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn a_legacy_or_unrecorded_build_is_older_than_any_head() {
    let l = layout("legacy");
    activate(&l, "claude", 2_026_092_201);
    activate(&l, "codex", v("0.157.0").build_id());
    let f = Vendors::default();
    f.head(CLAUDE_HEAD, "2.1.280", None);
    f.codex("0.156.0", None);
    let mut w = HeadWatch::new(&[]);
    assert_eq!(check(&mut w, &l, &f, at(0)), ["claude", "codex"]);
    assert_eq!(w.take_notes().len(), 2);
    crate::vendor_direct::ProgramStamp::record_legacy_version(
        &l,
        "claude",
        2_026_092_201,
        v("2.1.280"),
    )
    .unwrap();
    let mut w = HeadWatch::new(&[String::from("codex")]);
    assert!(check(&mut w, &l, &f, at(0)).is_empty());
    f.head(CLAUDE_HEAD, "2.1.281", None);
    assert_eq!(check(&mut w, &l, &f, at(300)), ["claude"]);
    assert_eq!(w.take_notes().len(), 1);
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn a_program_the_watch_leaves_alone_costs_no_request() {
    let f = Vendors::default();
    f.head(CLAUDE_HEAD, "2.1.281", None);
    f.codex("0.157.0", None);
    let l = layout("skip-absent");
    let mut w = HeadWatch::new(&[]);
    assert!(check(&mut w, &l, &f, at(0)).is_empty());
    assert!(f.take_gets().is_empty());
    let _ = std::fs::remove_dir_all(&l.prefix);
    let l = layout("skip");
    install(&l, "claude", "2.1.280");
    install(&l, "codex", "0.156.0");
    let mut w = HeadWatch::new(&[String::from("claude")]);
    std::fs::write(l.removed(), "# removed\ncodex\n").unwrap();
    assert!(check(&mut w, &l, &f, at(0)).is_empty());
    assert!(f.take_gets().is_empty(), "excluded and removed");
    std::fs::remove_file(l.removed()).unwrap();
    crate::pin::set_pinned(&l, "codex", true).unwrap();
    assert!(check(&mut w, &l, &f, at(300)).is_empty());
    assert!(f.take_gets().is_empty(), "held by a local pin");
    crate::pin::set_pinned(&l, "codex", false).unwrap();
    assert_eq!(
        check(&mut w, &l, &f, at(600)),
        ["codex"],
        "the negative control"
    );
    assert_eq!(f.take_gets(), [(CODEX_HEAD.to_string(), None)]);
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn an_exclusion_edited_mid_watch_is_heard_at_the_next_check() {
    let l = layout("live-exclude");
    install(&l, "claude", "2.1.280");
    let f = Vendors::default();
    f.head(CLAUDE_HEAD, "2.1.281", None);
    let all: Vec<String> = VENDORS.iter().map(|s| s.program.to_string()).collect();
    let others: Vec<String> = all.iter().filter(|p| *p != "claude").cloned().collect();
    let mut w = HeadWatch::new(&others);
    w.set_exclude(&all);
    assert!(!w.due(at(0)), "every watched head is excluded");
    assert!(check(&mut w, &l, &f, at(0)).is_empty());
    assert!(f.take_gets().is_empty(), "excluded: no request");
    w.set_exclude(&others);
    assert!(w.due(at(5)), "included again: due at once");
    assert_eq!(check(&mut w, &l, &f, at(5)), ["claude"]);
    assert_eq!(f.take_gets(), [(CLAUDE_HEAD.to_string(), None)]);
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// A GET for the heads these round tests watch: claude answers `claude` (after `hold` lets it
/// go, when set), codex answers a moved head, and every other vendor refuses (no move).
fn round_get(
    claude: &'static str,
    hold: Arc<(Mutex<bool>, Condvar)>,
) -> Arc<ConcurrentVendorGetFn<'static>> {
    Arc::new(move |program: &str, url: &str, _cap, _etag: Option<&str>| {
        let bytes = match program {
            "claude" => {
                let (lock, ready) = &*hold;
                let held = lock.lock().unwrap();
                let (held, _) = ready
                    .wait_timeout_while(held, 2 * HANG, |held| *held)
                    .unwrap();
                assert!(!*held, "the test released claude's GET");
                claude.as_bytes().to_vec()
            }
            "codex" => br#"{"tag_name":"rust-v0.157.0","assets":[]}"#.to_vec(),
            _ => return Err(VendorFetchError::Refused(String::from("HTTP 404"))),
        };
        Ok(VendorGet::Body {
            bytes,
            etag: None,
            effective_url: url.to_string(),
        })
    })
}

fn release(hold: &(Mutex<bool>, Condvar)) {
    *hold.0.lock().unwrap() = false;
    hold.1.notify_all();
}

/// Four installed programs, each a head the watch follows.
fn four_installed(label: &str) -> crate::testdir::ScratchLayout {
    let l = layout(label);
    install(&l, "claude", "2.1.280");
    install(&l, "codex", "0.156.0");
    install(&l, "agy", "1.0.0");
    install(&l, "muse", "1.0.0");
    l
}

/// The index of `program` in the watch's table.
fn slot(watch: &HeadWatch, program: &str) -> usize {
    watch
        .watched
        .iter()
        .position(|(spec, _)| spec.program == program)
        .unwrap()
}

/// EVERY DUE, INSTALLED HEAD STARTS IN ONE ROUND, so all of them share one tick: with four
/// installed programs due at once, all four GETs are in flight together, and after the round
/// each is next asked at the same moment, one cadence on. NEGATIVE CONTROL: the old two-a-round
/// cap (`due.truncate(2)`) leaves two in flight and staggers the rest a round later.
#[test]
fn every_due_head_starts_in_one_round() {
    let l = four_installed("one-round");
    let hold = Arc::new((Mutex::new(true), Condvar::new()));
    let get = round_get("2.1.280", Arc::clone(&hold));
    let mut watch = HeadWatch::new(&[]);
    let rest = ringing(&mut watch);
    assert!(watch.check_pending(&l, at(0), &get).is_empty());
    assert_eq!(watch.pending_count(), 4, "every due head is in flight");
    release(&hold);
    while watch.has_pending() {
        rang(&rest);
        let _ = watch.check_pending(&l, at(0), &get);
    }
    let next: Vec<_> = ["claude", "codex", "agy", "muse"]
        .iter()
        .map(|program| watch.watched[slot(&watch, program)].1.next_check_at)
        .collect();
    assert_eq!(next, vec![Some(at(0) + CADENCE); 4], "one shared tick");
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// A ROUND RINGS ITS HOST ONCE PER MOVED HEAD AND ONCE MORE AT AN UNMOVED END, NOT AT EVERY
/// ANSWER: each worker rings at most once, when its head moved or it is the round's last. Claude's
/// GET is held while codex moves and two others answer unmoved. The moved codex rings, and is
/// harvested while claude is still held; the unmoved answers ring nothing; claude, the round's
/// unmoved last, rings its end. Two rings in all. Then a round of codex alone, moved and last,
/// rings once. NEGATIVE CONTROL: ringing at every answer wakes the host at an unmoved answer, so
/// its harvest can come before codex's offer, and counts four; ringing only at the round's end
/// never harvests codex while claude is held, and the hang detector fails the test; ringing for a
/// move and again for the end rings a moved last worker twice.
#[test]
fn a_round_rings_per_moved_head_and_once_at_an_unmoved_end() {
    let l = four_installed("round-rings");
    let hold = Arc::new((Mutex::new(true), Condvar::new()));
    let get = round_get("2.1.280", Arc::clone(&hold));
    let mut watch = HeadWatch::new(&[]);
    let rest = Rest::inert();
    let rings = Arc::new(AtomicUsize::new(0));
    let (counted, inner) = (Arc::clone(&rings), rest.ringer());
    watch.bind_ring(Ring::new(move || {
        counted.fetch_add(1, Ordering::SeqCst);
        inner.ring();
    }));
    assert!(watch.check_pending(&l, at(0), &get).is_empty());
    rang(&rest);
    assert_eq!(watch.check_pending(&l, at(0), &get), ["codex"]);
    let (agy, muse) = (slot(&watch, "agy"), slot(&watch, "muse"));
    aterm_spec::wait::until_within(HANG, "the unmoved answers end", || {
        let done = |i: usize| {
            watch.pending[i]
                .as_ref()
                .is_none_or(|p| p.handle.is_finished())
        };
        (done(agy) && done(muse)).then_some(())
    });
    assert_eq!(rings.load(Ordering::SeqCst), 1, "only the moved head rang");
    assert!(
        watch.pending[slot(&watch, "claude")].is_some(),
        "claude is still held"
    );
    release(&hold);
    rang(&rest);
    assert!(watch.check_pending(&l, at(0), &get).is_empty());
    assert_eq!(
        watch.pending_count(),
        0,
        "the round's end harvests the rest"
    );
    assert_eq!(rings.load(Ordering::SeqCst), 2, "and the round's last rang");

    let mut alone = HeadWatch::new(&["claude", "agy", "muse"].map(String::from));
    let (counted, inner) = (Arc::clone(&rings), rest.ringer());
    alone.bind_ring(Ring::new(move || {
        counted.fetch_add(1, Ordering::SeqCst);
        inner.ring();
    }));
    assert!(alone.check_pending(&l, at(0), &get).is_empty());
    let codex = slot(&alone, "codex");
    aterm_spec::wait::until_within(HANG, "codex's worker ends", || {
        alone.pending[codex]
            .as_ref()
            .is_some_and(|p| p.handle.is_finished())
            .then_some(())
    });
    assert_eq!(
        rings.load(Ordering::SeqCst),
        3,
        "a moved last worker rings once"
    );
    rang(&rest);
    assert_eq!(alone.check_pending(&l, at(0), &get), ["codex"]);
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// A HEAD WORKER THAT PANICS STILL RINGS ITS ROUND'S END, from its drop guard: its host wakes,
/// harvests the dead worker as unavailable and asks that head a cadence later. NEGATIVE CONTROL:
/// a worker that rings from its body, after the GET, never rings, and the hang detector fails.
#[test]
fn a_panicking_head_worker_still_rings_the_round_end() {
    let l = layout("panicking-head");
    install(&l, "claude", "2.1.280");
    let get: Arc<ConcurrentVendorGetFn<'static>> =
        Arc::new(|_: &str, _: &str, _, _: Option<&str>| panic!("an injected GET panic"));
    let mut watch = HeadWatch::new(&[String::from("codex")]);
    let rest = ringing(&mut watch);
    assert!(watch.check_pending(&l, at(0), &get).is_empty());
    rang(&rest);
    assert!(watch.check_pending(&l, at(0), &get).is_empty());
    assert_eq!(watch.pending_count(), 0);
    let claude = slot(&watch, "claude");
    assert_eq!(watch.watched[claude].1.next_check_at, Some(at(0) + CADENCE));
    assert!(
        watch
            .take_notes()
            .iter()
            .any(|note| note == "vendor head worker unavailable: claude")
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// THE WATCH NAMES ITS NEXT WORK FOR ITS HOST'S ONE WAIT: now for a head never asked, a head's
/// deadline once asked, nothing while a round is in flight (its ring is the event), now again
/// while a moved head a higher-priority end deferred waits to be replayed (that wait is zero
/// once: the replay takes it), now for a deadline set back past a cadence, and nothing when
/// every head is excluded. NEGATIVE CONTROL: a next_due blind to the deferred move waits a whole
/// cadence to replay a head already found moved.
#[test]
fn next_due_names_the_heads_deadline_and_waits_on_a_round_in_flight() {
    let l = layout("next-due");
    install(&l, "claude", "2.1.280");
    let only_claude: Vec<String> = VENDORS
        .iter()
        .map(|spec| spec.program.to_string())
        .filter(|program| program != "claude")
        .collect();
    let mut watch = HeadWatch::new(&only_claude);
    assert_eq!(watch.next_due(at(0)), Some(at(0)), "never asked");
    let hold = Arc::new((Mutex::new(true), Condvar::new()));
    let get = round_get("2.1.281", Arc::clone(&hold));
    let rest = ringing(&mut watch);
    assert!(watch.check_pending(&l, at(0), &get).is_empty());
    assert_eq!(watch.next_due(at(0)), None, "the round's ring is the event");
    release(&hold);
    rang(&rest);
    assert_eq!(watch.check_pending(&l, at(0), &get), ["claude"]);
    assert_eq!(watch.next_due(at(1)), Some(at(0) + CADENCE));
    watch.defer_moved(&["claude"]);
    assert_eq!(
        watch.next_due(at(1)),
        Some(at(1)),
        "the deferred move replays now"
    );
    assert_eq!(watch.check_pending(&l, at(1), &get), ["claude"]);
    assert_eq!(watch.next_due(at(1)), Some(at(0) + CADENCE), "once");
    assert_eq!(
        watch.next_due(at(0) - Duration::from_secs(3600)),
        Some(at(0) - Duration::from_secs(3600)),
        "a deadline set back past a cadence is due"
    );
    watch.set_exclude(
        &VENDORS
            .iter()
            .map(|spec| spec.program.to_string())
            .collect::<Vec<_>>(),
    );
    assert_eq!(watch.next_due(at(1)), None, "nothing watched");
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// A ROUND'S DONE WORKERS ARE DUE, WHOEVER TOOK THEIR RING. A host wait that ends before the
/// host's round (the window lane's stand-by while Automatic updates are off, or a park that ends
/// on a bump, a held move or an index) takes the ring a round's end made, and the round must still
/// be harvested at the host's next wait, not at the end of a park. With claude's GET held, agy and
/// muse answer unmoved and ring nothing, yet are due now; once they are harvested the round still
/// in flight waits on its ring; claude's end rings, a wait that harvests nothing takes that ring,
/// and the round is still due now, then harvested. MUTANTS: a next_due that waits on every round
/// in flight (as it did) answers None once the ring was taken, so the window waits out its park;
/// one due only once every worker is done leaves agy's and muse's answers for the round's end.
#[test]
fn a_rounds_done_workers_are_due_whoever_took_their_ring() {
    let l = four_installed("done-due");
    let hold = Arc::new((Mutex::new(true), Condvar::new()));
    let get = round_get("2.1.280", Arc::clone(&hold));
    let mut watch = HeadWatch::new(&[String::from("codex")]);
    let rest = ringing(&mut watch);
    assert!(watch.check_pending(&l, at(0), &get).is_empty());
    assert_eq!(
        watch.pending_count(),
        3,
        "claude, agy and muse are in flight"
    );
    let (agy, muse) = (slot(&watch, "agy"), slot(&watch, "muse"));
    aterm_spec::wait::until_within(HANG, "agy's and muse's workers are done", || {
        let done = |i: usize| watch.pending[i].as_ref().is_some_and(PendingHead::done);
        (done(agy) && done(muse)).then_some(())
    });
    assert!(rest.take().is_none(), "unmoved answers ring nothing");
    assert_eq!(
        watch.next_due(at(0)),
        Some(at(0)),
        "done workers are due now"
    );
    assert!(watch.check_pending(&l, at(0), &get).is_empty());
    assert_eq!(watch.pending_count(), 1, "agy and muse are harvested");
    assert_eq!(
        watch.next_due(at(0)),
        None,
        "claude still runs: its ring is the event"
    );
    release(&hold);
    rang(&rest);
    assert_eq!(
        watch.next_due(at(0)),
        Some(at(0)),
        "the round's end is due, though a wait that harvested nothing took its ring"
    );
    assert!(
        watch.check_pending(&l, at(0), &get).is_empty(),
        "claude did not move"
    );
    assert_eq!(watch.pending_count(), 0, "the round is harvested");
    assert_eq!(
        watch.next_due(at(1)),
        Some(at(0) + CADENCE),
        "then the heads' next tick"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// A STANDING SESSION KEEPS ONE WAITER PER LOCK HOWEVER OFTEN IT IS TURNED OFF AND ON. A session
/// refused the seat arms a waiter that blocks on it. Turning Automatic updates off lets the claim
/// go (`release_host`) but cannot interrupt the waiter's blocking take, so each claim refused the
/// seat after that takes the waiter still blocked on it, instead of arming another beside it (one
/// more thread and open file per toggle). Once the seat frees, the waiter hands it to the
/// session's latest claim and rings. A waiter whose session let its claim go and made none since
/// lets the seat go the moment it takes it, so a third session sits. MUTANTS: arming a new waiter
/// at every refusal leaves a second waiter blocked at the first toggle; a waiter taken on but still
/// naming the claim that armed it lets the seat go when it frees, and the hang detector fails the
/// session's wait for its ring.
#[test]
fn a_standing_session_keeps_one_waiter_per_lock_across_toggles() {
    let l = layout("toggles");
    let mut sitter = host::HostClaim::new(Host::Session, &l, None);
    assert_eq!(sitter.admit().0, host::Admit::Yes);
    sitter.end_round();
    let mut session = HeadWatch::hosted(&[], Host::Session);
    let rest = ringing(&mut session);
    for toggle in 0..3 {
        assert!(
            !session.admitted(&l, at(0)),
            "refused the seat (toggle {toggle})"
        );
        assert!(session.admission_waits(), "it waits on a waiter");
        assert_eq!(
            session.waiters.running(),
            1,
            "one waiter blocks on the seat (toggle {toggle})"
        );
        session.release_host();
    }
    assert!(!session.admitted(&l, at(0)), "turned on again: refused");
    drop(sitter);
    rang(&rest);
    assert!(
        session.admitted(&l, at(0)),
        "its latest claim takes the seat its waiter took"
    );
    assert!(session.holds_seat() && session.seated_session());
    session.end_session_update_look();

    let mut other = HeadWatch::hosted(&[], Host::Session);
    let other_rest = ringing(&mut other);
    assert!(!other.admitted(&l, at(0)), "a second session is refused");
    assert_eq!(other.waiters.running(), 1);
    other.release_host();
    session.release_host();
    aterm_spec::wait::until_within(HANG, "the let-go claim's waiter ended", || {
        (other.waiters.running() == 0).then_some(())
    });
    let mut third = host::HostClaim::new(Host::Session, &l, None);
    assert_eq!(
        third.admit().0,
        host::Admit::Yes,
        "that waiter let the seat go"
    );
    assert!(
        other_rest.take().is_none(),
        "a waiter no claim took on rings nobody"
    );
    drop(third);
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// A SEATED SESSION KEEPS ONE RENDEZVOUS WAITER ACROSS TOGGLES, AND A REFUSAL OF THE SEAT NEVER
/// TAKES IT ON. A seated session refused the rendezvous (a window holds it) arms a waiter that
/// blocks on it. Turned off (`release_host`), the session lets the seat go but cannot interrupt
/// that waiter; each time it is turned on, seated again and refused the rendezvous, it takes the
/// waiter on. Turned on while a second session sits, it is refused the seat: that refusal arms a
/// seat waiter beside the rendezvous's, one per lock, and the rendezvous waiter is taken on only
/// once the seat is the session's again, so a claim without the seat never waits on the
/// rendezvous and never rounds without the seat. The seat waiter hands over the seat when the
/// second session goes, the rendezvous waiter hands over the rendezvous when the window closes,
/// and the session rounds. MUTANTS: arming every refusal on the seat's line takes the rendezvous
/// waiter on for the seat, and the count of one waiter per lock fails; a rendezvous waiter taken
/// on but still naming the claim that armed it lets the rendezvous go when the window closes, and
/// the hang detector fails the session's wait for its ring.
#[test]
fn a_seated_session_keeps_one_rendezvous_waiter_across_toggles() {
    let l = layout("hosts-toggles");
    let mut window = HeadWatch::hosted(&[], Host::Window);
    assert!(window.sit(&l), "a window holds the rendezvous");
    let mut session = HeadWatch::hosted(&[], Host::Session);
    let rest = ringing(&mut session);
    for toggle in 0..3 {
        assert!(
            !session.admitted(&l, at(0)),
            "seated, refused the rendezvous (toggle {toggle})"
        );
        assert!(session.holds_seat() && session.session_behind_window());
        assert!(session.admission_waits(), "it waits on a waiter");
        assert_eq!(
            session.waiters.running(),
            1,
            "one waiter blocks on the rendezvous (toggle {toggle})"
        );
        session.release_host();
    }
    let mut other = host::HostClaim::new(Host::Session, &l, None);
    assert_eq!(
        other.admit().0,
        host::Admit::Later(CADENCE),
        "a second session sits, behind the window"
    );
    assert!(other.holds_seat());
    assert!(!session.admitted(&l, at(0)), "turned on: refused the seat");
    assert!(!session.holds_seat() && session.admission_waits());
    assert_eq!(
        session.waiters.running(),
        2,
        "a seat waiter beside the rendezvous's: one per lock"
    );
    session.release_host();
    assert!(
        !session.admitted(&l, at(0)),
        "turned on again: refused the seat"
    );
    assert_eq!(session.waiters.running(), 2, "it took the seat waiter on");
    drop(other);
    rang(&rest);
    aterm_spec::wait::until_within(HANG, "the seat waiter ended", || {
        (session.waiters.running() == 1).then_some(())
    });
    assert!(
        !session.admitted(&l, at(0)),
        "seated by its waiter, refused the rendezvous"
    );
    assert!(session.holds_seat() && session.session_behind_window());
    assert!(session.admission_waits());
    assert_eq!(
        session.waiters.running(),
        1,
        "it took the rendezvous waiter on"
    );
    drop(window);
    rang(&rest);
    assert!(
        session.admitted(&l, at(0)),
        "its claim takes the rendezvous its waiter took"
    );
    assert!(session.seated_session());
    session.end_session_update_look();
    aterm_spec::wait::until_within(HANG, "every waiter ended", || {
        (session.waiters.running() == 0).then_some(())
    });
    drop(session);
    let _ = std::fs::remove_dir_all(&l.prefix);
}
