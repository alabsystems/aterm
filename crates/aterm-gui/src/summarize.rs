// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! SUMMARIZE IN A NEW TAB (`docs/DESIGN-owner-surfaces-2026-10-16.md` §4):
//! prose about the alab costs one click. Agents ▸ Summarize the Fleet in a New
//! Tab (Summarize This Mac where this Mac is in no fleet) starts ONE worker
//! through the `spawn agent=claude` path, raised, with no role and `from=ctl`,
//! whose task is [`FLEET_BRIEF`] (or [`MAC_BRIEF`]): read the survey once, say
//! what needs the person, what each machine's managers and workers are doing
//! and what finished, and end. Its result arrives where every `from=ctl` task's
//! does (the menu bar's Done for you).
//!
//! The row is grey with the reason in its tool tip while a worker cannot
//! start here ([`StartBlock`], the spawn's own checks plus a `claude` on the
//! PATH a new tab starts from). Whether this Mac is in a fleet decides the
//! brief and Read the Fleet's row ([`in_fleet`]): the fleet secret in the
//! state directory, the one answer the fleet glance reads too.

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The task Summarize the Fleet gives its worker. FACTS and one finishable
/// task, never standing orders; SELF-CONTAINED: it names no filesystem path,
/// because the worker's cwd is wherever the focused pane was and an installed
/// aterm has no repo checkout.
pub(crate) const FLEET_BRIEF: &str = "Summarize this aterm fleet for the person reading this \
tab. Run `aterm ctl survey --fleet` once. Write, in this order: first what needs the person \
(the machine, the session, and why); then what each machine's managers and workers are doing; \
then what finished. Change nothing and start nothing. Say in ordinary language when the summary is complete.";

/// [`FLEET_BRIEF`] for a Mac in no fleet: the same summary of this Mac's
/// sessions, from `aterm ctl survey`.
pub(crate) const MAC_BRIEF: &str = "Summarize this Mac's aterm sessions for the person reading \
this tab. Run `aterm ctl survey` once. Write, in this order: first what needs the person (the \
session, and why); then what each manager and its workers are doing; then what finished. \
Change nothing and start nothing. Say in ordinary language when the summary is complete.";

/// The worker order Summarize spawns: the brief alone, no fact line naming a
/// starter (no client waits on it; its task record's `from=` is `ctl`).
pub(crate) fn summary_order(fleet: bool) -> crate::task_launch::TaskOrder {
    let brief = if fleet { FLEET_BRIEF } else { MAC_BRIEF };
    crate::task_launch::TaskOrder {
        task: brief.as_bytes().to_vec(),
        model: None,
        caller: None,
        trusted_cwd: None,
        fact_line: false,
    }
}

/// Why Summarize cannot start a worker here, said in the row's tool tip:
/// what the worker's spawn would refuse, or the missing `claude`
/// ([`crate::App::summarize_start_block`]).
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

/// Why a second Summarize press books nothing while the first one's tab is
/// still booking ([`crate::App::summarize_in_new_tab`]).
pub(crate) const SUMMARY_STARTING: &str = "a summary is already starting";

/// How long one look for `claude` answers [`claude_on_path`]: the menu's
/// live bits are published on the way to every wait, and an install or
/// uninstall is noticed within this.
const CLAUDE_LOOK_TTL: Duration = Duration::from_secs(2);

/// Whether the agent CLI (`claude`) is launchable from a new tab: a file by
/// that name on the PATH that tab's shell starts from — this process's PATH
/// (launchd's four dirs, for a Finder or Dock launch) plus aterm's managed
/// `agents/` and `bin/` and the places a foreign `claude` measurably lives
/// ([`crate::spawn::fallback_child_path`]). A handful of stat calls, looked
/// at again after [`CLAUDE_LOOK_TTL`] so an install or uninstall mid-run is
/// noticed; only the managed layout is resolved once.
pub(crate) fn claude_on_path() -> bool {
    #[cfg(test)]
    if let Some(answer) = CLAUDE_ON_PATH_FOR_TEST.get() {
        return answer;
    }
    static LOOK: Mutex<Option<(Instant, bool)>> = Mutex::new(None);
    let now = Instant::now();
    let mut look = LOOK.lock().unwrap_or_else(|p| p.into_inner());
    if let Some((at, answer)) = *look
        && now.saturating_duration_since(at) < CLAUDE_LOOK_TTL
    {
        return answer;
    }
    let answer = claude_on_path_now();
    *look = Some((now, answer));
    answer
}

