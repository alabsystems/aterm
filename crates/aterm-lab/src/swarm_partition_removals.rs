// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `swarm partition`'s removals plane: F16's quorum Undo on the healed lab
//! (`docs/DESIGN-fluid-swarm-2026-10-28.md` §3.3.7, §4.1 F16).
//!
//! After the heal b2 turns hostile, as a stolen Mac would: a person at b2 runs
//! `aterm members remove` on every other member of the membership log, each named by its join
//! code, and each removal mints new epochs that leave its Mac out. Then a person on each Mac b2
//! removed presses "Undo b2's removals" ([`client::undo_removals`], the call that press makes;
//! `aterm members undo` only lists the rows, a share being a person's act), signing its share of
//! the quorum `Remove` of b2 at the cutoff before b2's first contested act, until the quorum
//! takes effect: b2's removals are
//! void and b2 is out for good. The epoch tender then mints one higher epoch that leaves b2 out
//! ([`client::tend_epochs_at`]), and a2, a Mac b2 removed, puts a value under it
//! ([`client::test_put_everyone`], the stand-in for a member's writer). The plane then reads each
//! Mac's own records: every Mac b2 removed is a member again and reads the value, b2 is out by
//! the quorum and reads none of it, and no `Wrap` of the value's epoch is addressed to b2.
//!
//! The removals are the aterm under test's CLI in each Mac's world. Between acts the lab carries
//! every Mac's records to every other ([`client::save_records`]), as the subscribers' pulls
//! would, so no act waits on a pull. The tender runs in each Mac's agent on the wall clock,
//! ranked so one Mac mints at once and the next a gap step (60 s) later; the lab runs the
//! tender's pass itself at a clock [`TEND_AHEAD`] gap steps on, so the rank spread does not hold
//! the lane, round the Macs until none finds a rotation due ([`TEND_ROUNDS`] rounds at most).
//!
//! Then F21's split (§4.1 F21): the lab cuts island A (a1, a2, a3) off from island B (b1, b2,
//! b3) again with the lane's own cut ([`split::Cut`]), which every process of each island
//! honours on every connect, and carries records only within each island itself. On A, a1
//! removes b3 and a3; on B, b3, not yet told, removes a3, which mints its epoch, and b1 puts a
//! value under it. Each removal is signed kept on its Mac ([`client::test_remove_unposted`]),
//! and then that Mac's own records sync (`aterm secrets sync` in its world) posts its held
//! records to every server that lacks its frontier: its own island's server must take them, and
//! the cut refuses the other's. That pass most likely carried a1's removal of b3 to island B
//! on 2026-11-02, when the vault's `aterm-swarm-policy` listed island B's server and the split
//! cut nothing (the heal-retry run; not traced, and b3 already held the removal before it
//! acted), so the split's row sees it held back, not merely idle ([`split::synced`]). b1's put
//! posts its records to every server it knows ([`client::test_put_everyone`]), all at once on
//! threads of its own. The lab makes each removal and the put in a process of its Mac's world
//! ([`act`], [`split::Cut::act`]), which its island's cut covers whole, as that Mac's own
//! process would be (a cut held on one thread of this process would let the post's other
//! threads across). The split's ledger ([`split::Ledger`]) reads every Mac and both servers
//! when the cut falls, again once an exchange in flight then has landed ([`split::LAND`]; each
//! island then takes in what its own Macs hold, so b3 acts on all its island holds, a1's acts
//! alone unknown to it), and just before the heal, and [`SPLIT_HELD`] fails where a record or
//! head either island made, or held alone at that second read, reached the other, naming the
//! plane, or where a records sync left its own island's server short; [`KEPT`] is not judged
//! on a split something crossed, or one not read whole or not held, since that run is not shown
//! to measure a write on the removed Mac's side. Then the lab heals (the cut lifts, and every
//! Mac takes every other's records) and runs the tender's pass as before: every member reads
//! b1's value, a write sealed under an epoch b3 minted past its cutoff; b3's epoch is keyed and
//! covered by a reseal; and no Mac wraps a newest epoch to b3. b3 plays F21's removed Mac
//! because b2 is out by F16's quorum by then.

use std::time::Instant;

use aterm_secrets::age::Recipient;
use aterm_secrets::client::{self, Paths};
use aterm_secrets::membership_log::{Log, MemberId, RecordId};
use aterm_secrets::text::Audience;

use super::commons::Mac;
use super::{Island, split};

/// The hostile Mac.
pub const REMOVER: &str = "b2";
/// The Mac that puts the value after the Undo: one b2 removed.
const PUTTER: &str = "a2";
/// The value's `[everyone]` entry name.
pub const VALUE_NAME: &str = "f16-after-undo";
/// The value: synthetic.
const VALUE: &[u8] = b"f16 lab value put after the quorum undo";
/// How many gap steps on the lab runs the tender's pass.
const TEND_AHEAD: u64 = 10;
/// The most rounds of the tender's pass round every Mac.
const TEND_ROUNDS: usize = 4;
/// The tender's gap step (`aterm_secrets::client::log_entries::GAP_STEP_MS`, 60 s).
const GAP_STEP_MS: u64 = 60_000;
/// F21's removed Mac: removed by a1 on island A while it mints an epoch on island B.
pub const CUT: &str = "b3";
/// The Mac on b3's island that writes under b3's epoch.
const WRITER: &str = "b1";
/// The Mac b3 removes on island B, which mints its epoch; a1 removes it too.
const ALSO: &str = "a3";
/// F21's value's `[everyone]` entry name.
pub const KEPT_NAME: &str = "f21-written-while-out";
/// F21's value: synthetic.
const KEPT_VALUE: &[u8] = b"f21 lab value b1 put under b3's epoch while a1 removed b3";

