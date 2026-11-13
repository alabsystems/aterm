// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`parse`] against the recorded `usage/refresh`/`usage/read` answer shapes (Muse
//! 1.4.13, values invented): the fresh read, the absent usage, and out-of-range figures.

use std::io::Cursor;
use std::sync::{Arc, Mutex};

use super::AppServer;
use super::*;
use aterm_json::json;

/// A refresh answer: plan words and a dated weekly figure.
#[test]
fn a_refresh_answer_reads_its_plan_and_weekly_figure() {
    let got = parse(&json!({
        "subscriptionActive": true,
        "tierName": "Muse Code Max Usage",
        "usage": {
            "observedAtMs": 1793061227023_i64,
            "tier": "99",
            "weekly": {"resetsAtMs": 1793577600000_i64, "usedPercent": 46},
            "window": {"resetsAtMs": 1793063035000_i64, "usedPercent": 36, "windowDurationMins": 300}
        }
    }))
    .expect("a reading");
    assert_eq!(got.plan.as_deref(), Some("Muse Code Max Usage"));
    assert_eq!(got.weekly_used_x10, Some(460));
    assert_eq!(got.weekly_resets_at, Some(1_793_577_600));
    assert_eq!(got.observed_at, Some(1_793_061_227));
}

/// A read answer (no tierName) still gives its figure.
#[test]
fn a_read_answer_without_a_plan_gives_its_figure() {
    let got = parse(&json!({
        "usage": {
            "observedAtMs": 1793061227023_i64,
            "weekly": {"resetsAtMs": 1793577600000_i64, "usedPercent": 9}
        }
    }))
    .expect("a reading");
    assert_eq!(got.plan, None);
    assert_eq!(got.weekly_used_x10, Some(90));
}

/// Absent usage keeps only the plan words a mint named; with neither, nothing.
#[test]
fn an_absent_usage_keeps_only_named_plan_words() {
    let got = parse(&json!({"subscriptionActive": true, "tierName": "Muse Code Max Usage"}))
        .expect("plan words alone");
    assert_eq!(got.weekly_used_x10, None);
    assert_eq!(parse(&json!({})), None);
    assert_eq!(parse(&json!({"subscriptionActive": false})), None);
}

/// An over-quota percent reads whole and clamps to the words' range; a missing or
/// undated weekly half is no figure.
#[test]
fn figures_outside_their_halves_do_not_read() {
    let over = parse(&json!({
        "usage": {"weekly": {"resetsAtMs": 1793577600000_i64, "usedPercent": 137}}
    }))
    .expect("over-quota reads");
    assert_eq!(over.weekly_used_x10, Some(1000));
    assert_eq!(
        parse(&json!({"usage": {"weekly": {"usedPercent": 10}}})),
        None,
        "undated"
    );
    assert_eq!(
        parse(&json!({"usage": {"weekly": {"resetsAtMs": 1793577600000_i64, "usedPercent": -1}}})),
        None,
        "negative"
    );
}

