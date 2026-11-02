// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `lab forever --mode sysmsg`'s EXPIRED LOGIN (v6 §14.17.5.6): the stub's stand-in answers
//! each program's turn with its real expired-login form, under Cruise (`LOG`, `sys-login`) and
//! Land (`LLOG`, `sys-land-login`), each alone on a lab of its own:
//!
//! * Claude Code runs on a stored claude.ai login of the lab's, as a person's `/login` leaves one
//!   ([`lay_stored`]: `.credentials.json` and a Max plan's profile in the scratch, the stub's base
//!   URL its only route, which the instance's route seam admits), so aterm reads its method, a
//!   subscription's, and the picker rule slice 12a changed is in reach ([`stored_arm`]);
//!   `--token-login` runs it on the environment's token instead (`claude --subscriber`), where
//!   nothing names the method. The stub answers it as the Anthropic API answers an expired OAuth
//!   token: a 401 `authentication_error` ([`claude_spec`], [`EXPIRED`]), which Claude Code draws
//!   as a lost login its reader names (`Please run /login`). agy and Muse Code get the stub's
//!   `401`: the Gemini API's `UNAUTHENTICATED` and the Responses API's rejected key, each a login
//!   wall its reader names. No program here goes without one, so none is a named skip.
//! * The stand-in rises as a system message's does ([`super::step`]): at Cruise's first point, or
//!   under Land while the stub holds the first prompt's answer, so the mode's own act meets it.
//! * Every agent runs under a scratch HOME with an inert browser: the lab's `open` shim first on
//!   `PATH` and in `$BROWSER`, which only logs what it was asked to open (`<scratch>/open.log`,
//!   [`crate::recorder`]), while the jail denies `/usr/bin/open` and Launch Services, unlogged.
//!   The recorder's own control runs at the raise ([`Played::control`]), and the person's pick is
//!   its live control on Claude Code's own way of opening the browser.
//! * [`judge`] wants, while the login is gone (from the act that met it to the person's first
//!   key): on Claude Code `/login` typed at most once and nothing else typed, no key of aterm's in
//!   its method picker (no `login-method@v1`, no other approval), the inert browser never run, no
//!   escalation, and one quiet notice (`login-choose`: one `EXCEPTION … kind=login` journaled, one
//!   band row of `need=now`, which plays no sound, opens no dialog and brings no window forward);
//!   on agy and Muse Code nothing typed, no band row, and one escalation (`ESCALATED` journaled)
//!   whose words (its ledger row's reason, else the tab's attention, what the person's badge shows)
//!   name how the agent signs in (`turn_end::sign_in_how`, [`escalations`]). A want with no
//!   evidence makes no pass: the control not logged, the browser or the band never read, no picker
//!   drawn, or on the stored login a picker whose reason names no subscription row.
//! * The person's play is the lab's, last ([`step`]), [`QUIET_MS`] past the wall's first reading:
//!   on Claude Code the person presses the picker's first method as a person does (`key
//!   as=person enter`), which hands the sign-in to the browser, so the inert browser logs it
//!   (read each look until it does, [`HANDOFF_HANG_MS`]). Then the stand-in signs in: the stub
//!   answers again. A real Claude Code sign-in ends at its production OAuth host, which the lab's
//!   jail keeps it from (its public build names no other), so the person leaves the dialog (Esc)
//!   and, signed in, prompts again; aterm's own steps past a real sign-in (`login-done@v1`'s
//!   Enter, `login-back@v1`'s line) are its unit and loop tests', never this lab's. On agy and
//!   Muse Code, whose sign-in is a relaunch or a key the person fixes, the person prompts again
//!   once the stub answers. [`judge_signed_in`] wants the hand-off logged on the person's key,
//!   nothing of aterm's between the pick (or the sign-in) and their prompt but `login-done@v1`,
//!   no notice there but the `login-cancelled` their own Esc draws, and past the prompt the tab
//!   going on once: no `/login` and no escalation again, nothing typed over the person's turn,
//!   no back-off past the policy's ladder ([`past_ladder`]), no short streak longer than the one
//!   the act that met the wall was typed on (the cut act and the person's turn add nothing,
//!   [`super::streak_kept`], v6 §14.17.5.7), the notice or escalation down, then Cruise's own act
//!   by its own back-off ([`go_on_by`]), or Land's landing ([`land_by`]).

use aterm_agent::supervise::policy::login::{RULE_LOGIN_DONE, RULE_LOGIN_METHOD};
use aterm_agent::supervise::policy::notice::{LOGIN_CANCELLED, LOGIN_CHOOSE};
use aterm_agent::supervise::policy::turn_end::{self, RULE_LOGIN, RULE_LOGIN_BACK, TurnEndTiming};
use aterm_phase::LoginMethod;

use super::super::outage::{Act, acts_after, first_read};
use super::super::{
    Chain, Harness, Row, Seen, Tab, Verdict, at, backoff_due, backoff_streak, on, pct_decode,
    person_turn, word,
};
use super::{
    Form, LAND_ON_MS, LandOn, STANDS_MS, WATCH_PAST_MS, land_end, land_on_by, met, streak_kept,
    wall_read,
};
use crate::Fail;
use crate::claude_login_proof::PICKER_ROW;
use crate::door::Door;
use crate::pyjson::J;
use crate::run::Run;
use crate::util::{now_ms, one_line};

/// Claude Code's stand-in's custom mode ([`claude_spec`]).
pub const MODE: &str = "login-expired";

/// The Anthropic API's message for an expired claude.ai OAuth token (a 401
/// `authentication_error`), as it answers a request carrying one.
pub const EXPIRED: &str =
    "OAuth token has expired. Please obtain a new token or refresh your existing token.";

/// How long past the wall's first reading the login stands with nobody at it before the lab plays
/// the person ([`step`]): a system message's stand ([`STANDS_MS`]), past Cruise's short-turn
/// back-off and Land's acts, so a harness that reads the login as a plain point types into it.
pub const QUIET_MS: i64 = STANDS_MS;

/// How long past the person's pick the lab reads the inert browser's log for their hand-off, a
/// read each look until it shows: a one-minute hang detector (Claude Code opens the browser
/// within a few seconds of the pick).
pub const HANDOFF_HANG_MS: i64 = 60_000;

/// The stored login's label ([`lay_stored`]): the stub reads its token as `oat:<label>`.
pub const STORED_LABEL: &str = "lab-login";

