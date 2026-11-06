// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Formal models of who receives what across the fabric (`docs/DESIGN-alab-one-mesh-2026-11-05.md`
//! §9 `FabricRouting`, §11 C7): mail's one hop, the sender a carrying connection proves, dedup,
//! revocation that follows the membership log both ways, and fleet-wide halts with their
//! halt-offs, their origin's removal and a Mac away past every log. The relay's own laws are
//! `MeshRelay`'s.
//!
//! §9 states one machine. It is written as machines that share no variable, `FabricRouting` (mail
//! and revocation, refined by real `bus.log` brokers and the pushes `federate.rs` opens),
//! `FabricHalts` (halts, refined by `halt.rs`'s state machine on each Mac) and `FabricMoves` (mail
//! shown once per session across key rotations, refined by the own-inbox move and the bridges on
//! one Mac's `bus.log`), so each conformance test walks every state of its own machine: their
//! product would be walked by none.

use aterm_spec::derive::*;

/// `FabricRouting`: three Macs A, B and C, each with `bus.log`. Mail M1 from a session on A to
/// one on B, M3 from C to B, and one `ev` record on A from the start. C may be removed (`Remove`)
/// and seated again with its key kept (`Readmit`, which stands for a readmission and for a void of
/// the removal alike); B applies each when its log reads it (`RevokeB`, `UnrevokeB`). C may be
/// partitioned away once and come back (`Split`, `Join`). A push (`PushA`, `PushC`) runs every
/// lane its Mac's plan names once, within its island, appending at the far end in the same step
/// (§9's `Forward` and `Append` as one step: a mirror push publishes at the far end as it
/// carries). The environment re-sends an old frame (`Replay1`, `Replay3`) and C, a member, writes
/// a lane that names A as its source over its own link to B (`Forge`).
///
/// The variables:
///
/// - `sp` the island: 0 one, 1 C apart, 2 C back;
/// - `cs` C's seat in the membership log: 0 current, 1 removed, 2 seated again;
/// - `lb` what B's applied read says of C (as `cs`), and `vb` whether B's `bus.log` refuses C's key;
/// - `p1`, `p3` M1 and M3 written on their sender's log (2: M3's group passed it uncarried);
/// - `m1b`, `m3b` how many times B appended M1 and M3, `m1c` how many times C appended M1;
/// - `r1`, `r3` whether the environment re-sent M1, M3; `ft` whether C tried the forged lane, and
///   `fg` whether B appended it;
/// - `bad` B appended a frame from C while its read had C removed; `eb` B appended A's `ev`;
///   `kb` the origin of the last frame B appended (1 A, 3 C), kept only by the dedup mutant;
///   `lost` a mail group passed a record it did not carry.
///
/// The mutants:
///
/// 1. a push forwards its Mac's mail to every member, with no destination filter (`FORWARD_ALL`
///    in `federate.rs`; the conformance test's negative control): breaks `MailOnlyToItsAddressee`;
/// 2. dedup by sequence alone: the index keeps one origin per sequence, so once two origins'
///    frames share one and a relay re-sends the first, it lands twice: breaks `AtMostOnce`;
/// 3. the sender is read from the frame, not from the proven connection: breaks
///    `SenderIsTheProvenPeer`;
/// 4. no revoke on `Remove`: breaks `NothingFromARemovedNode`;
/// 6. revoke on the `Remove` event, never lifted: breaks `NoCurrentSeatRevoked` after a `Readmit`;
/// 8. a push over every subject, not its mail lanes: breaks `EvNeverLeaves`;
///
/// Mutant 1, as the dead action `SkipC`: a second group commits past C's mail to B without
/// carrying it (`ANOTHER_GROUP`): breaks `GroupPassesOnlyCarried`, and `MailArrives`.
///
/// §9's mutant 5 (a halt-off without its id) and 7 (no lift on the origin's removal) are
/// `FabricHalts`' 5 and 7.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn fabric_routing_model() -> Model {
    aterm_spec::ty_model! {
        FabricRouting {
            const Buggy = 0;

            var sp = 0;
            var cs = 0;
            var lb = 0;
            var vb = 0;
            var p1 = 0;
            var m1b = 0;
            var m1c = 0;
            var p3 = 0;
            var m3b = 0;
            var r1 = 0;
            var r3 = 0;
            var ft = 0;
            var fg = 0;
            var bad = 0;
            var eb = 0;
            var kb = 0;
            var lost = 0;

            action Split when (sp == 0) { sp = 1; }
            action Join when (sp == 1) { sp = 2; }
            // A person removes C, and later seats it again with its key kept (a readmission, or a
            // void of the removal): records of the membership log, which B applies when it reads
            // them.
            action Remove when (cs == 0) { cs = 1; }
            action Readmit when (cs == 1) { cs = 2; }
            // B's revocation pass reads C's removal: its `bus.log` refuses C's key and principal.
            // Mutant 4 revokes nothing.
            action RevokeB when (cs == 1 && lb == 0) {
                lb = 1;
                vb = if Buggy == 4 { 0 } else { 1 };
            }
            // B's pass reads C seated again: the refusal lifts. Mutant 6 keeps it.
            action UnrevokeB when (cs == 2 && lb == 1) {
                lb = 2;
                vb = if Buggy == 6 { vb } else { 0 };
            }
            action PostA when (p1 == 0) { p1 = 1; }
            action PostC when (p3 == 0) { p3 = 1; }
            // A's pushes: its plan names B, the one Mac it wrote to, and the push carries A's
            // lanes to B alone. Mutant 1 plans a push to every member carrying every lane A wrote;
            // mutant 8 carries every subject, the `ev` record with them.
            action PushA when (p1 == 1 && m1b == 0) {
                m1b = 1;
                m1c = if Buggy == 1 && (sp > 1 || 1 > sp) { 1 } else { m1c };
                eb = if Buggy == 8 { 1 } else { eb };
                kb = if Buggy == 2 { 1 } else { kb };
            }
            // C's push to B, within its island: B's `bus.log` refuses it while it revokes C.
            action PushC when (p3 == 1 && m3b == 0 && (sp > 1 || 1 > sp)) {
                m3b = if vb == 0 { 1 } else { 0 };
                bad = if vb == 0 && lb == 1 { 1 } else { bad };
                kb = if Buggy == 2 && vb == 0 { 3 } else { kb };
            }
            // A second group on C's mail to B commits past M3 without carrying it.
            action SkipC when (Buggy == 1 && p3 == 1 && m3b == 0) {
                p3 = 2;
                lost = 1;
            }
            // The environment re-sends a frame B holds, under its origin's producer and sequence:
            // B's dedup holds it once. Mutant 2 takes it again once another origin's frame came
            // last.
            action Replay1 when (m1b > 0 && r1 == 0) {
                r1 = 1;
                m1b = if Buggy == 2 && kb == 3 { 2 } else { m1b };
            }
            action Replay3 when (m3b > 0 && r3 == 0) {
                r3 = 1;
                m3b = if Buggy == 2 && kb == 1 && vb == 0 { 2 } else { m3b };
                bad = if Buggy == 2 && kb == 1 && vb == 0 && lb == 1 { 1 } else { bad };
            }
            // C writes a lane naming A as its source over its own link to B: the ring B serves C
            // binds a lane's source to the node C proved. Mutant 3 takes the source the frame
            // names.
            action Forge when (ft == 0 && (sp > 1 || 1 > sp)) {
                ft = 1;
                fg = if Buggy == 3 && vb == 0 { 1 } else { 0 };
            }

            invariant MailOnlyToItsAddressee: m1c == 0;
            invariant AtMostOnce: m1b <= 1 && m3b <= 1;
            invariant SenderIsTheProvenPeer: fg == 0;
            invariant NothingFromARemovedNode: bad == 0;
            invariant NoCurrentSeatRevoked: lb == 1 || vb == 0;
            invariant EvNeverLeaves: eb == 0;
            invariant GroupPassesOnlyCarried: lost == 0;
            bound RouteBox:
                sp <= 2 && cs <= 2 && lb <= 2 && vb <= 1 && p1 <= 1 && m1b <= 2 && m1c <= 1 &&
                p3 <= 2 && m3b <= 2 && r1 <= 1 && r3 <= 1 && ft <= 1 && fg <= 1 && bad <= 1 &&
                eb <= 1 && kb <= 3 && lost <= 1;
        }
    }
}

