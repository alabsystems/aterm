// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the system-message lab's expired login (v6 §14.17.5.6): its stand-in, the person's
//! play's watch, and its two judges, each want with the run that breaks it.

use aterm_agent::supervise::policy::login::{
    RULE_LOGIN_BACK_OUT, RULE_LOGIN_DONE, RULE_LOGIN_METHOD,
};
use aterm_agent::supervise::policy::notice::{LOGIN_CANCELLED, LOGIN_CHOOSE};
use aterm_agent::supervise::policy::turn_end::{self, RULE_LOGIN};

use super::*;
use crate::forever::sysmsg::login::{
    self, BandRow, GO_ON_MS, Played, QUIET_MS, SIGNED_PROMPT, band_rows, rows,
};
use crate::forever::sysmsg::{Form, LAND_ON_MS};

/// The point Cruise's back-off acted from.
const MET: i64 = 125_000;
/// The login wall's first reading.
const WALL: i64 = MET + 2_000;
/// aterm's `/login`.
const LOGIN_AT: i64 = WALL + 500;
/// The login's notice.
const TOLD: i64 = WALL + 1_300;
/// The person's pick: [`QUIET_MS`] past the wall's reading.
const PICK: i64 = WALL + QUIET_MS;
/// The stand-in's sign-in, the person's Esc and their prompt.
const LIFT: i64 = PICK + 17_000;
const LEFT: i64 = LIFT + 2_000;
const PROMPT: i64 = LEFT + 14_000;
/// The prompt's answer, and Cruise's own act on its back-off.
const ANSWERED: i64 = PROMPT + 3_000;
const WENT_ON: i64 = ANSWERED + 120_000;

/// The login's band row read at `t` in `state`: a need now while it is live, none once it left
/// (`need=-`, as the band reads a superseded or withdrawn row).
fn band(t: i64, state: &str) -> (i64, String) {
    j(
        t,
        &format!(
            "12 at={} state={state} need={} title={} in tab 1",
            T0 + TOLD,
            if state == "live" { "now" } else { "-" },
            LOGIN_CHOOSE.title
        ),
    )
}

fn row(t: i64, rule: &str, decision: &str, command: &str, reason: &str) -> Row {
    Row {
        ts: T0 + t,
        box_seq: None,
        rule: rule.into(),
        decision: decision.into(),
        command: command.into(),
        reason: reason.into(),
    }
}

/// The run on Claude Code under Cruise: Cruise's done check meets the expired login; aterm types
/// `/login` once and tells the person once; the person picks a method (the inert browser logs
/// its hand-off), the stand-in signs in, the person leaves the dialog and prompts; Cruise goes on.
fn expired() -> Seen {
    let wall = format!("Please run /login · API Error: 401 {}", login::EXPIRED);
    Seen {
        journal: vec![
            j(5_000, &format!("EVENT idle seq=1 {DONE_CLAIM}")),
            j(
                5_100,
                "WAITING seq=1 until=x 1 short turn(s) in a row: 2 min back-off",
            ),
            j(
                MET,
                "CONTINUED seq=1 rule=continue-done-check@v1 [aterm harness] done is never the end",
            ),
            j(WALL, &format!("EVENT idle seq=2 ⏺ {wall}")),
            j(LOGIN_AT, &format!("TYPED seq=2 rule={RULE_LOGIN} /login")),
            j(
                TOLD,
                "EXCEPTION seq=3 kind=login when=now notice=login-choose Claude Code's login \
                 expired, so it cannot answer prompts. => Pick a sign-in method in the tab and \
                 finish signing in.",
            ),
            j(TOLD, "HOLD seq=3 point nothing why=policy"),
            j(LEFT + 600, "EVENT idle seq=4 ⎿  Login interrupted"),
            j(ANSWERED, &format!("EVENT idle seq=5 {DONE_CLAIM}")),
            j(
                ANSWERED + 100,
                "WAITING seq=5 until=x 1 short turn(s) in a row: 2 min back-off",
            ),
            j(
                WENT_ON,
                "CONTINUED seq=5 rule=continue-done-check@v1 [aterm harness] done is never the end",
            ),
        ],
        ledger: vec![
            point(5_000, 1),
            typed(MET, "continue-done-check@v1", DONE_CHECK),
            typed(LOGIN_AT, RULE_LOGIN, "/login"),
            row(TOLD, "-", "escalated", "", ""),
            point(ANSWERED, 1),
            typed(WENT_ON, "continue-done-check@v1", DONE_CHECK),
        ],
        status: vec![j(1_000, "task=open needs=- autopilot=cruise agent=-")],
        meta: vec![no_meta(1_000)],
        band: vec![
            band(TOLD + 2_000, "live"),
            band(ANSWERED + 2_000, "withdrawn"),
        ],
        band_reads: vec![T0 + TOLD + 2_000, T0 + ANSWERED + 2_000],
        end_ms: T0 + WENT_ON + 25_000,
        cover: Cover {
            outage_ms: Some(T0 + 5_500),
            outage_lifted_ms: Some(T0 + LIFT),
            login: Some(Played {
                control: Some(true),
                stored: false,
                opened_before: Some(String::new()),
                picked_ms: Some(T0 + PICK),
                no_pick: None,
                opened_after: Some("11:52:00 open https://claude.ai\n".into()),
                left_ms: Some(T0 + LEFT),
                prompt_ms: Some(T0 + PROMPT),
                prompt_refused: false,
            }),
            ..Cover::default()
        },
        ..Seen::default()
    }
}

/// agy's wall: its 401, as its reader names it.
const AGY_WALL: &str = "agent executor error: generating and executing: Error 401, Message: Request had \
                        invalid authentication credentials.";

/// The tab's attention as `meta` reads it at `t`: `text` raised, or none.
fn attention(t: i64, text: Option<&str>) -> (i64, String) {
    match text {
        Some(text) => j(
            t,
            &format!("attention_owners=1 attention_kinds=-:supervisor attention={text}"),
        ),
        None => no_meta(t),
    }
}