/// Whether harness `h`'s run of `chains` takes the expired login on a stored login of the lab's
/// ([`lay_stored`]): Claude Code's, unless `--token-login` named the environment's token. A
/// stored login is what a person's `/login` leaves, and the only one whose method aterm reads
/// (a subscription's, from its profile's plan), so the picker rule slice 12a changed is reached on
/// it alone (in the inline renderer [`lay_stored`] picks): on the environment's token nothing
/// names the method, and every build leaves the picker to the person.
#[must_use]
pub fn stored_arm(h: Harness, chains: &[Chain], token_login: bool) -> bool {
    h == Harness::Claude && !token_login && chains.iter().any(|c| c.login())
}

/// LAYS THE STORED LOGIN in the lab's scratch, nowhere else, as a person's `/login` leaves one: a
/// live fake login of [`STORED_LABEL`]'s in `$CLAUDE_CONFIG_DIR/.credentials.json`, where Claude
/// Code keeps it when no keychain answers (the jail denies the keychain, and its `security`
/// stand-in answers none), its profile naming a Max plan's account (`oauthAccount`), no
/// `apiKeyHelper`, and no proxy in the settings' `env`: the stub's base URL is its only route,
/// which the instance's route seam admits as the default route. Claude Code draws in its inline
/// renderer (`tui: "default"`), where the rule before slice 12a pressed the picker's subscription
/// row: in the fullscreen view that rule left the picker to the person anyway (`it is drawn in
/// Claude Code's fullscreen view, …`), so a run there could not tell the two rules apart.
///
/// # Errors
/// The scratch settings, login or profile could not be read or written.
pub fn lay_stored(run: &Run) -> Result<(), Fail> {
    let mut env = J::Obj(Vec::new());
    for k in crate::lab::PROXY_KEYS {
        env.set(k, J::Null);
    }
    env.set("ANTHROPIC_BASE_URL", J::s(crate::lab::stub_base_url(run)));
    env.set("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", J::s("1"));
    crate::verbs::settings_merge(
        run,
        &J::obj([
            ("apiKeyHelper", J::Null),
            ("env", env),
            ("tui", J::s("default")),
        ]),
    )?;
    let dir = run.w.join("home/.claude");
    let put = |name: &str, text: &str| {
        let p = dir.join(name);
        crate::sys::put(&p, 0o600, &crate::sys::PutSrc::Bytes(text.as_bytes()))
            .map_err(|e| Fail::die(format!("the stored login: {}: {e}", p.display())))
    };
    put(
        ".credentials.json",
        &crate::claude_rotation_proof::live_login(STORED_LABEL, now_ms()),
    )?;
    let text = crate::verbs::read_scratch(&dir.join(".claude.json"))?;
    let mut profile = J::parse(&text)
        .map_err(|e| Fail::die(format!("the stored login: the scratch .claude.json: {e}")))?;
    profile.set(
        "oauthAccount",
        crate::stub::account::composed_oauth_account(now_ms()),
    );
    put(".claude.json", &profile.dump(crate::pyjson::COMPACT))
}

/// What the person types once signed in again.
pub const SIGNED_PROMPT: &str = "I signed in again: go on with the parser where you stopped.";

/// How long past the person's prompt Cruise has to go on where it decided no short-turn back-off
/// past it ([`go_on_by`]): its answer and a step, with room.
pub const GO_ON_MS: i64 = 360_000;

/// How far past the end of the back-off its line names Cruise's act may come: a step and its read.
const BACK_OFF_LATE_MS: i64 = 60_000;

/// The ends of the short-turn back-offs the journal decided past the person's prompt at `p` and
/// before `before`, each a minute late at most ([`BACK_OFF_LATE_MS`]). The person's turn adds
/// nothing to the short streak (v6 §14.17.5.7), but the streak the agent's own short answers
/// earned before the wall stands past it ([`super::streak_kept`]), so the back-off it draws (8
/// min at a third short turn) can outlast the mode's own bound.
fn backed_off(seen: &Seen, p: i64, before: i64) -> impl Iterator<Item = i64> + '_ {
    seen.journal
        .iter()
        .filter(move |(t, _)| *t > p && *t < before)
        .filter_map(|(_, l)| super::super::backoff_due(l))
        .map(|due| due + BACK_OFF_LATE_MS)
}

/// When Cruise is due to have gone on past the person's prompt at `p`: by the end of the latest
/// back-off before `before` (its first act, [`backed_off`]), and never sooner than [`GO_ON_MS`]
/// past the prompt.
#[must_use]
pub fn go_on_by(seen: &Seen, p: i64, before: i64) -> i64 {
    backed_off(seen, p, before).fold(p + GO_ON_MS, i64::max)
}

/// When Land is due to have landed or held past the person's prompt at `p`: [`LAND_ON_MS`] past
/// the prompt, or past the end of the latest back-off before its landing or hold
/// ([`backed_off`]).
#[must_use]
pub fn land_by(seen: &Seen, p: i64) -> i64 {
    let end = land_end(seen, p).map_or(i64::MAX, |(t, _)| t);
    backed_off(seen, p, end)
        .map(|due| due + LAND_ON_MS)
        .fold(p + LAND_ON_MS, i64::max)
}

/// The lab's play of the person at an expired login ([`step`]), each with its time (ms).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Played {
    /// Whether the recorder's control, run at the raise, was logged ([`crate::recorder::control`]):
    /// the shim records an open in the jail.
    pub control: Option<bool>,
    /// Claude Code on a stored login of the lab's ([`stored_arm`]), whose method aterm reads.
    pub stored: bool,
    /// The inert browser's log as the person came to the tab, before any key of theirs: what
    /// aterm or the agent opened while the login was gone.
    pub opened_before: Option<String>,
    /// When the person pressed the method picker's first row (Claude Code).
    pub picked_ms: Option<i64>,
    /// Why the person pressed no method where they did not (no picker drawn).
    pub no_pick: Option<String>,
    /// The inert browser's log past the person's pick: once it grew, or [`HANDOFF_HANG_MS`] past
    /// the pick.
    pub opened_after: Option<String>,
    /// When the person's Esc left Claude Code's sign-in dialog.
    pub left_ms: Option<i64>,
    /// When the person's prompt went in (their Enter).
    pub prompt_ms: Option<i64>,
    /// The composer did not take the person's prompt.
    pub prompt_refused: bool,
}

