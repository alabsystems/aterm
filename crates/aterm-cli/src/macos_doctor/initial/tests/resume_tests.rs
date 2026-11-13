// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The login job that resumes an admitted, unfinished run with no window and no person (H7):
//! it is there exactly while the run owes a pass, whatever ends the process that admitted it.

use super::*;
use aterm_secrets::service::Launchctl;
use std::time::Duration;

/// A scratch account laid out as the person's: its doctor records where aterm keeps them, under
/// the home whose `Library/LaunchAgents` launchd loads at login.
fn account() -> (aterm_tempfile::TempDir, PathBuf, PathBuf) {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let root = home
        .join("Library")
        .join("Application Support")
        .join("aterm")
        .join("doctor");
    std::fs::create_dir_all(&root).unwrap();
    (scratch, home, root)
}

fn job(home: &Path) -> PathBuf {
    home.join("Library")
        .join("LaunchAgents")
        .join("systems.alab.aterm-setup.plist")
}

/// An admitted run that a pass leaves unfinished keeps its login job, which runs aterm's resume
/// entry at each login and again after any end but its own exit 0; the pass that finishes the run
/// removes it. Before, no admission wrote any setup job: after a logout, a restart or a SIGKILL
/// nothing resumed the run until a person opened a window.
#[test]
fn an_admitted_run_keeps_its_login_job_exactly_until_it_finishes() {
    let (_scratch, home, root) = account();
    let program = home.join("Applications/aterm.app/Contents/MacOS/aterm");
    resume::serve_from(&home, &program);
    let revision = authorize_at(&root, &selected(&[Setting::Widgets]))
        .unwrap()
        .unwrap();
    assert!(!finished_at(&root).unwrap());
    let text =
        std::fs::read_to_string(job(&home)).expect("an admitted, unfinished run has its login job");
    let argv = aterm_secrets::service::plist_argv(&text).unwrap();
    assert_eq!(
        argv,
        [
            program.to_string_lossy().into_owned(),
            "--version".into(),
            resume::VERB.into()
        ]
    );
    assert!(text.contains("<key>RunAtLoad</key>\n\t<true/>"), "{text}");
    assert!(
        text.contains(
            "<key>KeepAlive</key>\n\t<dict>\n\t\t<key>SuccessfulExit</key>\n\t\t<false/>"
        ),
        "{text}"
    );

    let (_, next) = execute_initial_for_revision(
        &mut Fake::default(),
        &root,
        revision,
        &mut None,
        SetupHooks::NONE,
    )
    .unwrap()
    .unwrap();
    assert_eq!(next, SetupNext::Done);
    assert!(finished_at(&root).unwrap());
    assert!(!job(&home).exists(), "a finished run has no login job");
}

/// The login job's arguments are answered with exit 0 by an aterm without its entry (a
/// rolled-back predecessor, an older build put back, both of which keep the definition): such a
/// front door meets `--version` first and answers it before it reads another word, so launchd,
/// which starts the job again after any end but exit 0, does not start it again. Before, the job
/// ran `aterm __setup-resume`, which such a build refused with exit 2 at every throttle for the
/// rest of the login, and at every login.
#[test]
fn an_aterm_without_the_resume_entry_answers_the_login_job_with_exit_0() {
    let (_scratch, home, root) = account();
    resume::serve_from(&home, &program(&home));
    authorize_at(&root, &selected(&[Setting::Widgets]))
        .unwrap()
        .unwrap();
    let text = std::fs::read_to_string(job(&home)).unwrap();
    let argv = aterm_secrets::service::plist_argv(&text).unwrap();
    let first = argv.get(1).expect("the job names its entry");
    assert!(
        crate::mode_free(first),
        "the front door answers {first:?} before it routes: {argv:?}"
    );
    assert_eq!(
        crate::decide_args(argv[1..].iter().cloned()),
        crate::CliAction::Version,
        "answered with the version and exit 0, whatever follows: {argv:?}"
    );
}

