// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Signature-**during**-selection (§5) — choosing which published index to trust.
//!
//! The reused release-listing flow (`aterm-update`'s `github.rs`) picks the highest
//! `build_number` release and *then* parses it. That ordering is exploitable here: a
//! repo-write adversary could publish a release carrying a huge (API-reported) build
//! number but an **unsigned / garbage `index.toml`**, and either DoS updates (the real
//! signed index is never reached) or shadow a lower, genuinely-signed index. So the order
//! is inverted: verify **first**, select **after**.
//!
//! [`select_index`] considers a candidate only if the WHOLE chain holds for it — the
//! candidate's master-signed roster admits, the index verifies under a machine that
//! roster still authorizes, the index's own attribution binds to the machine that
//! actually signed, and its *signed* `index_build` (the one inside the verified bytes —
//! never the unsigned API-reported number) is ≥ the durable high-water `floor`. Among
//! those it returns the highest signed `index_build`. A candidate that fails ANY of that
//! is **skipped, not a global abort**, so an unsigned high-build release can never
//! suppress a lower signed one. Freshness (`valid_until`) and the *advancing* floor
//! writes are applied by the caller to the winner (§8 gates 2–3).
//!
//! # The roster travels WITH the index, per candidate
//!
//! Each candidate carries the roster generation published beside it, not a roster fetched
//! once for the repo. Two reasons, and the second is the load-bearing one:
//!
//! * a release is a self-contained authorization unit — the machine that cut it, the
//!   generation that authorized that machine, and the index it signed all ride together,
//!   exactly as the appcast and `aterm-machines.toml` do on an aterm release;
//! * a single repo-wide roster fetch would have to be reconciled against N candidate
//!   indexes of different ages, and the natural way to do that ("use the newest roster
//!   for all of them") is precisely how an attacker who can publish ONE fresh roster
//!   revives an old index that generation still authorizes. Per-candidate binding makes
//!   `roster_seq` inside the index have to match the roster served with it.
//!
//! # A suppressed roster is a REFUSAL, never a downgrade
//!
//! A release carrying `index.toml` + `.sig` but no roster pair yields no candidate at
//! all — [`crate::net`] will not even build one. Whoever serves the index can therefore
//! stop atpkg from installing, which is a denial of service they already had (they serve
//! the index). What they cannot do is get an older root honoured instead: there is no
//! older root left to fall back to.
//!
//! # THE NEWEST ADMITTED GENERATION DECIDES — ranking is not by `index_build` alone
//!
//! Because the roster travels PER CANDIDATE, two candidates in one fetch can disagree about
//! who is authorized. Ranking them by signed `index_build` alone — the rule that predates
//! the roster and knew nothing about generations — hands the fetch to whichever party can
//! publish the bigger number, and the party who wants to is precisely the one being revoked:
//!
//! ```text
//!   owner  : roster_seq 5 (revokes m11), index_build  50, signed by m3
//!   thief  : roster_seq 4 (m11 still listed), index_build 100, signed by m11
//!   ranked by index_build alone  ⇒  the REVOKED machine wins, forever, because it only
//!                                    has to keep outbidding the owner's build number.
//! ```
//!
//! So [`select_index`] runs in two passes. Pass one admits every candidate's roster and
//! takes the HIGHEST generation admitted; pass two considers only the candidates carrying
//! that generation. Anything older is discarded before its index signature is ever checked
//! — the same "leave the candidate set before any crypto" rule revocation itself uses, one
//! level up. A generation the client has merely SEEN kills every older one in the same
//! pass, and [`Selection::observed_roster_seq`] is what the caller ratchets durably so it
//! stays dead in every later pass too.
//!
//! This does hand a repo-write attacker a way to suppress older candidates by serving the
//! newest (public, master-signed) roster beside a garbage index. That is not a downgrade —
//! it is the correct reading of "generation 5 exists", and it is the denial of service they
//! already had. What they cannot do is revive generation 4.

