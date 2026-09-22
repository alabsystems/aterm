// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm harness` — the command: the hook bridge, the read verbs, and the
//! two verbs that ACT
//! (design `docs/DESIGN-aterm-wrapper-2026-09-17.md` §4.3, §4.4, §4.5, §5.7).
//!
//! # The law this module is built to obey
//!
//! **aterm's own view is the SPINE; vendor hooks are ENRICHMENT that raise
//! confidence and are never load-bearing** (design §0.2, §4.2, §4.7, §5.8.1).
//! Two measurements forced it and both are answered here:
//!
//! * `claude --bare` removes hooks, plugins AND the statusLine in one flag
//!   (MEASURED, design §4.5);
//! * the live Claude Code session that hosted this work reads `detail=-` with
//!   `blocks` → `OK 0`, because an ADOPTED session owns no shell-integration
//!   block (MEASURED 2026-09-21 on this Mac, quoted in
//!   [`super::observe`]'s tests).
//!
//! So every read verb below — [`Cmd::Status`], [`Cmd::Usage`],
//! [`Cmd::Limits`], [`Cmd::Ledger`] — ANSWERS when no hook has ever fired,
//! and says so: each one carries a `source` field whose value is
//! [`Source::Grid`] until an enrichment row exists. A `--bare` launch leaves
//! every read verb working; what it costs is named in the output rather than
//! hidden, and `rm-approve` is the one capability that honestly reads
//! `inactive` without hooks, because answering a permission prompt is
//! hook-only (§0.2's asymmetry — aterm never types `y` at an approval).
//!
//! # Nothing polls
//!
//! There is no timer, no sleep and no loop in this file. Every subcommand is
//! one pass: read what is on stdin or in the ledger, decide, print, exit. The
//! ingress that does wait is the vendor PUSHING a hook into us, and the
//! grid-side ingress is [`super::observe`], driven from `subscribe`/`await`
//! by its caller.
//!
//! # The two exit contracts
//!
//! * [`Cmd::Hook`] and [`Cmd::StatusLine`] **always exit 0** and print
//!   nothing but the decision JSON (decide kinds) or the one HUD line
//!   (statusline). This is not politeness: Claude Code reads a failing hook
//!   command as a block, and the day one was saved it stopped the owner's
//!   worker's prompts, tool calls and stops (CHANGELOG.md:3495-3502). A hook
//!   that RUNS and fails must be a no-op with the vendor's own behaviour as
//!   the fallback.
//! * Every other subcommand exits 0 on success, 1 on a refusal it can
//!   explain, 2 on a usage error.
//!
//! # What is injected
//!
//! [`Env`] carries the clock, the paths and the session identity, so every
//! decision function below is a pure function of its arguments.
//! [`Env::from_process`] is the ONE place that reads the environment.
//!
//! STATUS (docs/README.md honesty ratchet): unit-tested, including the
//! malformed and empty payloads, the always-0 exit, and an install/uninstall
//! round trip that leaves the settings file byte-identical. The control-socket
//! verbs of design §5.7 (`aterm ctl … harness …`) do not exist yet; this is
//! the front-door command, and the bridge it installs calls THIS, not a
//! socket.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use aterm_json::{Map, Value};

use super::accounts::{self, AccountState, AuthState, NotCandidate};
use super::align;
use super::config::{self, HarnessConfig};
use super::disk;
use super::limits::{self, Evidence, WindowKind};
use super::mark::{self, Attach, HooksAbsent, Presence, Voice};
use super::ring::{Entry, Ring, RingConfig};
use super::rm_policy::{self, CwdPrefix, HookEvent, LedgerFields, RmDecision, RmPolicy, RmVerdict};
use super::source::Source;
use super::truncate_bytes;
use super::usage::{self, AccountView, UsageView, WindowView, parse_statusline, rfc3339_utc};
use super::watch::{self, Act, HostGuards, Plan, Rung, Watcher};
use super::wire::{CtlWire, WireConfig};
use crate::supervise::limit::breaks_a_line;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// The harness ABI this command speaks (design §3.1, §4.5).
pub const ABI: u32 = 1;

/// The harness name, which is also the state directory's name (design §1.3).
pub const HARNESS: &str = "claude-harness";

/// The capabilities `install` registers, and the default the written bridge
/// falls back to when nothing exported `ATERM_HARNESS_CAPS`.
///
/// DEVIATION from design §4.5, deliberate and reported: the design's bridge
/// gates on `ATERM_HARNESS_CAPS` alone, which the `__harness` prelude exports.
/// That prelude does not exist yet, so a bridge that gated on the variable
/// alone would be inert for every hand install — every hook silently doing
/// nothing, which is the worst of the three outcomes. The written bridge
/// therefore reads `${ATERM_HARNESS_CAPS:-<this literal>}`, so a
/// prelude-provisioned launch still NARROWS the set and a hand install works.
pub const DEFAULT_CAPS: &str = "introspect,rm-approve,usage-hud,limits";

/// The comment every command this module writes into a settings file ends
/// with, so `uninstall` can find exactly its own entries and nothing else.
/// It is a `#` comment, so the shell the vendor runs the command through
/// ignores it.
pub const OWN_MARK: &str = "# aterm-harness";

/// The most stdin bytes a hook payload may carry. A `PostToolUse` payload
/// embeds `tool_input`, which for a Bash call is a command line but for a
/// Write call is a whole file, so the 4 KiB of design §5.7 is a bound on the
/// ROW, not on the payload; this is the bound on the read, and past it the
/// payload is treated as malformed (which is an abstain, the safe answer).
pub const MAX_STDIN_BYTES: usize = 1 << 20;

/// The ledger of rm verdicts (design §5.1).
pub const RING_RM: &str = "rm";
/// The ledger of every other hook firing — enrichment rows, bounded.
pub const RING_EVENT: &str = "event";
/// The ledger of statusLine samples (design §5.2 source (a)).
pub const RING_STATUSLINE: &str = "statusline";
/// The ledger of disk measurements, removals and denials (design §5.5).
pub const RING_DISK: &str = "disk";
/// The ACTUATOR'S OWN journal (design §4.3, §5.8.6): one row before every
/// act and one verdict row after. [`watch::RING_RECOVERY`] is the same name;
/// it is re-exported here because `ledger` reads it and `ring_config` is the
/// one place a ring's bound is decided.
pub const RING_RECOVERY: &str = watch::RING_RECOVERY;
/// Refusal verdicts for plans that never became acts
/// ([`watch::RING_ACTUATION`]).
pub const RING_ACTUATION: &str = watch::RING_ACTUATION;

/// The rings `ledger` will read, in the order `--help` lists them.
///
/// `recovery` and `actuation` joined this list when the watch loop got a
/// verb: [`watch::Watcher::pump`] has always journalled into them, and until
/// then `ring_config` answered `None` for both — so every journal-before-act
/// row the loop wrote would have been dropped on the floor, and `harness
/// ledger recovery` (design §5.7, §5.8.7) answered `no ledger called`.
pub const RINGS: &[&str] = &[
    RING_RM,
    RING_EVENT,
    RING_STATUSLINE,
    RING_DISK,
    RING_RECOVERY,
    RING_ACTUATION,
];

/// The capability names `enable`/`disable` accept, derived from the ONE
/// declared set so a switch can never name a capability `install` does not
/// register.
#[must_use]
pub fn caps() -> Vec<&'static str> {
    DEFAULT_CAPS.split(',').collect()
}

/// The durable master switch's home. MIRRORS `aterm-gui`'s `config_path`
/// (XDG first, then `$HOME/.config/aterm/aterm.toml`), because the kill switch
/// may not depend on the thing it kills — including on a GUI being up to
/// resolve it (design §4.6.2).
#[must_use]
pub fn default_config_path() -> Option<PathBuf> {
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME").filter(|x| !x.is_empty()) {
        return Some(PathBuf::from(x).join("aterm").join("aterm.toml"));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/aterm/aterm.toml"))
}

/// The value of `[<table>] <key>` in `text`, when it is a bare TOML boolean.
///
/// A DELIBERATELY minimal reader for the two BOOLEAN keys it answers
/// (`harness.enabled`, `disk.apply`). Corrected 2026-09-22: this said "this
/// crate has none [no TOML reader]" and "the ONE key it has to answer", and
/// both were survivors — [`align::read_toml_dotted`] is a fail-closed reader
/// in this same module tree, and a second key landed here long ago. It stays
/// because of what it is for: `harness.enabled` is the KILL SWITCH, and a
/// kill switch may not depend on the grammar it kills, so a reader that
/// cannot refuse the whole file is the right shape for exactly these two.
/// The numeric knobs go through `align`'s reader ([`disk_config`]).
///
/// It understands the two spellings
/// `apply_prefs_edits` can produce — a `[table]` header with `key = false`
/// under it, and the top-level dotted `table.key = false` — ignores `#`
/// comments, and answers `None` for anything it does not recognise.
///
/// The tie breaks toward the SAFE answer, which for a kill switch is OFF: a
/// line this reader mis-attributes can only ever turn the harness off, never
/// on, because `None` and `Some(true)` are both "live" and only an explicit
/// `false` stops anything.
#[must_use]
pub fn toml_bool(text: &str, table: &str, key: &str) -> Option<bool> {
    let dotted = format!("{table}.{key}");
    let mut in_table = false;
    let mut answer = None;
    for raw in text.lines() {
        let line = match raw.find('#') {
            Some(i) => &raw[..i],
            None => raw,
        }
        .trim();
        if let Some(header) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_table = header.trim() == table;
            continue;
        }
        let Some((lhs, rhs)) = line.split_once('=') else {
            continue;
        };
        let lhs = lhs.trim();
        let named = (in_table && lhs == key) || lhs == dotted;
        if !named {
            continue;
        }
        answer = match rhs.trim() {
            "true" => Some(true),
            "false" => Some(false),
            // A non-boolean value is not an answer; the last VALID assignment
            // in the file wins, exactly as TOML's own last-wins would read.
            _ => answer,
        };
    }
    answer
}

/// How many rows `ledger` prints when the caller names no count.
pub const LEDGER_DEFAULT_ROWS: usize = 20;

/// The byte cap on any quoted, attacker-influenced text in an event row.
pub const EVENT_TEXT_CAP: usize = 512;

/// The rule name the `rm` policy attributes its decisions under
/// ([`super::mark::attribution`]).
pub const RULE_RM: &str = "rm policy";

/// The most rows one read of a ledger will hold in memory at once. The rings
/// are bounded already; this bounds the READ so a 64 MiB rm ledger of tiny
/// rows cannot be materialised whole to print twenty of them.
pub const READ_MAX_ROWS: usize = 4096;

/// The rm ledger's bound. 64 MiB per design §4.4; the event and statusline
/// rings are smaller because their rows are enrichment, not the record.
const RM_RING: RingConfig = RingConfig {
    max_bytes: 64 * 1024 * 1024,
    segments: 8,
};
const EVENT_RING: RingConfig = RingConfig {
    max_bytes: 16 * 1024 * 1024,
    segments: 8,
};
const STATUSLINE_RING: RingConfig = RingConfig {
    max_bytes: 4 * 1024 * 1024,
    segments: 4,
};
/// The disk ledger is the smallest of the four and the one that must SURVIVE
/// longest: a removal row is the only record that a tree was deleted and why
/// it was safe, and its rows arrive at most once per measurement.
const DISK_RING: RingConfig = RingConfig {
    max_bytes: 4 * 1024 * 1024,
    segments: 4,
};
/// The recovery journal is THE RECORD of every act the harness made and why
/// it was allowed, so it is sized above the enrichment rings and below the
/// rm ledger.
const RECOVERY_RING: RingConfig = RingConfig {
    max_bytes: 16 * 1024 * 1024,
    segments: 8,
};
/// Refusal verdicts only — the smallest ring, and the one a quiet machine
/// never writes to at all.
const ACTUATION_RING: RingConfig = RingConfig {
    max_bytes: 4 * 1024 * 1024,
    segments: 4,
};

/// The ring config for `name`, or `None` for a name no ring answers to.
#[must_use]
pub fn ring_config(name: &str) -> Option<RingConfig> {
    match name {
        RING_RM => Some(RM_RING),
        RING_EVENT => Some(EVENT_RING),
        RING_STATUSLINE => Some(STATUSLINE_RING),
        RING_DISK => Some(DISK_RING),
        RING_RECOVERY => Some(RECOVERY_RING),
        RING_ACTUATION => Some(ACTUATION_RING),
        _ => None,
    }
}

/// The usage text.
pub const USAGE: &str = "\
aterm harness — the Claude Code harness: the hook bridge, the read verbs, and
the two verbs that ACT (`switch` and `watch`).

USAGE:
    aterm harness hook <EVENT> [<capability>]   answer one vendor hook (stdin: its JSON)
    aterm harness statusline                    record the statusLine JSON, print the HUD line
    aterm harness install [--settings <path>]   write the plugin tree, merge into settings
    aterm harness uninstall [--settings <path>] undo exactly that
    aterm harness status [--json] [--assume-spine]  what the harness is and what it can see
    aterm harness mark [--json|--commands]      the four-state mark (design 4.6)
    aterm harness enable [cap=<c> ...]          turn capabilities on, durably
    aterm harness disable [cap=<c> ...]         turn capabilities off, durably
    aterm harness align [--json] [--write]      probe the installed program, print the verdict
    aterm harness caps [--json]                 the per-capability verdict and what decided it
    aterm harness usage [--json]                the usage view (design 5.2)
    aterm harness accounts [--json]             the account roster and what may be rotated into
    aterm harness accounts discover [--write]   propose one row per config directory (design 5.6)
    aterm harness accounts add <label> <dir>    add ONE config-dir row by hand (design 5.6)
    aterm harness limits [--json]               the failure classifier's verdict (design 5.8)
    aterm harness liveness [--json]             the stall verdict and the rung (design 5.4)
    aterm harness recover <class|action> [--target <t>] [--json] [--commands] [--assume-spine]
    aterm harness nudge [<sid>] [--level <l>] [--commands] [--assume-spine]
    aterm harness disk [<build-dir> ...] [--apply <class>] [--json]
    aterm harness switch <model|account> <t>    ONE guarded switch, by hand (design 5.7)
    aterm harness watch [--passes <n>]          RUN THE LOOP against this instance (design 5.3-5.8)
    aterm harness config get [<key>]            the harness's own config.toml (design 5.8.7)
    aterm harness config set <key> <value>      write one key, per-class sets enforced
    aterm harness ledger <rm|event|statusline|disk|recovery|actuation> [<n>] [--since <id>] [--json]

OPTIONS:
    --settings <path>   the Claude settings file to merge into
                        (default: $HOME/.claude/settings.json)
    --state <path>      the harness state directory
                        (default: $ATERM_HARNESS_STATE, else <aterm state>/harness/claude-harness)
    --config <path>     where the master switch is read from
                        (default: $XDG_CONFIG_HOME/aterm/aterm.toml, else $HOME/.config/...)
    --nonce <hex32>     the TARGET session's public launch nonce, the `nonce=<hex32>` its
                        `local` roster row carries (default: $ATERM_HARNESS_NONCE when it is
                        one). Every typed act is keyed by it and the server parses it before
                        it types anything, so without one recover/nudge answer
                        refused:unresolved rather than printing a line that dies at parse
    --utc-offset <s>    seconds east of UTC for the times the HUD prints (default: 0, i.e. UTC)
    --dry-run           install/uninstall: print what would be written, write nothing
    --purge             uninstall: also remove the ledgers (they are kept by default)
    --assume-spine      the caller ASSERTS a live watch on the grid spine. It is an
                        assumption, not a reading: this command opens no socket, so it
                        cannot check one, and the output says the mark rests on it.
                        recover/nudge: WITHOUT it, an L3 plan (one that TYPES) still
                        prints its decision but its --commands lines are withheld,
                        because hold/busy/custody and the approval-box fence are all
                        at their unmeasured defaults on this path
    --commands          mark/recover/nudge: print the control lines to send
    --level <l>         nudge: warn | stop | turn | escape (default: warn)
    --passes <n>        watch: stop after n decision passes (default: until the stream ends)
    --sock <path>       switch/watch: the control socket (default: the resolved one)
    --target <t>        recover: the model a switch-model types, or the label a
                        switch-account carries (its config dir is not read here)
    --apply <class>     disk: the ONE safelist class to act on. atpkg-gc and
                        claude-purge are surfaced and never removed from here;
                        they name the command that owns them
    --tree <path>       the harness tree: the signed store build, or a
                        dev-linked checkout (default: $ATERM_HARNESS_ROOT,
                        else <state>/tree if that is a directory)
    --against <path>    align/caps: the installed program the probes measure
                        (default: the contract's first wraps_any name, resolved
                        on PATH)
    --aterm-build <t>   align/caps: the aterm identity token the verdict is
                        keyed by (default: this build's own)
    --signed <entry>    align/caps: one signed alignment entry to intersect
                        with, e.g. claude@2026091701=aligned#a1b2c3d4
    --write             align: append the verdict to the alignment sidecar;
                        accounts discover: append the proposed rows to
                        accounts.toml with `[accounts] enabled = false`
    --json              a read verb's JSON form
    -h, --help          this text

`accounts` READS NO CREDENTIAL. It reads the roster the owner wrote and, per
config directory, only the non-secret `oauthAccount` organisation and seat
tier and the `cachedUsageUtilization` snapshot the vendor itself left on disk
— never a token, a keychain item or a secrets file — and it makes no network
call. `discover` writes nothing without `--write`, and what `--write` appends
leaves rotation OFF: `[accounts] enabled = true` is a separate, deliberate
edit, and design 5.8 also needs `cap.limits` level 4 before a rotation runs.

`disk` REPORTS AND REMOVES NOTHING by default. Every row carries the witness
that would make it safe to remove — a build tool's own marker plus two clocks
past `disk.target_stale_days`, or a version directory the live symlink does
not point at, or a package build the live one supersedes. Removal needs BOTH
`disk.apply = true` in aterm.toml AND an explicit `--apply <class>`; a refusal
is written to the `disk` ledger as a denial row rather than dropped. Nothing
under the transcripts root is removable under any flag. Build directories are
the ones you NAME on the line: this command enumerates no workspace of its own.

HOOKS ARE ENRICHMENT. Every read verb answers with no hook installed and says
so in its `source` field; a `--bare` launch (no hooks, no plugins, no
statusLine) leaves them all working. `rm-approve` is the one capability that
needs a hook, because answering a permission prompt is the vendor's channel
and aterm never types `y` at an approval.

`align` RUNS THE TREE'S PROBES against the installed program and computes the
per-capability verdict (design 3.2, 3.6). A probe that crashes, exits non-zero,
prints nothing or runs past its deadline reads `error`, never `ok` and never
`absent`. With no tree, no contract or no program the verdict is `pending`,
nothing renders, and the output says which input was missing. The only time
`align` touches the wrapped program itself is one bounded `--version` read;
nothing here runs it for any other reason, and no failure here can make it
un-launchable. `--signed` supplies the lane's own verdict, and the adopted one
is the INTERSECTION — local can lower, never raise. A `pending` verdict is
never written to the sidecar: a cached one would stop the next pass measuring.

`recover` and `nudge` DECIDE and PRINT; they send nothing. Nothing in those
two verbs opens a control socket, so the verdict they report is `planned` and
the lines they print under `--commands` are exactly what a host with the watch
open would send, in order, after writing its journal row.

`switch` and `watch` ARE the host: they open the control socket and act.
`switch` runs ONE switch through the same guarded path the loop uses — the
level, `[accounts] enabled`, `min_dwell_s`, the shared switch budget, `hold`,
the turn lease, the inject floor, the generation, the approval-box fence and
the launch-nonce fence — journals the row BEFORE the first line goes out and
writes the verdict row after, then answers `OK id=<n> verdict=executed` or
`refused:<why>`. `watch` parks on `subscribe … events` and arms the `await`
family for its deadlines; it never sleeps and never counts down. With no
control socket answering, `switch` answers `verdict=refused:unresolved` and
`watch` does not start and says why on stderr — either way a host that cannot
reach the session never reports a decision as though it had acted.

`config` reads and writes the harness's OWN `config.toml`. Every key is in a
closed registry, so an unknown one is refused by name rather than ignored, and
the per-class action sets of design 5.8.7 are enforced on write: a `switch-*`
for network-offline, unknown or model-bucket-limit, a `retry` for unknown, or
anything but the pinned members for auth and spend-billing is REFUSED and the
file is left exactly as it was. `harness.enabled` is not a key here (it lives
in aterm.toml) and neither is `[accounts] enabled` (it lives beside the roster
in accounts.toml) — one home per key.

THREE OFF SWITCHES, widest first. `harness.enabled = false` in aterm.toml is
the durable one for this machine — it is NOT in the harness's own state, so it
keeps working when the harness does not, and `enable`/`disable` here cannot
write it. `$ATERM_NO_HARNESS` bypasses one session with no config write. The
`enable`/`disable` verbs own one capability each, durably, in the harness's
own state directory.
";

// ---------------------------------------------------------------------------
// Injected environment
// ---------------------------------------------------------------------------

/// Everything the subcommands need from outside themselves, injected so every
/// decision below is a pure function of its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Env {
    /// The harness state directory (design §4.4). Absolute.
    pub state: PathBuf,
    /// The session's working directory. Absolute.
    pub cwd: PathBuf,
    /// The owner's home, when known; `None` leaves the `rm` policy's home
    /// patterns inert (see [`RmPolicy::home`]).
    pub home: Option<PathBuf>,
    /// The Claude settings file `install`/`uninstall` merge into.
    pub settings: PathBuf,
    /// Unix seconds. Read once, at the top of [`main_entry`].
    pub now: i64,
    /// The aterm session id (`$ATERM_PARENT_SESSION_ID`), or empty.
    pub sid: String,
    /// The TARGET SESSION's public launch nonce, 32 hex characters, as the
    /// `local`/`sessions` roster row publishes it (`nonce=<hex32>`), or empty.
    ///
    /// It is what `turn id=<epoch>:…` is keyed by, and the server parses it
    /// with `LaunchNonce::from_hex` BEFORE it types anything — so a wrong one
    /// is not a degraded act, it is no act at all. This module opens no
    /// control socket and so cannot read the roster itself: a host that holds
    /// the watch passes `--nonce`, and a caller that does not gets
    /// `refused:unresolved` on every typed act rather than a printed line
    /// that would die at parse. It used to be filled with the SESSION ID,
    /// which is not a launch nonce.
    pub nonce: String,
    /// Seconds east of UTC for printed wall-clock times.
    pub utc_offset_s: i64,
    /// `--sock` — the control socket the ACTING verbs (`switch`, `watch`)
    /// drive. `None` resolves it the way every other aterm client does
    /// ([`aterm_ctl::resolve_sock_for`]), which is also what the read verbs
    /// would do if any of them opened one; none does.
    pub sock: Option<String>,
    /// `aterm.toml` — where the DURABLE master switch lives (design §4.6.2).
    /// `None` when neither `$XDG_CONFIG_HOME` nor `$HOME` is set, which reads
    /// as "the key is unset", i.e. the default: ON.
    pub config: Option<PathBuf>,
    /// The raw `$ATERM_NO_HARNESS` value — the per-session bypass. `None`
    /// when unset; EMPTY and `"0"` are not engaged
    /// ([`super::mark::env_engaged`]).
    pub no_harness: Option<String>,
    /// The raw `$ATERM_HARNESS_CAPS` value the `__harness` prelude exports.
    /// Its presence is what proves a prelude ran.
    pub caps_env: Option<String>,
    /// The harness TREE — the signed store build, or a dev-linked checkout
    /// (`$ATERM_HARNESS_ROOT`, `--tree`). `None` means no tree was named and
    /// none was found, which reads `probe pending` and never an error: design
    /// §1.4's rule is that nothing about a harness may make the wrapped
    /// program un-launchable, and the read verbs hold the same line.
    pub tree: Option<PathBuf>,
    /// The installed program the probes measure (`--against`); `None` means
    /// resolve `wraps_any`'s first name on `PATH`.
    pub against: Option<PathBuf>,
    /// The aterm identity token the alignment tuple is keyed by (design §3.1:
    /// what `aterm --version` prints after the `aterm ` prefix, verbatim,
    /// compared for equality and never ordered).
    pub aterm_build: String,
    /// `$CLAUDE_CONFIG_DIR` — which account this session is running under
    /// (design §5.6). `None` means the variable is unset, which is the
    /// vendor's DEFAULT account: `<home>/.claude`, resolved by
    /// [`Env::active_config_dir`] rather than assumed here, so a machine with
    /// no home reads "unknown" and not the wrong directory.
    pub claude_config_dir: Option<PathBuf>,
}

