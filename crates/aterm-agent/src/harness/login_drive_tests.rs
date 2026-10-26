// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the vendor sign-in driver: the line grammars over recorded transcripts
//! (`fixtures/signin/`: Kimi Code 2.3.1's `login --region global` and Codex 0.164.0's
//! `login --device-auth`, recorded in a scratch home and cancelled, their codes fabricated; Grok
//! Build 1.0.67's `login --device-auth`, written from its source, its email at example.com, and
//! 1.0.70's under the no-desk sandbox, recorded so and cancelled, its code fabricated), the host
//! pins, the child's environment and the driven child itself (this test binary run in place as
//! the CLI).

use std::cell::RefCell;
use std::ffi::OsString;

use super::*;
use crate::ai::connector::{CODEX_SIGNIN, GROK_SIGNIN, Hosts, KIMI_SIGNIN};

const KIMI: &str = include_str!("fixtures/signin/kimi-2.3.1-login-global.stderr");
const GROK: &str = include_str!("fixtures/signin/grok-1.0.67-device-auth.stderr");
const GROK_NO_DESK: &str = include_str!("fixtures/signin/grok-1.0.70-device-auth-no-desk.stderr");
const CODEX: &str = include_str!("fixtures/signin/codex-0.164.0-device-auth.stdout");

