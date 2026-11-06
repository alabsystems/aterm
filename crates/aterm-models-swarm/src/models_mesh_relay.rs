// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Formal model of the mesh relay: each Mac appends each record once, and every up Mac gets every
//! table record (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.6, §9 `MeshRelay` and §11 B8).

use aterm_spec::derive::*;

/// `MeshRelay`: Macs A, B and C, whose neighbour edges form a cycle (with fewer than six members
/// every member is a neighbour, so three Macs are a triangle), and D, which may arrive. Every
/// record in the model is C's: one table record `r` on `tables.log`, and two rows of C's presence
/// subject on `metrics.log`, `p1` before a rebuild and `p3` after it (`RelayBox`'s three records
/// per origin).
///
/// A relay (`RelayXY`, the edge from X to Y) moves the next record of X's log past its cursor:
/// the neighbour appends it, answers it "already held", or refuses it. A Mac's log holds `r` as
/// copies (`ha`, `hb`, `hc`, `hd`: the copies appended, which only a per-edge dedup makes more
/// than one); a Mac's store holds it once its ingest consumer applied it or an `Inventory`
/// brought it (`sa`, `sb`, `sd`; C's store holds `r` from its publish). A cut drops `r` from a
/// log (`ka`, `kc`: still retained) and never from a store.
///
/// The first `Publish`, `Split`, `Remove`, `Readmit` or `PublishRow` picks one of four shapes
/// (`sc`); `AwayB` may come before it, and then the gap or the presence shape follows:
///
/// - **Cycle** (1): C publishes `r` with every Mac up, and the relay floods it round the
///   triangle. C may `Split` off (both its links down, `cs`), publish while split, go `AwayC`
///   while split (`cw`, its node service stopped) and `ReturnC`; `Join` heals the split. D may
///   `Arrive` at any point: a new Mac with empty stores, linked to A in the model (in the lab it
///   links to every Mac, and the projection reads A's edge and D's log and store). A's relay to
///   D has no cursor, so it starts at A's head and passes what lies before it with a new-link
///   notice (`nad`).
/// - **Gap** (2): B is away (`bw`) when C publishes; `r` reaches A; C's and A's logs may be cut
///   (`CutC`, `CutA`) below B's cursors, and B's cursors may `Expire` (dropped after 7 days).
///   When B returns, a relay whose record was cut reads a gap, and one whose cursor was dropped
///   starts at its head; either passes what it cannot deliver with a notice (`ncb`, `nab`) that
///   asks an `Inventory` of the pair (`InventoryCB`, `InventoryAB`), which moves `r` into B's
///   store.
/// - **Members** (3): the membership log changes first, on A: a person's `Remove` of C, a
///   key-kept `Readmit` of C (a `Remove` of C's old seat and an `Admit` of a new seat binding
///   the same key, which the log never reads as removing the key), or a rogue's `Remove` that a
///   quorum later voids (`Void`). The change reaches B by `Learn` (`lb`: the last change B
///   applied). `va` and `vb` are A's and B's logs reading C removed; `xa` and `xb` are A's and
///   B's brokers revoking C's key. A broker takes a record of C only while C is an origin there;
///   a revoked key's stream is refused, so C's own relay to that Mac does not run. A record a
///   neighbour refused is held by the relay while the relay's own Mac seats C, and let go once
///   it does not. A `Forge` offers A a record under C's subject that C's key did not sign: A
///   refuses it for good while it seats C (the signature), and leaves it unjudged while it does
///   not.
/// - **Presence** (4): C publishes `p1` (`cp`), is rebuilt with its node key kept once A holds
///   `p1` (`Rebuild`: its log gone, its relays without a cursor, its sequence numbers above the
///   high water its first neighbour holds of it), and publishes `p3`. B may be away while this
///   happens. `ga` and `gb` are the highest row A and B hold of C's subject (0 none, 1 `p1`, 3
///   `p3`), `na` and `nb` the newest row each serves, and `a1` whether A's log holds `p1` (C's
///   rows reach A from C alone: A holds every row of C's that B could relay to it). `qca` and
///   `qcb` are C's relays' progress (1 past `p1`, 2 past `p3`) and `qab` how many of A's rows
///   A's relay to B has passed, in the order A's log holds them. A row at or below the one a
///   subject holds appends nothing (astream's monotone `Last`), so which of `p1` and `p3` B's
///   log holds depends on the order they arrive; the highest it holds does not.
///
/// The edges' cursors (`eca` C to A, `ecb`, `eab`, `eba`, `eac`, `ebc`, `ead`) count the copies
/// of `r` each has passed in its source's log. The relay never sleeps and never polls, and the
/// model's `Relay` steps are its bursts; a conformance run drives each other action and lets the
/// relays and `Inventory` run until nothing more moves ([`SETTLES`]), and that state is the
/// model's unique state with none of them enabled.
///
/// The laws:
///
/// - `AppendedOnce`: no Mac holds two copies of one key (`TlaFlood.appended_once`);
/// - `OnlyCurrentOrigins`: no Mac appends a record of an origin its log reads as removed, nor a
///   record whose signature does not verify (`bad`; `TlaFlood.only_checked_origins` and
///   `no_forged_sender`);
/// - `NewestIsHighest`: each newest row is the highest sequence held for its subject
///   (`TlaFlood.newest_is_highest`);
/// - `NoCurrentSeatRevoked`: no broker revokes the key of a seat its own log reads as current,
///   after a `Readmit` or a `Void` too (`relay_island.rs`'s `no_origin_revoked`);
/// - `NoSkip`: an edge into a Mac that came back (B) or arrived (D) has passed `r` only where
///   that Mac's store holds it or a notice asks the pair's `Inventory` (astream's
///   `TlaPushEdge.NoSkip`, lifted from the edge to the store: every record below where a relay
///   resumes was answered or told);
/// - `SeqFresh`: no Mac answers a row of C's new life "already held" (`dp`): a rebuilt Mac's
///   sequence numbers never collide with a key its neighbours hold (`TlaSeqFresh.seq_fresh`);
/// - `bound RelayBox`;
/// - liveness [`every_up_mac_gets_every_table_record_liveness`] and
///   [`restarted_mac_reads_up_liveness`].
///
/// The mutants (the `Buggy` dial; the design numbers them 1 to 6):
///
/// - dial 1 enables two actions dead with no mutant on, the design's mutants 3 and 4:
///   `GapSkip`, a relay that passes a gap with no notice, so no `Inventory` follows a cut and a
///   return (the code's `NO_INVENTORY_ON_GAP`), which breaks `NoSkip` and
///   `EveryUpMacGetsEveryTableRecord`; and `RebuildReuse`, a rebuild that reuses its last
///   incarnation (the code's `REUSED_INCARNATION`), whose new row arrives as a duplicate of an
///   old key, which breaks `SeqFresh` and `RestartedMacReadsUp`;
/// - dial 2, the design's mutant 1: dedup keyed per edge, as the bridge before the relay
///   (`B/federation.rs:652`): each edge appends a fresh copy, so `r` goes round the cycle and
///   `AppendedOnce` breaks;
/// - dial 3, mutant 2: the newest row follows arrival: `p3` reaches B from C first after a
///   rebuild and `p1` from A later, and `NewestIsHighest` breaks;
/// - dial 4, mutant 5: no origin check: B's relay delivers C's record to A after A applied C's
///   removal, or A appends the forgery, and `OnlyCurrentOrigins` breaks;
/// - dial 5, mutant 6: a revocation made on the `Remove` event and never lifted: the `Remove`
///   inside a `Readmit`, or a voided `Remove`, leaves C's key revoked while the log reads its
///   seat current, and `NoCurrentSeatRevoked` breaks.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn mesh_relay_model() -> Model {
    aterm_spec::ty_model! {
        MeshRelay {
            const Buggy = 0;

            var sc = 0;
            var cr = 0;
            var ha = 0;
            var hb = 0;
            var hc = 0;
            var hd = 0;
            var sa = 0;
            var sb = 0;
            var sd = 0;
            var ka = 1;
            var kc = 1;
            var eca = 0;
            var ecb = 0;
            var eab = 0;
            var eba = 0;
            var eac = 0;
            var ebc = 0;
            var ead = 0;
            var nab = 0;
            var ncb = 0;
            var nad = 0;
            var bw = 0;
            var ex = 0;
            var cs = 0;
            var cw = 0;
            var dj = 0;
            var ev = 0;
            var lb = 0;
            var va = 0;
            var vb = 0;
            var xa = 0;
            var xb = 0;
            var fg = 0;
            var bad = 0;
            var cp = 0;
            var ru = 0;
            var a1 = 0;
            var ga = 0;
            var gb = 0;
            var na = 0;
            var nb = 0;
            var qca = 0;
            var qcb = 0;
            var qab = 0;
            var dp = 0;

            // C publishes `r` through its egress: in the cycle with B up, in the gap with B away,
            // or after the membership change on A. A Mac that is away publishes nothing.
            action Publish when (sc <= 3 && cr == 0 && (cw == 0 || cw == 2)) {
                sc = if sc == 0 { if bw == 1 { 2 } else { 1 } } else { sc };
                cr = 1;
                hc = 1;
            }

            // C's links go down, both of them, while it keeps running; `Join` heals them.
            action Split when (sc <= 1 && cs == 0 && bw == 0) {
                sc = 1;
                cs = 1;
            }
            action Join when (cs == 1) {
                cs = 2;
            }
            // C's node service stops while it is split, and starts again.
            action AwayC when (sc == 1 && cs == 1 && cw == 0) {
                cw = 1;
            }
            action ReturnC when (cw == 1) {
                cw = 2;
            }
            // D, a new Mac with empty stores: A's relay to it has no cursor, so it starts at A's
            // head and asks an `Inventory` for what lies before it.
            action Arrive when (sc == 1 && dj == 0) {
                dj = 1;
                ead = ha;
                nad = 1;
            }

            // B sleeps (its node service stops) and wakes.
            action AwayB when ((sc == 0 || sc == 4) && bw == 0) {
                bw = 1;
            }
            action ReturnB when ((sc == 2 || sc == 4) && bw == 1) {
                bw = 2;
            }
            // B's cursors on A and C are dropped: B has been away for more than 7 days.
            action Expire when (sc == 2 && bw == 1 && ex == 0) {
                ex = 1;
            }
            // The tables cut drops `r` from C's log, or from A's, below B's cursor alone: every
            // other neighbour's cursor has passed it.
            action CutC when (sc == 2 && bw == 1 && kc == 1 && eca > 0) {
                kc = 0;
            }
            action CutA when (sc == 2 && bw == 1 && ka == 1 && ha > 0 && eac > 0) {
                ka = 0;
            }

            // The membership log changes on A, where the person or the quorum acts; B applies it
            // at `Learn`. Each pass is the log-derived verdict; dial 5 revokes on the `Remove`
            // event and never lifts it.
            action Remove when (sc == 0 && bw == 0) {
                sc = 3;
                ev = 1;
                va = 1;
                xa = 1;
            }
            action Readmit when (sc == 0 && bw == 0) {
                sc = 3;
                ev = 2;
                xa = if Buggy == 5 { 1 } else { 0 };
            }
            action Void when (sc == 3 && ev == 1) {
                ev = 3;
                va = 0;
                xa = if Buggy == 5 { xa } else { 0 };
            }
            action Learn when (sc == 3 && (ev > lb || lb > ev)) {
                lb = ev;
                vb = if ev == 1 { 1 } else { 0 };
                xb = if Buggy == 5 {
                    if ev > 0 { 1 } else { xb }
                } else {
                    if ev == 1 { 1 } else { 0 }
                };
            }
            // A record under C's subject that C's key did not sign is offered to A, which never
            // appends it (refused for good while A seats C, unjudged while it does not); dial 4
            // appends it.
            action Forge when (sc == 3 && fg == 0) {
                fg = 1;
                bad = if Buggy == 4 { 1 } else { bad };
            }

            // C's relay to A: `r` while A takes C's records and C's stream; in the presence shape
            // `p1`, or after the rebuild `p3`, which A answers "already held" where the rebuilt
            // C reused an old key.
            action RelayCA when (
                (sc > 0 && sc <= 3 && (cs == 0 || cs == 2) && (cw == 0 || cw == 2) && xa == 0 &&
                    hc > eca && kc == 1)
                || (sc == 4 && ((cp == 1 && qca == 0) || (cp == 3 && qca <= 1)))
            ) {
                eca = if sc == 4 { eca } else { eca + 1 };
                ha = if sc <= 3 && (ha == 0 || (Buggy == 2 && ha == 1)) { ha + 1 } else { ha };
                sa = if sc == 4 { sa } else { 1 };
                qca = if sc == 4 { if cp == 1 { 1 } else { 2 } } else { qca };
                a1 = if sc == 4 && cp == 1 { 1 } else { a1 };
                ga = if sc == 4 {
                    if cp == 1 { 1 } else { if ru == 1 && ga > 0 { ga } else { 3 } }
                } else {
                    ga
                };
                na = if sc == 4 {
                    if cp == 1 { 1 } else { if ru == 1 && ga > 0 { na } else { 3 } }
                } else {
                    na
                };
                dp = if sc == 4 && cp == 3 && ru == 1 && ga > 0 { 1 } else { dp };
            }
            // C's relay to B: as to A; a cut record or a dropped cursor passes with a notice.
            action RelayCB when (
                (sc > 0 && sc <= 3 && (cs == 0 || cs == 2) && (cw == 0 || cw == 2) &&
                    (bw == 0 || bw == 2) && xb == 0 && hc > ecb)
                || (sc == 4 && (bw == 0 || bw == 2) &&
                    ((cp == 1 && qcb == 0) || (cp == 3 && qcb <= 1)))
            ) {
                ecb = if sc == 4 { ecb } else { if kc == 1 && ex == 0 { ecb + 1 } else { hc } };
                hb = if sc <= 3 && kc == 1 && ex == 0 && (hb == 0 || (Buggy == 2 && hb == 1)) {
                    hb + 1
                } else {
                    hb
                };
                sb = if sc <= 3 && kc == 1 && ex == 0 { 1 } else { sb };
                ncb = if sc <= 3 && (kc == 0 || ex == 1) { 1 } else { ncb };
                qcb = if sc == 4 { if cp == 1 { 1 } else { 2 } } else { qcb };
                gb = if sc == 4 {
                    if cp == 1 {
                        if gb == 0 { 1 } else { gb }
                    } else {
                        if ru == 1 && gb > 0 { gb } else { 3 }
                    }
                } else {
                    gb
                };
                nb = if sc == 4 {
                    if cp == 1 {
                        if gb == 0 { 1 } else { nb }
                    } else {
                        if ru == 1 && gb > 0 { nb } else { 3 }
                    }
                } else {
                    nb
                };
                dp = if sc == 4 && cp == 3 && ru == 1 && gb > 0 { 1 } else { dp };
            }
            // A's relay to B: `r` where B takes C's records, held while B refuses them and A
            // seats C, let go once A does not; a cut record or a dropped cursor passes with a
            // notice. In the presence shape A's rows, in the order A's log holds them.
            action RelayAB when (
                (sc > 0 && sc <= 3 && (bw == 0 || bw == 2) && ha > eab &&
                    ((ka == 1 && ex == 0 && ((vb == 0 && xb == 0) || Buggy == 4 || va == 1)) ||
                        ka == 0 || ex == 1))
                || (sc == 4 && (bw == 0 || bw == 2) &&
                    ((qab == 0 && ga > 0) || (qab == 1 && a1 == 1 && ga == 3)))
            ) {
                eab = if sc == 4 { eab } else { if ka == 1 && ex == 0 { eab + 1 } else { ha } };
                hb = if sc <= 3 && ka == 1 && ex == 0 && ((vb == 0 && xb == 0) || Buggy == 4) &&
                    (hb == 0 || (Buggy == 2 && hb == 1)) {
                    hb + 1
                } else {
                    hb
                };
                sb = if sc <= 3 && ka == 1 && ex == 0 && ((vb == 0 && xb == 0) || Buggy == 4) {
                    1
                } else {
                    sb
                };
                nab = if sc <= 3 && (ka == 0 || ex == 1) { 1 } else { nab };
                bad = if sc <= 3 && ka == 1 && ex == 0 && Buggy == 4 && vb == 1 { 1 } else { bad };
                qab = if sc == 4 { qab + 1 } else { qab };
                gb = if sc == 4 {
                    if qab == 0 && a1 == 1 { if gb == 0 { 1 } else { gb } } else { 3 }
                } else {
                    gb
                };
                nb = if sc == 4 {
                    if qab == 0 && a1 == 1 {
                        if gb == 0 || Buggy == 3 { 1 } else { nb }
                    } else {
                        3
                    }
                } else {
                    nb
                };
            }
            // B's relay to A: `r` where A takes C's records, held while A refuses them and B
            // seats C, let go once B does not.
            action RelayBA when (
                sc > 0 && sc <= 3 && (bw == 0 || bw == 2) && hb > eba &&
                    ((va == 0 && xa == 0) || Buggy == 4 || vb == 1)
            ) {
                eba = eba + 1;
                ha = if ((va == 0 && xa == 0) || Buggy == 4) &&
                    (ha == 0 || (Buggy == 2 && ha == 1)) {
                    ha + 1
                } else {
                    ha
                };
                sa = if (va == 0 && xa == 0) || Buggy == 4 { 1 } else { sa };
                bad = if Buggy == 4 && va == 1 { 1 } else { bad };
            }
            // A's and B's relays to C, the origin, while they seat it: C answers "already held".
            action RelayAC when (
                sc > 0 && sc <= 3 && (cs == 0 || cs == 2) && (cw == 0 || cw == 2) && va == 0 &&
                    ha > eac && ka == 1
            ) {
                eac = eac + 1;
                hc = if Buggy == 2 && hc == 1 { 2 } else { hc };
            }
            action RelayBC when (
                sc > 0 && sc <= 3 && (cs == 0 || cs == 2) && (cw == 0 || cw == 2) &&
                    (bw == 0 || bw == 2) && vb == 0 && hb > ebc
            ) {
                ebc = ebc + 1;
                hc = if Buggy == 2 && hc == 1 { 2 } else { hc };
            }
            // A's relay to D.
            action RelayAD when (sc == 1 && dj == 1 && ha > ead) {
                ead = ead + 1;
                hd = if hd == 0 || (Buggy == 2 && hd == 1) { hd + 1 } else { hd };
                sd = 1;
            }

            // A notice asks the pair's `Inventory`, which moves what either store lacks.
            action InventoryAB when (nab == 1 && (bw == 0 || bw == 2)) {
                nab = 0;
                sb = if sa == 1 { 1 } else { sb };
                sa = if sb == 1 { 1 } else { sa };
            }
            action InventoryCB when (
                ncb == 1 && (bw == 0 || bw == 2) && (cs == 0 || cs == 2) && (cw == 0 || cw == 2)
            ) {
                ncb = 0;
                sb = 1;
            }
            action InventoryAD when (nad == 1) {
                nad = 0;
                sd = if sa == 1 { 1 } else { sd };
            }

            // Dial 1: a relay that reads a gap asks no `Inventory` (`NO_INVENTORY_ON_GAP`); a
            // dropped cursor still asks one, as a new link.
            action GapSkip when (
                Buggy == 1 && sc == 2 && (bw == 0 || bw == 2) && ex == 0 &&
                    ((kc == 0 && hc > ecb) || (ka == 0 && ha > eab))
            ) {
                ecb = if kc == 0 { hc } else { ecb };
                eab = if ka == 0 { ha } else { eab };
            }

            // C publishes its presence row: `p1`, and `p3` once rebuilt.
            action PublishRow when ((sc == 0 || sc == 4) && (cp == 0 || cp == 2)) {
                sc = 4;
                cp = cp + 1;
            }
            // C is rebuilt with its node key kept: its log and its relays' cursors are gone, and
            // its next row is numbered above the high water A holds of it.
            action Rebuild when (sc == 4 && cp == 1 && ga > 0) {
                cp = 2;
            }
            // Dial 1: the rebuild reuses its last incarnation (`REUSED_INCARNATION`).
            action RebuildReuse when (Buggy == 1 && sc == 4 && cp == 1 && ga > 0) {
                cp = 2;
                ru = 1;
            }

            invariant AppendedOnce: ha <= 1 && hb <= 1 && hc <= 1 && hd <= 1;
            invariant OnlyCurrentOrigins: bad == 0;
            invariant NewestIsHighest: na == ga && nb == gb;
            invariant NoCurrentSeatRevoked: (va == 1 || xa == 0) && (vb == 1 || xb == 0);
            invariant NoSkip:
                (ecb == 0 || sb == 1 || ncb == 1) && (eab == 0 || sb == 1 || nab == 1) &&
                (ead == 0 || sd == 1 || nad == 1);
            invariant SeqFresh: dp == 0;
            bound RelayBox:
                sc <= 4 && cr <= 1 && ha <= 2 && hb <= 2 && hc <= 2 && hd <= 2 &&
                sa <= 1 && sb <= 1 && sd <= 1 && ka <= 1 && kc <= 1 &&
                eca <= 2 && ecb <= 2 && eab <= 2 && eba <= 2 && eac <= 2 && ebc <= 2 &&
                ead <= 2 && nab <= 1 && ncb <= 1 && nad <= 1 &&
                bw <= 2 && ex <= 1 && cs <= 2 && cw <= 2 && dj <= 1 &&
                ev <= 3 && lb <= 3 && va <= 1 && vb <= 1 && xa <= 1 && xb <= 1 &&
                fg <= 1 && bad <= 1 &&
                cp <= 3 && ru <= 1 && a1 <= 1 && ga <= 3 && gb <= 3 && na <= 3 && nb <= 3 &&
                qca <= 2 && qcb <= 2 && qab <= 2 && dp <= 1;
        }
    }
}

