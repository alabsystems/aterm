// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `@<sid> harness fact for=<rule> reader=<id> kinds=<k> wall=<w>` and `@<sid> harness fact told
//! episode=<hex>` (gap 23; `docs/DESIGN-system-awareness-2026-10-16.md` §14.4, "A
//! self-supervising agent's acts"): the session's standing proven codex-alab self-claim reads the
//! untold measured fact an act it types anyway may carry, and acknowledges it once that act
//! started. aterm makes every judgment: which acts a fact rides (`why=rule`), the limit's place
//! (`why=limit`), the kinds served (`why=kinds`), freshness (`why=stale`), once per episode
//! (`why=told`), the lease (`why=offered`) and the owner's policy (`why=off`, `why=facts-off`),
//! all in the session's [`crate::session_timeline::SessionMeta::self_facts`]. The read fails
//! closed like every call on the claim: with no candidate it answers without a main-thread hop,
//! and with one its hop is bounded by [`FACT_HOP`] (`why=busy` past it).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use aterm_agent::supervise::Fact;
use aterm_agent::supervise::approvals;
use aterm_agent::supervise::facts::FactKind;
use aterm_agent::supervise::facts_offer::{self, Pick, SERVED_KINDS};
use aterm_agent::supervise::occupancy::door::{self, FactDoor};
use aterm_agent::supervise::policy::turn_end::{FACT_RIDES, fact_suffix};

use crate::control::pct_encode;
use crate::self_claim::Proven;
use crate::session_timeline::SessionMeta;

/// How long the policy hop may take before the read answers `why=busy`: well inside codex-alab's
/// 5 s call deadline, so a modal dialog or a stalled main thread never costs the claim.
pub(crate) const FACT_HOP: Duration = Duration::from_millis(1_500);

/// The walls whose resume waits on a reset or a quota, as codex-alab names them (`wall=`): the
/// two aterm's `named_class` names `Usage` and `Model`, where an act of a limit's place is the
/// limit's own try and carries no fact.
const LIMIT_WALLS: [&str; 2] = ["usage", "entitlement"];

/// The longest `reader=` token.
const READER_MAX: usize = 64;

/// The longest `wall=` word.
const WALL_MAX: usize = 32;

/// `why=`: the act is none a fact rides ([`FACT_RIDES`]).
pub(crate) const WHY_RULE: &str = "rule";
/// `why=`: the act stands in a limit's place ([`facts_offer::limits_place_rule`]).
pub(crate) const WHY_LIMIT: &str = "limit";
/// `why=`: `kinds=` names no kind served ([`SERVED_KINDS`]).
pub(crate) const WHY_KINDS: &str = "kinds";
/// `why=`: the main thread did not answer the policy hop within [`FACT_HOP`].
pub(crate) const WHY_BUSY: &str = "busy";
/// `why=`: the policy is not in effect for this session.
pub(crate) const WHY_OFF: &str = "off";
/// `why=`: the owner's `[harness] facts` is off.
pub(crate) const WHY_FACTS_OFF: &str = "facts-off";

const DENIED: &str = "ERR denied\n";
const STALE_ACK: &str = "ERR stale fact\n";

/// The live readers the verb takes (tests pass their own).
pub(crate) struct Reads<'a> {
    /// The jobs board the session's calls are read from.
    pub(crate) board: &'a crate::vitals_jobs::JobsBoard,
    /// Whether a call's leader still lives at its birth ([`crate::vitals_jobs::leader_lives`]).
    pub(crate) lives: fn(u32, u64) -> bool,
    /// The state directory the session ledger sits under.
    pub(crate) state_dir: &'a dyn Fn() -> Option<PathBuf>,
    /// One sample of both clocks: the monotonic one the board ages its view by, and unix ms.
    pub(crate) clock: &'a dyn Fn() -> (Instant, i64),
}

/// A request the verb serves.
#[derive(Debug, PartialEq, Eq)]
enum Request<'a> {
    /// `fact for=<rule> reader=<id> kinds=<k>[,<k>…] wall=<w>`.
    Read {
        rule: &'a str,
        reader: &'a str,
        kinds: Vec<FactKind>,
        wall: &'a str,
    },
    /// `fact told episode=<016x>`.
    Told { id: u64 },
}

/// `<rule>`: `[a-z0-9-]+@v[0-9]+`.
fn rule_word(rule: &str) -> bool {
    rule.split_once("@v").is_some_and(|(name, version)| {
        !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !version.is_empty()
            && version.bytes().all(|b| b.is_ascii_digit())
    })
}

