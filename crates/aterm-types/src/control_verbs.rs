// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The control-protocol VERB TABLE: the single, typed source of truth for every
//! control-socket verb. One [`VerbSpec`] row per verb carries its op-class,
//! reply framing, targeting class, and help text — a first-sentence `summary`
//! plus the `detail` that completes it. Everything else projects from here: the
//! server maps `op` to its auth `Op` and generates BOTH help catalogs from the
//! two fields (the short one from the summaries, the full one from
//! [`VerbSpec::help_line`]); the aterm-ctl client parses replies by `framing`.
//! So the server (which produces a reply) and the client (which parses it) —
//! plus both catalogs and the router — can never disagree. Lives in `aterm-types` because
//! both binaries depend on it (alongside the `control_socket` shared-protocol
//! module).

/// Maximum schema-1 operator proposal body accepted by both the control client
/// and server. Keeping this admission bound in the shared protocol crate avoids
/// streaming a body after the server has already rejected and closed the frame.
pub const MAX_OPERATOR_PROPOSAL_BYTES: usize = 64 * 1024;

/// The byte cap of one KEYED attention entry (`meta set attention owner=<k>
/// <text>`), after trim — the server refuses a longer one (`ERR attention
/// too long (max 200 bytes)`), and a supervisor cuts its text to it. One
/// constant for both ends: the supervisor cut at the bare field's 256 while
/// the server refused past 200, and every escalation longer than that
/// reached no one (the live E2E of 2026-09-24, D2).
pub const META_ATTENTION_KEYED_MAX: usize = 200;

/// A verb's authority class. Neutral here so this crate needn't depend on the
/// server's `aterm-session`; the server maps it to its `Op`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OpClass {
    /// Pure observer / view-state control (incl. `subscribe`, the push face of read).
    Read,
    /// Injects the human input vocabulary the driven program observes, plus the
    /// app-drive verbs (`turn`/`spawn`/`close`/`tab`/`invoke`).
    Write,
    /// The out-of-band `signal` class.
    Signal,
    /// Rewrites the DURABLE on-disk config (`settings set|unset` → `aterm.toml`).
    /// A strictly greater authority than `Write` — split out so a keystroke-
    /// injection edge cannot flip a default-OFF security knob. The server maps this
    /// to `Op::ConfigWrite`, which is NOT provisioned to child edges.
    ConfigWrite,
    /// Moves the selection out of the process onto the system clipboard (`copy`, the
    /// exfil boundary). Split out of `Read` because writing the OS pasteboard leaves
    /// the process. Maps to `Op::ClipboardWrite`, NOT provisioned to child edges.
    ClipboardWrite,
    /// Owner-only privilege verb or build/meta verb — no op-class; gated separately.
    Owner,
}

/// The connection-SCOPE gate a verb needs, ORTHOGONAL to its op-class. Most verbs
/// are `Scoped` (authority is exactly the op-class, checked per target); a few are
/// gated by connection scope BEFORE/around the op check. Making this a table field
/// (rather than a hardcoded dispatch branch) keeps the table the single source of
/// truth for these exceptions too.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Access {
    /// Normal: authority is exactly `op`, checked per resolved target.
    Scoped,
    /// Non-sensitive build/meta provenance and instance posture — answered for ANY
    /// authenticated scope BEFORE target resolution
    /// (`version`/`update`/`help`/`verbs`/`privacy`). Self-scoped, no session; a
    /// selector is meaningless.
    AnyScopeMeta,
    /// Owner-only: only an OWNER-CLASS connection may run it, regardless of op-class
    /// (`sessions`/`who`/`grant`/`revoke`/`whoami`/`dial-*`) — the instance god-token,
    /// and the bridge connection, which carries Owner's power. A selector is rejected.
    ///
    /// TWO of its members then have their HANDLER tell the two owner-class scopes
    /// apart, because no table row can. `hold`: the Owner token is the local human's
    /// own credential, so it may set and lift a LOCAL halt (`origin=local`) on its own
    /// sessions, while the FLEET halt (`origin=fleet` — set, replaced, or lifted)
    /// stays reachable only from the bridge connection; that split turns on the
    /// STANDING hold's origin, which the table cannot see. `fabric`: the bridge
    /// connection is refused outright — `Scope::Owner` exactly, not owner-CLASS —
    /// because arming the supervisor chooses which process holds the bridge scope, and
    /// the bridge is that process rather than a party to arming it. For every other
    /// member the class IS the whole gate. What the class buys in all cases is the
    /// same: an edge token is kept out and a selector is rejected.
    OwnerOnly,
    /// Bridge-only: ONLY the fabric bridge connection may run it — the pair of
    /// `socketpair` ends the instance keeps when it launches its `aterm-link serve`
    /// child, served with the bridge scope PRE-RESOLVED instead of an `AUTH` line.
    /// The pinned set is THREE verbs: `deliver`, `outbox` and `outbox sent`.
    ///
    /// Strictly narrower than [`Self::OwnerOnly`], and deliberately so: Owner scope is
    /// what every in-session client already holds (`aterm-ctl @self` is Owner), so a
    /// prompt-injected agent holding Owner must not be able to forge an attested human
    /// order into a sibling's inbox (`deliver`, which stamps `from=`/`trust=`), read
    /// every session's outbound traffic (`outbox`), or release a `post --wait` for a
    /// message that never left the machine (`outbox sent`). There is no token file to
    /// steal: the only way to be the bridge is to be the process the instance spawned.
    /// A selector is rejected — every one of them names its session as an argument,
    /// like `raise`.
    ///
    /// `hold` LEFT this set when the local halt landed: the Owner token is the local
    /// human's credential and `hold=1` was always described as a human's halt, so the
    /// verb is [`Self::OwnerOnly`] now. What stayed bridge-only is the FLEET hold —
    /// `hold`'s handler refuses an Owner-issued act, `on` or `off`, against a standing
    /// `origin=fleet` hold and refuses `origin=fleet` from Owner outright — so the
    /// property this set exists for ("an injected agent holding Owner cannot lift a
    /// fleet halt locally") is enforced one level down rather than given up.
    ///
    /// A FLEET hold has NO OPERATOR UNDO. `aterm-gui`'s `fabric::apply_hold` has
    /// exactly two production callers — `hold` itself and the fail-closed
    /// `bridge_lost` — and no GUI, palette, key-binding or Owner path clears a fleet
    /// one. So a `reason=fabric-lost` hold stands until a bridge RECONNECTS and issues
    /// `hold off`; if none can (a deleted cap file, a `[fabric] command` that exits at
    /// startup), the only recovery is restarting the instance. DESIGN §11.2 says "or a
    /// human lifts it at the GUI"; that path does not exist, and this row says so
    /// rather than repeating it. A LOCAL hold is the one the owner's own `hold off`
    /// lifts.
    BridgeOnly,
}

/// How a verb's reply is framed on the wire.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Framing {
    /// A single `OK …` / `ERR …` status line (the default).
    Status,
    /// `OK <n>` header then `n` content lines.
    Lines,
    /// `OK <nbytes>` header then an `nbytes` raw byte body.
    Bytes,
    /// A push stream: `OK subscribe <n>` + a `sub <local> <sid>` map, then frames.
    Push,
}

/// What an `@<sid>` selector means for a verb (the targeting rule).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Target {
    /// Acts on the resolved SESSION (text/turn/image/send/…).
    Session,
    /// APP-level: acts on the resolved instance's FRONT window; the selector routes
    /// to the instance (window/chrome/controls/open/settings/invoke/…). `tab` and
    /// `spawn` are the exception: an explicit `@<sid>` aims them at the window
    /// HOSTING that session (`cmd_tab_aimed` / `cmd_spawn(.., Some(session))`).
    App,
    /// Self-scoped / fleet-wide; a selector is rejected (sessions/who/grant/version/…).
    Meta,
}

impl OpClass {
    /// The machine-readable name `help --json` prints.
    #[must_use]
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Signal => "signal",
            Self::ConfigWrite => "config-write",
            Self::ClipboardWrite => "clipboard-write",
            Self::Owner => "owner",
        }
    }
}

impl Access {
    /// The machine-readable name `help --json` prints.
    #[must_use]
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::Scoped => "scoped",
            Self::AnyScopeMeta => "any-scope-meta",
            Self::OwnerOnly => "owner-only",
            Self::BridgeOnly => "bridge-only",
        }
    }
}

impl Framing {
    /// The machine-readable name `help --json` prints.
    #[must_use]
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Lines => "lines",
            Self::Bytes => "bytes",
            Self::Push => "push",
        }
    }
}

impl Target {
    /// The machine-readable name `help --json` prints.
    #[must_use]
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::App => "app",
            Self::Meta => "meta",
        }
    }
}

/// One control verb, fully specified.
#[derive(Clone, Copy, Debug)]
pub struct VerbSpec {
    /// The verb keyword as typed on the socket (e.g. `text`, `turn`, `spawn`).
    pub name: &'static str,
    /// Authority class the verb needs (the server maps this to its `Op`).
    pub op: OpClass,
    /// How the verb's reply is framed on the wire.
    pub framing: Framing,
    /// What an `@<sid>` selector means for the verb.
    pub target: Target,
    /// The connection-scope gate (orthogonal to `op`): `Scoped` for the common case,
    /// `AnyScopeMeta` / `OwnerOnly` for the exceptions the dispatch used to hardcode.
    pub access: Access,
    /// The FIRST SENTENCE of the verb's help, at most [`SUMMARY_MAX_CHARS`] chars:
    /// the whole of this verb's row in the SHORT catalog (a bare `help`) and the head
    /// of its full entry. Short because the catalog an agent reads to FIND a verb must
    /// stay under [`SHORT_CATALOG_MAX_BYTES`] — with one string per verb it cost the
    /// whole of the `image`/`video`/`metrics` detail to find `text`.
    pub summary: &'static str,
    /// The rest of the entry — everything the summary does not say — or `""`. Shown
    /// by `help <verb>` and in the full catalog only. [`VerbSpec::help_line`] re-joins
    /// it after the summary with one space, so a full row whose split was a pure cut
    /// is the one-string help it came from, unchanged.
    pub detail: &'static str,
}

/// The cap on a [`VerbSpec::summary`] as a literal: the `help`/`verbs` rows quote it
/// through `concat!`, so the number an agent reads in the catalog and the number the
/// summary-length test enforces are one token that cannot drift apart.
macro_rules! summary_max_chars {
    () => {
        96
    };
}
/// The cap on a [`VerbSpec::summary`]: one readable row after the
/// [`CATALOG_TEXT_COLUMN`] gutter on a 125-column line.
pub const SUMMARY_MAX_CHARS: usize = summary_max_chars!();
/// The budget for the short catalog's rows put together (every [`catalog_lines`] row,
/// `\n`-terminated): what discovering a verb costs, whatever the table grows to.
///
/// RAISED FROM 8192 when the fabric's bridge plane completed (`outbox` / `outbox
/// sent`, DESIGN-aterm-fabric.md §11.2 as A3 settles it). The catalog was 8 170 B
/// at that moment — twenty-two bytes of headroom — so the choice was between
/// raising the number and rewording unrelated verbs' prose to make room, and
/// rewording a verb's help to fit an unrelated verb is exactly the drift a
/// generated golden exists to catch. What the budget actually bounds is the
/// SHAPE — one row per verb, first sentence only, capped at
/// [`SUMMARY_MAX_CHARS`] — and that shape is unchanged. The earlier move from
/// the design's 4 KiB to 8 KiB is the same accounting
/// (`docs/AGENT-EXPERIENCE-2026-08-26.md`, S7).
///
/// RAISED AGAIN FROM 9216 when `appnotice` landed (2026-09-10, the pull-down's
/// out-of-process voice — STATUS-SURFACE.md): the GUI's short `help` (the header
/// block plus these rows) measured 9 323 B against 9 216 with the row added, and
/// the same choice was made for the same reason. The shape is still one summary
/// row per verb under [`SUMMARY_MAX_CHARS`].
///
/// RAISED AGAIN FROM 9472 on 2026-09-11, for `fabric` and `fx` — the two verbs
/// the fabric-attach and effects-control rounds added, taking the table from 99
/// entries to 101. The GUI's short `help` MEASURES 9 610 B, so the ceiling was
/// already exceeded by the round that added them rather than by the merge that
/// carried them: `help_short_form_is_bounded_and_is_the_summary_catalog` is red
/// on the commit that landed them, with the same 9 610 against 9 472. Same
/// accounting as both raises above — the alternative is rewording unrelated
/// verbs' prose to make room, which is the drift a generated golden exists to
/// catch. The shape is unchanged: one summary row per verb under
/// [`SUMMARY_MAX_CHARS`].
///
/// RAISED AGAIN FROM 9728 on 2026-09-14, for `link` — the round-13 bridge-only
/// verb the bridge reports its broker link on, taking the table from 101
/// entries to 102. The GUI's short `help` MEASURES 9 838 B with the row; same
/// accounting as the three raises above, and the shape is unchanged.
///
/// RAISED AGAIN FROM 9856 on 2026-09-15, for round 15's three summary rows —
/// `inbox get <id> | inbox get @<off>`, `inbox seen … (and ack)` and `outbox
/// sent … [dup=1]`, 25 bytes between them, no new verb. The GUI's short `help`
/// MEASURES 9 863 B; `help_short_form_is_bounded_and_is_the_summary_catalog`
/// was red on the commits that grew them, whose runs filtered the suite to
/// `--lib fabric` and so never ran it. Same accounting as the four raises
/// above, and the shape is unchanged.
///
/// RAISED AGAIN FROM 9984 on 2026-09-17, for session identities (phase 1): the
/// `identities` row, the recut `spawn` summary (`[raise=<t|f>]` making room for
/// `[identity=<name>|-]`) and ` identity=` on the `sessions` summary, taking the
/// table from 104 entries to 105. The GUI's short `help` MEASURES 10 098 B with
/// them (10 059 B of content lines after the `OK 108 …` status line — three
/// framing lines and the 105 rows — read through aterm-ctl on the headless live
/// try of 2026-09-17), against 9 984; same
/// accounting as the five raises above - the
/// alternative is rewording unrelated verbs' prose to make room, which is the
/// drift a generated golden exists to catch - and the shape is unchanged: one
/// summary row per verb under [`SUMMARY_MAX_CHARS`].
///
/// Round 19's `story` — the presence band's write face, the one verb the round
/// adds — and the `chrome` summary that now names the presence line landed on
/// the same raise, taking the table from 105 entries to 106. The GUI's short
/// `help` MEASURES 10 238 B with both rounds' rows (10 199 B of content lines
/// after the `OK 109 …` status line — three framing lines and the 106 rows —
/// read through aterm-ctl on the headless live try of 2026-09-19): two bytes
/// under 10 240, so the next row raises it again. Same accounting, same
/// shape.
///
/// RAISED AGAIN FROM 10 240 on 2026-09-22, the raise the line above predicted.
/// Round 23's `topic` row (110 bytes: a 28-wide name, one space, an
/// 80-character summary, the newline) takes the table from 106 entries to 107,
/// and the `post` summary grew by 10 to spell `say[:<topic>]`: 120 bytes of
/// growth, and 10 238 + 120 = 10 358 is what aterm-gui's
/// `help_short_form_is_bounded_and_is_the_summary_catalog` MEASURED on the
/// wire reply. Note which test that is: the rows alone sum to 9 986 and
/// aterm-types' `summaries_fit_the_short_catalog_budget` sums ONLY rows, so it
/// holds under the old bound; the wire reply adds the `OK 110 …` status line
/// and the `#` framing lines, and only the gui test measures those. 10 496
/// leaves 138 bytes, about one more row. Same accounting as the six raises
/// above: rewording unrelated verbs' prose to make room is the drift a
/// generated golden exists to catch.
///
/// RAISED AGAIN FROM 10 496 on 2026-09-24 for the unified message system's two
/// rows (`messages`, `notice`), taking the table from 107 entries to 109. The
/// short `help` MEASURES 10 592 B on the wire (10 553 B of content lines after
/// the `OK 112 …` status line, read over the raw socket on the headless live try
/// of 2026-09-24, and the same 10 592 in
/// `help_short_form_is_bounded_and_is_the_summary_catalog`); the cap is that
/// plus 128 bytes, about one more row. Same accounting as the seven raises
/// above, and the shape is unchanged: one summary row per verb under
/// [`SUMMARY_MAX_CHARS`].
///
/// RAISED AGAIN FROM 10 720 on 2026-09-26, twice over. The audit of what aterm
/// prints at a person: seven summaries that ended on a colon, a semicolon or a
/// flourish (`update`, `flows`, `connect`, `who`, `spawn`, `subscribe`, and
/// `offscreen`'s detail) now carry the reply shape they answer with, trimmed to
/// the shortest wording that still says it. And `help`/`verbs` spell their third
/// form (`| --json`), `text` its fourth option (`[order=<logical|display>]`, its
/// summary now the wire grammar the usage line states). The short `help`
/// MEASURES 10 791 B in `help_short_form_is_bounded_and_is_the_summary_catalog`;
/// the cap is that plus about one row. Same accounting as the eight raises above,
/// and the shape is unchanged.
///
/// RAISED AGAIN FROM 10 875 on 2026-09-27, the row that raise left room for being
/// taken by `mainscreen` (the Windows daily-driver branch). `confirm` — the wire's
/// answer to the in-window close/quit question that replaced the Windows
/// task-modal dialog — takes the table one entry further, and the short `help`
/// MEASURES 10 963 B in `help_short_form_is_bounded_and_is_the_summary_catalog`
/// with both rows; the cap is that plus 84 bytes, about one more row. Same
/// accounting as the nine raises above, and the shape is unchanged.
pub const SHORT_CATALOG_MAX_BYTES: usize = 11047;

/// THE ONE `subscribe` STREAM VOCABULARY.
///
/// Frame sources first, then the two modifiers. Four hand-written copies of
/// this list existed after round 22 — the catalog row, the `ERR usage`
/// refusal, `Requested::parse`'s fold table, and the installed drive skill —
/// and the refusal spent a round telling operators that `mail` did not exist
/// in the build that had it. Everything that enumerates streams now derives
/// from here or is tested against it.
pub const SUBSCRIBE_STREAMS: &[&str] = &[
    "screen",
    "cursor",
    "events",
    "cells",
    "bytes",
    "mail",
    "sessions",
    "timestamps",
    "trim",
];
/// The column a catalog row's text starts in: a 28-wide name plus one space.
pub const CATALOG_TEXT_COLUMN: usize = 29;
/// The width a `help <verb>` entry is wrapped to.
pub const ENTRY_WRAP_COLUMNS: usize = 100;

impl VerbSpec {
    /// The verb's FULL help text: `summary`, then `detail` after one space when there
    /// is any. Where the split was a pure cut this is exactly the one-string help the
    /// two fields came from, so the full catalog reads as before the split (the golden
    /// test pins every row, the six reworded ones included).
    #[must_use]
    pub fn help_line(&self) -> String {
        if self.detail.is_empty() {
            self.summary.to_string()
        } else {
            format!("{} {}", self.summary, self.detail)
        }
    }

    /// The verb's full entry as `help <verb>` prints it: `<name padded> <text…>`, the
    /// text greedily word-wrapped at [`ENTRY_WRAP_COLUMNS`] with every continuation
    /// line indented to [`CATALOG_TEXT_COLUMN`], so a 900-char entry reads as one
    /// aligned paragraph instead of one line. Deterministic and lossless: it breaks
    /// only at single spaces (the table carries no run of spaces), so re-joining the
    /// lines' text with spaces reproduces [`Self::help_line`] exactly; a word longer
    /// than the text width stands alone on its own line, never split, never dropped.
    #[must_use]
    // Skip: iterator absent std bodies.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn entry_lines(&self) -> Vec<String> {
        let text_width = ENTRY_WRAP_COLUMNS - CATALOG_TEXT_COLUMN;
        let mut chunks: Vec<String> = Vec::new();
        let mut cur = String::new();
        let mut cur_chars = 0usize;
        for word in self.help_line().split(' ') {
            let w = word.chars().count();
            if !cur.is_empty() && cur_chars + 1 + w > text_width {
                chunks.push(std::mem::take(&mut cur));
                cur_chars = 0;
            }
            if !cur.is_empty() {
                cur.push(' ');
                cur_chars += 1;
            }
            cur.push_str(word);
            cur_chars += w;
        }
        chunks.push(cur);
        let gutter = " ".repeat(CATALOG_TEXT_COLUMN);
        chunks
            .iter()
            .enumerate()
            .map(|(i, text)| {
                if i == 0 {
                    catalog_row(self.name, text)
                } else {
                    format!("{gutter}{text}")
                }
            })
            .collect()
    }
}

use Access::{AnyScopeMeta, BridgeOnly, OwnerOnly, Scoped};
use Framing::{Bytes, Lines, Push, Status};
use OpClass::{ClipboardWrite, ConfigWrite, Owner, Read, Signal, Write};
use Target::{App, Meta, Session};

/// A `const`-friendly [`VerbSpec`] constructor for a normally-`Scoped` verb (keeps
/// the table terse; the vast majority of rows).
const fn v(
    name: &'static str,
    op: OpClass,
    framing: Framing,
    target: Target,
    summary: &'static str,
    detail: &'static str,
) -> VerbSpec {
    VerbSpec {
        name,
        op,
        framing,
        target,
        access: Scoped,
        summary,
        detail,
    }
}

/// A [`VerbSpec`] constructor with an explicit non-`Scoped` [`Access`] gate — the
/// build/meta (`AnyScopeMeta`) and owner-only (`OwnerOnly`) exceptions declare their
/// scope gate IN the table here, so the dispatch reads it instead of hardcoding a
/// verb list.
const fn va(
    name: &'static str,
    op: OpClass,
    framing: Framing,
    target: Target,
    access: Access,
    summary: &'static str,
    detail: &'static str,
) -> VerbSpec {
    VerbSpec {
        name,
        op,
        framing,
        target,
        access,
        summary,
        detail,
    }
}