use crate::sig::{Anchor, BuildFloor, TrustedIndex, TrustedRoster, VerifiedBytes, admit_roster};

/// One release's signed index + the master-signed roster published beside it.
///
/// All four blobs are RAW wire bytes and are handed to the verifier unmodified — no lossy
/// conversion, no re-serialization. Anything that stores or forwards a `Candidate` (the
/// §14 cache, the chained fetcher) is moving BYTES, never trust.
#[derive(Debug, Clone)]
pub struct Candidate {
    /// Release tag / id (diagnostics only — never trusted for selection).
    pub label: String,
    /// The raw `index.toml` asset bytes (verified as-is; never lossily converted).
    pub index_bytes: Vec<u8>,
    /// The detached `index.toml.sig` bytes — a MACHINE signature under the roster below.
    pub sig: Vec<u8>,
    /// The raw `aterm-machines.toml` bytes published on the same release.
    pub roster_bytes: Vec<u8>,
    /// The detached `aterm-machines.toml.sig` bytes — the PAPER MASTER's signature, and
    /// the only thing in this struct that a pinned key verifies directly.
    pub roster_sig: Vec<u8>,
}

/// The chosen index: authorized by a master-signed roster generation, past the floor,
/// plus its raw verified bytes (for the caller to record / re-use) and the originating
/// release label.
#[derive(Debug)]
pub struct Selected {
    /// The release the winning index came from.
    pub label: String,
    /// The parsed index, paired with the roster generation that authorized it — which is
    /// also the authority every `pkg-*.toml` under it verifies against.
    pub index: TrustedIndex,
    /// The exact verified bytes the index was parsed from.
    pub verified: VerifiedBytes,
}

/// What one verify-then-select pass learned: the winner (if any) **and** the newest roster
/// generation the pass admitted.
///
/// The second field is returned even when nothing was selected, and that is the point.
/// Admitting a generation is an OBSERVATION, and the replay defence is only worth having if
/// an observation is durable: a client that verified generation *n* must refuse *n-1*
/// forever after, whether or not it went on to install anything. Returning it separately —
/// rather than reading it back off the winner — means a pass that selected nothing (every
/// index refused, a fetch that only carried the revoking roster, a plan that held on a local
/// pin) still ratchets. `aterm-update`'s sibling tier ratchets on observation for exactly
/// this reason; atpkg used to ratchet only after a completed install, which left a client
/// that saw a revocation but installed nothing accepting the pre-revocation roster — and the
/// revoked machine with it — on the next pass.
#[derive(Debug)]
pub struct Selection {
    /// The winning index, or `None` if no candidate survived the whole chain.
    pub selected: Option<Selected>,
    /// The highest `roster_seq` ADMITTED in this pass (master-signed, parsed from the
    /// verified wrapper, above the durable floor, and fresh). `0` if none was. The caller
    /// ratchets its durable roster floor to this before doing anything else with the
    /// winner.
    pub observed_roster_seq: u64,
}

/// One candidate that got past the roster tier: the generation that admitted it, and the
/// index bytes still waiting to be verified under it.
struct Admitted {
    label: String,
    index_bytes: Vec<u8>,
    sig: Vec<u8>,
    roster: TrustedRoster,
}

