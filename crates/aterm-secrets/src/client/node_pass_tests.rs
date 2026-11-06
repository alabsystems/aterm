// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The node service's vault pass (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.13, §11 B10): no
//! subscriber runs in any of these, as on a windowless Mac without one.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use super::*;
use crate::client::log_entries_tests::{TwoMacs, hand, newcomer, two_macs};

/// Far enough past any first sight for every rank's step and the settle grace.
const PAST_EVERY_RANK: u64 = 1_000 * 60_000;

/// A pace that looks at once: the wait for a pass to run is the test's hang detector, never this.
const QUICK: NodePassPace = NodePassPace {
    every: Duration::from_secs(60),
    again: Duration::from_secs(5),
    look: Duration::from_millis(1),
    lock_wait: aterm_spec::wait::MUST_HAPPEN,
};

/// [`two_macs`], with m40 seated and m22's newest `[everyone]` and Commons epochs leaving it out:
/// a gap on m22, which holds the keys. What m7 and m40 know stays out of it.
fn a_gap_on_m22() -> (TwoMacs, Recipient) {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let (m40, request, allowed) = newcomer(macs.root.path(), m7, "m40");
    let (bundle, logged) = hand(m7, &request, &allowed, true);
    assert!(logged);
    write_from_log_bundle(&m40, &bundle).expect("m40 writes from the log");
    post_records_to_servers(m7).expect("m7 posts the admission");
    sync_records(m22, None).expect("m22 pulls the admission");
    let m40 = request.binding().recipient();
    {
        let lock = StateLock::take(m22).unwrap();
        log_entries::retire_recipients_in_log_local(m22, &lock, &[], &BTreeSet::from([m40]))
            .expect("a re-key that leaves m40 out");
    }
    assert!(!wrapped(m22, &m40), "the fixture leaves a gap");
    (macs, m40)
}

fn wrapped(paths: &Paths, recipient: &Recipient) -> bool {
    load_records(paths)
        .unwrap()
        .log()
        .unwrap()
        .wraps_maximal_audience_to(&EpochAudience::Everyone, recipient)
}

/// Sets its loop's stop when the test leaves it.
struct Running<'scope> {
    stop: &'scope AtomicBool,
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

/// [`run_node_pass`] on `paths` on its own thread, its clock `clock`; `held` stands for the
/// node's switches.
fn spawn_pass<'scope>(
    scope: &'scope std::thread::Scope<'scope, '_>,
    paths: &'scope Paths,
    stop: &'scope AtomicBool,
    clock: &'scope AtomicU64,
    held: &'scope (dyn Fn() -> bool + Sync),
) -> Running<'scope> {
    scope.spawn(move || {
        run_node_pass(&NodePassHost {
            paths,
            stop,
            held: &|| held(),
            may_post: &|| true,
            start_converge: &|| None,
            now_ms: &|| clock.load(Ordering::Acquire),
            log: &|_| {},
            pace: QUICK,
        });
    });
    Running { stop }
}

fn a_pass_ran_after(paths: &Paths, after_ms: u64) {
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "a node pass keeps its end",
        || {
            node_pass_receipt(paths)
                .filter(|receipt| receipt.ran_ms >= after_ms)
                .map(|_| ())
        },
    );
}