/// One act a person typed: `aterm members remove|undo` on a Mac.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Act {
    /// The Mac it ran on.
    pub on: String,
    /// The Mac it named.
    pub names: String,
    /// It exited 0.
    pub ok: bool,
    /// What it said, on one line.
    pub said: String,
}

/// One Mac at the plane's end, read from its own records.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MacEnd {
    /// The Mac.
    pub mac: String,
    /// Its records hold a membership log.
    pub has_log: bool,
    /// Its log has it as a member.
    pub member: bool,
    /// Its log has the quorum `Remove` of b2 in effect.
    pub undone: bool,
    /// Its log has b2 out by a quorum.
    pub remover_out_by_quorum: bool,
    /// It finds a rotation due (two newest epochs, or a newest one wrapped to a Mac now out).
    pub rotation_due: bool,
    /// It reads the value, as it was put.
    pub reads_value: bool,
}

/// The removals plane's evidence.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Evidence {
    /// Why the plane did not run, where it did not.
    pub skipped: Option<String>,
    /// The Macs b2 removed, in order.
    pub targets: Vec<String>,
    /// b2's removals.
    pub removals: Vec<Act>,
    /// The Undo shares, in order.
    pub undos: Vec<Act>,
    /// The voters at the Undo's cutoff and how many signed, as the last Mac to sign read them.
    pub voters: usize,
    /// How many of them signed.
    pub signed: usize,
    /// What the tender's passes minted: `<mac> minted <n> records in round <k>`.
    pub minted: Vec<String>,
    /// Each failure to carry records between Macs: `<from> -> <to>: <why>`.
    pub carry_failures: Vec<String>,
    /// The value's put: `Err` with why it failed.
    pub put: Option<Result<(), String>>,
    /// The Macs whose records wrap the value's epoch to b2.
    pub wrapped_to_remover: Vec<String>,
    /// b2's row in a1's `members list`, as a person reads it.
    pub remover_row: Option<String>,
    /// Each Mac at the end.
    pub ends: Vec<MacEnd>,
    /// F21's split, where the plane got that far.
    pub kept: Option<Kept>,
    /// Milliseconds the plane took.
    pub ms: u64,
}

/// One Mac at the end of F21's split, read from its own records.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeptEnd {
    /// The Mac.
    pub mac: String,
    /// Its records hold a membership log.
    pub has_log: bool,
    /// Its log has it as a member.
    pub member: bool,
    /// Its log has b3 as a member.
    pub cut_member: bool,
    /// It reads b1's value, as it was put.
    pub reads_value: bool,
    /// Its log finds b3's epoch keyed: cut, with an honest write relying on it.
    pub keyed: bool,
    /// Its log finds b3's epoch covered by a full reseal.
    pub covered: bool,
    /// It finds a rotation or a reseal due.
    pub rotation_due: bool,
}

/// F21's split's evidence.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Kept {
    /// Why it did not run, where it did not.
    pub skipped: Option<String>,
    /// The removals in the split, in order.
    pub removals: Vec<Act>,
    /// b1's put: `Err` with why it failed.
    pub put: Option<Result<(), String>>,
    /// The split's ledger: every Mac and server when the cut fell, once what was in flight then
    /// had landed, and just before the heal.
    pub ledger: Option<split::Ledger>,
    /// b1's value went under an epoch b3 minted.
    pub under_cut_epoch: bool,
    /// What the tender's passes minted: `<mac> minted <n> records in round <k>`.
    pub minted: Vec<String>,
    /// The Macs whose records wrap a newest `[everyone]` epoch to b3.
    pub wrapped_to_cut: Vec<String>,
    /// Each Mac at the end.
    pub ends: Vec<KeptEnd>,
}

/// The plane's check that b2's removals were undone.
pub const UNDONE: &str = "removals: the Macs one member removed undo its removals by a quorum";
/// The plane's check named in the design: `removed_macs_that_reach_a_quorum_are_back_in_and_read_new_values`.
pub const BACK_IN: &str =
    "removals: removed Macs that reach a quorum are back in and read new values";
/// F21's check (`docs/DESIGN-fluid-swarm-2026-10-28.md` §4.1 F21, build step 11).
pub const KEPT: &str = "removals: a write on the removed Mac's side survives the heal";
/// F21's split's own check: the lab's split held, so [`KEPT`] judges the product
/// ([`split::judge`]).
pub const SPLIT_HELD: &str = "removals: no record or head crossed F21's split";

/// One named check of the plane.
pub type Check = super::Check;

