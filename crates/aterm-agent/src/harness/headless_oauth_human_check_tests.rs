// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HEADLESS SIGN-IN'S DRIVE PRESSES NOTHING ON A CHECK, AND A CONSENT CONTROL ONCE PER PAGE
//! LOAD (`docs/DESIGN-claude-sign-in-sitting-2026-11-12.md` §3, the HARD LIMIT): what its page
//! script ([`oauth_step_js`]) reads before it acts, and what the drive does with each answer,
//! over a stand-in browser's CDP pipes ([`browser`]).

use super::*;
use crate::harness::open_url::Met;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The account the sign-in is for.
const EMAIL: &str = "person@example.com";

/// A synthetic, handwritten authorize URL (every value made up).
const AUTHORIZE: &str = concat!(
    "https://claude.com/cai/oauth/authorize",
    "?code=true",
    "&redirect_uri=http%3A%2F%2Flocalhost%3A9123%2Fcallback",
    "&state=st_human_1"
);

/// The consent page's own address.
const CONSENT: &str = "https://claude.ai/oauth/authorize?code=true&state=st_human_1";

/// The callback [`AUTHORIZE`]'s flow ends at.
const CALLBACK: &str = "http://127.0.0.1:9123/callback?code=made-up&state=st_human_1";

/// What the stand-in page answers one evaluation.
enum Answer {
    /// This value.
    Value(Value),
}

/// A stand-in browser over the CDP pipes whose page answers the `n`-th evaluation (from 1) of
/// `expression` as `page(expression, n)` says: the client, how many evaluations it was asked
/// for so far, and its thread, which answers the log of every method it was sent (each
/// evaluation as `read`).
fn browser(
    mut page: impl FnMut(&str, usize) -> Answer + Send + 'static,
) -> (
    CdpClient,
    Arc<AtomicUsize>,
    std::thread::JoinHandle<Vec<String>>,
) {
    let (to_browser_r, to_browser_w) = aterm_uds::pipe().expect("pipe");
    let (from_browser_r, mut from_browser_w) = aterm_uds::pipe().expect("pipe");
    let asked = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&asked);
    let thread = std::thread::spawn(move || {
        let mut reader = to_browser_r;
        let mut pending = Vec::new();
        let mut buf = [0u8; 8192];
        let mut log = Vec::new();
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            pending.extend_from_slice(&buf[..n]);
            for msg in split_nul_messages(&mut pending) {
                let id = msg.get("id").and_then(Value::as_u64).unwrap_or(0);
                let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
                let answer = match method {
                    "Target.createTarget" => json!({"id": id, "result": {"targetId": "t1"}}),
                    "Target.attachToTarget" => json!({"id": id, "result": {"sessionId": "s1"}}),
                    "Runtime.evaluate" => {
                        let n = counted.fetch_add(1, Ordering::SeqCst) + 1;
                        let expression = msg
                            .pointer("/params/expression")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        match page(expression, n) {
                            Answer::Value(value) => json!({"id": id, "result": {
                                "result": {"type": "object", "value": value}
                            }}),
                        }
                    }
                    _ => json!({"id": id, "result": {}}),
                };
                log.push(if method == "Runtime.evaluate" {
                    "read".to_owned()
                } else {
                    method.to_owned()
                });
                let mut bytes = aterm_json::to_vec(&answer).expect("a message");
                bytes.push(0);
                if from_browser_w.write_all(&bytes).is_err() || method == "Browser.close" {
                    return log;
                }
            }
        }
        log
    });
    (CdpClient::new(to_browser_w, from_browser_r), asked, thread)
}

/// What one drive did: its end, what it met, the callbacks it delivered, and its browser's log.
type Drove = (
    Result<(), &'static str>,
    Option<Met>,
    Vec<String>,
    Vec<String>,
);