#[test]
fn a_mac_with_no_subscriber_heals_a_gap_and_materializes_a_new_record() {
    let (macs, m40) = a_gap_on_m22();
    let (m22, m7) = (&macs.m22, &macs.m7);
    let start = aterm_time::unix_millis().unwrap();
    let (clock22, clock7) = (AtomicU64::new(start), AtomicU64::new(start));
    let (stop22, stop7) = (AtomicBool::new(false), AtomicBool::new(false));
    std::thread::scope(|scope| {
        let _m22 = spawn_pass(scope, m22, &stop22, &clock22, &|| false);
        let _m7 = spawn_pass(scope, m7, &stop7, &clock7, &|| false);

        // The first look sees the gap; its rank's time comes by the clock, with no new record.
        a_pass_ran_after(m22, start);
        clock22.store(start + PAST_EVERY_RANK, Ordering::Release);
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "m22's pass heals m40's gap by a timer's pass",
            || wrapped(m22, &m40).then_some(()),
        );

        // A record arrives by no path the pass knows of. The clock does not move, so only the
        // record makes a pass run, and it writes the file from what m7 holds, with no network.
        a_pass_ran_after(m7, start);
        macs.server.stop();
        test_put_everyone(m22, "later", b"later-v1").expect("m22 signs an entry");
        save_records(m7, &load_records(m22).unwrap().encode()).expect("the record reaches m7");
        let later = m7.home.join(".secrets/later");
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "m7's pass materializes the entry its log just took",
            || (std::fs::read(&later).ok()? == b"later-v1").then_some(()),
        );
        assert_eq!(
            clock7.load(Ordering::Acquire),
            start,
            "no timer's pass ran for this"
        );
    });
}

/// The negative control: the same gap, clock and record with the pass held, as `[swarm]` holds it
/// or `MESH` off never starts it, and the gap and the missing file stay.
#[test]
fn a_held_pass_leaves_the_gap_and_writes_no_file() {
    let (macs, m40) = a_gap_on_m22();
    let (m22, m7) = (&macs.m22, &macs.m7);
    let start = aterm_time::unix_millis().unwrap();
    let clock = AtomicU64::new(start + PAST_EVERY_RANK);
    let (stop22, stop7) = (AtomicBool::new(false), AtomicBool::new(false));
    let asked = AtomicUsize::new(0);
    let held = || {
        asked.fetch_add(1, Ordering::AcqRel);
        true
    };
    test_put_everyone(m22, "later", b"later-v1").expect("m22 signs an entry");
    save_records(m7, &load_records(m22).unwrap().encode()).expect("the record reaches m7");
    std::thread::scope(|scope| {
        let _m22 = spawn_pass(scope, m22, &stop22, &clock, &held);
        let _m7 = spawn_pass(scope, m7, &stop7, &clock, &held);
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "each loop looked several times",
            || (asked.load(Ordering::Acquire) >= 40).then_some(()),
        );
    });
    assert!(!wrapped(m22, &m40));
    assert!(!m7.home.join(".secrets/later").exists());
    assert_eq!(node_pass_receipt(m22), None);
    assert_eq!(node_pass_receipt(m7), None);
}

#[test]
fn a_person_who_turned_the_agent_off_holds_the_pass() {
    let (macs, m40) = a_gap_on_m22();
    let m22 = &macs.m22;
    crate::service::record_subscriber_off(&m22.home).unwrap();
    let clock = AtomicU64::new(aterm_time::unix_millis().unwrap() + PAST_EVERY_RANK);
    let stop = AtomicBool::new(false);
    let asked = AtomicUsize::new(0);
    let not_held = || {
        asked.fetch_add(1, Ordering::AcqRel);
        false
    };
    std::thread::scope(|scope| {
        let _m22 = spawn_pass(scope, m22, &stop, &clock, &not_held);
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the loop looked several times",
            || (asked.load(Ordering::Acquire) >= 20).then_some(()),
        );
    });
    assert!(!wrapped(m22, &m40));
    assert_eq!(node_pass_receipt(m22), None);
}

