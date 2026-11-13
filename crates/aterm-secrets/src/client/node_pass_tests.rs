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
    stop: &'scope aterm_uds::stop::Stop,
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.stop.set();
    }
}

/// [`run_node_pass`] on `paths` on its own thread, its clock `clock`; `held` stands for the
/// node's switches.
fn spawn_pass<'scope>(
    scope: &'scope std::thread::Scope<'scope, '_>,
    paths: &'scope Paths,
    stop: &'scope aterm_uds::stop::Stop,
    clock: &'scope AtomicU64,
    held: &'scope (dyn Fn() -> bool + Sync),
) -> Running<'scope> {
    scope.spawn(move || {
        run_node_pass(&NodePassHost {
            paths,
            stop,
            held: &|| held(),
            may_post: &|| true,
            routes: &|| [None; 3],
            make_inputs: &|| {
                assert!(!held(), "held work does not construct an observer");
                assert!(
                    !crate::service::subscriber_turned_off(&paths.home),
                    "Off work does not construct an observer"
                );
                ConvergenceInputs::new(Box::new(|_| None))
            },
            start_converge: &|_| {
                assert!(!held(), "held work does not start convergence");
                assert!(
                    !crate::service::subscriber_turned_off(&paths.home),
                    "Off work does not start convergence"
                );
                None
            },
            now_ms: &|| clock.load(Ordering::Acquire),
            log: &|_| {},
            pace: QUICK,
            rung: None,
            broker: false,
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
    let (stop22, stop7) = (aterm_uds::stop::Stop::new(), aterm_uds::stop::Stop::new());
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
    let (stop22, stop7) = (aterm_uds::stop::Stop::new(), aterm_uds::stop::Stop::new());
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
    let stop = aterm_uds::stop::Stop::new();
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
fn a_stopped_node_does_not_make_inputs_or_start_convergence() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let stop = aterm_uds::stop::Stop::new();
    stop.set();
    run_node_pass(&NodePassHost {
        paths: &macs.m7,
        stop: &stop,
        held: &|| panic!("a stopped loop reads no admission inputs"),
        may_post: &|| panic!("a stopped loop starts no vault work"),
        routes: &|| panic!("a stopped loop reads no routes"),
        make_inputs: &|| panic!("a stopped loop constructs no observer"),
        start_converge: &|_| panic!("a stopped loop starts no converge job"),
        now_ms: &|| panic!("a stopped loop reads no clock"),
        log: &|_| {},
        pace: QUICK,
        rung: None,
        broker: false,
    });
    assert_eq!(node_pass_receipt(&macs.m7), None);
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

/// The next timed pass counts from the look that read the clock, however long that look, its
/// pass and the converge after it took.
/// NEGATIVE CONTROL: a deadline counted from when the look ended comes that look's length late.
#[test]
fn a_passs_timer_counts_from_the_look_that_read_the_clock() {
    let wait = Duration::from_secs(60);
    // The look read the clock 10 s ago, and its pass ran then.
    let read = Instant::now().checked_sub(Duration::from_secs(10)).unwrap();
    let mut due = NodePassDue::default();
    assert_eq!(
        due.next_at(1_000, read),
        Some(read),
        "before the first pass, at once"
    );
    due.ran(None, 1_000, wait);
    assert_eq!(due.next_at(1_000, read), read.checked_add(wait));
    assert_eq!(
        due.next_at(31_000, read),
        read.checked_add(wait / 2),
        "a later look, half the wait on"
    );
    assert_eq!(
        due.next_at(999, read),
        Some(read),
        "a clock set back, at once"
    );
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
    let parent = m7.home.join(".config/gh");
    std::fs::create_dir_all(&parent).unwrap();
    let mode = |bits| {
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(bits)).unwrap();
    };
    mode(0o500);
    test_put_everyone(m22, "~/.config/gh/hosts.yml", b"token-v1").unwrap();
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
    assert_eq!(
        std::fs::read(parent.join("hosts.yml")).unwrap(),
        b"token-v1"
    );
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
    let stop = aterm_uds::stop::Stop::new();
    let writer = StateLock::take(m22).unwrap();
    std::thread::scope(|scope| {
        let _running = Running { stop: &stop };
        scope.spawn(|| {
            run_node_pass(&NodePassHost {
                paths: m22,
                stop: &stop,
                held: &|| false,
                may_post: &|| false,
                routes: &|| [None; 3],
                make_inputs: &|| ConvergenceInputs::new(Box::new(|_| None)),
                start_converge: &|_| None,
                now_ms: &|| clock.load(Ordering::Acquire),
                log: &|_| {},
                pace: NodePassPace {
                    lock_wait: Duration::ZERO,
                    ..QUICK
                },
                rung: None,
                broker: false,
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
        stop.set();
    });
    let wait = |n| node_pass_wait(&NodePassPace::LIVE, n);
    assert_eq!(wait(0), Duration::from_secs(60));
    assert_eq!(wait(1), Duration::from_secs(5));
    assert_eq!(wait(2), Duration::from_secs(10));
    assert_eq!(wait(4), Duration::from_secs(40));
    assert_eq!(wait(5), Duration::from_secs(60));
    assert_eq!(wait(u32::MAX), Duration::from_secs(60));
}

/// A version the pass applied starts one repair once the LAN allows. A subsequent relevant
/// local-input change owes a refresh even when the signed records and wall clock stay fixed.
#[test]
fn a_version_the_pass_applied_owes_one_converge_runner_once_the_lan_allows() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let start = aterm_time::unix_millis().unwrap();
    let clock = AtomicU64::new(start);
    let stop = aterm_uds::stop::Stop::new();
    let lan = AtomicBool::new(false);
    let held = AtomicBool::new(false);
    let runners = AtomicUsize::new(0);
    let work = std::sync::Mutex::new(Vec::new());
    let looked = AtomicUsize::new(0);
    let source = std::sync::Arc::new(AtomicUsize::new(0));
    let identity = std::sync::Arc::new(AtomicUsize::new(0));
    let made_inputs = AtomicUsize::new(0);
    let successful = converge_program(macs.root.path(), "converge-success", "exit 0");
    std::thread::scope(|scope| {
        let _running = Running { stop: &stop };
        scope.spawn(|| {
            run_node_pass(&NodePassHost {
                paths: m7,
                stop: &stop,
                held: &|| {
                    looked.fetch_add(1, Ordering::AcqRel);
                    held.load(Ordering::Acquire)
                },
                may_post: &|| lan.load(Ordering::Acquire),
                routes: &|| [None; 3],
                make_inputs: &|| {
                    made_inputs.fetch_add(1, Ordering::AcqRel);
                    let source = std::sync::Arc::clone(&source);
                    let identity = std::sync::Arc::clone(&identity);
                    let mut seen = source.load(Ordering::Acquire);
                    let mut identity_seen = identity.load(Ordering::Acquire);
                    ConvergenceInputs::new(Box::new(move |refresh| {
                        let current = source.load(Ordering::Acquire);
                        if current == seen {
                            None
                        } else {
                            seen = current;
                            Some(crate::converge::RepositoryRequest::Refresh(
                                crate::converge::RefreshRequest::inputs(
                                    crate::converge::RefreshScope::new([1; 32]),
                                    refresh.version,
                                ),
                            ))
                        }
                    }))
                    .with_refresh(Box::new(|| crate::converge::RefreshObservation {
                        version: crate::converge::RefreshVersion {
                            input: 7,
                            attempt: 9,
                            ..crate::converge::RefreshVersion::default()
                        },
                        ..crate::converge::RefreshObservation::default()
                    }))
                    .with_identity(Box::new(move || {
                        let current = identity.load(Ordering::Acquire);
                        if current == identity_seen {
                            crate::client::IdentityDebt::None
                        } else {
                            identity_seen = current;
                            crate::client::IdentityDebt::Changed
                        }
                    }))
                },
                start_converge: &|requested| {
                    work.lock().unwrap().push(requested);
                    runners.fetch_add(1, Ordering::AcqRel);
                    Some(ended_runner(&successful))
                },
                now_ms: &|| clock.load(Ordering::Acquire),
                log: &|_| {},
                pace: QUICK,
                rung: None,
                broker: false,
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
        aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "initial refresh", || {
            (runners.load(Ordering::Acquire) == 1).then_some(())
        });
        looks_past(5);
        assert_eq!(
            runners.load(Ordering::Acquire),
            1,
            "startup ensures freshness once without an applied version"
        );

        lan.store(false, Ordering::Release);
        test_put_everyone(m22, "later", b"later-v1").unwrap();
        save_records(m7, &load_records(m22).unwrap().encode()).unwrap();
        let later = m7.home.join(".secrets/later");
        aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the apply", || {
            std::fs::read(&later).ok().map(|_| ())
        });
        looks_past(5);
        assert_eq!(runners.load(Ordering::Acquire), 1, "the LAN is closed");
        lan.store(true, Ordering::Release);
        aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the owed runner", || {
            (runners.load(Ordering::Acquire) >= 2).then_some(())
        });
        looks_past(5);
        assert_eq!(
            runners.load(Ordering::Acquire),
            2,
            "the unrelated applied version owes one repair"
        );
        identity.store(1, Ordering::Release);
        looks_past(10);
        assert_eq!(
            runners.load(Ordering::Acquire),
            3,
            "a late identity input owes local repair after the initial work completed"
        );
        let records = load_records(m7).unwrap().encode();
        source.store(1, Ordering::Release);
        looks_past(10);
        assert_eq!(runners.load(Ordering::Acquire), 4);
        for subscriber_off in [false, true] {
            if subscriber_off {
                crate::service::record_subscriber_off(&m7.home).unwrap();
            } else {
                held.store(true, Ordering::Release);
            }
            looks_past(5);
            let before = runners.load(Ordering::Acquire);
            identity.fetch_add(1, Ordering::Release);
            looks_past(5);
            assert_eq!(
                runners.load(Ordering::Acquire),
                before,
                "held starts no job"
            );
            if subscriber_off {
                std::fs::remove_file(crate::service::subscriber_off_path(&m7.home)).unwrap();
            } else {
                held.store(false, Ordering::Release);
            }
            looks_past(10);
            assert_eq!(
                runners.load(Ordering::Acquire),
                before + 1,
                "a resumed node owes one refresh with unchanged records and clock"
            );
        }
        stop.set();
        assert_eq!(made_inputs.load(Ordering::Acquire), 1);
        assert_eq!(load_records(m7).unwrap().encode(), records);
        assert_eq!(clock.load(Ordering::Acquire), start);
        let requests = work.lock().unwrap();
        assert_eq!(
            requests
                .iter()
                .map(|request| request.work())
                .collect::<Vec<_>>(),
            vec![
                crate::converge::RepositoryWork::Refresh,
                crate::converge::RepositoryWork::Repair,
                crate::converge::RepositoryWork::Repair,
                crate::converge::RepositoryWork::Refresh,
                crate::converge::RepositoryWork::Refresh,
                crate::converge::RepositoryWork::Refresh,
            ],
            "a relevant local source change refreshes without another signed record"
        );
        let version = crate::converge::RefreshVersion {
            input: 7,
            attempt: 9,
            ..crate::converge::RefreshVersion::default()
        };
        assert_eq!(
            requests[0],
            crate::converge::RepositoryRequest::Refresh(crate::converge::RefreshRequest::ensure(
                None, version
            )),
            "startup shares an existing generation without inventing a later fence"
        );
        assert_eq!(
            requests[3],
            crate::converge::RepositoryRequest::Refresh(crate::converge::RefreshRequest::inputs(
                crate::converge::RefreshScope::new([1; 32]),
                version,
            )),
            "the local transition retains its sampled input epoch and expected scope"
        );
    });
}

/// A pace whose timer never comes in a test, after a pass that finished or one that did not: a
/// loop a bell rings looks only on what it hears.
const FAR: NodePassPace = NodePassPace {
    every: Duration::from_secs(3_600),
    again: Duration::from_secs(3_600),
    ..QUICK
};

/// What a loop a bell rings did, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Seen {
    /// A look: its read of the switches.
    Look,
    /// A wait, until its deadline (`None`: none).
    Wait(Option<Instant>),
    /// A change of network a wait heard.
    Moved,
    /// The loop's own watch told this key, and rings it next.
    Told(u64),
}

/// Changes of network a test makes by ringing `moved`. Each wait waits on what the loop waits on
/// too, and notes itself and what it heard.
struct TestRoutes<'a> {
    moved: &'a aterm_uds::ready::Doorbell,
    seen: &'a std::sync::Mutex<Vec<Seen>>,
}

impl super::super::member_routes::Changes for TestRoutes<'_> {
    fn wait(&mut self, deadline: Option<Instant>, also: &[BorrowedFd<'_>]) -> bool {
        self.seen.lock().unwrap().push(Seen::Wait(deadline));
        let mut fds = also.to_vec();
        fds.push(self.moved.fd());
        let _ = aterm_uds::ready::any(&fds, deadline);
        let moved = self.moved.clear();
        if moved {
            self.seen.lock().unwrap().push(Seen::Moved);
        }
        moved
    }
}

/// [`run_node_pass_hearing`] on a Mac, its bell rung by its own watches and the test, its routes
/// moved by the test, and each look and wait noted.
struct Ringing<'a> {
    paths: &'a Paths,
    stop: aterm_uds::stop::Stop,
    held: AtomicBool,
    rung: Arc<aterm_uds::ready::Doorbell>,
    moved: aterm_uds::ready::Doorbell,
    seen: Arc<std::sync::Mutex<Vec<Seen>>>,
    clock: u64,
    /// Where its LAN broker's socket would be: none, unless the test makes one.
    #[cfg(target_os = "macos")]
    broker: PathBuf,
    pace: NodePassPace,
}

impl<'a> Ringing<'a> {
    fn new(paths: &'a Paths, held: bool) -> Self {
        Self {
            paths,
            stop: aterm_uds::stop::Stop::new(),
            held: AtomicBool::new(held),
            rung: Arc::new(aterm_uds::ready::Doorbell::new().unwrap()),
            moved: aterm_uds::ready::Doorbell::new().unwrap(),
            seen: Arc::new(std::sync::Mutex::new(Vec::new())),
            clock: aterm_time::unix_millis().unwrap(),
            #[cfg(target_os = "macos")]
            broker: paths.home.join("absent-broker.sock"),
            pace: FAR,
        }
    }

    /// Run the loop until its stop, hearing its LAN broker at a socket of its own (macOS), where
    /// none answers.
    fn run(
        &self,
        make_inputs: &dyn Fn() -> ConvergenceInputs,
        start_converge: &dyn Fn(
            crate::converge::RepositoryRequest,
        ) -> Option<aterm_uds::spawnfd::Child>,
    ) {
        #[cfg(target_os = "macos")]
        aterm_uds::lan::seam_here(
            aterm_uds::lan::Seam {
                socket: Some(self.broker.clone()),
                ..aterm_uds::lan::Seam::default()
            },
            || self.hear(make_inputs, start_converge),
        );
        #[cfg(not(target_os = "macos"))]
        self.hear(make_inputs, start_converge);
    }

    fn hear(
        &self,
        make_inputs: &dyn Fn() -> ConvergenceInputs,
        start_converge: &dyn Fn(
            crate::converge::RepositoryRequest,
        ) -> Option<aterm_uds::spawnfd::Child>,
    ) {
        run_node_pass_hearing(
            &NodePassHost {
                paths: self.paths,
                stop: &self.stop,
                held: &|| {
                    self.seen.lock().unwrap().push(Seen::Look);
                    self.held.load(Ordering::Acquire)
                },
                may_post: &|| true,
                routes: &|| [None; 3],
                make_inputs,
                start_converge,
                now_ms: &|| self.clock,
                log: &|_| {},
                pace: self.pace,
                rung: Some(Arc::clone(&self.rung)),
                broker: true,
            },
            || TestRoutes {
                moved: &self.moved,
                seen: &self.seen,
            },
            {
                let seen = Arc::clone(&self.seen);
                move |key| seen.lock().unwrap().push(Seen::Told(key))
            },
        );
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    /// Wait until `holds` of what the loop did since its `from`th note.
    fn until(&self, what: &str, from: usize, holds: impl Fn(&[Seen]) -> bool) {
        aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, what, || {
            holds(&self.seen()[from..]).then_some(())
        });
    }

    /// Wait until the loop's watch told each of `keys` (each file's first look tells, whatever
    /// it finds), the loop looked after its last tell, and it waits again with its bell quiet:
    /// from then on what rings it is what the test does. Answers where the notes stand then.
    fn quiet(&self, keys: &[u64]) -> usize {
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the watch's first looks told and looked at, and the loop waits again",
            || {
                let seen = self.seen();
                let told = seen
                    .iter()
                    .rposition(|seen| matches!(seen, Seen::Told(_)))?;
                let quiet = keys.iter().all(|key| seen.contains(&Seen::Told(*key)))
                    && seen[told..].contains(&Seen::Look)
                    && matches!(seen.last(), Some(Seen::Wait(_)))
                    && self.bell_quiet();
                quiet.then_some(seen.len())
            },
        )
    }

    /// Whether nothing rang the loop since its last look.
    fn bell_quiet(&self) -> bool {
        aterm_uds::ready::any_now(&[self.rung.fd()]).is_ok_and(|ready| ready.is_empty())
    }
}

/// A converge pass that ends at once, well: nothing it leaves owes a look sooner than its recheck.
fn ended_well(
    root: &Path,
) -> impl Fn(crate::converge::RepositoryRequest) -> Option<aterm_uds::spawnfd::Child> {
    let success = converge_program(root, "converge-success", "exit 0");
    move |_| Some(ended_runner(&success))
}

/// A held loop looks on its bell, and else only once a pass's wait, its backstop: the ring that
/// lifts the hold runs the pass, after which the loop keeps its timer's deadline.
/// NEGATIVE CONTROL: a wait that does not clear and read the bell spins on it and never looks.
#[test]
fn a_held_loop_waits_for_its_bell_and_its_backstop() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let m7 = &macs.m7;
    let ringing = Ringing::new(m7, true);
    let start = ended_well(macs.root.path());
    std::thread::scope(|scope| {
        let _running = Running {
            stop: &ringing.stop,
        };
        scope.spawn(|| {
            ringing.run(&|| ConvergenceInputs::new(Box::new(|_| None)), &start);
        });
        ringing.until("the held loop waits", 0, |seen| {
            seen.iter().any(|seen| matches!(seen, Seen::Wait(_)))
        });
        let seen = ringing.seen();
        let backstop = Instant::now() + FAR.every / 2;
        assert!(
            seen.iter()
                .all(|seen| !matches!(seen, Seen::Wait(at) if at.is_none_or(|at| at < backstop))),
            "a held loop's one deadline is its backstop: {seen:?}"
        );
        let from = seen.len();
        ringing.rung.ring();
        ringing.until("a ring looks again", from, |seen| {
            seen.contains(&Seen::Look)
        });
        assert_eq!(node_pass_receipt(m7), None, "still held");

        ringing.held.store(false, Ordering::Release);
        ringing.rung.ring();
        a_pass_ran_after(m7, ringing.clock);
        // The pass ran in a look already noted, and the waits after it may be noted too: every
        // look from the receipt's on is open, so read from the latest.
        let open = |seen: &[Seen]| seen.iter().rposition(|seen| *seen == Seen::Look);
        ringing.until("the open loop waits on its timer", 0, |seen| {
            open(seen).is_some_and(|at| {
                seen[at..]
                    .iter()
                    .any(|seen| matches!(seen, Seen::Wait(Some(_))))
            })
        });
        let seen = ringing.seen();
        let waits = seen[open(&seen).unwrap()..].to_vec();
        let latest = Instant::now() + FAR.every;
        assert!(
            waits
                .iter()
                .all(|seen| !matches!(seen, Seen::Wait(at) if at.is_none_or(|at| at > latest))),
            "no deadline past the pass's timer: {waits:?}"
        );
    });
}