fn program(home: &Path) -> PathBuf {
    home.join("Applications/aterm.app/Contents/MacOS/aterm")
}

/// A launchctl that answers as launchd would for one job, and the calls it heard.
struct Launchd {
    loaded: bool,
    running: bool,
    heard: Vec<&'static str>,
}

impl Launchd {
    fn call(&mut self, call: Launchctl<'_>) -> Result<std::process::Output, String> {
        use std::os::unix::process::ExitStatusExt as _;
        let (verb, ok, stdout) = match call {
            Launchctl::Print(_, label) => {
                assert_eq!(label, resume::LABEL);
                let pid = if self.running { "\tpid = 77\n" } else { "" };
                (
                    "print",
                    self.loaded,
                    format!("gui/501/{label} = {{\n{pid}}}\n"),
                )
            }
            Launchctl::Bootstrap(_, plist) => {
                assert!(plist.ends_with("systems.alab.aterm-setup.plist"));
                self.loaded = true;
                self.running = true;
                ("bootstrap", true, String::new())
            }
            Launchctl::Bootout(..) => {
                self.loaded = false;
                self.running = false;
                ("bootout", true, String::new())
            }
            Launchctl::Kickstart(..) => {
                self.running = true;
                ("kickstart", true, String::new())
            }
            Launchctl::Restart(..) => {
                assert!(self.running, "only a running job is started again");
                ("restart", true, String::new())
            }
            other => panic!("keeping the login job never asks {other:?}"),
        };
        self.heard.push(verb);
        Ok(std::process::Output {
            status: std::process::ExitStatus::from_raw(if ok { 0 } else { 113 << 8 }),
            stdout: stdout.into_bytes(),
            stderr: Vec::new(),
        })
    }
}

/// Keeping the login job loads one launchd does not hold (which starts it), starts again one held
/// idle after its exit 0, leaves one that runs this build under this definition, reloads one
/// under an older definition (a window keeps it while it holds the run's claim, so the job is
/// between its passes), and with nothing owed removes its definition and asks launchd nothing.
#[test]
fn keeping_the_login_job_loads_or_starts_it_and_never_stops_one_that_runs() {
    let (_scratch, home, root) = account();
    resume::serve_from(&home, &program(&home));
    let boundary = aterm_types::update_boundary::Boundary::new();
    let revision = authorize_at(&root, &selected(&[Setting::Widgets]))
        .unwrap()
        .unwrap();
    let written = std::fs::read(job(&home)).unwrap();
    for (loaded, running, kept, heard) in [
        (
            false,
            false,
            SetupResume::Loaded,
            &["print", "bootstrap"][..],
        ),
        (true, true, SetupResume::Running, &["print"][..]),
        (
            true,
            false,
            SetupResume::Started,
            &["print", "kickstart"][..],
        ),
    ] {
        let mut launchd = Launchd {
            loaded,
            running,
            heard: Vec::new(),
        };
        let got = resume::keep_at(
            &root,
            &mut |call| launchd.call(call),
            &|_, _| false,
            &boundary,
        )
        .unwrap();
        assert_eq!(got, kept, "loaded {loaded}, running {running}");
        assert_eq!(launchd.heard, heard, "loaded {loaded}, running {running}");
        assert_eq!(std::fs::read(job(&home)).unwrap(), written);
    }

    // An older definition (another aterm's program) held idle is loaded again as this one.
    std::fs::write(job(&home), b"an older definition").unwrap();
    let mut launchd = Launchd {
        loaded: true,
        running: false,
        heard: Vec::new(),
    };
    let got = resume::keep_at(
        &root,
        &mut |call| launchd.call(call),
        &|_, _| false,
        &boundary,
    )
    .unwrap();
    assert_eq!(got, SetupResume::Loaded);
    assert_eq!(launchd.heard, ["print", "bootout", "print", "bootstrap"]);
    assert_eq!(std::fs::read(job(&home)).unwrap(), written);

    // The same with a job that runs between its passes: it is loaded again as this one now, not
    // left on the older definition until the next login.
    std::fs::write(job(&home), b"an older definition").unwrap();
    let mut launchd = Launchd {
        loaded: true,
        running: true,
        heard: Vec::new(),
    };
    let got = resume::keep_at(
        &root,
        &mut |call| launchd.call(call),
        &|_, _| panic!("a job under an older definition is loaded again whatever it runs"),
        &boundary,
    )
    .unwrap();
    assert_eq!(got, SetupResume::Loaded);
    assert_eq!(launchd.heard, ["print", "bootout", "print", "bootstrap"]);
    assert_eq!(std::fs::read(job(&home)).unwrap(), written);

    let (_, next) = execute_initial_for_revision(
        &mut Fake::default(),
        &root,
        revision,
        &mut None,
        SetupHooks::NONE,
    )
    .unwrap()
    .unwrap();
    assert_eq!(next, SetupNext::Done);
    // A definition a crash left after the run finished goes at the next keep, with no launchctl.
    std::fs::write(job(&home), &written).unwrap();
    let got = resume::keep_at(
        &root,
        &mut |call| panic!("nothing owed asks launchd nothing: {call:?}"),
        &|_, _| panic!("nothing owed reads no job"),
        &boundary,
    )
    .unwrap();
    assert_eq!(got, SetupResume::Ended);
    assert!(!job(&home).exists());
}

