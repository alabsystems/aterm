// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE WATCH LOOP — the actuator and the timing over the grid spine
//! (design `docs/DESIGN-aterm-wrapper-2026-09-17.md` §5.3, §5.4, §5.6, §5.8,
//! §5.8.10).
//!
//! # Nothing in this file samples on a clock
//!
//! There is no `sleep`, no `Instant::now`, no interval and no cadence here.
//! Ingress is a PARKED stream plus the `await` family for deadlines:
//!
//! * a pushed frame — `DELTA`, `EVENT`, `GAP` — arrives on the subscribe
//!   handle the host already holds ([`Frame`], [`parse_frame`]);
//! * every deadline is ONE parked `await … timeout=<ms>` ([`Arm`],
//!   [`Watcher::next_arm`]) and `OK timeout` IS the verdict firing
//!   ([`Wake::Deadline`]), while a latch that arrives early ends the wait with
//!   strictly more information for strictly less work ([`Wake::Latched`]);
//! * a vendor hook is PUSHED by the vendor when hooks exist at all
//!   ([`Wake::Hook`]) and creates no state this loop cannot reach without it.
//!
//! [`AWAIT_CLAMP_MS`] is the server's own clamp (600 000 ms), so any deadline
//! up to ten minutes arms in one call and a longer one re-arms at the clamp —
//! one wake per ten minutes is a bounded re-arm, not a poll, and
//! [`Watcher::next_arm`] is the only place a duration is ever computed.
//!
//! The honest limit the design records is honoured verbatim: **a phase change
//! is NOT pushed on the events digest** (`timeline_wire_kind` maps only
//! `meta-change`, `closing` and the fabric kinds), so the phase is READ from
//! `status`, gated on `revision` having moved, and never awaited. That read is
//! [`super::observe::Observer::read`], whose gate is `needs_grid`.
//!
//! # The spine, with zero hooks
//!
//! Every capability here still works with NO hook installed, degraded but
//! alive (design §0.2, corrected 2026-09-21). Stall classification, the warn
//! ladder, the limit classes, the model switch, the re-login door and the
//! escalation all run off `status` + the revision-gated grid read. What hooks
//! add is exactness, and the two places they are load-bearing SAY SO:
//! [`Liveness::StoppedShort`] is unreachable without them
//! ([`LivenessVerdict::available`] answers `false`) and the Stop-block rung is
//! the hook's own decision channel ([`Rung::StopBlock`] needs
//! [`LivenessState::stop_seen_at`], which only a hook sets).
//!
//! # Every act obeys §4.3
//!
//! Journal a row BEFORE, take the turn lease, respect `hold`, the inject floor
//! and the generation, write the verdict AFTER. Three of those are SERVER-SIDE
//! and this loop holds no local copy of any of them: `hold` is the server's
//! `ERR halted`, a live driver is its `ERR busy`, the inject floor is its
//! `ERR rate`. The loop's part is to SEND under the lease and to read the
//! stated word back faithfully ([`refusal_of`]) — a refusal the server states
//! and the ledger spells `timeout` is the failure mode that read-back exists
//! to prevent. Typing is allowed under those
//! guards (owner ruling D1'). Two rules are absolute and are pinned by tests:
//! **the harness never types `y` at an approval** — answering a permission
//! prompt is the vendor's own channel — and **it never presses Enter bare**:
//! every typed act is the fenced composition of §4.3 (`turn … submit=none`
//! then `key if=<composer-row> enter`), which reproduces the operator's
//! check-under-one-lock fence with shipped verbs.
//!
//! # What it reuses rather than re-deriving (design §0.1)
//!
//! | Fact | Reader |
//! |---|---|
//! | the four judgments and the ordered recovery table | [`super::limits`] |
//! | the spine's events, the revision gate, the phase | [`super::observe`] |
//! | the four-state mark and in-band attribution | [`super::mark`] |
//! | the shell-line classifier | [`crate::supervise::classify`] |
//! | the composer-ready pattern the fence guards on | [`crate::claude_composer_ready_pattern`] |
//! | the line folder that keeps a quote inside one line | [`crate::supervise::limit::one_line`] |
//! | the JSON string escaper every ledger row is built with | [`crate::supervise::journal::json_str`] |
//! | the launch-nonce reader `turn id=` is fenced on | [`aterm_session::LaunchNonce`] |
//!
//! The composer pattern is NOT `crate::claude_prompt_ready_pattern`, and this
//! row said it was until 2026-09-22: the prompt-ready pattern also matches an
//! approval box's ` ❯ 1. Yes` option row, which is the 2026-09-21 defect —
//! a guarded Enter landing on a highlighted option.
//!
//! STATUS (docs/README.md honesty ratchet): unit-tested against a fake wire —
//! an overload storm, an exhausted window, a hung tool, a refusal for each
//! guard, both wake orders, and a source scan that pins the no-timer rule. No
//! socket is opened in this module: [`Wire`] is the seam — and the production
//! transport it used to say was still coming has landed as
//! [`super::wire::CtlWire`], driven by `aterm harness watch`, with one
//! hand-asked act reaching [`Watcher::execute`] through `aterm harness
//! switch`. What is still TARGET is the HOST: these are a process an operator
//! or an agent starts, not a supervisor the window runs on its own (design
//! §4.1).

use super::limits::{
    self, Action, Budget, Class, Evidence, Guards as LimitGuards, Ledger, LimitsConfig, RetryText,
};
use super::mark;
use super::observe::{self, Confidence, Observer, SessionPhase};
use super::source::Source;
use super::truncate_bytes;
use crate::supervise::journal::json_str;
use crate::supervise::prompt::parse_prompt;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// The server's own clamp on `await … timeout=` (control_session.rs:1355,
/// quoted by design §5.8.4). A deadline up to ten minutes arms in ONE call;
/// a longer one is re-armed at the clamp, which is a bounded re-arm and not a
/// poll — the loop wakes once per clamp, not once per tick.
pub const AWAIT_CLAMP_MS: u64 = 600_000;

/// The FLOOR on any computed `await … timeout=`. A parked wait shorter than
/// this is not a wait: it is a cadence with a clamp in front of it.
///
/// One second, not one millisecond, and the difference is the whole of the
/// NEVER-POLL law. [`Watcher::next_arm`] used to clamp into `1..=`, so any
/// deadline already in the past armed `timeout=1` and the loop turned into a
/// ~1000-pass-per-second spin — three control requests and one fresh socket
/// each — with no sleep anywhere in it. A moment in the past is now dropped
/// at the call site rather than floored; this floor covers the remaining
/// case, a moment that arrives while the pass is running.
pub const ARM_FLOOR_MS: u64 = 1_000;

/// The busy footer a Claude Code turn draws while it works. The same literal
/// `crate::supervise::run`'s `BUSY_FOOTER` waits on; it is private there, and
/// this is a wire token, so it carries `.` where the screen shows a space
/// (the guard grammar of §4.3: one wire token, `.` for a space).
pub const BUSY_FOOTER: &str = "esc.to.interrupt";

/// The guard an `escape` press is fenced on (design §5.4 rung 4, MEASURED
/// literal). The spaced spelling would arm `(?i)esc` and press a key named
/// `to`, which is why this is one token.
pub const ESCAPE_GUARD: &str = "(?i)esc.to.interrupt";

/// `submit_window=` on a `settle=gone:` turn, in milliseconds. NOT optional:
/// that settle is two waits, and arming the second in the gap would settle on
/// the PRE-response screen (the `turn` catalog entry, quoted by §4.3).
pub const SUBMIT_WINDOW_MS: u64 = 1_500;

/// `yield=<floor>` on every typed turn: the verb parks INSIDE itself and
/// answers `ERR yield timeout` having typed NOTHING, which is why the harness
/// never runs a standalone `await momentum` before a `turn` (§4.3).
pub const YIELD_FLOOR: &str = "0.2";

/// `tool_stall_s` as milliseconds (design §5.4). Exactly the server clamp, so
/// it arms in one call.
pub const TOOL_STALL_MS: u64 = 600_000;

/// `api_stall_s` as milliseconds (design §5.4).
pub const API_STALL_MS: u64 = 180_000;

/// `warn_s` as milliseconds: how long a warn holds before it badges attention.
pub const WARN_MS: u64 = 120_000;

/// `nudge_after_s` as milliseconds: rung 3 needs `phase != running` for this
/// long, and rung 3 is opt-in.
pub const NUDGE_AFTER_MS: u64 = 900_000;

/// `settle_s` as milliseconds: the turn-boundary wait before a switch.
pub const SETTLE_MS: u64 = 30_000;

/// The link `stop` hook blocks on `await inbox` for up to this long
/// (hook.rs:73-76, `timeout` 600). A `Stop` with no `SessionEnd` inside this
/// window is an EXPLANATION, never a symptom (design §5.4).
pub const STOP_HOOK_WAIT_MS: u64 = 600_000;

/// `stop.max_blocks` per session — below the vendor's own cap of 8.
pub const STOP_MAX_BLOCKS: u32 = 3;

/// The recovery ledger (design §4.4, §5.8.6).
pub const RING_RECOVERY: &str = "recovery";

/// The actuation ledger: the INTENT rows (design §4.4; the verdict half of a
/// driven turn is `history`'s own record, read back by id).
pub const RING_ACTUATION: &str = "actuation";

/// The byte cap on text quoted out of a screen into a ledger row or an `ask`.
pub const QUOTE_CAP: usize = 240;

/// The cooperative-lease holder this loop acquires AND releases as.
///
/// Named once because the two spellings must be the SAME string:
/// `lease_release` (`aterm-gui/src/control_session.rs`) releases a live
/// cooperative lease only when `!live || force || holder == Some(h)`, so a
/// bare `lease release` after a held acquire answers `ERR lease held by …`
/// and the lease survives its whole TTL.
pub const LEASE_HOLDER: &str = "harness:claude-harness";

/// How many pieces of limit evidence one generation may hold.
///
/// A generation lasts until the program exits, and `reclassify` walks the
/// whole vector on every new-evidence wake, so an uncapped vector is an
/// O(n)-per-wake cost with n growing all session. A banner that scrolls out
/// and back, or one carrying a figure that ticks (`Approaching usage limit ·
/// 12% left`), is a fresh row every time it is drawn. The classifier reads
/// the LAST `StopFailure`, the FIRST banner and any window, so a bounded
/// window of the newest evidence decides what an unbounded one would.
pub const MAX_EVIDENCE: usize = 64;

// ---------------------------------------------------------------------------
// Ingress — the pushed stream
// ---------------------------------------------------------------------------

/// One frame off the parked `subscribe` stream. These are the THREE shapes
/// the wire actually carries (subscribe.rs), parsed rather than guessed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    /// `DELTA <sid> seq=<n> …` — content moved.
    Delta {
        /// The content counter the frame carried. PER-GRID: an alt-screen
        /// round trip does not move the main grid's counter.
        seq: Option<u64>,
    },
    /// `EVENT <sid> <kind> …` — the lifecycle digest.
    Event {
        /// The digest kind (`turn`, `title`, `block-complete`, `bell`,
        /// `meta`, `closing`, `exited`, `harness`, `session-exited`, …).
        kind: String,
    },
    /// `GAP <sid> bytes-dropped=<n> | resync=<seq> | events-resync=<floor>` —
    /// state MAY have been dropped. A GAP is not quiet.
    Gap {
        /// `bytes-dropped=`.
        bytes_dropped: u64,
        /// `resync=`.
        resync: Option<u64>,
        /// `events-resync=`.
        events_resync: Option<u64>,
    },
}

/// Parse one wire line into a [`Frame`]. Anything else — `BYTES`, a styled
/// body row, a blank line — is `None`, which the loop treats as "not a frame
/// I act on" rather than as an error.
#[must_use]
pub fn parse_frame(line: &str) -> Option<Frame> {
    let line = line.trim_end_matches(['\r', '\n']);
    let mut words = line.split_whitespace();
    let tag = words.next()?;
    // `<sid>` (or `*` for the instance stream). Dropped: the loop watches one
    // session and the host routes.
    let _sid = words.next()?;
    let rest: Vec<&str> = words.collect();
    let field = |key: &str| -> Option<&str> {
        rest.iter()
            .find_map(|t| t.strip_prefix(key)?.strip_prefix('='))
    };
    match tag {
        "DELTA" => Some(Frame::Delta {
            seq: field("seq").and_then(|v| v.parse().ok()),
        }),
        "EVENT" => Some(Frame::Event {
            kind: truncate_bytes(rest.first()?, 64).to_string(),
        }),
        "GAP" => Some(Frame::Gap {
            bytes_dropped: field("bytes-dropped")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
            resync: field("resync").and_then(|v| v.parse().ok()),
            events_resync: field("events-resync").and_then(|v| v.parse().ok()),
        }),
        _ => None,
    }
}

/// A vendor hook that fired, as the bridge hands it over. ENRICHMENT: every
/// one of these RAISES confidence in something the spine already saw, and
/// none of them is the only way this loop learns anything (design §0.2).
#[derive(Debug, Clone, PartialEq)]
pub enum HookEvent {
    /// A `StopFailure` / `Notification` / `PostModelSwitch` / window row, as
    /// the limit classifier's own evidence vocabulary.
    Evidence(Evidence),
    /// A tool went out, with the exact id only a hook carries.
    PreToolUse {
        /// The vendor's `tool_use_id`.
        tool_use_id: String,
    },
    /// That tool came back.
    PostToolUse {
        /// The vendor's `tool_use_id`.
        tool_use_id: String,
    },
    /// A turn boundary, exactly. The Stop-block rung's ONLY carrier.
    Stop,
    /// The session ended; a pending `stop-hook-waiting` explanation is over.
    SessionEnd,
    /// The bridge reached us at all.
    SessionStart {
        /// The link hooks' metadata block was seen beside ours, so a held
        /// `Stop` has an explanation (design §5.4 (i)).
        link_hooks: bool,
    },
}

/// Which parked deadline a wake belongs to. Every one of these is an
/// `await … timeout=<ms>`; none is a sleep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Timer {
    /// `await seq <last_seq> timeout=<tool_stall_ms>` — a tool in flight.
    ToolStall,
    /// `await seq <last_seq> timeout=<api_stall_ms>` — a request that stalled.
    ApiStall,
    /// `await gone <busy footer> timeout=<settle_ms>` — the turn boundary an
    /// L3/L4 act must wait for.
    Boundary,
    /// The limits engine's next scheduled moment (a T1/T2 column, a
    /// `resets_at`, an armed `wait`, the `unknown` burst window).
    Schedule,
    /// `await gone <auth banner>` while a human walks through the vendor's own
    /// sign-in flow (design §5.8.10 step 4).
    ReloginWait,
    /// `warn_s` — the warn rung's own hold before it badges attention.
    Warn,
    /// The scheduled switch-back at `resets_at` + jitter (design §5.8.5 (a)).
    SwitchBack,
}

impl Timer {
    /// The ledger and `--json` spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Timer::ToolStall => "tool-stall",
            Timer::ApiStall => "api-stall",
            Timer::Boundary => "boundary",
            Timer::Schedule => "schedule",
            Timer::ReloginWait => "relogin-wait",
            Timer::Warn => "warn",
            Timer::SwitchBack => "switch-back",
        }
    }
}

/// ONE parked wait: the `await` condition, its timeout, and which deadline it
/// is. "Park at most one per driver" (the `await` catalog entry), so
/// [`Watcher::next_arm`] returns at most one of these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arm {
    /// The `await` operands, without the verb and without `timeout=`.
    pub cond: Vec<String>,
    /// `timeout=<ms>`, already clamped to [`AWAIT_CLAMP_MS`].
    pub timeout_ms: u64,
    /// Which deadline this is.
    pub timer: Timer,
}

impl Arm {
    /// The control line this arm sends, exactly.
    #[must_use]
    pub fn line(&self) -> String {
        format!("await {} timeout={}", self.cond.join(" "), self.timeout_ms)
    }
}

