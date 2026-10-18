// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The control-protocol VERB TABLE: the single, typed source of truth for every
//! control-socket verb. One [`VerbSpec`] row per verb carries its op-class,
//! reply framing, targeting class, and help text — a first-sentence `summary`
//! plus the `detail` that completes it. Everything else projects from here: the
//! server maps `op` to its auth `Op` and generates BOTH help catalogs from the
//! two fields (the short one from the summaries, the full one from
//! [`VerbSpec::help_line`]); the aterm-ctl client parses replies by `framing`.
//! Client framing, server authorization metadata and help catalogs therefore
//! share one declaration. The separate handlers must still implement the
//! declared behavior. This lives in `aterm-types`, alongside `control_socket`,
//! so both the client library and the GUI host use the same protocol metadata.

/// The byte cap of one KEYED attention entry (`meta set attention owner=<k>
/// <text>`), after trim — the server refuses a longer one (`ERR attention
/// too long (max 200 bytes)`), and a supervisor cuts its text to it. One
/// constant for both ends, so no escalation is cut longer than the server takes.
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
    /// (`sessions`/`who`/`grant`/`revoke`/`whoami`) — the instance god-token,
    /// and the bridge connection, which carries Owner's power. A selector is rejected.
    ///
    /// TWO of its members then have their HANDLER tell the two owner-class scopes
    /// apart, because no table row can. `hold`: the Owner token is the local human's
    /// own credential, so it may set and lift a LOCAL halt (`origin=local`) on its own
    /// sessions, while the FLEET halt (`origin=fleet` — set, replaced, or lifted)
    /// stays reachable only from the bridge connection; each origin is its own slot
    /// on the session, and an Owner `off` that finds only the fleet's standing is
    /// refused, which turns on the STANDING holds, which the table cannot see. `fabric`: the bridge
    /// connection is refused outright — `Scope::Owner` exactly, not owner-CLASS —
    /// because arming the supervisor chooses which process holds the bridge scope, and
    /// the bridge is that process rather than a party to arming it. For every other
    /// member the class IS the whole gate. What the class buys in all cases is the
    /// same: an edge token is kept out and a selector is rejected.
    OwnerOnly,
    /// Bridge-only: ONLY the fabric bridge connection may run it — the pair of
    /// `socketpair` ends the instance keeps when it launches its `aterm-link serve`
    /// child, served with the bridge scope PRE-RESOLVED instead of an `AUTH` line.
    /// The pinned set is FOUR verbs: `deliver`, `link`, `outbox` and `outbox sent`.
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
    /// `hold` is [`Self::OwnerOnly`] (the Owner token is the local human's credential);
    /// its handler keeps the FLEET hold bridge-issued, refusing `origin=fleet` from
    /// Owner outright and an Owner `off` that finds only a fleet hold standing, so an
    /// injected agent holding Owner still cannot lift a fleet halt.
    ///
    /// A FLEET hold has NO OPERATOR UNDO: no GUI, palette, key-binding or Owner path
    /// clears one (`aterm-gui`'s `fabric::apply_hold` is called only by `hold` and
    /// `bridge_lost`, the latter only under `[fabric] hold_on_bridge_lost = true`). A
    /// `reason=fabric-lost` hold stands until a bridge reconnects and
    /// issues `hold off`; failing that, only quitting (which ends the sessions)
    /// clears it — a seamless update carries the holds (`SessionRecord::hold` and
    /// `more_holds`), and so does a PTY keeper's crash recovery (`fabric::KeeperCarry`).
    /// DESIGN §11.2's "or a human lifts it at the GUI" does not exist. A LOCAL hold
    /// lifts with the owner's own `hold off`, and only with it: a fleet lift leaves it.
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
    /// the other [`Access`] values for the exceptions.
    pub access: Access,
    /// The FIRST SENTENCE of the verb's help, at most `summary_max_chars!()` (96) chars:
    /// the whole of this verb's row in the SHORT catalog (a bare `help`) and the head
    /// of its full entry. Short because the catalog an agent reads to FIND a verb must
    /// stay under [`SHORT_CATALOG_MAX_BYTES`].
    pub summary: &'static str,
    /// The rest of the entry, or `""`. Shown by `help <verb>` and in the full catalog
    /// only; [`VerbSpec::help_line`] joins it after the summary with one space.
    pub detail: &'static str,
}

/// The cap on a [`VerbSpec::summary`] as a literal: one readable row after the
/// [`CATALOG_TEXT_COLUMN`] gutter on a 125-column line. The `help`/`verbs` rows
/// quote it through `concat!`, so the number an agent reads in the catalog and the
/// number the compile-time check below enforces are one token that cannot drift apart.
macro_rules! summary_max_chars {
    () => {
        96
    };
}

