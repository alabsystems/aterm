// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Harness agent upgrade driver (`aterm agent upgrade`, live relaunch, and transcript inspection).
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

/// The ladder's projection: the model variables a look's real [`Facts`] stand for.
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
    pub home: PathBuf,
    pub state: PathBuf,
    pub sock: Option<String>,
    /// The window harness's own token, presented on [`Self::sock`] alone ([`harness_client`]):
    /// a window's sweep passes it, so what the drive types reads `by=harness`; the CLI's is
    /// `None`, the token beside the socket.
    pub token: Option<HarnessToken>,
    pub only_sid: Option<String>,
    pub dry_run: bool,
    pub human_grace_s: u32,
    pub hand_back: bool,
    pub background: bool,
    pub aterm_state: Option<PathBuf>,
    pub ask: bool,
    pub several_logins: bool,
    pub usage_credits: bool,
    pub usage_tap: Option<super::tap::Armed>,
    pub instances: Instances,
    pub agents_off: Vec<aterm_phase::Program>,
    pub land: bool,
    pub timings: super::Timings,
}

/// Source of live `aterm` control sockets for a CLI upgrade sweep.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Instances {
    Machine,
    Listed(Vec<String>),
}

fn model_evidence(opts: &Opts, records: &Records) -> models::Evidence {
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
        _ => models::Evidence {
            credits_billed,
            ..models::Evidence::default()
        },
    }
}

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
    pub pid: u32,
    pub tab: String,
    pub session: String,
    pub from: String,
    pub to: String,
    pub step: String,
}

impl Report {
    /// Whether this report is something the sweep did or recorded, as opposed to a wait, a skip or `current`.
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

    /// The same line under another name, so a log never calls a relaunch an upgrade.
    #[must_use]
    pub fn line_as(&self, kind: &str) -> String {
        format!(
            "{kind} pid={} tab={} session={} from={} to={} step={}",
            self.pid, self.tab, self.session, self.from, self.to, self.step
        )
    }
}

thread_local! {
    static GIVEN_NOW: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

pub(super) fn now_s() -> u64 {
    GIVEN_NOW
        .with(std::cell::Cell::get)
        .unwrap_or_else(|| aterm_time::unix_secs().unwrap_or(0))
}

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

#[cfg(any(not(target_os = "macos"), test))]
fn ps(args: &[&str]) -> Option<String> {
    let mut cmd = aterm_uds::spawnfd::command(aterm_uds::os_path!("ps"));
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
    #[cfg(not(unix))]
    {
        aterm_uds::process::pid_alive(pid)
    }
}

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

pub(super) type ProcRow = (u32, u32, Comm);

#[derive(Clone)]
pub(super) struct Comm {
    name: std::sync::OnceLock<String>,
    #[cfg(target_os = "macos")]
    record: Option<aterm_sysprobe::kproc::Record>,
}

impl Comm {
    #[cfg(target_os = "macos")]
    const fn named_when_asked(r: aterm_sysprobe::kproc::Record) -> Self {
        Self {
            name: std::sync::OnceLock::new(),
            record: Some(r),
        }
    }

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

pub(super) fn table() -> Vec<ProcRow> {
    #[cfg(target_os = "macos")]
    {
        aterm_sysprobe::kproc::records()
            .unwrap_or_default()
            .into_iter()
            .filter(|r| !r.zombie)
            .map(|r| (r.pid, r.ppid, Comm::named_when_asked(r)))
            .collect()
    }
    #[cfg(not(target_os = "macos"))]
    {
        ps_table()
    }
}

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

#[cfg(target_os = "macos")]
fn comm_name(r: &aterm_sysprobe::kproc::Record) -> String {
    base_name(&squash(&aterm_sysprobe::kproc::ps_comm(
        r,
        aterm_sysprobe::kproc::Locale::C,
    )))
}

#[cfg(target_os = "macos")]
fn tty_word(tty: Option<i32>) -> String {
    tty.map_or_else(|| "??".to_string(), |dev| format!("dev{dev}"))
}

pub(super) fn descendants_in(table: &[ProcRow], root: u32) -> Option<Vec<(u32, String)>> {
    if table.is_empty() {
        return None;
    }
    let mut found = Vec::new();
    let mut frontier = vec![root];
    while let Some(parent) = frontier.pop() {
        for (pid, ppid, name) in table {
            if *ppid == parent
                && *pid != root
                && name != aterm_sysprobe::kproc::DEFUNCT
                && !name.ends_with(" <defunct>")
                && !found.iter().any(|(p, _)| p == pid)
            {
                found.push((*pid, name.to_string()));
                frontier.push(*pid);
            }
        }
    }
    Some(found)
}

fn base_name(comm: &str) -> String {
    comm.rsplit('/')
        .next()
        .unwrap_or(comm)
        .trim_start_matches('-')
        .to_string()
}

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

pub(super) fn owned_by_aterm(owner: (u32, &str)) -> bool {
    owner.0 == 1 || owner.1 == "aterm" || owner.1.starts_with("aterm-")
}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Job {
    Foreground,
    Background,
    TabProgram { foreground: bool },
    NoJobControl,
}

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

fn job_of<K: Kernel + ?Sized>(k: &K, agent: u32) -> Option<(Job, u32)> {
    let (parent, pgid, tpgid) = k.ids(agent)?;
    let (_, parent_pgid, _) = k.ids(parent)?;
    let opened = k
        .opener(agent, parent)
        .is_some_and(|name| owned_by_aterm((parent, name.as_str())));
    Some((job(agent, pgid, tpgid, parent_pgid, opened), parent))
}

pub(super) const fn foreground_shell(job: Option<(Job, u32)>) -> Result<u32, &'static str> {
    match job {
        Some((Job::Foreground, shell)) => Ok(shell),
        Some((Job::Background, _)) => Err("not-foreground"),
        Some((Job::NoJobControl | Job::TabProgram { .. }, _)) => Err("not-a-shell-job"),
        None => Err("ids"),
    }
}

pub(super) const fn leads_terminal(job: Option<(Job, u32)>) -> Result<(), &'static str> {
    match job {
        Some((Job::Foreground | Job::TabProgram { foreground: true }, _)) => Ok(()),
        Some((Job::Background | Job::TabProgram { foreground: false }, _)) => Err("not-foreground"),
        Some((Job::NoJobControl, _)) => Err("not-a-shell-job"),
        None => Err("ids"),
    }
}

pub(super) const fn not_a_shell_job(job: Option<(Job, u32)>) -> Option<Job> {
    match job {
        Some((job @ (Job::NoJobControl | Job::TabProgram { .. }), _)) => Some(job),
        Some((Job::Foreground | Job::Background, _)) | None => None,
    }
}

pub(super) trait Kernel {
    fn job(&self, agent: u32) -> Option<(Job, u32)> {
        job_of(self, agent)
    }
    fn ids(&self, pid: u32) -> Option<(u32, i64, i64)> {
        ids(pid)
    }
    fn parent(&self, pid: u32) -> Option<u32>;
    fn terminal(&self, pid: u32) -> Option<(u32, String)>;
    fn opener(&self, agent: u32, parent: u32) -> Option<String> {
        self.terminal(agent)
            .filter(|(owner, _)| *owner == parent)
            .map(|(_, name)| name)
    }
    fn process_start(&self, pid: u32) -> Option<String> {
        kernel_start(pid)
    }
    fn restart_background(&self, pid: u32) -> Vec<String> {
        live_background(pid)
    }
    fn image(&self, pid: u32) -> Option<PathBuf> {
        exe_of(pid)
    }
    fn shell_holds_tab(&self, shell: u32) -> Option<bool> {
        if shell == 0 {
            return None;
        }
        let (_, pgid, tpgid) = self.ids(shell)?;
        Some(pgid == i64::from(shell) && pgid == tpgid)
    }
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

const OWN_KEEP_AWAKE: &str = "caffeinate";

pub(super) fn background(pid: u32, t: &[ProcRow]) -> Vec<String> {
    background_procs(pid, t)
        .into_iter()
        .map(|(_, name)| name)
        .collect()
}

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

pub(super) const UNREADABLE_TABLE: &str = "ps-unreadable";

pub(super) fn live_background(pid: u32) -> Vec<String> {
    background_or_unreadable(pid, &table())
}

pub(super) fn background_or_unreadable(pid: u32, t: &[ProcRow]) -> Vec<String> {
    if t.is_empty() {
        return vec![UNREADABLE_TABLE.to_string()];
    }
    background(pid, t)
}

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
        (upgrade::Agent::Muse, _) => {
            "Muse Code's live upgrade asks nothing (its quiet flow alone): it is restarted onto \
             the new build at its next genuinely idle point, and nothing is typed into it"
                .to_string()
        }
        (upgrade::Agent::Agy, _) => {
            "agy's live upgrade asks nothing (its quiet flow alone): it is restarted onto the new \
             build at its next genuinely idle point, and nothing is typed into it"
                .to_string()
        }
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

pub(super) fn cwd_of(pid: u32) -> Option<String> {
    aterm_sysprobe::process_cwd(pid).map(|p| p.to_string_lossy().into_owned())
}

pub(super) fn exe_of(pid: u32) -> Option<PathBuf> {
    aterm_sysprobe::process_path(pid)
}

/// Every file `pid` holds open through a descriptor, `None` when unreadable or exited, read in
/// process ([`aterm_uds::holders::open_files`]).
pub(super) fn open_files(pid: u32) -> Option<Vec<PathBuf>> {
    aterm_uds::holders::open_files(pid).ok()
}

/// Returns `scan.holders` when `scan.unreadable` is empty; sealed tables (`HolderScan::sealed`) do not make the scan incomplete.
fn complete_file_holders(scan: aterm_uds::holders::HolderScan) -> Option<Vec<u32>> {
    scan.unreadable.is_empty().then_some(scan.holders)
}

/// Every process holding `file` open through a descriptor, `None` when unknown: only a
/// complete in-process scan ([`aterm_uds::holders::scan_file_holders`]) is an answer.
pub(super) fn file_holders(file: &Path) -> Option<Vec<u32>> {
    complete_file_holders(aterm_uds::holders::scan_file_holders(file, &[]).ok()?)
}

/// `lsof -t`'s holders of `file`: the oracle [`file_holders`] is held to.
#[cfg(all(test, target_os = "macos"))]
pub(super) fn lsof_holders(file: &Path) -> Option<Vec<u32>> {
    let mut cmd = aterm_uds::spawnfd::command(aterm_uds::os_path!("lsof"));
    cmd.args(["-n", "-P", "-t"]).arg(file);
    let out = aterm_uds::deadline::output_until(&mut cmd, Instant::now() + Duration::from_secs(10))
        .ok()?;
    lsof_holder_pids(&out)
}

#[cfg(all(test, unix))]
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
        1 if out.stdout.is_empty() => Some(Vec::new()),
        _ => None,
    }
}

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

pub(super) fn running_block(c: &mut Client, tab: &str) -> Option<u64> {
    let (head, body) = c.request_counted(&format!("@{tab} blocks 1 --json")).ok()?;
    let blocks = blocks_of(&head, &body)?;
    let last = blocks.last()?;
    (last.get("state").and_then(Value::as_str) == Some("executing"))
        .then(|| last.get("id").and_then(Value::as_u64))
        .flatten()
}

pub(super) fn exit_status(c: &mut Client, tab: &str, block: Option<u64>) -> Option<i64> {
    let (head, body) = c.request_counted(&format!("@{tab} blocks 2 --json")).ok()?;
    parse_exit_status(&head, &body, block)
}

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

pub(super) const LINE_BLOCKS: u32 = 8;

pub(super) fn block_exit(head: &str, body: &str, block: u64) -> Option<i64> {
    blocks_of(head, body)?
        .iter()
        .find(|b| b.get("id").and_then(Value::as_u64) == Some(block))
        .filter(|b| b.get("state").and_then(Value::as_str) == Some("complete"))?
        .get("exit")
        .and_then(Value::as_i64)
}

pub(super) fn line_status(c: &mut Client, tab: &str, block: Option<u64>) -> Option<i64> {
    let block = block?;
    let (head, body) = c
        .request_counted(&format!("@{tab} blocks {LINE_BLOCKS} --json"))
        .ok()?;
    block_exit(&head, &body, block)
}

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

pub(super) fn twin_naming(twin: &Path, program: &str) -> Option<PathBuf> {
    let exe = catalog::twin_binary(&std::fs::read_to_string(twin).ok()?)?;
    (exe.file_name().and_then(|n| n.to_str()) == Some(program) && exe.is_file())
        .then(|| twin.to_path_buf())
}

pub(super) fn managed_agent_twin(program: &str) -> Option<PathBuf> {
    twin_naming(&agents_dir()?.join(program), program)
}

#[cfg(not(test))]
fn agents_dir() -> Option<PathBuf> {
    Some(atpkg::store::resolve_configured()?.agents_dir())
}

#[cfg(test)]
thread_local! {
    pub(super) static AGENTS_DIR: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn agents_dir() -> Option<PathBuf> {
    AGENTS_DIR.with(|dir| dir.borrow().clone())
}

pub(super) fn twin_version(twin: &Path) -> Option<String> {
    let exe = catalog::twin_binary(&std::fs::read_to_string(twin).ok()?)?;
    store_version(&exe)
}

#[cfg(not(test))]
const VERSION_WAIT: Duration = Duration::from_secs(5);
#[cfg(test)]
const VERSION_WAIT: Duration = aterm_spec::wait::MUST_HAPPEN;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Probed {
    Version(Version),
    NoVersion,
    TimedOut,
}

fn version_of(exe: &Path) -> Probed {
    version_within(exe, VERSION_WAIT)
}

fn version_within(exe: &Path, limit: Duration) -> Probed {
    let deadline = Instant::now() + limit;
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
            // SAFETY: `process_group(0)` made this unreaped child's PID its private PGID.
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

pub(super) fn native_root(home: &Path) -> PathBuf {
    home.join(".local/share/claude/versions")
}

fn managed_twin(layout: &atpkg::store::Layout, program: &str) -> Option<PathBuf> {
    let twin = layout.agents_dir().join(program);
    if !atpkg::linkmode::is_linked(layout, program) {
        return Some(twin);
    }
    let exe = super::upgrade_catalog::twin_binary(&std::fs::read_to_string(&twin).ok()?)?;
    exe.starts_with(layout.prefix.join("store")).then_some(twin)
}

pub(super) struct Targets {
    managed: Option<Candidate>,
    native: Option<Candidate>,
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

    pub(super) fn for_session(&self, running_native: bool) -> Vec<Candidate> {
        self.managed
            .iter()
            .chain(self.native.iter().filter(|_| running_native))
            .cloned()
            .collect()
    }
}

const VERSION_RETRY_FIRST: Duration = Duration::from_secs(2 * 60);
const VERSION_RETRY_MAX: Duration = Duration::from_secs(60 * 60);

#[derive(Clone, Debug)]
enum Seen {
    Answered(Option<Version>),
    TimedOut { streak: u32, retry: Instant },
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Asked {
    Answered(Option<Version>),
    Unanswered,
}

fn cached_version(exe: &Path) -> Asked {
    cached_version_with(exe, Instant::now(), version_of)
}

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

pub(super) type Client = RelayClient<aterm_uds::CtlStream>;

pub(super) fn connect(opts: &Opts, sid: &str) -> Result<Client, String> {
    let sock = match &opts.sock {
        Some(s) => s.clone(),
        None => aterm_ctl::resolve_sock_for(Some(sid)).map_err(|e| format!("{e}"))?,
    };
    connect_at(opts, &sock)
}

pub(super) fn connect_at(opts: &Opts, sock: &str) -> Result<Client, String> {
    let token = opts
        .token
        .as_ref()
        .filter(|_| opts.sock.as_deref() == Some(sock));
    let c = harness_client(sock, token).map_err(|e| format!("{sock}: {e}"))?;
    Ok(if opts.dry_run { c.reads_only() } else { c })
}

/// The window harness's in-memory control token, which reads `by=harness` on the window's own
/// socket. Its `Debug` never prints it, so no `Opts` dump logs it.
#[derive(Clone, PartialEq, Eq)]
pub struct HarnessToken(String);

impl HarnessToken {
    /// The token `token`, trimmed.
    #[must_use]
    pub fn new(token: &str) -> Self {
        Self(token.trim().to_string())
    }
}

impl std::fmt::Debug for HarnessToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HarnessToken(redacted)")
    }
}

/// A connection to `sock` that presents `token`, probed with `version`. A token the server
/// refuses (`ERR auth`, or the connection it closed on the refusal) is replaced once by the token
/// beside the socket, every client's: a seamless successor that rebound the socket knows only
/// its own. `None` presents the token beside the socket, unprobed.
/// # Errors
/// The socket could not be reached, the token beside it read, or the probe's reply read.
pub fn harness_client(sock: &str, token: Option<&HarnessToken>) -> std::io::Result<Client> {
    let Some(token) = token else {
        return RelayClient::connect_local_beside(sock);
    };
    let mut c = RelayClient::connect_local(sock, &token.0)?;
    match c.request_counted("version") {
        Ok((head, _)) if head == "ERR auth" => RelayClient::connect_local_beside(sock),
        Ok(_) => Ok(c),
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::UnexpectedEof
            ) =>
        {
            RelayClient::connect_local_beside(sock)
        }
        Err(e) => Err(e),
    }
}

