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
