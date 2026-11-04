// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `harness fact` (gap 23): only the session's standing proven codex-alab claim reads; aterm
//! judges the rule, the limit's place, the kind and the owner's policy; the main thread is asked
//! only with a candidate and its silence reads `busy`; the ack books the holder's told row once.

use std::sync::{Arc, PoisonError};
use std::time::{Duration, Instant};

use aterm_agent::supervise::approvals;
use aterm_agent::supervise::facts::{EpisodeKey, FactKind};
use aterm_agent::supervise::policy::turn_end::fact_suffix;
use aterm_agent::supervise::{Fact, SupervisorConfig};
use aterm_vitals::jobs::{Consumer, JobRow, JobsView};

use super::*;
use crate::harness_policy_verb::PolicyFacts;
use crate::session_timeline::{claim_supervisor_as, release_supervisor};
use crate::vitals_jobs::JobsBoard;

const BASE_MS: i64 = 1_760_000_000_000;

/// The connection the claim is bound to.
const CONN: u64 = 31;

/// The tab the board names the session's calls under.
const TAB: u64 = 4;

/// A read of an act a fact rides, by thread `reader`.
fn read_by(reader: &str) -> String {
    format!("fact for=continue@v1 reader={reader} kinds=no-progress wall=-")
}

const R1: &str = "thread-1";
const R2: &str = "thread-2";

fn lives(_: u32, _: u64) -> bool {
    true
}

fn none_lives(_: u32, _: u64) -> bool {
    false
}

/// The policy hop answering the owner's defaults, in effect.
fn on() -> Result<PolicyFacts, String> {
    Ok(PolicyFacts {
        generation: 1,
        policy: SupervisorConfig::default(),
        effective: (true, "on"),
        session_questions: None,
        codex_rotation: false,
    })
}

/// A policy hop the read must never take.
fn never() -> Result<PolicyFacts, String> {
    panic!("the main thread was asked without a candidate")
}

/// A codex-alab holder on the stand-in process, which runs.
fn me() -> String {
    format!("codex-alab@{}", crate::self_claim::stand_in::pid())
}

fn no_proof(_: &str) -> Option<Proven> {
    None
}

/// One session, its board and its state directory, at the clocks `t0` and [`BASE_MS`].
struct Fx {
    h: crate::session_store::SessionHandle,
    board: JobsBoard,
    dir: aterm_tempfile::TempDir,
    t0: Instant,
}

impl Fx {
    fn new(local: u64) -> Self {
        Self {
            h: crate::session_store::test_handle_on(local, -1),
            board: JobsBoard::new(),
            dir: aterm_tempfile::tempdir().expect("scratch"),
            t0: Instant::now(),
        }
    }

    fn ctx(&self) -> &crate::SessionCtx {
        self.h.ctx.as_ref()
    }

    fn sid(&self) -> String {
        self.ctx().self_id.as_str().to_string()
    }

    /// The standing proven claim of `holder` on [`CONN`].
    fn claim(&self, holder: &str) {
        claim_supervisor_as(
            self.ctx(),
            holder,
            Some(CONN),
            None,
            Some(crate::self_claim::stand_in::running()),
            crate::metrics::now_us(),
            aterm_slog::Actor::Ctl,
        )
        .expect("a proven claim");
    }

    /// A new view at `t0 + at` naming the session's background calls at no progress, each
    /// `(pid, quiet minutes)`, born at `pid * 10`.
    fn post(&self, at: Duration, calls: &[(u32, u64)]) {
        let rows = calls
            .iter()
            .map(|&(pid, min)| JobRow {
                leader: (pid, Some(u64::from(pid) * 10)),
                ..crate::vitals_jobs::tests::call_row(
                    TAB,
                    Consumer::AgentBg,
                    true,
                    min,
                    pid,
                    "python3",
                )
            })
            .collect();
        self.board.post(
            Arc::new(JobsView {
                rows,
                ..JobsView::default()
            }),
            std::collections::BTreeMap::from([(TAB, self.sid())]),
            self.t0 + at,
        );
    }