/// Verify-then-select over `candidates` (see the module docs). `anchor` is the pinned
/// paper-master keyset plus the durable `roster_seq` floor; `floor` is the durable
/// `index_build` high-water **and the generation that set it** ([`BuildFloor`]); `now_unix`
/// drives the roster's freshness window and the per-machine expiry gates.
///
/// Returns the highest-signed-`index_build` candidate that survives the whole chain, among
/// only those carrying the NEWEST generation this pass admitted — plus that generation, for
/// the caller's durable ratchet.
///
/// The caller still applies the index's own freshness gate and the *advancing* floor
/// writes (this function only reads the floors as filters).
#[must_use]
pub fn select_index(
    anchor: &Anchor,
    candidates: Vec<Candidate>,
    floor: BuildFloor,
    now_unix: i64,
) -> Selection {
    let nothing = Selection {
        selected: None,
        observed_roster_seq: 0,
    };
    // THE INERT GUARD, stated here as well as inside `admit_roster`. This loop's shape is
    // "skip anything that fails", and that shape is one negated condition away from
    // "accept anything that is handed to us": if an unarmed anchor ever stopped refusing,
    // every candidate below would sail through. An unpinned build selects NOTHING — and
    // observes nothing either, so it cannot even ratchet a floor.
    if !anchor.is_armed() {
        return nothing;
    }

    // PASS 1 — THE ROSTER TIER, for every candidate. Master signature, parse-from-verified,
    // schema, the roster_seq ratchet, freshness. Nothing about any index is looked at until
    // every generation on offer has been weighed, because which generation is newest decides
    // WHO may have signed an index — and that question has to be settled before, not after,
    // ranking by a number the signer chose.
    let mut admitted: Vec<Admitted> = Vec::new();
    let mut observed_roster_seq = 0u64;
    for c in candidates {
        let Ok(roster) = admit_roster(anchor, c.roster_bytes, &c.roster_sig, now_unix) else {
            continue;
        };
        observed_roster_seq = observed_roster_seq.max(roster.seq());
        admitted.push(Admitted {
            label: c.label,
            index_bytes: c.index_bytes,
            sig: c.sig,
            roster,
        });
    }
    if admitted.is_empty() {
        return nothing;
    }

    // PASS 2 — only the newest generation gets to authorize anything. An older generation's
    // candidate is dropped HERE, before its index signature is checked, so a revoked
    // machine's mathematically valid index signature is never verified — the same ordering
    // rule the deny-list gets, applied one tier up.
    let mut best: Option<Selected> = None;
    for a in admitted {
        if a.roster.seq() != observed_roster_seq {
            continue;
        }
        // (2) THE INDEX — verified under the LIVE machines only (revoked and expired ones
        // left the set before any crypto), parsed only from the verified bytes, then bound
        // to the machine that actually signed it.
        let Ok((index, verified)) = a.roster.authorize_index(a.index_bytes, &a.sig) else {
            continue;
        };
        // (3) Anti-rollback on the SIGNED build (never the unsigned API number), scoped to
        // the generation that recorded the floor: a strictly newer generation re-bases it,
        // which is what keeps one machine's absurd `index_build` from bricking the store
        // beyond the master's reach. See `BuildFloor`.
        // The generation the index DECLARES, never the one it was served beside —
        // see `TrustedIndex::authorizing_seq`.
        if !floor.admits(index.authorizing_seq(), index.index_build) {
            continue;
        }
        // (4) Highest signed index_build wins, within the newest generation.
        if best
            .as_ref()
            .is_none_or(|b| index.index_build > b.index.index_build)
        {
            best = Some(Selected {
                label: a.label,
                index,
                verified,
            });
        }
    }
    Selection {
        selected: best,
        observed_roster_seq,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sig::testkit;

    /// A minimal but complete, valid index naming one program, at `build`, attributed to
    /// the test roster's machine.
    fn index_body(build: u64) -> String {
        format!(
            "schema = 2\nindex_build = {build}\nvalid_until = \"2030-07-05T12:00:00Z\"\n\
             machine_id = \"{id}\"\nroster_seq = {seq}\n\
             [programs.ay]\nrepo = \"ay\"\n",
            id = testkit::MACHINE_ID,
            seq = testkit::SEQ
        )
    }

    /// A candidate whose index is genuinely signed by the rostered machine, published
    /// beside the genuinely master-signed roster.
    fn signed(label: &str, build: u64) -> Candidate {
        let raw = index_body(build).into_bytes();
        let sig = testkit::sign(&testkit::MACHINE_SEED, &raw);
        let (roster_bytes, roster_sig) = testkit::published_roster();
        Candidate {
            label: label.into(),
            index_bytes: raw,
            sig,
            roster_bytes,
            roster_sig,
        }
    }

    /// A candidate carrying a (high) build but an INVALID index signature.
    fn unsigned(label: &str, build: u64) -> Candidate {
        let mut c = signed(label, build);
        c.sig = vec![0u8; 64]; // valid length, but not a real signature
        c
    }

    /// The anchor the tests select under.
    fn anchor(roster_floor: u64) -> Anchor {
        Anchor::of(
            vec![testkit::pubkey_b64(&testkit::MASTER_SEED)],
            roster_floor,
        )
    }

    /// A build floor of `build`, recorded under the fixture's own generation — i.e. one
    /// that actually BINDS, which is what a floor test wants. A floor carrying a different
    /// generation would be silently waived and every floor assertion below would pass
    /// vacuously.
    fn floor(build: u64) -> BuildFloor {
        BuildFloor {
            index_build: build,
            roster_seq: testkit::SEQ,
        }
    }

    /// Nothing recorded — first contact.
    fn no_floor() -> BuildFloor {
        BuildFloor::none()
    }

    // THE DANGEROUS DIRECTION. An unarmed anchor selects NOTHING, even from a candidate
    // set every byte of which is genuinely signed. Delete the `is_armed` guard and this
    // fails on the first assertion — `admit_roster` still refuses, so it fails there
    // instead, which is the belt-and-braces the double statement buys.
    #[test]
    fn an_unarmed_anchor_selects_nothing() {
        let cands = vec![signed("v60", 60)];
        let out = select_index(
            &Anchor::of(vec![], 0),
            cands.clone(),
            no_floor(),
            testkit::NOW,
        );
        assert!(
            out.selected.is_none(),
            "an unpinned build must install nothing, not everything"
        );
        assert_eq!(
            out.observed_roster_seq, 0,
            "an unpinned build observes nothing either — it must not ratchet a floor it \
             cannot have verified"
        );
        // NON-VACUITY: the identical candidate set is selected the moment a master is
        // pinned, so the refusal above is the anchor and not a broken fixture.
        assert!(
            select_index(&anchor(0), cands, no_floor(), testkit::NOW)
                .selected
                .is_some()
        );
    }

    // The highest SIGNED index_build wins; an unsigned higher-build release is skipped and
    // can never suppress it (§5 DoS-resistance).
    #[test]
    fn picks_highest_signed_skips_unsigned() {
        let cands = vec![
            signed("v50", 50),
            unsigned("v999-attack", 999), // huge claimed build, bad signature
            signed("v60", 60),
            signed("v55", 55),
        ];
        let out = select_index(&anchor(0), cands, floor(40), testkit::NOW);
        let sel = out.selected.expect("a signed index wins");
        assert_eq!(sel.label, "v60");
        assert_eq!(sel.index.index_build, 60);
        assert_eq!(sel.index.attribution().machine_id, testkit::MACHINE_ID);
        assert_eq!(
            out.observed_roster_seq,
            testkit::SEQ,
            "the pass reports the generation it admitted, for the durable ratchet"
        );
    }

    // A signed index BELOW the floor is filtered out (anti-rollback); the winner is the
    // highest signed build at-or-above the floor.
    #[test]
    fn floor_filters_below_high_water() {
        let cands = vec![signed("v30", 30), signed("v45", 45), signed("v42", 42)];
        // Floor 44 ⇒ only v45 qualifies.
        let sel = select_index(&anchor(0), cands.clone(), floor(44), testkit::NOW)
            .selected
            .expect("v45 qualifies");
        assert_eq!(sel.index.index_build, 45);
        // Floor 100 ⇒ nothing qualifies.
        assert!(
            select_index(&anchor(0), cands, floor(100), testkit::NOW)
                .selected
                .is_none()
        );
    }

    // A DIFFERENT paper master ⇒ every candidate's roster fails to verify ⇒ None. Fail
    // closed, with no fallback to any other root — there is no other root.
    #[test]
    fn a_different_master_selects_nothing() {
        let other = Anchor::of(vec![testkit::pubkey_b64(&testkit::OUTSIDER_SEED)], 0);
        let out = select_index(&other, vec![signed("v60", 60)], no_floor(), testkit::NOW);
        assert!(out.selected.is_none());
        assert_eq!(
            out.observed_roster_seq, 0,
            "a roster that did not verify was not observed, and must not ratchet anything"
        );
    }

    // A candidate whose roster is MISSING (or empty) is skipped: no roster, no authority.
    // The index bytes and their signature are untouched and perfectly genuine.
    #[test]
    fn a_candidate_with_no_roster_is_skipped() {
        let mut c = signed("v60", 60);
        c.roster_bytes = vec![];
        c.roster_sig = vec![];
        assert!(
            select_index(&anchor(0), vec![c], no_floor(), testkit::NOW)
                .selected
                .is_none()
        );
        // ...and the one beside it, with its roster intact, still wins — a suppressed
        // roster takes down its own candidate, not the whole selection.
        let mut broken = signed("v70", 70);
        broken.roster_sig = vec![0u8; 64];
        let sel = select_index(
            &anchor(0),
            vec![broken, signed("v60", 60)],
            no_floor(),
            testkit::NOW,
        )
        .selected
        .expect("the intact candidate still qualifies");
        assert_eq!(
            sel.index.index_build, 60,
            "the higher build had no authority"
        );
    }

    // A REPLAYED roster generation is refused by the durable roster floor, even though the
    // index it authorizes is fresh and genuinely signed.
    #[test]
    fn a_rolled_back_roster_generation_selects_nothing() {
        let out = select_index(
            &anchor(testkit::SEQ + 1),
            vec![signed("v60", 60)],
            no_floor(),
            testkit::NOW,
        );
        assert!(
            out.selected.is_none(),
            "a client that has durably seen a newer generation refuses this one forever"
        );
        assert_eq!(
            out.observed_roster_seq, 0,
            "a generation the ratchet refused was never admitted, so it is not an observation"
        );
        // Equal is allowed, so re-fetching the current generation is not read as an attack.
        assert!(
            select_index(
                &anchor(testkit::SEQ),
                vec![signed("v60", 60)],
                no_floor(),
                testkit::NOW
            )
            .selected
            .is_some()
        );
    }

    // A STALE roster (its own window lapsed) selects nothing — the only defence a
    // brand-new install has, since it carries no floor.
    #[test]
    fn a_stale_roster_selects_nothing() {
        // 2103 — past the fixture roster's 2099 valid_until, and chosen explicitly so
        // this test says which gate it is exercising rather than inheriting a deadline.
        let far_future = 4_200_000_000_i64;
        assert!(
            select_index(&anchor(0), vec![signed("v60", 60)], no_floor(), far_future)
                .selected
                .is_none()
        );
    }

    // Empty candidate list ⇒ None.
    #[test]
    fn no_candidates_selects_nothing() {
        let out = select_index(&anchor(0), vec![], no_floor(), testkit::NOW);
        assert!(out.selected.is_none());
        assert_eq!(out.observed_roster_seq, 0);
    }

    // An equal-build tie does not regress below floor; equal to floor is allowed (>=).
    #[test]
    fn equal_to_floor_is_allowed() {
        let sel = select_index(&anchor(0), vec![signed("v41", 41)], floor(41), testkit::NOW)
            .selected
            .expect("equal passes");
        assert_eq!(sel.index.index_build, 41);
    }

    // ---------------------------------------------------------------------------------
    // THE NEWEST ADMITTED GENERATION DECIDES.
    // ---------------------------------------------------------------------------------

    /// A candidate on a generation OLDER than the newest this pass admitted is discarded,
    /// however high its signed `index_build` — because "who may sign an index" is settled by
    /// the roster tier before any index number is compared.
    ///
    /// The fixture cannot use `signed()` (one fixed generation), so it builds the two
    /// generations explicitly; `sig`'s own tests cover the revoked-machine case with real
    /// deny-lists, and `atpkg-keys`' owner→client suite drives the whole thing end to end.
    ///
    /// MUTATION: delete the `a.roster.seq() != observed_roster_seq` guard and this fails
    /// with the higher build winning.
    #[test]
    fn an_older_generation_loses_however_high_its_build() {
        use aterm_update_core::roster::Roster;

        // Generation SEQ+1, same single machine — a plain bump, so the ONLY difference
        // between the two candidates is which generation authorized them.
        let mut newer: Roster = testkit::roster();
        newer.roster_seq = testkit::SEQ + 1;
        let newer_bytes = newer.to_toml().expect("a valid roster emits").into_bytes();
        let newer_sig = testkit::sign(&testkit::MASTER_SEED, &newer_bytes);

        let body = format!(
            "schema = 2\nindex_build = 5\nvalid_until = \"2030-07-05T12:00:00Z\"\n\
             machine_id = \"{id}\"\nroster_seq = {seq}\n[programs.ay]\nrepo = \"ay\"\n",
            id = testkit::MACHINE_ID,
            seq = testkit::SEQ + 1
        )
        .into_bytes();
        let on_newer = Candidate {
            label: "newest-generation".into(),
            sig: testkit::sign(&testkit::MACHINE_SEED, &body),
            index_bytes: body,
            roster_bytes: newer_bytes,
            roster_sig: newer_sig,
        };
        // ...and a candidate on the OLD generation with a far higher build.
        let on_older = signed("old-generation-big-build", 9_000);

        let out = select_index(
            &anchor(0),
            vec![on_older, on_newer],
            no_floor(),
            testkit::NOW,
        );
        let sel = out
            .selected
            .expect("the newest generation has a valid index");
        assert_eq!(
            sel.label, "newest-generation",
            "index_build must not outrank the generation that authorizes the signer"
        );
        assert_eq!(sel.index.index_build, 5);
        assert_eq!(out.observed_roster_seq, testkit::SEQ + 1);

        // NON-VACUITY: without the newer generation present, the big-build candidate wins —
        // so the loss above is the generation ordering and not a broken fixture.
        let alone = select_index(
            &anchor(0),
            vec![signed("old-generation-big-build", 9_000)],
            no_floor(),
            testkit::NOW,
        )
        .selected
        .expect("it is perfectly valid on its own");
        assert_eq!(alone.index.index_build, 9_000);
    }

    /// A build floor recorded under an OLDER generation does not bind: the paper master can
    /// always rescue a store whose floor a machine drove out of reach.
    ///
    /// MUTATION: change `BuildFloor::admits`' `roster_seq > self.roster_seq` to `>=` (or
    /// drop the branch) and this fails — the rescue index is filtered out.
    #[test]
    fn a_floor_from_an_older_generation_is_rebased_not_obeyed() {
        // A floor at the u64 ceiling, recorded one generation back.
        let poisoned = BuildFloor {
            index_build: u64::MAX,
            roster_seq: testkit::SEQ - 1,
        };
        let sel = select_index(
            &anchor(0),
            vec![signed("rescue", 41)],
            poisoned,
            testkit::NOW,
        )
        .selected
        .expect("a newer generation re-bases the floor");
        assert_eq!(sel.index.index_build, 41);

        // NON-VACUITY, and the fail-closed direction: the SAME absurd floor recorded under
        // THIS generation still binds, so the waiver is strictly about being newer.
        let same_generation = BuildFloor {
            index_build: u64::MAX,
            roster_seq: testkit::SEQ,
        };
        assert!(
            select_index(
                &anchor(0),
                vec![signed("rescue", 41)],
                same_generation,
                testkit::NOW
            )
            .selected
            .is_none(),
            "within one generation the ratchet is exactly as monotonic as it was"
        );
    }
}