/// A login job launchd runs on a build an update replaced (its image is no longer the program
/// its definition names) is started again on this one (`kickstart -k`); one that runs this build
/// is left running. Before, the old build's job went on with the successor's records for the rest
/// of the login: a running job was never renewed.
#[test]
fn a_login_job_running_a_replaced_build_is_started_again_on_this_one() {
    let (_scratch, home, root) = account();
    resume::serve_from(&home, &program(&home));
    let boundary = aterm_types::update_boundary::Boundary::new();
    authorize_at(&root, &selected(&[Setting::Widgets]))
        .unwrap()
        .unwrap();
    for (replaced, kept, heard) in [
        (false, SetupResume::Running, &["print"][..]),
        (true, SetupResume::Renewed, &["print", "restart"][..]),
    ] {
        let mut launchd = Launchd {
            loaded: true,
            running: true,
            heard: Vec::new(),
        };
        let read = std::cell::RefCell::new(Vec::new());
        let got = resume::keep_at(
            &root,
            &mut |call| launchd.call(call),
            &|pid, at| {
                read.borrow_mut().push((pid, at.to_path_buf()));
                replaced
            },
            &boundary,
        );
        assert_eq!(got, Ok(kept), "replaced {replaced}");
        assert_eq!(launchd.heard, heard, "replaced {replaced}");
        assert_eq!(
            read.into_inner(),
            [(77, program(&home))],
            "the job's pid, against its program"
        );
    }
}

/// The login job ends (its exit 0) only when the records owe nothing and their writer is not
/// writing them: an end that meets profile.lock held, or a run admitted since, removes nothing
/// and tells the job to read the records again. Before, both answered as an end, and the job
/// exited 0 while a run was owed, which launchd does not start it again after.
#[test]
fn the_login_job_ends_only_once_nothing_is_owed_and_nothing_writes_the_records() {
    let (_scratch, home, root) = account();
    resume::serve_from(&home, &program(&home));
    authorize_at(&root, &selected(&[Setting::Widgets]))
        .unwrap()
        .unwrap();
    assert!(!resume::end_at(&root).unwrap(), "a run is owed");
    assert!(job(&home).exists());
    request_stop_at(&root, revision_at(&root).unwrap()).unwrap();
    execute_initial_for_revision(
        &mut Fake::default(),
        &root,
        revision_at(&root).unwrap(),
        &mut None,
        SetupHooks::NONE,
    )
    .unwrap();
    assert_eq!(resume::resumable_at(&root).unwrap(), None);
    std::fs::write(job(&home), b"a definition a crash left").unwrap();
    {
        let _writer = HeldLock::try_take_named(&root, "profile.lock")
            .unwrap()
            .expect("PRECONDITION: the records are free");
        assert!(
            !resume::end_at(&root).unwrap(),
            "their writer keeps the definition with them"
        );
        assert!(job(&home).exists());
    }
    assert!(resume::end_at(&root).unwrap());
    assert!(!job(&home).exists());
}

