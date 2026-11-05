// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `codex-alab-proof --facts-only` (gap 23): aterm's measured no-progress fact rides one act a
//! real codex-alab types anyway, once per stall (CONTRACT §5 "aterm fact"). The leg's model has
//! codex start two calls with no terminal: one takes a lock and stops itself holding it, the other
//! prints a fast rhythm into a file and then blocks behind that lock. The jobs engine reads the
//! second `no_progress` once φ reaches 8 over its own μ, about 3 min of quiet (`aterm-vitals`'
//! `a_codex_pipe_call_blocked_on_a_stopped_holders_lock_reads_no_progress_after_its_rhythm`).
//! Each of Cruise's continuations reads `harness fact`, so the first after that carries aterm's
//! rider and the next is told nothing. The leg reads passively, from codex-alab's journal of each
//! read and the model's requests. It never asks `sys jobs` while it measures, so it makes the
//! board no fresher than production would, and it takes one `sys jobs fresh=600000` read after.
//! If the engine never names the call, the leg ends UNOBSERVED, a finding about the jobs engine on
//! Codex's call shape, never a pass. This leg is not in the default run (gap 23 §12 step 4).

use aterm_agent::supervise::facts::{EpisodeKey, FACT_FRESH_MS};
use aterm_agent::supervise::facts_offer::{WHY_NONE, WHY_STALE};
use aterm_agent::supervise::policy::turn_end::{FACT_OPEN, fact_suffix};
use aterm_agent::supervise::{BackgroundCall, Fact};

use super::*;
use crate::codex_alab_model::{
    FACT_TAG, FACTS_HOLDER_PID, FACTS_TICK_COUNT, FACTS_TICK_MS, FACTS_TICKS, FACTS_WAITER_PID,
};

/// The leg's name, which its prompt and its model's answers carry.
const LEG: &str = "facts";

/// A wait for something that must happen: a hang detector, never a latency budget.
const HANG: Duration = Duration::from_secs(60);

/// How often the leg rereads its files (codex-alab's journal, the pid and ticks files).
const POLL: Duration = Duration::from_secs(1);

/// φ 8 over the 10 s floor μ, ms: the quiet at which the engine reads the waiter `no_progress`.
const PHI8_QUIET_MS: u64 = 184_208;

/// [`PHI8_QUIET_MS`] as a wait.
const PHI8_QUIET: Duration = Duration::from_millis(PHI8_QUIET_MS);

/// THE LATEST QUIET AT WHICH A READ MAY STILL ANSWER `none`, ms from the rhythm's end (the ticks
/// file's last write). φ 8's quiet, plus the age a board reading may have and still answer
/// ([`FACT_FRESH_MS`]; an older one answers `stale`), plus 3 s: the engine credits the rhythm's
/// last tick up to one 2 s tick after it, and codex-alab journals a read up to its round trip
/// after aterm judged it.
const NONE_UNTIL_MS: u64 = PHI8_QUIET_MS + FACT_FRESH_MS.unsigned_abs() + 3_000;

/// One codex-alab journal line about aterm's fact (its `Supervisor::fact_for` and `fact_told`,
/// and the act's `submit`).
#[derive(Clone, Debug, PartialEq, Eq)]
enum FactLine {
    /// A read that gave no fact: aterm's `why=` word, or the refusal's line, and the act's rule.
    Read { why: String, rule: String },
    /// The fact rode the act under `rule`: its episode.
    Rides { episode: String, rule: String },
    /// The act that carried it started and was recorded, and aterm took the ack.
    Told { episode: String },
    /// Any other line about a fact (left off, not told, an aterm without the verb): its words.
    Other(String),
}