/// Judge the removals plane.
///
/// [`BACK_IN`] is F16's `removed_macs_that_reach_a_quorum_are_back_in_and_read_new_values`:
/// every Mac b2 removed is a member again on its own records and reads the value put after the
/// Undo, b2 is out by the quorum, holds the records and reads none of it, and no Mac's records
/// wrap the value's epoch to b2.
///
/// NEGATIVE CONTROL (proven in a lane run, 2026-10-29): make
/// `coalesce_concurrent_commons_epochs` return `Ok(())` at once (no new epoch after the quorum
/// `Remove`): the value goes under the epoch from before b2's removals, which is wrapped to b2,
/// so the voided remover reads it, and [`BACK_IN`] fails.
#[must_use]
pub fn judge(ev: &Evidence) -> Vec<Check> {
    if let Some(why) = &ev.skipped {
        return vec![Check {
            name: "removals: the plane ran",
            pass: false,
            detail: why.clone(),
        }];
    }
    let mut out = Vec::new();
    let mut off = Vec::new();
    for a in ev.removals.iter().filter(|a| !a.ok) {
        off.push(format!("{} could not remove {}: {}", a.on, a.names, a.said));
    }
    if ev.targets.is_empty() {
        off.push(format!("{REMOVER} found no member to remove"));
    }
    for e in ev.ends.iter().filter(|e| e.has_log) {
        if !e.undone {
            off.push(format!(
                "{}'s records do not undo {REMOVER}'s removals",
                e.mac
            ));
        } else if !e.remover_out_by_quorum {
            off.push(format!("{}'s records keep {REMOVER} in", e.mac));
        }
    }
    out.push(Check {
        name: UNDONE,
        pass: off.is_empty(),
        detail: if off.is_empty() {
            format!(
                "{REMOVER} removed {}; {} of {} voters signed; {}",
                ev.targets.join(", "),
                ev.signed,
                ev.voters,
                ev.remover_row.as_deref().unwrap_or("no row for it")
            )
        } else {
            off.join("; ")
        },
    });
    let mut wrong = Vec::new();
    match &ev.put {
        None => wrong.push(format!("{PUTTER} put no value")),
        Some(Err(why)) => wrong.push(format!("{PUTTER} could not put the value: {why}")),
        Some(Ok(())) => {}
    }
    for t in &ev.targets {
        match ev.ends.iter().find(|e| &e.mac == t) {
            None => wrong.push(format!("{t} was not read")),
            Some(e) if !e.has_log => wrong.push(format!("{t} holds no membership log")),
            Some(e) => {
                if !e.member {
                    wrong.push(format!("{t} is not back in"));
                }
                if !e.reads_value && ev.put == Some(Ok(())) {
                    wrong.push(format!("{t} does not read the value"));
                }
            }
        }
    }
    match ev.ends.iter().find(|e| e.mac == REMOVER) {
        Some(e) if e.has_log => {
            if e.reads_value {
                wrong.push(format!("{REMOVER} reads the value"));
            }
            if e.member {
                wrong.push(format!("{REMOVER} is still a member"));
            }
        }
        _ => wrong.push(format!("{REMOVER} holds no records to read")),
    }
    if !ev.wrapped_to_remover.is_empty() {
        wrong.push(format!(
            "{} wrap the value's epoch to {REMOVER}",
            ev.wrapped_to_remover.join(", ")
        ));
    }
    let due: Vec<&str> = ev
        .ends
        .iter()
        .filter(|e| e.rotation_due)
        .map(|e| e.mac.as_str())
        .collect();
    if !due.is_empty() {
        wrong.push(format!("{} still find a rotation due", due.join(", ")));
    }
    out.push(Check {
        name: BACK_IN,
        pass: wrong.is_empty(),
        detail: if wrong.is_empty() {
            format!(
                "{} back in and read {VALUE_NAME}; {REMOVER} reads nothing of it; {}",
                ev.targets.join(", "),
                if ev.minted.is_empty() {
                    "the tender minted nothing".to_owned()
                } else {
                    ev.minted.join(", ")
                }
            )
        } else {
            wrong.join("; ")
        },
    });
    let ledger = ev.kept.as_ref().and_then(|k| k.ledger.as_ref());
    let voiding: Vec<split::Shortfall> = split::shortfalls(ledger)
        .into_iter()
        .filter(|s| s.voids())
        .collect();
    out.push(judge_kept(ev.kept.as_ref(), &voiding));
    out.push(split::judge(SPLIT_HELD, "F21's split", ledger));
    out
}

/// [`KEPT`]: b1's write under the epoch b3 minted while a1 removed it is read after the heal by
/// every member, b3's epoch is keyed and covered once the tender's pass is done, nothing is due,
/// b3 is out everywhere, and no Mac wraps a newest `[everyone]` epoch to b3. Where the split is
/// not shown to have held ([`SPLIT_HELD`]; `short`, each way it fell short that voids it,
/// [`split::Shortfall::voids`]), it is not judged:
/// what crossed is the lab's leak, and a split not read whole or not held is not shown to have
/// kept a1's removal from b1, so the run is not shown to measure a write on the removed Mac's
/// side. Its words say which.
///
/// Its negative controls are the judge tests' (`swarm_partition_removals_tests.rs`). A lane run
/// with `reseal_ready` answering `false` would leave b3's epoch uncovered and a reseal due; it is
/// not run, since every Mac's aterm and the lab would need that mutant.
fn judge_kept(kept: Option<&Kept>, short: &[split::Shortfall]) -> Check {
    let fail = |detail: String| Check {
        name: KEPT,
        pass: false,
        detail,
    };
    let Some(k) = kept else {
        return fail("the split did not run".to_owned());
    };
    if let Some(why) = &k.skipped {
        return fail(format!("the split did not run: {why}"));
    }
    if !short.is_empty() {
        return fail(format!(
            "not judged: {} ({SPLIT_HELD}), so this run is not shown to measure a write on the \
             removed Mac's side",
            short
                .iter()
                .map(|s| s.words("F21's split"))
                .collect::<Vec<_>>()
                .join("; ")
        ));
    }
    let mut wrong: Vec<String> = k
        .removals
        .iter()
        .filter(|a| !a.ok)
        .map(|a| format!("{} could not remove {}: {}", a.on, a.names, a.said))
        .collect();
    match &k.put {
        None => wrong.push(format!("{WRITER} put no value")),
        Some(Err(why)) => wrong.push(format!("{WRITER} could not put the value: {why}")),
        Some(Ok(())) => {}
    }
    if k.put == Some(Ok(())) && !k.under_cut_epoch {
        wrong.push(format!("{WRITER} wrote under an epoch {CUT} did not mint"));
    }
    for name in ["a1", "a2", WRITER] {
        if !k.ends.iter().any(|e| e.mac == name && e.member) {
            wrong.push(format!("{name} is no member at the end"));
        }
    }
    for e in k.ends.iter().filter(|e| e.has_log) {
        if e.member && !e.reads_value {
            wrong.push(format!("{} does not read {KEPT_NAME}", e.mac));
        }
        if e.cut_member {
            wrong.push(format!("{}'s records keep {CUT} in", e.mac));
        }
        if !e.keyed {
            wrong.push(format!(
                "{}'s records do not find {CUT}'s epoch keyed",
                e.mac
            ));
        } else if !e.covered {
            wrong.push(format!("{}'s records leave {CUT}'s epoch uncovered", e.mac));
        }
        if e.rotation_due {
            wrong.push(format!("{} still finds a rotation due", e.mac));
        }
    }
    if !k.wrapped_to_cut.is_empty() {
        wrong.push(format!(
            "{} wrap a newest epoch to {CUT}",
            k.wrapped_to_cut.join(", ")
        ));
    }
    let readers: Vec<&str> = k
        .ends
        .iter()
        .filter(|e| e.reads_value)
        .map(|e| e.mac.as_str())
        .collect();
    Check {
        name: KEPT,
        pass: wrong.is_empty(),
        detail: if wrong.is_empty() {
            format!(
                "{WRITER} put {KEPT_NAME} under {CUT}'s epoch while a1 removed {CUT}; {} read it \
                 after the heal; {CUT}'s epoch is keyed and covered and no newest epoch is \
                 wrapped to it; {}",
                readers.join(", "),
                if k.minted.is_empty() {
                    "the tender minted nothing".to_owned()
                } else {
                    k.minted.join(", ")
                }
            )
        } else {
            wrong.join("; ")
        },
    }
}