/// What woke the loop.
#[derive(Debug, Clone, PartialEq)]
pub enum Wake {
    /// A frame off the parked stream.
    Frame(Frame),
    /// A vendor hook, pushed through `harness event`.
    Hook(HookEvent),
    /// The armed `await` LATCHED: the predicate came true before its timeout.
    Latched(Timer),
    /// The armed `await` answered `OK timeout`. For a stall wait that IS the
    /// verdict (design §5.4); for a schedule it is the moment arriving.
    Deadline(Timer),
    /// `custody` says a human took the reading position or the highlight.
    Custody {
        /// `owner=user` with a `changed=` that names a person.
        user: bool,
    },
    /// The session's generation moved under the loop; everything pending is
    /// dropped.
    Generation(u64),
    /// The stream ended.
    Closed,
}

// ---------------------------------------------------------------------------
// Liveness (design §5.4)
// ---------------------------------------------------------------------------

/// Which switch `harness switch` asked for (design §5.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchKind {
    /// `/model <target>` at a turn boundary — reversible, L3.
    Model,
    /// A relaunch into the target account's config directory — L4, and
    /// gated on `[accounts] enabled` as well as on the level.
    Account,
}

impl SwitchKind {
    /// The word on the command line.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            SwitchKind::Model => "model",
            SwitchKind::Account => "account",
        }
    }

    /// Read one operand. `None` for anything else, so an invented third kind
    /// is a refusal rather than a default.
    #[must_use]
    pub fn parse(word: &str) -> Option<SwitchKind> {
        match word {
            "model" => Some(SwitchKind::Model),
            "account" => Some(SwitchKind::Account),
            _ => None,
        }
    }

    /// The recovery-table action this switch IS, so the journal row, the
    /// budget and the verdict are the engine's own and not a parallel set.
    #[must_use]
    pub fn action(self) -> Action {
        match self {
            SwitchKind::Model => Action::SwitchModel,
            SwitchKind::Account => Action::SwitchAccount,
        }
    }

    /// What the configured target reads as, for the journal row's reason.
    #[must_use]
    pub fn target(self, cfg: &WatchConfig) -> String {
        match self {
            SwitchKind::Model => cfg.model_alt.clone(),
            SwitchKind::Account => cfg.account_label.clone(),
        }
    }
}

/// The liveness state table of design §5.4, verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Liveness {
    /// Active work confirmed — PUSHED, never sampled.
    #[default]
    Working,
    /// A long tool with no progress.
    ToolStalled,
    /// A request stalled. None of the three signals proves a hang, which is
    /// why rung 1 is a warning and rung 3 is opt-in.
    ApiStalled,
    /// Ended early. **UNREACHABLE with zero hooks and this module says so**
    /// rather than faking it: `history` holds only turns a DRIVER submitted,
    /// and `search`/`offscreen` find a literal, not an intent.
    StoppedShort,
    /// Not a stall: the link `stop` hook is holding the turn on `await inbox`.
    StopHookWaiting,
    /// Not a stall: a permission prompt is up, or a human is reading.
    WaitingHuman,
    /// Hand to the limit classifier.
    QuotaWait,
    /// The limit classifier owns this; the ladder is paused and its timers
    /// frozen from classification until the class clears.
    NetworkOffline,
    /// A `GAP` said the window was UNOBSERVED. Not quiet — re-read, don't
    /// tear down.
    Unobserved,
}

impl Liveness {
    /// The ledger and `--json` spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Liveness::Working => "working",
            Liveness::ToolStalled => "tool-stalled",
            Liveness::ApiStalled => "api-stalled",
            Liveness::StoppedShort => "stopped-short",
            Liveness::StopHookWaiting => "stop-hook-waiting",
            Liveness::WaitingHuman => "waiting-human",
            Liveness::QuotaWait => "quota-wait",
            Liveness::NetworkOffline => "network-offline",
            Liveness::Unobserved => "unobserved",
        }
    }

    /// Is this a STALL — the only kind of verdict the ladder acts on? The
    /// explanations (`stop-hook-waiting`, `waiting-human`, `quota-wait`,
    /// `network-offline`, `unobserved`) are not, and never prod.
    #[must_use]
    pub fn is_stall(self) -> bool {
        matches!(
            self,
            Liveness::ToolStalled | Liveness::ApiStalled | Liveness::StoppedShort
        )
    }

    /// Is this verdict an EXPLANATION — a named reason the ladder must not
    /// prod — rather than a stall or the plain absence of one?
    ///
    /// `plan_ladder` walks no rung for any non-stall verdict, but the two
    /// halves of "non-stall" are not the same fact. [`Liveness::Working`] is
    /// the absence of a symptom; each of these is a REASON the session is
    /// quiet, and every one of them names something that owns the session
    /// right now — a person at an approval box, a dropped window, the limit
    /// classifier, the network, a blocked `stop` hook. A hand-asked rung may
    /// assert a symptom the loop has not seen yet; it may not overrule one of
    /// these, which is what [`Watcher::nudge`] used to do by computing the
    /// verdict and throwing it away.
    #[must_use]
    pub fn is_explained(self) -> bool {
        matches!(
            self,
            Liveness::StopHookWaiting
                | Liveness::WaitingHuman
                | Liveness::QuotaWait
                | Liveness::NetworkOffline
                | Liveness::Unobserved
        )
    }
}

/// What the ladder is allowed to reach. `liveness.level` ships at
/// [`Level::Stop`]: rungs 1-2 only. Rungs 3-4 cross the operator's actuator
/// line and are enabled per harness by the owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Level {
    /// Rung 1 only: warn, badge, journal.
    Warn,
    /// Rungs 1-2: plus the Stop-block, which is the hook's decision channel.
    #[default]
    Stop,
    /// Rungs 1-3: plus the typed nudge.
    Turn,
    /// Rungs 1-4: plus the guarded escape press.
    Escape,
}

impl Level {
    /// The config spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Warn => "warn",
            Level::Stop => "stop",
            Level::Turn => "turn",
            Level::Escape => "escape",
        }
    }

    /// The level a config spelling names, if any.
    #[must_use]
    pub fn parse(name: &str) -> Option<Level> {
        match name {
            "warn" => Some(Level::Warn),
            "stop" => Some(Level::Stop),
            "turn" => Some(Level::Turn),
            "escape" => Some(Level::Escape),
            _ => None,
        }
    }
}

/// One rung of the warn-then-keep-going ladder (design §5.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rung {
    /// L1: icon, description, notice; after `warn_s`, attention.
    Warn,
    /// L2, only at a `Stop` event: the block reply. HOOK-ONLY — it IS the
    /// vendor's decision channel, so with zero hooks this rung is off and the
    /// others still run.
    StopBlock,
    /// L3, opt-in: the typed probe. The operator RFC forbids this for the
    /// operator ("stuck probes → read-only always"); it is offered here only
    /// as an owner-enabled harness rung, never on by default.
    Nudge,
    /// L3, opt-in: the guarded `escape` press then one nudge; `tool-stalled`
    /// only, after two stall windows.
    Escape,
    /// L1 plus a fabric `ask`; the watchdog then stops acting on this
    /// generation.
    Escalate,
}

impl Rung {
    /// The ledger spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Rung::Warn => "warn",
            Rung::StopBlock => "stop-block",
            Rung::Nudge => "nudge",
            Rung::Escape => "escape",
            Rung::Escalate => "escalate",
        }
    }

    /// The actuator level of §4.3 this rung uses.
    #[must_use]
    pub fn level(self) -> u8 {
        match self {
            Rung::Warn | Rung::Escalate => 1,
            Rung::StopBlock => 2,
            Rung::Nudge | Rung::Escape => 3,
        }
    }

    /// The lowest `liveness.level` that reaches this rung.
    #[must_use]
    pub fn needs(self) -> Level {
        match self {
            // Escalate is the terminal rung and is reachable at every level:
            // a watcher that may only warn must still be able to tell a human
            // it has given up, or the ladder ends in silence.
            Rung::Warn | Rung::Escalate => Level::Warn,
            Rung::StopBlock => Level::Stop,
            Rung::Nudge => Level::Turn,
            Rung::Escape => Level::Escape,
        }
    }
}

/// The liveness verdict, with the evidence behind it. `available = false` is
/// the honest answer for [`Liveness::StoppedShort`] with no hooks: the design
/// reports `stopped-short: unavailable (hooks=absent)` rather than faking it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LivenessVerdict {
    /// The state.
    pub class: Liveness,
    /// How sure.
    pub confidence: Confidence,
    /// Why, from a closed vocabulary this module owns.
    pub reasons: Vec<&'static str>,
    /// Whether the state could be reached at all with the evidence in hand.
    pub available: bool,
}

/// Everything the watchdog carries between wakes. No clock lives here: every
/// field is a millisecond stamp the caller injected.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LivenessState {
    /// The last pushed frame or grid change, in host milliseconds.
    pub last_change_ms: u64,
    /// The last `seq` a `DELTA` carried, which the stall waits arm on.
    pub last_seq: Option<u64>,
    /// The `status revision=` at the last verdict. A stall verdict needs one
    /// revision change since the last one.
    pub revision_at_verdict: Option<u64>,
    /// The last verdict and when it was taken.
    pub last_verdict: Option<(Liveness, u64)>,
    /// A stall wait answered `OK timeout` — signal A of the two-signal rule.
    pub stall_timer_fired: Option<Timer>,
    /// A tool went out with this id and has not come back (hooks only).
    pub tool_use_id: Option<String>,
    /// A `Stop` was seen at this instant, with no `SessionEnd` since.
    pub stop_seen_at: Option<u64>,
    /// The link hooks' metadata block was seen: a held `Stop` has an
    /// explanation.
    pub link_hooks: bool,
    /// A `GAP` landed: the window was UNOBSERVED, the dwell and the
    /// `since_ms` baseline reset, and any verdict crossing it journals
    /// `degraded:gap`.
    pub gap_at: Option<u64>,
    /// The highest rung taken in this episode.
    pub rung: Option<Rung>,
    /// When the warn rung was taken, so `warn_s` can badge attention.
    pub warned_at: Option<u64>,
    /// The warn rung's second half — `meta set attention` after `warn_s` —
    /// has been taken. Without this the badge would be due for ever and the
    /// loop would re-take rung 1 on every wake.
    pub badged: bool,
    /// How many Stop-blocks this session has had.
    pub stop_blocks: u32,
    /// The `escape.budget` ledger.
    pub escapes: Ledger,
    /// A human is reading: the ladder is paused until this instant.
    pub paused_until_ms: Option<u64>,
}

impl LivenessState {
    /// Everything an episode owns, cleared. The budgets and the hook-channel
    /// facts are NOT episode state and survive.
    fn end_episode(&mut self) {
        self.stall_timer_fired = None;
        self.rung = None;
        self.warned_at = None;
        self.badged = false;
        self.last_verdict = None;
    }
}

/// What a liveness read is decided from. Injected, so the classifier is a
/// pure function of its arguments and the tests drive it from fixtures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LivenessInputs<'a> {
    /// The watchdog's carried state.
    pub state: &'a LivenessState,
    /// aterm's own phase word.
    pub phase: SessionPhase,
    /// aterm's `since_ms`.
    pub since_ms: Option<u64>,
    /// aterm's `revision=`.
    pub revision: Option<u64>,
    /// The limit class in force, where one is. `network-offline` pauses the
    /// ladder; every other class hands the session to the recovery table.
    pub class: Option<Class>,
    /// A permission box is on the grid (`aterm_phase::parse_prompt` said so).
    pub prompt: bool,
    /// Vendor hooks have fired into this session.
    pub hooks: bool,
    /// The host's millisecond clock.
    pub now_ms: u64,
    /// `tool_stall_ms` / `api_stall_ms`.
    pub tool_stall_ms: u64,
    /// See above.
    pub api_stall_ms: u64,
}

/// Classify liveness (design §5.4's table).
///
/// **"Quiet is not proof a process is stuck" is honoured**: every STALL
/// verdict needs two independent signals — the bounded `await` answering `OK
/// timeout` (signal A) AND aterm's own `since_ms` agreeing (signal B) — plus
/// one `revision` change since the last verdict, and a quiet that has an
/// EXPLANATION is not a signal at all. The three explanations of §5.4 are
/// checked first and each one short-circuits: the link `stop` hook's
/// `await inbox`, a `GAP`, and a human at the keyboard.
#[must_use]
pub fn classify_liveness(inp: &LivenessInputs) -> LivenessVerdict {
    let st = inp.state;
    let v = |class: Liveness, confidence: Confidence, reasons: Vec<&'static str>| LivenessVerdict {
        class,
        confidence,
        reasons,
        available: true,
    };
    // Explanation 1: the class the limit watcher owns. `network-offline`
    // pauses the ladder for the class's lifetime — quiet is the network, not
    // the agent.
    if inp.class == Some(Class::NetworkOffline) {
        return v(
            Liveness::NetworkOffline,
            Confidence::Strong,
            vec!["limit-class"],
        );
    }
    if let Some(c) = inp.class
        && c != Class::Unknown
    {
        return v(Liveness::QuotaWait, Confidence::Strong, vec!["limit-class"]);
    }
    // Explanation 2: a GAP. The window was UNOBSERVED, not quiet.
    if st.gap_at.is_some() {
        return v(
            Liveness::Unobserved,
            Confidence::Unknown,
            vec!["gap", "degraded:gap"],
        );
    }
    // Explanation 3: a human, or the vendor, is waiting on a person.
    if inp.prompt {
        return v(
            Liveness::WaitingHuman,
            Confidence::Strong,
            vec!["permission-prompt"],
        );
    }
    if st.paused_until_ms.is_some_and(|p| inp.now_ms < p) {
        return v(Liveness::WaitingHuman, Confidence::Strong, vec!["custody"]);
    }
    // Explanation 4: the link `stop` hook is holding the turn.
    if let Some(at) = st.stop_seen_at
        && st.link_hooks
        && inp.now_ms.saturating_sub(at) < STOP_HOOK_WAIT_MS
    {
        return v(
            Liveness::StopHookWaiting,
            Confidence::Strong,
            vec!["stop-without-session-end", "link-hooks"],
        );
    }
    // Signal A: a bounded await answered `OK timeout`.
    let Some(fired) = st.stall_timer_fired else {
        return v(Liveness::Working, Confidence::Strong, vec!["pushed-frame"]);
    };
    // The revision gate: one classification change since the last verdict.
    // Without it a second timeout on the same still screen would re-verdict
    // for ever and walk the ladder on no new evidence.
    if st.last_verdict.is_some() && st.revision_at_verdict == inp.revision {
        return v(
            Liveness::Working,
            Confidence::Heuristic,
            vec!["revision-unmoved"],
        );
    }
    // Signal B: aterm's own `since_ms` agrees with the wait that fired.
    let want = match fired {
        Timer::ToolStall => inp.tool_stall_ms,
        _ => inp.api_stall_ms,
    };
    let agrees = inp.since_ms.is_some_and(|s| s >= want);
    if !agrees {
        return v(
            Liveness::Working,
            Confidence::Heuristic,
            vec!["since_ms-disagrees"],
        );
    }
    match fired {
        Timer::ToolStall => {
            // With hooks the tool has an exact id; without them the same wait
            // fires at lower confidence and with no `tool_use_id`. Nothing is
            // unreachable — the fidelity drops.
            let exact = st.tool_use_id.is_some();
            LivenessVerdict {
                class: Liveness::ToolStalled,
                confidence: if exact {
                    Confidence::Strong
                } else {
                    Confidence::Heuristic
                },
                reasons: if exact {
                    vec!["await-timeout", "since_ms", "tool_use_id"]
                } else {
                    vec!["await-timeout", "since_ms", "hooks-absent"]
                },
                available: true,
            }
        }
        _ => {
            let mut reasons = vec!["await-timeout", "since_ms"];
            if inp.phase == SessionPhase::Running {
                reasons.push("phase=running");
            }
            v(Liveness::ApiStalled, Confidence::Heuristic, reasons)
        }
    }
}

/// Whether [`Liveness::StoppedShort`] can be reached at all. It cannot
/// without hooks, and the design says so rather than faking it: the turn
/// boundary survives as `await gone <busy footer>`, but "the last assistant
/// message" has no grid equivalent.
#[must_use]
pub fn stopped_short_available(hooks: bool) -> bool {
    hooks
}

// ---------------------------------------------------------------------------
// The acts
// ---------------------------------------------------------------------------

