// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A token's consent page on the account's headless profile (plan package FL-137), driven over a
//! stand-in browser's CDP pipes ([`consent_browser`]): what is pressed on each page it can show,
//! what each stop leaves for the capture, and which requests and profiles are served at all.
//! [`serve_on_fixture`] is the opener's token branch on the stand-in browser, which the
//! setup-token stand-in runs (`setup_token::tests::stand_in`).

use super::*;
use crate::harness::open_url::Met;
use std::io::{Read as _, Write as _};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// The account the pages are for.
const EMAIL: &str = "person@example.com";

/// Another account a page may name.
const OTHER: &str = "other@example.com";

/// How long a check, the human check or a page that does not show the account stands in a test.
pub(in crate::harness) const STALL: Duration = Duration::from_millis(200);

/// What 2.1.318's setup-token asks its opener to open, its loopback listener on `port` (every
/// value made up).
pub(in crate::harness) fn token_url(port: u16) -> String {
    format!(
        "https://claude.com/cai/oauth/authorize?code=true&client_id=9d1c250a-e61b-44d9-88ed-\
         5944d1962f5e&response_type=code&redirect_uri=http%3A%2F%2Flocalhost%3A{port}%2F\
         callback&scope=user%3Ainference&code_challenge=STANDINCHALLENGE&\
         code_challenge_method=S256&state=STANDINSTATE"
    )
}

/// The callback [`token_url`]'s flow ends at.
pub(in crate::harness) fn token_callback(port: u16) -> String {
    format!("http://localhost:{port}/callback?code=test-code&state=STANDINSTATE")
}

/// What the stand-in browser's page shows before any act.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::harness) enum Shows {
    /// The consent page, naming the account as the one signed in.
    Account,
    /// The consent page, naming no account.
    Nobody,
    /// The consent page, naming another account as the one signed in.
    Another,
    /// The consent page naming the account, its `Authorize` disabled beside an hCaptcha.
    HumanCheck,
    /// The consent page naming the account, its `Authorize` disabled beside a Turnstile embedded
    /// in it: Cloudflare's marker with none of its challenge words, so no bot check.
    Turnstile,
    /// Cloudflare's check, which never clears.
    BotCheck,
    /// No consent page: the page is at its callback already, as for a grant the profile already
    /// holds ([`serve_on_fixture`]'s `callback`).
    Callback,
}

impl Shows {
    /// Whether the page stands on a human check, which only the drive's deadline ends.
    const fn checked(self) -> bool {
        matches!(self, Self::HumanCheck | Self::Turnstile)
    }
}

/// How many reads a test's drive of a page with a human check makes before the test ends its
/// deadline, as a quit cancels one ([`Deadline::cancelled_by`]): only the deadline ends a human
/// check, and this ends it on a count of reads rather than a clock.
pub(in crate::harness) const HUMAN_READS: usize = 3;

fn strings(list: &[&str]) -> Value {
    Value::Array(
        list.iter()
            .map(|item| Value::String((*item).to_owned()))
            .collect(),
    )
}

/// The page at `url` that shows `shows` for `email`, as [`TOKEN_PAGE_JS`] answers it, its
/// callback (for [`Shows::Callback`]) being `callback`.
pub(in crate::harness) fn consent_page(
    shows: Shows,
    url: &str,
    email: &str,
    callback: &str,
) -> Value {
    let asks = "Claude Code would like to connect to your Claude account.\nIt will use Claude on \
                your behalf.";
    let consent = |text: String, buttons: &[&str], markers: &[&str], check: bool| {
        json!({
            "url": url,
            "heading": "claude code would like to connect to your claude account",
            "text": text,
            "buttons": strings(buttons),
            "inputs": strings(&[]),
            "title": "authorize | claude",
            "markers": strings(markers),
            "humanCheck": check,
        })
    };
    let enabled = ["authorize", "cancel"];
    let named = format!("{asks}\nLogged in as {email}\nAuthorize\nCancel");
    match shows {
        Shows::Account => consent(named, &enabled, &[], false),
        Shows::Nobody => consent(format!("{asks}\nAuthorize\nCancel"), &enabled, &[], false),
        Shows::Another => consent(
            format!("{asks}\nLogged in as {OTHER}\nAuthorize\nCancel"),
            &enabled,
            &[],
            false,
        ),
        Shows::HumanCheck => consent(named, &["cancel"], &[], true),
        Shows::Turnstile => consent(named, &["cancel"], &["cf-turnstile"], true),
        Shows::BotCheck => json!({
            "url": url,
            "heading": "",
            "text": "Verify you are human by completing the action below.",
            "buttons": strings(&[]),
            "inputs": strings(&[]),
            "title": "just a moment...",
            "markers": strings(&["cf-turnstile", "challenge-platform"]),
            "humanCheck": false,
        }),
        Shows::Callback => json!({"url": callback}),
    }
}