/// The plane's report section.
#[must_use]
pub fn report(ev: &Evidence) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    if let Some(why) = &ev.skipped {
        let _ = writeln!(out, "- not run: {why}");
        return out;
    }
    let _ = writeln!(out, "- took {:.1}s", super::secs(ev.ms));
    for a in &ev.removals {
        let _ = writeln!(
            out,
            "- {} removes {}: {}",
            a.on,
            a.names,
            if a.ok {
                a.said.clone()
            } else {
                format!("refused: {}", a.said)
            }
        );
    }
    for a in &ev.undos {
        let _ = writeln!(
            out,
            "- {} undoes {REMOVER}'s removals: {}",
            a.on,
            if a.ok {
                a.said.clone()
            } else {
                format!("refused: {}", a.said)
            }
        );
    }
    let _ = writeln!(out, "- {} of {} voters signed", ev.signed, ev.voters);
    if let Some(row) = &ev.remover_row {
        let _ = writeln!(out, "- a1 lists {REMOVER}: {row}");
    }
    for m in &ev.minted {
        let _ = writeln!(out, "- the tender: {m}");
    }
    match &ev.put {
        Some(Ok(())) => {
            let _ = writeln!(out, "- {PUTTER} puts {VALUE_NAME}");
        }
        Some(Err(why)) => {
            let _ = writeln!(out, "- {PUTTER} could not put {VALUE_NAME}: {why}");
        }
        None => {}
    }
    for e in &ev.ends {
        let _ = writeln!(
            out,
            "- {}: {}{}{}",
            e.mac,
            if !e.has_log {
                "no membership log".to_owned()
            } else if e.member {
                "member".to_owned()
            } else {
                "out".to_owned()
            },
            if e.reads_value {
                ", reads the value"
            } else {
                ""
            },
            if e.rotation_due { ", rotation due" } else { "" }
        );
    }
    if let Some(k) = &ev.kept {
        report_kept(&mut out, k);
    }
    for f in &ev.carry_failures {
        let _ = writeln!(out, "- carrying records {f}");
    }
    out
}

/// F21's split's lines in the plane's report section.
fn report_kept(out: &mut String, k: &Kept) {
    use std::fmt::Write as _;
    if let Some(why) = &k.skipped {
        let _ = writeln!(out, "- F21's split: not run: {why}");
        return;
    }
    for a in &k.removals {
        let _ = writeln!(
            out,
            "- in the split, {} removes {}: {}",
            a.on,
            a.names,
            if a.ok {
                a.said.clone()
            } else {
                format!("refused: {}", a.said)
            }
        );
    }
    match &k.put {
        Some(Ok(())) => {
            let _ = writeln!(
                out,
                "- in the split, {WRITER} puts {KEPT_NAME} under {}",
                if k.under_cut_epoch {
                    format!("{CUT}'s epoch")
                } else {
                    format!("an epoch {CUT} did not mint")
                }
            );
        }
        Some(Err(why)) => {
            let _ = writeln!(
                out,
                "- in the split, {WRITER} could not put {KEPT_NAME}: {why}"
            );
        }
        None => {}
    }
    if let Some(ledger) = &k.ledger {
        out.push_str(&split::report("F21's split", ledger));
        if let Some(why) = &ledger.failed {
            let _ = writeln!(out, "- F21's split: the lab could not hold it: {why}");
        }
    }
    for m in &k.minted {
        let _ = writeln!(out, "- after the split's heal, the tender: {m}");
    }
    for e in &k.ends {
        let _ = writeln!(
            out,
            "- after the split, {}: {}{}{}{}",
            e.mac,
            if !e.has_log {
                "no membership log".to_owned()
            } else if e.member {
                "member".to_owned()
            } else {
                "out".to_owned()
            },
            if e.reads_value {
                format!(", reads {KEPT_NAME}")
            } else {
                String::new()
            },
            match (e.keyed, e.covered) {
                (true, true) => format!(", {CUT}'s epoch keyed and covered"),
                (true, false) => format!(", {CUT}'s epoch keyed, uncovered"),
                (false, _) => format!(", {CUT}'s epoch not keyed"),
            },
            if e.rotation_due { ", rotation due" } else { "" }
        );
    }
}

