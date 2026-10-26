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
use std::sync::Mutex;
#[cfg(target_os = "macos")]
use std::sync::atomic::{AtomicUsize, Ordering};

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
}

#[cfg(target_os = "macos")]
struct Counted<'a> {
    seen: &'a Seen,
    /// Run when the need goes up (the person approving the page).
    approve: Option<PathBuf>,
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
        if line.starts_with("another session is signing Grok Build in") {
            self.seen.waiting.fetch_add(1, Ordering::SeqCst);
        }
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

/// One gate over `bed`, its sandbox `sandbox`, run to its end with `owner`; the ledger lines.
#[cfg(target_os = "macos")]
fn gate_run(
    bed: &Bed,
    sandbox: Option<&Path>,
    (wait, ack): (bool, bool),
    extra: Vec<(&'static str, OsString)>,
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
        hang: Duration::from_secs(60),
        wait,
        ack,
        stopped: &never,
        now: &clock,
    };
    let mut ledger = Vec::new();
    let ended = run(&gate, owner, &mut |line| ledger.push(line.to_owned()));
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
    let mut owner = Counted {
        seen: &seen,
        approve: None,
    };
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
    let mut owner = Counted {
        seen: &seen,
        approve: Some(bed.root().join("approve")),
    };
    let (ended, _) = gate_run(&bed, sandbox, (true, true), vec![], &mut owner);
    assert!(
        matches!(ended, Ended::SignedIn { .. }),
        "CONTROL: {ended:?}"
    );
    assert_eq!(bed.runs(), 1, "CONTROL");
}

/// A missing sandbox, or one that cannot apply here, runs nothing and says why; it never falls
/// back to an unsandboxed grok. CONTROL: with the sandbox there, the vendor ran.
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
        let mut owner = Counted {
            seen: &seen,
            approve: None,
        };
        let (ended, ledger) = gate_run(&bed, sandbox, (true, true), vec![], &mut owner);
        let Ended::NotRun(why) = &ended else {
            panic!("{sandbox:?}: {ended:?}");
        };
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
    let mut owner = Counted {
        seen: &seen,
        approve: Some(bed.root().join("approve")),
    };
    let (ended, _) = gate_run(
        &bed,
        Some(Path::new(login_drive::SANDBOX_EXEC)),
        (true, true),
        vec![],
        &mut owner,
    );
    assert!(
        matches!(ended, Ended::SignedIn { .. }),
        "CONTROL: {ended:?}"
    );
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
    let mut owner = Counted {
        seen: &seen,
        approve: Some(bed.root().join("approve")),
    };
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
    let url = "https://accounts.x.ai/oauth2/device?user_code=WMRT-8KQZ";
    assert_eq!(
        *seen.texts.lock().unwrap(),
        [format!(
            "approve Grok Build sign-in: code WMRT-8KQZ at {url}"
        )]
    );
    assert_eq!(
        *seen.lines.lock().unwrap(),
        [format!(
            "NEEDS YOU: approve Grok Build sign-in, code WMRT-8KQZ, at {url}"
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
    assert_eq!(ledger, ["accounts.x.ai device signed-in"]);
    assert!(!ledger[0].contains("WMRT"));
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
                    let mut owner = Counted {
                        seen,
                        approve: None,
                    };
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
                    let mut owner = Counted {
                        seen,
                        approve: None,
                    };
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
    let mut owner = Counted {
        seen: &seen,
        approve: None,
    };
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
    let mut owner = Counted {
        seen: &seen,
        approve: Some(bed.root().join("approve")),
    };
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

#[test]
fn the_need_names_the_code_first_and_fits_the_attention_cap() {
    let url = "https://accounts.x.ai/oauth2/device?user_code=WMRT-8KQZ";
    assert_eq!(
        need_text(url, Some("WMRT-8KQZ")),
        format!("approve Grok Build sign-in: code WMRT-8KQZ at {url}")
    );
    let long = format!(
        "https://accounts.x.ai/oauth2/device?user_code=WMRT-8KQZ&{}",
        "x".repeat(300)
    );
    let text = need_text(&long, Some("WMRT-8KQZ"));
    assert_eq!(
        text,
        "approve Grok Build sign-in: code WMRT-8KQZ at https://accounts.x.ai"
    );
    assert!(text.len() <= aterm_types::control_verbs::META_ATTENTION_KEYED_MAX);
    let record = Record {
        generation: 3,
        t: 100,
        outcome: "hung".to_owned(),
    };
    assert_eq!(Record::parse(&record.line()), record);
    assert!(record.unsigned());
    assert!(!Record::default().unsigned());
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
    let mut owner = Counted {
        seen: &seen,
        approve: Some(bed.root().join("approve")),
    };
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
            let mut owner = Counted {
                seen: &seen,
                approve: Some(bed.root().join("approve")),
            };
            run(&gate, &mut owner, &mut |_| {})
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