/// The relay's and `Inventory`'s actions, with C's return and the split's heal, that
/// [`every_up_mac_gets_every_table_record_liveness`] holds weakly fair.
pub const TABLES_FAIR: [&str; 12] = [
    "RelayCA",
    "RelayCB",
    "RelayAB",
    "RelayBA",
    "RelayAC",
    "RelayBC",
    "RelayAD",
    "InventoryAB",
    "InventoryCB",
    "InventoryAD",
    "ReturnC",
    "Join",
];

/// The relays [`restarted_mac_reads_up_liveness`] holds weakly fair.
pub const PRESENCE_FAIR: [&str; 3] = ["RelayCA", "RelayCB", "RelayAB"];

/// The actions `MeshRelay`'s conformance runs to quiescence after each other step: the relay's
/// bursts and the `Inventory` exchanges.
pub const SETTLES: [&str; 10] = [
    "RelayCA",
    "RelayCB",
    "RelayAB",
    "RelayBA",
    "RelayAC",
    "RelayBC",
    "RelayAD",
    "InventoryAB",
    "InventoryCB",
    "InventoryAD",
];

/// None of `names`' actions of `model` is enabled.
fn settled(model: &Model, names: &[&'static str]) -> Expr {
    names
        .iter()
        .map(|name| {
            let guard = model
                .actions
                .iter()
                .find(|action| action.name == *name)
                .and_then(|action| action.guard.clone())
                .unwrap_or_else(|| panic!("`{name}` is a guarded action of `MeshRelay`"));
            iff(guard, bool_lit(false))
        })
        .reduce(and_)
        .unwrap_or(bool_lit(true))
}

/// `EveryUpMacGetsEveryTableRecord`: under fair relays and `Inventory`, with C coming back and
/// the split healing, the relay settles, again and again, with every up Mac that seats C holding
/// C's table record in its store (A unless its log reads C removed, B unless away or reading C
/// removed, D once arrived). Dial 1's `GapSkip` breaks it: B returns to cut logs and nothing
/// moves `r` into its store.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(mesh_relay_model)]
pub fn every_up_mac_gets_every_table_record_liveness() -> Liveness {
    let model = mesh_relay_model();
    let held = |seated: Expr, store: &'static str| or_(seated, eq(var(store), int(1)));
    let complete = or_(
        eq(var("cr"), int(0)),
        and_(
            and_(
                held(eq(var("va"), int(1)), "sa"),
                held(or_(eq(var("bw"), int(1)), eq(var("vb"), int(1))), "sb"),
            ),
            held(eq(var("dj"), int(0)), "sd"),
        ),
    );
    Liveness {
        name: "EveryUpMacGetsEveryTableRecord",
        goal: and_(complete, settled(&model, &TABLES_FAIR)),
        weak: TABLES_FAIR.to_vec(),
        strong: vec![],
        mutants: vec!["GapSkip"],
    }
}

/// `RestartedMacReadsUp`: under fair relays, the relay settles, again and again, with C's newest
/// row, once C was rebuilt and published again, its new row on A, and on B unless B is away.
/// Dial 1's `RebuildReuse` breaks it: A answers the new row "already held" and serves the old
/// one.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(mesh_relay_model)]
pub fn restarted_mac_reads_up_liveness() -> Liveness {
    let model = mesh_relay_model();
    let up = or_(
        le(var("cp"), int(2)),
        and_(
            eq(var("na"), int(3)),
            or_(eq(var("bw"), int(1)), eq(var("nb"), int(3))),
        ),
    );
    Liveness {
        name: "RestartedMacReadsUp",
        goal: and_(up, settled(&model, &PRESENCE_FAIR)),
        weak: PRESENCE_FAIR.to_vec(),
        strong: vec![],
        mutants: vec!["RebuildReuse"],
    }
}