/// What the stand-in browser's page answers ([`consent_browser`]).
pub(in crate::harness) struct Stand {
    /// Each read's answer before any act.
    pub(in crate::harness) before: Value,
    /// Each read's answer once an act came.
    pub(in crate::harness) after: Value,
    /// What an act answers: by default that it pressed, in the document `before` is.
    pub(in crate::harness) press: Value,
    /// A request the browser pauses on its way to the listener once the tab is sent to its
    /// page, before any act (a page that sends itself to its callback): its URL.
    pub(in crate::harness) paused: Option<String>,
    /// The read from which on (counting from 1) each read answers with a protocol error.
    pub(in crate::harness) fails_from: Option<usize>,
    /// How many reads it was asked for so far.
    pub(in crate::harness) asked: Arc<AtomicUsize>,
}

impl Stand {
    /// A page that answers `before` until an act and `after` from then on, whose press
    /// presses.
    pub(in crate::harness) fn new(before: Value, after: Value) -> Self {
        let at = before
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        Self {
            before,
            after,
            press: json!({"at": at, "did": "pressed"}),
            paused: None,
            fails_from: None,
            asked: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// The cancellation ([`Deadline::cancelled_by`]) that ends a test's deadline once the page
    /// was asked for `ends_after` reads, where that names a count, as a quit would end it.
    fn ends(&self, ends_after: Option<usize>) -> impl Fn() -> bool + use<> {
        let asked = Arc::clone(&self.asked);
        move || ends_after.is_some_and(|reads| asked.load(Ordering::SeqCst) >= reads)
    }
}

/// A stand-in browser over the CDP pipes whose page answers as `stand` says. Its log names each
/// method it was sent, an evaluation of [`TOKEN_PAGE_JS`] as `read` and any other as `act`.
pub(in crate::harness) fn consent_browser(
    stand: Stand,
) -> (CdpClient, std::thread::JoinHandle<Vec<String>>) {
    use super::super::split_nul_messages;
    let (to_browser_r, to_browser_w) = aterm_uds::pipe().expect("pipe");
    let (from_browser_r, mut from_browser_w) = aterm_uds::pipe().expect("pipe");
    let browser = std::thread::spawn(move || {
        let mut reader = to_browser_r;
        let mut pending = Vec::new();
        let mut buf = [0u8; 8192];
        let mut log = Vec::new();
        let mut acted = false;
        let mut send = |value: Value| {
            let mut bytes = aterm_json::to_vec(&value).expect("a message");
            bytes.push(0);
            from_browser_w.write_all(&bytes).is_ok()
        };
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            pending.extend_from_slice(&buf[..n]);
            for msg in split_nul_messages(&mut pending) {
                let id = msg.get("id").and_then(Value::as_u64).unwrap_or(0);
                let method = msg
                    .get("method")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let mut failed = false;
                let result = match method.as_str() {
                    "Target.createTarget" => json!({"targetId": "t1"}),
                    "Target.attachToTarget" => json!({"sessionId": "s1"}),
                    "Page.navigate" => {
                        if let Some(url) = &stand.paused {
                            let paused = json!({
                                "method": "Fetch.requestPaused",
                                "sessionId": "s1",
                                "params": {"requestId": "r1", "request": {"url": url}},
                            });
                            if !send(paused) {
                                return log;
                            }
                        }
                        json!({})
                    }
                    "Runtime.evaluate" => {
                        let read = msg.pointer("/params/expression").and_then(Value::as_str)
                            == Some(TOKEN_PAGE_JS);
                        log.push(if read { "read" } else { "act" }.to_owned());
                        if read {
                            let asked = stand.asked.fetch_add(1, Ordering::SeqCst) + 1;
                            failed = stand.fails_from.is_some_and(|from| asked >= from);
                        }
                        acted |= !read;
                        let value = if !read {
                            stand.press.clone()
                        } else if acted {
                            stand.after.clone()
                        } else {
                            stand.before.clone()
                        };
                        json!({"result": {"type": "object", "value": value}})
                    }
                    _ => json!({}),
                };
                if method != "Runtime.evaluate" {
                    log.push(method.clone());
                }
                let answer = if failed {
                    json!({"id": id, "error": {"code": -32000, "message": "Internal error"}})
                } else {
                    json!({"id": id, "result": result})
                };
                if !send(answer) || method == "Browser.close" {
                    return log;
                }
            }
        }
        log
    });
    (CdpClient::new(to_browser_w, from_browser_r), browser)
}

/// THE OPENER'S TOKEN BRANCH ON THE STAND-IN BROWSER: `url`'s request admitted as the opener
/// admits it ([`TokenRequest::of`], for `email`'s profile in `store` or `label`'s), its page
/// showing `shows`, and its callback (`callback`, the page an act leads to) handed to the
/// waiting listener as the opener hands it (`deliver_claude_callback`): what the drive ended
/// with, and the browser's log (empty where the request was not served). A page with a human
/// check is read [`HUMAN_READS`] times, and its deadline then ends it.
pub(in crate::harness) fn serve_on_fixture(
    url: &str,
    (store, label, email): (Option<&Path>, Option<&str>, Option<&str>),
    shows: Shows,
    callback: &str,
) -> (Result<(), &'static str>, Vec<String>) {
    let _ = take_met();
    let request = match TokenRequest::of(url, store, label, email) {
        Ok(request) => request,
        Err(why) => return (Err(why), Vec::new()),
    };
    let stand = Stand::new(
        consent_page(shows, url, &request.email, callback),
        json!({"url": callback}),
    );
    let ends = stand.ends(shows.checked().then_some(HUMAN_READS));
    let (mut cdp, browser) = consent_browser(stand);
    let clock = Instant::now;
    let deadline = Deadline::new(aterm_spec::wait::MUST_HAPPEN, &clock).cancelled_by(&ends);
    let served = super::super::finish_deadline(
        drive_until(
            &mut cdp,
            &request,
            (deadline, Duration::from_millis(5)),
            (STALL, STALL),
            |cb| super::super::deliver_claude_callback(cb, deadline),
        ),
        deadline,
    );
    drop(cdp);
    (served, browser.join().expect("the stand-in browser"))
}

/// An accounts store holding `email`'s proven headless profile.
fn store_with_profile(email: &str) -> aterm_tempfile::TempDir {
    let store = aterm_tempfile::Builder::new()
        .prefix("aterm-token-consent-")
        .suffix(".noindex")
        .tempdir()
        .expect("a store");
    let profile = super::super::resolve_browser_profile_dir(store.path(), None, Some(email));
    std::fs::create_dir_all(&profile).expect("the proven profile");
    store
}

/// What one drive did: its end, what it met, its browser's log and the callbacks it delivered.
type Drove = (
    Result<(), &'static str>,
    Option<Met>,
    Vec<String>,
    Vec<String>,
);

/// One drive of the stand-in page `stand`, whose deadline the hang detector's, or `ends_after`
/// reads asked for, ends; a page that does not show the account may stand for `proof_stall`.
fn drive_on(stand: Stand, ends_after: Option<usize>, proof_stall: Duration) -> Drove {
    let store = store_with_profile(EMAIL);
    let url = token_url(54545);
    let request =
        TokenRequest::of(&url, Some(store.path()), Some("work"), Some(EMAIL)).expect("served");
    let ends = stand.ends(ends_after);
    let (mut cdp, browser) = consent_browser(stand);
    let mut delivered = Vec::new();
    let clock = Instant::now;
    let deadline = Deadline::new(aterm_spec::wait::MUST_HAPPEN, &clock).cancelled_by(&ends);
    let ended = drive_until(
        &mut cdp,
        &request,
        (deadline, Duration::from_millis(5)),
        (STALL, proof_stall),
        |cb| {
            delivered.push(cb.to_owned());
            Ok(200)
        },
    );
    let met = take_met();
    drop(cdp);
    (
        ended,
        met,
        browser.join().expect("the stand-in browser"),
        delivered,
    )
}

/// One drive of a page that shows `shows`, whose act leads to its callback: a page with a human
/// check for [`HUMAN_READS`] reads, then its deadline ends.
fn drive(shows: Shows) -> Drove {
    let url = token_url(54545);
    let callback = token_callback(54545);
    drive_on(
        Stand::new(
            consent_page(shows, &url, EMAIL, &callback),
            json!({"url": callback}),
        ),
        shows.checked().then_some(HUMAN_READS),
        STALL,
    )
}

/// AUTHORIZE IS PRESSED ONLY WHERE THE PAGE SHOWS THE ACCOUNT: a consent page that names the
/// account as the one signed in is pressed once and its callback delivered; one that names
/// nobody is pressed nothing and ends naming that, on its host; one that names another account
/// presses nothing and ends naming that, a stop that holds the step; one that carries a human
/// check's widget (an hCaptcha, or a Turnstile embedded in it that is no bot check) is waited on
/// until the drive's deadline, which names it, and Cloudflare's check is waited on for its
/// stall, with nothing but the page's read ever evaluated there and no input sent. A page that
/// asks for a sign-in is no consent of the account's, and a check after the press is still a
/// check.
/// NEGATIVE CONTROL: with `consent_step` pressing an enabled `Authorize` whatever `shows_email`
/// says, the page that names nobody is pressed: `` Nobody: an act was sent ``.
#[test]
fn authorize_is_pressed_only_where_the_page_shows_the_account() {
    let (ended, met, log, delivered) = drive(Shows::Account);
    assert_eq!(ended, Ok(()));
    assert_eq!(met, None);
    assert_eq!(log.iter().filter(|m| *m == "act").count(), 1, "{log:?}");
    assert_eq!(delivered, [token_callback(54545)]);
    assert!(log.ends_with(&["Target.closeTarget".to_owned(), "Browser.close".to_owned()]));

    for (shows, end, word) in [
        (
            Shows::Nobody,
            TOKEN_CONSENT_UNPROVEN,
            MetWord::ConsentUnproven,
        ),
        (Shows::Another, WRONG_ACCOUNT, MetWord::ConsentWrongAccount),
        (Shows::HumanCheck, TOKEN_HUMAN_CHECK, MetWord::HumanCheck),
        (Shows::Turnstile, TOKEN_HUMAN_CHECK, MetWord::HumanCheck),
        (Shows::BotCheck, BOT_CHECK_UNCLEARED, MetWord::BotCheck),
    ] {
        let (ended, met, log, delivered) = drive(shows);
        assert!(
            !log.iter().any(|m| m == "act"),
            "{shows:?}: an act was sent: {log:?}"
        );
        assert_eq!(ended, Err(end), "{shows:?}");
        let met = met.unwrap_or_else(|| panic!("{shows:?}: nothing met was noted"));
        assert_eq!(met.word, word, "{shows:?}");
        assert!(word.holds_a_token_step(), "{shows:?}");
        assert_eq!(met.host.as_deref(), Some("claude.com"), "{shows:?}");
        assert!(
            !log.iter().any(|m| m.starts_with("Input.")),
            "{shows:?}: an input was sent: {log:?}"
        );
        assert!(log.iter().any(|m| m == "read"), "{shows:?}: {log:?}");
        assert!(delivered.is_empty(), "{shows:?}");
    }

    // A sign-in page on the profile is no consent: nothing is typed into it.
    let sign_in = ConsentPage {
        page: Page {
            url: token_url(54545),
            buttons: vec!["continue with email".to_owned()],
            inputs: vec![Field::Email],
            ..Page::default()
        },
        ..ConsentPage::default()
    };
    assert_eq!(
        consent_step(&sign_in, &consent_gate(EMAIL), false),
        Consent::Unproven
    );
    let page = |shows: Shows| {
        consent_page_of(&consent_page(
            shows,
            &token_url(54545),
            EMAIL,
            &token_callback(54545),
        ))
    };
    // Once pressed, nothing is pressed again; a human check that comes after the press is still
    // waited on as one, and one beside an enabled `Authorize` is too.
    let named = page(Shows::Account);
    assert_eq!(
        consent_step(&named, &consent_gate(EMAIL), false),
        Consent::Press
    );
    assert_eq!(
        consent_step(&named, &consent_gate(EMAIL), true),
        Consent::Wait
    );
    let checked = ConsentPage {
        human_check: true,
        ..named
    };
    for spent in [false, true] {
        assert_eq!(
            consent_step(&checked, &consent_gate(EMAIL), spent),
            Consent::HumanCheck,
            "{spent}"
        );
    }
    assert_eq!(
        consent_step(&page(Shows::Turnstile), &consent_gate(EMAIL), false),
        Consent::HumanCheck
    );
}

/// THE READ AND THE PRESS: the read sets and presses nothing, and knows each human check's
/// widget aterm's sign-in window knows (hCaptcha, reCAPTCHA, Turnstile). The press judges the
/// page again before it presses, in this order: the document the read was made in, then
/// Cloudflare's check or block page, then a human check's widget, then the account shown as
/// the one signed in and no other address but the vendor's, then an enabled button whose
/// whole label is `authorize`, and it presses at most that one button. What it is given is a
/// string literal, never script text.
#[test]
fn the_press_judges_the_page_again_before_its_one_click() {
    assert!(
        !TOKEN_PAGE_JS.contains(".click("),
        "the read presses nothing"
    );
    assert!(!TOKEN_PAGE_JS.contains("setNativeValue") && !TOKEN_PAGE_JS.contains(".value ="));
    for widget in [
        "textarea[name='h-captcha-response']",
        "iframe[src*='hcaptcha.com']",
        "textarea[name='g-recaptcha-response']",
        "iframe[src*='recaptcha']",
        "input[name='cf-turnstile-response']",
        "iframe[src*='challenges.cloudflare.com']",
    ] {
        assert!(TOKEN_PAGE_JS.contains(widget), "the read misses {widget}");
    }
    let at = "https://claude.com/cai/oauth/authorize?\"; x(); \"";
    let press = press_js(at, " Person@Example.com ").expect("a press");
    let find = |needle: &str| {
        press
            .find(needle)
            .unwrap_or_else(|| panic!("the press lacks {needle}: {press}"))
    };
    let moved = find(
        r#"if (here !== "https://claude.com/cai/oauth/authorize?\"; x(); \"") return answer("moved");"#,
    );
    let bot = find(r#"blockPageOf(text)) return answer("unready");"#);
    let widgets = find(&format!(
        r#"if (document.querySelector("{}")) return answer("unready");"#,
        human_check_widgets!()
    ));
    let shown = find(r#"return answer("unshown");"#);
    let absent = find(r#"if (!hit) return answer("absent");"#);
    let click = find("hit.click();");
    assert!(
        moved < bot && bot < widgets && widgets < shown && shown < absent && absent < click,
        "{press}"
    );
    assert!(
        press.contains(r#"const email = "person@example.com""#),
        "{press}"
    );
    assert!(press.contains(r#"senders = ["anthropic"]"#), "{press}");
    assert!(press.contains(r#"label(b) === "authorize""#), "{press}");
    assert!(press.contains(r#""logged in as""#), "{press}");
    assert_eq!(press.matches(".click(").count(), 1, "{press}");
    assert!(press.contains(&format!(r#"return answer("{PRESSED}");"#)));
}

/// THE PRESS PRESSES ONLY THE PAGE THE READ JUDGED: where the page changed between the read and
/// the press (it moved, a check came, it names the account no longer), the press answers that it
/// pressed nothing, and the drive counts no press: the callback the page then reaches is handed
/// over nowhere, and the drive ends naming a page that did not show the account. A press whose
/// answer never came is not made again, and counts as none either.
/// NEGATIVE CONTROL: with `press` taking every answer for a press, the callback is handed over:
/// `` moved: a callback was delivered ``.
#[test]
fn a_press_on_a_page_that_changed_counts_as_none() {
    let url = token_url(54545);
    let callback = token_callback(54545);
    for did in ["moved", "unready", "unshown", "absent"] {
        let mut stand = Stand::new(
            consent_page(Shows::Account, &url, EMAIL, &callback),
            json!({"url": callback}),
        );
        stand.press = json!({"at": url, "did": did});
        let (ended, met, log, delivered) = drive_on(stand, None, STALL);
        assert!(delivered.is_empty(), "{did}: a callback was delivered");
        assert_eq!(ended, Err(TOKEN_CONSENT_UNPROVEN), "{did}");
        assert_eq!(
            met.map(|met| met.word),
            Some(MetWord::ConsentUnproven),
            "{did}"
        );
        assert_eq!(
            log.iter().filter(|m| *m == "act").count(),
            1,
            "{did}: {log:?}"
        );
    }
}

/// NO CALLBACK WITHOUT THE PRESS: before the tab is sent to the page, the browser is made to
/// pause each request to the waiting listener's port; a page that sends itself to its callback
/// with nothing pressed (a grant the profile already holds skips the consent page) has that
/// request failed in the browser, and a callback the first read already stands at is handed over
/// nowhere: each ends naming a page that did not show the account, with nothing pressed. After
/// the one press, a paused callback is failed in the browser too and handed over by aterm,
/// once.
/// NEGATIVE CONTROL: with `serve_page` handing over a callback whether or not it pressed, the
/// callback the first read stands at is delivered: `` the first read's callback was delivered ``.
#[test]
fn a_callback_with_no_press_behind_it_is_handed_over_nowhere() {
    let url = token_url(54545);
    let callback = token_callback(54545);
    // The first read stands at the callback.
    let (ended, met, log, delivered) = drive(Shows::Callback);
    assert!(
        delivered.is_empty(),
        "the first read's callback was delivered"
    );
    assert_eq!(ended, Err(TOKEN_CONSENT_UNPROVEN));
    assert_eq!(met.map(|met| met.word), Some(MetWord::ConsentUnproven));
    assert!(!log.iter().any(|m| m == "act"), "{log:?}");
    // The browser is told to pause the listener's requests before it is sent anywhere.
    let enabled = log
        .iter()
        .position(|m| m == "Fetch.enable")
        .expect("the hold");
    let sent = log
        .iter()
        .position(|m| m == "Page.navigate")
        .expect("the page");
    assert!(enabled < sent, "{log:?}");

    // The page sends itself to its callback before anything is pressed.
    let mut stand = Stand::new(
        consent_page(Shows::Account, &url, EMAIL, &callback),
        json!({"url": callback}),
    );
    stand.paused = Some(callback.clone());
    let (ended, met, log, delivered) = drive_on(stand, None, STALL);
    assert!(delivered.is_empty(), "a paused callback was delivered");
    assert_eq!(ended, Err(TOKEN_CONSENT_UNPROVEN));
    assert_eq!(met.map(|met| met.word), Some(MetWord::ConsentUnproven));
    assert!(!log.iter().any(|m| m == "act"), "{log:?}");
    assert_eq!(
        log.iter().filter(|m| *m == "Fetch.failRequest").count(),
        1,
        "the browser was let through: {log:?}"
    );
    assert!(!log.iter().any(|m| m == "Fetch.continueRequest"), "{log:?}");

    // After the one press the drive hands the callback over itself, once.
    let (ended, _, log, delivered) = drive(Shows::Account);
    assert_eq!(ended, Ok(()));
    assert_eq!(delivered, [callback]);
    assert!(!log.iter().any(|m| m == "Fetch.continueRequest"), "{log:?}");
}

/// A READ THAT FAILS WHILE A CHECK STANDS IS READ AGAIN, AND THE DEADLINE STILL NAMES IT: where
/// a human check or a page that does not show the account stood at the last read, a read that
/// fails is made again, and one the deadline ended ends the drive as its deadline does, naming
/// what stood, a stop that holds the step, never a browser that could not be driven. With
/// nothing standing, a read that fails ends the drive as one.
/// NEGATIVE CONTROL: with a failed read ending the drive unless a bot check stands, the human
/// check ends at its second read as `browser-control`: `` left: Some(BrowserControl) ``.
#[test]
fn a_read_that_fails_while_a_check_stands_still_names_it() {
    let url = token_url(54545);
    let callback = token_callback(54545);
    // The consent page, its `Authorize` not yet drawn and nothing standing.
    let mut drawing = consent_page(Shows::HumanCheck, &url, EMAIL, &callback);
    if let Some(page) = drawing.as_object_mut() {
        page.insert("humanCheck".to_owned(), Value::Bool(false));
    }
    for (what, page, word, reads) in [
        (
            "HumanCheck",
            consent_page(Shows::HumanCheck, &url, EMAIL, &callback),
            MetWord::HumanCheck,
            3,
        ),
        (
            "Nobody",
            consent_page(Shows::Nobody, &url, EMAIL, &callback),
            MetWord::ConsentUnproven,
            3,
        ),
        ("nothing standing", drawing, MetWord::BrowserControl, 2),
    ] {
        let mut stand = Stand::new(page, json!({"url": callback}));
        stand.fails_from = Some(2);
        let (ended, met, log, delivered) = drive_on(stand, Some(3), aterm_spec::wait::MUST_HAPPEN);
        assert_eq!(met.map(|met| met.word), Some(word), "{what}: {ended:?}");
        assert_eq!(
            log.iter().filter(|m| *m == "read").count(),
            reads,
            "{what}: {log:?}"
        );
        assert!(ended.is_err(), "{what}");
        assert!(
            delivered.is_empty() && !log.iter().any(|m| m == "act"),
            "{what}"
        );
    }
}

/// ONLY A SETUP-TOKEN'S REQUEST, ONLY ON A PROVEN PROFILE: the opener serves a token's page only
/// for a request `setup_token::admit_url` and Claude's pin admit, for a named account whose
/// headless profile a sign-in proved; it makes no profile.
#[test]
fn only_a_setup_tokens_request_on_a_proven_profile_is_served() {
    let store = store_with_profile(EMAIL);
    let url = token_url(54545);
    let of = |url: &str, store: Option<&Path>, email: Option<&str>| {
        TokenRequest::of(url, store, Some("work"), email).map(|request| request.email)
    };
    assert_eq!(
        of(&url, Some(store.path()), Some(EMAIL)),
        Ok(EMAIL.to_owned())
    );
    let wide = url.replace("user%3Ainference", "user%3Ainference+user%3Aprofile");
    let elsewhere = url.replace("https://claude.com/", "https://evil.example/");
    for (what, url, store, email) in [
        (
            "a wider scope",
            wide.as_str(),
            Some(store.path()),
            Some(EMAIL),
        ),
        (
            "another host",
            elsewhere.as_str(),
            Some(store.path()),
            Some(EMAIL),
        ),
        ("no account", url.as_str(), Some(store.path()), None),
        ("no store", url.as_str(), None, Some(EMAIL)),
        ("no profile", url.as_str(), Some(store.path()), Some(OTHER)),
    ] {
        assert!(of(url, store, email).is_err(), "{what} was served");
    }
    assert!(
        !super::super::resolve_browser_profile_dir(store.path(), None, Some(OTHER)).exists(),
        "a profile was made"
    );
    // A profile still marked unproven is none a sign-in proved.
    let profile = super::super::resolve_browser_profile_dir(store.path(), None, Some(EMAIL));
    std::fs::write(profile.join(super::super::PROFILE_UNPROVEN_MARK), b"").expect("the mark");
    assert!(of(&url, Some(store.path()), Some(EMAIL)).is_err());
}
