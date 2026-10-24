// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the vendor sign-in driver: the line grammars over recorded transcripts
//! (`fixtures/signin/`: Kimi Code 2.3.1's `login --region global` and Codex 0.164.0's
//! `login --device-auth`, recorded in a scratch home and cancelled, their codes fabricated; Grok
//! Build 1.0.67's `login --device-auth`, written from its source, its email at example.com), the
//! host pins, the child's environment and the driven child itself (this test binary run in place
//! as the CLI).

use std::cell::RefCell;
use std::ffi::OsString;

use super::*;
use crate::ai::connector::{CODEX_SIGNIN, GROK_SIGNIN, Hosts, KIMI_SIGNIN};

const KIMI: &str = include_str!("fixtures/signin/kimi-2.3.1-login-global.stderr");
const GROK: &str = include_str!("fixtures/signin/grok-1.0.67-device-auth.stderr");
const CODEX: &str = include_str!("fixtures/signin/codex-0.164.0-device-auth.stdout");

fn lines_grammar(signin: &SignIn, flow: Flow) -> (&'static Grammar, Completion, &'static Hosts) {
    let route = signin.route(Some(flow)).expect("the route");
    match &route.drive {
        Drive::Lines {
            grammar,
            completion,
            ..
        } => (grammar, *completion, &route.hosts),
        Drive::AppServer => panic!("a line route"),
    }
}

fn scanned(grammar: &Grammar, transcript: &str) -> Scan {
    let mut scan = Scan::default();
    for line in transcript.split('\n') {
        scan.feed(grammar, line);
    }
    scan
}

#[test]
fn each_vendors_transcript_names_its_page_and_code_and_says_it_is_done() {
    let (kimi, completion, hosts) = lines_grammar(&KIMI_SIGNIN, Flow::Device);
    assert_eq!(completion, Completion::Device);
    let scan = scanned(kimi, KIMI);
    let url = scan.url.as_deref().expect("kimi's url");
    assert!(
        url.starts_with("https://www.kimi.ai/code/authorize_device?"),
        "{url}"
    );
    assert_eq!(scan.code.as_deref(), Some("QXR4-MZK7"));
    assert!(scan.done && scan.email.is_none(), "{scan:?}");
    let global = hosts.of(Some(Region::Global)).expect("global hosts");
    assert!(pinned(url, global));

    let (grok, _, hosts) = lines_grammar(&GROK_SIGNIN, Flow::Device);
    let scan = scanned(grok, GROK);
    let url = scan.url.as_deref().expect("grok's url");
    assert_eq!(https_host(url).as_deref(), Some("accounts.x.ai"));
    assert!(pinned(url, hosts.of(None).expect("fixed")));
    assert_eq!(scan.code.as_deref(), Some("WMRT-8KQZ"));
    assert!(scan.done);
    assert_eq!(scan.email.as_deref(), Some("grok-one@example.com"));

    let (codex, _, hosts) = lines_grammar(&CODEX_SIGNIN, Flow::Device);
    let scan = scanned(codex, CODEX);
    assert_eq!(
        scan.url.as_deref(),
        Some("https://auth.openai.com/codex/device"),
        "the URL on the line after its lead, its colours stripped"
    );
    assert!(pinned(
        scan.url.as_deref().unwrap_or(""),
        hosts.of(None).expect("fixed")
    ));
    assert_eq!(scan.code.as_deref(), Some("K7QM-XP4RT"));
    assert!(!scan.done, "a cancelled run never said it signed in");
    let mut done = scan.clone();
    done.feed(codex, "Successfully logged in");
    assert!(done.done);
}