/// Is this string the `<epoch>` half of a `turn id=` the SERVER will parse?
///
/// MEASURED 2026-09-22 against the shipped parser: `pty_idem::parse_key`
/// (`aterm-gui/src/pty_idem.rs`) does `LaunchNonce::from_hex(epoch)?` and, on
/// `None`, answers `ERR usage: id=<epoch>:<producer>:<seq>` as the WHOLE
/// reply, before `attempt()` runs — so a turn id built from anything but a
/// 32-char lowercase-hex launch nonce types nothing at all. A session id
/// (`s-1e918c…`) is not one, and neither is the word `harness`.
///
/// The reader is [`aterm_session::LaunchNonce::from_hex`], the same one the
/// server uses; nothing about the shape is re-derived here.
#[must_use]
pub fn valid_turn_nonce(s: &str) -> bool {
    aterm_session::LaunchNonce::from_hex(s).is_some()
}

/// A turn id, in the one shape `turn` accepts: `<launch-nonce>:<producer>:
/// <seq>`, and it must LEAD the operand list (a trailing `id=` is typed as
/// text). The producer parses as a `u64` by construction.
///
/// The nonce half is NOT free-form: see [`valid_turn_nonce`]. [`Watcher::plan`]
/// refuses every typed act with [`Refuse::Unresolved`] when the configured
/// nonce does not satisfy it, rather than sending a line the server rejects
/// at parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnId {
    /// The target session's PUBLIC launch nonce, 32 lowercase hex characters,
    /// as the `local`/`sessions` roster row publishes it (`nonce=<hex32>`).
    /// It is NOT the session id and NOT a host-chosen word.
    pub nonce: String,
    /// The producer, a `u64`.
    pub producer: u64,
    /// The per-producer sequence.
    pub seq: u64,
}

impl TurnId {
    /// The wire spelling.
    #[must_use]
    pub fn as_string(&self) -> String {
        format!("{}:{}:{}", self.nonce, self.producer, self.seq)
    }
}

/// The CLOSED actuator vocabulary. Nothing outside this enum is ever sent, and
/// three absences are deliberate and permanent in ABI 1:
///
/// * there is no `y` and no bare `Enter` — answering an approval is the
///   vendor's channel and a bare submit is the operator's line (§4.3);
/// * there is no `close` and no `signal` — L5 is human-only and is not in any
///   harness's actuator vocabulary;
/// * there is no credential act — the re-login path types six characters and
///   the VENDOR opens its own browser flow (§5.8.10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Act {
    /// L1: `meta set icon <glyph>`.
    Icon(String),
    /// L1: `meta set description <text>`.
    Description(String),
    /// L1: `meta set attention aterm harness: <why>` — the badge the menu
    /// bar raises. The text ALWAYS opens with [`mark::STEM`] (§4.3 L1: every
    /// attention writer stamps a fixed prefix of its own), and it is never
    /// sent over a non-empty attention that does not ([`Watcher::pump`]
    /// reads `meta` first): the link Notification hook's `claude needs
    /// approval: …` and supervise's `limited: …` are other writers' badges.
    ///
    /// It used to carry no text at all and rendered a bare `meta set
    /// attention`, which the server answers `ERR usage` (`MetaWriteError::
    /// Empty`) — every warn and escalate rung would have failed on its badge
    /// the day the Wire got a transport.
    Attention(String),
    /// L1: `meta unset attention` — sent ONLY when the standing attention
    /// opens with [`mark::STEM`] ([`Watcher::pump`] reads `meta` first). The
    /// harness clears its own badge and never another writer's.
    ClearAttention,
    /// L1: `appnotice <lane> <text>`.
    Notice(String),
    /// L2: the decision JSON returned to the hook that asked. NOT a socket
    /// send — the bridge prints it — so it has no control line.
    StopBlock {
        /// The reason, opening with the harness's own attribution stem.
        reason: String,
    },
    /// L3, half one of the fence: `turn id=… presses=1 settle=gone:<footer>
    /// submit_window=<ms> yield=<floor> submit=none <text>`.
    Type {
        /// The turn id, which LEADS.
        id: TurnId,
        /// What is typed. Never screen-derived, never a row's own words.
        text: String,
    },
    /// L3, half two of the fence: `key if=<composer-row> enter`. Never bare.
    Submit {
        /// The one-wire-token guard.
        guard: String,
    },
    /// L3: `key if=<re> escape` — the guarded interrupt.
    Escape {
        /// The one-wire-token guard.
        guard: String,
    },
    /// L4: `spawn` a new session running the twin. The old session is left
    /// for the human — never `close`, never a signal.
    ///
    /// **UNSENDABLE TODAY.** [`Act::line`] answers `None` for it and both
    /// planners refuse [`Refuse::Unsupported`] before one is built: no
    /// control verb carries an environment into a new session, so the §5.6
    /// mechanism (a `spawn` of the twin whose PRELUDE selects
    /// `CLAUDE_CONFIG_DIR`) waits on the §4.1 host. The variant stays because
    /// the recovery table's shape is the design's and a `harness recover
    /// switch-account` must still be able to PRINT what it would do.
    Relaunch {
        /// The account label this relaunch selects.
        label: String,
        /// `CLAUDE_CONFIG_DIR` for the new session.
        config_dir: String,
        /// `--resume <session-id|transcript path>`, where one is known.
        resume: Option<String>,
        /// `--model`, where the switch asked for one.
        model: Option<String>,
    },
    /// L1: `post to=<principal> kind=ask <text>`. With `fabric=absent` this
    /// QUEUES and answers `no-bridge=1`; the honest report is "queued", never
    /// "sent" and never re-posted.
    Ask {
        /// The principal.
        to: String,
        /// The question.
        text: String,
    },
}

impl Act {
    /// The actuator level of §4.3.
    #[must_use]
    pub fn level(&self) -> u8 {
        match self {
            Act::Icon(_)
            | Act::Description(_)
            | Act::Attention(_)
            | Act::ClearAttention
            | Act::Notice(_)
            | Act::Ask { .. } => 1,
            Act::StopBlock { .. } => 2,
            Act::Type { .. } | Act::Submit { .. } | Act::Escape { .. } => 3,
            Act::Relaunch { .. } => 4,
        }
    }

    /// The word the tab label shows while this act is open (`◆ … · typing`).
    #[must_use]
    pub fn verb(&self) -> &'static str {
        match self {
            Act::Icon(_) | Act::Description(_) | Act::Notice(_) => "annotating",
            Act::Attention(_) => "flagging",
            Act::ClearAttention => "unflagging",
            Act::StopBlock { .. } => "answering stop",
            Act::Type { .. } | Act::Submit { .. } => "typing",
            Act::Escape { .. } => "interrupting",
            Act::Relaunch { .. } => "relaunching",
            Act::Ask { .. } => "escalating",
        }
    }

    /// The control line this act sends, or `None` for an act that is answered
    /// in-band rather than over the socket ([`Act::StopBlock`]).
    #[must_use]
    pub fn line(&self) -> Option<String> {
        match self {
            Act::Icon(g) => Some(format!(
                "meta set icon {}",
                super::one_line(g, mark::ICON_CAP)
            )),
            Act::Description(d) => Some(format!(
                "meta set description {}",
                super::one_line(d, mark::DESCRIPTION_CAP)
            )),
            Act::Attention(why) => Some(format!("meta set attention {}", attention_text(why))),
            Act::ClearAttention => Some("meta unset attention".to_string()),
            Act::Notice(t) => Some(format!(
                "appnotice {} {}",
                mark::APPNOTICE_LANE,
                super::one_line(t, mark::DESCRIPTION_CAP)
            )),
            Act::StopBlock { .. } => None,
            // `id=` LEADS; `submit_window=` is not optional on a `settle=gone:`
            // turn; `submit=none` because the Enter is the fenced press below.
            //
            // `--` is the parser's OPTION SENTINEL and is not optional either.
            // MEASURED 2026-09-22 against the shipped parser
            // (`aterm-gui/src/control_session.rs`): `turn` consumes leading
            // `k=v` tokens and breaks at the first token that is not a known
            // option, so a text beginning with `timeout=1` or `yield=9` would
            // be EATEN as an option — silently not typed, and changing this
            // turn's own timing on the way. The shipped caller at
            // `control.rs` already spells the sentinel; this one did not.
            Act::Type { id, text } => Some(format!(
                "turn id={} presses=1 settle=gone:{BUSY_FOOTER} submit_window={SUBMIT_WINDOW_MS} \
                 yield={YIELD_FLOOR} submit=none -- {}",
                id.as_string(),
                super::one_line(text, QUOTE_CAP)
            )),
            Act::Submit { guard } => Some(format!("key if={guard} enter")),
            Act::Escape { guard } => Some(format!("key if={guard} escape")),
            // THERE IS NO LINE. Corrected 2026-09-22: this arm used to build
            // `spawn env=CLAUDE_CONFIG_DIR=<dir> cmd=claude`, and neither key
            // exists. MEASURED against a live instance —
            //
            // ```text
            // $ aterm ctl spawn env=CLAUDE_CONFIG_DIR=/Users//…/.claude-alt cmd=claude
            // ERR usage: spawn [window=<id>] [raise=<t|f>] [cwd=<path>] …
            // ```
            //
            // — and against the parser: `parse_spawn_args`
            // (`aterm-gui/src/control_media.rs`) admits `cwd|identity|window|
            // raise|split|connected|place|of` and answers `Err(())` for
            // anything else. So every rotation failed at PARSE, was read back
            // as `refused:unresolved` (the same word the roster uses for "no
            // candidate account"), and still consumed the switch budget.
            //
            // The mechanism design §5.6 actually specifies is a `spawn` of the
            // TWIN whose PRELUDE selects `CLAUDE_CONFIG_DIR`. The twin and the
            // prelude seam are TARGET (§4.1's host), so until one exists there
            // is no admitted line that rotates an account, and `plan_limits`
            // and [`Watcher::switch`] refuse [`Refuse::Unsupported`] before an
            // act is ever built. Emitting `spawn cwd=<dir>` instead would be
            // worse than nothing: it starts a session on the DEFAULT account,
            // which is the exhausted one the rotation is rotating away from.
            Act::Relaunch { .. } => None,
            Act::Ask { to, text } => Some(format!(
                "post to={to} kind=ask {}",
                super::one_line(text, QUOTE_CAP)
            )),
        }
    }
}

/// The server's cap on `attention` (`meta set attention` takes 256 bytes).
pub const ATTENTION_CAP: usize = 256;

/// The attention text an [`Act::Attention`] sets: `aterm harness: <why>`,
/// folded to one line and bound to [`ATTENTION_CAP`] AS A WHOLE, so the
/// stem always survives the cut and the server never answers `too long`.
#[must_use]
pub fn attention_text(why: &str) -> String {
    let why = super::one_line(why, ATTENTION_CAP);
    let text = if why.is_empty() {
        mark::STEM.to_string()
    } else {
        format!("{}: {why}", mark::STEM)
    };
    super::one_line(&text, ATTENTION_CAP)
}

/// Whether a standing attention (decoded, `None` when unset) is one the
/// harness wrote: it opens with [`mark::STEM`].
#[must_use]
pub fn attention_is_ours(standing: Option<&str>) -> bool {
    standing.is_some_and(|t| t.starts_with(mark::STEM))
}

/// The read that gates every attention act: `meta`, then the standing
/// attention decoded. `Ok` when the act may go — a set over nothing or over
/// the harness's own badge, a clear over the harness's own badge — and
/// otherwise the ledger word for why it did not: `attention-kept` (another
/// writer's badge stands, e.g. the link hook's `claude needs approval: …`
/// or supervise's `limited: …`), `attention-none` (a clear with nothing to
/// clear), `attention-unread` (the read was refused, so nothing proves the
/// badge is ours). Sends exactly one line, the bare `meta`.
pub fn attention_gate(w: &mut dyn Wire, act: &Act) -> Result<(), &'static str> {
    let Ok(reply) = w.send("meta") else {
        return Err("attention-unread");
    };
    let standing = crate::supervise::run::standing_attention(&reply);
    let ours = attention_is_ours(standing.as_deref());
    match (act, standing.is_some()) {
        (Act::ClearAttention, false) => Err("attention-none"),
        (_, false) => Ok(()),
        (_, true) if ours => Ok(()),
        (_, true) => Err("attention-kept"),
    }
}

/// The fenced composition of §4.3: a `turn` that submits NOTHING, then a
/// guarded `enter`. This is the ONLY way this module types, and the reason is
/// shipped: a generic `turn` submits with an unfenced Enter, and only the
/// operator route re-checks the generation and rejects approval-shaped
/// screens under the lock — so every typed act reproduces that fence with
/// shipped verbs.
///
/// The operator's fence has THREE parts and only the generation half is
/// reproducible with `turn id=`. The approval half is owed HERE, in two
/// places, because `key if=` has no negative form:
///
/// * the guard is [`crate::claude_composer_ready_pattern`], which an option
///   row (` ❯ 1. Yes`) does not match — the plain caret pattern did, and a
///   bare Enter on it selects the highlighted option;
/// * [`Watcher::plan_limits`] refuses every L3 act outright while
///   `parse_prompt` reads a box on the grid, because the box and a composer
///   row can be on screen together and no row pattern separates that.
#[must_use]
pub fn fenced(id: TurnId, text: &str) -> Vec<Act> {
    vec![
        Act::Type {
            id,
            text: text.to_string(),
        },
        Act::Submit {
            guard: crate::claude_composer_ready_pattern().to_string(),
        },
    ]
}

/// The compensating clear of §4.3's atomicity: `turn … submit=none` types
/// WITHOUT submitting and does not clear the composer (`control_verbs`:
/// "types WITHOUT submitting"), so a fence whose Enter was refused leaves the
/// typed text sitting there and the NEXT row's Enter would submit both
/// concatenated. The clear is guarded on the composer the same way the Enter
/// was, so it writes nothing when there is no composer row to clear.
#[must_use]
pub fn compose_clear_line() -> String {
    format!("key if={} ctrl+u", crate::claude_composer_ready_pattern())
}

// ---------------------------------------------------------------------------
// The guarded actuator (design §4.3, §5.8.6)
// ---------------------------------------------------------------------------

/// Why an act was refused. The ledger spellings of §5.8.6, plus the two this
/// loop adds and names: `refused:custody` (a human is reading or selecting
/// right now — a state no keystroke signal sees) and `refused:level` (the
/// rung is above `liveness.level`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refuse {
    /// A standing halt: every PTY-reaching verb answers `ERR halted`.
    Hold,
    /// A live turn or a cooperative lease: not a boundary.
    Busy,
    /// `custody` says `owner=user`; defer.
    Custody,
    /// The request's generation is not the state's.
    Generation,
    /// The capability, the rung or the action is switched off.
    Disabled,
    /// The rung is above `liveness.level`.
    Level,
    /// One automatic action already awaits its verdict; queued, not acted.
    InFlight,
    /// The mark reads `bypassed`: the harness is not live and observes
    /// nothing.
    Bypassed,
    /// The budget is dry. A switch DEGRADES to L1 rather than refusing;
    /// `escape` has its own bucket and is refused outright.
    Budget,
    /// Terminal for this generation: a human owns it now.
    Terminal,
    /// An approval box is on the grid: the keyboard belongs to the person
    /// being asked, and no automatic act may type while it is up.
    WaitingHuman,
    /// The act names a thing this host cannot resolve — an account label with
    /// no config directory behind it, or a turn id whose `<epoch>` is not a
    /// launch nonce ([`valid_turn_nonce`]). Refused rather than sent
    /// half-built.
    Unresolved,
    /// The server's GUARD said no: `key if=<re>` found no matching row and
    /// answered `OK skipped`. That is a success REPLY and a refused ACT, and
    /// collapsing the two is what let a half-landed fence be recorded as
    /// `executed` with the composer still dirty.
    Skipped,
    /// The idempotency key had already been seen (`OK … dup=1`), so the
    /// server replayed the earlier outcome and typed NOTHING this time. The
    /// fenced Enter must not follow: there is no text of ours to submit.
    Duplicate,
    /// The line did not reach the session intact — `ERR write failed
    /// partial accepted=…`. Neither a guard nor a timeout.
    Transport,
    /// The SERVER'S GRAMMAR does not admit the line this act would need, so
    /// there is no line to send. Distinct from [`Refuse::Unresolved`] on
    /// purpose: "I have no candidate" and "the verb I would use does not
    /// exist" are different facts about different halves of the system, and
    /// spelling both `refused:unresolved` is how a dead capability read as a
    /// configured one for a whole release (the account rotation emitted
    /// `spawn env=… cmd=…`, which `parse_spawn_args` answers `ERR usage:`
    /// — MEASURED 2026-09-22 against a live instance).
    Unsupported,
}