    /// `rest` asked at `t0 + at` (unix ms [`BASE_MS`] `+ at`) by the Owner on [`CONN`].
    fn ask(
        &self,
        rest: &str,
        at: Duration,
        policy: impl FnOnce() -> Result<PolicyFacts, String>,
    ) -> String {
        self.ask_as(rest, true, Some(CONN), &no_proof, lives, at, policy)
    }

    #[allow(clippy::too_many_arguments)]
    fn ask_as(
        &self,
        rest: &str,
        owner: bool,
        conn: Option<u64>,
        prove: &dyn Fn(&str) -> Option<Proven>,
        lives: fn(u32, u64) -> bool,
        at: Duration,
        policy: impl FnOnce() -> Result<PolicyFacts, String>,
    ) -> String {
        let state = self.dir.path().to_path_buf();
        let state_dir = move || Some(state.clone());
        let now = self.t0 + at;
        let now_ms = BASE_MS + i64::try_from(at.as_millis()).expect("a short test");
        let clock = move || (now, now_ms);
        cmd(
            self.ctx(),
            rest,
            owner,
            conn,
            prove,
            Reads {
                board: &self.board,
                lives,
                state_dir: &state_dir,
                clock: &clock,
            },
            policy,
        )
    }

    /// The session ledger's lines.
    fn ledger(&self) -> Vec<String> {
        std::fs::read_to_string(approvals::ledger_under(self.dir.path(), Some(&self.sid())))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn seeded(&self) -> bool {
        self.ctx()
            .meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .self_facts
            .seeded()
    }
}

/// `key=`'s value in a reply line.
fn field<'a>(reply: &'a str, key: &str) -> Option<&'a str> {
    reply
        .trim_end()
        .split(' ')
        .find_map(|w| w.strip_prefix(key)?.strip_prefix('='))
}

/// `%XX` decoded, the inverse of [`crate::control::pct_encode`].
fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).expect("hex");
            out.push(u8::from_str_radix(hex, 16).expect("hex"));
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).expect("UTF-8")
}

/// The fact an `OK fact=…` reply offered, read back from its words.
fn offered(reply: &str) -> Fact {
    assert!(
        reply.starts_with("OK fact=") && !reply.starts_with("OK fact=- "),
        "{reply}"
    );
    assert_eq!(field(reply, "kind"), Some("no-progress"), "{reply}");
    Fact {
        key: EpisodeKey {
            kind: FactKind::NoProgress,
            id: u64::from_str_radix(field(reply, "episode").expect("episode"), 16).expect("hex"),
        },
        text: pct_decode(field(reply, "fact").expect("fact")),
        as_of_ms: field(reply, "as_of_ms")
            .and_then(|ms| ms.parse().ok())
            .expect("as_of_ms"),
    }
}

fn why(w: &str) -> String {
    format!("OK fact=- why={w}\n")
}

fn told(fact: &Fact) -> String {
    format!("fact told episode={:016x}", fact.key.id)
}

const AT: Duration = Duration::from_millis(1_000);