pub(super) fn roster_connection(sock: &str) -> Option<Client> {
    RelayClient::connect_local_beside(sock)
        .ok()
        .map(RelayClient::reads_only)
}

pub(super) fn connect_reads(opts: &Opts, sid: &str) -> Result<Client, String> {
    connect(
        &Opts {
            dry_run: true,
            ..opts.clone()
        },
        sid,
    )
}

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

pub(super) fn roster_aside(c: &mut Client, agents_off: &[aterm_phase::Program]) -> Option<Aside> {
    roster_aside_within(c, agents_off, CLAIMS_WAIT)
}

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

const CLAIMS_WAIT: Duration = Duration::from_secs(2);

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

pub(super) struct Screen {
    pub(super) rows: Vec<String>,
    pub(super) cursor: Option<(usize, usize)>,
    pub(super) seq: u64,
    pub(super) first: usize,
    pub(super) generation: Option<String>,
    pub(super) human: HumanInput,
}

pub(super) fn screen(c: &mut Client, sid: &str) -> Option<Screen> {
    screen_query(c, &format!("@{sid} text --json"))
}

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

fn is_generation(g: &str) -> bool {
    ScreenGen::parse(g).is_some()
}

pub(super) fn composer_empty(c: &mut Client, sid: &str, scr: &Screen) -> bool {
    let dim = dim_placeholder(c, sid, scr);
    upgrade::composer_is_empty(&scr.rows, scr.cursor, dim)
}

