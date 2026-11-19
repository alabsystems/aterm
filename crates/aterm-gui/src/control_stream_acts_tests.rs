// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `act`, `need`, `clear`, `land`, `model` and `upgrade` records (HX-15j), driven through
//! the same rig as the stream's own tests: a real claim on a real connection, the shipping
//! lanes and reader, and records written by the wire library's codec. The `meta set harness_*`
//! lines are served by the shipping `meta` verb on another owner connection the claimant's
//! process proves, and the land check reads the service's stand-in repository and `sys left`
//! reads, in line, where the window reads them on a thread of its own.

use alab_wire::record::{
    Did, HeldBy, LandHold, LandRead, Landing, NeedKind, Offer, UpgradePhase, WaitReason,
};
use aterm_agent::harness::acts::{self, HoldLeg, LandHeld};
use aterm_agent::harness::footer::FooterFacts;
use aterm_agent::harness::leftover::LeftRead;
use aterm_agent::harness::repo_work::RepoRead;
use aterm_slog::{Actor, Conf, Key, Kind, Rec, Src};

use super::kinds::{WALL, applied, hand_clock, kinds, of, stream_rows, survey};
use super::state::{screen, state, timeline};
use super::*;
use crate::session_timeline::StreamModel;
use crate::task_record::{TaskMark, TaskState};

/// The approval rule `approve-all@v1` (scope `approvals`).
const APPROVE_ALL: &str = "approve-all@v1";
/// A question's rule, `answer-recommended@v1` (scope `questions`).
const ANSWER: &str = "answer-recommended@v1";
/// The commit a `land` record says landed.
const COMMIT: &str = "3f9a1c2e4b7d";

fn rule(rule: &str) -> Body {
    Body::Act {
        did: Did::Rule {
            act: "acted".to_string(),
            rule: rule.to_string(),
        },
    }
}

fn wait(reason: WaitReason, wait_ms: Option<u64>) -> Body {
    Body::Act {
        did: Did::Wait { reason, wait_ms },
    }
}

fn held(held: Option<HeldBy>) -> Body {
    Body::Act {
        did: Did::Held { held },
    }
}

fn need(kind: NeedKind, entry: &str, needs: &[&str]) -> Body {
    Body::Need {
        kind,
        entry: entry.to_string(),
        needs: needs.iter().map(|need| (*need).to_string()).collect(),
    }
}

fn clear(need: u64) -> Body {
    Body::Clear { need }
}

/// A Land hold since `WALL`, with `nudges` nudges and a read of the yield the repository held.
fn land_held(nudges: u64) -> Body {
    Body::Land {
        state: Landing::Held(LandHold {
            since_ms: WALL,
            nudges,
            read: Some(LandRead {
                legs: vec!["repo".to_string()],
                open: Vec::new(),
                changed: Some(2),
                untracked: None,
                stashes: None,
                unpushed: Some(1),
                unlanded: None,
                left: None,
            }),
        }),
    }
}

/// The hold [`land_held`] tells, as `meta set harness_land` keeps it.
fn kept_hold(nudges: u32) -> LandHeld {
    LandHeld {
        since_ms: Some(i64::try_from(WALL).expect("a wall time")),
        nudges,
        read: true,
        legs: HoldLeg::Repo.bit(),
        changed: 2,
        unpushed: 1,
        ..LandHeld::default()
    }
}

fn landed() -> Body {
    Body::Land {
        state: Landing::Landed {
            commit: COMMIT.to_string(),
        },
    }
}

fn land_open() -> Body {
    Body::Land {
        state: Landing::Open,
    }
}

fn model(model: Option<&str>, effort: Option<&str>) -> Body {
    Body::Model {
        model: model.map(str::to_string),
        effort: effort.map(str::to_string),
        key: Some(true),
        offers: vec![Offer {
            model: "claude-opus-5".to_string(),
            levels: vec!["low".to_string(), "high".to_string()],
        }],
        permission: Some("auto".to_string()),
    }
}

fn upgrade(phase: UpgradePhase, to: &str) -> Body {
    Body::Upgrade {
        phase,
        to: to.to_string(),
    }
}

/// The line the codec writes for record `seq` of `body`, as the client sent it.
fn line(seq: u64, body: Body) -> String {
    Record { seq, ms: 0, body }.line().expect("a record line")
}

fn scope() -> Option<String> {
    Some("scope".to_string())
}

fn holder() -> String {
    format!("claude-code-alab@{}", running().pid)
}

/// Read `sid`'s session metadata.
fn meta<R>(
    rig: &Rig,
    sid: &str,
    read: impl FnOnce(&crate::session_timeline::SessionMeta) -> R,
) -> R {
    let handle = rig.service.handle(sid);
    let meta = handle
        .ctx
        .meta
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    read(&meta)
}

/// The NEEDS YOU set `sid`'s claim told last, as `status needs_told=` reads it.
fn told(rig: &Rig, sid: &str) -> Option<Vec<String>> {
    meta(rig, sid, |meta| {
        meta.self_needs(crate::metrics::now_us())
            .and_then(|told| told.current().map(|set| set.needs().to_vec()))
    })
}

/// The needs `sid`'s log holds that the session's own harness raised, by word.
fn harness_needs(rig: &Rig, sid: &str) -> Vec<&'static str> {
    timeline(rig, sid, |timeline| {
        timeline
            .slog()
            .log()
            .state()
            .needs
            .iter()
            .flatten()
            .filter(|need| need.src == Src::Harness)
            .map(|need| need.word)
            .collect()
    })
}

