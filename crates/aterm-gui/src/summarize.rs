// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! SUMMARIZE IN A NEW TAB (`docs/DESIGN-owner-surfaces-2026-10-16.md` §4): prose about the alab costs one click.

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The task Summarize the Fleet gives its worker.
pub(crate) const FLEET_BRIEF: &str = "Summarize this aterm fleet for the person reading this \
tab. Run `aterm ctl survey --fleet` once. Write, in this order: first what needs the person \
(the machine, the session, and why); then what each machine's managers and workers are doing; \
then what finished. Change nothing and start nothing: this is a read-only summary, with nothing \
to commit, push or land.";

/// [`FLEET_BRIEF`] for a Mac in no fleet.
pub(crate) const MAC_BRIEF: &str = "Summarize this Mac's aterm sessions for the person reading \
this tab. Run `aterm ctl survey` once, then `aterm ctl survey --brief` once, for what each \
session's log says. Write, in this order: first what needs the person (the session, and why); \
then what each manager and its workers are doing; then what finished. \
Change nothing and start nothing: this is a read-only summary, with nothing to commit, push or \
land.";

/// [`FLEET_BRIEF`]'s survey sentence, after which [`FABRIC_READ`] goes.
const FLEET_SURVEY: &str = "Run `aterm ctl survey --fleet` once.";

/// [`MAC_BRIEF`]'s survey sentence (the survey, then the local brief), after
/// which [`FABRIC_READ`] goes.
const MAC_SURVEY: &str = "Run `aterm ctl survey` once, then `aterm ctl survey --brief` once, \
for what each session's log says.";

/// THE FABRIC'S SENTENCE.
pub(crate) const FABRIC_READ: &str =
    "Then run `aterm fabric brief` once, for what the fabric's other machines report.";

/// The brief a summary's worker is given.
pub(crate) fn summary_brief(fleet: bool, fabric: bool) -> String {
    let (brief, survey) = if fleet {
        (FLEET_BRIEF, FLEET_SURVEY)
    } else {
        (MAC_BRIEF, MAC_SURVEY)
    };
    if fabric {
        brief.replacen(survey, &format!("{survey} {FABRIC_READ}"), 1)
    } else {
        brief.to_string()
    }
}

/// The worker order Summarize spawns.
pub(crate) fn summary_order(fleet: bool, fabric: bool) -> crate::task_launch::TaskOrder {
    crate::task_launch::TaskOrder {
        task: summary_brief(fleet, fabric).into_bytes(),
        agent: aterm_phase::Program::Claude,
        model: None,
        caller: None,
        trusted_cwd: None,
        fact_line: false,
        autopilot: aterm_agent::supervise::Autopilot::Land,
    }
}

/// The name of [`summary_folder`] under aterm's state root.
const SUMMARY_FOLDER: &str = "summary";

/// THE FOLDER A SUMMARY'S WORKER STARTS IN.
pub(crate) fn summary_folder() -> Option<std::path::PathBuf> {
    summary_folder_under(&aterm_types::dirs::state_dir()?)
}

/// [`summary_folder`] under the state root `state`: made if it is not there
/// yet, then made owner-only; a folder whose mode cannot be set is no folder.
fn summary_folder_under(state: &std::path::Path) -> Option<std::path::PathBuf> {
    let dir = state.join(SUMMARY_FOLDER);
    std::fs::create_dir_all(&dir).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).ok()?;
    }
    Some(dir)
}

/// Why Summarize books nothing where aterm has no folder of its own to start the worker in ([`summary_folder`]).
pub(crate) const NO_SUMMARY_FOLDER: &str =
    "no folder of aterm's own to start the summary in (no state directory)";

/// Whether `dir` is the summary folder under the state root `state` ([`summary_folder`]) or inside it, as given or resolved.
pub(crate) fn in_summary_folder_under(dir: &std::path::Path, state: &std::path::Path) -> bool {
    let folder = state.join(SUMMARY_FOLDER);
    dir.starts_with(&folder)
        || folder
            .to_str()
            .and_then(crate::spawn_folder::canonical_folder)
            .is_some_and(|canon| dir.starts_with(canon))
}