/// Drives [`AUTHORIZE`] for [`EMAIL`] on the stand-in page `page`, reading with no pause, until
/// its callback or, once the page was asked for `reads` evaluations, its deadline, as a quit
/// ends one ([`Deadline::cancelled_by`]): only the deadline ends a human check, and this ends it
/// on a count of reads rather than a clock.
fn drive(page: impl FnMut(&str, usize) -> Answer + Send + 'static, reads: usize) -> Drove {
    let _ = take_met();
    let (mut cdp, asked, thread) = browser(page);
    let ends = move || asked.load(Ordering::SeqCst) >= reads;
    let clock = Instant::now;
    let deadline = Deadline::new(aterm_spec::wait::MUST_HAPPEN, &clock).cancelled_by(&ends);
    let mut delivered = Vec::new();
    let ended = finish_deadline(
        drive_cdp_oauth_until(
            &mut cdp,
            AUTHORIZE,
            Some(EMAIL),
            (deadline, Duration::ZERO),
            |_, _| Err("no mailbox in a test"),
            |cb| {
                delivered.push(cb.to_owned());
                Ok(200)
            },
        ),
        deadline,
    );
    let met = take_met();
    drop(cdp);
    (
        ended,
        met,
        delivered,
        thread.join().expect("the stand-in browser"),
    )
}

/// A page's answer `state` at [`CONSENT`].
fn at(state: &str) -> Answer {
    Answer::Value(json!({"state": state, "url": CONSENT}))
}

