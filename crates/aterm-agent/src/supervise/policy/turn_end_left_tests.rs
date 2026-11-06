// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// THE LEFT LEG (`docs/DESIGN-leftovers-by-default-2026-10-30.md` §2): under Land a `Nothing left:`
// defense lands only once each process the session's agent started that still runs was ended or
// kept by a `keep <token>` line, and a view that cannot say holds nothing.

use crate::harness::acts::HoldLeg;
use crate::harness::leftover::{LeftRead, LeftWord, token_of};
use crate::supervise::facts::{EpisodeKey, Fact, FactKind};
use crate::supervise::policy::exception::{Exception, Urgency, exception_for};

/// A row's token as `sys left` prints it.
const TOKEN: &str = "2606@1792098840004410:3";

/// The rows `tokens` name, each re-proved at the yield.
fn rows(tokens: &[&str]) -> LeftRead {
    LeftRead::Rows(
        tokens
            .iter()
            .map(|t| token_of(t).expect("a token"))
            .collect(),
    )
}

/// Land on, its instruction typed at `t`: the state and when the instruction's turn ends.
fn instructed(t: Instant) -> (TurnEndState, Instant) {
    let mut st = TurnEndState::default();
    assert_eq!(
        act(&mut st, &land(idle("Stage 1 landed.", Some(3 * MIN))), t),
        land_instruction()
    );
    (st, t + 3 * MIN)
}

/// A turn of real work that ended on `said`, the host having read `left`.
fn worked(said: &str, left: LeftRead) -> TurnEndReading {
    TurnEndReading {
        leftover: left,
        ..land(idle(said, Some(3 * MIN)))
    }
}

/// A short turn that ran a tool (`aterm ctl end`, `sys left`) and ended on `said`: no real work,
/// but no reply said again with nothing done either.
fn tooled(said: &str, left: LeftRead) -> TurnEndReading {
    TurnEndReading {
        ran_tools: true,
        ..worked(said, left)
    }
    .with_worked(Duration::from_secs(10))
}

/// A short answer `said`, no tool run: an act's no-work answer.
fn short(said: &str, left: LeftRead) -> TurnEndReading {
    TurnEndReading {
        ran_tools: false,
        ..worked(said, left)
    }
    .with_worked(Duration::from_secs(10))
}

trait Worked {
    fn with_worked(self, worked: Duration) -> Self;
}

impl Worked for TurnEndReading {
    fn with_worked(self, worked: Duration) -> Self {
        Self {
            worked: Some(worked),
            ..self
        }
    }
}

/// The decision at `r` at `now`, a wait (the back-off) waited out on the same point read again:
/// the act and when it came.
fn decided(st: &mut TurnEndState, r: &TurnEndReading, now: Instant) -> (TurnEndAction, Instant) {
    let mut now = now;
    let mut a = at(st, r, now);
    let same = TurnEndReading {
        worked: None,
        ..r.clone()
    };
    for _ in 0..10 {
        let TurnEndAction::WaitUntil { until, .. } = a else {
            break;
        };
        now = until;
        a = at(st, &same, now);
    }
    (a, now)
}

/// The text `a` types.
fn typed(a: &TurnEndAction) -> &str {
    match a {
        TurnEndAction::Type { text, .. } => text,
        other => panic!("nothing typed: {other:?}"),
    }
}

/// The legs Land's hold names.
fn legs(st: &TurnEndState) -> Vec<HoldLeg> {
    st.land_held()
        .map(|held| held.legs_held().collect())
        .unwrap_or_default()
}

