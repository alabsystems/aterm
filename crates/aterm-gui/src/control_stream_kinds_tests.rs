// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `usage`, `account`, `progress`, `goal` and `compact` records and the session's
//! current-state record ([`crate::session_timeline::StreamKinds`]), driven through the same rig
//! as the stream's own tests: a real claim on a real connection, the shipping lanes and reader,
//! and records written by the wire library's codec. Each built-in source is called as its own
//! caller calls it: the usage tap's hook (`harness_host::log_usage`), the status sweep's screen
//! facts (`observe_screen_facts`), the transcript feeder (`apply_batch_timed`), the footer's
//! facts (`set_claude_footer`), and the `status` and `survey` words.

use alab_wire::record::{CompactState, Item, ItemState, Limit};
use aterm_agent::harness::accounts::AuthRoute;
use aterm_agent::harness::footer::FooterFacts;
use aterm_agent::harness::steps::{Step, StepBatch, StepEvent, StepPos, StepState};
use aterm_slog::{Actor, Conf, Key, Kind, Op, Rec, Src};

use super::state::{records, screen, state, timeline};
use super::*;
use crate::session_timeline::{AgentGauges, StreamKinds, StreamedItem};

/// The hand clock's wall time: every log of these tests reads it, so no mark budget refills.
pub(super) const WALL: u64 = 1_792_000_000_000;

pub(super) fn hand_clock(rig: &Rig, sid: &str) {
    timeline(rig, sid, |timeline| {
        timeline
            .slog_mut()
            .set_clock(crate::session_log::Clock::reading(|| {
                aterm_slog::Stamp::new(WALL, 5_000)
            }));
    });
}

fn usage(account: &str, session: u8, weekly: u8, context_left: Option<u8>) -> Body {
    let limit = |name: &str, used: u8, resets_ms: Option<u64>| Limit {
        name: name.to_string(),
        used: Some(used),
        resets_ms,
    };
    Body::Usage {
        account: account.to_string(),
        limits: vec![
            limit("session", session, Some(WALL + 3_600_000)),
            limit("weekly", weekly, None),
        ],
        context_left,
    }
}

fn account(account: &str, state: &str) -> Body {
    Body::Account {
        account: account.to_string(),
        state: state.to_string(),
    }
}

fn goal(text: Option<&str>) -> Body {
    Body::Goal {
        text: text.map(str::to_string),
    }
}

fn compact(state: CompactState, context_left: Option<u8>) -> Body {
    Body::Compact {
        state,
        context_left,
    }
}

fn progress(items: &[(&str, ItemState)]) -> Body {
    Body::Progress {
        items: items
            .iter()
            .map(|(text, state)| Item {
                text: (*text).to_string(),
                state: *state,
            })
            .collect(),
    }
}

/// An owner connection holding `sid`'s contract 4 claim of `duties`, stated with `rest`
/// (`login=<label>`, `other`).
fn claimant(rig: &Rig, sid: &str, duties: &str, rest: &str) -> Client {
    let mut client = rig.dial("owner");
    assert_eq!(
        client.request(format!("claim {sid} 4 {duties} {rest}").trim_end()),
        "OK claim"
    );
    client
}

/// What `sid`'s current-state record keeps of the stream's other kinds.
pub(super) fn kinds(rig: &Rig, sid: &str) -> StreamKinds {
    timeline(rig, sid, |timeline| timeline.agent().streamed.clone())
}

/// The kinds `sid`'s stream applied.
pub(super) fn applied(rig: &Rig, sid: &str) -> Vec<&'static str> {
    rig.state(sid).applied.iter().copied().collect()
}

/// The live records of `sid`'s log of kind `kind`, oldest first.
pub(super) fn of(rig: &Rig, sid: &str, kind: Kind) -> Vec<Rec> {
    records(rig, sid)
        .into_iter()
        .filter(|rec| rec.k == kind)
        .collect()
}

/// The live records of `sid`'s log a stream record wrote: each carries `hseq=`.
pub(super) fn stream_rows(rig: &Rig, sid: &str) -> Vec<Rec> {
    records(rig, sid)
        .into_iter()
        .filter(|rec| rec.f.get_u(Key::Hseq).is_some())
        .collect()
}

fn refused_account() -> Option<String> {
    Some("refused:account".to_string())
}

/// `usage` AND `account` ARE TAKEN ONLY FOR THE TAB'S OWN ACCOUNT, the claim's `login=`: a
/// record for that account is acknowledged with no `dropped`, enters the applied kinds and is
/// kept (and a `usage` writes its row, `account=<label> h5= d7= pct= hseq= holder=`).
/// NEGATIVE HALF: a record for another account is acknowledged `dropped=refused:account`,
/// writes no row, replaces nothing kept and leaves the applied kinds alone; under a claim that
/// states no `login=` every `usage` and `account` record is refused the same way; and a claim
/// without `accounts` has its `account` record dropped for its scope before any account is
/// compared.
#[test]
fn usage_and_account_are_taken_only_for_the_tabs_account() {
    let rig = Rig::new(2);
    let holder = format!("claude-code-alab@{}", running().pid);
    let mut stream = claimant(&rig, ONE, "walls,accounts", "login=main");
    stream.open(ONE);

    // The tab's own account: taken.
    assert_eq!(stream.record(1, usage("main", 40, 60, Some(70))), (1, None));
    assert_eq!(stream.record(2, account("main", "ok")), (2, None));
    assert_eq!(applied(&rig, ONE), ["account", "usage"]);
    let kept = kinds(&rig, ONE);
    let fed = kept.usage.clone().expect("the usage reading is kept");
    assert_eq!(
        (fed.account.as_str(), fed.context_left, fed.hseq),
        ("main", Some(70), 1)
    );
    assert_eq!(
        (fed.used("session"), fed.used("weekly"), fed.used("monthly")),
        (Some(40), Some(60), None)
    );
    assert_eq!(
        fed.limits[0].resets_ms,
        Some(WALL + 3_600_000),
        "the whole reading is kept, reset times included"
    );
    let own = kept.account.clone().expect("the account line is kept");
    assert_eq!(
        (own.account.as_str(), own.state.as_str(), own.hseq, own.fed),
        ("main", "ok", 2, true)
    );
    let rows = of(&rig, ONE, Kind::Usage);
    assert_eq!(rows.len(), 1, "{rows:?}");
    let row = &rows[0];
    assert_eq!(
        (
            row.op,
            row.f.get_str(Key::Account),
            row.f.get_u(Key::H5),
            row.f.get_u(Key::D7),
            row.f.get_u(Key::Pct),
            row.f.get_u(Key::Hseq),
            row.f.get_str(Key::Holder),
        ),
        (
            Op::Mark,
            Some("main"),
            Some(40),
            Some(60),
            Some(70),
            Some(1),
            Some(holder.as_str())
        )
    );
    assert_eq!(
        (row.src, row.conf, row.by),
        (
            Src::Harness,
            Conf::Claim,
            crate::session_log::sid_actor(ONE).expect("a minted session id")
        )
    );

    // NEGATIVE HALF: another account's records. Each is acknowledged with the refusal, so the
    // harness sees it; nothing is kept, nothing written, and no reader stands down for them.
    let before = (records(&rig, ONE).len(), rig.state(ONE).dropped);
    assert_eq!(
        stream.record(3, usage("work", 1, 2, Some(3))),
        (3, refused_account())
    );
    assert_eq!(
        stream.record(4, account("work", "limited")),
        (4, refused_account())
    );
    assert_eq!(kinds(&rig, ONE), kept, "a refused record replaces nothing");
    assert_eq!(
        (records(&rig, ONE).len(), rig.state(ONE).dropped),
        (before.0, before.1 + 2),
        "no row, and both counted as dropped"
    );
    assert_eq!(rig.state(ONE).unapplied, 0, "refused, not left unapplied");

    // NEGATIVE HALF: a claim that states no `login=` has no own account. Nothing was applied
    // on this tab, so its applied kinds stay empty and its built-in readers keep feeding.
    let mut nameless = claimant(&rig, TWO, "walls,accounts", "");
    let id = nameless.id();
    nameless.open(TWO);
    assert_eq!(
        nameless.record(1, usage("main", 40, 60, None)),
        (1, refused_account())
    );
    assert_eq!(
        nameless.record(2, account("main", "ok")),
        (2, refused_account())
    );
    assert!(applied(&rig, TWO).is_empty(), "{:?}", applied(&rig, TWO));
    assert_eq!(kinds(&rig, TWO), StreamKinds::default());
    assert!(stream_rows(&rig, TWO).is_empty());

    // NEGATIVE HALF: the scope comes first. The same claimant, now with `login=main` and
    // without `accounts`: its `account` record is dropped for the scope, its `usage` is taken.
    drop(nameless);
    rig.ended(id);
    let mut narrow = claimant(&rig, TWO, "walls", "login=main");
    assert_eq!(narrow.open(TWO).acked, 2);
    assert_eq!(
        narrow.record(3, account("main", "ok")),
        (3, Some("scope".to_string()))
    );
    assert_eq!(narrow.record(4, usage("main", 5, 6, None)), (4, None));
    assert_eq!(applied(&rig, TWO), ["usage"]);
    let two = kinds(&rig, TWO);
    assert!(two.account.is_none(), "{two:?}");
    assert_eq!(two.usage.map(|fed| fed.hseq), Some(4));
}

