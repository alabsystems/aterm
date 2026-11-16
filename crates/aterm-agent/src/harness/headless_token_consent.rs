// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A TOKEN'S CONSENT PAGE ON THE ACCOUNT'S OWN HEADLESS PROFILE (plan package FL-137;
//! `docs/DESIGN-claude-sign-in-sitting-2026-11-12.md` S5). After a headless sign-in binds a row,
//! the row's token is made on the same profile: `claude setup-token` hands its authorize request
//! to aterm's opener ([`super::super::open_url::TOKEN_ENV`]), which serves it here as the
//! profile's second pinned request ([`super::authorize_token_consent`]).
//!
//! - **Only a setup-token's request, only on a proven profile.** The request must pass
//!   [`super::super::setup_token::admit_url`] (an inference token, nothing wider) and
//!   [`super::super::open_url::CLAUDE_PIN`], and the account's profile must be one a sign-in
//!   proved ([`super::has_browser_profile`]): none is made here ([`TokenRequest::of`]).
//! - **The page is read, then acted on.** Each read is [`TOKEN_PAGE_JS`], which reads and sets
//!   nothing. The one act is a press of `Authorize` ([`press_js`]), which judges the page again
//!   as it presses: the document the read was made in and no other, no check in it, the
//!   account still shown, and an enabled button whose whole label is `authorize`. A press that
//!   finds the page moved or the account not shown presses nothing; one that finds a check
//!   ends the drive on it; one that pressed is never made again ([`drive_until`]).
//! - **Authorize only where the page shows the account.** It is pressed only where the page
//!   names the account's exact email as the one signed in ([`super::shows_email`]). A page that
//!   names another account ends the drive naming it ([`MetWord::ConsentWrongAccount`]); a page
//!   that names none, or asks for an email, a code or a password, is pressed and filled nothing,
//!   and once it has stood for [`PROOF_STALL`] ends the drive naming it
//!   ([`MetWord::ConsentUnproven`]).
//! - **No callback without the press.** Before the tab is sent anywhere, the browser is made to
//!   pause every request to the waiting `setup-token`'s loopback listener, and each is failed in
//!   the browser: the callback reaches the listener only as aterm hands it over. A callback the
//!   page reaches before the one press, by a request, in its address or in an event (a grant the
//!   profile already held, which skips the consent page), is handed over nowhere, and ends the
//!   drive as a page that did not show the account ([`MetWord::ConsentUnproven`]).
//! - **A check ends the drive on its first read.** A page that carries one ([`check_met`]: a
//!   block page, Cloudflare's bot check, or a human check's widget anywhere in it, its open
//!   shadow roots and same-origin frames included, as the shared predicate reads them) ends the
//!   drive at once, naming it, with nothing on it pressed, filled or followed, whether its
//!   `Authorize` stands disabled or not: an invisible check runs on the press of the button it
//!   is bound to, and nobody is at this profile to answer one. The drive does not wait for a
//!   check to clear, since one that cleared by itself would let software past it, and a press
//!   that meets a check at the press ends it the same way ([`Pressed::Check`]).
//! - **The callback.** A loopback callback is handed to the waiting `setup-token`'s listener, and
//!   a hosted one to its code file (`headless_oauth::deliver_claude_callback`): nothing is typed
//!   into its terminal.

use super::super::auth_deadline::Deadline;
use super::super::open_url::{CLAUDE_PIN, MailboxFlow, MetWord};
use super::{
    CHECK_PAGE_JS, CdpClient, CheckMet, Field, Gate, Page, SIGNED_IN_WORDS, TOKEN_CONSENT_UNPROVEN,
    TOKEN_HUMAN_CHECK, another_account, browser_control_failed, callback_delivery_result,
    check_met, close_flow_page, navigate_flow_tab, note_met, open_flow_tab, page_of, shows_email,
    sign_in_check_words, sign_in_out_of_time, take_met,
};
use aterm_json::{Value, json};
use std::path::Path;
use std::time::{Duration, Instant};

/// How long a consent page that does not show the account as the one signed in may stand
/// before the drive ends naming it: long enough for a page that names its account a moment
/// after it draws, and well inside the opener's own deadline.
#[cfg_attr(
    not(target_os = "macos"),
    allow(
        dead_code,
        reason = "only macOS's opener serves a token's consent page"
    )
)]
pub(in crate::harness) const PROOF_STALL: Duration = Duration::from_secs(15);

/// The refusal of a consent page that names another account than the token's, as the headless
/// sign-in's drive refuses one.
pub(in crate::harness) const WRONG_ACCOUNT: &str =
    "headless browser profile is signed in to a different account";