/// A LEFT ROW HOLDS LAND UNTIL IT IS ENDED OR KEPT: a defense over a clean repository and one row
/// the agent left owes no check; the nudge carries the whole left suffix and the hold names the
/// left leg alone. Once the row is gone the next defense owes the check, and its `LANDED` lands
/// with `left=0`. The same row kept by a keep line, `running` beside it, owes the check at once
/// and lands with `left=kept:1`.
/// NEGATIVE CONTROL: the leg dropped from `land_defended` owes the check at the first defense.
#[test]
fn land_holds_on_a_left_row_until_it_is_ended_or_kept() {
    let (mut st, now) = instructed(t0());
    let r = worked(DEFENSE, rows(&[TOKEN]));
    let (a, now) = decided(&mut st, &r, now);
    assert_eq!(
        a.rule_id(),
        Some(RULE_LAND_NUDGE),
        "no check owed over a left row: {a:?}"
    );
    assert!(!st.land_check_owed());
    assert!(
        typed(&a).contains(&left_suffix(1)),
        "the whole suffix: {a:?}"
    );
    assert_eq!(legs(&st), vec![HoldLeg::Left]);
    assert_eq!(st.land_held().map(|h| h.left), Some(1));
    assert!(!st.landed());
    st.acted(&a, &r, now);
    // The agent ends it (`aterm ctl end <token>`, a tool run) and defends again.
    let ended = tooled(DEFENSE, rows(&[]));
    let (b, now) = decided(&mut st, &ended, now + MIN);
    assert_eq!(
        b.rule_id(),
        Some(RULE_LAND_CHECK),
        "the check once it is gone: {b:?}"
    );
    st.acted(&b, &ended, now);
    let (c, _) = decided(&mut st, &short(LANDED_WORD, rows(&[])), now + MIN);
    assert_eq!(c, TurnEndAction::Nothing, "{st:?}");
    assert!(st.landed(), "LANDED lands: {st:?}");
    assert_eq!(st.landed_left().map(LeftWord::word).as_deref(), Some("0"));

    let (mut st, now) = instructed(t0());
    let kept = format!(
        "{DEFENSE}\n- keep `{TOKEN}`: the preview server the person asked for, still running"
    );
    assert!(
        land_check::hint_clause(&kept).is_none(),
        "the keep line reads no open work"
    );
    let r = worked(&kept, rows(&[TOKEN]));
    let (a, now) = decided(&mut st, &r, now);
    assert_eq!(
        a.rule_id(),
        Some(RULE_LAND_CHECK),
        "a kept row holds nothing: {a:?}"
    );
    st.acted(&a, &r, now);
    let (b, _) = decided(&mut st, &short(LANDED_WORD, rows(&[TOKEN])), now + MIN);
    assert_eq!(b, TurnEndAction::Nothing, "{st:?}");
    assert!(
        st.landed(),
        "the kept row still runs, and LANDED lands: {st:?}"
    );
    assert_eq!(
        st.landed_left().map(LeftWord::word).as_deref(),
        Some("kept:1")
    );
}

/// A VIEW THAT CANNOT SAY, OR ONE WITH NO ROW OF THE AGENT'S (a `name` row is never the agent's
/// to end, so the host gives none), NEVER HOLDS LAND: the partial view lands with
/// `left=unchecked`, the empty one with `left=0`. NEGATIVE CONTROL: one row of the agent's in the
/// same view holds.
#[test]
fn a_name_row_or_a_partial_view_never_holds_land() {
    for (view, word) in [(LeftRead::Unchecked, "unchecked"), (rows(&[]), "0")] {
        let (mut st, now) = instructed(t0());
        let r = worked(DEFENSE, view.clone());
        let (a, now) = decided(&mut st, &r, now);
        assert_eq!(a.rule_id(), Some(RULE_LAND_CHECK), "{view:?}: {a:?}");
        st.acted(&a, &r, now);
        let (b, _) = decided(&mut st, &short(LANDED_WORD, view.clone()), now + MIN);
        assert_eq!(b, TurnEndAction::Nothing, "{view:?}");
        assert!(st.landed(), "{view:?}");
        assert_eq!(
            st.landed_left().map(LeftWord::word).as_deref(),
            Some(word),
            "{view:?}"
        );
    }
    let (mut st, now) = instructed(t0());
    let (a, _) = decided(&mut st, &worked(DEFENSE, rows(&[TOKEN])), now);
    assert_eq!(a.rule_id(), Some(RULE_LAND_NUDGE), "one row holds: {a:?}");
    assert_eq!(legs(&st), vec![HoldLeg::Left]);
}

/// THE LEFT LEG HOLDS AT MOST THREE TIMES AN EPISODE: a row that comes back under a new token
/// after each end holds three defenses; the fourth owes the check, and its `LANDED` lands with
/// the row it left (`left=1`). NEGATIVE CONTROL: a cap of one lands at the second defense, which
/// still holds here.
#[test]
fn the_left_leg_stops_after_three_holds() {
    let tokens = [
        TOKEN,
        "2607@1792098840014410:1",
        "2608@1792098840024410:1",
        "2609@1792098840034410:1",
    ];
    let (mut st, mut now) = instructed(t0());
    for (k, token) in tokens.iter().take(3).enumerate() {
        let r = if k == 0 {
            worked(DEFENSE, rows(&[token]))
        } else {
            tooled(DEFENSE, rows(&[token]))
        };
        let (a, then) = decided(&mut st, &r, now);
        assert_eq!(
            a.rule_id(),
            Some(RULE_LAND_NUDGE),
            "hold {} still holds: {a:?}",
            k + 1
        );
        assert_eq!(legs(&st), vec![HoldLeg::Left], "hold {}", k + 1);
        st.acted(&a, &r, then);
        now = then + MIN;
    }
    let r = tooled(DEFENSE, rows(&[tokens[3]]));
    let (a, then) = decided(&mut st, &r, now);
    assert_eq!(
        a.rule_id(),
        Some(RULE_LAND_CHECK),
        "the leg holds no more: {a:?}"
    );
    st.acted(&a, &r, then);
    let (b, _) = decided(&mut st, &short(LANDED_WORD, rows(&[tokens[3]])), then + MIN);
    assert_eq!(b, TurnEndAction::Nothing, "{st:?}");
    assert!(st.landed());
    assert_eq!(st.landed_left().map(LeftWord::word).as_deref(), Some("1"));
}