fn log_of(paths: &Paths) -> Option<Log> {
    client::load_records(paths).ok()?.log().cloned()
}

fn recipient_of(paths: &Paths) -> Option<Recipient> {
    client::read_machine_identity(paths)
        .ok()
        .flatten()
        .map(|identity| identity.recipient())
}

/// Carry every Mac's records to every other, noting each failure.
fn carry(macs: &[Mac], ev: &mut Evidence) {
    carry_among(&macs.iter().collect::<Vec<_>>(), ev);
}

/// Carry each of `macs`' records to each other of them, noting each failure.
fn carry_among(macs: &[&Mac], ev: &mut Evidence) {
    let wires: Vec<(String, Vec<u8>)> = macs
        .iter()
        .filter_map(|m| {
            let store = client::load_records(&m.paths).ok()?;
            (!store.is_empty()).then(|| (m.name.clone(), store.encode()))
        })
        .collect();
    for (from, wire) in &wires {
        for to in macs.iter().filter(|m| &m.name != from) {
            if let Err(e) = client::save_records(&to.paths, wire) {
                ev.carry_failures
                    .push(format!("{from} -> {}: {e}", to.name));
            }
        }
    }
}

fn one_line(out: &super::world::Out) -> String {
    super::one_line(&format!("{} {}", out.stdout.trim(), out.stderr.trim()))
}

/// Run the plane on `macs` (the lane's Macs), each act `act(mac, args)` the aterm under test's
/// CLI in that Mac's world, F21's split held by `cut`.
pub fn run(
    macs: &[Mac],
    cut: &dyn split::Cut,
    act: &dyn Fn(&str, &[&str]) -> super::world::Out,
) -> Evidence {
    let t = Instant::now();
    let mut ev = Evidence::default();
    drive(macs, cut, act, &mut ev);
    ev.ms = u64::try_from(t.elapsed().as_millis()).unwrap_or(u64::MAX);
    ev
}

fn drive(
    macs: &[Mac],
    cut: &dyn split::Cut,
    act: &dyn Fn(&str, &[&str]) -> super::world::Out,
    ev: &mut Evidence,
) {
    let mac = |name: &str| macs.iter().find(|m| m.name == name);
    let Some(remover) = mac(REMOVER) else {
        ev.skipped = Some(format!("no {REMOVER}"));
        return;
    };
    carry(macs, ev);
    if !client::removes_by_log(&remover.paths) {
        ev.skipped = Some(format!(
            "{REMOVER} removes no Mac through a membership log (the Commons plane seeds the log)"
        ));
        return;
    }
    let (Some(log), Some(remover_recipient)) =
        (log_of(&remover.paths), recipient_of(&remover.paths))
    else {
        ev.skipped = Some(format!("{REMOVER} holds no log or no identity"));
        return;
    };
    let Some(remover_actor): Option<MemberId> = log
        .membership()
        .members()
        .iter()
        .find(|(_, m)| m.binding.recipient() == remover_recipient)
        .map(|(actor, _)| *actor)
    else {
        ev.skipped = Some(format!("{REMOVER} is no member of its own log"));
        return;
    };
    let listed = match client::member_list(&remover.paths) {
        Ok(Some(listed)) => listed,
        Ok(None) => {
            ev.skipped = Some(format!("{REMOVER} lists no members"));
            return;
        }
        Err(e) => {
            ev.skipped = Some(format!("{REMOVER} lists no members: {e}"));
            return;
        }
    };

    // b2 removes every other member, each by its join code.
    for row in listed
        .iter()
        .filter(|row| row.removed.is_none() && row.name != REMOVER)
    {
        let out = act(REMOVER, &["members", "remove", &row.code]);
        ev.removals.push(Act {
            on: REMOVER.to_owned(),
            names: row.name.clone(),
            ok: out.code == 0,
            said: one_line(&out),
        });
        if out.code == 0 {
            ev.targets.push(row.name.clone());
        }
    }
    carry(macs, ev);

    // Each Mac b2 removed signs its share until the quorum takes effect. A share is a person's
    // press of the Undo row (`aterm members undo` only lists the rows), so the lab stands in for
    // the press with the call the Machines view makes on it.
    for target in ev.targets.clone() {
        let Some(m) = mac(&target) else {
            continue;
        };
        let (ok, said) = match client::undo_removals(&m.paths, REMOVER) {
            Ok(share) => (
                true,
                format!(
                    "{} this Mac's share; {} more needed",
                    if share.signed {
                        "signed"
                    } else {
                        "already signed"
                    },
                    share.undo.needed()
                ),
            ),
            Err(e) => (false, e.to_string()),
        };
        ev.undos.push(Act {
            on: target.clone(),
            names: REMOVER.to_owned(),
            ok,
            said,
        });
        carry(macs, ev);
        let undo = mac(&target)
            .and_then(|m| log_of(&m.paths))
            .and_then(|log| log.undo(&remover_actor));
        if let Some(undo) = undo {
            ev.voters = undo.voters.len();
            ev.signed = undo.signed.len();
            if undo.took_effect {
                break;
            }
        }
    }

    // The tender's pass, at a clock TEND_AHEAD gap steps on, until no Mac finds a rotation due.
    let ahead = aterm_time::unix_millis()
        .unwrap_or(0)
        .saturating_add(TEND_AHEAD * GAP_STEP_MS);
    for round in 1..=TEND_ROUNDS {
        if !macs.iter().any(|m| rotation_due(&m.paths)) {
            break;
        }
        for m in macs {
            if let Ok(signed) = client::tend_epochs_at(&m.paths, ahead)
                && !signed.is_empty()
            {
                ev.minted.push(format!(
                    "{} minted {} records in round {round}",
                    m.name,
                    signed.len()
                ));
                carry(macs, ev);
            }
        }
    }

    // a2 puts the value; every Mac reads its own records.
    let put = mac(PUTTER).map(|m| put_everyone(&m.paths));
    ev.put = Some(match &put {
        Some(Ok(_)) => Ok(()),
        Some(Err(e)) => Err(e.to_string()),
        None => Err(format!("no {PUTTER}")),
    });
    carry(macs, ev);
    let epoch = put.and_then(Result::ok);
    for m in macs {
        let end = read_end(m, &remover_actor);
        if let (Some(epoch), Some(log), Some(identity)) = (
            epoch,
            log_of(&m.paths),
            client::read_machine_identity(&m.paths).ok().flatten(),
        ) && let Ok(Some(opened)) = log.open_audience_entries(&identity)
            && opened
                .wrapped_recipients
                .get(&epoch)
                .is_some_and(|set| set.contains(&remover_recipient))
        {
            ev.wrapped_to_remover.push(m.name.clone());
        }
        ev.ends.push(end);
    }
    if let Some(a1) = mac("a1")
        && let Ok(Some(listed)) = client::member_list(&a1.paths)
    {
        ev.remover_row = listed
            .iter()
            .find(|row| row.name == REMOVER)
            .and_then(|row| row.removed.as_ref())
            .map(client::RemovedNote::title);
    }
    let kept = keyed_write(macs, cut, act, ev);
    ev.kept = Some(kept);
}