/// A token's authorize request that the opener serves on the account's headless profile:
/// admitted by [`Self::of`] alone.
pub(in crate::harness) struct TokenRequest<'a> {
    url: &'a str,
    flow: MailboxFlow,
    /// The account's email, as the capture named it.
    pub(in crate::harness) email: String,
}

impl<'a> TokenRequest<'a> {
    /// The request `url` names, for the account `email` whose proven profile is in the accounts
    /// store `store_dir` (found by `email`, else by `label`).
    ///
    /// # Errors
    /// `url` is not a setup-token's request under Claude's pin, no account or store is named,
    /// or the account has no profile a sign-in proved.
    pub(in crate::harness) fn of(
        url: &'a str,
        store_dir: Option<&Path>,
        label: Option<&str>,
        email: Option<&str>,
    ) -> Result<Self, &'static str> {
        use super::super::setup_token::{Admit, admit_url};
        if !matches!(admit_url(url.as_bytes()), Admit::Admitted(_)) {
            return Err("the token's authorize request is not one Claude Code's setup-token makes");
        }
        let flow = MailboxFlow::pinned(url, CLAUDE_PIN)?;
        let email = email
            .map(str::trim)
            .filter(|email| !email.is_empty())
            .ok_or("no account is named for the token's consent page")?;
        let store = store_dir.ok_or("no accounts store holds the account's headless profile")?;
        if !super::has_browser_profile(store, label.unwrap_or("default"), Some(email)) {
            return Err(
                "the account has no headless browser profile a sign-in proved, so its token's \
                 consent page is served on none",
            );
        }
        Ok(Self {
            url,
            flow,
            email: email.to_owned(),
        })
    }
}

/// Reads a token's consent page: [`super::VENDOR_PAGE_JS`]'s [`Page`], the check it carries
/// (`checkPageOf()`, the one predicate every drive reads) included. It sets and presses nothing.
pub(in crate::harness) const TOKEN_PAGE_JS: &str = super::VENDOR_PAGE_JS;

/// What the drive does on one read of a token's consent page ([`consent_step`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::harness) enum Consent {
    /// Read it again.
    Wait,
    /// Press `Authorize` ([`press_js`]).
    Press,
    /// The page names another account as the one signed in.
    WrongAccount,
    /// The page does not show the account as the one signed in, or asks for a sign-in.
    Unproven,
    /// The page carries a check ([`check_met`]): the drive ends on it.
    Check(CheckMet),
}

fn is_authorize(label: &str) -> bool {
    label.trim() == "authorize"
}

/// The step on `page` for the account `gate` names, `spent` saying whether the one press was
/// made already: a check the page carries ([`check_met`]) ends the drive, before the press or
/// after it, with nothing pressed; another account named refuses; once the press was made, the
/// drive only reads; an enabled `Authorize` is pressed only where the page shows the account
/// ([`shows_email`]); a page that asks for an email, a code or a password is no consent of the
/// account's.
#[must_use]
pub(in crate::harness) fn consent_step(page: &Page, gate: &Gate<'_>, spent: bool) -> Consent {
    if let Some(check) = check_met(page) {
        return Consent::Check(check);
    }
    if another_account(page, gate) {
        return Consent::WrongAccount;
    }
    if spent {
        return Consent::Wait;
    }
    if page.buttons.iter().any(|label| is_authorize(label)) {
        return if shows_email(page, gate) {
            Consent::Press
        } else {
            Consent::Unproven
        };
    }
    if page
        .inputs
        .iter()
        .any(|field| matches!(field, Field::Email | Field::Code | Field::Password))
    {
        return Consent::Unproven;
    }
    Consent::Wait
}

/// The gate [`shows_email`] and [`another_account`] judge a consent page by: the account's
/// `email`, with Anthropic's own addresses never taken for another account's.
#[must_use]
pub(in crate::harness) fn consent_gate(email: &str) -> Gate<'_> {
    Gate {
        hosts: &["claude.ai", "claude.com"],
        completion: crate::ai::connector::Completion::Loopback,
        code: None,
        email: Some(email),
        senders: SENDERS,
        otp: None,
        pressed: 0,
        confirmed: false,
        filled_code: false,
        filled_email: false,
        filled_otp: false,
        bound: None,
        sign_in_once: None,
        types_email: false,
    }
}

/// The vendor whose own addresses a consent page may show beside the account's
/// ([`consent_gate`], [`press_js`]).
const SENDERS: &[&str] = &["anthropic"];

/// What [`press_js`] answers where it pressed `Authorize`.
const PRESSED: &str = "pressed";

