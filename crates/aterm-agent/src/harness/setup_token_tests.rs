// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The setup-token capture's tests: its grammar over frames built from Claude Code 2.1.318's
//! own literals, its URL check, its route refusal and child environment, the expiry, and a
//! stand-in `claude setup-token` (this test binary in a child mode, [`stand_in`]) for each way a
//! run ends.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use zeroize::Zeroizing;

use super::*;
use crate::harness::align::owned_fixture::Born;
use crate::harness::wake_deadline::SleepSeam;

const EMAIL: &str = "person@example.com";

/// A token in grammar, spelled in halves so no scanner reads it as a key.
pub(in crate::harness) fn token() -> String {
    format!(
        "{}{}",
        "sk-ant-oa", "t01-STANDIN_tok-0123456789abcdefghijklmnopqrstuvwxyzABCDEFGH"
    )
}

/// Another one.
fn other_token() -> String {
    format!(
        "{}{}",
        "sk-ant-oa", "t01-OTHER_tok-9876543210zyxwvutsrqponmlkjihgfedcbaZYXWVUTS"
    )
}

/// What 2.1.318's authorize URL looks like for a setup-token (`Ngo`): `scope` is
/// `user:inference` alone, URL-encoded.
fn url_with(host: &str, scope: &str) -> String {
    format!(
        "https://{host}/cai/oauth/authorize?code=true&client_id=9d1c250a-e61b-44d9-88ed-\
         5944d1962f5e&response_type=code&redirect_uri=http%3A%2F%2Flocalhost%3A54545%2Fcallback&\
         scope={scope}&code_challenge=STANDINCHALLENGE&code_challenge_method=S256&\
         state=STANDINSTATE"
    )
}

fn good_url() -> String {
    url_with("claude.com", "user%3Ainference")
}

/// The waiting frame (`urlToCopy` in 2.1.318), as Ink lays it out with colour: the dim hint, the
/// URL as an OSC 8 link whose text is the URL, and the paste prompt.
fn waiting_frame(url: &str) -> String {
    format!(
        "\x1b[2mBrowser didn't open? Use the url below to sign in \x1b[22m\n\n\
         \x1b]8;;{url}\x1b\\\x1b[2m{url}\x1b[22m\x1b]8;;\x1b\\\n\n\
         Paste code here if prompted > "
    )
}

/// The success frame (2.1.318's literals), the token in warning colour.
fn success_frame(token: &str, words: &str) -> String {
    format!(
        "\x1b[32m\u{2713} Long-lived authentication token created successfully!\x1b[39m\n\n\
         Your OAuth token (valid for {words}):\n\n\
         \x1b[33m{token}\x1b[39m\n\n\
         \x1b[2mStore this token securely. You won't be able to see it again.\x1b[22m\n\n\
         \x1b[2mUse this token by setting: export CLAUDE_CODE_OAUTH_TOKEN=<token>\x1b[22m\n"
    )
}

/// Ink's redraw of `lines` lines: erase each, move up, back to column one.
fn erase(lines: usize) -> String {
    "\x1b[2K\x1b[1A".repeat(lines) + "\x1b[2K\x1b[G"
}

/// What the judge says of `raw`, fed `step` bytes at a time with a judgment after each, and
/// once more as ended where `ended`.
fn judged_in_steps(raw: &[u8], step: usize, ended: bool) -> Verdict {
    let mut out = Vec::with_capacity(BUFFER);
    let mut cleaner = Cleaner::default();
    let mut judge = Judge::new(EMAIL);
    for piece in raw.chunks(step) {
        assert!(cleaner.feed(piece, &mut out), "the frame fits the buffer");
        match judge.judge(&out, false) {
            Verdict::Pending => {}
            done => return done,
        }
    }
    if ended {
        judge.judge(&out, true)
    } else {
        Verdict::Pending
    }
}

fn judged(raw: &str) -> Verdict {
    judged_in_steps(raw.as_bytes(), raw.len().max(1), true)
}

#[track_caller]
fn minted(verdict: Verdict) -> Minted {
    match verdict {
        Verdict::Minted(minted) => minted,
        Verdict::Refused(e) => panic!("refused: {e}"),
        Verdict::Pending => panic!("no verdict"),
    }
}

#[track_caller]
fn refused(verdict: Verdict) -> Error {
    match verdict {
        Verdict::Refused(e) => e,
        Verdict::Minted(_) => panic!("a token was taken"),
        Verdict::Pending => panic!("no verdict"),
    }
}

/// THE PLAIN FRAMES READ AS ONE TOKEN, a year: the waiting frame, then the success frame, as
/// 2.1.318 words them.
#[test]
fn the_plain_frames_give_the_token() {
    let raw = waiting_frame(&good_url()) + &success_frame(&token(), "1 year");
    let minted = minted(judged(&raw));
    assert_eq!(minted.token.as_slice(), token().as_bytes());
    assert_eq!(minted.lifetime, Lifetime::Days(365));
    assert_eq!(minted.account, None);
}

/// INK'S REDRAWS AND ESCAPES ARE SKIPPED: the waiting frame drawn three times over erase
/// sequences and carriage returns, then the success frame drawn twice (the token twice), read a
/// byte at a time, give the one token. Two different tokens are refused.
#[test]
fn redraws_and_a_repeated_token_give_one_token_and_two_tokens_are_refused() {
    let wait = waiting_frame(&good_url());
    let mut raw = wait.clone();
    for _ in 0..2 {
        raw += &erase(5);
        raw += "\r";
        raw += &wait;
    }
    raw += &erase(5);
    raw += &success_frame(&token(), "1 year");
    raw += &erase(9);
    raw += &success_frame(&token(), "1 year");
    for step in [1, 7, 4096] {
        let minted = minted(judged_in_steps(raw.as_bytes(), step, true));
        assert_eq!(
            minted.token.as_slice(),
            token().as_bytes(),
            "in steps of {step}"
        );
    }
    // A frame whose export line carries another token is refused: the frame is judged whole.
    let two = wait.clone()
        + &success_frame(&token(), "1 year").replace("=<token>", &format!("={}", other_token()));
    assert_eq!(refused(judged(&two)), Error::TwoTokens);
    // One whose export line carries the same token gives it.
    let same = wait.clone()
        + &success_frame(&token(), "1 year").replace("=<token>", &format!("={}", token()));
    assert_eq!(minted(judged(&same)).token.as_slice(), token().as_bytes());
    // A token-shaped value before the frame that differs from the frame's is refused too.
    let early = wait + &other_token() + "\n" + &success_frame(&token(), "1 year");
    assert_eq!(refused(judged(&early)), Error::TwoTokens);
}