/// The booking a summary's worker is spawned with.
pub(crate) fn summary_booking(
    folder: &std::path::Path,
) -> crate::control::control_media::SpawnBooking {
    crate::control::control_media::SpawnBooking {
        cwd: folder.to_str().map(str::to_string),
        split: None,
        raise: true,
        admission: crate::tab_spawn::SpawnAdmission::Summary,
    }
}

/// What Summarize books in `folder` ([`summary_folder`]): its order
/// ([`summary_order`]: the fleet's brief or this Mac's, the fabric's
/// sentence where `fabric` is configured), trusting that folder alone as a
/// spawn's named folder is, and its booking ([`summary_booking`]).
fn summary_spawn(
    fleet: bool,
    fabric: bool,
    folder: Option<&std::path::Path>,
) -> Result<
    (
        crate::task_launch::TaskOrder,
        crate::control::control_media::SpawnBooking,
    ),
    String,
> {
    let folder = folder.ok_or_else(|| NO_SUMMARY_FOLDER.to_string())?;
    let order = crate::task_launch::TaskOrder {
        trusted_cwd: folder
            .to_str()
            .and_then(crate::spawn_folder::canonical_folder),
        ..summary_order(fleet, fabric)
    };
    Ok((order, summary_booking(folder)))
}

/// Why Summarize cannot start a worker here, said in the row's tool tip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum StartBlock {
    /// No `claude` on the PATH a new tab's shell starts from.
    NoClaude,
    /// This instance starts no worker.
    Worker(crate::control::control_media::WorkerBlock),
}

impl StartBlock {
    /// The line that says why, and what changes it.
    pub(crate) fn hint(&self) -> String {
        use crate::control::control_media::WorkerBlock;
        match self {
            Self::NoClaude => "(claude not found \u{2014} aterm pkg install claude)".to_string(),
            Self::Worker(WorkerBlock::Unhosted) => {
                "(needs the control socket \u{2014} this aterm started without one)".to_string()
            }
            Self::Worker(WorkerBlock::Unsupervised(switch)) => {
                format!("(the window harness would not carry it \u{2014} {switch})")
            }
            Self::Worker(WorkerBlock::NoIntegration) => {
                "(needs shell integration \u{2014} off with -e or --no-shell-integration)"
                    .to_string()
            }
            Self::Worker(WorkerBlock::Shell(name)) => {
                format!("(needs zsh, bash or fish as the shell, not {name})")
            }
            Self::Worker(WorkerBlock::OldFish) => "(needs fish 3.4 or later)".to_string(),
        }
    }
}

/// Why a second Summarize press books nothing while the first one's tab is still booking ([`crate::App::summarize_in_new_tab`]).
pub(crate) const SUMMARY_STARTING: &str = "a summary is already starting";

/// How long one look for `claude` answers [`claude_on_path`]: the menu's
/// live bits are published on the way to every wait, and an install or
/// uninstall is noticed within this.
const CLAUDE_LOOK_TTL: Duration = Duration::from_secs(2);

/// Whether the agent CLI (`claude`) is launchable from a new tab: a file by that name on the PATH that tab's shell starts from.
pub(crate) fn claude_on_path() -> bool {
    #[cfg(test)]
    if let Some(answer) = CLAUDE_ON_PATH_FOR_TEST.get() {
        return answer;
    }
    static LOOK: Look = Look::new();
    LOOK.answer(
        CLAUDE_LOOK_TTL,
        "aterm-claude-look",
        claude_on_path_now,
        Instant::now(),
    )
}

/// A yes or no about the file system that the main thread asks on the way to every wait (the menu's live bits: [`claude_on_path`], [`in_fleet`]).
pub(crate) struct Look {
    state: Mutex<LookState>,
}