impl Env {
    /// THE one environment read in this module.
    ///
    /// # Errors
    ///
    /// The state root cannot be located, or the working directory cannot be
    /// read.
    pub fn from_process() -> Result<Env, String> {
        let home = std::env::var_os("HOME").map(PathBuf::from).filter(|h| {
            // A relative HOME would make every home-anchored deny pattern
            // compare against the wrong thing; treat it as unknown.
            h.is_absolute()
        });
        let state = match std::env::var_os("ATERM_HARNESS_STATE") {
            Some(dir) if Path::new(&dir).is_absolute() => PathBuf::from(dir),
            Some(_) => return Err("ATERM_HARNESS_STATE must be absolute".to_string()),
            None => crate::operator::default_state_root()
                .map_err(|e| format!("the aterm state root is unknown: {e}"))?
                .join("harness")
                .join(HARNESS),
        };
        let cwd = std::env::current_dir()
            .map_err(|e| format!("the working directory cannot be read: {e}"))?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
            .unwrap_or(0);
        let settings = home
            .clone()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".claude")
            .join("settings.json");
        Ok(Env {
            state,
            cwd,
            home,
            settings,
            now,
            sid: std::env::var("ATERM_PARENT_SESSION_ID").unwrap_or_default(),
            nonce: std::env::var("ATERM_HARNESS_NONCE")
                .ok()
                .filter(|n| watch::valid_turn_nonce(n))
                .unwrap_or_default(),
            utc_offset_s: 0,
            sock: None,
            config: default_config_path(),
            no_harness: std::env::var("ATERM_NO_HARNESS").ok(),
            caps_env: std::env::var("ATERM_HARNESS_CAPS").ok(),
            tree: std::env::var_os("ATERM_HARNESS_ROOT")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute()),
            against: None,
            aterm_build: String::from(aterm_types::version::APP_VERSION),
            claude_config_dir: std::env::var_os("CLAUDE_CONFIG_DIR")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute()),
        })
    }

    /// The account roster (design §5.6). Absent is the EMPTY roster with
    /// rotation off, never an error.
    #[must_use]
    pub fn accounts_path(&self) -> PathBuf {
        self.state.join(accounts::ACCOUNTS_FILE)
    }

    /// The harness's OWN `config.toml` (design §3.7, §5.8.7). Absent is the
    /// shipped default, never an error — a first run has no file.
    #[must_use]
    pub fn harness_config_path(&self) -> PathBuf {
        self.state.join(config::CONFIG_FILE)
    }

    /// The config directory this session is running under: `$CLAUDE_CONFIG_DIR`
    /// where it is set, else the vendor's default `<home>/.claude`. `None`
    /// only where neither is known — a state the roster prints as `active=-`
    /// rather than guessing a directory and calling an account active.
    #[must_use]
    pub fn active_config_dir(&self) -> Option<PathBuf> {
        self.claude_config_dir
            .clone()
            .or_else(|| self.home.as_ref().map(|h| h.join(".claude")))
    }

    /// The durable master switch, read from [`Env::config`]. `None` is the
    /// key being unset, which is the default: ON.
    ///
    /// An UNREADABLE config file also reads `None`. That is deliberate and it
    /// is the one place this module's tie-breaking runs the other way: a
    /// missing or unreadable `aterm.toml` is the normal state of a fresh
    /// machine, and treating it as "the owner switched the harness off" would
    /// make the product inert out of the box for a reason nothing displays.
    #[must_use]
    pub fn master_switch(&self) -> Option<bool> {
        let path = self.config.as_ref()?;
        let text = std::fs::read_to_string(path).ok()?;
        toml_bool(&text, "harness", "enabled")
    }

    /// How a harness is attached to this session (design §4.6.1's
    /// `bypassed=no-prelude`).
    ///
    /// The prelude exporting `$ATERM_HARNESS_CAPS` is the design's own
    /// evidence. A HAND `aterm harness install` is attached too — see
    /// [`DEFAULT_CAPS`]'s deviation note: the prelude does not exist yet, and
    /// a mark that read `bypassed` on every hand install would be reporting
    /// the prelude's absence as the harness's.
    #[must_use]
    pub fn attach(&self) -> Attach {
        if self.caps_env.as_deref().is_some_and(|c| !c.is_empty()) {
            Attach::Prelude
        } else if self.bridge_path().is_file() {
            Attach::Installed
        } else {
            Attach::None
        }
    }

    /// The per-capability switches on disk.
    #[must_use]
    pub fn caps_off(&self) -> Vec<String> {
        mark::read_disabled(&self.state).into_iter().collect()
    }

    /// The file the user's own statusLine command is copied into when
    /// `install` takes the slot, and chained from at every firing
    /// (design §4.5 `statusline` branch, §5.2 source (a)).
    #[must_use]
    pub fn statusline_user(&self) -> PathBuf {
        self.state.join("statusline.user")
    }

    /// The plugin tree `install` writes (design §4.5).
    #[must_use]
    pub fn plugin_dir(&self) -> PathBuf {
        self.state.join("plugin")
    }

    /// Where the vendor keeps its transcripts: `~/.claude/projects`. A
    /// FILESYSTEM fact, not a hook — `--bare` removes hooks, plugins and the
    /// statusLine in one flag and does not touch this, which is why §5.2's
    /// zero-hook answer is read from here.
    #[must_use]
    pub fn transcripts_root(&self) -> Option<PathBuf> {
        self.home
            .as_ref()
            .map(|h| h.join(".claude").join("projects"))
    }

    /// The bridge script every registered hook runs.
    #[must_use]
    pub fn bridge_path(&self) -> PathBuf {
        self.plugin_dir().join("hooks").join("bridge.sh")
    }

    /// Where the ledgers live.
    #[must_use]
    pub fn ledger_dir(&self) -> PathBuf {
        self.state.join("ledger")
    }

    /// The harness TREE this pass reads its contract and probes from
    /// (design §1.3): `--tree`, else `$ATERM_HARNESS_ROOT`, else a dev-linked
    /// `<state>/tree` when that is a directory.
    ///
    /// The dev-linked fallback is what makes this stage usable before the
    /// owner's publishing ceremony exists: `ln -s <checkout>/harness
    /// <state>/tree` and every verb here works against it exactly as it will
    /// against `store/claude-harness/<build>/`. `None` is not an error — it is
    /// `probe pending` with a note saying how to name one.
    #[must_use]
    pub fn harness_tree(&self) -> Option<PathBuf> {
        if let Some(tree) = &self.tree {
            return Some(tree.clone());
        }
        let dev = self.state.join("tree");
        dev.is_dir().then_some(dev)
    }

    /// The verbatim copy of the settings file taken before the first merge,
    /// so `uninstall` can put the original bytes back.
    #[must_use]
    pub fn settings_backup(&self) -> PathBuf {
        let mut name = self.settings.as_os_str().to_os_string();
        name.push(".aterm-harness.orig");
        PathBuf::from(name)
    }
}

// A `Source` enum used to live here, spelling aterm's own view `spine` while
// `usage::WindowSource` spelled the identical fact `grid`. One vocabulary now
// (`super::source::Source`), so a read verb's `source=` and a window's
// `source` are the same word for the same input; `spine` is the architecture
// word and no longer a wire one.

// ---------------------------------------------------------------------------
// The grammar
// ---------------------------------------------------------------------------

/// One parsed invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cmd {
    /// Answer one vendor hook. Always exits 0.
    Hook {
        /// The vendor's event name, verbatim.
        event: String,
        /// The capability that owns the hook (design §4.5 rule (b)); kept
        /// for the ledger row, never used to widen a decision.
        cap: Option<String>,
    },
    /// Record the statusLine JSON and print one footer line. Always exits 0.
    StatusLine,
    /// Write the plugin tree and merge into the settings file.
    Install {
        /// Print what would be written; write nothing.
        dry_run: bool,
    },
    /// Undo exactly what `install` did.
    Uninstall {
        /// Print what would be written; write nothing.
        dry_run: bool,
        /// Also remove the ledgers.
        purge: bool,
    },
    /// What the harness is, and what it can currently see.
    Status {
        /// The JSON form.
        json: bool,
        /// The caller ASSERTS a live watch on the grid spine. Nothing here
        /// can check it, so the output says the answer rests on it.
        spine: bool,
    },
    /// The mark alone (design §4.6): one state, and the control-protocol
    /// lines that render it.
    Mark {
        /// The JSON form.
        json: bool,
        /// Print the `meta set …` / `appnotice …` command lines instead of
        /// the state, for a host to send over the control socket.
        commands: bool,
        /// The caller ASSERTS a live watch on the grid spine. Nothing here
        /// can check it, so the output says the answer rests on it.
        spine: bool,
    },
    /// Turn capabilities on, durably, in the harness's own state.
    Enable {
        /// The capabilities named; EMPTY means every declared one.
        caps: Vec<String>,
    },
    /// Turn capabilities off, durably, in the harness's own state.
    Disable {
        /// The capabilities named; EMPTY means every declared one.
        caps: Vec<String>,
    },
    /// The usage view (design §5.2).
    Usage {
        /// The JSON form.
        json: bool,
    },
    /// The failure classifier's verdict (design §5.8).
    Limits {
        /// The JSON form.
        json: bool,
    },
    /// The stall verdict and the rung (design §5.4).
    Liveness {
        /// The JSON form.
        json: bool,
    },
    /// What the recovery table would do (design §5.8.4, §5.8.7).
    Recover {
        /// A class name or an action name.
        what: String,
        /// The model or account label the step aims at.
        target: Option<String>,
        /// The JSON form.
        json: bool,
        /// Print the control lines instead of the decision.
        commands: bool,
        /// The caller ASSERTS a live watch on the grid spine. This command
        /// opens no socket, so an L3 act's guards are assumptions without it.
        spine: bool,
    },
    /// One ladder rung, by hand (design §5.4, §5.7).
    Nudge {
        /// The session the rung is about; empty means this one.
        sid: String,
        /// Which rung.
        level: watch::Level,
        /// The JSON form.
        json: bool,
        /// Print the control lines instead of the decision.
        commands: bool,
        /// The caller ASSERTS a live watch on the grid spine. This command
        /// opens no socket, so an L3 act's guards are assumptions without it.
        spine: bool,
    },
    /// The disk report, and the one apply path (design §5.5).
    Disk {
        /// Build directories to consider. EMPTY means no `cargo-targets`
        /// rows: this command enumerates no workspace of its own.
        targets: Vec<PathBuf>,
        /// The ONE safelist class to act on. `None` is report-only.
        apply: Option<disk::Class>,
        /// The JSON form.
        json: bool,
    },
    /// The alignment verdict for the installed program (design §3.2, §3.5).
    Align {
        /// The JSON form.
        json: bool,
        /// Append the verdict to the alignment sidecar.
        write: bool,
        /// One signed `alignment` entry of design §3.4, to intersect the
        /// local verdict with. EMPTY means no signed row, which is the
        /// `(local)` case.
        signed: Option<String>,
    },
    /// The per-capability verdict, its reason and the probe that decided it
    /// (design §3.6, §5.7's `harness caps`).
    Caps {
        /// The JSON form.
        json: bool,
        /// One signed `alignment` entry, as for [`Cmd::Align`].
        signed: Option<String>,
    },
    /// ONE switch, asked for by hand and ACTUALLY RUN (design §5.7).
    Switch {
        /// Model or account.
        kind: watch::SwitchKind,
        /// The model to type, or the account label to relaunch into.
        target: String,
        /// The JSON form.
        json: bool,
    },
    /// RUN THE WATCH LOOP against this instance (design §5.3-§5.8).
    Watch {
        /// Stop after this many decision passes; `None` runs until the
        /// pushed stream ends. It is a BOUND, never a cadence.
        passes: Option<u64>,
        /// The JSON form: one object per pass.
        json: bool,
    },
    /// Read or write the harness's own `config.toml` (design §5.8.7).
    Config {
        /// The key; EMPTY on a `get` means every key.
        key: String,
        /// `Some` for a `set`, `None` for a `get`.
        value: Option<String>,
        /// The JSON form.
        json: bool,
    },
    /// The account roster and what may be rotated into (design §5.6).
    Accounts {
        /// Add ONE `config-dir` row by hand: the label and its directory.
        /// `None` for the list and the discovery forms.
        add: Option<(String, PathBuf)>,
        /// Scan for config directories and PROPOSE rows instead of listing
        /// the roster.
        discover: bool,
        /// `discover --write`: append the proposals the roster does not
        /// already carry. Without it, discovery writes nothing.
        write: bool,
        /// The JSON form.
        json: bool,
    },
    /// Rows out of one ledger.
    Ledger {
        /// Which ring.
        name: String,
        /// How many rows, newest last.
        count: usize,
        /// Only rows after this id.
        since: u64,
        /// The JSON form (one object per line, with the id).
        json: bool,
    },
    /// Print [`USAGE`] and exit 0.
    Help,
}

/// Flags that are not part of a [`Cmd`] because they configure [`Env`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Overrides {
    /// `--state`.
    pub state: Option<PathBuf>,
    /// `--settings`.
    pub settings: Option<PathBuf>,
    /// `--nonce` — the target session's public launch nonce.
    pub nonce: Option<String>,
    /// `--utc-offset`.
    pub utc_offset_s: Option<i64>,
    /// `--config` — where the durable master switch is read from.
    pub config: Option<PathBuf>,
    /// `--tree` — the harness tree (the signed store build, or a dev-linked
    /// checkout).
    pub tree: Option<PathBuf>,
    /// `--against` — the installed program the probes measure.
    pub against: Option<PathBuf>,
    /// `--aterm-build` — the identity token the alignment tuple is keyed by.
    /// Injected so a test never keys a verdict by this build's own version.
    pub aterm_build: Option<String>,
    /// `--sock` — the control socket `switch`/`watch` act through. `None`
    /// resolves it the way every other aterm client does.
    pub sock: Option<String>,
}