/// The escalation agy's lost login raises (a 12d build's: how agy signs in leads the wall's words
/// its badge would cut it off behind).
fn agy_told() -> String {
    let how = turn_end::sign_in_how(aterm_phase::Program::Agy, AGY_WALL);
    format!(
        "agy wall: agent executor error: generating and executing: Error 401, Mess… (the login is \
         gone, {how}: agent executor error: generating and exec…)"
    )
}

/// [`expired`] on agy: the wall its reader names, one escalation (journaled, its words the tab's
/// attention, no ledger row) naming how agy signs in, no pick and no Esc.
fn expired_agy() -> Seen {
    let mut s = expired();
    s.journal = vec![
        s.journal[0].clone(),
        s.journal[1].clone(),
        s.journal[2].clone(),
        j(WALL, &format!("EVENT idle seq=2 ⚠ {AGY_WALL}")),
        j(TOLD, "ESCALATED seq=2 attention=OK"),
        j(ANSWERED, &format!("EVENT idle seq=5 {DONE_CLAIM}")),
        s.journal[9].clone(),
        s.journal[10].clone(),
    ];
    s.ledger = vec![
        point(5_000, 1),
        typed(MET, "continue-done-check@v1", DONE_CHECK),
        point(TOLD, 1),
        point(ANSWERED, 1),
        typed(WENT_ON, "continue-done-check@v1", DONE_CHECK),
    ];
    s.meta = vec![
        no_meta(1_000),
        attention(TOLD + 1_900, Some(&agy_told())),
        no_meta(PICK + 3_000),
        no_meta(ANSWERED + 1_000),
    ];
    s.band = vec![j(TOLD + 2_000, "-"), j(ANSWERED + 2_000, "-")];
    s.band_reads = vec![T0 + TOLD + 2_000, T0 + ANSWERED + 2_000];
    s.cover.outage_lifted_ms = Some(T0 + PICK);
    s.cover.login = Some(Played {
        control: Some(true),
        opened_before: Some(String::new()),
        prompt_ms: Some(T0 + PROMPT),
        ..Played::default()
    });
    s
}

fn verdict(chain: Chain, seen: &Seen, program: &str, s: Scenario) -> (&'static str, String) {
    one(chain, seen, program, s)
}

fn gone(seen: &Seen, program: &str) -> (&'static str, String) {
    verdict(Chain::SysLogin, seen, program, Scenario::Sys(Form::Login))
}

fn signed(seen: &Seen, program: &str) -> (&'static str, String) {
    verdict(Chain::SysLogin, seen, program, Scenario::SignedIn(false))
}

fn push(s: &mut Seen, t: i64, line: &str, row: Option<Row>) {
    s.journal.push(j(t, line));
    s.journal.sort_by_key(|(t, _)| *t);
    if let Some(r) = row {
        s.ledger.push(r);
        s.ledger.sort_by_key(|r| r.ts);
    }
}

#[test]
fn claude_codes_expired_login_left_to_the_person_and_told_once_passes() {
    let (w, why) = gone(&expired(), "claude");
    assert_eq!(w, "PASS", "{why}");
    assert!(
        why.contains("Cruise's continue-done-check@v1 typed at +125.0 s met it, read at +127.0 s")
            && why.contains("/login once at +127.5 s")
            && why.contains("the inert browser logged nothing (its control logged)")
            && why.contains(&format!("told once at +128.3 s ({}", LOGIN_CHOOSE.id))
            && why.contains(&format!("one band row \"{} in tab 1\"", LOGIN_CHOOSE.title)),
        "{why}"
    );
}

/// THE MAIN ARM (bin-w4, the rule before 2026-11-01): aterm presses the subscription row of its
/// own `/login`'s picker, which hands the sign-in to the browser before any key of the person's.
#[test]
fn aterm_answering_its_picker_and_running_the_browser_fails() {
    let mut s = expired();
    s.ledger.push(row(
        LOGIN_AT + 2_000,
        RULE_LOGIN_METHOD,
        "approved",
        "the subscription's sign-in",
        "",
    ));
    s.ledger.sort_by_key(|r| r.ts);
    if let Some(p) = s.cover.login.as_mut() {
        p.opened_before = Some("11:47:03 open https://claude.ai\n".into());
    }
    let (w, why) = gone(&s, "claude");
    assert_eq!(w, "FAIL", "{why}");
    assert!(
        why.contains(&format!(
            "ANSWERED THE PICKER: approved {RULE_LOGIN_METHOD} at +129.5 s"
        )) && why.contains("THE BROWSER RAN ON ATERM'S KEY")
            && why.contains("open https://claude.ai"),
        "{why}"
    );
    // Any other key of aterm's in the dialog is one too: its Esc over no roster row.
    let mut s = expired();
    s.ledger.push(row(
        LOGIN_AT + 9_000,
        RULE_LOGIN_BACK_OUT,
        "approved",
        "the /login aterm typed: Esc, no method picked",
        "",
    ));
    let (w, why) = gone(&s, "claude");
    assert_eq!(w, "FAIL", "{why}");
    assert!(
        why.contains(&format!(
            "A KEY OF ATERM'S WHILE THE LOGIN WAS GONE: approved {RULE_LOGIN_BACK_OUT} at +136.5 s"
        )),
        "{why}"
    );
}