/// THE TABLE. Ordered by function for a readable generated catalog. Each row's
/// help is two literals: the `summary` (its first sentence, the short catalog's
/// whole row — at most [`SUMMARY_MAX_CHARS`]) and the `detail` that completes it
/// (`""` when the summary says it all); `help_line` joins them with one space.
pub const VERBS: &[VerbSpec] = &[
    // build & meta — answered for ANY authenticated scope BEFORE target resolution
    // (non-sensitive global provenance); the `AnyScopeMeta` access declares that.
    va(
        "version",
        Owner,
        Status,
        Meta,
        AnyScopeMeta,
        "build provenance (version, commit, trustc, flavor, signature)",
        "The compiler keys: `flavor=t|r` says Trust or upstream Rust; `trust=` is Trust's OWN \
         version (the `trust:` line of `-vV`; `none` on an upstream build); `rust_compat=` is \
         the Rust release the compiler is compatible with; `trustc=`/`trustc_commit=`/\
         `trustc_host=` carry the compiler's rustc-shaped release token, commit and host — \
         renamed from rustc in v0.10.0, so a script parsing `rustc=` finds nothing. \
         `trustc=` and `rust_compat=` are the same value: Trust prints a rustc-shaped release \
         for cargo's sake, and `trust=` is where it says its own.",
    ),
    va(
        "update",
        Owner,
        Status,
        Meta,
        AnyScopeMeta,
        "self-updater [status|check|apply]: OK current_build= staged_build= outcome=",
        "Every line: enabled= current_build= commit= staged_build= staged_version= staged_commit= \
         staged_is_same_commit= (the stage is the running commit: a churn relaunch, not a source \
         change) relaunch_ready= (a newer stage exists) apply_posture= failing=<n>:<kind> \
         failing_applies= persistent= outcome=\"…\". Only when known or true: \
         stale_check=<rfc3339> or check_unrecorded=true (no completed check in 4 h / none \
         recorded), checked_at=<rfc3339> (the last completed check) next_check=<rfc3339> \
         (this process's next one), checker_stalled=<s>:<phase> checker_respawns=<n> \
         checker_deferred=<n> (the check loop's own health), apply_phase= (automatic \
         postures only), apply_policy_reason=, \
         delivery=deferred|blocked, installable=false (a disk-image, translocated or dev copy \
         never updates), apply_refusal= apply_refusal_at= apply_failure=, changelog=, and on \
         macOS freeze_seed_ms= handoff_capture_ms= (once a seamless handoff has timed its \
         capture: the first freeze rung the last dry run seeded, and the capture it timed) and \
         handoff_park_ms= (once a park has landed: what it really cost, readers stopped to \
         capture done — the floor the next first rung is budgeted from) — prose values \
         pct-encoded. apply_posture: automatic, automatic-idle, manual-config, \
         disabled-env, retry-wait, manual-only, handoff-unavailable, applying, unreconciled, \
         none, disabled, unknown. apply_phase: prefer-idle, prefer-output-gap, keys-only, land \
         — the build is running within a minute of being armed, whatever the terminal is \
         doing. Linux adds linux_installed_build= linux_staged_build= linux_trial=<phase|none> \
         (an enrolled local baseline has no trial) linux_trial_starts= linux_trial_healthy=. \
         check = one synchronous check of the channel \
         now, then the same line. apply (Owner-only) = press the staged build: macOS answers OK \
         once the request is queued and the live-session handoff keeps the shells; Linux \
         re-verifies and replaces the on-disk executable in place, no handoff.",
    ),
    va(
        "help",
        Owner,
        Lines,
        Meta,
        AnyScopeMeta,
        "help [<verb> | --full | --json]: this catalog (alias: verbs).",
        concat!(
            "Bare = one summary row per verb (first sentence, <= ",
            summary_max_chars!(),
            " chars); help <verb> = that verb's full entry, wrapped; help --full = the \
             complete catalog with the protocol header; help --json = the verb table as one \
             JSON line (name, op, framing, target, access, summary per verb)"
        ),
    ),
    va(
        "verbs",
        Owner,
        Lines,
        Meta,
        AnyScopeMeta,
        "verbs [<verb> | --full | --json]: this catalog (alias: help).",
        concat!(
            "Bare = one summary row per verb (first sentence, <= ",
            summary_max_chars!(),
            " chars); help <verb> = that verb's full entry, wrapped; help --full = the \
             complete catalog with the protocol header; help --json = the verb table as one \
             JSON line (name, op, framing, target, access, summary per verb)"
        ),
    ),
    // The macOS consent posture: instance-wide, self-scoped, and `AnyScopeMeta` for
    // the same reason `version` is — an agent that just took an EPERM has to be able
    // to ask a `--headless` instance what its permission state is. `Read`, not
    // `Owner`: every fact here is one `status` already exposes a session at a time.
    va(
        "privacy",
        Read,
        Lines,
        Meta,
        AnyScopeMeta,
        "privacy [--json]: the macOS consent posture behind EPERM",
        "Self-scoped and instance-wide: no `@<sel>` (a selector is rejected), and a `--headless` \
         instance answers it. `OK <n>` then n lines — `schema=1`, the platform, the code-signing \
         identity macOS keys a grant to (`bundle_id= signing= team= dr= grant_stable=`), \
         `full_disk_access=` with the probe that read it, the `covers=`/`uncovered=`/`unmeasured=` service \
         split, one `folder` line, the `claimants=` census, `prompt_possible=`, one `session` line per LIVE session (never \
         truncated, so `sessions_total=` always equals the number of `session` lines), then \
         `containment`, `warmup=`, `observer`, `remediate` and a closing `note`. Free text is \
         pct-encoded and `-` is unset. A background check in flight reports `full_disk_access=unknown` \
         and `probe=pending`, with no sample age (`-`, or null in JSON). Reading this verb raises NO dialog: the probe reads state \
         that already exists, and the ONE value derived from another is `covers=app-data`, which \
         follows the completed grant by Apple's documented rule and nothing else. Per-folder state \
         defaults to `unknown` BY CONSTRUCTION — the only way to learn whether a folder is \
         readable is to read it, which is the very act that raises the prompt — and a `folder` \
         value leaves `unknown` for exactly two reasons: aterm itself observed an access (its \
         own EPERM, or a warm-up the human asked for, rendered `allowed`/`denied`/`asking`/`error`), \
         or the service is one a MEASURED Full Disk Access grant covers, rendered \
         `covered-by-fda`. An observation always outranks the coverage rule, so a warmed folder \
         that answered EPERM reads `denied` even while the grant is held. A service with no \
         measurement of its own is never moved off `unknown` by the grant. `source=` names where \
         the non-`unknown` values came from — `none`, `warmup`, `fda`, or `warmup,fda`. \
         `anchor=` on the `install=` row is whether macOS can still FIND the code this \
         process's grants are keyed to: `live`, `displaced` (the bundle is on disk under a \
         different name), `deleted` (its path no longer resolves at all), `not-bundled` (a dev \
         run, not a fault) or `unknown`. `displaced` and `deleted` mean NO grant can be \
         validated against this process — every consent decision for it and for the sessions \
         it spawned falls back to asking — and they earn their own `note` row, because \
         `running=` is the exec-time path, frozen for the life of the process, and so reads healthy in exactly that state, and \
         `full_disk_access=granted` can be true at the same moment and mean nothing. \
         `claimants=` is the one row about the TCC record rather than this process: macOS \
         keeps ONE code requirement per bundle id, and a copy that does not satisfy it \
         REPLACES it when it asks, resetting the grant for every copy. It reads \
         `claimants=<n> conflicting=<n> census=<complete|partial|unavailable> sole=<yes|no> \
         age_ms=<n>`, then a `claimant path= signing= dr= team= conflicts=yes` line per copy \
         whose requirement differs (at most 8; the counts stay true) and one `note`. A copy \
         sharing the running requirement is counted, not listed. Candidates are the running \
         bundle's folder, /Applications, ~/Applications and every copy LaunchServices has \
         registered, except a copy in the Trash; `partial` means one of them could not be \
         read, and only `complete` may say `sole=yes`. `claimants=-` with `census=pending` is \
         a first census still out; `census=off` was never asked (headless, non-macOS, privacy \
         disabled, or no readable bundle id). The verb only reports; it never moves a bundle. \
         Settings ▸ Security lists the conflicting copies and offers to move an ad-hoc or \
         unsigned one to the Trash: an owner press confirmed in an alert, which `act` \
         refuses. \
         `unavailable` on an \
         `observer` row is a THIRD value, distinct from `off` and from `false`: the observer \
         could not be consulted, which is not the same as its having answered no. \
         `full_disk_access=granted` removes this class of interruption for the folders that \
         grant covers, and establishes exactly two things: `fda_scope=this_process` and \
         `app-data` coverage for THIS host (`covers=app-data`) — MEASURED 2026-09-19 on macOS \
         26.6.2, not merely documented: with the grant held, four other-app data locations read \
         with zero dialogs and no `tccd` traffic at all. That is the prompt owners actually hit \
         (`\"would like to access data from other apps\"`), and it is the ONLY durable answer to \
         it: macOS records an app-data allow against one process instance (a pid / pid_version / \
         boot-session triple) and ships no System Settings switch for the class, so answering \
         the dialog cannot hold across a restart. Every other measurable service stays \
         `unmeasured`; `uncovered` is the never-covered class — cloud-storage File Provider \
         domains, the media and photo libraries, and App Management, each its own service with \
         its own Settings pane that this grant does not reach. An adopted session inherits \
         nothing. Only a human can change any of this — aterm cannot grant it, and neither can \
         you.",
    ),
    // screen / terminal state
    v(
        "text",
        Read,
        Lines,
        Session,
        "text [--json] [trim] [tail=<n>|rows=<a>-<b>] [order=<logical|display>]: the visible screen.",
        "trim drops the trailing all-blank rows: the header becomes `OK <n> trimmed=<k>` with n = \
         the rows actually sent (interior blanks stay, so row i is still screen row i); `--json` \
         adds \"trimmed\":k and keeps dims.rows = the grid. Off by default (scripts count rows). \
         `tail=<n>` sends only the LAST n rows of the grid (n >= 1; more than the grid has is the \
         grid) and `rows=<a>-<b>` only the inclusive 0-based span a..b, clamped to the grid — a \
         span with no row on the grid, or b < a, is `ERR bad rows`. Whenever the reply does not \
         start at row 0 the header adds `first=<row>` (`--json`: \"first\":row, after trimmed) so \
         reply line i is screen row first+i; `trim` then drops the trailing blank rows of the \
         SLICE. WHY: a bottom-pinned TUI (Claude Code keeps its composer on the last row of a \
         63-row grid) never has a blank tail, so `trim` drops nothing and every look costs the \
         whole grid — `text tail=20 trim` is the cheap read there. The options go in any order \
         after --json. `order=display` lays each row out as the frame PAINTS it (a \
         right-to-left run reversed in place, the renderer's own permutation) instead of the \
         logical order a driver sends against (`order=logical`, the default); `--json` then ends \
         with \"order\":\"display\",\"display_map\":[…], per row the visual→logical column map \
         (map[v] = logical column drawn at visual column v), null where the row did not move. \
         Any other argument is `ERR usage: text [--json] [trim] [tail=<n>|rows=<a>-<b>] \
         [order=<logical|display>]` — nothing is silently ignored. `--json` always carries \
         \"gen\":\"<epoch>.<seq>\" after seq: the screen generation of the rows sent, read \
         under the same lock (`status gen=`), which a fenced `key if-gen=` names — so a press \
         is bound to the read it was decided on, not to a later one — and \"human_ms\":<ms|null> \
         after gen (`status human_ms=`: how long ago a person last gave the session input, \
         null for never), before trimmed and first, which come last but for \
         `order=display`'s \"order\" and \"display_map\", which end the object",
    ),
    v(
        "screen",
        Read,
        Lines,
        Session,
        "the full styled grid as one lossless JSON frame",
        "",
    ),
    v(
        "line",
        Read,
        Status,
        Session,
        "line <n>: one physical line of scrollback+screen",
        "",
    ),
    v(
        "lines",
        Read,
        Status,
        Session,
        // The optional field rides in the detail: the full row reads exactly as
        // one string would, and the short catalog (at its budget) stays as it was.
        "OK <scrollback-line-count>",
        "[truncated=<lost>]",
    ),
    // LINES framing, not `lines`'s Status: the reply is `OK <n> …` + n rows, and the
    // client picks its framing from this row (`framing_of`), so a Status row would
    // make `aterm ctl offscreen` print the header and drop every row.
    v(
        "offscreen",
        Read,
        Lines,
        Session,
        "offscreen [since=<i>] [tail=<n>] [max=<n>] [screen=1]: rows a TUI scrolled off",
        "— Claude Code and other fullscreen apps repaint the ALTERNATE screen in place, so \
         `lines` stays 0 and a row that leaves the top is gone from the grid; aterm keeps those \
         rows in memory, always on (4 MiB shared by every session of the process, at least \
         256 KiB each), by comparing each committed frame (a DEC 2026 close; at most one per \
         16 ms for an app that sends \
         none) with the one before. Reply `OK <n> first=<i> last=<j> lost=<k> breaks=<b> back=<d> \
         [back_at=<i> pin=<p>] epoch=<e> origin=<o> alt=<0|1> seq=<s> [enabled=0] [more=1] \
         [screen_rows=<m>]` then n lines, oldest first: line m is archived row first+m, and last= \
         is the reply's own last row. since= is exclusive, so page and poll with since=<last>; \
         since=<origin>:<i> (a `history` arch= mark) from another origin reads from the start and \
         origin= shows it; an index past the newest is `ERR bad since`. An aterm self-update \
         hands the archive to the new instance with its origin, so marks go on reading: it \
         carries the rows after the marks of the last 8 submitted turns, and at least the \
         running app's last 8 screens (without a turn, the newest), up to 1 MiB (older ones \
         count in lost=); a handoff that cannot carry it starts a new origin, and a resize as \
         the new instance takes over is a breaks= gap that loses nothing. At most 2000 rows per \
         reply, or max=<n>; tail=<n> takes the newest n instead; more=1 says rows were left out. \
         lost= counts rows after since that were evicted or wiped, breaks= the discontinuities at \
         or after since (a redraw with no overlap, a scroll-back past the oldest row kept — its \
         rows come again after the gap — a resize, leaving the alt screen, a reset), \
         back= how many archived rows the screen shows again: screen rows pin..pin+back are \
         archived rows back_at.. (pin= header rows above them). enabled=0: the archive is off, so \
         nothing that scrolled away was kept. screen=1 appends the current screen rows, read \
         under the same lock: n counts them too, and they are the last screen_rows=<m> lines. \
         Anything else is `ERR usage: offscreen [since=<i>] [tail=<n>] [max=<n>] [screen=1]`; \
         there is no --json form",
    ),
    // Write, not Read: it moves the screen the driven program's next byte lands
    // on, which is not view state — a read-only edge must not be able to pull a
    // running full-screen app off its screen. No PTY byte, so not `feed`.
    v(
        "mainscreen",
        Write,
        Status,
        Session,
        // The summary is the short catalog's whole row, which is at its budget
        // (`SHORT_CATALOG_MAX_BYTES`): the reply shape and no more.
        "leave a dead app's alt screen: OK left=<0|1>",
        "— a full-screen app killed from outside (Stop-Process, kill -9) never sends `?1049l`, so \
         the tab keeps its last screen: `lines` reads 0 and the shell paints onto it. A shell \
         that sends OSC 133 marks leaves that screen by itself at its next prompt; this is the \
         recovery for one that sends none (cmd.exe). `OK left=1`: the dead screen, with anything \
         typed onto it, is kept in `offscreen`, and the primary screen and its scrollback are \
         back (under Windows ConPTY the primary rows are all in the scrollback and the screen \
         starts blank, with the cursor where the console has it). `OK left=0`: the primary \
         screen was already up and nothing changed. Anything after the verb is `ERR usage: \
         mainscreen`",
    ),
    v(
        "cell",
        Read,
        Status,
        Session,
        "cell <r> <c>: OK <grapheme%enc> <fg> <bg> <attrs>[ link=]",
        "",
    ),
    v(
        "cursor",
        Read,
        Status,
        Session,
        "OK <row> <col> <visible> <style>",
        "",
    ),
    v(
        "dims",
        Read,
        Status,
        Session,
        "OK <rows> <cols> <px_w> <px_h>",
        "",
    ),
    v(
        "modes",
        Read,
        Lines,
        Session,
        "DEC modes: alt_screen=, cursor_visible=, ..., focus_reporting=, sync_output= (14 keys)",
        "",
    ),
    v(
        "title",
        Read,
        Status,
        Session,
        "OK <title> (from shell integration)",
        "",
    ),
    v(
        "cwd",
        Read,
        Status,
        Session,
        "OK <cwd> (from shell integration)",
        "",
    ),
    v("colors", Read, Status, Session, "OK fg= bg= cursor=", ""),
    v(
        "search",
        Read,
        Lines,
        Session,
        "search <pat> [case] [regex]: full-history find, one \"<row> <col> <len>\" per match",
        "(a hit straddling a SOFT WRAP is one match whose col+len runs past the grid width \
         and continues at column 0 of the next row; regex ^ and $ bind to the reader's \
         LOGICAL line, so a continuation row has no ^ of its own)",
    ),
    // `find` DRIVES the find bar; `search` above ANSWERS the same question without
    // one. Both are `Read`, and the reason is the op-class boundary rather than the
    // absence of mutation: find mode exists to DIVERT keystrokes away from the PTY,
    // so not one byte of a typed query reaches the driven program. What it moves is
    // the viewport and the highlight — precisely what `scroll` and `select` move,
    // and they are `Read` for the same reason. It reports the match position, which
    // is what `search` (`Read`) already answers, so a read edge learns nothing here
    // it could not already ask for. Classifying it `Write` would have been the real
    // escalation: `Op::WriteInput` and `Op::ReadScreen` are independent (a `push`
    // connection carries write WITHOUT read), so a keystroke-only edge could have
    // typed a query and read match positions back off a screen it may not read.
    v(
        "find",
        Read,
        Status,
        App,
        "find [open|type|key|next|prev|case|regex|accept|cancel|status]: drive the FIND BAR",
        "- the bar ⌘F opens on macOS and Ctrl+Shift+F opens elsewhere, which no other verb \
         reaches: `key` and `send` land beneath the window's shortcuts (`key ctrl+f` types ^F \
         into the shell), and find mode diverts keystrokes AWAY from the PTY. Forms: `find \
         open` = the Edit ▸ Find… path itself; `find type <text>` inserts at the caret; `find \
         key <name>` runs one field edit, `<name>` one of back, delete, left, right, word-left, \
         word-right, home, end, kill-start, kill-end, kill-word-back, kill-word-forward; `find \
         next`/`find prev` step matches (^S/^R, wrapping); `find case`/`find regex` TOGGLE the \
         case and regex flags (⌥⌘C/⌥⌘R on macOS, Alt+C/Alt+R elsewhere; read the reply to \
         learn the new value — there is no set form because the keystroke has none); \
         `find accept` = ⏎ (exit, stay on the match), `find cancel` = ⎋ (exit, restore the \
         pre-find viewport); `find status` mutates nothing. Every form answers the SAME line: `OK \
         open=0` when no find bar is up, else `OK open=1 query=<pct> case= regex= regex_error= \
         matches=<n> current=<i> row= col= len= truncated= stale=`. row/col/len are the CURRENT \
         match in SELECTION coordinates — row 0..rows is the live screen, a NEGATIVE row is \
         scrollback above it — which are NOT `search`'s rows: `search` emits ABSOLUTE rows \
         (0-indexed from the oldest retained line, never negative) that feed `line`/`text` \
         directly, and a `find` row fed to `line` reads the wrong line or is refused. They are \
         `-` when there is no match — \
         never a position that does not exist. current/matches are the 1-based `i/n` the find bar \
         itself paints, so the wire and the glass always agree; `truncated=1` means the search \
         index capped the batch, so that pair counts within the cap rather than over the whole \
         history, and `stale=1` means the terminal has CHANGED since that pair was counted (the \
         find bar paints the same fact as a trailing `…`): output is not re-searched per PTY \
         batch, so the count is a past census and can be wrong in either direction until the next \
         edit or `find next`/`find prev`. A form that types or steps while the bar is CLOSED changes nothing and answers \
         `open=0`; it can never fall through to the PTY. FRONT window of the resolved instance, \
         like `hover`",
    ),
    v(
        "selection",
        Read,
        Lines,
        Session,
        "OK <n> + the selected text",
        "",
    ),
    // `copy` is the EXFIL BOUNDARY: it moves the selection OUT of the process onto
    // the system clipboard, so it is `ClipboardWrite`, not `Read`. `scroll`/`select`
    // (below) stay `Read` — viewport nav is part of reading and nothing leaves the
    // process. A read-only edge can pan and select but cannot exfiltrate.
    v(
        "copy",
        ClipboardWrite,
        Status,
        Session,
        "copy the selection to the system clipboard, OK <bytes>",
        "",
    ),
    v(
        "blocks",
        Read,
        Lines,
        Session,
        "shell-integration command blocks (exit codes, command text, state)",
        "",
    ),
    v(
        "blocktext",
        Read,
        Lines,
        Session,
        "blocktext <id> [trim]: one command block's output",
        "trim drops the trailing all-blank rows (`OK <n> trimmed=<k>`, n = the rows sent); any \
         other tail is `ERR usage: blocktext <id> [trim]`",
    ),
    // seeing: pixels, recording, history
    v(
        "image",
        Read,
        Status,
        Session,
        "image [path] : capture the APPLICATION-RENDERED CLIENT FRAME -> PNG, reply OK <w> <h> \
         <path>",
        "(bare filename confined to the runtime images/ dir; auto-named when omitted). In a \
         window the frame is bound to a successful application-present transaction; headless is a \
         semantic-renderer artifact. Platform compositor visibility and scanout are not observed. \
         image --bytes = return the PNG base64'd OVER THE WIRE (OK 1 + `<w> <h> <nbytes> \
         <base64>`) instead of a server-local path — the form a REMOTE (dial/TLS) driver uses. \
         image --meta = opt into an additive captured-frame pixel fingerprint plus \
         terminal/native/composite phase, raster, paint, geometry, theme, and per-leaf metadata \
         (existing replies stay byte-for-byte unchanged; with --bytes the reply is OK 2 + \
         metadata and PNG rows). image plain = bare pixels; image read = inline OSC-1337 images \
         as base64. @<sid> captures the app-render frame of the window showing that session",
    ),
    v(
        "window",
        Read,
        Status,
        App,
        "window [<target>] [path] : assemble a full-window artifact",
        "(platform-owned chrome + the exact submitted client destination) -> PNG, reply OK <w> \
         <h> <path> (target: front | prefs | about | menu | tab-menu | conn-card | \
         session-picker | connections | update, default front — aliases settings, palette and \
         software-update resolve too; bare filename confined to images/). Every target but \
         front is photographed in the front window, so one it is not showing (a closed \
         overlay, Settings in a background tab, about or update while Settings shows another \
         page) is refused, never answered with a plain front frame: `ERR <target> is not open \
         in the front window; …` and the step that opens it: `open <target>`, or `invoke \
         ConnectToSession` for session-picker and `invoke ConfigureConnection` for conn-card \
         (the front session needs a connection). Compositor visibility and scanout are not \
         observed",
    ),
    v(
        "video",
        Read,
        Status,
        App,
        "record N seconds (0.5..=60) of the front window's WSI-SUBMITTED destination frames",
        "-> frame_NNNN.png + index.json (same-clock timestamps; compositor visibility and scanout are not observed). Flags: full | keys (owner-only keystroke log: hardware input, plus socket input aimed at the tab ON SCREEN — `key`, `ctrl`, `send`, `feed`, `paste`, flagless OR an explicit `@<sid>` naming the front tab — each stamped on the frame clock. A verb aimed at a BACKGROUND session egresses on the control thread and CANNOT be logged (`@self` expands to that when the driving session is not front), and input that lands on a WINDOW this take is not capturing (the front window changed mid-take — an `aterm ctl spawn` alone does it) has no frame here that could answer it; those attempts are COUNTED instead and reported as `unlogged_inputs=` on the reply line, live as `unlogged=` on `video status`, and in index.json meta (with the window share broken out as `unlogged_other_window`), so an empty inputs[] is never ambiguous and a logged row is never a key the recorded window never saw. Drive the FRONT tab when you need key->frame latency) | pace (keep redraws flowing) | trail (the cursor effect's admission verdicts — the `trail` verb's ring rows — logged on the frame clock into index.json trail[], with trail_requested/trail_logged/trail_lost in meta and `trail= trail_lost=` on the reply; frame.t_us - verdict.t_us is the verdict->glass delay) | fps=<n> (cap capture rate, 1..=120) | budget=<MiB> (frame-store RAM, 64..=4096, default 512). Every recording carries >=1 baseline keyframe; retention converges to 8 eligible completed recordings while preserving fresh/live handoffs. `video status` = one-line read of the in-flight recording (recording= mode= elapsed_ms= frames= resized= keys=, and for a keys take the RUNNING inputs= unlogged= so a driver learns mid-take that it is driving an unloggable path); `video stop` = finalize it now — owner-only, like `keys`, because it truncates the take. `video frames [count=N]` = no capture; list the newest recording's N highest-delta (most-changed) frames as `frame n= delta= t_us= seq= <path>` rows, so an AI pulls just the eventful key frames instead of every PNG (default 8, max 64). index.json meta reports honest coverage: head_truncated/evicted_frames/ring_skipped/covered_us vs requested_ms, plus keys_requested/inputs_logged/unlogged_inputs. key->captured-frame latency = first recorded submitted destination containing the glyph minus inputs[].t_us (an inputs[] row is `ch` for a character or `key` for a named key like ArrowUp/Escape — NOT key->photon and NOT comparable to a keystroke-sampled latency: the recorder can only see a key's effect on its NEXT captured frame, so at fps=N no reading goes below ~1000/N ms however fast the terminal is. index.json `analysis` publishes that floor with every number (per row capture_floor_ms/at_capture_floor; per take capture_floor_p50_ms, capture_interval_p50_ms, attempts_outpace_readings, capture_verdict) — use `metrics percentiles` input_p50/p95/p99_ms when you want typing latency); cadence gaps = frames[].t_us deltas vs ~16667, and a paced or headless present-real take also publishes ticks[] (every instant the loop's redraw PROVED the screen unchanged) so an unchanged screen's gaps read as quiet, not as missed samples, while a tick that neither presented nor proved it (a stall) stays a hole",
    ),
    v(
        "appstatus",
        Read,
        Lines,
        App,
        "OK <n> + one `activity` row per live/finished app-initiated job",
        "What aterm has been doing on its own initiative — a toolchain install, a self-update download — as the message band shows it, plus the finished jobs the ring still remembers, and the agent harness's notes (kind=harness, always phase=done: a harness note is a record, never a row). Rows are `activity kind=<toolchain|update|harness> phase=<live|done> progress=<pct>/100|- title=<t> detail=<d> stats=<s> outcome=<ok|warn|-> [since_ms=<ms>]` — progress= is `-` on every phase=done row (and on a live row with no fill yet), and stats= is empty on done rows; live rows first, then finished oldest-first. Free text is percent-encoded. Read-only: it starts and cancels nothing.",
    ),
    // `appnotice` is the WRITE face of the same surface: a record made from OUTSIDE
    // the process. Owner-only (a child edge must not be able to forge "Claude Code …
    // is up to date" onto the record), Write op-class because it mutates app state.
    va(
        "appnotice",
        Write,
        Status,
        App,
        OwnerOnly,
        "appnotice <toolchain|update|harness> <text>: record a text in the message log",
        "The out-of-process voice of the status surface: an `aterm pkg install claude` run in a \
         terminal (no GUI child to stream markers) says what it did in the same record the \
         GUI's own passes write. On the `toolchain` lane a text of the shape `managed-current: \
         …` or `machine-settings: …` is taken as that atpkg marker is; the `harness` lane (an \
         agent harness's notes, posted from outside the window; aterm's own supervisor runs \
         inside it and posts none) is an `appstatus` entry of kind=harness; every text on every \
         lane is recorded in the message log (Settings ▸ Messages, `appstatus`) and never \
         raises a row; replies `OK recorded`. Owner-only: only the instance token may write to \
         the record.",
    ),
    // `messages` is the unified message system's READ face: the log the band and
    // Settings ▸ Messages show, one engine-built row per record (design rulings
    // 163-198). `appstatus` stays its toolchain/update/harness projection.
    v(
        "messages",
        Read,
        Lines,
        App,
        "messages [<n>] [since=<id>] [tag=<tag>] [sev=<sev>] [live]: the message log, one row each",
        "Rows ascend by id: `message <id> at=<unix_ms> ago_ms=<ms> tag=<tag> \
         sev=<success|info|warn|error> origin=<host|wire|carried> \
         state=<held|live|folded|stale|unseen|superseded|resolved-ok|resolved-warn|dismissed|answered|evicted|carried|withdrawn|recorded> \
         glass=<row|-> rep=<n> key=<k|-> title=<t> detail=<d> actions=<a|-> \
         [progress=<n>/100|level=<n>/100|busy=1] [load=<network|disk|cpu|memory>] \
         [since_ms=<ms>]`. level= is a measured level (the system.strain row only), never \
         progress. A bare <n> keeps the newest n (default 64, max 512); since=<id> keeps the OLDEST n above the id \
         (default all), so paging with the last id you saw never skips a row; tag= filters; sev= is a floor (sev=warn is warn and error); \
         live keeps unretired rows. at= is the wall clock at ingress; since_ms= is on rows \
         retired by this process only. glass= is the band row (0 = top) or -. detail= is the \
         lines joined by a newline, actions= the button labels joined by a comma. Free text is \
         percent-encoded. Read-only; `appstatus` is its toolchain/update/harness projection.",
    ),
    // `notice` is its WRITE face: a script's records, failures and work in flight
    // under the attention rule. Owner-only (a child edge may not raise, end or
    // press a row), Write op-class because it mutates the band.
    va(
        "notice",
        Write,
        Status,
        App,
        OwnerOnly,
        "notice <post|progress|done|dismiss|act> \u{2026}: record, show work in flight, end or press a message",
        "Owner-only. `notice post <tag> [sev=<sev>] [key=<key>] [hold=<1..3600>] <title>[ -- \
         <detail>]`: sev=success or info (the default) is a RECORD in the message log, never a \
         row (`OK recorded message=<id>`); sev=warn or error is a row on the band (`OK \
         message=<id>`). `notice progress <key> [tag=<tag>] [pct=<0..100>|done=<n>/<total>|busy] \
         [unit=<bytes|items|steps>] [load=<network|disk|cpu>] <title>[ -- <stats>]`: one \
         live row per key with the full-width meter (pct= or done= fill it; busy, the default, is \
         the moving highlight and shows the elapsed time after 10 s), and with done= how long is \
         left; a row appears after 2 s, so a quick job never flashes; send the line again to \
         move it (an identical line costs nothing and keeps the row from going stale after 120 \
         s). `notice done <key> [ok|warn|withdraw] [<words>]` ends it with its finish (`OK \
         done=<id> how=<resolved-ok|resolved-warn|withdrawn>`, or `OK done=- how=gone`); warn is \
         a brief flash, and a failure that must stay is `notice post <tag> sev=error key=<key> \
         <title>`, which takes the row's place. `notice dismiss <id>` takes a row down; `notice \
         act <id> <label|index|details>` presses its button as a person would (`OK \
         acted=<label> performed=<1|0>`). A band title is at most 6 words and 48 characters with \
         no clause; a key is 1-40 of a-z 0-9 . _ - and lives as wire.<key>; the toolchain, \
         update and harness tags are aterm's own; at most 3 wire rows are live (the band's \
         three), 60 new messages and 16 KiB of words a minute, 10 presses a minute (`ERR busy \
         notice: \u{2026} retry_ms=<ms>`). No button can be authored from the socket.",
    ),
    // `story` is the WRITE face of the PRESENCE band (round 19): a watcher's
    // decision, told to the session's window. Owner-only (a child edge must not
    // write `✓ approved` onto a band it does not drive), Write op-class because it
    // mutates App state, Session target because it names the session it is about.
    // `chose` (2026-09-24, the question-policy contract v3) joined the set for the
    // in-window supervisor answering Claude Code's question box; the eight words
    // no longer fit the summary cap, so the summary names the slot and the detail
    // leads with the set.
    va(
        "story",
        Write,
        Status,
        Session,
        OwnerOnly,
        "story <verb> [<text>]: tell the window what a watcher decided or the harness chose",
        "The verb is the CLOSED SET \
         `approved|dismissed|reconnected|timeout|exit|compacted|warned|chose` — anything else \
         is `ERR usage` — and the text is optional, at most 96 bytes, no control byte. The \
         presence band under the tab bar says who is driving a session and what happened while \
         the human was away; a watcher's decisions (`aterm drive watch`: an approved read, a \
         dismissed survey, an outage ridden out, its TIMEOUT or EXIT, the worker's compaction \
         and the context warning before it) live in another process and reach the band only \
         here. `aterm drive watch` posts one per journaled decision by itself. The point takes \
         the phase slot for three seconds (`✓ approved`, then back to the phase), is spoken to \
         a screen reader as `approved by watcher`, and is a story point: the tab's violet dot \
         until the human looks, and approvals and choices are counted in the `◇ quiet` \
         summary. `chose` is the harness's supervisor answering Claude Code's question dialog \
         by policy (`[harness] answer_questions`, or the session's `meta set questions`), its \
         text the policy word (`◆ chose recommended`, spoken `chose by harness, recommended`): it also \
         plays one short quiet chime (`choice_sound`, under the Music effects master, silent \
         in serious mode, at most one per 2 s, a background tab's too) and pulses the rim of \
         each window whose front tab is that session once (none under reduced motion or with \
         the presence rim off). Never send the command: the band carries no command text, and \
         the watcher tells an approval bare. Reply `OK story=<n>`, the point's seq (`status \
         story=` reports it). Takes the ordinary `@<sid>` selector; bare, it is the \
         connection's own session. Owner-only.",
    ),
    v(
        "chrome",
        Read,
        Lines,
        App,
        "the front window's native UI (toolbar + menu bar) and its presence line",
        "The last line is `presence rim=<none|drive|wait|stop|stop-hold> level=<quiet|note|story|\
         driving|driven|attention|limited|hold> band=\"<row>\" sentence=\"<spoken>\"` — what \
         the human sees of the presence band under the front window's tab bar: the rim's \
         colour state, the severity, the band row's six slots fitted to the window's width \
         (role, phase and since, hand, mail, `ctx <n>% left`, fabric — two spaces between slots; \
         `\"\"` when the row is folded) and the sentence a screen reader gets. Both quoted values escape `\"` and \
         `\\`. The band never carries command text, a mail body, an OSC title or a limit \
         message, so neither does this line. Off macOS there is no menu bar: the `menu \
         \"<title>\"` lines are the app's menu model, less the rows this platform cannot act \
         on (the two update rows where there is no updater); `controls menu` is the live \
         palette, with each row's enabled state. Read-only.",
    ),
    v(
        "controls",
        Read,
        Lines,
        App,
        "GUI controls as text (the nine `window` targets, default front)",
        "Targets: front | prefs | about | menu | tab-menu | conn-card | session-picker | \
         connections | update — the roster the verb's own usage error enumerates. `controls \
         connections` is Owner-escalated; the rest answer at any scope. On Windows `front` \
         ends with `confirm open=<true|false>`: a close or quit waiting on an answer in the \
         window, `by=` who asked. When a control client asked, `answer=\"confirm yes\"` goes \
         ahead and `cancel=\"confirm no\"` keeps it; when a person asked, both are `-` and \
         the person answers it.",
    ),
    v(
        "panes",
        Read,
        Lines,
        App,
        "the front window's ACTIVE-tab split-pane layout: `layout tab=<i> panes=<n> zoomed=<bool>`",
        "header, then one `pane session=<sid> rect=<row_off>,<col_off>,<rows>x<cols> \
         focused=<bool>` row per visible pane (cell coords; 1-cell divider gaps between rects). \
         @<sid> describes the window whose ACTIVE tab displays that session; when none does \
         (background tab, or no window) the reply is `OK 0` with the header's panes=0 — an \
         honest empty layout, never an ERR, so poll loops need no error arm for it",
    ),
    v(
        "inspect",
        Read,
        Lines,
        App,
        "versioned native tab-app semantics: inspect app/v1 tabs",
        "| inspect app/v1 view <view-id> <text|controls|tree|audit>. `tabs` prints each tab's \
         `index=` 0-BASED — the number `tab <N>` / `tab close <N>` take (the strip's painted \
         ordinals count from 1)",
    ),
    v(
        "cast",
        Read,
        Bytes,
        Session,
        "asciicast v2 recording (compact, sendable);",
        "`cast frames [count=N]` expands it to a keyframe flipbook",
    ),
    v(
        "temporal",
        Read,
        Bytes,
        Session,
        "temporal [status|<tick>] [trim]: the screen reconstructed at a past instant",
        "(needs temporal_recording=true; `temporal status` reports the reachable tick range). trim \
         drops the trailing all-blank rows — byte-framed, so `OK <nbytes> trimmed=<k>` counts the \
         trimmed body; `status` has no rows, so `status trim` is `ERR usage`",
    ),
    v(
        "history",
        Read,
        Lines,
        Session,
        "history [<n>] [since=<id>]: the turn LEDGER",
        "- id/submitted/status/dur_ms/seq/hash/arch/text per completed turn; arch=<origin>:<last> \
         is the alt-screen archive's mark when the turn started, for `offscreen since=`. An aterm \
         self-update carries the ledger and the id count to the new instance when it can: ids \
         keep rising, and the carried records print carried=1 before text= (their started_ms and \
         seq are the old instance's)",
    ),
    // `meta` reads/writes the USER-settable session metadata. Base op-class Read
    // (the bare form is a pure metadata readout); the `meta set`/`meta unset`
    // sub-forms are WRITES and are escalated to WriteInput by the server's
    // argument-aware `escalated_op` seam (session-scoped state like `lease` —
    // NOT ConfigWrite: nothing durable on disk is rewritten).
    v(
        "meta",
        Read,
        Status,
        Session,
        "meta -> OK title= user_title= description= icon= role= attention= cwd= state=",
        "attention_owner= attention_owners= supervisor= questions= agent_cwd= (pct-encoded; '-' = unset; \
         read fields by key). `meta set <title|description|icon|role|attention|questions> \
         <text...>` / `meta unset <field>` set or clear the USER metadata (write-gated; user title outranks \
         the OSC title in tab labels; `role operator` names the fleet operator and a non-empty \
         `attention` is the typed needs-human escalation the menu-bar status item badges; caps: \
         title 120B, description 1024B, icon 64B, role 64B, attention 256B). KEYED ATTENTION: \
         `meta set attention owner=<k> <text...>` / `meta unset attention owner=<k>` write only \
         owner <k>'s entry (<k>: one token of 1..64 bytes from [A-Za-z0-9._:@/-]; text 200B), so \
         a supervisor and a human raise and clear their own escalations independently; the bare \
         form is owner `-` and clears only its own. At most 8 owners, one slot kept for the bare \
         owner (a new keyed owner past 7 is `ERR attention owners full`); `attention=` and every reader (menu bar, tab chrome, \
         `sessions meta=1`, the `meta` event) show the MOST RECENTLY SET entry, and \
         `attention_owner=` names its owner (`%2D` = the bare owner) of `attention_owners=` \
         holding one. Only the bare entry survives an update's restore — a keyed owner \
         re-asserts its own. SUPERVISOR: `meta set supervisor <holder> [ttl=<ms>]` / `meta unset \
         supervisor [holder=<holder>]` (Owner-only; an edge gets `ERR denied`) shows a running supervisor as \
         `supervisor=` in `meta`, `status` and `sessions`. Without ttl= the claim lasts while \
         the connection that set it keeps serving requests (it closes, or turns into a \
         subscribe stream: cleared); with ttl=<1..600000> it is a lease that lapses unless \
         re-set — the spelling for one-request-per-connection clients; at the lapse the claim \
         is removed with a `meta-change field=supervisor value=-` event and a box it was \
         holding reaches the menu bar and the notification. While a claim is live the agent's \
         own prompts, questions and walls (every `wall:<kind>`) raise no menu row or \
         notification: the supervisor answers them or escalates with `meta set attention \
         owner=<k>`. Another holder's live \
         claim is `ERR busy supervisor=<holder>`; the same holder renews. A bare unset clears \
         whichever holder's claim stands; `holder=<holder>` clears it only while that holder \
         holds it (`OK` either way), so a supervisor giving its own claim back never clears \
         another's. QUESTIONS: `meta set questions ask|recommended` / `meta unset questions` \
         (Owner-only; an edge gets `ERR denied`) set how the in-window supervisor treats this \
         session's Claude Code question dialog, over the `[harness] answer_questions` switch: \
         `ask` leaves it to a person (how a controlling session takes a worker's questions), \
         `recommended` answers it with its recommended option. A closed set, case-folded and \
         stored lowercase — any other value is `ERR usage: meta set questions \
         ask|recommended`; `questions=-` is unset (the switch decides); it counts toward `sessions meta=1` and survives a \
         restore and an update's handoff. AGENT_CWD: the foreground program's own working \
         directory as the OS reports it (`-` with no foreground program or none readable) — \
         where an agent was launched, which a shell with no OSC 7 leaves `cwd=` without. \
         No `window=` here, \
         for the reason `status` gives: `meta` is polled and the window lives on the main thread \
         — ask `sessions`/`ls` (one hop for the whole fleet) or `dims`",
    ),
    // `status` is the READ-ONLY Subject+Status record (RFC: Tab Subject &
    // Status §8) — what a session IS and what it is DOING, classified entirely
    // locally. Versioned because a later interpretation tier consumes the same
    // record; there is no write sub-form, so it takes no `escalated_op` entry.
    v(
        "status",
        Read,
        Status,
        Session,
        "status: the session's SUBJECT + classified STATUS (pct-encoded; '-' = unset).",
        "Reply: OK schema=1 sid= subject= subject_source=pin|osc|cwd|unavailable observed= \
         phase=unknown|starting|idle|running|quiet|exited since_ms= \
         outcome=none|success|failure|signal exit_code= signal= detail= \
         confidence=exact|strong|heuristic|unknown reasons= attribution=live|adopted|unknown \
         fs_consent=covered|denied|unknown conflict= revision= enabled= hold=<0|1> \
         fabric=<connected|stale|stalled|disconnected|absent> fabric_rtt_ms=<n|-> \
         fabric_link_age_ms=<n|-> identity=<name|-> \
         hand=<-|turn:<id>[:<holder>]|lease:<holder>|driving:<sid>> \
         level=<quiet|note|story|driving|driven|attention|limited|hold> story=<n> \
         why=<-|prompt,question,escalation,mail> path=<frozen|live> program=<name|-> \
         agent=<busy|prompt|question|wall:<kind>|idle|survey|unknown|-> agent_detail=<pct|-> \
         agent_rev=<n> \
         agent_since_ms=<ms> agent_gen=<e.s|-> agent_fp=<hex16|-> \
         integration=<on|off|degraded|-> \
         input=<-|clear|pending|typeahead|stalled|stopped> input_bytes=<n|-> \
         input_wait_ms=<ms|-> fg_rss_mb=<n|-> supervisor=<pct|-> human_ms=<ms|-> \
         path_evidence=<measured:<ms>|unconfirmed:<ms>|carried|-> copy=<managed|foreign|-> \
         upgrade=<-|<state>/<to>/<why>/<age>> history_lost=<n> \
         integration_rev=<current|stale:<hex16>|frozen|-> gen=<e.s|-> \
         seq=<n|-> hash=<hex16|->. `history_lost=` is how many scrollback lines this \
         session's in-session updates could NOT carry, summed over every update it crossed: \
         an update carries a tab's whole history, and a history that changed under its \
         export or outran it keeps only the newest lines, counted here (and said once on \
         the band) — `0` when every line crossed. `seq=`/`hash=` STAMP THE LIVE SCREEN: the terminal's \
         content_seq and FNV-1a-64 of the UNTRIMMED visible screen, the same pair `turn` \
         returns and `history` keeps per turn id, so work built from a screen read can be \
         matched against the ledger rather than believed (a stamped report carries them). \
         `gen=<epoch>.<seq>` is the screen GENERATION a fenced press names (`key if-gen=`): \
         `seq=` alone is per grid and repeats after an alternate-screen re-entry, the epoch \
         moves on every screen switch. `-` for all three when the terminal lock was contended. \
         `attribution=`/`fs_consent=` are this \
         session's consent posture (see `privacy` and `await consent`). \
         Read-only. `observed=false` means never classified, which is NOT `phase=unknown` \
         (classified, no evidence); `subject_source=unavailable` means the terminal lock was \
         contended, never a silent fall to a lower rung. Fields are ADDITIVE and never bump the \
         schema, so reject an unknown schema MAJOR rather than best-effort parsing, and treat an \
         unknown phase/outcome/reason token as unknown rather than an error. `enabled=false` \
         means `tab_status` is off and every phase will read unknown. `detail=` is the \
         sanitized RUNNING command — the program reduced to its basename (a quoted path with \
         a space is one word; a Windows `.exe`/`.cmd`/`.bat`/`.com` is dropped, so \
         `& 'C:\\Program Files\\Git\\usr\\bin\\less.exe' x` reads `less`), plus an \
         allow-listed subcommand, never an argument (`claude`, `codex`, `targo%20test`); a \
         COMPOUND line reads the segment that RUNS — the last of an `&&`/`;` chain, the \
         first of an `||` chain, one wrapper (`sudo`, `env`, `exec`, ...) unwrapped — so \
         `cd ~/ay && exec claude` reads `detail=claude`; a line that OPENS with a shell \
         keyword is not parsed and reads that keyword, so `for i in 1 2 3; do ...; done` \
         reads `detail=for`, and a keyword opening a LATER segment reads the same way \
         (`cd x && for ...; done` is `for`, never its closer `done`) — while the \
         shell-integration block executes, `-` otherwise or \
         when the terminal lock was contended: how an agent tells that a peer is another \
         agent before typing into it. \
         `hold=1` means a drive halt stands on this session, local or fleet: every PTY-reaching \
         verb answers `ERR halted reason=<r> origin=<local|fleet>` (the reason is on that line \
         and on the `hold` record in `events`); an `origin=local` hold is the owner's own and \
         `hold <sid> off` lifts it, a fleet hold only the bridge lifts. \
         `fabric=` is INSTANCE state, not this session's, and it is the BRIDGE'S BROKER LINK, not \
         the bridge process: `absent` = \
         no bridge was ever launched; `connected` = a bridge is attached AND its last exchange \
         with the broker was acknowledged; `stalled` = a bridge is attached but its link is down \
         (the dial failed — no socket, connection refused — the broker closed the connection, or \
         an ack did not arrive within the bridge's 5 s ack deadline; the bridge redials with \
         back-off from 100 ms to 5 s, mail queues and none arrives meanwhile, and `post --wait` \
         answers `ERR fabric stalled id=<n> queued=1` at once); `stale` = a bridge is attached, \
         has NOT said its link is down, and has answered nothing for work this instance handed \
         it (round 21: no ack, no landing and no `link` record for three link refreshes after a \
         post was queued) — the wedged-HELPER case, which `stalled` cannot see because a hung \
         helper holds both its lanes open and reports nothing, and which read `connected` for \
         as long as it hung before round 21; its posts queue and `post --wait` answers \
         `ERR fabric stale id=<n> queued=1`, and the fix is to kill the bridge pid and let the \
         instance relaunch it; `disconnected` = the bridge this \
         instance had is gone, which is itself a held state. NO HEARTBEAT ON A QUIET FLEET keeps \
         these honest: the bridge reports its link over its own control lane (`link`) on every \
         change and after an ack that moved the round trip by more than 2x — not on a clock, \
         with one bounded exception (a bridge that has found a presence row nothing hosts \
         re-reads the roster about once a minute until it retires it, and any answered broker \
         read is an ack, so it reports for as long as that takes and then stops). So \
         `fabric_rtt_ms=` is the \
         last acknowledged round trip to the broker in ms (`-` before the first) and \
         `fabric_link_age_ms=` is how long ago that ack was — a large age on `connected` means \
         a quiet link, not a dead one, and only the next exchange can tell. That is why `stale` \
         is NOT read off this number: it is read off an exchange that was asked for and never \
         came. \
         `identity=<name|->` is the agent identity the session was spawned under (`spawn \
         identity=<name>`), `-` for the human's own agent config - the same word the \
         `sessions` roster carries, so a driver polling one session need not re-read the \
         fleet to learn whose login it is driving. \
         `hand=` is whose hand is on the keyboard, as the presence band prints it: `turn:<id>` \
         a peer's open `turn` (`:<holder>` names the driver by its `meta role` or short sid \
         when the turn came over an edge — the edge's source; an Owner-token turn names \
         nobody, whatever edges stand), `lease:<holder>` a cooperative drive lease, \
         `driving:<sid>` this session's own open turn into another, `-` nobody. `level=` is the band's severity, ascending \
         `quiet < note < story < driving < driven < attention < limited < hold` (the rim is \
         teal from `driven`, amber at `attention`, red at `limited` (an agent at a \
         wall) and `hold`; `story` is the \
         violet dot: something happened since the human last looked); `story=` is the seq of \
         the newest story point (`0` = nothing ever happened; `ctl story` replies with it). \
         All three are read from the presence slot the wakes keep current — no lock, no \
         classifier — so they cost the poll nothing. `why=` says what put the session at \
         `level=attention`, comma-joined in that order: `prompt` (an approval box), `question` \
         (the agent asked), `escalation` (a typed `meta attention`), `mail` (an unread \
         task/ask with no hand on the session); `-` at any other level. \
         `path=<frozen|live>`, after `why=` and before the stamp (which stays last): whether \
         this session's shell fronts aterm's managed \
         `agents/` on PATH — the same value the `sessions` roster carries (`frozen` = \
         `claude`/`codex` typed at its prompt resolve to a foreign copy), MEASURED from the \
         environment of a program the shell started where one has been read, else the \
         mark the shell was adopted with; `path_evidence=`, `copy=` and `upgrade=` (after \
         `supervisor=`, before the stamp) are the `sessions` roster's owner columns — see \
         `help sessions`. \
         `program=` is the argv[0] basename of the PTY's FOREGROUND process-group leader \
         (`zsh` at a prompt, `sleep`, `claude` — argv[0], never the executable path, which for \
         a self-updated agent is a version number; a shell interpreter running atpkg's own shim \
         for `claude` or `codex` — the script in the managed prefix's `agents/` or `bin/`, \
         before its `exec` — reads as that agent; a script of that name anywhere else stays the \
         shell), \
         re-resolved off the event loop when the \
         foreground group changes (and at most every 5 s while the screen moves, for an `exec` \
         in place), so it names an adopted or non-integrated session's \
         program where `detail=` cannot; `-` until resolved. `agent=` is the SERVER'S VERDICT \
         on the screen, read by that program's own screen reader (aterm-phase): applied only \
         when the foreground program is an identified agent (`claude`, `codex`) or the screen \
         shows Claude Code's composer frame or one of its boxes, and re-read \
         whenever the content or the terminal's cursor moved and the live zone (the last 40 \
         rows of the screen's content) or the cursor's row changed, at most 4 Hz \
         — never gated on `revision=`; anything else reads `-` (a shell whose last line ends in \
         `?` is not a question). `wall:<kind>` is the WALL the last turn ended on, one of \
         `usage-session`, `usage-weekly`, `model-bucket`, `spend`, `context`, `auth`, \
         `api-error`, `overloaded` (a 529 reads `wall:overloaded`, never `idle`), `memory` \
         (Claude Code's critical-memory banner, read by its place right above the composer's \
         frame, never from a quote of it: restart it, then `claude --continue`), or \
         the SERVER'S OWN `unresponsive` — the program has stopped reading its input (see \
         `input=` below), whatever its screen still shows, with `agent_detail=stopped` for a \
         stopped job; it is published and cleared by aterm in step with `input=`, and the \
         screen's verdict returns when the program reads again; `unknown` is \
         an identified agent whose reader has no evidence for a phase (a Codex screen outside \
         its choice box; a Claude Code screen with no prompt box that holds the terminal's \
         cursor — its launch, and the moment after its folder-trust dialog is answered, before \
         its REPL is up, including when its inline renderer is relaunched in the same tab and \
         the previous run's prompt box is still on the screen above the new launch line) — \
         never act on it as idle: Claude Code reads `idle` only at the prompt box that holds \
         the cursor, where it keeps the cursor once its REPL is drawn and a prompt typed is \
         taken: at the foot of its fullscreen screen or, in its inline renderer, under the \
         latest launch line. \
         `agent_detail=` is a prompt's `<kind>[:<verdict>]` (`bash:not-read-only`, \
         `edit`, never the command) or a wall's reset time, `-` otherwise; `agent_rev=` bumps \
         each time the word or detail moves (`0` = never published) and `agent_since_ms=` is \
         how long ago. `agent_gen=`/`agent_fp=` are the `gen=`/`hash=` of the screen the \
         verdict was READ from, which can be one sweep older than `gen=`/`hash=`: a press \
         decided from the verdict fences on THEM (`key if-gen=<agent_gen> if-fp=<agent_fp> \
         1`), never on the fresh stamp. `agent_detail=bash:read-only` holds only when every \
         reading of the box's command rows (joined by newline and by space) classifies \
         read-only; it is advisory — the supervisor's own decider decides. `subscribe … \
         events` pushes `EVENT <local> agent <word> rev=<n> gen=<e.s> fp=<hex16>` on each \
         move and `await agent <word>[,<word>…]` parks on it. A frame-only identification \
         applies while the program is unresolved or `node`/`bun`/`deno`, never to a shell or \
         pager showing a captured screen. `integration=` is whether this \
         session's OSC 133/633 marks reach aterm: `on`, `off` (not required), or `degraded` — \
         a nonce is required and none is in use (an adopted shell whose update did not \
         carry it, or one re-keyed through its channel that has not reached its next prompt \
         yet), so `detail=` and blocks stay dark while `program=` still names it; `-` \
         when the terminal lock was contended. `integration_rev=` (after `history_lost=`, \
         before the stamp) is WHICH shell integration the session's shell runs, against the \
         one this build ships: `current` (its last SIGNED `633;P;AtermIntegration=<rev>` — \
         the script folder its integration body came from — is this build's), \
         `stale:<rev>` (another build's: a shell adopted across an update runs the old \
         build's until its next prompt, when it takes this build's body through its \
         pointer and the word reads `current`), `frozen` (adopted with an integration from \
         before loaders, fixed at spawn: no pointer reaches it, so a fix to its marks \
         reaches it only in a new tab — or when the live agent upgrade types its relaunch \
         line there, which also sources this build's loader), `-` (no integrated shell, one \
         before its first prompt, or the lock was contended). \
         `input=` is whether the program is READING ITS INPUT, probed live from the kernel \
         (macOS; `-` elsewhere and off a tty): `clear` nothing unread; `pending` raw-mode \
         input unread and not (yet) judged a stall; `typeahead` complete lines a canonical \
         (line-mode) reader has not asked for yet — a shell's ordinary type-ahead, never \
         flagged; `stalled` raw-mode input unread for 10 s or more by a program that has \
         drawn nothing since and is burning CPU (or has sat asleep for 5 min — a pager on a \
         slow pipe or a shell inside a slow completion sleeps too, and a program that keeps \
         drawing is never stalled), entered only while aterm has read all the program's \
         output, so a program waiting on aterm is not blamed; `stopped` input queued under a \
         stopped job. `input_bytes=` counts every unread byte (the \
         kernel's queue plus aterm's own spill behind it) and `input_wait_ms=` is a LOWER \
         BOUND on how long the oldest has waited; `fg_rss_mb=` is the foreground program's \
         resident size in MiB while `stalled`/`stopped`, `-` otherwise. A frozen program's \
         screen never moves, so no screen fence sees this: an input-writing verb into a \
         session whose raw input has waited a second answers `ERR busy input-unread` (see \
         `key`). While `stalled`/`stopped` aterm also raises the session's attention itself, \
         as owner `aterm` (`meta attention_owner=aterm`: `<program> is frozen: not reading \
         input since <HH:MM> … — restart it: aterm ctl @<sid> signal term`), over any older \
         badge, and clears it when the program reads again; nothing is armed while no input \
         is unread. The queue `signal term` drops is not read: a program that lives through \
         it stays `stalled` (`input_bytes=0`) until it ends or shows it is alive — it draws, \
         reads a key sent since, or stops spinning with nothing left unread — and after 5 s \
         the line reads `… is still running after its restart signal … — end it: aterm ctl \
         @<sid> signal kill`. `supervisor=` is the live `meta set supervisor` \
         claim — who is answering this session's prompts — `-` when none. \
         `human_ms=` is how long ago a PERSON last gave this session input — a key (an arrow, \
         a digit, Enter and Esc included), text, a paste, a click, a drag, a wheel notch or a \
         scroll of the view, from the window (a hover does not count), never a control verb \
         (`key`/`send`/`paste`/`turn`/`mouse`/`feed-bin`) — in ms, `-` when no person has since \
         the session started. An automatic supervisor keeps its hands off while it is under \
         `[harness] human_grace_s`, and waits on it before it keys a dialog a person may be \
         navigating (`text --json` carries it too, as \"human_ms\"; every `sessions` row as \
         `human_ms=`); `subscribe … events` pushes `EVENT <local> human` when a person starts \
         after 30 s without. The stamp lives in this instance, so a session carried across an \
         update starts at `-` again. \
         No `window=` here: `status` is \
         polled, and the window lives on the main thread, so a per-poll hop would be a \
         latency regression — ask `sessions`/`ls` (one hop for the whole fleet) or `dims`",
    ),
    v(
        "timeline",
        Read,
        Lines,
        Session,
        "timeline [<n>] [since=<id>]: the session EVENT TIMELINE",
        "- one `event <id> t=<ms> kind=<k> ...` line per recorded event: the lifecycle kinds \
         (spawned/state-change/title-change/cwd-change/meta-change/agent-change/human/modes-restored \
         — the last is the PTY reader handing the terminal back after a foreground program lost it \
         with modes armed: `from=<pgid> to=<pgid> program=<name|-> reverted=<csv> bytes=<n>`) AND \
         the fabric/messaging \
         kinds the same ring records (hold/inbox/inbox-seen/post/fetch/post-landed/topic), monotonic ids, \
         drop-oldest \
         ring. The two rows a session records as it is retired — `closing reason= by=` and the \
         final `state-change state=closed` — cannot be asked for here after the close: the sid \
         stops resolving in the same store write (only a request that resolved the session just \
         before that write can still read them). A live `subscribe @<sid> events` watch is what \
         delivers `closing` \
         (as `EVENT <local> closing reason= by=`, ahead of `exited`); `exits` keeps the same facts \
         afterwards",
    ),
    v(
        "metrics",
        Read,
        Status,
        App,
        "render/latency counters [reset|percentiles]",
        "- percentiles: p50/p95/p99 input->application-present-return / \
         output->application-present-return / frame-render distributions; plain line carries \
         max_frame_gap_ms= (worst successful-present-return gap since reset), \
         rust_main_to_first_present_ms= and rust_main_to_first_visible_ms= (Rust entry->the FIRST \
         window's actual reveal — time-to-visible; on a warm Windows launch the reveal precedes \
         the backend join, so it runs well under first-present) plus \
         startup_phase_schema=1/startup_phase_valid= and eight exclusive startup_*_ms phases \
         (router, GUI prepare, winit dispatch, initial surface attach, successful-redraw \
         wait/compose/surface/finalize); startup_attach_schema=1/startup_attach_valid= drills the \
         attach parent into \
         dispatch/prepare/window-create/window-setup/backend-finalize/chrome-geometry/surface-create/finish, \
         effect_pipeline_builds=/effect_pipeline_build_ms= report the EFFECT-only cell pipelines \
         this process compiled ON DEMAND (the nine that only ever draw a cursor trail / fire / \
         rain / sparkle / sprite layer are built the first frame that binds one, never at launch \
         — a default `cursor_trail = false` run reads 0/0.00 for its whole life, and a non-zero \
         reading with no effect enabled means the demand gate has re-eagerised), then \
         first_present_ms= (compatibility GUI main_entry->the same successful-present \
         publication; dyld/compositor/scanout unobserved) first_visible_ms= (GUI main_entry->the \
         same reveal instant) and the CHILD's own round trip: n_echo= \
         echo_p50_ms=/echo_p95_ms=/echo_p99_ms= echo_last_ms=/echo_max_ms= with the \
         echo_total=/echo_arms=/echo_coalesced=/echo_expired=/echo_dropped_locked= ledger that \
         qualifies them — bytes out to the PTY->the first bytes back, the ONE slice on this \
         line that is not aterm. Near its end the line carries the main-loop TURN census max_turn_ms= \
         max_turn_owner= max_turn_at_ms= last_turn_ms= turns= long_turns= \
         long_turn_threshold_ms= (a main thread parked OUTSIDE the redraw: read the max against \
         max_redraw_total_ms), then the STRAIN engine's strain=calm|suspect|open|off (off = config \
         explain_heavy_load = false) with strain_probe_us_max=/strain_scan_us_max= (the probe \
         thread's slowest machine reading and process sweep since reset; 0 until an episode \
         first samples), and last the self-update's pre-Commit input: input_dropped= \
         (keystrokes typed into an updated window before Commit that its bounded queue \
         refused — lost) and input_incoherent= (replays that had to disarm the held \
         modifiers: a drop, or a window losing focus with input already queued for it), both \
         cumulative for the process and kept across `metrics reset`. IT NAMES WHO OWES THE TIME: echo_* high with input_* low means the PROGRAM \
         is behind (a TUI sharing its scheduling band with a build has been measured echoing at \
         p99 150ms while aterm wrote every key at p99 6.29ms), the reverse means aterm is; a \
         lone max_present_latency_ms is not evidence of either. READ THE SLICES HONESTLY: present_* and input_* are OPEN INTERVALS closed by the next qualifying present, so any stretch in which nothing presented is INSIDE the number (only a 5 s discard bounds it) — a multi-second present_latency means \"nothing presented for that long\", not \"a frame took that long\"; input_* closes on the next CONTENT present, which under concurrent streaming output may be a log-line frame rather than the key's echo, so it reads LOW rather than high. input_* counts EVERY key a content present answered (up to 32 waiting per window; the overflow is input_edges_dropped), so keys that never echo — a password prompt — each book their wait until the next content present (under the 5 s discard): measure typing latency in a program that echoes. Quote n_* with the percentiles, never a lone last_/max_, and note both stop at application-present return (no compositor selection, scanout or photons)",
    ),
    // drive input
    v(
        "turn",
        Write,
        Lines,
        Session,
        "turn [option=value ...] <text>: ONE HUMAN TURN",
        "- type <text>, verified submit (submit=none types WITHOUT submitting; default 'enter', \
         and any key-verb name is a valid submit key), wait for the screen to settle (idle for \
         idle=<ms>, cap timeout=<ms>), return the settled screen. Options: [idle=<ms>] \
         [timeout=<ms>] [submit=<key|none|guarded:<re>>] [settle=match:<re>|gone:<re>] [submit_window=<ms>] \
         [presses=<n>] [submit_verify=<auto|seq|block>] [trim=<0|1>] [typed=<0|1>] \
         [cadence=<ms>] [yield=<floor>] [if-gen=<epoch>.<seq>]. settle=match:<re> keys \
         the settle on a visible row matching <re> instead of global idle; settle=gone:<re> on \
         <re> LEAVING the screen (an agent's busy footer) — and that one is TWO waits: the \
         pattern must APPEAR within submit_window after the verified submit (the `await gone` \
         predicate is level-triggered and the footer can land a frame after the submit \
         verified, so arming it in that gap would settle on the PRE-response screen), then \
         LEAVE; a pattern never seen in that window falls back to the idle settle, so a wrong \
         pattern degrades to a plain turn rather than an instant false `settled`. \
         submit=guarded:<re> submits with Enter only while the row holding the CURSOR still \
         matches <re> — where a submit lands: a composer holds the cursor, an approval box does \
         not, and Claude Code's transcript repeats the composer's `❯` at column 0 — checked and \
         pressed under ONE terminal lock hold (the `key if=` delivery), so text typed into a \
         composer is not submitted into whatever replaced it; a miss on the first press leaves \
         the text typed and answers `OK 0 turn skipped reason=guard submitted=0 seq=<n> id=<n>` \
         (zero rows), a miss on a re-press ends the presses, and the verdict gains \
         `pressed=<n>`, the Enters written (`submitted=0 pressed=1` = written, not verified — \
         never retry it blindly). Reply: verdict line \
         (id=/submitted=<0|1>/status=settled|timeout/seq=/dur_ms=/hash=<FNV16 of settled screen>) \
         then the rows; trim=1 drops the trailing all-blank rows and closes the verdict with \
         trimmed=<k> — hash= stays the FNV of the UNTRIMMED screen (a screen identity, the value \
         `history` reports), not of the bytes sent. App-agnostic; humans can interject. \
         THE EVEN HAND: a plain turn is ONE PASTE — no keystroke behind any cell, so it lays no \
         cursor ribbon, earns no stardust and steps no melody (every light has a keystroke behind \
         it). typed=1 presses <text> one key per grapheme through the SAME seam a keyboard uses, \
         cadence=<ms> apart (default 70, an absolute schedule that never drifts): the turn lays \
         real ribbon and earns real stardust, and its only signature is that machine-even \
         interval — the seam is told nothing else. The typed prefix is capped at 240 graphemes; \
         the remainder is one paste (voiced as one up-strum of the music box's chord, keyed off \
         the paste itself, human or agent alike). The verdict then carries typed=<keys> \
         pasted=<graphemes>. yield=<floor 0..=1> parks BEFORE typing until the target's typing \
         momentum (the `trail status momentum=` number) has exhaled to the floor — analytic, one \
         timer per reading, no polling; the human's typing is never blocked (the agent yields, \
         the human never waits) — then adds yielded_ms=<n>; a yield that outlives timeout= types \
         NOTHING and answers `ERR yield timeout momentum=<v>`. \
         if-gen=<epoch>.<seq> (the `gen=` a read returned — `status`, `text --json`) types ONLY \
         while the screen is still that generation, checked under the terminal lock after the \
         yield and immediately before the paste goes to the input seam: any output since — a \
         person's keystroke echoed, a box, a repaint — types NOTHING and answers `OK 0 turn \
         skipped reason=changed submitted=0 seq=<n> id=<n>`, so a driver that judged a screen \
         (an empty composer) never pastes into another one; read again and decide again. \
         EXACTLY-ONCE: a leading id=<epoch>:<producer>:<seq> makes the turn replay-safe — see \
         `send`'s entry for the key, which exactly `send`, `key`, `feed-bin` and `turn` take. \
         The OTHER input verbs (`paste`, `ctrl`, `feed`, `paste-bin`, `mouse`, `pointer`, \
         `hwkey`) take NO key — a leading id= on `paste` is delivered as literal text — so a \
         crash-safe driver retries only through the four keyed verbs. A DUPLICATE answers `OK 0 \
         dup=1` (this verb is Lines-framed) and carries NONE of the verdict fields above, id= \
         included: nothing was typed, so there is no verdict to report. The reply's own id= is \
         the TURN id and is unrelated to the key you sent. UNREAD INPUT: a turn into a program \
         that has left input unread is refused `ERR busy input-unread …` before it types \
         anything — asked when it arrives and again after its yield= (see `key`; turn takes \
         no unread=ok) — and it never RE-presses while an earlier byte is still unread: its \
         one Enter waits in the queue and the verdict says submitted=0",
    ),
    v(
        "lease",
        Write,
        Status,
        Session,
        "lease [status|acquire|release ...]: a drive lease — cooperative, or a HARD hold",
        "— one holder at a time, TTL-expiring (default 30000ms, max 600000), surfaced in `who` as \
         driving=lease:<holder>. Forms: lease [status] | lease acquire [ttl=<ms>] [holder=<name>] \
         [hard] | lease release [holder=<name>] [force]. acquire refuses a live `turn` or a \
         DIFFERENT holder's live lease (steals a lapsed one; same holder renews, over the \
         connection that renews); a live lease also blocks another connection's `turn` from \
         stomping it. COOPERATIVE by default: ADVISORY for raw send/key/feed (use `turn` for \
         HARD arbitration) — the coordination signal cooperating agents check before driving. \
         `hard` makes it a HOLD: every OTHER connection's send/key/ctrl/feed/mouse/paste/turn \
         (and feed-bin) is refused `ERR busy lease=<holder>`, while the holder's own connection \
         writes on — its own `turn` runs under it and hands it back; `status` says hard=1. A \
         person's keyboard is never blocked",
    ),
    v(
        "send",
        Write,
        Status,
        Session,
        "write text to the PTY; reply `OK seq=<n>`",
        "— the content baseline, so `await seq <n+1>` waits for the output this input causes \
         (same seq= on key/ctrl/feed/mouse/paste). EXACTLY-ONCE: a LEADING \
         id=<epoch>:<producer>:<seq> stamps the write, and `key`, `feed-bin` and `turn` take the \
         same key. <epoch> is this session's launch nonce (the roster's nonce=); <producer> is any \
         u64 stable per driver; <seq> is that driver's own monotone sequence. A sequence at or \
         below the producer's high-water writes NOTHING and answers the duplicate marker IN THE \
         VERB'S OWN FRAMING — `OK dup=1` for a `Status`-framed verb (`send`, `key`, `feed-bin`), \
         `OK 0 dup=1` for a `Lines`/`Bytes` one (`turn`), because a bare `OK dup=1` would make a \
         Lines client read `dup=1` as a row count — so a driver that crashed without seeing its \
         reply can retry safely. A retry of a sequence whose attempt \
         did NOT answer OK gets `ERR in-doubt seq=<n>` — it may have typed, so it is reported, \
         never replayed, and the session's `timeline` carries the row. A key minted before a \
         relaunch is `ERR epoch`. OPTIONS LEAD: only a FIRST token spelled id= is a key, so \
         `send hello id=1` still types `hello id=1`, and a leading `--` ends option parsing. \
         GUARDED: a leading if=<re> makes the write conditional on a visible row matching \
         <re>, checked and written under ONE hold of the terminal lock — see `key`, which \
         carries the contract; `send` and `key` are the two verbs that take it, in either \
         order beside id=, and the two that take the if-gen=/if-fp= fences `key` describes. \
         An aterm older than the fences types a leading if-gen= into `send`'s body as TEXT \
         (`key` answers it ERR usage), so fence a press with `key`. UNREAD INPUT: while the \
         program has left earlier input unread, `send` is refused `ERR busy input-unread …` \
         and writes nothing — see `key`; a leading unread=ok queues anyway",
    ),
    v(
        "paste",
        Write,
        Status,
        Session,
        "bracketed-paste text to the PTY",
        "the payload rides the engine's PASTE seam — bracket guards when the app has set \
         DEC 2004 AS THE FRAME IS WRITTEN, control-byte sanitize, LF->CR — but NOT the \
         `confirm_multiline_paste` prompt a person's paste answers. A driver that asked \
         for a paste has already decided, and parking this reply on a window banner would \
         hang the caller; so an unbracketed multi-line payload reaches the shell here \
         without the confirmation the same text raises from the keyboard, and its first \
         line can run. Same-uid, token-gated and owner-scoped — whoever can send this can \
         already run commands — so the difference is stated rather than left to be \
         discovered. That is also why the framing is read at WRITE time and not captured \
         earlier the way a keyboard paste's is: a keyboard paste captures the mode its \
         confirmation was judged under, and this verb was never shown one",
    ),
    v(
        "key",
        Write,
        Status,
        Session,
        "key [id=<key>] [if=<re>] <name>: send a named key (enter/tab/up/...)",
        "— delivered beneath the window's shortcuts on every platform: `key ctrl+shift+t` \
         sends ^T to the shell and opens no tab, and no chord opens the find bar; the window's \
         own commands are `invoke <action>` (names via `controls menu`), `tab new` and `find \
         open`. Accepts a leading id=<epoch>:<producer>:<seq> idempotency key (see `send`). \
         GUARDED PRESS: a leading if=<re> makes the press conditional — under ONE hold of \
         the terminal lock the regex is tested against the visible rows and, only if some \
         row matches, the key is written, so no output can land between the check and the \
         press. WHY: a prompt can vanish between a read and a press — measured, a Claude \
         Code permission prompt resolved between a supervisor's screen read and its `key \
         1`, and the digit landed in the composer as text; a read-then-press is two \
         requests, and only a check made under the same lock as the write closes the gap. \
         A match answers `OK seq=<n>` exactly like a plain press; NO match writes nothing \
         and answers `OK skipped seq=<n>` — exit 0, a skipped guard is an answer, not an \
         error. A bad pattern is `ERR badregex` (nothing written); a sink that cannot take \
         the frame right now (a spill ahead of it, another writer, a full tty buffer, a \
         master that is not non-blocking) is the transient `ERR busy sink` — zero bytes, \
         retry. THE REGEX IS ONE WIRE TOKEN: the control line is split on whitespace and \
         nothing quotes, so write a space as `.` — `key if=Do.you.want.to.proceed 1`, never \
         `key if='Do you want' 1` (that arms `Do` and presses `you`). Composes with id= in \
         EITHER order (`key id=… if=… 1` and `key if=… id=… 1` are one request); a skipped \
         guard gives its id= sequence back, so the retry is re-evaluated against the live \
         screen rather than answered `dup=1`. ORDER OF REFUSALS: `ERR halted`, `ERR rate`, \
         `ERR busy turn=`, `ERR usage`, `ERR busy input-unread`, `ERR badregex`, then the \
         guard and fences, then the id= claim — so a halted session is `ERR halted` whatever \
         the guard says, and no refusal spends an id= sequence. UNREAD INPUT (macOS; never \
         refused elsewhere): when the program has left input unread in its tty — raw-mode \
         bytes at least a second old, or any bytes while its job is stopped — every \
         input-writing verb (send key ctrl feed feed-bin paste paste-bin mouse focus turn \
         hwkey pointer, invoke Paste, operator-propose-bin) writes NOTHING and answers `ERR \
         busy input-unread bytes=<n> wait_ms=<ms> input=<pending|stalled|stopped> (<why and \
         the remedy>)`: a key sent now would be read AFTER those bytes, against a screen the \
         program has not drawn. A screen fence cannot see it — a frozen screen always passes \
         if=, if-gen= and if-fp=. It is `ERR busy`, so a back-off retries it; a human's \
         window keyboard is never refused; `signal` (int interrupts, term restarts a frozen \
         program, cont resumes a stopped one), `close`, `resize`, `pane` and `tab` stay open; \
         a canonical reader's queued lines (a shell's type-ahead) never refuse; and nor does \
         a lone signal character (key ctrl+c, ctrl c, feed 03 — the tty's own intr, quit and \
         susp) while the tty has ISIG on (a cbreak program) and the kernel can take it now \
         (no spill ahead of it, room in the queue), because the line discipline turns it \
         into a signal as it is written and it never queues — in raw mode it is a byte like \
         any other, refused, and `signal int` is the interrupt. A leading \
         unread=ok queues anyway — on send and key in any order beside the other options, on \
         hwkey, pointer and invoke (the App lane) as the FIRST token; any other value, or a second one, is `ERR usage`. The guarded press is delivered on the control thread against the \
         target's own terminal and PTY (like every `@<sid>` input verb), so for the tab on \
         screen it skips the App seam's cosmetic side effects; a press that must ride the \
         seam is a plain `key`. `send if=<re> <text>` is the same guard on a raw write. \
         FENCES (key [if-gen=<epoch>.<seq>] [if-fp=<hex16>] <name>): a leading if-gen= presses \
         only if the screen generation is still that one (`status gen=`, or `agent_gen=` for a \
         press decided from the agent verdict — any output since moves it, and so does any \
         screen switch), and if-fp=<hex16> only if FNV-1a-64 of the visible screen is still \
         that value (`status hash=`, `agent_fp=`, `turn hash=` — an identical repaint keeps it). \
         Checked under the same lock hold as the press; a screen that moved answers `OK skipped \
         reason=changed seq=<n>`, nothing written, the id= sequence given back, so a supervisor \
         that decided on one box cannot press into the box that replaced it. They compose with \
         if= and id= in any order; a bad value is `ERR usage`, and so is if-seq= (seq= is per \
         grid and repeats after an alternate-screen re-entry, so it is no fence). A plain `if=` \
         miss still answers `OK skipped` exactly. TRAILING TOKENS: `mods=<list>` \
         (`mods=ctrl+shift`; `ctrl+u` is `u mods=ctrl`), `type=press|repeat|release` \
         (`down`/`up` alias the ends) and `base=<c>` (the US-layout key kitty reports as \
         its third field). A MODIFIER KEY'S RELEASE takes `mods=` as the state the release \
         LEAVES, as kitty reports it: `key shiftleft type=release` is Shift let go (`CSI \
         57441;1:3u` under report-all + event types), and `mods=shift` on it says the other \
         Shift is still held (`CSI 57441;2:3u`) — so a release does not repeat its press's \
         `mods=` (before 2026-09-27 the key's own bit was cleared whatever `mods=` said)",
    ),
    v("ctrl", Write, Status, Session, "send a control char", ""),
    v(
        "feed",
        Write,
        Status,
        Session,
        "write raw bytes to the PTY",
        "",
    ),
    v(
        "hwkey",
        Write,
        Status,
        App,
        "hwkey <char|name> [mods=] [count=] [interval=]: inject a key through the OS event queue",
        "(macOS only: a real NSEvent posted to this app; elsewhere it answers `ERR hwkey: \
         hardware-path injection is implemented on macOS only`), so it takes the SAME winit \
         path a physical keypress takes and carries the NSEvent-queue backdate. Use this — NOT \
         `key` — to measure typing latency: `key` posts straight to the main thread and is \
         born already dequeued, so it cannot see OS-level key queueing (the drawable park) \
         at all. Replies `OK posted=<n>` — events handed to the OS queue, not bytes \
         written; read the result with `metrics percentiles` (`n_key_write` moves only on \
         this path). Refused like every input verb while input waits unread; a leading \
         unread=ok presses anyway",
    ),
    v(
        "feed-bin",
        Write,
        Status,
        Session,
        "length-prefixed raw bytes to the PTY",
        "`feed-bin <n> [id=<key>]` then <n> raw bytes. The optional idempotency key (see `send`) \
         rides the HEADER, not the payload: a frame whose key is refused still consumes its \
         announced bytes, so the stream stays framed and the next request line is the client's",
    ),
    v(
        "paste-bin",
        Write,
        Status,
        Session,
        "length-prefixed bytes to the PTY with PASTE semantics",
        "(bracketed-paste guards as DEC 2004 stands when the frame is written + control-byte \
         sanitize + LF->CR); the binary twin of `paste`, framing contract included",
    ),
    v("mouse", Write, Status, Session, "inject a mouse event", ""),
    // `pointer` is `Write` because it drives the WINDOW's pointer through the very
    // entry point winit's `CursorMoved` calls, and under DEC 1000/1002/1003 that
    // motion is REPORTED to the driven program — `Op::WriteInput`'s own doc lists
    // the mouse in the human vocabulary. It deliberately reports NOTHING derived
    // from the grid: `link=` here would hand a keystroke-only edge a fact only
    // `cell` (`Read`) is entitled to answer, and write does not imply read.
    v(
        "pointer",
        Write,
        Status,
        App,
        "pointer [move <r> <c>|leave|status]: put the POINTER on a cell, so hover resolves",
        "— `mouse move` posts an engine `InputEvent` and never touches the window's pointer, so \
         nothing it does makes a link hover, a divider cursor or a tab-strip highlight happen. \
         This drives `App::on_cursor_moved` — the identical function `WindowEvent::CursorMoved` \
         calls — after mapping the cell to the centre pixel of the frame it is drawn in, and \
         `pointer leave` drives `on_cursor_left` (what `WindowEvent::CursorLeft` calls). Reply: \
         `OK at=<row>,<col>` is where the pointer ACTUALLY resolved, read back from the window \
         AFTER the real path ran, in window (not pane-local) cells — so it states where the \
         pointer IS rather than repeating the request. A cell outside the grid is `ERR`, never \
         silently clamped onto a neighbour and reported as though it had been honoured. `OK at=-` \
         when the window holds no pointer position at all: after `pointer leave`, and before the \
         first move of any kind. `at=-` says only that — never a position that does not exist. \
         Read the hover the pointer resolved with `cell <r> <c>` (`link=`) and see the destination \
         band with `image`. FRONT window of the resolved instance, like `hover`. Refused like every \
         input verb while input waits unread; a leading unread=ok moves it anyway",
    ),
    v(
        "resize",
        Write,
        Status,
        Session,
        "resize <r> <c>: resize the engine + PTY (grid first, window echoed to match).",
        "`resize px <w> <h>` instead resizes the WINDOW in physical pixels and lets the grid \
         follow from the platform resize event — the same path an edge drag takes, so it is the \
         form that exercises the live-resize width throttle (the cell form pre-applies the grid, \
         so the window event never sees a column change). Drive several back to back to reproduce \
         a drag's event pressure; read the result with `metrics` (`resize_present`) and `dims` \
         (`layer_*`). The px form names a WINDOW and a driven px resize reaches only the FRONT \
         one, so it is accepted for the focused pane of the front window's active tab alone (the \
         flagless form, `@self`, or an `@<sid>` that resolves to it); aimed at any other session \
         — a sibling pane, a background tab, another window — it answers `ERR resize px: only \
         the focused pane of the front window's active tab`, while the cell form reaches every \
         session's engine + PTY",
    ),
    v("focus", Write, Status, Session, "send focus in/out", ""),
    v(
        "scroll",
        Read,
        Status,
        Session,
        "scroll the view (up|down|top|bottom|N|prev/next-prompt)",
        "A bare `scroll` mutates nothing and reports the current position.",
    ),
    v(
        "select",
        Read,
        Status,
        Session,
        "select region / word <r> <c> / line <r> / block <r1> <c1> <r2> <c2> / extend / clear",
        "`extend <r> <c>` is the shift-click grow of whichever selection stands. \
         `word` and `line` are the double/triple-click gestures and select the \
         LOGICAL line, not the physical row: a soft-wrapped line is selected \
         from its first row through its last (so `copy` gets the whole command, \
         not the window's width of it), and a word straddling the wrap is \
         selected whole across both rows — except across a RAGGED wrap, where a \
         double-width glyph cut the row short and the blank it left still splits \
         the word. A HARD newline still ends either one.",
    ),
    v(
        "signal",
        Signal,
        Status,
        Session,
        "signal <sig> [pid=<n>] [quiet=<s>]: send a signal to the foreground process group",
        "- <sig> is int|quit|tstp|hup|term|kill|cont (c, z and the sig-prefixed names too). \
         `pid=<n>` sends it to exactly that process, and only while it leads the \
         foreground process group (else ERR, nothing sent); refused under a hold \
         like every form. `quiet=<s>` sends it only while the tab is left alone, in the \
         same decision: `ERR busy person human_ms=<ms>` when a person gave the session \
         input within <s> seconds, `ERR busy input-unread bytes=<n>` while written input \
         sits unread (a prompt on its way) — nothing sent, nothing discarded. cont resumes a stopped job, the remedy `ERR busy input-unread … \
         input=stopped` names; term is the restart for a program that has stopped reading \
         its input, and int interrupts one whose tty is raw (where `key ctrl+c` is a byte \
         queued behind the unread input). Never refused by the unread-input gate. term, \
         kill, hup and quit first DISCARD the input a raw-mode (or stopped) job left unread \
         — keys typed at a frozen program outlive it, and the shell that takes the tty back \
         would run them as commands — and say so: `OK signalled pgrp <n> discarded=<bytes>` \
         (`pid <n>` with pid=), counting the kernel's queue and what aterm itself still held \
         behind a full one (a paste's tail), both dropped. Canonical (line-mode) type-ahead \
         is the shell's and is kept; int, tstp and cont discard nothing (macOS; elsewhere \
         nothing is discarded). A dropped queue is not a read: a program published \
         `input=stalled` that lives through term, hup or quit (a Node program's SIGTERM \
         listener never runs while its JS thread spins) stays published, with `status \
         input=stalled input_bytes=0` and every input verb refused, until it ends, draws, \
         reads a key sent since, or stops spinning, and after 5 s its attention names `signal \
         kill`. The reply does not wait for the \
         program to end. POSIX only: Windows answers `ERR signal unsupported on this \
         platform`",
    ),
    // app / GUI drive (selector routes to the instance's front window)
    v(
        "tab",
        Write,
        Status,
        App,
        "drive a window's tabs (new|N|next|prev|close [N]|move <from> <to>)",
        "- flagless drives the FRONT window; `@<sid> tab …` drives the window hosting <sid> \
         (the same aim as `@<sid> spawn`), so an agent in a background window walks its own \
         tabs without touching the human's. `N`, `<from>`, `<to>` and the reply's `<active>` \
         are 0-BASED — the `index=` `inspect app/v1 tabs` prints; the strip's painted \
         ordinals count from 1. Replies `OK <active> <count>` when the action \
         HAPPENED, and `ERR <reason>` when it did not: an index no tab holds, a tab \
         host that declined the close (a native document waiting on a durable \
         checkpoint, or a close deferred behind a pending update handoff), or a job \
         running in the tab being closed, refused as `close` refuses its pane: `ERR close \
         refused (a running job armed the last-tab confirm)` for the window's last tab, \
         `ERR close refused (a running job in that tab)` for any other. Nothing moved; \
         finish the job and close again (a Cmd-Shift-O co-viewed pane never counts). A \
         --headless instance never refuses a close, and drives its one logical window like \
         a real one (no `ERR headless`)",
    ),
    // `pane` is `tab`'s within-a-window twin and carries `tab`'s class for `tab`'s
    // reason: choosing which pane the keyboard drives is an input-routing authority,
    // so a read-only edge must not be able to redirect the human's next keystroke.
    // Its reply is one bit about the caller's OWN act — strictly less than the tab
    // COUNT `tab` already hands a write edge — so it needs no read authority beside
    // it; `panes` (`Read`) is what answers where focus went and what the rects are.
    v(
        "pane",
        Write,
        Status,
        App,
        "pane <left|right|up|down>: move keyboard focus to the adjacent pane",
        "- the ⌘⌥-arrow / `focus_pane_*` binding's own path, so the active-pane mark moves with \
         it and the window re-mirrors term/master/socket onto the newly focused pane exactly as a \
         click-to-focus does. `spawn split=v|h` makes the panes; this is what then walks them. \
         Reply `OK moved=1` when focus changed and `OK moved=0` when it did not — a single-pane \
         tab, or no neighbour that way — which is a fact about the caller's own request, not a \
         reading of the layout: ask `panes` (read-side) for the rects and which pane is focused. \
         FRONT window of the resolved instance, like `hover` — it does NOT aim at the window \
         hosting an `@<sid>`, the way `tab` and `spawn` do",
    ),
    v(
        "open",
        Write,
        Status,
        App,
        "open a native tab app: `open app settings [/route]`",
        "or `open app markdown|editor <local-file-uri>`; compatibility aux targets remain available",
    ),
    v(
        "act",
        Write,
        Status,
        App,
        "dispatch an exact semantic native-app action: act app/v1 view <view-id> <ui-key> <action>",
        "[value]",
    ),
    // `settings set|unset` atomically REWRITES the durable on-disk `aterm.toml`
    // (flipping default-OFF security knobs), so it is `ConfigWrite` — a strictly
    // greater authority than the `Write` keystroke class, and NOT carried by a
    // child's inherited write edge.
    v(
        "settings",
        ConfigWrite,
        Status,
        App,
        "settings [open|close|toggle|section <name>]; `settings set|unset <key> [value...]`",
        "",
    ),
    v(
        "invoke",
        Write,
        Status,
        App,
        "invoke <action>: fire a menu action by name (enabled-gated; names via `controls menu`)",
        "— an action that writes to the session (`Paste`) is refused like every input verb \
         while input waits unread; a leading unread=ok fires it anyway",
    ),
    // `invoke`'s class and lane: `confirm yes` finishes the close an `invoke` parked
    // in the window (aterm-gui `close_confirm`), so it retires sessions and is in
    // the halt set beside `tab`.
    v(
        "confirm",
        Write,
        Status,
        App,
        "confirm yes|no: answer a close or quit waiting in the window",
        "— on Windows a close or quit that would end a running job asks in the window \
         instead of a dialog, and every verb keeps answering while it waits (`controls \
         front` shows `confirm open=true`). `OK answered=<yes|no> \
         kind=<quit|close-window>`: `yes` runs the close, and a confirmed quit ends the \
         process, so its `OK` is the instance's last reply; `no` keeps everything. Only a \
         question a control client raised (`invoke CloseTab`, `invoke Quit`) is the wire's \
         to answer: one a person raised at the window is `ERR a quit is waiting for the \
         person at the window` (or `a window close`), and with none standing it is `ERR \
         nothing is waiting for an answer`. A keystroke never answers it: `key enter` goes \
         to the shell as always. Anything else is `ERR usage: confirm yes|no`",
    ),
    // Runtime-only per-session visual toggle (nothing durable is written); the
    // observability face scripts/tests read (`status` = one Status line).
    v(
        "rain",
        Write,
        Status,
        App,
        "rain [status|on|off|toggle]: matrix rain for the focused window's front session",
        "(status prints config_enabled= session_override= effective= engine= active= \
         scope=window|focused-pane focused= animating=, plus a live engine's weather= density= \
         tick= scanned= material= emitting= vis= drain= seq= streak= diag)",
    ),
    // Read-only observability for an effect that is otherwise nearly INVISIBLE
    // by design: PRISM WAKE answers program output with a ~0.10-coverage comet
    // that lives under a second, so "off", "W11-demoted", "suppressed" and
    // "resting" all look identical on glass. No write form — the knobs are
    // durable config (`settings set output_streak.…`).
    v(
        "streak",
        Read,
        Status,
        App,
        "streak [status]: output-streak (PRISM WAKE) state for the focused window",
        "(prints config_enabled= sound_key= motion_animates= focused= fx_focused= \
         serious_sound= sounds_master= volume= engine=none|live panes= active= intensity= \
         tail= max_streaks= idle_secs=; `motion_animates` folds the W11 unfocused \
         demotion, which outranks even motion=full; `fx_focused` is the focus input the \
         render tick actually uses (raw focus OR a live typed wake OR an in-flight \
         recording), so a window can animate with focused=false; `panes` counts the \
         composed path's per-pane engines, which is where a SPLIT's streaks live)",
    ),
    // Runtime-only, one-shot, keystroke-gated: ARMS a rainbow-kitty
    // celebration that fires on the session's next keyed edge (a green command
    // block, or a keyed Enter) — latch, don't act. Nothing durable is written
    // and nothing lights until the edge; `fx status` is the read face.
    v(
        "fx",
        Write,
        Status,
        App,
        "inspect or arm a one-shot rainbow-kitty celebration on the front session",
        "(fx [status|celebrate [sig=<char>] [bars=<1..4>] [on=green|enter]]; \
         `celebrate` LATCHES a sing-along that fires on that session's next exit-0 command \
         block — OSC 133/633 `D` status 0, the same Enter-armed verdict edge the pet and the \
         music box read — or, `on=enter`, on its next keyed Enter (a key event or raw bytes \
         ending in a newline; `send`/`turn` count, program output never does). It runs `bars` \
         1.6 s bars (default 2, max 4): the spine driven to full, a fan of 5/7/9/11 stars per \
         bar, then a drop fan and a beat-quantised \
         outro on do so the song ends on the bar line. `sig=` picks the key by character \
         (default `C`; every character is a distinct verse). Refused while an arm is pending, \
         inside the 30 s cooldown of the last arm, or when the cursor trail is disabled, \
         uses another style, or Serious Mode suppresses companions. Every form answers the same status line: `armed=none|green|enter sig= bars= \
         live= bar= drive= cooldown_ms= style=` — `live` is a fired run on the stage, `bar` \
         its current bar, `cooldown_ms` the wait before the next arm is admitted)",
    ),
    // Read-only observability for an effect that is otherwise audible-only: the
    // tone-of-typing mood steering the trail synth's melody, AND the
    // AUDIBILITY ORACLE — the one verb that must be able to refute "sound is
    // broken" without a second reading. No write form — the knob is durable
    // config (`settings set tone_melody`).
    v(
        "tone",
        Read,
        Status,
        App,
        "tone [status]: tone-of-typing state for the focused window",
        "(prints tone= effective= knob= sounds= volume= audio= active= \
         window_chars= inferences= dropped= seam= engine_sound= trail= focused= \
         serious_sound= motion_stage= shed= revives= reopens_left=; `effective` is what the synth is \
         stamping, \
         `inferences` separates \"the model ran and said technical\" from \"the model never \
         ran\". `seam=open` means a keypress makes a sound RIGHT NOW; `seam=closed:<reason>` \
         names the gate that stops it, one of sounds-off volume-zero trail-off serious-mode \
         unfocused host-inert host-wedged resize-quiet engine-silent — so a silent session is \
         one reading, not a bisection of six settings. The seven fields after it are the \
         witnesses that verdict was computed from, including the pair that used to need a \
         second verb: motion_stage= and shed= (a dark trail under load shed or Reduce Motion \
         still sounds its keys — a key-time click costs no GPU — so motion_stage=reduced \
         shed=0.00 beside seam=open is CORRECT, not a contradiction). audio= is the audio \
         worker's own state, not merely that a channel exists: opening = ingress open and no \
         device opened yet (it opens lazily on the first cue, so this is a fresh process, not \
         a fault), live = the platform queue actually started, paused/stopped = parked, \
         failed = a platform call failed, reopening = a fault was seen and the next cue \
         opens a fresh device inside the reopen budget (recoverable, not a verdict), \
         wedged = stuck inside one platform call and \
         dropping cues (dropped= counts them, revives= counts the worker restarts that reset \
         that counter), inert = it can never sound. reopens_left= is the DEVICE reopen \
         budget: a transient CoreAudio fault (a device switch, a coreaudiod restart, a wake \
         from sleep) now costs one reopen and the next key sounds, where it used to be \
         terminal for the process; reopens_left=0 beside audio=failed is the one honestly \
         permanent reading. The typed window's TEXT is never reported)",
    ),
    // The cursor cat's COLLECTION, and the direct way to put one on. `Write`
    // rather than `Read` because the wear form changes what the user sees and
    // stamps the durable collection; not `ConfigWrite`, which is reserved for
    // `aterm.toml` and the security knobs a keystroke edge must never reach.
    // The cursor cat's COLLECTION, and the direct way to put one on. `Write`
    // rather than `Read` because the wear form changes what the user sees and
    // stamps the durable collection; not `ConfigWrite`, which is reserved for
    // `aterm.toml` and the security knobs a keystroke edge must never reach.
    v(
        "kitty",
        Write,
        Lines,
        App,
        "kitty [wear <key>]: the cursor cat collection, and which one to wear",
        "Bare `kitty` lists the collection, one `cat key= coat= iris= age= seen= worn=` row \
         per collected cat, `worn=1` marking the one on the cursor now. `kitty wear <key>` \
         puts that cat on and answers `worn key= coat= iris=`. The key is the row's own \
         `key=`, so the list is the menu. THIS IS THE DIRECT SWITCH: Favourite This Kitty \
         pins the cat that would ride ANYWAY (the focused window's tenured program cat, \
         else the launch kitty), so wearing a different one used to mean making it appear \
         first — relaunching for a fresh launch kitty, or running a program long enough to \
         earn tenure. The collection elects by GREATEST pin stamp, so a wear is monotone: \
         it needs no unpin, and it survives a merge of the collection from another \
         instance. Refused, never silent, when effects or `[sparkle_words.feline]` are off \
         (there is no cat to dress) or when no row answers to that key.",
    ),
    // Read-only observability for the cursor-trail engine: the last N
    // licensed/declined verdicts from the fixed-size admission diagnosis ring,
    // and (`trail status`) the engine's standing state. The one-command face
    // of what the rainbow-trail blackout hunt did with ATERM_TRACE_SPAWN
    // stderr logs, and of what "I don't see the rainbow cursor trails" needed a
    // video recording for.
    v(
        "trail",
        Read,
        Lines,
        App,
        "trail [<n>]: the focused window's last <n> cursor-trail spawn-seam verdicts",
        "(default all, ring cap 32), newest last — one `admission seq= phase=licensed|declined \
         reason= age_ms= origin= target= alt= licence=` row per judged cursor move, from the \
         engine's fixed-size DIAGNOSTIC ring. `licence=` names the class that admitted a \
         licensed row: `key` (a press hint — typed, Backspace, nav, Return, a composer \
         newline, Tab or ⌃V as a gesture), `inflight` (no hint was fresh and the unpaid \
         presses still waiting on the row licensed the batch — a stalled prompt catching \
         up), `insert` (a DELIVERED insert, \
         Rainbow Kitty only — a file drop, ⌘V, and into the tab on screen the `paste` verb, \
         `paste-bin`, `turn`'s paste phase, an unguarded `key tab` or `key ctrl+v` — laid as \
         one sweep when its bytes provably landed; `send`/`feed`, a guarded `key if=` and \
         any input routed to a background session stamp nothing and stay dark by \
         contract), `rewrite` (the program pulled the caret back inside that insert's span \
         and the ribbon retracted to it), or `none` on a decline. A move paints only if a \
         keypress or a delivered insert LICENSED it, so a \
         decline carries one of five reasons: `no-fresh-hint` (no key hint was fresh — the \
         move was program output nobody's fingers asked for; under Rainbow Kitty also a \
         fresh typed stamp whose press was already paid, since a stamp licenses light \
         only while the credit ring still owes a cell), `no-credits` (a multi-cell \
         coalesce outran the press CREDIT budget), `off-shape` (licensed and classified, but \
         the style's shape gates laid nothing), `program-row` (the anchored-echo lane refused \
         a row that has advanced keylessly, or one contesting a fresher row's echo — a \
         spinner/status row must never spend a typed stamp; also a key's echo that lands \
         on a glyph no pressed key typed — zsh's Ctrl-R status row ending on its fake \
         cursor `_`, or the visible caret walked along the search's match row — whose \
         presses are then forgotten), `hidden-relocation` (a key was \
         pressed, but the caret reappeared from behind a repaint's hidden bracket too far or \
         too late for the bounded hide-bridge to name a source cell — the one refusal that \
         happens before the licence gate, so it is reported here rather than counted twice). \
         Every observed move is counted \
         exactly once: \
         licensed + declined is the number of cursor deltas the seam has judged. \
         `trail status`: one standing-state row instead — `trail style= resolved= \
         config_enabled= effective= focused= motion= motion_stage= shed= intensity= \
         sound_seam= \
         licensed= declined= last_decline_reason= spawns= ribbon_active= ribbon_look= \
         ribbon_segments= ribbon_hue_bands= ribbon_drawn= ribbon_curtain_ms= \
         field= sparks= momentum= \
         momentum_display= momentum_glow= flow= combo= combo_best= glow_active= pet_active= cat_active= \
         block_fill= block_fill_rgb= block_fill_base= block_fill_base_from= \
         pet_action= pet_content= pet_pending= pet_body= pet_focus= pet_reason= \
         pet_anchor= pet_event_seq= pet_pose= inserts_delivered= inserts_lit= \
         inserts_retracted= last_insert_cells= inflight_licensed= inflight_forgotten= \
         credits= swallowed_no_echo= park_returns= park_flushed=` (every gate \
         from the config knob to the glass, in the order the frame path walks them, plus \
         the cumulative tally the ring has forgotten — `licensed=0 declined>0` blames the \
         licence and names why, `licensed>0` over a dark screen blames everything \
         downstream of it). `sound_seam=` is the KEY-TIME CLICK, adjudicated \
         beside the light rather than inferred from it: `shed=0.00 \
         intensity=0.00 sound_seam=true` is a window whose trail is dark for a \
         performance or accessibility reason and whose TYPING IS STILL HEARD (a \
         key-time click costs no GPU), while `sound_seam=false` beside \
         `focused=false` is the one dark case that is supposed to be silent. \
         `ribbon_drawn=` is THE FACT BESIDE THE CLAIM, and \
         `ribbon_curtain_ms=` is why the two can differ: `ribbon_segments=` counts the \
         planned boundaries the row is willing to CLAIM are lit, floored once for the \
         arc's dimmest stop, while `ribbon_drawn=` counts the last frame's own quads \
         whose composite a glass census would read as band ink, and \
         `ribbon_curtain_ms=` is the milliseconds left in a falling curtain (`none` \
         when none is falling) — so `ribbon_segments=0 ribbon_drawn>0 \
         ribbon_curtain_ms=` a number is a band still gathering into the hand at a \
         level under the claim floor, not a band that is gone. Both are 0 on every \
         style but rainbow kitty. The `inserts_*` four are the DELIVERED INSERTS, and advance \
         under Rainbow Kitty only: how many the host reported delivered (a paste's completed \
         write, a bare Tab's or ⌃V's dispatch), how many the seam lit as one sweep, how many \
         placeholder rewrites it retracted, and the last one's width in cells (priced from \
         its text, else the 32-cell bound) — `inserts_delivered>0 inserts_lit=0` after a \
         drop means the bytes landed and no echo was laid as the insert: refused, or not \
         seen inside its 2 s window (read the ring's `licence=` and `reason=`). The \
         `inflight_*` pair and `credits=` are the PRESSES IN FLIGHT: a typed key waits on \
         its row until the row echoes or a move it cannot explain forgets it (bounded at \
         ten seconds) — `inflight_licensed=` counts batches the waiting presses alone \
         licensed after a stalled prompt caught up (the ring row reads `licence=inflight`), \
         `inflight_forgotten=` counts the edges that dropped a non-empty pool (a backward \
         or cross-row hop, a forward hop the share rule (`no-credits`) or the cap refused, \
         a Return's own row change, an arrow, a kill — a same-row forward hop refused \
         `no-fresh-hint` keeps its one credit for the next key, and a glyph's echo that \
         lands with Enter pressed behind it spends neither the Enter nor the pool), and \
         `credits=` is the pool right now — \
         `credits>0` over a silent row is a stall in progress. \
         `swallowed_no_echo=` counts the presses that banked NOTHING because the pty was in \
         canonical no-echo mode at the key (`read -s`, `sudo`, an `ssh` passphrase, `passwd` \
         — iTerm2's password-mode rule, read off the master's termios): the tty will never \
         echo them, so they are neither a licence nor a credit, and a dark password prompt \
         with this count rising is the rule working. A raw-mode program (Claude Code, vim, \
         a shell prompt under readline) echoes for itself and is never counted here. \
         `park_returns=` / `park_flushed=` are the HELD PARKS (Rainbow Kitty only): a \
         same-row backward move a key stands behind — Ink parking the caret at the start \
         of the input before it rewrites the row — is held for one stamp window rather \
         than judged at once; `park_returns=` counts the rewrites that came back past the \
         park's origin and were judged as one echo from it, `park_flushed=` the parks \
         judged as the plain retreat they were (no return: silence, a Backspace, a scroll, \
         the next unrelated move). The \
         `block_fill*` four are the BLOCK CURSOR's body, which no \
         other field covers: a style can take the caret away from the terminal entirely, \
         and `glow_active=false pet_active=false` over a tinted block is what that looks \
         like from every other gate. `block_fill=` names the owner the frame actually \
         painted (`rainbow`/`forge`/`phaser`/`bolt`/`comet`/`droplet`/`beamrod`, or `none` \
         for the terminal's own cursor colour), `block_fill_rgb=` is the hex it drew, and \
         `block_fill_base=`/`block_fill_base_from=` are the colour that body was built \
         FROM and which source supplied it (`cursor_color`/`trail_color`/`style_identity`, \
         or `white` — a CursorColor-based owner handed no pinned cursor colour builds from \
         the theme-polar white, and the shipped-default rainbow style takes that path) — \
         so a caret that ignored OSC 12 is separable from one that honoured it. \
         `momentum_glow=` is the TYPING-MOMENTUM GLOW's own decayed value (0.00-1.00, the \
         `cursor_momentum_glow` halo that warms the caret and pins its blink) — not \
         `momentum=`, which is the rainbow engine's cat momentum — so a warm caret reads \
         beside a nonzero number instead of `momentum=0.00`. \
         `flow=`/`combo=`/`combo_best=` are the row's one reading about the PERSON: \
         `combo=` counts keys typed at speed (the eased spine at 0.8 or above) with no \
         delete, and zeroes on a delete, a kill, or a hand that dropped below speed; \
         `flow=` is that run's climb toward the 24-key bar the theme OPENS at, 0.00-1.00; \
         `combo_best=` is the window's high-water mark, which an exit never resets. A \
         driver that reads `flow=1.00` is looking at a human mid-flow and can hold its \
         turn. The three are 0 on every style but rainbow kitty, whose spine prices them. \
         While rainbow kitty owns the frame the row ends with `v2_quads= v2_halos= v2_stars= \
         v2_meteors= v2_bridged= ribbon_retired= ribbon_followed= ribbon_follow_missed=` — the \
         frame's quads, halos, live stars and meteors; `v2_bridged=`, the cells the engine's \
         echo ledger relit for a \
         late echo the ring scored `declined`; `ribbon_retired=`, the window's cumulative count \
         of ribbon cells taken off by CONTENT: the host saw the glyph under a cell change (a row \
         rewritten with other text — the band melts in 120 ms) or go (a submit that cleared \
         the composer, a row cleared and not put back — the band is released to its own \
         swoosh, drawn into the hand over 0.64 s) — the number that says a band went out \
         because its text moved out from under it or went, as against expiring (a redraw that \
         puts the same text back counts nothing, and a caret relocation the gate declined \
         over a row whose text still stands retires nothing: only the mirror moves); \
         `ribbon_followed=`, its twin: the cumulative count of ribbon cells the FOLLOW PASS \
         carried to another row WITH their text (an input box re-laid one or two rows up or \
         down with the same glyphs at the same columns — Claude Code's bottom-anchored \
         composer growing a row without a scroll), clocks intact, nothing melted; a run \
         carried off the caret's row (the box grew under a typing hand) then flows into the \
         fold and is gone 1.50 s after the fold; and `ribbon_follow_missed=`, the cumulative \
         count of ribbon cells whose glyphs left their row while their run's text ARRIVED on a \
         neighbouring row (two or more of its glyphs, and no fewer than half of those neither \
         already standing there when they were typed nor carried by a wrap to the start of \
         their own row, now at their own columns) and the \
         follow pass still did not carry it, because the arrivals were not one block — each \
         such cell is left to the content witness where it stood, which melts it (or, for a \
         run with glyphs still standing, releases it) instead of the band going with its \
         text, so `0` is the healthy reading and a climbing count over a growing composer is \
         the band vanishing in a blink. \
         In a `--headless` instance the engine ticks only while a capture drives its clock (`image` \
         after each key, or a `video`), and a caret on ROW 0 has no sky band there (no chrome \
         head band above the grid), so `v2_stars=0` on row 0 is the geometry, not a dark trail \
         - judge stars from row 1 or lower. Read-only; typed text is never reported. \
         `pet_action` names the resident brain's current action \
         in lowercase (`none` without a drawn body), `pet_content` is earned contentment \
         in 0..1, and `pet_pending` counts queued clicks/strokes. `pet_body` is the last \
         drawn body as `x0,x1,y0,y1` in frame pixels, right/bottom exclusive, or `none`; \
         `pet_focus` and `pet_reason` name located console attention and its cause, \
         `pet_anchor` names its command block or `none`, `pet_event_seq` identifies \
         the coalesced source observation within that reason, and `pet_pose` names \
         the last resolved full-body art pose. These are observations, and reading \
         them never advances the pet",
    ),
    // Read-only observability for SELECTION/VIEWPORT CUSTODY: which of the eleven
    // custody-moving events last fired. Several of them leave identical state behind
    // (an auto-repeat tick, a bare modifier and a key release each change nothing at
    // all), so `scroll` and `selection` cannot answer "why did my selection
    // disappear?" between them — only the engine's own record can. No write form: the
    // record is written by the seams that make the decision.
    v(
        "custody",
        Read,
        Status,
        Session,
        "custody: why the reading position or the highlight last moved",
        "— one `last=<transition|none> event=<0-8|-> changed=<transition|none> \
         took_selection=<transition|none> offset= \
         owner=user|tail selection=yes|no scrollback=` line naming the PressCustody transition \
         the engine recorded: a press (TypingPress, RepeatPress, InertPress, ReleaseEvent), a \
         gesture (UserScroll, UserScrollTowardLive, SnapToLive, UserSelect, UserClear), or \
         output (OutputAtLive, OutputWhileReading, OutputDamagesTheSelectedRows[InPlace], \
         OutputTookTheSelectionUnattributed[InPlace], OutputInvalidatesTheCoordinateSpace). \
         `last` is the most recent event of any kind; \
         `changed` is the most recent one that actually TOOK the offset or the highlight, so \
         ordinary shell output cannot bury the answer. `none` means nothing has moved custody \
         yet. Read-only; reports no screen content",
    ),
    v(
        "hover",
        Write,
        Status,
        App,
        "hover <on|off>: set the drop-target highlight (no toggle form)",
        "",
    ),
    // session lifecycle
    v(
        "spawn",
        Write,
        Status,
        App,
        "spawn [window=] [raise=<t|f>] [cwd=] [split=<v|h>] [identity=<name>|-] [connected=…]: OK <sid>",
        "mints a session: a tab, or `split=v|h` divides the focused pane; the sid is immediately \
         addressable. AIM it with window=<id> (an id from `inspect app/v1 tabs`) or `@<sid> \
         spawn` (the window hosting <sid>; window= wins); split= then divides THAT window's \
         focused pane. raise= defaults to true when no window was named (the `aterm new-tab` \
         attach contract) and FALSE when one was - an agent aiming at a background window is \
         not asking to see it; say raise=true to insist. Unknown id: `ERR no such window <id>`. \
         A --headless instance owns logical window 0 (the one `ls`/`windows`/`dims` name), so \
         `window=0` and `@<sid>` aim there exactly as at a real window - only `window=<other>` \
         is `ERR no such window` - and the raise is simply a no-op (no OS surface). The \
         connected form, `spawn \
         connected=controlled|controller place=window|tab of=<sid> [cwd=<path>]`, also mints a \
         `both` session connection with of=<sid> (controlled: of= drives the newborn; \
         controller: the newborn drives of=, its shell gets ATERM_OBSERVE_SESSION_ID) - \
         Owner-only, of= mandatory, no window=/raise=/split= beside it, place=window is `ERR \
         headless` with no GUI. `identity=<name>` (session identities): the newborn's agents \
         keep their login, settings and skills in `<state>/identities/<name>/` instead of the \
         human's `$HOME` - `CLAUDE_CONFIG_DIR`/`CODEX_HOME` point into it, set AFTER the env \
         strip, so neither login leaks into the other. The name folds to lowercase \
         (`[a-z0-9][a-z0-9._-]{0,63}`); the directory is created ONCE on first use (0700, \
         primed with the aterm skills) - this verb is the \
         only create path, and a create failure is the reply (`ERR identity <name>: <why>`). \
         `@<sid> spawn` (and the connected form's of=) INHERITS the aimed session's identity; \
         `identity=-` opts out; a plain spawn has none. Spawn-time and immutable: `meta set \
         identity` is refused. Owner-only like connected= (an identity is a directory of \
         credentials). `identities` lists and forgets them",
    ),
    v(
        "close",
        Write,
        Status,
        Session,
        "@<sid> close: retire that session and close its pane - the death half of spawn",
        "`OK closed <sid>` means the session LEFT the registry: its pane is gone, its PTY was hung \
         up, and the ledger has the row (`exits` -> `reason=ctl-close by=<caller>`). ONE PANE: a \
         pane of a split collapses the way Cmd-W collapses one and its sibling panes keep their \
         shells; the tab goes only with its last pane, and the window only with its last tab. An \
         unresolvable sid, or one no window holds, is `ERR no such session`. A job running in \
         the pane refuses the close and nothing moves: `ERR close refused (a running job in that \
         pane)`, or `ERR close refused (a running job armed the last-tab confirm)` when that pane \
         is its window's last - finish or interrupt the job, then close again. Only the closed \
         pane's job counts: a sibling's keeps running, and a session another tab or window also \
         shows is not hung up. That refusal is the verb's WHOLE confirm: a wire close never \
         shows a dialog, so a driver is never left waiting on a click it cannot give (a \
         `--headless` instance never refuses). Closing a window's LAST tab DEFERS the window \
         teardown that retires the session, so the verb waits for it before answering. A \
         session still registered after its close says what holds it: `ERR close deferred \
         (another window still displays this session)` or `ERR close deferred (another tab \
         still displays this session)` - the pane is gone and that viewer keeps the session, so \
         close it there too (one close per viewer); `ERR close deferred (the window teardown \
         has not run)` - a native or document close barrier held the window; `ERR close \
         deferred (the session outlived its window's teardown)` or `ERR close deferred (the \
         session outlived its pane)` - no window shows it any more; `ERR close deferred (a \
         pending update handoff is being cancelled first)` - the close runs when that \
         cancellation lands. `ERR session view changed during close` = the tab's shape moved \
         under the close and nothing was retired, so retry; `ERR close refused (the pane is \
         still open)` = the collapse did not take and nothing was retired. The verb takes NO \
         argument: anything after it is `ERR usage: close`, never a close. And when that LAST tab is the \
         LAST window's, the teardown ENDS THE PROCESS: `OK closed <sid>` is this instance's \
         final reply, the memory-only `exits` ledger and any `subscribe` watch's `closing` / \
         `exited` frames go with it, and the next verb gets a connect error, not an `ERR` line",
    ),
    // waits
    v(
        "ready",
        Read,
        Status,
        Session,
        "block until the session is alive and idle",
        "",
    ),
    v(
        "await",
        Read,
        Status,
        Session,
        "await <idle|seq|match|gone|block|inbox|consent|momentum|agent> [args] [timeout=<ms>]: wait",
        "Full grammar: `await idle <ms>` (no output for that long), `await seq [<n>]` (the \
         content sequence passed <n>; bare = the next change, so `await seq <n> timeout=0` is the \
         cheap dirty check), `await match <re> [rows <a> <b>]` (a visible row matches), `await \
         gone <re> [rows <a> <b>]` (NO visible row matches — the inverse of match, for a turn \
         whose end is a row LEAVING: a coding agent keeps its composer glyph on screen while it \
         thinks and can sit static for seconds mid-turn, so neither `match` on the prompt nor \
         `idle` says the turn is over, but its busy footer disappearing does — `await gone \
         esc.to.interrupt`, ONE whitespace-free token, since the wire never quotes; \
         level-triggered like match/seq, so a surface already clear of the \
         pattern latches at arm; a `rows <a> <b>` span must meet a row of the current grid — \
         `ERR bad rows` otherwise, since an empty span scans nothing and nothing-scanned is \
         never \"clear\"), `await block` (the running \
         command completed), `await momentum <floor 0..=1>` — THE YIELD: latch once the typing \
         momentum of the window hosting this session (the `trail status momentum=` number) has \
         decayed to the floor, so an agent waits for the human's ribbon to exhale before typing \
         into a shared session; solved ANALYTICALLY from the ribbon's release law (v·e^(−t/2)): \
         one reading, one kernel timer at the crossing, a re-read when it fires (a ribbon the \
         human re-lit gets its next crossing), never a poll — answers `OK momentum <v>`, and a \
         session no window hosts reads 0.00 at once; `await inbox since=<id> [kinds=<k,...>]` — the FABRIC predicate: \
         it latches on an inbox row with id > `since` of an accepted kind (default: every kind \
         but `note`), or on a `hold` transition when `hold` is one of the listed kinds. Monotone, \
         so a row the agent chose to ignore cannot latch the same wait twice. `await inbox \
         re=<off> [since=<id>] [kinds=<k,...>]` narrows it to THE REPLIES TO ONE POST — a row \
         carrying that `re=` of any kind: an `answer`, a `report`, the recipient's receipt \
         (`ack … verdict=`), the asker's own bridge's `expired` — and `since=` may then be \
         omitted. `await agent <word>[,<word>…]` — the SERVER'S agent verdict (`status agent=`, \
         words busy|prompt|question|idle|survey|unknown|-, and `wall` for any wall or \
         `wall:<kind>` for one): LATCHED, so a verdict already in \
         the set answers at once, else it parks until the status sweep publishes one (no \
         watcher armed, no screen read by the waiter); answers `OK agent <word> rev=<n>`, and \
         an unknown word is `ERR usage`. And `await consent` \
         — the macOS privacy posture of THIS session: the instance's Full Disk Access state, this \
         session's `fs_consent=` and its `attribution=` (see `privacy`). Its deadline starts when \
         the request arrives. The first completed observation establishes the baseline; a cold \
         pending check cannot establish it. It latches on the first completed observation that \
         differs from that baseline; a refresh still pending cannot latch. Its default `timeout=300000` is finite on purpose — \
         the system consent dialog it waits behind never expires, so an agent must not park on an \
         absent human forever; a timeout there is an ordinary timeout reply, not an error. A \
         latch says aterm's own posture CHANGED, not that a human \
         answered a dialog (aterm cannot observe the answer). Rechecks obey the configured interval \
         and a polling tick; macOS probe duration has no guaranteed bound. The kernel forms \
         (idle/seq/match/gone/block) answer `OK <predicate> <seq>` on a latch, and the inbox ROW \
         latch `OK inbox <id>` — the newest matching inbox row id, which is your next `since=`, \
         not a screen seq; `await consent` latches as `OK consent fs_consent= fda= attribution= \
         elapsed_ms=` (no seq), and `await inbox`'s hold-TRANSITION latch answers `OK inbox \
         hold=0|1`. Every form answers `OK timeout` otherwise, which the client exits 124 on. \
         One control lane per parked wait, so park at most one per driver.",
    ),
    v(
        "wait",
        Read,
        Status,
        Session,
        "block until the running command completes (OSC-133)",
        "",
    ),
    // streaming
    v(
        "subscribe",
        Read,
        Push,
        Session,
        "subscribe @<sel>[,...]|@* <streams> [since=][every-frame]: push DELTA/EVENT/GAP/BYTES/MAIL;",
        "streams=screen,cursor,cells,bytes,events,mail,sessions, at least one of them (a modifier-only \
         list is `ERR usage`); events = the per-target digest (`EVENT <local> turn|block-complete|\
         meta|title|bell …`, `EVENT <local> agent <word> rev=<n> gen=<e.s> fp=<hex16>` each \
         time the server's agent \
         verdict moves (`status agent=`), `EVENT <local> human` when a person starts typing, \
         clicking or scrolling in it through a window after 30 s without (`status human_ms=`), then, as the \
         session is retired, `EVENT <local> closing reason= by=` \
         — the `exits` row, and this watch is the only wire path that carries it — before its \
         one `EVENT <local> exited`, not necessarily adjacent: a title or bell frame of the same \
         watch can land between the two — and `GAP <local> events-dropped=<n>` first when the \
         timeline evicted records this watch had not been shown); sessions = instance lifecycle (`EVENT * \
         session-created <sid>` / \
         `EVENT * session-exited <sid> reason=<shell-exit|ctl-close|ui-close|window-close|app-quit|\
         unknown>` for sibling spawns/exits, no `ls` polling — the reason is the `exits` ledger's, \
         a trailing additive token; `app-quit` is reserved, not produced today; on `@*`, which \
         watches up to 256 sessions, adopts every other one — left over at the handshake or \
         created later — once the cap has room, and writes each watch's `sub <local> <sid>` ack \
         only after that watch is seeded, a session's `session-created` comes after its ack (or, \
         for one already gone, beside its `session-exited`), except that one the 256-target cap \
         leaves unwatched is announced at once as `session-created <sid> watch=deferred` and, if a \
         slot frees, acked with no second announcement — the marker rides that one line, so a \
         reader that must know what it does not watch goes by the acks it holds; and `EVENT * \
         fabric-retire <sid>` for a `fabric retire` request the bridge is to act on) and is OWNER-ONLY \
         because it reports \
         the whole roster, not just your targets — a scoped edge asking for it gets `ERR denied`; \
         add `timestamps` (alias `ts`) INSIDE <streams> (`cells,ts`; trailing is `ERR unknown \
         subscribe arg`) to prefix frames with `T <local|*> <t_us>` lines (video's clock) so the \
         stream is a timed frame source — at most one per channel per wake, tagged `<local>` for \
         session frames and `*` for `sessions` events, so the second token is not always numeric; \
         add `trim` INSIDE <streams> too (`screen,trim`; trailing is `ERR unknown subscribe arg`) \
         to stop each screen DELTA after its last non-blank row — `screen <nrows>` is then the \
         count sent (inert without screen); mail = one `MAIL <local> id=<n> off=<n> from=<p> \
         kind=<k>[ re=<n>][ topic=<t>]` line per row DELIVERED into that session's inbox, \
         METADATA ONLY (no \
         body, ever — read the words with `inbox get`), preceded by `GAP <local> mail-dropped=<n>` \
         when the ring evicted rows the subscriber had not been shown; it is a LIVE stream seeded \
         to the ring's high, so it replays no backlog (`await inbox` is what reads history), and \
         it narrows with `mail:kinds=<k,..>`, `mail:from=<class|principal>` and/or \
         `mail:topic=<t>` (broadcast rows on that topic only) — class is \
         human|agent|service|other by the sender's `h-`/`s-`/`a-` prefix, and an unknown kind or \
         key is `ERR usage`, never a subscription that silently matches nothing. `mail` needs no \
         authority beyond this verb's: it is the push face of `inbox`, which is the same \
         `ReadScreen`",
    ),
    // fabric messaging — the per-session INBOX RING, this session's outbound posts,
    // the halt, and the BRIDGE-plane verbs. `inbox`/`inbox get`/`inbox seen`/`post`
    // are ordinary scoped verbs an agent inside the session calls; `deliver`/
    // `outbox`/`outbox sent` are `BridgeOnly`, so no token reaches them (see
    // [`Access::BridgeOnly`]); `hold` is `OwnerOnly` — the local owner's halt from
    // the Owner token, the fleet's from the bridge, told apart in the handler.
    va(
        "topic",
        Owner,
        Lines,
        Session,
        OwnerOnly,
        "topic add <t> [since=head|@<off>] | topic drop <t> | topic ls: broadcast opt-ins",
        "RECEIVER-SIDE OPT-IN for `post to=say:<topic>`. A record on `say/<topic>` reaches \
         this session only because this session asked for the topic, so a fleet-wide \
         broadcast cannot put a word in front of an agent that did not want it; the set is \
         EMPTY by default and an empty set receives nothing. `ls` (and the bare form) answers `OK <n>` then one \
         `topic <t> since=<head|@<off>>` row each; `add` and `drop` answer ONE STATUS LINE \
         (see `framing_of`) and each pushes `EVENT <local> topic add|drop …` on the `events` \
         digest, which is how this session's bridge learns of the change AT ONCE (a session its \
         push lane does not watch, past the 256-target cap, has no watch to push it; the bridge \
         reads the `topic ls` of every session whose watch it holds no ack for on every 2 s \
         roster round). On a watched session a `drop` then an `add` are two events in order, \
         so the second's `since=` is read; a polled session is read as its final set, so a \
         drop and re-add between two rounds keeps the topic's first `since=`, and its \
         `since=head` resolves when the round reads it. `add` answers `OK <t> since=<head|@<off>> added=<0|1>` — \
         `since=head` (the default) takes only records published from then on, `since=@<off>` \
         replays the topic from that broker offset so a session joining late can read what \
         it missed. `drop` answers `OK <t> dropped=<0|1>`. The topic is \
         `[a-z0-9][a-z0-9._-]{0,31}`; anything else is `ERR usage`. A delivered broadcast is \
         an ordinary inbox row — same ring, same per-sender quota, `task`/`control` still demoted \
         from a principal this session does not accept — carrying `topic=<t>`, and it is \
         pushed on `subscribe … mail` with `topic=<t>` too. OWNER, not Read: adding a topic \
         changes what reaches an agent's inbox, which is the halt's authority class rather \
         than a read's. The set survives a bridge restart and a seamless update.",
    ),
    // The TYPED RE-KEY (2026-09-26, aterm-gui `shell_rekey::typed`): the live agent
    // upgrade heals a shell whose integration is degraded in the relaunch line it
    // types at that shell's prompt. Owner-only because it hands out a key that
    // lets a session's OSC 133/633 marks through; Session target because it names
    // the tab; `Owner` op because no fine op authorizes minting mark authority.
    va(
        "rekey",
        Owner,
        Status,
        Session,
        OwnerOnly,
        "rekey shell=<pid> | withdraw: re-key a degraded tab",
        "For a tab whose `status integration=` reads `degraded` — its shell signs its OSC \
         133/633 marks with a nonce this window does not hold, and its script predates the \
         re-key channel — while `<pid>` leads the tab's foreground process group (its SHELL \
         holds the terminal, not a program): a fresh key is written to a one-use file \
         (`0600`, created exclusively, never through a symlink, in the private control dir) \
         and authorized as one the shell has not taken, and the reply is `OK rekey ttl=<s> \
         path=<path>` — the PATH, never the key, the path last and whole. A line typed at the \
         shell's prompt then reads the file into the script's globals and removes it (the \
         harness's relaunch line does — the live agent upgrade's, a restart in place's and a \
         relaunch on exit's); `integration=` reads `on` from the first \
         mark signed with the key, not before. THE TYPED UPGRADE (2026-09-26): for a tab \
         whose shell runs an integration from before LOADERS (`status \
         integration_rev=frozen`, or no loader known for it), the file also names this \
         build's script folder and the session's body pointer, and the line sources this \
         build's loader, upgrading the shell in place — issued for such a tab when its \
         integration is `on` too, with the key it already signs with in the file (no key \
         moves, and nothing is authorized). Refused, nothing written: `ERR rekey \
         integration=on` (a working key is never moved: a shell with a loader, whose next \
         integration arrives through its pointer), `integration=off`, `pending` (the \
         re-key channel's own key waits for the shell's next prompt), `ERR pid <n> is not the \
         foreground process group`. `rekey withdraw` settles the waiting key by its file: \
         still there, the file is removed and the authorization the key replaced restored \
         (`OK rekey withdrawn`); gone, the shell read it and the key stays (`OK rekey \
         taken`); `OK rekey none` when nothing waits. The window settles every key itself \
         `ttl=` seconds after issuing it, and when the session closes. Needs an `@<sid>`. \
         POSIX only",
    ),
    v(
        "inbox",
        Read,
        Lines,
        Session,
        "inbox [<n>] [since=<id>] [--peek] [--meta]: this session's message rows",
        "Header `OK <n> hold=<0|1> holder=<p|-> seen=<id> bus_head=<off> oldest_on_bus=<@off|-> \
         dropped=<n> pending=<n>`, \
         then one `msg <id> off=<n> t=<ms> from=<p> kind=<k> \
         trust=<human|agent|relayed|screen> [re=<n> re-id=<id>] [verdict=<v>] [dl=<ms>] [late=1] \
         [demoted=<k>] \
         [via=<p,...>] len=<n> [more=1] [truncated=1] text=<pct>` row per message and one `post \
         <id> to=<> kind=<> off=- len=<n> [key=<token>]` row per outbound post that has not \
         landed yet (`off=` is always `-` on that row: a post with an offset has landed and is \
         no longer listed; `key=` is the post's idempotency key, when it has one). \
         `trust=` is the \
         RECEIVER's verdict on what the content is, never a sender's claim, and it is printed \
         before the text on purpose. `text=` is pct-encoded and cut at 512 B with `more=1`; \
         `inbox get <id>` returns the whole of what this endpoint HOLDS. `truncated=1` means the \
         endpoint never received the rest: the delivering bridge cut the body to fit one control \
         line and `len=` names the true size — `inbox get @<off>` fetches it whole from the bus \
         — and a cut row carries `more=1` too, even when what survived is under 512 B. `dropped=` \
         counts UNHANDLED rows the bounded ring evicted (never silently) — every evicted row \
         above `seen=`, not merely one nobody listed — and `pending=` the unlisted delivered \
         rows this reply did not carry. A DROPPED ROW IS NOT LOST: every one of this session's \
         records from `oldest_on_bus=@<off>` (the lowest offset ever delivered here) to \
         `bus_head=` is on the broker's durable log whether or not the ring still holds it, and \
         `inbox get @<off>` fetches it through the bridge — the offsets in between are shared \
         with the whole fleet, so most belong to other lanes (`ERR no such record`). \
         `verdict=<handled|refused|deferred>` rides a \
         `kind=ack` row: the RECIPIENT's word on the ask/task at `re=`, published by their \
         bridge when they ran `inbox seen <id> <verdict>` (a receipt; `re-id=` names your post). \
         `inbox <n>` selects the NEWEST n rows matching `since=`, \
         returned in increasing id order; it is not a FIFO batch. Without `--peek`, only the \
         returned message rows become LISTED — per-row state used by eviction and the per-peer \
         quota. `--peek` changes no listed state and `--meta` omits `text=`. The HANDLED \
         watermark `seen=` moves only on `inbox seen`, which also LISTS every row at or below \
         its argument (see that entry). Batch consumers must not advance `seen=` across \
         omitted older rows they have not handled.",
    ),
    v(
        "inbox get",
        Read,
        Bytes,
        Session,
        "inbox get <id> | inbox get @<off>: one message's body, by row id or by broker offset",
        "`OK <nbytes>` then that many raw bytes (up to 256 KiB) — the un-PREVIEWED form of the \
         `inbox` row's `text=`, which the row cuts at 512 B. Reading a body moves no watermark. \
         NOT ALWAYS THE WHOLE MESSAGE, and it says which: a body the delivering bridge had to cut \
         to fit one control line is answered `OK <nbytes> truncated=1 len=<true-size>` — the \
         missing bytes are on the bus, and `inbox get @<off>` fetches them. Only the first token \
         after `OK` is the frame length, so the marker does not change the framing. `inbox get \
         @<off>` is the SAME record BY BROKER OFFSET, for a row the bounded ring evicted \
         (`dropped=`), a listing cut, or the delivery cut (`truncated=1`): answered from the \
         ring when it holds the WHOLE row, else FETCHED through the bridge from the broker's \
         durable log — one bounded read on this session's own lane, carried back in chunks when \
         it does not fit one control line — and answered `OK <nbytes> off=<n> from=<p> kind=<k> \
         trust=<t> [re=<n>] [dl=<ms>] [demoted=<k>] [via=<p,...>] [verdict=<v>] [truncated=1 \
         len=<n>]` then the body, whole up to 256 KiB (`truncated=1` only for a record larger \
         than that), the record's fields on the tail because it has no row id: nothing is \
         re-appended, no watermark, quota or event moves. Every one of this session's records \
         from the header's `oldest_on_bus=` to `bus_head=` is fetchable while the broker holds \
         it; the offsets in between are shared with the whole fleet, so most answer `ERR no \
         such record`. The read PARKS for the bridge's next drain (its 250 ms idle tick; up \
         to 2 s under load) and is bounded at 10 s (`ERR timeout off=<n>`); with no bridge that \
         can read it answers `ERR fabric <state> off=<n>` at once; at most 8 reads park per \
         session (`ERR busy`). `ERR no such record off=<n>` is the ONE answer for an offset that \
         is not on this session's lane — another session's record, another face's, or nothing \
         there — and nothing says which.",
    ),
    v(
        "inbox seen",
        Write,
        Status,
        Session,
        "inbox seen <id> [handled|refused|deferred]: advance the HANDLED watermark (and ack)",
        "`OK seen=<id>`, and pushes `EVENT <local> inbox-seen <id> off=<n>` on the events digest. \
         A VERDICT IS A RECEIPT (R8): with `handled|refused|deferred` the event also carries \
         `verdict=<v> kind=<k> from=<p>` for THE ROW THE ID NAMES (not every row the watermark \
         passes — a bare `inbox seen <id>` names no verdict for any of them), and a bridge \
         running with receipts on (the DEFAULT since round 21, whichever way the bridge was \
         set up; `--no-receipts` or `[fabric] receipts = false` turns it off) then \
         publishes `kind=ack re=<off> verdict=<v>` onto the SENDER's inbox lane for an `ask` or \
         `task` row — never for a `note`, a demoted task included — so the sender's `post \
         --wait-ack` returns, their `await inbox re=<off>` latches and their `inbox` lists `ack … \
         re=<off> verdict=<v>`. The receipt is OWED until it is on the bus: the endpoint lists it \
         on the bridge's `outbox` peek until the bridge retires it, so a verdict given while the \
         broker is down or the bridge is being replaced is acked when they return — exactly once, \
         under a producer sequence pinned to it. Once per row per CHANGE of word: the same verdict again acks \
         nothing, `deferred` then `handled` acks twice and the newer word is the answer. A \
         session that only `--peek`s never acks, and `aterm fabric` shows its unhandled mail's \
         age. \
         It ALSO LISTS every row at or below `<id>`, which is a second effect and not a side \
         effect: listing is what the ring counts as read for eviction and what RELEASES the \
         sender's per-peer quota, so an agent that only ever `--peek`s can still acknowledge its \
         mail and keep receiving. Write-gated exactly like `meta set`: it records a decision and \
         reaches no PTY, so it stays answerable while a fleet `hold` is on — a halted agent must \
         still be able to mark the notice read.",
    ),
    v(
        "post",
        Write,
        Status,
        Session,
        "post to=<@<sid>[@<node>]|<principal>|say[:<topic>]> kind=<k> [opts] <text>: send a message",
        "`to=say[:<topic>]` BROADCASTS: ONE record on `/f/<F>/pub/<node>/<sid>/say/<topic>` \
         whatever the number of receivers, and every session on any node that ran `topic \
         add <topic>` gets one ordinary `deliver` from it — same ring, same per-sender \
         quota, `task`/`control` still demoted from a principal that node does not accept, \
         with `topic=<t>` on the inbox row and on the `subscribe … mail` push line. \
         The topic is `[a-z0-9][a-z0-9._-]{0,31}` and anything else is `ERR usage`; bare \
         `to=say` is the topic `say`. The KIND rides in the body for a broadcast (the \
         subject's last segment is the topic), so `say/<topic>` costs one subject per \
         topic rather than one per topic and kind. A sender does not receive its own \
         broadcast unless it added the topic itself. \
         kind is `ask|answer|task|report|note|ack|control`; `re=<n>` names the offset being \
         answered, `dl=<ms>` is an advisory deadline, `via=<p>` marks a relay, and `--wait[=<ms>]` \
         (ON by default for `ask` and `task`) blocks until the bridge reports the record landed \
         and answers `OK <id> off=<n>` — the broker-assigned offset is the correlation id an \
         answer carries back as `re=`. `--wait-ack[=<ms>]` (`ask`/`task` only; implies the \
         landing wait) then ALSO waits for the RECIPIENT's word: their bridge's receipt, `kind=ack \
         re=<off> verdict=<v>`, published when they run `inbox seen <id> handled|refused|deferred` \
         with receipts on, answers `OK <id> off=<n> ack=<verdict> msg=<inbox id>`; the asker's \
         own bridge recording the deadline passed answers `ERR expired id=<n> off=<n>`; and the \
         bound is `<ms>` when given, else `dl=` plus 5 s for that verdict to come back, else 30 s \
         — a timeout is `ERR timeout id=<n> \
         off=<n>`, naming the offset because the post DID land (`await inbox re=<off>` picks it \
         up later; a recipient who has TURNED receipts off never acks, so bound it — that is \
         no longer the out-of-the-box case). \
         `key=<token>` (1–64 of `[A-Za-z0-9._:-]`) is the \
         IDEMPOTENCY KEY, per session: the bridge reserves a producer sequence for the key \
         durably BEFORE publishing and reuses it on any later post under the same key, so the \
         broker's own `(producer_id, producer_seq)` dedup — rebuilt from its log on a broker \
         restart — appends nothing and answers the ORIGINAL offset; that post answers `OK <id> \
         off=<n> dup=1`, one record is on the bus, and it holds across a bridge restart and a \
         broker restart alike (`outbox` is a peek that carries the key on every drain). A key \
         names ONE record: a re-post under it answers the first post's record whatever its own \
         kind, body or `dl=` (and so starts no deadline of its own). Its ADDRESS is still \
         resolved first, against the live roster: a re-post to one that no longer routes is \
         retired like any post the bridge cannot route (`ERR unroutable`, or `ambiguous`), \
         appends nothing, and leaves the key naming its record. The \
         newest 4096 keys per session are kept; a re-post under an older key is a new record. \
         `dl=<ms>` on an `ask`/`task` is a DEADLINE the asker's OWN bridge keeps (the broker \
         holds no timers): when it passes with no `answer|report|ack` carrying `re=<off>` on \
         the asker's lane (a reply delivered to another session settles nothing), that bridge \
         puts `kind=expired re=<off> dl=<ms>` in the asker's inbox \
         — exactly once, checked against the bus before it is written — a reply arriving after \
         it is delivered `late=1` (by the bridge that recorded the verdict; one relaunched since \
         delivers it unflagged), and `aterm fabric` lists such asks under WARNINGS. When the \
         link cannot report a landing the wait ends at \
         once with `ERR fabric <absent|stalled|disconnected> id=<n> <queued=1|no-bridge=1>`, and \
         WHICH \
         of those two tokens it carries is the instruction. `queued=1` says the message is STILL \
         IN THE OUTBOX and a bridge will publish it (a bridge exit is the ordinary relaunch \
         path, and `outbox` is a peek that removes nothing), so it must not be read as `not \
         sent` and re-posted — a `post` without `key=` carries no idempotency key that would \
         collapse the duplicate, and one with it is still queued. `no-bridge=1` says \
         something NARROWER about the same queued \
         message, and the difference is the whole report an agent makes: this instance has no \
         `[fabric] command`, so no bridge exists to drain the outbox RIGHT NOW and none is \
         coming ON ITS OWN. It is not a verdict on the message. `fabric attach <command...>` \
         arms a supervisor and that same outbox drains — measured 2026-09-12, a post refused \
         `no-bridge=1` landed in the addressee's inbox the moment a bridge attached — so the \
         honest sentence is \"queued, and unpublishable until this instance has a bridge\", never \
         \"not sent\". Reporting it as lost is how a delivered task gets done twice. Further \
         posts only fill the queue until `ERR outbox full`. THE THIRD OUTCOME, which this row \
         used to omit: `ERR timeout id=<n>`, the `--wait` expiring with no landing reported. It \
         is queued exactly like the other two, and must not be re-posted either. THE FOURTH IS \
         THE ONE THAT IS NOT QUEUED: `ERR <reason> id=<n>` — `unroutable`, `ambiguous` or \
         `undeliverable` — is the bridge RETIRING the post, and the wait is released with that \
         verdict rather than a timeout. `outbox` then omits the row (it lists only rows that are \
         not dead), so no bridge drains it again: this one is reported as its reason, and \
         re-posting is right once the address is. The body is inline text up to 4 KiB, or `len=<n>` followed \
         by that many raw bytes up to 256 KiB. There is no `to=fleet`: a node holds no fleet \
         write grant, and an agent may only ASK a human to halt. REFUSED to an edge-token \
         connection — a write-input edge over one session would otherwise speak AS that session, \
         and the sender of a post must be attested by the instance Owner. Exempt from `hold`.",
    ),
    va(
        "deliver",
        Write,
        Status,
        Meta,
        BridgeOnly,
        "deliver <sid> off=<n> from=<p> kind=<k> ...: put one bus record in a session's inbox",
        "BRIDGE-ONLY: only the fabric bridge connection may call it, Owner included — it stamps \
         `from=` and `trust=`, and Owner scope is what every in-session client already holds, so \
         an attestation an injected agent could forge would attest nothing. Idempotent on `off=` \
         over the last 1024 delivered offsets: a \
         redelivered offset answers the id it first got and appends nothing, which is what turns \
         the bus's at-least-once cursor into exactly-once at the endpoint — and NOT one offset \
         further, because the dedup window is twice the 512-row ring. A redelivery older than \
         that reappears as a FRESH row (the row was dropped unread and the bus still holds it), \
         which a session that lists its mail without ever running `inbox seen` can reach: the \
         bridge refills from the persisted `seen=` offset. `ERR quota` past 64 unread rows from \
         one ATTESTED PEER — counted on the cap-forced `<src>` (the part after `@`, or the whole \
         `from=` when there is none) and NOT on the whole `from=` string, half of which the \
         sending node chooses — so one peer cannot evict a human's unread `task` under a burst of \
         `note`s by rotating pseudo-sids, and eviction never drops an `h-*` row ahead of an \
         agent's. `deliver <sid> landed=<post-id> off=<n>` is the \
         other form: it closes an outbound `post` and releases its `--wait`. \
         `verdict=<handled|refused|deferred>` on a `kind=ack` row is the recipient's receipt (R8); any other word \
         is `ERR usage`. `deliver <sid> fetched=<off> …` is the THIRD form: the bridge answering \
         an `inbox get @<off>` this session parked (listed on the `outbox` peek as `fetch sid= \
         off=`) with the record as read from the broker's log — the row fields without `off=`, \
         and `len=<true size>` when the record is over 256 KiB and the body is cut there — or \
         `err=<token>` (`no-record`, `unreadable`, `observer`, `forged-self`, `via`, `malformed`, \
         `oversize`, `refused`) for why there is none. A body that does not fit one line arrives \
         in CHUNKS, each a whole `fetched=` line with the same fields: `at=<n>` is the byte \
         offset where its `text=` starts in the decoded body and `more=1` marks all but the \
         last. It touches no ring: the parked read takes the answer, nothing is appended, and a \
         complete answer nothing waits on is kept (bounded) for the read that asks next; a \
         refused answer fails the read parked on it. `deliver <sid> receipt=<rid> off=<n|->` is \
         the FOURTH form: the bridge retiring a receipt the session owed (R8) — `off=` where the \
         `ack` landed, `-` for none sent — idempotently.",
    ),
    // `link`: the bridge's report of ITS OWN broker link — what turns `fabric=`
    // from "a bridge process is attached" into "mail can move". Bridge-only
    // because Owner scope is what every in-session client holds, and a client
    // that could say `link up` would make `post --wait` burn its timeout on a
    // link the bridge itself knows is down.
    va(
        "link",
        Write,
        Status,
        Meta,
        BridgeOnly,
        "link up rtt=<ms> | link down reason=<token> [rtt=<ms>]: the bridge reports its broker link",
        "BRIDGE-ONLY. The record behind `status`'s `fabric=`, `fabric_rtt_ms=` and \
         `fabric_link_age_ms=`: `up` with the round trip of the ack just received moves the \
         instance to `fabric=connected` and stamps the ack's time; `down` with a one-token \
         reason (`no-socket`, `refused`, `denied`, `no-ack`, `closed`, `attach`, `subscribe`, \
         `read`) moves it to `fabric=stalled` and wakes every parked `post --wait` so it answers \
         `ERR fabric stalled id=<n> queued=1` instead of sitting out its timeout. The bridge \
         sends it on every change, after an ack that moved the round trip by more than 2x, and \
         otherwise after an ack at most once per 2 s (so the age stays honest on a busy link) \
         — never on a timer, never as a heartbeat. `OK`, or `OK stale=1` when the report came \
         from a bridge incarnation that no longer owns the link (a ghost lane; the report is \
         dropped, the live bridge's state stands). `ERR usage` for anything else.",
    ),
    va(
        "hold",
        Write,
        Status,
        Meta,
        OwnerOnly,
        "hold <sid> on|off [reason=<pct>] [origin=fleet|local]: the drive halt, local or fleet",
        "`OK hold=<0|1>`. TWO ORIGINS, AND THE CONNECTION CHOOSES. From the inherited bridge \
         connection this is the FLEET halt: `origin=fleet` by default, either origin accepted, \
         and `off` lifts whatever stands. From the Owner token — the local human's own \
         credential — it is the LOCAL halt: `origin=local` is the default and the only origin \
         accepted, and an Owner-issued act against a standing `origin=fleet` hold, `on` or `off`, \
         is `ERR denied`. Owner scope is what every in-session client already holds, so a fleet \
         halt an injected agent could lift or replace locally would be no halt at all; a local \
         halt is the owner's own stop and the owner's own to lift — and NOT a containment stop: \
         Owner is the scope every in-session client holds (`aterm-ctl @self` is Owner), so the \
         halted session's own agent can lift a local hold with `hold <sid> off`; the halt an agent \
         cannot lift is the fleet's. An edge token is `ERR denied` \
         either way, and a selector is rejected: the session is the argument. While on, every \
         PTY-reaching verb resolving to that session answers `ERR halted reason=<r> \
         origin=<local|fleet>` from ANY scope — `send key ctrl feed feed-bin paste paste-bin mouse \
         pointer resize focus signal turn close invoke hwkey pane tab confirm \
         operator-propose-bin` — a \
         TRANSIENT class beside `ERR busy`, so existing back-off code already does the right \
         thing. That enumeration is NOT maintained by hand: it is pinned verb-for-verb against \
         the set this build enforces (`fabric::is_pty_reaching`), so a verb a halt refuses cannot \
         be missing from it and a verb named here cannot be answerable. `focus` is in that set \
         because it writes the DEC 1004 focus reports to the PTY; `pointer` is, because the \
         terminal REPORTS pointer motion to the program under DEC 1000/1002/1003; `hwkey` is, \
         because it posts a real NSEvent and so takes the same path a physical keypress does; \
         `invoke` is, because `invoke Paste` writes the clipboard into the front tab's PTY; \
         `pane` is, because it moves which pane — and so which session — the keyboard drives; \
         `tab` is, because `tab close [N]` RETIRES a session exactly as `close` does, and a \
         driver refused `close` used to type that instead; `confirm` is, because `confirm yes` \
         runs the close an `invoke` left waiting in the window. `invoke`, `hwkey`, `pane`, \
         `pointer`, `tab` and `confirm` resolve no session, so they are refused while ANY \
         session on the instance is \
         held — including the aimed `@<sid> tab …` form, which drives the window HOSTING that \
         session and therefore answers `ERR halted` whenever any session of that instance is \
         held, not only the one it names. `post`, `inbox seen`, \
         `meta set`, `lease` and every read verb stay answerable, and the physical keyboard is untouched: a \
         halt stops drivers, not humans. When the bridge connection closes, the instance holds \
         every session that bridge ever delivered to or held with `reason=fabric-lost \
         origin=fleet` — the halt must not depend on a killable process staying alive — and, \
         being fleet-origin, only a reconnecting bridge's `hold off` lifts it. An Owner-issued \
         hold does not make a session bridge-governed: a bridge dying afterwards halts what IT \
         touched, not what the owner did.",
    ),
    va(
        "outbox",
        Read,
        Bytes,
        Meta,
        BridgeOnly,
        "outbox [<max>]: drain the queued outbound posts, bodies included",
        "BRIDGE-ONLY, and the mirror image of `deliver`: `deliver` is how a record enters the \
         instance, `outbox` is how one leaves it. `OK <nbytes>` then that many raw bytes, holding \
         one `post sid=<s> id=<n> to=<pct> kind=<k> [re=<n>] [dl=<ms>] [via=<p,...>] \
         [key=<token>] len=<n>` line per queued post followed by that post's `len` body bytes — \
         `key=` is the caller's idempotency key, carried on EVERY drain so a relaunched bridge \
         maps it to the same reserved producer sequence — a length prefix and not \
         a row, because a body may contain newlines and a line-framed listing could not carry it. \
         AFTER every post, one bodiless `receipt sid=<s> rid=<n> off=<n> verdict=<v> kind=<k> \
         from=<p>` line per receipt a session OWES (an `inbox seen <id> <verdict>` on an \
         ask/task, until the bridge publishes the `ack` and retires it with `deliver <sid> \
         receipt=<rid>`), and one bodiless `fetch sid=<s> off=<n>` line per `inbox get @<off>` a \
         session has parked for the bridge (after, so a parser from before these lines still \
         drains every post); the bridge answers each with `deliver <sid> fetched=<off> …`. \
         A PEEK: it moves no watermark and drops nothing, so a bridge that dies mid-publish \
         re-reads the same posts on restart and republishes them under the same producer \
         sequence. `outbox sent` is what retires one. One drain is BOUNDED IN BYTES as well as \
         by `<max>` (4 MiB across all sessions, always at least one post so a large body is \
         never stranded): the per-session queue bounds are per session, and one reply used to \
         concatenate every session's bodies. A drain stopped by the budget is resumed by the \
         next call, which is safe precisely because nothing was retired. Owner-forbidden for the same reason \
         `deliver` is: an Owner-token connection reading this would read every session's \
         outbound traffic.",
    ),
    va(
        "outbox sent",
        Write,
        Status,
        Meta,
        BridgeOnly,
        "outbox sent <sid> <id> off=<n|-> [dup=1]: retire one queued outbound post",
        "BRIDGE-ONLY. `off=<n>` is the broker offset the post landed at: it fills the `post` \
         row's `off=`, releases a `post --wait` parked on it, and lets the endpoint drop the \
         retained body — the body is kept only until this arrives, which is what bounds the \
         queue's memory. `dup=1` beside it is the broker's verdict that the publish was DEDUPED \
         — the post re-used a producer sequence an earlier post under the same `key=` had \
         reserved, so `off=` is that earlier record's and nothing was appended — and the parked \
         wait answers `OK <id> off=<n> dup=1`; it is sticky across a retried retirement and \
         refused on the `off=-` form (a duplicate landed, by definition). `off=-` retires it \
         as permanently undeliverable instead, with an \
         `undeliverable` row explaining why arriving separately through `deliver`. An optional \
         `reason=<word>` on the `off=-` form names WHICH refusal it was, and a `post --wait` \
         parked on that post wakes with `ERR <reason> id=<n>` instead of a uniform `ERR \
         undeliverable`: routing is the bridge's knowledge, not the endpoint's, so `ERR \
         ambiguous` for a sid two nodes claim can only reach the sender this way. Idempotent: \
         retiring a post twice is `OK`, not a second event, so a bridge that retries after a lost \
         reply cannot double-push. A connection that could forge this would release a `post \
         --wait` for a message that never left the machine.",
    ),
    // sessions, presence & capability — the OwnerOnly access declares the owner
    // gate IN the table (the dispatch reads it, no hardcoded verb list). `who`
    // keeps its `Read` op-class (a fleet-wide presence readout) yet is Owner-gated:
    // op-class and scope-gate are orthogonal, which is exactly why `access` is a
    // separate field.
    va(
        "operator",
        Owner,
        Status,
        Meta,
        OwnerOnly,
        "embedded opt-in operator: \
         status|inspect|manage|unmanage|next|extend|ack|reconcile|clear-fault",
        "(Owner-only)",
    ),
    va(
        "operator-propose-bin",
        Owner,
        Status,
        Meta,
        OwnerOnly,
        "length-prefixed JSON proposal on stdin for the embedded operator actuator (Owner-only)",
        "",
    ),
    // `fabric`: the bridge SUPERVISOR of a running instance. Session-independent
    // like `operator` (answered before session resolution, so a windowless
    // instance can be attached), Owner-only because arming the supervisor is
    // choosing which process holds `Scope::Bridge`.
    va(
        "fabric",
        Owner,
        Status,
        Meta,
        OwnerOnly,
        "fabric status|attach [<command...>]|retire <sid>...: the bridge of a RUNNING instance",
        "`fabric status` -> `OK state=<absent|connected|stalled|disconnected> supervised=<0|1> \
         command=<pct|-> reason=<token|-> rtt_ms=<n|-> link_age_ms=<n|->`: `state=` is the \
         link (`status`'s own `fabric=` token — the bridge's BROKER link, `stalled` while a \
         bridge is attached but that link is down), `reason=` why it is down (`starting` before \
         the bridge's first report, `no-socket`, `refused`, `denied`, `no-ack`, `closed`, \
         `attach`, `subscribe`, `read`, `bridge-lost`; `-` when up), `rtt_ms=`/`link_age_ms=` \
         the last acknowledged round trip and its age exactly as `status` carries them, \
         `supervised=` \
         whether a supervisor is armed in this process (the latch that flips `post`'s refusal \
         from `no-bridge=1` to `queued=1`), `command=` the argv it runs — or, unarmed, the one \
         the instance was launched with. `fabric attach [<command...>]` arms the supervisor NOW, \
         without a relaunch: with no argument it uses the `[fabric] command` the instance was \
         LAUNCHED with (`ERR fabric no command` when \
         there was none — a command added to the config after launch is not re-read; pass it), \
         otherwise the given words are the argv, split on whitespace exactly like the config \
         string and never through a shell. `OK attached command=<pct>` means the supervisor \
         thread started; the bridge connects asynchronously, so watch `fabric status` / `status \
         fabric=` for `connected`. ONCE PER PROCESS: the supervisor relaunches its child forever \
         and has no stop handle, so a second `attach` is `ERR fabric already supervised \
         command=<pct>` naming the argv that is running — the request's argv was NOT applied. \
         Because that latch is one-way, `argv[0]` is PRE-FLIGHTED before it closes: a program \
         that does not exist, is not a regular file, or has no execute bit (a bare name: found \
         nowhere on `PATH`, the same `PATH` the child inherits) is `ERR fabric not executable \
         program=<pct> reason=<not-found|not-a-file|no-exec-bit|not-on-path>`, arms nothing, \
         and leaves the slot open for the corrected command; a program that passes and still \
         fails to start is the supervisor's to retry (back-off to 30 s), logged. Owner ONLY — \
         an edge token and the bridge connection itself are `ERR denied` — for the reason \
         `deliver` is bridge-only: whoever arms the supervisor chooses which process holds \
         `Scope::Bridge`, and the bridge is that process, not a party to arming it. `fabric retire <sid>...` asks THIS instance's bridge to retire the presence rows of sessions nobody hosts — the operator's `aterm fabric doctor --retire-ghosts` with the bridge up: a sid a session here carries is `ERR hosted <sid>` and nothing is requested; otherwise `OK requested=<n>`, the request rides the `sessions` push as `EVENT * fabric-retire <sid>`, and the bridge re-checks hosting and publishes `exited` through its OWN producer sequence, so no second process ever writes the sequence file.",
    ),
    // `sessions` is the fleet roster. Its trailing tokens are ADDITIVE: `meta=`
    // (stage 1), then `window=`/`active=`/`wfocus=` from ONE main-thread hop per
    // call (F2: windows on the session verbs) and `detail=` from each engine's
    // executing block (F5: who lives in a session). Old clients key on the sid
    // field and ignore the tail.
    va(
        "sessions",
        Owner,
        Lines,
        Meta,
        OwnerOnly,
        "OK <n> local sid parent state title meta= nonce= window= active= wfocus= detail= identity= path=",
        "Owner-only. `nonce=<hex32>` is the session's PUBLIC launch nonce — the freshness fence \
         an edge binds to and the fabric's `epoch=` verbatim; it is here because `whoami` reports \
         only the connection's own session, so a bridge could not read any other's. \
         `window=<id|none|->`: the hosting window (the `dims` rule — the front \
         window when it shows the session, else the lowest window id); `none` = a session no \
         window holds; `-` = the main thread could not be asked (the line is still printed). \
         A `--headless` instance owns one logical window, id 0 — the one `dims` reports as \
         `window=0 geometry=headless` — so its sessions read `window=0`, never `none`. \
         `active=<1|0|->`: on that window's active tab. `wfocus=<1|0|->`: that window is \
         aterm's MOST RECENTLY FOCUSED window — set when a window gains focus and never \
         cleared on blur, minimize or app deactivate, so exactly one window reads `1` for as \
         long as aterm runs (it is NOT \"the OS key window right now\"). `detail=<pct|->`: the \
         sanitized RUNNING command — the program reduced to its basename (a quoted path with \
         a space is one word; a Windows `.exe`/`.cmd`/`.bat`/`.com` is dropped), plus an \
         allow-listed subcommand, never an argument (`claude`, `codex`, `targo%20test`); a \
         COMPOUND line reads the segment that RUNS — the last of an `&&`/`;` chain, the \
         first of an `||` chain, one wrapper (`sudo`, `env`, `exec`, ...) unwrapped — so \
         `cd ~/ay && exec claude` reads `claude`; a line that OPENS with a shell keyword is \
         not parsed and reads that keyword (`for i in 1 2 3; do ...; done` reads `for`), and \
         a keyword opening a LATER segment reads the same way (`cd x && for ...; done` is \
         `for`, never its closer `done`). The \
         same value `status` carries; `-` when idle or the engine lock was contended. \
         `identity=<name|->`: the agent identity the session was spawned under (`spawn \
         identity=<name>` - the directory its agents keep their login in; `identities` lists \
         them), `-` for the human's own agent config - and for a shell adopted from a build \
         without the field, which keeps its env and drops the label. `path=<frozen|live>`: \
         whether the session's shell fronts aterm's managed `agents/` on PATH, so \
         `claude`/`codex` typed at its prompt run the managed build - `frozen` = they \
         resolve to a FOREIGN copy (a native install, a brew cask). MEASURED where it can \
         be (2026-09-24): the environment of a program the shell itself started names the \
         PATH the shell exports. A live reading wins and lowers the adoption mark; a frozen \
         one is the shell's only once a SECOND job agrees (one job's `PATH=... cmd` \
         override reads frozen too) and never raises the mark; a shell whose children have \
         not been read keeps the mark it was adopted with (a shell spawned by a build before \
         the self-healing sessions, 2026-09-16, is carried as `frozen`). \
         `path_evidence=` (below) says which. A tab whose foreground is an agent is \
         never the place to type a shell line into. After it (2026-09-23): \
         `program=<name|-> agent=<word|-> \
         agent_detail=<pct|-> agent_rev=<n> agent_since_ms=<ms> agent_gen=<e.s|-> \
         agent_fp=<hex16|->`, the foreground program and \
         the server's agent verdict exactly as `status` prints them, read from the session \
         (no lock, no hop), so one read names every agent tab and what it waits on. Then \
         `human_ms=<ms|->` (since a person last typed, clicked or scrolled in it through a \
         window, as \
         `status` prints it); then (2026-09-25) \
         `role=<pct|-> attention=<pct|-> user_title=<pct|->`, the typed metadata `meta` \
         prints, so a fleet reader classifies every session from this one read; and \
         `supervisor=<pct|->`, the live `meta set supervisor` claim. Then the OWNER'S \
         COLUMNS (2026-09-24), last: `path_evidence=<measured:<ms>|unconfirmed:<ms>|carried|->` \
         - how `path=` was learned (read that many ms ago from a child of the shell; \
         `unconfirmed` = one job read frozen, which is not yet taken as the shell's; the \
         adoption mark, never measured; `-` = a shell a self-healing build spawned); \
         `copy=<managed|foreign|->` - for `claude`/`codex` in front, whether the build it \
         runs is aterm's managed one (from the store) or a foreign copy (the native \
         installer's `~/.local/share/claude/versions/<v>`, a brew cask); \
         `upgrade=<-|<state>/<to>/<why>/<age>>` - the live agent upgrade of the \
         conversation in the tab (`aterm harness upgrade --status` has the rest): state \
         `pending`, `announced`, `restarting`, `deferred`, `skipped`, `stalled` or `done`, \
         the target version, the wait or stall (`settling`, `attended`, `awaiting-ready`, \
         `next-round:<span>` for a round resting before it asks again, `ready` for one \
         that gave up and acts on a late READY, `overdue` ...), \
         and how long the session has been behind (for `done`, \
         how long ago it finished). One main-thread hop per call, not per session; the \
         client `ls` \
         relays these lines verbatim and `windows` folds them per window. The menu-bar \
         status item's fleet scan reads this on every open under a 2 s per-peer budget, so \
         a peer whose main thread cannot answer inside it drops out of that menu rather \
         than being listed from the registry alone — one hop per open, never a poll. \
         `sessions bridge` is the Fabric bridge's identity-only roster: `OK <n>` then \
         `<local> <sid> nonce=<hex32>` per session, sorted by local id. It takes \
         one registry snapshot and reads no terminal, metadata, timeline or \
         window placement; the public `sessions` response remains unchanged. \
         `sessions status` is the bridge's one-hop status snapshot: `OK <n>` then one \
         `<local> <sid> <nonce> sid=<local> revision=<n> hold=<0|1> detail=<pct|-> agent=<word|->` \
         row per readable session. The sid and nonce fence local-id reuse; an \
         unreadable session has no row, so the bridge retries its ordinary `@sid status`",
    ),
    va(
        "who",
        Read,
        Lines,
        Meta,
        OwnerOnly,
        "OK <n> local sid driving= watchers= turns= state nonce= fgpgid=",
        "Owner-only. Each row has `nonce=<hex32>`, the session's public launch nonce, \
         and ends in `fgpgid=<pid|->`, the PTY's current foreground process group \
         (`-` when the kernel cannot read it). Both are read without the `sessions` \
         window-placement hop; a harness can match `fgpgid` to a process's own \
         group and controlling-terminal foreground group without reading its env. \
         driving= is `<turn-id>` under a \
         live turn, `lease:<holder>` under a cooperative drive lease (the `lease` verb's \
         surfacing), and `-` when nobody drives.",
    ),
    // `exits` reads the instance's roster journal — every sid the instance ever
    // hosted — so it is Owner-gated like `sessions`, whose past it is.
    va(
        "exits",
        Owner,
        Lines,
        Meta,
        OwnerOnly,
        "exits [<n>] [since=<id>]: the instance EXIT LEDGER - why each session went. Owner-only",
        "- one `exit <id> t=<ms> sid=<sid> local=<n> \
         reason=<shell-exit|ctl-close|ui-close|window-close|app-quit|unknown> exit_code=<n|-> \
         by=<sid|human|ctl|bridge|->` line per session that left the registry (`app-quit` is reserved, not \
         produced today: quit ends the process, ledger included, and deregisters nothing), \
         oldest-first, monotonic ids, \
         drop-oldest ring (`OK 0` = none retained); `<n>` keeps the newest n, `since=<id>` keeps \
         ids strictly greater (page with the last id you saw); the ring is MEMORY-ONLY and \
         per instance - nothing is written to disk, so a `close` that retires the LAST session \
         of the LAST window ends the process and takes the whole ledger with it (read it \
         before that close, not after); `t=` is the `timeline`/`history` \
         clock; `by=` is the closing CALLER: an edge-scoped client's own sid (the session its token \
         was granted to), `human` for a UI/window close, `ctl` for an owner-token control client \
         (the per-instance token names no session; never the front tab, never the closed \
         session), `bridge` for the fabric bridge, `-` when the close path did not say; \
         `exit_code=-` = \
         the child was hung up by a close, died by signal, was not aterm's to reap, or had not \
         yet exited at either of the ledger's two non-blocking looks. The same facts reach a live \
         watcher as they happen: a `subscribe @<sid> events` watch on the closing session gets \
         `EVENT <local> closing reason= by=` ahead of its `EVENT <local> exited`, and a \
         `subscribe … sessions` watch gets `EVENT * session-exited <sid> reason=`; the `timeline` \
         verb cannot be asked for the `closing` row after the close (the sid stops resolving in \
         the store write that records it; only a request that resolved the session just before \
         that write can still read it)",
    ),
    // `identities` is the on-disk roster of agent identities (`spawn identity=`):
    // Owner-gated like `sessions`, whose `identity=` column it explains, and
    // Lines-framed from the table so the client is untouched.
    va(
        "identities",
        Owner,
        Lines,
        Meta,
        OwnerOnly,
        "identities [<name>|forget <name> [confirm=<name>]]: the agent identities on disk. Owner-only",
        "- an identity is `<state>/identities/<name>/`, the directory a session spawned with \
         `spawn identity=<name>` points its agents' home variables into (`CLAUDE_CONFIG_DIR`, \
         `CODEX_HOME`), so its login, settings and skills are its own and never the human's. \
         `identities` -> `OK <n>` then one `<name> dir=<pct> sessions=<n> \
         agents=<prog>:present|absent,...` line per identity, sorted by name: `sessions=` \
         counts the LIVE sessions carrying it, and `present` means the agent's subdirectory \
         is NON-EMPTY - a `read_dir` name listing, bytes never opened; a primed identity \
         reads `present` for every agent aterm knows, and aterm never reads, names or claims \
         a login. `identities <name>` -> `OK <n>` (the row plus one line per agent, so the \
         client's line framing carries them all): that row, then one `agent=<prog> var=<VAR> \
         home=<pct> files=present|absent` line per agent. The `forget` form answers ONE status \
         line (no row count): `identities forget <name>` -> `ERR \
         confirm: identities forget <name> confirm=<name> removes <pct>` (nothing removed); \
         with `confirm=<name>` -> `ERR identity in use sessions=<sid>,...` while any live \
         session carries it, else `OK removed=<pct> left=keychain` - the tree is gone, and \
         with it the credentials the agents keep as files, but a login an agent keeps in the \
         macOS keychain is NOT: sign out in the agent first (`/logout`); aterm never reads, \
         names or deletes a keychain item it did not create. Names fold to lowercase. \
         Unknown: `ERR no such identity <name>`; anything else: `ERR usage: identities \
         [<name>|forget <name> [confirm=<name>]]`. A read except `forget`, which removes a \
         directory and never a session; a shell adopted from a build without the label reads \
         as no user of its identity (`sessions=0`), so close it before forgetting",
    ),
    va(
        "whoami",
        Owner,
        Status,
        Meta,
        OwnerOnly,
        "OK <session> <nonce> <scope>",
        "",
    ),
    // `family` is `Read`/`Scoped`: the no-arg form walks the RESOLVED (already
    // gated) session. Its EXPLICIT-sid sub-form (`family <sid>`) additionally
    // demands Owner in the handler — a PER-ARGUMENT check the static table cannot
    // express (the authority depends on whether an arbitrary node is named), so it
    // stays a documented runtime check beyond the table, not an `access` value.
    v(
        "family",
        Read,
        Lines,
        Session,
        "the session's parent + direct children",
        "",
    ),
    v(
        "edges",
        Read,
        Lines,
        Session,
        "inbound capability edges (--json). alias: grants",
        "",
    ),
    v(
        "grants",
        Read,
        Lines,
        Session,
        "inbound capability edges (--json). alias: edges",
        "",
    ),
    va(
        "grant",
        Owner,
        Status,
        Meta,
        OwnerOnly,
        "mint a cross-session edge (Owner only)",
        "",
    ),
    va(
        "revoke",
        Owner,
        Status,
        Meta,
        OwnerOnly,
        "revoke a cross-session edge: revoke <edge-hex> removes one;",
        "revoke src=<sid> sweeps every edge from that source and replies OK <removed> (Owner only)",
    ),
    // Session connections (design §6): the connection-grain verbs over the
    // `grant`/`revoke` op-level primitives. All Owner-only, all self-scoped —
    // the endpoints ride as `dst=`/`src=` ARGUMENTS, never a selector.
    va(
        "connect",
        Owner,
        Status,
        Meta,
        OwnerOnly,
        "connect dst=<sid> src=<sid> [kind=pull|push|both]: set the src->dst session connection",
        "(declarative: mint the missing ops, revoke the excess, so the rows equal exactly kind; \
         default both), reply `OK read-screen=<hex> \
         write-input=<hex> signal=<hex>` with only the minted ops present (Owner only)",
    ),
    va(
        "disconnect",
        Owner,
        Status,
        Meta,
        OwnerOnly,
        "disconnect dst=<sid> src=<sid> [kind=pull|push|both]: dissolve the session connection \
         src->dst",
        "(kind-filtered ok: kind=pull revokes only the pull half), reply OK <removed> (Owner only)",
    ),
    va(
        "flows",
        Owner,
        Lines,
        Meta,
        OwnerOnly,
        "the instance's session-connection graph: OK <n> + `<src> <dst> <op>` per live edge",
        "across EVERY session's table (--json \
         groups per pair: {\"flows\":[{src,dst,ops:[..]}]}). Owner-only",
    ),
    va(
        "raise",
        Owner,
        Status,
        Meta,
        OwnerOnly,
        "raise <sid>: raise the window hosting that session and select its tab",
        "(the session-connection Show twin; Owner only)",
    ),
    va(
        "dial",
        Owner,
        Status,
        Meta,
        OwnerOnly,
        "dial <name>: relay this connection over TLS to the saved network-drive peer <name>",
        "- subsequent verbs run on the remote (owner-only; a pre-relay failure answers one ERR \
         line, success sends no local reply)",
    ),
    va(
        "dial-list",
        Owner,
        Status,
        Meta,
        OwnerOnly,
        "list saved network-drive connections",
        "",
    ),
    va(
        "dial-token",
        Owner,
        Status,
        Meta,
        OwnerOnly,
        "token for a saved network-drive connection",
        "",
    ),
];