/// Parse `args` (the operands AFTER `harness`).
///
/// # Errors
///
/// An unknown subcommand, an unknown flag, a flag with no value, or a
/// non-numeric count.
pub fn parse(args: &[String]) -> Result<(Cmd, Overrides), String> {
    let mut over = Overrides::default();
    let mut rest: Vec<&str> = Vec::new();
    let mut json = false;
    let mut dry_run = false;
    let mut purge = false;
    let mut since = 0u64;
    let mut i = 0;
    let mut help = false;
    let mut commands = false;
    let mut spine = false;
    let mut level: Option<watch::Level> = None;
    let mut target: Option<String> = None;
    let mut apply: Option<disk::Class> = None;
    let mut passes: Option<u64> = None;
    let mut signed: Option<String> = None;
    let mut write = false;
    // The flag values are read by advancing `i` rather than through a closure:
    // a closure capturing `i` mutably would still hold that borrow at the
    // `i += 1` below.
    while i < args.len() {
        let a = args[i].as_str();
        let value = |i: usize, flag: &str| -> Result<String, String> {
            args.get(i + 1)
                .cloned()
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match a {
            "-h" | "--help" | "help" => help = true,
            "--json" => json = true,
            "--dry-run" => dry_run = true,
            "--purge" => purge = true,
            "--commands" => commands = true,
            "--assume-spine" => spine = true,
            "--config" => {
                over.config = Some(PathBuf::from(value(i, "--config")?));
                i += 1;
            }
            "--state" => {
                over.state = Some(PathBuf::from(value(i, "--state")?));
                i += 1;
            }
            "--settings" => {
                over.settings = Some(PathBuf::from(value(i, "--settings")?));
                i += 1;
            }
            "--tree" => {
                over.tree = Some(PathBuf::from(value(i, "--tree")?));
                i += 1;
            }
            "--against" => {
                over.against = Some(PathBuf::from(value(i, "--against")?));
                i += 1;
            }
            "--aterm-build" => {
                over.aterm_build = Some(value(i, "--aterm-build")?);
                i += 1;
            }
            "--signed" => {
                signed = Some(value(i, "--signed")?);
                i += 1;
            }
            "--write" => write = true,
            "--nonce" => {
                let v = value(i, "--nonce")?;
                if !watch::valid_turn_nonce(&v) {
                    return Err(format!(
                        "--nonce wants the target session's 32-character launch nonce, the \
                         `nonce=<hex32>` its `local` roster row carries, not {v:?}"
                    ));
                }
                over.nonce = Some(v);
                i += 1;
            }
            "--utc-offset" => {
                let v = value(i, "--utc-offset")?;
                over.utc_offset_s =
                    Some(v.parse::<i64>().map_err(|_| {
                        format!("--utc-offset wants a number of seconds, not {v:?}")
                    })?);
                i += 1;
            }
            "--level" => {
                let v = value(i, "--level")?;
                level =
                    Some(watch::Level::parse(&v).ok_or_else(|| {
                        format!("--level wants warn|stop|turn|escape, not {v:?}")
                    })?);
                i += 1;
            }
            "--target" => {
                target = Some(value(i, "--target")?);
                i += 1;
            }
            "--sock" => {
                over.sock = Some(value(i, "--sock")?);
                i += 1;
            }
            "--passes" => {
                let v = value(i, "--passes")?;
                passes = Some(
                    v.parse::<u64>()
                        .map_err(|_| format!("--passes wants a count, not {v:?}"))?,
                );
                i += 1;
            }
            "--apply" => {
                let v = value(i, "--apply")?;
                apply = Some(disk::Class::parse(&v).ok_or_else(|| {
                    format!(
                        "--apply wants one of {}, not {v:?}",
                        disk::Class::ALL
                            .iter()
                            .map(|c| c.as_str())
                            .collect::<Vec<_>>()
                            .join(" | ")
                    )
                })?);
                i += 1;
            }
            "--since" => {
                let v = value(i, "--since")?;
                since = v
                    .parse::<u64>()
                    .map_err(|_| format!("--since wants a row id, not {v:?}"))?;
                i += 1;
            }
            other if other.starts_with('-') => return Err(format!("unknown flag {other}")),
            other => rest.push(other),
        }
        i += 1;
    }
    if help {
        return Ok((Cmd::Help, over));
    }
    let Some(&verb) = rest.first() else {
        return Ok((Cmd::Help, over));
    };
    let cmd = match verb {
        "hook" => {
            let event = rest
                .get(1)
                .ok_or_else(|| "hook needs the vendor's event name".to_string())?;
            Cmd::Hook {
                event: (*event).to_string(),
                cap: rest.get(2).map(|c| (*c).to_string()),
            }
        }
        "statusline" => Cmd::StatusLine,
        "install" => Cmd::Install { dry_run },
        "uninstall" => Cmd::Uninstall { dry_run, purge },
        "status" => Cmd::Status { json, spine },
        "mark" => Cmd::Mark {
            json,
            commands,
            spine,
        },
        "enable" => Cmd::Enable {
            caps: cap_operands(&rest[1..]),
        },
        "disable" => Cmd::Disable {
            caps: cap_operands(&rest[1..]),
        },
        "align" => Cmd::Align {
            json,
            write,
            signed: signed.clone(),
        },
        "caps" => Cmd::Caps {
            json,
            signed: signed.clone(),
        },
        "usage" => Cmd::Usage { json },
        "accounts" => {
            let (discover, add) = match rest.get(1) {
                None => (false, None),
                Some(&"discover") => (true, None),
                Some(&"add") => {
                    let label = rest.get(2).ok_or_else(|| {
                        "accounts add needs a label and the config directory it names".to_string()
                    })?;
                    let dir = rest.get(3).ok_or_else(|| {
                        format!(
                            "accounts add {label} needs the config directory: `accounts add \
                             <label> <dir>`"
                        )
                    })?;
                    (false, Some(((*label).to_string(), PathBuf::from(*dir))))
                }
                Some(other) => {
                    return Err(format!(
                        "accounts takes nothing, `discover` or `add <label> <dir>`, not {other:?}"
                    ));
                }
            };
            if write && !discover {
                return Err(
                    "--write belongs to `accounts discover`: the roster is the owner's file and \
                     `accounts` alone never writes it"
                        .to_string(),
                );
            }
            Cmd::Accounts {
                add,
                discover,
                write,
                json,
            }
        }
        "switch" => {
            let kind_word = rest
                .get(1)
                .ok_or_else(|| "switch needs `model` or `account`".to_string())?;
            let kind = watch::SwitchKind::parse(kind_word)
                .ok_or_else(|| format!("switch takes `model` or `account`, not {kind_word:?}"))?;
            // The target is an OPERAND, not `--target`: `switch account alt-1`
            // reads as one sentence, and a switch with no target would have
            // to invent one from the config — which is how a rotation into
            // the account you are already on happens.
            let target = rest
                .get(2)
                .map(|t| (*t).to_string())
                .or_else(|| target.clone())
                .ok_or_else(|| {
                    format!(
                        "switch {} needs the target: the model to type, or the account label to \
                         relaunch into (`aterm harness accounts` lists them)",
                        kind.as_str()
                    )
                })?;
            Cmd::Switch { kind, target, json }
        }
        "watch" => Cmd::Watch { passes, json },
        "config" => {
            let action = rest
                .get(1)
                .ok_or_else(|| "config needs `get` or `set`".to_string())?;
            match *action {
                "get" => Cmd::Config {
                    key: rest.get(2).map_or(String::new(), |k| (*k).to_string()),
                    value: None,
                    json,
                },
                "set" => {
                    let k = rest
                        .get(2)
                        .ok_or_else(|| "config set needs a key and a value".to_string())?;
                    // The VALUE is the rest of the line joined by spaces, so
                    // `config set cap.limits.actions.auth relogin escalate`
                    // works without a shell quote and an accidentally split
                    // array is not read as a one-element one.
                    let value = rest.get(3..).unwrap_or(&[]).join(" ");
                    if value.is_empty() {
                        return Err(format!(
                            "config set {k} needs a value (`aterm harness config get {k}` prints \
                             the one in force)"
                        ));
                    }
                    Cmd::Config {
                        key: (*k).to_string(),
                        value: Some(value),
                        json,
                    }
                }
                other => {
                    return Err(format!("config takes `get` or `set`, not {other:?}"));
                }
            }
        }
        "limits" => Cmd::Limits { json },
        "liveness" => Cmd::Liveness { json },
        "recover" => {
            let what = rest.get(1).ok_or_else(|| {
                "recover needs a class or an action (`aterm harness limits` names the class)"
                    .to_string()
            })?;
            if limits::Class::parse(what).is_none() && limits::Action::parse(what).is_none() {
                return Err(format!(
                    "no class or action called {what:?}; the classes are {} and the actions are {}",
                    limits::Class::ALL
                        .iter()
                        .map(|c| c.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    limits::Action::ALL
                        .iter()
                        .map(|a| a.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            Cmd::Recover {
                what: (*what).to_string(),
                target,
                json,
                commands,
                spine,
            }
        }
        "nudge" => Cmd::Nudge {
            sid: rest.get(1).map_or(String::new(), |s| (*s).to_string()),
            level: level.unwrap_or(watch::Level::Warn),
            json,
            commands,
            spine,
        },
        "disk" => Cmd::Disk {
            targets: rest[1..].iter().map(PathBuf::from).collect(),
            apply,
            json,
        },
        "ledger" => {
            let name = rest
                .get(1)
                .ok_or_else(|| format!("ledger needs one of {}", RINGS.join(", ")))?;
            if ring_config(name).is_none() {
                return Err(format!(
                    "no ledger called {name:?}; the ledgers are {}",
                    RINGS.join(", ")
                ));
            }
            let count = match rest.get(2) {
                Some(n) => n
                    .parse::<usize>()
                    .map_err(|_| format!("ledger wants a row count, not {n:?}"))?,
                None => LEDGER_DEFAULT_ROWS,
            };
            Cmd::Ledger {
                name: (*name).to_string(),
                count,
                since,
                json,
            }
        }
        other => return Err(format!("unknown subcommand {other:?}")),
    };
    Ok((cmd, over))
}

/// The capability names on an `enable`/`disable` line. Both spellings are
/// accepted: design §4.6.2's `cap=<c>` and the bare name, because a person
/// typing the verb twice a day will type the bare one and a refusal there
/// teaches nothing.
fn cap_operands(rest: &[&str]) -> Vec<String> {
    rest.iter()
        .map(|o| o.strip_prefix("cap=").unwrap_or(o))
        .filter(|o| !o.is_empty())
        .map(|o| (*o).to_string())
        .collect()
}

// ---------------------------------------------------------------------------
// The hook bridge — the pure half
// ---------------------------------------------------------------------------

/// What a vendor event is for.
///
/// MEASURED (design §4.5, the binary's own `/hooks` help table):
/// `PermissionRequest` and `PreToolUse` carry a DECISION channel; every other
/// event's stdout is ignored, so writing to it would be noise at best.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookClass {
    /// The vendor reads one JSON object off stdout.
    Decide,
    /// The vendor reads nothing; stdout must be empty.
    Event,
}

/// Which class `event` is in. Unknown events are [`HookClass::Event`] — the
/// safe answer, since printing a decision the vendor did not ask for is the
/// only way this command can change behaviour it was not invited to change.
#[must_use]
pub fn hook_class(event: &str) -> HookClass {
    match HookEvent::parse(event) {
        Some(_) => HookClass::Decide,
        None => HookClass::Event,
    }
}

/// Everything one hook firing produces.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HookReply {
    /// What goes to stdout. EMPTY for every [`HookClass::Event`] kind, and
    /// empty for a decide kind that abstains.
    pub stdout: String,
    /// A row for [`RING_RM`].
    pub rm_row: Option<String>,
    /// A row for [`RING_EVENT`].
    pub event_row: Option<String>,
    /// A row for [`RING_STATUSLINE`].
    pub statusline_row: Option<String>,
}

/// The decision object for an allow, in the shape the vendor's own zod schema
/// documents (design §5.1, MEASURED from the 2.1.274 binary):
/// `PermissionRequest` answers `decision: {behavior: "allow"}` and
/// `PreToolUse` answers `permissionDecision: "allow"` with a reason.
///
/// There is no deny form and there never will be one here: the policy of
/// [`super::rm_policy`] has exactly two answers, allow and abstain, and an
/// abstain prints nothing so the vendor's own prompt shows.
#[must_use]
pub fn allow_json(event: HookEvent, id: u64) -> String {
    let mut inner = Map::new();
    inner.insert(
        "hookEventName".to_owned(),
        Value::from(event.as_str().to_owned()),
    );
    match event {
        HookEvent::PermissionRequest => {
            let mut decision = Map::new();
            decision.insert("behavior".to_owned(), Value::from("allow".to_owned()));
            inner.insert("decision".to_owned(), Value::Object(decision));
        }
        HookEvent::PreToolUse => {
            inner.insert(
                "permissionDecision".to_owned(),
                Value::from("allow".to_owned()),
            );
            // IN-BAND ATTRIBUTION through the ONE helper (design §4.6.1):
            // the line this decision causes inside the vendor's own
            // transcript names the harness and the ledger row that explains
            // it, so a reader never has to guess who approved a tool call.
            inner.insert(
                "permissionDecisionReason".to_owned(),
                Value::from(mark::attribution(Voice::Field, RULE_RM, id)),
            );
        }
    }
    let mut root = Map::new();
    root.insert("hookSpecificOutput".to_owned(), Value::Object(inner));
    aterm_json::to_string(&Value::Object(root)).unwrap_or_default()
}

/// A bounded `&str` field out of a hook payload.
fn field<'a>(payload: &'a Value, key: &str) -> Option<&'a str> {
    payload.get(key).and_then(Value::as_str)
}

/// The command a Bash tool call carries, if it is one.
fn tool_command(payload: &Value) -> Option<&str> {
    payload
        .get("tool_input")
        .and_then(|i| i.get("command"))
        .and_then(Value::as_str)
}

/// Decide one hook firing. PURE: the clock, the paths and the row id are all
/// arguments.
///
/// `id` is the id the rm ring will assign; it goes into the row AND into the
/// `PreToolUse` reason, so a person reading the vendor's transcript can find
/// the ledger row that explains the approval.
///
/// A payload that is not JSON, is not an object, or is empty ABSTAINS: it
/// still produces a row (so the failure is visible) and prints nothing.
#[must_use]
pub fn hook_reply(event: &str, payload: &str, cap: Option<&str>, env: &Env, id: u64) -> HookReply {
    let parsed: Option<Value> = aterm_json::from_str::<Value>(payload)
        .ok()
        .filter(|v| matches!(v, Value::Object(_)));
    let ts = rfc3339_utc(env.now);
    match hook_class(event) {
        HookClass::Decide => {
            // `parse` succeeded for `hook_class` to answer Decide.
            let Some(ev) = HookEvent::parse(event) else {
                return HookReply::default();
            };
            let doc = parsed.unwrap_or(Value::Null);
            let (verdict, cwd) = rm_verdict(&doc, env, ev);
            let stdout = match verdict.decision {
                RmDecision::Allow => allow_json(ev, id),
                RmDecision::Abstain => String::new(),
            };
            let row = rm_policy::to_ledger_json(
                &verdict,
                &LedgerFields {
                    id,
                    ts: &ts,
                    sid: &env.sid,
                    session_id: field(&doc, "session_id").unwrap_or_default(),
                    cwd: &cwd.display().to_string(),
                    event: ev,
                    command: tool_command(&doc).unwrap_or_default(),
                    mode: field(&doc, "permission_mode").unwrap_or_default(),
                    generation: 0,
                },
            );
            HookReply {
                stdout,
                rm_row: Some(row),
                ..HookReply::default()
            }
        }
        HookClass::Event => {
            let doc = parsed.unwrap_or(Value::Null);
            let row = event_row_json(event, &doc, cap, &ts, &env.sid, id, payload.is_empty());
            if event == "StatusLine" {
                HookReply {
                    statusline_row: Some(row),
                    ..HookReply::default()
                }
            } else {
                HookReply {
                    event_row: Some(row),
                    ..HookReply::default()
                }
            }
        }
    }
}

/// The rm verdict for one decide-class payload, and the directory it was
/// computed against.
///
/// The payload's own `cwd` wins when it is absolute — the vendor knows where
/// the tool call will run and this process may not — and [`Env::cwd`] is the
/// fallback. A payload naming a tool other than `Bash`, or carrying no
/// command, abstains by NAME rather than by accident, so the ledger row says
/// which it was.
fn rm_verdict(doc: &Value, env: &Env, ev: HookEvent) -> (RmVerdict, PathBuf) {
    let cwd = field(doc, "cwd")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| env.cwd.clone());
    let policy = RmPolicy {
        home: env.home.clone(),
        require_cwd_prefix: CwdPrefix::SessionCwd,
        ..RmPolicy::default()
    };
    // The mode rule is NOT re-stated here: `rm_policy::evaluate` applies it
    // first (rule 1), so `prompt-only` answering `PreToolUse` abstains with
    // that module's own reason string and there is one copy of the rule.
    let tool = field(doc, "tool_name").unwrap_or_default();
    let Some(command) = tool_command(doc) else {
        return (
            RmVerdict {
                decision: RmDecision::Abstain,
                reason: if matches!(doc, Value::Null) {
                    "input:unreadable".to_string()
                } else {
                    "input:no-command".to_string()
                },
                targets: Vec::new(),
            },
            cwd,
        );
    };
    if tool != "Bash" {
        return (
            RmVerdict {
                decision: RmDecision::Abstain,
                reason: "input:not-bash".to_string(),
                targets: Vec::new(),
            },
            cwd,
        );
    }
    let verdict = rm_policy::evaluate(command, &cwd, ev, &policy);
    (verdict, cwd)
}

/// One bounded enrichment row. The payload is NEVER echoed whole: only the
/// closed set of fields below is copied, each cut to [`EVENT_TEXT_CAP`], so a
/// hostile `tool_input` cannot fill the ledger and a row stays one line.
///
/// Rows are DATA, never instructions (design §4.4): every field here is text
/// a vendor, a screen or an agent wrote.
#[must_use]
pub fn event_row_json(
    event: &str,
    doc: &Value,
    cap: Option<&str>,
    ts: &str,
    sid: &str,
    id: u64,
    empty_input: bool,
) -> String {
    let mut o = Map::new();
    o.insert("id".to_owned(), Value::from(id));
    o.insert("ts".to_owned(), Value::from(ts.to_owned()));
    o.insert("sid".to_owned(), Value::from(sid.to_owned()));
    o.insert(
        "event".to_owned(),
        Value::from(truncate_bytes(event, 64).to_owned()),
    );
    if let Some(cap) = cap {
        o.insert(
            "cap".to_owned(),
            Value::from(truncate_bytes(cap, 64).to_owned()),
        );
    }
    if matches!(doc, Value::Null) {
        o.insert(
            "input".to_owned(),
            Value::from(if empty_input { "empty" } else { "unreadable" }.to_owned()),
        );
    }
    // The closed field set. Each is a string the vendor documents for at
    // least one event; an event that does not carry one simply has no key.
    for key in [
        "session_id",
        "tool_name",
        "error",
        "error_details",
        "error_type",
        "notification_type",
        "source",
        "from_model",
        "to_model",
        "prompt",
    ] {
        if let Some(v) = field(doc, key) {
            o.insert(
                key.to_owned(),
                Value::from(truncate_bytes(v, EVENT_TEXT_CAP).to_owned()),
            );
        }
    }
    aterm_json::to_string(&Value::Object(o))
        .unwrap_or_else(|_| format!("{{\"id\":{id},\"event\":\"unserializable\"}}"))
}

/// The HUD line for one statusLine payload, and the view behind it.
///
/// A payload that will not parse yields the fixed text below rather than an
/// error: the vendor renders this string in its footer, so an error message
/// there would be the harness shouting at the owner once a second.
#[must_use]
pub fn hud_from_statusline(
    payload: &str,
    now: i64,
    utc_offset_s: i64,
) -> (String, Option<UsageView>) {
    match parse_statusline(payload) {
        Ok(line) => {
            let mut view = UsageView::new(now);
            let mut account = AccountView::new("account", true);
            account.add_statusline(&line, 0);
            view.accounts.push(account);
            let text = usage::hud_line(&view, utc_offset_s);
            (text, Some(view))
        }
        Err(_) => ("aterm harness".to_string(), None),
    }
}

// ---------------------------------------------------------------------------
// The read verbs — the pure half
// ---------------------------------------------------------------------------

/// What the read verbs were able to see. Built from the ledgers alone, so it
/// is exactly as true with zero hooks as with all of them — the difference
/// shows up in [`Readout::source`], never in whether there is an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Readout {
    /// Where the answer came from.
    pub source: Source,
    /// Whether any hook has ever fired into this state directory.
    pub hooks_present: bool,
    /// Rows in each ledger, by name. EMPTY on a readout built by
    /// [`Readout::from_presence`], which asked only whether each ledger has
    /// rows; the counts are absent there, not zero.
    pub rows: BTreeMap<String, u64>,
    /// The newest id across every ledger; 0 when they are all empty.
    pub ledger_seq: u64,
}

impl Readout {
    /// The readout of a state directory whose ledgers hold `rows` rows.
    #[must_use]
    pub fn new(rows: BTreeMap<String, u64>, ledger_seq: u64) -> Readout {
        let statusline = rows.get(RING_STATUSLINE).copied().unwrap_or(0) > 0;
        let hooks = statusline
            || rows.get(RING_RM).copied().unwrap_or(0) > 0
            || rows.get(RING_EVENT).copied().unwrap_or(0) > 0;
        Readout {
            source: if statusline {
                Source::StatusLine
            } else if hooks {
                Source::Hook
            } else {
                Source::Grid
            },
            hooks_present: hooks,
            rows,
            ledger_seq,
        }
    }

    /// The same readout from PRESENCE alone — which ledgers hold rows, not
    /// how many. [`Readout::rows`] is left empty, because this constructor
    /// never learned the counts and reporting zeros would be a lie.
    ///
    /// Everything [`presence_of`] reads is computed here identically: the
    /// `source` ladder and `hooks_present` are `rows > 0` tests and nothing
    /// else, which is why the counts were never needed for the mark.
    #[must_use]
    pub fn from_presence(present: &BTreeMap<String, bool>, ledger_seq: u64) -> Readout {
        let statusline = present.get(RING_STATUSLINE).copied().unwrap_or(false);
        let hooks = statusline
            || present.get(RING_RM).copied().unwrap_or(false)
            || present.get(RING_EVENT).copied().unwrap_or(false);
        Readout {
            source: if statusline {
                Source::StatusLine
            } else if hooks {
                Source::Hook
            } else {
                Source::Grid
            },
            hooks_present: hooks,
            rows: BTreeMap::new(),
            ledger_seq,
        }
    }
}

/// The MARK for this session (design §4.6), built from the switches, the
/// attachment and what the ledgers saw.
///
/// `spine` is the caller's ASSERTION that a live watch on the grid spine is
/// up and answering — never assumed here, and never CHECKED here either. A
/// one-shot command holds no watch, so `run` passes what `--assume-spine`
/// said and nothing else: the honest answer for a bare `aterm harness status`
/// is `degraded=spine-down`, and a host that does hold the watch says so.
///
/// §4.6's *never `armed` on an assumption* is a CALLER obligation, and this
/// flag is where a caller discharges it by saying so. Nothing in this process
/// can verify it — the module opens no socket — so the two commands that take
/// it PRINT the assumption beside the mark it produced. That is the whole
/// mechanism: the assertion is visible, not silent.
#[must_use]
pub fn presence_of(read: &Readout, env: &Env, spine: bool) -> Presence {
    let caps = caps();
    let caps_off = env.caps_off();
    // The enrichment channel's cause, from what THIS process can prove.
    // `bare` is not among them: nothing here can see the vendor's launch
    // flags, so claiming it would be a guess (design §4.6.3 wants the cause
    // reported, not invented).
    let hooks_absent = if read.hooks_present {
        None
    } else if mark::env_engaged(env.no_harness.as_deref()) {
        Some(HooksAbsent::Env)
    } else if settings_carry_ours(env) {
        Some(HooksAbsent::Timeout)
    } else {
        Some(HooksAbsent::Settings)
    };
    mark::presence(&mark::Inputs {
        enabled: env.master_switch(),
        no_harness: env.no_harness.as_deref(),
        attach: Some(env.attach()),
        spine,
        caps: &caps,
        caps_off: &caps_off,
        // NO ACT from here, deliberately. `acting` is a two-second window
        // (§4.6.1) and this command's clock is [`Env::now`], which is whole
        // SECONDS; a one-shot process also cannot see an act that is still
        // open, only rows already journalled. Reporting `acting` off a
        // second-resolution ledger read would mean claiming a window this
        // process cannot measure, and §4.6.1's honesty rule is that the mark
        // never renders `acting` without the journal row behind it. A HOST
        // that holds the watch passes the act to [`super::mark::presence`]
        // directly; it is `Inputs::act` and it is not CLI-shaped.
        act: None,
        now_ms: 0,
        hooks: read.hooks_present,
        hooks_absent,
    })
}

/// Does the vendor settings file carry OUR entries? Cheap and textual: the
/// marker every command this module writes ends with is a literal.
fn settings_carry_ours(env: &Env) -> bool {
    std::fs::read_to_string(&env.settings).is_ok_and(|t| t.contains(OWN_MARK))
}

/// The `harness status` line. Field names and spellings MIRROR the control
/// protocol's `status` where they overlap (design §5.7, corrected
/// 2026-09-19): the same `schema=1`, fields additive, an unknown token read as
/// unknown rather than an error.
#[must_use]
pub fn status_line(read: &Readout, env: &Env) -> String {
    let rows = |name: &str| read.rows.get(name).copied().unwrap_or(0);
    format!(
        "OK schema=1 harness={HARNESS} abi={ABI} source={} hooks={} caps={DEFAULT_CAPS} \
         rm_rows={} event_rows={} statusline_rows={} ledger_seq={} state={}",
        read.source.as_str(),
        if read.hooks_present {
            "present"
        } else {
            "absent"
        },
        rows(RING_RM),
        rows(RING_EVENT),
        rows(RING_STATUSLINE),
        read.ledger_seq,
        env.state.display(),
    )
}

/// The `harness status --json` document, schema 1.
#[must_use]
pub fn status_json(read: &Readout, env: &Env) -> String {
    let mut o = Map::new();
    o.insert("schema".to_owned(), Value::from(1u64));
    o.insert("harness".to_owned(), Value::from(HARNESS.to_owned()));
    o.insert("abi".to_owned(), Value::from(u64::from(ABI)));
    o.insert(
        "source".to_owned(),
        Value::from(read.source.as_str().to_owned()),
    );
    o.insert("hooks".to_owned(), Value::from(read.hooks_present));
    o.insert(
        "caps".to_owned(),
        Value::Array(
            DEFAULT_CAPS
                .split(',')
                .map(|c| Value::from(c.to_owned()))
                .collect(),
        ),
    );
    o.insert("ledger_seq".to_owned(), Value::from(read.ledger_seq));
    o.insert(
        "rows".to_owned(),
        Value::Object(
            read.rows
                .iter()
                .map(|(k, v)| (k.clone(), Value::from(*v)))
                .collect(),
        ),
    );
    o.insert(
        "state".to_owned(),
        Value::from(env.state.display().to_string()),
    );
    aterm_json::to_string(&Value::Object(o)).unwrap_or_default()
}

/// A read verb's JSON form: the verb's own document with the `source` field
/// this module's law requires, wrapped so the inner document stays exactly
/// what its own module produces.
///
/// `extra` carries per-verb top-level keys, in the order given, for a verb
/// whose SOURCE word is not the whole provenance — `usage` also says which
/// transcript it folded and what chose it.
fn sourced_json(
    kind: &str,
    source: Source,
    body: &str,
    extra: Vec<(&'static str, Value)>,
) -> String {
    let mut o = Map::new();
    o.insert("schema".to_owned(), Value::from(1u64));
    o.insert("kind".to_owned(), Value::from(kind.to_owned()));
    o.insert("source".to_owned(), Value::from(source.as_str().to_owned()));
    for (key, value) in extra {
        o.insert(key.to_owned(), value);
    }
    o.insert(
        kind.to_owned(),
        aterm_json::from_str::<Value>(body).unwrap_or(Value::Null),
    );
    aterm_json::to_string(&Value::Object(o)).unwrap_or_default()
}

/// The evidence one recorded event row contributes to the limit classifier,
/// if any (design §5.8.1).
#[must_use]
pub fn evidence_from_row(line: &str) -> Option<Evidence> {
    let doc = aterm_json::from_str::<Value>(line).ok()?;
    let event = doc.get("event").and_then(Value::as_str)?;
    let text = |k: &str| doc.get(k).and_then(Value::as_str).map(str::to_owned);
    match event {
        "StopFailure" => Some(Evidence::StopFailure {
            error: text("error").unwrap_or_default(),
            details: text("error_details"),
        }),
        "Notification" => Some(Evidence::Notification {
            kind: text("notification_type").unwrap_or_default(),
        }),
        "PostModelSwitch" => Some(Evidence::PostModelSwitch {
            from: text("from_model").unwrap_or_default(),
            to: text("to_model").unwrap_or_default(),
            source: text("source"),
        }),
        _ => None,
    }
}

/// The window evidence one statusLine sample contributes.
#[must_use]
pub fn windows_from_row(line: &str, age_s: u64) -> Vec<Evidence> {
    let Ok(sl) = parse_statusline(line) else {
        return Vec::new();
    };
    usage::windows_from_statusline(&sl, age_s)
        .into_iter()
        .filter_map(|(name, w): (String, WindowView)| {
            WindowKind::parse(&name).map(|which| Evidence::Window {
                which,
                used_pct: w.used_pct,
                resets_at: w.resets_at,
                source: Source::StatusLine,
                age_s: Some(age_s),
            })
        })
        .collect()
}

/// The `harness limits` text for one classification (or the absence of one).
#[must_use]
pub fn limits_text(class: Option<&limits::Classification>, source: Source) -> String {
    match class {
        None => format!(
            "class=none source={} — no limit evidence in the ledger",
            source.as_str()
        ),
        Some(c) => format!(
            "class={} unpaired={} storm={} no_response={} resets_at={} source={} reasons={}",
            c.class.as_str(),
            c.unpaired,
            c.storm,
            c.no_response,
            c.resets_at.map_or("-".to_string(), rfc3339_utc),
            source.as_str(),
            if c.reasons.is_empty() {
                "-".to_string()
            } else {
                c.reasons.join(";")
            },
        ),
    }
}

/// The `harness limits --json` document, schema 1.
#[must_use]
pub fn limits_json(class: Option<&limits::Classification>, source: Source) -> String {
    let mut o = Map::new();
    o.insert("schema".to_owned(), Value::from(1u64));
    o.insert("kind".to_owned(), Value::from("limits".to_owned()));
    o.insert("source".to_owned(), Value::from(source.as_str().to_owned()));
    match class {
        None => {
            // "none" is not a [`Class`]: the classifier answers `None` when
            // no evidence names a class, and saying so is not the same as
            // naming `unknown`, which is its fail-closed class.
            o.insert("class".to_owned(), Value::from("none".to_owned()));
            o.insert("reasons".to_owned(), Value::Array(Vec::new()));
        }
        Some(c) => {
            o.insert("class".to_owned(), Value::from(c.class.as_str().to_owned()));
            o.insert("unpaired".to_owned(), Value::from(c.unpaired));
            o.insert("storm".to_owned(), Value::from(c.storm));
            o.insert("no_response".to_owned(), Value::from(c.no_response));
            o.insert(
                "resets_at".to_owned(),
                c.resets_at
                    .map_or(Value::Null, |r| Value::from(rfc3339_utc(r))),
            );
            o.insert(
                "reasons".to_owned(),
                Value::Array(c.reasons.iter().map(|r| Value::from(r.clone())).collect()),
            );
        }
    }
    aterm_json::to_string(&Value::Object(o)).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// The installed artifacts
// ---------------------------------------------------------------------------

/// `plugin/.claude-plugin/plugin.json`.
#[must_use]
pub fn plugin_manifest() -> String {
    let mut o = Map::new();
    o.insert("name".to_owned(), Value::from(HARNESS.to_owned()));
    o.insert(
        "description".to_owned(),
        Value::from("aterm harness bridge — control channel only; aterm renders".to_owned()),
    );
    o.insert("version".to_owned(), Value::from("1.0.0".to_owned()));
    format!(
        "{}\n",
        aterm_json::to_string(&Value::Object(o)).unwrap_or_default()
    )
}

/// The `(event, mode, capability)` rows the bridge is registered for
/// (design §4.5). Every row carries exactly three arguments, which is rule (b).
pub const HOOK_ROWS: &[(&str, &str, &str)] = &[
    ("PermissionRequest", "decide", "rm-approve"),
    ("PreToolUse", "decide", "rm-approve"),
    ("PostToolUse", "event", "rm-approve"),
    ("StopFailure", "event", "introspect"),
    ("Notification", "event", "introspect"),
    ("PostModelSwitch", "event", "introspect"),
    ("SessionStart", "event", "introspect"),
    ("SessionEnd", "event", "introspect"),
];

/// `plugin/hooks/hooks.json`, for a `--plugin-dir` launch.
#[must_use]
pub fn hooks_json() -> String {
    let mut events = Map::new();
    for (event, mode, cap) in HOOK_ROWS {
        let mut entry = Map::new();
        entry.insert("type".to_owned(), Value::from("command".to_owned()));
        entry.insert(
            "command".to_owned(),
            Value::from(format!(
                "sh \"${{CLAUDE_PLUGIN_ROOT}}/hooks/bridge.sh\" {mode} {event} {cap} {OWN_MARK}"
            )),
        );
        let mut group = Map::new();
        group.insert("hooks".to_owned(), Value::Array(vec![Value::Object(entry)]));
        let slot = events
            .entry((*event).to_owned())
            .or_insert_with(|| Value::Array(Vec::new()));
        if let Value::Array(list) = slot {
            list.push(Value::Object(group));
        }
    }
    let mut root = Map::new();
    root.insert(
        "description".to_owned(),
        Value::from("aterm harness bridge — control channel only; aterm renders".to_owned()),
    );
    root.insert("hooks".to_owned(), Value::Object(events));
    format!(
        "{}\n",
        aterm_json::to_string(&Value::Object(root)).unwrap_or_default()
    )
}

/// `plugin/hooks/bridge.sh` — the control channel, which never paints and
/// ALWAYS exits 0 (design §4.5).
///
/// `exe` is the absolute path of the aterm binary the hooks will run, so a
/// `$PATH` the vendor does not share cannot change which program answers.
#[must_use]
pub fn bridge_sh(exe: &str) -> String {
    format!(
        r#"#!/bin/sh
# aterm harness bridge — control channel only; aterm renders. {OWN_MARK}
# usage: bridge.sh <decide|event|statusline> <HookEvent> <capability-id>
# Never exits non-zero: Claude Code reads a failing hook command as a BLOCK.
# The per-session bypass, read with THE shared flag rule (mark::env_engaged):
# unset, EMPTY and "0" are not engaged. `-n` alone would have made an
# inherited `ATERM_NO_HARNESS=` a veto nothing intended, and would have made
# the bridge and the mark disagree about the same session — the one failure
# the indicator exists to prevent.
case "${{ATERM_NO_HARNESS:-}}" in ""|0) ;; *) exit 0 ;; esac
mode=$1; ev=$2; cap=$3
[ -n "$cap" ] || exit 0
# The cap gate of design 4.5, with the one deviation this build states in its
# own docs: the `__harness` prelude that exports ATERM_HARNESS_CAPS does not
# exist yet, so an unset variable falls back to the set `install` registered
# rather than making every hook inert. A prelude that sets it still NARROWS.
caps=${{ATERM_HARNESS_CAPS:-{DEFAULT_CAPS}}}
case ",$caps," in *,"$cap",*) ;; *) exit 0 ;; esac
case $mode in
  decide|event)
    out=$('{exe}' harness hook "$ev" "$cap" 2>/dev/null) || exit 0
    [ -n "$out" ] && printf '%s' "$out" ;;
  statusline)
    in=$(cat)                                        # read stdin ONCE
    printf '%s' "$in" | '{exe}' harness statusline 2>/dev/null
    ;;