/// THE LATEST APPLIED `account` LINE IS KEPT IN THE CURRENT-STATE RECORD as text, the record as
/// `Record::line` writes it, with its `seq` and the stream's epoch, and it writes no log row:
/// nothing of it lives in the log, so no retention of the log can lose it. It stays after the
/// claim that sent it ends (it then stands no reader down), until a newer applied `account`
/// record replaces it.
/// NEGATIVE HALF: a record for another account, and a resend of a `seq` already applied,
/// replace nothing; the first `account` record's line is not the one kept once a second was
/// applied.
#[test]
fn the_latest_account_record_is_kept_in_the_current_state_record() {
    let rig = Rig::new(2);
    let line = |seq: u64, body: Body| Record { seq, ms: 0, body }.line().expect("a record line");
    let mut stream = claimant(&rig, ONE, "accounts", "login=main");
    let id = stream.id();
    stream.open(ONE);
    let opened = records(&rig, ONE).len();

    assert_eq!(stream.record(1, account("main", "ok")), (1, None));
    let first = kinds(&rig, ONE).account.expect("the first line is kept");
    assert_eq!(first.line, line(1, account("main", "ok")));
    // The one record the log gained is the agent run every applied record opens (D15): the
    // `account` record itself wrote none.
    let after_first = records(&rig, ONE);
    assert_eq!(after_first.len(), opened + 1, "{after_first:?}");
    assert_eq!(
        after_first.last().map(|rec| (rec.k, rec.op)),
        Some((Kind::Agent, Op::Open))
    );

    assert_eq!(stream.record(2, account("main", "limited")), (2, None));
    let kept = kinds(&rig, ONE).account.expect("the latest line is kept");
    assert_eq!(
        (
            kept.account.as_str(),
            kept.state.as_str(),
            kept.hseq,
            kept.fed
        ),
        ("main", "limited", 2, true)
    );
    assert_eq!(kept.epoch, crate::control_auth::process_instance_id());
    assert_eq!(
        kept.line,
        line(2, account("main", "limited")),
        "the applied line is kept as text"
    );
    assert_ne!(kept.line, first.line, "the latest, not the first");
    assert_eq!(
        records(&rig, ONE).len(),
        opened + 1,
        "an `account` record writes no log row"
    );
    assert!(stream_rows(&rig, ONE).is_empty());
    assert!(
        records(&rig, ONE)
            .iter()
            .all(|rec| rec.f.get_str(Key::Account).is_none()),
        "the log holds nothing of the account"
    );

    // NEGATIVE HALF: a refused record and a resend replace nothing.
    assert_eq!(
        stream.record(3, account("work", "ok")),
        (3, refused_account())
    );
    assert_eq!(
        stream.record(2, account("main", "gone")),
        (2, Some("applied".to_string()))
    );
    assert_eq!(kinds(&rig, ONE).account, Some(kept.clone()));

    // The claim is released: the line stays as text, and no longer stands a reader down.
    assert!(crate::session_timeline::release_supervisor(
        &rig.service.handle(ONE).ctx,
        Some(id),
        crate::metrics::now_us(),
        Actor::Ctl,
    ));
    let after = kinds(&rig, ONE).account.expect("kept past its claim");
    assert_eq!(
        (after.line.as_str(), after.fed),
        (kept.line.as_str(), false)
    );
    assert!(kinds(&rig, ONE).fed_account().is_none());

    // Another claimant's applied `account` record replaces it.
    let mut next = claimant(&rig, ONE, "accounts", "other login=work");
    next.open(ONE);
    assert_eq!(next.record(1, account("work", "ok")), (1, None));
    let replaced = kinds(&rig, ONE).account.expect("the newer line");
    assert_eq!(
        (replaced.line, replaced.hseq, replaced.fed),
        (line(1, account("work", "ok")), 1, true)
    );
}