/// The loop's own watch over the records file rings it: a new one runs a pass with no clock moved
/// and no deadline come. The record comes once the off switch's watch's first look was told and
/// looked at.
/// NEGATIVE CONTROL: a loop that watched no records file waits out its hour-long timer.
#[test]
fn a_ringing_loop_hears_a_new_records_file() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let ringing = Ringing::new(m7, false);
    let start = ended_well(macs.root.path());
    std::thread::scope(|scope| {
        let _running = Running {
            stop: &ringing.stop,
        };
        scope.spawn(|| {
            ringing.run(&|| ConvergenceInputs::new(Box::new(|_| None)), &start);
        });
        a_pass_ran_after(m7, ringing.clock);
        ringing.quiet(&[OFF_WATCH]);
        test_put_everyone(m22, "later", b"later-v1").expect("m22 signs an entry");
        save_records(m7, &load_records(m22).unwrap().encode()).expect("the record reaches m7");
        let later = m7.home.join(".secrets/later");
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the pass the new records file rang materializes its entry",
            || (std::fs::read(&later).ok()? == b"later-v1").then_some(()),
        );
    });
}

/// The loop's own watch over the subscriber's off switch rings it: the switch it found on, taken
/// away once the records watch's first look was told and looked at, runs the pass with no
/// deadline come.
/// NEGATIVE CONTROL: a loop that watched no off switch waits out its hour-long backstop.
#[test]
fn a_ringing_loop_hears_its_off_switch_taken_away() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let m7 = &macs.m7;
    crate::service::record_subscriber_off(&m7.home).unwrap();
    let ringing = Ringing::new(m7, false);
    let start = ended_well(macs.root.path());
    std::thread::scope(|scope| {
        let _running = Running {
            stop: &ringing.stop,
        };
        scope.spawn(|| {
            ringing.run(&|| ConvergenceInputs::new(Box::new(|_| None)), &start);
        });
        ringing.quiet(&[RECORDS_WATCH]);
        assert_eq!(node_pass_receipt(m7), None, "off");
        std::fs::remove_file(crate::service::subscriber_off_path(&m7.home)).unwrap();
        a_pass_ran_after(m7, ringing.clock);
    });
}