#[test]
fn typing_into_the_lost_login_fails() {
    // /login twice.
    let mut s = expired();
    push(
        &mut s,
        LOGIN_AT + 60_000,
        &format!("TYPED seq=3 rule={RULE_LOGIN} /login"),
        Some(typed(LOGIN_AT + 60_000, RULE_LOGIN, "/login")),
    );
    let (w, why) = gone(&s, "claude");
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("/login TYPED 2 TIMES"), "{why}");
    // Cruise's own act typed into the login's wall on its back-off.
    let mut s = expired();
    push(
        &mut s,
        WALL + 241_000,
        "CONTINUED seq=3 rule=continue@v1 [aterm harness] keep going",
        None,
    );
    let (w, why) = gone(&s, "claude");
    assert_eq!(w, "FAIL", "{why}");
    assert!(
        why.contains("TYPED WHILE THE LOGIN WAS GONE: continue@v1 at +368.0 s"),
        "{why}"
    );
    // On agy no act at all, `/login` included, which agy has none of.
    let mut s = expired_agy();
    push(
        &mut s,
        WALL + 500,
        &format!("TYPED seq=2 rule={RULE_LOGIN} /login"),
        None,
    );
    let (w, why) = gone(&s, "agy");
    assert_eq!(w, "FAIL", "{why}");
    assert!(
        why.contains(&format!(
            "TYPED WHILE THE LOGIN WAS GONE: {RULE_LOGIN} at +127.5 s"
        )),
        "{why}"
    );
}

#[test]
fn the_login_told_twice_otherwise_or_never_fails() {
    let mut s = expired();
    push(
        &mut s,
        TOLD + 60_000,
        "EXCEPTION seq=4 kind=login when=now notice=login-choose again",
        None,
    );
    let (w, why) = gone(&s, "claude");
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("TOLD 2 TIMES"), "{why}");
    let mut s = expired();
    for (_, l) in &mut s.journal {
        *l = l.replace("notice=login-choose", "notice=login-sign-in");
    }
    let (w, why) = gone(&s, "claude");
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("NOT THE PERSON'S PICK"), "{why}");
    let mut s = expired();
    s.journal.retain(|(_, l)| !l.starts_with("EXCEPTION "));
    let (w, why) = gone(&s, "claude");
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("NOBODY TOLD"), "{why}");
    // Two band rows for one login.
    let mut s = expired();
    s.band.push(j(
        TOLD + 70_000,
        &format!(
            "12 at={} state=live need=now title={} in tab 1 | 13 at={} state=live need=now title={} in tab 1",
            T0 + TOLD,
            LOGIN_CHOOSE.title,
            T0 + TOLD + 65_000,
            LOGIN_CHOOSE.title
        ),
    ));
    s.band.sort_by_key(|(t, _)| *t);
    let (w, why) = gone(&s, "claude");
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("2 BAND ROWS"), "{why}");
}

#[test]
fn agys_lost_login_escalated_once_naming_its_sign_in_passes_and_otherwise_fails() {
    let (w, why) = gone(&expired_agy(), "agy");
    assert_eq!(w, "PASS", "{why}");
    assert!(
        why.contains("one escalation at +128.3 s, naming how it signs in"),
        "{why}"
    );
    let mut s = expired_agy();
    s.journal.retain(|(_, l)| !l.starts_with("ESCALATED "));
    let (w, why) = gone(&s, "agy");
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("NOBODY TOLD"), "{why}");
    // The record of a build whose badge cut the sign-in off behind agy's 401 (bin-w4, 02fe265c25).
    let mut s = expired_agy();
    s.meta[1] = attention(
        TOLD + 1_900,
        Some(
            "agy wall: agent executor error: generating and executing: Error 401, Mess… (the \
             login is gone: agent executor error: generating and executing: Error 401, Message: \
             Request had invalid authenticat…)",
        ),
    );
    let (w, why) = gone(&s, "agy");
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("THE ESCALATION NAMES NO SIGN-IN"), "{why}");
    let mut s = expired_agy();
    push(&mut s, TOLD + 60_000, "ESCALATED seq=3 attention=OK", None);
    let (w, why) = gone(&s, "agy");
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("ESCALATED 2 TIMES"), "{why}");
}

/// An escalation's words are its ledger row's reason where it writes one, else the tab's attention.
#[test]
fn an_escalations_words_are_its_rows_reason_else_the_tabs_attention() {
    let s = expired_agy();
    let told = login::escalations(&s, T0 + WALL, T0 + PICK);
    assert_eq!(told, vec![(T0 + TOLD, agy_told())]);
    let mut s = expired_agy();
    s.ledger.push(row(
        TOLD + 10,
        "-",
        "escalated",
        "",
        "the login is gone: a row's own words",
    ));
    s.ledger.sort_by_key(|r| r.ts);
    let told = login::escalations(&s, T0 + WALL, T0 + PICK);
    assert_eq!(
        told,
        vec![(T0 + TOLD, "the login is gone: a row's own words".into())]
    );
    // The control: an attention raised long before the escalation is not its words.
    let mut s = expired_agy();
    s.meta[1].0 -= 60_000;
    s.meta.insert(2, no_meta(TOLD - 30_000));
    let told = login::escalations(&s, T0 + WALL, T0 + PICK);
    assert_eq!(told, vec![(T0 + TOLD, String::new())]);
}

/// Muse Code's escalation names its own remedy, read off the wall it quotes.
#[test]
fn muses_sign_in_is_read_off_the_wall_it_quotes() {
    let words = "your API key from META_API_KEY was rejected — update or unset it";
    let how = turn_end::sign_in_how(aterm_phase::Program::Muse, words);
    assert_ne!(
        how,
        turn_end::sign_in_how(aterm_phase::Program::Muse, ""),
        "the control: the wall's own remedy"
    );
    let mut s = expired_agy();
    s.journal[3] = j(WALL, "EVENT idle seq=2 -");
    s.meta[1] = attention(
        TOLD + 1_900,
        Some(&format!(
            "muse wall: {words} (the login is gone, {how}: your API key from META_API_KEY was \
             rejected — update or u…)"
        )),
    );
    let (w, why) = gone(&s, "muse");
    assert_eq!(w, "PASS", "{why}");
    // The record of a build whose badge cut Muse Code's remedy (bin-w4, 02fe265c25).
    s.meta[1] = attention(
        TOLD + 1_900,
        Some(&format!(
            "muse wall: {words} (the login is gone: {words} (as Muse Code says, update or u…)"
        )),
    );
    let (w, why) = gone(&s, "muse");
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("THE ESCALATION NAMES NO SIGN-IN"), "{why}");
}