/// `progress`, `goal` AND `compact` ARE WRITTEN UNDER THEIR OWN WORDS, each row stamped
/// `holder=<the claim's holder>`, `src=harness`, `conf=claim` and `by=<the tab's session>` with
/// the record's `seq` as `hseq=`: `mark <run> progress state= n= pct=`, `mark <run> goal
/// state=on|off text=`, and a `compact` step that `started` opens and `done` closes (or opens
/// and closes at once where none is open). They are written with the session's mark budget at
/// zero, and the current-state record keeps the plan's items, the goal's text and the
/// compaction.
/// NEGATIVE HALF: a claim without `state` has each dropped for its scope and writes nothing;
/// the built-in goal reader's row in a tab no stream feeds carries neither `holder=` nor
/// `hseq=` and is not `src=harness`; and with the budget at zero a `mark` claim by the same
/// session is refused, before and after.
#[test]
fn progress_goal_and_compact_are_written_under_their_words() {
    let rig = Rig::new(2);
    let by = crate::session_log::sid_actor(ONE).expect("a minted session id");
    let holder = format!("claude-code-alab@{}", running().pid);
    hand_clock(&rig, ONE);
    let mut stream = rig.claimant(ONE, "state");
    stream.open(ONE);
    // The agent run opens from the claim's row, and the rows below are written on it.
    assert_eq!(stream.record(1, state(State::Working)), (1, None));
    let run = of(&rig, ONE, Kind::Agent)
        .into_iter()
        .find(|rec| rec.op == Op::Open)
        .expect("the agent run")
        .node;

    // The session's whole mark budget, spent: the stream's rows neither read nor spend it.
    let refused = |rig: &Rig| {
        timeline(rig, ONE, |timeline| {
            timeline.note_harness_act("approve", &holder, by).is_none()
        })
    };
    let spent = timeline(&rig, ONE, |timeline| {
        (0..10_000)
            .take_while(|_| timeline.note_harness_act("approve", &holder, by).is_some())
            .count()
    });
    assert!(
        (1..10_000).contains(&spent),
        "CONTROL: a finite budget: {spent}"
    );
    assert!(
        refused(&rig),
        "NEGATIVE HALF: a `mark` claim is refused now"
    );

    let plan = [
        ("read the plan", ItemState::Done),
        ("build the kinds", ItemState::Active),
        ("land", ItemState::Pending),
    ];
    for (seq, body) in [
        (2, progress(&plan)),
        (3, goal(Some("ship the stream split"))),
        (4, compact(CompactState::Started, Some(12))),
        (5, compact(CompactState::Done, Some(80))),
        (6, goal(None)),
        (7, compact(CompactState::Done, None)),
        (8, progress(&[])),
        (9, progress(&[("land", ItemState::Done)])),
    ] {
        assert_eq!(stream.record(seq, body), (seq, None));
    }
    assert_eq!(
        applied(&rig, ONE),
        ["compact", "goal", "progress", "state"],
        "each kind is applied, and none is left unapplied"
    );
    assert_eq!(rig.state(ONE).unapplied, 0);
    assert!(refused(&rig), "the stream's rows gave the budget nothing");

    // Every row the eight records wrote carries the four stamps.
    let rows: Vec<Rec> = stream_rows(&rig, ONE)
        .into_iter()
        .filter(|rec| rec.k != Kind::Agent)
        .collect();
    assert_eq!(rows.len(), 9, "{rows:?}");
    for rec in &rows {
        assert_eq!(
            (rec.src, rec.conf, rec.by, rec.f.get_str(Key::Holder)),
            (Src::Harness, Conf::Claim, by, Some(holder.as_str())),
            "{rec:?}"
        );
        let line = aterm_slog::encode(rec, aterm_slog::Tier::Host).expect("a record line");
        for word in [
            "src=harness",
            "conf=claim",
            &format!("by={ONE}"),
            &format!("holder={holder}"),
        ] {
            assert!(line.contains(word), "{word} in {line}");
        }
    }

    // `progress`: marks on the run, the plan's shape in words and numbers.
    let read: Vec<_> = of(&rig, ONE, Kind::Progress)
        .iter()
        .map(|rec| {
            (
                (rec.op, rec.node == run),
                rec.f.get_word(Key::State),
                rec.f.get_u(Key::N),
                rec.f.get_u(Key::Pct),
                rec.f.get_u(Key::Hseq),
            )
        })
        .collect();
    let mark = (Op::Mark, true);
    assert_eq!(
        read,
        [
            (mark, Some("working"), Some(3), Some(33), Some(2)),
            (mark, Some("none"), Some(0), None, Some(8)),
            (mark, Some("done"), Some(1), Some(100), Some(9)),
        ]
    );
    // `goal`: on with its text, off with none.
    let goals = of(&rig, ONE, Kind::Goal);
    let read: Vec<_> = goals
        .iter()
        .map(|rec| {
            (
                (rec.op, rec.node == run),
                rec.f.get_word(Key::State),
                rec.f.get_str(Key::Text),
                rec.f.get_u(Key::Hseq),
            )
        })
        .collect();
    assert_eq!(
        read,
        [
            (mark, Some("on"), Some("ship the stream split"), Some(3)),
            (mark, Some("off"), None, Some(6)),
        ]
    );
    let line = aterm_slog::encode(&goals[0], aterm_slog::Tier::Host).expect("a record line");
    assert!(line.contains(" mark ") && line.contains(" goal "), "{line}");
    // `compact`: a step. `started` opened it and `done` closed that one; the `done` with none
    // open opened and closed its own.
    let steps = of(&rig, ONE, Kind::Compact);
    let read: Vec<_> = steps
        .iter()
        .map(|rec| (rec.op, rec.f.get_u(Key::Pct), rec.f.get_u(Key::Hseq)))
        .collect();
    assert_eq!(
        read,
        [
            (Op::Open, Some(12), Some(4)),
            (Op::Close, Some(80), Some(5)),
            (Op::Open, None, Some(7)),
            (Op::Close, None, Some(7)),
        ]
    );
    assert_eq!(
        steps[0].node, steps[1].node,
        "`done` closed what `started` opened"
    );
    assert_ne!(steps[0].node, steps[2].node);
    assert_eq!(steps[2].node, steps[3].node);
    let line = aterm_slog::encode(&steps[0], aterm_slog::Tier::Host).expect("a record line");
    assert!(
        line.contains(" open ") && line.contains(" compact "),
        "{line}"
    );

    // The current-state record keeps what the rows only count.
    let kept = kinds(&rig, ONE);
    assert_eq!(
        kept.progress.map(|fed| (fed.items, fed.hseq)),
        Some((vec![("land".to_string(), StreamedItem::Done)], 9))
    );
    assert_eq!(kept.goal.map(|fed| (fed.text, fed.hseq)), Some((None, 6)));
    assert_eq!(
        kept.compact
            .map(|fed| (fed.done, fed.context_left, fed.hseq)),
        Some((true, None, 7))
    );

    // NEGATIVE HALF: no `state` in the grant, no row.
    let mut narrow = claimant(&rig, TWO, "walls", "login=main");
    narrow.open(TWO);
    for (seq, body) in [
        (1, progress(&plan)),
        (2, goal(Some("ship"))),
        (3, compact(CompactState::Started, None)),
    ] {
        assert_eq!(narrow.record(seq, body), (seq, Some("scope".to_string())));
    }
    assert!(stream_rows(&rig, TWO).is_empty());
    assert_eq!(kinds(&rig, TWO), StreamKinds::default());
    assert!(applied(&rig, TWO).is_empty());
    // NEGATIVE HALF: the built-in goal reader's row, in that tab: its own source, no stamp.
    screen(&rig, TWO, "busy", None);
    timeline(&rig, TWO, |timeline| {
        let at = timeline.slog().now();
        timeline
            .slog_mut()
            .observe_screen_facts(None, Some(true), at);
    });
    let built_in = of(&rig, TWO, Kind::Goal);
    assert_eq!(built_in.len(), 1, "{built_in:?}");
    let rec = &built_in[0];
    assert_eq!((rec.src, rec.conf), (Src::Screen, Conf::Strong));
    assert_eq!(
        (rec.f.get_u(Key::Hseq), rec.f.get_str(Key::Holder)),
        (None, None)
    );
}

/// One survey word of `sid`'s row.
pub(super) fn survey(rig: &Rig, sid: &str, key: &str) -> String {
    let here = crate::survey::Here {
        host: None,
        instances: 1,
        access: aterm_types::fleet_access::FleetAccess::ReadOnly,
        drive: false,
        facts: None,
    };
    let rows = crate::survey::survey_rows(
        &rig.service.store,
        crate::survey::SurveyLevel::Content,
        usize::MAX,
        &here,
    );
    let row = rows
        .lines()
        .find(|line| line.starts_with("session ") && line.contains(&format!("sid={sid} ")))
        .unwrap_or_else(|| panic!("no row of {sid} in {rows}"));
    let prefix = format!("{key}=");
    row.split(' ')
        .find_map(|word| word.strip_prefix(prefix.as_str()))
        .unwrap_or_else(|| panic!("no {key}= in {row}"))
        .to_string()
}