/// A SEEDED LANDING STANDS OVER A LEFT ROW: a loop after the one that landed reads the landing's
/// own words again, and the row it reads then holds nothing. NEGATIVE CONTROL: the same reading
/// in a loop that landed nothing holds.
#[test]
fn a_seeded_landing_stands_over_a_left_row() {
    let r = TurnEndReading {
        worked: None,
        ..worked(DEFENSE, rows(&[TOKEN]))
    };
    let mut st = TurnEndState::default();
    st.seed_landed(said_mark(Some(DEFENSE)));
    let a = at(&mut st, &r, t0());
    assert_eq!(a, TurnEndAction::Nothing, "{st:?}");
    assert!(st.landed(), "the seeded landing stands: {st:?}");

    let (mut st, now) = instructed(t0());
    let (a, _) = decided(&mut st, &worked(DEFENSE, rows(&[TOKEN])), now);
    assert_eq!(
        a.rule_id(),
        Some(RULE_LAND_NUDGE),
        "unseeded, it holds: {a:?}"
    );
    assert!(!st.landed());
}

/// A HOLD ON THE LEFT LEG ALONE IS LATER: the same defense said again with nothing done over the
/// row is the plain repeat, `Later` and no notice, and the hold names the left leg alone.
/// NEGATIVE CONTROL: the left leg left out of `hold_at_yield`'s legs names none.
#[test]
fn a_left_only_hold_is_later() {
    let (mut st, mut now) = instructed(t0());
    let r = worked(DEFENSE, rows(&[TOKEN]));
    let (a, then) = decided(&mut st, &r, now);
    assert_eq!(a.rule_id(), Some(RULE_LAND_NUDGE), "{a:?}");
    st.acted(&a, &r, then);
    now = then;
    let again = short(DEFENSE, rows(&[TOKEN]));
    for answers in 1.. {
        assert!(answers <= 4, "never held: {st:?}");
        let (a, then) = decided(&mut st, &again, now + MIN);
        now = then;
        if a == TurnEndAction::Nothing {
            break;
        }
        assert_eq!(a.rule_id(), Some(RULE_LAND_NUDGE), "{a:?}");
        st.acted(&a, &again, now);
    }
    let e = exception_for(&st, &again, &cfg(), now).expect("the repeat is journaled");
    assert_eq!(
        e,
        Exception::repeats(crate::harness::acts::HeldBy::SameReply)
    );
    assert_eq!(e.when, Urgency::Later);
    assert!(e.notice.is_none(), "no notice: {e:?}");
    assert_eq!(legs(&st), vec![HoldLeg::Left]);
}

/// THE LEFT SUFFIX NEVER PUSHES THE REPOSITORY'S ASK OUT: as the room shrinks it rides whole,
/// then short, then not at all, while the ask rides wherever it fits alone. Asserted on the
/// suffixes' own functions, not on their prose. NEGATIVE CONTROL: the left suffix fitted before
/// the repository's ask cuts the ask.
#[test]
fn the_left_suffix_never_pushes_the_repository_ask_out() {
    let repo = RepoRead::Found(crate::harness::repo_work::RepoWork {
        repo: "/Users//a/Developer/parser".to_string(),
        changed: 2,
        untracked: 1,
        ..Default::default()
    });
    let ask = |text: &str| text.contains(&repo_work_suffix(repo.found().expect("found")));
    let mut seen = Vec::new();
    for base_units in (0..VERBATIM_UNITS).step_by(5) {
        let base = "x".repeat(base_units);
        let alone = fit_act(&base, &[Suffix::Repo(&repo, RepoAsk::All)], None).0;
        if !ask(&alone) {
            continue;
        }
        let (text, cut) = fit_act(
            &base,
            &[
                Suffix::Repo(&repo, RepoAsk::All),
                Suffix::Left(2, RepoAsk::All),
            ],
            None,
        );
        assert!(ask(&text) && !cut, "{base_units}: the ask rides: {text}");
        let form = if text.contains(&left_suffix(2)) {
            0
        } else if text.contains(&left_suffix_short(2)) {
            1
        } else {
            2
        };
        if seen.last() != Some(&form) {
            seen.push(form);
        }
    }
    assert_eq!(
        seen,
        vec![0, 1, 2],
        "whole, short, none as the room shrinks"
    );
}

