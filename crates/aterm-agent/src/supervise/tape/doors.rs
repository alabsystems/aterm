// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Append-only door names for harness tape effect records.

/// Control-socket round-trip lane.
pub const CTL: &str = "ctl";

/// The teller's lane (`story`), its own `Ctl` beside the loop's.
pub const TELLER: &str = "teller";

/// Prefix (`host.<method>`) for calls into the window's `IdleHost`.
pub const HOST: &str = "host.";

/// Returns whether `door` is a host call that acts on the session.
#[must_use]
pub fn is_host_act(door: &str) -> bool {
    crate::supervise::run::host_door::HOST_ACTS.contains(&door)
}

/// Prefix (`probe.<name>`) for derived policy probes.
pub const PROBE: &str = "probe.";

/// Approval-ledger bound write door.
pub const WRITE_BOUND: &str = "write.bound";

/// A sleep: the request is its length; a replay returns at once.
pub const SLEEP: &str = "sleep";

/// The stop flag another thread raises: taped once, at the first true read.
pub const RAISED_STOP: &str = "raised.stop";

/// The hand-over flag, read once at the run's end.
pub const RAISED_HANDOVER: &str = "raised.handover";

/// Standard-output line write door.
pub const OUT: &str = "out";

/// A journal row the loop wrote.
pub const JOURNAL: &str = "journal";

/// An approval-ledger row the loop wrote.
pub const LEDGER: &str = "ledger";

/// Legacy notes-file write door kept reserved for older tapes.
pub const NOTES: &str = "notes";

/// Decision mark for an approval box.
pub const DECIDE_APPROVAL: &str = "decide.approval";

/// A decision on a question box, as [`DECIDE_APPROVAL`].
pub const DECIDE_QUESTION: &str = "decide.question";

/// A decision at a turn's end, as [`DECIDE_APPROVAL`].
pub const DECIDE_TURN_END: &str = "decide.turn_end";

/// Approval-policy probe for a hidden twin of a Read path.
pub const PROBE_HIDDEN_TWIN: &str = "probe.hidden_twin";

/// Approval-policy probe for resolved symlink components of a Read path.
pub const PROBE_REAL_COMPONENTS: &str = "probe.real_components";

/// Approval-policy probe for physical symlink components of an `rm` operand.
pub const PROBE_RM_PHYSICAL: &str = "probe.rm_physical";

/// Kernel process probe binding a worker to its session.
pub const PROBE_WORKER_PROC: &str = "probe.worker_proc";

/// Projected git configuration view probe for a directory.
pub const PROBE_GIT_VIEW: &str = "probe.git_view";

/// Probe checking whether Claude Code settings inject git-relevant environment variables.
pub const PROBE_SETTINGS_ENV: &str = "probe.settings_env";

/// Probe for aliases and functions defined in the worker shell's startup files.
pub const PROBE_SHELL_STARTUP: &str = "probe.shell_startup";

/// The ask rules and PreToolUse hooks of the agent's own settings.
pub const PROBE_OWNER_ASKS: &str = "probe.owner_asks";

/// Where the worker's Bash tool may stand, from Claude Code's transcripts.
pub const PROBE_SHELL_CWDS: &str = "probe.shell_cwds";

/// Probe for a named time zone's UTC offset at an instant.
pub const PROBE_ZONE_OFFSET: &str = "probe.zone_offset";

/// The local zone's offset at an instant, as [`PROBE_ZONE_OFFSET`].
pub const PROBE_LOCAL_OFFSET: &str = "probe.local_offset_at";

/// The local zone's offset now (`date +%z`), read once a run.
pub const PROBE_TZ_OFFSET: &str = "probe.tz_offset";

/// Task-result snapshot write door at a tasked session's done edge.
pub const TASK_RESULT: &str = "task.result";

/// Probe for the machine's sleep count since boot.
pub const PROBE_ASLEEP: &str = "probe.asleep";

/// Probe for the timestamps of the machine's last sleep and wake.
pub const PROBE_LAST_SLEEP: &str = "probe.last_sleep";

/// Probe for the wake the machine is in: a full one (graphics up) or macOS's dark wake.
pub const PROBE_FULL_WAKE: &str = "probe.full_wake";
