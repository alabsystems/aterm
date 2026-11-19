// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{age::Identity, commons::binding::Request};

const TRUST: Trust = Trust {
    network: [7; 16],
    authority: [11; 32],
    genesis: [13; 32],
};
const NOW: u64 = 400 * DAY_MS;
/// The floor a member that reports [`RELAYS`] clears: the release the relay tests below sign,
/// so they judge the relay and a report's age, not where a deployed fence such as
/// [`CHECKPOINTS`] stands.
const RELAYED: Floor = in_place(Some(RELAYS));

fn member(n: u8) -> (Binding, [u8; 32]) {
    enrolled(n, TRUST.network, TRUST.authority)
}

/// Member `n`'s device and writer key as enrolled under `network` and `authority`: the same
/// actor under each.
fn enrolled(n: u8, network: [u8; 16], authority: [u8; 32]) -> (Binding, [u8; 32]) {
    let seed = [n + 31; 32];
    let request = Request::create(
        network,
        authority,
        [n + 41; 16],
        [n + 47; 16],
        &Identity::from_secret_bytes([n + 19; 32]),
        &seed,
    )
    .unwrap();
    (request.binding().clone(), seed)
}

#[test]
fn a_version_report_verifies_only_as_its_signer_wrote_it() {
    let (binding, seed) = member(1);
    let actor = binding.actor();
    let wire = report(&seed, TRUST.network, &actor, LIFTED).unwrap();
    assert_eq!(wire.len(), REPORT_LEN);
    assert_eq!(reported(&wire, TRUST.network, &actor).unwrap(), LIFTED);
    // Negative controls: another actor, another network, a raised version, a key that is not
    // the actor's own.
    let (other, other_seed) = member(2);
    assert!(reported(&wire, TRUST.network, &other.actor()).is_err());
    assert!(reported(&wire, [8; 16], &actor).is_err());
    let mut raised = wire.clone();
    raised[8] += 1;
    assert!(reported(&raised, TRUST.network, &actor).is_err());
    assert!(report(&other_seed, TRUST.network, &actor, LIFTED).is_err());
    assert!(reported(&wire[..REPORT_LEN - 1], TRUST.network, &actor).is_err());
}

/// An actor whose writer key is of small order reports no version: the identity's forgery, which
/// ring alone takes for every message, is no report of [`LIFTED`].
///
/// NEGATIVE CONTROL: `ed25519::verifies` through ring's `UnparsedPublicKey`: the forged report
/// reads as `LIFTED`.
#[test]
fn a_small_order_key_reports_no_version() {
    use crate::ed25519::{FORGED, SMALL_ORDER_KEY};
    let (binding, _) = member(1);
    let mut actor = binding.actor();
    actor[..32].copy_from_slice(&SMALL_ORDER_KEY);
    let mut wire = b"ACV1".to_vec();
    wire.extend_from_slice(&LIFTED.bytes());
    wire.extend_from_slice(&FORGED);
    assert_eq!(wire.len(), REPORT_LEN);
    let error = reported(&wire, TRUST.network, &actor)
        .unwrap_err()
        .to_string();
    assert_eq!(error, "Commons version report signature did not verify");
}

#[test]
fn versions_read_their_release_and_order_by_it() {
    assert_eq!(Version::parse("0.146.0+g1a2b3c"), Some(LIFTED));
    assert_eq!(Version::parse("0.145.0"), Some(Version(0, 145, 0)));
    assert!(Version(0, 145, 9) < LIFTED && LIFTED < Version(1, 0, 0));
    for bad in ["0.146", "0.146.0.1", "v0.146.0", ""] {
        assert_eq!(Version::parse(bad), None, "{bad}");
    }
    assert_eq!(LIFTED.to_string(), "0.146.0");
}

/// A process with no installed identity (this test binary) still runs this code, which is
/// LIFTED's: it reports LIFTED, not the unstamped 0.0.0 a peer would hold as older.
#[test]
fn an_unstamped_process_reports_the_release_its_code_is() {
    assert_eq!(aterm_stamp::installed(), None);
    assert_eq!(
        Version::parse(aterm_stamp::app_version()),
        Some(Version(0, 0, 0))
    );
    assert_eq!(Version::own(), LIFTED);
}

#[test]
fn the_ledger_reads_back_and_refuses_another_trust() {
    let (binding, _) = member(1);
    let mut ledger = Ledger::watched(TRUST, NOW - DAY_MS, NOW);
    assert!(ledger.record(binding.actor(), NOW - 5, Said::Version(LIFTED)));
    assert!(!ledger.record(binding.actor(), NOW - 9, Said::Unknown));
    assert_eq!(
        ledger.entry(&binding.actor()),
        Some(&Entry {
            at_ms: NOW - 5,
            version: Some(LIFTED),
            said_ms: NOW - 5,
        })
    );
    let text = ledger.text();
    assert_eq!(
        Ledger::parse(&text, TRUST).map(|read| read.text()),
        Some(text.clone())
    );
    let foreign = Trust {
        genesis: [14; 32],
        ..TRUST
    };
    assert!(Ledger::parse(&text, foreign).is_none());
    assert!(Ledger::parse(&text.replace(MAGIC, "x"), TRUST).is_none());
}

#[test]
fn the_watch_grows_only_while_peers_are_heard_and_restarts_after_a_gap_or_a_clock_that_went_back() {
    let (peer, _) = member(1);
    let mut ledger = Ledger::new(TRUST);
    // Negative control: a device that hears nobody keeps no watch.
    assert!(!ledger.watch(NOW));
    assert_eq!(ledger.watched_since(NOW), None);
    ledger.record(peer.actor(), NOW, Said::Unknown);
    assert!(ledger.watch(NOW));
    ledger.record(peer.actor(), NOW + GAP, Said::Unknown);
    assert!(!ledger.watch(NOW + GAP));
    assert_eq!(ledger.watched_since(NOW + GAP), Some(NOW));
    // Nobody heard for longer than the gap: the watch stops growing and lapses.
    assert!(!ledger.watch(NOW + 2 * GAP + 1));
    assert_eq!(ledger.watched_since(NOW + 2 * GAP + 1), None);
    ledger.record(peer.actor(), NOW + 2 * GAP + 1, Said::Unknown);
    assert!(ledger.watch(NOW + 2 * GAP + 1));
    assert_eq!(
        ledger.watched_since(NOW + 2 * GAP + 1),
        Some(NOW + 2 * GAP + 1)
    );
    ledger.record(peer.actor(), NOW, Said::Unknown);
    assert!(ledger.watch(NOW));
    assert_eq!(ledger.watched_since(NOW), Some(NOW));
    // A watch not kept within the gap is no watch to a reader.
    assert_eq!(ledger.watched_since(NOW + GAP + 1), None);
    // Nor is one read back after a restart, until a peer is heard again.
    let mut read = Ledger::parse(&ledger.text(), TRUST).unwrap();
    assert!(!read.watch(NOW + 1));
    assert_eq!(read.watched_since(NOW + 1), Some(NOW));
}

#[test]
fn the_gate_holds_for_a_recent_older_member_and_opens_past_it() {
    let (old, _) = member(1);
    let (new, _) = member(2);
    let (unheard, _) = member(3);
    let watched = Ledger::watched(TRUST, NOW - RECENT, NOW);
    let mut ledger = watched.clone();
    ledger.record(old.actor(), NOW - DAY_MS, Said::Version(Version(0, 145, 0)));
    ledger.record(new.actor(), NOW - DAY_MS, Said::Version(LIFTED));
    let members = [&old, &new, &unheard];
    // Held by the member heard a day ago on 0.145; the one never heard through a 7-day watch,
    // and the one on 0.146, do not hold it.
    assert_eq!(
        ledger.holding(members, NOW),
        vec![(old.actor(), Hold::Older(Some(Version(0, 145, 0))))]
    );
    let gate = Gate {
        ledger: &ledger,
        now_ms: NOW,
    };
    assert!(Gate::admit(Some(&gate), LEGACY_MAX_FACTS + 1, false, members).is_err());
    // A completed round without a report holds it too.
    let mut silent_report = ledger.clone();
    silent_report.record(old.actor(), NOW, Said::Nothing);
    assert_eq!(
        silent_report.holding(members, NOW),
        vec![(old.actor(), Hold::Older(None))]
    );
    // The gate opens once the older member updates, or once it was last heard 7 days ago.
    let mut updated = ledger.clone();
    updated.record(old.actor(), NOW, Said::Version(LIFTED));
    assert!(updated.holding(members, NOW).is_empty());
    let mut later = Ledger::watched(TRUST, NOW - RECENT, NOW + RECENT - DAY_MS - 1);
    later.record(old.actor(), NOW - DAY_MS, Said::Version(Version(0, 145, 0)));
    later.record(new.actor(), NOW - DAY_MS, Said::Version(LIFTED));
    assert_eq!(later.holding(members, NOW + RECENT - DAY_MS - 1).len(), 1);
    assert!(later.holding(members, NOW + RECENT - DAY_MS).is_empty());
    let gate = Gate {
        ledger: &updated,
        now_ms: NOW,
    };
    assert!(Gate::admit(Some(&gate), LEGACY_MAX_FACTS + 1, false, members).is_ok());
    // A report older than 7 days no longer counts, however recent the contact: the member may
    // have gone back to 0.145 since.
    let mut stale = Ledger::watched(TRUST, NOW - 2 * RECENT, NOW);
    stale.record(new.actor(), NOW - RECENT, Said::Version(LIFTED));
    stale.record(new.actor(), NOW, Said::Unknown);
    assert_eq!(
        stale.holding([&new], NOW),
        vec![(new.actor(), Hold::Older(None))]
    );
    stale.record(new.actor(), NOW, Said::Version(LIFTED));
    assert!(stale.holding([&new], NOW).is_empty());
    // Negative control: a watch shorter than 7 days cannot vouch for the member never heard.
    let mut short = Ledger::watched(TRUST, NOW - RECENT + 1, NOW);
    short.record(old.actor(), NOW, Said::Version(LIFTED));
    short.record(new.actor(), NOW, Said::Version(LIFTED));
    assert_eq!(
        short.holding(members, NOW),
        vec![(unheard.actor(), Hold::Unwatched)]
    );
}

#[test]
fn the_gate_applies_only_to_the_first_node_0_145_refuses() {
    let (old, _) = member(1);
    let mut ledger = Ledger::watched(TRUST, NOW - RECENT, NOW);
    ledger.record(old.actor(), NOW, Said::Nothing);
    let gate = Gate {
        ledger: &ledger,
        now_ms: NOW,
    };
    assert!(Gate::admit(Some(&gate), LEGACY_MAX_FACTS, false, [&old]).is_ok());
    assert!(Gate::admit(None, LEGACY_MAX_FACTS, false, [&old]).is_ok());
    assert!(Gate::admit(Some(&gate), LEGACY_MAX_FACTS + 1, true, [&old]).is_ok());
    // Negative control: an ungated write past the caps is refused even with nobody holding.
    assert!(Gate::admit(None, LEGACY_MAX_FACTS + 1, false, [] as [&Binding; 0]).is_err());
    assert!(Gate::admit(Some(&gate), LEGACY_MAX_FACTS + 1, false, [&old]).is_err());
}

