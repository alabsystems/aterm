// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A sign-in's child (a renewal's `claude auth login`, a vendor CLI's sign-in) never outlives the
//! aterm process that started it: when that process is `SIGKILL`ed, exits, or execs in its own
//! place while the sign-in waits (a quit, an update's outgoing process after its handoff, or its
//! cold lane's exec; updates left three such logins running for days on the owner's Mac), the
//! child ends with it. Where that process's executable was replaced since it started, so the tie
//! cannot hold, no sign-in starts its child. The starter is a stand-in: this test binary, re-run as
//! [`STARTER`], which runs one sign-in of a stand-in program that waits for ever, a renewal
//! unattended or as a person's sign-in on a terminal (`aterm_agent::harness::cli::renewal_run`),
//! or a vendor's driven sign-in (`aterm_agent::harness::login_drive::drive`), with the front door
//! as its tether executable, as the window's sign-ins run through the window's own.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_spec::wait::MUST_HAPPEN;
use aterm_uds::process::{Birth, read_identity};
use aterm_uds::spawnfd::Stdio;

use crate::{launch_isolation, standin};

/// The stand-in starter's [`standin::ENV`] word.
const STARTER: &str = "renewal-starter";

/// The starter once it exec'd in its own place ([`Ends::Execs`]): it says so ([`EXECED`]) and
/// waits for [`QUIT`].
const AFTER_EXEC: &str = "renewal-starter-after-exec";

/// A starter whose executable to run through was replaced before it ran its sign-ins: it runs
/// each [`Flow`] once and says how each ended ([`ENDED`]).
const REPLACED: &str = "renewal-starter-replaced";

/// The variable that names the folder of the stand-in program, which also says how the starter
/// is told to end ([`QUIT`], [`EXEC`]).
const FOLDER: &str = "RENEWAL_STAND_IN_DIR";

/// The file whose appearance in [`FOLDER`] makes the starter exit by itself.
const QUIT: &str = "quit";

/// The file whose appearance in [`FOLDER`] makes the starter exec in its own place.
const EXEC: &str = "exec";

/// The file the image a starter's exec put in its place writes as it runs.
const EXECED: &str = "execed";

/// The file a [`REPLACED`] starter writes once its sign-ins ended, a line for each.
const ENDED: &str = "ended";

/// The variable that names the starter's [`Flow`] ([`Flow::word`]).
const FLOW: &str = "RENEWAL_FLOW";

/// The stand-in program: it says its pid beside itself, then waits for ever in `sleep`'s image
/// (ten minutes, a bound on a leftover), which ends at `SIGTERM`.
const PROVIDER: &str = "#!/bin/sh\necho $$ > \"${0%/*}/pid\"\nexec /bin/sleep 600\n";

/// How long the starter's sign-in may wait for its child: past every wait here, so only the
/// starter's own end can end the child.
const SIGN_IN_BOUND: Duration = Duration::from_secs(600);

/// The renewal's staging home under [`FOLDER`], laid out as the harness store lays one out
/// (`<state>/accounts/logins/<label>+<n>`), so whatever the renewal reads of its store is in
/// the folder.
const STAGING: &str = "state/accounts/logins/stand-in+1";

/// Play the stand-in asked for, or return to the test.
fn play_if_asked() {
    let Some(spec) = standin::spec() else { return };
    let folder = PathBuf::from(std::env::var_os(FOLDER).expect("the stand-in's folder"));
    match spec.as_str() {
        STARTER => play_starter(&folder),
        AFTER_EXEC => play_after_exec(&folder),
        REPLACED => play_replaced(&folder),
        _ => {
            standin::put(format!("unknown stand-in: {spec}\n").as_bytes());
            standin::exit(2)
        }
    }
}