/// `<id>`: 1 to [`READER_MAX`] characters of `[A-Za-z0-9_-]`.
fn reader_token(reader: &str) -> bool {
    (1..=READER_MAX).contains(&reader.len())
        && reader
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// `<w>`: `-`, or 1 to [`WALL_MAX`] characters of `[a-z0-9-]`.
fn wall_word(wall: &str) -> bool {
    wall == "-"
        || ((1..=WALL_MAX).contains(&wall.len())
            && wall
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'))
}

/// The request `rest` spells, read strictly: each key of the read exactly once, in any order;
/// the ack's episode exactly 16 lowercase hex digits. `None` for anything else.
fn parse(rest: &str) -> Option<Request<'_>> {
    let mut toks = rest.split_whitespace();
    if toks.next() != Some("fact") {
        return None;
    }
    let toks: Vec<&str> = toks.collect();
    if let ["told", episode] = toks.as_slice() {
        let hex = episode.strip_prefix("episode=")?;
        let lower = hex.len() == 16
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        return lower
            .then(|| u64::from_str_radix(hex, 16).ok())
            .flatten()
            .map(|id| Request::Told { id });
    }
    let (mut rule, mut reader, mut kinds, mut wall) = (None, None, None, None);
    for tok in toks {
        let (key, value) = tok.split_once('=')?;
        let slot = match key {
            "for" => &mut rule,
            "reader" => &mut reader,
            "kinds" => &mut kinds,
            "wall" => &mut wall,
            _ => return None,
        };
        if slot.replace(value).is_some() {
            return None;
        }
    }
    let (rule, reader, kinds, wall) = (rule?, reader?, kinds?, wall?);
    if !rule_word(rule) || !reader_token(reader) || !wall_word(wall) {
        return None;
    }
    let kinds = kinds
        .split(',')
        .map(FactKind::of_word)
        .collect::<Option<Vec<_>>>()?;
    Some(Request::Read {
        rule,
        reader,
        kinds,
        wall,
    })
}

/// `OK fact=- why=<w>`.
fn why(w: &str) -> String {
    format!("OK fact=- why={w}\n")
}

/// The fact's line: its sentence, kind, episode, reading time and rider ([`fact_suffix`], the
/// words aterm's own loop adds to the act it rides), the text values percent-encoded.
fn offered(fact: &Fact) -> String {
    format!(
        "OK fact={} kind={} episode={:016x} as_of_ms={} rider={}\n",
        pct_encode(&fact.text),
        fact.key.kind.word(),
        fact.key.id,
        fact.as_of_ms,
        pct_encode(&fact_suffix(fact))
    )
}

/// `write` under the session's standing proven claim ([`crate::control::self_claim_write`])
/// where its holder is a codex-alab one ([`crate::self_claim::holder_pid`]); `None` otherwise,
/// with nothing written. A contract-3 rostered harness's claim is refused until its own
/// amendment: aterm's loop on such a tab resumes when that claim ends and never reads this
/// account, so serving it would tell an episode twice.
fn as_claim<T>(
    ctx: &crate::SessionCtx,
    conn: Option<u64>,
    prove: &dyn Fn(&str) -> Option<Proven>,
    write: impl FnOnce(&mut SessionMeta, &Proven) -> T,
) -> Option<(String, T)> {
    let (holder, out) = crate::control::self_claim_write(ctx, conn, prove, |meta, proven| {
        let codex = meta
            .live_claim(crate::metrics::now_us())
            .is_some_and(|claim| crate::self_claim::holder_pid(&claim.holder).is_some());
        codex.then(|| write(meta, proven))
    })?;
    Some((holder, out?))
}

/// The session ledger's text, for the account's one seed: empty where there is no state
/// directory or no ledger yet; an error where one stands that cannot be read, which seeds
/// nothing, so a told episode is never offered again for want of its row.
fn ledger_body(state_dir: &dyn Fn() -> Option<PathBuf>, sid: &str) -> Result<String, String> {
    let Some(state) = state_dir() else {
        return Ok(String::new());
    };
    approvals::read_ledger_text(&approvals::ledger_under(&state, Some(sid)))
        .map(Option::unwrap_or_default)
        .map_err(|e| format!("ERR the session ledger is unreadable: {e}\n"))
}

/// The verb's control-thread half: `fact for=… reader=… kinds=… wall=…` and `fact told
/// episode=…` (any other form is the `harness` usage line), Owner scope only and only from the
/// session's standing proven codex-alab claim (on its connection `conn`, or one whose process
/// `prove` proves), else `ERR denied` with the account untouched. `policy` is the main-thread hop
/// for the session's policy, asked only where a candidate stands.
pub(crate) fn cmd(
    ctx: &crate::SessionCtx,
    rest: &str,
    owner: bool,
    conn: Option<u64>,
    prove: &dyn Fn(&str) -> Option<Proven>,
    reads: Reads<'_>,
    policy: impl FnOnce() -> Result<crate::harness_policy_verb::PolicyFacts, String>,
) -> String {
    let Some(request) = parse(rest) else {
        return crate::harness_recovery_verb::USAGE.to_string();
    };
    if !owner {
        return DENIED.to_string();
    }
    match request {
        Request::Read {
            rule,
            reader,
            kinds,
            wall,
        } => read(
            ctx,
            conn,
            prove,
            &reads,
            policy,
            (rule, reader, &kinds, wall),
        ),
        Request::Told { id } => told(ctx, conn, prove, &reads, id),
    }
}

/// THE READ: each `why=` answering the first that holds. From the request alone, the rule, the
/// limit's place and the kinds; then, outside every lock, the board's reading (each leader
/// re-proved alive) and, for an unseeded account, the session ledger; under the claim's write,
/// the seed, the observe and the pick, leasing nothing; only with a candidate, the policy hop;
/// under the claim's write again, the pick again and the offer leased to (claim, `reader`).
fn read(
    ctx: &crate::SessionCtx,
    conn: Option<u64>,
    prove: &dyn Fn(&str) -> Option<Proven>,
    reads: &Reads<'_>,
    policy: impl FnOnce() -> Result<crate::harness_policy_verb::PolicyFacts, String>,
    (rule, reader, kinds, wall): (&str, &str, &[FactKind], &str),
) -> String {
    let sid = ctx.self_id.as_str();
    let Some((_, seeded)) = as_claim(ctx, conn, prove, |meta, _| meta.self_facts.seeded()) else {
        return DENIED.to_string();
    };
    if !FACT_RIDES.contains(&rule) {
        return why(WHY_RULE);
    }
    if facts_offer::limits_place_rule(rule) && LIMIT_WALLS.contains(&wall) {
        return why(WHY_LIMIT);
    }
    if !kinds.iter().any(|kind| SERVED_KINDS.contains(kind)) {
        return why(WHY_KINDS);
    }
    let (now, now_ms) = (reads.clock)();
    let reading = crate::vitals_jobs::facts_reading(reads.board, reads.lives, sid, now, now_ms);
    let body = if seeded {
        None
    } else {
        match ledger_body(reads.state_dir, sid) {
            Ok(body) => Some(body),
            Err(refused) => return refused,
        }
    };
    let Some((_, first)) = as_claim(ctx, conn, prove, |meta, proven| {
        if let Some(body) = &body {
            meta.self_facts
                .seed(&door::seed_rows_in(body, Some(sid)), now_ms);
        }
        meta.self_facts.observe(reading.as_ref(), now_ms);
        meta.self_facts
            .pick(reading.as_ref(), proven, reader, now_ms)
    }) else {
        return DENIED.to_string();
    };
    if let Pick::Why(w) = first {
        return why(w);
    }
    match policy() {
        Err(_) => return why(WHY_BUSY),
        Ok(facts) if !facts.effective.0 => return why(WHY_OFF),
        Ok(facts) if !facts.policy.facts => return why(WHY_FACTS_OFF),
        Ok(_) => {}
    }
    let Some((holder, second)) = as_claim(ctx, conn, prove, |meta, proven| {
        let pick = meta
            .self_facts
            .pick(reading.as_ref(), proven, reader, now_ms);
        if let Pick::Fact(fact) = &pick {
            meta.self_facts.offer(fact, proven.clone(), reader, now_ms);
        }
        pick
    }) else {
        return DENIED.to_string();
    };
    match second {
        Pick::Fact(fact) => {
            aterm_log::debug!(
                "self-claim @{sid}: {holder} offered fact {} {:016x} to {reader} for {rule}",
                fact.key.kind.word(),
                fact.key.id
            );
            offered(&fact)
        }
        Pick::Why(w) => why(w),
    }
}

/// THE ACK: under the claim's write, the account tells the episode where this claim's proof was
/// offered it (`ERR stale fact` otherwise, nothing changed); then, outside the locks, the
/// holder's told row is booked in the session ledger. The told mark stands though the booking
/// fail. No Messages entry is posted: the act's own record carries the rider verbatim.
fn told(
    ctx: &crate::SessionCtx,
    conn: Option<u64>,
    prove: &dyn Fn(&str) -> Option<Proven>,
    reads: &Reads<'_>,
    id: u64,
) -> String {
    let sid = ctx.self_id.as_str();
    let (_, now_ms) = (reads.clock)();
    let Some((holder, fact)) = as_claim(ctx, conn, prove, |meta, proven| {
        meta.self_facts.ack(id, proven, now_ms)
    }) else {
        return DENIED.to_string();
    };
    let Some(fact) = fact else {
        return STALE_ACK.to_string();
    };
    crate::self_claim::book_fact((reads.state_dir)().as_deref(), sid, &holder, &fact, now_ms);
    aterm_log::info!(
        "self-claim @{sid}: {holder} told fact {} {:016x} door={}",
        fact.key.kind.word(),
        fact.key.id,
        FactDoor::TurnEnd.word()
    );
    "OK\n".to_string()
}

#[cfg(test)]
#[path = "harness_fact_verb_tests.rs"]
mod tests;
