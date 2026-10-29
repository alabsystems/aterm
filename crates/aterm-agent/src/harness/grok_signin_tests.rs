// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The machine's one Grok Build sign-in, driven against a stand-in vendor (this test binary run
//! in place): it reads files and the environment before anything runs, runs nothing without its
//! sandbox, keeps the vendor's browser open off the desk while its page and code are read, and
//! starts one sign-in and one need for any number of waiting callers. No real grok runs, and no
//! page is opened: the stand-in's "browser open" is `/usr/bin/open -h`, which only prints its
//! usage whether or not the sandbox stops it.

use super::*;
#[cfg(target_os = "macos")]
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
#[cfg(target_os = "macos")]
use std::sync::{Arc, Mutex};

/// Grok Build 1.0.70's lines under the no-desk sandbox (`fixtures/signin/…no-desk.stderr`, its
/// code made up), up to the wait.
const GROK_LINES: &str = include_str!("fixtures/signin/grok-1.0.70-device-auth-no-desk.stderr");

/// The stand-in's arm file in its test's root: the mode it runs in.
const ARM: &str = "stand-in.arm";

/// The test's root, from the staging home the gate hands the stand-in
/// (`<root>/state/.signin-grok-…`).
fn stand_in_root() -> Option<PathBuf> {
    let staging = PathBuf::from(std::env::var_os("GROK_HOME")?);
    let root = staging.parent()?.parent()?.to_path_buf();
    root.join(ARM).is_file().then_some(root)
}

/// Grok Build as this test binary run in place: where its staging home's root holds
/// [`ARM`], it counts its run (a line in `count`), checks it was run as `grok login
/// --device-auth`, tries to open "the browser" by running `/usr/bin/open -h` (writing `opened`
/// where that ran), prints 1.0.70's device lines, waits for `approve`, then, armed `sign`,
/// writes its login to `GROK_AUTH_PATH` and says it signed in (exit 0), and armed `expire`
/// exits 1 as an expired code does. Anywhere else it is an empty test.
#[test]
fn grok_signin_stand_in() {
    let Some(root) = stand_in_root() else {
        return;
    };
    let mode = std::fs::read_to_string(root.join(ARM)).unwrap_or_default();
    {
        use std::io::Write as _;
        let mut count = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(root.join("count"))
            .expect("count");
        writeln!(count, "{}", std::process::id()).expect("count line");
    }
    let argv: Vec<String> = std::env::args().skip_while(|a| a != "--").skip(1).collect();
    if argv != ["login", "--device-auth"] {
        eprintln!("the stand-in was run as {argv:?}");
        std::process::exit(3);
    }
    let opened = aterm_uds::spawnfd::command("/usr/bin/open")
        .arg("-h")
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .status()
        .is_ok();
    if opened {
        std::fs::write(root.join("opened"), "").expect("opened marker");
    }
    eprint!("{GROK_LINES}");
    let until = Instant::now() + Duration::from_secs(60);
    while !root.join("approve").exists() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(20));
    }
    if mode.trim() != "sign" {
        std::process::exit(1);
    }
    let login = std::env::var_os("GROK_AUTH_PATH").expect("GROK_AUTH_PATH");
    std::fs::write(login, "{\"stand-in\":true}\n").expect("login");
    eprintln!("\r\u{1b}[K\u{2713} Signed in as grok-one@example.com");
    std::process::exit(0);
}

#[cfg(target_os = "macos")]
fn lead() -> Vec<OsString> {
    [
        "harness::grok_signin::tests::grok_signin_stand_in",
        "--exact",
        "--nocapture",
        "--",
    ]
    .map(OsString::from)
    .to_vec()
}

/// A test's root: `home/` (HOME), `state/` (the harness state), the stand-in's files.
#[cfg(target_os = "macos")]
struct Bed {
    tmp: aterm_tempfile::TempDir,
    exe: PathBuf,
    lead: Vec<OsString>,
}

#[cfg(target_os = "macos")]
impl Bed {
    fn new(mode: &str) -> Bed {
        let tmp = aterm_tempfile::tempdir().expect("tmp");
        std::fs::create_dir_all(tmp.path().join("home")).expect("home");
        std::fs::write(tmp.path().join(ARM), mode).expect("arm");
        Bed {
            tmp,
            exe: std::env::current_exe().expect("test exe"),
            lead: lead(),
        }
    }

    fn root(&self) -> &Path {
        self.tmp.path()
    }

    fn home(&self) -> PathBuf {
        self.root().join("home")
    }

    fn state(&self) -> PathBuf {
        self.root().join("state")
    }

    fn login(&self) -> PathBuf {
        self.home().join(".grok/auth.json")
    }

    fn runs(&self) -> usize {
        std::fs::read_to_string(self.root().join("count"))
            .map(|t| t.lines().count())
            .unwrap_or(0)
    }
}

/// The need's owner, counted: each need, line and take-down, and what was said.
#[cfg(target_os = "macos")]
#[derive(Default)]
struct Seen {
    needs: AtomicUsize,
    clears: AtomicUsize,
    waiting: AtomicUsize,
    texts: Mutex<Vec<String>>,
    lines: Mutex<Vec<String>>,
    said: Mutex<Vec<String>>,
}

#[cfg(target_os = "macos")]
struct Counted<'a> {
    seen: &'a Seen,
    /// Run when the need goes up (the person approving the page).
    approve: Option<PathBuf>,
    /// Set when the leader tells the person its ladder (a Ctrl-C right after it).
    stop_on_say: Option<Arc<AtomicBool>>,
}

#[cfg(target_os = "macos")]
impl<'a> Counted<'a> {
    fn new(seen: &'a Seen, approve: Option<PathBuf>) -> Counted<'a> {
        Counted {
            seen,
            approve,
            stop_on_say: None,
        }
    }
}

#[cfg(target_os = "macos")]
impl Owner for Counted<'_> {
    fn need(&mut self, text: &str, line: &str, _url: &str) {
        self.seen.needs.fetch_add(1, Ordering::SeqCst);
        self.seen.texts.lock().unwrap().push(text.to_owned());
        self.seen.lines.lock().unwrap().push(line.to_owned());
        if let Some(file) = &self.approve {
            std::fs::write(file, "").expect("approve");
        }
    }

    fn clear(&mut self) {
        self.seen.clears.fetch_add(1, Ordering::SeqCst);
    }

    fn say(&mut self, line: &str) {
        self.seen.said.lock().unwrap().push(line.to_owned());
        if line.starts_with("another session is signing Grok Build in") {
            self.seen.waiting.fetch_add(1, Ordering::SeqCst);
        }
        if line.starts_with("Grok Build sign-in: ")
            && let Some(stop) = &self.stop_on_say
        {
            stop.store(true, Ordering::SeqCst);
        }
    }
}

/// What the automatic rung's stand-in does when asked ([`Rung`]).
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Act {
    /// Answers at once, pressing nothing (an approval presses and waits for grok's end).
    Say,
    /// Presses the page (the stand-in vendor's `approve`), then says its answer at once, as a
    /// rung whose confirm landed in its last seconds and whose page never said so does.
    PressThenSay,
    /// The person's Ctrl-C while it runs: sets the stop, then waits for its `finished` (which a
    /// stop makes true) and says approved, as the headless browser's loop does.
    StopWhileRunning,
}

/// The automatic rung as a stand-in: its ladder as given, and its answer. Told to approve, it
/// approves as the headless browser would (the stand-in vendor's `approve` file) and returns once
/// grok says it is done (a hang detector, never a latency budget); told anything else, it says
/// so at once and approves nothing, unless its [`Act`] says otherwise.
#[cfg(target_os = "macos")]
#[derive(Clone)]
struct Rung {
    ladder: Ladder,
    /// Its [`Auto::page_answers`]: Grok Build's own ([`Cli::renews_headlessly`], false while
    /// accounts.x.ai answers the headless browser with its block page) for [`Rung::bare`], and a
    /// vendor whose page answers for [`Rung::ready`].
    answers: bool,
    /// The pinned browser fetches here: where its ladder's browser is missing, the ladder runs
    /// the browser rung ([`browser_rung`]) with a fetch that succeeds, counted in `fetched`.
    fetchable: bool,
    fetched: usize,
    said: Approval,
    act: Act,
    approve: Option<PathBuf>,
    /// The stop it sets ([`Act::StopWhileRunning`], or while its ladder is read where
    /// `stop_in_ladder`): the gate's `stopped` reads it.
    stop: Arc<AtomicBool>,
    stop_in_ladder: bool,
    /// How often its ladder was read and whether it was allowed to fetch a browser, how often it
    /// was asked, the row's email it was handed, how long its deadline left it, and whether its
    /// `finished` came true while it waited.
    read: usize,
    fetch: Option<bool>,
    asked: usize,
    email: Option<String>,
    left: Option<Duration>,
    saw_finished: bool,
}

#[cfg(target_os = "macos")]
impl Rung {
    /// A machine with nothing ready: no row, no profile, no mailbox, no browser.
    fn bare() -> Rung {
        Rung {
            ladder: Ladder::bare("none installed"),
            answers: Cli::Grok.renews_headlessly(),
            fetchable: false,
            fetched: 0,
            said: Approval::NeedsYou("a bare machine's rung was asked".to_owned()),
            act: Act::Say,
            approve: None,
            stop: Arc::new(AtomicBool::new(false)),
            stop_in_ladder: false,
            read: 0,
            fetch: None,
            asked: 0,
            email: None,
            left: None,
            saw_finished: false,
        }
    }

    /// The roster's default-home row `main` (`grok-one@example.com` unless `email` says
    /// otherwise), its profile made, a headless browser found and a window browser installed,
    /// answering `said`; an approval presses the stand-in vendor's `approve` in `root`. It stands
    /// in for a vendor whose sign-in page answers the headless browser (`answers`), the only one
    /// whose rung the gate runs: Grok Build's page does not, and its rung is skipped
    /// (`grok_s_block_page_skips_the_rung_and_its_fetch_and_asks_the_person`).
    fn ready(root: &Path, email: &str, said: Approval) -> Rung {
        Rung {
            ladder: Ladder {
                label: "main".to_owned(),
                from: RowFrom::Default,
                email: Some(email.to_owned()),
                profile: true,
                mailbox: false,
                mailbox_unready: None,
                browser: Browser::Found,
                window: true,
                adopt: ADOPT_LABEL.to_owned(),
            },
            answers: true,
            said,
            approve: Some(root.join("approve")),
            ..Rung::bare()
        }
    }
}

#[cfg(target_os = "macos")]
impl Auto for Rung {
    fn page_answers(&self) -> bool {
        self.answers
    }

    fn ladder(&mut self, fetch: bool, _stopped: &dyn Fn() -> bool) -> Ladder {
        self.read += 1;
        self.fetch = Some(fetch);
        if self.stop_in_ladder {
            // The person's Ctrl-C while the pinned browser downloads.
            self.stop.store(true, Ordering::SeqCst);
        }
        let mut ladder = self.ladder.clone();
        if self.fetchable && matches!(ladder.browser, Browser::Missing(_)) {
            let wanted = fetch && ladder.email.is_some() && (ladder.profile || ladder.mailbox);
            let fetched = &mut self.fetched;
            ladder.browser = browser_rung(false, wanted, || {
                *fetched += 1;
                Ok(PathBuf::from("/fetched/chrome-headless-shell"))
            });
        }
        ladder
    }