pub(super) fn dim_placeholder(c: &mut Client, sid: &str, scr: &Screen) -> bool {
    dim_cell(scr).is_some_and(|at| {
        c.request_line(&format!("@{sid} cell {at} 2"))
            .is_ok_and(|line| cell_is_dim(&line))
    })
}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Left {
    Gone,
    Kept(&'static str),
}

pub(super) fn compact(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

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

fn left_cut(st: &mut St, c: &mut Client, tab: &str, text: &str) {
    st.left_typed = text.to_string();
    let _ = settle_left(st, c, tab, left_standing);
}

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

pub(super) fn held(c: &mut Client, sid: &str, grace_s: u32) -> bool {
    status_line(c, sid).is_none_or(|line| held_by_status(&line, grace_s))
}

pub(super) fn status_line(c: &mut Client, sid: &str) -> Option<String> {
    c.request_line(&format!("@{sid} status")).ok()
}

/// What [`step`] answers for a tab whose agent supervises itself under a proven self-claim ([`self_claimed_by_status`]).
pub const SKIP_SELF_CLAIMED: &str = "skip:self-claimed";

#[must_use]
pub(super) fn self_claimed_by_status(line: &str) -> bool {
    crate::is_ok_reply(line) && line.split_whitespace().any(|t| t == "supervisor_proven=1")
}

pub(super) fn held_by_status(line: &str, grace_s: u32) -> bool {
    !crate::is_ok_reply(line)
        || super::relaunch::hand_on_tab(line)
        || line.split_whitespace().any(|t| {
            t.strip_prefix("input=")
                .is_some_and(|input| !matches!(input, "-" | "clear"))
        })
        || super::relaunch::person_present(line, grace_s)
}

/// How long a hand lasts unless renewed (`lease acquire ttl=`).
pub(crate) const HAND_TTL_MS: u64 = 60_000;

/// How often a holder renews its hand: well inside [`HAND_TTL_MS`], so a
/// renewal a starved holder makes late still lands before the hand lapses.
pub(crate) const HAND_RENEW: Duration = Duration::from_secs(20);

/// The holder name of this process's hand ([`Hand`]).
#[must_use]
pub fn hand_name() -> String {
    format!("{HOST_HOLDER}{}", std::process::id())
}

const HOST_HOLDER: &str = "aterm-harness@";

/// The request that takes (or renews) this process's hand on a tab, without its `@<tab>` selector.
#[must_use]
pub fn hand_take_request() -> String {
    format!(
        "lease acquire ttl={HAND_TTL_MS} holder={} hard",
        hand_name()
    )
}

/// The request that gives this process's hand back.
#[must_use]
pub fn hand_give_back_request() -> String {
    format!("lease release holder={}", hand_name())
}

/// [`hand_take_request`] kept to the asking connection (`lease acquire … here`).
#[must_use]
pub fn hand_take_here_request() -> String {
    hand_take_here_request_as(&hand_name())
}

/// [`hand_take_here_request`] under the hand name `holder`.
#[must_use]
pub fn hand_take_here_request_as(holder: &str) -> String {
    format!("lease acquire ttl={HAND_TTL_MS} holder={holder} hard here")
}

/// [`hand_give_back_request`] kept to the asking connection (`lease release … here`, [`hand_take_here_request`]'s pair).
#[must_use]
pub fn hand_give_back_here_request() -> String {
    hand_give_back_here_request_as(&hand_name())
}

/// [`hand_give_back_here_request`] under the hand name `holder`.
#[must_use]
pub fn hand_give_back_here_request_as(holder: &str) -> String {
    format!("lease release holder={holder} here")
}

/// Whether a claim's holder (percent-decoded) is a window's own host.
#[must_use]
pub fn is_host_holder(holder: &str) -> bool {
    holder.starts_with(HOST_HOLDER)
}

#[must_use]
pub(super) fn our_hand(hand: &str) -> bool {
    hand.strip_prefix("lease:") == Some(hand_name().as_str())
}

#[derive(Debug)]
pub(super) struct Hand {
    tab: String,
    held: bool,
    renewed: Instant,
}

impl Hand {
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

    pub(super) fn take_or_none(c: &mut Client, tab: &str) -> Hand {
        Hand::take(c, tab).unwrap_or_else(|| Hand {
            tab: tab.to_string(),
            held: false,
            renewed: Instant::now(),
        })
    }

    pub(super) fn keep(&mut self, c: &mut Client) {
        if self.held && self.renewed.elapsed() >= HAND_RENEW {
            self.held = Hand::take(c, &self.tab).is_some_and(|h| h.held);
            self.renewed = Instant::now();
        }
    }

    pub(super) fn give_back(&mut self, c: &mut Client) {
        if std::mem::take(&mut self.held) {
            let _ = c.request_line(&format!("@{} {}", self.tab, hand_give_back_request()));
        }
    }
}

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

pub(super) fn quiet_s(status: Option<&str>, st: &mut St, seq: u64, now: u64) -> u64 {
    if seq != st.last_seq {
        st.last_seq = seq;
        st.seq_since_s = now;
    }
    let field = |key: &str| status?.split_whitespace().find_map(|t| t.strip_prefix(key));
    let since_ms = field("agent_since_ms=").and_then(|ms| ms.parse::<u64>().ok());
    match (field("agent="), since_ms) {
        (Some("idle" | "question"), Some(ms)) => ms / 1000,
        (Some(word), Some(_)) if word != "-" && word != "unknown" => 0,
        _ => now.saturating_sub(st.seq_since_s),
    }
}

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Typed {
    Changed,
    Yielded,
    Cut(String),
    Refused(String),
}

impl Typed {
    pub(super) fn word(&self) -> String {
        match self {
            Typed::Changed => "changed".to_string(),
            Typed::Yielded => "yield".to_string(),
            Typed::Cut(why) => format!("cut-{why}"),
            Typed::Refused(e) => first_word(e),
        }
    }
}

pub(super) const TURN_WAIT: &str = "idle=1 timeout=10000";

pub(super) fn turn(c: &mut Client, sid: &str, text: &str) -> Result<(), String> {
    turn_fenced(c, sid, text, None).map_err(|e| match e {
        Typed::Refused(e) => e,
        other => other.word(),
    })
}

fn turn_takes_gen(c: &mut Client) -> bool {
    c.help_counted("turn")
        .is_ok_and(|(head, body)| crate::is_ok_reply(&head) && (head + &body).contains("if-gen="))
}

fn turn_takes_cap(c: &mut Client) -> bool {
    c.help_counted("turn")
        .is_ok_and(|(head, body)| crate::is_ok_reply(&head) && (head + &body).contains("cap=<n>"))
}

const NOTICE_KEYS: &str = "typed=1 cadence=2 cap=4096 ";

const PRESSED_DIGIT_MS: u64 = 150;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Put {
    Paste,
    Pressed,
}

fn turn_takes_guard(c: &mut Client) -> bool {
    c.help_counted("turn")
        .is_ok_and(|(head, body)| crate::is_ok_reply(&head) && (head + &body).contains("guarded:"))
}

fn turn_takes_rows(c: &mut Client) -> bool {
    c.help_counted("turn")
        .is_ok_and(|(head, body)| crate::is_ok_reply(&head) && (head + &body).contains("if-rows="))
}

pub(super) fn turn_fenced(
    c: &mut Client,
    sid: &str,
    text: &str,
    generation: Option<&str>,
) -> Result<(), Typed> {
    checked_turn(c, sid, text, generation, Put::Paste)
}

pub(super) fn notice_fenced(
    c: &mut Client,
    sid: &str,
    text: &str,
    generation: Option<&str>,
) -> Result<(), Typed> {
    checked_turn(c, sid, text, generation, Put::Pressed)
}

fn checked_turn(
    c: &mut Client,
    sid: &str,
    text: &str,
    generation: Option<&str>,
    put: Put,
) -> Result<(), Typed> {
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

#[derive(Clone)]
pub(super) struct ModelCtx {
    list: Priority,
    baked: Option<Baked>,
    offered: Vec<String>,
    default_model: Option<String>,
    native: Option<(Option<Baked>, Vec<String>)>,
    versions: (String, String),
}

impl ModelCtx {
    fn view(&self, native: bool) -> (Option<&Baked>, &[String]) {
        match (&self.native, native) {
            (Some((baked, offered)), true) => (baked.as_ref(), offered),
            _ => (self.baked.as_ref(), &self.offered),
        }
    }

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
        let changes = ctx.grow(&ev);
        if !changes.is_empty() && !opts.dry_run {
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

    fn grow(&mut self, ev: &models::Evidence) -> Vec<models::Change> {
        let now = i64::try_from(now_s()).unwrap_or(0);
        let recs = models::recommendations(ev, self.baked.as_ref(), &self.versions.0);
        models::ingest(&mut self.list, &recs, self.baked.as_ref(), now)
    }

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

    fn offer(&mut self, ev: &models::Evidence) {
        self.offered = models::offered(&self.list, &self.versions.0, ev, self.baked.as_ref(), None);
        if let Some((nb, no)) = &mut self.native {
            *no = models::offered(&self.list, &self.versions.1, ev, nb.as_ref(), None);
        }
    }

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

fn agent_settings(
    opts: &Opts,
    sf: &SessionFile,
    claude_dir: &Path,
) -> Option<models::UserModelSettings> {
    use crate::supervise::policy::claude_settings;
    use crate::supervise::policy::git_config::WorkerEnv;
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
                managed_said.insert(key.to_string(), value.clone());
            } else if !said_below.contains_key(key) {
                said_below.insert(key.to_string(), value.clone());
            }
        }
    }
    for (key, value) in managed_said {
        said_below.insert(key, value);
    }
    Some(models::model_settings_of(&Value::Object(said_below)))
}

/// Every settings document a relaunch line of `argv` in `cwd` would have Claude Code read
/// for the tap's [`super::tap::gate`]: under the host's home, the user's `settings.json` and
/// `cowork_settings.json` alike (the line sets no cowork flag of its own, and the shell it is
/// typed into may), the project's, the managed ones.
pub(super) fn tap_settings(
    opts: &Opts,
    argv: &[String],
    cwd: &str,
) -> Vec<(String, super::netwatch::Source)> {
    tap_settings_under(&opts.home, argv, cwd)
}

/// WHETHER A RELAUNCH LINE OF `argv` IN `cwd` UNDER THE USER HOME `home` FINDS ITS SETTINGS
/// ADMIT the usage tap ([`super::tap::settings_admit`] over [`tap_settings`]'s documents):
/// the one judgment the relaunch's gate and the Claude Code footer's reason for a missing
/// week share, so the footer never promises a restart the relaunch's line would not tap.
#[must_use]
pub fn tap_settings_admit(home: &Path, argv: &[String], cwd: &str) -> bool {
    super::tap::settings_admit(&tap_settings_under(home, argv, cwd))
}

/// [`tap_settings`] under the user home `home`.
fn tap_settings_under(
    home: &Path,
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
            home.display()
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
        let sources =
            claude_settings::sources_under(&worker, Path::new(cwd), managed.as_deref(), Some(home))
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

#[cfg(not(test))]
fn host_xdg_config_home() -> Option<String> {
    std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|v| !v.trim().is_empty())
}

#[cfg(test)]
fn host_xdg_config_home() -> Option<String> {
    None
}

#[cfg(not(test))]
fn managed_settings_dir() -> Option<PathBuf> {
    crate::supervise::policy::claude_settings::MANAGED_DIR.map(PathBuf::from)
}

#[cfg(not(test))]
fn managed_preferences_dir() -> Option<PathBuf> {
    Some(PathBuf::from(
        crate::supervise::policy::claude_settings::MANAGED_PREFERENCES_DIR,
    ))
}

#[cfg(test)]
thread_local! {
    pub(super) static MANAGED_SETTINGS: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
    pub(super) static MANAGED_PREFERENCES: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn managed_settings_dir() -> Option<PathBuf> {
    MANAGED_SETTINGS.with(|dir| dir.borrow().clone())
}

#[cfg(test)]
fn managed_preferences_dir() -> Option<PathBuf> {
    MANAGED_PREFERENCES.with(|dir| dir.borrow().clone())
}

fn model_record_path(opts: &Opts, session: &str) -> PathBuf {
    model_record_in(&opts.state, session)
}

fn model_record_in(state: &Path, session: &str) -> PathBuf {
    state
        .join("upgrade")
        .join("models")
        .join(format!("{session}.json"))
}

/// The resume line a person is told to run, under the relaunch's own model rule.
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

struct ModelRead {
    verdict: ModelVerdict,
    cold: bool,
    due_for_s: u64,
    ride_barred: Option<&'static str>,
}

#[cfg(all(test, unix))]
fn model_read(opts: &Opts, sf: &SessionFile, launch: Option<&str>, mctx: &ModelCtx) -> ModelRead {
    model_read_for(opts, sf, launch, mctx, false, None, None)
}

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

type ModelRow = (&'static str, String, &'static str);

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

pub(super) fn riding_model_in(
    opts: &Opts,
    sf: &SessionFile,
    argv: &[String],
    mctx: &ModelCtx,
    process_view: Option<&ModelCtx>,
) -> Option<String> {
    let launch = upgrade::launch_model(argv);
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

fn runs_native(opts: &Opts, pid: Option<u32>, argv: &[String]) -> bool {
    pid.and_then(exe_of)
        .or_else(|| argv.first().map(PathBuf::from))
        .is_some_and(|e| e.starts_with(native_root(&opts.home)))
}

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

pub(super) fn launch_kept(opts: &Opts, session: &str, argv: &[String]) -> Option<String> {
    let launch = upgrade::launch_model(argv)?;
    models::launch_model_kept(Some(&launch), &load_model_record(opts, session)).map(str::to_owned)
}

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

pub(super) fn record_asked(opts: &Opts, session: &str, model: &str) -> Option<models::Asked> {
    if opts.dry_run || model.is_empty() {
        return None;
    }
    let mut rec = load_model_record(opts, session);
    let asked = rec.asked(model, now_s());
    save_model_record(opts, session, &rec);
    Some(asked)
}

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

fn model_wants_a_look(opts: &Opts, sf: &SessionFile, mctx: &ModelCtx) -> bool {
    let mctx = &*mctx.at(opts, sf);
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
    if let Some((first, _, rows)) = reads.first()
        && reads.iter().all(|(judged, _, _)| judged == first)
    {
        model_read_kept(opts, sf, &before, first, rows);
    }
    reads
        .iter()
        .any(|(_, read, _)| matches!(read.verdict, ModelVerdict::Due { .. }))
}

/// `aterm harness upgrade models`.
#[must_use]
pub fn models_report(opts: &Opts) -> String {
    use std::fmt::Write as _;
    let targets = Targets::read(&opts.home);
    let mut ctx = ModelCtx::read(opts, &targets);
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

/// `aterm harness upgrade models set <id>,<id>,...`.
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

pub(super) enum Terminated {
    Sent,
    Refused(String),
    #[cfg_attr(
        not(unix),
        expect(dead_code, reason = "only unix sends the signal itself")
    )]
    Failed,
}

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

/// One row of the wait log ([`St::waits`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WaitNote {
    pub at: u64,
    pub word: String,
    pub note: String,
}

/// How many [`WaitNote`]s a record keeps, newest last.
pub const WAITS_KEPT: usize = 24;

#[derive(Debug)]
pub(super) struct Unsent {
    phase: Phase,
    release: String,
    marker: String,
    markers: Vec<String>,
    fallback_from: Option<String>,
}

const FALLBACK_NO_MODEL: &str = "-";

pub(super) const SAME_PROGRAM: &str = "same";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct St {
    pub(super) phase: Phase,
    pub(super) from: String,
    pub(super) to: String,
    pub(super) source: String,
    pub(super) marker: String,
    pub(super) markers: Vec<String>,
    pub(super) release: String,
    pub(super) asked: Vec<String>,
    pub(super) salt: u64,
    pub(super) last_seq: u64,
    pub(super) seq_since_s: u64,
    pub(super) pid: u32,
    pub(super) notice_pid: u32,
    pub(super) notice_start: String,
    pub(super) shell: u32,
    pub(super) tab: String,
    pub(super) tabs: Vec<String>,
    pub(super) tabs_until: Vec<(String, u64)>,
    pub(super) line: String,
    pub(super) back_line: String,
    pub(super) prompt: Option<PromptMark>,
    pub(super) line_block: Option<u64>,
    pub(super) noted: String,
    pub(super) cause: String,
    pub(super) model_before: String,
    pub(super) mark: u64,
    pub(super) launch_model: String,
    pub(super) model_list: String,
    pub(super) fallback_from: Option<String>,
    pub(super) login: Option<String>,
    pub(super) home: Option<String>,
    pub(super) home_profile: Option<String>,
    pub(super) failed_start_kept: Option<FailedStart>,
    pub(super) confirm_by: u64,
    pub(super) look_word: String,
    pub(super) resumed_on: String,
    pub(super) resumed_pid: u32,
    pub(super) relaunched_pid: u32,
    pub(super) relaunched_start: String,
    pub(super) relaunched_at: u64,
    pub(super) moved_at: u64,
    pub(super) moved_login: Option<String>,
    pub(super) moved_left_model: Option<String>,
    pub(super) hold_since_s: u64,
    pub(super) hold_seen_s: u64,
    pub(super) ready_since: u64,
    pub(super) pending_since: u64,
    pub(super) proc_start: String,
    pub(super) waits: Vec<WaitNote>,
    pub(super) wait: String,
    pub(super) wait_since: u64,
    pub(super) wait_seen: u64,
    pub(super) held_by: Vec<HeldBy>,
    pub(super) idle_looks: u32,
    pub(super) idle_pid: u32,
    pub(super) idle_at: u64,
    pub(super) still_print: String,
    pub(super) still_since: u64,
    pub(super) still_at: u64,
    pub(super) request: Request,
    pub(super) request_tab: String,
    pub(super) request_at: u64,
    pub(super) outcome: String,
    pub(super) done_at: u64,
    pub(super) agent: upgrade::Agent,
    pub(super) thread: String,
    pub(super) codex_home: String,
    pub(super) codex_home_env: Option<String>,
    pub(super) argv: Vec<String>,
    pub(super) cwd: String,
    pub(super) twin: String,
    pub(super) muse_data: String,
    pub(super) muse_env: Vec<String>,
    pub(super) agy_config: String,
    pub(super) agy_env: Vec<String>,
    pub(super) gemini_chats: String,
    pub(super) gemini_env: Vec<String>,
    pub(super) gemini_child: u32,
    pub(super) resume_argv: Vec<String>,
    pub(super) resume_home: String,
    pub(super) resume_start: String,
    pub(super) left_typed: String,
    pub(super) exited_at: u64,
    pub(super) hand_seen_s: u64,
    pub(super) failed_at: u64,
    pub(super) noticed_at: u64,
    pub(super) last_stop: String,
    pub(super) streak_why: String,
    pub(super) stop_streak: u32,
    pub(super) notices: u32,
    pub(super) own_work_at: u64,
    pub(super) work_end_at: Vec<u64>,
    pub(super) goal: bool,
    pub(super) firsts: Vec<(String, u64)>,
    pub(super) progress_at: u64,
    pub(super) progress_key: String,
    pub(super) looked_at: u64,
    pub(super) looked_by: String,
    pub(super) point_at: u64,
    pub(super) guard: String,
    pub(super) refused: u32,
    pub(super) refused_at: u64,
    pub(super) work_seen: bool,
    pub(super) limit_until: u64,
    pub(super) limit_spend: bool,
    pub(super) quiet: bool,
    pub(super) restart_look: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Retarget {
    Kept,
    InPlace,
    Anew,
    Rested,
}

pub(super) const fn retarget_how(how: Retarget, st: &St) -> &'static str {
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

/// How often a look that sees the agent's own work restamps [`St::own_work_at`].
pub const OWN_WORK_RESTAMP_S: u64 = upgrade_status::OWN_WORK_HOLD_S / 4;

/// How often a look that finds a hand on a restart's tab RESTAMPS [`St::hand_seen_s`].
pub const HAND_RESTAMP_S: u64 = STALE_S / 5;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FailedStart {
    to: String,
    source: String,
    failed_at: u64,
    stop_streak: u32,
}

impl FailedStart {
    fn of(&self, to: &Candidate) -> bool {
        self.is(&to.version, to.source.as_str())
    }

    fn is(&self, version: &Version, source: &str) -> bool {
        self.source == source && Version::parse(&self.to).as_ref() == Some(version)
    }

    const fn rests(&self, now: u64) -> bool {
        now < self.failed_at.saturating_add(upgrade::RETRY_S)
    }

    fn word(&self) -> String {
        format!(
            "{} {} {} {}",
            self.to, self.source, self.failed_at, self.stop_streak
        )
    }

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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Proc<'a> {
    pub(super) start: &'a str,
    pub(super) started_s: u64,
}
/// The slack past a step's own deadline before the watch reads its record ([`due_by`]).
pub const WATCH_SLACK: u64 = 5 * 60;

/// The least time between two watches of one record ([`watch_at`]).
pub const WATCH_GAP: u64 = upgrade::REASK_S;

impl St {
    pub(super) fn agent_records(&self, user_home: &Path) -> Records {
        Records::of_recorded(
            user_home,
            self.home.as_deref(),
            self.home_profile.as_deref(),
        )
    }

    pub(super) fn keep_home(&mut self, records: &Records, user_home: &Path) -> bool {
        let (home, profile) = (records.home_word(user_home), records.profile_word());
        let changed = self.home != home || self.home_profile != profile;
        self.home = home;
        self.home_profile = profile;
        changed
    }

    pub(super) fn relaunched(&mut self, at_s: u64) {
        self.phase = Phase::Relaunched { at_s };
        self.relaunched_at = at_s;
    }

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

    fn legacy_progress_at(&self) -> u64 {
        let at = match &self.phase {
            Phase::Announced { at_s, .. }
            | Phase::Exiting { at_s }
            | Phase::Relaunched { at_s } => *at_s,
            Phase::Pending | Phase::Done | Phase::Failed(_) => 0,
        };
        at.max(self.failed_at).max(self.salt)
    }

    pub(super) const fn time_ready(&mut self, ready: bool, facts: &mut Facts, now: u64) {
        self.ready_since = upgrade::ready_since(self.ready_since, ready, facts, now);
        if self.ready_since != 0 {
            facts.ready_s = now.saturating_sub(self.ready_since);
        }
    }

    pub(super) const fn time_failed(&self, facts: &mut Facts, now: u64) {
        facts.failed_s = self.failed_for(now);
    }

    pub(super) fn time_work(&self, facts: &mut Facts, now: u64) {
        facts.work_ended = self.work_seen
            && self.open_round()
            && !facts.background_point
            && facts.background.is_empty()
            && self.work_end_open(now);
    }

    fn work_end_open(&self, now: u64) -> bool {
        let since = match &self.phase {
            Phase::Announced { at_s, .. } => now.saturating_sub((*at_s).max(self.noticed_at)),
            Phase::Failed(_) => now.saturating_sub(self.stopped_at()),
            _ => return false,
        };
        since >= upgrade::WORK_END_FLOOR_S && self.work_end_left(now) > 0
    }

    fn work_end_left(&self, now: u64) -> u32 {
        let spent = self
            .work_end_at
            .iter()
            .filter(|&&at| now.saturating_sub(at) < upgrade::WORK_END_WINDOW_S)
            .count();
        upgrade::WORK_END_DAILY.saturating_sub(u32::try_from(spent).unwrap_or(u32::MAX))
    }

    fn spend_work_end(&mut self, now: u64) {
        self.work_end_at
            .retain(|&at| now.saturating_sub(at) < upgrade::WORK_END_WINDOW_S);
        self.work_end_at.push(now);
        let keep = usize::try_from(upgrade::WORK_END_DAILY).unwrap_or(usize::MAX);
        if self.work_end_at.len() > keep {
            self.work_end_at.remove(0);
        }
    }

    fn open_round(&self) -> bool {
        matches!(&self.phase, Phase::Announced { .. })
            || matches!(&self.phase, Phase::Failed(why) if why == upgrade::GAVE_UP)
    }

    fn saw_work(&mut self, facts: &Facts) {
        if self.open_round() && (facts.background_point || !facts.background.is_empty()) {
            self.work_seen = true;
        }
    }

    pub(super) const fn failed_for(&self, now: u64) -> u64 {
        if matches!(self.phase, Phase::Failed(_)) {
            now.saturating_sub(self.failed_at)
        } else {
            0
        }
    }

    pub(super) fn promised_model(&self) -> Option<&str> {
        (matches!(self.phase, Phase::Announced { .. }) && !self.model_list.is_empty())
            .then_some(self.model_list.as_str())
    }

    pub(super) fn carried_model(&self) -> Option<&str> {
        let restarts = match &self.phase {
            Phase::Announced { .. } => true,
            Phase::Failed(why) => why == upgrade::GAVE_UP,
            _ => false,
        };
        (restarts && !self.model_list.is_empty()).then_some(self.model_list.as_str())
    }

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

    fn same_process(&self, running: &Version, who: Proc<'_>) -> bool {
        if !self.proc_start.is_empty() && !who.start.is_empty() {
            return self.proc_start == who.start;
        }
        Version::parse(&self.from).is_none_or(|from| from == *running)
    }

    pub(super) fn failed_start(&self) -> Option<FailedStart> {
        match self.agent {
            upgrade::Agent::Claude => {}
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

    fn keep_stop(&mut self, kept: FailedStart) {
        self.phase = Phase::Failed("no-resume".to_string());
        self.failed_at = kept.failed_at;
        "no-resume".clone_into(&mut self.streak_why);
        self.stop_streak = kept.stop_streak;
    }

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

    fn history(&self) -> History {
        History {
            streak_why: self.streak_why.clone(),
            stop_streak: self.stop_streak,
            notices: self.notices,
            waits: self.waits.clone(),
            own_work_at: self.own_work_at,
            firsts: self.firsts.clone(),
            work_end_at: self.work_end_at.clone(),
            goal: self.goal,
        }
    }

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
        let home = prior
            .as_ref()
            .map(|p| (p.home.clone(), p.home_profile.clone()));
        let (mut st, how) = match prior {
            Some(mut st)
                if st.to == to.version.to_string()
                    && (matches!(st.phase, Phase::Pending | Phase::Failed(_))
                        || st.model_list == model.unwrap_or_default())
                    && !(matches!(st.phase, Phase::Failed(_)) && !st.same_process(from, who)) =>
            {
                // Managed and native builds can share a version, and which one
                // the session moves to follows its launch: until the restart
                // begins, the label names the build the sweep would start now.
                if matches!(st.phase, Phase::Pending | Phase::Announced { .. }) {
                    st.source = to.source.as_str().to_string();
                }
                let same = st.same_process(from, who);
                if st.phase == Phase::Pending && !same {
                    st.pending_since = now;
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
                    st.pending_since = st.salt.max(who.started_s);
                }
                if !who.start.is_empty() && (same || st.phase == Phase::Pending) {
                    st.proc_start = who.start.to_string();
                }
                (st, Retarget::Kept)
            }
            Some(mut st)
                if matches!(st.phase, Phase::Pending | Phase::Announced { .. })
                    && st.model_list == model.unwrap_or_default()
                    && Version::parse(&st.from).as_ref() == Some(from)
                    && st.same_process(from, who) =>
            {
                st.to = to.version.to_string();
                st.source = to.source.as_str().to_string();
                if st.proc_start.is_empty() && !who.start.is_empty() {
                    st.proc_start = who.start.to_string();
                }
                let word = st.word_onto(to, now, true);
                (st.request, st.request_tab, st.request_at) = word;
                (st, Retarget::InPlace)
            }
            Some(mut old) if matches!(old.phase, Phase::Pending | Phase::Announced { .. }) => {
                old.owe_release("retargeted");
                let same = old.same_process(from, who);
                let pending_since = if same { old.behind_since() } else { now };
                let history = same.then(|| old.history());
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
            Some(old) if matches!(old.phase, Phase::Failed(_)) => {
                let same = old.same_process(from, who);
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

    pub(super) fn request_for(&self, tab: &str) -> Request {
        if self.request_tab.is_empty() || self.request_tab == tab {
            self.request.clone()
        } else {
            Request::None
        }
    }

    fn word_onto(&self, to: &Candidate, now: u64, same: bool) -> (Request, String, u64) {
        match &self.request {
            Request::Skip(_) if !self.request.holds_build(&to.version.to_string(), now) => {
                (Request::None, String::new(), 0)
            }
            Request::Now if !same => (Request::None, String::new(), 0),
            word => (word.clone(), self.request_tab.clone(), self.request_at),
        }
    }

    fn stop_kept_onto(&self) -> String {
        match self.agent {
            upgrade::Agent::Codex => {}
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

    pub(super) fn owners_hold(&self, tab: &str) -> Option<Request> {
        match self.request_for(tab) {
            word @ (Request::Skip(_) | Request::DeferUntil(_)) => Some(word),
            Request::None | Request::Now => None,
        }
    }

    pub(super) fn holds_off(&self, tab: &str, to: &Candidate, now: u64) -> bool {
        self.owners_hold(tab)
            .is_some_and(|word| word.holds_build(&to.version.to_string(), now))
            || self
                .failed_start()
                .is_some_and(|failed| failed.of(to) && failed.rests(now))
    }

    pub(super) fn retried_by(&self, st: &mut St) {
        let Some(build) = Version::parse(&st.to) else {
            return;
        };
        if let Some(failed) = self.failed_start().filter(|f| f.is(&build, &st.source)) {
            "no-resume".clone_into(&mut st.streak_why);
            st.stop_streak = failed.stop_streak;
        }
    }

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

    fn build_behind(&self) -> Option<Version> {
        Version::parse(if self.is_upgrade() {
            &self.from
        } else {
            &self.to
        })
    }

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

    pub(super) const fn behind_since(&self) -> u64 {
        if self.pending_since == 0 {
            self.salt
        } else {
            self.pending_since
        }
    }

    pub(super) fn rung_at(&self, now: u64) -> upgrade::Rung {
        match self.behind_since() {
            0 => upgrade::Rung::Prefer,
            since => upgrade::rung(now.saturating_sub(since)),
        }
    }

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

    pub(super) fn note_wait(&mut self, word: &str, now: u64) {
        if self.wait != word {
            self.wait = word.to_string();
            self.wait_since = now;
            self.log_wait(word, now);
        }
        self.wait_seen = if word == "attended" { now } else { 0 };
        if upgrade_status::holds_by_own_work(&self.wait, false) {
            self.saw_own_work(now);
        }
    }

    pub(super) const fn saw_hand(&mut self, now: u64) -> bool {
        let due = self.hand_seen_s == 0 || now.saturating_sub(self.hand_seen_s) >= HAND_RESTAMP_S;
        if due {
            self.hand_seen_s = now;
        }
        due
    }

    const fn saw_own_work(&mut self, now: u64) {
        if self.own_work_at == 0 || now.saturating_sub(self.own_work_at) >= OWN_WORK_RESTAMP_S {
            self.own_work_at = now;
        }
    }

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

    fn annotate_wait(&mut self, now: u64, note: &str) {
        if let Some(last) = self.waits.last_mut()
            && last.at == now
            && last.note.is_empty()
        {
            last.note = super::one_line(note, 240);
        }
    }

    fn note_step(&mut self, step: &str, status: &str, now: u64) {
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
            self.refused = 0;
            self.refused_at = 0;
        }
    }

    pub(super) fn held_past_go(&self, read: &str) -> bool {
        let act = read.split_once(AT_POINT).map_or(read, |(act, _)| act);
        if act == "would-announce" {
            refused(&format!("wait:{}", self.wait))
        } else {
            (act == "would-restart" || act.starts_with("would-restart:")) && self.restart_look
        }
    }

    fn note_held(&mut self, step: &str, held: &[upgrade::Held], now: u64) {
        if !step.starts_with("wait:") {
            self.held_by.clear();
            return;
        }
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
        let agent = match v.get("agent") {
            None => upgrade::Agent::Claude,
            Some(word) => upgrade::Agent::parse(word.as_str()?)?,
        };
        if super::lanes::lanes(agent.program()) == super::lanes::Lanes::NONE {
            return None;
        }
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
        if v.get("progress_key").is_none() {
            st.progress_key = st.progress_key();
            st.progress_at = st.legacy_progress_at();
        }
        if v.get("notices").is_none() {
            st.notices = upgrade_status::round_notices(&st.phase, &st.streak_why, st.stop_streak);
        }
        Some(st)
    }

    pub(super) const fn in_flight(&self) -> bool {
        matches!(self.phase, Phase::Exiting { .. } | Phase::Relaunched { .. })
    }

    pub(super) fn relaunched_gone(&self, alive: impl Fn(u32) -> bool) -> bool {
        self.relaunched_pid != 0 && !alive(self.relaunched_pid)
    }

    pub(super) fn close_overtaken(&mut self, why: &str, now: u64) {
        self.exited_at = 0;
        self.stop(why, now);
    }

    pub(super) fn landed_after_stop(&self, sf: &SessionFile, k: &dyn Kernel) -> bool {
        match self.agent {
            upgrade::Agent::Claude => {}
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

    fn started_by_line(&self, sf: &SessionFile) -> bool {
        let stop = self.stopped_at();
        stop != 0
            && models::parse_lstart(&sf.proc_start).is_some_and(|start| start <= stop)
            && (self.to.is_empty() || self.to == "-" || sf.version == self.to)
    }

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

    pub(super) fn relaunched_on_exit_of(&self, pid: u32) -> bool {
        self.pid == pid
            && (self.cause == super::relaunch::CAUSE_EXIT
                || self.cause == super::relaunch::CAUSE_FRESH)
    }

    pub(super) fn stuck(&self, now: u64) -> Option<&'static str> {
        upgrade_status::stuck_on(&self.phase, self.exited_at, now)
    }

    pub(super) fn is_upgrade(&self) -> bool {
        self.cause.is_empty() || self.cause == super::relaunch::CAUSE_UPGRADE_FRESH
    }

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

    pub(super) fn carried_through_hand(&mut self, now: u64) -> bool {
        let claude = match self.agent {
            upgrade::Agent::Claude => true,
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

    pub(super) fn confirming(&self) -> bool {
        self.phase == Phase::Done && self.confirm_by != 0
    }

    fn notice_belongs_to(&self, sf: &SessionFile, tab: &str) -> bool {
        self.tab == tab
            && self.notice_pid == sf.pid
            && !self.notice_start.is_empty()
            && self.notice_start == squash(&sf.proc_start)
    }

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

    fn hears_ready(&self) -> bool {
        match &self.phase {
            Phase::Announced { .. } => true,
            Phase::Failed(why) => why == upgrade::GAVE_UP,
            _ => false,
        }
    }

    pub(super) fn round_markers(&self) -> impl Iterator<Item = &String> {
        std::iter::once(&self.marker)
            .chain(&self.markers)
            .filter(|m| !m.is_empty())
    }

    pub(super) fn owe_release(&mut self, why: &str) {
        if self.release.is_empty() && self.notified() {
            self.release = why.to_string();
        }
    }

    pub(super) fn stop(&mut self, why: &str, now: u64) {
        self.owe_release(why);
        self.forget_markers();
        self.fail(why, now);
    }

    pub(super) fn fail(&mut self, why: &str, now: u64) {
        if self.streak_why == why {
            self.stop_streak = self.stop_streak.saturating_add(1);
        } else {
            why.clone_into(&mut self.streak_why);
            self.stop_streak = 1;
        }
        self.phase = Phase::Failed(why.to_string());
        self.failed_at = now.saturating_add(self.rest_extension());
    }

    pub(super) fn stopped_at(&self) -> u64 {
        if self.failed_at == 0 || !matches!(self.phase, Phase::Failed(_)) {
            return 0;
        }
        self.failed_at.saturating_sub(self.rest_extension())
    }

    fn rest_extension(&self) -> u64 {
        if self.streak_why == upgrade::GAVE_UP {
            upgrade::unanswered_rest_extension(self.stop_streak)
        } else {
            upgrade::rest_extension(self.stop_streak)
        }
    }

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
        self.back_line.clear();
        self.exited_at = 0;
        self.hand_seen_s = 0;
        back
    }

    pub(super) fn unsent(&mut self, back: Unsent) {
        self.phase = back.phase;
        self.release = back.release;
        self.marker = back.marker;
        self.markers = back.markers;
        self.fallback_from = back.fallback_from;
        self.back_line.clear();
    }

    pub(super) fn signal_failed(&mut self, back: Unsent, now: u64) {
        self.unsent(back);
        self.stop("signal-refused", now);
    }

    fn give_up(&mut self, now: u64) {
        self.owe_release("gave-up");
        self.fail(upgrade::GAVE_UP, now);
        if self.request == Request::Now {
            self.request = Request::None;
            self.request_tab.clear();
            self.request_at = 0;
        }
    }

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

    fn spend_ask(&mut self, asks: u32, marker: &str) {
        let before = match self.phase {
            Phase::Announced { asks, .. } => asks,
            _ => 0,
        };
        if asks > before && !self.asked.iter().any(|m| m == marker) {
            self.notices = self.notices.saturating_add(1);
        }
    }

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
        self.quiet = false;
        self.release.clear();
        self.ready_since = 0;
        self.last_stop.clear();
        self.work_seen = false;
    }

    fn released(&mut self) {
        self.release.clear();
        self.forget_markers();
    }

    fn dropped(&mut self) {
        self.release.clear();
        self.forget_markers();
    }

    fn notice_gone(&mut self, tab: &str, now: u64) -> String {
        let dropped = std::mem::take(&mut self.release);
        self.forget_markers();
        self.asked.clear();
        self.new_round(now);
        self.phase = Phase::Pending;
        self.notice_pid = 0;
        self.notice_start.clear();
        self.noted.clear();
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

    pub(super) fn forget_markers(&mut self) {
        self.marker.clear();
        self.markers.clear();
        self.ready_since = 0;
    }
}

pub(super) fn state_dir(opts: &Opts) -> PathBuf {
    opts.state.join("upgrade")
}

/// Small regular record files; a FIFO must never park the synchronous upgrade watcher.
fn read_record_text(path: &Path) -> std::io::Result<Option<String>> {
    super::accounts::read_bounded(path, 2 * 1024 * 1024)
}

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

pub(super) const STATE_KEEP_S: u64 = 7 * 24 * 60 * 60;

pub(super) fn settled(st: &St) -> bool {
    match st.phase {
        Phase::Done | Phase::Failed(_) => true,
        Phase::Pending => st.request == Request::None,
        _ => false,
    }
}

const fn driven_here(agent: upgrade::Agent) -> bool {
    match agent {
        upgrade::Agent::Claude => true,
        upgrade::Agent::Codex
        | upgrade::Agent::Muse
        | upgrade::Agent::Agy
        | upgrade::Agent::Gemini => false,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TabLife {
    Live,
    Gone,
    Unknown,
}

pub(super) fn registry_life<'a>(
    graph: Option<&'a Path>,
    known_live: &'a [String],
) -> impl Fn(&str) -> TabLife + 'a {
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

pub(super) const fn registry_vouches_for_state(
    private_registry: bool,
    private_state: bool,
) -> bool {
    private_registry == private_state
}

pub(super) fn registry_graph() -> Option<PathBuf> {
    registry_vouches_for_state(
        aterm_uds::control_socket_dir_is_private(),
        aterm_types::dirs::runs_under_private_state_root(),
    )
    .then(aterm_uds::control_socket_dir)
    .flatten()
    .map(|dir| dir.join("graph"))
}

pub(super) fn sat_a_week(entry: &std::fs::DirEntry, now: u64) -> bool {
    entry
        .metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
        .is_some_and(|at| now.saturating_sub(at.as_secs()) >= STATE_KEEP_S)
}

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
        if name.is_some_and(|name| super::restart_stamp::prune(&opts.state, name, now)) {
            continue;
        }
        if let Some(id) = name.and_then(|name| name.strip_suffix(".writes")) {
            if !live.contains(id) && note_names_nothing(&path, id) {
                let _ = std::fs::remove_file(&path);
            }
            continue;
        }
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
        let Some(st) = read_record_text(&path)
            .ok()
            .flatten()
            .and_then(|text| St::from_json(&text))
        else {
            continue;
        };
        let held_elsewhere = st.home.is_some()
            && st
                .agent_records(&opts.home)
                .session_files()
                .is_none_or(|files| conversation_live(&files, id));
        if driven_here(st.agent) && settled(&st) && !held_elsewhere {
            remove(opts, id);
        }
    }
}

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

pub(super) fn save(opts: &Opts, session: &str, st: &St) {
    save_at(opts, session, st, now_s());
}

pub(super) fn save_checked(opts: &Opts, session: &str, st: &St) -> std::io::Result<()> {
    record::save_at_checked(opts, session, st, now_s())
}

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

pub(super) fn build_proved(
    opts: &Opts,
    r: &Report,
    to: &Candidate,
    now: u64,
) -> Result<(), &'static str> {
    build_start::proved(opts, r, to, now)
}

mod record {
    use super::{Opts, St, state_dir};
    use std::path::{Path, PathBuf};

    fn replace_text(path: &Path, text: &str) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            use std::io::Write as _;
            let (tmp, mut file) = aterm_uds::atomic_file::create_temp_beside(path, 0o666)?;
            let written = file.write_all(text.as_bytes());
            drop(file);
            let result = written.and_then(|()| std::fs::rename(&tmp, path));
            if result.is_err() {
                let _ = std::fs::remove_file(&tmp);
            }
            result
        }
        #[cfg(not(unix))]
        {
            let mut tmp = path.as_os_str().to_os_string();
            tmp.push(".tmp");
            let tmp = PathBuf::from(tmp);
            std::fs::write(&tmp, text)?;
            std::fs::rename(&tmp, path)
        }
    }

    fn state_path(opts: &Opts, session: &str) -> PathBuf {
        state_dir(opts).join(format!("{session}.json"))
    }

    fn move_path(opts: &Opts, session: &str) -> PathBuf {
        state_dir(opts).join(format!("{session}.move"))
    }

    pub(in super::super) fn kept_move(opts: &Opts, session: &str) -> Option<St> {
        St::from_json(&super::read_record_text(&move_path(opts, session)).ok()??)
            .filter(|st| st.moved_at != 0)
    }

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
        let _ = replace_text(&path, &kept.to_json());
    }

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
        load_checked(opts, session).ok().flatten()
    }

    fn load_checked(opts: &Opts, session: &str) -> std::io::Result<Option<St>> {
        Ok(super::read_record_text(&state_path(opts, session))?
            .and_then(|text| St::from_json(&text)))
    }

    pub(in super::super) fn save_at(opts: &Opts, session: &str, st: &St, now: u64) {
        let _ = save_at_with(opts, session, st, now, |opts, session, out| {
            write_verbatim(opts, session, out);
            Ok(())
        });
    }

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
        let on_disk = load_checked(opts, session)?;
        let mut out = st.clone();
        match &on_disk {
            Some(disk) if disk.progress_key == key => out.progress_at = disk.progress_at,
            _ => out.progress_at = now,
        }
        out.progress_key = key;
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
        super::build_start::note(opts, session, on_disk.as_ref(), st, now);
        Ok(())
    }

    fn write_verbatim(opts: &Opts, session: &str, st: &St) {
        if opts.dry_run {
            return;
        }
        let path = state_path(opts, session);
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = replace_text(&path, &st.to_json());
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

    pub(in super::super) fn restore(opts: &Opts, session: &str, prior: Option<&St>) {
        if opts.dry_run {
            return;
        }
        match prior {
            Some(st) => write_verbatim(opts, session, st),
            None => remove(opts, session),
        }
    }

    pub(in super::super) fn remove(opts: &Opts, session: &str) {
        if opts.dry_run {
            return;
        }
        if let Some(disk) = load(opts, session) {
            keep_move(opts, session, &disk);
        }
        let _ = std::fs::remove_file(state_path(opts, session));
    }

    #[cfg(all(test, unix))]
    pub(in super::super) fn write_verbatim_for_test(opts: &Opts, session: &str, st: &St) {
        write_verbatim(opts, session, st);
    }
}

#[cfg(all(test, unix))]
use record::write_verbatim_for_test as write_verbatim;
pub(super) use record::{kept_move, load, remove, restore, save_at};

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
    let _ = super::cli::bound_ledger(&path, LEDGER_MAX_BYTES, LEDGER_KEEP_ROWS);
}

/// `<state>/upgrade/ledger.jsonl` — what `aterm harness ledger upgrade` reads.
#[must_use]
pub fn ledger_path(opts: &Opts) -> PathBuf {
    state_dir(opts).join("ledger.jsonl")
}

const LEDGER_MAX_BYTES: u64 = super::cli::DISK_LEDGER_MAX_BYTES;

const LEDGER_KEEP_ROWS: usize = super::cli::DISK_LEDGER_KEEP_ROWS;

#[cfg(test)]
thread_local! {
    static SESSION_FILE_SCANS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub(super) fn session_files(home: &Path) -> Option<Vec<SessionFile>> {
    session_files_in(&home.join(".claude"))
}

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
        let file = read_record_text(&path)
            .ok()
            .flatten()
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

pub(super) const SESSION_ROSTER_RESCAN: Duration = Duration::from_secs(1);

#[derive(Default)]
pub(super) struct SessionRosterCache {
    stamp: Option<SystemTime>,
    scanned_at: Option<Instant>,
    files: Option<Vec<SessionFile>>,
}

impl SessionRosterCache {
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
            self.stamp = stamp;
            self.scanned_at = Some(now);
            self.files = scan();
        }
        self.files.as_deref()
    }
}

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

fn preliminary_unique_owner(
    files: Option<&[SessionFile]>,
    wanted: &SessionFile,
) -> Result<(), &'static str> {
    let files = files.ok_or("session-files-unreadable")?;
    unique_live_owner(files, wanted)
        .then_some(())
        .ok_or("conversation-owner-ambiguous")
}

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

#[derive(Clone, Debug, PartialEq, Eq)]
struct HostClaim {
    tab: String,
    group: i64,
    parent: u32,
}

type Claimed<'a> = (&'a SessionFile, atpkg::caller_shell::ProcArgs, HostClaim);

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

pub(super) fn session_file_in(claude_dir: &Path, pid: u32) -> Option<SessionFile> {
    let t = read_record_text(&claude_dir.join(format!("sessions/{pid}.json"))).ok()??;
    let file = upgrade::parse_session_file(&t).ok()?;
    Some(SessionFile {
        claude_dir: Some(claude_dir.to_path_buf()),
        ..file
    })
}

pub(super) fn transcript_exists(home: &Path, session: &str) -> Option<bool> {
    transcript_exists_in(&home.join(".claude"), session)
}

pub(super) fn transcript_exists_in(claude_dir: &Path, session: &str) -> Option<bool> {
    let name = format!("{session}.jsonl");
    match std::fs::read_dir(claude_dir.join("projects")) {
        Ok(dirs) => Some(dirs.flatten().any(|e| e.path().join(&name).is_file())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && claude_dir.is_dir() => Some(false),
        Err(_) => None,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Records {
    dir: Option<PathBuf>,
    profile: Option<PathBuf>,
}

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

pub(super) fn profile_in(user_home: &Path, vars: HomeVars<'_>, dir: &Path) -> PathBuf {
    profile_apart(user_home, vars, Some(dir)).unwrap_or_else(|| claude_profile(user_home, dir))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct HomeVars<'a> {
    pub(super) config: Option<&'a str>,
    pub(super) home: Option<&'a str>,
}

impl<'a> HomeVars<'a> {
    pub(super) fn of(args: &'a atpkg::caller_shell::ProcArgs) -> Self {
        Self {
            config: args.env_var(super::accounts::HOME_ENV),
            home: args.env_var(super::accounts::HOMEDIR_ENV),
        }
    }

    pub(super) const fn config(config: Option<&'a str>) -> Self {
        Self { config, home: None }
    }
}

fn homedir(user_home: &Path, home: Option<&str>) -> PathBuf {
    home.map(Path::new)
        .filter(|home| home.is_absolute())
        .map_or_else(|| user_home.to_path_buf(), Path::to_path_buf)
}

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

pub(super) fn same_dir(a: &Path, b: &Path) -> bool {
    a == b || std::fs::canonicalize(a).is_ok_and(|a| std::fs::canonicalize(b).is_ok_and(|b| a == b))
}

pub(super) fn home_dir_at(home: &str, cwd: impl FnOnce() -> Option<PathBuf>) -> Option<PathBuf> {
    let base = if Path::new(home).is_absolute() {
        PathBuf::from("/")
    } else {
        cwd().filter(|cwd| cwd.is_absolute())?
    };
    claude_dir_at(Some(home), None, &base, || None).ok()
}

impl Records {
    pub(super) fn at(
        user_home: &Path,
        vars: HomeVars<'_>,
        cwd: impl FnOnce() -> Option<PathBuf>,
    ) -> Self {
        let dir = claude_home_dir(Some(user_home), vars, cwd);
        Self::in_dir(user_home, vars, dir)
    }

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

    fn in_dir(user_home: &Path, vars: HomeVars<'_>, dir: Option<PathBuf>) -> Self {
        Self {
            profile: profile_apart(user_home, vars, dir.as_deref()),
            dir,
        }
    }

    pub(super) fn of(user_home: &Path, home: Option<&str>) -> Self {
        Self::at(user_home, HomeVars::config(home), || None)
    }

    pub(super) fn of_recorded(user_home: &Path, home: Option<&str>, profile: Option<&str>) -> Self {
        let of = Self::of(user_home, home);
        Self {
            profile: profile.map(PathBuf::from).or(of.profile),
            dir: of.dir,
        }
    }

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

    pub(super) fn home_word(&self, user_home: &Path) -> Option<String> {
        self.dir
            .as_ref()
            .filter(|dir| **dir != user_home.join(".claude"))
            .map(|dir| dir.to_string_lossy().into_owned())
    }

    pub(super) fn profile_word(&self) -> Option<String> {
        self.profile
            .as_ref()
            .map(|profile| profile.to_string_lossy().into_owned())
    }

    pub(super) fn session_file(&self, pid: u32) -> Option<SessionFile> {
        session_file_in(self.dir.as_deref()?, pid).map(|sf| self.stamped(sf))
    }

    pub(super) fn session_files(&self) -> Option<Vec<SessionFile>> {
        session_files_in(self.dir.as_deref()?)
            .map(|files| files.into_iter().map(|sf| self.stamped(sf)).collect())
    }

    fn stamped(&self, sf: SessionFile) -> SessionFile {
        SessionFile {
            profile: self.profile.clone(),
            ..sf
        }
    }

    pub(super) fn profile(&self, user_home: &Path) -> Option<PathBuf> {
        self.profile.clone().or_else(|| {
            self.dir
                .as_deref()
                .map(|dir| claude_profile(user_home, dir))
        })
    }

    pub(super) fn billing_context(
        &self,
        user_home: &Path,
        args: &atpkg::caller_shell::ProcArgs,
    ) -> Option<super::profile::Context> {
        let dir = self.dir()?;
        super::profile::Context::of_exec_env(user_home, dir, &args.env)
    }

    pub(super) fn transcript(&self, session: &str) -> Option<PathBuf> {
        transcript_in(self.dir.as_deref()?, session)
    }

    pub(super) fn transcript_exists(&self, session: &str) -> Option<bool> {
        transcript_exists_in(self.dir.as_deref()?, session)
    }

    pub(super) fn unique_owner(&self, wanted: &SessionFile) -> Result<(), &'static str> {
        preliminary_unique_owner(self.session_files().as_deref(), wanted)
    }

    pub(super) fn tasked(&self, session: &str, ours: &[approvals::Typed]) -> Option<bool> {
        Tasked::default().of_in(self.dir.as_deref()?, session, ours)
    }

    pub(super) fn finished(&self, session: &str, ours: &[approvals::Typed]) -> Option<bool> {
        self.finished_in(&LOOK_MEMOS, session, ours)
    }

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

    pub(super) fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }
}

/// Whether conversation `session` has a task ([`upgrade::TaskScan`]), read once.
#[must_use]
pub fn tasked(home: &Path, session: &str, ours: &[approvals::Typed]) -> Option<bool> {
    Tasked::default().of(home, session, ours)
}

/// What the supervisor of tab `tab` typed into it.
#[must_use]
pub fn supervisor_typed(opts: &Opts, tab: &str) -> Vec<approvals::Typed> {
    conversation_typed(opts, tab, None, &[])
}

pub(super) const EARLIER_TABS: usize = 8;

/// What the harness typed into a conversation, keyed to the conversation rather than its current tab.
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

fn stamp_tab(stamps: &mut Vec<(String, u64)>, tab: &str, until: u64) {
    match stamps.iter_mut().find(|(t, _)| t == tab) {
        Some((_, at)) => *at = (*at).max(until),
        None => stamps.push((tab.to_string(), until)),
    }
}

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

/// [`lefts_through`] for a LOOP that keeps its ledger at `ledger` for session `sid`.
#[must_use]
pub fn lefts_beside(ledger: &Path, sid: Option<&str>) -> Vec<String> {
    let read = |p: &Path| std::fs::read_to_string(p).ok();
    lefts_beside_in(ledger, &read(ledger).unwrap_or_default(), sid, &read)
}

/// [`lefts_beside`] for a loop that read its own ledger at `ledger` once and reads every file beside it through `read`.
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

/// [`lefts_beside_in`] over the earlier tabs' ledgers already read (`earlier`: each tab and its ledger's text).
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

/// The earlier tabs' ledgers beside the loop ledger at `ledger` of session `sid`.
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

fn earlier_tabs_path(root: &Path, tab: &str) -> PathBuf {
    let ledger = approvals::ledger_under(root, Some(tab));
    let stem = ledger
        .file_stem()
        .map_or_else(|| "self".into(), |s| s.to_string_lossy().into_owned());
    ledger.with_file_name(format!("{stem}.earlier-tabs"))
}

fn noted_earlier_tabs(root: &Path, tab: &str) -> Vec<String> {
    noted_earlier_tabs_read(root, tab, &|p| std::fs::read_to_string(p).ok())
}

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

/// Note that a cold restore reopened tab `tab` from tab `was`.
pub fn note_earlier_tab(opts: &Opts, tab: &str, was: &str) {
    if let Some(root) = opts.aterm_state.as_deref() {
        note_earlier_tab_under(root, tab, was);
    }
}

pub(crate) fn note_earlier_tab_under(root: &Path, tab: &str, was: &str) {
    if !super::tap::is_tab_id(tab) || !super::tap::is_tab_id(was) || was == tab {
        return;
    }
    let Some(body) = earlier_tab_note(tab, was, noted_earlier_tabs(root, tab)) else {
        return;
    };
    let _ = models::write_atomic_ordered(&earlier_tabs_path(root, tab), &body);
}

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

/// [`tasked`] kept up for one conversation at a time, read on from where the last read ended.
#[derive(Clone, Debug, Default)]
pub struct Tasked {
    session: String,
    path: Option<PathBuf>,
    read_to: u64,
    scan: upgrade::TaskScan,
}

const TASK_READ_BYTES: u64 = 8 << 20;

impl Tasked {
    /// [`tasked`], for `session`.
    pub fn of(&mut self, home: &Path, session: &str, ours: &[approvals::Typed]) -> Option<bool> {
        self.of_in(&home.join(".claude"), session, ours)
    }

    /// [`Tasked::of`] of the conversation the relaunch snapshot `snap` names.
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
        let whole = buf.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
        buf.truncate(whole);
        self.read_to = self.read_to.saturating_add(whole as u64);
        for line in lossy(buf).lines() {
            if self.scan.line(line) && self.scan.settled() {
                return Some(true);
            }
        }
        if self.scan.tasked() {
            return Some(true);
        }
        at_end.then_some(false)
    }
}

/// Whether conversation `session`'s task is finished by its own record ([`upgrade::FinishedScan`]).
#[derive(Clone, Debug, Default)]
pub struct Finished {
    session: String,
    path: Option<PathBuf>,
    dir: Option<PathBuf>,
    begun: bool,
    read_to: u64,
    at_end: bool,
    scan: upgrade::FinishedScan,
}

impl Finished {
    /// `Some(finished)` for `session` by the record of the agent the relaunch snapshot `snap` names.
    pub fn of_snapshot(
        &mut self,
        user_home: &Path,
        snap: &super::relaunch::Snapshot,
        ours: &[approvals::Typed],
    ) -> Option<bool> {
        self.of_snapshot_at(user_home, snap, ours, || live_cwd(snap))
    }

    /// The turn the conversation ends on.
    #[must_use]
    pub fn last_turn_key(&self) -> Option<String> {
        self.at_end.then(|| self.scan.last_turn_key()).flatten()
    }

    /// The turn the conversation ends on.
    #[must_use]
    pub fn last_turn(&self) -> Option<upgrade::LastTurn> {
        self.at_end.then(|| self.scan.last_turn())
    }

    /// The Claude Code directory this memo last read its conversation in.
    #[must_use]
    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

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

    #[cfg(all(test, unix))]
    pub(super) fn of_in(
        &mut self,
        claude_dir: &Path,
        session: &str,
        ours: &[approvals::Typed],
    ) -> Option<bool> {
        self.of_in_from(claude_dir, session, ours, TAIL_BYTES)
    }

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

fn writes_note_path(opts: &Opts, session: &str) -> PathBuf {
    state_dir(opts).join(format!("{session}.writes"))
}

/// Record the directory a moved agent writes its conversation in while it lives.
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

fn repo_root(dir: &Path) -> Option<&Path> {
    dir.ancestors()
        .take(REPO_ANCESTORS)
        .find(|d| std::fs::symlink_metadata(d.join(".git")).is_ok())
}

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

const REPO_ANCESTORS: usize = 32;

const REPO_WORKTREES: usize = 64;

fn newest_row_at(path: &Path) -> Option<u64> {
    let (tail, _) = tail_to_end(path, TAIL_BYTES);
    tail.lines().rev().find_map(|line| {
        let v: Value = aterm_json::from_str(line).ok()?;
        models::parse_utc(v.get("timestamp")?.as_str()?)
    })
}

pub(super) fn finished_where_written(
    opts: &Opts,
    records: &Records,
    session: &str,
    ours: &[approvals::Typed],
) -> Option<bool> {
    WhereWritten::new(opts, records, session, ours).finished()
}

pub(super) struct WhereWritten<'a> {
    opts: &'a Opts,
    records: &'a Records,
    session: &'a str,
    ours: &'a [approvals::Typed],
    scan: std::cell::OnceCell<Option<upgrade::FinishedScan>>,
}

impl<'a> WhereWritten<'a> {
    pub(super) const fn new(
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

    pub(super) const fn records(&self) -> &'a Records {
        self.records
    }

    pub(super) const fn session(&self) -> &'a str {
        self.session
    }

    pub(super) const fn ours(&self) -> &'a [approvals::Typed] {
        self.ours
    }

    fn scan(&self) -> Option<&upgrade::FinishedScan> {
        self.scan
            .get_or_init(|| read_where_written(self.opts, self.records, self.session, self.ours))
            .as_ref()
    }

    pub(super) fn finished(&self) -> Option<bool> {
        self.scan().map(upgrade::FinishedScan::finished)
    }

    pub(super) fn said_done(&self) -> Option<(bool, bool)> {
        self.scan()
            .map(|scan| (scan.said_done(), scan.said_irreversible()))
    }
}