#[derive(Clone, Copy)]
struct LookState {
    /// The last look's answer, and when it ended.
    last: Option<(bool, Instant)>,
    /// A look is out on its own thread.
    looking: bool,
}

impl Look {
    pub(crate) const fn new() -> Self {
        Self {
            state: Mutex::new(LookState {
                last: None,
                looking: false,
            }),
        }
    }

    /// The answer at `now`: the first look's, made here.
    pub(crate) fn answer(
        &'static self,
        ttl: Duration,
        name: &'static str,
        look: fn() -> bool,
        now: Instant,
    ) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let Some((answer, at)) = state.last else {
            let answer = look();
            state.last = Some((answer, now));
            return answer;
        };
        if !state.looking && now.saturating_duration_since(at) >= ttl {
            state.looking = true;
            let spawned =
                crate::qos::spawn_lane_thread(name, crate::qos::Role::Background, move || {
                    let answer = look();
                    let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
                    *state = LookState {
                        last: Some((answer, Instant::now())),
                        looking: false,
                    };
                });
            if spawned.is_err() {
                *state = LookState {
                    last: Some((answer, now)),
                    looking: false,
                };
            }
        }
        answer
    }
}

#[cfg(test)]
thread_local! {
    /// TEST ARM of [`claude_on_path`].
    pub(crate) static CLAUDE_ON_PATH_FOR_TEST: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
}

/// [`claude_on_path`]'s look.
fn claude_on_path_now() -> bool {
    static MANAGED: std::sync::OnceLock<Option<(String, String)>> = std::sync::OnceLock::new();
    let managed = MANAGED.get_or_init(|| {
        let layout = atpkg::store::resolve_configured()?;
        Some((
            layout.agents_dir().to_str()?.to_owned(),
            layout.bin_dir().to_str()?.to_owned(),
        ))
    });
    let path = crate::spawn::fallback_child_path(
        std::env::var("PATH").ok().as_deref(),
        aterm_types::dirs::home_dir().as_deref(),
        managed
            .as_ref()
            .map(|(agents, bin)| (agents.as_str(), bin.as_str())),
    );
    std::env::split_paths(&path)
        .any(|dir| !dir.as_os_str().is_empty() && dir.join("claude").is_file())
}

/// How long one look at the fleet secret answers [`in_fleet`]: the menu's
/// live bits are published on the way to every wait, and membership changes
/// only at an `aterm fleet init|join|leave`.
const FLEET_LOOK_TTL: Duration = Duration::from_secs(15);

/// WHETHER THIS MAC IS IN A FLEET, for the Agents rows and Summarize's brief.
pub(crate) fn in_fleet() -> bool {
    static LOOK: Look = Look::new();
    LOOK.answer(
        FLEET_LOOK_TTL,
        "aterm-fleet-look",
        || {
            aterm_types::dirs::state_dir()
                .is_some_and(|dir| crate::fleet_glance::holds_fleet_key(&dir))
        },
        Instant::now(),
    )
}

impl crate::App {
    /// Why Summarize cannot launch here, `None` when it can.
    pub(crate) fn summarize_start_block(&self) -> Option<StartBlock> {
        if let Err(block) = self.worker_shell(aterm_phase::Program::Claude) {
            return Some(StartBlock::Worker(block));
        }
        (!claude_on_path()).then_some(StartBlock::NoClaude)
    }

    /// What a Summarize press books in `folder` ([`summary_spawn`]): the
    /// fleet's brief where this Mac is in a fleet ([`in_fleet`]), this Mac's
    /// otherwise, with the fabric's sentence where THIS aterm's fabric is
    /// configured ([`Self::fabric_configured`]).
    fn summary_spawn_in(
        &self,
        folder: Option<&std::path::Path>,
    ) -> Result<
        (
            crate::task_launch::TaskOrder,
            crate::control::control_media::SpawnBooking,
        ),
        String,
    > {
        summary_spawn(in_fleet(), self.fabric_configured(), folder)
    }