#[test]
fn the_persons_sign_in_and_cruise_going_on_once_passes() {
    let (w, why) = signed(&expired(), "claude");
    assert_eq!(w, "PASS", "{why}");
    assert!(
        why.contains("the person picked at +427.0 s")
            && why.contains(
                "the inert browser logged their hand-off: 11:52:00 open https://claude.ai"
            )
            && why.contains("the person prompted at +460.0 s")
            && why.contains("Cruise went on once: continue-done-check@v1 at +583.0 s"),
        "{why}"
    );
    let (w, why) = signed(&expired_agy(), "agy");
    assert_eq!(w, "PASS", "{why}");
}

#[test]
fn past_the_persons_sign_in_the_logins_acts_again_or_none_fail() {
    // aterm's key past the person's pick.
    let mut s = expired();
    s.ledger.push(row(
        PICK + 5_000,
        RULE_LOGIN_BACK_OUT,
        "approved",
        "Esc",
        "",
    ));
    let (w, why) = signed(&s, "claude");
    assert_eq!(w, "FAIL", "{why}");
    assert!(
        why.contains("A KEY OF ATERM'S PAST THE PERSON'S PICK"),
        "{why}"
    );
    // /login again past the prompt.
    let mut s = expired();
    push(
        &mut s,
        ANSWERED + 1_000,
        &format!("TYPED seq=5 rule={RULE_LOGIN} /login"),
        None,
    );
    let (w, why) = signed(&s, "claude");
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("THE LOGIN'S ACTS AGAIN"), "{why}");
    // An act over the person's turn, before its answer.
    let mut s = expired();
    push(
        &mut s,
        PROMPT + 1_000,
        "CONTINUED seq=4 rule=continue@v1 [aterm harness] keep going",
        None,
    );
    let (w, why) = signed(&s, "claude");
    assert_eq!(w, "FAIL", "{why}");
    assert!(
        why.contains("TYPED OVER THE PERSON'S TURN: continue@v1 at +461.0 s"),
        "{why}"
    );
    // Cruise never going on.
    let mut s = expired();
    s.journal.retain(|(t, _)| *t < T0 + WENT_ON);
    s.end_ms = T0 + PROMPT + GO_ON_MS + 1_000;
    let (w, why) = signed(&s, "claude");
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("CRUISE DID NOT GO ON"), "{why}");
    s.end_ms = T0 + PROMPT + 60_000;
    assert_eq!(signed(&s, "claude").0, "UNOBSERVED");
    // The notice left standing past the sign-in.
    let mut s = expired();
    s.band.pop();
    let (w, why) = signed(&s, "claude");
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("THE LOGIN'S NOTICE STILL UP"), "{why}");
    // Told again past it.
    let mut s = expired_agy();
    push(&mut s, ANSWERED + 500, "ESCALATED seq=6 attention=OK", None);
    s.meta.push(attention(ANSWERED + 900, Some(&agy_told())));
    let (w, why) = signed(&s, "agy");
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("TOLD AGAIN"), "{why}");
}

#[test]
fn a_person_who_never_came_or_whose_prompt_was_refused_is_unobserved() {
    let mut s = expired();
    s.cover.login = Some(Played::default());
    assert_eq!(signed(&s, "claude").0, "UNOBSERVED");
    let mut s = expired();
    if let Some(p) = s.cover.login.as_mut() {
        p.prompt_ms = None;
        p.prompt_refused = true;
    }
    let (w, why) = signed(&s, "claude");
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(why.contains("the composer did not take it"), "{why}");
    let mut s = expired();
    s.cover.outage_ms = None;
    assert_eq!(gone(&s, "claude").0, "UNOBSERVED");
}

#[test]
fn under_land_no_act_of_lands_into_the_lost_login() {
    let mut s = expired();
    for (_, l) in &mut s.journal {
        *l = l.replace(
            "CONTINUED seq=1 rule=continue-done-check@v1 [aterm harness] done is never the end",
            "TYPED seq=1 rule=land@v1 [aterm harness] Autopilot is Land: land",
        );
    }
    let (w, why) = verdict(
        Chain::SysLandLogin,
        &s,
        "claude",
        Scenario::SysLand(Form::Login),
    );
    assert_eq!(w, "PASS", "{why}");
    assert!(
        why.contains("Land's land@v1 typed at +125.0 s met it"),
        "{why}"
    );
    push(
        &mut s,
        WALL + 30_000,
        "CONTINUED seq=3 rule=land-nudge@v1 [aterm harness] Still landing: x",
        None,
    );
    let (w, why) = verdict(
        Chain::SysLandLogin,
        &s,
        "claude",
        Scenario::SysLand(Form::Login),
    );
    assert_eq!(w, "FAIL", "{why}");
    assert!(
        why.contains("TYPED WHILE THE LOGIN WAS GONE: land-nudge@v1 at +157.0 s"),
        "{why}"
    );
}

/// [`expired`] whose agent's own short answers had run to three by the point the act the wall
/// cut was typed on, a streak that stands past the person's turn (v6 §14.17.5.7), so it draws
/// Cruise's third short turn's back-off (8 min), and Cruise's act `late` past its end.
fn backed_off(late: i64) -> Seen {
    let mut s = streak_past_prompt(3, late);
    s.ledger.push(point(MET - 1_000, 3));
    s.ledger.sort_by_key(|r| r.ts);
    s
}

