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

fn member(n: u8) -> (Binding, [u8; 32]) {
    let seed = [n + 31; 32];
    let request = Request::create(
        TRUST.network,
        TRUST.authority,
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