#[test]
fn a_kimi_row_takes_only_its_own_regions_hosts_and_an_off_pin_host_is_refused() {
    let (_, _, hosts) = lines_grammar(&KIMI_SIGNIN, Flow::Device);
    let global = hosts.of(Some(Region::Global)).expect("global");
    let mainland = hosts.of(Some(Region::MainlandCn)).expect("mainland");
    assert!(
        hosts.of(None).is_none(),
        "a row naming no region has no hosts"
    );
    let mainland_url = "https://auth.kimi.com/code/authorize_device?user_code=QXR4-MZK7";
    assert!(
        !pinned(mainland_url, global),
        "a global row refuses kimi.com"
    );
    assert!(pinned(mainland_url, mainland));
    for off in [
        "https://kimi.ai.example.com/device",
        "http://www.kimi.ai/code/authorize_device",
        "https://user@www.kimi.ai/code",
        "https://www.kimi.ai:8443/code",
        "https://www.kimi.ai\\@evil.example/",
        "https://www.kimi.ai /code",
    ] {
        assert!(!pinned(off, global), "{off}");
    }
    assert!(pinned("https://WWW.Kimi.AI:443/code", global));
}

#[test]
fn ansi_and_carriage_return_redraws_are_stripped() {
    assert_eq!(
        strip_ansi("\u{1b}[94mhttps://a.example/x\u{1b}[0m"),
        "https://a.example/x"
    );
    assert_eq!(
        strip_ansi("spinner\r\u{1b}[K\u{2713} Signed in"),
        "\u{2713} Signed in"
    );
    assert_eq!(
        strip_ansi("\u{1b}]8;;https://x\u{7}link\u{1b}]8;;\u{7}"),
        "link"
    );
}

#[test]
fn a_line_past_its_bound_is_cut_and_lines_past_theirs_are_drained_unread() {
    let long = format!("{}\nsecond\nthird\n", "x".repeat(10));
    let mut got = Vec::new();
    read_lines_within(long.as_bytes(), 4, 2, |l| got.push(l));
    assert_eq!(got, ["xxxx", "seco"]);
}

fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
    let owned: Vec<(String, OsString)> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), OsString::from(v)))
        .collect();
    move |name| {
        owned
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    }
}

#[test]
fn kimis_child_runs_with_telemetry_and_updates_off_in_its_staging_home_and_no_opener() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let staging = tmp.path().join("sol+1");
    let path = opener_free_path(&tmp.path().join("store")).expect("private PATH");
    assert_eq!(
        std::fs::read_dir(path.path()).expect("read").count(),
        0,
        "nothing, `open` least of all, is on its PATH"
    );
    let var = vars(&[
        ("HOME", "/Users//example"),
        ("PATH", "/usr/bin:/bin"),
        ("KIMI_CODE_IDENTITY_SLUG", "someone-else"),
        ("KIMI_CODE_HOME", "/Users//example/.kimi-code"),
        ("KIMI_API_KEY", "not-passed"),
        ("KIMI_CODE_USER_AGENT", "spoofed/1.0"),
        ("HTTP_USER_AGENT", "spoofed/1.0"),
        ("BROWSER", "/usr/bin/open"),
    ]);
    let env = child_env(
        Cli::Kimi,
        &KIMI_SIGNIN,
        &staging,
        path.path().as_os_str(),
        &var,
    );
    let get = |name: &str| {
        env.iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.to_string_lossy().into_owned())
    };
    assert_eq!(get("KIMI_DISABLE_TELEMETRY").as_deref(), Some("1"));
    assert_eq!(get("KIMI_CODE_NO_AUTO_UPDATE").as_deref(), Some("1"));
    assert_eq!(get("KIMI_CODE_HOME"), Some(staging.display().to_string()));
    assert_eq!(get("PATH"), Some(path.path().display().to_string()));
    assert_eq!(get("HOME").as_deref(), Some("/Users//example"));
    for never in [
        "KIMI_CODE_IDENTITY_SLUG",
        "KIMI_API_KEY",
        "BROWSER",
        "KIMI_CODE_USER_AGENT",
        "HTTP_USER_AGENT",
    ] {
        assert!(get(never).is_none(), "{never} never reaches the child");
    }
    assert!(
        !env.iter()
            .any(|(k, v)| k.to_ascii_uppercase().contains("USER_AGENT")
                || v.to_string_lossy().contains("spoofed")),
        "no User-Agent override: {env:?}"
    );
    assert_eq!(
        child_args(
            KIMI_SIGNIN.route(None).expect("route"),
            Some(Region::Global)
        ),
        ["login", "--region", "global"]
    );
}