/// Presses `Authorize` on the consent page read at `at`, for the account `email`, judging the
/// page again as [`consent_step`] judged it: only in the document at `at`, and in no other; only
/// while it carries no check (`checkPageOf()`, the shared predicate); only while its text names
/// `email` beside its signed-in words and names no other address but the vendor's
/// ([`shows_email`]); and only the first enabled, shown button whose whole label is
/// `authorize`. It answers what it did and the address of the document it ran in: [`PRESSED`],
/// or, with nothing pressed, `moved`, `check` (with the check's word as `check`), `unshown`
/// (the account is not shown) or `absent` (no such button). `None` where `at` or `email` is no
/// string a script can hold.
#[must_use]
pub(in crate::harness) fn press_js(at: &str, email: &str) -> Option<String> {
    let literal = |value: &str| aterm_json::to_string(&Value::String(value.to_owned())).ok();
    let list = |words: &[&str]| {
        aterm_json::to_string(&Value::Array(
            words
                .iter()
                .map(|word| Value::String((*word).to_owned()))
                .collect(),
        ))
        .ok()
    };
    Some(format!(
        r#"(() => {{
  const here = String(window.location && window.location.href || "");
  const answer = (did) => ({{ at: here, did }});
  if (here !== {at}) return answer("moved");
{check}
  const met = checkPageOf();
  if (met) return {{ at: here, did: "check", check: met }};
  const text = String(document.body && document.body.innerText || "");
  const lower = text.toLowerCase();
  const email = {email}, signedIn = {signed_in}, senders = {senders};
  const emails = lower.split(/[^a-z0-9@._%+-]+/).map(w => w.replace(/^[^a-z0-9]+|[^a-z0-9]+$/g, "")).filter(w => {{
    const parts = w.split("@");
    return parts.length === 2 && parts[0].length > 0 && parts[1].split(".").length >= 2 && parts[1].split(".").every(p => p.length > 0);
  }});
  const vendor = (e) => senders.some(s => e.split("@")[1].split(".").includes(s));
  if (!signedIn.some(w => lower.includes(w)) || !emails.includes(email) || emails.some(e => e !== email && !vendor(e))) return answer("unshown");
  const usable = (el) => !el.disabled && !el.hidden && el.type !== "hidden" && el.getAttribute("aria-disabled") !== "true" && el.getClientRects().length > 0;
  const label = (b) => String(b.innerText || b.value || b.getAttribute("aria-label") || "").trim().toLowerCase().slice(0, 80);
  const hit = Array.from(document.querySelectorAll("button, input[type='submit'], [role='button']")).filter(usable).find(b => label(b) === "authorize");
  if (!hit) return answer("absent");
  hit.click();
  return answer("{pressed}");
}})()"#,
        at = literal(at)?,
        check = CHECK_PAGE_JS,
        email = literal(&email.trim().to_ascii_lowercase())?,
        signed_in = list(SIGNED_IN_WORDS)?,
        senders = list(SENDERS)?,
        pressed = PRESSED,
    ))
}

/// What one press of `Authorize` ([`press_js`]) came to.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Pressed {
    /// It pressed, on the page the read judged: the one proof a callback is handed over on.
    Yes,
    /// It answered that it pressed nothing (the page moved, the account is no longer shown, no
    /// `Authorize` stands).
    No,
    /// It pressed nothing: the page carries this check, read at the press. The drive ends on it,
    /// and presses nothing again.
    Check(CheckMet),
    /// No answer came: whether it pressed is not known, so no press is made again, and no
    /// callback is handed over on it.
    Unanswered,
}

/// Presses `Authorize` on the tab `session`'s page, read at `at`, for `email`, within `within`.
fn press(
    cdp: &mut CdpClient,
    session: &str,
    (at, email): (&str, &str),
    within: Duration,
) -> Pressed {
    let Some(script) = press_js(at, email) else {
        return Pressed::No;
    };
    match cdp.call(Some(session), "Runtime.evaluate", evaluate(&script), within) {
        Ok(answer) => match answer.pointer("/result/value/did").and_then(Value::as_str) {
            Some(PRESSED) => Pressed::Yes,
            // A check met at the press is one whatever its word: an unknown word is a human
            // check by the general name, so it still ends the drive.
            Some("check") => Pressed::Check(
                answer
                    .pointer("/result/value/check")
                    .and_then(CheckMet::of_value)
                    .unwrap_or_else(|| CheckMet::HumanCheck("captcha".into())),
            ),
            _ => Pressed::No,
        },
        Err(_) => Pressed::Unanswered,
    }
}