    fn approve(&mut self, handoff: &Handoff<'_>) -> Approval {
        self.asked += 1;
        self.email = handoff.email.map(str::to_owned);
        self.left = Some(handoff.deadline.saturating_duration_since(Instant::now()));
        let wait_for_end = |rung: &mut Rung| {
            let until = Instant::now() + Duration::from_secs(60);
            while !(handoff.finished)() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(20));
            }
            rung.saw_finished = (handoff.finished)();
        };
        match self.act {
            Act::StopWhileRunning => {
                self.stop.store(true, Ordering::SeqCst);
                wait_for_end(self);
                return Approval::Approved { page: false };
            }
            Act::PressThenSay => {
                if let Some(file) = &self.approve {
                    std::fs::write(file, "").expect("approve");
                }
            }
            Act::Say => {
                if let Approval::Approved { .. } = self.said {
                    if let Some(file) = &self.approve {
                        std::fs::write(file, "").expect("approve");
                    }
                    wait_for_end(self);
                }
            }
        }
        self.said.clone()
    }
}

fn vars(pairs: Vec<(&'static str, OsString)>) -> impl Fn(&str) -> Option<OsString> {
    move |name| {
        pairs
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.clone())
    }
}

#[cfg(target_os = "macos")]
fn never() -> bool {
    false
}

#[cfg(unix)]
fn clock() -> i64 {
    crate::supervise::limit::unix_now()
}

