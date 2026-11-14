// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Each law of `SharedFile` with the one mutant that breaks it, in the order of the design's
/// table (docs/DESIGN-shared-files-2026-11-11.md §8): the mutant at index `i` is `Buggy == i + 1`.
pub const SHARED_FILE_LAWS: [(&str, &str); 16] = [
    ("ReadTotal", "ReadOpensCustody"),
    ("SnapshotMonotone", "RefreshTakesOlder"),
    ("HeldNeverLive", "HeldReadAsLive"),
    ("IntentDurable", "DropOnRefusal"),
    ("PublishOnce", "PublishTwice"),
    ("PublishObservedOnly", "PublishOverMoved"),
    ("FencedNeverSigns", "FencedSigns"),
    ("OwnKeyOnly", "ForeignRowVisible"),
    ("OwnerOverAutomatic", "AutoWritesOwner"),
    ("OwnerOverBirth", "BirthBeatsOwner"),
    ("FixedIdentity", "RewriteFixed"),
    ("CommitIsExplicit", "WatcherPublishes"),
    ("AbsenceRetiresNothing", "MissingRowRetires"),
    ("LinkNeverLimits", "LinkLimitsPool"),
    ("LimitsByReset", "ClockOrdersReadings"),
    ("NoSecretMaterialized", "SecretInSnapshot"),
];