esac
exit 0
"#
    )
}

/// The `hooks` fragment `install` merges into the user's settings, pointing
/// at the absolute `bridge` path (a settings hook has no
/// `${{CLAUDE_PLUGIN_ROOT}}`).
#[must_use]
pub fn settings_hooks(bridge: &Path) -> Value {
    let mut events = Map::new();
    for (event, mode, cap) in HOOK_ROWS {
        let mut entry = Map::new();
        entry.insert("type".to_owned(), Value::from("command".to_owned()));
        entry.insert(
            "command".to_owned(),
            Value::from(format!(
                "sh '{}' {mode} {event} {cap} {OWN_MARK}",
                bridge.display()
            )),
        );
        let mut group = Map::new();
        group.insert("hooks".to_owned(), Value::Array(vec![Value::Object(entry)]));
        let slot = events
            .entry((*event).to_owned())
            .or_insert_with(|| Value::Array(Vec::new()));
        if let Value::Array(list) = slot {
            list.push(Value::Object(group));
        }
    }
    Value::Object(events)
}

/// The `statusLine` fragment (design §5.2 source (a)).
#[must_use]
pub fn settings_statusline(bridge: &Path) -> Value {
    let mut o = Map::new();
    o.insert("type".to_owned(), Value::from("command".to_owned()));
    o.insert(
        "command".to_owned(),
        Value::from(format!(
            "sh '{}' statusline StatusLine usage-hud {OWN_MARK}",
            bridge.display()
        )),
    );
    o.insert("refreshInterval".to_owned(), Value::from(60u64));
    Value::Object(o)
}

/// Whether a command string is one this module wrote.
#[must_use]
pub fn is_ours(command: &str) -> bool {
    command.contains(OWN_MARK)
}

// ---------------------------------------------------------------------------
// The settings merge
// ---------------------------------------------------------------------------

/// Remove every hook entry this module wrote, under EVERY event — not only
/// the ones this build registers, so an entry an older build left behind does
/// not linger — and drop a group left empty by that removal, and an event
/// left with no groups.
///
/// # Errors
///
/// `doc` is not an object, or its `hooks` is not the shape the vendor
/// documents. Nothing is changed on an error: the caller refuses rather than
/// half-merging.
pub fn strip_ours(doc: &mut Value) -> Result<(), String> {
    let Value::Object(root) = doc else {
        return Err("the settings file is not a JSON object".to_string());
    };
    let Some(hooks) = root.get_mut("hooks") else {
        return Ok(());
    };
    let Value::Object(events) = hooks else {
        return Err("`hooks` is not a JSON object".to_string());
    };
    for (event, groups) in events.iter_mut() {
        let Value::Array(groups) = groups else {
            return Err(format!("`hooks.{event}` is not a JSON array"));
        };
        for group in groups.iter_mut() {
            if let Value::Object(g) = group
                && let Some(Value::Array(entries)) = g.get_mut("hooks")
            {
                entries.retain(|e| {
                    !e.get("command")
                        .and_then(Value::as_str)
                        .is_some_and(is_ours)
                });
            }
        }
        groups.retain(|g| {
            g.get("hooks")
                .and_then(Value::as_array)
                .is_none_or(|entries| !entries.is_empty())
        });
    }
    events.retain(|_, groups| !groups.as_array().is_some_and(Vec::is_empty));
    if events.is_empty() {
        root.remove("hooks");
    }
    Ok(())
}

/// Merge our hooks and statusLine into `doc`, keeping every other key and
/// every foreign hook entry.
///
/// PRESERVATION, stated exactly: every KEY and every VALUE the file held is
/// kept. Key ORDER and whitespace are not — the writer sorts keys and emits
/// compact JSON — which is why [`install_files`] keeps the original bytes and
/// [`uninstall_files`] puts them back when the document is otherwise
/// unchanged.
///
/// # Errors
///
/// As [`strip_ours`].
pub fn merge_ours(doc: &mut Value, bridge: &Path) -> Result<(), String> {
    strip_ours(doc)?;
    let Value::Object(root) = doc else {
        return Err("the settings file is not a JSON object".to_string());
    };
    let ours = settings_hooks(bridge);
    let Value::Object(our_events) = ours else {
        return Ok(());
    };
    let hooks = root
        .entry("hooks".to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    let Value::Object(events) = hooks else {
        return Err("`hooks` is not a JSON object".to_string());
    };
    for (event, groups) in our_events {
        let slot = events
            .entry(event.clone())
            .or_insert_with(|| Value::Array(Vec::new()));
        let Value::Array(slot) = slot else {
            return Err(format!("`hooks.{event}` is not a JSON array"));
        };
        if let Value::Array(ours) = groups {
            slot.extend(ours);
        }
    }
    root.insert("statusLine".to_owned(), settings_statusline(bridge));
    Ok(())
}

/// The user's own `statusLine.command`, when they have one that is not ours.
#[must_use]
pub fn user_statusline(doc: &Value) -> Option<String> {
    let cmd = doc
        .get("statusLine")
        .and_then(|s| s.get("command"))
        .and_then(Value::as_str)?;
    if is_ours(cmd) {
        None
    } else {
        Some(cmd.to_owned())
    }
}

/// Render a settings document the way this module writes one.
#[must_use]
pub fn render(doc: &Value) -> String {
    format!("{}\n", aterm_json::to_string(doc).unwrap_or_default())
}

// ---------------------------------------------------------------------------
// I/O: the shell around the pure half
// ---------------------------------------------------------------------------

/// Create `dir` and everything above it, 0700 on Unix.
fn ensure_dir(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(dir)?.permissions();
        perms.set_mode(0o700);
        std::fs::set_permissions(dir, perms)?;
    }
    Ok(())
}

/// Write `text` to `path` through a temporary file in the same directory, so
/// a reader never sees a half-written document.
fn write_atomic(path: &Path, text: &str, mode: u32) -> io::Result<()> {
    write_atomic_if(path, text, mode, || true).map(|_| ())
}

/// How many times `install`/`uninstall` redo read → merge → write when the
/// settings file changed under them, before refusing
/// ([`write_atomic_if`]).
pub const SETTINGS_TRIES: usize = 3;

/// Whether `path` still holds exactly `original` (`None`: it was absent or
/// unreadable when the merge read it).
fn settings_unchanged(path: &Path, original: Option<&str>) -> bool {
    std::fs::read_to_string(path).ok().as_deref() == original
}

/// [`write_atomic`], with `still` asked IMMEDIATELY before the rename: when it
/// answers false the temporary file is removed, `path` is left untouched and
/// the answer is `Ok(false)`.
///
/// This is the settings writers' rule (docs/DESIGN-aterm-wrapper-2026-09-17.md
/// §6.1): two installers write `~/.claude/settings.json` — this one and the
/// aterm-link block the GUI's primer pass installs by default — and each
/// strips only its own entries. A merge computed from bytes another writer
/// has since replaced would put the OLD document back and silently drop the
/// other writer's block; the harness's own block is one nothing reinstalls.
/// So the file is re-read just before the rename and compared with what the
/// merge read, and a change sends the caller back to read and merge again.
fn write_atomic_if(
    path: &Path,
    text: &str,
    mode: u32,
    still: impl FnOnce() -> bool,
) -> io::Result<bool> {
    let dir = path.parent().unwrap_or(Path::new("."));
    ensure_dir(dir)?;
    let tmp = dir.join(format!(
        ".{}.tmp",
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "harness".to_string())
    ));
    std::fs::write(&tmp, text.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    let _ = mode;
    if !still() {
        let _ = std::fs::remove_file(&tmp);
        return Ok(false);
    }
    std::fs::rename(&tmp, path)?;
    Ok(true)
}

/// The file a settings path NAMES, through any chain of symlinks.
///
/// A `rename` onto a symlink replaces the LINK, not its target: an operator
/// whose `~/.claude/settings.json` points into a dotfiles checkout would get a
/// regular file where the link was. Followed by hand rather than by
/// `canonicalize`, which fails on a link whose target does not exist yet —
/// and a settings link planted ahead of the file is exactly the case a fresh
/// write should honour. Bounded so a cycle answers something.
/// (The shape is `aterm_link::hook::link_target`; that crate is not in this
/// crate's graph, so this is the same discipline, not a call.)
#[must_use]
pub fn link_target(path: &Path) -> PathBuf {
    let mut cur = path.to_path_buf();
    for _ in 0..32 {
        let Ok(next) = std::fs::read_link(&cur) else {
            break;
        };
        cur = if next.is_absolute() {
            next
        } else {
            cur.parent().map_or(next.clone(), |dir| dir.join(&next))
        };
    }
    cur
}

/// Open one ring, creating the ledger directory if it is missing.
fn open_ring(env: &Env, name: &str) -> io::Result<Ring> {
    let dir = env.ledger_dir();
    ensure_dir(&dir)?;
    let cfg = ring_config(name).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, format!("no ledger {name:?}"))
    })?;
    Ring::open(&dir, name, cfg)
}

/// Append one row, and answer the id it got. A ledger that cannot be opened
/// is NOT an error on the hook path: the decision still stands and the caller
/// still exits 0. Journal-before-act (design §4.3) is honoured by appending
/// the intent row before the decision reaches stdout; when the journal itself
/// is unavailable the decision is still printed, because refusing to answer a
/// hook is the outcome that changes the vendor's behaviour.
fn append_row(env: &Env, name: &str, row: &str) -> io::Result<u64> {
    let mut ring = open_ring(env, name)?;
    ring.append(row)
}

/// [`append_row`] for a caller that holds the STATE DIRECTORY rather than an
/// [`Env`] — the watch loop's `journal` seam ([`super::wire::CtlWire`]).
///
/// It exists so there is ONE place a ledger directory is made and moded
/// (0700 through [`ensure_dir`]); the loop used to have no writer at all, and
/// a second one that forgot the mode would have left the actuation record
/// world-readable on a shared machine.
///
/// # Errors
///
/// The ledger directory could not be made, the name answers to no ring, or
/// the ring refused the append.
pub(super) fn append_to(state: &Path, name: &str, row: &str) -> io::Result<u64> {
    let dir = state.join("ledger");
    ensure_dir(&dir)?;
    let cfg = ring_config(name).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, format!("no ledger {name:?}"))
    })?;
    Ring::open(&dir, name, cfg)?.append(row)
}

/// Which rings hold at least one row, and the newest id across them.
///
/// The three booleans [`Readout`] computes its `source` and `hooks_present`
/// from are all `rows > 0`, and answering them with [`Ring::len`] read every
/// byte of every segment: MEASURED 2026-09-22, `aterm harness mark --json`
/// took 6.66 ms against a tiny ledger and 55.48 ms against a full 64 MiB rm
/// ring — 72 MiB of reads for three booleans. [`Ring::has_rows`] answers each
/// from the ids already in memory after the open.
fn read_presence(env: &Env) -> (BTreeMap<String, bool>, u64) {
    let mut present = BTreeMap::new();
    let mut seq = 0;
    for name in RINGS {
        let (has, next) = open_ring(env, name).map_or((false, 0), |r| {
            (r.has_rows(), r.next_id().saturating_sub(1))
        });
        present.insert((*name).to_string(), has);
        seq = seq.max(next);
    }
    (present, seq)
}

/// Row counts for every ring, and the newest id across them.
fn read_counts(env: &Env) -> (BTreeMap<String, u64>, u64) {
    let mut rows = BTreeMap::new();
    let mut seq = 0;
    for name in RINGS {
        let (count, next) = open_ring(env, name).map_or((0, 0), |r| {
            (r.len().unwrap_or(0), r.next_id().saturating_sub(1))
        });
        rows.insert((*name).to_string(), count);
        seq = seq.max(next);
    }
    (rows, seq)
}

/// The newest `max` rows of one ring, oldest first.
fn tail(env: &Env, name: &str, since: u64, max: usize) -> Vec<Entry> {
    let Ok(ring) = open_ring(env, name) else {
        return Vec::new();
    };
    // `read_since` walks OLDEST first, so the newest rows are reached by
    // moving the cursor up rather than by reading the whole ledger and
    // dropping most of it.
    let want = max.min(READ_MAX_ROWS);
    let newest = ring.next_id().saturating_sub(1);
    let from = since.max(newest.saturating_sub(u64::try_from(want).unwrap_or(u64::MAX)));
    ring.read_since(from, want).unwrap_or_default()
}

/// Read stdin, bounded. A read error or non-UTF-8 input answers `""`, which
/// every caller treats as a malformed payload — the safe answer.
fn read_stdin() -> String {
    let mut buf = Vec::new();
    let _ = io::stdin()
        .lock()
        .take(MAX_STDIN_BYTES as u64 + 1)
        .read_to_end(&mut buf);
    if buf.len() > MAX_STDIN_BYTES {
        return String::new();
    }
    String::from_utf8(buf).unwrap_or_default()
}

/// `aterm harness hook <EVENT> [<cap>]`. ALWAYS 0.
fn run_hook(event: &str, cap: Option<&str>, env: &Env, out: &mut dyn Write) -> ExitCode {
    let payload = read_stdin();
    // ONE open of the rm ring, for both the id and the append. Two opens
    // meant two end-to-end scans of the active segment (`adopt_active`
    // recovers `next_id` and `tail_dirty` by reading it): MEASURED
    // 2026-09-22, 8.74 ms per hook at an empty ring against 16.41 ms at
    // 6.27 MiB, a slope of 1.22 ms per MiB — half of it the second scan. The
    // open also takes the ring's exclusive advisory lock, so opening once
    // halves the window in which a second session's hook is refused
    // `ResourceBusy`.
    let mut rm = open_ring(env, RING_RM).ok();
    let id = rm.as_ref().map_or(0, Ring::next_id);
    let reply = hook_reply(event, &payload, cap, env, id);
    // Journal first, then answer (design §4.3).
    if let (Some(row), Some(ring)) = (&reply.rm_row, rm.as_mut()) {
        let _ = ring.append(row);
    }
    // Release the lock before the other rings are touched.
    drop(rm);
    if let Some(row) = &reply.event_row {
        let _ = append_row(env, RING_EVENT, row);
    }
    if let Some(row) = &reply.statusline_row {
        let _ = append_row(env, RING_STATUSLINE, row);
    }
    if !reply.stdout.is_empty() {
        let _ = out.write_all(reply.stdout.as_bytes());
    }
    let _ = out.flush();
    ExitCode::SUCCESS
}

/// `aterm harness statusline`. ALWAYS 0, and always prints exactly one line,
/// because the vendor renders whatever this says in its footer.
fn run_statusline(env: &Env, out: &mut dyn Write) -> ExitCode {
    let payload = read_stdin();
    let _ = append_row(env, RING_STATUSLINE, payload.trim_end_matches('\n'));
    // Chain to the user's own statusLine when `install` found one, exactly as
    // design §4.5's `statusline` branch does: their line wins the slot, and
    // the harness's numbers stay readable through `harness usage`.
    if let Ok(user) = std::fs::read_to_string(env.statusline_user()) {
        let user = user.trim();
        if !user.is_empty()
            && let Some(text) = chain_statusline(user, &payload)
        {
            let _ = out.write_all(text.as_bytes());
            let _ = out.flush();
            return ExitCode::SUCCESS;
        }
    }
    let (line, _) = hud_from_statusline(&payload, env.now, env.utc_offset_s);
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
    ExitCode::SUCCESS
}

/// Run the user's own statusLine command with `payload` on its stdin and
/// answer its stdout. `None` when it could not be run or said nothing, in
/// which case the caller prints the harness's own line rather than a blank
/// footer.
///
/// BOUNDED, and folded to ONE line, because [`run_statusline`]'s contract is
/// "always 0, always exactly one line" and a chained command is arbitrary
/// user code: it may loop, it may stat an NFS mount, it may print megabytes,
/// and a deadline-free `wait_with_output` hands every one of those to the
/// vendor's renderer. [`STATUSLINE_BUDGET`] is the deadline and
/// [`STATUSLINE_MAX_STDOUT`] the cap; the shipped bounded runner
/// ([`super::align::capture_bounded`]) is what enforces both.
///
/// A non-zero EXIT is not a refusal here: the exit code is not part of the
/// vendor's statusLine contract, so a command that prints its line and exits
/// 1 keeps its line. Silence, a crash before any output, and a deadline are
/// all `None`.
fn chain_statusline(command: &str, payload: &str) -> Option<String> {
    let mut cmd = std::process::Command::new("/bin/sh");
    cmd.arg("-c").arg(command);
    let got = super::align::capture_bounded(
        cmd,
        STATUSLINE_BUDGET,
        Some(payload.as_bytes().to_vec()),
        STATUSLINE_MAX_STDOUT,
    )
    .ok()?;
    let text = String::from_utf8_lossy(&got.stdout).into_owned();
    let line = statusline_one_line(&text);
    if line.is_empty() { None } else { Some(line) }
}

/// The deadline on a chained user statusLine. TARGET: the vendor re-renders
/// its footer often, so a command that has not answered in this long is one
/// the footer is better off without.
pub const STATUSLINE_BUDGET: std::time::Duration = std::time::Duration::from_secs(2);

/// The byte cap on a chained statusLine's stdout, before the one-line fold.
pub const STATUSLINE_MAX_STDOUT: usize = 64 * 1024;

/// The byte cap on the line handed to the vendor.
pub const STATUSLINE_LINE_CAP: usize = usage::HUD_MAX_BYTES;

/// Fold arbitrary stdout into the ONE line this command promises: the first
/// non-empty line, line-breaking characters folded to spaces, capped on a
/// character boundary, with the newline the caller needs. Empty when there is
/// nothing to show.
///
/// Not [`super::one_line`]: a statusline is COLUMNAR, and that helper collapses
/// runs of spaces and trims the front, which would pull a padded line apart.
/// What is shared is the part that was wrong here — the question "does this
/// character break a line". It asked `char::is_control()`, which is Cc-only,
/// so `U+2028` in the vendor's stdout reached a surface promising one line.
fn statusline_one_line(text: &str) -> String {
    let Some(first) = text.lines().find(|l| !l.trim().is_empty()) else {
        return String::new();
    };
    let folded: String = super::truncate_bytes(first.trim_end(), STATUSLINE_LINE_CAP)
        .chars()
        .map(|c| if breaks_a_line(c) { ' ' } else { c })
        .collect();
    let folded = folded.trim_end().to_owned();
    if folded.is_empty() {
        String::new()
    } else {
        format!("{folded}\n")
    }
}

/// What `install` would write, as (path, contents, mode) rows.
#[must_use]
pub fn install_files(env: &Env, exe: &str) -> Vec<(PathBuf, String, u32)> {
    let plugin = env.plugin_dir();
    vec![
        (
            plugin.join(".claude-plugin").join("plugin.json"),
            plugin_manifest(),
            0o600,
        ),
        (plugin.join("hooks").join("hooks.json"), hooks_json(), 0o600),
        (env.bridge_path(), bridge_sh(exe), 0o700),
    ]
}

/// `aterm harness install`.
///
/// The settings file is read, merged and written up to [`SETTINGS_TRIES`]
/// times: when another writer (the GUI primer's aterm-link block, an editor)
/// replaced it between the read and the rename, the merge is redone from the
/// new bytes rather than writing the stale document over theirs
/// ([`write_atomic_if`]); after the last try the install refuses, naming the
/// file.
fn run_install(env: &Env, dry_run: bool, out: &mut dyn Write, err: &mut dyn Write) -> ExitCode {
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "aterm".to_string());
    let files = install_files(env, &exe);
    let settings = link_target(&env.settings);
    for _ in 0..SETTINGS_TRIES {
        let original = std::fs::read_to_string(&settings).ok();
        let mut doc = match &original {
            Some(text) if !text.trim().is_empty() => match aterm_json::from_str::<Value>(text) {
                Ok(v) => v,
                Err(e) => {
                    let _ = writeln!(
                        err,
                        "aterm harness install: {} is not JSON ({e}); nothing was written",
                        settings.display()
                    );
                    return ExitCode::from(1);
                }
            },
            _ => Value::Object(Map::new()),
        };
        let chained = user_statusline(&doc);
        if let Err(e) = merge_ours(&mut doc, &env.bridge_path()) {
            let _ = writeln!(
                err,
                "aterm harness install: {}: {e}; nothing was written",
                settings.display()
            );
            return ExitCode::from(1);
        }
        let rendered = render(&doc);
        if dry_run {
            for (path, _, _) in &files {
                let _ = writeln!(out, "would write {}", path.display());
            }
            if let Some(cmd) = &chained {
                let _ = writeln!(
                    out,
                    "would chain your statusLine: {}",
                    truncate_bytes(cmd, 200)
                );
            }
            let _ = writeln!(out, "would merge into {}:", settings.display());
            let _ = out.write_all(rendered.as_bytes());
            return ExitCode::SUCCESS;
        }
        if let Err(e) = ensure_dir(&env.state) {
            let _ = writeln!(err, "aterm harness install: {}: {e}", env.state.display());
            return ExitCode::FAILURE;
        }
        for (path, text, mode) in &files {
            if let Err(e) = write_atomic(path, text, *mode) {
                let _ = writeln!(err, "aterm harness install: {}: {e}", path.display());
                return ExitCode::FAILURE;
            }
        }
        // The user's own statusLine, copied verbatim BEFORE ours takes the slot.
        if let Some(cmd) = &chained
            && let Err(e) = write_atomic(&env.statusline_user(), &format!("{cmd}\n"), 0o600)
        {
            let _ = writeln!(
                err,
                "aterm harness install: {}: {e}",
                env.statusline_user().display()
            );
            return ExitCode::FAILURE;
        }
        // The original bytes, kept so `uninstall` can put them back exactly.
        if let Some(text) = &original {
            let backup = env.settings_backup();
            if !backup.exists()
                && let Err(e) = write_atomic(&backup, text, 0o600)
            {
                let _ = writeln!(err, "aterm harness install: {}: {e}", backup.display());
                return ExitCode::FAILURE;
            }
        }
        match write_atomic_if(&settings, &rendered, 0o600, || {
            settings_unchanged(&settings, original.as_deref())
        }) {
            Ok(true) => {
                let _ = writeln!(
                    out,
                    "installed {HARNESS} abi={ABI}: {} hooks and the statusLine in {}",
                    HOOK_ROWS.len(),
                    settings.display()
                );
                let _ = writeln!(
                    out,
                    "hooks are ENRICHMENT: `aterm harness status|usage|limits|ledger` answer \
                     without them"
                );
                return ExitCode::SUCCESS;
            }
            // Another writer replaced the file under this merge: read again.
            Ok(false) => {}
            Err(e) => {
                let _ = writeln!(err, "aterm harness install: {}: {e}", settings.display());
                return ExitCode::FAILURE;
            }
        }
    }
    let _ = writeln!(
        err,
        "aterm harness install: {} changed under every one of {SETTINGS_TRIES} tries (another \
         writer is replacing it); the harness block was NOT written — run install again",
        settings.display()
    );
    ExitCode::FAILURE
}