/// A records layout the loop cannot read, a transition another writer holds unfinished, is
/// watched by what its selection is judged by: that writer, finishing it, rings the loop, whose
/// pass then runs on the records it names, with no deadline come. The node's own pass, which
/// finishes such a transition when it takes the vault, finds the vault taken; the writer lets it
/// go and finishes once the off switch's watch's first look was told and looked at.
/// NEGATIVE CONTROL: a loop that watches no records while their layout names none waits out its
/// hour-long retry.
#[test]
fn a_ringing_loop_hears_its_unfinished_records_layout_finished() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let m7 = &macs.m7;
    let entry = m7.records_file();
    let writer = StateLock::take(m7).unwrap();
    assert!(crate::records_storage::interrupt_after_exchange(&entry).is_err());
    assert!(
        m7.records_data_file().is_err(),
        "the layout names no data file"
    );
    let mut ringing = Ringing::new(m7, false);
    ringing.pace = NodePassPace {
        lock_wait: Duration::ZERO,
        ..FAR
    };
    let start = ended_well(macs.root.path());
    std::thread::scope(|scope| {
        let _running = Running {
            stop: &ringing.stop,
        };
        scope.spawn(|| {
            ringing.run(&|| ConvergenceInputs::new(Box::new(|_| None)), &start);
        });
        a_pass_ran_after(m7, ringing.clock);
        assert_eq!(
            node_pass_receipt(m7).map(|receipt| receipt.said),
            Some(NodePassSaid::Busy),
            "the vault is taken"
        );
        ringing.quiet(&[OFF_WATCH]);
        assert!(m7.records_data_file().is_err(), "still unfinished");
        drop(writer);
        crate::records_storage::recover_pending(&entry).expect("the writer finishes it");
        assert!(m7.records_data_file().is_ok());
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the pass the finished layout rang finishes",
            || {
                node_pass_receipt(m7)
                    .filter(|receipt| receipt.said == NodePassSaid::Done)
                    .map(|_| ())
            },
        );
    });
}