/// The spec for `verb`, or `None` for an unknown verb.
#[must_use]
pub fn spec(verb: &str) -> Option<&'static VerbSpec> {
    VERBS.iter().find(|s| s.name == verb)
}

/// Whether `verb` is OWNER-ONLY per the table — only the instance god-token may run
/// it, regardless of op-class. The dispatch reads THIS (not a hardcoded verb list),
/// so classifying a verb `OwnerOnly` in the table is what gates it. Unknown verbs
/// are not owner-only (they fall through to `ERR unknown verb`).
#[must_use]
pub fn is_owner_only(verb: &str) -> bool {
    spec(verb).is_some_and(|s| matches!(s.access, Access::OwnerOnly))
}

/// Whether `verb` is non-sensitive build/meta provenance or instance posture,
/// answered for ANY authenticated scope BEFORE target resolution
/// (`version`/`update`/`help`/`verbs`/`privacy`).
#[must_use]
pub fn is_any_scope_meta(verb: &str) -> bool {
    spec(verb).is_some_and(|s| matches!(s.access, Access::AnyScopeMeta))
}

/// Whether `verb` is BRIDGE-ONLY per the table — only the fabric bridge connection
/// may run it, and no token unlocks it, Owner included. The dispatch reads THIS (not
/// a hardcoded verb list), so classifying a verb `BridgeOnly` in the table is what
/// gates it. Unknown verbs are not bridge-only (`ERR unknown verb`).
#[must_use]
pub fn is_bridge_only(verb: &str) -> bool {
    spec(verb).is_some_and(|s| matches!(s.access, Access::BridgeOnly))
}