/// `text` laid out as 2.1.318's renderer lays it on a `dumb` terminal (S9 lane A): each word
/// written, then the cursor moved past the blank after it to the next word's column (`CSI <n>G`),
/// the line ending in a newline; nothing writes a space.
fn placed(text: &str) -> String {
    let mut out = String::new();
    for line in text.split('\n') {
        let mut col = 0usize;
        for (i, word) in line.split(' ').enumerate() {
            if i > 0 {
                col += 1;
                out += &format!("\x1b[{}G", col + 1);
            }
            out += word;
            col += word.chars().count();
        }
        out += "\n";
    }
    out
}

/// WORDS PLACED BY COLUMN MOVES READ AS ONE LINE: 2.1.318 on the capture's `dumb` terminal skips
/// the blanks between words with a move to a column (S9 lane A saw `CSI <n>G` between every
/// word), so its paste prompt is read, and its success frame laid out the same way gives the
/// token; a move back to a column at or left of the cursor is a redraw, a newline, so a redraw's
/// pieces are never one word.
/// NEGATIVE CONTROL: reading `G` as any other CSI (a newline, as before the lab ran) puts each
/// word on a line of its own, and the paste prompt is never read: the lab's first run hung there.
#[test]
fn words_placed_by_column_moves_read_as_one_line() {
    let clean = |raw: &str| {
        let mut out = Vec::with_capacity(BUFFER);
        assert!(Cleaner::default().feed(raw.as_bytes(), &mut out));
        String::from_utf8(out).expect("UTF-8")
    };
    assert_eq!(
        clean(&placed("Paste code here if prompted >")),
        "Paste code here if prompted >\n"
    );
    assert_eq!(clean("abc\x1b[1Gdef"), "abc\ndef", "back to column one");
    assert_eq!(clean("abcd\x1b[3Gx"), "abcd\nx", "back within the line");
    assert_eq!(clean("ab\x1b[3Gc"), "abc", "a move to where it stands");
    assert_eq!(
        clean("a\x1b[2Cb\x1b[6Gc"),
        "a b c",
        "a forward after a forward"
    );
    assert_eq!(
        clean("\u{2722} x\x1b[5Gy"),
        "\u{2722} x y",
        "a character is one column"
    );

    let link = |url: &str| format!("\x1b]8;;{url}\x1b\\{url}\x1b]8;;\x1b\\\n");
    let mut out = Vec::with_capacity(BUFFER);
    let mut judge = Judge::new(EMAIL);
    let waiting = placed("Browser didn't open? Use the url below to sign in (c to copy)")
        + &link(&good_url())
        + &placed("Paste code here if prompted > ");
    assert!(Cleaner::default().feed(waiting.as_bytes(), &mut out));
    assert!(matches!(judge.judge(&out, false), Verdict::Pending));
    assert!(
        judge.url_ok && judge.asks,
        "the URL admitted and the prompt read"
    );

    let success = placed(&format!(
        "\u{2713} Long-lived authentication token created successfully!\n\n\
         Your OAuth token (valid for 1 year):\n\n{}\n\n\
         Store this token securely. You won't be able to see it again.\n\n\
         Use this token by setting: export CLAUDE_CODE_OAUTH_TOKEN=<token>",
        token()
    ));
    let minted = minted(judged(&(waiting + &success)));
    assert_eq!(minted.token.as_slice(), token().as_bytes());
    assert_eq!(minted.lifetime, Lifetime::Days(365));
}

/// NO TOKEN, NO MINT: a success frame with no token, or output that ends with none, is
/// `NoToken`; a token before the sign-in was checked is `Unchecked`.
#[test]
fn no_token_and_an_unchecked_token_are_refused() {
    let frame = success_frame(&token(), "1 year");
    let no_token = frame.replace(&token(), "");
    let raw = waiting_frame(&good_url()) + &no_token;
    assert_eq!(refused(judged(&raw)), Error::NoToken);
    assert_eq!(refused(judged(&waiting_frame(&good_url()))), Error::NoToken);
    assert_eq!(refused(judged(&frame)), Error::Unchecked);
    // A frame cut inside its token gives nothing yet: the token's line is not whole.
    let raw = waiting_frame(&good_url()) + &frame;
    let cut = raw.find(&token()).expect("the token") + 20;
    assert!(matches!(
        judged_in_steps(&raw.as_bytes()[..cut], 1, false),
        Verdict::Pending
    ));
}

/// EACH FAILURE PHRASE OF 2.1.318 IS ITS OWN FAILURE, the specific before the generic, and none
/// of the error's words are the child's.
#[test]
fn each_failure_phrase_is_its_failure() {
    let cases = [
        (
            "OAuth error: Invalid code. Please make sure the full code was copied",
            Failure::CodeIncomplete,
        ),
        (
            "OAuth error: Failed to exchange authorization code for access token. Please try again.",
            Failure::Exchange,
        ),
        (
            "Your account is on hold and can't sign in to Claude Code. View details or appeal: \
             https://claude.ai/appeal",
            Failure::OnHold,
        ),
        (
            "Anthropic reported the token revoked; it was not shown.",
            Failure::Withheld,
        ),
        (
            "The token could not be confirmed revoked and was not shown ask your admin to revoke \
             it in Console.",
            Failure::Withheld,
        ),
        (
            "OAuth error: Couldn't start the local listener that receives the browser sign-in. \
             Try again.",
            Failure::Listener,
        ),
        ("OAuth error: something new SENTINEL-7", Failure::OAuth),
    ];
    for (phrase, failure) in cases {
        let raw = waiting_frame(&good_url())
            + "\n\x1b[31m"
            + phrase
            + "\x1b[39m\n\nPress Enter to retry.\n";
        let error = refused(judged(&raw));
        assert_eq!(error, Error::Failed(failure), "{phrase}");
        assert!(
            !format!("{error} {error:?}").contains("SENTINEL"),
            "{phrase}"
        );
    }
}