/// One shared file on one Mac, with a peer Mac writing beside it
/// (docs/DESIGN-shared-files-2026-11-11.md §1 to §5, §8). One family of laws to a walk: the
/// first step chooses the area (`area`), and only that area's actions run.
///
/// Custody is this Mac's standing at its Commons home (`custody`: 0 live, 1 preserving, its
/// writer fenced and its succession unfinished, 2 unsettled, a completed succession invalidated,
/// 3 refused for membership, a busy vault, policy or keys), and `era` the generation of the home
/// a pass opens now (0 the original, 1 its admitted successor's). A succession completes
/// (`Admit`), and a completion can be invalidated (`Invalidate`): a pass then opens the
/// original home again, as unsettled, until the succession completes once more.
///
/// 1. **Reading** (`aterm_file::File::read`, `aterm_db::file::materialize`). The replica's
///    frontier at the home a pass opens now is `rfr`, and at the lineage's other home `ofr`. The
///    snapshot holds the home and frontier it was projected from (`sera`, `sfr`); the state word
///    (`word`: 0 never, 1 live, 2 held) is the last recorded pass's, with the custody that pass
///    met (`src`). `hera` and `hfr` are the newest home and frontier any snapshot here was
///    ever written from. A pass that opened custody replaces the snapshot only with a later home
///    or a frontier that contains the old one, and records its word either way. A pass custody
///    refused (`custody` 3) opened nothing: it leaves the snapshot as it was and records its
///    word all the same, so it is a recorded pass like any other. The runtime's pass looks and
///    records in one step (`Materialize`); `aterm file refresh` in another process takes a view
///    (`RefreshLook`: `vera`, `vfr`, `vsrc`, `vlacks`) and records it later (`RefreshWrite`). A
///    view taken before a pass that has recorded since (`look` 2) is older than that pass and
///    records nothing: without that rule a view of live custody, taken before a fence or before
///    custody refused, would turn a held word live once the later pass had run. `older` is such
///    a view recorded. A read (`Read`) has no custody in its guard: `answered` of `reads`
///    answered, `opens` the custody steps a read took. A record holding a credential-shaped
///    value can reach the replica (`PeerSecret`, `rsecret`); `ssecret` is one in the snapshot or
///    the face. `authored` is a record or receipt written from a materializer's open.
///
///    **The unsettled open.** While its succession was complete the successor could author
///    and take peers' records (`Grow` at `era` 1), none of which its fenced original holds.
///    So an unsettled pass opens a home that can lack its own lineage's newest records: there
///    `ofr`, the successor's frontier, counts what the successor added to the prefix it
///    imported. `lacks` is that fact about the snapshot: it was projected from a home that
///    lacked a record its successor held when it was read. That is what a read of an original
///    home after an invalidated completion can mislead, and the laws bound it. HeldNeverLive:
///    a snapshot is under a live word only where the pass that left the word opened the newest
///    home and the snapshot lacks nothing (`src` 0, `lacks` 0), and no open authors.
///    SnapshotMonotone: the unsettled home is the earlier one, so its view never replaces a
///    snapshot of the successor's home, and where no pass read the successor it contains the
///    frontier the last snapshot of that home had; the completion made again, the successor's
///    home replaces it and the word is live once more.
/// 2. **Writing** (`aterm_file::File::write`, `aterm_db::file::publish`,
///    `aterm_db_core::file_merge::settle`). Of `acc` accepted intents, `q` wait in the outbox,
///    `h` are held and `p` published. The intent at the head of the outbox names one register,
///    whose candidates were the ones the intent observed until the peer wrote (`cb`, a
///    candidate the intent never observed) or this Mac did (`mine`, its record a candidate
///    still); `recs` is the records in the live home's history that carry the head intent's
///    row. The publisher signs only with live custody, only while the register is as observed,
///    and only an intent no record carries (`Sign`). A crash there leaves the intent queued
///    beside its record: an intent whose record is in history costs nothing, whatever the
///    register holds by then, and leaves the outbox behind its done marker (`Done`). So a peer
///    that replaces this Mac's record (`PeerReplaces`) leaves the intent published, never
///    held. A succession imports only the witnessed prefix: a record signed before the fence
///    and still awaiting its witness is in no successor's home (`AdmitUnwitnessed`), so the
///    successor signs the intent again and it still has one record. An intent no record
///    carries, on a register that moved, is held (`Hold`). `lost` records a candidate replaced
///    unobserved, `fsig` a record signed by a fenced actor.
/// 3. **Authority on merge** (`aterm_db::file::admit`,
///    `aterm_db_core::file_merge::eligible_owned`). The mine row visible at this Mac's key and
///    the log entry visible under its uuid are signed by `rowby` and `logby` (0 none, 1 this
///    Mac, 2 the peer); the log snapshot keeps `logrows` of the `logged` entries. On an Owner
///    column `proj` is what projects (0 nothing, 1 the owner's cell, 2 an automatic cell). On a
///    Birth-or-Owner column `birth` and `own` are the two candidates and `bshown` the Birth
///    cell showing. `ident` is the provider and realm (0 unborn, 1 as born, 2 rewritten).
/// 4. **The face.** `face` is the face against what the materializer last wrote (0 the same, 1 a
///    cell edited, 2 a row missing); a pass that sees it changed (`Watch`) queues nothing, a
///    `Commit` turns differing cells into `intents`, and `Restore` rewrites it. The row is
///    retired (`rowlive` 0) only when asked (`asked`, `Remove`).
/// 5. **Rotation's reads.** This Mac holds a login for an account or not (`held`), and a link
///    names the account for it (`link`: 0 none, 1 live, 2 ended); `inpool` is the pool once
///    `built`. Each Mac reads one window as a reset and a percent (`ra`, `pa`; `rb`, `pb`), and
///    the fold is run in two worlds that differ only in which Mac's clock read last (`fr1`,
///    `fp1`: this Mac's; `fr2`, `fp2`: the peer's).
///
/// The bounds: the outbox within its cap (`Cap` stands for 1,024 intents) and a log snapshot
/// within its keep (`Keep`).
///
/// Mutants, as [`SHARED_FILE_LAWS`] names them: 1 a read opens custody, and answers only where
/// that succeeds, 2 a pass that opened custody writes the view it took over a newer snapshot,
/// and any pass its word over a later pass's, 3 a pass that opened a fenced or unsettled home
/// is taken for a live one, 4 an intent whose
/// register moved is dropped, 5 a retry signs an intent a record already carries, 6 the
/// publisher writes over a register that moved, 7 the publisher signs as a fenced actor, 8 a
/// row or an entry at another Mac's key projects, 9 an automatic cell projects on an Owner
/// column, 10 a Birth cell shows beside an Owner candidate, 11 a write after birth changes the
/// provider or realm, 12 a watcher publishes a file write, 13 a row missing from a committed
/// face is retired, 14 the pool is the linked accounts only, 15 the fold takes the reading
/// whose clock read last, 16 a credential-shaped value is projected.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn shared_file_model() -> Model {
    aterm_spec::ty_model! {
        SharedFile {
            const Buggy = 0;
            const Cap = 2;
            const Keep = 2;
            var area = 0;
            var custody = 0; var era = 0;
            var rfr = 0; var ofr = 0; var sera = 0; var sfr = 0; var word = 0; var src = 0;
            var hera = 0; var hfr = 0; var older = 0; var lacks = 0;
            var look = 0; var vera = 0; var vfr = 0; var vsrc = 0; var vlacks = 0;
            var reads = 0; var answered = 0; var opens = 0; var authored = 0;
            var rsecret = 0; var ssecret = 0;
            var acc = 0; var q = 0; var h = 0; var p = 0;
            var cb = 0; var mine = 0; var recs = 0; var lost = 0; var fsig = 0;
            var rowby = 0; var logby = 0; var logged = 0; var logrows = 0;
            var proj = 0; var oc = 0; var birth = 0; var own = 0; var bshown = 0; var ident = 0;
            var face = 0; var commits = 0; var intents = 0; var rowlive = 1; var asked = 0;
            var held = 0; var link = 0; var built = 0; var inpool = 0;
            var ra = 0; var pa = 0; var rb = 0; var pb = 0; var folded = 0;
            var fr1 = 0; var fp1 = 0; var fr2 = 0; var fp2 = 0;

            action ChooseRead when (area == 0) { area = 1; }
            action ChooseWrite when (area == 0) { area = 2; }
            action ChooseAuthority when (area == 0) { area = 3; }
            action ChooseFace when (area == 0) { area = 4; }
            action ChooseRotation when (area == 0) { area = 5; }

            // Custody, for the reading and the writing walks: one succession, completed with
            // every record of the fenced home or without its unwitnessed one, and a completion
            // invalidated and made again. A successor's frontier starts again, and each home
            // keeps its own.
            action Fence when (area > 0 && area <= 2 && custody == 0 && era == 0) { custody = 1; }
            action Admit when (area > 0 && area <= 2 && custody > 0 && custody <= 2) {
                custody = 0; era = 1;
                rfr = if custody == 1 {0} else {ofr};
                ofr = rfr;
            }
            action AdmitUnwitnessed when (area == 2 && custody == 1 && mine == 1) {
                custody = 0; era = 1; mine = 0; recs = 0;
            }
            action Invalidate when (area == 1 && custody == 0 && era == 1) {
                custody = 2; era = 0; rfr = ofr; ofr = rfr;
            }
            action Refuse when (area == 1 && custody == 0) { custody = 3; }
            action Recover when (area == 1 && custody == 3) { custody = 0; }

            // 1. Reading. Only the home orders two snapshots of different homes. A pass that
            // records makes a view taken before it the older one, and a pass custody refused
            // records too. At an unsettled home `ofr` is what the successor added.
            action Grow when (area == 1 && custody == 0 && 1 > rfr) { rfr = rfr + 1; }
            action PeerSecret when (area == 1 && rsecret == 0) { rsecret = 1; }
            action Materialize when (area == 1 && custody <= 3) {
                sera = if custody <= 2
                    && (era > sera || (era == sera && sfr <= rfr) || Buggy == 2) {era}
                    else {sera};
                sfr = if custody <= 2
                    && (era > sera || (era == sera && sfr <= rfr) || Buggy == 2) {rfr}
                    else {sfr};
                lacks = if custody <= 2
                    && (era > sera || (era == sera && sfr <= rfr) || Buggy == 2) {
                    if custody == 2 && ofr > 0 {1} else {0}
                } else {lacks};
                ssecret = if custody <= 2
                    && (era > sera || (era == sera && sfr <= rfr) || Buggy == 2) {
                    if Buggy == 16 {rsecret} else {0}
                } else {ssecret};
                hera = if custody <= 2 && era > hera {era} else {hera};
                hfr = if custody <= 2 && era > hera {rfr}
                    else {if custody <= 2 && era == hera && rfr > hfr {rfr} else {hfr}};
                word = if custody == 0 || (Buggy == 3 && custody <= 2) {1} else {2};
                src = custody;
                authored = if custody > 0 && custody <= 2 && Buggy == 3 {1} else {authored};
                look = if look == 1 {2} else {look};
            }
            action RefreshLook when (area == 1 && custody <= 3 && look == 0) {
                look = 1; vsrc = custody;
                vera = if custody <= 2 {era} else {0};
                vfr = if custody <= 2 {rfr} else {0};
                vlacks = if custody == 2 && ofr > 0 {1} else {0};
            }
            action RefreshWrite when (area == 1 && look > 0) {
                look = 0; vera = 0; vfr = 0; vsrc = 0; vlacks = 0;
                sera = if vsrc <= 2
                    && ((look == 1 && (vera > sera || (vera == sera && sfr <= vfr)))
                        || Buggy == 2) {vera} else {sera};
                sfr = if vsrc <= 2
                    && ((look == 1 && (vera > sera || (vera == sera && sfr <= vfr)))
                        || Buggy == 2) {vfr} else {sfr};
                lacks = if vsrc <= 2
                    && ((look == 1 && (vera > sera || (vera == sera && sfr <= vfr)))
                        || Buggy == 2) {vlacks} else {lacks};
                ssecret = if vsrc <= 2
                    && ((look == 1 && (vera > sera || (vera == sera && sfr <= vfr)))
                        || Buggy == 2) {if Buggy == 16 {rsecret} else {0}} else {ssecret};
                hera = if vsrc <= 2 && (look == 1 || Buggy == 2) && vera > hera {vera}
                    else {hera};
                hfr = if vsrc <= 2 && (look == 1 || Buggy == 2) && vera > hera {vfr}
                    else {if vsrc <= 2 && (look == 1 || Buggy == 2) && vera == hera
                        && vfr > hfr {vfr} else {hfr}};
                word = if look == 1 || Buggy == 2 {
                    if vsrc == 0 || (Buggy == 3 && vsrc <= 2) {1} else {2}
                } else {word};
                src = if look == 1 || Buggy == 2 {vsrc} else {src};
                authored = if vsrc > 0 && vsrc <= 2 && Buggy == 3 {1} else {authored};
                older = if look == 2 && Buggy == 2 {1} else {older};
            }
            action Read when (area == 1 && reads == 0) {
                reads = 1;
                answered = if Buggy == 1 && custody > 0 {0} else {1};
                opens = if Buggy == 1 {1} else {0};
            }

            // 2. Writing. An append needs no custody; a full outbox refuses the new write.
            action Accept when (area == 2 && 3 > acc && Cap > q) { acc = acc + 1; q = q + 1; }
            action PeerWrite when (area == 2 && q > 0 && cb == 0) { cb = 1; }
            action PeerReplaces when (area == 2 && q > 0 && mine == 1) { mine = 0; cb = 1; }
            action Sign when (area == 2 && q > 0 && 2 > recs
                && (custody == 0 || Buggy == 7) && (cb == 0 || Buggy == 6)
                && (recs == 0 || Buggy == 5)) {
                mine = 1; recs = recs + 1; cb = 0;
                lost = if cb == 1 {1} else {lost};
                fsig = if custody > 0 {1} else {fsig};
            }
            action Done when (area == 2 && q > 0 && recs > 0 && (custody == 0 || Buggy == 7)) {
                q = q - 1; p = p + 1; mine = 0; cb = 0; recs = 0;
            }
            action Hold when (area == 2 && q > 0 && recs == 0 && cb == 1
                && (custody == 0 || Buggy == 7)) {
                q = q - 1; cb = 0; h = if Buggy == 4 {h} else {h + 1};
            }

            // 3. Authority on merge: a row and an entry at this Mac's key, signed by it or by the
            // peer; an automatic cell on an Owner column; a Birth cell and an Owner candidate; a
            // write to a fixed column after birth.
            action OwnRow when (area == 3 && rowby == 0) { rowby = 1; }
            action ForeignRow when (area == 3) { rowby = if Buggy == 8 {2} else {rowby}; }
            action OwnEntry when (area == 3 && 3 > logged) {
                logged = logged + 1; logby = if logby == 2 {2} else {1};
                logrows = if Keep > logrows {logrows + 1} else {logrows};
            }
            action ForeignEntry when (area == 3) { logby = if Buggy == 8 {2} else {logby}; }
            action OwnerWrites when (area == 3 && oc == 0) { oc = 1; proj = 1; }
            action AutomaticWrites when (area == 3) { proj = if Buggy == 9 {2} else {proj}; }
            action Birth when (area == 3 && birth == 0) {
                birth = 1; ident = 1;
                bshown = if own == 1 {if Buggy == 10 {1} else {0}} else {1};
            }
            action OwnerSets when (area == 3 && own == 0) {
                own = 1; bshown = if Buggy == 10 {bshown} else {0};
            }
            action Rewrite when (area == 3 && birth == 1) {
                ident = if Buggy == 11 {2} else {ident};
            }

            // 4. The face: edits, a pass that sees them, a commit by name, a restore, a removal.
            action EditCell when (area == 4 && face == 0 && 2 > commits) { face = 1; }
            action LoseRow when (area == 4 && face == 0 && 2 > commits) { face = 2; }
            action Watch when (area == 4 && face > 0 && 2 > intents) {
                intents = if Buggy == 12 {intents + 1} else {intents};
                face = if Buggy == 12 {0} else {face};
            }
            action Commit when (area == 4 && face > 0) {
                commits = commits + 1; face = 0;
                intents = if face == 1 || Buggy == 13 {intents + 1} else {intents};
                rowlive = if face == 2 && Buggy == 13 {0} else {rowlive};
            }
            action Restore when (area == 4 && face > 0) { face = 0; }
            action Remove when (area == 4 && rowlive == 1 && asked == 0) { asked = 1; rowlive = 0; }

            // 5. Rotation's reads: the pool, and one window read on two Macs (this Mac an old
            // week at the larger percent or a new week at the smaller; the peer the reverse).
            action SignIn when (area == 5 && held == 0) { held = 1; built = 0; inpool = 0; }
            action Link when (area == 5 && link == 0) { link = 1; built = 0; inpool = 0; }
            action EndLink when (area == 5 && link == 1) { link = 2; built = 0; inpool = 0; }
            action Pool when (area == 5) {
                built = 1;
                inpool = if link == 1 {1}
                    else {if held == 1 {if Buggy == 14 {0} else {1}} else {0}};
            }
            action HereOldWeek when (area == 5 && ra == 0) { ra = 1; pa = 2; }
            action HereNewWeek when (area == 5 && ra == 0) { ra = 2; pa = 1; }
            action PeerOldWeek when (area == 5 && rb == 0) { rb = 1; pb = 1; }
            action PeerNewWeek when (area == 5 && rb == 0) { rb = 2; pb = 2; }
            action Fold when (area == 5 && ra > 0 && rb > 0 && folded == 0) {
                folded = 1;
                fr1 = if Buggy == 15 {ra} else {if ra > rb {ra} else {rb}};
                fp1 = if Buggy == 15 {pa} else {if ra > rb {pa}
                    else {if rb > ra {pb} else {if pa > pb {pa} else {pb}}}};
                fr2 = if Buggy == 15 {rb} else {if ra > rb {ra} else {rb}};
                fp2 = if Buggy == 15 {pb} else {if ra > rb {pa}
                    else {if rb > ra {pb} else {if pa > pb {pa} else {pb}}}};
            }

            invariant ReadTotal: answered == reads && opens == 0;
            invariant SnapshotMonotone: hera <= sera && hfr <= sfr && older == 0;
            invariant HeldNeverLive: (word == 0 || word == 2 || (src == 0 && lacks == 0))
                && authored == 0;
            invariant IntentDurable: acc == q + h + p;
            invariant PublishOnce: recs <= 1;
            invariant PublishObservedOnly: lost == 0;
            invariant FencedNeverSigns: fsig == 0;
            invariant OwnKeyOnly: rowby <= 1 && logby <= 1;
            invariant OwnerOverAutomatic: proj <= 1;
            invariant OwnerOverBirth: own == 0 || bshown == 0;
            invariant FixedIdentity: ident <= 1;
            invariant CommitIsExplicit: intents <= commits;
            invariant AbsenceRetiresNothing: rowlive == 1 || asked == 1;
            invariant LinkNeverLimits: built == 0 || held == 0 || inpool == 1;
            invariant LimitsByReset: fr1 == fr2 && fp1 == fp2;
            invariant NoSecretMaterialized: ssecret == 0;
            bound OutboxWithinCap: q <= Cap;
            bound LogWithinKeep: logrows <= Keep;
        }
    }
}

#[cfg(test)]
#[path = "shared_file_tests.rs"]
mod tests;
