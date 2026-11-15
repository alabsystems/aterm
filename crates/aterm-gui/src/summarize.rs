// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Summarize in a new tab (`docs/DESIGN-owner-surfaces-2026-10-16.md` §4).

use std::sync::Mutex;
use std::time::{Duration, Instant};

#[cfg(all(target_os = "macos", debug_assertions))]
#[path = "summarize_discovery_hold.rs"]
mod discovery_hold;

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

/// [`MAC_BRIEF`]'s survey sentence, after which [`FABRIC_READ`] goes.
const MAC_SURVEY: &str = "Run `aterm ctl survey` once, then `aterm ctl survey --brief` once, \
for what each session's log says.";

/// THE FABRIC'S SENTENCE: the brief's machine form, the read the Overview
/// makes, since only it says which sessions are this node's own. The fabric
/// is one machine's bus, so today every session there is this Mac's.
pub(crate) const FABRIC_READ: &str = "Then run `aterm fabric brief --json` once: a session \
with \"this\":true is one of this Mac's own, already in the survey; any other names its node in \
\"node\".";

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
        effort: None,
        caller: None,
        manager_node: None,
        trusted_cwd: None,
        fact_line: false,
        autopilot: alab_policy::config::Autopilot::Land,
        chrome: false,
        program: None,
    }
}

/// The name of [`summary_folder`] under aterm's state root.
const SUMMARY_FOLDER: &str = "summary";

/// THE FOLDER A SUMMARY'S WORKER STARTS IN.
pub(crate) fn summary_folder() -> Option<std::path::PathBuf> {
    summary_folder_under(&aterm_types::dirs::state_dir()?)
}

/// [`summary_folder`] under the state root `state`, created owner-only.
fn summary_folder_under(state: &std::path::Path) -> Option<std::path::PathBuf> {
    let dir = state.join(SUMMARY_FOLDER);
    #[cfg(unix)]
    crate::control_auth::ensure_private_dir(&dir).ok()?;
    #[cfg(not(unix))]
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Why Summarize books nothing when no summary folder can be prepared ([`summary_folder`]).
pub(crate) const NO_SUMMARY_FOLDER: &str =
    "no folder of aterm's own to start the summary in (no state directory)";

/// Whether `dir` is the summary folder under the state root `state` ([`summary_folder`])
/// or inside it.
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
        fleet: false,
    }
}

/// What Summarize books in `folder` ([`summary_folder`], [`summary_order`], [`summary_booking`]).
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
    /// The launch-path or fleet observation has not answered yet.
    DiscoveryPending,
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
            Self::DiscoveryPending => "(checking available agents)".to_string(),
            Self::NoClaude => "(claude not found \u{2014} aterm pkg install claude)".to_string(),
            Self::Worker(WorkerBlock::Unhosted) => {
                "(needs the control socket \u{2014} this aterm started without one)".to_string()
            }
            Self::Worker(WorkerBlock::Unsupervised(switch)) => {
                format!("(the window harness would not carry it \u{2014} {switch})")
            }
            Self::Worker(WorkerBlock::NoIntegration) => {
                "(needs shell integration \u{2014} off with --no-shell-integration)".to_string()
            }
            Self::Worker(WorkerBlock::Shell(name)) => {
                format!("(needs zsh, bash or fish as the shell, not {name})")
            }
            Self::Worker(WorkerBlock::OldFish) => "(needs fish 3.4 or later)".to_string(),
        }
    }
}

/// Why a second Summarize press books nothing ([`crate::App::summarize_in_new_tab`]).
pub(crate) const SUMMARY_STARTING: &str = "a summary is already starting";

/// How long one look for `claude` answers [`claude_on_path`].
const CLAUDE_LOOK_TTL: Duration = Duration::from_secs(2);
static CLAUDE_LOOK: Look = Look::new();

/// Whether the agent CLI (`claude`) is launchable from a new tab.
fn claude_on_path(
    proxy: Option<&aterm_window::event_loop::EventLoopProxy<crate::Wake>>,
) -> Option<bool> {
    #[cfg(test)]
    if let Some(answer) = CLAUDE_ON_PATH_FOR_TEST.get() {
        return answer;
    }
    CLAUDE_LOOK.answer(
        CLAUDE_LOOK_TTL,
        "aterm-claude-look",
        claude_on_path_now,
        Instant::now(),
        discovery_wake(proxy),
    )
}

/// Cached file-system check the main thread asks ([`claude_on_path`], [`in_fleet`]).
pub(crate) struct Look {
    state: Mutex<LookState>,
}

#[derive(Clone, Copy)]
struct LookState {
    /// The last completed look's answer.
    last: Option<bool>,
    /// A look is out on its own thread.
    looking: bool,
    /// The next refresh, or the retry after a worker could not start.
    next: Option<Instant>,
    /// Worker creation failed, so the event loop owes a timer to try again.
    retry_pending: bool,
}

impl Look {
    pub(crate) const fn new() -> Self {
        Self {
            state: Mutex::new(LookState {
                last: None,
                looking: false,
                next: None,
                retry_pending: false,
            }),
        }
    }

    /// The last answer at `now`, or `None` until the first background read finishes.
    pub(crate) fn answer(
        &'static self,
        ttl: Duration,
        name: &'static str,
        look: impl FnOnce() -> bool + Send + 'static,
        now: Instant,
        wake: impl FnOnce() + Send + 'static,
    ) -> Option<bool> {
        self.answer_with_spawn(ttl, look, now, wake, |work| {
            crate::qos::spawn_lane_thread(name, crate::qos::Role::Background, work).is_ok()
        })
    }