/// THE AUTHORIZE URL: claude.com's (or claude.ai's) authorize path with `user:inference` alone
/// is admitted; another host, a look-alike, a user or port, plain http, a wider scope, two
/// scopes or another lifetime is refused; a URL that is no authorization request is neither.
#[test]
fn the_authorize_url_is_admitted_only_for_an_inference_token() {
    assert_eq!(
        admit_url(good_url().as_bytes()),
        Admit::Admitted("claude.com")
    );
    let ai = good_url().replace("claude.com/cai/oauth", "claude.ai/oauth");
    assert_eq!(admit_url(ai.as_bytes()), Admit::Admitted("claude.ai"));
    let lifetime = good_url() + "&expires_in=31536000";
    assert_eq!(
        admit_url(lifetime.as_bytes()),
        Admit::Admitted("claude.com")
    );
    for bad in [
        url_with("evil.example", "user%3Ainference"),
        url_with("claude.com.evil.example", "user%3Ainference"),
        url_with("someone@claude.com", "user%3Ainference"),
        url_with("claude.com:8443", "user%3Ainference"),
        good_url().replace("https://", "http://"),
        url_with("claude.com", "user%3Ainference+user%3Aprofile"),
        url_with("claude.com", "user%3Ainference%20org%3Acreate_api_key"),
        good_url() + "&scope=user%3Aprofile",
        good_url() + "&expires_in=60",
        good_url().replace("/cai/oauth/authorize", "/oauth/authorize/../device"),
    ] {
        assert_eq!(admit_url(bad.as_bytes()), Admit::Refused, "{bad}");
    }
    assert_eq!(
        admit_url(b"https://code.claude.com/docs/en/legal-and-compliance"),
        Admit::NotARequest
    );
    // In the frame, a refused request ends the capture before its token.
    let raw = waiting_frame(&url_with("evil.example", "user%3Ainference"))
        + &success_frame(&token(), "1 year");
    assert_eq!(refused(judged(&raw)), Error::Request);
    // An OSC 8 link whose address differs from its text is judged by its address too.
    let hidden = format!(
        "\x1b]8;;{}\x1b\\{}\x1b]8;;\x1b\\\n",
        url_with("claude.com", "user%3Ainference+user%3Aprofile"),
        good_url()
    ) + &success_frame(&token(), "1 year");
    assert_eq!(refused(judged(&hidden)), Error::Request);
}

/// THE LIFETIME WORDS: "1 year" is 365 days, "30 days" 30, "1 day" 1, "less than a day" is
/// stored never; the end is the earliest the rounded words allow, and with no words the
/// owner's mint + 31536000 s, assumed.
#[test]
fn the_lifetime_words_give_the_expiry() {
    let at = 1_800_000_000;
    let cases = [
        ("1 year", Lifetime::Days(365)),
        ("30 days", Lifetime::Days(30)),
        ("1 day", Lifetime::Days(1)),
        ("less than a day", Lifetime::UnderADay),
        ("a while", Lifetime::Unprinted),
        ("1 days", Lifetime::Unprinted),
    ];
    for (words, lifetime) in cases {
        let line = format!("Your OAuth token (valid for {words}):");
        assert_eq!(valid_for(line.as_bytes()), Some(lifetime), "{words}");
    }
    assert_eq!(valid_for(b"Store this token securely."), None);
    assert_eq!(
        expiry_of(Lifetime::Days(365), at),
        Some((at + 365 * 86_400 - 43_200, ExpiryFrom::Printed))
    );
    assert_eq!(
        expiry_of(Lifetime::Days(30), at),
        Some((at + 30 * 86_400 - 43_200, ExpiryFrom::Printed))
    );
    assert_eq!(
        expiry_of(Lifetime::Days(1), at),
        Some((at + 43_200, ExpiryFrom::Printed))
    );
    assert_eq!(expiry_of(Lifetime::UnderADay, at), None);
    assert_eq!(
        expiry_of(Lifetime::Unprinted, at),
        Some((at + 31_536_000, ExpiryFrom::Assumed))
    );
    let raw = waiting_frame(&good_url()) + &success_frame(&token(), "less than a day");
    assert_eq!(minted(judged(&raw)).lifetime, Lifetime::UnderADay);
    let raw = waiting_frame(&good_url()) + &success_frame(&token(), "30 days");
    assert_eq!(minted(judged(&raw)).lifetime, Lifetime::Days(30));
}

/// AN ACCOUNT THE FRAME NAMES MUST BE THE ONE ASKED FOR: a later build's frame naming another
/// email is refused, naming the one to sign in as; the same email (in any case) is kept.
#[test]
fn a_named_account_must_be_the_one_asked_for() {
    let named = |email: &str| {
        waiting_frame(&good_url())
            + &success_frame(&token(), "1 year").replace(
                "successfully!",
                &format!("successfully!\nSigned in as {email}."),
            )
    };
    assert_eq!(
        refused(judged(&named("someone.else@example.com"))),
        Error::WrongAccount(EMAIL.to_owned())
    );
    assert_eq!(
        minted(judged(&named("Person@Example.com")))
            .account
            .as_deref(),
        Some("Person@Example.com")
    );
}

/// THE DEFAULT ROUTE ONLY: a verb environment naming a route is named by its variable (never
/// its value); `NO_PROXY` and an empty value route nothing.
#[test]
fn a_route_in_the_environment_is_named() {
    let env = |pairs: &[(&str, &str)]| -> Vec<(OsString, OsString)> {
        pairs.iter().map(|(k, v)| (k.into(), v.into())).collect()
    };
    for key in [
        "HTTPS_PROXY",
        "https_proxy",
        "ANTHROPIC_BASE_URL",
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_PROXY_URL",
    ] {
        assert_eq!(
            route_in(&env(&[("HOME", "/h"), (key, "http://gw.example:1")])).as_deref(),
            Some(key)
        );
    }
    assert_eq!(
        route_in(&env(&[("NO_PROXY", "*"), ("HTTPS_PROXY", " ")])),
        None
    );
}

/// THE CHILD'S ENVIRONMENT HOLDS NO ROUTE, CREDENTIAL, BROWSER OR ATERM VARIABLE, whatever the
/// verb's holds: exactly the unrouted pass-through, the fixed PATH, `TERM=dumb`, both Claude
/// homes on the scratch folder and the opt-outs.
#[test]
fn the_child_environment_is_closed() {
    let scratch = Path::new("/scratch/setup-token-1-2");
    let env = child_env(&|_| Some(OsString::from("set")), scratch);
    let mut names: Vec<&str> = env.iter().map(|(k, _)| *k).collect();
    names.sort_unstable();
    let mut want: Vec<&str> = super::super::login_drive::passed_unrouted()
        .chain([
            "PATH",
            "TERM",
            "CLAUDE_CONFIG_DIR",
            "CLAUDE_SECURESTORAGE_CONFIG_DIR",
        ])
        .chain(OPT_OUTS.iter().map(|(k, _)| *k))
        .collect();
    want.sort_unstable();
    assert_eq!(names, want);
    for name in &names {
        assert!(
            !super::super::netwatch::names_a_route(name, "x"),
            "{name} routes"
        );
        assert!(
            !super::super::accounts::CREDENTIAL_KEYS.contains(name),
            "{name}"
        );
        assert!(
            !super::super::accounts::PROFILE_KEYS.contains(name),
            "{name}"
        );
        assert!(*name != "BROWSER" && !name.starts_with("ATERM"), "{name}");
    }
    let value = |name: &str| env.iter().find(|(k, _)| *k == name).map(|(_, v)| v.clone());
    assert_eq!(value("TERM"), Some("dumb".into()));
    assert_eq!(
        value("CLAUDE_CONFIG_DIR"),
        Some(scratch.as_os_str().to_owned())
    );
    assert_eq!(
        value("CLAUDE_SECURESTORAGE_CONFIG_DIR"),
        Some(scratch.as_os_str().to_owned())
    );
    assert_eq!(value("NO_PROXY"), Some("set".into()));
}