/// `aterm harness uninstall`.
///
/// Read, strip and write up to [`SETTINGS_TRIES`] times, as
/// [`run_install`]: a document another writer replaced under the strip is
/// stripped again from the new bytes, never overwritten with the old ones.
fn run_uninstall(
    env: &Env,
    dry_run: bool,
    purge: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let settings = link_target(&env.settings);
    let backup = env.settings_backup();
    let mut restored = None;
    for _ in 0..SETTINGS_TRIES {
        let original = std::fs::read_to_string(&settings).ok();
        let mut doc = match &original {
            Some(text) if !text.trim().is_empty() => match aterm_json::from_str::<Value>(text) {
                Ok(v) => v,
                Err(e) => {
                    let _ = writeln!(
                        err,
                        "aterm harness uninstall: {} is not JSON ({e}); nothing was written",
                        settings.display()
                    );
                    return ExitCode::from(1);
                }
            },
            _ => Value::Object(Map::new()),
        };
        if let Err(e) = strip_ours(&mut doc) {
            let _ = writeln!(
                err,
                "aterm harness uninstall: {}: {e}; nothing was written",
                settings.display()
            );
            return ExitCode::from(1);
        }
        // Put the user's own statusLine back, or drop ours.
        if let Value::Object(root) = &mut doc {
            let ours = root
                .get("statusLine")
                .and_then(|s| s.get("command"))
                .and_then(Value::as_str)
                .is_some_and(is_ours);
            if ours {
                match std::fs::read_to_string(env.statusline_user()) {
                    Ok(cmd) if !cmd.trim().is_empty() => {
                        let mut o = Map::new();
                        o.insert("type".to_owned(), Value::from("command".to_owned()));
                        o.insert(
                            "command".to_owned(),
                            Value::from(cmd.trim_end_matches('\n').to_owned()),
                        );
                        root.insert("statusLine".to_owned(), Value::Object(o));
                    }
                    _ => {
                        root.remove("statusLine");
                    }
                }
            }
        }
        // Byte-identity: when what is left is the SAME DOCUMENT the backup
        // holds, the original bytes go back verbatim — key order, whitespace
        // and all — so an install/uninstall round trip leaves the file
        // unchanged. When the owner has edited it since, the pruned render is
        // written instead and the backup is kept, because their edit is worth
        // more than our formatting.
        let backup_text = std::fs::read_to_string(&backup).ok();
        let restore = backup_text.as_ref().and_then(|text| {
            aterm_json::from_str::<Value>(text)
                .ok()
                .filter(|orig| *orig == doc)
                .map(|_| text.clone())
        });
        let rendered = restore.clone().unwrap_or_else(|| render(&doc));
        if dry_run {
            let _ = writeln!(
                out,
                "would write {} ({})",
                settings.display(),
                if restore.is_some() {
                    "the original bytes"
                } else {
                    "a pruned render; the settings changed since install"
                }
            );
            let _ = out.write_all(rendered.as_bytes());
            let _ = writeln!(out, "would remove {}", env.plugin_dir().display());
            if purge {
                let _ = writeln!(out, "would remove {}", env.ledger_dir().display());
            }
            return ExitCode::SUCCESS;
        }
        if original.is_none() && !settings.exists() {
            restored = Some(restore.is_some());
            break;
        }
        match write_atomic_if(&settings, &rendered, 0o600, || {
            settings_unchanged(&settings, original.as_deref())
        }) {
            Ok(true) => {
                restored = Some(restore.is_some());
                break;
            }
            // Another writer replaced the file under this strip: read again.
            Ok(false) => {}
            Err(e) => {
                let _ = writeln!(err, "aterm harness uninstall: {}: {e}", settings.display());
                return ExitCode::FAILURE;
            }
        }
    }
    let Some(restored) = restored else {
        let _ = writeln!(
            err,
            "aterm harness uninstall: {} changed under every one of {SETTINGS_TRIES} tries \
             (another writer is replacing it); nothing was removed — run uninstall again",
            settings.display()
        );
        return ExitCode::FAILURE;
    };
    if restored {
        let _ = std::fs::remove_file(&backup);
    }
    let _ = std::fs::remove_file(env.statusline_user());
    let _ = std::fs::remove_dir_all(env.plugin_dir());
    if purge {
        let _ = std::fs::remove_dir_all(env.ledger_dir());
    }
    let _ = writeln!(
        out,
        "uninstalled {HARNESS} from {}{}",
        settings.display(),
        if purge {
            "; the ledgers were removed"
        } else {
            "; the ledgers were kept (--purge removes them)"
        }
    );
    ExitCode::SUCCESS
}

/// `aterm harness status`.
fn run_status(env: &Env, json: bool, spine: bool, out: &mut dyn Write) -> ExitCode {
    let (rows, seq) = read_counts(env);
    let read = Readout::new(rows, seq);
    let presence = presence_of(&read, env, spine);
    if json {
        let mut doc =
            aterm_json::from_str::<Value>(&status_json(&read, env)).unwrap_or(Value::Null);
        if let Value::Object(o) = &mut doc {
            o.insert("presence".to_owned(), presence.json());
            // The ASSERTION rides beside the mark it produced. This command
            // opens no socket and cannot check it, so a reader that sees
            // `armed` can also see what it rests on (§4.6, F9).
            o.insert("spine_asserted".to_owned(), Value::from(spine));
        }
        let _ = writeln!(out, "{}", aterm_json::to_string(&doc).unwrap_or_default());
    } else {
        // ONE line: `status` is read by machines. The assertion is a field
        // on it, not a second line.
        let _ = writeln!(
            out,
            "{} {} spine={}",
            status_line(&read, env),
            presence.status_fields(),
            if spine { "asserted" } else { "unread" }
        );
    }
    ExitCode::SUCCESS
}

/// The one sentence both mark-printing commands owe about the spine.
///
/// This command opens no socket, so `--assume-spine` is an ASSERTION it
/// cannot check. §4.6 forbids `armed` on an assumption; what keeps that true
/// here is that the assumption is printed beside the mark, so a reader can
/// see the mark rests on the caller's word rather than on a watch that
/// answered.
#[must_use]
pub fn spine_note(spine: bool) -> &'static str {
    if spine {
        "--assume-spine: this command holds no watch on the grid spine and cannot check one, so \
         the mark above rests on the caller's assertion, not on a watch that answered"
    } else {
        "this command holds no watch on the grid spine, so it cannot confirm one; a host that \
         does asserts it with --assume-spine"
    }
}

/// `aterm harness mark`.
fn run_mark(env: &Env, json: bool, commands: bool, spine: bool, out: &mut dyn Write) -> ExitCode {
    // The mark needs the three booleans, never the counts, and this verb is
    // the one design §4.6.1 expects a host to re-assert on every change.
    let (present, seq) = read_presence(env);
    let presence = presence_of(&Readout::from_presence(&present, seq), env, spine);
    if commands {
        // The three CLI-side surfaces of design §4.6.1, as lines a host pipes
        // into `aterm ctl`. This command opens no socket: the module law is
        // that nothing in this tree talks to one.
        let _ = writeln!(out, "{}", presence.meta_set_icon());
        let _ = writeln!(out, "{}", presence.meta_set_description());
        let _ = writeln!(out, "{}", presence.appnotice());
        return ExitCode::SUCCESS;
    }
    if json {
        let _ = writeln!(
            out,
            "{}",
            aterm_json::to_string(&presence.json()).unwrap_or_default()
        );
        return ExitCode::SUCCESS;
    }
    let _ = writeln!(out, "OK schema=1 {}", presence.status_fields());
    let _ = writeln!(out, "{} {}", presence.mark.glyph(), presence.sentence());
    let _ = writeln!(out, "{}", spine_note(spine));
    ExitCode::SUCCESS
}

/// `aterm harness enable|disable [cap=<c> …]`.
///
/// The MASTER switch is deliberately not writable here. It lives in
/// `aterm.toml` and not in the harness's own state precisely so it still
/// works when the harness tree is missing, broken or mid-update (design
/// §4.6.2: *"the kill switch may never depend on the thing it kills"*), and
/// two homes for one key would drift. So these verbs own capability state and
/// NAME the master switch's home in their own output.
fn run_switch(
    env: &Env,
    names: &[String],
    enable: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let declared = caps();
    let current = mark::read_disabled(&env.state);
    let verdict = mark::switch(&current, &declared, names, enable);
    if !verdict.unknown.is_empty() {
        let _ = writeln!(
            err,
            "aterm harness: no capability called {}; the capabilities are {}",
            verdict.unknown.join(", "),
            declared.join(", ")
        );
        return ExitCode::from(1);
    }
    if verdict.changed
        && let Err(e) = mark::write_disabled(&env.state, &verdict.disabled)
    {
        let _ = writeln!(
            err,
            "aterm harness: the capability switches could not be written to {}: {e}",
            mark::caps_path(&env.state).display()
        );
        return ExitCode::from(1);
    }
    let live: Vec<&str> = declared
        .iter()
        .copied()
        .filter(|c| !verdict.disabled.contains(*c))
        .collect();
    let _ = writeln!(
        out,
        "{} {} · live: {} · off: {}",
        if enable { "enabled" } else { "disabled" },
        if names.is_empty() {
            "every capability".to_owned()
        } else {
            names.join(", ")
        },
        if live.is_empty() {
            "-".to_owned()
        } else {
            live.join(", ")
        },
        if verdict.disabled.is_empty() {
            "-".to_owned()
        } else {
            verdict
                .disabled
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        },
    );
    if !verdict.changed {
        let _ = writeln!(out, "nothing changed — the switches already said so");
    }
    let _ = writeln!(
        out,
        "the MASTER switch is not here: it is `harness.enabled` in aterm.toml \
         (`aterm ctl settings set harness.enabled false`), so it keeps working when this \
         harness does not. $ATERM_NO_HARNESS bypasses one session."
    );
    ExitCode::SUCCESS
}

/// `aterm harness usage`.
///
/// TWO sources, in authority order, and the second one is why this command
/// still answers with ZERO hooks (design §0.2, §5.2): the statusLine ring is
/// the vendor's own figures and arrives on a hook `--bare` removes; the
/// session's TRANSCRIPT is a file on disk that no vendor flag takes away.
/// Windows (percent used, reset instants) exist only in the first; SPEND
/// (tokens per model) exists only in the second, so the transcript is folded
/// WHETHER OR NOT a statusLine sample is there and `source=` still names the
/// higher authority. A run with neither prints the empty view and says which
/// source was missing — never a row of zeros labelled as if something had
/// reported them.
///
/// WHICH transcript is its own fact, printed beside the path and carried as
/// `transcript_pick` in the JSON: a session a payload NAMED, or the newest
/// file in the project directory when nothing named one. The second answers
/// `source=transcript-newest`, because two Claude Code sessions in one
/// working directory share a project directory and "newest" is then the more
/// recently active of the two ([`fold_session_transcript`]).
fn run_usage(env: &Env, json: bool, out: &mut dyn Write) -> ExitCode {
    let newest = tail(env, RING_STATUSLINE, 0, 1).pop();
    let (mut source, view) = match &newest {
        Some(entry) => {
            let (_, view) = hud_from_statusline(&entry.line, env.now, env.utc_offset_s);
            (Source::StatusLine, view)
        }
        None => (Source::Grid, None),
    };
    let mut view = view.unwrap_or_else(|| UsageView::new(env.now));
    let folded = fold_session_transcript(env);
    if let Some((_, fold, pick)) = &folded {
        // SPEND lives only in the transcript, so it is folded whether or not
        // a statusLine sample exists; `source` still names the highest
        // AUTHORITY, which is the statusLine when one is there.
        if view.accounts.is_empty() {
            view.accounts.push(AccountView::new("account", true));
        }
        if let Some(account) = view.accounts.iter_mut().find(|a| a.active) {
            account.add_transcript(fold, &usage::PriceTable::new());
        }
        if source == Source::Grid {
            source = pick.source();
        }
    }
    if json {
        let extra = match &folded {
            Some((path, _, pick)) => vec![
                ("transcript", Value::from(path.display().to_string())),
                ("transcript_pick", Value::from(pick.as_str().to_owned())),
            ],
            None => Vec::new(),
        };
        let _ = writeln!(
            out,
            "{}",
            sourced_json("usage", source, &usage::usage_json(&view), extra)
        );
    } else {
        let line = usage::hud_line(&view, env.utc_offset_s);
        let _ = writeln!(out, "{line}  [source={}]", source.as_str());
        match (&folded, source) {
            (Some((path, fold, pick)), _) => {
                let _ = writeln!(
                    out,
                    "SPEND folded from {} ({} assistant rows), picked by {}.{}",
                    path.display(),
                    fold.assistant_rows,
                    pick.as_str(),
                    match pick {
                        TranscriptPick::Newest =>
                            " NOTHING named a session, so this is the newest transcript in this \
                             project directory: if two Claude Code sessions share this working \
                             directory it may be the other one's.",
                        _ => "",
                    }
                );
                if source != Source::StatusLine {
                    let _ = writeln!(
                        out,
                        "no statusLine sample yet, so no live windows — they arrive with the \
                         vendor's statusLine hook."
                    );
                }
            }
            (None, Source::Grid) => {
                let _ = writeln!(
                    out,
                    "no statusLine sample and no transcript for this directory — the vendor's \
                     windows arrive through a hook, and its transcript is what answers without \
                     one"
                );
            }
            (None, _) => {}
        }
    }
    ExitCode::SUCCESS
}

/// The auth evidence THIS process has, by account label (design §5.8.10).
///
/// Exactly one account's live state is observable from here — the one the
/// session is running under — and it is observable only because the limit
/// classifier already reads the banners and the hook rows. Every other
/// account is `Unknown` until something observes it, and the roster prints
/// that word rather than a guess. A `.claude.json` with no `oauthAccount`
/// fills in `unauthenticated` later, inside `AccountState::read_dir`, which
/// never overwrites what this function said.
fn observed_auth(env: &Env, roster: &accounts::Roster) -> BTreeMap<String, AuthState> {
    let mut out = BTreeMap::new();
    let (evidence, _) = gathered_evidence(env);
    if limits::classify(&evidence, env.now).is_some_and(|c| c.class == limits::Class::Auth)
        && let Some(dir) = env.active_config_dir()
        && let Some(row) = roster.row_for_dir(&dir)
    {
        out.insert(row.label.clone(), AuthState::LoginExpired);
    }
    out
}

/// `aterm harness accounts [discover] [--write]` (design §5.6, §5.7).
///
/// READS a roster and two non-secret facts per config directory; reads no
/// credential, no keychain item and no token, and says which account a
/// `switch-account` would rotate into and WHY every other one is out.
/// `discover` proposes rows and writes nothing without `--write`.
fn run_accounts(
    env: &Env,
    add: Option<&(String, PathBuf)>,
    discover: bool,
    write: bool,
    json: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let path = env.accounts_path();
    let roster = match accounts::Roster::load(&path) {
        Ok(roster) => roster,
        Err(why) => {
            // Fail-closed and whole-file, exactly as the contract is: a
            // roster that does not admit rotates nowhere.
            let _ = writeln!(err, "aterm harness: {why}");
            return ExitCode::from(1);
        }
    };
    if let Some(add) = add {
        return run_accounts_add(&roster, &path, add, json, out, err);
    }
    if discover {
        return run_accounts_discover(env, &roster, &path, write, json, out, err);
    }
    let states = accounts::roster_states(
        &roster,
        env.active_config_dir().as_deref(),
        &observed_auth(env, &roster),
    );
    let chosen = accounts::select(&states).map(|s| s.row.label.clone());
    if json {
        let _ = writeln!(
            out,
            "{}",
            accounts_json(&roster, &states, chosen.as_deref(), env.now)
        );
        return ExitCode::SUCCESS;
    }
    let _ = writeln!(
        out,
        "roster {} · rotation {} · {} account(s)",
        path.display(),
        if roster.enabled { "on" } else { "OFF" },
        states.len()
    );
    for state in &states {
        let why = match accounts::candidacy(state) {
            Ok(()) if chosen.as_deref() == Some(state.row.label.as_str()) => {
                "NEXT — the §5.6 selection".to_string()
            }
            Ok(()) => "candidate".to_string(),
            Err(why) => format!("not a candidate: {}", why.as_str()),
        };
        let _ = writeln!(
            out,
            "  {:<16} {:<11} auth={:<15} 5h={:<6} 7d={:<6} age={:<8} {}",
            truncate_bytes(&state.row.label, 64),
            state.row.kind.as_str(),
            state.auth.as_str(),
            pct(state, accounts::FIVE_HOUR),
            pct(state, accounts::SEVEN_DAY),
            state
                .snapshot
                .as_ref()
                .and_then(|s| s.age_s(env.now))
                .map_or_else(|| "-".to_string(), |a| format!("{a}s")),
            why
        );
    }
    if states.is_empty() {
        let _ = writeln!(
            out,
            "no rows — `aterm harness accounts discover` lists the config directories on this \
             machine and prints the rows they would become; nothing is written without --write"
        );
    } else if !roster.enabled {
        let _ = writeln!(
            out,
            "rotation is OFF: `[accounts] enabled = true` in {} is the per-capability consent, \
             and §5.8's table still needs `cap.limits` level ≥ 4",
            path.display()
        );
    }
    let _ = writeln!(
        out,
        "every figure above is the vendor's OWN cache in each config directory, with its age; \
         this command makes no network call and reads no credential"
    );
    ExitCode::SUCCESS
}

/// One window's percent, or `-` where nothing measured it.
fn pct(state: &AccountState, name: &str) -> String {
    state
        .snapshot
        .as_ref()
        .and_then(|s| s.window(name))
        .map_or_else(|| "-".to_string(), |w| format!("{:.0}%", w.used_pct))
}

fn run_accounts_discover(
    env: &Env,
    roster: &accounts::Roster,
    path: &Path,
    write: bool,
    json: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let Some(home) = env.home.as_ref() else {
        let _ = writeln!(
            err,
            "aterm harness: $HOME is unknown, so there is nowhere to scan for config directories"
        );
        return ExitCode::from(1);
    };
    let found = accounts::discover(home, env.claude_config_dir.as_deref(), roster);
    let fresh: Vec<&accounts::Proposal> = found
        .iter()
        .filter(|p| !p.already && p.signed_in())
        .collect();
    if write && !fresh.is_empty() {
        let mut text = String::new();
        if !path.exists() {
            // A roster written from nothing starts with rotation OFF. Consent
            // is a separate, deliberate edit (design §2.3, §5.6).
            text.push_str(
                "# Written by `aterm harness accounts discover --write`.\n\
                 # Rotation is OFF until the owner turns it on, and no credential is here.\n\
                 [accounts]\nenabled = false\n",
            );
        }
        for p in &fresh {
            text.push_str(&p.to_toml());
        }
        if let Some(parent) = path.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            let _ = writeln!(err, "aterm harness: {}: {e}", parent.display());
            return ExitCode::from(1);
        }
        let appended = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut f| std::io::Write::write_all(&mut f, text.as_bytes()));
        if let Err(e) = appended {
            let _ = writeln!(err, "aterm harness: {}: {e}", path.display());
            return ExitCode::from(1);
        }
        // Re-admit what was just written: a file this command produced that
        // its own reader refuses is a bug worth failing on, here, rather than
        // at the next rotation.
        if let Err(why) = accounts::Roster::load(path) {
            let _ = writeln!(
                err,
                "aterm harness: the rows were appended to {} but the roster no longer admits — \
                 {why}",
                path.display()
            );
            return ExitCode::from(1);
        }
    }
    if json {
        let mut rows = Vec::new();
        for p in &found {
            let mut o = Map::new();
            o.insert("label".to_owned(), Value::from(p.label.clone()));
            o.insert("dir".to_owned(), Value::from(p.dir.display().to_string()));
            o.insert("already".to_owned(), Value::from(p.already));
            o.insert(
                "state".to_owned(),
                Value::from(
                    if p.signed_in() {
                        AuthState::SignedIn
                    } else {
                        AuthState::Unauthenticated
                    }
                    .as_str()
                    .to_owned(),
                ),
            );
            if let Some(snap) = &p.snapshot {
                o.insert("identity".to_owned(), identity_json(&snap.identity));
            }
            rows.push(Value::Object(o));
        }
        let mut doc = Map::new();
        doc.insert("schema".to_owned(), Value::from(1u64));
        doc.insert(
            "kind".to_owned(),
            Value::from("accounts-discover".to_owned()),
        );
        doc.insert("written".to_owned(), Value::from(write));
        doc.insert("proposed".to_owned(), Value::Array(rows));
        let _ = writeln!(
            out,
            "{}",
            aterm_json::to_string(&Value::Object(doc)).unwrap_or_default()
        );
        return ExitCode::SUCCESS;
    }
    let _ = writeln!(
        out,
        "{} config director(y|ies) found under {} — NO credential, keychain item or token was \
         read",
        found.len(),
        home.display()
    );
    for p in &found {
        let identity = p
            .snapshot
            .as_ref()
            .filter(|s| s.identity.known())
            .map(|s| {
                format!(
                    " · {}{}",
                    s.identity.org.clone().unwrap_or_else(|| "-".to_string()),
                    s.identity
                        .tier
                        .as_ref()
                        .map(|t| format!(" ({t})"))
                        .unwrap_or_default()
                )
            })
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "  {:<16} {} · {}{}{}",
            truncate_bytes(&p.label, 64),
            p.dir.display(),
            if p.signed_in() {
                AuthState::SignedIn.as_str()
            } else {
                AuthState::Unauthenticated.as_str()
            },
            identity,
            if p.already {
                " · already in the roster"
            } else {
                ""
            }
        );
    }
    if write {
        let _ = writeln!(
            out,
            "appended {} row(s) to {} with rotation left OFF",
            fresh.len(),
            path.display()
        );
    } else if fresh.is_empty() {
        let _ = writeln!(out, "nothing new to propose");
    } else {
        for p in &fresh {
            let _ = write!(out, "{}", p.to_toml());
        }
        let _ = writeln!(
            out,
            "nothing was written. `--write` appends exactly the rows above to {}, with \
             `[accounts] enabled = false`: turning rotation ON is a separate, deliberate edit",
            path.display()
        );
    }
    ExitCode::SUCCESS
}