/// A broker socket no broker answers is dialled again on its own backoff, and only a broker that
/// answers owes a look: a retry that finds none waits again.
/// NEGATIVE CONTROL: a wait that ends at the broker's retry looks at each one.
#[cfg(target_os = "macos")]
#[test]
fn a_dead_brokers_retry_dials_again_without_a_look() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let m7 = &macs.m7;
    let sockets = aterm_tempfile::tempdir().unwrap();
    let mut ringing = Ringing::new(m7, false);
    ringing.broker = sockets.path().join("broker.sock");
    // The socket of a broker that is gone: a dial finds no listener.
    drop(std::os::unix::net::UnixListener::bind(&ringing.broker).unwrap());
    let start = ended_well(macs.root.path());
    std::thread::scope(|scope| {
        let _running = Running {
            stop: &ringing.stop,
        };
        scope.spawn(|| {
            ringing.run(&|| ConvergenceInputs::new(Box::new(|_| None)), &start);
        });
        a_pass_ran_after(m7, ringing.clock);
        let from = ringing.quiet(&[RECORDS_WATCH, OFF_WATCH]);
        let far = Instant::now() + FAR.every / 4;
        // What the loop did, its watch's tells left out: a wait that ran to a retry's time and
        // then waited for a later one, with no look between.
        ringing.until("a retry that finds no broker waits again", from, |seen| {
            let done: Vec<Seen> = seen
                .iter()
                .copied()
                .filter(|seen| !matches!(seen, Seen::Told(_)))
                .collect();
            done.windows(2).any(|pair| {
                matches!(pair, [Seen::Wait(Some(retry)), Seen::Wait(Some(next))]
                    if *retry < far && next > retry)
            })
        });
    });
}