/// `ACT`, `NEED`, `CLEAR` AND `LAND` ARE ADMITTED BY THE SCOPE OF THE `meta set` EACH REPLACES
/// and applied as that line is, each row through the stream's arm with all four stamps: an act
/// by its rule's scope (`approve-all@v1` under `approvals`), written as `meta set harness_act`
/// writes it (`word=approve rule= harness_act=`); `wait` under `state`, kept as the claim's
/// continuation observation (`harness_activity`: the record's `seq` its sequence, its reason,
/// its end `wait_ms` from now) with no row; `held` under `turn-end` as `harness_held`'s stop;
/// `need` under `approvals` as `mark need word=<kind> text=<entry>` with the set
/// `harness_needs` tells, which the fold raises as the harness's; `clear` as the `mark clear`
/// that lifts it and takes the set down; `land` under `turn-end` as `harness_land`'s hold.
/// NEGATIVE HALF: under a grant without that scope each is acknowledged `dropped=scope`, keeps
/// nothing and writes nothing (a question's act under `approvals` alone, `wait` without
/// `state`); a `need` whose set `harness_needs` would refuse is `dropped=refused:usage`; and a
/// refused record never enters the applied kinds.
#[test]
fn act_need_clear_and_land_records_are_admitted_by_the_scope_of_the_act_they_replace() {
    let rig = Rig::new(2);
    let now = crate::metrics::now_us;

    // ONE holds `state` alone: `wait` is taken, every other act kind is out of its scope.
    let mut one = rig.claimant(ONE, "state");
    one.open(ONE);
    assert_eq!(
        one.record(1, wait(WaitReason::Composing, Some(5_000))),
        (1, None)
    );
    for (seq, body) in [
        (2, rule(APPROVE_ALL)),
        (3, held(Some(HeldBy::NoWork))),
        (
            4,
            need(NeedKind::Review, "review the plan", &["read the plan"]),
        ),
        (5, clear(4)),
        (6, land_held(1)),
        (7, landed()),
    ] {
        assert_eq!(one.record(seq, body), (seq, scope()), "record {seq}");
    }
    assert_eq!(applied(&rig, ONE), ["act"], "only the `wait` was applied");
    let (revision, activity) = meta(&rig, ONE, |meta| {
        (meta.supervisor_revision, meta.continuation_activity.clone())
    });
    let (kept_under, activity) = activity.expect("the `wait` is the continuation observation");
    assert_eq!(
        (kept_under, activity.sequence, activity.reason.as_str()),
        (revision, 1, "composing")
    );
    assert_eq!(
        activity
            .until_ms
            .map(|until| until.saturating_sub(activity.observed_ms)),
        Some(5_000)
    );
    assert!(
        meta(&rig, ONE, |meta| meta.self_held(now()).is_none()
            && meta.self_land(now()).is_none()),
        "NEGATIVE HALF: nothing out of scope was kept"
    );
    assert!(
        stream_rows(&rig, ONE)
            .iter()
            .all(|rec| rec.k == Kind::Agent),
        "a `wait` writes no row, and no record out of scope wrote one"
    );
    assert!(harness_needs(&rig, ONE).is_empty());

    // TWO holds `approvals` and `turn-end`.
    let by = crate::session_log::sid_actor(TWO).expect("a minted session id");
    let mut two = rig.claimant(TWO, "approvals,turn-end");
    two.open(TWO);
    assert_eq!(two.record(1, rule(APPROVE_ALL)), (1, None));
    assert_eq!(
        two.record(2, rule(ANSWER)),
        (2, scope()),
        "a question's act"
    );
    assert_eq!(two.record(3, wait(WaitReason::Working, None)), (3, scope()));
    assert_eq!(two.record(4, held(Some(HeldBy::NoWork))), (4, None));
    assert_eq!(
        two.record(5, need(NeedKind::Review, "review the plan", &["  \t "])),
        (5, Some("refused:usage".to_string())),
        "a need that folds to nothing is no set `harness_needs` tells"
    );
    assert_eq!(
        two.record(
            6,
            need(
                NeedKind::Review,
                "review the plan",
                &["read the plan", "say yes"]
            )
        ),
        (6, None)
    );
    assert_eq!(harness_needs(&rig, TWO), ["review"], "the fold raised it");
    assert_eq!(
        told(&rig, TWO),
        Some(vec!["read the plan".to_string(), "say yes".to_string()])
    );
    assert_eq!(two.record(7, land_held(2)), (7, None));
    assert_eq!(
        meta(&rig, TWO, |meta| (
            meta.self_held(now()),
            meta.self_land(now()).map(|glass| glass.held)
        )),
        (Some(acts::HeldBy::NoWork), Some(kept_hold(2)))
    );
    assert_eq!(two.record(8, clear(6)), (8, None));
    assert!(harness_needs(&rig, TWO).is_empty(), "the clear lifted it");
    assert_eq!(told(&rig, TWO), None, "and took the set down");
    assert_eq!(applied(&rig, TWO), ["act", "clear", "land", "need"]);
    assert_eq!(
        meta(&rig, TWO, |meta| meta.continuation_activity.clone()),
        None
    );

    // Every row TWO's records wrote: the act as `meta set harness_act` writes it, the need and
    // its clear, each with all four stamps.
    let rows: Vec<Rec> = stream_rows(&rig, TWO)
        .into_iter()
        .filter(|rec| rec.k != Kind::Agent)
        .collect();
    let read: Vec<_> = rows
        .iter()
        .map(|rec| {
            (
                rec.k,
                rec.f.get_u(Key::Hseq),
                rec.f.get_word(Key::Word),
                rec.f.get_str(Key::HarnessAct),
                rec.f.get_str(Key::Text),
            )
        })
        .collect();
    assert_eq!(
        read,
        [
            (Kind::Act, Some(1), Some("approve"), Some(APPROVE_ALL), None),
            (
                Kind::Need,
                Some(6),
                Some("review"),
                None,
                Some("review the plan")
            ),
            (Kind::Clear, Some(8), Some("review"), None, None),
        ]
    );
    let holder = holder();
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
            &format!("by={TWO}"),
            &format!("holder={holder}"),
        ] {
            assert!(line.contains(word), "{word} in {line}");
        }
    }
}