/// The stub's answer to Claude Code at an expired login: the Anthropic API's 401 for an expired
/// OAuth token ([`EXPIRED`]).
#[must_use]
pub fn claude_spec() -> J {
    J::obj([
        ("status", J::Int(401)),
        (
            "json",
            J::obj([
                ("type", J::s("error")),
                (
                    "error",
                    J::obj([
                        ("type", J::s("authentication_error")),
                        ("message", J::s(EXPIRED)),
                    ]),
                ),
            ]),
        ),
    ])
}

/// Raises the expired login's stand-in for harness `h`: Claude Code's custom mode ([`MODE`]),
/// else the stub's `401`. What the stub answered, and the mode's name.
///
/// # Errors
/// The stub did not take the mode.
pub fn raise(run: &Run, h: Harness) -> Result<(String, &'static str), Fail> {
    match h {
        Harness::Claude => Ok((
            crate::verbs::set_custom_mode(run, MODE, &claude_spec())?,
            MODE,
        )),
        Harness::Agy | Harness::Muse => Ok((
            crate::verbs::set_mode(run, Form::Login.stub_mode())?,
            Form::Login.stub_mode(),
        )),
    }
}

/// The inert browser's log as it stands.
fn opened(run: &Run) -> String {
    crate::util::scratch_text(&run.w.join("open.log"))
}

/// The person's first key at the tab ([`Played`]) or the stand-in's sign-in, whichever came
/// first: where the login's gone time ends.
#[must_use]
pub fn person_at(seen: &Seen) -> Option<i64> {
    let p = seen.cover.login.as_ref();
    [
        p.and_then(|p| p.picked_ms),
        p.and_then(|p| p.left_ms),
        p.and_then(|p| p.prompt_ms),
        seen.cover.outage_lifted_ms,
    ]
    .into_iter()
    .flatten()
    .min()
}

/// RAISES THE EXPIRED LOGIN when it is due ([`super::step`]'s rule), and plays the person
/// [`QUIET_MS`] past the wall's first reading, one move a look: Claude Code's pick of its first
/// method, the inert browser read past it, the stand-in's sign-in (the stub's scripted mode
/// back), the person's Esc out of Claude Code's dialog, then the person's prompt.
///
/// # Errors
/// The stub did not take a mode, or a key of the person's was refused.
pub(super) fn step(
    run: &Run,
    door: &Door<'_>,
    h: Harness,
    tab: &mut Tab,
    now: i64,
) -> Result<(), Fail> {
    let name = tab.chain.name();
    if tab.seen.cover.outage_ms.is_none() {
        if super::raise_due(run, tab) {
            let (answer, mode) = raise(run, h)?;
            tab.seen.cover.outage_ms = Some(now);
            let control = crate::recorder::control(run);
            let logged = crate::recorder::nothing_opened("", &crate::util::scratch_text(&control));
            tab.seen.cover.login = Some(Played {
                control: Some(logged),
                stored: h == Harness::Claude && run.route_seam == "1",
                ..Played::default()
            });
            println!(
                "  [{name}] +{} stub    the expired login raised: mode {mode}, the person signs in {} s past the wall's first reading ({}); the inert browser's control {}",
                at(tab.t0, now),
                QUIET_MS / 1000,
                one_line(&answer, 120),
                if logged { "logged" } else { "NOT logged" }
            );
            super::open_gate(run, tab);
        }
        return Ok(());
    }
    let Some(wall) = wall_read(&tab.seen) else {
        return Ok(());
    };
    if now < wall + QUIET_MS {
        return Ok(());
    }
    let sid = tab.sid.clone();
    let t0 = tab.t0;
    let at_ = |t: i64| at(t0, t);
    let played = tab.seen.cover.login.get_or_insert_with(Played::default);
    if played.opened_before.is_none() {
        played.opened_before = Some(opened(run));
    }
    if h == Harness::Claude && played.picked_ms.is_none() && played.no_pick.is_none() {
        let screen = on(door, &sid, &["text"]);
        if screen.contains(PICKER_ROW) {
            let r = door.ctl(&[&format!("@{sid}"), "key", "as=person", "enter"])?;
            if r.code != 0 {
                return Err(Fail::die(format!(
                    "the person's pick: `key as=person enter` refused ({}{}): it needs the seam, a debug or dev-seams aterm",
                    r.stdout.trim(),
                    r.stderr.trim()
                )));
            }
            let t = now_ms();
            played.picked_ms = Some(t);
            println!(
                "  [{name}] +{} person  pressed the sign-in picker's first method (\"{PICKER_ROW}\")",
                at_(t)
            );
        } else {
            let last: Vec<&str> = screen
                .lines()
                .filter(|l| !l.trim().is_empty())
                .rev()
                .take(4)
                .collect();
            played.no_pick = Some(format!(
                "no sign-in method picker on the screen (its last rows: {})",
                one_line(&last.into_iter().rev().collect::<Vec<_>>().join(" / "), 200)
            ));
            println!(
                "  [{name}] +{} person  {}",
                at_(now),
                played.no_pick.as_deref().unwrap_or_default()
            );
        }
        return Ok(());
    }
    if let Some(picked) = played.picked_ms
        && played.opened_after.is_none()
    {
        let log = opened(run);
        let before = played.opened_before.as_deref().unwrap_or_default().len();
        if log.len() <= before && now < picked + HANDOFF_HANG_MS {
            return Ok(());
        }
        println!(
            "  [{name}] +{} browser the inert browser's log past the pick: {}",
            at_(now),
            if log.trim().is_empty() {
                "(nothing)".to_string()
            } else {
                one_line(&log, 200)
            }
        );
        played.opened_after = Some(log);
        return Ok(());
    }
    if tab.seen.cover.outage_lifted_ms.is_none() {
        let answer = crate::verbs::set_mode(run, super::super::MODE)?;
        tab.seen.cover.outage_lifted_ms = Some(now);
        println!(
            "  [{name}] +{} stub    the stand-in signed in: mode {} again ({})",
            at_(now),
            super::super::MODE,
            one_line(&answer, 120)
        );
        return Ok(());
    }
    let played = tab.seen.cover.login.get_or_insert_with(Played::default);
    if h == Harness::Claude && played.left_ms.is_none() {
        let r = door.ctl(&[&format!("@{sid}"), "key", "as=person", "esc"])?;
        if r.code != 0 {
            return Err(Fail::die(format!(
                "the person's Esc: `key as=person esc` refused ({}{})",
                r.stdout.trim(),
                r.stderr.trim()
            )));
        }
        let t = now_ms();
        played.left_ms = Some(t);
        println!(
            "  [{name}] +{} person  Esc: left the sign-in dialog (its sign-in ends at Claude Code's production OAuth host, out of the lab's reach)",
            at_(t)
        );
        return Ok(());
    }
    if played.prompt_ms.is_none() && !played.prompt_refused {
        let sent = person_turn(door, &sid, h.name(), SIGNED_PROMPT)?;
        let played = tab.seen.cover.login.get_or_insert_with(Played::default);
        match sent {
            Some((t, stamp)) => {
                played.prompt_ms = Some(t);
                println!(
                    "  [{name}] +{} person  signed in, prompted: \"{SIGNED_PROMPT}\" (status human_ms={stamp})",
                    at_(t)
                );
            }
            None => {
                played.prompt_refused = true;
                println!(
                    "  [{name}] +{} person  the composer did NOT take the person's prompt",
                    at_(now)
                );
            }
        }
    }
    Ok(())
}

