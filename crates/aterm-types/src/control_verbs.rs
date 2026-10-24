// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The control-protocol verb table: the typed source of truth for every control-socket verb.

/// The longest control request line the server reads, its newline not counted (a frame's body is bounded on its own).
pub const MAX_REQUEST_LINE: usize = 64 * 1024;

/// The longest task a worker is given (`spawn agent=<word> … len=<n>`, its task the n raw bytes after the line).
pub const MAX_TASK_BYTES: usize = 64 * 1024;

/// The longest `timeout=` a `turn` over another Mac's drive takes (the step-in).
pub const DRIVE_TURN_MAX_MS: u64 = 22_000;

/// The line a server answers a worker's spawn head (`spawn … agent=<word> len=<n>`) with when it will read the n bytes of task that follow, its newline included.
#[must_use]
pub fn task_frame_go(len: usize) -> String {
    format!("GO {len}\n")
}

/// Why a request line past [`MAX_REQUEST_LINE`] is refused, in the server's `ERR` line and in the client's refusal before it sends one.
#[must_use]
pub fn request_line_too_long() -> String {
    format!("request line too long (max {MAX_REQUEST_LINE} bytes)")
}

/// Why a selector that names nothing is refused, in the server's `ERR` line and in the client's refusal before it sends one.
pub const EMPTY_SELECTOR: &str = "ERR usage: empty or malformed selector: an @ names a session \
     (@self, @<sid>, @<n>; @. is the active tab)";

/// Whether a selector's body (what follows its `@`) names something.
#[inline]
#[must_use]
pub fn selector_body_names_something(body: &str) -> bool {
    !body.is_empty() && !body.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// The byte cap of one keyed attention entry (`meta set attention owner=<k> <text>`), after trim.
pub const META_ATTENTION_KEYED_MAX: usize = 200;

/// A verb's authority class.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OpClass {
    /// Pure observer / view-state control (incl. `subscribe`, the push face of read).
    Read,
    /// Injects the human input vocabulary the driven program observes, plus the app-drive verbs (`turn`/`spawn`/`close`/`tab`/`invoke`).
    Write,
    /// The out-of-band `signal` class.
    Signal,
    /// Rewrites the durable on-disk config (`settings set|unset` → `aterm.toml`).
    ConfigWrite,
    /// Moves the selection onto the system clipboard (`copy`, the exfil boundary).
    ClipboardWrite,
    /// Owner-only privilege verb or build/meta verb — no op-class; gated separately.
    Owner,
}

/// The connection-scope gate a verb needs, orthogonal to its op-class.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Access {
    /// Normal: authority is exactly `op`, checked per resolved target.
    Scoped,
    /// Non-sensitive build/meta provenance and instance posture, answered for any authenticated scope before target resolution (`version`/`update`/`help`/`verbs`/ `privacy`).
    AnyScopeMeta,
    /// Owner-only: only an owner-class connection may run it, regardless of op-class (`sessions`/`who`/`grant`/`revoke`/`whoami`) — the instance god-token, and the bridge connection, which carries Owner's power.
    OwnerOnly,
    /// Bridge-only: only the fabric bridge connection may run it (the `socketpair` end the instance keeps for its `aterm-link serve` child, served with the bridge scope pre-resolved instead of an `AUTH` line).
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
    /// Acts on the resolved session (text/turn/image/send/…).
    Session,
    /// App-level: acts on the resolved instance's front window.
    App,
    /// Self-scoped / fleet-wide; a selector is rejected (sessions/who/grant/version/…).
    Meta,
}

/// Which of the control server's lanes a request costs (`aterm-gui`'s `control_lanes.rs`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lane {
    /// Answers at once: an RPC lane.
    Rpc,
    /// Mostly waits — for an event, a settle, the owner, the bridge — or hands the connection to a relay.
    Wait,
    /// A wait lane when an explicit `@<sid>` other than `@self` aims it, else an RPC lane.
    WaitWhenAimed,
}

/// What a verb does with a `--json`/`json` token in its request.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Json {
    /// The token is an ordinary argument.
    None,
    /// The verb has no structured form, and its grammar never carries the token as data, so the server answers `ERR json: not supported for <verb>`.
    Refused,
    /// The server answers JSON — most wrap it in `json_ok` (`OK 1` and one object), `log` answers one object per record — so under the token the reply frames as [`Framing::Lines`] whatever the row's own framing.
    Emits,
}

/// What follows a request's line on the stream before it is answered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Body {
    /// Nothing: the line is the whole request.
    None,
    /// `<verb> <n>` then `n` raw bytes; `keyed`: an `id=<key>` may follow the length.
    Counted {
        /// Whether the header takes the idempotency key.
        keyed: bool,
    },
    /// A `len=<n>` among the leading options announces `n` bytes (`post`).
    PostLen,
}

/// What a verb's request does to a session's input, as the server's input gates ask it: a set of the flags below.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Input(u16);

impl Input {
    /// No input gate asks the verb.
    pub const NONE: Self = Self(0);
    /// A fleet halt refuses it: it can reach a session's PTY, signal its child, retire a live session, or re-aim the seam that writes.
    pub const PTY: Self = Self(1);
    /// Writes input bytes to a session's PTY, so the unread-input gate asks it.
    pub const WRITES: Self = Self(1 << 1);
    /// Self-targeted, it passes the per-session self-feed floor, so a raw client cannot drive an output→observe→write storm on its own session.
    pub const FLOORED: Self = Self(1 << 2);
    /// Refused while a `turn` or another connection's hard drive lease holds the session, so raw writes cannot corrupt a turn's output.
    pub const LEASED: Self = Self(1 << 3);
    /// Takes a leading `id=<epoch>:<producer>:<seq>` idempotency key.
    pub const KEYED: Self = Self(1 << 4);
    /// Takes a leading `if=<re>` guard, the `if-gen=`/`if-fp=`/`if-human=` fences and `unread=ok`.
    pub const GUARDED: Self = Self(1 << 5);
    /// Can drive a modal overlay or native Settings tab in front through the App input seam (or races a human in one), so a scoped caller's flagless request is checked against the front surface first.
    pub const FRONT_DRIVING: Self = Self(1 << 6);
    /// A successful reply carries ` seq=<content_seq>`, the baseline `await seq` waits past.
    pub const STAMPS_SEQ: Self = Self(1 << 7);
    /// An authorized attempt, malformed or not, is a newer input boundary for the cursor's move licence.
    pub const FENCES: Self = Self(1 << 8);
    /// An App-lane gesture that takes a leading `unread=ok`, lifting the unread-input gate for itself.
    pub const APP_UNREAD: Self = Self(1 << 9);
    /// Its argument is the program's own bytes (`send`, `feed`, a frame's body), not a gesture.
    pub const PINNED: Self = Self(1 << 10);
    /// Takes a leading `if-supervisor=<holder>` fence: the verbs a supervisor types into its session with.
    pub const SUPERVISED: Self = Self(1 << 11);
    /// Takes a leading `cause=@<off>|<seq>` naming the mail or session-log record the input acts on, checked against what the session holds before a byte is written.
    pub const CAUSE: Self = Self(1 << 12);

    /// These flags and `other`'s.
    #[inline]
    #[must_use]
    pub const fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Whether every flag of `flags` is set.
    #[inline]
    #[must_use]
    pub const fn contains(self, flags: Self) -> bool {
        self.0 & flags.0 == flags.0
    }
}

impl OpClass {
    /// The machine-readable name `help --json` prints.
    #[inline]
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
    #[inline]
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
    #[inline]
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
    #[inline]
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
    /// The connection-scope gate (orthogonal to `op`).
    pub access: Access,
    /// The first sentence of the verb's help, at most `summary_max_chars!` (96) chars.
    pub summary: &'static str,
    /// The rest of the entry, or `""`.
    pub detail: &'static str,
    /// The lane the request costs, unless one of its forms says otherwise.
    pub lane: Lane,
    /// Whether a successful reply retains an exact artifact (a capture's file, a recording's frames) the client must acknowledge after consuming it ([`ARTIFACT_REPLY_CHALLENGE_PREFIX`]), unless a form says otherwise.
    pub ack: bool,
    /// What the verb does with a `--json`/`json` token.
    pub json: Json,
    /// The verb's sub-forms, in match order: the first whose [`When`] matches the request decides ([`spec_for`]).
    pub forms: &'static [Form],
    /// What follows the request's line on the stream.
    pub body: Body,
    /// What the request does to a session's input ([`Input`]).
    pub input: Option<Input>,
    /// Whether a fleet peer may run the verb.
    pub fleet: bool,
}

impl VerbSpec {
    /// The row's [`Input`] class: what it declares, else [`Input::NONE`].
    #[inline]
    #[must_use]
    pub const fn input_class(&self) -> Input {
        match self.input {
            Some(input) => input,
            None => Input::NONE,
        }
    }
}

/// What a [`Form`] matches in a request's tail.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum When {
    /// The first word is exactly this one.
    Word(&'static str),
    /// The first word is one of these.
    AnyOf(&'static [&'static str]),
    /// The first word starts with `@`.
    AtWord,
    /// Any word is one of these.
    Flag(&'static [&'static str]),
    /// There is any word at all.
    Args,
}

/// One sub-form of a verb: the [`When`] that picks it out of a request, and each fact in which it differs from its row (`None`: the row's).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Form {
    /// What the form matches in the request's tail.
    pub when: When,
    /// The authority class the form needs instead of its row's.
    pub op: Option<OpClass>,
    /// Only an owner-class connection may run the form, whatever its row's [`Access`].
    pub owner_only: bool,
    /// How the form's reply is framed instead of its row's.
    pub framing: Option<Framing>,
    /// The lane the form costs instead of its row's.
    pub lane: Option<Lane>,
    /// Whether the form's reply needs the artifact acknowledgement, instead of its row's.
    pub ack: Option<bool>,
}

/// [`HOLD_COST`] as a literal, so the `hold` entry and the window's Take Over tool tip quote it through `concat!`.
#[macro_export]
macro_rules! hold_cost {
    () => {
        "While any tab is held, agents on this aterm cannot use invoke, tab, pane, hwkey, \
         pointer or confirm, or open the palette, a tab menu, the Autopilot menu or Overview, \
         and a sudo with no terminal cannot ask for its password in any tab."
    };
}
/// What a hold costs the whole instance, in the one sentence every surface that states it uses.
pub const HOLD_COST: &str = hold_cost!();

/// [`TURN_SUBMIT_BUSY_REPLY`] as a literal, so the `turn` entry quotes it through `concat!`.
macro_rules! turn_submit_busy_reply {
    () => {
        "ERR key delivery failed sink=busy"
    };
}
/// A guarded `turn`'s answer when its text is typed and the sink took none of its first Enter before the submit window ended: the text stands in the composer unsent. Never an `ERR busy…` head, which says nothing was written and gives an `id=` back. A re-press the sink takes none of answers no such thing: it stops the re-presses, and the verdict's `pressed=<n>` counts the Enters that went.
pub const TURN_SUBMIT_BUSY_REPLY: &str = turn_submit_busy_reply!();
/// [`TURN_SUBMIT_BUSY_REPLY`] framed as the wire carries it.
pub const TURN_SUBMIT_BUSY_LINE: &str = concat!(turn_submit_busy_reply!(), "\n");

/// The cap on a [`VerbSpec::summary`] as a literal.
macro_rules! summary_max_chars {
    () => {
        96
    };
}