#[cfg(test)]
thread_local! {
    /// TEST ARM of [`claude_on_path`]: `Some(answer)` stands in for the look
    /// at this machine's PATH on this thread, so a test of Summarize's row
    /// does not depend on what is installed where it runs.
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

/// WHETHER THIS MAC IS IN A FLEET, for the Agents rows and Summarize's brief:
/// the one answer the fleet glance's book reads too
/// (`fleet_glance::holds_fleet_key`), looked at once per [`FLEET_LOOK_TTL`]
/// on the main thread; no state directory is no fleet (the rows that need
/// one grey).
pub(crate) fn in_fleet() -> bool {
    static LOOK: Mutex<Option<(Instant, bool)>> = Mutex::new(None);
    let now = Instant::now();
    let mut look = LOOK.lock().unwrap_or_else(|p| p.into_inner());
    if let Some((at, answer)) = *look
        && now.saturating_duration_since(at) < FLEET_LOOK_TTL
    {
        return answer;
    }
    let answer = aterm_types::dirs::state_dir()
        .is_some_and(|dir| crate::fleet_glance::holds_fleet_key(&dir));
    *look = Some((now, answer));
    answer
}

impl crate::App {
    /// Why Summarize cannot launch here, `None` when it can: the worker's
    /// spawn would refuse ([`Self::worker_shell`], the spawn's own check), or
    /// no `claude` is on the PATH a new tab starts from ([`claude_on_path`]).
    pub(crate) fn summarize_start_block(&self) -> Option<StartBlock> {
        if let Err(block) = self.worker_shell() {
            return Some(StartBlock::Worker(block));
        }
        (!claude_on_path()).then_some(StartBlock::NoClaude)
    }

    /// AGENTS ▸ SUMMARIZE THE FLEET IN A NEW TAB: one worker (`spawn
    /// agent=claude`, [`Self::book_spawn`]) in a new tab of the front window,
    /// raised so the person who clicked sees it, no role, its task
    /// [`summary_order`] (the fleet's brief in a fleet, this Mac's
    /// otherwise), `from=ctl`. `Err` is the spawn's refusal before anything
    /// is booked — what [`Self::summarize_start_block`] greys the row for; a
    /// tab that did not open is said when it settles.
    pub(crate) fn summarize_in_new_tab(&mut self) -> Result<(), String> {
        if let Some(block) = self.summarize_start_block() {
            return Err(block.hint());
        }
        // SINGLE-FLIGHT: while one summary's tab is still booking, a second
        // press (a double click, the palette after the menu) books nothing.
        if self.tab_spawns.summary_pending() {
            return Err(SUMMARY_STARTING.to_string());
        }
        let plan = self.resolve_spawn(
            crate::control::SpawnAim::Front,
            None,
            Some(summary_order(in_fleet())),
        )?;
        self.book_spawn(
            plan,
            crate::control::control_media::SpawnBooking {
                cwd: None,
                split: None,
                raise: true,
                admission: crate::tab_spawn::SpawnAdmission::Summary,
            },
            Box::new(|app, spawned| {
                match spawned {
                    // No client waits on a reply: the worker's launcher
                    // starts at once (`App::start_spawned`), as a wire
                    // spawn's starts once its reply is taken.
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
    /// standing order, no filesystem path, and no role for the worker.
    #[test]
    fn the_briefs_read_the_survey_once_and_end() {
        for (fleet, survey) in [
            (true, "`aterm ctl survey --fleet`"),
            (false, "`aterm ctl survey`"),
        ] {
            let order = summary_order(fleet);
            let brief = String::from_utf8(order.task.clone()).expect("UTF-8");
            assert!(brief.contains(survey), "{brief}");
            assert!(!brief.contains("ATERM-TASK-DONE"), "no output marker");
            assert!(!brief.contains("meta set role"), "no role");
            assert_eq!(order.caller, None, "from=ctl");
            assert!(!order.fact_line);
        }
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
    /// NEGATIVE CONTROL: a role is just a label — an `operator` role standing
    /// elects nothing, so the summary still books and says it.
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
    /// anything is created — no tab. The CONTROL, an instance a worker can
    /// start in, has no block, and its press gets past the checks and books
    /// the spawn (which a unit test's App, with no event loop, answers at
    /// once: no tab, and the reservation released).
    #[test]
    fn summarize_is_grey_for_what_its_spawn_would_refuse() {
        use crate::control::control_media::WorkerBlock;
        // What turns a worker off, and the block that names it.
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
    /// tip says so instead of a launcher that stalls. NEGATIVE CONTROL: a
    /// fish that answers `3.7.1` passes once it has answered. Each stand-in
    /// runs once, unclocked, before the product meets it (macOS's first exec
    /// of a fresh script can outlast the product's own `--version` wait), and
    /// each must be remembered as the version it named, so a fish that timed
    /// out fails either half.
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
            assert!(app.worker_shell().is_ok(), "{version}: not known yet");
            // A hang detector, not a latency budget: the question must end.
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
                assert!(app.worker_shell().is_ok(), "{version} takes the operand");
            }
        }
    }
}
