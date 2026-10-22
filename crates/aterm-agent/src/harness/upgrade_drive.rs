// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The I/O half of [`super::upgrade`]: advances Claude Code sessions one step
//! at a time through the live upgrade. No Codex is moved live (the Codex
//! screen lane's retirement, 2026-10-16): the Codex pass
//! (`codex_alab_drive.rs`) carries codex-alab's crash relaunch in flight and
//! prunes the Codex records, and a tab whose restart in flight is a Codex's
//! belongs to that pass alone.
//!
//! Two callers take the same steps. The window's host asks [`due`] when the
//! session is attached, when atpkg's activation notice says a newer build was
//! installed ([`super::upgrade_wake`]), and on a short retry ladder while a
//! look could not read what decides it ([`Due::Unread`]). On [`Due::Yes`] its
//! worker takes one [`step`] at the loop's next idle point: announce, or after
//! READY end and relaunch the agent ([`super::relaunch`]), handing the new
//! process back to its supervisor ([`Opts::hand_back`]), whose next idle point
//! types the continuation ([`super::relaunch::resume`]). A person or script
//! runs [`sweep`] (`aterm harness upgrade`): one step for every session, the
//! continuation typed in the same step.
//!
//! Every fact comes from a source that cannot be stale for its process:
//! Claude's session file (checked against the kernel start time, so a recycled
//! pid never reads as the session), `KERN_PROCARGS2` for argv and any tab id,
//! the host's PTY foreground-group roster where macOS omits the environment,
//! the process table, and the tab's screen and hold over the control socket.
//! What cannot be read is a wait, never a guess; nothing types or signals
//! unless [`super::upgrade::next_step`] says go, and every step taken is one
//! ledger line.
//!
//! State is one small JSON file per conversation under
//! `<harness state>/upgrade/`, so the next step resumes where the last
//! stopped, including across the agent's own exit while that restart is
//! fresh ([`super::relaunch::expired`]). The same files are the owner's view
//! and carry the owner's word ([`rows`], [`View`], [`ask`]; `aterm harness
//! upgrade <sid> --now|--defer|--skip`).

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use aterm_json::{Map, Value};
use aterm_types::control_verbs::ScreenGen;

use super::relaunch::{
    Launch, NoPlan, Plan, PromptLook, PromptMark, Relaunch, RelaunchLineError, STALE_S,
    WAIT_HAND_LIFTS, await_new, carry_on, confirm, expired, fallback_carried, fallback_origin,
    hand_waits_out, launched, pause, plan, recorded, relaunch, relaunch_of, resume_mode,
    screen_mode, type_relaunch_line_with, unplanned, wait_now, wait_until,
};
use super::upgrade::{self, Candidate, Facts, Phase, Request, SessionFile, Source, Step, Version};
use super::upgrade_catalog::{self as catalog, Baked};
use super::upgrade_models::{self as models, ModelRecord, ModelVerdict, Priority};
use crate::RelayClient;
use crate::supervise::approvals;
use crate::supervise::policy::git_config::claude_dir_at;
use crate::supervise::run::composer_guard;
use crate::supervise::screen::HumanInput;

#[path = "upgrade_status.rs"]
mod upgrade_status;
pub(super) use upgrade_status::{last_command, wait_words_of};

#[path = "codex_alab_drive.rs"]
mod codex_alab_drive;

#[path = "upgrade_muse_drive.rs"]
mod muse;
/// The exit status of the command that just ran in a tab, from one `blocks 2
/// --json` reply (`head`, `body`): what a task's `exited` mark reads its
/// agent's code from (`supervise::task_mark`). The shell-integration reader
/// the Muse Code and agy lanes read their own exits with
/// ([`parse_exit_status`]).
pub(crate) use self::parse_exit_status as shell_exit_status;
pub(super) use muse::carry_in_flight as muse_carry_in_flight;

#[path = "upgrade_agy_drive.rs"]
mod agy;
pub(super) use agy::carry_in_flight as agy_carry_in_flight;

#[path = "upgrade_gemini_drive.rs"]
mod gemini;
pub(super) use gemini::carry_in_flight as gemini_carry_in_flight;

#[path = "upgrade_watch.rs"]
mod watch;

#[path = "upgrade_build_start.rs"]
mod build_start;
pub use build_start::BUILD_FAILS;
#[cfg(all(test, unix))]
pub(crate) use upgrade_status::pin_awake_since;
#[cfg(any(test, debug_assertions))]
pub use upgrade_status::progress_projection;
pub(super) use upgrade_status::runs_under;
pub use upgrade_status::{
    ATTENTION_OWNER, AUTOPILOT_OFF, Aside, Ask, Attention, CLAIMED, DIGEST_AFTER_S,
    HUNG_EXIT_AFTER_S, HeldBy, LIMIT_GRACE_S, Level, MOVE_BUDGET_S, NEED_THEN_MAX, NEED_WHY_MAX,
    NOT_FOREGROUND, NOW_QUIETS_S, Need, NeedSeverity, REFUSED_STOPPED, Remedy, Row,
    STALLED_AFTER_S, SUSPENDED_AFTER_S, TELL_S, UNWATCHED_AFTER_S, View, WAKE_CATCH_UP_S,
    WAKE_GRACE_S, ask, ask_for, awake_since, elapsed_words, refusal_sentence, refused_stopped,
    rows, status_rows, system_awake_since, tab_row, tab_rows, unwatched_since, word_marker,
};
#[cfg(all(test, unix))]
use watch::watch_with;
pub use watch::{Watched, Watcher, watch};

/// The world and mutants of `HarnessUpgradeLadder`
/// (`aterm_models_harness::harness_upgrade_ladder_model`): the lane's actions
/// bind where the real rule runs (`upgrade::rung`, `upgrade::gate`,
/// `St::still`, projected by [`ladder_projection`]); these are the agent's,
/// person's and clock's steps for the Tier-1 bind on Claude Code's lane
/// (`tests::ladder_claude`), plus the `Buggy = 1` members. Proof and test
/// builds only.
#[cfg(any(test, debug_assertions))]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "TurnStarts",
        reason = "A turn of the session's own starting: Claude Code's step. The Tier-1 bind reads it busy exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "TurnEnds",
        reason = "A turn of the session's own ending: the agent's step. The Tier-1 bind reads its end exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "Repaint",
        reason = "A redraw with no new words (a footer clock): the vendor's screen. The Tier-1 bind repaints the clock exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "Still",
        reason = "The screen's own quiet standing: the vendor's screen and the clock. The Tier-1 bind holds the screen still exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "Type",
        reason = "A person's keystroke in the tab: a person's step. The Tier-1 bind stamps the keystroke exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "KeysPause",
        reason = "A person pausing past the keys gap but still near: a person's step and the clock. The Tier-1 bind ages the stamp exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "KeysAway",
        reason = "A person leaving the tab past the grace: a person's step and the clock. The Tier-1 bind ages the stamp exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "DraftTyped",
        reason = "A draft left in the composer: a person's step. The Tier-1 bind types one exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "DraftCleared",
        reason = "That draft cleared: a person's step. The Tier-1 bind clears it exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "BoxUp",
        reason = "A dialog drawn over the composer: the vendor's step. The Tier-1 bind draws a measured box exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "BoxDown",
        reason = "That dialog answered: the vendor's step. The Tier-1 bind takes it down exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "SeqSettle",
        reason = "Buggy=1 negative control only; a look at a pause the ladder counts that waits `settling` for a screen that only repainted (the incident's 120 `wait:settling`), which `St::still` does not"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "AttendedWithoutLadder",
        reason = "Buggy=1 negative control only; a person merely near the tab holding the move at every rung, where `gate` narrows the grace to the keys gap from KeysOnly"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "NowWaivesThePerson",
        reason = "Buggy=1 negative control only; the owner's `--now` waiving the person outright, where `gate` keeps the keys gap at the last rung"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "MoveOverADraft",
        reason = "Buggy=1 negative control only; a move typed over a draft, which `gate` never makes"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "MoveOverABox",
        reason = "Buggy=1 negative control only; a move typed over a dialog, which `gate` never makes"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "MoveMidTurn",
        reason = "Buggy=1 negative control only; a session ended while its own turn runs, which `gate` holds"
    )
)]
#[doc(hidden)]
pub fn ladder_environment() {}

/// The ladder's projection (`aterm_models_harness::harness_upgrade_ladder_model`;
/// `#[refines]` anchors on `upgrade::gate`, `upgrade::rung`, `St::still`):
/// the model variables a look's real [`Facts`] stand for.
///
/// * `rung`: 0 Prefer … 3 Land;
/// * `work`: a turn the gate waits for (Claude's status or busy footer);
/// * `quiet`: the screen's own settle;
/// * `seen`: the reader's run of idle looks at the same words (2 once it stood
///   [`upgrade::QUIET_S`]);
/// * `keys`: 0 nobody near, 1 a person within the grace who has not typed
///   within [`upgrade::KEYS_GAP_S`], 2 one who has;
/// * `draft`, `dialog`.
///
/// Proof and test builds only.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
#[must_use]
pub fn ladder_projection(f: &Facts) -> std::collections::BTreeMap<&'static str, i64> {
    let flag = |b: bool| i64::from(b);
    let quiet = upgrade::quiet_s();
    std::collections::BTreeMap::from([
        (
            "rung",
            match f.rung {
                upgrade::Rung::Prefer => 0,
                upgrade::Rung::Settled => 1,
                upgrade::Rung::KeysOnly => 2,
                upgrade::Rung::Land => 3,
            },
        ),
        ("work", flag(f.status != "idle" || f.busy_footer)),
        ("quiet", flag(f.status_age_s >= quiet && f.quiet_s >= quiet)),
        ("seen", if f.still_s >= quiet { 2 } else { 0 }),
        ("keys", if f.typing { 2 } else { flag(f.attended) }),
        ("draft", flag(!f.composer_empty)),
        ("dialog", flag(f.approval_box)),
    ])
}

/// THE WORLD AND THE MUTANTS of `HarnessUpgradeProgress`
/// (`aterm_models_harness::harness_upgrade_progress_model`, design record
/// 2026-09-28 §4): its lane's actions are bound where the real code runs
/// (`upgrade::next_step`, `upgrade::break_step` and the watch's `watch_with`,
/// all projected by [`progress_projection`]); these are the session's, a
/// person's, the agent's and the clocks' steps, each one the Tier-1 binds
/// build, and the `Buggy = 1` members (among them the closed gap
/// `RecordedWordStands`: `upgrade::held_point`, `upgrade::NEXT_POINT`). What the owner is TOLD of a
/// record is not the machine's: `Row::attention` is bound over the same
/// states in `upgrade_progress_tests.rs`. Compiled only for proof and test
/// builds: nothing calls it.
#[cfg(any(test, debug_assertions))]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "TurnRuns",
        reason = "A turn of the agent's own starting: Claude Code's step (the loop's `NotIdle`/`NotBusy`, no point offered). The Tier-1 binds read it as a busy status and footer (the watch's read) exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "BreakSettles",
        reason = "A settled break of the agent's own background work: the agent's and its supervisor loop's (`host_steps_in_background`). The Tier-1 binds offer the break step (`background_point`) there, and read the screen as the watch's dry read does."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "IdlePoint",
        reason = "An idle point: the agent's turn ending and the loop offering it (`host_steps_here`). The Tier-1 binds read an idle, settled screen exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "WallStands",
        reason = "A usage limit's notice on the screen: Claude Code's and the API's step. The Tier-1 binds read `Facts::limited` exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "PersonHolds",
        reason = "A person's box or draft standing: a person's step (the loop's `Prompt` guard). The Tier-1 binds read an approval box exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "ResetPasses",
        reason = "The wall's named reset passing, its notice still on the screen: the clock's and the vendor's step, which the record keeps as the transcript names it (`St::limit_until`) and the view only READS (`Row::limit_passed`, `LIMIT_GRACE_S`). The Tier-1 view bind builds it through `limit_until`."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "WorkStarts",
        reason = "A process starting under the agent: the agent's step. The Tier-1 reducer bind names it in `Facts::background` exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "WorkEnds",
        reason = "That process ending: the agent's step. The Tier-1 reducer bind empties `Facts::background` exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "Answers",
        reason = "The agent's READY to the round's notice: the agent's step (`heard`). The Tier-1 reducer bind hands the reducer `ready` exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "Window",
        reason = "The round's clock running out: the clock's step (`REASK_S` since the notice or the pending round's progress, `RETRY_S` since the stop), which the reducer and the view only READ. The Tier-1 binds build it through `at_s`, `failed_s`/`retry_at` and `progress_at`."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "NoLookWithoutAPoint",
        reason = "Buggy=1 negative control only; main before `136ff0735`, whose view named no `watch_at` instant, so nothing looked at a record off a point. The host's wake at `watch_at` is bound by the existing T1-h tests."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "RecordedWordStands",
        reason = "Buggy=1 negative control only; main until 2026-09-29, whose watch recorded a word only for a read that waits, so a read that would act left the last word. The real read now says what holds the point of a give-up or a re-arm (`upgrade::held_point`) and the watch records it, else `upgrade::NEXT_POINT` (`upgrade_watch::look_word`), for a Claude Code record: a Codex record's act read records nothing still, and the settled-break half is a re-decision of the law's truth, not a change of code. The Tier-1 binds replay the old look against the real read, and aterm-spec's liveness sweep breaks the law with it."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "WatchTypes",
        reason = "Buggy=1 negative control only; the watch with hands, which the watch's read-only connections refuse before a byte is written (`a_dry_runs_connection_reads_and_has_no_hands`) and the Tier-1 watch tests refuse at the control socket (zero input requests)."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "RestartOverWork",
        reason = "Buggy=1 negative control only; the restart over processes under the agent, where the real `gate_restart` waits `background`."
    )
)]
#[doc(hidden)]
pub fn progress_environment() {}

/// What one sweep is told.
#[derive(Clone, Debug)]
pub struct Opts {
    /// The user's home (Claude's `~/.claude`, the native install, the hooks).
    pub home: PathBuf,
    /// The harness state directory; this module writes under `upgrade/` in it.
    pub state: PathBuf,
    /// A control socket to use instead of the one each tab's id resolves to.
    pub sock: Option<String>,
    /// Only this tab (`s-<hex>`), when set.
    pub only_sid: Option<String>,
    /// Plan and print; type, signal and write nothing.
    pub dry_run: bool,
    /// `[harness] human_grace_s`: a tab a person typed into this recently
    /// (`status` `human_ms=`) is held: nothing typed, agent not signalled.
    pub human_grace_s: u32,
    /// A supervisor loop takes the relaunched agent (the window's host): a
    /// relaunch ends once the new process holds the conversation (`adopted`),
    /// and the continuation is typed at the loop's next idle point
    /// ([`super::relaunch::resume`]), after the loop has answered whatever the
    /// new process opened with (trust dialog, resume choice, model consent).
    /// `false` (the hand-run verb, no loop) types it in the same step.
    pub hand_back: bool,
    /// The step is taken at a break of the agent's own background work
    /// ([`crate::supervise::IdleHost::at_background`]), not an idle point.
    /// Only a line that ends nothing may be typed: a notice (the first, or a
    /// re-ask after [`upgrade::REASK_S`]) or the release line a give-up, void
    /// or stop owes (`release_next`). Past [`upgrade::MAX_ASKS`] the upgrade
    /// gives up here ([`upgrade::Facts::background_point`],
    /// [`upgrade::next_step`]). Nothing is ended and no restart in flight is
    /// carried on.
    pub background: bool,
    /// The aterm state root under which each tab's supervisor keeps its
    /// approval ledger (`<root>/drive/<sid>.jsonl`): what its loop typed is the
    /// harness's own, no task ([`upgrade::TaskScan`], [`supervisor_typed`]).
    /// `None`: every unmarked turn reads as someone's.
    pub aterm_state: Option<PathBuf>,
    /// `[harness] upgrade_ask`: the upgrade asks first, typing its notice and
    /// restarting only on READY (the flow [`super::upgrade`] describes).
    /// `false`, the default: nothing is typed before the move, which comes only
    /// at a genuinely idle point ([`upgrade::Facts::quiet`]), and nothing is
    /// typed after the resume either ([`St::quiet`]).
    pub ask: bool,
    /// Two or more Claude Code accounts are listed for this home
    /// ([`super::accounts::several_listed`]; `docs/DESIGN-claude-accounts-2026-10-01.md`
    /// §1, §9). The home keeps one profile and every Claude Code start writes its
    /// login's model caches into it, so under two logins those caches say nothing
    /// about the session judged: they are not read ([`model_evidence`]) and a
    /// model is judged by the build's own catalog alone.
    pub several_logins: bool,
    /// `[harness] usage_credits` (`crate::supervise::SupervisorConfig::usage_credits`):
    /// a session may be moved onto a model the account bills to usage
    /// credits. `false` — the default, the owner's —
    /// reads the profile's word on how the plan bills Fable
    /// ([`credits_billed_for`]): such a model is offered to no upgrade
    /// ([`models::availability`]) and no relaunch is made onto it, a
    /// fallback's or a return's ([`super::relaunch`]'s
    /// `refused:model-on-credits`).
    pub usage_credits: bool,
    /// Set when `[harness] usage_tap` is on
    /// (`crate::supervise::SupervisorConfig::usage_tap`): the aterm a tap's
    /// status-line command names ([`super::tap::stable_exe`]) and the login of
    /// the machine's one account ([`super::tap::Armed`]). Every Claude Code
    /// relaunch line this pass types carries the tap where its gate holds
    /// ([`super::relaunch::plan`], [`super::tap::gate`]). `None` carries none.
    pub usage_tap: Option<super::tap::Armed>,
    /// Where a pass with no roster of its own learns the live tabs
    /// ([`Instances`], [`instance_tabs`]): the machine's for the product, a
    /// test's own stand-ins in a test.
    pub instances: Instances,
    /// The agents whose own switch is off (`[harness.<agent>] enabled =
    /// false`, `crate::supervise::SupervisorConfig::off`): a pass leaves their
    /// lanes alone ([`lanes`]), counts none of their restarts in flight
    /// ([`in_flight`]) and finds none of their tabs due ([`due`]). The CLI's
    /// sweep and the window's host (each step, and the carry after an update)
    /// fill it from the config; empty takes every lane.
    pub agents_off: Vec<aterm_phase::Program>,
    /// THE TAB'S AUTOPILOT IS LAND (`docs/DESIGN-harness-autopilot-2026-10-15.md`
    /// §2.2): a relaunch's carry-on types no continuation of Cruise's — no
    /// `keep going`, no Cruise prompt — and leaves its point to the tab's
    /// loop, whose land policy types the land instruction, a nudge, or
    /// nothing once the task landed (`done:landing`;
    /// [`super::relaunch::resume`], F2).
    /// The window's host sets it, for the one tab it carries on, from that
    /// session's effective Autopilot; `false` — the CLI's sweep, which
    /// carries no Autopilot of its own — reads the tab's own `status` at the
    /// carry-on (`relaunch::tab_under_land`), and carries on as Cruise does
    /// only where that says no Land.
    pub land: bool,
    /// Every bounded wait this pass takes ([`super::Timings`]): the
    /// production values for the product ([`super::Timings::for_build`], which a
    /// debug build's fast-timers seam turns to the test ones), the test values
    /// in a test.
    pub timings: super::Timings,
}

/// Where a pass with no roster of its own (the CLI's sweep) learns the live
/// tabs whose leaders name the Claude homes it reads ([`instance_tabs`],
/// [`Homes::read`]). A field, not a `cfg(test)` arm: the machine's instances
/// are the owner's, and a test (in-crate, or in `tests/` without `cfg(test)`)
/// names the sockets it may dial.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Instances {
    /// Every instance `aterm ctl ls` lists ([`aterm_ctl::local_instances`]);
    /// where that directory cannot be listed, or lists none, the one
    /// instance [`connect`] reaches. The product's.
    Machine,
    /// The instances at these control sockets and no other (none: no
    /// instance at all): a test's own stand-ins, never the machine's.
    Listed(Vec<String>),
}

/// Claude Code's own model files for the home `records` names
/// ([`models::read_evidence`]: the served catalog cache in its directory and
/// the profile kept for it, [`Records::profile`]), or the unknown
/// ([`models::Evidence`]'s default) while two or more accounts are listed
/// ([`Opts::several_logins`]). That the caches then follow the login that
/// started last is read in the bundle, not measured.
fn model_evidence(opts: &Opts, records: &Records) -> models::Evidence {
    // A home's standard-profile view for reports and cheap pre-filters. A
    // process's final decision is rebuilt with its own captured selector.
    let profile = records
        .profile(&opts.home)
        .zip(records.dir())
        .map(|(plain, dir)| super::profile::Context {
            plain,
            records: dir.to_path_buf(),
            selection: super::profile::Selection::Standard,
            home: None,
            config: None,
            login: None,
        });
    let credits_billed = credits_billed_for(opts, profile.as_ref()).unwrap_or_else(|| {
        models::CREDITS_BILLED_UNREAD
            .iter()
            .map(|f| (*f).to_string())
            .collect()
    });
    model_evidence_with_billing(opts, records, credits_billed)
}

fn model_evidence_with_billing(
    opts: &Opts,
    records: &Records,
    credits_billed: Vec<String>,
) -> models::Evidence {
    match (records.dir(), records.profile(&opts.home)) {
        (Some(dir), Some(profile)) if !opts.several_logins => models::Evidence {
            credits_billed,
            ..models::read_evidence(dir, &profile)
        },
        // The unknown — but for what usage credits bill, which fails closed.
        _ => models::Evidence {
            credits_billed,
            ..models::Evidence::default()
        },
    }
}

/// THE MODEL FAMILIES THE HOME'S ACCOUNT BILLS TO USAGE CREDITS now, under
/// `[harness] usage_credits = false` ([`Opts::usage_credits`]): by the
/// profile Claude Code keeps for the home `records` names
/// ([`super::profile::read`], [`models::credits_billed`]). None under the
/// key. FAILING CLOSED (the review of 2026-10-19, P4) with two or more
/// accounts listed (the profile is whichever login started last,
/// [`Opts::several_logins`]) and with no profile read: Fable
/// ([`models::CREDITS_BILLED_UNREAD`]) in the preliminary home view only.
/// A final process decision keeps unknown billing as `None`, permitting no
/// model move. `Some([])` is known not to bill a family, or the explicit key.
pub(super) fn credits_billed_for(
    opts: &Opts,
    profile: Option<&super::profile::Context>,
) -> Option<Vec<String>> {
    if opts.usage_credits {
        return Some(Vec::new());
    }
    if opts.several_logins {
        return None;
    }
    let profile = profile?;
    super::profile::effective_for_billing(&profile.plain, Some(&profile.records), profile.selection)
        .ok()
        .and_then(|path| super::profile::read(&path).ok())
        .map(|p| models::credits_billed(&p, i64::try_from(now_s()).unwrap_or(i64::MAX)))
}

/// The global config Claude Code keeps for the Claude home `claude_dir`
/// (onboarding, the profile, the model access caches), by the directory alone:
/// `~/.claude.json` beside the user's own `user_home/.claude`, and
/// `<dir>/.claude.json` inside any other home a `CLAUDE_CONFIG_DIR=<dir>` names
/// (`super::relaunch`'s module docs). Two raw values say otherwise: an empty
/// `CLAUDE_CONFIG_DIR` keeps its records in a directory whose profile is
/// `$HOME`'s, and one that names `~/.claude` itself keeps the profile inside
/// it. A reader that knows the home's raw value asks [`profile_apart`]
/// ([`Records::profile`]).
fn claude_profile(user_home: &Path, claude_dir: &Path) -> PathBuf {
    if claude_dir == user_home.join(".claude") {
        user_home.join(".claude.json")
    } else {
        claude_dir.join(".claude.json")
    }
}

/// What a sweep did with one session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    /// The agent's pid.
    pub pid: u32,
    /// The aterm tab (`s-<hex>`), or `-`.
    pub tab: String,
    /// The conversation.
    pub session: String,
    /// The running version.
    pub from: String,
    /// The target (`<version>(<source>)`), or `-`.
    pub to: String,
    /// The step taken or the reason for waiting.
    pub step: String,
}

impl Report {
    /// Whether this report is something the sweep did or recorded (typed,
    /// signalled, relaunched, gave up, refused, held back), as opposed to a
    /// wait, a skip or `current`.
    #[must_use]
    pub fn is_act(&self) -> bool {
        !(self.step == "current"
            || self.step.starts_with("wait:")
            || self.step.starts_with("skip:")
            || self.step.starts_with("busy:")
            || self.step.starts_with("would-"))
    }

    /// One line: `upgrade pid=… tab=… session=… from=… to=… step=…`.
    #[must_use]
    pub fn line(&self) -> String {
        self.line_as("upgrade")
    }

    /// The same line under another name (`relaunch` for a relaunch on exit,
    /// `carry-on` for a relaunched agent's continuation), so a log never
    /// calls a relaunch an upgrade.
    #[must_use]
    pub fn line_as(&self, kind: &str) -> String {
        format!(
            "{kind} pid={} tab={} session={} from={} to={} step={}",
            self.pid, self.tab, self.session, self.from, self.to, self.step
        )
    }
}

thread_local! {
    /// The second a pass on this thread reads as now ([`at_second`]): `None`,
    /// the wall clock.
    static GIVEN_NOW: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

/// Now, in unix seconds: the second this thread's pass was given
/// ([`at_second`]), else the wall clock.
pub(super) fn now_s() -> u64 {
    GIVEN_NOW
        .with(std::cell::Cell::get)
        .unwrap_or_else(|| aterm_time::unix_secs().unwrap_or(0))
}

/// `f`, with every [`now_s`] it reads on this thread answering `now`: a pass
/// handed a time (the watch, [`watch::watch_with`]) decides at that time
/// throughout, its read included, never at the wall clock's. The previous
/// second is restored after, a panic included.
pub(super) fn at_second<R>(now: u64, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<u64>);
    impl Drop for Restore {
        fn drop(&mut self) {
            GIVEN_NOW.with(|c| c.set(self.0));
        }
    }
    let _restore = Restore(GIVEN_NOW.with(|c| c.replace(Some(now))));
    f()
}

// ---------------------------------------------------------------- processes
//
// THE PROCESS TABLE, AS `ps` READS IT. macOS reads the kernel's records in
// this process ([`aterm_sysprobe::kproc`]: `sysctl(KERN_PROC)`, the table `ps`
// itself reads, and `KERN_PROCARGS2`): a `ps` child cost 0.02-0.4 s of a
// loaded Mac's wall each, and the host's read of one tab's job took five of
// them, several times a second, through every attach. Every other host runs
// `ps`. The values are `ps`'s own, column for column (`upgrade_drive_kproc_tests.rs`
// holds the two to each other on the live machine); a terminal is a word only
// the walk compares ([`tty_word`]).

/// `ps` in the C locale and UTC, the rendering Claude's `procStart` uses.
///
/// Built through `aterm_uds::spawnfd::command`: on Windows a `ps` (Git for
/// Windows, MSYS) is a console image and the harness host has no console, so a
/// bare spawn would flash a console window.
#[cfg(any(not(target_os = "macos"), test))]
fn ps(args: &[&str]) -> Option<String> {
    let mut cmd = aterm_uds::spawnfd::command("ps");
    cmd.args(args).env("LC_ALL", "C").env("TZ", "UTC");
    let out = aterm_uds::deadline::output_until(&mut cmd, Instant::now() + Duration::from_secs(10))
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

pub(super) fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The kernel's start time of `pid`, rendered as Claude renders `procStart`.
///
/// On Windows that rendering is the raw creation `FILETIME` in decimal (100 ns
/// ticks since 1601-01-01 UTC), the exact integer Claude Code writes as a
/// session's `procStart` (measured: `134369042337562842`). It is read by
/// syscall ([`aterm_uds::process::read_creation_filetime`]: `GetProcessTimes`),
/// never `ps`: Git for Windows' MSYS `ps` takes Cygwin pids and has no `-o
/// lstart`, so a `ps` here answers nothing and would flash a console window
/// from the window's console-less harness host. On macOS the kernel's record
/// ([`aterm_sysprobe::kproc::record`]); elsewhere `ps -o lstart=` in the C
/// locale and UTC (whitespace-squashed), the spelling `procStart` carries there.
pub(super) fn kernel_start(pid: u32) -> Option<String> {
    #[cfg(windows)]
    {
        aterm_uds::process::read_creation_filetime(i32::try_from(pid).ok()?)
            .map(|ticks| ticks.to_string())
    }
    #[cfg(target_os = "macos")]
    {
        aterm_sysprobe::kproc::record(pid).map(|r| super::footer::lstart_utc(r.start_s))
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let s = squash(&ps(&["-o", "lstart=", "-p", &pid.to_string()])?);
        (!s.is_empty()).then_some(s)
    }
}

/// Whether process `pid` lives (one we may not signal included). Pid 0 is no
/// process — `kill(0, 0)` asks about the caller's own process group, which
/// always answers — so it is never alive: [`super::relaunch::NO_SHELL`], the
/// shell of a tab's own program, reads gone wherever it is asked about.
pub(super) fn alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(unix)]
    {
        let Ok(p) = libc::pid_t::try_from(pid) else {
            return false;
        };
        // SAFETY: signal 0 checks existence and permission; nothing is delivered.
        let rc = unsafe { libc::kill(p, 0) };
        rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    // Windows: the workspace's one `OpenProcess` probe, the same contract
    // (a pid that exists but is not ours reads alive, like `EPERM`).
    #[cfg(not(unix))]
    {
        aterm_uds::process::pid_alive(pid)
    }
}

/// `(ppid, pgid, tpgid)` of `pid`.
pub(super) fn ids(pid: u32) -> Option<(u32, i64, i64)> {
    #[cfg(target_os = "macos")]
    {
        let r = aterm_sysprobe::kproc::record(pid)?;
        Some((r.ppid, i64::from(r.pgid), i64::from(r.tpgid)))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let out = ps(&["-o", "ppid=,pgid=,tpgid=", "-p", &pid.to_string()])?;
        let mut it = out.split_whitespace();
        Some((
            it.next()?.parse().ok()?,
            it.next()?.parse().ok()?,
            it.next()?.parse().ok()?,
        ))
    }
}

/// A cheap kernel group lookup before any process-table sweep. The focused
/// `ids` read later checks the controlling terminal's foreground group too.
fn process_group(pid: u32) -> Option<i64> {
    #[cfg(unix)]
    {
        let pid = libc::pid_t::try_from(pid).ok()?;
        // SAFETY: getpgid reads one process's kernel identity without changing it.
        let group = unsafe { libc::getpgid(pid) };
        (group > 0).then_some(i64::from(group))
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        None
    }
}

/// One row of the process table: `(pid, ppid, command basename)`, the
/// basename read when it is first asked for ([`Comm`]).
pub(super) type ProcRow = (u32, u32, Comm);

/// A process table row's command basename: the `comm` column `ps` prints,
/// [`base_name`]d. Given whole where a row was parsed from `ps` (or written by
/// a test); on macOS read from the row's kernel record the first time it is
/// asked for ([`comm_name`]). Its `argv[0]` costs a `KERN_PROCARGS2` read and
/// an escape per process, 14-16 ms of a table of 1 100 (measured 2026-10-20,
/// a debug build, where the records alone took 1.2 ms), and nearly every
/// reader names a few rows (an agent's descendants, a shell's own row), where
/// every row once paid. A name read later than its row is the row's process's
/// as the old table's was: each row's words were read one by one after the
/// records, never with them.
#[derive(Clone)]
pub(super) struct Comm {
    name: std::sync::OnceLock<String>,
    #[cfg(target_os = "macos")]
    record: Option<aterm_sysprobe::kproc::Record>,
}

impl Comm {
    /// The basename `r`'s `argv[0]` gives ([`comm_name`]), read when first
    /// asked for.
    #[cfg(target_os = "macos")]
    fn named_when_asked(r: aterm_sysprobe::kproc::Record) -> Self {
        Self {
            name: std::sync::OnceLock::new(),
            record: Some(r),
        }
    }

    /// The basename.
    pub(super) fn as_str(&self) -> &str {
        self.name.get_or_init(|| {
            #[cfg(target_os = "macos")]
            if let Some(r) = &self.record {
                return comm_name(r);
            }
            String::new()
        })
    }
}

impl From<String> for Comm {
    fn from(name: String) -> Self {
        Self {
            name: std::sync::OnceLock::from(name),
            #[cfg(target_os = "macos")]
            record: None,
        }
    }
}

impl From<&str> for Comm {
    fn from(name: &str) -> Self {
        Self::from(name.to_string())
    }
}

impl std::ops::Deref for Comm {
    type Target = str;
    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl PartialEq<str> for Comm {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for Comm {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq for Comm {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl std::fmt::Debug for Comm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self.as_str(), f)
    }
}

impl std::fmt::Display for Comm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The whole table: `(pid, ppid, command basename)`.
pub(super) fn table() -> Vec<ProcRow> {
    #[cfg(target_os = "macos")]
    {
        aterm_sysprobe::kproc::records()
            .unwrap_or_default()
            .into_iter()
            .map(|r| (r.pid, r.ppid, Comm::named_when_asked(r)))
            .collect()
    }
    #[cfg(not(target_os = "macos"))]
    {
        ps_table()
    }
}

/// [`table`] from `ps -A -o pid=,ppid=,comm=`.
#[cfg(any(not(target_os = "macos"), test))]
fn ps_table() -> Vec<ProcRow> {
    let Some(out) = ps(&["-A", "-o", "pid=,ppid=,comm="]) else {
        return Vec::new();
    };
    out.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let pid = it.next()?.parse().ok()?;
            let ppid = it.next()?.parse().ok()?;
            Some((
                pid,
                ppid,
                base_name(&it.collect::<Vec<_>>().join(" ")).into(),
            ))
        })
        .collect()
}

/// The basename of the `comm` column `ps` prints for `r` (its `argv[0]`,
/// [`base_name`] of the whitespace-squashed word as the `ps` row's parse
/// gives it).
#[cfg(target_os = "macos")]
fn comm_name(r: &aterm_sysprobe::kproc::Record) -> String {
    base_name(&squash(&aterm_sysprobe::kproc::ps_comm(
        r,
        aterm_sysprobe::kproc::Locale::C,
    )))
}

/// A controlling terminal as one word of a table row: `??` for none, as `ps`
/// prints it, else its device number. The walk ([`terminal_owner`]) only asks
/// whether two rows share a terminal and whether a row has one; `ps`'s NAME
/// for a device costs a scan of `/dev`.
#[cfg(target_os = "macos")]
fn tty_word(tty: Option<i32>) -> String {
    tty.map_or_else(|| "??".to_string(), |dev| format!("dev{dev}"))
}

/// Every descendant of `root` in the process table `table` (`(pid, ppid,
/// basename)`), breadth first; `None` for an empty table, which no read of a
/// live machine is (`ps` itself is in it): unread, never "no child". The live
/// upgrade's process rule of the lanes past Claude Code's (Muse Code's G-M3,
/// agy's G-A2(d)): an idle agent of theirs has no child at all.
pub(super) fn descendants_in(table: &[ProcRow], root: u32) -> Option<Vec<(u32, String)>> {
    if table.is_empty() {
        return None;
    }
    let mut found = Vec::new();
    let mut frontier = vec![root];
    while let Some(parent) = frontier.pop() {
        for (pid, ppid, name) in table {
            if *ppid == parent && *pid != root && !found.iter().any(|(p, _)| p == pid) {
                found.push((*pid, name.to_string()));
                frontier.push(*pid);
            }
        }
    }
    Some(found)
}

/// A `comm` column's basename, a login shell's leading `-` dropped.
fn base_name(comm: &str) -> String {
    comm.rsplit('/')
        .next()
        .unwrap_or(comm)
        .trim_start_matches('-')
        .to_string()
}

/// The whole table with each process's controlling terminal:
/// `(pid, ppid, tty, command basename)`, the tty `??` (macOS) or `?` (Linux)
/// for none.
#[cfg(any(not(target_os = "macos"), test))]
fn tty_table() -> Vec<(u32, u32, String, String)> {
    let Some(out) = ps(&["-A", "-o", "pid=,ppid=,tty=,comm="]) else {
        return Vec::new();
    };
    out.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let pid = it.next()?.parse().ok()?;
            let ppid = it.next()?.parse().ok()?;
            let tty = it.next()?.to_string();
            Some((pid, ppid, tty, base_name(&it.collect::<Vec<_>>().join(" "))))
        })
        .collect()
}

/// The process that owns `pid`'s terminal: walking up from `pid`, the first
/// ancestor not on `pid`'s tty, which opened the pty and holds its master.
/// `(pid, basename)`, or `None` when `pid` has no terminal or the walk breaks.
/// This tells an aterm tab from a pane: a multiplexer (tmux, screen, zellij,
/// dtach) opens its own pty for every pane.
fn terminal_owner(pid: u32, t: &[(u32, u32, String, String)]) -> Option<(u32, &str)> {
    let row = |p: u32| t.iter().find(|(q, _, _, _)| *q == p);
    let tty = row(pid)?.2.as_str();
    if !tty.chars().any(|c| c.is_ascii_digit()) {
        return None; // `??`, `?`, `-`: no controlling terminal at all.
    }
    let mut p = pid;
    for _ in 0..64 {
        let (_, pp, _, _) = row(p)?;
        let (q, _, qtty, name) = row(*pp)?;
        if qtty != tty {
            return Some((*q, name.as_str()));
        }
        p = *q;
    }
    None
}

/// [`Kernel::terminal`] read now: the walk ([`terminal_owner`]) over the whole
/// table. macOS reads the kernel's records once and names the owner alone
/// ([`comm_name`]): the walk needs every row's parent and terminal, and no
/// other row's name.
fn live_terminal(pid: u32) -> Option<(u32, String)> {
    #[cfg(target_os = "macos")]
    {
        let rows = aterm_sysprobe::kproc::records()?;
        let walk: Vec<(u32, u32, String, String)> = rows
            .iter()
            .map(|r| (r.pid, r.ppid, tty_word(r.tty), String::new()))
            .collect();
        let (owner, _) = terminal_owner(pid, &walk)?;
        let r = rows.iter().find(|r| r.pid == owner)?;
        Some((owner, comm_name(r)))
    }
    #[cfg(not(target_os = "macos"))]
    {
        terminal_owner(pid, &tty_table()).map(|(owner, name)| (owner, name.to_string()))
    }
}

/// [`Kernel::opener`] from one read of `agent` and `parent` alone: the walk
/// [`terminal_owner`] makes stops at `parent` exactly when `agent` is on a
/// terminal and `parent` is on none or another, which is all the job's read
/// needs to know (a whole table costs some 20 ms of `ps` on a busy Mac, and
/// every look at the job pays this one).
fn opener(agent: u32, parent: u32) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let tty = aterm_sysprobe::kproc::record(agent)?.tty?;
        let parent = aterm_sysprobe::kproc::record(parent)?;
        (parent.tty != Some(tty)).then(|| comm_name(&parent))
    }
    #[cfg(not(target_os = "macos"))]
    {
        ps_opener(agent, parent)
    }
}

/// [`opener`] from `ps -o pid=,tty=,comm= -p <agent>,<parent>`.
#[cfg(any(not(target_os = "macos"), test))]
fn ps_opener(agent: u32, parent: u32) -> Option<String> {
    let out = ps(&["-o", "pid=,tty=,comm=", "-p", &format!("{agent},{parent}")])?;
    let rows: Vec<(u32, String, String)> = out
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let pid = it.next()?.parse().ok()?;
            let tty = it.next()?.to_string();
            Some((pid, tty, base_name(&it.collect::<Vec<_>>().join(" "))))
        })
        .collect();
    let row = |p: u32| rows.iter().find(|(q, _, _)| *q == p);
    let (_, tty, _) = row(agent)?;
    if !tty.chars().any(|c| c.is_ascii_digit()) {
        return None; // `??`, `?`, `-`: no controlling terminal at all.
    }
    let (_, parent_tty, name) = row(parent)?;
    (parent_tty != tty).then(|| name.clone())
}

/// Whether the terminal `owner` ([`terminal_owner`]) opened is an aterm tab:
/// the owner is an aterm process (`aterm` or an `aterm-<tool>` alias), or pid
/// 1 (the aterm that opened the pty handed its tabs to a successor in a live
/// update and exited, reparenting the shell). A multiplexer cannot exit while
/// its pane lives, so pid 1 never stands for one. `script`, `ssh`, `sudo`'s
/// pty and multiplexers read as not the tab's: typing reaches what they show,
/// not the agent.
pub(super) fn owned_by_aterm(owner: (u32, &str)) -> bool {
    owner.0 == 1 || owner.1 == "aterm" || owner.1.starts_with("aterm-")
}

/// The owner's name as one report word: `[A-Za-z0-9._-]`, at most 32 bytes.
fn owner_word(owner: Option<(u32, &str)>) -> String {
    let word: String = owner
        .map(|(_, name)| name)
        .unwrap_or("none")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .take(32)
        .collect();
    if word.is_empty() {
        "?".to_string()
    } else {
        word
    }
}

/// Where the agent stands among its terminal's jobs, from its `(pgid, tpgid)`,
/// its parent's `pgid`, and whether its parent opened its terminal. A
/// job-control shell starts each job in its own process group and hands it
/// the terminal; a script, `sh -c`, `make`, an IDE task or a shell without job
/// control runs its child in its own group. The restart waits for the
/// parent's `pgid == tpgid` ("the shell took the terminal back"), which for a
/// parent sharing the agent's group already holds while the agent runs, so
/// the relaunch line would go to whatever the launcher runs next, or nowhere.
/// An aterm tab's own program (`aterm -e claude`, `exec claude` in a tab)
/// leads its terminal's session and group as a shell's job does, but its
/// parent is the aterm that opened the pty ([`terminal_owner`],
/// [`owned_by_aterm`]): no shell is between them to take the terminal back,
/// and the tab closes as the agent ends. The own program of a pty another
/// program opened (`tmux new claude`, `script claude`) is read as that
/// program's job, and the terminal-owner check holds it back
/// (`held-back:terminal:<owner>`): what the tab types reaches that program.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Job {
    /// A job of a job-control shell, and the terminal's foreground now.
    Foreground,
    /// A job, but not the terminal's foreground right now (suspended, or put in
    /// the background): what is typed would reach the shell, not the agent.
    Background,
    /// AN ATERM TAB'S OWN PROGRAM: no shell's job, so never restarted or
    /// relaunched (refused for good, `not-a-shell-job`), but what the tab
    /// types reaches it while it leads its terminal's `foreground`
    /// ([`leads_terminal`]: the release an older build's notice owes it).
    TabProgram {
        /// Its group is its terminal's foreground group.
        foreground: bool,
    },
    /// Not a job of a job-control shell, or of no shell at all: nothing will
    /// take the terminal back when it ends. Refused for good, before anything
    /// is typed.
    NoJobControl,
}

/// [`Job`] from the numbers `ps` reports, and whether the agent's parent is
/// the aterm that opened its terminal (`aterm_opened`).
fn job(agent: u32, agent_pgid: i64, tpgid: i64, parent_pgid: i64, aterm_opened: bool) -> Job {
    if agent_pgid != i64::from(agent) {
        Job::NoJobControl
    } else if aterm_opened {
        Job::TabProgram {
            foreground: tpgid == agent_pgid,
        }
    } else if parent_pgid == agent_pgid {
        Job::NoJobControl
    } else if tpgid != agent_pgid {
        Job::Background
    } else {
        Job::Foreground
    }
}

/// [`job`] read through `k` now, with the parent it was read against: the
/// agent's and its parent's ids ([`Kernel::ids`]) and whether that parent is
/// an aterm that opened the agent's terminal ([`Kernel::opener`],
/// [`owned_by_aterm`]). An opener that cannot be read decides nothing here;
/// every act asks for the terminal's owner again itself, and waits on one
/// that is not the tab's.
fn job_of<K: Kernel + ?Sized>(k: &K, agent: u32) -> Option<(Job, u32)> {
    let (parent, pgid, tpgid) = k.ids(agent)?;
    let (_, parent_pgid, _) = k.ids(parent)?;
    let opened = k
        .opener(agent, parent)
        .is_some_and(|name| owned_by_aterm((parent, name.as_str())));
    Some((job(agent, pgid, tpgid, parent_pgid, opened), parent))
}

/// What one [`Job`] read lets through: `Ok(shell)` when the agent is its
/// shell's foreground job (`shell` the parent read against), else the wait
/// word. Asked before every announce and SIGTERM, so a ctrl-z between looks is
/// caught.
pub(super) fn foreground_shell(job: Option<(Job, u32)>) -> Result<u32, &'static str> {
    match job {
        Some((Job::Foreground, shell)) => Ok(shell),
        Some((Job::Background, _)) => Err("not-foreground"),
        Some((Job::NoJobControl | Job::TabProgram { .. }, _)) => Err("not-a-shell-job"),
        None => Err("ids"),
    }
}

/// What one [`Job`] read lets be TYPED to the agent with no restart after it
/// (the release, [`release`]): the agent leads its terminal's foreground —
/// its shell's foreground job, or an aterm tab's own program in front — so
/// the line reaches it; else the wait word.
pub(super) fn leads_terminal(job: Option<(Job, u32)>) -> Result<(), &'static str> {
    match job {
        Some((Job::Foreground | Job::TabProgram { foreground: true }, _)) => Ok(()),
        Some((Job::Background | Job::TabProgram { foreground: false }, _)) => Err("not-foreground"),
        Some((Job::NoJobControl, _)) => Err("not-a-shell-job"),
        None => Err("ids"),
    }
}

/// The [`Job`] of a read that refuses the upgrade for good before anything
/// is typed or signalled ([`not_a_job`]): no job of a job-control shell, or
/// an aterm tab's own program. `None` for a shell's job, in front or not,
/// and for no read.
pub(super) fn not_a_shell_job(job: Option<(Job, u32)>) -> Option<Job> {
    match job {
        Some((job @ (Job::NoJobControl | Job::TabProgram { .. }), _)) => Some(job),
        Some((Job::Foreground | Job::Background, _)) | None => None,
    }
}

/// The process facts every act rests on, each read from the kernel when asked.
/// The job is read again before the announce, the SIGTERM and the last look,
/// since the agent can be suspended or backgrounded in between. A trait so a
/// test can script what each read answers and prove that each re-read guards
/// its act.
pub(super) trait Kernel {
    /// Where the agent stands among its terminal's jobs, and its parent. The
    /// default reads it through [`Kernel::ids`] and [`Kernel::opener`]
    /// ([`job_of`]), as [`Live`] does.
    fn job(&self, agent: u32) -> Option<(Job, u32)> {
        job_of(self, agent)
    }
    /// `(ppid, pgid, tpgid)` of `pid` ([`ids`]), `None` when it cannot be read
    /// now. The default reads the kernel, as [`Live`] does.
    fn ids(&self, pid: u32) -> Option<(u32, i64, i64)> {
        ids(pid)
    }
    /// `pid`'s parent, `None` when it cannot be read now: the process is gone,
    /// or `ps` could not be run. Never a verdict about the process.
    fn parent(&self, pid: u32) -> Option<u32>;
    /// The program that opened `pid`'s terminal ([`terminal_owner`]).
    fn terminal(&self, pid: u32) -> Option<(u32, String)>;
    /// `parent`'s basename where `parent` opened `agent`'s terminal (the
    /// owner [`Kernel::terminal`] names is `parent`); `None` where it did
    /// not, or that cannot be read now. The default asks
    /// [`Kernel::terminal`]; [`Live`] reads the two processes alone
    /// ([`opener`]), never the whole table.
    fn opener(&self, agent: u32, parent: u32) -> Option<String> {
        self.terminal(agent)
            .filter(|(owner, _)| *owner == parent)
            .map(|(_, name)| name)
    }
    /// The process's kernel start, read again at each restart fence. `None` is
    /// unknown and never proves the process is still the same one.
    fn process_start(&self, pid: u32) -> Option<String> {
        kernel_start(pid)
    }
    /// Work still running under the process at the restart's last look. An
    /// unreadable table counts as work, as in [`live_background`].
    fn restart_background(&self, pid: u32) -> Vec<String> {
        live_background(pid)
    }
    /// The executable the kernel mapped for `pid` ([`exe_of`]): with its argv,
    /// the build the process runs ([`launched`]), which the move's line back
    /// resumes it on ([`St::back_line`]). `None` where it cannot be read.
    fn image(&self, pid: u32) -> Option<PathBuf> {
        exe_of(pid)
    }
    /// Whether `shell` has its terminal back (a job-control shell leading its
    /// own process group, which is the terminal's foreground); `None` when it
    /// cannot be read now. The default reads it through [`Kernel::ids`], as
    /// [`Live`] does.
    fn shell_holds_tab(&self, shell: u32) -> Option<bool> {
        if shell == 0 {
            return None;
        }
        let (_, pgid, tpgid) = self.ids(shell)?;
        Some(pgid == i64::from(shell) && pgid == tpgid)
    }
    /// The processes of `procs` (`(pid, basename)` under `agent`,
    /// [`background_procs`]) named for the notice ([`upgrade::running_clause`]).
    /// The default uses the table alone (no age, no command); [`Live`] reads
    /// both from `ps`.
    fn describe(&self, agent: u32, procs: &[(u32, String)]) -> Vec<upgrade::Held> {
        let _ = agent;
        procs
            .iter()
            .map(|(pid, name)| upgrade::Held {
                pid: *pid,
                name: name.clone(),
                age_s: 0,
                command: String::new(),
            })
            .collect()
    }
}

/// The kernel itself: its process table ([`ids`], [`live_terminal`], [`opener`]:
/// `ps` off macOS) and `kill(pid, 0)`.
pub(super) struct Live;

impl Kernel for Live {
    fn parent(&self, pid: u32) -> Option<u32> {
        if !alive(pid) {
            return None;
        }
        ids(pid).map(|(ppid, _, _)| ppid)
    }

    fn terminal(&self, pid: u32) -> Option<(u32, String)> {
        live_terminal(pid)
    }

    fn opener(&self, agent: u32, parent: u32) -> Option<String> {
        opener(agent, parent)
    }

    fn describe(&self, agent: u32, procs: &[(u32, String)]) -> Vec<upgrade::Held> {
        held_of(agent, procs)
    }
}

/// Shells and the keep-awake helper: work running under the agent (a Bash
/// tool, background shell, build, a command's `caffeinate`). Not MCP servers
/// (a resume restarts them) nor the agent's own keep-awake ([`OWN_KEEP_AWAKE`]).
const BACKGROUND: &[&str] = &[
    "sh",
    "bash",
    "zsh",
    "fish",
    "dash",
    "ksh",
    "tcsh",
    "caffeinate",
];

/// The agent's own keep-awake is not work: Claude Code starts `caffeinate -i
/// -t 300` as its own child every turn and stops it some 30 s after the turn
/// ends. It holds nothing a restart could lose (the relaunched agent starts
/// its own), and `-t` ends it anyway. Only as the agent's direct child: a
/// `caffeinate` under a Bash tool's shell or any other helper stays work.
const OWN_KEEP_AWAKE: &str = "caffeinate";

/// The background work under `pid`, by basename: every [`BACKGROUND`] program
/// at any depth, except the agent's own keep-awake ([`OWN_KEEP_AWAKE`], its
/// direct child), whose descendants are still walked. The one reading of
/// "something runs under the agent" for every restart (the live upgrade's
/// gate and last look, the model-only restart, a Codex embedded session).
pub(super) fn background(pid: u32, t: &[ProcRow]) -> Vec<String> {
    background_procs(pid, t)
        .into_iter()
        .map(|(_, name)| name)
        .collect()
}

/// [`background`] with each process's pid: `(pid, basename)`, in the order
/// the walk met them.
pub(super) fn background_procs(pid: u32, t: &[ProcRow]) -> Vec<(u32, String)> {
    let mut frontier = vec![pid];
    let mut found = Vec::new();
    let mut seen = 0;
    while let Some(p) = frontier.pop() {
        seen += 1;
        if seen > 4096 {
            break;
        }
        for (c, pp, name) in t {
            if *pp == p {
                frontier.push(*c);
                let own_keep_awake = p == pid && name == OWN_KEEP_AWAKE;
                if BACKGROUND.contains(&name.as_str()) && !own_keep_awake {
                    found.push((*c, name.to_string()));
                }
            }
        }
    }
    found
}

/// The word [`live_background`] answers with when the process table cannot be
/// read: one entry of work, so a gate that asks "does anything run?" waits.
pub(super) const UNREADABLE_TABLE: &str = "ps-unreadable";

/// [`background`] over the live process table, failing closed: a real table
/// always holds this process, so an empty one (`ps` failed) reads as
/// [`UNREADABLE_TABLE`] (something may run) rather than "nothing runs".
pub(super) fn live_background(pid: u32) -> Vec<String> {
    background_or_unreadable(pid, &table())
}

/// [`live_background`] over a table already read: an empty one reads as
/// [`UNREADABLE_TABLE`].
pub(super) fn background_or_unreadable(pid: u32, t: &[ProcRow]) -> Vec<String> {
    if t.is_empty() {
        return vec![UNREADABLE_TABLE.to_string()];
    }
    background(pid, t)
}

/// What runs under the agent, named for the notice
/// ([`upgrade::running_clause`], [`Kernel::describe`]): each of `procs` with
/// its age, plus its command when its parent is `agent` (the agent's own
/// Bash-tool shell, [`upgrade::Held::command`]), as `ps -o
/// pid=,ppid=,etime=,args=` gives them in the C locale. Processes gone since
/// the walk are left out; an unreadable read names nothing.
pub(super) fn held_of(agent: u32, procs: &[(u32, String)]) -> Vec<upgrade::Held> {
    if procs.is_empty() {
        return Vec::new();
    }
    #[cfg(target_os = "macos")]
    {
        use aterm_sysprobe::kproc;
        let now = aterm_time::unix_secs().unwrap_or(0);
        procs
            .iter()
            .filter_map(|(p, name)| {
                let r = kproc::record(*p)?;
                Some(upgrade::Held {
                    pid: *p,
                    name: name.clone(),
                    age_s: now.saturating_sub(r.start_s),
                    command: if r.ppid == agent {
                        command_head(&squash(&kproc::ps_args(&r, kproc::Locale::C)))
                    } else {
                        String::new()
                    },
                })
            })
            .collect()
    }
    #[cfg(not(target_os = "macos"))]
    {
        ps_held_of(agent, procs)
    }
}

/// [`held_of`] from one `ps -o pid=,ppid=,etime=,args= -p <procs>` read.
#[cfg(any(not(target_os = "macos"), test))]
fn ps_held_of(agent: u32, procs: &[(u32, String)]) -> Vec<upgrade::Held> {
    let list = procs
        .iter()
        .map(|(p, _)| p.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let Some(out) = ps(&["-o", "pid=,ppid=,etime=,args=", "-p", &list]) else {
        return Vec::new();
    };
    procs
        .iter()
        .filter_map(|(p, name)| {
            let line = out
                .lines()
                .find(|l| l.split_whitespace().next() == Some(p.to_string().as_str()))?;
            let mut it = line.split_whitespace();
            let _ = it.next();
            let ppid: u32 = it.next()?.parse().ok()?;
            let age_s = parse_etime(it.next()?)?;
            let args = it.collect::<Vec<_>>().join(" ");
            Some(upgrade::Held {
                pid: *p,
                name: name.clone(),
                age_s,
                command: if ppid == agent {
                    command_head(&args)
                } else {
                    String::new()
                },
            })
        })
        .collect()
}

/// The give-up's ledger row, for the owner: why it stopped asking, what it owes
/// the agent now ([`upgrade::GAVE_UP`]: the release, and a late READY still
/// honoured before it), when the next round asks ([`upgrade::RETRY_S`]), and
/// what still ran under the agent ([`upgrade::held_list`]). The release is owed
/// here, not typed: the `released:<why>` row that follows says it was. The
/// Codex lane types no release and hears no READY once it stopped. Under the
/// quiet flow ([`upgrade::Facts::quiet`]) a round gives up only when an older
/// aterm (or `upgrade_ask`) announced it and the agent's own work held the
/// restart past the notice's window ([`upgrade::quiet_step`]); the next round
/// moves the session at its next idle point, asking nothing.
pub(super) fn gave_up_words(agent: upgrade::Agent, held: &[upgrade::Held], quiet: bool) -> String {
    let span = upgrade::span(upgrade::RETRY_S);
    let why = match (agent, quiet) {
        (upgrade::Agent::Claude, false) => format!(
            "no READY answer it could act on after the last notice: this round stops asking, \
             owes the agent the line that releases it (typed wherever a notice may go, a break \
             of its own work included), and still honours a READY that comes before that line; \
             a new round asks again in {span}"
        ),
        (upgrade::Agent::Claude, true) => format!(
            "the agent's own work held the restart past the window of the notice an earlier \
             build typed, and the quiet upgrade asks nothing again: this round stops, owes the \
             agent the line that releases it (typed wherever a notice may go, a break of its \
             own work included), and still honours a READY that comes before that line; in \
             {span} a new round moves the session at its next idle point, asking nothing"
        ),
        (upgrade::Agent::Codex, false) => format!(
            "no READY answer it could act on after the last notice: this round stops asking; a \
             new round asks again in {span}"
        ),
        (upgrade::Agent::Codex, true) => format!(
            "the agent's own work held the move past the window of the notice an earlier build \
             typed, and the quiet upgrade asks nothing again: this round stops; in {span} a new \
             round moves the session at its next idle point, asking nothing"
        ),
        // Muse Code: its live upgrade is the quiet flow alone, which asks
        // nothing and so never gives up: no round of its own exists to stop.
        (upgrade::Agent::Muse, _) => {
            "Muse Code's live upgrade asks nothing (its quiet flow alone): it is restarted onto \
             the new build at its next genuinely idle point, and nothing is typed into it"
                .to_string()
        }
        // agy: its live upgrade is the quiet flow alone, which asks nothing
        // and so never gives up: no round of its own exists to stop.
        (upgrade::Agent::Agy, _) => {
            "agy's live upgrade asks nothing (its quiet flow alone): it is restarted onto the new \
             build at its next genuinely idle point, and nothing is typed into it"
                .to_string()
        }
        // Gemini CLI: relaunched on exit only, never upgraded, so no round
        // of its own exists to give up.
        (upgrade::Agent::Gemini, _) => {
            "no upgrade lane moves Gemini CLI (it is only relaunched on exit): nothing is asked \
             of it or typed into it for an upgrade"
                .to_string()
        }
    };
    let list = upgrade::held_list(held);
    if list.is_empty() {
        why
    } else {
        format!("{why}; still running under the agent: {list}")
    }
}

/// `ps`'s `etime` (`[[dd-]hh:]mm:ss`) in seconds.
#[cfg(any(not(target_os = "macos"), test))]
pub(super) fn parse_etime(etime: &str) -> Option<u64> {
    let (days, clock) = match etime.split_once('-') {
        Some((d, rest)) => (d.parse::<u64>().ok()?, rest),
        None => (0, etime),
    };
    let parts = clock
        .split(':')
        .map(|p| p.parse::<u64>().ok())
        .collect::<Option<Vec<_>>>()?;
    let (h, m, sec) = match parts.as_slice() {
        [m, sec] => (0, *m, *sec),
        [h, m, sec] => (*h, *m, *sec),
        _ => return None,
    };
    Some(days * 86_400 + h * 3_600 + m * 60 + sec)
}

/// The part of a background shell's command line that says what it does.
/// Claude Code's Bash tool runs every command as `zsh -c source <snapshot> …
/// && eval '<command>' < /dev/null && pwd -P >| <cwd file>`, whose opening is
/// the same boilerplate every time, so this takes the `eval` payload with the
/// shell's `'"'"'` quoting undone and `ps`'s `\012` escapes read as spaces.
/// Any other command line is its own head.
pub(super) fn command_head(args: &str) -> String {
    let args = args.replace("\\012", " ");
    let Some((_, payload)) = args.split_once("&& eval '") else {
        return args;
    };
    let payload = payload
        .rsplit_once("' < /dev/null")
        .map_or(payload, |(head, _)| head);
    payload.replace("'\"'\"'", "'")
}

/// Whether `pid` is this process or one of its ancestors: ending it would end
/// the sweep that is driving it.
pub(super) fn is_our_ancestor(pid: u32, t: &[ProcRow]) -> bool {
    let mut p = std::process::id();
    for _ in 0..64 {
        if p == pid {
            return true;
        }
        match t.iter().find(|(c, _, _)| *c == p) {
            Some((_, pp, _)) if *pp != p && *pp != 0 => p = *pp,
            _ => return false,
        }
    }
    false
}

/// The kernel's working directory of `pid`.
pub(super) fn cwd_of(pid: u32) -> Option<String> {
    aterm_sysprobe::process_cwd(pid).map(|p| p.to_string_lossy().into_owned())
}

/// The executable the kernel mapped for `pid` (its text), canonical.
pub(super) fn exe_of(pid: u32) -> Option<PathBuf> {
    aterm_sysprobe::process_path(pid)
}

/// Every file `pid` holds open through a descriptor, `None` when unreadable or exited: the
/// lanes' read of a process's own locks (codex-alab's writer lock, a Muse Code's session lock,
/// an agy's presence lock and crash file). Read in process ([`aterm_uds::holders::open_files`]),
/// else by [`lsof_open_files`] where that is unsupported or a descriptor is refused (a file in a
/// protected folder, which the kernel describes only to an entitled reader such as `lsof`).
pub(super) fn open_files(pid: u32) -> Option<Vec<PathBuf>> {
    match aterm_uds::holders::open_files(pid) {
        Ok(files) => Some(files),
        // Exited: it holds nothing now, which `lsof` answers as nothing read.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => lsof_open_files(pid),
    }
}

/// Every file `pid` holds open (`lsof -p`), `None` when unreadable: [`open_files`]'s fallback.
pub(super) fn lsof_open_files(pid: u32) -> Option<Vec<PathBuf>> {
    let mut cmd = aterm_uds::spawnfd::command("lsof");
    cmd.args(["-n", "-P", "-w", "-p", &pid.to_string(), "-Fn"]);
    let out = aterm_uds::deadline::output_until(&mut cmd, Instant::now() + Duration::from_secs(10))
        .ok()?;
    // lsof exits 1 for a pid it found nothing for: an unreadable answer.
    if !out.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.strip_prefix('n').map(PathBuf::from))
            .collect(),
    )
}

/// Positive holders do not make an incomplete sweep complete. Only a fully
/// readable scan can establish that this list contains every holder.
fn complete_file_holders(scan: aterm_uds::holders::HolderScan) -> Option<Vec<u32>> {
    scan.unreadable.is_empty().then_some(scan.holders)
}

/// Every process holding `file` open through a descriptor, `None` when
/// unreadable: the lanes' read of who holds a lock (codex-alab's writer lock, a
/// Muse Code's session lock, an agy's presence lock) or an agy conversation's
/// database before a line resumes it. On macOS the holder scan reads it in this
/// process ([`aterm_uds::holders::scan_file_holders`]: 9-22 ms where `lsof -t`
/// took 0.2-0.3 s of a loaded Mac). A scan that could not read some processes'
/// tables (a descriptor on a file in a protected folder is refused, `EPERM`)
/// asks `lsof` about those processes alone ([`lsof_holders_among`]) and adds the
/// holders it names. MEASURED on the owner's Mac, not a rule: some agent of this
/// uid holds such a file, so every scan there is incomplete (7 of 513
/// processes, 69 descriptors refused), and `lsof`, which carries the
/// `com.apple.rootless.datavault.metadata` entitlement, read the device and
/// inode of all 69. A process `lsof` lists nothing for while it lives, or a
/// regular file's descriptor it names no device or inode of (one that closed as
/// it was read excepted), leaves the answer unknown. Elsewhere [`lsof_holders`] reads it. The two name the same holders
/// of a file held through a descriptor (`file_holders_are_lsof_s`); `lsof -t`
/// alone also names a process whose working directory or a mapping is the
/// file, which none of these files is to its holder.
pub(super) fn file_holders(file: &Path) -> Option<Vec<u32>> {
    holder_census(
        aterm_uds::holders::scan_file_holders(file, &[]),
        |pids| lsof_holders_among(file, pids),
        || lsof_holders(file),
    )
}

/// [`file_holders`]' answer from the holder scan's `scan`: a complete scan's
/// holders; an incomplete one's with those `among` names of the processes it
/// could not read (unknown where `among` is); `whole` (`lsof` over every
/// process) where the scan is unsupported. PURE but for the two `lsof` reads.
fn holder_census(
    scan: std::io::Result<aterm_uds::holders::HolderScan>,
    among: impl FnOnce(&[u32]) -> Option<Vec<u32>>,
    whole: impl FnOnce() -> Option<Vec<u32>>,
) -> Option<Vec<u32>> {
    match scan {
        Ok(scan) if scan.unreadable.is_empty() => complete_file_holders(scan),
        Ok(scan) => {
            let mut holders = scan.holders;
            holders.extend(among(&scan.unreadable)?);
            holders.sort_unstable();
            holders.dedup();
            Some(holders)
        }
        Err(e) if e.kind() == std::io::ErrorKind::Unsupported => whole(),
        Err(_) => None,
    }
}

/// Which of `pids` hold `file` open through a descriptor, read from `lsof`'s
/// listing of their descriptors (`-F 0pftDin`) by the file's device and inode
/// ([`lsof_fd_holders`]), `None` when that is unknown.
#[cfg(unix)]
fn lsof_holders_among(file: &Path, pids: &[u32]) -> Option<Vec<u32>> {
    use std::os::unix::fs::MetadataExt as _;
    let meta = std::fs::metadata(file).ok()?;
    let list: Vec<String> = pids.iter().map(u32::to_string).collect();
    let mut cmd = aterm_uds::spawnfd::command("lsof");
    cmd.args(["-n", "-P", "-p", &list.join(","), "-F", "0pftDin"]);
    let out = aterm_uds::deadline::output_until(&mut cmd, Instant::now() + Duration::from_secs(10))
        .ok()?;
    // `st_dev` is Darwin's 32-bit `dev_t`, which `MetadataExt` widens with its sign.
    let key = (meta.dev() & u64::from(u32::MAX), meta.ino());
    lsof_fd_holders(&out, pids, key, aterm_uds::process::pid_alive)
}

/// Off Unix no scan is incomplete (it is unsupported): unknown.
#[cfg(not(unix))]
fn lsof_holders_among(_file: &Path, _pids: &[u32]) -> Option<Vec<u32>> {
    None
}

/// What `lsof` names a descriptor that closed between its listing of the
/// process's table and its read of the descriptor (MEASURED: a process opening
/// and closing a file in a loop, read 200 times, showed 755 such descriptors,
/// no type, device or inode). The holder scan holds such a descriptor to hold
/// nothing, and so does [`lsof_fd_holders`]. Unix, as the reader is.
#[cfg(unix)]
const LSOF_FD_CLOSED: &[u8] = b"vnode: FD unavailable";

/// The holders among `pids` of the file whose device and inode are `key`, in
/// one completed `lsof -F 0pftDin` listing of their descriptors (each field
/// ended by a NUL, a name's newline escaped): each process with a numbered
/// descriptor on that device and inode. `None` where the listing cannot say:
/// `lsof` failed or wrote to stderr (it exits 1 whenever some pid it was given
/// has no file, which is not a failure here), a field does not parse, it lists
/// a process it was not asked about or one twice, a pid it lists nothing for is
/// `alive`, or a regular file's descriptor (or one of no reported type) has no
/// device or inode and did not close as it was read ([`LSOF_FD_CLOSED`]).
/// `cwd`, `txt` and the like are no descriptors, as the holder scan reads none.
#[cfg(unix)]
fn lsof_fd_holders(
    out: &std::process::Output,
    pids: &[u32],
    key: (u64, u64),
    alive: impl Fn(u32) -> bool,
) -> Option<Vec<u32>> {
    /// One descriptor's fields, judged once the next set begins.
    #[derive(Default)]
    struct Fd<'a> {
        numbered: bool,
        kind: Option<&'a str>,
        dev: Option<u64>,
        ino: Option<u64>,
        closed: bool,
    }
    /// The descriptor `fd` of the process listed last, judged: its process
    /// joins `holders` where it is the file; `None` where it may be, unread.
    fn settle(
        fd: Option<Fd<'_>>,
        listed: &[u32],
        key: (u64, u64),
        holders: &mut Vec<u32>,
    ) -> Option<()> {
        let (Some(fd), Some(&pid)) = (fd, listed.last()) else {
            return Some(());
        };
        let held = match (fd.dev, fd.ino) {
            _ if !fd.numbered => false,
            (Some(dev), Some(ino)) => (dev, ino) == key,
            _ if fd.closed => false,
            _ if matches!(fd.kind, None | Some("REG")) => return None,
            _ => false,
        };
        if held && holders.last() != Some(&pid) {
            holders.push(pid);
        }
        Some(())
    }
    if !out.stderr.is_empty() || !matches!(out.status.code(), Some(0 | 1)) {
        return None;
    }
    let mut listed: Vec<u32> = Vec::new();
    let mut holders: Vec<u32> = Vec::new();
    let mut fd: Option<Fd<'_>> = None;
    for field in out.stdout.split(|b| *b == 0) {
        let field = field.strip_prefix(b"\n").unwrap_or(field);
        let Some((&tag, value)) = field.split_first() else {
            continue;
        };
        let text = || std::str::from_utf8(value).ok();
        match tag {
            b'p' => {
                settle(fd.take(), &listed, key, &mut holders)?;
                let pid: u32 = text()?.parse().ok()?;
                if !pids.contains(&pid) || listed.contains(&pid) {
                    return None;
                }
                listed.push(pid);
            }
            b'f' => {
                settle(fd.take(), &listed, key, &mut holders)?;
                listed.last()?;
                fd = Some(Fd {
                    numbered: text()?.parse::<u32>().is_ok(),
                    ..Fd::default()
                });
            }
            b't' => fd.as_mut()?.kind = Some(text()?),
            b'D' => {
                let hex = text()?.strip_prefix("0x")?;
                fd.as_mut()?.dev = Some(u64::from_str_radix(hex, 16).ok()?);
            }
            b'i' => fd.as_mut()?.ino = Some(text()?.parse().ok()?),
            b'n' => fd.as_mut()?.closed = value == LSOF_FD_CLOSED,
            _ => {}
        }
    }
    settle(fd, &listed, key, &mut holders)?;
    if pids.iter().any(|p| !listed.contains(p) && alive(*p)) {
        return None;
    }
    Some(holders)
}

/// Every process holding `file` open (`lsof -t`), `None` when unreadable:
/// [`file_holders`] where the holder scan is unsupported, and its oracle.
pub(super) fn lsof_holders(file: &Path) -> Option<Vec<u32>> {
    let mut cmd = aterm_uds::spawnfd::command("lsof");
    cmd.args(["-n", "-P", "-t"]).arg(file);
    let out = aterm_uds::deadline::output_until(&mut cmd, Instant::now() + Duration::from_secs(10))
        .ok()?;
    lsof_holder_pids(&out)
}

/// The readable holder census in one completed `lsof -t` reply.
fn lsof_holder_pids(out: &std::process::Output) -> Option<Vec<u32>> {
    if !out.stderr.is_empty() {
        return None;
    }
    match out.status.code()? {
        0 => std::str::from_utf8(&out.stdout)
            .ok()?
            .split_whitespace()
            .map(str::parse)
            .collect::<Result<Vec<u32>, _>>()
            .ok(),
        // Only lsof's ordinary, empty no-match answer proves no holders.
        1 if out.stdout.is_empty() => Some(Vec::new()),
        _ => None,
    }
}

// ---------------------------------------------------------------- the relaunch lanes' shared reads

// The reads the relaunch-on-exit lanes past Claude Code's share — Muse Code's
// (`upgrade_muse_drive.rs`) and agy's (`upgrade_agy_drive.rs`), each of which
// reads an exit off the shell's word: the tab's command blocks
// (`blocks --json`), a process's start, the shell's dialect, and the version
// atpkg's record names for a store build and its twin.

/// The blocks of one `blocks --json` reply.
pub(super) fn blocks_of(head: &str, body: &str) -> Option<Vec<Value>> {
    if !crate::is_ok_reply(head) {
        return None;
    }
    let json = if body.trim().is_empty() {
        head.strip_prefix("OK ")?.to_string()
    } else {
        body.to_string()
    };
    let v: Value = aterm_json::from_str(json.trim()).ok()?;
    Some(v.get("blocks")?.as_array()?.clone())
}

/// The block a TUI runs in, read while it runs: the tab's last block,
/// executing (`MuseRun::block`, `AgyRun::block`).
pub(super) fn running_block(c: &mut Client, tab: &str) -> Option<u64> {
    let (head, body) = c.request_counted(&format!("@{tab} blocks 1 --json")).ok()?;
    let blocks = blocks_of(&head, &body)?;
    let last = blocks.last()?;
    (last.get("state").and_then(Value::as_str) == Some("executing"))
        .then(|| last.get("id").and_then(Value::as_u64))
        .flatten()
}

/// The exit status of the command that just ran in `tab`, per shell
/// integration: the finished block ending where the entering prompt begins
/// (`blocks 2 --json`) — block `block` where the TUI's snapshot named the one
/// it ran in ([`running_block`]), so another command run since is never read
/// as its exit. `None` while there is none.
pub(super) fn exit_status(c: &mut Client, tab: &str, block: Option<u64>) -> Option<i64> {
    let (head, body) = c.request_counted(&format!("@{tab} blocks 2 --json")).ok()?;
    parse_exit_status(&head, &body, block)
}

/// [`exit_status`] over one `blocks --json` reply (also a task's `exited`
/// mark's read, [`shell_exit_status`]).
pub(crate) fn parse_exit_status(head: &str, body: &str, block: Option<u64>) -> Option<i64> {
    let blocks = blocks_of(head, body)?;
    let (last, rest) = blocks.split_last()?;
    if last.get("state").and_then(Value::as_str) != Some("entering") {
        return None;
    }
    let prompt = last.get("prompt").and_then(Value::as_u64)?;
    let done = rest.last()?;
    (done.get("state").and_then(Value::as_str) == Some("complete")
        && done.get("end").and_then(Value::as_u64) == Some(prompt)
        && block.is_none_or(|block| done.get("id").and_then(Value::as_u64) == Some(block)))
    .then(|| done.get("exit").and_then(Value::as_i64))
    .flatten()
}

/// How many of the tab's newest command blocks a look at a relaunch line's
/// own block reads ([`line_status`]): the line's, the prompt after it, and a
/// few commands a person ran since.
pub(super) const LINE_BLOCKS: u32 = 8;

/// THE EXIT STATUS OF COMMAND BLOCK `block` in one `blocks --json` reply:
/// `None` while it runs, or where the reply does not hold it complete (no
/// shell integration, a block scrolled past the window, blocks reset).
pub(super) fn block_exit(head: &str, body: &str, block: u64) -> Option<i64> {
    blocks_of(head, body)?
        .iter()
        .find(|b| b.get("id").and_then(Value::as_u64) == Some(block))
        .filter(|b| b.get("state").and_then(Value::as_str) == Some("complete"))?
        .get("exit")
        .and_then(Value::as_i64)
}

/// THE EXIT STATUS OF A RELAUNCH LINE'S OWN COMMAND (its block `block`,
/// `St::line_block`, read right before the line was typed), once it ended.
/// `None` while that command runs, where no block was read before it was
/// typed (no shell integration), or where the tab no longer shows it. Each
/// lane reads it by its own table.
pub(super) fn line_status(c: &mut Client, tab: &str, block: Option<u64>) -> Option<i64> {
    let block = block?;
    let (head, body) = c
        .request_counted(&format!("@{tab} blocks {LINE_BLOCKS} --json"))
        .ok()?;
    block_exit(&head, &body, block)
}

/// When `pid` started, unix milliseconds (to the second: `ps`'s elapsed
/// time, now's second less its start's), while it runs
/// (`MuseRun::started_ms`, `AgyRun::started_ms`).
pub(super) fn started_ms(pid: u32) -> Option<u64> {
    #[cfg(target_os = "macos")]
    let age_s = {
        let r = aterm_sysprobe::kproc::record(pid)?;
        aterm_time::unix_secs()?.saturating_sub(r.start_s)
    };
    #[cfg(not(target_os = "macos"))]
    let age_s = parse_etime(ps(&["-o", "etime=", "-p", &pid.to_string()])?.trim())?;
    let now_ms = u64::try_from(aterm_time::since_epoch()?.as_millis()).ok()?;
    Some(now_ms.saturating_sub(age_s.saturating_mul(1000)))
}

/// The dialect of the shell `shell` from its executable (`args`, its exec
/// path, else the process table's name), as a relaunch line is written for
/// it; `None` for a shell no line is written for (`sh`, tcsh, nushell…),
/// which a lane's plan waits on for ever.
pub(super) fn shell_dialect_of(
    shell: u32,
    args: Option<atpkg::caller_shell::ProcArgs>,
) -> Option<upgrade::Dialect> {
    args.map(|a| a.exec_path)
        .or_else(|| {
            table()
                .into_iter()
                .find(|(p, _, _)| *p == shell)
                .map(|(_, _, n)| n.to_string())
        })
        .as_deref()
        .map(base_name)
        .as_deref()
        .and_then(upgrade::Dialect::from_exe_name)
}

/// The version a store build names in atpkg's record beside it
/// (`<store>/<program>/<build>.vendor`: `version = "…"`); `None` for a
/// program run from no store, or no record. Never `--version`, which may
/// take an agent's own locks (Muse Code's auth lock, measured) and runs the
/// vendor's code.
pub(super) fn store_version(program: &Path) -> Option<String> {
    let build = program.parent()?.parent()?;
    let record = build.with_extension("vendor");
    let text = std::fs::read_to_string(record).ok()?;
    text.lines().find_map(|l| {
        let v = l.trim().strip_prefix("version")?.trim_start();
        let v = v.strip_prefix('=')?.trim().trim_matches('"');
        (!v.is_empty() && !v.chars().any(char::is_control)).then(|| v.to_string())
    })
}

/// `twin` where its script names a build of `program` that exists (an atpkg
/// twin: `exec '<store build>' "$@"`, whatever it exports before).
pub(super) fn twin_naming(twin: &Path, program: &str) -> Option<PathBuf> {
    let exe = catalog::twin_binary(&std::fs::read_to_string(twin).ok()?)?;
    (exe.file_name().and_then(|n| n.to_str()) == Some(program) && exe.is_file())
        .then(|| twin.to_path_buf())
}

/// atpkg's `agents/<program>` twin, where it names a build of `program`
/// ([`twin_naming`]), in the configured prefix's `agents/` ([`agents_dir`]).
pub(super) fn managed_agent_twin(program: &str) -> Option<PathBuf> {
    twin_naming(&agents_dir()?.join(program), program)
}

/// atpkg's `agents/` directory, where its twins are: the configured
/// prefix's ([`atpkg::store::resolve_configured`]).
#[cfg(not(test))]
fn agents_dir() -> Option<PathBuf> {
    Some(atpkg::store::resolve_configured()?.agents_dir())
}

#[cfg(test)]
thread_local! {
    /// The atpkg `agents/` directory a test's twin lookups read (none unless
    /// the test names one): the machine's own prefix is never a test's.
    pub(super) static AGENTS_DIR: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// [`agents_dir`] in a test: the one it named on this thread.
#[cfg(test)]
fn agents_dir() -> Option<PathBuf> {
    AGENTS_DIR.with(|dir| dir.borrow().clone())
}

/// The version the twin's build names ([`store_version`]), for a record's
/// `to`.
pub(super) fn twin_version(twin: &Path) -> Option<String> {
    let exe = catalog::twin_binary(&std::fs::read_to_string(twin).ok()?)?;
    store_version(&exe)
}

/// How long `<exe> --version` may take before it is killed and read as no
/// version. A healthy build answers in well under a second; the bound guards
/// against one that never does (a quarantined download at a first-run dialog,
/// a wrapper waiting on a lock), which would otherwise hold `sweep.lock` and
/// every Claude upgrade on the machine.
///
/// A test waits the one-minute hang detector instead: the build it probes is a
/// stand-in script it just wrote, and on a loaded host that exec waits on the
/// system, not on the script (macOS assesses each new executable's first exec,
/// one scan at a time machine-wide: a fresh script took 5.4 s behind 40 queued
/// scans, against 0.01 s for one already run), so a five-second bound read the
/// queue and failed the probe of a build that starts. A test that wants the
/// bound's own answer names one ([`version_within`], `build_start::probe_within`).
#[cfg(not(test))]
const VERSION_WAIT: Duration = Duration::from_secs(5);
#[cfg(test)]
const VERSION_WAIT: Duration = aterm_spec::wait::MUST_HAPPEN;

/// What one `<exe> --version` probe found ([`version_within`]).
#[derive(Clone, Debug, PartialEq, Eq)]
enum Probed {
    /// It answered, naming this version.
    Version(Version),
    /// It finished within the bound and named no version (failed, could not
    /// run, or printed something else): a property of the file, remembered
    /// while it is unchanged ([`cached_version`]).
    NoVersion,
    /// It did not finish within the bound (the child, or something holding
    /// its output, still ran) and its process group was killed: as much about
    /// the moment as the file, so it is asked again on a back-off
    /// ([`VERSION_RETRY_FIRST`]), never written off.
    TimedOut,
}

/// What `<exe> --version` says ([`Probed`]): its leading version, no version
/// (a child that fails or prints none), or no answer within [`VERSION_WAIT`].
fn version_of(exe: &Path) -> Probed {
    version_within(exe, VERSION_WAIT)
}

/// [`version_of`] with an explicit bound covering the child's output and
/// exit. The leading version token is read from at most the first 64 KiB;
/// this is a prefix budget, not a complete-output claim. The output is
/// collected before any wait can reap the child, so every group signal still
/// has its unreaped child's PID reserved. On Unix the shared nonblocking
/// reader observes the same deadline even if a writer escaped the group.
/// On other hosts a blocking reader may outlive the probe until its pipe
/// closes; only the direct child is killed there. The final blocking reap
/// still relies on OS completion; escaped descendants are not terminated.
fn version_within(exe: &Path, limit: Duration) -> Probed {
    let deadline = Instant::now() + limit;
    // `spawnfd::command`, as for `ps`: no console window from the harness host.
    let mut cmd = aterm_uds::spawnfd::command(exe);
    cmd.arg("--version")
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .stderr(aterm_uds::spawnfd::Stdio::null());
    #[cfg(unix)]
    cmd.process_group(0);
    let Ok(mut child) = cmd.spawn() else {
        return Probed::NoVersion;
    };
    #[cfg(unix)]
    let group = libc::pid_t::try_from(child.id()).ok();
    let kill_group = || {
        #[cfg(unix)]
        if let Some(group) = group {
            // SAFETY: `process_group(0)` made this child's PID its private
            // PGID. Every call below precedes a successful reap, so this
            // unreaped Child still reserves the numeric PID.
            unsafe { libc::kill(-group, libc::SIGKILL) };
        }
    };
    let Some(stdout) = child.stdout.take() else {
        kill_group();
        let _ = child.kill();
        let _ = child.wait();
        return Probed::NoVersion;
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let _reader = std::thread::Builder::new()
        .name("agent-version-probe".into())
        .spawn(move || {
            #[cfg(unix)]
            let read = super::align::read_capture_pipe(stdout, 64 * 1024, deadline);
            #[cfg(not(unix))]
            let read = {
                let mut buf = Vec::new();
                stdout.take(64 * 1024).read_to_end(&mut buf).map(|_| buf)
            };
            let _ = tx.send(read);
        });
    // Keep the Child unreaped until the pipe has answered. A child that
    // exited while a descendant holds stdout still reserves its PID here.
    let out = match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(Ok(out)) => out,
        Ok(Err(error)) => {
            kill_group();
            let _ = child.kill();
            let _ = child.wait();
            return if error.kind() == std::io::ErrorKind::TimedOut {
                Probed::TimedOut
            } else {
                Probed::NoVersion
            };
        }
        Err(_) => {
            // Includes reader startup failure or panic: its sender is gone.
            kill_group();
            let _ = child.kill();
            let _ = child.wait();
            return Probed::TimedOut;
        }
    };
    let succeeded = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                kill_group();
                let _ = child.kill();
                let _ = child.wait();
                return Probed::TimedOut;
            }
        }
    };
    if !succeeded {
        return Probed::NoVersion;
    }
    let text = String::from_utf8_lossy(&out);
    text.split_whitespace()
        .next()
        .and_then(Version::parse)
        .map_or(Probed::NoVersion, Probed::Version)
}

/// Complete UTF-8 stdout from a successful child within `limit` and `cap`.
/// Missing, unreadable, oversized or unfinished evidence is unknown. The shared
/// capture owns the process group, deadline and pipe reader.
pub(super) fn bounded_output(
    program: &Path,
    args: &[&std::ffi::OsStr],
    limit: Duration,
    cap: u64,
) -> Option<String> {
    let cap = usize::try_from(cap).ok()?;
    let mut cmd = aterm_uds::spawnfd::command(program);
    cmd.args(args);
    let out = super::align::capture_bounded(cmd, limit, cap.checked_add(1)?).ok()?;
    if !out.success() || out.stdout.len() > cap {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

// ---------------------------------------------------------------- targets

/// Where Claude's native installer keeps its builds.
pub(super) fn native_root(home: &Path) -> PathBuf {
    home.join(".local/share/claude/versions")
}

/// atpkg's twin of `program` (`<prefix>/agents/<program>`), the managed build
/// a session may be moved onto, unless `program` is dev-linked (`aterm pkg
/// link`) and its twin names the developer's checkout rather than a store
/// build: no session is moved onto, or ordered against, a checkout's build. An
/// unlinked program's twin is the managed one, whatever it holds.
fn managed_twin(layout: &atpkg::store::Layout, program: &str) -> Option<PathBuf> {
    let twin = layout.agents_dir().join(program);
    if !atpkg::linkmode::is_linked(layout, program) {
        return Some(twin);
    }
    let exe = super::upgrade_catalog::twin_binary(&std::fs::read_to_string(&twin).ok()?)?;
    exe.starts_with(layout.prefix.join("store")).then_some(twin)
}

/// The builds a session could move to, read once per sweep.
pub(super) struct Targets {
    /// aterm's managed twin (`<prefix>/agents/claude`).
    managed: Option<Candidate>,
    /// The native install's current build (what `~/.local/bin/claude` names).
    native: Option<Candidate>,
    /// A build whose `--version` did not answer in time is no candidate yet
    /// ([`Asked::Unanswered`]): what it is stays unknown until the version
    /// cache asks again, and a look that found nothing else to take has read
    /// nothing whole ([`UNREAD_VERSION`]).
    unanswered: bool,
}

impl Targets {
    pub(super) fn read(home: &Path) -> Targets {
        Targets::read_with(
            atpkg::store::resolve_configured().and_then(|layout| managed_twin(&layout, "claude")),
            home,
            cached_version,
        )
    }

    /// [`Targets::read`] of the managed `twin` (`None`: no store, or a
    /// dev-linked checkout's, [`managed_twin`]), each build's version asked of
    /// `ask` ([`cached_version`] in the product).
    fn read_with(twin: Option<PathBuf>, home: &Path, ask: impl Fn(&Path) -> Asked) -> Targets {
        let unanswered = std::cell::Cell::new(false);
        let version = |exe: &Path| match ask(exe) {
            Asked::Answered(v) => v,
            Asked::Unanswered => {
                unanswered.set(true);
                None
            }
        };
        let managed = twin.and_then(|twin| {
            let version = version(&twin)?;
            Some(Candidate {
                exe: twin,
                version,
                source: Source::Managed,
            })
        });
        let native = std::fs::canonicalize(home.join(".local/bin/claude"))
            .ok()
            .filter(|target| target.starts_with(native_root(home)))
            .and_then(|target| {
                // The native installer names each build's file by its version.
                let version = target
                    .file_name()
                    .and_then(|n| Version::parse(&n.to_string_lossy()))
                    .or_else(|| version(&target))?;
                Some(Candidate {
                    exe: target,
                    version,
                    source: Source::Native,
                })
            });
        Targets {
            managed,
            native,
            unanswered: unanswered.get(),
        }
    }

    /// What this session could move to: the managed twin always, the native
    /// build only for a session that runs native (a managed session is never
    /// moved onto a vendor-updated copy).
    pub(super) fn for_session(&self, running_native: bool) -> Vec<Candidate> {
        self.managed
            .iter()
            .chain(self.native.iter().filter(|_| running_native))
            .cloned()
            .collect()
    }
}

/// How long after a timed-out probe ([`Probed::TimedOut`]) the same file is
/// probed again, doubling per consecutive timeout up to
/// [`VERSION_RETRY_MAX`]. A timeout is no verdict on the build, but neither
/// should it be re-run every minute while holding `sweep.lock` for
/// [`VERSION_WAIT`]. Until then [`due`] reads the build as unanswered
/// ([`UNREAD_VERSION`]) and looks in between cost no probe.
const VERSION_RETRY_FIRST: Duration = Duration::from_secs(2 * 60);
const VERSION_RETRY_MAX: Duration = Duration::from_secs(60 * 60);

/// What [`cached_version`] remembers of one file as it was (size, mtime).
#[derive(Clone, Debug)]
enum Seen {
    /// The probe finished: its answer, until the file changes.
    Answered(Option<Version>),
    /// It timed out `streak` times in a row; not asked again before `retry`.
    TimedOut { streak: u32, retry: Instant },
}

/// What [`cached_version`] can say of a build.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Asked {
    /// Its probe finished: the version it named, or none.
    Answered(Option<Version>),
    /// Its probe did not answer within [`VERSION_WAIT`], this time or the
    /// last: what it is stays unknown until the back-off lets it be asked
    /// again ([`VERSION_RETRY_FIRST`]).
    Unanswered,
}

/// [`version_of`], asked again only when the file's size or mtime moved (the
/// twin is re-rendered whenever its build changes, and the host asks at every
/// worker start and activation notice). A version or clean failure is
/// remembered while the file is unchanged; a timeout is asked again on a
/// back-off ([`VERSION_RETRY_FIRST`]).
fn cached_version(exe: &Path) -> Asked {
    cached_version_with(exe, Instant::now(), version_of)
}

/// [`cached_version`] at `now`, over the `probe` it caches.
fn cached_version_with(exe: &Path, now: Instant, probe: impl FnOnce(&Path) -> Probed) -> Asked {
    type Cache = std::collections::HashMap<PathBuf, (u64, SystemTime, Seen)>;
    static CACHE: std::sync::Mutex<Option<Cache>> = std::sync::Mutex::new(None);
    let Some(meta) = std::fs::metadata(exe)
        .ok()
        .filter(std::fs::Metadata::is_file)
    else {
        return Asked::Answered(None);
    };
    let Ok(modified) = meta.modified() else {
        return Asked::Answered(None);
    };
    let stamp = (meta.len(), modified);
    let before = CACHE.lock().ok().and_then(|cache| {
        cache
            .as_ref()
            .and_then(|c| c.get(exe))
            .filter(|(len, at, _)| (*len, *at) == stamp)
            .map(|(_, _, seen)| seen.clone())
    });
    let streak = match before {
        Some(Seen::Answered(v)) => return Asked::Answered(v),
        Some(Seen::TimedOut { retry, .. }) if now < retry => return Asked::Unanswered,
        Some(Seen::TimedOut { streak, .. }) => streak,
        None => 0,
    };
    let (seen, asked) = match probe(exe) {
        Probed::Version(v) => (Seen::Answered(Some(v.clone())), Asked::Answered(Some(v))),
        Probed::NoVersion => (Seen::Answered(None), Asked::Answered(None)),
        Probed::TimedOut => {
            let wait = VERSION_RETRY_FIRST
                .saturating_mul(1 << streak.min(16))
                .min(VERSION_RETRY_MAX);
            let retry = now.checked_add(wait).unwrap_or(now);
            (
                Seen::TimedOut {
                    streak: streak.saturating_add(1),
                    retry,
                },
                Asked::Unanswered,
            )
        }
    };
    if let Ok(mut cache) = CACHE.lock() {
        cache
            .get_or_insert_with(Cache::new)
            .insert(exe.to_path_buf(), (stamp.0, stamp.1, seen));
    }
    asked
}

// ---------------------------------------------------------------- the tab

pub(super) type Client = RelayClient<aterm_uds::CtlStream>;

/// The control connection for `sid`'s instance ([`connect_at`]): read-only
/// for a dry run.
pub(super) fn connect(opts: &Opts, sid: &str) -> Result<Client, String> {
    let sock = match &opts.sock {
        Some(s) => s.clone(),
        None => aterm_ctl::resolve_sock_for(Some(sid)).map_err(|e| format!("{e}"))?,
    };
    connect_at(opts, &sock)
}

/// THE ONE WAY THE PASS CONNECTS to an instance, at `sock` (its token beside
/// it). A DRY RUN's connection is read-only ([`RelayClient::reads_only`]): it
/// sends the read verbs alone, so a dry run — the CLI's `--dry-run` and the
/// window's watch ([`watch`]) — cannot type, press, paste, lease or signal
/// through it whatever path it takes.
pub(super) fn connect_at(opts: &Opts, sock: &str) -> Result<Client, String> {
    let c = RelayClient::connect_local_beside(sock).map_err(|e| format!("{sock}: {e}"))?;
    Ok(if opts.dry_run { c.reads_only() } else { c })
}

/// A connection that reads one instance's roster (`who`, `sessions`) at
/// `sock`: read-only whatever the pass is, since a roster read needs no hand.
pub(super) fn roster_connection(sock: &str) -> Option<Client> {
    RelayClient::connect_local_beside(sock)
        .ok()
        .map(RelayClient::reads_only)
}

/// [`connect`], read-only whatever `opts` says: the watch's own reads
/// ([`watch`]), which run off any point and must have no hands.
pub(super) fn connect_reads(opts: &Opts, sid: &str) -> Result<Client, String> {
    connect(
        &Opts {
            dry_run: true,
            ..opts.clone()
        },
        sid,
    )
}

/// The instance's live tab ids.
pub(super) fn roster(c: &mut Client) -> Vec<String> {
    let Ok((head, body)) = c.request_counted("sessions") else {
        return Vec::new();
    };
    if !crate::is_ok_reply(&head) {
        return Vec::new();
    }
    body.lines()
        .filter_map(|l| {
            l.split_whitespace()
                .find(|t| t.starts_with("s-"))
                .map(str::to_owned)
        })
        .collect()
}

/// The tabs set aside from a window's host by a hand it does not hold, from
/// one `sessions` read ([`Aside`]). Another supervisor's: each row whose live
/// claim `supervisor=<holder|->` names a holder other than a window's own host
/// ([`HOST_HOLDER`]; a predecessor's carried claim lapses by itself). The
/// owner's: each row whose agent its own switch holds off (its `program=` one
/// of `agents_off`, `[harness.<agent>] enabled = false`), and each row whose
/// effective Autopilot is Off (`autopilot=off`); a switched-off agent's row
/// reads both, and the switch ranks first ([`Aside::agent_off`]). For
/// `--status` ([`status_rows`]), which reads from outside the window. Bounded
/// by [`CLAIMS_WAIT`]; `None` for no roster.
pub(super) fn roster_aside(c: &mut Client, agents_off: &[aterm_phase::Program]) -> Option<Aside> {
    roster_aside_within(c, agents_off, CLAIMS_WAIT)
}

/// [`roster_aside`], an instance that has not answered within `wait` given up
/// on (`None`).
fn roster_aside_within(
    c: &mut Client,
    agents_off: &[aterm_phase::Program],
    wait: Duration,
) -> Option<Aside> {
    c.transport().set_read_timeout(Some(wait)).ok()?;
    let (head, body) = c.request_counted("sessions").ok()?;
    if !crate::is_ok_reply(&head) {
        return None;
    }
    let mut aside = Aside::default();
    for row in body.lines() {
        let mut words = row.split_whitespace();
        let Some(sid) = words.nth(1).filter(|s| s.starts_with("s-")) else {
            continue;
        };
        // Past the parent, the state and the title, every column is
        // `key=value`: a title never reads as a field. Each key's first wins.
        let (mut holder, mut autopilot, mut program) = (None, None, None);
        for word in words.skip(3) {
            if let Some(h) = word.strip_prefix("supervisor=") {
                holder = holder.or(Some(h));
            } else if let Some(a) = word.strip_prefix("autopilot=") {
                autopilot = autopilot.or(Some(a));
            } else if let Some(p) = word.strip_prefix("program=") {
                program = program.or(Some(p));
            }
        }
        if program
            .and_then(aterm_phase::program_of)
            .is_some_and(|p| agents_off.contains(&p))
        {
            aside.agent_off.insert(sid.to_string());
        }
        if holder.is_some_and(|h| h != "-" && !is_host_holder(&aterm_codec::pct::decode(h))) {
            aside.claimed.insert(sid.to_string());
        }
        if autopilot == Some("off") {
            aside.off.insert(sid.to_string());
        }
    }
    Some(aside)
}

/// The aside of every instance `clients` reach, as one ([`roster_aside`] of
/// each; tab ids are unique across instances): an instance that answers no
/// roster adds nothing and holds no other back. `--status`'s view of a machine
/// running several aterms.
pub(super) fn aside_of(
    clients: impl IntoIterator<Item = Client>,
    agents_off: &[aterm_phase::Program],
) -> Aside {
    clients
        .into_iter()
        .filter_map(|mut c| roster_aside(&mut c, agents_off))
        .fold(Aside::default(), |mut all, one| {
            all.claimed.extend(one.claimed);
            all.off.extend(one.off);
            all.agent_off.extend(one.agent_off);
            all
        })
}

/// How long [`roster_aside`] waits for the roster: the 2 s a discovery
/// client gives a peer (`aterm ctl ls`), four times the instance's own
/// bound on the one hop its `sessions` reply takes.
const CLAIMS_WAIT: Duration = Duration::from_secs(2);

/// `who` reads the owner-only registry without the `sessions`
/// window-placement hop. Each PTY master's foreground process group is a
/// kernel-backed tab claim, available even when macOS returns argv without
/// another process's environment. Both the upgrade drivers and the
/// supervisor's worker binding (`supervise`'s `Session::worker_env`) read it
/// through [`roster_rows`] and judge a group by [`unique_tab_for_group`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LiveTab {
    pub(crate) sid: String,
    pub(crate) fgpgid: Option<i64>,
}

pub(super) fn host_roster(c: &mut Client) -> Option<Vec<LiveTab>> {
    let (head, body) = c.request_counted("who").ok()?;
    parse_host_roster(&head, &body)
}

fn parse_host_roster(head: &str, body: &str) -> Option<Vec<LiveTab>> {
    if !crate::is_ok_reply(head) {
        return None;
    }
    Some(roster_rows(body)?.into_iter().map(|(_, tab)| tab).collect())
}

/// A `who` reply's rows (`<local> <s-id> … fgpgid=<group|->`), each with the
/// instance-local id it leads with. `None` — no roster at all — for a row
/// that is not that shape, and for an `s-…` id two rows claim.
pub(crate) fn roster_rows(body: &str) -> Option<Vec<(u64, LiveTab)>> {
    let mut rows: Vec<(u64, LiveTab)> = Vec::new();
    for row in body.lines() {
        let mut words = row.split_whitespace();
        let local = words.next()?.parse::<u64>().ok()?;
        let sid = words.next()?.to_string();
        if !sid.starts_with("s-") || rows.iter().any(|(_, t)| t.sid == sid) {
            return None;
        }
        let fgpgid = row
            .split_whitespace()
            .find_map(|word| word.strip_prefix("fgpgid="))
            .and_then(|word| word.parse::<i64>().ok())
            .filter(|&group| group > 0);
        rows.push((local, LiveTab { sid, fgpgid }));
    }
    Some(rows)
}

fn tab_is_live(tabs: &[LiveTab], sid: &str) -> bool {
    tabs.iter().any(|tab| tab.sid == sid)
}

/// A duplicated foreground group is ambiguity, never permission to choose
/// whichever tab happened to be listed first.
pub(crate) fn unique_tab_for_group(tabs: &[LiveTab], group: i64) -> Option<&str> {
    let mut found = None;
    for tab in tabs.iter().filter(|tab| tab.fgpgid == Some(group)) {
        if found.is_some() {
            return None;
        }
        found = Some(tab.sid.as_str());
    }
    found
}

/// Recheck the unique PTY claim before a delayed typed or signalled step.
pub(super) fn process_in_tab(
    c: &mut Client,
    pid: u32,
    tab: &str,
    expected_group: Option<i64>,
    expected_parent: Option<u32>,
) -> bool {
    let Some(tabs) = host_roster(c) else {
        return false;
    };
    let Some(group) = process_group(pid) else {
        return false;
    };
    expected_group.is_none_or(|expected| expected == group)
        && unique_tab_for_group(&tabs, group) == Some(tab)
        && ids(pid).is_some_and(|(parent, pgid, tpgid)| {
            expected_parent.is_none_or(|expected| expected == parent)
                && pgid == group
                && tpgid == group
        })
}

/// The tab's visible rows, the cursor within them, and its content sequence.
pub(super) struct Screen {
    pub(super) rows: Vec<String>,
    pub(super) cursor: Option<(usize, usize)>,
    pub(super) seq: u64,
    /// The screen row `rows[0]` is: the coordinates the `cell` verb takes.
    pub(super) first: usize,
    /// The screen generation (`gen`, `<epoch>.<seq>`) the read was taken at:
    /// what a fenced `turn if-gen=` names ([`turn_fenced`]). `None` from a
    /// host that sends none.
    pub(super) generation: Option<String>,
    /// When a person last gave the session input, as the server stamped it
    /// at the read (`"human_ms"`): the one presence fact
    /// ([`upgrade::attended_by`], [`upgrade::Facts::attended`]).
    pub(super) human: HumanInput,
}

pub(super) fn screen(c: &mut Client, sid: &str) -> Option<Screen> {
    screen_query(c, &format!("@{sid} text --json"))
}

/// Claude's composer is pinned near the bottom. The continuation wait needs
/// only that slice while a draft is present; a positive result is confirmed
/// against the whole screen before it can authorize a turn.
const CONTINUATION_TAIL_ROWS: usize = 20;

fn screen_tail(c: &mut Client, sid: &str) -> Option<Screen> {
    screen_query(
        c,
        &format!("@{sid} text --json tail={CONTINUATION_TAIL_ROWS}"),
    )
}

fn screen_query(c: &mut Client, request: &str) -> Option<Screen> {
    let (head, body) = c.request_counted(request).ok()?;
    if !crate::is_ok_reply(&head) {
        return None;
    }
    parse_screen(if body.trim().is_empty() {
        head.strip_prefix("OK ")?
    } else {
        &body
    })
}

/// One `text --json` object: its rows, the cursor made relative to them (the
/// cursor row is absolute; `first` is the row the reply starts at), and `seq`.
fn parse_screen(json: &str) -> Option<Screen> {
    let v: Value = aterm_json::from_str(json.trim()).ok()?;
    let rows: Vec<String> = v
        .get("rows")?
        .as_array()?
        .iter()
        .map(|r| r.as_str().unwrap_or("").to_string())
        .collect();
    let first = v.get("first").and_then(Value::as_u64).unwrap_or(0);
    let cursor = v.get("cursor").and_then(|cur| {
        let row = cur.get("row")?.as_u64()?.checked_sub(first)?;
        let col = cur.get("col")?.as_u64()?;
        Some((usize::try_from(row).ok()?, usize::try_from(col).ok()?))
    });
    let seq = v.get("seq").and_then(Value::as_u64).unwrap_or(0);
    let generation = v
        .get("gen")
        .and_then(Value::as_str)
        .filter(|g| is_generation(g))
        .map(str::to_owned);
    // Absent (an older host) is unknown; `null` is never — for how long,
    // where the host says (`"unkeyed_ms"`); a number is its age; anything
    // else is unknown, as the supervisor reads it.
    let human = match v.get("human_ms") {
        None => HumanInput::Unknown,
        Some(Value::Null) => HumanInput::never(v.get("unkeyed_ms").and_then(Value::as_u64)),
        Some(h) => h.as_u64().map_or(HumanInput::Unknown, HumanInput::Ago),
    };
    Some(Screen {
        rows,
        cursor,
        seq,
        first: usize::try_from(first).ok()?,
        generation,
        human,
    })
}

/// Whether `g` is a screen generation the server accepts as the `if-gen=`
/// fence, by the server's own parser ([`ScreenGen::parse`], shared through
/// `aterm_types::control_verbs`), never a local copy of its shape. Anything
/// else is never put on a fenced request line.
fn is_generation(g: &str) -> bool {
    ScreenGen::parse(g).is_some()
}

/// [`upgrade::composer_is_empty`] over `scr`, with the one fact its rows cannot
/// carry: whether the text at the caret row's column 2 is drawn dim (Claude's
/// placeholder suggestion) or not (a typed draft whose caret was moved home).
/// Read with the `cell` verb where [`upgrade::composer_dim_row`] says a read
/// can change the answer ([`dim_cell`]). A reply that is not an `OK` naming
/// `dim` reads not dim: a suggestion then waits as a draft would, and nothing
/// is typed over a draft.
pub(super) fn composer_empty(c: &mut Client, sid: &str, scr: &Screen) -> bool {
    let dim = dim_placeholder(c, sid, scr);
    upgrade::composer_is_empty(&scr.rows, scr.cursor, dim)
}

/// Whether the composer's placeholder cell ([`dim_cell`]) reads dim, asked of
/// the host (`cell <row> 2`): what [`composer_empty`] reads a suggestion by.
pub(super) fn dim_placeholder(c: &mut Client, sid: &str, scr: &Screen) -> bool {
    dim_cell(scr).is_some_and(|at| {
        c.request_line(&format!("@{sid} cell {at} 2"))
            .is_ok_and(|line| cell_is_dim(&line))
    })
}

/// The idle continuation can wait minutes with a person's draft in the
/// composer, so these repeated polls read only the bottom rows. A missing
/// cursor or caret makes the slice inconclusive, and an unreadable tail (an
/// older host) falls back to the full screen. A positive tail never suffices
/// to type: the full screen must independently show an empty composer.
pub(super) fn continuation_composer_empty(
    c: &mut Client,
    sid: &str,
    tail_available: &mut bool,
) -> bool {
    if *tail_available {
        match screen_tail(c, sid) {
            Some(scr)
                if scr.cursor.is_some_and(|(row, _)| row < scr.rows.len())
                    && aterm_phase::phase::composer_index(&scr.rows).is_some() =>
            {
                if !composer_empty(c, sid, &scr) {
                    return false;
                }
            }
            Some(_) => {}
            None => *tail_available = false,
        }
    }
    screen(c, sid).is_some_and(|scr| composer_empty(c, sid, &scr))
}

use crate::supervise::screen::cell_is_dim;

/// What stands of text the lane typed and did not send, read in Claude Code's
/// prompt ([`left_standing`]). Never taken back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Left {
    /// It is no longer there: the person cleared it, or submitted it.
    Gone,
    /// Still there, and why: alone and unsent (`unsent`), among a person's
    /// typing (`mixed`, theirs now), or hidden (a box, no composer, no read).
    Kept(&'static str),
}

/// `s` with all whitespace dropped, to compare a composer's text across its
/// wrapping (which may split a word) and its rows' indentation.
pub(super) fn compact(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// What stands of what the Claude lane left typed: a notice or release the
/// host stopped part-way ([`Typed::Cut`]: deadline, screen switch, a person's
/// gesture) or typed whole with its guarded Enter unpressed (`guard`). Read
/// only: aterm never takes its words back, since a fenced `ctrl+u` cannot see
/// a person's key the host counted but Claude Code has not drawn, and could
/// erase it (or, after `ctrl-z`, hand the key to the shell). The text is
/// recorded ([`St::left_typed`]) so no look reads it as a person's draft and
/// the supervisor never sends it (`upgrade::own_draft`); the owner is told,
/// and a person sends or clears it.
///
/// aterm's part alone in the prompt (compared from the text's start with
/// whitespace dropped, [`compact`]) is `unsent`; with a person's keys among
/// it, `mixed`; a prompt holding anything else, or nothing, means `Gone`. A
/// box or no drawn prompt keeps the record as is.
pub(super) fn left_standing(c: &mut Client, tab: &str, text: &str) -> Left {
    let ours = compact(text);
    let mark = compact(upgrade::HARNESS_MARK);
    let Some(scr) = screen(c, tab) else {
        return Left::Kept("screen-unreadable");
    };
    if aterm_phase::prompt::prompt_box_span(&scr.rows).is_some() {
        return Left::Kept("box");
    }
    let draft = aterm_phase::phase::composer_rules(&scr.rows)
        .and_then(|_| aterm_phase::phase::composer_draft(&scr.rows));
    let Some((_, lines)) = draft else {
        return Left::Kept("no-composer");
    };
    if composer_empty(c, tab, &scr) {
        return Left::Gone;
    }
    let draft = compact(&lines.concat());
    if ours.starts_with(&draft) {
        Left::Kept("unsent")
    } else if draft.starts_with(&mark) {
        Left::Kept("mixed")
    } else {
        Left::Gone
    }
}

/// A pressed text the host stopped ([`Typed::Cut`]): aterm's words, part or
/// all (`guard`) of `text`, stand in Claude Code's prompt. Recorded
/// ([`St::left_typed`]) so later looks read them first ([`left_first`]) and
/// never as a person's draft. Never taken back ([`left_standing`]); a person
/// sends or clears them.
fn left_cut(st: &mut St, c: &mut Client, tab: &str, text: &str) {
    st.left_typed = text.to_string();
    let _ = settle_left(st, c, tab, left_standing);
}

/// One read of what stands of [`St::left_typed`] by the lane's `standing`
/// (Claude Code's prompt, [`left_standing`], or Codex's composer; each presses
/// nothing): the record goes once the text is gone, sent or cleared by a
/// person.
fn settle_left(
    st: &mut St,
    c: &mut Client,
    tab: &str,
    standing: fn(&mut Client, &str, &str) -> Left,
) -> Left {
    let left = standing(c, tab, &st.left_typed);
    if left == Left::Gone {
        st.left_typed.clear();
    }
    left
}

/// Aterm's own words left in the prompt ([`St::left_typed`]) come first at a
/// look: read, never pressed ([`settle_left`]); while they stand the look
/// waits `wait:left-typed:<why>`, never `draft`. `None` when nothing of
/// aterm's is recorded or it is gone. A dry run reads them the same way.
fn left_first(
    opts: &Opts,
    r: &Report,
    st: &mut St,
    c: &mut Client,
    tab: &str,
    sf: &SessionFile,
    now: u64,
) -> Option<Report> {
    if st.left_typed.is_empty() {
        return None;
    }
    let r = match settle_left(st, c, tab, left_standing) {
        Left::Gone => return None,
        Left::Kept(why) => said(r.clone(), format!("wait:left-typed:{why}")),
    };
    st.note_step(&r.step, &sf.status, now);
    save(opts, &sf.session_id, st);
    Some(r)
}

/// Whether someone other than this sweep holds the tab: a halt, a hand on it
/// (a turn, lease or driver other than this process's [`Hand`]), unread input
/// (`input=`: a first prompt a signal would drop), or a person who typed
/// within `grace_s` seconds. `0` asks only the hold, hands and input (the
/// gates read the person as [`upgrade::Facts::attended`], which `--now`
/// waives).
pub(super) fn held(c: &mut Client, sid: &str, grace_s: u32) -> bool {
    status_line(c, sid).is_none_or(|line| held_by_status(&line, grace_s))
}

/// The tab's `status` line, or `None` when it cannot be read.
pub(super) fn status_line(c: &mut Client, sid: &str) -> Option<String> {
    c.request_line(&format!("@{sid} status")).ok()
}

/// What [`step`] answers for a tab whose agent supervises itself under a
/// proven self-claim ([`self_claimed_by_status`]): aterm takes no upgrade
/// decision there (the native harness contract §7).
pub const SKIP_SELF_CLAIMED: &str = "skip:self-claimed";

/// Whether one `status` reply shows a proven self-claim on the tab
/// (`supervisor_proven=1`): its agent supervises itself, and aterm takes no
/// upgrade decision there ([`due`], [`step`]). An older server prints no such
/// field and so shows none.
#[must_use]
pub(super) fn self_claimed_by_status(line: &str) -> bool {
    crate::is_ok_reply(line) && line.split_whitespace().any(|t| t == "supervisor_proven=1")
}

/// [`held`] over one `status` reply; anything but `OK` reads as held. Its halt
/// and hands are [`super::relaunch::hand_on_tab`]'s, the one list the host
/// carries an asked exit through. A `human_ms=` under the grace is a person at
/// the keyboard, who wins; `-` or no field (an older server) is nobody.
pub(super) fn held_by_status(line: &str, grace_s: u32) -> bool {
    !crate::is_ok_reply(line)
        || super::relaunch::hand_on_tab(line)
        || line.split_whitespace().any(|t| {
            t.strip_prefix("input=")
                .is_some_and(|input| !matches!(input, "-" | "clear"))
        })
        || super::relaunch::person_present(line, grace_s)
}

/// The harness's hand on a tab whose agent it restarts. From the signal to the
/// relaunched agent's first idle the tab is a bare shell and then a booting
/// agent, and an orchestrator's prompt sent then would run as a shell command
/// line or die with the old process. So a restart takes the server's hard
/// drive lease (holder [`hand_name`], `lease acquire … hard`) before its last
/// look and holds it, renewed through every wait ([`Hand::keep`]), to the
/// agent's first idle (`relaunch::first_idle`); every other connection's write
/// is refused `ERR busy lease=…`.
///
/// A person's keyboard is never blocked: a keystroke before the signal stops
/// the restart ([`held`], the signal's `quiet=`, [`terminate`]); after it, a
/// person at the returned prompt holds the relaunch line. A step that dies
/// holding the hand leaves it to lapse; an aterm without hard leases holds
/// nothing, and the restart relies on its last looks and fences.
pub(super) const HAND_TTL_MS: u64 = 60_000;

/// How often a wait under the hand renews it ([`Hand::keep`]): well inside
/// [`HAND_TTL_MS`], so no relaunch wait (exit 30 s, shell prompt 15 s, new
/// process 90 s, first idle 15 s, a codex-alab's first idle 60 s) outlasts it.
const HAND_RENEW: Duration = Duration::from_secs(20);

/// The holder name of this process's hand ([`Hand`]): the name its supervisor
/// loops claim sessions under, spelled the same as the window's host
/// (`aterm-gui`'s `harness_host::holder`), by whose [`HOST_HOLDER`] `--status`
/// tells a window's own claim from another supervisor's ([`roster_aside`]).
#[must_use]
pub fn hand_name() -> String {
    format!("{HOST_HOLDER}{}", std::process::id())
}

/// What every window's host's holder name begins with ([`hand_name`]).
const HOST_HOLDER: &str = "aterm-harness@";

/// The request that takes (or renews) this process's hand on a tab, without
/// its `@<tab>` selector: what [`Hand::take`] sends. A renewal from another
/// connection moves the hand there.
#[must_use]
pub fn hand_take_request() -> String {
    format!(
        "lease acquire ttl={HAND_TTL_MS} holder={} hard",
        hand_name()
    )
}

/// The request that gives this process's hand back ([`hand_take_request`]'s
/// pair, [`Hand::give_back`]).
#[must_use]
pub fn hand_give_back_request() -> String {
    format!("lease release holder={}", hand_name())
}

/// [`hand_take_request`] kept to the asking connection (`lease acquire …
/// here`): what the turn-end loop types its texts under (`supervise`'s
/// `Session::type_under_hand`). Every lane of this process takes its hand
/// under the one [`hand_name`], so a plain acquire would move another
/// lane's live hand — a relaunch's or an upgrade's [`Hand`], another
/// loop's — to the loop's connection; this one is refused instead, and
/// renews only a hand the loop's own connection already holds (one an
/// earlier give-back failed to return). The hand it takes is that
/// connection's: the server frees it when the connection ends, so a stop
/// that hangs the loop's connection up gives it back.
#[must_use]
pub fn hand_take_here_request() -> String {
    hand_take_here_request_as(&hand_name())
}

/// [`hand_take_here_request`] under the hand name `holder`: what a loop
/// whose setup holds its holder takes the hand under (`supervise`'s
/// `Session::type_under_hand`). The window's host names its loops by its
/// hand name (`aterm-gui`'s `harness_host::holder`), so live it is the very
/// request; a replay of that loop's tape, in a process of its own, asks it
/// as the recorded run did rather than under its own pid.
#[must_use]
pub fn hand_take_here_request_as(holder: &str) -> String {
    format!("lease acquire ttl={HAND_TTL_MS} holder={holder} hard here")
}

/// [`hand_give_back_request`] kept to the asking connection (`lease release
/// … here`, [`hand_take_here_request`]'s pair): a hand another lane holds
/// under the same name, over another connection, is never freed by it.
#[must_use]
pub fn hand_give_back_here_request() -> String {
    hand_give_back_here_request_as(&hand_name())
}

/// [`hand_give_back_here_request`] under the hand name `holder`
/// ([`hand_take_here_request_as`]'s pair).
#[must_use]
pub fn hand_give_back_here_request_as(holder: &str) -> String {
    format!("lease release holder={holder} here")
}

/// Whether a claim's holder (percent-decoded) is a window's own host —
/// this one's or a predecessor's, whose carried claim names a process that
/// is gone and lapses by itself ([`HOST_HOLDER`]) — and so no other
/// supervisor's. The one spelling `--status` ([`roster_aside`]) and the
/// window's host (`aterm-gui`'s `harness_host::claimed_by_another`) read a
/// `supervisor=` by.
#[must_use]
pub fn is_host_holder(holder: &str) -> bool {
    holder.starts_with(HOST_HOLDER)
}

/// Whether one `status` `hand=` value is this process's own [`Hand`]: no
/// one else's hand on the tab.
#[must_use]
pub(super) fn our_hand(hand: &str) -> bool {
    hand.strip_prefix("lease:") == Some(hand_name().as_str())
}

/// This process's hand on one tab (see [`HAND_TTL_MS`]).
#[derive(Debug)]
pub(super) struct Hand {
    tab: String,
    /// The server holds it for this process (it took no lease, or it
    /// lapsed and another driver took the tab: not held).
    held: bool,
    renewed: Instant,
}

impl Hand {
    /// Takes (or renews) this process's hand on `tab`: `None` when another
    /// driver holds it (another holder's live lease, or a live `turn`), which
    /// a restart waits out. From a server without hard leases, a hand that
    /// holds nothing.
    pub(super) fn take(c: &mut Client, tab: &str) -> Option<Hand> {
        let reply = c.request_line(&format!("@{tab} {}", hand_take_request()));
        let held = match reply {
            Ok(reply) if reply.starts_with("OK lease acquired") => true,
            Ok(reply) if reply.starts_with("ERR busy") || reply.starts_with("ERR lease held") => {
                return None;
            }
            _ => false,
        };
        Some(Hand {
            tab: tab.to_string(),
            held,
            renewed: Instant::now(),
        })
    }

    /// [`Self::take`] for a relaunch already under way (the agent ended):
    /// another driver's hold is no reason to leave a bare shell, so the hand
    /// is simply not held and the line relies on its own fences.
    pub(super) fn take_or_none(c: &mut Client, tab: &str) -> Hand {
        Hand::take(c, tab).unwrap_or_else(|| Hand {
            tab: tab.to_string(),
            held: false,
            renewed: Instant::now(),
        })
    }

    /// Renew the hand once [`HAND_RENEW`] has passed since it was last taken:
    /// called through every wait under it.
    pub(super) fn keep(&mut self, c: &mut Client) {
        if self.held && self.renewed.elapsed() >= HAND_RENEW {
            self.held = Hand::take(c, &self.tab).is_some_and(|h| h.held);
            self.renewed = Instant::now();
        }
    }

    /// Gives the hand back, never anyone else's (the server matches the
    /// holder and never releases a `turn`). Best effort: otherwise it lapses
    /// ([`HAND_TTL_MS`]).
    pub(super) fn give_back(&mut self, c: &mut Client) {
        if std::mem::take(&mut self.held) {
            let _ = c.request_line(&format!("@{} {}", self.tab, hand_give_back_request()));
        }
    }
}

/// Runs one typed act under the harness's hand: `hand` when the caller already
/// holds one on the tab (kept; its caller gives it back), else a hand taken for
/// this act alone and given back when it is over. `None` when the tab is
/// another driver's (`Hand::take` refused): the act types nothing and the
/// caller answers `held`. This makes a typed act aterm's own for the window's
/// presence layer (aterm's own hand never shows a row); every typed act in the
/// harness goes through it or [`type_fenced`]. `act` runs its own fence read
/// and its `turn` with nothing of aterm's between them.
pub(super) fn under_hand<T>(
    c: &mut Client,
    tab: &str,
    hand: Option<&mut Hand>,
    act: impl FnOnce(&mut Client) -> T,
) -> Option<T> {
    let mut own = match hand {
        Some(_) => None,
        None => Some(Hand::take(c, tab)?),
    };
    if let Some(hand) = own.as_mut().or(hand) {
        hand.keep(c);
    }
    let out = act(c);
    if let Some(mut own) = own {
        own.give_back(c);
    }
    Some(out)
}

/// How long the session has been quiet, for [`upgrade::Facts::quiet_s`]: since
/// the agent's verdict last moved, as the server publishes it on `status`
/// (`agent=… agent_since_ms=…`, what the supervisor's `await agent` wakes on).
/// A turn end (`idle`, `question`) counts from its stamp and any other verdict
/// is not quiet, so a repaint no verdict reads (Claude Code's dim `❯ continue`
/// suggestion, a ticking footer) never restarts the count. A server that
/// publishes no verdict, or `unknown`, is measured from the looks' samples of
/// the screen's `seq` (kept in `st` either way).
pub(super) fn quiet_s(status: Option<&str>, st: &mut St, seq: u64, now: u64) -> u64 {
    if seq != st.last_seq {
        st.last_seq = seq;
        st.seq_since_s = now;
    }
    let field = |key: &str| status?.split_whitespace().find_map(|t| t.strip_prefix(key));
    let since_ms = field("agent_since_ms=").and_then(|ms| ms.parse::<u64>().ok());
    match (field("agent="), since_ms) {
        (Some("idle" | "question"), Some(ms)) => ms / 1000,
        // A verdict with no evidence behind it (`unknown`: a Codex screen
        // whose last message scrolled away) or none says nothing either way.
        (Some(word), Some(_)) if word != "-" && word != "unknown" => 0,
        _ => now.saturating_sub(st.seq_since_s),
    }
}

/// Types one line at a shell's prompt as keystrokes (raw `send`, then Enter),
/// never as a paste. `Ok` only when both answer `OK`.
///
/// A paste is framed for bracketed paste whenever the terminal's mode says
/// so, and a crashed agent never restores the modes it set; a bracketed
/// paste into a shell without support (macOS `/bin/bash` 3.2) leaves the
/// closing frame's `01~` glued to `--resume <id>`. Raw bytes are read as typed
/// whatever the modes; Enter stays a carriage return under the kitty
/// `disambiguate` flag an agent leaves.
pub(super) fn type_line(c: &mut Client, sid: &str, line: &str) -> Result<(), String> {
    for request in [
        format!("@{sid} send -- {line}"),
        format!("@{sid} key enter"),
    ] {
        let reply = c.request_line(&request).map_err(|e| format!("{e}"))?;
        if !crate::is_ok_reply(&reply) {
            return Err(reply);
        }
    }
    Ok(())
}

/// Why a typed turn did not go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Typed {
    /// The screen moved between the judging read and the paste, and the host
    /// typed nothing (`turn if-gen=`'s `skipped reason=changed`). A wait: the
    /// next look judges the screen as it is.
    Changed,
    /// A person kept typing through the whole park (`yield=`) and the host
    /// typed nothing (`ERR yield timeout`). A wait like [`Typed::Changed`],
    /// never a refusal, which would fail an upgrade whose agent already ended.
    Yielded,
    /// The host pressed part of the text and stopped without Enter (`skipped
    /// reason=<why> typed=<k>`, k > 0: a person's key or a screen switch
    /// between keys, `person`/`changed`, or the deadline), or pressed all of
    /// it and its guarded Enter missed (`guard`: the cursor's row no longer the
    /// composer's). Not delivered, not recorded as typed, not retried at once:
    /// the caller records the standing words ([`left_cut`]) and never takes
    /// them back ([`left_standing`] only reads), so no later look reads them
    /// as a person's draft and the supervisor never sends them.
    Cut(String),
    /// Anything else the host answered (or the connection failed with).
    Refused(String),
}

impl Typed {
    /// The report word: `changed`, `yield`, `cut-<why>`, or the refusal's
    /// first words.
    pub(super) fn word(&self) -> String {
        match self {
            Typed::Changed => "changed".to_string(),
            Typed::Yielded => "yield".to_string(),
            Typed::Cut(why) => format!("cut-{why}"),
            Typed::Refused(e) => first_word(e),
        }
    }
}

/// How a harness-typed turn waits: for its verified submit and nothing more
/// (`idle=1` settles once the Enter is seen taken, never on the answer), with
/// `timeout=` bounding the park on a person's typing (`yield=`), the echo and
/// the submit's tries. A long wait would hold the tab (`hand=turn:<id>`) and
/// hide the answer from the loop; instead the loop awaits the answer as the
/// harness's own turn ([`typed`], `crate::supervise::HostStep::typed`), so
/// nothing is continued before it ends and it does not count as worker work
/// for the continue back-off.
pub(super) const TURN_WAIT: &str = "idle=1 timeout=10000";

/// Type one turn into the tab. `Ok` only on an `OK` reply.
pub(super) fn turn(c: &mut Client, sid: &str, text: &str) -> Result<(), String> {
    turn_fenced(c, sid, text, None).map_err(|e| match e {
        Typed::Refused(e) => e,
        other => other.word(),
    })
}

/// Whether the host's `turn` takes the `if-gen=` fence (its `help turn` names
/// it). An older host would type a leading `if-gen=…` as text, so the fence
/// is never sent unprobed. Asked once per connection
/// ([`RelayClient::help_counted`]) and by [`typing_fence`] before its read, so
/// nothing is asked between the read and the paste.
fn turn_takes_gen(c: &mut Client) -> bool {
    c.help_counted("turn")
        .is_ok_and(|(head, body)| crate::is_ok_reply(&head) && (head + &body).contains("if-gen="))
}

/// Whether the host's `turn` takes `cap=<n>` (per `help turn`): room to press
/// a long text key by key. An older host would type a leading `cap=…`, so it
/// is never sent unprobed; the answer is kept with the rest of the help.
fn turn_takes_cap(c: &mut Client) -> bool {
    c.help_counted("turn")
        .is_ok_and(|(head, body)| crate::is_ok_reply(&head) && (head + &body).contains("cap=<n>"))
}

/// How a notice is put in front of the agent: pressed one key at a time, not
/// pasted. Claude Code hands the model a long paste inside `<pasted_content>`
/// with a warning that it may hold instructions its person did not write, so
/// the agent declines to act on it (measured: 963 characters as one paste
/// arrived wrapped; as keys at 2 ms, plain). The notice is one line with no
/// control characters, so pressing it is exact; 2 ms a key is ~2 s a notice,
/// and `cap=4096` is room for far more (the turn's `timeout=` grows to cover
/// it, [`turn_wait`]). A host without `cap=` gets a paste.
///
/// Only the agent's own words are pressed ([`notice_fenced`]: notice and
/// release). The relaunch line and the continuation stay one paste
/// ([`turn_fenced`]): a shell reads a paste whole, past its bindings, and a
/// person's Enter during key-by-key typing would run a partial line.
const NOTICE_KEYS: &str = "typed=1 cadence=2 cap=4096 ";

/// The render a pressed digit waits for, in milliseconds: the host holds each
/// ASCII digit of a pressed text until the key before it has echoed
/// (`aterm-gui`'s `TypingHold`), about one Claude Code render (~150 ms at p99
/// under a build), and a notice carries 60-100 digits.
const PRESSED_DIGIT_MS: u64 = 150;

/// How a pressed text's turn waits ([`checked_turn`]): [`TURN_WAIT`] with its
/// `timeout=` (the whole turn's budget: the yield's park, the typing, the
/// submit seen taken) grown by the text's keys at [`NOTICE_KEYS`]' cadence and
/// its digits' renders ([`PRESSED_DIGIT_MS`]): the host cuts a text part-way at
/// its deadline ([`Typed::Cut`]), and the typed words stand until a person
/// clears them. The turn returns once its submit is taken, so the larger budget
/// costs nothing. A pasted text (`keys` empty) waits [`TURN_WAIT`].
fn turn_wait(keys: &str, text: &str) -> String {
    if keys.is_empty() {
        return TURN_WAIT.to_string();
    }
    let value = |words: &str, key: &str| {
        words
            .split_whitespace()
            .find_map(|w| w.strip_prefix(key)?.parse::<u64>().ok())
            .unwrap_or(0)
    };
    let count = |n: usize| u64::try_from(n).unwrap_or(u64::MAX);
    let pressed = count(text.chars().count()).min(value(keys, "cap="));
    let digits = count(text.chars().filter(char::is_ascii_digit).count());
    let timeout = value(TURN_WAIT, "timeout=")
        .saturating_add(pressed.saturating_mul(value(keys, "cadence=")))
        .saturating_add(digits.saturating_mul(PRESSED_DIGIT_MS));
    TURN_WAIT
        .split_whitespace()
        .map(|w| {
            if w.starts_with("timeout=") {
                format!("timeout={timeout}")
            } else {
                w.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// How a checked turn puts its text in ([`checked_turn`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Put {
    /// One paste: the relaunch line, the continuation.
    Paste,
    /// Key by key where the host takes `cap=` ([`NOTICE_KEYS`]): the notice
    /// and the release.
    Pressed,
}

/// Whether the host's `turn` takes the guarded submit
/// (`submit=guarded:<re>`, per `help turn`): Enter pressed only while the
/// cursor's row still matches. Never sent unprobed (an older host would type
/// it); the answer is kept with the rest of the help.
fn turn_takes_guard(c: &mut Client) -> bool {
    c.help_counted("turn")
        .is_ok_and(|(head, body)| crate::is_ok_reply(&head) && (head + &body).contains("guarded:"))
}

/// Whether the host's `turn` takes the narrow `if-rows=` fence (per `help
/// turn`). The same kept answer [`turn_takes_gen`] asks.
fn turn_takes_rows(c: &mut Client) -> bool {
    c.help_counted("turn")
        .is_ok_and(|(head, body)| crate::is_ok_reply(&head) && (head + &body).contains("if-rows="))
}

/// The checked type: one turn into the tab, parked first on a person's typing
/// momentum (`yield=0.2`, the loop's own floor) and, with a `generation`,
/// pasted only while the screen is still the one it names, checked by the host
/// under its terminal lock right before the paste (`turn if-gen=`), returning
/// once its submit is seen taken ([`TURN_WAIT`]). This closes the window
/// between judging the screen and pasting, where a person's first keystroke
/// could merge with the harness's text. `None`, or a host without the fence,
/// types unfenced. One paste ([`Put::Paste`]) for the relaunch line and the
/// continuation; the agent's own words go through [`notice_fenced`].
pub(super) fn turn_fenced(
    c: &mut Client,
    sid: &str,
    text: &str,
    generation: Option<&str>,
) -> Result<(), Typed> {
    checked_turn(c, sid, text, generation, Put::Paste)
}

/// [`turn_fenced`] pressed key by key ([`NOTICE_KEYS`]), for words the agent
/// must read as its person's (notice and release). The host holds the fence
/// between keys, never submits a text it could not press whole, and presses
/// Enter only while the cursor's row is still the composer's ending in the
/// text's last word (`submit=guarded:`); each miss is a [`Typed::Cut`] whose
/// words the caller records and leaves standing.
pub(super) fn notice_fenced(
    c: &mut Client,
    sid: &str,
    text: &str,
    generation: Option<&str>,
) -> Result<(), Typed> {
    checked_turn(c, sid, text, generation, Put::Pressed)
}

/// The checked type's one request ([`turn_fenced`], [`notice_fenced`]).
fn checked_turn(
    c: &mut Client,
    sid: &str,
    text: &str,
    generation: Option<&str>,
    put: Put,
) -> Result<(), Typed> {
    // A `rows:` fence ([`claude_fence`]) is the narrow one; anything else is
    // the read's generation.
    let fence = match generation {
        Some(g) if g.starts_with(ROWS_FENCE) => g
            .strip_prefix(ROWS_FENCE)
            .and_then(aterm_types::control_verbs::RowsFence::parse)
            .filter(|_| turn_takes_rows(c))
            .map(|f| format!("if-rows={f} "))
            .unwrap_or_default(),
        Some(g) if is_generation(g) && turn_takes_gen(c) => format!("if-gen={g} "),
        _ => String::new(),
    };
    let keys = if put == Put::Pressed && turn_takes_cap(c) {
        NOTICE_KEYS
    } else {
        ""
    };
    // A pressed text's Enter is guarded. The host stops typing on a person's
    // key or a screen switch ([`Typed::Cut`]), and on an `if-rows=` fence
    // when a box takes the composer frame's place, but keys in flight before
    // a box's first frame still reach it, and the `if-gen=` fallback has no
    // frame at all; an unguarded Enter could confirm a box's focused `1. Yes`.
    // So Enter is pressed only while the cursor's row is still the composer's
    // ending in the text's last word ([`composer_guard`], the supervisor's own
    // guard); a miss presses nothing ([`Typed::Cut`]`("guard")`). A pasted
    // notice (host without `cap=`) keeps a plain Enter: Claude Code draws a
    // long paste as `[Pasted text …]`, which no text row would match.
    let submit = if !keys.is_empty() && turn_takes_guard(c) {
        format!("submit=guarded:{} ", composer_guard('❯', text))
    } else {
        String::new()
    };
    let wait = turn_wait(keys, text);
    let (head, _) = c
        .request_counted(&format!(
            "@{sid} turn {fence}{keys}{submit}yield=0.2 {wait} {text}"
        ))
        .map_err(|e| Typed::Refused(format!("{e}")))?;
    turn_verdict(&head)
}

/// Judges one `turn` reply's head. A `skipped` verdict pressed no Enter: one
/// that typed part of the text (`typed=<k>`, k > 0), or a guarded Enter that
/// missed (`reason=guard`), is [`Typed::Cut`] by its reason; one that typed
/// nothing is [`Typed::Changed`] (`reason=changed`) or a wait (`reason=person`,
/// `reason=deadline`), like a yield that outlived the timeout
/// ([`Typed::Yielded`]). Any other `OK` typed.
fn turn_verdict(head: &str) -> Result<(), Typed> {
    if head.starts_with("ERR yield timeout") {
        return Err(Typed::Yielded);
    }
    if !crate::is_ok_reply(head) {
        return Err(Typed::Refused(head.to_string()));
    }
    let words: Vec<&str> = head.split_whitespace().collect();
    if words.contains(&"skipped") {
        let field = |key: &str| words.iter().find_map(|w| w.strip_prefix(key));
        let reason = field("reason=").unwrap_or_default();
        let typed = field("typed=").and_then(|k| k.parse::<u64>().ok());
        return Err(match (reason, typed) {
            (why, Some(1..)) => {
                Typed::Cut(if why.is_empty() { "unknown" } else { why }.to_string())
            }
            ("guard", _) => Typed::Cut("guard".to_string()),
            ("changed", _) => Typed::Changed,
            ("person" | "deadline", _) => Typed::Yielded,
            _ => Typed::Refused(head.to_string()),
        });
    }
    Ok(())
}

// ---------------------------------------------------------------- models

/// The step's model half, read once per pass: the priority list (grown from
/// Claude Code's own recommendations), the managed build's baked catalog,
/// every model a restart on it may ask for ([`models::offered`]; each
/// conversation moves to the newest of its own family first, then by the
/// list, [`models::model_due`]), and the person's saved default. The offer is
/// a Claude home's: Claude Code keeps its model caches ([`model_evidence`])
/// per home, so an agent under another home is judged by that home's
/// ([`ModelCtx::at`]). The allowlist and the default come from the agent's
/// settings chain ([`agent_settings`], [`ModelCtx::settle`]).
#[derive(Clone)]
pub(super) struct ModelCtx {
    list: Priority,
    baked: Option<Baked>,
    /// Every model a restart on the managed build may ask for under the
    /// home's caches — before an agent's allowlist cuts it
    /// ([`ModelCtx::settle`]).
    offered: Vec<String>,
    /// The saved default model (`model`, the highest document of the judged
    /// agent's settings chain that sets it, [`ModelCtx::settle`]): a choice of
    /// the person's, which the list never moves across families.
    default_model: Option<String>,
    /// The native build's own baked catalog and offer, for a session running
    /// the vendor's native install rather than the managed twin. `None` when no
    /// native build is installed.
    native: Option<(Option<Baked>, Vec<String>)>,
    /// The versions the offers are made for, the managed build's and the
    /// native build's (empty: none): what another home's offers are made for
    /// too ([`ModelCtx::at`]).
    versions: (String, String),
}

impl ModelCtx {
    /// The catalog and the offer a session is judged by: the native build's
    /// for a session that runs it (when one is installed), else the managed
    /// one's.
    fn view(&self, native: bool) -> (Option<&Baked>, &[String]) {
        match (&self.native, native) {
            (Some((baked, offered)), true) => (baked.as_ref(), offered),
            _ => (self.baked.as_ref(), &self.offered),
        }
    }

    /// No model half at all (a test's pass, or a list that cannot be read):
    /// nothing is ever due.
    pub(super) fn none() -> ModelCtx {
        ModelCtx {
            list: Priority::seed(0),
            baked: None,
            offered: Vec::new(),
            default_model: None,
            native: None,
            versions: Default::default(),
        }
    }

    fn read(opts: &Opts, targets: &Targets) -> ModelCtx {
        let now = i64::try_from(now_s()).unwrap_or(0);
        let path = state_dir(opts).join("models.json");
        let Ok(list) = (if opts.dry_run {
            std::fs::read_to_string(&path).map_or_else(
                |_| Ok(Priority::seed(now)),
                |t| Priority::parse(&t).ok_or_else(String::new),
            )
        } else {
            models::load_or_seed(&path, now)
        }) else {
            return ModelCtx::none();
        };
        let managed = targets.managed.as_ref();
        let baked = managed
            .and_then(|m| std::fs::read_to_string(&m.exe).ok())
            .and_then(|twin| catalog::twin_binary(&twin))
            .and_then(|bin| catalog::baked_cached(&bin, &state_dir(opts).join("builds")));
        let label = managed.map(|m| m.version.to_string()).unwrap_or_default();
        let ev = model_evidence(opts, &Records::of(&opts.home, None));
        // The native build is judged by its own version and catalog (the two
        // builds may know different models); its file is the binary itself.
        let native = targets.native.as_ref().map(|n| {
            (
                n.version.to_string(),
                catalog::baked_cached(&n.exe, &state_dir(opts).join("builds")),
            )
        });
        let mut ctx = ModelCtx {
            list,
            baked,
            offered: Vec::new(),
            default_model: None,
            versions: (
                label,
                native.as_ref().map(|(v, _)| v.clone()).unwrap_or_default(),
            ),
            native: native.map(|(_, nb)| (nb, Vec::new())),
        };
        // The user's home's growth is the pass's list: written and journaled
        // (`models-changed`), but for a dry run.
        let changes = ctx.grow(&ev);
        if !changes.is_empty() && !opts.dry_run {
            // Ordered: an older list is grown again from the same evidence next pass,
            // and each change is journaled as `models-changed`.
            let _ = models::write_atomic_ordered(&path, &ctx.list.render());
            for ch in &changes {
                let r = Report {
                    pid: 0,
                    tab: "-".to_string(),
                    session: "-".to_string(),
                    from: "-".to_string(),
                    to: ch.id.clone(),
                    step: "models-changed".to_string(),
                };
                ledger(opts, &r, &format!("{} ({})", ch.placed, ch.source));
            }
        }
        ctx.offer(&ev);
        ctx
    }

    /// Grows the list from what Claude Code recommends under a home
    /// ([`models::recommendations`]: the launch announcements its login was
    /// shown, `ev`, then the managed build's own `latest_per_family`), each
    /// admitted by [`models::ingest`]: the changes made, in memory. Asked for
    /// the user's home by [`ModelCtx::read`], which writes and journals them,
    /// and for every other home an agent is judged under ([`ModelCtx::at`],
    /// that agent's alone).
    fn grow(&mut self, ev: &models::Evidence) -> Vec<models::Change> {
        let now = i64::try_from(now_s()).unwrap_or(0);
        let recs = models::recommendations(ev, self.baked.as_ref(), &self.versions.0);
        models::ingest(&mut self.list, &recs, self.baked.as_ref(), now)
    }

    /// The context an agent is judged by, `sf` its record: this pass's own
    /// catalogs and list for an agent in the user's `~/.claude`; for one under
    /// another Claude home ([`Records::of_file`]), the list grown from that
    /// home's recommendations ([`ModelCtx::grow`]) and that home's offer
    /// ([`ModelCtx::offer`]); for every agent, the allowlist and saved default
    /// of its own settings chain ([`ModelCtx::settle`], [`agent_settings`]: the
    /// project the agent stands in decides them). Another home's growth is
    /// this agent's alone, made again at each judgment and never written to the
    /// pass's `models.json`, so one login's announcement never reaches another
    /// login's agents.
    fn at(&self, opts: &Opts, sf: &SessionFile) -> std::borrow::Cow<'_, ModelCtx> {
        let records = Records::of_file(&opts.home, sf);
        let mut ctx = self.clone();
        if records.home_word(&opts.home).is_some() {
            let ev = model_evidence(opts, &records);
            ctx.grow(&ev);
            ctx.offer(&ev);
        }
        let settings = records.dir().and_then(|dir| agent_settings(opts, sf, dir));
        ctx.settle(settings);
        std::borrow::Cow::Owned(ctx)
    }

    /// The final offer for one process, including a default-home session
    /// whose cached standard-profile view cannot establish its billing.
    /// Catalog/access caches keep their existing plain-profile identity.
    fn for_profile(
        &self,
        opts: &Opts,
        records: &Records,
        profile: Option<&super::profile::Context>,
    ) -> ModelCtx {
        let mut ctx = self.clone();
        let Some(credits_billed) = credits_billed_for(opts, profile) else {
            ctx.offered.clear();
            if let Some((_, offered)) = &mut ctx.native {
                offered.clear();
            }
            return ctx;
        };
        let ev = model_evidence_with_billing(opts, records, credits_billed);
        if records.home_word(&opts.home).is_some() {
            ctx.grow(&ev);
        }
        ctx.offer(&ev);
        ctx
    }

    fn at_for_profile(
        &self,
        opts: &Opts,
        sf: &SessionFile,
        profile: Option<&super::profile::Context>,
    ) -> ModelCtx {
        let records = Records::of_file(&opts.home, sf);
        let mut ctx = self.for_profile(opts, &records, profile);
        ctx.settle(records.dir().and_then(|dir| agent_settings(opts, sf, dir)));
        ctx
    }

    /// A Claude home's offer, in place of any other: what each build offers
    /// under that home's caches (`ev`), before any allowlist — which is the
    /// judged agent's settings' ([`ModelCtx::settle`]).
    fn offer(&mut self, ev: &models::Evidence) {
        self.offered = models::offered(&self.list, &self.versions.0, ev, self.baked.as_ref(), None);
        if let Some((nb, no)) = &mut self.native {
            *no = models::offered(&self.list, &self.versions.1, ev, nb.as_ref(), None);
        }
    }

    /// The offer and the default under `settings`: the offer cut to their
    /// allowlist ([`models::allowed_by`], the same cut [`models::offered`]
    /// makes, in the same order) and their saved default (the user's own
    /// `settings.json` for [`models_report`]; an agent's settings chain for its
    /// judgment, [`ModelCtx::at`]). `None`, a chain that could not be read
    /// whole: nothing is offered and nothing is moved.
    fn settle(&mut self, settings: Option<models::UserModelSettings>) {
        let Some(user) = settings else {
            self.offered.clear();
            if let Some((_, no)) = &mut self.native {
                no.clear();
            }
            self.default_model = None;
            return;
        };
        let allowed = user.allowed.as_deref();
        self.offered.retain(|id| models::allowed_by(allowed, id));
        if let Some((_, no)) = &mut self.native {
            no.retain(|id| models::allowed_by(allowed, id));
        }
        self.default_model = user.default;
    }
}

/// The `model` and `availableModels` Claude Code takes for the agent `sf`
/// ([`ModelCtx::at`]), whose records are in the Claude home `claude_dir`, read
/// through the one settings chain
/// ([`crate::supervise::policy::claude_settings::sources_under`]): the managed
/// file and its drop-ins, a `--settings` the agent was launched with (read only
/// while the process is the record's own), the project's local and shared
/// files in its cwd, and the user's `settings.json` in `claude_dir`. Each key
/// comes from the highest document that sets it, as Claude Code merges them
/// (user, project, local, flag, policy, a later one overriding; within the
/// managed tier the sorted drop-ins over the file). Fail-closed: a document
/// that is there and cannot be read whole or parsed (one inside a
/// file-provider domain under [`Opts::home`] is never opened) makes the chain
/// unknown, `None`: nothing is offered.
fn agent_settings(
    opts: &Opts,
    sf: &SessionFile,
    claude_dir: &Path,
) -> Option<models::UserModelSettings> {
    use crate::supervise::policy::claude_settings;
    use crate::supervise::policy::git_config::WorkerEnv;
    // The user's document is the one in `claude_dir`, where the agent's
    // records are ([`Records`]). The argv (its `--settings`) is read only off
    // the record's own process (its kernel start matching `procStart`): a gone
    // agent's pid may be another process's now.
    let own = kernel_start(sf.pid).as_deref() == Some(squash(&sf.proc_start).as_str());
    let mut process = own
        .then(|| atpkg::caller_shell::process_args(sf.pid))
        .flatten()
        .unwrap_or_default();
    process.env.insert(
        0,
        format!("{}={}", super::accounts::HOME_ENV, claude_dir.display()),
    );
    let worker = WorkerEnv::new(process);
    let managed = managed_settings_dir();
    let sources = claude_settings::sources_under(
        &worker,
        Path::new(&sf.cwd),
        managed.as_deref(),
        Some(&opts.home),
    );
    let in_managed = |name: &str| {
        managed
            .as_deref()
            .is_some_and(|dir| Path::new(name).starts_with(dir))
    };
    const KEYS: [&str; 2] = ["model", "availableModels"];
    let mut managed_said = Map::new();
    let mut said_below = Map::new();
    for source in sources {
        let Some(text) = source.text.ok()? else {
            continue;
        };
        let doc = claude_settings::parse(&text).ok()?;
        for key in KEYS {
            let Some(value) = doc.get(key) else {
                continue;
            };
            if in_managed(&source.name) {
                // The drop-ins, in order, over the file.
                managed_said.insert(key.to_string(), value.clone());
            } else if !said_below.contains_key(key) {
                // The first below the managed tier is the highest.
                said_below.insert(key.to_string(), value.clone());
            }
        }
    }
    for (key, value) in managed_said {
        said_below.insert(key, value);
    }
    Some(models::model_settings_of(&Value::Object(said_below)))
}

/// The settings chain the usage tap's gate reads (`super::tap::gate`) for a
/// Claude Code relaunched from `argv` (`argv[0]` first) in `cwd`: every
/// document Claude Code reads for it
/// ([`crate::supervise::policy::claude_settings::sources_under`], fenced at
/// [`Opts::home`]: one inside a file-provider domain is reported, never
/// opened) and its managed preference domains
/// ([`crate::supervise::policy::claude_settings::managed_preferences`],
/// reported, never opened). It reads over the flags the line carries (aterm's
/// own tap shed, [`upgrade::rewrite_argv`]), as the process the line starts
/// reads them, with the default home ([`Opts::home`]'s `.claude`: the gate
/// takes no other) and both user documents, `settings.json` and
/// `cowork_settings.json`: the variable that picks one is the process's and
/// the line does not carry it, and a superset can only refuse. A launch whose
/// flags cannot be carried reads nothing: no line is made for it.
pub(super) fn tap_settings(
    opts: &Opts,
    argv: &[String],
    cwd: &str,
) -> Vec<(String, super::netwatch::Source)> {
    use crate::supervise::policy::claude_settings;
    use crate::supervise::policy::git_config::WorkerEnv;
    let Ok(mut flags) = upgrade::rewrite_argv(argv, "-") else {
        return Vec::new();
    };
    flags.truncate(flags.len().saturating_sub(2));
    let carried: Vec<String> = argv.first().cloned().into_iter().chain(flags).collect();
    let account = ["USER", "LOGNAME"].iter().filter_map(|key| {
        std::env::var(key)
            .ok()
            .map(|value| format!("{key}={value}"))
    });
    let worker = |cowork: bool| {
        let env = [format!(
            "{}={}",
            super::accounts::HOMEDIR_ENV,
            opts.home.display()
        )]
        .into_iter()
        .chain(account.clone())
        .chain(cowork.then(|| "CLAUDE_CODE_USE_COWORK_PLUGINS=1".to_string()))
        .collect();
        WorkerEnv::new(atpkg::caller_shell::ProcArgs {
            argv: carried.clone(),
            env,
            ..Default::default()
        })
    };
    let managed = managed_settings_dir();
    let mut out: Vec<(String, super::netwatch::Source)> = Vec::new();
    for cowork in [false, true] {
        let worker = worker(cowork);
        let sources = claude_settings::sources_under(
            &worker,
            Path::new(cwd),
            managed.as_deref(),
            Some(&opts.home),
        )
        .into_iter()
        .chain(
            managed_preferences_dir()
                .map(|dir| claude_settings::managed_preferences(&worker, &dir))
                .unwrap_or_default(),
        );
        for source in sources {
            if !out.iter().any(|(name, _)| *name == source.name) {
                out.push((source.name, source.text.into()));
            }
        }
    }
    out
}

/// Whether the vendor's profile store holds a config for a Claude Code in the
/// default home under [`Opts::home`] (`super::accounts::profile_configs_dir`,
/// `super::accounts::profile_configured`: one listing, names only): the usage
/// tap's gate refuses a session the vendor may authenticate by a profile.
/// Asked at `$HOME/.config/anthropic` and, where this process has one, at
/// `$XDG_CONFIG_HOME/anthropic` too: the relaunched process inherits its
/// shell's, which this cannot read, and the superset can only refuse.
pub(super) fn tap_profile_store(opts: &Opts) -> bool {
    use super::accounts::{profile_configs_dir, profile_configured};
    let home = opts.home.display().to_string();
    let by_home = profile_configs_dir([("HOME", home.as_str())]);
    let by_xdg = host_xdg_config_home()
        .and_then(|xdg| profile_configs_dir([("XDG_CONFIG_HOME", xdg.as_str())]));
    by_home
        .into_iter()
        .chain(by_xdg)
        .any(|configs| profile_configured(&configs))
}

/// This process's `XDG_CONFIG_HOME`, where it has one ([`tap_profile_store`]).
#[cfg(not(test))]
fn host_xdg_config_home() -> Option<String> {
    std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|v| !v.trim().is_empty())
}

/// [`host_xdg_config_home`] in a test: none; the machine's own is never a
/// test's.
#[cfg(test)]
fn host_xdg_config_home() -> Option<String> {
    None
}

/// Claude Code's managed settings directory the model rule reads
/// ([`agent_settings`]) and the usage tap's gate ([`tap_settings`]): this
/// platform's ([`crate::supervise::policy::claude_settings::MANAGED_DIR`]).
#[cfg(not(test))]
fn managed_settings_dir() -> Option<PathBuf> {
    crate::supervise::policy::claude_settings::MANAGED_DIR.map(PathBuf::from)
}

/// The managed preference domains' directory the usage tap's gate looks in
/// ([`tap_settings`]): this platform's
/// ([`crate::supervise::policy::claude_settings::MANAGED_PREFERENCES_DIR`]).
#[cfg(not(test))]
fn managed_preferences_dir() -> Option<PathBuf> {
    Some(PathBuf::from(
        crate::supervise::policy::claude_settings::MANAGED_PREFERENCES_DIR,
    ))
}

#[cfg(test)]
thread_local! {
    /// The managed settings directory a test's model rule reads (none
    /// unless the test names one): the machine's own is never a test's.
    pub(super) static MANAGED_SETTINGS: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
    /// The managed preferences directory a test's tap gate looks in (none
    /// unless the test names one).
    pub(super) static MANAGED_PREFERENCES: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// [`managed_settings_dir`] in a test: the one it named on this thread.
#[cfg(test)]
fn managed_settings_dir() -> Option<PathBuf> {
    MANAGED_SETTINGS.with(|dir| dir.borrow().clone())
}

/// [`managed_preferences_dir`] in a test: the one it named on this thread.
#[cfg(test)]
fn managed_preferences_dir() -> Option<PathBuf> {
    MANAGED_PREFERENCES.with(|dir| dir.borrow().clone())
}

fn model_record_path(opts: &Opts, session: &str) -> PathBuf {
    model_record_in(&opts.state, session)
}

/// Where `session`'s [`ModelRecord`] lives under the harness's state root
/// `state` ([`Opts::state`]; [`state_dir`]'s `upgrade`).
fn model_record_in(state: &Path, session: &str) -> PathBuf {
    state
        .join("upgrade")
        .join("models")
        .join(format!("{session}.json"))
}

/// The resume line a person is told to run, under the relaunch's own model
/// rule: [`super::resume::command`] over the process's argv, less a `--model`
/// the conversation's record marks failed (the harness's own ask that did not
/// take, [`models::launch_model_kept`]), as every relaunch line leaves it off
/// ([`line_model_of`]); a person's own `--model` is kept. `state` is the
/// harness's state root ([`Opts::state`]; `None`: the line as launched). Every
/// resume line shown to a person is this one ([`super::resume::of_entry`]), so
/// no two disagree for one conversation.
#[must_use]
pub fn resume_command_kept(state: Option<&Path>, argv: &[String], session: &str) -> Option<String> {
    let launch = upgrade::launch_model(argv);
    let dropped = launch.as_deref().zip(state).is_some_and(|(m, state)| {
        let record = std::fs::read_to_string(model_record_in(state, session))
            .ok()
            .and_then(|t| ModelRecord::parse(&t))
            .unwrap_or_default();
        models::launch_model_kept(Some(m), &record).is_none()
    });
    if dropped {
        super::resume::command(
            &super::relaunch::with_model(argv.to_vec(), Some("")),
            session,
        )
    } else {
        super::resume::command(argv, session)
    }
}

fn load_model_record(opts: &Opts, session: &str) -> ModelRecord {
    std::fs::read_to_string(model_record_path(opts, session))
        .ok()
        .and_then(|t| ModelRecord::parse(&t))
        .unwrap_or_default()
}

fn save_model_record(opts: &Opts, session: &str, rec: &ModelRecord) {
    if opts.dry_run {
        return;
    }
    let _ = models::write_atomic(&model_record_path(opts, session), &rec.render());
}

/// One conversation's model, read once per visit.
struct ModelRead {
    verdict: ModelVerdict,
    /// The conversation's prompt cache is cold (no answer for
    /// [`models::CACHE_COLD_S`]).
    cold: bool,
    /// Seconds the current due move has been due (0 when none is) — the clock
    /// [`models::MODEL_WARM_MAX_S`] bounds.
    due_for_s: u64,
    /// Why the model a notice carried no longer rides its round's restart
    /// ([`models::ride_bar`]), or `None`: it rides, or none was carried.
    ride_barred: Option<&'static str>,
}

/// Read what the conversation runs now and decide, judged as the managed build
/// (the tests' seam; a visit reads the build first, [`model_read_for`]).
#[cfg(all(test, unix))]
fn model_read(opts: &Opts, sf: &SessionFile, launch: Option<&str>, mctx: &ModelCtx) -> ModelRead {
    model_read_for(opts, sf, launch, mctx, false, None, None)
}

/// Read what the conversation runs now and decide, judged by the build the
/// session runs (`native`: the vendor's native install, [`ModelCtx::view`]).
/// A model this harness asked for that now runs is recorded as applied (never
/// asked for again). `carried`: the model a notice of the upgrade carried
/// ([`St::carried_model`]), re-judged by the same read ([`models::ride_bar`]).
fn model_read_for(
    opts: &Opts,
    sf: &SessionFile,
    launch: Option<&str>,
    mctx: &ModelCtx,
    native: bool,
    carried: Option<&str>,
    process_view: Option<&ModelCtx>,
) -> ModelRead {
    let before = load_model_record(opts, &sf.session_id);
    let mut record = before.clone();
    let tail = std::cell::OnceCell::new();
    // Judged by the Claude home the agent runs under.
    let mctx = match process_view {
        Some(view) => std::borrow::Cow::Borrowed(view),
        None => mctx.at(opts, sf),
    };
    let (read, rows) = model_judge(
        opts,
        sf,
        &mut record,
        &tail,
        launch,
        &mctx,
        native,
        now_s(),
        carried,
    );
    model_read_kept(opts, sf, &before, &record, &rows);
    read
}

/// A ledger row one read owes — its step, the model, the detail — written
/// only where the read is kept ([`model_read_kept`]).
type ModelRow = (&'static str, String, &'static str);

/// Keep one read: its record where it changed, and the rows it owes.
fn model_read_kept(
    opts: &Opts,
    sf: &SessionFile,
    before: &ModelRecord,
    record: &ModelRecord,
    rows: &[ModelRow],
) {
    if record != before {
        save_model_record(opts, &sf.session_id, record);
    }
    for (step, model, detail) in rows {
        let mut r = blank(sf);
        r.to = format!("model:{model}");
        ledger(opts, &said(r, *step), detail);
    }
}

/// One read's judgment under one view, on `record` in memory, at `now`:
/// [`models::model_read_step`] (the early return, the settle step, a person's
/// `/model`, the rule, the ride's guard over a `carried` model, the cache and
/// the due clock, in that order; bound at Tier 1 to the derived
/// `HarnessModelSwitch`) over the view's catalog and offer and the
/// transcript's tail. It reads the transcript (once per `tail`, only past the
/// early return) and writes nothing: the record and the rows it owes are the
/// caller's to keep ([`model_read_kept`]), so the host's pre-filter can judge
/// under two views and keep what both agree on ([`model_wants_a_look`]).
#[allow(clippy::too_many_arguments)]
fn model_judge(
    opts: &Opts,
    sf: &SessionFile,
    record: &mut ModelRecord,
    tail: &std::cell::OnceCell<String>,
    launch: Option<&str>,
    mctx: &ModelCtx,
    native: bool,
    now: u64,
    carried: Option<&str>,
) -> (ModelRead, Vec<ModelRow>) {
    let (baked, offered) = mctx.view(native);
    let view = models::ModelView {
        list: &mctx.list,
        baked,
        offered,
        launch,
        default_model: mctx.default_model.as_deref(),
        started_s: models::parse_lstart(&sf.proc_start),
        now,
        carried,
    };
    let step = models::model_read_step(record, &view, || {
        tail.get_or_init(|| {
            // The conversation's transcript, where its agent keeps it.
            Records::of_file(&opts.home, sf)
                .transcript(&sf.session_id)
                .map(|p| tail_to_end(&p, TAIL_BYTES).0)
                .unwrap_or_default()
        })
        .as_str()
    });
    let mut rows = Vec::new();
    if let Some(model) = step.settled.verified {
        rows.push(("model-verified", model, "the conversation runs it"));
    }
    if let Some(model) = step.settled.failed {
        rows.push((
            "model-failed",
            model,
            "asked for on a relaunch and not what runs",
        ));
    }
    let read = ModelRead {
        verdict: step.verdict,
        cold: step.cold,
        due_for_s: step.due_for_s,
        ride_barred: step.ride_barred,
    };
    (read, rows)
}

/// The model that rides a restart made for another reason (a relaunch on
/// exit, a restart in place): the model the rule moves the conversation
/// (`sf`, launched with `argv`) to ([`models::model_due`]) when a move is due,
/// taken now because the restart happens anyway ([`models::model_moves_now`]'s
/// `build-restart`). `None` when nothing is due or the move is not the
/// harness's. It is recorded as asked right before the relaunch line that
/// carries it is typed ([`record_asked`], `relaunch::relaunch`).
pub(super) fn riding_model_in(
    opts: &Opts,
    sf: &SessionFile,
    argv: &[String],
    mctx: &ModelCtx,
    process_view: Option<&ModelCtx>,
) -> Option<String> {
    let launch = upgrade::launch_model(argv);
    // A native session is judged by the native build's own catalog.
    let native = runs_native(opts, Some(sf.pid), argv);
    let mread = model_read_for(
        opts,
        sf,
        launch.as_deref(),
        mctx,
        native,
        None,
        process_view,
    );
    models::model_to(None, &mread.verdict, mread.cold, true, mread.due_for_s)
}

/// Whether the build process `pid` runs is native, read off the process or,
/// once it has exited (relaunch on exit), off `argv[0]`, which the relaunch
/// runs again.
fn runs_native(opts: &Opts, pid: Option<u32>, argv: &[String]) -> bool {
    pid.and_then(exe_of)
        .or_else(|| argv.first().map(PathBuf::from))
        .is_some_and(|e| e.starts_with(native_root(&opts.home)))
}

/// The models a restart made for another reason (a memory banner's restart in
/// place, an exit's relaunch) carries for `session`, launched with `argv`: the
/// riding model ([`riding_model_in`], only when `ride`, only off the live
/// record `sf`), and otherwise the `/model` a person chose by hand
/// ([`hand_model_in`]). The process started at the record's start, else at
/// `start` (its `ps` start, [`kernel_start`]). One catalog read for both, none
/// when neither can be asked. The transcript is the one in `records`, the
/// agent's own ([`Records`]).
#[expect(
    clippy::too_many_arguments,
    reason = "the restart's captured process profile selector"
)]
pub(super) fn restart_models(
    opts: &Opts,
    records: &Records,
    sf: Option<&SessionFile>,
    start: &str,
    session: &str,
    argv: &[String],
    ride: bool,
    profile: Option<&super::profile::Context>,
) -> (Option<String>, Option<String>) {
    let ride = ride && sf.is_some();
    if !ride && upgrade::launch_model(argv).is_none() {
        return (None, None);
    }
    let mctx = ModelCtx::read(opts, &Targets::read(&opts.home));
    let riding = sf.filter(|_| ride).and_then(|sf| {
        let view = mctx.at_for_profile(opts, sf, profile);
        riding_model_in(opts, sf, argv, &mctx, Some(&view))
    });
    let native = runs_native(opts, sf.map(|sf| sf.pid), argv);
    let started = sf
        .and_then(|sf| models::parse_lstart(&sf.proc_start))
        .or_else(|| models::parse_lstart(start));
    let hand = hand_model_in(opts, records, session, argv, started, &mctx, native);
    (riding, hand)
}

/// The model a person chose by hand for `session` (launched with `argv` by a
/// process started at `started_s`, [`models::hand_model`]), read through the
/// catalog of the build it runs (`native`) against the record's remembered
/// `/model` choice. `None` when the launch named no model (Claude's `/model`
/// then saved the choice as the default the relaunch starts on) or nobody
/// chose one. The transcript is the one in `records`, the agent's own.
pub(super) fn hand_model_in(
    opts: &Opts,
    records: &Records,
    session: &str,
    argv: &[String],
    started_s: Option<u64>,
    mctx: &ModelCtx,
    native: bool,
) -> Option<String> {
    let launch = upgrade::launch_model(argv)?;
    let (tail, _) = tail_to_end(&records.transcript(session)?, TAIL_BYTES);
    let (baked, _) = mctx.view(native);
    let human = load_model_record(opts, session).human;
    models::hand_model(&tail, baked, &human, &launch, started_s)
}

/// The launch's `--model` a relaunch of `session`, launched with `argv`,
/// keeps ([`models::launch_model_kept`], over the record as it stands): what
/// a restart records as [`St::launch_model`].
pub(super) fn launch_kept(opts: &Opts, session: &str, argv: &[String]) -> Option<String> {
    let launch = upgrade::launch_model(argv)?;
    models::launch_model_kept(Some(&launch), &load_model_record(opts, session)).map(str::to_owned)
}

/// The model the relaunch line of `session`, launched with `argv`, asks for
/// ([`models::line_model`]): `asks`, else nothing kept of a `--model` the
/// record marks failed. The record is read only where the line asks no model
/// and the launch names one.
pub(super) fn line_model_of<'a>(
    opts: &Opts,
    session: &str,
    argv: &[String],
    asks: Option<&'a str>,
) -> Option<&'a str> {
    if asks.is_some() {
        return asks;
    }
    let launch = upgrade::launch_model(argv)?;
    models::line_model(None, Some(&launch), &load_model_record(opts, session))
}

/// Remember `model` as the one the relaunch of `session` asks for, right
/// before the line that asks it is typed, so it is never asked for twice (a
/// model then not taken is recorded as failed, [`model_read_for`]). The ask,
/// for [`take_back_ask`] should that line not be typed; `None` where nothing
/// was recorded.
pub(super) fn record_asked(opts: &Opts, session: &str, model: &str) -> Option<models::Asked> {
    if opts.dry_run || model.is_empty() {
        return None;
    }
    let mut rec = load_model_record(opts, session);
    let asked = rec.asked(model, now_s());
    save_model_record(opts, session, &rec);
    Some(asked)
}

/// The build a session runs, as a relaunch target for a model-only restart:
/// the managed twin when it runs that (every managed relaunch goes through
/// the twin), the native build for a native session, else the process's own
/// executable.
fn same_build(
    targets: &Targets,
    exe: &Path,
    running: &Version,
    running_native: bool,
) -> Option<Candidate> {
    let pick = if running_native {
        targets.native.as_ref()
    } else {
        targets.managed.as_ref()
    };
    if let Some(c) = pick.filter(|c| c.version == *running) {
        return Some(c.clone());
    }
    (!exe.as_os_str().is_empty()).then(|| Candidate {
        exe: exe.to_path_buf(),
        version: running.clone(),
        source: if running_native {
            Source::Native
        } else {
            Source::Managed
        },
    })
}

/// The cheap pre-filter [`due`] and the pass ask: whether this session's
/// model is due (the launch's own `--model` is not read here — it is in
/// [`visit_with_claim`]).
fn model_wants_a_look(opts: &Opts, sf: &SessionFile, mctx: &ModelCtx) -> bool {
    // Judged by the Claude home the agent runs under, as the visit judges it.
    let mctx = &*mctx.at(opts, sf);
    // Which build the session runs is not read here, to stay cheap: a look is
    // wanted when a move is due under either view, and the visit judges it by
    // the real build.
    let views: Vec<bool> = std::iter::once(false)
        .chain(mctx.native.is_some().then_some(true))
        .filter(|&native| !mctx.view(native).1.is_empty())
        .collect();
    let before = load_model_record(opts, &sf.session_id);
    let tail = std::cell::OnceCell::new();
    let now = now_s();
    let reads: Vec<(ModelRecord, ModelRead, Vec<ModelRow>)> = views
        .iter()
        .map(|&native| {
            let mut judged = before.clone();
            let (read, rows) =
                model_judge(opts, sf, &mut judged, &tail, None, mctx, native, now, None);
            (judged, read, rows)
        })
        .collect();
    // Reads are kept only where every view left the record alike (always,
    // with one view). With two, a clock stepped under the view the session
    // does not run would restart the real one on every sweep; the visit,
    // which knows the build, steps it.
    if let Some((first, _, rows)) = reads.first()
        && reads.iter().all(|(judged, _, _)| judged == first)
    {
        model_read_kept(opts, sf, &before, first, rows);
    }
    reads
        .iter()
        .any(|(_, read, _)| matches!(read.verdict, ModelVerdict::Due { .. }))
}

/// `aterm harness upgrade models`: the list best first, each model's
/// availability on the managed build (and why not), every model a restart may
/// ask for (a conversation moves to the newest of its own family first), the
/// list's target, and what Claude Code recommends. Read-only (`opts.dry_run`
/// is honoured by [`ModelCtx::read`]).
#[must_use]
pub fn models_report(opts: &Opts) -> String {
    use std::fmt::Write as _;
    let targets = Targets::read(&opts.home);
    let mut ctx = ModelCtx::read(opts, &targets);
    // The user's own home's view: its `settings.json` allowlist and default.
    ctx.settle(Some(
        std::fs::read_to_string(opts.home.join(".claude/settings.json"))
            .map(|t| models::parse_user_settings(&t))
            .unwrap_or_default(),
    ));
    let label = targets
        .managed
        .as_ref()
        .map_or_else(|| "?".to_string(), |m| m.version.to_string());
    let ev = model_evidence(opts, &Records::of(&opts.home, None));
    let (_, verdicts) = models::target(&ctx.list, &label, &ev, ctx.baked.as_ref());
    let mut out = format!("Claude Code (managed): {label}\n");
    for (i, (id, a)) in verdicts.iter().enumerate() {
        let name = ctx
            .baked
            .as_ref()
            .and_then(|b| b.model(id))
            .map_or(String::new(), |m| format!(" ({})", m.display_name));
        let verdict = match a {
            models::Availability::Yes(why) => format!("available — {why}"),
            models::Availability::No(why) => format!("NOT available — {why}"),
        };
        let _ = writeln!(out, "  {}. {id}{name}: {verdict}", i + 1);
    }
    let _ = writeln!(
        out,
        "on offer (a session moves to the newest of its own family first): {}",
        if ctx.offered.is_empty() {
            "none".to_string()
        } else {
            ctx.offered.join(", ")
        }
    );
    let _ = writeln!(
        out,
        "target (then, for a model nobody chose, up this list): {}",
        models::list_target(&ctx.list, &ctx.offered).unwrap_or("none available")
    );
    if let (Some(n), Some((_, offered))) = (targets.native.as_ref(), ctx.native.as_ref()) {
        let _ = writeln!(
            out,
            "native Claude Code {} (a session on it is judged by its own catalog): on offer {}; target {}",
            n.version,
            if offered.is_empty() {
                "none".to_string()
            } else {
                offered.join(", ")
            },
            models::list_target(&ctx.list, offered).unwrap_or("none available")
        );
    }
    for (id, source) in models::recommendations(&ev, ctx.baked.as_ref(), &label) {
        let _ = writeln!(out, "  recommended by Claude Code: {id} ({source})");
    }
    out
}

/// `aterm harness upgrade models set <id>,<id>,...`: replace the list, best
/// first; every entry a Claude model id, no repeats. One ledger row.
///
/// # Errors
/// An entry that is not a model id, a repeat, or a write that failed.
pub fn set_models(opts: &Opts, list: &str) -> Result<Vec<String>, String> {
    let now = i64::try_from(now_s()).unwrap_or(0);
    let path = state_dir(opts).join("models.json");
    let ids: Vec<String> = list
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let mut p = models::load_or_seed(&path, now).unwrap_or_else(|_| Priority::seed(now));
    let candidate = Priority {
        ids: ids.clone(),
        history: p.history.clone(),
    };
    if ids.is_empty() || Priority::parse(&candidate.render()).is_none() {
        return Err(
            "every entry must be a Claude model id (claude-<family>-<n>[-<n>]), no repeats"
                .to_string(),
        );
    }
    p.ids.clone_from(&ids);
    p.history.push(models::Change {
        id: list.to_string(),
        placed: "set".into(),
        source: "owner".into(),
        at: now,
    });
    models::write_atomic(&path, &p.render()).map_err(|e| format!("{}: {e}", path.display()))?;
    let r = Report {
        pid: 0,
        tab: "-".to_string(),
        session: "-".to_string(),
        from: "-".to_string(),
        to: ids.join(","),
        step: "models-set".to_string(),
    };
    ledger(opts, &r, "the priority list was set by hand");
    Ok(ids)
}

/// How the one signal went.
pub(super) enum Terminated {
    Sent,
    /// Not sent, and why (a hold, or the process no longer leads the
    /// foreground group): a wait, not a failure.
    Refused(String),
    /// The kernel refused a signal we did send.
    #[cfg_attr(
        not(unix),
        expect(dead_code, reason = "only unix sends the signal itself")
    )]
    Failed,
}

/// SIGTERM to the agent as one server-side decision: `signal term pid=<pid>
/// quiet=<grace_s>` reaches exactly that process, only while it leads the
/// tab's foreground group, and is refused under a hold, a person's input
/// within the grace, or unread written input. An older aterm (`ERR bad signal
/// argument: quiet=…`, or `ERR unknown signal` without `pid=`) sends nothing;
/// then the hold is re-read and the older form sent, or the kernel asked
/// directly.
pub(super) fn terminate(c: &mut Client, tab: &str, pid: i32, grace_s: u32) -> Terminated {
    match c.request_line(&format!("@{tab} signal term pid={pid} quiet={grace_s}")) {
        Ok(reply) if crate::is_ok_reply(&reply) => Terminated::Sent,
        Ok(reply) if reply.starts_with("ERR bad signal argument: quiet=") => {
            if held(c, tab, grace_s) {
                return Terminated::Refused("held".to_string());
            }
            match c.request_line(&format!("@{tab} signal term pid={pid}")) {
                Ok(reply) if crate::is_ok_reply(&reply) => Terminated::Sent,
                Ok(reply) if reply.starts_with("ERR halted") => {
                    Terminated::Refused("held".to_string())
                }
                Ok(_) | Err(_) => Terminated::Refused("refused".to_string()),
            }
        }
        Ok(reply) if reply.starts_with("ERR unknown signal") => {
            if held(c, tab, grace_s) {
                return Terminated::Refused("held".to_string());
            }
            #[cfg(unix)]
            {
                // SAFETY: a plain signal to one verified pid of our own uid.
                if unsafe { libc::kill(pid, libc::SIGTERM) } == 0 {
                    Terminated::Sent
                } else {
                    Terminated::Failed
                }
            }
            // Windows has no SIGTERM for a console process, and this drive
            // never sends `TerminateProcess`: a wait, never a kill. No Windows
            // step reaches here (every job read is `ps -o …`).
            #[cfg(not(unix))]
            {
                let _ = pid;
                Terminated::Refused("unsupported".to_string())
            }
        }
        Ok(reply) if reply.starts_with("ERR halted") || reply.starts_with("ERR busy") => {
            Terminated::Refused("held".to_string())
        }
        Ok(_) | Err(_) => Terminated::Refused("refused".to_string()),
    }
}

// ---------------------------------------------------------------- state

/// One row of the wait log ([`St::waits`]): when a session's upgrade began to
/// wait on `word` (or acted: `announced:1`, `gave-up`), and the facts that
/// decided it (`note`, from [`wait_note`]): the screen's state, the person
/// stamp, the composer's verdict, the render state, what runs under the agent.
/// So "why has this tab not been upgraded" is one read of the record.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WaitNote {
    /// Unix seconds.
    pub at: u64,
    /// The wait word, or the act's step word.
    pub word: String,
    /// The deciding facts, one line; empty where none were read.
    pub note: String,
}

/// How many [`WaitNote`]s a record keeps, newest last.
pub const WAITS_KEPT: usize = 24;

/// What a restart's record took when it was written ([`St::signalled`]) and
/// puts back if its signal is never sent ([`St::unsent`]).
#[derive(Debug)]
pub(super) struct Unsent {
    phase: Phase,
    release: String,
    marker: String,
    markers: Vec<String>,
    /// [`St::fallback_from`] as it stood: a restart that carries a hand
    /// choice makes that the origin ([`restart`]), and one never sent did not.
    fallback_from: Option<String>,
}

/// [`St::fallback_from`] on disk for a fallback whose launch named no model.
/// Never a model id.
const FALLBACK_NO_MODEL: &str = "-";

/// [`St::source`] of a restart's or a relaunch's record that kept the
/// session on the program it ran: no build was moved to.
pub(super) const SAME_PROGRAM: &str = "same";

/// What the sweep remembers about one conversation's upgrade. Everything a
/// relaunch needs is written before the agent is signalled, so a sweep that
/// dies between exit and relaunch leaves enough for the next to finish.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct St {
    pub(super) phase: Phase,
    pub(super) from: String,
    pub(super) to: String,
    pub(super) source: String,
    /// The READY marker of the latest notice ([`upgrade::ready_marker`]).
    pub(super) marker: String,
    /// Every READY marker this round typed and still honours, the latest
    /// included ([`answered`]): notices queued behind a usage limit arrive at
    /// once, and a READY to any of them answers this upgrade. Forgotten with
    /// `marker` ([`St::forget_markers`]) at a void, release or owner's word.
    pub(super) markers: Vec<String>,
    /// The release owed ([`upgrade::release_prompt`]): why the upgrade
    /// abandoned a notice without restarting the agent (`gave-up`, `void`,
    /// `skipped`, `deferred`, a stop's reason), or empty. Set once per
    /// abandonment ([`St::owe_release`]); cleared when typed or dropped
    /// ([`release_void`]), when a new notice supersedes it, or when the
    /// restart ends the agent (its carry-on says continue).
    pub(super) release: String,
    /// Every READY marker this round typed, honoured or not, since
    /// [`St::markers`] forgets them at a void, stop or release while an owed
    /// release still needs them. Never an answer: a READY to one marks the
    /// agent's last answer to the upgrade, after which a direction counts
    /// ([`release_void`], [`upgrade::directed_since_ready`]). Opened by the
    /// round's first notice ([`St::announced`]) and carried with an owed
    /// release onto a new target's state ([`St::for_target`]).
    pub(super) asked: Vec<String>,
    pub(super) salt: u64,
    pub(super) last_seq: u64,
    pub(super) seq_since_s: u64,
    /// The agent that was signalled.
    pub(super) pid: u32,
    /// The process whose tab received the READY notice. `pid` becomes the
    /// process signalled at restart, so notice ownership needs its own fence.
    pub(super) notice_pid: u32,
    pub(super) notice_start: String,
    /// The shell that gets its prompt back.
    pub(super) shell: u32,
    /// The tab the shell lives in.
    pub(super) tab: String,
    /// THE EARLIER TABS THAT HELD THIS CONVERSATION, oldest first (at most
    /// [`EARLIER_TABS`], never [`St::tab`]): a cold restore reopens the tab
    /// under a new sid, and the turns the old tab's loop typed into the
    /// conversation are in THAT tab's ledger — read as the harness's own
    /// beside the current tab's ([`conversation_typed`]), never as someone's
    /// prompt that reopens a finished task (the cross-lane review's
    /// XREV-LC-1). Carried by [`save_at`] from the record on disk, and set by
    /// a cold restore's relaunch from the tab its layout names
    /// (`relaunch::after_host_ended_from`). On disk the key `tabs` is
    /// written only when set.
    pub(super) tabs: Vec<String>,
    /// WHEN EACH EARLIER TAB LOST THE CONVERSATION (unix seconds, `(tab,
    /// until)`, a tab of [`Self::tabs`]): stamped as a record is written for
    /// another tab ([`save_at`]: the tab on disk, at that write) and as a
    /// relaunch names a tab it left (`relaunch::after_exit_as`: the cold
    /// restore's `was`, and the record's own tab). What that tab's loop typed
    /// AFTER it (and [`upgrade::TYPED_WITHIN_MS`] past it) is another
    /// conversation's — a tab a resumed conversation left can go on
    /// supervising its own — never this one's harness turn
    /// ([`conversation_typed`]; the decision review's RR-LC-4: a person's
    /// `keep going` that reopened a finished task read as the harness's).
    /// A tab with no stamp (an older record's) is read unbounded. On disk
    /// `tabs_until`, `<until> <tab>` each, written only when set.
    pub(super) tabs_until: Vec<(String, u64)>,
    /// The line typed at that prompt — once announced and before the
    /// signal, the line the announcement was planned on.
    pub(super) line: String,
    /// THE LINE BACK (LC-5 of ): the move's relaunch line on
    /// the build its agent ran — the process's own program, with its launch,
    /// its Claude home and login and the model it ran (a list's model is the
    /// move's) — planned at the restart beside [`Self::line`] ([`restart`]).
    /// Typed once in its place where the move's relaunch ended as it started
    /// ([`St::roll_back`]), so a build that answers its probe and fails only
    /// at a real start leaves no agent down. Empty where none was planned (a
    /// move onto the program the agent runs, a restart on its own program, an
    /// older record's). On disk `back_line`, written only when set.
    pub(super) back_line: String,
    /// Where that prompt left the cursor when a relaunch attempt first read it
    /// ([`PromptMark`]): kept across attempts, so a person's typing at the
    /// prompt is judged against the prompt as it came back, never against
    /// their own half-typed command.
    pub(super) prompt: Option<PromptMark>,
    /// The shell integration's command block the relaunch line was typed into
    /// (`super::relaunch::line_block`, read right before it was; `None`: the
    /// tab's shell marks none, or none was read): its end is the shell's own
    /// word that the line's command ran and ended (`super::relaunch::ShellBack`).
    /// Stamped with every line typed; written to disk only when set.
    pub(super) line_block: Option<u64>,
    /// The last held-back reason the ledger was told ([`held_back`]), so it is
    /// said once, or the stuck restart it reported (`stuck:<what>`,
    /// [`note_stuck`]).
    pub(super) noted: String,
    /// Why the agent is being relaunched: empty for this module's upgrade,
    /// [`super::relaunch::CAUSE_EXIT`] for a relaunch on exit — which of the
    /// two continuations the new process is typed.
    pub(super) cause: String,
    /// The model the agent's last turn named when it answered READY
    /// ([`upgrade::transcript_model`] over the tail that proved the answer), or
    /// empty when no turn named one. Said in the continuation and in the
    /// outcome, never acted on: the relaunch carries the launch's own flags.
    pub(super) model_before: String,
    /// Where that tail read ended in the transcript, in bytes: the resumed
    /// session's first turn is the first one written past it. `0`: no mark was
    /// taken (a restart an older build began), and the resumed model is then
    /// unconfirmed rather than read from the start of the conversation.
    pub(super) mark: u64,
    /// The `--model` the relaunch line keeps from the launch
    /// ([`launch_kept`]), or empty when the launch named none or one the
    /// record marks failed: what decides the model a resumed session comes
    /// back on, said in the outcome.
    pub(super) launch_model: String,
    /// The model the relaunch line asks for (the newest of the conversation's
    /// family, else the list's, [`super::upgrade_models`]), or empty when it
    /// keeps the launch's own `--model` or names none. Said in the outcome.
    pub(super) model_list: String,
    /// The fallback origin: the model the launch named before a bucket's
    /// fallback relaunch replaced it (`None`: no fallback stands; `Some("")`:
    /// one stands and the launch named none), what the way back at the
    /// bucket's reset returns to ([`super::relaunch::fallback_origin`]). Set by
    /// the fallback's restart, carried over other restarts while it stands
    /// (replaced by a hand choice such a restart carries), cleared by the way
    /// back. Stored as empty, [`FALLBACK_NO_MODEL`], or the model.
    pub(super) fallback_from: Option<String>,
    /// The login the relaunch line carries ([`super::relaunch::Launch::login`]):
    /// the `CLAUDE_SECURESTORAGE_CONFIG_DIR` the planner required on
    /// [`St::line`] (the ended process's, or an account move's target); `None`
    /// when it required none. The line's fact as a field, so the owner's view
    /// and a host checking the new process parse no shell line. Written to disk
    /// only when set (empty included).
    pub(super) login: Option<String>,
    /// The Claude home the relaunched agent keeps its records under
    /// ([`Records::home_word`]): the directory the ended process's records were
    /// read from ([`Records::of_snapshot`]), a relative `CLAUDE_CONFIG_DIR`
    /// (empty included) resolved, never kept raw. The relaunched process files
    /// its record and transcript there, so every step that carries the relaunch
    /// on looks there ([`St::agent_records`]). An upgrade's record names it from
    /// the visit that finds its agent ([`Homes`]), and every retarget carries
    /// it. `None`: the user's own `~/.claude`. Written to disk only when set; an
    /// older record reads as none.
    pub(super) home: Option<String>,
    /// The profile that home keeps apart from its directory ([`profile_apart`]:
    /// `$HOME/.claude.json` for a `CLAUDE_CONFIG_DIR` set and empty; beside
    /// another `HOME`'s `.claude`): the login Claude Code keeps for the agent,
    /// which its model rule judges it by. Kept and read back with the home
    /// ([`St::keep_home`], [`St::agent_records`]), since a pass that learns the
    /// home from this record ([`recorded_homes`]) would otherwise read
    /// `<dir>/.claude.json`, nobody's. A relaunch also records
    /// `~/.claude/.claude.json` for a home naming the user's own `~/.claude`,
    /// but nothing reads it: the model rule reads a home's own evidence only
    /// where [`Records::home_word`] names a home ([`ModelCtx::at`]), which
    /// `~/.claude` never is, so that agent is judged by the pass's own
    /// `~/.claude.json`; and [`Homes::add`] skips `~/.claude`, so the sweep's
    /// visit reads the agent unstamped and [`St::keep_home`] writes this back
    /// to `None`. `None` for every other home; written to disk only when set.
    pub(super) home_profile: Option<String>,
    /// The build that failed to start for the conversation, kept by a restart's
    /// record on its own program ([`St::kept_by_restart`]) from the record it
    /// was written over (the stop's own record holds it as its stop,
    /// [`St::failed_start`]). The round after the restart rests as that stop's
    /// would ([`St::retarget`]). Written to disk only when set
    /// ([`FailedStart::word`]).
    pub(super) failed_start_kept: Option<FailedStart>,
    /// Set once the continuation is typed while the resumed session's first
    /// answer is still unread ([`super::relaunch::confirm`]): the epoch second
    /// past which a sweep still finding none records the model unconfirmed.
    /// For a QUIET restart ([`St::quiet`], nothing typed), the second its one
    /// short look at what the resumed build reports did not come back closes
    /// (`super::relaunch::restore_look`). `0`: nothing pending. The phase is
    /// already done, so nothing types the continuation twice.
    pub(super) confirm_by: u64,
    /// THE WORD A RESTART THAT TYPED NOTHING ENDS ITS LOOK WITH
    /// (`done:finished`, `done:taskless`, `done:no-continue`; empty: none, or a
    /// quiet restart's `done:resumed`): such a carry-on keeps the quiet
    /// restart's ONE LOOK open (`super::relaunch::restore_look`) at what the
    /// resumed build reports did not come back, and says this word again when
    /// nothing did (the cross-lane review's XREV-LC-2). On disk the key
    /// `look_word` is written only when set.
    pub(super) look_word: String,
    /// The build the relaunched process runs — its session file's `version`,
    /// the one its transcript rows carry — and its pid, for the `done` row. A
    /// Claude Code's is stamped only as its continuation is typed
    /// (`continued`). (A record the retired Codex lane wrote may carry the
    /// TUI it adopted here.)
    pub(super) resumed_on: String,
    pub(super) resumed_pid: u32,
    /// The TUI a Codex restart's relaunch line brought up, as the look that
    /// FOUND it recorded it (`codex_alab_drive::relaunched_by_us`), adopted
    /// or not; `0`: none found yet. Once it is gone, a Codex leaving the tab
    /// was that TUI — a person's `/exit`, a crash — never the restart's exit
    /// (`relaunch::restarted`).
    /// A relaunched Claude Code's is stamped as its supervisor ADOPTS it
    /// (`relaunch::await_new`, cleared as its relaunch line is typed): the
    /// adoption is said once, however many steps find that process — never [`St::resumed_pid`],
    /// which says its continuation was typed.
    pub(super) relaunched_pid: u32,
    /// Codex: the nonempty kernel birth observed when `relaunched_pid` was FIRST
    /// found. Empty means unknown, including records written before this field.
    /// A repeated look never replaces it with a reused pid's birth. Only this
    /// identity may hand the first launch's permission floor to a later record.
    pub(super) relaunched_start: String,
    /// When this round's relaunch line was typed (unix seconds; `0`: none, or
    /// an older record): the move's instant. [`Phase::Relaunched`] carries it
    /// only until the phase moves on, and a Claude Code move reaches
    /// [`Phase::Done`] at its session's first idle point after the relaunch
    /// (days later for a goal-mode loop), so the finished move's record names
    /// it from here ([`upgrade_status::Row::moved_at`]). Stamped with the
    /// phase; every later relaunch line restamps it.
    pub(super) relaunched_at: u64,
    /// THE CONVERSATION'S LAST MOVE, outliving the record that made it: when
    /// the relaunch line of the newest restart that MOVED the conversation —
    /// onto a bucket's fallback model, back from it, under another account —
    /// was typed (unix seconds; `0`: none known). A move's own record names it
    /// by [`St::relaunched_at`] ([`super::relaunch::last_move_at`]); the one
    /// writer ([`save_at`]) carries it from the record on disk onto every
    /// record saved over it — a crash's relaunch, a cold restore's, a memory
    /// restart, an upgrade's round — so the limit row the move left is still
    /// the one it left once the record is written over
    /// ([`super::relaunch::moved_off`]; LIM-1's review: an account move, aterm
    /// ended before the carry-on was answered, and the cold restore's record,
    /// cause `host`, held the carry-on behind the old account's weekly) — and
    /// where the record itself is removed, it stays beside it until the move
    /// is older than the longest limit ([`record::kept_move`]). On
    /// disk the key `moved_at` is WRITTEN ONLY WHEN SET: a record with none is
    /// byte for byte what it was.
    pub(super) moved_at: u64,
    /// THAT MOVE'S TARGET ([`super::relaunch::last_move_target`]): for an
    /// account move, the login its relaunch line required (the move record's
    /// own [`St::login`]); `None` for another move, or one an older build
    /// wrote. Carried beside [`St::moved_at`] by the one writer, so the limit
    /// row the move left is hidden only from an agent that runs under the
    /// login moved to (the residual review's R4: one back under the login it
    /// left — a cold restore from a leaf captured before the move, or a
    /// person's `claude -r` under the old login — had that account's limit
    /// hidden, and the carry-on typed into it). On disk `moved_login`,
    /// WRITTEN ONLY WHEN SET.
    pub(super) moved_login: Option<String>,
    /// …and for a bucket's fallback ([`super::relaunch::CAUSE_MODEL`]), the
    /// model it LEFT ([`super::relaunch::fallback_origin`]; `""`: the launch
    /// named none): an agent that runs that model again is not under the
    /// move. On disk `moved_left_model`, WRITTEN ONLY WHEN SET (empty
    /// included, as [`FALLBACK_NO_MODEL`]).
    pub(super) moved_left_model: Option<String>,
    /// The look that first saw the person's hold standing now
    /// ([`upgrade::person_hold`]), every look since seeing it too, none more
    /// than [`upgrade::HOLD_GAP_S`] apart; `0`: none.
    pub(super) hold_since_s: u64,
    /// The last look that saw that hold ([`upgrade::hold_since`]); `0`: none.
    pub(super) hold_seen_s: u64,
    /// When the READY answer the upgrade acts on was first heard, held at
    /// the session's limit ([`upgrade::ready_since`]); `0`: none now. What
    /// bounds how long the agent's own background work may hold a READY
    /// restart ([`upgrade::Facts::ready_s`]).
    pub(super) ready_since: u64,
    // The owner's view, read by [`upgrade_status`]: none of it gates an act
    // except `request`, the owner's own word.
    /// When the session was first found behind the build it is being moved
    /// to, carried onto a newer target while nothing has begun ([`St::for_target`]):
    /// the age is the session's, not the build's. `0` in a state an older
    /// build wrote, read as `salt` ([`St::behind_since`]).
    pub(super) pending_since: u64,
    /// The process that age counts: the kernel start (`ps lstart`, squashed) of
    /// the Claude Code process this record was minted behind. An age belongs to
    /// one process, never carried by [`St::for_target`] onto one the owner
    /// restarted by hand. Empty in a record an older build wrote, read against
    /// the build it was minted behind ([`St::same_process`]).
    pub(super) proc_start: String,
    /// The wait log: the last [`WAITS_KEPT`] changes of wait and acts, newest
    /// last ([`WaitNote`]). Gates nothing.
    pub(super) waits: Vec<WaitNote>,
    /// The last wait a step recorded ([`upgrade::wait_word`]); empty once it
    /// acts. The host's watch also records one off any point: its read's wait
    /// or what holds the point its step waits for, else
    /// [`upgrade::NEXT_POINT`] (`upgrade_watch::look_word`).
    pub(super) wait: String,
    /// When that wait began: the first step that recorded the same word.
    pub(super) wait_since: u64,
    /// For a person at the tab (`attended`), when a look last recorded it; a
    /// wait not confirmed since is the last point's word
    /// ([`upgrade_status::Row::wait_seen`]). `0` for every other wait and in
    /// older states.
    pub(super) wait_seen: u64,
    /// What ran under the agent at the last waiting look, by pid, name and
    /// start ([`HeldBy`], [`St::note_held`]): what the owner is told holds the
    /// move. Empty once a step acts.
    pub(super) held_by: Vec<HeldBy>,
    /// How many looks in a row read the screen authoritatively idle
    /// ([`upgrade::Facts::idle_looks`]); 0 after any other look. One process's
    /// run: the looks are of [`St::idle_pid`], each within [`IDLE_RUN_GAP_S`]
    /// of the last ([`St::idle_at`]); another process's screen, or a long gap,
    /// begins the run again.
    pub(super) idle_looks: u32,
    /// The process whose screen those looks read; `0`: none.
    pub(super) idle_pid: u32,
    /// When the last of them was taken (unix seconds); `0`: none.
    pub(super) idle_at: u64,
    /// The repaint-proof settle's run ([`St::still`],
    /// [`upgrade::Facts::still_s`]): the print of the last words the last
    /// authoritatively idle look read ([`upgrade::still_print`]), when the run
    /// of that print began, and its latest look; empty and `0` with no run.
    pub(super) still_print: String,
    pub(super) still_since: u64,
    pub(super) still_at: u64,
    /// The owner's word ([`Request`]), written by `aterm harness upgrade <sid>
    /// --now|--defer|--skip` under the sweep lock ([`upgrade_status::ask`]).
    pub(super) request: Request,
    /// The tab the owner named with that word: the word is on the tab, so the
    /// conversation resumed elsewhere carries none of it ([`St::request_for`]).
    /// Empty: no word, or an older record.
    pub(super) request_tab: String,
    /// When (unix seconds) the owner's word was written; `0`: none or older
    /// record. Bounds how long a `--now` keeps an overdue upgrade from reading
    /// stalled ([`upgrade_status::Row::stall`]).
    pub(super) request_at: u64,
    /// The owner's outcome line of a finished restart
    /// ([`super::relaunch::confirm`]) and when it was written — what the window
    /// records once, as the restart's record.
    pub(super) outcome: String,
    pub(super) done_at: u64,
    /// Which agent this record is of ([`upgrade::Agent`]): a Codex's is
    /// filed as `codex-<tab>` and carried by the Codex pass
    /// (`codex_alab_drive`) alone — every Claude loop here passes it by.
    pub(super) agent: upgrade::Agent,
    /// Codex: the thread the TUI held (embedded, by its writer lock) or the
    /// one its exit named (daemon mode); empty until known.
    pub(super) thread: String,
    /// Codex: the `$CODEX_HOME` the TUI ran with.
    pub(super) codex_home: String,
    /// Codex: the `CODEX_HOME` the TUI's own exec environment set, verbatim
    /// ([`super::relaunch::CodexRun::home_env`]),
    /// which every relaunch line it plans carries as required environment
    /// (`codex_alab::line_env`); `None`: the TUI had no such variable,
    /// and the line adds none. [`St::codex_home`] alone cannot tell a home
    /// set to `~/.codex` from the default. On disk the key `codex_home_env`
    /// is WRITTEN ONLY WHEN SET (empty included): a record with none is byte
    /// for byte what it was, and an older record reads as none.
    pub(super) codex_home_env: Option<String>,
    /// Codex: the TUI's argv, its working directory and the managed twin, as
    /// they were at the exit — what the relaunch is planned from once the
    /// exit names the thread, by which time the TUI is gone.
    pub(super) argv: Vec<String>,
    pub(super) cwd: String,
    pub(super) twin: String,
    /// Muse Code: THE DATA HOME ITS SESSIONS ARE UNDER (`$XDG_DATA_HOME`,
    /// else `$HOME/.local/share`, canonical), as the snapshot read it off the
    /// process's own exec environment: where the relaunch finds the session's
    /// lock (a live holder keeps its line untyped: Muse Code opens its picker
    /// on a held session) and the relaunched process holding it. Empty for
    /// every other agent. On disk the key `muse_data` is WRITTEN ONLY WHEN
    /// SET: a record with none is byte for byte what it was.
    pub(super) muse_data: String,
    /// Muse Code: THE HOMES ITS RELAUNCH LINE CARRIES, `NAME=VALUE` each
    /// (`XDG_DATA_HOME`, `XDG_CONFIG_HOME` as the process's own exec
    /// environment set them, verbatim; `upgrade_muse::line_env`): REQUIRED on
    /// the line, which is refused rather than typed without them. On disk the
    /// key `muse_env` is WRITTEN ONLY WHEN SET.
    pub(super) muse_env: Vec<String>,
    /// agy: THE CONFIG DIRECTORY ITS CONVERSATIONS ARE UNDER
    /// (`$HOME/.gemini/antigravity-cli`, as its `HOME` and its hidden
    /// `--gemini_dir`/`--app_data_dir` place it, canonical), as the snapshot
    /// read it: where the relaunch finds the conversation's presence lock (a
    /// live holder keeps its line untyped) and the crash file of the agy its
    /// line started. Empty for every other agent. On disk the key `agy_config`
    /// is WRITTEN ONLY WHEN SET: a record with none is byte for byte what it
    /// was.
    pub(super) agy_config: String,
    /// agy: THE HOME ITS RELAUNCH LINE CARRIES (`HOME=<dir>`, where its own
    /// exec environment set another than the user's;
    /// `upgrade_agy::line_env`): REQUIRED on the line, which is refused
    /// rather than typed without it. On disk the key `agy_env` is WRITTEN
    /// ONLY WHEN SET.
    pub(super) agy_env: Vec<String>,
    /// Gemini CLI: ITS PROJECT'S CHATS DIRECTORY
    /// (`<GEMINI_CLI_HOME or HOME>/.gemini/tmp/<slug>/chats`), as the snapshot
    /// read it: where the relaunch's pre-check folds the session's files
    /// right before its line, and the start-time rule reads which session a
    /// relaunched process came up on. Empty for every other agent. On disk
    /// the key `gemini_chats` is WRITTEN ONLY WHEN SET.
    pub(super) gemini_chats: String,
    /// Gemini CLI: THE HOMES ITS RELAUNCH LINE CARRIES (`GEMINI_CLI_HOME`
    /// where set, `HOME` where another than the user's;
    /// `upgrade_gemini::line_env`): REQUIRED on the line. On disk the key
    /// `gemini_env` is WRITTEN ONLY WHEN SET.
    pub(super) gemini_env: Vec<String>,
    /// Gemini CLI: THE CHILD NODE THE CRASHED PARENT STARTED (`0`: none
    /// found): orphaned by a parent's SIGKILL, it reads the terminal while
    /// its old group is the foreground one, and nothing is typed while it
    /// does. On disk the key `gemini_child` is WRITTEN ONLY WHEN SET.
    pub(super) gemini_child: u32,
    /// A proven self-claimed codex-alab's relaunch (the crash rule,
    /// [`super::self_claimed`]): the resume line its own session record named,
    /// validated against the live process while it ran, typed as it is in
    /// place of the line the lane plans from `argv`. Empty: the lane's own
    /// plan.
    pub(super) resume_argv: Vec<String>,
    /// A self-claimed relaunch's `CODEX_HOME` in a record without
    /// [`Self::codex_home_env`]: a fallback, used when nonempty. New records
    /// carry that field too, which keeps an explicitly empty value.
    pub(super) resume_home: String,
    /// With `resume_argv`: the original process's kernel start, kept until its
    /// relaunch line is typed, so a deferred carry records the same once-only
    /// witness as an immediate relaunch.
    pub(super) resume_start: String,
    /// Text the lane typed into the composer and did not submit: on Codex, a
    /// guarded Enter that missed (`turn … reason=guard`); on Claude Code, a
    /// notice or release stopped part-way or with its guarded Enter unpressed
    /// ([`Typed::Cut`]). Kept so later visits say `left-typed` and never read
    /// it as a person's draft; neither lane takes it back ([`left_standing`]
    /// only reads). Carried across a retarget ([`St::for_target`]). Empty:
    /// nothing of the lane's there.
    pub(super) left_typed: String,
    /// When the agent a restart ended was seen gone (unix seconds): the Codex
    /// TUI `/exit` was typed into, or the Claude Code SIGTERM was sent to. `0`
    /// while it lives or nothing was sent. Stamped where the agent is seen
    /// gone ([`St::seen_gone`]), never at the signal. A move failing after it
    /// has no process to vet it by; its record is the tab's
    /// (`upgrade_status::Row::exited_at`).
    pub(super) exited_at: u64,
    /// The last look that found a hand the relaunch line waits on standing
    /// on the tab while this restart's line was still to type (unix seconds;
    /// `0`: none since its signal): a hold, another driver's lease, a turn
    /// ([`super::relaunch::hand_on_tab`]). The restart's stale clock does not
    /// run while one stands ([`super::relaunch::exit_clock`]): a hand is
    /// waited out, never timed out (LC-7 of the 2026-10-15 sweep: a hold just
    /// after the upgrade's SIGTERM expired the restart `stale-exit`, and
    /// nothing relaunched it once the hold lifted). Restamped at most every
    /// [`HAND_RESTAMP_S`] ([`St::saw_hand`]).
    pub(super) hand_seen_s: u64,
    /// When this round stopped ([`Phase::Failed`], unix seconds), stamped by
    /// every stop ([`St::fail`]) and by the void of a given-up round's late
    /// READY ([`St::void`]); the [`upgrade::RETRY_S`] rest counts from it
    /// ([`St::time_failed`], [`upgrade::Step::Rearm`]). `0` for other phases
    /// and for an older build's stop, which then reads as long ago and is
    /// re-armed at its first look.
    pub(super) failed_at: u64,
    /// When the latest notice was typed (unix seconds; `0`: none recorded). Not
    /// the re-ask clock, which a limit's episode ([`St::hold_clock`]) and a
    /// void's release ([`St::void`]) move on when nothing was asked, so the
    /// owner's "last asked" (`upgrade_status::Row::noticed_at`) reads this.
    /// Stamped by every notice of either lane, and carried onto a newer build's
    /// round by the retarget of a round that never restarted its agent. The
    /// Codex lane's quiet move reads it too: a TUI a notice once reached is
    /// carried on, not resumed as if never asked.
    pub(super) noticed_at: u64,
    /// Why the round before this one stopped, set by the re-arm that started
    /// this one ([`St::rearm`]; on a Codex record also by a retarget of a round
    /// that gave up, [`St::stop_kept_onto`]) and cleared by its first notice:
    /// the owner's view keeps a refusal's stall through the new round's first
    /// look (`upgrade_status::Row::last_stop`). It gates one thing: the Codex
    /// lane's quiet move of a round whose predecessor gave up is carried on, not
    /// resumed as if never asked.
    pub(super) last_stop: String,
    /// The same stop in a row ([`St::fail`]): why the latest stop happened and
    /// how many consecutive stops had that reason. A failure repeating every
    /// round rests longer each time ([`upgrade::rest_extension`]). Never
    /// terminal.
    pub(super) streak_why: String,
    pub(super) stop_streak: u32,
    /// The notices this process has been asked while behind, in every round:
    /// each ask counted once, when a notice first asks it ([`St::spend_ask`]).
    /// An ask the agent answered READY stays counted when that answer did not
    /// move it: "unanswered" in the owner's words means no ask has moved it.
    /// Carried and reset with the stop streak ([`St::history`]). The owner's
    /// "Asked N times" (`upgrade_status::Row::notices_asked`) reads this, not
    /// the round's own count, which a retarget restarts. An older record is read
    /// with the round's count ([`St::from_json`]).
    pub(super) notices: u32,
    /// The last look that saw the agent's own work hold the move (unix seconds;
    /// `0`: none): a wait word that is work, or processes under the agent
    /// ([`upgrade_status::holds_by_own_work`]). Never cleared by an act, which
    /// clears the wait and holders: the digest's class is the round's and would
    /// otherwise flip to quiet at every act. Read against
    /// [`upgrade_status::OWN_WORK_HOLD_S`].
    pub(super) own_work_at: u64,
    /// When the end of the agent's own work last asked (unix seconds, oldest
    /// first, at most [`upgrade::WORK_END_DAILY`]): the ask that end leads to is
    /// exempt from [`upgrade::MAX_ASKS`], so the driver bounds it, at most
    /// [`upgrade::WORK_END_DAILY`] in any [`upgrade::WORK_END_WINDOW_S`],
    /// [`upgrade::WORK_END_FLOOR_S`] apart ([`St::time_work`]).
    pub(super) work_end_at: Vec<u64>,
    /// A `/goal` was in force at the last look that read the screen (Claude
    /// Code's `◎ /goal active` footer): the owner-facing words name it as what
    /// keeps the agent busy. Gates nothing.
    pub(super) goal: bool,
    /// The first time each distinct wait word was recorded, for the record's
    /// life (at most [`FIRSTS_KEPT`]): the wait log's last [`WAITS_KEPT`] rows
    /// cover only hours, so a multi-day stall's first cause would scroll off
    /// it. Carried across a retarget while the same process is behind
    /// ([`St::adopt`]). Gates nothing.
    pub(super) firsts: Vec<(String, u64)>,
    // The watch's fields, none of which gates an act. A step runs only at a
    // point the session offers, so nothing in a step can notice that no point
    // comes: these say when the record last moved, who last looked at it off a
    // point, and which of the loop's guards withheld the last point, so a
    // deadline ([`due_by`], [`watch_at`]) is computed from the record alone.
    /// When the record's progress key ([`St::progress_key`]) last changed, in
    /// unix seconds: stamped by the one writer ([`save`]) and nothing else;
    /// [`restore`] puts an earlier record back without stamping
    /// ([`write_verbatim`]), so a rolled-back act never restarts the step's
    /// clock. A record an older build wrote reads as `max(at, failed_at, salt)`
    /// ([`St::from_json`]): past due at the first look of a build that watches.
    pub(super) progress_at: u64,
    /// The key [`St::progress_at`] was stamped for ([`St::progress_key`]):
    /// what the writer compares the record on disk with.
    pub(super) progress_key: String,
    /// When a look off a point (the watch) last read this record (unix
    /// seconds, `0`: never), and the aterm build that looked. Written by the
    /// watch alone; carried by every other save.
    pub(super) looked_at: u64,
    pub(super) looked_by: String,
    /// The last point the session's loop offered the host (an idle point or a
    /// settled break, unix seconds; `0`: none), and the guard that withheld
    /// the latest point it did not offer ([`crate::supervise::Guard::word`];
    /// empty: none). Copied from the window's worker by the watch; nothing
    /// here decides by them.
    pub(super) point_at: u64,
    pub(super) guard: String,
    /// The notice refused in a row: how many looks since the upgrade's last act
    /// could not type the notice (`wait:announce-refused:<why>`,
    /// [`St::note_step`]), first at [`St::refused_at`]. A refused notice spends
    /// no ask, so without this count nothing would follow. Any act resets it;
    /// other waits leave it. Past `upgrade_status::TELL_S` the owner is told
    /// (`upgrade_status::Row::refused_notice`).
    pub(super) refused: u32,
    /// When the first of those refusals was (unix seconds; `0`: none now).
    pub(super) refused_at: u64,
    /// THE AGENT'S OWN WORK WAS SEEN RUNNING since the round's latest notice —
    /// a look at a break of it, or one with processes under the agent — while
    /// the round is open (announced, or resting after it gave up). Cleared by
    /// every notice ([`St::announced`], set again by that very visit when its
    /// look saw work) and by a new round. What makes the first idle point with
    /// nothing under the agent THE END OF ITS WORK ([`St::time_work`],
    /// [`upgrade::Facts::work_ended`]): the point its "not yet" waited for.
    /// Recorded and read by Claude Code's lane alone; the owner's view reads
    /// it of a round that gave up (`upgrade_status::Row::work_seen`: a quiet
    /// give-up the agent's own work held, which `--now` does not move — round
    /// eight, #4's review, and its review).
    pub(super) work_seen: bool,
    /// The usage limit's named reset as the transcript said it at the last
    /// look that read it (unix seconds, [`upgrade::transcript_limit_reset`]:
    /// the reset a row names, never the wait
    /// [`upgrade::transcript_limit_until`] assigns a row naming none; `0`:
    /// none named or never read). Kept exactly as the latest read says, past
    /// or future, and cleared when the model answers again, by visit
    /// ([`queue_facts`]) and watch alike, so a later limit's reset is never an
    /// earlier one's. Gates
    /// nothing; the owner's view reads a `limited` wait by it
    /// (`upgrade_status::Row::limit_passed`).
    pub(super) limit_until: u64,
    /// The standing limit is a spend or credit wall
    /// ([`aterm_phase::WallKind::Spend`]), as the last look reading
    /// [`St::limit_until`] found it (Claude Code's transcript row,
    /// [`upgrade::transcript_limit_spend`]). Nothing ends it by itself, and
    /// with no named reset no week bounds it; only the account holder lifts
    /// it. Gates nothing; the owner's view reads it
    /// (`upgrade_status::Row::spend_held`).
    pub(super) limit_spend: bool,
    /// The restart under way is quiet ([`upgrade::Facts::quiet`]): nothing was
    /// typed before it and nothing is after; the agent was idle and resumes
    /// idle, the tab's loop deciding its next point as if nothing moved
    /// (`done:resumed`, `super::relaunch::carry_on`). Set with the signal from
    /// a round that typed nothing and owes no release; a round an older aterm
    /// or `upgrade_ask` announced is carried on.
    pub(super) quiet: bool,
    /// The restart's own last look recorded the wait: the reducer said go and
    /// [`restart`]'s later checks (the hand, `held`; the last look, `changed`,
    /// `changed-before-signal`; the signal, `signal-<why>`; the plan; the
    /// ownership) waited. A dry read stops at `would-restart`, so the host's
    /// watch keeps this word rather than record that nothing holds the move
    /// ([`St::held_past_go`]). Set by the visit that recorded it; cleared by
    /// every later step record ([`St::note_step`]) and the watch's own. The
    /// notice's fence needs no such mark: `announce-refused:<why>` is unique.
    pub(super) restart_look: bool,
}

/// Which arm of [`St::retarget`] made a record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Retarget {
    /// The record as it was: already on the build, or its model's.
    Kept,
    /// Moved onto the newer build in place (a round not yet
    /// restarting, for the same model, on the build the session still runs).
    InPlace,
    /// Rebuilt: a new pending round, any release the old notice owes carried.
    Anew,
    /// Rebuilt for a new process of a conversation whose round stopped because
    /// its target failed to start for it ([`St::failed_start`]), or after a
    /// restart on its own program that carried such a stop: the stop, its rest
    /// and its streak kept, so that build is not tried again until the rest
    /// runs out.
    Rested,
}

/// How a retarget left the round, in the ledger's words: the one wording of the
/// visit's row, the host's re-tab and the watch's (`retargeted:<from>-><to>`),
/// by the arm of [`St::retarget`] that made `st`. [`DroppedRide::say`] says it
/// too.
pub(super) fn retarget_how(how: Retarget, st: &St) -> &'static str {
    match (how, &st.phase) {
        (Retarget::Kept | Retarget::InPlace, Phase::Announced { .. }) => {
            "its notice and any READY still count"
        }
        (Retarget::Kept | Retarget::InPlace, Phase::Failed(_)) => "kept as it stopped",
        (Retarget::Kept | Retarget::InPlace, _) => "still pending",
        (Retarget::Anew, _) => "started over",
        (Retarget::Rested, _) => "kept stopped: that build failed to start for this conversation",
    }
}

/// How many distinct wait words [`St::firsts`] keeps.
pub const FIRSTS_KEPT: usize = 32;

/// How often a look that sees the agent's own work restamps
/// [`St::own_work_at`]: a quarter of the hold it feeds
/// ([`upgrade_status::OWN_WORK_HOLD_S`]), so the row (a summary to the window,
/// a wake on its GUI thread) does not change at every look.
pub const OWN_WORK_RESTAMP_S: u64 = upgrade_status::OWN_WORK_HOLD_S / 4;

/// How often a look that finds a hand on a restart's tab RESTAMPS
/// [`St::hand_seen_s`]: a fifth of the stale bound it holds off
/// ([`super::relaunch::STALE_S`]), so the record — and the row the window is
/// sent — changes at most once a minute under an hour's hold, and the line
/// still has the whole bound once the hand lifts, less this.
pub const HAND_RESTAMP_S: u64 = STALE_S / 5;

/// A build that failed to start for a conversation ([`St::failed_start`]): its
/// version and install ([`Source`]'s word), the instant the stop's rest counts
/// from ([`St::failed_at`]; the rest ends [`upgrade::RETRY_S`] past it) and the
/// stop in a row ([`St::stop_streak`]). What a new process of the conversation
/// keeps ([`St::keep_stop`]) and a restart on its own program carries
/// ([`St::kept_by_restart`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FailedStart {
    to: String,
    source: String,
    failed_at: u64,
    stop_streak: u32,
}

impl FailedStart {
    /// Whether this is the build `to`: its version, in its install.
    fn of(&self, to: &Candidate) -> bool {
        self.is(&to.version, to.source.as_str())
    }

    /// Whether this is build `version` in the install `source` names.
    fn is(&self, version: &Version, source: &str) -> bool {
        self.source == source && Version::parse(&self.to).as_ref() == Some(version)
    }

    /// Whether its rest still stands at `now`: the round it stopped re-arms
    /// [`upgrade::RETRY_S`] past [`St::failed_at`] ([`upgrade::retry_due`]).
    fn rests(&self, now: u64) -> bool {
        now < self.failed_at.saturating_add(upgrade::RETRY_S)
    }

    /// `<version> <source> <failed_at> <streak>`, as [`St::to_json`] writes it.
    fn word(&self) -> String {
        format!(
            "{} {} {} {}",
            self.to, self.source, self.failed_at, self.stop_streak
        )
    }

    /// [`FailedStart::word`] read back; anything else is none.
    fn parse(word: &str) -> Option<FailedStart> {
        let mut it = word.split_whitespace();
        let read = FailedStart {
            to: it.next()?.to_string(),
            source: it.next()?.to_string(),
            failed_at: it.next()?.parse().ok()?,
            stop_streak: it.next()?.parse().ok()?,
        };
        it.next().is_none().then_some(read)
    }
}

/// What one process has answered ([`St::history`]), carried onto the record
/// for the next build while it is the same process ([`St::adopt`]).
struct History {
    streak_why: String,
    stop_streak: u32,
    notices: u32,
    waits: Vec<WaitNote>,
    own_work_at: u64,
    firsts: Vec<(String, u64)>,
    work_end_at: Vec<u64>,
    goal: bool,
}

/// The Claude Code (or Codex) process a record is about, as the visit that
/// writes it reads it: its kernel start and that instant in unix seconds. What
/// [`St::for_target`] carries an age against and stamps ([`St::proc_start`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Proc<'a> {
    /// The kernel start, squashed; empty: unknown.
    pub(super) start: &'a str,
    /// The same, in unix seconds; `0`: unknown.
    pub(super) started_s: u64,
}
/// The slack past a step's own deadline before the watch reads its record
/// ([`due_by`]): the step that keeps the deadline runs at the session's next
/// point, and a point minutes late is no stall.
pub const WATCH_SLACK: u64 = 5 * 60;

/// The least time between two watches of one record ([`watch_at`]): the
/// re-ask interval, so a stuck record is read at most once per
/// [`upgrade::REASK_S`] and a waking machine climbs one step, not several.
pub const WATCH_GAP: u64 = upgrade::REASK_S;

impl St {
    /// Where the agent this record relaunches keeps its records, for a user
    /// whose home is `user_home`: under the Claude home its line carries
    /// ([`St::home`], [`Records::of`]), with the profile that home keeps
    /// apart ([`St::home_profile`]); a record with none, the user's own.
    pub(super) fn agent_records(&self, user_home: &Path) -> Records {
        Records::of_recorded(
            user_home,
            self.home.as_deref(),
            self.home_profile.as_deref(),
        )
    }

    /// Keeps the Claude home `records` names on this record, for a user whose
    /// home is `user_home`, as [`St::agent_records`] reads it back: its
    /// directory ([`St::home`], [`Records::home_word`]) and the profile it
    /// keeps apart ([`St::home_profile`], [`Records::profile_word`]). Every
    /// writer of the home writes both here. Whether either changed.
    pub(super) fn keep_home(&mut self, records: &Records, user_home: &Path) -> bool {
        let (home, profile) = (records.home_word(user_home), records.profile_word());
        let changed = self.home != home || self.home_profile != profile;
        self.home = home;
        self.home_profile = profile;
        changed
    }

    /// The relaunch line was typed at `at_s`: the phase, and the move's
    /// instant that outlives it ([`St::relaunched_at`]). Both lanes' one way
    /// into [`Phase::Relaunched`] after a line they typed.
    pub(super) fn relaunched(&mut self, at_s: u64) {
        self.phase = Phase::Relaunched { at_s };
        self.relaunched_at = at_s;
    }

    /// The progress key: what, changed, means the upgrade moved: the phase
    /// (its word carries asks and a stop's reason), the phase's stamp
    /// (`at_s`), the round's salt, the target and the owed release. Wait
    /// words, looks, held-by lists and the READY clock change while nothing
    /// moves and must not restart the step clock. Neither do refused notices
    /// ([`St::refused`], [`St::refused_at`]) or seen work ([`St::work_seen`]):
    /// counting a refusal would push back the deadline the watch needs to see
    /// that very failure. The notice that finally lands moves the phase.
    pub(super) fn progress_key(&self) -> String {
        let at = match &self.phase {
            Phase::Announced { at_s, .. }
            | Phase::Exiting { at_s }
            | Phase::Relaunched { at_s } => *at_s,
            Phase::Pending | Phase::Done | Phase::Failed(_) => 0,
        };
        format!(
            "{} at={at} salt={} to={} release={}",
            self.phase.word(),
            self.salt,
            self.to,
            self.release
        )
    }

    /// [`St::progress_at`] for a record written before it: the latest stamp
    /// the record carries of its own — the phase's `at_s`, the stop's
    /// `failed_at`, the round's salt.
    fn legacy_progress_at(&self) -> u64 {
        let at = match &self.phase {
            Phase::Announced { at_s, .. }
            | Phase::Exiting { at_s }
            | Phase::Relaunched { at_s } => *at_s,
            Phase::Pending | Phase::Done | Phase::Failed(_) => 0,
        };
        at.max(self.failed_at).max(self.salt)
    }

    /// The READY clock, stamped ([`upgrade::ready_since`]) and read into
    /// `facts` ([`upgrade::Facts::ready_s`]): how long the acted-on READY has
    /// stood, held at a limit, restarted by every notice ([`St::announced`]),
    /// cleared with the markers. Both drivers keep it: it bounds the agent's
    /// work under the answer (by an announced upgrade's re-ask, a given-up
    /// one's void).
    pub(super) fn time_ready(&mut self, ready: bool, facts: &mut Facts, now: u64) {
        self.ready_since = upgrade::ready_since(self.ready_since, ready, facts, now);
        if self.ready_since != 0 {
            facts.ready_s = now.saturating_sub(self.ready_since);
        }
    }

    /// The rest's clock read into `facts` ([`upgrade::Facts::failed_s`]): how
    /// long ago a stopped round stopped ([`St::failed_at`]); an unstamped older
    /// stop reads as long ago. Past [`upgrade::RETRY_S`] the reducer re-arms
    /// ([`upgrade::retry_due`]).
    pub(super) fn time_failed(&self, facts: &mut Facts, now: u64) {
        facts.failed_s = self.failed_for(now);
    }

    /// The end of the agent's own work, read into `facts`
    /// ([`upgrade::Facts::work_ended`]): its work was seen since the round's
    /// latest notice ([`St::work_seen`]) and this look is an idle point with
    /// nothing under the agent. Claude Code's lane alone.
    pub(super) fn time_work(&self, facts: &mut Facts, now: u64) {
        facts.work_ended = self.work_seen
            && self.open_round()
            && !facts.background_point
            && facts.background.is_empty()
            && self.work_end_open(now);
    }

    /// Whether the end of the agent's own work may ask now. That ask is exempt
    /// from the round's [`upgrade::MAX_ASKS`] and its clock, so it is bounded
    /// here, else an agent that answers "not yet" by starting short jobs is
    /// asked at the end of every one and the round never gives up:
    /// [`upgrade::WORK_END_FLOOR_S`] since the latest notice (for a resting
    /// round, since it stopped), and at most [`upgrade::WORK_END_DAILY`] asks in
    /// any window. Outside them the ordinary clock decides.
    fn work_end_open(&self, now: u64) -> bool {
        let since = match &self.phase {
            Phase::Announced { at_s, .. } => now.saturating_sub((*at_s).max(self.noticed_at)),
            // From the stop, not the end of a repeated stop's longer rest
            // (`failed_at` sits `rest_extension()` past the stop).
            Phase::Failed(_) => now.saturating_sub(self.stopped_at()),
            _ => return false,
        };
        since >= upgrade::WORK_END_FLOOR_S && self.work_end_left(now) > 0
    }

    /// How many asks the end of the agent's work has left: the day's budget
    /// less the asks of the last [`upgrade::WORK_END_WINDOW_S`] — a rolling
    /// window, so two bursts either side of a boundary are not twelve.
    fn work_end_left(&self, now: u64) -> u32 {
        let spent = self
            .work_end_at
            .iter()
            .filter(|&&at| now.saturating_sub(at) < upgrade::WORK_END_WINDOW_S)
            .count();
        upgrade::WORK_END_DAILY.saturating_sub(u32::try_from(spent).unwrap_or(u32::MAX))
    }

    /// One ask the end of the agent's work led to, spent at `now`.
    fn spend_work_end(&mut self, now: u64) {
        self.work_end_at
            .retain(|&at| now.saturating_sub(at) < upgrade::WORK_END_WINDOW_S);
        self.work_end_at.push(now);
        let keep = usize::try_from(upgrade::WORK_END_DAILY).unwrap_or(usize::MAX);
        if self.work_end_at.len() > keep {
            self.work_end_at.remove(0);
        }
    }

    /// A round the agent's own work may hold open: announced, or resting after
    /// it gave up ([`upgrade::GAVE_UP`]) — the rounds whose "not yet" the end
    /// of that work answers.
    fn open_round(&self) -> bool {
        matches!(&self.phase, Phase::Announced { .. })
            || matches!(&self.phase, Phase::Failed(why) if why == upgrade::GAVE_UP)
    }

    /// What this look saw of the agent's own work, kept for the end of it
    /// ([`St::work_seen`]): a break of it, or processes under the agent, while
    /// the round is open.
    fn saw_work(&mut self, facts: &Facts) {
        if self.open_round() && (facts.background_point || !facts.background.is_empty()) {
            self.work_seen = true;
        }
    }

    /// Seconds since this round stopped ([`St::time_failed`]); `0` for a
    /// round that has not.
    pub(super) fn failed_for(&self, now: u64) -> u64 {
        if matches!(self.phase, Phase::Failed(_)) {
            now.saturating_sub(self.failed_at)
        } else {
            0
        }
    }

    /// The model an announced round promised ([`St::model_list`] of a
    /// [`Phase::Announced`] round; `None` otherwise): the model its READY
    /// consented to, which a retarget onto a newer build keeps. Visit and watch
    /// both hand it to [`St::for_target`], so the watch makes the record what
    /// the next visit would. Only while it still rides: a visit whose read bars
    /// it ([`models::ride_bar`]) drops it ([`ride_dropped`]).
    pub(super) fn promised_model(&self) -> Option<&str> {
        (matches!(self.phase, Phase::Announced { .. }) && !self.model_list.is_empty())
            .then_some(self.model_list.as_str())
    }

    /// The model a notice of this round carried, which a restart of the round
    /// would still ask for ([`St::model_list`]): an announced round's
    /// ([`St::promised_model`]) or a gave-up one's (a late READY still restarts
    /// it, [`St::give_up`]); `None` otherwise. Every visit re-judges it
    /// ([`models::ride_bar`]).
    pub(super) fn carried_model(&self) -> Option<&str> {
        let restarts = match &self.phase {
            Phase::Announced { .. } => true,
            Phase::Failed(why) => why == upgrade::GAVE_UP,
            _ => false,
        };
        (restarts && !self.model_list.is_empty()).then_some(self.model_list.as_str())
    }

    /// An announced round left with nothing to move at `now` (its model's ride
    /// dropped, [`ride_dropped`], the build current and nothing else due): the
    /// agent it asked to wind down is released ([`St::owe_release`]) and a new
    /// pending round begins, its markers forgotten so no READY to it acts, as
    /// the owner's `--skip`/`--defer` ends an announcement. Whether it was
    /// announced.
    fn release_unridden(&mut self, now: u64) -> bool {
        if !matches!(self.phase, Phase::Announced { .. }) {
            return false;
        }
        self.owe_release("model-dropped");
        self.new_round(now);
        self.phase = Phase::Pending;
        self.forget_markers();
        self.noted.clear();
        true
    }

    fn fresh(from: &Version, to: &Candidate, who: Proc<'_>, now: u64) -> St {
        St {
            from: from.to_string(),
            to: to.version.to_string(),
            source: to.source.as_str().to_string(),
            salt: now,
            seq_since_s: now,
            pending_since: now,
            proc_start: who.start.to_string(),
            ..St::default()
        }
    }

    /// Whether the age this record carries belongs to the process now holding
    /// the conversation (`who`'s kernel start; `running`: the build it runs). By
    /// the process's own start when both are known; for a record an older build
    /// wrote, by the build it was minted behind (a session that runs another
    /// build was restarted since). A record naming neither is carried.
    fn same_process(&self, running: &Version, who: Proc<'_>) -> bool {
        if !self.proc_start.is_empty() && !who.start.is_empty() {
            return self.proc_start == who.start;
        }
        Version::parse(&self.from).is_none_or(|from| from == *running)
    }

    /// The build that failed to start for this record's conversation: a Claude
    /// Code relaunch onto it that never registered the conversation
    /// (`no-resume`: the new process crashed at start or refused the
    /// conversation, and its shell had the tab back), this stopped record's own
    /// target, else the one a restart's record keeps ([`St::failed_start_kept`]).
    /// The failure is the conversation's on that build, never its process's
    /// alone as every other stop's is. A relaunch that kept its program
    /// ([`SAME_PROGRAM`]) is no such failure.
    pub(super) fn failed_start(&self) -> Option<FailedStart> {
        match self.agent {
            upgrade::Agent::Claude => {}
            // Muse Code and agy: their relaunches type the managed twin, and
            // Gemini CLI's the shell's own `gemini`, never a build a move
            // chose, so no build failed to start for a conversation of
            // theirs.
            upgrade::Agent::Codex
            | upgrade::Agent::Muse
            | upgrade::Agent::Agy
            | upgrade::Agent::Gemini => return None,
        }
        match &self.phase {
            Phase::Failed(why) if why == "no-resume" && self.source != SAME_PROGRAM => {
                Some(FailedStart {
                    to: self.to.clone(),
                    source: self.source.clone(),
                    failed_at: self.failed_at,
                    stop_streak: self.stop_streak,
                })
            }
            _ => self.failed_start_kept.clone(),
        }
    }

    /// The stop a new process of the conversation keeps: `old` stopped because
    /// its target failed to start for the conversation ([`St::failed_start`]),
    /// or carries such a stop over a restart on its own program. A person
    /// following the stop's remedy (`claude --resume <id>`) starts a new
    /// process, and a fresh round would move it onto the same broken build. So
    /// the stop, its rest's start ([`St::failed_at`]) and its streak
    /// ([`St::stop_streak`]) are kept: that build is not tried again until the
    /// rest runs out or a newer build is offered. The owner's `--now` re-arms
    /// only a give-up.
    fn keep_stop(&mut self, kept: FailedStart) {
        self.phase = Phase::Failed("no-resume".to_string());
        self.failed_at = kept.failed_at;
        "no-resume".clone_into(&mut self.streak_why);
        self.stop_streak = kept.stop_streak;
    }

    /// THE MOVE'S RELAUNCH ENDED AS IT STARTED, TAKEN BACK ONTO THE BUILD ITS
    /// AGENT RAN (LC-5 of ) at `now`: a Claude Code move's
    /// line (onto another image than the agent's) was seen to run and end
    /// with nothing registering the conversation, its shell holding the tab
    /// again — the build answered its probe and fails only at a real start (a
    /// crash in its interface, a resume it refuses). Stopped there, that one
    /// working agent stayed down until a person resumed it, where the
    /// harness is to need nobody.
    ///
    /// The failure is recorded as the stop it replaces would record it
    /// ([`St::fail`], `no-resume`) and KEPT ([`St::failed_start_kept`]), so
    /// the build's note counts it ([`build_start::note`]) and the
    /// conversation's next round onto that build rests as the stop's would
    /// ([`St::retarget`]). The record becomes a relaunch on the agent's own
    /// program ([`SAME_PROGRAM`], onto [`Self::from`]) whose line is the line
    /// back ([`St::back_line`]), asking no model the move asked
    /// ([`St::model_list`] cleared), in flight again ([`Phase::Exiting`] at
    /// `now`, its prompt marked afresh): the relaunch types it under every
    /// guard a relaunch line has, and carries the agent on as it would have
    /// on the new build. Taken once: a line back that ends as it started too
    /// is the stop, as before. `false`, nothing changed, where there is no
    /// line back, the record already runs its own program, or the agent is
    /// not a Claude Code.
    pub(super) fn roll_back(&mut self, now: u64) -> bool {
        let claude = match self.agent {
            upgrade::Agent::Claude => true,
            upgrade::Agent::Codex
            | upgrade::Agent::Agy
            | upgrade::Agent::Muse
            | upgrade::Agent::Gemini => false,
        };
        if !claude || self.back_line.is_empty() || self.source == SAME_PROGRAM {
            return false;
        }
        self.fail("no-resume", now);
        self.failed_start_kept = self.failed_start();
        SAME_PROGRAM.clone_into(&mut self.source);
        self.to.clone_from(&self.from);
        self.line = std::mem::take(&mut self.back_line);
        self.model_list.clear();
        self.prompt = None;
        self.phase = Phase::Exiting { at_s: now };
        true
    }

    /// What a process has answered, carried onto the next build's record while
    /// the same process is behind: the stops in a row, the wait log, the first
    /// time each wait was seen, the last look that saw its work, the day's asks
    /// at the end of that work and the goal in force.
    fn history(&self) -> History {
        History {
            // The stop in a row, whatever its reason: every `Failed` reason is
            // about the process or its launch line, so the streak follows the
            // process across a new build.
            streak_why: self.streak_why.clone(),
            stop_streak: self.stop_streak,
            // The notices it was asked ride with the stops they ended in.
            notices: self.notices,
            waits: self.waits.clone(),
            own_work_at: self.own_work_at,
            firsts: self.firsts.clone(),
            work_end_at: self.work_end_at.clone(),
            goal: self.goal,
        }
    }

    /// Take a process's [`History`] onto this fresh record.
    fn adopt(&mut self, history: History) {
        self.streak_why = history.streak_why;
        self.stop_streak = history.stop_streak;
        self.notices = history.notices;
        self.waits = history.waits;
        self.own_work_at = history.own_work_at;
        self.firsts = history.firsts;
        self.work_end_at = history.work_end_at;
        self.goal = history.goal;
    }

    /// The state for moving onto `to` (and, when the model rule moves the
    /// conversation, onto `model`): the prior one when it names that version
    /// and, once announced, that model; else a fresh one that keeps what still
    /// holds (how long the session has been behind, the owner's word) and,
    /// whatever has begun, the model a bucket's fallback replaced
    /// ([`St::fallback_from`]), so the way back still has a model to return to.
    /// A skip is of one version. An announcement that named another model is
    /// announced again: its READY was consent to a different restart. A round
    /// not yet restarting, for the same model, moves onto a newer build in
    /// place, its READY kept.
    fn for_target(
        prior: Option<St>,
        from: &Version,
        to: &Candidate,
        model: Option<&str>,
        who: Proc<'_>,
        now: u64,
    ) -> St {
        Self::retarget(prior, from, to, model, who, now).0
    }

    /// [`St::for_target`], plus which of its arms made the record
    /// ([`Retarget`]), for [`retarget_how`]'s ledger wording (phase and marker
    /// alone cannot tell a rebuilt pending record from one kept in place).
    /// Aterm's own text left in the prompt ([`St::left_typed`]) rides every
    /// arm.
    fn retarget(
        prior: Option<St>,
        from: &Version,
        to: &Candidate,
        model: Option<&str>,
        who: Proc<'_>,
        now: u64,
    ) -> (St, Retarget) {
        let left = prior
            .as_ref()
            .map(|p| p.left_typed.clone())
            .unwrap_or_default();
        // The Claude home the conversation's agent keeps its records under
        // ([`St::home`]) is the conversation's, whatever arm makes the record:
        // a visit binds it to the agent that holds it now.
        let home = prior
            .as_ref()
            .map(|p| (p.home.clone(), p.home_profile.clone()));
        let (mut st, how) = match prior {
            // Reused when still Pending, for the same model, or Failed: every
            // `Failed` reason concerns the process or its launch line, so the
            // round rests for the build whatever model would ride it
            // (re-minting on a model comparison would repeat the same refusal
            // every sweep). The reducer re-arms it after `upgrade::RETRY_S`
            // (`St::rearm`).
            Some(mut st)
                if st.to == to.version.to_string()
                    && (matches!(st.phase, Phase::Pending | Phase::Failed(_))
                        || st.model_list == model.unwrap_or_default())
                    // A stopped round belongs to its process too: a session the
                    // owner restarted by hand onto this build is minted afresh
                    // below, its predecessor's owed release carried.
                    && !(matches!(st.phase, Phase::Failed(_)) && !st.same_process(from, who)) =>
            {
                // Managed and native builds can share a version, and which one
                // the session moves to follows its launch: until the restart
                // begins, the label names the build the sweep would start now.
                if matches!(st.phase, Phase::Pending | Phase::Announced { .. }) {
                    st.source = to.source.as_str().to_string();
                }
                // A pending record follows its process: a session the owner
                // restarted by hand onto another build is behind since then.
                let same = st.same_process(from, who);
                if st.phase == Phase::Pending && !same {
                    st.pending_since = now;
                    // Its minting too: the classifier's horizons count from the
                    // salt ([`upgrade_status::Row::minted_at`]). Kept past every
                    // marker the old salt could have made ([`St::new_round`]).
                    st.salt = now.max(
                        st.salt
                            .saturating_add(u64::from(upgrade::MAX_ASKS))
                            .saturating_add(1),
                    );
                    st.from = from.to_string();
                    st.wait.clear();
                    st.wait_since = 0;
                    st.wait_seen = 0;
                    st.held_by.clear();
                    // What the old process answered is not this one's
                    // ([`St::history`]): its stop streak and refusals would
                    // read a hand restart as a stop that repeats, and tell the
                    // owner again to do what they just did.
                    st.stop_streak = 0;
                    st.streak_why.clear();
                    st.notices = 0;
                    st.last_stop.clear();
                    st.refused = 0;
                    st.refused_at = 0;
                    st.waits.clear();
                    st.firsts.clear();
                    st.own_work_at = 0;
                    st.work_end_at.clear();
                    st.goal = false;
                } else if st.proc_start.is_empty()
                    && who.started_s != 0
                    && st.pending_since != 0
                    && st.pending_since < st.salt
                    && st.pending_since < who.started_s
                {
                    // A record an older build wrote whose age predates both its
                    // own minting and the process now holding the conversation:
                    // the age begins at the later of the salt and the process's
                    // start.
                    st.pending_since = st.salt.max(who.started_s);
                }
                // The record takes the holder's start only where the age it
                // carries is the holder's (or is reset above): an announced record
                // of another process keeps its own, so the arm below mints the
                // next build's record afresh.
                if !who.start.is_empty() && (same || st.phase == Phase::Pending) {
                    st.proc_start = who.start.to_string();
                }
                (st, Retarget::Kept)
            }
            // A READY covers any version: a round not yet restarting, for the
            // same model, on the build the session still runs and for the process
            // the round was about ([`St::same_process`]), is retargeted in place
            // onto a newer build, keeping its phase, asks, notice, marker and any
            // READY (a READY says "my work is at a stopping point", not "move me
            // to that version"). A changed model is a new consent (the next arm);
            // a `--skip` names one build and does not follow ([`St::word_onto`]).
            Some(mut st)
                if matches!(st.phase, Phase::Pending | Phase::Announced { .. })
                    && st.model_list == model.unwrap_or_default()
                    && Version::parse(&st.from).as_ref() == Some(from)
                    && st.same_process(from, who) =>
            {
                st.to = to.version.to_string();
                st.source = to.source.as_str().to_string();
                // A record an older build wrote learns its process here.
                if st.proc_start.is_empty() && !who.start.is_empty() {
                    st.proc_start = who.start.to_string();
                }
                // The same process, as this arm's guard says.
                let word = st.word_onto(to, now, true);
                (st.request, st.request_tab, st.request_at) = word;
                (st, Retarget::InPlace)
            }
            // A notice for another build or model is abandoned: the agent it
            // reached is owed its release, carried onto the new state until a new
            // notice supersedes it. Its stamp ([`St::noticed_at`]) and a give-up
            // before it ([`St::stop_kept_onto`]) tell the Codex lane, which types
            // no release, that a notice once reached that TUI.
            Some(mut old) if matches!(old.phase, Phase::Pending | Phase::Announced { .. }) => {
                old.owe_release("retargeted");
                // The age is the process's, not the build's: carried only while
                // the same process is still behind.
                let same = old.same_process(from, who);
                let pending_since = if same { old.behind_since() } else { now };
                let history = same.then(|| old.history());
                let fallback_from = fallback_origin(&old);
                let (request, request_tab, request_at) = old.word_onto(to, now, same);
                // What this process was told and how its round ended ride with it
                // only ([`St::stop_kept_onto`]).
                let (noticed_at, last_stop) = if same {
                    (old.noticed_at, old.stop_kept_onto())
                } else {
                    (0, String::new())
                };
                let mut st = St {
                    pending_since,
                    request,
                    request_tab,
                    request_at,
                    fallback_from,
                    tab: old.tab,
                    release: old.release,
                    asked: old.asked,
                    notice_pid: old.notice_pid,
                    notice_start: old.notice_start,
                    noticed_at,
                    last_stop,
                    ..St::fresh(from, to, who, now)
                };
                if let Some(history) = history {
                    st.adopt(history);
                }
                (st, Retarget::Anew)
            }
            // A stopped upgrade's owed release follows onto the new one, with the
            // notice's fence (typed only to the process that notice reached,
            // [`release_void`]). The same process, still behind
            // ([`St::same_process`]), keeps its time behind (a round that gives up
            // and re-arms for ever must not restart its clock with each build), stop
            // streak, history ([`St::history`]) and the Codex lane's notice stamp; a
            // process the round was not about is behind from now, asked afresh. The
            // owner's word follows as [`St::word_onto`] says.
            Some(old) if matches!(old.phase, Phase::Failed(_)) => {
                let same = old.same_process(from, who);
                // A build that failed to start for the conversation stays failed
                // for it ([`St::keep_stop`]); its own process's stop is kept whole
                // by the first arm.
                let rested = old.failed_start().filter(|f| f.of(to));
                let history = same.then(|| old.history());
                let pending_since = if same { old.behind_since() } else { now };
                let fallback_from = fallback_origin(&old);
                let (request, request_tab, request_at) = old.word_onto(to, now, same);
                let (noticed_at, last_stop) = if same {
                    (old.noticed_at, old.stop_kept_onto())
                } else {
                    (0, String::new())
                };
                let mut st = St {
                    pending_since,
                    request,
                    request_tab,
                    request_at,
                    fallback_from,
                    release: old.release,
                    asked: old.asked,
                    tab: old.tab,
                    notice_pid: old.notice_pid,
                    notice_start: old.notice_start,
                    noticed_at,
                    last_stop,
                    ..St::fresh(from, to, who, now)
                };
                if let Some(history) = history {
                    st.adopt(history);
                }
                if let Some(kept) = rested {
                    st.keep_stop(kept);
                    (st, Retarget::Rested)
                } else {
                    (st, Retarget::Anew)
                }
            }
            // A finished move, or a restart's own record (a relaunch, a restart
            // in place). One that kept the session on its own program (`source`
            // [`SAME_PROGRAM`], the session still on that build) moved nothing,
            // so what it kept ([`St::kept_by_restart`]) still stands: the time
            // behind, the owner's hold off this build, and a build that failed to
            // start for the conversation. A finished or under-way move begins a
            // new wait, its word gone.
            Some(old) => {
                let kept =
                    old.source == SAME_PROGRAM && Version::parse(&old.to).as_ref() == Some(from);
                let hold = (kept && old.request.holds_build(&to.version.to_string(), now))
                    .then(|| (old.request.clone(), old.request_tab.clone(), old.request_at));
                let rested = old.failed_start().filter(|f| f.of(to));
                let mut st = St {
                    fallback_from: fallback_origin(&old),
                    release: old.release,
                    asked: old.asked,
                    tab: old.tab,
                    notice_pid: old.notice_pid,
                    notice_start: old.notice_start,
                    ..St::fresh(from, to, who, now)
                };
                if kept && old.pending_since != 0 {
                    st.pending_since = old.pending_since;
                }
                if let Some((request, request_tab, request_at)) = hold {
                    (st.request, st.request_tab, st.request_at) =
                        (request, request_tab, request_at);
                }
                if let Some(kept) = rested {
                    st.keep_stop(kept);
                    (st, Retarget::Rested)
                } else {
                    (st, Retarget::Anew)
                }
            }
            None => (St::fresh(from, to, who, now), Retarget::Anew),
        };
        if st.left_typed.is_empty() {
            st.left_typed = left;
        }
        if st.home.is_none()
            && let Some((home, profile)) = home
        {
            st.home = home;
            st.home_profile = profile;
        }
        (st, how)
    }

    /// The owner's word as it applies to this conversation in `tab`: none
    /// when the owner named another tab. `--now` waives a wait for the tab
    /// the owner looked at; resumed elsewhere, the conversation is asked
    /// afresh.
    pub(super) fn request_for(&self, tab: &str) -> Request {
        if self.request_tab.is_empty() || self.request_tab == tab {
            self.request.clone()
        } else {
            Request::None
        }
    }

    /// The owner's word a retarget onto `to` keeps of this record at `now`
    /// ([`St::retarget`]'s in-place, abandoning and stopped arms; `same`: the
    /// process holding the conversation is the one the record was about). A
    /// word is the session's: `--defer` follows it to the next build and to a
    /// process restarted by hand, or the next point would announce inside the
    /// snooze. A standing `--now` waives the wait for a person at the tab, so
    /// it follows only the process it hurried: a session just restarted by hand
    /// is one a person is at. A skip names one build; one of `to` itself still
    /// holds ([`Request::holds_build`]), whatever process holds the conversation.
    fn word_onto(&self, to: &Candidate, now: u64, same: bool) -> (Request, String, u64) {
        match &self.request {
            Request::Skip(_) if !self.request.holds_build(&to.version.to_string(), now) => {
                (Request::None, String::new(), 0)
            }
            Request::Now if !same => (Request::None, String::new(), 0),
            word => (word.clone(), self.request_tab.clone(), self.request_at),
        }
    }

    /// The stop before the round a retarget of this record begins
    /// ([`St::last_stop`], [`St::retarget`]'s abandoning and stopped arms), a
    /// Codex record's alone: a round that gave up is that stop, as the rest's
    /// re-arm would record it ([`St::rearm`]); any other keeps the stop before
    /// it. The Codex lane types no release, so this and the notice's stamp
    /// ([`St::noticed_at`]) carry the TUI's quiet move on after a give-up rather
    /// than resume it as if never asked.
    fn stop_kept_onto(&self) -> String {
        match self.agent {
            upgrade::Agent::Codex => {}
            // Muse Code, agy and Gemini CLI: no upgrade round of their own
            // (relaunch on exit only).
            upgrade::Agent::Claude
            | upgrade::Agent::Muse
            | upgrade::Agent::Agy
            | upgrade::Agent::Gemini => return String::new(),
        }
        match &self.phase {
            Phase::Failed(why) if why == upgrade::GAVE_UP => why.clone(),
            _ => self.last_stop.clone(),
        }
    }

    /// The owner's hold this record carries for `tab` ([`St::request_for`]): a
    /// `--skip` or a `--defer` ([`Request::holds_build`]); `None` for no word,
    /// and for `--now`, which hurries a move and holds none.
    pub(super) fn owners_hold(&self, tab: &str) -> Option<Request> {
        match self.request_for(tab) {
            word @ (Request::Skip(_) | Request::DeferUntil(_)) => Some(word),
            Request::None | Request::Now => None,
        }
    }

    /// Whether this record keeps the conversation in `tab` off the build `to` at
    /// `now`, whatever its round: the owner's hold ([`St::owners_hold`]), or a
    /// build that failed to start for the conversation while its stop's rest
    /// stands ([`St::failed_start`]). A relaunch on exit never targets such a
    /// build ([`super::relaunch::relaunch_target`]), however the agent ended.
    pub(super) fn holds_off(&self, tab: &str, to: &Candidate, now: u64) -> bool {
        self.owners_hold(tab)
            .is_some_and(|word| word.holds_build(&to.version.to_string(), now))
            || self
                .failed_start()
                .is_some_and(|failed| failed.of(to) && failed.rests(now))
    }

    /// What a relaunch onto a build that failed to start for the conversation
    /// keeps of this record, written onto its own record `st` (`st.to`,
    /// `st.source`): the stop in a row, so that build failing again is a stop
    /// that repeats ([`St::fail`]) and rests longer. A relaunch on exit reaches
    /// that build only once the rest has run out ([`St::holds_off`]).
    pub(super) fn retried_by(&self, st: &mut St) {
        let Some(build) = Version::parse(&st.to) else {
            return;
        };
        if let Some(failed) = self.failed_start().filter(|f| f.is(&build, &st.source)) {
            "no-resume".clone_into(&mut st.streak_why);
            st.stop_streak = failed.stop_streak;
        }
    }

    /// What a restart or relaunch of the conversation on its own program keeps
    /// of this record in `tab`, written onto its own record `st` (neither moved
    /// the session, so neither is over): the owner's hold ([`St::owners_hold`]);
    /// the time behind ([`St::behind_since`]) only of a record about the build
    /// the restart keeps ([`St::build_behind`]), else its old clock would start
    /// the next round at the ladder's last rung; and a build that failed to
    /// start for the conversation ([`St::failed_start`]).
    pub(super) fn kept_by_restart(&self, tab: &str, st: &mut St) {
        if let Some(word) = self.owners_hold(tab) {
            st.request = word;
            st.request_tab.clone_from(&self.request_tab);
            st.request_at = self.request_at;
        }
        st.failed_start_kept = self.failed_start();
        let behind = match &self.phase {
            Phase::Pending | Phase::Announced { .. } => self.is_upgrade(),
            Phase::Failed(_) => self.is_upgrade() || self.source == SAME_PROGRAM,
            Phase::Exiting { .. } | Phase::Relaunched { .. } | Phase::Done => {
                self.source == SAME_PROGRAM
            }
        };
        let same_build = self
            .build_behind()
            .is_some_and(|build| Version::parse(&st.from).as_ref() == Some(&build));
        if behind && same_build && self.pending_since != 0 {
            st.pending_since = self.pending_since;
        }
    }

    /// The build this record says the session is behind on
    /// ([`St::kept_by_restart`]): an upgrade round's `from`, any other record's
    /// `to` (the build its restart or relaunch brought the session up on).
    /// `None` where that is no version.
    fn build_behind(&self) -> Option<Version> {
        Version::parse(if self.is_upgrade() {
            &self.from
        } else {
            &self.to
        })
    }

    /// A new round of this upgrade, armed afresh by the owner's word
    /// ([`upgrade_status::ask`]). The salt is minted past every marker the old
    /// one could have made (`salt + asks`, asks at most [`upgrade::MAX_ASKS`]),
    /// so no READY given before the word answers a notice typed after it. The
    /// time behind is kept; the refused-notice count ([`St::refused`]) starts
    /// over, so an `Upgrade now` on a refused-notice row does not read the same
    /// stall at once.
    fn new_round(&mut self, now: u64) {
        if self.pending_since == 0 {
            self.pending_since = self.salt;
        }
        self.refused = 0;
        self.refused_at = 0;
        self.salt = now.max(
            self.salt
                .saturating_add(u64::from(upgrade::MAX_ASKS))
                .saturating_add(1),
        );
    }

    /// Since when the session has been behind: [`Self::pending_since`], or
    /// the second the state was minted for a state an older build wrote.
    pub(super) fn behind_since(&self) -> u64 {
        if self.pending_since == 0 {
            self.salt
        } else {
            self.pending_since
        }
    }

    /// Where this upgrade stands on the ladder at `now` ([`upgrade::rung`] of
    /// time behind, [`Self::behind_since`]). A state with no known start (no
    /// record, no salt) is at the first rung, since an unknown age relaxes
    /// nothing. The visit reads the ladder's clock here ([`ladder_look`]), the
    /// Land rung included (with `--now`); as a read of the clock,
    /// `HarnessUpgradeLadder::Advance` is the clock's step, aged through this
    /// reader in its Tier-1 bind (`upgrade_ladder_claude_tests.rs`).
    pub(super) fn rung_at(&self, now: u64) -> upgrade::Rung {
        match self.behind_since() {
            0 => upgrade::Rung::Prefer,
            since => upgrade::rung(now.saturating_sub(since)),
        }
    }

    /// The repaint-proof settle ([`upgrade::Facts::still_s`]): carries this
    /// look's `print` of the last words ([`upgrade::still_print`]; `None` for a
    /// look not authoritatively idle) into the run of looks at the same print,
    /// each within [`IDLE_RUN_GAP_S`] of the last, and returns the seconds since
    /// the run began. A new print, a longer gap, or a non-idle look restarts it
    /// (`0`). Repaints that leave the words (a footer clock, a goal counter, a
    /// dim suggestion) are not in the print and restart nothing.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "HarnessUpgradeLadder",
            action = "Look",
            project = "aterm_agent::harness::upgrade_drive::ladder_projection"
        )
    )]
    pub(super) fn still(&mut self, print: Option<String>, now: u64) -> u64 {
        let Some(print) = print else {
            self.still_print.clear();
            (self.still_since, self.still_at) = (0, 0);
            return 0;
        };
        let run = self.still_since != 0
            && self.still_print == print
            && now.saturating_sub(self.still_at) <= IDLE_RUN_GAP_S;
        if !run {
            self.still_print = print;
            self.still_since = now;
        }
        self.still_at = now;
        now.saturating_sub(self.still_since)
    }

    /// Record the wait `word` a sweep found, keeping when it began while the
    /// same word repeats.
    pub(super) fn note_wait(&mut self, word: &str, now: u64) {
        if self.wait != word {
            self.wait = word.to_string();
            self.wait_since = now;
            self.log_wait(word, now);
        }
        self.wait_seen = if word == "attended" { now } else { 0 };
        // The word alone says the agent's own work holds the move (a turn, a
        // shell, a workflow): stamped at every look, never by an act.
        if upgrade_status::holds_by_own_work(&self.wait, false) {
            self.saw_own_work(now);
        }
    }

    /// A hand the relaunch line waits on was seen on the restart's tab at
    /// `now` ([`St::hand_seen_s`]): restamped at most every
    /// [`HAND_RESTAMP_S`]. Returns whether the record changed.
    pub(super) fn saw_hand(&mut self, now: u64) -> bool {
        let due = self.hand_seen_s == 0 || now.saturating_sub(self.hand_seen_s) >= HAND_RESTAMP_S;
        if due {
            self.hand_seen_s = now;
        }
        due
    }

    /// The agent's own work was seen holding the move at `now`
    /// ([`St::own_work_at`], restamped at most every [`OWN_WORK_RESTAMP_S`]).
    fn saw_own_work(&mut self, now: u64) {
        if self.own_work_at == 0 || now.saturating_sub(self.own_work_at) >= OWN_WORK_RESTAMP_S {
            self.own_work_at = now;
        }
    }

    /// Add `word` to the wait log ([`St::waits`]), the oldest going past
    /// [`WAITS_KEPT`].
    fn log_wait(&mut self, word: &str, now: u64) {
        self.waits.push(WaitNote {
            at: now,
            word: word.to_string(),
            note: String::new(),
        });
        if self.waits.len() > WAITS_KEPT {
            self.waits.remove(0);
        }
        if self.firsts.len() < FIRSTS_KEPT && !self.firsts.iter().any(|(w, _)| w == word) {
            self.firsts.push((word.to_string(), now));
        }
    }

    /// Attach the facts a look decided on to the wait log's newest row, when
    /// that row is this look's (`now`): only a change of wait wrote one.
    fn annotate_wait(&mut self, now: u64, note: &str) {
        if let Some(last) = self.waits.last_mut()
            && last.at == now
            && last.note.is_empty()
        {
            last.note = super::one_line(note, 240);
        }
    }

    /// What a visit's report says about waiting: a `wait:` step is recorded
    /// ([`upgrade::wait_word`], with Claude's `status` beside `not-idle`); an
    /// act clears it; `skip:`/`busy:`/`would-` change nothing. A
    /// `held-back:<why>` here is the first sweep of a wait said once
    /// ([`attended_once`]: `held-back:attended`, then `wait:attended`), so it is
    /// recorded as that wait, never as an act.
    fn note_step(&mut self, step: &str, status: &str, now: u64) {
        // Whose word this is, the visit says after (`St::restart_look`).
        self.restart_look = false;
        if refused(step) {
            if self.refused == 0 {
                self.refused_at = now;
            }
            self.refused = self.refused.saturating_add(1);
        }
        if let Some(why) = step
            .strip_prefix("wait:")
            .or_else(|| step.strip_prefix("held-back:"))
        {
            self.note_wait(&upgrade::wait_word(why, status), now);
        } else if !step.starts_with("skip:") && !step.starts_with("would-") {
            if !self.wait.is_empty() || step.starts_with("announced") {
                self.log_wait(step, now);
            }
            self.wait.clear();
            self.wait_since = 0;
            self.wait_seen = 0;
            // An act: the notice owed is typed, or no longer owed.
            self.refused = 0;
            self.refused_at = 0;
        }
    }

    /// Whether the recorded wait is one the act's own last checks recorded, past
    /// where a dry read of that act stops (`would-announce` before the notice's
    /// fence, `announce-refused:<why>`; `would-restart…` before [`restart`]'s
    /// hand, last look and signal, [`St::restart_look`]). The dry read cannot see
    /// what held it, and recording `upgrade::NEXT_POINT` would drop the `TELL_S`
    /// warning (`Row::refused_notice`). A read of another act gets its own word.
    pub(super) fn held_past_go(&self, read: &str) -> bool {
        let act = read.split_once(AT_POINT).map_or(read, |(act, _)| act);
        if act == "would-announce" {
            refused(&format!("wait:{}", self.wait))
        } else {
            (act == "would-restart" || act.starts_with("would-restart:")) && self.restart_look
        }
    }

    /// What holds the move, for the owner ([`St::held_by`]): what runs under the
    /// agent at a look whose step waited (`held`, named by [`Kernel::describe`]
    /// at `now`), by pid, name and start, never its command. An act, or a look
    /// finding nothing running, clears it.
    fn note_held(&mut self, step: &str, held: &[upgrade::Held], now: u64) {
        if !step.starts_with("wait:") {
            self.held_by.clear();
            return;
        }
        // `ps` ages to the second at a different moment than `now`: keep the
        // start a process already had so it reads the same at every look.
        let kept = |pid: u32, name: &str, since: u64| {
            self.held_by
                .iter()
                .find(|o| o.pid == pid && o.name == name && o.since.abs_diff(since) <= 2)
                .map_or(since, |o| o.since)
        };
        self.held_by = held
            .iter()
            .map(|h| HeldBy {
                pid: h.pid,
                name: h.name.clone(),
                since: kept(h.pid, &h.name, now.saturating_sub(h.age_s)),
            })
            .collect();
        // Processes under the agent at a look that waited: its own work,
        // remembered past the acts that clear the list ([`St::own_work_at`]).
        if upgrade_status::holds_by_own_work(&self.wait, !self.held_by.is_empty()) {
            self.saw_own_work(now);
        }
    }

    fn to_json(&self) -> String {
        let (name, at, asks, why) = match &self.phase {
            Phase::Pending => ("pending", 0, 0, ""),
            Phase::Announced { at_s, asks } => ("announced", *at_s, *asks, ""),
            Phase::Exiting { at_s } => ("exiting", *at_s, 0, ""),
            Phase::Relaunched { at_s } => ("relaunched", *at_s, 0, ""),
            Phase::Done => ("done", 0, 0, ""),
            Phase::Failed(w) => ("failed", 0, 0, w.as_str()),
        };
        let mut o = Map::new();
        for (k, v) in [
            ("phase", name),
            ("why", why),
            ("from", &self.from),
            ("to", &self.to),
            ("source", &self.source),
            ("marker", &self.marker),
            ("notice_start", &self.notice_start),
            ("relaunched_start", &self.relaunched_start),
            ("tab", &self.tab),
            ("line", &self.line),
            (
                "prompt",
                &self
                    .prompt
                    .as_ref()
                    .map(PromptMark::word)
                    .unwrap_or_default(),
            ),
            ("noted", &self.noted),
            ("cause", &self.cause),
            ("model_before", &self.model_before),
            ("launch_model", &self.launch_model),
            ("model_list", &self.model_list),
            (
                "fallback_from",
                match self.fallback_from.as_deref() {
                    None => "",
                    Some("") => FALLBACK_NO_MODEL,
                    Some(m) => m,
                },
            ),
            ("resumed_on", &self.resumed_on),
            ("wait", &self.wait),
            ("request", &self.request.word()),
            ("request_tab", &self.request_tab),
            ("outcome", &self.outcome),
            ("agent", self.agent.word()),
            ("thread", &self.thread),
            ("codex_home", &self.codex_home),
            ("cwd", &self.cwd),
            ("twin", &self.twin),
            ("resume_home", &self.resume_home),
            ("resume_start", &self.resume_start),
            ("left_typed", &self.left_typed),
            ("release", &self.release),
            ("last_stop", &self.last_stop),
            ("streak_why", &self.streak_why),
            ("still_print", &self.still_print),
            ("proc_start", &self.proc_start),
            ("progress_key", &self.progress_key),
            ("looked_by", &self.looked_by),
            ("guard", &self.guard),
        ] {
            o.insert(k.into(), Value::from(v));
        }
        if let Some(login) = &self.login {
            o.insert("login".into(), Value::from(login.as_str()));
        }
        if let Some(home) = &self.home {
            o.insert("home".into(), Value::from(home.as_str()));
        }
        if let Some(home) = &self.codex_home_env {
            o.insert("codex_home_env".into(), Value::from(home.as_str()));
        }
        if !self.muse_data.is_empty() {
            o.insert("muse_data".into(), Value::from(self.muse_data.as_str()));
        }
        if !self.agy_config.is_empty() {
            o.insert("agy_config".into(), Value::from(self.agy_config.as_str()));
        }
        if !self.gemini_chats.is_empty() {
            o.insert(
                "gemini_chats".into(),
                Value::from(self.gemini_chats.as_str()),
            );
        }
        if self.gemini_child != 0 {
            o.insert(
                "gemini_child".into(),
                Value::from(u64::from(self.gemini_child)),
            );
        }
        if let Some(profile) = &self.home_profile {
            o.insert("home_profile".into(), Value::from(profile.as_str()));
        }
        if let Some(failed) = &self.failed_start_kept {
            o.insert("failed_start_kept".into(), Value::from(failed.word()));
        }
        if !self.back_line.is_empty() {
            o.insert("back_line".into(), Value::from(self.back_line.as_str()));
        }
        if let Some(block) = self.line_block {
            o.insert("line_block".into(), Value::from(block));
        }
        if self.moved_at != 0 {
            o.insert("moved_at".into(), Value::from(self.moved_at));
        }
        if let Some(login) = &self.moved_login {
            o.insert("moved_login".into(), Value::from(login.as_str()));
        }
        if let Some(model) = &self.moved_left_model {
            let model = if model.is_empty() {
                FALLBACK_NO_MODEL
            } else {
                model.as_str()
            };
            o.insert("moved_left_model".into(), Value::from(model));
        }
        if !self.look_word.is_empty() {
            o.insert("look_word".into(), Value::from(self.look_word.as_str()));
        }
        for (k, v) in [
            ("at", at),
            ("asks", u64::from(asks)),
            ("salt", self.salt),
            ("last_seq", self.last_seq),
            ("seq_since", self.seq_since_s),
            ("pid", u64::from(self.pid)),
            ("notice_pid", u64::from(self.notice_pid)),
            ("shell", u64::from(self.shell)),
            ("mark", self.mark),
            ("confirm_by", self.confirm_by),
            ("resumed_pid", u64::from(self.resumed_pid)),
            ("relaunched_pid", u64::from(self.relaunched_pid)),
            ("relaunched_at", self.relaunched_at),
            ("hold_since", self.hold_since_s),
            ("hold_seen", self.hold_seen_s),
            ("ready_since", self.ready_since),
            ("pending_since", self.pending_since),
            ("wait_since", self.wait_since),
            ("wait_seen", self.wait_seen),
            ("done_at", self.done_at),
            ("request_at", self.request_at),
            ("exited_at", self.exited_at),
            ("hand_seen", self.hand_seen_s),
            ("idle_looks", u64::from(self.idle_looks)),
            ("idle_pid", u64::from(self.idle_pid)),
            ("idle_at", self.idle_at),
            ("failed_at", self.failed_at),
            ("noticed_at", self.noticed_at),
            ("stop_streak", u64::from(self.stop_streak)),
            ("notices", u64::from(self.notices)),
            ("still_since", self.still_since),
            ("still_at", self.still_at),
            ("progress_at", self.progress_at),
            ("looked_at", self.looked_at),
            ("point_at", self.point_at),
            ("refused", u64::from(self.refused)),
            ("refused_at", self.refused_at),
            ("work_seen", u64::from(self.work_seen)),
            ("own_work_at", self.own_work_at),
            ("goal", u64::from(self.goal)),
            ("limit_until", self.limit_until),
            ("quiet", u64::from(self.quiet)),
            ("limit_spend", u64::from(self.limit_spend)),
            ("restart_look", u64::from(self.restart_look)),
        ] {
            o.insert(k.into(), Value::from(v));
        }
        let held_by: Vec<String> = self
            .held_by
            .iter()
            .map(|h| format!("{} {} {}", h.pid, h.name, h.since))
            .collect();
        let waits: Vec<String> = self
            .waits
            .iter()
            .map(|w| format!("{} {} {}", w.at, w.word, w.note))
            .collect();
        let firsts: Vec<String> = self
            .firsts
            .iter()
            .map(|(word, at)| format!("{at} {word}"))
            .collect();
        let work_end_at: Vec<String> = self.work_end_at.iter().map(u64::to_string).collect();
        let tabs_until: Vec<String> = self
            .tabs_until
            .iter()
            .map(|(tab, until)| format!("{until} {tab}"))
            .collect();
        for (k, list) in [
            ("argv", &self.argv),
            ("muse_env", &self.muse_env),
            ("agy_env", &self.agy_env),
            ("gemini_env", &self.gemini_env),
            ("tabs", &self.tabs),
            ("tabs_until", &tabs_until),
            ("resume_argv", &self.resume_argv),
            ("markers", &self.markers),
            ("asked", &self.asked),
            ("held_by", &held_by),
            ("waits", &waits),
            ("firsts", &firsts),
            ("work_end_at", &work_end_at),
        ] {
            if !list.is_empty() {
                o.insert(
                    k.into(),
                    Value::Array(list.iter().map(|a| Value::from(a.as_str())).collect()),
                );
            }
        }
        aterm_json::to_string(&Value::Object(o)).unwrap_or_default()
    }

    fn from_json(text: &str) -> Option<St> {
        let v: Value = aterm_json::from_str(text).ok()?;
        let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let n = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
        let small = |k: &str| u32::try_from(n(k)).unwrap_or(0);
        let list = |k: &str| -> Vec<String> {
            v.get(k)
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default()
        };
        let phase = match s("phase").as_str() {
            "pending" => Phase::Pending,
            "announced" => Phase::Announced {
                at_s: n("at"),
                asks: u32::try_from(n("asks")).unwrap_or(u32::MAX),
            },
            "exiting" => Phase::Exiting { at_s: n("at") },
            "relaunched" => Phase::Relaunched { at_s: n("at") },
            "done" => Phase::Done,
            "failed" => Phase::Failed(s("why")),
            _ => return None,
        };
        // A record with no word is Claude Code's, as every record before the
        // word was. A word this build does not know is a newer build's
        // record, and so is one of an agent it has no lane for (read as that
        // agent, whose lanes are not written here; since Gemini CLI's lane,
        // every agent has one): no record here, so every scan of the
        // directory passes its file by as it lies. A Muse Code's, an agy's
        // and a Gemini CLI's are their relaunch lanes'.
        let agent = match v.get("agent") {
            None => upgrade::Agent::Claude,
            Some(word) => upgrade::Agent::parse(word.as_str()?)?,
        };
        if super::lanes::lanes(agent.program()) == super::lanes::Lanes::NONE {
            return None;
        }
        // A record an older build wrote mid-move has no `relaunched_at`: its
        // phase's own stamp is the line's instant, so the move it finishes
        // after an update still says when it happened. A Done record of that
        // build has lost it with the phase, and keeps `0`.
        let relaunched_at = match (&phase, v.get("relaunched_at")) {
            (Phase::Relaunched { at_s }, None) => *at_s,
            _ => n("relaunched_at"),
        };
        let mut st = St {
            phase,
            from: s("from"),
            to: s("to"),
            source: s("source"),
            marker: s("marker"),
            markers: list("markers"),
            release: s("release"),
            asked: list("asked"),
            salt: n("salt"),
            last_seq: n("last_seq"),
            seq_since_s: n("seq_since"),
            pid: small("pid"),
            notice_pid: small("notice_pid"),
            notice_start: s("notice_start"),
            shell: small("shell"),
            tab: s("tab"),
            line: s("line"),
            back_line: s("back_line"),
            prompt: PromptMark::parse(&s("prompt")),
            line_block: v.get("line_block").and_then(Value::as_u64),
            noted: s("noted"),
            cause: s("cause"),
            model_before: s("model_before"),
            mark: n("mark"),
            launch_model: s("launch_model"),
            model_list: s("model_list"),
            fallback_from: match s("fallback_from") {
                m if m.is_empty() => None,
                m if m == FALLBACK_NO_MODEL => Some(String::new()),
                m => Some(m),
            },
            login: v.get("login").and_then(Value::as_str).map(str::to_owned),
            home: v.get("home").and_then(Value::as_str).map(str::to_owned),
            home_profile: v
                .get("home_profile")
                .and_then(Value::as_str)
                .map(str::to_owned),
            failed_start_kept: v
                .get("failed_start_kept")
                .and_then(Value::as_str)
                .and_then(FailedStart::parse),
            confirm_by: n("confirm_by"),
            look_word: s("look_word"),
            resumed_on: s("resumed_on"),
            resumed_pid: small("resumed_pid"),
            relaunched_pid: small("relaunched_pid"),
            relaunched_start: s("relaunched_start"),
            relaunched_at,
            moved_at: n("moved_at"),
            moved_login: v
                .get("moved_login")
                .and_then(Value::as_str)
                .map(str::to_owned),
            moved_left_model: v.get("moved_left_model").and_then(Value::as_str).map(|m| {
                if m == FALLBACK_NO_MODEL {
                    String::new()
                } else {
                    m.to_owned()
                }
            }),
            tabs: list("tabs"),
            tabs_until: list("tabs_until")
                .iter()
                .filter_map(|w| {
                    let (until, tab) = w.split_once(' ')?;
                    Some((tab.to_string(), until.parse().ok()?))
                })
                .collect(),
            hold_since_s: n("hold_since"),
            hold_seen_s: n("hold_seen"),
            ready_since: n("ready_since"),
            pending_since: n("pending_since"),
            wait: s("wait"),
            wait_since: n("wait_since"),
            wait_seen: n("wait_seen"),
            // `<pid> <name> <since>` each; a malformed one is left out.
            held_by: list("held_by")
                .iter()
                .filter_map(|h| {
                    let mut it = h.split_whitespace();
                    let held = HeldBy {
                        pid: it.next()?.parse().ok()?,
                        name: it.next()?.to_string(),
                        since: it.next()?.parse().ok()?,
                    };
                    it.next().is_none().then_some(held)
                })
                .collect(),
            idle_looks: small("idle_looks"),
            idle_pid: small("idle_pid"),
            idle_at: n("idle_at"),
            request: Request::parse(&s("request")).unwrap_or_default(),
            request_tab: s("request_tab"),
            request_at: n("request_at"),
            outcome: s("outcome"),
            done_at: n("done_at"),
            agent,
            thread: s("thread"),
            codex_home: s("codex_home"),
            codex_home_env: v
                .get("codex_home_env")
                .and_then(Value::as_str)
                .map(str::to_owned),
            argv: list("argv"),
            cwd: s("cwd"),
            twin: s("twin"),
            muse_data: s("muse_data"),
            muse_env: list("muse_env"),
            agy_config: s("agy_config"),
            agy_env: list("agy_env"),
            gemini_chats: s("gemini_chats"),
            gemini_env: list("gemini_env"),
            gemini_child: small("gemini_child"),
            resume_argv: list("resume_argv"),
            resume_home: s("resume_home"),
            resume_start: s("resume_start"),
            left_typed: s("left_typed"),
            exited_at: n("exited_at"),
            hand_seen_s: n("hand_seen"),
            failed_at: n("failed_at"),
            noticed_at: n("noticed_at"),
            last_stop: s("last_stop"),
            streak_why: s("streak_why"),
            stop_streak: small("stop_streak"),
            notices: small("notices"),
            still_print: s("still_print"),
            still_since: n("still_since"),
            still_at: n("still_at"),
            own_work_at: n("own_work_at"),
            proc_start: s("proc_start"),
            // `<at> <word> <note>` each; a malformed one is left out.
            waits: list("waits")
                .iter()
                .filter_map(|w| {
                    let mut it = w.splitn(3, ' ');
                    Some(WaitNote {
                        at: it.next()?.parse().ok()?,
                        word: it.next()?.to_string(),
                        note: it.next().unwrap_or_default().to_string(),
                    })
                })
                .collect(),
            work_end_at: list("work_end_at")
                .iter()
                .filter_map(|t| t.parse().ok())
                .take(usize::try_from(upgrade::WORK_END_DAILY).unwrap_or(usize::MAX))
                .collect(),
            goal: n("goal") != 0,
            firsts: list("firsts")
                .iter()
                .filter_map(|w| {
                    let (at, word) = w.split_once(' ')?;
                    Some((word.to_string(), at.parse().ok()?))
                })
                .take(FIRSTS_KEPT)
                .collect(),
            progress_at: n("progress_at"),
            progress_key: s("progress_key"),
            looked_at: n("looked_at"),
            looked_by: s("looked_by"),
            point_at: n("point_at"),
            guard: s("guard"),
            refused: small("refused"),
            refused_at: n("refused_at"),
            work_seen: n("work_seen") != 0,
            limit_until: n("limit_until"),
            quiet: n("quiet") != 0,
            limit_spend: n("limit_spend") != 0,
            restart_look: n("restart_look") != 0,
        };
        // A record from before the progress stamp ([`St::progress_at`]): its
        // key is what it holds now and it last moved no later than its own
        // latest stamp, so the writer stamps it only once it changes and an
        // old record is past due at once.
        if v.get("progress_key").is_none() {
            st.progress_key = st.progress_key();
            st.progress_at = st.legacy_progress_at();
        }
        // A record from before the count ([`St::notices`]): what its round
        // shows — whole rounds given up, and this round's asks.
        if v.get("notices").is_none() {
            st.notices = upgrade_status::round_notices(&st.phase, &st.streak_why, st.stop_streak);
        }
        Some(st)
    }

    pub(super) fn in_flight(&self) -> bool {
        matches!(self.phase, Phase::Exiting { .. } | Phase::Relaunched { .. })
    }

    /// THE PROCESS THIS RESTART'S RELAUNCH BROUGHT UP IS GONE ALREADY: the
    /// one it found or adopted ([`St::relaunched_pid`]) is no process `alive`
    /// finds. The ONE rule both lanes' relaunch on exit closes such a record
    /// in flight by, as one overtaken (`relaunch::after_exit_as`): the
    /// window's host tries an exit again on the agent before it when the
    /// agent its relaunch started ended before anything read it, and carried
    /// on, that try held the tab for a dead process until the record went
    /// stale, the harness's hand on the tab throughout.
    pub(super) fn relaunched_gone(&self, alive: impl Fn(u32) -> bool) -> bool {
        self.relaunched_pid != 0 && !alive(self.relaunched_pid)
    }

    /// THIS RESTART IN FLIGHT, OVERTAKEN, IS CLOSED at `now` for `why`: an
    /// exit of ANOTHER process than the one it names — the agent its
    /// relaunch brought up, or a later one — or of one it brought up gone
    /// already ([`Self::relaunched_gone`]). Never the end of the agent this
    /// record names, which its relaunch had already seen gone: no failure
    /// after ITS exit ([`Self::exited_at`] cleared,
    /// `upgrade_status::Row::failed_after_exit`), so the owner's view vets
    /// the stop against a holder as any stop's. The ONE close both lanes'
    /// relaunch on exit makes of such a record (`relaunch::after_exit_as`),
    /// so a relaunched agent's own
    /// orderly exit never reads as the upgrade's failure.
    pub(super) fn close_overtaken(&mut self, why: &str, now: u64) {
        self.exited_at = 0;
        self.stop(why, now);
    }

    /// The relaunch this record stopped as never picked up, picked up after all:
    /// a Claude Code restart that ended its agent and stopped `no-resume` after
    /// that exit (a reading a slow start or an early read makes wrong), whose
    /// conversation `sf` holds now in a process its recorded shell started
    /// ([`relaunch_of`]) that can be the line's own ([`Self::started_by_line`]).
    /// Only a final stop (`relaunch::ends_at_once`); the live relaunch on exit's
    /// own loop retries its stop. A parent that cannot be read now is no landing.
    pub(super) fn landed_after_stop(&self, sf: &SessionFile, k: &dyn Kernel) -> bool {
        match self.agent {
            upgrade::Agent::Claude => {}
            // Muse Code, agy and Gemini CLI: a stopped relaunch of theirs is
            // its lane's, and a Claude Code session file says nothing of it.
            upgrade::Agent::Codex
            | upgrade::Agent::Muse
            | upgrade::Agent::Agy
            | upgrade::Agent::Gemini => return false,
        }
        matches!(&self.phase, Phase::Failed(why) if why == "no-resume")
            && self.exited_at != 0
            && super::relaunch::ends_at_once(&self.cause)
            && self.started_by_line(sf)
            && relaunch_of(k, sf, &sf.session_id, self.pid, self.shell) == Relaunch::Ours
    }

    /// Whether `sf`'s process can be the one this stopped record's line started,
    /// by what the stop knew: it started no later than the stop
    /// ([`Self::stopped_at`]) and runs the build the line ran ([`St::to`], where
    /// known). The stop's own remedy is a person's `claude --resume <id>` at that
    /// very shell, typed only after the stop, so a later start or another build
    /// is that hand resume, never the line's. Claude's own record of the start
    /// (`procStart`) is read; a stop with no stamp or a start not recorded is
    /// none.
    fn started_by_line(&self, sf: &SessionFile) -> bool {
        let stop = self.stopped_at();
        stop != 0
            && models::parse_lstart(&sf.proc_start).is_some_and(|start| start <= stop)
            && (self.to.is_empty() || self.to == "-" || sf.version == self.to)
    }

    /// [`Self::landed_after_stop`]'s record reopened at `now`: in flight again
    /// as a relaunch that landed ([`Phase::Relaunched`]), and the stop taken
    /// back (no rest to count from, [`St::failed_at`], and not counted as a
    /// repeat, [`St::stop_streak`]).
    pub(super) fn reopen_landed(&mut self, now: u64) {
        if let Phase::Failed(why) = &self.phase
            && *why == self.streak_why
        {
            self.stop_streak = self.stop_streak.saturating_sub(1);
            if self.stop_streak == 0 {
                self.streak_why.clear();
            }
        }
        self.phase = Phase::Relaunched { at_s: now };
        self.failed_at = 0;
    }

    /// Whether this record is a relaunch of agent `pid`'s own exit
    /// ([`super::relaunch::CAUSE_EXIT`], or [`super::relaunch::CAUSE_FRESH`]
    /// with nothing to resume), filed only once an attempt of that exit read it
    /// as a crash. Both lanes judge a later attempt of the same exit that read
    /// nothing ([`super::relaunch::ExitRecord::Unread`]) by it: by then what the
    /// crash left is gone, and read again it would say a graceful exit.
    pub(super) fn relaunched_on_exit_of(&self, pid: u32) -> bool {
        self.pid == pid
            && (self.cause == super::relaunch::CAUSE_EXIT
                || self.cause == super::relaunch::CAUSE_FRESH)
    }

    /// A restart under way that has not moved at `now`: `exiting`, `exited` or
    /// `relaunched` past [`STALE_S`] ([`upgrade_status::stuck_on`], shared with
    /// the owner's view so the ledger's note, [`note_stuck`], and `--status`'s
    /// `stalled=stuck:<what>` agree); `None` otherwise.
    pub(super) fn stuck(&self, now: u64) -> Option<&'static str> {
        upgrade_status::stuck_on(&self.phase, self.exited_at, now)
    }

    /// Whether this record is an upgrade's (listed in the owner's view), not a
    /// relaunch's filed beside it ([`St::cause`]): an agent relaunched after an
    /// exit, memory or model restart is no upgrade; the upgrade's own afresh
    /// restart of a taskless conversation
    /// ([`super::relaunch::CAUSE_UPGRADE_FRESH`]) is.
    pub(super) fn is_upgrade(&self) -> bool {
        self.cause.is_empty() || self.cause == super::relaunch::CAUSE_UPGRADE_FRESH
    }

    /// The signalled agent seen gone at `now` ([`St::exited_at`]): kept from the
    /// first look that saw it, never stamped while [`St::pid`] runs. Returns
    /// whether it was newly seen. Gone is exited, reaped or not
    /// ([`super::upgrade_wake::wait_exit`], asked without a wait): the SIGTERMed
    /// agent is a zombie until its shell reaps it, which `kill(pid, 0)` still
    /// finds.
    pub(super) fn seen_gone(&mut self, now: u64) -> bool {
        if self.exited_at != 0
            || self.pid == 0
            || !super::upgrade_wake::wait_exit(self.pid, Duration::ZERO)
        {
            return false;
        }
        self.exited_at = now;
        true
    }

    /// THE RESTART'S EXIT WAS CARRIED THROUGH A HAND THAT HELD ITS TAB
    /// (`relaunch::carry_restart`'s `through_hand`, LC-7 of ):
    /// a Claude Code's relaunch still to type, its agent gone, counts its
    /// stale bound from `now` — the moment its tab was free to type at again
    /// — as from an exit just seen ([`Self::seen_gone`]). A Codex lane keeps
    /// its own. Whether anything moved.
    pub(super) fn carried_through_hand(&mut self, now: u64) -> bool {
        let claude = match self.agent {
            upgrade::Agent::Claude => true,
            // Muse Code, agy and Gemini CLI: each lane keeps its own clock
            // (as Codex's does), their records never a restart's.
            upgrade::Agent::Codex
            | upgrade::Agent::Muse
            | upgrade::Agent::Agy
            | upgrade::Agent::Gemini => false,
        };
        if !claude
            || !matches!(self.phase, Phase::Exiting { .. })
            || self.pid == 0
            || !super::upgrade_wake::wait_exit(self.pid, Duration::ZERO)
        {
            return false;
        }
        self.exited_at = now;
        true
    }

    /// Whether a typed continuation's model is still to be read
    /// ([`super::relaunch::confirm`]), or a quiet restart's look is still
    /// open ([`St::confirm_by`]).
    pub(super) fn confirming(&self) -> bool {
        self.phase == Phase::Done && self.confirm_by != 0
    }

    fn notice_belongs_to(&self, sf: &SessionFile, tab: &str) -> bool {
        self.tab == tab
            && self.notice_pid == sf.pid
            && !self.notice_start.is_empty()
            && self.notice_start == squash(&sf.proc_start)
    }

    // The upgrade's own transitions: what every step that types, gives up,
    // voids or stops does to the record, one place each so the release a
    // notice owes is never forgotten on some path
    // (`harness_upgrade_never_strands_model` is bound to these).

    /// Whether a notice of this round of a Claude upgrade reached an agent
    /// still alive to hold for it: announced, or given up. A restart past its
    /// signal has no such agent (its conversation is the carry-on's, or a
    /// person's who resumed it elsewhere, where "nothing will restart this
    /// session" would be false). A relaunch record ([`St::cause`]) typed no
    /// notice, and a Codex record is its own lane's.
    fn notified(&self) -> bool {
        driven_here(self.agent)
            && self.cause.is_empty()
            && match &self.phase {
                Phase::Announced { .. } => true,
                Phase::Failed(why) => why == upgrade::GAVE_UP,
                Phase::Pending | Phase::Exiting { .. } | Phase::Relaunched { .. } | Phase::Done => {
                    false
                }
            }
    }

    /// Whether this phase acts on a READY to the round's markers: announced, or
    /// given up but still hearing a late answer ([`upgrade::next_step`]). No
    /// other phase may let one hold the release ([`upgrade::gate_release`],
    /// [`heard`]).
    fn hears_ready(&self) -> bool {
        match &self.phase {
            Phase::Announced { .. } => true,
            Phase::Failed(why) => why == upgrade::GAVE_UP,
            _ => false,
        }
    }

    /// The READY markers of this round ([`St::marker`], [`St::markers`]), the
    /// ones an answer counts for ([`answered`]); none typed yet, or forgotten by
    /// a void or a release, is none.
    pub(super) fn round_markers(&self) -> impl Iterator<Item = &String> {
        std::iter::once(&self.marker)
            .chain(&self.markers)
            .filter(|m| !m.is_empty())
    }

    /// The upgrade abandons its notice for `why`: one release line is owed
    /// ([`St::release`]), once per abandonment with the first reason kept, if a
    /// notice of this round reached the agent.
    pub(super) fn owe_release(&mut self, why: &str) {
        if self.release.is_empty() && self.notified() {
            self.release = why.to_string();
        }
    }

    /// The upgrade stops at `now` for `why` (a refusal, a failed signal or
    /// relaunch, a conversation resumed elsewhere): [`Phase::Failed`] stamped
    /// ([`St::fail`]; the round rests [`upgrade::RETRY_S`], then re-arms), the
    /// owed release if the agent was asked, and the round's markers forgotten:
    /// no stopped phase acts on a READY, and a standing READY would otherwise
    /// hold the release (`wait:release:ready`) for ever.
    pub(super) fn stop(&mut self, why: &str, now: u64) {
        self.owe_release(why);
        self.forget_markers();
        self.fail(why, now);
    }

    /// The round stops at `now` for `why`: [`Phase::Failed`] and the stamp its
    /// rest counts from ([`St::failed_at`]). Every stop of either lane comes
    /// through here.
    pub(super) fn fail(&mut self, why: &str, now: u64) {
        if self.streak_why == why {
            self.stop_streak = self.stop_streak.saturating_add(1);
        } else {
            why.clone_into(&mut self.streak_why);
            self.stop_streak = 1;
        }
        self.phase = Phase::Failed(why.to_string());
        // The rest counts from here; a stop that repeats pushes it later
        // ([`Self::rest_extension`]).
        self.failed_at = now.saturating_add(self.rest_extension());
    }

    /// When this stopped round stopped (unix seconds): [`St::failed_at`] less the
    /// longer rest a stop that repeats carries ([`Self::rest_extension`]), or the
    /// void's instant that began the rest again ([`Self::void`]). `0` for a stop
    /// with no stamp (an older build's) and every other phase.
    pub(super) fn stopped_at(&self) -> u64 {
        if self.failed_at == 0 || !matches!(self.phase, Phase::Failed(_)) {
            return 0;
        }
        self.failed_at.saturating_sub(self.rest_extension())
    }

    /// How much later than [`upgrade::RETRY_S`] the next round starts, for the
    /// stop the streak is on: a whole round the agent declined by its own ladder
    /// ([`upgrade::unanswered_rest_extension`]), every other repeating stop by
    /// [`upgrade::rest_extension`]. One function for the stop ([`Self::fail`])
    /// and the void that begins the rest again ([`Self::void`]), so one rest
    /// never stretches by two ladders.
    fn rest_extension(&self) -> u64 {
        if self.streak_why == upgrade::GAVE_UP {
            upgrade::unanswered_rest_extension(self.stop_streak)
        } else {
            upgrade::rest_extension(self.stop_streak)
        }
    }

    /// A new round of a stopped upgrade ([`upgrade::Step::Rearm`]) at `now`:
    /// pending again with a fresh salt ([`St::new_round`], so no READY to the
    /// stopped round answers the new one), markers forgotten, asks reset, the
    /// owner's `--now` spent, the stop's stamp cleared and why it stopped kept
    /// as [`St::last_stop`] until the new round's first notice. Everything of
    /// the old restart goes (prompt position, line block, exit and pids), so a
    /// later stop never reads as one after that old exit. A release still owed
    /// stays owed; a relaunch's record re-armed is the upgrade's own from here.
    /// Answers why the round had stopped (`rearmed:<why>`).
    pub(super) fn rearm(&mut self, now: u64) -> String {
        let why = match std::mem::take(&mut self.phase) {
            Phase::Failed(why) => why,
            other => other.word(),
        };
        self.last_stop.clone_from(&why);
        self.forget_markers();
        self.new_round(now);
        self.failed_at = 0;
        self.prompt = None;
        self.line_block = None;
        self.exited_at = 0;
        self.resumed_pid = 0;
        self.relaunched_pid = 0;
        self.relaunched_start.clear();
        self.noted.clear();
        // An older record kept a fallback's origin in its cause alone: kept
        // past the cause.
        self.fallback_from = fallback_origin(self);
        self.cause.clear();
        self.hold_since_s = 0;
        self.hold_seen_s = 0;
        self.work_seen = false;
        if self.request == Request::Now {
            self.request = Request::None;
            self.request_tab.clear();
            self.request_at = 0;
        }
        why
    }

    /// The restart's record, written before its SIGTERM ([`restart`]): the
    /// agent `pid`, the `shell` whose prompt returns in `tab`, the relaunch
    /// `line`, and [`Phase::Exiting`] at `now`, consuming the READY it acts on
    /// (the markers, no longer honoured once signalled) and any release owed
    /// by an earlier notice (the carry-on says continue). Returns what was
    /// taken, for [`St::unsent`] if the signal is never sent.
    pub(super) fn signalled(
        &mut self,
        pid: u32,
        shell: u32,
        tab: &str,
        line: String,
        now: u64,
    ) -> Unsent {
        let back = Unsent {
            phase: std::mem::replace(&mut self.phase, Phase::Exiting { at_s: now }),
            release: std::mem::take(&mut self.release),
            marker: std::mem::take(&mut self.marker),
            markers: std::mem::take(&mut self.markers),
            fallback_from: self.fallback_from.clone(),
        };
        self.pid = pid;
        self.shell = shell;
        tab.clone_into(&mut self.tab);
        self.line = line;
        // A new signal: no line back until its restart plans one
        // ([`St::back_line`]).
        self.back_line.clear();
        // A new signal: its agent is not seen gone yet, nor a hand on its
        // line.
        self.exited_at = 0;
        self.hand_seen_s = 0;
        back
    }

    /// The signal a restart [`St::signalled`] recorded was never sent (the
    /// session changed, the server refused): the phase, READY, release and
    /// fallback origin it took are put back as they were.
    pub(super) fn unsent(&mut self, back: Unsent) {
        self.phase = back.phase;
        self.release = back.release;
        self.marker = back.marker;
        self.markers = back.markers;
        self.fallback_from = back.fallback_from;
        self.back_line.clear();
    }

    /// The kernel refused the signal of a recorded restart at `now`: the agent
    /// answered READY and lives on, stopped, so the round stops
    /// (`signal-refused`, [`St::stop`]) and the agent is owed its release.
    pub(super) fn signal_failed(&mut self, back: Unsent, now: u64) {
        self.unsent(back);
        self.stop("signal-refused", now);
    }

    /// [`Step::GiveUp`] at `now`: the round stops asking ([`upgrade::GAVE_UP`],
    /// stamped by [`St::fail`]), the agent is owed its release, and the markers
    /// stay honoured until the release is typed (a READY first restarts it);
    /// [`upgrade::RETRY_S`] later a new round asks ([`St::rearm`]). The owner's
    /// `--now` is spent with this round, so a late READY hours later restarts
    /// under the ordinary waits, not with the settle and attended guard waived.
    fn give_up(&mut self, now: u64) {
        self.owe_release("gave-up");
        self.fail(upgrade::GAVE_UP, now);
        if self.request == Request::Now {
            self.request = Request::None;
            self.request_tab.clear();
            self.request_at = 0;
        }
    }

    /// [`Step::Void`] at `now`: the READY a person held past the drain (or, for
    /// an upgrade that gave up asking, the agent's own background work) is
    /// void: the markers forgotten, the agent owed its release, and an
    /// announced upgrade's re-ask clock restarted so the next notice comes
    /// [`upgrade::REASK_S`] after the release. A gave-up upgrade's rest begins
    /// again at the void, stretched as a repeating stop's
    /// ([`Self::rest_extension`]) and never sooner than the rest it already
    /// earned. A lane that types no release (Codex) keeps its clock.
    fn void(&mut self, now: u64) {
        self.owe_release("void");
        self.forget_markers();
        if let Phase::Announced { asks, .. } = self.phase
            && !self.release.is_empty()
        {
            self.phase = Phase::Announced { at_s: now, asks };
        }
        if matches!(self.phase, Phase::Failed(_)) {
            let rest = now.saturating_add(self.rest_extension());
            self.failed_at = self.failed_at.max(rest);
        }
    }

    /// The round's `asks`-th notice, READY marker `marker`, is typed by either
    /// lane: it counts one onto [`St::notices`] when it asks anew, none when it
    /// asks again what the round already counted (an ask no later than the
    /// phase holds, [`upgrade::announce_asks`], or a marker typed before,
    /// [`upgrade::rearmed`]). Read before the phase moves and the marker joins
    /// [`St::asked`].
    fn spend_ask(&mut self, asks: u32, marker: &str) {
        let before = match self.phase {
            Phase::Announced { asks, .. } => asks,
            _ => 0,
        };
        if asks > before && !self.asked.iter().any(|m| m == marker) {
            self.notices = self.notices.saturating_add(1);
        }
    }

    /// The `asks`-th notice, READY marker `marker`, typed at `now`: it asks
    /// afresh, so it supersedes a release still owed. The round's earlier
    /// markers stay honoured beside it.
    fn announced(&mut self, marker: String, now: u64, asks: u32) {
        // A first notice opens its round: nothing earlier is its answer.
        if self.phase == Phase::Pending {
            self.markers.clear();
            self.asked.clear();
        }
        self.spend_ask(asks, &marker);
        if !self.markers.contains(&marker) {
            self.markers.push(marker.clone());
        }
        if !self.asked.contains(&marker) {
            self.asked.push(marker.clone());
        }
        self.marker = marker;
        self.phase = Phase::Announced { at_s: now, asks };
        self.noticed_at = now;
        // A round that typed a notice is carried on after its restart.
        self.quiet = false;
        self.release.clear();
        self.ready_since = 0;
        self.last_stop.clear();
        // Work seen before this notice is no answer to it: the visit that
        // typed it stamps what it saw ([`St::time_work`]).
        self.work_seen = false;
    }

    /// The release line was typed: nothing is owed, and the agent was told the
    /// upgrade is off for now, so no READY to this round's notices counts.
    fn released(&mut self) {
        self.release.clear();
        self.forget_markers();
    }

    /// The owed release was dropped ([`release_void`]): nothing is owed, and
    /// the round is over as if the line had been typed, markers forgotten.
    fn dropped(&mut self) {
        self.release.clear();
        self.forget_markers();
    }

    /// The notice's process is gone ([`notice_process_gone`]) and a process the
    /// upgrade never asked holds the conversation in `tab`: the upgrade becomes
    /// pending in a new round ([`St::new_round`]) in the holder's tab.
    /// Forgotten: the markers, `asked`, the notice's fence and the gone
    /// process's looks; the salt is minted past every old marker, since both
    /// processes share one transcript. Returns the release owed to the gone
    /// process (empty: none) for the caller to say once. The time behind and
    /// the owner's word on its tab are kept; a `--now` is spent, as at a give-up.
    fn notice_gone(&mut self, tab: &str, now: u64) -> String {
        let dropped = std::mem::take(&mut self.release);
        self.forget_markers();
        self.asked.clear();
        self.new_round(now);
        self.phase = Phase::Pending;
        self.notice_pid = 0;
        self.notice_start.clear();
        self.noted.clear();
        // The gone process's looks are not the holder's: its idle run counts
        // toward no lag of the new one, and what ran under it holds nothing.
        self.idle_looks = 0;
        self.idle_pid = 0;
        self.idle_at = 0;
        self.held_by.clear();
        self.work_seen = false;
        tab.clone_into(&mut self.tab);
        self.hold_since_s = 0;
        self.hold_seen_s = 0;
        if self.request == Request::Now {
            self.request = Request::None;
            self.request_tab.clear();
            self.request_at = 0;
        }
        dropped
    }

    /// Holds the upgrade's clocks through a limit that stood until `until`
    /// ([`hold_clock`]): an announced upgrade's re-ask clock
    /// ([`upgrade::clock_held_until`]) and the READY clock ([`St::ready_since`])
    /// start no sooner. Returns whether either moved.
    fn hold_clock(&mut self, until: u64) -> bool {
        let phase = upgrade::clock_held_until(&self.phase, until);
        let ready_since = if self.ready_since == 0 {
            0
        } else {
            self.ready_since.max(until)
        };
        let moved = phase != self.phase || ready_since != self.ready_since;
        self.phase = phase;
        self.ready_since = ready_since;
        moved
    }

    /// Forget every READY marker this round issued ([`answered`] then finds
    /// no answer, whatever the transcript holds).
    pub(super) fn forget_markers(&mut self) {
        self.marker.clear();
        self.markers.clear();
        self.ready_since = 0;
    }
}

pub(super) fn state_dir(opts: &Opts) -> PathBuf {
    opts.state.join("upgrade")
}

/// A pending record the session has settled, for a session with nothing to
/// move (`current`): its state goes. Two records qualify, neither begun (no
/// notice, nothing owed):
///
/// * a same-build restart (`from == to`: the model behind) for a session whose
///   model the list no longer moves;
/// * a record for a build the session now runs or passed (`to <= running`),
///   reached by hand or by Claude Code's own updater; kept, it would lend the
///   next build its age.
///
/// Never forgotten here: an announced or stopped round, or a record holding a
/// `--defer` still in force (the retarget carries it to the next build).
/// Returns the record when one was dropped for having been reached by hand.
fn forget_settled_pending(
    opts: &Opts,
    session: &str,
    prior: Option<&St>,
    running: &Version,
) -> Option<St> {
    let model_only = |st: &St| st.from == running.to_string() && st.to == running.to_string();
    let reached = |st: &St| Version::parse(&st.to).is_some_and(|to| to <= *running);
    let now = now_s();
    let word_in_force = |st: &St| matches!(st.request, Request::DeferUntil(t) if t > now);
    let st = prior
        .filter(|st| st.phase == Phase::Pending && st.release.is_empty())
        .filter(|st| !word_in_force(st))
        .filter(|st| model_only(st) || reached(st))?;
    if opts.dry_run {
        return None;
    }
    remove(opts, session);
    (!model_only(st)).then(|| st.clone())
}

/// How long a settled upgrade's state outlives the last live Claude that held
/// its conversation ([`prune_states`]), or the tab its Codex ran in
/// ([`codex_alab_drive::prune`]).
pub(super) const STATE_KEEP_S: u64 = 7 * 24 * 60 * 60;

/// Whether an upgrade's record is settled (done, failed, or pending with no
/// owner's word on it): the one test both lanes' prunes use ([`prune_states`],
/// [`codex_alab_drive::prune`]). A restart in flight (the orphan pass finishes it), an
/// announcement (it may still be answered) and an owner's
/// `--now`/`--defer`/`--skip` (shown in `--status`) are not.
pub(super) fn settled(st: &St) -> bool {
    match st.phase {
        Phase::Done | Phase::Failed(_) => true,
        Phase::Pending => st.request == Request::None,
        _ => false,
    }
}

/// Whether a record of `agent`'s lane is this module's to drive — Claude
/// Code's: the notice it owes ([`St::notified`]), its restart left orphaned
/// ([`orphans`]), its stuck note ([`note_stuck`]), its prune
/// ([`prune_states`]) and the Claude home it names ([`recorded_homes`]) — or
/// its lane's, which does all of that for its own records (Codex's,
/// `codex_alab_drive`, and Muse Code's, [`muse`]: one per tab).
fn driven_here(agent: upgrade::Agent) -> bool {
    match agent {
        upgrade::Agent::Claude => true,
        // Muse Code, agy and Gemini CLI: their lanes' (`muse::prune`,
        // `agy::prune`, `gemini::prune`, each relaunch's carry), never Claude
        // Code's.
        upgrade::Agent::Codex
        | upgrade::Agent::Muse
        | upgrade::Agent::Agy
        | upgrade::Agent::Gemini => false,
    }
}

/// What a lane's prune of its per-tab records (`<word>-<tab>`: codex-alab's
/// `codex_alab_drive::prune`, Muse Code's `muse::prune`, agy's `agy::prune`,
/// Gemini CLI's `gemini::prune`) knows of a tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TabLife {
    /// Proven live: this pass saw it live, or the rendezvous registry names it.
    Live,
    /// Proven gone: no entry names it in a registry this pass can vouch for.
    Gone,
    /// Neither is proven: the record is kept.
    Unknown,
}

/// The rendezvous registry's word on a tab, for a lane's prune ([`TabLife`]).
/// Every live aterm session publishes `graph/<sid>` in the default control
/// directory (`aterm_uds::control_socket_dir`) and retires it on close; tab
/// ids are never reused. An entry reads live, a missing one gone, but only in
/// a registry this pass can vouch for: it must hold an entry for every tab in
/// the non-empty `known_live` (host roster, running agents). Otherwise
/// everything is unknown. A tab in `known_live` is live regardless.
pub(super) fn registry_life<'a>(
    graph: Option<&'a Path>,
    known_live: &'a [String],
) -> impl Fn(&str) -> TabLife + 'a {
    /// `Some(true)` an entry for `sid` exists in `graph`, `Some(false)` none,
    /// `None` unreadable or `sid` not a tab id (never joined onto a path).
    fn entry(graph: &Path, sid: &str) -> Option<bool> {
        let hex = sid.strip_prefix("s-")?;
        if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        match std::fs::symlink_metadata(graph.join(sid)) {
            Ok(_) => Some(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(false),
            Err(_) => None,
        }
    }
    let vouched = graph.filter(|graph| {
        graph.is_dir()
            && !known_live.is_empty()
            && known_live.iter().all(|sid| entry(graph, sid) == Some(true))
    });
    move |tab: &str| {
        if known_live.iter().any(|sid| sid == tab) {
            return TabLife::Live;
        }
        match vouched.and_then(|graph| entry(graph, tab)) {
            Some(true) => TabLife::Live,
            Some(false) => TabLife::Gone,
            None => TabLife::Unknown,
        }
    }
}

/// Whether this pass's registry may speak for the records under its state root
/// ([`registry_life`]): both default, or both private. A private registry over
/// the default state root names none of the user's tabs and would read them all
/// gone, deleting settled records. Mismatched, no registry is read and nothing
/// goes.
pub(super) fn registry_vouches_for_state(private_registry: bool, private_state: bool) -> bool {
    private_registry == private_state
}

/// The rendezvous registry this pass may read for its state root
/// (`<control dir>/graph`, [`registry_vouches_for_state`]), or `None`.
pub(super) fn registry_graph() -> Option<PathBuf> {
    registry_vouches_for_state(
        aterm_uds::control_socket_dir_is_private(),
        aterm_types::dirs::runs_under_private_state_root(),
    )
    .then(aterm_uds::control_socket_dir)
    .flatten()
    .map(|dir| dir.join("graph"))
}

/// Whether the state file `entry` has sat unwritten for [`STATE_KEEP_S`] at
/// `now` (unix seconds). A file whose write time cannot be read has not.
pub(super) fn sat_a_week(entry: &std::fs::DirEntry, now: u64) -> bool {
    entry
        .metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
        .is_some_and(|at| now.saturating_sub(at.as_secs()) >= STATE_KEEP_S)
}

/// Removes the state of conversations nothing will visit again: no live
/// session file names it, the file has not been written for [`STATE_KEEP_S`],
/// and the upgrade is [`settled`]; and a conversation's `.writes` note once
/// it names nothing ([`note_names_nothing`]). A conversation resumed later is asked
/// afresh, as a week-old state would anyway. Called under the sweep lock and
/// only with a session-file list that was read (an unreadable one proves
/// nothing dead). The Codex lane's per-tab records are [`codex_alab_drive::prune`]'s;
/// an explicit restart's stamps, by their own rule, [`super::restart_stamp::prune`]'s.
fn prune_states(opts: &Opts, files: &[SessionFile], now: u64) {
    if opts.dry_run {
        return;
    }
    let Ok(dir) = std::fs::read_dir(state_dir(opts)) else {
        return;
    };
    let live: std::collections::BTreeSet<&str> =
        files.iter().map(|f| f.session_id.as_str()).collect();
    for entry in dir.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|name| name.to_str());
        // An explicit restart's stamp: its own rule (stale, whatever its
        // process), not a conversation's.
        if name.is_some_and(|name| super::restart_stamp::prune(&opts.state, name, now)) {
            continue;
        }
        // A note of where a moved agent writes ([`note_writes`]): its own
        // rule, never its age (it is written only on change) nor its
        // record's (a cold restore reads it with no record left).
        if let Some(id) = name.and_then(|name| name.strip_suffix(".writes")) {
            if !live.contains(id) && note_names_nothing(&path, id) {
                let _ = std::fs::remove_file(&path);
            }
            continue;
        }
        // A move kept beside a removed record ([`record::kept_move`]): its
        // own rule, the move's age.
        if let Some(id) = name.and_then(|name| name.strip_suffix(".move")) {
            record::prune_kept_move(opts, id, now);
            continue;
        }
        let Some(id) = name.and_then(|name| name.strip_suffix(".json")) else {
            continue;
        };
        if live.contains(id) || !sat_a_week(&entry, now) {
            continue;
        }
        let Some(st) = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| St::from_json(&text))
        else {
            continue;
        };
        // A conversation held under the Claude home its record names
        // ([`St::home`]: one this pass may not have read) is live too; one
        // whose records cannot be read whole is kept.
        let held_elsewhere = st.home.is_some()
            && st
                .agent_records(&opts.home)
                .session_files()
                .is_none_or(|files| conversation_live(&files, id));
        if driven_here(st.agent) && settled(&st) && !held_elsewhere {
            // Through the one remover: the move it carried stays.
            remove(opts, id);
        }
    }
}

/// Whether the `.writes` note at `note` ([`note_writes`]) names no
/// transcript of `session` (the review's RR3-LC5-CLEAN): it is blank, or
/// the `projects` of the directory it names is not there, or is there and
/// holds no `<session>.jsonl` — a note [`finish_dir`] never chooses. A
/// note that cannot be read, a `projects` listing with any error, or a
/// transcript whose existence cannot be read names one: a directory this
/// pass cannot see may hold it. Only complete absence permits pruning.
fn note_names_nothing(note: &Path, session: &str) -> bool {
    note_names_nothing_with(
        std::fs::read_to_string(note),
        session,
        |path| {
            std::fs::read_dir(path)
                .map(|entries| entries.map(|entry| entry.map(|entry| entry.path())))
        },
        Path::try_exists,
    )
}

/// The actual note-pruning decision with the filesystem observations supplied.
/// The iterator stays lazy, as `ReadDir` is; tests can supply entry and metadata
/// failures without changing permissions or touching an owner's files.
fn note_names_nothing_with<I>(
    text: std::io::Result<String>,
    session: &str,
    projects: impl FnOnce(&Path) -> std::io::Result<I>,
    mut exists: impl FnMut(&Path) -> std::io::Result<bool>,
) -> bool
where
    I: IntoIterator<Item = std::io::Result<PathBuf>>,
{
    let Ok(text) = text else {
        return false;
    };
    let noted = PathBuf::from(text.trim_end());
    if noted.as_os_str().is_empty() {
        return true;
    }
    match projects(&noted.join("projects")) {
        Ok(projects) => {
            for project in projects {
                let Ok(project) = project else {
                    return false;
                };
                match exists(&project.join(format!("{session}.jsonl"))) {
                    Ok(false) => {}
                    Ok(true) | Err(_) => return false,
                }
            }
            true
        }
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
    }
}

/// The one writer of `session`'s record: [`save_at`] at this second.
pub(super) fn save(opts: &Opts, session: &str, st: &St) {
    save_at(opts, session, st, now_s());
}

/// Save the same progress-stamped record, refusing an act that needs the
/// claim on disk when its write did not complete.
pub(super) fn save_checked(opts: &Opts, session: &str, st: &St) -> std::io::Result<()> {
    record::save_at_checked(opts, session, st, now_s())
}

/// WHETHER AN AGENT MAY BE MOVED ONTO `to` AT `now`
/// ([`build_start::starts`]): no failure to start noted against the build,
/// or one a probe run since proved to start without an agent — or, for a
/// build no probe proves any more, a probe run for the move the owner asked
/// for (`owner_now`). What the upgrade's visit and a relaunch on exit both
/// ask before they take `to`; a probe run here is said on the ledger as row
/// `r`. A build no probe has run on is probed first where the step
/// `moving` stakes an agent on it now ([`build_start::starts`]).
pub(super) fn build_starts(
    opts: &Opts,
    r: &Report,
    to: &Candidate,
    now: u64,
    owner_now: bool,
    moving: bool,
) -> Result<(), &'static str> {
    build_start::starts(opts, r, to, now, owner_now, moving)
}

/// Whether a crashed agent's relaunch may move onto `to` at `now`: the build
/// PROVED to start ([`build_start::proved`], LC-5 of ).
pub(super) fn build_proved(
    opts: &Opts,
    r: &Report,
    to: &Candidate,
    now: u64,
) -> Result<(), &'static str> {
    build_start::proved(opts, r, to, now)
}

/// The upgrade record has one writer and one remover. Every clock the view and
/// the watch read off a record (its progress stamp, `St::progress_at`, and the
/// deadlines built on it) is kept by [`save_at`], which stamps progress as it
/// writes, and a record that goes, goes by [`remove`]. The record's path is
/// private to this module, so no step can write or remove a record past them
/// and move a clock no deadline sees. A path built by hand from [`state_dir`],
/// as the settled records' prune builds one, is outside this.
mod record {
    use super::{Opts, St, state_dir};
    use std::path::PathBuf;

    fn state_path(opts: &Opts, session: &str) -> PathBuf {
        state_dir(opts).join(format!("{session}.json"))
    }

    fn move_path(opts: &Opts, session: &str) -> PathBuf {
        state_dir(opts).join(format!("{session}.move"))
    }

    /// THE CONVERSATION'S LAST MOVE, KEPT BESIDE ITS RECORD once the one
    /// remover ([`remove`]) took away a record that carried it: a record that
    /// holds nothing but the move ([`St::moved_at`] and its target), or
    /// `None`. The move is the conversation's, not the record's: a record
    /// closed before the first turn served after the move (an upgrade round
    /// written over it and then reached by hand,
    /// [`super::forget_settled_pending`]; a settled record pruned,
    /// [`super::prune_states`]) took it along, and the limit row the move left
    /// held a relaunch's carry-on until that limit reset.
    /// Read where the record is ([`super::super::relaunch::moved_off`]), and
    /// carried onto the next record [`save_at`] writes.
    pub(in super::super) fn kept_move(opts: &Opts, session: &str) -> Option<St> {
        St::from_json(&std::fs::read_to_string(move_path(opts, session)).ok()?)
            .filter(|st| st.moved_at != 0)
    }

    /// Keeps the last move `disk` knows of as `session`'s kept move
    /// ([`kept_move`]), unless one as new is kept already.
    fn keep_move(opts: &Opts, session: &str, disk: &St) {
        let moved = super::super::relaunch::last_move_at(disk);
        if moved == 0 || kept_move(opts, session).is_some_and(|kept| kept.moved_at >= moved) {
            return;
        }
        let (moved_login, moved_left_model) = super::super::relaunch::last_move_target(disk);
        let kept = St {
            phase: super::Phase::Done,
            agent: disk.agent,
            moved_at: moved,
            moved_login,
            moved_left_model,
            ..St::default()
        };
        let path = move_path(opts, session);
        let tmp = path.with_extension("move.tmp");
        if std::fs::write(&tmp, kept.to_json()).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }

    /// Takes away `session`'s kept move ([`kept_move`]) once, at `now`, the
    /// move is older than the longest limit and its grace
    /// ([`super::upgrade_status::LONGEST_LIMIT_S`],
    /// [`super::upgrade_status::LIMIT_GRACE_S`]): every limit row written
    /// before it has reset, so it hides nothing a reset would not. Answers
    /// whether one is kept still.
    pub(in super::super) fn prune_kept_move(opts: &Opts, session: &str, now: u64) -> bool {
        let Some(kept) = kept_move(opts, session) else {
            return false;
        };
        let spent = kept
            .moved_at
            .saturating_add(super::upgrade_status::LONGEST_LIMIT_S)
            .saturating_add(super::upgrade_status::LIMIT_GRACE_S);
        if now < spent || opts.dry_run {
            return true;
        }
        let _ = std::fs::remove_file(move_path(opts, session));
        false
    }

    pub(in super::super) fn load(opts: &Opts, session: &str) -> Option<St> {
        St::from_json(&std::fs::read_to_string(state_path(opts, session)).ok()?)
    }

    /// Writes `st` as `session`'s record at `now`, stamping its progress: when its
    /// progress key ([`St::progress_key`]) differs from the record on disk (or
    /// there is none) it is written as having moved at `now`
    /// ([`St::progress_at`]); otherwise the stamp on disk is kept, so a stale copy
    /// saved again restarts no clock. Detecting progress at the single writer
    /// means no step can forget or fake the stamp. A dry run writes nothing.
    ///
    /// And THE CONVERSATION'S LAST MOVE ([`St::moved_at`], with its target) is
    /// carried from the record on disk onto the one written over it, whatever
    /// step writes it: the move is the conversation's, not the record's. So are
    /// THE EARLIER TABS that held it ([`St::tabs`], and when each lost it,
    /// [`St::tabs_until`]): a record written for a new tab keeps the tab on
    /// disk among them.
    pub(in super::super) fn save_at(opts: &Opts, session: &str, st: &St, now: u64) {
        let _ = save_at_with(opts, session, st, now, |opts, session, out| {
            // The legacy policy: record writes are best effort and the build
            // note still runs even when that write did not complete.
            write_verbatim(opts, session, out);
            Ok(())
        });
    }

    /// The shared writer's checked arm: progress and carry stamps are the
    /// same as save_at's; a failed write is returned before an act can start.
    pub(in super::super) fn save_at_checked(
        opts: &Opts,
        session: &str,
        st: &St,
        now: u64,
    ) -> std::io::Result<()> {
        save_at_with(opts, session, st, now, write_verbatim_checked)
    }

    fn save_at_with(
        opts: &Opts,
        session: &str,
        st: &St,
        now: u64,
        writer: impl FnOnce(&Opts, &str, &St) -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        if opts.dry_run {
            return Ok(());
        }
        let key = st.progress_key();
        let on_disk = load(opts, session);
        let mut out = st.clone();
        match &on_disk {
            Some(disk) if disk.progress_key == key => out.progress_at = disk.progress_at,
            _ => out.progress_at = now,
        }
        out.progress_key = key;
        // The move on disk — its record's, or kept beside a record removed
        // ([`kept_move`]) — where it is the newer: its target rides with its
        // instant.
        let kept = on_disk
            .is_none()
            .then(|| kept_move(opts, session))
            .flatten();
        for disk in on_disk.iter().chain(kept.iter()) {
            let moved = super::super::relaunch::last_move_at(disk);
            if moved != 0 && moved >= out.moved_at {
                (out.moved_login, out.moved_left_model) =
                    super::super::relaunch::last_move_target(disk);
            }
            out.moved_at = out.moved_at.max(moved);
        }
        if let Some(disk) = &on_disk {
            out.tabs = super::earlier_tabs(
                &out.tab,
                disk.tabs
                    .iter()
                    .chain(std::iter::once(&disk.tab))
                    .chain(out.tabs.iter()),
            );
            // When each earlier tab lost the conversation: the stamps on disk and
            // in the record, the newest of each, and the tab on disk now, where
            // this write is another tab's.
            let mut until = disk.tabs_until.clone();
            for (tab, at) in &out.tabs_until {
                super::stamp_tab(&mut until, tab, *at);
            }
            if !disk.tab.is_empty() && disk.tab != out.tab {
                super::stamp_tab(&mut until, &disk.tab, now);
            }
            until.retain(|(tab, _)| out.tabs.contains(tab));
            out.tabs_until = until;
        }
        writer(opts, session, &out)?;
        // A build that failed to start is the build's fact too, noted (and a
        // stop taken back, retracted) by the one writer every stop is saved
        // through.
        super::build_start::note(opts, session, on_disk.as_ref(), st, now);
        Ok(())
    }

    /// Writes `st` as `session`'s record exactly as it is, progress stamp
    /// included. [`save_at`] writes through it after stamping; [`restore`] uses
    /// it alone, so putting back a record after an act that did not go ahead is
    /// no progress. A dry run writes nothing.
    fn write_verbatim(opts: &Opts, session: &str, st: &St) {
        if opts.dry_run {
            return;
        }
        let path = state_path(opts, session);
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, st.to_json()).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }

    fn write_verbatim_checked(opts: &Opts, session: &str, st: &St) -> std::io::Result<()> {
        if opts.dry_run {
            return Ok(());
        }
        let path = state_path(opts, session);
        #[cfg(unix)]
        {
            aterm_uds::atomic_file::write(&path, st.to_json().as_bytes(), 0o600)
        }
        #[cfg(not(unix))]
        {
            // Keep the existing portable replacement route, returning its
            // failure to the checked caller instead of hiding it.
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = path.with_extension("json.tmp");
            std::fs::write(&tmp, st.to_json())?;
            std::fs::rename(&tmp, &path)
        }
    }

    /// Puts back `session`'s record from before an act that did not go ahead:
    /// `prior` as read, progress stamp included ([`write_verbatim`]), or no
    /// record.
    pub(in super::super) fn restore(opts: &Opts, session: &str, prior: Option<&St>) {
        if opts.dry_run {
            return;
        }
        match prior {
            Some(st) => write_verbatim(opts, session, st),
            None => remove(opts, session),
        }
    }

    /// The one remover of `session`'s record: a record owed nothing more goes
    /// here, as does the absence [`restore`] puts back. The conversation's last
    /// move it carried stays, beside it ([`kept_move`]). A dry run removes
    /// nothing.
    pub(in super::super) fn remove(opts: &Opts, session: &str) {
        if opts.dry_run {
            return;
        }
        if let Some(disk) = load(opts, session) {
            keep_move(opts, session, &disk);
        }
        let _ = std::fs::remove_file(state_path(opts, session));
    }

    /// A test's planted record, written exactly as given: the watch suite's
    /// negative controls stamp their own progress.
    #[cfg(all(test, unix))]
    pub(in super::super) fn write_verbatim_for_test(opts: &Opts, session: &str, st: &St) {
        write_verbatim(opts, session, st);
    }
}

#[cfg(all(test, unix))]
use record::write_verbatim_for_test as write_verbatim;
pub(super) use record::{kept_move, load, remove, restore, save_at};

/// One ledger line per step taken: `<state>/upgrade/ledger.jsonl`. `detail` is
/// the step's own: for a `relaunched`, an `exited` and a `failed:relaunch:…` row
/// it is the line typed, whole, with its `cd '<cwd>'` and, for a session on one
/// of the owner's listed accounts, the login directory it carries
/// (`CLAUDE_SECURESTORAGE_CONFIG_DIR='<dir>'`: a path under the owner's state
/// directory, never a credential).
pub(super) fn ledger(opts: &Opts, r: &Report, detail: &str) {
    if opts.dry_run {
        return;
    }
    let mut o = Map::new();
    o.insert("t".into(), Value::from(now_s()));
    o.insert("pid".into(), Value::from(r.pid));
    for (k, v) in [
        ("tab", &r.tab),
        ("session", &r.session),
        ("from", &r.from),
        ("to", &r.to),
        ("step", &r.step),
    ] {
        o.insert(k.into(), Value::from(v.as_str()));
    }
    o.insert("detail".into(), Value::from(detail));
    let dir = state_dir(opts);
    let _ = std::fs::create_dir_all(&dir);
    let path = ledger_path(opts);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(
            f,
            "{}",
            aterm_json::to_string(&Value::Object(o)).unwrap_or_default()
        );
    }
    // Cut to its newest rows past the bound, like the disk journal
    // ([`super::cli::bound_ledger`]); every write holds the sweep lock, so no
    // other sweeper appends between the cut's read and its rename.
    let _ = super::cli::bound_ledger(&path, LEDGER_MAX_BYTES, LEDGER_KEEP_ROWS);
}

/// `<state>/upgrade/ledger.jsonl` — what `aterm harness ledger upgrade` reads.
#[must_use]
pub fn ledger_path(opts: &Opts) -> PathBuf {
    state_dir(opts).join("ledger.jsonl")
}

/// The ledger size past which it is cut to its newest [`LEDGER_KEEP_ROWS`]
/// rows: the disk journal's bound ([`super::cli::DISK_LEDGER_MAX_BYTES`]).
const LEDGER_MAX_BYTES: u64 = super::cli::DISK_LEDGER_MAX_BYTES;

/// How many rows survive a cut ([`LEDGER_MAX_BYTES`]).
const LEDGER_KEEP_ROWS: usize = super::cli::DISK_LEDGER_KEEP_ROWS;

// ---------------------------------------------------------------- claude files

#[cfg(test)]
thread_local! {
    /// Counts complete directory scans, not candidates, in one test thread.
    static SESSION_FILE_SCANS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Every session record in `~/.claude/sessions`, or `None` when the roster is
/// partial, since a partial roster cannot prove a conversation has one live
/// owner: a `<digits>.json` sibling that is unreadable or malformed (maybe
/// mid-write) while its process may live invalidates the scan until the next
/// sweep. A file not named `<digits>.json` is not Claude Code's, and an
/// unreadable one whose pid is not running names no holder, so one stale
/// sibling cannot block every upgrade. A record whose file name and `pid`
/// disagree vetoes while either process runs.
pub(super) fn session_files(home: &Path) -> Option<Vec<SessionFile>> {
    session_files_in(&home.join(".claude"))
}

/// [`session_files`] of the Claude Code directory `claude_dir` itself —
/// `CLAUDE_CONFIG_DIR`, or `~/.claude`: its `sessions/`.
pub(super) fn session_files_in(claude_dir: &Path) -> Option<Vec<SessionFile>> {
    #[cfg(test)]
    SESSION_FILE_SCANS.with(|count| count.set(count.get() + 1));
    let dir = std::fs::read_dir(claude_dir.join("sessions")).ok()?;
    let mut files = Vec::new();
    for entry in dir {
        let path = entry.ok()?.path();
        if path.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let Some(file_pid) = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .and_then(|stem| stem.parse::<u32>().ok())
        else {
            continue;
        };
        let file = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| upgrade::parse_session_file(&text).ok());
        match file {
            Some(file) if file.pid == file_pid => files.push(SessionFile {
                claude_dir: Some(claude_dir.to_path_buf()),
                ..file
            }),
            Some(file) if !alive(file_pid) && !alive(file.pid) => {}
            None if !alive(file_pid) => {}
            _ => return None,
        }
    }
    Some(files)
}

/// How often [`SessionRosterCache`] rescans a session directory whose mtime has
/// not moved: an in-place write to an existing `<pid>.json` leaves the
/// directory's mtime. Between rescans a complete snapshot is kept, so the
/// relaunch's 250 ms poll does not reparse every record; an unreadable or
/// incomplete one is retried at the next poll.
pub(super) const SESSION_ROSTER_RESCAN: Duration = Duration::from_secs(1);

/// [`session_files`] behind [`SESSION_ROSTER_RESCAN`]'s cache: what the
/// relaunch's wait for the new process reads (`relaunch::await_new`).
#[derive(Default)]
pub(super) struct SessionRosterCache {
    stamp: Option<SystemTime>,
    scanned_at: Option<Instant>,
    files: Option<Vec<SessionFile>>,
}

impl SessionRosterCache {
    /// The records in `records`' directory, read as every reader of them
    /// reads them ([`Records::session_files`]: each stamped with the profile
    /// its home keeps apart), behind the cache.
    pub(super) fn files(&mut self, records: &Records, now: Instant) -> Option<&[SessionFile]> {
        let dir = records.dir()?;
        let stamp = std::fs::metadata(dir.join("sessions"))
            .and_then(|m| m.modified())
            .ok();
        self.files_at(stamp, now, || records.session_files())
    }

    fn files_at(
        &mut self,
        stamp: Option<SystemTime>,
        now: Instant,
        scan: impl FnOnce() -> Option<Vec<SessionFile>>,
    ) -> Option<&[SessionFile]> {
        if self.files.is_none()
            || stamp.is_none()
            || stamp != self.stamp
            || self
                .scanned_at
                .is_none_or(|at| now.saturating_duration_since(at) >= SESSION_ROSTER_RESCAN)
        {
            // Record the stamp before scanning. A file created during the
            // scan then forces another read on the next poll.
            self.stamp = stamp;
            self.scanned_at = Some(now);
            self.files = scan();
        }
        self.files.as_deref()
    }
}

/// State is keyed by conversation, so a READY marker can be acted on only
/// when exactly one kernel-verified live process holds that conversation.
/// An unreadable kernel start is ambiguous and fails closed.
fn unique_live_owner(files: &[SessionFile], wanted: &SessionFile) -> bool {
    let mut owner = None;
    for sf in files.iter().filter(|sf| sf.session_id == wanted.session_id) {
        if !alive(sf.pid) {
            continue;
        }
        let Some(start) = kernel_start(sf.pid) else {
            return false;
        };
        if start != squash(&sf.proc_start) {
            continue;
        }
        if owner.replace(sf.pid).is_some() {
            return false;
        }
    }
    owner == Some(wanted.pid)
}

/// Preliminary owner check against the sweep's already-complete roster. It may
/// skip an irrelevant or ambiguous candidate, but it never authorizes an act:
/// those still re-read the whole directory at their own decision point.
fn preliminary_unique_owner(
    files: Option<&[SessionFile]>,
    wanted: &SessionFile,
) -> Result<(), &'static str> {
    let files = files.ok_or("session-files-unreadable")?;
    unique_live_owner(files, wanted)
        .then_some(())
        .ok_or("conversation-owner-ambiguous")
}

/// Fresh, complete directory read for every decision that can type, signal or
/// consume a notice. The initial sweep roster is only a candidate index. Read
/// where `wanted` was read ([`Records::of_file`]: the Claude home its agent
/// keeps its records under; `home` is the user's).
pub(super) fn require_unique_owner(home: &Path, wanted: &SessionFile) -> Result<(), &'static str> {
    Records::of_file(home, wanted).unique_owner(wanted)
}

pub(super) fn conversation_live(files: &[SessionFile], session: &str) -> bool {
    files
        .iter()
        .filter(|sf| sf.session_id == session)
        .any(|sf| {
            alive(sf.pid)
                && kernel_start(sf.pid).is_none_or(|start| start == squash(&sf.proc_start))
        })
}

/// The host prefilters with one cheap group lookup. Only a process whose
/// group uniquely matches a local PTY foreground group incurs focused `ps`
/// and argv reads. The later visit still applies the newer job-control and
/// terminal-owner guards; a group match alone never authorizes an act.
#[derive(Clone, Debug, PartialEq, Eq)]
struct HostClaim {
    tab: String,
    group: i64,
    parent: u32,
}

/// A record whose agent leads a tab, its process read whole: what the host
/// inspects further ([`host_candidates`]).
type Claimed<'a> = (&'a SessionFile, atpkg::caller_shell::ProcArgs, HostClaim);

/// The records whose agent leads a tab and that `needs_inspection` asks
/// about, each claimed, or, where its process could not be read whole (ids or
/// argv unread, or the tab's foreground moving between roster and ids), the
/// step's wait (`wait:`[`UNREAD_PROCESS`]): nothing was read to decide on, so
/// nothing is decided, rather than silently dropping it. A record whose
/// readable environment names another tab is left out.
fn host_candidates<'a>(
    files: &'a [SessionFile],
    live_tabs: &[LiveTab],
    mut needs_inspection: impl FnMut(&SessionFile, &str) -> bool,
    mut read_args: impl FnMut(u32) -> Option<atpkg::caller_shell::ProcArgs>,
    mut read_group: impl FnMut(u32) -> Option<i64>,
    mut read_ids: impl FnMut(u32) -> Option<(u32, i64, i64)>,
) -> Vec<Result<Claimed<'a>, Report>> {
    if !live_tabs.iter().any(|tab| tab.fgpgid.is_some()) {
        return Vec::new();
    }
    files
        .iter()
        .filter(|sf| {
            sf.kind == "interactive"
                && sf.entrypoint == "cli"
                && upgrade::is_session_id(&sf.session_id)
        })
        .filter_map(|sf| {
            let group = read_group(sf.pid)?;
            let tab = unique_tab_for_group(live_tabs, group)?;
            if !needs_inspection(sf, tab) {
                return None;
            }
            let read = read_ids(sf.pid)
                .filter(|&(_, checked_group, tty_front)| {
                    checked_group == group && tty_front == group
                })
                .and_then(|(parent, _, _)| Some((parent, read_args(sf.pid)?)));
            let Some((parent, args)) = read else {
                return Some(Err(Report {
                    tab: tab.to_string(),
                    step: format!("wait:{UNREAD_PROCESS}"),
                    ..blank(sf)
                }));
            };
            if args
                .env_var("ATERM_PARENT_SESSION_ID")
                .is_some_and(|env_tab| env_tab != tab)
            {
                return None;
            }
            Some(Ok((
                sf,
                args,
                HostClaim {
                    tab: tab.to_string(),
                    group,
                    parent,
                },
            )))
        })
        .collect()
}

/// The host need not inspect a process or build a whole process table when
/// every known target is already at or below the version Claude reported.
/// A native target remains a possibility until the process is examined, and
/// an in-flight restart must keep its recovery path even if its new process
/// already reports the target version. Unknown versions take the full path.
fn needs_upgrade_inspection(sf: &SessionFile, targets: &Targets, prior: Option<&St>) -> bool {
    // A restart in flight, a release owed whatever build the agent runs now
    // ([`release_visit`]), and an announced round whose notice carried a model
    // ([`St::promised_model`]): the ride is the round's own state, so its READY
    // must still be acted on or a barred ride dropped ([`ride_dropped`]). A
    // gave-up round's late READY acts only while its release is owed, which
    // this lets through.
    if prior
        .is_some_and(|st| st.in_flight() || !st.release.is_empty() || st.promised_model().is_some())
    {
        return true;
    }
    // A relaunch stopped as never picked up whose own agent holds the
    // conversation now, whatever its build: the visit reopens and carries it
    // on ([`St::landed_after_stop`]).
    if prior.is_some_and(|st| st.landed_after_stop(sf, &Live)) {
        return true;
    }
    let Some(running) = Version::parse(&sf.version) else {
        return true;
    };
    targets
        .managed
        .iter()
        .chain(targets.native.iter())
        .any(|target| target.version > running)
}

fn claim_still_live(c: &mut Client, pid: u32, claim: &HostClaim) -> bool {
    process_in_tab(c, pid, &claim.tab, Some(claim.group), Some(claim.parent))
}

#[cfg(all(test, unix))]
pub(super) fn session_file_of(home: &Path, pid: u32) -> Option<SessionFile> {
    session_file_in(&home.join(".claude"), pid)
}

/// [`session_file_of`] in the Claude Code directory `claude_dir` itself.
pub(super) fn session_file_in(claude_dir: &Path, pid: u32) -> Option<SessionFile> {
    let t = std::fs::read_to_string(claude_dir.join(format!("sessions/{pid}.json"))).ok()?;
    let file = upgrade::parse_session_file(&t).ok()?;
    Some(SessionFile {
        claude_dir: Some(claude_dir.to_path_buf()),
        ..file
    })
}

/// Whether any transcript of `session` exists (`None`: nobody can say).
/// Claude files a transcript only once the conversation has a message, and
/// one without cannot be resumed (`--resume` answers "No conversation found
/// with session ID").
pub(super) fn transcript_exists(home: &Path, session: &str) -> Option<bool> {
    transcript_exists_in(&home.join(".claude"), session)
}

/// [`transcript_exists`] in the Claude Code directory `claude_dir` itself. A
/// home with no `projects/` holds no transcript: Claude Code makes it with the
/// home's first message. `None` only where the projects exist and cannot be
/// listed, or the home itself is not there (then the agent keeps its records
/// elsewhere).
pub(super) fn transcript_exists_in(claude_dir: &Path, session: &str) -> Option<bool> {
    let name = format!("{session}.jsonl");
    match std::fs::read_dir(claude_dir.join("projects")) {
        Ok(dirs) => Some(dirs.flatten().any(|e| e.path().join(&name).is_file())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && claude_dir.is_dir() => Some(false),
        Err(_) => None,
    }
}

/// Where one agent's Claude Code keeps its own records: its process's
/// `sessions/<pid>.json` and its conversation's `projects/<project>/<id>.jsonl`,
/// in the directory `CLAUDE_CONFIG_DIR` names in its exec environment, else its
/// `HOME`'s `.claude` (`docs/DESIGN-aterm-wrapper-2026-09-17.md`). Every step
/// about an agent reads its records, never the user's home alone; one resolver
/// names every such directory ([`Records::at`], [`Records::of_process`]) and
/// the profile kept for it ([`profile_apart`], [`Records::profile`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Records {
    /// The Claude Code directory, where one is known.
    dir: Option<PathBuf>,
    /// The profile, where the home's raw value keeps it apart from what the
    /// directory alone says ([`profile_apart`]); `None` for every other home,
    /// whose profile is read by its directory ([`claude_profile`]).
    profile: Option<PathBuf>,
}

/// The profile (global config) Claude Code keeps for a home, where it differs
/// from what the records' directory alone says ([`claude_profile`]): its
/// onboarding, login and the login's model caches ([`models::read_evidence`]).
/// Claude Code keeps it at `join(CLAUDE_CONFIG_DIR || homedir(),
/// ".claude.json")` while the records are under `CLAUDE_CONFIG_DIR ??
/// join(homedir(), ".claude")` ([`Records::at`]); a legacy `.config.json` in
/// the records directory can stand in for it and a custom OAuth URL can rename
/// it (`profile::effective`, which `accounts stats` follows and the model rule
/// does not). So a set and empty `CLAUDE_CONFIG_DIR` (records in the working
/// directory) keeps its profile at `<homedir>/.claude.json`, while `.`, naming
/// the same directory, keeps it inside; a set one naming the user's own
/// `~/.claude` keeps it inside too, `<dir>/.claude.json`, as every set home
/// does; an unset one whose `<HOME>/.claude` (`dir`) is not the user's keeps it
/// beside that directory. `None` for every other home.
pub(super) fn profile_apart(
    user_home: &Path,
    vars: HomeVars<'_>,
    dir: Option<&Path>,
) -> Option<PathBuf> {
    match vars.config {
        Some("") => Some(homedir(user_home, vars.home).join(".claude.json")),
        Some(_) => dir
            .filter(|dir| *dir == user_home.join(".claude"))
            .map(|dir| dir.join(".claude.json")),
        None => dir
            .filter(|dir| *dir != user_home.join(".claude"))
            .and_then(Path::parent)
            .map(|home| home.join(".claude.json")),
    }
}

/// The profile of the home whose records are in `dir`, named by the
/// environment `vars`: the one kept apart ([`profile_apart`]), else the
/// directory's ([`claude_profile`]). [`Records::profile`] for a caller that has
/// resolved the directory already (`profile::path_in`).
pub(super) fn profile_in(user_home: &Path, vars: HomeVars<'_>, dir: &Path) -> PathBuf {
    profile_apart(user_home, vars, Some(dir)).unwrap_or_else(|| claude_profile(user_home, dir))
}

/// What a Claude Code's exec environment says of its home ([`Records::at`],
/// [`claude_home_dir`]): its raw `CLAUDE_CONFIG_DIR` (`None`: unset) and its
/// raw `HOME`, `USERPROFILE` on Windows ([`super::accounts::HOMEDIR_ENV`];
/// `None`: unset, or not read).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct HomeVars<'a> {
    /// The raw `CLAUDE_CONFIG_DIR`.
    pub(super) config: Option<&'a str>,
    /// The raw `HOME` (`USERPROFILE` on Windows): Claude Code's `homedir()`.
    pub(super) home: Option<&'a str>,
}

impl<'a> HomeVars<'a> {
    /// The two, as the exec record `args` holds them.
    pub(super) fn of(args: &'a atpkg::caller_shell::ProcArgs) -> Self {
        Self {
            config: args.env_var(super::accounts::HOME_ENV),
            home: args.env_var(super::accounts::HOMEDIR_ENV),
        }
    }

    /// A raw `CLAUDE_CONFIG_DIR` alone, its process's `HOME` not known (the
    /// user's is taken).
    pub(super) fn config(config: Option<&'a str>) -> Self {
        Self { config, home: None }
    }
}

/// Claude Code's `homedir()` for a process whose raw `HOME` is `home`: that
/// directory where it is absolute, else the user's (`user_home`).
fn homedir(user_home: &Path, home: Option<&str>) -> PathBuf {
    home.map(Path::new)
        .filter(|home| home.is_absolute())
        .map_or_else(|| user_home.to_path_buf(), Path::to_path_buf)
}

/// The one resolver of a Claude Code directory from an exec environment
/// (`vars`), as Claude Code resolves it: `CLAUDE_CONFIG_DIR ?? join(homedir(),
/// ".claude")`. A set one by [`home_dir_at`]; an unset one under the process's
/// own `HOME`, else `fallback`'s (the user's home; `None`: nothing is named).
/// Lexical. Every reader asks it: [`Records::at`] and the footer's
/// ([`super::footer::claude_dir_of`]).
pub(super) fn claude_home_dir(
    fallback: Option<&Path>,
    vars: HomeVars<'_>,
    cwd: impl FnOnce() -> Option<PathBuf>,
) -> Option<PathBuf> {
    match vars.config {
        Some(config) => home_dir_at(config, cwd),
        None => vars
            .home
            .map(Path::new)
            .filter(|home| home.is_absolute())
            .or(fallback)
            .map(|home| home.join(".claude")),
    }
}

/// Whether `a` and `b` name the same directory however spelled (a link, a
/// `..`).
pub(super) fn same_dir(a: &Path, b: &Path) -> bool {
    a == b || std::fs::canonicalize(a).is_ok_and(|a| std::fs::canonicalize(b).is_ok_and(|b| a == b))
}

/// The Claude Code directory a set `CLAUDE_CONFIG_DIR` names (`home`, its raw
/// value), for a process whose working directory `cwd` reads
/// ([`Records::at`]): an absolute one as it is; a relative one, empty included,
/// under that working directory, read only then, and `None` where it is not
/// read. Lexical, as Claude Code's own joins are ([`claude_dir_at`]).
pub(super) fn home_dir_at(home: &str, cwd: impl FnOnce() -> Option<PathBuf>) -> Option<PathBuf> {
    let base = if Path::new(home).is_absolute() {
        PathBuf::from("/")
    } else {
        cwd().filter(|cwd| cwd.is_absolute())?
    };
    claude_dir_at(Some(home), None, &base, || None).ok()
}

impl Records {
    /// The one resolver: the records of a Claude Code whose exec environment
    /// held `vars`, for a user whose home is `user_home`, where Claude Code
    /// files them (`CLAUDE_CONFIG_DIR ?? join(homedir(), ".claude")`). Unset:
    /// under the process's own `HOME`, the user's where that is not known.
    /// Absolute: that directory. Relative, empty included: under the working
    /// directory `cwd` reads, asked only then ([`home_dir_at`]). A relative home
    /// whose working directory is not read names nothing, and every lookup
    /// answers that it cannot say.
    pub(super) fn at(
        user_home: &Path,
        vars: HomeVars<'_>,
        cwd: impl FnOnce() -> Option<PathBuf>,
    ) -> Self {
        let dir = claude_home_dir(Some(user_home), vars, cwd);
        Self::in_dir(user_home, vars, dir)
    }

    /// The records of the running Claude Code `pid` where it filed them. Claude
    /// Code keeps `CLAUDE_CONFIG_DIR` raw and opens every path under a relative
    /// one (empty included) from the directory it runs in at that moment, so
    /// its session record, written as it starts, stays under the directory it
    /// started in while a later `process.chdir` (a worktree) moves where it
    /// writes next. So the home resolves from the directory that holds its own
    /// record (`sessions/<pid>.json` naming this process's start,
    /// [`kernel_start`]): its working directory now (`cwd`), else the one it
    /// started in (`started_in`, its exec `PWD`); with neither, its working
    /// directory now. An unset or absolute home names its directory by itself.
    /// Where what it writes now ([`Records::at`]) differs, a look waits
    /// ([`Records::left_by`]).
    pub(super) fn of_process(
        user_home: &Path,
        vars: HomeVars<'_>,
        pid: u32,
        cwd: impl FnOnce() -> Option<PathBuf>,
        started_in: Option<&str>,
    ) -> Self {
        let Some(relative) = vars.config.filter(|home| Path::new(home).is_relative()) else {
            return Self::at(user_home, vars, || None);
        };
        let vars = HomeVars {
            config: Some(relative),
            ..vars
        };
        let now = cwd();
        let started = started_in
            .map(Path::new)
            .filter(|dir| dir.is_absolute())
            .and_then(|dir| std::fs::canonicalize(dir).ok());
        let start = std::cell::OnceCell::new();
        for base in [now.clone(), started].into_iter().flatten() {
            let records = Self::at(user_home, vars, || Some(base));
            let its_own = records.session_file(pid).is_some_and(|sf| {
                start.get_or_init(|| kernel_start(pid)).as_deref()
                    == Some(squash(&sf.proc_start).as_str())
            });
            if its_own {
                return records;
            }
        }
        Self::at(user_home, vars, || now)
    }

    /// Whether the running Claude Code whose record these records hold has left
    /// them: its raw home `home` is relative (empty included) and resolves from
    /// the directory it runs in now (`cwd`) to another directory than these, or
    /// to none. A worktree's `process.chdir` since it filed its record
    /// ([`Records::of_process`]) does that: its record keeps the status it had
    /// while its conversation goes on under the new home. An unset or absolute
    /// home names one directory wherever it runs.
    pub(super) fn left_by(
        &self,
        user_home: &Path,
        home: Option<&str>,
        cwd: impl FnOnce() -> Option<PathBuf>,
    ) -> bool {
        let Some(relative) = home.filter(|home| Path::new(home).is_relative()) else {
            return false;
        };
        match (
            self.dir(),
            Self::at(user_home, HomeVars::config(Some(relative)), cwd).dir(),
        ) {
            (Some(filed), Some(now)) => !same_dir(filed, now),
            _ => true,
        }
    }

    /// The records in `dir` of a Claude Code whose raw home was `home`, its
    /// profile kept apart where that home keeps it so ([`profile_apart`]).
    fn in_dir(user_home: &Path, vars: HomeVars<'_>, dir: Option<PathBuf>) -> Self {
        Self {
            profile: profile_apart(user_home, vars, dir.as_deref()),
            dir,
        }
    }

    /// [`Records::at`] of a home that names its directory by itself: unset (the
    /// user's own) or absolute, what a record of the agent names ([`St::home`],
    /// [`Records::home_word`]). A relative one (an older build's record) names
    /// nothing.
    pub(super) fn of(user_home: &Path, home: Option<&str>) -> Self {
        Self::at(user_home, HomeVars::config(home), || None)
    }

    /// The records a record of the agent names ([`St::agent_records`]): the
    /// directory its home word names ([`Records::of`]) and the profile it
    /// kept apart (`profile`, [`Records::profile_word`]) — the records they
    /// were written from ([`St::keep_home`]), an empty home's profile
    /// included, which its directory alone does not name.
    pub(super) fn of_recorded(user_home: &Path, home: Option<&str>, profile: Option<&str>) -> Self {
        let of = Self::of(user_home, home);
        Self {
            profile: profile.map(PathBuf::from).or(of.profile),
            dir: of.dir,
        }
    }

    /// The records of the agent the relaunch snapshot `snap` is of: where its
    /// record was read as the snapshot was taken
    /// ([`super::relaunch::Snapshot::claude_dir`]), else (no process was read
    /// for it, a reopened layout's) where its home resolves from the directory
    /// its line starts it in ([`super::relaunch::Snapshot::cwd`]), where its
    /// relaunched process files its record.
    pub(super) fn of_snapshot(user_home: &Path, snap: &super::relaunch::Snapshot) -> Self {
        match &snap.claude_dir {
            Some(dir) => Self::in_dir(
                user_home,
                HomeVars::config(snap.home.as_deref()),
                Some(dir.clone()),
            ),
            None => Self::at(user_home, HomeVars::config(snap.home.as_deref()), || {
                Some(PathBuf::from(&snap.cwd))
            }),
        }
    }

    /// The records of the agent whose record `sf` is: the Claude Code
    /// directory it was read from ([`SessionFile::claude_dir`]) and the
    /// profile of the home it was read under ([`SessionFile::profile`]),
    /// else — a record parsed from text alone — the user's own
    /// `user_home/.claude`.
    pub(super) fn of_file(user_home: &Path, sf: &SessionFile) -> Self {
        Self {
            dir: Some(
                sf.claude_dir
                    .clone()
                    .unwrap_or_else(|| user_home.join(".claude")),
            ),
            profile: sf.profile.clone(),
        }
    }

    /// What a record of the agent keeping these records names as its home
    /// ([`St::home`]), for a user whose home is `user_home`: `None` for the
    /// user's own `~/.claude`, else the directory, which [`Records::of`] reads
    /// back.
    pub(super) fn home_word(&self, user_home: &Path) -> Option<String> {
        self.dir
            .as_ref()
            .filter(|dir| **dir != user_home.join(".claude"))
            .map(|dir| dir.to_string_lossy().into_owned())
    }

    /// What a record of the agent keeping these records names as the profile
    /// its home keeps apart ([`St::home_profile`]): `None` but where
    /// [`profile_apart`] names one, which [`Records::of_recorded`] reads back.
    pub(super) fn profile_word(&self) -> Option<String> {
        self.profile
            .as_ref()
            .map(|profile| profile.to_string_lossy().into_owned())
    }

    /// The process `pid`'s record ([`session_file_in`]), stamped with the
    /// profile these records keep apart ([`SessionFile::profile`]).
    pub(super) fn session_file(&self, pid: u32) -> Option<SessionFile> {
        session_file_in(self.dir.as_deref()?, pid).map(|sf| self.stamped(sf))
    }

    /// Every record, complete or `None` ([`session_files_in`]), each stamped
    /// with the profile these records keep apart.
    pub(super) fn session_files(&self) -> Option<Vec<SessionFile>> {
        session_files_in(self.dir.as_deref()?)
            .map(|files| files.into_iter().map(|sf| self.stamped(sf)).collect())
    }

    /// `sf`, read here, as the home it was read under keeps it: whoever judges
    /// its agent by its login's model caches reads its profile
    /// ([`Records::of_file`]). Only a home [`profile_apart`] names one for
    /// differs from what the directory says, so every other record is as it
    /// was read.
    fn stamped(&self, sf: SessionFile) -> SessionFile {
        SessionFile {
            profile: self.profile.clone(),
            ..sf
        }
    }

    /// The profile Claude Code keeps for this home, for a user whose home is
    /// `user_home`: the one kept apart ([`profile_apart`]), else the
    /// directory's ([`claude_profile`]). `None` with no directory.
    pub(super) fn profile(&self, user_home: &Path) -> Option<PathBuf> {
        self.profile.clone().or_else(|| {
            self.dir
                .as_deref()
                .map(|dir| claude_profile(user_home, dir))
        })
    }

    /// The exact billing-profile identity from this process's captured exec
    /// environment and already resolved record directory. This is independent
    /// of [`Self::profile`], whose older model/access-cache identity is kept.
    pub(super) fn billing_context(
        &self,
        user_home: &Path,
        args: &atpkg::caller_shell::ProcArgs,
    ) -> Option<super::profile::Context> {
        let dir = self.dir()?;
        super::profile::Context::of_exec_env(user_home, dir, &args.env)
    }

    /// The one transcript of `session` ([`transcript_in`]).
    pub(super) fn transcript(&self, session: &str) -> Option<PathBuf> {
        transcript_in(self.dir.as_deref()?, session)
    }

    /// Whether any transcript of `session` exists ([`transcript_exists_in`]).
    pub(super) fn transcript_exists(&self, session: &str) -> Option<bool> {
        transcript_exists_in(self.dir.as_deref()?, session)
    }

    /// [`require_unique_owner`] over these records.
    pub(super) fn unique_owner(&self, wanted: &SessionFile) -> Result<(), &'static str> {
        preliminary_unique_owner(self.session_files().as_deref(), wanted)
    }

    /// [`tasked`] over these records.
    pub(super) fn tasked(&self, session: &str, ours: &[approvals::Typed]) -> Option<bool> {
        Tasked::default().of_in(self.dir.as_deref()?, session, ours)
    }

    /// [`Finished`] over these records, as the upgrade's look made at every
    /// visit asks it: kept per conversation in this process
    /// ([`LOOK_MEMOS`]), its first read starting [`FINISHED_ONCE_BYTES`]
    /// back as a restart's one read does ([`finished_where_written`]), and
    /// every later one reading on from where the last ended. A fresh
    /// [`TAIL_BYTES`] read at every visit read a finish with more than that
    /// of Claude Code's wakes after it as open, and `upgrade_ask` typed its
    /// notice into the finished task.
    pub(super) fn finished(&self, session: &str, ours: &[approvals::Typed]) -> Option<bool> {
        self.finished_in(&LOOK_MEMOS, session, ours)
    }

    /// [`Self::finished`], its memos kept in `memos` (the process's own,
    /// [`LOOK_MEMOS`], or a test's).
    pub(super) fn finished_in(
        &self,
        memos: &std::sync::Mutex<Vec<Finished>>,
        session: &str,
        ours: &[approvals::Typed],
    ) -> Option<bool> {
        let dir = self.dir.as_deref()?;
        let held = |m: &Finished| m.session == session && m.dir.as_deref() == Some(dir);
        let taken = memos.lock().ok().and_then(|mut memos| {
            let at = memos.iter().position(held)?;
            Some(memos.remove(at))
        });
        let mut memo = taken.unwrap_or_default();
        let finished = memo.of_in_from(dir, session, ours, FINISHED_ONCE_BYTES);
        if let Ok(mut memos) = memos.lock() {
            memos.retain(|m| !held(m));
            memos.push(memo);
            let over = memos.len().saturating_sub(LOOK_MEMOS_KEPT);
            memos.drain(..over);
        }
        finished
    }

    /// The directory, where one is known.
    pub(super) fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }
}

/// Whether conversation `session` has a task ([`upgrade::TaskScan`]), read
/// once: `Some(false)` when nobody but the harness has asked it anything (no
/// transcript among readable projects, or only the harness's own turns),
/// `Some(true)` once it holds someone else's prompt, `None` when nobody can say
/// (projects unreadable, the home missing, two transcripts claim it, or it is
/// longer than [`Tasked`] reads at once). A home with no projects holds no task
/// ([`transcript_exists_in`]). `ours`: what the session's supervisor typed, and
/// when ([`supervisor_typed`]), which is no one else's prompt.
#[must_use]
pub fn tasked(home: &Path, session: &str, ours: &[approvals::Typed]) -> Option<bool> {
    Tasked::default().of(home, session, ours)
}

/// What the supervisor of tab `tab` typed into it, and when, from its loop's
/// ledger ([`crate::supervise::approvals::typed_texts`] under
/// [`Opts::aterm_state`]): none when that root is not known. Into every
/// conversation the tab has held: [`upgrade::TaskScan`] places each text in
/// one by its time.
#[must_use]
pub fn supervisor_typed(opts: &Opts, tab: &str) -> Vec<approvals::Typed> {
    conversation_typed(opts, tab, None, &[])
}

/// The most earlier tabs a conversation's record keeps ([`St::tabs`]).
pub(super) const EARLIER_TABS: usize = 8;

/// WHAT THE HARNESS TYPED INTO A CONVERSATION, keyed to the conversation
/// and not to the tab it is in now: what the supervisor of `tab` typed
/// ([`supervisor_typed`]), and what the supervisors of the EARLIER tabs that
/// held it typed — `extra`, those `session`'s record names ([`St::tabs`],
/// and its [`St::tab`] where that is another), and those a cold restore
/// noted beside each tab's ledger read ([`note_earlier_tab`], followed tab
/// to tab, at most [`EARLIER_TABS`] in all). A cold restore gives the
/// reopened tab a new sid: read by the new tab's ledger alone, the old
/// loop's act after the worker's finish read as someone's prompt, and the
/// relaunch typed its carry-on into a finished task (XREV-LC-1) —
/// or, where no relaunch was made (a held tab, an agent a person started
/// there, a carried claim) and no record names the conversation, the loop
/// that later took the resumed conversation continued its finished task
/// (the decision review's RR-LC-3). Each ledger is read by its own sid; a
/// false match across tabs needs the same words within `TYPED_WITHIN_MS` of
/// a person's prompt in the same conversation.
///
/// An earlier tab's rows count only up to when the conversation left it
/// ([`St::tabs_until`], and [`upgrade::TYPED_WITHIN_MS`] past it): that tab
/// may go on supervising another conversation (the decision review's
/// RR-LC-4).
#[must_use]
pub fn conversation_typed(
    opts: &Opts,
    tab: &str,
    session: Option<&str>,
    extra: &[String],
) -> Vec<approvals::Typed> {
    let record = session.and_then(|s| load(opts, s));
    typed_through(opts, tab, session, record.as_ref(), extra, &[])
}

/// [`conversation_typed`] for the conversation of record `st` (its
/// [`St::tabs`] and their stamps, in memory, beside the record on disk).
pub(super) fn conversation_typed_by(
    opts: &Opts,
    tab: &str,
    session: &str,
    st: &St,
) -> Vec<approvals::Typed> {
    let record = load(opts, session);
    typed_through(
        opts,
        tab,
        Some(session),
        record.as_ref(),
        &st.tabs,
        &st.tabs_until,
    )
}

/// `stamps`: `(tab, until)` with `until` the newest for its tab.
fn stamp_tab(stamps: &mut Vec<(String, u64)>, tab: &str, until: u64) {
    match stamps.iter_mut().find(|(t, _)| t == tab) {
        Some((_, at)) => *at = (*at).max(until),
        None => stamps.push((tab.to_string(), until)),
    }
}

/// [`conversation_typed`]'s reading: `tab`, the earlier tabs the upgrade
/// ledger names for `session` ([`ledger_tabs`]), those `extra` and `record`
/// name, and those noted beside each ledger, each earlier tab's rows up to
/// its stamp in `until`, `record` or the ledger (the newest).
fn typed_through(
    opts: &Opts,
    tab: &str,
    session: Option<&str>,
    record: Option<&St>,
    extra: &[String],
    until: &[(String, u64)],
) -> Vec<approvals::Typed> {
    let Some(root) = opts.aterm_state.as_deref() else {
        return Vec::new();
    };
    let ledgered = session.map_or_else(Vec::new, |session| {
        ledger_tabs(
            std::fs::read_to_string(ledger_path(opts))
                .unwrap_or_default()
                .as_str(),
            session,
            tab,
        )
    });
    let mut stamps: Vec<(String, u64)> = Vec::new();
    for (t, at) in until
        .iter()
        .chain(record.iter().flat_map(|st| st.tabs_until.iter()))
    {
        stamp_tab(&mut stamps, t, *at);
    }
    for (t, at) in &ledgered {
        if let Some(at) = at {
            stamp_tab(&mut stamps, t, *at);
        }
    }
    // The ledger's first: the record's and the notes' are kept before them
    // within [`EARLIER_TABS`].
    let extra: Vec<String> = ledgered
        .into_iter()
        .map(|(t, _)| t)
        .chain(extra.iter().cloned())
        .collect();
    tabs_through(root, tab, record, &extra)
        .iter()
        .flat_map(|t| {
            let rows = approvals::typed_texts(&approvals::ledger_under(root, Some(t)), Some(t));
            let last_ms = (t != tab)
                .then(|| stamps.iter().find(|(s, _)| s == t))
                .flatten()
                .map(|(_, at)| {
                    i64::try_from(
                        at.saturating_mul(1000)
                            .saturating_add(upgrade::TYPED_WITHIN_MS),
                    )
                    .unwrap_or(i64::MAX)
                });
            rows.into_iter()
                .filter(move |r| last_ms.is_none_or(|last| r.at_ms <= last))
        })
        .collect()
}

/// THE EARLIER TABS THE UPGRADE LEDGER NAMES FOR CONVERSATION `session`,
/// now in `tab` (relaunch-7 of , the review's KR-4): every
/// other tab a row of the ledger text `body` ([`ledger_path`]) names it in —
/// a step the upgrade took there, which every release records with both its
/// tab and its conversation — oldest first, each with the second the ledger
/// last says it held the conversation until: the first row after the last
/// one naming the two together that names that tab with another
/// conversation, or the conversation in another tab; `None` while no row
/// says it left. A tab a cold restore minted before the leaf carried its sid
/// (no release before v0.106.0) has no note of the tab it was reopened from;
/// a conversation the upgrade ever moved there is linked to it here by what
/// the ledger recorded, never by a guess from text.
pub(super) fn ledger_tabs(body: &str, session: &str, tab: &str) -> Vec<(String, Option<u64>)> {
    let rows: Vec<(u64, String, String)> = body
        .lines()
        .filter_map(|line| {
            let v: Value = aterm_json::from_str(line).ok()?;
            let field = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
            Some((v.get("t")?.as_u64()?, field("tab")?, field("session")?))
        })
        .collect();
    let mut last: Vec<(String, usize)> = Vec::new();
    for (i, (_, t, s)) in rows.iter().enumerate() {
        if s == session && t != tab && super::tap::is_tab_id(t) {
            last.retain(|(x, _)| x != t);
            last.push((t.clone(), i));
        }
    }
    last.into_iter()
        .map(|(t, i)| {
            let left = rows.iter().skip(i + 1).find(|(_, rt, rs)| {
                (*rt == t && rs != session && rs != "-") || (rs == session && *rt != t && rt != "-")
            });
            let until = left.map(|(at, _, _)| *at);
            (t, until)
        })
        .collect()
}

/// The tabs a conversation in `tab` now was held in ([`typed_through`]'s):
/// `tab` first, then the earlier tabs `extra` and `record` name, then those
/// noted beside each ledger under `root` ([`note_earlier_tab`]), at most
/// [`EARLIER_TABS`] earlier ones in all.
fn tabs_through(root: &Path, tab: &str, record: Option<&St>, extra: &[String]) -> Vec<String> {
    tabs_through_read(
        root,
        tab,
        record,
        extra,
        &|p| std::fs::read_to_string(p).ok(),
        &|_| false,
    )
}

/// [`tabs_through`], each note beside a ledger read by `read` (a loop's
/// door, `supervise::Fx::read_text`, for [`lefts_beside_in`]), the notes
/// beside an earlier tab `ends` says ended the books before it not followed
/// ([`earlier_ledgers_in`]).
fn tabs_through_read(
    root: &Path,
    tab: &str,
    record: Option<&St>,
    extra: &[String],
    read: &dyn Fn(&Path) -> Option<String>,
    ends: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    let mut tabs = vec![tab.to_string()];
    tabs.extend(earlier_tabs(
        tab,
        extra.iter().chain(
            record
                .iter()
                .flat_map(|st| st.tabs.iter().chain(std::iter::once(&st.tab))),
        ),
    ));
    let mut at = 0;
    while at < tabs.len() {
        if at > 0 && ends(&tabs[at]) {
            at += 1;
            continue;
        }
        for t in noted_earlier_tabs_read(root, &tabs[at], read) {
            if tabs.len() > EARLIER_TABS {
                break;
            }
            if !tabs.contains(&t) {
                tabs.push(t);
            }
        }
        at += 1;
    }
    tabs
}

/// THE PERSON'S OWN EXCHANGES LEFT TO THEM by the loops of every tab that
/// held conversation `session` of record `st`, now in `tab`
/// ([`tabs_through`], [`approvals::open_left`]): the keys of the turns they
/// were left at (the review's RR3-TE-2). What a relaunch types nothing
/// into while one names the conversation's last turn
/// ([`turn_key_where_written`]).
pub(super) fn lefts_through(opts: &Opts, tab: &str, session: &str, st: &St) -> Vec<String> {
    let Some(root) = opts.aterm_state.as_deref() else {
        return Vec::new();
    };
    let record = load(opts, session);
    tabs_through(root, tab, record.as_ref(), &st.tabs)
        .iter()
        .filter_map(|t| approvals::open_left(&approvals::ledger_under(root, Some(t)), Some(t)))
        .collect()
}

/// [`lefts_through`] for a LOOP that keeps its ledger at `ledger` for
/// session `sid`: its own ledger's open left exchange, and — where the
/// ledger sits under a state root's `drive` (`approvals::path_under`) —
/// those of the earlier tabs a cold restore noted beside it
/// ([`note_earlier_tab`]): a restored tab's loop takes up the left exchange
/// of the loop that ran the conversation before aterm ended.
#[must_use]
pub fn lefts_beside(ledger: &Path, sid: Option<&str>) -> Vec<String> {
    let read = |p: &Path| std::fs::read_to_string(p).ok();
    lefts_beside_in(ledger, &read(ledger).unwrap_or_default(), sid, &read)
}

/// [`lefts_beside`] for a loop that read its own ledger at `ledger` once
/// (`body`) and reads every file beside it through `read` (its door,
/// `supervise::Fx::read_text`, main's 8533e1164: a harness tape holds what
/// the loop read).
#[must_use]
pub fn lefts_beside_in(
    ledger: &Path,
    body: &str,
    sid: Option<&str>,
    read: &dyn Fn(&Path) -> Option<String>,
) -> Vec<String> {
    let earlier: Vec<(String, String)> = earlier_ledgers_in(ledger, sid, read)
        .into_iter()
        .filter_map(|(t, p)| Some((t, read(&p)?)))
        .collect();
    lefts_of(body, sid, &earlier)
}

/// [`lefts_beside_in`] over the earlier tabs' ledgers already read
/// (`earlier`: each tab and its ledger's text): the open left exchange of
/// `body`'s session `sid`, then each earlier tab's own, each key once.
#[must_use]
pub fn lefts_of(body: &str, sid: Option<&str>, earlier: &[(String, String)]) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    let open = approvals::open_left_in(body, sid).into_iter().chain(
        earlier
            .iter()
            .filter_map(|(t, body)| approvals::open_left_in(body, Some(t))),
    );
    for key in open {
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    keys
}

/// THE EARLIER TABS' LEDGERS beside the loop ledger at `ledger` of session
/// `sid`: where the ledger sits under a state root's `drive`
/// (`approvals::path_under`), each tab a cold restore noted the session's tab
/// was reopened from ([`note_earlier_tab`], and those noted beside each of
/// theirs: [`tabs_through_read`]) with the path of its loop's ledger, each
/// note read through `read` (the loop's door). None elsewhere, or with no
/// session.
///
/// THE WALK ENDS AT A TAB WHOSE OWN BOOKS NO LONGER TAKE UP THOSE BEFORE IT
/// (the review of 3af6c3da7d, KRF-3): its ledger holds rows of its own
/// session and no take-up's row ([`approvals::took_up_earlier`]) — its loop's
/// cut dropped that row once no state the books before it began stood, as a
/// continuous ledger's cut drops their rows ([`approvals::bound`]), or a
/// build before the take-up wrote it. That tab's rows are of the books; the
/// tabs noted beside it, reached only through it, are not. Walked through, a
/// second cold restore (A, then B, then C) read A's model switch again where
/// B's loop had switched back and its cut had dropped that row, and its
/// loop relaunched the agent at the bucket's reset onto the model it ran.
#[must_use]
pub fn earlier_ledgers_in(
    ledger: &Path,
    sid: Option<&str>,
    read: &dyn Fn(&Path) -> Option<String>,
) -> Vec<(String, PathBuf)> {
    let root = ledger
        .parent()
        .filter(|d| d.file_name().is_some_and(|n| n == "drive"))
        .and_then(Path::parent);
    let (Some(root), Some(tab)) = (root, sid.map(|s| s.trim_start_matches('@'))) else {
        return Vec::new();
    };
    let ends = |t: &str| {
        read(&approvals::ledger_under(root, Some(t))).is_some_and(|body| {
            approvals::holds_rows_of(&body, t) && !approvals::took_up_earlier(&body, t)
        })
    };
    tabs_through_read(root, tab, None, &[], read, &ends)
        .into_iter()
        .skip(1)
        .map(|t| {
            let path = approvals::ledger_under(root, Some(&t));
            (t, path)
        })
        .collect()
}

/// Where a cold restore notes the tabs tab `tab` was reopened from
/// ([`note_earlier_tab`]): beside its loop's ledger under the aterm state
/// root `root` (`<dir>/<tab>.jsonl` → `<dir>/<tab>.earlier-tabs`), one sid a
/// line. Beside the ledger and not in it: a ledger is cut back to its newest
/// rows ([`approvals::bound`]), and this is said once.
fn earlier_tabs_path(root: &Path, tab: &str) -> PathBuf {
    let ledger = approvals::ledger_under(root, Some(tab));
    let stem = ledger
        .file_stem()
        .map_or_else(|| "self".into(), |s| s.to_string_lossy().into_owned());
    ledger.with_file_name(format!("{stem}.earlier-tabs"))
}

/// The tabs a cold restore noted tab `tab` was reopened from, oldest
/// first ([`note_earlier_tab`]); none where nothing was noted or the file
/// cannot be read.
fn noted_earlier_tabs(root: &Path, tab: &str) -> Vec<String> {
    noted_earlier_tabs_read(root, tab, &|p| std::fs::read_to_string(p).ok())
}

/// [`noted_earlier_tabs`], the note read by `read`.
fn noted_earlier_tabs_read(
    root: &Path,
    tab: &str,
    read: &dyn Fn(&Path) -> Option<String>,
) -> Vec<String> {
    read(&earlier_tabs_path(root, tab))
        .map(|body| {
            body.lines()
                .filter(|t| super::tap::is_tab_id(t) && *t != tab)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// A COLD RESTORE REOPENED TAB `tab` FROM TAB `was` (the decision review's
/// RR-LC-3): noted beside `tab`'s loop ledger, whatever the restore does
/// next — a relaunch records `was` in the conversation's record too
/// ([`St::tabs`]), but a held tab, a tab an agent ran in since, a carried
/// claim, or `[harness] relaunch = false` writes no record, and most
/// conversations have none. So what `was`'s loop typed stays the harness's
/// own for any loop that later takes a conversation in `tab`
/// ([`conversation_typed`]), never a prompt that reopens a finished task.
/// Once per `was`, the newest [`EARLIER_TABS`] kept; a kept sid (`was` is
/// `tab`) or an empty one notes nothing. Best effort: a write that fails
/// leaves the reading as it was before this note.
pub fn note_earlier_tab(opts: &Opts, tab: &str, was: &str) {
    if let Some(root) = opts.aterm_state.as_deref() {
        note_earlier_tab_under(root, tab, was);
    }
}

/// [`note_earlier_tab`] under the aterm state root `root`.
pub(crate) fn note_earlier_tab_under(root: &Path, tab: &str, was: &str) {
    if !super::tap::is_tab_id(tab) || !super::tap::is_tab_id(was) || was == tab {
        return;
    }
    let Some(body) = earlier_tab_note(tab, was, noted_earlier_tabs(root, tab)) else {
        return;
    };
    // Ordered: a hint for the done gate; a lost one leaves that tab un-noted.
    let _ = models::write_atomic_ordered(&earlier_tabs_path(root, tab), &body);
}

/// The earlier-tab note this new provenance would write, if it adds one.
/// Kept separate from file I/O so the actual note and its reader can be
/// exercised together without reading or writing anyone's ledger.
fn earlier_tab_note(tab: &str, was: &str, mut noted: Vec<String>) -> Option<String> {
    if !super::tap::is_tab_id(tab)
        || !super::tap::is_tab_id(was)
        || was == tab
        || noted.iter().any(|t| t == was)
    {
        return None;
    }
    noted.push(was.to_string());
    let over = noted.len().saturating_sub(EARLIER_TABS);
    noted.drain(..over);
    Some(noted.iter().map(|t| format!("{t}\n")).collect())
}

/// The earlier tabs `tabs` names for a conversation in `current` now, oldest
/// first: none empty, never `current`, each once (where it was last named),
/// the newest [`EARLIER_TABS`].
pub(super) fn earlier_tabs<'a>(
    current: &str,
    tabs: impl IntoIterator<Item = &'a String>,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in tabs {
        if !super::tap::is_tab_id(t) || t == current {
            continue;
        }
        out.retain(|o| o != t);
        out.push(t.clone());
    }
    let over = out.len().saturating_sub(EARLIER_TABS);
    out.drain(..over);
    out
}

/// [`tasked`] kept up for one conversation at a time, read on from where the
/// last read ended — a transcript is only appended to — so a caller that asks
/// at every turn end reads each row once. A conversation that has a task has
/// it for good: nothing is read for it again.
#[derive(Clone, Debug, Default)]
pub struct Tasked {
    /// The conversation this is about.
    session: String,
    /// Its transcript, once found (the projects are scanned for it once).
    path: Option<PathBuf>,
    /// How far into its transcript the rows are folded in (a whole line's end).
    read_to: u64,
    scan: upgrade::TaskScan,
}

/// The most of a transcript one [`Tasked::of`] reads: a task shows at the
/// first answered prompt near the head, and a taskless conversation is a few
/// harness turns; a longer record with none shown is read on at the next ask.
const TASK_READ_BYTES: u64 = 8 << 20;

impl Tasked {
    /// [`tasked`], for `session` — read on from where the last read of the
    /// same conversation ended, from its start for another one or for a
    /// transcript that shrank (it was rewritten). `ours` is read afresh at
    /// every ask: the supervisor records a turn as it types it, before the
    /// row it wrote can be read here.
    pub fn of(&mut self, home: &Path, session: &str, ours: &[approvals::Typed]) -> Option<bool> {
        self.of_in(&home.join(".claude"), session, ours)
    }

    /// [`Tasked::of`] of the conversation the relaunch snapshot `snap` names, in
    /// the agent's own records ([`Records::of_snapshot`]; `user_home` is the
    /// user's), as every relaunch reads them. `None` where it names no
    /// conversation, or its records cannot be found (a relative home whose
    /// directory is unknown).
    /// The FILED file, never where a moved agent writes now
    /// ([`conversation_dir`]): the task's prompt is in it by construction,
    /// and a worktree home's file opens after the move with none (read there,
    /// a fresh memo found a tasked worker taskless).
    pub fn of_snapshot(
        &mut self,
        user_home: &Path,
        snap: &super::relaunch::Snapshot,
        ours: &[approvals::Typed],
    ) -> Option<bool> {
        let session = snap.session.as_deref()?;
        let records = Records::of_snapshot(user_home, snap);
        self.of_in(records.dir()?, session, ours)
    }

    /// [`Tasked::of`] in the Claude Code directory `claude_dir` itself.
    pub(super) fn of_in(
        &mut self,
        claude_dir: &Path,
        session: &str,
        ours: &[approvals::Typed],
    ) -> Option<bool> {
        if self.session != session {
            *self = Tasked {
                session: session.to_string(),
                ..Tasked::default()
            };
        }
        if self.scan.settled() {
            return Some(true);
        }
        self.scan.supervisor_typed(ours);
        if self.path.as_ref().is_none_or(|p| !p.is_file()) {
            self.path = transcript_in(claude_dir, session);
        }
        let Some(path) = self.path.as_ref() else {
            // No message yet among projects that can be read: no task.
            return transcript_exists_in(claude_dir, session)
                .and_then(|any| (!any).then_some(false));
        };
        let mut f = std::fs::File::open(path).ok()?;
        let len = f.metadata().ok()?.len();
        if len < self.read_to {
            self.read_to = 0;
            self.scan = upgrade::TaskScan::default();
            self.scan.supervisor_typed(ours);
        }
        f.seek(SeekFrom::Start(self.read_to)).ok()?;
        let mut buf = Vec::new();
        f.take(TASK_READ_BYTES).read_to_end(&mut buf).ok()?;
        let at_end = self.read_to.saturating_add(buf.len() as u64) >= len;
        // Whole lines only: one caught half-written is read next time.
        let whole = buf.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
        buf.truncate(whole);
        self.read_to = self.read_to.saturating_add(whole as u64);
        for line in lossy(buf).lines() {
            if self.scan.line(line) && self.scan.settled() {
                return Some(true);
            }
        }
        // A command of someone else's still waiting for its turn is a task
        // already; nothing of anyone's is none only once the end was read.
        if self.scan.tasked() {
            return Some(true);
        }
        at_end.then_some(false)
    }
}

/// WHETHER CONVERSATION `session`'S TASK IS FINISHED BY ITS OWN RECORD
/// ([`upgrade::FinishedScan`]), kept up for one conversation at a time and
/// read on from where the last read ended — what the window's host answers
/// its loop with at every point a turn ran (`supervise::IdleHost::finished`).
/// Not [`Tasked`], which stops reading for good once a task shows: a finish
/// is said and taken back as the record grows. The first read starts
/// before the transcript's end, on a line's start — only what follows the
/// last finish can take it back — [`FINISHED_ONCE_BYTES`] back for the loop's
/// memo ([`Self::of_snapshot`]: a new loop is a restart's read of a tab whose
/// background tasks may have woken it for hours since its landing), and every
/// later read goes on from its offset, so a finish folded once stays folded
/// however long the wakes after it run (a fixed tail would lose it). Another
/// conversation, or a transcript that shrank (rewritten), is read afresh the
/// same way.
#[derive(Clone, Debug, Default)]
pub struct Finished {
    /// The conversation this is about.
    session: String,
    /// Its transcript, once found.
    path: Option<PathBuf>,
    /// The Claude Code directory [`Self::path`] was found in: a conversation
    /// whose agent moved where it writes (XT-3, [`conversation_dir`]) is
    /// found afresh there, though the file it read is still a file.
    dir: Option<PathBuf>,
    /// The transcript has been read: [`Self::read_to`] is where.
    begun: bool,
    /// How far into the transcript the rows are folded in (a whole line's end).
    read_to: u64,
    /// The last read reached the transcript's end: [`Self::scan`] holds its
    /// last rows.
    at_end: bool,
    scan: upgrade::FinishedScan,
}

impl Finished {
    /// `Some(finished)` for `session` by the record of the agent the relaunch
    /// snapshot `snap` names, in ITS OWN RECORDS ([`Records::of_snapshot`]),
    /// as [`Tasked::of_snapshot`] reads them; `None` where it names no
    /// conversation, its records cannot be found, no transcript is there, or
    /// more was written since the last read than one read takes. Its first
    /// read starts [`FINISHED_ONCE_BYTES`] back, as a relaunch's one read
    /// does ([`finished_where_written`]): read from [`TAIL_BYTES`], a new loop
    /// on a finished tab with more wakes than that after its finish (a
    /// supervisor restart, a cold restore) read it open, and the next wake's
    /// end was continued.
    ///
    /// Read where the agent writes its conversation NOW ([`conversation_dir`],
    /// XT-3): a relative home's agent that entered a worktree writes under
    /// the worktree's home, and its finish said there is the one that counts.
    pub fn of_snapshot(
        &mut self,
        user_home: &Path,
        snap: &super::relaunch::Snapshot,
        ours: &[approvals::Typed],
    ) -> Option<bool> {
        self.of_snapshot_at(user_home, snap, ours, || live_cwd(snap))
    }

    /// The turn the conversation ends on, by its key, as the last read
    /// ([`Self::of_snapshot`]) left it ([`upgrade::FinishedScan::last_turn_key`]:
    /// read as far back as the finish is, so an idle compaction's block after
    /// the turn hides it not; the merge review's XR-1). `None` where that read
    /// did not reach the transcript's end, or the record names no turn.
    #[must_use]
    pub fn last_turn_key(&self) -> Option<String> {
        self.at_end.then(|| self.scan.last_turn_key()).flatten()
    }

    /// The turn the conversation ends on, by every key that names it
    /// ([`upgrade::LastTurn`]: its last row's and the prompt that opened
    /// it), as the last read left it: `None` where that read did not reach
    /// the transcript's end.
    #[must_use]
    pub fn last_turn(&self) -> Option<upgrade::LastTurn> {
        self.at_end.then(|| self.scan.last_turn())
    }

    /// The Claude Code directory this memo last read its conversation in:
    /// where the agent writes it now ([`conversation_dir`]).
    #[must_use]
    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    /// [`Finished::of_snapshot`], the agent's working directory read by
    /// `cwd` (asked only under a relative home).
    pub(super) fn of_snapshot_at(
        &mut self,
        user_home: &Path,
        snap: &super::relaunch::Snapshot,
        ours: &[approvals::Typed],
        cwd: impl FnOnce() -> Option<PathBuf>,
    ) -> Option<bool> {
        let session = snap.session.as_deref()?;
        let dir = conversation_dir(user_home, snap, session, cwd)?;
        self.of_in_from(&dir, session, ours, FINISHED_ONCE_BYTES)
    }

    /// [`Finished::of_snapshot`] in the Claude Code directory `claude_dir`
    /// itself, its first read [`TAIL_BYTES`] back: a test's fresh tail. `ours`
    /// is read afresh at every ask, as [`Tasked::of`]'s.
    #[cfg(all(test, unix))]
    pub(super) fn of_in(
        &mut self,
        claude_dir: &Path,
        session: &str,
        ours: &[approvals::Typed],
    ) -> Option<bool> {
        self.of_in_from(claude_dir, session, ours, TAIL_BYTES)
    }

    /// [`Finished::of_snapshot`] in the Claude Code directory `claude_dir`
    /// itself, its first read starting `back` bytes before the transcript's
    /// end (at most [`TASK_READ_BYTES`], one read's worth) — and, where what
    /// that read holds SETTLES NOTHING ([`upgrade::FinishedScan::settled`]:
    /// no prompt of someone's, no check or land act, no answer that lands,
    /// only rows that leave the finish as it stood before them), read again
    /// from twice as far back, until a read settles it or starts at the
    /// record's head. A window of wakes alone cannot say whether the task was
    /// finished before it: read from [`FINISHED_ONCE_BYTES`] back and no
    /// further, a finish behind more of its background tasks' wakes than that read open,
    /// and a relaunch typed its carry-on into the finished task
    /// . Later reads go on from where the last ended, one
    /// read's worth at a time. `ours` is read afresh at every ask, as
    /// [`Tasked::of`]'s.
    fn of_in_from(
        &mut self,
        claude_dir: &Path,
        session: &str,
        ours: &[approvals::Typed],
        back: u64,
    ) -> Option<bool> {
        if self.session != session {
            *self = Finished {
                session: session.to_string(),
                ..Finished::default()
            };
        }
        self.scan.supervisor_typed(ours);
        if self.dir.as_deref() != Some(claude_dir)
            || self.path.as_ref().is_none_or(|p| !p.is_file())
        {
            let found = transcript_in(claude_dir, session);
            if found != self.path {
                self.begun = false;
            }
            self.path = found;
            self.dir = Some(claude_dir.to_path_buf());
        }
        self.at_end = false;
        let path = self.path.as_ref()?;
        let mut f = std::fs::File::open(path).ok()?;
        let len = f.metadata().ok()?.len();
        if self.begun && len >= self.read_to {
            let at_end = self.fold_on(&mut f, len, false)?;
            self.at_end = at_end;
            return at_end.then(|| self.scan.finished());
        }
        // The first read, or a transcript rewritten: from a line's start
        // `back` bytes before its end — the byte before it read too, so a
        // window that begins on a line's start keeps that line — to the end,
        // and again from twice as far back while it settles nothing.
        self.begun = true;
        let mut back = back.min(TASK_READ_BYTES - 1);
        loop {
            self.scan = upgrade::FinishedScan::default();
            self.scan.supervisor_typed(ours);
            self.read_to = len.saturating_sub(back).saturating_sub(1);
            let from = self.read_to;
            let mut skip_cut = from != 0;
            loop {
                let before = self.read_to;
                if self.fold_on(&mut f, len, std::mem::take(&mut skip_cut))? {
                    break;
                }
                if self.read_to == before {
                    return None;
                }
            }
            if from == 0 || self.scan.settled() {
                break;
            }
            back = back.saturating_mul(2);
        }
        self.at_end = true;
        Some(self.scan.finished())
    }

    /// One read's worth ([`TASK_READ_BYTES`]) of the transcript `f` (`len`
    /// bytes long when asked) folded in from [`Self::read_to`], whole lines
    /// only — one caught half-written is read next time — its first line
    /// skipped where `skip_cut` (a read begun inside a line), and whether it
    /// reached the end. `None` where it cannot be read.
    fn fold_on(&mut self, f: &mut std::fs::File, len: u64, skip_cut: bool) -> Option<bool> {
        f.seek(SeekFrom::Start(self.read_to)).ok()?;
        let mut buf = Vec::new();
        Read::take(&mut *f, TASK_READ_BYTES)
            .read_to_end(&mut buf)
            .ok()?;
        let at_end = self.read_to.saturating_add(buf.len() as u64) >= len;
        let cut = if skip_cut {
            buf.iter()
                .position(|b| *b == b'\n')
                .map_or(buf.len(), |i| i + 1)
        } else {
            0
        };
        let whole = buf.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
        let whole = whole.max(cut);
        self.read_to = self.read_to.saturating_add(whole as u64);
        for line in lossy(buf[cut..whole].to_vec()).lines() {
            self.scan.line(line);
        }
        Some(at_end)
    }
}

/// Where the Claude Code directory a MOVED agent writes its conversation in
/// is noted ([`note_writes`]): `<state>/<session>.writes`, one path.
fn writes_note_path(opts: &Opts, session: &str) -> PathBuf {
    state_dir(opts).join(format!("{session}.writes"))
}

/// THE DIRECTORY A MOVED AGENT WRITES ITS CONVERSATION IN, NOTED WHILE IT
/// LIVES (the decision review's RR-LC-5): `dir`, where the window host's
/// finished read found the conversation of `snap` ([`Finished::dir`]: under
/// a relative home, an agent that entered a worktree writes under the
/// worktree's home, XT-3), where that is not the directory its record was
/// filed in. Once the process is gone its working directory is no one's to
/// read, and the relaunch's done:finished gate read the filed file — ending
/// on the person's prompt and the move — and typed its carry-on into a task
/// the worker finished in the worktree. Written only when it changes; best
/// effort.
pub fn note_writes(opts: &Opts, snap: &super::relaunch::Snapshot, dir: &Path) {
    let Some(session) = snap.session.as_deref() else {
        return;
    };
    let filed = Records::of_snapshot(&opts.home, snap);
    if filed.dir().is_none_or(|f| same_dir(f, dir)) {
        return;
    }
    let text = dir.to_string_lossy();
    let path = writes_note_path(opts, session);
    if std::fs::read_to_string(&path).is_ok_and(|t| t.trim_end() == text) {
        return;
    }
    let _ = models::write_atomic(&path, &format!("{text}\n"));
}

/// THE DIRECTORY A RELAUNCH READS CONVERSATION `session`'S FINISH IN, its
/// agent gone ([`note_writes`]): the one its agent was noted writing in,
/// where that transcript's newest row is newer than the filed one's (in
/// `records`, [`Records::dir`]) — the worker's last words are there — else
/// the filed directory. A filed transcript written since (the relaunched
/// agent resumes the filed file, and a person's prompt there reopens the
/// task) is the newer, and read. The TASK stays the filed file's to say
/// ([`Tasked`]).
pub(super) fn finish_dir(opts: &Opts, records: &Records, session: &str) -> Option<PathBuf> {
    let filed = records.dir().map(Path::to_path_buf);
    let noted = std::fs::read_to_string(writes_note_path(opts, session))
        .ok()
        .map(|t| PathBuf::from(t.trim_end()))
        .filter(|d| !d.as_os_str().is_empty());
    let newest = |dir: &Path| transcript_in(dir, session).and_then(|p| newest_row_at(&p));
    noted
        .filter(|n| {
            newest(n).is_some_and(|w| filed.as_deref().and_then(newest).is_none_or(|f| w > f))
        })
        .or(filed)
}

/// WHERE THE AGENT `snap` NAMES WENT ON WRITING CONVERSATION `session`, NOTED
/// AS A RELAUNCH OR A RESTART FIRST LOOKS AT IT ('s relative
/// home wrong action): the host notes it at its finished read
/// ([`note_writes`]), and a crash (or aterm's own end) between a worker's
/// last turn in a worktree's home and that read sent the relaunch to the
/// filed transcript — ending on the person's prompt and the move — which
/// read the task open, so its carry-on was typed into the task the worker
/// finished in the worktree. So the look that has the snapshot in hand
/// finds it itself: under a RELATIVE home (empty included) its home
/// resolved where the agent runs now, while that is still the snapshot's
/// process ([`live_cwd`]), and under each worktree of the repository it
/// started in (`cwd`, [`repo_worktrees`]: a `process.chdir` moves it only
/// into a worktree, 2.1.300's `EnterWorktree`), at the worktree's root and
/// at the start's own place inside it; the one whose transcript of
/// `session` has the newest row is noted where that row is newer than the
/// note's and the filed one's. [`finish_dir`] then reads it as it reads the
/// host's note. An unset or absolute home names one directory wherever it
/// runs: nothing to find. Best effort, as [`note_writes`].
pub(super) fn note_moved_writes(
    opts: &Opts,
    snap: &super::relaunch::Snapshot,
    session: &str,
    cwd: &str,
) {
    let Some(home) = snap
        .home
        .as_deref()
        .filter(|home| Path::new(home).is_relative())
    else {
        return;
    };
    let filed = Records::of_snapshot(&opts.home, snap);
    let Some(filed_dir) = filed.dir().map(Path::to_path_buf) else {
        return;
    };
    let newest = |dir: &Path| transcript_in(dir, session).and_then(|p| newest_row_at(&p));
    let start = Path::new(cwd);
    let mut bases: Vec<PathBuf> = live_cwd(snap).into_iter().collect();
    if start.is_absolute() {
        let inside = repo_root(start).and_then(|root| {
            start
                .strip_prefix(root)
                .ok()
                .map(Path::to_path_buf)
                .filter(|rel| !rel.as_os_str().is_empty())
        });
        for wt in repo_worktrees(start) {
            if let Some(rel) = inside.as_deref() {
                bases.push(wt.join(rel));
            }
            bases.push(wt);
        }
    }
    let noted = std::fs::read_to_string(writes_note_path(opts, session))
        .ok()
        .map(|t| PathBuf::from(t.trim_end()))
        .filter(|d| !d.as_os_str().is_empty());
    let floor = [Some(filed_dir.clone()), noted]
        .into_iter()
        .flatten()
        .filter_map(|d| newest(&d))
        .max();
    let found = bases
        .into_iter()
        .filter_map(|base| {
            Records::at(&opts.home, HomeVars::config(Some(home)), || Some(base))
                .dir()
                .map(Path::to_path_buf)
        })
        .filter(|dir| !same_dir(dir, &filed_dir))
        .filter_map(|dir| newest(&dir).map(|at| (at, dir)))
        .filter(|(at, _)| floor.is_none_or(|f| *at > f))
        .max_by_key(|(at, _)| *at);
    if let Some((_, dir)) = found {
        note_writes(opts, snap, &dir);
    }
}

/// The root of the git worktree `dir` is in: the nearest directory at or
/// above it holding a `.git` (a repository's directory or a worktree's
/// file).
fn repo_root(dir: &Path) -> Option<&Path> {
    dir.ancestors()
        .take(REPO_ANCESTORS)
        .find(|d| std::fs::symlink_metadata(d.join(".git")).is_ok())
}

/// EVERY WORKTREE OF THE REPOSITORY `dir` IS IN, as git itself registers
/// them, nothing run: the main worktree (the directory of the common git
/// directory) and each linked one (`<common>/worktrees/*/gitdir`, each
/// naming `<worktree>/.git`), where it is a directory. The common git
/// directory is `dir`'s repository's `.git`, or — `dir` in a linked
/// worktree — the one its `.git` file's `gitdir:` and that directory's
/// `commondir` lead to. Each git file is read small and regular
/// ([`super::footer::read_small`]); at most [`REPO_WORKTREES`] linked ones.
/// None outside a repository.
fn repo_worktrees(dir: &Path) -> Vec<PathBuf> {
    let Some(top) = repo_root(dir) else {
        return Vec::new();
    };
    let dot_git = top.join(".git");
    let common = if dot_git.is_dir() {
        dot_git
    } else {
        let Some(gitdir) = super::footer::read_small(&dot_git, 4096).and_then(|t| {
            t.lines()
                .find_map(|l| l.strip_prefix("gitdir:"))
                .map(|g| top.join(g.trim()))
        }) else {
            return Vec::new();
        };
        super::footer::read_small(&gitdir.join("commondir"), 4096)
            .map_or_else(|| gitdir.clone(), |c| gitdir.join(c.trim()))
    };
    let mut out: Vec<PathBuf> = common
        .file_name()
        .filter(|n| *n == ".git")
        .and(common.parent())
        .map(Path::to_path_buf)
        .into_iter()
        .collect();
    if let Ok(linked) = std::fs::read_dir(common.join("worktrees")) {
        out.extend(
            linked
                .flatten()
                .take(REPO_WORKTREES)
                .filter_map(|e| super::footer::read_small(&e.path().join("gitdir"), 4096))
                .map(|t| PathBuf::from(t.trim()))
                .filter(|p| p.is_absolute())
                .filter_map(|p| p.parent().map(Path::to_path_buf)),
        );
    }
    out.retain(|wt| std::fs::symlink_metadata(wt).is_ok_and(|m| m.is_dir()));
    out.sort();
    out.dedup();
    out
}

/// How far up from a start directory [`repo_root`] looks for a `.git`.
const REPO_ANCESTORS: usize = 32;

/// How many linked worktrees [`repo_worktrees`] reads.
const REPO_WORKTREES: usize = 64;

/// When the newest row of the transcript at `path` that says so was
/// written (its `timestamp`, unix seconds), read from its last
/// [`TAIL_BYTES`].
fn newest_row_at(path: &Path) -> Option<u64> {
    let (tail, _) = tail_to_end(path, TAIL_BYTES);
    tail.lines().rev().find_map(|line| {
        let v: Value = aterm_json::from_str(line).ok()?;
        models::parse_utc(v.get("timestamp")?.as_str()?)
    })
}

/// [`Records::finished`] read from further back ([`FINISHED_ONCE_BYTES`]),
/// for a read made once a restart (its carry-on, its restore look): the
/// landing of a finished tab whose background tasks kept waking it for hours
/// is still in the window — and read where the agent last wrote the
/// conversation ([`finish_dir`]): what a relaunch's carry-on and its
/// restore look end a finished task on.
pub(super) fn finished_where_written(
    opts: &Opts,
    records: &Records,
    session: &str,
    ours: &[approvals::Typed],
) -> Option<bool> {
    WhereWritten::new(opts, records, session, ours).finished()
}

/// ONE READ OF CONVERSATION `session`'S RECORD WHERE IT WAS WRITTEN
/// ([`read_where_written`]), made when first asked and shared by every
/// question one relaunch's carry-on asks of it: whether its task is finished
/// ([`finished_where_written`], the relaunch's `nothing_owed`), then whether
/// its last words are a done report ([`Self::said_done`]). The read
/// reaches back past a window that settles nothing, so on a conversation the
/// harness's own `keep going` drives it reads to the last person's prompt
/// (the review of , KR-DONE-3: 2.1 s for 109 MiB, a debug
/// build), and each question read it again.
pub(super) struct WhereWritten<'a> {
    opts: &'a Opts,
    records: &'a Records,
    session: &'a str,
    ours: &'a [approvals::Typed],
    scan: std::cell::OnceCell<Option<upgrade::FinishedScan>>,
}

impl<'a> WhereWritten<'a> {
    /// Nothing read yet.
    pub(super) fn new(
        opts: &'a Opts,
        records: &'a Records,
        session: &'a str,
        ours: &'a [approvals::Typed],
    ) -> Self {
        Self {
            opts,
            records,
            session,
            ours,
            scan: std::cell::OnceCell::new(),
        }
    }

    /// The records the conversation is read in.
    pub(super) fn records(&self) -> &'a Records {
        self.records
    }

    /// The conversation.
    pub(super) fn session(&self) -> &'a str {
        self.session
    }

    /// What the harness typed into it.
    pub(super) fn ours(&self) -> &'a [approvals::Typed] {
        self.ours
    }

    /// The one read: `None` where nothing can be read to the end.
    fn scan(&self) -> Option<&upgrade::FinishedScan> {
        self.scan
            .get_or_init(|| read_where_written(self.opts, self.records, self.session, self.ours))
            .as_ref()
    }

    /// [`finished_where_written`], by the one read.
    pub(super) fn finished(&self) -> Option<bool> {
        self.scan().map(upgrade::FinishedScan::finished)
    }

    /// Whether the conversation's last words are a done report
    /// ([`upgrade::FinishedScan::said_done`]), and whether it names an
    /// irreversible act ([`upgrade::FinishedScan::said_irreversible`]), by the
    /// one read: what a relaunch's carry-on under Cruise types the Cruise
    /// prompt after (the live lab's LIVE-RL-1), and how
    /// ([`upgrade::checking`], the review's RR4-RL-3).
    pub(super) fn said_done(&self) -> Option<(bool, bool)> {
        self.scan()
            .map(|scan| (scan.said_done(), scan.said_irreversible()))
    }
}

/// THE TURN CONVERSATION `session` ENDS ON, by its key
/// ([`upgrade::FinishedScan::last_turn_key`]), read as [`finished_where_written`]
/// reads its finish — from [`FINISHED_ONCE_BYTES`] back, where the agent last
/// wrote it ([`finish_dir`]): what a left exchange is matched by (the
/// review's RR3-TE-2). Read from a [`TAIL_BYTES`] tail, Claude Code's idle
/// compaction after the person's exchange (its boundary, summary and
/// restored files: past 256 KiB) left no turn in the window, the key was
/// `None`, and the relaunch typed its carry-on into the exchange (the merge
/// review's XR-1). `None` where nothing can be read.
pub(super) fn turn_key_where_written(
    opts: &Opts,
    records: &Records,
    session: &str,
) -> Option<upgrade::LastTurn> {
    Some(read_where_written(opts, records, session, &[])?.last_turn())
}

/// Whether conversation `session` of record `st`, now in tab `tab`, ends on
/// a person's own exchange a loop left to them: an open `left-to-person@v1`
/// row of any tab that held it ([`lefts_through`]) names its last turn
/// ([`turn_key_where_written`], [`upgrade::LastTurn::names`]). A turn since —
/// anyone's prompt, a wake's answer — names another, and a loop's act since
/// closed the row. What a relaunch types nothing into (`done:left`) and an
/// upgrade asks nothing at.
pub(super) fn left_to_person(
    opts: &Opts,
    records: &Records,
    tab: &str,
    session: &str,
    st: &St,
) -> bool {
    let lefts = lefts_through(opts, tab, session, st);
    !lefts.is_empty()
        && turn_key_where_written(opts, records, session)
            .is_some_and(|now| lefts.iter().any(|k| now.names(k)))
}

/// [`finished_where_written`]'s one read, whole: the record folded from
/// [`FINISHED_ONCE_BYTES`] back to its end, where the agent last wrote it
/// ([`finish_dir`]). `None` where nothing can be read to the end.
fn read_where_written(
    opts: &Opts,
    records: &Records,
    session: &str,
    ours: &[approvals::Typed],
) -> Option<upgrade::FinishedScan> {
    let mut memo = Finished::default();
    memo.of_in_from(
        &finish_dir(opts, records, session)?,
        session,
        ours,
        FINISHED_ONCE_BYTES,
    )?;
    Some(memo.scan)
}

/// The upgrade look's [`Finished`] memos ([`Records::finished`]), the most
/// recently asked last: one per conversation the process looked at, at most
/// [`LOOK_MEMOS_KEPT`].
static LOOK_MEMOS: std::sync::Mutex<Vec<Finished>> = std::sync::Mutex::new(Vec::new());

/// How many conversations [`LOOK_MEMOS`] keeps: past it the least recently
/// asked is forgotten, and read afresh from [`FINISHED_ONCE_BYTES`] back if
/// it is asked again.
const LOOK_MEMOS_KEPT: usize = 64;

/// How much of a transcript's end is read for the READY answer and the model
/// the agent ran.
pub(super) const TAIL_BYTES: u64 = 262_144;

/// How far back a restart's one read of whether the task is finished starts
/// ([`finished_where_written`]), and a new loop's memo's first
/// ([`Finished::of_snapshot`]): half of one read's worth
/// ([`TASK_READ_BYTES`]), sixteen times [`TAIL_BYTES`].
pub(super) const FINISHED_ONCE_BYTES: u64 = TASK_READ_BYTES / 2;

/// The one transcript holding `session` under the user's home `home` (every
/// reader of an agent's own reads it in its records: [`Records::transcript`]).
#[cfg(all(test, unix))]
pub(super) fn transcript(home: &Path, session: &str) -> Option<PathBuf> {
    transcript_in(&home.join(".claude"), session)
}

/// [`transcript`] in the Claude Code directory `claude_dir` itself.
pub(super) fn transcript_in(claude_dir: &Path, session: &str) -> Option<PathBuf> {
    let mut found = None;
    for e in std::fs::read_dir(claude_dir.join("projects"))
        .ok()?
        .flatten()
    {
        let p = e.path().join(format!("{session}.jsonl"));
        if p.is_file() {
            if found.is_some() {
                return None;
            }
            found = Some(p);
        }
    }
    found
}

/// The tail of the transcript of the conversation the relaunch snapshot `snap`
/// names, in its agent's own records ([`Records::of_snapshot`]; `user_home` is
/// the user's): the one bounded read ([`TAIL_BYTES`]) of
/// [`named_limit_reset`], [`named_standing_wall`] and [`named_reset_line`].
/// `None`: no conversation named, or no transcript read.
///
/// Read where the agent writes it NOW ([`conversation_dir`], XT-3): the
/// standing limit of a relative home's agent that entered a worktree is in
/// the worktree home's file, the start home's standing still.
fn snapshot_tail(user_home: &Path, snap: &super::relaunch::Snapshot) -> Option<String> {
    snapshot_tail_at(user_home, snap, || live_cwd(snap))
}

/// [`snapshot_tail`], the agent's working directory read by `cwd`.
pub(super) fn snapshot_tail_at(
    user_home: &Path,
    snap: &super::relaunch::Snapshot,
    cwd: impl FnOnce() -> Option<PathBuf>,
) -> Option<String> {
    let session = snap.session.as_deref()?;
    let path = transcript_in(&conversation_dir(user_home, snap, session, cwd)?, session)?;
    Some(tail_to_end(&path, TAIL_BYTES).0)
}

/// THE CLAUDE CODE DIRECTORY THAT HOLDS THE CONVERSATION `session` THE AGENT
/// `snap` NAMES WRITES NOW (the cross-lane review's XT-3): under a RELATIVE
/// home (empty included), the home its live working directory names
/// ([`Records::at`] with `cwd`, as [`process_transcript_tail`] reads it) —
/// once Claude Code changes directory (2.1.300's `EnterWorktree`) the start
/// home's transcript stands still and the worktree home's grows (MEASURED,
/// the homes lane's h2wtobs1) — where its transcript is there; else where it
/// filed its record ([`Records::of_snapshot`]): an absolute or unset home,
/// the same directory, or nothing written under the new one yet. The
/// conversation's own TASK stays the filed file's to say ([`Tasked`]: the
/// worktree file holds no prompt of the person's), and so does every
/// relaunch-time read: the relaunched agent resumes the file it filed.
fn conversation_dir(
    user_home: &Path,
    snap: &super::relaunch::Snapshot,
    session: &str,
    cwd: impl FnOnce() -> Option<PathBuf>,
) -> Option<PathBuf> {
    let filed = Records::of_snapshot(user_home, snap)
        .dir()
        .map(Path::to_path_buf);
    let writing = snap
        .home
        .as_deref()
        .filter(|home| Path::new(home).is_relative())
        .and_then(|home| {
            Records::at(user_home, HomeVars::config(Some(home)), cwd)
                .dir()
                .map(Path::to_path_buf)
        })
        .filter(|w| filed.as_deref().is_none_or(|f| !same_dir(w, f)))
        .filter(|w| transcript_in(w, session).is_some());
    writing.or(filed)
}

/// The working directory of the agent `snap` names, read only while the
/// process is still the snapshot's (its kernel start): a reused pid's is no
/// one's.
fn live_cwd(snap: &super::relaunch::Snapshot) -> Option<PathBuf> {
    (kernel_start(snap.pid).as_deref() == Some(snap.start.as_str()))
        .then(|| aterm_sysprobe::process_cwd(snap.pid))
        .flatten()
}

/// The reset named by the standing usage limit of the conversation `snap`
/// names ([`upgrade::transcript_limit_reset`] over [`snapshot_tail`]; past or
/// future; `None`: none named or no transcript read). A supervisor loop whose
/// limit episode read no reset of its own, or a clock time with no day, asks
/// its host for this (`supervise::IdleHost::limit_until`): a notice left on
/// screen [`LIMIT_GRACE_S`] past its reset counts as an idle point.
#[must_use]
pub fn named_limit_reset(user_home: &Path, snap: &super::relaunch::Snapshot) -> Option<u64> {
    upgrade::transcript_limit_reset(&snapshot_tail(user_home, snap)?)
}

/// The tail of the transcript Claude Code process `pid` writes now, which a
/// supervisor confirms a wall on its screen by
/// (`supervise::IdleHost::transcript_tail`). Found as the footer finds a
/// process's conversation: the record its Claude Code directory keeps of the
/// process (whose `procStart` must be `start`, so a reused pid's record is no
/// one's, [`super::footer::session_of_pid`]) and its transcript
/// ([`super::footer::sole_transcript_path`]): the last [`TAIL_BYTES`] of a
/// regular file. `None`: no record, no transcript yet, an ambiguous one, or
/// one that cannot be opened.
///
/// A relative home (empty included) names two directories once its Claude Code
/// changes directory (a worktree): the record stays where it was filed at
/// start ([`Records::of_process`]) while the conversation goes on under the
/// home its directory now names ([`Records::at`]). So the id comes from the
/// record and the transcript from where it writes now; with nothing written
/// there yet, or its directory unread, there is no tail and the screen decides.
#[must_use]
pub fn process_transcript_tail(
    config_dir: Option<&str>,
    home: Option<&str>,
    started_in: Option<&str>,
    pid: u32,
    start: &str,
) -> Option<String> {
    let vars = (config_dir, home, started_in);
    process_transcript_end(vars, pid, start, TAIL_BYTES).map(|(_, tail)| tail)
}

/// [`process_transcript_tail`] with the file it read, and more of its end
/// ([`super::result::TRANSCRIPT_READ_BYTES`]): what a tasked session's result
/// is snapshotted from at the done edge — the land act's prompt and the
/// report before it, past a landing's replies that ran the suites again — the
/// path kept for `result full` to read later
/// (`supervise::IdleHost::transcript`).
#[must_use]
pub fn process_transcript(
    config_dir: Option<&str>,
    home: Option<&str>,
    started_in: Option<&str>,
    pid: u32,
    start: &str,
) -> Option<(PathBuf, String)> {
    let vars = (config_dir, home, started_in);
    process_transcript_end(vars, pid, start, super::result::TRANSCRIPT_READ_BYTES)
}

/// The transcript the Claude Code `pid` started at `start` writes now, and at
/// most the last `bytes` of it ([`process_transcript_tail`]), under its
/// `(CLAUDE_CONFIG_DIR, HOME, PWD)` at exec.
fn process_transcript_end(
    (config_dir, home, started_in): (Option<&str>, Option<&str>, Option<&str>),
    pid: u32,
    start: &str,
    bytes: u64,
) -> Option<(PathBuf, String)> {
    let cwd = aterm_sysprobe::process_cwd(pid);
    // Where its record is filed, and where it writes its conversation now,
    // by the one resolver: an unset home under its own `HOME`, else this
    // account's.
    let vars = HomeVars {
        config: config_dir,
        home,
    };
    let user_home = home
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())
        .or_else(atpkg::platform::account_home);
    let user_home = match (config_dir, user_home) {
        (_, Some(user_home)) => user_home,
        // A set home names its directory without one.
        (Some(_), None) => PathBuf::new(),
        (None, None) => return None,
    };
    let filed = Records::of_process(&user_home, vars, pid, || cwd.clone(), started_in);
    let writes = Records::at(&user_home, vars, || cwd.clone());
    let (filed, writes) = (filed.dir()?.to_path_buf(), writes.dir()?.to_path_buf());
    let started = models::parse_lstart(start)?;
    let entry = super::footer::session_of_pid(&filed, pid, Some(started))?;
    // The project's own place is the directory it runs in now.
    let entry = super::footer::SessionEntry {
        cwd: cwd.unwrap_or(entry.cwd),
        ..entry
    };
    let path = super::footer::sole_transcript_path(&writes, &entry)?;
    let mut f = super::footer::open_regular(&path)?;
    let len = f.metadata().ok()?.len();
    let tail = tail_window(&mut f, len, bytes).0;
    Some((path, tail))
}

/// [`process_transcript_tail`] of the Claude Code `snap` names, its exec
/// environment read afresh (a process's environment at exec never moves; an
/// unreadable one is read as the footer reads it, aterm's own home's
/// `.claude`). `None` for a Codex, which writes no such transcript.
#[must_use]
pub fn snapshot_transcript_tail(snap: &super::relaunch::Snapshot) -> Option<String> {
    snapshot_transcript_end(snap, TAIL_BYTES).map(|(_, tail)| tail)
}

/// [`snapshot_transcript_tail`] with the file it read, and as much of its end
/// as [`process_transcript`] reads: what a tasked session's result is
/// snapshotted from.
#[must_use]
pub fn snapshot_transcript(snap: &super::relaunch::Snapshot) -> Option<(PathBuf, String)> {
    snapshot_transcript_end(snap, super::result::TRANSCRIPT_READ_BYTES)
}

/// The transcript the Claude Code `snap` names, and at most the last `bytes`
/// of it ([`snapshot_transcript_tail`]).
fn snapshot_transcript_end(
    snap: &super::relaunch::Snapshot,
    bytes: u64,
) -> Option<(PathBuf, String)> {
    match &snap.run {
        super::relaunch::AgentRun::Claude => {}
        super::relaunch::AgentRun::Codex(_)
        | super::relaunch::AgentRun::Muse(_)
        | super::relaunch::AgentRun::Agy(_)
        | super::relaunch::AgentRun::Gemini(_) => return None,
    }
    let args = atpkg::caller_shell::process_args(snap.pid);
    let home = aterm_types::dirs::home_dir();
    let (config_dir, home, started_in) = match &args {
        Some(args) => (
            args.env_var(super::accounts::HOME_ENV),
            args.env_var(super::accounts::HOMEDIR_ENV),
            args.env_var("PWD"),
        ),
        None => (None, home.as_deref().and_then(Path::to_str), None),
    };
    process_transcript_end((config_dir, home, started_in), snap.pid, &snap.start, bytes)
}

/// The quota wall the conversation `snap` names stands at now
/// ([`upgrade::transcript_standing_wall`] over [`snapshot_tail`]; `None`: no
/// quota rejection stands last, or no transcript read), which the window's host
/// hands the accounts recorder (`accounts::record_wall`). It reads; it records
/// nothing.
#[must_use]
pub fn named_standing_wall(
    user_home: &Path,
    snap: &super::relaunch::Snapshot,
) -> Option<upgrade::StandingWall> {
    upgrade::transcript_standing_wall(&snapshot_tail(user_home, snap)?)
}

/// When the newest answer of the conversation `snap` names came, of those by
/// the second `by` ([`upgrade::transcript_last_answer`] over [`snapshot_tail`];
/// `None`: no answer by then, or no transcript read): the instant the session's
/// screen's word on its account's usage was true, which the window's host hands
/// the accounts recorder (`accounts::record_usage`). It reads; it records
/// nothing.
#[must_use]
pub fn named_last_answer(
    user_home: &Path,
    snap: &super::relaunch::Snapshot,
    by: u64,
) -> Option<u64> {
    upgrade::transcript_last_answer(&snapshot_tail(user_home, snap)?, by)
}

/// The line the usage-limit dialog of the conversation `snap` names ended with
/// since `since` (unix seconds; [`upgrade::transcript_reset_line`] over
/// [`snapshot_tail`]; `None`: no such row yet, or no transcript read): what the
/// window's host reads a claimed limit reset's outcome from. It reads; it
/// records nothing.
#[must_use]
pub fn named_reset_line(
    user_home: &Path,
    snap: &super::relaunch::Snapshot,
    since: u64,
) -> Option<String> {
    upgrade::transcript_reset_line(&snapshot_tail(user_home, snap)?, since)
}

/// Whether conversation `session` is kept under `home`: a transcript of that
/// name is in `<home>/.claude/projects/*/`. What a host with no process to read
/// (a reopened layout's agent) decides "the agent's Claude home was the default
/// one" on. `false` for a name that is no session id (it is joined into a
/// path), a conversation with no message yet, and where the projects cannot be
/// listed. It lists directory names; no transcript is opened.
#[must_use]
pub fn conversation_under(home: &Path, session: &str) -> bool {
    upgrade::is_session_id(session) && transcript_exists(home, session) == Some(true)
}

/// The last `bytes` of `path`, decoded lossily: a cut inside a multi-byte
/// character would make a strict decode refuse the whole window, hiding a
/// READY on the last line. Lossy spoils only the already-cut first line (a
/// U+FFFD that [`upgrade::transcript_has_ready`] skips as not JSON) and a
/// last line caught half-written.
///
/// Returns the byte offset where the read ended, the mark a later read starts
/// from ([`since`]), taken from what was read rather than a separately asked
/// length, so an append between the two is never skipped.
pub(super) fn tail_to_end(path: &Path, bytes: u64) -> (String, u64) {
    let Ok(mut f) = std::fs::File::open(path) else {
        return (String::new(), 0);
    };
    let len = f.metadata().map_or(0, |m| m.len());
    tail_window(&mut f, len, bytes)
}

/// [`tail_to_end`]'s read of `f`, measured `len` long. The read is bounded too,
/// since a writer may append after `len` was measured, and the mark returned is
/// the end actually read, so the next [`since`] read picks up every byte the
/// cap left behind.
fn tail_window(f: &mut (impl Read + Seek), len: u64, bytes: u64) -> (String, u64) {
    let start = len.saturating_sub(bytes);
    if f.seek(SeekFrom::Start(start)).is_err() {
        return (String::new(), 0);
    }
    let mut buf = Vec::new();
    let _ = f.take(bytes).read_to_end(&mut buf);
    let end = start.saturating_add(buf.len() as u64);
    (lossy(buf), end)
}

/// At most `cap` bytes of `path` from byte `from` on, decoded lossily like
/// [`tail_to_end`]. Empty when the file is shorter than `from`: a transcript
/// that shrank was rewritten, and nothing in it can be told to come after the
/// mark.
pub(super) fn since(path: &Path, from: u64, cap: u64) -> String {
    let Ok(mut f) = std::fs::File::open(path) else {
        return String::new();
    };
    if f.metadata().map_or(0, |m| m.len()) < from || f.seek(SeekFrom::Start(from)).is_err() {
        return String::new();
    }
    let mut buf = Vec::new();
    let _ = f.take(cap).read_to_end(&mut buf);
    lossy(buf)
}

fn lossy(buf: Vec<u8>) -> String {
    String::from_utf8(buf).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

// ---------------------------------------------------------------- the sweep

/// One sweep: every live Claude Code session advanced by at most one step
/// (a restart counts as one: exit, relaunch and continue, each bounded), then
/// every restart a previous sweep left in flight carried on.
///
/// One actor at a time, machine-wide: a hand-run `aterm harness upgrade`, the
/// window's host ([`step`]) and a relaunch on exit
/// ([`super::relaunch::after_exit`]) share `<state>/upgrade/sweep.lock`; one
/// that finds it held reports `busy` and does nothing, or two would type the
/// notice twice.
#[must_use]
pub fn sweep(opts: &Opts) -> Vec<Report> {
    sweep_with_roster(opts, None)
}

/// The window's step for one tab (`opts.only_sid` on the instance
/// `opts.sock`): its foreground Claude Code, or a restart left in flight,
/// advanced one step at an idle point its supervisor parked at (the host's
/// worker calls this when [`due`] says so). Every proof the sweep makes is
/// made here: the instance's PTY roster names the tab uniquely, and every act
/// re-reads it. `current` when there is nothing to do.
#[must_use]
pub fn step(opts: &Opts) -> Report {
    let tab = opts.only_sid.clone().unwrap_or_else(|| "-".to_string());
    let mut self_claimed = false;
    let tabs = connect(opts, &tab).ok().and_then(|mut c| {
        self_claimed = status_line(&mut c, &tab).is_some_and(|l| self_claimed_by_status(&l));
        host_roster(&mut c)
    });
    let current = || Report {
        pid: 0,
        tab: tab.clone(),
        session: "-".to_string(),
        from: "-".to_string(),
        to: "-".to_string(),
        step: "current".to_string(),
    };
    // aterm takes no upgrade decision on a session its own agent supervises
    // under a proven self-claim (the native harness contract §7).
    if self_claimed {
        return Report {
            step: SKIP_SELF_CLAIMED.to_string(),
            ..current()
        };
    }
    let Some(tabs) = tabs.filter(|tabs| tab_is_live(tabs, &tab)) else {
        return Report {
            step: "wait:tab-not-live".to_string(),
            ..current()
        };
    };
    let reports = sweep_with_roster(opts, Some(&tabs));
    pick(&reports, &tab).cloned().unwrap_or_else(current)
}

/// The one word [`step`] answers with: the tab's own act, else its wait, then
/// any act, then any wait. Another tab's act never stands for this tab's wait
/// (it would be left unmoved until the next activation notice); a current tab
/// leaves the other's word.
fn pick<'a>(reports: &'a [Report], tab: &str) -> Option<&'a Report> {
    let own = |r: &&Report| r.tab == tab;
    reports
        .iter()
        .filter(own)
        .find(|r| r.is_act())
        .or_else(|| reports.iter().filter(own).find(|r| r.step != "current"))
        .or_else(|| reports.iter().find(|r| r.is_act()))
        .or_else(|| reports.iter().find(|r| r.step != "current"))
}

/// Whether tab `opts.only_sid` has an upgrade to take: its foreground
/// Claude Code runs an older build or a model the model rule moves
/// ([`super::upgrade_models`]), or a restart of this tab is in flight — each
/// only while that agent's lane is on ([`Opts::agents_off`]), as [`step`]
/// takes it.
/// Read-only and cheap enough to ask at worker start, every activation notice
/// and every look: one roster read, session files when needed, cached build
/// versions, the model list. A failed read decides nothing ([`Due::Unread`]),
/// neither parking nor releasing on a guess. A whole read with nothing to act
/// on is [`Due::No`]: nothing to move to, a Codex (never moved live), an agent
/// past its launch with no record of its own ([`RECORD_WINDOW_S`]).
#[must_use]
pub fn due(opts: &Opts) -> Due {
    let Some(tab) = opts.only_sid.as_deref() else {
        return Due::No;
    };
    if !in_flight(opts).is_empty() || confirming(opts) {
        return Due::Yes;
    }
    let Ok(mut c) = connect(opts, tab) else {
        return Due::Unread(UNREAD_SOCKET);
    };
    // A proven self-claim's session is its agent's to upgrade (contract §7).
    if status_line(&mut c, tab).is_some_and(|l| self_claimed_by_status(&l)) {
        return Due::No;
    }
    let Some(tabs) = host_roster(&mut c) else {
        return Due::Unread(UNREAD_SOCKET);
    };
    // The lanes past Claude Code's, each by name in their fixed order
    // ([`lane_due`]) — or the tab is the Claude Code half's to read.
    due_after_lanes(opts, &tabs, &|agent| lane_due(opts, &tabs, agent))
}

/// Whether the lane of `agent`, past Claude Code's, owns tab
/// `opts.only_sid` and what is due there. `None`: the tab is not that lane's.
/// Each lane by name.
fn lane_due(opts: &Opts, tabs: &[LiveTab], agent: upgrade::Agent) -> Option<Due> {
    match agent {
        // No Codex is moved live (the Codex screen lane's retirement,
        // 2026-10-16): a Codex tab is due only for a restart in flight
        // ([`in_flight`]), and Claude Code's half finds no record of it.
        upgrade::Agent::Codex => None,
        // Never among the others: its half is read last ([`due_after_lanes`]).
        upgrade::Agent::Claude => None,
        // Muse Code's live upgrade (its quiet flow): a tab its TUI leads is
        // due while its build is behind the twin's by their store records
        // ([`muse::live::upgrade_due`]).
        upgrade::Agent::Muse => muse::live::upgrade_due(opts, tabs),
        // agy's live upgrade (its quiet flow), the same way by its own
        // records ([`agy::live::upgrade_due`]).
        upgrade::Agent::Agy => agy::live::upgrade_due(opts, tabs),
        // Gemini CLI: no upgrade lane, so no move is due ([`lane_on`] keeps
        // this off; no `due` of its own is written).
        upgrade::Agent::Gemini => None,
    }
}

/// A lane past Claude Code's owns a tab whose foreground leader is its agent,
/// even when its build is current or its process
/// could not be read whole ([`UNREAD_PROCESS`]); only a foreground no such
/// lane owns (`lane_due` = `None` for every agent of
/// [`upgrade::Agent::others`], asked in that order) can have a Claude session
/// record that makes an upgrade due. A separate seam so the read-heavy Claude
/// roster is provably skipped on every other lane's answer, and on every
/// answer while the Claude lane is off. A lane that is off ([`lane_on`]) has
/// nothing due.
fn due_after_lanes(
    opts: &Opts,
    tabs: &[LiveTab],
    lane_due: &dyn Fn(upgrade::Agent) -> Option<Due>,
) -> Due {
    for &agent in upgrade::Agent::others() {
        if let Some(due) = lane_due(agent) {
            return if lane_on(opts, agent) { due } else { Due::No };
        }
    }
    let Some(tab) = opts
        .only_sid
        .as_deref()
        .filter(|_| lane_on(opts, upgrade::Agent::Claude))
    else {
        return Due::No;
    };
    let homes = Homes::read(
        opts,
        Some(tabs),
        atpkg::caller_shell::process_args,
        aterm_sysprobe::process_cwd,
    );
    let Some(files) = homes.roster(claude_records(&opts.home)) else {
        return Due::Unread(UNREAD_FILES);
    };
    match due_among(
        opts,
        &files,
        tabs,
        || Targets::read(&opts.home),
        atpkg::caller_shell::process_args,
        process_group,
        ids,
    ) {
        // No record of the tab's agent, whose home could not be named: one
        // may stand where nothing was read.
        Due::Unread(UNREAD_RECORD) if homes.unnamed(tab) => Due::Unread(UNREAD_PROCESS),
        Due::Unread(UNREAD_RECORD) => no_record(tabs, tab, started_s, alive, now_s()),
        due => due,
    }
}

/// Claude Code's session records read whole ([`session_files`]): none where it
/// keeps none (no `.claude/sessions`, e.g. another `CLAUDE_CONFIG_DIR`), and
/// `None` when one could not be read whole (half-written, or gone between the
/// listing and the read).
fn claude_records(home: &Path) -> Option<Vec<SessionFile>> {
    session_files(home).or_else(|| (!home.join(".claude/sessions").is_dir()).then(Vec::new))
}

/// The Claude homes one pass reads past the user's own `~/.claude` (which each
/// reader reads first): every directory a Claude Code in the instance's tabs
/// may keep its records under (one started with `CLAUDE_CONFIG_DIR=<dir>`
/// files them in `<dir>` only). A home is learned from the exec environment of
/// the tab's foreground leader, resolved where Claude Code filed its record
/// ([`Records::of_process`]); only the tab `opts.only_sid` names, when it names
/// one. With no roster of its own (the CLI's sweep), every instance's tabs
/// ([`instance_tabs`]) plus the homes the records name ([`St::home`]).
///
/// A live leader whose home cannot be named is never read as under the user's
/// home: its tab is [`Homes::unnamed`], and a look that finds no record of its
/// agent waits ([`UNREAD_PROCESS`]). Only a pass's own roster names such tabs:
/// the instances' tabs a pass without one reads teach it homes alone, since many
/// of their leaders (a `sudo` shell, another user's process) cannot be read.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Homes {
    /// The homes' records, the user's own excluded, each directory once.
    dirs: Vec<Records>,
    /// The tabs whose leader's home could not be named.
    unnamed: Vec<String>,
}

impl Homes {
    /// The homes of `tabs` (with none, of every instance's tabs, then of the
    /// records), each leader's exec record read by `read_args` and, for a
    /// relative home, its working directory by `read_cwd`. A home read off a
    /// live leader comes first: [`Homes::add`] keeps the first reading of a
    /// directory, and a record's word is only as fresh as the visit that wrote
    /// it.
    pub(super) fn read(
        opts: &Opts,
        tabs: Option<&[LiveTab]>,
        mut read_args: impl FnMut(u32) -> Option<atpkg::caller_shell::ProcArgs>,
        mut read_cwd: impl FnMut(u32) -> Option<PathBuf>,
    ) -> Self {
        let mut homes = Homes::default();
        // With no roster of its own, the instances' tabs only teach homes:
        // none of their waits is this pass's to report.
        let every;
        let (tabs, learned) = match tabs {
            Some(tabs) => (tabs, false),
            None => {
                every = instance_tabs(opts);
                (every.as_slice(), true)
            }
        };
        let ours = tabs
            .iter()
            .filter(|tab| opts.only_sid.as_deref().is_none_or(|only| only == tab.sid));
        for tab in ours {
            let Some(leader) = tab.fgpgid.and_then(|group| u32::try_from(group).ok()) else {
                continue;
            };
            // A leader gone since the roster leads nothing: no agent is there.
            let unnamed = |homes: &mut Homes| {
                if !learned && alive(leader) {
                    homes.unnamed.push(tab.sid.clone());
                }
            };
            let Some(args) = read_args(leader) else {
                unnamed(&mut homes);
                continue;
            };
            // Unset: under the leader's own `HOME` — the user's own, which
            // every reader reads first, unless it was launched with another.
            let records = Records::of_process(
                &opts.home,
                HomeVars::of(&args),
                leader,
                || read_cwd(leader),
                args.env_var("PWD"),
            );
            if records.dir().is_some() {
                homes.add(&opts.home, records);
            } else {
                unnamed(&mut homes);
            }
        }
        if learned {
            for records in recorded_homes(opts) {
                homes.add(&opts.home, records);
            }
        }
        homes
    }

    /// `records`, unless their directory is the user's own
    /// `user_home/.claude` or already here — the same directory however
    /// spelled (a link, a `..`): read twice, every record in it would read
    /// as a second owner of its own conversation.
    fn add(&mut self, user_home: &Path, records: Records) {
        let Some(dir) = records.dir() else {
            return;
        };
        if !same_dir(dir, &user_home.join(".claude"))
            && !self
                .dirs
                .iter()
                .filter_map(Records::dir)
                .any(|d| same_dir(d, dir))
        {
            self.dirs.push(records);
        }
    }

    /// The homes the records of `sessions` name ([`St::home`]): where the
    /// owner's view reads their holders (`upgrade_status`'s `holders`).
    pub(super) fn of_records(opts: &Opts, sessions: &std::collections::BTreeSet<String>) -> Self {
        let mut homes = Homes::default();
        for session in sessions {
            if let Some(st) = load(opts, session).filter(|st| st.home.is_some()) {
                homes.add(&opts.home, st.agent_records(&opts.home));
            }
        }
        homes
    }

    /// The homes' directories, in the order read.
    #[cfg(all(test, unix))]
    pub(super) fn dirs(&self) -> Vec<PathBuf> {
        self.dirs
            .iter()
            .filter_map(Records::dir)
            .map(Path::to_path_buf)
            .collect()
    }

    /// Whether tab `sid`'s leader's home could not be named.
    pub(super) fn unnamed(&self, sid: &str) -> bool {
        self.unnamed.iter().any(|tab| tab == sid)
    }

    /// The records of the user's home (`first`, read by the caller's own
    /// rule) and of every home here ([`session_files_in`]), as one roster:
    /// `None` when any could not be read whole, as the user's own fails a
    /// pass. A home with no `sessions/` at all keeps none yet (a Claude Code
    /// files its record moments after it starts, [`RECORD_WINDOW_S`]).
    pub(super) fn roster(&self, first: Option<Vec<SessionFile>>) -> Option<Vec<SessionFile>> {
        let mut files = first?;
        for records in &self.dirs {
            match records.session_files() {
                Some(more) => files.extend(more),
                None if records.dir().is_some_and(|dir| {
                    matches!(
                        std::fs::symlink_metadata(dir.join("sessions")),
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound
                    )
                }) => {}
                None => return None,
            }
        }
        Some(files)
    }
}

/// Every live tab on the machine, for a pass with no roster of its own (the
/// CLI's sweep, [`Homes::read`]): the `who` roster of every instance
/// [`Opts::instances`] names, each bounded by [`CLAIMS_WAIT`]; tab ids are
/// unique across instances. An explicit `--sock`, or a machine directory that
/// cannot be listed, asks the one instance it names.
fn instance_tabs(opts: &Opts) -> Vec<LiveTab> {
    let roster = |mut c: Client| {
        c.transport().set_read_timeout(Some(CLAIMS_WAIT)).ok()?;
        host_roster(&mut c)
    };
    let one = || {
        connect_reads(opts, opts.only_sid.as_deref().unwrap_or(""))
            .ok()
            .and_then(roster)
            .unwrap_or_default()
    };
    if opts.sock.is_some() {
        return one();
    }
    let socks = match &opts.instances {
        Instances::Machine => match aterm_ctl::local_instances() {
            Ok(instances) if !instances.is_empty() => {
                instances.into_iter().map(|(_, sock)| sock).collect()
            }
            _ => return one(),
        },
        Instances::Listed(socks) => socks.clone(),
    };
    socks
        .iter()
        .filter_map(|sock| roster_connection(sock))
        .filter_map(roster)
        .flatten()
        .collect()
}

/// The Claude homes the conversations' records name ([`St::home`]): what a
/// pass with no roster reads past the user's own ([`Homes::read`]).
fn recorded_homes(opts: &Opts) -> Vec<Records> {
    let Ok(dir) = std::fs::read_dir(state_dir(opts)) else {
        return Vec::new();
    };
    dir.flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| St::from_json(&std::fs::read_to_string(e.path()).ok()?))
        .filter(|st| driven_here(st.agent) && st.home.is_some())
        .map(|st| st.agent_records(&opts.home))
        .collect()
}

/// [`due`]'s Claude Code half for tab `opts.only_sid`, over a complete read of
/// the session records (`files`) and the instance's tabs, with the candidate
/// builds read by `read_targets` (at most once) and the process reads
/// injected ([`tab_agent`]). `Yes` when the record whose agent leads the tab
/// is behind, `No` when one leads and is not, [`UNREAD_VERSION`] when a
/// candidate build's `--version` did not answer in time, [`UNREAD_PROCESS`]
/// when a behind leader's process could not be read whole, and
/// [`UNREAD_RECORD`] when no record is the tab's agent's (for [`no_record`]).
fn due_among(
    opts: &Opts,
    files: &[SessionFile],
    tabs: &[LiveTab],
    read_targets: impl Fn() -> Targets,
    read_args: impl FnMut(u32) -> Option<atpkg::caller_shell::ProcArgs>,
    read_group: impl FnMut(u32) -> Option<i64>,
    read_ids: impl FnMut(u32) -> Option<(u32, i64, i64)>,
) -> Due {
    let Some(tab) = opts.only_sid.as_deref() else {
        return Due::No;
    };
    let targets = std::cell::OnceCell::new();
    let mctx = std::cell::OnceCell::new();
    // The tab's agent wanted nothing of what answered, and a build did not.
    let unanswered = std::cell::Cell::new(false);
    let wants = |sf: &SessionFile| {
        let tg = targets.get_or_init(&read_targets);
        let wanted = needs_upgrade_inspection(sf, tg, load(opts, &sf.session_id).as_ref())
            || model_wants_a_look(opts, sf, mctx.get_or_init(|| ModelCtx::read(opts, tg)));
        unanswered.set(!wanted && tg.unanswered);
        wanted
    };
    match tab_agent(tab, files, tabs, wants, read_args, read_group, read_ids) {
        TabAgent::Claimed(_) => Due::Yes,
        TabAgent::Nothing if unanswered.get() => Due::Unread(UNREAD_VERSION),
        TabAgent::Nothing => Due::No,
        TabAgent::Unread => Due::Unread(UNREAD_PROCESS),
        TabAgent::NoRecord => Due::Unread(UNREAD_RECORD),
    }
}

/// What a look read of the Claude Code that leads one tab ([`tab_agent`]).
#[derive(Debug, PartialEq, Eq)]
enum TabAgent<'a> {
    /// The record whose agent leads the tab, one the look asked about, its
    /// process read whole.
    Claimed(&'a SessionFile),
    /// A record whose agent leads the tab was read, and there is nothing to
    /// ask: the look wants none of it (a current build), or its environment
    /// names another tab.
    Nothing,
    /// A record the look asked about leads the tab, but its process could
    /// not be read whole ([`host_candidates`]'s wait).
    Unread,
    /// No record's agent leads the tab.
    NoRecord,
}

/// What a look at tab `tab` reads of its Claude Code among the session
/// records (`files`) and the instance's tabs ([`host_candidates`]): the
/// leading records asked about as `wants` says, with process reads injected.
/// A record counts as read only where it decides (claimed whole, wanted by
/// nothing, another tab's by its environment), never where its process could
/// not be read, so a failed `ps` never reads as "not due".
fn tab_agent<'a>(
    tab: &str,
    files: &'a [SessionFile],
    tabs: &[LiveTab],
    mut wants: impl FnMut(&SessionFile) -> bool,
    read_args: impl FnMut(u32) -> Option<atpkg::caller_shell::ProcArgs>,
    read_group: impl FnMut(u32) -> Option<i64>,
    read_ids: impl FnMut(u32) -> Option<(u32, i64, i64)>,
) -> TabAgent<'a> {
    let mut leads = false;
    let seen = host_candidates(
        files,
        tabs,
        |sf, t| {
            let ours = t == tab;
            leads |= ours;
            ours && wants(sf)
        },
        read_args,
        read_group,
        read_ids,
    );
    if let Some(sf) = seen
        .iter()
        .find_map(|c| c.as_ref().ok().map(|(sf, _, _)| *sf))
    {
        TabAgent::Claimed(sf)
    } else if !seen.is_empty() {
        TabAgent::Unread
    } else if leads {
        TabAgent::Nothing
    } else {
        TabAgent::NoRecord
    }
}

/// How long a tab's agent may run with no record of its own before a look
/// takes that as final ([`no_record`]). Claude Code writes
/// `sessions/<pid>.json` shortly after starting, so an agent running this long
/// without one keeps none a look can find (another `CLAUDE_CONFIG_DIR`, a
/// non-interactive session), and the step, which acts only on a record, has
/// nothing to take.
pub const RECORD_WINDOW_S: u64 = 30;

/// A look that found no record of the tab's agent: a wait ([`UNREAD_RECORD`])
/// while the tab's foreground job (its group leader, started at the unix
/// second `started` reads) is younger than [`RECORD_WINDOW_S`] at `now`, and
/// nothing to take once older. A leader whose start cannot be read is a
/// process not read ([`UNREAD_PROCESS`]) while it lives (`alive`), and nothing
/// to take once gone; likewise a tab the instance no longer has.
fn no_record(
    tabs: &[LiveTab],
    tab: &str,
    started: impl FnOnce(u32) -> Option<u64>,
    alive: impl FnOnce(u32) -> bool,
    now: u64,
) -> Due {
    let Some(live) = tabs.iter().find(|t| t.sid == tab) else {
        return Due::No;
    };
    let Some(leader) = live.fgpgid.and_then(|group| u32::try_from(group).ok()) else {
        return Due::Unread(UNREAD_PROCESS);
    };
    match started(leader) {
        Some(at) if now.saturating_sub(at) >= RECORD_WINDOW_S => Due::No,
        Some(_) => Due::Unread(UNREAD_RECORD),
        None if alive(leader) => Due::Unread(UNREAD_PROCESS),
        None => Due::No,
    }
}

/// The unix second `pid` started, as the kernel says ([`kernel_start`]).
pub(super) fn started_s(pid: u32) -> Option<u64> {
    models::parse_lstart(&kernel_start(pid)?)
}

/// What [`due`] read of a tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Due {
    /// An upgrade to take: the tab's agent is behind, a restart of it is in
    /// flight, or its carry-on's model is still to be read.
    Yes,
    /// Read whole, and nothing to take.
    No,
    /// A read failed, so nothing is decided: the socket did not answer
    /// ([`UNREAD_SOCKET`]), Claude Code's records could not be read whole
    /// ([`UNREAD_FILES`]), the agent's process could not be
    /// ([`UNREAD_PROCESS`]), a launch record is not written yet
    /// ([`UNREAD_RECORD`], for at most [`RECORD_WINDOW_S`]), or a candidate
    /// build's `--version` did not answer in time ([`UNREAD_VERSION`], until
    /// the version cache asks again). The word is the look's wait
    /// (`wait:<word>`), and the host looks again soon rather than treating the
    /// session as not due.
    Unread(&'static str),
}

/// [`Due::Unread`]'s word for a look the instance's socket did not answer.
pub const UNREAD_SOCKET: &str = "no-socket";
/// [`Due::Unread`]'s word for a look that found no record of the tab's
/// agent among Claude Code's session records while it may still be written
/// ([`RECORD_WINDOW_S`]).
pub const UNREAD_RECORD: &str = "no-record";
/// [`Due::Unread`]'s word for a look that could not read the tab agent's
/// process whole (ids, argv or start; a `ps` that could not spawn, or the
/// foreground moving between reads), for Claude Code and Codex alike.
pub const UNREAD_PROCESS: &str = "no-process";
/// [`Due::Unread`]'s word for a look that could not read Claude Code's
/// session records whole: the sweep's own wait for it.
pub const UNREAD_FILES: &str = "session-files-unreadable";
/// The wait of a look at a behind agent that has left its record
/// ([`Records::left_by`]: a worktree under a relative Claude home since it
/// filed it), whose status that record no longer keeps: nothing is asked of it
/// or restarted until it is back where its record is.
pub const HOME_MOVED: &str = "home-moved";
/// [`Due::Unread`]'s word for a look whose tab agent is behind nothing that
/// answered while a build it could move to did not answer `--version` within
/// [`VERSION_WAIT`] (a first-run check, a loaded machine): unknown until the
/// version cache asks again ([`VERSION_RETRY_FIRST`], doubling). As "not due"
/// it would wait for the next activation notice.
pub const UNREAD_VERSION: &str = "no-version";

/// Mints the session's upgrade state the moment its host sees it behind: at
/// the worker's attach and at every activation notice, an agent that is tab
/// `opts.only_sid`'s foreground job on an older build than one it may move to
/// gets its upgrade state minted as the first step would (`Pending`, target,
/// tab, `pending_since` = `since`), so the owner's view counts time behind
/// from then rather than from the first step after a busy turn. A Claude Code
/// state is minted where none stands, or where the standing one finished with
/// another build ([`behind_anew`]); a Codex is never moved live, so has
/// nothing to note. A state in flight or already on this build is left
/// alone. Nothing is typed, read off the screen or signalled.
/// [`Behind::Busy`] when another actor holds the lock and [`Behind::Unread`]
/// when a read failed ([`due`]'s words; at a fresh launch the record may not
/// be written yet): both asked again. An agent past its launch with no record
/// ([`RECORD_WINDOW_S`]) has nothing to note.
pub fn note_behind(opts: &Opts, since: u64) -> Behind {
    let Some(tab) = opts.only_sid.as_deref().filter(|_| !opts.dry_run) else {
        return Behind::Nothing;
    };
    let Ok(_held) = sweep_lock(opts) else {
        return Behind::Busy;
    };
    let Some(tabs) = connect(opts, tab)
        .ok()
        .and_then(|mut c| host_roster(&mut c))
    else {
        return Behind::Unread(UNREAD_SOCKET);
    };
    behind_after_lanes(opts, tab, &tabs, since, &|agent| {
        lane_noted(opts, &tabs, agent, since)
    })
}

/// What the lane of `agent`, past Claude Code's, noted of tab
/// `opts.only_sid` when its agent leads it. `None`: the tab is not that
/// lane's. Each lane by name.
fn lane_noted(opts: &Opts, tabs: &[LiveTab], agent: upgrade::Agent, _since: u64) -> Option<Behind> {
    match agent {
        // No Codex is moved live (the Codex screen lane's retirement,
        // 2026-10-16): nothing of one is noted behind.
        upgrade::Agent::Codex => None,
        // Never among the others: its half is read last ([`behind_after_lanes`]).
        upgrade::Agent::Claude => None,
        // Muse Code: a tab its TUI leads is its lane's, whose quiet flow
        // mints no ladder state, so nothing is noted (never Claude Code's
        // half, which would read the tab's foreground as one of its own).
        upgrade::Agent::Muse => muse::live::leads(opts, tabs).then_some(Behind::Nothing),
        // agy: the same (its quiet flow mints no ladder state either).
        upgrade::Agent::Agy => agy::live::leads(opts, tabs).then_some(Behind::Nothing),
        // Gemini CLI: no upgrade lane, so no build it is behind is noted.
        upgrade::Agent::Gemini => None,
    }
}

/// [`note_behind`] of `tab` past its roster read: the lanes past Claude
/// Code's first, each by name in their fixed order ([`upgrade::Agent::others`],
/// `lane_noted`) — a tab one of them leads is that lane's alone — and only
/// then Claude Code's half, its session records read. A separate seam, as
/// [`due_after_lanes`] is, so the Claude roster is provably skipped on every
/// other lane's answer.
fn behind_after_lanes(
    opts: &Opts,
    tab: &str,
    tabs: &[LiveTab],
    since: u64,
    lane_noted: &dyn Fn(upgrade::Agent) -> Option<Behind>,
) -> Behind {
    for &agent in upgrade::Agent::others() {
        if let Some(noted) = lane_noted(agent) {
            return noted;
        }
    }
    let homes = Homes::read(
        opts,
        Some(tabs),
        atpkg::caller_shell::process_args,
        aterm_sysprobe::process_cwd,
    );
    let Some(files) = homes.roster(claude_records(&opts.home)) else {
        return Behind::Unread(UNREAD_FILES);
    };
    let sf = match tab_agent(
        tab,
        &files,
        tabs,
        |_| true,
        atpkg::caller_shell::process_args,
        process_group,
        ids,
    ) {
        TabAgent::Claimed(sf) => sf,
        TabAgent::Nothing => return Behind::Nothing,
        TabAgent::Unread => return Behind::Unread(UNREAD_PROCESS),
        TabAgent::NoRecord if homes.unnamed(tab) => return Behind::Unread(UNREAD_PROCESS),
        TabAgent::NoRecord => {
            return match no_record(tabs, tab, started_s, alive, now_s()) {
                Due::Unread(word) => Behind::Unread(word),
                Due::Yes | Due::No => Behind::Nothing,
            };
        }
    };
    let native = exe_of(sf.pid).is_some_and(|exe| exe.starts_with(native_root(&opts.home)));
    if mint_behind(opts, sf, tab, &Targets::read(&opts.home), native, since) {
        Behind::Noted
    } else {
        Behind::Nothing
    }
}

/// What [`note_behind`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Behind {
    /// The session's state was minted behind, its age from the `since`
    /// asked with.
    Noted,
    /// Nothing to note: no agent behind in the tab, or its state already
    /// says so (or is in flight).
    Nothing,
    /// Another actor holds the state's lock (a step under way): asked again
    /// once it lets go.
    Busy,
    /// A read failed ([`Due::Unread`]'s words): nothing noted, and nothing
    /// decided — asked again.
    Unread(&'static str),
}

/// Whether the state standing for a session leaves it to be noted behind anew
/// on build `to` ([`note_behind`]): finished (done, its carry-on's model no
/// longer owed, or failed) with another build. A state in flight, pending or
/// announced is the step's, as is one finished with `to` itself (done, or a
/// stopped round the step re-arms after [`upgrade::RETRY_S`]).
pub(super) fn behind_anew(st: &St, to: &Version) -> bool {
    matches!(st.phase, Phase::Done | Phase::Failed(_))
        && !st.confirming()
        && Version::parse(&st.to).as_ref() != Some(to)
}

/// [`note_behind`]'s record for `sf` in `tab` among `targets` (`native`: runs
/// the native install): minted where a build it may move to is newer and no
/// state stands except one [`behind_anew`], as [`St::for_target`] mints it,
/// behind since `since` (never later than now).
fn mint_behind(
    opts: &Opts,
    sf: &SessionFile,
    tab: &str,
    targets: &Targets,
    native: bool,
    since: u64,
) -> bool {
    let Some(running) = Version::parse(&sf.version) else {
        return false;
    };
    let Some(target) = upgrade::choose_target(&running, &targets.for_session(native)) else {
        return false;
    };
    let prior = load(opts, &sf.session_id);
    if let Some(st) = prior
        .as_ref()
        .filter(|st| !behind_anew(st, &target.version))
    {
        return moved_tabs(st, tab) && retab(opts, sf, tab, prior, &running, &target);
    }
    let who = squash(&sf.proc_start);
    let who = Proc {
        start: &who,
        started_s: models::parse_lstart(&sf.proc_start).unwrap_or(0),
    };
    let mut st = St::for_target(prior, &running, &target, None, who, now_s());
    st.pending_since = st.pending_since.min(since);
    st.tab = tab.to_string();
    st.keep_home(&Records::of_file(&opts.home, sf), &opts.home);
    save(opts, &sf.session_id, &st);
    true
}

/// Whether record `st` of a conversation just proven held in `tab` waits in
/// another tab while nothing but the tab ties it there: pending, or a stopped
/// round resting. Never a notice out (the tab is its fence,
/// [`St::notice_belongs_to`]; a visit reopens a notice whose process is gone,
/// [`St::notice_gone`]), nor a restart under way or finished.
fn moved_tabs(st: &St, tab: &str) -> bool {
    st.tab != tab && matches!(st.phase, Phase::Pending | Phase::Failed(_))
}

/// A conversation resumed in another tab: its record `prior` still names its
/// first tab, and the watch, the window's view and `--status` find records by
/// tab. Once it is proven held in `tab`, the record is made what that tab's
/// first visit would make it ([`St::for_target`], ledgered once as
/// `retargeted:<from>-><to>` where the build changes), keeping its round and
/// time behind. Nothing is typed. Returns `true` (noted).
fn retab(
    opts: &Opts,
    sf: &SessionFile,
    tab: &str,
    prior: Option<St>,
    running: &Version,
    target: &Candidate,
) -> bool {
    let Some(old) = prior.as_ref().map(|p| p.to.clone()) else {
        return false;
    };
    let who = squash(&sf.proc_start);
    let who = Proc {
        start: &who,
        started_s: models::parse_lstart(&sf.proc_start).unwrap_or(0),
    };
    let now = now_s();
    let (mut st, how) = St::retarget(prior, running, target, None, who, now);
    if matches!(st.phase, Phase::Pending | Phase::Failed(_)) {
        tab.clone_into(&mut st.tab);
    }
    st.keep_home(&Records::of_file(&opts.home, sf), &opts.home);
    save(opts, &sf.session_id, &st);
    if how == Retarget::Rested {
        let r = Report {
            pid: sf.pid,
            tab: tab.to_string(),
            session: sf.session_id.clone(),
            from: running.to_string(),
            to: format!("{}({})", target.version, target.source.as_str()),
            step: String::new(),
        };
        rested_said(opts, &r, &st, now);
    }
    if Version::parse(&old).as_ref() != Some(&target.version) {
        let r = Report {
            pid: sf.pid,
            tab: tab.to_string(),
            session: sf.session_id.clone(),
            from: running.to_string(),
            to: format!("{}({})", target.version, target.source.as_str()),
            step: format!("retargeted:{old}->{}", target.version),
        };
        ledger(
            opts,
            &r,
            &format!(
                "upgrade target changed: {old} -> {}, found in another tab; {}",
                target.version,
                retarget_how(how, &st)
            ),
        );
    }
    true
}

/// The stop a new process of the conversation keeps ([`St::keep_stop`]), saved
/// and said once on the ledger (`kept-stopped:<why>`): the build that failed to
/// start for the conversation, and when it is tried again. A dry run writes and
/// says nothing.
fn rested_said(opts: &Opts, r: &Report, st: &St, now: u64) {
    if opts.dry_run {
        return;
    }
    let Phase::Failed(why) = &st.phase else {
        return;
    };
    save(opts, &r.session, st);
    // The rest counts from [`St::failed_at`], which a repeated stop sets later.
    let rests = st
        .failed_at
        .saturating_add(upgrade::RETRY_S)
        .saturating_sub(now);
    ledger(
        opts,
        &said(r.clone(), format!("kept-stopped:{why}")),
        &format!(
            "a new process holds the conversation (resumed by hand after the stop), running {}; \
             {} failed to start for this conversation, so it is not tried again for it until the \
             stop's rest ends in {} (a newer build is tried at once)",
            st.from,
            st.to,
            upgrade::span(rests)
        ),
    );
}

/// What the window's host does after one [`step`] on a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum After {
    /// Park again at the session's next idle point: the step acted and the
    /// next waits on the agent's answer at a turn end.
    NextIdle,
    /// Look again after this pause: the step waited (busy, held, a person,
    /// unsettled, no READY yet, another actor on the lock) or held the session
    /// back. Each consecutive wait pauses longer ([`LATER`]).
    Later(std::time::Duration),
    /// Nothing more for this build: done, current, a round's own stop just
    /// said (`failed:…`, `refused:…`; the next look finds it resting), or
    /// skipped by the owner. A resting round (`wait:failed`) is not finished:
    /// it is looked at again ([`upgrade::RETRY_S`]).
    Finished,
    /// The step cannot run at all — its state directory cannot hold the lock
    /// — and says so once; nothing more until the next activation notice.
    Broken,
}

/// The pauses before consecutive looks at a session whose step waited: the
/// first long enough for a session that just went idle to settle
/// ([`upgrade::QUIET_S`]), the last a ten-minute look for as long as it
/// waits. At run time its first rung is the settle this process runs
/// ([`first_look`]).
pub const LATER: [std::time::Duration; 4] = [
    std::time::Duration::from_secs(upgrade::QUIET_S),
    std::time::Duration::from_secs(60),
    std::time::Duration::from_secs(300),
    std::time::Duration::from_secs(600),
];

/// [`LATER`]'s first rung as this process runs it: the settle
/// ([`upgrade::quiet_s`], [`upgrade::QUIET_S`] in every shipped binary), so
/// the look after a settle's wait comes when the settle can have stood.
#[must_use]
pub fn first_look() -> std::time::Duration {
    std::time::Duration::from_secs(upgrade::quiet_s())
}

/// [`LATER`] with its first rung [`first_look`]: the ladder [`after`] climbs.
fn later_rungs() -> [std::time::Duration; 4] {
    let mut rungs = LATER;
    rungs[0] = first_look();
    rungs
}

/// The longest gap between two looks reading the same process's screen idle
/// for them to count in a row ([`St::idle_looks`]): the ladder's longest pause
/// ([`LATER`]) plus a settle's slack. Past it the run begins again: an idle
/// screen from hours before says nothing of Claude's status now.
pub const IDLE_RUN_GAP_S: u64 = LATER[LATER.len() - 1].as_secs() + upgrade::QUIET_S;

/// How soon a session whose step waited on a person's hold
/// ([`upgrade::person_hold`]) is looked at again, however long it has waited:
/// often enough to measure the hold's dwell ([`upgrade::hold_since`]), so a
/// READY it holds past [`upgrade::DRAIN_S`] is voided, not acted on when the
/// person lets go hours later.
pub const HOLD_LOOK: std::time::Duration = std::time::Duration::from_secs(60);

/// A look that runs a little late never begins a person's hold again: the gap
/// the drain's dwell tolerates is at least two looks.
const _: () = assert!(upgrade::HOLD_GAP_S >= 2 * HOLD_LOOK.as_secs());

/// [`After`] from one [`step`]'s word, `waits` the session's consecutive waits
/// before it. An attended wait's word alone reads as while it owns the point
/// (the first rung); the window's host, which counts its looks, asks
/// [`after_attended`] instead.
///
/// An untaught word is looked at again, never taken as the last word: every
/// last word is named ([`is_last_word`]), and anything else is
/// [`unknown_step`], looked at again at the ladder's longest pause (so it does
/// not poll), with a one-time warning from the window's host. Otherwise a new
/// step word would silently stop the host looking at the session.
#[must_use]
pub fn after(step: &str, waits: u32) -> After {
    known_after(step, waits).unwrap_or(After::Later(LATER[LATER.len() - 1]))
}

/// Whether [`after`] was never taught `step`: it reads [`After::Later`] as a
/// guess, and the window's host says so once (`harness_host`'s
/// `UpgradeRun::after`), so the word is added here rather than read as done.
#[must_use]
pub fn unknown_step(step: &str) -> bool {
    known_after(step, 0).is_none()
}

/// The step deadline of a record: the unix second by which the step that
/// moves `st` on should have run, from the record alone, or `None` for a
/// record owed nothing until a newer build or the owner's next word. Pure (no
/// clock, screen or process), so a timer can keep it where no point comes. One
/// exhaustive match with no wildcard arm, so a new phase or word does not
/// compile without a budget; wait words ([`St::wait`]) never decide one.
///
/// | phase | deadline |
/// |---|---|
/// | `pending` | [`St::progress_at`] + [`upgrade::REASK_S`]; refused notices ([`St::refused`]) are no progress, so they never push it back |
/// | `announced` | `at_s` + `REASK_S` + [`WATCH_SLACK`]: the re-ask's clock, held by a limit ([`St::hold_clock`]) |
/// | `failed` | `failed_at` + [`upgrade::RETRY_S`] + `WATCH_SLACK`: the re-arm (`failed_at` already carries a repeated stop's longer rest, [`St::fail`]) |
/// | `exiting`, `relaunched` | `at_s` + [`STALE_S`] + 1: when the owner's view reads it stuck ([`St::stuck`]) |
/// | `done` | `confirm_by` + `WATCH_SLACK` while the model confirm, or a quiet restart's look, is owed ([`St::confirming`]); else none |
///
/// The owner's word: a `--skip` of this target holds a round not yet
/// restarting (pending, announced, or stopped awaiting re-arm,
/// [`upgrade::rearm_held`]) for good; a `--defer` holds the same until its
/// second, the deadline being the later of that plus `WATCH_SLACK` and the
/// phase's own; a restart in flight is held by neither ([`Request::holds`]).
/// The word is the one on the record's own tab ([`St::request_for`]).
#[deny(clippy::wildcard_enum_match_arm)]
pub(super) fn due_by(st: &St) -> Option<u64> {
    let step = match &st.phase {
        Phase::Pending => Some(st.progress_at.saturating_add(upgrade::REASK_S)),
        Phase::Announced { at_s, .. } => Some(
            at_s.saturating_add(upgrade::REASK_S)
                .saturating_add(WATCH_SLACK),
        ),
        Phase::Failed(_) => Some(
            st.failed_at
                .saturating_add(upgrade::RETRY_S)
                .saturating_add(WATCH_SLACK),
        ),
        Phase::Exiting { at_s } | Phase::Relaunched { at_s } => {
            Some(at_s.saturating_add(STALE_S).saturating_add(1))
        }
        Phase::Done => st
            .confirming()
            .then(|| st.confirm_by.saturating_add(WATCH_SLACK)),
    };
    // Whether the word holds a round that has not begun restarting, by the
    // phase: pending, announced, and a stopped round's re-arm.
    let unbegun = match &st.phase {
        Phase::Pending | Phase::Announced { .. } | Phase::Failed(_) => true,
        Phase::Exiting { .. } | Phase::Relaunched { .. } | Phase::Done => false,
    };
    match st.request_for(&st.tab) {
        Request::None | Request::Now => step,
        Request::Skip(v) => {
            let this_target = Version::parse(&v).is_some_and(|v| Some(v) == Version::parse(&st.to));
            if this_target && unbegun { None } else { step }
        }
        Request::DeferUntil(t) => {
            if unbegun {
                step.map(|s| s.max(t.saturating_add(WATCH_SLACK)))
            } else {
                step
            }
        }
    }
}

/// When the host watches a record: the earlier of the step deadline
/// ([`due_by`]) and the move clock (behind its target for [`MOVE_BUDGET_S`],
/// [`St::behind_since`], kept across retargets and rounds, the one bound an
/// endlessly re-arming round cannot reset), but never sooner than
/// [`WATCH_GAP`] after the last watch ([`St::looked_at`]). `None` for a record
/// [`due_by`] owes nothing (finished, or held by `--skip`).
pub(super) fn watch_at(st: &St) -> Option<u64> {
    let step = due_by(st)?;
    let moved = st.behind_since().saturating_add(MOVE_BUDGET_S);
    Some(step.min(moved).max(st.looked_at.saturating_add(WATCH_GAP)))
}

/// The last words: nothing more for this build until the next activation
/// notice or owner's word ([`After::Finished`]). `current`, `done` and
/// `done:<how>`, `ended:<how>` (the relaunch lane: the agent ended by itself,
/// nothing resumed), `skip:<why>` (not this step's session), a dry run's
/// `would-<act>`, the owner's `wait:skipped`, and the restart's acts that end
/// the process this worker watches ([`moved`]): `terminated`, `relaunched`
/// and `exited`. The process that follows is a new session with its own
/// worker.
fn is_last_word(step: &str) -> bool {
    matches!(
        step.split_once(':').map_or(step, |(head, _)| head),
        "current" | "done" | "ended" | "skip" | "terminated" | "relaunched" | "exited"
    ) || step.starts_with("would-")
        || step == "wait:skipped"
}

/// [`after`] for a word it was taught; `None` for one it was not.
fn known_after(step: &str, waits: u32) -> Option<After> {
    let later = || ladder(waits);
    Some(match step.split_once(':').map_or(step, |(head, _)| head) {
        // `continued`: the resumed session's answer (the model it came back
        // on) is read at its turn's end. `adopted`: the relaunched agent is its loop's, and
        // the continuation is typed at the loop's next idle point.
        "announced" | "continued" | "adopted" => After::NextIdle,
        // `gave-up`, `drain-expired`: the upgrade stopped asking or voided a
        // READY and owes the agent its release, and a given-up upgrade still
        // honours a late READY, which comes at a turn's end: the next idle
        // point's, never the last word.
        "gave-up" | "drain-expired" => After::NextIdle,
        // `released`: the line is typed; a voided upgrade asks again later, and
        // one that gave up is looked at again, resting until its next round.
        // `rearmed`: a new round, whose first notice the next look types.
        "released" | "rearmed" => later(),
        // The last words ([`is_last_word`]), the owner's `--skip` among them:
        // nothing more until a newer build or the owner's next word (both wake
        // the host). A `--defer` runs out on its own, so it is looked at again.
        _ if is_last_word(step) => After::Finished,
        // A stopped round with nothing owed: `wait:failed` is said only when no
        // release is owed (that waits as `wait:release:<why>`,
        // [`release_is_next`]) and no READY can move it. It is the upgrade's
        // quiet word, which the derived model's `Look` is bound to
        // (`harness_upgrade_never_strands_model`). Not a last word: the round
        // rests `upgrade::RETRY_S` and re-arms, so it is looked at again.
        _ if step == "wait:failed" => later(),
        // A round's own stop, said as it stops (`failed:<why>`,
        // `refused:<what>`): the next look finds it resting, as above.
        "failed" | "refused" => later(),
        "busy" if matches!(step, "busy:state-unwritable" | "busy:lock-unopenable") => After::Broken,
        // A person's hold: its dwell is sampled ([`HOLD_LOOK`]).
        _ if matches!(step, "wait:box" | "wait:draft") => After::Later(HOLD_LOOK),
        // A person at the tab: looked at again at the first rung while the
        // point is the upgrade's ([`after_attended`], with the host's look
        // count), so the look finding the grace lapsed lands close enough to
        // take the act due at this point ([`owns_turn_ends`]) before the
        // loop's own continuation does.
        _ if attended(step) => After::Later(first_look()),
        // `left-typed:<what>` (the text recorded and standing) is looked at
        // again like any wait.
        "wait" | "busy" | "held-back" | "left-typed" => later(),
        // `reopened:notice-process-gone`: no step returns it (the visit ledgers
        // it and goes on); taught as a pending round's word.
        "reopened" => later(),
        _ => return None,
    })
}

/// The re-look ladder ([`LATER`]) at its `waits`-th rung (the last once past it).
fn ladder(waits: u32) -> After {
    After::Later(
        crate::supervise::ladder::Ladder(&later_rungs())
            .step(usize::try_from(waits).unwrap_or(usize::MAX)),
    )
}

/// Whether one [`step`]'s word is a person at the tab holding the act due at
/// this point: `held-back:attended` (once per round), then `wait:attended`.
#[must_use]
pub fn attended(step: &str) -> bool {
    matches!(step, "held-back:attended" | "wait:attended")
}

/// [`After`] an attended wait ([`attended`]): `looks` its attended looks in a
/// row at this point, this one included ([`attended_owned`]), `same` its waits
/// on the same word before it. At the first rung while the next look still
/// owns the point, so the look that finds the grace lapsed takes the act.
/// Past the backstop the point is the loop's again, and it returns to the
/// ladder rather than a costly look (status read, process scan, state save,
/// journal row) every twenty seconds.
#[must_use]
pub fn after_attended(looks: u32, same: u32, grace_s: u32) -> After {
    if attended_owned(looks, grace_s) {
        After::Later(first_look())
    } else {
        ladder(same)
    }
}

/// Whether one [`step`]'s word says it typed a turn into the agent
/// ([`crate::supervise::HostStep::typed`]): a notice (`announced:<n>`), a
/// carry-on (`continued`, both lanes; Claude Code's record then awaits the
/// model its answer names, while a Codex's is done as typed), a quiet
/// restart's one line that something did not come back
/// (`continued:restore`), or the release line (`released:<why>`). Its answer
/// is the harness's own turn; a short one is no short turn of the worker's.
#[must_use]
pub fn typed(step: &str) -> bool {
    matches!(
        step.split_once(':').map_or(step, |(head, _)| head),
        "announced" | "continued" | "released"
    )
}

/// Which turn one [`step`]'s word typed ([`typed`]), for the turn-end policy
/// ([`crate::supervise::HostStep::typed`]): the carry-on (`continued`) and the
/// release that takes a notice back (`released:<why>`; [`upgrade::release_prompt`]:
/// carry on as you would have without it) are the worker's work resumed,
/// whose answer of real work may be a done report; the notice
/// (`announced:<n>`) asks for a stopping point, and its answer never is; nor
/// is the quiet restart's one line (`continued:restore`;
/// [`upgrade::restore_prompt`]), which asks only that what did not come back
/// be restored and says nothing else is asked: its answer (`restored the
/// monitor; nothing else`) is that line's, never the task's end. The
/// carry-on that types the Cruise prompt in place of `keep going`
/// (`continued:check`, [`upgrade::checking`]) is that prompt: its answer is
/// read as the prompt's yield (the live lab's LIVE-RL-3). `None` where it
/// typed nothing.
#[must_use]
pub fn host_turn(step: &str) -> Option<crate::supervise::policy::turn_end::HostTurn> {
    use crate::supervise::policy::turn_end::HostTurn;
    typed(step).then(|| {
        if step == "continued:restore"
            || step.split_once(':').map_or(step, |(head, _)| head) == "announced"
        {
            HostTurn::Notice
        } else if step == "continued:check" {
            HostTurn::Check
        } else {
            HostTurn::CarryOn
        }
    })
}

/// Whether one [`step`]'s word says it moved the session
/// ([`crate::supervise::HostStep::moved`]): it typed a turn ([`typed`]) or
/// ended the agent (`terminated`, `relaunched`, `adopted`, `exited`,
/// `done:fresh`, and `done:unconfirmed` for a new process whose prompt never
/// came up). Every other word (waits, `current`, skips, last words such as
/// `done`, `done:model-unconfirmed`, `gave-up`, `failed:…`, `done:taskless`,
/// `done:finished`, `done:lost`, `done:no-continue`, `done:said-done`) typed
/// and ended nothing: its point still stands.
#[must_use]
pub fn moved(step: &str) -> bool {
    typed(step)
        || matches!(
            step.split_once(':').map_or(step, |(head, _)| head),
            "adopted" | "relaunched" | "terminated" | "exited"
        )
        || matches!(step, "done:fresh" | "done:unconfirmed")
}

/// Whether one [`step`]'s word says a NEW AGENT PROCESS came up where the
/// harness ended the old one: the relaunch line typed (`relaunched`), the
/// relaunched process found and taken (`adopted`, the hosted lanes' word for
/// a relaunch whose process registered), or a fresh start (`done:fresh`, and
/// `done:unconfirmed` for one whose prompt never came up). A part of
/// [`moved`]: a turn typed into the running agent ([`typed`]: a notice, a
/// carry-on, a release) and a stop with no start yet (`terminated`,
/// `exited`) brought no process up. The session log's relaunch lift reads it
/// (`harness_host::log_act`, session log Phase 6 step 6C-1): only such a
/// step can have replaced the agent run whose failure it lifts.
#[must_use]
pub fn started(step: &str) -> bool {
    matches!(
        step.split_once(':').map_or(step, |(head, _)| head),
        "adopted" | "relaunched"
    ) || matches!(step, "done:fresh" | "done:unconfirmed")
}

/// Holds the upgrade's clocks through a limit episode the window's loop saw:
/// tab `opts.only_sid`'s records have their re-ask and READY clocks start no
/// sooner than `until`, the last unix second the loop knew the session
/// limited ([`St::hold_clock`]). Called by the host as the episode opens and
/// closes (`IdleHost::limited`), since the upgrade's own look holds the clock
/// only when it finds the limit ([`upgrade::clock_held`]) and the host takes
/// no step during an episode. Files only, under the sweep lock; `false` while
/// another sweep holds it (the host retries before its next step), `true`
/// once applied or with nothing to apply.
#[must_use]
pub fn hold_clock(opts: &Opts, until: u64) -> bool {
    let Some(tab) = opts.only_sid.as_deref() else {
        return true;
    };
    let _held = match sweep_lock(opts) {
        Ok(held) => held,
        Err("another-sweep") => return false,
        Err(_) => return true,
    };
    let Ok(dir) = std::fs::read_dir(state_dir(opts)) else {
        return true;
    };
    for e in dir.flatten() {
        let path = e.path();
        if path.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let Some(session) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
            continue;
        };
        if let Some(mut st) = load(opts, &session).filter(|st| st.tab == tab)
            && st.hold_clock(until)
        {
            save(opts, &session, &st);
        }
    }
    true
}

/// How many consecutive looks the upgrade may wait for background work under
/// a READY agent before it stops owning the session's turn ends
/// ([`owns_turn_ends`]); past it the worker is continued. This bounds only
/// ownership; the wait itself is bounded by the reducer's re-ask
/// ([`upgrade::REASK_S`]) and give-up ([`upgrade::MAX_ASKS`],
/// [`upgrade::next_step`]).
pub const OWNED_BACKGROUND_LOOKS: u32 = 3;

/// How many quick looks in a row (the upgrade's own gate settling: the
/// screen quiet [`upgrade::QUIET_S`]) the upgrade may own the session's turn
/// ends for.
pub const OWNED_SETTLE_LOOKS: u32 = 2;

/// Whether the upgrade owns the session's turn ends after the step that said
/// `step` (`looks`: for an attended word, [`attended`], the attended looks in
/// a row at this point; for others the waits in a row). The window's host
/// answers its loop with this (`IdleHost::owns_turn_end`) so the turn-end
/// policy types nothing over a wind-down: an announcement out, and while READY
/// is given and the restart's gate only waits (its settle for
/// [`OWNED_SETTLE_LOOKS`] looks, a background drain for
/// [`OWNED_BACKGROUND_LOOKS`]), and while an owed release's own gate only
/// waits out the settle at its point ([`release_settles`], for
/// [`OWNED_SETTLE_LOOKS`] looks as well), and while Muse Code's quiet restart
/// waits on its session's log to catch up with its idle screen
/// ([`log_settles`], the same bound). An answer without READY
/// (`awaiting-ready`), a person's hold, a release held by anything else
/// (`wait:release:<why>`), and every last word own nothing. It lives beside
/// [`after`] because a step's word is this module's to interpret.
///
/// A person at the tab owns them while their grace (`grace_s`) runs, bounded
/// only as a backstop ([`attended_owned`]). That wait arises only where the
/// upgrade's act is due at this point and only the person holds it; the loop
/// holds off for the same grace, so ownership decides who takes the point when
/// it lapses.
#[must_use]
pub fn owns_turn_ends(step: &str, looks: u32, grace_s: u32) -> bool {
    if attended(step) {
        return attended_owned(looks, grace_s);
    }
    match step.split_once(':').map_or(step, |(head, _)| head) {
        "announced" => true,
        "wait" => match step {
            // `status-stale`: Claude's status catching up with its idle screen
            // ([`upgrade::gate_announce`]), a settle like `not-idle`.
            // `ready-unrecorded`: READY on screen, its transcript row still to
            // be written; the point is the restart's, and a continuation would
            // void the answer. `superseded`: the re-ask owed to a superseded
            // READY, due here once the turn settles. Both bounded as the settle
            // is.
            "wait:settling"
            | "wait:not-idle"
            | "wait:busy"
            | "wait:status-stale"
            | "wait:ready-unrecorded"
            | "wait:superseded" => looks < OWNED_SETTLE_LOOKS,
            // Muse Code's quiet restart waiting on its session's log to catch
            // up with its idle screen ([`log_settles`]): bounded as the settle is.
            _ if log_settles(step) => looks < OWNED_SETTLE_LOOKS,
            // An owed release whose gate waits the settle at this point.
            _ if release_settles(step) => looks < OWNED_SETTLE_LOOKS,
            // A session that answered READY waits out a shell under it.
            "wait:background" => looks < OWNED_BACKGROUND_LOOKS,
            // The notice was due at this very point and its own fence refused
            // it — a repaint, a keystroke's echo ([`refused`]).
            _ if refused(step) => looks < OWNED_REFUSED_LOOKS,
            _ => false,
        },
        _ => false,
    }
}

/// [`owns_turn_ends`] under the flow the step was taken in (round eight,
/// #25): under the QUIET flow (`quiet`, [`upgrade::Facts::quiet`], the
/// default) a wait on the agent's own work — `wait:background` — OWNS
/// NOTHING. That ownership exists for an
/// agent that answered READY and stopped while its background work drains:
/// the loop's `keep going` must not be typed over the answer. The quiet flow
/// asks for no READY and acts on none: its restart simply waits for the
/// work to end ([`upgrade::quiet_step`]), so every pending round with a shell
/// under its agent read `wait:background` at an idle point and held the
/// loop's continuation for [`OWNED_BACKGROUND_LOOKS`] looks (about six
/// minutes, [`LATER`]) for a restart that could not come before the work
/// ended. Every other word owns as [`owns_turn_ends`] says, in both flows:
/// the settle the quiet restart is due at, a person's hold, a notice.
#[must_use]
pub fn owns_turn_ends_as(step: &str, looks: u32, grace_s: u32, quiet: bool) -> bool {
    !(quiet && drains(step)) && owns_turn_ends(step, looks, grace_s)
}

/// [`bounded_wait`] under the flow the step was taken in: under the quiet flow
/// the waits on the agent's own work own nothing ([`owns_turn_ends_as`]), so
/// they spend none of the settle's looks.
#[must_use]
pub fn bounded_wait_as(step: &str, quiet: bool) -> bool {
    !(quiet && drains(step)) && bounded_wait(step)
}

/// The DRAIN'S word: a wait on the agent's own work under it
/// (`wait:background`).
fn drains(step: &str) -> bool {
    step == "wait:background"
}

/// Whether a wait counts toward the bound its word is owned for
/// ([`owns_turn_ends`]): the settle's (`settling`, `not-idle`, `busy`,
/// `status-stale`, `ready-unrecorded`, `superseded`, and Muse Code's log
/// catching up, [`log_settles`]) and the drain's (`background`), the waits of
/// a gate whose act is due at the point. Others count nothing: a person at the tab (counted at its
/// point, [`attended_owned`]), `awaiting-ready`, a resting round (`failed`), a
/// box, a draft, a limit; counting them would spend the settle's looks before
/// the READY came. An agent's turn never restarts the count, only the
/// upgrade's acts. A notice its own fence refused ([`refused`]) has its own
/// count, which the owner's word also restarts.
#[must_use]
pub fn bounded_wait(step: &str) -> bool {
    matches!(
        step,
        "wait:settling"
            | "wait:not-idle"
            | "wait:busy"
            | "wait:status-stale"
            | "wait:ready-unrecorded"
            | "wait:superseded"
            | "wait:background"
    ) || release_settles(step)
        || log_settles(step)
}

/// THE QUIET RESTART OF MUSE CODE OR AGY WAITING ON ITS AGENT'S OWN RECORD TO
/// CATCH UP WITH ITS IDLE SCREEN (`upgrade_muse_live`'s and
/// `upgrade_agy_live`'s gates): `wait:turn-open` — Muse's run's `terminal`
/// written a few hundred milliseconds after the reader reads the turn over
/// (measured at every turn end of the 2026-10-18 lab runs: the screen idle at
/// …805.139, the step at …805.327, the terminal at …805.639), agy's summary
/// row reading `RUNNING` as long after (the screen idle at …674.6, the row
/// `RUNNING` until …675.4) — and `wait:queued-work`, work's end told and the
/// turn the agent starts on it by itself still to come (Muse's queued inbox
/// item, agy's waking message). The settle Claude Code's restart waits out
/// at a fresh turn end, in their words: the restart is due at this very
/// point once the record is written, so the point is owned as the settle's
/// is, for [`OWNED_SETTLE_LOOKS`] looks counted with it ([`bounded_wait`]) —
/// owning nothing, a loop that continues at turn ends typed its continuation
/// at once at every one, and a continuously driven Muse was upgraded only
/// once its loop rested (the review of 2026-10-18). Every other wait of those
/// gates owns nothing: a turn running, open work, a child, a draft, a person,
/// an unreadable or unproved record. No step of Claude Code's says either
/// word.
#[must_use]
pub fn log_settles(step: &str) -> bool {
    matches!(step, "wait:turn-open" | "wait:queued-work")
}

/// AN OWED RELEASE WAITING OUT THE SETTLE AT ITS POINT
/// (`wait:release:settling`, `…:not-idle`, `…:busy`, `…:status-stale`): the
/// release is the upgrade's next act ([`release_is_next`]) and its gate
/// ([`upgrade::gate_release`], the notice's) only waits for the screen and
/// the status a fresh turn end left to go quiet, as the notice's and the
/// restart's settle do. Such a point is owned as theirs is, for
/// [`OWNED_SETTLE_LOOKS`] looks counted with them ([`bounded_wait`]), and
/// looked at again at the ladder's first rung ([`after`]): owning nothing,
/// a give-up's release gated at a fresh turn end (its usual case) left the
/// point to the loop, whose `keep going` was typed there and then voided
/// the release as direction taken up since (`release_void`, `directed`) —
/// a turn typed, and the agent never told the notice no longer applies
/// . Every other gate of a release (a box, a draft, a
/// person, a limit, a READY standing) owns nothing, as before.
#[must_use]
pub fn release_settles(step: &str) -> bool {
    step.strip_prefix("wait:release:")
        .is_some_and(|why| matches!(why, "settling" | "not-idle" | "busy" | "status-stale"))
}

/// Whether one [`step`]'s word is a notice its own fence refused
/// (`wait:announce-refused:<why>`): the gate said go and the host's `turn`
/// typed nothing, because the screen moved between the fence's read and the
/// paste (`changed`), a person typed through the park (`yield`), or it was
/// refused outright. Retried within the visit first ([`NOTICE_FENCE_TRIES`]);
/// then the point is owned for [`OWNED_REFUSED_LOOKS`] first-rung looks
/// ([`after_refused`]) and the refusals are counted for the owner
/// (`St::refused`).
#[must_use]
pub fn refused(step: &str) -> bool {
    step.starts_with("wait:announce-refused:")
}

/// How many refused looks since the upgrade's last act or owner's word a
/// refused notice ([`refused`]) may own the session's turn ends for
/// ([`owns_turn_ends`]): three, [`LATER`]'s first rung apart
/// ([`after_refused`]), each typing the notice up to [`NOTICE_FENCE_TRIES`]
/// times. Counted apart from the settle's waits ([`bounded_wait`]). An act or
/// an owner's word renews it; an agent's turn does not. Without it the loop's
/// `keep going` would take the idle point the notice needed.
pub const OWNED_REFUSED_LOOKS: u32 = 3;

/// [`After`] a refused notice ([`refused`]): `looks` its waits since the last
/// act (as [`owns_turn_ends`] read), `same` its waits on the same word.
/// Looked at again at the first rung while the point is still the upgrade's,
/// so the notice is retried within it; back on the ladder once it is the
/// loop's.
#[must_use]
pub fn after_refused(looks: u32, same: u32) -> After {
    if looks < OWNED_REFUSED_LOOKS {
        After::Later(first_look())
    } else {
        ladder(same)
    }
}

/// How many times one visit types a notice its fence refused as `changed` (a
/// repaint, a keystroke's echo) before waiting
/// (`wait:announce-refused:changed`). Each try reads the composer afresh and is
/// fenced on that read, so a draft or box stops it at once, and a person
/// typing through the park (`yield`) is never typed at again. A later try
/// types only where that read still meets the look's gates ([`retry_open`]:
/// no person's hand since the look, no running turn, the reader's idle point).
pub const NOTICE_FENCE_TRIES: usize = 3;

/// Whether an attended wait ([`owns_turn_ends`]) still owns the session's turn
/// ends after `looks` attended looks in a row at this point: while they span
/// ([`LATER`]'s first rung apart, [`after_attended`]) no more than the person's
/// grace (`grace_s`) plus [`ATTENDED_OWNED_S`]. Each look re-reads the person's
/// stamp, so the look after their grace lapses takes the act; the bound is a
/// backstop for a stamp that fails closed ([`upgrade::attended_by`]) and would
/// read attended for ever. The host counts the looks at one point: any other
/// step or a turn in between (the person's, the agent's READY) starts them
/// over.
#[must_use]
pub fn attended_owned(looks: u32, grace_s: u32) -> bool {
    u64::from(looks).saturating_mul(first_look().as_secs())
        <= u64::from(grace_s).saturating_add(ATTENDED_OWNED_S)
}

/// How long past the person's grace an attended wait may keep owning the
/// session's turn ends ([`attended_owned`]), holding the agent's act for a
/// person reading, scrolling or clicking.
pub const ATTENDED_OWNED_S: u64 = 30 * 60;

/// Whether a step taken at a break of the agent's background work
/// (`IdleHost::at_background`) gives back the turn ends a notice claimed
/// there. A session whose screen stays busy never reaches the idle point that
/// would recompute the claim ([`owns_turn_ends`]), so these give it back at
/// once: the owner's `--skip`/`--defer`, a person at the tab, aterm's hold,
/// the limit, a full queue of untaken notices, a give-up, a void, a release
/// typed or owed, and every stop. A wait on the agent's own work keeps the
/// claim, since the agent was asked to finish up.
#[must_use]
pub fn released_at_break(step: &str) -> bool {
    matches!(
        step,
        "gave-up"
            | "wait:skipped"
            | "wait:deferred"
            | "wait:attended"
            | "wait:held"
            | "wait:limited"
            | "wait:queued"
            | "wait:failed"
            | "wait:done"
    ) || [
        "failed",
        "refused",
        "held-back",
        "drain-expired",
        "released",
        "wait:release",
    ]
    .iter()
    .any(|head| step.starts_with(head))
}

/// A window supplies its own live PTY roster; an unscoped CLI keeps its
/// report-only wait when macOS omits another process's environment.
fn sweep_with_roster(opts: &Opts, live_tabs: Option<&[LiveTab]>) -> Vec<Report> {
    let _held = match sweep_lock(opts) {
        Ok(lock) => lock,
        Err(why) => {
            return vec![Report {
                pid: 0,
                tab: "-".to_string(),
                session: "-".to_string(),
                from: "-".to_string(),
                to: "-".to_string(),
                step: format!("busy:{why}"),
            }];
        }
    };
    // Files only, plus whatever else this sweep finds first; never at a break
    // of the agent's background work, where only a line that ends nothing is
    // typed (a notice, or an owed release).
    let mut reports = if opts.background {
        Vec::new()
    } else {
        confirmations(opts)
    };
    // Drive groups share a store across instances. This roster and one
    // registry cannot exclude live tabs in another namespace or a failed
    // discovery publication, so automatic sweeps leave those groups alone.
    // The Muse Code and agy lanes' per-tab records of tabs that are gone go
    // here, under the lock, as codex-alab's go in its pass ([`muse::prune`],
    // [`agy::prune`]); each one's pass is its live upgrade's quiet step
    // (below), and its crash relaunch in flight its window's host's. Gemini
    // CLI's lane moves nothing, and its records of tabs that are gone go here
    // ([`gemini::prune`]).
    muse::sweep_prune(opts, live_tabs);
    agy::sweep_prune(opts, live_tabs);
    gemini::sweep_prune(opts, live_tabs);
    reports.extend(lanes(opts, live_tabs, &|agent| match agent {
        // Codex restarts in flight, a codex-alab's crash relaunch
        // ([`codex_alab_drive::sweep`]); no Codex is moved live. No Claude
        // session file needed.
        upgrade::Agent::Codex => codex_alab_drive::sweep(opts, live_tabs),
        upgrade::Agent::Claude => claude_pass(opts, live_tabs),
        // Muse Code's live upgrade: the quiet flow's step for the tab its TUI
        // leads, by the window's own roster ([`muse::live::upgrade_pass`]; a
        // hand-run sweep moves no Muse Code).
        upgrade::Agent::Muse => muse::live::upgrade_pass(opts, live_tabs),
        // agy's live upgrade: its quiet flow's step, the same way
        // ([`agy::live::upgrade_pass`]; a hand-run sweep moves no agy).
        upgrade::Agent::Agy => agy::live::upgrade_pass(opts, live_tabs),
        // Gemini CLI: relaunch on exit only, no upgrade lane — never swept
        // ([`lane_on`]); its relaunch records are pruned above.
        upgrade::Agent::Gemini => Vec::new(),
    }));
    reports
}

/// The lanes' share of one sweep, under its lock: every lane past Claude
/// Code's first, in their fixed order ([`upgrade::Agent::others`]: the Codex
/// pass, its restarts in flight), then the Claude lane, each by `pass` — and
/// none for an agent whose own switch is off ([`Opts::agents_off`]), whose
/// sessions the pass leaves as they are. Claude Code's reports come first,
/// then each other lane's in that order.
fn lanes(
    opts: &Opts,
    live_tabs: Option<&[LiveTab]>,
    pass: &dyn Fn(upgrade::Agent) -> Vec<Report>,
) -> Vec<Report> {
    let swept = |agent: upgrade::Agent| {
        if lane_on(opts, agent) {
            pass(agent)
        } else {
            Vec::new()
        }
    };
    let others: Vec<Report> = upgrade::Agent::others()
        .iter()
        .flat_map(|&agent| swept(agent))
        .collect();
    // A tab another lane's pass reports on (a Codex restart in flight) belongs to
    // that lane alone: the Claude lane's machine-wide waits (Claude's records
    // unreadable) are not that tab's.
    let others_tab = opts
        .only_sid
        .as_ref()
        .is_some_and(|sid| live_tabs.is_some() && others.iter().any(|r| r.tab == *sid));
    let mut reports = Vec::new();
    if !others_tab {
        reports.extend(swept(upgrade::Agent::Claude));
    }
    reports.extend(others);
    reports
}

/// Whether a pass with `opts` takes `agent`'s lane: that agent has one the
/// sweep carries ([`super::lanes::lanes`]: its live upgrade, or its relaunch
/// on exit where the sweep carries that relaunch's restart in flight on,
/// [`super::lanes::sweep_carries_relaunch`] — a codex-alab's crash relaunch;
/// not a Muse Code's, an agy's nor a Gemini CLI's, whose relaunch in flight
/// its window's host carries) and its own switch is
/// not off ([`Opts::agents_off`]). The one answer [`lanes`], [`due`] and
/// [`in_flight`] give, so the window's host is never told a step is due that
/// its step then leaves alone.
fn lane_on(opts: &Opts, agent: upgrade::Agent) -> bool {
    let written = super::lanes::lanes(agent.program());
    (written.upgrade || (written.relaunch && super::lanes::sweep_carries_relaunch(agent.program())))
        && !opts.agents_off.contains(&agent.program())
}

/// A Codex restart in flight carried on from where it stopped (the relaunch
/// primitive's continuation step for a Codex record): a proven self-claimed
/// codex-alab's crash relaunch by its own path, and a record the retired
/// vendor Codex lane left closed ([`codex_alab_drive::carry_in_flight`]).
pub(super) fn codex_carry_in_flight(opts: &Opts, r: Report, st: St, session: &str) -> Report {
    codex_alab_drive::carry_in_flight(opts, r, st, session)
}

/// A codex-alab's [`super::relaunch::Snapshot`], read while it runs
/// ([`codex_alab_drive::snapshot`]): an ALab build's
/// ([`super::codex_alab::alab_bytes`]).
pub(super) fn codex_alab_snapshot(
    opts: &Opts,
    tab: &str,
    pid: u32,
    start: String,
    shell: u32,
    args: &atpkg::caller_shell::ProcArgs,
) -> Result<super::relaunch::Snapshot, &'static str> {
    codex_alab_drive::snapshot(opts, tab, pid, start, shell, args, &Live)
}

/// THE CRASH RULE'S RELAUNCH of a proven self-claimed codex-alab (the
/// self-claim contract, §4): `snap` the agent that crashed, `resume` the line
/// aterm's own copy of its record validated while it ran, `asked` the explicit
/// restart's cause where its stamp named the exit. The Codex lane relaunches
/// on that line ([`super::self_claimed`]), never on its own plan.
#[must_use]
pub fn self_claimed_after_exit(
    opts: &Opts,
    snap: &super::relaunch::Snapshot,
    resume: &super::self_claimed::Resume,
    asked: Option<super::restart_stamp::Cause>,
) -> Report {
    let cause = match asked {
        Some(super::restart_stamp::Cause::Stall) => super::relaunch::ExitCause::Stall,
        Some(super::restart_stamp::Cause::Asked) => super::relaunch::ExitCause::Asked,
        None => super::relaunch::ExitCause::Exit,
    };
    self_claimed_relaunch(opts, snap, resume, cause)
}

/// [`self_claimed_after_exit`] on its exit's cause as the relaunch reads it
/// (`relaunch::ExitCause`): the one relaunch of a proven self-claimed
/// codex-alab, whoever owes it — its worker's crash rule, a relaunch carried
/// to a seamless update's successor, or a restore after aterm's own end
/// (`relaunch::after_host_ended`, its snapshot carrying the validated line,
/// [`super::relaunch::CodexRun::self_claimed`]).
pub(super) fn self_claimed_relaunch(
    opts: &Opts,
    snap: &super::relaunch::Snapshot,
    resume: &super::self_claimed::Resume,
    cause: super::relaunch::ExitCause,
) -> Report {
    codex_alab_drive::self_claimed_after_exit(opts, snap, resume, cause, &Live)
}

/// A Muse Code TUI's [`super::relaunch::Snapshot`], read while it runs
/// ([`muse::snapshot`]).
pub(super) fn muse_snapshot(
    opts: &Opts,
    tab: &str,
    pid: u32,
    start: String,
    shell: u32,
    args: &atpkg::caller_shell::ProcArgs,
) -> Result<super::relaunch::Snapshot, &'static str> {
    muse::snapshot(opts, tab, pid, start, shell, args, &Live)
}

/// A Muse Code's snapshot followed between idle points ([`muse::follow`]):
/// the session `/new` or an in-TUI `/resume` moved it to.
pub(super) fn muse_follow(snap: &mut super::relaunch::Snapshot) -> bool {
    muse::follow(snap)
}

/// One look at a Muse Code that left its tab ([`muse::look_at_exit`]).
pub(super) fn muse_look_at_exit(
    opts: &Opts,
    snap: &super::relaunch::Snapshot,
) -> super::relaunch::ExitLook {
    muse::look_at_exit(opts, snap, &Live)
}

/// A Muse Code relaunch on exit ([`muse::after_exit`]).
pub(super) fn muse_after_exit(
    opts: &Opts,
    snap: &super::relaunch::Snapshot,
    left: &super::relaunch::ExitRecord,
    upgrade: bool,
    cause: super::relaunch::ExitCause,
) -> Report {
    muse::after_exit(opts, snap, left, upgrade, cause, &Live)
}

/// An agy TUI's [`super::relaunch::Snapshot`], read while it runs
/// ([`agy::snapshot`]).
pub(super) fn agy_snapshot(
    opts: &Opts,
    tab: &str,
    pid: u32,
    start: String,
    shell: u32,
    args: &atpkg::caller_shell::ProcArgs,
) -> Result<super::relaunch::Snapshot, &'static str> {
    agy::snapshot(opts, tab, pid, start, shell, args, &Live)
}

/// One look at an agy that left its tab ([`agy::look_at_exit`]).
pub(super) fn agy_look_at_exit(
    opts: &Opts,
    snap: &super::relaunch::Snapshot,
) -> super::relaunch::ExitLook {
    agy::look_at_exit(opts, snap, &Live)
}

/// An agy relaunch on exit ([`agy::after_exit`]).
pub(super) fn agy_after_exit(
    opts: &Opts,
    snap: &super::relaunch::Snapshot,
    left: &super::relaunch::ExitRecord,
    upgrade: bool,
    cause: super::relaunch::ExitCause,
) -> Report {
    agy::after_exit(opts, snap, left, upgrade, cause, &Live)
}

/// A Gemini CLI TUI's [`super::relaunch::Snapshot`], read while it runs
/// ([`gemini::snapshot`]).
pub(super) fn gemini_snapshot(
    opts: &Opts,
    tab: &str,
    pid: u32,
    start: String,
    shell: u32,
    args: &atpkg::caller_shell::ProcArgs,
) -> Result<super::relaunch::Snapshot, &'static str> {
    gemini::snapshot(opts, tab, pid, start, shell, args, &Live)
}

/// One look at a Gemini CLI that left its tab ([`gemini::look_at_exit`]).
pub(super) fn gemini_look_at_exit(
    opts: &Opts,
    snap: &super::relaunch::Snapshot,
) -> super::relaunch::ExitLook {
    gemini::look_at_exit(opts, snap, &Live)
}

/// A Gemini CLI relaunch on exit ([`gemini::after_exit`]).
pub(super) fn gemini_after_exit(
    opts: &Opts,
    snap: &super::relaunch::Snapshot,
    left: &super::relaunch::ExitRecord,
    upgrade: bool,
    cause: super::relaunch::ExitCause,
) -> Report {
    gemini::after_exit(opts, snap, left, upgrade, cause, &Live)
}

/// THE LINE A PERSON IS TOLD for `snap`'s Gemini CLI ([`gemini::told`]):
/// Gemini CLI's own `gemini --resume <uuid>` in its project, only where the
/// read-only pre-check, read now, says that resume loads every turn.
#[must_use]
pub fn gemini_told_line(snap: &super::relaunch::Snapshot) -> Option<String> {
    gemini::told(snap)
}

/// [`gemini_told_line`] of `session` in the project `root` whose chats
/// directory is `chats`, behind the homes `env` (a restored leaf's
/// [`gemini::told_at`]).
#[must_use]
pub fn gemini_told(
    session: &str,
    chats: Option<&Path>,
    env: &[(String, String)],
    root: &str,
) -> Option<String> {
    gemini::told_at(session, chats, env, root)
}

/// Whether the explicit restart of `snap`'s agy would be carried
/// ([`agy::carries_restart_onto`]), onto the managed twin a store build's
/// relaunch types ([`managed_agent_twin`], read only for one), behind the
/// heal its line takes for a user whose home is `user_home`.
pub(super) fn agy_carries_restart(user_home: &Path, snap: &super::relaunch::Snapshot) -> bool {
    let twin = super::upgrade_agy::store_build(&snap.program)
        .then(|| managed_agent_twin(super::upgrade_agy::PROGRAM))
        .flatten();
    agy::carries_restart_onto(snap, user_home, twin)
}

/// The Claude lane's share of one sweep: every live Claude Code session
/// advanced one step, then its restarts left in flight carried on.
fn claude_pass(opts: &Opts, live_tabs: Option<&[LiveTab]>) -> Vec<Report> {
    if live_tabs.is_some_and(<[LiveTab]>::is_empty) {
        return Vec::new();
    }
    // Every Claude home an agent in the tabs runs under, the user's first: a
    // user's home that keeps no records at all (no `sessions/`: every agent
    // runs under another home) holds none, as [`due`] reads it, and never
    // waits the pass for good.
    let homes = Homes::read(
        opts,
        live_tabs,
        atpkg::caller_shell::process_args,
        aterm_sysprobe::process_cwd,
    );
    let Some(files) = homes.roster(claude_records(&opts.home)) else {
        return vec![Report {
            pid: 0,
            tab: "-".to_string(),
            session: "-".to_string(),
            from: "-".to_string(),
            to: "-".to_string(),
            step: format!("wait:{UNREAD_FILES}"),
        }];
    };
    // Bookkeeping under the lock, never at a break of the agent's background
    // work, where only a line that ends nothing is typed.
    if !opts.background {
        prune_states(opts, &files, now_s());
    }
    let mut reports = Vec::new();
    reports.extend(if let Some(tabs) = live_tabs {
        // Read installed versions only after a cheap PTY-group match. A window
        // with no Claude job does not execute a managed twin just to ask its
        // version; a matched but current job stops before `ps` and argv reads.
        let targets = std::cell::OnceCell::new();
        let mctx = std::cell::OnceCell::new();
        let candidates = host_candidates(
            &files,
            tabs,
            |sf, tab| {
                let tg = targets.get_or_init(|| Targets::read(&opts.home));
                opts.only_sid.as_deref().is_none_or(|only| only == tab)
                    && (needs_upgrade_inspection(sf, tg, load(opts, &sf.session_id).as_ref())
                        || model_wants_a_look(
                            opts,
                            sf,
                            mctx.get_or_init(|| ModelCtx::read(opts, tg)),
                        ))
            },
            atpkg::caller_shell::process_args,
            process_group,
            ids,
        );
        // A process not read whole is the step's wait, said: the next look
        // reads it again.
        let (mut claimed, mut waits) = (Vec::new(), Vec::new());
        for candidate in candidates {
            match candidate {
                Ok(c) => claimed.push(c),
                Err(wait) => waits.push(wait),
            }
        }
        if !claimed.is_empty() {
            let t = table();
            let targets = targets.get_or_init(|| Targets::read(&opts.home));
            let mctx = mctx.get_or_init(|| ModelCtx::read(opts, targets));
            waits.extend(claimed.into_iter().map(|(sf, args, claim)| {
                visit_models(
                    opts,
                    sf,
                    Some(&files),
                    &t,
                    targets,
                    &Live,
                    Some(&args),
                    Some(&claim),
                    mctx,
                )
            }));
        }
        waits
    } else if files.is_empty() {
        Vec::new()
    } else {
        let t = table();
        let targets = Targets::read(&opts.home);
        let mctx = ModelCtx::read(opts, &targets);
        files
            .iter()
            .map(|sf| {
                visit_models(
                    opts,
                    sf,
                    Some(&files),
                    &t,
                    &targets,
                    &Live,
                    None,
                    None,
                    &mctx,
                )
            })
            .collect()
    });
    // A tab whose agent's home could not be named, and whose agent no record
    // read here names: its look waits, never "current".
    for sid in &homes.unnamed {
        if !reports.iter().any(|r| r.tab == *sid) {
            reports.push(Report {
                pid: 0,
                tab: sid.clone(),
                session: "-".to_string(),
                from: "-".to_string(),
                to: "-".to_string(),
                step: format!("wait:{UNREAD_PROCESS}"),
            });
        }
    }
    if !opts.background {
        reports.extend(orphans(opts, &files, live_tabs));
    }
    reports
}

/// Takes `<state>/upgrade/sweep.lock` without waiting. `Ok(None)` for a dry
/// run, which acts on nothing.
///
/// One try decides, because the guard is released by `LOCK_UN`
/// ([`SweepLock`]), like atpkg's store lock (`atpkg::lock::Flock`). An
/// `flock(2)` lives on the open file description, and a child mid-spawn on any
/// thread holds a copy of every descriptor until its exec, so a lock released
/// only by close would still read held for that long. `LOCK_UN` strips the
/// lock from the description itself, every inherited copy included, so a lock
/// that reads held is really a sweep's.
pub(super) fn sweep_lock(opts: &Opts) -> Result<Option<SweepLock>, &'static str> {
    if opts.dry_run {
        return Ok(None);
    }
    let dir = state_dir(opts);
    std::fs::create_dir_all(&dir).map_err(|_| "state-unwritable")?;
    let lock: std::fs::File = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("sweep.lock"))
        .map_err(|_| "lock-unopenable")?;
    #[expect(
        clippy::disallowed_methods,
        reason = "the take lands in a SweepLock, whose Drop releases by LOCK_UN"
    )]
    let taken = lock.try_lock();
    match taken {
        Ok(()) => Ok(Some(SweepLock(lock))),
        Err(_) => Err("another-sweep"),
    }
}

/// A held [`sweep_lock`], released by `LOCK_UN` on drop rather than by close,
/// so the next sweep finds it free at once whatever another thread is
/// spawning (`a_second_sweeper_does_nothing_while_the_first_holds_the_lock`).
pub(super) struct SweepLock(std::fs::File);

impl Drop for SweepLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Whether a restart of the tab `opts.only_sid` still owes the model its
/// resumed session came back on ([`St::confirming`]).
fn confirming(opts: &Opts) -> bool {
    std::fs::read_dir(state_dir(opts)).is_ok_and(|dir| {
        dir.flatten().any(|e| {
            let path = e.path();
            path.extension().is_some_and(|x| x == "json")
                && std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|text| St::from_json(&text))
                    .is_some_and(|st| {
                        st.confirming() && opts.only_sid.as_ref().is_none_or(|s| *s == st.tab)
                    })
        })
    })
}

/// The conversations an earlier sweep left mid-restart (agent signalled, new
/// process not yet relaunched or carried on) that a sweep with `opts` would
/// resume ([`orphans`]' filter, without its liveness checks): none of an
/// agent whose lane is off ([`lane_on`]), which stays where it was until that
/// switch is back on. A sweep that stands down strands them, and says which.
#[must_use]
pub fn in_flight(opts: &Opts) -> Vec<String> {
    let Ok(dir) = std::fs::read_dir(state_dir(opts)) else {
        return Vec::new();
    };
    let mut out: Vec<String> = dir
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            if path.extension().is_none_or(|x| x != "json") {
                return None;
            }
            let session = path.file_stem()?.to_string_lossy().into_owned();
            let st = load(opts, &session).filter(St::in_flight)?;
            let ours =
                opts.only_sid.as_ref().is_none_or(|s| *s == st.tab) && lane_on(opts, st.agent);
            ours.then_some(session)
        })
        .collect();
    out.sort();
    out
}

/// The agent the first conversation [`in_flight`] names ran (its record's
/// agent): what a successor carrying that restart says it as when it does
/// not land. `None` while nothing is in flight.
#[must_use]
pub fn in_flight_agent(opts: &Opts) -> Option<upgrade::Agent> {
    in_flight(opts)
        .first()
        .and_then(|session| load(opts, session))
        .map(|st| st.agent)
}

fn blank(sf: &SessionFile) -> Report {
    Report {
        pid: sf.pid,
        tab: "-".to_string(),
        session: sf.session_id.clone(),
        from: sf.version.clone(),
        to: "-".to_string(),
        step: String::new(),
    }
}

pub(super) fn first_word(s: &str) -> String {
    s.split_whitespace().take(2).collect::<Vec<_>>().join("-")
}

pub(super) fn said(mut r: Report, step: impl Into<String>) -> Report {
    r.step = step.into();
    r
}

#[cfg(all(test, unix))]
fn visit(
    opts: &Opts,
    sf: &SessionFile,
    t: &[ProcRow],
    targets: &Targets,
    k: &dyn Kernel,
) -> Report {
    // The roster of the home the record was read in, as a pass reads it.
    let files = Records::of_file(&opts.home, sf).session_files();
    visit_with_claim(opts, sf, files.as_deref(), t, targets, k, None, None)
}

#[cfg(all(test, unix))]
#[allow(clippy::too_many_arguments)]
fn visit_with_claim(
    opts: &Opts,
    sf: &SessionFile,
    initial_files: Option<&[SessionFile]>,
    t: &[ProcRow],
    targets: &Targets,
    k: &dyn Kernel,
    prefetched_args: Option<&atpkg::caller_shell::ProcArgs>,
    host_claim: Option<&HostClaim>,
) -> Report {
    visit_models(
        opts,
        sf,
        initial_files,
        t,
        targets,
        k,
        prefetched_args,
        host_claim,
        &ModelCtx::none(),
    )
}

/// The model a notice carried no longer rides its round (`why`,
/// [`models::ride_bar`]): dropped from the round's record `st` before anything
/// reads it, so the visit's retarget ([`St::for_target`]), a gave-up round's
/// late READY and the watch's retarget take the one verdict. Saved at once;
/// ledgered by [`DroppedRide::say`] once the visit knows how the round goes on.
/// An announced round with nothing else to restart it (`idle`) is released
/// ([`St::release_unridden`]). A dry run changes the record in memory alone.
fn ride_dropped(
    opts: &Opts,
    session: &str,
    st: &mut St,
    why: &'static str,
    idle: bool,
    now: u64,
) -> DroppedRide {
    let model = std::mem::take(&mut st.model_list);
    let released = idle && st.release_unridden(now);
    if !opts.dry_run {
        save(opts, session, st);
    }
    DroppedRide {
        why,
        model,
        released,
    }
}

/// A ride the visit dropped ([`ride_dropped`]): the model, the rule's word that
/// barred it, and whether its round was released.
struct DroppedRide {
    why: &'static str,
    model: String,
    released: bool,
}

impl DroppedRide {
    /// The drop's one ledger row (`model-ride-dropped`), with how the round goes
    /// on: `round`, as the visit's retarget left it by `how`, in a retarget
    /// row's own words ([`retarget_how`]). Said only once that is known: the
    /// same visit may yet abandon the round ([`St::retarget`]'s abandoning arm).
    fn say(&self, opts: &Opts, r: &Report, round: &St, how: Retarget) {
        if opts.dry_run {
            return;
        }
        let row = Report {
            to: format!("model:{}", self.model),
            ..said(r.clone(), "model-ride-dropped")
        };
        let then = if self.released {
            "with the build current and nothing else due, nothing is left to restart for: the \
             agent the notice asked to wind down is released, and the upgrade is pending again"
                .to_string()
        } else {
            format!("the round goes on without it, {}", retarget_how(how, round))
        };
        ledger(
            opts,
            &row,
            &format!(
                "{}: the model the notice carried is not one the model rule may move this \
                 conversation to now, so it no longer rides the restart; {then}",
                self.why
            ),
        );
    }
}

/// Whether the process an announcement's notice reached ([`St::notice_pid`],
/// [`St::notice_start`]) is proven gone, and how: `exited` (its pid names no
/// process) or `recycled` (a process started under that pid at another time).
/// A notice without a fence proves nothing, and an unreadable start (`ps`
/// failed, or the process went between reads) is no verdict: each is waited
/// on like a live notified process, which may still answer.
fn notice_process_gone(st: &St) -> Option<&'static str> {
    if st.notice_pid == 0 || st.notice_start.is_empty() {
        return None;
    }
    if !alive(st.notice_pid) {
        return Some("exited");
    }
    kernel_start(st.notice_pid)
        .is_some_and(|start| start != st.notice_start)
        .then_some("recycled")
}

/// One session advanced one step, with the pass's model half ([`ModelCtx`]):
/// a session whose model the rule moves (newest of its own family first, then
/// up the list) restarts when the model ladder ([`models::model_moves_now`])
/// says the move is due, onto a newer build if there is one or else the same
/// build, with the model on the relaunch line (`--model`, session-only; never
/// Claude's `/model`, which also saves the person's default).
#[allow(clippy::too_many_arguments)]
fn visit_models(
    opts: &Opts,
    sf: &SessionFile,
    initial_files: Option<&[SessionFile]>,
    t: &[ProcRow],
    targets: &Targets,
    k: &dyn Kernel,
    prefetched_args: Option<&atpkg::caller_shell::ProcArgs>,
    host_claim: Option<&HostClaim>,
    mctx: &ModelCtx,
) -> Report {
    let mut r = blank(sf);
    // The session file names this process, not a recycled pid.
    if !alive(sf.pid) || kernel_start(sf.pid).as_deref() != Some(squash(&sf.proc_start).as_str()) {
        return said(r, "wait:stale-file");
    }
    if sf.kind != "interactive" || sf.entrypoint != "cli" || !upgrade::is_session_id(&sf.session_id)
    {
        return said(r, "wait:not-an-interactive-cli");
    }
    let fetched_args;
    let args = if let Some(args) = prefetched_args {
        args
    } else {
        let Some(args) = atpkg::caller_shell::process_args(sf.pid) else {
            return said(r, "wait:argv-unreadable");
        };
        fetched_args = args;
        &fetched_args
    };
    let Some(tab) = host_claim
        .map(|claim| claim.tab.clone())
        .or_else(|| args.env_var("ATERM_PARENT_SESSION_ID").map(str::to_owned))
    else {
        // macOS may yield argv without env for another process. The unscoped
        // CLI has no PTY roster claim and must wait rather than guess a tab.
        return said(r, "wait:no-aterm-tab");
    };
    if args
        .env_var("ATERM_PARENT_SESSION_ID")
        .is_some_and(|env_tab| env_tab != tab)
    {
        return said(r, "wait:tab-identity-conflict");
    }
    r.tab.clone_from(&tab);
    if opts.only_sid.as_ref().is_some_and(|s| *s != tab) {
        return said(r, "skip:not-selected");
    }
    if let Err(why) = preliminary_unique_owner(initial_files, sf) {
        return said(r, format!("wait:{why}"));
    }
    let mut prior = load(opts, &sf.session_id);
    // An announcement belongs to its notice's process: nothing here acts on it
    // from another process or tab, and while that process lives this one
    // waits. Once it is proven gone the upgrade is reopened, pending in a new
    // round ([`St::notice_gone`], ledgered once), and this visit goes on as the
    // pending upgrade's, under every first-notice gate.
    if let Some(st) = prior
        .as_mut()
        .filter(|st| matches!(st.phase, Phase::Announced { .. }) && !st.notice_belongs_to(sf, &tab))
    {
        let Some(how) = notice_process_gone(st) else {
            return said(r, "wait:notice-owned-by-other-process");
        };
        let reopen = Report {
            to: format!("{}({})", st.to, st.source),
            ..r.clone()
        };
        if opts.dry_run {
            return said(reopen, "would-reopen:notice-process-gone");
        }
        if let Err(why) = require_unique_owner(&opts.home, sf) {
            return said(r, format!("wait:{why}"));
        }
        let (gone_pid, now) = (st.notice_pid, now_s());
        let dropped = st.notice_gone(&tab, now);
        let reopen = said(reopen, "reopened:notice-process-gone");
        st.note_step(&reopen.step, "", now);
        save(opts, &sf.session_id, st);
        ledger(
            opts,
            &reopen,
            &format!(
                "the process the notice reached is gone (pid {gone_pid} {how}), and this one, \
                 never asked, holds the conversation: the upgrade is pending again in a new \
                 round, and asks it only as a pending upgrade asks — while the conversation is \
                 still behind — under every gate a first notice is{}",
                if dropped.is_empty() {
                    String::new()
                } else {
                    format!("; the release owed to the gone process (`{dropped}`) is dropped")
                }
            ),
        );
    }
    // A relaunch stopped as never picked up, picked up after all
    // ([`St::landed_after_stop`]): reopened as the relaunch that landed
    // (ledgered once), so the owner is not shown `agent-down` and a resume
    // command while that agent runs idle there.
    if let Some(st) = prior
        .as_mut()
        .filter(|st| st.tab == tab && st.landed_after_stop(sf, k))
    {
        let reopen = Report {
            to: format!("{}({})", st.to, st.source),
            ..r.clone()
        };
        if opts.dry_run {
            return said(reopen, "would-reopen:relaunch-landed");
        }
        st.reopen_landed(now_s());
        save(opts, &sf.session_id, st);
        ledger(
            opts,
            &said(reopen, "reopened:relaunch-landed"),
            "the relaunch stopped as never picked up came up after all: the process its \
             shell started holds the conversation, so it is carried on as a relaunch that \
             landed",
        );
    }
    if let Some(mut st) = prior.clone().filter(St::in_flight) {
        r.to = format!("{}({})", st.to, st.source);
        if st.tab != tab {
            return said(r, "wait:conversation-in-other-tab");
        }
        if sf.pid == st.pid {
            // Still exiting: never a harder signal. A shutdown that hangs
            // past the bound is said once on the ledger, and waited on.
            let r = said(r, "wait:exiting");
            note_stuck(opts, &r, &mut st, &sf.session_id, now_s());
            return r;
        }
        // A new process holds the conversation. It is the relaunch only if the
        // shell the line was typed at started it; resumed by hand elsewhere,
        // it is not this upgrade's to tell. The failure is permanent, so it is
        // recorded only from a parent actually read; an unreadable one is a
        // wait.
        match relaunch_of(k, sf, &sf.session_id, st.pid, st.shell) {
            Relaunch::Ours => {}
            Relaunch::Unread => return said(r, "wait:ids"),
            Relaunch::NotOurs if opts.dry_run => return said(r, "would-fail:resumed-elsewhere"),
            Relaunch::NotOurs => {
                st.stop("resumed-elsewhere", now_s());
                let r = said(r, "failed:resumed-elsewhere");
                ledger(
                    opts,
                    &r,
                    "a process this upgrade did not relaunch holds the conversation",
                );
                save(opts, &sf.session_id, &st);
                return r;
            }
        }
        // The relaunch landed.
        if opts.dry_run {
            return said(r, "would-continue");
        }
        let Ok(mut c) = connect(opts, &tab) else {
            return said(r, "wait:no-socket");
        };
        if !roster(&mut c).contains(&tab) {
            return said(r, "wait:tab-not-live");
        }
        if host_claim.is_some_and(|claim| !claim_still_live(&mut c, sf.pid, claim)) {
            return said(r, "wait:tab-ownership-changed");
        }
        return carry_on(opts, r, &mut st, &mut c, &sf.session_id.clone(), sf, None);
    }
    let Some(running) = Version::parse(&sf.version) else {
        return said(r, "wait:version-unreadable");
    };
    let exe = exe_of(sf.pid).unwrap_or_default();
    let running_native = exe.starts_with(native_root(&opts.home));
    // The model half: what the conversation runs now against the list's best
    // available model. A change prefers a cold cache (a switch re-reads the
    // whole history) but never waits on one without bound
    // (`models::model_moves_now`).
    let launch = upgrade::launch_model(&args.argv);
    // The ride: the model a notice of this round carried, which a restart of it
    // would ask for ([`St::carried_model`]), re-judged by this very read
    // ([`models::ride_bar`]).
    let carried = prior.as_ref().and_then(St::carried_model);
    // A file parsed without its source directory cannot establish the
    // process's profile; do not turn the default Records fallback into proof.
    let profile = sf
        .claude_dir
        .as_ref()
        .and_then(|_| Records::of_file(&opts.home, sf).billing_context(&opts.home, args));
    let view = mctx.at_for_profile(opts, sf, profile.as_ref());
    let mread = model_read_for(
        opts,
        sf,
        launch.as_deref(),
        mctx,
        running_native,
        carried,
        Some(&view),
    );
    // A model a person chose by hand since the launch, which a restart the
    // list moves to no model carries in place of the launch's `--model`:
    // read only where a line is planned, not on every visit.
    let hand_model = || {
        hand_model_in(
            opts,
            &Records::of_file(&opts.home, sf),
            &sf.session_id,
            &args.argv,
            models::parse_lstart(&sf.proc_start),
            mctx,
            running_native,
        )
    };
    // Whether a newer build is due regardless of the model — decided before the
    // model, because a restart already happening is the moment the model rides.
    let build_target = upgrade::choose_target(&running, &targets.for_session(running_native));
    // Once announced, the model rides the upgrade's own state: the READY
    // answer warms the cache again, and a decision re-taken then would drop
    // the model half-way. Only while the rule's laws still hold for it: a
    // ride this read barred is dropped from the round below ([`ride_dropped`]).
    let announced_model = prior
        .as_ref()
        .and_then(St::promised_model)
        .filter(|_| mread.ride_barred.is_none());
    let model_to = models::model_to(
        announced_model,
        &mread.verdict,
        mread.cold,
        build_target.is_some(),
        mread.due_for_s,
    );
    // A newer build, or with the build current and the model behind the same
    // build again, relaunched with `--model`: a command-line model is
    // session-only, while Claude's `/model <id>` also rewrites the person's
    // default for new sessions.
    let target = build_target.or_else(|| {
        model_to
            .as_ref()
            .and_then(|_| same_build(targets, &exe, &running, running_native))
    });
    let dropped = match (mread.ride_barred, prior.as_mut()) {
        (Some(why), Some(st)) => Some(ride_dropped(
            opts,
            &sf.session_id,
            st,
            why,
            target.is_none(),
            now_s(),
        )),
        _ => None,
    };
    // The model it runs, which the announcement's reason is read against
    // ([`models::move_why`]).
    let running_model = match &mread.verdict {
        ModelVerdict::Due { from, .. } => Some(from.as_str()),
        ModelVerdict::Keep(_) => None,
    };
    let Some(target) = target else {
        // A dropped ride's round, as the drop left it: released, or a gave-up
        // round kept as it stopped (no retarget follows).
        if let (Some(dropped), Some(st)) = (&dropped, prior.as_ref()) {
            dropped.say(opts, &r, st, Retarget::Kept);
        }
        // Nothing to move, and still a release owed: the conversation runs
        // the build its upgrade stopped short of (resumed by hand on it after
        // a relaunch that failed), and the agent in it was asked to wind down.
        if let Some(st) = prior.clone().filter(|st| !st.release.is_empty()) {
            return release_visit(opts, r, sf, st, &tab, t, k, host_claim);
        }
        return match &mread.verdict {
            ModelVerdict::Due { .. } => said(r, "wait:model-cache-warm"),
            ModelVerdict::Keep(_) => {
                if let Some(gone) =
                    forget_settled_pending(opts, &sf.session_id, prior.as_ref(), &running)
                {
                    let reached = Report {
                        to: gone.to.clone(),
                        ..said(r.clone(), "reached:by-hand")
                    };
                    ledger(
                        opts,
                        &reached,
                        &format!(
                            "the session runs {running}, the build this upgrade was moving it to \
                             ({} at its worst): it got there by another route, so the record is \
                             closed and nothing ages toward the next build",
                            gone.to
                        ),
                    );
                }
                said(r, "current")
            }
        };
    };
    r.to = format!("{}({})", target.version, target.source.as_str());
    // An agent that left its record ([`Records::left_by`]: a worktree under a
    // relative home): the record's status is frozen and its conversation goes
    // on under another home, so a restart would wait on a status that cannot
    // move. Nothing is announced, restarted or recorded until it is back.
    if Records::of_file(&opts.home, sf).left_by(
        &opts.home,
        args.env_var(super::accounts::HOME_ENV),
        || aterm_sysprobe::process_cwd(sf.pid),
    ) {
        return said(r, format!("wait:{HOME_MOVED}"));
    }
    // The restart before this one has not said its model yet: a new upgrade
    // would start from a fresh state and lose that outcome's `done` row.
    if prior.as_ref().is_some_and(St::confirming) {
        return said(r, "wait:confirming");
    }
    let now = now_s();
    let who = squash(&sf.proc_start);
    let who = Proc {
        start: &who,
        started_s: models::parse_lstart(&sf.proc_start).unwrap_or(0),
    };
    // A record for another build than the one this look chose (an older one,
    // or one no longer offered), not yet begun restarting: the visit
    // retargets it ([`St::for_target`]).
    let retargeted_from = prior
        .as_ref()
        .filter(|p| {
            matches!(
                p.phase,
                Phase::Pending | Phase::Announced { .. } | Phase::Failed(_)
            ) && Version::parse(&p.to).as_ref() != Some(&target.version)
        })
        .map(|p| p.to.clone());
    let (mut st, how) = St::retarget(prior, &running, &target, model_to.as_deref(), who, now);
    // The Claude home its agent keeps its records under ([`St::home`]), saved
    // at once where it moved: a look that cannot read the tab returns without
    // saving.
    if st.keep_home(&Records::of_file(&opts.home, sf), &opts.home) {
        save(opts, &sf.session_id, &st);
    }
    // A dropped ride is said with how this retarget left its round.
    if let Some(dropped) = &dropped {
        dropped.say(opts, &r, &st, how);
    }
    // A stop kept for a new process of the conversation: said once, saved
    // at once (a look that cannot read the tab returns without saving).
    if how == Retarget::Rested {
        rested_said(opts, &r, &st, now);
    }
    // The tab a pending upgrade waits in: what the owner names (`aterm
    // harness upgrade <sid> --now`) and what the window shows it under. From
    // the announcement on, `tab` is the notice's fence and is set there.
    if st.phase == Phase::Pending {
        st.tab.clone_from(&tab);
    }
    // The visit's own retarget, ledgered once like the watch's
    // (`retargeted:<from>-><to>`, [`retarget_how`]). Saved before any look: a
    // look that cannot read the tab returns without saving, and the retarget
    // would be said again at every such visit.
    if let Some(old) = retargeted_from {
        save(opts, &sf.session_id, &st);
        let word = format!("retargeted:{old}->{}", target.version);
        ledger(
            opts,
            &Report {
                step: word,
                ..r.clone()
            },
            &format!(
                "upgrade target changed: {old} -> {}; {}",
                target.version,
                retarget_how(how, &st)
            ),
        );
    }
    if matches!(st.request, Request::DeferUntil(t) if now >= t) {
        st.request = Request::None;
        st.request_tab.clear();
        st.request_at = 0;
    }
    // The process proofs, before any socket is dialled and while the upgrade
    // can still act. An environment tab id rides into tmux panes unchanged,
    // and the host's PTY-group match is only a first filter; the job-control
    // and terminal-owner checks prove typing reaches this agent.
    if matches!(st.phase, Phase::Pending | Phase::Announced { .. }) {
        let job = k.job(sf.pid);
        if let Some(job) = not_a_shell_job(job) {
            let r = not_a_job(opts, r, &mut st, job);
            save(opts, &sf.session_id, &st);
            return r;
        }
        // Suspended or put in the background, the shell holds the terminal,
        // and nothing the screen shows is the agent's.
        if let Err(why) = foreground_shell(job) {
            st.note_wait(why, now);
            save(opts, &sf.session_id, &st);
            return said(r, format!("wait:{why}"));
        }
        let owner = k.terminal(sf.pid);
        let owner = owner.as_ref().map(|(pid, name)| (*pid, name.as_str()));
        if !owner.is_some_and(owned_by_aterm) {
            let why = format!("terminal:{}", owner_word(owner));
            st.note_wait(&why, now);
            let r = held_back(opts, r, &mut st, &sf.session_id, &why);
            save(opts, &sf.session_id, &st);
            return r;
        }
    }
    let (mut c, mut facts, shown_mode, looked_hand, seen) =
        match look(opts, sf, &mut st, &tab, host_claim, t, now) {
            Ok(look) => look,
            Err(why) => return said(r, format!("wait:{why}")),
        };
    // Aterm's own words left in the prompt, first ([`left_first`]).
    if let Some(r) = left_first(opts, &r, &mut st, &mut c, &tab, sf, now) {
        return r;
    }
    // The transcript tail, read every visit: for the login wall
    // ([`login_facts`]); once announced or with a release owed, for READY; at
    // a restart, for the model the agent ran and where the resumed session's
    // turns will start.
    let records = Records::of_file(&opts.home, sf);
    let recent = records
        .transcript(&sf.session_id)
        .map(|p| tail_to_end(&p, TAIL_BYTES));
    let tail = recent.as_ref().map(|(text, _)| text.as_str());
    let rearmed = login_facts(&records, &sf.session_id, &mut st, &mut facts, tail, now);
    // A usage limit the transcript says stands, and a notice queued behind
    // one: what the screen alone cannot say ([`queue_facts`]).
    let asked = st.request_for(&tab);
    let requeued = queue_facts(&mut st, &mut facts, tail, now, &asked);
    let ready = heard(&st, sf, &tab, tail);
    // A READY the screen shows before the transcript records it, and one the
    // agent was moved on from since.
    ready_facts(&st, sf, &tab, tail, ready, &mut facts);
    // How long that READY has stood unacted on, held at the limit: what
    // bounds the agent's own background work under it.
    st.time_ready(ready, &mut facts, now);
    // How long a stopped round has rested: past RETRY_S a new one starts.
    st.time_failed(&mut facts, now);
    // Whether the agent's own work, seen since the latest notice, has ended.
    st.time_work(&mut facts, now);
    let request = st.request_for(&tab);
    // A give-up spent on notices that never reached the model is asked about
    // as the upgrade would be had they spent no ask ([`upgrade::rearmed`]),
    // unless a READY came first. The refund is in that phase, so the notice
    // it leads to is the next ask.
    let mut asked_about = match rearmed {
        Some(phase) if !ready => {
            facts.undelivered = false;
            phase
        }
        _ => st.phase.clone(),
    };
    let taken_back = asked_about != st.phase;
    let mut step = upgrade::requested_step(&request, &asked_about, &facts, ready, now, &st.to);
    // At a break of the agent's own background work nothing is ended,
    // whatever the plan says.
    if opts.background {
        step = upgrade::break_step(step);
    }
    // The end of the agent's own work re-arms a given-up round and asks in the
    // same visit ([`upgrade::Facts::work_ended`]), so the loop never gets the
    // point in between: the re-arm is ledgered as usual, and the new round's
    // first notice goes here under every first-notice gate.
    let mut rearmed_here = None;
    if step == Step::Rearm
        && facts.work_ended
        && matches!(&asked_about, Phase::Failed(why) if why == upgrade::GAVE_UP)
        && !opts.dry_run
    {
        rearmed_here = Some(rearm(opts, r.clone(), &mut st, now));
        let request = st.request_for(&tab);
        step = upgrade::requested_step(&request, &st.phase, &facts, false, now, &st.to);
        asked_about = st.phase.clone();
    }
    // The last read before anything is typed or signalled: the agent holds its
    // terminal. If suspended or backgrounded since the first look, the shell
    // holds it while the agent's last frame may still read as an idle empty
    // composer. The shell proven here is the one the plan is made against.
    let mut proven = None;
    if matches!(step, Step::Announce | Step::Terminate | Step::Fresh) {
        match foreground_shell(k.job(sf.pid)) {
            Ok(shell) => proven = Some(shell),
            Err(why) => step = Step::Wait(why),
        }
    }
    // A BUILD THAT FAILED TO START — for any conversation, this one's own
    // rested round included — is moved onto by no agent until it is proved
    // to start without one ([`build_starts`]; the owner's `--now` probes a
    // build no probe proves any more, for this agent): an announcement asks
    // for a move that would not come, and a restart ends a working agent
    // onto a build that exits at once (handoff §4, round eighteen). Nor is
    // a build a crash's probe found exiting. A build never run here is
    // probed once before the first agent is moved onto it, and held only
    // where that probe saw it exit with a failure ('s LC-5;
    // a probe that could not run it or did not see it end holds none): only
    // a crash's relaunch needs a probe's proof first ([`build_proved`], LC-5
    // of ). A model move on the build the agent already runs
    // moves it onto no other build.
    // Asked wherever the round would move, and wherever it has asked its
    // agent, whatever its step: such a round is abandoned and its agent
    // released at once, never left winding down, or stopped for a restart,
    // that the hold keeps from coming; a round that asked nothing waits, any
    // release it owes typed ([`upgrade::build_hold`]).
    let moving = matches!(step, Step::Announce | Step::Terminate | Step::Fresh);
    if target.version != running
        && (upgrade::asked_for_a_move(&asked_about, ready) || moving)
        && build_starts(opts, &r, &target, now, request == Request::Now, moving).is_err()
    {
        step = upgrade::build_hold(step, &asked_about, ready);
    }
    // Where a dry read that would act stands ([`upgrade::held_point`]): the
    // give-up and re-arm are taken whatever the session shows, so a read off
    // any point (the host's watch) says what holds the point they wait for,
    // after the act's word ([`at_point`]), as a wait word
    // ([`upgrade::wait_word`]). A real visit at a point takes the step.
    let held_point = opts
        .dry_run
        .then(|| upgrade::held_point(&request, &step, &facts, now, &st.to))
        .flatten()
        .map(|w| upgrade::wait_word(w, &facts.status));
    let result = match step {
        Step::Wait("attended") => attended_once(opts, r, &mut st),
        Step::Wait(why) => said(r, format!("wait:{why}")),
        // No stop is for good: the round that stopped has rested, and a new
        // one starts — its first notice at the next look, under every gate.
        Step::Rearm => rearm(opts, r, &mut st, now),
        Step::GiveUp => {
            if let Err(why) = require_unique_owner(&opts.home, sf) {
                return said(r, format!("wait:{why}"));
            }
            st.give_up(now);
            let r = said(r, "gave-up");
            ledger(
                opts,
                &r,
                &gave_up_words(
                    upgrade::Agent::Claude,
                    &k.describe(sf.pid, &background_procs(sf.pid, t)),
                    facts.quiet,
                ),
            );
            r
        }
        Step::Void(BUILD_FAILS) => build_held(opts, r, &mut st, now),
        Step::Void(why) => drain_expired(opts, r, &mut st, why, &facts, now),
        Step::Announce => {
            if let Err(why) = require_unique_owner(&opts.home, sf) {
                return said(r, format!("wait:{why}"));
            }
            // The look's pill, no read of its own: the upgrade's exchanges with
            // the host are fenced and counted.
            let mode = resume_mode(&records, &sf.session_id, shown_mode.clone());
            let model = model_to.clone().or_else(hand_model);
            let planned = proven.map_or(Err(NoPlan::Wait("ids")), |shell| {
                // The home and the login this very process runs under, off
                // the exec environment the visit already read.
                let home = args.env_var(super::accounts::HOME_ENV);
                let homedir = home_carried(&opts.home, home, &sf.cwd, &records)?;
                plan(
                    opts,
                    &Launch {
                        homedir: homedir.as_deref(),
                        billing: if model_to.is_some() {
                            super::relaunch::ModelBilling::Move(profile.as_ref())
                        } else {
                            super::relaunch::ModelBilling::Keep
                        },
                        ..launch_of(
                            sf,
                            &args.argv,
                            &target,
                            model.as_deref(),
                            mode.as_deref(),
                            true,
                            home,
                            args.env_var(super::accounts::LOGIN_ENV),
                            stored_login_of(&args.env),
                        )
                    },
                    shell,
                    t,
                )
            });
            // The line the announcement was planned on: what the restart after
            // READY is to type (it plans its own again from its own look).
            let planned_line = planned.as_ref().map(|p| p.line.clone()).ok();
            let salt = st.salt;
            let asks = upgrade::announce_asks(&asked_about, facts.undelivered);
            let again = if taken_back {
                Some(
                    "again: the upgrade had given up on notices that never reached the model, \
                     each answered by the login wall",
                )
            } else if requeued {
                Some(
                    "again: the notice before it never reached the model, its turn answered by \
                     a usage limit that is over",
                )
            } else if facts.undelivered {
                Some(
                    "again: the notice before it never reached the model, its turn answered by \
                     the login wall",
                )
            } else {
                None
            };
            // What a later try must still find on its fenced read
            // ([`retry_open`]): the look's gates, as the owner's word left them.
            let retry = Retry {
                looked: looked_hand,
                owner_now: st.request_for(&tab) == Request::Now,
                at_break: facts.background_point,
                // The person's grace at the look's rung: a person merely near
                // the tab holds a retry no longer than they held the look.
                grace_s: upgrade::person_grace(upgrade::rung_in_force(&facts), opts.human_grace_s),
            };
            // A notice the host stopped part-way, or typed whole and did not
            // send: its text and why ([`left_cut`]).
            let mut cut: Option<(String, String)> = None;
            let result = announce(opts, r, &mut st, planned, now, asks, again, |asks| {
                let marker = upgrade::ready_marker(
                    &sf.session_id,
                    &target.version,
                    salt.wrapping_add(u64::from(asks)),
                );
                // What runs under the agent, named, so it can tell live work
                // from a wait that can never end ([`upgrade::STOPPING_POINT`]).
                let text = upgrade::with_running(
                    &upgrade::prepare_prompt_with_model(
                        &running,
                        &target.version,
                        target.source,
                        &marker,
                        model_to.as_deref(),
                        running_model,
                    ),
                    &upgrade::running_clause(&k.describe(sf.pid, &background_procs(sf.pid, t))),
                );
                require_unique_owner(&opts.home, sf).map_err(str::to_string)?;
                if host_claim.is_some_and(|claim| !claim_still_live(&mut c, sf.pid, claim)) {
                    return Err("tab-ownership-changed".to_string());
                }
                // The last look before typing, under aterm's own hand and fence
                // ([`type_fenced`]), the words pressed key by key.
                match type_fenced(
                    &mut c,
                    &tab,
                    &text,
                    Some(&retry),
                    Put::Pressed,
                    opts.timings.fence_every,
                ) {
                    Ok(()) => Ok(marker),
                    Err(Typed::Cut(why)) => {
                        let word = Typed::Cut(why.clone()).word();
                        cut = Some((text, why));
                        Err(word)
                    }
                    Err(e) => Err(e.word()),
                }
            });
            if let Some((text, _)) = cut {
                left_cut(&mut st, &mut c, &tab, &text);
            }
            if result.step.starts_with("announced:") {
                // The ask the end of the agent's work led to is spent from
                // the day's budget ([`St::work_end_open`]).
                if facts.work_ended {
                    st.spend_work_end(now);
                }
                st.model_list = model_to.clone().unwrap_or_default();
                st.line = planned_line.unwrap_or_default();
                st.login = args.env_var(super::accounts::LOGIN_ENV).map(str::to_owned);
                st.tab.clone_from(&tab);
                st.notice_pid = sf.pid;
                st.notice_start = squash(&sf.proc_start);
            }
            result
        }
        Step::Terminate if opts.dry_run => said(r, "would-restart"),
        Step::Fresh if opts.dry_run => said(r, "would-restart:fresh"),
        // The restart: after READY, or afresh for a taskless conversation,
        // with nothing told and nothing resumed.
        Step::Terminate | Step::Fresh => {
            let fresh = step == Step::Fresh;
            // A quiet restart ([`upgrade::Facts::quiet`]) of a round that typed
            // nothing: its model rides the restart as a fresh one's does, and
            // nothing is typed after it. A pending round that still owes this
            // agent a release typed it a notice once ([`St::owe_release`]), so
            // the agent may have answered READY and stopped: it is carried on
            // as an announced round's is.
            let unasked = !fresh && facts.quiet && asked_about == Phase::Pending;
            let owed_here = !st.release.is_empty() && st.notice_belongs_to(sf, &tab);
            let quiet = unasked && !owed_here;
            st.quiet = quiet;
            if fresh || unasked {
                st.model_list = model_to.clone().unwrap_or_default();
            }
            if !fresh {
                // What the agent ran, from the tail that just proved READY, and
                // where that read ended (the resumed session's turns come
                // after it); a restart that waits instead takes both again.
                (st.model_before, st.mark) = model_and_mark(recent.as_ref());
            }
            st.launch_model = launch_kept(opts, &sf.session_id, &args.argv).unwrap_or_default();
            let hand = st.model_list.is_empty().then(hand_model).flatten();
            let restarting = Restarting {
                argv: &args.argv,
                target: &target,
                host_claim,
                // The person's grace at the rung in force (`--now` is the last
                // rung).
                grace: upgrade::person_grace(
                    if request == Request::Now {
                        upgrade::Rung::Land
                    } else {
                        facts.rung
                    },
                    opts.human_grace_s,
                ),
                fresh,
                quiet,
                // The quiet step reads a lagging status as past only on the
                // owner's `--now`: the last look before the signal reads it the
                // same way, or the owner's word would move nothing.
                lagging: !fresh
                    && facts.quiet
                    && request == Request::Now
                    && upgrade::status_lags(&facts),
                mode: shown_mode.as_deref(),
                model: hand.as_deref(),
                login: args.env_var(super::accounts::LOGIN_ENV),
                home: args.env_var(super::accounts::HOME_ENV),
                stored_login: stored_login_of(&args.env),
                profile: profile.as_ref(),
            };
            restart(opts, r, &mut st, &mut c, &tab, sf, t, k, &restarting)
        }
    };
    // The word the restart's own checks said, past the reducer's go
    // ([`St::restart_look`]): kept only where it is the visit's last word.
    let restart_word = (!opts.dry_run && matches!(step, Step::Terminate | Step::Fresh))
        .then(|| result.step.clone());
    // The owed release ([`release`]), here if it is the next act and may be
    // typed, else at the next point; a background-work break counts, since the
    // line ends nothing. The READY it waits behind is re-read off the record
    // as the step left it (a stop in this step forgot its markers).
    let result = if release_next(&step, &result.step, &st, &facts, ready) {
        let ready = heard(&st, sf, &tab, tail);
        release(
            opts, result, &mut st, &mut c, &tab, sf, host_claim, k, &facts, ready, tail,
        )
    } else {
        result
    };
    // A work-end re-arm whose notice did not go this visit is still the
    // visit's act, reported as the re-arm so the host restarts its counts.
    let result = match rearmed_here {
        Some(rearmed) if !result.step.starts_with("announced:") => rearmed,
        _ => result,
    };
    // What this look saw of the agent's own work, for the end of it.
    st.saw_work(&facts);
    st.note_step(&result.step, &facts.status, now);
    st.restart_look =
        result.step.starts_with("wait:") && restart_word.as_deref() == Some(result.step.as_str());
    // Whose copy runs: the vendor's own install (its updater may already have
    // laid the newer build down — "Restart to update" in its footer) or the
    // managed twin (which exports DISABLE_AUTOUPDATER).
    let copy = if running_native { "foreign" } else { "managed" };
    st.annotate_wait(
        now,
        &wait_note(
            &facts,
            &format!("{seen} copy={copy}"),
            facts.background.len(),
        ),
    );
    // What holds a move that waits, named to the owner (`held_by=`): described
    // only where something runs and the step waited.
    let held = if facts.background.is_empty() || !result.step.starts_with("wait:") {
        Vec::new()
    } else {
        k.describe(sf.pid, &background_procs(sf.pid, t))
    };
    st.note_held(&result.step, &held, now);
    save(opts, &sf.session_id, &st);
    match held_point {
        Some(held) if rounds_step(&result.step) => {
            let word = at_point(&result.step, &held);
            said(result, word)
        }
        _ => result,
    }
}

/// The separator between a dry read's act and what holds its point
/// ([`at_point`]): `gave-up@box`, `would-rearm:unanswered@not-idle:busy`. No
/// step or wait word contains it ([`upgrade::wait_word`] cuts a status to
/// `[a-z-]`).
pub(super) const AT_POINT: char = '@';

/// Whether a dry read's word is the round's own next step: the reducer's
/// announce (`would-announce`), restart (`would-restart`,
/// `would-restart:fresh`, `would-restart:model=…`), give-up (`gave-up`, the
/// one act a dry run reports as done) or re-arm (`would-rearm:<why>`),
/// whatever [`at_point`] appended. Not the lane's edges (an owed release, a
/// void, a refusal, a restart carried on), for which the watch records no
/// word (`upgrade_watch::look_word`).
pub(super) fn rounds_step(word: &str) -> bool {
    let act = word.split_once(AT_POINT).map_or(word, |(act, _)| act);
    act == "would-announce"
        || act == "gave-up"
        || act == "would-restart"
        || act.starts_with("would-restart:")
        || act.starts_with("would-rearm:")
}

/// A dry read's act `word` with what holds its point (`held`, a wait word):
/// `<word>@<held>` ([`AT_POINT`]). Said by the visit's dry run for a give-up
/// or re-arm read off any point ([`upgrade::held_point`]) and recorded by the
/// host's watch.
pub(super) fn at_point(word: &str, held: &str) -> String {
    format!("{word}{AT_POINT}{held}")
}

/// The ladder's reading of one look by agent `agent`'s reader (`claude`,
/// `codex`), shared by both lanes' looks and the ladder's Tier-1 bind, so the
/// bind reads a screen exactly as a look does: the rung ([`St::rung_at`]); a
/// person near the tab within `grace_s` ([`upgrade::attended_by`]) and a
/// keystroke within [`upgrade::KEYS_GAP_S`] (an unknown stamp is none here:
/// `attended` fails closed below KeysOnly and typing shows as a draft); the
/// reader's idle reading with no wall or prompt; and the repaint-proof settle
/// ([`St::still`], [`upgrade::still_print`]).
pub(super) fn ladder_look(
    st: &mut St,
    agent: &str,
    scr: &Screen,
    pid: u32,
    grace_s: u32,
    now: u64,
) -> LadderLook {
    let reading = aterm_phase::read_at(
        Some(agent),
        &scr.rows,
        scr.cursor.map(|(row, _)| row),
        scr.cursor.map(|(_, col)| col),
    );
    let idle_read = reading.phase == aterm_phase::Phase::Idle
        && reading.phase_authoritative
        && reading.wall.is_none()
        && reading.prompt.is_none();
    let print = reading
        .composer
        .as_ref()
        .filter(|_| idle_read)
        .map(|(caret, _)| upgrade::still_print(pid, &scr.rows, *caret));
    LadderLook {
        rung: st.rung_at(now),
        attended: upgrade::attended_by(scr.human, grace_s),
        typing: scr
            .human
            .within(upgrade::person_grace(upgrade::Rung::KeysOnly, grace_s))
            == Some(true),
        idle_read,
        still_s: st.still(print, now),
    }
}

/// [`ladder_look`]'s answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct LadderLook {
    /// [`upgrade::Facts::rung`].
    pub(super) rung: upgrade::Rung,
    /// [`upgrade::Facts::attended`].
    pub(super) attended: bool,
    /// [`upgrade::Facts::typing`].
    pub(super) typing: bool,
    /// The one reader read the screen idle as evidence, no wall, no prompt:
    /// what a run of idle looks counts ([`St::idle_looks`]), and where the
    /// window's loop would take its step at all.
    pub(super) idle_read: bool,
    /// [`upgrade::Facts::still_s`].
    pub(super) still_s: u64,
}

/// Whether a turn runs, by the `esc to interrupt` hint
/// ([`upgrade::Facts::busy_footer`]), read only below the composer (its bottom
/// rule and the footer, where Claude Code draws it during a turn) and on the
/// live status row just above the composer's top rule (the spinner row, e.g.
/// `✶ Deliberating… (4s · ↑ 1.2k tokens · esc to interrupt)`), read as its
/// LINE ([`aterm_phase::phase::status_line`]: a narrow pane wraps the hint,
/// `… · esc to` over `  interrupt)`). A footer quoted in the transcript is
/// therefore no running turn. No composer means a box replaced it, which the
/// `box` gate waits on.
pub(super) fn busy_footer(rows: &[String]) -> bool {
    let Some((caret, draft)) = aterm_phase::phase::composer_draft(rows) else {
        return false;
    };
    let busy = aterm_phase::anchors::anchor("busy.interrupt");
    aterm_phase::phase::status_line(rows).is_some_and(|line| line.contains(busy))
        || rows
            .iter()
            .skip(caret.saturating_add(draft.len()))
            .any(|row| row.contains(busy))
}

/// One look at the session's tab, everything the gates are asked over: the
/// tab dialled and proven live (and, for the window's step, still the PTY
/// group's claim), its screen read once, and that read's [`Facts`] (person
/// stamp, hold, composer, box, busy footer, the limit ([`upgrade::limited`]),
/// work under the agent), with the screen's quiet and a person's hold timed
/// across looks on `st`, an announced upgrade's re-ask clock held while
/// limited ([`upgrade::clock_held`]), and the permission mode the read's pill
/// shows ([`screen_mode`]), which a relaunch resumes in. `Err` names the
/// wait.
fn look(
    opts: &Opts,
    sf: &SessionFile,
    st: &mut St,
    tab: &str,
    host_claim: Option<&HostClaim>,
    t: &[ProcRow],
    now: u64,
) -> Result<(Client, Facts, Option<String>, HandAt, String), &'static str> {
    let mut c = connect(opts, tab).map_err(|_| "no-socket")?;
    if !roster(&mut c).iter().any(|live| live == tab) {
        return Err("tab-not-live");
    }
    if host_claim.is_some_and(|claim| !claim_still_live(&mut c, sf.pid, claim)) {
        return Err("tab-ownership-changed");
    }
    let scr = screen(&mut c, tab).ok_or("screen-unreadable")?;
    // One `status` read: the hold and hands, and the agent's verdict the quiet
    // is measured from ([`quiet_s`]); and the composer's placeholder cell.
    let status = status_line(&mut c, tab);
    let read = TabRead {
        scr: &scr,
        status: status.as_deref(),
        dim: dim_placeholder(&mut c, tab, &scr),
    };
    let (facts, seen) = look_facts(opts, sf, st, tab, &read, t, now);
    Ok((
        c,
        facts,
        screen_mode(&scr.rows),
        HandAt::of(scr.human),
        seen,
    ))
}

/// What [`look`] reads off the tab over the socket, handed to [`look_facts`]:
/// the screen, the `status` reply, and whether the composer's placeholder
/// cell reads dim ([`dim_placeholder`]).
pub(super) struct TabRead<'a> {
    pub(super) scr: &'a Screen,
    pub(super) status: Option<&'a str>,
    pub(super) dim: bool,
}

/// The pure half of [`look`]: a Claude Code look's [`Facts`] from what it
/// read off the tab (`read`), timed across looks on `st`, and the wait log's
/// `seen` note. Reads nothing over the socket, so the ladder's Claude bind
/// (`upgrade_ladder_claude_tests.rs`) drives exactly this assembly.
pub(super) fn look_facts(
    opts: &Opts,
    sf: &SessionFile,
    st: &mut St,
    tab: &str,
    read: &TabRead<'_>,
    t: &[ProcRow],
    now: u64,
) -> (Facts, String) {
    let TabRead { scr, status, dim } = *read;
    // A person at the tab and the ladder's reading of this screen
    // ([`ladder_look`]).
    let ladder = ladder_look(st, "claude", scr, sf.pid, opts.human_grace_s, now);
    let quiet = quiet_s(status, st, scr.seq, now);
    let mut facts = Facts {
        status: sf.status.clone(),
        status_age_s: now.saturating_sub(sf.status_updated_at_ms / 1000),
        composer_empty: upgrade::composer_is_empty(&scr.rows, scr.cursor, dim),
        approval_box: aterm_phase::prompt::prompt_box_span(&scr.rows).is_some(),
        busy_footer: busy_footer(&scr.rows),
        background: background(sf.pid, t),
        // The hold and the hands alone: a person's keystroke is `attended`,
        // which the owner's `--now` waives.
        held: status.is_none_or(|line| held_by_status(line, 0)),
        quiet_s: quiet,
        hold_s: 0,
        owner_now: false,
        attended: ladder.attended,
        background_point: opts.background,
        // Asked only where it can move the step: before anything is ended.
        taskless: matches!(st.phase, Phase::Pending | Phase::Announced { .. })
            && Records::of_file(&opts.home, sf).tasked(
                &sf.session_id,
                &conversation_typed(opts, tab, Some(&sf.session_id), &[]),
            ) == Some(false),
        limited: upgrade::limited(&scr.rows),
        // The screen's word; the transcript's is added by the visit, which
        // reads the tail ([`visit_models`]).
        login: upgrade::login_wall(upgrade::Agent::Claude, &scr.rows),
        undelivered: false,
        // The transcript's word, added by the visit ([`queue_facts`]).
        queued: false,
        ready_s: 0,
        idle_looks: 0,
        // Stamped by the visit ([`St::time_failed`]).
        failed_s: 0,
        rung: ladder.rung,
        typing: ladder.typing,
        still_s: ladder.still_s,
        // Claude Code's own work shows in its status and under it.
        // Stamped by the visit ([`St::time_work`]).
        work_ended: false,
        // The screen's half: the agent's last words on an idle-as-evidence
        // screen answer READY to this round. The visit keeps it only where the
        // transcript does not yet hear that READY ([`visit_models`]).
        ready_unrecorded: ladder.idle_read
            && !opts.background
            && aterm_phase::said_tail(&scr.rows)
                .is_some_and(|said| st.round_markers().any(|m| upgrade::says_ready(&said, m))),
        // The transcript's word, added by the visit.
        ready_superseded: false,
        // `[harness] upgrade_ask = false`, the default: nothing is typed
        // before the move ([`upgrade::Facts::quiet`]). Nor, whatever the
        // key, into a conversation whose record says its task is finished
        // (asked only before any notice, read on from where this process's
        // last look ended, its first from [`FINISHED_ONCE_BYTES`] back:
        // [`Records::finished`]): no notice, release or carry-on reopens a
        // task its worker closed ([`upgrade::FinishedScan`]). Nor into A
        // PERSON'S OWN EXCHANGE a loop left to them, still the conversation's
        // last turn ([`left_to_person`]): the harness types
        // nothing there until someone else's turn ends, and a notice would
        // ask the agent to answer inside it. The move goes the quiet way, at
        // a genuinely idle point, resumed with nothing typed (the relaunch's
        // `done:left`) — a round that announced before the exchange too,
        // whose re-ask would be typed there (its READY superseded by the
        // exchange, or its work still running at the re-ask's time). Read
        // only while a loop's row is open ([`lefts_through`]).
        quiet: !opts.ask
            || (st.phase == Phase::Pending
                && Records::of_file(&opts.home, sf).finished(
                    &sf.session_id,
                    &conversation_typed(opts, tab, Some(&sf.session_id), &[]),
                ) == Some(true))
            || (matches!(st.phase, Phase::Pending | Phase::Announced { .. })
                && left_to_person(
                    opts,
                    &Records::of_file(&opts.home, sf),
                    tab,
                    &sf.session_id,
                    st,
                )),
        // The owner's waiting record has stood a look before a move that
        // types nothing first ([`upgrade::Facts::unnoticed`]).
        unnoticed: now.saturating_sub(st.behind_since()) < upgrade::quiet_s(),
    };
    // The session's reader, as the loop vets an idle point: what Claude's
    // status is read against ([`upgrade::IDLE_LOOKS`]). A run is one
    // process's consecutive looks ([`St::idle_looks`]).
    let idle_read = ladder.idle_read;
    let in_a_row = st.idle_pid == sf.pid && now.saturating_sub(st.idle_at) <= IDLE_RUN_GAP_S;
    st.idle_looks = match (idle_read, in_a_row) {
        (false, _) => 0,
        (true, true) => st.idle_looks.saturating_add(1),
        (true, false) => 1,
    };
    (st.idle_pid, st.idle_at) = if idle_read { (sf.pid, now) } else { (0, 0) };
    facts.idle_looks = st.idle_looks;
    (st.hold_since_s, st.hold_seen_s) =
        upgrade::hold_since((st.hold_since_s, st.hold_seen_s), &facts, now);
    if st.hold_since_s != 0 {
        facts.hold_s = now.saturating_sub(st.hold_since_s);
    }
    st.phase = upgrade::clock_held(&st.phase, &facts, now);
    // What the look saw that no fact carries, for the wait log ([`wait_note`]):
    // the engine's render state, and where the cursor and the composer's caret
    // stood — the two a displaced grid parts.
    let render = status
        .and_then(|line| {
            line.split_whitespace()
                .find_map(|t| t.strip_prefix("render="))
        })
        .unwrap_or("-");
    let place = |at: Option<usize>| at.map_or_else(|| "-".to_string(), |n| n.to_string());
    // A `/goal` in force keeps the agent's status busy at every turn end.
    st.goal = aterm_phase::goal_active(&scr.rows);
    let seen = format!(
        "render={render} cursor={} caret={} goal={}",
        scr.cursor
            .map_or_else(|| "-".to_string(), |(row, col)| format!("{row},{col}")),
        place(aterm_phase::phase::composer_draft(&scr.rows).map(|(caret, _)| caret)),
        u8::from(st.goal),
    );
    (facts, seen)
}

/// The facts a wait was decided on, one line, for the wait log ([`St::waits`]):
/// the gates' inputs by name, `seen` what the look read beside them ([`look`]),
/// and how many processes ran under the agent.
fn wait_note(f: &Facts, seen: &str, holders: usize) -> String {
    let b = |v: bool| u8::from(v);
    format!(
        "status={} attended={} held={} composer_empty={} box={} busy_footer={} limited={} \
         login={} queued={} quiet_s={} idle_looks={} ready_s={} hold_s={} background={} {seen} \
         holders={holders}",
        f.status,
        b(f.attended),
        b(f.held),
        b(f.composer_empty),
        b(f.approval_box),
        b(f.busy_footer),
        b(f.limited),
        b(f.login),
        b(f.queued),
        f.quiet_s,
        f.idle_looks,
        f.ready_s,
        f.hold_s,
        f.background.len(),
    )
}

/// How far back a give-up's last notice is looked for when the ordinary tail
/// ([`TAIL_BYTES`]) no longer holds it ([`login_facts`]); a conversation can
/// run on for hundreds of KB after the login returns.
const GAVE_UP_TAIL_BYTES: u64 = 8 * 1024 * 1024;

/// The login wall from the transcript `tail` ([`upgrade::Facts::login`],
/// [`upgrade::Facts::undelivered`]), which the screen alone cannot give: the
/// wall still standing after its row left the screen
/// ([`upgrade::transcript_login_wall`]: wait `login`, re-ask clock held); the
/// wall lifted since the latest notice (the window restarts from the lift,
/// [`St::hold_clock`], so a notice read before the login went is not re-asked
/// at once); and whether the latest notice reached the model
/// ([`upgrade::transcript_login`]).
///
/// For a given-up upgrade ([`upgrade::GAVE_UP`]) the round's notices are read
/// further back ([`GAVE_UP_TAIL_BYTES`]), and where its last notice was the
/// wall's the give-up is taken back ([`upgrade::rearmed`]), counting the
/// notices the model did receive by the recorded markers, else as they were
/// minted ([`upgrade::round_markers`]).
fn login_facts(
    records: &Records,
    session: &str,
    st: &mut St,
    f: &mut Facts,
    tail: Option<&str>,
    now: u64,
) -> Option<Phase> {
    let said = tail.map_or_else(upgrade::TranscriptLogin::default, |t| {
        upgrade::transcript_login(&st.phase, &st.marker, t)
    });
    if said.stands {
        f.login = true;
        st.phase = upgrade::clock_held(&st.phase, f, now);
    }
    if let Some(lifted) = said.lifted_at {
        let _ = st.hold_clock(lifted);
    }
    let gave_up =
        matches!(&st.phase, Phase::Failed(why) if why == upgrade::GAVE_UP) && !st.marker.is_empty();
    if !gave_up {
        f.undelivered = said.undelivered == Some(true);
        return None;
    }
    let deep = records
        .transcript(session)
        .map(|p| tail_to_end(&p, GAVE_UP_TAIL_BYTES).0);
    let text = deep.as_deref().or(tail)?;
    f.undelivered = upgrade::notice_fate(text, &st.marker) == Some(true);
    let markers = if st.asked.is_empty() {
        Version::parse(&st.to)
            .map_or_else(Vec::new, |to| upgrade::round_markers(session, &to, st.salt))
    } else {
        st.asked.clone()
    };
    upgrade::rearmed(
        &st.phase,
        f.undelivered,
        upgrade::notices_received(text, &markers),
    )
}

/// A usage limit, and a notice queued behind one, from the transcript `tail`.
///
/// * The session's last word is Claude Code's limit row and its reset (else
///   [`upgrade::REASK_S`] after the row) is still to come
///   ([`upgrade::transcript_limit_until`]; a later finished `/login` ends it):
///   limited whatever the screen shows. Under the quiet upgrade a screen's
///   notice [`LIMIT_GRACE_S`] past a named reset is no limit.
/// * The latest upgrade notice (this ask's when announced, else any, since
///   retargets and new rounds leave notices with markers the state no longer
///   names; one scan, [`upgrade::notice_scan`]) answered by the limit and not
///   taken ([`upgrade::NoticeFate::Queued`]): limited, nothing typed behind it,
///   clock held ([`upgrade::clock_held`]), while the limit holds by the
///   transcript ([`upgrade::Scan::queued_until`]). After that, with no limit on
///   screen, this ask's notice never reached the model
///   ([`upgrade::Facts::undelivered`], retyped as the same ask), and another's
///   leaves a pending upgrade free to follow it. More than
///   [`upgrade::REQUEUE_MAX`] untaken copies is a full queue
///   ([`upgrade::Facts::queued`]): nothing typed, clock held, until the model
///   writes a row of its own, the queue has rested
///   ([`upgrade::Scan::rests_until`]) or the owner's `--now` follows the latest
///   notice, never while a limit stands. Each lets exactly one more notice go.
/// * Taken later ([`upgrade::NoticeFate::Taken`]): its window opens when the
///   model took it ([`St::hold_clock`]).
///
/// Bound with [`upgrade::next_step`], [`upgrade::requested_step`] and
/// [`upgrade::announce_asks`] to `harness_upgrade_limit_queue_model`
/// (`upgrade_queued_tests.rs`).
fn queue_facts(
    st: &mut St,
    f: &mut Facts,
    tail: Option<&str>,
    now: u64,
    request: &Request,
) -> bool {
    let Some(text) = tail else {
        return false;
    };
    // Until when the limit holds (its named reset, else a re-ask's wait after a
    // row naming none), and the named reset alone for the owner's view
    // ([`St::limit_until`]; `0`: none named, bounded in the view by
    // `upgrade_status::LONGEST_LIMIT_S`).
    let until = upgrade::transcript_limit_until(text);
    let reset = upgrade::transcript_limit_reset(text);
    st.limit_until = reset.unwrap_or(0);
    // And whether that limit is money, no usage window ([`St::limit_spend`]).
    st.limit_spend = upgrade::transcript_limit_spend(text);
    // Under the quiet upgrade a screen's limit notice [`LIMIT_GRACE_S`] past
    // the reset its row names is no limit: the loop offers the point
    // (`supervise::run`'s `limit_spent`) and the restart, which types nothing,
    // goes there. A future reset, or a row naming none, leaves the screen's
    // word: a reset is never guessed. The ask flow keeps the screen's word.
    if f.quiet && reset.is_some_and(|reset| now >= reset.saturating_add(LIMIT_GRACE_S)) {
        f.limited = false;
    }
    if until.is_some_and(|until| until > now) {
        f.limited = true;
        st.phase = upgrade::clock_held(&st.phase, f, now);
    }
    let ours = matches!(st.phase, Phase::Announced { .. }) && !st.marker.is_empty();
    let marker = if ours {
        st.marker.as_str()
    } else {
        upgrade::READY_PREFIX
    };
    let Some(scan) = upgrade::notice_scan(text, marker) else {
        return false;
    };
    match scan.fate {
        upgrade::NoticeFate::Queued => {
            if scan.queued_until().is_some_and(|until| until > now) {
                f.limited = true;
                st.phase = upgrade::clock_held(&st.phase, f, now);
                return false;
            }
            if f.limited {
                return false;
            }
            let owner_asked = *request == Request::Now
                && scan.typed_at.is_some_and(|typed| st.request_at > typed);
            if scan.queued_copies() > upgrade::REQUEUE_MAX
                && !owner_asked
                && scan.rests_until().is_none_or(|rested| now < rested)
            {
                f.queued = true;
                st.phase = upgrade::clock_held(&st.phase, f, now);
            }
            if ours {
                f.undelivered = true;
            }
            ours
        }
        upgrade::NoticeFate::Taken(Some(at)) if ours => {
            let _ = st.hold_clock(at.min(now));
            false
        }
        _ => false,
    }
}

/// The release, typed if it may be now ([`upgrade::gate_release`]): one line
/// ([`upgrade::release_prompt`]) into the session of an upgrade that abandoned
/// its notice ([`St::release`]), under every fence the notice is typed under
/// (the agent in front of a terminal the tab owns, the conversation's one
/// live owner, the tab still the PTY group's claim, a fresh read with no box
/// or draft whose generation the turn is fenced on) — save that the agent
/// need not be a shell's job: no restart follows the release, so an aterm
/// tab's own program in front is enough ([`leads_terminal`]). Typed:
/// nothing owed, markers forgotten ([`St::released`]), ledgered once
/// (`released:<why>`). Not typed: still owed; `r` is kept when it is an act
/// the host looks on after (`gave-up`, `drain-expired:…`), else the wait says
/// what holds it (`wait:release:<why>`), never a last word ([`owed_word`]).
/// Called only where the release is the next act ([`release_is_next`]). A
/// release no longer this upgrade's ([`release_void`]: the notified process no
/// longer holds the conversation here, or the agent took up later direction)
/// is dropped instead, said once ([`drop_release`]), and `r` kept.
#[allow(clippy::too_many_arguments)]
fn release(
    opts: &Opts,
    r: Report,
    st: &mut St,
    c: &mut Client,
    tab: &str,
    sf: &SessionFile,
    host_claim: Option<&HostClaim>,
    k: &dyn Kernel,
    f: &Facts,
    ready: bool,
    tail: Option<&str>,
) -> Report {
    let owed = owed_word;
    if let Some(why) = release_void(st, sf, tab, tail) {
        drop_release(opts, &r, st, why);
        return r;
    }
    if let upgrade::Gate::Wait(why) = upgrade::gate_release(f, ready) {
        return owed(r, why);
    }
    if let Err(why) = leads_terminal(k.job(sf.pid)) {
        return owed(r, why);
    }
    let owner = k.terminal(sf.pid);
    if !owner
        .as_ref()
        .is_some_and(|(pid, name)| owned_by_aterm((*pid, name.as_str())))
    {
        return owed(r, "terminal");
    }
    if opts.dry_run {
        let why = st.release.clone();
        return if r.is_act() {
            r
        } else {
            said(r, format!("would-release:{why}"))
        };
    }
    if let Err(why) = require_unique_owner(&opts.home, sf) {
        return owed(r, why);
    }
    if host_claim.is_some_and(|claim| !claim_still_live(c, sf.pid, claim)) {
        return owed(r, "tab-ownership-changed");
    }
    let text = upgrade::release_prompt(st.agent, f.quiet);
    match type_fenced(c, tab, &text, None, Put::Pressed, opts.timings.fence_every) {
        Ok(()) => {
            let why = st.release.clone();
            st.released();
            let r = said(r, format!("released:{why}"));
            ledger(
                opts,
                &r,
                if f.quiet {
                    "the upgrade abandoned the notice it gave the agent without restarting it: one \
                     line told it the upgrade is off for now, nothing the notice asked of it \
                     still applies, and to carry on as it would have without it"
                } else {
                    "the upgrade abandoned the notice it gave the agent without restarting it: one \
                     line told it the upgrade is off for now, nothing will restart the session \
                     without asking it again first, nothing the notice asked of it still \
                     applies, and to carry on as it would have without it"
                },
            );
            r
        }
        Err(e) => {
            if matches!(e, Typed::Cut(_)) {
                left_cut(st, c, tab, &text);
            }
            owed(r, &e.word())
        }
    }
}

/// The word of a step that leaves a release owed, `why` what holds it: `r` when
/// it is an act of this step the host parks at the next idle point on
/// (`gave-up`, `drain-expired:…`), else `wait:release:<why>`, which the host
/// looks at again ([`after`]). Never a last word while a release is owed, or
/// the window would never look for it again.
fn owed_word(r: Report, why: &str) -> Report {
    if r.is_act() && after(&r.step, 0) == After::NextIdle {
        r
    } else {
        said(r, format!("wait:release:{why}"))
    }
}

/// A conversation owed a release with no upgrade to take now ([`release`]): its
/// live process runs the build the upgrade stopped short of, the target gone
/// (rolled back, uninstalled). A release no longer this upgrade's
/// ([`release_void`]) is dropped; otherwise one look, and the release if it may
/// be typed. Either way the step is recorded ([`St::note_step`]); a drop
/// records `current`, clearing a stale `wait=release:<gate>`.
#[allow(clippy::too_many_arguments)]
fn release_visit(
    opts: &Opts,
    r: Report,
    sf: &SessionFile,
    mut st: St,
    tab: &str,
    t: &[ProcRow],
    k: &dyn Kernel,
    host_claim: Option<&HostClaim>,
) -> Report {
    let now = now_s();
    let recent = Records::of_file(&opts.home, sf)
        .transcript(&sf.session_id)
        .map(|p| tail_to_end(&p, TAIL_BYTES));
    let tail = recent.as_ref().map(|(text, _)| text.as_str());
    if let Some(why) = release_void(&st, sf, tab, tail) {
        if opts.dry_run {
            return said(r, format!("would-drop-release:{why}"));
        }
        drop_release(opts, &r, &mut st, why);
        let r = said(r, "current");
        st.note_step(&r.step, &sf.status, now);
        save(opts, &sf.session_id, &st);
        return r;
    }
    let (mut c, facts, _, _, _) = match look(opts, sf, &mut st, tab, host_claim, t, now) {
        Ok(look) => look,
        Err(why) => return said(r, format!("wait:release:{why}")),
    };
    // Aterm's own words left in the prompt, first ([`left_first`]): a release
    // the host stopped part-way included.
    if let Some(r) = left_first(opts, &r, &mut st, &mut c, tab, sf, now) {
        return r;
    }
    let r = release(
        opts,
        said(r, "wait:release"),
        &mut st,
        &mut c,
        tab,
        sf,
        host_claim,
        k,
        &facts,
        false,
        tail,
    );
    st.note_step(&r.step, &facts.status, now);
    save(opts, &sf.session_id, &st);
    r
}

/// Why an owed release is no longer this upgrade's to type into `sf` in `tab`:
/// `other-process` (the notified process no longer holds the conversation in
/// its tab, [`St::notice_belongs_to`]) or `directed` (someone spoke to the
/// agent after its last READY to a marker this round typed, [`St::asked`], and
/// it took that up, [`upgrade::directed_since_ready`]: the line would pull it
/// off that). A direction the agent answered with READY, or has not answered
/// yet, drops nothing: the agent holds for the restart and only the release
/// frees it.
fn release_void(st: &St, sf: &SessionFile, tab: &str, tail: Option<&str>) -> Option<&'static str> {
    if !st.notice_belongs_to(sf, tab) {
        return Some("other-process");
    }
    tail.is_some_and(|t| upgrade::directed_since_ready(t, &st.asked))
        .then_some("directed")
}

/// Drops an owed release that is no longer this upgrade's ([`release_void`],
/// `why`): nothing owed, markers forgotten (the round is over as if the line
/// had been typed), ledgered once (`release-dropped:<why>`). A dry run
/// changes nothing.
fn drop_release(opts: &Opts, r: &Report, st: &mut St, why: &str) {
    if opts.dry_run {
        return;
    }
    st.dropped();
    ledger(
        opts,
        &said(r.clone(), format!("release-dropped:{why}")),
        if why == "directed" {
            "someone directed the conversation after the agent's last answer to the upgrade, and \
             the agent took that up: the line telling it to carry on is not typed over that"
        } else {
            "the process the notice reached no longer holds the conversation in its tab: the line \
             is not typed into another"
        },
    );
}

/// What a restart records of the transcript before its signal, from the tail
/// the READY answer was read in ([`tail_to_end`]): the model the agent's last
/// turn named (empty when none did) and where that read ended (`0`: no read),
/// past which the resumed session's first turn is looked for
/// (`relaunch::resumed_model`).
pub(super) fn model_and_mark(recent: Option<&(String, u64)>) -> (String, u64) {
    recent.map_or((String::new(), 0), |(text, end)| {
        (upgrade::transcript_model(text).unwrap_or_default(), *end)
    })
}

/// Restarts a previous sweep left between the exit and the new process: the
/// agent is gone and no live process holds its conversation yet, by the pass's
/// own list (`files`) and by a fresh read of Claude's session files wherever
/// that list says none does ([`registered_since`]).
fn orphans(opts: &Opts, files: &[SessionFile], live_tabs: Option<&[LiveTab]>) -> Vec<Report> {
    let Ok(dir) = std::fs::read_dir(state_dir(opts)) else {
        return Vec::new();
    };
    let fresh = std::cell::OnceCell::new();
    let mut out = Vec::new();
    for e in dir.flatten() {
        let path = e.path();
        if path.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let Some(session) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
            continue;
        };
        let Some(mut st) = load(opts, &session).filter(St::in_flight) else {
            continue;
        };
        // A Codex restart is the Codex lane's to carry on ([`codex`]).
        if !driven_here(st.agent) {
            continue;
        }
        if live_tabs.is_some_and(|tabs| !tab_is_live(tabs, &st.tab)) {
            continue;
        }
        // A live process holding the conversation is `visit`'s; a session file
        // left by a dead owner holds nothing and must not strand the restart.
        if conversation_live(files, &session) {
            continue; // `visit` has it.
        }
        if opts.only_sid.as_ref().is_some_and(|s| *s != st.tab) {
            continue;
        }
        // Read where the record says its agent keeps its records ([`St::home`]).
        let held = if st.home.is_some() {
            st.agent_records(&opts.home)
                .session_files()
                .is_some_and(|files| conversation_live(&files, &session))
        } else {
            registered_since(&fresh, &opts.home, &session)
        };
        if held {
            continue; // `visit` has it, or the loop's `resume`.
        }
        let r = Report {
            pid: st.pid,
            tab: st.tab.clone(),
            session: session.clone(),
            from: st.from.clone(),
            to: format!("{}({})", st.to, st.source),
            step: String::new(),
        };
        // The agent is still exiting (a SIGTERMed Claude Code that removed its
        // session file and hung): never a harder signal, nothing acted on;
        // past the bound it is noted once in the ledger.
        if alive(st.pid) {
            note_stuck(opts, &r, &mut st, &session, now_s());
            continue;
        }
        // A hand the line waits on, standing on the tab now, is waited out
        // before the clock is read (`relaunch::hand_waits_out`, LC-7 of the
        // 2026-10-15 sweep): nothing attempted, uncounted, the clock stopped
        // — the only carrier of a tab no host carries.
        if hand_waits_out(opts, &mut st, &session, alive) {
            if st.seen_gone(now_s()) {
                save(opts, &session, &st);
            }
            out.push(said(r, WAIT_HAND_LIFTS));
            continue;
        }
        // Expiry is decided on the exit as an earlier look saw it (the host's
        // carry, the relaunch's wait), else on the signal: this pass's first
        // sighting may come long after the exit (`relaunch::STALE_S`).
        let stale = expired(&st, now_s());
        // Gone: the tab's record from here, stamped before the expiry is acted
        // on and before a socket is dialled.
        if !opts.dry_run && st.seen_gone(now_s()) {
            save(opts, &session, &st);
        }
        if let Some((why, detail)) = stale {
            if opts.dry_run {
                out.push(said(r, format!("would-fail:{why}")));
                continue;
            }
            st.stop(why, now_s());
            let r = said(r, format!("failed:{why}"));
            ledger(opts, &r, &detail);
            save(opts, &session, &st);
            out.push(r);
            continue;
        }
        if opts.dry_run {
            out.push(said(r, format!("would-resume:{}", st.phase.word())));
            continue;
        }
        let tab = st.tab.clone();
        let Ok(mut c) = connect(opts, &tab) else {
            out.push(said(r, "wait:no-socket"));
            continue;
        };
        if !roster(&mut c).contains(&tab) {
            out.push(said(r, "wait:tab-not-live"));
            continue;
        }
        let r = match st.phase {
            Phase::Exiting { .. } => relaunch(opts, r, &mut st, &mut c, &session, &Live),
            _ => await_new(opts, r, &mut st, &mut c, &session, &Live),
        };
        save(opts, &session, &st);
        out.push(r);
    }
    out
}

/// Whether a live process holds `session` now, by one fresh read per pass
/// (`fresh`), taken only for a restart the pass's list shows unheld:
/// [`claude_pass`] reads the session files once, before its visits act, and a
/// visit that relaunches its agent registers the new process after that list,
/// so from the list alone the orphan pass would carry the restart again. A read
/// that fails decides nothing, and the list stands.
fn registered_since(
    fresh: &std::cell::OnceCell<Option<Vec<SessionFile>>>,
    home: &Path,
    session: &str,
) -> bool {
    fresh
        .get_or_init(|| session_files(home))
        .as_deref()
        .is_some_and(|files| conversation_live(files, session))
}

/// Whether the agent has answered a READY marker of this round ([`St::marker`],
/// [`St::markers`]) in `tail`, as its last word after the latest notice
/// ([`upgrade::transcript_has_ready`]). No marker (none typed yet, or voided by
/// [`drain_expired`] or forgotten by a release) is no answer.
fn answered(st: &St, tail: Option<&str>) -> bool {
    let Some(text) = tail else {
        return false;
    };
    st.round_markers()
        .any(|m| upgrade::transcript_has_ready(text, m))
}

/// The two READY facts the transcript decides, once the visit has read `tail`
/// and whether the upgrade hears a READY there (`ready`, [`heard`]); both only
/// for a phase that acts on one, from the process and tab its notice reached
/// ([`St::hears_ready`], [`St::notice_belongs_to`]).
///
/// * [`upgrade::Facts::ready_unrecorded`]: kept from the look where the
///   screen's last words answer READY to this round and the transcript, read,
///   does not yet. An unreadable transcript holds nothing.
/// * [`upgrade::Facts::ready_superseded`]: an announced round's READY the
///   agent was directed on from since ([`upgrade::ready_superseded`]).
fn ready_facts(
    st: &St,
    sf: &SessionFile,
    tab: &str,
    tail: Option<&str>,
    ready: bool,
    f: &mut Facts,
) {
    let hearing = !ready && st.hears_ready() && st.notice_belongs_to(sf, tab);
    f.ready_unrecorded &= hearing && tail.is_some();
    f.ready_superseded = hearing
        && matches!(st.phase, Phase::Announced { .. })
        && tail.is_some_and(|t| {
            let markers: Vec<String> = st.round_markers().cloned().collect();
            upgrade::ready_superseded(t, &markers)
        });
}

/// The READY the upgrade will act on, given to the reducer
/// ([`upgrade::next_step`]) and holding the release back
/// ([`upgrade::gate_release`]): the agent [`answered`] a marker of this round,
/// in a phase that acts on one ([`St::hears_ready`]), from the very process
/// and tab its notice reached ([`St::notice_belongs_to`]); resumed elsewhere,
/// the answer is no consent. A READY no phase acts on is none, so it cannot
/// hold the release for ever.
fn heard(st: &St, sf: &SessionFile, tab: &str, tail: Option<&str>) -> bool {
    st.hears_ready() && st.notice_belongs_to(sf, tab) && answered(st, tail)
}

/// Whether, after this visit's `step` said `word`, the owed release is the
/// upgrade's next act ([`release`]): it stopped asking or voided the READY
/// (`gave-up`, `drain-expired`), stopped in this very step (`failed:…`,
/// `refused:…`), or waits on nothing it would type or signal
/// ([`upgrade::release_is_next`]). A wait of the notice's or restart's own
/// gate is not: what it waits for goes first ([`owns_turn_ends`]).
fn release_is_next(step: &Step, word: &str) -> bool {
    match step {
        Step::GiveUp | Step::Void(_) => true,
        Step::Wait(why) => upgrade::release_is_next(why),
        Step::Announce | Step::Terminate | Step::Fresh => {
            word.starts_with("failed:") || word.starts_with("refused:")
        }
        // A new round: its first notice supersedes a release still owed.
        Step::Rearm => false,
    }
}

/// Whether the visit tries the owed release now ([`release`]): one is owed
/// ([`St::release`]) and it is the next act ([`release_is_next`]); or, at a
/// break, an announced upgrade with no READY ([`unanswered_at_break`]), whose
/// notice's window a void's release goes before; or a quiet pending round its
/// agent's own work holds ([`quiet_release_due`]). `ready` is the READY the
/// step was taken on ([`heard`]).
fn release_next(step: &Step, word: &str, st: &St, f: &Facts, ready: bool) -> bool {
    !st.release.is_empty()
        && (release_is_next(step, word)
            || (*step == Step::Wait("background") && unanswered_at_break(st, f, ready))
            || quiet_release_due(step, st, f))
}

/// A quiet pending round that owes its agent a release, its restart held by the
/// agent's own work: a notice reached the agent once (typed by an older aterm,
/// or under `upgrade_ask`) and its round was abandoned since (a retarget, a
/// re-armed give-up, the owner's word run out). The quiet flow types no new
/// notice to supersede the release, and its restart waits on work that may
/// never end ([`upgrade::own_work_holds`]), so the release is the round's next
/// act there. Any other wait (the settle, a person, a limit) is the restart's
/// to wait out.
fn quiet_release_due(step: &Step, st: &St, f: &Facts) -> bool {
    f.quiet
        && st.phase == Phase::Pending
        && matches!(step, Step::Wait(w) if upgrade::own_work_holds(w))
}

/// A break of the agent's own work ([`upgrade::Facts::background_point`])
/// with a notice standing unanswered: announced, and no READY the step was
/// taken on.
fn unanswered_at_break(st: &St, f: &Facts, ready: bool) -> bool {
    f.background_point && !ready && matches!(st.phase, Phase::Announced { .. })
}

/// A new round of a stopped upgrade ([`upgrade::Step::Rearm`]) at `now`: the
/// record re-armed ([`St::rearm`]) and ledgered once as `rearmed:<why>` (why
/// it stopped, how long it rested). Both lanes take it here. Nothing is typed
/// or signalled; the new round's first notice is the next look's. A dry run
/// only says it (`would-rearm:<why>`).
pub(super) fn rearm(opts: &Opts, r: Report, st: &mut St, now: u64) -> Report {
    let stamped = st.failed_at != 0;
    let rested = st.failed_for(now);
    if opts.dry_run {
        let why = match &st.phase {
            Phase::Failed(why) => why.clone(),
            other => other.word(),
        };
        return said(r, format!("would-rearm:{why}"));
    }
    let why = st.rearm(now);
    let r = said(r, format!("rearmed:{why}"));
    let when = if stamped {
        format!("{} ago", upgrade::span(rested))
    } else {
        "before its time was recorded".to_string()
    };
    ledger(
        opts,
        &r,
        &format!(
            "no stop is for good: the round that stopped ({why}) {when} has rested its {}, and \
             a new round starts — new READY markers, its asks reset; its first notice is typed \
             at the next point every gate lets one go",
            upgrade::span(upgrade::RETRY_S)
        ),
    );
    r
}

/// The drain's bound ([`upgrade::DRAIN_S`]) passed with the restart gate still
/// held: by a person's box or draft for [`upgrade::HOLD_S`] (`why`,
/// [`upgrade::person_hold`]), by the agent's background work (`background`), or
/// by Claude's status claiming work over an idle screen (`status-stale`). The
/// READY is void ([`St::void`]): the markers are forgotten so the old answer
/// can never end the agent later, the agent is owed its release, and the ledger
/// says why once. An announced upgrade stays announced with its clock restarted
/// (re-asked after [`upgrade::REASK_S`], bounded by [`upgrade::MAX_ASKS`]); a
/// given-up one rests again ([`upgrade::RETRY_S`]). Nothing is signalled. A dry
/// run only says it.
fn drain_expired(opts: &Opts, r: Report, st: &mut St, why: &str, f: &Facts, now: u64) -> Report {
    if opts.dry_run {
        return said(r, format!("would-void:{why}"));
    }
    let asks_again = matches!(st.phase, Phase::Announced { .. });
    st.void(now);
    let r = said(r, format!("drain-expired:{why}"));
    let (what, held_s) = match why {
        "box" => ("a box nobody answered", f.hold_s),
        "draft" => ("a draft nobody sent", f.hold_s),
        // Claude's own `busy`/`shell` over an idle screen: the restart
        // never takes it for idle ([`upgrade::gate_restart`]).
        "status-stale" => (
            "Claude Code's own status, never back to idle over an idle screen,",
            f.ready_s,
        ),
        _ => ("work of the agent's own still running", f.ready_s),
    };
    // The release is owed here, and typed only where the step may type it
    // ([`release`]); a lane that types none (Codex) says nothing of one.
    let owed = if st.release.is_empty() {
        ""
    } else {
        "the agent is owed its release, and "
    };
    ledger(
        opts,
        &r,
        &format!(
            "the READY answer is void: {} min after the announcement {what} had held the \
             restart for {} min; {owed}{}",
            upgrade::DRAIN_S / 60,
            held_s / 60,
            if asks_again {
                "it is asked again later".to_string()
            } else {
                format!(
                    "a new round asks it again in {}",
                    upgrade::span(upgrade::RETRY_S)
                )
            }
        ),
    );
    r
}

/// A round that asked its agent whose build fails to start
/// ([`upgrade::build_hold`]): stopped at `now` (`failed:build-fails`), its
/// markers forgotten so no READY to them restarts the agent later, and the
/// agent owed its release ([`release`]). The round rests as any stop does
/// ([`upgrade::RETRY_S`]); the next asks nothing until a probe proves the build
/// starts or a newer one is the target ([`BUILD_FAILS`]). A dry run only says
/// it.
fn build_held(opts: &Opts, r: Report, st: &mut St, now: u64) -> Report {
    if opts.dry_run {
        return said(r, format!("would-fail:{BUILD_FAILS}"));
    }
    st.stop(BUILD_FAILS, now);
    let r = said(r, format!("failed:{BUILD_FAILS}"));
    ledger(
        opts,
        &r,
        "the build this round would move the agent onto failed to start and is not proved to \
         start since, so no move comes: the round stops, the agent it asked is owed its \
         release, and the next round asks nothing until the build is proved to start or a \
         newer one is the target",
    );
    r
}

/// Records that the agent is no job of a job-control shell (`job`, a
/// [`not_a_shell_job`] read): the upgrade stops for this (session, target),
/// ledgered once. An aterm tab's own program an older build announced to
/// (one that read it as the foreground job of a "shell" that was the aterm)
/// keeps the release its round owes ([`St::stop`]): typed where the tab's
/// own program leads its terminal ([`leads_terminal`]), it tells the agent
/// the notice no longer stands. An agent no job-control shell started is
/// owed none: it never leads its terminal, so the line could never be typed.
fn not_a_job(opts: &Opts, r: Report, st: &mut St, job: Job) -> Report {
    if opts.dry_run {
        return said(r, "would-refuse:not-a-shell-job");
    }
    st.stop("not-a-shell-job", now_s());
    if job == Job::NoJobControl {
        st.release.clear();
    }
    let r = said(r, "refused:not-a-shell-job");
    ledger(
        opts,
        &r,
        "the agent is no job of an interactive shell: nothing would take the terminal back to relaunch it",
    );
    r
}

/// The agent's terminal is not the tab's ([`owned_by_aterm`]): typing into the
/// tab would not reach it, so the session is not moved. A wait (it may run in
/// the tab next time), said once per (session, target, reason) in the ledger
/// and as an act the host logs. A dry run only says the wait.
fn held_back(opts: &Opts, r: Report, st: &mut St, session: &str, why: &str) -> Report {
    if opts.dry_run || st.noted == why {
        return said(r, format!("wait:{why}"));
    }
    st.noted = why.to_string();
    let r = said(r, format!("held-back:{why}"));
    ledger(
        opts,
        &r,
        "the agent's terminal is not the tab's (a multiplexer pane, `script`, ssh): nothing typed into the tab would reach it, so it is not moved",
    );
    save(opts, session, st);
    r
}

/// A restart under way that has not moved ([`St::stuck`]: `exiting`, `exited`
/// or `relaunched` past [`STALE_S`]), noted once in the ledger as
/// `stuck:<what>`: a Claude Code hung in shutdown, or a relaunched agent its
/// carry-on never finds idle, waits unbounded, and the ledger is where the
/// owner diagnoses it. Keyed on [`St::noted`] like [`held_back`]. A note only,
/// never a failure (which would leave a later-exiting agent un-relaunched), a
/// signal or typing. Claude Code upgrade records only (Codex ledgers its own),
/// never from `--status`; a dry run writes nothing.
pub(super) fn note_stuck(opts: &Opts, r: &Report, st: &mut St, session: &str, now: u64) {
    if opts.dry_run || !driven_here(st.agent) || !st.is_upgrade() {
        return;
    }
    let Some(what) = st.stuck(now) else {
        return;
    };
    let (Phase::Exiting { at_s } | Phase::Relaunched { at_s }) = st.phase else {
        return;
    };
    let key = format!("stuck:{what}");
    if st.noted == key {
        return;
    }
    st.noted.clone_from(&key);
    ledger(
        opts,
        &said(r.clone(), key),
        &format!(
            "under way for {}, past its {} bound: {}",
            upgrade::span(now.saturating_sub(at_s)),
            upgrade::span(STALE_S),
            upgrade_status::stuck_words(st.agent, what)
        ),
    );
    save(opts, session, st);
}

/// The tab is attended ([`upgrade::Facts::attended`]) and `--now` is not in
/// force: the upgrade waits for the person to step away. Said once per
/// upgrade like [`held_back`], in the ledger and as an act the host logs
/// (`held-back:attended`), since the host notes only acts; later steps say
/// `wait:attended`. A dry run only says the wait.
fn attended_once(opts: &Opts, r: Report, st: &mut St) -> Report {
    const WHY: &str = "attended";
    if opts.dry_run || st.noted == WHY {
        return said(r, format!("wait:{WHY}"));
    }
    st.noted = WHY.to_string();
    let r = said(r, format!("held-back:{WHY}"));
    ledger(
        opts,
        &r,
        "a person is at this tab (a person gave this session input within `[harness] \
         human_grace_s`, by aterm's per-session stamp `status human_ms=`): nothing is typed into \
         it on the upgrade's own judgment until they step away, or the owner's `aterm harness \
         upgrade <sid> --now`",
    );
    r
}
/// The announcement, for a restart that can be carried out. `planned` is
/// [`plan`]'s answer, asked first: a notice for a launch the signal would then
/// refuse interrupts the agent for nothing. `send` types the notice as the
/// `asks`-th ([`upgrade::announce_asks`]) and answers its READY marker;
/// `again` is why it is typed again (a notice the login wall answered, a
/// give-up taken back), ledgered beside the marker.
#[allow(clippy::too_many_arguments)]
fn announce(
    opts: &Opts,
    r: Report,
    st: &mut St,
    planned: Result<Plan, NoPlan>,
    now: u64,
    asks: u32,
    again: Option<&str>,
    send: impl FnOnce(u32) -> Result<String, String>,
) -> Report {
    if let Err(no) = planned {
        return unplanned(opts, r, st, no);
    }
    if opts.dry_run {
        return said(r, "would-announce");
    }
    match send(asks) {
        Ok(marker) => {
            st.announced(marker, now, asks);
            let r = said(r, format!("announced:{asks}"));
            let detail = match again {
                Some(why) => format!("{} ({why})", st.marker),
                None => st.marker.clone(),
            };
            ledger(opts, &r, &detail);
            r
        }
        // A turn found running at the fence's read: the gate's own wait, the
        // notice never tried ([`claude_fence`]).
        Err(e) if e == TURN_RUNNING => said(r, format!("wait:{TURN_RUNNING}")),
        Err(e) => said(r, format!("wait:announce-refused:{}", first_word(&e))),
    }
}

/// What the upgrade's relaunch line is made from: the live session's
/// conversation and directory, its argv, the target build, the model it moves
/// to, the permission mode it last ran in, whether it is resumed (`false`: a
/// conversation nobody asked anything, started afresh), the Claude home and
/// login its process runs under, and whether that process authenticates by its
/// stored login ([`Launch::stored_login`]).
#[allow(clippy::too_many_arguments)]
fn launch_of<'a>(
    sf: &'a SessionFile,
    argv: &'a [String],
    target: &'a Candidate,
    model: Option<&'a str>,
    mode: Option<&'a str>,
    resume: bool,
    home: Option<&'a str>,
    login: Option<&'a str>,
    stored_login: bool,
) -> Launch<'a> {
    Launch {
        session: &sf.session_id,
        resume,
        cwd: &sf.cwd,
        agent: sf.pid,
        argv,
        exe: &target.exe,
        model,
        mode,
        home,
        homedir: None,
        login,
        stored_login,
        billing: crate::harness::relaunch::ModelBilling::Keep,
    }
}

/// THE `HOME` WHOSE `.claude` IS `dir`: what an agent with no
/// `CLAUDE_CONFIG_DIR` keeps its records under ([`claude_home_dir`]: lexically
/// `<HOME>/.claude`), so a line carrying it starts the agent under the same
/// home. `None` for a directory no `HOME` names that way (another name, a
/// relative path, no parent, not UTF-8).
pub(crate) fn homedir_holding(dir: &Path) -> Option<String> {
    let parent = dir
        .parent()
        .filter(|_| dir.file_name().is_some_and(|n| n == ".claude"))?;
    parent
        .is_absolute()
        .then(|| parent.to_str().map(str::to_owned))
        .flatten()
}

/// Whether the exec environment `env` (`KEY=value`, as the kernel hands it
/// back) authenticates by its stored login
/// ([`super::accounts::ProcessLogin::of_exec_env`]); an environment that reads
/// empty was not read, and answers `false`.
fn stored_login_of(env: &[String]) -> bool {
    super::accounts::ProcessLogin::of_exec_env(env).is_some_and(|who| who.stored_login)
}

/// Whether the line starts the agent under the Claude home its records are in.
/// A relative `CLAUDE_CONFIG_DIR` (set and empty included; `home` is the raw
/// value) resolves against the process's directory ([`home_dir_at`], as Claude
/// Code does), and the line starts the new process in `cwd`
/// ([`super::relaunch::plan`]; empty: where the agent runs now). Where that
/// names another directory than `records` (the sweep's [`Homes`], the relaunch
/// snapshot's [`Records::of_snapshot`]), the relaunched Claude Code would keep
/// its conversation where nothing finds it again: refused (`refused:home`). An
/// absolute home is carried as it is. Every planner asks it: the upgrade's
/// restart, the restart in place and the relaunch on exit.
///
/// AN UNSET ONE UNDER ANOTHER `HOME` THAN THE USER'S (`user_home`): Claude
/// Code takes `homedir()`, the process's own `HOME`, and keeps its home at
/// `<HOME>/.claude` ([`claude_home_dir`]), where the one resolver reads such
/// an agent's records (round eighteen). The line CARRIES that `HOME` — the
/// directory that holds the records' `.claude`, its answer `Some(HOME)` —
/// so the relaunched process looks for its conversation, and files its
/// record, where the one it replaces did (LC-4: refused
/// `refused:home` before, its remedy a `claude --resume` the tab's shell
/// could not run). Records under another directory than a `.claude` are
/// refused the same way: no `HOME` names them. `Ok(None)`: the line carries
/// no `HOME`, as every line for an agent under the user's own home.
pub(super) fn home_carried(
    user_home: &Path,
    home: Option<&str>,
    cwd: &str,
    records: &Records,
) -> Result<Option<String>, NoPlan> {
    let Some(home) = home else {
        return match records.dir() {
            Some(dir) if !same_dir(dir, &user_home.join(".claude")) => homedir_holding(dir)
                .map(Some)
                .ok_or_else(|| NoPlan::Refused {
                    why: "line:home".to_string(),
                    what: "home".to_string(),
                    detail: format!(
                        "it runs under another HOME than its shell's (its records are in {}), \
                         and no HOME the relaunch line could carry names them",
                        dir.display()
                    ),
                }),
            _ => Ok(None),
        };
    };
    if Path::new(home).is_absolute() {
        return Ok(None);
    }
    if cwd.is_empty() {
        return Ok(None);
    }
    let at = home_dir_at(home, || Some(PathBuf::from(cwd)));
    if at.is_some() && at.as_deref() == records.dir() {
        return Ok(None);
    }
    let named = if home.is_empty() {
        "is set and empty, which names the directory it runs in".to_string()
    } else {
        format!("({home}) is relative")
    };
    Err(NoPlan::Refused {
        why: "line:home".to_string(),
        what: "home".to_string(),
        detail: format!(
            "its CLAUDE_CONFIG_DIR {named}, and the relaunch line would start it in {cwd}, under \
             another Claude home than the one its records are in"
        ),
    })
}

/// The last look before typing into Claude's composer: a fresh read with an
/// empty composer and no box, and that read's generation, which the typed turn
/// is fenced on ([`turn_fenced`]).
///
/// The server checks the generation after its `yield=0.2` park and types
/// nothing if any output came since, so nothing of ours may sit between the
/// read and the `turn`: the `help turn` probe is asked first
/// ([`turn_takes_gen`]). A read that needs the column-2 `cell`
/// ([`composer_empty`]) asks it and reads once more, fenced on that read if its
/// composer row and cursor match what the `cell` judged, else `changed`. The
/// narrower rows fence is [`claude_fence`].
pub(super) fn typing_fence(c: &mut Client, tab: &str) -> Result<Option<String>, &'static str> {
    typing_fence_read(c, tab).map(|(generation, _)| generation)
}

/// [`typing_fence`] plus the read its generation names, which a later notice
/// try is judged on ([`retry_open`]), so no read sits between that judgment
/// and the paste's fence.
fn typing_fence_read(c: &mut Client, tab: &str) -> Result<(Option<String>, Screen), &'static str> {
    let _ = turn_takes_gen(c);
    let Some(first) = screen(c, tab) else {
        return Err("screen-unreadable");
    };
    let Some(dim_at) = dim_cell(&first) else {
        return fence_verdict(&first, false).map(|generation| (generation, first));
    };
    let dim = c
        .request_line(&format!("@{tab} cell {dim_at} 2"))
        .is_ok_and(|line| cell_is_dim(&line));
    let Some(scr) = screen(c, tab) else {
        return Err("screen-unreadable");
    };
    if !same_composer(&first, &scr) {
        return Err("changed");
    }
    fence_verdict(&scr, dim).map(|generation| (generation, scr))
}

/// The prefix [`claude_fence`] marks its narrow fence with, in the string a
/// generation otherwise rides in ([`turn_fenced`] reads it back).
const ROWS_FENCE: &str = "rows:";

/// [`claude_fence`]'s word for a read that shows a turn running: the gate's own
/// `busy`, not a refusal of the fence ([`announce`] says `wait:busy`; the
/// release `wait:release:busy`).
const TURN_RUNNING: &str = "busy";

/// [`typing_fence_read`] for Claude Code's input box: the same read and
/// verdict, fenced on the box's three rows ([`composer_rows`]) where the host
/// takes `turn if-rows=`, not on the whole screen's generation. `if-gen=`
/// refuses on any output since the read, and Claude Code repaints itself (its
/// spinner, a workflow's timer), so a notice would be refused at every look;
/// what the fence guards against lands in the box (a person's keystroke, a box
/// drawn over it). A box not of that shape, or a host without the fence, keeps
/// the generation. The help answer is the one [`typing_fence_read`] cached.
///
/// The rows fence sees nothing outside the box, so a turn begun after the look
/// (a person's Enter, a background task waking the agent) would take the paste:
/// the read is held to the look's own turn fact ([`busy_footer`]),
/// [`TURN_RUNNING`].
fn claude_fence(c: &mut Client, tab: &str) -> Result<(Option<String>, Screen), &'static str> {
    let (generation, read) = typing_fence_read(c, tab)?;
    if busy_footer(&read.rows) {
        return Err(TURN_RUNNING);
    }
    let narrow = composer_rows(&read)
        .filter(|_| turn_takes_rows(c))
        .map(|fence| format!("{ROWS_FENCE}{fence}"));
    Ok((narrow.or(generation), read))
}

/// Claude Code's input box in one read, as a rows fence: the caret row and the
/// two rules around it, in grid rows, hashed by the definition the host shares
/// ([`aterm_types::control_verbs::RowsFence::hash_rows`]). `None` when the
/// cursor's row is not the caret row of the frame
/// [`aterm_phase::phase::composer_rules`] reads (another program's composer, a
/// dialog, a short read). A labelled top rule (`──── xhigh ─`) is hashed too:
/// it changes only when a person changes a setting, and the fence then rightly
/// refuses.
fn composer_rows(scr: &Screen) -> Option<aterm_types::control_verbs::RowsFence> {
    let (caret, _) = scr.cursor?;
    let above = caret.checked_sub(1)?;
    let below = caret.checked_add(1)?;
    if aterm_phase::phase::composer_rules(&scr.rows) != Some((above, below)) {
        return None;
    }
    let rows = scr.rows.get(above..=below)?;
    Some(aterm_types::control_verbs::RowsFence {
        first: scr.first.checked_add(above)?,
        last: scr.first.checked_add(below)?,
        hash: aterm_types::control_verbs::RowsFence::hash_rows(rows.iter().map(String::as_str)),
    })
}

/// When a person last gave the tab input, as one read stamped it, on this
/// process's clock ([`HumanInput`] is an age at the read), compared by a later
/// notice try ([`retry_open`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HandAt {
    /// The host sent no stamp: nothing can be proved of a person's hand.
    Unknown,
    /// No person since the host began serving the session.
    Never,
    /// About then.
    At(Instant),
}

impl HandAt {
    /// The stamp of a read taken just now.
    pub(super) fn of(human: HumanInput) -> HandAt {
        match human {
            HumanInput::Unknown => HandAt::Unknown,
            HumanInput::Never | HumanInput::NeverFor(_) => HandAt::Never,
            HumanInput::Ago(ms) => Instant::now()
                .checked_sub(Duration::from_millis(ms))
                .map_or(HandAt::Unknown, HandAt::At),
        }
    }

    /// Whether `later` (a later read's stamp) proves no person gave input
    /// since this one: the same keystroke within [`HAND_SLACK`] (two ages off
    /// one clock at two replies), or still nobody. An unknown stamp on either
    /// side fails closed.
    pub(super) fn still(self, later: HandAt) -> bool {
        match (self, later) {
            (_, HandAt::Never) => true,
            (HandAt::At(then), HandAt::At(now)) => now <= then + HAND_SLACK,
            _ => false,
        }
    }
}

/// How far apart two reads may place the same keystroke ([`HandAt::still`]):
/// each age is the server's at its read, excluding the reply's trip back.
const HAND_SLACK: Duration = Duration::from_millis(500);

/// What a notice try after the first ([`NOTICE_FENCE_TRIES`]) is judged
/// against ([`retry_open`]): the look's person stamp, whether `--now` waives a
/// person at the tab, whether the look was at a background-work break, and
/// the person's grace at the look's rung ([`upgrade::person_grace`]).
pub(super) struct Retry {
    pub(super) looked: HandAt,
    pub(super) owner_now: bool,
    pub(super) at_break: bool,
    pub(super) grace_s: u32,
}

/// Whether a notice its fence refused as `changed` may be typed again in the
/// same visit: `read`, the fenced read, still meets the look's gates. No
/// person's hand since the look (a message submitted in between would take the
/// notice into its turn), none within the grace unless `--now` waives it, no
/// running turn ([`busy_footer`]), and an idle point ([`reads_idle`]), which a
/// background-work break skips: there the reader reads busy by definition.
pub(super) fn retry_open(read: &Screen, retry: &Retry) -> bool {
    let hand = HandAt::of(read.human);
    if !retry.looked.still(hand) {
        return false;
    }
    if !retry.owner_now && upgrade::attended_by(read.human, retry.grace_s) {
        return false;
    }
    if busy_footer(&read.rows) {
        return false;
    }
    if retry.at_break {
        return true;
    }
    reads_idle(read)
}

/// Whether Claude Code's own reader reads `read` at an idle point: idle,
/// authoritatively, no wall, no box ([`ladder_look`]'s `idle_read`).
fn reads_idle(read: &Screen) -> bool {
    let reading = aterm_phase::read_at(
        Some("claude"),
        &read.rows,
        read.cursor.map(|(row, _)| row),
        read.cursor.map(|(_, col)| col),
    );
    reading.phase == aterm_phase::Phase::Idle
        && reading.phase_authoritative
        && reading.wall.is_none()
        && reading.prompt.is_none()
}

/// Whether the restart's last look (`read`) shows the turn over, where the step
/// read past a lagging status (`lagging`, [`Restarting::lagging`]): no busy
/// footer ([`busy_footer`]) and Claude's own reader at an idle point
/// ([`reads_idle`]). Claude Code need not restamp a status it does not change
/// (`statusUpdatedAt` stood still through each wait measured,
/// docs/measured/claude-code-dialogs-2026-09-24.md), so a turn begun over a
/// stale `busy` shows only on the screen. Not lagging: Claude's `idle` decides.
fn turn_over_shown(read: &Screen, lagging: bool) -> bool {
    !lagging || (!busy_footer(&read.rows) && reads_idle(read))
}

/// One read's word on the composer, no fence: no box up and the composer empty
/// ([`composer_empty`]). For a line typed unfenced (a continuation whose screen
/// never held still, `relaunch::type_continuation`), where no generation is
/// carried.
pub(super) fn composer_clear(c: &mut Client, tab: &str) -> bool {
    screen(c, tab).is_some_and(|scr| {
        aterm_phase::prompt::prompt_box_span(&scr.rows).is_none() && composer_empty(c, tab, &scr)
    })
}

/// [`typing_fence`]'s verdict over one read, `dim` the `cell` answer for its
/// caret row (`false`: not asked, or not dim).
fn fence_verdict(scr: &Screen, dim: bool) -> Result<Option<String>, &'static str> {
    if aterm_phase::prompt::prompt_box_span(&scr.rows).is_some() {
        return Err("box");
    }
    if !upgrade::composer_is_empty(&scr.rows, scr.cursor, dim) {
        return Err("draft");
    }
    Ok(scr.generation.clone())
}

/// The absolute row whose column 2 [`composer_empty`] asks the `cell` of: the
/// composer's caret row when a read can change the answer
/// ([`upgrade::composer_dim_row`]), not the cursor's, which a net-zero re-grid
/// can part from the composer.
fn dim_cell(scr: &Screen) -> Option<usize> {
    upgrade::composer_dim_row(&scr.rows, scr.cursor).and_then(|caret| scr.first.checked_add(caret))
}

/// Whether two reads show the same composer — the cursor at the same place,
/// and the same text on the composer's caret row — so a `cell` asked between
/// them judged the later one's caret row too. The rows above it (a spinner, a
/// timer) may have moved.
fn same_composer(a: &Screen, b: &Screen) -> bool {
    fn caret(s: &Screen) -> Option<&str> {
        aterm_phase::phase::composer_draft(&s.rows)
            .and_then(|(caret, _)| s.rows.get(caret))
            .map(String::as_str)
    }
    a.cursor == b.cursor && a.first == b.first && caret(a) == caret(b)
}

/// Types a line of the harness's own into the agent's composer (the notice, the
/// release) under the harness's hand, tried again when the screen moved.
///
/// The hand ([`Hand`], a hard drive lease under the supervisor loops' holder) is
/// what the window's presence layer recognises as aterm's own, so the tab shows
/// no `Level::Driven` row: an unleased `turn` raises one and folds it when the
/// lease drops, two net-zero PTY resizes that desync the tab's program and
/// invalidate the fence. Best-effort until the render-integrity plan's R02 and
/// R15 land. A tab another driver holds (`Hand::take` refused) is a wait
/// (`held`); a server without hard leases holds nothing.
///
/// Each try is a fresh read ([`claude_fence`]) and a fenced turn
/// ([`turn_fenced`], [`notice_fenced`]: `put` says whether the words are pasted whole or pressed
/// key by key, [`Put`]). With a `retry` (the notice: the look's stamp and gates),
/// only the fence's own `changed` is tried again, up to [`NOTICE_FENCE_TRIES`]
/// times `fence_every` apart ([`super::Timings::fence_every`]), and a try after the first types only
/// where [`retry_open`] finds the read it is fenced on still what the look
/// required; a box, a draft or a person's typing is the read's answer, and a
/// yield that timed out means a person is typing. Without one (the release,
/// which never retried) it is one try under the hand. `Err` is the last try's
/// verdict, whose [`Typed::word`] is the report word; a [`Typed::Cut`] is the
/// caller's to record ([`left_cut`]).
pub(super) fn type_fenced(
    c: &mut Client,
    tab: &str,
    text: &str,
    retry: Option<&Retry>,
    put: Put,
    fence_every: Duration,
) -> Result<(), Typed> {
    let mut hand = Hand::take(c, tab).ok_or_else(|| Typed::Refused("held".to_string()))?;
    let out = (|| {
        let mut tries = 0;
        loop {
            tries += 1;
            let again = retry.is_some() && tries < NOTICE_FENCE_TRIES;
            let (generation, read) = match claude_fence(c, tab) {
                Ok(fenced) => fenced,
                Err("changed") if again => {
                    std::thread::sleep(fence_every);
                    continue;
                }
                Err(why) => return Err(Typed::Refused(why.to_string())),
            };
            if tries > 1 && !retry.is_some_and(|retry| retry_open(&read, retry)) {
                return Err(Typed::Changed);
            }
            // A one-key card over the composer (the session survey) is the
            // person's: no key of a pressed text answers it, but its first key
            // closes it unanswered. A pressed text waits (`survey`) and leaves
            // the card to the person (`aterm_phase::survey::survey_block`).
            if put == Put::Pressed && aterm_phase::survey::survey_block(&read.rows).is_some() {
                return Err(Typed::Refused("survey".to_string()));
            }
            let typed = match put {
                Put::Paste => turn_fenced(c, tab, text, generation.as_deref()),
                Put::Pressed => notice_fenced(c, tab, text, generation.as_deref()),
            };
            match typed {
                Ok(()) => return Ok(()),
                Err(Typed::Changed) if again => std::thread::sleep(fence_every),
                Err(e) => return Err(e),
            }
        }
    })();
    hand.give_back(c);
    out
}

/// What one [`restart`] is made of beyond the session's own facts: the launch's
/// argv, the build it moves to, the tab's claim, the person's grace at the
/// signal ([`upgrade::person_grace`] of the rung in force), whether it starts
/// the agent afresh ([`Step::Fresh`]: no task, nothing resumed or typed after),
/// the permission mode read off the tab's pill ([`screen_mode`]; `None`: none
/// drawn), and the model a person chose by hand ([`hand_model_in`]), asked for
/// when the list moves the conversation to none.
struct Restarting<'a> {
    argv: &'a [String],
    target: &'a Candidate,
    host_claim: Option<&'a HostClaim>,
    grace: u32,
    fresh: bool,
    /// A quiet restart ([`upgrade::Facts::quiet`]): no notice was typed, so
    /// none is the process's to own — the process looked at is the one ended.
    quiet: bool,
    /// The step read past a status that only lags, on the owner's `--now` under
    /// the quiet flow ([`upgrade::quiet_step`]): the last look takes that same
    /// status, unrewritten, for `idle`, but only on a screen that still shows
    /// the turn over ([`turn_over_shown`]). `false`: only `idle` lets the
    /// signal go.
    lagging: bool,
    mode: Option<&'a str>,
    model: Option<&'a str>,
    /// The login the process runs under (the raw
    /// `CLAUDE_SECURESTORAGE_CONFIG_DIR`; `None`: unset), re-applied exactly:
    /// an upgrade never moves a session onto another account
    /// ([`super::relaunch::Launch::login`]).
    login: Option<&'a str>,
    /// The Claude home it runs under, the same way (the raw
    /// `CLAUDE_CONFIG_DIR`; `None`: unset), re-applied exactly
    /// ([`super::relaunch::Launch::home`]).
    home: Option<&'a str>,
    /// Whether that exec environment authenticates by its stored login
    /// ([`super::relaunch::Launch::stored_login`]).
    stored_login: bool,
    /// The exact live billing context; not inferred from the user's home.
    profile: Option<&'a super::profile::Context>,
}

/// Whether Claude's own status, re-read at the restart's last look (`a`), lets
/// the signal go: `idle`, or, where the step read past a lagging status
/// ([`Restarting::lagging`]), the very status the visit's look read (`looked`),
/// unrewritten since. A turn begun since may leave a stale `busy` unrestamped,
/// so the screen beside it must show the turn over too ([`turn_over_shown`]).
fn status_lets_signal(a: &SessionFile, looked: &SessionFile, lagging: bool) -> bool {
    a.status == "idle"
        || (lagging
            && a.status == looked.status
            && a.status_updated_at_ms == looked.status_updated_at_ms)
}

#[allow(clippy::too_many_arguments)]
fn restart(
    opts: &Opts,
    r: Report,
    st: &mut St,
    c: &mut Client,
    tab: &str,
    sf: &SessionFile,
    t: &[ProcRow],
    k: &dyn Kernel,
    how: &Restarting<'_>,
) -> Report {
    let Restarting {
        argv,
        target,
        host_claim,
        grace,
        fresh,
        quiet,
        lagging,
        mode: shown_mode,
        model: hand_model,
        login,
        home,
        stored_login,
        profile,
    } = *how;
    // Never `--resume` a conversation with no transcript
    // ([`Records::transcript_exists`]): Claude refuses it and exits, leaving
    // the tab with no agent. Such a restart starts afresh, as [`Step::Fresh`]
    // does, and its record says so.
    let records = Records::of_file(&opts.home, sf);
    let fresh = fresh || records.transcript_exists(&sf.session_id) == Some(false);
    let status_holds = |a: &SessionFile| status_lets_signal(a, sf, lagging);
    // The notice this restart answers is the process's own; afresh there is
    // none and the conversation must still have no task; quietly there is none
    // and the process is the one the visit looked at, in its tab.
    let ours = |st: &St, a: &SessionFile| {
        if fresh {
            records.tasked(
                &sf.session_id,
                &conversation_typed(opts, tab, Some(&sf.session_id), &[]),
            ) == Some(false)
        } else if quiet {
            st.tab == tab && a.pid == sf.pid && squash(&a.proc_start) == squash(&sf.proc_start)
        } else {
            st.notice_belongs_to(a, tab)
        }
    };
    if !ours(st, sf) {
        return said(
            r,
            if fresh {
                "wait:tasked"
            } else {
                "wait:notice-owned-by-other-process"
            },
        );
    }
    if is_our_ancestor(sf.pid, t) {
        return said(r, "wait:self");
    }
    // Everything the relaunch needs is read before the agent is ended. The
    // parent must be a job-control shell holding the agent as its foreground
    // job ([`Job`]): only then does its `pgid == tpgid` mean its prompt is
    // back. The line is planned again against that shell, since hooks or
    // directories may have changed since the announcement.
    let job = k.job(sf.pid);
    if let Some(job) = not_a_shell_job(job) {
        return not_a_job(opts, r, st, job);
    }
    let proven = match foreground_shell(job) {
        Ok(shell) => shell,
        Err(why) => return said(r, format!("wait:{why}")),
    };
    // The model the list moves it to, else a model a person chose by hand
    // since the launch: its `--model` would undo that `/model`.
    let model = recorded(&st.model_list).or(hand_model);
    let mode = resume_mode(&records, &sf.session_id, shown_mode.map(str::to_owned));
    let launch = Launch {
        billing: if !st.model_list.is_empty() {
            super::relaunch::ModelBilling::Move(profile)
        } else {
            super::relaunch::ModelBilling::Keep
        },
        ..launch_of(
            sf,
            argv,
            target,
            model,
            mode.as_deref(),
            !fresh,
            home,
            login,
            stored_login,
        )
    };
    let planned = home_carried(&opts.home, home, &sf.cwd, &records).and_then(|homedir| {
        let launch = Launch {
            homedir: homedir.as_deref(),
            ..launch
        };
        plan(opts, &launch, proven, t)
    });
    let Plan { shell, line, .. } = match planned {
        Ok(p) => p,
        Err(no) => return unplanned(opts, r, st, no),
    };
    // THE LINE BACK ([`St::back_line`]): the same resume on the program this
    // process runs, planned now with the line it stands beside, for a build
    // that answers its probe and fails only at a real start. The program is
    // read as a crash's relaunch reads it ([`launched`]): the image, or the
    // script an interpreter runs (an npm install, the managed twin). A model
    // the list moves to is the move's, so it carries only a model a person
    // chose by hand. None where the move runs that very program, or the
    // program or a line for it cannot be had (the move goes as it did).
    let back_line = k
        .image(sf.pid)
        .and_then(|image| launched(argv, Some(&image), &native_root(&opts.home), true))
        .map(|(program, _)| program)
        .filter(|program| *program != target.exe)
        .and_then(|program| {
            let back = Launch {
                exe: &program,
                model: hand_model,
                billing: super::relaunch::ModelBilling::Keep,
                ..launch
            };
            home_carried(&opts.home, home, &sf.cwd, &records)
                .and_then(|homedir| {
                    let back = Launch {
                        homedir: homedir.as_deref(),
                        ..back
                    };
                    plan(opts, &back, proven, t)
                })
                .ok()
        })
        .filter(|back| back.shell == shell)
        .map(|back| back.line)
        .unwrap_or_default();
    // The harness's hand on the tab ([`Hand`]), taken before the last look so
    // no other connection's write lands in the dying agent or the bare shell.
    // Given back on every exit before the signal; the relaunch renews it and
    // returns it after the new agent's first idle ([`super::relaunch::relaunch`]).
    let Some(mut hand) = Hand::take(c, tab) else {
        return said(r, "wait:held");
    };
    let aborted: Option<Report> = 'signal: {
        // The last look before the irreversible act: same process, still idle,
        // still its shell's foreground job, composer empty, nothing running
        // under it, no hand, no unread input, no person within `grace`.
        let again = records.session_file(sf.pid);
        let scr = screen(c, tab);
        let still = again.is_some_and(|a| {
            a.session_id == sf.session_id
                && status_holds(&a)
                && ours(st, &a)
                && k.process_start(sf.pid).as_deref() == Some(squash(&a.proc_start).as_str())
        }) && foreground_shell(k.job(sf.pid)) == Ok(shell)
            && scr.is_some_and(|s| turn_over_shown(&s, lagging) && composer_empty(c, tab, &s))
            && k.restart_background(sf.pid).is_empty()
            && !held(c, tab, grace)
            && host_claim.is_none_or(|claim| claim_still_live(c, sf.pid, claim));
        if !still {
            break 'signal Some(said(r.clone(), "wait:changed"));
        }
        if let Err(why) = require_unique_owner(&opts.home, sf) {
            break 'signal Some(said(r.clone(), format!("wait:{why}")));
        }
        let Ok(pid) = i32::try_from(sf.pid) else {
            break 'signal Some(said(r.clone(), "wait:pid"));
        };
        // A restart carries the agent on: the READY it acts on and any release
        // an earlier notice owed are taken off the record with the exit
        // ([`St::signalled`]), and put back if the signal is never sent.
        let back = st.signalled(sf.pid, shell, tab, line, now_s());
        st.back_line.clone_from(&back_line);
        st.login = login.map(str::to_owned);
        // Afresh: the new process holds a conversation of its own, and
        // nothing is carried on (`relaunch::await_new`).
        if fresh {
            super::relaunch::CAUSE_UPGRADE_FRESH.clone_into(&mut st.cause);
        }
        // The fallback origin this restart carries ([`fallback_carried`]), put
        // back with the rest if the signal is never sent.
        st.fallback_from =
            fallback_carried(fallback_origin(st), recorded(&st.model_list), hand_model);
        if save_checked(opts, &sf.session_id, st).is_err() {
            st.unsent(back);
            st.cause.clear();
            break 'signal Some(said(r.clone(), "wait:claim-unwritten"));
        }
        // A second owner, a job-control change or a moved terminal during the
        // state write vetoes the signal: re-read right before SIGTERM.
        let current = records.session_file(sf.pid);
        let owns_notice = current.as_ref().is_some_and(|a| {
            a.session_id == sf.session_id
                && status_holds(a)
                && ours(st, a)
                && k.process_start(sf.pid).as_deref() == Some(squash(&a.proc_start).as_str())
        });
        let owner = k.terminal(sf.pid);
        let owner = owner.as_ref().map(|(pid, name)| (*pid, name.as_str()));
        // Composer, hands and input are read again, last, on the same
        // connection: a draft or prompt begun during the state write vetoes
        // the signal, which would drop unread input.
        if !owns_notice
            || require_unique_owner(&opts.home, sf).is_err()
            || foreground_shell(k.job(sf.pid)) != Ok(shell)
            || !owner.is_some_and(owned_by_aterm)
            || host_claim.is_some_and(|claim| !claim_still_live(c, sf.pid, claim))
            || !typing_fence_read(c, tab).is_ok_and(|(_, read)| turn_over_shown(&read, lagging))
            || held(c, tab, grace)
        {
            st.unsent(back);
            st.cause.clear();
            save(opts, &sf.session_id, st);
            break 'signal Some(said(r.clone(), "wait:changed-before-signal"));
        }
        // The model asked (`st.model_list`) is recorded by the relaunch line
        // that asks it (`relaunch::relaunch`): a signal never sent asks nothing.
        match terminate(c, tab, pid, grace) {
            // Ended: its carry-on, not a release, tells it to go on.
            Terminated::Sent => None,
            Terminated::Refused(why) => {
                st.unsent(back);
                st.cause.clear();
                save(opts, &sf.session_id, st);
                Some(said(r.clone(), format!("wait:signal-{why}")))
            }
            Terminated::Failed => {
                // The agent answered READY and lives on, stopped: owed its
                // release, its READY no longer acted on.
                st.signal_failed(back, now_s());
                save(opts, &sf.session_id, st);
                Some(said(r.clone(), "failed:signal-refused"))
            }
        }
    };
    if let Some(r) = aborted {
        hand.give_back(c);
        return r;
    }
    ledger(
        opts,
        &said(r.clone(), "terminated"),
        if fresh {
            "SIGTERM (no task: restarted afresh, nothing typed or resumed)"
        } else if quiet {
            "SIGTERM (idle: nothing asked or typed; resumed as it was)"
        } else {
            "SIGTERM"
        },
    );
    relaunch(opts, r, st, c, &sf.session_id, k)
}

/// Takes back, off the conversation's model record ([`ModelRecord::take_back`]),
/// the ask of a relaunch line that was not typed, so it never settles as a model
/// that failed.
pub(super) fn take_back_ask(opts: &Opts, session: &str, asked: Option<models::Asked>) {
    if let Some(asked) = asked {
        let mut rec = load_model_record(opts, session);
        rec.take_back(asked);
        save_model_record(opts, session, &rec);
    }
}

/// THE CONFIRMATIONS a carry-on left pending ([`confirm`]): each restart whose
/// continuation was typed before its resumed session had answered is read
/// again, and says its `done` row once the answer is written or its time is
/// up. Only files are read and written — but for a restore look whose resumed
/// build warned in an open task, whose one line is typed into its tab where
/// that tab's own Autopilot is on, under the tab's own guards
/// (`relaunch::restore_look`) — so every sweep runs it first, whatever else it
/// finds; a hand-run sweep for one tab (`only_sid`) confirms only that tab's.
fn confirmations(opts: &Opts) -> Vec<Report> {
    if opts.dry_run {
        return Vec::new();
    }
    let Ok(dir) = std::fs::read_dir(state_dir(opts)) else {
        return Vec::new();
    };
    let now = now_s();
    let mut out = Vec::new();
    for e in dir.flatten() {
        let path = e.path();
        if path.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let Some(session) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
            continue;
        };
        let Some(mut st) = load(opts, &session).filter(St::confirming) else {
            continue;
        };
        if opts.only_sid.as_ref().is_some_and(|s| *s != st.tab) {
            continue;
        }
        let r = Report {
            pid: st.resumed_pid,
            tab: st.tab.clone(),
            session: session.clone(),
            from: st.from.clone(),
            to: format!("{}({})", st.resumed_on, st.source),
            step: String::new(),
        };
        out.extend(confirm(opts, &r, &mut st, &session, now));
    }
    out
}

// The upgrade tests drive real processes through Unix APIs (process groups, modes, inodes).
#[cfg(all(test, unix))]
#[path = "upgrade_drive_tests.rs"]
mod tests;

// On Windows the start time is read by syscall, not `ps`; its one behaviour is
// checked here, where the Unix process-group tests cannot run.
#[cfg(all(test, windows))]
mod kernel_start_win_tests {
    /// This process's start reads as the raw creation `FILETIME` decimal (what
    /// Claude Code records as `procStart`) — all digits, equal to the aterm-uds
    /// primitive, and never a `ps` rendering. A process that never existed
    /// reads nothing, and pid 0 reads nothing (no `ps` is spawned either way).
    #[test]
    fn the_start_is_the_filetime_decimal_read_by_syscall() {
        let me = std::process::id();
        let start = super::kernel_start(me).expect("this process's start");
        assert!(
            start.chars().all(|c| c.is_ascii_digit()),
            "a FILETIME decimal, not a `ps` lstart: {start:?}"
        );
        let ticks = aterm_uds::process::read_creation_filetime(i32::try_from(me).unwrap())
            .expect("the raw creation time");
        assert_eq!(start, ticks.to_string(), "the syscall's exact decimal");
        // A pid that is almost certainly free, and pid 0, read nothing.
        assert_eq!(super::kernel_start(0x7fff_fff0), None, "no such process");
        assert_eq!(super::kernel_start(0), None, "never pid 0");
    }
}