/// The one table of GUI targets.
macro_rules! aux_targets {
    (
        front: $(#[$fdoc:meta])* $front:ident = $fkw:literal $(| $falias:literal)*;
        opens: $(#[$o0doc:meta])* $o0:ident = $o0kw:literal $(| $o0alias:literal)*
            $(, $(#[$odoc:meta])* $o:ident = $okw:literal $(| $oalias:literal)*)*;
        gestures: $($(#[$gdoc:meta])* $g:ident = $gkw:literal $(| $galias:literal)*),*;
    ) => {
        /// A GUI surface the `window`, `controls` and `open` verbs name, beyond the bare front window.
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        pub enum AuxTarget {
            $(#[$fdoc])* $front,
            $(#[$o0doc])* $o0,
            $($(#[$odoc])* $o,)*
            $($(#[$gdoc])* $g,)*
        }

        impl AuxTarget {
            /// Every target, in the table's order: the order the help and the refusals name them in.
            pub const ALL: &'static [AuxTarget] =
                &[Self::$front, Self::$o0 $(, Self::$o)* $(, Self::$g)*];

            /// Parse a verb's target keyword or alias, case-insensitively; the empty word is the front window.
            #[inline]
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

            /// The target's keyword (for replies, refusals and default capture file names).
            #[inline]
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
            #[inline]
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
        /// The frontmost terminal window (the plain `window`/`chrome` capture).
        Front = "front" | "window" | "terminal";
    opens:
        /// The native Settings tab.
        Prefs = "prefs" | "preferences" | "settings",
        /// The native Settings `/about` route, projected from its compiled semantic frame while that route is open.
        About = "about",
        /// The in-window command palette, an own-rendered card on the front window.
        Menu = "menu" | "palette",
        /// The in-grid session context menu (the tab menu).
        TabMenu = "tab-menu",
        /// The Autopilot menu, the chip's card over the pane.
        AutopilotMenu = "autopilot-menu",
        /// A fact's menu, the same card hung from a fact of Claude Code's composer rule (the usage's: its Traffic card, a graph over time).
        FactMenu = "fact-menu",
        /// The Overview; `open overview` raises it on the front window, `controls overview` reads the open card's groups, chips and manages lines (where this aterm's fabric is on, its "On the fabric" group too: `overview fabric chip node= sid= need= age= mark= older=` lines, read off the fabric's log and never acted on), and every wire read of it is Owner-gated.
        Overview = "overview" | "connections",
        /// The native Settings `/updates` route, projected from its compiled semantic frame while that route is open.
        Update = "update" | "software-update";
    gestures: ;
}

/// The aux targets `open` opens, ` | `-separated ([`AuxTarget::opens`]).
pub const OPEN_TARGETS: &str = open_targets!();
/// The targets `window` and `controls` take, ` | `-separated: every [`AuxTarget`].
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

const _: () = {
    let (mut v, mut total) = (0, 0);
    while v < VERBS.len() {
        let (name, summary) = (VERBS[v].name, VERBS[v].summary);
        let chars = char_count(summary);
        assert!(
            chars > 0 && chars <= summary_max_chars!(),
            "a control verb's summary is empty or over summary_max_chars!()"
        );
        let pad = (CATALOG_TEXT_COLUMN - 1).saturating_sub(char_count(name));
        total += name.len() + pad + 1 + summary.len() + 1;
        v += 1;
    }
    assert!(
        total <= SHORT_CATALOG_MAX_BYTES,
        "the short catalog's rows are over SHORT_CATALOG_MAX_BYTES"
    );
};

const _: () = {
    let mut i = 0;
    while i < VERBS.len() {
        let row = &VERBS[i];
        assert!(
            !(matches!(row.op, Owner) && matches!(row.access, Scoped)),
            "an Owner-class verb is Scoped, so no gate holds it"
        );
        assert!(
            !(matches!(row.access, Scoped) && matches!(row.target, Meta)),
            "a Scoped verb has no target to check its op against"
        );
        assert!(
            !matches!(row.json, Json::Emits) || matches!(row.op, Read),
            "a JSON-emitting verb is not a Read verb"
        );
        let name = row.name.as_bytes();
        let mut verb_len = 0;
        while verb_len < name.len() && name[verb_len] != b' ' {
            verb_len += 1;
        }
        if verb_len < name.len() {
            let mut j = 0;
            let mut base = None;
            while j < VERBS.len() {
                if bytes_eq(VERBS[j].name.as_bytes(), name, verb_len) {
                    base = Some(&VERBS[j]);
                }
                j += 1;
            }
            let Some(base) = base else {
                panic!("a sub-form row's verb has no row of its own");
            };
            assert!(
                base.access as u8 == row.access as u8 && base.target as u8 == row.target as u8,
                "a sub-form row's access or target differs from its verb's"
            );
        }
        i += 1;
    }
};

const _: () = {
    let gates_on_writes = Input::FLOORED
        .with(Input::LEASED)
        .with(Input::KEYED)
        .with(Input::GUARDED)
        .with(Input::FRONT_DRIVING)
        .with(Input::STAMPS_SEQ)
        .with(Input::PINNED);
    let mut i = 0;
    while i < VERBS.len() {
        let row = &VERBS[i];
        let class = row.input_class();
        assert!(
            row.input.is_some() || matches!(row.op, Read),
            "a verb that can mutate declares its input class (Input::NONE is a decision)"
        );
        assert!(
            !matches!(row.op, Read) || class.0 & Input::PTY.with(Input::WRITES).0 == 0,
            "a Read verb reaches a PTY or writes input"
        );
        assert!(
            !class.contains(Input::WRITES) || class.contains(Input::PTY),
            "an input-writing verb is outside the halt"
        );
        assert!(
            class.0 & gates_on_writes.0 == 0 || class.contains(Input::WRITES),
            "an input gate asks a verb that writes no input"
        );
        assert!(
            !class.contains(Input::APP_UNREAD)
                || (class.contains(Input::PTY) && matches!(row.target, App)),
            "a leading unread=ok is an App verb's, inside the halt"
        );
        let name = row.name.as_bytes();
        let mut verb_len = 0;
        while verb_len < name.len() && name[verb_len] != b' ' {
            verb_len += 1;
        }
        if verb_len < name.len() {
            let mut j = 0;
            while j < VERBS.len() {
                if bytes_eq(VERBS[j].name.as_bytes(), name, verb_len) {
                    assert!(
                        VERBS[j].input_class().0 == class.0,
                        "a sub-form row's input class differs from its verb's"
                    );
                }
                j += 1;
            }
        }
        i += 1;
    }
};

/// Whether `a` is exactly the first `len` bytes of `b`, in a const context.
const fn bytes_eq(a: &[u8], b: &[u8], len: usize) -> bool {
    if a.len() != len || b.len() < len {
        return false;
    }
    let mut i = 0;
    while i < len {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// The budget for the short catalog's rows put together (every [`catalog_lines`] row, `\n`-terminated).
pub const SHORT_CATALOG_MAX_BYTES: usize = 14134;

/// The `subscribe` stream vocabulary: frame sources, then the two modifiers.
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
    /// This row, served to a fleet peer too (the `fleet` field).
    #[must_use]
    pub const fn for_fleet(mut self) -> Self {
        self.fleet = true;
        self
    }

    /// The verb's full help text: `summary`, then `detail` after one space when there is any.
    #[must_use]
    pub fn help_line(&self) -> String {
        if self.detail.is_empty() {
            self.summary.to_string()
        } else {
            format!("{} {}", self.summary, self.detail)
        }
    }

    /// The verb's full entry as `help <verb>` prints it.
    #[must_use]
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

use crate::control_request::head;
use Access::{AnyScopeMeta, BridgeOnly, OwnerOnly, Scoped};
use Framing::{Bytes, Lines, Push, Status};
use Lane::{Rpc, Wait, WaitWhenAimed};
use OpClass::{ClipboardWrite, ConfigWrite, Owner, Read, Signal, Write};
use Target::{App, Meta, Session};
use When::{AnyOf, Args, AtWord, Flag, Word};

/// A `const`-friendly [`VerbSpec`] constructor for a normally-`Scoped` verb (keeps the table terse; the vast majority of rows).
const fn v(
    name: &'static str,
    op: OpClass,
    framing: Framing,
    target: Target,
    summary: &'static str,
    detail: &'static str,
) -> VerbSpec {
    va(name, op, framing, target, Scoped, summary, detail)
}

impl VerbSpec {
    /// The row with its request costing `lane`.
    const fn lane(mut self, lane: Lane) -> Self {
        self.lane = lane;
        self
    }

    /// The row with its successful reply acknowledged.
    const fn ack(mut self) -> Self {
        self.ack = true;
        self
    }

    /// The row with its `--json`/`json` handling.
    const fn json(mut self, json: Json) -> Self {
        self.json = json;
        self
    }

    /// The row with its sub-forms, in match order.
    const fn forms(mut self, forms: &'static [Form]) -> Self {
        self.forms = forms;
        self
    }

    /// The row with a body after its line.
    const fn body(mut self, body: Body) -> Self {
        self.body = body;
        self
    }

    /// The row with its input class declared.
    const fn input(mut self, input: Input) -> Self {
        self.input = Some(input);
        self
    }
}

/// A driven write's class: the halt, the unread-input gate, the self-feed floor, the drive lease, the front-surface fence and the licence fence all ask it.
const DRIVEN: Input = Input::PTY
    .with(Input::WRITES)
    .with(Input::FLOORED)
    .with(Input::LEASED)
    .with(Input::FRONT_DRIVING)
    .with(Input::FENCES);

/// A [`Form`] matching `when` that differs from its row in nothing yet.
const fn form(when: When) -> Form {
    Form {
        when,
        op: None,
        owner_only: false,
        framing: None,
        lane: None,
        ack: None,
    }
}

impl Form {
    /// The form needing `op`.
    const fn op(mut self, op: OpClass) -> Self {
        self.op = Some(op);
        self
    }

    /// The form run only by an owner-class connection.
    const fn owner_only(mut self) -> Self {
        self.owner_only = true;
        self
    }

    /// The form's reply framed as `framing`.
    const fn framing(mut self, framing: Framing) -> Self {
        self.framing = Some(framing);
        self
    }

    /// The form costing `lane`.
    const fn lane(mut self, lane: Lane) -> Self {
        self.lane = Some(lane);
        self
    }

    /// The form's reply acknowledged or not.
    const fn ack(mut self, ack: bool) -> Self {
        self.ack = Some(ack);
        self
    }

    /// Whether this form's [`When`] matches `tail`.
    #[cfg_attr(trust_verify, trust::skip)]
    fn matches(&self, tail: &str) -> bool {
        let mut words = tail.split_whitespace();
        match self.when {
            Word(word) => words.next() == Some(word),
            AnyOf(any) => words.next().is_some_and(|first| any.contains(&first)),
            AtWord => words.next().is_some_and(|first| first.starts_with('@')),
            Flag(flags) => words.any(|w| flags.contains(&w)),
            Args => words.next().is_some(),
        }
    }
}

/// A [`VerbSpec`] constructor with an explicit non-`Scoped` [`Access`] gate, so the dispatch reads the exception from the table instead of hardcoding a verb list.
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
        lane: Rpc,
        ack: false,
        json: Json::None,
        forms: &[],
        body: Body::None,
        input: None,
        fleet: false,
    }
}

/// The table.
pub const VERBS: &[VerbSpec] = &[
    va(
        "version",
        Owner,
        Status,
        Meta,
        AnyScopeMeta,
        "build provenance (version, commit, trustc, flavor, signature)",
        "— `flavor=t|r` is Trust or upstream Rust; `trust=` is Trust's version (`none` upstream); \
         `trustc=` and `rust_compat=` are the compiler's rustc-shaped release, with \
         `trustc_commit=` and `trustc_host=` (there is no `rustc=`). A fleet peer runs only the \
         verbs `help --json` marks `fleet`; any other verb, an `@` selector or a frame is `ERR \
         denied: …` and closes the connection. Under `[fleet] allow = \"drive\"`, a node named by \
         `aterm fleet allow drive --from` and proved by its key also runs, as Owner, `@<sid> \
         status|text|screen|result|turn|key|send` on a hosted session, `hold <sid> on|off`, \
         `spawn agent=<claude|codex|gemini|agy|muse>` with its task, `await task`, and `@<sid> close` \
         on a worker it spawned (`aterm help fleet`).",
    )
    .input(Input::NONE)
    .for_fleet(),
    va(
        "update",
        Owner,
        Status,
        Meta,
        AnyScopeMeta,
        "self-updater [status|check|apply]: OK current_build= staged_build= outcome=",
        "— one line: `enabled= current_build= commit= staged_build= staged_version= staged_commit= \
         staged_is_same_commit= relaunch_ready= apply_posture= failing=<n>[:<kind>] \
         failing_applies= persistent= outcome=\"…\"`, plus, where they apply, \
         `stale_check=<rfc3339>` (no check in 4 h) or `check_unrecorded=true`, `checked_at= \
         next_check=`, `checker_stalled=<s>:<phase> checker_respawns= checker_deferred=`, \
         `apply_phase= apply_held=` (automatic postures), `apply_policy_reason=`, \
         `delivery=deferred|blocked`, `installable=false` (a disk-image, translocated or dev copy \
         never updates), on Linux \
         `linux_installed_build= linux_staged_build= linux_trial=<phase|none> linux_trial_starts= \
         linux_trial_healthy=`, `apply_refusal= apply_refusal_at= apply_failure= trial_wait=`, the \
         handoff timings `freeze_seed_ms= handoff_capture_ms= handoff_park_ms=`, and `changelog=` \
         (prose pct-encoded). `apply_posture`: `automatic`, `automatic-idle`, `waits-terminal` \
         (installs once a terminal tab opens), `manual-config`, `disabled-env`, `retry-wait`, \
         `manual-only`, `handoff-unavailable`, `applying`, `unreconciled`, `none`, `disabled`, \
         `unknown`. `apply_phase`: `prefer-idle`, `prefer-output-gap`, `keys-only`, then `land` \
         within a minute of arming; `apply_held=restored-agents` holds the apply (up to 7 min) \
         while reopened tabs' agents relaunch. `check` (Owner-only) runs one channel check and \
         answers the same line. `apply` (Owner-only) queues the staged build on macOS (a \
         live-session handoff keeps shells) or replaces the executable in place on Linux.",
    )
    .forms(&[
        form(Word("check")).owner_only().lane(Wait),
        form(Word("apply")).owner_only(),
    ])
    .input(Input::NONE),
    va(
        "help",
        Owner,
        Lines,
        Meta,
        AnyScopeMeta,
        "help [<verb> | --full | --json]: this catalog (alias: verbs).",
        concat!(
            "Bare: one summary row per verb (<= ",
            summary_max_chars!(),
            " chars). `help <verb>` (or a sub-form such as `help inbox get`): that entry, \
             wrapped. `--full`: every entry, after the protocol header. `--json`: the table as \
             one JSON line (`name op framing target access fleet summary` per verb)."
        ),
    )
    .input(Input::NONE),
    va(
        "verbs",
        Owner,
        Lines,
        Meta,
        AnyScopeMeta,
        "verbs [<verb> | --full | --json]: this catalog (alias: help).",
        "",
    )
    .input(Input::NONE),
    va(
        "privacy",
        Read,
        Lines,
        Meta,
        AnyScopeMeta,
        "privacy [--json]: the macOS consent posture behind EPERM",
        "— instance-wide (every selector but `@.` is refused; `--headless` answers it); \
         reading raises no dialog. `OK <n>` then rows: `schema=1`; `platform= os=`; `bundle_id= \
         display_name= signing= team= dr= grant_stable=<yes|no> dev_build=<true|false>`; \
         `install= running= anchor=`; `full_disk_access= probe= probe_age_ms= fda_scope=`; \
         `covers=`, `uncovered=`, `unmeasured=`; `folder <name>=<state>… source=`; \
         `claimants=<n|-> conflicting=<n|-> census=<complete|partial|unavailable|pending|off> \
         [sole=<yes|no>] [age_ms=<n>]` and up to 8 `claimant path= signing= dr= team= \
         conflicts=yes trash=<offered|signature|requirement|linked|running-unstable|no-tool>` \
         rows (then `claimant truncated=<m> of=<total>`, and a `note <pct>` when any copy \
         conflicts); `prompt_possible=`; `attribution_root= \
         sessions_total= sessions_adopted=` and one `session sid= attribution= responsible_pid= \
         responsible= fs_consent= cwd=` row per readable session; `containment mode= seatbelt= \
         protected=`; `warmup= warmup_last_ms=`; `debugger \
         mode=<enabled|disabled|unrecognized|unknown> member=<admin|developer|no|unknown> \
         self_hardened=<yes|no|unknown> right= probe= probe_age_ms= pending=off pending_right=- \
         pending_age_ms=- last=- watch_ms=-`; `devtools state=<allowed|denied|unlisted|unknown> \
         subject= requests= not_allowed= answer_age_ms= window_s=600 probe= probe_age_ms= \
         grant=<granted|not_granted|unknown> list=<on|off|absent|unknown> list_probe= \
         program_check=<n|-> program_check_oldest_ms=<ms|->`; `observer fda= responsible= log= \
         taskport=`; `access_card \
         state=<quiet|asked|opened|granted|declined> asked_at= opened_at= verified_at= renew_at= \
         needs=`; `remediate fda= files= debugger= devtools=`; then a `note <pct>` when `anchor=` \
         is `displaced` or `deleted`, and the closing `note <pct>`. `--json` is `OK 1` and one \
         object of the same facts, nested (`identity`, `folders`, `census`, `sessions`, \
         `containment`, `warmup`, `debugger`, `devtools`, `access_card`, `observers`, \
         `remediate`), with `grant_unverifiable` for a `displaced` or `deleted` anchor and only \
         the closing `note`. Free text is pct-encoded and `-` is unset; an in-flight probe reads \
         `full_disk_access=unknown probe=pending`. A folder stays `unknown` until the warm-up \
         asks macOS for it (`asking`, then `allowed`, `denied` or `error`) or a measured grant \
         covers it (`covered-by-fda`; `source=none|warmup|fda|warmup,fda`); \
         `fda_scope=this_process` covers `app-data` for this process. `anchor=` is `live`, \
         `displaced`, `deleted`, `not-bundled` or `unknown` (`displaced` and `deleted` match no \
         grant). Conflicting bundle copies reset TCC grants when they prompt; `trash=offered` \
         marks a copy Settings ▸ Security can move to Trash. `devtools`: `state=` is tccd's \
         newest answer within `window_s=`, `list=` the system `TCC.db` switch, and `grant=` the \
         two joined (the newer wins, the switch when a time is missing; `list=absent` is \
         `not_granted`); `program_check=` counts this user's programs waiting at macOS's program \
         check now by the latest jobs reading (one is pulled when none is fresh), a lower bound, \
         and `program_check_oldest_ms=` the longest held (`-` with none held, off macOS, or with \
         no reading). Only a person changes these, at the places `remediate` names: \
         `settings:<pane>` is a System Settings pane, `aterm-settings:<page>` a page of aterm's \
         Settings (the debugger's is Settings ▸ Security); `devtools hand-over` opens the \
         Developer Tools pane and `await consent [kind=debugger]` waits for a change. System \
         State turns debugger access on once per Mac behind macOS's own password dialog; nothing \
         a session sends starts or answers it, and `[privacy] notice = false` pauses it.",
    )
    .json(Json::Emits),
    v(
        "text",
        Read,
        Lines,
        Session,
        "text [--json] [trim] [tail=<n>|rows=<a>-<b>] [order=<logical|display>]: visible screen",
        "— `OK <n>` then the rows. `trim` drops trailing blank rows (`OK <n> trimmed=<k>`); \
         `tail=<n>` sends the last `n` rows and `rows=<a>-<b>` the inclusive 0-based span, \
         clamped to the grid (an empty span or `b < a` is `ERR bad rows`); a slice that starts \
         past row 0 adds `first=<row>`, so reply line `i` is row `first+i`. `order=display` lays \
         each row out as painted (RTL runs reversed); `order=logical` (the default) is reading \
         order. `--json` adds `\"gen\":\"<epoch>.<seq>\"` (`status gen=`, the `key if-gen=` \
         fence), `\"human_ms\":<ms|null>` (`status human_ms=`; `\"unkeyed_ms\"` beside a null), \
         `\"human_seq\":<n>` (the `key|send if-human=` fence), `\"launch\"` (`status launch=`: \
         `{\"id\":<n>,\"seq\":<n>,\"word\":\"claude|codex\",\"why\":\"person\"}`, \
         `{\"why\":\"<word>\"}` or `null`), `\"inputs\":[<n|null>,<n>]` (input writes before \
         and after the read; `null` across an active write, partial frame or spill), \
         `\"submits\":<n>` (Enters written over typed input since the previous Enter), and, when \
         they apply, `\"trimmed\"`, `\"first\"`, `\"order\":\"display\"` with \
         `\"display_map\"`, and `\"frame\":\"open\"`. Every form first waits, holding nothing, \
         up to the 1 s sync timeout from the frame's start for an unfinished DEC 2026 frame to \
         close; one still open is read as it stands, the header saying `frame=open` after the \
         count (`aterm ctl` notes it on stderr), and `--json`'s stamps are taken after that \
         wait. `cursor`, `cell`, `screen`, `line`, `search`, `offscreen screen=1`, `blocktext`, \
         `select`, `selection`, `copy`, `turn` and `cast drift` read the same way; `blocks` and \
         `wait` never wait for a frame.",
    )
    .json(Json::Emits),
    v(
        "screen",
        Read,
        Lines,
        Session,
        "screen [--json] [since=<etag>]: full styled grid as one lossless JSON frame",
        "— ends with `\"etag\":\"<16 hex>\"`, a fingerprint of the screen generation, output fed, \
         dimensions, cursor, colours, palette, selection, style policy and viewport. While it is \
         unchanged, `since=<etag>` answers `OK 1` then \
         `{\"unchanged\":true,\"seq\":<n>,\"gen\":\"<epoch>.<seq>\",\"etag\":\"<etag>\"}` \
         without reading the grid. Read on a whole frame as `text` is: a reply on one still \
         unfinished, `unchanged` included, ends with `\"frame\":\"open\"` after the etag.",
    )
    .json(Json::Emits),
    v(
        "line",
        Read,
        Status,
        Session,
        "line <n>: one physical line of scrollback+screen",
        "— `n` is an absolute row (`search` coordinates). `ERR evicted`: older than retained \
         history; `ERR unreadable`: a retained row that could not be read; `ERR out of range`: \
         past the screen's end. A screen row is read on a whole frame as `text` is (no word for \
         one still open); a history row at once.",
    )
    .json(Json::Refused),
    v(
        "lines",
        Read,
        Status,
        Session,
        "OK <scrollback-line-count>",
        "[truncated=<lost>]",
    )
    .json(Json::Refused),
    v(
        "offscreen",
        Read,
        Lines,
        Session,
        "offscreen [since=<i>] [tail=<n>] [max=<n>] [screen=1]: rows a TUI scrolled off",
        "— keeps the rows that scroll off the top of the alternate screen, diffing committed \
         frames (a DEC 2026 close, else every 16 ms), in 4 MiB shared across sessions (>= 256 KiB \
         each). Reply `OK <n> first=<i> last=<j> lost=<k> breaks=<b> back=<d> [back_at=<i> \
         pin=<p>] epoch=<e> origin=<o> alt=<0|1> seq=<s> [enabled=0] [more=1] [frame=open] \
         [screen_rows=<m>]` then `n` rows, oldest first (line `m` is archived row `first+m`). \
         `since=` is exclusive (`since=<last>` pages; `since=<origin>:<i>`, from `history` \
         `arch=`, reads from 0 when the origin changed; past `last` is `ERR bad since`). At most \
         2000 rows, or `max=<n>`; `tail=<n>` takes the newest `n`; `more=1` marks a cut. `lost=` \
         counts rows evicted after `since`; `breaks=` counts discontinuities (a redraw with no \
         overlap, a resize, leaving the alt screen, a reset); `back=` counts archived rows shown \
         on screen again, at rows `pin..pin+back` from `back_at`. `screen=1` appends the screen, \
         read under the same lock on a whole frame as `text` is, as the last `screen_rows=<m>` \
         lines. A seamless update carries up to 1 MiB from the last 8 turns' marks.",
    )
    .json(Json::Refused),
    v(
        "mainscreen",
        Write,
        Status,
        Session,
        "leave a dead app's alt screen: OK left=<0|1>",
        "— for a full-screen program that died (`kill -9`, `Stop-Process`) without `?1049l` under \
         a shell with no OSC 133 marks (`cmd.exe`). `OK left=1` archives the dead screen into \
         `offscreen` and restores the primary screen and scrollback; `OK left=0`: the primary \
         screen was already showing. Under Windows ConPTY the console host stays on its alt \
         screen until a program writes `ESC[?1049l`.",
    )
    .input(Input::NONE)
    .lane(Wait),
    v(
        "cell",
        Read,
        Status,
        Session,
        "cell <r> <c>|abs=<n> <c>: OK <grapheme%enc> <fg> <bg> <attrs>[ link=][ frame=open]",
        "— `abs=<n>` reads absolute row `n` (`line`/`search` coordinates, history or screen; a \
         history row has no `link=`); `ERR evicted` older than retained history, `ERR out of \
         range` past the screen. A screen row is read on a whole frame as `text` is \
         (` frame=open` last when one is still unfinished); a history row at once.",
    )
    .json(Json::Refused),
    v(
        "cursor",
        Read,
        Status,
        Session,
        "OK <row> <col> <visible> <style>[ frame=open]",
        "— read on a whole frame as `text` is; ` frame=open` comes last (`--json`: \
         `\"frame\":\"open\"`) when one is still unfinished.",
    )
    .json(Json::Emits),
    v(
        "dims",
        Read,
        Status,
        Session,
        "OK <rows> <cols> <px_w> <px_h>",
        "then `session= cell_w= cell_h= font_px= scale= window=<id|none> window_rows= \
         window_cols=`, the grid, frame and surface sizes, offsets, bands, crops, padding, \
         `geometry=`, `present_retry_*` and `layer_*` (`--json` lacks `layer_*`).",
    )
    .json(Json::Emits),
    v(
        "modes",
        Read,
        Lines,
        Session,
        "the DEC modes, one `key=value` line each (`alt_screen=` … `sync_output=`)",
        "",
    )
    .json(Json::Refused),
    v(
        "title",
        Read,
        Status,
        Session,
        "OK <title> (OSC 0/2, set by a program)",
        "",
    )
    .json(Json::Refused),
    v(
        "cwd",
        Read,
        Status,
        Session,
        "OK <cwd> (from shell integration)",
        "",
    )
    .json(Json::Refused),
    v(
        "colors",
        Read,
        Status,
        Session,
        "OK fg= bg= cursor= palette_sets= refused= program_palette= program_slots=",
        "— live default colours plus program overrides: `palette_sets=allowed|refused` (OSC 4/21 \
         gate), `refused=1` once a set was refused, `program_palette=<idx>:<rrggbb>,…` (`-` for \
         none) and `program_slots=` from `fg,bg,cursor,selection_bg,selection_fg` (`-` for none); \
         `reset` clears them.",
    )
    .json(Json::Refused),
    v(
        "theme",
        Read,
        Status,
        App,
        "theme [status] | theme list | theme palette [session]: active theme and palette",
        "— `OK id=\"<id>\" label=\"<label>\" configured=\"<raw>\"|- side=dark|light \
         source=builtin|file|pack|default [path=\"<file>\"] reported=dark|light miss=\"<why>\"|- \
         os_appearance=dark|light preview=\"<id>\"|- [restore=\"<id>\" ttl_ms=<n>|-] fg=#rrggbb \
         bg= cursor=|- selection=|- selection_fg=|- … generate=on|off \
         generate_from=theme|config|default pinned=<n> palette=<16 #rrggbb, comma separated>`. \
         `miss=` names a configured theme that could not be drawn (Default is drawn instead); \
         `generate=` says whether slots 16–255 derive from the theme (`palette_generate`); \
         `pinned=` counts sessions with their own theme; `reported=` is the DSR 996 / mode 2031 \
         baseline (a session's own shows in `status reported_scheme= scheme_from=`). `theme \
         palette [session]` answers `OK 256`, one `slot index=<n> rgb=#rrggbb \
         origin=theme|config|default|generated|xterm` row per slot. `theme list` answers `OK \
         <n>`, one `theme id= label= appearance= measured= pair= source= status= drawn= notes= \
         description= type= skipped=` row per built-in and themes-folder theme, then one `folder \
         path= kept= seen= truncated=` row.",
    )
    .forms(&[
        form(Word("list")).framing(Lines),
        form(Word("palette")).framing(Lines),
        form(Word("status")),
        form(Word("revert")).op(Write),
        form(Args).op(ConfigWrite),
    ]),
    v(
        "theme preview",
        Write,
        Status,
        App,
        "theme preview <name> [ttl=<secs>] | theme revert: preview a theme transiently",
        "— applies `<name>` (built-in, themes-folder id, community pack id (`source=pack`) or \
         `dark:…,light:…`) across windows and sessions without writing config; explicit colour \
         keys still apply. Ends after `ttl=` seconds (default 120, max 3600), on `theme revert`, \
         on a config change, or on a themes reload that leaves `<name>` unavailable. Unknown \
         names are refused. Answers the `theme` status line.",
    )
    .input(Input::NONE),
    v(
        "theme session",
        Read,
        Status,
        App,
        "theme session [status] | set <name> | clear: this session's own theme",
        "— `OK session=<local> theme=\"<selector>\"|- origin=ctl|spawn|split|restored|instance \
         asked=<origin>|- id=\"<drawn id>\" source=builtin|file|pack|default side=dark|light \
         reported=dark|light miss=\"<why>\"|- generate=on|off fg=#rrggbb bg=#rrggbb`. Recolours \
         this session's cells, cursor, selection, palette and DSR 996 / mode 2031 reports; window \
         chrome follows the instance theme. Inherited by splits, carried across seamless updates \
         and restores, and unaffected by instance previews. `set` refuses a name the session \
         cannot draw (`ERR theme session \"<name>\": <why>`), including a community pack theme \
         not yet in the themes folder: `aterm theme install <id>` adds one (`theme preview` \
         draws it without installing); if it stops drawing later, `miss=` is set and the \
         instance theme is drawn. `spawn theme=<name>` sets it at creation.",
    )
    .forms(&[
        form(Word("status")),
        form(Args).op(Write),
    ]),
    v(
        "theme set",
        ConfigWrite,
        Status,
        App,
        "theme set <value> | theme commit: write `theme` to aterm.toml",
        "— writes through `settings set theme`. `OK saved: theme = <value>` appends any \
         unavailable theme or selector warning after `; `. `theme commit` with no active preview \
         is `ERR no theme preview to commit`.",
    )
    .input(Input::NONE),
    v(
        "search",
        Read,
        Lines,
        Session,
        "search <pat> [case] [regex]: find in history; OK <n>[ incomplete…], \"<row> <col> \
         <len>\" each",
        "— searches the newest `search_history_lines` lines (default 100000); the header carries \
         `screen_first=<row>`, the screen's top row. `incomplete` names its causes: `capped` \
         (100000 matches), `rewrapping` (a resize holds history; screen only), `long-line` (the \
         regex budget, or a row over 1 MiB), `moving` (the screen switched, resized or renumbered \
         mid-scan), `older` (older history not searched; alone it is a bare `incomplete`). Rows \
         are absolute (`line` coordinates); a soft-wrapped hit can run past `col+len > cols` into \
         the next row, and regex `^` and `$` bind to the logical line. The search starts on a \
         whole frame as `text` reads; ` frame=open` before `screen_first=` says the frame was \
         unfinished, which is no `incomplete` cause.",
    ),
    v(
        "find",
        Read,
        Status,
        App,
        "find [open|type|key|next|prev|case|regex|accept|cancel|status]: drive the FIND BAR",
        "— drives the window's find bar (⌘F / Ctrl+Shift+F), which `key` and `send` do not reach. \
         `find open` opens it; `find type <text>` inserts at the caret; `find key <name>` runs \
         one edit (`back`, `delete`, `left`, `right`, `word-left`, `word-right`, `home`, `end`, \
         `kill-start`, `kill-end`, `kill-word-back`, `kill-word-forward`); `find next`/`find \
         prev` step matches (wrapping); `find case`/`find regex` toggle those flags; `find \
         accept` exits on the match (⏎); `find cancel` exits and restores the viewport (⎋); `find \
         status` reads without changing. All forms answer `OK open=0` when closed, else `OK \
         open=1 query=<pct> case= regex= regex_error= matches=<n> current=<i> row= col= len= \
         truncated= stale=`. `row= col= len=` use selection coordinates (`0..rows` is the live \
         screen, negative is scrollback, `-` when no match) — not `search`/`line` absolute rows. \
         `current=`/`matches=` are 1-based (`truncated=1` when capped, `stale=1` when output \
         arrived since the count). Typing or stepping while closed changes nothing and answers \
         `open=0`. Acts on the front window.",
    ),
    v(
        "selection",
        Read,
        Lines,
        Session,
        "OK <n>[ incomplete][ frame=open] + the selected text",
        "— read on a whole frame as `text` is; ` frame=open` ends the header when one is still \
         unfinished.",
    )
    .json(Json::Refused),
    v(
        "copy",
        ClipboardWrite,
        Status,
        Session,
        "copy the selection to the system clipboard, OK <bytes>[ incomplete][ frame=open]",
        "— read as `selection` is.",
    )
    .json(Json::Refused)
    .input(Input::NONE),
    v(
        "blocks",
        Read,
        Lines,
        Session,
        "blocks [<n>]: shell-integration command blocks (exit codes, command text, state)",
        "— the newest `<n>` (default all), oldest first, one `block <id> \
         <prompt|entering|executing|complete> exit=<n|-> prompt=<row> cmd=<row|-> out=<row|-> \
         end=<row|-> cwd=<pct> cmdline=<pct>` row each; `--json` adds the command's column \
         (`cmdcol`).",
    )
    .json(Json::Emits),
    v(
        "blocktext",
        Read,
        Lines,
        Session,
        "blocktext <id> [trim]: one command block's output",
        "— `trim` drops trailing blank rows (`OK <n> trimmed=<k>`). Read on a whole frame as \
         `text` is (` frame=open` after the count when one is still unfinished).",
    )
    .json(Json::Refused),
    v(
        "image",
        Read,
        Status,
        Session,
        "image [path]: the client frame aterm drew, as a PNG (OK <w> <h> <path>)",
        "— a bare filename lands in the runtime `images/` dir; omitted, one is chosen. Captures \
         the last presented client frame (headless: the semantic renderer's), not what the OS \
         compositor showed. `@<sid>` captures the whole window showing that session, so edge \
         tokens are refused (`ERR denied`; use `image read`, `text` or `screen`). `image \
         --bytes` returns the PNG inline (`OK 1` then `<w> <h> <nbytes> <base64>`). `image \
         --meta` adds the frame's pixel fingerprint, phase, theme and per-leaf identity: `OK <w> \
         <h> image-meta image-meta-version=3 <key=value…> path=\"<path>\"` (with `--bytes`: `OK \
         2`, the `image-meta` row, then the PNG row; \
         `frame-phase=destination|presented|staged|unknown`; `identity=unavailable` when \
         provenance was not kept). `image plain` leaves out the \
         visual effects; `image read` returns the session's inline OSC 1337 images as base64.",
    )
    .ack()
    .forms(&[
        form(Word("read")).framing(Lines).ack(false),
        form(Flag(&["--bytes", "bytes"])).framing(Lines).ack(false),
    ]),
    v(
        "window",
        Read,
        Status,
        App,
        "window [<target>] [path] : assemble a full-window artifact",
        concat!(
            "(platform chrome + submitted client frame) -> PNG, reply `OK <w> <h> <path>`. \
             Targets: ",
            window_targets!(),
            " (",
            open_aliases!(),
            "). A bare filename lands in `images/` and needs an extension in `<target>`'s slot \
             (`ERR unknown window target '<word>' …` otherwise). A target other than `front` must \
             already be open in the front window (`ERR <target> is not open in the front window; \
             use `open <target>` first`)."
        ),
    )
    .ack()
    .forms(&[form(AnyOf(&["overview", "connections"])).owner_only()]),
    v(
        "video",
        Read,
        Status,
        App,
        "video [<secs>] [full|half] [keys] [pace] [fps=] [budget=]: record frames",
        "— records submitted client frames on one monotonic clock into `frame_NNNN.png` and \
         `index.json` (the newest 8 recordings are kept). `secs` defaults to 3 (`0.5..=60`), \
         `fps=` takes `1..=120`, `budget=` (frame-store MiB) defaults to 512 (`64..=4096`), and \
         resolution to `half`; `pace` forces continuous redraws. `keys` (Owner-only) logs \
         hardware input and `key`/`ctrl`/`send`/`feed`/`paste` on the visible front tab \
         (`inputs= unlogged_inputs=` on the reply count what it logged and the input to other \
         tabs). `video status` answers `recording=true mode= elapsed_ms= frames= resized= \
         keys=<true|false> [inputs= unlogged=]` or `recording=false exporting=<true|false>`; \
         `video stop` (Owner-only) ends a recording early. `video frames [count=N]` lists the \
         newest recording's `N` most-changed frames (`frame n= delta= t_us= seq= <path>`, \
         default 8, max 64). `index.json` carries coverage (`head_truncated`, `evicted_frames`, \
         `ring_skipped`, `covered_us`, `requested_ms`, `keys_requested`, `inputs_logged`, \
         `unlogged_inputs`), `ticks[]` (unchanged redraw instants under `pace` or headless) and \
         `analysis` (at `fps=N` the key-to-frame floor is `~1000/N` ms; typing latency is \
         `metrics percentiles` `input_*`).",
    )
    .lane(Wait)
    .ack()
    .forms(&[
        form(AnyOf(&["status", "stop"])).lane(Rpc).ack(false),
        form(Word("frames")).framing(Lines).lane(Rpc),
    ]),
    v(
        "appstatus",
        Read,
        Lines,
        App,
        "OK <n> + one `activity` row per live/finished app-initiated job",
        "— lists app-initiated jobs (`toolchain`, `update`) and `harness` notes: `activity \
         kind=<toolchain|update|harness> phase=<live|done> progress=<pct>/100|- title=<t> \
         detail=<d> stats=<s> outcome=<ok|warn|-> [since_ms=<ms>]` (live rows first, then \
         finished oldest-first; free text pct-encoded).",
    ),
    va(
        "appnotice",
        Write,
        Status,
        App,
        OwnerOnly,
        "appnotice <toolchain|update|harness> <text>: log a helper notice",
        "— Owner-only. Records `<text>` from a helper process (`aterm pkg install`, harness) into \
         Settings ▸ Messages and `appstatus` (`kind=harness` on the `harness` lane) without \
         raising a band row (`OK recorded`). On `toolchain`, `managed-current: …` and \
         `machine-settings: …` update those `atpkg` markers.",
    )
    .input(Input::NONE),
    v(
        "messages",
        Read,
        Lines,
        App,
        "messages [<n>] [since=<id>] [tag=<tag>] [sev=<sev>] [live]: the message log, one row \
         each",
        "— rows ascend by id: `message <id> at=<unix_ms> ago_ms=<ms> tag=<tag> \
         sev=<success|info|warn|error> origin=<host|wire|carried> \
         state=<held|live|review|folded|stale|unseen|superseded|resolved-ok|resolved-warn|dismissed|answered|evicted|carried|withdrawn|recorded|quit> \
         glass=<row|-> rep=<n> key=<k|-> title=<t> detail=<d> actions=<a|-> \
         [progress=<n>/100|busy=1] [load=<network|disk|cpu>] [reveal=pending] [since_ms=<ms>] \
         [need=<now|working|notice|later>] [rev=<confirmation>]` (free text pct-encoded; `rev` \
         is an opaque per-entry observation on open digest entries, changed by each accepted \
         report, including an identical repeat; unrelated entries do not change it). Bare `<n>` \
         keeps the newest `n` (default 64, max \
         512); `since=<id>` pages forward from `<id>`; `tag=` and `sev=` (floor) filter; \
         `need=later` selects exceptions to review; `live` selects unretired band rows \
         (`need=later live` for open exceptions; `reveal=pending` marks a row still inside its \
         grace period).",
    ),
    va(
        "notice",
        Write,
        Status,
        App,
        OwnerOnly,
        "notice <post|progress|done|settle|ask|later|dismiss|act> …: post and manage band messages",
        "— Owner-only. `notice post <tag> [sev=<sev>] [key=<key>] [hold=<1..120>] <title>[ -- \
         <detail>]`: `sev=success|info` (default `info`) only logs (`OK recorded message=<id>`), \
         `sev=warn|error` raises a band row (`OK message=<id>`). `notice progress <key> \
         [tag=<tag>] [pct=<0..100>|done=<n>/<total>|busy] [unit=<bytes|items|steps>] \
         [load=<network|disk|cpu>] <title>[ -- <stats>]` shows a metered row after 2 s (stale \
         after 120 s unrefreshed); `notice done <key> [ok|warn|withdraw] [<words>]` ends it (`OK \
         done=<id|-> how=<resolved-ok|resolved-warn|withdrawn|gone>`). `notice ask <key> \
         [tag=<tag>] <title> -- <reason> -- <step>` raises a standing owner ask (reason and step \
         <= 90 chars; quiet for 24 h after a dismissal, a Details read or an answer; \
         `OK message=<id>`). `notice later <key> [tag=<tag>] [url=<https-url>] <title> -- \
         <reason> -- <step>[ -- \
         <detail>]` files a non-urgent exception in the menu-bar item and Settings ▸ Messages \
         (`OK later=<id> how=<new|restated|duplicate|quiet>`; up to 4 wire entries, filed after \
         14 d; `url=` adds an Open in browser button). `notice settle <key> <id> <revision>` \
         resolves that open digest entry only while its key and confirmation (`id` and `rev` from \
         its `messages need=later live` row) still match (`OK settled=<id|-> \
         how=<resolved-ok|gone>`); an entry repeated, changed, replaced or filed since that \
         observation is left alone. Hosts share clock-independent confirmations seeded from random \
         entropy at startup; cross-process freshness is probabilistic. Missing entropy or an \
         exhausted `rev` disables settlement (`how=gone`); `rev` is independent of UI revisions. \
         Settling never forgets a dismissal. `notice dismiss <id|review>` dismisses a row or \
         files the open exceptions; `notice act <id> <label|index|details>` presses a button (`OK \
         acted=<label> performed=<1|0>`). Titles <= 6 words / 48 chars (`sev=success|info` posts \
         are clipped at 120 chars); keys `1..=40` of `[a-z0-9._-]` (stored as `wire.<key>`); \
         built-in tags are reserved (use `script` or your own). Limits: 3 live wire rows, 60 \
         messages or 16 KiB a minute, 10 acts a minute (`ERR busy notice: … retry_ms=<ms>`; \
         `retry_ms=1000` during a version handoff).",
    )
    .input(Input::NONE),
    va(
        "story",
        Write,
        Status,
        Session,
        OwnerOnly,
        "story chose [<text>]: record what the harness chose",
        "— Owner-only; takes `@<sid>` (bare: active tab). `<text>` is optional (<= 96 bytes, no \
         control bytes). Records a harness question-dialog policy choice (`[harness] \
         answer_questions` or `meta set questions`) as a story point (`status story=` and `status \
         level=story` until a human key clears it; nothing is drawn or played). Replies `OK \
         story=<n>`.",
    )
    .input(Input::NONE),
    va(
        "devtools",
        Write,
        Status,
        App,
        OwnerOnly,
        "devtools hand-over [now]: open macOS Developer Tools settings",
        "— Owner-only. Opens macOS Privacy & Security ▸ Developer Tools (`privacy`'s `devtools` \
         row; seeds an empty list via `spctl developer-mode enable-terminal`) with aterm's helper \
         panel. Bare `devtools hand-over` waits for a quiet window moment and answers `OK queued` \
         (lapses after 30 min). `now` skips the wait and answers `OK shown` or `OK unshown`. \
         Otherwise answers why nothing ran: `OK granted` (already listed and on), `OK unknown` \
         (list and tccd unreadable), `OK declined` (owner clicked Not now; 30 d / 90 d cooldown), \
         `OK unstable` (ad hoc/dev build, bare only), `OK listed` (row on while macOS still \
         checks, bare only), or `OK recent` (opened within 10 min, or 1 min for `now`). Headless \
         answers `ERR headless: …`, non-macOS `ERR unsupported: …`, bad args `ERR usage: devtools \
         hand-over [now]`.",
    )
    .input(Input::NONE),
    v(
        "appearance",
        Write,
        Status,
        App,
        "appearance [dark|light]: the OS appearance the theme follows",
        "— answers `os_appearance=<dark|light> source=<nsapp|desktop|window|inert|injected> \
         theme_side=<dark|light|-> reported_scheme=<dark|light> bg_dark=<0|1> theme=<id> \
         installs=<n> session_installs=<n>` (a session's `status` carries `os_appearance= \
         theme_side= reported_scheme= bg_dark=` plus `scheme_from=`). `appearance dark|light` \
         injects an OS flip (`dev-seams` builds).",
    )
    .input(Input::NONE),
    v(
        "text-scale",
        Write,
        Status,
        App,
        "text-scale [0.85..2.0|os]: the text scale native pages lay out at",
        "— answers `text_scale=<f> source=<os|injected>`. `text-scale <0.85..2.0>` injects a scale \
         and `text-scale os` clears it (`dev-seams` builds; `$ATERM_TEXT_SCALE` injects one at \
         launch).",
    )
    .input(Input::NONE),
    v(
        "pressure",
        Write,
        Status,
        App,
        "pressure [normal|warn|critical]: memory pressure and the crash-restore journal's shed",
        "— answers `level=<-|normal|warn|critical> journals=<n> rings_shed=<n> \
         journal_resident_bytes=<n> rings_back_in_ms=<-|n>`: the last OS memory-pressure level \
         (`-` before any), the sessions journaling a crash-restore region, the history rings \
         shed under `warn`/`critical` (the screen still restores; scrollback above the keyframe \
         does not), the resident bytes of every crash-restore region, and the ms until the next \
         shed ring returns (a session's `status` says `journal_ring=<live|shedding|shed> \
         journal_bytes=`). `pressure normal|warn|critical` injects that level on macOS (debug or \
         `dev-seams` builds) as the OS's would: `warn`/`critical` also evict every session's \
         in-memory scrollback down to a fraction of its budget, and `critical` retires the warm \
         spare.",
    )
    .input(Input::NONE),
    v(
        "program-check",
        Write,
        Status,
        App,
        "program-check [full|live|held <pid>|held -]: the stuck check's macOS program-check board",
        "— answers `OK reading=<live|full> held=<pid|-> syspolicyd=<-|ok|full|stuck> \
         syspolicyd_workers=<n>/<limit|->|- syspolicyd_full_ms=<ms|->`, the board `status` \
         carries (`aterm help permissions`), sampled once a minute on macOS. The forms below \
         need `dev-seams`: `program-check full` stages `syspolicyd` at the kernel thread limit \
         (after 5 min it reads `stuck`, raising the restart alert and pausing harness \
         continuations); `program-check live` restores live sampling; `program-check held <pid>` \
         has the jobs engine read `<pid>`, a live process of this user (any other is refused), \
         as held at macOS's program check whatever its CPU (a tab's job member where its reading \
         judged a start), so `sys jobs` shows `wait=program-check` and a `finding \
         kind=program-check` row and `privacy` counts it; `program-check held -` ends it.",
    )
    .input(Input::NONE),
    v(
        "card",
        Write,
        Status,
        App,
        "card [seed=<u64>|seed=off]: pin the card's stars (debug builds)",
        "— a release build answers every form `ERR card needs a debug build`. Bare `card` \
         answers `OK seed=<u64|off> rearmed=0`. `card seed=<u64>` pins every agent card's \
         starfield to that seed and re-arms visible cards (`OK seed=<u64|off> rearmed=<cards>`); \
         `card seed=off` restores per-session seeding.",
    )
    .input(Input::NONE),
    v(
        "aos",
        Write,
        Status,
        App,
        "aos [status|attach|pause|repair|publish|bundle|resume|detach|trace]: glow host \
         (debug builds)",
        "— hosts the front window's typing-momentum glow as an Aos task. `aos [status]` answers \
         `OK window=<id> attached=<bool> code=<name> …` (revision, pause queue, trace cursor, \
         hot/active/settling flags, `cold_at=`, last wake and boundary counters). `aos attach \
         [code=<shipped|demo-stranded-settle>]` checkpoints the glow and records keys, resets, \
         frame ticks and scheduler queries; `aos pause` holds at the step boundary; `aos repair \
         [code=<name>]` shadow-replays live and candidate code over the trace and swaps the \
         candidate in only when live matches the paused head and the candidate keeps \
         `momentum-glow.no-stranded-light` (on a refusal a glow the repair paused resumes on the \
         old code, and one `aos pause` held stays paused, as for `aos publish`); `aos publish \
         plan=<24 hex>` judges a 12-byte schedule plan under the pinned acceptance policy and \
         publishes it at the step boundary (pausing first) only if it is admitted and, over the \
         shipped code, useful (`useful=`, `gain_ppm=`, `bundle=`); `aos bundle` exports the live \
         plan's candidate bundle (`bundle=<digest> rev= bytes= hex=`); `aos resume` drops a \
         pending update; `aos detach` stops hosting; `aos trace` reports the log length, the \
         trace and checkpoint digests and the broken flag, and `aos trace hex` adds the exported \
         trace file's bytes.",
    )
    .forms(&[form(AnyOf(&["status", "trace"])).op(Read)])
    .input(Input::NONE),

    v(
        "login-notice",
        Write,
        Status,
        App,
        "login-notice [<code> [<label>]|disk [spent|inodes|off]|end|back]: harness login notices",
        "— bare, lists `OK notices=<code>:<id>,…`, the Claude Code login, renewal, account and \
         usage-limit notices (`aterm help harness`); the other forms need a debug or \
         `dev-seams` build. `login-notice <code|id> [<label>]` \
         (label `main` by default) raises that notice on the first tab (`account-signed-out` and \
         `renewal-wrong-account` update the accounts line). `login-notice disk \
         [spent|inodes|off]` raises the disk-full alert (`spent`: after cache cleanup; \
         `inodes`: inodes exhausted; `off`: `[disk] auto_free_gib = 0`). `login-notice end` \
         withdraws the notices; `login-notice back` marks the login restored and closes its \
         Settings ▸ Messages row. Replies `OK sid=<sid> notice=<id|disk-full|end|back>`, the \
         first tab's session.",
    )
    .input(Input::NONE),
    v(
        "security-posture",
        Write,
        Status,
        App,
        "security-posture [list|off|<name>]: Settings ▸ Security's posture source",
        "— bare, answers `OK posture=<host|off|name> views=<n>`: where Settings ▸ Security reads \
         its macOS access posture, and how many views draw it. `security-posture list` answers \
         `OK postures=<name>,…`. `security-posture <name|off>` stages a fixture or restores \
         `off` (`dev-seams` builds, headless only; a staged button calls no OS API).",
    )
    .input(Input::NONE),
    va(
        "askpass",
        Owner,
        Status,
        Meta,
        OwnerOnly,
        "askpass: prompt on the session's tab for a no-tty sudo password",
        "— `aterm-askpass` (`SUDO_ASKPASS`) asks it when `sudo` has no tty on macOS. Once the \
         kernel shows the caller is a child of `/usr/bin/sudo`, it raises a native password \
         sheet on that tab and returns the password once, never stored or logged. Owner only: \
         child edges, the bridge, headless instances and held sessions are refused (`aterm help \
         sudo`).",
    )
    .lane(Wait)
    .input(Input::PTY),
    v(
        "chrome",
        Read,
        Lines,
        App,
        "the front window's native UI (toolbar + menu bar) and its presence line",
        "— off macOS there is no menu bar (`menu \"<title>\"` lines give the menu model; `controls \
         menu` shows the live palette); macOS `--headless` prints `toolbar (none)`, `toolbar-tabs` \
         (with `\"done\"` tab states) and `menu (none)`. On Windows a `strip-version \
         shown=<yes|no> …` line reports the tab-strip version label (`cols=` for `pointer move -1 \
         <col>`, `action=`, or `reason=no-strip|not-laid-out|no-ui-face|strip-full`). One \
         `autopilot sid=<sid> window=<id> program=<claude|agy|muse|gemini|codex> \
         face=<cruise|cruise-until|land|landed|theirs|off|master-off|agent-off|unhosted|-> \
         drawn=\"<cells>\"|drawn=- why=<not-drawn|no-room|no-host|no-footer|switch-only> \
         spoken=\"<label>\"|spoken=- description=\"<words>\"|description=-` line per supervised \
         pane in any window. Then `statusitem title=<❯|❯•|❯N> needs=<n> stuck=<n> exceptions=<n> \
         done=<n> fleet=<off|unread|reading|failed|as_of:<unix ms>>` and, on macOS, the `❯` menu's \
         `statusmenu \"<label>\" enabled=<yes|no> [checked=<yes|no>] [tip=\"<text>\"] [indent=<n>] \
         [submenu]` lines (`statusmenu ---` for a separator; debug builds add `at=<i>[.<j>…]` for \
         `person menubar press`). Last, `presence rim=<none|stuck|decide|yours> pressing=<@sid|-> \
         pill=\"<sentence>\"`: the window's needs border and tab-bar pill (`\"` and `\\` escaped). \
         `pressing` names the border's session and the pill may name another; a worker's report \
         can light the border with no pill. `stuck`/`decide` come from the most pressing session's \
         `status needs=`, `yours` from a person's hold on the front tab; `[presence] rim` off \
         hides both.",
    ),
    v(
        "controls",
        Read,
        Lines,
        App,
        "GUI controls as text (the `window` targets, default front)",
        concat!(
            "— targets: ",
            window_targets!(),
            " (`connections` aliases `overview`). `controls autopilot-menu` lists open Autopilot \
             menu rows (`autopilot-menu/mode/<word>`, `autopilot-menu/land-in/<minutes>`, \
             `autopilot-menu/land-at`, `autopilot-menu/land-cancel`, `autopilot-menu/settings`, \
             or `autopilot-menu gap index=<i>`). `controls fact-menu` reads a fact menu \
             (`fact-menu open=true session=<sid> fact=<fact>`, rows `fact-menu/<fact>/<act>`). \
             `controls overview` is Owner-escalated and includes remote fabric needs (`overview \
             fabric chip node=<id> sid=<sid> need=<word> age=<s|-> mark=<decide|stuck> \
             older=<0|1> selected=<bool>`, header `fabric=<n> \
             fabric_error=<run|refused|no-brief|->`). Off macOS `front` ends with `confirm \
             open=<true|false>` (`by=`, `answer=\"confirm yes\"`, `cancel=\"confirm no\"`, or `-` \
             when a person asked)."
        ),
    )
    .forms(&[form(AnyOf(&["overview", "connections"])).owner_only()]),
    v(
        "panes",
        Read,
        Lines,
        App,
        "the front window's active-tab panes: `layout tab=<i> panes=<n> zoomed= terminal=`",
        "header (`terminal=false` with `panes=0` when the active tab is a native view), then \
         one `pane session=<local> rect=<row_off>,<col_off>,<rows>x<cols> focused=<bool>` row \
         per visible pane (cell coordinates; dividers are 1-cell gaps). `@<sid>` describes the \
         window showing that session (`OK 0` when none does).",
    ),
    v(
        "inspect",
        Read,
        Lines,
        App,
        "versioned native tab-app semantics: inspect app/v1 tabs",
        "| `inspect app/v1 view <view-id> <text|controls|tree|audit>`. `tabs` prints each tab's \
         0-based `index=` (`tab <N>` / `tab close <N>`).",
    ),
    v(
        "cast",
        Read,
        Bytes,
        Session,
        "asciicast v2 recording (compact, sendable); `cast drift` audits it against the screen.",
        "`cast frames [count=N]` expands it to a keyframe flipbook. `cast drift [max_runs=<k>] \
         [rows=<n>] [seed=auto|alt|primary]` replays it through fresh engines against the live \
         screen to pinpoint a resize run the app never repainted after: `OK <n> \
         verdict=<clean|desync|unfaithful|unknown|empty> fidelity=<pass|fail|-> \
         against=<screen|replay> drift_rows=<d> runs=<total> candidates=<c> capped=<k> \
         culprit=<t,…|-> anchor=<t|-> seed=<alt|primary|ambiguous> \
         seed_basis=<arg|toggle|live|fidelity|default> geom=<CxR> evicted=<n> dropped=<n|-> \
         cut=<quiescent|racy|file> folds=<f> fold_events=<e> ms=<ms>[ carried=0 \
         from_build=<n|->][ law=ambiguous][ undo=off][ computed=client]`, then `run t=<t> n=<k> \
         gap_ms=<ms> geom=<CxR>><CxR>>… net=<zero|CxR> first_out_ms=<ms|-> clear_ms=<ms|-> \
         culprit=<0|1>` per listed candidate, `row <r> live=<pct> expected=<pct>` (`replayed=` \
         when unfaithful) per wrong row, and `cursor`. Candidates are resize runs <= 250 ms apart \
         after `anchor=` (last full clear) or alt-screen resizes whose displaced rows were not \
         restored before returning to the original size. `culprit=` names the candidate whose \
         faithful replay demoted alt-screen rows (else varies the newest `max_runs`, default 8, \
         max 16). `cut=racy` (a screen still moving, or a frame still unfinished at the read's \
         bound) compares replays (`against=replay fidelity=-`); `unknown \
         seed=ambiguous` or `law=ambiguous` means seeds or `aterm_resize_undo` headers disagree; \
         `carried=0 from_build=` marks a recording starting at a seamless update; `undo=off` \
         replays without resize undo. At most 2 analyses run at once (`ERR busy` after 10 s); \
         `aterm ctl` falls back to `computed=client` and supports `cast drift --file <cast> \
         [--screen <text>] [--until <t>] [seed=…]`.",
    )
    .forms(&[
        form(Word("frames")).framing(Lines),
        form(Word("drift")).framing(Lines).lane(Wait),
    ]),
    v(
        "temporal",
        Read,
        Bytes,
        Session,
        "temporal [status|<tick>] [trim]: the screen reconstructed at a past instant",
        "— `<tick>` is µs since the session started (default: the latest recorded instant); a \
         replay needs `temporal_recording = true` (`ERR temporal: this session is not \
         recording; …`). `temporal status` answers `enabled= latest_tick= \
         reachable_from=<tick|-> keyframes= live_events= dropped_events= lost_resizes= \
         dropped_input= dropped_replies= pending_resizes=` (`reachable_from=-`: nothing \
         replays); a replay at the spine's end adds `pending_resizes=` to its header. `trim` \
         drops trailing blank rows (`OK <nbytes> trimmed=<k>`).",
    )
    .forms(&[form(Word("status")).framing(Status)]),
    v(
        "history",
        Read,
        Lines,
        Session,
        "history [<n>] [since=<id>]: the turn LEDGER",
        "— one `turn <id> submitted=<0|1> status= started_ms= dur_ms= seq= hash= by= \
         arch=<origin>:<last> [carried=1] text=<pct>` line per completed turn, the newest 512 \
         (above half, the window harness's own are evicted first). `by=` is \
         `harness|ctl|bridge|<sid>|-`; `status=` is `settled|timeout|hangup|pressed` (`pressed`: \
         Enter was sent and no settle awaited, as by the harness or `turn settle=submitted`, its \
         `seq`/`hash` read on a whole frame as `text` reads). `arch=` feeds `offscreen since=`. \
         A record a seamless update or crash restore carried says `carried=1`, its `started_ms` \
         and `seq` the old instance's (the journal rewrites the ledger every 2 s; `status \
         journal_ledger_lag=`); `unheld_below=<id>` in the header says older records may be \
         missing.",
    )
    .json(Json::Refused),
    v(
        "meta",
        Read,
        Status,
        Session,
        "meta -> OK title= user_title= description= icon= role= attention= cwd= state=",
        "— `attention_owner= attention_owners= attention_kinds= supervisor= supervisor_proven= \
         questions= agent_cwd= trusted_cwd= worker= autopilot= land_at= autopilot_by= \
         autopilot_since=` (pct-encoded; `-` is unset, an unset `title=` is empty; \
         `trusted_cwd=` and `worker=` come from `spawn agent=<word>`; the window is in \
         `sessions` and `dims`). `meta set \
         <title|description|icon|role|attention|questions|autopilot> <text...>` / `meta unset \
         <field>` set or clear user metadata (title 120 B, description 1024 B, icon 64 B, role 64 \
         B, attention 256 B; `user_title=` overrides the OSC `title=`). KEYED ATTENTION: `meta set \
         attention owner=<k> [kind=ask|wait] <text...>` / `meta unset attention owner=<k>` (`<k>` \
         in `[A-Za-z0-9._:@/-]{1,64}`, text <= 200 B, at most 8 owners with the bare `-`, else \
         `ERR attention owners full`) lets writers track escalations apart. `kind=ask` (the \
         default, unless the text starts `limited:` while the harness handles the wall) counts in \
         the menu bar (`status needs=ask`); `kind=wait` is a record that never counts. \
         `attention_kinds=` lists `<ask|wait|->:<owner>`; `attention=` shows the newest entry \
         (`attention_owner=`, `%2D` for the bare one); each change is a timeline `meta-change` \
         row. SUPERVISOR: `meta set supervisor <holder> [ttl=<1..600000>]` / `meta unset \
         supervisor [holder=<holder>]` (Owner-only; `ERR busy supervisor=<holder>` while another \
         holds it) quiets menu-bar and desktop alerts while the supervisor handles prompts \
         (`supervisor_proven=1`: a direct foreground `codex-alab@<pid>` connection). HARNESS_ACT: \
         `meta set harness_act <act> [until_us=<n>]` and `meta prepare harness_act` (`OK \
         until_us=<n> lease_ms=5000`; `ERR expired harness_act`, `ERR busy harness_act`) record a \
         proven self-claim's act (`timeline` `harness-act act= holder=`, `status last_act=`). \
         QUESTIONS: `meta set questions ask|recommended|long-term|short-term` / `meta unset \
         questions` (Owner-only) override `[harness] answer_questions` and `question_priority`. \
         AUTOPILOT: `meta set autopilot cruise [until=HH:MM|+DURATION]|land|off` / `meta unset \
         autopilot` (Owner-only; aliases `forever`/`infinity` and `wrap-up`/`wrapup`/`wrap`; \
         `until=` goes with `cruise` only) sets the session's Autopilot (`OK`, plus \
         `land_at=<unix s> local=<date>T<HH:MM><offset>` for `until=`, plus `journaled=0` when \
         the disk sync took over 5 s); a finished Land sets `task=landed`.",
    )
    .forms(&[form(AnyOf(&["set", "unset"])).op(Write)]),
    v(
        "harness",
        Read,
        Status,
        Session,
        "harness policy: OK gen= effective=<on|off> why= <each [harness] key>= questions=",
        "— read-only `[harness]` policy for a self-supervising agent (`meta` \
         `supervisor_proven=1`). `gen=` is the policy generation (`EVENT <local> harness-policy \
         gen=<n>`); `effective=` and `why=` give this session's verdict \
         (`unhosted|disabled|headless|agent-off|suspended|hold|autopilot-off|enabled`). Followed \
         by every `[harness]` key (pct-encoded, `-` when empty, `continue_text=` composed with \
         `rules_file`; `autopilot=` is the table default while the session's word is `status \
         autopilot=`), `questions=<ask|recommended|long-term|short-term>`, and the composed \
         Cruise/Land prompts: `done_check_text=`, `land_text=`, `land_nudge_text=`, \
         `land_answer_text=`, `needs_hint_text=`, `needs_ack_text=`. Edge tokens receive only `OK \
         effective=<on|off>`.",
    ),
    v(
        "resources", Read, Lines, Session,
        "resources [capabilities|register]: delegated job ownership and current usage",
        "— bare: `OK <n> resources version=1`, then this session's registered jobs (pid and start \
         identity, AI and job labels, `cpu_cores`, physical bytes, `procs=`, `age_ms=`, \
         `sample=complete|unavailable`). \
         `resources capabilities` answers `OK resources version=1 available= self_register= \
         group_signals= runner= ai= session=`. `resources register start=<kernel-start> \
         [ai=<word>] job=<pct label>` (local, Owner-only) binds the caller's kernel peer pid for \
         `aterm resource-run`.",
    )
    .forms(&[
        form(Word("capabilities")).framing(Status),
        form(Word("register")).framing(Status).op(Write).owner_only(),
    ]),
    v(
        "status",
        Read,
        Status,
        Session,
        "OK schema=1 sid= subject= phase= outcome= … needs= … agent= … gen= seq= hash=",
        "— one line of the session's state (no `--json`; `-` is unset, free text pct-encoded), its \
         fields in this order. Process: `schema=1 sid=<local> subject= \
         subject_source=<pin|osc|cwd|unavailable> observed=<true|false> \
         phase=<unknown|starting|idle|running|quiet|exited> since_ms= \
         outcome=<none|success|failure|signal> exit_code= signal= detail= \
         confidence=<unknown|heuristic|strong|exact> reasons=<shell_block,fg_job,…|-> \
         attribution=<live|adopted|unknown> fs_consent=<covered|denied|unknown> conflict= \
         revision= enabled=`. Hold and fabric: `hold=<0|1> hold_advisory=<fabric-lost|-> \
         fabric=<absent|connected|stale|stalled|disconnected> fabric_rtt_ms= fabric_link_age_ms= \
         fleet=`. Presence: \
         `hand=<turn:<id>[:<holder>]|lease:<holder>|driving:<sid>|-> hand_pid= \
         level=<quiet|note|story|driving|driven|attention|limited|hold> story= why= \
         needs=<frozen|stopped|stalled|login|memory|wall|ask|approval|question|-> needs_n= \
         hold_by=<person|remote|local|bridge|-> typed_by=<pct|-> typed_ms= \
         path=<frozen|live>`. Agent: `program= \
         agent=<busy|prompt|question|idle|survey|unknown|wall:<kind>|-> agent_detail= \
         agent_rev= agent_since_ms= agent_gen= agent_fp= \
         program_state=<idle|working|blocked|done|error>[:<permission|question|auth>] \
         last_act=` (`agent_detail=bash:read-only` is the read-only rule's advisory verdict on a \
         Bash box's command alone). Theme: `os_appearance= theme_side= reported_scheme= \
         scheme_from=<theme|program> bg_dark= session_theme= theme_origin=` (`conpty=` on \
         Windows). Input and host: `integration=<on|off|degraded> \
         input=<clear|pending|typeahead|stalled|stopped|-> input_bytes= input_wait_ms= \
         fg_rss_mb= disk=<ok|low|short|emergency|-> disk_free_mb= disk_room_mb= disk_age_ms= \
         job=<verdict|none> job_quiet_ms= syspolicyd=<-|ok|full|stuck> syspolicyd_workers= \
         syspolicyd_full_ms= occupancy=<off|observe|prompt> \
         occupancy_hold=<declared|undeclared|none> ready= slots= idle_ready_min= waits= \
         waits_ready=` (`occupancy why` explains `occupancy_hold=` to `idle_ready_min=`; `waits=` \
         counts the session's declared wakes still to be told and `waits_ready=` those whose \
         condition held, which `waits` lists). Harness and records: `supervisor= supervisor_proven= \
         autopilot=<off|cruise|land|-> human_ms= human_seq= \
         launch=<person|since|typed|boxed|other|pending|none> launch_seq= path_evidence= \
         copy=<managed|foreign|-> upgrade= history_lost= journal=<live|off> (then its \
         `journal_*` fields, and `restore=…` after a crash restore) integration_rev= \
         born=<cold|spare-<tier>> render=<ok|displaced|desync-risk|unverified|unjudged> \
         resizes= flaps= land_at= task=<open|landed|theirs|-> pause= land_held= land_nudges= land_since= task_state= \
         task_rev= log_seq= log_ep= run= turn= step= need= need_since_ms= \
         rollout=<ok|missing|stalled|-> rollout_asks= editing= bells= bells_sounded= \
         last_bell=`, then `gen=<epoch>.<seq> seq= hash=`. `seq=` and `hash=` are `-` on a \
         contended lock or an unfinished DEC 2026 frame, which `status` never waits for; `gen=` \
         stands. `if-gen=`, `if-human=` and `if-fp=` fence on `gen=`, `human_seq=` and `hash=`.",
    ),
    v(
        "timeline",
        Read,
        Lines,
        Session,
        "timeline [<n>] [since=<id>]: the session EVENT TIMELINE",
        "— one `event <id> t=<ms> kind=<k> ...` line per event, a view of the session `log` \
         (every row it still holds, its newest 512 or 64 KiB evicted last; ids carry across a \
         seamless update, marked by a `handoff` row). Kinds: `spawned`, `state-change` \
         (`state=`), `title-change`, `cwd-change`, `meta-change`, `agent-change`, `task-change` \
         (`rev= state= attention=<0|1> [why=] [exit=]`), `status-change` (`phase= outcome= \
         exit_code= signal= confidence=`), `human`, `harness-policy` (`gen=`), `job-change`, \
         `modes-restored` \
         (`from=<pgid> to=<pgid> program=<name|-> reverted=<csv> bytes=<n>` or `reason=manual \
         source=<ctl|menu> reverted=<csv|-> bytes=<n>`), the fabric's `hold`, `inbox`, \
         `inbox-seen`, `post`, `fetch`, `post-landed` and `topic`, `render` (`desync-risk \
         run=<r> displaced=<+-k> net=zero at=<ms>` or `healed run=<r> after_ms=<ms>`), \
         `harness-act` (`act=<act> holder=<holder>`), and `program-state` \
         (`state=<idle|working|blocked|done|error|-> kind=<permission|question|auth|->`, kept as \
         the program marks them: 64 a run, 20 at once then one a second, a flap past that \
         folded into the newest state, which is never dropped; `log` says `coalesced=<n>`). The \
         final `closing reason= by=` reaches `subscribe @<sid> events` and stays in `exits`.",
    ),
    v(
        "log",
        Read,
        Lines,
        Session,
        "log [<node>|n] [lvl=] [since= ep=] [n=] [kinds=] [class=turn] [tier=content] [--json]: \
         THE LOG",
        "— hierarchical session log: session (`-`), runs (`r<n>`), agent turns (`t<n>`) and steps \
         (`p<n>`), plus point marks `human` (`why=mail`, `reached=0`), `input` (`verb= [act=] \
         [re=] [dropped= [pool=shared]] [cause= cause_k=]`), `task` (`rev= state= attention= \
         [why=] [code=<exit>]`), `agent`, `program` (OSC 7501 `status program_state=`: \
         `state=<idle|working|blocked|done|error|none> [ask=<permission|question|auth>] \
         [coalesced=<n>] src=engine conf=claim`) and `row` (`row=<kind>`: a timeline row held \
         as a row alone, such as `job-change`); `lvl=` is `session|run|turn|step`. Header \
         (stderr on `aterm ctl`): \
         `OK <n> v=2 as_of=<seq> ep=<e> oldest=<seq>[ lost=<k>][ more=<k>]`, then `<seq> \
         <open|close|mark|amend|gap> <node> <kind> [par=<node>] <k=v ...> [by=<actor>] \
         src=<where> conf=<exact|strong|heuristic|claim> t=<wall ms>` (`close` adds `out=`, \
         `dur_ms=`, `dur=wall`, `steps= appr= qs= walls= stalls= errs= lost=`; `detail=` holds a \
         sanitized command or wall reset). It never logs command arguments or typed bytes; \
         `tier=content` adds the in-memory-only `title=`, `meta field=attention attn_text=` \
         (`fp=`), `reason=`, `topic_name=`, `since=`, `closer=`, `reply=`, `row_text=`. Page \
         with `since=<as_of> ep=<ep>` (an update writes `mark - handoff carried=<n> shed=<m> \
         epoch=<ep>` or `gap why=handoff epoch=<ep>`; an eviction or reset writes `gap \
         why=<evicted|reset> lost=<k> resync_to=<seq>`). A closed session stays readable as \
         `@<sid> log` (its last 512, journaled unless `[session_log] journal = false`, mirrored \
         under `[fabric] log = \"auto\"|\"turns\"`). `log n` (Owner-only) reads the instance \
         node log (`open n start \
         prev_end=<clean|handoff|killed|signal|panic|running|unknown|none|->`, `mark n link \
         state=<stalled|connected|disconnected> gen=<n> by=bridge|aterm`, `mark n config \
         key=<key>` / `config n=<k>`).",
    )
    .json(Json::Emits),
    v(
        "brief",
        Read,
        Lines,
        Session,
        "brief [<node>|all] [budget=<tokens>] [since=<seq> ep=<e>]: the session log, \
         token-budgeted",
        "— a digest of `log` (`budget=` default 600 tokens, a token 4 bytes): `OK <n> brief \
         sid=<sid> v=2 as_of=<seq> ep=<e> budget=<b> used=<u>`, then session summary \
         (`need=<word>`, `[src+]`), newest turns (`by=human`, `[screen+]`, heuristic \
         `mail=@<off>~` when a turn without `cause=` followed a `kind=task` delivery), steps, \
         marks, `need`/`claim` lines, live `jobs …` summary, optional `gap why=<w> lost=<k> \
         resync_to=<seq>`, and `more=`. Confidence suffixes: `!` exact, `+` strong, `~` \
         heuristic, `?` claim. `brief all [budget=]` (Owner-only, default budget 1000) summarizes \
         all sessions (`OK <n> brief all sessions=<k> v=2 budget=<b> used=<u>`, \
         `more=sessions:<k>`).",
    ),
    v(
        "mark",
        Write,
        Status,
        Session,
        "mark <node|-> <kind> [word=] [text=] [re=] [as=] [ttl=] [rule=]: write an attributed \
         claim",
        "— appends a `src=mark conf=claim` record to `log` (`OK seq=<n> node=<node> by=<actor>`). \
         `<kind>`: `note text=<pct>` (<= 160B), `verdict \
         word=<on-track|off-track|blocked|done|failed|needs-human|unsure>`, `label text=<pct> \
         [ttl=<1..3600 s, 600>]` (<= 32 graphemes / 40B), `need word=<review|decision|help>`, \
         `clear [word=<review|decision|help>]`, `act \
         word=<approve|deny|answer|continue|relaunch|escalate|interrupt|dispatch> [rule=<id>]`; \
         optional `re=<seq|@off>` and `as=<name>` (`[A-Za-z0-9._-]{1,32}`, never a built-in \
         actor: `ERR mark: as=<w> is a session log actor`). `<node>` is `-` or an open \
         `r<n>|t<n>|p<n>` (`ERR mark: <node> is not open`). Never writes to a PTY or mutates \
         state (`by=ctl|harness|bridge|<sid>|-`). Rate-limited to 1/s burst 20 (`ERR busy \
         retry_ms=<ms>`), capped at 1/4 of the log ring.",
    )
    .input(Input::NONE),
    v(
        "resizes",
        Read,
        Lines,
        Session,
        "resizes [<n>] [since=<id>]: grid resize ledger, net-zero flap runs and render verdicts",
        "— header: `OK <n> total=<N> runs=<R> flaps=<F> risks=<D> \
         render=<ok|displaced|desync-risk|unverified|unjudged> unledgered=<U> lost=<L> \
         nudge=<idle|armed|short|restoring> nudged=<N>`, then `resize`, `run` and open/owed \
         `epoch` rows. `resize <id> t=<ms> run=<r> ord=<n> from=<CxR> to=<CxR> alt=<0|1> \
         sync=<0|1> trimmed=<k> demoted=<k> pushed=<k> revealed=<k> appended=<k> restored=<k> \
         shift=<+-k> cut=<k> site=<input|window|chrome|term|pass|redraw|ctl-cross|-> at=<pct \
         file:line|-> win=<CxR|-> chrome=<k|->`. `run <r> t=<ms> n=<k> alt=<0|1> \
         geom=<CxR>><CxR>>... net=<zero|changed> displaced=<+-k> \
         verdict=<none|silent|desync-risk|unverified|healed|reflow> first_out_ms=<ms|-> \
         healed_ms=<ms|->` (on the alt screen, a net-zero flap drawn over without a clear has \
         `appended=1 restored=0` and reads `desync-risk`; `cast drift` confirms). `epoch <e> \
         t=<ms> geom=<CxR>><CxR> shift=<+-k> pushed=<k> cut=<k> drew=<0|1> \
         class=<agent|learned|unknown> verdict=<open|desync-risk|unjudged|healed|restored> \
         by=<app|undo|nudge|-> tried=<0|1>` tracks accumulated alt-screen debt since the last \
         full clear or screen switch. Under `[render] repaint_nudge = \"on\"` (the default; \
         `\"log\"` logs the nudge without making it, `\"off\"` does neither), aterm repairs an \
         agent-class `desync-risk` epoch at a quiet moment by signalling a PTY one row shorter, \
         then restoring it (`by=nudge`). `status` carries `render= resizes= flaps=`.",
    ),
    v(
        "metrics",
        Read,
        Status,
        App,
        "render/latency counters [reset|percentiles]",
        "— `percentiles` prints `n_<x>=` and `<x>_p50/_p95/_p99_ms=` for `input`, `present`, \
         `render`, `key_write`, `key_queue`, `pre_present`, `acquire`, `acquire_queue`, `resize`, \
         `reflow`, `present_tainted`, `term_wait_redraw`/`term_wait_press`, `echo`, \
         `present_glass`, `out_req`, `ime`, `status_glance` (`max_status_glance_ms=`), \
         `need_publish` (`max_need_publish_ms=`), and `tx_chunk`/`tx_last` (`max_tx_hold_ms=`), \
         plus `turns_by_root=<root>:<n>/<p50>/<p95>/<p99>` and counters `input_edges_dropped= \
         input_predicted= ime_cancelled= input_refused= reply_dropped=`. Plain `metrics` prints \
         the frame and present counters (`max_frame_gap_ms=`, `first_present_ms=`, \
         `first_visible_ms=`), the startup phases (`startup_*_ms` with their \
         `*_schema=`/`*_valid=` flags, `process_birth_to_rust_main_ms=`, \
         `rust_main_to_first_present_ms=`), the PTY child round trip (`n_echo= echo_p50_ms=` … \
         `echo_dropped_locked=`), the main-loop census (`max_turn_ms= max_turn_owner= … \
         main_busy_ms_per_s=`), the update handoff counters (`input_dropped= input_incoherent= \
         handoff_claim_stamps= handoff_warm_pre_dial_ms=`), `control_accepts=` … \
         `control_busy_replies=`, and the PTY fd census (`pty_masters= pty_sessions= pty_kits=`). \
         `echo_*` is the child round trip; `input_*` is key to the next content present (or a \
         proven prediction, when `input_predicted=` > 0). Quote `n_*` with every percentile.",
    )
    .json(Json::Emits),
    v(
        "occupancy",
        Read,
        Lines,
        Session,
        "occupancy [why|check --replay <dur>]: why the occupancy door fired, or why nothing did",
        "— the harness occupancy door (`[harness] occupancy`), read-only: nothing is typed or \
         written. `occupancy why` (the default) is the door's last word as this session's loop \
         told it: `occupancy=<off|observe|prompt> hold \
         <declared|undeclared> since <HH:MM> held <span> ready=<0|1>` (or `hold none`), a line a \
         built-in rule (`wait-nudge@v1`, `hold-room@v1`: `rev=builtin \
         <quiet|would|held:<guard>|paced|spent|typed|skipped why=<w>|told|paced why=<w>|refused \
         why=<w>> [HH:MM]:` then each term with `✓` or `✗`) and `  guards: brakes <none|<guard>> \
         <✓|✗>` then each guard the break judged (`person`, `halted`, `draft`, `taskless`, \
         `exchange`, `managed`, `memory`, `reach`, `autopilot <mode>`) with `✓`, or `✗` where it \
         holds (`  guards: none judged` before a break); `OK 1` and `occupancy=<mode> unsaid` \
         where no loop told one. `occupancy check --replay <n>s|m|h|d` (at most 36h) re-runs the \
         decider over the window's facts ring (`<state>/occupancy/facts.ring`, one row a minute, \
         the newest 1440 kept) with this session's `fact-hold@v1` ledger rows: `replay from \
         <HH:MM> to <HH:MM> looks=<n> holds=<n>`, then `<HH:MM> <rule> \
         <would|held:<guard>|paced|spent>` at each change, then `<rule> first would \
         <HH:MM|never>` per rule.",
    ),
    v(
        "wake",
        Write,
        Status,
        Session,
        "wake label=<token> when=<term> …: declare a wake for this session's agent",
        "— the occupancy door's declared wakes (`[harness] occupancy`): the agent's own book, \
         judged by aterm and told back to it. `wake label=<1-64 of A-Z a-z 0-9 . _ -> \
         when=<term>[ and <term>…] [for=<dur>] [need=<sys>[ and <sys>…] [for=<dur>]] \
         [unless=<source>]… [prio=critical] [by=+<dur>]`, `<term>` a `pid:<n> exited` of the \
         session's own tree, an `at:<HH>:<MM>` or `in:<dur>`, or a machine term of `await sys` \
         (`need=` takes machine terms alone; `free`, `disk=` and `vol=` arrive in a later slice; \
         `file:` and `gone:` are not served yet), `<dur>` `<n>s|m|h` and `by=` at most +48h \
         (default +24h): `OK wake #<n>` (` await_only=1` for a time alone, which only `await \
         wake` answers), `ERR usage wake: <why>`, or `ERR denied wake: <why>`. `wake rm #<n>` \
         and `wake taken #<n>` remove it or mark it taken (`OK wake #<n> rm|taken`, `ERR no such \
         wake #<n>`). At most 16 a session. Only a process descended from the session's own PTY \
         child (and from its hosted agent, where aterm hosts one), proved by the kernel's peer \
         pid and each process's birth, may declare, remove, take or await: `ERR denied wake: no \
         kernel peer` (a relay or the bridge), `another session's`, `not in this session's tree`, \
         or `occupancy=off` (under `off` every book ends). No word of a declaration is ever typed \
         into a session. `waits` and `await wake` read and wait on the book; `status` carries \
         `waits= waits_ready=`.",
    )
    .input(Input::NONE),
    v(
        "waits",
        Read,
        Lines,
        Session,
        "waits [why [#<n>]]: this session's declared wakes",
        "— `OK <n>` then one line a wake, lowest ordinal first: `#<n> <pending|ready|answering|\
         answered|claimed|told|taken|expired> prio=<normal|critical> kinds=<kind>[,…] \
         declared=<ms> by=<ms> since=<ms|-> label=<token>` (`<kind>` `pid|at|in|sys`, times unix \
         ms). `waits why` says why each is not told yet: `#<n> <state>[ why=<due|unless|agent|\
         path|off|admit>] atoms=<kind>:<0|1>[,…][ need=<sys …>] [await_only=1]`, and `waits why \
         #<n>` that wake alone. A peer outside the session's tree (see `wake`) is shown ordinals, \
         states, priorities, atom kinds and times alone: no label and no need. `ERR denied wake: \
         occupancy=off` under `off`.",
    ),
    v(
        "sys",
        Read,
        Lines,
        Session,
        "sys [<view>] [--json] [since=<c>]: cores, GPU, memory, disk of the machine, aterm and \
         this tab",
        "— views: `overview` (default) | `tab` | `aterm` | `global` | `jobs` | `cores` | `gpu` | \
         `mem` | `disk` | `net` | `procs` | `why`; options: `scope=pane|tab|window|aterm|global` \
         (on resource/job views), `top=<n>` (default 8, max 64), `fresh=<ms>` (0 forces a live \
         sample; stale replies add `state=stale age_ms=`), `depth=` (on `disk`), `instances=all` \
         (folded by `aterm ctl`). Header: \
         `OK <n> sys v=1 view= as_of= age_ms= window_ms= next=<ep>.<seq>`; \
         `since=<ep>.<seq>` returns changed lines, `same <keys>`, `e` events, or `gap \
         why=reset|evicted` (`ERR bad since head=` if ahead). Records use `key=value` in base \
         units (`cpu_mc=`, `fp_bytes=`, `write_bps=`, `busy_pm=`) or `unavailable(<why>)` / \
         `lane_off:`. `jobs` lists stuck-engine rows (`job id= kind= consumer= … class= [wait=] \
         [quiet_ms= phi=] verdict= [no_progress=1]`) and `finding` rows. `disk` lists roots \
         (`state=complete|walking|<refusal> alloc_bytes= [walked_entries=] entries= growth_bps=`) \
         and top `dir` subtrees. Processes print `pid=` and `exe=<word|->` (never argv/env; paths \
         only for the caller's tab; `caller=` is advisory; machine-wide rows beyond the caller's \
         scope read `withheld=<n>`). `why [cpu|mem]` says who uses the CPU or memory and why, \
         with the cause aterm can name, the caller's `you` role and the split; `why` alone picks \
         what deviates most. See `aterm help sys` and `await sys`.",
    )
    .lane(Wait)
    .json(Json::Emits),
    v(
        "turn",
        Write,
        Lines,
        Session,
        "turn [option=value ...] <text>: ONE HUMAN TURN",
        concat!(
            "— types `<text>`, verifies the submit (`submit=` names any `key`, default \
             `enter`; `none` skips it), waits for the screen to settle (`idle=<ms>`, default \
             1500, within `timeout=<ms>`, default 240000), and returns the settled screen. \
             Options, after a leading `id=` and `if-supervisor=`: `[idle=<ms>] [timeout=<ms>] \
             [submit=<key|none|guarded:<re>>] [settle=match:<re>|gone:<re>|submitted] \
             [submit_window=<ms>] [presses=<n>] [submit_verify=<auto|seq|block>] [trim=<0|1>] \
             [typed=<0|1>] [cadence=<ms>] [cap=<n>] [yield=<floor>] [if-gen=<epoch>.<seq>] \
             [if-rows=<first>-<last>:<hex16>] [cause=@<off>|<seq>]`; they end at `--` or the \
             first word that is not one of them (text that starts `<option>=` needs `--`, or \
             `-- --` when no option but a leading `id=`, `if-supervisor=` or `cause=` comes \
             before it). `presses=<n>` (default 3, max 10) retries the submit within \
             `submit_window=<ms>` (default 2000). `settle=match:<re>` settles when a visible row \
             matches; `settle=gone:<re>` waits for `<re>` to appear within `submit_window` and \
             then leave (an idle settle when it never shows); `settle=submitted` returns once the \
             submit is verified (`status=submitted`). `submit_verify=auto` (the default) \
             verifies a Claude Code submit by the typed text leaving the composer, another TUI's \
             by `seq`, and a shell's by its command block. `submit=guarded:<re>` presses Enter \
             only while the cursor row matches `<re>`, under the same terminal lock (as `key \
             if=`; a first-press miss is `OK 0 turn skipped reason=guard submitted=0 seq=<n> \
             id=<n>`, plus `pressed=<n>` when re-presses stop; a first Enter the sink takes none \
             of before `submit_window=` (or `timeout=`, if sooner) ends is `",
            turn_submit_busy_reply!(),
            "`, the text typed and not submitted, and a re-press it takes none of stops the \
             re-presses as a miss does). `typed=1` presses key by key at \
             `cadence=<ms>` (default 70) up to `cap=<n>` graphemes (default 240, max 4096) and \
             pastes the rest (`typed=<keys> pasted=<graphemes>`), stopping with `OK 0 turn \
             skipped reason=<deadline|person|changed> typed=<k> pasted=<p> submitted=0 seq=<n> \
             id=<n>` when the timeout, a person's key, or a screen or input-frame change \
             overtakes it. `yield=<floor 0..=1>` waits for `trail status` `momentum=` to fall to \
             `<floor>` before typing (`yielded_ms=<n>`, or `ERR yield timeout momentum=<v>`). \
             `if-gen=<epoch>.<seq>` (`status gen=`) and `if-rows=<first>-<last>:<hex16>` \
             (FNV-1a-64 of those rows) fence on the screen before typing (`reason=changed` on a \
             mismatch). `if-rows=` never vouches for half a frame: a `typed=1` turn whose first \
             look finds the fence spanning a composer box (rules at `<first>` and `<last>`) in an \
             unfinished DEC 2026 frame waits within `timeout=` for it to close and judges the \
             fence on it; any other turn waits as `text` does, and a frame still open has moved. \
             The settled screen is read on a whole frame too (` frame=open` when not). \
             `if-supervisor=<holder>` fences on the live supervisor claim; `cause=` links the \
             turn in `log` to held mail (`@<off>`) or a log record (`<seq>`; `ERR cause` when it \
             names neither). A leading `id=<epoch>:<producer>:<seq>` makes it exactly-once (`OK \
             0 dup=1` on a repeat; a turn that reached for the PTY with nothing, or whose first \
             write a full input queue or the `if-supervisor=` fence refused, gives the id back, \
             whatever it answered, and any other refused write leaves it in doubt; see `send`). \
             A `hold` before the bytes land is `ERR halted …`; one after \
             but before the first submit press stops it (`OK 0 turn skipped reason=halted[ \
             typed=<k> pasted=<p>] submitted=0 seq=<n> id=<n>`, `typed=` on a `typed=1` turn); a \
             later one only ends re-presses. Into a `stalled` or stopped program it is `ERR busy \
             input-unread …` (`turn` takes no `unread=ok`); input over 1 s old appends \
             `warn=input-unread input_bytes=<n> input_wait_ms=<ms>`. Its first write refused \
             whole, by a full input queue or the `if-supervisor=` fence, is `ERR busy \
             input-queue-full …` or `ERR busy supervisor=<holder|->` as for `send`, nothing \
             typed; a later one is `ERR paste delivery failed` or `ERR key delivery failed`. \
             Reply: a verdict line (`id= \
             submitted=<0|1> status=settled|timeout|submitted seq= dur_ms= hash=<FNV16>[ \
             task_rev=<n>][ agent_rev=<n>][ frame=open][ trimmed=<k>]`; `agent_rev=` feeds \
             `await agent <words> since=<n>`), then the rows (`trim=1` drops trailing blank rows; \
             `hash=` covers the untrimmed screen)."
        ),
    )
    .lane(Wait)
    .input(
        DRIVEN
            .with(Input::KEYED)
            .with(Input::SUPERVISED)
            .with(Input::CAUSE),
    ),
    v(
        "lease",
        Write,
        Status,
        Session,
        "lease [status|acquire|release ...]: a drive lease — cooperative, or a HARD hold",
        "(one holder; TTL default 30000 ms, max 600000), shown in `who` as \
         `driving=lease:<holder>`. Forms: `lease [status]` | `lease acquire [ttl=<ms>] \
         [holder=<name>] [hard] [here]` | `lease release [holder=<name>] [force] [here]`. \
         `acquire` refuses a live `turn` or another holder's live lease (renews for the same \
         holder); `here` binds the lease to the requesting connection so it releases on \
         disconnect. Advisory by default (`turn` checks it); `hard` (`status` `hard=1`) refuses \
         every other connection's \
         `send`/`key`/`ctrl`/`feed`/`mouse`/`paste`/`paste-bin`/`turn`/`feed-bin` with `ERR busy \
         lease=<holder>`. `status` also reports `pid=<n>` (peer PID of the \
         acquiring local connection). Never blocks physical keyboard input and does not cross a \
         seamless update.",
    )
    .input(Input::NONE),
    v(
        "send",
        Write,
        Status,
        Session,
        "write text to the PTY; reply `OK seq=<n>`",
        "— `seq=<n>` is the content baseline (`await seq <n+1>` waits for resulting output). A \
         trailing `\\n` (backslash, n) is written as Enter (CR); no other escape is decoded. \
         Leading options (before `--`): `[id=<epoch>:<producer>:<seq>] [if=<re>] \
         [if-gen=<epoch>.<seq>] [if-fp=<hex16>] [if-human=<n>] [if-supervisor=<holder>] \
         [unread=ok] [cause=@<off>|<seq>] [act=approve|deny|answer] [as=person]`. `id=` (shared \
         by `send`, `key`, `feed-bin`, `turn`) makes the write idempotent against the session's \
         `nonce=` (a repeat is `OK dup=1`, or `OK 0 dup=1` for `turn`; `ERR epoch` across a \
         relaunch). A refusal at admission or of the request's own words (`ERR halted`, `ERR \
         rate`, `ERR usage`, `ERR badregex`, `ERR cause`) takes no sequence. Once claimed, the \
         id goes back only on `ERR busy…`, `ERR denied`, a fence's `OK skipped…`, or a `turn` \
         that reached for the PTY with nothing, or whose first write the input queue or the \
         `if-supervisor=` fence refused, whatever it answered (`ERR halted`, `ERR yield`, `ERR exited`, an \
         `OK 0 turn skipped` before its first paste or key: the `if-gen=`/`if-rows=` fence's \
         `reason=changed` with no `typed=`, or `typed=0 pasted=0`); any other attempt that did \
         not finish `OK` leaves it `ERR in-doubt seq=<n>`. \
         `if=<re>`, `if-gen=`, `if-fp=`, `if-human=` and `if-supervisor=` fence under the \
         terminal/meta lock (see `key`). `unread=ok` bypasses the `input=stalled|stopped` refusal \
         (`ERR busy input-unread …`); slow unread queues (>= 1 s) append `warn=input-unread \
         input_bytes=<n> input_wait_ms=<ms>`. `cause=@<off>|<seq>` links a turn opened within 5 s \
         to mail or a `log` record (`ERR cause` if invalid; `cause_k=mail|rec`). \
         `act=approve|deny|answer` tags the dismissed approval/question box in `log` \
         (`out=approved|declined|answered` for harness, `out=passed by=<actor>` otherwise, \
         `re=<box>`, rate-limited with `dropped=<n>` / `pool=shared` on `mark - input verb= \
         by=`). `as=person` (debug / `dev-seams` only; no fence, `id=`, `act=` or `cause=` beside \
         it) routes through the human input seam (`status human_ms=` / `human_seq=`).",
    )
    .input(
        DRIVEN
            .with(Input::KEYED)
            .with(Input::GUARDED)
            .with(Input::STAMPS_SEQ)
            .with(Input::PINNED)
            .with(Input::SUPERVISED)
            .with(Input::CAUSE),
    ),
    v(
        "paste",
        Write,
        Status,
        Session,
        "paste <text>: bracketed-paste text to the PTY",
        "— DEC 2004 brackets when the program enabled them at write time, control bytes \
         sanitized and LF made CR, with no `confirm_multiline_paste` banner. Takes a leading \
         `if-supervisor=<holder>` fence.",
    )
    .lane(WaitWhenAimed)
    .input(DRIVEN.with(Input::STAMPS_SEQ).with(Input::SUPERVISED)),
    v(
        "key",
        Write,
        Status,
        Session,
        "key [id=<key>] [if=<re>] <name>: send a named key (enter/tab/up/...)",
        "— delivered beneath window shortcuts (`key ctrl+shift+t` sends ^T to the PTY; use \
         `invoke <action>`, `tab new` or `find open` for window actions). Leading options (in any \
         order): `[id=<epoch>:<producer>:<seq>] [if=<re>] [if-gen=<epoch>.<seq>] [if-fp=<hex16>] \
         [if-human=<n>] [if-supervisor=<holder>] [unread=ok] [act=approve|deny|answer] \
         [as=person]`. `if=<re>` tests visible rows under the same terminal lock hold as the \
         write (write spaces as `.` since the wire splits on whitespace, e.g. `key \
         if=Do.you.want.to.proceed 1`): a match writes and replies `OK seq=<n>`; no match writes \
         nothing, releases `id=` (so a retry is re-evaluated rather than `dup=1`), and replies \
         `OK skipped seq=<n>`; invalid regex is `ERR badregex`; a full/contended sink is `ERR \
         busy sink` (or `ERR write failed partial accepted=<n>`). `if-gen=<epoch>.<seq>` (`status \
         gen=`, `agent_gen=`) and `if-fp=<hex16>` (`status hash=`, `agent_fp=`, `turn hash=`) \
         reply `OK skipped reason=changed seq=<n>` if the screen moved (`if-seq=` is rejected as \
         `ERR usage`). A row fence (`if=`, `if-fp=`) waits as `text` does for an unfinished DEC \
         2026 frame to close and answers `OK skipped reason=changed` on one still open; `if-gen=` \
         and `if-human=` are decided as the screen stands. `if-human=<n>` (`status human_seq=`) \
         replies `OK skipped reason=person seq=<n>` if a person gave input since `<n>`. \
         `if-supervisor=<holder>` requires `<holder>` to hold `meta set supervisor` (`ERR busy \
         supervisor=<holder|->` otherwise). Refusal order: `ERR halted`, `ERR rate`, `ERR busy \
         turn=`, `ERR usage` (the options, then the key name), `ERR busy input-unread bytes=<n> \
         wait_ms=<ms> input=<stalled|stopped>`, `ERR badregex`, `ERR busy supervisor=`, `id=`, \
         then the `if=`/`if-gen=`/`if-fp=`/`if-human=` guards. On macOS, raw input unread >= 1 s \
         appends `warn=input-unread input_bytes=<n> input_wait_ms=<ms>`; stalled or \
         stopped programs refuse unless `unread=ok` or an ISIG signal char (`ctrl+c`) can be taken \
         immediately. Trailing tokens: `mods=<ctrl+shift+...>` (on `type=release`, `mods=` names \
         the modifiers remaining held), `type=press|repeat|release` (`down`/`up`), and \
         `base=<c>`.",
    )
    .input(
        DRIVEN
            .with(Input::KEYED)
            .with(Input::GUARDED)
            .with(Input::STAMPS_SEQ)
            .with(Input::SUPERVISED),
    ),
    v("ctrl", Write, Status, Session, "ctrl <letter>: press Ctrl+<letter>", "")
    .input(DRIVEN.with(Input::STAMPS_SEQ)),
    v(
        "feed",
        Write,
        Status,
        Session,
        "feed <hex>: write those bytes to the PTY (spaces ignored)",
        "",
    )
    .input(DRIVEN.with(Input::STAMPS_SEQ).with(Input::PINNED)),
    v(
        "hwkey",
        Write,
        Status,
        App,
        "hwkey <char|name> [mods=] [count=] [interval=]: inject a key through the OS event queue",
        "— macOS only (`ERR hwkey: hardware-path injection is implemented on macOS only` \
         elsewhere). Posts a real `NSEvent` (`shape=press` builds it from a CoreGraphics key \
         event, so layout and Option dead keys compose), to measure hardware-path typing latency \
         in `metrics percentiles` (`n_key_write`). Replies `OK posted=<n> key=<0|1>`. Takes a \
         leading `unread=ok`; `ERR hwkey` while a palette, Overview or tab menu is open, or \
         System State is the front window's active view.",
    )
    .input(Input::PTY.with(Input::WRITES).with(Input::APP_UNREAD)),
    v(
        "feed-bin",
        Write,
        Status,
        Session,
        "feed-bin <n> [id=<key>], then <n> bytes: raw bytes to the PTY",
        "— a refused `id=` still reads the `<n>` bytes, so the connection stays framed.",
    )
    .body(Body::Counted { keyed: true })
    .input(
        Input::PTY
            .with(Input::WRITES)
            .with(Input::FLOORED)
            .with(Input::LEASED)
            .with(Input::KEYED)
            .with(Input::FENCES)
            .with(Input::PINNED),
    ),
    v(
        "paste-bin",
        Write,
        Status,
        Session,
        "paste-bin <n>, then <n> bytes: bytes to the PTY as `paste` writes text",
        "",
    )
    .lane(Wait)
    .body(Body::Counted { keyed: false })
    .input(
        Input::PTY
            .with(Input::WRITES)
            .with(Input::FLOORED)
            .with(Input::LEASED)
            .with(Input::FENCES)
            .with(Input::PINNED),
    ),
    v(
        "mouse",
        Write,
        Status,
        Session,
        "mouse <action> [<button>] <row> <col> [mods=] [count=]: a mouse event",
        "— `<action>` is `press|release|move|wheelup|wheeldown|wheelleft|wheelright`, `<button>` \
         `left|middle|right|back|forward` (only `move` may leave it out: no button held); \
         trailing `[mods=..] [count=N] [side=left|right] [block=0|1] [lines=N]`.",
    )
    .input(DRIVEN.with(Input::STAMPS_SEQ)),
    v(
        "pointer",
        Write,
        Status,
        App,
        "pointer [move <r> <c>|move key=<k>|click|wheel dy=<n>|leave|status|mods <m>]: drive the \
         POINTER",
        "— drives the front window's pointer as the OS would: moves, leaves, clicks, wheel and \
         modifiers. `move <r> <c>` targets window cell `<r>,<c>` (negative rows are the chrome \
         bands above row 0, e.g. `move -1 <c>`), replying `OK at=<row>,<col> px=<x>,<y>`; on a \
         native view `move key=<ui-key>` or `move xy=<x>,<y>` replies `OK at=native view=<id> \
         x=<x> y=<y> key=<ui-key|-> px=<x>,<y>` (`ERR the page under the pointer changed …` while \
         a changed view's next frame is unpresented). `pointer leave`, and `status` with no \
         position, reply `OK at=-`. \
         `click` (Owner-only; 10 a minute on band capsules, else `ERR busy notice: 10 presses a \
         minute retry_ms=<ms>`) left-clicks where the pointer is. `wheel dy=<n> [dx=<n>] \
         [unit=lines|pt] [key=<ui-key>|xy=<x>,<y>]` scrolls (each `|n|<=10000`, not both 0). \
         `mods <list>` (Owner-only) holds modifiers (`cmd`/`super`, `ctrl`, `alt`/`option`, \
         `shift`, `link`, or `none`) until the next physical event (`cell <r> <c>` `link=` reads a \
         hovered link). Takes a leading `unread=ok`.",
    )
    .forms(&[form(AnyOf(&["click", "mods"])).owner_only()])
    .input(Input::PTY.with(Input::WRITES).with(Input::APP_UNREAD)),
    v(
        "resize",
        Write,
        Status,
        Session,
        "resize <r> <c>: resize the engine + PTY (grid first, window echoed to match).",
        "`resize px <w> <h>` resizes the front window in physical pixels and lets the grid follow \
         from the platform resize event (measure with `metrics` `resize_present` and `dims` \
         `layer_*`); accepted only for the focused pane of the front window's active tab (`ERR \
         resize px: only the focused pane of the front window's active tab` otherwise).",
    )
    .input(Input::PTY.with(Input::FENCES)),
    v("focus", Write, Status, Session, "focus <in|out>: send a focus report", "")
    .input(Input::PTY.with(Input::WRITES).with(Input::FRONT_DRIVING).with(Input::FENCES)),
    v(
        "scroll",
        Read,
        Status,
        Session,
        "scroll the view (up|down|top|bottom|N|prev/next-prompt)",
        "— answers `OK <offset> <scrollback>` (rows scrolled back, rows held); a bare `scroll` \
         only reports them.",
    )
    .input(Input::FENCES),
    v(
        "select",
        Read,
        Status,
        Session,
        "select <r1> <c1> <r2> <c2>|word <r> <c>|line <r>|block <r1> <c1> <r2> <c2>|extend <r> \
         <c>|clear",
        "— `extend <r> <c>` grows the selection (shift-click). `word` and `line` select across \
         soft wraps (not ragged double-width ones) up to hard newlines. Made on a whole frame as \
         `text` reads one; `OK frame=open` when one is still unfinished.",
    ),
    v(
        "signal",
        Signal,
        Status,
        Session,
        "signal <sig> [pid=<n>] [quiet=<s>]: send a signal to the foreground process group",
        "— POSIX only (`ERR signal unsupported on this platform` on Windows). `<sig>` is \
         `int|quit|tstp|hup|term|kill|cont` (also `c`, `z` and `sig<name>`). `pid=<n>` requires \
         `<n>` to lead the foreground process group. `quiet=<s>` refuses when a person typed \
         within `<s>` s (`ERR busy person human_ms=<ms>`) or input is queued unread (`ERR busy \
         input-unread bytes=<n>`). On macOS `term`, `kill`, `hup` and `quit` first discard \
         unread raw-mode input (`OK signalled pgrp <n> discarded=<bytes>`, or `pid <n>`). A \
         `status input=stalled` program that survives `term`/`hup`/`quit` stays stalled (with \
         `input_bytes=0`) until it exits, draws or reads. `restart` restarts a supervised agent \
         on its conversation.",
    )
    .input(Input::PTY.with(Input::FENCES)),
    v(
        "restart",
        Signal,
        Status,
        Session,
        "restart [pid=<n>] [if-stalled] [quiet=<s>]: restart the tab's agent in place",
        "— POSIX only (`ERR restart unsupported on this platform` on Windows). Stamps the \
         supervised foreground agent (Claude Code, agy, codex-alab), sends `SIGTERM` (escalating \
         to `SIGKILL` after 5 s if still stalled and deaf, or 30 s otherwise), and has the window \
         harness relaunch it on the same conversation. Options: `pid=<n>` (must lead foreground \
         pgrp), `if-stalled` (requires `status input=stalled`), `quiet=<s>` (`ERR busy person \
         human_ms=<ms>` if human input within `<s>` s). Replies `OK restarting pid <n> \
         cause=<stall|asked>[ discarded=<bytes>]`, `OK restart gone pid <n>`, or `ERR restart \
         unavailable: \
         <not-the-hosts-agent|not-relaunchable|not-supervised|driver-lease|driver-turn|halted|start-unread|no-state>` \
         (or `ERR not-stalled`, `ERR bad restart argument: <word>`, `ERR restart failed: <what>`). \
         Works even when `[harness] relaunch = false`.",
    )
    .input(Input::PTY.with(Input::FENCES)),
    v(
        "reset",
        Write,
        Status,
        Session,
        "reset [flush]: restore terminal modes to host defaults; keeps screen and scrollback",
        "— reverts negotiated terminal modes (alt screen, kitty keys, modifyOtherKeys, mouse \
         tracking, focus, DEC 2026/2048/2031, cursor/keypad/paste/wrap/charset, hidden cursor, \
         scroll region), cancels torn escapes, closes OSC 8 links, and restores theme colours \
         (OSC 104/110–119, `colors`) without clearing screen or scrollback. Records \
         `modes-restored reason=manual source=ctl reverted=<csv|-> bytes=<n>` on `timeline` and \
         replies `OK reset reverted=<csv|-> bytes=<n>` (`flush` drops unread macOS tty input and \
         appends `discarded=<n|->`). `invoke ResetTerminal` resets the front window's focused \
         pane (`source=menu`).",
    )
    .input(Input::PTY),
    v(
        "tab",
        Write,
        Status,
        App,
        "drive a window's tabs (new|N|next|prev|close [N]|move <from> <to>)",
        "— flagless drives the front window; `@<sid> tab …` drives the window hosting `<sid>`. \
         `N`, `<from>`, `<to>` and `<active>` in `OK <active> <count>` are 0-based (`inspect \
         app/v1 tabs` `index=`). Refuses closing a tab with a running job (`ERR close refused (a \
         running job armed the last-tab confirm)` or `ERR close refused (a running job in that \
         tab)`) or an uncheckpointed native doc (`--headless` never refuses).",
    )
    .input(Input::PTY),
    v(
        "pane",
        Write,
        Status,
        App,
        "pane <left|right|up|down>: move keyboard focus to the adjacent pane",
        "— acts on the front window's active tab (`spawn split=v|h` creates splits) and replies \
         `OK moved=<1|0>` (read layout rects and focus via `panes`).",
    )
    .input(Input::PTY),
    v(
        "open",
        Write,
        Status,
        App,
        "open a native tab app: `open app settings [/route]`",
        concat!(
            "or `open app markdown|editor <local-file-uri>`. `open <target> [close]` opens or \
             closes a front-window surface (",
            open_targets!(),
            "; ",
            open_aliases!(),
            "), replying `OK opened <target>` or `OK closed <target>`. `open fact-menu \
             <model|effort|path|branch|context|usage|top-effort|account|close>` opens or closes \
             that composer-rule menu. While any session is held, opening `menu`, `tab-menu`, \
             `overview`, `autopilot-menu` or `fact-menu` is `ERR halted` (see `hold`)."
        ),
    )
    .input(Input::NONE),
    v(
        "edit",
        Write,
        Status,
        App,
        "@<sid> edit <file-uri> [line=<L>[:<C>]]: open it in a new Editor tab beside <sid>",
        "— also `[create=1]` (a missing file opens empty and its first save creates it), \
         `[for=<word>]` (the program it is for, shown in the view's header) and `[wait=0]`. \
         Owner only (`ERR scope`). Replies `OK edit ticket=<t> view=<v> doc=<d> \
         uri=<canonical-uri>` once the tab opens to the right of `<sid>`'s, and holds `<sid>` \
         (`status editing=`; its typing is `ERR busy editing`) until `await edit ticket=<t>` \
         resolves; `wait=0` skips the hold. Errors: `ERR unsupported headless`, `ERR no-window`, \
         `ERR refused \
         <not-regular|too-large|invalid-utf8|symlink|file-provider|missing|bad-uri|unreadable>`, \
         `ERR timeout ticket=<t>` (30 s).",
    )
    .lane(Wait)
    .input(Input::NONE),
    v(
        "doc",
        Read,
        Lines,
        App,
        "live Editor documents: doc ls [uri=<file-uri>] | doc read <doc-id|file-uri> \
         [lines=<A>-<B>]",
        "— `doc ls` replies `OK <n>` then `doc=<d> uri=<canonical-uri> seq=<s> dirty=0|1 \
         disk=same|changed|missing|unknown views=<v,…> ticket=<t,…|-> cursor=<view>:<L>:<C>|- \
         sel=<view>:<L>:<C>-<L>:<C>|-`. `doc read` replies `OK <nbytes> doc=<d>`, \
         `<n>:<h6>|<text>` lines (`<h6>` = 6-hex FNV-1a-64 of line), and `-- lines=<total> \
         seq=<s> [next=<n>] [cut=<n>]` (256 KiB cap per read; `ERR unknown doc <ref>` if not \
         open; `doc apply|save|log` answer `ERR unsupported`).",
    )
    .forms(&[
        form(Word("read")).framing(Bytes),
        form(Word("apply")).op(Write),
        form(Word("save")).op(Write).framing(Status),
    ])
    .input(Input::NONE),
    v(
        "act",
        Write,
        Status,
        App,
        "act app/v1 view <view-id> <ui-key> <action> [value]: one native-app action",
        "",
    )
    .input(Input::NONE),
    v(
        "settings",
        ConfigWrite,
        Status,
        App,
        "settings [open|close|toggle|section <name>]; `settings set|unset <key> [value...]`",
        "— bare `settings` is `toggle`. `set` and `unset` write `aterm.toml`, answering `OK \
         saved: <key> = <value>`, `OK \
         removed: <key>; now <effective>` (an unset, or a set to the key's default) or `OK <key>: \
         unchanged`.",
    )
    .input(Input::NONE),
    v(
        "invoke",
        Write,
        Status,
        App,
        "invoke <action>: fire a menu action by name (enabled-gated; names via `controls menu`)",
        "— `Paste` honors the unread-input gate (it takes a leading `unread=ok`). Off macOS, \
         `CloseTab` or `Quit` over a running job (and a multi-tab `Quit` on Windows) parks an \
         in-window confirmation, replying `OK confirm pending \
         kind=<quit|close-window|close-tab|close-pane> window=<n>; …`; `confirm yes|no` answers \
         it.",
    )
    .input(Input::PTY.with(Input::APP_UNREAD)),
    v(
        "confirm",
        Write,
        Status,
        App,
        "confirm yes|no: answer a close or quit waiting in the window",
        "— answers an in-window confirmation (`controls front` `confirm open=true`) raised by a \
         control client (`invoke CloseTab|Quit`): `OK answered=<yes|no> \
         kind=<quit|close-window|close-tab|close-pane>`. Refuses confirmations raised by a human \
         at the window (`ERR a quit is waiting for the person at the window`) or when none is \
         pending (`ERR nothing is waiting for an answer`).",
    )
    .input(Input::PTY),
    va(
        "person",
        Write,
        Status,
        App,
        OwnerOnly,
        "person look <window>|off | person menubar press <at>: the person, stood in for (debug \
         builds)",
        "— debug-only stand-in for the human at the window; requires `ATERM_DEBUG_PERSON_INVOKE=1` \
         at launch (`ERR denied: …` or `ERR person needs a debug build` otherwise). `person look \
         <window>|off` (`window=` from `ls`) marks a headless window as looked at for `status \
         end_seen=`, the tab `•` dot and Done for you (`OK person look window=<n>` / `OK person \
         look off`; `ERR no window <n>`, `ERR a window looks for itself`). `person menubar press \
         <at>` clicks the `statusmenu` row at path `at=<i>[.<j>…]` from `chrome` (`OK pressed \
         \"<label>\"`, or `ERR no row at <at>`, `ERR not an action`, `ERR disabled \"<label>\"`, \
         `ERR no menu-bar item off macOS`).",
    )
    .input(Input::NONE),
    v(
        "rain",
        Write,
        Status,
        App,
        "rain [status|on|off|toggle]: matrix rain for the focused window's front session",
        "(status prints `config_enabled= session_override= effective= engine= active= \
         scope=window|focused-pane focused= animating=`, plus a live engine's `weather= density= \
         tick= scanned= material= emitting= vis= drain= seq= streak=`)",
    )
    .input(Input::NONE),
    v(
        "streak",
        Read,
        Status,
        App,
        "streak [status]: output-streak (PRISM WAKE) state for the focused window",
        "(prints `config_enabled= sound_key= motion_animates= focused= fx_focused= serious_sound= \
         serious_visual= sounds_master= volume= engine=none|live panes= active= intensity= tail= \
         max_streaks= idle_secs=`; `serious_visual` is false under Serious Mode, \
         `motion_animates` reflects the unfocused demotion of `motion=full`, `fx_focused` is true \
         on focus, a typed wake or a recording, and `panes` counts the split panes' engines)",
    ),
    v(
        "fx",
        Write,
        Status,
        App,
        "inspect or arm a one-shot rainbow-kitty celebration on the front session",
        "(`fx [status|celebrate [sig=<char>] [bars=<1..4>] [on=green|enter|done]]`; `celebrate` \
         arms a `bars`-bar (default 2, max 4; 1.6 s/bar; key `sig=`, default `C`; 30 s cooldown) \
         celebration firing on the next exit-0 command block (`on=green`, OSC 133/633 `D`), keyed \
         Enter (`on=enter`), or codex `done` (`on=done`, OSC 7501 `state=done` in `status \
         program_state=`). Replies `armed=none|green|enter|done sig= bars= live= bar= drive= \
         cooldown_ms= style=`)",
    )
    .input(Input::NONE),
    v(
        "tone",
        Read,
        Status,
        App,
        "tone [status]: tone-of-typing state for the focused window",
        "(prints tone= effective= knob= sounds= volume= audio= active= window_chars= inferences= \
         dropped= seam= engine_sound= trail= focused= serious_sound= motion_stage= shed= revives= \
         reopens_left=; `seam=open` means a keypress sounds now, while `seam=closed:<reason>` \
         names \
         `sounds-off|volume-zero|trail-off|serious-mode|unfocused|host-inert|host-wedged|resize-quiet|engine-silent`; \
         `audio=opening|live|paused|stopped|failed|reopening|wedged|inert` tracks the audio worker \
         and `reopens_left=` the device reopen budget; typed text is never reported)",
    ),
    v(
        "kitty",
        Write,
        Lines,
        App,
        "kitty [wear <key>]: the cursor cat collection, and which one to wear",
        "— bare `kitty` lists `cat key= coat= iris= age= seen= worn=` rows (`worn=1` on the active \
         cat). `kitty wear <key>` pins and switches to `<key>`, replying `worn key= coat= iris=` \
         (refused when effects or `[sparkle_words.feline]` are off or `<key>` is unknown).",
    )
    .input(Input::NONE),
    v(
        "trail",
        Read,
        Lines,
        App,
        "trail [<n>]: the focused window's last <n> cursor-trail spawn-seam verdicts",
        "(default all, ring cap 32), newest last: `admission seq= phase=licensed|declined reason= \
         age_ms= origin= target= alt= licence=`. `licence=` is \
         `key|inflight|insert|rewrite|recall|motion|none` (`send`/`feed`, guarded `key if=` and \
         background input stay dark); `reason=` on decline is \
         `no-fresh-hint|no-credits|off-shape|program-row|hidden-relocation`. `trail status` \
         prints one standing-state row: `trail style= resolved= config_enabled= effective= \
         focused= motion= motion_stage= shed= intensity= sound_seam= licensed= declined= \
         last_decline_reason= spawns= ribbon_active= ribbon_look= ribbon_segments= \
         ribbon_hue_bands= ribbon_drawn= ribbon_curtain_ms= field= sparks= momentum= \
         momentum_display= momentum_glow= flow= combo= combo_best= glow_active= pet_active= \
         cat_active= block_fill= block_fill_rgb= block_fill_base= block_fill_base_from= \
         pet_action= pet_content= pet_pending= pet_body= pet_focus= pet_reason= pet_anchor= \
         pet_event_seq= pet_pose= inserts_delivered= inserts_lit= inserts_retracted= \
         last_insert_cells= inflight_licensed= inflight_forgotten= credits= swallowed_no_echo= \
         park_returns= park_flushed= issue= comet_cells= comet_alpha_max=`, and when rainbow \
         kitty owns the frame appends `v2_quads= v2_halos= v2_stars= v2_meteors= v2_bridged= \
         ribbon_retired= ribbon_followed= ribbon_follow_missed=`. In `--headless`, ticks advance \
         only when a capture (`image` or `video`) drives the clock, and row 0 has no sky band \
         above it (`v2_stars=0` on row 0). Read-only; typed text is never reported.",
    ),
    v(
        "custody",
        Read,
        Status,
        Session,
        "custody: why the reading position or the highlight last moved",
        "— one `last=<transition|none> event=<0-8|-> changed=<transition|none> \
         took_selection=<transition|none> offset= owner=user|tail selection=yes|no scrollback=` \
         line: the latest press-custody transition (`last=`) and the latest that moved the \
         viewport or the selection (`changed=`).",
    )
    .json(Json::Refused),
    v(
        "hover",
        Write,
        Status,
        App,
        "hover <on|off>: set the drop-target highlight (no toggle form)",
        "",
    )
    .input(Input::NONE),
    v(
        "spawn",
        Write,
        Status,
        App,
        "spawn [window=] [raise=<t|f>] [cwd=] [split=<v|h>] [connected=…|agent=…]: OK <sid>",
        "— mints a session (a new tab, or `split=v|h` splitting the focused pane), addressable at \
         once. `theme=<name>` gives it a session theme (`theme session`; `ERR theme \"<name>\": \
         <why>`). `window=<id>` (`inspect app/v1 tabs`; `window=0` on `--headless`; `ERR no such \
         window <id>`) or `@<sid> spawn` picks the window; `raise=` defaults to `true` with no \
         window named, else `false`. A bad `cwd=` is `ERR no such folder <path>` or `ERR no \
         access to folder <path>`. Connected form (Owner-only): `spawn \
         connected=controlled|controller place=window|tab of=<sid> [cwd=<path>]` mints a `both` \
         connection with `of=<sid>` (`controller` sets `ATERM_OBSERVE_SESSION_ID`; `place=window` \
         is `ERR headless` without a GUI). Worker form (Owner-only): `spawn … \
         agent=<claude|codex|gemini|agy|muse> [model=<m>] [autopilot=<land|cruise>] [from=<sid>] \
         len=<n>` replies `GO <n>`, reads `<n>` task bytes (`1..=65536`, no NUL; `ERR spawn \
         agent=<word>: the task is <n> bytes (max 65536)` beyond; `aterm ctl spawn agent=<word> \
         [model=<m>] [cwd=<p>] -` reads stdin), writes `<state>/tasks/` (`ATERM_TASK_FILE`), and \
         at the login shell's (`zsh`/`bash`/`fish`) first prompt launches the interactive agent \
         (`--prompt-interactive=` for `gemini`/`agy`, `GEMINI_CLI_TRUSTED_FOLDERS_PATH=` for \
         `gemini`, `--trust-workspace` for `muse`; `codex` is codex-alab, which carries the \
         session itself, `supervisor_proven=1`, and whose landing marks `task` `done \
         why=landed` with its rollout's report). Sets `task` to `working` (`from=<sid|ctl>`) \
         and `autopilot` to `land` (or `autopilot=cruise`; `meta set autopilot land` switches \
         later); marks `stalled why=shell` if no shell prompt within 20 s, `stalled why=startup` \
         if no agent composer (for `codex`, no proven claim) within 120 s of `since_ms=`, or \
         `stalled why=held` if stuck 120 s at an unhandled screen. Refuses with `ERR \
         unsupervised: <switch>` when the window harness cannot supervise (`[harness] enabled = \
         false`, `headless = false`, or `autopilot = \"off\"`).",
    )
    .input(Input::NONE),
    v(
        "close",
        Write,
        Status,
        Session,
        "@<sid> close: retire that session and close its pane",
        "— replies `OK closed <sid>` once the session leaves the registry, its pane closes, its \
         PTY is hung up, and `exits` records `reason=ctl-close by=<caller>`. Collapses one split \
         pane (closing the tab on its last pane, the window on its last tab, and the process on \
         its last window). Refuses if unresolvable (`ERR no such session`), if a job is running in \
         that pane (`ERR close refused (a running job in that pane)` or `ERR close refused (a \
         running job armed the last-tab confirm)`; `--headless` never refuses), or if a view or \
         teardown still holds the session (`ERR close deferred (<why>)`, `ERR session view \
         changed during close`, `ERR close refused (the pane is still open)`).",
    )
    .input(Input::PTY),
    v(
        "task",
        Read,
        Status,
        Session,
        "task [mark <state> …]: a worker's task record: OK task rev= state= from= since_ms=",
        "— `result_bytes= [why=] [exit=] [result_path=] [fact= fact_rev=] agent= attention=` (or \
         `OK task -` when the session has no task). `state=`: `working`, `done` (under Land, work landed on remote \
         `main` and defended with `Nothing left:`), `exited` (`exit=<code>`), or `stalled` \
         (`why=startup|shell|held`; it resumes `working why=moved` or `working why=turn`). `why=` \
         is `landed|marker|done-check|exit|shell|startup|held|turn|moved`. `rev=` increments on \
         record changes and on `meta set attention owner=supervisor|needs|codex-alab` changes \
         (`status` and `sessions` carry `task_state=<state|-> task_rev=<n>`, `subscribe … events` \
         emits `EVENT <local> task rev=<n> state=<s> attention=<0|1> [why=<w>] [exit=<n>]`, and \
         `turn` verdicts carry `task_rev=`). `fact=<rule>@v<n>#<ordinal>|-` is the occupancy door's \
         word for a managed worker's line the harness held for its manager (written under the \
         owner's `[harness] occupancy = \"prompt\"` alone), shown once first edited on the reply, \
         `await task` rows and the event (`fact= fact_rev=` at their end); `fact_rev=` moves on its \
         own, never `rev=`, and no verb takes `fact=` as input. `task mark <working|done|exited|stalled> [from=<s-…|ctl>] \
         [why=<word>] [exit=<n>] [result_bytes=<n>] [result_path=<pct>]` (Owner-only; `ERR denied` \
         for edges; `ERR busy update-committing` if a seamless handoff blocks > 10 s) updates the \
         record and replies with `changed=<0|1>`.",
    )
    .forms(&[form(Word("mark")).owner_only()]),
    v(
        "result",
        Read,
        Lines,
        Session,
        "result [full]: a worker's result, the report it ended its task with",
        "— replies `OK <n> source=transcript session=<id> uuid=<row> bytes=<n>` (or `OK <n> \
         source=screen program=<agent> bytes=<n>` for Gemini CLI, agy and Muse Code, `OK <n> \
         source=rollout thread=<id> turn=<id> bytes=<n>` for codex-alab) followed by `<n>` lines \
         (up to 24000 chars, ending with `… truncated; full: <path>` when truncated; `result \
         full` adds `full=1` and returns the full transcript exchange since the task prompt, \
         `why=no-transcript` for a screen or rollout result). Snapshotted when `task` is marked \
         `done` (a transcript's retried on `no-reply`/`no-marker`, updating `result_bytes=`). \
         Errors: `ERR no task`, `ERR result unavailable \
         why=<working|stalled|exited|no-transcript|no-reply|no-marker|empty|no-check|no-report|no-snapshot|unreadable|no-prompt|no-words>`.",
    ),
    v(
        "ready",
        Read,
        Status,
        Session,
        "ready [<ms>|timeout=<ms>]: block until the session is alive and idle",
        "— answers `OK ready prompt` (shell integration shows a prompt) or `OK ready idle` (60 ms \
         of quiet); `OK timeout` after 30 s by default (max 600 s); `ERR exited` if the session \
         ends.",
    )
    .lane(Wait)
    .json(Json::Refused),
    v(
        "await",
        Read,
        Status,
        Session,
        "await <idle|seq|match|gone|block|frame|inbox|consent|momentum|agent|task|log|sys|edit> \
         …: wait",
        "— blocks on a condition (`timeout=` default 30000 ms, 300000 for `await consent`, max \
         600000; `OK timeout` exits 124 in `aterm ctl`; `ERR exited` when the session ends; 64 \
         waits at once across tabs, then `ERR control server busy`). `await idle <ms>`: no \
         output for `<ms>`. `await seq [<n>]`: content `seq` > `<n>`, by default the current \
         one, and an `<n>` past it answers at once (`OK seq <current>`). `await match <re> [rows \
         <a> <b>]` / `await gone <re> [rows <a> <b>]`: a visible row matches, or every row in \
         range is clear of `<re>` on a closed DEC 2026 frame or for 1 s (`ERR bad rows` on an \
         empty span). `await frame`: an open DEC 2026 \
         frame closes (60 s hang cap; `text --json` `\"frame\":\"open\"`). `await block`: a \
         command finishes after the wait is armed, by an OSC 133/633 `D` that follows its `C`, \
         which follows its prompt's `A` and `B`, with no other `A` and no reset (RIS) between \
         them (a `D` out of that order answers nothing; `wait` also answers a command that \
         already finished). `await sys <condition> \
         [for=<dur>] [scope=tab|aterm]`: the terms `aterm help sys` lists, joined by `and`; a \
         bare `quiet` is `cpu<25% wait<0.25 \
         gpu<10% throttle=none swap<1MB/s`, a term it cannot measure dropped as \
         `skipped=<metric>:<why>` (`OK sys <metric>=<value> … held_ms= as_of= next=`, a `job=` \
         term's reading `job=<id> verdict= class= [wait=] [quiet_ms=] [phi=] [exit=]`; `ERR \
         unavailable <metric> why=<…>`, `ERR gone job=<id>`). On a volume, one or two of \
         `free>=<size|room>`, `free><size|room>` (`room` is `status disk_room_mb=`) or \
         `disk=<word>[,…]` joined by `and`, `[vol=<absolute path>]`: for an owner-class caller \
         an unmet `free` term first frees unused build caches (`disk=` alone only waits) (`OK \
         sys free_bytes= disk= vol= freed_bytes= held_ms=0 as_of=`, `ERR unmet free_bytes= \
         need_bytes= vol= freed_bytes= why=<spent|held-back|cleanup-off|settings|not-reclaimed> \
         hogs= largest_bytes= [key=idle_build_dirs]`, or `ERR unavailable free why=<…>`). \
         `await momentum <floor 0..=1>`: `trail status momentum=` falls to `<floor>` (`OK \
         momentum <v>`). `await inbox since=<id>|re=<off> [topic=<t>] [kinds=<k|hold,...>]`: \
         an inbox row past `since=` of those kinds, a reply to `re=<off>` (an `ack … verdict=` \
         wakes it), a `post to=say:<t>` broadcast, or, with `hold` among `kinds=`, a `hold` \
         change (`OK inbox <id>`, `OK inbox hold=0|1`; `ERR bad since high=<n>`). `await \
         agent <word>[,<word>…] [since=<rev>]`: `status agent=` is one of \
         `busy|prompt|question|idle|survey|unknown|-|wall|wall:<kind>` at a `rev` past \
         `since=`, such as a `settle=submitted` turn's `agent_rev=` (`OK agent <word> \
         rev=<n>`). `await task [@<sel>[:<rev>[:<fact_rev>]][,...]] [any|all] [since=<rev>] [facts=1 \
         [fact_since=<fact_rev>]]`: a `task` `rev` past `since=` whose state is not `working` or \
         whose attention stands, with `facts=1` a fact word noted past `fact_since=` (by default \
         the `fact_rev` at the wait; a clear wakes nothing), or the session closing (`OK <n>`, or \
         `OK <n> status=timeout`, then `@<sid> rev=<n> \
         state=<working|done|exited|stalled|-|closed> agent=<word> attention=<pct|-> \
         result_bytes=<n> [fact=<word|-> fact_rev=<n>]` rows). `await wake [timeout=<ms>]`: \
         the session's next ready wake (`wake`), its `need=` holding, a build slot reserved where \
         that names `slots` (`OK wake #<n> ready since=<ms>[ slots=<usable>/<n>]`), or its last \
         open wake's end (`EXPIRED wake #<n> why=<due|unless|agent|path|off|admit|rm|taken|told>`); \
         one parked at a time a session (`ERR busy await wake parked`), `ERR usage await wake: no \
         wake pending`, and `ERR denied wake: <why>` from outside the session's tree as `wake` \
         says. A reply that cannot be written leaves the wake ready. `await consent [kind=debugger] [timeout=<ms>]`: this session's \
         consent (`privacy`'s `full_disk_access=` and its `session` row's `fs_consent=` and \
         `attribution=`; with `kind=debugger`, `debugger mode=`) differs from the first completed \
         reading (`OK consent fs_consent= fda= attribution= elapsed_ms=`, or `OK consent \
         debugger_mode= elapsed_ms=`). `await log need|open=<lvl>|close=<lvl>|kind=<k> \
         [since=<seq> ep=<e>]` (`<lvl>` in `session|run|turn|step`; `need` also answers a run's \
         `out=fail` close, the session's close and a `mark` need (`conf=claim`); `status log_seq= \
         log_ep=`): `OK seq=<n> ep=<e> [as_of=<head> latched=1] [gap=<why>:<lost>] <record>` or \
         `OK gap why=<w> lost=<k> resync_to=<seq> ep=<e>`. `await edit ticket=<t>|uri=<file-uri> \
         [timeout=<ms>]` (Owner-only): `OK edit done result=<finished|closed|aborted> saved=<0|1> \
         seq=<n>`, `ERR unknown ticket` or `ERR hangup`.",
    )
    .lane(Wait)
    .forms(&[form(Word("task")).framing(Lines)]),
    v(
        "wait",
        Read,
        Status,
        Session,
        "wait [<ms>]: block until the running command completes (OSC 133)",
        "— answers `OK complete <id> exit=<code|->`; with nothing running it returns the newest \
         completed block at once. `ERR exited` once the session exits or is closed; `OK timeout` \
         after 30 s by default (max 600 s); without shell integration it can only time out or end. \
         It reads the shell's marks alone, so a program's unfinished DEC 2026 frame never holds \
         it past its timeout.",
    )
    .lane(Wait)
    .json(Json::Refused),
    v(
        "subscribe",
        Read,
        Push,
        Session,
        "subscribe [@<sel>[,...]|@*] <streams> [since=][every-frame]: push \
         DELTA/EVENT/GAP/BYTES/MAIL.",
        "streams: `screen,cursor,cells,bytes,events,mail,log,sessions` (plus inline modifiers \
         `timestamps`/`ts` for `T <local|*> <t_us>` prefixes and `trim` for `screen,trim`; no \
         target means `@.`; resumed via `since=<seq>` for content, `since-turn=<id>` and \
         `since-block=<id>` for `events`' turns and blocks, or `since-log=<seq> log-ep=<e>` for \
         `log`, an anchor needing a single target; each target \
         acks `sub <local> <sid>[ log_seq=<seq> log_ep=<e>]`; `every-frame` re-reads `cells` on \
         every wake and sends a frame only when it changed). `log` pushes `LOG <local> \
         <record>` (`log:lvl=<session|run|turn|step>` and `log:class=turn` filter) and `GAP \
         <local> log-lost=<n> why=<evicted|handoff|reset|…> resync_to=<seq> ep=<e>`. `events` \
         pushes `EVENT <local> \
         turn|block-complete|meta|title|bell|inbox|inbox-seen|post|fetch|post-landed|hold|topic \
         …` (`turn <id> submitted=<0|1> status=<s> dur_ms=<n> by=<who>`, `agent <word> rev=<n> \
         gen=<e.s> fp=<hex16>` from `status agent=`, `task rev=<n> state=<s> attention=<0|1> \
         [why=<w>] [exit=<n>]`, `status phase=<p> outcome=<o> exit_code=<n|-> signal=<n|-> \
         confidence=<c>`, `human` (`status human_ms=`), `harness-policy gen=<n>`, `job <verdict> \
         job=<id> class=<class> rev=<n> [task=<id>] [code=<n>]` (`status job=`), `wake #<n> \
         ready` (a declared wake's condition held, under the owner's `[harness] occupancy = \
         \"prompt\"` alone; `waits`), `resize n=<k> \
         id=<last> to=<CxR> run=<r>[ dropped=<n>]`, `render desync-risk run=<r> displaced=<+-k> \
         net=zero at=<ms>` / `render healed run=<r> after_ms=<ms>` (`status render=`), `closing \
         reason= by=`, `exited`, and `GAP <local> events-dropped=<n>`: the timeline's rows are a \
         view of the session `log`, and a GAP counts the rows past the watermark it no longer \
         holds). `sessions` (Owner-only) \
         pushes `EVENT * session-created <sid>[ watch=deferred]`, `EVENT * session-exited <sid> \
         reason=<shell-exit|ctl-close|ui-close|window-close|unknown>`, `EVENT * \
         fabric-retire <sid>`, and `EVENT * bye <quit|handoff>`. `mail` pushes live `MAIL <local> \
         id=<n> off=<n> from=<p> kind=<k>[ re=<n>][ topic=<t>]` (filtered by `mail:kinds=<k,..>`, \
         `mail:from=<class|principal>`, `mail:topic=<t>`) and `GAP <local> mail-dropped=<n>`. \
         `screen`, `cursor` and `cells` push only whole DEC 2026 frames: a target's unfinished \
         frame puts its content push off (to the frame's 60 s hang detector at most, then as it \
         stands) and holds up no other target or stream. Capped at 1/16 of fd limit (`ERR \
         subscription capacity busy; retry`).",
    ),
    va(
        "topic",
        Owner,
        Lines,
        Session,
        OwnerOnly,
        "topic add <t> [since=head|@<off>] | topic drop <t> | topic ls: broadcast opt-ins",
        "— receiver opt-in for `post to=say:<topic>` (`<t>` in `[a-z0-9][a-z0-9._-]{0,31}`, max 32 \
         topics per session before `ERR topics full (max 32): drop one first`). `topic [ls]` \
         replies `OK <n> seen=<w>` then `topic <t> since=<head|@<off>>` rows. `topic add <t> \
         [since=head|@<off>]` replies `OK <t> since=<head|@<off>> added=<0|1>`; `topic drop <t>` \
         replies `OK <t> dropped=<0|1>` (unsubscribed broadcasts to `deliver` are refused with \
         `ERR not-subscribed`). Delivered broadcasts carry `topic=<t>` in `inbox`, `subscribe … \
         mail`, and `await inbox since=<id> topic=<t>`.",
    )
    .forms(&[form(AnyOf(&["add", "drop"])).framing(Status)])
    .input(Input::NONE),
    va(
        "rekey",
        Owner,
        Status,
        Session,
        OwnerOnly,
        "rekey shell=<pid> | withdraw: re-key a degraded tab",
        "— POSIX only; requires `@<sid>`. For a tab with `status integration=degraded` (or \
         `integration_rev=frozen`) while `<pid>` leads the foreground process group (`ERR pid <n> \
         is not the foreground process group` otherwise): writes a one-use `0600` key/loader file \
         and replies `OK rekey ttl=<s> path=<path>` (refuses with `ERR rekey integration=on`, \
         `integration=off`, or `pending`). `rekey withdraw` settles a waiting key (`OK rekey \
         withdrawn|taken|none`).",
    )
    .input(Input::NONE),
    v(
        "inbox",
        Read,
        Lines,
        Session,
        "inbox [<n>] [since=<id>] [--peek] [--meta]: this session's message rows",
        "— header: `OK <n> hold=<0|1> holder=<turn-id|lease:<holder>|-> seen=<id> \
         bus_head=<off> oldest_on_bus=<@off|-> dropped=<n> pending=<n>`, then `msg <id> \
         off=<n> t=<ms> from=<p> kind=<k> trust=<human|agent|relayed|screen> [re=<n> [re-id=<id>]] \
         [verdict=<handled|refused|deferred>] [dl=<ms>] [late=1] [demoted=<k>] [via=<p,...>] \
         [topic=<t>] len=<n> [more=1] [truncated=1] text=<pct>` (text preview capped at 512 B; \
         use `inbox get <id>` or `inbox get @<off>` for full bodies; lost bus records below \
         retention return `ERR retained-from=<floor>` or `ERR no such record`) and queued \
         outbound `post <id> to=<> kind=<> off=- len=<n> [key=<token>]` rows. `<n>` selects the \
         newest `n` `msg` rows above `since=`; `--peek` leaves rows unlisted and `--meta` omits \
         `text=`.",
    ),
    v(
        "inbox get",
        Read,
        Bytes,
        Session,
        "inbox get <id> | inbox get @<off>: one message's body, by row id or by broker offset",
        "— replies `OK <nbytes>[ truncated=1 len=<true-size>]` then `<nbytes>` raw bytes (up to \
         256 KiB, unpreviewed `inbox` `text=`) from the local ring without moving `seen=`. `inbox \
         get @<off>` fetches by broker offset (including evicted `dropped=` rows) from the ring or \
         through the bridge (`OK <nbytes> off=<n> from=<p> kind=<k> trust=<t> [re=<n>] [dl=<ms>] \
         [demoted=<k>] [via=<p,...>] [verdict=<v>] [truncated=1 len=<n>]` + body; errors: `ERR \
         retained-from=<floor> off=<n>` below `oldest_on_bus=`/`bus_head=` retention, `ERR no such \
         record off=<n>`, `ERR timeout off=<n>` after 10 s, `ERR fabric <state> off=<n>`, `ERR \
         fetch failed reason=<why> off=<n>` (a malformed, refused or log-replaced answer), or \
         `ERR busy` past 8 parked reads).",
    )
    .forms(&[form(AtWord).lane(Wait)]),
    v(
        "inbox seen",
        Write,
        Status,
        Session,
        "inbox seen <id> [handled|refused|deferred]: advance the HANDLED watermark (and ack)",
        "— lists all rows `<= <id>`, advances `seen=`, and replies `OK seen=<w>` (emitting `EVENT \
         <local> inbox-seen <id> off=<n> floor=<f|-> seen=<w>`). Adding \
         `handled|refused|deferred` attaches `verdict=<v> kind=<k> from=<p>` for row `<id>` and \
         queues a receipt (`kind=ack re=<off> verdict=<v>`) on `outbox` for an `ask` or `task` \
         row (unless `[fabric] receipts = false`), waking the sender's `post --wait-ack` and \
         `await inbox re=<off>`. Answerable during `hold`.",
    )
    .input(Input::NONE),
    v(
        "post",
        Write,
        Status,
        Session,
        "post to=<@<sid>[@<node>]|<principal>|say[:<topic>]> kind=<k> [opts] [--] <text>: send a \
         message",
        "— Owner-only (refused to edge tokens; exempt from `hold`). `to=say[:<topic>]` broadcasts \
         on `say/<topic>` (`<topic>` in `[a-z0-9][a-z0-9._-]{0,31}`, default `say`) to sessions \
         with `topic add <topic>` (`topic=<t>`). \
         `kind=<ask|answer|task|report|note|ack|control>`. Options before `--`: `[re=<n>] \
         [dl=<ms>] [via=<p,...>] [key=<token>] [--wait[=<ms>]] [--wait-ack[=<ms>]] [len=<n>]` \
         (inline text <= 4 KiB, or `len=<n>` + raw bytes <= 256 KiB; no `to=fleet`). `--wait` \
         (default on for `ask`/`task`) waits for broker landing (`OK <id> off=<n>`, or `OK <id> off=<n> \
         dup=1` when deduped by idempotency `key=<token>`). `--wait-ack` (`ask`/`task` only) also \
         waits for the recipient's `inbox seen` receipt (`kind=ack re=<off> verdict=<v>` -> `OK \
         <id> off=<n> ack=<verdict> msg=<inbox id>`, `ERR expired id=<n> off=<n>` when `dl=` \
         lapses and delivers `kind=expired re=<off> dl=<ms>` / marks later replies `late=1`, or \
         `ERR timeout id=<n> off=<n>`). If the link cannot report landing, `--wait` returns `ERR \
         fabric <absent|stalled|stale|disconnected> id=<n> <queued=1|no-bridge=1>` or `ERR \
         timeout id=<n>` (the message remains queued in `outbox` — do not re-post; crashes that \
         fail to carry it retire with `reason=not-carried`, and full queues return `ERR outbox \
         full`). Permanent broker/routing refusals retire the post and return `ERR \
         <unroutable|ambiguous|undeliverable|refused|subject-budget> id=<n>` (`state=exited` is \
         `unroutable`).",
    )
    .lane(Wait)
    .body(Body::PostLen)
    .input(Input::NONE),
    va(
        "deliver",
        Write,
        Status,
        Meta,
        BridgeOnly,
        "deliver <sid> off=<n> from=<p> kind=<k> ...: put one bus record in a session's inbox",
        "— Bridge-only; idempotent on `off=` (`ERR quota` past 64 unread rows from one peer; \
         `ERR not-subscribed` for a `topic=` the session never added). `deliver <sid> \
         landed=<post-id> off=<n>` completes an outbound \
         `post`, `deliver <sid> fetched=<off> …` (or `err=<why>`) answers a parked `inbox get \
         @<off>`, and `deliver <sid> receipt=<rid> off=<n|->` retires an owed receipt.",
    )
    .input(Input::NONE),
    va(
        "link",
        Write,
        Status,
        Meta,
        BridgeOnly,
        "link up rtt=<ms> | down reason=<token> [rtt=<ms>] | log <id> | bye <quit|handoff>: the \
         link",
        "— Bridge-only: `up rtt=<ms>` sets `status fabric=connected`; `down reason=<token>` \
         sets `fabric=stalled` and wakes `post --wait` with `ERR fabric stalled id=<n> \
         queued=1`; `log <identity> [continues=<off> was=<id>]` binds the broker log generation; \
         `mirror …`, `set-aside files=<n>` and `quarantine torn=<k>` report the bus mirror and \
         quarantined state files; `bye <quit|handoff>` acknowledges `EVENT * bye`. Replies `OK` \
         (`OK stale=1` to a superseded bridge lane).",
    )
    .input(Input::NONE),
    va(
        "hello",
        Write,
        Status,
        Meta,
        BridgeOnly,
        "hello lane=<n> [build=<token>]: bridge lane grammar and build handshake",
        "— Bridge-only: records `bridge_lane=` and `bridge_build=` for `fabric status` and \
         replies `OK lane=<n> build=<b>` (`OK lane=<n> build=<b> log=turns` to a `log=turns` \
         hello, `OK stale=1` to a superseded lane). `refill=<lane,...>` naming `say` says the \
         bridge refills broadcasts; `log=auto|turns` is its bus mirror's mode (`fabric status \
         bridge_log=`), and only `log=turns` opens `log-take`.",
    )
    .input(Input::NONE),
    va(
        "hold",
        Write,
        Status,
        Meta,
        OwnerOnly,
        "hold <sid> on|off [reason=<pct>] [origin=fleet|local]: the drive halt, local or fleet",
        concat!(
            "— replies `OK hold=<0|1>` (plus `journaled=0` when an `origin=local` hold's \
             crash-journal sync took over 5 s). A session has four independent hold slots: \
             `origin=local` (Owner sets and lifts it), `origin=fleet` (the fabric bridge does; \
             from Owner it is `ERR denied`), `origin=person` (`reason=taken-over`, only the \
             person's Take Over / Hand Back in the window; `invoke TakeOver|HandBack` is `ERR \
             denied`), and `origin=remote` (a fleet drive's). While any slot stands, every \
             PTY-reaching verb that resolves to the session answers `ERR halted reason=<r> \
             origin=<person|remote|fleet|local>` from ANY scope — `send key ctrl feed feed-bin \
             paste paste-bin mouse pointer resize focus signal restart reset turn close invoke \
             hwkey pane tab confirm askpass` — and while any session is held the \
             session-independent window verbs (`invoke`, `hwkey`, `pane`, `pointer`, `tab`, \
             `confirm`, `askpass`, and `open \
             menu|palette|tab-menu|autopilot-menu|fact-menu|overview|connections`) are refused \
             across the instance: \"",
            hold_cost!(),
            "\" `spawn`, `survey`, `await`, `result`, `post`, `inbox seen`, `meta set`, `lease` \
             and every read verb stay answerable. When the bridge disconnects, bridge-governed \
             sessions show `status hold_advisory=fabric-lost` (under `[fabric] \
             hold_on_bridge_lost = true` they take a `reason=fabric-lost origin=fleet` hold). \
             Holds survive seamless updates (`timeline` `carried=1`; a hold during the final \
             commit is `ERR busy update-committing`) and PTY-keeper crash restores."
        ),
    )
    .input(Input::NONE),
    va(
        "outbox",
        Read,
        Bytes,
        Meta,
        BridgeOnly,
        "outbox [<max>] [earlier-receipts=1]: drain the queued outbound posts, bodies included",
        "— Bridge-only peek, up to `<max>` posts or 4 MiB, retiring nothing until `outbox \
         sent`: `OK <nbytes>`, then each `post sid= id= to= kind= … len=<n>` line and its body, \
         then `receipt sid= rid= off= verdict= kind= from=` and `fetch sid= off=` lines, and a \
         final `more` line when `<max>` or the budget left posts queued. `earlier-receipts=1` \
         adds the receipts owed for inbox rows from an earlier bus log, each \
         ending ` log=earlier`.",
    ),
    va(
        "outbox sent",
        Write,
        Status,
        Meta,
        BridgeOnly,
        "outbox sent <sid> <id> off=<n|-> [dup=1]: retire one queued outbound post",
        "— Bridge-only, idempotent: `off=<n>` records the broker offset (`dup=1` when \
         deduped by `key=`) and wakes `post --wait`; `off=- [reason=<word>]` retires the post \
         as undeliverable (`post --wait` gets `ERR <reason> id=<n>`).",
    )
    .input(Input::NONE),
    va(
        "log-take",
        Read,
        Bytes,
        Meta,
        BridgeOnly,
        "log-take [n=<1..64>] [tier=wire|meta] [key=<hex>]: the bus mirror's next slots",
        "— Bridge-only, after `hello log=turns` (else `ERR mirror off`): `OK <nbytes> \
         slots=<k>[ more=1]`, then `<sid> <ep>:<seq>:<ms> <v=2 body>` lines; nothing advances \
         until `log-landed`.",
    ),
    va(
        "log-landed",
        Write,
        Status,
        Meta,
        BridgeOnly,
        "log-landed <sid> <ep>:<seq>:<ms>...: the bus mirror's slots landed",
        "— Bridge-only: advances each session's bus-mirror cursor, replying one word per pair \
         (`ok|stale|done|fenced|adopting|gone|ahead`).",
    )
    .input(Input::NONE),
    va(
        "fabric",
        Owner,
        Status,
        Meta,
        OwnerOnly,
        "fabric status|attach [<command...>]|retire <sid>...: the bridge of a RUNNING instance",
        "— Owner-only. `fabric status` replies `OK \
         state=<absent|connected|stale|stalled|disconnected> supervised=<0|1> command=<pct|-> \
         argv=<pct|-> reason=<token|-> rtt_ms=<n|-> link_age_ms=<n|-> build=<token> \
         bridge_lane=<n|-> bridge_build=<token|-> lane_faults=<n>[ bridge_log=<auto|turns> \
         mirror=<starting|on|stopped|capped|budget|unretained>[:<reason>]]` (`bridge_log=` and \
         `mirror=` once the bridge's `hello` said `log=auto|turns`; `state=` is each session's \
         `status fabric=`). `fabric attach [<command...>]` arms the bridge supervisor \
         once per process using the `[fabric] command` in `aterm.toml` or launch argv (`OK \
         attached command=<pct>`, or `ERR fabric no command`, `ERR fabric not configured \
         command=<pct>`, `ERR fabric already supervised command=<pct>`, `ERR fabric not \
         executable program=<pct> reason=<not-found|not-a-file|no-exec-bit|not-on-path>`). \
         `fabric retire <sid>...` requests the bridge to publish `exited` for unhosted ghost sids \
         via `EVENT * fabric-retire <sid>` (`OK requested=<n>`, or `ERR hosted <sid>`).",
    )
    .input(Input::NONE),
    va(
        "sessions",
        Owner,
        Lines,
        Meta,
        OwnerOnly,
        "OK <n> local sid parent state title meta= nonce= window= active= wfocus= detail= path=",
        "— Owner-only roster (relayed by `ls` and folded by `windows`). Columns per session: \
         `nonce=<hex32>` (fabric `epoch=`), `window=<id|none|->` (`window=0` on \
         `--headless`), `active=<1|0|->`, `wfocus=<1|0|->` (most recently focused window), \
         `detail=<pct|->`, `path=<frozen|live>`, `program=<name|-> agent=<word|-> \
         agent_detail=<pct|-> agent_rev=<n> agent_since_ms=<ms> agent_gen=<e.s|-> \
         agent_fp=<hex16|->`, `human_ms=<ms|->`, `role=<pct|-> attention=<pct|-> \
         user_title=<pct|->`, `supervisor=<pct|->`, `autopilot=<off|cruise|land|->`, \
         `path_evidence=<measured:<ms>|unconfirmed:<ms>|carried|->`, `copy=<managed|foreign|->`, \
         `upgrade=<-|<state>/<to>/<why>/<age>>`, `land_at=<unix-s|-> task=<open|landed|theirs|-> \
         pause=<unix-s|->`, and `task_state=<working|done|exited|stalled|-> task_rev=<n>`. \
         `sessions bridge` returns identity-only `<local> <sid> nonce=<hex32>` rows without \
         main-thread or terminal locks. `sessions status` returns `<local> <sid> <nonce> \
         sid=<local> revision=<n> hold=<0|1> hold_origin=<person|remote|fleet|local, \
         comma-joined, or -> detail=<pct|-> agent=<word|->`.",
    )
    .input(Input::NONE),
    va(
        "who",
        Read,
        Lines,
        Meta,
        OwnerOnly,
        "OK <n> local sid driving= watchers= turns= state nonce= fgpgid=",
        "— Owner-only, answered off the main thread: `driving=<turn-id|lease:<holder>|->`, \
         `nonce=<hex32>`, `fgpgid=<pid|->`.",
    ),
    va(
        "survey",
        Read,
        Lines,
        Meta,
        OwnerOnly,
        "survey [facts|content]: each session's program, state, task, prompt, last words and tool",
        "— Owner-only and fleet-readable (`[fleet] allow`). Replies `OK <n>`, an `instance pid= \
         host=<pct> version= sessions=<n> level=<facts|content> cut=<n> at_ms=<unix ms> node=<32 \
         hex|-> instances=<n>[ drive=1]` header (`drive=1`: a fleet reader this machine lets \
         drive it), and one `session` row per session: `local= sid= state= \
         program=<pct|-> program_kind=<agent|shell|program|-> agent=<word|-> agent_detail=<pct|-> \
         agent_ms=<ms> human_ms=<ms|-> bg=<n|-> task=<state|-> task_rev=<n> task_from=<pct|-> \
         task_ms=<ms|-> model=<pct|-> effort=<pct|-> ctx_left_pct=<n|-> detail=<pct|-> \
         doing=<pct|-> role=<pct|-> supervisor=<pct|-> attention=<1|pct|-> needs=<word|-> \
         hold_by=<word|-> held_from=<32 hex|-> typed_by=<pct|-> typed_ms=<ms|-> bells=<n> \
         bells_sounded=<n> last_bell=<ms>/<sound>/<program|->[,…]|->`, plus under `content` (the \
         local default; `attention=` is the excerpt, `needs=ask` selects the ask excerpt and \
         `by=harness` turns are skipped in `prompt=`): `user_title= title= cwd= branch= prompt= \
         said= cmd= rows=` (<= 400 B per excerpt, credentials/fleet codes redacted, fleet \
         replies capped at 256 KiB; `rows=-` on an unfinished DEC 2026 frame, never waited for). \
         `aterm ctl survey [--fleet] [--budget <chars>]`, `survey [--json] [--sid <s>] [--fleet \
         | --host <h>]` (`--timeout <s>` with `--fleet` or `--host`) and `survey --brief \
         [--budget <chars>]` (`log n` + `brief all`) format multi-instance and fleet digests and \
         take no `facts`/`content` word.",
    )
    .for_fleet(),
    va(
        "exits",
        Owner,
        Lines,
        Meta,
        OwnerOnly,
        "exits [<n>] [since=<id>]: the instance EXIT LEDGER (Owner-only)",
        "— one `exit <id> t=<ms> sid=<sid> local=<n> \
         reason=<shell-exit|ctl-close|ui-close|window-close|unknown> exit_code=<n|-> \
         by=<sid|human|ctl|bridge|->` line per exited session, read from the closed-session \
         index (its last 512 closes, in the order they left the registry; also pushed live via \
         `subscribe @<sid> events` `closing reason= by=` and `subscribe … sessions` \
         `session-exited <sid> reason=`).",
    )
    .input(Input::NONE),
    va(
        "whoami",
        Owner,
        Status,
        Meta,
        OwnerOnly,
        "OK <session> <nonce> <scope>",
        "",
    )
    .input(Input::NONE),
    v(
        "family",
        Read,
        Lines,
        Session,
        "the session's parent + direct children",
        "— `family [<sid>]` -> `OK <n>` then `self <sid> <state> <title>`, `parent` (`parent - - \
         -` for a root), `child` rows, and (for Owner) `pushes`/`pushed-by`/`pulls`/`pulled-by` \
         peer rows.",
    )
    .forms(&[form(Args).owner_only()]),
    v(
        "edges",
        Read,
        Lines,
        Session,
        "inbound capability edges (--json). alias: grants",
        "",
    )
    .json(Json::Emits),
    v(
        "grants",
        Read,
        Lines,
        Session,
        "inbound capability edges (--json). alias: edges",
        "",
    )
    .json(Json::Emits),
    va(
        "grant",
        Owner,
        Status,
        Meta,
        OwnerOnly,
        "grant <src-sid> <op>: mint a cross-session edge onto `whoami`'s session",
        "— Owner-only; replies `OK <token-hex>`. `<op>` is \
         `read-screen|write-input|signal|config-write|clipboard-write|derive-loop`; a \
         `<src-sid>` that is a session-log actor word is `ERR reserved`.",
    )
    .input(Input::NONE),
    va(
        "revoke",
        Owner,
        Status,
        Meta,
        OwnerOnly,
        "revoke <edge-hex> | revoke src=<sid>: remove one edge, or all from <sid> onto `whoami`'s \
         session",
        "— Owner-only. A token answers `OK`, `src=` answers `OK <removed>` (`ERR no such edge` \
         when none matched); a token also finds a grant a closed session held.",
    )
    .input(Input::NONE),
    va(
        "connect",
        Owner,
        Status,
        Meta,
        OwnerOnly,
        "connect dst=<sid> src=<sid> [kind=pull|push|both]: set the src->dst session connection",
        "— Owner-only and declarative: a new kind revokes the connection's tokens and mints each \
         op afresh, so the connection's edges are exactly `kind=` (default `both`), then replies \
         `OK read-screen=<hex> write-input=<hex> signal=<hex>`, naming each op the connection \
         holds; a `src=` that is a session-log actor word is `ERR reserved`.",
    )
    .input(Input::NONE),
    va(
        "disconnect",
        Owner,
        Status,
        Meta,
        OwnerOnly,
        "disconnect dst=<sid> src=<sid> [kind=pull|push|both]: dissolve that connection",
        "— Owner-only; `kind=pull` revokes only the pull half; replies `OK <removed>`.",
    )
    .input(Input::NONE),
    va(
        "flows",
        Owner,
        Lines,
        Meta,
        OwnerOnly,
        "the instance's session-connection graph: OK <n> + `<src> <dst> <op>` per live edge",
        "across every session's table (Owner-only); `--json` groups them per pair: \
         `{\"flows\":[{\"src\",\"dst\",\"ops\":[…]}]}`.",
    )
    .input(Input::NONE),
    va(
        "raise",
        Owner,
        Status,
        Meta,
        OwnerOnly,
        "raise <sid>: raise the window hosting that session and select its tab",
        "— Owner-only: the connection card's Show button, on the wire.",
    )
    .input(Input::NONE),
];

/// The spec for `verb`, or `None` for an unknown verb.
#[must_use]
pub fn spec(verb: &str) -> Option<&'static VerbSpec> {
    VERBS.iter().find(|s| s.name == verb)
}

/// `verb`'s [`Input`] class ([`VerbSpec::input_class`]), [`Input::NONE`] for an unknown verb.
#[must_use]
pub fn input_of(verb: &str) -> Input {
    spec(verb).map_or(Input::NONE, VerbSpec::input_class)
}

/// A request's verb facts with its sub-form resolved ([`spec_for`]).
#[derive(Clone, Copy, Debug)]
pub struct Resolved {
    /// The row naming the request: a listed sub-form row (`inbox get`) or the verb's.
    pub row: &'static VerbSpec,
    /// The authority class the request needs.
    pub op: OpClass,
    /// Only an owner-class connection may run the request, whatever its row's [`Access`].
    pub owner_only: bool,
    /// How the reply is framed, a `--json` on a [`Json::Emits`] row included.
    pub framing: Framing,
    /// The lane the request costs.
    pub lane: Lane,
    /// Whether a successful reply needs the artifact acknowledgement.
    pub ack: bool,
}

/// The one resolution of a request's verb facts: `verb` and `rest`, the text after it (no selector).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn spec_for(verb: &str, rest: &str) -> Option<Resolved> {
    let words = rest.trim_start();
    let (sub, after_sub) = words.split_once(char::is_whitespace).unwrap_or((words, ""));
    let sub_row = (!sub.is_empty())
        .then(|| {
            VERBS
                .iter()
                .find(|s| s.name.strip_prefix(verb).and_then(|n| n.strip_prefix(' ')) == Some(sub))
        })
        .flatten();
    let (row, tail) = match sub_row {
        Some(row) => (row, after_sub),
        None => (VERBS.iter().find(|s| s.name == verb)?, rest),
    };
    let tail = if row.input_class().contains(Input::APP_UNREAD) {
        let trimmed = tail.trim_start();
        match trimmed.split_once(char::is_whitespace) {
            Some(("unread=ok", after)) => after,
            None if trimmed == "unread=ok" => "",
            _ => tail,
        }
    } else {
        tail
    };
    let form = row.forms.iter().find(|f| f.matches(tail));
    let framing = if matches!(row.json, Json::Emits) && json_flag(rest) {
        Lines
    } else {
        form.and_then(|f| f.framing).unwrap_or(row.framing)
    };
    Some(Resolved {
        row,
        op: form.and_then(|f| f.op).unwrap_or(row.op),
        owner_only: form.is_some_and(|f| f.owner_only),
        framing,
        lane: form.and_then(|f| f.lane).unwrap_or(row.lane),
        ack: form.and_then(|f| f.ack).unwrap_or(row.ack),
    })
}

/// Whether a request's text after its verb carries the `--json`/`json` token.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn json_flag(rest: &str) -> bool {
    rest.split_whitespace()
        .any(|t| t == "--json" || t == "json")
}

/// The verbs that answer JSON under `--json`/`json` ([`Json::Emits`]), in table order — the list `help` advertises.
#[cfg_attr(trust_verify, trust::skip)]
pub fn json_verbs() -> impl Iterator<Item = &'static str> {
    VERBS
        .iter()
        .filter(|s| matches!(s.json, Json::Emits))
        .map(|s| s.name)
}

/// Whether `verb` is owner-only per the table, whatever its op-class.
#[inline]
#[must_use]
pub fn is_owner_only(verb: &str) -> bool {
    spec(verb).is_some_and(|s| matches!(s.access, Access::OwnerOnly))
}

/// Whether `verb` is answered for any authenticated scope before target resolution ([`Access::AnyScopeMeta`]).
#[inline]
#[must_use]
pub fn is_any_scope_meta(verb: &str) -> bool {
    spec(verb).is_some_and(|s| matches!(s.access, Access::AnyScopeMeta))
}

/// The bridge lane's grammar generation, which the bridge and the instance serving it each state in `hello` (`lane=`) so either can tell which side reads less of the lane.
pub const BRIDGE_LANE: u32 = 6;

/// The lane that added `link quarantine torn=<k>` (5 in [`BRIDGE_LANE`]'s list), for both sides.
pub const LINK_QUARANTINE_LANE: u32 = 5;

/// Whether `verb` is bridge-only per the table: no token unlocks it, Owner included.
#[inline]
#[must_use]
pub fn is_bridge_only(verb: &str) -> bool {
    spec(verb).is_some_and(|s| matches!(s.access, Access::BridgeOnly))
}

/// Trailer emitted after a complete guarded response.
pub const ARTIFACT_REPLY_CHALLENGE_PREFIX: &str = "ACK-CHALLENGE ";
/// Client echo sent only after consuming the complete response and challenge.
pub const ARTIFACT_REPLY_ACK_PREFIX: &str = "ACK ";

/// [`CONTROL_BUSY_REPLY`] as a literal, so its framed line is one `concat!`.
macro_rules! control_busy_reply {
    () => {
        "ERR control server busy; retry"
    };
}
/// The control socket's busy reply.
pub const CONTROL_BUSY_REPLY: &str = control_busy_reply!();
/// [`CONTROL_BUSY_REPLY`] framed as the wire carries it.
pub const CONTROL_BUSY_LINE: &str = concat!(control_busy_reply!(), "\n");

/// What leads a [`host_gone_line`], up to its pid.
const HOST_GONE_PREFIX: &str = "ERR gone pid=";

/// The forward's host went away.
#[must_use]
pub fn host_gone_line(pid: u32, sock: &str) -> String {
    format!("{HOST_GONE_PREFIX}{pid} sock={sock}\n")
}

/// The pid and socket a [`host_gone_line`] names, or `None` for any other line (its own `\r\n` ignored).
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

/// Maximum UTF-8 bytes in one control-protocol reply line.
pub const MAX_CONTROL_REPLY_LINE_BYTES: usize = 8 << 20;

/// The screen's generation, `(invalidation epoch, content seq)`, spelled `<epoch>.<seq>` on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScreenGen {
    /// The terminal-wide invalidation epoch.
    pub epoch: u64,
    /// The active grid's content seq within that epoch.
    pub seq: u64,
}

impl ScreenGen {
    /// Parse the wire spelling `<epoch>.<seq>` (two unsigned decimals, each a `u64`); any other shape is `None`.
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
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.epoch.to_string())?;
        f.write_str(".")?;
        f.write_str(&self.seq.to_string())
    }
}

/// The rows fence, `turn if-rows=<first>-<last>:<hex16>`.
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
    /// Parse the wire spelling `<first>-<last>:<hex16>` (two unsigned decimals with `first <= last`, exactly sixteen hex digits).
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

    /// FNV-1a-64 over `rows`, each followed by `\n` (over every visible row, the screen's `hash=` stamp).
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

/// Whether `nonce` is the 32-hex-digit (either case) nonce of a guarded artifact acknowledgement.
#[inline]
#[must_use]
pub fn valid_artifact_ack_nonce(nonce: &str) -> bool {
    nonce.len() == 32 && nonce.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Whether a successful reply for this request may carry exact capture/video retention.
#[inline]
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn artifact_reply_requires_ack(verb: &str, request: &str) -> bool {
    spec_for(verb, head(request).rest).is_some_and(|r| r.ack)
}

/// The reply framing of `verb` for the full `request` line.
#[inline]
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn framing_of(verb: &str, request: &str) -> Framing {
    spec_for(verb, head(request).rest).map_or(Status, |r| r.framing)
}

/// One catalog row: the verb name in the [`CATALOG_TEXT_COLUMN`] gutter, then `text`.
fn catalog_row(name: &str, text: &str) -> String {
    format!("{name:<w$} {text}", w = CATALOG_TEXT_COLUMN - 1)
}

/// The short catalog: one `<name padded> <summary>` row per verb, in table order, answered to a bare `help` and bounded by [`SHORT_CATALOG_MAX_BYTES`].
pub fn catalog_lines() -> impl Iterator<Item = String> {
    VERBS.iter().map(|s| catalog_row(s.name, s.summary))
}

/// The full catalog: one `<name padded> <summary detail>` row per verb, in table order (`help --full`, and the `aterm help introspection` manual).
pub fn catalog_lines_full() -> impl Iterator<Item = String> {
    VERBS.iter().map(|s| catalog_row(s.name, &s.help_line()))
}

#[cfg(test)]
mod tests {
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

    #[test]
    fn a_selector_body_names_something_only_when_it_is_a_nonblank_word() {
        for named in [".", "self", "env", "12", "s-0f1e", "*", "s-ab:3"] {
            assert!(selector_body_names_something(named), "{named:?}");
        }
        for nothing in ["", " ", "\t", " s-ab", "s-ab ", "s-\nab", "s-\u{1b}ab"] {
            assert!(!selector_body_names_something(nothing), "{nothing:?}");
        }
        assert!(EMPTY_SELECTOR.starts_with("ERR usage"));
        assert!(!EMPTY_SELECTOR.contains('\n'));
    }

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

    #[test]
    fn json_flag_switches_every_json_capable_verb_to_lines() {
        assert!(json_verbs().count() >= 10, "non-vacuity");
        for v in json_verbs() {
            for flag in ["--json", "json"] {
                assert_eq!(
                    framing_of(v, &format!("{v} {flag}")),
                    Lines,
                    "{v} {flag} must frame as Lines (the json_ok `OK 1\\n<body>` shape)"
                );
            }
        }
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
        assert_eq!(framing_of("image", "@s-a image bytes"), Lines);
        assert_eq!(framing_of("image", "@child image read"), Lines);
        assert_eq!(framing_of("cast", "@s-a cast frames"), Lines);
        assert_eq!(framing_of("cast", "@s-a cast drift"), Lines);
        assert_eq!(framing_of("cast", "cast drift max_runs=4 rows=10"), Lines);
        assert_eq!(framing_of("video", "video frames"), Lines);
        assert_eq!(framing_of("video", "@s-a video frames count=5"), Lines);
        assert_eq!(framing_of("video", "video 3 full"), Status);
        assert_eq!(framing_of("video", "video status"), Status);
        assert_eq!(framing_of("temporal", "temporal status"), Status);
        assert_eq!(framing_of("temporal", "temporal"), Bytes);
        assert_eq!(framing_of("temporal", "temporal 1200"), Bytes);
        assert_eq!(framing_of("temporal", "@s-a temporal status"), Status);
        assert_eq!(framing_of("topic", "topic"), Lines);
        assert_eq!(framing_of("topic", "topic ls"), Lines);
        assert_eq!(framing_of("topic", "topic add build ttl=60"), Status);
        assert_eq!(framing_of("topic", "@s-a topic drop build"), Status);
        assert_eq!(framing_of("await", "await task @s-a,@s-b all"), Lines);
        assert_eq!(framing_of("await", "@s-a await task since=3"), Lines);
        assert_eq!(
            framing_of("await", "await task @s-a:3:1,@s-b any facts=1 fact_since=2"),
            Lines
        );
        assert_eq!(framing_of("await", "@s-a await task facts=1"), Lines);
        assert_eq!(framing_of("await", "await agent idle"), Status);
        assert_eq!(framing_of("await", "await inbox since=1"), Status);
        assert_eq!(
            framing_of("edit", "@s-a edit file:///tmp/x.txt line=3"),
            Status
        );
        assert_eq!(framing_of("await", "await edit ticket=1"), Status);
        assert_eq!(
            framing_of(
                "await",
                "@s-a await edit uri=file:///tmp/x.txt timeout=5000"
            ),
            Status
        );
        assert_eq!(framing_of("task", "@s-a task mark done"), Status);
        assert_eq!(framing_of("task", "@s-a task"), Status);
        for (request, framing) in [
            ("doc", Lines),
            ("doc ls", Lines),
            ("doc ls uri=file:///tmp/x.txt", Lines),
            ("@s-a doc ls", Lines),
            ("doc read 3", Bytes),
            ("doc read file:///tmp/x.txt lines=1-20", Bytes),
            ("@s-a doc read doc=3 lines=40-", Bytes),
            ("doc apply 3 base=7 len=12", Lines),
            ("doc log 3 since=2", Lines),
            ("doc save 3", Status),
            ("@s-a doc save file:///tmp/x.txt", Status),
        ] {
            assert_eq!(framing_of("doc", request), framing, "{request}");
        }
        assert_eq!(framing_of("result", "@s-a result"), Lines);
        assert_eq!(framing_of("result", "result full"), Lines);
        assert_eq!(framing_of("cursor", "cursor --json"), Lines);
        assert_eq!(framing_of("dims", "dims json"), Lines);
        assert_eq!(framing_of("cursor", "@s-a cursor --json"), Lines);
        assert_eq!(framing_of("edges", "edges --json"), Lines);
        assert_eq!(framing_of("dims", "dims"), Status);
        assert_eq!(framing_of("meta", "meta"), Status);
        assert_eq!(framing_of("meta", "meta set title build agent"), Status);
        assert_eq!(framing_of("meta", "@s-a meta unset icon"), Status);
        assert_eq!(framing_of("timeline", "timeline"), Lines);
        assert_eq!(framing_of("occupancy", "occupancy"), Lines);
        assert_eq!(framing_of("occupancy", "occupancy why"), Lines);
        assert_eq!(
            framing_of("occupancy", "@s-a occupancy check --replay 24h"),
            Lines
        );
        assert_eq!(framing_of("timeline", "@s-a timeline 10 since=3"), Lines);
        assert_eq!(framing_of("resizes", "resizes"), Lines);
        assert_eq!(framing_of("resizes", "@s-a resizes 20 since=4"), Lines);
        assert_eq!(framing_of("trail", "trail"), Lines);
        assert_eq!(framing_of("trail", "@s-a trail 5"), Lines);
        assert_eq!(framing_of("status", "status"), Status);
        assert_eq!(framing_of("status", "@s-a status"), Status);
        assert_eq!(framing_of("wake", "wake label=built when=in:5m"), Status);
        assert_eq!(
            framing_of(
                "wake",
                "@self wake label=b when=pid:42 exited need=slots>=1"
            ),
            Status
        );
        assert_eq!(framing_of("wake", "@s-a wake rm #1"), Status);
        assert_eq!(framing_of("wake", "wake taken #2"), Status);
        assert_eq!(framing_of("waits", "waits"), Lines);
        assert_eq!(framing_of("waits", "waits why"), Lines);
        assert_eq!(framing_of("waits", "@s-a waits why #2"), Lines);
        assert_eq!(framing_of("await", "await wake"), Status);
        assert_eq!(framing_of("await", "@self await wake timeout=5000"), Status);
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
        let one = spec("lines").unwrap();
        assert_eq!(
            one.entry_lines(),
            vec![catalog_row("lines", &one.help_line())]
        );
        assert!(spec("image").unwrap().entry_lines().len() > 5);
    }

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
            ("hold", Write, Status, Meta, OwnerOnly),
            ("outbox", Read, Bytes, Meta, BridgeOnly),
            ("outbox sent", Write, Status, Meta, BridgeOnly),
            ("log-take", Read, Bytes, Meta, BridgeOnly),
            ("log-landed", Write, Status, Meta, BridgeOnly),
        ] {
            let s = spec(name).unwrap_or_else(|| panic!("{name} is in the table"));
            assert_eq!(s.op, op, "{name} op-class");
            assert_eq!(s.framing, framing, "{name} framing");
            assert_eq!(s.target, target, "{name} target");
            assert_eq!(s.access, access, "{name} access");
        }
        assert_eq!(framing_of("inbox", "inbox"), Lines);
        assert_eq!(framing_of("inbox", "inbox 20 since=4 --meta"), Lines);
        assert_eq!(framing_of("inbox", "inbox get 7"), Bytes);
        assert_eq!(framing_of("inbox", "@s-a inbox get 7"), Bytes);
        assert_eq!(framing_of("inbox", "inbox seen 7 handled"), Status);
        assert_eq!(framing_of("inbox", "@s-a inbox seen 7"), Status);
        assert_eq!(
            framing_of("deliver", "deliver s-a off=9 from=h-x kind=task"),
            Status
        );
        assert_eq!(framing_of("hold", "hold s-a on reason=x"), Status);
        assert_eq!(framing_of("post", "post to=@s-b kind=ask hello"), Status);
        assert_eq!(framing_of("log-take", "log-take n=32 tier=wire"), Bytes);
        assert_eq!(framing_of("log-landed", "log-landed s-a 1:4:2"), Status);
        assert_eq!(framing_of("outbox", "outbox"), Bytes);
        assert_eq!(framing_of("outbox", "outbox 8"), Bytes);
        assert_eq!(framing_of("outbox", "outbox sent s-a 3 off=91"), Status);
    }

    #[test]
    fn copy_and_settings_have_their_own_op_class() {
        assert_eq!(spec("copy").unwrap().op, OpClass::ClipboardWrite);
        assert_eq!(spec("settings").unwrap().op, OpClass::ConfigWrite);
        assert_eq!(spec("scroll").unwrap().op, OpClass::Read);
        assert_eq!(spec("select").unwrap().op, OpClass::Read);
    }

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
        assert_eq!(
            s.json,
            Json::Emits,
            "privacy --json is part of the contract"
        );
        assert_eq!(framing_of("privacy", "privacy --json"), Lines);
        assert_eq!(framing_of("privacy", "privacy"), Lines);
    }

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
        assert_eq!((session.op, session.target), (Read, App));
        let op = |rest: &str| spec_for("theme", rest).map(|r| r.op);
        assert_eq!(op("session"), Some(Read));
        assert_eq!(op("session status"), Some(Read));
        assert_eq!(op("session set Hearth"), Some(Write));
        assert_eq!(op("session clear"), Some(Write));
        assert_eq!(op("palette session"), Some(Read));
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

    #[test]
    fn spec_for_prefers_the_listed_sub_form_row() {
        let r = |verb: &str, rest: &str| spec_for(verb, rest).expect("a table verb");
        let seen = r("inbox", "seen 7");
        assert_eq!((seen.row.name, seen.op), ("inbox seen", Write));
        assert_eq!(r("inbox", "").row.name, "inbox");
        assert_eq!(r("inbox", "20 since=4").op, Read);
        let upper = r("theme", "SET Nord");
        assert_eq!((upper.row.name, upper.op), ("theme", ConfigWrite));
        assert_eq!(r("theme", "set Nord").row.name, "theme set");
        assert_eq!(r("theme", "preview Nord").op, Write);
        assert_eq!(r("theme", "revert").op, Write);
        assert_eq!(r("theme", "status").op, Read);
        assert_eq!(r("theme", "").op, Read);
        assert_eq!(r("inbox", "get @5").lane, Wait);
        assert_eq!(r("inbox", "get 5").lane, Rpc);
        assert_eq!(r("mainscreen", "").lane, Wait);
        assert_eq!(r("await", "task @s-a").framing, Lines);
        assert_eq!(r("cast", "drift rows=4").lane, Wait);
        assert_eq!(r("cast", "frames").lane, Rpc);
        assert_eq!(r("paste", "hi").lane, WaitWhenAimed);
        assert_eq!(r("video", "3 full").lane, Wait);
        assert_eq!(r("video", "status").lane, Rpc);
        assert_eq!(r("text", "").lane, Rpc);
        assert_eq!(r("edit", "file:///tmp/x.txt line=3").lane, Wait);
        assert_eq!(r("await", "edit ticket=1").lane, Wait);
        assert_eq!(r("doc", "ls").op, Read);
        assert_eq!(r("doc", "read 3 lines=1-20").op, Read);
        assert_eq!(r("doc", "log 3").op, Read);
        assert_eq!(r("doc", "apply 3 len=12").op, Write);
        assert_eq!(r("doc", "save 3").op, Write);
        assert_eq!(r("doc", "read 3").lane, Rpc);
        assert!(r("update", "check").owner_only && r("update", "apply").owner_only);
        assert_eq!(r("update", "check").lane, Wait);
        assert!(!r("update", "status").owner_only && !r("update", "").owner_only);
        assert!(r("family", "s-1").owner_only && !r("family", "").owner_only);
        assert!(r("pointer", "click").owner_only && !r("pointer", "move 3 4").owner_only);
        assert!(r("pointer", "mods link").owner_only);
        assert!(r("pointer", "unread=ok click").owner_only);
        assert!(r("pointer", "unread=ok mods link").owner_only);
        assert!(!r("pointer", "unread=ok move 3 4").owner_only);
        assert!(!r("update", "unread=ok check").owner_only);
        assert!(r("window", "overview").owner_only && !r("window", "prefs").owner_only);
        assert!(r("window", "connections").owner_only);
        assert!(r("controls", "overview").owner_only && !r("controls", "").owner_only);
        assert!(r("controls", "connections").owner_only);
        assert!(r("task", "mark done").owner_only && !r("task", "").owner_only);
        let wake = r("wake", "label=b when=in:5m");
        assert_eq!((wake.op, wake.lane, wake.owner_only), (Write, Rpc, false));
        assert_eq!(
            wake.row.input_class(),
            Input::NONE,
            "a wake writes no session input"
        );
        assert_eq!(r("wake", "rm #1").op, Write);
        assert_eq!((r("waits", "why").op, r("waits", "").lane), (Read, Rpc));
        assert_eq!(r("await", "wake").lane, Wait);
        assert_eq!(r("meta", "unset icon").op, Write);
        assert_eq!(r("meta", "").op, Read);
        assert_eq!(r("log", "take").row.name, "log");
        assert!(spec_for("bogus", "").is_none());
    }

    #[test]
    fn the_hold_help_enumerates_exactly_the_halt_set() {
        const MARKER: &str = "from ANY scope — `";
        let halts = |verb: &str| input_of(verb).contains(Input::PTY);
        let hold = spec("hold").expect("hold ships").help_line();
        let listed: Vec<&str> = hold
            .split_once(MARKER)
            .expect("the `hold` detail enumerates the halted set after `from ANY scope — `")
            .1
            .split_once('`')
            .expect("the enumeration is one back-ticked run")
            .0
            .split_whitespace()
            .collect();

        for verb in &listed {
            assert!(
                halts(verb),
                "`help hold` names `{verb}` as refused under a halt and the halt \
                 does not refuse it"
            );
        }
        for verb in VERBS.iter().map(|s| s.name).filter(|n| halts(n)) {
            assert!(
                listed.contains(&verb),
                "a halt refuses `{verb}` and `help hold` — the only shipped \
                 statement of what a halt refuses — does not name it"
            );
        }
        let refused = VERBS.iter().filter(|s| halts(s.name)).count();
        assert_eq!(
            listed.len(),
            refused,
            "the `hold` detail lists {} verbs for a set of {refused}: a repeat \
             is a second literal in the same sentence",
            listed.len(),
        );
    }
}