#[test]
fn a_pass_waits_out_a_writer_and_then_heals() {
    let (macs, m40) = a_gap_on_m22();
    let m22 = &macs.m22;
    let now = aterm_time::unix_millis().unwrap();
    let mut pass = NodePass::default();
    let _ = pass.once(m22, now, QUICK.lock_wait, &|| false).unwrap();

    let mut writer = Some(StateLock::take(m22).unwrap());
    let blocked = std::rc::Rc::new(std::cell::Cell::new(0));
    let seen = std::rc::Rc::clone(&blocked);
    OBSERVE_BLOCKED.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            seen.set(seen.get() + 1);
            drop(writer.take());
        }));
    });
    let (receipt, error) = pass
        .once(
            m22,
            now + PAST_EVERY_RANK,
            aterm_spec::wait::MUST_HAPPEN,
            &|| false,
        )
        .unwrap();
    OBSERVE_BLOCKED.with(|hook| *hook.borrow_mut() = None);
    assert!(blocked.get() > 0, "the pass met the writer and waited");
    assert_eq!(receipt.said, NodePassSaid::Done, "{error:?}");
    assert!(wrapped(m22, &m40));
}

#[test]
fn a_pass_past_its_wait_says_busy_and_reads_nothing_as_empty() {
    let (macs, m40) = a_gap_on_m22();
    let m22 = &macs.m22;
    let now = aterm_time::unix_millis().unwrap();
    let mut pass = NodePass::default();
    let writer = StateLock::take(m22).unwrap();
    let (receipt, error) = pass
        .once(m22, now + PAST_EVERY_RANK, Duration::ZERO, &|| false)
        .unwrap();
    assert_eq!(receipt.said, NodePassSaid::Busy, "{error:?}");
    assert_eq!(error.unwrap().kind(), crate::ErrorKind::VaultBusy);
    assert_eq!(receipt.signed, 0);

    // Busy brings the next pass soon, where a pass that found nothing would wait its minute.
    let mut due = NodePassDue::default();
    due.ran(None, now, QUICK.again);
    assert!(due.due(None, now + 5_000));
    let mut idle = NodePassDue::default();
    idle.ran(None, now, QUICK.every);
    assert!(!idle.due(None, now + 5_000));

    drop(writer);
    let (receipt, error) = pass.once(m22, now, QUICK.lock_wait, &|| false).unwrap();
    assert_eq!(receipt.said, NodePassSaid::Done, "{error:?}");
    let (receipt, error) = pass
        .once(m22, now + PAST_EVERY_RANK, QUICK.lock_wait, &|| false)
        .unwrap();
    assert_eq!(receipt.said, NodePassSaid::Done, "{error:?}");
    assert!(wrapped(m22, &m40), "once the writer went the pass healed");
}

#[test]
fn a_pass_is_due_on_a_new_record_on_its_timer_and_after_a_clock_set_back() {
    let stamp = |len: u64| {
        Some(fsx::FileStamp {
            dev: 1,
            ino: 2,
            len,
            mtime_ns: 3,
            ctime_ns: 4,
        })
    };
    let mut due = NodePassDue::default();
    assert!(due.due(stamp(1), 1_000), "the first look always runs");
    due.ran(stamp(1), 1_000, Duration::from_secs(60));
    assert!(!due.due(stamp(1), 1_000));
    assert!(!due.due(stamp(1), 60_999));
    assert!(due.due(stamp(1), 61_000), "every 60 seconds");
    assert!(due.due(stamp(2), 1_000), "a new record, at once");
    assert!(due.due(None, 1_000), "a file gone is news too");
    assert!(due.due(stamp(1), 999), "a clock set back");
    // A record that came while a pass ran is due at the next look.
    due.ran(stamp(1), 2_000, Duration::from_secs(60));
    assert!(due.due(stamp(3), 2_000));
}

#[test]
fn a_file_a_program_keeps_rewriting_is_put_back_once() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let m7 = &macs.m7;
    let file = m7.home.join(".secrets/shared");
    let now = aterm_time::unix_millis().unwrap();
    let mut pass = NodePass::default();
    let _ = pass.once(m7, now, QUICK.lock_wait, &|| false).unwrap();

    std::fs::write(&file, b"a program's").unwrap();
    let (receipt, error) = pass.once(m7, now, QUICK.lock_wait, &|| false).unwrap();
    assert_eq!(receipt.said, NodePassSaid::Done, "{error:?}");
    assert_eq!(std::fs::read(&file).unwrap(), b"everyone-v1");

    std::fs::write(&file, b"a program's").unwrap();
    let (receipt, error) = pass.once(m7, now, QUICK.lock_wait, &|| false).unwrap();
    assert_eq!(receipt.said, NodePassSaid::Done, "{error:?}");
    assert_eq!(std::fs::read(&file).unwrap(), b"a program's", "only once");
}