// ---- The stand-in ---------------------------------------------------------------------------

/// The file in the stand-in's `HOME` that says its mode (first line) and the `sleep` it keeps
/// in its group (second line), and so that it is one.
pub(in crate::harness) const MODE_FILE: &str = ".aterm-setup-token-stand-in";

/// Set when this test binary is a verb running a capture of the stand-in ([`verb_stand_in`]):
/// the stand-in's `HOME`.
const VERB_HOME: &str = "ATERM_SETUP_TOKEN_TEST_VERB";

/// The stand-in's own `HOME` where this process is one (`setup-token`'s environment and a mode
/// file), else `None`.
fn stand_in_home() -> Option<PathBuf> {
    let var = |k: &str| std::env::var_os(k);
    let scratch = PathBuf::from(var("CLAUDE_CONFIG_DIR")?);
    let is_capture = var("TERM").is_some_and(|t| t == "dumb")
        && var("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC").is_some_and(|v| v == "1")
        && scratch
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with(SCRATCH_PREFIX));
    let home = PathBuf::from(var("HOME")?);
    (is_capture && home.join(MODE_FILE).is_file()).then_some(home)
}

/// Writes `text` to `path` whole (through a rename).
fn put(path: &Path, text: &str) {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text).expect("a stand-in file");
    std::fs::rename(&tmp, path).expect("a stand-in file, renamed");
}

/// THE STAND-IN `claude setup-token`: a no-op in a plain test run. As the capture's child it
/// says its pid (and its kept `sleep`'s, in its group) and its environment's names, waits for
/// the test's `go`, then plays its mode with 2.1.318's frames.
#[test]
fn stand_in() {
    let Some(home) = stand_in_home() else {
        return;
    };
    let mode_text = std::fs::read_to_string(home.join(MODE_FILE)).expect("the mode");
    let mut lines = mode_text.lines();
    let mode = lines.next().unwrap_or("").to_owned();
    let sleep = lines.next().map(PathBuf::from);
    let names: Vec<String> = std::env::vars_os()
        .map(|(k, v)| format!("{}={}", k.to_string_lossy(), v.to_string_lossy()))
        .collect();
    put(&home.join("env"), &names.join("\n"));
    let mut member = None;
    if let Some(sleep) = sleep {
        let kept = aterm_uds::spawnfd::command(sleep)
            .arg("600")
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .stderr(aterm_uds::spawnfd::Stdio::null())
            .spawn()
            .expect("the kept sleep");
        put(&home.join("member"), &kept.id().to_string());
        member = Some(kept);
    }
    put(&home.join("pid"), &std::process::id().to_string());
    let until = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while !home.join("go").exists() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut out = std::io::stdout().lock();
    let mut say = |text: &str| {
        let _ = out.write_all(text.as_bytes());
        let _ = out.flush();
    };
    let wait_for_ever = || loop {
        std::thread::sleep(Duration::from_millis(50));
    };
    match mode.as_str() {
        "success" => {
            say(&waiting_frame(&good_url()));
            // The person signs in once told to: the capture said the browser opened.
            while !home.join("told").exists() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(10));
            }
            say(&erase(5));
            say(&success_frame(&token(), "1 year"));
            std::thread::sleep(Duration::from_millis(500));
        }
        "short" => {
            say(&waiting_frame(&good_url()));
            while !home.join("told").exists() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(10));
            }
            say(&erase(5));
            say(&success_frame(&token(), "less than a day"));
            std::thread::sleep(Duration::from_millis(500));
        }
        "paste" => {
            say(&waiting_frame(&good_url()));
            // Masked, as Ink's paste input is: the code never echoes.
            // SAFETY: plain termios calls on this process's stdin, a terminal.
            unsafe {
                let mut t: libc::termios = std::mem::zeroed();
                if libc::tcgetattr(0, &mut t) == 0 {
                    t.c_lflag &= !libc::ECHO;
                    libc::tcsetattr(0, libc::TCSANOW, &t);
                }
            }
            let mut line = String::new();
            let _ = std::io::stdin().read_line(&mut line);
            put(&home.join("pasted"), line.trim_end());
            say(&success_frame(&token(), "1 year"));
        }
        "failure" => {
            say(&waiting_frame(&good_url()));
            say(
                "\nOAuth error: Failed to exchange authorization code for access token. \
                 SENTINEL-c0ffee\n\nPress Enter to retry.\n",
            );
            wait_for_ever();
        }
        "noise" => {
            say(&waiting_frame(&good_url()));
            let line = "noise ".repeat(20) + "\n";
            for _ in 0..(BUFFER / line.len() + 64) {
                say(&line);
            }
            wait_for_ever();
        }
        "bad-host" => {
            say(&waiting_frame(&url_with(
                "evil.example",
                "user%3Ainference",
            )));
            wait_for_ever();
        }
        "wide-scope" => {
            say(&waiting_frame(&url_with(
                "claude.com",
                "user%3Ainference+user%3Aprofile",
            )));
            wait_for_ever();
        }
        "wait" => {
            say(&waiting_frame(&good_url()));
            wait_for_ever();
        }
        _ => wait_for_ever(),
    }
    drop(member);
}

/// A stand-in's world: its `HOME` (the mode file in it) and the account's store folder.
pub(in crate::harness) struct World {
    home: aterm_tempfile::TempDir,
    store: PathBuf,
}

impl World {
    /// A world whose stand-in plays `mode`, keeping a `sleep` in its group where `group`.
    pub(in crate::harness) fn new(mode: &str, group: bool) -> Self {
        let home = aterm_tempfile::Builder::new()
            .prefix(&format!("aterm-setup-token-{mode}-"))
            .suffix(".noindex")
            .tempdir()
            .expect("the stand-in's home");
        let mut text = format!("{mode}\n");
        if group {
            text += &crate::test_program::kept("/bin/sleep").to_string_lossy();
            text += "\n";
        }
        std::fs::write(home.path().join(MODE_FILE), text).expect("the mode file");
        let store = home.path().join("store");
        Self { home, store }
    }