/// The CLI as this test binary run in place: where its `KIMI_CODE_HOME` (which only the driver
/// hands it) holds a `.stand-in` script (`<transcript> <exit>`), it checks it was run as `kimi
/// login --region global` (after `--`), prints that transcript to its standard error, writes the
/// login file into that home when it exits 0, and exits; anywhere else it is an empty test.
#[test]
fn signin_stand_in() {
    let Some(which) = std::env::var_os("KIMI_CODE_HOME")
        .map(std::path::PathBuf::from)
        .and_then(|home| std::fs::read_to_string(home.join(".stand-in")).ok())
    else {
        return;
    };
    let home = std::path::PathBuf::from(std::env::var_os("KIMI_CODE_HOME").unwrap_or_default());
    let mut words = which.split_whitespace();
    let transcript = match words.next() {
        Some("kimi") => KIMI.to_owned(),
        Some("off-pin") => KIMI.replace("www.kimi.ai", "www.kimi.ai.example.com"),
        _ => String::new(),
    };
    let code: i32 = words.next().and_then(|w| w.parse().ok()).unwrap_or(1);
    let argv: Vec<String> = std::env::args().skip_while(|a| a != "--").skip(1).collect();
    if argv != ["login", "--region", "global"] {
        eprintln!("the stand-in was run as {argv:?}");
        std::process::exit(3);
    }
    if code == 0 {
        std::fs::create_dir_all(home.join("credentials")).expect("mkdir");
        std::fs::write(home.join("credentials/kimi-code.json"), "{}").expect("stand-in login");
    }
    eprint!("{transcript}");
    std::process::exit(code);
}

/// The pages handed over: each one's host and code.
type Handed = Vec<(Option<String>, Option<String>)>;

#[derive(Default)]
struct Seen {
    handed: RefCell<Handed>,
}

impl Approver for &Seen {
    fn approve(&mut self, handoff: &Handoff<'_>) -> Approval {
        self.handed
            .borrow_mut()
            .push((https_host(handoff.url), handoff.code.map(str::to_owned)));
        Approval::Approved { page: false }
    }
}

fn stand_in_drive(script: &str) -> (Drove, Handed) {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let staging = tmp.path().join("sol+1");
    std::fs::create_dir_all(&staging).expect("staging");
    std::fs::write(staging.join(".stand-in"), script).expect("script");
    let path = opener_free_path(&tmp.path().join("store")).expect("PATH");
    let env = child_env(
        Cli::Kimi,
        &KIMI_SIGNIN,
        &staging,
        path.path().as_os_str(),
        &vars(&[]),
    );
    let (grammar, completion, hosts) = lines_grammar(&KIMI_SIGNIN, Flow::Device);
    let args = child_args(
        KIMI_SIGNIN.route(None).expect("kimi's route"),
        Some(Region::Global),
    );
    let exe = std::env::current_exe().expect("test exe");
    let lead: Vec<OsString> = [
        "harness::login_drive::tests::signin_stand_in",
        "--exact",
        "--nocapture",
        "--",
    ]
    .map(OsString::from)
    .to_vec();
    let seen = Seen::default();
    let drove = drive(
        &Spec {
            program: &exe,
            lead: &lead,
            args: &args,
            env: &env,
            cwd: &staging,
            grammar,
            completion,
            hosts: hosts.of(Some(Region::Global)).expect("global"),
            email: Some("kimi-one@example.com"),
            hang: Duration::from_secs(60),
        },
        &mut &seen,
    );
    let handed = seen.handed.borrow().clone();
    (drove, handed)
}

