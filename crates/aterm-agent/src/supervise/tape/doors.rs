// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE DOORS' NAMES: the word a tape's record names the place a value crossed
//! the loop's boundary by — the `door` of an astream `Exchange`, `Emit` or
//! `Raised` record.
//!
//! A replay checks each request at the door it was recorded at, and a later
//! build must still read a tape an earlier one wrote, so these names are
//! APPEND-ONLY: a door renamed here would make every tape recorded before the
//! rename diverge at its first crossing. A door that goes away keeps its name
//! taken. On the tape a name is spelled once per file and referred to by
//! number after that ([`super::codec`]), so a name's length costs nothing per
//! crossing.
//!
//! The replay plan's §4.3 is the table of what crosses each door and how a
//! replay answers it.

/// The loop's control-socket lane: a request (`args`) out, a `CtlReply` or a
/// transport error back ([`super::codec::ctl_request`],
/// [`super::codec::ctl_reply`]).
pub const CTL: &str = "ctl";

/// The teller's lane (`story`), its own `Ctl` beside the loop's.
pub const TELLER: &str = "teller";

/// A call into the window's host (`IdleHost`), as `host.<method>`: a query
/// as a round trip (its arguments out, its answer back), a notification as
/// an output (`super::host`). The method's own name follows, so a method's
/// door is as stable as its name; the trait's macro spells this prefix as a
/// literal (`concat!`), which
/// `host_door::tests::a_method_the_trait_grows_is_taped_by_construction`
/// holds to this one.
pub const HOST: &str = "host.";

/// Whether `door` is one of the host's calls that are ACTS on the session
/// (the replay plan's §5.1): the host's step at an idle point (`at_idle`,
/// `at_idle_on`: an upgrade's notice, a relaunch, a carry-on typed), the
/// line it may type at a break of background work (`at_background`), a
/// restart it makes for the policy, a limit reset it lets the loop take, a
/// `/usage` probe it lets the loop type. Like a control act, each closes the
/// tape's frame after it, so what the session was done is on disk before
/// what follows; and a replay that differs at one differs in a decision.
///
/// Which calls those are is said where each is declared — marked `act` in
/// the trait's method list — and read here from the list the trait's macro
/// writes from the marks (`run::host_door::HOST_ACTS`): a method added as an
/// act is one by construction, never missing from a list kept beside it.
#[must_use]
pub fn is_host_act(door: &str) -> bool {
    crate::supervise::run::host_door::HOST_ACTS.contains(&door)
}

/// A derived policy probe (the git view, a worker variable),
/// as `probe.<name>`.
pub const PROBE: &str = "probe.";

/// The approval ledger's bound (`approvals::bound`): a write the loop makes,
/// recorded with its outcome, compared and never performed on a replay.
pub const WRITE_BOUND: &str = "write.bound";

/// A sleep: the request is its length; a replay returns at once.
pub const SLEEP: &str = "sleep";

/// The stop flag another thread raises: taped once, at the first true read.
pub const RAISED_STOP: &str = "raised.stop";

/// The hand-over flag, read once at the run's end.
pub const RAISED_HANDOVER: &str = "raised.handover";

/// A line the loop printed: a round trip, the line out and the write's
/// outcome back — a line the loop could not print ends it, so the outcome is a
/// value the loop observes.
pub const OUT: &str = "out";

/// A journal row the loop wrote.
pub const JOURNAL: &str = "journal";

/// An approval-ledger row the loop wrote.
pub const LEDGER: &str = "ledger";

/// A line the loop wrote to its notes file: a round trip, as [`OUT`] — a notes
/// file that cannot be written ends the loop.
pub const NOTES: &str = "notes";

/// A decision on an approval box: its outcome, the judged screen's record
/// and the rule that decided.
pub const DECIDE_APPROVAL: &str = "decide.approval";

/// A decision on a question box, as [`DECIDE_APPROVAL`].
pub const DECIDE_QUESTION: &str = "decide.question";

/// A decision at a turn's end, as [`DECIDE_APPROVAL`].
pub const DECIDE_TURN_END: &str = "decide.turn_end";

/// The approval policy's probe for a hidden twin of a Read's name on its way
/// (`policy::probe`): the absolute path as the run's roots write it out, the
/// twin's path or nothing (or why the folder could not be listed) back —
/// never a folder's listing.
pub const PROBE_HIDDEN_TWIN: &str = "probe.hidden_twin";

/// The approval policy's probe for where a Read's symbolic links lead: the
/// path out, its resolved components back.
pub const PROBE_REAL_COMPONENTS: &str = "probe.real_components";

/// The rm rule's probe for where an operand's links lead as `rm` walks them:
/// the path out, its components and whether a link on the way is not root's
/// back.
pub const PROBE_RM_PHYSICAL: &str = "probe.rm_physical";

/// The worker read through the kernel and bound to its session: the group
/// and session out, the worker's digest and pid (or why it is not the
/// worker, which names no variable's value) back — never its environment.
pub const PROBE_WORKER_PROC: &str = "probe.worker_proc";

/// What a git read would load in a directory: the worker's digest, the
/// directory and where the worker may write out, the view PROJECTED to the
/// keys the policy reads back (`GitView::project`).
pub const PROBE_GIT_VIEW: &str = "probe.git_view";

/// Whether Claude Code's settings give the Bash tool a git-relevant variable
/// the worker lacks: the worker's digest and launch directory out, the
/// verdict back.
pub const PROBE_SETTINGS_ENV: &str = "probe.settings_env";

/// What the worker's Bash tool's shell starts with: the aliases and
/// functions found (or what fails every line, a line named by its number and
/// the word that failed it) back — never an rc file's text.
pub const PROBE_SHELL_STARTUP: &str = "probe.shell_startup";

/// The ask rules and PreToolUse hooks of the agent's own settings.
pub const PROBE_OWNER_ASKS: &str = "probe.owner_asks";

/// Where the worker's Bash tool may stand, from Claude Code's transcripts.
pub const PROBE_SHELL_CWDS: &str = "probe.shell_cwds";

/// A named zone's offset from UTC at an instant: the zone and instant out,
/// the offset back.
pub const PROBE_ZONE_OFFSET: &str = "probe.zone_offset";

/// The local zone's offset at an instant, as [`PROBE_ZONE_OFFSET`].
pub const PROBE_LOCAL_OFFSET: &str = "probe.local_offset_at";

/// The local zone's offset now (`date +%z`), read once a run.
pub const PROBE_TZ_OFFSET: &str = "probe.tz_offset";