/// One gate over `bed`, its sandbox `sandbox`, run to its end with `owner` and a bare machine's
/// rung ([`Rung::bare`]); the ledger lines.
#[cfg(target_os = "macos")]
fn gate_run(
    bed: &Bed,
    sandbox: Option<&Path>,
    (wait, ack): (bool, bool),
    extra: Vec<(&'static str, OsString)>,
    owner: &mut dyn Owner,
) -> (Ended, Vec<String>) {
    gate_run_with(bed, sandbox, (wait, ack), extra, &mut Rung::bare(), owner)
}

/// [`gate_run`] with the automatic rung `auto`.
#[cfg(target_os = "macos")]
fn gate_run_with(
    bed: &Bed,
    sandbox: Option<&Path>,
    (wait, ack): (bool, bool),
    extra: Vec<(&'static str, OsString)>,
    auto: &mut dyn Auto,
    owner: &mut dyn Owner,
) -> (Ended, Vec<String>) {
    gate_run_as(
        bed,
        sandbox,
        (wait, ack),
        (Duration::from_secs(60), &never),
        extra,
        auto,
        owner,
    )
}

/// [`gate_run_with`] with the sign-in's hang detector `hang` and the caller's `stopped`.
#[cfg(target_os = "macos")]
fn gate_run_as(
    bed: &Bed,
    sandbox: Option<&Path>,
    (wait, ack): (bool, bool),
    (hang, stopped): (Duration, &dyn Fn() -> bool),
    extra: Vec<(&'static str, OsString)>,
    auto: &mut dyn Auto,
    owner: &mut dyn Owner,
) -> (Ended, Vec<String>) {
    let mut env = vec![("HOME", bed.home().into_os_string())];
    env.extend(extra);
    let var = vars(env);
    let home = bed.home();
    let state = bed.state();
    let gate = Gate {
        state: &state,
        home: Some(&home),
        var: &var,
        program: (&bed.exe, &bed.lead),
        sandbox,
        hang,
        wait,
        ack,
        stopped,
        now: &clock,
    };
    let mut ledger = Vec::new();
    let ended = run(&gate, auto, owner, &mut |line| ledger.push(line.to_owned()));
    (ended, ledger)
}

#[test]
fn signed_in_is_read_from_the_credentials_and_the_login_files_in_grok_s_order() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let home = tmp.path().join("home");
    let login = home.join(".grok/auth.json");
    let pooled = tmp.path().join("pooled.json");
    let os = |p: &Path| p.as_os_str().to_os_string();
    assert_eq!(signed_in(&vars(vec![]), Some(&home)), None);
    std::fs::create_dir_all(login.parent().unwrap()).unwrap();
    std::fs::write(&login, "").unwrap();
    assert_eq!(
        signed_in(&vars(vec![]), Some(&home)),
        None,
        "an empty login is none"
    );
    std::fs::write(&login, "{}").unwrap();
    assert_eq!(
        signed_in(&vars(vec![]), Some(&home)),
        Some(Signed::File(login.clone()))
    );
    for name in CREDENTIAL_ENV {
        assert_eq!(
            signed_in(&vars(vec![(name, OsString::from("k"))]), None),
            Some(Signed::Env(name))
        );
        assert_eq!(signed_in(&vars(vec![(name, OsString::new())]), None), None);
    }
    // GROK_AUTH_PATH names the only file grok reads: an empty one is signed out, whatever the
    // home holds, and a full one is signed in.
    std::fs::write(&pooled, "").unwrap();
    let pool = vars(vec![(LOGIN_FILE_ENV, os(&pooled))]);
    assert_eq!(signed_in(&pool, Some(&home)), None);
    std::fs::write(&pooled, "{}").unwrap();
    assert_eq!(signed_in(&pool, Some(&home)), Some(Signed::File(pooled)));
    // GROK_HOME moves the home's login.
    let other = tmp.path().join("other");
    let moved = vars(vec![(HOME_ENV, os(&other))]);
    assert_eq!(signed_in(&moved, Some(&home)), None);
    assert_eq!(target(&moved, Some(&home)), Ok(other.join("auth.json")));
    assert!(target(&vars(vec![(HOME_ENV, OsString::from("rel"))]), Some(&home)).is_err());
    assert!(
        target(&pool, Some(&home))
            .unwrap_err()
            .contains("a pooled login")
    );
}

/// A machine already signed in returns at once, and the vendor never runs: an `auth.json`, a
/// credential variable. CONTROL: signed out, the same gate runs it.
#[cfg(target_os = "macos")]
#[test]
fn an_auth_file_signs_in_without_running_the_vendor() {
    let bed = Bed::new("sign");
    std::fs::create_dir_all(bed.login().parent().unwrap()).unwrap();
    std::fs::write(bed.login(), "{\"kept\":1}").unwrap();
    let seen = Seen::default();
    let sandbox = Some(Path::new(login_drive::SANDBOX_EXEC));
    let mut owner = Counted::new(&seen, None);
    let (ended, ledger) = gate_run(&bed, sandbox, (true, true), vec![], &mut owner);
    assert_eq!(ended, Ended::Already(Signed::File(bed.login())));
    assert_eq!(ended.exit(), 0);
    let (keyed, _) = gate_run(
        &bed,
        sandbox,
        (true, true),
        vec![("XAI_API_KEY", OsString::from("xai-k"))],
        &mut owner,
    );
    assert_eq!(keyed, Ended::Already(Signed::Env("XAI_API_KEY")));
    assert_eq!(bed.runs(), 0, "the vendor never ran");
    assert!(ledger.is_empty() && seen.needs.load(Ordering::SeqCst) == 0);
    assert!(!bed.state().join(LOCK_FILE).exists(), "no lock was taken");
    std::fs::remove_file(bed.login()).unwrap();
    let mut owner = Counted::new(&seen, Some(bed.root().join("approve")));
    let (ended, _) = gate_run(&bed, sandbox, (true, true), vec![], &mut owner);
    assert!(
        matches!(ended, Ended::SignedIn { .. }),
        "CONTROL: {ended:?}"
    );
    assert_eq!(bed.runs(), 1, "CONTROL");
}

/// A missing sandbox, or one that cannot apply here, runs nothing and says why; it never falls
/// back to an unsandboxed grok, and reads no ladder (a live one may fetch a browser) for a
/// sign-in that cannot run. CONTROL: with the sandbox there, the ladder was read once and the
/// vendor ran.
#[cfg(target_os = "macos")]
#[test]
fn a_missing_sandbox_runs_nothing() {
    let bed = Bed::new("sign");
    let seen = Seen::default();
    let missing = bed.root().join("no-sandbox-exec");
    for sandbox in [
        None,
        Some(missing.as_path()),
        Some(bed.root()),
        Some(Path::new("/usr/bin/false")),
    ] {
        let mut owner = Counted::new(&seen, None);
        let mut rung = Rung::bare();
        let (ended, ledger) =
            gate_run_with(&bed, sandbox, (true, true), vec![], &mut rung, &mut owner);
        let Ended::NotRun(why) = &ended else {
            panic!("{sandbox:?}: {ended:?}");
        };
        assert_eq!(rung.read, 0, "{sandbox:?}: no ladder was read");
        assert!(
            why.starts_with("Grok Build opens this machine's default browser through the OS")
                && why.ends_with("nothing was run"),
            "{why}"
        );
        assert_eq!(ended.exit(), EXIT_SIGNED_OUT);
        assert!(ledger.is_empty());
    }
    assert_eq!(bed.runs(), 0, "nothing ran");
    assert_eq!(seen.needs.load(Ordering::SeqCst), 0);
    assert!(!bed.login().exists());
    let mut owner = Counted::new(&seen, Some(bed.root().join("approve")));
    let mut rung = Rung::bare();
    let (ended, _) = gate_run_with(
        &bed,
        Some(Path::new(login_drive::SANDBOX_EXEC)),
        (true, true),
        vec![],
        &mut rung,
        &mut owner,
    );
    assert!(
        matches!(ended, Ended::SignedIn { .. }),
        "CONTROL: {ended:?}"
    );
    assert_eq!(rung.read, 1, "CONTROL: the ladder was read once");
    assert_eq!(bed.runs(), 1, "CONTROL");
}

/// Under the gate the vendor's own browser open is stopped (its `open` never runs) while its
/// page and code are read into the one need, and its login is placed 0600 in the default home,
/// with the ledger keeping host, flow and outcome only. CONTROL: the same stand-in run with no
/// sandbox does run `open`.
#[cfg(target_os = "macos")]
#[test]
fn under_the_gate_the_vendors_browser_open_is_stopped_and_its_page_and_code_are_read() {
    let bed = Bed::new("sign");
    let seen = Seen::default();
    let mut owner = Counted::new(&seen, Some(bed.root().join("approve")));
    let (ended, ledger) = gate_run(
        &bed,
        Some(Path::new(login_drive::SANDBOX_EXEC)),
        (true, true),
        vec![],
        &mut owner,
    );
    assert_eq!(
        ended,
        Ended::SignedIn {
            file: bed.login(),
            email: Some("grok-one@example.com".to_owned()),
        }
    );
    assert_eq!(ended.exit(), 0);
    assert!(
        !bed.root().join("opened").exists(),
        "the sandboxed vendor could not run `open`"
    );
    // No row, no browser here: the need is this sign-in's code and page alone. accounts.x.ai
    // answers the headless browser with its block page (design §5 (g)), so no once-ever setup
    // would make the next sign-in automatic, and none leads it.
    let url = "https://accounts.x.ai/oauth2/device?user_code=WMRT-8KQZ";
    assert!(!Cli::Grok.renews_headlessly());
    assert_eq!(
        *seen.texts.lock().unwrap(),
        [format!("approve code WMRT-8KQZ at {url}")]
    );
    assert_eq!(
        *seen.lines.lock().unwrap(),
        [format!(
            "NEEDS YOU: approve this Grok Build sign-in: code WMRT-8KQZ at {url}"
        )]
    );
    assert_eq!(seen.clears.load(Ordering::SeqCst), 1, "the need came down");
    assert_eq!(
        std::fs::read_to_string(bed.login()).unwrap(),
        "{\"stand-in\":true}\n"
    );
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(bed.login()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    // Grok's rung is skipped whatever the row has: its page answers no headless browser.
    assert_eq!(
        ledger,
        [
            "accounts.x.ai device signed-in; row(default-home, no grok row) profile(none) \
             mailbox(none configured) headless-browser(none installed) headless(skipped: \
             accounts.x.ai answers the headless browser with its block page) person(asked)"
        ]
    );
    assert!(!ledger[0].contains("WMRT") && !ledger[0].contains("https://"));
    assert_eq!(read_record(&bed.state()).outcome, "signed-in");
    // CONTROL: the stand-in run plainly (its own staging home, no sandbox) runs `open`.
    let staging = bed.state().join(".signin-grok-control");
    std::fs::create_dir_all(&staging).unwrap();
    let out = aterm_uds::spawnfd::command(&bed.exe)
        .args(&bed.lead)
        .args(["login", "--device-auth"])
        .env("GROK_HOME", &staging)
        .env("GROK_AUTH_PATH", staging.join("auth.json"))
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .output()
        .expect("plain stand-in");
    assert!(out.status.success(), "{out:?}");
    assert!(
        bed.root().join("opened").exists(),
        "CONTROL: unsandboxed, its `open` runs"
    );
}

/// Five callers that wait, at once, start exactly one sign-in child and publish one need: the
/// first leads, the four others wait on the lock and find the login it placed.
#[cfg(target_os = "macos")]
#[test]
fn five_concurrent_waiters_start_one_sign_in_and_publish_one_need() {
    let bed = Bed::new("sign");
    let seen = Seen::default();
    let approve = bed.root().join("approve");
    let results: Vec<Ended> = std::thread::scope(|s| {
        let runs: Vec<_> = (0..5)
            .map(|_| {
                let (bed, seen) = (&bed, &seen);
                s.spawn(move || {
                    let mut owner = Counted::new(seen, None);
                    gate_run(
                        bed,
                        Some(Path::new(login_drive::SANDBOX_EXEC)),
                        (true, false),
                        vec![],
                        &mut owner,
                    )
                    .0
                })
            })
            .collect();
        // The person approves once the need is up and the four others wait (a hang detector).
        let until = Instant::now() + Duration::from_secs(60);
        while (seen.needs.load(Ordering::SeqCst) < 1 || seen.waiting.load(Ordering::SeqCst) < 4)
            && Instant::now() < until
        {
            std::thread::sleep(Duration::from_millis(20));
        }
        std::fs::write(&approve, "").expect("approve");
        runs.into_iter()
            .map(|r| r.join().expect("caller"))
            .collect()
    });
    assert_eq!(bed.runs(), 1, "one sign-in child: {results:?}");
    assert_eq!(seen.needs.load(Ordering::SeqCst), 1, "one need");
    assert_eq!(seen.waiting.load(Ordering::SeqCst), 4, "four waited");
    let led = results
        .iter()
        .filter(|e| matches!(e, Ended::SignedIn { .. }))
        .count();
    let joined = results
        .iter()
        .filter(|e| **e == Ended::SignedInElsewhere(Signed::File(bed.login())))
        .count();
    assert_eq!((led, joined), (1, 4), "{results:?}");
    assert!(results.iter().all(|e| e.exit() == 0));
}

/// A sign-in that ends without a login (an expired code) ends every caller that waited on it,
/// and none starts a second; a background caller then leaves it alone ([`Ended::Backoff`]),
/// and a person's own launch starts one more.
#[cfg(target_os = "macos")]
#[test]
fn a_sign_in_that_ends_unsigned_ends_its_waiters_and_is_never_looped() {
    let bed = Bed::new("expire");
    let seen = Seen::default();
    let approve = bed.root().join("approve");
    let sandbox = Some(Path::new(login_drive::SANDBOX_EXEC));
    let results: Vec<Ended> = std::thread::scope(|s| {
        let runs: Vec<_> = (0..3)
            .map(|_| {
                let (bed, seen) = (&bed, &seen);
                s.spawn(move || {
                    let mut owner = Counted::new(seen, None);
                    gate_run(bed, sandbox, (true, true), vec![], &mut owner).0
                })
            })
            .collect();
        let until = Instant::now() + Duration::from_secs(60);
        while (seen.needs.load(Ordering::SeqCst) < 1 || seen.waiting.load(Ordering::SeqCst) < 2)
            && Instant::now() < until
        {
            std::thread::sleep(Duration::from_millis(20));
        }
        std::fs::write(&approve, "").expect("approve");
        runs.into_iter()
            .map(|r| r.join().expect("caller"))
            .collect()
    });
    assert_eq!(bed.runs(), 1, "{results:?}");
    let unsigned = results
        .iter()
        .filter(|e| matches!(e, Ended::Unsigned { word, .. } if word == "exited-1"))
        .count();
    let waited = results
        .iter()
        .filter(|e| **e == Ended::Waited("exited-1".to_owned()))
        .count();
    assert_eq!((unsigned, waited), (1, 2), "{results:?}");
    assert!(results.iter().all(|e| e.exit() == EXIT_SIGNED_OUT));
    assert!(!bed.login().exists());
    let mut owner = Counted::new(&seen, None);
    let (background, _) = gate_run(&bed, sandbox, (true, false), vec![], &mut owner);
    assert!(
        matches!(&background, Ended::Backoff { outcome, .. } if outcome == "exited-1"),
        "{background:?}"
    );
    assert_eq!(bed.runs(), 1, "a background caller started nothing");
    let (person, _) = gate_run(&bed, sandbox, (true, true), vec![], &mut owner);
    assert!(
        matches!(&person, Ended::Unsigned { .. }),
        "a person's launch starts one more: {person:?}"
    );
    assert_eq!(bed.runs(), 2);
}

/// A caller that does not wait ends [`Ended::Busy`] while another holds the lock, and runs
/// nothing; a held lock alone is no sign-in to a reader ([`running`] reads the leader's record).
/// CONTROL: the lock dropped, it leads.
#[cfg(target_os = "macos")]
#[test]
fn a_caller_that_does_not_wait_is_busy_while_another_signs_in() {
    let bed = Bed::new("sign");
    let seen = Seen::default();
    let held = try_take(lock_file(&bed.state()).unwrap())
        .unwrap()
        .expect("free");
    assert!(!running(&bed.state()), "a holder that leads no sign-in");
    let mut owner = Counted::new(&seen, Some(bed.root().join("approve")));
    let sandbox = Some(Path::new(login_drive::SANDBOX_EXEC));
    let (ended, _) = gate_run(&bed, sandbox, (false, true), vec![], &mut owner);
    assert_eq!(ended, Ended::Busy);
    assert_eq!(ended.exit(), EXIT_BUSY);
    assert_eq!(bed.runs(), 0);
    drop(held);
    assert!(!running(&bed.state()));
    let (ended, _) = gate_run(&bed, sandbox, (false, true), vec![], &mut owner);
    assert!(
        matches!(ended, Ended::SignedIn { .. }),
        "CONTROL: {ended:?}"
    );
}

/// Every setup for `label`, each with and without a window `accounts browser` opens here.
fn every_setup(label: &str) -> Vec<Setup> {
    let label = label.to_owned();
    let mut all = vec![
        Setup::Window(label.clone()),
        Setup::Mailbox(label.clone()),
        Setup::MailboxPassword {
            label: label.clone(),
            login: super::super::mailbox_routes::mask_login("\u{1F600}\u{1F600}@example.test"),
        },
    ];
    for window in [true, false] {
        all.push(Setup::Email {
            label: label.clone(),
            window,
        });
        all.push(Setup::Adopt {
            label: label.clone(),
            window,
        });
    }
    all
}

/// The need's first step after adopting the row or adding its email is one that works on this
/// machine, in the order it runs. With a window `accounts browser` opens here (one installed, or
/// the pinned one fetched first), the terminal's line runs `adopt-default`, then `accounts
/// browser`, the mailbox the alternative. NEGATIVE CONTROL: with none (no window browser and the
/// pinned fetch failed), it runs `adopt-default`, then puts the mailbox in the vault, and names
/// `accounts browser` only after installing a browser, never as the next command; the email
/// setup alike.
#[test]
fn the_setup_names_accounts_browser_only_where_it_opens_a_window() {
    let url = "https://accounts.x.ai/oauth2/device?user_code=WMRT-8KQZ";
    let adopt = "run `aterm harness accounts adopt-default main --email <your x.ai email> --cli \
                 grok`, ";
    let window = needs_you_line(
        Some(&Setup::Adopt {
            label: "main".to_owned(),
            window: true,
        }),
        url,
        Some("WMRT-8KQZ"),
    );
    assert_eq!(
        window,
        format!(
            "NEEDS YOU: once, so Grok Build signs in by itself from now on: {adopt}then run \
             `aterm harness accounts browser main --cli grok` at this machine's desk and sign in \
             there, or put that email's mailbox in the vault (ai-mailbox.toml); or approve this \
             sign-in now: code WMRT-8KQZ at {url}"
        )
    );
    let none = needs_you_line(
        Some(&Setup::Adopt {
            label: "main".to_owned(),
            window: false,
        }),
        url,
        Some("WMRT-8KQZ"),
    );
    assert_eq!(
        none,
        format!(
            "NEEDS YOU: once, so Grok Build signs in by itself from now on: {adopt}then put that \
             email's mailbox in the vault (ai-mailbox.toml), or install a Chromium that opens a \
             window (Google Chrome for Testing or Chromium) and run `aterm harness accounts \
             browser main --cli grok` at this machine's desk; or approve this sign-in now: code \
             WMRT-8KQZ at {url}"
        )
    );
    // The step after the first, in each: the window where it opens, else the mailbox.
    let next = |line: &str| {
        let after = line.split_once("then ").expect("a second step").1;
        after
            .split_once(',')
            .map_or(after, |(step, _)| step)
            .to_owned()
    };
    assert!(next(&window).starts_with("run `aterm harness accounts browser main"));
    assert!(next(&none).starts_with("put that email's mailbox in the vault"));
    assert!(
        none.find("install a Chromium").expect("the alternative")
            < none.find("accounts browser").expect("named after it"),
        "{none}"
    );
    for window in [true, false] {
        let email = needs_you_line(
            Some(&Setup::Email {
                label: "work".to_owned(),
                window,
            }),
            url,
            Some("WMRT-8KQZ"),
        );
        assert!(
            email.starts_with(
                "NEEDS YOU: once, so Grok Build signs in by itself from now on: add `email = \
                 \"…\"` to work's row in Grok Build's roster, then "
            ),
            "{email}"
        );
        let step = next(&email);
        assert_eq!(
            step.starts_with("run `aterm harness accounts browser work --cli grok`"),
            window,
            "{email}"
        );
        assert_eq!(
            step.starts_with("put that email's mailbox in the vault"),
            !window,
            "{email}"
        );
    }
}

/// The need leads with the once-ever setup in short words, then this sign-in's code and page,
/// then the setup's exact command, within the attention cap: the whole page where it fits, else
/// its host, else without the command, for the longest admissible label and code. For the label
/// `adopt-default` gives, every setup's code ends within the menu row's first 69 bytes (its 72,
/// less the ellipsis a cut row ends with; aterm-gui's `needs` test reads the row itself).
#[test]
fn the_need_leads_with_the_setup_and_fits_the_attention_cap() {
    let cap = aterm_types::control_verbs::META_ATTENTION_KEYED_MAX;
    let url = "https://accounts.x.ai/oauth2/device?user_code=WMRT-8KQZ";
    let window = Setup::Window("main".to_owned());
    assert_eq!(
        need_words(Some(&window), url, Some("WMRT-8KQZ")),
        format!(
            "once: sign main in at the desk; or code WMRT-8KQZ at {url} (aterm harness accounts \
             browser main --cli grok)"
        )
    );
    assert_eq!(
        needs_you_line(Some(&window), url, Some("WMRT-8KQZ")),
        format!(
            "NEEDS YOU: once, so Grok Build signs in by itself from now on: run `aterm harness \
             accounts browser main --cli grok` at this machine's desk and sign in there, or put \
             main's mailbox in the vault (ai-mailbox.toml); or approve this sign-in now: code \
             WMRT-8KQZ at {url}"
        )
    );
    let mailbox = Setup::Mailbox("main".to_owned());
    assert_eq!(
        need_words(Some(&mailbox), url, Some("WMRT-8KQZ")),
        format!("once: main's mailbox in the vault; or code WMRT-8KQZ at {url} (ai-mailbox.toml)")
    );
    assert!(
        needs_you_line(Some(&mailbox), url, Some("WMRT-8KQZ")).starts_with(
            "NEEDS YOU: once, so Grok Build signs in by itself from now on: put main's mailbox in \
             the vault (ai-mailbox.toml), or install a Chromium that opens a window"
        )
    );
    assert!(
        need_words(
            Some(&Setup::Email {
                label: "work".to_owned(),
                window: true
            }),
            url,
            Some("WMRT-8KQZ")
        )
        .starts_with("once: an email on work's grok row; or code WMRT-8KQZ at ")
    );
    let adopt = Setup::Adopt {
        label: "default-home".to_owned(),
        window: true,
    };
    assert!(
        need_words(Some(&adopt), url, Some("WMRT-8KQZ")).ends_with(
            "(aterm harness accounts adopt-default default-home --email <you> --cli grok)"
        ) && needs_you_line(Some(&adopt), url, Some("WMRT-8KQZ"))
            .contains("run `aterm harness accounts adopt-default default-home --email")
    );
    for setup in every_setup(ADOPT_LABEL) {
        let text = need_words(Some(&setup), url, Some("WMRT-8KQZ"));
        let at = text.find("WMRT-8KQZ").expect("the code");
        assert!(at + "WMRT-8KQZ".len() <= 69, "{setup:?}: {text}");
        assert!(text.starts_with("once: ") && text.len() <= cap, "{text}");
    }
    let long = format!(
        "https://accounts.x.ai/oauth2/device?user_code=WMRT-8KQZ&{}",
        "x".repeat(300)
    );
    let text = need_words(Some(&window), &long, Some("WMRT-8KQZ"));
    assert_eq!(
        text,
        "once: sign main in at the desk; or code WMRT-8KQZ at https://accounts.x.ai (aterm \
         harness accounts browser main --cli grok)"
    );
    let widest = "a".repeat(64);
    assert!(super::super::accounts::admissible_label(&widest));
    let code = "ABCD-EFGH-IJKL-M";
    for setup in every_setup(&widest) {
        let text = need_words(Some(&setup), &long, Some(code));
        assert!(
            text.len() <= cap && text.contains(&format!("code {code} at https://accounts.x.ai")),
            "{text}"
        );
        assert!(text.starts_with("once: "), "{text}");
    }
    // With no setup (no profile or mailbox gets the rung past Grok's block page): the code and
    // the page alone, the code within the menu row, and the page's host where the page passes
    // the cap.
    assert_eq!(
        need_words(None, url, Some("WMRT-8KQZ")),
        format!("approve code WMRT-8KQZ at {url}")
    );
    assert_eq!(
        needs_you_line(None, url, Some("WMRT-8KQZ")),
        format!("NEEDS YOU: approve this Grok Build sign-in: code WMRT-8KQZ at {url}")
    );
    assert_eq!(
        need_words(None, &long, Some(code)),
        format!("approve code {code} at https://accounts.x.ai")
    );
    assert_eq!(
        cut("ab\u{e9}".to_owned(), 3),
        "ab",
        "a cut keeps whole characters"
    );
    let record = Record {
        generation: 3,
        t: 100,
        outcome: "hung".to_owned(),
        by: false,
    };
    assert_eq!(Record::parse(&record.line()), record);
    assert!(record.unsigned());
    assert!(!Record::default().unsigned());
    let refused = Record {
        outcome: "refused".to_owned(),
        by: true,
        ..record
    };
    assert_eq!(Record::parse(&refused.line()), refused);
    assert!(refused.line().ends_with(" by=headless\n"));
}

/// The ledger's line keeps every rung whole within a sign-in row's bound: at the longest label,
/// a failed fetch's longest reason, a rung skipped for Grok's block page, or the automatic
/// rung's longest reason, and the longest outcome, the person's rung still ends it. A rung's
/// reason is bounded on its own, so a long one never pushes the person's rung off the line.
#[test]
fn the_ledger_line_keeps_every_rung_within_a_sign_in_row() {
    let cap = super::super::accounts::LEDGER_WHY_MAX;
    let mut ladder = Ladder::bare(&format!(
        "none installed; the pinned fetch failed: {}",
        "y".repeat(300)
    ));
    ladder.label = "a".repeat(64);
    ladder.from = RowFrom::Default;
    ladder.email = Some("main@example.com".to_owned());
    ladder.profile = true;
    let fetch_failed = rungs_line(&ladder, "not ready", true);
    let blocked = rungs_line(&ladder, Skip::Blocked.words(), true);
    ladder.browser = Browser::Found;
    let rung_stopped = rungs_line(
        &ladder,
        &headless_words("needs sign-in", &"z".repeat(300)),
        true,
    );
    for rungs in [fetch_failed, blocked, rung_stopped] {
        let line = ledger_line(Some("accounts.x.ai"), "signed-in-elsewhere", &rungs);
        assert!(
            line.len() <= cap && line.ends_with(" person(asked)"),
            "{} bytes: {line}",
            line.len()
        );
        assert!(!line.contains("main@example.com"), "{line}");
    }
}

/// Whether a sign-in runs is read from its leader's record and never from the lock: with the
/// lock free, a record naming this live process (and its start) says one runs, and the lock is
/// still free after the look, so a caller arriving then is never told to wait for nothing. A
/// record of a process that is gone, of another start under the pid, too old, or unreadable says
/// none runs.
#[cfg(unix)]
#[test]
fn running_reads_the_leaders_record_and_never_takes_the_lock() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let state = tmp.path().join("state");
    let lock = lock_file(&state).expect("lock file");
    assert!(!running(&state), "no record");
    let now = clock();
    let leading = Leading::write(&state, now);
    for _ in 0..100 {
        assert!(running(&state), "a live leader's record");
    }
    let lock = match try_take(lock) {
        Ok(Ok(held)) => held,
        other => panic!("the look took the lock: {:?}", other.map(|r| r.is_ok())),
    };
    drop(lock);
    drop(leading);
    assert!(!running(&state), "the leader's record went with it");
    let me = std::process::id();
    let born = birth_word(me);
    let gone = aterm_uds::spawnfd::command("/usr/bin/true")
        .spawn()
        .and_then(|mut c| {
            let pid = c.id();
            c.wait().map(|_| pid)
        })
        .expect("a process that ends");
    let longest = i64::try_from((login_drive::HANG + LOCK_GRACE).as_secs()).unwrap();
    for (text, why) in [
        (format!("pid={me} birth={born} t={now}\n"), ""),
        (
            format!("pid={gone} birth={born} t={now}\n"),
            "its process is gone",
        ),
        (
            format!("pid={me} birth=1.000000 t={now}\n"),
            "another start",
        ),
        (
            format!("pid={me} birth={born} t={}\n", now - longest - 60),
            "too old",
        ),
        (format!("pid={me} t={now}\n"), "no start"),
        ("garbage".to_owned(), "unreadable"),
    ] {
        std::fs::write(state.join(RUNNING_FILE), &text).unwrap();
        assert_eq!(running(&state), why.is_empty(), "{why}: {text}");
    }
}

/// A login grok refused (its grok started after it was written and still drew the sign-in
/// screen) is marked so, and every gate then reads the machine as signed out, until a newer
/// login replaces it. A login written after the walled grok started is one a restart reads.
#[test]
fn a_login_grok_refused_reads_signed_out_until_a_newer_one_replaces_it() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let home = tmp.path().join("home");
    let login = home.join(".grok/auth.json");
    std::fs::create_dir_all(login.parent().unwrap()).unwrap();
    std::fs::write(&login, "{\"expired\":1}").unwrap();
    let none = vars(vec![]);
    let written = std::fs::metadata(&login).unwrap().modified().unwrap();
    let before = written - Duration::from_secs(5);
    let after = written + Duration::from_secs(5);
    assert_eq!(
        wall_login(&none, Some(&home), Some(before)),
        WallLogin::SignedIn(Signed::File(login.clone())),
        "written after the walled grok started: a restart reads it"
    );
    assert_eq!(
        wall_login(&none, Some(&home), None),
        WallLogin::SignedIn(Signed::File(login.clone())),
        "no start known: never judged refused"
    );
    assert_eq!(
        wall_login(&none, Some(&home), Some(after)),
        WallLogin::Refused(login.clone())
    );
    assert_eq!(
        wall_login(
            &vars(vec![("XAI_API_KEY", "k".into())]),
            Some(&home),
            Some(after)
        ),
        WallLogin::SignedIn(Signed::Env("XAI_API_KEY"))
    );
    assert!(
        signed_in(&none, Some(&home)).is_some(),
        "CONTROL: before the mark"
    );
    mark_refused(&login).unwrap();
    assert_eq!(signed_in(&none, Some(&home)), None, "refused");
    assert_eq!(
        wall_login(&none, Some(&home), Some(after)),
        WallLogin::SignedOut
    );
    // A newer login (grok's own sign-in, or the gate's) undoes the mark.
    let marked = std::fs::metadata(refused_marker(&login))
        .unwrap()
        .modified()
        .unwrap();
    std::fs::write(&login, "{\"fresh\":1}").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&login)
        .unwrap()
        .set_modified(marked + Duration::from_secs(1))
        .unwrap();
    assert_eq!(
        signed_in(&none, Some(&home)),
        Some(Signed::File(login.clone()))
    );
}