/// [`expired`] whose points past the person's prompt name `streak` short turns in a row, its
/// back-off the ladder's for it, and Cruise's act `late` past its end: 12d's run, where the act
/// the wall cut and the person's turn each counted one, is `streak_past_prompt(3, …)` over the
/// one short turn before the wall.
fn streak_past_prompt(streak: u32, late: i64) -> Seen {
    let step = i64::try_from(
        turn_end::TurnEndTiming::default()
            .short_wait_at(streak)
            .expect("a back-off")
            .as_millis(),
    )
    .expect("ms");
    let mut s = expired();
    let went = ANSWERED + 100 + step + late;
    for (t, l) in &mut s.journal {
        if *t == T0 + ANSWERED + 100 {
            (*t, *l) = backoff(ANSWERED + 100, 5, streak, step);
        } else if *t == T0 + WENT_ON {
            *t = T0 + went;
        }
    }
    for r in &mut s.ledger {
        if r.ts == T0 + WENT_ON {
            r.ts = T0 + went;
        } else if r.ts == T0 + ANSWERED {
            *r = point(ANSWERED, streak);
        }
    }
    s.end_ms = T0 + went + 25_000;
    s
}

#[test]
fn cruise_goes_on_by_the_back_off_the_persons_turn_drew() {
    let (w, why) = signed(&backed_off(2_000), "claude");
    assert_eq!(w, "PASS", "{why}");
    assert!(
        why.contains("Cruise went on once: continue-done-check@v1"),
        "{why}"
    );
    let (w, why) = signed(&backed_off(90_000), "claude");
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("CRUISE DID NOT GO ON"), "{why}");
    // The control: with no back-off named past the prompt, the same act is late.
    let mut s = backed_off(2_000);
    s.journal.retain(|(_, l)| !l.contains("back-off"));
    let (w, why) = signed(&s, "claude");
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("CRUISE DID NOT GO ON"), "{why}");
    // The watch waits the back-off out.
    let mut quiet = backed_off(2_000);
    quiet.journal.retain(|(_, l)| !l.starts_with("CONTINUED "));
    quiet
        .ledger
        .retain(|r| r.decision != "typed" || r.ts < T0 + ANSWERED);
    let due = login::go_on_by(&quiet, T0 + PROMPT, i64::MAX);
    // The back-off's line names its end to the second.
    assert!(
        (due - (T0 + ANSWERED + 100 + 9 * 60_000)).abs() < 1_000,
        "{due}"
    );
    assert!(!login::watched(&quiet, false, T0 + PROMPT + GO_ON_MS));
    assert!(login::watched(&quiet, false, due));
}

/// [`backed_off`] under Land: Land's instruction met the login, and past the back-off the person's
/// turn drew Land instructs again `late` past its end, the agent defends its landing and Land
/// lands 4 s later.
fn landed_after(late: i64) -> Seen {
    let mut s = backed_off(late);
    for (_, l) in &mut s.journal {
        if l.starts_with("CONTINUED seq=1 ") {
            *l = "TYPED seq=1 rule=land@v1 [aterm harness] Autopilot is Land: land".into();
        } else if l.starts_with("CONTINUED seq=5 ") {
            *l = "CONTINUED seq=5 rule=land@v1 [aterm harness] Autopilot is Land: land".into();
        }
    }
    let went = ANSWERED + 100 + 8 * 60_000 + late;
    push(
        &mut s,
        went + 2_000,
        &format!("EVENT idle seq=6 {DONE_CLAIM}"),
        None,
    );
    push(
        &mut s,
        went + 4_000,
        "DONE seq=6 rule=land@v1 the worker defended that everything is landed on the remote main \
         branch",
        None,
    );
    s.end_ms = T0 + went + 25_000;
    s
}

fn land_signed(seen: &Seen) -> (&'static str, String) {
    verdict(
        Chain::SysLandLogin,
        seen,
        "claude",
        Scenario::SignedIn(true),
    )
}

#[test]
fn land_carries_on_by_the_back_off_the_persons_turn_drew() {
    let (w, why) = land_signed(&landed_after(2_000));
    assert_eq!(w, "PASS", "{why}");
    assert!(
        why.contains("Land carried on (land@v1 at +") && why.contains(") and landed at +"),
        "{why}"
    );
    // The control: with no back-off named past the prompt, the same landing is late.
    let mut s = landed_after(2_000);
    s.journal.retain(|(_, l)| !l.contains("back-off"));
    let (w, why) = land_signed(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("LAND DID NOT CARRY ON"), "{why}");
    // Landing past when it was due fails too.
    let (w, why) = land_signed(&landed_after(60_000 + LAND_ON_MS));
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("past when it was due"), "{why}");
    // The watch waits the back-off out, and ends past the landing.
    let mut quiet = landed_after(2_000);
    quiet
        .journal
        .retain(|(_, l)| !l.starts_with("DONE ") && !l.starts_with("CONTINUED seq=5 "));
    let due = login::land_by(&quiet, T0 + PROMPT);
    assert!(
        (due - (T0 + ANSWERED + 100 + 8 * 60_000 + 60_000 + LAND_ON_MS)).abs() < 1_000,
        "{due}"
    );
    assert!(!login::watched(&quiet, true, due - 1_000));
    assert!(login::watched(&quiet, true, due));
    let s = landed_after(2_000);
    let landed = T0 + ANSWERED + 100 + 8 * 60_000 + 6_000;
    assert!(!login::watched(&s, true, landed));
    assert!(login::watched(&s, true, landed + 20_000));
}