/// `on`'s own records sync (`aterm secrets sync`, `act` in its world, cut as its island is) under
/// F21's split, noted in `ledger` ([`split::synced`]): the pass that most likely carried a1's
/// removal of b3 to island B on 2026-11-02 (not traced), so the split's row sees it held back,
/// its own island's server taking what `on` made.
fn sync_under_split(
    on: &Mac,
    act: &dyn Fn(&str, &[&str]) -> super::world::Out,
    watched: &[split::Watched],
    servers: &[(Island, String)],
    signers: &split::Signers,
    ledger: &mut split::Ledger,
) {
    let out = act(&on.name, &["secrets", "sync"]);
    let said = (out.code != 0).then(|| one_line(&out));
    let island = island_of(&on.name);
    let (Some(mac), Some((_, server))) = (
        watched.iter().find(|w| w.name == on.name),
        servers.iter().find(|(i, _)| *i == island),
    ) else {
        ledger.syncs.push(split::Synced {
            mac: on.name.clone(),
            island,
            made: 0,
            lacking: Vec::new(),
            unread: Some(format!(
                "no {} or no island {} server",
                on.name,
                island.word()
            )),
            said,
        });
        return;
    };
    let synced = split::synced(ledger, mac, server, signers, said);
    ledger.syncs.push(synced);
}

/// The island a lane Mac is on, by its name (`a1` …, `b1` …).
fn island_of(name: &str) -> Island {
    if name.starts_with('a') {
        Island::A
    } else {
        Island::B
    }
}