#[test]
fn a_member_is_silent_only_through_a_watch_of_30_days() {
    let (heard, _) = member(1);
    let (gone, _) = member(2);
    let mut ledger = Ledger::watched(TRUST, NOW - SILENT, NOW);
    ledger.record(heard.actor(), NOW - SILENT + 1, Said::Version(LIFTED));
    ledger.record(gone.actor(), NOW - SILENT, Said::Version(LIFTED));
    assert!(ledger.silent(&gone.actor(), NOW));
    assert!(!ledger.silent(&heard.actor(), NOW));
    let (never, _) = member(3);
    assert!(ledger.silent(&never.actor(), NOW));
    // Negative controls: a watch a day short, or one not kept within the gap, names nobody.
    let short = Ledger::watched(TRUST, NOW - SILENT + DAY_MS, NOW);
    assert!(!short.silent(&never.actor(), NOW));
    let stale = Ledger::watched(TRUST, NOW - 2 * SILENT, NOW - GAP - 1);
    assert!(!stale.silent(&never.actor(), NOW));
}

#[test]
fn a_member_first_seen_on_the_roster_is_timed_from_then() {
    let (joined, _) = member(1);
    let (heard, _) = member(2);
    let mut ledger = Ledger::watched(TRUST, NOW - 2 * SILENT, NOW);
    ledger.record(heard.actor(), NOW - SILENT, Said::Version(LIFTED));
    // Without a note, a member never heard through a 30-day watch reads as silent.
    assert!(ledger.silent(&joined.actor(), NOW));
    assert!(ledger.note(joined.actor(), NOW - SILENT + 1));
    // Noting never moves a member already heard, or one noted before.
    assert!(!ledger.note(heard.actor(), NOW));
    assert!(!ledger.note(joined.actor(), NOW));
    assert!(ledger.silent(&heard.actor(), NOW));
    assert!(!ledger.silent(&joined.actor(), NOW));
    assert!(ledger.silent(&joined.actor(), NOW + 1));
    // A member noted within 7 days holds the gate as one heard without a report.
    let mut fresh = Ledger::watched(TRUST, NOW - RECENT, NOW);
    fresh.note(joined.actor(), NOW - 1);
    assert_eq!(
        fresh.holding([&joined], NOW),
        vec![(joined.actor(), Hold::Older(None))]
    );
    // A note is not hearing anyone: it keeps no watch and names no admin to defer to.
    let mut unwatched = Ledger::new(TRUST);
    unwatched.note(joined.actor(), NOW);
    assert!(!unwatched.watch(NOW));
    assert!(!unwatched.lifted_within(&joined.actor(), NOW, GAP));
    unwatched.record(joined.actor(), NOW, Said::Nothing);
    assert!(!unwatched.lifted_within(&joined.actor(), NOW, GAP));
    unwatched.record(joined.actor(), NOW, Said::Version(LIFTED));
    assert!(unwatched.lifted_within(&joined.actor(), NOW, GAP));
    assert!(!unwatched.lifted_within(&joined.actor(), NOW + GAP, GAP));
}

#[test]
fn a_refresh_is_left_only_to_a_smaller_admin_that_reported_0_146_lately() {
    let mut pair = [member(1).0.actor(), member(2).0.actor()];
    pair.sort();
    let [smaller, local] = pair;
    let mut ledger = Ledger::new(TRUST);
    ledger.record(smaller, NOW - 1, Said::Version(LIFTED));
    assert!(ledger.defers(&local, [smaller], NOW));
    // Negative controls: the smaller admin never runs a refresh this one could leave to it
    // when it is not an admin here, is the larger, reported too long ago, or runs 0.145.
    assert!(!ledger.defers(&local, [], NOW));
    assert!(!ledger.defers(&smaller, [local], NOW));
    assert!(!ledger.defers(&local, [smaller], NOW - 1 + DEFER));
    let mut older = Ledger::new(TRUST);
    older.record(smaller, NOW - 1, Said::Version(Version(0, 145, 0)));
    assert!(!older.defers(&local, [smaller], NOW));
    older.record(smaller, NOW, Said::Nothing);
    assert!(!older.defers(&local, [smaller], NOW));
    let mut noted = Ledger::new(TRUST);
    noted.note(smaller, NOW);
    assert!(!noted.defers(&local, [smaller], NOW));
}

#[test]
fn returns_stop_at_63_members_while_the_gate_holds() {
    let (old, _) = member(1);
    let (new, _) = member(2);
    let mut ledger = Ledger::watched(TRUST, NOW - RECENT, NOW);
    ledger.record(old.actor(), NOW, Said::Version(Version(0, 145, 0)));
    ledger.record(new.actor(), NOW, Said::Version(LIFTED));
    assert_eq!(ledger.room(false, [&old, &new], NOW), LEGACY_MAX_MEMBERS);
    // The history past the caps, or no reader holding, lets the roster fill.
    assert_eq!(ledger.room(true, [&old, &new], NOW), MAX_MEMBERS);
    assert_eq!(ledger.room(false, [&new], NOW), MAX_MEMBERS);
    ledger.record(old.actor(), NOW, Said::Version(LIFTED));
    assert_eq!(ledger.room(false, [&old, &new], NOW), MAX_MEMBERS);
}

/// The registry's gate ([`in_place`]) counts only what it heard: a member on an older release,
/// one whose last round said nothing and one unheard through a watch of any length each hold it,
/// and so does this device where its own build is older. With no release named every member
/// holds it. It opens only once every member reported the release or later within 7 days.
/// CONTROL: the same ledger passes the caps' gate, which lets a member unheard through a 7-day
/// watch pass.
/// NEGATIVE CONTROL: with `holding_below` skipping its `unheard_passes` arm (an unheard member
/// judged by the watch alone), the member never heard no longer holds: `` left: [(…, Older(Some(
/// Version(0, 146, 0))))] right: [(…, Older(Some(Version(0, 146, 0)))), (…, Unheard)] ``.
#[test]
fn the_in_place_gate_counts_only_members_it_heard_and_none_while_no_release_is_named() {
    let (old, _) = member(1);
    let (new, _) = member(2);
    let (unheard, _) = member(3);
    let release = Version(0, 150, 0);
    let mut ledger = Ledger::watched(TRUST, NOW - 2 * RECENT, NOW);
    ledger.record(old.actor(), NOW - DAY_MS, Said::Version(LIFTED));
    ledger.record(new.actor(), NOW - DAY_MS, Said::Version(release));
    let members = [&old, &new, &unheard];
    assert_eq!(
        ledger.holding_below(members, NOW, in_place(Some(release))),
        vec![
            (old.actor(), Hold::Older(Some(LIFTED))),
            (unheard.actor(), Hold::Unheard)
        ]
    );
    assert!(ledger.holding(members, NOW).is_empty(), "control");
    // No release named: the member on the newest release holds it with the rest.
    assert_eq!(
        ledger.holding_below(members, NOW, in_place(None)),
        vec![
            (old.actor(), Hold::Older(Some(LIFTED))),
            (new.actor(), Hold::Older(Some(release))),
            (unheard.actor(), Hold::Unheard)
        ]
    );
    // Every member reported the release or later: the gate opens.
    ledger.record(old.actor(), NOW, Said::Version(release));
    ledger.record(unheard.actor(), NOW, Said::Version(Version(0, 151, 0)));
    assert!(
        ledger
            .holding_below(members, NOW, in_place(Some(release)))
            .is_empty()
    );
    // A later round that said nothing holds it again, and so does a report 7 days old.
    let mut quiet = ledger.clone();
    quiet.record(new.actor(), NOW, Said::Nothing);
    assert_eq!(
        quiet.holding_below(members, NOW, in_place(Some(release))),
        vec![(new.actor(), Hold::Older(None))]
    );
    assert_eq!(
        ledger
            .holding_below(members, NOW + RECENT, in_place(Some(release)))
            .len(),
        3
    );
    // This device is judged by its own running build, which a test binary reports as LIFTED.
    let mut own = ledger.clone();
    own.know_local(&old);
    assert_eq!(
        own.holding_below(members, NOW, in_place(Some(release))),
        vec![(old.actor(), Hold::Older(Some(Version::own())))]
    );
    assert!(
        own.holding_below([&old], NOW, in_place(Some(LIFTED)))
            .is_empty()
    );
    assert_eq!(
        own.holding_below([&old], NOW, in_place(None)),
        vec![(old.actor(), Hold::Older(Some(Version::own())))]
    );
}

/// One device writes no vault entry to a file only where it reported [`NO_SECRET_FILE`] or later
/// within 7 days ([`Ledger::writes_no_secret_file`]); every other device, and this one on a test
/// binary's own build, fails closed.
/// NEGATIVE CONTROL: with `writes_no_secret_file` judging by [`CAPS`] (an unheard member passes
/// a 7-day watch), the device never heard passes: `assertion failed: !ledger
/// .writes_no_secret_file(&unheard, NOW)`.
#[test]
fn a_device_writes_no_secret_file_only_on_a_recent_report_of_the_release_that_stopped() {
    let release = NO_SECRET_FILE.expect("a release is named");
    let (new, _) = member(1);
    let (old, _) = member(2);
    let (silent, _) = member(3);
    let (unheard, _) = member(4);
    let (stale, _) = member(5);
    let mut ledger = Ledger::watched(TRUST, NOW - 2 * RECENT, NOW);
    ledger.record(new.actor(), NOW - DAY_MS, Said::Version(release));
    ledger.record(old.actor(), NOW - DAY_MS, Said::Version(Version(0, 202, 0)));
    ledger.record(silent.actor(), NOW - DAY_MS, Said::Nothing);
    ledger.record(stale.actor(), NOW - RECENT - DAY_MS, Said::Version(release));
    // Heard since, by a round that ended before reports were traded: the report stays 8 days old.
    ledger.record(stale.actor(), NOW, Said::Unknown);
    assert!(ledger.writes_no_secret_file(&new, NOW));
    assert!(!ledger.writes_no_secret_file(&old, NOW));
    assert!(!ledger.writes_no_secret_file(&silent, NOW));
    assert!(!ledger.writes_no_secret_file(&unheard, NOW));
    assert!(!ledger.writes_no_secret_file(&stale, NOW));
    // The report stops counting 7 days after it was made.
    assert!(!ledger.writes_no_secret_file(&new, NOW - DAY_MS + RECENT));
    // This device is judged by its own running build, older than the release in a test binary.
    let mut own = ledger.clone();
    own.know_local(&new);
    assert!(Version::own() < release);
    assert!(!own.writes_no_secret_file(&new, NOW));
}

