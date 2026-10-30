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

use std::time::Instant;

use aterm_secrets::age::Recipient;
use aterm_secrets::client::{self, Paths};
use aterm_secrets::membership_log::{Log, MemberId};
use aterm_secrets::text::Audience;

use super::commons::Mac;

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
    /// Milliseconds the plane took.
    pub ms: u64,
}

/// The plane's check that b2's removals were undone.
pub const UNDONE: &str = "removals: the Macs one member removed undo its removals by a quorum";
/// The plane's check named in the design: `removed_macs_that_reach_a_quorum_are_back_in_and_read_new_values`.
pub const BACK_IN: &str =
    "removals: removed Macs that reach a quorum are back in and read new values";

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
    out
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
    for f in &ev.carry_failures {
        let _ = writeln!(out, "- carrying records {f}");
    }
    out
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
/// CLI in that Mac's world.
pub fn run(macs: &[Mac], act: &dyn Fn(&str, &[&str]) -> super::world::Out) -> Evidence {
    let t = Instant::now();
    let mut ev = Evidence::default();
    drive(macs, act, &mut ev);
    ev.ms = u64::try_from(t.elapsed().as_millis()).unwrap_or(u64::MAX);
    ev
}

fn drive(macs: &[Mac], act: &dyn Fn(&str, &[&str]) -> super::world::Out, ev: &mut Evidence) {
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
    let put = mac(PUTTER).map(|m| client::test_put_everyone(&m.paths, VALUE_NAME, VALUE));
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