fn identity_json(identity: &accounts::Identity) -> Value {
    let mut o = Map::new();
    o.insert(
        "org".to_owned(),
        identity
            .org
            .clone()
            .map_or(Value::Null, |o| Value::from(o.clone())),
    );
    o.insert(
        "tier".to_owned(),
        identity
            .tier
            .clone()
            .map_or(Value::Null, |t| Value::from(t.clone())),
    );
    Value::Object(o)
}

/// `harness accounts --json` (design §5.6's shape, with the `auth=` column of
/// §5.8.10 and the refusal reason beside every non-candidate).
///
/// DEVIATION from the design's sketch, stated: there is no `last_used` key.
/// Nothing records it yet, and a key that always reads `null` is worse than
/// an absent one. `dir` is printed in full rather than as the sketch's
/// `dir_hash`, because the roster is the owner's own file and the directory
/// is what a relaunch carries — a hash would name nothing the owner could act
/// on.
fn accounts_json(
    roster: &accounts::Roster,
    states: &[AccountState],
    chosen: Option<&str>,
    now: i64,
) -> String {
    let mut rows = Vec::new();
    for state in states {
        let mut o = Map::new();
        o.insert("label".to_owned(), Value::from(state.row.label.clone()));
        o.insert(
            "kind".to_owned(),
            Value::from(state.row.kind.as_str().to_owned()),
        );
        o.insert("dir".to_owned(), Value::from(state.row.dir.clone()));
        o.insert("priority".to_owned(), Value::from(state.row.priority));
        o.insert("active".to_owned(), Value::from(state.active));
        o.insert(
            "auth".to_owned(),
            Value::from(state.auth.as_str().to_owned()),
        );
        let mut windows = Map::new();
        if let Some(snap) = &state.snapshot {
            o.insert("identity".to_owned(), identity_json(&snap.identity));
            for (name, w) in &snap.windows {
                let mut win = Map::new();
                win.insert("used_pct".to_owned(), Value::from(w.used_pct));
                win.insert(
                    "resets_at".to_owned(),
                    w.resets_at.map_or(Value::Null, Value::from),
                );
                win.insert(
                    "source".to_owned(),
                    Value::from(Source::Cache.as_str().to_owned()),
                );
                win.insert(
                    "age_s".to_owned(),
                    snap.age_s(now).map_or(Value::Null, Value::from),
                );
                windows.insert(name.clone(), Value::Object(win));
            }
        }
        o.insert("windows".to_owned(), Value::Object(windows));
        o.insert(
            "candidate".to_owned(),
            Value::from(accounts::candidacy(state).is_ok()),
        );
        o.insert(
            "why_not".to_owned(),
            accounts::candidacy(state)
                .err()
                .map_or(Value::Null, |why: NotCandidate| {
                    Value::from(why.as_str().to_owned())
                }),
        );
        rows.push(Value::Object(o));
    }
    let mut doc = Map::new();
    doc.insert("schema".to_owned(), Value::from(1u64));
    doc.insert("kind".to_owned(), Value::from("accounts".to_owned()));
    doc.insert("enabled".to_owned(), Value::from(roster.enabled));
    doc.insert(
        "active".to_owned(),
        states
            .iter()
            .find(|s| s.active)
            .map_or(Value::Null, |s| Value::from(s.row.label.clone())),
    );
    doc.insert(
        "next".to_owned(),
        chosen.map_or(Value::Null, |c| Value::from(c.to_owned())),
    );
    doc.insert("accounts".to_owned(), Value::Array(rows));
    aterm_json::to_string(&Value::Object(doc)).unwrap_or_default()
}

/// The vendor's project directory name for a working directory.
///
/// MEASURED on this machine (2026-09-21): `/Users//<user>/aterm` is
/// `-Users-<user>-aterm`, and `/Users//<user>/aterm/.claude/worktrees/x` is
/// `-Users-<user>-aterm--claude-worktrees-x` — so `/` and `.` both become
/// `-`, and a leading `/` leaves a leading `-`. UNVERIFIED for characters
/// neither of those two examples contains; every non-alphanumeric that is not
/// already `-` is folded the same way, which is the rule that reproduces both
/// measurements. A name that does not exist on disk simply finds no
/// transcript, which is a `None`, not a wrong answer.
#[must_use]
pub fn project_dir_name(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// How many directory entries the transcript search will look at. A project
/// directory holds one file per session; this is the fence against a
/// directory that holds something else entirely.
pub const MAX_TRANSCRIPT_ENTRIES: usize = 4096;

/// The byte cap on the transcript this command folds. `fold_reader` streams,
/// so the cost is one pass and no line is ever held; the cap is what keeps a
/// pathological file from turning a read verb into a long job. Rows past it
/// are not folded, and the fold's own counters say how much was read.
pub const MAX_TRANSCRIPT_BYTES: u64 = 256 * 1024 * 1024;

/// The longest vendor `session_id` this reader will turn into a file name.
/// The measured ids are UUIDs (36 bytes); this is the fence, not the shape.
pub const MAX_SESSION_ID: usize = 128;

/// WHICH fact chose the transcript that was folded.
///
/// The difference matters and is printed: a transcript picked because a
/// payload NAMED its session is this session's; one picked because it is the
/// newest file in the project directory is this session's only while this
/// session is the one that wrote last. Two Claude Code sessions in one
/// working directory share one project directory, so the second case can
/// fold another session's tokens into this one's view — which is exactly what
/// it now refuses to do silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptPick {
    /// The statusLine payload's own `session_id` (or the file stem of its
    /// `transcript_path`, where only that was carried).
    StatusLineSession,
    /// A hook row's `session_id`.
    HookSession,
    /// Nothing named a session: the newest `.jsonl` by modification time.
    Newest,
}

impl TranscriptPick {
    /// The schema-1 spelling, for the `pick` field and the printed note.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            TranscriptPick::StatusLineSession => "statusline-session",
            TranscriptPick::HookSession => "hook-session",
            TranscriptPick::Newest => "newest-mtime",
        }
    }

    /// The `source` a fold chosen this way answers with.
    #[must_use]
    pub fn source(self) -> Source {
        match self {
            TranscriptPick::Newest => Source::TranscriptNewest,
            _ => Source::Transcript,
        }
    }
}

/// A `session_id` that may be turned into a file NAME inside the project
/// directory, or `None`.
///
/// This is the whole path fence: the id is joined onto a directory, so it is
/// admitted only as `[A-Za-z0-9._-]`, never empty, never leading `.` (which
/// covers `.` and `..` and every dotfile), and bounded. A payload carrying
/// `../../elsewhere` therefore names nothing and the fold falls through to
/// the newest file, rather than reading a path a hook chose. Rows are data,
/// never instructions (design §4.4).
#[must_use]
pub fn admissible_session_id(id: &str) -> Option<&str> {
    if id.is_empty() || id.len() > MAX_SESSION_ID || id.starts_with('.') {
        return None;
    }
    id.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
        .then_some(id)
}

/// The vendor session id this harness state has evidence for, and what named
/// it — the statusLine ring first (the vendor's own payload, which carries
/// `session_id` AND `transcript_path`), then the newest hook row that carries
/// one.
///
/// Neither source is the spine: both arrive on a hook. That is why the caller
/// still has a newest-by-mtime fallback, and why it says which it used.
fn named_session(env: &Env) -> Option<(String, TranscriptPick)> {
    if let Some(entry) = tail(env, RING_STATUSLINE, 0, 1).pop()
        && let Ok(line) = usage::parse_statusline(&entry.line)
    {
        // `session_id` first; the `transcript_path`'s file STEM is the same
        // id by another road, and taking the stem rather than the path is
        // what keeps this reader inside the project directory it computed.
        let from_path = line.transcript_path.as_deref().and_then(|p| {
            Path::new(p)
                .file_stem()
                .and_then(|s| s.to_str())
                .map(str::to_owned)
        });
        if let Some(id) = line
            .session_id
            .as_deref()
            .and_then(admissible_session_id)
            .map(str::to_owned)
            .or_else(|| {
                from_path
                    .as_deref()
                    .and_then(admissible_session_id)
                    .map(str::to_owned)
            })
        {
            return Some((id, TranscriptPick::StatusLineSession));
        }
    }
    for entry in tail(env, RING_EVENT, 0, EVENT_ROWS_FOR_SESSION)
        .iter()
        .rev()
    {
        if let Ok(aterm_json::Value::Object(row)) = aterm_json::from_str(&entry.line)
            && let Some(id) = row
                .get("session_id")
                .and_then(aterm_json::Value::as_str)
                .and_then(admissible_session_id)
        {
            return Some((id.to_owned(), TranscriptPick::HookSession));
        }
    }
    None
}

/// How many event rows are searched for a `session_id`, newest first. A hook
/// that carries one carries it on every firing, so the newest few are all
/// that can matter; the bound is what keeps a read verb a read verb.
pub const EVENT_ROWS_FOR_SESSION: usize = 64;

/// Fold THIS session's transcript out of this working directory's project
/// directory, with the path it read and what chose it.
///
/// `None` when there is no home, no project directory for this directory, or
/// no `.jsonl` in it.
///
/// The choice is made in one order, and the answer says which rung it reached
/// ([`TranscriptPick`]):
///
/// 1. the session a statusLine payload names, resolved to `<id>.jsonl` in
///    this project directory;
/// 2. the session the newest hook row names, resolved the same way;
/// 3. the newest `.jsonl` by modification time.
///
/// Rung 3 is the OLD behaviour and it is now the LAST rung rather than the
/// only one, because it is wrong whenever two Claude Code sessions share a
/// working directory: the newest file is then the more recently active of the
/// two, and folding it attributes another session's tokens to this one. A
/// named session is resolved by NAME inside the computed directory — never by
/// following a path a payload supplied — and an id that could not be a file
/// name names nothing ([`admissible_session_id`]).
fn fold_session_transcript(env: &Env) -> Option<(PathBuf, usage::TranscriptUsage, TranscriptPick)> {
    let dir = env.transcripts_root()?.join(project_dir_name(&env.cwd));
    let named = named_session(env).and_then(|(id, pick)| {
        let path = dir.join(format!("{id}.jsonl"));
        path.is_file().then_some((path, pick))
    });
    let (path, pick) = match named {
        Some(found) => found,
        None => (newest_transcript(&dir)?, TranscriptPick::Newest),
    };
    let file = std::fs::File::open(&path).ok()?;
    let mut fold = usage::TranscriptUsage::new();
    // A read error mid-file keeps what was folded before it: a partial spend
    // is a measurement of part of the session, and it is labelled by the
    // fold's own row counters.
    let _ = fold.fold_reader(std::io::BufReader::new(std::io::Read::take(
        file,
        MAX_TRANSCRIPT_BYTES,
    )));
    Some((path, fold, pick))
}

/// The newest `.jsonl` in `dir` by modification time — rung 3, and only rung
/// 3. It is a weaker answer than a named session and the caller labels it so.
fn newest_transcript(dir: &Path) -> Option<PathBuf> {
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .take(MAX_TRANSCRIPT_ENTRIES)
    {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let Ok(when) = meta.modified() else { continue };
        if newest.as_ref().is_none_or(|(have, _)| when > *have) {
            newest = Some((when, path));
        }
    }
    newest.map(|(_, path)| path)
}

/// `aterm harness limits`.
fn run_limits(env: &Env, json: bool, out: &mut dyn Write) -> ExitCode {
    let (evidence, source) = gathered_evidence(env);
    let class = limits::classify(&evidence, env.now);
    let text = if json {
        limits_json(class.as_ref(), source)
    } else {
        limits_text(class.as_ref(), source)
    };
    let _ = writeln!(out, "{text}");
    ExitCode::SUCCESS
}

// ---------------------------------------------------------------------------
// The watch verbs (design §5.4, §5.7, §5.8.7)
// ---------------------------------------------------------------------------

/// Every piece of limit evidence the ledgers hold, and which source it is the
/// HIGHEST of. Shared by `limits`, `liveness` and `recover` so the three can
/// never disagree about one session's state.
fn gathered_evidence(env: &Env) -> (Vec<Evidence>, Source) {
    let hooks: Vec<Evidence> = tail(env, RING_EVENT, 0, 64)
        .iter()
        .filter_map(|e| evidence_from_row(&e.line))
        .collect();
    let mut source = if hooks.is_empty() {
        Source::Grid
    } else {
        Source::Hook
    };
    let mut evidence = hooks;
    if let Some(entry) = tail(env, RING_STATUSLINE, 0, 1).pop() {
        let windows = windows_from_row(&entry.line, 0);
        if !windows.is_empty() {
            source = Source::StatusLine;
        }
        evidence.extend(windows);
    }
    (evidence, source)
}

/// A watcher seeded from the ledgers, bound to one generation.
///
/// This command holds NO watch on the grid spine, so the watcher it builds has
/// never read a `status`: every field the spine would fill reads its own
/// honest absence rather than a guess. Evidence that came from a HOOK is fed
/// as a hook and evidence that came from the statusLine is fed as an
/// observation, because the two are different facts (design §0.2) and
/// `hooks=` reports which.
/// Whether a vendor hook has fired INTO THIS SESSION — three-valued, because
/// two of the three answers used to be folded into one boolean.
///
/// The state directory is one per MACHINE (`<aterm state>/harness/
/// claude-harness`), not one per session, and the old test was "does the
/// event ring hold any row at all" — no sid, no bound. So one hook that fired
/// in another session weeks ago made this session report `hooks=present`, and
/// `watch::stopped_short_available` then claimed a capability that cannot
/// work here; a `--bare` session inherited the same false positive. The
/// inverse was wrong too: "I cannot attribute a row to this session" was
/// reported as `absent`, which reads as a measurement and is not one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookChannel {
    /// At least one event row carries THIS session's sid.
    Present,
    /// The ring was read, this session's sid is known, and no row carries it.
    Absent,
    /// The question cannot be answered here: this process does not know its
    /// own sid, so no row can be attributed to this session either way.
    Unknown,
}

impl HookChannel {
    /// The wire word every surface prints.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            HookChannel::Present => "present",
            HookChannel::Absent => "absent",
            HookChannel::Unknown => "unknown",
        }
    }

    /// Whether a hook-only capability may be claimed. `Unknown` breaks to the
    /// safe answer: an unproved channel is not a channel.
    #[must_use]
    pub fn present(self) -> bool {
        self == HookChannel::Present
    }
}

/// Has a hook fired into THIS session? See [`HookChannel`] for why the answer
/// has three values.
fn hook_channel(env: &Env) -> HookChannel {
    if env.sid.is_empty() {
        return HookChannel::Unknown;
    }
    let seen = tail(env, RING_EVENT, 0, EVENT_ROWS_FOR_SESSION)
        .iter()
        .any(|e| match aterm_json::from_str(&e.line) {
            Ok(aterm_json::Value::Object(row)) => {
                row.get("sid").and_then(aterm_json::Value::as_str) == Some(env.sid.as_str())
            }
            _ => false,
        });
    if seen {
        HookChannel::Present
    } else {
        HookChannel::Absent
    }
}

fn seeded_watcher(env: &Env) -> (Watcher, Source, HookChannel) {
    let (evidence, source) = gathered_evidence(env);
    // The owner's own config.toml, where one admits. A file that does NOT
    // admit falls back to the shipped table rather than refusing the read
    // verb it is behind — design §1.4's rule — and `harness config get` is
    // where the refusal is printed in full.
    let harness_cfg = HarnessConfig::load(&env.harness_config_path()).unwrap_or_default();
    let mut cfg = watch::WatchConfig {
        principal: "owner".to_string(),
        nonce: env.nonce.clone(),
        enabled: harness_cfg.liveness_enabled,
        level: harness_cfg.liveness_level,
        ..watch::WatchConfig::default()
    };
    apply_accounts(env, &mut cfg);
    let mut w = Watcher::new(&env.sid, cfg, harness_cfg.limits);
    w.wake(&watch::Wake::Generation(1), 0, env.now);
    let channel = hook_channel(env);
    let hook_rows = channel.present();
    for e in evidence {
        if hook_rows && matches!(e, Evidence::Window { .. }) {
            w.observed(e, env.now);
        } else if hook_rows {
            w.wake(
                &watch::Wake::Hook(watch::HookEvent::Evidence(e)),
                0,
                env.now,
            );
        } else {
            w.observed(e, env.now);
        }
    }
    (w, source, channel)
}

/// Fill [`watch::WatchConfig`]'s two rotation fields from the roster
/// (design §5.6). THE wiring that was missing.
///
/// `accounts_enabled` is the roster's own consent and nothing else — never
/// inferred from a row existing. `account_label` and `account_dir` are the
/// §5.6 selection over the roster, and both are left as they were when
/// nothing is selectable, so `plan_limits` still answers `refused:unresolved`
/// rather than relaunching on an empty config directory. That refusal is now
/// a MEASURED "no candidate", where before it was "nothing ever wrote these
/// fields".
pub fn apply_accounts(env: &Env, cfg: &mut watch::WatchConfig) {
    let Ok(roster) = accounts::Roster::load(&env.accounts_path()) else {
        // A roster that does not admit rotates nowhere — fail-closed, and
        // `harness accounts` is where the reason is printed.
        return;
    };
    cfg.accounts_enabled = roster.enabled;
    let states = accounts::roster_states(
        &roster,
        env.active_config_dir().as_deref(),
        &observed_auth(env, &roster),
    );
    if let Some(next) = accounts::select(&states) {
        cfg.account_label = next.row.label.clone();
        cfg.account_dir = Some(next.row.dir.clone());
    }
}

/// The host facts this one-shot command can prove. It reads no socket, so it
/// asserts nothing about `hold`, `busy` or `custody` — all three read FALSE
/// and the output says the verdict is `planned`, never `executed`.
///
/// **`hold: false, busy: false, custody_user: false` IS AN ASSERTION OF THE
/// ALL-CLEAR, AND NOTHING HERE MEASURED IT.** That is tolerable for an L0/L1
/// plan, which annotates; it is not tolerable for an L3 one, which types.
/// [`guards_note`] is what a caller prints beside such a plan, and
/// [`refuse_unwitnessed_l3`] is what refuses to hand out its control lines
/// unless the caller took the assertion on itself with `--assume-spine`. The
/// grid is not read either, so `Watcher::prompt` — the approval-box fence —
/// is at its `false` default on this path; that is the whole reason the
/// refusal exists.
fn planning_guards(env: &Env) -> HostGuards {
    HostGuards {
        hold: false,
        busy: false,
        custody_user: false,
        generation: 1,
        engaged: env.master_switch() != Some(false)
            && !mark::env_engaged(env.no_harness.as_deref()),
    }
}

/// How a planned act is RENDERED, and under whose assertion. The three
/// travel together through every socket-free planner, so they are one
/// argument rather than three positional booleans a caller can transpose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Render {
    /// The JSON form.
    json: bool,
    /// Print the control lines instead of the decision.
    commands: bool,
    /// The caller ASSERTS a live watch on the grid spine (`--assume-spine`).
    spine: bool,
}

/// The one sentence a socket-free planner owes beside a plan it did not
/// measure the guards for.
pub const GUARDS_NOTE: &str = "# no spine was read: this command opens no socket, so hold=no \
     busy=no custody=no and the approval-box fence are ASSUMPTIONS, not readings";

/// Does this plan TYPE? An L3 act is the one a wrong guard can hurt.
fn plan_types(plan: &Plan) -> bool {
    matches!(plan, Plan::Act(p) if p.level >= 3)
}

/// Refuse to print the control lines of an L3 plan the caller did not
/// witness. `--assume-spine` moves the assertion onto the caller, which is
/// exactly what that flag is for and what `spine_note` already says about it.
///
/// It is a refusal of `--commands`, never of the DECISION: the plan is still
/// printed, because saying what the rung would be costs nothing. What is
/// withheld is the sendable line, which on this path would be a `turn` under
/// three unmeasured guards and an approval-box fence at its default.
fn refuse_unwitnessed_l3(plan: &Plan, commands: bool, spine: bool, out: &mut dyn Write) -> bool {
    if !commands || spine || !plan_types(plan) {
        return false;
    }
    let _ = writeln!(
        out,
        "# --commands withheld: this is an L3 act (it TYPES) and no spine was read here, so the \
         approval-box fence is at its default and hold/busy/custody are assumptions. Re-run with \
         --assume-spine to take that assertion on yourself, or use `aterm harness watch`, which \
         reads the spine every pass."
    );
    true
}

/// Render one [`Plan`] as lines, as JSON, or as the control lines a host
/// would send.
fn plan_out(plan: &Plan, what: &str, json: bool, commands: bool, out: &mut dyn Write) -> ExitCode {
    if commands && let Plan::Act(p) = plan {
        for act in &p.acts {
            match act.line() {
                Some(line) => {
                    let _ = writeln!(out, "{line}");
                }
                None => {
                    // A Stop-block is printed BY THE BRIDGE on the hook's own
                    // stdout; there is no control line for it, and inventing
                    // one would be a second decision channel.
                    let _ = writeln!(out, "# {} is answered in-band by the hook", act.verb());
                }
            }
        }
        return ExitCode::SUCCESS;
    }
    let mut o = Map::new();
    o.insert("schema".to_owned(), Value::from(1u64));
    o.insert("kind".to_owned(), Value::from(what.to_owned()));
    let (verdict, detail) = match plan {
        Plan::Idle => ("idle".to_owned(), "nothing is due".to_owned()),
        Plan::Wait { until_ms, timer } => (
            "wait".to_owned(),
            format!(
                "{} in {until_ms} ms (armed as an await, never counted down)",
                timer.as_str()
            ),
        ),
        Plan::Refused { why, what } => (why.as_str().to_owned(), what.clone()),
        Plan::Act(p) => ("planned".to_owned(), p.reason.clone()),
    };
    o.insert("verdict".to_owned(), Value::from(verdict.clone()));
    o.insert("detail".to_owned(), Value::from(detail.clone()));
    if let Plan::Act(p) = plan {
        o.insert("cap".to_owned(), Value::from(p.cap.to_owned()));
        o.insert("action".to_owned(), Value::from(p.action.clone()));
        o.insert("level".to_owned(), Value::from(u64::from(p.level)));
        o.insert("lease".to_owned(), Value::from(p.lease));
        o.insert(
            "commands".to_owned(),
            Value::Array(
                p.acts
                    .iter()
                    .filter_map(Act::line)
                    .map(Value::from)
                    .collect(),
            ),
        );
    }
    if json {
        let _ = writeln!(
            out,
            "{}",
            aterm_json::to_string(&Value::Object(o)).unwrap_or_default()
        );
        return ExitCode::SUCCESS;
    }
    let _ = writeln!(out, "verdict={verdict} {detail}");
    if let Plan::Act(p) = plan {
        let _ = writeln!(
            out,
            "cap={} action={} level={} lease={}",
            p.cap, p.action, p.level, p.lease
        );
        for act in &p.acts {
            let _ = writeln!(
                out,
                "  {}",
                act.line()
                    .unwrap_or_else(|| { format!("(in-band) {}", act.verb()) })
            );
        }
        let _ = writeln!(
            out,
            "nothing was sent: this command opens no control socket, so a host with the watch \
             open runs these after writing its journal row"
        );
    }
    ExitCode::SUCCESS
}