/// Whether the expired login's watch is over: the person's prompt refused; Cruise's first act
/// past the person's prompt [`WATCH_PAST_MS`] ago, or none by when it is due ([`go_on_by`]);
/// under Land (`land`), its landing or hold past the prompt [`WATCH_PAST_MS`] ago, or neither by
/// when it is due ([`land_by`]). The chain's hang detector ends it otherwise.
#[must_use]
pub fn watched(seen: &Seen, land: bool, now: i64) -> bool {
    let Some(played) = seen.cover.login.as_ref() else {
        return false;
    };
    if played.prompt_refused {
        return true;
    }
    let Some(p) = played.prompt_ms else {
        return false;
    };
    if land {
        return match land_end(seen, p) {
            Some((t, _)) => now - t >= WATCH_PAST_MS,
            None => now >= land_by(seen, p),
        };
    }
    match acts_after(seen, p).first() {
        Some(a) => now - a.at >= WATCH_PAST_MS,
        None => now >= go_on_by(seen, p, i64::MAX),
    }
}

/// The harness's band rows of a `messages tag=harness` reply that raise the band (`sev=warn` and
/// up), each `<id> at=<ms> state=<state> need=<class> title=<title>`, joined by ` | `; `-` for
/// none.
#[must_use]
pub fn rows(messages: &str) -> String {
    let rows: Vec<String> = messages
        .lines()
        .filter_map(|l| l.strip_prefix("message "))
        .filter(|l| matches!(word(l, "sev").as_deref(), Some("warn" | "error")))
        .map(|l| {
            format!(
                "{} at={} state={} need={} title={}",
                l.split_whitespace().next().unwrap_or("?"),
                word(l, "at").unwrap_or_else(|| "?".into()),
                word(l, "state").unwrap_or_else(|| "?".into()),
                word(l, "need").unwrap_or_else(|| "-".into()),
                pct_decode(&word(l, "title").unwrap_or_default())
            )
        })
        .collect();
    if rows.is_empty() {
        "-".to_string()
    } else {
        rows.join(" | ")
    }
}

/// One band row of [`rows`]: its id, when it was posted (ms), its state, its need's class and
/// its title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BandRow {
    /// Its id.
    pub id: String,
    /// `at=`, ms.
    pub at: i64,
    /// `state=`.
    pub state: String,
    /// `need=`: `now` for a need the band shows at once, which plays no sound, opens no dialog and
    /// brings no window forward (v6 §14.17's bar, as the hold's ask).
    pub need: String,
    /// The title.
    pub title: String,
}

/// The band rows of a [`rows`] read.
#[must_use]
pub fn band_rows(read: &str) -> Vec<BandRow> {
    read.split(" | ")
        .filter(|r| *r != "-")
        .filter_map(|r| {
            Some(BandRow {
                id: r.split_whitespace().next()?.to_string(),
                at: word(r, "at")?.parse().ok()?,
                state: word(r, "state").unwrap_or_default(),
                need: word(r, "need").unwrap_or_default(),
                title: r.split_once(" title=").map(|(_, t)| t.to_string())?,
            })
        })
        .collect()
}

/// The band rows the tab's reads saw posted from `from` to `to`, each once, as its last read
/// before `to` drew it: what the band said while that time lasted (a row the person's sign-in
/// later superseded reads `need=-` past it).
fn posted(seen: &Seen, from: i64, to: i64) -> Vec<BandRow> {
    let mut seen_rows: Vec<BandRow> = Vec::new();
    for (_, read) in seen.band.iter().filter(|(t, _)| *t < to) {
        for r in band_rows(read) {
            if r.at < from || r.at >= to {
                continue;
            }
            match seen_rows.iter_mut().find(|s| s.id == r.id) {
                Some(s) => *s = r,
                None => seen_rows.push(r),
            }
        }
    }
    seen_rows
}

/// The keys aterm pressed or the texts it sent from `from` to `to` that are no typed act of the
/// journal's: the ledger's approvals and declines (a key in a dialog: a method picked, an Esc).
fn keys_pressed(seen: &Seen, from: i64, to: i64) -> Vec<&Row> {
    seen.ledger
        .iter()
        .filter(|r| r.ts > from && r.ts < to)
        .filter(|r| matches!(r.decision.as_str(), "approved" | "declined"))
        .collect()
}

/// How far apart an escalation's journal line and what it told may be read.
const TOLD_NEAR_MS: i64 = 5_000;

/// The escalations journaled from `from` to `to` (`ESCALATED …`), each with what it told: the
/// reason of the ledger's escalated row beside it where it names one, else the tab's attention
/// (`meta`) as read next.
#[must_use]
pub fn escalations(seen: &Seen, from: i64, to: i64) -> Vec<(i64, String)> {
    seen.journal
        .iter()
        .filter(|(t, l)| *t > from && *t < to && l.starts_with("ESCALATED "))
        .map(|(t, _)| {
            let ledger = seen
                .ledger
                .iter()
                .filter(|r| r.decision == "escalated" && !r.reason.is_empty())
                .find(|r| (r.ts - t).abs() <= TOLD_NEAR_MS)
                .map(|r| r.reason.clone());
            let attention = || {
                seen.meta
                    .iter()
                    .filter(|(m, _)| *m >= t - TOLD_NEAR_MS)
                    .find(|(_, w)| word(w, "attention_owners").is_some_and(|o| o != "0"))
                    .and_then(|(_, w)| w.split_once(" attention=").map(|(_, a)| a.to_string()))
            };
            (*t, ledger.or_else(attention).unwrap_or_default())
        })
        .collect()
}