/// THE LEFT SUFFIX RIDES EITHER LEAD OF LAND'S CONTRADICTION: led by git's finding, or by the
/// agent's list standing over a clean repository, the contradiction carries the rows the agent
/// left, whole where they fit, and with none left it names none. Asserted on the suffix's own
/// function, not on its prose. Run against a contradiction whose either lead drops the left
/// suffix from its parts, this test fails.
#[test]
fn the_left_suffix_rides_either_lead_of_lands_contradiction() {
    let found = RepoRead::Found(crate::harness::repo_work::RepoWork {
        repo: "/Users//a/Developer/parser".to_string(),
        unlanded: 1,
        ..Default::default()
    });
    let listed = "The parser's branch is not merged yet.";
    for (repo, list) in [(&found, None), (&RepoRead::Clean, Some(listed))] {
        for left in [0, 2] {
            let text = land_contradicted_left(repo, list, left, &[], None).expect("a lead");
            assert!(text.starts_with(LAND_CONTRADICTED_HEAD), "{text}");
            assert_eq!(
                text.contains(&left_suffix(2)),
                left == 2,
                "{list:?}, {left} left: {text}"
            );
        }
    }
}

/// KEEP LINES ARE LEFT OUT OF THE BODY LAND READS: a keep line under the defense, bulleted or
/// bare, its token in backticks or not, keeps its token; a word that is no token keeps nothing,
/// and a keep line above the defense keeps nothing either. The body Land reads for open work
/// leaves the keep lines out, so `running` beside a kept token reads nothing open.
/// NEGATIVE CONTROL: keep lines left in the body read `running` open.
#[test]
fn keep_lines_are_left_out_of_the_body_land_reads() {
    let other = "2607@1792098840014410:1";
    let linux = "2608@1792098840024410";
    let defense = format!(
        "keep 2609@1792098840034410:1 above the defense\n{DEFENSE}\n- keep `{TOKEN}`: the dev \
         server the person asked for, still running\nKEEP {other}: the docs watcher, still \
         running\n2. keep {linux}, still running\n- keep it running for now\n- keep `{TOKEN}`"
    );
    let kept = needs::kept_tokens(&defense);
    assert_eq!(
        kept,
        [TOKEN, other, linux]
            .iter()
            .map(|t| token_of(t).expect("a token"))
            .collect::<Vec<_>>(),
        "each token once, in order"
    );
    assert!(needs::kept_tokens(DEFENSE).is_empty());
    let only_kept = format!(
        "{DEFENSE}\n- keep `{TOKEN}`: the dev server the person asked for, still running\nkeep \
         {other}, still running"
    );
    assert!(
        !needs::defense_body(&only_kept).contains("running"),
        "{}",
        needs::defense_body(&only_kept)
    );
    assert!(
        clean_defense(Some(&only_kept)),
        "kept rows read nothing open"
    );
    assert!(land_open_found(Some(&only_kept)).is_empty());
    assert!(
        needs::defense_body(&defense).contains("keep it running"),
        "a word that is no token is the reply's own"
    );
}

/// A FINISHED RECORD KEEPS A LANDING OVER A LEFT ROW: the left leg holds only the `LANDED` that
/// would make a landing, so a landing made stands where its record keeps it, over a row kept or
/// not. A loop after the one that landed keeps it at a wall, the episode's books carried or not;
/// and a landing that stood over Off's park (which leaves the episode, its kept tokens with it)
/// stays where the record says finished though the words moved on, as the same park with no
/// finished record withdraws it. NEGATIVE CONTROL: gate the record on the leg, and the unkept row
/// withdraws the landing at the wall.
#[test]
fn a_finished_record_keeps_a_landing_over_a_left_row() {
    let restored = |kept: bool| {
        let now = t0();
        let mut st = TurnEndState::default();
        st.seed_acts(&[5 * MIN], now);
        st.seed_landed(None);
        st.seed_land(0, false, false, false);
        let mut books = LandBooks::default();
        if kept {
            books.kept[KEPT_TOKENS - 1] = token_of(TOKEN);
        }
        st.seed_land_books(books);
        let mut limit = land(TurnEndReading {
            finished_record: Some(true),
            leftover: rows(&[TOKEN]),
            ..quiet_walled(
                WallKind::UsageWeekly,
                "You've hit your weekly limit · resets 9am (America/Los_Angeles)",
                None,
            )
        });
        limit.reset_at = Some(now + 30 * MIN);
        at(&mut st, &limit, now);
        st
    };
    assert!(restored(true).landed(), "a kept row holds nothing");
    assert!(restored(false).landed(), "nor does an unkept one");
    for (record, stands) in [(Some(true), true), (None, false)] {
        let (mut st, now) = landed_at(t0());
        st.park_off();
        let r = TurnEndReading {
            finished_record: record,
            leftover: rows(&[TOKEN]),
            ..land(idle("Renamed the lexer's tokens.", None))
        };
        let (a, _) = until_acted(&mut st, &r, now + 10 * MIN);
        assert_eq!(st.landed(), stands, "{record:?}: {a:?}");
    }
}