/// The transcript feeder hands `sid`'s log one batch: its file's binding on the first
/// (`bind`), then one compaction at offset `off`.
fn transcript_compact(rig: &Rig, sid: &str, bind: bool, off: u64) {
    let step = |off: u64, ev: StepEvent| Step {
        pos: StepPos {
            fp: 1,
            id: None,
            off,
            at_ms: WALL,
        },
        state: StepState::Partial,
        ev,
    };
    let batch = StepBatch {
        steps: bind
            .then(|| step(0, StepEvent::Bind { len: 0 }))
            .into_iter()
            .chain(std::iter::once(step(off, StepEvent::Compact)))
            .collect(),
        ..StepBatch::default()
    };
    let handle = rig.service.handle(sid);
    let mut clock = 0;
    crate::session_log::transcript::apply_batch_timed(
        &handle.ctx.timeline,
        crate::session_log::transcript::Source::Transcript,
        &batch,
        (false, None),
        &mut || {
            clock += 1;
            clock
        },
        &mut |_, _| {},
    );
}

/// What each built-in source wrote in `sid`'s log, by its own source word: the usage tap's
/// marks, the context-left marks, the goal's edges and the transcript's compact steps.
#[derive(Debug, PartialEq, Eq)]
struct BuiltIn {
    usage: usize,
    ctx: usize,
    goal: Vec<&'static str>,
    compact: usize,
}

fn built_in(rig: &Rig, sid: &str) -> BuiltIn {
    let all = records(rig, sid);
    let count = |kind: Kind, src: Src, op: Op| {
        all.iter()
            .filter(|rec| rec.k == kind && rec.src == src && rec.op == op)
            .count()
    };
    BuiltIn {
        usage: count(Kind::Usage, Src::Aterm, Op::Mark),
        ctx: count(Kind::Ctx, Src::Screen, Op::Mark),
        goal: all
            .iter()
            .filter(|rec| rec.k == Kind::Goal && rec.src == Src::Screen)
            .filter_map(|rec| rec.f.get_word(Key::State))
            .collect(),
        compact: count(Kind::Compact, Src::Transcript, Op::Open),
    }
}

/// The readers' words of `sid`: the goal, `status`'s `auth=` and `token=`, and the survey
/// row's `account=`, `auth=`, `ctx_left_pct=`, `usage_5h_pct=` and `usage_7d_pct=`.
fn words(rig: &Rig, sid: &str) -> (Option<String>, String, [String; 5]) {
    let (goal, status) = timeline(rig, sid, |timeline| {
        (
            timeline.goal().map(str::to_string),
            crate::token_shown::status_of(timeline),
        )
    });
    let row = [
        "account",
        "auth",
        "ctx_left_pct",
        "usage_5h_pct",
        "usage_7d_pct",
    ]
    .map(|key| survey(rig, sid, key));
    (goal, status, row)
}

/// EACH BUILT-IN SOURCE STANDS DOWN PER KIND, ONCE THE STREAM APPLIED THAT KIND, in one tab:
/// the usage tap's mark, the context-left mark and the survey row's quota words under `usage`;
/// `status`'s `auth=` and `token=` and the survey row's `account=` and `auth=` under `account`;
/// the goal reader (`goal=`) and the goal's log edge under `goal`; the transcript's compact
/// step under `compact`. From then the reader's word is the record's. No built-in reader of the
/// plan's items exists: `progress` stands nothing down, and the engine's taskbar progress mark
/// (another fact under the same log word) goes on.
/// NEGATIVE HALF: before any of them is applied every built-in source feeds; a kind the stream
/// has not applied keeps its built-in words while another kind of the same tab stands down;
/// and when the claim ends every built-in source feeds again, from what it reads then.
#[test]
fn the_usage_quota_plan_and_goal_readers_stand_down_for_each_kind_the_stream_applied() {
    let rig = Rig::new(2);
    hand_clock(&rig, ONE);
    const PGID: i32 = 4242;
    timeline(&rig, ONE, |timeline| {
        timeline.note_foreground_group(PGID);
    });
    let mut stream = claimant(&rig, ONE, "state,walls,accounts", "login=main");
    let id = stream.id();
    stream.open(ONE);
    assert_eq!(stream.record(1, state(State::Working)), (1, None));
    // The screen reader's own reading: its reader names the agent, as the sweep leaves it.
    screen(&rig, ONE, "busy", None);
    timeline(&rig, ONE, |timeline| {
        timeline.set_gauges(AgentGauges {
            ctx_pct: Some(55),
            shells: None,
        });
        assert!(timeline.set_claude_footer(
            PGID,
            Some(1),
            Some(FooterFacts {
                goal: Some("the footer's goal".to_string()),
                route: Some(AuthRoute::Stored),
                account: Some("footer-account".to_string()),
                ..FooterFacts::default()
            }),
        ));
    });
    // Each built-in source, called as its caller calls it.
    let tap =
        |rig: &Rig| crate::harness_host::log_usage(&rig.service.store, ONE, Some(40), Some(60));
    let screen_facts = |rig: &Rig, ctx: Option<u8>, goal: Option<bool>| {
        timeline(rig, ONE, |timeline| {
            let at = timeline.slog().now();
            timeline.slog_mut().observe_screen_facts(ctx, goal, at);
        });
    };
    let engine = |rig: &Rig, pct: u8| {
        timeline(rig, ONE, |timeline| {
            let at = timeline.slog().now();
            let read = crate::session_log::observed::EngineRead {
                integration: aterm_core::terminal::ShellIntegrationPosture::On,
                notify_total: 0,
                bell_total: 0,
                progress: Some(aterm_types::osc::TaskbarProgress::Paused(pct)),
            };
            timeline.slog_mut().observe_engine(&read, at);
        });
    };
    let taskbar = |rig: &Rig| {
        of(rig, ONE, Kind::Progress)
            .iter()
            .filter(|rec| rec.src == Src::Engine)
            .count()
    };
    let built = |account: &str, auth: &str, ctx: &str, h5: &str, d7: &str| {
        [account, auth, ctx, h5, d7].map(str::to_string)
    };

    // NEGATIVE HALF: nothing but `state` applied, and every built-in source feeds.
    tap(&rig);
    screen_facts(&rig, Some(55), Some(true));
    transcript_compact(&rig, ONE, true, 1);
    engine(&rig, 57);
    assert_eq!(
        built_in(&rig, ONE),
        BuiltIn {
            usage: 1,
            ctx: 1,
            goal: vec!["on"],
            compact: 1
        }
    );
    assert_eq!(taskbar(&rig), 1);
    let footer_words = (
        Some("the footer's goal".to_string()),
        "auth=stored token=-".to_string(),
        built("footer-account", "stored", "55", "-", "-"),
    );
    assert_eq!(words(&rig, ONE), footer_words);
    assert!(
        stream_rows(&rig, ONE)
            .iter()
            .all(|rec| rec.k == Kind::Agent)
    );

    // `usage` applied: the tap's mark, the context-left mark and the row's quota words stand
    // down, and the row says what the record says.
    assert_eq!(stream.record(2, usage("main", 10, 20, Some(70))), (2, None));
    tap(&rig);
    screen_facts(&rig, Some(30), None);
    assert_eq!(
        (built_in(&rig, ONE).usage, built_in(&rig, ONE).ctx),
        (1, 1),
        "neither built-in usage writer wrote"
    );
    assert_eq!(of(&rig, ONE, Kind::Usage).len(), 2, "the record's own row");
    // NEGATIVE HALF: the kinds the stream has not applied keep their built-in words, in the
    // same tab: the goal, the account words and the transcript's compact step.
    screen_facts(&rig, None, Some(false));
    transcript_compact(&rig, ONE, false, 2);
    assert_eq!(
        built_in(&rig, ONE),
        BuiltIn {
            usage: 1,
            ctx: 1,
            goal: vec!["on", "off"],
            compact: 2
        }
    );
    assert_eq!(
        words(&rig, ONE),
        (
            footer_words.0.clone(),
            footer_words.1.clone(),
            built("footer-account", "stored", "70", "10", "20"),
        )
    );

    // `goal`, `account`, `compact` and `progress` applied: each built-in source stands down.
    assert_eq!(stream.record(3, goal(Some("the stream's goal"))), (3, None));
    assert_eq!(stream.record(4, account("main", "ok")), (4, None));
    assert_eq!(
        stream.record(5, compact(CompactState::Started, Some(9))),
        (5, None)
    );
    assert_eq!(
        stream.record(6, compact(CompactState::Done, Some(90))),
        (6, None)
    );
    assert_eq!(
        stream.record(7, progress(&[("land", ItemState::Active)])),
        (7, None)
    );
    assert_eq!(
        applied(&rig, ONE),
        ["account", "compact", "goal", "progress", "state", "usage"]
    );
    let before = built_in(&rig, ONE);
    tap(&rig);
    screen_facts(&rig, Some(20), Some(true));
    // The stream's last goal row said `on`, so only a reading of no goal is an edge the goal
    // writer would write if it did not stand down: it writes nothing.
    screen_facts(&rig, None, Some(false));
    transcript_compact(&rig, ONE, false, 3);
    assert_eq!(built_in(&rig, ONE), before, "no built-in writer wrote");
    assert_eq!(
        words(&rig, ONE),
        (
            Some("the stream's goal".to_string()),
            "auth=- token=ok".to_string(),
            built("main", "-", "70", "10", "20"),
        )
    );
    // A record that names no goal says there is none, though the footer still names one.
    assert_eq!(stream.record(8, goal(None)), (8, None));
    assert_eq!(words(&rig, ONE).0, None);
    // `progress` has no built-in reader to stand down: the engine's taskbar mark goes on.
    engine(&rig, 20);
    assert_eq!(
        taskbar(&rig),
        2,
        "the engine's progress mark is another fact"
    );

    // The claim is released: every built-in source feeds again, from what it reads then.
    assert!(crate::session_timeline::release_supervisor(
        &rig.service.handle(ONE).ctx,
        Some(id),
        crate::metrics::now_us(),
        Actor::Ctl,
    ));
    let kept = kinds(&rig, ONE);
    assert!(
        kept.usage.is_none()
            && kept.goal.is_none()
            && kept.compact.is_none()
            && kept.progress.is_none()
            && kept.fed_account().is_none(),
        "{kept:?}"
    );
    assert_eq!(words(&rig, ONE), footer_words);
    tap(&rig);
    screen_facts(&rig, Some(20), Some(true));
    transcript_compact(&rig, ONE, false, 4);
    assert_eq!(
        built_in(&rig, ONE),
        BuiltIn {
            usage: before.usage + 1,
            ctx: before.ctx + 1,
            goal: vec!["on", "off", "on"],
            compact: before.compact + 1
        }
    );
}