    fn answer_with_spawn(
        &'static self,
        ttl: Duration,
        look: impl FnOnce() -> bool + Send + 'static,
        now: Instant,
        wake: impl FnOnce() + Send + 'static,
        spawn: impl FnOnce(Box<dyn FnOnce() + Send>) -> bool,
    ) -> Option<bool> {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let answer = state.last;
        if !state.looking && state.next.is_none_or(|next| now >= next) {
            state.looking = true;
            state.retry_pending = false;
            drop(state);
            let spawned = spawn(Box::new(move || {
                let answer = look();
                let finished = Instant::now();
                let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
                let changed = state.last != Some(answer);
                *state = LookState {
                    last: Some(answer),
                    looking: false,
                    next: Some(finished + ttl),
                    retry_pending: false,
                };
                drop(state);
                if changed {
                    wake();
                }
            }));
            if !spawned {
                let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
                state.looking = false;
                state.next = Some(now + ttl);
                state.retry_pending = true;
            }
        }
        answer
    }

    fn retry_deadline(&self) -> Option<Instant> {
        let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.retry_pending.then_some(state.next).flatten()
    }

    /// Service only failed worker creation; settled or inflight reads need no timer.
    fn retry_if_due(&self, now: Instant, retry: impl FnOnce()) -> Option<Instant> {
        let at = self.retry_deadline()?;
        if now >= at {
            retry();
        }
        self.retry_deadline()
    }
}

fn discovery_wake(
    proxy: Option<&aterm_window::event_loop::EventLoopProxy<crate::Wake>>,
) -> impl FnOnce() + Send + 'static {
    let proxy = proxy.cloned();
    move || {
        if let Some(proxy) = proxy {
            let _ = proxy.send_event(crate::Wake::AgentsDiscoveryReady);
        }
    }
}

#[cfg(test)]
thread_local! {
    /// TEST ARM of [`claude_on_path`].
    pub(crate) static CLAUDE_ON_PATH_FOR_TEST: std::cell::Cell<Option<Option<bool>>> =
        const { std::cell::Cell::new(None) };
    static FLEET_FOR_TEST: std::cell::Cell<Option<Option<bool>>> =
        const { std::cell::Cell::new(None) };
}

/// [`claude_on_path`]'s look.
fn claude_on_path_now() -> bool {
    #[cfg(all(target_os = "macos", debug_assertions))]
    discovery_hold::claude();
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

/// How long one look at the fleet secret answers [`in_fleet`].
const FLEET_LOOK_TTL: Duration = Duration::from_secs(15);
static FLEET_LOOK: Look = Look::new();

fn in_fleet_now() -> bool {
    #[cfg(all(target_os = "macos", debug_assertions))]
    discovery_hold::fleet();
    aterm_types::dirs::state_dir().is_some_and(|dir| crate::fleet_glance::holds_fleet_key(&dir))
}

/// WHETHER THIS MAC IS IN A FLEET, for the Agents rows and Summarize's brief.
pub(crate) fn in_fleet(
    proxy: Option<&aterm_window::event_loop::EventLoopProxy<crate::Wake>>,
) -> Option<bool> {
    #[cfg(test)]
    if let Some(answer) = FLEET_FOR_TEST.get() {
        return answer;
    }
    FLEET_LOOK.answer(
        FLEET_LOOK_TTL,
        "aterm-fleet-look",
        in_fleet_now,
        Instant::now(),
        discovery_wake(proxy),
    )
}

impl crate::App {
    /// Retry a refused discovery worker from the loop's deadline, independent of menu reads.
    pub(crate) fn agents_discovery_retry(&self, now: Instant) -> Option<Instant> {
        let retry = |look: &'static Look, ttl: Duration, name: &'static str, read: fn() -> bool| {
            look.retry_if_due(now, || {
                let _ = look.answer(ttl, name, read, now, discovery_wake(self.proxy.as_ref()));
            })
        };
        [
            retry(
                &CLAUDE_LOOK,
                CLAUDE_LOOK_TTL,
                "aterm-claude-look",
                claude_on_path_now,
            ),
            retry(
                &FLEET_LOOK,
                FLEET_LOOK_TTL,
                "aterm-fleet-look",
                in_fleet_now,
            ),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Publish a completed discovery read without waiting for another input or the menu TTL.
    pub(crate) fn on_agents_discovery_ready(&mut self) {
        #[cfg(any(target_os = "macos", test))]
        {
            self.agents.agents_live_at = None;
            self.publish_agents_live(true, Instant::now());
        }
        self.palette_refresh_live();
    }

    /// Why Summarize cannot launch here, `None` when it can.
    pub(crate) fn summarize_start_block(&self) -> Option<StartBlock> {
        if let Err(block) = self.worker_shell(aterm_phase::Program::Claude) {
            return Some(StartBlock::Worker(block));
        }
        match claude_on_path(self.proxy.as_ref()) {
            None => Some(StartBlock::DiscoveryPending),
            Some(false) => Some(StartBlock::NoClaude),
            Some(true) if in_fleet(self.proxy.as_ref()).is_none() => {
                Some(StartBlock::DiscoveryPending)
            }
            Some(true) => None,
        }
    }

    /// What a Summarize press books ([`summary_spawn`], [`in_fleet`], [`Self::fabric_configured`]).
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
        let fleet =
            in_fleet(self.proxy.as_ref()).ok_or_else(|| StartBlock::DiscoveryPending.hint())?;
        summary_spawn(fleet, self.fabric_configured(), folder)
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
        let _act = crate::tab_marks::ActScope::gesture("summarize");
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
#[path = "summarize_tests.rs"]
mod tests;