/// A file its apply could not write stays due: the next pass writes it once it can, and a pass
/// says nothing finished until then. The subscriber's sync does the same through `failed`.
#[test]
fn a_failed_apply_is_tried_again_with_no_new_record() {
    use std::os::unix::fs::PermissionsExt as _;
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let now = aterm_time::unix_millis().unwrap();
    let mut pass = NodePass::default();
    let parent = m7.home.join(".config/tool");
    std::fs::create_dir_all(&parent).unwrap();
    let mode = |bits| {
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(bits)).unwrap();
    };
    mode(0o500);
    test_put_everyone(m22, "~/.config/tool/token", b"token-v1").unwrap();
    save_records(m7, &load_records(m22).unwrap().encode()).unwrap();
    let (receipt, _) = pass.once(m7, now, QUICK.lock_wait, &|| false).unwrap();
    assert_eq!(receipt.said, NodePassSaid::Failed);
    let (receipt, _) = pass.once(m7, now, QUICK.lock_wait, &|| false).unwrap();
    assert_eq!(
        receipt.said,
        NodePassSaid::Failed,
        "no pass reads this as done"
    );

    mode(0o700);
    let (receipt, error) = pass.once(m7, now, QUICK.lock_wait, &|| false).unwrap();
    assert_eq!(receipt.said, NodePassSaid::Done, "{error:?}");
    assert_eq!(std::fs::read(parent.join("token")).unwrap(), b"token-v1");
}

/// A Mac that holds its vault as a log alone keeps the rule too, with no envelope to read.
#[test]
fn a_log_only_mac_puts_a_rewritten_file_back_once() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let m7 = &macs.m7;
    let (m40, request, allowed) = newcomer(macs.root.path(), m7, "m40");
    let (bundle, _) = hand(m7, &request, &allowed, true);
    write_from_log_bundle(&m40, &bundle).unwrap();
    assert!(load_state(&m40).unwrap().digest.is_none(), "log-only");
    let file = m40.home.join(".secrets/shared");
    let now = aterm_time::unix_millis().unwrap();
    let mut pass = NodePass::default();
    let _ = pass.once(&m40, now, QUICK.lock_wait, &|| false).unwrap();

    std::fs::write(&file, b"a program's").unwrap();
    let (receipt, error) = pass.once(&m40, now, QUICK.lock_wait, &|| false).unwrap();
    assert_eq!(receipt.said, NodePassSaid::Done, "{error:?}");
    assert_eq!(std::fs::read(&file).unwrap(), b"everyone-v1");

    std::fs::write(&file, b"a program's").unwrap();
    let (receipt, error) = pass.once(&m40, now, QUICK.lock_wait, &|| false).unwrap();
    assert_eq!(receipt.said, NodePassSaid::Done, "{error:?}");
    assert_eq!(std::fs::read(&file).unwrap(), b"a program's", "only once");
}