/// Whether `word` is an episode as codex-alab journals one: 16 lowercase hex digits.
fn is_episode(word: &str) -> bool {
    word.len() == 16 && word.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Whether `word` is a rule id (`continue@v1`).
fn is_rule(word: &str) -> bool {
    word.split_once('@').is_some_and(|(name, v)| {
        !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && v.strip_prefix('v')
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    })
}

/// The fact line codex-alab's journal entry `rest` (what follows `codex_alab_autopilot: `) is, if
/// it is one.
fn fact_line(rest: &str) -> Option<FactLine> {
    if let Some(read) = rest.strip_prefix("fact: ") {
        let line = read
            .rsplit_once(" (")
            .and_then(|(why, rule)| Some((why, rule.strip_suffix(')')?)))
            .filter(|(why, rule)| !why.is_empty() && is_rule(rule))
            .map_or_else(
                || FactLine::Other(rest.to_string()),
                |(why, rule)| FactLine::Read {
                    why: why.to_string(),
                    rule: rule.to_string(),
                },
            );
        return Some(line);
    }
    if let Some(told) = rest.strip_prefix("fact ") {
        let words: Vec<&str> = told.split_whitespace().collect();
        return Some(match words.as_slice() {
            [_, episode, "told"] if is_episode(episode) => FactLine::Told {
                episode: (*episode).to_string(),
            },
            _ => FactLine::Other(rest.to_string()),
        });
    }
    if rest.contains(": fact: ") {
        return Some(FactLine::Other(rest.to_string()));
    }
    let (_, tail) = rest.split_once(": fact ")?;
    let words: Vec<&str> = tail.split_whitespace().collect();
    Some(match words.as_slice() {
        [_, episode, "rides", rule] if is_episode(episode) && is_rule(rule) => FactLine::Rides {
            episode: (*episode).to_string(),
            rule: (*rule).to_string(),
        },
        _ => FactLine::Other(rest.to_string()),
    })
}

/// Every fact line of codex-alab's journal, in order, each with when it was journaled: the
/// line's leading UTC stamp as unix ms (`None` where it has none that parses).
fn fact_lines(journal: &str) -> Vec<(Option<i64>, FactLine)> {
    journal
        .lines()
        .filter_map(|l| l.split_once("codex_alab_autopilot: "))
        .filter_map(|(head, rest)| {
            let at = head
                .split_whitespace()
                .next()
                .and_then(crate::workers::iso_ms);
            Some((at, fact_line(rest.trim_end())?))
        })
        .collect()
}

/// What the journal's fact lines say of the stall: the reads' answers before the first ride, each
/// with when it was journaled, the first ride, and the reads' answers after it.
#[derive(Debug, Default, PartialEq, Eq)]
struct Story {
    before: Vec<(String, Option<i64>)>,
    rides: Vec<(String, String)>,
    after: Vec<String>,
    told: Vec<String>,
    other: Vec<String>,
}

fn story(lines: &[(Option<i64>, FactLine)]) -> Story {
    let mut s = Story::default();
    for (at, line) in lines {
        match line {
            FactLine::Read { why, .. } if s.rides.is_empty() => s.before.push((why.clone(), *at)),
            FactLine::Read { why, .. } => s.after.push(why.clone()),
            FactLine::Rides { episode, rule } => s.rides.push((episode.clone(), rule.clone())),
            FactLine::Told { episode } => s.told.push(episode.clone()),
            FactLine::Other(words) => s.other.push(words.clone()),
        }
    }
    s
}

/// The reads before the ride, each as its answer and its quiet in seconds since `blocked_ms`
/// (`-` where its line had no stamp), for a check's label.
fn reads_at(before: &[(String, Option<i64>)], blocked_ms: i64) -> Vec<String> {
    before
        .iter()
        .map(|(why, at)| match at {
            Some(at) => format!("{why} at {}", secs(at - blocked_ms)),
            None => format!("{why} at -"),
        })
        .collect()
}

/// CHECK 1, PLACED IN TIME: the reads before the ride, each at its quiet since `blocked_ms` (the
/// rhythm's end, unix ms) by its journal stamp, hold iff there is at least one, every one is
/// stamped and answered `none` or `stale`, each `none` came before [`NONE_UNTIL_MS`] (from there
/// the engine has named the stall, so an act's read that gives no fact may only answer `stale`),
/// and at least one came before [`PHI8_QUIET_MS`]: a read taken before the stall could be named,
/// the leg's negative control.
fn reads_before_hold(before: &[(String, Option<i64>)], blocked_ms: i64) -> bool {
    let ms = |v: u64| i64::try_from(v).unwrap_or(i64::MAX);
    let quiet = |at: &Option<i64>| at.map(|at| at - blocked_ms);
    !before.is_empty()
        && before
            .iter()
            .all(|(why, at)| match (why.as_str(), quiet(at)) {
                (WHY_NONE, Some(q)) => q < ms(NONE_UNTIL_MS),
                (WHY_STALE, Some(_)) => true,
                _ => false,
            })
        && before
            .iter()
            .any(|(_, at)| quiet(at).is_some_and(|q| q < ms(PHI8_QUIET_MS)))
}

/// The rider at the end of a typed act's words: from its last [`FACT_OPEN`] on.
fn rider_of(user: &str) -> Option<&str> {
    user.rfind(FACT_OPEN).map(|at| &user[at..])
}

/// The pid and the whole minutes a no-progress rider names (`… (pid <pid>) has made no progress
/// for <n> min)`).
fn told_call(rider: &str) -> Option<(u32, u64)> {
    let (_, after) = rider.split_once("(pid ")?;
    let (pid, after) = after.split_once(')')?;
    let (_, after) = after.split_once("has made no progress for ")?;
    let (min, _) = after.split_once(" min")?;
    Some((pid.parse().ok()?, min.parse().ok()?))
}

/// aterm's rider for a call of the agent's, `pid`, that made no progress for `min` minutes, with
/// no task id and no named wait, as aterm's own `fact_suffix` and `BackgroundCall::told` spell it.
fn expected_rider(pid: u32, min: u64) -> String {
    let call = BackgroundCall {
        pid,
        pgid: pid,
        birth: None,
        exe: String::new(),
        task: None,
        quiet_ms: min * 60_000,
        wait: None,
    };
    fact_suffix(&Fact {
        key: EpisodeKey::no_progress(pid, 0),
        text: call.told(),
        as_of_ms: 0,
    })
}

/// The leaders of the agent calls a `sys jobs` reply reads `no_progress=1`.
fn no_progress_leaders(reply: &str) -> Vec<u32> {
    reply
        .lines()
        .filter(|l| l.starts_with("job ") && l.split_whitespace().any(|w| w == "no_progress=1"))
        .filter_map(|l| field(l, "id")?.strip_prefix('a')?.parse().ok())
        .collect()
}

/// One fact row of the session ledger (`fact-no-progress@v1`), as `aterm harness ledger --json`
/// lists it.
#[derive(Debug, PartialEq, Eq)]
struct LedgerFact {
    sid: String,
    decision: String,
    command: String,
    reason: String,
    holder: String,
}

/// The `fact-no-progress@v1` rows of `rows`.
fn ledger_facts(rows: &[J]) -> Vec<LedgerFact> {
    let s = |row: &J, k: &str| row.get(k).and_then(J::as_str).unwrap_or("").to_string();
    rows.iter()
        .filter(|row| row.get("rule_id").and_then(J::as_str) == Some("fact-no-progress@v1"))
        .map(|row| LedgerFact {
            sid: s(row, "sid").trim_start_matches('@').to_string(),
            decision: s(row, "decision"),
            command: s(row, "command"),
            reason: s(row, "reason"),
            holder: s(row, "holder"),
        })
        .collect()
}

/// Each `messages` row's title and detail, decoded, the detail's lines joined with nothing between
/// them: aterm splits a long harness note's whole text into detail lines on character
/// boundaries (`message_reporters::harness_note`), so a rider may straddle two of them.
fn message_texts(reply: &str) -> Vec<String> {
    reply
        .lines()
        .filter(|l| l.starts_with("message "))
        .map(|l| {
            format!(
                "{} {}",
                pct_decode(&field(l, "title").unwrap_or_default()),
                pct_decode(&field(l, "detail").unwrap_or_default())
                    .lines()
                    .collect::<String>()
            )
        })
        .collect()
}

/// A pid file's pid.
fn pid_in(path: &Path) -> Option<i32> {
    crate::util::read_regular_text(path)
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// How many ticks the waiter printed into its file.
fn ticks_in(path: &Path) -> usize {
    crate::util::read_regular_text(path)
        .map(|t| t.lines().filter(|l| l.starts_with("tick ")).count())
        .unwrap_or(0)
}

/// When the file at `path` was last written, unix ms: for the ticks file, the rhythm's last tick,
/// where the waiter's quiet starts.
fn written_ms(path: &Path) -> Option<i64> {
    let at = std::fs::symlink_metadata(path).ok()?.modified().ok()?;
    i64::try_from(at.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis()).ok()
}

/// The tool outputs the leg's requests carried, for a failed start's diagnosis.
fn tool_outputs(proof: &Proof) -> Vec<String> {
    proof
        .requests(LEG)
        .iter()
        .map(|r| str_of(r, "tool_output"))
        .filter(|o| !o.is_empty())
        .map(|o| crate::util::one_line(&o, 200))
        .collect()
}

/// The `--facts-only` lane: the package linked, the facts leg opened (its calls must really run,
/// so under the jail alone) and claimed, then [`facts_leg`].
pub(super) fn legs(proof: &mut Proof, package: &Path) -> Result<(), Fail> {
    proof.link(package).map_err(Fail::die)?;
    proof.inst.await_socket(instance::BOOT)?;
    let _ = proof.inst.ctl(&["ready"]);
    println!(
        "facts-only (real Codex TUI under Cruise, one tab: a call stalled behind a stopped holder's lock)"
    );
    let leg = proof.open_leg(LEG)?;
    let pid = proof.claim_with_limit(&leg, HANG)?;
    facts_leg(proof, &leg, pid)?;
    Ok(())
}

/// THE FACTS LEG (gap 23 §11): its two calls run, then it waits, passively, for the act that
/// carries aterm's fact and the one after it, and checks the five things §11 states. It kills the
/// two calls by pid at its end.
pub(super) fn facts_leg(proof: &mut Proof, leg: &Leg, pid: Option<i32>) -> Result<(), Stopped> {
    let began = Instant::now();
    let measured = measure(proof, leg, pid);
    let pids = [FACTS_HOLDER_PID, FACTS_WAITER_PID].map(|f| pid_in(&leg.proj.join(f)));
    for p in pids.into_iter().flatten() {
        let _ = crate::sys::kill(p, libc::SIGKILL);
    }
    // Codex reaps its calls only when it next polls their sessions, so a killed call may stay a
    // zombie: dead, which is all the kill must prove.
    let dead = Proof::wait_for(HANG, POLL, || {
        pids.into_iter()
            .flatten()
            .all(|p| !crate::procs::alive(p) || crate::procs::zombie(p))
    });
    println!(
        "  facts: holder {} and waiter {} killed by pid ({}); {:.0} s on this leg",
        pids[0].map_or_else(|| "-".to_string(), |p| p.to_string()),
        pids[1].map_or_else(|| "-".to_string(), |p| p.to_string()),
        if matches!(dead, Ok(true)) {
            "dead"
        } else {
            "NOT dead"
        },
        began.elapsed().as_secs_f64()
    );
    measured.and(dead.map(|_| ()))
}

/// The leg's measurement: everything but the kill.
fn measure(proof: &mut Proof, leg: &Leg, pid: Option<i32>) -> Result<(), Stopped> {
    let proj = &leg.proj;
    let (mut holder, mut waiter, mut ticks) = (None, None, 0);
    let started = Proof::wait_for(
        HANG + Duration::from_millis(FACTS_TICK_MS * FACTS_TICK_COUNT as u64),
        POLL,
        || {
            holder = pid_in(&proj.join(FACTS_HOLDER_PID));
            waiter = pid_in(&proj.join(FACTS_WAITER_PID));
            ticks = ticks_in(&proj.join(FACTS_TICKS));
            holder.is_some() && waiter.is_some() && ticks >= FACTS_TICK_COUNT
        },
    )?;
    // The rhythm's end, as near as one look a second tells it: the stall's quiet starts here. Its
    // unix ms, which places each read in time, is the ticks file's last write, the last tick
    // itself (the look's own time, a second later at most, where that cannot be read).
    let blocked = Instant::now();
    let blocked_ms = written_ms(&proj.join(FACTS_TICKS)).unwrap_or_else(crate::util::now_ms);
    let held = holder.and_then(crate::procs::state_of).unwrap_or_default();
    let waits = waiter.and_then(crate::procs::state_of).unwrap_or_default();
    let ran = started && held.starts_with('T') && waits.starts_with('S');
    proof.check(
        &format!(
            "facts: the holder's call took the lock and stopped itself (pid {}, state {held:?}); the waiter's printed {ticks} of {FACTS_TICK_COUNT} ticks into {FACTS_TICKS} and blocks behind it (pid {}, state {waits:?})",
            holder.map_or_else(|| "-".to_string(), |p| p.to_string()),
            waiter.map_or_else(|| "-".to_string(), |p| p.to_string()),
        ),
        ran,
    );
    if !ran {
        println!("  facts: the calls' outputs: {:?}", tool_outputs(proof));
        return Ok(());
    }

    // Reads come only at acts, one a turn (SLOW_TURN): past φ 8, the act in flight ends within a
    // turn, and one more is allowed for a read whose view was too old to tell from.
    let mut lines = Vec::new();
    let rode = Proof::wait_for(PHI8_QUIET + 2 * SLOW_TURN + HANG, POLL, || {
        lines = fact_lines(&Proof::codex_log(leg));
        lines
            .iter()
            .any(|(_, l)| matches!(l, FactLine::Rides { .. }))
    })?;
    let rode_after = blocked.elapsed();
    if !rode {
        let jobs = proof.on(&leg.sid, &["sys", "jobs", "scope=tab", "fresh=600000"]);
        let named = no_progress_leaders(&jobs.stdout);
        let s = story(&lines);
        if named.is_empty() {
            proof.unobserved("facts: the jobs engine named no codex call stuck");
        } else {
            proof.check(
                &format!(
                    "facts: the engine named the call (no_progress=1 on {named:?}) and an act carried its fact (reads {:?}; other {:?})",
                    reads_at(&s.before, blocked_ms),
                    s.other
                ),
                false,
            );
        }
        for row in jobs.stdout.lines() {
            println!("  facts: sys jobs {}", crate::util::one_line(row, 240));
        }
        println!(
            "  facts: the reads said {:?} (other {:?})",
            reads_at(&s.before, blocked_ms),
            s.other
        );
        println!("  model {} {}", leg.name, proof.tags(leg.name).join(" "));
        return Ok(());
    }

    // The act that carried it, then the next one and its read.
    let ridden = |p: &Proof| {
        p.requests(LEG)
            .iter()
            .position(|r| str_of(r, "tag").contains(FACT_TAG))
    };
    let mut at = None;
    Proof::wait_for(HANG, POLL, || {
        at = ridden(proof);
        at.is_some()
    })?;
    let next_continued = |p: &Proof| {
        at.is_some_and(|i| {
            p.requests(LEG)
                .iter()
                .skip(i + 1)
                .any(|r| str_of(r, "tag").starts_with("facts:continue"))
        })
    };
    let next = Proof::wait_for(SLOW_TURN + HANG, POLL, || next_continued(proof))?;
    Proof::wait_for(HANG, POLL, || {
        lines = fact_lines(&Proof::codex_log(leg));
        !story(&lines).after.is_empty()
    })?;
    let s = story(&lines);
    let rows = proof.requests(LEG);
    let tags: Vec<String> = rows.iter().map(|r| str_of(r, "tag")).collect();
    let carried: Vec<usize> = (0..rows.len())
        .filter(|&i| tags[i].contains(FACT_TAG))
        .collect();
    let continued_before = at.map_or(0, |i| {
        tags[..i]
            .iter()
            .filter(|t| t.starts_with("facts:continue"))
            .count()
    });

    // 1. No fact before the engine names the stall, and the first act after it carries it: each
    // read placed at its quiet since the rhythm's end.
    proof.check(
        &format!(
            "facts: before the engine named the stall, every read answered none or stale and no act carried a fact; a none only before {} of quiet, one read before φ 8's {} (reads {:?}; {continued_before} continuation(s) before the one that carried it)",
            secs(i64::try_from(NONE_UNTIL_MS).unwrap_or(i64::MAX)),
            secs(i64::try_from(PHI8_QUIET_MS).unwrap_or(i64::MAX)),
            reads_at(&s.before, blocked_ms)
        ),
        reads_before_hold(&s.before, blocked_ms) && continued_before >= 1,
    );

    // 2. One act carried it, aterm's rider for the waiter, verbatim.
    let jobs = proof.on(&leg.sid, &["sys", "jobs", "scope=tab", "fresh=600000"]);
    let named = no_progress_leaders(&jobs.stdout);
    for row in jobs.stdout.lines().filter(|l| l.starts_with("job ")) {
        println!("  facts: sys jobs {}", crate::util::one_line(row, 240));
    }
    let user = at.map_or_else(String::new, |i| str_of(&rows[i], "u_full"));
    let rider = rider_of(&user).unwrap_or_default().to_string();
    let call = told_call(&rider);
    let leader = named.first().copied();
    let verbatim = call.is_some_and(|(p, min)| {
        Some(p) == leader && i32::try_from(p).ok() == waiter && rider == expected_rider(p, min)
    });
    let rule_rode = s.rides.first().map(|(_, r)| r.as_str()).unwrap_or("-");
    proof.check(
        &format!(
            "facts: exactly one act carried a fact, the one journaled `rides` ({rule_rode}), its rider aterm's fact_suffix for the waiter (sys jobs no_progress=1 on {named:?}, waiter pid {}): {rider:?}",
            waiter.map_or_else(|| "-".to_string(), |p| p.to_string()),
        ),
        carried.len() == 1 && s.rides.len() == 1 && rule_rode == "continue@v1" && verbatim,
    );

    // 3. Once per episode: the next act carries none, its read told.
    let episode = s.rides.first().map(|(e, _)| e.clone()).unwrap_or_default();
    let after_ok = next
        && at.is_some_and(|i| {
            tags.iter()
                .skip(i + 1)
                .find(|t| t.starts_with("facts:continue"))
                .is_some_and(|t| !t.contains(FACT_TAG))
        });
    proof.check(
        &format!(
            "facts: the next continuation carries no fact and its read answered told (reads after {:?}; told {:?})",
            s.after, s.told
        ),
        after_ok && s.after.first().map(String::as_str) == Some("told") && s.told == [episode.clone()],
    );

    // 4. The holder's told row.
    let ledger = session_ledger(proof, &leg.sid);
    let facts = ledger_facts(&ledger);
    let holder_word = pid.map_or_else(String::new, |p| format!("codex-alab@{p}"));
    let sentence = rider
        .strip_prefix(FACT_OPEN)
        .and_then(|r| r.strip_suffix(')'))
        .unwrap_or_default();
    let booked = facts.len() == 1
        && facts.iter().all(|f| {
            f.sid == leg.sid
                && f.decision == "typed"
                && f.reason.split_whitespace().any(|w| w == "door=turn-end")
                && f.reason
                    .split_whitespace()
                    .any(|w| w == format!("episode={episode}"))
                && f.holder == holder_word
                && f.command == sentence
        });
    proof.check(
        &format!(
            "facts: the session ledger holds one fact-no-progress@v1 typed door=turn-end row for this sid, the episode that rode and holder={holder_word} ({facts:?})"
        ),
        booked,
    );

    // 5. One Messages entry carries the rider (codex-alab's act record); no stuck line.
    let messages = proof.inst.ctl(&["messages", "512"]);
    let texts = message_texts(&messages.stdout);
    let carrying = texts
        .iter()
        .filter(|t| !rider.is_empty() && t.contains(&rider))
        .count();
    let stuck = texts
        .iter()
        .filter(|t| t.contains("[aterm harness] aterm:"))
        .count();
    proof.check(
        &format!(
            "facts: Messages hold one entry carrying the rider (codex-alab's act record) and no `[aterm harness] aterm:` stuck line ({carrying} carrying, {stuck} stuck, of {})",
            texts.len()
        ),
        messages.code == 0 && carrying == 1 && stuck == 0,
    );
    let acted = format!("codex-alab @{}: ", leg.sid);
    for text in texts.iter().filter(|t| t.contains(&acted)) {
        println!("  facts: message {}", crate::util::one_line(text, 400));
    }

    let t_of = |i: usize| int_of(&rows[i], "t");
    let waiter_out = rows
        .iter()
        .position(|r| str_of(r, "tag") == "facts:person")
        .map(t_of);
    if let (Some(from), Some(i)) = (waiter_out, at) {
        println!(
            "  facts: the waiter's output reached the model, then the fact rode {} later; the stall's quiet {} s at that act (the rhythm's end seen {:.0} s before `rides` was read)",
            secs(t_of(i) - from),
            call.map_or(0, |(_, min)| min) * 60,
            rode_after.as_secs_f64()
        );
    }
    println!(
        "  facts: reads before {:?}, after {:?}",
        reads_at(&s.before, blocked_ms),
        s.after
    );
    println!("  facts: other fact lines {:?}", s.other);
    println!("  facts: rider {rider:?}");
    println!(
        "  facts: codex-alab@{}",
        pid.map_or_else(|| "-".to_string(), |p| p.to_string())
    );
    println!("  acts {} {}", leg.name, proof.acts(&leg.sid).join(" "));
    println!("  model {} {}", leg.name, tags.join(" "));
    Ok(())
}

/// The session ledger of `sid` (`aterm harness ledger @<sid> 4096 --json`), as the upgrade leg
/// reads its own: one row a line.
fn session_ledger(proof: &Proof, sid: &str) -> Vec<J> {
    let out = aterm_uds::spawnfd::command(&proof.inst.aterm)
        .env_clear()
        .envs(
            proof
                .world_env
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str())),
        )
        .args(["harness", "ledger", &format!("@{sid}"), "4096", "--json"])
        .stdin(Stdio::null())
        .output();
    out.map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
        .lines()
        .filter_map(|l| J::parse(l).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEX: &str = "00c0ffee0123abcd";

    fn journal(lines: &[&str]) -> String {
        lines
            .iter()
            .map(|l| format!("2026-11-04T21:00:00.000000Z DEBUG codex_alab_autopilot: {l}\n"))
            .collect()
    }

    /// CODEX-ALAB'S FACT LINES READ AS IT JOURNALS THEM (its `Supervisor::fact_for`, `fact_told`
    /// and `submit`, a6430a662d): a read's `why`, a ride and its ack in order, any other fact line
    /// kept apart, and no other journal line. NEGATIVE CONTROLS: an episode of 15 digits or in
    /// capitals, a rule that is no rule id, and the no-terminal line (`<key>: fact: …`) are no
    /// read, ride or ack.
    #[test]
    fn codex_alab_s_fact_lines_tell_the_reads_the_ride_and_its_ack() {
        let log = journal(&[
            "fact: none (continue@v1)",
            "fact: stale (continue@v1)",
            "t1: act continue@v1: typed",
            &format!("t1: fact no-progress {HEX} rides continue@v1"),
            &format!("fact no-progress {HEX} told"),
            "fact: told (continue@v1)",
            "fact: ERR denied (continue@v1)",
            "t1: fact: no background terminal beside another root thread (continue@v1)",
            "fact: aterm 0.166.0 has no harness fact (ERR usage: harness policy | recovery); not asked again under this claim",
            &format!(
                "t1: fact no-progress {HEX} gone stale before the start: continue@v1 typed without it"
            ),
            &format!("fact no-progress {HEX} not told: Refused(\"ERR stale fact\")"),
            "fact: none (not a rule)",
            &format!("t1: fact no-progress {} rides continue@v1", &HEX[1..]),
            &format!(
                "t1: fact no-progress {} rides continue@v1",
                HEX.to_uppercase()
            ),
        ]);
        let lines = fact_lines(&log);
        let s = story(&lines);
        let at = crate::workers::iso_ms("2026-11-04T21:00:00.000Z");
        assert!(at.is_some());
        assert_eq!(
            s.before,
            [("none".to_string(), at), ("stale".to_string(), at)]
        );
        assert_eq!(s.rides, [(HEX.to_string(), "continue@v1".to_string())]);
        assert_eq!(s.told, [HEX]);
        assert_eq!(s.after, ["told", "ERR denied"]);
        assert_eq!(s.other.len(), 7, "{:?}", s.other);
        assert!(fact_lines(&journal(&["t1: act continue@v1: typed"])).is_empty());
    }

    /// Journal lines as codex-alab writes them, each at its own UTC stamp.
    fn stamped(lines: &[(&str, &str)]) -> String {
        lines
            .iter()
            .map(|(at, l)| format!("{at} DEBUG codex_alab_autopilot: {l}\n"))
            .collect()
    }

    /// CHECK 1 IN TIME, OVER RECORDED ROWS. L3's (`docs/measured/codex-alab-facts-2026-11-04.md`:
    /// a `none` 116 s into the stall, then the ride at 246 s) hold. The late ride's do not: a
    /// second act read `none` at 246 s, past φ 8's 184 s plus the board's 5 s and 3 s of slack,
    /// and only the third carried the fact. NEGATIVE CONTROLS: a late read that answered `stale`
    /// holds; a first read already past φ 8 (no read before the stall could be named), an
    /// unstamped read, an answer other than `none` or `stale`, and no read at all do not.
    #[test]
    fn check_1_holds_l3_s_reads_and_refuses_a_late_ride() {
        let blocked = crate::workers::iso_ms("2026-11-04T22:22:52.500Z").expect("a stamp");
        let reads = |rows: &[(&str, &str)]| story(&fact_lines(&stamped(rows))).before;
        let none = "fact: none (continue@v1)";
        let stale = "fact: stale (continue@v1)";
        let rides = "01a1a8a8-2b04-7e42-9ab1-95d63e47f9f6: fact no-progress e51ba952cba02aca rides continue@v1";
        let (first, second, third) = (
            "2026-11-04T22:24:48.194454Z",
            "2026-11-04T22:26:58.219119Z",
            "2026-11-04T22:29:08.251521Z",
        );
        let l3 = reads(&[
            (first, none),
            (second, rides),
            (
                "2026-11-04T22:26:58.225327Z",
                "fact no-progress e51ba952cba02aca told",
            ),
            (third, "fact: told (continue@v1)"),
        ]);
        assert_eq!(reads_at(&l3, blocked), ["none at 115.7 s"]);
        assert!(reads_before_hold(&l3, blocked));
        let late = reads(&[(first, none), (second, none), (third, rides)]);
        assert_eq!(
            reads_at(&late, blocked),
            ["none at 115.7 s", "none at 245.7 s"]
        );
        assert!(!reads_before_hold(&late, blocked));
        assert!(reads_before_hold(
            &reads(&[(first, none), (second, stale), (third, rides)]),
            blocked
        ));
        assert!(!reads_before_hold(
            &reads(&[(second, stale), (third, rides)]),
            blocked
        ));
        let unstamped = reads(&[("-", none), (third, rides)]);
        assert_eq!(unstamped, [("none".to_string(), None)]);
        assert!(!reads_before_hold(&unstamped, blocked));
        assert!(!reads_before_hold(
            &reads(&[(first, "fact: busy (continue@v1)"), (third, rides)]),
            blocked
        ));
        assert!(!reads_before_hold(&reads(&[(third, rides)]), blocked));
    }

    /// THE RIDER IS ATERM'S OWN WORDS: the rider at the end of the act, the pid and minutes it
    /// names, and aterm's `fact_suffix` of `BackgroundCall::told` for them agree. NEGATIVE
    /// CONTROLS: a rider whose frame, pid or minutes differ is not the expected one, and words
    /// with no rider have none.
    #[test]
    fn the_expected_rider_is_aterms_fact_suffix_for_the_call() {
        let rider = " (aterm: a background call (pid 4200) has made no progress for 4 min)";
        let act = format!("keep going (standing rules: be brief){rider}");
        assert_eq!(rider_of(&act), Some(rider));
        assert_eq!(told_call(rider), Some((4200, 4)));
        assert_eq!(expected_rider(4200, 4), rider);
        assert_ne!(expected_rider(4201, 4), rider);
        assert_ne!(expected_rider(4200, 5), rider);
        assert_ne!(
            expected_rider(4200, 4),
            rider.replace(" (aterm: ", " (aterm said: ")
        );
        assert_eq!(rider_of("keep going (standing rules: be brief)"), None);
        assert_eq!(
            told_call(" (aterm: a background call has made no progress)"),
            None
        );
    }

    /// `sys jobs` names the agent calls it reads `no_progress=1` by their leader. NEGATIVE
    /// CONTROLS: a quiet call without the word, a shell job and a finding are none.
    #[test]
    fn sys_jobs_names_the_no_progress_calls_by_their_leader() {
        let reply = "OK 4 sys v=1 view=jobs as_of=1 age_ms=10 window_ms=0 next=1.2\n\
             job id=a4200 kind=agent-call consumer=agent pgid=4200 exe=perl class=waiting wait=unnamed quiet_ms=190000 phi=8.25 mu_ms=10000 mu_from=own verdict=quiet no_progress=1\n\
             job id=a4100 kind=agent-call consumer=agent pgid=4100 exe=perl class=stopped stop=held stopped_pid=4100:perl quiet_ms=220000 phi=1.59 verdict=fine\n\
             job id=b5 kind=shell consumer=person-fg exe=codex class=running verdict=fine\n\
             finding kind=stopped pid=4100 exe=perl\n";
        assert_eq!(no_progress_leaders(reply), [4200]);
        assert!(no_progress_leaders(&reply.replace(" no_progress=1", "")).is_empty());
    }

    /// The ledger's fact rows are read with their holder, the sid without its `@`. NEGATIVE
    /// CONTROL: a self-claim's act row is no fact row.
    #[test]
    fn the_ledger_s_fact_rows_carry_their_holder() {
        let rows = [
            r#"{"ts":1,"sid":"@s-1","rule_id":"fact-no-progress@v1","decision":"typed","command":"a background call (pid 4200) has made no progress for 4 min","reason":"kind=no-progress episode=00c0ffee0123abcd door=turn-end","holder":"codex-alab@77"}"#,
            r#"{"ts":2,"sid":"s-1","rule_id":"self-claim-act@v1","decision":"typed","command":"","holder":"codex-alab@77"}"#,
        ]
        .map(|r| J::parse(r).expect("a row"));
        assert_eq!(
            ledger_facts(&rows),
            [LedgerFact {
                sid: "s-1".to_string(),
                decision: "typed".to_string(),
                command: "a background call (pid 4200) has made no progress for 4 min".to_string(),
                reason: format!("kind=no-progress episode={HEX} door=turn-end"),
                holder: "codex-alab@77".to_string(),
            }]
        );
    }

    /// Messages rows are read by their decoded title and detail, the detail's lines rejoined, so a
    /// rider split across two of them reads whole. NEGATIVE CONTROL: the header is no row.
    #[test]
    fn messages_rows_are_read_by_their_decoded_words() {
        let reply = "OK 1 messages\n\
             message 7 at=1 ago_ms=2 tag=harness sev=info origin=wire state=live glass=- rep=1 key=- title=codex-alab%20%405 detail=typed%20%22keep%20going%20(ate%0Arm%3A%20x)%22 actions=-\n";
        assert_eq!(
            message_texts(reply),
            ["codex-alab @5 typed \"keep going (aterm: x)\""]
        );
    }
}