/// Trailer emitted after a complete guarded response. Its unpredictable nonce
/// is generated by the server only after receiving the request, so a client
/// cannot pipeline a valid acknowledgement before consuming the response.
pub const ARTIFACT_REPLY_CHALLENGE_PREFIX: &str = "ACK-CHALLENGE ";
/// Client echo sent only after consuming the complete response and challenge.
pub const ARTIFACT_REPLY_ACK_PREFIX: &str = "ACK ";

/// Maximum UTF-8 bytes in one control-protocol reply line. The shipping client
/// enforces this on every line; producers of intentionally large single-line
/// payloads use the same ceiling so they never emit a reply their own client
/// must reject.
pub const MAX_CONTROL_REPLY_LINE_BYTES: usize = 8 << 20;

/// THE SCREEN'S GENERATION: `(invalidation epoch, content seq)`, spelled
/// `<epoch>.<seq>` on the wire — `status gen=`, `text --json`'s `"gen"`,
/// `agent_gen=`, and the `if-gen=` fence a typed act (`key`, `send`) names.
///
/// ONE READER for both ends: the server parses `if-gen=` with
/// [`ScreenGen::parse`] and a client deciding whether a read's `gen` can be
/// sent back as a fence asks the same function, so a client never sends a
/// fence the server answers with `ERR usage` (the supervisor's own digits-only
/// check, which this replaced, passed an epoch past `u64::MAX`).
///
/// WHY NOT `seq=` ALONE. The content seq is the ACTIVE grid's counter, and
/// every alternate-screen entry installs a fresh grid whose counter starts
/// again at 1 — so a TUI that leaves and re-enters the alternate screen and
/// draws a second dialog with the same number of writes shows the first
/// dialog's `seq=` over a different screen (measured on a private headless
/// aterm: box A `ls -la /tmp/work` and box B `rm -rf /tmp/work` both read
/// `seq=15`, and a fence on that value pressed into B). The epoch is the
/// terminal's invalidation epoch, monotonic for the terminal's life and
/// advanced by every screen switch and by RIS, so within one epoch the grid is
/// one grid and its seq only grows: the pair names a screen generation that
/// never repeats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScreenGen {
    /// The terminal-wide invalidation epoch.
    pub epoch: u64,
    /// The active grid's content seq within that epoch.
    pub seq: u64,
}