/// The foreground group of a [`footer_tab`].
const TAB_PGID: i32 = 4242;

/// A tab the built-in sources read, as the stand-down test sets one up: a foreground group,
/// a Claude Code screen, the screen's context gauge at 55 and a footer naming a goal, a
/// sign-in route and an account.
fn footer_tab(rig: &Rig, sid: &str) {
    hand_clock(rig, sid);
    timeline(rig, sid, |timeline| {
        timeline.note_foreground_group(TAB_PGID);
    });
    screen(rig, sid, "busy", None);
    timeline(rig, sid, |timeline| {
        timeline.set_gauges(AgentGauges {
            ctx_pct: Some(55),
            shells: None,
        });
        assert!(timeline.set_claude_footer(
            TAB_PGID,
            Some(1),
            Some(FooterFacts {
                goal: Some("the footer's goal".to_string()),
                route: Some(AuthRoute::Stored),
                account: Some("footer-account".to_string()),
                ..FooterFacts::default()
            }),
        ));
    });
}

/// The status sweep's screen facts, as its caller hands them to `sid`'s log.
fn screen_facts(rig: &Rig, sid: &str, ctx: Option<u8>, goal: Option<bool>) {
    timeline(rig, sid, |timeline| {
        let at = timeline.slog().now();
        timeline.slog_mut().observe_screen_facts(ctx, goal, at);
    });
}

/// Place `sid`'s claim again on connection `conn`, as the same claimant with the same `duties`
/// and the `login=` given: a renewal in place.
fn renew(rig: &Rig, sid: &str, conn: u64, duties: &str, login: Option<&str>) {
    let duties: Vec<Duty> = duties
        .split(',')
        .map(|word| Duty::from_word(word).expect("a scope word"))
        .collect();
    place_as(
        &rig.service.handle(sid),
        4,
        &duties,
        conn,
        running(),
        "claude-code-alab",
        login,
    )
    .expect("the claim is placed again");
}

fn strings<const N: usize>(words: [&str; N]) -> [String; N] {
    words.map(str::to_string)
}

/// THE TAB'S OWN ACCOUNT IS THE CLAIM'S `login=` AS IT STANDS NOW: a claim placed again under
/// another `login=` (or none) keeps neither the `usage` reading nor the fed `account` of the
/// account it named before, so the survey row, `status` and the built-in writers are the
/// built-in sources' again until the stream sends a record for the account the claim now names.
/// The kept account line stays as text and stands no reader down.
/// NEGATIVE HALF: a claim placed again under the same `login=` keeps both, and their readers
/// stay stood down; after the change a record for the old account is refused, as any other
/// account's is.
#[test]
fn a_claim_placed_again_under_another_login_keeps_nothing_of_the_old_account() {
    let rig = Rig::new(2);
    footer_tab(&rig, ONE);
    let mut stream = claimant(&rig, ONE, "walls,accounts", "login=main");
    let id = stream.id();
    stream.open(ONE);
    assert_eq!(stream.record(1, usage("main", 10, 20, Some(70))), (1, None));
    assert_eq!(stream.record(2, account("main", "ok")), (2, None));
    let fed_words = (
        "auth=- token=ok".to_string(),
        strings(["main", "-", "70", "10", "20"]),
    );
    let footer_words = (
        "auth=stored token=-".to_string(),
        strings(["footer-account", "stored", "55", "-", "-"]),
    );
    let read = |rig: &Rig| {
        let (_, status, row) = words(rig, ONE);
        (status, row)
    };
    let tap =
        |rig: &Rig| crate::harness_host::log_usage(&rig.service.store, ONE, Some(40), Some(60));
    assert_eq!(read(&rig), fed_words);

    // NEGATIVE HALF: the same claim placed again, `login=main` as before: nothing moves.
    let kept = kinds(&rig, ONE);
    renew(&rig, ONE, id, "walls,accounts", Some("main"));
    assert_eq!(kinds(&rig, ONE), kept);
    assert_eq!(read(&rig), fed_words);
    tap(&rig);
    screen_facts(&rig, ONE, Some(30), None);
    assert_eq!(
        (built_in(&rig, ONE).usage, built_in(&rig, ONE).ctx),
        (0, 0),
        "the built-in usage writers stay stood down"
    );

    // The claim now names another account: nothing of `main` is shown for it.
    renew(&rig, ONE, id, "walls,accounts", Some("other"));
    let after = kinds(&rig, ONE);
    assert!(after.usage.is_none(), "{after:?}");
    assert!(after.fed_account().is_none(), "{after:?}");
    assert_eq!(
        after.account.as_ref().map(|line| line.line.as_str()),
        kept.account.as_ref().map(|line| line.line.as_str()),
        "the kept line stays as text"
    );
    assert_eq!(read(&rig), footer_words);
    tap(&rig);
    screen_facts(&rig, ONE, Some(20), None);
    assert_eq!(
        (built_in(&rig, ONE).usage, built_in(&rig, ONE).ctx),
        (1, 1),
        "the built-in usage writers feed again"
    );

    // NEGATIVE HALF: the old account is another account now; the new one is taken.
    assert_eq!(
        stream.record(3, usage("main", 1, 2, Some(3))),
        (3, refused_account())
    );
    assert_eq!(
        stream.record(4, account("main", "ok")),
        (4, refused_account())
    );
    assert_eq!(read(&rig), footer_words);
    assert_eq!(stream.record(5, usage("other", 5, 6, Some(7))), (5, None));
    assert_eq!(stream.record(6, account("other", "limited")), (6, None));
    assert_eq!(
        read(&rig),
        (
            "auth=- token=limited".to_string(),
            strings(["other", "-", "7", "5", "6"]),
        )
    );

    // A claim placed again with no `login=` has no own account: nothing is kept for it.
    renew(&rig, ONE, id, "walls,accounts", None);
    let none = kinds(&rig, ONE);
    assert!(
        none.usage.is_none() && none.fed_account().is_none(),
        "{none:?}"
    );
    assert_eq!(read(&rig), footer_words);
}