/// The starter: one sign-in of [`FLOW`]'s flow on a thread of its own, while it waits to be told
/// to exit ([`QUIT`]) or to exec in its own place ([`EXEC`]), as the update's cold lane does.
fn play_starter(folder: &Path) -> ! {
    aterm_uds::tether::through(launch_isolation::aterm_executable());
    let word = std::env::var(FLOW).unwrap_or_default();
    let Some(flow) = Flow::of(&word) else {
        standin::put(format!("unknown flow: {word:?}\n").as_bytes());
        standin::exit(2)
    };
    let at = folder.to_path_buf();
    std::thread::spawn(move || {
        let ended = flow.run(&at);
        standin::put(format!("the sign-in ended: {ended}\n").as_bytes());
    });
    loop {
        if folder.join(QUIT).exists() {
            standin::exit(0)
        }
        if folder.join(EXEC).exists() {
            let error = aterm_uds::spawnfd::command(standin::exe())
                .args(std::env::args_os().skip(1))
                .env(standin::ENV, AFTER_EXEC)
                .exec();
            standin::put(format!("the exec in the starter's place: {error}\n").as_bytes());
            standin::exit(3)
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The image the starter's exec put in its place: it says it runs, then waits for [`QUIT`].
fn play_after_exec(folder: &Path) -> ! {
    if std::fs::write(folder.join(EXECED), b"").is_err() {
        standin::exit(3)
    }
    while !folder.join(QUIT).exists() {
        std::thread::sleep(Duration::from_millis(10));
    }
    standin::exit(0)
}

/// A starter whose recorded executable another file replaced, as an update's swap of the bundle
/// does under a running window: each flow's sign-in, run once, and a line saying how it ended.
fn play_replaced(folder: &Path) -> ! {
    let image = folder.join("image");
    let next = folder.join("image.next");
    let replaced = std::fs::write(&image, b"the build this starter started from")
        .and_then(|()| {
            aterm_uds::tether::through(image.clone());
            std::fs::write(&next, b"an update's build")
        })
        .and_then(|()| std::fs::rename(&next, &image));
    if let Err(e) = replaced {
        standin::put(format!("replace the recorded image: {e}\n").as_bytes());
        standin::exit(3)
    }
    let said: String = Flow::ALL
        .iter()
        .map(|flow| format!("{flow:?}: {}\n", flow.run(folder)))
        .collect();
    let written = std::fs::write(folder.join("ended.next"), said)
        .and_then(|()| std::fs::rename(folder.join("ended.next"), folder.join(ENDED)));
    standin::exit(i32::from(written.is_err()) * 3)
}

/// Which sign-in the starter runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Flow {
    /// A renewal with no terminal, as the window's upkeep and an agent's `accounts renew` run.
    Unattended,
    /// A person's renewal on their terminal.
    Person,
    /// A vendor CLI's sign-in driven with no terminal (`accounts renew --cli <id>`).
    Vendor,
}

impl Flow {
    /// Every flow.
    const ALL: [Self; 3] = [Self::Unattended, Self::Person, Self::Vendor];

    /// Its word in [`FLOW`].
    const fn word(self) -> &'static str {
        match self {
            Self::Unattended => "unattended",
            Self::Person => "person",
            Self::Vendor => "vendor",
        }
    }

    /// The flow `word` names.
    fn of(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|flow| flow.word() == word)
    }

    /// One sign-in of the stand-in program in `folder`, and how it ended.
    fn run(self, folder: &Path) -> String {
        let (program, home) = (folder.join("claude"), folder.join(STAGING));
        match self {
            Self::Unattended => format!(
                "{:?}",
                aterm_agent::harness::cli::renewal_run(&program, &home, Some(SIGN_IN_BOUND))
            ),
            Self::Person => format!(
                "{:?}",
                aterm_agent::harness::cli::renewal_run(&program, &home, None)
            ),
            Self::Vendor => vendor_sign_in(&program, folder),
        }
    }
}

/// A vendor CLI's driven sign-in of `program`, a stand-in that never names a page, bounded by
/// [`SIGN_IN_BOUND`]: how it ended.
fn vendor_sign_in(program: &Path, cwd: &Path) -> String {
    use aterm_agent::ai::connector::{Completion, Grammar};
    use aterm_agent::harness::login_drive::{Approval, Approver, Handoff, Spec, drive};

    /// An approver that approves nothing; the stand-in never names a page to it.
    struct Refuses;

    impl Approver for Refuses {
        fn approve(&mut self, _handoff: &Handoff<'_>) -> Approval {
            Approval::Refused(String::from("a stand-in approves nothing"))
        }
    }

    let grammar = Grammar {
        url: &[],
        code: &[],
        done: &[],
        signed_in_as: None,
    };
    let env = [(
        String::from("PATH"),
        std::ffi::OsString::from("/usr/bin:/bin"),
    )];
    let drove = drive(
        &Spec {
            program,
            lead: &[],
            args: &[],
            env: &env,
            cwd,
            grammar: &grammar,
            completion: Completion::Device,
            hosts: &[],
            email: None,
            hang: SIGN_IN_BOUND,
        },
        &mut Refuses,
    );
    format!("{:?}", drove.outcome)
}

/// How the starter ends while its sign-in waits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ends {
    /// `SIGKILL`.
    Killed,
    /// It exits by itself, as a quit or an update's outgoing process does.
    Exits,
    /// It execs in its own place, as an update's cold lane does, and runs on as the image the
    /// exec put there, which knows nothing of the sign-in.
    Execs,
}

/// The stand-in starter, `SIGKILL`ed and reaped on drop while it is unreaped, so a failing run
/// leaves it behind no longer than the test.
struct Starter(aterm_uds::spawnfd::Child);