/// ONLY THE SESSION'S STANDING PROVEN CODEX-ALAB CLAIM READS: an Owner without a claim, a line
/// that is not the Owner's, an unproven `codex-alab@` claim, another connection the proof does
/// not name, a proven claim of a holder that is no codex-alab one and a rostered contract-3
/// claim all get `ERR denied`, the account untouched (unseeded, nothing leased: the claim then
/// made is offered the call). CONTROL: the same read on the proven claim's connection, or from a
/// process the proof names, is offered.
#[test]
fn only_a_codex_alab_proven_claim_reads() {
    let fx = Fx::new(9_301);
    fx.post(Duration::ZERO, &[(700, 9)]);
    let denied = "ERR denied\n";
    let ask = |owner, conn, prove: &dyn Fn(&str) -> Option<Proven>, rest: &str| {
        fx.ask_as(rest, owner, conn, prove, lives, AT, never)
    };
    let ack = "fact told episode=00000000000000aa";
    for rest in [read_by(R1).as_str(), ack] {
        assert_eq!(
            ask(true, Some(CONN), &no_proof, rest),
            denied,
            "no claim: {rest}"
        );
    }
    let now = crate::metrics::now_us();
    claim_supervisor_as(
        fx.ctx(),
        &me(),
        Some(CONN),
        None,
        None,
        now,
        aterm_slog::Actor::Ctl,
    )
    .expect("an unproven claim");
    assert_eq!(
        ask(true, Some(CONN), &no_proof, &read_by(R1)),
        denied,
        "unproven"
    );
    assert!(release_supervisor(
        fx.ctx(),
        None,
        now,
        aterm_slog::Actor::Ctl
    ));
    fx.claim("grok-alab@4242");
    assert_eq!(
        ask(true, Some(CONN), &no_proof, &read_by(R1)),
        denied,
        "a proven holder that is no codex-alab one"
    );
    assert!(release_supervisor(
        fx.ctx(),
        None,
        now,
        aterm_slog::Actor::Ctl
    ));
    crate::session_timeline::claim_supervisor_scoped(
        fx.ctx(),
        "grok-alab@4242",
        Some(CONN),
        crate::session_timeline::scoped_input_tests::scoped(&[
            aterm_agent::harness::contract::Scope::Approvals,
        ]),
        now,
        aterm_slog::Actor::Ctl,
    )
    .expect("a rostered claim");
    assert_eq!(
        ask(true, Some(CONN), &no_proof, &read_by(R1)),
        denied,
        "a contract-3 claim"
    );
    assert!(release_supervisor(
        fx.ctx(),
        None,
        now,
        aterm_slog::Actor::Ctl
    ));
    fx.claim(&me());
    assert_eq!(
        ask(false, Some(CONN), &no_proof, &read_by(R1)),
        denied,
        "not the Owner's"
    );
    assert_eq!(
        ask(true, Some(CONN + 1), &no_proof, &read_by(R1)),
        denied,
        "another connection the proof does not name"
    );
    assert!(!fx.seeded(), "the account untouched");
    let named = |_: &str| Some(crate::self_claim::stand_in::running());
    let fact = offered(&fx.ask_as(&read_by(R2), true, Some(CONN + 1), &named, lives, AT, on));
    assert_eq!(
        offered(&fx.ask(&read_by(R2), AT, on)).key,
        fact.key,
        "CONTROL: the claim's own connection, taken again by its reader"
    );
}

/// ATERM JUDGES THE RULE, THE LIMIT'S PLACE AND THE KIND, from the request alone and before any
/// hop: an act no fact rides reads `why=rule`; an act of a limit's place at a reset or quota wall
/// reads `why=limit`; continue is no limit's-place rule and is offered there, as in aterm's loop;
/// a kind aterm does not serve reads `why=kinds`.
#[test]
fn aterm_judges_the_rule_the_limit_and_the_kind() {
    let fx = Fx::new(9_302);
    fx.claim(&me());
    fx.post(Duration::ZERO, &[(700, 9)]);
    let ask = |rest: &str| fx.ask(rest, AT, never);
    assert_eq!(
        ask("fact for=api-retry@v1 reader=t kinds=no-progress wall=server"),
        why(WHY_RULE)
    );
    for wall in ["usage", "entitlement"] {
        assert_eq!(
            ask(&format!(
                "fact for=continue-done-check@v1 reader=t kinds=no-progress wall={wall}"
            )),
            why(WHY_LIMIT),
            "{wall}"
        );
    }
    assert_eq!(
        ask("fact for=continue@v1 reader=t kinds=hold,wake wall=-"),
        why(WHY_KINDS)
    );
    offered(&fx.ask(
        "fact for=continue@v1 reader=t kinds=no-progress wall=usage",
        AT,
        on,
    ));
    offered(&fx.ask(
        "fact for=continue-done-check@v1 reader=t kinds=hold,no-progress wall=server",
        AT,
        on,
    ));
}