/// CRUISE TELLS THE LEFT SUFFIX AND HOLDS NOTHING: its done check names the rows its claim left in
/// its own-work words, and types on; with none left, no suffix. NEGATIVE CONTROL: the suffix
/// dropped from `done_check_at` names none.
#[test]
fn cruise_tells_the_left_suffix_and_holds_nothing() {
    let claim = |left: LeftRead| TurnEndReading {
        leftover: left,
        ..under(Autopilot::Cruise, idle(DEFENSE, Some(3 * MIN)))
    };
    let mut st = TurnEndState::default();
    let a = at(&mut st, &claim(rows(&[TOKEN])), t0());
    assert_eq!(a.rule_id(), Some(RULE_DONE_CHECK), "{a:?}");
    assert!(typed(&a).contains(&own_left_suffix(1)), "{a:?}");
    assert!(st.land_held().is_none() && !st.landed());
    let mut st = TurnEndState::default();
    let a = at(&mut st, &claim(rows(&[])), t0());
    assert_eq!(a.rule_id(), Some(RULE_DONE_CHECK), "{a:?}");
    assert!(!carries_left_suffix(typed(&a), 1), "{a:?}");
}

/// THE LEFTOVER FACT NEVER RIDES THE ACT CARRYING THE LEFT SUFFIX: the nudge that names the rows
/// leaves the fact that names them again untold, while a fact of another kind rides the same act.
/// NEGATIVE CONTROL: let the leftover fact ride, and both are in one act.
#[test]
fn the_leftover_fact_never_rides_the_act_carrying_the_left_suffix() {
    let fact = |kind: FactKind| Fact {
        key: EpisodeKey { kind, id: 7 },
        text: "1 process this session started still runs; `aterm ctl @self sys left` lists it"
            .to_string(),
        as_of_ms: 0,
    };
    for (kind, rides) in [(FactKind::Leftover, false), (FactKind::NoProgress, true)] {
        let (mut st, now) = instructed(t0());
        let r = TurnEndReading {
            fact: Some(fact(kind)),
            ..worked(DEFENSE, rows(&[TOKEN]))
        };
        let (a, _) = decided(&mut st, &r, now);
        assert!(carries_left_suffix(typed(&a), 1), "{kind:?}: {a:?}");
        assert_eq!(carries_fact(&a, &fact(kind)), rides, "{kind:?}: {a:?}");
    }
}

/// THE LEFT LEG HOLDS THE CHECK'S `LANDED` TOO: a row born after the defense that owed the check
/// holds its `LANDED`, which lands nothing and audits no defense, and the nudge after it names the
/// row; once the agent ends it, the same defense said again is owed the check again, and its
/// `LANDED` lands with `left=0`. NEGATIVE CONTROL: drop the leg from `clean`, and the first
/// `LANDED` lands over the row; let a held `LANDED` audit its defense, and the defense said again
/// gets a nudge, not the check.
#[test]
fn the_left_leg_holds_the_checks_landed_over_a_row_born_since_its_defense() {
    let (mut st, now) = instructed(t0());
    let r = worked(DEFENSE, rows(&[]));
    let (a, now) = decided(&mut st, &r, now);
    assert_eq!(a.rule_id(), Some(RULE_LAND_CHECK), "no row yet: {a:?}");
    st.acted(&a, &r, now);
    let answer = short(LANDED_WORD, rows(&[TOKEN]));
    let (b, now) = decided(&mut st, &answer, now + MIN);
    assert!(!st.landed(), "the row holds the LANDED: {st:?}");
    assert_eq!(b.rule_id(), Some(RULE_LAND_NUDGE), "{b:?}");
    assert!(carries_left_suffix(typed(&b), 1), "{b:?}");
    assert_eq!(legs(&st), vec![HoldLeg::Left]);
    st.acted(&b, &answer, now);
    let ended = tooled(DEFENSE, rows(&[]));
    let (c, now) = decided(&mut st, &ended, now + MIN);
    assert_eq!(
        c.rule_id(),
        Some(RULE_LAND_CHECK),
        "the held LANDED audited nothing: {c:?}"
    );
    st.acted(&c, &ended, now);
    let (d, _) = decided(&mut st, &short(LANDED_WORD, rows(&[])), now + MIN);
    assert_eq!(d, TurnEndAction::Nothing, "{st:?}");
    assert!(st.landed());
    assert_eq!(st.landed_left().map(LeftWord::word).as_deref(), Some("0"));
}