impl Refuse {
    /// The ledger verdict spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Refuse::Hold => "refused:hold",
            Refuse::Busy => "refused:busy",
            Refuse::Custody => "refused:custody",
            Refuse::Generation => "refused:generation",
            Refuse::Disabled => "refused:disabled",
            Refuse::Level => "refused:level",
            Refuse::InFlight => "refused:in-flight",
            Refuse::Bypassed => "refused:bypassed",
            Refuse::Budget => "refused:budget",
            Refuse::Terminal => "refused:terminal",
            Refuse::WaitingHuman => "refused:waiting-human",
            Refuse::Unresolved => "refused:unresolved",
            Refuse::Skipped => "refused:skipped",
            Refuse::Duplicate => "refused:duplicate",
            Refuse::Transport => "refused:transport",
            Refuse::Unsupported => "refused:unsupported",
        }
    }
}

/// The refusal a server error line STATES, read as a word and never as a
/// substring.
///
/// The control protocol frames a refusal as `ERR <word> …` — `ERR halted
/// reason=…`, `ERR busy turn=…`, `ERR rate (self-feed floor)` — so the word
/// after `ERR` is the cause. A `contains("rate")` scan over free-form text
/// calls `separate`, `corporate` and any path or model id holding those five
/// letters a rate refusal, and spells a standing halt `refused:busy`; the
/// ledger then hides which of three different causes stopped the act.
///
/// `None` means the reply is not a stated refusal: the act went out and did
/// not come back, which is a timeout, not a guard. A line with NO `ERR` word
/// in it is not a stated refusal and answers `None` — there is no first-token
/// fallback, because reading the first word of any free-form line as a
/// refusal word is the same defect as the substring scan, moved one step.
/// MEASURED 2026-09-22: the old fallback read `rate limited by the vendor` as
/// `refused:budget`.
///
/// The words are the ones the shipped verbs actually state. Four were missing
/// until 2026-09-22 and each one was spelled `timeout` in the ledger — a word
/// whose documented meaning is "the act went out and did not come back":
///
/// | stated line | verdict |
/// |---|---|
/// | `ERR usage: id=<epoch>:<producer>:<seq>` | `refused:unresolved` |
/// | `ERR epoch …` | `refused:generation` |
/// | `ERR lease held by …` | `refused:busy` |
/// | `ERR write failed partial accepted=…` | `refused:transport` |
#[must_use]
pub fn refusal_of(err: &str) -> Option<Refuse> {
    let words: Vec<&str> = err.split_whitespace().collect();
    let i = words.iter().position(|w| *w == "ERR")?;
    let word = words.get(i + 1).copied()?;
    match word.trim_end_matches(&[':', ','][..]) {
        "halted" => Some(Refuse::Hold),
        "busy" | "lease" => Some(Refuse::Busy),
        "rate" => Some(Refuse::Budget),
        "usage" => Some(Refuse::Unresolved),
        "epoch" => Some(Refuse::Generation),
        "write" => Some(Refuse::Transport),
        _ => None,
    }
}

/// Did a SUCCESS reply say the guard declined? `key if=<re>` answers
/// `OK skipped` when no row on the grid matches, and `guarded_input_reply`
/// (`aterm-gui/src/control_input.rs`) is where that word is minted; the
/// server itself models it as a distinct outcome (`pty_idem::guarded` calls
/// `skipped_by_guard` and RELEASES the key rather than marking it applied).
#[must_use]
pub fn reply_skipped(reply: &str) -> bool {
    reply
        .lines()
        .next()
        .is_some_and(|l| l.trim_end() == "OK skipped" || l.starts_with("OK skipped "))
}

/// Did a SUCCESS reply say the key was a REPLAY? `turn`/`key` answer
/// `OK … dup=1` when the idempotency key has been seen before: the earlier
/// outcome is replayed and nothing is typed this time. The catalog states
/// that a duplicate "carries NONE of the verdict fields", so a `dup=1` turn
/// must not be followed by the fenced Enter — there is no text of ours on the
/// screen to submit, and the Enter would land on whatever composer row IS
/// there, including a person's unsent draft.
#[must_use]
pub fn reply_duplicate(reply: &str) -> bool {
    reply.lines().next().is_some_and(|l| {
        l.starts_with("OK")
            && l.split_whitespace()
                .any(|w| w.trim_end_matches(&[',', ';'][..]) == "dup=1")
    })
}

impl From<limits::Refusal> for Refuse {
    fn from(r: limits::Refusal) -> Refuse {
        match r {
            limits::Refusal::StaleGeneration => Refuse::Generation,
            limits::Refusal::Disabled | limits::Refusal::Exhausted => Refuse::Disabled,
            limits::Refusal::Hold => Refuse::Hold,
            limits::Refusal::Busy => Refuse::Busy,
            limits::Refusal::SelfTarget => Refuse::Disabled,
            limits::Refusal::InFlight => Refuse::InFlight,
            limits::Refusal::Terminal(_) => Refuse::Terminal,
        }
    }
}

/// The SERVER-SIDE facts every act is gated on (design §4.3: refusals key on
/// server-side facts only, because an Owner connection is anonymous by
/// construction and a "caller sid == target sid" rule is not computable).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HostGuards {
    /// `status hold=1`.
    pub hold: bool,
    /// `who` answered anything but `driving=-`.
    pub busy: bool,
    /// `custody owner=user` with a `changed=` that names a person.
    pub custody_user: bool,
    /// The session's generation as the host reads it.
    pub generation: u64,
    /// The mark is not `bypassed`.
    pub engaged: bool,
}

/// One planned act, with the journal row's fields already decided. Nothing is
/// sent until the journal row is written (§4.3's order: journal, act,
/// verdict).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planned {
    /// What to send, in order.
    pub acts: Vec<Act>,
    /// The capability that owns it (`limits` or `liveness`).
    pub cap: &'static str,
    /// The action or rung name, for the ledger.
    pub action: String,
    /// The level actually used — below the action's own where a dry budget
    /// degraded it.
    pub level: u8,
    /// Why, naming the table row or the rung.
    pub reason: String,
    /// Whether the turn lease is held across the act.
    pub lease: bool,
    /// The limits table index this came from, where it did.
    pub step: Option<usize>,
    /// The limits action this came from, where it did.
    pub limit_action: Option<Action>,
    /// The ladder rung this came from, where it did.
    pub rung: Option<Rung>,
    /// For an armed `wait`: when the wait ends, as the engine computed it.
    /// Dropping this is what made a `retry` unreachable: `retry`'s own timing
    /// gate reads the wait the engine armed, and a `None` here armed nothing.
    pub wait_until: Option<i64>,
}

/// What one decision pass concluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// Nothing is due. The loop parks on its arm; an idle session costs
    /// nothing.
    Idle,
    /// Something is due later. The loop arms THIS instant as a deadline — it
    /// never counts down to it.
    Wait {
        /// The host-millisecond instant.
        until_ms: u64,
        /// Which deadline.
        timer: Timer,
    },
    /// Act, under the guards that already passed.
    Act(Box<Planned>),
    /// Refused, and the ledger says why.
    Refused {
        /// The spelling.
        why: Refuse,
        /// What was refused.
        what: String,
    },
}

/// The act that is out, awaiting its verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenAct {
    /// The journal row that explains it.
    pub id: u64,
    /// Which capability.
    pub cap: &'static str,
    /// The action or rung name.
    pub action: String,
    /// The generation it was decided against.
    pub generation: u64,
    /// When the journal row was written.
    pub began_ms: u64,
    /// The limits action, where it came from the recovery table.
    pub limit_action: Option<Action>,
    /// The rung, where it came from the ladder.
    pub rung: Option<Rung>,
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// `[cap.liveness]` plus the two identity fields a typed act needs. The limit
/// side's knobs are [`LimitsConfig`] and are NOT copied here — one home per
/// key (design §4.6.2's rule, applied to config as well as to the switch).
// No `PartialEq`: `zone_offset` is a function pointer, and comparing two of
// those does not produce a meaningful result (their addresses are not unique
// across codegen units). A config is compared field by field where a test
// needs to.
#[derive(Debug, Clone)]
pub struct WatchConfig {
    /// Is the watchdog on at all?
    pub enabled: bool,
    /// How far the ladder may reach.
    pub level: Level,
    /// `tool_stall_s`, in milliseconds.
    pub tool_stall_ms: u64,
    /// `api_stall_s`, in milliseconds.
    pub api_stall_ms: u64,
    /// `warn_s`, in milliseconds.
    pub warn_ms: u64,
    /// `nudge_after_s`, in milliseconds.
    pub nudge_after_ms: u64,
    /// `settle_s`, in milliseconds: the turn-boundary wait before an act.
    pub settle_ms: u64,
    /// `stop.max_blocks` per session.
    pub stop_max_blocks: u32,
    /// `escape.budget`.
    pub escape_budget: Budget,
    /// The principal an escalation is addressed to.
    pub principal: String,
    /// The TARGET SESSION's public launch nonce (`nonce=<hex32>` on its
    /// `local`/`sessions` roster row), for every turn id.
    ///
    /// It is the host's job to read it — one roster read at adopt time,
    /// alongside the generation — and an EMPTY string is the honest default
    /// for a host that has not. Anything that is not 32 lowercase hex
    /// characters refuses every typed act at [`Watcher::plan`] with
    /// [`Refuse::Unresolved`], the same way an unresolved account directory
    /// refuses a rotation, because the server would reject the line at parse
    /// and the ledger would then blame a `timeout`. The default was the word
    /// `harness` until 2026-09-22 and the only shipped filler was the session
    /// id; neither parses.
    pub nonce: String,
    /// The host's producer id, for every turn id.
    pub producer: u64,
    /// The alternative model `switch-model` types.
    pub model_alt: String,
    /// The primary model the switch-back types.
    pub model_primary: String,
    /// The account label a `switch-account` relaunch names.
    pub account_label: String,
    /// Is account rotation switched on at all? OFF by default, and the ONE
    /// place that answers it: `limits::Guards::accounts_enabled` is what
    /// skips `switch-account`, and before this field existed the argument was
    /// hard-coded `false` here, so the action was dead in production while
    /// its table row and its tests read as live.
    pub accounts_enabled: bool,
    /// `CLAUDE_CONFIG_DIR` for the account [`Self::account_label`] names,
    /// resolved by the host from §5.6's `accounts.toml`. `None` means the
    /// label resolved to nothing, and a rotation is then REFUSED
    /// ([`Refuse::Unresolved`]) rather than relaunched on an empty config
    /// directory, which is the default account — the opposite of a rotation.
    pub account_dir: Option<String>,
    /// Seconds east of UTC, injected: this module owns no clock and no zone.
    pub local_offset_s: i64,
    /// How a named zone in a banner is placed, injected. The shipped reader
    /// is [`crate::supervise::limit::zone_offset_s`], which is what
    /// `supervise::run` defaults to; a test injects a table instead.
    pub zone_offset: fn(&str) -> Option<i64>,
}

impl Default for WatchConfig {
    fn default() -> WatchConfig {
        WatchConfig {
            enabled: true,
            level: Level::Stop,
            tool_stall_ms: TOOL_STALL_MS,
            api_stall_ms: API_STALL_MS,
            warn_ms: WARN_MS,
            nudge_after_ms: NUDGE_AFTER_MS,
            settle_ms: SETTLE_MS,
            stop_max_blocks: STOP_MAX_BLOCKS,
            escape_budget: Budget {
                count: 2,
                window_s: 3600,
            },
            principal: "owner".to_string(),
            nonce: String::new(),
            producer: 1,
            model_alt: "opus".to_string(),
            model_primary: "fable".to_string(),
            account_label: "next".to_string(),
            accounts_enabled: false,
            account_dir: None,
            local_offset_s: 0,
            zone_offset: crate::supervise::limit::zone_offset_s,
        }
    }
}

// ---------------------------------------------------------------------------
// The watcher
// ---------------------------------------------------------------------------

/// The loop's whole state for one `(sid, generation)`.
///
/// Everything is keyed by `(sid, generation)`; a decision computed against an
/// older generation is DROPPED rather than applied late (design §4.2).
#[derive(Debug, Clone)]
pub struct Watcher {
    cfg: WatchConfig,
    limits_cfg: LimitsConfig,
    sid: String,
    generation: u64,
    obs: Observer,
    limits: Option<limits::State>,
    carry: limits::Carry,
    evidence: Vec<Evidence>,
    live: LivenessState,
    open: Option<OpenAct>,
    hooks: bool,
    seq: u64,
    switch_back_at: Option<i64>,
    switched_model: bool,
    resync_due: bool,
    closed: bool,
    last_status: observe::StatusSample,
    prompt: bool,
    /// The class that was last seen to PAUSE the liveness ladder, so the
    /// pause and its lift are each journalled ONCE. A limit class
    /// short-circuits `classify_liveness`, which turns the stall ladder off
    /// for the class's lifetime; without this the capability stopped watching
    /// with nothing on the record saying so.
    ladder_paused: Option<Class>,
}

impl Watcher {
    /// A fresh watcher for one session.
    #[must_use]
    pub fn new(sid: &str, cfg: WatchConfig, limits_cfg: LimitsConfig) -> Watcher {
        Watcher {
            cfg,
            limits_cfg,
            sid: sid.to_string(),
            generation: 0,
            obs: Observer::new(),
            limits: None,
            carry: limits::Carry::default(),
            evidence: Vec::new(),
            live: LivenessState::default(),
            open: None,
            hooks: false,
            seq: 0,
            switch_back_at: None,
            switched_model: false,
            resync_due: false,
            closed: false,
            last_status: observe::StatusSample::default(),
            prompt: false,
            ladder_paused: None,
        }
    }

    /// The session this watcher watches.
    #[must_use]
    pub fn sid(&self) -> &str {
        &self.sid
    }

    /// The generation every decision is bound to.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Has a vendor hook ever fired into this loop? Reported BESIDE the mark,
    /// never folded into it: a harness with zero hooks is alive.
    #[must_use]
    pub fn hooks(&self) -> bool {
        self.hooks
    }

    /// The watchdog's carried state.
    #[must_use]
    pub fn liveness_state(&self) -> &LivenessState {
        &self.live
    }

    /// The limit engine's state, where a class is live. TEST-ONLY: nothing
    /// outside this module's own tests reads it, so it is not advertised on
    /// the public surface as if a host did.
    #[must_use]
    #[cfg(test)]
    pub(crate) fn limits_state(&self) -> Option<&limits::State> {
        self.limits.as_ref()
    }

    /// The act awaiting its verdict, where one is out. TEST-ONLY, for the
    /// reason [`Self::limits_state`] gives.
    #[must_use]
    #[cfg(test)]
    pub(crate) fn open_act(&self) -> Option<&OpenAct> {
        self.open.as_ref()
    }

    /// Does the loop owe a resync — a `GAP` said state MAY have been dropped,
    /// so the next pass re-reads `screen` once and reconciles against the
    /// resumable ledgers rather than counting a dropped frame as silence?
    #[must_use]
    pub fn resync_due(&self) -> bool {
        self.resync_due
    }

    /// The stream ended.
    #[must_use]
    pub fn closed(&self) -> bool {
        self.closed
    }

    /// The configuration in force.
    #[must_use]
    pub fn config(&self) -> &WatchConfig {
        &self.cfg
    }

    /// Name the model a `switch-model` types and the account label a
    /// `switch-account` relaunch carries. The account's config DIR is
    /// §5.6's and is not read here.
    pub fn set_target(&mut self, target: &str) {
        self.cfg.model_alt = target.to_string();
        self.cfg.account_label = target.to_string();
    }

    /// Adopt the TARGET SESSION's public launch nonce, which the host reads
    /// once at adopt time off the `local`/`sessions` roster row
    /// ([`super::wire::CtlWire`] does).
    ///
    /// An EMPTY or malformed value is NOT written: leaving the previous one
    /// in place is the safe answer, because the only thing an unparseable
    /// nonce can do is turn every typed act into `refused:unresolved`, and
    /// silently overwriting a good one with a failed read would do exactly
    /// that.
    pub fn adopt_nonce(&mut self, nonce: &str) {
        if valid_turn_nonce(nonce) {
            self.cfg.nonce = nonce.to_string();
        }
    }