pub(super) fn turn_key_where_written(
    opts: &Opts,
    records: &Records,
    session: &str,
) -> Option<upgrade::LastTurn> {
    Some(read_where_written(opts, records, session, &[])?.last_turn())
}

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

static LOOK_MEMOS: std::sync::Mutex<Vec<Finished>> = std::sync::Mutex::new(Vec::new());

const LOOK_MEMOS_KEPT: usize = 64;

pub(super) const TAIL_BYTES: u64 = 262_144;

pub(super) const FINISHED_ONCE_BYTES: u64 = TASK_READ_BYTES / 2;

#[cfg(all(test, unix))]
pub(super) fn transcript(home: &Path, session: &str) -> Option<PathBuf> {
    transcript_in(&home.join(".claude"), session)
}

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

fn snapshot_tail(user_home: &Path, snap: &super::relaunch::Snapshot) -> Option<String> {
    snapshot_tail_at(user_home, snap, || live_cwd(snap))
}

pub(super) fn snapshot_tail_at(
    user_home: &Path,
    snap: &super::relaunch::Snapshot,
    cwd: impl FnOnce() -> Option<PathBuf>,
) -> Option<String> {
    let session = snap.session.as_deref()?;
    let path = transcript_in(&conversation_dir(user_home, snap, session, cwd)?, session)?;
    Some(tail_to_end(&path, TAIL_BYTES).0)
}

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