/// A writer on the author's lock, which no look at the state sees, makes a due heal busy within
/// the pass's wait: it never blocks in the lock past it.
#[test]
fn a_due_heal_under_the_authors_lock_says_busy_within_its_wait() {
    let (macs, m40) = a_gap_on_m22();
    let m22 = &macs.m22;
    let now = aterm_time::unix_millis().unwrap();
    let mut pass = NodePass::default();
    // A gap's rank follows its epoch's id: this Mac's turn comes at once for this test, so the
    // heal is due whichever the fixture's epochs drew.
    UNRANKED.with(|unranked| unranked.set(true));
    let author = log_entries::RecordsAuthorLock::take(m22).unwrap();
    let (receipt, error) = pass.once(m22, now, Duration::ZERO, &|| false).unwrap();
    assert_eq!(receipt.said, NodePassSaid::Busy, "{error:?}");
    assert_eq!(receipt.signed, 0);
    assert!(!wrapped(m22, &m40));
    drop(author);
    // Nothing a busy pass noted loses the heal: it is still due at the same clock.
    let (receipt, error) = pass.once(m22, now, QUICK.lock_wait, &|| false).unwrap();
    assert_eq!(receipt.said, NodePassSaid::Done, "{error:?}");
    assert!(wrapped(m22, &m40), "{receipt:?}");
}

/// A pass that signed while the LAN stayed closed dials nothing, and owes its post to the first
/// pass that may: the only way this Mac's server hears of it here.
#[test]
fn a_post_waits_for_the_lan_and_is_owed_until_it_is_made() {
    let (macs, m40) = a_gap_on_m22();
    let (m22, m7) = (&macs.m22, &macs.m7);
    let now = aterm_time::unix_millis().unwrap();
    let mut pass = NodePass::default();
    let (first, _) = pass.once(m22, now, QUICK.lock_wait, &|| false).unwrap();
    let (second, _) = pass
        .once(m22, now + PAST_EVERY_RANK, QUICK.lock_wait, &|| false)
        .unwrap();
    assert!(first.signed + second.signed > 0);
    assert!(wrapped(m22, &m40));
    sync_records(m7, None).unwrap();
    assert!(!wrapped(m7, &m40), "closed LAN: nothing reached the server");

    let _ = pass
        .once(m22, now + PAST_EVERY_RANK, QUICK.lock_wait, &|| true)
        .unwrap();
    sync_records(m7, None).unwrap();
    assert!(wrapped(m7, &m40), "the owed post reached the server");
}

/// The loop's wait after a pass that did not finish: soon, whatever the minute says.
#[test]
fn a_busy_pass_in_the_loop_runs_again_soon_and_backs_off() {
    let (macs, _) = a_gap_on_m22();
    let m22 = &macs.m22;
    let start = aterm_time::unix_millis().unwrap();
    let clock = AtomicU64::new(start);
    let stop = AtomicBool::new(false);
    let writer = StateLock::take(m22).unwrap();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            run_node_pass(&NodePassHost {
                paths: m22,
                stop: &stop,
                held: &|| false,
                may_post: &|| false,
                start_converge: &|| None,
                now_ms: &|| clock.load(Ordering::Acquire),
                log: &|_| {},
                pace: NodePassPace {
                    lock_wait: Duration::ZERO,
                    ..QUICK
                },
            });
        });
        aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "a busy pass", || {
            node_pass_receipt(m22)
                .filter(|receipt| receipt.said == NodePassSaid::Busy)
                .map(|_| ())
        });
        drop(writer);
        clock.store(start + QUICK.again.as_millis() as u64, Ordering::Release);
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the next pass comes after the short wait, well inside the minute",
            || {
                node_pass_receipt(m22)
                    .filter(|receipt| receipt.ran_ms > start)
                    .map(|_| ())
            },
        );
        stop.store(true, Ordering::Release);
    });
    let wait = |n| node_pass_wait(&NodePassPace::LIVE, n);
    assert_eq!(wait(0), Duration::from_secs(60));
    assert_eq!(wait(1), Duration::from_secs(5));
    assert_eq!(wait(2), Duration::from_secs(10));
    assert_eq!(wait(4), Duration::from_secs(40));
    assert_eq!(wait(5), Duration::from_secs(60));
    assert_eq!(wait(u32::MAX), Duration::from_secs(60));
}