/// A `compact` STEP THE STREAM OPENED ENDS WITH ITS CLAIM'S FEED: a `started` no `done`
/// followed is closed by aterm when the claim is released (`out=passed src=aterm conf=strong
/// by=aterm holder=<who opened it>`), so it takes no later mark and the next claimant's `done`
/// opens and closes a step of its own. A step one holder's record opened is never closed as
/// another holder's.
/// NEGATIVE HALF: a `started` whose own claim sends `done` is closed by that record
/// (`src=harness out=ok`), and the claim's end then closes nothing more.
#[test]
fn a_compact_step_the_stream_opened_ends_with_its_claims_feed() {
    let rig = Rig::new(2);
    let holder = format!("claude-code-alab@{}", running().pid);
    let read = |rig: &Rig, sid: &str| -> Vec<_> {
        of(rig, sid, Kind::Compact)
            .iter()
            .map(|rec| {
                (
                    rec.op,
                    rec.src,
                    rec.f.get_word(Key::Out),
                    rec.f.get_u(Key::Hseq),
                    rec.f.get_str(Key::Holder).map(str::to_string),
                )
            })
            .collect()
    };
    let release = |rig: &Rig, sid: &str, id: u64| {
        assert!(crate::session_timeline::release_supervisor(
            &rig.service.handle(sid).ctx,
            Some(id),
            crate::metrics::now_us(),
            Actor::Ctl,
        ));
    };
    let held = Some(holder.clone());

    // NEGATIVE HALF: started and done under one claim; its end closes nothing more.
    let mut whole = rig.claimant(TWO, "state");
    let two = whole.id();
    whole.open(TWO);
    assert_eq!(whole.record(1, state(State::Working)), (1, None));
    assert_eq!(
        whole.record(2, compact(CompactState::Started, None)),
        (2, None)
    );
    assert_eq!(
        whole.record(3, compact(CompactState::Done, None)),
        (3, None)
    );
    release(&rig, TWO, two);
    assert_eq!(
        read(&rig, TWO),
        [
            (Op::Open, Src::Harness, None, Some(2), held.clone()),
            (Op::Close, Src::Harness, Some("ok"), Some(3), held.clone()),
        ]
    );

    // A `started` and no `done`: the step is open while the claim feeds `compact`.
    let mut first = rig.claimant(ONE, "state");
    let id = first.id();
    first.open(ONE);
    assert_eq!(first.record(1, state(State::Working)), (1, None));
    assert_eq!(
        first.record(2, compact(CompactState::Started, Some(12))),
        (2, None)
    );
    let node = of(&rig, ONE, Kind::Compact)[0].node;
    let open = |rig: &Rig| timeline(rig, ONE, |timeline| timeline.slog().log().is_open(node));
    assert!(open(&rig));
    release(&rig, ONE, id);
    assert!(!open(&rig), "the step ended with the claim");
    let rows = of(&rig, ONE, Kind::Compact);
    assert_eq!(
        read(&rig, ONE),
        [
            (Op::Open, Src::Harness, None, Some(2), held.clone()),
            (Op::Close, Src::Aterm, Some("passed"), None, held.clone()),
        ]
    );
    assert_eq!(
        (rows[1].node, rows[1].conf, rows[1].by),
        (node, Conf::Strong, Actor::Aterm)
    );

    // The next claimant's `done` is its own compaction: a step of its own, not the first's.
    let mut next = claimant(&rig, ONE, "state", "other");
    next.open(ONE);
    assert_eq!(
        next.record(1, compact(CompactState::Done, Some(80))),
        (1, None)
    );
    let rows = of(&rig, ONE, Kind::Compact);
    assert_eq!(rows.len(), 4, "{rows:?}");
    assert_eq!(
        (rows[2].op, rows[3].op, rows[2].node == rows[3].node),
        (Op::Open, Op::Close, true)
    );
    assert_ne!(rows[2].node, node);
    assert_eq!(
        (rows[3].src, rows[3].f.get_u(Key::Hseq)),
        (Src::Harness, Some(1))
    );

    // The log's own rule, with no claim between: a step `a@1` opened is ended as `a@1`'s
    // before `b@2`'s `done` writes its own.
    let by = crate::session_log::sid_actor(ONE).expect("a minted session id");
    let before = of(&rig, ONE, Kind::Compact).len();
    timeline(&rig, ONE, |timeline| {
        let at = timeline.slog().now();
        let slog = timeline.slog_mut();
        assert!(slog.stream_compact(false, None, 7, "a@1", by, at));
        assert!(slog.stream_compact(true, None, 8, "b@2", by, at));
    });
    let tail = read(&rig, ONE).split_off(before);
    let (a, b) = (Some("a@1".to_string()), Some("b@2".to_string()));
    assert_eq!(
        tail,
        [
            (Op::Open, Src::Harness, None, Some(7), a.clone()),
            (Op::Close, Src::Aterm, Some("passed"), None, a),
            (Op::Open, Src::Harness, None, Some(8), b.clone()),
            (Op::Close, Src::Harness, Some("ok"), Some(8), b),
        ]
    );
}