    /// AGENTS ▸ SUMMARIZE THE FLEET IN A NEW TAB.
    pub(crate) fn summarize_in_new_tab(&mut self) -> Result<(), String> {
        if let Some(block) = self.summarize_start_block() {
            return Err(block.hint());
        }
        if self.tab_spawns.summary_pending() {
            return Err(SUMMARY_STARTING.to_string());
        }
        let (order, booking) = self.summary_spawn_in(summary_folder().as_deref())?;
        let plan = self.resolve_spawn(crate::control::SpawnAim::Front, Some(order))?;
        self.book_spawn(
            plan,
            booking,
            Box::new(|app, spawned| {
                match spawned {
                    Ok(spawned) => {
                        let _ = app.start_spawned(spawned);
                    }
                    Err(e) => {
                        app.post_message(crate::message_reporters::summarize_not_started(&e));
                    }
                }
                app.mark_status_dirty();
            }),
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::App;

    /// The two briefs are one finishable task each: the survey they read
    /// (the fleet's with `--fleet`, this Mac's without), an ordinary report, no
    /// standing order, no filesystem path, no role for the worker, and
    /// nothing to land, under Land.
    #[test]
    fn the_briefs_read_the_survey_once_and_end() {
        for (fleet, fabric, survey) in [
            (true, false, "`aterm ctl survey --fleet`"),
            (false, false, "`aterm ctl survey`"),
            (true, true, "`aterm ctl survey --fleet`"),
            (false, true, "`aterm ctl survey`"),
        ] {
            let order = summary_order(fleet, fabric);
            let brief = String::from_utf8(order.task.clone()).expect("UTF-8");
            assert!(brief.contains(survey), "{brief}");
            assert!(!brief.contains("ATERM-TASK-DONE"), "no output marker");
            assert!(!brief.contains("meta set role"), "no role");
            assert_eq!(order.caller, None, "from=ctl");
            assert!(!order.fact_line);
            assert_eq!(
                order.autopilot,
                aterm_agent::supervise::Autopilot::Land,
                "spawned under Land: its end is its landing"
            );
        }
    }

    /// THE FABRIC'S SENTENCE, ONLY WHERE THE FABRIC IS ON,
    /// in all four arms: with no fabric each
    /// brief is [`FLEET_BRIEF`] or [`MAC_BRIEF`] byte for byte and names no
    /// fabric; with it, the same brief with exactly [`FABRIC_READ`] added,
    /// right after its survey and before what it asks for.
    #[test]
    fn the_briefs_read_the_fabric_only_where_it_is_on() {
        let task = |fleet: bool, fabric: bool| {
            String::from_utf8(summary_order(fleet, fabric).task).expect("UTF-8")
        };
        for (fleet, today, survey) in [
            (true, FLEET_BRIEF, FLEET_SURVEY),
            (false, MAC_BRIEF, MAC_SURVEY),
        ] {
            let plain = task(fleet, false);
            assert_eq!(plain, today, "byte for byte without the fabric");
            assert!(!plain.contains("fabric"), "{plain}");
            assert_eq!(today.matches(survey).count(), 1, "{today}");
            let read = task(fleet, true);
            assert!(
                read.contains(&format!("{survey} {FABRIC_READ} Write,")),
                "{read}"
            );
            assert_eq!(read.matches("aterm fabric brief").count(), 1, "{read}");
            assert_eq!(read.replacen(&format!(" {FABRIC_READ}"), "", 1), plain);
        }
    }

    /// THIS MAC'S BRIEF READS THE SURVEY, THEN THE LOCAL BRIEF:
    /// `aterm ctl survey` once, then `aterm ctl survey
    /// --brief` once, and where the fabric is on its sentence follows both.
    #[test]
    fn this_macs_brief_reads_the_survey_then_the_local_brief() {
        for fabric in [false, true] {
            let brief = String::from_utf8(summary_order(false, fabric).task).expect("UTF-8");
            let at = |s: &str| brief.find(s).unwrap_or_else(|| panic!("{s}: {brief}"));
            let survey = at("`aterm ctl survey` once");
            let local = at("`aterm ctl survey --brief` once");
            assert!(survey < local, "{brief}");
            assert_eq!(brief.matches("aterm ctl survey").count(), 2, "{brief}");
            if fabric {
                assert!(local < at("`aterm fabric brief`"), "{brief}");
            }
            let fleet = String::from_utf8(summary_order(true, fabric).task).expect("UTF-8");
            assert!(!fleet.contains("--brief"), "{fleet}");
        }
    }

    /// A PRESS NAMES THE FABRIC WHERE THIS ATERM'S IS ON,
    /// at the press: the order Summarize books
    /// ([`App::summary_spawn_in`]) carries [`FABRIC_READ`] exactly where
    /// [`App::fabric_configured`] says this aterm's fabric is configured.
    #[test]
    fn a_press_names_the_fabric_where_this_aterms_fabric_is_on() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let folder = summary_folder_under(root.path()).expect("made");
        let mut app = App::headless_for_test();
        for on in [false, true] {
            app.agents.overview_fabric.configured = on;
            let (order, booking) = app.summary_spawn_in(Some(&folder)).expect("books");
            assert_eq!(booking.cwd.as_deref(), folder.to_str());
            let task = String::from_utf8(order.task).expect("UTF-8");
            assert_eq!(task.contains(FABRIC_READ), on, "fabric {on}: {task}");
        }
    }

    /// A SUMMARY STARTS IN ATERM'S OWN FOLDER ([`summary_folder`],
    /// [`summary_booking`]), and the probe of a session's repository reads
    /// nothing there (`harness_host::session_repo_work`), so its clean
    /// defense lands under Land, a repository there or not.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn a_summary_starts_in_aterms_own_folder_where_no_repository_is_read() {
        let state = aterm_tempfile::tempdir().expect("state root");
        let folder = summary_folder_under(state.path()).expect("the summary folder");
        assert!(folder.is_dir() && in_summary_folder_under(&folder, state.path()));
        let booking = summary_booking(&folder);
        assert_eq!(booking.cwd.as_deref(), folder.to_str());
        assert!(booking.raise);
        let git = |dir: &std::path::Path, args: &[&str]| {
            let out = aterm_uds::spawnfd::command("git")
                .args([
                    "-c",
                    "user.name=Andrew Yates",
                    "-c",
                    "user.email=a@example.invalid",
                ])
                .args([
                    "-c",
                    "init.defaultBranch=main",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .current_dir(dir)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .expect("git");
            assert!(out.status.success(), "git {args:?}");
        };
        let dirty = |dir: &std::path::Path| {
            git(dir, &["init", "-q", "."]);
            std::fs::write(dir.join("notes.md"), "x").expect("notes");
        };
        dirty(&folder);
        let home = aterm_tempfile::tempdir().expect("home");
        assert_eq!(
            crate::harness_host::session_repo_work_under(
                &folder,
                Some(home.path()),
                Some(state.path())
            ),
            None
        );
        let elsewhere = aterm_tempfile::tempdir().expect("tmp");
        dirty(elsewhere.path());
        assert!(!in_summary_folder_under(elsewhere.path(), state.path()));
        let found = crate::harness_host::session_repo_work_under(
            elsewhere.path(),
            Some(home.path()),
            Some(state.path()),
        );
        assert!(found.is_some_and(|w| w.untracked == 1));
    }

    /// THE SUMMARY FOLDER IS PRIVATE ([`summary_folder_under`]): made under
    /// the state root it is given, owner-only (0o700) on unix even where the
    /// umask would leave it readable by others, and found again.
    #[test]
    fn the_summary_folder_is_private_to_its_owner() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let there = root.path().join(SUMMARY_FOLDER);
            std::fs::create_dir(&there).expect("pre-made");
            std::fs::set_permissions(&there, std::fs::Permissions::from_mode(0o755))
                .expect("world-readable");
        }
        let folder = summary_folder_under(root.path()).expect("made");
        assert_eq!(folder, root.path().join(SUMMARY_FOLDER));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&folder)
                .expect("meta")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o700, "owner-only");
        }
        assert_eq!(summary_folder_under(root.path()), Some(folder));
        let blocked = aterm_tempfile::tempdir().expect("tempdir");
        std::fs::write(blocked.path().join(SUMMARY_FOLDER), b"").expect("a file in its place");
        assert_eq!(
            summary_folder_under(blocked.path()),
            None,
            "CONTROL: no folder"
        );
    }

    /// WITH NO FOLDER OF ITS OWN, SUMMARIZE REFUSES ([`summary_spawn`]) with
    /// a plain reason instead of booking a tab where the focused pane was (a
    /// person's repository, where Land would press the summary to commit and
    /// push their work).
    #[test]
    fn summarize_with_no_folder_of_its_own_refuses_in_plain_words() {
        let refused = summary_spawn(true, false, None).map(|_| ());
        assert_eq!(refused, Err(NO_SUMMARY_FOLDER.to_string()));
        assert!(!NO_SUMMARY_FOLDER.contains("ATERM-TASK-DONE"));
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let folder = summary_folder_under(root.path()).expect("made");
        let (order, booking) = summary_spawn(false, false, Some(&folder)).expect("CONTROL: books");
        assert_eq!(booking.cwd.as_deref(), folder.to_str(), "started there");
        assert!(booking.raise && booking.split.is_none());
        let canonical = std::fs::canonicalize(&folder).expect("canonical");
        assert_eq!(
            order.trusted_cwd.as_deref(),
            canonical.to_str(),
            "trusted there"
        );
        assert_eq!(order.autopilot, aterm_agent::supervise::Autopilot::Land);
    }

    /// Summarize starts nothing where no worker can start: a headless test
    /// App hosts no harness, so the row's block is the spawn's own, and the
    /// press is refused with that reason before anything is booked.
    #[test]
    fn summarize_refuses_with_the_block_before_booking_anything() {
        let mut app = crate::App::headless_for_test();
        let block = app.summarize_start_block().expect("no harness here");
        assert!(matches!(block, StartBlock::Worker(_)), "{block:?}");
        assert_eq!(app.summarize_in_new_tab(), Err(block.hint()));
    }

    /// An App whose worker spawn this test can steer: a window harness
    /// hosted (as a control socket starts one) and on for a headless
    /// instance, shell integration on, a zsh — what a worker's spawn needs
    /// (`App::worker_shell`) — and `claude` on the PATH a new tab starts
    /// from ([`CLAUDE_ON_PATH_FOR_TEST`]).
    fn workers_app() -> App {
        CLAUDE_ON_PATH_FOR_TEST.set(Some(true));
        let mut app = App::headless_for_test();
        app.harness = Some(crate::harness_host::HostHandle::for_supervised_test(
            Vec::new(),
            std::sync::Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
        ));
        app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
            crate::app_config::HarnessPolicy::read("[harness]\nenabled = true\nheadless = true\n"),
        );
        app.session_factory.integrate = true;
        app.session_factory.shell_override = Some("/bin/zsh".to_string());
        app
    }

    /// A SUMMARY WHOSE TAB DID NOT OPEN IS SAID ON THE BAND, not a stderr
    /// line a Dock-launched aterm's nobody reads: past every check, a unit
    /// test's App (no event loop) answers the booked spawn at once with an
    /// error, and the press's own row says so; a second failure restates the
    /// one row, and the single-flight reservation was released each time.
    #[test]
    fn a_summary_whose_tab_did_not_open_is_said_on_the_band() {
        let said = |app: &App| {
            app.messages
                .live_by_key("harness.summarize")
                .map(|live| live.msg.title.clone())
        };
        let mut app = workers_app();
        assert_eq!(app.summarize_in_new_tab(), Ok(()));
        assert!(!app.tab_spawns.summary_pending(), "released");
        assert_eq!(app.summarize_in_new_tab(), Ok(()));
        assert_eq!(said(&app).as_deref(), Some("Couldn't start the summary"));
        let mut app = workers_app();
        let ctx = app.pool.get(0).expect("the stub").ctx.clone();
        ctx.meta.lock().unwrap().role = Some("operator".to_string());
        assert_eq!(app.summarize_in_new_tab(), Ok(()));
        assert!(said(&app).is_some());
    }

    /// SUMMARIZE IS GREY FOR WHAT ITS SPAWN WOULD REFUSE: with no window
    /// harness hosted (no control socket), the harness off, shell integration
    /// off, a shell no worker's line is written for, or no `claude` to start,
    /// the row's block names it, and a press is refused that way before
    /// anything is created — no tab.
    #[test]
    fn summarize_is_grey_for_what_its_spawn_would_refuse() {
        use crate::control::control_media::WorkerBlock;
        type Case = (fn(&mut App), fn(&StartBlock) -> bool);
        let blocked: [Case; 5] = [
            (
                |app| app.harness = None,
                |b| *b == StartBlock::Worker(WorkerBlock::Unhosted),
            ),
            (
                |app| {
                    app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
                        crate::app_config::HarnessPolicy::read("[harness]\nenabled = false\n"),
                    );
                },
                |b| {
                    *b == StartBlock::Worker(WorkerBlock::Unsupervised("[harness] enabled = false"))
                },
            ),
            (
                |app| app.session_factory.integrate = false,
                |b| *b == StartBlock::Worker(WorkerBlock::NoIntegration),
            ),
            (
                |app| app.session_factory.shell_override = Some("/bin/sh".to_string()),
                |b| matches!(b, StartBlock::Worker(WorkerBlock::Shell(_))),
            ),
            (
                |_| CLAUDE_ON_PATH_FOR_TEST.set(Some(false)),
                |b| *b == StartBlock::NoClaude,
            ),
        ];
        for (turn_off, is_block) in blocked {
            let mut app = workers_app();
            turn_off(&mut app);
            let sessions = app.pool.iter().count();
            let block = app.summarize_start_block().expect("the row is grey");
            assert!(is_block(&block), "{block:?}");
            assert_eq!(app.summarize_in_new_tab(), Err(block.hint()));
            assert_eq!(app.pool.iter().count(), sessions, "no tab was created");
        }
        let mut app = workers_app();
        assert_eq!(app.summarize_start_block(), None, "a worker can start here");
        let sessions = app.pool.iter().count();
        assert_eq!(app.summarize_in_new_tab(), Ok(()), "past every check");
        assert_eq!(app.pool.iter().count(), sessions, "no event loop, no tab");
        assert!(
            !app.tab_spawns.summary_pending(),
            "the reservation is released"
        );
    }

    /// A FISH THAT SAID IT IS OLDER THAN 3.4 GREYS SUMMARIZE: the first look
    /// at a fish starts its `--version` off the main thread and blocks
    /// nothing, and once it has answered `3.3.1` the worker's check refuses it
    /// (`WorkerBlock::OldFish`) before a worker is created, so the grey row's
    /// tip says so instead of a launcher that stalls.
    #[cfg(unix)]
    #[test]
    fn a_fish_known_older_than_3_4_greys_summarize() {
        use crate::control::control_media::WorkerBlock;
        use std::os::unix::fs::PermissionsExt as _;
        let dir = aterm_tempfile::Builder::new()
            .prefix("aterm-old-fish")
            .tempdir()
            .expect("scratch dir");
        for (version, old) in [("3.3.1", true), ("3.7.1", false)] {
            let folder = dir.path().join(version);
            std::fs::create_dir(&folder).expect("a folder per fish");
            let fish = folder.join("fish");
            std::fs::write(
                &fish,
                format!("#!/bin/sh\necho 'fish, version {version}'\n"),
            )
            .expect("a stand-in fish");
            std::fs::set_permissions(&fish, std::fs::Permissions::from_mode(0o755))
                .expect("executable");
            aterm_spec::wait::first_exec(&fish, &["--version"]);
            let mut app = workers_app();
            app.session_factory.shell_override = Some(fish.to_string_lossy().into_owned());
            assert!(
                app.worker_shell(aterm_phase::Program::Claude).is_ok(),
                "{version}: not known yet"
            );
            let asked = std::time::Instant::now();
            let answered = loop {
                if let Some(answered) = crate::task_launch::fish_answered(&fish) {
                    break answered;
                }
                assert!(
                    asked.elapsed() < std::time::Duration::from_secs(60),
                    "{version}: the fish was never asked"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            };
            assert_eq!(answered, Some(!old), "{version}: the version it named");
            if old {
                let block = StartBlock::Worker(WorkerBlock::OldFish);
                assert_eq!(app.summarize_start_block(), Some(block.clone()));
                assert_eq!(app.summarize_in_new_tab(), Err(block.hint()));
            } else {
                assert!(
                    app.worker_shell(aterm_phase::Program::Claude).is_ok(),
                    "{version} takes the operand"
                );
            }
        }
    }

    /// THE MENU'S LIVE FILE BITS ARE LOOKED AT AGAIN OFF THE MAIN THREAD: the
    /// first answer is looked at where it is asked; once it is older than its
    /// TTL the next ask starts one look on a thread of its own and answers the
    /// last at once, without waiting for it, and asks while it is out start no
    /// other; once it has ended its answer stands.
    #[test]
    fn a_stale_look_is_looked_at_again_off_the_asking_thread() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use std::sync::{Condvar, PoisonError};
        static LOOK: Look = Look::new();
        static LOOKS: AtomicUsize = AtomicUsize::new(0);
        static ANSWER: AtomicBool = AtomicBool::new(false);
        static GATE: (Mutex<bool>, Condvar) = (Mutex::new(true), Condvar::new());
        fn look() -> bool {
            LOOKS.fetch_add(1, Ordering::SeqCst);
            let (open, bell) = &GATE;
            let open = open.lock().unwrap_or_else(PoisonError::into_inner);
            let _ = bell
                .wait_timeout_while(open, Duration::from_secs(60), |open| !*open)
                .unwrap_or_else(PoisonError::into_inner);
            ANSWER.load(Ordering::SeqCst)
        }
        let ttl = Duration::from_secs(2);
        let t0 = Instant::now();
        assert!(
            !LOOK.answer(ttl, "aterm-test-look", look, t0),
            "the first, here"
        );
        assert_eq!(LOOKS.load(Ordering::SeqCst), 1);
        assert!(!LOOK.answer(ttl, "aterm-test-look", look, t0 + Duration::from_secs(1)));
        assert_eq!(LOOKS.load(Ordering::SeqCst), 1, "fresh: no look");
        *GATE.0.lock().unwrap_or_else(PoisonError::into_inner) = false;
        ANSWER.store(true, Ordering::SeqCst);
        let stale = t0 + ttl * 2;
        assert!(
            !LOOK.answer(ttl, "aterm-test-look", look, stale),
            "stale: the last answer, at once"
        );
        let began = Instant::now();
        while LOOKS.load(Ordering::SeqCst) < 2 {
            assert!(began.elapsed() < Duration::from_secs(60), "no look started");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(!LOOK.answer(ttl, "aterm-test-look", look, stale));
        assert_eq!(LOOKS.load(Ordering::SeqCst), 2, "one look out at a time");
        *GATE.0.lock().unwrap_or_else(PoisonError::into_inner) = true;
        GATE.1.notify_all();
        let began = Instant::now();
        while !LOOK.answer(ttl, "aterm-test-look", look, t0) {
            assert!(
                began.elapsed() < Duration::from_secs(60),
                "the look never ended"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(LOOKS.load(Ordering::SeqCst), 2);
    }
}