#[test]
fn the_watch_ends_past_the_persons_prompt_and_cruises_act() {
    let s = expired();
    assert!(!login::watched(&s, false, T0 + WENT_ON + 1_000));
    assert!(login::watched(&s, false, T0 + WENT_ON + 20_000));
    let mut quiet = expired();
    quiet.journal.retain(|(t, _)| *t < T0 + WENT_ON);
    assert!(!login::watched(&quiet, false, T0 + PROMPT + 60_000));
    assert!(login::watched(&quiet, false, T0 + PROMPT + GO_ON_MS));
    let mut unplayed = expired();
    unplayed.cover.login = Some(Played::default());
    assert!(!login::watched(&unplayed, false, T0 + 3_600_000));
    if let Some(p) = unplayed.cover.login.as_mut() {
        p.prompt_refused = true;
    }
    assert!(login::watched(&unplayed, false, T0 + PROMPT));
    assert!(!SIGNED_PROMPT.contains(Chain::SysLogin.token().as_str()));
}

#[test]
fn the_band_rows_read_back_as_written() {
    let messages = "message 5 at=17 ago_ms=1 tag=harness sev=info origin=host state=recorded \
                    glass=- rep=1 key=- title=Autopilot%20set%20to%20cruise detail=- actions=-\n\
                    message 12 at=1792980409718 ago_ms=5 tag=harness sev=warn origin=host \
                    state=live glass=row rep=1 key=harness.ch.s-1 \
                    title=Claude%27s%20login%20expired%20in%20tab%202 detail=Pick actions=- \
                    need=now\n";
    let read = rows(messages);
    assert_eq!(
        read,
        "12 at=1792980409718 state=live need=now title=Claude's login expired in tab 2"
    );
    assert_eq!(
        band_rows(&read),
        [BandRow {
            id: "12".into(),
            at: 1_792_980_409_718,
            state: "live".into(),
            need: "now".into(),
            title: "Claude's login expired in tab 2".into(),
        }]
    );
    assert_eq!(rows(""), "-");
    assert!(band_rows("-").is_empty());
}

#[test]
fn claudes_stand_in_is_the_apis_answer_to_an_expired_oauth_token() {
    let spec = login::claude_spec();
    assert_eq!(spec.get("status").and_then(J::as_i64), Some(401));
    let err = spec.get("json").and_then(|j| j.get("error"));
    assert_eq!(
        err.and_then(|e| e.get("type")).and_then(J::as_str),
        Some("authentication_error")
    );
    assert_eq!(
        err.and_then(|e| e.get("message")).and_then(J::as_str),
        Some(login::EXPIRED)
    );
    assert!(crate::verbs::custom_mode_name(login::MODE).is_ok());
    assert_eq!(Form::Login.stub_mode(), "401");
    assert!(crate::stub::not_custom(Form::Login.stub_mode()).is_some());
}

/// A want with no evidence makes no pass: the recorder's control not logged at the raise, its log
/// never read, the band never read while the login was gone, or no picker drawn when the person
/// came (on Claude Code, whose picker and browser wants then judge nothing).
#[test]
fn a_login_whose_browser_band_or_picker_went_unseen_is_unobserved() {
    let control = |s: &mut Seen, logged: Option<bool>| {
        if let Some(p) = s.cover.login.as_mut() {
            p.control = logged;
        }
    };
    for (program, base) in [("claude", expired()), ("agy", expired_agy())] {
        let mut s = base.clone();
        control(&mut s, Some(false));
        let (w, why) = gone(&s, program);
        assert_eq!(w, "UNOBSERVED", "{program}: {why}");
        assert!(why.contains("control was not logged"), "{why}");
        let mut s = base.clone();
        control(&mut s, None);
        assert_eq!(gone(&s, program).0, "UNOBSERVED", "{program}");
        let mut s = base.clone();
        if let Some(p) = s.cover.login.as_mut() {
            p.opened_before = None;
        }
        let (w, why) = gone(&s, program);
        assert_eq!(w, "UNOBSERVED", "{program}: {why}");
        assert!(why.contains("log was never read"), "{why}");
        let mut s = base.clone();
        s.band_reads.clear();
        let (w, why) = gone(&s, program);
        assert_eq!(w, "UNOBSERVED", "{program}: {why}");
        assert!(why.contains("the band was never read"), "{why}");
        // A fail is a fail whatever went unseen beside it.
        let mut s = base;
        s.band_reads.clear();
        push(
            &mut s,
            WALL + 241_000,
            "CONTINUED seq=3 rule=continue@v1 [aterm harness] keep going",
            None,
        );
        assert_eq!(gone(&s, program).0, "FAIL", "{program}");
    }
    // No picker drawn: neither the picker's wants nor the person's hand-off were seen.
    let mut s = expired();
    if let Some(p) = s.cover.login.as_mut() {
        p.picked_ms = None;
        p.opened_after = None;
        p.no_pick = Some("no sign-in method picker on the screen (its last rows: ❯)".into());
    }
    let (w, why) = gone(&s, "claude");
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(why.contains("no method picker was drawn"), "{why}");
    let (w, why) = signed(&s, "claude");
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(why.contains("the person picked nothing"), "{why}");
}

/// The recorder's live control: the person's pick hands Claude Code's sign-in to the browser, and
/// the inert browser logs it. A pick it logged nothing of fails, as the same miss would have hidden
/// one of aterm's.
#[test]
fn a_persons_pick_the_inert_browser_logged_nothing_of_fails() {
    let mut s = expired();
    if let Some(p) = s.cover.login.as_mut() {
        p.opened_after = Some(String::new());
    }
    let (w, why) = signed(&s, "claude");
    assert_eq!(w, "FAIL", "{why}");
    assert!(
        why.contains("THE RECORDER MISSED THE PERSON'S HAND-OFF"),
        "{why}"
    );
    let mut s = expired();
    if let Some(p) = s.cover.login.as_mut() {
        p.opened_after = None;
    }
    assert_eq!(signed(&s, "claude").0, "UNOBSERVED");
}