    pub(in crate::harness) fn home(&self) -> &Path {
        self.home.path()
    }

    /// The stand-in, as the program a capture runs: this test binary at [`stand_in`].
    pub(in crate::harness) fn program() -> Program {
        Program {
            path: std::env::current_exe().expect("the test binary"),
            args: [
                "--exact",
                "harness::setup_token::tests::stand_in",
                "--nocapture",
                "--test-threads=1",
                "--quiet",
                "setup-token",
            ]
            .into_iter()
            .map(OsString::from)
            .collect(),
        }
    }

    /// The verb's environment: this world's `HOME`, and `extra`.
    pub(in crate::harness) fn env(&self, extra: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        let mut env: Vec<(OsString, OsString)> = vec![
            ("HOME".into(), self.home().as_os_str().to_owned()),
            ("USER".into(), "person".into()),
            ("BROWSER".into(), "/bin/false".into()),
            ("ATERM_SESSION_ID".into(), "s-1".into()),
            ("CLAUDE_CODE_OAUTH_TOKEN".into(), "not-for-the-child".into()),
        ];
        if let Some(tmp) = std::env::var_os("TMPDIR") {
            env.push(("TMPDIR".into(), tmp));
        }
        env.extend(extra.iter().map(|(k, v)| ((*k).into(), (*v).into())));
        env
    }

    /// The scratch folders left in the store.
    fn scratch_left(&self) -> Vec<String> {
        std::fs::read_dir(&self.store)
            .map(|entries| {
                entries
                    .flatten()
                    .filter_map(|e| e.file_name().into_string().ok())
                    .filter(|n| n.starts_with(SCRATCH_PREFIX))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Runs a capture of the stand-in with `person`, under `deadline`.
    fn capture(&self, person: &Stub<'_>, deadline: WakeDeadline) -> Result<Minted, Error> {
        let program = Self::program();
        let env = self.env(&[]);
        capture(
            &Capture {
                program: &program,
                store: &self.store,
                sign_in_as: EMAIL,
                env: &env,
                window: None,
                deadline,
            },
            person,
        )
    }
}

/// The person, stood in for: says `go` to the stand-in once it proved the stand-in (and its
/// group's `sleep`) by birth, and records what the capture said.
struct Stub<'a> {
    world: &'a World,
    group: bool,
    proven: Mutex<Vec<Born>>,
    said: Mutex<Vec<String>>,
    /// The code to paste the first time it is asked.
    code: Mutex<Option<Zeroizing<Vec<u8>>>>,
    /// Stops (Ctrl-C) once the capture said the browser opened.
    stops: bool,
    /// Says, once it proved the stand-in, that the host slept this long.
    sleeps: Option<(crate::harness::wake_deadline::Sleeper, Duration)>,
    pasted_asks: AtomicBool,
}

impl<'a> Stub<'a> {
    fn new(world: &'a World, group: bool) -> Self {
        Self {
            world,
            group,
            proven: Mutex::new(Vec::new()),
            said: Mutex::new(Vec::new()),
            code: Mutex::new(None),
            stops: false,
            sleeps: None,
            pasted_asks: AtomicBool::new(false),
        }
    }

    fn said(&self) -> Vec<String> {
        self.said.lock().unwrap().clone()
    }

    fn proven(&self) -> Vec<Born> {
        self.proven.lock().unwrap().clone()
    }

    /// Proves the stand-in once its pid files are there, then lets it go.
    fn prove(&self) {
        let mut proven = self.proven.lock().unwrap();
        if !proven.is_empty() {
            return;
        }
        let read = |name: &str| {
            std::fs::read_to_string(self.world.home().join(name))
                .ok()
                .and_then(|t| t.trim().parse::<i32>().ok())
        };
        let Some(pid) = read("pid") else {
            return;
        };
        let member = if self.group {
            let Some(member) = read("member") else {
                return;
            };
            Some(member)
        } else {
            None
        };
        proven.push(Born::of(pid).expect("the stand-in runs"));
        if let Some(member) = member {
            proven.push(Born::of(member).expect("its sleep runs"));
        }
        std::fs::write(self.world.home().join("go"), "").expect("go");
        if let Some((sleeper, slept)) = self.sleeps {
            sleeper.slept(slept);
        }
    }
}

impl Person for Stub<'_> {
    fn say(&self, line: &str) {
        if line.contains("browser window opened") {
            std::fs::write(self.world.home().join("told"), "").expect("told");
        }
        self.said.lock().unwrap().push(line.to_owned());
    }

    fn paste(
        &self,
        prompt: &str,
        cancelled: &AtomicBool,
    ) -> std::io::Result<Option<Zeroizing<Vec<u8>>>> {
        assert_eq!(prompt, PASTE_PROMPT);
        self.pasted_asks.store(true, Ordering::SeqCst);
        if let Some(code) = self.code.lock().unwrap().take() {
            return Ok(Some(code));
        }
        let until = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        while !cancelled.load(Ordering::SeqCst) {
            assert!(Instant::now() < until, "the paste was never cancelled");
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(None)
    }

    fn stopped(&self) -> bool {
        self.prove();
        self.stops
            && self
                .said
                .lock()
                .unwrap()
                .iter()
                .any(|line| line.contains("browser window opened"))
    }
}

/// Asserts each process `stub` proved is gone, and no scratch folder is left.
#[track_caller]
fn ended_and_swept(world: &World, stub: &Stub<'_>) {
    let proven = stub.proven();
    assert_eq!(
        proven.len(),
        if stub.group { 2 } else { 1 },
        "the stand-in was proven"
    );
    for born in proven {
        assert!(born.gone_within_a_minute(), "{} still runs", born.pid);
    }
    assert_eq!(world.scratch_left(), Vec::<String>::new());
}

/// No line the capture said holds any byte of the authorize URL's own.
#[track_caller]
fn no_url_said(stub: &Stub<'_>) {
    for line in stub.said() {
        for part in [
            "STANDIN",
            "oauth/authorize",
            "code_challenge",
            "client_id",
            "https://",
        ] {
            assert!(!line.contains(part), "said {line:?}");
        }
    }
}

/// A STAND-IN THAT MAKES A TOKEN: the capture takes it, says the browser opened on claude.com
/// with the account to sign in as and never a byte of the URL, the child holds no route,
/// credential, browser or aterm variable, and the scratch folder is gone.
#[test]
fn a_stand_in_that_makes_a_token_gives_it() {
    let world = World::new("success", false);
    let stub = Stub::new(&world, false);
    let minted = world
        .capture(&stub, WakeDeadline::after(super::super::login_drive::HANG))
        .expect("a token");
    assert_eq!(minted.token.as_slice(), token().as_bytes());
    assert_eq!(
        minted.expiry().map(|(_, from)| from),
        Some(ExpiryFrom::Printed)
    );
    ended_and_swept(&world, &stub);
    no_url_said(&stub);
    assert!(
        stub.said()
            .iter()
            .any(|l| l.contains("claude.com") && l.contains(EMAIL)),
        "{:?}",
        stub.said()
    );
    let env = std::fs::read_to_string(world.home().join("env")).expect("the child's env");
    for kv in env.lines() {
        let name = kv.split_once('=').map_or(kv, |(k, _)| k);
        assert!(
            !super::super::netwatch::ROUTE_KEYS.contains(&name),
            "{name}"
        );
        assert!(
            !super::super::accounts::CREDENTIAL_KEYS.contains(&name),
            "{name}"
        );
        assert!(name != "BROWSER" && !name.starts_with("ATERM"), "{name}");
    }
    assert!(env.lines().any(|kv| kv == "TERM=dumb"), "{env}");
    let config = env
        .lines()
        .find_map(|kv| kv.strip_prefix("CLAUDE_CONFIG_DIR="))
        .expect("CLAUDE_CONFIG_DIR");
    assert!(Path::new(config).starts_with(&world.store), "{config}");
}

/// THE PASTED CODE REACHES THE STAND-IN'S TERMINAL: the person's code, handed over in a buffer
/// that wipes itself, is what the stand-in read at its paste prompt, and the token follows.
#[test]
fn the_pasted_code_reaches_the_stand_in() {
    let world = World::new("paste", false);
    let stub = Stub::new(&world, false);
    *stub.code.lock().unwrap() = Some(Zeroizing::new(b"the-code#the-state".to_vec()));
    let minted = world
        .capture(&stub, WakeDeadline::after(super::super::login_drive::HANG))
        .expect("a token");
    assert_eq!(minted.token.as_slice(), token().as_bytes());
    assert_eq!(
        std::fs::read_to_string(world.home().join("pasted")).expect("what it read"),
        "the-code#the-state"
    );
    ended_and_swept(&world, &stub);
}

/// A capture whose page opens in aterm's sign-in window: the stub proves the stand-in and keeps
/// what was said, and the window's waiter answers the paste and says whether the opener failed.
struct Windowed<'a> {
    stub: &'a Stub<'a>,
    waiter: WindowWaiter<'a>,
    /// Written to the route's outcome file once the capture says the window opened.
    fails_with: Option<(&'a Path, &'a str)>,
}

impl Person for Windowed<'_> {
    fn say(&self, line: &str) {
        if line.contains("sign-in window opened")
            && let Some((outcome, record)) = self.fails_with
        {
            std::fs::write(outcome, record).expect("the opener's record");
        }
        self.stub.say(line);
    }
    fn paste(
        &self,
        prompt: &str,
        cancelled: &AtomicBool,
    ) -> std::io::Result<Option<Zeroizing<Vec<u8>>>> {
        self.waiter.paste(prompt, cancelled)
    }
    fn stopped(&self) -> bool {
        self.stub.stopped()
    }
    fn failed(&self) -> Option<String> {
        self.waiter.failed()
    }
}