/// The format fence's floor ([`FENCE`]) holds for every member heard within 7 days while no
/// release reads format 2 ([`READS_FORMAT`] is `None`): a Mac on the release that shipped the
/// fence, or any later one, cannot follow a format-2 node, so the first format-2 write would cut
/// it off. A member unheard for 7 days through a watch that long passes, as [`CAPS`] does: with
/// most seats silent, a floor that waited on each would never open, and a Mac that returns below
/// it is fenced by its bound, not misled. Once a release is named, a member reporting it passes
/// and one before it holds.
/// NEGATIVE CONTROL: with [`READS_FORMAT`] the fence's own release, 0.205.0 (as first built),
/// members reporting 0.205.0 and 9.0.0 passed [`FENCE`]: only the silent one held it
/// (`left: [(.., Older(None))]`, `right` the three holds below).
/// In the test: the same release under a floor that counts only members it heard ([`in_place`])
/// holds for the unheard member, and before the watch is 7 days long the unheard member holds
/// [`FENCE`] too.
#[test]
fn vault_fence_the_floor_lets_an_unheard_seat_pass() {
    let (fenced, _) = member(1);
    let (later, _) = member(2);
    let (unheard, _) = member(3);
    let (silent, _) = member(4);
    assert!(FENCE.unheard_passes);
    assert_eq!(FENCE.version, READS_FORMAT);
    let shipped_fence = Version(0, 205, 0);
    let mut ledger = Ledger::watched(TRUST, NOW - 2 * RECENT, NOW);
    ledger.record(fenced.actor(), NOW - DAY_MS, Said::Version(shipped_fence));
    ledger.record(later.actor(), NOW - DAY_MS, Said::Version(Version(9, 0, 0)));
    ledger.record(silent.actor(), NOW - DAY_MS, Said::Nothing);
    let members = [&fenced, &later, &unheard, &silent];
    assert_eq!(
        ledger.holding_below(members, NOW, FENCE),
        vec![
            (fenced.actor(), Hold::Older(Some(shipped_fence))),
            (later.actor(), Hold::Older(Some(Version(9, 0, 0)))),
            (silent.actor(), Hold::Older(None)),
        ]
    );
    // Once a release reads format 2 (FL-148 names it), the floor is FENCE's shape at it.
    let reads = Version(0, 210, 0);
    let named = Floor {
        version: Some(reads),
        ..FENCE
    };
    ledger.record(later.actor(), NOW - DAY_MS, Said::Version(reads));
    assert_eq!(
        ledger.holding_below(members, NOW, named),
        vec![
            (fenced.actor(), Hold::Older(Some(shipped_fence))),
            (silent.actor(), Hold::Older(None)),
        ]
    );
    assert_eq!(
        ledger.holding_below(members, NOW, in_place(Some(reads))),
        vec![
            (fenced.actor(), Hold::Older(Some(shipped_fence))),
            (unheard.actor(), Hold::Unheard),
            (silent.actor(), Hold::Older(None)),
        ],
        "control"
    );
    let young = Ledger::watched(TRUST, NOW - DAY_MS, NOW);
    assert_eq!(
        young.holding_below([&unheard], NOW, FENCE),
        vec![(unheard.actor(), Hold::Unwatched)],
        "control"
    );
    ledger.record(fenced.actor(), NOW, Said::Version(Version(0, 210, 1)));
    ledger.record(silent.actor(), NOW, Said::Version(reads));
    assert!(ledger.holding_below(members, NOW, named).is_empty());
}

/// A store on `local`'s device whose roster is `members`.
fn store(local: &Binding, members: &[&Binding]) -> Reports {
    let mut reports = Reports::new(TRUST, local);
    reports.set_roster(members.iter().copied(), NOW);
    reports
}

/// Take `wire` as a device takes a report a peer handed over: screened under the store, its
/// signature verified outside it, then accepted.
fn take(reports: &mut Reports, wire: &[u8], now_ms: u64) -> bool {
    reports
        .screen(wire, now_ms)
        .and_then(|member| relayed(wire, &member, now_ms).ok())
        .is_some_and(|report| reports.accept(report, now_ms))
}

/// The reports a store keeps, in the order it took them.
fn kept(reports: &Reports) -> Vec<[u8; REPORT2_LEN]> {
    let (kept, _) = reports.since(0);
    kept.iter().map(|report| *report.wire()).collect()
}

/// One side of a relay round: its store, its actor, and its mark for the other side.
struct Side<'a> {
    reports: &'a mut Reports,
    actor: Actor,
    mark: &'a mut Mark,
}

/// A relay round between two stores, each flight encoded and read back as two peers trade them
/// after a repair round. The third flight goes only where the second wanted or offered
/// something, the fourth only where the third wanted something; a side moves its mark once its
/// offer was answered and what the answer wanted was sent. The bytes each flight put on the
/// wire, with its 4-byte frame length.
fn round(initiator: Side<'_>, responder: Side<'_>, now_ms: u64) -> Vec<usize> {
    let count = |want: &[bool]| want.iter().filter(|want| **want).count();
    let offer1 = initiator
        .reports
        .offer(*initiator.mark, &responder.actor, &[], now_ms);
    let flight1 = offer_flight(offer1.entries());
    let theirs1 = read_offer(&flight1).unwrap();
    let want1 = responder.reports.want(&theirs1, now_ms);
    let offer2 = responder
        .reports
        .offer(*responder.mark, &initiator.actor, &theirs1, now_ms);
    let flight2 = answer_flight(&want1, offer2.entries());
    let (wanted1, theirs2) = read_answer(&flight2, offer1.entries().len()).unwrap();
    let mut sent = vec![4 + flight1.len(), 4 + flight2.len()];
    if count(&wanted1) == 0 && theirs2.is_empty() {
        *initiator.mark = offer1.covers();
        *responder.mark = offer2.covers();
        return sent;
    }
    let want2 = initiator.reports.want(&theirs2, now_ms);
    let flight3 = give_flight(&want2, &initiator.reports.give(&offer1, &wanted1));
    *initiator.mark = offer1.covers();
    sent.push(4 + flight3.len());
    let (wanted2, given1) = read_give(&flight3, offer2.entries().len(), count(&want1)).unwrap();
    for wire in taken(&theirs1, &want1, &given1) {
        take(responder.reports, wire, now_ms);
    }
    if count(&wanted2) > 0 {
        let flight4 = last_flight(&responder.reports.give(&offer2, &wanted2));
        sent.push(4 + flight4.len());
        let given2 = read_last(&flight4, count(&want2)).unwrap();
        for wire in taken(&theirs2, &want2, &given2) {
            take(initiator.reports, wire, now_ms);
        }
    }
    *responder.mark = offer2.covers();
    sent
}

/// Feed a store's kept reports to `ledger`, as the hearing tick does. Whether any changed it.
fn hear(ledger: &mut Ledger, reports: &Reports, now_ms: u64) -> bool {
    let (kept, _) = reports.since(0);
    kept.iter().fold(false, |changed, report| {
        ledger.relay(report, now_ms) | changed
    })
}

/// LT-04C's exit test. Member A signs its version with the time; B, which met A, keeps the
/// report; C, which never met A, gets it from B over the four flights and its ledger counts A
/// for the checkpoint gate, which held A as unheard before. That is no contact with A: C's watch
/// does not start. The same path with one signature byte changed leaves A unheard, whether B
/// checks the report (it keeps none) or hands the changed bytes to C as wanted (C refuses them
/// after its own check).
///
/// NEGATIVE CONTROL: with [`relayed`] returning the report without checking its signature, the
/// changed report is taken: `assertion failed: !take(&mut honest, &changed, NOW)`.
#[test]
fn a_report_relayed_by_another_member_counts_once_its_signature_checks() {
    let (a, seed_a) = member(1);
    let (b, _) = member(2);
    let (c, _) = member(3);
    let roster = [&a, &b, &c];
    let signed_ms = NOW - 5 * 60 * 1000;
    let wire = report_at(&seed_a, TRUST.network, &a.actor(), RELAYS, signed_ms).unwrap();
    let unheard = vec![(a.actor(), Hold::Unheard)];

    let mut at_b = store(&b, &roster);
    let mut at_c = store(&c, &roster);
    let mut ledger = Ledger::new(TRUST);
    ledger.know_local(&c);
    assert_eq!(ledger.holding_below([&a], NOW, RELAYED), unheard);
    assert!(take(&mut at_b, &wire, NOW));
    let (mut b_mark, mut c_mark) = (Mark::default(), Mark::default());
    let sent = round(
        Side {
            reports: &mut at_b,
            actor: b.actor(),
            mark: &mut b_mark,
        },
        Side {
            reports: &mut at_c,
            actor: c.actor(),
            mark: &mut c_mark,
        },
        NOW,
    );
    // One entry offered, one want bit and no entries back, then the one report.
    assert_eq!(sent, [4 + 6 + 24, 4 + 4 + 1 + 2, 4 + 4 + 2 + 136]);
    assert_eq!(kept(&at_c), [wire]);
    assert!(hear(&mut ledger, &at_c, NOW));
    assert!(ledger.holding_below([&a], NOW, RELAYED).is_empty());
    assert_eq!(
        ledger.entry(&a.actor()),
        Some(&Entry {
            at_ms: signed_ms,
            version: Some(RELAYS),
            said_ms: signed_ms,
        })
    );
    assert!(!ledger.watch(NOW));
    assert_eq!(ledger.watched_since(NOW), None);
    // Both sides are done with each other: the next round is two empty flights.
    let again = round(
        Side {
            reports: &mut at_c,
            actor: c.actor(),
            mark: &mut c_mark,
        },
        Side {
            reports: &mut at_b,
            actor: b.actor(),
            mark: &mut b_mark,
        },
        NOW,
    );
    assert_eq!(again, [4 + 4 + 2 + 24, 4 + 4 + 1 + 2]);
    let idle = round(
        Side {
            reports: &mut at_b,
            actor: b.actor(),
            mark: &mut b_mark,
        },
        Side {
            reports: &mut at_c,
            actor: c.actor(),
            mark: &mut c_mark,
        },
        NOW,
    );
    assert_eq!(idle.iter().sum::<usize>(), 20);

    // One signature byte changed: B keeps nothing, so C is offered nothing.
    let mut changed = wire;
    changed[REPORT2_LEN - 1] ^= 1;
    let mut honest = store(&b, &roster);
    let mut reader = store(&c, &roster);
    assert!(!take(&mut honest, &changed, NOW));
    round(
        Side {
            reports: &mut honest,
            actor: b.actor(),
            mark: &mut Mark::default(),
        },
        Side {
            reports: &mut reader,
            actor: c.actor(),
            mark: &mut Mark::default(),
        },
        NOW,
    );
    assert!(kept(&reader).is_empty());
    // A peer that hands C the changed bytes for an entry C wanted: the cheap rules pass, the
    // signature does not.
    let offered = [(tag(&a.actor()), signed_ms)];
    let want = reader.want(&offered, NOW);
    assert_eq!(want, [true]);
    let (_, given) = read_give(&give_flight(&[], &[changed]), 0, 1).unwrap();
    let wires = taken(&offered, &want, &given);
    assert_eq!(wires, [&changed]);
    assert!(reader.screen(wires[0], NOW).is_some());
    assert!(!take(&mut reader, wires[0], NOW));
    assert!(kept(&reader).is_empty());
    let mut ledger = Ledger::new(TRUST);
    ledger.know_local(&c);
    assert!(!hear(&mut ledger, &reader, NOW));
    assert_eq!(ledger.holding_below([&a], NOW, RELAYED), unheard);
}