/// A `compact` STEP THE STREAM OPENED ENDS AS A SEAMLESS UPDATE'S SUCCESSOR ADOPTS THE LOG:
/// the predecessor's log is carried with the step a `started` opened and no `done` closed, and
/// the successor, whose stream feeds nothing until a claim and its records come again, closes
/// it as the end of the stream's feed closes it (`out=passed src=aterm conf=strong by=aterm
/// holder=<who opened it>`), not as a node no producer holds (`out=gone conf=exact`, no
/// `holder=`). It is not left open under the run, where survey would count it and a mark that
/// names no node would land in it.
/// NEGATIVE HALF: a step its own `done` closed before the update is carried closed, and the
/// adoption writes no compact row of its own.
#[test]
fn a_compact_step_the_stream_opened_ends_when_a_seamless_update_adopts_the_log() {
    let rig = Rig::new(2);
    let held = Some(format!("claude-code-alab@{}", running().pid));
    let [one, two] = [ONE, TWO].map(|sid| crate::session_log::sid_actor(sid).expect("a sid"));
    // `sid`'s log, carried as the handoff worker carries it and adopted by a successor: each
    // compact row of the successor's log, and whether a compact step is still open there.
    let carried = |rig: &Rig, sid: &str| {
        let (lines, ids, seq) = timeline(rig, sid, |timeline| {
            let slog = timeline.slog_mut();
            let ids = slog.handoff_carry();
            let seq = slog.handoff_seq();
            let out = slog
                .carry_source()
                .prepare()
                .encode(aterm_slog::CarryBudget::SESSION)
                .expect("carried");
            slog.lift_read_fence();
            (out.lines, ids, seq)
        });
        let mut successor = crate::session_timeline::SessionTimeline::for_test();
        let at = successor.slog().now();
        successor
            .slog_mut()
            .adopt_carried(&lines, ids, seq, None, at)
            .expect("adopts");
        let log = successor.slog().log();
        let rows: Vec<_> = log
            .window()
            .filter(|rec| rec.k == Kind::Compact)
            .map(|rec| {
                (
                    rec.op,
                    rec.src,
                    rec.conf,
                    rec.by,
                    rec.f.get_word(Key::Out),
                    rec.f.get_str(Key::Holder).map(str::to_string),
                )
            })
            .collect();
        let open = log.open_nodes().iter().any(|e| e.kind == Kind::Compact);
        (rows, open)
    };

    // A `started` and no `done` when the update comes.
    let mut first = rig.claimant(ONE, "state");
    first.open(ONE);
    assert_eq!(first.record(1, state(State::Working)), (1, None));
    assert_eq!(
        first.record(2, compact(CompactState::Started, Some(12))),
        (2, None)
    );
    assert_eq!(
        carried(&rig, ONE),
        (
            vec![
                (Op::Open, Src::Harness, Conf::Claim, one, None, held.clone()),
                (
                    Op::Close,
                    Src::Aterm,
                    Conf::Strong,
                    Actor::Aterm,
                    Some("passed"),
                    held.clone()
                ),
            ],
            false
        )
    );

    // NEGATIVE HALF: started and done before the update; the adoption closes nothing more.
    let mut whole = rig.claimant(TWO, "state");
    whole.open(TWO);
    assert_eq!(whole.record(1, state(State::Working)), (1, None));
    assert_eq!(
        whole.record(2, compact(CompactState::Started, None)),
        (2, None)
    );
    assert_eq!(
        whole.record(3, compact(CompactState::Done, None)),
        (3, None)
    );
    assert_eq!(
        carried(&rig, TWO),
        (
            vec![
                (Op::Open, Src::Harness, Conf::Claim, two, None, held.clone()),
                (Op::Close, Src::Harness, Conf::Claim, two, Some("ok"), held),
            ],
            false
        )
    );
}

/// A `usage` RECORD STANDS ITS WHOLE KIND DOWN, NOT ONLY THE PARTS IT STATES (D29: each
/// built-in source stands down by kind, so no kind has two writers): once a `usage` record is
/// applied, the survey row's `ctx_left_pct=`, `usage_5h_pct=` and `usage_7d_pct=` are the
/// latest record's alone, `-` for a part it leaves unsaid (no `context_left`, a limit under
/// another name than `session` or `weekly`, a limit with no `used`) though the screen's gauge
/// and the usage tap hold readings; and neither the usage tap's mark nor the context-left mark
/// writes, whatever the record states.
/// NEGATIVE HALF: before the record every built-in part feeds and the row is the built-in
/// sources'; when the claim ends each feeds again.
#[test]
fn a_usage_record_stands_its_whole_kind_down() {
    let rig = Rig::new(2);
    footer_tab(&rig, ONE);
    let mut stream = claimant(&rig, ONE, "walls", "login=main");
    let id = stream.id();
    stream.open(ONE);
    let limits = |limits: &[(&str, Option<u8>)], context_left: Option<u8>| Body::Usage {
        account: "main".to_string(),
        limits: limits
            .iter()
            .map(|(name, used)| Limit {
                name: (*name).to_string(),
                used: *used,
                resets_ms: None,
            })
            .collect(),
        context_left,
    };
    let quota = |rig: &Rig| {
        ["ctx_left_pct", "usage_5h_pct", "usage_7d_pct"].map(|key| survey(rig, ONE, key))
    };
    let tap =
        |rig: &Rig| crate::harness_host::log_usage(&rig.service.store, ONE, Some(40), Some(60));
    // The built-in rows: the usage tap's `(h5, d7)` marks and the context-left marks' `pct`.
    let built = |rig: &Rig| {
        let all = records(rig, ONE);
        let taps: Vec<_> = all
            .iter()
            .filter(|rec| rec.k == Kind::Usage && rec.src == Src::Aterm)
            .map(|rec| (rec.f.get_u(Key::H5), rec.f.get_u(Key::D7)))
            .collect();
        let ctx: Vec<_> = all
            .iter()
            .filter(|rec| rec.k == Kind::Ctx && rec.src == Src::Screen)
            .filter_map(|rec| rec.f.get_u(Key::Pct))
            .collect();
        (taps, ctx)
    };

    // NEGATIVE HALF: nothing applied: the row is the built-in sources' and each writes.
    assert_eq!(quota(&rig), strings(["55", "-", "-"]));
    tap(&rig);
    screen_facts(&rig, ONE, Some(55), None);
    let fed = (vec![(Some(40), Some(60))], vec![55]);
    assert_eq!(built(&rig), fed);

    // Limits under the vendor's own names, no `used` of `session`, and no `context_left`: the
    // record is applied and kept, its row says none of the three, the survey row reads none
    // of them, and no built-in part writes.
    let vendor = [
        ("five_hour", Some(40)),
        ("seven_day", Some(60)),
        ("session", None),
    ];
    assert_eq!(stream.record(1, limits(&vendor, None)), (1, None));
    assert_eq!(applied(&rig, ONE), ["usage"]);
    assert_eq!(
        kinds(&rig, ONE).usage.map(|fed| fed.limits.len()),
        Some(3),
        "the whole reading is kept"
    );
    let rows = stream_rows(&rig, ONE);
    let row = rows
        .iter()
        .find(|rec| rec.k == Kind::Usage)
        .expect("the record's row");
    assert_eq!(
        (
            row.f.get_str(Key::Account),
            row.f.get_u(Key::H5),
            row.f.get_u(Key::D7),
            row.f.get_u(Key::Pct)
        ),
        (Some("main"), None, None, None)
    );
    assert_eq!(
        quota(&rig),
        strings(["-", "-", "-"]),
        "the record's, not the screen's gauge"
    );
    tap(&rig);
    screen_facts(&rig, ONE, Some(30), None);
    assert_eq!(built(&rig), fed, "no built-in usage writer wrote");

    // Only the weekly window and the context left: the row reads the record's two and `-`
    // for the five-hour window the tap holds.
    assert_eq!(
        stream.record(2, limits(&[("weekly", Some(20))], Some(70))),
        (2, None)
    );
    assert_eq!(quota(&rig), strings(["70", "-", "20"]));
    tap(&rig);
    assert_eq!(built(&rig), fed, "no built-in usage writer wrote");

    // Both windows and no context left, after a record that carried it: the latest record is
    // read whole, so the context left reads `-` and its mark stays down.
    assert_eq!(
        stream.record(
            3,
            limits(&[("session", Some(10)), ("weekly", Some(25))], None)
        ),
        (3, None)
    );
    assert_eq!(quota(&rig), strings(["-", "10", "25"]));
    tap(&rig);
    screen_facts(&rig, ONE, Some(20), None);
    assert_eq!(built(&rig), fed, "no built-in usage writer wrote");

    // NEGATIVE HALF: the claim ends, and each built-in part feeds again from what it reads.
    assert!(crate::session_timeline::release_supervisor(
        &rig.service.handle(ONE).ctx,
        Some(id),
        crate::metrics::now_us(),
        Actor::Ctl,
    ));
    assert_eq!(quota(&rig), strings(["55", "-", "-"]));
    tap(&rig);
    screen_facts(&rig, ONE, Some(20), None);
    assert_eq!(
        built(&rig),
        (
            vec![(Some(40), Some(60)), (Some(40), Some(60))],
            vec![55, 20]
        )
    );
}