fn live_cwd(snap: &super::relaunch::Snapshot) -> Option<PathBuf> {
    (kernel_start(snap.pid).as_deref() == Some(snap.start.as_str()))
        .then(|| aterm_sysprobe::process_cwd(snap.pid))
        .flatten()
}

/// Read the standing usage-limit reset timestamp for `snap`'s conversation.
#[must_use]
pub fn named_limit_reset(user_home: &Path, snap: &super::relaunch::Snapshot) -> Option<u64> {
    upgrade::transcript_limit_reset(&snapshot_tail(user_home, snap)?)
}

/// The tail of the transcript Claude Code process `pid` writes now.
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

/// [`process_transcript_tail`] with the file it read, and more of its end ([`super::result::TRANSCRIPT_READ_BYTES`]).
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

fn process_transcript_end(
    (config_dir, home, started_in): (Option<&str>, Option<&str>, Option<&str>),
    pid: u32,
    start: &str,
    bytes: u64,
) -> Option<(PathBuf, String)> {
    let cwd = aterm_sysprobe::process_cwd(pid);
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
        (Some(_), None) => PathBuf::new(),
        (None, None) => return None,
    };
    let filed = Records::of_process(&user_home, vars, pid, || cwd.clone(), started_in);
    let writes = Records::at(&user_home, vars, || cwd.clone());
    let (filed, writes) = (filed.dir()?.to_path_buf(), writes.dir()?.to_path_buf());
    let started = models::parse_lstart(start)?;
    let entry = super::footer::session_of_pid(&filed, pid, Some(started))?;
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