/// Rule 1: a time-signed report is 136 bytes that verify only as its signer wrote them and only
/// for the network its member is bound to. Any one changed bit, a byte more or fewer, the same
/// member's binding under another network, and a report signed for another network each count
/// for nothing; a key that is not the actor's signs none.
///
/// NEGATIVE CONTROL: with `signed_at` leaving the network out of the signed message, the report
/// verifies under the same member's binding to another network: `assertion failed:
/// relayed(&wire, &elsewhere, NOW).is_err()`.
#[test]
fn a_relayed_report_with_a_changed_byte_or_another_network_counts_for_nothing() {
    let (a, seed) = member(1);
    let actor = a.actor();
    let wire = report_at(&seed, TRUST.network, &actor, RELAYS, NOW).unwrap();
    assert_eq!(wire.len(), 136);
    let read = relayed(&wire, &a, NOW).unwrap();
    assert_eq!(
        (read.actor(), read.version(), read.signed_ms(), read.wire()),
        (actor, RELAYS, NOW, &wire)
    );
    for at in 0..REPORT2_LEN {
        for bit in [1, 0x80] {
            let mut changed = wire;
            changed[at] ^= bit;
            assert!(relayed(&changed, &a, NOW).is_err(), "byte {at} bit {bit}");
        }
    }
    assert!(relayed(&wire[..REPORT2_LEN - 1], &a, NOW).is_err());
    assert!(relayed(&[wire.as_slice(), &[0]].concat(), &a, NOW).is_err());
    assert!(report_at(&member(2).1, TRUST.network, &actor, RELAYS, NOW).is_err());

    let other = [8; 16];
    let (elsewhere, _) = enrolled(1, other, TRUST.authority);
    assert_eq!(elsewhere.actor(), actor);
    assert!(relayed(&wire, &elsewhere, NOW).is_err());
    let foreign = report_at(&seed, other, &actor, RELAYS, NOW).unwrap();
    assert!(relayed(&foreign, &a, NOW).is_err());
    // It is that network's report, and this trust's ledger and store take none of it.
    let report = relayed(&foreign, &elsewhere, NOW).unwrap();
    let mut ledger = Ledger::new(TRUST);
    assert!(!ledger.relay(&report, NOW));
    assert_eq!(ledger.entry(&actor), None);
    let (b, _) = member(2);
    let mut reports = store(&b, &[&a, &b]);
    assert!(!reports.accept(report, NOW));
    assert!(kept(&reports).is_empty());
}

/// Rule 2: a relayed report counts only under the roster binding of the member that signed it.
/// A validly signed report of an actor off the roster is neither wanted, screened nor kept; A's
/// report does not verify under B's binding; one verified under the same actor's binding to
/// another authority is refused by the store and the ledger; and the local writer's own report
/// is never taken from a peer.
///
/// NEGATIVE CONTROL: with [`relayed`] not comparing the report's actor with the member's, A's
/// report verifies under B's binding: `assertion failed: relayed(&wire_a, &b, NOW).is_err()`.
#[test]
fn a_relayed_report_counts_only_under_its_own_members_roster_binding() {
    let (a, seed_a) = member(1);
    let (b, seed_b) = member(2);
    let (c, _) = member(3);
    let (outsider, seed_out) = member(4);
    let sign = |seed: &[u8; 32], member: &Binding| {
        report_at(seed, TRUST.network, &member.actor(), RELAYS, NOW).unwrap()
    };
    let (wire_a, wire_b, wire_out) = (
        sign(&seed_a, &a),
        sign(&seed_b, &b),
        sign(&seed_out, &outsider),
    );
    let mut reports = store(&b, &[&a, &b, &c]);
    let mut ledger = Ledger::new(TRUST);
    ledger.know_local(&b);

    // Signed by its own key, for this network, by an actor that is no member here.
    let report = relayed(&wire_out, &outsider, NOW).unwrap();
    assert_eq!(reports.want(&[(tag(&outsider.actor()), NOW)], NOW), [false]);
    assert!(reports.screen(&wire_out, NOW).is_none());
    assert!(!reports.accept(report, NOW));

    assert!(relayed(&wire_a, &b, NOW).is_err());

    // The signature binds the network, not the authority: the binding's authority must be the
    // trust's.
    let (other_authority, _) = enrolled(1, TRUST.network, [12; 32]);
    let report = relayed(&wire_a, &other_authority, NOW).unwrap();
    assert!(!reports.accept(report.clone(), NOW));
    assert!(!ledger.relay(&report, NOW));
    assert_eq!(ledger.entry(&a.actor()), None);
    let mut strangers = store(&b, &[&other_authority, &b]);
    assert!(strangers.screen(&wire_a, NOW).is_none());
    assert!(!take(&mut strangers, &wire_a, NOW));

    // The local writer's version is this running build's, never a peer's word.
    assert_eq!(reports.want(&[(tag(&b.actor()), NOW)], NOW), [false]);
    assert!(reports.screen(&wire_b, NOW).is_none());
    let own = relayed(&wire_b, &b, NOW).unwrap();
    assert!(!reports.accept(own.clone(), NOW));
    assert!(!ledger.relay(&own, NOW));
    assert_eq!(ledger.entry(&b.actor()), None);
    assert!(kept(&reports).is_empty());

    // Control: A's report under A's roster binding is kept and counted.
    assert!(take(&mut reports, &wire_a, NOW));
    assert!(hear(&mut ledger, &reports, NOW));
    assert_eq!(ledger.entry(&a.actor()).unwrap().version, Some(RELAYS));
}

/// Rule 3: a report signed seven days ago counts for nothing: it does not verify as a relayed
/// report, a ledger handed one verified a moment earlier refuses it, and a store neither wants
/// nor screens it. One that counted stops then: the gate holds its member as unheard seven days
/// after it was signed, and the store offers it to nobody and drops it at its next roster.
///
/// NEGATIVE CONTROL: with `timely` not bounding the signed time below, the report signed seven
/// days ago verifies: `assertion failed: relayed(&old, &a, NOW).is_err()`.
#[test]
fn a_relayed_report_signed_seven_days_ago_counts_for_nothing_and_a_kept_one_stops_counting_then() {
    let (a, seed) = member(1);
    let (b, _) = member(2);
    let (c, _) = member(3);
    let sign = |signed_ms| report_at(&seed, TRUST.network, &a.actor(), RELAYS, signed_ms).unwrap();
    let old = sign(NOW - RECENT);
    assert!(relayed(&old, &a, NOW).is_err());
    let report = relayed(&old, &a, NOW - 1).unwrap();
    let mut ledger = Ledger::new(TRUST);
    assert!(!ledger.relay(&report, NOW));
    assert_eq!(ledger.entry(&a.actor()), None);
    let mut reports = store(&b, &[&a, &b, &c]);
    let offered = [(tag(&a.actor()), NOW - RECENT)];
    assert_eq!(reports.want(&offered, NOW), [false]);
    assert_eq!(reports.want(&offered, NOW - 1), [true]);
    assert!(reports.screen(&old, NOW).is_none());
    assert!(!reports.accept(report, NOW));

    let signed_ms = NOW - DAY_MS;
    let fresh = sign(signed_ms);
    let then = signed_ms + RECENT;
    assert!(take(&mut reports, &fresh, NOW));
    assert!(hear(&mut ledger, &reports, NOW));
    assert!(ledger.holding_below([&a], then - 1, RELAYED).is_empty());
    assert_eq!(
        ledger.holding_below([&a], then, RELAYED),
        vec![(a.actor(), Hold::Unheard)]
    );
    let offer = |reports: &Reports, now_ms| {
        reports
            .offer(Mark::default(), &c.actor(), &[], now_ms)
            .entries()
            .to_vec()
    };
    assert_eq!(offer(&reports, then - 1), [(tag(&a.actor()), signed_ms)]);
    assert!(offer(&reports, then).is_empty());
    assert!(reports.file().is_some());
    assert!(!reports.set_roster([&a, &b, &c], then - 1));
    assert_eq!(kept(&reports), [fresh]);
    assert!(reports.file().is_none());
    assert!(!reports.set_roster([&a, &b, &c], then));
    assert!(kept(&reports).is_empty());
    assert_eq!(reports.file().map(|file| file.len()), Some(90));
}

/// Rule 4: a report signed more than ten minutes past this device's clock is refused by the
/// verifier, the store and the ledger. One signed within that counts from this device's own
/// time, not the signer's: the ledger holds no time ahead of its clock, the report stops
/// counting seven days from now, and a round with the member at the same moment replaces it.
///
/// NEGATIVE CONTROL: with [`Ledger::relay`] storing the signed time and not the lesser of it
/// and now, the entry is ten minutes ahead: `` left: Some(Entry { at_ms: 34560600000, version:
/// Some(Version(0, 207, 0)), said_ms: 34560600000 }) right: Some(Entry { at_ms: 34560000000,
/// version: Some(Version(0, 207, 0)), said_ms: 34560000000 }) ``.
#[test]
fn a_relayed_report_signed_more_than_ten_minutes_ahead_is_refused_and_one_within_that_counts_from_now()
 {
    let (a, seed) = member(1);
    let (b, _) = member(2);
    let sign = |signed_ms| report_at(&seed, TRUST.network, &a.actor(), RELAYS, signed_ms).unwrap();
    let mut reports = store(&b, &[&a, &b]);
    let mut ledger = Ledger::new(TRUST);

    let far = sign(NOW + AHEAD + 1);
    assert!(relayed(&far, &a, NOW).is_err());
    assert!(reports.screen(&far, NOW).is_none());
    assert_eq!(
        reports.want(&[(tag(&a.actor()), NOW + AHEAD + 1)], NOW),
        [false]
    );
    // Verified by a clock one millisecond later, where it is within the bound.
    let report = relayed(&far, &a, NOW + 1).unwrap();
    assert!(!reports.accept(report.clone(), NOW));
    assert!(!ledger.relay(&report, NOW));
    assert_eq!(ledger.entry(&a.actor()), None);

    let near = sign(NOW + AHEAD);
    assert!(take(&mut reports, &near, NOW));
    assert!(hear(&mut ledger, &reports, NOW));
    assert_eq!(
        ledger.entry(&a.actor()),
        Some(&Entry {
            at_ms: NOW,
            version: Some(RELAYS),
            said_ms: NOW,
        })
    );
    assert!(
        ledger
            .holding_below([&a], NOW + RECENT - 1, RELAYED)
            .is_empty()
    );
    assert_eq!(
        ledger.holding_below([&a], NOW + RECENT, RELAYED),
        vec![(a.actor(), Hold::Unheard)]
    );
    assert!(ledger.record(a.actor(), NOW, Said::Version(LIFTED)));
    assert_eq!(ledger.entry(&a.actor()).unwrap().version, Some(LIFTED));
}