/// NO CANDIDATE TAKES NO MAIN-THREAD HOP: with no view, no stalled call, or only a told one, the
/// read answers from the account alone.
#[test]
fn no_candidate_takes_no_main_thread_hop() {
    let fx = Fx::new(9_303);
    fx.claim(&me());
    assert_eq!(fx.ask(&read_by(R1), AT, never), why(facts_offer::WHY_STALE));
    fx.post(Duration::ZERO, &[]);
    assert_eq!(fx.ask(&read_by(R1), AT, never), why(facts_offer::WHY_NONE));
    fx.post(AT, &[(700, 9)]);
    let fact = offered(&fx.ask(&read_by(R1), AT * 2, on));
    assert_eq!(fx.ask(&told(&fact), AT * 2, never), "OK\n");
    assert_eq!(
        fx.ask(&read_by(R1), AT * 3, never),
        why(facts_offer::WHY_TOLD)
    );
}

/// A MAIN THREAD THAT DOES NOT ANSWER READS BUSY: a hop refused or run out answers `OK fact=-
/// why=busy`, never `ERR`, and leases nothing (another reader is then offered the call).
#[test]
fn a_main_thread_that_does_not_answer_reads_busy() {
    let fx = Fx::new(9_304);
    fx.claim(&me());
    fx.post(Duration::ZERO, &[(700, 9)]);
    assert_eq!(
        fx.ask(&read_by(R1), AT, || Err(
            "main-thread reply did not arrive".into()
        )),
        why(WHY_BUSY)
    );
    offered(&fx.ask(&read_by(R2), AT, on));
}

/// THE OWNER'S POLICY LIMITS: `[harness] facts = false` reads `why=facts-off`, a policy not in
/// effect for the session (a hold) `why=off`; neither leases the call, which another reader is
/// then offered.
#[test]
fn facts_off_and_an_off_session_offer_nothing() {
    let fx = Fx::new(9_305);
    fx.claim(&me());
    fx.post(Duration::ZERO, &[(700, 9)]);
    let facts_off = || {
        on().map(|mut f| {
            f.policy.facts = false;
            f
        })
    };
    let held = || {
        on().map(|mut f| {
            f.effective = (false, "hold");
            f
        })
    };
    assert_eq!(fx.ask(&read_by(R1), AT, facts_off), why(WHY_FACTS_OFF));
    assert_eq!(fx.ask(&read_by(R1), AT, held), why(WHY_OFF));
    offered(&fx.ask(&read_by(R2), AT, on));
}

/// THE ACK BOOKS THE HOLDER'S ROW AND NOTHING ELSE: the read's rider is aterm's own
/// [`fact_suffix`] of the fact it names; its ack books one fact row with the holder and the
/// turn-end door, which seeds a successor told; a second ack reads `ERR stale fact` and books
/// nothing; the episode is then told.
#[test]
fn the_ack_books_the_holders_row_and_nothing_else() {
    let fx = Fx::new(9_306);
    fx.claim(&me());
    fx.post(Duration::ZERO, &[(700, 9)]);
    let reply = fx.ask(&read_by(R1), AT, on);
    let fact = offered(&reply);
    assert_eq!(
        field(&reply, "rider").map(pct_decode),
        Some(fact_suffix(&fact)),
        "{reply}"
    );
    assert_eq!(fact.as_of_ms, BASE_MS, "the view's time: {reply}");
    assert!(fx.ledger().is_empty(), "a read books nothing");
    assert_eq!(fx.ask(&told(&fact), AT, never), "OK\n");
    let rows = fx.ledger();
    assert_eq!(rows.len(), 1, "{rows:#?}");
    let row = &rows[0];
    let v: aterm_json::Value = aterm_json::from_str(row).expect("a JSON row");
    assert_eq!(
        approvals::SelfClaimFactRow::of(&v),
        Some((me().as_str(), FactKind::NoProgress.rule())),
        "{row}"
    );
    assert!(
        row.contains(&format!("episode={:016x} door=turn-end", fact.key.id)),
        "{row}"
    );
    assert_eq!(
        aterm_agent::supervise::occupancy::door::seed_rows_in(&format!("{row}\n"), Some(&fx.sid()))
            .len(),
        1,
        "{row}"
    );
    assert_eq!(fx.ask(&told(&fact), AT, never), "ERR stale fact\n");
    assert_eq!(fx.ledger().len(), 1, "a second ack books nothing");
    assert_eq!(fx.ask(&read_by(R1), AT, never), why(facts_offer::WHY_TOLD));
}