impl ScreenGen {
    /// Parse the wire spelling `<epoch>.<seq>` (two unsigned decimals, each
    /// fitting a `u64`); any other shape — one number, a sign, a third part,
    /// an overflow — is `None`.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        let (epoch, seq) = value.split_once('.')?;
        let decimal = |s: &str| {
            (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                .then(|| s.parse::<u64>().ok())
                .flatten()
        };
        Some(Self {
            epoch: decimal(epoch)?,
            seq: decimal(seq)?,
        })
    }
}

impl core::fmt::Display for ScreenGen {
    // Plain `write_str`s, not `write!`: see `crate::trust_fmt` for why this
    // crate builds no runtime-argument `format_args!`.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.epoch.to_string())?;
        f.write_str(".")?;
        f.write_str(&self.seq.to_string())
    }
}

/// Validate the fixed-width lowercase/uppercase hexadecimal nonce grammar used
/// by guarded artifact acknowledgement frames.
#[must_use]
pub fn valid_artifact_ack_nonce(nonce: &str) -> bool {
    nonce.len() == 32 && nonce.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Whether a successful reply for this request may carry exact capture/video
/// retention. Kept beside [`framing_of`] so the shipping client and server-side
/// protocol documentation share one classification. An `ERR` never needs the
/// acknowledgement.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn artifact_reply_requires_ack(verb: &str, request: &str) -> bool {
    let req_no_sel = request
        .strip_prefix('@')
        .and_then(|rest| rest.split_once(' ').map(|(_, tail)| tail))
        .unwrap_or(request);
    let sub = req_no_sel.split_whitespace().nth(1);
    let image_bytes = req_no_sel
        .split_whitespace()
        .skip(1)
        .any(|token| token == "--bytes" || token == "bytes");
    match verb {
        // File captures retain exact-name guards. In-memory byte captures keep
        // their admission only through the bounded write+flush and therefore do
        // not need a post-response ACK. `image read` is an ordinary terminal-
        // inline projection with no capture job or retained payload slot.
        "image" => sub != Some("read") && !image_bytes,
        "window" => true,
        // Status/stop are ordinary in-memory control replies. A recording result
        // and `video frames` both advertise retained server-local paths.
        "video" => !matches!(sub, Some("status" | "stop")),
        _ => false,
    }
}