/// A writer that keeps every line it is handed, for the wire below.
#[derive(Clone, Default)]
struct Sink(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn dialogue(answers: &str) -> (Option<super::Reading>, Vec<String>) {
    let (got, methods) = dialogue_read(answers);
    (got.ok(), methods)
}

fn dialogue_read(answers: &str) -> (Result<super::Reading, Missed>, Vec<String>) {
    let sink = Sink::default();
    let server = AppServer {
        from: Box::new(Cursor::new(answers.as_bytes().to_vec())),
        to: Box::new(sink.clone()),
    };
    let got = read_usage(server, Instant::now() + aterm_spec::wait::MUST_HAPPEN);
    let sent = String::from_utf8(
        sink.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone(),
    )
    .expect("utf8 wire");
    let methods = sent
        .lines()
        .map(|line| {
            let message: aterm_json::Value = aterm_json::from_str(line).expect("a message");
            message
                .get("method")
                .and_then(aterm_json::Value::as_str)
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    (got, methods)
}

/// The dialogue stamps `jsonrpc` (Muse's MSP refuses a message without it), opts into
/// the experimental API, and reads the refresh back into a figure.
#[test]
fn the_dialogue_stamps_jsonrpc_and_reads_the_refresh() {
    let answers = concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"serverInfo\":{\"name\":\"muse\",\"version\":\"1.4.13\"}}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{",
        "\"subscriptionActive\":true,\"tierName\":\"Muse Code Max Usage\",",
        "\"usage\":{\"observedAtMs\":1793061227023,",
        "\"weekly\":{\"resetsAtMs\":1793577600000,\"usedPercent\":46}}}}\n"
    );
    let sink = Sink::default();
    let server = AppServer {
        from: Box::new(Cursor::new(answers.as_bytes().to_vec())),
        to: Box::new(sink.clone()),
    };
    let got =
        read_usage(server, Instant::now() + aterm_spec::wait::MUST_HAPPEN).expect("a reading");
    assert_eq!(got.weekly_used_x10, Some(460));
    assert_eq!(got.plan.as_deref(), Some("Muse Code Max Usage"));
    let sent = String::from_utf8(
        sink.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone(),
    )
    .expect("utf8 wire");
    let lines: Vec<&str> = sent.lines().collect();
    assert_eq!(lines.len(), 3, "{sent}");
    for line in &lines {
        let message: aterm_json::Value = aterm_json::from_str(line).expect("a message");
        assert_eq!(
            message.get("jsonrpc").and_then(aterm_json::Value::as_str),
            Some("2.0"),
            "{line}"
        );
    }
    assert!(
        lines[0].contains("\"experimentalApi\":true"),
        "{}",
        lines[0]
    );
    assert!(lines[1].contains("\"initialized\""), "{}", lines[1]);
    assert!(lines[2].contains("\"usage/refresh\""), "{}", lines[2]);
}

/// A mint that names a plan but carries no weekly figure does not mask the host's
/// complete last-observed read, and the fresher mint names the merged reading.
#[test]
fn a_plan_only_refresh_falls_back_to_a_complete_read() {
    let (got, methods) = dialogue(concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"serverInfo\":{\"name\":\"muse\"}}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{",
        "\"subscriptionActive\":true,\"tierName\":\"Muse Code Max Usage\"}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"usage\":{",
        "\"observedAtMs\":1793061227023,",
        "\"weekly\":{\"resetsAtMs\":1793577600000,\"usedPercent\":46}}}}\n"
    ));
    let got = got.expect("the complete read");
    assert_eq!(got.weekly_used_x10, Some(460));
    assert_eq!(got.weekly_resets_at, Some(1_793_577_600));
    assert_eq!(got.plan.as_deref(), Some("Muse Code Max Usage"));
    assert_eq!(
        methods,
        ["initialize", "initialized", "usage/refresh", "usage/read"]
    );
}

/// A refused mint reads the host's last observation, as before.
#[test]
fn a_refused_refresh_falls_back_to_the_last_observed_read() {
    let (got, methods) = dialogue(concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"serverInfo\":{\"name\":\"muse\"}}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"error\":{\"code\":-32080,",
        "\"message\":\"usage/refresh is rate limited\"}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"usage\":{",
        "\"observedAtMs\":1793061227023,",
        "\"weekly\":{\"resetsAtMs\":1793577600000,\"usedPercent\":46}}}}\n"
    ));
    let got = got.expect("the last-observed read");
    assert_eq!(got.weekly_used_x10, Some(460));
    assert_eq!(
        methods,
        ["initialize", "initialized", "usage/refresh", "usage/read"]
    );
}

/// An explicitly inactive subscription is final: stale tier and usage words in
/// the same answer name no reading, and no last-observed read is even asked.
#[test]
fn an_inactive_refresh_never_falls_back_to_a_stale_read() {
    let stale = json!({
        "subscriptionActive": false,
        "tierName": "Muse Code Max Usage",
        "usage": {
            "observedAtMs": 1_793_061_227_023_i64,
            "weekly": {"usedPercent": 46, "resetsAtMs": 1_793_577_600_000_i64}
        }
    });
    assert_eq!(parse(&stale), None, "inactive outranks stale words");
    let (got, methods) = dialogue(concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"serverInfo\":{\"name\":\"muse\"}}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{",
        "\"subscriptionActive\":false,\"tierName\":\"Muse Code Max Usage\",",
        "\"usage\":{\"observedAtMs\":1793061227023,",
        "\"weekly\":{\"resetsAtMs\":1793577600000,\"usedPercent\":46}}}}\n"
    ));
    assert_eq!(got, None);
    assert_eq!(methods, ["initialize", "initialized", "usage/refresh"]);
}