/// One quiet notice on Claude Code: its band row a need now (the band, which plays no sound, opens
/// no dialog and brings no window forward), and no escalation beside it; on agy and Muse Code the
/// escalation alone, no band row.
#[test]
fn a_louder_or_second_signal_while_the_login_is_gone_fails() {
    let mut s = expired();
    push(&mut s, TOLD + 4_000, "ESCALATED seq=3 attention=OK", None);
    s.meta.push(attention(
        TOLD + 5_000,
        Some("claude wall: Please run /login (the login is gone)"),
    ));
    let (w, why) = gone(&s, "claude");
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("ESCALATED BESIDE THE NOTICE"), "{why}");
    let mut s = expired();
    for (_, r) in &mut s.band {
        *r = r.replace("need=now", "need=later");
    }
    let (w, why) = gone(&s, "claude");
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("THE LOGIN'S BAND ROW IS NO NEED NOW"), "{why}");
    let mut s = expired_agy();
    s.band[0] = j(
        TOLD + 2_000,
        &format!(
            "14 at={} state=live need=now title=agy's login expired in tab 1",
            T0 + TOLD
        ),
    );
    let (w, why) = gone(&s, "agy");
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("A BAND ROW BESIDE THE ESCALATION"), "{why}");
}

/// On the stored login aterm reads the method, a subscription's, so its picker's reason names the
/// subscription row: the rule slice 12a changed was reached. One that names no method did not
/// reach it, and judges nothing of that rule.
#[test]
fn on_the_stored_login_the_picker_rule_slice_12a_changed_is_reached_or_unobserved() {
    let subscription = aterm_phase::LoginMethod::Subscription.label();
    let with_reason = |why: &str| {
        let mut s = expired();
        if let Some(p) = s.cover.login.as_mut() {
            p.stored = true;
        }
        push(
            &mut s,
            TOLD,
            &format!(
                "NOT ESCALATED seq=3 the notice login-choose stands: pick the sign-in method in \
                 the tab, then finish the browser sign-in (Claude Code's login method picker: \
                 {why})"
            ),
            None,
        );
        s
    };
    let (w, why) = gone(
        &with_reason(&format!(
            "its `{subscription}` row opens the sign-in in your browser, which aterm never \
             brings forward by itself"
        )),
        "claude",
    );
    assert_eq!(w, "PASS", "{why}");
    assert!(
        why.contains(&format!("its picker's `{subscription}` row named")),
        "{why}"
    );
    let (w, why) = gone(
        &with_reason("nothing says which method this session signs in with"),
        "claude",
    );
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(why.contains("THE PICKER RULE WAS NOT REACHED"), "{why}");
    // The old rule's own refusal in the fullscreen view, past a method it read, is no reach either.
    let (w, why) = gone(
        &with_reason(
            "it is drawn in Claude Code's fullscreen view, past whose method pick aterm reads \
             nothing yet",
        ),
        "claude",
    );
    assert_eq!(w, "UNOBSERVED", "{why}");
    // The environment's token names no method, and its run wants none named.
    let mut s = with_reason("nothing says which method this session signs in with");
    if let Some(p) = s.cover.login.as_mut() {
        p.stored = false;
    }
    assert_eq!(gone(&s, "claude").0, "PASS");
}

/// Between the person's pick and their prompt aterm's `login-done@v1` (its Enter at `Login
/// successful`, a real sign-in's) and the `login-cancelled` notice their own Esc draws are no
/// act or word of aterm's over the person: anything else is.
#[test]
fn past_the_persons_pick_only_the_sign_ins_own_enter_and_the_escs_notice_pass() {
    let mut s = expired();
    push(
        &mut s,
        LIFT - 2_500,
        "EVENT idle seq=4 Login successful. Press Enter to continue",
        Some(point(LIFT - 2_500, 1)),
    );
    push(
        &mut s,
        LIFT - 2_000,
        &format!("TYPED seq=4 rule={RULE_LOGIN_DONE} Enter"),
        None,
    );
    push(
        &mut s,
        LEFT + 700,
        &format!(
            "EXCEPTION seq=4 kind=login when=now notice={} Claude Code's sign-in was cancelled",
            LOGIN_CANCELLED.id
        ),
        None,
    );
    let (w, why) = signed(&s, "claude");
    assert_eq!(w, "PASS", "{why}");
    assert!(
        why.contains(&format!(
            "the person's Esc drew {} at +",
            LOGIN_CANCELLED.id
        )),
        "{why}"
    );
    // Another act of aterm's there fails.
    let mut s = expired();
    push(
        &mut s,
        LIFT - 2_000,
        &format!("TYPED seq=4 rule={RULE_LOGIN} /login"),
        None,
    );
    let (w, why) = signed(&s, "claude");
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("TYPED PAST THE PERSON'S PICK"), "{why}");
    // The cancelled notice before the person's Esc, or the login's notice again, is aterm's own.
    for (t, notice) in [
        (LIFT - 2_000, LOGIN_CANCELLED.id),
        (LEFT + 700, LOGIN_CHOOSE.id),
    ] {
        let mut s = expired();
        push(
            &mut s,
            t,
            &format!("EXCEPTION seq=4 kind=login when=now notice={notice} x"),
            None,
        );
        let (w, why) = signed(&s, "claude");
        assert_eq!(w, "FAIL", "{notice}: {why}");
        assert!(why.contains("TOLD PAST THE PERSON'S PICK"), "{why}");
    }
}

/// On agy and Muse Code nothing of aterm's comes between the stand-in's sign-in and the person's
/// prompt either, and the escalation is down once they prompted.
#[test]
fn on_agy_an_act_past_the_sign_in_or_the_escalation_left_up_fails() {
    let mut s = expired_agy();
    push(
        &mut s,
        PICK + 5_000,
        "CONTINUED seq=4 rule=continue@v1 [aterm harness] keep going",
        None,
    );
    let (w, why) = signed(&s, "agy");
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("TYPED PAST THE SIGN-IN: continue@v1"), "{why}");
    let mut s = expired_agy();
    if let Some(m) = s.meta.last_mut() {
        *m = attention(ANSWERED + 1_000, Some(&agy_told()));
    }
    let (w, why) = signed(&s, "agy");
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("THE LOGIN'S ESCALATION STILL UP"), "{why}");
    let mut s = expired_agy();
    s.meta.pop();
    let (w, why) = signed(&s, "agy");
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(
        why.contains("attention was never read past the prompt"),
        "{why}"
    );
}

