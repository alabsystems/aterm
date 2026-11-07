// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Formal model of gap healing: a current seat that lacks a `Wrap` of a newest epoch whose batch
//! has settled is a gap that a ranked holder heals, and a missing `Wrap` is never a revocation
//! (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.2 M1, M4 and M5, and §9).

use aterm_spec::derive::*;

/// `GapHeal`: four members of one audience (`[everyone]`, or Commons until the fold). A and B
/// run this design's build: each vault pass closes the gaps it ranks for
/// (`log_entries::close_wrap_gaps`) and then tends the re-key dues it carries
/// (`remove::tend_epochs_at`), in that order (`remove::vault_pass_at`). C may leave, and in two
/// shapes it is a pre-R1 build that mints by the intersection carry. D is a member admitted on
/// the home LAN. The model fixes the order of every gap it opens as A first and B second, and C,
/// an old build, last; C never heals a seat an epoch left out. `Log::gap_rank` orders a gap's
/// holders by a hash, so a conformance run picks identities that give this order.
///
/// The clock counts steps. A `Tick` is the clock passing the next deadline: a rank step
/// (`log_entries::GAP_STEP_MS`), the settle grace (`log_entries::SETTLE_GRACE_MS`) or the
/// remover's grace (`log_entries::REMOVER_GRACE_MS`). The model counts each as one step and
/// keeps only their order, so a conformance run advances its injected clock at each `Tick` to
/// the deadline that `Tick` stands for. A `Tick` waits until every Mac that is up has run its
/// vault pass since its last record arrived (`pa`, `pb`). A batch that ends in a non-`Wrap`
/// record (a remover's re-`Put`s) is settled once whole. One that ends in `Wrap`s is settled
/// once whole where every recipient its batch could wrap holds a `Wrap` of it (its own, or a
/// heal's) or sorts below its last `Wrap` (`Log::batch_whole`, one-mesh §11 H2): a batch wraps in
/// ascending recipient order, so the rest of it could wrap no one. Otherwise it settles only by the settle grace: a whole batch
/// an observer holds at a `Tick` is settled. B's rank comes due one step after its clock for
/// the gap starts (`bt`), and the clock is dropped once B sees no gap, as the gap mark keeps
/// only open gaps.
///
/// The model rests on four timing assumptions, which a conformance run's clocks must meet:
///
/// 1. A record signed in a step reaches every Mac that is up before the next step starts. A
///    `Tick` waits for every delivery, a Mac posts its heals before it sleeps (`Sleep` needs
///    `qb` 0), and a wake's pull succeeds before its pass (`Wake` brings A every record). The
///    code posts and pulls best-effort: `remove::vault_pass` drops a failed post, and a failed
///    pull does not stop the pass.
/// 2. Every first sight in a step counts from that step's start, and a first sight at a wake
///    from the next step's start (`va` 5). The code counts each Mac's settle and ranks from its
///    own first sight, so a wake's settle can fall in the step B's rank comes due.
/// 3. B's due pass runs at the start of the step its rank falls due in, before any other action
///    of that step (`bt` 2). Nothing schedules it: the gap mark's `due` only lets a pass skip
///    work, and a pass runs at the subscriber's next sync.
/// 4. A roster deny reaches B at once (`Deny`).
///
/// `NoStorm` holds only under them. Outside them a rank-0 and a rank-1 due can fall less than one
/// delivery apart, and both holders sign a heal: the ranks bound duplicates (design M5, and §9's
/// `AtMostTwoHeals`) but do not remove them.
///
/// The first action picks one of three shapes (`sc`):
///
/// - `OldMint` (1): C mints epoch 2 by the intersection carry over a partial view, wrapping A, B
///   and itself and leaving D out though D's `Admit` lies in its past (the 88-to-3 drop). Its
///   batch reaches A as a prefix (`DeliverToA` twice) and B whole (`DeliverToB`). A may `Sleep`
///   and `Wake`; while A is down, B heals at its rank. The model fixes A's recipient the lowest
///   of A, B and C, so A's prefix (its `Epoch` and A's own `Wrap`) leaves B and C above it and
///   never settles. D's recipient sorts above C's, the batch's last `Wrap`, so the whole batch
///   settles only by the grace; `OldMintUnder` is the same mint with D's recipient below C's
///   (`dl`), so the whole batch is settled as soon as it arrives.
/// - `OldMintOmitsB` (2): as `OldMint`, but C leaves B out (and there is no D). C could not know
///   B (B's `Admit` is not in its batch's past), so its batch could wrap only A and C, and it is
///   settled as soon as it arrives whole. A heals B. A person on B removes C (`RemoveByB`). A B
///   that a heal has wrapped mints in its own batch; a B that cannot open the newest epoch signs
///   the `Remove` alone. That leaves a re-key due, minted
///   by whichever tender is ready first. B's (`RotateByB`) is the remover's and, once A's heal
///   reaches it, a carrier's, so it ranks 0, and it waits only the settle hold from its first
///   sight of the due (`rb`). A's (`Rotate`) is a carrier's but not the remover's, so it waits
///   the remover's grace from its first sight of the `Remove` (`gr`). It also waits for C's batch
///   to settle at A and for A's closer to run, which heals B first. Both need the newest epoch
///   still 2: the model assumes one tender's batch reaches the other before that one is ready.
///   Two heads minted in one step are coalescing (`EpochConvergence`, design M4), outside this
///   model.
/// - `Churn` (3): A removes C and mints epoch 2 in its own batch (`Remove`) while B admits D
///   (`Admit`); A's epoch wraps D only if D's `Admit` reached A first. A person's roster deny of
///   D (`Deny`, a swarm-roster deny or a node removal) may follow.
///
/// Delivery: `DeliverToA` and `DeliverToB` bring C's batch; `ToB` brings A's records to B (its
/// batch as a prefix, then the rest, then its heals, and C's batch with a heal of it where B
/// lacks that batch); `ToA` brings B's records to A (D's `Admit`, B's heals and, in
/// `OldMintOmitsB`, B's `Remove` and whatever B minted). A heal is pushed: `qb` is a heal of A's
/// not yet at B and `qa` one of B's not yet at A.
///
/// The variables of the newest epoch `ep` (1 the genesis epoch wrapped to A, B and C; 2 the
/// shape's mint; 3 the re-key after C's removal) are as follows:
///
/// - its minter `mm` (0 genesis, 1 A, 2 B, 3 C);
/// - whether C's `Remove` lies in its past (`er`);
/// - whether its minter knew D's `Admit` (`mk`);
/// - the minter's own `Wrap`s to each Mac (`wa`..`wd`);
/// - the heal `Wrap`s A and B signed of it (`hab`, `hac`, `had`, `hbc`, `hbd`, the second
///   letter the target; `hbd` is also the `Wrap` B's admission of D signs).
///
/// `va` is A's view of the newest epoch's batch, which is C's, B's or A's own (A's own reads 3):
/// 0 none, 1 a prefix (its `Epoch` and A's own `Wrap`), 2 whole and unsettled, 3 settled, 4 a
/// prefix A slept across a step with (its grace from the first sight its pass recorded ran out),
/// 5 whole, first seen at a wake (counted from the next step's start, assumption 2), and 6 none,
/// A asleep across a step. `vb` is B's view of the newest batch: 0 none, 1 a prefix of A's (its
/// `Epoch` and A's own `Wrap`, after A's `Remove` where there is one), 2 whole and unsettled,
/// 3 settled.
///
/// The other variables:
///
/// - `ad` D is admitted, `dk` A holds D's `Admit`, `dy` D is denied, `dl` D's recipient sorts
///   below the last `Wrap` of C's batch (`OldMintUnder`);
/// - `cx` C is removed, `kr` A holds C's `Remove`;
/// - `gr` A's clock for the remover's grace (1 started, 2 run), and `rb` B's tender's settle hold
///   (1 started, 2 run);
/// - `sa` A is asleep;
/// - `age` counts the steps a current seat lacked the newest epoch while a Mac that is up held it
///   settled and knew the seat;
/// - `dw` a Mac that held D's deny signed a `Wrap` to D;
/// - `lost` a Mac that could not open the newest epoch minted the next.
///
/// The laws:
///
/// - `GapsClose`: every gap closes within one step of a Mac that is up seeing it, a heal ranked
///   first at once and the next a step later;
/// - `NoStorm`: at most one `Wrap` of an epoch to one Mac (under the timing assumptions above);
/// - `RemovalIsFinal`: no Mac removed before an epoch's mint ever gets a `Wrap` of it;
/// - `DeniedNeverWrapped`: no Mac that holds a seat's deny wraps it. Under assumption 4 every Mac
///   holds it. Until wave D turns a deny into a `Remove`, a Mac the deny has not reached may
///   still wrap the seat;
/// - `OnlyHoldersMintOrHeal`: a Mac that cannot open the newest epoch mints nothing and heals
///   nothing (a heal is held to it by construction, since it wraps a key the healer opened);
/// - `FrontierHealsAtOnce`: once A, up and ranked first, has run its pass over C's whole batch
///   whose last `Wrap` leaves no recipient it could wrap above it, no gap of it is open: a gap
///   below a settled frontier heals at once, not after the settle grace.
///
/// The mutants:
///
/// 1. the old gap rule: a seat a newest epoch's minter knew and left out is read as revoked
///    (`Log::left_out`), so it is never healed;
/// 2. the heal counts a removed Mac as current and wraps it;
/// 3. the heal and the remover's mint count a denied Mac as current;
/// 4. a remover that cannot open the newest epoch mints in its batch anyway (gen 66);
/// 5. no rank: every holder heals at its first settled look (`remove::UNRANKED`);
/// 6. the heal reads a prefix or an unsettled batch as final;
/// 7. a mint counts a removed Mac as current and wraps it;
/// 8. B's rank counts from its first sight of the gap, not from when the batch settled
///    (`log_entries::GAPS_FROM_FIRST_SIGHT`), so it comes due in the step A's does;
/// 9. a whole batch that ends in `Wrap`s reads as a prefix until the settle grace even where its
///    last `Wrap` leaves no recipient it could wrap above it, as before H2
///    (`rekey::FRONTIER_BLIND`), so A holds D's or B's gap a step.
///
/// The old intersection carry (`carried_audience_recipients`' left-out filter) is no mutant here.
/// In every shape it agrees with the current seats on each seat the minter knows. The closer runs
/// before the tender in each pass, so a re-key in `OldMintOmitsB` finds B already healed, and
/// `Remove`'s epoch 1 wraps every seat A knows.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn gap_heal_model() -> Model {
    aterm_spec::ty_model! {
        GapHeal {
            const Buggy = 0;

            var sc = 0;
            var ep = 1;
            var mm = 0;
            var er = 0;
            var mk = 0;
            var wa = 1;
            var wb = 1;
            var wc = 1;
            var wd = 0;
            var hab = 0;
            var hac = 0;
            var had = 0;
            var hbc = 0;
            var hbd = 0;
            var va = 3;
            var vb = 3;
            var ad = 0;
            var dk = 0;
            var dy = 0;
            var dl = 0;
            var cx = 0;
            var kr = 0;
            var sa = 0;
            var pa = 0;
            var pb = 0;
            var qa = 0;
            var qb = 0;
            var bt = 0;
            var age = 0;
            var dw = 0;
            var lost = 0;
            var gr = 0;
            var rb = 0;

            action OldMint when (sc == 0) {
                sc = 1;
                ep = 2;
                mm = 3;
                mk = 1;
                wd = 0;
                ad = 1;
                dk = 1;
                va = 0;
                vb = 0;
            }
            action OldMintUnder when (sc == 0) {
                sc = 1;
                ep = 2;
                mm = 3;
                mk = 1;
                wd = 0;
                ad = 1;
                dk = 1;
                dl = 1;
                va = 0;
                vb = 0;
            }
            action OldMintOmitsB when (sc == 0) {
                sc = 2;
                ep = 2;
                mm = 3;
                mk = 1;
                wb = 0;
                va = 0;
                vb = 0;
            }
            action Churn when (sc == 0) {
                sc = 3;
            }

            // B admits D on the home LAN, wrapping the newest epoch B can open: epoch 2 once it
            // holds A's batch whole, and epoch 1 while it holds none of it, which leaves D a gap
            // of A's epoch 2. While B holds only a prefix of A's batch it reads itself as left
            // out of epoch 2 and admits no one.
            action Admit when ((bt <= 1 || bt == 3) && sc == 3 && ad == 0 &&
                (ep == 1 || vb == 0 || vb == 3)) {
                ad = 1;
                hbd = if ep == 1 || vb == 3 { 1 } else { 0 };
                pb = 0;
            }
            // A removes C and, holding the newest epoch, mints the next in the same batch, wrapped
            // to the current seats it knows. Mutant 8 wraps the removed C, mutant 3 the denied D.
            action Remove when ((bt <= 1 || bt == 3) && sc == 3 && cx == 0) {
                cx = 1;
                kr = 1;
                ep = 2;
                mm = 1;
                er = 1;
                mk = dk;
                wa = 1;
                wb = 1;
                wc = if Buggy == 7 { 1 } else { 0 };
                wd = if dk == 1 && (dy == 0 || Buggy == 3) { 1 } else { 0 };
                dw = if dk == 1 && dy == 1 && Buggy == 3 { 1 } else { dw };
                hab = 0;
                hac = 0;
                had = 0;
                hbc = 0;
                hbd = 0;
                vb = 0;
                bt = 0;
                pa = 0;
            }
            // A person denies D in the swarm roster (or its node is removed), on A once A holds
            // D's `Admit`; the model pushes the roster to B at once (timing assumption 4).
            action Deny when ((bt <= 1 || bt == 3) && sc == 3 && ad == 1 && dk == 1 && dy == 0) {
                dy = 1;
                pa = 0;
                pb = 0;
            }
            // A person on B removes C. B mints in its own batch only when it opens the newest
            // epoch, which it does once A's heal of it arrived; mutant 4 mints without it.
            action RemoveByB when ((bt <= 1 || bt == 3) && sc == 2 && cx == 0 && vb > 1) {
                cx = 1;
                ep = if (hab == 1 && qb == 0) || Buggy == 4 { 3 } else { ep };
                mm = if (hab == 1 && qb == 0) || Buggy == 4 { 2 } else { mm };
                er = if (hab == 1 && qb == 0) || Buggy == 4 { 1 } else { er };
                lost = if Buggy == 4 && (hab == 0 || qb == 1) { 1 } else { lost };
                wa = if (hab == 1 && qb == 0) || Buggy == 4 { 1 } else { wa };
                wb = if (hab == 1 && qb == 0) || Buggy == 4 { 1 } else { wb };
                wc = if (hab == 1 && qb == 0) || Buggy == 4 {
                    if Buggy == 7 { 1 } else { 0 }
                } else {
                    wc
                };
                hab = if (hab == 1 && qb == 0) || Buggy == 4 { 0 } else { hab };
                hac = if (hab == 1 && qb == 0) || Buggy == 4 { 0 } else { hac };
                vb = if (hab == 1 && qb == 0) || Buggy == 4 { 3 } else { vb };
                va = if (hab == 1 && qb == 0) || Buggy == 4 { 0 } else { va };
                bt = if (hab == 1 && qb == 0) || Buggy == 4 { 0 } else { bt };
                qb = if (hab == 1 && qb == 0) || Buggy == 4 { 0 } else { qb };
                pb = 0;
            }
            // A's tender re-keys the due C's `Remove` left (the newest epoch still wraps a Mac the
            // log removed), wrapped to the current seats. A carries epoch 2 but did not sign the
            // `Remove`, so it waits the remover's grace (`gr`), C's batch to settle at A, and its
            // own closer, which runs first in the pass. Mutant 7 wraps the removed C.
            action Rotate when ((bt <= 1 || bt == 3) && sc == 2 && ep == 2 && gr == 2 && va == 3 &&
                pa == 1) {
                ep = 3;
                mm = 1;
                er = 1;
                wa = 1;
                wb = 1;
                wc = if Buggy == 7 { 1 } else { 0 };
                hab = 0;
                hac = 0;
                vb = 0;
                va = 3;
                bt = 0;
                qb = 0;
                rb = 0;
                pa = 0;
            }
            // B's tender re-keys the same due: B signed the `Remove` and carries epoch 2 once A's
            // heal reached it, so it ranks 0 with no grace and waits only the settle hold from
            // its first sight of the due (`rb`). Its batch ends in re-`Put`s. Mutant 7 wraps the
            // removed C.
            action RotateByB when ((bt <= 1 || bt == 3) && sc == 2 && ep == 2 && rb == 2) {
                ep = 3;
                mm = 2;
                er = 1;
                wa = 1;
                wb = 1;
                wc = if Buggy == 7 { 1 } else { 0 };
                hab = 0;
                hac = 0;
                vb = 3;
                va = 0;
                bt = 0;
                rb = 0;
                pb = 0;
            }

            // C's batch is settled once whole where its last `Wrap` leaves no recipient it could
            // wrap above it (`dl`, or B unknown to C); mutant 9 reads it as a prefix until the
            // grace all the same.
            action DeliverToA when ((bt <= 1 || bt == 3) && mm == 3 && sa == 0 && (va == 0 || (va == 1 && sc == 1))) {
                va = if sc == 1 && va == 0 {
                    1
                } else {
                    if (sc == 2 || dl == 1) && (if Buggy == 9 { 1 } else { 0 }) == 0 {
                        3
                    } else {
                        2
                    }
                };
                pa = 0;
            }
            action DeliverToB when ((bt <= 1 || bt == 3) && mm == 3 && vb == 0) {
                vb = if (sc == 2 || dl == 1) && (if Buggy == 9 { 1 } else { 0 }) == 0 {
                    3
                } else {
                    2
                };
                pb = 0;
            }
            action ToA when ((bt <= 1 || bt == 3) && sa == 0 &&
                (qa == 1 || (dk == 0 && ad == 1) || (kr == 0 && cx == 1) || (mm == 2 && va == 0))) {
                qa = 0;
                dk = ad;
                kr = cx;
                gr = if kr == 0 && cx == 1 && ep == 2 { 1 } else { gr };
                // B's records bring C's batch whole with B's `Remove` (`OldMintOmitsB`, where it
                // settles once whole; mutant 9 reads it as a prefix until the grace), or B's heal
                // of D to a whole batch A holds unsettled, which then leaves nothing its batch could
                // wrap missing.
                va = if mm == 2 && va == 0 {
                    if lost == 1 { 2 } else { 3 }
                } else {
                    if kr == 0 && cx == 1 && va == 0 {
                        if (if Buggy == 9 { 1 } else { 0 }) == 0 { 3 } else { 2 }
                    } else {
                        if mm == 3 && hbd == 1 && (va == 2 || va == 5) &&
                            (if Buggy == 9 { 1 } else { 0 }) == 0 {
                            3
                        } else {
                            va
                        }
                    }
                };
                pa = 0;
            }
            // A heal A signed as soon as C's whole batch settled at A can precede that batch at B
            // (`OldMintOmitsB`, `OldMintUnder`): A's records bring it whole with the heal. A's heal
            // of D settles C's whole batch wherever it lands.
            action ToB when ((bt <= 1 || bt == 3) && ((mm == 1 && vb <= 1) || qb == 1)) {
                vb = if mm == 1 && vb <= 1 {
                    if vb == 0 { 1 } else { 3 }
                } else {
                    if mm == 3 && (if Buggy == 9 { 1 } else { 0 }) == 0 &&
                        ((vb == 0 && (va == 2 || va == 3 || va == 5) &&
                            (sc == 2 || dl == 1 || had == 1)) || (vb == 2 && had == 1)) {
                        3
                    } else {
                        if mm == 3 && vb == 0 && (va == 2 || va == 3 || va == 5) { 2 } else { vb }
                    }
                };
                qb = if mm == 1 && vb <= 1 { qb } else { 0 };
                rb = if sc == 2 && cx == 1 && ep == 2 && qb == 1 { 1 } else { rb };
                pb = 0;
            }
            // A sleeps only after its pass ran over what it last received, so a gap's settle clock
            // starts at that pass's first sight.
            action Sleep when ((bt <= 1 || bt == 3) && sc == 1 && sa == 0 && qb == 0 && pa == 1) {
                sa = 1;
            }
            // A wakes to C's whole batch, and B's heal of D where B signed one: where D's `Wrap`
            // (C's own below its last, or a heal) leaves nothing its batch could wrap missing,
            // the batch is settled as it arrives.
            action Wake when ((bt <= 1 || bt == 3) && sa == 1) {
                sa = 0;
                qa = 0;
                va = if (dl == 1 || hbd == 1) && (va <= 2 || va > 3) &&
                    (if Buggy == 9 { 1 } else { 0 }) == 0 {
                    3
                } else {
                    if va <= 1 || va == 6 {
                        if va == 6 { 5 } else { 2 }
                    } else {
                        if va == 4 { 3 } else { va }
                    }
                };
                pa = 0;
            }

            // A's vault pass: ranked first, it wraps every gap it sees at once. Mutant 1 sees no
            // gap where the minter knew the seat; mutant 6 reads an unsettled batch as settled.
            action HealA when ((bt <= 1 || bt == 3) && sc > 0 && sa == 0) {
                pa = 1;
                hab = if wa == 1 && va > 0 && va <= 5 && (va == 3 || Buggy == 6) &&
                    (va <= 1 || wb == 0) && hab == 0 &&
                    (if Buggy == 1 { 1 } else { 0 }) == 0 {
                    1
                } else {
                    hab
                };
                hac = if (cx == 0 || Buggy == 2) && wa == 1 && va > 0 && va <= 5 &&
                    (va == 3 || Buggy == 6) &&
                    (va <= 1 || wc == 0) && hac == 0 && (hbc == 0 || qa == 1) &&
                    (if Buggy == 1 { 1 } else { 0 }) == 0 {
                    1
                } else {
                    hac
                };
                had = if ad == 1 && (dy == 0 || Buggy == 3) && dk == 1 &&
                    wa == 1 && va > 0 && va <= 5 && (va == 3 || Buggy == 6) &&
                    (va <= 1 || wd == 0) && had == 0 && (hbd == 0 || qa == 1) &&
                    (if Buggy == 1 { mk } else { 0 }) == 0 {
                    1
                } else {
                    had
                };
                qb = if wa == 1 && va > 0 && va <= 5 && (va == 3 || Buggy == 6) && (
                    ((va <= 1 || wb == 0) && hab == 0 &&
                        (if Buggy == 1 { 1 } else { 0 }) == 0) ||
                    ((cx == 0 || Buggy == 2) &&
                        (va <= 1 || wc == 0) && hac == 0 && (hbc == 0 || qa == 1) &&
                        (if Buggy == 1 { 1 } else { 0 }) == 0) ||
                    (ad == 1 && (dy == 0 || Buggy == 3) && dk == 1 &&
                        (va <= 1 || wd == 0) && had == 0 && (hbd == 0 || qa == 1) &&
                        (if Buggy == 1 { mk } else { 0 }) == 0)) {
                    1
                } else {
                    qb
                };
                dw = if dy == 1 && Buggy == 3 && ad == 1 && dk == 1 &&
                    wa == 1 && va > 0 && va <= 5 && va == 3 &&
                    (va <= 1 || wd == 0) && had == 0 && (hbd == 0 || qa == 1) {
                    1
                } else {
                    dw
                };
            }
            // B's vault pass: ranked second, it starts its clock for a gap it sees (`bt` 1) and
            // wraps it once the clock has run a step. Timing assumption 3 runs that pass at the
            // step's start, before anything else in the step (`bt` 2), so a Mac that wakes in the
            // step syncs B's heal before its own pass (`bt` 3 once it ran). Mutant 5 wraps at
            // once, mutant 8 starts the clock at first sight of an unsettled batch
            // (`log_entries::GAPS_FROM_FIRST_SIGHT`), mutants 1 and 6 as A's.
            action HealB when (sc > 0) {
                pb = 1;
                bt = if ((cx == 0 || Buggy == 2) &&
                        ((vb > 1 && wb == 1) || (hab == 1 && qb == 0)) &&
                        (vb == 3 || Buggy == 6 || (Buggy == 8 && vb == 2)) &&
                        (vb <= 1 || wc == 0) && hbc == 0 && (hac == 0 || qb == 1) &&
                        (if Buggy == 1 { 1 } else { 0 }) == 0) ||
                    (ad == 1 && (dy == 0 || Buggy == 3) &&
                        ((vb > 1 && wb == 1) || (hab == 1 && qb == 0)) &&
                        (vb == 3 || Buggy == 6 || (Buggy == 8 && vb == 2)) &&
                        (vb <= 1 || wd == 0) && hbd == 0 && (had == 0 || qb == 1) &&
                        (if Buggy == 1 { mk } else { 0 }) == 0) {
                    if bt == 0 { 1 } else { if bt == 2 { 3 } else { bt } }
                } else {
                    0
                };
                hbc = if (bt > 1 || Buggy == 5) && (cx == 0 || Buggy == 2) &&
                    ((vb > 1 && wb == 1) || (hab == 1 && qb == 0)) &&
                    (vb == 3 || Buggy == 6) &&
                    (vb <= 1 || wc == 0) && hbc == 0 && (hac == 0 || qb == 1) &&
                    (if Buggy == 1 { 1 } else { 0 }) == 0 {
                    1
                } else {
                    hbc
                };
                hbd = if (bt > 1 || Buggy == 5) && ad == 1 && (dy == 0 || Buggy == 3) &&
                    ((vb > 1 && wb == 1) || (hab == 1 && qb == 0)) &&
                    (vb == 3 || Buggy == 6) &&
                    (vb <= 1 || wd == 0) && hbd == 0 && (had == 0 || qb == 1) &&
                    (if Buggy == 1 { mk } else { 0 }) == 0 {
                    1
                } else {
                    hbd
                };
                qa = if (bt > 1 || Buggy == 5) &&
                    ((vb > 1 && wb == 1) || (hab == 1 && qb == 0)) &&
                    (vb == 3 || Buggy == 6) && (
                    ((cx == 0 || Buggy == 2) &&
                        (vb <= 1 || wc == 0) && hbc == 0 && (hac == 0 || qb == 1) &&
                        (if Buggy == 1 { 1 } else { 0 }) == 0) ||
                    (ad == 1 && (dy == 0 || Buggy == 3) &&
                        (vb <= 1 || wd == 0) && hbd == 0 && (had == 0 || qb == 1) &&
                        (if Buggy == 1 { mk } else { 0 }) == 0)) {
                    1
                } else {
                    qa
                };
                dw = if dy == 1 && Buggy == 3 && (bt > 1 || Buggy == 5) && ad == 1 &&
                    ((vb > 1 && wb == 1) || (hab == 1 && qb == 0)) && vb == 3 &&
                    (vb <= 1 || wd == 0) && hbd == 0 && (had == 0 || qb == 1) {
                    1
                } else {
                    dw
                };
            }

            action Tick when ((bt <= 1 || bt == 3) && sc > 0 && pb == 1 && qb == 0 &&
                (mm == 0 || mm == 2 || (mm == 3 && vb > 1) || (mm == 1 && vb == 3)) &&
                (sa == 1 || (pa == 1 && qa == 0 && dk == ad && kr == cx &&
                    (mm <= 1 || va == 2 || va == 3 || va == 5)))) {
                pa = 0;
                pb = 0;
                va = if va == 2 {
                    3
                } else {
                    if va == 5 {
                        2
                    } else {
                        if va == 1 { 4 } else { if va == 0 && sa == 1 { 6 } else { va } }
                    }
                };
                vb = if vb == 2 { 3 } else { vb };
                bt = if bt == 1 { 2 } else { bt };
                gr = if gr == 1 { 2 } else { gr };
                rb = if rb == 1 { 2 } else { rb };
                age = if (ad == 1 && dy == 0 && wd == 0 && had == 0 && hbd == 0 &&
                        ((sa == 0 && wa == 1 && va == 3 && dk == 1) ||
                            (((vb > 1 && wb == 1) || (hab == 1 && qb == 0)) && vb == 3))) ||
                    (wb == 0 && hab == 0 && sa == 0 && wa == 1 && va == 3) {
                    if age == 0 { 1 } else { 2 }
                } else {
                    0
                };
            }

            invariant GapsClose: age <= 1;
            invariant NoStorm: wb + hab <= 1 && wd + had + hbd <= 1 && wc + hac + hbc <= 1;
            invariant RemovalIsFinal: er == 0 || wc + hac + hbc == 0;
            invariant DeniedNeverWrapped: dw == 0;
            invariant OnlyHoldersMintOrHeal: lost == 0;
            invariant FrontierHealsAtOnce:
                if sa == 0 && pa == 1 && mm == 3 && (sc == 2 || dl == 1) && wa == 1 &&
                    (va == 2 || va == 3 || va == 5) {
                    (wb == 1 || hab == 1) &&
                        (ad == 0 || dy == 1 || dk == 0 || wd == 1 || had == 1 || hbd == 1)
                } else {
                    1 == 1
                };
            bound GapHealBox:
                sc <= 3 && ep <= 3 && mm <= 3 && er <= 1 && mk <= 1 &&
                wa <= 1 && wb <= 1 && wc <= 1 && wd <= 1 &&
                hab <= 1 && hac <= 1 && had <= 1 && hbc <= 1 && hbd <= 1 &&
                va <= 6 && vb <= 3 && ad <= 1 && dk <= 1 && dy <= 1 && dl <= 1 && cx <= 1 &&
                kr <= 1 &&
                sa <= 1 && pa <= 1 && pb <= 1 && qa <= 1 && qb <= 1 && bt <= 3 && age <= 2 &&
                dw <= 1 && lost <= 1 && gr <= 2 && rb <= 2;
        }
    }
}