/// A refused login does not sign the machine in: the gate runs its one sandboxed sign-in, puts
/// the new login in its place and takes the mark down. CONTROL: the same login unmarked is
/// already signed in, and nothing runs.
#[cfg(target_os = "macos")]
#[test]
fn a_refused_login_is_signed_in_again_through_the_gate() {
    let bed = Bed::new("sign");
    std::fs::create_dir_all(bed.login().parent().unwrap()).unwrap();
    std::fs::write(bed.login(), "{\"expired\":1}").unwrap();
    let seen = Seen::default();
    let sandbox = Some(Path::new(login_drive::SANDBOX_EXEC));
    let mut owner = Counted::new(&seen, Some(bed.root().join("approve")));
    let (ended, _) = gate_run(&bed, sandbox, (true, true), vec![], &mut owner);
    assert_eq!(ended, Ended::Already(Signed::File(bed.login())), "CONTROL");
    assert_eq!(bed.runs(), 0, "CONTROL");
    mark_refused(&bed.login()).unwrap();
    let (ended, _) = gate_run(&bed, sandbox, (true, true), vec![], &mut owner);
    assert!(matches!(ended, Ended::SignedIn { .. }), "{ended:?}");
    assert_eq!(bed.runs(), 1);
    assert_eq!(
        std::fs::read_to_string(bed.login()).unwrap(),
        "{\"stand-in\":true}\n",
        "the refused login was replaced"
    );
    assert!(!refused_marker(&bed.login()).exists(), "the mark came down");
    assert!(signed_in(&|_: &str| None, Some(&bed.home())).is_some());
}