/// A `Runtime.evaluate` of `expression`, answered by value.
fn evaluate(expression: &str) -> Value {
    json!({"expression": expression, "awaitPromise": true, "returnByValue": true})
}

/// Has the browser pause every request of the tab `session` to the waiting `setup-token`'s
/// loopback listener (on `flow`'s port, under either of its loopback names) before the tab is
/// sent anywhere ([`paused_callback`] fails each), so the callback reaches the listener only as
/// aterm hands it over, after the one press. A hosted flow's callback is a page of Claude's that
/// hands nothing over by itself, and needs none.
///
/// # Errors
/// The browser did not take it: the drive does not go on without it.
fn hold_listener(
    cdp: &mut CdpClient,
    session: &str,
    flow: &MailboxFlow,
    deadline: Deadline<'_>,
) -> Result<(), &'static str> {
    let Some(port) = flow.loopback_port() else {
        return Ok(());
    };
    cdp.call(
        Some(session),
        "Fetch.enable",
        json!({"patterns": [
            {"urlPattern": format!("http://localhost:{port}/*"), "requestStage": "Request"},
            {"urlPattern": format!("http://127.0.0.1:{port}/*"), "requestStage": "Request"},
        ]}),
        deadline.left()?,
    )
    .map(drop)
    .map_err(browser_control_failed)
}

/// Fails, in the browser, every request of the tab `session` it paused on its way to the
/// listener ([`hold_listener`]), whatever it was, and answers the first that was `flow`'s
/// callback: none reaches the listener by the browser. A fail that does not go through leaves
/// the request paused, where it reaches nothing either.
fn paused_callback(
    cdp: &mut CdpClient,
    session: &str,
    flow: &MailboxFlow,
    deadline: Deadline<'_>,
) -> Option<String> {
    let mut callback = None;
    loop {
        let paused = cdp.take_events("Fetch.requestPaused", Some(session));
        if paused.is_empty() {
            return callback;
        }
        for event in paused {
            let url = event
                .pointer("/params/request/url")
                .and_then(Value::as_str)
                .unwrap_or("");
            if callback.is_none() && flow.is_callback(url) {
                callback = Some(url.to_owned());
            }
            if let (Some(id), Ok(within)) = (
                event.pointer("/params/requestId").and_then(Value::as_str),
                deadline.left(),
            ) {
                let _ = cdp.call(
                    Some(session),
                    "Fetch.failRequest",
                    json!({"requestId": id, "errorReason": "BlockedByClient"}),
                    within,
                );
            }
        }
    }
}

/// How a drive whose deadline came ends, noted for [`take_met`]: on the unproven page that stood
/// at its last read (`unproven`), else as the headless sign-in's drive ends
/// ([`sign_in_out_of_time`]). A check is never waited on, so none stands at the deadline.
fn out_of_time(unproven: bool, last_host: Option<&str>) -> &'static str {
    if unproven {
        note_met(MetWord::ConsentUnproven, last_host);
        return TOKEN_CONSENT_UNPROVEN;
    }
    sign_in_out_of_time(last_host)
}

/// Ends the drive on `check`, met on `last_host`, noted for [`take_met`]: nothing on its page
/// was pressed, and nothing is pressed again.
fn met_check(check: &CheckMet, last_host: Option<&str>) -> &'static str {
    note_met(check.word(), last_host);
    match check {
        CheckMet::HumanCheck(_) => TOKEN_HUMAN_CHECK,
        other => sign_in_check_words(other),
    }
}

/// Ends the drive on a callback that came with no press of `Authorize` behind it: nothing is
/// handed over, and the page is one that did not show the account.
fn unpressed(last_host: Option<&str>) -> &'static str {
    note_met(MetWord::ConsentUnproven, last_host);
    TOKEN_CONSENT_UNPROVEN
}

/// Drives `request`'s consent page in the browser `cdp` drives (the module's summary says how),
/// reading every `poll` until `deadline`; a page that does not show the account may stand for
/// `proof_stall`, and a check ends the drive on its first read. `deliver` hands a callback to
/// the waiting `setup-token`. What it met is noted for [`take_met`].
///
/// # Errors
/// The page met what aterm never passes, did not show the account, named another, reached its
/// callback with nothing pressed, left Claude's pinned hosts, reached no callback in time, or
/// the browser could not be driven.
pub(in crate::harness) fn drive_until(
    cdp: &mut CdpClient,
    request: &TokenRequest<'_>,
    (deadline, poll): (Deadline<'_>, Duration),
    proof_stall: Duration,
    deliver: impl FnMut(&str) -> Result<u16, &'static str>,
) -> Result<(), &'static str> {
    let _ = take_met();
    let (target_id, session) = open_flow_tab(cdp, deadline)?;
    let served = hold_listener(cdp, &session, &request.flow, deadline)
        .and_then(|()| navigate_flow_tab(cdp, &session, request.url, deadline))
        .and_then(|()| {
            serve_page(
                cdp,
                &session,
                request,
                (deadline, poll),
                proof_stall,
                deliver,
            )
        });
    close_flow_page(cdp, &target_id);
    served
}