/// THE PAGE SCRIPT READS EVERY CHECK BEFORE IT ACTS, AND PRESSES CONSENT THROUGH ONE LATCH:
/// Cloudflare's block page, its bot check, and then a human check's widget anywhere in the page
/// (each one aterm's sign-in window knows: hCaptcha, reCAPTCHA, Turnstile) are read before any
/// press, fill, submit or input event, so a page that holds one is acted on in no way, whether
/// the button its check is bound to stands enabled or not. Every consent control is pressed
/// through one press that a document already pressed in answers `consent_pressed` instead, and
/// that a control held `disabled` or `aria-disabled` answers `loading` with nothing pressed;
/// an approval form is pressed only through its own enabled, shown submit button, never
/// submitted past a disabled `Authorize`. What the latch is given is a string literal, never
/// script text.
/// NEGATIVE CONTROL: with the human-check line removed from `oauth_step_js`, `` the page script
/// misses its human check ``.
#[test]
fn the_sign_in_script_reads_every_check_before_it_acts_and_presses_consent_once() {
    let js = oauth_step_js(Some(EMAIL), Some("123456"), (false, false), None);
    let widgets = HUMAN_CHECK_KINDS.map(|(_, selectors)| selectors).join(", ");
    // THE ONE TABLE: every human check's selectors are in the joined list, in the page script's
    // `humanCheckKinds`, and each is one the HTTP engine's HTML reader reads.
    for (name, selectors) in HUMAN_CHECK_KINDS {
        assert!(widgets.contains(selectors), "{name}");
        assert!(
            CHECK_PAGE_JS.contains(&format!("[\"{name}\", \"{selectors}\"]")),
            "{name}"
        );
        for selector in selectors.split(',') {
            assert!(html_selector_supported(selector), "{name}: {selector}");
        }
    }
    for widget in [
        "textarea[name='h-captcha-response']",
        "iframe[src*='hcaptcha.com']",
        "textarea[name='g-recaptcha-response']",
        "iframe[src*='recaptcha']",
        "input[name='cf-turnstile-response']",
        "iframe[src*='challenges.cloudflare.com']",
        "iframe[src*='arkoselabs']",
        "input[name='fc-token']",
    ] {
        assert!(widgets.contains(widget), "{widget}");
    }
    let find = |needle: &str| {
        js.find(needle)
            .unwrap_or_else(|| panic!("no {needle}: {js}"))
    };
    assert!(js.contains(CHECK_PAGE_JS));
    let human = find("if (met) return { state: met.kind, check: met.name, url: href };");
    for act in [
        ".click(",
        "requestSubmit(",
        ".submit(",
        "setNativeValue(",
        "fillCodeInputs(",
        "dispatchEvent(",
        "pressConsent(",
        "submitInput(",
    ] {
        for (at, _) in js.match_indices(act) {
            assert!(
                human < at,
                "{act} at {at}, before the human check at {human}"
            );
        }
    }
    // One press for every consent control, which a document already pressed in skips.
    assert_eq!(js.matches("\"clicked_consent\"").count(), 1, "{js}");
    let spent = find(r#"if (consentSpent) return { state: "consent_pressed", url: href };"#);
    let disabled = find(
        r#"if (control.disabled || String(control.getAttribute("aria-disabled") || "").toLowerCase() === "true") return { state: "loading", url: href };"#,
    );
    let click = find("control.click();");
    let pressed = find(r#"return { state: "clicked_consent", url: href, doc: pageDoc, target };"#);
    assert!(
        human < spent && spent < disabled && disabled < click && click < pressed,
        "{js}"
    );
    assert!(
        !js.contains("btn.click()"),
        "a consent button pressed past the latch"
    );
    assert!(
        !js.contains("approveForm.requestSubmit") && !js.contains("approveForm.submit"),
        "an approval form submitted past its own controls"
    );
    assert!(
        js.contains("pressConsent(approve, btnText(approve))"),
        "{js}"
    );
    assert!(
        js.contains("visibleControl(control) && control.getClientRects().length > 0"),
        "an approval form's submit button pressed disabled or unshown"
    );
    assert!(js.contains("const consentSpent = href === null && pageDoc === null;"));
    // The latch's address and document are literals, whatever they hold.
    let at = "https://claude.ai/oauth/authorize?\"; x(); \"";
    let latched = oauth_step_js(
        Some(EMAIL),
        None,
        (true, false),
        Some((at, "1731600000000.5")),
    );
    assert!(
        latched.contains(
            r#"const consentSpent = href === "https://claude.ai/oauth/authorize?\"; x(); \"" && pageDoc === "1731600000000.5";"#
        ),
        "{latched}"
    );
}

/// A SIGN-IN PAGE WITH A HUMAN CHECK IS PRESSED NOTHING, AND ITS FIRST READ ENDS THE DRIVE
/// NAMING IT: the drive sends the page nothing but its read (no input, no navigation past its
/// start) and ends on that read naming the human check on its host, a stop that holds the row's
/// step for a person. A check that would have gone away by itself is not waited for either: its
/// callback is never delivered. NEGATIVE CONTROL: with the drive taking `human_check` as any
/// other answer, the drive reads on: `` it ends on its first read ``.
#[test]
fn a_sign_in_page_with_a_human_check_is_pressed_nothing_and_ends_naming_it() {
    for clears_after in [usize::MAX, 2] {
        let (ended, met, delivered, log) = drive(
            move |_, n| {
                if n <= clears_after {
                    at("human_check")
                } else {
                    Answer::Value(json!({"state": "callback", "url": CALLBACK}))
                }
            },
            usize::MAX,
        );
        assert_eq!(
            met,
            Some(Met::new(MetWord::HumanCheck, Some("claude.ai"))),
            "the human check was not named"
        );
        assert_eq!(ended, Err(HUMAN_CHECK_MET));
        let met = met.unwrap();
        assert!(met.stopped() && met.word.is_bot_protection() && met.takes_a_person());
        assert!(delivered.is_empty(), "{clears_after}");
        assert_eq!(
            log.iter().filter(|m| *m == "read").count(),
            1,
            "it ends on its first read: {log:?}"
        );
        assert_eq!(
            log.iter().filter(|m| *m == "Page.navigate").count(),
            1,
            "the page was sent somewhere: {log:?}"
        );
        assert!(!log.iter().any(|m| m.starts_with("Input.")), "{log:?}");
    }
}

/// A CONSENT CONTROL IS PRESSED ONCE PER PAGE LOAD: on a consent page that stays where it is
/// after its press, the drive hands the page script the address and document its one press was
/// made in, so every later read of that document presses nothing; a reload of the page (a new
/// document) may be pressed once again, and the drive delivers the callback the page then
/// reaches. The stand-in page plays the script's latch: it presses where the expression it is
/// given does not name its own address and document as pressed, which
/// `the_sign_in_script_reads_every_check_before_it_acts_and_presses_consent_once` holds the
/// script to.
/// NEGATIVE CONTROL: with the script's latch never spent (`consentSpent` always false), every
/// read of the page presses: `` presses per page load `` (20, not 2).
#[test]
fn a_consent_control_is_pressed_once_per_page_load() {
    const LOAD: usize = 10;
    let presses = Arc::new(AtomicUsize::new(0));
    let pressed = Arc::clone(&presses);
    let doc = |n: usize| {
        if n <= LOAD {
            "1731600000000.5"
        } else {
            "1731600009999.25"
        }
    };
    let (ended, met, delivered, log) = drive(
        move |expression, n| {
            if n > 2 * LOAD {
                return Answer::Value(json!({"state": "callback", "url": CALLBACK}));
            }
            let spent = format!(
                "const consentSpent = href === {} && pageDoc === {};",
                aterm_json::to_string(&CONSENT).unwrap(),
                aterm_json::to_string(&doc(n)).unwrap()
            );
            if expression.contains(&spent) {
                return at("consent_pressed");
            }
            pressed.fetch_add(1, Ordering::SeqCst);
            Answer::Value(json!({
                "state": "clicked_consent",
                "url": CONSENT,
                "doc": doc(n),
                "target": "authorize",
            }))
        },
        usize::MAX,
    );
    assert_eq!(presses.load(Ordering::SeqCst), 2, "presses per page load");
    assert_eq!(ended, Ok(()));
    assert_eq!(met, None);
    assert_eq!(delivered, [CALLBACK]);
    assert_eq!(log.iter().filter(|m| *m == "read").count(), 2 * LOAD + 1);
}

/// PROTON'S OWN HUMAN VERIFICATION IS A CHECK OF THE ONE PREDICATE: its captcha frame (on its
/// account API or its verification host), its captcha node and its `Human verification` modal
/// are read by the shared predicate itself, the page script's and the HTTP engine's alike, so
/// every act that reads it (a webmail sign-in's fill and submit included) stops on them. A
/// Proton sign-in page with none of them is no check.
/// NEGATIVE CONTROL: with the `Proton` row taken out of `HUMAN_CHECK_KINDS` and
/// `humanCheckKinds` (the branch's first form, which read it only in the mail reader's own
/// list), Proton's frame is no check: `` Proton's captcha frame: None ``.
#[test]
fn protons_human_verification_is_a_check_of_the_one_predicate() {
    const LOGIN: &str = "https://account.proton.me/login";
    for (what, html, check) in [
        (
            "Proton's captcha frame",
            "<html><body><iframe src='https://account-api.proton.me/core/v4/captcha?Token=made-up'></iframe></body></html>",
            Some(CheckMet::HumanCheck("Proton".into())),
        ),
        (
            "Proton's verification host",
            "<html><body><iframe src='https://verify.proton.me/?methods=captcha'></iframe></body></html>",
            Some(CheckMet::HumanCheck("Proton".into())),
        ),
        (
            "Proton's captcha node",
            "<html><body><div data-testid='human-verification:captcha'></div></body></html>",
            Some(CheckMet::HumanCheck("Proton".into())),
        ),
        (
            "Proton's modal",
            "<html><body><h1>Human verification</h1><p>To fight spam and abuse, please complete the verification.</p></body></html>",
            Some(CheckMet::HumanCheck("captcha".into())),
        ),
        (
            "a plain Proton sign-in",
            "<html><head><title>Sign in | Proton</title></head><body><h1>Sign in</h1><input name='username'><input type='password' name='password'><button>Sign in</button></body></html>",
            None,
        ),
    ] {
        assert_eq!(check_in_html(LOGIN, html), check, "{what}: {html}");
    }
    assert!(
        CHECK_PAGE_JS.contains(r#"["Proton", ""#),
        "the page script's row"
    );
    assert!(CHECK_PAGE_JS.contains(r#"const humanCheckHeadings = ["human verification"];"#));
    // A rendered page whose heading names the check is one too, read by the Rust predicate.
    let modal = Page {
        url: LOGIN.to_owned(),
        heading: "Human verification".to_owned(),
        ..Page::default()
    };
    assert_eq!(
        check_met(&modal),
        Some(CheckMet::HumanCheck("captcha".into()))
    );
    let plain = Page {
        url: LOGIN.to_owned(),
        heading: "Sign in".to_owned(),
        ..Page::default()
    };
    assert_eq!(check_met(&plain), None);
}

/// THE WEBMAIL STEP READS THE CHECK BEFORE IT ACTS, AND A SIGNED-IN INBOX BY WHAT NO MAIL CAN
/// PUT THERE: the webmail step's check answer comes before every fill, code fill, submit and
/// press in the script, and again before it opens a message; on a sign-in page it reads the
/// whole predicate (`checkMet()`), and once the inbox is up (its rows, no sign-in field) the
/// mailbox's (`mailboxCheckMet()`), which reads the top document's title against Cloudflare's
/// whole titles, Cloudflare's marks, Google's `/sorry` page and a widget in the top document,
/// never the page's words, headings or controls, where a message's subject is, nor a frame,
/// where its body is.
/// NEGATIVE CONTROL: with `webmail_script` reading `checkMet()` on the inbox too (the branch's
/// first form), the inbox scope is gone: `` the inbox is read by the mailbox's predicate ``.
#[test]
fn the_webmail_step_reads_the_check_before_it_acts() {
    let creds = (
        Some("ada@x.example"),
        Some("fixture-only"),
        Some("fixture-only"),
        Some("123456"),
    );
    let mut scripts = vec![webmail_step_js(creds, (false, false, false, false), 0, &[])];
    #[cfg(unix)]
    scripts.push(webmail_login_js(creds, (false, false, false, false)));
    for script in &scripts {
        assert!(script.contains(CHECK_PAGE_JS));
        let answer = script
            .find("if (met) return { state: met.kind, check: met.name, url: href };")
            .expect("the check's answer");
        assert!(
            script.contains("const checkHere = () => inboxUp ? mailboxCheckMet() : checkMet();"),
            "the inbox is read by the mailbox's predicate"
        );
        assert!(script.find("const met = checkHere();").expect("the read") < answer);
        for act in [
            "setNativeValue(",
            "fillCodeInputs(",
            "submitInput(",
            "clickSubmit(",
            ".click(",
        ] {
            for (at, _) in script.match_indices(act) {
                assert!(
                    answer < at,
                    "{act} at {at} before the check's answer at {answer}"
                );
            }
        }
        let open = script.find("chosen.click();").expect("the message's open");
        assert!(script.find("!checkHere()").expect("the check before it") < open);
    }
    // The mailbox's predicate reads no words, headings, controls or frames.
    let mailbox = CHECK_PAGE_JS
        .split("const mailboxCheckMet = () => {")
        .nth(1)
        .and_then(|rest| rest.split("\n  };").next())
        .expect("the mailbox's predicate");
    for read in [
        "innerText",
        "textContent",
        "heads",
        "humanCheckOf",
        "checkRoots",
        "botCheckWords",
    ] {
        assert!(
            !mailbox.contains(read),
            "the mailbox's predicate reads {read}: {mailbox}"
        );
    }
    for read in [
        "botCheckTitles.includes(title)",
        "botCheckMarkers()",
        "/sorry",
        "humanCheckKinds",
    ] {
        assert!(
            mailbox.contains(read),
            "the mailbox's predicate misses {read}: {mailbox}"
        );
    }
}