/// A caller that waits on a lock held by a caller that runs no sign-in (it only read the files)
/// is not told a sign-in runs; once the lock is free it leads. CONTROL: the waiters of a real
/// sign-in are told (`five_concurrent_waiters_start_one_sign_in_and_publish_one_need`).
#[cfg(target_os = "macos")]
#[test]
fn a_waiter_is_told_of_a_sign_in_only_while_one_runs() {
    let bed = Bed::new("sign");
    let seen = Seen::default();
    let polls = AtomicUsize::new(0);
    let held = try_take(lock_file(&bed.state()).unwrap())
        .unwrap()
        .expect("free");
    let ended = std::thread::scope(|s| {
        let waiter = s.spawn(|| {
            let var = vars(vec![("HOME", bed.home().into_os_string())]);
            let home = bed.home();
            let state = bed.state();
            let polled = || {
                polls.fetch_add(1, Ordering::SeqCst);
                false
            };
            let gate = Gate {
                state: &state,
                home: Some(&home),
                var: &var,
                program: (&bed.exe, &bed.lead),
                sandbox: Some(Path::new(login_drive::SANDBOX_EXEC)),
                hang: Duration::from_secs(60),
                wait: true,
                ack: true,
                stopped: &polled,
                now: &clock,
            };
            let mut owner = Counted::new(&seen, Some(bed.root().join("approve")));
            run(&gate, &mut Rung::bare(), &mut owner, &mut |_| {})
        });
        // The waiter polls the held lock (a hang detector, never a latency budget).
        let until = Instant::now() + Duration::from_secs(60);
        while polls.load(Ordering::SeqCst) < 3 && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(polls.load(Ordering::SeqCst) >= 3, "the waiter never waited");
        assert_eq!(
            seen.waiting.load(Ordering::SeqCst),
            0,
            "told of a sign-in that does not run"
        );
        drop(held);
        waiter.join().expect("waiter")
    });
    assert!(matches!(ended, Ended::SignedIn { .. }), "{ended:?}");
    assert_eq!(seen.waiting.load(Ordering::SeqCst), 0);
    assert_eq!(bed.runs(), 1, "it led the one sign-in");
}

/// The row that signs the default home in is the roster's row with no `dir`, else the pool's
/// active row, else the default home's own key with no email; and the setup the need leads
/// with follows it.
#[test]
fn the_default_home_s_row_is_its_rosters_row_else_the_active_one() {
    let roster = |text: &str| {
        super::super::accounts::Roster::parse_for(Cli::Grok, text).expect("a grok roster")
    };
    let both = roster(
        "[[account]]\nlabel = \"work\"\nclass = \"dedicated\"\ndir = \"/l/work\"\n\
         email = \"work@example.com\"\n\n[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n\
         email = \"main@example.com\"\n",
    );
    assert_eq!(
        pick_row(&both, Some("work")),
        (
            "main".to_owned(),
            RowFrom::Default,
            Some("main@example.com".to_owned())
        ),
        "the default home's own row wins over the active one"
    );
    let dirs = roster("[[account]]\nlabel = \"work\"\nclass = \"dedicated\"\ndir = \"/l/work\"\n");
    assert_eq!(
        pick_row(&dirs, Some("work")),
        ("work".to_owned(), RowFrom::Active, None)
    );
    assert_eq!(
        pick_row(&dirs, Some("gone")),
        (LEDGER_ACCOUNT.to_owned(), RowFrom::None, None),
        "an active label the roster lacks is no row"
    );
    assert_eq!(
        pick_row(&super::super::accounts::Roster::default(), None),
        (LEDGER_ACCOUNT.to_owned(), RowFrom::None, None)
    );
    // A row a renewal moved off the default home (its binding names a `dir`) is no longer the
    // default home's row in the bound roster the gate reads; it is still the active one.
    let mut logins = super::super::accounts::Logins::default();
    logins.labels.insert(
        "main".to_owned(),
        super::super::accounts::Binding {
            dir: Some("/l/main+1".to_owned()),
            ..super::super::accounts::Binding::default()
        },
    );
    let moved = super::super::accounts::bound(&both, &logins);
    assert_eq!(
        pick_row(&moved, None),
        (LEDGER_ACCOUNT.to_owned(), RowFrom::None, None)
    );
    assert_eq!(
        pick_row(&moved, Some("main")),
        (
            "main".to_owned(),
            RowFrom::Active,
            Some("main@example.com".to_owned())
        )
    );
    // The setup adopts the default home under a label the roster does not list.
    assert_eq!(adopt_label(&dirs), ADOPT_LABEL);
    assert_eq!(adopt_label(&both), LEDGER_ACCOUNT, "`main` is listed");
    let mut ladder = Ladder::bare("none installed");
    assert_eq!(
        ladder.setup(),
        Setup::Adopt {
            label: ADOPT_LABEL.to_owned(),
            window: false
        }
    );
    ladder.window = true;
    assert_eq!(
        ladder.setup(),
        Setup::Adopt {
            label: ADOPT_LABEL.to_owned(),
            window: true
        },
        "a window accounts browser opens here"
    );
    ladder.window = false;
    assert!(!ladder.ready());
    ladder.label = "work".to_owned();
    ladder.from = RowFrom::Active;
    assert_eq!(
        ladder.setup(),
        Setup::Email {
            label: "work".to_owned(),
            window: false
        }
    );
    assert!(
        ladder
            .words()
            .contains("row(work, the pool's active, no email)")
    );
    // No email: never ready, whatever else is there (the page asks for one).
    ladder.profile = true;
    ladder.mailbox = true;
    ladder.browser = Browser::Found;
    assert!(!ladder.ready(), "a row with no email");
    ladder.profile = false;
    ladder.mailbox = false;
    ladder.browser = Browser::Missing("none installed".to_owned());
    ladder.email = Some("work@example.com".to_owned());
    assert_eq!(
        ladder.setup(),
        Setup::Mailbox("work".to_owned()),
        "no window browser and none fetchable: `accounts browser` would refuse"
    );
    ladder.window = true;
    assert_eq!(ladder.setup(), Setup::Window("work".to_owned()));
    // The need leads with none of them while Grok's page answers the headless browser with its
    // block page: no profile or mailbox gets the automatic rung past it.
    assert!(!Cli::Grok.renews_headlessly());
    assert_eq!(ladder.need_setup(), None);
    // Ready only with a browser beside a profile or a mailbox.
    ladder.profile = true;
    assert!(!ladder.ready(), "a profile with no browser");
    ladder.browser = Browser::Found;
    assert!(ladder.ready());
    ladder.profile = false;
    assert!(
        !ladder.ready(),
        "a browser with neither profile nor mailbox"
    );
    ladder.mailbox = true;
    assert!(ladder.ready());
    assert!(
        !ladder.words().contains("work@example.com"),
        "the ladder's words never name the email: {}",
        ladder.words()
    );
}

/// THE POSITIVE: the automatic rung approves the page as the row (its email handed to it, its
/// deadline cut to AUTO_WITHIN of a sign-in whose own hang detector is longer, so the person
/// keeps the rest of the code's life), grok's login is placed in the default home, and no person
/// is asked: no need, and the ledger says so.
/// The rung stands in for a vendor whose page answers the headless browser ([`Rung::ready`]), the
/// only one whose rung the gate runs: Grok Build's is skipped.
#[cfg(target_os = "macos")]
#[test]
fn an_automatic_approval_signs_in_and_asks_nobody() {
    let bed = Bed::new("sign");
    let seen = Seen::default();
    let mut owner = Counted::new(&seen, None);
    let mut rung = Rung::ready(
        bed.root(),
        "grok-one@example.com",
        Approval::Approved { page: true },
    );
    let hang = AUTO_WITHIN * 4;
    let (ended, ledger) = gate_run_as(
        &bed,
        Some(Path::new(login_drive::SANDBOX_EXEC)),
        (true, false),
        (hang, &never),
        vec![],
        &mut rung,
        &mut owner,
    );
    assert_eq!(
        ended,
        Ended::SignedIn {
            file: bed.login(),
            email: Some("grok-one@example.com".to_owned()),
        }
    );
    assert_eq!(rung.asked, 1);
    assert_eq!(rung.email.as_deref(), Some("grok-one@example.com"));
    assert_eq!(rung.fetch, Some(true), "the ladder may fetch a browser");
    let left = rung.left.expect("asked");
    assert!(
        left <= AUTO_WITHIN && left > AUTO_WITHIN - Duration::from_secs(60),
        "the rung's deadline is AUTO_WITHIN away, not the sign-in's {hang:?}: {left:?}"
    );
    assert_eq!(seen.needs.load(Ordering::SeqCst), 0, "nobody was asked");
    assert_eq!(
        std::fs::read_to_string(bed.login()).unwrap(),
        "{\"stand-in\":true}\n"
    );
    assert_eq!(
        ledger,
        [
            "accounts.x.ai device signed-in; row(main, the default home's) profile(ready) \
             mailbox(none configured) headless-browser(found) headless(approved) person(not \
             asked)"
        ]
    );
    assert!(!ledger[0].contains("grok-one@example.com"));
    assert_eq!(bed.runs(), 1);
}

/// NEGATIVE CONTROL: the automatic rung's refusal (another code, another account) ends the
/// sign-in fail-closed: grok is ended, no need goes up, nothing is placed, the end says the way
/// out, and the next background caller backs off. A person's own launch after it skips the rung
/// once and asks them, so a refusing profile never leaves the machine with no way in; the launch
/// after that asks the rung again. CONTROL: a rung that approves signs in with no need.
/// The rung stands in for a vendor whose page answers the headless browser ([`Rung::ready`]), the
/// only one whose rung the gate runs: Grok Build's is skipped.
#[cfg(target_os = "macos")]
#[test]
fn a_refused_automatic_approval_ends_fail_closed() {
    let bed = Bed::new("sign");
    let seen = Seen::default();
    let sandbox = Some(Path::new(login_drive::SANDBOX_EXEC));
    let mut owner = Counted::new(&seen, Some(bed.root().join("approve")));
    let mut rung = Rung::ready(
        bed.root(),
        "grok-one@example.com",
        Approval::Refused("the page shows another code".to_owned()),
    );
    let (ended, ledger) = gate_run_with(&bed, sandbox, (true, true), vec![], &mut rung, &mut owner);
    let refused = |e: &Ended, says: &str| matches!(e, Ended::Unsigned { word, why } if word == "refused" && why.contains(says));
    assert!(refused(&ended, "another code"), "{ended:?}");
    assert!(
        refused(&ended, "the next launch from a terminal asks you"),
        "the end says the way out: {ended:?}"
    );
    assert!(
        refused(&ended, &refused_way_out("main", true)),
        "a window opens here, so it names `accounts browser`: {ended:?}"
    );
    assert_eq!(ended.exit(), EXIT_SIGNED_OUT);
    assert_eq!(rung.asked, 1);
    assert_eq!(
        seen.needs.load(Ordering::SeqCst),
        0,
        "no need after a refusal"
    );
    assert!(!bed.login().exists(), "nothing was placed");
    assert!(!bed.root().join("approve").exists(), "nothing was approved");
    assert!(
        ledger[0].starts_with("accounts.x.ai device refused; ")
            && ledger[0]
                .contains("headless(refused: the page shows another code) person(not asked)"),
        "{ledger:?}"
    );
    let record = read_record(&bed.state());
    assert_eq!((record.outcome.as_str(), record.by), ("refused", true));
    let (background, _) = gate_run_with(
        &bed,
        sandbox,
        (true, false),
        vec![],
        &mut rung.clone(),
        &mut owner,
    );
    assert!(
        matches!(&background, Ended::Backoff { outcome, .. } if outcome == "refused"),
        "{background:?}"
    );
    assert_eq!(bed.runs(), 1, "never looped");
    // A person's launch: the rung is skipped (its browser not fetched), and the person approves.
    let mut again = rung.clone();
    let (person, ledger) =
        gate_run_with(&bed, sandbox, (true, true), vec![], &mut again, &mut owner);
    assert!(matches!(person, Ended::SignedIn { .. }), "{person:?}");
    assert_eq!(again.asked, 1, "the refusing rung was not asked again");
    assert_eq!(
        again.fetch,
        Some(false),
        "no browser fetched for a skipped rung"
    );
    assert_eq!(seen.needs.load(Ordering::SeqCst), 1, "the person was asked");
    assert!(
        ledger[0].contains("headless(skipped: the last automatic try was refused) person(asked)"),
        "{ledger:?}"
    );
    assert!(!read_record(&bed.state()).by);
    // The next sign-in asks the rung again. CONTROL: one that approves asks nobody.
    std::fs::remove_file(bed.login()).unwrap();
    std::fs::remove_file(bed.root().join("approve")).unwrap();
    let mut control = Rung::ready(
        bed.root(),
        "grok-one@example.com",
        Approval::Approved { page: true },
    );
    let (ended, _) = gate_run_with(
        &bed,
        sandbox,
        (true, true),
        vec![],
        &mut control,
        &mut owner,
    );
    assert!(
        matches!(ended, Ended::SignedIn { .. }),
        "CONTROL: {ended:?}"
    );
    assert_eq!(control.asked, 1, "CONTROL");
    assert_eq!(seen.needs.load(Ordering::SeqCst), 1, "CONTROL");
}