/// THE ONE TABLE OF GUI TARGETS: each row a target's variant, its keyword,
/// the aliases that resolve to it, and — by its group — which verbs take it.
/// `front` is the window itself; `opens` the surfaces `open <target>
/// [close]` opens and closes; `gestures` the two only a gesture opens (they
/// need a pair or an intent the wire cannot give). `window` photographs and
/// `controls` reads every one. From it come [`AuxTarget`] (its `parse`,
/// `keyword` and `opens`), the help entries of `open`, `window` and
/// `controls`, and the refusals of all three, so a target cannot be taken by
/// a verb whose help does not name it, nor named and refused.
macro_rules! aux_targets {
    (
        front: $(#[$fdoc:meta])* $front:ident = $fkw:literal $(| $falias:literal)*;
        opens: $(#[$o0doc:meta])* $o0:ident = $o0kw:literal $(| $o0alias:literal)*
            $(, $(#[$odoc:meta])* $o:ident = $okw:literal $(| $oalias:literal)*)*;
        gestures: $($(#[$gdoc:meta])* $g:ident = $gkw:literal $(| $galias:literal)*),*;
    ) => {
        /// A GUI surface the `window`, `controls` and `open` verbs name, beyond
        /// the bare front window: Settings routes and the front window's
        /// overlays. Parsed from a verb's target keyword ([`Self::parse`]).
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        pub enum AuxTarget {
            $(#[$fdoc])* $front,
            $(#[$o0doc])* $o0,
            $($(#[$odoc])* $o,)*
            $($(#[$gdoc])* $g,)*
        }

        impl AuxTarget {
            /// Every target, in the table's order: the order the help and the
            /// refusals name them in.
            pub const ALL: &'static [AuxTarget] =
                &[Self::$front, Self::$o0 $(, Self::$o)* $(, Self::$g)*];

            /// Parse a verb's target keyword or alias, case-insensitively;
            /// the empty word is the front window. `None` for any other word,
            /// so the verb refuses it naming the targets.
            #[must_use]
            pub fn parse(s: &str) -> Option<Self> {
                match s.trim().to_ascii_lowercase().as_str() {
                    "" | $fkw $(| $falias)* => Some(Self::$front),
                    $o0kw $(| $o0alias)* => Some(Self::$o0),
                    $($okw $(| $oalias)* => Some(Self::$o),)*
                    $($gkw $(| $galias)* => Some(Self::$g),)*
                    _ => None,
                }
            }

            /// The target's keyword (for replies, refusals and default
            /// capture file names).
            #[must_use]
            pub const fn keyword(self) -> &'static str {
                match self {
                    Self::$front => $fkw,
                    Self::$o0 => $o0kw,
                    $(Self::$o => $okw,)*
                    $(Self::$g => $gkw,)*
                }
            }

            /// Whether `open <target> [close]` opens (and closes) it.
            #[must_use]
            pub const fn opens(self) -> bool {
                matches!(self, Self::$o0 $(| Self::$o)*)
            }
        }

        /// The targets `open` takes, ` | `-joined, as a literal.
        macro_rules! open_targets {
            () => {
                concat!($o0kw $(, " | ", $okw)*)
            };
        }
        /// The targets `window` and `controls` take, ` | `-joined.
        macro_rules! window_targets {
            () => {
                concat!($fkw, " | ", $o0kw $(, " | ", $okw)* $(, " | ", $gkw)*)
            };
        }
        /// The aliases the open targets also answer to, each `alias=keyword`.
        macro_rules! open_aliases {
            () => {
                concat!(
                    "aliases" $(, " ", $o0alias, "=", $o0kw)*
                    $($(, " ", $oalias, "=", $okw)*)*
                )
            };
        }
    };
}

aux_targets! {
    front:
        /// The frontmost terminal window (the plain `window`/`chrome`
        /// capture).
        Front = "front" | "window" | "terminal";
    opens:
        /// The native Settings tab. `controls prefs` keeps its historical
        /// line prefixes; its route, preference rows and `ui` records come
        /// from the compiled native semantic tree (`inspect app/v1` is the
        /// versioned form).
        Prefs = "prefs" | "preferences" | "settings",
        /// The native Settings `/about` route, projected from its compiled
        /// semantic frame while that route is open.
        About = "about",
        /// The in-window command palette, an own-rendered card on the front
        /// window; `controls menu` lists its (filtered) command rows.
        Menu = "menu" | "palette",
        /// The in-grid session context menu (the tab menu); `controls
        /// tab-menu` lists the open card's rows and cursor, and a closed one
        /// is `tab-menu open=false`.
        TabMenu = "tab-menu",
        /// The Autopilot menu, the chip's card over the pane; `controls
        /// autopilot-menu` lists the open card's rows by their stable keys
        /// (`autopilot-menu/mode/<word>`, `autopilot-menu/land-in/<minutes>`,
        /// `autopilot-menu/land-at`, `autopilot-menu/land-cancel`,
        /// `autopilot-menu/settings`), and `open autopilot-menu` opens it
        /// from the front window's focused session's chip.
        AutopilotMenu = "autopilot-menu",
        /// The connection map; `open connections` raises it on the front
        /// window, and every wire read of it is Owner-gated.
        Connections = "connections",
        /// The native Settings `/updates` route, projected from its compiled
        /// semantic frame while that route is open.
        Update = "update" | "software-update";
    gestures:
        /// The connection confirm/configure card: it needs a pair only the
        /// gestures (menu, picker, drag) carry, so it is never `open`ed from
        /// the wire; `controls conn-card` reads the open card.
        ConnCard = "conn-card",
        /// The session picker (`invoke ConnectToSession`); `controls
        /// session-picker` reads its intent and filtered rows.
        SessionPicker = "session-picker";
}

/// The aux targets `open` opens, ` | `-separated ([`AuxTarget::opens`]).
pub const OPEN_TARGETS: &str = open_targets!();
/// The targets `window` and `controls` take, ` | `-separated: every
/// [`AuxTarget`].
pub const WINDOW_TARGETS: &str = window_targets!();

/// The chars in `s`, in a const context: a UTF-8 continuation byte is not a char.
const fn char_count(s: &str) -> usize {
    let bytes = s.as_bytes();
    let (mut i, mut chars) = (0, 0);
    while i < bytes.len() {
        if bytes[i] & 0xC0 != 0x80 {
            chars += 1;
        }
        i += 1;
    }
    chars
}

// Every summary fits its catalog row and the rows together fit the short catalog's
// budget: checked when this crate compiles, over the table itself, so the caps the
// catalog quotes hold by construction.
const _: () = {
    let (mut v, mut total) = (0, 0);
    while v < VERBS.len() {
        let (name, summary) = (VERBS[v].name, VERBS[v].summary);
        let chars = char_count(summary);
        assert!(
            chars > 0 && chars <= summary_max_chars!(),
            "a control verb's summary is empty or over summary_max_chars!()"
        );
        // `catalog_row`'s bytes: the name padded to the gutter, one space, the
        // summary, then the row's `\n`.
        let pad = (CATALOG_TEXT_COLUMN - 1).saturating_sub(char_count(name));
        total += name.len() + pad + 1 + summary.len() + 1;
        v += 1;
    }
    assert!(
        total <= SHORT_CATALOG_MAX_BYTES,
        "the short catalog's rows are over SHORT_CATALOG_MAX_BYTES"
    );
};

/// The budget for the short catalog's rows put together (every [`catalog_lines`] row,
/// `\n`-terminated): what discovering a verb costs, whatever the table grows to.
///
/// The bound is on the SHAPE (one row per verb, first sentence only, capped at
/// `summary_max_chars!()`, 96), so a new verb raises it by about its own row; never reword
/// unrelated verbs' help to make room. Checked at compile time with the summary cap.
pub const SHORT_CATALOG_MAX_BYTES: usize = 12696;

/// THE ONE `subscribe` STREAM VOCABULARY: frame sources first, then the two
/// modifiers. The catalog row, the `ERR usage` refusal, `Requested::parse`'s fold
/// table and the installed drive skill derive from it or are tested against it.
pub const SUBSCRIBE_STREAMS: &[&str] = &[
    "screen",
    "cursor",
    "events",
    "cells",
    "bytes",
    "mail",
    "log",
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
    /// is any.
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
/// whole row — at most `summary_max_chars!()`, 96) and the `detail` that completes it
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
         `trustc_host=` carry the compiler's rustc-shaped release token, commit and host \
         (there is no `rustc=`). `trustc=` and `rust_compat=` are the same value: Trust \
         prints a rustc-shaped release for cargo's sake, and `trust=` is where it says its own.",
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
         checker_deferred=<n> (the check loop's own health), apply_phase= and apply_held= \
         (automatic postures only), apply_policy_reason=, \
         delivery=deferred|blocked, installable=false (a disk-image, translocated or dev copy \
         never updates), apply_refusal= apply_refusal_at= apply_failure=, changelog=, and on \
         macOS trial_wait= (the build's boot trial waiting for a window to confirm it, and \
         until when, or the recovery proof that refused a confirmation of it), \
         freeze_seed_ms= handoff_capture_ms= (once a seamless handoff has timed its \
         capture: the first freeze rung the last dry run seeded, and the capture it timed) and \
         handoff_park_ms= (once a park has landed: what it really cost, readers stopped to \
         capture done — the floor the next first rung is budgeted from) — prose values \
         pct-encoded. apply_posture: automatic, automatic-idle, manual-config, \
         disabled-env, retry-wait, manual-only, handoff-unavailable, applying, unreconciled, \
         none, disabled, unknown. apply_phase: prefer-idle, prefer-output-gap, keys-only, land \
         — the build is running within a minute of being armed, whatever the terminal is \
         doing, unless apply_held= names the one hold that outlasts the ladder: \
         restored-agents, while the agents of the tabs a restore reopened are still being \
         relaunched (at most seven minutes from the first relaunch, once per process). \
         Linux adds linux_installed_build= linux_staged_build= linux_trial=<phase|none> \
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
    // `Owner`: every fact here is one `status` already exposes a session at a time
    // (so an edge's report carries only the sessions it may read),
    // or an instance-level fact about this Mac that any program running as this
    // user can read for itself (`DevToolsSecurity -status`, the unified log).
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
         split, one `folder` line, the `claimants=` census, `prompt_possible=`, one `session` line per LIVE session the caller may \
         read (an edge scope sees only the sessions it holds a read edge into; never truncated, so \
         `sessions_total=` always equals the number of `session` lines), then \
         `containment`, `warmup=`, `debugger`, `devtools`, `observer`, `access_card`, `remediate` and a closing `note`. Free text is \
         pct-encoded and `-` is unset. A background check in flight reports `full_disk_access=unknown` \
         and `probe=pending`, with no sample age (`-`, or null in JSON). Reading this verb raises NO dialog: the probe reads state \
         that already exists, and the ONE value derived from another is `covers=app-data`, which \
         follows the completed grant by Apple's documented rule and nothing else. Per-folder state \
         defaults to `unknown` BY CONSTRUCTION — the only way to learn whether a folder is \
         readable is to read it, which is the very act that raises the prompt — and a `folder` \
         value leaves `unknown` for exactly two reasons: aterm itself observed an access (its \
         own EPERM, or a warm-up: the human's press in Settings, or the window's own pass at \
         launch while Full Disk Access reads off; rendered `allowed`/`denied`/`asking`/`error`), \
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
         age_ms=<n>`, then a `claimant path= signing= dr= team= conflicts=yes trash=` line per \
         copy whose requirement differs (at most 8; the counts stay true) and one `note`. \
         `trash=` is whether Settings ▸ Security's Move to Trash takes the copy, by the rule its \
         button reads: `offered`, or the first reason it does not — `signature` (signed with a \
         certificate, or unreadable), `requirement` (ad hoc, but not to its cdhash), `linked` \
         (a link leads to it), `running-unstable` (the running copy's grant does not survive \
         updates) or `no-tool` (no /usr/bin/trash). A copy \
         sharing the running requirement is counted, not listed. Candidates are the running \
         bundle's folder, /Applications, ~/Applications and every copy LaunchServices has \
         registered, except a copy in the Trash; `partial` means one of them could not be \
         read, and only `complete` may say `sole=yes`. A copy is a bundle, counted once \
         however it is spelled: a link is the bundle it leads to, and `path=` names the \
         bundle. `claimants=-` with `census=pending` is a first census still out; \
         `census=off` was never asked (headless, non-macOS, privacy disabled, or no readable \
         bundle id). The verb only reports; it never moves a bundle. \
         Settings ▸ Security lists the conflicting copies and offers to move the `trash=offered` \
         ones to the Trash: an owner press confirmed in an alert, which `act` refuses. \
         The `debugger` row is NOT privacy: it is the Authorization Services right \
         `system.privilege.taskport.debug`, the password macOS asks for when a debugger takes \
         control of another process (\"Developer Tools Access needs to take control of another \
         process\"). It is keyed to this user and this Mac, not to aterm, so Full Disk Access \
         does not reach it. It reads `debugger mode=<enabled|disabled|unrecognized|unknown> \
         member=<admin|developer|no|unknown> self_hardened=<yes|no|unknown> \
         right=system.privilege.taskport.debug probe=<label> probe_age_ms=<n|-> pending= \
         pending_right= pending_age_ms= last= watch_ms=`. `mode=` is developer mode as Apple's \
         `DevToolsSecurity -status` reports it, read by a background check that raises no \
         dialog: `unrecognized` means Apple's tool does not recognise the rule, and names no \
         cause; `unknown` means not observed, never `disabled`. `member=` is the first \
         developer-mode rule this account matches. `self_hardened=no` means this aterm runs \
         without the hardened runtime or with `get-task-allow`, so under `mode=enabled` any \
         program running as this user can attach to it. `pending=` is about a password request \
         for this user on this Mac, whatever app asked, and names no requester; this build \
         does not watch for one, so it reads `off` and the fields after it are unset, as is \
         `observer taskport=`. `off`, `unavailable` and `unknown` are not `no`. Only a human \
         can change `mode=` (it takes an administrator's password): \
         `remediate debugger=` names a place for that human, never a command: \
         `aterm-settings:security`, aterm's Settings ▸ Security, where the owner can allow or \
         turn off debugger access for this Mac — their press, confirmed in aterm's alert; macOS \
         should then ask for an administrator password in its own dialog. Running \
         `DevToolsSecurity` \
         yourself, with any flag, or `sudo` is not the fix; `await consent kind=debugger` waits \
         for `mode=` to change. \
         The `devtools` row is tccd's answer for aterm on macOS's Privacy & Security ▸ \
         Developer Tools list (`kTCCServiceDeveloperTool`), which macOS asks before a new \
         program run in aterm first starts. It reads `devtools \
         state=<allowed|denied|unlisted|unknown> subject=<bundle id|-> requests=<n|-> \
         not_allowed=<n|-> answer_age_ms=<n|-> window_s=600 probe=<label> probe_age_ms=<n|-> \
         grant=<granted|not_granted|unknown> list=<on|off|absent|unknown> list_probe=<label>`. \
         `grant=` is the verdict, both halves joined: `list=` is aterm's switch on the list \
         (its row in the system TCC.db, read-only: `on`, `off`, `absent`), and tccd's answer \
         below decides instead when it is newer than the switch's last change; `unknown` only \
         when neither was read. `list_probe=` says why `list=` reads as it does (`read`; \
         `no_full_disk_access` = the database opens only to an app with Full Disk Access; \
         `sqlite_failed`, `format_unrecognized`, `timed_out`, `spawn_failed`, \
         `output_unreadable`, `open_failed`; the `refused_*` labels). \
         `state=` is tccd's newest answer for `subject=` in its own log over the last \
         `window_s=` seconds (macOS asks it once per new program aterm starts). tccd answers \
         for the app a new program starts under, and its log names that app by bundle \
         id alone, so `state=` and the counts also take in answers for another copy carrying \
         aterm's id; how tccd answers a copy that does not meet the code requirement it holds \
         for aterm is unmeasured. A background `log show` reads it, and one `sqlite3 -readonly` \
         the switch; neither raises a dialog or writes anything. `unlisted` is tccd's \
         `Unknown (None)`, its \
         measured answer while aterm is not on the list. `requests=` counts tccd's \
         answers for aterm in the window and `not_allowed=` those that were not `allowed`: new \
         programs macOS checked before letting them start. `answer_age_ms=` is the age of \
         tccd's newest answer; `probe_age_ms=` the age of aterm's read. `unknown` is not \
         `unlisted`: see `probe=` (`pending` = not read yet, usually on a first read; \
         `no_recent_request`; `log_unavailable` = not an administrator account; `log_failed`, \
         `timed_out`, `spawn_failed`, `output_unreadable` = aterm could not read the log this \
         time; `refused_observer_off`; `format_unrecognized`). tccd's answer changes only \
         after the next new program starts; `list=` the moment the switch moves. The list has \
         no dialog of its own, and only a human changes it, in System Settings: `remediate \
         devtools=settings:Privacy_DevTools` names that place, never a command; `aterm doctor` \
         and the window open it for the owner, with aterm's panel beside it when an aterm \
         window shows it, holding aterm's icon to drag into the list (or press + below the \
         list and choose aterm, the way with no panel), and `devtools \
         hand-over` asks the window to; Settings \
         \u{25b8} Security's DEVELOPER TOOLS card says where the list stands. `spctl \
         developer-mode enable-terminal` adds only Terminal (it makes the pane appear on a \
         Mac whose list is empty); `tccutil`, \
         editing TCC.db and `sudo` are not the fix. Being listed should also skip macOS's first-run \
         malware scan for everything run in aterm, agents and downloaded tools included, so it \
         is the operator's decision. \
         `access_card` says where the Full Disk Access question stands, with no dialog of its \
         own: `state=<quiet|asked|opened|granted|declined>`, `asked_at=`, `opened_at=`, \
         `verified_at=` and `renew_at=` (unix seconds; `-` unset; `renew_at=` is a week after the \
         ask, the earliest the question may be raised again) and `needs=`, how many times macOS \
         showed the \"access data from other apps\" dialog for a program of this aterm (or \
         refused aterm a read) since this window started. aterm raises the question only after \
         such a need, at most once a week, and never to an owner who has already opened \
         Settings about a window older than that act. \
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
         domains, the media and photo libraries, App Management and Contacts, each its own \
         service with its own Settings pane that this grant does not reach. An adopted session inherits \
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
         null for never), then \"human_seq\":<n> (`status human_seq=`: how many person \
         gestures have reached it — the value `key|send if-human=` fences on), then \
         \"launch\" (`status launch=`, read under the same lock and judged against that \
         human_seq): {\"id\":<n>,\"seq\":<n>,\"word\":\"claude|codex\",\"why\":\"person\"} \
         while the person count is still the one the shell's last signed command start \
         recorded, {\"why\":\"<word>\"} for every other word, null for none, before \
         trimmed and first, which come last but for \
         `order=display`'s \"order\" and \"display_map\", which end the object",
    ),
    v(
        "screen",
        Read,
        Lines,
        Session,
        "screen [--json] [since=<etag>]: the full styled grid as one lossless JSON frame.",
        "The frame ends with \"etag\":\"<16 hex>\", a fingerprint of everything it is a \
         function of (the screen generation, every byte of output fed, the dimensions and \
         line sizes, the cursor, colours, palette and selection, the style policy a config \
         reload sets — bold as bright, faint opacity — and the viewport the images are laid \
         out against). `since=<etag>` sends that fingerprint back: while \
         none of it has moved the reply is `OK 1` then \
         {\"unchanged\":true,\"seq\":<n>,\"gen\":\"<epoch>.<seq>\",\"etag\":\"<etag>\"} \
         instead of the frame, and any other etag gets the full frame — a poll of a settled \
         screen costs no grid read. The frame is one line within the reply-line cap \
         (MAX_CONTROL_REPLY_LINE_BYTES, 8 MiB): past it the images are `truncated` (their \
         real `nbytes` kept; `image read` serves each), and a grid too big even so (about \
         35,000 cells) answers `ERR screen: frame too large (<n> bytes > <cap>)`. Any other \
         argument is `ERR usage: screen [--json] [since=<etag>]`",
    ),
    v(
        "line",
        Read,
        Status,
        Session,
        "line <n>: one physical line of scrollback+screen",
        "n is an absolute row in the same coordinates as search. ERR evicted means the row \
         is older than retained history; ERR unreadable means a retained row could not be \
         read; ERR out of range means the row is beyond the live screen's end.",
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
         back. `OK left=0`: the primary screen was already up and nothing changed. Under \
         Windows ConPTY the console host stays on the dead app's screen, and nothing a terminal \
         sends moves it: the primary rows are all in the scrollback, the screen starts blank \
         with the cursor where the console has it, and the next full-screen program's exit \
         shows the screen as it was when the killed app started, every row since kept in the \
         scrollback just above it. A console program that writes `ESC[?1049l` brings the \
         console back at once (pwsh: `[Console]::Write(\"$([char]27)[?1049l\")`). Anything \
         after the verb is `ERR usage: mainscreen`",
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
        "then keyed facts, read by key: session= cell_w= cell_h= font_px= scale= \
         window=<id|none> (the hosting window) window_rows= window_cols=, frame/surface sizes, \
         bands, crops, geometry= and present_retry_*; --json carries the same",
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
        "OK <title> (OSC 0/2, set by a program)",
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
    v(
        "colors",
        Read,
        Status,
        Session,
        "OK fg= bg= cursor= palette_sets= refused= program_palette= program_slots=",
        "- the live default colours, then what programs set over the theme: `palette_sets=allowed|\
         refused` (the session's OSC 4/21 gate), `refused=1` once it refused a palette set, \
         `program_palette=<idx>:<rrggbb>,…` (ascending; `-` for none) and `program_slots=` from \
         fg,bg,cursor,selection_bg,selection_fg (`-` for none). A program's colours live for the \
         session; `reset` puts them back.",
    ),
    // The live theme. The bare form and `theme list` READ; the transient preview
    // lane (`preview`/`revert`) is runtime-only like `rain`, so it is Write; the
    // forms that rewrite `aterm.toml` (`set`, `commit`) are ConfigWrite like
    // `settings set`. aterm-gui's `escalated_op` raises the class by sub-form.
    v(
        "theme",
        Read,
        Status,
        App,
        "theme [status] | theme list | theme palette [session]: the theme on screen, source and palette",
        "`OK id=\"<id>\" label=\"<label>\" configured=\"<raw>\"|- side=dark|light \
         source=builtin|file|default [path=\"<file>\"] reported=dark|light miss=\"<why>\"|- \
         os_appearance=dark|light preview=\"<id>\"|- [restore=\"<id>\" ttl_ms=<n>|-] \
         fg=#rrggbb bg= cursor=|- selection=|- selection_fg=|- … generate=on|off \
         generate_from=theme|config|default pinned=<n> palette=<16 #rrggbb, comma \
         separated>`: `miss` names every configured name the window cannot draw (Default is \
         drawn instead) and every selector issue. `generate` says whether slots 16-255 are \
         derived from the theme's colours (`palette_generate`) and who decided; `pinned` \
         counts the sessions drawing a theme of their own, which a preview does not reach. \
         `theme palette` answers `OK 256` and one `slot index=<n> rgb=#rrggbb \
         origin=theme|config|default|generated|xterm` row per palette slot; `theme palette \
         session` answers the aimed session's. `reported` is the theme's baseline for DSR \
         996 / mode 2031; a session whose program set its own background reports that \
         instead (`status` `reported_scheme=` `scheme_from=`). `theme list` answers `OK <n>` and one \
         `theme id= label= appearance= measured= pair= source= status= drawn= notes= \
         description=` row per built-in and themes-folder file, then one `folder path= kept= \
         seen= truncated=` row.",
    ),
    v(
        "theme preview",
        Write,
        Status,
        App,
        "theme preview <name> [ttl=<secs>] | theme revert: try a theme without writing aterm.toml",
        "The preview draws `<name>` (a built-in, a themes-folder id or a `dark:…,light:…` \
         selector) in every window and session, with the config's colour keys still applied; \
         nothing durable is written. It ends by itself after `ttl` seconds (default 120, at \
         most 3600), at `theme revert`, and at any config or themes-folder reload, and the \
         colours of the configured theme come back. A name that does not resolve is refused \
         and changes nothing. Answers the `theme` status line.",
    ),
    v(
        "theme session",
        Write,
        Status,
        Session,
        "theme session [status] | set <name> | clear: this session's own theme",
        "`OK session=<sid> theme=\"<selector>\"|- origin=ctl|spawn|split|restored|identity|instance \
         asked=<origin>|- id=\"<drawn id>\" source=builtin|file|default side=dark|light \
         reported=dark|light miss=\"<why>\"|- generate=on|off fg=#rrggbb bg=#rrggbb`. A session \
         draws its own theme, else its identity's `identity_themes` entry, else the instance's; \
         the config's colour keys still apply. It recolours the session's cells, background, \
         cursor, selection and palette and what DSR 996 / mode 2031 report to its program; the \
         window chrome follows the instance theme. A split from a themed pane inherits it; it \
         survives a seamless update and a restore; the instance preview does not reach it. \
         `set` refuses a name that does not draw (`ERR theme session \"<name>\": <why>`) and \
         changes nothing; one that stops drawing later keeps its selector and draws the \
         instance theme (`miss=`). Flagless it acts on the front terminal; `@<sid>` aims it. \
         `spawn theme=<name>` gives a newborn one.",
    ),
    v(
        "theme set",
        ConfigWrite,
        Status,
        App,
        "theme set <value> | theme commit: write `theme` to aterm.toml (commit: the preview's)",
        "Through the `settings set theme` lane. `OK saved: theme = <value>` carries every \
         name the running catalog cannot draw and every selector issue after `; ` (`theme: \
         \"<name>\" is unavailable (…); Default is used`), so a typo is never a silent OK. \
         `theme commit` with no preview standing is `ERR no theme preview to commit`.",
    ),
    v(
        "search",
        Read,
        Lines,
        Session,
        "search <pat> [case] [regex]: find in history; OK <n>[ incomplete…], \"<row> <col> <len>\" each",
        "(searches the newest search_history_lines lines, default 100000; incomplete is \
         followed by every cause that applies: capped: the list stopped at 100000 matches; \
         changed: the terminal changed while the search ran; \
         rewrapping: a resize holds the history away, only the screen was searched; \
         long-line: a regex ran out of scan budget on a very long line, so that line was \
         not searched to its end; older: older history was not searched either. incomplete \
         alone: older history was not searched. The header's screen_first=<row> is the absolute live-screen top \
         in the searched frame. A hit straddling a SOFT WRAP is one match whose col+len runs past the \
         grid width and continues at column 0 of the next row; regex ^ and $ bind to the reader's \
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
         <base64>`) instead of a server-local path — for a driver that cannot read the server's files. \
         image --meta = add the frame's pixel fingerprint, phase, theme and per-leaf identity: OK <w> <h> \
         image-meta <key=value…> path=\"<path>\" (with --bytes: OK 2, the image-meta row, then \
         the PNG row; `identity=unavailable` when the frame's provenance was not retained). image plain = bare pixels; image read = inline OSC-1337 images \
         as base64. @<sid> captures the app-render frame of the window showing that session. \
         The frame is the whole window (sibling panes, tab titles, overlays), so an edge token \
         is refused it (`ERR denied`); it reads its session with image read, text or screen",
    ),
    v(
        "window",
        Read,
        Status,
        App,
        "window [<target>] [path] : assemble a full-window artifact",
        concat!(
            "(platform-owned chrome + the exact submitted client destination) -> PNG, reply OK \
             <w> <h> <path> (target: ",
            window_targets!(),
            ", default front; ",
            open_aliases!(),
            "; a bare filename confined to images/, and in the target's \
         place it needs an extension: a bare word that is no target is `ERR unknown window \
         target '<word>' …`, never a file of that name). Every target but \
         front is photographed in the front window, so one it is not showing (a closed \
         overlay, Settings in a background tab, about or update while Settings shows another \
         page) is refused, never answered with a plain front frame: `ERR <target> is not open \
         in the front window; …` and the step that opens it: `open <target>`, or `invoke \
         ConnectToSession` for session-picker and `invoke ConfigureConnection` for conn-card \
         (the front session needs a connection). Compositor visibility and scanout are not \
         observed"
        ),
    ),
    v(
        "video",
        Read,
        Status,
        App,
        "record N seconds (0.5..=60) of the front window's WSI-SUBMITTED destination frames",
        "-> frame_NNNN.png + index.json (same-clock timestamps; compositor visibility and scanout are not observed). Flags: full | keys (owner-only keystroke log: hardware input, plus socket input aimed at the tab ON SCREEN — `key`, `ctrl`, `send`, `feed`, `paste`, flagless OR an explicit `@<sid>` naming the front tab — each stamped on the frame clock. A verb aimed at a BACKGROUND session egresses on the control thread and CANNOT be logged (`@self` expands to that when the driving session is not front), and input that lands on a WINDOW this take is not capturing (the front window changed mid-take — an `aterm ctl spawn` alone does it) has no frame here that could answer it; those attempts are COUNTED instead and reported as `unlogged_inputs=` on the reply line, live as `unlogged=` on `video status`, and in index.json meta (with the window share broken out as `unlogged_other_window`), so an empty inputs[] is never ambiguous and a logged row is never a key the recorded window never saw. Drive the FRONT tab when you need key->frame latency) | pace (keep redraws flowing) | fps=<n> (cap capture rate, 1..=120) | budget=<MiB> (frame-store RAM, 64..=4096, default 512). Every recording carries >=1 baseline keyframe; retention converges to 8 eligible completed recordings while preserving fresh/live handoffs. `video status` = one-line read of the in-flight recording (recording= mode= elapsed_ms= frames= resized= keys=, and for a keys take the RUNNING inputs= unlogged= so a driver learns mid-take that it is driving an unloggable path); `video stop` = finalize it now — owner-only, like `keys`, because it truncates the take. `video frames [count=N]` = no capture; list the newest recording's N highest-delta (most-changed) frames as `frame n= delta= t_us= seq= <path>` rows, so an AI pulls just the eventful key frames instead of every PNG (default 8, max 64). index.json meta reports honest coverage: head_truncated/evicted_frames/ring_skipped/covered_us vs requested_ms, plus keys_requested/inputs_logged/unlogged_inputs. key->captured-frame latency = first recorded submitted destination containing the glyph minus inputs[].t_us (an inputs[] row is `ch` for a character or `key` for a named key like ArrowUp/Escape — NOT key->photon and NOT comparable to a keystroke-sampled latency: the recorder can only see a key's effect on its NEXT captured frame, so at fps=N no reading goes below ~1000/N ms however fast the terminal is. index.json `analysis` publishes that floor with every number (per row capture_floor_ms/at_capture_floor; per take capture_floor_p50_ms, capture_interval_p50_ms, attempts_outpace_readings, capture_verdict) — use `metrics percentiles` input_p50/p95/p99_ms when you want typing latency); cadence gaps = frames[].t_us deltas vs ~16667, and a paced or headless present-real take also publishes ticks[] (every instant the loop's redraw PROVED the screen unchanged) so an unchanged screen's gaps read as quiet, not as missed samples, while a tick that neither presented nor proved it (a stall) stays a hole",
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
         state=<held|live|review|folded|stale|unseen|superseded|resolved-ok|resolved-warn|dismissed|answered|evicted|carried|withdrawn|recorded|quit> \
         glass=<row|-> rep=<n> key=<k|-> title=<t> detail=<d> actions=<a|-> \
         [progress=<n>/100|busy=1] [load=<network|disk|cpu>] \
         [since_ms=<ms>] [need=<now|working|notice|later>]`. A bare <n> keeps the newest n (default 64, max 512); since=<id> keeps the OLDEST n above the id \
         (default all), so paging with the last id you saw never skips a row; tag= filters; sev= is a floor (sev=warn is warn and error); \
         need=later keeps the exceptions to review, open (state=review) and filed; \
         live keeps the unretired rows that are on or queued for the band (an open exception is \
         not one: ask need=later live). at= is the wall clock at ingress; since_ms= is on rows \
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
        "notice <post|progress|done|ask|dismiss|act> \u{2026}: record, show work, ask, end or press a message",
        "Owner-only. `notice post <tag> [sev=<sev>] [key=<key>] [hold=<1..120>] <title>[ -- \
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
         <title>`, which takes the row's place. `notice ask <key> [tag=<tag>] <title> -- \
         <reason> -- <step>` is a standing ask: the owner is needed now for a reason only \
         the owner can close (the reason says why, at most 90 characters, never that \
         something is pending; the step is the one action, at most 90, never a snooze or \
         two), shown until `notice done <key> ok` or read, then not raised again for a \
         day (`OK message=<id>`). `notice dismiss <id>` takes a row down (`notice dismiss review` files every open exception into the log); `notice \
         act <id> <label|index|details>` presses its button as a person would (`OK \
         acted=<label> performed=<1|0>`). A band title is at most 6 words and 48 characters with \
         no clause; a key is 1-40 of a-z 0-9 . _ - and lives as wire.<key>; a tag is `script` \
         or one of your own, since aterm's own (crash, config, update, toolchain, packages, \
         privacy, session, window, render, a11y, fabric, harness, system) are refused; at most \
         3 wire rows are live (the band's \
         three), 60 new messages and 16 KiB of words a minute, 10 presses a minute (`ERR busy \
         notice: \u{2026} retry_ms=<ms>`); while aterm is switching to a new version every \
         notice is `ERR busy notice: aterm is switching to its new version retry_ms=1000`, and \
         the retry reaches the new version. No button can be authored from the socket.",
    ),
    // `story` is the WRITE face of the PRESENCE band: a watcher's decision, told to
    // the session's window. Owner-only (a child edge must not write `✓ approved` onto
    // a band it does not drive), Write op-class because it mutates App state, Session
    // target because it names the session it is about. The verb set does not fit the
    // summary cap, so the detail leads with it.
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
    // `devtools hand-over` asks the window to bring the owner to System Settings ▸
    // Privacy & Security ▸ Developer Tools (owner ask 2026-10-15): `aterm doctor`'s way
    // to the window's helper panel. Owner-only, Write op-class because it can put
    // System Settings in front of the owner; Owner is what every in-session client
    // holds, so the window holds the ask to its own hand-over's rules, and the help
    // says so.
    va(
        "devtools",
        Write,
        Status,
        App,
        OwnerOnly,
        "devtools hand-over [now]: ask the window to bring the owner to the Developer Tools list",
        "Owner-only, and Owner is what every in-session client holds, agents and fabric tasks \
         included, so the window holds every ask to its own rules. It is about macOS's \
         Privacy & Security \u{25b8} Developer Tools list (whether macOS checks each new \
         program aterm starts before it runs: `privacy`'s `devtools` row), not debugger \
         access. It never puts aterm on the list or turns a switch on: only the owner adds \
         aterm there, in System Settings, and macOS asks for their password. On a Mac whose \
         list aterm read whole and empty, the page is brought up after `spctl developer-mode \
         enable-terminal`, which adds Terminal, switched off, so that the page appears. The \
         bare form waits for a quiet moment (an aterm window focused, the owner at the \
         keyboard or mouse within 5 minutes but not for 10 s, aterm running at least 5 s after \
         the owner opened it (90 s after an update or a relaunch), no folder warm-up asking, \
         no update switching versions), then opens the page with aterm's helper panel beside it; \
         one with no such moment within 30 minutes lapses (`OK queued`). `now`, for a person \
         at `aterm doctor`'s terminal, skips the quiet moment: the window shows its panel and \
         the asker opens the page itself (`OK shown`; `OK unshown` when no panel could be \
         shown). Otherwise nothing is done, and the answer says why: `OK granted` (aterm is \
         on the list and switched on), `OK unknown` (aterm read neither the list nor tccd's \
         answers, so it leaves the list alone), `OK declined` (the owner said Not now to the \
         panel; that holds 30 days, 90 on a repeat), `OK unstable` (an ad hoc or unsigned build, \
         whose grant the next build would lose; the bare form only, since `now` has a person \
         at the terminal), `OK listed` (aterm's row reads on while macOS still checks its \
         new programs, so the page holds nothing to add; the bare form only) or `OK recent` \
         (aterm opened the page within 10 minutes; for `now`, \
         within a minute, or the window answered a `now` within a minute, whether or not its \
         asker opened the page, or aterm's Full Disk Access page waits for its quiet moment \
         or its panel is up, since that page goes first). A headless instance answers `ERR headless: \u{2026}`, a \
         window off macOS `ERR unsupported: \u{2026}`, and any other form `ERR usage: \
         devtools hand-over [now]`. The owner's own presses (Settings \u{25b8} Security's \
         Open Developer Tools\u{2026}, the digest line's button) open the page whatever these \
         say.",
    ),
    // The OS light/dark appearance the instance follows, and (development
    // builds only) a flip injected through the same event the platform
    // observer posts. `Write` for the injection: it re-themes every session.
    v(
        "appearance",
        Write,
        Status,
        App,
        "appearance [dark|light]: the OS appearance the theme follows",
        "Answers `os_appearance=<dark|light> source=<nsapp|desktop|window|inert|injected> \
         theme_side=<dark|light|-> reported_scheme=<dark|light> bg_dark=<0|1> theme=<id> \
         installs=<n>`: the desktop appearance last read (`source` says from where: the \
         application's appearance on macOS, the XDG portal or gsettings on Linux, the window \
         theme on Windows, nothing on a headless instance, a development build's \
         `$ATERM_OS_APPEARANCE`), the side it picked of a `theme` that follows it (a \
         `dark:…,light:…` pair; `-` for a single theme, or none), what DSR 996 and mode 2031 report for the configured palette (a \
         session whose program set its own background reports that, see `status` \
         `scheme_from=`), whether the theme's background is dark, the theme drawn, and how many installs re-coloured every session since \
         launch. `appearance dark|light` injects an OS flip and answers the same line after \
         it lands; it needs a development build (`dev-seams`) and is refused otherwise. Each \
         session's `status` carries the same fields for that session, plus `scheme_from=`.",
    ),
    // The crash-restore journal's memory shed (`aterm-gui`'s `crash_restore::ShedPolicy`).
    // `Write` for the injection: it sheds scrollback and history rings.
    v(
        "pressure",
        Write,
        Status,
        App,
        "pressure [normal|warn|critical]: memory pressure and the crash-restore journal's shed",
        "Answers `level=<-|normal|warn|critical> journals=<n> rings_shed=<n> \
         journal_resident_bytes=<n> rings_back_in_ms=<-|n>`: the memory-pressure level the OS \
         last reported (`-`: none since launch), how many sessions journal a crash-restore \
         region and how many of their history rings memory pressure took away (the largest \
         under `warn` until half is gone, every one under `critical`; a crash of a shed session \
         restores its screen exactly and counts the scrollback above the keyframe's few hundred \
         lines lost), the RAM every region costs this window, and in how many milliseconds the \
         next shed ring comes back (a minute after the pressure eased, then one a second). Each \
         session's `status` says `journal_ring=<live|shed>` and `journal_bytes=`, and `metrics` \
         carries `journal_resident_bytes=`. `pressure normal|warn|critical` injects a level as \
         macOS's memory-pressure source reports one (the scrollback is shed too) and answers \
         the same line once the journal applied it; it needs a development build \
         (`dev-seams`) on macOS and is refused otherwise.",
    ),
    // The `sudo` askpass relay (`docs/DESIGN-sudo-askpass-2026-10-15.md`). The
    // `aterm-askpass` helper a no-terminal `sudo` runs sends this; the window
    // raises a password sheet on the asking session's tab and answers once with
    // the secret. Intercepted in `serve_request_line` so it can read the peer
    // and gate on `Scope::Owner` EXACTLY (not owner-class), and so a held
    // session is refused; the table row is for the catalog and the scope test.
    va(
        "askpass",
        Owner,
        Status,
        Meta,
        OwnerOnly,
        "askpass: a no-tty sudo in a session asks the owner for the password on that tab",
        "The `SUDO_ASKPASS` helper aterm points every macOS session at (the \
         `aterm-askpass` argv0 alias) sends this when `sudo` has no terminal, as \
         in Claude Code's shell tool. aterm answers only when the kernel shows \
         the asking process is a child of Apple's `/usr/bin/sudo`, raises a \
         native password sheet on that session's own tab, and returns the \
         password `sudo` demands once — it is never stored, logged or shown to \
         the agent. Owner-exact (an agent holding the Owner token cannot forge a \
         human's password; a child edge, a bridge and a held session are \
         refused), macOS only, and `ERR askpass headless` with no window. Not a \
         verb for a person to type; `aterm help sudo` is the contract.",
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
         (role, phase and since, hand, mail when there is any, `ctx <n>% left`, fabric — two \
         spaces between slots; `\"\"` when the row is folded) and the sentence a screen reader \
         gets — the row as DRAWN: one the window holds back prints `\"\"` and ends the line \
         ` held=<driver|handoff|height>` (a driver's lease or open turn on the session, a \
         handoff's freeze, a window one row tall). Both quoted values escape `\"` and \
         `\\`. The band never carries command text, a mail body, an OSC title or a limit \
         message, so neither does this line. Off macOS there is no menu bar: the `menu \
         \"<title>\"` lines are the app's menu model, less the rows this platform cannot act \
         on (the two update rows where there is no updater); `controls menu` is the live \
         palette, with each row's enabled state. A macOS `--headless` instance has no menu \
         bar either and prints `menu (none)`: reading the bar would create the application \
         object a headless process never builds; `controls menu` still serves the menu \
         model. On Windows a `strip-version shown=<yes|no> …` \
         line follows the tab-model line: the version label at the right end of the tab strip \
         (the macOS Version menu's title without its colour emoji, ` ↑` while an update is \
         staged), the strip `cols=` that `pointer move -1 <col>` takes and the `action=` a \
         click fires, or `reason=no-strip|not-laid-out|no-ui-face|strip-full` when it is not \
         shown (`not-laid-out`: no frame has laid the strip out yet — a headless instance \
         lays it out when it composes one, e.g. for `image`). The \
         line before the presence line is `statusitem title=<❯|❯•|❯⚠> exceptions=<n> \
         warnings=<n>`: the menu-bar item's mark (`❯•` is something to look at and nothing to do \
         now), readable without a menu bar. Read-only.",
    ),
    v(
        "controls",
        Read,
        Lines,
        App,
        "GUI controls as text (the `window` targets, default front)",
        concat!(
            "Targets: ",
            window_targets!(),
            " — the roster the verb's own usage error enumerates. `controls autopilot-menu` lists the open Autopilot menu's rows by their \
         stable keys (`autopilot-menu/mode/<word>`, `autopilot-menu/land-in/<minutes>`, \
         `autopilot-menu/land-at`, `autopilot-menu/land-cancel`, `autopilot-menu/settings`); \
         a place a row left while it was open is `autopilot-menu gap index=<i>`, blank and \
         inert, so the rows after it keep their frame rows. `controls connections` is \
         Owner-escalated; the rest answer at any scope. Off macOS `front` \
         ends with `confirm open=<true|false>`: a close or quit waiting on an answer in the \
         window, `by=` who asked. When a control client asked, `answer=\"confirm yes\"` goes \
         ahead and `cancel=\"confirm no\"` keeps it; when a person asked, both are `-` and \
         the person answers it."
        ),
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
        "asciicast v2 recording (compact, sendable); `cast drift` audits it against the screen;",
        "`cast frames [count=N]` expands it to a keyframe flipbook. `cast drift [max_runs=<k>] \
         [rows=<n>] [seed=auto|alt|primary]` replays it through fresh engines against the live \
         screen and names a resize run the app never repainted after (an alt-screen net-zero \
         flap shifts every row up one): `OK <n> verdict=<clean|desync|unfaithful|unknown|empty> \
         fidelity=<pass|fail|-> against=<screen|replay> drift_rows=<d> runs=<total> \
         candidates=<c> capped=<k> culprit=<t,…|-> anchor=<t|-> \
         seed=<alt|primary|ambiguous> seed_basis=<arg|toggle|live|fidelity|default> \
         geom=<CxR> evicted=<n> dropped=<n|-> cut=<quiescent|racy|file> folds=<f> ms=<ms>[ \
         undo=off][ computed=client]`, then a `run t=<t> n=<k> gap_ms=<ms> geom=<CxR>><CxR>>… \
         net=<zero|CxR> first_out_ms=<ms|-> clear_ms=<ms|-> culprit=<0|1>` line per LISTED \
         candidate, a `row <r> live=<pct> expected=<pct>` line per wrong row (`replayed=` when \
         the recording cannot reproduce the screen) and one `cursor` line; parse by key. A \
         candidate is a run of 2+ resizes at most 250 ms apart, output between them included, \
         after the `anchor` (the last full clear), or a RETURN: an alternate-screen resize whose \
         moved rows no grow handed back and no such run holds, through the resize that brings \
         the size back, however long after (a flap time split); the verdict weighs EVERY \
         candidate. The \
         culprit is the candidate the faithful replay's engine journal saw demote rows on the \
         alternate screen; only when it names none are the newest `max_runs` (default 8, at \
         most 16, 0 = none) varied one at a time. `candidates=` counts them all, `capped=` \
         those not listed (a culprit is always listed). `run t=`/`anchor=` are the cast's own \
         seconds, not the `resizes`/`timeline` clock. `fidelity=pass`: the faithful replay IS \
         the live screen; `unfaithful` names no culprit. A screen that kept moving (`cut=racy`) \
         is answered with the replays against each other (`against=replay fidelity=-`); with \
         no screen and no alt-screen toggle to seed from, `unknown seed=ambiguous` when the two \
         seeds disagree (`seed=alt` for a recording that starts mid-app). At most two analyses \
         fold at once per instance (`ERR busy` after 10 s). The one `aterm` binary computes it \
         client-side against a server without it (`computed=client`), and offline: \
         `aterm ctl cast drift --file <cast> [--screen <text>] [--until <t>] [seed=…]`. `undo=off`: the replays dropped the engine's resize \
         undo (a flap with nothing output between its halves moves nothing), as \
         an older aterm did — always against a server without the verb, which predates the \
         undo too, and offline only when that alone reproduces the `--screen`",
    ),
    v(
        "temporal",
        Read,
        Bytes,
        Session,
        "temporal [status|<tick>] [trim]: the screen reconstructed at a past instant",
        "(needs temporal_recording=true; `temporal status` reports the reachable tick range, \
         `lost_resizes=` (resizes the spine will never hold) and `pending_resizes=` (resizes \
         not on it yet: they reach it at the program's next output, and a replay at the \
         spine's end adds ` pending_resizes=<n>` to its header meanwhile). trim drops the \
         trailing all-blank rows — byte-framed, so `OK <nbytes> trimmed=<k>` counts the \
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
         seq are the old instance's); a ledger it could not carry whole adds unheld_below=<id> \
         to the OK header when that floor is above since=, until the turns it lost would have \
         aged out of the ledger anyway (records below it may be gone)",
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
        "attention_owner= attention_owners= supervisor= supervisor_proven= questions= agent_cwd= \
         autopilot= land_at= (pct-encoded; '-' = unset; read fields by key). `meta set \
         <title|description|icon|role|attention|questions|autopilot> <text...>` / `meta unset \
         <field>` set or clear the USER metadata (write-gated; user title outranks \
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
         holding one. A seamless update carries every owner's entry (the server's own stall \
         entry is derived again); a restore carries only the bare one — a keyed owner \
         re-asserts its own. SUPERVISOR: `meta set supervisor <holder> [ttl=<ms>]` / `meta unset \
         supervisor [holder=<holder>]` (Owner-only; an edge gets `ERR denied`) shows a running supervisor as \
         `supervisor=` in `meta`, `status` and `sessions`. Without ttl= the claim lasts while \
         the connection that set it keeps serving requests (it closes, or turns into a \
         subscribe stream: cleared); with ttl=<1..600000> it is a lease that lapses unless \
         re-set — the spelling for one-request-per-connection clients; at the lapse the claim \
         is removed with a `meta-change field=supervisor value=-` event and a box it was \
         holding reaches the menu bar (and a desktop notification with desktop_alerts = true). While a claim is live the agent's \
         own prompts, questions and walls (every `wall:<kind>`) raise no menu row or \
         notification: the supervisor answers them or escalates with `meta set attention \
         owner=<k>`. Another holder's live \
         claim is `ERR busy supervisor=<holder>`; the same holder renews. A seamless update \
         carries a ttl= claim with its remaining lease, which its holder renews on the new \
         instance; a connection's claim ends with the old instance and its holder claims again. \
         The new instance's own supervisor holds off a session it cannot vouch for (a \
         connection's claim, or any from an older build) for two renewal steps and a margin \
         (45 s) after the update, so that holder claims it first. A bare unset clears \
         whichever holder's claim stands; `holder=<holder>` clears it only while that holder \
         holds it (`OK` either way), so a supervisor giving its own claim back never clears \
         another's. A PROVEN SELF-CLAIM (`supervisor_proven=1`) is `codex-alab@<pid>` set on a \
         direct local connection whose kernel peer pid is <pid> while <pid> leads the session's \
         foreground group: it replaces the window harness's claim and a proven claim whose \
         process is gone, is renewed only by a proven request, is never carried across an \
         update, and is cleared when its process leaves the foreground or exits; the \
         spelling alone proves nothing. HARNESS_ACT: `meta set harness_act <act>` (<act>: one token like an owner \
         key) records an act the holder of the session's live PROVEN self-claim took (`status \
         supervisor_proven=1`): only that holder, on the claim's own connection or one the \
         kernel proves is its process again, may (anyone else: `ERR denied`); it is one \
         `harness-act` row on the `timeline` (`act=<act> holder=<holder>`) and `status \
         last_act=<act>`. QUESTIONS: `meta set questions ask|recommended|long-term|short-term` / \
         `meta unset questions` (Owner-only; an edge gets `ERR denied`) set how the in-window \
         supervisor treats this session's questions, over the `[harness] answer_questions` \
         switch and `question_priority`: `ask` leaves its Claude Code question DIALOG to a \
         person (how a controlling session takes a worker's questions; a question it asks in \
         prose, which no `drive answer` can answer, stays the switch's), `recommended` answers \
         the dialog with its recommended option and a question in prose with `answer_text`, \
         `long-term` and `short-term` have the worker decide by that priority's fixed policy \
         text, in the dialog and in prose. A closed set, case-folded and stored lowercase — any \
         other value is `ERR usage: meta set questions ask|recommended|long-term|short-term`; \
         `questions=-` is unset (the switch decides); it counts toward `sessions meta=1` and \
         survives a restore and an update's handoff. AGENT_CWD: the foreground program's own \
         working directory as the OS reports it (`-` with no foreground program or none \
         readable) — where an agent was launched, which a shell with no OSC 7 leaves `cwd=` \
         without. AUTOPILOT: `meta set autopilot cruise [until=…]|land|off` / `meta unset \
         autopilot` \
         (Owner-only; an edge gets `ERR denied`) set how far the window's harness carries this \
         session's work, over the `[harness] autopilot` default: `cruise` keeps it moving until \
         the agent's clear final statement, `land` wraps it up cleanly (the land instruction \
         once, at most two nudges, then nothing typed but the land answer to a question asked \
         in prose, which has no such bound; a question dialog is answered as under `cruise`), \
         `off` keeps the harness's hands off — its supervisor stops and starts no more, its \
         boxes reach the menu bar, an exit is never relaunched. The owner's words \
         `forever`/`infinity` (cruise) and \
         `wrap-up`/`wrapup`/`wrap` (land) are admitted; any case; stored as the mode's word — \
         any other value is `ERR usage: meta set autopilot cruise \
         [until=HH:MM|+DURATION]|land|off`. A TIMED CRUISE: `meta set autopilot cruise \
         until=<when>` cruises until a time, then lands — `<when>` is a local `HH:MM` (the \
         earliest instant after now that reads it, today else tomorrow; on a night the clocks go \
         back, the earlier reading still ahead; a time the clocks skip is refused with its date) \
         or `+<n>h`, `+<n>m`, `+<n>h<n>m` (1 minute to 24 hours). The reply names the instant: \
         `OK land_at=<unix s> local=<date>T<HH:MM><offset>`. Only `cruise` takes `until=` \
         (`ERR until= goes with cruise only: usage: …`), and any write of the word without it, \
         the same word included, and `meta unset autopilot` cancel the timer; at the time the \
         window writes `land` only if the stored word is still that timed `cruise`, so a hand \
         write always wins. `autopilot=` is \
         printed on EVERY bare `meta`, `-` while unset (the default decides), and after it \
         `land_at=`, the instant a timed Cruise lands (unix seconds; `-` while none); the master \
         switch (`[harness] enabled = false`) wins over the word. It counts toward `sessions \
         meta=1` and survives a restore and an update's handoff; the footer's Autopilot chip \
         and Settings > Harness write the same word. No `window=` here, \
         for the reason `status` gives: `meta` is polled and the window lives on the main thread \
         — ask `sessions`/`ls` (one hop for the whole fleet) or `dims`",
    ),
    // `harness policy` is the session's `[harness]` policy as the window's one
    // reader holds it, for an agent that answers for itself (codex-alab, the
    // native harness contract §3). Read-class and Scoped: an edge reads only
    // `effective=`.
    v(
        "harness",
        Read,
        Status,
        Session,
        "harness policy -> OK gen= effective=<on|off> why= <every [harness] key>= questions=",
        "The session's `[harness]` policy, as the window's one reader of aterm.toml holds it, for \
         an agent that supervises itself (a proven self-claim, see `meta`) and so never parses the \
         file. `gen=` is the policy's generation (`0`: the launch read); `subscribe … events` \
         pushes `EVENT <local> harness-policy gen=<n>` when it moves. `effective=` and `why=` are \
         aterm's verdict for THIS session, first refusal first: `unhosted` (no control socket), \
         `disabled` (`enabled = false`), `headless` (a headless instance under `headless = \
         false`), `agent-off` (the agent's own `[harness.<agent>]` table is off), `suspended` (the \
         window's harness is handed off for a seamless update), `hold` (a standing halt), \
         `autopilot-off` (the session's Autopilot is `off`); `enabled` when it is on. Then every \
         `[harness]` key in the reader's own spelling and order (`aterm help harness`), \
         pct-encoded, `-` for an empty value, with `continue_text=` already composed with the \
         `rules_file` text as the window's own continuation is, and last `questions=<ask|\
         recommended|long-term|short-term>`, resolved: the session's own `meta questions` word, \
         else `ask` under `answer_questions = false`, else `question_priority`. An edge token \
         reads only `OK effective=<on|off>`. Read-only. Any other form is `ERR usage: harness \
         policy`.",
    ),
    // `status` is the READ-ONLY Subject+Status record (RFC: Tab Subject &
    // Status §8) — what a session IS and what it is DOING, classified entirely
    // locally. Versioned so any consumer (a script, an agent, the harness) can
    // reject a schema it does not know; there is no write sub-form, so it takes
    // no `escalated_op` entry.
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
         hold_advisory=<fabric-lost|-> \
         fabric=<connected|stale|stalled|disconnected|absent> fabric_rtt_ms=<n|-> \
         fabric_link_age_ms=<n|-> identity=<name|-> \
         hand=<-|turn:<id>[:<holder>]|lease:<holder>|driving:<sid>> hand_pid=<pid|-> \
         level=<quiet|note|story|driving|driven|attention|limited|hold> story=<n> \
         why=<-|prompt,question,escalation,mail> path=<frozen|live> program=<name|-> \
         agent=<busy|prompt|question|wall:<kind>|idle|survey|unknown|-> agent_detail=<pct|-> \
         agent_rev=<n> \
         agent_since_ms=<ms> agent_gen=<e.s|-> agent_fp=<hex16|-> \
         os_appearance=<dark|light> theme_side=<dark|light|-> reported_scheme=<dark|light|-> \
         scheme_from=<theme|program|-> bg_dark=<0|1|-> \
         program_state=<idle|working|blocked|done|error[:<kind>]|-> last_act=<act|-> \
         [conpty=<openconsole|inbox|->] integration=<on|off|degraded|-> \
         input=<-|clear|pending|typeahead|stalled|stopped> input_bytes=<n|-> \
         input_wait_ms=<ms|-> fg_rss_mb=<n|-> supervisor=<pct|-> supervisor_proven=<0|1> \
         autopilot=<off|cruise|land|-> \
         human_ms=<ms|-> human_seq=<n> \
         launch=<person|since|typed|boxed|other|pending|none> launch_seq=<n|-> \
         path_evidence=<measured:<ms>|unconfirmed:<ms>|carried|-> copy=<managed|foreign|-> \
         upgrade=<-|<state>/<to>/<why>/<age>> history_lost=<n> \
         integration_rev=<current|stale:<hex16>|frozen|-> \
         born=<cold|spare-parked|spare-warming|spare-ready> [spare_age_ms=<ms>] \
         render=<ok|displaced|desync-risk|unverified|unjudged|-> resizes=<n> flaps=<n> \
         land_at=<unix-s|-> task=<open|landed|done|spent|theirs|-> \
         log_seq=<n> log_ep=<e> run=<r<n>|-> turn=<t<n>|-> \
         step=<p<n>|-> need=<word|-> need_since_ms=<ms|-> gen=<e.s|-> seq=<n|-> hash=<hex16|->. \
         `log_seq=`/`log_ep=` are the session log's head and epoch as a reader is shown them \
         (`log since=<log_seq> ep=<log_ep>` and `await log … since= ep=` read what comes after), \
         `run=`/`turn=`/`step=` its open path, `need=` the most urgent need it OBSERVED \
         (`approval:<kind>`, `question`, `wall:<kind>`, `unresponsive`, `failure`, `escalated`, \
         `exited`; a need a `mark` claimed is `brief`'s `claim need`, never this) and \
         `need_since_ms=` its age; `-` when none. \
         `render=` is the resize ledger's verdict (see `resizes`): \
         `displaced` while a resize has moved alt-screen content and the engine still owes the \
         rows (until a full clear or screen switch repaints it, an undo hands the rows back, or \
         the size comes back and the epoch is judged), `desync-risk` once, 1 s on, the app has drawn over a \
         net-zero flap's displaced content without clearing (between its halves or after), \
         `unverified` when it drew over a net-changed run's with no clear seen (repainted or \
         not; `cast drift` decides, a size that later came back, however long after, weighed \
         as a flap), `ok` for a flap the engine's resize undo restored, and `unjudged` when \
         the ENGINE owes rows (its render debt) with the geometry back and 1 s passed but the \
         program has not shown it clears its screen when resized, so a miss cannot be told \
         from a repaint without a clear (never pushed as an event; Claude Code, and a session \
         a full clear healed within 1 s of a resize, read `desync-risk` instead) — `-` \
         when the terminal lock was contended; `resizes=`/`flaps=` count the session's grid \
         resizes and net-zero flap runs. `conpty=` (Windows only) is the console host the \
         session runs in: `openconsole` (an OpenConsole.exe) or `inbox` (the Windows console \
         host), `-` with no pseudoconsole behind it; aterm.toml `conpty` opts into \
         OpenConsole and `aterm doctor` says why a tab fell back. `history_lost=` is how many scrollback lines this \
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
         contended: `subject=-`, never a guess from the cwd. Fields are ADDITIVE and never bump the \
         schema, so reject an unknown schema MAJOR rather than best-effort parsing, and treat an \
         unknown phase/outcome/reason token as unknown rather than an error. `enabled=false` \
         means `tab_status` is off and every phase will read unknown. `detail=` is the \
         sanitized RUNNING command — the program reduced to its basename (a quoted path with \
         a space is one word; a Windows `.exe`/`.cmd`/`.bat`/`.com` is dropped, so \
         `& 'C:\\Program Files\\Git\\usr\\bin\\less.exe' x` reads `less`), plus an \
         allow-listed subcommand, never an argument (`claude`, `codex`, `targo%20test`); a \
         COMPOUND line reads the segment that RUNS — the last of an `&&`/`;` chain, the \
         first of an `||` chain, one wrapper (`sudo`, `env`, `exec`, ...) unwrapped — so \
         `cd ~/proj && exec claude` reads `detail=claude`; a line that OPENS with a shell \
         keyword is not parsed and reads that keyword, so `for i in 1 2 3; do ...; done` \
         reads `detail=for`, and a keyword opening a LATER segment reads the same way \
         (`cd x && for ...; done` is `for`, never its closer `done`) — while the \
         shell-integration block executes, `-` otherwise or \
         when the terminal lock was contended: how an agent tells that a peer is another \
         agent before typing into it. \
         `hold=1` means a drive halt stands on this session, local, fleet or both: every PTY-reaching \
         verb answers `ERR halted reason=<r> origin=<local|fleet>` (the fleet's when both stand; \
         the reason is on that line and on the `hold` record in `events`, whose `hold_origin=` \
         lists every origin standing); an `origin=local` hold is the owner's own and \
         `hold <sid> off` lifts it, a fleet hold only the bridge lifts, and the session stays \
         held until both have. \
         `fabric=` is INSTANCE state, not this session's, and it is the BRIDGE'S BROKER LINK, not \
         the bridge process: `absent` = \
         no bridge is attached (none configured, or the first is still starting; a waiting post \
         answers `queued=1` or `no-bridge=1` accordingly); `connected` = a bridge is attached \
         AND its last exchange with the broker was acknowledged; `stalled` = a bridge is attached but its link is down \
         (the dial failed — no socket, connection refused — the broker closed the connection, or \
         an ack did not arrive within the bridge's 5 s ack deadline; the bridge redials with \
         back-off from 100 ms to 5 s, mail queues and none arrives meanwhile, and `post --wait` \
         answers `ERR fabric stalled id=<n> queued=1` at once); `stale` = a bridge is attached, \
         has NOT said its link is down, and has answered nothing for work this instance handed \
         it (no ack, no landing and no `link` record for three link refreshes after a post was \
         queued) — the wedged-HELPER case, which `stalled` cannot see because a hung helper \
         holds both its lanes open and reports nothing; its posts queue and `post --wait` \
         answers `ERR fabric stale id=<n> queued=1`; kill the bridge (its pid is on `aterm \
         fabric`'s BRIDGES row) and aterm relaunches it; `disconnected` = the bridge this \
         instance had is gone: the sessions it governed keep running, each with \
         `hold_advisory=fabric-lost` until a bridge attaches again (a hold that stood stays; \
         `[fabric] hold_on_bridge_lost = true` halts them instead). NO HEARTBEAT ON A QUIET FLEET keeps \
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
         `driving:<sid>` this session's own open turn into another, `-` nobody. `hand_pid=` is \
         the kernel's pid of a live drive lease's taker (`lease status pid=`; `-` for no hand, \
         a `turn`, a lapsed lease, or a taker no kernel named): a `lease:aterm-harness@<pid>` \
         is aterm's own hand only when `hand_pid=` is that aterm's pid, since any client can \
         spell the name. `level=` is the band's severity, ascending \
         `quiet < note < story < driving < driven < attention < limited < hold` (the rim is \
         teal from `driven`, amber at `attention`, red at `limited` (an agent at a \
         wall nobody answers: one the in-window harness resumes by itself reads `note`) and `hold`; `story` is the \
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
         when the foreground program is an identified agent (`claude`, `codex`, `agy`, `muse`; \
         a `codex` is named and never read: aterm reads no Codex screen, so its verdict is \
         `unknown`) or the screen shows one of their composers or boxes (Gemini CLI's too: it \
         runs as `node`, and only its screen names it), and re-read \
         whenever the content or the terminal's cursor moved and the live zone (the last 40 \
         rows of the screen's content, a run of 4 or more blank rows counting as one) or the \
         cursor's row changed, at most 4 Hz \
         — never gated on `revision=`; anything else reads `-` (a shell whose last line ends in \
         `?` is not a question). `wall:<kind>` is the WALL the last turn ended on, one of \
         `usage-session`, `usage-weekly`, `model-bucket`, `spend`, `context`, `auth`, \
         `api-error`, `overloaded` (a 529 reads `wall:overloaded`, never `idle`), `memory` \
         (Claude Code's critical-memory banner, read by its place right above the composer's \
         frame, never from a quote of it: restart it, then resume its own conversation with \
         `claude --resume <id>`, never the directory's newest), or \
         the SERVER'S OWN `unresponsive` — the program has stopped reading its input (see \
         `input=` below), whatever its screen still shows, with `agent_detail=stopped` for a \
         stopped job; it is published and cleared by aterm in step with `input=`, and the \
         screen's verdict returns when the program reads again; `unknown` is \
         an identified agent whose reader has no evidence for a phase (every Codex screen; a \
         Claude Code screen with no prompt box that holds the terminal's \
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
         pager showing a captured screen. `reported_scheme=` is what this session's DSR 996 \
         and mode 2031 report: the theme's appearance while the background is the theme's \
         (`scheme_from=theme`), the polarity of the background its program set with OSC 11 \
         while that holds (`scheme_from=program`); `bg_dark=` is the live background's polarity \
         (`appearance` explains `os_appearance=` and `theme_side=`). \
         `program_state=` is what a `codex` in front says \
         about itself (OSC 7501 `state=<word>:app=codex[:kind=permission|question|auth]`; a \
         `blocked` report's kind after a colon), shown only while the program in front names \
         `codex` or is a runtime that hosts one (npm's `node`, `bun`) and only since the \
         foreground group last moved, cleared by `state=clear`, a \
         full reset, a foreground change and the program's exit: a claim for display, never \
         `agent=`, `agent_since_ms=` or anything an `await` or a fenced press reads. \
         `last_act=` is the last act an agent that supervises itself recorded (`meta set \
         harness_act`). \
         `integration=` is whether this \
         session's OSC 133/633 marks reach aterm: `on`, `off` (not required), or `degraded` — \
         a nonce is required and none is in use (an adopted shell — a seamless update or \
         a keeper crash recovery — that carried none, or one re-keyed through its channel \
         that has not reached its next prompt yet), so `detail=` and blocks stay dark while `program=` still names it; `-` \
         when the terminal lock was contended. `integration_rev=` (after `history_lost=`, \
         before `born=`) is WHICH shell integration the session's shell runs, against the \
         one this build ships: `current` (its last SIGNED `633;P;AtermIntegration=<rev>` — \
         the script folder its integration body came from — is this build's), \
         `stale:<rev>` (another build's: a shell adopted across an update runs the old \
         build's until its next prompt, when it takes this build's body through its \
         pointer and the word reads `current`), `frozen` (adopted with an integration from \
         before loaders, fixed at spawn: no pointer reaches it, so a fix to its marks \
         reaches it only in a new tab — or when the live agent upgrade types its relaunch \
         line there, which also sources this build's loader), `-` (no integrated shell, one \
         before its first prompt, or the lock was contended). `born=` is how the tab's shell \
         started: `cold` (spawned for this tab), or a warm spare the tab adopted \
         (config `spare_shell`): `spare-parked` (held before any startup file), \
         `spare-warming` or `spare-ready` (its startup files already ran), with \
         `spare_age_ms=` its age at adoption. \
         `input=` is whether the program is READING ITS INPUT, probed live from the kernel \
         (macOS; `-` elsewhere and off a tty): `clear` nothing unread; `pending` raw-mode \
         input unread and not (yet) judged a stall; `typeahead` complete lines a canonical \
         (line-mode) reader has not asked for yet — a shell's ordinary type-ahead, never \
         flagged; `stalled` raw-mode input unread for 10 s or more by a program that has \
         drawn nothing since and is burning CPU (or has sat asleep for 5 min — a pager on a \
         slow pipe or a shell inside a slow completion sleeps too, and a program that keeps \
         drawing is never stalled), both ages stretched by the machine's load (the larger \
         1- or 5-minute load average per online CPU, up to 10x: at a load of 150 on 18 CPUs \
         a spinning program is judged at 83 s, a sleeper at 42 min, so a program starved of \
         CPU is slow, not frozen, while burning CPU means half of one thread's share of \
         the loaded machine), entered only while aterm has read all the program's \
         output, so a program waiting on aterm is not blamed; `stopped` input queued under a \
         stopped job. `input_bytes=` counts every unread byte (the \
         kernel's queue plus aterm's own spill behind it) and `input_wait_ms=` is a LOWER \
         BOUND on how long the oldest has waited; `fg_rss_mb=` is the foreground program's \
         resident size in MiB while `stalled`/`stopped`, `-` otherwise. A frozen program's \
         screen never moves, so no screen fence sees this: an input-writing verb into a \
         `stalled` or `stopped` session answers `ERR busy input-unread`, and one written \
         behind raw input a second old carries `warn=input-unread` on its OK (see `key`). While `stalled`/`stopped` aterm also raises the session's attention itself, \
         as owner `aterm` (`meta attention_owner=aterm`: `<program> is frozen: not reading \
         input since <HH:MM> … — restart it: aterm ctl @<sid> signal term`, or, where the \
         window's harness would carry the agent's restart, `… restart it: aterm ctl @<sid> \
         restart pid=<leader> if-stalled; aterm relaunches it on its conversation` — a Codex \
         only as codex-alab, on the line its own record names), \
         over any older badge, and clears it when the program reads again; nothing is armed \
         while no input is unread. The queue `signal term` drops is not read: a program that \
         lives through it stays `stalled` (`input_bytes=0`) until it ends or shows it is \
         alive — it draws, reads a key sent since, or stops spinning with nothing left unread \
         — and after 5 s the line reads `… is still running after its restart signal … — end \
         it: aterm ctl @<sid> signal kill` (or the `restart` above, where it would be \
         carried). `supervisor=` is the live `meta set supervisor` \
         claim — who is answering this session's prompts — `-` when none. `autopilot=` beside \
         it is the session's EFFECTIVE Autopilot while it runs an agent the harness supervises \
         (its `meta set autopilot` word, else `[harness] autopilot`; `off` under the master \
         switch or with no control socket), `-` for any other session. For an agent session \
         `land_at=` is the instant its timed Cruise lands (unix seconds) and `task=` its \
         task's face under Land as its loop last told this window's harness (`landed`: \
         finished after the land instruction; `done`: finished before a landing was asked; \
         `spent`: no done reply after the nudges; `theirs`: a person's own exchange; \
         `open` otherwise), printed only while this window's harness holds the live claim; \
         `-` otherwise. \
         `human_ms=` is how long ago a PERSON last gave this session input — a key (an arrow, \
         a digit, Enter and Esc included), text, a paste, a click, a drag, a wheel notch or a \
         scroll of the view, from the window (a hover does not count), never a control verb \
         (`key`/`send`/`paste`/`turn`/`mouse`/`feed-bin`) — in ms, `-` when no person has since \
         the session started. An automatic supervisor keeps its hands off while it is under \
         `[harness] human_grace_s`, and waits on it before it keys a dialog a person may be \
         navigating (`text --json` carries it too, as \"human_ms\"; every `sessions` row as \
         `human_ms=`); `subscribe … events` pushes `EVENT <local> human` when a person starts \
         after 30 s without. `human_seq=` beside it counts those gestures (`0` before the \
         first): read it with a screen, and a write fenced `key|send if-human=<it>` lands only \
         if no person has keyed since. The stamp and the count live in this instance, so a \
         session carried across an update starts at `-` and `0` again. \
         `launch=` is the LAUNCH WITNESS: where that count stood (`launch_seq=`) in the \
         terminal-lock hold that parsed the shell's last signed command start (OSC 133;C), \
         before any frame could show a byte the command wrote — recorded wherever a signed \
         shell integration runs (zsh, bash, fish, PowerShell; any host). `person`: the \
         last gesture then was a plain Enter key press, the command's first word is \
         `claude` or `codex`, no box was on screen at the mark, and `human_seq=` still \
         equals `launch_seq=` — so no gesture can be a reaction to what the command drew; \
         a supervisor answers a folder-trust dialog under it at once only beside \
         `input=clear` (measured on macOS alone today: elsewhere `input=-` and the grace \
         holds), every key fenced `if-human=<launch_seq>`. \
         `since`: it was `person`, and a gesture has been counted since. `typed`: the last \
         gesture at the mark was not a plain Enter (or there was none). `boxed`: a box was on \
         screen at the mark. `other`: the first word names no agent this covers (`exec`, a \
         wrapper, or in zsh and fish an alias by another name; bash reports the command an \
         alias expands to). `pending`: the mark's screen is not read yet. `none`: no signed \
         command start since this instance began serving the session (an unintegrated shell \
         never has one; the count restarts across an update, so a mark never crosses one). \
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
         (spawned/state-change/title-change/cwd-change/meta-change/agent-change/status-change \
         (`phase= outcome= exit_code= signal= confidence=`, one per transition that survived \
         dwell)/human/modes-restored \
         — the last is the PTY reader handing the terminal back after a foreground program lost it \
         with modes armed: `from=<pgid> to=<pgid> program=<name|-> reverted=<csv> bytes=<n>`, or a \
         manual `reset`: `reason=manual source=<ctl|menu> reverted=<csv|-> bytes=<n>`) AND \
         the fabric/messaging \
         kinds the same ring records (hold/inbox/inbox-seen/post/fetch/post-landed/topic), and \
         `render` — the resize ledger entering or leaving `desync-risk` (`desync-risk run=<r> \
         displaced=<+-k> net=zero at=<ms>` / `healed run=<r> after_ms=<ms>`, see `resizes`; a \
         healthy resize records nothing here) — monotonic ids, rising on across an aterm \
         self-update (whose successor's first row is `handoff`), drop-oldest \
         ring. The two rows a session records as it is retired — `closing reason= by=` and the \
         final `state-change state=closed` — cannot be asked for here after the close: the sid \
         stops resolving in the same store write (only a request that resolved the session just \
         before that write can still read them). A live `subscribe @<sid> events` watch is what \
         delivers `closing` \
         (as `EVENT <local> closing reason= by=`, ahead of `exited`); `exits` keeps the same facts \
         afterwards",
    ),
    v(
        "log",
        Read,
        Lines,
        Session,
        "log [<node>] [lvl=] [since= ep=] [n=] [kinds=] [class=turn] [tier=content] [--json]: SESSION LOG",
        "- what the session did, as a tree: the session (`-`), its runs (`r<n>`: a shell \
         command between OSC 133 C and D, a program in front, an agent), an agent run's turns \
         (`t<n>`) and a turn's steps (`p<n>`: an approval box, a question dialog, a wall, an \
         input stall, the survey), with point marks: `human` (a person's input burst) on the \
         session, `agent` (the screen's verdict) on the agent run, others on the deepest open \
         node. `OK <n> v=2 as_of=<seq> ep=<e> oldest=<seq>[ lost=<k>][ more=<k>]` (`aterm ctl` \
         prints this header on stderr, as `aterm-ctl: OK …`, and the records on stdout), then one \
         record per line, oldest first: `<seq> <open|close|mark|amend|gap> <node> <kind> \
         [par=<node>] <k=v ...> [by=<actor>] src=<where> conf=<exact|strong|heuristic|claim> \
         t=<wall ms>` — a close carries `out=` and `dur_ms=` (`dur=wall` when measured from a \
         block's own stamps) and its rollup (`steps= appr= qs= walls= stalls= errs= lost=`); \
         values pct-encoded; a word this build does not know reads `unknown`. Never stored: a \
         command's arguments (a `cmd` run keeps its program word, and a known subcommand, as \
         `detail=`, as `status detail=` shows it), a box's command, the bytes typed, or screen \
         text beyond a wall's reset time (its `detail=`). `as_of=` is the newest seq: read on with `since=<as_of> ep=<ep>` (both, \
         from the same reply: an aterm self-update moves `ep=`; one that carries the log \
         continues it, writing `mark - handoff carried=<n> shed=<m> epoch=<ep>` with every \
         run, turn and step still open, and an old cursor reads on with a `gap why=handoff` \
         over the seqs the old process could still have used; one that carries none starts \
         with `gap why=handoff epoch=<ep>` and resets old cursors; seqs and node ids always \
         go on above the old process's). The ring holds 2048 \
         records or 256 KiB and evicts level-aware: a closed turn's detail first, then the open \
         records of closed runs and turns, closes once they pass half of it, then current \
         detail, never an open node's own open record (`lost=` counts every eviction; \
         `oldest=` is the seq from which nothing is missing). A read that covered records it \
         will never see, or a cursor of another epoch, gets a `gap why=<evicted|reset> lost=<k> \
         resync_to=<seq>` line first (`lost=` counts all it covered: where the seqs jump between \
         the records shows where); commands \
         the terminal's own block history dropped before any read reached them, a `gap - gap \
         why=evicted` record. \
         `<node>` keeps that node's subtree; `lvl=session|run|turn|step` the levels down to \
         it; `n=` the newest n that match (`more=` counts the rest); `kinds=<k>,...` those \
         kinds; `class=turn` what a fleet mirror would carry (sessions, runs, turns, needs, \
         gaps). The reply stops at the host tier; `tier=content` adds content-tier fields \
         (the OSC title, which `timeline` and `text` already show). `--json` (or `json`, \
         anywhere): one object per record, the same keys, the gap line as `{\"op\":\"gap\",\
         \"why\":…,\"lost\":…,\"resync_to\":…}`. A session that closed in this process \
         still answers `log @<sid>` (and `brief`), for the last 512 closed: the newest 32 from a \
         copy evicted as the ring evicts to 64 KiB, older ones from their session's open and \
         close alone, `lost=` counting the rest and `since=` reading on as it did; an edge \
         reads it only if it could read the session live. Local to this instance: no file, no \
         bus, not carried by an update",
    ),
    v(
        "brief",
        Read,
        Lines,
        Session,
        "brief [<node>|all] [budget=<tokens>] [since=<seq> ep=<e>]: the session log, token-budgeted",
        "- what an AI reader needs first, under `budget=` tokens (600 by default; bytes / 4): \
         `OK <n> brief sid=<sid> v=2 as_of=<seq> ep=<e> budget=<b> used=<u>` (on stderr from \
         `aterm ctl`, as `log`'s), a session line \
         (`- <program> <run> <age> · need=<word> <age> [src+]`, or the phase when nothing is \
         needed), the newest turns (`t17 open 4m10s by=human`, `t16 done 6m02s · 3 steps · \
         appr 1 [screen+]`) and their steps, ranges of older ones, the marks worth a line, \
         `need`/`claim` lines, and `more=` naming what was left out (`-` when nothing was). \
         The suffix says how sure: `!` exact, `+` strong, `~` heuristic, `?` claim; a phase \
         nothing has observed yet has none (`- session · -`). `<node>` and `since=<seq> \
         ep=<e>` (from a `log` or `brief` reply) narrow the records it reads; the needs and the \
         open path come from the whole log. The gap that read crossed (a cursor of another \
         epoch or ahead of the head, `why=reset`; one behind the ring, `why=evicted`) is a \
         `gap why=<w> lost=<k> resync_to=<seq>` line before `more=`, as `log` prints it. \
         `brief all [budget=]` is every session's under one `OK <n> brief all sessions=<k> v=2 \
         budget=<b> used=<u>` header, the whole within the budget (1000): shared evenly among \
         as many sessions as fit, those needing something first, then the most recently \
         active, and `more=sessions:<k>` counting the rest. Owner-only, like `sessions`",
    ),
    v(
        "mark",
        Write,
        Status,
        Session,
        "mark <node|-> <kind> [word=] [text=] [re=] [as=] [ttl=] [rule=]: write an attributed claim",
        "to the session log (design §7.4), answered `OK seq=<n> node=<node> by=<actor>`. \
         <kind>: `note text=<pct>` (at most 160 bytes encoded), `verdict word=<on-track|off-track|\
         blocked|done|failed|needs-human|unsure>`, `label text=<pct> [ttl=<1..3600 s, 600>]` (at \
         most 32 graphemes and 40 bytes), `need word=<review|decision|help>`, `clear \
         [word=<review|decision|help>]`, `act word=<approve|deny|answer|continue|relaunch|\
         escalate|interrupt|dispatch> [rule=<id>]`; each takes `re=<seq|@off>` (a record of this \
         log, or mail this session holds) and `as=<name>` ([A-Za-z0-9._-]{1,32}, never an actor \
         word: `ERR mark: as=<w> is a session log actor`). <node> is `-` (the deepest open node) \
         or an OPEN `r<n>|t<n>|p<n>` (`ERR mark: <node> is not open`, never moved elsewhere). A \
         connection that may not read the session names neither a node nor `re=` (`ERR denied`) \
         and is answered `OK by=<actor>` alone. The record is `src=mark conf=claim`, `by=` is \
         the CONNECTION's (`ctl` for any instance-token client — the owner's own and any \
         process inside a session alike — `harness` for the in-window harness, `bridge`, an \
         edge's source sid, else `-`), and `as=` is printed as a self-declared name, never as \
         `by=`. A MARK NEVER ACTS: it puts no byte on a PTY, signals nothing and retires no \
         session; to act, use `key`/`send`/`turn` under your own authority (the log records \
         that as `mark - input by=`). A claim's `clear` lifts only needs a claim raised, and \
         only its raiser's or, from `ctl`, any but the harness's; `brief` shows the harness's \
         verdict and label first, a label only from the harness or the owner. Rate-limited per \
         (session, author) at 1/s with bursts of 20: over it, `ERR \
         busy retry_ms=<ms>`; marks hold at most a quarter of the session's ring, the oldest \
         going first. Text is refused, never stripped or clamped, when empty, over its cap or \
         holding control, bidi or invisible formatting. Not halted by `hold`, not leased",
    ),
    v(
        "resizes",
        Read,
        Lines,
        Session,
        "resizes [<n>] [since=<id>]: grid resize ledger, net-zero flap runs and render verdicts",
        "- `OK <n> total=<N> runs=<R> flaps=<F> risks=<D> \
         render=<ok|displaced|desync-risk|unverified|unjudged> \
         unledgered=<U> lost=<L> nudge=<idle|armed|short|restoring> nudged=<N>` (`aterm ctl` prints this header on stderr, as `aterm-ctl: OK \
         …`, and the rows on stdout), then the `resize` rows (the newest <n>, or those past \
         `since=<id>`; oldest first), the `run` rows they belong to, and every run still \
         `silent`, `desync-risk` or `unverified` whatever the selection, then one `epoch` row \
         per epoch that is open or owed rows — one kind token first, parse by key. `resize <id> t=<ms> run=<r> ord=<n> from=<CxR> to=<CxR> alt=<0|1> sync=<0|1> \
         trimmed=<k> demoted=<k> pushed=<k> revealed=<k> appended=<k> restored=<k> shift=<+-k> \
         site=<input|window|chrome|term|pass|redraw|ctl-cross|-> at=<pct file:line|-> \
         win=<CxR|-> chrome=<k|->`: what the ENGINE did to the rows (a demote moves every row \
         up, an append or a trim moves nothing; `restored=` rows the engine's resize undo \
         handed back, a quiet alt-screen flap's demoted rows (also in `revealed=`) or pushed \
         ones; `ord=` the engine's resize ordinal), who asked \
         (`site=` the entry point that ran the window pass — the input dispatch a ctl `resize` \
         arrives by, the OS window, its chrome rows (tab strip, message band, presence row), a \
         direct re-grid, a pane pass, a deferred tab drawn — with `at=` the source line that \
         called it, in that build's source; `ctl-cross` a cross-session `resize`; `-` a resize \
         no path booked, found in the engine's journal and counted as `unledgered=`; `lost=` \
         counts ones that journal dropped first) and the window's grid and chrome rows then. \
         `run <r> t=<ms> n=<k> alt=<0|1> geom=<CxR>><CxR>>... net=<zero|changed> \
         displaced=<+-k> verdict=<none|silent|desync-risk|unverified|healed|reflow> \
         first_out_ms=<ms|-> healed_ms=<ms|->`: resizes on one screen with no full clear or \
         screen switch between them, at most 250 ms apart with nothing drawn between (the \
         app's SIGWINCH handler could see only the last size) or, once the run has displaced \
         rows and the resize takes the previous one back (a flap's second half), whatever \
         was drawn between (a frame is no sign the handler read the smaller size), or however \
         late while nothing has drawn since (a grow the undo answers then nets it to zero); \
         the 250 ms is measured only from a row its own path booked, never from a `site=-` \
         one; a `flap` is a net-zero run of two or more. Judged on the ALTERNATE \
         screen only, where rows are \
         addressed absolutely and a demoted row is gone unless the engine undoes the flap \
         (nothing output between its halves; `restored=`, and the rows go back to the run \
         whose shrink stashed them, even one the 250 ms gap closed, so it moved nothing): \
         `silent` = content displaced and the \
         app has not drawn since (or drew under 1 s ago), `desync-risk` = a NET-ZERO run the \
         app drew over without clearing, between its halves or after (its handler read an \
         unchanged size and skipped the repaint; the grow appended rather than restored, \
         `appended=1 restored=0`), `unverified` = a run whose geometry stayed changed, drawn \
         over since with no clear seen (the handler read the change, but a repaint that does \
         not clear and a diff on the moved rows look alike here; `cast drift` decides, and \
         weighs a size that later came back, however long after, as a flap; a later heal \
         still counts), `healed` = a full clear (ED 2, a home ED 0) or a screen switch \
         repainted it. `<CxR>` is cols x rows, the cast's `r` \
         spelling; `t=` is the `timeline` clock in ms, not the cast's seconds; \
         `first_out_ms=`/`healed_ms=` are upper bounds, taken when a reader (a status sweep, a \
         watch, this verb) first saw the draw or the heal, in ms after the resize the draw \
         followed (for a draw between a flap's halves, the shrink) and after the run's last \
         resize. A RISK, not a proof: a flap whose halves are more than 250 ms apart with an \
         app frame between them is two net-changed runs, never `desync-risk` (the half that \
         moved rows reads `unverified` once drawn on), a changed-size repaint that does not \
         clear between a flap's halves reads as a draw on displaced rows (a window dragged \
         down and straight back, at the turn), and a `desync-risk` stands until a full \
         clear or a screen switch even when the app repaints every row without clearing; \
         `cast drift` replays the recording to decide. \
         `epoch <e> t=<ms> geom=<CxR>><CxR> shift=<+-k> pushed=<k> drew=<0|1> \
         class=<agent|learned|unknown> verdict=<open|desync-risk|unjudged|healed|restored> \
         by=<app|undo|nudge|-> tried=<0|1>`: the second judge, which does not depend on run shape. \
         The engine counts the rows it owes the alternate screen since the last repaint from \
         scratch (`shift` rows moved up, `pushed` rows lost off the bottom; zeroed by a full \
         clear, a screen switch, a reset; exact however far the journal overran), an epoch \
         opens at the first resize after that with the geometry it began at, and it is at \
         risk when rows are owed, the geometry is BACK at its start and 1 s passed since the \
         last resize (`drew=` whether the program has drawn since: information, not a \
         condition — a flap an app frame split, or one nothing followed, is judged alike). \
         At risk: `desync-risk` for `class=agent` (the presence classifier reads Claude Code) \
         or `learned` (a run of this session was healed by a full clear inside the 1 s), \
         `unjudged` for `unknown`; `healed` = a full clear or screen switch repainted it, \
         `restored` = the engine's undo handed the rows back (`by=undo`). One `aterm.log` \
         line per epoch that reaches `desync-risk` or `unjudged`. A ConPTY session accrues \
         no debt and so no epoch. THE REPAINT NUDGE (`[render] repaint_nudge = \"on\"|\"log\"|\"off\"`, \
         default `on`): for an agent-class `desync-risk` epoch aterm tells the PROGRAM (only; \
         never the engine, the window or the screen it draws) its terminal is one row shorter, \
         waits for the program's full repaint, tells it the true size again and waits for the \
         second repaint, sending no key. `nudge=` is that machine's phase (`short` while the PTY \
         is held), `nudged=` how many times it has held one; `by=nudge` on a closed epoch and \
         on `EVENT … render healed` says it was the repair. Mode `log` runs everything but the \
         two size changes and writes `would-nudge …` to `aterm.log`. It waits for a quiet moment \
         and never runs under a halt, a driver's lease, a person's key in the last 1.5 s, a \
         selection, an open DEC 2026 frame, a shared session or a live drag. `status` carries `render= resizes= flaps=`; `subscribe … events` pushes \
         `EVENT <local> resize` and `EVENT <local> render …`",
    ),
    v(
        "metrics",
        Read,
        Status,
        App,
        "render/latency counters [reset|percentiles]",
        "- percentiles: n_<x>= and <x>_p50/_p95/_p99_ms= for input, present, render, key_write, \
         key_queue, pre_present, acquire, acquire_queue, resize, reflow, present_tainted, \
         term_wait_redraw/term_wait_press, echo, present_glass, out_req and ime, plus \
         turns_by_root=<root>:<n>/<p50>/<p95>/<p99> and counters including input_edges_dropped= \
         ime_cancelled= input_refused= (input a full queue refused) reply_dropped= (query \
         replies dropped). Plain line carries \
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
         max_redraw_total_ms) and main_busy_ms_per_s= (the booked turns' sum per second since \
         reset, about_to_wait included: the main thread's occupancy, also on the percentiles \
         line), then the self-update's pre-Commit input: input_dropped= \
         (keystrokes typed into an updated window before Commit that its bounded queue \
         refused — lost) and input_incoherent= (replays that had to disarm the held \
         modifiers: a drop, or a window losing focus with input already queued for it), both \
         cumulative for the process and kept across `metrics reset`; then \
         handoff_claim_stamps=/handoff_warm_pre_dial_ms= (`none` unless this boot claimed an \
         update handoff; `park` projects the parent's sampled park duration onto the claim, \
         so proof - park sums park->sample and claim->proof, excluding sample->claim; \
         after Commit the successor's stamps carry \
         `commit`, so commit - proof is the freeze's last segment), then process_birth_to_rust_main_ms= (the kernel's \
         birth of this process -> Rust main: the launcher's spawn, a Gatekeeper hold, dyld; \
         `none` off Apple) and startup_prepare_schema=1/startup_prepare_valid= with ten \
         exclusive startup_prepare_*_ms phases that split startup_gui_prepare_ms \
         (boot/warm/event_loop/services/journal/session0/control/config_join/app/arm; read \
         live, so a headless launch answers too), and the control socket's control_accepts= … \
         control_busy_replies=. \
         The line ends with the \
         PTY census: pty_masters= (the PTY masters the sessions hold open) and pty_sessions= \
         (the live sessions that own one) — equal at rest; a closing tab's master may lead for \
         a moment while its reader winds down, and a lead that STAYS is a closed tab's master \
         nothing will close. IT NAMES WHO OWES THE TIME: echo_* high with input_* low means the PROGRAM \
         is behind (a TUI sharing its scheduling band with a build has been measured echoing at \
         p99 150ms while aterm wrote every key at p99 6.29ms), the reverse means aterm is; a \
         lone max_present_latency_ms is not evidence of either. READ THE SLICES HONESTLY: present_* and input_* are OPEN INTERVALS closed by the next qualifying present, so any stretch in which nothing presented is INSIDE the number (only a 5 s discard bounds it) — a multi-second present_latency means \"nothing presented for that long\", not \"a frame took that long\"; input_* closes on the next CONTENT present, which under concurrent streaming output may be a log-line frame rather than the key's echo, so it can read low. input_* counts EVERY key a content present answered (up to 32 waiting per window; the overflow is input_edges_dropped), so keys that never echo — a password prompt — each book their wait until the next content present (under the 5 s discard): measure typing latency in a program that echoes. Quote n_* with the percentiles, never a lone last_/max_, and note both stop at application-present return (no compositor selection, scanout or photons)",
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
         [cadence=<ms>] [cap=<n>] [yield=<floor>] [if-gen=<epoch>.<seq>] \
         [if-rows=<first>-<last>:<hex16>] [cause=@<off>|<seq>]. cause= (once, in any position \
         among the options) names the mail (@broker offset) or session-log record (seq) this \
         turn acts on: the turn it opens carries cause= in `log`; it must name mail this session \
         holds or a record of its own log, else ERR cause and nothing is typed (an edge needs \
         read too); an aterm older than it types cause= as text, so check `help turn` names \
         cause= first. presses=<n> is the most submit presses in all (default 3, \
         max 10; 1 never re-presses), each verified within submit_window=<ms> (default 2000). \
         settle=match:<re> keys \
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
         interval — the seam is told nothing else. The typed prefix is capped at 240 graphemes \
         (cap=<n>, up to 4096, raises it: an agent may read a long PASTE as pasted, not as its \
         person's words, and the same text pressed key by key as its person's); \
         the remainder is one paste (voiced as one up-strum of the music box's chord, keyed off \
         the paste itself, human or agent alike). The verdict then carries typed=<keys> \
         pasted=<graphemes>. A typed text is never submitted in part: one whose schedule would \
         end past timeout= is not begun, one the deadline overtakes stops there, and a FENCED \
         one (if-gen=/if-rows=) stops at a person's key or a screen switch counted between its \
         keys (the fences' own values cannot be asked again: the turn's own echo moves them), \
         and an if-rows= one whose rows ARE an input frame — the first row a rule, the last a \
         rule, the row after the first opening with a caret (`❯`); any other rows, the person \
         and the screen switch alone — at the cursor leaving that frame: before each key the \
         row under the cursor must still be the bottom rule, \
         and the nearest row at or above it that is the top rule must still sit over a row \
         opening with the caret, the frame free to move and to grow and every row outside it \
         free to change, so a box drawn in its place stops the typing. A frame the program has \
         not finished drawing (an open DEC 2026 bracket) is not judged: the key waits for it, \
         and a digit key waits until the program has read every key before it and the \
         caret has moved since the key before it. Each stop \
         presses nothing more, Enter included, and answers `OK 0 turn skipped \
         reason=<deadline|person|changed> typed=<k> pasted=<p> submitted=0 seq=<n> id=<n>` \
         (zero rows), the k keys already typed left where they landed; a wait the budget runs \
         out in is reason=deadline. \
         yield=<floor 0..=1> parks BEFORE typing until the target's typing \
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
         if-rows=<first>-<last>:<hex16> is the NARROW fence, checked at the same point: it types \
         only while FNV-1a-64 of visible rows <first>..=<last> (0-based; each row as `text` serves \
         it, followed by a newline — the `hash=` stamp's own definition over just those rows) is \
         still <hex16>, so rows outside the range (a spinner, a ticking timer) may move; a range \
         past the grid answers `reason=changed`, and a bad value is `ERR usage`. Both fences may \
         be given, and each must hold. \
         EXACTLY-ONCE: a leading id=<epoch>:<producer>:<seq> makes the turn replay-safe — see \
         `send`'s entry for the key, which exactly `send`, `key`, `feed-bin` and `turn` take. \
         The OTHER input verbs (`paste`, `ctrl`, `feed`, `paste-bin`, `mouse`, `pointer`, \
         `hwkey`) take NO key — a leading id= on `paste` is delivered as literal text — so a \
         crash-safe driver retries only through the four keyed verbs. A DUPLICATE answers `OK 0 \
         dup=1` (this verb is Lines-framed) and carries NONE of the verdict fields above, id= \
         included: nothing was typed, so there is no verdict to report. The reply's own id= is \
         the TURN id and is unrelated to the key you sent. UNREAD INPUT: a turn into a program \
         `status` calls `stalled` (or a stopped job) is refused `ERR busy input-unread …` \
         before it types anything — asked when it arrives and again after its yield= (see \
         `key`; turn takes no unread=ok); one into a program that is only slow (raw input a \
         second old) types, and its verdict line closes with ` warn=input-unread \
         input_bytes=<n> input_wait_ms=<ms>` (the reading when it arrived). It never \
         RE-presses while an earlier byte is still unread — the presses END there, never \
         wait: its one Enter waits in the queue, the verdict says submitted=0, and the turn \
         settles as ever",
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
         writes on — its own `turn` runs under it and hands it back; `status` says hard=1. \
         `status` also says pid=<n>: the kernel's pid of the process that took the lease (the \
         peer of the direct connection its acquire came on, re-read at each renewal), never \
         the holder= text, which any client can spell; absent where no kernel named one (the \
         bridge, a relay). A person's keyboard is never blocked. A lease does not cross an aterm self-update: the \
         new instance starts with none, and its holder acquires it again there",
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
         order beside id=, and the two that take the if-gen=/if-fp=/if-human= fences `key` \
         describes. \
         An aterm older than the fences types a leading if-gen= into `send`'s body as TEXT \
         (`key` answers it ERR usage), so fence a press with `key`, and send a fenced write \
         only where `help send` names the fence. UNREAD INPUT: behind raw input the program \
         has left unread for a second, `send` writes and answers `OK seq=<n> \
         warn=input-unread input_bytes=<n> input_wait_ms=<ms>`; into a `stalled` program or a \
         stopped job it is refused `ERR busy input-unread …` and writes nothing — see `key`; \
         a leading unread=ok queues anyway. CAUSE: a leading cause=@<off>|<seq> names the mail \
         (@broker offset) or session-log record (seq) this input acts on — a turn it opens \
         within 5 s carries cause= in `log`; it must name mail this session holds or a record \
         of its own log, else ERR cause and nothing is written (an edge needs read too). An \
         aterm older than it types cause= as TEXT: send one only where `help send` names it. \
         Every write verb's input is noted in the session log as `mark - input verb= by=` once \
         per author's 30 s burst, never its bytes",
    ),
    v(
        "paste",
        Write,
        Status,
        Session,
        "bracketed-paste text to the PTY",
        "the payload gets paste handling — bracket guards when the app has set \
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
         retry. A guarded write the terminal's input queue takes only in part (a macOS pty \
         holds 1022 bytes) is still delivered WHOLE: the rest follows in order as the program \
         reads, ahead of anything written after it (a guarded press behind it is `ERR busy \
         sink` until it has gone), and the reply is `OK seq=<n>`; only a rest that cannot be \
         queued answers `ERR write failed partial accepted=<n>`, in doubt, never retried. \
         THE REGEX IS ONE WIRE TOKEN: the control line is split on whitespace and \
         nothing quotes, so write a space as `.` — `key if=Do.you.want.to.proceed 1`, never \
         `key if='Do you want' 1` (that arms `Do` and presses `you`). Composes with id= in \
         EITHER order (`key id=… if=… 1` and `key if=… id=… 1` are one request); a skipped \
         guard gives its id= sequence back, so the retry is re-evaluated against the live \
         screen rather than answered `dup=1`. ORDER OF REFUSALS: `ERR halted`, `ERR rate`, \
         `ERR busy turn=`, `ERR usage`, `ERR busy input-unread`, `ERR badregex`, then the \
         guard and fences, then the id= claim — so a halted session is `ERR halted` whatever \
         the guard says, and no refusal spends an id= sequence. UNREAD INPUT (macOS; never \
         refused or warned elsewhere): a key sent while the program has left input unread in \
         its tty is read AFTER those bytes. A SLOW READ IS A WARNING: behind raw-mode bytes \
         at least a second old every input-writing verb (send key ctrl feed feed-bin paste \
         paste-bin mouse focus turn hwkey pointer, invoke Paste) \
         WRITES, and its OK closes its first line with ` warn=input-unread input_bytes=<n> \
         input_wait_ms=<ms>` (the backlog it was written behind; after seq=, so every \
         parser of the reply reads what it read) — on a loaded machine a healthy program is \
         often that slow. Only where writing is known to be harmful does it write NOTHING \
         and answer `ERR busy input-unread bytes=<n> wait_ms=<ms> input=<stalled|stopped> \
         (<why and the remedy>)`: `status` publishes the program `stalled` (raw input 10 s \
         old, the age stretched by the machine's load, while it spins and draws nothing, or \
         sleeps for 5 min stretched alike — see `status`), its job is stopped with bytes \
         queued, or aterm dropped a stalled program's queue (`signal term`, `reset flush`) \
         and it has not ended or read since. A screen fence cannot see it — a frozen screen \
         always passes if=, if-gen= and if-fp=. It is `ERR busy`, so a back-off retries it; \
         a human's window keyboard is never refused; `signal` (int interrupts, term restarts a frozen \
         program, cont resumes a stopped one), `close`, `resize`, `pane` and `tab` stay open; \
         a canonical reader's queued lines (a shell's type-ahead) never refuse or warn; and \
         nor does \
         a lone signal character (key ctrl+c, ctrl c, feed 03 — the tty's own intr, quit and \
         susp) while the tty has ISIG on (a cbreak program) and the kernel can take it now \
         (no spill ahead of it, room in the queue), because the line discipline turns it \
         into a signal as it is written and it never queues — in raw mode it is a byte like \
         any other, refused into a stalled program, and `signal int` is the interrupt. A \
         leading \
         unread=ok queues anyway — on send and key in any order beside the other options, on \
         hwkey, pointer and invoke (the App lane) as the FIRST token; any other value, or a second one, is `ERR usage`. The guarded press is delivered on the control thread against the \
         target's own terminal and PTY (like every `@<sid>` input verb), so for the tab on \
         screen it skips the App seam's cosmetic side effects; a press that must ride the \
         seam is a plain `key`. `send if=<re> <text>` is the same guard on a raw write. \
         FENCES (key [if-gen=<epoch>.<seq>] [if-fp=<hex16>] [if-human=<n>] <name>): a leading if-gen= presses \
         only if the screen generation is still that one (`status gen=`, or `agent_gen=` for a \
         press decided from the agent verdict — any output since moves it, and so does any \
         screen switch), and if-fp=<hex16> only if FNV-1a-64 of the visible screen is still \
         that value (`status hash=`, `agent_fp=`, `turn hash=` — an identical repaint keeps it). \
         Checked under the same lock hold as the press; a screen that moved answers `OK skipped \
         reason=changed seq=<n>`, nothing written, the id= sequence given back, so a supervisor \
         that decided on one box cannot press into the box that replaced it. They compose with \
         if= and id= in any order; a bad value is `ERR usage`, and so is if-seq= (seq= is per \
         grid and repeats after an alternate-screen re-entry, so it is no fence). A plain `if=` \
         miss still answers `OK skipped` exactly. THE PERSON FENCE: a leading if-human=<n> \
         presses only if no PERSON has given the session input since the read that returned \
         <n> (`status human_seq=`, `text --json`'s \"human_seq\": how many person gestures have \
         reached the session through a window) — a key a person typed that the program has not \
         read yet moves no screen, so no screen fence sees it; checked under the same lock \
         hold, ahead of the screen fences, it answers `OK skipped reason=person seq=<n>`, \
         nothing written. It is NOT atomic with the person's own write, which takes no \
         terminal lock: a person's gesture counted after the check and written before this \
         press, inside the one lock hold, still lands first. THE SUPERVISOR FENCE: a leading \
         if-supervisor=<holder> — on key, send and turn beside the other options, and on paste \
         as its ONE leading option — writes only while <holder> holds the session's live \
         supervisor claim (`meta set supervisor`), read under the meta lock right before the \
         write and again before each phase of a turn; else `ERR busy supervisor=<holder|->`, \
         nothing written (a supervisor whose claim a proven self-claim replaced types \
         nothing after it). TRAILING TOKENS: `mods=<list>` \
         (`mods=ctrl+shift`; `ctrl+u` is `u mods=ctrl`), `type=press|repeat|release` \
         (`down`/`up` alias the ends) and `base=<c>` (the US-layout key kitty reports as \
         its third field). A MODIFIER KEY'S RELEASE takes `mods=` as the state the release \
         LEAVES, as kitty reports it: `key shiftleft type=release` is Shift let go (`CSI \
         57441;1:3u` under report-all + event types), and `mods=shift` on it says the other \
         Shift is still held (`CSI 57441;2:3u`) — so a release does not repeat its press's \
         `mods=`",
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
        "pointer [move <r> <c>|click|leave|status|mods <m>]: put the POINTER on a cell for hover",
        "— `mouse move` posts an engine `InputEvent` and never touches the window's pointer, so \
         nothing it does makes a link hover, a divider cursor or a tab-strip highlight happen. \
         This drives `App::on_cursor_moved` — the identical function `WindowEvent::CursorMoved` \
         calls — after mapping the cell to the centre pixel of the frame it is drawn in, and \
         `pointer leave` drives `on_cursor_left` (what `WindowEvent::CursorLeft` calls). Reply: \
         `OK at=<row>,<col> px=<x>,<y>` is where the pointer ACTUALLY resolved, read back from \
         the window AFTER the real path ran, in window (not pane-local) cells, and the window \
         pixel it is at — so it states where the pointer IS rather than repeating the request, \
         whoever put it there (a person's or a seat's pointer too). A NEGATIVE row is a chrome row above the \
         grid — `move -1 <c>` the one right above row 0, the message band's last row when one is \
         up — and reads back negative too. `click` presses and releases the left button where \
         the pointer is, through the functions `WindowEvent::MouseInput` calls (a band capsule, \
         a tab), each on macOS after the `CursorMoved` at the same pixel that AppKit sends \
         ahead of a press and a release; `ERR` before any move. `click` is Owner-only, as `notice` is, and a click on \
         a band capsule spends one of `notice act`'s 10 presses a minute (`ERR busy notice: \
         10 presses a minute retry_ms=<ms>` once they are spent); while a seamless update has \
         parked the process it presses nothing and spends nothing, refused as `notice` is \
         (`ERR busy notice: aterm is switching to its new version retry_ms=<ms>`). A cell \
         outside the grid or the chrome rows is `ERR`, \
         never silently clamped onto a neighbour and reported as though it had been honoured. `OK at=-` \
         when the window holds no pointer position at all: after `pointer leave`, and before the \
         first move of any kind. `at=-` says only that — never a position that does not exist. \
         `mods <list>` sets the modifier keys the window holds, through the function \
         `WindowEvent::ModifiersChanged` calls, until the person's own next key, click, motion \
         or focus change puts back the modifiers they really hold; Owner-only, like `click`: \
         `+`-joined `cmd` (`super`), `ctrl`, \
         `alt` (`option`), `shift`, or `link` (the open-link modifier: Cmd on macOS, Ctrl \
         elsewhere), or `none`; `pointer mods link` then `move` shows the hand and the caption a \
         held Cmd offers, and `click` is a Cmd-click (a plain-text `path:line` opens aterm's \
         editor there). Read the hover the pointer resolved with `cell <r> <c>` (`link=`) and \
         see the destination band with `image`. FRONT window of the resolved instance, like `hover`. Refused like every \
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
         input=stopped` names; term ends a program that has stopped reading its input, and \
         int interrupts one whose tty is raw (where `key ctrl+c` is a byte queued behind the \
         unread input). A signal is just a signal: the window's harness reads the exit it \
         causes as the program's own (a graceful one is left at the shell). To restart an \
         agent the harness supervises, use `restart` — its record is written before its \
         signal, so the exit is relaunched on its conversation, and it ends a survivor itself. Never refused by the unread-input gate. term, \
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
    v(
        "restart",
        Signal,
        Status,
        Session,
        "restart [pid=<n>] [if-stalled] [quiet=<s>]: restart the tab's agent in place",
        "- the agent the window's harness supervises or watches in this tab (Claude Code, \
         codex-alab) is ended and relaunched on its own conversation: the stall's remedy, and a \
         person's restart. Its record comes FIRST: a stamp naming the process (pid and kernel start) is written before \
         anything is sent, so whoever reads the exit — this window, or its successor after a \
         seamless update — relaunches it, a keystroke before it not read as a person's /exit. \
         `pid=<n>` acts only while <n> leads the foreground process group; `if-stalled` only while \
         a stall is published for it (`status input=stalled`); `quiet=<s>` not while a person gave \
         the session input within <s> seconds. Replies: `OK restarting pid <n> cause=<stall|asked>` \
         (` discarded=<bytes>` when input it left unread was dropped first, as `signal term` \
         drops it), answered at once — the program's end is not waited for; a request made while \
         an earlier one's stamp still names the process joins it (one stamp, its cause kept, its \
         time renewed once this request's SIGTERM is sent; one watch ends the program, on the \
         earlier request's clocks); \
         `OK restart gone pid <n>` (it stopped leading the tab before anything was sent: nothing \
         sent, and a stamp this request wrote taken back — never one it joined); `ERR restart \
         unavailable: <why>` — not-the-hosts-agent (no harness snapshot names \
         that process), not-relaunchable, not-supervised (the harness's claim is not live here), \
         driver-lease, driver-turn, halted, start-unread, no-state — nothing written, nothing sent: \
         use `signal term` and resume it yourself; `ERR not-stalled`; `ERR busy person \
         human_ms=<ms>`; `ERR pid <n> is not the foreground process group (<pgrp>)`; `ERR no \
         foreground process group`; `ERR bad restart argument: <word>`; `ERR restart failed: \
         <what>`. THE KILL IS ITS CONTRACT: the restart always ends the program it stamped — one \
         still leading the tab 5 s after the SIGTERM with its stall still published and nothing \
         read (deaf), or 30 s after it in any case, gets SIGKILL (the input left unread dropped \
         first); one that stopped leading the tab by then (a job stopped from elsewhere) gets \
         it once it leads again, while its stamp stands (5 min). A hold, another driver's lease \
         or named turn, or another supervisor's claim that lands after the request pauses the \
         SIGKILL until it is lifted, while the stamp stands; one that outlasts the stamp lets the \
         program go. No SIGKILL is sent in the stamp's last 30 s, nor after it, when its exit \
         would no longer be read as the restart's, nor once the tab's stamp names another \
         program (a later restart's), nor before this window's harness has read the agent (a \
         successor after a seamless update reads it once it leads its tab again). Its end read \
         while a hold, a lease or a turn stands is still the restart's: relaunched once that is \
         lifted, unless another supervisor claims the tab first, which takes it. `[harness] \
         relaunch = false` does not refuse it. Refused under a hold like `signal`. POSIX only: Windows \
         answers `ERR restart unsupported on this platform`",
    ),
    v(
        "reset",
        Write,
        Status,
        Session,
        "reset [flush]: hand a stuck terminal back to its host defaults; keeps screen and scrollback",
        "- the escape hatch the automatic foreground handback cannot be: every mode a program \
         negotiates (alt screen, kitty keys, modifyOtherKeys, mouse tracking and encoding, focus, \
         2026, 2048, 2031, cursor keys, keypad, paste, wrap, charset, hidden cursor, scroll \
         region) goes back to what the host configured, a torn escape sequence is cancelled \
         (CAN), an open OSC 8 link is closed and program-set colours (palette, fg/bg/cursor/\
         selection) go back to the theme (OSC 104/110-119, `colors`; the colour stack is kept) \
         — the handback's own byte plan with no evidence gate. Never clears the screen or the scrollback; leaving the alt screen shows the main \
         screen again. It runs on the session's PTY reader between two output batches, so the \
         bytes are recorded like program output (temporal RawIn, cast, `bytes`) and `timeline` \
         gets `modes-restored reason=manual source=ctl reverted=<csv|-> bytes=<n>`. Reply: `OK \
         reset reverted=<csv|-> bytes=<n>` (`reverted=- bytes=0`: nothing was stuck). `flush` \
         then drops the tty input queue the stuck modes filled (mouse reports, CSI-u chords): \
         ` discarded=<n>` counts the bytes the kernel reported readable plus any aterm still \
         held; a partial line typed under canonical mode is dropped too but not counted (the \
         kernel does not report it). The flush is macOS-only: elsewhere (and on a session with no \
         tty) nothing is flushed and the field reads ` discarded=-`, never a 0. A held session answers `ERR halted` (the menu row is the human's \
         way). A live TUI loses its modes too. Edit ▸ Reset Terminal (`invoke ResetTerminal`) \
         is the same act on the front session",
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
        concat!(
            "or `open app markdown|editor <local-file-uri>`. `open <target> [close]` opens (or \
             closes) a surface of the front window, target: ",
            open_targets!(),
            " (",
            open_aliases!(),
            "); reply `OK opened \
             <target>` (`OK closed <target>`). `front` is always open; conn-card and \
             session-picker open only from their gestures (`invoke ConfigureConnection`, \
             `invoke ConnectToSession`); `window <target>` photographs and `controls <target>` \
             reads each one"
        ),
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
         while input waits unread; a leading unread=ok fires it anyway. An action that asks \
         first (off macOS, `CloseTab` or `Quit` over a running job; on Windows also over \
         several tabs) answers `OK confirm pending \
         kind=<quit|close-window|close-tab|close-pane> window=<n>; `confirm yes|no` answers \
         it` — nothing has closed yet — and one made while another question stands is `ERR \
         <action>: a quit is waiting for an answer in the window; …`",
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
        "— off macOS a close or quit that would end a running job asks in the window \
         instead of a dialog, and every verb keeps answering while it waits (`controls \
         front` shows `confirm open=true`). `OK answered=<yes|no> \
         kind=<quit|close-window|close-tab|close-pane>`: `yes` runs the close, and a confirmed quit ends the \
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
         serious_sound= serious_visual= sounds_master= volume= engine=none|live panes= \
         active= intensity= tail= max_streaks= idle_secs=; `serious_visual` is false while \
         Serious Mode takes the comet; `motion_animates` folds the W11 unfocused \
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
         witnesses that verdict was computed from, among them \
         motion_stage= and shed= (a dark trail under load shed or Reduce Motion \
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
         from sleep) costs one reopen and the next key sounds; \
         reopens_left=0 beside audio=failed is the one honestly \
         permanent reading. The typed window's TEXT is never reported)",
    ),
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
         pins only the cat that would ride anyway (the focused window's tenured program cat, \
         else the launch kitty). The collection elects by GREATEST pin stamp, so a wear is monotone: \
         it needs no unpin, and it survives a merge of the collection from another \
         instance. Refused, never silent, when effects or `[sparkle_words.feline]` are off \
         (there is no cat to dress) or when no row answers to that key.",
    ),
    // Read-only observability for the cursor-trail engine: the last N
    // licensed/declined verdicts from the fixed-size admission diagnosis ring,
    // and (`trail status`) the engine's standing state.
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
         and the ribbon retracted to it), `recall` (the caret came to rest on or left of a \
         typed echo with no key behind the move — a redisplay torn across reads, whose \
         cells the ribbon retracted), `motion` (Classic and Classic mono only: the classic \
         wake lights every observed cursor move, typed or not, so its rows are never \
         declined), or `none` on a decline. On every style but Classic a move paints only \
         if a keypress or a delivered insert LICENSED it, so a \
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
         addressable. `theme=<name>` gives the newborn its own theme (`theme session`), checked \
         first: one that does not draw is `ERR theme \"<name>\": <why>` and nothing is spawned. AIM it with window=<id> (an id from `inspect app/v1 tabs`) or `@<sid> \
         spawn` (the window hosting <sid>; window= wins); split= then divides THAT window's \
         focused pane. raise= defaults to true when no window was named (the `aterm new-tab` \
         attach contract) and FALSE when one was - an agent aiming at a background window is \
         not asking to see it; say raise=true to insist. Unknown id: `ERR no such window <id>`. \
         On macOS and Linux a cwd= no shell can start in is refused: `ERR no such folder \
         <path>`, or `ERR no access to folder <path>` for one this user may not enter. \
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
         is its window's last - finish or interrupt the job, then close again. A job is a command \
         the shell's integration says is running, or a foreground program the shell started; work \
         a shell does inside itself with no command marks (cmd.exe marks only its prompts, and \
         `--no-shell-integration` marks nothing) is not seen, and the close proceeds. Only the \
         closed pane's job counts: a sibling's keeps running, and a session another tab or window \
         also shows is not hung up. That refusal is the verb's WHOLE confirm: a wire close never \
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
        "ready [<ms>|timeout=<ms>]: block until the session is alive and idle",
        "Answers `OK ready prompt` (shell integration shows a prompt) or `OK ready idle` (60 ms \
         of quiet); `OK timeout` after 30 s by default (max 600 s); `ERR exited` if the session \
         ends.",
    ),
    v(
        "await",
        Read,
        Status,
        Session,
        "await <idle|seq|match|gone|block|inbox|consent|momentum|agent|log> [args] [timeout=<ms>]: wait",
        "Full grammar: `await idle <ms>` (no output for that long — measured only while aterm is \
         reading the session: while an update has its reader parked the clock is held, and a \
         whole <ms> is counted again once output flows; `ready` and a `turn`'s idle settle alike), \
         `await seq [<n>]` (the \
         content sequence passed <n>; bare = the next change, so `await seq <n> timeout=0` is the \
         cheap dirty check; a <n> ABOVE the current sequence — read off another grid, or off the \
         instance an update replaced, whose successor counts afresh — answers at once, `OK seq \
         <current>`, the anchor to use next), `await match <re> [rows <a> <b>]` (a visible row matches), `await \
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
         so a row the agent chose to ignore cannot latch the same wait twice; the ids keep rising \
         across an aterm self-update, and a `since=` above every id the session has handed out \
         is `ERR bad since high=<n>` — re-read `inbox` and take its newest id. `await inbox \
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
         elapsed_ms=` (no seq). `await consent kind=debugger` waits instead on the debugger rule \
         (the `debugger` row of `privacy`), with the same deadline, default and ceiling, and \
         latches as `OK consent debugger_mode=<enabled|disabled|unrecognized> elapsed_ms=` on the \
         first completed reading whose mode differs from the baseline, the first completed one; \
         a reading that observed no mode (pending, refused, a headless instance) neither arms \
         nor latches, so there it can only time out. It is read from the instance's cached answer, refreshed at most once a \
         minute unless focus gain or the owner's change retires it, never a probe per poll, and \
         it ends `ERR exited` when this session exits. An older server answers `ERR \
         usage: await consent [timeout=<ms>]`: read `privacy` once instead, never in a loop. \
         And `await inbox`'s hold-TRANSITION latch answers `OK inbox \
         hold=0|1`. Every form answers `ERR exited` once its session exits or is closed, and \
         `OK timeout` otherwise, after 30 s by \
         default (`await consent` 300 s; max 600 s), which the client exits 124 on. \
         Each parked wait holds one of the 64 wait slots every tab shares (past them, one of 4 \
         request slots; then `ERR control server busy`), so park at most one per driver. \
         `await log need|open=<lvl>|close=<lvl>|kind=<k> [since=<seq> ep=<e>]` parks on the \
         SESSION LOG (<lvl> session|run|turn|step; `need` is a record that raises one: an \
         approval, question, wall or stall step's open, a run closed `out=fail`, the session's \
         close, a `need` mark, whose claim shows `conf=claim`) and answers `OK seq=<n> ep=<e> \
         <record>` for the first record past `since=` that satisfies it, with ` gap=<why>:<lost>` \
         before the record when records before it were lost; `OK gap why=<w> lost=<k> \
         resync_to=<seq> ep=<e>` when the read crossed a loss and nothing satisfied it — re-arm \
         with `since=<resync_to>`, which reads past the hole. `since=` needs the `ep=` it came \
         with (a `log`, `brief`, `status log_seq=/log_ep=` or an earlier `await log` reply's). \
         With no `since=`, `need` LATCHES: an observed need already open answers at once (`OK \
         seq=<raised> ep=<e> as_of=<head> latched=1 <record>`, a claim's need left out), the \
         same need every time until it closes — after reading it, re-arm with `since=<as_of> \
         ep=<e>`, never without `since=` — and every other form waits for what comes after the \
         head. A session whose shell exited answers its close record, then `ERR exited`. Woken \
         by every producer, never polled; an older server answers ERR usage. For a turn's end, \
         `await log close=turn`; for a prompt to answer, `await log need since=<seq> ep=<e>`",
    ),
    v(
        "wait",
        Read,
        Status,
        Session,
        "wait [<ms>]: block until the running command completes (OSC 133)",
        "Answers `OK complete <id> exit=<code|->`; with nothing running it returns the newest \
         completed block at once. `ERR exited` once the session exits or is closed; `OK timeout` \
         after 30 s by default (max 600 s); without shell integration it can only time out or end.",
    ),
    // streaming
    v(
        "subscribe",
        Read,
        Push,
        Session,
        "subscribe @<sel>[,...]|@* <streams> [since=][every-frame]: push DELTA/EVENT/GAP/BYTES/MAIL;",
        "streams=screen,cursor,cells,bytes,events,mail,log,sessions, at least one of them (a \
         modifier-only list is `ERR usage`); log = the SESSION LOG live (`LOG <local> <record>`, \
         the line `log` prints at the host tier, one per record appended; `log:lvl=<session|run|\
         turn|step>` and `log:class=turn` filter as `log lvl= class=turn` do), `GAP <local> \
         log-lost=<n> why=<evicted|handoff|reset|…> resync_to=<seq> ep=<e>` when the records past \
         the cursor crossed a loss (exact), live from the head unless resumed with \
         `since-log=<seq> log-ep=<e>` (single target, the pair together; `since=` is the content \
         sequence's), or, for a session `@*` adopts that was created after the subscribe, from \
         its log's start; its target's `sub <local> <sid>` line names where it starts, ` \
         log_seq=<seq> log_ep=<e>` (resume from the last `LOG` read, or from there); events = the per-target digest (`EVENT <local> turn|block-complete|\
         meta|title|bell|inbox|inbox-seen|post|fetch|post-landed|hold|topic …` (the fabric kinds \
         are metadata, never a body), `EVENT <local> agent <word> rev=<n> gen=<e.s> fp=<hex16>` each \
         time the server's agent \
         verdict moves (`status agent=`), `EVENT <local> status phase=<p> outcome=<o> \
         exit_code=<n|-> signal=<n|-> confidence=<c>` each time the classified status moves \
         phase or outcome (`status phase=`; one per transition that survived dwell), \
         `EVENT <local> human` when a person starts typing, \
         clicking or scrolling in it through a window after 30 s without (`status human_ms=`), \
         `EVENT <local> harness-policy gen=<n>` when the window's `[harness]` policy moves (re-read \
         `harness policy`), \
         `EVENT <local> resize n=<k> id=<last> to=<CxR> run=<r>[ dropped=<n>]` at most once a \
         wake for the grid resizes booked since (see `resizes`; `dropped=` counts rows its ring \
         evicted before this watch was shown them — a count on this frame, never a `GAP`), \
         `EVENT <local> render desync-risk run=<r> displaced=<+-k> net=zero at=<ms>` / `EVENT \
         <local> render healed run=<r> after_ms=<ms>` when `status render=` enters or leaves \
         `desync-risk`, then, as the \
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
         EMPTY by default and an empty set receives nothing. `ls` (and the bare form) answers `OK <n> \
         seen=<w>` (the HANDLED watermark, as `inbox seen` moves it) then one \
         `topic <t> since=<head|@<off>>` row each; `add` and `drop` answer ONE STATUS LINE \
         (see `framing_of`) and each pushes `EVENT <local> topic add|drop …` on the `events` \
         digest, which is how this session's bridge learns of the change AT ONCE (a session its \
         push lane does not watch, past the 256-target cap, has no watch to push it; the bridge \
         reads the `topic ls` of every session whose watch it holds no ack for on every 2 s \
         roster round). On a watched session a `drop` then an `add` are two events in order, \
         so the second's `since=` is read; a polled session is read as its final set, so a \
         drop and re-add between two rounds keeps the topic's first `since=`, and its \
         `since=head` resolves when the round reads it. A `drop` takes effect at this endpoint \
         the moment it answers: `deliver` checks the set itself, so a broadcast the bridge \
         still routes by its older copy is refused `ERR not-subscribed`. `add` answers `OK <t> since=<head|@<off>> added=<0|1>` — \
         `since=head` (the default) takes only records published from then on, `since=@<off>` \
         replays the topic from that broker offset so a session joining late can read what \
         it missed. `drop` answers `OK <t> dropped=<0|1>`. The topic is \
         `[a-z0-9][a-z0-9._-]{0,31}`; anything else is `ERR usage`. A delivered broadcast is \
         an ordinary inbox row — same ring, same per-sender quota, `task`/`control` still demoted \
         from a principal this session does not accept — carrying `topic=<t>`, and it is \
         pushed on `subscribe … mail` with `topic=<t>` too. OWNER, not Read: adding a topic \
         changes what reaches an agent's inbox, which is the halt's authority class rather \
         than a read's. The set survives a bridge restart and a seamless update, and so do its \
         unhandled broadcasts: the bridge keeps each topic's oldest one the session has not \
         marked handled, and a relaunch that keeps the session offers the topic again from \
         there, in order, each row once. A session \
         opts into at most 32 topics: an `add` of a new one past that answers \
         `ERR topics full (max 32): drop one first` and changes nothing (every opt-in rides \
         the session across an update, which carries a bounded record).",
    ),
    // The TYPED RE-KEY (aterm-gui `shell_rekey::typed`): the live agent
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
         mark signed with the key, not before. THE TYPED UPGRADE: for a tab \
         whose shell does not sign this build's integration revision at the prompt it holds \
         — one from before LOADERS (`status integration_rev=frozen`), or a loader whose \
         prompt hook never runs, which no body pointer reaches — the file also names this \
         build's script folder and the session's body pointer, and the line sources this \
         build's loader, upgrading the shell in place — issued for such a tab when its \
         integration is `on` too, with the key it already signs with in the file (no key \
         moves, and nothing is authorized). Refused, nothing written: `ERR rekey \
         integration=on` (a working key is never moved: a shell that signs this build's \
         revision, whose next integration arrives through its pointer), `integration=off`, \
         `pending` (the re-key channel's own key waits for the shell's next prompt), `ERR pid \
         <n> is not the foreground process group`. `rekey withdraw` settles the waiting key by its file: \
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
         above `seen=`, not merely one nobody listed — plus broadcasts a full quota refused and, \
         across a seamless update (which keeps the count), the broadcasts still unhandled when \
         it was drawn, which it does not carry — and `pending=` the unlisted delivered \
         rows this reply did not carry. A DROPPED ROW IS NOT LOST: every one of this session's \
         records from `oldest_on_bus=@<off>` (the lowest offset ever delivered here) to \
         `bus_head=` is on the broker's durable log whether or not the ring still holds it, and \
         `inbox get @<off>` fetches it through the bridge — the offsets in between are shared \
         with the whole fleet, so most belong to other sessions (`ERR no such record`). \
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
         such record`. The read PARKS for the bridge: its `fetch` event wakes the bridge \
         at once, and the bridge's 2 s roster round finds one it missed; it is bounded at 10 s (`ERR timeout off=<n>`); with no bridge that \
         can read it answers `ERR fabric <state> off=<n>` at once; at most 8 reads park per \
         session (`ERR busy`). `ERR no such record off=<n>` is the ONE answer for an offset that \
         is not one of this session's records — another session's record, another face's, or nothing \
         there — and nothing says which.",
    ),
    v(
        "inbox seen",
        Write,
        Status,
        Session,
        "inbox seen <id> [handled|refused|deferred]: advance the HANDLED watermark (and ack)",
        "`OK seen=<id>`, and pushes `EVENT <local> inbox-seen <id> off=<n> floor=<f|-> \
         seen=<w>` on the events digest: `off=` is the named row's offset, and `floor=` the \
         ADDRESSED lane's refill floor — the lowest offset of an addressed row not yet \
         handled, else one past the highest delivered, `-` before the first — which a \
         broadcast never moves; the bridge refills the session from it when a seamless update \
         keeps the session but not its ring. `seen=` is the watermark itself, which releases \
         the BROADCASTS at or below it: the bridge offers a kept session every broadcast above \
         it again, from each topic's oldest. \
         A VERDICT IS A RECEIPT: with `handled|refused|deferred` the event also carries \
         `verdict=<v> kind=<k> from=<p>` for THE ROW THE ID NAMES (not every row the watermark \
         passes — a bare `inbox seen <id>` names no verdict for any of them), and a bridge \
         running with receipts on (the default; `--no-receipts` or `[fabric] receipts = false` \
         turns it off) then \
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
        "post to=<@<sid>[@<node>]|<principal>|say[:<topic>]> kind=<k> [opts] [--] <text>: send a message",
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
         Options lead and the first token that is not one begins the text; `--` ends them \
         outright, so text the sender did not write (a peer's mail, an issue) goes after \
         `--` and can never be read as `to=`/`kind=`/`re=`/`key=`/`len=`. \
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
         up later; a recipient who has TURNED receipts off never acks, so bound it). \
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
         once with `ERR fabric <absent|stalled|stale|disconnected> id=<n> <queued=1|no-bridge=1>`, and \
         WHICH \
         of those two tokens it carries is the instruction. `queued=1` says the message is STILL \
         IN THE OUTBOX and a bridge will publish it (a bridge exit is the ordinary relaunch \
         path, and `outbox` is a peek that removes nothing; a crash of the window is not, and a \
         session the PTY keeper recovers retires each post it lost `reason=not-carried`), so it \
         must not be read as `not sent` and re-posted — a `post` without `key=` carries no \
         idempotency key that would \
         collapse the duplicate, and one with it is still queued. `no-bridge=1` says \
         something NARROWER about the same queued \
         message, and the difference is the whole report an agent makes: this instance has no \
         `[fabric] command`, so no bridge exists to drain the outbox RIGHT NOW and none is \
         coming ON ITS OWN. It is not a verdict on the message. `aterm fabric on` (or `fabric \
         attach`, which arms the `[fabric] command` aterm.toml names) arms a supervisor and \
         that same outbox drains, so the honest sentence is \"queued, and \
         unpublishable until this instance has a bridge\", never \
         \"not sent\". Reporting it as lost is how a delivered task gets done twice. Further \
         posts only fill the queue until `ERR outbox full`. THE THIRD OUTCOME: \
         `ERR timeout id=<n>`, the `--wait` expiring with no landing reported. It \
         is queued exactly like the other two, and must not be re-posted either. THE FOURTH IS \
         THE ONE THAT IS NOT QUEUED: `ERR <reason> id=<n>` — `unroutable`, `ambiguous`, \
         `undeliverable`, `refused` (the broker refuses the record for good, e.g. a lane \
         this node's cap does not grant: the same post is refused again), or `subject-budget` \
         (the post would open a new subject — a new lane, or a broadcast topic past the last \
         quarter the node keeps for its sessions — past this node's lifetime budget of distinct \
         subjects) — is the bridge \
         RETIRING the post, and the wait is released with that verdict rather than a timeout. `outbox` then omits the row (it lists only rows that are \
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
         bridge refills from the persisted `inbox seen` floor. `ERR quota` past 64 unread rows from \
         one ATTESTED PEER — counted on the cap-forced `<src>` (the part after `@`, or the whole \
         `from=` when there is none) and NOT on the whole `from=` string, half of which the \
         sending node chooses — so one peer cannot evict a human's unread `task` under a burst of \
         `note`s by rotating pseudo-sids, and eviction never drops an `h-*` row ahead of an \
         agent's. A `topic=<t>` row whose topic the session's own set does not hold is \
         `ERR not-subscribed`, whatever the bridge's copy of the set says, and is not counted \
         in `dropped=`. `deliver <sid> landed=<post-id> off=<n>` is the \
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
        "link up rtt=<ms> | link down reason=<token> [rtt=<ms>] | link log <id>: the broker link",
        "BRIDGE-ONLY. The record behind `status`'s `fabric=`, `fabric_rtt_ms=` and \
         `fabric_link_age_ms=`: `up` with the round trip of the ack just received moves the \
         instance to `fabric=connected` and stamps the ack's time; `down` with a one-token \
         reason (`no-socket`, `refused`, `denied`, `no-ack`, `closed`, `cut-fleet`, \
         `cut-inbox` or `cut-say`: the broker answering and that one subscription cut, \
         `attach`, `expired`: the broker refusing this node's cap because its `exp=` passed, \
         renewed by minting the same ring again, `busy`: the broker at its connection limit, \
         `record`: the broker refusing this node's own will or \
         presence row for good, `protocol`: a broker in another astream protocol (another aterm \
         release), `subscribe`, `read`, `state-locked`: another bridge holds \
         this node's state dir and this one stands by, or `endpoint-grammar`: this instance \
         lacks a verb the bridge needs, so the two are different builds) moves it to `fabric=stalled` and wakes every parked `post --wait` so it answers \
         `ERR fabric stalled id=<n> queued=1` instead of sitting out its timeout. The bridge \
         sends it on every change, after an ack that moved the round trip by more than 2x, and \
         otherwise after an ack at most once per 2 s (so the age stays honest on a busy link) \
         — never on a timer, never as a heartbeat. `OK`, or `OK stale=1` when the report came \
         from a bridge incarnation that no longer owns the link (a ghost lane; the report is \
         dropped, the live bridge's state stands). `log <identity>` (astream's \
         `<32 hex>.<generation>`, sent on every attach before any `deliver`) names the bus log \
         the offsets it hands this instance index: a CHANGE from the one it named before is a \
         log switch (bus.log replaced, repaired or restored, or another host's broker), and every \
         session forgets the offsets it holds — its `off=` dedup, the `inbox get @<off>` reads it \
         answered (one still parked answers `ERR fetch failed reason=log-replaced`) and the \
         offset threading of the rows and posts it already holds — answering `OK forgot=1`; \
         otherwise `OK`; a log switch also starts the bus mirror's cursors over. `mirror \
         on|stopped|capped [reason=<token>]` is the bus mirror's state (session log phase 4, \
         only from a bridge whose `hello` said `log=turns`), which `fabric status` prints as \
         `mirror=` beside `bridge_log=turns`. `ERR usage` for anything else.",
    ),
    // `hello`: the bridge's first word on its lane — which lane grammar it
    // speaks and which build it is. Bridge-only like `link`: what it records is
    // what `fabric status` says about the bridge, and an Owner client that
    // could say it would name a build the bridge is not.
    va(
        "hello",
        Write,
        Status,
        Meta,
        BridgeOnly,
        "hello lane=<n> [build=<token>]: the bridge names its lane grammar and its build",
        "BRIDGE-ONLY. The bridge's first word on its verb lane: the LANE GRAMMAR it reads and \
         writes (`lane=`, its build's `BRIDGE_LANE`) and the build it is (`build=<version>+g<commit>`). \
         Answered `OK lane=<n> build=<b>` with this instance's own, so each side speaks the older \
         lane of the two; any other `key=value` is read past, so a later bridge can say more. \
         Recorded for the bridge incarnation that owns the link and shown by `fabric status` \
         (`bridge_lane=`, `bridge_build=`, beside this instance's `build=`), and a bridge of \
         another build than its instance is logged as one. An instance from before the verb \
         answers `ERR unknown verb`, which a bridge reads as lane 0, the grammar as it stood \
         before the handshake. `OK stale=1` from a bridge incarnation that no longer owns the \
         link (nothing is recorded); `ERR usage` for a missing or malformed `lane=` or `build=`.",
    ),
    va(
        "hold",
        Write,
        Status,
        Meta,
        OwnerOnly,
        "hold <sid> on|off [reason=<pct>] [origin=fleet|local]: the drive halt, local or fleet",
        "`OK hold=<0|1>`: whether the session is held AFTER the act. TWO ORIGINS, EACH ITS \
         OWN SLOT, AND THE CONNECTION CHOOSES: a session holds a set of origins, `on` sets the \
         slot of the origin it names and `off` lifts that slot and no other, and the session is \
         held while either stands — so a lift that leaves the other origin's hold standing \
         answers `OK hold=1`. From the inherited bridge connection this is the FLEET halt: \
         `origin=fleet` by default, either origin accepted; a fleet halt on a session the owner \
         has held stands BESIDE the owner's hold, and its lift leaves the owner's standing. From \
         the Owner token — the local human's own credential — it is the LOCAL halt: \
         `origin=local` is the default and the only origin accepted (`origin=fleet` is `ERR \
         denied`), an Owner `on` beside a fleet hold sets the owner's own slot, and an Owner \
         `off` that finds only an `origin=fleet` hold standing is `ERR denied`. Owner scope is \
         what every in-session client already holds, so a fleet \
         halt an injected agent could lift or replace locally would be no halt at all; a local \
         halt is the owner's own stop and the owner's own to lift — and NOT a containment stop: \
         Owner is the scope every in-session client holds (`aterm-ctl @self` is Owner), so the \
         halted session's own agent can lift a local hold with `hold <sid> off`; the halt an agent \
         cannot lift is the fleet's. An edge token is `ERR denied` \
         either way, and a selector is rejected: the session is the argument. While on, every \
         PTY-reaching verb resolving to that session answers `ERR halted reason=<r> \
         origin=<local|fleet>` (the fleet's hold when both stand: the one the session's own \
         agent cannot lift) from ANY scope — `send key ctrl feed feed-bin paste paste-bin mouse \
         pointer resize focus signal restart reset turn close invoke hwkey pane tab confirm \
         askpass` — a \
         TRANSIENT class beside `ERR busy`, so existing back-off code already does the right \
         thing. That enumeration is NOT maintained by hand: it is pinned verb-for-verb against \
         the set this build enforces (`fabric::is_pty_reaching`), so a verb a halt refuses cannot \
         be missing from it and a verb named here cannot be answerable. `focus` is in that set \
         because it writes the DEC 1004 focus reports to the PTY; `pointer` is, because the \
         terminal REPORTS pointer motion to the program under DEC 1000/1002/1003; `hwkey` is, \
         because it posts a real NSEvent and so takes the same path a physical keypress does; \
         `invoke` is, because `invoke Paste` writes the clipboard into the front tab's PTY; \
         `pane` is, because it moves which pane — and so which session — the keyboard drives; \
         `tab` is, because `tab close [N]` RETIRES a session exactly as `close` does; \
         `confirm` is, because `confirm yes` \
         runs the close an `invoke` left waiting in the window. `invoke`, `hwkey`, `pane`, \
         `pointer`, `tab` and `confirm` resolve no session, so they are refused while ANY \
         session on the instance is \
         held — including the aimed `@<sid> tab …` form, which drives the window HOSTING that \
         session and therefore answers `ERR halted` whenever any session of that instance is \
         held, not only the one it names. `post`, `inbox seen`, \
         `meta set`, `lease` and every read verb stay answerable, and the physical keyboard is untouched: a \
         halt stops drivers, not humans. When the bridge connection closes, a hold that stands \
         stays, and every other session that bridge ever delivered to or held KEEPS RUNNING with \
         `status hold_advisory=fabric-lost`; under `[fabric] \
         hold_on_bridge_lost = true` the instance holds each of them with `reason=fabric-lost \
         origin=fleet` instead — the halt must not depend on a killable process staying alive — \
         and, being fleet-origin, only a reconnecting bridge's `hold off` lifts it, leaving an \
         owner's local hold beside it standing. An \
         Owner-issued hold does not make a session bridge-governed: a bridge dying afterwards \
         touches what IT governed, not what the owner did. Every hold, both origins, SURVIVES a seamless update: \
         the new process puts each back in its slot, reason and origin as they stood, before the adopted \
         session answers a single verb (its `timeline` row says `carried=1`), and a hold that \
         moves while the update is taking over makes that attempt stand down, every session \
         left as it was (an automatic update tries again later), rather than be lost. In the \
         last instant of a takeover — once the update has compared the holds and is handing \
         the session over — a `hold` waits for it instead: it is answered as usual if the \
         attempt stands down, the connection closes with no answer if the new process took \
         over (retry: the new process answers), and past 10 s it is `ERR busy \
         update-committing` with nothing moved. Never `OK` for a hold the new process did not \
         adopt. A quit ends the sessions, so a relaunch after one carries nothing; a crash the PTY \
         keeper recovers a session from carries its holds as an update does, as the session's \
         last META delivered to the keeper named it. The pump attempts a sample every quarter \
         second; a busy lock, a full queue or a failed link can leave an older sample, so a \
         hold changed since that sample may not survive the crash.",
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
         never stranded). A drain stopped by the budget is resumed by the \
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
    // `log-take` / `log-landed`: THE BUS MIRROR's two halves of one batch
    // (session log phase 4, design §5.5). Bridge-only like `outbox`: the
    // records they hand out are every session's, and the landing they take
    // moves the cursor a seamless update carries.
    va(
        "log-take",
        Read,
        Bytes,
        Meta,
        BridgeOnly,
        "log-take [n=<1..64>] [tier=wire|meta] [key=<hex>]: the bus mirror's next slots",
        "BRIDGE-ONLY, and served only to a bridge whose recorded `hello` said `log=turns` \
         (`[fabric] log = \"turns\"`; the default `off` sends neither verb; any other bridge is \
         answered `ERR mirror off`). `OK <nbytes> \
         slots=<k>[ more=1]` then that many bytes: one `<sid> <ep>:<seq>:<ms> <v=2 body>` line \
         per slot, oldest first per session, sessions round robin, at most `n` (64, the \
         default) in all. A slot is a bus-class record of the session log (the turn class, the \
         need-bearing steps, `mark seen`), a cover (`op=gap why=evicted|reset|dropped n=<k>`, \
         the `k` records from its `ms=` the ring lost before a take reached them, exact), or a \
         snapshot (`op=snap`); `<ep>:<seq>:<ms>` is where the session lands once that slot and \
         every slot before it are on the bus. Bodies are encoded at the bus tier: no host or \
         content key, `tier=meta` adds the program and model words, and an edge's sid reads as \
         `by=e<8 hex>`, a hash keyed by `key=` (the node's, minted once and kept in the \
         bridge's pin, never derived from its caps). A \
         take moves nothing: `log-landed` does. A timeline busy at the take is skipped (`more=1`); \
         a closed session's unlanded tail is taken until it lands. `OK 0 slots=0 stale=1` to a \
         lane that no longer owns the link; `ERR busy adopting=1` while a seamless successor has \
         not yet adopted its sessions.",
    ),
    va(
        "log-landed",
        Write,
        Status,
        Meta,
        BridgeOnly,
        "log-landed <sid> <ep>:<seq>:<ms>...: the bus mirror's slots landed",
        "BRIDGE-ONLY. Moves each named session's mirror cursor to the given one (monotone: a \
         cursor at or below it, or of another seq space, is `stale`), answered `OK <word> ...`, \
         one word per pair in order: `ok`, `stale`, `done` (a closed session's tail is all on the \
         bus and is forgotten), `fenced` (a seamless update's export drew the cursor: keep the \
         slots and land them again after the update commits or stands down), `adopting` (a \
         successor has not yet adopted the session: keep them), `gone` (no such session, after \
         adoption), `ahead` (past anything the session's log numbered). It writes nothing to a \
         PTY. `OK stale=1` to a lane that no longer owns the link; `ERR mirror off` to a bridge \
         whose recorded `hello` did not say `log=turns`.",
    ),
    // sessions, presence & capability — the OwnerOnly access declares the owner
    // gate IN the table (the dispatch reads it, no hardcoded verb list). `who`
    // keeps its `Read` op-class (a fleet-wide presence readout) yet is Owner-gated:
    // op-class and scope-gate are orthogonal, which is exactly why `access` is a
    // separate field.
    // `fabric`: the bridge SUPERVISOR of a running instance. Session-independent
    // (answered before session resolution, so a windowless
    // instance can be attached), Owner-only because arming the supervisor
    // starts the process that holds `Scope::Bridge` — and which process that is
    // is the config's to say, not the caller's (`fabric attach` arms only what
    // aterm.toml names).
    va(
        "fabric",
        Owner,
        Status,
        Meta,
        OwnerOnly,
        "fabric status|attach [<command...>]|retire <sid>...: the bridge of a RUNNING instance",
        "`fabric status` -> `OK state=<absent|connected|stale|stalled|disconnected> supervised=<0|1> \
         command=<pct|-> reason=<token|-> rtt_ms=<n|-> link_age_ms=<n|-> build=<token> \
         bridge_lane=<n|-> bridge_build=<token|-> lane_faults=<n>`: `state=` is the \
         link (`status`'s own `fabric=` token — the bridge's BROKER link, `stalled` while a \
         bridge is attached but that link is down), `reason=` why it is down (`starting` before \
         the bridge's first report, `no-socket`, `refused`, `denied`, `no-ack`, `closed`, \
         `cut-<face>`, `attach`, `expired`, `busy`, `record`, `protocol`, `subscribe`, `read`, \
         `bridge-lost`; `-` when up), `rtt_ms=`/`link_age_ms=` \
         the last acknowledged round trip and its age exactly as `status` carries them, \
         `build=` this instance's own build and `bridge_lane=`/`bridge_build=` what its bridge \
         said it is in `hello` (`-` before it, and from a bridge older than the handshake) — a \
         `bridge_build=` other than `build=` is a bridge of another build than its instance — \
         `lane_faults=` how many of the bridge's lines this instance could not parse (the mail \
         they carried is held back on the bus), `supervised=` \
         whether a supervisor is armed in this process (the latch that flips `post`'s refusal \
         from `no-bridge=1` to `queued=1`), `command=` the argv it runs — or, unarmed, the one \
         the instance was launched with. `fabric attach [<command...>]` arms the supervisor NOW, \
         without a relaunch, and ONLY WITH WHAT THE INSTANCE'S CONFIG NAMES: with no argument the \
         `[fabric] command` its aterm.toml names now (the file is read again, so a command `aterm \
         fabric on` wrote after launch counts), else the one it was LAUNCHED with (`ERR fabric \
         no command` when neither names one); given words, they are the argv, split on \
         whitespace exactly like the config string and never through a shell, and must BE one of \
         those two commands — any other is `ERR fabric not configured command=<pct>`, nothing \
         armed: the Owner token every in-session client holds does not choose which process \
         holds `Scope::Bridge`. `OK attached command=<pct>` means the supervisor \
         thread started; the bridge connects asynchronously, so watch `fabric status` / `status \
         fabric=` for `connected`. ONCE PER PROCESS: the supervisor relaunches its child forever \
         and has no stop handle, so a second `attach` is `ERR fabric already supervised \
         command=<pct>` naming the argv that is running — the request's argv was NOT applied. \
         Because that latch is one-way, `argv[0]` is PRE-FLIGHTED before it closes: a program \
         that does not exist, is not a regular file, or has no execute bit (a bare name: found \
         nowhere on `PATH`, the same `PATH` the child inherits) is `ERR fabric not executable \
         program=<pct> reason=<not-found|not-a-file|no-exec-bit|not-on-path>`, arms nothing, \
         and leaves the slot open for the corrected command (fix aterm.toml, attach again); a \
         program that passes and still fails to start is the supervisor's to retry (back-off to \
         30 s), logged. Owner ONLY — an edge token and the bridge connection itself are `ERR \
         denied` — and Owner arms only what the config names: the bridge is the process that \
         holds `Scope::Bridge`, not a party to arming it. `fabric retire <sid>...` asks THIS instance's bridge to retire the presence rows of sessions nobody hosts — the operator's `aterm fabric doctor --retire-ghosts` with the bridge up: a sid a session here carries is `ERR hosted <sid>` and nothing is requested; otherwise `OK requested=<n>`, the request rides the `sessions` push as `EVENT * fabric-retire <sid>`, and the bridge re-checks hosting and publishes `exited` through its OWN producer sequence, so no second process ever writes the sequence file.",
    ),
    // `sessions` is the fleet roster. Its trailing tokens are ADDITIVE (old clients
    // key on the sid field and ignore the tail); `window=`/`active=`/`wfocus=` come
    // from ONE main-thread hop per call.
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
         `cd ~/proj && exec claude` reads `claude`; a line that OPENS with a shell keyword is \
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
         be: the environment of a program the shell itself started names the \
         PATH the shell exports. A live reading wins and lowers the adoption mark; a frozen \
         one is the shell's only once a SECOND job agrees (one job's `PATH=... cmd` \
         override reads frozen too) and never raises the mark; a shell whose children have \
         not been read keeps the mark it was adopted with (a shell spawned by a build before \
         the self-healing sessions is carried as `frozen`). \
         `path_evidence=` (below) says which. A tab whose foreground is an agent is \
         never the place to type a shell line into. After it: \
         `program=<name|-> agent=<word|-> \
         agent_detail=<pct|-> agent_rev=<n> agent_since_ms=<ms> agent_gen=<e.s|-> \
         agent_fp=<hex16|->`, the foreground program and \
         the server's agent verdict exactly as `status` prints them, read from the session \
         (no lock, no hop), so one read names every agent tab and what it waits on. Then \
         `human_ms=<ms|->` (since a person last typed, clicked or scrolled in it through a \
         window, as \
         `status` prints it); then \
         `role=<pct|-> attention=<pct|-> user_title=<pct|->`, the typed metadata `meta` \
         prints, so a fleet reader classifies every session from this one read; and \
         `supervisor=<pct|->`, the live `meta set supervisor` claim, and \
         `autopilot=<off|cruise|land|->`, the agent tab's effective Autopilot as `status` \
         prints it. Then the OWNER'S \
         COLUMNS: `path_evidence=<measured:<ms>|unconfirmed:<ms>|carried|->` \
         - how `path=` was learned (read that many ms ago from a child of the shell; \
         `unconfirmed` = one job read frozen, which is not yet taken as the shell's; the \
         adoption mark, never measured; `-` = a shell a self-healing build spawned); \
         `copy=<managed|foreign|->` - for `claude`/`codex` in front, whether the build it \
         runs is aterm's managed one (from the store) or a foreign copy (the native \
         installer's `~/.local/share/claude/versions/<v>`, a brew cask); \
         `upgrade=<-|<state>/<to>/<why>/<age>>` - the live agent upgrade of the \
         conversation in the tab (`aterm harness upgrade --status` has the rest): state \
         `pending`, `announced`, `restarting`, `deferred`, `skipped`, `stalled` or `done`, \
         the target version, the wait or stall (`settling`, `attended`, `awaiting-ready` \
         for an agent that has not answered READY, `superseded` for one that answered, then \
         got more work before the restart, and is asked again when that turn ends, \
         `next-round:<span>` for a round resting before it asks again, `ready` for one \
         that gave up and acts on a late READY, `overdue` ...), \
         and how long the session has been behind (for `done`, \
         how long ago it finished). Last, `land_at=<unix-s|-> \
         task=<open|landed|done|spent|theirs|->`, the agent tab's timed Cruise and its task's \
         face as `status` prints them. One main-thread hop per call, not per session; the \
         client `ls` \
         relays these lines verbatim and `windows` folds them per window. The menu-bar \
         status item's fleet scan reads this on every open under a 2 s per-peer budget, so \
         a peer whose main thread cannot answer inside it drops out of that menu rather \
         than being listed from the registry alone — one hop per open, never a poll. \
         `sessions bridge` is the Fabric bridge's identity-only roster: `OK <n>` then \
         `<local> <sid> nonce=<hex32>` per session, sorted by local id. It takes \
         one registry snapshot and reads no terminal, metadata, timeline or \
         window placement. \
         `sessions status` is the bridge's one-hop status snapshot: `OK <n>` then one \
         `<local> <sid> <nonce> sid=<local> revision=<n> hold=<0|1> \
         hold_origin=<fleet|local|fleet,local|-> detail=<pct|-> agent=<word|->` row per readable \
         session; `hold_origin=` says whose holds stand (`fleet,local`: the fleet's halt beside the \
         owner's own), which `status` does not, so the bridge never lifts the owner's own. The sid and nonce fence local-id reuse; an \
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
         ids strictly greater (page with the last id you saw; the ids go on rising across an \
         aterm self-update, though the rows before it stay behind); the ring is MEMORY-ONLY and \
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
        "`family [<sid>]` -> `OK <n>` then `self <sid> <state> <title>`, one `parent` row \
         (`parent - - -` for a root) and a `child` row per direct child; titles pct-encoded. \
         Naming a <sid> is Owner-only. For the Owner, `pushes`/`pushed-by`/`pulls`/`pulled-by` \
         rows follow, one per connected peer, in the same shape; a scoped edge gets none and \
         reads `edges`",
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
        "`grant <src-sid> <op>` mints an edge from <src-sid> onto this session and replies \
         `OK <token-hex>` (the caller delivers it); <op> is read-screen, write-input, signal, \
         config-write, clipboard-write or derive-loop. A <src-sid> that is one of the session \
         log's actor words (human ctl harness bridge aterm vendor -) is refused `ERR reserved`, \
         and the log names an edge's acts by its source only when that is a minted sid (`-` \
         otherwise). `connect` sets whole connections; `revoke` removes",
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
         write-input=<hex> signal=<hex>` naming every op the connection now holds, \
         minted now or before; a src= that is a session log actor word is refused \
         `ERR reserved` (Owner only)",
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

/// THE BRIDGE LANE'S GRAMMAR GENERATION: what the bridge and the instance serving
/// it each say in `hello` (`lane=`), so the two can tell which of them reads
/// less of the lane — the bridge binary is whatever `[fabric] command` names,
/// and nothing makes it the instance's own build.
///
/// THE RULE THE NUMBER CARRIES. A change that one side would answer with `ERR
/// usage` from the other — a new `deliver` field, a closed word added to a
/// list the other side checks, a new line in a frame the other side parses
/// closed — bumps it, and the side that made it writes the new thing only to a
/// peer whose `hello` named this lane or a later one. Lane 0 is a peer from
/// before the handshake (an instance answers `hello` with `ERR unknown verb`, a
/// bridge never sends one): the lane as it stood at 1, `hello` aside. An
/// additive change the other side already reads past (a `key=value` column
/// after a roster's identity, a bodiless outbox line after every post) needs no
/// bump.
///
/// THE LANES SO FAR: 1, the lane as `hello` found it. 2, `link log <identity>`
/// (the bus log's identity, told on every attach before any `deliver`), which
/// an instance at lane 1 answers `ERR usage`.
pub const BRIDGE_LANE: u32 = 2;

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

/// [`CONTROL_BUSY_REPLY`] as a literal, so its framed line is one `concat!`.
macro_rules! control_busy_reply {
    () => {
        "ERR control server busy; retry"
    };
}
/// The control socket's BUSY reply. The listener answers it to a connection it
/// cannot admit (every request lane at work, or the open-connection cap
/// reached) and then drops the connection; a lane answers it to a WAIT none of
/// its lanes may take (every wait lane taken and the request lanes' wait budget
/// spent) and keeps the connection. Either way nothing was served, and the
/// request may be sent again. Clients match its `ERR control server busy`
/// prefix (`aterm-agent`'s `TURNED_AWAY`, `aterm ctl`'s report).
pub const CONTROL_BUSY_REPLY: &str = control_busy_reply!();
/// [`CONTROL_BUSY_REPLY`] framed as the wire carries it.
pub const CONTROL_BUSY_LINE: &str = concat!(control_busy_reply!(), "\n");

/// What leads a [`host_gone_line`], up to its pid.
const HOST_GONE_PREFIX: &str = "ERR gone pid=";

/// THE FORWARD'S HOST WENT AWAY: the line an aterm that forwards a connection to the aterm hosting its
/// session — a sibling the person opened, a child it spawned — writes in place
/// of a bare end of stream when that host hung up first and no longer serves at
/// `sock`, the socket it was reached at: an update's Commit `_exit`, a crash, a
/// quit. `ERR gone pid=<host> sock=<path>`, the path last (it may hold spaces);
/// the connection closes after it. The leading `gone` classifies it as every
/// `ERR <token>` does: transient.
///
/// It is the host's HANG-UP, typed: a bare EOF from a forwarder that still
/// answers reads as a deliberate end (`subscribe` exits 0 "session over" while
/// the session lives on in the host's successor; `turn` exits 1, the refusal code
/// a caller retries). A client reads it as its own server going away — a hang-up
/// of `pid`, whose successor republishes the session's graph entry beside
/// `sock` — wherever it arrives: in place of a reply, or as a stream's last
/// line, never relayed as a frame. It is never `ERR forward …`, which says the
/// request did not reach the host at all.
#[must_use]
pub fn host_gone_line(pid: u32, sock: &str) -> String {
    format!("{HOST_GONE_PREFIX}{pid} sock={sock}\n")
}

/// The host a [`host_gone_line`] names — its pid and the socket it was reached
/// at — or `None` for any other line. The line's own `\r\n` is ignored.
#[must_use]
pub fn host_gone(line: &str) -> Option<(u32, &str)> {
    let rest = line
        .trim_end_matches(['\r', '\n'])
        .strip_prefix(HOST_GONE_PREFIX)?;
    let (pid, sock) = rest.split_once(" sock=")?;
    if pid.is_empty() || !pid.bytes().all(|b| b.is_ascii_digit()) || sock.is_empty() {
        return None;
    }
    Some((pid.parse().ok()?, sock))
}

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
/// fence the server answers with `ERR usage`.
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

/// THE ROWS FENCE: `turn if-rows=<first>-<last>:<hex16>` (design record
/// 2026-09-28 §3.2 C7b) — type only while the visible rows `first..=last`
/// (0-based grid rows) still hash to `hash`, checked where `if-gen=` is.
/// `if-gen=` refuses on ANY output since the read, so a screen that repaints
/// itself (a spinner, a ticking timer) would refuse every press. This fence names only the rows the typing lands
/// in and whose change matters (Claude Code's input box: a person's keystroke
/// echoed there, a box drawn over it), and lets the rest move.
///
/// ONE DEFINITION for both ends, as [`ScreenGen`] is: the server hashes its
/// rows and a client the rows a read served it through [`RowsFence::hash_rows`]
/// — FNV-1a-64 over each row followed by `\n`, the definition the screen's own
/// `hash=` stamp uses over every row — so the two cannot disagree on bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowsFence {
    /// The first grid row hashed.
    pub first: usize,
    /// The last grid row hashed (inclusive).
    pub last: usize,
    /// FNV-1a-64 of those rows ([`RowsFence::hash_rows`]).
    pub hash: u64,
}

impl RowsFence {
    /// Parse the wire spelling `<first>-<last>:<hex16>`: two unsigned
    /// decimals with `first <= last`, and exactly sixteen hex digits; any
    /// other shape is `None`.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        let (range, hex) = value.split_once(':')?;
        let (first, last) = range.split_once('-')?;
        let decimal = |s: &str| {
            (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                .then(|| s.parse::<usize>().ok())
                .flatten()
        };
        let (first, last) = (decimal(first)?, decimal(last)?);
        if first > last || hex.len() != 16 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let hash = u64::from_str_radix(hex, 16).ok()?;
        Some(Self { first, last, hash })
    }

    /// FNV-1a-64 over `rows`, each followed by `\n` — over every visible row,
    /// the screen's `hash=` stamp.
    #[must_use]
    pub fn hash_rows<'a>(rows: impl IntoIterator<Item = &'a str>) -> u64 {
        let mut h = aterm_hash::Fnv1a64::new();
        for row in rows {
            h.write(row.as_bytes());
            h.write(b"\n");
        }
        h.finish()
    }
}

impl core::fmt::Display for RowsFence {
    // Plain `write_str`s, not `write!`: see `crate::trust_fmt` for why this
    // crate builds no runtime-argument `format_args!`.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        f.write_str(&self.first.to_string())?;
        f.write_str("-")?;
        f.write_str(&self.last.to_string())?;
        f.write_str(":")?;
        let mut digits = [0u8; 16];
        for (i, d) in digits.iter_mut().enumerate() {
            let nibble = (self.hash >> (60 - 4 * i)) & 0xf;
            *d = HEX[usize::try_from(nibble).unwrap_or(0)];
        }
        f.write_str(core::str::from_utf8(&digits).unwrap_or("0000000000000000"))
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
/// leading `@<selector>` is skipped, and the `image read` / `cast frames` / `cast
/// drift` SUB-FORMS flip their base verb's framing to `Lines` (bare `image`
/// rasterizes to a status line; bare `cast` is a byte body). Unknown/plain verbs
/// are `Status`.
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
    // `cast drift` answers `OK <n> verdict=…` + n `run`/`row`/`cursor` lines. A
    // client that read it as the bare verb's byte count would wait for `<n>`
    // BYTES; one that knows no `drift` (an older server answers the bare cast)
    // is told apart by the header's `verdict=` (aterm-ctl's `drift` module).
    if verb == "cast" && matches!(sub, Some("frames" | "drift")) {
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
    // same hazard: under Lines framing aterm-ctl refuses the reply as a
    // `malformed response header`.
    if verb == "identities" && sub == Some("forget") {
        return Status;
    }
    // `topic add <t> …` / `topic drop <t>` answer ONE status line
    // (`OK <t> since=<..> added=<0|1>`, `OK <t> dropped=<0|1>`), not the
    // `OK <n>` + `topic <t> since=<..>` rows the bare form and `topic ls`
    // answer. The same flip as `inbox seen`, and the same hazard: under Lines
    // framing the client reads the TOPIC as a row count and reports a malformed
    // header.
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
    // `theme list` answers `OK <n>` + n theme rows; the bare `theme` and every
    // other sub-form answer one status line.
    if verb == "theme" && matches!(sub, Some("list" | "palette")) {
        return Lines;
    }
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

/// The verbs whose reply the server answers as JSON under `--json`/`json` (most
/// wrap it in `json_ok`, `OK 1` and one object; `log` answers one object per
/// record), and which therefore frame as [`Framing::Lines`] under the flag.
///
/// This list MIRRORS the server's `json_ok` call sites (`aterm-gui`'s
/// `cmd_*_json` helpers). It is the one framing input that is not derivable from
/// [`VERBS`], because json-capability is a property of the server's handler, not
/// of the verb row. A missing entry makes the client read the `OK 1` header as
/// Status and drop the JSON body. Add a verb here in the same change that adds
/// its `_json` handler.
pub const JSON_CAPABLE_VERBS: &[&str] = &[
    "text", "screen", "cursor", "dims", "blocks", "edges", "grants", "metrics", "privacy", "log",
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
/// order — `help --full`, and the `aterm help introspection` manual.
pub fn catalog_lines_full() -> impl Iterator<Item = String> {
    VERBS.iter().map(|s| catalog_row(s.name, &s.help_line()))
}

#[cfg(test)]
mod tests {
    /// The forward's host-gone line reads back what it wrote — a socket path
    /// with spaces included, as it is last — and no other line reads as one.
    #[test]
    fn the_host_gone_line_round_trips_and_nothing_else_reads_as_it() {
        use super::{host_gone, host_gone_line};
        let sock = "/Users//a b/Library/Application Support/aterm/aterm-4242.sock";
        let line = host_gone_line(4242, sock);
        assert_eq!(line, format!("ERR gone pid=4242 sock={sock}\n"));
        assert_eq!(host_gone(&line), Some((4242, sock)));
        for other in [
            "ERR gone fishing",
            "ERR gone pid= sock=/s",
            "ERR gone pid=+1 sock=/s",
            "ERR gone pid=12 sock=",
            "ERR forward: could not reach the aterm that hosts that session",
            "OK gone pid=1 sock=/s",
        ] {
            assert_eq!(host_gone(other), None, "{other}");
        }
    }

    /// THE ROWS FENCE's wire spelling reads back what it wrote, refuses every
    /// other shape, and its hash is FNV-1a-64 over the rows joined by `\n` —
    /// the definition of the screen's `hash=` over all of them.
    #[test]
    fn a_rows_fence_round_trips_refuses_bad_shapes_and_hashes_the_rows_joined() {
        use super::RowsFence;
        let f = RowsFence {
            first: 12,
            last: 14,
            hash: 0x00ab_cdef_0123_4567,
        };
        assert_eq!(f.to_string(), "12-14:00abcdef01234567");
        assert_eq!(RowsFence::parse(&f.to_string()), Some(f));
        for bad in [
            "",
            "12-14",
            "14-12:00abcdef01234567",
            "12-14:abc",
            "a-b:00abcdef01234567",
            "12:00abcdef01234567",
            "12-14:00abcdef0123456g",
            "-1-3:00abcdef01234567",
        ] {
            assert_eq!(RowsFence::parse(bad), None, "{bad:?}");
        }
        assert_eq!(
            RowsFence::hash_rows(["ab", "", "❯ c"]),
            aterm_hash::fnv1a_64("ab\n\n❯ c\n".as_bytes())
        );
        assert_ne!(RowsFence::hash_rows(["ab"]), RowsFence::hash_rows(["ab "]));
    }

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
        assert_eq!(framing_of("cast", "@s-a cast drift"), Lines);
        assert_eq!(framing_of("cast", "cast drift max_runs=4 rows=10"), Lines);
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
        assert_eq!(framing_of("resizes", "resizes"), Lines);
        assert_eq!(framing_of("resizes", "@s-a resizes 20 since=4"), Lines);
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
            ("hello", Write, Status, Meta, BridgeOnly),
            // Owner-class, not bridge-only: the local owner's halt rides the Owner
            // token; the fleet hold is kept bridge-issued in the handler.
            ("hold", Write, Status, Meta, OwnerOnly),
            ("outbox", Read, Bytes, Meta, BridgeOnly),
            ("outbox sent", Write, Status, Meta, BridgeOnly),
            // The bus mirror's (session log phase 4, ruling Q11): the take a
            // byte frame whose `OK <nbytes>` a Bytes reader counts, the
            // landing a status line.
            ("log-take", Read, Bytes, Meta, BridgeOnly),
            ("log-landed", Write, Status, Meta, BridgeOnly),
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
        assert_eq!(framing_of("log-take", "log-take n=32 tier=wire"), Bytes);
        assert_eq!(framing_of("log-landed", "log-landed s-a 1:4:2"), Status);
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
    /// why a `--headless` instance answers it at all; `Meta` rejects a selector; `Read` because every
    /// fact it aggregates is one `status` already exposes a session at a time, or
    /// an instance-level fact about this Mac any program running as this user can
    /// read for itself (the `debugger` row), so the verb introduces no new
    /// authority; `Lines` because it replies `OK <n>` plus n rows.
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
    }

    /// The `theme` rows carry the class each sub-form needs (the read forms
    /// Read, the runtime preview lane Write, the forms that rewrite `aterm.toml`
    /// ConfigWrite), and `theme list` alone flips to Lines framing. Selector-
    /// aware, like every other sub-form rule.
    #[test]
    fn theme_rows_carry_their_classes_and_only_list_is_lines() {
        for (name, op) in [
            ("theme", Read),
            ("theme preview", Write),
            ("theme set", ConfigWrite),
        ] {
            let s = spec(name).unwrap_or_else(|| panic!("{name} is in the table"));
            assert_eq!(s.op, op, "{name} op-class");
            assert_eq!(s.framing, Status, "{name} row framing");
            assert_eq!(s.target, App, "{name} target");
        }
        let session = spec("theme session").expect("theme session is in the table");
        assert_eq!((session.op, session.target), (Write, Session));
        assert_eq!(framing_of("theme", "theme list"), Lines);
        assert_eq!(framing_of("theme", "@s-1 theme list"), Lines);
        assert_eq!(framing_of("theme", "theme palette"), Lines);
        assert_eq!(framing_of("theme", "@3 theme palette session"), Lines);
        for request in [
            "theme",
            "theme status",
            "theme session",
            "theme session set Hearth",
            "theme session clear",
            "theme preview Nord ttl=5",
            "theme revert",
            "theme commit",
            "theme set Nord",
        ] {
            assert_eq!(framing_of("theme", request), Status, "{request}");
        }
    }
}