/// A RECORD WHOSE ROW THE LOG REFUSED IS NOT APPLIED: with the session's log closed, a `usage`,
/// `progress`, `goal` or `compact` record is acknowledged `dropped=refused:log` and counted
/// dropped, its kind leaves the applied set, nothing of it is kept and what the stream kept of
/// that kind before is dropped, so the kind's built-in source reads again and no kind is left
/// with its own writer silenced and no row from the stream.
/// NEGATIVE HALF: the same records are applied and kept while the log is open, and an
/// `account` record, which writes no row, is applied with the log closed.
#[test]
fn a_record_whose_row_the_log_refused_is_not_applied() {
    let rig = Rig::new(2);
    footer_tab(&rig, ONE);
    let mut stream = claimant(&rig, ONE, "state,walls,accounts", "login=main");
    stream.open(ONE);
    let plan = [("land", ItemState::Active)];
    let goal_word = |rig: &Rig| timeline(rig, ONE, |timeline| timeline.goal().map(str::to_string));

    // NEGATIVE HALF: the log is open, and each is applied and kept.
    for (seq, body) in [
        (1, state(State::Working)),
        (2, usage("main", 10, 20, Some(70))),
        (3, progress(&plan)),
        (4, goal(Some("the stream's goal"))),
        (5, compact(CompactState::Started, None)),
    ] {
        assert_eq!(stream.record(seq, body), (seq, None));
    }
    assert_eq!(
        applied(&rig, ONE),
        ["compact", "goal", "progress", "state", "usage"]
    );
    let kept = kinds(&rig, ONE);
    assert!(
        kept.usage.is_some()
            && kept.progress.is_some()
            && kept.goal.is_some()
            && kept.compact.is_some(),
        "{kept:?}"
    );
    assert_eq!(goal_word(&rig), Some("the stream's goal".to_string()));
    assert_eq!(survey(&rig, ONE, "ctx_left_pct"), "70");

    // The session's log closes; no row can be written.
    timeline(&rig, ONE, |timeline| {
        let at = timeline.slog().now();
        assert!(timeline.slog_mut().close_at_quit(at));
    });
    let rows = records(&rig, ONE).len();
    let dropped = rig.state(ONE).dropped;
    let refused = Some("refused:log".to_string());
    for (seq, body) in [
        (6, usage("main", 11, 21, Some(71))),
        (7, progress(&plan)),
        (8, goal(Some("another goal"))),
        (9, compact(CompactState::Done, None)),
    ] {
        assert_eq!(stream.record(seq, body), (seq, refused.clone()));
    }
    assert_eq!(records(&rig, ONE).len(), rows, "no row was written");
    assert_eq!(rig.state(ONE).dropped, dropped + 4);
    assert_eq!(applied(&rig, ONE), ["state"], "none of the four is applied");
    let after = kinds(&rig, ONE);
    assert!(
        after.usage.is_none()
            && after.progress.is_none()
            && after.goal.is_none()
            && after.compact.is_none(),
        "{after:?}"
    );
    // The built-in sources read again.
    assert_eq!(goal_word(&rig), Some("the footer's goal".to_string()));
    assert_eq!(survey(&rig, ONE, "ctx_left_pct"), "55");

    // NEGATIVE HALF: `account` writes no row, so a closed log refuses nothing of it.
    assert_eq!(stream.record(10, account("main", "ok")), (10, None));
    assert_eq!(applied(&rig, ONE), ["account", "state"]);
    assert_eq!(kinds(&rig, ONE).fed_account().map(|fed| fed.hseq), Some(10));
}

/// THE GOAL'S EDGE MEMORY IS THE LOG'S LAST `goal` ROW, whoever wrote it: where the built-in
/// reader last said `on` and the stream then said `off`, the footer's goal read after the
/// claim ends is an edge and writes `on`, so the log's last goal row agrees with `goal=`; while
/// the stream feeds `goal` the same reading, an edge against the stream's `off`, writes nothing.
/// NEGATIVE HALF: where the stream's last row said `on`, the same reading is no edge and
/// writes nothing.
#[test]
fn the_goal_reader_resumes_from_the_logs_last_goal_row() {
    let rig = Rig::new(2);
    let release = |rig: &Rig, sid: &str, id: u64| {
        assert!(crate::session_timeline::release_supervisor(
            &rig.service.handle(sid).ctx,
            Some(id),
            crate::metrics::now_us(),
            Actor::Ctl,
        ));
    };
    // The goal rows of `sid`'s log: who wrote each and what it said.
    let goals = |rig: &Rig, sid: &str| -> Vec<_> {
        of(rig, sid, Kind::Goal)
            .iter()
            .map(|rec| (rec.src, rec.f.get_word(Key::State)))
            .collect()
    };
    for (sid, last, expect) in [
        // The stream last said `off`: the footer's goal is an edge.
        (
            ONE,
            None,
            vec![
                (Src::Screen, Some("on")),
                (Src::Harness, Some("off")),
                (Src::Screen, Some("on")),
            ],
        ),
        // NEGATIVE HALF: the stream last said `on`: no edge, no row.
        (
            TWO,
            Some("the stream's goal"),
            vec![
                (Src::Screen, Some("on")),
                (Src::Harness, Some("off")),
                (Src::Harness, Some("on")),
            ],
        ),
    ] {
        footer_tab(&rig, sid);
        screen_facts(&rig, sid, None, Some(true));
        let mut stream = rig.claimant(sid, "state");
        let id = stream.id();
        stream.open(sid);
        assert_eq!(stream.record(1, goal(None)), (1, None));
        if last.is_some() {
            assert_eq!(stream.record(2, goal(last)), (2, None));
        }
        // Stood down: the footer's reading writes nothing and is not kept. Where the stream
        // last said `off` the reading is an edge, so a goal writer that did not stand down
        // would write its `on` here.
        let streamed = goals(&rig, sid);
        screen_facts(&rig, sid, None, Some(true));
        assert_eq!(goals(&rig, sid), streamed, "{sid}: stood down");
        release(&rig, sid, id);
        assert_eq!(
            timeline(&rig, sid, |timeline| timeline.goal().map(str::to_string)),
            Some("the footer's goal".to_string())
        );
        screen_facts(&rig, sid, None, Some(true));
        assert_eq!(goals(&rig, sid), expect, "{sid}");
    }
}