/// [`process_transcript_tail`] of the Claude Code `snap` names.
#[must_use]
pub fn snapshot_transcript_tail(snap: &super::relaunch::Snapshot) -> Option<String> {
    snapshot_transcript_end(snap, TAIL_BYTES).map(|(_, tail)| tail)
}

/// [`snapshot_transcript_tail`] with the file it read, and as much of its end as [`process_transcript`] reads.
#[must_use]
pub fn snapshot_transcript(snap: &super::relaunch::Snapshot) -> Option<(PathBuf, String)> {
    snapshot_transcript_end(snap, super::result::TRANSCRIPT_READ_BYTES)
}

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

/// Read the standing quota wall for `snap`'s conversation.
#[must_use]
pub fn named_standing_wall(
    user_home: &Path,
    snap: &super::relaunch::Snapshot,
) -> Option<upgrade::StandingWall> {
    upgrade::transcript_standing_wall(&snapshot_tail(user_home, snap)?)
}

/// When the newest answer of the conversation `snap` names came.
#[must_use]
pub fn named_last_answer(
    user_home: &Path,
    snap: &super::relaunch::Snapshot,
    by: u64,
) -> Option<u64> {
    upgrade::transcript_last_answer(&snapshot_tail(user_home, snap)?, by)
}

/// Read the final usage-limit dialog reset line for `snap`'s conversation.
#[must_use]
pub fn named_reset_line(
    user_home: &Path,
    snap: &super::relaunch::Snapshot,
    since: u64,
) -> Option<String> {
    upgrade::transcript_reset_line(&snapshot_tail(user_home, snap)?, since)
}

/// Whether conversation `session` is kept under `home`.
#[must_use]
pub fn conversation_under(home: &Path, session: &str) -> bool {
    upgrade::is_session_id(session) && transcript_exists(home, session) == Some(true)
}

pub(super) fn tail_to_end(path: &Path, bytes: u64) -> (String, u64) {
    let Ok(mut f) = std::fs::File::open(path) else {
        return (String::new(), 0);
    };
    let len = f.metadata().map_or(0, |m| m.len());
    tail_window(&mut f, len, bytes)
}

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

/// One sweep.
#[must_use]
pub fn sweep(opts: &Opts) -> Vec<Report> {
    sweep_with_roster(opts, None)
}

/// The window's step for one tab (`opts.only_sid` on the instance `opts.sock`).
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

/// Whether tab `opts.only_sid` has an upgrade to take.
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
    if status_line(&mut c, tab).is_some_and(|l| self_claimed_by_status(&l)) {
        return Due::No;
    }
    let Some(tabs) = host_roster(&mut c) else {
        return Due::Unread(UNREAD_SOCKET);
    };
    due_after_lanes(opts, &tabs, &|agent| lane_due(opts, &tabs, agent))
}

fn lane_due(opts: &Opts, tabs: &[LiveTab], agent: upgrade::Agent) -> Option<Due> {
    match agent {
        upgrade::Agent::Codex => None,
        upgrade::Agent::Claude => None,
        upgrade::Agent::Muse => muse::live::upgrade_due(opts, tabs),
        upgrade::Agent::Agy => agy::live::upgrade_due(opts, tabs),
        upgrade::Agent::Gemini => None,
    }
}

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
        Due::Unread(UNREAD_RECORD) if homes.unnamed(tab) => Due::Unread(UNREAD_PROCESS),
        Due::Unread(UNREAD_RECORD) => no_record(tabs, tab, started_s, alive, now_s()),
        due => due,
    }
}