/// The login exceptions journaled from `from` to `to` (`EXCEPTION … kind=login …`).
fn login_exceptions(seen: &Seen, from: i64, to: i64) -> Vec<&(i64, String)> {
    seen.journal
        .iter()
        .filter(|(t, l)| {
            *t > from
                && *t < to
                && l.starts_with("EXCEPTION ")
                && word(l, "kind").as_deref() == Some("login")
        })
        .collect()
}

/// How the agent of harness `h` signs in, as an escalation's `reason` names it, if it does
/// ([`turn_end::sign_in_how`], the policy's own words): read for no wall, for each wall the tab's
/// points from `from` to `to` drew and for each the reason quotes (the words past a `: ` in it), so
/// that a program's own remedy (Muse Code's) is read off the wall it names.
#[must_use]
pub fn names_sign_in(
    seen: &Seen,
    h: Harness,
    reason: &str,
    (from, to): (i64, i64),
) -> Option<String> {
    let walls = seen
        .journal
        .iter()
        .filter(|(t, _)| *t > from && *t <= to)
        .filter_map(|(_, l)| {
            let rest = l.strip_prefix("EVENT ")?;
            let (_, words) = rest.split_once(" seq=")?.1.split_once(' ')?;
            Some(words.trim_start_matches(|c: char| !c.is_alphanumeric()))
        });
    let quoted = reason.split(['(', ')']).flat_map(|part| {
        part.match_indices(": ")
            .map(move |(i, _)| part[i + 2..].trim())
    });
    std::iter::once("")
        .chain(walls)
        .chain(quoted)
        .map(|wall| turn_end::sign_in_how(h.program(), wall))
        .find(|how| reason.contains(how.as_str()))
}