/// Rule 5: the version a ledger holds for a member moves only to a report signed later than the
/// one it holds. An older relayed report replaces neither a newer relayed one nor a newer one
/// heard in a round, one signed at the same moment changes nothing, and a round that said
/// nothing is undone only by a report signed more than [`AHEAD`] after it. The time the member
/// was last heard never goes back.
///
/// NEGATIVE CONTROL: with [`Ledger::relay`] taking a report's version whatever its time, the
/// older report replaces the newer: `assertion failed: !ledger.relay(&older, NOW)`.
#[test]
fn an_older_relayed_report_never_replaces_a_newer_one_relayed_or_heard_directly() {
    let (a, seed) = member(1);
    let actor = a.actor();
    let (old, new) = (RELAYS, Version(0, 208, 0));
    let sign = |version, signed_ms| {
        let wire = report_at(&seed, TRUST.network, &actor, version, signed_ms).unwrap();
        relayed(&wire, &a, NOW).unwrap()
    };
    let (earlier, later) = (NOW - 2 * STEP, NOW - STEP);
    let older = sign(old, earlier);
    let newer = sign(new, later);
    let held = Entry {
        at_ms: later,
        version: Some(new),
        said_ms: later,
    };

    let mut ledger = Ledger::new(TRUST);
    assert!(ledger.relay(&newer, NOW));
    assert!(!ledger.relay(&older, NOW));
    assert_eq!(ledger.entry(&actor), Some(&held));

    let mut direct = Ledger::new(TRUST);
    direct.record(actor, later, Said::Version(new));
    assert!(!direct.relay(&older, NOW));
    assert!(!direct.relay(&sign(old, later), NOW));
    assert_eq!(direct.entry(&actor), Some(&held));

    // A report signed after a round replaces what the round heard.
    let mut moved = Ledger::new(TRUST);
    moved.record(actor, earlier, Said::Version(old));
    assert!(moved.relay(&newer, NOW));
    assert_eq!(moved.entry(&actor), Some(&held));

    let mut quiet = Ledger::new(TRUST);
    quiet.record(actor, later, Said::Nothing);
    assert!(!quiet.relay(&older, NOW));
    assert_eq!(quiet.entry(&actor).unwrap().version, None);
    let after = later + AHEAD + 1;
    assert!(quiet.relay(&sign(new, after), NOW));
    assert_eq!(
        quiet.entry(&actor),
        Some(&Entry {
            at_ms: after,
            version: Some(new),
            said_ms: after,
        })
    );
}

/// A report's time is read off its signer's clock, which may run up to [`AHEAD`] fast, so one
/// stamped within ten minutes after a round with its member may have been signed before that
/// round. It does not replace what the round said: a member that signed a release under a fast
/// clock and then went back to an older build, which a round heard, still holds the checkpoint
/// gate when its earlier report arrives through a peer. Nor does such a report undo a round
/// that said nothing. A report stamped more than ten minutes after the round, counted more than
/// ten minutes after it, replaces it; the time the member was last heard moves either way.
///
/// NEGATIVE CONTROL: with [`Ledger::relay`] taking any report stamped later than the held one
/// (`at_ms > entry.said_ms`), the report signed before the round replaces what the round said:
/// `assertion failed: !ledger.relay(&early, heard_ms + MINUTE)`.
#[test]
fn a_relayed_report_stamped_within_ten_minutes_after_a_round_does_not_replace_what_the_round_said()
{
    const MINUTE: u64 = 60 * 1000;
    let (a, seed) = member(1);
    let actor = a.actor();
    let (older, newer) = (Version(0, 202, 0), RELAYS);
    let sign = |signed_ms, now_ms| {
        let wire = report_at(&seed, TRUST.network, &actor, newer, signed_ms).unwrap();
        relayed(&wire, &a, now_ms).unwrap()
    };
    // The member's clock runs nine minutes fast. It signs `newer` two minutes before the
    // round, then runs `older`, which the round hears; a peer hands the report over a minute
    // after the round.
    let heard_ms = NOW;
    let early = sign(heard_ms - 2 * MINUTE + 9 * MINUTE, heard_ms + MINUTE);
    let mut ledger = Ledger::new(TRUST);
    ledger.record(actor, heard_ms, Said::Version(older));
    let held = vec![(actor, Hold::Older(Some(older)))];
    assert_eq!(ledger.holding_below([&a], heard_ms, RELAYED), held);
    assert!(!ledger.relay(&early, heard_ms + MINUTE));
    assert_eq!(
        ledger.entry(&actor),
        Some(&Entry {
            at_ms: heard_ms + MINUTE,
            version: Some(older),
            said_ms: heard_ms,
        })
    );
    assert_eq!(ledger.holding_below([&a], heard_ms + MINUTE, RELAYED), held);
    assert!(!ledger.writes_no_secret_file(&a, heard_ms + MINUTE));

    // Stamped exactly ten minutes after the round it still does not; a millisecond more does.
    let edge = heard_ms + AHEAD;
    assert!(!ledger.relay(&sign(edge, edge), edge));
    assert_eq!(ledger.entry(&actor).unwrap().version, Some(older));
    // Stamped later still, but counted at this device's time within the ten minutes.
    assert!(!ledger.relay(&sign(edge + AHEAD, edge), edge));
    assert_eq!(ledger.entry(&actor).unwrap().version, Some(older));
    assert!(ledger.relay(&sign(edge + 1, edge + 1), edge + 1));
    assert_eq!(
        ledger.entry(&actor),
        Some(&Entry {
            at_ms: edge + 1,
            version: Some(newer),
            said_ms: edge + 1,
        })
    );

    let mut quiet = Ledger::new(TRUST);
    quiet.record(actor, heard_ms, Said::Nothing);
    assert!(!quiet.relay(&sign(edge, edge), edge));
    assert_eq!(quiet.entry(&actor).unwrap().version, None);
    assert!(quiet.relay(&sign(edge + 1, edge + 1), edge + 1));
    assert_eq!(quiet.entry(&actor).unwrap().version, Some(newer));
}

/// Rule 6: a relayed report is no contact. A ledger that only ever took relayed reports keeps
/// no watch, and a running watch lapses once no peer was heard for longer than [`GAP`], however
/// many relayed reports arrive meanwhile. A watch kept by contact is not shortened by them.
///
/// NEGATIVE CONTROL: with [`Ledger::relay`] recording its time as a contact (`contact_ms`), a
/// relayed report starts the watch: `assertion failed: !relayed_only.watch(NOW)`.
#[test]
fn a_relayed_report_neither_starts_nor_extends_the_watch() {
    let (a, seed) = member(1);
    let (peer, _) = member(2);
    let sign = |signed_ms| {
        let wire = report_at(&seed, TRUST.network, &a.actor(), RELAYS, signed_ms).unwrap();
        relayed(&wire, &a, signed_ms).unwrap()
    };
    let mut relayed_only = Ledger::new(TRUST);
    assert!(relayed_only.relay(&sign(NOW), NOW));
    assert!(!relayed_only.watch(NOW));
    assert_eq!(relayed_only.watched_since(NOW), None);
    assert!(relayed_only.text().contains("\n0 0\n"));

    let mut ledger = Ledger::new(TRUST);
    ledger.record(peer.actor(), NOW, Said::Unknown);
    assert!(ledger.watch(NOW));
    // The contact at NOW carries the watch to NOW + GAP and no further.
    ledger.relay(&sign(NOW + GAP), NOW + GAP);
    assert!(!ledger.watch(NOW + GAP));
    assert_eq!(ledger.watched_since(NOW + GAP), Some(NOW));
    for hour in 1..=7 {
        let now = NOW + GAP + hour * STEP;
        ledger.relay(&sign(now), now);
        assert!(!ledger.watch(now));
    }
    assert_eq!(ledger.watched_since(NOW + 2 * GAP), Some(NOW));
    assert_eq!(ledger.watched_since(NOW + 2 * GAP + 1), None);
    assert_eq!(
        ledger.entry(&a.actor()).unwrap().at_ms,
        NOW + GAP + 7 * STEP
    );
}

/// The two report formats never stand for each other: the 80-byte reader refuses a time-signed
/// report and the 136-byte reader an untimed one, an untimed report's signature set in a
/// time-signed frame does not verify, nor a time-signed one's in an untimed frame, and a
/// time-signed report under the untimed magic is refused.
///
/// NEGATIVE CONTROL: with `signed_at` building the untimed report's message (`signed`), the
/// untimed signature verifies in a time-signed frame: `assertion failed: relayed(&lifted, &a,
/// NOW).is_err()`.
#[test]
fn a_timed_report_is_never_read_as_an_untimed_one_nor_an_untimed_one_as_timed() {
    let (a, seed) = member(1);
    let actor = a.actor();
    let untimed = report(&seed, TRUST.network, &actor, RELAYS).unwrap();
    let timed = report_at(&seed, TRUST.network, &actor, RELAYS, NOW).unwrap();
    assert_eq!(reported(&untimed, TRUST.network, &actor).unwrap(), RELAYS);
    assert_eq!(relayed(&timed, &a, NOW).unwrap().version(), RELAYS);
    assert!(reported(&timed, TRUST.network, &actor).is_err());
    assert!(relayed(&untimed, &a, NOW).is_err());

    let mut lifted = timed;
    lifted[72..].copy_from_slice(&untimed[16..]);
    assert!(relayed(&lifted, &a, NOW).is_err());
    let mut lowered = untimed.clone();
    lowered[16..].copy_from_slice(&timed[72..]);
    assert!(reported(&lowered, TRUST.network, &actor).is_err());
    let mut renamed = timed;
    renamed[..4].copy_from_slice(b"ACV1");
    assert!(relayed(&renamed, &a, NOW).is_err());
}

/// A ledger that counted relayed reports writes the `heard` file as before: the same magic, the
/// trust's three pins, the watch, and one line of four words for each member, a relayed one's
/// in the fields a round's fills. `Ledger::text` and `Ledger::parse` are those of 0.203.0, so a
/// build without relayed reports on the same Mac reads the file and writes it back unchanged.
///
/// NEGATIVE CONTROL: with `text` writing a fifth word on each member's line, the file is not
/// the one expected, and `parse` reads none of it: `assertion `left == right` failed`, the left
/// lines ending ` relayed`.
#[test]
fn a_ledger_holding_relayed_reports_keeps_the_file_format_an_older_build_reads() {
    let (a, seed) = member(1);
    let (b, _) = member(2);
    let signed_ms = NOW - 5;
    let wire = report_at(&seed, TRUST.network, &a.actor(), RELAYS, signed_ms).unwrap();
    let mut ledger = Ledger::new(TRUST);
    assert!(ledger.relay(&relayed(&wire, &a, NOW).unwrap(), NOW));
    ledger.record(b.actor(), NOW - 9, Said::Version(LIFTED));
    let hex = aterm_codec::hex::encode_lower;
    let mut lines = [
        format!("{} {signed_ms} 0.207.0 {signed_ms}\n", hex(&a.actor())),
        format!("{} {} 0.146.0 {}\n", hex(&b.actor()), NOW - 9, NOW - 9),
    ];
    lines.sort();
    let expected = format!(
        "aterm-commons-heard 1\n{} {} {}\n0 0\n{}",
        hex(&TRUST.network),
        hex(&TRUST.authority),
        hex(&TRUST.genesis),
        lines.concat()
    );
    let text = ledger.text();
    assert_eq!(text, expected);
    let read = Ledger::parse(&text, TRUST).unwrap();
    assert_eq!(read.text(), text);
    assert_eq!(read.entry(&a.actor()), ledger.entry(&a.actor()));
}