    /// Set how far the ladder may reach. The loop's own level is
    /// `liveness.level` and ships at [`Level::Stop`]; a rung asked for BY
    /// HAND names its own, and the guards below it are unchanged.
    pub fn set_level(&mut self, level: Level) {
        self.cfg.level = level;
    }

    /// aterm's own last `status` reading — the spine's contract, mirrored
    /// rather than re-spelled (design §5.7: the overlapping fields carry the
    /// same `phase`/`confidence`/`reasons` vocabulary, never a second one).
    #[must_use]
    pub fn status(&self) -> &observe::StatusSample {
        &self.last_status
    }

    /// Fold one piece of limit evidence that did NOT arrive on a hook — a
    /// banner the spine read, a window a statusLine sample carried, a row
    /// replayed out of the ledger.
    ///
    /// It deliberately does NOT set `hooks`: a spine reading is not a hook
    /// firing, and reporting one as the other is exactly the conflation the
    /// central law exists to prevent.
    pub fn observed(&mut self, evidence: Evidence, now_s: i64) {
        self.push_evidence(evidence);
        self.reclassify(now_s);
    }

    /// Record one piece of evidence, BOUNDED and de-duplicated. `true` when
    /// the vector actually changed, which is what "new evidence" means to
    /// [`Self::reclassify`].
    ///
    /// Two rules, both toward the safe answer:
    ///
    /// * a `Banner` identical to one already held (same text, same parsed
    ///   reset) is NOT new — the screen was redrawn, nothing happened. Hook
    ///   values are never de-duplicated: three `StopFailure`s are three
    ///   failures and the classifier counts them.
    /// * at [`MAX_EVIDENCE`] the OLDEST banner makes room. A banner arriving
    ///   into a full vector of hook values is dropped rather than evicting
    ///   one — not because the banner is weaker evidence (since 2026-09-19
    ///   it is rank 1 and completes a pair like any other channel, §5.8.2)
    ///   but because it is the one kind that can be READ AGAIN: the grid is
    ///   still there and `limit_notice` will hand it over on the next
    ///   revision, while a hook fires once and is gone. Hook values are also
    ///   COUNTED — three `overloaded`s are a storm — so evicting one loses a
    ///   fact the classifier cannot recover.
    fn push_evidence(&mut self, e: Evidence) -> bool {
        if let Evidence::Banner { text, resets_at } = &e
            && self.evidence.iter().any(|held| {
                matches!(held, Evidence::Banner { text: t, resets_at: r }
                    if t == text && r == resets_at)
            })
        {
            return false;
        }
        // A PAINTED window is one window, however many times the panel is
        // redrawn: the newest reading REPLACES the one held for that kind,
        // and an unchanged figure is not new evidence. Without this the
        // revision-gated grid read would fill the vector with copies of one
        // frame and evict the hook values, which is the opposite of what
        // `MAX_EVIDENCE` is for.
        if let Evidence::Window {
            which,
            source: Source::Grid,
            ..
        } = &e
            && let Some(at) = self.evidence.iter().position(|held| {
                matches!(
                    held,
                    Evidence::Window { which: w, source: Source::Grid, .. } if w == which
                )
            })
        {
            if self.evidence[at] == e {
                return false;
            }
            self.evidence[at] = e;
            return true;
        }
        if self.evidence.len() >= MAX_EVIDENCE {
            match self
                .evidence
                .iter()
                .position(|held| matches!(held, Evidence::Banner { .. }))
            {
                Some(i) => {
                    self.evidence.remove(i);
                }
                None if matches!(e, Evidence::Banner { .. }) => return false,
                // Nothing but hook values, and this is another one: the
                // oldest goes, because the classifier reads the newest.
                None => {
                    self.evidence.remove(0);
                }
            }
        }
        self.evidence.push(e);
        true
    }

    /// The acts one recovery-table action becomes for the class in force.
    /// `None` when no class is live — there is nothing to recover from, and
    /// saying so is not the same as naming `unknown`.
    #[must_use]
    pub fn acts_of(&self, action: Action, level: u8, now_s: i64) -> Option<Vec<Act>> {
        let st = self.limits.as_ref()?;
        Some(self.acts_for(action, level, st, now_s))
    }

    /// ONE ladder rung, asked for by hand (design §5.7's `harness nudge`).
    ///
    /// It goes through the SAME guarded path the loop uses — the level, the
    /// custody read, the escape bucket — so a manual rung can reach nothing
    /// an automatic one could not.
    #[must_use]
    pub fn nudge(&self, rung: Rung, host: &HostGuards, now_ms: u64) -> Plan {
        if !self.cfg.enabled {
            return Plan::Refused {
                why: Refuse::Disabled,
                what: rung.as_str().to_string(),
            };
        }
        if !host.engaged {
            return Plan::Refused {
                why: Refuse::Bypassed,
                what: rung.as_str().to_string(),
            };
        }
        if rung.needs() > self.cfg.level {
            return Plan::Refused {
                why: Refuse::Level,
                what: rung.as_str().to_string(),
            };
        }
        let verdict = self.liveness(now_ms);
        if verdict.class.is_explained() {
            // THE LADDER'S OWN ENTRY CONDITION, which this path used to skip.
            // `plan_ladder` walks no rung when the verdict is an EXPLANATION
            // — `waiting-human` (an approval box), `unobserved` (a GAP),
            // `quota-wait`, `network-offline`, `stop-hook-waiting` — and the
            // docstring above promises a manual rung reaches nothing an
            // automatic one could. It did not: `nudge` computed the verdict
            // and threw it away, so `--level turn` reached an L3 `turn` with
            // an approval box on the grid.
            //
            // `working` is deliberately NOT refused here: it is the absence
            // of a symptom, not a reason, and a hand-asked rung is an
            // operator asserting one the loop has not seen yet. The fences
            // that still hold on that path are `rung_plan`'s custody and
            // approval-box tests and the launch-nonce fence.
            //
            // The refusal names the verdict's own class rather than a generic
            // word, so the operator learns WHY.
            return Plan::Refused {
                why: match verdict.class {
                    Liveness::WaitingHuman => Refuse::WaitingHuman,
                    Liveness::QuotaWait | Liveness::NetworkOffline => Refuse::Terminal,
                    _ => Refuse::Unresolved,
                },
                what: format!("{}: {}", rung.as_str(), verdict.class.as_str()),
            };
        }
        let plan = self.rung_plan(rung, &verdict, host, false);
        // A hand-asked rung reaches nothing the loop could not, and that
        // includes the last fence: `Rung::Nudge` types.
        self.fence_turn_nonce(plan)
    }

    /// ONE switch, asked for BY HAND (design §5.7's `harness switch <model|
    /// account> <target>`).
    ///
    /// It is the sibling of [`Self::nudge`] and obeys the same rule: a
    /// manual act reaches nothing an automatic one could not. Every gate the
    /// engine applies to a `switch-*` candidate is applied here, in the same
    /// order and against the same values —
    ///
    /// * `enabled`, `engaged`, the generation and the one-in-flight rule,
    ///   from [`Self::plan`]'s own head;
    /// * `hold` and the terminal state, from [`limits::step`]'s head;
    /// * the action's level against `cap.limits.level`, `[accounts] enabled`
    ///   for `switch-account`, `min_dwell_s` and the shared switch budget,
    ///   from the engine's per-action `guard` — read from [`limits::Carry`],
    ///   which is where `last_switch_at` and the switch ledger live whether
    ///   or not a class is currently in force;
    /// * `busy`, `custody`, the approval box and the account directory, from
    ///   [`Self::plan_limits`]'s L3/L4 fences;
    /// * the launch-nonce fence, from [`Self::fence_turn_nonce`].
    ///
    /// TWO deliberate differences from the automatic path, both toward the
    /// safe answer:
    ///
    /// * a DRY budget refuses (`refused:budget`) instead of degrading to the
    ///   L1 display the engine falls back to. Degrading is how an automatic
    ///   table keeps announcing while it waits; a person who typed `switch`
    ///   asked for the switch, and painting a description instead would read
    ///   as success;
    /// * the `self=1` rule is stated and does not fire, exactly as
    ///   [`Self::limit_guards`] states it: the host acts on the session it
    ///   OBSERVES, and a caller-sid test is not computable for an anonymous
    ///   Owner connection (§4.3 (c)). It is named here so its absence is a
    ///   decision on the record rather than an omission.
    #[must_use]
    pub fn switch(&self, kind: SwitchKind, host: &HostGuards, now_s: i64) -> Plan {
        let action = kind.action();
        let what = action.as_str().to_string();
        let refuse = |why: Refuse| Plan::Refused {
            why,
            what: what.clone(),
        };
        if !self.limits_cfg.enabled {
            return refuse(Refuse::Disabled);
        }
        if !host.engaged {
            return refuse(Refuse::Bypassed);
        }
        if host.generation != self.generation {
            return refuse(Refuse::Generation);
        }
        if self.open.is_some() {
            return refuse(Refuse::InFlight);
        }
        if host.hold {
            return refuse(Refuse::Hold);
        }
        if self.limits.as_ref().and_then(|st| st.terminal).is_some() {
            return refuse(Refuse::Terminal);
        }
        if action.level() > self.limits_cfg.level {
            return refuse(Refuse::Level);
        }
        if kind == SwitchKind::Account && !self.cfg.accounts_enabled {
            return refuse(Refuse::Disabled);
        }
        if kind == SwitchKind::Account {
            // No admitted control line rotates an account (see [`Act::line`]),
            // so the hand-asked path refuses exactly where the automatic one
            // does and by the same name. Stated ABOVE the dwell and budget
            // gates on purpose: a capability that cannot act must not spend
            // the allowance shared with `switch-model`.
            return refuse(Refuse::Unsupported);
        }
        if let Some(last) = self.carry.last_switch_at
            && now_s < last.saturating_add(i64::try_from(self.limits_cfg.min_dwell_s).unwrap_or(0))
        {
            return Plan::Refused {
                why: Refuse::Budget,
                what: format!("{what} within min_dwell_s"),
            };
        }
        if self.carry.switches.left(self.limits_cfg.budget, now_s) == 0 {
            return refuse(Refuse::Budget);
        }
        if host.busy {
            return refuse(Refuse::Busy);
        }
        if host.custody_user {
            return refuse(Refuse::Custody);
        }
        if self.prompt {
            // The approval box is the vendor's channel and the keyboard is
            // the person's. A typed act here would land an Enter on the
            // highlighted option — the CRITICAL defect this fence exists for.
            return refuse(Refuse::WaitingHuman);
        }
        let acts = match kind {
            SwitchKind::Model => self.fence(&format!("/model {}", self.cfg.model_alt)),
            // Unreachable: the account arm returned `refused:unsupported`
            // above. Restated rather than left as a live branch, so no
            // rotation can be assembled from this path by accident.
            SwitchKind::Account => return refuse(Refuse::Unsupported),
        };
        self.fence_turn_nonce(Plan::Act(Box::new(Planned {
            acts,
            cap: "limits",
            action: what.clone(),
            level: action.level(),
            reason: format!(
                "harness switch {} {}",
                kind.as_str(),
                kind.target(&self.cfg)
            ),
            lease: action.level() >= 3,
            step: None,
            limit_action: Some(action),
            rung: None,
            wait_until: None,
        })))
    }

    /// The limits configuration in force. TEST-ONLY, for the reason
    /// [`Self::limits_state`] gives.
    #[must_use]
    #[cfg(test)]
    pub(crate) fn limits_config(&self) -> &LimitsConfig {
        &self.limits_cfg
    }