fn window_route(world: &World) -> WindowRoute {
    WindowRoute {
        opener: PathBuf::from("/nonexistent/aterm-open-url"),
        outcome: world.home().join("outcome"),
        code: world.home().join("outcome.code"),
    }
}

/// IN THE SIGN-IN WINDOW NOBODY IS ASKED FOR THE CODE: a hosted flow's code, which the window's
/// opener hands over in the capture's own file, is taken once and typed at the stand-in's
/// paste prompt, and the token follows. The child is told its page opens through aterm's
/// opener with the window's grant and the account to sign in as; a capture with no window
/// hands it none of that.
#[test]
fn a_window_capture_types_the_code_its_opener_handed_over() {
    let world = World::new("paste", false);
    let stub = Stub::new(&world, false);
    let route = window_route(&world);
    std::fs::write(&route.code, "the-code#the-state").expect("the opener's code");
    let person = Windowed {
        stub: &stub,
        waiter: WindowWaiter::new(&route),
        fails_with: None,
    };
    let program = World::program();
    let env = world.env(&[]);
    let minted = capture(
        &Capture {
            program: &program,
            store: &world.store,
            sign_in_as: EMAIL,
            env: &env,
            window: Some(&route),
            deadline: WakeDeadline::after(super::super::login_drive::HANG),
        },
        &person,
    )
    .expect("a token");
    assert_eq!(minted.token.as_slice(), token().as_bytes());
    assert_eq!(
        std::fs::read_to_string(world.home().join("pasted")).expect("what it read"),
        "the-code#the-state"
    );
    assert!(!route.code.exists(), "the code is taken once");
    assert!(
        stub.said().iter().any(
            |line| line.contains("aterm's sign-in window opened") && line.contains("by itself")
        ),
        "{:?}",
        stub.said()
    );
    ended_and_swept(&world, &stub);

    let trace = |name: &str| (name == "ATERM_SIGN_IN_TRACE").then(|| OsString::from("/t/trace"));
    let held: Vec<(&str, OsString)> = window_env(&route, EMAIL, &trace);
    let value = |name: &str| {
        held.iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.to_string_lossy().into_owned())
    };
    assert_eq!(
        value("BROWSER").as_deref(),
        Some("/nonexistent/aterm-open-url")
    );
    assert_eq!(
        value(super::super::open_url::WINDOW_ENV).as_deref(),
        Some("1")
    );
    assert_eq!(
        value(super::super::open_url::EMAIL_ENV).as_deref(),
        Some(EMAIL)
    );
    assert_eq!(
        value(super::super::open_url::CODE_ENV),
        Some(route.code.to_string_lossy().into_owned())
    );
    assert_eq!(
        value(super::super::open_url::OUTCOME_ENV),
        Some(route.outcome.to_string_lossy().into_owned())
    );
    #[cfg(target_os = "macos")]
    assert_eq!(value("ATERM_SIGN_IN_TRACE").as_deref(), Some("/t/trace"));
    // CONTROL: the child of a capture with no window is handed no opener and no grant.
    let plain = child_env(&|_| None, Path::new("/scratch"));
    assert!(
        !plain
            .iter()
            .any(|(key, _)| *key == "BROWSER" || *key == super::super::open_url::WINDOW_ENV)
    );
}

