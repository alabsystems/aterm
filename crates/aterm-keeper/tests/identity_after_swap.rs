// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A keeper outlives the build it started from. An update moves the old bundle to
//! `aterm.app.rollback` and the successor's boot-health confirmation deletes it, while macOS
//! checks a running process's code through the file it started from: with that file deleted
//! the lookup fails `ENOENT` (OSStatus 100002), and with another build at the path it fails
//! too. So the keeper runs from its own copy of its code, and a window's designated-requirement
//! check (§5.7) still admits it.
//!
//! `aterm keeper serve --identity designated` runs from a copy of this test binary (bare, and in
//! an ad-hoc signed bundle); that build is deleted, then replaced at its path, and a dial as the
//! original build must still admit the keeper. Negative controls: other code (`nc`, a bundle
//! sealing another Info.plist) is refused. The last test checks that `aterm keeper stop` leaves
//! no copy behind.
//!
//! No WindowServer: every process is this binary (run by `exec`, never LaunchServices), `nc`,
//! `codesign`, `ditto` or `launchctl`; stand-ins end by SIGKILL. Each test ends COULD NOT RUN
//! if its initial `aterm_uds::deadline::trust_service_silent` probe finds no answer. A later unavailable
//! identity check fails with `COULD NOT RUN`; it establishes no code-identity verdict.
//! The negative controls check Apple-signed code.

#![cfg(target_vendor = "apple")]

use std::ffi::OsString;
use std::io::Write as _;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
// `spawnfd`'s: each child (the keepers, `nc`, the verbs) inherits only its stdio, never a pipe
// another test thread had open unflagged at the spawn.
use aterm_uds::spawnfd::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use aterm_keeper::client::{KeeperClient, refused_identity};
use aterm_keeper::identity::{Identity, IdentityPolicy};
use aterm_keeper::job::{self, JobState};
use aterm_spec::wait::{MUST_HAPPEN, within};
use aterm_uds::deadline::{output_until, trust_service_silent};

/// What a child run of this binary does instead of its test.
const ROLE: &str = "ATERM_KEEPER_SWAP_ROLE";
/// The keeper role's socket.
const SOCK: &str = "ATERM_KEEPER_SWAP_SOCK";
/// The verb role's arguments, one per line.
const ARGS: &str = "ATERM_KEEPER_SWAP_ARGS";

const BARE: &str = "a_keeper_whose_build_is_deleted_is_still_this_builds";
const BUNDLED: &str = "a_keeper_whose_app_is_deleted_is_still_that_apps";
const STOPPED: &str = "a_stopped_keeper_leaves_no_copy_of_its_code";

/// Play the role a parent test gave this run, if any; never returns then.
///
/// `keeper`: `aterm keeper serve` on the socket the test names, with the
/// designated identity — exactly what `aterm keeper start` submits.
/// `verb`: `aterm keeper <ARGS>`, its words on stdout, exit 0 or 1.
fn play_role_if_asked() {
    match std::env::var(ROLE).as_deref() {
        Ok("keeper") => {
            let sock = std::env::var(SOCK).expect("the test names the socket");
            let code = aterm_keeper::cli::main_entry(
                [
                    "serve",
                    "--sock",
                    &sock,
                    "--identity",
                    "designated",
                    "--no-relaunch",
                ]
                .into_iter()
                .map(Into::into)
                .collect(),
                "test",
            );
            eprintln!("keeper role: serve returned {code:?}");
            aterm_uds::process::kill_self_now()
        }
        Ok("verb") => {
            let args = std::env::var(ARGS).expect("the test names the verb");
            let code =
                aterm_keeper::cli::main_entry(args.lines().map(OsString::from).collect(), "test");
            let _ = std::io::stdout().flush();
            std::process::exit(i32::from(code != ExitCode::SUCCESS))
        }
        _ => {}
    }
}

struct Scratch {
    dir: PathBuf,
    kids: Vec<Child>,
    /// A launchd job to remove, whatever happens.
    job: Option<String>,
}