/// A version the pass applied starts one converge runner, once the LAN allows, and a pass that
/// applied none starts none: the subscriber beside it finds that version unchanged.
#[test]
fn a_version_the_pass_applied_owes_one_converge_runner_once_the_lan_allows() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let start = aterm_time::unix_millis().unwrap();
    let clock = AtomicU64::new(start);
    let stop = AtomicBool::new(false);
    let lan = AtomicBool::new(false);
    let runners = AtomicUsize::new(0);
    let looked = AtomicUsize::new(0);
    let successful = converge_program(macs.root.path(), "converge-success", "exit 0");
    std::thread::scope(|scope| {
        scope.spawn(|| {
            run_node_pass(&NodePassHost {
                paths: m7,
                stop: &stop,
                held: &|| {
                    looked.fetch_add(1, Ordering::AcqRel);
                    false
                },
                may_post: &|| lan.load(Ordering::Acquire),
                start_converge: &|| {
                    runners.fetch_add(1, Ordering::AcqRel);
                    Some(ended_runner(&successful))
                },
                now_ms: &|| clock.load(Ordering::Acquire),
                log: &|_| {},
                pace: QUICK,
            });
        });
        a_pass_ran_after(m7, start);
        let looks_past = |n: usize| {
            let from = looked.load(Ordering::Acquire);
            aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "more looks", || {
                (looked.load(Ordering::Acquire) >= from + n).then_some(())
            });
        };
        looks_past(5);
        lan.store(true, Ordering::Release);
        looks_past(5);
        assert_eq!(
            runners.load(Ordering::Acquire),
            0,
            "nothing was applied yet"
        );

        lan.store(false, Ordering::Release);
        test_put_everyone(m22, "later", b"later-v1").unwrap();
        save_records(m7, &load_records(m22).unwrap().encode()).unwrap();
        let later = m7.home.join(".secrets/later");
        aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the apply", || {
            std::fs::read(&later).ok().map(|_| ())
        });
        looks_past(5);
        assert_eq!(runners.load(Ordering::Acquire), 0, "the LAN is closed");
        lan.store(true, Ordering::Release);
        aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the owed runner", || {
            (runners.load(Ordering::Acquire) >= 1).then_some(())
        });
        looks_past(5);
        assert_eq!(
            runners.load(Ordering::Acquire),
            1,
            "one version, one runner"
        );
        stop.store(true, Ordering::Release);
    });
}

fn converge_program(root: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let sh = aterm_uds::deadline::stand_in(Path::new("/bin/sh"));
    let path = root.join(name);
    std::fs::write(&path, format!("#!{}\n{body}\n", sh.display())).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path
}

fn ended_runner(exe: &Path) -> aterm_uds::spawnfd::Child {
    let mut child = crate::converge::start(exe, crate::converge::SUBSCRIBER).unwrap();
    child.wait().unwrap();
    child
}

#[test]
fn node_convergence_retries_unfinished_or_failed_runners_and_honors_the_lan_hold() {
    for code in [1, crate::converge::UNFINISHED] {
        let root = aterm_tempfile::tempdir().unwrap();
        let failed = converge_program(root.path(), "retry", &format!("exit {code}"));
        let held = converge_program(
            root.path(),
            "held",
            &format!("exit {}", crate::converge::HELD),
        );
        let success = converge_program(root.path(), "success", "exit 0");
        let calls = std::cell::Cell::new(0);
        let start = || {
            calls.set(calls.get() + 1);
            Some(ended_runner(match calls.get() {
                1 | 4 => &failed,
                2 => &held,
                _ => &success,
            }))
        };
        let mut converge = NodeConverge {
            owed: true,
            ..NodeConverge::default()
        };
        converge.tend_at(0, &|| false, &start);
        assert_eq!(calls.get(), 0, "an owed version still honors the LAN hold");
        converge.tend_at(1, &|| true, &start);
        converge.tend_at(2, &|| true, &start);
        for at in [3, 4_000, 5_001] {
            converge.tend_at(at, &|| true, &start);
        }
        assert_eq!(
            calls.get(),
            1,
            "one failure is consumed once, without a hot loop"
        );
        converge.tend_at(5_002, &|| false, &start);
        assert_eq!(calls.get(), 1, "a due retry cannot bypass the LAN hold");
        converge.tend_at(6_000, &|| true, &start);
        assert_eq!(
            calls.get(),
            2,
            "unchanged records do not strand the failure"
        );
        converge.tend_at(6_001, &|| true, &start);
        assert_eq!(calls.get(), 3, "the held pass stays owed");
        converge.tend_at(6_002, &|| true, &start);
        converge.tend_at(30_000, &|| true, &start);
        assert_eq!(calls.get(), 3, "success ends the retry streak");

        converge.owed = true;
        converge.tend_at(30_001, &|| true, &start);
        converge.tend_at(30_002, &|| true, &start);
        converge.tend_at(35_001, &|| true, &start);
        assert_eq!(calls.get(), 4);
        converge.tend_at(35_002, &|| true, &start);
        assert_eq!(
            calls.get(),
            5,
            "success reset the next failure to five seconds"
        );
        converge.tend_at(35_003, &|| true, &start);
        converge.tend_at(60_000, &|| true, &start);
        assert_eq!(calls.get(), 5);
    }
}