    /// Fold one wake into the state. Returns the notes worth journaling.
    ///
    /// Ordering is deliberate and is what makes a pushed frame and a hook
    /// event NOT race: both fold into the SAME state here and nothing is
    /// decided until [`Self::plan`] runs afterwards, so the two orders
    /// converge. A hook carries exactness; a frame carries the fact.
    pub fn wake(&mut self, wake: &Wake, now_ms: u64, now_s: i64) -> Vec<&'static str> {
        let mut notes = Vec::new();
        match wake {
            Wake::Frame(Frame::Delta { seq }) => {
                self.live.last_change_ms = now_ms;
                if seq.is_some() {
                    self.live.last_seq = *seq;
                }
                self.live.stall_timer_fired = None;
                self.live.gap_at = None;
                notes.push("delta");
            }
            Wake::Frame(Frame::Event { kind }) => {
                self.live.last_change_ms = now_ms;
                self.live.stall_timer_fired = None;
                match kind.as_str() {
                    "exited" | "closing" | "session-exited" => {
                        self.closed = true;
                        notes.push("exited");
                    }
                    _ => notes.push("event"),
                }
            }
            Wake::Frame(Frame::Gap { .. }) => {
                // A GAP is NOT quiet. Mark the window UNOBSERVED, reset the
                // dwell and the `since_ms` baseline, and owe a resync — a
                // dropped frame and a stalled agent are otherwise
                // indistinguishable at the classifier's input.
                self.live.gap_at = Some(now_ms);
                self.live.last_change_ms = now_ms;
                self.live.stall_timer_fired = None;
                self.resync_due = true;
                notes.push("degraded:gap");
            }
            Wake::Hook(h) => {
                self.hooks = true;
                self.fold_hook(h, now_ms, now_s);
                notes.push("hook");
            }
            Wake::Latched(t) => {
                // A latch is the predicate coming true EARLY: strictly more
                // information for strictly less work. It never makes a stall.
                self.live.stall_timer_fired = None;
                if matches!(t, Timer::ToolStall | Timer::ApiStall) {
                    self.live.last_change_ms = now_ms;
                }
                notes.push("latched");
            }
            Wake::Deadline(t) => {
                if matches!(t, Timer::ToolStall | Timer::ApiStall) {
                    // `OK timeout` on a bounded await IS the stall verdict's
                    // first signal.
                    self.live.stall_timer_fired = Some(*t);
                }
                notes.push("deadline");
            }
            Wake::Custody { user } => {
                if *user {
                    self.live.paused_until_ms =
                        Some(now_ms.saturating_add(limits::HUMAN_PAUSE_S as u64 * 1000));
                    if let Some(st) = &mut self.limits {
                        st.observe(&limits::Event::HumanKeystroke, now_s);
                    }
                    notes.push("custody");
                }
            }
            Wake::Generation(g) => {
                self.adopt_generation(*g, now_s);
                notes.push("generation");
            }
            Wake::Closed => {
                self.closed = true;
                notes.push("closed");
            }
        }
        notes
    }

    /// The generation moved: every pending decision is dropped, the limit
    /// state is retired with its carry kept, and the episode ends.
    fn adopt_generation(&mut self, generation: u64, now_s: i64) {
        if generation == self.generation {
            return;
        }
        self.generation = generation;
        if let Some(st) = &mut self.limits {
            st.observe(&limits::Event::GenerationChanged(generation), now_s);
            self.carry = st.carry();
        }
        self.limits = None;
        self.evidence.clear();
        self.open = None;
        self.live.end_episode();
    }

    fn fold_hook(&mut self, h: &HookEvent, now_ms: u64, now_s: i64) {
        match h {
            HookEvent::Evidence(e) => {
                self.push_evidence(e.clone());
                self.reclassify(now_s);
            }
            HookEvent::PreToolUse { tool_use_id } => {
                self.live.tool_use_id = Some(truncate_bytes(tool_use_id, 64).to_string());
                self.live.last_change_ms = now_ms;
            }
            HookEvent::PostToolUse { .. } => {
                self.live.tool_use_id = None;
                self.live.last_change_ms = now_ms;
                self.live.stall_timer_fired = None;
                self.live.end_episode();
            }
            HookEvent::Stop => {
                self.live.stop_seen_at = Some(now_ms);
                self.live.last_change_ms = now_ms;
            }
            HookEvent::SessionEnd => {
                self.live.stop_seen_at = None;
                self.closed = true;
            }
            HookEvent::SessionStart { link_hooks } => {
                self.live.link_hooks = *link_hooks;
                self.live.stop_seen_at = None;
            }
        }
    }

    /// ONE spine read, through the SHIPPED gate: `status` always (378 B, the
    /// change detector) and the grid ONLY where `revision` moved, which is
    /// [`Observer::read`]'s own rule and therefore not a second copy of it.
    ///
    /// What comes back is folded by [`Self::fold_sample`] — the SAME folder
    /// [`Self::sample`] uses, which is the point. These were two folders
    /// until 2026-09-22: this one tee'd the two `CtlReply`s on their way past,
    /// cloned both, re-parsed them, and folded a SUBSET of what the other
    /// folded. MEASURED 2026-09-22 in release, that duplicated work — two
    /// reply clones plus a second parse of the status line and of a 3,669-byte
    /// 40-row grid — cost 10.16 µs of every pass, and the sample it re-derived
    /// was already parsed inside [`Observer::read`]. The subset was a defect,
    /// not a shortcut: the vendor's painted `/usage` figures — rank-1 limit
    /// evidence, and the only source that carries the Fable bucket at all —
    /// were read by `sample`, which only tests call, and never by the live
    /// loop.
    pub fn read_spine(
        &mut self,
        w: &mut dyn observe::Introspect,
        now_ms: u64,
        now_s: i64,
    ) -> Vec<observe::Event> {
        let Ok(read) = self.obs.read(w, now_ms) else {
            // `status` did not parse: there is no sample, so there is nothing
            // to fold and nothing to say. The next pass retries.
            return Vec::new();
        };
        self.fold_sample(&read.sample, &read.events, now_ms, now_s);
        read.events
    }

    /// Fold a grid-spine sample: the events it implies, and the limit
    /// evidence its banners carry. The caller gates the GRID read on
    /// `revision` having moved ([`Observer::needs_grid`]); this folds what
    /// came back.
    pub fn sample(
        &mut self,
        sample: &observe::Sample,
        now_ms: u64,
        now_s: i64,
    ) -> Vec<observe::Event> {
        let events = self.obs.on_sample(sample, now_ms);
        self.fold_sample(sample, &events, now_ms, now_s);
        events
    }

    /// THE ONE FOLDER over a spine sample and the events it implied, whether
    /// the sample came off a transport ([`Self::read_spine`]) or from a
    /// caller that already had one ([`Self::sample`]).
    fn fold_sample(
        &mut self,
        sample: &observe::Sample,
        events: &[observe::Event],
        now_ms: u64,
        now_s: i64,
    ) {
        self.last_status = sample.status.clone();
        // THE INGRESS PREDICATE'S ONE SOURCE. The subscribed stream is
        // `events`, which carries no `DELTA` (`DELTA` belongs to
        // `screen|cells|bytes`), so `Wake::Frame(Frame::Delta)` never fires
        // against a real wire and `last_seq` stayed `None` for the whole life
        // of the loop. Every arm was then `await seq 0` — a predicate already
        // true, answered `OK seq <n>` at once — so no deadline could fire, the
        // stall ladder was unreachable in production, and `pump` spun.
        // `status` carries `seq=` and the loop already reads `status` every
        // pass, so the fix is to read the field that was already on the wire.
        if let Some(seq) = sample.status.seq {
            self.live.last_seq = Some(seq);
        }
        let mut painted = false;
        if let Some(g) = &sample.grid {
            // The approval box, from the SHIPPED reader. It is read to KNOW a
            // person is being asked — never to answer, which is the vendor's
            // own channel (§0.2's asymmetry).
            self.prompt = parse_prompt(&g.rows).is_some();
            // A GAP owed a re-read; one landed, so the window is observed
            // again. Until then the verdict stays `unobserved`.
            self.resync_due = false;
            self.live.gap_at = None;
            // The figures the vendor PAINTED on its `/usage` panel: rank-1
            // evidence that survives `--bare`, and the only source that
            // carries the Fable bucket at all (design §5.8.2's correction).
            // Read here rather than in `fold_events` because it is a read of
            // the GRID, and the caller already gated that read on the status
            // revision having moved.
            for e in limits::Evidence::windows_from_screen(
                &g.rows,
                now_s,
                self.cfg.local_offset_s,
                self.cfg.zone_offset,
            ) {
                painted |= self.push_evidence(e);
            }
        }
        self.fold_events(events, now_ms, now_s);
        if painted {
            self.reclassify(now_s);
        }
    }

    /// The half of a spine pass that is pure over the events: the quiet clock,
    /// the banners that are limit evidence, and the exit.
    fn fold_events(&mut self, events: &[observe::Event], now_ms: u64, now_s: i64) {
        let mut new_evidence = false;
        for e in events {
            match &e.kind {
                // Only OUTPUT MOVING refutes a stall. A turn boundary is a
                // boundary, not evidence the screen moved: the first read
                // after a timeout sits on the same still screen and would
                // otherwise cancel the verdict that read was taken to
                // confirm.
                observe::EventKind::OutputMoved { .. } => {
                    self.live.last_change_ms = now_ms;
                    self.live.stall_timer_fired = None;
                }
                observe::EventKind::TurnBegan | observe::EventKind::TurnEnded => {
                    self.live.last_change_ms = now_ms;
                }
                observe::EventKind::Banner { kind, text, reset } => {
                    if *kind != observe::BannerKind::Permission {
                        // A banner is BELIEVED as evidence about what was
                        // drawn and never OBEYED as an instruction.
                        new_evidence |= self.push_evidence(Evidence::Banner {
                            text: text.clone(),
                            resets_at: reset.as_deref().and_then(|r| {
                                limits::banner_reset_at(
                                    r,
                                    now_s,
                                    self.cfg.local_offset_s,
                                    self.cfg.zone_offset,
                                )
                            }),
                        });
                    }
                }
                observe::EventKind::Exited { .. } => self.closed = true,
                observe::EventKind::Started { .. } | observe::EventKind::Quiet { .. } => {}
            }
        }
        if new_evidence {
            self.reclassify(now_s);
        }
    }

    /// Re-run the classifier over everything seen this generation and fold the
    /// verdict into the engine's state. The CLASSIFIER is
    /// [`limits::classify`]; no second judgment is made here.
    fn reclassify(&mut self, now_s: i64) {
        let Some(c) = limits::classify(&self.evidence, now_s) else {
            return;
        };
        match &mut self.limits {
            None => {
                self.limits = Some(limits::State::new(
                    &c,
                    now_s,
                    self.generation,
                    self.carry.clone(),
                ));
            }
            Some(st) => st.observe(&limits::Event::Reclassified(c), now_s),
        }
    }

    /// The limit class in force, where one is.
    #[must_use]
    pub fn class(&self) -> Option<Class> {
        self.limits.as_ref().map(|s| s.class)
    }

    /// The liveness verdict as of `now_ms`.
    #[must_use]
    pub fn liveness(&self, now_ms: u64) -> LivenessVerdict {
        classify_liveness(&LivenessInputs {
            state: &self.live,
            phase: self.last_status.phase,
            since_ms: self.last_status.since_ms,
            revision: self.last_status.revision,
            class: self.class(),
            prompt: self.prompt,
            hooks: self.hooks,
            now_ms,
            tool_stall_ms: self.cfg.tool_stall_ms,
            api_stall_ms: self.cfg.api_stall_ms,
        })
    }

    /// The ONE parked wait, or `None` when there is nothing to wait for and
    /// the loop simply parks on the pushed stream.
    ///
    /// This is the only place in the module that computes a duration, and
    /// every duration it computes becomes an `await … timeout=<ms>` rather
    /// than a sleep. A deadline past [`AWAIT_CLAMP_MS`] arms AT the clamp and
    /// is re-armed on the wake — a bounded re-arm, not a poll.
    #[must_use]
    pub fn next_arm(&self, now_ms: u64, now_s: i64) -> Option<Arm> {
        if self.closed {
            return None;
        }
        let seq = self.live.last_seq;
        // An act that is out owes its boundary wait first.
        if self.open.is_some() {
            return Some(Arm {
                cond: vec!["gone".to_string(), BUSY_FOOTER.to_string()],
                timeout_ms: self.cfg.settle_ms.min(AWAIT_CLAMP_MS),
                timer: Timer::Boundary,
            });
        }
        // The re-login wait: the harness stops acting and watches for the
        // auth banner to leave. It polls nothing and types nothing.
        if let Some(st) = &self.limits
            && st.relogin_at.is_some()
        {
            return Some(Arm {
                cond: vec!["gone".to_string(), AUTH_BANNER.to_string()],
                timeout_ms: (self.limits_cfg.relogin_wait_s.saturating_mul(1000))
                    .min(AWAIT_CLAMP_MS),
                timer: Timer::ReloginWait,
            });
        }
        // The limit engine's own next moment.
        //
        // A DEADLINE IN THE PAST IS NOT A DEADLINE. `consider` used to clamp
        // every computed duration into `1..=AWAIT_CLAMP_MS`, so a moment that
        // had already passed armed `timeout=1` — and `warned_at` is cleared
        // only by `end_episode`, which a HOOKLESS session (the central law's
        // case) never reaches, so from `warned_at + warn_ms` onward the loop
        // ran ~1000 passes a second for ever, each pass a `status`, a `who`, a
        // `custody` and a fresh socket. That is the NEVER-POLL law defeated by
        // its own clamp. A past moment is now dropped by the caller (each
        // `consider` site states what still owes work), and the floor on a
        // FUTURE one is [`ARM_FLOOR_MS`] rather than 1 ms, so a moment that
        // arrives a millisecond late still parks.
        let mut best: Option<(u64, Timer)> = None;
        let mut consider = |until_ms: u64, timer: Timer| {
            let ms = until_ms
                .saturating_sub(now_ms)
                .clamp(ARM_FLOOR_MS, AWAIT_CLAMP_MS);
            match best {
                Some((b, _)) if b <= ms => {}
                _ => best = Some((ms, timer)),
            }
        };
        if let Some(until_s) = self.schedule_at(now_s) {
            let delta = until_s.saturating_sub(now_s).max(0) as u64;
            consider(
                now_ms.saturating_add(delta.saturating_mul(1000)),
                Timer::Schedule,
            );
        }
        // NO `Timer::SwitchBack` ARM. Design §5.8.5's switch-back is recorded
        // (`switch_back_at`) and never acted on: `plan`/`plan_limits` never
        // consult [`Self::switch_back_due`], and [`Self::switch_back_acts`]
        // and [`Self::adopt_switch`] have no production caller. Arming a
        // deadline for an act that cannot happen was the second copy of the
        // 1 ms spin above — `switch_back_at` is cleared only by
        // `adopt_switch`, so once `resets_at` passed the arm never moved
        // again. The acting half is TARGET and says so on those three
        // methods; nothing here arms for it.
        //
        // The warn rung's SECOND HALF is a real deadline: `plan_ladder` owes
        // `meta set attention` at `warned_at + warn_s` and can then clear the
        // debt by setting `badged`. Once the badge is taken — or once the
        // moment has passed and the ladder has been asked — there is nothing
        // left to wait for, so the arm is not offered.
        if let Some(w) = self.live.warned_at
            && !self.live.badged
            && w.saturating_add(self.cfg.warn_ms) > now_ms
        {
            consider(w.saturating_add(self.cfg.warn_ms), Timer::Warn);
        }
        if let Some((ms, timer)) = best {
            // Even a pure deadline is armed as `await seq <last_seq>`: a
            // screen that moves before it fires ends the wait EARLY with more
            // information, which is strictly better than a bare timer and is
            // the reason nothing here counts down.
            return Some(Arm {
                cond: vec!["seq".to_string(), seq.unwrap_or(0).to_string()],
                timeout_ms: ms,
                timer,
            });
        }
        // Nothing scheduled: the stall wait IS the ingress. A tool in flight
        // gets the tool window, anything else the api window — and `OK
        // timeout` on it is the verdict, `OK seq <n>` its refutation.
        let (timeout_ms, timer) = if self.live.tool_use_id.is_some() {
            (self.cfg.tool_stall_ms, Timer::ToolStall)
        } else {
            (self.cfg.api_stall_ms, Timer::ApiStall)
        };
        Some(Arm {
            cond: vec!["seq".to_string(), seq.unwrap_or(0).to_string()],
            timeout_ms: timeout_ms.min(AWAIT_CLAMP_MS),
            timer,
        })
    }

    /// The limit engine's next scheduled instant, in unix seconds.
    fn schedule_at(&self, now_s: i64) -> Option<i64> {
        let st = self.limits.as_ref()?;
        let guards = self.limit_guards(&HostGuards::default());
        match limits::step(&self.limits_cfg, st, &guards, now_s, self.generation).decision {
            limits::Decision::Wait { until } if until > now_s => Some(until),
            _ => None,
        }
    }

    fn limit_guards(&self, host: &HostGuards) -> LimitGuards {
        let mut g = LimitGuards::from_config(&self.limits_cfg, self.cfg.accounts_enabled);
        g.hold = host.hold;
        g.busy = host.busy;
        // The host acts on the session it OBSERVES: `self` is a host-side
        // fact, not a caller check (§4.3 (c)). A caller-sid test is not
        // computable for an anonymous Owner connection, so it is not made.
        g.caller_is_target = false;
        g.self_allowed = true;
        g
    }

    /// Decide the next act. PURE over the state: nothing is recorded until
    /// [`Self::commit`], and nothing is sent until the journal row is
    /// written.
    ///
    /// The limit table outranks the ladder, because §5.4's own table hands
    /// `quota-wait` and `network-offline` to §5.8 and pauses the ladder for
    /// the class's lifetime.
    #[must_use]
    pub fn plan(&self, host: &HostGuards, now_ms: u64, now_s: i64) -> Plan {
        if !self.cfg.enabled {
            return Plan::Refused {
                why: Refuse::Disabled,
                what: "watch".to_string(),
            };
        }
        if !host.engaged {
            return Plan::Refused {
                why: Refuse::Bypassed,
                what: "watch".to_string(),
            };
        }
        if host.generation != self.generation {
            return Plan::Refused {
                why: Refuse::Generation,
                what: "watch".to_string(),
            };
        }
        if self.open.is_some() {
            // Never two automatic actions in flight: a second class arriving
            // while one action awaits its verdict is QUEUED, not acted on.
            return Plan::Refused {
                why: Refuse::InFlight,
                what: "watch".to_string(),
            };
        }
        let plan = if self.limits.is_some() {
            self.plan_limits(host, now_ms, now_s)
        } else {
            self.plan_ladder(host, now_ms, now_s)
        };
        self.fence_turn_nonce(plan)
    }

    /// The LAST fence before a plan leaves [`Self::plan`]: a typed act needs a
    /// real launch nonce, and a host that has not read one gets a refusal
    /// rather than a line the server rejects at parse.
    ///
    /// It is PUBLIC because a caller that assembles a [`Planned`] itself —
    /// `harness recover` does, for one named step of the table — bypasses
    /// [`Self::plan`] and owes the same fence.
    ///
    /// This sits in `plan` rather than in either planner because both reach
    /// [`fenced`] and the shape of the failure is the same on both paths:
    /// `pty_idem::parse_key` answers `ERR usage: …` as the whole reply and
    /// `attempt()` never runs, so nothing is typed, nothing is submitted, and
    /// — before [`refusal_of`] learned the word — the ledger said `timeout`.
    #[must_use]
    pub fn fence_turn_nonce(&self, plan: Plan) -> Plan {
        let Plan::Act(p) = &plan else { return plan };
        if !p.acts.iter().any(|a| matches!(a, Act::Type { .. })) {
            return plan;
        }
        if valid_turn_nonce(&self.cfg.nonce) {
            return plan;
        }
        Plan::Refused {
            why: Refuse::Unresolved,
            what: p.action.clone(),
        }
    }

    fn plan_limits(&self, host: &HostGuards, now_ms: u64, now_s: i64) -> Plan {
        let Some(st) = &self.limits else {
            return Plan::Idle;
        };
        let guards = self.limit_guards(host);
        let decided = limits::step(&self.limits_cfg, st, &guards, now_s, self.generation);
        match decided.decision {
            limits::Decision::Refused { why } => Plan::Refused {
                why: why.into(),
                what: format!("limits.{}", st.class),
            },
            limits::Decision::Wait { until } => Plan::Wait {
                until_ms: now_ms.saturating_add(
                    (until.saturating_sub(now_s).max(0) as u64).saturating_mul(1000),
                ),
                timer: Timer::Schedule,
            },
            limits::Decision::Act {
                action,
                level,
                reason,
            } => {
                if level >= 3 && host.custody_user {
                    // A person is reading or selecting RIGHT NOW. Defer —
                    // this is the state no keystroke signal sees.
                    return Plan::Refused {
                        why: Refuse::Custody,
                        what: action.as_str().to_string(),
                    };
                }
                if level >= 3 && self.prompt {
                    // The approval half of §4.3's fence, owed here because
                    // the limit path never reaches `classify_liveness`'s
                    // `prompt` short-circuit: `classify_liveness` answers
                    // `quota-wait` on the class BEFORE it looks at the box,
                    // so once a class is in force nothing downstream sees
                    // it. An approval box is the vendor's own channel and
                    // the keyboard is the person's; a typed act here lands
                    // an Enter on the highlighted option.
                    //
                    // `self.prompt` is what the SPINE last read
                    // (`parse_prompt` over the grid). A grid never read reads
                    // `false`, which is why the guard on the Enter is
                    // composer-shaped as well — two fences, neither alone.
                    return Plan::Refused {
                        why: Refuse::WaitingHuman,
                        what: action.as_str().to_string(),
                    };
                }
                if action == Action::SwitchAccount {
                    // The relaunch has NO admitted control line (see
                    // [`Act::line`]). Refused here, by its own name, so the
                    // ledger cannot read it as "the roster named no
                    // candidate" — and refused BEFORE `commit`, so a dead
                    // capability stops consuming the shared switch budget and
                    // stops moving `last_switch_at`. It SUBSUMES the older
                    // `refused:unresolved` arm (an account label with no
                    // config directory): a rotation with a resolved directory
                    // is refused here too, because the verb does not exist.
                    return Plan::Refused {
                        why: Refuse::Unsupported,
                        what: action.as_str().to_string(),
                    };
                }
                let acts = self.acts_for(action, level, st, now_s);
                Plan::Act(Box::new(Planned {
                    acts,
                    cap: "limits",
                    action: action.as_str().to_string(),
                    level,
                    reason,
                    lease: level >= 3,
                    step: Some(decided.step),
                    limit_action: Some(action),
                    rung: None,
                    wait_until: decided.wait_until,
                }))
            }
        }
    }

    /// The acts one recovery-table action becomes. A `level` below the
    /// action's own is a DEGRADE: the L1 half only, and the ledger says
    /// `degraded:budget`.
    fn acts_for(&self, action: Action, level: u8, st: &limits::State, now_s: i64) -> Vec<Act> {
        let class = st.class;
        let reset = st
            .resets_at
            .map(|r| format!(" · resets in {}s", r.saturating_sub(now_s).max(0)))
            .unwrap_or_default();
        let l1 = |what: &str| -> Vec<Act> {
            vec![
                Act::Description(format!("{}: {what}{reset}", class.as_str())),
                Act::Notice(format!("harness · {} · {what}", class.as_str())),
            ]
        };
        if level < action.level() {
            let mut acts = l1("budget dry — displaying only");
            acts.push(Act::Attention(format!(
                "{} on {} — budget dry, displaying only",
                class.as_str(),
                self.sid
            )));
            return acts;
        }
        match action {
            Action::LetVendorRetry => l1("vendor retrying"),
            Action::Wait => l1("waiting for the window to reset"),
            Action::Escalate => {
                let mut acts = l1("escalated — a human owns it now");
                acts.push(Act::Attention(format!(
                    "{} on {} — escalated, a human owns it now",
                    class.as_str(),
                    self.sid
                )));
                acts.push(Act::Ask {
                    to: self.cfg.principal.clone(),
                    text: format!(
                        "aterm harness: {} on {}{reset} — the recovery table is exhausted or the \
                         next step needs you",
                        class.as_str(),
                        self.sid
                    ),
                });
                acts
            }
            Action::Retry => {
                let text = match self.limits_cfg.retry_text {
                    // The fixed word, by default. `last-prompt` replays a
                    // LEDGER row under §5.8.6's origin/size/one-line rules —
                    // never screen text — and the host owns that replay, so
                    // this loop degrades to `continue` rather than inventing
                    // one.
                    RetryText::Continue | RetryText::LastPrompt => "continue",
                };
                self.fence(text)
            }
            Action::SwitchModel => self.fence(&format!("/model {}", self.cfg.model_alt)),
            Action::LowerPriority => self.fence("/low-priority"),
            Action::LimitReset => {
                // L3 opens the vendor's confirmation and then STOPS: the
                // "Yes, use my reset" keypress is the human's.
                let mut acts = self.fence("/limit-reset");
                acts.push(Act::Attention(format!(
                    "{} on {} — /limit-reset is open, the confirming keypress is yours",
                    class.as_str(),
                    self.sid
                )));
                acts
            }
            Action::Relogin => {
                // The harness types six characters. The VENDOR then opens
                // Anthropic's own sign-in URL in the machine's browser and a
                // human completes the flow there. No token is read, held,
                // forwarded or renewed here, and the authorization code the
                // browserless path asks for is never typed or relayed.
                let mut acts = vec![Act::Description(format!(
                    "auth: login expired · {}",
                    self.sid
                ))];
                acts.push(Act::Attention(format!(
                    "auth: login expired on {} — finish the sign-in in the browser",
                    self.sid
                )));
                acts.extend(self.fence("/login"));
                acts
            }
            Action::SwitchAccount => vec![
                // The LABEL is named in the annotation, because the relaunch
                // line carries only the directory it resolved to: a journal
                // row that says `rotating account` and nothing else cannot
                // be read back against `accounts.toml`.
                Act::Description(format!(
                    "{}: rotating account -> {}",
                    class.as_str(),
                    self.cfg.account_label
                )),
                Act::Relaunch {
                    label: self.cfg.account_label.clone(),
                    // `plan_limits` refuses an unresolved label before this
                    // is built, so the `unwrap_or_default` below is the
                    // empty-string case that `Act::line` then declines to
                    // send — two fences, neither alone.
                    config_dir: self.cfg.account_dir.clone().unwrap_or_default(),
                    resume: None,
                    model: None,
                },
            ],
            // Unreachable in ABI 1; the engine skips it before this point.
            Action::ExtraUsage => l1("extra usage is unreachable"),
        }
    }

    fn fence(&self, text: &str) -> Vec<Act> {
        fenced(
            TurnId {
                nonce: self.cfg.nonce.clone(),
                producer: self.cfg.producer,
                seq: self.seq.saturating_add(1),
            },
            text,
        )
    }

    fn plan_ladder(&self, host: &HostGuards, now_ms: u64, _now_s: i64) -> Plan {
        let verdict = self.liveness(now_ms);
        // Rung 1's SECOND HALF — `meta set attention` after `warn_s` — is
        // owed by the warn already taken and needs no fresh verdict. It is
        // decided before the stall test on purpose: the revision gate makes
        // a still screen read `working` (correctly: there is no new
        // evidence), and without this the badge a standing warn owes would
        // never land.
        if let (Some(Rung::Warn), Some(at), false) =
            (self.live.rung, self.live.warned_at, self.live.badged)
        {
            if now_ms >= at.saturating_add(self.cfg.warn_ms) {
                return self.rung_plan(Rung::Warn, &verdict, host, true);
            }
            return Plan::Wait {
                until_ms: at.saturating_add(self.cfg.warn_ms),
                timer: Timer::Warn,
            };
        }
        if !verdict.class.is_stall() {
            // A verdict that is an EXPLANATION is not a symptom: the ladder
            // does not run and nothing is prodded. `stop-hook-waiting`,
            // `waiting-human`, `quota-wait`, `network-offline` and
            // `unobserved` all land here, which is the whole point of naming
            // them separately from a stall.
            return Plan::Idle;
        }
        if verdict.class == Liveness::StoppedShort && !stopped_short_available(self.hooks) {
            return Plan::Refused {
                why: Refuse::Disabled,
                what: "stopped-short: unavailable (hooks=absent)".to_string(),
            };
        }
        let next = self.next_rung(&verdict, now_ms);
        let Some(rung) = next else {
            return Plan::Idle;
        };
        if rung.needs() > self.cfg.level {
            // The rung is above `liveness.level`. The ladder KEEPS GOING at
            // the level it has — it does not stop — so the escalate rung is
            // offered instead and the skip is journaled.
            if rung != Rung::Escalate {
                return self.rung_plan(Rung::Escalate, &verdict, host, false);
            }
            return Plan::Refused {
                why: Refuse::Level,
                what: rung.as_str().to_string(),
            };
        }
        self.rung_plan(rung, &verdict, host, false)
    }

    /// Which rung is due. Rung 1 first and always; rung 2 only at a `Stop`
    /// event and under its own cap; rung 3 only when the session has not been
    /// running for `nudge_after_s`; rung 4 only for `tool-stalled` after two
    /// stall windows and only with budget; then escalate.
    fn next_rung(&self, verdict: &LivenessVerdict, now_ms: u64) -> Option<Rung> {
        let taken = self.live.rung;
        if taken.is_none() {
            return Some(Rung::Warn);
        }
        let warned = self.live.warned_at.unwrap_or(now_ms);
        match taken {
            Some(Rung::Warn) => {
                if now_ms < warned.saturating_add(self.cfg.warn_ms) {
                    return None;
                }
                if self.live.stop_seen_at.is_some()
                    && self.live.stop_blocks < self.cfg.stop_max_blocks
                {
                    return Some(Rung::StopBlock);
                }
                if now_ms >= warned.saturating_add(self.cfg.nudge_after_ms)
                    && self.last_status.phase != SessionPhase::Running
                {
                    return Some(Rung::Nudge);
                }
                None
            }
            Some(Rung::StopBlock) => {
                if now_ms >= warned.saturating_add(self.cfg.nudge_after_ms)
                    && self.last_status.phase != SessionPhase::Running
                {
                    return Some(Rung::Nudge);
                }
                None
            }
            Some(Rung::Nudge) => {
                if verdict.class == Liveness::ToolStalled
                    && now_ms >= warned.saturating_add(self.cfg.tool_stall_ms.saturating_mul(2))
                {
                    return Some(Rung::Escape);
                }
                Some(Rung::Escalate)
            }
            Some(Rung::Escape) => Some(Rung::Escalate),
            Some(Rung::Escalate) | None => None,
        }
    }

    fn rung_plan(
        &self,
        rung: Rung,
        verdict: &LivenessVerdict,
        host: &HostGuards,
        badge: bool,
    ) -> Plan {
        if rung.level() >= 3 && host.custody_user {
            return Plan::Refused {
                why: Refuse::Custody,
                what: rung.as_str().to_string(),
            };
        }
        if rung.level() >= 3 && self.prompt {
            // THE APPROVAL FENCE, owed by every planner and not just two.
            // It used to live in `plan_limits` and `switch` only, and the
            // automatic ladder inherited it by accident — through
            // `classify_liveness`'s `waiting-human` short-circuit feeding
            // `plan_ladder`'s `is_stall()` test. `Watcher::nudge` computes a
            // verdict and then hands the rung straight here, so a hand-asked
            // `--level turn` reached an L3 `turn` with an approval box on the
            // grid. `claude_composer_ready_pattern`'s own doc states the rule
            // this closes: the guard is never a licence, and a caller that
            // types must ALSO refuse while a box is up.
            return Plan::Refused {
                why: Refuse::WaitingHuman,
                what: rung.as_str().to_string(),
            };
        }
        if rung == Rung::Escape && self.live.escapes.left(self.cfg.escape_budget, 0) == 0 {
            return Plan::Refused {
                why: Refuse::Budget,
                what: rung.as_str().to_string(),
            };
        }
        let why = verdict.reasons.join(",");
        let tool = self
            .live
            .tool_use_id
            .as_deref()
            .map_or(String::new(), |t| format!(" tool_use_id={t}"));
        let acts = match rung {
            Rung::Warn => {
                let mut acts = vec![
                    Act::Icon("⏸".to_string()),
                    Act::Description(format!("{}{tool} · {why}", verdict.class.as_str())),
                    Act::Notice(format!("harness · liveness · {}", verdict.class.as_str())),
                ];
                if badge {
                    acts.push(Act::Attention(format!(
                        "{} on {}{tool} · {why}",
                        verdict.class.as_str(),
                        self.sid
                    )));
                }
                acts
            }
            Rung::StopBlock => vec![Act::StopBlock {
                reason: format!(
                    "{}task items appear unfinished ({}) — continue or state why you stopped",
                    mark::attribution(mark::Voice::Reply, "liveness", 0),
                    verdict.class.as_str()
                ),
            }],
            Rung::Nudge => self.fence(
                "status check: are you blocked? if the last tool is hung, interrupt and retry \
                 with a timeout",
            ),
            Rung::Escape => vec![Act::Escape {
                guard: ESCAPE_GUARD.to_string(),
            }],
            Rung::Escalate => vec![
                Act::Attention(format!(
                    "{} on {} — the ladder is done, a human is needed ({why})",
                    verdict.class.as_str(),
                    self.sid
                )),
                Act::Ask {
                    to: self.cfg.principal.clone(),
                    text: format!(
                        "aterm harness: {} on {} — the ladder is done and a human is needed ({why})",
                        verdict.class.as_str(),
                        self.sid
                    ),
                },
            ],
        };
        Plan::Act(Box::new(Planned {
            acts,
            cap: "liveness",
            action: rung.as_str().to_string(),
            level: rung.level(),
            reason: format!("liveness.{}[{}]", verdict.class.as_str(), rung.as_str()),
            lease: rung.level() >= 3,
            step: None,
            limit_action: None,
            rung: Some(rung),
            wait_until: None,
        }))
    }

    /// Record what [`Self::plan`] decided, with the ledger id of the journal
    /// row that was written BEFORE it. An L3/L4 act becomes the one in
    /// flight; an L0/L1 one completes at once.
    pub fn commit(&mut self, planned: &Planned, id: u64, now_ms: u64, now_s: i64) {
        if planned.acts.iter().any(|a| matches!(a, Act::Type { .. })) {
            self.seq = self.seq.saturating_add(1);
        }
        if let (Some(action), Some(st)) = (planned.limit_action, self.limits.as_mut()) {
            let decided = limits::Step {
                decision: limits::Decision::Act {
                    action,
                    level: planned.level,
                    reason: planned.reason.clone(),
                },
                skipped: Vec::new(),
                step: planned.step.unwrap_or(0),
                wait_until: planned.wait_until,
            };
            st.commit(&decided, id, now_s);
        }
        if let Some(rung) = planned.rung {
            self.live.rung = Some(rung.max(self.live.rung.unwrap_or(rung)));
            if rung == Rung::Warn {
                if self.live.warned_at.is_none() {
                    self.live.warned_at = Some(now_ms);
                } else {
                    self.live.badged = true;
                }
            }
            if rung == Rung::StopBlock {
                self.live.stop_blocks = self.live.stop_blocks.saturating_add(1);
            }
            if rung == Rung::Escape {
                self.live.escapes.spend(now_s);
            }
            self.live.last_verdict = Some((self.liveness(now_ms).class, now_ms));
            self.live.revision_at_verdict = self.last_status.revision;
        }
        if planned.level >= 3 {
            self.open = Some(OpenAct {
                id,
                cap: planned.cap,
                action: planned.action.clone(),
                generation: self.generation,
                began_ms: now_ms,
                limit_action: planned.limit_action,
                rung: planned.rung,
            });
        }
    }

    /// The verdict for the act in flight. An act decided against an OLDER
    /// generation is dropped rather than applied late.
    pub fn verdict(&mut self, verdict: limits::Verdict, now_ms: u64, now_s: i64) {
        let Some(open) = self.open.take() else {
            return;
        };
        if open.generation != self.generation {
            return;
        }
        let executed = matches!(
            verdict,
            limits::Verdict::Executed | limits::Verdict::Settled
        );
        if let Some(st) = &mut self.limits {
            st.verdict(verdict, now_s);
            self.carry = st.carry();
        }
        if executed && open.limit_action == Some(Action::SwitchModel) {
            self.switched_model = true;
            if self.limits_cfg.switch_back {
                // Trigger (a) of §5.8.5: the harness's own deadline at
                // `resets_at` + jitter on the window that caused the switch.
                self.switch_back_at = self
                    .limits
                    .as_ref()
                    .and_then(|s| s.resets_at)
                    .map(|r| r.saturating_add(limits::RESET_JITTER_S));
            }
        }
        let _ = now_ms;
    }

    /// **TARGET — no production caller.** Adopt a switch the harness did NOT
    /// make (trigger (c) of §5.8.5): a
    /// `PostModelSwitch` back to the primary that is not the host's own
    /// pending id. The harness adopts the state, consumes no budget, and
    /// starts the dwell.
    pub fn adopt_switch(&mut self, to_model: &str, now_s: i64) {
        if to_model == self.cfg.model_primary {
            self.switched_model = false;
            self.switch_back_at = None;
            if let Some(st) = &mut self.limits {
                st.last_switch_at = Some(now_s);
                self.carry = st.carry();
            }
        }
    }

    /// **TARGET — no production caller.** Is a switch-back due?
    ///
    /// Corrected 2026-09-22 against the code: this docstring used to say "the
    /// three triggers of §5.8.5 all land here", and one does. Trigger (a),
    /// the harness's own deadline at `resets_at + jitter`, is the only one
    /// recorded ([`Self::verdict`]); trigger (b)
    /// (`Notification quota_auto_resume_fired`) reaches the limit engine and
    /// never this function; trigger (c) (`PostModelSwitch` back to the
    /// primary) would arrive through [`Self::adopt_switch`], which nothing
    /// calls. §5.8.5's `switch_back_headroom_pct` hysteresis is not tested
    /// here either.
    ///
    /// Nothing PLANS on this answer — `plan`/`plan_limits` never consult it —
    /// so a `switch-model` the harness makes is not switched back. That is
    /// the state of the capability, said here rather than implied by an armed
    /// deadline: [`Self::next_arm`] arms none for it.
    #[must_use]
    pub fn switch_back_due(&self, now_s: i64) -> bool {
        self.switched_model && self.switch_back_at.is_some_and(|at| now_s >= at)
    }

    /// **TARGET — no production caller.** The acts a due switch-back would
    /// take; see [`Self::switch_back_due`] for why none does.
    #[must_use]
    pub fn switch_back_acts(&self) -> Vec<Act> {
        self.fence(&format!("/model {}", self.cfg.model_primary))
    }

    /// The resync a `GAP` owed has been done.
    pub fn resynced(&mut self) {
        self.resync_due = false;
        self.live.gap_at = None;
    }

    /// The journal row written BEFORE an act (design §4.3's shape).
    #[must_use]
    pub fn journal_row(&self, p: &Planned, now_s: i64, evidence: &[u64]) -> String {
        let budget_left = self
            .limits
            .as_ref()
            .map_or(self.limits_cfg.budget.count, |s| {
                s.budget.left(self.limits_cfg.budget, now_s)
            });
        let ids = evidence
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "{{\"ts\":{now_s},\"sid\":{},\"gen\":{},\"cap\":\"{}\",\"class\":\"{}\",\
             \"level\":{},\"action\":{},\"reason\":{},\"evidence_ids\":[{ids}],\
             \"budget_left\":\"{budget_left}/{}\"}}",
            json_str(&self.sid),
            self.generation,
            p.cap,
            self.class().map_or("none", Class::as_str),
            p.level,
            json_str(&p.action),
            json_str(&p.reason),
            self.limits_cfg.budget.as_string(),
        )
    }
}