/// The kept reports' file (`<commons>/reports`) is 90 bytes of magic, trust pins and count, then
/// 136 bytes a report, and a store reads back from it only what its members signed: every
/// report where the file is whole, the local writer's own among them and not as a first one;
/// none for an entry with a changed byte or a member off the reader's roster, the rest of the
/// file still read; and none at all from a file of another trust, a file cut short, lengthened,
/// under another magic or with a count that is not its length, or no file.
///
/// NEGATIVE CONTROL: with `Reports::parse` comparing the file's pins only through the
/// authority, a file of another genesis is read: `assertion failed: kept(&read_in(foreign,
/// &whole)).is_empty()`.
#[test]
fn kept_reports_read_back_only_as_their_members_signed_them_and_a_foreign_or_damaged_file_reads_as_none()
 {
    let (a, seed_a) = member(1);
    let (b, seed_b) = member(2);
    let (c, seed_c) = member(3);
    let roster = [&a, &b, &c];
    let sign = |seed: &[u8; 32], member: &Binding| {
        report_at(seed, TRUST.network, &member.actor(), RELAYS, NOW).unwrap()
    };
    let (wire_a, wire_c) = (sign(&seed_a, &a), sign(&seed_c, &c));
    let mut reports = store(&b, &roster);
    assert_eq!(reports.file(), None);
    assert!(take(&mut reports, &wire_a, NOW));
    assert!(take(&mut reports, &wire_c, NOW));
    assert!(reports.sign_own(&seed_b, RELAYS, NOW).unwrap());
    let whole = reports.file().unwrap();
    assert_eq!(reports.file(), None);
    let head = [
        b"ACVRPT1\0".as_slice(),
        &TRUST.network,
        &TRUST.authority,
        &TRUST.genesis,
        &3u16.to_le_bytes(),
    ]
    .concat();
    assert_eq!(head.len(), 90);
    assert_eq!(whole[..90], head[..]);
    assert_eq!(whole.len(), 90 + 3 * 136);

    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("reports");
    let read_in = |trust: Trust, bytes: &[u8]| {
        fsx::write_atomic(&path, bytes, 0o600).unwrap();
        let mut read = Reports::new(trust, &b);
        read.set_roster(roster, NOW);
        read.load_from(&path, NOW);
        read
    };
    let sorted = |mut wires: Vec<[u8; REPORT2_LEN]>| {
        wires.sort();
        wires
    };
    let mut read = read_in(TRUST, &whole);
    assert_eq!(sorted(kept(&read)), sorted(kept(&reports)));
    assert_eq!(read.file(), None);
    // Its own report read back is the one it kept, not a first one to follow an hour later.
    assert!(reports.own_due(RELAYS, NOW + STEP));
    assert!(!read.own_due(RELAYS, NOW + STEP));
    assert!(read.own_due(RELAYS, NOW + REFRESH));

    // One bit of A's signature changed: A's entry is not read, the others are, and the file is
    // due to be written without it.
    let at = whole
        .windows(REPORT2_LEN)
        .position(|entry| entry == wire_a)
        .unwrap();
    let mut changed = whole.clone();
    changed[at + REPORT2_LEN - 1] ^= 1;
    let mut read = read_in(TRUST, &changed);
    assert_eq!(kept(&read).len(), 2);
    assert!(!kept(&read).contains(&wire_a));
    assert_eq!(read.file().map(|file| file.len()), Some(90 + 2 * 136));
    // A reader whose roster lacks A reads the other two.
    fsx::write_atomic(&path, &whole, 0o600).unwrap();
    let mut without = store(&b, &[&b, &c]);
    without.load_from(&path, NOW);
    assert_eq!(kept(&without).len(), 2);
    assert!(!kept(&without).contains(&wire_a));

    let foreign = Trust {
        genesis: [14; 32],
        ..TRUST
    };
    assert!(kept(&read_in(foreign, &whole)).is_empty());
    assert!(kept(&read_in(TRUST, &whole[..whole.len() - 1])).is_empty());
    assert!(kept(&read_in(TRUST, &[whole.as_slice(), &[0]].concat())).is_empty());
    let mut renamed = whole.clone();
    renamed[6] = b'2';
    assert!(kept(&read_in(TRUST, &renamed)).is_empty());
    let mut miscounted = whole.clone();
    miscounted[88] = 2;
    assert!(kept(&read_in(TRUST, &miscounted)).is_empty());
    assert!(kept(&read_in(TRUST, &[])).is_empty());
    let mut absent = store(&b, &roster);
    absent.load_from(&dir.path().join("none"), NOW);
    assert!(kept(&absent).is_empty());
}

/// When the local writer signs: at once where it is on the roster and keeps no report of its
/// own; once more an hour after that first report; then a day after its last, or an hour after
/// it where the running release is another. It signs at the caller's time, with its own key
/// only, and never while it is off the roster.
///
/// NEGATIVE CONTROL: with `own_due` not counting a first report (`early`), no second follows
/// an hour later: `assertion failed: own.own_due(RELAYS, NOW + STEP)`.
#[test]
fn a_member_signs_again_after_a_day_or_an_hour_after_a_new_release_and_once_an_hour_after_its_first()
 {
    let (a, seed_a) = member(1);
    let (b, seed) = member(2);
    let mut off = store(&b, &[&a]);
    assert!(!off.own_due(RELAYS, NOW));
    assert!(!off.sign_own(&seed, RELAYS, NOW).unwrap());
    assert!(kept(&off).is_empty());

    let mut own = store(&b, &[&a, &b]);
    let signed = |own: &Reports| {
        let (kept, _) = own.since(0);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].actor(), b.actor());
        (kept[0].version(), kept[0].signed_ms())
    };
    assert!(own.own_due(RELAYS, NOW));
    assert!(own.sign_own(&seed_a, RELAYS, NOW).is_err());
    assert!(kept(&own).is_empty());
    assert!(own.sign_own(&seed, RELAYS, NOW).unwrap());
    assert_eq!(signed(&own), (RELAYS, NOW));

    assert!(!own.sign_own(&seed, RELAYS, NOW + STEP - 1).unwrap());
    assert!(own.own_due(RELAYS, NOW + STEP));
    assert!(own.sign_own(&seed, RELAYS, NOW + STEP).unwrap());
    let second = NOW + STEP;
    assert_eq!(signed(&own), (RELAYS, second));
    // That happens once: the next is a day later, or an hour later on another release.
    assert!(!own.own_due(RELAYS, second + STEP));
    assert!(!own.own_due(RELAYS, second + REFRESH - 1));
    assert!(own.own_due(RELAYS, second + REFRESH));
    let next = Version(0, 208, 0);
    assert!(!own.own_due(next, second + STEP - 1));
    assert!(own.own_due(next, second + STEP));
    assert!(own.sign_own(&seed, next, second + STEP).unwrap());
    assert_eq!(signed(&own), (next, second + STEP));
    assert!(!own.own_due(next, second + 2 * STEP));
    assert!(own.sign_own(&seed, next, second + STEP + REFRESH).unwrap());
    assert_eq!(signed(&own), (next, second + STEP + REFRESH));
}

/// A report signed and verified outside the store is kept as the local writer's own only where
/// it is that writer's, under this trust, and one is due: as [`Reports::sign_own`] keeps one,
/// with the signing done by a caller that holds no lock on the store. Another member's report
/// is not kept this way, nor the writer's own while none is due, one signed less than an hour
/// after the kept one, or one made for another network; a member off the roster keeps none.
///
/// NEGATIVE CONTROL: with `accept_own` not comparing the report's actor with the local
/// writer's, A's report is kept as B's own: `assertion failed: !own.accept_own(of_a, NOW)`.
#[test]
fn a_store_keeps_a_report_signed_outside_it_only_as_its_own_writers_and_only_when_one_is_due() {
    let (a, seed_a) = member(1);
    let (b, seed) = member(2);
    let made = |seed: &[u8; 32], member: &Binding, signed_ms, now_ms| {
        let wire = report_at(seed, *member.network(), &member.actor(), RELAYS, signed_ms).unwrap();
        (relayed(&wire, member, now_ms).unwrap(), wire)
    };
    let mut own = store(&b, &[&a, &b]);
    let (of_a, _) = made(&seed_a, &a, NOW, NOW);
    assert!(!own.accept_own(of_a, NOW));
    let (elsewhere, _) = enrolled(2, [8; 16], TRUST.authority);
    let (foreign, _) = made(&seed, &elsewhere, NOW, NOW);
    assert!(!own.accept_own(foreign, NOW));
    assert!(kept(&own).is_empty());

    let (first, first_wire) = made(&seed, &b, NOW, NOW);
    // A peer never hands this store its own writer's report; its own signing does.
    assert!(!own.accept(first.clone(), NOW));
    assert!(own.accept_own(first, NOW));
    assert_eq!(kept(&own), [first_wire]);
    let (soon, _) = made(&seed, &b, NOW + STEP - 1, NOW + STEP - 1);
    assert!(!own.accept_own(soon, NOW + STEP - 1));
    // Due an hour after the first, but not with a report signed before that hour passed.
    let (stale, _) = made(&seed, &b, NOW + 1, NOW + STEP);
    assert!(!own.accept_own(stale, NOW + STEP));
    assert_eq!(kept(&own), [first_wire]);
    let (second, second_wire) = made(&seed, &b, NOW + STEP, NOW + STEP);
    assert!(own.accept_own(second, NOW + STEP));
    assert_eq!(kept(&own), [second_wire]);
    assert!(!own.own_due(RELAYS, NOW + 2 * STEP));

    let mut off = store(&b, &[&a]);
    let (unlisted, _) = made(&seed, &b, NOW, NOW);
    assert!(!off.accept_own(unlisted, NOW));
    assert!(kept(&off).is_empty());
}