/// `n` tokens of distinct processes, as `sys left` prints them.
fn tokens(n: u32) -> Vec<String> {
    (0..n)
        .map(|k| format!("{}@{}:1", 2700 + k, 1_792_098_840_100_000 + u64::from(k)))
        .collect()
}

/// `DEFENSE` with a keep line for each of `kept`.
fn keeping(kept: &[String]) -> String {
    let lines: Vec<String> = kept.iter().map(|t| format!("- keep `{t}`")).collect();
    format!("{DEFENSE}\n{}", lines.join("\n"))
}

/// A DEFENSE KEEPS EVERY ROW ITS KEEP LINES NAME, AS MANY AS ITS EPISODE'S BOOKS HOLD: nine kept
/// rows owe the check and its `LANDED` lands with `left=kept:9`. Past [`KEPT_TOKENS`] the episode
/// cannot say which rows were kept: the leg holds nothing and the landing says `left=unchecked`.
/// A kept token whose process is gone is let go, so the books carry only rows that still run.
/// NEGATIVE CONTROL: books of eight tokens that drop the oldest hold the ninth kept row's
/// `LANDED`; an overflow that leaves `kept_over` unset holds the seventeenth's; a gone row's token
/// kept stays in the books.
#[test]
fn a_defense_keeps_every_row_its_keep_lines_name() {
    for (n, word) in [
        (9, "kept:9".to_string()),
        (
            u32::try_from(KEPT_TOKENS).expect("small") + 1,
            "unchecked".to_string(),
        ),
    ] {
        let kept = tokens(n);
        let all: Vec<&str> = kept.iter().map(String::as_str).collect();
        let (mut st, now) = instructed(t0());
        let r = worked(&keeping(&kept), rows(&all));
        let (a, now) = decided(&mut st, &r, now);
        assert_eq!(a.rule_id(), Some(RULE_LAND_CHECK), "{n}: {a:?}");
        st.acted(&a, &r, now);
        let (b, _) = decided(&mut st, &short(LANDED_WORD, rows(&all)), now + MIN);
        assert_eq!(b, TurnEndAction::Nothing, "{n}: {st:?}");
        assert!(
            st.landed(),
            "{n}: every kept row runs, and LANDED lands: {st:?}"
        );
        assert_eq!(
            st.landed_left().map(LeftWord::word).as_deref(),
            Some(word.as_str()),
            "{n}"
        );
    }
    let kept = tokens(2);
    let (mut st, now) = instructed(t0());
    let first = worked(&keeping(&kept[..1]), rows(&[kept[0].as_str(), TOKEN]));
    let (a, now) = decided(&mut st, &first, now);
    assert_eq!(
        a.rule_id(),
        Some(RULE_LAND_NUDGE),
        "the unkept row holds: {a:?}"
    );
    st.acted(&a, &first, now);
    let next = tooled(&keeping(&kept[1..]), rows(&[kept[1].as_str(), TOKEN]));
    let _ = decided(&mut st, &next, now + MIN);
    assert_eq!(
        st.land_books()
            .kept
            .iter()
            .flatten()
            .copied()
            .collect::<Vec<_>>(),
        vec![token_of(&kept[1]).expect("a token")],
        "the gone row's token is let go"
    );
}

/// The state a loop that follows starts with where the one before it judged a yield and stopped
/// before its act: Land's episode, its last act `land_act`, the books the yield's row carried, and
/// the yield `ago` before `now` to read again ([`TurnEndState::seed_land_point`]).
fn restarted(
    land_act: &'static str,
    nudges: u32,
    books: LandBooks,
    ago: Duration,
    now: Instant,
) -> TurnEndState {
    let mut st = TurnEndState::default();
    st.seed_land(nudges, false, false, false);
    st.seed_land_books(books);
    st.seed_land_owed(land_act);
    st.seed_short(1);
    st.seed_point(ago, now);
    st.seed_land_point();
    st
}