/// NEGATIVE CONTROL: a page aterm approved by itself that grok then signs ANOTHER account in on
/// (its report names another email than the row's) is refused: nothing is placed and no need
/// goes up.
/// The rung stands in for a vendor whose page answers the headless browser ([`Rung::ready`]), the
/// only one whose rung the gate runs: Grok Build's is skipped.
#[cfg(target_os = "macos")]
#[test]
fn grok_signing_another_account_in_after_an_automatic_approval_is_refused() {
    let bed = Bed::new("sign");
    let seen = Seen::default();
    let mut owner = Counted::new(&seen, None);
    let mut rung = Rung::ready(
        bed.root(),
        "someone-else@example.com",
        Approval::Approved { page: false },
    );
    let (ended, ledger) = gate_run_with(
        &bed,
        Some(Path::new(login_drive::SANDBOX_EXEC)),
        (true, true),
        vec![],
        &mut rung,
        &mut owner,
    );
    let says = "another account than the one main's row names";
    assert!(
        matches!(&ended, Ended::Unsigned { word, why } if word == "refused" && why.contains(says)),
        "{ended:?}"
    );
    assert!(
        !bed.login().exists(),
        "another account's login was not placed"
    );
    assert_eq!(seen.needs.load(Ordering::SeqCst), 0);
    assert!(
        ledger[0].starts_with("accounts.x.ai device refused; "),
        "{ledger:?}"
    );
    assert!(
        !ledger[0].contains("someone-else") && !ledger[0].contains("grok-one"),
        "{ledger:?}"
    );
    assert!(read_record(&bed.state()).by, "the rung's refusal");
}

/// NEGATIVE CONTROL: a rung that pressed the page and then stopped for a person (its confirm
/// landed in its last seconds and the page never said so) is still checked: grok signing
/// another account in than the row's is refused and nothing is placed, whoever the record says
/// was asked. CONTROL: where the rung never ran (no route ready), the person's own approval is
/// theirs, and the same report is placed.
/// The rung stands in for a vendor whose page answers the headless browser ([`Rung::ready`]), the
/// only one whose rung the gate runs: Grok Build's is skipped.
#[cfg(target_os = "macos")]
#[test]
fn a_rung_that_pressed_and_then_stopped_is_still_held_to_the_rows_account() {
    let bed = Bed::new("sign");
    let seen = Seen::default();
    let sandbox = Some(Path::new(login_drive::SANDBOX_EXEC));
    let mut owner = Counted::new(&seen, None);
    let mut rung = Rung {
        act: Act::PressThenSay,
        ..Rung::ready(
            bed.root(),
            "someone-else@example.com",
            Approval::NeedsYou("the sign-in page was not approved in time".to_owned()),
        )
    };
    let (ended, ledger) = gate_run_with(&bed, sandbox, (true, true), vec![], &mut rung, &mut owner);
    assert!(
        matches!(&ended, Ended::Unsigned { word, why } if word == "refused" && why.contains("another account")),
        "{ended:?}"
    );
    assert!(
        !bed.login().exists(),
        "another account's login was not placed"
    );
    assert!(
        ledger[0].contains("headless(needs you: the sign-in page was not approved in time)"),
        "{ledger:?}"
    );
    // CONTROL: no route ready, so only a person could have approved: theirs is placed.
    let bed = Bed::new("sign");
    let mut owner = Counted::new(&seen, Some(bed.root().join("approve")));
    let mut person = Rung::ready(
        bed.root(),
        "someone-else@example.com",
        Approval::Approved { page: true },
    );
    person.ladder.browser = Browser::Missing("none installed".to_owned());
    let (ended, _) = gate_run_with(&bed, sandbox, (true, true), vec![], &mut person, &mut owner);
    assert_eq!(person.asked, 0, "CONTROL: the rung never ran");
    assert_eq!(
        ended,
        Ended::SignedIn {
            file: bed.login(),
            email: Some("grok-one@example.com".to_owned()),
        },
        "CONTROL"
    );
}

/// A stop is heard on every rung, and nothing runs after it: a Ctrl-C while the pinned browser
/// downloads starts no grok; one while the automatic rung runs ends it (its `finished` comes
/// true) with nothing placed; one just before the need goes up raises none. Each ends
/// `stopped`, asks nobody and places nothing. CONTROL: unstopped, the same gate runs grok.
/// The rung stands in for a vendor whose page answers the headless browser ([`Rung::ready`]), the
/// only one whose rung the gate runs: Grok Build's is skipped.
#[cfg(target_os = "macos")]
#[test]
fn a_stop_ends_every_rung_and_nothing_runs_after_it() {
    let sandbox = Some(Path::new(login_drive::SANDBOX_EXEC));
    let stopped_end = |e: &Ended| matches!(e, Ended::Unsigned { word, .. } if word == "stopped");
    // During the browser's fetch (the ladder's read).
    let bed = Bed::new("sign");
    let seen = Seen::default();
    let mut owner = Counted::new(&seen, Some(bed.root().join("approve")));
    let mut rung = Rung {
        stop_in_ladder: true,
        ..Rung::ready(
            bed.root(),
            "grok-one@example.com",
            Approval::Approved { page: true },
        )
    };
    let stop = Arc::clone(&rung.stop);
    let stopped = move || stop.load(Ordering::SeqCst);
    let (ended, ledger) = gate_run_as(
        &bed,
        sandbox,
        (true, true),
        (Duration::from_secs(60), &stopped),
        vec![],
        &mut rung,
        &mut owner,
    );
    assert!(stopped_end(&ended), "fetch: {ended:?}");
    assert_eq!(bed.runs(), 0, "fetch: grok never started after the stop");
    assert_eq!(rung.asked, 0, "fetch");
    assert!(
        ledger[0].starts_with("no-page device stopped; ")
            && ledger[0].ends_with("headless(not reached) person(not asked)"),
        "{ledger:?}"
    );
    // While the automatic rung runs.
    let bed = Bed::new("sign");
    let mut owner = Counted::new(&seen, Some(bed.root().join("approve")));
    let mut rung = Rung {
        act: Act::StopWhileRunning,
        ..Rung::ready(
            bed.root(),
            "grok-one@example.com",
            Approval::Approved { page: true },
        )
    };
    let stop = Arc::clone(&rung.stop);
    let stopped = move || stop.load(Ordering::SeqCst);
    let (ended, ledger) = gate_run_as(
        &bed,
        sandbox,
        (true, true),
        (Duration::from_secs(60), &stopped),
        vec![],
        &mut rung,
        &mut owner,
    );
    assert!(stopped_end(&ended), "rung: {ended:?}");
    assert!(rung.saw_finished, "rung: the stop made its `finished` true");
    assert!(!bed.login().exists(), "rung: nothing placed");
    assert!(
        ledger[0].contains("headless(stopped) person(not asked)"),
        "{ledger:?}"
    );
    // Just before the need goes up (right after the ladder is told).
    let bed = Bed::new("sign");
    let mut owner = Counted::new(&seen, Some(bed.root().join("approve")));
    let stop = Arc::new(AtomicBool::new(false));
    owner.stop_on_say = Some(Arc::clone(&stop));
    let stopped = move || stop.load(Ordering::SeqCst);
    let (ended, _) = gate_run_as(
        &bed,
        sandbox,
        (true, true),
        (Duration::from_secs(60), &stopped),
        vec![],
        &mut Rung::bare(),
        &mut owner,
    );
    assert!(stopped_end(&ended), "need: {ended:?}");
    assert!(!bed.login().exists(), "need: nothing placed");
    assert_eq!(
        seen.needs.load(Ordering::SeqCst),
        0,
        "no need after any stop"
    );
    // CONTROL: unstopped, grok runs and the rung approves.
    let bed = Bed::new("sign");
    let mut owner = Counted::new(&seen, None);
    let mut rung = Rung::ready(
        bed.root(),
        "grok-one@example.com",
        Approval::Approved { page: true },
    );
    let (ended, _) = gate_run_with(&bed, sandbox, (true, true), vec![], &mut rung, &mut owner);
    assert!(
        matches!(ended, Ended::SignedIn { .. }),
        "CONTROL: {ended:?}"
    );
    assert_eq!(bed.runs(), 1, "CONTROL");
}

/// A stop for a person (the row's profile signed out of x.ai, or a page only a person can pass)
/// goes on to the one need, this sign-in's code and page with no once-ever setup ahead of it
/// (Grok's need leads with none, `Ladder::need_setup`), and the person's approval of the same
/// code signs the machine in; the ledger names both rungs.
/// The rung stands in for a vendor whose page answers the headless browser ([`Rung::ready`]), the
/// only one whose rung the gate runs: Grok Build's is skipped.
#[cfg(target_os = "macos")]
#[test]
fn a_stop_for_a_person_goes_on_to_the_need_with_the_same_code() {
    for (said, word) in [
        (
            Approval::NeedsSignIn("the vendor's sign-in page waited on a person".to_owned()),
            "headless(needs sign-in: the vendor's sign-in page waited on a person)",
        ),
        (
            Approval::NeedsYou("a passkey prompt".to_owned()),
            "headless(needs you: a passkey prompt)",
        ),
    ] {
        let bed = Bed::new("sign");
        let seen = Seen::default();
        let mut owner = Counted::new(&seen, Some(bed.root().join("approve")));
        let mut rung = Rung::ready(bed.root(), "grok-one@example.com", said);
        let (ended, ledger) = gate_run_with(
            &bed,
            Some(Path::new(login_drive::SANDBOX_EXEC)),
            (true, true),
            vec![],
            &mut rung,
            &mut owner,
        );
        assert!(matches!(ended, Ended::SignedIn { .. }), "{word}: {ended:?}");
        assert_eq!(rung.asked, 1, "{word}");
        assert_eq!(seen.needs.load(Ordering::SeqCst), 1, "{word}: one need");
        let text = seen.texts.lock().unwrap()[0].clone();
        let line = seen.lines.lock().unwrap()[0].clone();
        assert!(
            text.starts_with("approve code WMRT-8KQZ at https://accounts.x.ai/"),
            "{word}: {text}"
        );
        assert!(
            line.starts_with(
                "NEEDS YOU: approve this Grok Build sign-in: code WMRT-8KQZ at \
                 https://accounts.x.ai/"
            ),
            "{word}: {line}"
        );
        assert!(
            ledger[0].starts_with("accounts.x.ai device signed-in; ")
                && ledger[0].contains(&format!("{word} person(asked)")),
            "{ledger:?}"
        );
    }
}