/// `aterm harness liveness [--json]` (design §5.4's introspection shape).
fn run_liveness(env: &Env, json: bool, out: &mut dyn Write) -> ExitCode {
    let (w, _source, hooks) = seeded_watcher(env);
    let v = w.liveness(0);
    let st = w.status();
    let hooks_word = hooks.as_str();
    if json {
        let mut o = Map::new();
        o.insert("schema".to_owned(), Value::from(1u64));
        o.insert("kind".to_owned(), Value::from("liveness".to_owned()));
        // The overlapping fields MIRROR `status`'s own contract rather than
        // re-spelling it (design §5.7).
        o.insert(
            "phase".to_owned(),
            Value::from(st.phase.as_str().to_owned()),
        );
        o.insert(
            "confidence".to_owned(),
            Value::from(v.confidence.as_str().to_owned()),
        );
        o.insert(
            "reasons".to_owned(),
            Value::Array(
                v.reasons
                    .iter()
                    .map(|r| Value::from((*r).to_owned()))
                    .collect(),
            ),
        );
        o.insert(
            "since_ms".to_owned(),
            st.since_ms.map_or(Value::Null, Value::from),
        );
        o.insert(
            "revision".to_owned(),
            st.revision.map_or(Value::Null, Value::from),
        );
        o.insert("class".to_owned(), Value::from(v.class.as_str().to_owned()));
        o.insert(
            "limit_class".to_owned(),
            w.class()
                .map_or(Value::Null, |c| Value::from(c.as_str().to_owned())),
        );
        o.insert(
            "rung".to_owned(),
            w.liveness_state()
                .rung
                .map_or(Value::Null, |r| Value::from(r.as_str().to_owned())),
        );
        o.insert("hooks".to_owned(), Value::from(hooks_word.to_owned()));
        // The one state that is HOOK-ONLY says so rather than reading false.
        o.insert(
            "stopped_short".to_owned(),
            Value::from(if watch::stopped_short_available(hooks.present()) {
                "available".to_owned()
            } else {
                format!("unavailable (hooks={hooks_word})")
            }),
        );
        o.insert("spine".to_owned(), Value::from(false));
        let _ = writeln!(
            out,
            "{}",
            aterm_json::to_string(&Value::Object(o)).unwrap_or_default()
        );
        return ExitCode::SUCCESS;
    }
    let _ = writeln!(
        out,
        "OK schema=1 class={} confidence={} reasons={} phase={} limit_class={} rung={} hooks={hooks_word}",
        v.class.as_str(),
        v.confidence.as_str(),
        if v.reasons.is_empty() {
            "-".to_owned()
        } else {
            v.reasons.join(",")
        },
        st.phase.as_str(),
        w.class().map_or("none", limits::Class::as_str),
        w.liveness_state().rung.map_or("-", watch::Rung::as_str),
    );
    let _ = writeln!(
        out,
        "this command holds no watch on the grid spine, so `phase` and `since_ms` read their own \
         absence; a host that does holds the watch and answers from it"
    );
    if !hooks.present() {
        let _ = writeln!(
            out,
            "stopped-short: unavailable (hooks={hooks_word}) — every other state is reachable \
             from the spine alone"
        );
    }
    ExitCode::SUCCESS
}

/// `aterm harness recover <class|action> [--target <t>]`.
fn run_recover(
    env: &Env,
    what: &str,
    target: Option<&str>,
    how: Render,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let Render {
        json,
        commands,
        spine,
    } = how;
    let (mut w, _source, _hooks) = seeded_watcher(env);
    if let Some(t) = target {
        w.set_target(t);
    }
    let guards = planning_guards(env);
    if let Some(action) = limits::Action::parse(what) {
        // One step of the table, for the class the LEDGER names. A class is
        // never asserted from the command line: the evidence decides it.
        let Some(acts) = w.acts_of(action, action.level(), env.now) else {
            let _ = writeln!(
                err,
                "aterm harness: no limit class is live, so there is nothing for {what} to recover \
                 from (`aterm harness limits` says what the ledger names)"
            );
            return ExitCode::from(1);
        };
        let plan = Plan::Act(Box::new(watch::Planned {
            acts,
            cap: "limits",
            action: action.as_str().to_owned(),
            level: action.level(),
            reason: format!(
                "harness recover {what}{}",
                target.map_or(String::new(), |t| format!(" target={t}"))
            ),
            lease: action.level() >= 3,
            step: None,
            limit_action: Some(action),
            rung: None,
            wait_until: None,
        }));
        // `run_recover` assembles the plan itself, so it owes `Watcher::plan`'s
        // last fence by hand: a typed act needs a real launch nonce, and
        // without one this printed a `turn id=:1:1 …` that the server rejects
        // at parse before it types a byte.
        let plan = w.fence_turn_nonce(plan);
        if !spine {
            let _ = writeln!(out, "{GUARDS_NOTE}");
        }
        let commands = commands && !refuse_unwitnessed_l3(&plan, commands, spine, out);
        return plan_out(&plan, "recover", json, commands, out);
    }
    let Some(class) = limits::Class::parse(what) else {
        let _ = writeln!(
            err,
            "aterm harness: {what} is neither a class nor an action"
        );
        return ExitCode::from(2);
    };
    match w.class() {
        Some(live) if live == class => {}
        live => {
            let _ = writeln!(
                err,
                "aterm harness: the ledger names {}, not {what}; a class is read from the \
                 evidence and never asserted on the command line",
                live.map_or("no class", limits::Class::as_str)
            );
            return ExitCode::from(1);
        }
    }
    let plan = w.plan(&guards, 0, env.now);
    if !spine {
        let _ = writeln!(out, "{GUARDS_NOTE}");
    }
    let commands = commands && !refuse_unwitnessed_l3(&plan, commands, spine, out);
    plan_out(&plan, "recover", json, commands, out)
}

/// Open the control socket this instance answers on, or say why not.
///
/// NOTHING is retried and nothing is cached: a self-update relaunches aterm
/// under a new socket name, and a caller that resolves on every run follows
/// it (`aterm_ctl::resolve_sock_for`'s own rule).
fn open_wire(env: &Env) -> Result<CtlWire, String> {
    let sock = match &env.sock {
        Some(sock) => sock.clone(),
        None => aterm_ctl::resolve_sock_for(Some(&env.sid))
            .map_err(|e| format!("no control socket: {e}"))?,
    };
    let token = aterm_ctl::read_token_beside(&sock).map_err(|e| format!("{e}"))?;
    CtlWire::connect(WireConfig {
        sock,
        token,
        sid: env.sid.clone(),
        state: env.state.clone(),
        engaged: env.master_switch() != Some(false)
            && !mark::env_engaged(env.no_harness.as_deref()),
    })
}

/// `aterm harness switch <model|account> <target>` (design §5.7).
///
/// This is one of the two verbs that ACT. It takes the same watcher every
/// read verb builds — the owner's `config.toml`, the roster's rotation
/// consent, the ledger's evidence — binds it to the LIVE session's launch
/// nonce and generation, reads the spine once so the approval-box fence has
/// something to fence on, asks [`Watcher::switch`] for the plan, and runs it
/// through [`Watcher::execute`]: the loop's own actuator, not a second copy.
///
/// The reply is design §5.7's: `OK id=<n> verdict=executed|refused:<why>`.
fn run_switch_now(
    env: &Env,
    kind: watch::SwitchKind,
    target: &str,
    json: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let mut wire = match open_wire(env) {
        Ok(wire) => wire,
        Err(why) => return acted_out(out, err, json, "switch", 0, "refused:unresolved", &why),
    };
    let (mut w, _source, _hooks) = seeded_watcher(env);
    w.set_target(target);
    w.adopt_nonce(wire.nonce());
    let mut now_ms = watch::Wire::now_ms(&mut wire);
    let mut now_s = watch::Wire::now_unix(&mut wire);
    // The spine, once: `self.prompt` is what refuses an L3 act while an
    // approval box is up, and a grid never read leaves it false.
    w.read_spine(&mut wire, now_ms, now_s);
    let host = watch::Wire::guards(&mut wire);
    // The wire's generation is the LIVE one; adopt it before planning so a
    // relaunch since this process started reads as a new generation rather
    // than as a mismatch.
    w.wake(&watch::Wake::Generation(host.generation), now_ms, now_s);
    now_ms = watch::Wire::now_ms(&mut wire);
    now_s = watch::Wire::now_unix(&mut wire);
    let plan = w.switch(kind, &host, now_s);
    match plan {
        Plan::Act(planned) => {
            let done = w.execute(&mut wire, &planned, now_ms, now_s);
            let detail = format!("{} -> {target}", kind.as_str());
            acted_out(
                out,
                err,
                json,
                "switch",
                done.journal,
                &done.verdict,
                &detail,
            )
        }
        Plan::Refused { why, what } => acted_out(out, err, json, "switch", 0, why.as_str(), &what),
        // `switch` asks for one act; the planner answers `Act` or `Refused`
        // for it and never `Idle`/`Wait`, both of which belong to the table
        // walk. Reported as a refusal rather than silently as success.
        other => acted_out(
            out,
            err,
            json,
            "switch",
            0,
            "refused:unresolved",
            &format!("the planner answered {other:?} rather than an act"),
        ),
    }
}

/// The `OK id=<n> verdict=…` reply of design §5.7, in both forms.
///
/// A refusal exits 1: a shell that reads only the exit code must not read
/// `refused:hold` as a switch that happened.
fn acted_out(
    out: &mut dyn Write,
    err: &mut dyn Write,
    json: bool,
    kind: &str,
    id: u64,
    verdict: &str,
    detail: &str,
) -> ExitCode {
    if json {
        let mut o = Map::new();
        o.insert("schema".to_owned(), Value::from(1u64));
        o.insert("kind".to_owned(), Value::from(kind.to_owned()));
        o.insert("id".to_owned(), Value::from(id));
        o.insert("verdict".to_owned(), Value::from(verdict.to_owned()));
        o.insert("detail".to_owned(), Value::from(detail.to_owned()));
        let _ = writeln!(
            out,
            "{}",
            aterm_json::to_string(&Value::Object(o)).unwrap_or_default()
        );
    } else {
        let _ = writeln!(out, "OK id={id} verdict={verdict}");
        if !detail.is_empty() {
            let _ = writeln!(out, "# {detail}");
        }
    }
    if verdict == "executed" || verdict == "settled" {
        ExitCode::SUCCESS
    } else {
        let _ = err.write_all(b"");
        ExitCode::from(1)
    }
}

/// `aterm harness watch` — RUN THE LOOP (design §5.3-§5.8).
///
/// The loop itself is [`Watcher::pump`] and has been written since this
/// module's first commit; what it never had was a transport and a verb.
/// Both are here now: [`CtlWire`] parks on `subscribe … events` and arms the
/// `await` family for every deadline, and this function turns passes into
/// lines. There is no sleep, no cadence and no timer in the path — a pass
/// happens because the server pushed a frame or because an armed wait
/// answered, and `--passes` is a BOUND on how many, never a rate.
fn run_watch(
    env: &Env,
    passes: Option<u64>,
    json: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let mut wire = match open_wire(env) {
        Ok(wire) => wire,
        Err(why) => {
            let _ = writeln!(err, "aterm harness: {why}");
            return ExitCode::from(1);
        }
    };
    let (mut w, source, hooks) = seeded_watcher(env);
    w.adopt_nonce(wire.nonce());
    if !json {
        let _ = writeln!(
            out,
            "# watching {} · source={} hooks={} · nonce={} · ingress=subscribe+await (no timer)",
            if env.sid.is_empty() {
                "this session"
            } else {
                &env.sid
            },
            source.as_str(),
            hooks.as_str(),
            if wire.nonce().is_empty() {
                "-"
            } else {
                "(read)"
            }
        );
    }
    let mut n = 0u64;
    loop {
        if passes.is_some_and(|max| n >= max) {
            break;
        }
        let pass = w.pump(&mut wire);
        n = n.saturating_add(1);
        if json {
            let mut o = Map::new();
            o.insert("schema".to_owned(), Value::from(1u64));
            o.insert("kind".to_owned(), Value::from("watch-pass".to_owned()));
            o.insert("pass".to_owned(), Value::from(n));
            o.insert("wake".to_owned(), Value::from(format!("{:?}", pass.wake)));
            o.insert(
                "events".to_owned(),
                Value::from(u64::try_from(pass.events.len()).unwrap_or(0)),
            );
            o.insert(
                "journal".to_owned(),
                pass.journal.map_or(Value::Null, Value::from),
            );
            o.insert(
                "verdict".to_owned(),
                pass.verdict.clone().map_or(Value::Null, Value::from),
            );
            o.insert(
                "sent".to_owned(),
                Value::Array(pass.sent.iter().cloned().map(Value::from).collect()),
            );
            let _ = writeln!(
                out,
                "{}",
                aterm_json::to_string(&Value::Object(o)).unwrap_or_default()
            );
        } else {
            let _ = writeln!(
                out,
                "pass={n} wake={:?} events={} journal={} verdict={} sent={}",
                pass.wake,
                pass.events.len(),
                pass.journal.unwrap_or(0),
                pass.verdict.as_deref().unwrap_or("-"),
                pass.sent.len()
            );
        }
        if matches!(pass.wake, watch::Wake::Closed) || w.closed() {
            break;
        }
    }
    if let Some(why) = wire.failure() {
        let _ = writeln!(err, "aterm harness: the watch ended — {why}");
        return ExitCode::from(1);
    }
    if !json {
        let _ = writeln!(out, "# {n} pass(es); the pushed stream ended");
    }
    ExitCode::SUCCESS
}

/// `aterm harness config get|set` (design §5.7, §5.8.7).
fn run_config(
    env: &Env,
    key: &str,
    value: Option<&str>,
    json: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let path = env.harness_config_path();
    let mut cfg = match HarnessConfig::load(&path) {
        Ok(cfg) => cfg,
        Err(why) => {
            // Fail-closed and whole-file: a config that does not admit is
            // never half-applied, and `set` on it would write a file the
            // reader just refused.
            let _ = writeln!(err, "aterm harness: {why}");
            return ExitCode::from(1);
        }
    };
    let Some(value) = value else {
        return config_get(&cfg, key, &path, json, out, err);
    };
    if let Err(why) = cfg.set(key, value) {
        let _ = writeln!(err, "aterm harness: {why}");
        return ExitCode::from(1);
    }
    if let Some(parent) = path.parent()
        && let Err(e) = ensure_dir(parent)
    {
        let _ = writeln!(err, "aterm harness: {}: {e}", parent.display());
        return ExitCode::from(1);
    }
    // 0600: the file carries no secret, but it decides what the harness may
    // do to this session, and the state directory around it is already 0700.
    if let Err(e) = write_atomic(&path, &cfg.to_toml(), 0o600) {
        let _ = writeln!(err, "aterm harness: {}: {e}", path.display());
        return ExitCode::from(1);
    }
    // Re-admit what was just written. A file this command produced that its
    // own reader refuses is a bug worth failing on HERE, rather than at the
    // next classification.
    match HarnessConfig::load(&path) {
        Ok(back) if back == cfg => {}
        Ok(_) => {
            let _ = writeln!(
                err,
                "aterm harness: {} was written but reads back as a different value",
                path.display()
            );
            return ExitCode::from(1);
        }
        Err(why) => {
            let _ = writeln!(
                err,
                "aterm harness: {} was written but no longer admits — {why}",
                path.display()
            );
            return ExitCode::from(1);
        }
    }
    config_get(&cfg, key, &path, json, out, err)
}

fn config_get(
    cfg: &HarnessConfig,
    key: &str,
    path: &Path,
    json: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    if key.is_empty() {
        if json {
            let mut o = Map::new();
            o.insert("schema".to_owned(), Value::from(1u64));
            o.insert("kind".to_owned(), Value::from("harness-config".to_owned()));
            o.insert("file".to_owned(), Value::from(path.display().to_string()));
            let mut keys = Map::new();
            for name in config::key_names() {
                keys.insert(
                    name.clone(),
                    Value::from(cfg.get(&name).unwrap_or_default()),
                );
            }
            o.insert("keys".to_owned(), Value::Object(keys));
            let _ = writeln!(
                out,
                "{}",
                aterm_json::to_string(&Value::Object(o)).unwrap_or_default()
            );
            return ExitCode::SUCCESS;
        }
        let _ = writeln!(out, "# {}", path.display());
        for name in config::key_names() {
            let _ = writeln!(out, "{name} = {}", cfg.get(&name).unwrap_or_default());
        }
        let _ = writeln!(
            out,
            "# the per-class action sets of design 5.8.7 are enforced on `set`: a switch-* for \
             network-offline, unknown or model-bucket-limit, a retry for unknown, or anything \
             but the pinned members of auth and spend-billing is refused and nothing is written"
        );
        return ExitCode::SUCCESS;
    }
    let Some(value) = cfg.get(key) else {
        let _ = writeln!(
            err,
            "aterm harness: `{key}` is not a key this build carries; `aterm harness config get` \
             lists every one"
        );
        return ExitCode::from(2);
    };
    if json {
        let mut o = Map::new();
        o.insert("schema".to_owned(), Value::from(1u64));
        o.insert("kind".to_owned(), Value::from("harness-config".to_owned()));
        o.insert("key".to_owned(), Value::from(key.to_owned()));
        o.insert("value".to_owned(), Value::from(value));
        let _ = writeln!(
            out,
            "{}",
            aterm_json::to_string(&Value::Object(o)).unwrap_or_default()
        );
    } else {
        let _ = writeln!(out, "{key} = {value}");
    }
    ExitCode::SUCCESS
}

/// `aterm harness accounts add <label> <dir>` (design §5.6).
///
/// The SAME no-credentials rule `discover` holds: the directory is checked
/// for existence and for the non-secret `oauthAccount` marker the vendor
/// itself leaves there, and nothing inside it is read beyond that. No token,
/// keychain item or email address is read, quoted or written.
///
/// The row is appended with the roster's own consent UNCHANGED — a first
/// write creates the file with `[accounts] enabled = false`, exactly as
/// `discover --write` does, because turning rotation on is a separate,
/// deliberate edit (design §2.3, §5.6).
fn run_accounts_add(
    roster: &accounts::Roster,
    path: &Path,
    add: &(String, PathBuf),
    json: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let (label, dir) = (add.0.as_str(), add.1.as_path());
    if !dir.is_absolute() {
        let _ = writeln!(
            err,
            "aterm harness: {} is not absolute; a relaunch exports CLAUDE_CONFIG_DIR and a \
             relative path would resolve against whatever directory the relaunch happened to \
             start in",
            dir.display()
        );
        return ExitCode::from(2);
    }
    if roster.rows.iter().any(|r| r.label == label) {
        let _ = writeln!(
            err,
            "aterm harness: {} already carries an account called {label:?}; a duplicate label \
             would make the roster refuse whole",
            path.display()
        );
        return ExitCode::from(1);
    }
    if let Some(existing) = roster
        .rows
        .iter()
        .find(|r| !r.dir.is_empty() && Path::new(&r.dir) == dir)
    {
        let _ = writeln!(
            err,
            "aterm harness: {} is already the directory of account {:?}",
            dir.display(),
            existing.label
        );
        return ExitCode::from(1);
    }
    if !dir.is_dir() {
        let _ = writeln!(
            err,
            "aterm harness: {} is not a directory; `accounts add` names the config DIRECTORY \
             CLAUDE_CONFIG_DIR would point at",
            dir.display()
        );
        return ExitCode::from(1);
    }
    let row = accounts::row_toml(label, dir).unwrap_or_default();
    if row.is_empty() {
        let _ = writeln!(
            err,
            "aterm harness: {label:?} is outside [A-Za-z0-9._-]; a label names a directory on a \
             relaunch command line and is quoted into ledger rows"
        );
        return ExitCode::from(2);
    }
    let mut text = String::new();
    if !path.exists() {
        text.push_str(
            "# Written by `aterm harness accounts add`.\n\
             # Rotation is OFF until the owner turns it on, and no credential is here.\n\
             [accounts]\nenabled = false\n",
        );
    }
    text.push_str(&row);
    if let Some(parent) = path.parent()
        && let Err(e) = ensure_dir(parent)
    {
        let _ = writeln!(err, "aterm harness: {}: {e}", parent.display());
        return ExitCode::from(1);
    }
    let appended = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut f| f.write_all(text.as_bytes()));
    if let Err(e) = appended {
        let _ = writeln!(err, "aterm harness: {}: {e}", path.display());
        return ExitCode::from(1);
    }
    if let Err(why) = accounts::Roster::load(path) {
        let _ = writeln!(
            err,
            "aterm harness: the row was appended to {} but the roster no longer admits — {why}",
            path.display()
        );
        return ExitCode::from(1);
    }
    if json {
        let mut o = Map::new();
        o.insert("schema".to_owned(), Value::from(1u64));
        o.insert("kind".to_owned(), Value::from("accounts-add".to_owned()));
        o.insert("label".to_owned(), Value::from(label.to_owned()));
        o.insert("dir".to_owned(), Value::from(dir.display().to_string()));
        o.insert("file".to_owned(), Value::from(path.display().to_string()));
        o.insert("rotation".to_owned(), Value::from(roster.enabled));
        let _ = writeln!(
            out,
            "{}",
            aterm_json::to_string(&Value::Object(o)).unwrap_or_default()
        );
    } else {
        let _ = writeln!(
            out,
            "added {label} -> {} to {} · rotation {} · no credential was read",
            dir.display(),
            path.display(),
            if roster.enabled { "on" } else { "OFF" }
        );
    }
    ExitCode::SUCCESS
}

/// `aterm harness nudge [<sid>] [--level <l>]` — one ladder rung, by hand.
fn run_nudge(
    env: &Env,
    sid: &str,
    level: watch::Level,
    how: Render,
    out: &mut dyn Write,
) -> ExitCode {
    let Render {
        json,
        commands,
        spine,
    } = how;
    let (mut w, _source, _hooks) = seeded_watcher(env);
    if !sid.is_empty() && sid != env.sid {
        // Named for the §5.7 grammar and reported honestly: this command
        // holds no watch on another session and opens no socket, so it can
        // only say what the rung WOULD be here.
        let _ = writeln!(
            out,
            "# {sid} is not this session ({}); the rung below is this session's",
            if env.sid.is_empty() { "-" } else { &env.sid }
        );
    }
    // A manual rung is asked for at the level it names and goes through the
    // SAME guarded path as an automatic one, so it can reach nothing the
    // loop could not.
    let rung = match level {
        watch::Level::Warn => Rung::Warn,
        watch::Level::Stop => Rung::StopBlock,
        watch::Level::Turn => Rung::Nudge,
        watch::Level::Escape => Rung::Escape,
    };
    // The level the operator NAMED is the level in force for this one rung;
    // the ladder's own `liveness.level` bounds what the LOOP reaches on its
    // own initiative, and a hand-asked rung is not that.
    w.set_level(level);
    let plan = w.nudge(rung, &planning_guards(env), 0);
    if !spine {
        let _ = writeln!(out, "{GUARDS_NOTE}");
    }
    let commands = commands && !refuse_unwitnessed_l3(&plan, commands, spine, out);
    plan_out(&plan, "nudge", json, commands, out)
}

/// `aterm harness ledger <name> [<n>]`.
/// The store prefix's default, MIRRORING `atpkg`'s `platform::default_prefix`
/// rather than depending on it — the same discipline [`default_config_path`]
/// keeps for `aterm.toml`. Read-only either way: every row the store produces
/// is delegated to `aterm pkg gc`.
fn default_store_dir(home: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library")
            .join("Application Support")
            .join("aterm")
            .join("pkg")
            .join("store")
    } else {
        home.join(".local")
            .join("share")
            .join("aterm")
            .join("pkg")
            .join("store")
    }
}

/// Where `disk` looks. Every path is derived HERE and handed to
/// [`disk::scan`], which reads no environment of its own.
fn disk_roots(env: &Env, targets: &[PathBuf]) -> disk::Roots {
    let home = env.home.clone();
    disk::Roots {
        volume: Some(env.cwd.clone()),
        versions_dir: home.as_ref().map(|h| {
            h.join(".local")
                .join("share")
                .join("claude")
                .join("versions")
        }),
        live_link: home
            .as_ref()
            .map(|h| h.join(".local").join("bin").join("claude")),
        store_dir: home.as_ref().map(|h| default_store_dir(h)),
        targets: targets.to_vec(),
        transcripts: home.as_ref().map(|h| h.join(".claude").join("projects")),
    }
}