/// A BURST OF `act` RECORDS WITHIN THE STREAM'S BUDGET IS ALL WRITTEN: with the session's whole
/// `mark` budget spent, so `meta set harness_act`'s arm is refused, the stream's `RATE_BURST`
/// records are each applied and each writes its row; they neither read nor spend that budget.
/// NEGATIVE HALF: the stream's own budget is the one a record has, so the record past it is
/// acknowledged `dropped=rate` and writes nothing; and the `mark` arm is still refused after.
#[test]
fn a_burst_of_act_records_within_the_stream_budget_is_all_written() {
    let rig = Rig::new(2);
    let by = crate::session_log::sid_actor(ONE).expect("a minted session id");
    let holder = holder();
    hand_clock(&rig, ONE);
    let mut stream = rig.claimant(ONE, "approvals");
    stream.open(ONE);

    let refused = |rig: &Rig| {
        timeline(rig, ONE, |timeline| {
            timeline
                .note_harness_act(APPROVE_ALL, &holder, by)
                .is_none()
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
    assert!(refused(&rig), "the `mark` arm is refused now");

    let streamed = |rig: &Rig| {
        of(rig, ONE, Kind::Act)
            .iter()
            .filter(|rec| rec.src == Src::Harness)
            .count()
    };
    let burst = u64::from(RATE_BURST);
    for seq in 1..=burst {
        assert_eq!(
            stream.record(seq, rule(APPROVE_ALL)),
            (seq, None),
            "record {seq}"
        );
    }
    assert_eq!(streamed(&rig), usize::try_from(burst).expect("a count"));
    assert_eq!(rig.state(ONE).dropped, 0);
    assert_eq!(
        stream.record(burst + 1, rule(APPROVE_ALL)),
        (burst + 1, Some("rate".to_string())),
        "NEGATIVE HALF: past the stream's own budget"
    );
    assert_eq!(
        streamed(&rig),
        usize::try_from(burst).expect("a count"),
        "the record past the budget wrote nothing"
    );
    assert!(
        refused(&rig),
        "the stream's rows gave the `mark` budget nothing"
    );
}

/// A `land` RECORD SAYING `landed` FINISHES THE TASK ONLY WHERE THE REPOSITORY READER AND `sys
/// left` AGREE: with the repository reading clean and `sys left` reading no row, the session's
/// open task is marked `done why=landed left=0`. Each `landed` is applied either way: it lifts
/// the claim's Land hold and is the `landed@v1` act, written through the stream's act arm.
/// NEGATIVE HALF: a repository read that could not read it, a `sys left` that could not say
/// (unchecked, or not asked) and one that found a process the agent left running each leave the
/// task open; and under a claim without `turn-end` the record is out of scope and checks
/// nothing.
#[test]
fn a_landed_record_finishes_the_task_only_when_the_repo_and_sys_left_agree() {
    let rig = Rig::new(2);
    let now = crate::metrics::now_us;
    let mut stream = rig.claimant(ONE, "turn-end");
    stream.open(ONE);
    for sid in [ONE, TWO] {
        let handle = rig.service.handle(sid);
        crate::task_record::mark_session_task(
            &handle.ctx,
            handle.local_id,
            &TaskMark::new(TaskState::Working),
        )
        .expect("the task opens");
    }
    let task = |rig: &Rig, sid: &str| {
        timeline(rig, sid, |timeline| {
            timeline
                .task()
                .map(|record| (record.state, record.why, record.left.clone()))
        })
    };
    let reads = |rig: &Rig, repo: RepoRead, left: LeftRead| {
        *rig.service
            .repo
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = repo;
        *rig.service
            .left
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = left;
    };
    let working = Some((TaskState::Working, None, None));
    let one_left = LeftRead::Rows(vec![aterm_uds::process::Identity {
        pid: 4242,
        birth: aterm_uds::process::Birth(7),
        idversion: None,
    }]);

    assert_eq!(stream.record(1, land_held(1)), (1, None));
    assert!(meta(&rig, ONE, |meta| meta.self_land(now()).is_some()));
    for (seq, repo, left) in [
        (2, RepoRead::Unread, LeftRead::Rows(Vec::new())),
        (3, RepoRead::Clean, LeftRead::Unchecked),
        (4, RepoRead::Clean, LeftRead::NotAsked),
        (5, RepoRead::Clean, one_left),
    ] {
        reads(&rig, repo, left);
        assert_eq!(stream.record(seq, landed()), (seq, None), "record {seq}");
        assert_eq!(task(&rig, ONE), working, "record {seq} finished nothing");
    }
    assert!(
        meta(&rig, ONE, |meta| meta.self_land(now()).is_none()),
        "a landed record lifts the hold"
    );
    let landed_rows = |rig: &Rig| {
        of(rig, ONE, Kind::Act)
            .iter()
            .filter(|rec| {
                rec.src == Src::Harness && rec.f.get_str(Key::HarnessAct) == Some(acts::RULE_LANDED)
            })
            .count()
    };
    assert_eq!(landed_rows(&rig), 4, "each is the `landed@v1` act");

    // Both agree.
    reads(&rig, RepoRead::Clean, LeftRead::Rows(Vec::new()));
    assert_eq!(stream.record(6, landed()), (6, None));
    assert_eq!(
        task(&rig, ONE),
        Some((
            TaskState::Done,
            Some(crate::task_record::WHY_LANDED),
            Some("0".to_string())
        ))
    );
    assert_eq!(landed_rows(&rig), 5);
    assert_eq!(applied(&rig, ONE), ["land"]);

    // NEGATIVE HALF: a claim without `turn-end` cannot say it landed, whatever the readers say.
    let mut other = rig.claimant(TWO, "state");
    other.open(TWO);
    assert_eq!(other.record(1, landed()), (1, scope()));
    assert_eq!(task(&rig, TWO), working);
}

/// `model` AND `upgrade` ARE KEPT IN THE CURRENT-STATE RECORD, under any grant (they need only
/// the open stream): the latest of each, its words and its line as the codec writes it, with no
/// log row. While kept, `status upgrade=` and the survey row's `upgrade=` read the record in the
/// engine row's grammar (`<phase>/<to>/-/<age>`), and the survey's `model=` and `effort=` read
/// its words.
/// NEGATIVE HALF: once the claim stops feeding the kinds (released), nothing of either is kept
/// and every reader reads what it read before the stream sent them.
#[test]
fn model_and_upgrade_records_are_kept_in_the_current_state_record() {
    let rig = Rig::new(2);
    hand_clock(&rig, ONE);
    let mut stream = rig.claimant(ONE, "-");
    let id = stream.id();
    let epoch = stream.open(ONE).epoch;
    let upgrade_word = |rig: &Rig| {
        let now_s = aterm_time::unix_millis().unwrap_or(0) / 1000;
        let awake_s = aterm_uds::process::awake_ms().unwrap_or(0) / 1000;
        let status = timeline(rig, ONE, |timeline| timeline.upgrade_column(now_s, awake_s));
        let row = survey(rig, ONE, "upgrade");
        assert_eq!(
            status.rsplit_once('/').map(|(head, _)| head),
            row.rsplit_once('/').map(|(head, _)| head),
            "status and the survey row read the same word"
        );
        row
    };
    let before = (
        upgrade_word(&rig),
        survey(&rig, ONE, "model"),
        survey(&rig, ONE, "effort"),
    );
    assert_eq!(before, ("-".to_string(), "-".to_string(), "-".to_string()));

    assert_eq!(
        stream.record(1, model(Some("claude-opus-5"), Some("high"))),
        (1, None)
    );
    assert_eq!(
        stream.record(2, upgrade(UpgradePhase::Waiting, "2.1.330")),
        (2, None)
    );
    assert_eq!(applied(&rig, ONE), ["model", "upgrade"]);
    let kept = kinds(&rig, ONE);
    assert_eq!(
        kept.model,
        Some(StreamModel {
            model: Some("claude-opus-5".to_string()),
            effort: Some("high".to_string()),
            key: Some(true),
            offers: vec![(
                "claude-opus-5".to_string(),
                vec!["low".to_string(), "high".to_string()]
            )],
            permission: Some("auto".to_string()),
            epoch: epoch.clone(),
            hseq: 1,
            line: line(1, model(Some("claude-opus-5"), Some("high"))),
        })
    );
    let kept_upgrade = kept.upgrade.expect("the upgrade is kept");
    assert_eq!(
        (
            kept_upgrade.phase,
            kept_upgrade.to.as_str(),
            kept_upgrade.epoch.as_str(),
            kept_upgrade.hseq,
            kept_upgrade.line.as_str()
        ),
        (
            "waiting",
            "2.1.330",
            epoch.as_str(),
            2,
            line(2, upgrade(UpgradePhase::Waiting, "2.1.330")).as_str()
        )
    );
    assert!(
        upgrade_word(&rig).starts_with("waiting/2.1.330/-/"),
        "{}",
        upgrade_word(&rig)
    );
    assert_eq!(survey(&rig, ONE, "model"), "claude-opus-5");
    assert_eq!(survey(&rig, ONE, "effort"), "high");
    assert!(
        stream_rows(&rig, ONE)
            .iter()
            .all(|rec| rec.k == Kind::Agent),
        "neither writes a row"
    );

    // The latest of each is kept.
    assert_eq!(
        stream.record(3, model(Some("claude-opus-5"), Some("max"))),
        (3, None)
    );
    assert_eq!(
        stream.record(4, upgrade(UpgradePhase::Done, "2.1.330")),
        (4, None)
    );
    let kept = kinds(&rig, ONE);
    assert_eq!(
        kept.model
            .as_ref()
            .map(|fed| (fed.effort.as_deref(), fed.hseq)),
        Some((Some("max"), 3))
    );
    assert_eq!(
        kept.upgrade.as_ref().map(|fed| (fed.phase, fed.hseq)),
        Some(("done", 4))
    );
    assert_eq!(survey(&rig, ONE, "effort"), "max");
    assert!(upgrade_word(&rig).starts_with("done/2.1.330/-/"));

    // NEGATIVE HALF: the claim is released, and nothing of either kind is kept.
    assert!(crate::session_timeline::release_supervisor(
        &rig.service.handle(ONE).ctx,
        Some(id),
        crate::metrics::now_us(),
        Actor::Ctl,
    ));
    let kept = kinds(&rig, ONE);
    assert_eq!((kept.model, kept.upgrade), (None, None));
    assert_eq!(
        (
            upgrade_word(&rig),
            survey(&rig, ONE, "model"),
            survey(&rig, ONE, "effort"),
        ),
        before
    );
}

/// A tab the built-in sources read: a foreground group, a Claude Code screen, a footer naming
/// a model and an effort, and the engine's live-upgrade row offering 2.1.310.
fn built_in_tab(rig: &Rig, sid: &str) {
    const PGID: i32 = 4242;
    hand_clock(rig, sid);
    timeline(rig, sid, |timeline| {
        timeline.note_foreground_group(PGID);
    });
    screen(rig, sid, "busy", None);
    timeline(rig, sid, |timeline| {
        assert!(timeline.set_claude_footer(
            PGID,
            Some(1),
            Some(FooterFacts {
                model: Some("footer-model".to_string()),
                effort: Some("low".to_string()),
                ..FooterFacts::default()
            }),
        ));
        assert!(
            timeline.set_upgrade(Some(aterm_agent::harness::upgrade_drive::Row {
                tab: sid.to_string(),
                from: "2.1.305".to_string(),
                to: "2.1.310".to_string(),
                phase: aterm_agent::harness::upgrade::Phase::Pending,
                wait: "background".to_string(),
                ..aterm_agent::harness::upgrade_drive::Row::default()
            }))
        );
    });
}

/// The words a tab's readers show of its model and its upgrade: the survey row's `model=`,
/// `effort=` and `upgrade=` (its state and target: the engine's reason and every age move with
/// the clock), and the footer's model and effort.
fn model_words(rig: &Rig, sid: &str) -> (String, String, String, Option<String>, Option<String>) {
    let upgrade = survey(rig, sid, "upgrade");
    let shown =
        timeline(rig, sid, |timeline| timeline.claude_footer_shown()).expect("the footer is shown");
    (
        survey(rig, sid, "model"),
        survey(rig, sid, "effort"),
        upgrade.split('/').take(2).collect::<Vec<_>>().join("/"),
        shown.model,
        shown.effort,
    )
}

/// A STREAM TAB THAT NEVER SENDS `upgrade` OR `model` KEEPS THE BUILT-IN WORDS: two stream tabs,
/// both read by the footer and the engine's upgrade row; the one whose stream applied `model`
/// and `upgrade` reads the records (the footer's model and effort readers and the engine-filled
/// `upgrade=` stand down), and the other, whose stream applied only `state`, reads the footer's
/// model and effort and the engine's `upgrade=` throughout.
/// NEGATIVE HALF: before its stream sends either kind, the first tab reads the built-in words
/// too; a `model` record that names no model reads `-` though the footer names one (the stand-
/// down is the kind's, not the word's); and once its claim is released it reads the built-in
/// words again, the engine's row kept beneath the record.
#[test]
fn a_stream_tab_that_never_sends_upgrade_or_model_keeps_the_built_in_words() {
    let rig = Rig::new(2);
    built_in_tab(&rig, ONE);
    built_in_tab(&rig, TWO);
    let mut one = rig.claimant(ONE, "state");
    let id = one.id();
    one.open(ONE);
    let mut two = rig.claimant(TWO, "state");
    two.open(TWO);
    assert_eq!(one.record(1, state(State::Working)), (1, None));
    assert_eq!(two.record(1, state(State::Working)), (1, None));
    let engine_word = model_words(&rig, ONE).2;
    assert!(
        engine_word.ends_with("/2.1.310"),
        "the engine's row: {engine_word}"
    );
    let built = (
        "footer-model".to_string(),
        "low".to_string(),
        engine_word,
        Some("footer-model".to_string()),
        Some("low".to_string()),
    );
    assert_eq!(
        model_words(&rig, ONE),
        built,
        "NEGATIVE HALF: nothing sent yet"
    );
    assert_eq!(model_words(&rig, TWO), built);

    assert_eq!(
        one.record(2, model(Some("claude-opus-5"), Some("high"))),
        (2, None)
    );
    assert_eq!(
        one.record(3, upgrade(UpgradePhase::Moving, "2.1.330")),
        (3, None)
    );
    let streamed = (
        "claude-opus-5".to_string(),
        "high".to_string(),
        "moving/2.1.330".to_string(),
        Some("claude-opus-5".to_string()),
        Some("high".to_string()),
    );
    assert_eq!(model_words(&rig, ONE), streamed);
    assert_eq!(
        model_words(&rig, TWO),
        built,
        "the other tab keeps its own words"
    );
    assert_eq!(applied(&rig, TWO), ["state"]);

    // A record that names no model says none, though the footer names one.
    assert_eq!(one.record(4, model(None, Some("high"))), (4, None));
    let unsaid = model_words(&rig, ONE);
    assert_eq!((unsaid.0.as_str(), unsaid.3), ("-", None));

    // The claim is released: the built-in words again, the engine's row kept beneath.
    assert!(crate::session_timeline::release_supervisor(
        &rig.service.handle(ONE).ctx,
        Some(id),
        crate::metrics::now_us(),
        Actor::Ctl,
    ));
    assert_eq!(model_words(&rig, ONE), built);
    assert_eq!(model_words(&rig, TWO), built);
}

/// THE `meta set harness_*` WRITERS STAND DOWN FOR EACH KIND THE STREAM APPLIED: once the
/// claim's stream has applied an `act`, the same claimant's `meta set harness_act`,
/// `harness_held` and `harness_activity` are answered `ERR usage: stream` and keep nothing;
/// once it has applied a `land`, `harness_land` is; once a `need`, `harness_needs` (`set` and
/// `unset` alike) is.
/// NEGATIVE HALF: before any of them, and for each kind not yet applied, every writer writes
/// (`OK`), a `state` record standing none of them down; and another claimant's claim, placed
/// once the first is released, writes them all again.
#[test]
fn the_meta_set_harness_writers_stand_down_for_each_kind_the_stream_applied() {
    let rig = Rig::new(2);
    let mut stream = rig.claimant(ONE, "state,approvals,questions,walls,turn-end");
    let id = stream.id();
    stream.open(ONE);
    let mut lines = rig.dial("owner");
    // The observation's sequence stays 0, below every record's `seq`, so the stream's `wait`
    // is newer than any the line kept.
    let answers = |lines: &mut Client| -> [String; 5] {
        [
            format!("set harness_act {APPROVE_ALL}"),
            "set harness_held no-work".to_string(),
            "set harness_activity reason=composing wait_ms=5000 seq=0".to_string(),
            format!("set harness_land since={WALL} nudges=1"),
            "set harness_needs entry=review%20the%20plan need=read%20the%20plan".to_string(),
        ]
        .map(|rest| lines.request(&format!("meta {ONE} {rest}")))
    };
    let ok = || "OK".to_string();
    let stood = || "ERR usage: stream".to_string();

    assert_eq!(answers(&mut lines), [ok(), ok(), ok(), ok(), ok()]);
    assert_eq!(stream.record(1, state(State::Working)), (1, None));
    assert_eq!(
        answers(&mut lines),
        [ok(), ok(), ok(), ok(), ok()],
        "NEGATIVE HALF: `state` stands no writer down"
    );

    let activity = |rig: &Rig| meta(rig, ONE, |meta| meta.continuation_activity.clone());
    assert_eq!(stream.record(2, wait(WaitReason::Dialog, None)), (2, None));
    let fed = activity(&rig);
    assert_eq!(
        fed.as_ref()
            .map(|(_, activity)| (activity.sequence, activity.reason.as_str())),
        Some((2, "dialog")),
        "the stream's `wait` is the observation"
    );
    assert_eq!(
        answers(&mut lines),
        [stood(), stood(), stood(), ok(), ok()],
        "`act` stands its three writers down"
    );
    assert_eq!(activity(&rig), fed, "the stood-down writer kept nothing");
    assert_eq!(
        lines.request(&format!("meta {ONE} unset harness_held")),
        stood()
    );

    assert_eq!(stream.record(3, land_open()), (3, None));
    assert_eq!(
        answers(&mut lines),
        [stood(), stood(), stood(), stood(), ok()]
    );
    assert_eq!(
        meta(&rig, ONE, |meta| meta.self_land(crate::metrics::now_us())),
        None,
        "the stood-down `harness_land` kept no hold"
    );

    assert_eq!(
        stream.record(4, need(NeedKind::Decision, "decide", &["pick one"])),
        (4, None)
    );
    assert_eq!(
        answers(&mut lines),
        [stood(), stood(), stood(), stood(), stood()]
    );
    assert_eq!(
        lines.request(&format!("meta {ONE} unset harness_needs")),
        stood()
    );
    assert_eq!(
        told(&rig, ONE),
        Some(vec!["pick one".to_string()]),
        "the set is the stream's"
    );

    // NEGATIVE HALF: another claimant, once the first is released, writes them all.
    assert!(crate::session_timeline::release_supervisor(
        &rig.service.handle(ONE).ctx,
        Some(id),
        crate::metrics::now_us(),
        Actor::Ctl,
    ));
    assert_eq!(
        lines.request(&format!(
            "claim {ONE} 4 state,approvals,questions,walls,turn-end other"
        )),
        "OK claim"
    );
    assert_eq!(answers(&mut lines), [ok(), ok(), ok(), ok(), ok()]);
}

/// What the five `meta set harness_*` writers the act kinds replace answer for `sid`, on
/// `lines`: `harness_act`, `harness_held`, `harness_activity` (its `seq=` far below every record
/// it is compared with, which is none: the two count apart), `harness_land` and `harness_needs`.
fn writers(lines: &mut Client, sid: &str) -> [String; 5] {
    [
        format!("set harness_act {APPROVE_ALL}"),
        "set harness_held no-work".to_string(),
        "set harness_activity reason=composing wait_ms=5000 seq=0".to_string(),
        format!("set harness_land since={WALL} nudges=1"),
        "set harness_needs entry=review%20the%20plan need=read%20the%20plan".to_string(),
    ]
    .map(|rest| lines.request(&format!("meta {sid} {rest}")))
}

/// Set `sid`'s task to working, the task a landing may finish.
fn open_task(rig: &Rig, sid: &str) {
    let handle = rig.service.handle(sid);
    crate::task_record::mark_session_task(
        &handle.ctx,
        handle.local_id,
        &TaskMark::new(TaskState::Working),
    )
    .expect("the task opens");
}

/// `sid`'s task: its state, why and `left=`.
fn task_of(rig: &Rig, sid: &str) -> Option<(TaskState, Option<&'static str>, Option<String>)> {
    timeline(rig, sid, |timeline| {
        timeline
            .task()
            .map(|record| (record.state, record.why, record.left.clone()))
    })
}

/// What the land check's stand-in readers answer: the repository and `sys left`.
fn land_reads(rig: &Rig, repo: RepoRead, left: LeftRead) {
    *rig.service
        .repo
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = repo;
    *rig.service
        .left
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = left;
}

/// THE WINDOW'S LAND CHECK READS NOTHING OF A CLAIMANT IT CANNOT FIND, AND SAYS SO: where no
/// snapshot names the claimant and its working directory cannot be read (here no process has
/// its pid), the repository reads unread, which is not clean, so a `landed` finishes nothing
/// though `sys left` found no row.
/// NEGATIVE HALF: the same task, the same claim and the same empty `sys left` finish it once the
/// repository reads clean.
#[test]
fn a_land_check_that_finds_no_live_claimant_reads_the_repository_unread() {
    let gone = Proven {
        pid: 2_000_000_000,
        ..running()
    };
    let read = super::super::live_repo(ONE, &gone, None, None);
    assert_eq!(read, RepoRead::Unread, "nothing was read");
    assert!(!read.is_clean());

    let rig = Rig::new(2);
    let mut stream = rig.claimant(ONE, "turn-end");
    stream.open(ONE);
    open_task(&rig, ONE);
    land_reads(&rig, read, LeftRead::Rows(Vec::new()));
    assert_eq!(stream.record(1, landed()), (1, None));
    assert_eq!(task_of(&rig, ONE), Some((TaskState::Working, None, None)));
    land_reads(&rig, RepoRead::Clean, LeftRead::Rows(Vec::new()));
    assert_eq!(stream.record(2, landed()), (2, None));
    assert_eq!(
        task_of(&rig, ONE).map(|(state, why, _)| (state, why)),
        Some((TaskState::Done, Some(crate::task_record::WHY_LANDED)))
    );
}

/// THE NEEDS A STREAM RAISED ARE LIFTED WHEN ITS CLAIM ENDS: two needs raised by the stream of
/// a claim of `approvals` and `turn-end` stand (the fold raised them as the harness's) until the
/// claim is released; then each is lifted by `mark - clear word=<its word>` through the stream's
/// arm (`src=harness conf=claim by=<the tab> holder=<the claim's holder>`, no `hseq=`, as no
/// record sent it), and the stream holds none open.
/// NEGATIVE HALF: the same claimant claims again and opens its stream again: what was
/// acknowledged stands, a `clear` of a need the ended claim's stream raised is applied and
/// writes nothing, and a need it raises now stands.
#[test]
fn the_needs_a_stream_raised_are_lifted_when_its_claim_ends() {
    let rig = Rig::new(2);
    let by = crate::session_log::sid_actor(TWO).expect("a minted session id");
    let holder = holder();
    let mut two = rig.claimant(TWO, "approvals,turn-end");
    let id = two.id();
    two.open(TWO);
    assert_eq!(
        two.record(
            1,
            need(NeedKind::Review, "review the plan", &["read the plan"])
        ),
        (1, None)
    );
    assert_eq!(
        two.record(2, need(NeedKind::Decision, "decide", &["pick one"])),
        (2, None)
    );
    let raised = |rig: &Rig| {
        let mut words = harness_needs(rig, TWO);
        words.sort_unstable();
        words
    };
    assert_eq!(raised(&rig), ["decision", "review"]);

    assert!(crate::session_timeline::release_supervisor(
        &rig.service.handle(TWO).ctx,
        Some(id),
        crate::metrics::now_us(),
        Actor::Ctl,
    ));
    assert!(
        raised(&rig).is_empty(),
        "the claim's end lifted what its stream raised"
    );
    let mut lifts: Vec<_> = of(&rig, TWO, Kind::Clear)
        .iter()
        .map(|rec| {
            (
                rec.f.get_word(Key::Word),
                rec.f.get_u(Key::Hseq),
                rec.src,
                rec.conf,
                rec.by,
                rec.f.get_str(Key::Holder).map(str::to_string),
            )
        })
        .collect();
    lifts.sort_unstable_by_key(|lift| lift.0);
    let lift = |word| {
        (
            Some(word),
            None,
            Src::Harness,
            Conf::Claim,
            by,
            Some(holder.clone()),
        )
    };
    assert_eq!(lifts, [lift("decision"), lift("review")]);
    assert!(
        rig.state(TWO).needs.is_empty(),
        "the stream holds none open"
    );

    // NEGATIVE HALF: the same claimant, claimed again.
    let mut again = rig.claimant(TWO, "approvals,turn-end");
    assert_eq!(again.open(TWO).acked, 2, "what was acknowledged stands");
    assert_eq!(again.record(3, clear(1)), (3, None));
    assert_eq!(
        of(&rig, TWO, Kind::Clear).len(),
        2,
        "a clear of the ended claim's need writes nothing"
    );
    assert_eq!(
        again.record(4, need(NeedKind::Help, "help", &["sign in"])),
        (4, None)
    );
    assert_eq!(raised(&rig), ["help"]);
}

/// A STREAM'S `wait` IS KEPT OVER WHAT A `meta set harness_activity` LINE KEPT IN THE SAME
/// REVISION, WHATEVER `seq=` THAT LINE CHOSE: the line's `seq=` and the stream's record `seq`
/// count apart, so the stream's first `wait` (record 1) replaces the line's report of `seq=100`,
/// and the stream's own count orders its later ones.
/// NEGATIVE HALF: the line's report is the observation until the stream sends one; a stream
/// record at or below the one kept moves nothing; and once the stream applied `act` the line
/// is the stream's (`ERR usage: stream`) and keeps nothing.
#[test]
fn a_streams_wait_is_kept_over_a_meta_lines_report_whatever_its_seq() {
    let rig = Rig::new(2);
    let mut stream = rig.claimant(ONE, "state");
    stream.open(ONE);
    let mut lines = rig.dial("owner");
    let revision = meta(&rig, ONE, |meta| meta.supervisor_revision);
    let kept = |rig: &Rig| {
        meta(rig, ONE, |meta| {
            meta.continuation_activity.clone().map(|(rev, activity)| {
                (
                    rev,
                    activity.sequence,
                    activity.reason,
                    activity.from_stream,
                )
            })
        })
    };
    assert_eq!(
        lines.request(&format!(
            "meta {ONE} set harness_activity reason=working wait_ms=- seq=100"
        )),
        "OK"
    );
    assert_eq!(
        kept(&rig),
        Some((revision, 100, "working".to_string(), false)),
        "NEGATIVE HALF: the line's report stands until the stream sends one"
    );

    assert_eq!(stream.record(1, wait(WaitReason::Person, None)), (1, None));
    assert_eq!(
        kept(&rig),
        Some((revision, 1, "person".to_string(), true)),
        "the stream's first `wait` replaces the line's, though 1 < 100"
    );
    assert_eq!(stream.record(2, wait(WaitReason::Working, None)), (2, None));
    assert_eq!(kept(&rig), Some((revision, 2, "working".to_string(), true)));
    assert_eq!(
        lines.request(&format!(
            "meta {ONE} set harness_activity reason=person wait_ms=- seq=101"
        )),
        "ERR usage: stream"
    );
    assert_eq!(
        kept(&rig),
        Some((revision, 2, "working".to_string(), true)),
        "the stood-down line kept nothing"
    );
}

/// WHAT A CLAIM'S STREAM APPLIED ENDS WITH THE CLAIM: with `act`, `land` and `need` applied,
/// the same claimant's five `meta set harness_*` writers are the stream's (`ERR usage:
/// stream`); once the claim is released its applied kinds are none, and when the same claimant
/// (the same process) claims again, with no stream of the new claim having applied any kind,
/// each writer writes (`OK`).
/// NEGATIVE HALF: the stream it opens again on the new claim resends only what follows (its
/// `acked` kept), and an `act` it applies there stands the act's three writers down again.
#[test]
fn a_claims_applied_kinds_end_with_it_and_its_claimant_writes_again_once_it_claims_again() {
    let rig = Rig::new(2);
    let duties = "state,approvals,questions,walls,turn-end";
    let mut stream = rig.claimant(ONE, duties);
    let id = stream.id();
    stream.open(ONE);
    let mut lines = rig.dial("owner");
    assert_eq!(stream.record(1, rule(APPROVE_ALL)), (1, None));
    assert_eq!(stream.record(2, land_open()), (2, None));
    assert_eq!(
        stream.record(3, need(NeedKind::Decision, "decide", &["pick one"])),
        (3, None)
    );
    assert_eq!(applied(&rig, ONE), ["act", "land", "need"]);
    let ok = || "OK".to_string();
    let stood = || "ERR usage: stream".to_string();
    assert_eq!(
        writers(&mut lines, ONE),
        [stood(), stood(), stood(), stood(), stood()],
        "NEGATIVE HALF: while the claim stands, each is the stream's"
    );

    assert!(crate::session_timeline::release_supervisor(
        &rig.service.handle(ONE).ctx,
        Some(id),
        crate::metrics::now_us(),
        Actor::Ctl,
    ));
    assert!(
        applied(&rig, ONE).is_empty(),
        "the claim's end ends what its stream applied"
    );
    let mut again = rig.claimant(ONE, duties);
    assert_eq!(
        writers(&mut lines, ONE),
        [ok(), ok(), ok(), ok(), ok()],
        "the same claimant's new claim: no stream of it applied any kind"
    );

    assert_eq!(again.open(ONE).acked, 3, "what was acknowledged stands");
    assert_eq!(again.record(4, rule(APPROVE_ALL)), (4, None));
    assert_eq!(
        writers(&mut lines, ONE),
        [stood(), stood(), stood(), ok(), ok()]
    );
}

/// A `landed` RECORD FINISHES THE TASK ONLY UNDER LAND (or no known mode): with the
/// repository clean and `sys left` empty, a session whose own Autopilot word is `cruise` or
/// `off` keeps its task working (nothing ends a Cruise or an Off task), as a proven
/// self-claim's landed face is held.
/// NEGATIVE HALF: the same session under `land` has the same `landed` finish it.
#[test]
fn a_landed_record_finishes_nothing_under_cruise_or_off() {
    let rig = Rig::new(2);
    let mut stream = rig.claimant(ONE, "turn-end");
    stream.open(ONE);
    open_task(&rig, ONE);
    land_reads(&rig, RepoRead::Clean, LeftRead::Rows(Vec::new()));
    let autopilot = |rig: &Rig, word: &str| {
        rig.service
            .handle(ONE)
            .ctx
            .meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .autopilot = Some(word.to_string());
    };
    for (seq, word) in [(1, "cruise"), (2, "off")] {
        autopilot(&rig, word);
        assert_eq!(stream.record(seq, landed()), (seq, None), "{word}");
        assert_eq!(
            task_of(&rig, ONE),
            Some((TaskState::Working, None, None)),
            "{word}: the task stays open"
        );
    }
    autopilot(&rig, "land");
    assert_eq!(stream.record(3, landed()), (3, None));
    assert_eq!(
        task_of(&rig, ONE),
        Some((
            TaskState::Done,
            Some(crate::task_record::WHY_LANDED),
            Some("0".to_string())
        ))
    );
}

/// A `landed` THAT ARRIVES WHILE THE SESSION'S LAND CHECK RUNS IS CHECKED AGAIN ONCE IT ENDS:
/// the window's check runs one per session at a time, and an ask made while one runs keeps its
/// claimant (the newest one's) for the running thread to check once more, with its own check,
/// when the first ends; then the session's place is given back, so a later ask is checked too.
/// NEGATIVE HALF: an ask made while a check runs starts no check of its own.
#[test]
fn a_landed_ask_while_a_check_runs_is_checked_once_the_check_ends() {
    use std::sync::mpsc;
    let handle = session(9, "t00000000c6");
    let claimant = |pid: u32| Proven { pid, ..running() };
    let calls: Arc<Mutex<Vec<Proven>>> = Arc::new(Mutex::new(Vec::new()));
    let seen = |calls: &Arc<Mutex<Vec<Proven>>>| {
        calls.lock().unwrap_or_else(PoisonError::into_inner).clone()
    };
    let wait_for = |calls: &Arc<Mutex<Vec<Proven>>>, n: usize| {
        let deadline = Instant::now() + SETTLE;
        while seen(calls).len() < n {
            assert!(
                Instant::now() < deadline,
                "the checks seen: {:?}",
                seen(calls)
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    };
    let (release, released) = mpsc::channel::<()>();
    let check = {
        let calls = Arc::clone(&calls);
        move |_: &SessionHandle, proven: &Proven| {
            let first = {
                let mut calls = calls.lock().unwrap_or_else(PoisonError::into_inner);
                calls.push(proven.clone());
                calls.len() == 1
            };
            if first {
                let _ = released.recv_timeout(SETTLE);
            }
        }
    };
    let unrun = {
        let calls = Arc::clone(&calls);
        move |_: &SessionHandle, _: &Proven| {
            calls
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(claimant(1));
        }
    };

    super::super::spawn_land_check(&handle, &claimant(11), check);
    wait_for(&calls, 1);
    super::super::spawn_land_check(&handle, &claimant(12), unrun.clone());
    super::super::spawn_land_check(&handle, &claimant(13), unrun.clone());
    assert_eq!(
        seen(&calls),
        [claimant(11)],
        "NEGATIVE HALF: an ask while a check runs starts none"
    );
    release.send(()).expect("the first check waits");
    wait_for(&calls, 2);
    assert_eq!(
        seen(&calls),
        [claimant(11), claimant(13)],
        "the newest ask is checked by the running thread's own check"
    );

    // The place is given back (or the running thread takes the ask): a later ask is checked.
    let later = {
        let calls = Arc::clone(&calls);
        move |_: &SessionHandle, proven: &Proven| {
            calls
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(proven.clone());
        }
    };
    super::super::spawn_land_check(&handle, &claimant(14), later);
    wait_for(&calls, 3);
    assert_eq!(seen(&calls), [claimant(11), claimant(13), claimant(14)]);
}