/// Cloudflare's bot check that did not clear ends the sign-in `bot-check` with no need: nobody
/// is asked to pass it (design R-13), nothing is placed, and the ledger names it, with the
/// person not asked. CONTROL: a stop for a person on the same gate raises the one need
/// ([`a_stop_for_a_person_goes_on_to_the_need_with_the_same_code`]).
/// The rung stands in for a vendor whose page answers the headless browser ([`Rung::ready`]), the
/// only one whose rung the gate runs: Grok Build's is skipped.
#[cfg(target_os = "macos")]
#[test]
fn a_bot_check_ends_the_sign_in_on_record_and_asks_nobody() {
    let sandbox = Some(Path::new(login_drive::SANDBOX_EXEC));
    let bed = Bed::new("sign");
    let seen = Seen::default();
    let mut owner = Counted::new(&seen, Some(bed.root().join("approve")));
    let said = "Cloudflare's bot check on `accounts.x.ai/sign-in` did not clear in 90 s; aterm \
                never tries to pass it";
    let mut rung = Rung::ready(
        bed.root(),
        "grok-one@example.com",
        Approval::BotCheck(said.to_owned()),
    );
    let (ended, ledger) = gate_run_with(&bed, sandbox, (true, true), vec![], &mut rung, &mut owner);
    assert!(
        matches!(&ended, Ended::Unsigned { word, why } if word == "bot-check"
            && why.contains("Cloudflare") && !why.contains("person")),
        "{ended:?}"
    );
    assert_eq!(rung.asked, 1);
    assert_eq!(seen.needs.load(Ordering::SeqCst), 0, "no need goes up");
    assert!(seen.lines.lock().unwrap().is_empty(), "no NEEDS YOU line");
    assert!(!bed.login().exists(), "nothing is placed");
    assert!(
        ledger[0].starts_with("accounts.x.ai device bot-check; ")
            && ledger[0].contains("headless(bot check: Cloudflare")
            && ledger[0].ends_with("person(not asked)"),
        "{ledger:?}"
    );
    assert_eq!(read_record(&bed.state()).outcome, "bot-check");
    assert!(!read_record(&bed.state()).by, "not the rung's refusal");
}

/// With no headless browser, or neither a profile nor a mailbox, the automatic rung is never
/// asked and the need goes up at once, with this sign-in's code. CONTROL: the same row with both
/// asks the rung first.
/// The rung stands in for a vendor whose page answers the headless browser ([`Rung::ready`]), the
/// only one whose rung the gate runs: Grok Build's is skipped.
#[cfg(target_os = "macos")]
#[test]
fn with_no_route_ready_the_rung_is_skipped_and_the_need_goes_up_at_once() {
    let unready = |root: &Path, ladder: fn(&mut Ladder)| {
        let mut rung = Rung::ready(
            root,
            "grok-one@example.com",
            Approval::Approved { page: true },
        );
        ladder(&mut rung.ladder);
        rung
    };
    for (what, mut rung) in [
        (
            "no browser",
            unready(Path::new("/nowhere"), |l| {
                l.browser = Browser::Missing("none installed".to_owned());
            }),
        ),
        (
            "no profile or mailbox",
            unready(Path::new("/nowhere"), |l| {
                l.profile = false;
                l.mailbox = false;
            }),
        ),
    ] {
        let bed = Bed::new("sign");
        let seen = Seen::default();
        let mut owner = Counted::new(&seen, Some(bed.root().join("approve")));
        let (ended, ledger) = gate_run_with(
            &bed,
            Some(Path::new(login_drive::SANDBOX_EXEC)),
            (true, true),
            vec![],
            &mut rung,
            &mut owner,
        );
        assert!(matches!(ended, Ended::SignedIn { .. }), "{what}: {ended:?}");
        assert_eq!(rung.asked, 0, "{what}: the rung was never asked");
        assert_eq!(seen.needs.load(Ordering::SeqCst), 1, "{what}");
        assert!(
            seen.texts.lock().unwrap()[0].starts_with("approve code WMRT-8KQZ at "),
            "{what}"
        );
        assert!(
            ledger[0].contains("headless(not ready) person(asked)"),
            "{what}: {ledger:?}"
        );
    }
    // CONTROL: ready, the rung is asked first (and here it approves, so nobody is).
    let bed = Bed::new("sign");
    let seen = Seen::default();
    let mut owner = Counted::new(&seen, None);
    let mut rung = Rung::ready(
        bed.root(),
        "grok-one@example.com",
        Approval::Approved { page: true },
    );
    let (ended, _) = gate_run_with(
        &bed,
        Some(Path::new(login_drive::SANDBOX_EXEC)),
        (true, true),
        vec![],
        &mut rung,
        &mut owner,
    );
    assert!(
        matches!(ended, Ended::SignedIn { .. }),
        "CONTROL: {ended:?}"
    );
    assert_eq!(
        (rung.asked, seen.needs.load(Ordering::SeqCst)),
        (1, 0),
        "CONTROL"
    );
}

/// accounts.x.ai answers aterm's headless browser with Cloudflare's block page (design §5 (g);
/// `Cli::renews_headlessly` is false for Grok, and the machine's rung, [`LiveAuto`], says its
/// page does not answer), so the automatic rung could only load xAI's page and fail. A Grok
/// sign-in whose row has an email, a profile and a mailbox, with no headless browser installed,
/// asks the rung for no approval and lets its ladder fetch no browser; the person's need goes up
/// at once with this sign-in's code and page (no once-ever setup), and their approval signs the
/// machine in. The terminal and the record say why the rung did not run. With a headless browser
/// installed, where the rung's ladder is ready, the rung still approves nothing and the person is
/// asked. CONTROL: the same row for a vendor whose page answers (the stand-in's `answers`)
/// fetches the pinned browser and asks the rung first, which approves, and nobody is asked.
#[cfg(target_os = "macos")]
#[test]
fn grok_s_block_page_skips_the_rung_and_its_fetch_and_asks_the_person() {
    assert!(!Cli::Grok.renews_headlessly(), "accounts.x.ai's block page");
    assert!(
        !LiveAuto::new(None, None, None, 0).page_answers(),
        "the machine's rung is Grok Build's"
    );
    let sandbox = Some(Path::new(login_drive::SANDBOX_EXEC));
    let row = |root: &Path, answers: bool| {
        let mut rung = Rung {
            answers,
            fetchable: true,
            ..Rung::ready(
                root,
                "grok-one@example.com",
                Approval::Approved { page: true },
            )
        };
        rung.ladder.mailbox = true;
        rung.ladder.browser = Browser::Missing("none installed".to_owned());
        rung
    };
    let bed = Bed::new("sign");
    let seen = Seen::default();
    let mut owner = Counted::new(&seen, Some(bed.root().join("approve")));
    let mut rung = row(bed.root(), Cli::Grok.renews_headlessly());
    let (ended, ledger) = gate_run_with(&bed, sandbox, (true, true), vec![], &mut rung, &mut owner);
    assert_eq!(
        ended,
        Ended::SignedIn {
            file: bed.login(),
            email: Some("grok-one@example.com".to_owned()),
        }
    );
    assert_eq!(rung.asked, 0, "the rung approved nothing");
    assert_eq!(
        (rung.fetch, rung.fetched),
        (Some(false), 0),
        "no browser was fetched for it"
    );
    assert_eq!(seen.needs.load(Ordering::SeqCst), 1, "the person was asked");
    let url = "https://accounts.x.ai/oauth2/device?user_code=WMRT-8KQZ";
    assert_eq!(
        *seen.texts.lock().unwrap(),
        [format!("approve code WMRT-8KQZ at {url}")]
    );
    assert_eq!(
        *seen.lines.lock().unwrap(),
        [format!(
            "NEEDS YOU: approve this Grok Build sign-in: code WMRT-8KQZ at {url}"
        )]
    );
    assert_eq!(
        ledger,
        [
            "accounts.x.ai device signed-in; row(main, the default home's) profile(ready) \
             mailbox(ready) headless-browser(none installed) headless(skipped: accounts.x.ai \
             answers the headless browser with its block page) person(asked)"
        ]
    );
    assert!(
        seen.said
            .lock()
            .unwrap()
            .iter()
            .any(|line| line.ends_with(Skip::Blocked.told())),
        "{:?}",
        seen.said.lock().unwrap()
    );
    assert_eq!(bed.runs(), 1);
    // The same row on a machine whose headless browser is installed: its ladder is ready, and
    // the gate alone keeps the rung from approving.
    let bed = Bed::new("sign");
    let seen = Seen::default();
    let mut owner = Counted::new(&seen, Some(bed.root().join("approve")));
    let mut rung = row(bed.root(), Cli::Grok.renews_headlessly());
    rung.ladder.browser = Browser::Found;
    assert!(rung.ladder.ready(), "the rung's own routes are all there");
    let (ended, ledger) = gate_run_with(&bed, sandbox, (true, true), vec![], &mut rung, &mut owner);
    assert!(matches!(ended, Ended::SignedIn { .. }), "{ended:?}");
    assert_eq!(rung.asked, 0, "the rung approved nothing");
    assert_eq!(
        (rung.fetch, rung.fetched),
        (Some(false), 0),
        "no browser was fetched for it"
    );
    assert_eq!(seen.needs.load(Ordering::SeqCst), 1, "the person was asked");
    assert_eq!(
        *seen.texts.lock().unwrap(),
        [format!("approve code WMRT-8KQZ at {url}")]
    );
    assert!(
        ledger[0].ends_with(
            "headless-browser(found) headless(skipped: accounts.x.ai answers the headless browser \
             with its block page) person(asked)"
        ),
        "{ledger:?}"
    );
    // CONTROL: a vendor whose page answers fetches the pinned browser and runs the rung.
    let bed = Bed::new("sign");
    let seen = Seen::default();
    let mut owner = Counted::new(&seen, None);
    let mut rung = row(bed.root(), true);
    let (ended, ledger) = gate_run_with(&bed, sandbox, (true, true), vec![], &mut rung, &mut owner);
    assert!(
        matches!(ended, Ended::SignedIn { .. }),
        "CONTROL: {ended:?}"
    );
    assert_eq!(
        (rung.fetch, rung.fetched, rung.asked),
        (Some(true), 1, 1),
        "CONTROL"
    );
    assert_eq!(seen.needs.load(Ordering::SeqCst), 0, "CONTROL");
    assert!(
        ledger[0].ends_with(
            "headless-browser(fetched the pinned one) headless(approved) person(not asked)"
        ),
        "CONTROL: {ledger:?}"
    );
}