/// The `[disk]` knobs, all three of them, read from `aterm.toml`.
///
/// This used to say the two NUMERIC knobs of design §5.5 (`warn_free_gib`,
/// `target_stale_days`) "stay at their defaults because this file has no TOML
/// number reader", which was false when it was written: [`align`]'s
/// `read_toml_dotted` is a fail-closed reader in this same module tree, it
/// already returns `Val::Int`, and [`config`] reads every `Kind::Num` through
/// it. The USAGE paragraph advertised `disk.target_stale_days` meanwhile, so
/// the help named a knob nothing read. Both are read here now, through that
/// one reader — no second grammar, which is the thing the old sentence was
/// right to be afraid of.
///
/// A file that does NOT admit falls back to the shipped defaults rather than
/// refusing the verb behind it (design §1.4's rule), exactly as the boolean
/// path does; a value outside the knob's range is dropped the same way.
fn disk_config(env: &Env) -> disk::Config {
    let text = env
        .config
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();
    let apply = toml_bool(&text, "disk", "apply").unwrap_or(disk::DEFAULT_APPLY);
    let num = |key: &str| -> Option<i64> { align::toml_int(&text, "disk", key) };
    let d = disk::Config::default();
    disk::Config {
        apply,
        warn_free_gib: num("warn_free_gib")
            .and_then(|v| u64::try_from(v).ok())
            .unwrap_or(d.warn_free_gib),
        // A build directory is never a candidate before this many days, so a
        // NEGATIVE value would make everything a candidate at once. Refused,
        // not clamped: the tie breaks to the shipped default.
        target_stale_days: num("target_stale_days")
            .filter(|v| *v >= 0)
            .unwrap_or(d.target_stale_days),
    }
}

/// `aterm harness disk` (design §5.5). Report-only unless a class is named
/// AND `disk.apply` is on; every refusal becomes a denial row.
fn run_disk(
    env: &Env,
    targets: &[PathBuf],
    class: Option<disk::Class>,
    json: bool,
    out: &mut dyn Write,
) -> ExitCode {
    let roots = disk_roots(env, targets);
    let config = disk_config(env);
    let free = roots.volume.as_deref().and_then(disk::free_bytes);
    let survey = disk::scan(&roots, env.now, disk::Trigger::OnDemand, config, free);
    let rep = disk::report(&survey, config);
    let _ = append_row(env, RING_DISK, &disk::report_row(env.now, &env.sid, &rep));

    let done = class.map(|_| {
        disk::apply(
            &rep,
            survey.transcripts_root.as_deref(),
            class,
            &mut disk::remove_tree,
        )
    });
    if let Some(done) = &done {
        for row in &done.removed {
            let _ = append_row(env, RING_DISK, &disk::removal_row(env.now, &env.sid, row));
        }
        for refusal in &done.denials {
            let _ = append_row(
                env,
                RING_DISK,
                &disk::denial_row(env.now, &env.sid, refusal),
            );
        }
    }

    if json {
        let _ = writeln!(out, "{}", rep.to_json());
        if let Some(done) = &done {
            let mut o = Map::new();
            o.insert("schema".to_owned(), Value::from(1u64));
            o.insert("kind".to_owned(), Value::from("disk-apply".to_owned()));
            o.insert(
                "removed".to_owned(),
                Value::from(u64::try_from(done.removed.len()).unwrap_or(u64::MAX)),
            );
            o.insert("freed_bytes".to_owned(), Value::from(done.freed_bytes));
            o.insert(
                "denials".to_owned(),
                Value::Array(
                    done.denials
                        .iter()
                        .map(|d| Value::from(d.describe()))
                        .collect(),
                ),
            );
            let _ = writeln!(
                out,
                "{}",
                aterm_json::to_string(&Value::Object(o)).unwrap_or_default()
            );
        }
        return ExitCode::SUCCESS;
    }

    let _ = writeln!(out, "{}", rep.headline());
    for row in &rep.rows {
        let _ = writeln!(out, "  {}", row.line());
    }
    for note in &rep.notes {
        let _ = writeln!(out, "  note: {note}");
    }
    if rep.rows.is_empty() {
        let _ = writeln!(
            out,
            "  nothing with a witness — a directory with no proof that a build tool laid it is never a candidate"
        );
    }
    match &done {
        None => {
            let _ = writeln!(out, "report only: {}", disk::Refusal::NoClass.describe());
        }
        Some(done) => {
            let _ = writeln!(out, "{}", done.headline());
            for row in &done.removed {
                let _ = writeln!(
                    out,
                    "  removed {} — {}",
                    row.path.display(),
                    row.witness.describe()
                );
            }
            for refusal in &done.denials {
                let _ = writeln!(out, "  denied: {}", refusal.describe());
            }
        }
    }
    ExitCode::SUCCESS
}

fn run_ledger(
    env: &Env,
    name: &str,
    count: usize,
    since: u64,
    json: bool,
    out: &mut dyn Write,
) -> ExitCode {
    let rows = tail(env, name, since, count);
    for entry in &rows {
        if json {
            let _ = writeln!(out, "{}", entry.line);
        } else {
            let _ = writeln!(out, "{} {}", entry.id, entry.line);
        }
    }
    if rows.is_empty() && !json {
        let _ = writeln!(
            out,
            "no rows in {name} — source={} (hooks are enrichment; an empty ledger is not a fault)",
            Source::Grid.as_str()
        );
    }
    ExitCode::SUCCESS
}

// ---------------------------------------------------------------------------
// Alignment (design §3.2, §3.5, §3.6) — the `align` and `caps` verbs
// ---------------------------------------------------------------------------

/// Everything one alignment pass computed, in one value, so `align` and `caps`
/// render the SAME run rather than each doing its own.
#[derive(Debug, Clone)]
pub struct AlignReport {
    /// What the verdict is keyed by (design §3.5).
    pub tuple: align::Tuple,
    /// The adopted verdict and its per-capability detail.
    pub alignment: align::Alignment,
    /// The admitted contract, when there was one to admit.
    pub contract: Option<align::Contract>,
    /// Every probe result, file probes and `[[tool]]` rows alike.
    pub probes: Vec<align::ProbeResult>,
    /// The tree the contract and probes came from.
    pub tree: Option<PathBuf>,
    /// The installed program the probes measured.
    pub candidate: Option<PathBuf>,
    /// One sentence per thing that was missing or disagreed. NEVER empty when
    /// the verdict is `pending`: a pending verdict that does not say why is
    /// indistinguishable from a broken one.
    pub notes: Vec<String>,
    /// The refusal that stopped the pass, if one did. An admission refusal is
    /// the whole file's, so there is no partial contract to report beside it.
    pub refusal: Option<String>,
}

impl AlignReport {
    /// An empty report keyed by `tuple`: no contract, no probes, `pending`.
    fn pending(tuple: align::Tuple, note: String) -> Self {
        AlignReport {
            tuple,
            alignment: align::Alignment {
                verdict: align::Verdict::Pending,
                caps: Vec::new(),
                attested: align::Attestation::None,
                disagreement: None,
            },
            contract: None,
            probes: Vec::new(),
            tree: None,
            candidate: None,
            notes: vec![note],
            refusal: None,
        }
    }

    /// The JSON form both verbs print, the alignment document plus what this
    /// pass measured it against.
    #[must_use]
    pub fn to_json(&self) -> String {
        let doc = self.alignment.to_json(&self.tuple);
        let Ok(Value::Object(mut o)) = aterm_json::from_str::<Value>(&doc) else {
            return doc;
        };
        o.insert(
            "tree".to_owned(),
            self.tree
                .as_ref()
                .map_or(Value::Null, |p| Value::from(p.display().to_string())),
        );
        o.insert(
            "candidate".to_owned(),
            self.candidate
                .as_ref()
                .map_or(Value::Null, |p| Value::from(p.display().to_string())),
        );
        o.insert(
            "probes".to_owned(),
            Value::Array(
                self.probes
                    .iter()
                    .map(|p| {
                        let mut r = Map::new();
                        r.insert("id".to_owned(), Value::from(p.id.clone()));
                        r.insert(
                            "status".to_owned(),
                            Value::from(p.status.as_str().to_owned()),
                        );
                        r.insert("detail".to_owned(), Value::from(p.detail.clone()));
                        Value::Object(r)
                    })
                    .collect(),
            ),
        );
        o.insert(
            "notes".to_owned(),
            Value::Array(self.notes.iter().map(|n| Value::from(n.clone())).collect()),
        );
        o.insert(
            "refusal".to_owned(),
            self.refusal.clone().map_or(Value::Null, Value::from),
        );
        aterm_json::to_string(&Value::Object(o)).unwrap_or(doc)
    }
}

/// Run one alignment pass: admit the contract, digest the probe set, resolve
/// the program, run the probes, evaluate, and adopt the intersection with the
/// signed entry when one was given.
///
/// EVERY missing input is a NOTE and a `pending` verdict, never a panic and
/// never an error exit: design §1.4's rule is that no failure inside the
/// harness path may make the wrapped program un-launchable, and the read verbs
/// hold the same line one step earlier. The one thing that exits non-zero is a
/// contract this client REFUSED, because that is a statement about a file
/// somebody published, not about a machine that has not installed one.
#[must_use]
pub fn align_now(env: &Env, signed: Option<&str>) -> AlignReport {
    let harness_build = env
        .harness_tree()
        .as_deref()
        .and_then(|t| t.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| String::from("dev"));
    let mut tuple = align::Tuple {
        harness: HARNESS.to_string(),
        harness_build,
        program: String::new(),
        program_build: String::new(),
        aterm_build: env.aterm_build.clone(),
        probe_set: String::new(),
    };

    let Some(tree) = env.harness_tree() else {
        return AlignReport::pending(
            tuple,
            String::from(
                "no harness tree: name one with --tree or $ATERM_HARNESS_ROOT (a dev-linked \
                 checkout at <state>/tree is found automatically)",
            ),
        );
    };
    let contract_path = tree.join("harness.toml");
    let text = match std::fs::read_to_string(&contract_path) {
        Ok(text) => text,
        Err(e) => {
            let mut report =
                AlignReport::pending(tuple, format!("{}: {e}", contract_path.display()));
            report.tree = Some(tree);
            return report;
        }
    };
    let contract = match align::Contract::admit(&text) {
        Ok(contract) => contract,
        Err(why) => {
            let mut report = AlignReport::pending(tuple, format!("{}", contract_path.display()));
            report.tree = Some(tree);
            report.refusal = Some(why);
            return report;
        }
    };
    tuple.probe_set = contract.probe_set.clone();
    tuple.program = contract
        .wraps_any
        .first()
        .cloned()
        .unwrap_or_else(|| String::from("-"));

    let mut notes: Vec<String> = Vec::new();
    let probes_dir = tree.join("probes");

    // The declared `probe_set` must equal what this tree's probes digest to,
    // or the verdict would be filed under a key that describes other bytes.
    if !contract.probe_set.is_empty() {
        match align::probe_set_digest(&probes_dir) {
            Ok(measured) if measured == contract.probe_set => {}
            Ok(measured) => {
                let alignment = align::Alignment {
                    verdict: align::Verdict::Unsupported(format!(
                        "probe_set {} does not match probes/ ({})",
                        align::probe_set_short(&contract.probe_set),
                        align::probe_set_short(&measured)
                    )),
                    caps: contract
                        .capabilities
                        .iter()
                        .map(|c| align::CapVerdict {
                            id: c.id.clone(),
                            serve: align::Serve::Unsupported(String::from(
                                "the tree's probe_set is not what it declares",
                            )),
                            blamed: None,
                            ok: 0,
                            total: c.needs.len(),
                        })
                        .collect(),
                    attested: align::Attestation::Local,
                    disagreement: None,
                };
                return AlignReport {
                    tuple,
                    alignment,
                    contract: Some(contract),
                    probes: Vec::new(),
                    tree: Some(tree),
                    candidate: None,
                    notes,
                    refusal: None,
                };
            }
            Err(why) => notes.push(why),
        }
    }

    let candidate = env
        .against
        .clone()
        .or_else(|| which_on_path(&tuple.program));
    let Some(candidate) = candidate else {
        let mut report = AlignReport::pending(
            tuple.clone(),
            format!(
                "{} is not on PATH: name the installed copy with --against",
                tuple.program
            ),
        );
        report.tree = Some(tree);
        report.contract = Some(contract);
        return report;
    };
    tuple.program_build = align::tool_version(
        candidate.to_string_lossy().as_ref(),
        align::PROBE_ONE_BUDGET,
    )
    .as_deref()
    .and_then(align::first_dotted_number)
    .unwrap_or_else(|| String::from("unknown"));

    let ctx = align::ProbeCtx {
        candidate: candidate.clone(),
        tree: tree.clone(),
        state: env.state.clone(),
        online: false,
    };
    let run = align::run_probes(&probes_dir, &ctx, align::PROBE_BUDGET);
    if let Some(refusal) = &run.refusal {
        notes.push(refusal.clone());
    }
    let mut probes = run.results;
    for row in &contract.tools {
        let mut runner = |bin: &str| align::tool_version(bin, align::PROBE_ONE_BUDGET);
        probes.push(align::probe_tool(row, &mut runner));
    }

    // The owner's `[alignment] required` / `allow_*` overrides live in the
    // harness config (design §3.7) and are not read here: this verb reports
    // the contract's own list, and says so rather than implying it read one.
    let local = align::evaluate(&contract, &probes, &contract.required, &[]);
    let alignment = match signed {
        None => {
            if !matches!(local.verdict, align::Verdict::Pending) {
                notes.push(String::from(
                    "no signed alignment row was given, so this verdict is local only",
                ));
            }
            align::adopt(None, local)
        }
        // ONE selection rule, `align::signed_for`, not a second copy of it.
        // This arm used to hand-roll `SignedRow::parse` + `matches` here,
        // which is how the tested function (`signed_for`, design §3.4's
        // fail-closed `Reject::Alignment`) ended up dead while the shipped
        // path was the copy — with different failure semantics into the
        // bargain: `--signed` takes one operand today, but the rule is
        // whole-list, and a list whose selection degrades entry by entry is
        // not fail-closed.
        Some(entry) => match align::signed_for(&[entry], &tuple) {
            Err(why) => {
                notes.push(why);
                align::adopt(None, local)
            }
            Ok(None) => {
                notes.push(format!(
                    "the signed entry is not about this tuple ({}@{}, probe_set {})",
                    tuple.program,
                    tuple.program_build,
                    align::probe_set_short(&tuple.probe_set)
                ));
                align::adopt(None, local)
            }
            Ok(Some(row)) => align::adopt(Some(&row.verdict), local),
        },
    };
    if let Some(disagreement) = &alignment.disagreement {
        notes.push(format!("signed and local disagree: {disagreement}"));
    }
    AlignReport {
        tuple,
        alignment,
        contract: Some(contract),
        probes,
        tree: Some(tree),
        candidate: Some(candidate),
        notes,
        refusal: None,
    }
}

/// The first `name` on `$PATH` that is a regular file. Not a full `which`: it
/// does not consult aliases, functions or `PATHEXT`, because the thing being
/// resolved is a vendor binary a shim already puts in one place.
#[must_use]
fn which_on_path(name: &str) -> Option<PathBuf> {
    if name.is_empty() || name.contains('/') {
        return None;
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

fn run_align(
    env: &Env,
    json: bool,
    write: bool,
    signed: Option<&str>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let report = align_now(env, signed);
    let written = write.then(|| write_alignment(env, &report));
    if json {
        let _ = writeln!(out, "{}", report.to_json());
    } else {
        let (ok, total) = report.alignment.counts();
        let _ = writeln!(out, "verdict   {}", report.alignment.wire());
        let _ = writeln!(
            out,
            "attested  {} ({ok}/{total} capabilities on)",
            report.alignment.attested.as_str()
        );
        let _ = writeln!(
            out,
            "tree      {}",
            report
                .tree
                .as_ref()
                .map_or_else(|| String::from("-"), |p| p.display().to_string())
        );
        let _ = writeln!(
            out,
            "program   {}@{} {}",
            report.tuple.program,
            report.tuple.program_build,
            report
                .candidate
                .as_ref()
                .map_or_else(|| String::from("-"), |p| p.display().to_string())
        );
        let _ = writeln!(out, "aterm     {}", report.tuple.aterm_build);
        let _ = writeln!(out, "key       {}", report.tuple.key());
        let (pok, pabsent, perror) = probe_tally(&report.probes);
        let _ = writeln!(out, "probes    {pok} ok, {pabsent} absent, {perror} error");
        for note in &report.notes {
            let _ = writeln!(out, "note      {note}");
        }
        if let Some(written) = &written {
            match written {
                Ok(path) => {
                    let _ = writeln!(out, "wrote     {}", path.display());
                }
                Err(why) => {
                    let _ = writeln!(err, "aterm harness align: {why}");
                }
            }
        }
    }
    match &report.refusal {
        Some(why) => {
            let _ = writeln!(err, "aterm harness align: {why}");
            ExitCode::from(1)
        }
        None => ExitCode::SUCCESS,
    }
}

fn run_caps(
    env: &Env,
    json: bool,
    signed: Option<&str>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let report = align_now(env, signed);
    if json {
        let _ = writeln!(out, "{}", report.to_json());
    } else {
        for cap in &report.alignment.caps {
            let _ = writeln!(
                out,
                "{:<16} {:<4} {}",
                cap.id,
                if cap.on() { "on" } else { "OFF" },
                cap.reason()
            );
        }
        // The `[[tool]]` rows are listed BESIDE the capabilities, in design
        // §1.2's own spelling, because a person reading "usage-hud.sheet is
        // off" needs to see which tool was absent or too old.
        for row in report.contract.iter().flat_map(|c| c.tools.iter()) {
            let id = row.probe_id();
            let found = report.probes.iter().find(|p| p.id == id);
            let _ = writeln!(
                out,
                "tool {:<11} {:<4} {}",
                row.bin,
                found.map_or("?", |p| p.status.as_str()),
                found.map_or_else(
                    || String::from("no result"),
                    |p| if p.detail.is_empty() {
                        row.caps.join(", ")
                    } else {
                        p.detail.clone()
                    }
                )
            );
        }
        if report.alignment.caps.is_empty() {
            for note in &report.notes {
                let _ = writeln!(out, "note {note}");
            }
        }
    }
    match &report.refusal {
        Some(why) => {
            let _ = writeln!(err, "aterm harness caps: {why}");
            ExitCode::from(1)
        }
        None => ExitCode::SUCCESS,
    }
}

/// `(ok, absent, error)` over a probe set.
fn probe_tally(probes: &[align::ProbeResult]) -> (usize, usize, usize) {
    let mut tally = (0, 0, 0);
    for probe in probes {
        match probe.status {
            align::ProbeStatus::Ok => tally.0 += 1,
            align::ProbeStatus::Absent => tally.1 += 1,
            align::ProbeStatus::Error => tally.2 += 1,
        }
    }
    tally
}

/// Append this verdict to the alignment sidecar (design §1.3, §3.5).
///
/// APPEND-ONLY, like the bump file the other lock-free writers use: the row is
/// built whole and written in one `write`, so a torn write costs that row and
/// never the file, and [`align::read_sidecar`] drops an unreadable row.
/// A `pending` verdict is NOT written: caching "I measured nothing" would make
/// the next pass skip the measurement.
fn write_alignment(env: &Env, report: &AlignReport) -> Result<PathBuf, String> {
    if matches!(report.alignment.verdict, align::Verdict::Pending) {
        return Err(String::from(
            "nothing was measured, so nothing was written: a cached `pending` would stop \
             the next pass measuring",
        ));
    }
    let path = align::sidecar_path(&env.state, &report.tuple.harness_build);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let line = align::sidecar_line(&report.tuple, &report.alignment.wire(), env.now);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    file.write_all(line.as_bytes())
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

/// Route one parsed command. Injected writers so the tests drive this directly.
pub fn run(cmd: &Cmd, env: &Env, out: &mut dyn Write, err: &mut dyn Write) -> ExitCode {
    match cmd {
        Cmd::Help => {
            let _ = out.write_all(USAGE.as_bytes());
            ExitCode::SUCCESS
        }
        Cmd::Hook { event, cap } => run_hook(event, cap.as_deref(), env, out),
        Cmd::StatusLine => run_statusline(env, out),
        Cmd::Install { dry_run } => run_install(env, *dry_run, out, err),
        Cmd::Uninstall { dry_run, purge } => run_uninstall(env, *dry_run, *purge, out, err),
        Cmd::Status { json, spine } => run_status(env, *json, *spine, out),
        Cmd::Mark {
            json,
            commands,
            spine,
        } => run_mark(env, *json, *commands, *spine, out),
        Cmd::Enable { caps } => run_switch(env, caps, true, out, err),
        Cmd::Disable { caps } => run_switch(env, caps, false, out, err),
        Cmd::Align {
            json,
            write,
            signed,
        } => run_align(env, *json, *write, signed.as_deref(), out, err),
        Cmd::Caps { json, signed } => run_caps(env, *json, signed.as_deref(), out, err),
        Cmd::Usage { json } => run_usage(env, *json, out),
        Cmd::Accounts {
            add,
            discover,
            write,
            json,
        } => run_accounts(env, add.as_ref(), *discover, *write, *json, out, err),
        Cmd::Switch { kind, target, json } => run_switch_now(env, *kind, target, *json, out, err),
        Cmd::Watch { passes, json } => run_watch(env, *passes, *json, out, err),
        Cmd::Config { key, value, json } => run_config(env, key, value.as_deref(), *json, out, err),
        Cmd::Limits { json } => run_limits(env, *json, out),
        Cmd::Liveness { json } => run_liveness(env, *json, out),
        Cmd::Recover {
            what,
            target,
            json,
            commands,
            spine,
        } => run_recover(
            env,
            what,
            target.as_deref(),
            Render {
                json: *json,
                commands: *commands,
                spine: *spine,
            },
            out,
            err,
        ),
        Cmd::Nudge {
            sid,
            level,
            json,
            commands,
            spine,
        } => run_nudge(
            env,
            sid,
            *level,
            Render {
                json: *json,
                commands: *commands,
                spine: *spine,
            },
            out,
        ),
        Cmd::Disk {
            targets,
            apply,
            json,
        } => run_disk(env, targets, *apply, *json, out),
        Cmd::Ledger {
            name,
            count,
            since,
            json,
        } => run_ledger(env, name, *count, *since, *json, out),
    }
}

/// The front door's entry point (`aterm harness …`).
///
/// A usage error exits 2 for every subcommand BUT the two hook ones: a hook
/// that cannot parse its own argv still exits 0, because a non-zero exit from
/// a hook command is read by Claude Code as a block on the agent's turn.
pub fn main_entry(argv: Vec<OsString>) -> ExitCode {
    let args: Vec<String> = argv
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let hook_shaped = matches!(
        args.first().map(String::as_str),
        Some("hook" | "statusline")
    );
    let (cmd, over) = match parse(&args) {
        Ok(parsed) => parsed,
        Err(e) => {
            eprintln!("aterm harness: {e}\n\nRun `aterm harness --help` for usage.");
            return if hook_shaped {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(2)
            };
        }
    };
    let mut env = match Env::from_process() {
        Ok(env) => env,
        Err(e) => {
            eprintln!("aterm harness: {e}");
            return if hook_shaped {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(2)
            };
        }
    };
    if let Some(state) = over.state {
        env.state = state;
    }
    if let Some(settings) = over.settings {
        env.settings = settings;
    }
    if let Some(nonce) = over.nonce {
        env.nonce = nonce;
    }
    if let Some(offset) = over.utc_offset_s {
        env.utc_offset_s = offset;
    }
    if let Some(config) = over.config {
        env.config = Some(config);
    }
    if let Some(tree) = over.tree {
        env.tree = Some(tree);
    }
    if let Some(against) = over.against {
        env.against = Some(against);
    }
    if let Some(build) = over.aterm_build {
        env.aterm_build = build;
    }
    if let Some(sock) = over.sock {
        env.sock = Some(sock);
    }
    let stdout = io::stdout();
    let stderr = io::stderr();
    let mut out = stdout.lock();
    let mut err = stderr.lock();
    run(&cmd, &env, &mut out, &mut err)
}

#[path = "cli_tests.rs"]
#[cfg(test)]
mod tests;