/// The back-off the person's turn drew is bounded from outside the journal's own word: no streak
/// longer than the points the tab read, and no due past what the policy's ladder gives the streak
/// the journal names.
#[test]
fn a_back_off_past_the_policys_ladder_fails() {
    // Seven short turns of the four points the tab read, an hour's back-off.
    let mut s = backed_off(2_000);
    for (t, l) in &mut s.journal {
        if l.contains("3 short turn(s)") {
            (*t, *l) = backoff(ANSWERED + 100, 5, 7, 60 * 60_000);
        }
    }
    let (w, why) = signed(&s, "claude");
    assert_eq!(w, "FAIL", "{why}");
    assert!(
        why.contains("BACKED OFF PAST THE LADDER") && why.contains("7 short turns in a row"),
        "{why}"
    );
    // Three short turns whose back-off is due an hour on: the ladder gives them 8 min.
    let mut s = backed_off(2_000);
    for (t, l) in &mut s.journal {
        if l.contains("3 short turn(s)") {
            (*t, *l) = backoff(ANSWERED + 100, 5, 3, 60 * 60_000);
        }
    }
    let (w, why) = signed(&s, "claude");
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("BACKED OFF PAST THE LADDER"), "{why}");
    // The control: the ladder's own 8 min passes.
    assert_eq!(signed(&backed_off(2_000), "claude").0, "PASS");
}

/// THE STREAK STANDS AT THE AGENT'S OWN SHORT ANSWERS past the person's sign-in (v6 §14.17.5.7):
/// the act the expired login cut and the person's own turn add nothing, so no point and no
/// back-off past their prompt names more short turns in a row than the point the act that met
/// the wall was typed on. 12d's run (one before the wall, three past the prompt, Cruise's act on
/// the 8 min back-off) and a build that counts the cut act alone (two) fail, under Cruise and
/// Land and on agy; the control, a streak of three the agent's own short answers earned before
/// the wall, passes with the same 8 min back-off.
#[test]
fn past_the_persons_sign_in_a_streak_the_cut_act_or_their_turn_lengthened_fails() {
    let (w, why) = signed(&expired(), "claude");
    assert_eq!(w, "PASS", "{why}");
    assert!(
        why.contains(
            "the streak kept: 1 short turn(s) in a row past the person's prompt, of the 1 before \
             the act the wall cut"
        ),
        "{why}"
    );
    for streak in [3, 2] {
        let (w, why) = signed(&streak_past_prompt(streak, 2_000), "claude");
        assert_eq!(w, "FAIL", "{streak}: {why}");
        assert!(
            why.contains(&format!(
                "COUNTED A TURN THAT WAS NOT THE AGENT'S ANSWER: at +463.0 s a point names \
                 {streak} short turns in a row past the person's prompt (+460.0 s), of the 1 the \
                 point at +5.0 s named before the act the wall cut (+125.0 s)"
            )),
            "{streak}: {why}"
        );
    }
    // The back-off's line alone names it, the point record unread.
    let mut s = streak_past_prompt(3, 2_000);
    s.ledger.retain(|r| r.ts != T0 + ANSWERED);
    let (w, why) = signed(&s, "claude");
    assert_eq!(w, "FAIL", "{why}");
    assert!(
        why.contains("at +463.1 s a back-off names 3 short turns in a row"),
        "{why}"
    );
    // The control: the agent's own three before the wall stand past it.
    let (w, why) = signed(&backed_off(2_000), "claude");
    assert_eq!(w, "PASS", "{why}");
    assert!(why.contains("the streak kept: 3 short turn(s)"), "{why}");
    // Under Land, 12d's run (one short turn by the act the wall cut) fails as well.
    let mut s = landed_after(2_000);
    s.ledger.retain(|r| r.ts != T0 + MET - 1_000);
    let (w, why) = land_signed(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(
        why.contains("COUNTED A TURN THAT WAS NOT THE AGENT'S ANSWER"),
        "{why}"
    );
    assert_eq!(land_signed(&landed_after(2_000)).0, "PASS");
    // On agy, the person's turn counted: two past their prompt.
    let mut s = expired_agy();
    for r in &mut s.ledger {
        if r.ts == T0 + ANSWERED {
            *r = point(ANSWERED, 2);
        }
    }
    let (w, why) = signed(&s, "agy");
    assert_eq!(w, "FAIL", "{why}");
    assert!(
        why.contains("a point names 2 short turns in a row"),
        "{why}"
    );
    // No point record before the act that met the wall: the streak it was typed on is unread.
    let mut s = expired();
    s.ledger.retain(|r| r.ts != T0 + 5_000);
    let (w, why) = signed(&s, "claude");
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(
        why.contains("the streak it was typed on is unread"),
        "{why}"
    );
}

/// A STREAK THE PERSON'S TURN RESET FAILS (v6 §14.17.5.7): past the person's prompt a point naming
/// none of the one short turn the agent's own answer earned before the expired login fails, under
/// Cruise: a person's own turn neither counts nor resets. NEGATIVE CONTROL: the one-sided want
/// passes it.
#[test]
fn past_the_persons_sign_in_a_streak_their_turn_reset_fails() {
    let mut s = expired();
    for r in &mut s.ledger {
        if r.ts == T0 + ANSWERED {
            *r = point(ANSWERED, 0);
        }
    }
    let (w, why) = signed(&s, "claude");
    assert_eq!(w, "FAIL", "{why}");
    assert!(
        why.contains("DROPPED THE AGENT'S OWN SHORT ANSWERS: at +463.0 s a point names 0"),
        "{why}"
    );
}