/// A change of network owes a look at once: the wait that hears one looks next, rather than
/// waiting again. It comes while the loop, its watch's first looks told and looked at, waits on a
/// far deadline.
/// NEGATIVE CONTROL: a wait that drops what its changes said waits again.
#[test]
fn a_change_of_network_owes_a_look() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let m7 = &macs.m7;
    let ringing = Ringing::new(m7, false);
    let start = ended_well(macs.root.path());
    std::thread::scope(|scope| {
        let _running = Running {
            stop: &ringing.stop,
        };
        scope.spawn(|| {
            ringing.run(&|| ConvergenceInputs::new(Box::new(|_| None)), &start);
        });
        a_pass_ran_after(m7, ringing.clock);
        ringing.quiet(&[RECORDS_WATCH, OFF_WATCH]);
        let far = Instant::now() + FAR.every / 4;
        ringing.until(
            "the loop waits on a far deadline",
            0,
            |seen| matches!(seen.last(), Some(Seen::Wait(Some(at))) if *at > far),
        );
        let from = ringing.seen().len();
        ringing.moved.ring();
        ringing.until(
            "the change is heard and something follows it",
            from,
            |seen| {
                seen.iter()
                    .position(|seen| *seen == Seen::Moved)
                    .is_some_and(|at| at + 1 < seen.len())
            },
        );
        let seen = ringing.seen()[from..].to_vec();
        let at = seen.iter().position(|seen| *seen == Seen::Moved).unwrap();
        assert_eq!(seen[at + 1], Seen::Look, "{seen:?}");
    });
}