/// What a store offers one peer: each kept report once, in the order it took them, and from
/// then on only what it took since; never the peer's own report, and never one the peer's own
/// offer in the same round names at the same or a later time. Each peer has its own mark, and a
/// mark another store made (one from before a restart) covers nothing, so every report is
/// offered again. What it gives is the kept report of each entry the answer wanted.
///
/// NEGATIVE CONTROL: with `Reports::offer` not reading the peer's own offer (`theirs`), A's
/// report is offered back to the peer that just named it: `` left: [([..], 34552800000),
/// ([..], 34559999999), ([..], 34560000000)] right: [([..], 34559999999), ([..],
/// 34560000000)] `` (tags elided).
#[test]
fn a_peer_is_offered_each_kept_report_once_and_never_its_own_or_one_it_just_offered() {
    let (a, seed_a) = member(1);
    let (b, seed_b) = member(2);
    let (c, seed_c) = member(3);
    let (d, seed_d) = member(4);
    let roster = [&a, &b, &c, &d];
    let sign = |seed: &[u8; 32], member: &Binding, signed_ms| {
        report_at(seed, TRUST.network, &member.actor(), RELAYS, signed_ms).unwrap()
    };
    let a_ms = NOW - 2 * STEP;
    let wires = [
        sign(&seed_a, &a, a_ms),
        sign(&seed_c, &c, NOW - 2),
        sign(&seed_d, &d, NOW - 1),
    ];
    let fill = |reports: &mut Reports| {
        for wire in &wires {
            assert!(take(reports, wire, NOW));
        }
        assert!(reports.sign_own(&seed_b, RELAYS, NOW).unwrap());
    };
    let mut reports = store(&b, &roster);
    fill(&mut reports);
    let (of_a, of_b, of_c, of_d) = (
        (tag(&a.actor()), a_ms),
        (tag(&b.actor()), NOW),
        (tag(&c.actor()), NOW - 2),
        (tag(&d.actor()), NOW - 1),
    );

    let first = reports.offer(Mark::default(), &c.actor(), &[], NOW);
    assert_eq!(first.entries(), [of_a, of_d, of_b]);
    let own = kept(&reports)[3];
    assert_eq!(reports.give(&first, &[true, false, true]), [wires[0], own]);
    assert!(reports.give(&first, &[true, true]).is_empty());
    let mark = first.covers();
    let again = reports.offer(mark, &c.actor(), &[], NOW);
    assert!(again.entries().is_empty());
    assert_eq!(again.covers(), mark);
    // An offer of nothing covers what the mark did, so answering it moves nothing.
    let none = Offer::none(mark);
    assert!(none.entries().is_empty());
    assert_eq!(none.covers(), mark);
    assert!(reports.give(&none, &[]).is_empty());
    // Another peer has its own mark.
    let to_d = reports.offer(Mark::default(), &d.actor(), &[], NOW);
    assert_eq!(to_d.entries(), [of_a, of_c, of_b]);

    // The peer's own offer names A's report: at that time or later it has it, earlier it has not.
    let unnamed = |theirs: &[(Tag, u64)]| {
        reports
            .offer(Mark::default(), &c.actor(), theirs, NOW)
            .entries()
            .to_vec()
    };
    assert_eq!(unnamed(&[of_a]), [of_d, of_b]);
    assert_eq!(unnamed(&[(of_a.0, of_a.1 + 1)]), [of_d, of_b]);
    assert_eq!(unnamed(&[(of_a.0, of_a.1 - 1)]), [of_a, of_d, of_b]);

    // A report taken after the mark is offered alone.
    let later = sign(&seed_a, &a, a_ms + STEP);
    assert!(take(&mut reports, &later, NOW));
    let next = reports.offer(mark, &c.actor(), &[], NOW);
    assert_eq!(next.entries(), [(of_a.0, a_ms + STEP)]);
    assert_eq!(reports.give(&next, &[true]), [later]);
    assert_ne!(next.covers(), mark);

    // A store made after a restart is offered from its start under the old store's mark.
    let mut restarted = store(&b, &roster);
    fill(&mut restarted);
    let all = restarted.offer(next.covers(), &c.actor(), &[], NOW);
    assert_eq!(all.entries(), [of_a, of_d, of_b]);
    assert_ne!(all.covers(), mark);
}

/// What a ledger is fed of a store: [`Reports::since`] with the stamp its last read returned
/// gives only the reports the store took after that read, in the order it took them, and the
/// stamp to read from next. So each kept report is fed once, a report that replaced a kept one
/// is fed as a new one, a read with nothing new returns none and the same stamp, and a read
/// from 0 returns every kept report.
///
/// NEGATIVE CONTROL: with `Reports::since` reading from the start whatever its stamp
/// (`self.newer(0)`), the read after the first returns the first report again:
/// `` left: ([Relayed { .. }], 1) right: ([], 1) ``.
#[test]
fn a_store_is_read_from_a_stamp_for_only_what_it_took_since() {
    let (a, seed_a) = member(1);
    let (b, _) = member(2);
    let (c, seed_c) = member(3);
    let sign = |seed: &[u8; 32], member: &Binding, signed_ms| {
        report_at(seed, TRUST.network, &member.actor(), RELAYS, signed_ms).unwrap()
    };
    let wires = |read: &[Relayed]| read.iter().map(|report| *report.wire()).collect::<Vec<_>>();
    let mut reports = store(&b, &[&a, &b, &c]);
    assert_eq!(reports.since(0), (Vec::new(), 0));

    let of_a = sign(&seed_a, &a, NOW - STEP);
    assert!(take(&mut reports, &of_a, NOW));
    let (first, read) = reports.since(0);
    assert_eq!(wires(&first), [of_a]);
    assert_eq!(reports.since(read), (Vec::new(), read));

    let of_c = sign(&seed_c, &c, NOW);
    assert!(take(&mut reports, &of_c, NOW));
    let (second, next) = reports.since(read);
    assert_eq!(second.len(), 1);
    assert_eq!(wires(&second), [of_c]);
    assert!(next > read);

    // A's later report replaces its kept one and is read as taken last.
    let moved = sign(&seed_a, &a, NOW);
    assert!(take(&mut reports, &moved, NOW));
    let (third, last) = reports.since(next);
    assert_eq!(wires(&third), [moved]);
    assert_eq!(reports.since(last), (Vec::new(), last));
    assert_eq!(wires(&reports.since(read).0), [of_c, moved]);
    assert_eq!(wires(&reports.since(0).0), [of_c, moved]);
}

/// A report this device signed while its clock ran far ahead does not stop it signing once the
/// clock is right: at the next roster, the same roster as before, a kept report stamped more
/// than [`AHEAD`] past the clock is dropped, the device's own included, so its own report is
/// due at once and is signed at the clock's time. One stamped [`AHEAD`] past the clock is kept.
///
/// NEGATIVE CONTROL: with `Reports::set_roster` dropping only a report whose member left or
/// that was signed [`RECENT`] ago (no bound on a time ahead), the far-ahead report stays and
/// none is due for a day: `assertion failed: kept(&reports).is_empty()`.
#[test]
fn a_kept_report_stamped_far_ahead_is_dropped_at_the_next_roster_and_the_device_signs_again() {
    let (a, seed_a) = member(1);
    let (b, seed) = member(2);
    let roster = [&a, &b];
    let mut reports = store(&b, &roster);
    // The clock read an hour ahead when this device signed and when it took A's report.
    let fast = NOW + STEP;
    assert!(reports.sign_own(&seed, RELAYS, fast).unwrap());
    let of_a = report_at(&seed_a, TRUST.network, &a.actor(), RELAYS, fast).unwrap();
    assert!(take(&mut reports, &of_a, fast));
    assert!(reports.file().is_some());
    assert!(!reports.own_due(RELAYS, NOW));

    // Still within the bound at a clock ten minutes short of that hour.
    assert!(!reports.set_roster(roster, fast - AHEAD));
    assert_eq!(kept(&reports).len(), 2);
    assert_eq!(reports.file(), None);

    assert!(!reports.set_roster(roster, NOW));
    assert!(kept(&reports).is_empty());
    assert_eq!(reports.file().map(|file| file.len()), Some(90));
    assert!(reports.own_due(RELAYS, NOW));
    assert!(reports.sign_own(&seed, RELAYS, NOW).unwrap());
    let (own, _) = reports.since(0);
    assert_eq!((own[0].actor(), own[0].signed_ms()), (b.actor(), NOW));
}

/// A store moves the report it keeps for a member only to one signed an hour or more after it:
/// one signed a millisecond short of that is neither wanted, screened nor kept, nor is an older
/// one, so a member that signs often changes what each device keeps of it, and writes to its
/// file, at most once an hour.
///
/// NEGATIVE CONTROL: with `Reports::takes` not comparing a report's time with the kept one's,
/// a report signed a millisecond short of an hour later is wanted: `` left: [true] right:
/// [false] ``.
#[test]
fn a_member_moves_its_kept_report_at_most_once_an_hour() {
    let (a, seed) = member(1);
    let (c, _) = member(3);
    let sign = |signed_ms| report_at(&seed, TRUST.network, &a.actor(), RELAYS, signed_ms).unwrap();
    let first_ms = NOW - 2 * STEP;
    let name = tag(&a.actor());
    let mut reports = store(&c, &[&a, &c]);
    assert!(take(&mut reports, &sign(first_ms), NOW));
    assert!(reports.file().is_some());

    let short = sign(first_ms + STEP - 1);
    assert_eq!(reports.want(&[(name, first_ms + STEP - 1)], NOW), [false]);
    assert!(reports.screen(&short, NOW).is_none());
    assert!(!reports.accept(relayed(&short, &a, NOW).unwrap(), NOW));
    assert_eq!(reports.want(&[(name, first_ms)], NOW), [false]);
    assert!(!take(&mut reports, &sign(first_ms - 1), NOW));
    assert_eq!(kept(&reports), [sign(first_ms)]);
    assert_eq!(reports.file(), None);

    let moved = sign(first_ms + STEP);
    assert_eq!(reports.want(&[(name, first_ms + STEP)], NOW), [true]);
    assert!(take(&mut reports, &moved, NOW));
    assert_eq!(kept(&reports), [moved]);
    assert!(reports.file().is_some());
    assert!(!take(&mut reports, &sign(first_ms), NOW));
    assert!(!take(&mut reports, &short, NOW));
    assert_eq!(kept(&reports), [moved]);
}

/// A member that left the roster is dropped from a store at once: its kept report is no longer
/// kept, fed to the ledger, written to the file or offered, and a report of it is neither
/// wanted, screened nor taken until it is on the roster again.
///
/// NEGATIVE CONTROL: with `Reports::set_roster` keeping the reports of members that left, A's
/// stays: `assertion failed: !kept(&reports).contains(&wire_a)`.
#[test]
fn a_member_gone_from_the_roster_is_neither_kept_nor_offered() {
    let (a, seed_a) = member(1);
    let (b, _) = member(2);
    let (c, seed_c) = member(3);
    let (d, _) = member(4);
    let wire_a = report_at(&seed_a, TRUST.network, &a.actor(), RELAYS, NOW).unwrap();
    let wire_c = report_at(&seed_c, TRUST.network, &c.actor(), RELAYS, NOW).unwrap();
    let mut reports = store(&b, &[&a, &b, &c, &d]);
    assert!(take(&mut reports, &wire_a, NOW));
    assert!(take(&mut reports, &wire_c, NOW));
    assert_eq!(reports.file().map(|file| file.len()), Some(90 + 2 * 136));
    let offered = |reports: &Reports| {
        reports
            .offer(Mark::default(), &d.actor(), &[], NOW)
            .entries()
            .to_vec()
    };
    assert_eq!(
        offered(&reports),
        [(tag(&a.actor()), NOW), (tag(&c.actor()), NOW)]
    );
    // An offer made while A was a member gives nothing of A once it left.
    let before = reports.offer(Mark::default(), &d.actor(), &[], NOW);

    assert!(reports.set_roster([&b, &c, &d], NOW));
    assert!(!kept(&reports).contains(&wire_a));
    assert_eq!(kept(&reports), [wire_c]);
    assert_eq!(offered(&reports), [(tag(&c.actor()), NOW)]);
    assert_eq!(reports.give(&before, &[true, true]), [wire_c]);
    assert_eq!(reports.file().map(|file| file.len()), Some(90 + 136));
    assert_eq!(reports.want(&[(tag(&a.actor()), NOW)], NOW), [false]);
    assert!(reports.screen(&wire_a, NOW).is_none());
    assert!(!take(&mut reports, &wire_a, NOW));
    assert!(!reports.set_roster([&b, &c, &d], NOW));
    assert_eq!(reports.file(), None);

    assert!(reports.set_roster([&a, &b, &c, &d], NOW));
    assert!(take(&mut reports, &wire_a, NOW));
    assert_eq!(kept(&reports), [wire_c, wire_a]);
}