#[test]
fn node_convergence_coalesces_new_versions_while_a_runner_still_runs() {
    let root = aterm_tempfile::tempdir().unwrap();
    let sleep = aterm_uds::deadline::stand_in(Path::new("/bin/sleep"));
    let running = converge_program(
        root.path(),
        "running",
        &format!("exec {} 600", sleep.display()),
    );
    let success = converge_program(root.path(), "success", "exit 0");
    let calls = std::cell::Cell::new(0);
    let start = || {
        calls.set(calls.get() + 1);
        Some(if calls.get() == 1 {
            crate::converge::start(&running, crate::converge::SUBSCRIBER).unwrap()
        } else {
            ended_runner(&success)
        })
    };
    let mut converge = NodeConverge {
        owed: true,
        ..NodeConverge::default()
    };
    converge.tend_at(0, &|| true, &start);
    converge.owed = true;
    for at in [1, 2, 3] {
        converge.tend_at(at, &|| true, &start);
    }
    assert_eq!(calls.get(), 1, "new work cannot start a concurrent runner");
    let child = converge.runner.as_mut().unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
    converge.tend_at(4, &|| true, &start);
    assert_eq!(
        calls.get(),
        2,
        "new work remains owed after the failed runner"
    );
    converge.tend_at(5, &|| true, &start);
    converge.tend_at(60_000, &|| true, &start);
    assert_eq!(
        calls.get(),
        2,
        "the coalesced successful pass ends the debt"
    );
}

#[test]
fn a_mac_with_no_vault_gets_nothing_made() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(root.path());
    let mut pass = NodePass::default();
    assert!(pass.once(&paths, 1, Duration::ZERO, &|| false).is_none());
    assert!(!paths.dir.exists());
}

#[test]
fn a_receipt_reads_back_and_a_foreign_file_reads_as_none() {
    for receipt in [
        NodePassReceipt {
            ran_ms: 1_793_900_315_000,
            said: NodePassSaid::Done,
            signed: 2,
            applied: Some(41),
        },
        NodePassReceipt {
            ran_ms: 1,
            said: NodePassSaid::Busy,
            signed: 0,
            applied: None,
        },
        NodePassReceipt {
            ran_ms: 2,
            said: NodePassSaid::Failed,
            signed: 1,
            applied: None,
        },
    ] {
        assert_eq!(
            NodePassReceipt::parse(&receipt.encode()),
            Some(receipt.clone())
        );
    }
    for foreign in [
        "",
        "2 1 done 0 -",
        "1 x done 0 -",
        "1 1 done 0",
        "1 1 done 0 - more",
    ] {
        assert_eq!(NodePassReceipt::parse(foreign), None, "{foreign}");
    }
}