/// A run a Stop ended, and records anywhere but an account's own layout, have no login job.
#[test]
fn a_stopped_run_and_records_outside_an_account_have_no_login_job() {
    let (_scratch, home, root) = account();
    resume::serve_from(&home, &program(&home));
    let revision = authorize_at(&root, &selected(&[Setting::Widgets]))
        .unwrap()
        .unwrap();
    assert!(job(&home).exists());
    request_stop_at(&root, revision).unwrap();
    let mut host = Fake::default();
    let pass =
        execute_initial_for_revision(&mut host, &root, revision, &mut None, SetupHooks::NONE)
            .unwrap();
    assert!(pass.is_none(), "the pass folds the Stop and runs no stage");
    assert!(host.calls.is_empty());
    assert!(finished_at(&root).unwrap());
    assert!(!job(&home).exists(), "a stopped run owes no pass");
    assert_eq!(resume::resumable_at(&root).unwrap(), None);

    let bare = aterm_tempfile::tempdir().unwrap();
    authorize_at(bare.path(), &selected(&[Setting::Widgets]))
        .unwrap()
        .unwrap();
    assert!(
        resume::resumable_at(bare.path()).unwrap().is_some(),
        "the run is owed all the same"
    );
    let boundary = aterm_types::update_boundary::Boundary::new();
    let refused = resume::keep_at(
        bare.path(),
        &mut |call| panic!("records outside an account ask launchd nothing: {call:?}"),
        &|_, _| panic!("records outside an account read no job"),
        &boundary,
    );
    assert!(refused.is_err(), "{refused:?}");
}

/// Each window worker holds the window lock shared while it runs, as many may at once, in one
/// process or several: the login job reads a window open until the last worker returns, and not
/// after, though the windows stay open. Before, a window held it for its life, so a run reopened
/// after its worker returned (a widened `aterm init` cut short) waited for that window to quit.
#[test]
fn the_login_job_reads_a_window_open_only_while_a_window_worker_runs() {
    let root = aterm_tempfile::tempdir().unwrap();
    let root = root.path();
    assert!(!resume::window_open_at(root).unwrap());
    let first = resume::hold_window_at(root).unwrap();
    let second = resume::hold_window_at(root).unwrap();
    assert!(resume::window_open_at(root).unwrap());
    drop(first);
    assert!(
        resume::window_open_at(root).unwrap(),
        "one worker still runs setup"
    );
    drop(second);
    assert!(
        !resume::window_open_at(root).unwrap(),
        "every worker returned: the job takes the run up"
    );
}

/// An open window whose setup worker returned hears a run saved into the setup records
/// (`resume::SetupRecordsWatch`, which `aterm`'s `setup_take_up` waits on): a record renamed into
/// the folder, as every save writes one, ends its wait; a wait with nothing written ends on its
/// own time, saying nothing moved; and a watch whose folder went says so, so the window starts a
/// new one rather than wait on nothing.
#[test]
fn the_records_watch_hears_a_save_into_the_setup_records() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path().join("doctor");
    std::fs::create_dir_all(&root).unwrap();
    let watch = resume::SetupRecordsWatch::at(&root).unwrap();
    assert_eq!(
        watch.wait(Duration::from_millis(1)),
        Ok(false),
        "nothing written"
    );
    let staged = root.join(".initial-setup.json.tmp");
    std::fs::write(&staged, b"{}").unwrap();
    std::fs::rename(&staged, root.join("initial-setup.json")).unwrap();
    assert_eq!(
        watch.wait(Duration::from_secs(60)),
        Ok(true),
        "a save into the records moves it"
    );
    std::fs::remove_dir_all(&root).unwrap();
    assert!(
        watch.wait(Duration::from_secs(60)).is_err(),
        "a folder that went is said"
    );
}