/// [`drive_until`]'s reads and its one act, on the tab `session` of an open page.
fn serve_page(
    cdp: &mut CdpClient,
    session: &str,
    request: &TokenRequest<'_>,
    (deadline, poll): (Deadline<'_>, Duration),
    proof_stall: Duration,
    mut deliver: impl FnMut(&str) -> Result<u16, &'static str>,
) -> Result<(), &'static str> {
    let gate = consent_gate(&request.email);
    let mut unproven: Option<Instant> = None;
    // `pressed`: the press answered that it pressed `Authorize` on the page the read judged, the
    // one proof a callback is handed over on; `unanswered`: a press whose answer never came, after
    // which none is made again.
    let (mut pressed, mut unanswered) = (false, false);
    let mut last_host: Option<String> = None;
    let pause = || {
        let _ = deadline.pause(poll);
    };
    loop {
        let Ok(within) = deadline.remaining() else {
            return Err(out_of_time(unproven.is_some(), last_host.as_deref()));
        };
        let read = match cdp.call(
            Some(session),
            "Runtime.evaluate",
            evaluate(TOKEN_PAGE_JS),
            within,
        ) {
            Ok(read) => read,
            Err(why) => {
                // A read the deadline cut short ends the drive as the deadline does, naming
                // what stood.
                if deadline.expired() {
                    return Err(out_of_time(unproven.is_some(), last_host.as_deref()));
                }
                // A read the page replaced under it (a navigation), or one made while an
                // unproven page stood, is read again; any other failure ends the drive.
                if why != "browser execution context changed" && unproven.is_none() {
                    return Err(browser_control_failed(why));
                }
                pause();
                continue;
            }
        };
        let value = read
            .pointer("/result/value")
            .cloned()
            .unwrap_or(Value::Null);
        let seen = page_of(&value);
        let url = seen.url.as_str();
        // A callback, by a request the browser paused on its way to the listener, by the page's
        // own address or by an event, is handed over only after the one press, and only by
        // aterm: the listener had none of it from the browser ([`hold_listener`]).
        let callback = match paused_callback(cdp, session, &request.flow, deadline) {
            Some(callback) => Some(callback),
            None if request.flow.is_callback(url) => Some(url.to_owned()),
            None => cdp.take_matching_event_url(session, |u| request.flow.is_callback(u)),
        };
        if let Some(callback) = callback {
            if !pressed {
                return Err(unpressed(last_host.as_deref()));
            }
            return callback_delivery_result(deliver(&callback));
        }
        if let Some(host) = super::super::login_drive::https_host(url) {
            if !request.flow.admits_provider_url(url) {
                return Err(
                    "the token's consent page left Claude's pinned hosts, and nothing was pressed",
                );
            }
            last_host = Some(host);
        }
        let now = deadline.now();
        match consent_step(&seen, &gate, pressed || unanswered) {
            // A check ends the drive on its first read: nothing on its page is pressed, and the
            // drive does not wait for it to clear.
            Consent::Check(check) => return Err(met_check(&check, last_host.as_deref())),
            Consent::Press => {
                unproven = None;
                match deadline.left() {
                    Ok(within) => match press(cdp, session, (url, &request.email), within) {
                        Pressed::Yes => pressed = true,
                        Pressed::No => {}
                        Pressed::Check(check) => {
                            return Err(met_check(&check, last_host.as_deref()));
                        }
                        Pressed::Unanswered => unanswered = true,
                    },
                    Err(_) => continue,
                }
            }
            Consent::WrongAccount => {
                note_met(MetWord::ConsentWrongAccount, last_host.as_deref());
                return Err(WRONG_ACCOUNT);
            }
            Consent::Unproven => {
                let since = *unproven.get_or_insert(now);
                if now.saturating_duration_since(since) >= proof_stall {
                    note_met(MetWord::ConsentUnproven, last_host.as_deref());
                    return Err(TOKEN_CONSENT_UNPROVEN);
                }
            }
            Consent::Wait => unproven = None,
        }
        pause();
    }
}

#[cfg(all(test, unix))]
#[path = "headless_token_consent_tests.rs"]
pub(in crate::harness) mod tests;