/// A RESTART AT A HELD YIELD COUNTS ITS HOLD ONCE: a loop that stops after judging a yield the
/// left leg held, before its nudge, hands on books that leave that unsettled hold out, and the
/// loop that reads the yield again counts it, once its act settles it. So with a restart at each
/// of three holds, the third still holds and the fourth defense owes the check. NEGATIVE CONTROL:
/// hand on the unsettled hold too, and the books the point hands on count it before its act,
/// which the loop that follows then counts again.
#[test]
fn a_restart_at_a_held_yield_counts_its_hold_once() {
    let rows_of = tokens(4);
    let (mut st, mut now) = instructed(t0());
    let mut land_act = RULE_LAND;
    for (k, token) in rows_of.iter().take(3).enumerate() {
        let r = if k == 0 {
            worked(DEFENSE, rows(&[token.as_str()]))
        } else {
            tooled(DEFENSE, rows(&[token.as_str()]))
        };
        let _ = at(&mut st, &r, now);
        assert_eq!(
            st.land_books().left_holds,
            u8::try_from(k).expect("small"),
            "hold {} not handed on before its act",
            k + 1
        );
        let nudges = u32::try_from(k).expect("small");
        st = restarted(land_act, nudges, st.land_books(), MIN, now + MIN);
        let again = TurnEndReading {
            worked: None,
            ..r.clone()
        };
        let (a, then) = decided(&mut st, &again, now + MIN);
        assert_eq!(
            a.rule_id(),
            Some(RULE_LAND_NUDGE),
            "hold {} still holds after the restart: {a:?}",
            k + 1
        );
        st.acted(&a, &again, then);
        assert_eq!(
            st.land_books().left_holds,
            u8::try_from(k + 1).expect("small"),
            "hold {} counted once",
            k + 1
        );
        land_act = RULE_LAND_NUDGE;
        now = then + MIN;
    }
    let r = tooled(DEFENSE, rows(&[rows_of[3].as_str()]));
    let (a, _) = decided(&mut st, &r, now);
    assert_eq!(
        a.rule_id(),
        Some(RULE_LAND_CHECK),
        "the leg holds no more: {a:?}"
    );
}

/// A LEFT ROW THAT ENDS IN THE BACK-OFF IS JUDGED AGAIN BEFORE THE ACT: a defense the left leg held
/// over a short turn waits its back-off; read again with the row gone, or over a view that cannot
/// say, the defense owes the check, and read again with one of two rows gone, the nudge names the
/// one that still runs. The books the point journals leave out the hold the back-off may take
/// back, so a loop that follows from them counts none the yield no longer holds; its act settles
/// one hold where it still holds, and none where it no longer does. NEGATIVE CONTROL: judge
/// nothing again at the same point, and the act is a nudge naming the rows of the first reading;
/// judge it again only over rows read, and the view that cannot say still holds; journal the hold
/// before anything settles it, and the point's books count it.
#[test]
fn a_left_row_that_ends_in_the_back_off_is_judged_again_before_the_act() {
    let two = tokens(2);
    for (before, after, check) in [
        (rows(&[TOKEN]), rows(&[]), true),
        (rows(&[TOKEN]), LeftRead::Unchecked, true),
        (
            rows(&[two[0].as_str(), two[1].as_str()]),
            rows(&[two[0].as_str()]),
            false,
        ),
    ] {
        let (mut st, now) = instructed(t0());
        let r = tooled(DEFENSE, before);
        let a = at(&mut st, &r, now);
        let TurnEndAction::WaitUntil { until, .. } = a else {
            panic!("PRECONDITION: a short turn waits its back-off: {a:?}");
        };
        assert_eq!(legs(&st), vec![HoldLeg::Left]);
        assert_eq!(
            st.land_books().left_holds,
            0,
            "{after:?}: the point's books leave its unsettled hold out"
        );
        let later = TurnEndReading {
            worked: None,
            ..tooled(DEFENSE, after.clone())
        };
        let (b, then) = decided(&mut st, &later, until);
        if check {
            assert_eq!(
                b.rule_id(),
                Some(RULE_LAND_CHECK),
                "{after:?}: no row proved: {b:?}"
            );
        } else {
            assert_eq!(b.rule_id(), Some(RULE_LAND_NUDGE), "{b:?}");
            assert!(
                carries_left_suffix(typed(&b), 1),
                "the one that runs: {b:?}"
            );
        }
        st.acted(&b, &later, then);
        assert_eq!(
            st.land_books().left_holds,
            u8::from(!check),
            "{after:?}: the yield's one hold, kept only where it still holds"
        );
    }
}