/// The reply framing of `verb` for the full `request` line. SELECTOR-AWARE: a
/// leading `@<selector>` is skipped, and the `image read` / `cast frames`
/// SUB-FORMS flip their base verb's framing to `Lines` (bare `image` rasterizes to
/// a status line; bare `cast` is a byte body). Unknown/plain verbs are `Status`.
#[must_use]
// Skip: iterator absent std bodies.
#[cfg_attr(trust_verify, trust::skip)]
pub fn framing_of(verb: &str, request: &str) -> Framing {
    let req_no_sel = request
        .strip_prefix('@')
        .and_then(|r| r.split_once(' ').map(|x| x.1))
        .unwrap_or(request);
    let sub = req_no_sel.split_whitespace().nth(1);
    if verb == "image" && sub == Some("read") {
        return Lines;
    }
    // `image --bytes` / `image bytes` returns the PNG base64'd on one line
    // (`OK 1\n<w> <h> <nbytes> <base64>`), so it is Lines-framed, not the Status
    // `OK <w> <h> <path>` of a file capture. The flag may sit anywhere in the tail.
    if verb == "image"
        && req_no_sel
            .split_whitespace()
            .any(|t| t == "--bytes" || t == "bytes")
    {
        return Lines;
    }
    if verb == "cast" && sub == Some("frames") {
        return Lines;
    }
    // `inbox get <id>` returns ONE message body as a length-prefixed byte frame
    // (`OK <nbytes>` + n raw bytes) and `inbox seen <id>` one `OK seen=<id>` status
    // line — neither is the `OK <n>` + rows the bare `inbox` listing answers. The
    // same base-verb/sub-form flip as `image read`, and for the same reason: a
    // client that read `OK seen=42` as a row count hangs waiting for 42 rows that
    // never come. Both sub-forms have their own table row (`inbox get`/`inbox
    // seen`), but `spec()` keys on the verb KEYWORD, so the flip has to be here.
    if verb == "inbox" {
        match sub {
            Some("get") => return Bytes,
            Some("seen") => return Status,
            _ => {}
        }
    }
    // `outbox sent <sid> <id> off=<n>` answers one `OK` status line, not the
    // `OK <nbytes>` + body frame the bare `outbox` drain answers. Same flip, same
    // hazard: a client reading `OK` as a byte count parks on a body that never comes.
    if verb == "outbox" && sub == Some("sent") {
        return Status;
    }
    // `identities forget <name> [confirm=<name>]` answers ONE status line —
    // `OK removed=<pct> left=keychain`, or the confirm/in-use `ERR` — not the
    // `OK <n>` + rows the bare listing and `identities <name>` answer. Same flip,
    // same hazard, MEASURED on the live try of 2026-09-17: under Lines framing
    // aterm-ctl refused the removal's reply as a `malformed response header`.
    if verb == "identities" && sub == Some("forget") {
        return Status;
    }
    // `topic add <t> …` / `topic drop <t>` answer ONE status line
    // (`OK <t> since=<..> added=<0|1>`, `OK <t> dropped=<0|1>`), not the
    // `OK <n>` + `topic <t> since=<..>` rows the bare form and `topic ls`
    // answer. The same flip as `inbox seen`, and the same hazard: under Lines
    // framing the client reads the TOPIC as a row count and reports a malformed
    // header — which is what `identities forget` measured on 2026-09-17.
    if verb == "topic" && matches!(sub, Some("add" | "drop")) {
        return Status;
    }
    // `video frames [count=N]` lists the newest recording's top-delta frames as
    // `OK <n>\n` + n rows — Lines-framed, unlike the base `video <secs>` capture
    // (a single Status `OK …` dump line) and `video status`/`video stop`.
    if verb == "video" && sub == Some("frames") {
        return Lines;
    }
    // `temporal status` replies a single Status `OK enabled=… …` line, NOT the
    // Bytes-framed screen reconstruction the base `temporal [tick]` returns — so it
    // must NOT inherit temporal's Bytes framing (the client would read the `OK …`
    // line as a byte count and report a malformed header).
    if verb == "temporal" && sub == Some("status") {
        return Status;
    }
    // `--json`/`json` read mode: the server wraps the body with `json_ok` = a
    // uniform `OK 1\n<body>` (Lines framing) for EVERY json-capable read verb. Verbs
    // whose plain reply is Status-framed (`cursor`, `dims`, `metrics`) must therefore
    // switch to Lines under the flag or the client reads only the `OK 1` header and
    // silently drops the JSON body. Harmless for the already-Lines members
    // (text/screen/…).
    if JSON_CAPABLE_VERBS.contains(&verb)
        && req_no_sel
            .split_whitespace()
            .any(|t| t == "--json" || t == "json")
    {
        return Lines;
    }
    spec(verb).map_or(Status, |s| s.framing)
}