fn lines_grammar(signin: &SignIn, flow: Flow) -> (&'static Grammar, Completion, &'static Hosts) {
    let route = signin.route(Some(flow)).expect("the route");
    match &route.drive {
        Drive::Lines {
            grammar,
            completion,
            ..
        } => (grammar, *completion, &route.hosts),
        Drive::CodexPkce => panic!("a line route"),
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
/// login file into that home when it exits 0, and exits; `hold` writes its pid to `pid` there,
/// prints Kimi's transcript and waits a minute before it exits 0. Anywhere else it is an empty
/// test.
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
    let first = words.next();
    let transcript = match first {
        Some("kimi" | "hold") => KIMI.to_owned(),
        Some("kimi-unfinished") => KIMI
            .lines()
            .filter(|line| !line.contains("Logged in"))
            .collect::<Vec<_>>()
            .join("\n"),
        Some("off-pin") => KIMI.replace("www.kimi.ai", "www.kimi.ai.example.com"),
        _ => String::new(),
    };
    let code: i32 = words.next().and_then(|w| w.parse().ok()).unwrap_or(1);
    let argv: Vec<String> = std::env::args().skip_while(|a| a != "--").skip(1).collect();
    if argv != ["login", "--region", "global"] {
        eprintln!("the stand-in was run as {argv:?}");
        std::process::exit(3);
    }
    if first == Some("hold") {
        std::fs::write(home.join("pid"), std::process::id().to_string()).expect("pid");
        eprint!("{transcript}");
        std::thread::sleep(Duration::from_secs(60));
        std::process::exit(0);
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
    let seen = Seen::default();
    let drove = stand_in_drive_in(tmp.path(), script, &mut &seen);
    let handed = seen.handed.borrow().clone();
    (drove, handed)
}

/// The Kimi stand-in driven in `root/sol+1`, its page handed to `approver`.
fn stand_in_drive_in(root: &Path, script: &str, approver: &mut dyn Approver) -> Drove {
    let tmp = root;
    let staging = tmp.join("sol+1");
    std::fs::create_dir_all(&staging).expect("staging");
    std::fs::write(staging.join(".stand-in"), script).expect("script");
    let path = opener_free_path(&tmp.join("store")).expect("PATH");
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
    drive(
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
        approver,
    )
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

/// Grok Build 1.0.70's lines under the no-desk sandbox (its failed browser open between the URL
/// and the code's lead) still name its URL and code, and say it is done only once it signed in.
#[test]
fn grok_1_0_70s_sandboxed_lines_still_name_its_url_and_code() {
    let (grok, _, hosts) = lines_grammar(&GROK_SIGNIN, Flow::Device);
    let scan = scanned(grok, GROK_NO_DESK);
    let url = scan.url.as_deref().expect("grok's url");
    assert_eq!(
        url,
        "https://accounts.x.ai/oauth2/device?user_code=WMRT-8KQZ"
    );
    assert!(pinned(url, hosts.of(None).expect("fixed")));
    assert_eq!(scan.code.as_deref(), Some("WMRT-8KQZ"));
    assert!(!scan.done && scan.email.is_none(), "{scan:?}");
    let mut done = scan.clone();
    done.feed(grok, "\r\u{1b}[K\u{2713} Signed in as grok-one@example.com");
    assert!(done.done);
    assert_eq!(done.email.as_deref(), Some("grok-one@example.com"));
}

/// The approver found the row's profile signed out. Unix's, as its one test is.
#[cfg(unix)]
struct SignedOut;

#[cfg(unix)]
impl Approver for SignedOut {
    fn approve(&mut self, _: &Handoff<'_>) -> Approval {
        Approval::NeedsSignIn("the page waits on a person to sign in".to_owned())
    }
}

/// An approver asking for a sign-in once ends the child at once and the sign-in
/// `needs-sign-in`. CONTROL: one that approves ends it signed in.
#[cfg(unix)]
#[test]
fn an_approver_asking_for_a_sign_in_once_ends_the_child_needs_sign_in() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let drove = stand_in_drive_in(tmp.path(), "hold 0", &mut SignedOut);
    assert_eq!(
        drove.outcome,
        Outcome::NeedsSignIn("the page waits on a person to sign in".to_owned())
    );
    assert_eq!(drove.why(Flow::Device), "www.kimi.ai device needs-sign-in");
    let pid: i32 = std::fs::read_to_string(tmp.path().join("sol+1/pid"))
        .expect("the stand-in's pid")
        .parse()
        .expect("a pid");
    // SAFETY: signal 0 only asks whether this test's own child still exists.
    let alive = unsafe { libc::kill(pid, 0) } == 0;
    if alive {
        // SAFETY: the child is this test's own stand-in, left running by a broken driver.
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
    assert!(!alive, "the child was ended");

    let (drove, _) = stand_in_drive("kimi 0");
    assert!(
        matches!(drove.outcome, Outcome::SignedIn { .. }),
        "CONTROL: {drove:?}"
    );
}

/// Whether this process can look LaunchServices' database up: a Mach name lookup, which sends
/// LaunchServices nothing and so opens nothing.
#[cfg(target_os = "macos")]
fn launch_services_reachable() -> bool {
    unsafe extern "C" {
        static bootstrap_port: core::ffi::c_uint;
        fn bootstrap_look_up(
            bp: core::ffi::c_uint,
            name: *const core::ffi::c_char,
            sp: *mut core::ffi::c_uint,
        ) -> core::ffi::c_int;
    }
    let mut port: core::ffi::c_uint = 0;
    // SAFETY: `bootstrap_port` is the task's bootstrap port, set before `main`; the name is a
    // valid C string and `port` a valid out-pointer.
    unsafe {
        bootstrap_look_up(
            bootstrap_port,
            c"com.apple.lsd.mapdb".as_ptr(),
            &raw mut port,
        ) == 0
    }
}

/// Grok Build as this test binary run in place: where its `GROK_HOME` (which only the driver
/// hands it) holds `.stand-in`, it writes `ran` there, checks it was run as `grok login
/// --device-auth` (after `--`), prints 1.0.70's sandboxed lines, and says it signed in (exit 0)
/// only where LaunchServices cannot be looked up, as under [`NO_DESK`]; where it can, it exits 1.
/// Anywhere else it is an empty test.
#[cfg(target_os = "macos")]
#[test]
fn grok_stand_in() {
    let Some(home) = std::env::var_os("GROK_HOME")
        .map(PathBuf::from)
        .filter(|home| home.join(".stand-in").is_file())
    else {
        return;
    };
    std::fs::write(home.join("ran"), "").expect("marker");
    let argv: Vec<String> = std::env::args().skip_while(|a| a != "--").skip(1).collect();
    if argv != ["login", "--device-auth"] {
        eprintln!("the stand-in was run as {argv:?}");
        std::process::exit(3);
    }
    eprint!("{GROK_NO_DESK}");
    if launch_services_reachable() {
        std::process::exit(1);
    }
    eprintln!("\r\u{1b}[K\u{2713} Signed in as grok-one@example.com");
    std::process::exit(0);
}

/// A Grok row's staging home and store, its stand-in armed.
#[cfg(target_os = "macos")]
struct GrokBed {
    tmp: aterm_tempfile::TempDir,
    staging: PathBuf,
    store: crate::harness::accounts::Store,
    roster: crate::harness::accounts::Roster,
}

#[cfg(target_os = "macos")]
impl GrokBed {
    fn new() -> GrokBed {
        let tmp = aterm_tempfile::tempdir().expect("tmp");
        let staging = tmp.path().join("grok-1+1");
        std::fs::create_dir_all(&staging).expect("staging");
        std::fs::write(staging.join(".stand-in"), "").expect("arm");
        let store = crate::harness::accounts::Store::at_for(
            &tmp.path().join("state"),
            Cli::Grok,
            tmp.path(),
        );
        let roster = crate::harness::accounts::Roster::parse_for(
            Cli::Grok,
            "[[account]]\nlabel = \"grok-1\"\nclass = \"dedicated\"\nemail = \"grok-one@example.com\"\n",
        )
        .expect("grok's roster");
        GrokBed {
            tmp,
            staging,
            store,
            roster,
        }
    }

    fn lead() -> Vec<OsString> {
        [
            "harness::login_drive::tests::grok_stand_in",
            "--exact",
            "--nocapture",
            "--",
        ]
        .map(OsString::from)
        .to_vec()
    }

    /// Its sign-in through [`sign_in_with`], the sandbox `no_desk`.
    fn sign_in(&self, no_desk: Option<&Path>) -> (Drove, Handed) {
        let route = GROK_SIGNIN.route(None).expect("grok's route");
        let req = Request {
            cli: Cli::Grok,
            route,
            row: self.roster.row("grok-1").expect("the row"),
            staging: &self.staging,
            store: &self.store,
            hosts: route.hosts.of(None).expect("fixed"),
        };
        let exe = std::env::current_exe().expect("test exe");
        let seen = Seen::default();
        let drove = sign_in_with(
            &req,
            (&exe, &GrokBed::lead()),
            None,
            no_desk,
            &vars(&[]),
            &mut &seen,
            Duration::from_secs(60),
        );
        let handed = seen.handed.borrow().clone();
        (drove, handed)
    }

    /// The same child driven plainly, as a CLI whose `no_desk` is empty runs.
    fn plain(&self) -> Drove {
        let (grammar, completion, hosts) = lines_grammar(&GROK_SIGNIN, Flow::Device);
        let path = opener_free_path(&self.tmp.path().join("store")).expect("PATH");
        let env = child_env(
            Cli::Grok,
            &GROK_SIGNIN,
            &self.staging,
            path.path().as_os_str(),
            &vars(&[]),
        );
        let exe = std::env::current_exe().expect("test exe");
        let seen = Seen::default();
        drive(
            &Spec {
                program: &exe,
                lead: &GrokBed::lead(),
                args: &child_args(GROK_SIGNIN.route(None).expect("route"), None),
                env: &env,
                cwd: &self.staging,
                grammar,
                completion,
                hosts: hosts.of(None).expect("fixed"),
                email: Some("grok-one@example.com"),
                hang: Duration::from_secs(60),
            },
            &mut &seen,
        )
    }
}

/// Under the no-desk sandbox a sign-in child cannot look LaunchServices up, so a CLI that opens
/// the default browser through the OS opens nothing, and it still prints its page and code: the
/// stand-in signs in only there. CONTROL: the same child run plainly reaches LaunchServices and
/// exits 1.
#[cfg(target_os = "macos")]
#[test]
fn a_no_desk_child_cannot_reach_launch_services_and_a_plain_one_can() {
    assert_eq!(
        no_desk_argv(Path::new("/opt/grok/bin/grok"), &[OsString::from("login")]),
        ["-p", NO_DESK, "/opt/grok/bin/grok", "login"].map(OsString::from)
    );
    let bed = GrokBed::new();
    let (drove, handed) = bed.sign_in(Some(Path::new(SANDBOX_EXEC)));
    assert_eq!(
        drove.outcome,
        Outcome::SignedIn {
            email: Some("grok-one@example.com".to_owned()),
            page: false
        }
    );
    assert_eq!(
        handed,
        [(
            Some("accounts.x.ai".to_owned()),
            Some("WMRT-8KQZ".to_owned())
        )]
    );
    assert_eq!(
        bed.plain().outcome,
        Outcome::Exited(Some(1)),
        "CONTROL: run plainly, it reaches LaunchServices"
    );
}

/// A no-desk CLI's sign-in runs nothing where its sandbox is missing or is not a file: `NotRun`,
/// naming why, and the stand-in never ran. CONTROL: with the sandbox there, it ran.
#[cfg(target_os = "macos")]
#[test]
fn a_no_desk_sign_in_without_its_sandbox_runs_nothing() {
    let bed = GrokBed::new();
    let missing = bed.tmp.path().join("no-sandbox-exec");
    let a_dir = bed.tmp.path().to_path_buf();
    for no_desk in [None, Some(missing.as_path()), Some(a_dir.as_path())] {
        let (drove, handed) = bed.sign_in(no_desk);
        let Outcome::NotRun(why) = &drove.outcome else {
            panic!("{no_desk:?}: {drove:?}");
        };
        assert!(
            why.starts_with("Grok Build opens this machine's default browser through the OS")
                && why.ends_with("is not here; nothing was run"),
            "{why}"
        );
        assert!(handed.is_empty() && !bed.staging.join("ran").exists());
    }
    let (drove, _) = bed.sign_in(Some(Path::new(SANDBOX_EXEC)));
    assert!(bed.staging.join("ran").is_file(), "CONTROL: {drove:?}");
}

/// A no-desk CLI's sign-in runs nothing where its sandbox cannot apply here (as `sandbox-exec`
/// cannot under another sandbox, exiting 71 before its child starts): `NotRun`, naming that, and
/// the stand-in never ran, where it once said "exited 71". CONTROL: the real sandbox passes its
/// try.
#[cfg(target_os = "macos")]
#[test]
fn a_no_desk_sign_in_whose_sandbox_cannot_apply_runs_nothing_and_says_so() {
    let bed = GrokBed::new();
    let refusing = Path::new("/usr/bin/false");
    let (drove, handed) = bed.sign_in(Some(refusing));
    let Outcome::NotRun(why) = &drove.outcome else {
        panic!("{drove:?}");
    };
    assert!(
        why.contains("cannot run here: /usr/bin/false exited 1 on a try")
            && why.contains("another sandbox")
            && why.ends_with("nothing was run"),
        "{why}"
    );
    assert!(handed.is_empty() && !bed.staging.join("ran").exists());
    assert_eq!(sandbox_refusal(Path::new(SANDBOX_EXEC)), None, "CONTROL");
}

/// A no-desk CLI that is not installed runs nothing and says it is not installed, where the
/// sandbox once failed to start it and the sign-in "exited 71". CONTROL: the stand-in, a file,
/// runs.
#[cfg(target_os = "macos")]
#[test]
fn a_no_desk_cli_that_is_not_installed_is_named_so() {
    let bed = GrokBed::new();
    let route = GROK_SIGNIN.route(None).expect("grok's route");
    let req = Request {
        cli: Cli::Grok,
        route,
        row: bed.roster.row("grok-1").expect("the row"),
        staging: &bed.staging,
        store: &bed.store,
        hosts: route.hosts.of(None).expect("fixed"),
    };
    let seen = Seen::default();
    let drove = sign_in_with(
        &req,
        (Path::new("no-such-grok-cli"), &[]),
        None,
        Some(Path::new(SANDBOX_EXEC)),
        &vars(&[("PATH", "/usr/bin:/bin")]),
        &mut &seen,
        Duration::from_secs(60),
    );
    let Outcome::NotRun(why) = &drove.outcome else {
        panic!("{drove:?}");
    };
    assert!(
        why.contains("`no-such-grok-cli` is not installed here"),
        "{why}"
    );
    let (drove, _) = bed.sign_in(Some(Path::new(SANDBOX_EXEC)));
    assert!(bed.staging.join("ran").is_file(), "CONTROL: {drove:?}");
}

/// OVER SSH a sign-in the person runs (`accounts login`) opens no browser on this machine's
/// desk, which is not theirs: Grok Build's runs under the no-desk sandbox, and Kimi Code's gets
/// the opener-free `PATH` (held while it runs). Grok's refuses where the sandbox is missing.
/// CONTROL: at the desk each runs plainly with this process's `PATH`.
#[cfg(target_os = "macos")]
#[test]
fn over_ssh_a_persons_sign_in_opens_no_browser_on_this_machines_desk() {
    use std::ffi::OsStr;
    let bed = GrokBed::new();
    let home = bed.tmp.path().join("home");
    let parent = vars(&[("PATH", "/usr/bin:/bin"), ("TERM", "xterm")]);
    let sandbox = Some(Path::new(SANDBOX_EXEC));
    let route = GROK_SIGNIN.route(None).expect("grok's route");
    let grok = Request {
        cli: Cli::Grok,
        route,
        row: bed.roster.row("grok-1").expect("the row"),
        staging: &bed.staging,
        store: &bed.store,
        hosts: route.hosts.of(None).expect("fixed"),
    };
    let path_of = |cmd: &aterm_uds::spawnfd::Command| {
        cmd.get_envs()
            .find(|(k, _)| *k == OsStr::new("PATH"))
            .and_then(|(_, v)| v.map(OsStr::to_os_string))
    };
    let (cmd, held) = login_command(&grok, &home, &parent, (true, sandbox)).expect("over SSH");
    assert_eq!(cmd.get_program(), OsStr::new(SANDBOX_EXEC));
    let args: Vec<OsString> = cmd.get_args().map(OsStr::to_os_string).collect();
    assert_eq!(args[..2], [OsString::from("-p"), OsString::from(NO_DESK)]);
    let verb: Vec<OsString> = child_args(route, None)
        .into_iter()
        .map(OsString::from)
        .collect();
    assert_eq!(args[3..], verb[..], "{args:?}");
    let held = held.expect("Grok Build's opener-free PATH too");
    assert_eq!(path_of(&cmd), Some(held.path().as_os_str().to_os_string()));
    let missing = bed.tmp.path().join("no-sandbox-exec");
    let refused = login_command(&grok, &home, &parent, (true, Some(&missing)));
    assert!(
        refused
            .as_ref()
            .is_err_and(|why| why.contains("over SSH") && why.contains("is not here")),
        "{:?}",
        refused.map(|_| ())
    );
    let (desk, _) = login_command(&grok, &home, &parent, (false, sandbox)).expect("at the desk");
    assert_ne!(desk.get_program(), OsStr::new(SANDBOX_EXEC), "CONTROL");
    assert_eq!(path_of(&desk), Some(OsString::from("/usr/bin:/bin")));

    let roster = crate::harness::accounts::Roster::parse_for(
        Cli::Kimi,
        "[[account]]\nlabel = \"kimi-1\"\nclass = \"dedicated\"\nemail = \"kimi-one@example.com\"\nregion = \"global\"\n",
    )
    .expect("kimi's roster");
    let kimi_store = crate::harness::accounts::Store::at_for(
        &bed.tmp.path().join("state"),
        Cli::Kimi,
        bed.tmp.path(),
    );
    let route = KIMI_SIGNIN.route(None).expect("kimi's route");
    let kimi = Request {
        cli: Cli::Kimi,
        route,
        row: roster.row("kimi-1").expect("the row"),
        staging: &bed.staging,
        store: &kimi_store,
        hosts: route.hosts.of(Some(Region::Global)).expect("global"),
    };
    let (cmd, held) = login_command(&kimi, &home, &parent, (true, sandbox)).expect("over SSH");
    let held = held.expect("an opener-free PATH, held");
    assert_eq!(path_of(&cmd), Some(held.path().as_os_str().to_os_string()));
    assert!(!held.path().join("open").exists());
    assert_ne!(
        cmd.get_program(),
        OsStr::new(SANDBOX_EXEC),
        "kimi needs no sandbox"
    );
    let (desk, held) = login_command(&kimi, &home, &parent, (false, sandbox)).expect("desk");
    assert!(held.is_none(), "CONTROL");
    assert_eq!(path_of(&desk), Some(OsString::from("/usr/bin:/bin")));
}

/// OVER SSH a person's Codex sign-in opens no browser on this machine's desk and is one the
/// person can finish: `codex login` opens the default browser through LaunchServices whatever
/// its `PATH`, and its page calls back to this machine's 127.0.0.1:1455, which the person's
/// browser cannot reach, so a browser row runs `codex login --device-auth` under the no-desk
/// sandbox with the opener-free `PATH` (a Grok Build browser row its device route too). CONTROL:
/// at the desk the browser row runs plain `codex login` with this process's `PATH`, and a Kimi
/// row, whose page calls nothing back, keeps its route over SSH.
#[cfg(target_os = "macos")]
#[test]
fn over_ssh_a_codex_browser_sign_in_runs_its_device_route_off_this_machines_desk() {
    use std::ffi::OsStr;
    let bed = GrokBed::new();
    let home = bed.tmp.path().join("home");
    let parent = vars(&[("PATH", "/usr/bin:/bin"), ("TERM", "xterm")]);
    let sandbox = Some(Path::new(SANDBOX_EXEC));
    let roster = crate::harness::accounts::Roster::parse_for(
        Cli::Codex,
        "[[account]]\nlabel = \"codex-1\"\nclass = \"dedicated\"\nemail = \"codex-one@example.com\"\n",
    )
    .expect("codex's roster");
    let store = crate::harness::accounts::Store::at_for(
        &bed.tmp.path().join("state"),
        Cli::Codex,
        bed.tmp.path(),
    );
    let route = CODEX_SIGNIN.route(None).expect("codex's route");
    assert!(calls_back_here(route), "the default is the browser route");
    let codex = Request {
        cli: Cli::Codex,
        route,
        row: roster.row("codex-1").expect("the row"),
        staging: &bed.staging,
        store: &store,
        hosts: route.hosts.of(None).expect("fixed"),
    };
    let path_of = |cmd: &aterm_uds::spawnfd::Command| {
        cmd.get_envs()
            .find(|(k, _)| *k == OsStr::new("PATH"))
            .and_then(|(_, v)| v.map(OsStr::to_os_string))
    };
    let (cmd, held) = login_command(&codex, &home, &parent, (true, sandbox)).expect("over SSH");
    assert_eq!(cmd.get_program(), OsStr::new(SANDBOX_EXEC));
    let args: Vec<OsString> = cmd.get_args().map(OsStr::to_os_string).collect();
    assert_eq!(args[..2], [OsString::from("-p"), OsString::from(NO_DESK)]);
    assert_eq!(
        args[3..],
        [OsString::from("login"), OsString::from("--device-auth")],
        "{args:?}"
    );
    let held = held.expect("an opener-free PATH, held");
    assert_eq!(path_of(&cmd), Some(held.path().as_os_str().to_os_string()));
    let missing = bed.tmp.path().join("no-sandbox-exec");
    assert!(
        login_command(&codex, &home, &parent, (true, Some(&missing)))
            .is_err_and(|why| why.contains("over SSH") && why.contains("is not here")),
        "no sandbox, nothing run"
    );
    let (desk, held) = login_command(&codex, &home, &parent, (false, sandbox)).expect("desk");
    assert_ne!(desk.get_program(), OsStr::new(SANDBOX_EXEC), "CONTROL");
    let args: Vec<OsString> = desk.get_args().map(OsStr::to_os_string).collect();
    assert_eq!(args, [OsString::from("login")], "CONTROL: {args:?}");
    assert!(held.is_none(), "CONTROL");
    assert_eq!(path_of(&desk), Some(OsString::from("/usr/bin:/bin")));

    let grok_browser = GROK_SIGNIN
        .route(Some(Flow::Browser))
        .expect("grok's browser route");
    let grok = Request {
        cli: Cli::Grok,
        route: grok_browser,
        row: bed.roster.row("grok-1").expect("the row"),
        staging: &bed.staging,
        store: &bed.store,
        hosts: grok_browser.hosts.of(None).expect("fixed"),
    };
    assert_eq!(
        login_route(&grok, true).map(|r| r.flow),
        Ok(Flow::Device),
        "grok's loopback route over SSH"
    );
    assert_eq!(
        login_route(&grok, false).map(|r| r.flow),
        Ok(Flow::Browser),
        "CONTROL: at the desk"
    );
    let kimi_route = KIMI_SIGNIN.route(None).expect("kimi's route");
    assert!(
        !calls_back_here(kimi_route),
        "CONTROL: kimi's page calls nothing back"
    );
}

#[test]
fn slow_approver_past_output_grace_still_drains_buffered_signed_in_line() {
    struct SlowApprover;
    impl Approver for SlowApprover {
        fn approve(&mut self, _handoff: &Handoff<'_>) -> Approval {
            std::thread::sleep(OUTPUT_GRACE + Duration::from_millis(150));
            Approval::Approved { page: true }
        }
    }

    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let drove = stand_in_drive_in(tmp.path(), "kimi 0", &mut SlowApprover);
    assert_eq!(
        drove.outcome,
        Outcome::SignedIn {
            email: None,
            page: true,
        },
        "buffered `Signed in` line must still be drained when approver exceeds OUTPUT_GRACE"
    );

    let tmp_ctrl = aterm_tempfile::tempdir().expect("tmp_ctrl");
    let control = stand_in_drive_in(tmp_ctrl.path(), "kimi-unfinished 0", &mut SlowApprover);
    assert_eq!(
        control.outcome,
        Outcome::Exited(Some(0)),
        "CONTROL: exit 0 without a completion phrase stays Exited(Some(0))"
    );
}