/// F21's split on `macs`, held by `lab_cut`, each records sync `act(mac, args)` the aterm under
/// test's CLI in that Mac's world (see the module doc).
#[allow(clippy::too_many_lines)]
fn keyed_write(
    macs: &[Mac],
    lab_cut: &dyn split::Cut,
    act: &dyn Fn(&str, &[&str]) -> super::world::Out,
    ev: &mut Evidence,
) -> Kept {
    let mut k = Kept::default();
    let mac = |name: &str| macs.iter().find(|m| m.name == name);
    let (Some(a1), Some(cut), Some(writer)) = (mac("a1"), mac(CUT), mac(WRITER)) else {
        k.skipped = Some(format!("no a1, {CUT} or {WRITER}"));
        return k;
    };
    carry(macs, ev);
    let listed = match client::member_list(&a1.paths) {
        Ok(Some(listed)) => listed,
        Ok(None) => {
            k.skipped = Some("a1 lists no members".to_owned());
            return k;
        }
        Err(e) => {
            k.skipped = Some(format!("a1 lists no members: {e}"));
            return k;
        }
    };
    let code = |name: &str| {
        listed
            .iter()
            .find(|row| row.name == name && row.removed.is_none())
            .map(|row| row.code.clone())
    };
    let (Some(cut_code), Some(also_code)) = (code(CUT), code(ALSO)) else {
        k.skipped = Some(format!("{CUT} or {ALSO} is no member on a1's list"));
        return k;
    };
    let (Some(log), Some(cut_recipient)) = (log_of(&cut.paths), recipient_of(&cut.paths)) else {
        k.skipped = Some(format!("{CUT} holds no log or no identity"));
        return k;
    };
    let Some(cut_actor): Option<MemberId> = log
        .membership()
        .members()
        .iter()
        .find(|(_, m)| m.binding.recipient() == cut_recipient)
        .map(|(actor, _)| *actor)
    else {
        k.skipped = Some(format!("{CUT} is no member of its own log"));
        return k;
    };
    let island_a: Vec<&Mac> = macs
        .iter()
        .filter(|m| island_of(&m.name) == Island::A)
        .collect();
    let island_b: Vec<&Mac> = macs
        .iter()
        .filter(|m| island_of(&m.name) == Island::B)
        .collect();
    let watched: Vec<split::Watched> = macs
        .iter()
        .map(|m| split::Watched {
            name: m.name.clone(),
            island: island_of(&m.name),
            paths: m.paths.clone(),
        })
        .collect();
    let servers = lab_cut.servers();
    let mut signers = split::Signers::new();

    // The split: the lane's cut again, so no process of either island reaches the other's.
    if let Err(e) = lab_cut.set(true) {
        k.skipped = Some(format!("the lab could not cut the islands apart: {e}"));
        return k;
    }
    let held_from = Instant::now();
    let mut ledger = split::Ledger {
        cut: split::read(&watched, &servers, &mut signers),
        ..split::Ledger::default()
    };
    // Once an exchange that was crossing when the cut fell has landed, each island takes in
    // what its own Macs hold (so b3 acts on all its island holds, a1's acts alone unknown to
    // it), and the ledger takes its start read: what either island holds alone from here on, or
    // makes, must stay there.
    if split::land(held_from).is_err() {
        let _ = lab_cut.set(false);
        k.skipped = Some("interrupted while the split's in-flight exchanges landed".to_owned());
        return k;
    }
    carry_among(&island_a, ev);
    carry_among(&island_b, ev);
    ledger.start = split::read(&watched, &servers, &mut signers);
    ledger.landed_ms = u64::try_from(held_from.elapsed().as_millis()).unwrap_or(u64::MAX);
    let mut remove = |on: &Mac, name: &str, code: &str| {
        let (ok, said) = match lab_cut.act(&on.name, &[REMOVE_WORD, code]) {
            Ok(n) => (true, format!("signed {n} records, kept on {}", on.name)),
            Err(e) => (false, e),
        };
        k.removals.push(Act {
            on: on.name.clone(),
            names: name.to_owned(),
            ok,
            said,
        });
    };

    // Island A: a1 removes b3 and a3, and a1's own records sync posts that to every server that
    // lacks its frontier: island A's takes it, and the cut refuses island B's.
    remove(a1, CUT, &cut_code);
    remove(a1, ALSO, &also_code);
    carry_among(&island_a, ev);
    sync_under_split(a1, act, &watched, &servers, &signers, &mut ledger);
    // Island B: b3, not yet told, removes a3 (its epoch), its records sync puts that on island
    // B's server, and b1 writes under that epoch.
    remove(cut, ALSO, &also_code);
    sync_under_split(cut, act, &watched, &servers, &signers, &mut ledger);
    carry_among(&island_b, ev);
    let put = lab_cut
        .act(WRITER, &[PUT_WORD])
        .and_then(|epoch| epoch_of(&epoch));
    k.put = Some(put.as_ref().map(|_| ()).map_err(Clone::clone));
    k.under_cut_epoch = put.as_ref().is_ok_and(|epoch| {
        log_of(&writer.paths)
            .and_then(|log| log.get(epoch).map(|r| *r.actor()))
            .is_some_and(|actor| actor == cut_actor)
    });
    let epoch = put.ok();
    carry_among(&island_b, ev);
    ledger.end = split::read(&watched, &servers, &mut signers);
    ledger.ms = u64::try_from(held_from.elapsed().as_millis()).unwrap_or(u64::MAX);

    // The heal: the cut lifts, then the tender's pass, each round further on, until no Mac
    // finds one due.
    if let Err(e) = lab_cut.set(false) {
        ledger.failed = Some(format!("the heal: {e}"));
    }
    k.ledger = Some(ledger);
    carry(macs, ev);
    let base = aterm_time::unix_millis()
        .unwrap_or(0)
        .saturating_add(TEND_AHEAD * GAP_STEP_MS);
    for round in 1..=TEND_ROUNDS {
        if !macs.iter().any(|m| rotation_due(&m.paths)) {
            break;
        }
        let steps = u64::try_from(round).unwrap_or(u64::MAX) * TEND_AHEAD;
        let at = base.saturating_add(steps.saturating_mul(GAP_STEP_MS));
        for m in macs {
            if let Ok(signed) = client::tend_epochs_at(&m.paths, at)
                && !signed.is_empty()
            {
                k.minted.push(format!(
                    "{} minted {} records in round {round}",
                    m.name,
                    signed.len()
                ));
                carry(macs, ev);
            }
        }
    }

    for m in macs {
        let mut end = KeptEnd {
            mac: m.name.clone(),
            ..KeptEnd::default()
        };
        if let (Some(log), Some(identity)) = (
            log_of(&m.paths),
            client::read_machine_identity(&m.paths).ok().flatten(),
        ) {
            end.has_log = true;
            let membership = log.membership();
            let me = identity.recipient();
            let active: std::collections::BTreeSet<Recipient> = membership
                .members()
                .values()
                .map(|member| member.binding.recipient())
                .collect();
            end.member = active.contains(&me);
            end.cut_member = membership.contains(&cut_actor);
            end.rotation_due = end.member && log.rotation_due(&me, &active);
            let keyed = log
                .keyed_epochs()
                .into_iter()
                .find(|keyed| Some(keyed.epoch) == epoch);
            end.keyed = keyed.is_some();
            end.covered = keyed.is_some_and(|keyed| keyed.covered);
            if let Ok(Some(opened)) = log.open_audience_entries(&identity) {
                end.reads_value = opened
                    .by_audience
                    .get(&Audience::Everyone)
                    .and_then(|entries| entries.get(KEPT_NAME))
                    .is_some_and(|entry| entry.value.as_slice() == KEPT_VALUE);
                let wraps_cut = opened
                    .maximal_epochs
                    .get(&aterm_secrets::membership_log::EpochAudience::Everyone)
                    .into_iter()
                    .flatten()
                    .any(|newest| {
                        opened
                            .wrapped_recipients
                            .get(newest)
                            .is_some_and(|set| set.contains(&cut_recipient))
                    });
                if wraps_cut {
                    k.wrapped_to_cut.push(m.name.clone());
                }
            }
        }
        k.ends.push(end);
    }
    k
}