impl Scratch {
    fn new(prefix: &str) -> Self {
        let dir = std::env::temp_dir()
            .canonicalize()
            .expect("temp dir")
            .join(format!("{prefix}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        private_dir(&dir.join("run"));
        private_dir(&dir.join("home"));
        Self {
            dir,
            kids: Vec::new(),
            job: None,
        }
    }

    fn sock(&self) -> PathBuf {
        self.dir.join("run").join("k.sock")
    }

    fn home(&self) -> PathBuf {
        self.dir.join("home")
    }

    /// Where a keeper given [`Self::home`] keeps the copies it runs from.
    fn copies(&self) -> PathBuf {
        self.home()
            .join("Library/Application Support/aterm/keeper-code")
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.dir.join("keeper.log")).unwrap_or_default()
    }

    /// Run the keeper role from `exe` (a run of `test`), its stderr in `log`.
    fn start_keeper(&mut self, exe: &Path, test: &str, log: &str) {
        let log = std::fs::File::create(self.dir.join(log)).expect("log");
        let kid = Command::new(exe)
            .args(["--exact", test, "--nocapture", "--test-threads", "1"])
            .env(ROLE, "keeper")
            .env(SOCK, self.sock())
            // Where a keeper keeps its own things is per user: this test's.
            .env("HOME", self.home())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .expect("the keeper");
        self.kids.push(kid);
    }

    /// `aterm keeper <args>` run by `exe` (a run of `test`): its first line
    /// that starts `keeper=` or `job=` (the test harness may print its own
    /// words before it, on the same line), or everything it said.
    fn verb(&self, exe: &Path, test: &str, args: &[&str]) -> String {
        let out = Command::new(exe)
            .args(["--exact", test, "--nocapture", "--test-threads", "1"])
            .env(ROLE, "verb")
            .env(ARGS, args.join("\n"))
            .env("HOME", self.home())
            .stdin(Stdio::null())
            .output()
            .expect("the verb");
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.lines()
            .find_map(|l| {
                l.find("keeper=")
                    .or_else(|| l.find("job="))
                    .map(|at| &l[at..])
            })
            .map_or_else(
                || format!("{text}{}", String::from_utf8_lossy(&out.stderr)),
                str::to_string,
            )
    }

    /// The copy the keeper `pid` runs from, checked to be under
    /// [`Self::copies`] in its own `<pid>-<start>` directory; the path of its
    /// executable inside that directory.
    fn runs_from_its_copy(&self, pid: u32) -> PathBuf {
        let running = aterm_uds::process::read_process_path(pid as i32).expect("its executable");
        let copies = self.copies();
        let inside = running.strip_prefix(&copies).unwrap_or_else(|_| {
            panic!(
                "the keeper runs {} — not a copy under {}",
                running.display(),
                copies.display()
            )
        });
        let mut parts = inside.components();
        let own = parts.next().expect("its copy's directory");
        assert!(
            own.as_os_str()
                .to_string_lossy()
                .starts_with(&format!("{pid}-")),
            "the keeper {pid} runs another keeper's copy: {}",
            running.display()
        );
        parts.as_path().to_path_buf()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Some(label) = &self.job {
            let _ = job::remove(label);
        }
        for kid in &mut self.kids {
            let _ = kid.kill();
            let _ = kid.wait();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn private_dir(path: &Path) {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .expect("a private directory");
}

/// `trustd` going silent after the one probe refuses every later code check
/// through a process's gate: the machine's moment, never the code's verdict.
const TRUSTD_SILENT: &str = "the process trustd gate was silent — the machine's, not the code's";

/// Ends a test that checks code COULD NOT RUN while `trustd` does not answer the
/// probe (the crate's `require_trust_service`, which this crate cannot reach).
fn require_trust_service() {
    if trust_service_silent() {
        aterm_spec::host::could_not_run(
            "macOS's trustd does not answer, so no code check runs here — the machine's, not the code's",
        );
    }
}

/// Whether `why` is an identity check's words for a gate that refused it
/// unanswered.
fn trustd_silent(why: &str) -> bool {
    why.starts_with("macOS's code-signing service (trustd) is not answering")
}

impl Scratch {
    /// Dial the keeper as this build's window does (its own designated identity). A check
    /// `trustd` did not answer, ours of the keeper or the keeper's of this dial (its log says
    /// so), panics as COULD NOT RUN, so a probe retries only a keeper not yet listening.
    fn dial_as_this_build(&self) -> std::io::Result<String> {
        let own = Identity::for_self(IdentityPolicy::Designated).expect("own requirement");
        let got =
            KeeperClient::connect(&self.sock(), &own, MUST_HAPPEN).and_then(|c| c.status("test"));
        if let Err(e) = &got {
            if trustd_silent(&e.to_string()) {
                aterm_spec::host::could_not_run(format_args!("{TRUSTD_SILENT}: {e}"));
            }
            let log = self.log();
            if log.contains(
                "identity could not be verified: macOS's code-signing service (trustd) is not \
                 answering",
            ) {
                aterm_spec::host::could_not_run(format_args!(
                    "{TRUSTD_SILENT}: the keeper's check of this dial: {e}: {log}"
                ));
            }
        }
        got
    }
}

/// A byte-for-byte copy of this test binary at `to`: the very same code (its
/// cdhash) as the test that checks it.
fn copy_of_this_binary(to: &Path) {
    private_dir(to.parent().expect("a directory"));
    std::fs::copy(std::env::current_exe().expect("test binary"), to).expect("copy");
}

/// An app bundle at `app` around a copy of this test binary, its Info.plist
/// naming `id`, signed ad hoc (`codesign` seals the Info.plist and the
/// resources into the executable's signature, so two bundles with different
/// ids are different code). Returns its executable.
fn signed_bundle(app: &Path, id: &str) -> PathBuf {
    let contents = app.join("Contents");
    let exe = contents.join("MacOS").join("aterm");
    copy_of_this_binary(&exe);
    private_dir(&contents.join("Resources"));
    std::fs::write(contents.join("Resources").join("r.txt"), id).expect("a resource");
    std::fs::write(
        contents.join("Info.plist"),
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\
             <key>CFBundleExecutable</key><string>aterm</string>\
             <key>CFBundleIdentifier</key><string>{id}</string>\
             <key>CFBundlePackageType</key><string>APPL</string>\
             </dict></plist>\n"
        ),
    )
    .expect("Info.plist");
    // A hang detector: a codesign macOS stops answering fails this test, never hangs the suite.
    let sign = output_until(
        aterm_uds::spawnfd::command("/usr/bin/codesign")
            .args(["--force", "--sign", "-"])
            .arg(app),
        Instant::now() + MUST_HAPPEN,
    )
    .expect("codesign");
    assert!(sign.status.success(), "{sign:?}");
    exe
}

/// Copy the bundle `from` to `to` as an installer does (`ditto`).
fn ditto(from: &Path, to: &Path) {
    private_dir(to.parent().expect("a directory"));
    let out = Command::new("/usr/bin/ditto")
        .arg(from)
        .arg(to)
        .output()
        .expect("ditto");
    assert!(out.status.success(), "{out:?}");
}

#[test]
fn a_keeper_whose_build_is_deleted_is_still_this_builds() {
    play_role_if_asked();
    require_trust_service();
    let mut s = Scratch::new("akswap");
    // The build the keeper starts from: this binary, so this test dials as it.
    let build = s.dir.join("build").join("aterm");
    copy_of_this_binary(&build);
    s.start_keeper(&build, BARE, "keeper.log");
    let before = within(MUST_HAPPEN, Duration::from_millis(10), || {
        s.dial_as_this_build().ok()
    })
    .unwrap_or_else(|| panic!("the keeper never answered this build: {}", s.log()));
    assert!(before.starts_with("keeper=running"), "{before}");
    // It answers from its own copy of its code: the same pid (`exec` keeps the one launchd
    // watches), running a file under the given HOME's keeper code directory.
    let keeper = s.kids[0].id();
    let inner = s.runs_from_its_copy(keeper);
    assert_eq!(inner, Path::new("aterm"), "a bare build is copied alone");

    // A second keeper on the same socket cannot serve (one already answers):
    // it copies itself too, then removes its copy as `serve` returns, and
    // leaves the live keeper's alone.
    s.start_keeper(&build, BARE, "second.log");
    within(MUST_HAPPEN, Duration::from_millis(10), || {
        s.kids[1].try_wait().ok().flatten()
    })
    .expect("the second keeper never ended");
    let second = std::fs::read_to_string(s.dir.join("second.log")).unwrap_or_default();
    assert!(second.contains("a keeper already answers"), "{second}");
    let left: Vec<_> = std::fs::read_dir(s.copies())
        .expect("the copies' directory")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        left.len() == 1 && left[0].starts_with(&format!("{keeper}-")),
        "a keeper that could not serve left its copy (the live keeper is {keeper}): {left:?}"
    );

    // The update's boot-health confirmation deletes the build it ran from…
    std::fs::remove_dir_all(s.dir.join("build")).expect("delete the build");
    let after = s.dial_as_this_build();
    assert!(
        after
            .as_ref()
            .is_ok_and(|t| t.starts_with("keeper=running")),
        "with its build deleted, the keeper is refused: {after:?}"
    );

    // …and the next update puts a different build at the same path.
    private_dir(&s.dir.join("build"));
    std::fs::copy("/usr/bin/true", &build).expect("another program at the path");
    let reused = s.dial_as_this_build();
    assert!(
        reused
            .as_ref()
            .is_ok_and(|t| t.starts_with("keeper=running")),
        "with another build at its path, the keeper is refused: {reused:?}"
    );

    // Negative control: a "keeper" that runs other code is refused.
    let fake = s.dir.join("run").join("f.sock");
    s.kids.push(
        Command::new("/usr/bin/nc")
            .arg("-lU")
            .arg(&fake)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("nc"),
    );
    within(MUST_HAPPEN, Duration::from_millis(10), || {
        fake.exists().then_some(())
    })
    .expect("nc never listened");
    let own = Identity::for_self(IdentityPolicy::Designated).expect("own requirement");
    let refused = KeeperClient::connect(&fake, &own, MUST_HAPPEN).expect_err("nc admitted");
    if trustd_silent(&refused.to_string()) {
        aterm_spec::host::could_not_run(format_args!("{TRUSTD_SILENT}: {refused}"));
    }
    assert!(
        refused_identity(&refused),
        "nc not refused as other code: {refused}"
    );
}

/// The installed shape: the keeper runs from an app bundle (`A`, as `/Applications/aterm.app`),
/// so it copies the whole bundle (the signature seals its Info.plist and resources) and checks
/// the copy before running from it. The dialing window is the same app at another path (`B`,
/// the same bytes), as `aterm keeper status --identity designated` runs.
#[test]
fn a_keeper_whose_app_is_deleted_is_still_that_apps() {
    play_role_if_asked();
    require_trust_service();
    let mut s = Scratch::new("akswapb");
    let sock = s.sock();
    let sock_arg = sock.to_string_lossy().into_owned();
    let status = ["status", "--sock", &sock_arg, "--identity", "designated"];
    let staged = s.dir.join("staged").join("aterm.app");
    signed_bundle(&staged, "com.aterm.keeper.swaptest");
    let (a, b) = (
        s.dir.join("a").join("aterm.app"),
        s.dir.join("b").join("aterm.app"),
    );
    ditto(&staged, &a);
    ditto(&staged, &b);
    let in_bundle = Path::new("Contents/MacOS/aterm");
    let (a_exe, b_exe) = (a.join(in_bundle), b.join(in_bundle));
    // Other code: the same executable sealed with another Info.plist.
    let other = s.dir.join("other").join("aterm.app");
    let other_exe = signed_bundle(&other, "com.aterm.keeper.swaptest.other");

    s.start_keeper(&a_exe, BUNDLED, "keeper.log");
    let before = within(MUST_HAPPEN, Duration::from_millis(100), || {
        let said = s.verb(&b_exe, BUNDLED, &status);
        said.starts_with("keeper=running").then_some(said)
    })
    .unwrap_or_else(|| {
        panic!(
            "the keeper never answered its app: {} / {}",
            s.verb(&b_exe, BUNDLED, &status),
            s.log()
        )
    });
    assert!(before.starts_with("keeper=running"), "{before}");
    let inner = s.runs_from_its_copy(s.kids[0].id());
    assert_eq!(
        inner,
        Path::new("aterm.app").join(in_bundle),
        "the keeper does not run from a copy of its whole bundle"
    );

    // The update's boot-health confirmation deletes the bundle it ran from…
    std::fs::remove_dir_all(&a).expect("delete the app");
    let after = s.verb(&b_exe, BUNDLED, &status);
    assert!(
        after.starts_with("keeper=running"),
        "with its app deleted, the keeper is refused: {after} / {}",
        s.log()
    );

    // …and the next update puts another app at the same path.
    ditto(&other, &a);
    let reused = s.verb(&b_exe, BUNDLED, &status);
    assert!(
        reused.starts_with("keeper=running"),
        "with another app at its path, the keeper is refused: {reused}"
    );

    // Negative control: dialed from the other app, which is not the keeper's code, the keeper
    // is refused.
    let refused = s.verb(&other_exe, BUNDLED, &status);
    assert!(
        refused.starts_with("keeper=refused"),
        "a bundle sealing another Info.plist admitted the keeper: {refused}"
    );
}

/// `aterm keeper stop` (launchd's SIGTERM, which the keeper does not catch) waits for the keeper
/// to end, then removes the copies of every keeper that has ended. The keeper is submitted as
/// `aterm keeper start` submits it, under a test label.
#[test]
fn a_stopped_keeper_leaves_no_copy_of_its_code() {
    play_role_if_asked();
    require_trust_service();
    let mut s = Scratch::new("akstop");
    let label = format!("{}.{}.stop", job::TEST_LABEL_PREFIX, std::process::id());
    assert_eq!(job::state(&label).expect("launchctl"), JobState::Absent);
    let build = s.dir.join("build").join("aterm");
    copy_of_this_binary(&build);
    let env = |k: &str, v: &Path| format!("{k}={}", v.display());
    let (home, role, sock) = (
        env("HOME", &s.home()),
        format!("{ROLE}=keeper"),
        env(SOCK, &s.sock()),
    );
    let build_arg = build.to_string_lossy().into_owned();
    s.job = Some(label.clone());
    // launchd gives a job no environment of the caller's: `env` sets it, then
    // becomes the keeper (the same pid, as the job's program would be).
    job::submit(
        &label,
        "/usr/bin/env",
        &[
            &home,
            &role,
            &sock,
            &build_arg,
            "--exact",
            STOPPED,
            "--nocapture",
            "--test-threads",
            "1",
        ],
        &s.dir.join("keeper.log"),
    )
    .expect("submit");
    within(MUST_HAPPEN, Duration::from_millis(10), || {
        s.dial_as_this_build()
            .ok()
            .filter(|t| t.starts_with("keeper=running"))
    })
    .unwrap_or_else(|| panic!("the keeper never answered: {}", s.log()));
    let JobState::Loaded { pid: Some(pid) } = job::state(&label).expect("launchctl") else {
        panic!("the job runs nothing");
    };
    s.runs_from_its_copy(pid);

    let stopped = s.verb(&build, STOPPED, &["stop", "--label", &label]);
    assert_eq!(stopped, format!("keeper=stopped label={label}"));
    let left: Vec<_> = std::fs::read_dir(s.copies())
        .expect("the copies' directory")
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert!(
        left.is_empty(),
        "`aterm keeper stop` left the keeper's code behind: {left:?}"
    );
}
