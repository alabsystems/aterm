// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Shell integration types (re-exported from `aterm-types`) and the terminal's caps on
//! them. Most of the API is in [`super::shell_api`].

pub use aterm_types::{
    Annotation, BlockState, CommandMark, OutputBlock, TerminalMark, current_time_ms,
};

/// Maximum completed command marks (OSC 133). FIFO eviction when exceeded.
pub(super) const COMMAND_MARKS_MAX: usize = 1000;

/// Maximum completed output blocks. FIFO eviction.
pub(super) const OUTPUT_BLOCKS_MAX: usize = 1000;

/// Maximum number of user-created marks (OSC 1337 SetMark); oldest evicted
/// first.
pub(super) const TERMINAL_MARKS_MAX: usize = 1000;

/// Maximum number of annotations (OSC 1337 AddAnnotation); oldest evicted
/// first.
pub(super) const ANNOTATIONS_MAX: usize = 1000;

// `ShellState` lives in `domain` (a leaf module) to avoid a dependency cycle.
pub use crate::domain::ShellState;

/// Which shell signed a START record (`633;P;AtermStart=`): one nobody held
/// (`cold`), or a warm spare about to wait at its first prompt hook (`spare`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartKind {
    /// A shell that ran its startup files in view: a cold tab, or a parked
    /// spare after its release.
    Cold,
    /// A warm spare whose startup files ran unseen, now at its hold.
    Spare,
}

/// A START record's flags (`633;P;AtermStartFlags=pre:<n>,s:<n>,j:<n|->,r:<0|1>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StartFlags {
    /// Prompt hooks that ran ahead of the integration's first one.
    pub pre: u32,
    /// Pending `sched` events.
    pub sched: u32,
    /// Jobs, or `None` where the shell did not count them (bash's cold mark).
    pub jobs: Option<u32>,
    /// Periodic hooks armed (`PERIOD` with `periodic`).
    pub periodic: bool,
}

/// A shell's START record, signed once at its first prompt hook after every
/// startup file (`docs/DESIGN-warm-spare-2026-10-16.md` §3.3): the rc folder
/// it ended in, its flags, and its kind. The host's warm spare learns from it
/// whether an rc generation may run unseen; nothing else reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartMark {
    /// Cold or spare.
    pub kind: StartKind,
    /// The final `${ZDOTDIR:-$HOME}` (zsh), `$HOME` (bash) or
    /// `$__fish_config_dir` (fish), unescaped.
    pub rc_dir: Option<String>,
    /// fish's other startup roots, unescaped: its sysconf folder, then each
    /// vendor conf.d folder its conf.d loop walks. `None` where the record
    /// named none (zsh, bash, or a fish that could not see them).
    pub roots: Option<Vec<String>>,
    /// The flags, when they parsed.
    pub flags: Option<StartFlags>,
}

impl StartFlags {
    /// Parse `pre:<n>,s:<n>,j:<n|->,r:<0|1>` strictly: every field once, in
    /// that order, nothing else.
    #[inline]
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        let mut parts = value.split(',');
        let mut field = |name: &str| -> Option<&str> { parts.next()?.strip_prefix(name) };
        let pre = field("pre:")?.parse().ok()?;
        let sched = field("s:")?.parse().ok()?;
        let jobs = match field("j:")? {
            "-" => None,
            n => Some(n.parse().ok()?),
        };
        let periodic = match field("r:")? {
            "0" => false,
            "1" => true,
            _ => return None,
        };
        if parts.next().is_some() {
            return None;
        }
        Some(Self {
            pre,
            sched,
            jobs,
            periodic,
        })
    }
}

/// What one OSC 7 does to the reported cwd: the app-event it queues for
/// poll-based hosts (`//host/path`, or the bare path for a local host; none
/// for a clear) and the local path it stores (none: clear it). Applied at once,
/// or held while an authenticated shell that does not sign its own sits at its
/// prompt (`ShellIntegrationState::held_cwd`); also the cwd a foreground job's
/// report replaced (`ShellIntegrationState::cwd_before_job`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CwdReport {
    /// The app-event payload, or `None` for a clear (which queues none).
    pub(super) event: Option<String>,
    /// The percent-decoded local path to store, or `None` to clear it.
    pub(super) path: Option<String>,
    /// The host the URI named (`file://<host>/…`), verbatim: `None` for an
    /// empty host, `localhost`, or a clear. Stored beside the path
    /// ([`super::Terminal::reported_cwd_host`]), so a consumer that resolves
    /// files against the cwd can tell a remote shell's directory from a local
    /// one.
    pub(super) host: Option<String>,
}

impl CwdReport {
    /// The report that stores `path` (none: clears it) with the `host` that
    /// qualifies it (`None` for a local one), and queues the event an OSC 7
    /// naming them queues; `None` when the engine would not store it
    /// (`handler_osc::reported_cwd_is_storable`). What a checkpoint carries
    /// and a job's end restores, rebuilt as OSC 7 would build it.
    #[inline]
    pub(super) fn of(path: Option<&str>, host: Option<&str>) -> Option<Self> {
        let Some(path) = path else {
            return Some(Self {
                event: None,
                path: None,
                host: None,
            });
        };
        let host = host.filter(|h| !h.is_empty() && !h.eq_ignore_ascii_case("localhost"));
        let event = match host {
            Some(h) => format!("//{h}{path}"),
            None => path.to_owned(),
        };
        super::handler_osc::reported_cwd_is_storable(&event).then(|| Self {
            event: Some(event),
            path: Some(path.to_owned()),
            host: host.map(str::to_owned),
        })
    }
}

/// The warm spare's marks as the engine holds them until the host takes them:
/// the parked hold's one-shot, and the START record being assembled (its rc and
/// flags arrive first; `AtermStart` completes it). Only the first record is
/// kept. Never checkpointed: a spare is never carried across an update.
#[derive(Debug, Default)]
pub(super) struct SpareMarks {
    /// `AtermSpare=parked` arrived and the host has not taken it.
    pub(super) parked: bool,
    /// The record's rc folder, before its `AtermStart`.
    pub(super) pending_rc: Option<String>,
    /// The record's other startup roots, before its `AtermStart`.
    pub(super) pending_roots: Option<Vec<String>>,
    /// The record's flags, before its `AtermStart`.
    pub(super) pending_flags: Option<StartFlags>,
    /// The completed record, until the host takes it.
    pub(super) mark: Option<StartMark>,
    /// A record was completed (taken or not): later ones are ignored.
    pub(super) recorded: bool,
    /// Signed spare marks refused as malformed.
    pub(super) refused: u32,
}