/// The putter's `[everyone]` value: `client::test_put_everyone`, a test seam that only a build with
/// debug assertions has (`cfg(any(test, debug_assertions))`). A release lab (the trail galleries' measuring
/// build) has none, so its removals plane reports the put as not made rather than failing to compile.
#[cfg(any(test, debug_assertions))]
fn put_everyone(paths: &Paths) -> aterm_secrets::Result<RecordId> {
    client::test_put_everyone(paths, VALUE_NAME, VALUE)
}

#[cfg(not(any(test, debug_assertions)))]
fn put_everyone(_paths: &Paths) -> aterm_secrets::Result<RecordId> {
    Err(aterm_secrets::Error::new(
        "a release lab has no test seam to put the [everyone] value; run the removals plane from a debug lab",
    ))
}

/// b1's value under the one newest `[everyone]` epoch it holds: [`put_everyone`]'s seam.
#[cfg(any(test, debug_assertions))]
fn put_kept(paths: &Paths) -> aterm_secrets::Result<RecordId> {
    client::test_put_everyone(paths, KEPT_NAME, KEPT_VALUE)
}

#[cfg(not(any(test, debug_assertions)))]
fn put_kept(_paths: &Paths) -> aterm_secrets::Result<RecordId> {
    Err(aterm_secrets::Error::new(
        "a release lab has no test seam to put the [everyone] value; run the removals plane from a debug lab",
    ))
}

/// [`act`]'s word for a removal kept on its Mac, then the removed Mac's join code.
const REMOVE_WORD: &str = "remove-unposted";
/// [`act`]'s word for b1's put under the one newest `[everyone]` epoch it holds.
const PUT_WORD: &str = "put-kept";

/// One of F21's acts on the Mac whose vault paths are `paths`, made in this process: the lab's
/// `partition-act` verb runs it in a process of that Mac's world ([`split::Cut::act`]), which
/// its island's cut covers whole. `remove-unposted <code>` says how many records it signed,
/// `put-kept` the epoch it sealed under, in hex.
///
/// # Errors
/// An act this verb does not know, or the act failed: why.
pub fn act(paths: &Paths, words: &[String]) -> Result<String, String> {
    match words {
        [word, code] if word == REMOVE_WORD => remove_unposted(paths, code)
            .map(|n| n.to_string())
            .map_err(|e| e.to_string()),
        [word] if word == PUT_WORD => put_kept(paths)
            .map(|epoch| aterm_codec::hex::encode_lower(&epoch))
            .map_err(|e| e.to_string()),
        _ => Err(format!("usage: {REMOVE_WORD} <join code> | {PUT_WORD}")),
    }
}

/// The epoch `put-kept` printed ([`act`]).
fn epoch_of(hex: &str) -> Result<RecordId, String> {
    let mut epoch = [0u8; 32];
    (hex.len() == 64 && aterm_codec::hex::decode_into(hex.as_bytes(), &mut epoch))
        .then_some(epoch)
        .ok_or_else(|| format!("{PUT_WORD} printed no epoch: {hex}"))
}

/// A removal kept on the Mac at `paths` ([`client::test_remove_unposted`]), a test seam as
/// [`put_everyone`]'s is.
#[cfg(any(test, debug_assertions))]
fn remove_unposted(paths: &Paths, who: &str) -> aterm_secrets::Result<usize> {
    client::test_remove_unposted(paths, who)
}

#[cfg(not(any(test, debug_assertions)))]
fn remove_unposted(_paths: &Paths, _who: &str) -> aterm_secrets::Result<usize> {
    Err(aterm_secrets::Error::new(
        "a release lab has no test seam to remove a Mac on one side of a split; run the removals plane from a debug lab",
    ))
}

fn rotation_due(paths: &Paths) -> bool {
    let (Some(log), Some(me)) = (log_of(paths), recipient_of(paths)) else {
        return false;
    };
    let membership = log.membership();
    let active: std::collections::BTreeSet<Recipient> = membership
        .members()
        .values()
        .map(|m| m.binding.recipient())
        .collect();
    active.contains(&me) && log.rotation_due(&me, &active)
}

fn read_end(m: &Mac, remover: &MemberId) -> MacEnd {
    let mut end = MacEnd {
        mac: m.name.clone(),
        ..MacEnd::default()
    };
    let (Some(log), Some(identity)) = (
        log_of(&m.paths),
        client::read_machine_identity(&m.paths).ok().flatten(),
    ) else {
        return end;
    };
    end.has_log = true;
    let membership = log.membership();
    let me = identity.recipient();
    end.member = membership
        .members()
        .values()
        .any(|member| member.binding.recipient() == me);
    end.undone = membership.quorums().iter().any(|q| q.member == *remover);
    end.remover_out_by_quorum = membership.removal(remover).is_some_and(|r| r.quorum);
    end.rotation_due = end.member && {
        let active = membership
            .members()
            .values()
            .map(|member| member.binding.recipient())
            .collect();
        log.rotation_due(&me, &active)
    };
    end.reads_value = log
        .open_audience_entries(&identity)
        .ok()
        .flatten()
        .and_then(|opened| {
            opened
                .by_audience
                .get(&Audience::Everyone)
                .and_then(|entries| entries.get(VALUE_NAME))
                .map(|entry| entry.value.as_slice() == VALUE)
        })
        .unwrap_or(false);
    end
}

#[cfg(test)]
#[path = "swarm_partition_removals_tests.rs"]
mod tests;