/// A DEAD LEADER IS NEVER OFFERED: a call the view names whose leader no longer lives at its
/// birth is no call of the reading. CONTROL: the same call alive is offered.
#[test]
fn a_dead_leader_is_never_offered() {
    let fx = Fx::new(9_307);
    fx.claim(&me());
    fx.post(Duration::ZERO, &[(700, 9)]);
    assert_eq!(
        fx.ask_as(
            &read_by(R1),
            true,
            Some(CONN),
            &no_proof,
            none_lives,
            AT,
            never
        ),
        why(facts_offer::WHY_NONE)
    );
    offered(&fx.ask(&read_by(R1), AT, on));
}

/// THE VERB READS ITS FORMS STRICTLY: anything but its two forms, each key once, is the `harness`
/// usage line, before any claim is looked at.
#[test]
fn a_malformed_fact_line_is_the_usage_line() {
    let fx = Fx::new(9_308);
    for rest in [
        "fact",
        "fact told",
        "fact told episode=AA00000000000000",
        "fact told episode=00aa",
        "fact told episode=00000000000000aa extra",
        "fact for=continue@v1 reader=t kinds=no-progress",
        "fact for=continue@v1 reader=t kinds=no-progress wall=- wall=-",
        "fact for=continue reader=t kinds=no-progress wall=-",
        "fact for=continue@v1 reader=a.b kinds=no-progress wall=-",
        "fact for=continue@v1 reader=t kinds=no-progress,bogus wall=-",
        "fact for=continue@v1 reader=t kinds=no-progress wall=Usage",
        "fact for=continue@v1 reader=t kinds=no-progress wall=- why=x",
    ] {
        assert_eq!(
            fx.ask(rest, AT, never),
            crate::harness_recovery_verb::USAGE,
            "{rest}"
        );
    }
    assert_eq!(
        fx.ask(
            "fact wall=- kinds=no-progress reader=t for=continue@v1",
            AT,
            never
        ),
        "ERR denied\n",
        "any order: well formed, then denied for want of a claim"
    );
}

/// A LEDGER THAT CANNOT BE READ SEEDS NOTHING AND OFFERS NOTHING: the account's one seed reads the
/// session ledger whole, so one standing where it cannot be read (here a directory) answers `ERR`
/// and leaves the account unseeded, never offering an episode a predecessor may have told.
/// CONTROL: with no ledger at all the account seeds empty and offers the call.
#[test]
fn an_unreadable_ledger_seeds_nothing_and_offers_nothing() {
    let fx = Fx::new(9_309);
    fx.claim(&me());
    fx.post(Duration::ZERO, &[(700, 9)]);
    let ledger = approvals::ledger_under(fx.dir.path(), Some(&fx.sid()));
    std::fs::create_dir_all(&ledger).expect("a directory where the ledger stands");
    let reply = fx.ask(&read_by(R1), AT, never);
    assert!(
        reply.starts_with("ERR the session ledger is unreadable: "),
        "{reply}"
    );
    assert!(!fx.seeded(), "nothing seeded");
    std::fs::remove_dir(&ledger).expect("the directory gone");
    offered(&fx.ask(&read_by(R1), AT, on));
    assert!(fx.seeded(), "CONTROL: seeded empty");
}