/// The verbs whose reply the server wraps in `json_ok` under `--json`/`json`, and
/// which therefore switch to [`Framing::Lines`] under the flag.
///
/// This list MIRRORS the server's `json_ok` call sites (`aterm-gui`'s
/// `cmd_*_json` helpers). It is the one framing input that is not derivable from
/// [`VERBS`], because json-capability is a property of the server's handler, not
/// of the verb row — so it is a hand-maintained duplicate, and duplicates drift.
/// `metrics` was missing here while `cmd_metrics_json` happily wrote Lines, so
/// `metrics --json` framed as Status and the client silently DROPPED the JSON
/// body. Named and exported so `aterm-gui`'s
/// `json_ok_sites_match_the_json_capable_verbs` test can bind the two ends
/// together; add a verb here in the same change that adds its `_json` handler.
pub const JSON_CAPABLE_VERBS: &[&str] = &[
    "text", "screen", "cursor", "dims", "blocks", "edges", "grants", "metrics", "privacy",
];

/// One catalog row: the verb name in the [`CATALOG_TEXT_COLUMN`] gutter, then `text`.
fn catalog_row(name: &str, text: &str) -> String {
    format!("{name:<w$} {text}", w = CATALOG_TEXT_COLUMN - 1)
}

/// The SHORT catalog: one `<name padded> <summary>` row per verb, in table order —
/// what the server answers a bare `help` with, bounded by
/// [`SHORT_CATALOG_MAX_BYTES`] in total. [`catalog_lines_full`] is the full form.
/// Both project the one table, so neither can drift from it (they ARE the table).
pub fn catalog_lines() -> impl Iterator<Item = String> {
    VERBS.iter().map(|s| catalog_row(s.name, s.summary))
}