fn acts_said(acts: &[Act<'_>], t0: i64) -> String {
    acts.iter()
        .map(|a| format!("{} at +{}", a.rule, at(t0, a.at)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn told_said(told: &[(i64, String)], t0: i64) -> String {
    told.iter()
        .map(|(t, why)| format!("+{} \"{}\"", at(t0, *t), one_line(why, 100)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn rows_said(rows: &[&Row], t0: i64) -> String {
    rows.iter()
        .map(|r| {
            format!(
                "{} {} at +{} ({})",
                r.decision,
                r.rule,
                at(t0, r.ts),
                one_line(
                    if r.command.is_empty() {
                        &r.reason
                    } else {
                        &r.command
                    },
                    80
                )
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// What the scenarios hold on harness `h`, for the summary: `LOG`/`LLOG` (`signed` false) and
/// `SGN`/`LSGN` (`signed` true), under Land where `land`. Claude Code's are its `/login`'s and its
/// picker's; agy and Muse Code type no `/login` and draw no picker.
#[must_use]
pub const fn what_on(h: Harness, land: bool, signed: bool) -> &'static str {
    match (h, land, signed) {
        (Harness::Claude, false, false) => {
            "an expired login under Cruise: /login typed at most once, its picker and the browser left to the person, one quiet need, nothing else typed"
        }
        (Harness::Claude, true, false) => {
            "an expired login under Land: /login typed at most once, its picker and the browser left to the person, one quiet need, no act of Land's"
        }
        (Harness::Claude, false, true) => {
            "the person's sign-in after an expired login under Cruise: the browser on their key alone, nothing of aterm's past their pick, then Cruise goes on once"
        }
        (Harness::Claude, true, true) => {
            "the person's sign-in after an expired login under Land: the browser on their key alone, nothing of aterm's past their pick, then Land carries on to its landing"
        }
        (_, false, false) => {
            "an expired login under Cruise: nothing typed, one quiet escalation naming how the agent signs in, no band row"
        }
        (_, true, false) => {
            "an expired login under Land: nothing typed, one quiet escalation naming how the agent signs in, no band row, no act of Land's"
        }
        (_, false, true) => {
            "the person's sign-in after an expired login under Cruise: nothing of aterm's past the sign-in, the escalation down, then Cruise goes on once"
        }
        (_, true, true) => {
            "the person's sign-in after an expired login under Land: nothing of aterm's past the sign-in, the escalation down, then Land carries on to its landing"
        }
    }
}

/// The verdict of a judge's findings: a fail where any want failed, else unobserved where a want's
/// evidence was missing, else a pass.
fn verdict(said: String, bad: &[String], unseen: &[String]) -> Verdict {
    if !bad.is_empty() {
        Verdict::Fail(format!("{said}: {}", bad.join("; ")))
    } else if !unseen.is_empty() {
        Verdict::Unobserved(format!("{said}; {}", unseen.join("; ")))
    } else {
        Verdict::Pass(said)
    }
}

/// The journal's word on Claude Code's method picker from `from` to `to`: its escalation's line
/// (`NOT ESCALATED … the notice login-choose stands: …`, or `ESCALATED …`), whose reason names
/// the method aterm read (`login::pick_method`'s words).
fn picker_reason(seen: &Seen, from: i64, to: i64) -> Option<&str> {
    seen.journal
        .iter()
        .filter(|(t, _)| *t > from && *t < to)
        .map(|(_, l)| l.as_str())
        .find(|l| {
            (l.starts_with("NOT ESCALATED ") || l.starts_with("ESCALATED "))
                && l.contains("login method picker")
        })
}

/// A SHORT-TURN BACK-OFF PAST THE POLICY'S OWN LADDER, journaled past the person's prompt at `p`:
/// one naming more short turns in a row than the tab read points (`EVENT …`) from its start to
/// that line, each point one short turn at most, or one due later than the ladder gives the
/// streak it names (`TurnEndTiming::short_wait_at`, the decider's own source), a minute late at
/// most ([`BACK_OFF_LATE_MS`]). [`go_on_by`] and [`land_by`] take the back-off as the journal
/// names it, so this bounds it from outside the journal's own word.
#[must_use]
pub fn past_ladder(seen: &Seen, p: i64, t0: i64) -> Option<String> {
    let timing = TurnEndTiming::default();
    seen.journal
        .iter()
        .filter(|(t, _)| *t > p)
        .find_map(|(t, l)| {
            let due = backoff_due(l)?;
            let streak = backoff_streak(l)?;
            let points = seen
                .journal
                .iter()
                .filter(|(at, x)| at <= t && x.starts_with("EVENT "))
                .count();
            let points = u32::try_from(points).unwrap_or(u32::MAX);
            if streak > points {
                return Some(format!(
                    "BACKED OFF PAST THE LADDER: at +{} the journal names {streak} short turns in a row, of the {points} points the tab read",
                    at(t0, *t)
                ));
            }
            let ladder = timing
                .short_wait_at(streak)
                .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
            (due > t + ladder + BACK_OFF_LATE_MS).then(|| {
                format!(
                    "BACKED OFF PAST THE LADDER: at +{} a back-off due {} later, past the {} the ladder gives {streak} short turns in a row",
                    at(t0, *t),
                    at(*t, due),
                    at(0, ladder)
                )
            })
        })
}

/// JUDGES THE EXPIRED LOGIN on `program` ([`Harness::name`]), under Land where `land`: what the
/// tab saw from the act that met it to the person's first key ([`person_at`]); the module's
/// wants. A want whose evidence is missing makes no pass: the inert browser's control not logged
/// or its log never read, the band never read in that time, and on Claude Code no picker drawn
/// when the person came, or on the stored login ([`Played::stored`]) a picker whose reason names
/// no subscription row (aterm never read the method, or a guard before it decided, so the rule
/// slice 12a changed was not what decided).
#[must_use]
pub fn judge(seen: &Seen, land: bool, program: &str, t0: i64) -> Verdict {
    let mode = if land { "Land" } else { "Cruise" };
    let Some(raised) = seen.cover.outage_ms else {
        return Verdict::Unobserved("the expired login was never raised".into());
    };
    let Some(t1) = met(seen) else {
        return Verdict::Fail(format!(
            "the expired login raised at +{}, but no act of {mode}'s met it within +{}",
            at(t0, raised),
            at(t0, seen.end_ms)
        ));
    };
    let Some(h) = Harness::of(program) else {
        return Verdict::Unobserved(format!("no harness {program}"));
    };
    let Some(wall) = wall_read(seen) else {
        return Verdict::Unobserved(format!(
            "the expired login met at +{} was never read as a point",
            at(t0, t1)
        ));
    };
    let gone_to = person_at(seen).unwrap_or(seen.end_ms + 1);
    let first = acts_after(seen, t1 - 1)
        .first()
        .map_or("?", |a| a.rule)
        .to_string();
    let mut said = format!(
        "the expired login raised at +{}; {mode}'s {first} typed at +{} met it, read at +{}; the person at +{}",
        at(t0, raised),
        at(t0, t1),
        at(t0, wall),
        at(t0, gone_to)
    );
    let mut bad = Vec::new();
    let mut unseen = Vec::new();
    let typed: Vec<Act<'_>> = acts_after(seen, t1)
        .into_iter()
        .filter(|a| a.at < gone_to)
        .collect();
    let (logins, others): (Vec<Act<'_>>, Vec<Act<'_>>) = typed
        .into_iter()
        .partition(|a| h == Harness::Claude && a.rule == RULE_LOGIN);
    if !others.is_empty() {
        bad.push(format!(
            "TYPED WHILE THE LOGIN WAS GONE: {}",
            acts_said(&others, t0)
        ));
    }
    if logins.len() > 1 {
        bad.push(format!(
            "/login TYPED {} TIMES: {}",
            logins.len(),
            acts_said(&logins, t0)
        ));
    }
    if h == Harness::Claude {
        said.push_str(&match logins.first() {
            Some(a) => format!("; /login once at +{}", at(t0, a.at)),
            None => "; no /login typed".to_string(),
        });
    }
    let keys = keys_pressed(seen, t1, gone_to);
    if let Some(r) = keys.iter().find(|r| r.rule == RULE_LOGIN_METHOD) {
        bad.push(format!(
            "ANSWERED THE PICKER: {} {} at +{} ({})",
            r.decision,
            r.rule,
            at(t0, r.ts),
            one_line(&r.command, 100)
        ));
    }
    let other_keys: Vec<&Row> = keys
        .iter()
        .copied()
        .filter(|r| r.rule != RULE_LOGIN_METHOD)
        .collect();
    if !other_keys.is_empty() {
        bad.push(format!(
            "A KEY OF ATERM'S WHILE THE LOGIN WAS GONE: {}",
            rows_said(&other_keys, t0)
        ));
    }
    let played = seen.cover.login.clone().unwrap_or_default();
    match played.opened_before.as_deref() {
        Some(log) if !log.trim().is_empty() => bad.push(format!(
            "THE BROWSER RAN ON ATERM'S KEY: the inert browser logged, before any key of the person's: {}",
            one_line(log, 160)
        )),
        Some(_) if played.control == Some(true) => {
            said.push_str("; the inert browser logged nothing (its control logged)");
        }
        Some(_) => unseen.push(
            "the inert browser's control was not logged at the raise, so no open could have been caught"
                .to_string(),
        ),
        None => unseen.push("the inert browser's log was never read (the person never came)".into()),
    }
    let band_read = seen.band_reads.iter().any(|t| *t > t1 && *t < gone_to);
    if !band_read {
        unseen.push("the band was never read while the login was gone".into());
    }
    let ups = escalations(seen, t1, gone_to);
    match h {
        Harness::Claude => {
            if let Some(why) = played.no_pick.as_deref() {
                unseen.push(format!(
                    "no method picker was drawn when the person came, so its wants (no key of aterm's at it, the browser left to the person) went unobserved: {why}"
                ));
            }
            if played.stored {
                let row = format!("`{}` row", LoginMethod::Subscription.label());
                match picker_reason(seen, t1, gone_to) {
                    Some(r) if r.contains(&row) => {
                        said.push_str(&format!("; on a stored login its picker's {row} named"));
                    }
                    Some(r) => unseen.push(format!(
                        "THE PICKER RULE WAS NOT REACHED on the stored login: its reason names no {row}, so the rule slice 12a changed was not what decided: {}",
                        one_line(r, 160)
                    )),
                    None if bad.is_empty() => unseen.push(
                        "no word on the picker in the journal, so the stored login's method was never seen read"
                            .to_string(),
                    ),
                    None => {}
                }
            }
            let told = login_exceptions(seen, t1, gone_to);
            match told[..] {
                [(t, l)] if word(l, "notice").as_deref() == Some(LOGIN_CHOOSE.id) => {
                    said.push_str(&format!(
                        "; told once at +{} ({}: {})",
                        at(t0, *t),
                        LOGIN_CHOOSE.id,
                        one_line(l, 120)
                    ));
                }
                [(t, l)] => bad.push(format!(
                    "NOT THE PERSON'S PICK: the login's notice at +{} is not `{}`: {}",
                    at(t0, *t),
                    LOGIN_CHOOSE.id,
                    one_line(l, 140)
                )),
                [] => {
                    bad.push("NOBODY TOLD: no login exception journaled while it was gone".into())
                }
                _ => bad.push(format!(
                    "TOLD {} TIMES: {}",
                    told.len(),
                    told.iter()
                        .map(|(t, l)| format!("+{} {}", at(t0, *t), one_line(l, 80)))
                        .collect::<Vec<_>>()
                        .join("; ")
                )),
            }
            if !ups.is_empty() {
                bad.push(format!(
                    "ESCALATED BESIDE THE NOTICE: {}",
                    told_said(&ups, t0)
                ));
            }
            let band: Vec<BandRow> = posted(seen, t1, gone_to)
                .into_iter()
                .filter(|r| r.title.starts_with(LOGIN_CHOOSE.title))
                .collect();
            match &band[..] {
                [r] if r.need == "now" => {
                    said.push_str(&format!("; one band row \"{}\" (need=now)", r.title));
                }
                [r] => bad.push(format!(
                    "THE LOGIN'S BAND ROW IS NO NEED NOW: \"{}\" need={}",
                    r.title, r.need
                )),
                [] if !band_read => {}
                rows => bad.push(format!(
                    "{} BAND ROWS titled \"{}\" while the login was gone",
                    rows.len(),
                    LOGIN_CHOOSE.title
                )),
            }
        }
        Harness::Agy | Harness::Muse => {
            match &ups[..] {
                [(t, why)] => match names_sign_in(seen, h, why, (t1, *t)) {
                    Some(how) => said.push_str(&format!(
                        "; one escalation at +{}, naming how it signs in (\"{how}\"): \"{}\"",
                        at(t0, *t),
                        one_line(why, 200)
                    )),
                    None => bad.push(format!(
                        "THE ESCALATION NAMES NO SIGN-IN: at +{} \"{}\"",
                        at(t0, *t),
                        one_line(why, 200)
                    )),
                },
                [] => bad.push("NOBODY TOLD: no escalation while the login was gone".into()),
                _ => bad.push(format!(
                    "ESCALATED {} TIMES: {}",
                    ups.len(),
                    told_said(&ups, t0)
                )),
            }
            let rows = posted(seen, t1, gone_to);
            if !rows.is_empty() {
                bad.push(format!(
                    "A BAND ROW BESIDE THE ESCALATION: {}",
                    rows.iter()
                        .map(|r| format!("\"{}\" need={}", r.title, r.need))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
    }
    verdict(said, &bad, &unseen)
}

/// JUDGES THE PERSON'S SIGN-IN on `program`, under Land where `land` ([`Played`]): on Claude
/// Code the person's pick handed the sign-in to the inert browser (the recorder's live control:
/// a pick it logged nothing of fails, as the same miss would have hidden one of aterm's), and from
/// the person's pick (agy's and Muse Code's sign-in, where they pick nothing) to their prompt
/// nothing of aterm's but `login-done@v1` (its Enter at `Login successful`, a real sign-in's) and
/// no login notice but the `login-cancelled` their own Esc draws; past the prompt no `/login`, no
/// login-back and no escalation again, nothing typed over the person's turn, no back-off past the
/// policy's ladder ([`past_ladder`]), no short streak past the prompt longer than the one the act
/// that met the wall was typed on ([`super::streak_kept`]), the login's notice or escalation
/// down, and the tab going on once (Cruise's own act by when it is due, [`go_on_by`]; Land's
/// landing by when it is due, [`land_by`]).
#[must_use]
pub fn judge_signed_in(seen: &Seen, land: bool, program: &str, t0: i64) -> Verdict {
    let Some(h) = Harness::of(program) else {
        return Verdict::Unobserved(format!("no harness {program}"));
    };
    let Some(played) = seen.cover.login.as_ref() else {
        return Verdict::Unobserved("the expired login was never raised".into());
    };
    let mut said = String::new();
    let mut bad = Vec::new();
    let mut unseen = Vec::new();
    if h == Harness::Claude {
        match (played.picked_ms, played.no_pick.as_deref()) {
            (Some(p), _) => {
                said.push_str(&format!("the person picked at +{}", at(t0, p)));
                let before = played.opened_before.as_deref().unwrap_or_default();
                match played.opened_after.as_deref() {
                    Some(after) if after.len() > before.len() => said.push_str(&format!(
                        "; the inert browser logged their hand-off: {}",
                        one_line(&after[before.len()..], 120)
                    )),
                    Some(_) => bad.push(format!(
                        "THE RECORDER MISSED THE PERSON'S HAND-OFF: the inert browser logged nothing in the {} past their pick, so it could not have caught one of aterm's either",
                        at(0, HANDOFF_HANG_MS)
                    )),
                    None => unseen.push("the inert browser was not read past their pick".into()),
                }
            }
            (None, Some(why)) => unseen.push(format!("the person picked nothing: {why}")),
            (None, None) => {
                return Verdict::Unobserved(format!(
                    "the person never came to the tab within +{}",
                    at(t0, seen.end_ms)
                ));
            }
        }
    }
    let Some(prompted) = played.prompt_ms else {
        return Verdict::Unobserved(format!(
            "{said}{}the person's prompt never went in{}",
            if said.is_empty() { "" } else { "; " },
            if played.prompt_refused {
                " (the composer did not take it)"
            } else {
                ""
            }
        ));
    };
    let (from, past) = match played.picked_ms {
        Some(p) => (Some(p), "THE PERSON'S PICK"),
        None => (seen.cover.outage_lifted_ms, "THE SIGN-IN"),
    };
    if let Some(from) = from {
        let acts: Vec<Act<'_>> = acts_after(seen, from)
            .into_iter()
            .filter(|a| a.at < prompted && a.rule != RULE_LOGIN_DONE)
            .collect();
        if !acts.is_empty() {
            bad.push(format!("TYPED PAST {past}: {}", acts_said(&acts, t0)));
        }
        let keys: Vec<&Row> = keys_pressed(seen, from, prompted)
            .into_iter()
            .filter(|r| r.rule != RULE_LOGIN_DONE)
            .collect();
        if !keys.is_empty() {
            bad.push(format!(
                "A KEY OF ATERM'S PAST {past}: {}",
                rows_said(&keys, t0)
            ));
        }
        let (escs, told): (Vec<_>, Vec<_>) = login_exceptions(seen, from, prompted)
            .into_iter()
            .partition(|(t, l)| {
                word(l, "notice").as_deref() == Some(LOGIN_CANCELLED.id)
                    && played.left_ms.is_some_and(|esc| *t >= esc)
            });
        if let [(t, _)] = escs[..] {
            said.push_str(&format!(
                "{}the person's Esc drew {} at +{}",
                if said.is_empty() { "" } else { "; " },
                LOGIN_CANCELLED.id,
                at(t0, *t)
            ));
        }
        if escs.len() > 1 || !told.is_empty() {
            bad.push(format!(
                "TOLD PAST {past}: {}",
                escs.iter()
                    .skip(1)
                    .chain(told.iter())
                    .map(|(t, l)| format!("+{} {}", at(t0, *t), one_line(l, 80)))
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
    }
    if !said.is_empty() {
        said.push_str("; ");
    }
    said.push_str(&format!(
        "the stand-in signed in at +{}; the person prompted at +{}",
        seen.cover
            .outage_lifted_ms
            .map_or_else(|| "?".into(), |t| at(t0, t)),
        at(t0, prompted)
    ));
    let after = acts_after(seen, prompted);
    let again: Vec<Act<'_>> = after
        .iter()
        .copied()
        .filter(|a| {
            a.rule == RULE_LOGIN
                || a.rule == RULE_LOGIN_BACK
                || a.rule == turn_end::RULE_OUTAGE_RESUME
        })
        .collect();
    if !again.is_empty() {
        bad.push(format!(
            "THE LOGIN'S ACTS AGAIN past the person's prompt: {}",
            acts_said(&again, t0)
        ));
    }
    let told: Vec<(i64, String)> = escalations(seen, prompted, i64::MAX)
        .into_iter()
        .filter(|(t, why)| names_sign_in(seen, h, why, (prompted, *t)).is_some())
        .chain(
            login_exceptions(seen, prompted, i64::MAX)
                .into_iter()
                .map(|(t, l)| (*t, l.clone())),
        )
        .collect();
    if !told.is_empty() {
        bad.push(format!(
            "TOLD AGAIN past the person's prompt: {}",
            told_said(&told, t0)
        ));
    }
    let answered = first_read(seen, prompted, i64::MAX);
    if let Some(a) = after.iter().find(|a| answered.is_none_or(|p| a.at < p)) {
        bad.push(format!(
            "TYPED OVER THE PERSON'S TURN: {} at +{}, before its answer was read",
            a.rule,
            at(t0, a.at)
        ));
    }
    if let Some(why) = past_ladder(seen, prompted, t0) {
        bad.push(why);
    }
    match met(seen) {
        Some(t1) => {
            let next = after.first().map_or(i64::MAX, |a| a.at);
            match streak_kept(seen, t1, (prompted, "the person's prompt", next), t0) {
                Ok(kept) => said.push_str(&kept),
                Err(LandOn::Fault(why)) => bad.push(why),
                Err(LandOn::Unwatched(why)) => unseen.push(why),
            }
        }
        None => unseen.push("no act met the expired login".into()),
    }
    if land {
        match land_on_by(seen, prompted, land_by(seen, prompted), t0) {
            Ok(on) => said.push_str(&on),
            Err(LandOn::Fault(why)) => bad.push(why),
            Err(LandOn::Unwatched(why)) => unseen.push(why),
        }
    } else {
        let due = go_on_by(seen, prompted, after.first().map_or(i64::MAX, |a| a.at));
        match after.first() {
            Some(a) if a.at <= due => said.push_str(&format!(
                "; Cruise went on once: {} at +{}",
                a.rule,
                at(t0, a.at)
            )),
            Some(a) => bad.push(format!(
                "CRUISE DID NOT GO ON: its first act {} only at +{}, {} past the person's prompt \
                 and {} past when it was due",
                a.rule,
                at(t0, a.at),
                at(prompted, a.at),
                at(due, a.at)
            )),
            None if seen.end_ms >= due => bad.push(format!(
                "CRUISE DID NOT GO ON: nothing typed in the {} watched past the person's prompt",
                at(prompted, seen.end_ms)
            )),
            None => unseen.push(format!(
                "watched to +{}, before Cruise's own act",
                at(t0, seen.end_ms)
            )),
        }
    }
    match h {
        Harness::Claude => {
            let live: Vec<BandRow> = seen
                .band
                .last()
                .map(|(_, r)| band_rows(r))
                .unwrap_or_default()
                .into_iter()
                .filter(|r| r.state == "live" && r.title.starts_with(LOGIN_CHOOSE.title))
                .collect();
            if let Some(r) = live.first() {
                bad.push(format!(
                    "THE LOGIN'S NOTICE STILL UP past the sign-in: band row {} \"{}\"",
                    r.id, r.title
                ));
            }
        }
        Harness::Agy | Harness::Muse => {
            match seen
                .meta
                .iter()
                .filter(|(t, _)| *t > prompted)
                .max_by_key(|(t, _)| *t)
            {
                Some((t, m)) if word(m, "attention_owners").is_some_and(|o| o != "0") => {
                    let up = m
                        .split_once(" attention=")
                        .map_or("", |(_, a)| a)
                        .to_string();
                    if names_sign_in(seen, h, &up, (prompted, *t)).is_some() {
                        bad.push(format!(
                            "THE LOGIN'S ESCALATION STILL UP past the sign-in: at +{} \"{}\"",
                            at(t0, *t),
                            one_line(&up, 160)
                        ));
                    }
                }
                Some(_) => said.push_str("; the escalation down"),
                None => unseen.push("the tab's attention was never read past the prompt".into()),
            }
        }
    }
    verdict(said, &bad, &unseen)
}