#[test]
fn a_driven_sign_in_hands_the_printed_code_over_and_an_off_pin_page_is_never_opened() {
    let (drove, handed) = stand_in_drive("kimi 0");
    assert_eq!(
        drove.outcome,
        Outcome::SignedIn {
            email: None,
            page: false
        }
    );
    assert_eq!(drove.host.as_deref(), Some("www.kimi.ai"));
    assert_eq!(
        handed,
        [(Some("www.kimi.ai".to_owned()), Some("QXR4-MZK7".to_owned()))]
    );
    assert_eq!(drove.why(Flow::Device), "www.kimi.ai device signed-in");

    let (drove, handed) = stand_in_drive("off-pin 0");
    assert_eq!(drove.outcome, Outcome::OffPin);
    assert!(handed.is_empty(), "nothing is opened off the pins");
    assert!(!drove.why(Flow::Device).contains("QXR4"));

    let (drove, _) = stand_in_drive("kimi 1");
    assert_eq!(drove.outcome, Outcome::Exited(Some(1)));
    assert_eq!(drove.why(Flow::Device), "www.kimi.ai device exited-1");
}

#[cfg(unix)]
fn executable(path: &Path, text: &str) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::write(path, text).expect("write");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

/// An npm-installed CLI is a bare word on the parent's `PATH` and a `#!/usr/bin/env node`
/// script: its opener-free child gets the program's own path and a `PATH` that holds its
/// interpreter and no browser opener.
#[cfg(unix)]
#[test]
fn an_opener_free_child_finds_its_bare_program_and_its_env_interpreter_and_no_opener() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(&bin).expect("bin");
    executable(
        &bin.join("kimi"),
        "#!/usr/bin/env -S node --no-warnings\nrequire('x');\n",
    );
    executable(&bin.join("node"), "");
    executable(&bin.join("open"), "");
    std::fs::write(bin.join("notexec"), "").expect("plain file");
    let store = tmp.path().join("store");
    let parent = std::env::join_paths([Path::new("relative"), &bin]).expect("PATH");
    let var = |name: &str| (name == "PATH").then(|| parent.clone());

    let (program, path, dir) =
        child_program(Path::new("kimi"), None, true, &store, &var).expect("child");
    assert_eq!(
        program,
        bin.join("kimi"),
        "the bare word, found on the parent's PATH"
    );
    let dir = dir.expect("an opener-free PATH");
    assert_eq!(path, dir.path().as_os_str());
    let linked: Vec<String> = std::fs::read_dir(dir.path())
        .expect("read")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(linked, ["node"]);
    assert_eq!(
        on_path(Path::new("node"), &path),
        Some(dir.path().join("node"))
    );
    assert_eq!(
        on_path(Path::new("open"), &path),
        None,
        "no opener on the child's PATH"
    );

    // Negative: an opener a script names is never linked; a path or a plain file is not a bare
    // executable word; a child that may open the browser keeps the parent's PATH.
    executable(&bin.join("lure"), "#!/usr/bin/env open\n");
    assert_eq!(env_interpreter(&bin.join("lure")), None);
    assert_eq!(on_path(Path::new("notexec"), &parent), None);
    assert_eq!(on_path(Path::new("bin/kimi"), &parent), None);
    let (program, path, dir) =
        child_program(Path::new("kimi"), None, false, &store, &var).expect("child");
    assert_eq!(
        (program, path, dir.is_none()),
        (bin.join("kimi"), parent.clone(), true)
    );
    let given = OsString::from("/given");
    let (_, path, dir) =
        child_program(Path::new("/abs/kimi"), Some(&given), true, &store, &var).expect("child");
    assert_eq!((path, dir.is_none()), (given, true));
}