/// MAIL ARRIVES: mail to a current member is appended on its Mac, again and again whatever else
/// happens, assuming each push runs once it stays possible, a partition heals, and B applies C's
/// new seat. Mail to a Mac the log keeps removed waits, held on its sender.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(fabric_routing_model)]
pub fn mail_arrives_liveness() -> aterm_spec::derive::Liveness {
    aterm_spec::derive::Liveness {
        name: "MailArrives",
        goal: and_(
            or_(eq(var("p1"), int(0)), le(int(1), var("m1b"))),
            or_(
                or_(eq(var("p3"), int(0)), le(int(1), var("m3b"))),
                eq(var("cs"), int(1)),
            ),
        ),
        weak: vec!["PushA", "PushC", "Join", "UnrevokeB"],
        strong: vec![],
        mutants: vec!["SkipC"],
    }
}

/// `FabricHalts`: a halt H1 from a person on A and a halt H2 from a person on C, a halt-off of
/// H1 from B's person and of H2 from A's, each lifting the id it names for every Mac; C's removal
/// and A's and B's revocation pass reading it (`Revoke`); C partitioned away once and back; the
/// span past which no Mac says a halt-off again and every log gave its records up (`Expire`,
/// `halt::OFF_RESAY_SPAN_MS` and `bus.log`'s seven days); and each Mac's saying again what is due
/// after it (`ResayAB`, `ResayC`), its asks for a halt held past its origin's saying among them.
///
/// A and B hear each other within the step a record is said in (one island, which never splits),
/// so they hold one view: `h1`, `h2` are A's and B's, `c1`, `c2` C's, each 0 unknown, 1 standing,
/// 2 lifted for every Mac, 3 lifted as its origin was removed. A Mac hears what the logs of its
/// island hold (`HearC`, `HearAB`); a Mac the log removed hears no relay. The other variables:
///
/// - `sp` the island (as `FabricRouting`'s), `cs` C removed, `g` A and B applied it;
/// - `o1`, `o2` the halt-off of H1, H2 on A's and B's logs: 0 never said, 1 held, 2 given up;
/// - `ex` the span passed; `f1` A said H1 again after it, `f2` C said H2 again;
/// - `c1f` C holds H1 under a saying from after the span, `h2f` A and B hold H2 so;
/// - `q` C asked for H1, `qab` A and B asked for H2; `ra`, `rc` they said again what was due.
///
/// The laws: `HaltOffLiftsOnlyItsHalt` (a halt is lifted for every Mac only by a halt-off naming
/// its id), `NoHaltOutlivesItsOrigin` (once a Mac applied the removal of a halt's origin, that
/// halt no longer stands there), `StaleHaltAsks` (a Mac that said again what was due after the
/// span holds no halt of another Mac under an old saying without asking for it), and the
/// liveness `HaltLiftsEverywhere`.
///
/// The mutants:
///
/// 5. a halt-off lifts every standing halt, whatever id it names (`halt::OFF_LIFTS_EVERY`):
///    breaks `HaltOffLiftsOnlyItsHalt`;
/// 7. no lift on the origin's removal, and what it said still applies (`halt::NO_LIFT_ON_GONE`):
///    breaks `NoHaltOutlivesItsOrigin`;
///
/// Mutant 1, as the dead action `ResayCOld`: C says again what is due as before the stale ask,
/// never asking (`halt::NO_STALE_ASK`, the case C4 left open): breaks `StaleHaltAsks`, and
/// `HaltLiftsEverywhere`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn fabric_halts_model() -> Model {
    aterm_spec::ty_model! {
        FabricHalts {
            const Buggy = 0;

            var sp = 0;
            var cs = 0;
            var g = 0;
            var h1 = 0;
            var c1 = 0;
            var c1f = 0;
            var h2 = 0;
            var h2f = 0;
            var c2 = 0;
            var o1 = 0;
            var o2 = 0;
            var ex = 0;
            var f1 = 0;
            var f2 = 0;
            var q = 0;
            var qab = 0;
            var ra = 0;
            var rc = 0;

            action Split when (sp == 0) { sp = 1; }
            action Join when (sp == 1) { sp = 2; }
            // Each halt is raised before the span passes; one raised after it is a fresh saying,
            // the same as the re-say the model tracks.
            action HaltA when (h1 == 0 && ex == 0) { h1 = 1; }
            action HaltC when (c2 == 0 && cs == 0 && ex == 0) { c2 = 1; }
            // B's person lifts H1 for every Mac; mutant 5 lifts every standing halt.
            action OffB1 when (h1 == 1) {
                h1 = 2;
                o1 = 1;
                h2 = if Buggy == 5 && h2 == 1 { 2 } else { h2 };
            }
            // A's person lifts H2, C's, for every Mac; mutant 5 lifts every standing halt.
            action OffA2 when (h2 == 1) {
                h2 = 2;
                o2 = 1;
                h1 = if Buggy == 5 && h1 == 1 { 2 } else { h1 };
            }
            action Remove when (cs == 0) { cs = 1; }
            // A's and B's pass reads C's removal: every halt of C's lifts (`Halts::lift_gone`),
            // and nothing C says applies after. Mutant 7 keeps it standing.
            action Revoke when (cs == 1 && g == 0) {
                g = 1;
                h2 = if h2 == 1 && (Buggy > 7 || 7 > Buggy) { 3 } else { h2 };
            }
            // C hears A's and B's logs: H1 with its halt-off while they hold it, H1 said again,
            // and the halt-off of H2, its own.
            action HearC when ((sp > 1 || 1 > sp) && g == 0) {
                c1 = if c1 == 0 {
                    if o1 == 1 { 2 } else { if ((h1 > 0 || 0 > h1) && ex == 0) || f1 == 1 { 1 } else { 0 } }
                } else {
                    if c1 == 1 && o1 == 1 { 2 } else { c1 }
                };
                c1f = if f1 == 1 && c1 <= 1 { 1 } else { c1f };
                c2 = if c2 == 1 && o2 == 1 { 2 } else { c2 };
            }
            // A and B hear C's log: H2, said again or not, and C's ask for H1, refused once they
            // applied C's removal (mutant 7 takes them). A Mac that lifted a halt for every Mac
            // answers its origin's saying it again, and an ask for it, with the halt-off.
            action HearAB when ((sp > 1 || 1 > sp)) {
                h2 = if (g == 0 || Buggy == 7) && h2 == 0 && (((c2 > 0 || 0 > c2) && ex == 0) || f2 == 1) {
                    1
                } else {
                    h2
                };
                h2f = if (g == 0 || Buggy == 7) && f2 == 1 && h2 <= 1 { 1 } else { h2f };
                o2 = if (g == 0 || Buggy == 7) && h2 == 2 && f2 == 1 { 1 } else { o2 };
                o1 = if (g == 0 || Buggy == 7) && h1 == 2 && q == 1 { 1 } else { o1 };
            }
            // The span passes: no Mac says a halt-off again and every log gives up what it held.
            action Expire when (ex == 0 && (o1 == 1 || o2 == 1)) {
                ex = 1;
                o1 = if o1 == 1 { 2 } else { o1 };
                o2 = if o2 == 1 { 2 } else { o2 };
            }
            // A and B say again what is due: A its standing H1, and both ask for H2, C's, held
            // under a saying from before the span.
            action ResayAB when (ex == 1 && ra == 0) {
                ra = 1;
                f1 = if h1 == 1 { 1 } else { f1 };
                qab = if h2 == 1 && h2f == 0 { 1 } else { qab };
            }
            // C says again what is due: its standing H2, and it asks for H1, A's, held under a
            // saying from before the span.
            action ResayC when (ex == 1 && rc == 0) {
                rc = 1;
                f2 = if c2 == 1 { 1 } else { f2 };
                q = if c1 == 1 && c1f == 0 { 1 } else { q };
            }
            // The path before the stale ask: C says again its own halts, and asks for none.
            action ResayCOld when (Buggy == 1 && ex == 1 && rc == 0) {
                rc = 1;
                f2 = if c2 == 1 { 1 } else { f2 };
            }

            invariant HaltOffLiftsOnlyItsHalt:
                ((h1 > 2 || 2 > h1) || o1 > 0) && ((c1 > 2 || 2 > c1) || o1 > 0) &&
                ((h2 > 2 || 2 > h2) || o2 > 0) && ((c2 > 2 || 2 > c2) || o2 > 0);
            invariant NoHaltOutlivesItsOrigin: g == 0 || (h2 > 1 || 1 > h2);
            invariant StaleHaltAsks: rc == 0 || (c1 > 1 || 1 > c1) || c1f == 1 || q == 1;
            bound HaltBox:
                sp <= 2 && cs <= 1 && g <= 1 && h1 <= 3 && c1 <= 3 && c1f <= 1 && h2 <= 3 &&
                h2f <= 1 && c2 <= 3 && o1 <= 2 && o2 <= 2 && ex <= 1 && f1 <= 1 && f2 <= 1 &&
                q <= 1 && qab <= 1 && ra <= 1 && rc <= 1;
        }
    }
}