impl Drop for Starter {
    fn drop(&mut self) {
        if !self.0.is_reaped() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

/// The stand-in program as the test proved it, `SIGKILL`ed on drop while its pid still reads its
/// birth, so a failing run leaves it behind no longer than the test.
struct Provider {
    pid: i32,
    birth: Birth,
}

impl Provider {
    /// Whether its pid no longer reads its birth: it ended, or the pid names a later process.
    fn gone(&self) -> bool {
        read_identity(self.pid).is_none_or(|now| now.birth != self.birth)
    }

    /// The stand-in in `folder`, once it said its pid and runs `sleep` with its thread run (the
    /// tether's watcher waits on a process whose thread has not run).
    fn running_sleep(folder: &Path) -> Option<Self> {
        let pid = std::fs::read_to_string(folder.join("pid"))
            .ok()
            .filter(|text| text.ends_with('\n'))?
            .trim()
            .parse::<i32>()
            .ok()?;
        runs_sleep_with_its_thread_run(pid).then_some(())?;
        read_identity(pid).map(|now| Self {
            pid,
            birth: now.birth,
        })
    }
}

impl Drop for Provider {
    fn drop(&mut self) {
        if !self.gone() {
            // SAFETY: the stand-in program this test started, its birth read again just now.
            unsafe { libc::kill(self.pid, libc::SIGKILL) };
        }
    }
}

/// A scratch folder holding the stand-in program as `claude` and the renewal's staging home.
fn stand_in_folder() -> aterm_tempfile::TempDir {
    let folder = aterm_tempfile::Builder::new()
        .prefix("aterm-renewal-child-")
        .suffix(".noindex")
        .tempdir()
        .expect("scratch");
    aterm_tempfile::stand_in(&folder.path().join("claude"), PROVIDER).expect("the stand-in claude");
    std::fs::create_dir_all(folder.path().join(STAGING)).expect("its staging home");
    folder
}

/// The stand-in starter `word` as the test `entry`, its sign-ins on the stand-in in `folder`,
/// `flow` its flow where it runs one.
fn start(entry: &str, word: &str, folder: &Path, flow: Flow) -> Starter {
    let aterm = launch_isolation::aterm_executable();
    aterm_spec::wait::first_exec(&aterm, &["--help"]);
    let mut command = aterm_uds::spawnfd::command(standin::exe());
    command
        .args(standin::argv(entry))
        .env(standin::ENV, word)
        .env(FOLDER, folder)
        .env(FLOW, flow.word())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .process_group(0);
    Starter(command.spawn().expect("start the stand-in starter"))
}

/// Start the starter as the test `entry`, running `flow`, wait until its sign-in's child runs
/// `sleep` with its thread run, end the starter as `ends` says, and see the child gone, by pid
/// and birth, within the hang detector: for [`Ends::Execs`] while the image the exec put in the
/// starter's place still runs.
fn the_child_ends_with_its_starter(entry: &str, flow: Flow, ends: Ends) {
    let folder = stand_in_folder();
    let mut starter = start(entry, STARTER, folder.path(), flow);
    let provider = aterm_spec::wait::until_within(
        aterm_spec::wait::FIRST_EXEC,
        "the sign-in's child says its pid and runs sleep, its thread run",
        || Provider::running_sleep(folder.path()),
    );
    match ends {
        Ends::Killed => starter.0.kill().expect("SIGKILL the stand-in starter"),
        Ends::Exits => std::fs::write(folder.path().join(QUIT), b"").expect("tell it to exit"),
        Ends::Execs => {
            std::fs::write(folder.path().join(EXEC), b"").expect("tell it to exec");
            aterm_spec::wait::until_within(
                MUST_HAPPEN,
                "the image the starter's exec put in its place runs",
                || folder.path().join(EXECED).exists().then_some(()),
            );
        }
    }
    let status =
        (ends != Ends::Execs).then(|| starter.0.wait().expect("reap the stand-in starter"));
    let ended = Instant::now();
    let deadline = ended + MUST_HAPPEN;
    while !provider.gone() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        provider.gone(),
        "the sign-in's child (pid {}) outlived its starter ({flow:?}, {ends:?}, {status:?}) by \
         {MUST_HAPPEN:?}",
        provider.pid
    );
    eprintln!(
        "the sign-in's child (pid {}) ended {:?} after its starter ({flow:?}, {ends:?})",
        provider.pid,
        ended.elapsed()
    );
    if ends == Ends::Execs {
        assert!(
            starter.0.try_wait().is_ok_and(|exited| exited.is_none()),
            "the image the starter's exec put in its place runs on"
        );
        std::fs::write(folder.path().join(QUIT), b"").expect("tell it to exit");
        let status = starter.0.wait().expect("reap the stand-in starter");
        assert!(status.success(), "the image after the exec: {status:?}");
    }
}

/// Whether `pid` runs `/bin/sleep`, the stand-in program's last image, with its thread run, as
/// the tether's watcher reads it before it signals.
fn runs_sleep_with_its_thread_run(pid: i32) -> bool {
    use std::os::unix::fs::MetadataExt as _;

    let file = |path: &Path| std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()));
    let image = aterm_uds::process::read_process_path(pid).and_then(|path| file(&path));
    image.is_some()
        && image == file(Path::new("/bin/sleep"))
        && aterm_uds::process::thread_ran(pid) == Some(true)
}