/// A SIGN-IN THE WINDOW'S OPENER ENDED WITHOUT AUTHORIZING ENDS THE CAPTURE WITH WHAT IT MET:
/// the stand-in, which would wait for ever, is ended, and the error is the opener's own closed
/// words, never a byte the child wrote.
#[test]
fn a_window_capture_ends_with_what_its_opener_met() {
    let world = World::new("wait", true);
    let stub = Stub::new(&world, true);
    let route = window_route(&world);
    let person = Windowed {
        stub: &stub,
        waiter: WindowWaiter::new(&route),
        fails_with: Some((
            &route.outcome,
            "opener-outcome=1\nphase=failed\nmet=window-bot-check claude.ai\n",
        )),
    };
    let program = World::program();
    let env = world.env(&[]);
    let error = capture(
        &Capture {
            program: &program,
            store: &world.store,
            sign_in_as: EMAIL,
            env: &env,
            window: Some(&route),
            deadline: WakeDeadline::after(super::super::login_drive::HANG),
        },
        &person,
    )
    .expect_err("no token");
    let Error::Window(why) = &error else {
        panic!("{error:?}");
    };
    assert!(why.contains("Cloudflare's check on claude.ai"), "{why}");
    assert!(error.to_string().contains("nothing was made"));
    ended_and_swept(&world, &stub);
    // A person's browser says no such thing.
    assert_eq!(stub.failed(), None);
}

/// A FAILURE ENDS THE GROUP AND HOLDS NO CHILD BYTE: the stand-in's exchange failure (with a
/// sentinel in it) is `Exchange`, whose words lack the sentinel; the stand-in and its group's
/// `sleep` are gone.
#[test]
fn a_failure_ends_the_group_and_says_nothing_of_the_child() {
    let world = World::new("failure", true);
    let stub = Stub::new(&world, true);
    let error = world
        .capture(&stub, WakeDeadline::after(super::super::login_drive::HANG))
        .expect_err("no token");
    assert_eq!(error, Error::Failed(Failure::Exchange));
    let words = format!("{error} {error:?} {:?}", stub.said());
    assert!(!words.contains("SENTINEL"), "{words}");
    ended_and_swept(&world, &stub);
}

/// THE AUTHORIZE URL ENDS THE GROUP: a stand-in that opens another host, or asks a wider scope,
/// is ended (with its group) and nothing was asked of the person.
#[test]
fn a_request_for_anything_else_ends_the_group() {
    for mode in ["bad-host", "wide-scope"] {
        let world = World::new(mode, true);
        let stub = Stub::new(&world, true);
        let error = world
            .capture(&stub, WakeDeadline::after(super::super::login_drive::HANG))
            .expect_err("no token");
        assert_eq!(error, Error::Request, "{mode}");
        ended_and_swept(&world, &stub);
        no_url_said(&stub);
        assert!(
            !stub.pasted_asks.load(Ordering::SeqCst),
            "{mode}: the person was asked"
        );
    }
}

/// A LONG SILENCE MEETS THE HANG DETECTOR, on the clock that counts sleep: the stand-in says
/// nothing, the host "sleeps" past HANG (the injected clock), and the capture ends it as `Hang`.
#[test]
fn a_long_silence_meets_the_hang_detector() {
    let world = World::new("silent", true);
    let seam = SleepSeam::hold();
    let deadline = WakeDeadline::after(super::super::login_drive::HANG);
    let mut stub = Stub::new(&world, true);
    stub.sleeps = Some((
        seam.sleeper(),
        super::super::login_drive::HANG + Duration::from_secs(1),
    ));
    assert_eq!(
        world.capture(&stub, deadline).expect_err("no token"),
        Error::Hang
    );
    ended_and_swept(&world, &stub);
}

/// CTRL-C ENDS IT: once the browser opened and the paste prompt is up, the person's stop ends
/// the stand-in and its group, cancels the prompt, and leaves no scratch folder.
#[test]
fn ctrl_c_ends_the_group() {
    let world = World::new("wait", true);
    let mut stub = Stub::new(&world, true);
    stub.stops = true;
    assert_eq!(
        world
            .capture(&stub, WakeDeadline::after(super::super::login_drive::HANG))
            .expect_err("no token"),
        Error::Stopped
    );
    ended_and_swept(&world, &stub);
}

/// NOISE PAST THE BUFFER ENDS IT: 256 KiB and more of output with no verdict is `Output`.
#[test]
fn noise_past_the_buffer_ends_it() {
    let world = World::new("noise", true);
    let stub = Stub::new(&world, true);
    assert_eq!(
        world
            .capture(&stub, WakeDeadline::after(super::super::login_drive::HANG))
            .expect_err("no token"),
        Error::Output
    );
    ended_and_swept(&world, &stub);
}

/// A ROUTE IN THE VERB'S ENVIRONMENT IS REFUSED BEFORE ANY SPAWN: no store folder, no scratch
/// folder and no stand-in, and the refusal names the variable, never its value.
#[test]
fn a_route_is_refused_before_any_spawn() {
    for (key, value) in [
        ("HTTPS_PROXY", "http://gw.example:3128"),
        ("ANTHROPIC_BASE_URL", "https://gw.example"),
    ] {
        let world = World::new("success", false);
        let program = World::program();
        let env = world.env(&[(key, value)]);
        let stub = Stub::new(&world, false);
        let error = capture(
            &Capture {
                program: &program,
                store: &world.store,
                sign_in_as: EMAIL,
                env: &env,
                window: None,
                deadline: WakeDeadline::after(super::super::login_drive::HANG),
            },
            &stub,
        )
        .expect_err("refused");
        assert_eq!(error, Error::Route(key.to_owned()));
        let words = error.to_string();
        assert!(
            words.contains(key) && !words.contains("gw.example"),
            "{words}"
        );
        assert!(!world.store.exists(), "the store was made");
        assert!(!world.home().join("pid").exists(), "the stand-in ran");
    }
}

/// THE VERB, KILLED: this test binary as a verb capturing a waiting stand-in (a no-op in a plain
/// test run).
#[test]
fn verb_stand_in() {
    let Some(home) = std::env::var_os(VERB_HOME).map(PathBuf::from) else {
        return;
    };
    struct Waits;
    impl Person for Waits {
        fn say(&self, _line: &str) {}
        fn paste(
            &self,
            _prompt: &str,
            cancelled: &AtomicBool,
        ) -> std::io::Result<Option<Zeroizing<Vec<u8>>>> {
            while !cancelled.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(None)
        }
        fn stopped(&self) -> bool {
            false
        }
    }
    let program = World::program();
    let mut env: Vec<(OsString, OsString)> = vec![("HOME".into(), home.clone().into_os_string())];
    if let Some(tmp) = std::env::var_os("TMPDIR") {
        env.push(("TMPDIR".into(), tmp));
    }
    let _ = capture(
        &Capture {
            program: &program,
            store: &home.join("store"),
            sign_in_as: EMAIL,
            env: &env,
            window: None,
            deadline: WakeDeadline::after(aterm_spec::wait::MUST_HAPPEN),
        },
        &Waits,
    );
}