/// A HALT LIFTED FOR EVERY MAC IS LIFTED ON EVERY MEMBER: no member holds for good a halt the
/// others lifted for every Mac, assuming each Mac hears its island's logs and says again what is
/// due once it stays possible, and a partition heals. A Mac the log keeps removed is no member.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(fabric_halts_model)]
pub fn halt_lifts_everywhere_liveness() -> aterm_spec::derive::Liveness {
    aterm_spec::derive::Liveness {
        name: "HaltLiftsEverywhere",
        goal: or_(
            eq(var("cs"), int(1)),
            and_(
                or_(neq(var("h1"), int(2)), neq(var("c1"), int(1))),
                or_(neq(var("h2"), int(2)), neq(var("c2"), int(1))),
            ),
        ),
        weak: vec!["HearC", "HearAB", "Join", "ResayC"],
        strong: vec![],
        mutants: vec!["ResayCOld"],
    }
}

/// `FabricMoves`: mail delivered once per destination session across key rotations (§11 C3b's
/// own-inbox move, C3e). B's ask X lies on A's `bus.log` for A's node N0 (from the start), and
/// A's key rotates twice (`Rotate1` to N1, `Rotate2` to N2). Each node's bridge runs while its
/// node is A's; its commit stays below what it delivered (`Deliver0`, `Deliver1`): a bridge killed
/// between a delivery and its commit, or a commit held below a record it could not deliver,
/// the worst case, since a commit past a record only takes its copies away. Under N1 the node
/// service's move of N0's unread mail lands its copy Y (`MoveLand1`, a burst) and may be
/// acknowledged, committing N0's group past X (`MoveAck1`), or not before the second rotation.
/// Under N2 the move of N0's mail carries X again while that group is not committed past it, and
/// commits it (`MoveLand2a`), and the move of N1's carries Y on (`MoveLand2b`). N2's bridge delivers each copy
/// (`DeliverA`, `DeliverB`). Every delivery names where its mail was first on A's log (`was=`,
/// followed back through every move); aterm, one process throughout, shows it to the session
/// unless it remembers the delivery's offset or the one it names, and remembers both
/// (`aterm_mesh_core::inbox_move::shows_anew`).
///
/// The variables: `r` the rotations; `d0`, `d1`, `da`, `db` whether X, Y and N2's copies of X
/// and of Y were delivered; `y`, `za`, `zb` whether those copies landed; `c0` N0's group committed
/// past X; `kx`, `ky` aterm remembers X's offset, Y's; `shown` how many times aterm showed the
/// ask to its session; `lost` N0's group committed past X with no copy landed.
///
/// The mutants:
///
/// 1. an endpoint before `LINK_MOVED_LANE`: no delivery names a first offset, and aterm shows
///    every copy (the conformance test's negative control): breaks `ShownOnce`;
/// 2. a copy followed back one move alone: N2's copy of Y names Y, which aterm knows only where
///    N1's bridge delivered it: breaks `ShownOnce`.
///
/// Mutant 1, as the dead action `DropMoved`: the move's commit is made with no copy landed:
/// breaks `AckOnlyCarried`, and `MailShown`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn fabric_moves_model() -> Model {
    aterm_spec::ty_model! {
        FabricMoves {
            const Buggy = 0;

            var r = 0;
            var d0 = 0;
            var y = 0;
            var c0 = 0;
            var d1 = 0;
            var za = 0;
            var zb = 0;
            var da = 0;
            var db = 0;
            var kx = 0;
            var ky = 0;
            var shown = 0;
            var lost = 0;

            action Rotate1 when (r == 0) { r = 1; }
            action Rotate2 when (r == 1) { r = 2; }
            // N0's bridge delivers X, its commit held below it.
            action Deliver0 when (r == 0 && d0 == 0) {
                d0 = 1;
                shown = if kx == 0 { shown + 1 } else { shown };
                kx = 1;
            }
            // The move to N1 lands Y, under N1's producer, numbered by X's offset; its commit is
            // made in a later burst, if at all before the second rotation.
            action MoveLand1 when (r == 1 && y == 0 && c0 == 0) { y = 1; }
            action MoveAck1 when (r == 1 && y == 1 && c0 == 0) { c0 = 1; }
            action DropMoved when (Buggy == 1 && r == 1 && y == 0 && c0 == 0) {
                c0 = 1;
                lost = 1;
            }
            // N1's bridge delivers Y naming X (mutant 1 names nothing), its commit held below.
            action Deliver1 when (r == 1 && y == 1 && d1 == 0) {
                d1 = 1;
                shown = if kx == 0 || Buggy == 1 { shown + 1 } else { shown };
                kx = if Buggy == 1 { kx } else { 1 };
                ky = 1;
            }
            // Under N2, the move of N0's mail carries X while N0's group stands below it, and the
            // move of N1's carries Y on.
            action MoveLand2a when (r == 2 && za == 0 && c0 == 0) {
                za = 1;
                c0 = 1;
            }
            action MoveLand2b when (r == 2 && zb == 0 && y == 1) { zb = 1; }
            // N2's bridge delivers its copy of X naming X, one move back.
            action DeliverA when (r == 2 && za == 1 && da == 0) {
                da = 1;
                shown = if kx == 0 || Buggy == 1 { shown + 1 } else { shown };
                kx = if Buggy == 1 { kx } else { 1 };
            }
            // N2's copy of Y names X, two moves back; mutant 2 names Y, one move back.
            action DeliverB when (r == 2 && zb == 1 && db == 0) {
                db = 1;
                shown = if Buggy == 1 || (Buggy == 2 && ky == 0) || (Buggy == 0 && kx == 0) {
                    shown + 1
                } else {
                    shown
                };
                kx = if Buggy == 1 || Buggy == 2 { kx } else { 1 };
                ky = if Buggy == 2 { 1 } else { ky };
            }

            invariant ShownOnce: shown <= 1;
            invariant AckOnlyCarried: lost == 0;
            bound MoveBox:
                r <= 2 && d0 <= 1 && y <= 1 && c0 <= 1 && d1 <= 1 && za <= 1 && zb <= 1 &&
                da <= 1 && db <= 1 && kx <= 1 && ky <= 1 && shown <= 4 && lost <= 1;
        }
    }
}

/// MAIL SHOWN: the ask is shown to its session, whatever the rotations and the moves' commits do,
/// assuming each rotation, each move and each delivery under the last node runs once it stays
/// possible.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(fabric_moves_model)]
pub fn mail_shown_liveness() -> aterm_spec::derive::Liveness {
    aterm_spec::derive::Liveness {
        name: "MailShown",
        goal: le(int(1), var("shown")),
        weak: vec![
            "Rotate1",
            "Rotate2",
            "MoveLand2a",
            "MoveLand2b",
            "DeliverA",
            "DeliverB",
        ],
        strong: vec![],
        mutants: vec!["DropMoved"],
    }
}