/// The browser rung fetches the pinned shell only where none is installed and the row's route
/// waits on a browser alone; a failed fetch is no browser, saying why, and an installed one is
/// never fetched over.
#[test]
fn the_pinned_browser_is_fetched_only_where_a_route_waits_on_it() {
    let fetched = std::cell::Cell::new(0);
    let fetch = |answer: Result<PathBuf, String>| {
        let fetched = &fetched;
        move || {
            fetched.set(fetched.get() + 1);
            answer
        }
    };
    assert_eq!(
        browser_rung(true, true, fetch(Ok(PathBuf::from("/b")))),
        Browser::Found
    );
    assert_eq!(
        browser_rung(false, false, fetch(Ok(PathBuf::from("/b")))),
        Browser::Missing("none installed".to_owned())
    );
    assert_eq!(fetched.get(), 0, "found, or not wanted: nothing fetched");
    assert_eq!(
        browser_rung(false, true, fetch(Ok(PathBuf::from("/b")))),
        Browser::Fetched
    );
    let failed = browser_rung(
        false,
        true,
        fetch(Err(
            "the download is 3 bytes and the pin is 9; nothing was installed".to_owned(),
        )),
    );
    assert_eq!(
        failed,
        Browser::Missing(
            "none installed; the pinned fetch failed: the download is 3 bytes and the pin is 9; \
             nothing was installed"
                .to_owned()
        )
    );
    assert_eq!(fetched.get(), 2);
    let mut ladder = Ladder::bare("unused");
    ladder.email = Some("main@example.com".to_owned());
    ladder.profile = true;
    ladder.browser = Browser::Fetched;
    assert!(ladder.ready(), "a fetched browser drives the rung");
    ladder.browser = failed;
    assert!(!ladder.ready());
    assert!(
        ladder
            .words()
            .contains("headless-browser(none installed; the pinned fetch failed")
    );
}

/// The machine's rung reads, before any browser, the row from the renewal-bound roster and the
/// profile `accounts browser` opens for it (`row_browser_profile`, under Claude Code's store
/// beside Grok Build's), and lets the approver type the row's email only where a mailbox reads
/// the code that brings; a row with no email offers no profile and no mailbox, whatever is on
/// disk; a renewal's binding that moves the row off the default home leaves it no longer that
/// home's row.
#[cfg(unix)]
#[test]
fn the_live_rung_reads_the_profile_accounts_browser_opens() {
    use super::super::accounts::{self, Store};
    use super::super::headless_oauth::row_browser_profile;
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let home = tmp.path().join("home");
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&state).unwrap();
    let roster = tmp.path().join("grok-accounts.toml");
    std::fs::write(
        &roster,
        "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\nemail = \"main@example.com\"\n",
    )
    .unwrap();
    let store = Store::at_for(&state, Cli::Grok, &home);
    let now = clock();
    let live = LiveAuto::new(
        Some(roster.clone()),
        Some(store.clone()),
        Some(home.clone()),
        now,
    );
    let read = live.row().expect("a store");
    assert_eq!(
        (read.label.as_str(), read.from, read.email.as_deref()),
        ("main", RowFrom::Default, Some("main@example.com"))
    );
    assert!(!read.profile, "no profile yet");
    assert_eq!(
        read.approver.types_email, read.mailbox,
        "the email is typed only where a mailbox reads the code it brings"
    );
    let profile = row_browser_profile(
        Cli::Grok,
        &store.dir,
        &store.shared_dir(),
        "main",
        Some("main@example.com"),
    );
    assert_eq!(read.approver.profile_dir(Some("main@example.com")), profile);
    std::fs::create_dir_all(&profile).unwrap();
    assert!(
        live.row().expect("a store").profile,
        "the profile accounts browser signed in"
    );
    // A row with no email: its label's profile is never one a person signed in.
    std::fs::write(
        &roster,
        "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(row_browser_profile(
        Cli::Grok,
        &store.dir,
        &store.shared_dir(),
        "main",
        None,
    ))
    .unwrap();
    let read = live.row().expect("a store");
    assert_eq!(read.email, None);
    assert!(!read.profile && !read.mailbox && !read.approver.types_email);
    // A renewal binds `main` to a pooled login: it is no longer the default home's row.
    std::fs::write(
        &roster,
        "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\nemail = \"main@example.com\"\n",
    )
    .unwrap();
    let pooled = store.logins_dir().join("main");
    store
        .transact_logins_within(&accounts::LOCK_LADDER, now, &mut None, |_, logins| {
            logins.labels.insert(
                "main".to_owned(),
                accounts::Binding {
                    dir: Some(pooled.to_string_lossy().into_owned()),
                    ..accounts::Binding::default()
                },
            );
            Ok(Vec::new())
        })
        .expect("a binding");
    let read = live.row().expect("a store");
    assert_eq!(
        (read.label.as_str(), read.from),
        (LEDGER_ACCOUNT, RowFrom::None)
    );
}

/// The machine's rung takes the one sign-in lock every CLI's headless sign-ins share before its
/// browser runs: while another of aterm's sign-ins holds it, the rung waits and, at its
/// deadline, stops for a person, naming the lock; a grok that ends meanwhile ends the wait.
/// Nothing here can launch a browser: the page's URL names two clients, which the approver
/// refuses before any browser starts. CONTROL: the lock free, the approver is reached (and
/// refuses that URL).
#[cfg(unix)]
#[test]
fn the_live_rung_waits_for_the_machines_sign_in_lock() {
    use super::super::accounts::Store;
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let home = tmp.path().join("home");
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&home).unwrap();
    let roster = tmp.path().join("grok-accounts.toml");
    std::fs::write(
        &roster,
        "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\nemail = \"main@example.com\"\n",
    )
    .unwrap();
    let store = Store::at_for(&state, Cli::Grok, &home);
    let mut live = LiveAuto::new(
        Some(roster),
        Some(store.clone()),
        Some(home.clone()),
        clock(),
    );
    let ladder = live.ladder(false, &|| false);
    assert_eq!(ladder.label, "main");
    // Its window is what `accounts browser` does here: a desk that opens one, and one installed
    // or the pinned one fetched, where no fetch of it failed (none has in this scratch home's
    // cache).
    assert_eq!(
        ladder.window,
        super::super::browser_window::window_route(Some(&home)).reachable()
    );
    if super::super::headless_fetch::Archive::host_window().is_some()
        && std::env::var_os("XDG_CACHE_HOME").is_none()
        && super::super::browser_window::no_window_desk().is_none()
        && super::super::headless_fetch::curl_present()
    {
        assert!(ladder.window, "fetchable with no browser installed");
    }
    let url = "https://accounts.x.ai/oauth2/device?client_id=a&client_id=b&user_code=WMRT-8KQZ";
    let mut approve = |finished: &'static dyn Fn() -> bool, within: Duration| -> Approval {
        let deadline = Instant::now() + within;
        live.approve(&Handoff {
            url,
            code: Some("WMRT-8KQZ"),
            completion: crate::ai::connector::Completion::Device,
            hosts: &["accounts.x.ai"],
            email: Some("main@example.com"),
            finished,
            deadline,
        })
    };
    let held = store.signin_now().expect("lock").expect("free");
    let said = approve(&|| false, Duration::from_millis(300));
    assert!(
        matches!(&said, Approval::NeedsYou(why) if why.starts_with(super::super::accounts::SIGNIN_HELD)),
        "{said:?}"
    );
    let said = approve(&|| true, Duration::from_secs(60));
    assert_eq!(
        said,
        Approval::Approved { page: false },
        "grok ended the wait"
    );
    drop(held);
    let said = approve(&|| false, Duration::from_secs(60));
    assert!(
        matches!(&said, Approval::Refused(why) if why.contains("conflicting clients")),
        "CONTROL: {said:?}"
    );
}

/// The record never credits the automatic rung with an approval grok's end made: a rung that
/// returns because grok finished, where grok then exits without a login (an expired code), is
/// named `grok ended while it ran`. CONTROL: one whose approval signs in is `approved`
/// (`an_automatic_approval_signs_in_and_asks_nobody`).
/// The rung stands in for a vendor whose page answers the headless browser ([`Rung::ready`]), the
/// only one whose rung the gate runs: Grok Build's is skipped.
#[cfg(target_os = "macos")]
#[test]
fn a_rung_that_ended_with_grok_is_not_named_approved() {
    let bed = Bed::new("expire");
    let seen = Seen::default();
    let mut owner = Counted::new(&seen, None);
    let mut rung = Rung::ready(
        bed.root(),
        "grok-one@example.com",
        Approval::Approved { page: false },
    );
    let (ended, ledger) = gate_run_with(
        &bed,
        Some(Path::new(login_drive::SANDBOX_EXEC)),
        (true, true),
        vec![],
        &mut rung,
        &mut owner,
    );
    assert!(
        matches!(&ended, Ended::Unsigned { word, .. } if word == "exited-1"),
        "{ended:?}"
    );
    assert!(
        ledger[0].starts_with("accounts.x.ai device exited-1; ")
            && ledger[0].ends_with("headless(grok ended while it ran) person(not asked)"),
        "{ledger:?}"
    );
}

/// THE WAY OUT OF AN AUTOMATIC REFUSAL NAMES `accounts browser` AS THE COMMAND ONLY WHERE IT OPENS
/// A WINDOW HERE, as the setup does: with a window, re-signing the row's profile in is that
/// command; with none, it comes after installing a Chromium that opens a window. Both keep the
/// approval at the next launch first.
#[test]
fn the_refusals_way_out_names_accounts_browser_only_where_it_opens_a_window() {
    let window = refused_way_out("main", true);
    let none = refused_way_out("main", false);
    for text in [&window, &none] {
        assert!(
            text.starts_with(
                "nothing was placed, and the next launch from a terminal asks you to approve its \
                 code yourself (or "
            ) && text.ends_with("`aterm harness accounts browser main --cli grok`)"),
            "{text}"
        );
    }
    assert_eq!(
        window,
        "nothing was placed, and the next launch from a terminal asks you to approve its code \
         yourself (or sign main's browser profile in again as its account: `aterm harness \
         accounts browser main --cli grok`)"
    );
    assert!(!window.contains("install"), "{window}");
    assert!(
        none.contains(
            "(or install a Chromium that opens a window (Google Chrome for Testing or \
                       Chromium) and sign main's browser profile in again"
        ),
        "{none}"
    );
}

/// A row whose `[[mailboxes]]` entry has no password yet names that entry, never "none
/// configured": its words say `mailbox(ab…@… has no password yet)`, and its setup asks for that
/// password, not for a mailbox the vault already holds. NEGATIVE CONTROL: reading `mailbox`
/// alone (the branch's first form) says `none configured`, and this fails.
#[test]
fn a_mailbox_with_no_password_yet_is_named_not_called_missing() {
    let mut ladder = Ladder::bare("none installed");
    ladder.label = "main".to_owned();
    ladder.from = RowFrom::Default;
    ladder.email = Some("grok-one@example.test".to_owned());
    ladder.mailbox_unready = Some("ow…@…".to_owned());
    let words = ladder.words();
    assert!(
        words.contains("mailbox(ow…@… has no password yet)"),
        "{words}"
    );
    assert_eq!(
        ladder.setup(),
        Setup::MailboxPassword {
            label: "main".to_owned(),
            login: "ow…@…".to_owned(),
        }
    );
    let line = needs_you_line(
        Some(&ladder.setup()),
        "https://accounts.x.ai/oauth2/device?user_code=WMRT-8KQZ",
        Some("WMRT-8KQZ"),
    );
    assert!(
        line.contains("fill in the password of mailbox ow…@…"),
        "{line}"
    );
    ladder.mailbox_unready = None;
    assert!(ladder.words().contains("mailbox(none configured)"));
    assert_eq!(ladder.setup(), Setup::Mailbox("main".to_owned()));
}