/// A store has a roster only once it was given a member of its own trust. Before that, and
/// under a roster of another network's members alone, it says so and wants, screens and keeps
/// nothing, so a caller can leave a peer's offer unanswered and not answer that it wants none.
///
/// NEGATIVE CONTROL: with `Reports::enrolled` true of every store, a new store reads as having
/// a roster: `assertion failed: !reports.enrolled()`.
#[test]
fn a_store_has_a_roster_only_once_it_was_given_a_member_of_its_trust() {
    let (a, seed_a) = member(1);
    let (b, _) = member(2);
    let wire = report_at(&seed_a, TRUST.network, &a.actor(), RELAYS, NOW).unwrap();
    let offered = [(tag(&a.actor()), NOW)];
    let mut reports = Reports::new(TRUST, &b);
    assert!(!reports.enrolled());
    assert_eq!(reports.want(&offered, NOW), [false]);
    assert!(reports.screen(&wire, NOW).is_none());
    assert!(!take(&mut reports, &wire, NOW));

    let (elsewhere, _) = enrolled(1, [8; 16], TRUST.authority);
    assert!(!reports.set_roster([&elsewhere], NOW));
    assert!(!reports.enrolled());
    assert_eq!(reports.want(&offered, NOW), [false]);

    assert!(reports.set_roster([&a, &b], NOW));
    assert!(reports.enrolled());
    assert_eq!(reports.want(&offered, NOW), [true]);
    assert!(take(&mut reports, &wire, NOW));
    assert!(reports.set_roster([], NOW));
    assert!(!reports.enrolled());
    assert!(kept(&reports).is_empty());
}

/// A member whose keys come from `n`, for a roster larger than [`member`] can number.
fn numbered(n: u16) -> (Binding, [u8; 32]) {
    let fill = |base: u8| {
        let mut bytes = [base; 32];
        bytes[..2].copy_from_slice(&n.to_le_bytes());
        bytes
    };
    let seed = fill(0x51);
    let request = Request::create(
        TRUST.network,
        TRUST.authority,
        fill(0x61)[..16].try_into().unwrap(),
        fill(0x71)[..16].try_into().unwrap(),
        &Identity::from_secret_bytes(fill(0x41)),
        &seed,
    )
    .unwrap();
    (request.binding().clone(), seed)
}

/// A store that keeps more reports than one offer holds offers [`RELAY_BATCH`] of them, the
/// most one flight carries, and the rest in the next round from the mark the first left: every
/// report is offered exactly once.
///
/// NEGATIVE CONTROL: with `Reports::offer` moving its mark to a report before finding the offer
/// full, the first report past the batch is never offered, and the next round offers three
/// where four are left: `` left: 3 right: 4 ``.
#[test]
fn a_store_offers_a_batch_a_round_and_the_rest_from_the_mark_it_left() {
    let (b, _) = member(2);
    let (peer, _) = member(3);
    let members: Vec<(Binding, [u8; 32])> = (0..RELAY_BATCH as u16 + 4).map(numbered).collect();
    let mut roster: Vec<&Binding> = members.iter().map(|(member, _)| member).collect();
    roster.extend([&b, &peer]);
    let mut reports = store(&b, &roster);
    for (member, seed) in &members {
        let wire = report_at(seed, TRUST.network, &member.actor(), RELAYS, NOW).unwrap();
        assert!(take(&mut reports, &wire, NOW));
    }
    let first = reports.offer(Mark::default(), &peer.actor(), &[], NOW);
    assert_eq!(first.entries().len(), RELAY_BATCH);
    assert_eq!(offer_flight(first.entries()).len(), OFFER_MAX);
    let all = vec![true; RELAY_BATCH];
    assert_eq!(
        give_flight(&all, &reports.give(&first, &all)).len(),
        GIVE_MAX
    );
    let second = reports.offer(first.covers(), &peer.actor(), &[], NOW);
    assert_eq!(second.entries().len(), 4);
    let third = reports.offer(second.covers(), &peer.actor(), &[], NOW);
    assert!(third.entries().is_empty());
    let named: std::collections::BTreeSet<Tag> = [first.entries(), second.entries()]
        .concat()
        .iter()
        .map(|(name, _)| *name)
        .collect();
    assert_eq!(named.len(), members.len());
}

/// Each of the four flights reads back what was written, at its largest exactly the size its
/// reader is bounded to, and an idle round is 20 bytes with its two frame lengths. A flight is
/// refused under another flight's magic, a byte short or long, with want bits for another
/// number of entries or a bit set past the last, with more entries than a batch, and with more
/// reports than its reader wanted. Of the reports given, a side takes one for each tag it
/// wanted and no other.
///
/// NEGATIVE CONTROL: with `Rest::end` passing a flight that has bytes left over, a flight one
/// byte long is read: `assertion failed: read_offer(&long(&offer)).is_err()`.
#[test]
fn a_relay_flight_is_read_only_whole_under_its_own_magic_and_for_the_counts_it_answers() {
    let entries: Vec<(Tag, u64)> = (0..RELAY_BATCH)
        .map(|n| ([n as u8; 16], NOW + n as u64))
        .collect();
    let wires: Vec<[u8; REPORT2_LEN]> = (0..RELAY_BATCH).map(|n| [n as u8; REPORT2_LEN]).collect();
    let want: Vec<bool> = (0..RELAY_BATCH).map(|n| n % 3 == 0).collect();
    let wanted = want.iter().filter(|want| **want).count();
    let offer = offer_flight(&entries);
    let answer = answer_flight(&want, &entries);
    let give = give_flight(&want, &wires[..wanted]);
    let full = give_flight(&want, &wires);
    let last = last_flight(&wires);
    assert_eq!(
        (offer.len(), answer.len(), full.len(), last.len()),
        (OFFER_MAX, ANSWER_MAX, GIVE_MAX, LAST_MAX)
    );
    assert_eq!(
        (OFFER_MAX, ANSWER_MAX, GIVE_MAX, LAST_MAX),
        (6150, 6182, 34854, 34822)
    );
    assert_eq!(read_offer(&offer).unwrap(), entries);
    assert_eq!(
        read_answer(&answer, RELAY_BATCH).unwrap(),
        (want.clone(), entries.clone())
    );
    assert_eq!(
        read_give(&give, RELAY_BATCH, wanted).unwrap(),
        (want.clone(), wires[..wanted].to_vec())
    );
    assert_eq!(read_last(&last, RELAY_BATCH).unwrap(), wires);
    // Eleven entries take two bytes of want bits.
    let odd = answer_flight(&want[..11], &entries[..2]);
    assert_eq!(odd.len(), 4 + 2 + 2 + 2 * 24);
    assert_eq!(
        read_answer(&odd, 11).unwrap(),
        (want[..11].to_vec(), entries[..2].to_vec())
    );
    assert_eq!(
        4 + offer_flight(&[]).len() + 4 + answer_flight(&[], &[]).len(),
        20
    );

    let long = |flight: &[u8]| [flight, &[0]].concat();
    assert!(read_offer(&long(&offer)).is_err());
    assert!(read_answer(&long(&answer), RELAY_BATCH).is_err());
    assert!(read_give(&long(&give), RELAY_BATCH, wanted).is_err());
    assert!(read_last(&long(&last), RELAY_BATCH).is_err());
    assert!(read_offer(&offer[..offer.len() - 1]).is_err());
    assert!(read_answer(&answer[..answer.len() - 1], RELAY_BATCH).is_err());
    assert!(read_give(&give[..give.len() - 1], RELAY_BATCH, wanted).is_err());
    assert!(read_last(&last[..last.len() - 1], RELAY_BATCH).is_err());
    // Each flight under the reader of another.
    assert!(read_offer(&last_flight(&[])).is_err());
    assert!(read_last(&offer_flight(&[]), 0).is_err());
    assert!(read_answer(&give_flight(&[], &[]), 0).is_err());
    assert!(read_give(&answer_flight(&[], &[]), 0, 0).is_err());
    // Want bits for another number of entries, and a bit set past the last entry.
    assert!(read_answer(&answer, RELAY_BATCH - 8).is_err());
    assert!(read_answer(&answer, RELAY_BATCH - 1).is_err());
    assert!(read_give(&give, RELAY_BATCH + 1, wanted).is_err());
    // More than was wanted, and more than a batch.
    assert!(read_give(&give, RELAY_BATCH, wanted - 1).is_err());
    assert!(read_last(&last, RELAY_BATCH - 1).is_err());
    let mut over = b"ACG1".to_vec();
    over.extend_from_slice(&(RELAY_BATCH as u16 + 1).to_le_bytes());
    over.resize(6 + (RELAY_BATCH + 1) * 24, 0);
    assert!(read_offer(&over).is_err());
    let mut wide = b"ACG4".to_vec();
    wide.extend_from_slice(&(RELAY_BATCH as u16 + 1).to_le_bytes());
    wide.resize(6 + (RELAY_BATCH + 1) * REPORT2_LEN, 0);
    assert!(read_last(&wide, RELAY_BATCH + 1).is_err());

    // What a side takes of what it is given: one report for each tag it wanted.
    let (a, seed_a) = member(1);
    let (b, seed_b) = member(2);
    let (c, seed_c) = member(3);
    let sign = |seed: &[u8; 32], member: &Binding, signed_ms| {
        report_at(seed, TRUST.network, &member.actor(), RELAYS, signed_ms).unwrap()
    };
    let (of_a, again, of_b, of_c) = (
        sign(&seed_a, &a, NOW),
        sign(&seed_a, &a, NOW + 1),
        sign(&seed_b, &b, NOW),
        sign(&seed_c, &c, NOW),
    );
    let offered = [(tag(&a.actor()), NOW), (tag(&b.actor()), NOW)];
    let given = [of_c, of_b, of_a, again, [0; REPORT2_LEN]];
    assert_eq!(taken(&offered, &[true, false], &given), [&of_a]);
    assert!(taken(&offered, &[false, false], &given).is_empty());
}