/// A runner's exit owes a look: the work its run left owed starts as it ends, with no deadline
/// come and no ring. It ends once the watch's first looks were told and looked at, and the ring
/// that read the moved input was looked at too.
/// NEGATIVE CONTROL: a wait that leaves the exit out waits out its timer and its recheck.
#[test]
fn a_runners_exit_owes_a_look() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let m7 = &macs.m7;
    let root = macs.root.path();
    let release = root.join("release");
    let sleep = aterm_uds::deadline::stand_in(Path::new("/bin/sleep"));
    // It ends with the test's folder too, should the test end before it releases the runner.
    let running = converge_program(
        root,
        "converge-until-released",
        &format!(
            "until [ -e \"{}\" ] || [ ! -d \"{}\" ]; do {} 0.01; done",
            release.display(),
            root.display(),
            sleep.display()
        ),
    );
    let success = converge_program(root, "converge-success", "exit 0");
    let calls = AtomicUsize::new(0);
    let start = |_| {
        Some(if calls.fetch_add(1, Ordering::AcqRel) == 0 {
            crate::converge::start(
                &running,
                crate::converge::SUBSCRIBER,
                crate::converge::RepositoryWork::Repair,
            )
            .unwrap()
        } else {
            ended_runner(&success)
        })
    };
    let source = Arc::new(AtomicUsize::new(0));
    let ringing = Ringing::new(m7, false);
    std::thread::scope(|scope| {
        let _running = Running {
            stop: &ringing.stop,
        };
        scope.spawn(|| {
            ringing.run(
                &|| {
                    let source = Arc::clone(&source);
                    let mut seen = source.load(Ordering::Acquire);
                    ConvergenceInputs::new(Box::new(move |refresh| {
                        let current = source.load(Ordering::Acquire);
                        (current != seen).then(|| {
                            seen = current;
                            crate::converge::RepositoryRequest::Refresh(
                                crate::converge::RefreshRequest::inputs(
                                    crate::converge::RefreshScope::new([1; 32]),
                                    refresh.version,
                                ),
                            )
                        })
                    }))
                },
                &start,
            );
        });
        aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the first runner", || {
            (calls.load(Ordering::Acquire) == 1).then_some(())
        });
        let from = ringing.quiet(&[RECORDS_WATCH, OFF_WATCH]);
        // An input moves while the runner runs: its work is owed to the runner's end.
        source.store(1, Ordering::Release);
        ringing.rung.ring();
        ringing.until("a look reads the input, then waits", from, |seen| {
            seen.iter()
                .position(|seen| *seen == Seen::Look)
                .is_some_and(|at| seen[at..].iter().any(|seen| matches!(seen, Seen::Wait(_))))
                && matches!(seen.last(), Some(Seen::Wait(_)))
                && ringing.bell_quiet()
        });
        assert_eq!(calls.load(Ordering::Acquire), 1, "one runner at a time");
        std::fs::write(&release, b"").unwrap();
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the owed runner starts as the first one exits",
            || (calls.load(Ordering::Acquire) == 2).then_some(()),
        );
    });
}

