// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What one sync pass costs in resolves of this Mac's entries (each one opens, hashes and wipes
//! every counted `Put` and `Delete` of the log): counted by the resolver's own seam, never timed
//! (ATERM-KI-1113-902).
//!
//! NEGATIVE CONTROL (2026-11-14): on the code before this change (each selection check read
//! afresh, `selection_inputs` stamping `state.toml` by its metadata, each collect of peers
//! reading the policy entry) the settled pass of
//! `a_settled_pass_resolves_this_macs_entries_at_most_once` resolved 5 times and opened 24 entry
//! records: three selection checks (the selection due, the verdict's files intact, the network
//! observation's) and two reads of the policy's servers (the records exchange's and the envelope
//! fetch's).

use super::*;
use crate::membership_log::entries::resolves_and_opens;

/// Resolves of this Mac's entries and entry records opened while `f` runs on this thread.
fn counted<T>(f: impl FnOnce() -> T) -> (T, (usize, usize)) {
    let (resolves, opens) = resolves_and_opens();
    let out = f();
    let (after_resolves, after_opens) = resolves_and_opens();
    (out, (after_resolves - resolves, after_opens - opens))
}

fn unchanged(outcome: crate::Result<SyncOutcome>) -> bool {
    matches!(outcome.expect("a pass"), SyncOutcome::Unchanged { .. })
}

/// The subscriber's pass on a settled vault resolves this Mac's entries once (the policy's
/// servers, read once for the pass), the selection checks answering from the selection the pass
/// before kept; so does the pass after the subscriber rewrote `state.toml` for `heard` alone,
/// which it does about once a minute. A pass that takes a new value writes it; the pass after it
/// reads and keeps the new selection and denial roster, and the next is settled again at one.
#[test]
fn a_settled_pass_resolves_this_macs_entries_at_most_once() {
    let _answering = ServersAnswering::set();
    let lab = log_entries_tests::two_macs(&[
        (Audience::Everyone, "shared", b"everyone-v1".as_slice()),
        (Audience::Everyone, "second", b"everyone-v2".as_slice()),
    ]);
    sync(&lab.m7, None).expect("m7 settles");

    let (outcome, (resolves, opens)) = counted(|| sync(&lab.m7, None));
    assert!(unchanged(outcome), "a settled pass changes nothing");
    assert_eq!(
        (resolves, opens),
        (1, 3),
        "a settled pass resolves this Mac's entries once, opening its 3 entries"
    );

    let mut state = load_state(&lab.m7).unwrap();
    state.heard = state.heard.saturating_add(61);
    save_state(&lab.m7, &state).unwrap();
    let (outcome, (resolves, _)) = counted(|| sync(&lab.m7, None));
    assert!(unchanged(outcome), "heard alone changes nothing");
    assert_eq!(
        resolves, 1,
        "a state.toml rewritten for heard alone keeps the selection"
    );

    {
        let admin = admin_open(&lab.m22).unwrap();
        let mut doc = admin.doc.clone();
        doc.set_entry(&Audience::Everyone, "shared", b"everyone-v3")
            .unwrap();
        admin_publish(&lab.m22, &admin, &doc).expect("m22 publishes a new value");
    }
    let (outcome, (resolves, _)) = counted(|| sync(&lab.m7, None));
    assert!(!unchanged(outcome), "the new value is taken");
    assert_eq!(
        crate::client::stored(&lab.m7, "shared").unwrap(),
        b"everyone-v3",
        "and written"
    );
    // The policy's servers, the wrap-gap pass's and the network observation's denial rosters
    // over the new records, the apply's own read, and its selection check under its own take,
    // which is not kept.
    assert!(
        resolves <= 7,
        "a pass that takes a value resolves {resolves} times"
    );

    // The apply's checks ran under its own take and kept nothing: the first settled pass reads
    // and keeps the new selection and the network observation's denial roster, beside the
    // policy's servers.
    let (outcome, (resolves, _)) = counted(|| sync(&lab.m7, None));
    assert!(unchanged(outcome), "settled again");
    assert_eq!(resolves, 3, "the first pass after it keeps what it read");
    let (outcome, (resolves, _)) = counted(|| sync(&lab.m7, None));
    assert!(unchanged(outcome), "settled");
    assert_eq!(resolves, 1, "and the next resolves once");
    lab.server.stop();
}