/// The login job's pass runs every stage that needs nobody (design §6.4) and leaves the person
/// stages, Permissions and the sign-outs, for a window at this Mac: it parks on them, and a
/// window's pass then runs only those and finishes the run. Before, the only pass with no window
/// was the keyboard observers' Caps Lock pass, which left every other stage to a window.
#[test]
fn the_login_jobs_pass_runs_the_background_stages_and_leaves_the_person_stages() {
    let root = aterm_tempfile::tempdir().unwrap();
    let root = root.path();
    let plan = selected(&[
        Setting::Widgets,
        Setting::Spotlight,
        Setting::Repositories,
        Setting::Permissions,
        Setting::Messages,
    ]);
    let revision = authorize_at(root, &plan).unwrap().unwrap();
    let mut host = Fake::default();
    let (_, next) = execute_initial_for_revision_target(
        &mut host,
        root,
        revision,
        &mut None,
        SetupHooks::NONE,
        PassTarget::Background,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        host.calls,
        [Stage::Workstation, Stage::Spotlight, Stage::Repositories]
    );
    assert_eq!(next, SetupNext::Park, "the person stages wait for a window");
    assert!(!finished_at(root).unwrap());
    assert!(resume::resumable_at(root).unwrap().is_some());
    let done = progress(root).unwrap().unwrap().done;
    for stage in [Stage::Workstation, Stage::Spotlight, Stage::Repositories] {
        assert_ne!(done & stage.bit(), 0, "{stage:?} done");
    }
    for stage in [Stage::Permissions, Stage::Accounts] {
        assert_eq!(done & stage.bit(), 0, "{stage:?} left");
    }

    let mut host = Fake::default();
    execute_initial_for_revision(&mut host, root, revision, &mut None, SetupHooks::NONE)
        .unwrap()
        .unwrap();
    assert_eq!(host.calls, [Stage::Permissions, Stage::Accounts]);
    assert!(finished_at(root).unwrap());
}

/// The login job's pass gives way at its next stage boundary once a window runs setup (or an
/// update replaced its build): the stage it is in ends, no other begins, and the pass ends with
/// what it left to try again, which the window's pass goes on from. Before, the job yielded only
/// between whole passes, so a window opened after Quit & Reopen waited out every background stage
/// (hours of clones) before its own pass could begin.
#[test]
fn the_login_jobs_pass_gives_way_at_the_next_stage_boundary() {
    static BEGUN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    fn stage(event: StageEvent<'_>) {
        if event.running {
            BEGUN.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }
    // A window opens while the first stage runs.
    fn gives_way() -> bool {
        BEGUN.load(std::sync::atomic::Ordering::SeqCst) >= 1
    }
    let root = aterm_tempfile::tempdir().unwrap();
    let root = root.path();
    let plan = selected(&[Setting::Widgets, Setting::Spotlight, Setting::Repositories]);
    let revision = authorize_at(root, &plan).unwrap().unwrap();
    let mut host = Fake::default();
    let (_, next) = execute_initial_for_revision_target(
        &mut host,
        root,
        revision,
        &mut None,
        SetupHooks {
            stage,
            gives_way,
            ..SetupHooks::NONE
        },
        PassTarget::Background,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        host.calls,
        [Stage::Workstation],
        "one stage, then it gives way"
    );
    assert_eq!(next, SetupNext::Retry, "the rest is left to try again");
    assert!(!finished_at(root).unwrap());

    let mut host = Fake::default();
    execute_initial_for_revision(&mut host, root, revision, &mut None, SetupHooks::NONE)
        .unwrap()
        .unwrap();
    assert_eq!(
        host.calls,
        [Stage::Spotlight, Stage::Repositories],
        "the window's pass goes on from there"
    );
    assert!(finished_at(root).unwrap());
}