/// A start the LAN holds waits on the LAN's own backoff, not on the due's times, which stay past
/// while it holds; a start the LAN lets through ends that backoff, and an exit is heard while its
/// runner runs.
/// NEGATIVE CONTROL: a `next_at` that takes the due's past retry wakes the loop each look.
#[test]
fn a_start_the_lan_holds_waits_on_the_lans_backoff() {
    let root = aterm_tempfile::tempdir().unwrap();
    let failed = converge_program(root.path(), "failed", "exit 1");
    let sleep = aterm_uds::deadline::stand_in(Path::new("/bin/sleep"));
    // It runs until killed, or until the test's folder goes, should the test end before it kills.
    let running = converge_program(
        root.path(),
        "running",
        &format!(
            "while [ -d \"{}\" ]; do {} 0.01; done",
            root.path().display(),
            sleep.display()
        ),
    );
    let calls = std::cell::Cell::new(0);
    let start = |_: crate::converge::RepositoryRequest| {
        calls.set(calls.get() + 1);
        Some(if calls.get() == 1 {
            ended_runner(&failed)
        } else {
            crate::converge::start(
                &running,
                crate::converge::SUBSCRIBER,
                crate::converge::RepositoryWork::Repair,
            )
            .unwrap()
        })
    };
    let mut converge = NodeConverge::default();
    converge.tend_at(0, [None; 3], &|| true, &start);
    converge.tend_at(1, [None; 3], &|| true, &start);
    assert_eq!(calls.get(), 1);
    assert!(
        converge.due.next_at().is_some_and(|at| at <= 6_000),
        "the failure's retry"
    );

    converge.tend_at(6_000, [None; 3], &|| false, &start);
    assert_eq!(converge.lan.remaining(6_000), Some(Duration::from_secs(5)));
    converge.tend_at(9_000, [None; 3], &|| false, &start);
    assert_eq!(
        converge.lan.remaining(9_000),
        Some(Duration::from_secs(2)),
        "a look before the LAN's retry keeps it"
    );
    converge.tend_at(11_000, [None; 3], &|| false, &start);
    assert_eq!(
        converge.lan.remaining(11_000),
        Some(Duration::from_secs(10)),
        "the LAN's retry backs off"
    );
    // On the loop's clock at 11 s, the due's retry is 5 s past, and the LAN's is 10 s ahead.
    converge.clock = Instant::now().checked_sub(Duration::from_millis(11_000));
    let next = converge.next_at().expect("a held start owes a look");
    assert!(
        next >= Instant::now() + Duration::from_secs(9),
        "the LAN's retry, not the due's past one"
    );

    converge.tend_at(11_001, [None; 3], &|| true, &start);
    assert_eq!(calls.get(), 2, "the LAN let the owed start through");
    assert_eq!(converge.lan.remaining(11_001), None, "and its backoff ends");
    assert_eq!(
        converge.next_at(),
        None,
        "a runner whose exit is heard owes no deadline"
    );
    let exit = converge.exit().expect("its exit is heard");
    assert!(
        aterm_uds::ready::any_now(&[exit]).unwrap().is_empty(),
        "it runs"
    );
    converge.runner.as_mut().unwrap().kill().unwrap();
    let exit = converge.exit().unwrap();
    let ready = aterm_uds::ready::any(
        &[exit],
        Instant::now().checked_add(aterm_spec::wait::MUST_HAPPEN),
    );
    assert_eq!(ready.unwrap(), vec![0], "its end is heard");
    // The kernel tells an exit before the child can be reaped: a look in between finds it running.
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "a look reaps the ended runner",
        || {
            converge.tend_at(11_002, [None; 3], &|| true, &start);
            converge.runner.is_none().then_some(())
        },
    );
    assert!(converge.exit().is_none(), "the look lets its exit go");
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
    let mut child = crate::converge::start(
        exe,
        crate::converge::SUBSCRIBER,
        crate::converge::RepositoryWork::Repair,
    )
    .unwrap();
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
        let start = |_: crate::converge::RepositoryRequest| {
            calls.set(calls.get() + 1);
            Some(ended_runner(match calls.get() {
                1 | 4 => &failed,
                2 => &held,
                _ => &success,
            }))
        };
        let mut converge = NodeConverge::default();
        converge.tend_at(0, [None; 3], &|| false, &start);
        assert_eq!(calls.get(), 0, "an owed version still honors the LAN hold");
        converge.tend_at(1, [None; 3], &|| true, &start);
        converge.tend_at(2, [None; 3], &|| true, &start);
        for at in [3, 4_000, 5_001] {
            converge.tend_at(at, [None; 3], &|| true, &start);
        }
        assert_eq!(
            calls.get(),
            1,
            "one failure is consumed once, without a hot loop"
        );
        converge.tend_at(5_002, [None; 3], &|| false, &start);
        assert_eq!(calls.get(), 1, "a due retry cannot bypass the LAN hold");
        converge.tend_at(6_000, [None; 3], &|| true, &start);
        assert_eq!(
            calls.get(),
            2,
            "unchanged records do not strand the failure"
        );
        converge.tend_at(6_001, [None; 3], &|| true, &start);
        assert_eq!(
            calls.get(),
            2,
            "a held pass waits for the shared five-second retry"
        );
        converge.tend_at(11_000, [None; 3], &|| true, &start);
        assert_eq!(calls.get(), 2);
        converge.tend_at(11_001, [None; 3], &|| true, &start);
        assert_eq!(
            calls.get(),
            3,
            "the held pass stays owed at its retry boundary"
        );
        converge.tend_at(11_002, [None; 3], &|| true, &start);
        converge.tend_at(30_000, [None; 3], &|| true, &start);
        assert_eq!(calls.get(), 3, "success ends the retry streak");

        converge
            .due
            .request(crate::converge::RepositoryRequest::Repair);
        converge.tend_at(30_001, [None; 3], &|| true, &start);
        converge.tend_at(30_002, [None; 3], &|| true, &start);
        converge.tend_at(35_001, [None; 3], &|| true, &start);
        assert_eq!(calls.get(), 4);
        converge.tend_at(35_002, [None; 3], &|| true, &start);
        assert_eq!(
            calls.get(),
            5,
            "success reset the next failure to five seconds"
        );
        converge.tend_at(35_003, [None; 3], &|| true, &start);
        converge.tend_at(60_000, [None; 3], &|| true, &start);
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
    let start = |_: crate::converge::RepositoryRequest| {
        calls.set(calls.get() + 1);
        Some(if calls.get() == 1 {
            crate::converge::start(
                &running,
                crate::converge::SUBSCRIBER,
                crate::converge::RepositoryWork::Repair,
            )
            .unwrap()
        } else {
            ended_runner(&success)
        })
    };
    let mut converge = NodeConverge::default();
    converge.tend_at(0, [None; 3], &|| true, &start);
    for at in [1, 2, 3] {
        converge.due.identity_changed();
        converge.tend_at(at, [None; 3], &|| true, &start);
    }
    assert_eq!(calls.get(), 1, "new work cannot start a concurrent runner");
    let child = converge.runner.as_mut().unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
    converge.tend_at(4, [None; 3], &|| true, &start);
    assert_eq!(
        calls.get(),
        2,
        "new work remains owed after the failed runner"
    );
    converge.tend_at(5, [None; 3], &|| true, &start);
    converge.tend_at(60_000, [None; 3], &|| true, &start);
    assert_eq!(
        calls.get(),
        3,
        "a successful local repair preserves the failed refresh's original retry"
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