/// The FULL catalog: one `<name padded> <summary detail>` row per verb, in table
/// order — `help --full`, and the `aterm help introspection` manual. Row for row
/// the catalog from before the summary/detail split, except the six rows (`help`,
/// `verbs`, `status`, `turn`, `lease`, `trail`) reworded on purpose so their first
/// sentence fits a summary (the golden test pins every row).
pub fn catalog_lines_full() -> impl Iterator<Item = String> {
    VERBS.iter().map(|s| catalog_row(s.name, &s.help_line()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire spelling round-trips, and every other shape — including a part
    /// that overflows `u64` — is refused, so a client that asks this reader
    /// never sends a fence the server refuses.
    #[test]
    fn screen_gen_parses_exactly_the_wire_spelling() {
        let g = ScreenGen::parse("3.15").expect("canonical spelling");
        assert_eq!(g, ScreenGen { epoch: 3, seq: 15 });
        assert_eq!(g.to_string(), "3.15");
        let max = ScreenGen {
            epoch: u64::MAX,
            seq: 0,
        };
        assert_eq!(ScreenGen::parse(&max.to_string()), Some(max));
        for bad in [
            "15",
            "3.",
            ".15",
            "3.15.1",
            "-3.15",
            "3.+15",
            "+3.15",
            "x.1",
            " 3.15",
            "",
            "18446744073709551616.1",
            "1.18446744073709551616",
        ] {
            assert_eq!(ScreenGen::parse(bad), None, "{bad:?}");
        }
    }

    /// Every json-capable verb must be a REAL table row, and a `Read` one — a
    /// typo or a removed verb would otherwise sit here silently doing nothing.
    #[test]
    fn json_capable_verbs_are_real_read_verbs() {
        assert!(!JSON_CAPABLE_VERBS.is_empty(), "non-vacuity");
        for v in JSON_CAPABLE_VERBS {
            let s = spec(v).unwrap_or_else(|| panic!("json-capable {v:?} is not in VERBS"));
            assert_eq!(s.op, Read, "json-capable {v:?} should be a Read verb");
        }
    }

    /// The regression: `metrics --json` framed as Status while the server wrote
    /// Lines, so the client consumed the `OK 1` header and dropped the JSON body.
    #[test]
    fn json_flag_switches_every_json_capable_verb_to_lines() {
        for v in JSON_CAPABLE_VERBS {
            for flag in ["--json", "json"] {
                assert_eq!(
                    framing_of(v, &format!("{v} {flag}")),
                    Lines,
                    "{v} {flag} must frame as Lines (the json_ok `OK 1\\n<body>` shape)"
                );
            }
        }
        // Negative control: WITHOUT the flag the verb keeps its table framing, so
        // the test above is not passing for a trivial reason.
        assert_eq!(framing_of("metrics", "metrics"), Status);
        assert_eq!(framing_of("cursor", "cursor"), Status);
    }

    #[test]
    fn table_has_no_duplicate_verbs() {
        let mut names: Vec<&str> = VERBS.iter().map(|s| s.name).collect();
        names.sort_unstable();
        let n = names.len();
        names.dedup();
        assert_eq!(names.len(), n, "duplicate verb in the table");
    }

    #[test]
    fn framing_base_and_sub_forms() {
        assert_eq!(framing_of("text", "text"), Lines);
        assert_eq!(framing_of("who", "who"), Lines);
        assert_eq!(framing_of("cast", "cast"), Bytes);
        assert_eq!(framing_of("subscribe", "subscribe @. screen"), Push);
        assert_eq!(framing_of("cursor", "cursor"), Status);
        assert_eq!(framing_of("bogus", "bogus"), Status);
        assert_eq!(framing_of("image", "image shot.png"), Status);
        assert_eq!(framing_of("image", "image --meta shot.png"), Status);
        assert_eq!(framing_of("image", "image --meta --bytes"), Lines);
        assert_eq!(framing_of("image", "@child image read"), Lines);
        assert_eq!(framing_of("cast", "@s-a cast frames"), Lines);
        // `video frames` lists rows (Lines); base `video`/`video status` stay Status.
        assert_eq!(framing_of("video", "video frames"), Lines);
        assert_eq!(framing_of("video", "@s-a video frames count=5"), Lines);
        assert_eq!(framing_of("video", "video 3 full"), Status);
        assert_eq!(framing_of("video", "video status"), Status);
        // `temporal status` is a Status line; base `temporal`/`temporal <tick>` stay Bytes.
        assert_eq!(framing_of("temporal", "temporal status"), Status);
        // `identities` lists (`OK <n>` + rows) and describes one (`OK <1+agents>`
        // + rows); its `forget` sub-form answers one status line.
        assert_eq!(framing_of("identities", "identities"), Lines);
        assert_eq!(framing_of("identities", "identities worker"), Lines);
        // A roster of the whole instance: answered before any session resolves.
        assert_eq!(spec("identities").map(|s| s.target), Some(Target::Meta));
        assert_eq!(framing_of("identities", "identities forget worker"), Status);
        assert_eq!(
            framing_of("identities", "identities forget worker confirm=worker"),
            Status
        );
        assert_eq!(framing_of("temporal", "temporal"), Bytes);
        assert_eq!(framing_of("temporal", "temporal 1200"), Bytes);
        assert_eq!(framing_of("temporal", "@s-a temporal status"), Status);
        // `--json` read mode replies Lines-framed (`OK 1\n<body>`) for every
        // json-capable verb; the Status-base ones (cursor/dims) must flip or the
        // client drops the JSON body. Both `--json` and bare `json` are accepted.
        assert_eq!(framing_of("cursor", "cursor --json"), Lines);
        assert_eq!(framing_of("dims", "dims json"), Lines);
        assert_eq!(framing_of("cursor", "@s-a cursor --json"), Lines);
        assert_eq!(framing_of("edges", "edges --json"), Lines);
        // Without the flag, cursor/dims keep their Status framing.
        assert_eq!(framing_of("dims", "dims"), Status);
        // Session metadata: `meta` (and its set/unset sub-forms) are single
        // status lines; `timeline` streams `OK <n>` + n event lines.
        assert_eq!(framing_of("meta", "meta"), Status);
        assert_eq!(framing_of("meta", "meta set title build agent"), Status);
        assert_eq!(framing_of("meta", "@s-a meta unset icon"), Status);
        assert_eq!(framing_of("timeline", "timeline"), Lines);
        assert_eq!(framing_of("timeline", "@s-a timeline 10 since=3"), Lines);
        // The admission-diagnosis ring streams `OK <n>` + n rows, with no
        // sub-form to change that (`trail 5` is a count, not a mode).
        assert_eq!(framing_of("trail", "trail"), Lines);
        assert_eq!(framing_of("trail", "@s-a trail 5"), Lines);
        // The Subject+Status record is ONE versioned status line, with no
        // sub-form to change that.
        assert_eq!(framing_of("status", "status"), Status);
        assert_eq!(framing_of("status", "@s-a status"), Status);
    }

    #[test]
    fn artifact_ack_classification_covers_every_guarded_subform() {
        for (verb, request) in [
            ("image", "image"),
            ("image", "image shot.png"),
            ("window", "window prefs shot.png"),
            ("video", "video 3 full"),
            ("video", "@s-a video frames count=4"),
        ] {
            assert!(
                artifact_reply_requires_ack(verb, request),
                "{request} must acknowledge exact artifact retention"
            );
        }
        assert!(!artifact_reply_requires_ack("video", "video status"));
        assert!(!artifact_reply_requires_ack("video", "video stop"));
        assert!(!artifact_reply_requires_ack("image", "@s-a image read"));
        assert!(!artifact_reply_requires_ack("image", "image --bytes"));
        assert!(!artifact_reply_requires_ack(
            "image",
            "@s-a image --meta --bytes"
        ));
        assert!(!artifact_reply_requires_ack("image", "@s-a image bytes"));
        assert!(!artifact_reply_requires_ack("text", "text"));
        assert!(valid_artifact_ack_nonce("00112233445566778899aabbccddeeff"));
        assert!(!valid_artifact_ack_nonce("artifact"));
    }

    #[test]
    fn every_verb_has_a_nonempty_help_line() {
        assert_eq!(catalog_lines().count(), VERBS.len());
        assert_eq!(catalog_lines_full().count(), VERBS.len());
        assert!(VERBS.iter().all(|s| !s.summary.is_empty()));
        assert!(spec("image").unwrap().help_line().contains("image --meta"));
    }

    /// Help that RESTATES THE VERB NAME and adds nothing.
    ///
    /// Ported from clean's help-truth C2, which found this to be the commonest
    /// help failure in a sibling CLI (`--verbose: "Show verbose output"`, 32
    /// instances of that one string). This catalog measures **0** across every
    /// row of `VERBS` today (99 rows at the 2026-09-10 read),
    /// which is the reason to pin it rather than to skip it: the check costs a
    /// millisecond and the property is one a hurried entry loses first.
    ///
    /// A row fails when, after removing the verb's own words and a list of
    /// filler verbs and articles, at most ONE content word survives — i.e. the
    /// summary told a reader nothing they could not have read off the name.
    #[test]
    fn no_summary_is_a_restatement_of_the_verb_name() {
        const FILLER: &[&str] = &[
            "the", "a", "an", "for", "of", "to", "and", "or", "this", "its", "it", "in", "on",
            "with", "from", "print", "prints", "show", "shows", "display", "displays", "run",
            "runs", "get", "gets", "set", "sets", "output", "command", "current", "aterm", "ctl",
        ];
        let words = |s: &str| -> Vec<String> {
            s.to_lowercase()
                .split(|c: char| !c.is_ascii_alphanumeric())
                .filter(|w| !w.is_empty())
                .map(str::to_string)
                .collect()
        };
        let mut bad: Vec<String> = Vec::new();
        for spec in VERBS {
            let own: std::collections::HashSet<String> = words(spec.name).into_iter().collect();
            let content = words(spec.summary)
                .into_iter()
                .filter(|w| !own.contains(w) && !FILLER.contains(&w.as_str()))
                .count();
            if content <= 1 {
                bad.push(format!("{}: {:?}", spec.name, spec.summary));
            }
        }
        assert!(
            bad.is_empty(),
            "{} catalog summary(ies) restate the verb name and add nothing — a \
             reader who did not know what the verb does still does not:\n  {}",
            bad.len(),
            bad.join("\n  ")
        );
    }

    /// Every repo-rooted path a catalog entry names must exist.
    ///
    /// Ported from clean's help-truth C3. A reader who follows a path out of
    /// help and gets nothing cannot tell whether they typed it wrong or the tool
    /// is lying.
    ///
    /// **This catalog names ZERO repo paths today, so the check is vacuous over
    /// it — and that is stated rather than hidden.** The first draft of this
    /// comment claimed the catalog named one (`docs/AGENT-EXPERIENCE-…`, which
    /// is in a doc comment on line 162, not in any `summary`/`detail`), and the
    /// plant that should have proved the check red PASSED. A gate that reads
    /// nothing reports success, which is the failure mode this whole family of
    /// checks exists to prevent, so the extractor is proved on a synthetic
    /// entry in the same test: if it ever stops finding a planted path, the
    /// test fails whatever the catalog holds.
    ///
    /// The surface that DOES name repo paths is the manual
    /// (`crates/aterm-cli/src/manual.rs`); it has its own check.
    #[test]
    fn every_repo_path_named_in_the_catalog_exists() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(std::path::Path::parent)
            .expect("crates/aterm-types has a workspace root two levels up");
        const ROOTS: &[&str] = &["crates/", "scripts/", "docs/", "tests/", "data/"];
        let scan = |name: &str, text: &str, missing: &mut Vec<String>| {
            for raw in text.split(|c: char| c.is_whitespace() || c == '`' || c == '"') {
                let tok = raw.trim_matches(|c: char| {
                    !c.is_alphanumeric() && c != '/' && c != '.' && c != '-' && c != '_'
                });
                if ROOTS.iter().any(|r| tok.starts_with(r)) && !root.join(tok).exists() {
                    missing.push(format!("{name}: `{tok}`"));
                }
            }
        };

        // THE EXTRACTOR IS PROVED FIRST. Everything below is vacuous over a
        // catalog that names no paths, and a vacuous check reports success.
        let mut probe = Vec::new();
        scan(
            "synthetic",
            "see `docs/NO-SUCH-FILE-9f3a.md` and crates/aterm-types/src/control_verbs.rs",
            &mut probe,
        );
        assert_eq!(
            probe,
            vec!["synthetic: `docs/NO-SUCH-FILE-9f3a.md`".to_string()],
            "the extractor must find a missing path and pass a real one; if this \
             fails the check below proves nothing about the catalog"
        );

        let mut missing = Vec::new();
        for spec in VERBS {
            scan(spec.name, spec.summary, &mut missing);
            scan(spec.name, spec.detail, &mut missing);
        }
        assert!(
            missing.is_empty(),
            "{} repo-rooted path(s) named in the verb catalog do not exist:\n  {}",
            missing.len(),
            missing.join("\n  ")
        );
    }

    /// The two-tier contract: every summary fits one catalog row, and the whole
    /// short catalog fits the discovery budget — COMPUTED from the table, so a row
    /// that grows past either bound fails here rather than in an agent's context
    /// window. Also the split's hygiene: no summary is padded or doubled-spaced (the
    /// wrap and the re-join both rely on single spaces), and `help_line` is exactly
    /// `summary`, one space, `detail`.
    #[test]
    fn summaries_fit_the_short_catalog_budget() {
        let mut total = 0usize;
        for s in VERBS {
            let n = s.summary.chars().count();
            assert!(
                n <= SUMMARY_MAX_CHARS,
                "{}: summary is {n} chars, over the {SUMMARY_MAX_CHARS} cap: {:?}",
                s.name,
                s.summary
            );
            for (field, text) in [("summary", s.summary), ("detail", s.detail)] {
                assert!(
                    !text.contains("  "),
                    "{}: {field} has a run of spaces",
                    s.name
                );
                assert_eq!(text.trim(), text, "{}: {field} is padded", s.name);
            }
            assert!(
                !s.detail.starts_with(' '),
                "{}: detail starts with a space",
                s.name
            );
            let expect = if s.detail.is_empty() {
                s.summary.to_string()
            } else {
                format!("{} {}", s.summary, s.detail)
            };
            assert_eq!(
                s.help_line(),
                expect,
                "{}: help_line is not summary + ' ' + detail",
                s.name
            );
            total += catalog_row(s.name, s.summary).len() + 1;
        }
        let short: usize = catalog_lines().map(|l| l.len() + 1).sum();
        assert_eq!(short, total);
        assert!(
            short <= SHORT_CATALOG_MAX_BYTES,
            "the short catalog is {short} bytes, over the {SHORT_CATALOG_MAX_BYTES} budget"
        );
        // Non-vacuity: the split is real — the table carries detail somewhere, and the
        // full catalog is far larger than the short one.
        assert!(VERBS.iter().any(|s| !s.detail.is_empty()));
        let full: usize = catalog_lines_full().map(|l| l.len() + 1).sum();
        assert!(full > 2 * short, "full {full} B vs short {short} B");
    }

    /// `entry_lines` is the `help <verb>` body: first row `<name padded> <text>`, every
    /// continuation row indented to the text column, no row wider than the wrap
    /// width unless a single word is, and re-joining the text reproduces `help_line`
    /// byte-for-byte — for EVERY verb, so the wrap is provably lossless and stable.
    #[test]
    fn entry_lines_wrap_every_verb_deterministically_and_losslessly() {
        let gutter = " ".repeat(CATALOG_TEXT_COLUMN);
        let text_width = ENTRY_WRAP_COLUMNS - CATALOG_TEXT_COLUMN;
        for s in VERBS {
            let lines = s.entry_lines();
            assert!(!lines.is_empty());
            let head = format!("{:<28} ", s.name);
            assert!(
                lines[0].starts_with(&head),
                "{}: first row leads with the name",
                s.name
            );
            let mut texts = vec![lines[0][head.len()..].to_string()];
            for l in &lines[1..] {
                assert!(
                    l.starts_with(&gutter),
                    "{}: continuation row not indented",
                    s.name
                );
                assert!(
                    !l[gutter.len()..].starts_with(' '),
                    "{}: doubled indent",
                    s.name
                );
                texts.push(l[gutter.len()..].to_string());
            }
            for l in &lines {
                let chars = l.chars().count();
                let one_word = !l.trim_start().contains(' ');
                assert!(
                    chars <= ENTRY_WRAP_COLUMNS || one_word,
                    "{}: row of {chars} chars breaks the {ENTRY_WRAP_COLUMNS}-column wrap: {l:?}",
                    s.name
                );
            }
            for t in &texts {
                assert!(
                    !t.is_empty() && t.trim() == t,
                    "{}: ragged wrap chunk {t:?}",
                    s.name
                );
                // Greedy: a chunk only ends because the next word would not fit.
                assert!(t.chars().count() <= text_width || !t.contains(' '));
            }
            assert_eq!(texts.join(" "), s.help_line(), "{}: wrap lost text", s.name);
            assert_eq!(
                s.entry_lines(),
                lines,
                "{}: wrap is not deterministic",
                s.name
            );
        }
        // A short verb is exactly its one full-catalog row; a long one wraps.
        let one = spec("lines").unwrap();
        assert_eq!(
            one.entry_lines(),
            vec![catalog_row("lines", &one.help_line())]
        );
        assert!(spec("image").unwrap().entry_lines().len() > 5);
    }

    /// The `Access` exceptions are the single source of truth for the scope gate the
    /// dispatch used to hardcode. Pin BOTH sets exactly so the table↔dispatch binding
    /// is total: a verb cannot become owner-only / any-scope-meta without being
    /// classified here (and the dispatch reads these predicates, not a verb list).
    #[test]
    fn access_exceptions_are_exactly_the_declared_sets() {
        let owner_only: Vec<&str> = VERBS
            .iter()
            .filter(|s| matches!(s.access, Access::OwnerOnly))
            .map(|s| s.name)
            .collect();
        assert_eq!(
            owner_only,
            [
                "appnotice",
                // The message band's write face; a child edge may not raise, end or
                // press a row.
                "notice",
                // The presence band's write face (round 19): the watcher tells the
                // window what it decided; a child edge may not.
                "story",
                // Receiver-side broadcast opt-in. Owner-class because adding a topic
                // changes what lands in a session's inbox — the halt's authority
                // class, not a read's.
                "topic",
                // The typed re-key: it hands out a key that lets a session's marks
                // through, so a child edge may not mint one.
                "rekey",
                // The drive halt. Owner-class so the LOCAL owner can halt its own
                // drivers; the handler keeps the FLEET hold (set, replace, lift)
                // bridge-issued only — see `Access::OwnerOnly`'s doc.
                "hold",
                "operator",
                "operator-propose-bin",
                // The bridge supervisor of a running instance: arming it chooses
                // which process holds `Scope::Bridge`, so Owner is the floor.
                "fabric",
                "sessions",
                "who",
                "exits",
                // The on-disk identity roster and its `forget`: Owner like
                // `sessions`, whose `identity=` column it explains.
                "identities",
                "whoami",
                "grant",
                "revoke",
                "connect",
                "disconnect",
                "flows",
                "raise",
                "dial",
                "dial-list",
                "dial-token",
            ],
            "the OwnerOnly set (dispatch gates exactly these on Owner scope)",
        );

        let any_meta: Vec<&str> = VERBS
            .iter()
            .filter(|s| matches!(s.access, Access::AnyScopeMeta))
            .map(|s| s.name)
            .collect();
        assert_eq!(
            any_meta,
            ["version", "update", "help", "verbs", "privacy"],
            "the AnyScopeMeta set (answered pre-scope for any authenticated caller)",
        );

        // The BridgeOnly set. It is EXACTLY the verbs that would let a prompt-injected
        // agent holding Owner scope forge an attested human order into a sibling's
        // inbox (`deliver`, which stamps `from=`/`trust=`), read every session's
        // outbound traffic (`outbox`), or release a `post --wait` for a message that
        // never left the machine (`outbox sent`). Every in-session client already
        // holds Owner, so a member arriving here without that property is a real
        // widening of the one authority no token unlocks — which is why the set is
        // pinned, not counted. `hold` was the fourth until the LOCAL halt landed: the
        // Owner token is the local human's credential, so the verb is OwnerOnly and
        // its handler keeps the FLEET hold bridge-issued only (an Owner act against
        // a standing `origin=fleet` hold is refused), which is the property this set
        // carried for it.
        let bridge_only: Vec<&str> = VERBS
            .iter()
            .filter(|s| matches!(s.access, Access::BridgeOnly))
            .map(|s| s.name)
            .collect();
        // `link` joined the set in round 13: it is how the bridge reports its
        // own broker link, and an Owner client that could say `link up` would
        // turn `fabric=stalled` back into a `post --wait` that burns its timeout.
        assert_eq!(
            bridge_only,
            ["deliver", "link", "outbox", "outbox sent"],
            "the BridgeOnly set (only the inherited bridge connection may run these)",
        );

        // The predicates project the same truth, and the three exception sets are
        // pairwise DISJOINT — a verb has exactly one scope gate.
        for v in &owner_only {
            assert!(is_owner_only(v), "{v} is_owner_only");
            assert!(!is_any_scope_meta(v), "{v} not any-scope-meta");
            assert!(!is_bridge_only(v), "{v} not bridge-only");
        }
        for v in &any_meta {
            assert!(is_any_scope_meta(v), "{v} is_any_scope_meta");
            assert!(!is_owner_only(v), "{v} not owner-only");
            assert!(!is_bridge_only(v), "{v} not bridge-only");
        }
        for v in &bridge_only {
            assert!(is_bridge_only(v), "{v} is_bridge_only");
            assert!(!is_owner_only(v), "{v} not owner-only");
            assert!(!is_any_scope_meta(v), "{v} not any-scope-meta");
        }
        // A normal `Scoped` verb is none of them; unknown verbs are none of them.
        assert!(!is_owner_only("text") && !is_any_scope_meta("text") && !is_bridge_only("text"));
        assert!(!is_owner_only("bogus") && !is_any_scope_meta("bogus") && !is_bridge_only("bogus"));
        // The fabric verbs an agent inside the session calls are ORDINARY scoped verbs:
        // classifying `inbox`/`post` bridge-only would leave the agent unable to read
        // its own mail, which is the whole point of the ring.
        for v in ["inbox", "inbox get", "inbox seen", "post"] {
            assert!(
                matches!(
                    spec(v).expect("fabric verb is in the table").access,
                    Access::Scoped
                ),
                "{v} is a scoped verb"
            );
        }
    }

    /// The fabric rows carry exactly the classes §11.2 of the fabric design gives
    /// them, and `framing_of` flips BOTH `inbox` sub-forms. The classes are what the
    /// server gates on and what the client parses with, so a silent change to one of
    /// them is a protocol change with no other alarm.
    #[test]
    fn fabric_verbs_carry_their_designed_classes() {
        for (name, op, framing, target, access) in [
            ("inbox", Read, Lines, Session, Scoped),
            ("inbox get", Read, Bytes, Session, Scoped),
            ("inbox seen", Write, Status, Session, Scoped),
            ("topic", Owner, Lines, Session, OwnerOnly),
            ("post", Write, Status, Session, Scoped),
            ("deliver", Write, Status, Meta, BridgeOnly),
            ("link", Write, Status, Meta, BridgeOnly),
            // Owner-class, not bridge-only: the local owner's halt rides the Owner
            // token; the fleet hold is kept bridge-issued in the handler.
            ("hold", Write, Status, Meta, OwnerOnly),
            ("outbox", Read, Bytes, Meta, BridgeOnly),
            ("outbox sent", Write, Status, Meta, BridgeOnly),
        ] {
            let s = spec(name).unwrap_or_else(|| panic!("{name} is in the table"));
            assert_eq!(s.op, op, "{name} op-class");
            assert_eq!(s.framing, framing, "{name} framing");
            assert_eq!(s.target, target, "{name} target");
            assert_eq!(s.access, access, "{name} access");
        }
        // The sub-form flip: the base listing is Lines, `get` is a byte body, `seen`
        // is a status line. Selector-aware, like every other sub-form rule.
        assert_eq!(framing_of("inbox", "inbox"), Lines);
        assert_eq!(framing_of("inbox", "inbox 20 since=4 --meta"), Lines);
        assert_eq!(framing_of("inbox", "inbox get 7"), Bytes);
        assert_eq!(framing_of("inbox", "@s-a inbox get 7"), Bytes);
        assert_eq!(framing_of("inbox", "inbox seen 7 handled"), Status);
        assert_eq!(framing_of("inbox", "@s-a inbox seen 7"), Status);
        // `deliver`/`hold` are single status lines with no sub-form to change that.
        assert_eq!(
            framing_of("deliver", "deliver s-a off=9 from=h-x kind=task"),
            Status
        );
        assert_eq!(framing_of("hold", "hold s-a on reason=x"), Status);
        assert_eq!(framing_of("post", "post to=@s-b kind=ask hello"), Status);
        // `outbox` flips the same way `inbox` does: the base drain is a byte frame,
        // the `sent` sub-form a status line. A client that read `OK` as a byte count
        // would park waiting for a body that never comes.
        assert_eq!(framing_of("outbox", "outbox"), Bytes);
        assert_eq!(framing_of("outbox", "outbox 8"), Bytes);
        assert_eq!(framing_of("outbox", "outbox sent s-a 3 off=91"), Status);
    }

    /// `copy` is the clipboard-exfil boundary and `settings` rewrites durable config
    /// — each split OUT of the coarse Read/Write class into its own op-class, so a
    /// read-only / keystroke-only edge can no longer reach them.
    #[test]
    fn copy_and_settings_have_their_own_op_class() {
        assert_eq!(spec("copy").unwrap().op, OpClass::ClipboardWrite);
        assert_eq!(spec("settings").unwrap().op, OpClass::ConfigWrite);
        // scroll/select stay Read — viewport nav is part of reading, nothing leaves.
        assert_eq!(spec("scroll").unwrap().op, OpClass::Read);
        assert_eq!(spec("select").unwrap().op, OpClass::Read);
    }

    /// The `privacy` row's SHAPE is load-bearing, not cosmetic. `AnyScopeMeta` is
    /// why a `--headless` instance answers it at all (`v()` would have hardcoded
    /// `Access::Scoped`, under which the consent readout is unavailable to exactly
    /// the caller it exists for); `Meta` rejects a selector; `Read` because every
    /// fact it aggregates is one `status` already exposes a session at a time, so
    /// the verb introduces no new authority; `Lines` because it replies `OK <n>`
    /// plus n rows.
    ///
    /// And its help obeys the scope ruling (design §7 S4): a grant mitigates a
    /// class of interruption for the folders it covers, and which services those
    /// are is unmeasured, so the wire help may never say the grant ends
    /// prompting. No grep_guard rule reads these words, and the help golden
    /// stops only an accidental change, not a deliberate re-record. "restart" is
    /// deliberately not in the list: the help truthfully says a per-process
    /// grant "cannot hold across a restart".
    #[test]
    fn privacy_is_a_headless_answerable_meta_read() {
        let s = spec("privacy").expect("privacy is in the table");
        assert_eq!(
            s.op, Read,
            "privacy is a Read verb (json-capable verbs must be)"
        );
        assert_eq!(s.framing, Lines, "privacy replies `OK <n>` + n lines");
        assert_eq!(
            s.target, Meta,
            "privacy is self-scoped; a selector is rejected"
        );
        assert_eq!(
            s.access, AnyScopeMeta,
            "privacy must be AnyScopeMeta or a headless instance cannot answer it"
        );
        assert!(is_any_scope_meta("privacy") && !is_owner_only("privacy"));
        assert!(
            JSON_CAPABLE_VERBS.contains(&"privacy"),
            "privacy --json is part of the contract"
        );
        assert_eq!(framing_of("privacy", "privacy --json"), Lines);
        assert_eq!(framing_of("privacy", "privacy"), Lines);

        let help = s.help_line().to_ascii_lowercase();
        for overclaim in [
            "all prompts",
            "every prompt",
            "no more prompts",
            "never prompt",
            "eliminates",
        ] {
            assert!(
                !help.contains(overclaim),
                "privacy help overclaims what a grant does: {overclaim:?}\n{help}"
            );
        }
    }
}