/// A KEEP LINE READS AS `sys left` PRINTS ITS ROW, AND NEVER INSIDE A NEED: `keep:` and a token
/// copied with its `end=` keep it; a keep line in a NEEDS YOU block under the defense keeps
/// nothing, as the body Land reads leaves the block out. NEGATIVE CONTROL: read the keep lines
/// in the reply with its needs, and the line in the need keeps its token; read `keep` alone, or
/// the token with its `end=`, and those lines keep nothing.
#[test]
fn a_keep_line_reads_as_sys_left_prints_it_and_never_inside_a_need() {
    let kept = tokens(3);
    let copied = format!(
        "{DEFENSE}\nkeep: {}\n- keep end={} (the preview server the person asked for)\n- keep \
         `end={}`",
        kept[0], kept[1], kept[2]
    );
    assert_eq!(
        needs::kept_tokens(&copied),
        kept.iter()
            .map(|t| token_of(t).expect("a token"))
            .collect::<Vec<_>>()
    );
    let asked = format!(
        "{DEFENSE}\n\nNEEDS YOU:\n- keep {} if you still want the preview",
        kept[0]
    );
    assert!(
        needs::kept_tokens(&asked).is_empty(),
        "{:?}",
        needs::kept_tokens(&asked)
    );
}

/// KEEP LINES OF GONE PROCESSES OVERFLOW NOTHING: a defense whose keep lines name more tokens than
/// the books hold, all but one of them processes no longer running, keeps the one that runs, and
/// the left leg still holds the unkept row beside it. NEGATIVE CONTROL: count every token toward
/// the books' room, and the episode loses what it kept and owes the check.
#[test]
fn keep_lines_of_gone_processes_overflow_nothing() {
    let kept = tokens(u32::try_from(KEPT_TOKENS).expect("small") + 1);
    let (mut st, now) = instructed(t0());
    let r = worked(&keeping(&kept), rows(&[kept[0].as_str(), TOKEN]));
    let (a, _) = decided(&mut st, &r, now);
    assert_eq!(
        a.rule_id(),
        Some(RULE_LAND_NUDGE),
        "the unkept row holds: {a:?}"
    );
    assert!(carries_left_suffix(typed(&a), 1), "{a:?}");
    let books = st.land_books();
    assert!(!books.kept_over, "{books:?}");
    assert!(
        books.kept.contains(&token_of(&kept[0])),
        "the kept row that runs stays kept: {books:?}"
    );
}

/// A LOOP THAT FOLLOWS LANDS A `LANDED` THE LEFT LEG HELD ONCE ITS ROWS ARE GONE: the loop before
/// it judged the check's `LANDED` held by a row born since the defense and stopped before its
/// nudge; read again with the row ended, the answer lands with `left=0`, and read again with the
/// row still running, it holds, naming the row, its hold counted once. NEGATIVE CONTROL: judge the
/// seeded `LANDED` again without landing it, and the loop types a nudge that names nothing.
#[test]
fn a_loop_that_follows_lands_a_landed_the_left_leg_held_once_its_rows_are_gone() {
    for (after, lands) in [(rows(&[]), true), (rows(&[TOKEN]), false)] {
        let (mut st, now) = instructed(t0());
        let r = worked(DEFENSE, rows(&[]));
        let (a, now) = decided(&mut st, &r, now);
        assert_eq!(a.rule_id(), Some(RULE_LAND_CHECK), "no row yet: {a:?}");
        st.acted(&a, &r, now);
        let answer = short(LANDED_WORD, rows(&[TOKEN]));
        let _ = at(&mut st, &answer, now + MIN);
        assert!(!st.landed(), "PRECONDITION: the row holds the LANDED");
        assert_eq!(legs(&st), vec![HoldLeg::Left], "PRECONDITION");
        let mut st = restarted(RULE_LAND_CHECK, 0, st.land_books(), MIN, now + 2 * MIN);
        st.seed_land_checked();
        let again = TurnEndReading {
            worked: None,
            leftover: after.clone(),
            ..answer.clone()
        };
        let (b, then) = decided(&mut st, &again, now + 2 * MIN);
        assert_eq!(st.landed(), lands, "{after:?}: {b:?} {st:?}");
        if lands {
            assert_eq!(b, TurnEndAction::Nothing, "{st:?}");
            assert_eq!(st.landed_left().map(LeftWord::word).as_deref(), Some("0"));
        } else {
            assert_eq!(b.rule_id(), Some(RULE_LAND_NUDGE), "{b:?}");
            assert!(carries_left_suffix(typed(&b), 1), "{b:?}");
            st.acted(&b, &again, then);
            assert_eq!(st.land_books().left_holds, 1, "counted once");
        }
    }
}