/// The starter is `SIGKILL`ed while its unattended renewal waits for a sign-in: the provider ends
/// too.
#[test]
fn a_renewals_provider_ends_with_its_sigkilled_starter() {
    play_if_asked();
    the_child_ends_with_its_starter(
        "renewal_child_live::a_renewals_provider_ends_with_its_sigkilled_starter",
        Flow::Unattended,
        Ends::Killed,
    );
}

/// The starter exits by itself while its unattended renewal waits, as a quit does, and as an
/// update's outgoing process does once its successor took over: the provider ends too.
#[test]
fn a_renewals_provider_ends_when_its_starter_exits_mid_wait() {
    play_if_asked();
    the_child_ends_with_its_starter(
        "renewal_child_live::a_renewals_provider_ends_when_its_starter_exits_mid_wait",
        Flow::Unattended,
        Ends::Exits,
    );
}

/// The starter execs in its own place while its unattended renewal waits, as an update's cold
/// lane does in a window with no terminal open: the provider ends, while the image the exec put
/// there, which knows nothing of the renewal, runs on.
#[test]
fn a_renewals_provider_ends_when_its_starter_execs_in_its_place() {
    play_if_asked();
    the_child_ends_with_its_starter(
        "renewal_child_live::a_renewals_provider_ends_when_its_starter_execs_in_its_place",
        Flow::Unattended,
        Ends::Execs,
    );
}

/// A person's `accounts renew` is `SIGKILL`ed while their sign-in waits on the terminal for a
/// code: the provider ends too, and leaves no prompt behind.
#[test]
fn a_persons_renewal_provider_ends_with_its_sigkilled_starter() {
    play_if_asked();
    the_child_ends_with_its_starter(
        "renewal_child_live::a_persons_renewal_provider_ends_with_its_sigkilled_starter",
        Flow::Person,
        Ends::Killed,
    );
}

/// The starter is `SIGKILL`ed while a vendor CLI's driven sign-in waits for its page: the vendor
/// CLI ends too.
#[test]
fn a_vendor_sign_ins_child_ends_with_its_sigkilled_starter() {
    play_if_asked();
    the_child_ends_with_its_starter(
        "renewal_child_live::a_vendor_sign_ins_child_ends_with_its_sigkilled_starter",
        Flow::Vendor,
        Ends::Killed,
    );
}

/// A starter whose recorded executable another file replaced (an update's swap of the bundle
/// under a running window) starts no sign-in's child, in any flow, since none could be tied to
/// it: each sign-in says the executable was replaced, and the stand-in never says its pid.
#[cfg(target_os = "macos")]
#[test]
fn no_sign_in_starts_its_child_once_its_starters_image_was_replaced() {
    play_if_asked();
    let folder = stand_in_folder();
    let mut starter = start(
        "renewal_child_live::no_sign_in_starts_its_child_once_its_starters_image_was_replaced",
        REPLACED,
        folder.path(),
        Flow::Unattended,
    );
    let ended = aterm_spec::wait::until_within(
        aterm_spec::wait::FIRST_EXEC,
        "the starter's sign-ins end, or one's child says its pid",
        || {
            if let Some(provider) = Provider::running_sleep(folder.path()) {
                return Some(Err(provider));
            }
            std::fs::read_to_string(folder.path().join(ENDED))
                .ok()
                .map(Ok)
        },
    );
    let said = match ended {
        Ok(said) => said,
        Err(provider) => panic!(
            "a sign-in's child (pid {}) started, untied, once its starter's image was replaced",
            provider.pid
        ),
    };
    let status = starter.0.wait().expect("reap the stand-in starter");
    assert!(
        status.success(),
        "the starter: {status:?}, it said {said:?}"
    );
    for flow in Flow::ALL {
        let line = said
            .lines()
            .find(|line| line.starts_with(&format!("{flow:?}: ")))
            .unwrap_or_else(|| panic!("{flow:?}: no line in {said:?}"));
        assert!(
            line.contains("Err(") || line.contains("NotRun("),
            "{flow:?}'s sign-in did not refuse: {line}"
        );
        assert!(
            line.contains("executable was replaced"),
            "{flow:?}'s sign-in says why it started nothing: {line}"
        );
    }
    assert!(
        !folder.path().join("pid").exists(),
        "no sign-in's child said its pid"
    );
}