/// A host that refuses in Muse Code's sign-in words is a login that is gone, told apart from a
/// read that merely gave nothing, whichever call it refuses.
/// NEGATIVE CONTROL: with `Rpc::call` answering `Fault::Unread` for every error (its words
/// dropped), the sign-out reads as nothing: `` left: Err(Unanswered) right: Err(SignedOut) ``.
#[test]
fn a_refusal_in_sign_in_words_is_a_signed_out_login() {
    let hello = "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"serverInfo\":{\"name\":\"muse\"}}}\n";
    let refused = |id: u8, said: &str| {
        format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"error\":{{\"code\":-32001,\"message\":\"{said}\"}}}}\n"
        )
    };
    let signed_out = format!(
        "{hello}{}{}",
        refused(2, "not logged in: run `muse login` first"),
        refused(3, "not logged in: run `muse login` first"),
    );
    assert_eq!(dialogue_read(&signed_out).0, Err(Missed::SignedOut));
    let at_hello = refused(1, "No Meta credentials were found");
    assert_eq!(dialogue_read(&at_hello).0, Err(Missed::SignedOut));
    let expired = format!(
        "{hello}{}{}",
        refused(2, "usage/refresh is rate limited"),
        refused(
            3,
            "your saved login is no longer valid. Log in again or use a different account."
        ),
    );
    assert_eq!(dialogue_read(&expired).0, Err(Missed::SignedOut));
    // CONTROL: refusals that say nothing of the login, and a host that just closes.
    let limited = format!(
        "{hello}{}{}",
        refused(2, "usage/refresh is rate limited"),
        refused(3, "no usage was observed yet"),
    );
    assert_eq!(dialogue_read(&limited).0, Err(Missed::Unanswered));
    assert_eq!(dialogue_read("").0, Err(Missed::Unanswered));
    assert_eq!(
        dialogue_read(&refused(1, "macOS Keychain access was canceled")).0,
        Err(Missed::Unanswered)
    );
}

/// The read's host runs under the no-desk sandbox: the command is `sandbox-exec` with the
/// profile, then the binary and its `serve` words.
#[cfg(target_os = "macos")]
#[test]
fn the_serve_command_is_the_no_desk_sandbox_around_the_host() {
    use super::super::login_drive::{NO_DESK, SANDBOX_EXEC};
    let cmd = serve_command(Path::new("/opt/muse/bin/muse")).expect("a sandbox here");
    assert_eq!(cmd.get_program(), std::ffi::OsStr::new(SANDBOX_EXEC));
    let args: Vec<String> = cmd
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        args,
        [
            "-p",
            NO_DESK,
            "/opt/muse/bin/muse",
            "serve",
            "--no-session-log",
            "--provider",
            "meta"
        ]
    );
}

/// THE FOURTH FINDING: a signed-out `muse serve` can put nothing on the screen, and what it
/// says of its login is heard. The stand-in host tries to start `/usr/bin/open` (with no
/// arguments, which opens nothing and prints its usage), writes how that ended, says Muse
/// Code's own sign-in words on its standard error and exits: under the sandbox the start is
/// refused (`126`), and the read comes back signed out.
/// NEGATIVE CONTROL (`ON_DESK`: the host run as a plain child, as before): `open` starts and
/// exits `1`: `` left: "open=1" right: "open=126" ``.
#[cfg(target_os = "macos")]
#[test]
fn a_signed_out_host_runs_off_the_desk_and_is_heard() {
    use super::super::provider_read::{Account, Read, Use};
    let mut tmp = aterm_uds::deadline::KeptUntilRan::new(
        aterm_tempfile::Builder::new()
            .prefix("aterm-muse-usage-nodesk-")
            .tempdir()
            .expect("temp dir")
            .keep(),
    );
    let home = tmp.path().join("home");
    let config = home.join(".config/muse");
    std::fs::create_dir_all(&config).expect("config");
    let marker = tmp.path().join("open-ended");
    let host = tmp.path().join("fake-muse.sh");
    crate::harness::stand_in::proven(
        &host,
        &format!(
            "/usr/bin/open >/dev/null 2>&1\necho \"open=$?\" > '{}'\n\
             echo 'error: not logged in: run `muse login` first' >&2\nexit 1\n",
            marker.display()
        ),
    );
    let read = |on_desk: bool| {
        let _ = std::fs::remove_file(&marker);
        ON_DESK.set(on_desk);
        let permit = Use::fixture(Read::MuseUsage, &config, None)
            .begin(Account::in_home(Read::MuseUsage, &config, None), 1, None)
            .expect("its own account");
        let got = fetch(
            &host,
            vec![
                ("PATH", std::ffi::OsString::from("/usr/bin:/bin")),
                ("HOME", home.clone().into_os_string()),
            ],
            tmp.path(),
            permit,
        );
        ON_DESK.set(false);
        (
            got,
            std::fs::read_to_string(&marker)
                .unwrap_or_default()
                .trim()
                .to_owned(),
        )
    };
    let (got, open) = read(false);
    assert_eq!(open, "open=126", "the sandbox refuses the opener's start");
    assert_eq!(got, Err(Missed::SignedOut), "its sign-in words are heard");
    let (got, open) = read(true);
    assert_eq!(open, "open=1", "CONTROL: a plain child starts it");
    assert_eq!(got, Err(Missed::SignedOut));
    tmp.ran();
}