/// A VERB KILLED BY SIGKILL TAKES ITS CHILD WITH IT AND ITS FOLDER IS SWEPT NEXT RUN: the
/// stand-in is gone within a minute of the verb's death, its terminal hung up; the verb's
/// scratch folder is left, and the next capture under that store removes it. CONTROL: the
/// folder was there after the kill, so the sweep is what removed it. (The hangup reaches the
/// session's leader: a member of its group that outlives the leader is the leader's to end, as
/// Claude Code's shutdown ends its own.)
#[test]
fn a_killed_verbs_child_ends_and_its_folder_is_swept_next_run() {
    let world = World::new("wait", false);
    let mut verb = aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary"));
    verb.args([
        "--exact",
        "harness::setup_token::tests::verb_stand_in",
        "--nocapture",
        "--test-threads=1",
        "--quiet",
    ])
    .env(VERB_HOME, world.home())
    .stdin(aterm_uds::spawnfd::Stdio::null())
    .stdout(aterm_uds::spawnfd::Stdio::null())
    .stderr(aterm_uds::spawnfd::Stdio::null());
    let mut verb = verb.spawn().expect("the verb");
    let stub = Stub::new(&world, false);
    let until = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while stub.proven().is_empty() {
        assert!(Instant::now() < until, "the stand-in never started");
        stub.prove();
        std::thread::sleep(Duration::from_millis(10));
    }
    let left = world.scratch_left();
    assert_eq!(left.len(), 1, "the verb's scratch folder: {left:?}");
    let _ = verb.kill();
    let _ = verb.wait();
    for born in stub.proven() {
        assert!(
            born.gone_within_a_minute(),
            "{} outlived its verb",
            born.pid
        );
    }
    assert_eq!(
        world.scratch_left(),
        left,
        "the killed verb's folder is left"
    );
    std::fs::write(world.home().join(MODE_FILE), "failure\n").expect("the next mode");
    for name in ["pid", "member", "go"] {
        let _ = std::fs::remove_file(world.home().join(name));
    }
    let next = Stub::new(&world, false);
    assert_eq!(
        world
            .capture(&next, WakeDeadline::after(super::super::login_drive::HANG))
            .expect_err("no token"),
        Error::Failed(Failure::Exchange)
    );
    ended_and_swept(&world, &next);
}

/// Set when this test binary reads a code unshown at its terminal ([`unshown_reader`]): where
/// it writes what it read.
const UNSHOWN_AT: &str = "ATERM_SETUP_TOKEN_TEST_UNSHOWN";

/// THE ZEROIZING UNSHOWN READ, in a child whose controlling terminal is a pty (a no-op in a
/// plain test run): a read already cancelled gives nothing, then the typed line is read.
#[test]
fn unshown_reader() {
    let Some(at) = std::env::var_os(UNSHOWN_AT).map(PathBuf::from) else {
        return;
    };
    let cancelled = AtomicBool::new(true);
    let none = crate::fleet_member::read_unshown_secret("first: ", &cancelled).expect("a read");
    assert!(none.is_none(), "a cancelled read gave a line");
    let open = AtomicBool::new(false);
    let line = crate::fleet_member::read_unshown_secret("code: ", &open)
        .expect("a read")
        .expect("a line");
    let line: &Zeroizing<Vec<u8>> = &line;
    std::fs::write(&at, &line[..]).expect("what it read");
}

/// THE PASTED CODE IS READ UNSHOWN INTO A BUFFER THAT WIPES ITSELF: a child reading at its own
/// terminal (`/dev/tty`, a pty here) gets the code typed there, without its newline, and the
/// terminal never echoes it.
#[test]
fn the_code_is_read_unshown_from_the_terminal() {
    use std::os::fd::AsRawFd as _;
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-setup-token-unshown-")
        .suffix(".noindex")
        .tempdir()
        .expect("a folder");
    let at = dir.path().join("got");
    let (master, slave, mut child) =
        aterm_pty::spawn_session_leader_on_pty_sized(25, 512, |slave| {
            let tty = || slave.try_clone().map(std::process::Stdio::from);
            #[allow(
                clippy::disallowed_types,
                reason = "the pty spawn seam takes StdCommand"
            )]
            let mut cmd = std::process::Command::new(std::env::current_exe()?);
            cmd.args([
                "--exact",
                "harness::setup_token::tests::unshown_reader",
                "--nocapture",
                "--test-threads=1",
                "--quiet",
            ])
            .env(UNSHOWN_AT, &at)
            .stdout(tty()?)
            .stderr(tty()?);
            Ok(cmd)
        })
        .expect("the reader starts");
    let mut master = std::fs::File::from(master);
    aterm_pty::set_nonblocking(master.as_raw_fd(), true).expect("nonblocking");
    let mut seen = Vec::new();
    let until = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let read_some = |seen: &mut Vec<u8>, master: &mut std::fs::File| {
        let mut buf = [0u8; 1024];
        match std::io::Read::read(master, &mut buf) {
            Ok(n) if n > 0 => seen.extend_from_slice(&buf[..n]),
            _ => std::thread::sleep(Duration::from_millis(10)),
        }
    };
    while find(&seen, b"code: ").is_none() {
        assert!(
            Instant::now() < until,
            "no prompt: {:?}",
            String::from_utf8_lossy(&seen)
        );
        read_some(&mut seen, &mut master);
    }
    std::io::Write::write_all(&mut master, b"the-code#the-state\r").expect("typed");
    while !at.exists() {
        assert!(
            Instant::now() < until,
            "nothing read: {:?}",
            String::from_utf8_lossy(&seen)
        );
        read_some(&mut seen, &mut master);
    }
    // Read on while it exits: on Apple a session leader's exit waits for its terminal's output
    // to drain.
    let status = loop {
        if let Some(status) = child.try_wait().expect("the reader's state") {
            break status;
        }
        assert!(Instant::now() < until, "the reader never ended");
        read_some(&mut seen, &mut master);
    };
    drop(slave);
    assert!(status.success(), "{status}");
    assert_eq!(
        std::fs::read(&at).expect("what it read"),
        b"the-code#the-state"
    );
    assert!(
        find(&seen, b"the-code").is_none(),
        "echoed: {:?}",
        String::from_utf8_lossy(&seen)
    );
}