fn claude_records(home: &Path) -> Option<Vec<SessionFile>> {
    session_files(home).or_else(|| (!home.join(".claude/sessions").is_dir()).then(Vec::new))
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Homes {
    dirs: Vec<Records>,
    unnamed: Vec<String>,
}

impl Homes {
    pub(super) fn read(
        opts: &Opts,
        tabs: Option<&[LiveTab]>,
        mut read_args: impl FnMut(u32) -> Option<atpkg::caller_shell::ProcArgs>,
        mut read_cwd: impl FnMut(u32) -> Option<PathBuf>,
    ) -> Self {
        let mut homes = Homes::default();
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
            let unnamed = |homes: &mut Homes| {
                if !learned && alive(leader) {
                    homes.unnamed.push(tab.sid.clone());
                }
            };
            let Some(args) = read_args(leader) else {
                unnamed(&mut homes);
                continue;
            };
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

    pub(super) fn of_records(opts: &Opts, sessions: &std::collections::BTreeSet<String>) -> Self {
        let mut homes = Homes::default();
        for session in sessions {
            if let Some(st) = load(opts, session).filter(|st| st.home.is_some()) {
                homes.add(&opts.home, st.agent_records(&opts.home));
            }
        }
        homes
    }

    #[cfg(all(test, unix))]
    pub(super) fn dirs(&self) -> Vec<PathBuf> {
        self.dirs
            .iter()
            .filter_map(Records::dir)
            .map(Path::to_path_buf)
            .collect()
    }

    pub(super) fn unnamed(&self, sid: &str) -> bool {
        self.unnamed.iter().any(|tab| tab == sid)
    }

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

fn recorded_homes(opts: &Opts) -> Vec<Records> {
    let Ok(dir) = std::fs::read_dir(state_dir(opts)) else {
        return Vec::new();
    };
    dir.flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| St::from_json(&read_record_text(&e.path()).ok()??))
        .filter(|st| driven_here(st.agent) && st.home.is_some())
        .map(|st| st.agent_records(&opts.home))
        .collect()
}

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

#[derive(Debug, PartialEq, Eq)]
enum TabAgent<'a> {
    Claimed(&'a SessionFile),
    Nothing,
    Unread,
    NoRecord,
}

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

/// How long a tab's agent may run with no record of its own before a look takes that as final ([`no_record`]).
pub const RECORD_WINDOW_S: u64 = 30;

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

pub(super) fn started_s(pid: u32) -> Option<u64> {
    models::parse_lstart(&kernel_start(pid)?)
}

/// What [`due`] read of a tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Due {
    Yes,
    No,
    Unread(&'static str),
}

/// [`Due::Unread`]'s word for a look the instance's socket did not answer.
pub const UNREAD_SOCKET: &str = "no-socket";
/// [`Due::Unread`]'s word for a look that found no record of the tab's agent among Claude Code's session records while it may still be written.
pub const UNREAD_RECORD: &str = "no-record";
/// [`Due::Unread`]'s word for a look that could not read the tab agent's process whole, for Claude Code and Codex alike.
pub const UNREAD_PROCESS: &str = "no-process";
/// [`Due::Unread`]'s word for a look that could not read Claude Code's session records whole.
pub const UNREAD_FILES: &str = "session-files-unreadable";
/// `Due::Wait` reason when a behind agent has moved away from its recorded home.
pub const HOME_MOVED: &str = "home-moved";
/// `Due::Unread` reason when no newer agent version is available.
pub const UNREAD_VERSION: &str = "no-version";

/// Mints the session's upgrade state the moment its host sees it behind.
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

fn lane_noted(opts: &Opts, tabs: &[LiveTab], agent: upgrade::Agent, _since: u64) -> Option<Behind> {
    match agent {
        upgrade::Agent::Codex => None,
        upgrade::Agent::Claude => None,
        upgrade::Agent::Muse => muse::live::leads(opts, tabs).then_some(Behind::Nothing),
        upgrade::Agent::Agy => agy::live::leads(opts, tabs).then_some(Behind::Nothing),
        upgrade::Agent::Gemini => None,
    }
}

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
    Noted,
    Nothing,
    Busy,
    Unread(&'static str),
}

pub(super) fn behind_anew(st: &St, to: &Version) -> bool {
    matches!(st.phase, Phase::Done | Phase::Failed(_))
        && !st.confirming()
        && Version::parse(&st.to).as_ref() != Some(to)
}

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

fn moved_tabs(st: &St, tab: &str) -> bool {
    st.tab != tab && matches!(st.phase, Phase::Pending | Phase::Failed(_))
}

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

fn rested_said(opts: &Opts, r: &Report, st: &St, now: u64) {
    if opts.dry_run {
        return;
    }
    let Phase::Failed(why) = &st.phase else {
        return;
    };
    save(opts, &r.session, st);
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
    NextIdle,
    Later(std::time::Duration),
    Finished,
    Broken,
}

/// The pauses before consecutive looks at a session whose step waited.
pub const LATER: [std::time::Duration; 4] = [
    std::time::Duration::from_secs(upgrade::QUIET_S),
    std::time::Duration::from_secs(60),
    std::time::Duration::from_secs(300),
    std::time::Duration::from_secs(600),
];

/// [`LATER`]'s first rung as this process runs it.
#[must_use]
pub fn first_look() -> std::time::Duration {
    std::time::Duration::from_secs(upgrade::quiet_s())
}

fn later_rungs() -> [std::time::Duration; 4] {
    let mut rungs = LATER;
    rungs[0] = first_look();
    rungs
}

/// The longest gap between two looks reading the same process's screen idle for them to count in a row ([`St::idle_looks`]).
pub const IDLE_RUN_GAP_S: u64 = LATER[LATER.len() - 1].as_secs() + upgrade::QUIET_S;

/// How soon a session whose step waited on a person's hold ([`upgrade::person_hold`]) is looked at again, however long it has waited.
pub const HOLD_LOOK: std::time::Duration = std::time::Duration::from_secs(60);

const _: () = assert!(upgrade::HOLD_GAP_S >= 2 * HOLD_LOOK.as_secs());

/// [`After`] from one [`step`]'s word, `waits` the session's consecutive waits before it.
#[must_use]
pub fn after(step: &str, waits: u32) -> After {
    known_after(step, waits).unwrap_or(After::Later(LATER[LATER.len() - 1]))
}

/// Whether [`after`] was never taught `step`.
#[must_use]
pub fn unknown_step(step: &str) -> bool {
    known_after(step, 0).is_none()
}

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

pub(super) fn watch_at(st: &St) -> Option<u64> {
    let step = due_by(st)?;
    let moved = st.behind_since().saturating_add(MOVE_BUDGET_S);
    Some(step.min(moved).max(st.looked_at.saturating_add(WATCH_GAP)))
}

fn is_last_word(step: &str) -> bool {
    matches!(
        step.split_once(':').map_or(step, |(head, _)| head),
        "current" | "done" | "ended" | "skip" | "terminated" | "relaunched" | "exited"
    ) || step.starts_with("would-")
        || step == "wait:skipped"
}

fn known_after(step: &str, waits: u32) -> Option<After> {
    let later = || ladder(waits);
    Some(match step.split_once(':').map_or(step, |(head, _)| head) {
        "announced" | "continued" | "adopted" => After::NextIdle,
        "gave-up" | "drain-expired" => After::NextIdle,
        "released" | "rearmed" => later(),
        _ if is_last_word(step) => After::Finished,
        _ if step == "wait:failed" => later(),
        "failed" | "refused" => later(),
        "busy" if matches!(step, "busy:state-unwritable" | "busy:lock-unopenable") => After::Broken,
        _ if matches!(step, "wait:box" | "wait:draft") => After::Later(HOLD_LOOK),
        _ if attended(step) => After::Later(first_look()),
        "wait" | "busy" | "held-back" | "left-typed" => later(),
        "reopened" => later(),
        _ => return None,
    })
}

fn ladder(waits: u32) -> After {
    After::Later(
        crate::supervise::ladder::Ladder(&later_rungs())
            .step(usize::try_from(waits).unwrap_or(usize::MAX)),
    )
}

/// Whether one [`step`]'s word is a person at the tab holding the act due at this point.
#[must_use]
pub fn attended(step: &str) -> bool {
    matches!(step, "held-back:attended" | "wait:attended")
}

/// [`After`] an attended wait ([`attended`]).
#[must_use]
pub fn after_attended(looks: u32, same: u32, grace_s: u32) -> After {
    if attended_owned(looks, grace_s) {
        After::Later(first_look())
    } else {
        ladder(same)
    }
}

/// Whether one [`step`]'s word says it typed a turn into the agent ([`crate::supervise::HostStep::typed`]).
#[must_use]
pub fn typed(step: &str) -> bool {
    matches!(
        step.split_once(':').map_or(step, |(head, _)| head),
        "announced" | "continued" | "released"
    )
}

/// Which turn one [`step`]'s word typed ([`typed`]), for the turn-end policy ([`crate::supervise::HostStep::typed`]).
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

/// Whether one [`step`]'s word says it moved the session ([`crate::supervise::HostStep::moved`]).
#[must_use]
pub fn moved(step: &str) -> bool {
    typed(step)
        || matches!(
            step.split_once(':').map_or(step, |(head, _)| head),
            "adopted" | "relaunched" | "terminated" | "exited"
        )
        || matches!(step, "done:fresh" | "done:unconfirmed")
}

/// Whether one [`step`]'s word says a NEW AGENT PROCESS came up where the harness ended the old one.
#[must_use]
pub fn started(step: &str) -> bool {
    matches!(
        step.split_once(':').map_or(step, |(head, _)| head),
        "adopted" | "relaunched"
    ) || matches!(step, "done:fresh" | "done:unconfirmed")
}

/// Holds the upgrade's clocks through a limit episode the window's loop saw.
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

/// Maximum consecutive looks the upgrade waits for background work under its own gate.
pub const OWNED_BACKGROUND_LOOKS: u32 = 3;

/// How many quick looks in a row the upgrade may own the session's turn ends for.
pub const OWNED_SETTLE_LOOKS: u32 = 2;

/// Return whether the upgrade owns the session's turn ends after `step`.
#[must_use]
pub fn owns_turn_ends(step: &str, looks: u32, grace_s: u32) -> bool {
    if attended(step) {
        return attended_owned(looks, grace_s);
    }
    match step.split_once(':').map_or(step, |(head, _)| head) {
        "announced" => true,
        "wait" => match step {
            "wait:settling"
            | "wait:not-idle"
            | "wait:busy"
            | "wait:status-stale"
            | "wait:ready-unrecorded"
            | "wait:superseded" => looks < OWNED_SETTLE_LOOKS,
            _ if log_settles(step) => looks < OWNED_SETTLE_LOOKS,
            _ if release_settles(step) => looks < OWNED_SETTLE_LOOKS,
            "wait:background" => looks < OWNED_BACKGROUND_LOOKS,
            _ if refused(step) => looks < OWNED_REFUSED_LOOKS,
            _ => false,
        },
        _ => false,
    }
}

/// [`owns_turn_ends`] under the flow the step was taken in.
#[must_use]
pub fn owns_turn_ends_as(step: &str, looks: u32, grace_s: u32, quiet: bool) -> bool {
    !(quiet && drains(step)) && owns_turn_ends(step, looks, grace_s)
}

/// [`bounded_wait`] under the flow the step was taken in.
#[must_use]
pub fn bounded_wait_as(step: &str, quiet: bool) -> bool {
    !(quiet && drains(step)) && bounded_wait(step)
}

fn drains(step: &str) -> bool {
    step == "wait:background"
}

/// Whether a wait counts toward the bound its word is owned for ([`owns_turn_ends`]).
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

/// Return whether `step` is a quiet Muse Code or agy restart waiting on its record to settle.
#[must_use]
pub fn log_settles(step: &str) -> bool {
    matches!(step, "wait:turn-open" | "wait:queued-work")
}

/// Whether `step` is an owed release waiting out the settle at its point (`wait:release:settling`, `…:not-idle`, `…:busy`, `…:status-stale`).
#[must_use]
pub fn release_settles(step: &str) -> bool {
    step.strip_prefix("wait:release:")
        .is_some_and(|why| matches!(why, "settling" | "not-idle" | "busy" | "status-stale"))
}

/// Whether one [`step`]'s word is a notice its own fence refused (`wait:announce-refused:<why>`).
#[must_use]
pub fn refused(step: &str) -> bool {
    step.starts_with("wait:announce-refused:")
}

/// Maximum refused looks before an upgrade-owned session stands down.
pub const OWNED_REFUSED_LOOKS: u32 = 3;

/// [`After`] a refused notice ([`refused`]).
#[must_use]
pub fn after_refused(looks: u32, same: u32) -> After {
    if looks < OWNED_REFUSED_LOOKS {
        After::Later(first_look())
    } else {
        ladder(same)
    }
}

/// Maximum attempts to type a notice when the input fence reports `changed`.
pub const NOTICE_FENCE_TRIES: usize = 3;

/// Whether an attended wait ([`owns_turn_ends`]) still owns the session's turn ends after `looks` attended looks in a row at this point.
#[must_use]
pub fn attended_owned(looks: u32, grace_s: u32) -> bool {
    u64::from(looks).saturating_mul(first_look().as_secs())
        <= u64::from(grace_s).saturating_add(ATTENDED_OWNED_S)
}

/// How long past the person's grace an attended wait may keep owning the session's turn ends ([`attended_owned`]).
pub const ATTENDED_OWNED_S: u64 = 30 * 60;

/// Whether a step taken at a break of the agent's background work (`IdleHost::at_background`) gives back the turn ends a notice claimed there.
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
    let mut reports = if opts.background {
        Vec::new()
    } else {
        confirmations(opts)
    };
    muse::sweep_prune(opts, live_tabs);
    agy::sweep_prune(opts, live_tabs);
    gemini::sweep_prune(opts, live_tabs);
    reports.extend(lanes(opts, live_tabs, &|agent| match agent {
        upgrade::Agent::Codex => codex_alab_drive::sweep(opts, live_tabs),
        upgrade::Agent::Claude => claude_pass(opts, live_tabs),
        upgrade::Agent::Muse => muse::live::upgrade_pass(opts, live_tabs),
        upgrade::Agent::Agy => agy::live::upgrade_pass(opts, live_tabs),
        upgrade::Agent::Gemini => Vec::new(),
    }));
    reports
}

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

fn lane_on(opts: &Opts, agent: upgrade::Agent) -> bool {
    let written = super::lanes::lanes(agent.program());
    (written.upgrade || (written.relaunch && super::lanes::sweep_carries_relaunch(agent.program())))
        && !opts.agents_off.contains(&agent.program())
}

pub(super) fn codex_carry_in_flight(opts: &Opts, r: Report, st: St, session: &str) -> Report {
    codex_alab_drive::carry_in_flight(opts, r, st, session)
}

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

/// Relaunch a proven self-claimed codex-alab after exit.
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

pub(super) fn self_claimed_relaunch(
    opts: &Opts,
    snap: &super::relaunch::Snapshot,
    resume: &super::self_claimed::Resume,
    cause: super::relaunch::ExitCause,
) -> Report {
    codex_alab_drive::self_claimed_after_exit(opts, snap, resume, cause, &Live)
}

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

pub(super) fn muse_follow(snap: &mut super::relaunch::Snapshot) -> bool {
    muse::follow(snap)
}

pub(super) fn muse_look_at_exit(
    opts: &Opts,
    snap: &super::relaunch::Snapshot,
) -> super::relaunch::ExitLook {
    muse::look_at_exit(opts, snap, &Live)
}

pub(super) fn muse_after_exit(
    opts: &Opts,
    snap: &super::relaunch::Snapshot,
    left: &super::relaunch::ExitRecord,
    upgrade: bool,
    cause: super::relaunch::ExitCause,
) -> Report {
    muse::after_exit(opts, snap, left, upgrade, cause, &Live)
}

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

pub(super) fn agy_look_at_exit(
    opts: &Opts,
    snap: &super::relaunch::Snapshot,
) -> super::relaunch::ExitLook {
    agy::look_at_exit(opts, snap, &Live)
}

pub(super) fn agy_after_exit(
    opts: &Opts,
    snap: &super::relaunch::Snapshot,
    left: &super::relaunch::ExitRecord,
    upgrade: bool,
    cause: super::relaunch::ExitCause,
) -> Report {
    agy::after_exit(opts, snap, left, upgrade, cause, &Live)
}

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

pub(super) fn gemini_look_at_exit(
    opts: &Opts,
    snap: &super::relaunch::Snapshot,
) -> super::relaunch::ExitLook {
    gemini::look_at_exit(opts, snap, &Live)
}

pub(super) fn gemini_after_exit(
    opts: &Opts,
    snap: &super::relaunch::Snapshot,
    left: &super::relaunch::ExitRecord,
    upgrade: bool,
    cause: super::relaunch::ExitCause,
) -> Report {
    gemini::after_exit(opts, snap, left, upgrade, cause, &Live)
}

/// The resume line a person is told for `snap`'s Gemini CLI ([`gemini::told`]).
#[must_use]
pub fn gemini_told_line(snap: &super::relaunch::Snapshot) -> Option<String> {
    gemini::told(snap)
}

/// [`gemini_told_line`] of `session` in the project `root` whose chats directory is `chats`, behind the homes `env`.
#[must_use]
pub fn gemini_told(
    session: &str,
    chats: Option<&Path>,
    env: &[(String, String)],
    root: &str,
) -> Option<String> {
    gemini::told_at(session, chats, env, root)
}

pub(super) fn agy_carries_restart(user_home: &Path, snap: &super::relaunch::Snapshot) -> bool {
    let twin = super::upgrade_agy::store_build(&snap.program)
        .then(|| managed_agent_twin(super::upgrade_agy::PROGRAM))
        .flatten();
    agy::carries_restart_onto(snap, user_home, twin)
}

fn claude_pass(opts: &Opts, live_tabs: Option<&[LiveTab]>) -> Vec<Report> {
    if live_tabs.is_some_and(<[LiveTab]>::is_empty) {
        return Vec::new();
    }
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
    if !opts.background {
        prune_states(opts, &files, now_s());
    }
    let mut reports = Vec::new();
    reports.extend(if let Some(tabs) = live_tabs {
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

pub(super) struct SweepLock(std::fs::File);

impl Drop for SweepLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

fn confirming(opts: &Opts) -> bool {
    std::fs::read_dir(state_dir(opts)).is_ok_and(|dir| {
        dir.flatten().any(|e| {
            let path = e.path();
            path.extension().is_some_and(|x| x == "json")
                && read_record_text(&path)
                    .ok()
                    .flatten()
                    .and_then(|text| St::from_json(&text))
                    .is_some_and(|st| {
                        st.confirming() && opts.only_sid.as_ref().is_none_or(|s| *s == st.tab)
                    })
        })
    })
}

/// List conversations left mid-restart by an earlier upgrade sweep.
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

/// The agent the first conversation [`in_flight`] names ran (its record's agent).
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

struct DroppedRide {
    why: &'static str,
    model: String,
    released: bool,
}

impl DroppedRide {
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
            let r = said(r, "wait:exiting");
            note_stuck(opts, &r, &mut st, &sf.session_id, now_s());
            return r;
        }
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
    let launch = upgrade::launch_model(&args.argv);
    let carried = prior.as_ref().and_then(St::carried_model);
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
    let build_target = upgrade::choose_target(&running, &targets.for_session(running_native));
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
    if Records::of_file(&opts.home, sf).left_by(
        &opts.home,
        args.env_var(super::accounts::HOME_ENV),
        || aterm_sysprobe::process_cwd(sf.pid),
    ) {
        return said(r, format!("wait:{HOME_MOVED}"));
    }
    if prior.as_ref().is_some_and(St::confirming) {
        return said(r, "wait:confirming");
    }
    let now = now_s();
    let who = squash(&sf.proc_start);
    let who = Proc {
        start: &who,
        started_s: models::parse_lstart(&sf.proc_start).unwrap_or(0),
    };
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
    if st.keep_home(&Records::of_file(&opts.home, sf), &opts.home) {
        save(opts, &sf.session_id, &st);
    }
    if let Some(dropped) = &dropped {
        dropped.say(opts, &r, &st, how);
    }
    if how == Retarget::Rested {
        rested_said(opts, &r, &st, now);
    }
    if st.phase == Phase::Pending {
        st.tab.clone_from(&tab);
    }
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
    if matches!(st.phase, Phase::Pending | Phase::Announced { .. }) {
        let job = k.job(sf.pid);
        if let Some(job) = not_a_shell_job(job) {
            let r = not_a_job(opts, r, &mut st, job);
            save(opts, &sf.session_id, &st);
            return r;
        }
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
    if let Some(r) = left_first(opts, &r, &mut st, &mut c, &tab, sf, now) {
        return r;
    }
    let records = Records::of_file(&opts.home, sf);
    let recent = records
        .transcript(&sf.session_id)
        .map(|p| tail_to_end(&p, TAIL_BYTES));
    let tail = recent.as_ref().map(|(text, _)| text.as_str());
    let rearmed = login_facts(&records, &sf.session_id, &mut st, &mut facts, tail, now);
    let asked = st.request_for(&tab);
    let requeued = queue_facts(&mut st, &mut facts, tail, now, &asked);
    let ready = heard(&st, sf, &tab, tail);
    ready_facts(&st, sf, &tab, tail, ready, &mut facts);
    st.time_ready(ready, &mut facts, now);
    st.time_failed(&mut facts, now);
    st.time_work(&mut facts, now);
    let request = st.request_for(&tab);
    let mut asked_about = match rearmed {
        Some(phase) if !ready => {
            facts.undelivered = false;
            phase
        }
        _ => st.phase.clone(),
    };
    let taken_back = asked_about != st.phase;
    let mut step = upgrade::requested_step(&request, &asked_about, &facts, ready, now, &st.to);
    if opts.background {
        step = upgrade::break_step(step);
    }
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
    let mut proven = None;
    if matches!(step, Step::Announce | Step::Terminate | Step::Fresh) {
        match foreground_shell(k.job(sf.pid)) {
            Ok(shell) => proven = Some(shell),
            Err(why) => step = Step::Wait(why),
        }
    }
    let moving = matches!(step, Step::Announce | Step::Terminate | Step::Fresh);
    if target.version != running
        && (upgrade::asked_for_a_move(&asked_about, ready) || moving)
        && build_starts(opts, &r, &target, now, request == Request::Now, moving).is_err()
    {
        step = upgrade::build_hold(step, &asked_about, ready);
    }
    let held_point = opts
        .dry_run
        .then(|| upgrade::held_point(&request, &step, &facts, now, &st.to))
        .flatten()
        .map(|w| upgrade::wait_word(w, &facts.status));
    let result = match step {
        Step::Wait("attended") => attended_once(opts, r, &mut st),
        Step::Wait(why) => said(r, format!("wait:{why}")),
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
            let mode = resume_mode(&records, &sf.session_id, shown_mode.clone());
            let model = model_to.clone().or_else(hand_model);
            let planned = proven.map_or(Err(NoPlan::Wait("ids")), |shell| {
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
            let retry = Retry {
                looked: looked_hand,
                owner_now: st.request_for(&tab) == Request::Now,
                at_break: facts.background_point,
                grace_s: upgrade::person_grace(upgrade::rung_in_force(&facts), opts.human_grace_s),
            };
            let mut cut: Option<(String, String)> = None;
            let result = announce(opts, r, &mut st, planned, now, asks, again, |asks| {
                let marker = upgrade::ready_marker(
                    &sf.session_id,
                    &target.version,
                    salt.wrapping_add(u64::from(asks)),
                );
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
        Step::Terminate | Step::Fresh => {
            let fresh = step == Step::Fresh;
            let unasked = !fresh && facts.quiet && asked_about == Phase::Pending;
            let owed_here = !st.release.is_empty() && st.notice_belongs_to(sf, &tab);
            let quiet = unasked && !owed_here;
            st.quiet = quiet;
            if fresh || unasked {
                st.model_list = model_to.clone().unwrap_or_default();
            }
            if !fresh {
                (st.model_before, st.mark) = model_and_mark(recent.as_ref());
            }
            st.launch_model = launch_kept(opts, &sf.session_id, &args.argv).unwrap_or_default();
            let hand = st.model_list.is_empty().then(hand_model).flatten();
            let restarting = Restarting {
                argv: &args.argv,
                target: &target,
                host_claim,
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
    let restart_word = (!opts.dry_run && matches!(step, Step::Terminate | Step::Fresh))
        .then(|| result.step.clone());
    let result = if release_next(&step, &result.step, &st, &facts, ready) {
        let ready = heard(&st, sf, &tab, tail);
        release(
            opts, result, &mut st, &mut c, &tab, sf, host_claim, k, &facts, ready, tail,
        )
    } else {
        result
    };
    let result = match rearmed_here {
        Some(rearmed) if !result.step.starts_with("announced:") => rearmed,
        _ => result,
    };
    st.saw_work(&facts);
    st.note_step(&result.step, &facts.status, now);
    st.restart_look =
        result.step.starts_with("wait:") && restart_word.as_deref() == Some(result.step.as_str());
    let copy = if running_native { "foreign" } else { "managed" };
    st.annotate_wait(
        now,
        &wait_note(
            &facts,
            &format!("{seen} copy={copy}"),
            facts.background.len(),
        ),
    );
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

pub(super) const AT_POINT: char = '@';

pub(super) fn rounds_step(word: &str) -> bool {
    let act = word.split_once(AT_POINT).map_or(word, |(act, _)| act);
    act == "would-announce"
        || act == "gave-up"
        || act == "would-restart"
        || act.starts_with("would-restart:")
        || act.starts_with("would-rearm:")
}

pub(super) fn at_point(word: &str, held: &str) -> String {
    format!("{word}{AT_POINT}{held}")
}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct LadderLook {
    pub(super) rung: upgrade::Rung,
    pub(super) attended: bool,
    pub(super) typing: bool,
    pub(super) idle_read: bool,
    pub(super) still_s: u64,
}

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

pub(super) struct TabRead<'a> {
    pub(super) scr: &'a Screen,
    pub(super) status: Option<&'a str>,
    pub(super) dim: bool,
}

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
    let ladder = ladder_look(st, "claude", scr, sf.pid, opts.human_grace_s, now);
    let quiet = quiet_s(status, st, scr.seq, now);
    let mut facts = Facts {
        status: sf.status.clone(),
        status_age_s: now.saturating_sub(sf.status_updated_at_ms / 1000),
        composer_empty: upgrade::composer_is_empty(&scr.rows, scr.cursor, dim),
        approval_box: aterm_phase::prompt::prompt_box_span(&scr.rows).is_some(),
        busy_footer: busy_footer(&scr.rows),
        background: background(sf.pid, t),
        held: status.is_none_or(|line| held_by_status(line, 0)),
        quiet_s: quiet,
        hold_s: 0,
        owner_now: false,
        attended: ladder.attended,
        background_point: opts.background,
        taskless: matches!(st.phase, Phase::Pending | Phase::Announced { .. })
            && Records::of_file(&opts.home, sf).tasked(
                &sf.session_id,
                &conversation_typed(opts, tab, Some(&sf.session_id), &[]),
            ) == Some(false),
        limited: upgrade::limited(&scr.rows),
        login: upgrade::login_wall(upgrade::Agent::Claude, &scr.rows),
        undelivered: false,
        queued: false,
        ready_s: 0,
        idle_looks: 0,
        failed_s: 0,
        rung: ladder.rung,
        typing: ladder.typing,
        still_s: ladder.still_s,
        work_ended: false,
        ready_unrecorded: ladder.idle_read
            && !opts.background
            && aterm_phase::said_tail(&scr.rows)
                .is_some_and(|said| st.round_markers().any(|m| upgrade::says_ready(&said, m))),
        ready_superseded: false,
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
        unnoticed: now.saturating_sub(st.behind_since()) < upgrade::quiet_s(),
    };
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
    let render = status
        .and_then(|line| {
            line.split_whitespace()
                .find_map(|t| t.strip_prefix("render="))
        })
        .unwrap_or("-");
    let place = |at: Option<usize>| at.map_or_else(|| "-".to_string(), |n| n.to_string());
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

const GAVE_UP_TAIL_BYTES: u64 = 8 * 1024 * 1024;

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
    let until = upgrade::transcript_limit_until(text);
    let reset = upgrade::transcript_limit_reset(text);
    st.limit_until = reset.unwrap_or(0);
    st.limit_spend = upgrade::transcript_limit_spend(text);
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

fn owed_word(r: Report, why: &str) -> Report {
    if r.is_act() && after(&r.step, 0) == After::NextIdle {
        r
    } else {
        said(r, format!("wait:release:{why}"))
    }
}

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

fn release_void(st: &St, sf: &SessionFile, tab: &str, tail: Option<&str>) -> Option<&'static str> {
    if !st.notice_belongs_to(sf, tab) {
        return Some("other-process");
    }
    tail.is_some_and(|t| upgrade::directed_since_ready(t, &st.asked))
        .then_some("directed")
}

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

pub(super) fn model_and_mark(recent: Option<&(String, u64)>) -> (String, u64) {
    recent.map_or((String::new(), 0), |(text, end)| {
        (upgrade::transcript_model(text).unwrap_or_default(), *end)
    })
}

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
        if !driven_here(st.agent) {
            continue;
        }
        if live_tabs.is_some_and(|tabs| !tab_is_live(tabs, &st.tab)) {
            continue;
        }
        if conversation_live(files, &session) {
            continue; // `visit` has it.
        }
        if opts.only_sid.as_ref().is_some_and(|s| *s != st.tab) {
            continue;
        }
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
        if alive(st.pid) {
            note_stuck(opts, &r, &mut st, &session, now_s());
            continue;
        }
        if hand_waits_out(opts, &mut st, &session, alive) {
            if st.seen_gone(now_s()) {
                save(opts, &session, &st);
            }
            out.push(said(r, WAIT_HAND_LIFTS));
            continue;
        }
        let stale = expired(&st, now_s());
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

fn answered(st: &St, tail: Option<&str>) -> bool {
    let Some(text) = tail else {
        return false;
    };
    st.round_markers()
        .any(|m| upgrade::transcript_has_ready(text, m))
}

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

fn heard(st: &St, sf: &SessionFile, tab: &str, tail: Option<&str>) -> bool {
    st.hears_ready() && st.notice_belongs_to(sf, tab) && answered(st, tail)
}

fn release_is_next(step: &Step, word: &str) -> bool {
    match step {
        Step::GiveUp | Step::Void(_) => true,
        Step::Wait(why) => upgrade::release_is_next(why),
        Step::Announce | Step::Terminate | Step::Fresh => {
            word.starts_with("failed:") || word.starts_with("refused:")
        }
        Step::Rearm => false,
    }
}

fn release_next(step: &Step, word: &str, st: &St, f: &Facts, ready: bool) -> bool {
    !st.release.is_empty()
        && (release_is_next(step, word)
            || (*step == Step::Wait("background") && unanswered_at_break(st, f, ready))
            || quiet_release_due(step, st, f))
}

fn quiet_release_due(step: &Step, st: &St, f: &Facts) -> bool {
    f.quiet
        && st.phase == Phase::Pending
        && matches!(step, Step::Wait(w) if upgrade::own_work_holds(w))
}

const fn unanswered_at_break(st: &St, f: &Facts, ready: bool) -> bool {
    f.background_point && !ready && matches!(st.phase, Phase::Announced { .. })
}

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
        "status-stale" => (
            "Claude Code's own status, never back to idle over an idle screen,",
            f.ready_s,
        ),
        _ => ("work of the agent's own still running", f.ready_s),
    };
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
        Err(e) if e == TURN_RUNNING => said(r, format!("wait:{TURN_RUNNING}")),
        Err(e) => said(r, format!("wait:announce-refused:{}", first_word(&e))),
    }
}

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

pub(crate) fn homedir_holding(dir: &Path) -> Option<String> {
    let parent = dir
        .parent()
        .filter(|_| dir.file_name().is_some_and(|n| n == ".claude"))?;
    parent
        .is_absolute()
        .then(|| parent.to_str().map(str::to_owned))
        .flatten()
}

fn stored_login_of(env: &[String]) -> bool {
    super::accounts::ProcessLogin::of_exec_env(env).is_some_and(|who| who.stored_login)
}

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

pub(super) fn typing_fence(c: &mut Client, tab: &str) -> Result<Option<String>, &'static str> {
    typing_fence_read(c, tab).map(|(generation, _)| generation)
}

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

const ROWS_FENCE: &str = "rows:";

const TURN_RUNNING: &str = "busy";

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HandAt {
    Unknown,
    Never,
    At(Instant),
}

impl HandAt {
    pub(super) fn of(human: HumanInput) -> HandAt {
        match human {
            HumanInput::Unknown => HandAt::Unknown,
            HumanInput::Never | HumanInput::NeverFor(_) => HandAt::Never,
            HumanInput::Ago(ms) => Instant::now()
                .checked_sub(Duration::from_millis(ms))
                .map_or(HandAt::Unknown, HandAt::At),
        }
    }

    pub(super) fn still(self, later: HandAt) -> bool {
        match (self, later) {
            (_, HandAt::Never) => true,
            (HandAt::At(then), HandAt::At(now)) => now <= then + HAND_SLACK,
            _ => false,
        }
    }
}

const HAND_SLACK: Duration = Duration::from_millis(500);

pub(super) struct Retry {
    pub(super) looked: HandAt,
    pub(super) owner_now: bool,
    pub(super) at_break: bool,
    pub(super) grace_s: u32,
}

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

fn turn_over_shown(read: &Screen, lagging: bool) -> bool {
    !lagging || (!busy_footer(&read.rows) && reads_idle(read))
}

pub(super) fn composer_clear(c: &mut Client, tab: &str) -> bool {
    screen(c, tab).is_some_and(|scr| {
        aterm_phase::prompt::prompt_box_span(&scr.rows).is_none() && composer_empty(c, tab, &scr)
    })
}

fn fence_verdict(scr: &Screen, dim: bool) -> Result<Option<String>, &'static str> {
    if aterm_phase::prompt::prompt_box_span(&scr.rows).is_some() {
        return Err("box");
    }
    if !upgrade::composer_is_empty(&scr.rows, scr.cursor, dim) {
        return Err("draft");
    }
    Ok(scr.generation.clone())
}

fn dim_cell(scr: &Screen) -> Option<usize> {
    upgrade::composer_dim_row(&scr.rows, scr.cursor).and_then(|caret| scr.first.checked_add(caret))
}

fn same_composer(a: &Screen, b: &Screen) -> bool {
    fn caret(s: &Screen) -> Option<&str> {
        aterm_phase::phase::composer_draft(&s.rows)
            .and_then(|(caret, _)| s.rows.get(caret))
            .map(String::as_str)
    }
    a.cursor == b.cursor && a.first == b.first && caret(a) == caret(b)
}

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

struct Restarting<'a> {
    argv: &'a [String],
    target: &'a Candidate,
    host_claim: Option<&'a HostClaim>,
    grace: u32,
    fresh: bool,
    quiet: bool,
    lagging: bool,
    mode: Option<&'a str>,
    model: Option<&'a str>,
    login: Option<&'a str>,
    home: Option<&'a str>,
    stored_login: bool,
    profile: Option<&'a super::profile::Context>,
}

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
    let records = Records::of_file(&opts.home, sf);
    let fresh = fresh || records.transcript_exists(&sf.session_id) == Some(false);
    let status_holds = |a: &SessionFile| status_lets_signal(a, sf, lagging);
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
    let job = k.job(sf.pid);
    if let Some(job) = not_a_shell_job(job) {
        return not_a_job(opts, r, st, job);
    }
    let proven = match foreground_shell(job) {
        Ok(shell) => shell,
        Err(why) => return said(r, format!("wait:{why}")),
    };
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
    let Some(mut hand) = Hand::take(c, tab) else {
        return said(r, "wait:held");
    };
    let aborted: Option<Report> = 'signal: {
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
        let back = st.signalled(sf.pid, shell, tab, line, now_s());
        st.back_line.clone_from(&back_line);
        st.login = login.map(str::to_owned);
        if fresh {
            super::relaunch::CAUSE_UPGRADE_FRESH.clone_into(&mut st.cause);
        }
        st.fallback_from =
            fallback_carried(fallback_origin(st), recorded(&st.model_list), hand_model);
        if save_checked(opts, &sf.session_id, st).is_err() {
            st.unsent(back);
            st.cause.clear();
            break 'signal Some(said(r.clone(), "wait:claim-unwritten"));
        }
        let current = records.session_file(sf.pid);
        let owns_notice = current.as_ref().is_some_and(|a| {
            a.session_id == sf.session_id
                && status_holds(a)
                && ours(st, a)
                && k.process_start(sf.pid).as_deref() == Some(squash(&a.proc_start).as_str())
        });
        let owner = k.terminal(sf.pid);
        let owner = owner.as_ref().map(|(pid, name)| (*pid, name.as_str()));
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
        match terminate(c, tab, pid, grace) {
            Terminated::Sent => None,
            Terminated::Refused(why) => {
                st.unsent(back);
                st.cause.clear();
                save(opts, &sf.session_id, st);
                Some(said(r.clone(), format!("wait:signal-{why}")))
            }
            Terminated::Failed => {
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

pub(super) fn take_back_ask(opts: &Opts, session: &str, asked: Option<models::Asked>) {
    if let Some(asked) = asked {
        let mut rec = load_model_record(opts, session);
        rec.take_back(asked);
        save_model_record(opts, session, &rec);
    }
}

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

#[cfg(all(test, unix))]
#[path = "upgrade_drive_tests.rs"]
mod tests;

#[cfg(all(test, windows))]
mod kernel_start_win_tests {
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
        assert_eq!(super::kernel_start(0x7fff_fff0), None, "no such process");
        assert_eq!(super::kernel_start(0), None, "never pid 0");
    }
}