/// Every place an apply falls back to the cache's own log progress asks it once
/// ([`peer_selection::LogProgress`]): a second ask is the first's answer, its network body let
/// go alike, with no second verification or resolve.
///
/// NEGATIVE CONTROL (2026-11-14): with `LogProgress::of` calling `cached_log_progress` on each
/// ask, the second ask counts a second (`left: 2, right: 1`).
#[test]
fn an_apply_asks_the_cached_log_progress_once() {
    let _answering = ServersAnswering::set();
    let lab =
        log_entries_tests::two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    sync(&lab.m7, None).expect("m7 settles");
    let config = configured(&lab.m7).unwrap();
    let mut state = load_state(&lab.m7).unwrap();
    // A held selection that is not the current one: the progress is taken from the cache.
    state.materialization_selection =
        Some(materialization_selection::Meaning::from_digest([7; 32]));
    let asks = || peer_selection::LOG_PROGRESS_ASKS.with(std::cell::Cell::get);
    let before = asks();
    let progress = peer_selection::LogProgress::default();
    let first = progress.of(&lab.m7, &config, &state, &mut None);
    let second = progress.of(&lab.m7, &config, &state, &mut None);
    assert!(first.is_some(), "the cache's own log progress is taken");
    assert_eq!(first, second, "the second ask is the first's answer");
    assert_eq!(asks() - before, 1, "asked once");
    lab.server.stop();
}

/// The policy servers in this thread's pass, as many reads as it keeps.
fn kept_policy_reads() -> usize {
    PASS_POLICIES.with(|held| held.borrow().as_ref().map_or(0, Vec::len))
}

/// A read of the policy's servers that meets a writer holding the state answers none and keeps
/// nothing, so the pass's next read asks again; a read that saw no writer is kept.
///
/// NEGATIVE CONTROL (2026-11-14): with `policy_peer_urls` keeping every read whose stamps did not
/// move, the busy read is kept (`left: 1, right: 0`).
#[test]
fn a_policy_read_under_a_writer_is_not_kept_for_the_pass() {
    let _answering = ServersAnswering::set();
    let lab =
        log_entries_tests::two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    sync(&lab.m7, None).expect("m7 settles");
    let config = configured(&lab.m7).unwrap();
    let _pass = PassScope::enter();
    {
        let _writer = StateLock::take(&lab.m7).expect("a writer holds the state");
        assert!(
            policy_peer_urls(&lab.m7, &config).is_empty(),
            "a busy state answers no servers"
        );
        assert_eq!(kept_policy_reads(), 0, "and that answer is not kept");
    }
    let settled = policy_peer_urls(&lab.m7, &config);
    assert_eq!(kept_policy_reads(), 1, "a read with no writer is kept");
    let (again, (resolves, _)) = counted(|| policy_peer_urls(&lab.m7, &config));
    assert_eq!(again, settled, "the kept read answers");
    assert_eq!(resolves, 0, "without another resolve");
    lab.server.stop();
}

/// A state with no applied selection is not current, answered without resolving this Mac's
/// entries.
///
/// NEGATIVE CONTROL (2026-11-14): with `materialization_selection_current` resolving whatever the
/// state holds, the check counts one resolve (`left: 1, right: 0`).
#[test]
fn no_applied_selection_is_not_current_without_a_resolve() {
    let _answering = ServersAnswering::set();
    let lab =
        log_entries_tests::two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    sync(&lab.m7, None).expect("m7 settles");
    let mut state = load_state(&lab.m7).unwrap();
    state.materialization_selection = None;
    let (current, (resolves, _)) = counted(|| materialization_selection_current(&lab.m7, &state));
    assert!(!current, "no applied selection is not current");
    assert_eq!(resolves, 0, "and costs no resolve");
    lab.server.stop();
}