/// The auth banner the re-login wait watches leave (design §5.8.10 step 4).
/// One wire token: `.` stands for a space.
pub const AUTH_BANNER: &str = "(?i)please.run./login";

/// The verdict row written AFTER an act (design §4.3, §5.8.6).
///
/// Every string body goes through [`json_str`], the workspace's own escaper,
/// which EMITS `\n`, `\t`, `\r` and `\u00xx` rather than folding them to a
/// space. Line framing is preserved by escaping, not by erasing: this row is
/// the audit record, and a local escaper that folded control characters lost
/// them from it forever.
#[must_use]
pub fn verdict_row(id: u64, journal: u64, now_s: i64, verdict: &str, detail: &str) -> String {
    format!(
        "{{\"id\":{id},\"ref\":{journal},\"ts\":{now_s},\"verdict\":{},\"detail\":{}}}",
        json_str(verdict),
        json_str(detail)
    )
}

// ---------------------------------------------------------------------------
// The transport seam and the pump
// ---------------------------------------------------------------------------

/// What one `pump` did, so a caller (and a test) can see the loop's whole
/// pass without reaching into it.
#[derive(Debug, Clone, PartialEq)]
pub struct Pass {
    /// What woke the loop.
    pub wake: Wake,
    /// What the spine read implied.
    pub events: Vec<observe::Event>,
    /// What was decided.
    pub plan: Plan,
    /// The journal row's ledger id, where one was written.
    pub journal: Option<u64>,
    /// The control lines actually sent, in order.
    pub sent: Vec<String>,
    /// The verdict spelling written after.
    pub verdict: Option<String>,
}

/// What one [`Watcher::execute`] did: the journal row it opened, the control
/// lines that actually went out, and the verdict spelling written after.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Executed {
    /// The ledger id of the journal row written BEFORE the first act.
    pub journal: u64,
    /// The control lines actually sent, in order.
    pub sent: Vec<String>,
    /// The verdict spelling (`executed`, `refused:busy`, `timeout`, …).
    pub verdict: String,
}

/// The reads, waits and writes the loop makes, behind a trait so tests drive
/// it from fixtures and the host drives it from a control socket. NO SOCKET IS
/// OPENED IN THIS MODULE, by design.
///
/// `park` is the whole ingress: it holds the subscribe handle AND arms the one
/// bounded `await` the loop asked for, and returns whichever came first. An
/// implementation that sleeps instead of parking would be a defect in the
/// host, not in this loop, and [`Arm::line`] is the exact wait it owes.
pub trait Wire: observe::Introspect {
    /// Park until a frame arrives or the armed wait answers. `arm` is `None`
    /// when the loop has no deadline and only wants the stream.
    fn park(&mut self, arm: Option<&Arm>) -> Wake;

    /// Send one control line; `Ok` with the reply's first line (`OK …`, e.g.
    /// `OK skipped seq=…`, or a bare `meta`'s `OK … attention=… …` readout)
    /// or `Err` with the server's `ERR …` text.
    fn send(&mut self, line: &str) -> Result<String, String>;

    /// Append one row to a ledger; the row's id.
    fn journal(&mut self, ring: &str, row: &str) -> u64;

    /// The server-side facts every act is gated on, read FRESH before the act
    /// (`who`, `custody`, `status hold=`) — never cached across a park.
    fn guards(&mut self) -> HostGuards;

    /// The host's millisecond clock.
    fn now_ms(&mut self) -> u64;

    /// The host's unix clock.
    fn now_unix(&mut self) -> i64;
}

impl Watcher {
    /// ONE iteration: park, fold, read the spine (gated on `revision`),
    /// decide, and — under the guards — journal, act and write the verdict.
    ///
    /// The order is §4.3's order and is not negotiable: the journal row is
    /// written BEFORE the first line is sent, so the mark can never render
    /// `acting` without a row behind it, and the verdict row is written after
    /// every act including a refused one.
    pub fn pump(&mut self, w: &mut dyn Wire) -> Pass {
        let arm = {
            let now_ms = w.now_ms();
            let now_s = w.now_unix();
            self.next_arm(now_ms, now_s)
        };
        let wake = w.park(arm.as_ref());
        let now_ms = w.now_ms();
        let now_s = w.now_unix();
        self.wake(&wake, now_ms, now_s);
        let events = self.read_spine(w, now_ms, now_s);
        let host = w.guards();
        let plan = self.plan(&host, now_ms, now_s);
        let mut pass = Pass {
            wake,
            events,
            plan: plan.clone(),
            journal: None,
            sent: Vec::new(),
            verdict: None,
        };
        let Plan::Act(planned) = plan else {
            if let Plan::Refused { why, what } = &pass.plan {
                let id = w.journal(
                    RING_ACTUATION,
                    &verdict_row(0, 0, now_s, why.as_str(), what),
                );
                pass.journal = Some(id);
                pass.verdict = Some(why.as_str().to_string());
            }
            self.journal_ladder_pause(w, now_s);
            return pass;
        };
        let done = self.execute(w, &planned, now_ms, now_s);
        pass.journal = Some(done.journal);
        pass.sent = done.sent;
        pass.verdict = Some(done.verdict);
        self.journal_ladder_pause(w, now_s);
        pass
    }

    /// A LIMIT CLASS TURNS THE LIVENESS LADDER OFF, and that has to be
    /// visible. [`classify_liveness`] short-circuits on any class, so from
    /// the moment one is in force no rung can be taken however long the
    /// session sits — a capability that silently stops working, with nothing
    /// on the record saying so.
    ///
    /// Both EDGES are journalled and only the edges, so the ledger says when
    /// the harness stopped watching for stalls and when it resumed. Written
    /// at the END of a pass, never before one: §4.3's order is that the act's
    /// own journal row is the FIRST row of its pass, and a bookkeeping row in
    /// front of it would make the ledger read as if something else had been
    /// decided first.
    fn journal_ladder_pause(&mut self, w: &mut dyn Wire, now_s: i64) {
        let paused = self.class();
        if paused == self.ladder_paused {
            return;
        }
        let (word, what) = match paused {
            Some(c) => ("ladder-paused", c.as_str().to_string()),
            None => ("ladder-resumed", "-".to_string()),
        };
        let _ = w.journal(RING_ACTUATION, &verdict_row(0, 0, now_s, word, &what));
        self.ladder_paused = paused;
    }

    /// ACT on one plan through the §4.3 order, and nothing else: journal the
    /// row BEFORE the first line goes out, take the turn lease where the plan
    /// asks for one, send the acts in order, unwind a half-landed fence, and
    /// write the verdict row after — including after a refusal.
    ///
    /// It is split out of [`Self::pump`] and PUBLIC because a HAND-ASKED act
    /// (`aterm harness switch`, design §5.7) must reach the actuator through
    /// the same code, not through a second copy of it. The plan it is given
    /// still comes from [`Self::plan`] or from [`Self::fence_turn_nonce`], so
    /// the decision fences are upstream of both callers and a manual act can
    /// reach nothing the loop could not.
    pub fn execute(
        &mut self,
        w: &mut dyn Wire,
        planned: &Planned,
        now_ms: u64,
        now_s: i64,
    ) -> Executed {
        // Journal BEFORE.
        let id = w.journal(RING_RECOVERY, &self.journal_row(planned, now_s, &[]));
        let mut sent: Vec<String> = Vec::new();
        let mut verdict = limits::Verdict::Executed;
        let mut refusal: Option<Refuse> = None;
        let mut typed_unsubmitted = false;
        let mut leased = false;
        if planned.lease {
            // The acquire's reply is READ. It used to be discarded, so a
            // refused lease still ran the whole act list and the server's own
            // `turn` refusal was the only thing that caught it — which put
            // `refused:busy` on the turn rather than on the lease it never
            // got.
            match w.send(&format!(
                "lease acquire ttl={} holder={LEASE_HOLDER}",
                self.cfg.settle_ms
            )) {
                Ok(_) => leased = true,
                Err(e) => {
                    let why = refusal_of(&e).unwrap_or(Refuse::Busy);
                    let vid = w.journal(
                        RING_RECOVERY,
                        &verdict_row(0, id, now_s, why.as_str(), &planned.action),
                    );
                    let _ = vid;
                    return Executed {
                        journal: id,
                        sent,
                        verdict: why.as_str().to_string(),
                    };
                }
            }
        }
        for act in &planned.acts {
            if matches!(act, Act::Attention(_) | Act::ClearAttention) {
                // The attention is SHARED (§4.3 L1): read what stands before
                // touching it, and journal a badge left alone.
                sent.push("meta".to_string());
                if let Err(word) = attention_gate(w, act) {
                    let _ = w.journal(
                        RING_RECOVERY,
                        &verdict_row(0, id, now_s, word, &planned.action),
                    );
                    continue;
                }
            }
            let Some(line) = act.line() else {
                // A Stop-block is answered in-band by the bridge, not sent.
                // A relaunch with no resolved config dir is declined there
                // too, and neither is an error.
                continue;
            };
            match w.send(&line) {
                // The guard said NO. `OK skipped` is a success REPLY and a
                // refused ACT: `key if=<re>` matched no row, so the Enter
                // never landed and the composer still holds this loop's text.
                // Recording it as `executed` — and, worse, clearing
                // `typed_unsubmitted` on it — is what made the concatenation
                // §4.3's atomicity comment names (`/model opuscontinue`)
                // reachable with no compensating clear at all.
                Ok(r) if reply_skipped(&r) => {
                    refusal = Some(Refuse::Skipped);
                    verdict = limits::Verdict::Refused;
                    break;
                }
                // The key was a REPLAY: nothing was typed this pass. The
                // fenced Enter must not follow — it would land on whatever
                // composer row is on screen, including a person's own unsent
                // draft, which is the F1 class this fence exists to prevent.
                Ok(r) if matches!(act, Act::Type { .. }) && reply_duplicate(&r) => {
                    refusal = Some(Refuse::Duplicate);
                    verdict = limits::Verdict::Refused;
                    break;
                }
                Ok(_) => {
                    // §4.3's fence is two sends and `turn … submit=none`
                    // does NOT clear the composer, so from here until the
                    // guarded Enter lands there is typed text on the screen
                    // that nothing else will take back.
                    if matches!(act, Act::Type { .. }) {
                        typed_unsubmitted = true;
                    }
                    if matches!(act, Act::Submit { .. }) {
                        typed_unsubmitted = false;
                    }
                    sent.push(line);
                }
                Err(e) => {
                    // The refusals key on SERVER-SIDE facts, and the server
                    // states them in its own words: `ERR halted` for a
                    // standing hold, `ERR busy turn=`/`lease=` for a live
                    // driver, `ERR rate (self-feed floor)` for the inject
                    // floor. All three are refusals of the act; anything else
                    // is an act that went out and did not come back.
                    refusal = refusal_of(&e);
                    verdict = if refusal.is_some() {
                        limits::Verdict::Refused
                    } else {
                        limits::Verdict::Timeout
                    };
                    break;
                }
            }
        }
        if typed_unsubmitted {
            // The fence half-landed. Unwind it: the composer holds this
            // loop's text, and the next table row's Enter would submit both
            // rows concatenated (`/model opuscontinue`, measured shape).
            // The clear is guarded, so a screen with no composer row takes
            // nothing, and the attempt is journalled either way.
            let line = compose_clear_line();
            // The CLEAR's own reply is read the same way the act's was: a
            // clear that answered `OK skipped` cleared nothing, and writing
            // `composer-cleared` on it is the same collapse one row later.
            let cleared = match w.send(&line) {
                Ok(r) => !reply_skipped(&r),
                Err(_) => false,
            };
            if cleared {
                sent.push(line);
            }
            let _ = w.journal(
                RING_RECOVERY,
                &verdict_row(
                    0,
                    id,
                    now_s,
                    if cleared {
                        "composer-cleared"
                    } else {
                        "composer-dirty"
                    },
                    &planned.action,
                ),
            );
        }
        if leased {
            // `lease_release` releases a LIVE cooperative lease only when the
            // holder matches (or `force`), so a bare `lease release` answers
            // `ERR lease held by …` and the lease survives its whole TTL —
            // during which every other cooperative driver is refused
            // `ERR busy`. Name the holder we acquired as.
            let _ = w.send(&format!("lease release holder={LEASE_HOLDER}"));
        }
        self.commit(planned, id, now_ms, now_s);
        // The verdict row AFTER.
        // The CAUSE the server stated, not a guess: a standing halt is
        // `refused:hold`, a live driver `refused:busy`, the inject floor
        // `refused:budget`. `Verdict::Refused` is only reached when
        // `refusal_of` read one of those three words, so the fallback below
        // is unreachable and spelled as the vaguest honest word.
        let spelling: &str = match verdict {
            limits::Verdict::Executed => "executed",
            limits::Verdict::Settled => "settled",
            limits::Verdict::Refused => refusal.map_or("refused", Refuse::as_str),
            limits::Verdict::Timeout => "timeout",
        };
        let vid = w.journal(
            RING_RECOVERY,
            &verdict_row(0, id, now_s, spelling, &planned.action),
        );
        let _ = vid;
        if planned.level >= 3 {
            self.verdict(verdict, now_ms, now_s);
        }
        Executed {
            journal: id,
            sent,
            verdict: spelling.to_string(),
        }
    }
}

#[path = "watch_tests.rs"]
#[cfg(test)]
mod tests;
