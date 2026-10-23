// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The control-protocol verb table: the typed source of truth for every control-socket verb.

/// The longest control request line the server reads, its newline not counted (a frame's body is bounded on its own).
pub const MAX_REQUEST_LINE: usize = 64 * 1024;

/// The longest task a WORKER is given (`spawn agent=<word> … len=<n>`, its task the n raw bytes after the line): the server refuses a longer frame unread and closes, and the client refuses one before sending it.
pub const MAX_TASK_BYTES: usize = 64 * 1024;

/// The longest `timeout=` a `turn` over another Mac's drive takes (the step-in): the driven Mac cuts a drive connection at 30 s, the dial and the reply inside it.
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

/// Why a selector that names nothing is refused, in the server's `ERR` line and in the client's refusal before it sends one: a bare `@` (what `@$W` becomes when `$W` is empty) or an `@` whose body is blank ([`selector_body_names_something`]).
pub const EMPTY_SELECTOR: &str = "ERR usage: empty or malformed selector: an @ names a session \
     (@self, @<sid>, @<n>; @. is the active tab)";

/// Whether a selector's body (what follows its `@`) names something: it is not empty and holds no whitespace or control character.
#[inline]
#[must_use]
pub fn selector_body_names_something(body: &str) -> bool {
    !body.is_empty() && !body.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// The byte cap of one keyed attention entry (`meta set attention owner=<k> <text>`), after trim: the server refuses a longer one and a supervisor cuts its text to it.
pub const META_ATTENTION_KEYED_MAX: usize = 200;

/// A verb's authority class.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OpClass {
    /// Pure observer / view-state control (incl.
    Read,
    /// Injects the human input vocabulary the driven program observes, plus the app-drive verbs (`turn`/`spawn`/`close`/`tab`/`invoke`).
    Write,
    /// The out-of-band `signal` class.
    Signal,
    /// Rewrites the durable on-disk config (`settings set|unset` → `aterm.toml`): greater than `Write`, so a keystroke-injection edge cannot flip a default-off security knob.
    ConfigWrite,
    /// Moves the selection onto the system clipboard (`copy`, the exfil boundary): split out of `Read` because it leaves the process.
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
    /// Owner-only: only an OWNER-CLASS connection may run it, regardless of op-class (`sessions`/`who`/`grant`/`revoke`/`whoami`) — the instance god-token, and the bridge connection, which carries Owner's power.
    OwnerOnly,
    /// Bridge-only: only the fabric bridge connection may run it (the `socketpair` end the instance keeps for its `aterm-link serve` child, served with the bridge scope pre-resolved instead of an `AUTH` line): `deliver`, `link`, `hello`, `outbox`, `outbox sent`.
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
    /// APP-level: acts on the resolved instance's FRONT window; the selector routes to the instance (window/chrome/controls/open/settings/invoke/…).
    App,
    /// Self-scoped / fleet-wide; a selector is rejected (sessions/who/grant/version/…).
    Meta,
}

/// Which of the control server's lanes a request costs (`aterm-gui`'s `control_lanes.rs`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lane {
    /// Answers at once: an RPC lane.
    Rpc,
    /// Mostly waits — for an event, a settle, the owner, the bridge — or hands the connection to a relay: a wait lane.
    Wait,
    /// A wait lane when an explicit `@<sid>` other than `@self` aims it, else an RPC lane: a `paste` into a session not on screen waits, bounded, for its bytes to reach the program, and a flagless one goes to the front tab.
    WaitWhenAimed,
}

/// What a verb does with a `--json`/`json` token in its request.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Json {
    /// The token is an ordinary argument: a payload verb (`send --json`, `search json`, `meta set title json`) receives it verbatim.
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
    /// A fleet halt refuses it (design §5.3): it can reach a session's PTY, signal its child, retire a live session, or re-aim the seam that writes.
    pub const PTY: Self = Self(1);
    /// Writes input bytes to a session's PTY, so the unread-input gate asks it.
    pub const WRITES: Self = Self(1 << 1);
    /// Self-targeted, it passes the per-session self-feed floor, so a raw client cannot drive an output→observe→write storm on its own session.
    pub const FLOORED: Self = Self(1 << 2);
    /// Refused while a `turn` or another connection's hard drive lease holds the session, so raw writes cannot corrupt a turn's output.
    pub const LEASED: Self = Self(1 << 3);
    /// Takes a leading `id=<epoch>:<producer>:<seq>` idempotency key (§11.2's set): every keyed verb can put bytes on the PTY, and no other verb may claim a mark.
    pub const KEYED: Self = Self(1 << 4);
    /// Takes a leading `if=<re>` guard, the `if-gen=`/`if-fp=`/`if-human=` fences and `unread=ok`: the keystroke-shaped writes a driver answers a prompt with.
    pub const GUARDED: Self = Self(1 << 5);
    /// Can drive a modal overlay or native Settings tab in front through the App input seam (or races a human in one), so a scoped caller's flagless request is checked against the front surface first.
    pub const FRONT_DRIVING: Self = Self(1 << 6);
    /// A successful reply carries ` seq=<content_seq>`, the baseline `await seq` waits past.
    pub const STAMPS_SEQ: Self = Self(1 << 7);
    /// An authorized attempt, malformed or not, is a newer input boundary for the cursor's move licence: it can inject bytes, signal the child, or move the cursor's coordinate space.
    pub const FENCES: Self = Self(1 << 8);
    /// An App-lane gesture that takes a leading `unread=ok`, lifting the unread-input gate for itself.
    pub const APP_UNREAD: Self = Self(1 << 9);
    /// Its argument is the program's own bytes (`send`, `feed`, a frame's body), not a gesture: a self-targeted request is delivered pinned to the session it was authorized against, never to an overlay or another tab that came to the front, and a background write stamps the input-arrival instant as a raw write does.
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
    /// The connection-scope gate (orthogonal to `op`): `Scoped` for the common case, the other [`Access`] values for the exceptions.
    pub access: Access,
    /// The FIRST SENTENCE of the verb's help, at most `summary_max_chars!()` (96) chars: the whole of this verb's row in the SHORT catalog (a bare `help`) and the head of its full entry.
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
    /// What the request does to a session's input ([`Input`]); `None` only on a read row that declares nothing ([`VerbSpec::input_class`] reads it as [`Input::NONE`]).
    pub input: Option<Input>,
    /// Whether a FLEET peer may run the verb: a connection the fleet listener authenticated (`[fleet]`, `docs/DESIGN-sessions-as-subagents-2026-10-15.md` §5), served under the server's `Scope::Fleet`.
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

/// What a [`Form`] matches in a request's TAIL: the text after the verb, and after the sub-word too when the request names a listed sub-form row (`inbox get`).
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
    /// Only an owner-class connection may run the form, whatever its row's [`Access`]: no fine op expresses what it reaches.
    pub owner_only: bool,
    /// How the form's reply is framed instead of its row's.
    pub framing: Option<Framing>,
    /// The lane the form costs instead of its row's.
    pub lane: Option<Lane>,
    /// Whether the form's reply needs the artifact acknowledgement, instead of its row's.
    pub ack: Option<bool>,
}

/// [`HOLD_COST`] as a literal, so the `hold` entry and the window's Take Over tool tip quote it through `concat!`: one sentence, never two that drift.
#[macro_export]
macro_rules! hold_cost {
    () => {
        "While any tab is held, agents on this aterm cannot use invoke, tab, pane, hwkey, \
         pointer or confirm, or open the palette, a tab menu, the Autopilot menu or Overview, \
         and a sudo with no terminal cannot ask for its password in any tab."
    };
}
/// WHAT A HOLD COSTS THE WHOLE INSTANCE, in the one sentence every surface that states it uses (`docs/DESIGN-owner-surfaces-2026-10-16.md` §4): Take Over's tool tip, the menu bar's You hold section and the `hold` entry.
pub const HOLD_COST: &str = hold_cost!();

/// The cap on a [`VerbSpec::summary`] as a literal: one readable row after the [`CATALOG_TEXT_COLUMN`] gutter on a 125-column line.
macro_rules! summary_max_chars {
    () => {
        96
    };
}

/// THE ONE TABLE OF GUI TARGETS: each row a target's variant, its keyword, the aliases that resolve to it, and — by its group — which verbs take it.
macro_rules! aux_targets {
    (
        front: $(#[$fdoc:meta])* $front:ident = $fkw:literal $(| $falias:literal)*;
        opens: $(#[$o0doc:meta])* $o0:ident = $o0kw:literal $(| $o0alias:literal)*
            $(, $(#[$odoc:meta])* $o:ident = $okw:literal $(| $oalias:literal)*)*;
        gestures: $($(#[$gdoc:meta])* $g:ident = $gkw:literal $(| $galias:literal)*),*;
    ) => {
        /// A GUI surface the `window`, `controls` and `open` verbs name, beyond the bare front window: Settings routes and the front window's overlays.
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
        /// A fact's menu, the same card hung from a fact of Claude Code's
        /// composer rule (the usage's: its Traffic card, a graph over time);
        /// `open fact-menu <model|effort|path|branch|context|usage>` opens the front
        /// window's focused session's, `open fact-menu close`
        /// closes it, and `controls fact-menu` lists its rows by their stable
        /// keys (`fact-menu/<fact>/<act>`), its first line naming the fact.
        FactMenu = "fact-menu",
        /// The Overview (`docs/DESIGN-owner-surfaces-2026-10-16.md` §6, was
        /// the Connection Map); `open overview` raises it on the front window,
        /// `controls overview` reads the open card's groups, chips and manages
        /// lines (where this aterm's fabric is on, its "On the fabric" group
        /// too: `overview fabric chip node= sid= need= age= mark= older=`
        /// lines, read off the fabric's log and never acted on), and every
        /// wire read of it is Owner-gated.
        Overview = "overview" | "connections",
        /// The native Settings `/updates` route, projected from its compiled
        /// semantic frame while that route is open.
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

/// The budget for the short catalog's rows put together (every [`catalog_lines`] row, `\n`-terminated): what discovering a verb costs.
pub const SHORT_CATALOG_MAX_BYTES: usize = 13874;

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

    /// The verb's full entry as `help <verb>` prints it: `<name padded> <text…>`, greedily word-wrapped at [`ENTRY_WRAP_COLUMNS`] with continuation lines indented to [`CATALOG_TEXT_COLUMN`].
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

/// A driven write's class: the halt, the unread-input gate, the self-feed floor,
/// the drive lease, the front-surface fence and the licence fence all ask it.
const DRIVEN: Input = Input::PTY
    .with(Input::WRITES)
    .with(Input::FLOORED)
    .with(Input::LEASED)
    .with(Input::FRONT_DRIVING)
    .with(Input::FENCES);

/// A [`Form`] matching `when` that differs from its row in nothing yet; the chained
/// setters below state how it differs.
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

/// A [`VerbSpec`] constructor with an explicit non-`Scoped` [`Access`] gate, so the dispatch
/// reads the exception from the table instead of hardcoding a verb list.
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

/// THE TABLE.
pub const VERBS: &[VerbSpec] = &[
    va(
        "version",
        Owner,
        Status,
        Meta,
        AnyScopeMeta,
        "build provenance (version, commit, trustc, flavor, signature)",
        "The compiler keys: `flavor=t|r` is Trust or upstream Rust; `trust=` is Trust's own \
         version (`none` on an upstream build); `rust_compat=` and `trustc=` carry the compiler's \
         rustc-shaped release, with `trustc_commit=` and `trustc_host=` (there is no `rustc=`). A \
         fleet peer (`[fleet]`) may run it: a fleet read runs only verbs marked `fleet` (`help \
         --json`); any other verb, `@` selector or frame is `ERR denied: …` and closes the \
         connection. At `[fleet] allow = \"drive\"`, a node named by `aterm fleet allow drive \
         --from` and proved by its key also runs the worker verbs as Owner: `@<sid> \
         status|text|screen|result|turn|key|send` on a hosted session, `hold <sid> on|off`, \
         `spawn agent=<claude|gemini|agy|muse>` with its task frame, `await task`, and `@<sid> \
         close` on a worker it spawned (`aterm help fleet`, DRIVE).",
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
        "Every line: `enabled= current_build= commit= staged_build= staged_version= \
         staged_commit= staged_is_same_commit= relaunch_ready= apply_posture= failing=<n>:<kind> \
         failing_applies= persistent= outcome=\"…\"`. When known or true: `stale_check=<rfc3339>` \
         or `check_unrecorded=true` (no check in 4 h / none recorded), `checked_at=<rfc3339> \
         next_check=<rfc3339>`, `checker_stalled=<s>:<phase> checker_respawns=<n> \
         checker_deferred=<n>`, `apply_phase=` and `apply_held=` (automatic postures only), \
         `apply_policy_reason=`, `delivery=deferred|blocked`, `installable=false` (a disk-image, \
         translocated or dev copy never updates), `apply_refusal= apply_refusal_at= \
         apply_failure=`, `changelog=`, and on macOS `trial_wait=` (boot trial waiting for a \
         window to confirm it, or the recovery proof that refused it), `freeze_seed_ms= \
         handoff_capture_ms=` (first freeze rung and timed capture from the last dry run) and \
         `handoff_park_ms=` (real park cost, readers stopped to capture done) — prose values \
         pct-encoded. `apply_posture`: `automatic`, `automatic-idle`, `waits-terminal` (installs \
         once a terminal tab opens), `manual-config`, `disabled-env`, `retry-wait`, \
         `manual-only`, `handoff-unavailable`, `applying`, `unreconciled`, `none`, `disabled`, \
         `unknown`. `apply_phase`: `prefer-idle`, `prefer-output-gap`, `keys-only`, `land` — runs \
         within a minute of arming unless `apply_held=restored-agents` (reopened tabs' agents \
         still relaunching, up to 7 min once per process). Linux adds `linux_installed_build= \
         linux_staged_build= linux_trial=<phase|none> linux_trial_starts= linux_trial_healthy=`. \
         `check` runs one synchronous channel check and returns the same line. `apply` \
         (Owner-only) queues the staged build on macOS (live-session handoff keeps shells) or \
         replaces the on-disk executable in place on Linux.",
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
            "Bare = one summary row per verb (first sentence, <= ",
            summary_max_chars!(),
            " chars); help <verb> = that verb's full entry, wrapped; help --full = the \
             complete catalog with the protocol header; help --json = the verb table as one \
             JSON line (name, op, framing, target, access, summary per verb)"
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
        concat!(
            "Bare = one summary row per verb (first sentence, <= ",
            summary_max_chars!(),
            " chars); help <verb> = that verb's full entry, wrapped; help --full = the \
             complete catalog with the protocol header; help --json = the verb table as one \
             JSON line (name, op, framing, target, access, summary per verb)"
        ),
    )
    .input(Input::NONE),
    va(
        "privacy",
        Read,
        Lines,
        Meta,
        AnyScopeMeta,
        "privacy [--json]: the macOS consent posture behind EPERM",
        "Self-scoped and instance-wide (no `@<sel>`, answered by `--headless`); reading raises no \
         dialog. `OK <n>` then rows: `schema=1`; `platform= os=`; code-signing identity \
         (`bundle_id= signing= team= dr= grant_stable= dev_build=`); `install= running= anchor=`; \
         `full_disk_access= probe= probe_age_ms= fda_scope=`; `covers=`, `uncovered=`, \
         `unmeasured=`; `folder <name>=<state>… source=`; `claimants=<n|-> conflicting=<n|-> \
         census=<complete|partial|unavailable|pending|off> [sole=<yes|no>] [age_ms=<n>]` and up \
         to 8 `claimant path= signing= dr= team= conflicts=yes \
         trash=<offered|signature|requirement|linked|running-unstable|no-tool>` rows (then \
         `claimant truncated=<m> of=<total>`); `prompt_possible=`; `attribution_root= \
         sessions_total= sessions_adopted=` and one `session sid= attribution= responsible_pid= \
         responsible= fs_consent= cwd=` row per readable session; `containment mode= seatbelt= \
         protected=`; `warmup= warmup_last_ms=`; `debugger \
         mode=<enabled|disabled|unrecognized|unknown> member=<admin|developer|no|unknown> \
         self_hardened=<yes|no|unknown> right= probe= probe_age_ms= pending=off pending_right=- \
         pending_age_ms=- last=- watch_ms=-`; `devtools state=<allowed|denied|unlisted|unknown> \
         subject= requests= not_allowed= answer_age_ms= window_s=600 probe= probe_age_ms= \
         grant=<granted|not_granted|unknown> list=<on|off|absent|unknown> list_probe=`; `observer \
         fda= responsible= log= taskport=`; `access_card \
         state=<quiet|asked|opened|granted|declined> asked_at= opened_at= verified_at= renew_at= \
         needs=`; `remediate fda= files= debugger= devtools=`; then `note <code|info> <text>` \
         rows. Free text is pct-encoded; `-` is unset; an in-flight probe reads \
         `full_disk_access=unknown probe=pending`. Folders stay `unknown` until aterm observes an \
         access (`allowed`, `denied`, `asking`, `error`) or a measured FDA grant covers them \
         (`covered-by-fda`, `source=none|warmup|fda|warmup,fda`). The FDA grant covers `app-data` \
         for this process (`fda_scope=this_process`). `anchor=` is `live`, `displaced`, \
         `deleted`, `not-bundled` or `unknown` (`displaced`/`deleted` matches no grant). \
         Conflicting bundle copies reset TCC grants when they prompt; `trash=offered` marks \
         copies Settings ▸ Security can move to Trash. `devtools`: `state=` is tccd's newest log \
         answer over `window_s=`, `list=` is the system `TCC.db` switch, and `grant=` joins them \
         (newer wins). Only a human changes these in System Settings (`remediate` names \
         `settings:Privacy_AllFiles`, `aterm-settings:security`, `settings:Privacy_DevTools`); \
         `devtools hand-over` opens the page and `await consent` (`await consent kind=debugger`) \
         waits for a change.",
    )
    .json(Json::Emits),
    v(
        "text",
        Read,
        Lines,
        Session,
        "text [--json] [trim] [tail=<n>|rows=<a>-<b>] [order=<logical|display>]: visible screen",
        "`OK <n>` then the rows. `trim` drops trailing all-blank rows (header `OK <n> \
         trimmed=<k>`; interior blanks stay). `tail=<n>` sends the last `n` rows and \
         `rows=<a>-<b>` the inclusive 0-based span clamped to the grid (an empty span or `b < a` \
         is `ERR bad rows`); when the slice starts past row 0 the header adds `first=<row>` so \
         reply line `i` is screen row `first+i`. `order=display` lays out each row as painted \
         (RTL runs reversed) instead of logical reading order (`order=logical`, the default). \
         `--json` carries `\"gen\":\"<epoch>.<seq>\"` (`status gen=`, which `key if-gen=` fences \
         on), `\"human_ms\":<ms|null>` (`status human_ms=`; beside a null `\"human_ms\"`, \
         `\"unkeyed_ms\":<ms>`), `\"human_seq\":<n>` (`status human_seq=`, which `key|send \
         if-human=` fences on), `\"launch\"` (`status launch=`: \
         `{\"id\":<n>,\"seq\":<n>,\"word\":\"claude|codex\",\"why\":\"person\"}` or \
         `{\"why\":\"<word>\"}` or `null`), `\"inputs\":[<n|null>,<n>]` (input writes before and \
         after reading the rows; `null` with an active write, partial frame or spill), \
         `\"submits\":<n>` (Enters written over typed input since the previous Enter), \
         `\"trimmed\"` and `\"first\"` when set, `\"order\":\"display\"` and `\"display_map\"` \
         under `order=display`, and `\"frame\":\"open\"` when the program's DEC 2026 frame was \
         still unfinished at the read's bound (`await frame`). Every form waits, holding nothing, \
         for an unfinished frame to close, up to the sync timeout (1 s) from the frame's run; one \
         still open then is read as it stands, the plain header saying `frame=open` after the \
         count (`aterm ctl` notes it on stderr). `--json`'s person stamps and first `inputs` \
         count are taken after that wait, just before the rows. `cursor`, `cell`, `screen`, \
         `line` of a screen row, `search`, `offscreen screen=1`, `blocktext`, `select`, \
         `selection`, `copy`, `turn` and `cast drift` read so too; `blocks` and `wait` never \
         wait for a frame. Anything else is `ERR usage: text [--json] [trim] \
         [tail=<n>|rows=<a>-<b>] [order=<logical|display>]`.",
    )
    .json(Json::Emits),
    v(
        "screen",
        Read,
        Lines,
        Session,
        "screen [--json] [since=<etag>]: full styled grid as one lossless JSON frame",
        "Ends with `\"etag\":\"<16 hex>\"`, a fingerprint of screen generation, output fed, \
         dimensions, cursor, colours, palette, selection, style policy and viewport. While \
         unchanged, `since=<etag>` returns `OK 1` then \
         `{\"unchanged\":true,\"seq\":<n>,\"gen\":\"<epoch>.<seq>\",\"etag\":\"<etag>\"}` without \
         reading the grid. Read on a whole frame as `text` is: a reply on one still unfinished, \
         the `unchanged` one too, ends with `\"frame\":\"open\"` after the etag. Anything else is \
         `ERR usage: screen [--json] [since=<etag>]`.",
    )
    .json(Json::Emits),
    v(
        "line",
        Read,
        Status,
        Session,
        "line <n>: one physical line of scrollback+screen",
        "`n` is an absolute row (`search` coordinates). `ERR evicted`: older than retained \
         history; `ERR unreadable`: retained row could not be read; `ERR out of range`: past the \
         live screen's end. A screen row is read on a whole frame as `text` is, with no word for \
         one still open; a history row at once.",
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
        "Keeps rows that scrolled off the top of the alternate screen (4 MiB shared across \
         sessions, >= 256 KiB each) by diffing committed frames (DEC 2026 close or 16 ms). Reply \
         `OK <n> first=<i> last=<j> lost=<k> breaks=<b> back=<d> [back_at=<i> pin=<p>] epoch=<e> \
         origin=<o> alt=<0|1> seq=<s> [enabled=0] [more=1] [frame=open] [screen_rows=<m>]` then \
         `n` rows, oldest first (line `m` is archived row `first+m`). `since=` is exclusive \
         (`since=<last>` pages; `since=<origin>:<i>` from `history` `arch=` reads from 0 if the \
         origin changed; past `last` is `ERR bad since`). Capped at 2000 rows or `max=<n>`; \
         `tail=<n>` takes the newest `n`; `more=1` marks truncation. `lost=` counts rows \
         evicted/wiped after `since`; `breaks=` counts discontinuities (no-overlap redraw, \
         resize, alt-screen exit, reset); `back=` counts archived rows visible on screen again at \
         rows `pin..pin+back` from `back_at`. `screen=1` appends the current screen under the \
         same lock as the last `screen_rows=<m>` lines, read on a whole frame as `text` is \
         (`frame=open` when the DEC 2026 frame was still unfinished at the read's bound; see \
         `await frame`). Seamless updates carry up to 1 MiB after the last 8 \
         turns' marks. Anything else is `ERR usage: offscreen [since=<i>] [tail=<n>] [max=<n>] \
         [screen=1]`.",
    )
    .json(Json::Refused),
    v(
        "mainscreen",
        Write,
        Status,
        Session,
        "leave a dead app's alt screen: OK left=<0|1>",
        "Recovers when a full-screen app dies (`kill -9`, `Stop-Process`) without sending \
         `?1049l` and the shell sends no OSC 133 marks (`cmd.exe`). `OK left=1` archives the dead \
         screen into `offscreen` and restores the primary screen and scrollback; `OK left=0` \
         means the primary screen was already active. Under Windows ConPTY, the console host \
         stays on the alt screen until a program writes `ESC[?1049l`. Any argument is `ERR usage: \
         mainscreen`.",
    )
    .input(Input::NONE)
    .lane(Wait),
    v(
        "cell",
        Read,
        Status,
        Session,
        "cell <r> <c>|abs=<n> <c>: OK <grapheme%enc> <fg> <bg> <attrs>[ link=][ frame=open]",
        "`abs=<n>` reads absolute row `n` (`line`/`search` coordinates, history or live screen, \
         without `link=`); `ERR evicted` if older than retained history, `ERR out of range` past \
         the screen. A screen row is read on a whole frame as `text` is, ` frame=open` coming \
         last when the frame was still unfinished at the read's bound; a history row at once, \
         with no word.",
    )
    .json(Json::Refused),
    v(
        "cursor",
        Read,
        Status,
        Session,
        "OK <row> <col> <visible> <style>[ frame=open]",
        "Read on a whole frame as `text` is; ` frame=open` comes last (`--json`: \
         \"frame\":\"open\") when the program's DEC 2026 frame was still unfinished after \
         the read's bound",
    )
    .json(Json::Emits),
    v(
        "dims",
        Read,
        Status,
        Session,
        "OK <rows> <cols> <px_w> <px_h>",
        "then keyed facts: `session= cell_w= cell_h= font_px= scale= window=<id|none> \
         window_rows= window_cols=`, frame/surface sizes, bands, crops, `geometry=` and \
         `present_retry_*` (`--json` carries the same).",
    )
    .json(Json::Emits),
    v(
        "modes",
        Read,
        Lines,
        Session,
        "DEC modes: alt_screen=, cursor_visible=, ..., focus_reporting=, sync_output= (14 keys)",
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
        "`OK id=\"<id>\" label=\"<label>\" configured=\"<raw>\"|- side=dark|light \
         source=builtin|file|default [path=\"<file>\"] reported=dark|light miss=\"<why>\"|- \
         os_appearance=dark|light preview=\"<id>\"|- [restore=\"<id>\" ttl_ms=<n>|-] fg=#rrggbb \
         bg= cursor=|- selection=|- selection_fg=|- … generate=on|off \
         generate_from=theme|config|default pinned=<n> palette=<16 #rrggbb, comma separated>`; \
         `miss=` names any configured theme that could not be drawn (Default is drawn instead); \
         `generate=` says whether slots 16–255 are derived from theme colours \
         (`palette_generate`); `pinned=` counts sessions with their own theme; `reported=` is the \
         theme baseline for DSR 996 / mode 2031 (per-session overrides show in `status` \
         `reported_scheme=` `scheme_from=`). `theme palette [session]` answers `OK 256` and one \
         `slot index=<n> rgb=#rrggbb origin=theme|config|default|generated|xterm` row per slot. \
         `theme list` answers `OK <n>`, one `theme id= label= appearance= measured= pair= source= \
         status= drawn= notes= description=` row per built-in and themes-folder theme, then one \
         `folder path= kept= seen= truncated=` row.",
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
        "Applies `<name>` (built-in, themes-folder id or `dark:…,light:…`) across windows and \
         sessions without writing config; explicit colour keys still apply. Ends after `ttl=` \
         seconds (default 120, max 3600), on `theme revert`, or on config/themes reload. Unknown \
         names are refused. Answers the `theme` status line.",
    )
    .input(Input::NONE),
    v(
        "theme session",
        Read,
        Status,
        App,
        "theme session [status] | set <name> | clear: this session's own theme",
        "`OK session=<sid> theme=\"<selector>\"|- origin=ctl|spawn|split|restored|instance \
         asked=<origin>|- id=\"<drawn id>\" source=builtin|file|default side=dark|light \
         reported=dark|light miss=\"<why>\"|- generate=on|off fg=#rrggbb bg=#rrggbb`. Recolours \
         this session's cells, cursor, selection, palette and DSR 996 / mode 2031 reports; window \
         chrome follows the instance theme. Inherited by splits, carried across seamless updates \
         and restores, and unaffected by instance previews. `set` refuses an undrawable name \
         (`ERR theme session \"<name>\": <why>`); if it stops drawing later, `miss=` is set and \
         the instance theme is drawn. `spawn theme=<name>` sets it at creation.",
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
        "Writes through `settings set theme`. `OK saved: theme = <value>` appends any unavailable \
         theme or selector warning after `; `. `theme commit` with no active preview is `ERR no \
         theme preview to commit`.",
    )
    .input(Input::NONE),
    v(
        "search",
        Read,
        Lines,
        Session,
        "search <pat> [case] [regex]: find in history; OK <n>[ incomplete…], \"<row> <col> \
         <len>\" each",
        "Searches the newest `search_history_lines` lines (default 100000); header carries \
         `screen_first=<row>` (absolute top row of the live screen). `incomplete` lists \
         applicable causes: `capped` (100000 matches), `rewrapping` (resize held history; screen \
         only), `long-line` (regex budget or >1 MiB row), `moving` (screen switched, resized or \
         renumbered rows during the scan), `older` (older history not searched). Rows are \
         absolute (`line` coordinates). Soft-wrapped hits can run past `col+len > cols` into the \
         next row; regex `^` and `$` bind to the logical line. The search starts on a whole frame \
         as `text` reads; ` frame=open` before `screen_first=` says a read of it found the frame \
         unfinished, which is no `incomplete` cause.",
    ),
    v(
        "find",
        Read,
        Status,
        App,
        "find [open|type|key|next|prev|case|regex|accept|cancel|status]: drive the FIND BAR",
        "Drives the window's find bar (⌘F / Ctrl+Shift+F), which `key` and `send` do not reach. \
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
        "Read on a whole frame as `text` is (as are `select`, `copy` and `blocktext`); ` \
         frame=open` ends the header when the program's DEC 2026 frame was still unfinished at \
         the read's bound.",
    )
    .json(Json::Refused),
    v(
        "copy",
        ClipboardWrite,
        Status,
        Session,
        "copy the selection to the system clipboard, OK <bytes>[ incomplete][ frame=open]",
        "Read as `selection` is.",
    )
    .json(Json::Refused)
    .input(Input::NONE),
    v(
        "blocks",
        Read,
        Lines,
        Session,
        "shell-integration command blocks (exit codes, command text, state)",
        "",
    )
    .json(Json::Emits),
    v(
        "blocktext",
        Read,
        Lines,
        Session,
        "blocktext <id> [trim]: one command block's output",
        "`trim` drops trailing all-blank rows (`OK <n> trimmed=<k>`); any other argument is `ERR \
         usage: blocktext <id> [trim]`. ` frame=open` after the count says the program's DEC 2026 \
         frame was still unfinished at the read's bound, as `text` says it.",
    )
    .json(Json::Refused),
    v(
        "image",
        Read,
        Status,
        Session,
        "image [path]: capture the application-rendered client frame -> PNG (OK <w> <h> <path>)",
        "A bare filename is confined to the runtime `images/` dir; omitted, it is auto-named. \
         Captures the last presented client frame (or the headless semantic renderer's); what the \
         OS compositor showed is not observed. `@<sid>` captures the whole window showing that \
         session, so edge tokens are refused (`ERR denied`; use `image read`, `text` or \
         `screen`). `image --bytes` returns the PNG inline (`OK 1` then `<w> <h> <nbytes> \
         <base64>`). `image --meta` adds the frame's pixel fingerprint, phase, theme and per-leaf \
         identity: `OK <w> <h> image-meta image-meta-version=3 <key=value…> path=\"<path>\"` \
         (with `--bytes`: `OK 2`, the `image-meta` row, then the PNG row; \
         `frame-phase=destination|presented|staged`; `identity=unavailable` when provenance was \
         not retained). `image plain` = bare pixels; `image read` = the session's inline OSC 1337 \
         images as base64.",
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
            "; aliases `settings`, `palette`, `connections-overview` and `software-update` also \
             resolve. A bare filename is confined to `images/` and needs an extension in \
             `<target>`'s slot (`ERR unknown window target '<word>' …` otherwise). Every target \
             except `front` must already be open in the front window (`ERR <target> is not open \
             in the front window; …`, naming `open <target>` or `invoke \
             ConnectToSession|ConfigureConnection`)."
        ),
    )
    .ack()
    .forms(&[form(AnyOf(&["overview", "connections"])).owner_only()]),
    v(
        "video",
        Read,
        Status,
        App,
        "video [<secs>] [full|half] [keys] [pace] [trail] [fps=] [budget=]: record frames",
        "Records submitted client frames on one monotonic clock into `frame_NNNN.png` + \
         `index.json` (8 recordings retained). `secs` defaults to 3 (`0.5..=60`), `fps=` to \
         `1..=120`, `budget=` (frame-store MiB) to 512 (`64..=4096`), resolution to `half` \
         (`full` optional); `pace` forces continuous redraws. `keys` (Owner-only) logs hardware \
         input and `key`/`ctrl`/`send`/`feed`/`paste` on the visible front tab \
         (`unlogged_inputs=` on the reply and `unlogged=` on `video status` count input to other \
         tabs/windows). `trail` logs cursor-trail admission verdicts into `index.json` `trail[]` \
         (`trail= trail_lost=` on the reply). `video status` reports an active recording \
         (`recording= mode= elapsed_ms= frames= resized= keys=`, plus `inputs= unlogged=` with \
         `keys`); `video stop` (Owner-only) ends it early. `video frames [count=N]` lists the \
         newest recording's `N` most-changed frames (`frame n= delta= t_us= seq= <path>`, default \
         8, max 64). `index.json` carries coverage (`head_truncated`, `evicted_frames`, \
         `ring_skipped`, `covered_us`, `requested_ms`, `keys_requested`, `inputs_logged`, \
         `unlogged_inputs`), `ticks[]` (unchanged redraw instants under `pace`/headless), and \
         `analysis` (at `fps=N`, key-to-frame floor is `~1000/N` ms; use `metrics percentiles` \
         `input_p50/p95/p99_ms` for typing latency).",
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
        "Lists app-initiated jobs (`toolchain`, `update`) and `harness` notes: `activity \
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
        "Owner-only. Records `<text>` from a helper process (`aterm pkg install`, harness) into \
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
        "Rows ascend by id: `message <id> at=<unix_ms> ago_ms=<ms> tag=<tag> \
         sev=<success|info|warn|error> origin=<host|wire|carried> \
         state=<held|live|review|folded|stale|unseen|superseded|resolved-ok|resolved-warn|dismissed|answered|evicted|carried|withdrawn|recorded|quit> \
         glass=<row|-> rep=<n> key=<k|-> title=<t> detail=<d> actions=<a|-> \
         [progress=<n>/100|busy=1] [load=<network|disk|cpu>] [reveal=pending] [since_ms=<ms>] \
         [need=<now|working|notice|later>]` (free text pct-encoded). Bare `<n>` keeps the newest \
         `n` (default 64, max 512); `since=<id>` pages forward from `<id>`; `tag=` and `sev=` \
         (floor) filter; `need=later` selects exceptions to review; `live` selects unretired band \
         rows (`need=later live` for open exceptions; `reveal=pending` marks a row still inside \
         its grace period).",
    ),
    va(
        "notice",
        Write,
        Status,
        App,
        OwnerOnly,
        "notice <post|progress|done|ask|later|dismiss|act> …: record, show work, ask, file, end \
         or press",
        "Owner-only. `notice post <tag> [sev=<sev>] [key=<key>] [hold=<1..120>] <title>[ -- \
         <detail>]`: `sev=success|info` (default `info`) logs only (`OK recorded message=<id>`); \
         `sev=warn|error` raises a band row (`OK message=<id>`). `notice progress <key> \
         [tag=<tag>] [pct=<0..100>|done=<n>/<total>|busy] [unit=<bytes|items|steps>] \
         [load=<network|disk|cpu>] <title>[ -- <stats>]` shows a metered row after 2 s (stale \
         after 120 s without refresh). `notice done <key> [ok|warn|withdraw] [<words>]` ends it \
         (`OK done=<id|-> how=<resolved-ok|resolved-warn|withdrawn|gone>`). `notice ask <key> \
         [tag=<tag>] <title> -- <reason> -- <step>` raises a standing owner ask (reason and step \
         <= 90 chars; suppressed 24 h after `done <key> ok` or dismissal; `OK message=<id>`). \
         `notice later <key> [tag=<tag>] [url=<https-url>] <title> -- <reason> -- <step>[ -- \
         <detail>]` files a non-urgent exception in the menu-bar item and Settings ▸ Messages \
         (`OK later=<id> how=<new|restated|duplicate|quiet>`, up to 4 wire entries, auto-filed \
         after 14 d; `url=` adds an Open in browser button). `notice dismiss <id|review>` \
         dismisses a row or files open exceptions; `notice act <id> <label|index|details>` \
         presses a button (`OK acted=<label> performed=<1|0>`). Titles <= 6 words / 48 chars; \
         keys `1..=40` of `[a-z0-9._-]` (stored as `wire.<key>`); built-in tags are reserved (use \
         `script` or a custom tag). Rate-limited to 3 live wire rows, 60 messages / 16 KiB per \
         min, 10 acts per min (`ERR busy notice: … retry_ms=<ms>`, or `retry_ms=1000` during \
         version handoff).",
    )
    .input(Input::NONE),
    va(
        "story",
        Write,
        Status,
        Session,
        OwnerOnly,
        "story chose [<text>]: record what the harness chose",
        "Owner-only; takes `@<sid>` (bare: own session). `<text>` is optional (<= 96 bytes, no \
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
        "Owner-only. Opens macOS Privacy & Security ▸ Developer Tools (`privacy`'s `devtools` \
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
        "Answers `os_appearance=<dark|light> source=<nsapp|desktop|window|inert|injected> \
         theme_side=<dark|light|-> reported_scheme=<dark|light> bg_dark=<0|1> theme=<id> \
         installs=<n>` (each session's `status` carries the same fields plus `scheme_from=`). \
         `appearance dark|light` injects an OS flip (requires a `dev-seams` build).",
    )
    .input(Input::NONE),
    v(
        "text-scale",
        Write,
        Status,
        App,
        "text-scale [0.85..2.0|os]: the text scale native pages lay out at",
        "Answers `text_scale=<f> source=<os|injected>`: the text scale Settings and native pages \
         lay out at. `text-scale <0.85..2.0>` injects a scale and `text-scale os` clears it \
         (requires `dev-seams`; `$ATERM_TEXT_SCALE` injects from launch).",
    )
    .input(Input::NONE),
    v(
        "pressure",
        Write,
        Status,
        App,
        "pressure [normal|warn|critical]: memory pressure and the crash-restore journal's shed",
        "Answers `level=<-|normal|warn|critical> journals=<n> rings_shed=<n> \
         journal_resident_bytes=<n> rings_back_in_ms=<-|n>`: the last OS memory-pressure level \
         (`-` before any), sessions journaling a crash-restore region, history rings shed under \
         `warn`/`critical` (screen restores intact; scrollback above the keyframe is lost), \
         resident bytes (`metrics` `journal_resident_bytes=`), and ms until the next shed ring \
         returns (each session's `status` reports `journal_ring=<live|shedding|shed>` and \
         `journal_bytes=`). `pressure normal|warn|critical` injects a level on macOS (requires \
         `dev-seams`).",
    )
    .input(Input::NONE),
    v(
        "program-check",
        Write,
        Status,
        App,
        "program-check [full|live]: the stuck check's reading of macOS's program check",
        "Answers `OK reading=<live|full> syspolicyd=<-|ok|full|stuck> \
         syspolicyd_workers=<n>/<limit|->|- syspolicyd_full_ms=<ms|->`: the board carried on \
         `status` (`aterm help permissions`, SLOW FIRST RUNS), sampled once a minute on macOS. \
         `program-check full` stages `syspolicyd` at the kernel thread limit (5 min reads \
         `stuck`, raising the restart alert and pausing harness continuations); `program-check \
         live` restores live sampling (requires `dev-seams`).",
    )
    .input(Input::NONE),
    v(
        "card",
        Write,
        Status,
        App,
        "card [seed=<u64>|seed=off]: pin the card's stars (debug builds)",
        "Bare `card` answers `OK seed=<u64|off>`. `card seed=<u64>` pins every agent card's \
         starfield to that seed and re-arms visible cards (`OK seed=<u64|off> rearmed=<cards>`); \
         `card seed=off` restores per-session seeding. Pinning requires a debug build.",
    )
    .input(Input::NONE),
    v(
        "aos",
        Write,
        Status,
        App,
        "aos [status|attach|pause|repair|resume|detach|trace]: host the momentum glow (debug \
         builds)",
        "Debug-only; hosts the front window's typing-momentum glow as an Aos task. `aos [status]` \
         answers `OK window=<id> attached=<bool> code=<name> ...` with revision, pause queue, \
         trace log cursor, hot/active/settling flags, `cold_at=`, last wake ns and boundary \
         counters. `aos attach [code=<shipped|demo-stranded-settle>]` checkpoints the glow and \
         records keys, frames and scheduler queries. `aos pause` holds at the step boundary; `aos \
         repair [code=<name>]` shadow-replays live and candidate code over the trace and swaps in \
         the candidate only if live matches the paused head and the candidate satisfies \
         `momentum-glow.no-stranded-light` (else resumes old code); `aos resume` drops a pending \
         update; `aos detach` stops hosting; `aos trace` reports log length, trace and checkpoint \
         digests, and broken flag.",
    )
    .forms(&[form(AnyOf(&["status", "trace"])).op(Read)])
    .input(Input::NONE),

    v(
        "login-notice",
        Write,
        Status,
        App,
        "login-notice [<code> [<label>]|disk [spent|inodes|off]|end|back]: harness login notices",
        "Requires `dev-seams`. Bare, lists `OK notices=<code>:<id>,…` for Claude Code \
         login/renewal and account-switch notices (`aterm help harness`). `login-notice <code> \
         [<label>]` (default label `main`) raises that notice on the first tab \
         (`account-signed-out` and `renewal-wrong-account` update the accounts line). \
         `login-notice disk [spent|inodes|off]` raises the disk-full alert (`spent` after cache \
         cleanup, `inodes` on inode exhaustion, `off` when `[disk] auto_free_gib = 0`). \
         `login-notice end` withdraws active notices; `login-notice back` marks login restored \
         and closes the Settings ▸ Messages row. Replies `OK sid=<tab> \
         notice=<id|disk-full|end|back>`.",
    )
    .input(Input::NONE),
    v(
        "security-posture",
        Write,
        Status,
        App,
        "security-posture [list|off|<name>]: Settings ▸ Security's posture source",
        "Bare, answers `OK posture=<host|off|name> views=<n>` (where Settings ▸ Security gets its \
         macOS access posture and how many views draw it). `security-posture list` answers `OK \
         postures=<name>,…`. `security-posture <name|off>` stages a fixture or restores `off` \
         (requires `dev-seams` on a headless instance; staged buttons do not call the OS).",
    )
    .input(Input::NONE),
    va(
        "askpass",
        Owner,
        Status,
        Meta,
        OwnerOnly,
        "askpass: prompt on the session's tab for a no-tty sudo password",
        "Used by `aterm-askpass` (`SUDO_ASKPASS`) when `sudo` has no tty on macOS. Verified \
         against the kernel as a child of `/usr/bin/sudo`, raises a native password sheet on that \
         tab, and returns the password once without storing or logging it. Owner-exact (child \
         edges, the bridge, headless instances and held sessions are refused; see `aterm help \
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
        "Read-only. Ends with `presence rim=<none|stuck|decide|yours> pressing=<@sid|-> \
         pill=\"<sentence>\"` (the window's needs border and tab-bar pill; escapes `\"` and `\\`). \
         `pressing` names the border's session; the pill can name another session. Worker \
         reports can light the border without a pill. `stuck`/`decide` come from the most \
         pressing session's `status needs=`, `yours` from a front person hold; `[presence] rim` \
         off hides both. Off macOS there is no \
         menu bar (`menu \"<title>\"` lines report the menu model; `controls menu` shows the live \
         palette); macOS `--headless` prints `menu (none)` and `toolbar (none)` followed by \
         `toolbar-tabs` (including `\"done\"` tab states). On Windows a `strip-version \
         shown=<yes|no> …` line reports the tab-strip version label (`cols=` for `pointer move -1 \
         <col>`, `action=`, or `reason=no-strip|not-laid-out|no-ui-face|strip-full`). Then one \
         `autopilot sid=<sid> window=<id> program=<claude|agy|muse|gemini|codex> \
         face=<cruise|cruise-until|land|landed|theirs|off|master-off|agent-off|unhosted|-> \
         drawn=\"<cells>\"|drawn=- why=<not-drawn|no-room|no-host|no-footer> \
         spoken=\"<label>\"|spoken=- description=\"<words>\"|description=-` line per supervised \
         pane across windows. Before `presence` comes `statusitem title=<❯|❯•|❯N> needs=<n> \
         stuck=<n> exceptions=<n> done=<n> fleet=<off|unread|reading|failed|as_of:<unix ms>>`, \
         and on macOS the `statusmenu \"<label>\" enabled=<yes|no> [checked=<yes|no>] \
         [tip=\"<text>\"] [indent=<n>] [submenu]` (`statusmenu ---` for separators; debug builds \
         append `at=<i>[.<j>…]` for `person menubar press`) lines of the `❯` menu.",
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
        "the front window's ACTIVE-tab split-pane layout: `layout tab=<i> panes=<n> \
         zoomed=<bool>`",
        "header, then one `pane session=<sid> rect=<row_off>,<col_off>,<rows>x<cols> \
         focused=<bool>` row per visible pane (cell coords; 1-cell divider gaps). `@<sid>` \
         describes the window whose active tab shows that session (`OK 0` with `panes=0` when \
         none does).",
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
        "asciicast v2 recording (compact, sendable); `cast drift` audits it against the screen;",
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
        "Requires `temporal_recording=true`. `temporal status` reports `reachable_from=` through \
         `latest_tick=` (`-` when unreplayable), `lost_resizes=`, `dropped_input=`, \
         `dropped_replies=`, and `pending_resizes=` (also appended to a replay header at the \
         spine's end). `trim` drops trailing blank rows (`OK <nbytes> trimmed=<k>`); `status \
         trim` is `ERR usage`.",
    )
    .forms(&[form(Word("status")).framing(Status)]),
    v(
        "history",
        Read,
        Lines,
        Session,
        "history [<n>] [since=<id>]: the turn LEDGER",
        "— `id/submitted/status/dur_ms/seq/hash/by/arch/text` per completed turn (newest 512; \
         harness turns evict first above half so non-harness callers keep >= 255). `by=` is \
         `harness|ctl|bridge|<sid>|-`; `status=` is `settled|timeout|hangup|pressed` (`pressed` \
         when Enter was sent without awaiting settle, its `seq`/`hash` read on a whole frame as \
         `text` reads, or `turn settle=submitted`). \
         `arch=<origin>:<last>` feeds `offscreen since=`. Seamless updates and crash-restore \
         journals (`status journal=off` when disabled; `started_ms` and `seq` from the old \
         instance, rewritten every 2 s with `status journal_ledger_lag=`) carry records with \
         `carried=1` before `text=` and add `unheld_below=<id>` to the header when older records \
         may be missing.",
    )
    .json(Json::Refused),
    v(
        "meta",
        Read,
        Status,
        Session,
        "meta -> OK title= user_title= description= icon= role= attention= cwd= state=",
        "`attention_owner= attention_owners= attention_kinds= supervisor= supervisor_proven= \
         questions= agent_cwd= trusted_cwd= worker= autopilot= land_at= autopilot_by= \
         autopilot_since=` (pct-encoded; `-` = unset; `trusted_cwd=` and `worker=` come from \
         `spawn agent=<word>`; no `window=` here — use `sessions` or `dims`). `meta set \
         <title|description|icon|role|attention|questions|autopilot> <text...>` / `meta unset \
         <field>` set or clear user metadata (write-gated; caps: title 120B, description 1024B, \
         icon 64B, role 64B, attention 256B; `user_title=` overrides OSC `title=`). KEYED \
         ATTENTION: `meta set attention owner=<k> [kind=ask|wait] <text...>` / `meta unset \
         attention owner=<k>` (`<k>` in `[A-Za-z0-9._:@/-]{1,64}`, text <= 200B, up to 7 keyed \
         owners plus bare `-`; `ERR attention owners full` past 7) lets multiple writers track \
         escalations independently. `kind=ask` (default, unless text starts with `limited:` while \
         the harness handles the wall) counts in the menu bar (`status needs=ask`); `kind=wait` \
         is a non-counting record; `attention_kinds=` lists `<ask|wait|->:<owner>`, `attention=` \
         shows the newest entry (`attention_owner=`, `%2D` for bare), and `sessions meta=1` / \
         `meta-change field=supervisor value=-` track changes (`fp=` in log). SUPERVISOR: `meta \
         set supervisor <holder> [ttl=<1..600000>]` / `meta unset supervisor [holder=<holder>]` \
         (Owner-only; `ERR busy supervisor=<holder>` if held by another) suppresses \
         menu-bar/desktop alerts while the supervisor handles prompts (`supervisor_proven=1` for \
         a direct foreground `codex-alab@<pid>` connection). HARNESS_ACT: `meta set harness_act \
         <act> [until_us=<n>]` and `meta prepare harness_act` (`OK until_us=<n> lease_ms=5000`; \
         `ERR expired harness_act` or `ERR busy harness_act`) record a proven self-claim's act \
         (`timeline` `harness-act` `act=<act> holder=<holder>`, `status last_act=`). QUESTIONS: \
         `meta set questions ask|recommended|long-term|short-term` / `meta unset questions` \
         (Owner-only) overrides `[harness] answer_questions` and `question_priority`. AUTOPILOT: \
         `meta set autopilot cruise [until=HH:MM|+DURATION]|land|off` / `meta unset autopilot` \
         (Owner-only; aliases `forever`/`infinity` and `wrap-up`/`wrapup`/`wrap`; `ERR until= \
         goes with cruise only: usage: …`; sets `task=landed` when Land completes) sets the \
         session's Autopilot mode (`OK land_at=<unix s> local=<date>T<HH:MM><offset>`, adding \
         `journaled=0` if disk sync exceeds 5 s).",
    )
    .forms(&[form(AnyOf(&["set", "unset"])).op(Write)]),
    v(
        "harness",
        Read,
        Status,
        Session,
        "harness policy -> OK gen= effective=<on|off> why= <every [harness] key>= questions=",
        "Read-only `[harness]` policy for a self-supervising agent (`meta` \
         `supervisor_proven=1`). `gen=` is the policy generation (`EVENT <local> harness-policy \
         gen=<n>`); `effective=` and `why=` give this session's verdict \
         (`unhosted|disabled|headless|agent-off|suspended|hold|autopilot-off|enabled`). Followed \
         by every `[harness]` key (pct-encoded, `-` when empty, `continue_text=` composed with \
         `rules_file`; `autopilot=` is the table default while the session's word is `status \
         autopilot=`), `questions=<ask|recommended|long-term|short-term>`, and the composed \
         Cruise/Land prompts: `done_check_text=`, `land_text=`, `land_nudge_text=`, \
         `land_answer_text=`, `needs_hint_text=`, `needs_ack_text=`. Edge tokens receive only `OK \
         effective=<on|off>`. Any other form is `ERR usage: harness policy`.",
    ),
    v(
        "resources", Read, Lines, Session,
        "resources [capabilities|register]: delegated job ownership and current usage",
        "Lists this session's registered jobs, PID/start identities, AI/job labels, `cpu_cores`, \
         physical bytes and `sample=complete` status. `resources capabilities` returns `OK \
         resources version=1 available= runner= ai= session=`. `resources register \
         start=<kernel-start> [ai=<word>] job=<percent-encoded-label>` (local, Owner-only) binds \
         the caller's kernel peer PID for `aterm resource-run`.",
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
        "OK state= phase= outcome= confidence= detail= program= since_ms= human_ms= …",
        "Single-line session snapshot (`--json` returns the same fields as JSON with `schema=1`). \
         Core fields: `sid=<local> id=<sid> state=<starting|running|exited|closed> \
         phase=<idle|running|blocked|done|error> outcome=<ok|fail|killed|-> exit_code=<n|-> \
         signal=<name|-> confidence=<exact|strong|heuristic|none> \
         observed=<osc133,conpty,procs,…|none> \
         integration=<active|unconfigured|inactive|lost|unsupported> integration_rev=<n> \
         detail=<pct|-> program=<name|-> \
         program_state=<idle|working|blocked|done|error|->:[permission|question|auth] \
         since_ms=<ms> revision=<n> seq=<n> gen=<epoch>.<seq>` (`seq=` and `hash=` are `-` on a \
         contended lock or an unfinished DEC 2026 frame, which `status` never waits for; `gen=` \
         stands). Input & human fences: \
         `human_ms=<ms|-> human_seq=<n> typed_ms=<ms|-> \
         typed_by=<human|harness|ctl|bridge|<sid>|-> input=<idle|writing|spill|frame> \
         input_bytes=<n> input_wait_ms=<ms> editing=<0|1> \
         launch=<id>:<seq>:<claude|codex>|why:<word>|- launch_seq=<n>` (fenced by `if-gen=`, \
         `if-human=`, `if-fp=`; `conflict=` on `ERR stale`). Agent & harness: \
         `agent=<ready|working|approval|question|wall|stalled|-> agent_detail=<pct|-> \
         agent_rev=<n> agent_since_ms=<ms> agent_gen=<e.s|-> agent_fp=<hex16|-> fp=<hex16> \
         autopilot=<off|cruise|land|-> land_at=<unix-s|-> land_since=<unix-s|-> land_nudges=<n> \
         land_held=<why|-> pause=<unix-s|-> task=<open|landed|theirs|-> \
         task_state=<working|done|exited|stalled|-> task_rev=<n> last_act=<act|-> \
         supervisor=<pct|-> supervisor_proven=<0|1> upgrade=<-|<state>/<to>/<why>/<age>> \
         rollout=<ok|missing|stalled|-> rollout_asks=<n> copy=<managed|foreign|-> \
         path=<frozen|live> path_evidence=<measured:<ms>|unconfirmed:<ms>|carried|->`; \
         `agent_detail=bash:read-only` is the read-only rule's verdict on a Bash box's command \
         alone, advisory: the supervisor's own decider still reads the box and the session. Needs & \
         level: `level=<quiet|note|story|driving|driven|attention|limited|hold> \
         needs=<none|approval|question|wall|ask|stuck|frozen|stopped> needs_n=<n> need=<word|-> \
         need_since_ms=<ms|-> needs_told=<0|1> attention_owner=<pct|-> story=<n> \
         end=<done|error|exited|-> end_seen=<0|1> end_cheered=<0|1> \
         hand=<none|harness|ctl|bridge|edge|lease> hand_pid=<pid|-> turn=<id|->`. Hold & fabric: \
         `hold=<0|1> hold_by=<pct|-> hold_origin=<person|remote|fleet|local|-> \
         hold_advisory=<fabric-lost|-> fabric=<absent|connected|stale|stalled|disconnected> \
         fabric_rtt_ms=<n|-> fabric_link_age_ms=<n|-> no-bridge=<0|1> queued=<n> dup=<0|1>`. \
         Terminal, theme & host: `render=<ok|displaced|desync-risk|unverified|unjudged> \
         resizes=<n> flaps=<n> bells=<n> (at most once per 100 ms) bells_sounded=<n> last_bell=<ms|-> \
         os_appearance=<dark|light> theme_side=<dark|light|-> reported_scheme=<dark|light> \
         scheme_from=<theme|program> bg_dark=<0|1> conpty=<0|1> history_lost=<n> \
         journal_ring=<live|shedding|shed> journal_bytes=<n> \
         born=<cold|spare-parked|spare-warming|spare-ready> (a spare's adds spare_age_ms=<ms>) \
         fg_rss_mb=<n|-> job=<verdict|-> job_quiet_ms=<ms|-> \
         disk=<ok|warn|critical> disk_free_mb=<n> disk_room_mb=<n> disk_age_ms=<ms> \
         syspolicyd=<-|ok|full|stuck> syspolicyd_workers=<n>/<limit|->|- \
         syspolicyd_full_ms=<ms|-> attribution=<live|adopted|unknown> \
         fs_consent=<covered|denied|unknown> log_ep=<e> log_seq=<n>`. Additional \
         cross-reference keys (`app=`, `as=`, `enabled=`, `ep=`, ` fleet=`, `hash=`, `kind=`, \
         `origin=`, `pid=`, `reason=`, `reasons=`, `rev=`, `run=`, `since=`, `src=`, `step=`, \
         `subject=`, `subject_source=`, `ticket=`, `view=`, `wait=`, `warn=`, `why=`, `window=`) \
         appear in related status/JSON subviews.",
    ),
    v(
        "timeline",
        Read,
        Lines,
        Session,
        "timeline [<n>] [since=<id>]: the session EVENT TIMELINE",
        "— one `event <id> t=<ms> kind=<k> ...` line per event, read from the session `log` \
         (it takes its newest rows, up to 512 or 64 KiB, only after its other closed detail; \
         monotonic ids carried across seamless updates with a `handoff` row). Lifecycle kinds: \
         `spawned`, `state-change` (`state=closed`), `title-change`, `cwd-change`, `meta-change`, \
         `agent-change`, `task-change` (`rev= state= attention=<0|1> [why=] [exit=]`), \
         `status-change` (`phase= outcome= exit_code= signal= confidence=`), `human`, \
         `modes-restored` (`from=<pgid> to=<pgid> program=<name|-> reverted=<csv> bytes=<n>` or \
         `reason=manual source=<ctl|menu> reverted=<csv|-> bytes=<n>`). Fabric/messaging kinds: \
         `hold`, `inbox`, `inbox-seen`, `post`, `fetch`, `post-landed`, `topic`. Other kinds: \
         `render` (`desync-risk run=<r> displaced=<+-k> net=zero at=<ms>` or `healed run=<r> \
         after_ms=<ms>`), `harness-act` (`act=<act> holder=<holder>`), and `program-state` \
         (`state=<idle|working|blocked|done|error|-> kind=<permission|question|auth|->`, the \
         newest 64 a run; `log` coalesces flaps with `coalesced=<n>`). Final `closing reason= \
         by=` is delivered via `subscribe @<sid> events` and retained in `exits`.",
    ),
    v(
        "log",
        Read,
        Lines,
        Session,
        "log [<node>|n] [lvl=] [since= ep=] [n=] [kinds=] [class=turn] [tier=content] [--json]: \
         THE LOG",
        "Hierarchical session log: session (`-`), runs (`r<n>`), agent turns (`t<n>`) and steps \
         (`p<n>`), plus point marks `human` (`why=mail`, `reached=0`), `input` (`verb= [act=] \
         [re=] [dropped= [pool=shared]] [cause= cause_k=]`), `task` (`rev= state= attention= \
         [why=] [code=<exit>]`), `agent`, and `program` (OSC 7501 `status program_state=`: \
         `state=<idle|working|blocked|done|error|none> [ask=<permission|question|auth>] \
         [coalesced=<n>] src=engine conf=claim`), and `row` (`row=<kind>`: a timeline row the \
         log holds as its row alone, such as `job-change`). Header (stderr on `aterm ctl`): \
         `OK <n> v=2 as_of=<seq> ep=<e> oldest=<seq>[ lost=<k>][ more=<k>]`, then `<seq> \
         <open|close|mark|amend|gap> <node> <kind> [par=<node>] <k=v ...> [by=<actor>] \
         src=<where> conf=<exact|strong|heuristic|claim> t=<wall ms>` (`close` adds `out=`, \
         `dur_ms=`, `dur=wall`, `steps= appr= qs= walls= stalls= errs= lost=`; `detail=` holds \
         sanitized command or wall reset). Never logs command args or typed bytes; `tier=content` \
         adds in-memory-only `title=`, `meta field=attention attn_text=` (`fp=`), `reason=`, \
         `topic_name=`, `since=`, `closer=`, `reply=`, `row_text=`. Page with `since=<as_of> \
         ep=<ep>` (updates write `mark - handoff carried=<n> shed=<m> epoch=<ep>` or \
         `gap why=handoff epoch=<ep>`; evictions/resets emit `gap why=<evicted|reset> lost=<k> \
         resync_to=<seq>`). Filter by \
         `<node>`, `lvl=session|run|turn|step`, `n=`, `kinds=`, `class=turn`, `--json`. Closed \
         sessions remain readable via `log @<sid>` (last 512, journaled unless `[session_log] \
         journal = false`, mirrored under `[fabric] log = \"auto\"|\"turns\"`). `log n` \
         (Owner-only) reads the instance node log (`open n start \
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
        "Token-budgeted digest of `log` (`budget=` default 600 tokens = bytes/4): `OK <n> brief \
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
        "Appends a `src=mark conf=claim` record to `log` (`OK seq=<n> node=<node> by=<actor>`). \
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
        "Header: `OK <n> total=<N> runs=<R> flaps=<F> risks=<D> \
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
         full clear/switch. Under `[render] repaint_nudge = \"on\"|\"log\"|\"off\"` (default \
         `on`), aterm repairs an agent-class `desync-risk` epoch at a quiet moment by briefly \
         signalling a 1-row-shorter PTY size then restoring it (`by=nudge`). `status` carries \
         `render= resizes= flaps=`.",
    ),
    v(
        "metrics",
        Read,
        Status,
        App,
        "render/latency counters [reset|percentiles]",
        "`percentiles` prints `n_<x>=` and `<x>_p50/_p95/_p99_ms=` for `input`, `present`, \
         `render`, `key_write`, `key_queue`, `pre_present`, `acquire`, `acquire_queue`, `resize`, \
         `reflow`, `present_tainted`, `term_wait_redraw`/`term_wait_press`, `echo`, \
         `present_glass`, `out_req`, `ime`, `status_glance` (`max_status_glance_ms=`), \
         `need_publish` (`max_need_publish_ms=`), and `tx_chunk`/`tx_last` (`max_tx_hold_ms=`), \
         plus `turns_by_root=<root>:<n>/<p50>/<p95>/<p99>` and counters `input_edges_dropped= \
         input_predicted= ime_cancelled= input_refused= reply_dropped=`. Plain `metrics` prints \
         `max_frame_gap_ms=`, `rust_main_to_first_present_ms=`, `rust_main_to_first_visible_ms=`, \
         `startup_phase_schema=1`/`startup_phase_valid=` with `startup_*_ms` \
         (`startup_gui_prepare_ms`), `startup_attach_schema=1`/`startup_attach_valid=`, \
         `effect_pipeline_builds=`/`effect_pipeline_build_ms=`, `first_present_ms=`, \
         `first_visible_ms=`, PTY child round-trip `n_echo= \
         echo_p50_ms=/echo_p95_ms=/echo_p99_ms= echo_last_ms=/echo_max_ms= \
         echo_total=/echo_arms=/echo_coalesced=/echo_expired=/echo_dropped_locked=`, main-loop \
         census `max_turn_ms= max_turn_owner= max_turn_at_ms= last_turn_ms= turns= long_turns= \
         long_turn_threshold_ms= main_busy_ms_per_s=`, update handoff counters `input_dropped= \
         input_incoherent= handoff_claim_stamps= handoff_warm_pre_dial_ms=`, \
         `process_birth_to_rust_main_ms=`, `startup_prepare_schema=1`/`startup_prepare_valid=`, \
         `control_accepts=` … `control_busy_replies=`, and PTY fd census `pty_masters= \
         pty_sessions= pty_kits=`. Compare `echo_*` (child round-trip) with `input_*` (key to \
         next content present, or proven prediction when `input_predicted=` > 0); always quote \
         `n_*` alongside percentiles.",
    )
    .json(Json::Emits),
    v(
        "sys",
        Read,
        Lines,
        Session,
        "sys [<view>] [--json] [since=<c>]: cores, GPU, memory, disk of the machine, aterm and \
         this tab",
        "Views: `overview` (default) | `tab` | `aterm` | `global` | `jobs` | `cores` | `gpu` | \
         `mem` | `disk` | `procs` | `why`; options: `scope=pane|tab|window|aterm|global` \
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
        "— type `<text>`, verify submit (`submit=none` skips submitting; default `enter`, or any \
         `key` name), wait for screen settle (`idle=<ms>`, capped at `timeout=<ms>`), and return \
         the settled screen. Options: `[idle=<ms>] [timeout=<ms>] \
         [submit=<key|none|guarded:<re>>] [settle=match:<re>|gone:<re>|submitted] \
         [submit_window=<ms>] [presses=<n>] [submit_verify=<auto|seq|block>] [trim=<0|1>] \
         [typed=<0|1>] [cadence=<ms>] [cap=<n>] [yield=<floor>] [if-gen=<epoch>.<seq>] \
         [if-rows=<first>-<last>:<hex16>] [cause=@<off>|<seq>]`. `cause=` links the turn in `log` \
         to a held mail (`@<off>`) or log record (`<seq>`; `ERR cause` if invalid). `presses=<n>` \
         (default 3, max 10) retries submit within `submit_window=<ms>` (default 2000). \
         `settle=match:<re>` settles when a visible row matches `<re>`; `settle=gone:<re>` waits \
         for `<re>` to appear within `submit_window` then leave (falling back to idle settle if \
         never seen); `settle=submitted` returns as soon as submit is verified \
         (`status=submitted`). `submit_verify=auto` (default) verifies Claude Code submits by \
         typed text leaving the composer, other TUI prompts by `seq`, and shells by command \
         block. `submit=guarded:<re>` submits with Enter only if the cursor row matches `<re>` \
         under the same terminal lock (`key if=`; `OK 0 turn skipped reason=guard submitted=0 \
         seq=<n> id=<n>` on first-press miss; adds `pressed=<n>` when re-presses stop). A `hold` \
         arriving before bytes land answers `ERR halted …`, or mid-turn stops immediately (`OK 0 \
         turn skipped reason=halted submitted=0 seq=<n> id=<n>`). Reply: verdict line (`id= \
         submitted=<0|1> status=settled|timeout|submitted seq= dur_ms= hash=<FNV16>[ \
         task_rev=<n>][ agent_rev=<n>][ frame=open][ trimmed=<k>]`; `agent_rev=` feeds `await \
         agent <words> since=<n>`) then rows (`trim=1` drops trailing blank rows while `hash=` \
         hashes the untrimmed screen). `typed=1` types key-by-key at `cadence=<ms>` (default 70) \
         up to `cap=<n>` graphemes (default 240, max 4096) and pastes any remainder (`typed=<keys> \
         pasted=<graphemes>`), stopping with `OK 0 turn skipped reason=<deadline|person|changed> \
         typed=<k> pasted=<p> submitted=0 seq=<n> id=<n>` if overtaken by timeout, a human key, or \
         a screen/input-frame change. `yield=<floor 0..=1>` waits for `trail status` `momentum=` \
         to drop to `<floor>` before typing (`yielded_ms=<n>`, or `ERR yield timeout \
         momentum=<v>`). `if-gen=<epoch>.<seq>` (`status gen=`) and \
         `if-rows=<first>-<last>:<hex16>` fence on the screen generation or row-range FNV-1a-64 \
         hash before typing (`reason=changed` on mismatch). `if-rows=` never vouches for half a \
         frame: a `typed=1` turn whose first look finds its input frame in a DEC 2026 frame still \
         within the sync timeout waits within `timeout=` for that frame to close and judges the \
         fence on it; any other turn waits as `text` does, and a frame still open then has moved; \
         the settled screen is read on a whole frame too (` frame=open` when not); \
         `if-supervisor=<holder>` fences on the live supervisor claim. Leading \
         `id=<epoch>:<producer>:<seq>` enables exactly-once replay (`OK 0 dup=1` on duplicate; see \
         `send`). Refused with `ERR busy input-unread …` into a `stalled` or stopped program \
         (`turn` takes no `unread=ok`), or appends `warn=input-unread input_bytes=<n> \
         input_wait_ms=<ms>` when input is >= 1 s old.",
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
        "Single-holder TTL lease (default 30000 ms, max 600000), shown in `who` as \
         `driving=lease:<holder>`. Forms: `lease [status]` | `lease acquire [ttl=<ms>] \
         [holder=<name>] [hard] [here]` | `lease release [holder=<name>] [force] [here]`. \
         `acquire` refuses a live `turn` or another holder's live lease (renews for the same \
         holder); `here` binds the lease to the requesting connection so it releases on \
         disconnect. Advisory by default (`turn` checks it); `hard` (`status` `hard=1`) refuses \
         every other connection's `send`/`key`/`ctrl`/`feed`/`mouse`/`paste`/`turn`/`feed-bin` \
         with `ERR busy lease=<holder>`. `status` also reports `pid=<n>` (peer PID of the \
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
        "— `seq=<n>` is the content baseline (`await seq <n+1>` waits for resulting output). \
         Leading options (before `--`): `[id=<epoch>:<producer>:<seq>] [if=<re>] \
         [if-gen=<epoch>.<seq>] [if-fp=<hex16>] [if-human=<n>] [if-supervisor=<holder>] \
         [unread=ok] [cause=@<off>|<seq>] [act=approve|deny|answer] [as=person]`. `id=` (shared \
         by `send`, `key`, `feed-bin`, `turn`) provides idempotency against the session's \
         `nonce=` (`OK dup=1` for `Status` verbs and `OK 0 dup=1` for `turn`; `ERR in-doubt \
         seq=<n>` if the prior attempt did not complete `OK`; `ERR epoch` across relaunches). \
         `if=<re>`, `if-gen=`, `if-fp=`, `if-human=` and `if-supervisor=` fence under the \
         terminal/meta lock (see `key`). `unread=ok` bypasses the `input=stalled|stopped` refusal \
         (`ERR busy input-unread …`); slow unread queues (>= 1 s) append `warn=input-unread \
         input_bytes=<n> input_wait_ms=<ms>`. `cause=@<off>|<seq>` links a turn opened within 5 s \
         to mail or a `log` record (`ERR cause` if invalid; `cause_k=mail|rec`). \
         `act=approve|deny|answer` tags the dismissed approval/question box in `log` \
         (`out=approved|declined|answered` for harness, `out=passed by=<actor>` otherwise, \
         `re=<box>`, rate-limited with `dropped=<n>` / `pool=shared` on `mark - input verb= \
         by=`). `as=person` (debug / `dev-seams` only, with no other option) routes through the \
         human input seam (`status human_ms=` / `human_seq=`).",
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
        "bracketed-paste text to the PTY",
        "Applies DEC 2004 bracket guards (if enabled at write time), control-byte sanitization \
         and LF->CR without raising the interactive `confirm_multiline_paste` banner. Accepts a \
         leading `if-supervisor=<holder>` fence.",
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
         turn=`, `ERR usage`, `ERR busy input-unread bytes=<n> wait_ms=<ms> \
         input=<stalled|stopped>`, `ERR badregex`, guards/fences, then `id=`. On macOS, raw input \
         unread >= 1 s appends `warn=input-unread input_bytes=<n> input_wait_ms=<ms>`; stalled or \
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
    v("ctrl", Write, Status, Session, "send a control char", "")
    .input(DRIVEN.with(Input::STAMPS_SEQ)),
    v(
        "feed",
        Write,
        Status,
        Session,
        "write raw bytes to the PTY",
        "",
    )
    .input(DRIVEN.with(Input::STAMPS_SEQ).with(Input::PINNED)),
    v(
        "hwkey",
        Write,
        Status,
        App,
        "hwkey <char|name> [mods=] [count=] [interval=]: inject a key through the OS event queue",
        "macOS only (`ERR hwkey: hardware-path injection is implemented on macOS only` \
         elsewhere). Posts a real `NSEvent` through the OS event queue (`shape=press` builds from \
         a CoreGraphics key event so layout and Option dead keys compose) to measure full \
         hardware-path typing latency in `metrics percentiles` (`n_key_write`). Replies `OK \
         posted=<n> key=<0|1>`. Takes leading `unread=ok`; refused (`ERR hwkey`) while a palette, \
         Overview or tab menu is open.",
    )
    .input(Input::PTY.with(Input::WRITES).with(Input::APP_UNREAD)),
    v(
        "feed-bin",
        Write,
        Status,
        Session,
        "length-prefixed raw bytes to the PTY",
        "`feed-bin <n> [id=<key>]` then `<n>` raw bytes. A refused `id=` still drains the `<n>` \
         payload bytes to keep the connection framed.",
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
        "length-prefixed bytes to the PTY with PASTE semantics",
        "Binary twin of `paste` (DEC 2004 brackets at write time, control-byte sanitization, \
         LF->CR).",
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
    v("mouse", Write, Status, Session, "inject a mouse event", "")
    .input(DRIVEN.with(Input::STAMPS_SEQ)),
    v(
        "pointer",
        Write,
        Status,
        App,
        "pointer [move <r> <c>|move key=<k>|click|wheel dy=<n>|leave|status|mods <m>]: drive the \
         POINTER",
        "Drives the front window's cursor path (`App::on_cursor_moved`, `on_cursor_left`, \
         `MouseInput`, `MouseWheel`, `ModifiersChanged`). `move <r> <c>` targets window cell \
         `<r>,<c>` (negative rows target chrome bands above row 0, e.g. `move -1 <c>`) and \
         replies `OK at=<row>,<col> px=<x>,<y>`; on native views, `move key=<ui-key>` or `move \
         xy=<x>,<y>` replies `OK at=native view=<id> x=<x> y=<y> key=<ui-key|-> px=<x>,<y>` (`ERR \
         the page under the pointer changed …` if re-present fails). `pointer leave` or \
         unpositioned status replies `OK at=-`. `click` (Owner-only; rate-limited on band \
         capsules to 10/min with `ERR busy notice: 10 presses a minute retry_ms=<ms>`) \
         left-clicks at the current pointer position. `wheel dy=<n> [dx=<n>] [unit=lines|pt] \
         [key=<ui-key>|xy=<x>,<y>]` scrolls (`1..=10000`). `mods <list>` (Owner-only) sets held \
         modifiers (`cmd`/`super`, `ctrl`, `alt`/`option`, `shift`, `link`, or `none`) until the \
         next physical event (read hovered links via `cell <r> <c>` `link=`). Takes leading \
         `unread=ok`.",
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
    v("focus", Write, Status, Session, "send focus in/out", "")
    .input(Input::PTY.with(Input::WRITES).with(Input::FRONT_DRIVING).with(Input::FENCES)),
    v(
        "scroll",
        Read,
        Status,
        Session,
        "scroll the view (up|down|top|bottom|N|prev/next-prompt)",
        "A bare `scroll` mutates nothing and reports the current position.",
    )
    .input(Input::FENCES),
    v(
        "select",
        Read,
        Status,
        Session,
        "select region / word <r> <c> / line <r> / block <r1> <c1> <r2> <c2> / extend / clear",
        "`extend <r> <c>` grows the current selection (shift-click). `word` and `line` select \
         across soft wraps (except ragged double-width wraps) up to hard newlines. Made on a \
         whole frame as `text` reads one; `OK frame=open` says the program's DEC 2026 frame was \
         still unfinished at the read's bound.",
    ),
    v(
        "signal",
        Signal,
        Status,
        Session,
        "signal <sig> [pid=<n>] [quiet=<s>]: send a signal to the foreground process group",
        "POSIX only (`ERR signal unsupported on this platform` on Windows). `<sig>` is \
         `int|quit|tstp|hup|term|kill|cont` (or `c`, `z`, `sig*`). `pid=<n>` requires `<n>` to \
         lead the foreground process group. `quiet=<s>` refuses if a human typed within `<s>` s \
         (`ERR busy person human_ms=<ms>`) or unread input is queued (`ERR busy input-unread \
         bytes=<n>`). On macOS, `term`, `kill`, `hup` and `quit` discard unread raw-mode input \
         first (`OK signalled pgrp <n> discarded=<bytes>`, or `pid <n>`). A `status \
         input=stalled` program that survives `term`/`hup`/`quit` stays stalled (`status \
         input=stalled input_bytes=0`) until it exits, draws or reads. To restart a supervised \
         agent on its conversation, use `restart`.",
    )
    .input(Input::PTY.with(Input::FENCES)),
    v(
        "restart",
        Signal,
        Status,
        Session,
        "restart [pid=<n>] [if-stalled] [quiet=<s>]: restart the tab's agent in place",
        "POSIX only (`ERR restart unsupported on this platform` on Windows). Stamps the \
         supervised foreground agent (Claude Code, agy, codex-alab), sends `SIGTERM` (escalating \
         to `SIGKILL` after 5 s if still stalled and deaf, or 30 s otherwise), and has the window \
         harness relaunch it on the same conversation. Options: `pid=<n>` (must lead foreground \
         pgrp), `if-stalled` (requires `status input=stalled`), `quiet=<s>` (`ERR busy person \
         human_ms=<ms>` if human input within `<s>` s). Replies `OK restarting pid <n> \
         cause=<stall|asked>[ discarded=<bytes>]`, `OK restart gone pid <n>`, or `ERR restart \
         unavailable: \
         <not-the-hosts-agent|not-relaunchable|not-supervised|driver-lease|driver-turn|halted|start-unread|no-state>` \
         (or `ERR not-stalled`, `ERR bad restart argument: <word>`, `ERR restart failed: \
         <what>`). Works even when `[harness] relaunch = false`.",
    )
    .input(Input::PTY.with(Input::FENCES)),
    v(
        "reset",
        Write,
        Status,
        Session,
        "reset [flush]: restore terminal modes to host defaults; keeps screen and scrollback",
        "Reverts negotiated terminal modes (alt screen, kitty keys, modifyOtherKeys, mouse \
         tracking, focus, DEC 2026/2048/2031, cursor/keypad/paste/wrap/charset, hidden cursor, \
         scroll region), cancels torn escapes, closes OSC 8 links, and restores theme colours \
         (OSC 104/110–119, `colors`) without clearing screen or scrollback. Records \
         `modes-restored reason=manual source=ctl reverted=<csv|-> bytes=<n>` on `timeline` and \
         replies `OK reset reverted=<csv|-> bytes=<n>` (`flush` drops unread macOS tty input and \
         appends `discarded=<n|->`). Equivalent to `invoke ResetTerminal`.",
    )
    .input(Input::PTY),
    v(
        "tab",
        Write,
        Status,
        App,
        "drive a window's tabs (new|N|next|prev|close [N]|move <from> <to>)",
        "Flagless drives the front window; `@<sid> tab …` drives the window hosting `<sid>`. `N`, \
         `<from>`, `<to>` and `<active>` in `OK <active> <count>` are 0-based (`inspect app/v1 \
         tabs` `index=`). Refuses closing a tab with a running job (`ERR close refused (a running \
         job armed the last-tab confirm)` or `ERR close refused (a running job in that tab)`) or \
         an uncheckpointed native doc (`--headless` never refuses).",
    )
    .input(Input::PTY),
    v(
        "pane",
        Write,
        Status,
        App,
        "pane <left|right|up|down>: move keyboard focus to the adjacent pane",
        "Acts on the front window's active tab (`spawn split=v|h` creates splits) and replies `OK \
         moved=<1|0>` (read layout rects and focus via `panes`).",
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
            "; aliases ",
            open_aliases!(),
            "), replying `OK opened <target>` (`OK closed <target>`). `open fact-menu \
             <model|effort|path|branch|context|usage|close>` opens or closes that composer-rule \
             menu. Opening `menu`, `tab-menu`, `overview`, `autopilot-menu` or `fact-menu` is \
             refused with `ERR halted` while any session is held (see `hold`)."
        ),
    )
    .input(Input::NONE),
    v(
        "edit",
        Write,
        Status,
        App,
        "open a file in a fresh Editor tab beside the caller: @<sid> edit <file-uri> \
         [line=<L>[:<C>]]",
        "`[create=1] [for=<word>] [wait=0]`. Owner-only (`ERR scope`). Opens a new Editor tab to \
         the right of `<sid>`'s tab (`OK edit ticket=<t> view=<v> doc=<d> uri=<canonical-uri>`) \
         and holds `<sid>` (`status` `editing=`, typing returns `ERR busy editing`) until `await \
         edit ticket=<t>` resolves (`wait=0` skips the hold). Errors: `ERR unsupported headless`, \
         `ERR no-window`, `ERR refused \
         <not-regular|too-large|invalid-utf8|symlink|file-provider|missing|bad-uri|unreadable>`, \
         `ERR timeout ticket=<t>` (30 s), `ERR unsupported create` (`create=1`).",
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
        "`doc ls` replies `OK <n>` then `doc=<d> uri=<canonical-uri> seq=<s> dirty=0|1 \
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
        "dispatch an exact semantic native-app action: act app/v1 view <view-id> <ui-key> <action>",
        "[value]",
    )
    .input(Input::NONE),
    v(
        "settings",
        ConfigWrite,
        Status,
        App,
        "settings [open|close|toggle|section <name>]; `settings set|unset <key> [value...]`",
        "answering `OK saved: <key> = <value>`, `OK removed: <key>; now <effective>` (an unset, or a set to the key's default) or `OK <key>: unchanged`",
    )
    .input(Input::NONE),
    v(
        "invoke",
        Write,
        Status,
        App,
        "invoke <action>: fire a menu action by name (enabled-gated; names via `controls menu`)",
        "`Paste` honors the unread-input gate (takes leading `unread=ok`). Off macOS, `CloseTab` \
         or `Quit` over a running job (and multi-tab `Quit` on Windows) parks an in-window \
         confirmation and replies `OK confirm pending \
         kind=<quit|close-window|close-tab|close-pane> window=<n>; `confirm yes|no` answers it`.",
    )
    .input(Input::PTY.with(Input::APP_UNREAD)),
    v(
        "confirm",
        Write,
        Status,
        App,
        "confirm yes|no: answer a close or quit waiting in the window",
        "Answers an in-window confirmation (`controls front` `confirm open=true`) raised by a \
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
        "Debug-only stand-in for the human at the window; requires `ATERM_DEBUG_PERSON_INVOKE=1` \
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
         tick= scanned= material= emitting= vis= drain= seq= streak= diag`)",
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
         `motion_animates` includes W11 unfocused demotion over `motion=full`, `fx_focused` is \
         true on focus, typed wake or active recording, and `panes` counts per-pane split \
         engines)",
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
         `audio=opening|live|paused|stopped|failed|reopening|wedged|inert` tracks the audio \
         worker and `reopens_left=` the device reopen budget; typed text is never reported)",
    ),
    v(
        "kitty",
        Write,
        Lines,
        App,
        "kitty [wear <key>]: the cursor cat collection, and which one to wear",
        "Bare `kitty` lists `cat key= coat= iris= age= seen= worn=` rows (`worn=1` on the active \
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
         line naming the latest `PressCustody` transition (`last=`) and the latest transition \
         that actually moved the viewport offset or selection (`changed=`). Read-only.",
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
        "Mints a session (a new tab, or `split=v|h` splitting the focused pane); `<sid>` is \
         immediately addressable. `theme=<name>` sets a session theme (`theme session`; `ERR \
         theme \"<name>\": <why>` if invalid). Target a window via `window=<id>` (`inspect app/v1 \
         tabs`; `window=0` on `--headless`; `ERR no such window <id>` if unknown) or `@<sid> \
         spawn`. `raise=` defaults to `true` when no window is named and `false` when one is \
         (`raise=true` forces raising). Invalid `cwd=` returns `ERR no such folder <path>` or \
         `ERR no access to folder <path>`. Connected form (Owner-only): `spawn \
         connected=controlled|controller place=window|tab of=<sid> [cwd=<path>]` mints a `both` \
         connection with `of=<sid>` (`controller` sets `ATERM_OBSERVE_SESSION_ID`; `place=window` \
         is `ERR headless` without a GUI). Worker form (Owner-only): `spawn … \
         agent=<claude|gemini|agy|muse> [model=<m>] [autopilot=<land|cruise>] [from=<sid>] \
         len=<n>` replies `GO <n>`, reads `<n>` task bytes (`1..=65536`, no NUL; `ERR spawn \
         agent=<word>: the task is <n> bytes (max 65536)` beyond; `aterm ctl spawn agent=<word> \
         [model=<m>] [cwd=<p>] -` reads stdin), writes `<state>/tasks/` (`ATERM_TASK_FILE`), and \
         at the login shell's (`zsh`/`bash`/`fish`) first prompt launches the interactive agent \
         (`--prompt-interactive=` for `gemini`/`agy`, `GEMINI_CLI_TRUSTED_FOLDERS_PATH=` for \
         `gemini`, `--trust-workspace` for `muse`). Sets `task` to `working` (`from=<sid|ctl>`) \
         and `autopilot` to `land` (or `autopilot=cruise`; `meta set autopilot land` switches \
         later); marks `stalled why=shell` if no shell prompt within 20 s, `stalled why=startup` \
         if no agent composer within 120 s of `since_ms=`, or `stalled why=held` if stuck 120 s \
         at an unhandled screen. Refuses with `ERR unsupervised: <switch>` when the window \
         harness cannot supervise (`[harness] enabled = false`, `headless = false`, or `autopilot \
         = \"off\"`), and `ERR agent codex: not yet` for `agent=codex`.",
    )
    .input(Input::NONE),
    v(
        "close",
        Write,
        Status,
        Session,
        "@<sid> close: retire that session and close its pane",
        "Replies `OK closed <sid>` once the session leaves the registry, its pane closes, its PTY \
         is hung up, and `exits` records `reason=ctl-close by=<caller>`. Collapses one split pane \
         (closing the tab on its last pane, the window on its last tab, and the process on its \
         last window). Refuses if unresolvable (`ERR no such session`), if a job is running in \
         that pane (`ERR close refused (a running job in that pane)` or `ERR close refused (a \
         running job armed the last-tab confirm)`; `--headless` never refuses), or if \
         view/teardown barriers hold the session (`ERR close deferred (another window still \
         displays this session)`, `ERR close deferred (another tab still displays this session)`, \
         `ERR close deferred (the window teardown has not run)`, `ERR close deferred (the session \
         outlived its window's teardown)`, `ERR close deferred (the session outlived its pane)`, \
         `ERR close deferred (a pending update handoff is being cancelled first)`, `ERR session \
         view changed during close`, `ERR close refused (the pane is still open)`). Takes no \
         arguments (`ERR usage: close`).",
    )
    .input(Input::PTY),
    v(
        "task",
        Read,
        Status,
        Session,
        "task [mark <state> …]: a worker's task record: OK task rev= state= from= since_ms=",
        "`result_bytes= [why=] [exit=] [result_path=] agent= attention=` (or `OK task -` when the \
         session has no task). `state=`: `working`, `done` (under Land, work landed on remote \
         `main` and defended with `Nothing left:`), `exited` (`exit=<code>`), or `stalled` \
         (`why=startup|shell|held`; resumes `working why=moved` or `working why=turn`). `why=` is \
         `landed|exit|shell|startup|held|turn|moved` (or legacy `marker|done-check`). `rev=` \
         increments on record changes and on `meta set attention owner=supervisor|needs` changes \
         (`status` and `sessions` carry `task_state=<state|-> task_rev=<n>`, `subscribe … events` \
         emits `EVENT <local> task rev=<n> state=<s> attention=<0|1> [why=<w>] [exit=<n>]`, and \
         `turn` verdicts carry `task_rev=`). `task mark <working|done|exited|stalled> \
         [from=<s-…|ctl>] [why=<word>] [exit=<n>] [result_bytes=<n>] [result_path=<pct>]` \
         (Owner-only; `ERR denied` for edges; `ERR busy update-committing` if a seamless handoff \
         blocks > 10 s) updates the record and replies with `changed=<0|1>`.",
    )
    .forms(&[form(Word("mark")).owner_only()]),
    v(
        "result",
        Read,
        Lines,
        Session,
        "result [full]: a worker's result, the report it ended its task with",
        "Replies `OK <n> source=transcript session=<id> uuid=<row> bytes=<n>` (or `OK <n> \
         source=screen program=<agent> bytes=<n>` for Gemini CLI, agy and Muse Code) followed by \
         `<n>` lines (up to 24000 chars, ending with `… truncated; full: <path>` when truncated; \
         `result full` adds `full=1` and returns the full transcript exchange since the task \
         prompt). Snapshotted when `task` is marked `done` (and retried on \
         `no-reply`/`no-marker`, updating `result_bytes=`). Errors: `ERR no task`, `ERR result \
         unavailable \
         why=<working|stalled|exited|no-transcript|no-reply|no-marker|empty|no-check|no-report|no-snapshot|unreadable|no-prompt|no-words>`.",
    ),
    v(
        "ready",
        Read,
        Status,
        Session,
        "ready [<ms>|timeout=<ms>]: block until the session is alive and idle",
        "Answers `OK ready prompt` (shell integration shows a prompt) or `OK ready idle` (60 ms \
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
        "await <idle|seq|match|gone|block|frame|inbox|consent|momentum|agent|task|log|sys> …: \
         wait",
        "Blocks on a condition (default `timeout=30000` ms, `await consent` `300000`, max \
         `600000`; `OK timeout` exits 124 in `aterm ctl`; `ERR exited` if the session ends; 64 \
         wait slots shared across tabs before `ERR control server busy`). Forms: `await idle \
         <ms>` (no output for `<ms>`). `await seq [<n>]` (content `seq` >= `<n>`, or next change; \
         `OK seq <current>`). `await match <re> [rows <a> <b>]` and `await gone <re> [rows <a> \
         <b>]` (visible row matches or all rows in range clear of `<re>` on a closed DEC 2026 \
         frame or for 1 s; `ERR bad rows` if span is empty). `await frame` (open DEC 2026 frame \
         closes, or 60 s hang cap; see `text --json` `\"frame\":\"open\"`). `await block` \
         (running command completes). `await sys <predicate> [for=<dur>] [scope=tab|aterm]` \
         (`<predicate>` is `quiet [<term> …]` or `<term> [and <term> …]` over `cpu`, `cpu.l<n>`, \
         `cpu.<cluster>`, `aterm`, `tab`, `wait`, `gpu`, `mem`, `swap`, `decompress`, `thrash`, \
         `tab.mem`, `aterm.mem`, `throttle`, `jobs.stuck`, `jobs.quiet`, `jobs.running`, \
         `load<0.5`, `cores_idle`, `avail`, `slots` answering `slots=<usable>/<n>` or \
         `why=unread_format`, or `job=<id>` / `task=<id>` `<verdict>` / `class=<class>` answering \
         `job=<id> verdict= class= [wait=] [quiet_ms=] [phi=] [exit=]` or `ERR gone job=<id>`; \
         `quiet` defaults to `cpu<25% wait<0.25 gpu<10% throttle=none swap<1MB/s`, dropping \
         unmeasurable terms as `skipped=<metric>:<why>`; replies `OK sys <metric>=<value> … \
         held_ms= as_of= next=` or `ERR unavailable <metric> why=<not_sampled|…>`) or disk demand \
         `await sys <free>=<size>|free>=room|disk=<word>[,…]> [and <term>] [vol=<path>]` (`room` \
         = `status disk_room_mb=`; frees unused build caches and replies `OK sys free_bytes= \
         disk= vol= freed_bytes= held_ms=0 as_of=`, `ERR unmet free_bytes= need_bytes= vol= \
         freed_bytes= why=<spent|held-back|cleanup-off|settings|not-reclaimed> hogs= \
         largest_bytes=`, or `ERR unavailable free \
         why=<unsupported|remote|automounted|no-mount|unreadable|watch-stalled|too-large|no-watch|room-unknown>`). \
         `await momentum <floor 0..=1>` (waits for `trail status momentum=` <= `<floor>`; `OK \
         momentum <v>`). `await inbox [since=<id>] [re=<off>] [topic=<t>] [kinds=<k,...>]` \
         (latches on inbox row > `since=` of accepted kinds, replies to `re=<off>`, broadcast on \
         `topic=<t>` from `post to=say:<t>`, or `hold` transition `OK inbox hold=0|1`; replies \
         `OK inbox <id>`, or `ERR bad since high=<n>`; `ack … verdict=` wakes `re=`). `await \
         agent <word>[,<word>…] [since=<rev>]` (`status agent=` in \
         `busy|prompt|question|idle|survey|unknown|-|wall|wall:<kind>` with `rev` > `since=`, \
         e.g. `agent_rev=` from a `settle=submitted` turn; `OK agent <word> rev=<n>`). `await \
         task [@<sel>[:<rev>][,...]] [any|all] [since=<rev>]` (edge-triggered on `task` `rev` > \
         `since=`; replies `OK <n>` or `OK <n> status=timeout` then `@<sid> rev=<n> \
         state=<working|done|exited|stalled|-|closed> agent=<word> attention=<pct|-> \
         result_bytes=<n>`). `await consent [kind=debugger] [timeout=<ms>]` (latches when \
         `privacy` `fs_consent= fda= attribution=` or \
         `debugger_mode=<enabled|disabled|unrecognized>` changes from baseline; `OK consent … \
         elapsed_ms=`). `await log need|open=<lvl>|close=<lvl>|kind=<k> [since=<seq> ep=<e>]` \
         (`<lvl>` in `session|run|turn|step`, `out=fail`, `conf=claim`, `status \
         log_seq=/log_ep=`; replies `OK seq=<n> ep=<e> [as_of=<head> latched=1] \
         [gap=<why>:<lost>] <record>` or `OK gap why=<w> lost=<k> resync_to=<seq> ep=<e>`). \
         `await edit ticket=<t>|uri=<file-uri> [timeout=<ms>]` (Owner-only; replies `OK edit done \
         result=<finished|closed|aborted> saved=<0|1> seq=<n>`, `ERR unknown ticket`, or `ERR \
         hangup`).",
    )
    .lane(Wait)
    .forms(&[form(Word("task")).framing(Lines)]),
    v(
        "wait",
        Read,
        Status,
        Session,
        "wait [<ms>]: block until the running command completes (OSC 133)",
        "Answers `OK complete <id> exit=<code|->`; with nothing running it returns the newest \
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
        "subscribe @<sel>[,...]|@* <streams> [since=][every-frame]: push \
         DELTA/EVENT/GAP/BYTES/MAIL;",
        "`streams=screen,cursor,cells,bytes,events,mail,log,sessions` (plus inline modifiers \
         `timestamps`/`ts` for `T <local|*> <t_us>` prefixes and `trim` for `screen,trim`; \
         resumed via `since=` for content or `since-log=<seq> log-ep=<e>` for `log`; each target \
         acks `sub <local> <sid>[ log_seq=<seq> log_ep=<e>]`). `log` pushes `LOG <local> \
         <record>` (`log:lvl=<session|run|turn|step>` and `log:class=turn` filter) and `GAP \
         <local> log-lost=<n> why=<evicted|handoff|reset|…> resync_to=<seq> ep=<e>`. `events` \
         pushes `EVENT <local> \
         turn|block-complete|meta|title|bell|inbox|inbox-seen|post|fetch|post-landed|hold|topic \
         …` (`turn <id> submitted=<0|1> status=<s> dur_ms=<n> by=<who>`, `agent <word> rev=<n> \
         gen=<e.s> fp=<hex16>` from `status agent=`, `task rev=<n> state=<s> attention=<0|1> \
         [why=<w>] [exit=<n>]`, `status phase=<p> outcome=<o> exit_code=<n|-> signal=<n|-> \
         confidence=<c>`, `human` (`status human_ms=`), `harness-policy gen=<n>`, `job <verdict> \
         job=<id> class=<class> rev=<n> [task=<id>] [code=<n>]` (`status job=`), `resize n=<k> \
         id=<last> to=<CxR> run=<r>[ dropped=<n>]`, `render desync-risk run=<r> displaced=<+-k> \
         net=zero at=<ms>` / `render healed run=<r> after_ms=<ms>` (`status render=`), `closing \
         reason= by=`, `exited`, and `GAP <local> events-dropped=<n>`). `sessions` (Owner-only) \
         pushes `EVENT * session-created <sid>[ watch=deferred]`, `EVENT * session-exited <sid> \
         reason=<shell-exit|ctl-close|ui-close|window-close|app-quit|unknown>`, `EVENT * \
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
        "Receiver opt-in for `post to=say:<topic>` (`<t>` in `[a-z0-9][a-z0-9._-]{0,31}`, max 32 \
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
        "POSIX only; requires `@<sid>`. For a tab with `status integration=degraded` (or \
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
        "Header: `OK <n> hold=<0|1> holder=<p|-> seen=<id> bus_head=<off> oldest_on_bus=<@off|-> \
         dropped=<n> pending=<n>`, then `msg <id> off=<n> t=<ms> from=<p> kind=<k> \
         trust=<human|agent|relayed|screen> [re=<n> re-id=<id>] \
         [verdict=<handled|refused|deferred>] [dl=<ms>] [late=1] [demoted=<k>] [via=<p,...>] \
         len=<n> [more=1] [truncated=1] text=<pct>` (text preview capped at 512 B; use `inbox get \
         <id>` or `inbox get @<off>` for full bodies; lost bus records below retention return \
         `ERR retained-from=<floor>` or `ERR no such record`) and queued outbound `post <id> \
         to=<> kind=<> off=- len=<n> [key=<token>]` rows. `<n>` selects the newest `n` rows above \
         `since=`; `--peek` leaves rows unlisted and `--meta` omits `text=`.",
    ),
    v(
        "inbox get",
        Read,
        Bytes,
        Session,
        "inbox get <id> | inbox get @<off>: one message's body, by row id or by broker offset",
        "Replies `OK <nbytes>[ truncated=1 len=<true-size>]` then `<nbytes>` raw bytes (up to 256 \
         KiB, unpreviewed `inbox` `text=`) from the local ring without moving `seen=`. `inbox get \
         @<off>` fetches by broker offset (including evicted `dropped=` rows) from the ring or \
         through the bridge (`OK <nbytes> off=<n> from=<p> kind=<k> trust=<t> [re=<n>] [dl=<ms>] \
         [demoted=<k>] [via=<p,...>] [verdict=<v>] [truncated=1 len=<n>]` + body; errors: `ERR \
         retained-from=<floor> off=<n>` below `oldest_on_bus=`/`bus_head=` retention, `ERR no \
         such record off=<n>`, `ERR timeout off=<n>` after 10 s, `ERR fabric <state> off=<n>`, or \
         `ERR busy` past 8 parked reads).",
    )
    .forms(&[form(AtWord).lane(Wait)]),
    v(
        "inbox seen",
        Write,
        Status,
        Session,
        "inbox seen <id> [handled|refused|deferred]: advance the HANDLED watermark (and ack)",
        "Lists all rows `<= <id>`, advances `seen=`, and replies `OK seen=<id>` (emitting `EVENT \
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
        "Owner-only (refused to edge tokens; exempt from `hold`). `to=say[:<topic>]` broadcasts \
         on `say/<topic>` (`<topic>` in `[a-z0-9][a-z0-9._-]{0,31}`, default `say`) to sessions \
         with `topic add <topic>` (`topic=<t>`). \
         `kind=<ask|answer|task|report|note|ack|control>`. Options before `--`: `[re=<n>] \
         [dl=<ms>] [via=<p>] [key=<token>] [--wait[=<ms>]] [--wait-ack[=<ms>]] [len=<n>]` (inline \
         text <= 4 KiB, or `len=<n>` + raw bytes <= 256 KiB; no `to=fleet`). `--wait` (default on \
         for `ask`/`task`) waits for broker landing (`OK <id> off=<n>`, or `OK <id> off=<n> \
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
        "Bridge-only (stamps `from=` and `trust=`). Delivers an inbound bus record into `<sid>`'s \
         inbox (idempotent on `off=` over the last 1024 offsets; `ERR quota` past 64 unread rows \
         from one peer `<src>`; `ERR not-subscribed` for an unopted `topic=<t>` instead of \
         `dropped=`; `verdict=<handled|refused|deferred>` on `kind=ack`). Other forms: `deliver \
         <sid> landed=<post-id> off=<n>` (completes an outbound `post`), `deliver <sid> \
         fetched=<off> … [len=<true size>] [at=<n> more=1] [text=<pct>] | \
         err=<no-record|unreadable|observer|forged-self|via|malformed|oversize|refused|retained> \
         [floor=<n>]` (answers a parked `inbox get @<off>` listed as `fetch sid= off=`), and \
         `deliver <sid> receipt=<rid> off=<n|->` (retires an owed receipt).",
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
        "Bridge-only. Updates `status` `fabric=`, `fabric_rtt_ms=` and `fabric_link_age_ms=`: `up \
         rtt=<ms>` sets `fabric=connected`; `down \
         reason=<no-socket|refused|denied|no-ack|closed|cut-fleet|cut-inbox|cut-say|attach|expired|busy|record|protocol|subscribe|read|state-locked|endpoint-grammar>` \
         (where `expired` means cap `exp=` passed) sets `fabric=stalled` and wakes `post --wait` \
         with `ERR fabric stalled id=<n> queued=1`. `log <identity> [continues=<off> was=<id>]` \
         binds the broker log generation (forgetting stale `off=` dedup and offsets with `OK \
         forgot=1` on a log switch and failing parked fetches with `ERR fetch failed \
         reason=log-replaced`). `mirror on|stopped|capped|budget|unretained [reason=<token>]` \
         updates `fabric status` `mirror=` (`bridge_log=turns|auto` from `hello` \
         `log=turns|auto`). `link bye <quit|handoff>` acknowledges `EVENT * bye <word>` \
         (withdrawing presence with `state=exited` on `quit`). `set-aside files=<n>` and \
         `quarantine torn=<k>` report quarantined bridge state files (`fabric.state.torn`). \
         Replies `OK` (or `OK stale=1` from a superseded bridge lane).",
    )
    .input(Input::NONE),
    va(
        "hello",
        Write,
        Status,
        Meta,
        BridgeOnly,
        "hello lane=<n> [build=<token>]: bridge lane grammar and build handshake",
        "Bridge-only handshake recording `bridge_lane=` and `bridge_build=<version>+g<commit>` \
         for `fabric status` (alongside this instance's `build=`) and replying `OK lane=<n> \
         build=<b>` (or `OK stale=1` from a superseded lane; extra `key=value` tokens are \
         ignored).",
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
            "Replies `OK hold=<0|1>` (adding `journaled=0` if crash-journal disk sync exceeds 5 s \
             on `origin=local`). Four independent hold slots per session: `origin=local` \
             (set/lifted by Owner), `origin=fleet` (set/lifted by the fabric bridge; \
             `origin=fleet` from Owner is `ERR denied`), `origin=person` (`reason=taken-over`, \
             set/lifted only by the human's Take Over / Hand Back in the window; `invoke \
             TakeOver|HandBack` is `ERR denied`), and `origin=remote` (set/lifted over fleet \
             drive). While any slot stands, every PTY-reaching verb resolving to that session \
             answers `ERR halted reason=<r> origin=<person|remote|fleet|local>` from ANY scope — \
             `send key ctrl feed feed-bin paste paste-bin mouse pointer resize focus signal \
             restart reset turn close invoke hwkey pane tab confirm askpass` — and \
             session-independent window verbs (`invoke`, `hwkey`, `pane`, `pointer`, `tab`, \
             `confirm`, `askpass`, plus `open \
             menu|palette|tab-menu|autopilot-menu|fact-menu|overview|connections`) are refused \
             across the instance whenever any session is held: \"",
            hold_cost!(),
            "\" `spawn`, `survey`, `await`, `result`, `post`, `inbox seen`, `meta set`, `lease` \
             and all read verbs stay answerable. On bridge disconnect, bridge-governed sessions \
             show `status hold_advisory=fabric-lost` (or acquire `reason=fabric-lost \
             origin=fleet` under `[fabric] hold_on_bridge_lost = true`). Holds survive seamless \
             updates (`timeline` `carried=1`; `ERR busy update-committing` if a hold arrives \
             during final commit) and PTY-keeper crash restores."
        ),
    )
    .input(Input::NONE),
    va(
        "outbox",
        Read,
        Bytes,
        Meta,
        BridgeOnly,
        "outbox [<max>]: drain the queued outbound posts, bodies included",
        "Bridge-only peek (up to `<max>` posts / 4 MiB, retiring nothing until `outbox sent`). \
         Replies `OK <nbytes>` then `<nbytes>` raw bytes containing `post sid=<s> id=<n> to=<pct> \
         kind=<k> [re=<n>] [dl=<ms>] [via=<p,...>] [key=<token>] len=<n>` + `len` body bytes per \
         post, followed by bodiless `receipt sid=<s> rid=<n> off=<n> verdict=<v> kind=<k> \
         from=<p>` lines (retired via `deliver <sid> receipt=<rid>`) and `fetch sid=<s> off=<n>` \
         lines (answered via `deliver <sid> fetched=<off> …`).",
    ),
    va(
        "outbox sent",
        Write,
        Status,
        Meta,
        BridgeOnly,
        "outbox sent <sid> <id> off=<n|-> [dup=1]: retire one queued outbound post",
        "Bridge-only, idempotent. `off=<n>` records the broker offset (`dup=1` if deduped under \
         `key=`, waking `post --wait` with `OK <id> off=<n> dup=1`) and frees the retained body. \
         `off=- [reason=<word>]` retires the post as undeliverable (waking `post --wait` with \
         `ERR <reason> id=<n>`).",
    )
    .input(Input::NONE),
    va(
        "log-take",
        Read,
        Bytes,
        Meta,
        BridgeOnly,
        "log-take [n=<1..64>] [tier=wire|meta] [key=<hex>]: the bus mirror's next slots",
        "Bridge-only (requires `hello` `log=turns`; `ERR mirror off` otherwise). Replies `OK \
         <nbytes> slots=<k>[ more=1]` then `<sid> <ep>:<seq>:<ms> <v=2 body>` lines (bus-class \
         records, `op=gap why=evicted|reset|dropped n=<k>` from `ms=`, or `op=snap`; `tier=meta` \
         adds program/model words and hashes edge sids as `by=e<8 hex>` using `key=`). Advances \
         no cursor until `log-landed`. Replies `OK 0 slots=0 stale=1` to a superseded lane, or \
         `ERR busy adopting=1` during update adoption.",
    ),
    va(
        "log-landed",
        Write,
        Status,
        Meta,
        BridgeOnly,
        "log-landed <sid> <ep>:<seq>:<ms>...: the bus mirror's slots landed",
        "Bridge-only (requires `hello` `log=turns`; `ERR mirror off` otherwise). Advances each \
         session's bus-mirror cursor and replies `OK <word> ...` \
         (`ok|stale|done|fenced|adopting|gone|ahead` per pair, or `OK stale=1` to a superseded \
         lane).",
    )
    .input(Input::NONE),
    va(
        "fabric",
        Owner,
        Status,
        Meta,
        OwnerOnly,
        "fabric status|attach [<command...>]|retire <sid>...: the bridge of a RUNNING instance",
        "Owner-only. `fabric status` replies `OK \
         state=<absent|connected|stale|stalled|disconnected> supervised=<0|1> command=<pct|-> \
         reason=<token|-> rtt_ms=<n|-> link_age_ms=<n|-> build=<token> bridge_lane=<n|-> \
         bridge_build=<token|-> lane_faults=<n>` (plus `argv=`, and mirrors `status` `fabric=` / \
         `no-bridge=1` / `queued=1`). `fabric attach [<command...>]` arms the bridge supervisor \
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
        "Owner-only roster (relayed by `ls` and folded by `windows`). Columns per session: \
         `nonce=<hex32>` (fabric `epoch=`), `window=<id|none|->` (`window=0 geometry=headless` on \
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
        "Owner-only. Lists `nonce=<hex32>`, `fgpgid=<pid|->`, and \
         `driving=<turn-id|lease:<holder>|->` without a main-thread window-placement hop.",
    ),
    va(
        "survey",
        Read,
        Lines,
        Meta,
        OwnerOnly,
        "survey [facts|content]: every session's program, state, task, prompt, last words, tool \
         in flight",
        "Owner-only and fleet-readable (`[fleet] allow`). Replies `OK <n>`, an `instance pid= \
         host=<pct> version= sessions=<n> level=<facts|content> cut=<n> at_ms=<unix ms> node=<32 \
         hex|-> instances=<n>` header, and one `session` row per session: `local= sid= state= \
         program=<pct|-> program_kind=<agent|shell|program|-> agent=<word|-> agent_detail=<pct|-> \
         agent_ms=<ms> human_ms=<ms|-> bg=<n|-> task=<state|-> task_rev=<n> task_from=<pct|-> \
         task_ms=<ms|-> model=<pct|-> effort=<pct|-> ctx_left_pct=<n|-> detail=<pct|-> \
         doing=<pct|-> role=<pct|-> supervisor=<pct|-> attention=<1|-> needs=<word|-> \
         hold_by=<word|-> held_from=<32 hex|-> typed_by=<pct|-> typed_ms=<ms|-> bells=<n> \
         bells_sounded=<n> last_bell=<ms>/<sound>/<program|->[,…]|->`, plus under `content` (the \
         local default; `needs=ask` selects the ask excerpt and `by=harness` turns are skipped in \
         `prompt=`): `user_title= title= cwd= branch= prompt= said= cmd= rows=` (<= 400 B per \
         excerpt, credentials/fleet codes redacted, fleet replies capped at 256 KiB; `rows=-` on \
         an unfinished DEC 2026 frame, never waited for). `aterm ctl survey [--json] [--budget \
         <chars>] [--sid <s>] [--fleet | --host <h>] [--timeout <s>]` and `survey --brief \
         [--budget <chars>]` (`log n` + `brief all`) format multi-instance and fleet digests.",
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
         reason=<shell-exit|ctl-close|ui-close|window-close|app-quit|unknown> exit_code=<n|-> \
         by=<sid|human|ctl|bridge|->` line per exited session (in-memory ring; also pushed live \
         via `subscribe @<sid> events` `closing reason= by=` and `subscribe … sessions` \
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
        "`family [<sid>]` -> `OK <n>` then `self <sid> <state> <title>`, `parent` (`parent - - -` \
         for a root), `child` rows, and (for Owner) `pushes`/`pushed-by`/`pulls`/`pulled-by` peer \
         rows.",
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
        "mint a cross-session edge (Owner only)",
        "`grant <src-sid> <op>` mints an edge onto this session and replies `OK <token-hex>`; \
         `<op>` is `read-screen|write-input|signal|config-write|clipboard-write|derive-loop` \
         (`ERR reserved` if `<src-sid>` is a session-log actor word).",
    )
    .input(Input::NONE),
    va(
        "revoke",
        Owner,
        Status,
        Meta,
        OwnerOnly,
        "revoke a cross-session edge: revoke <edge-hex> removes one;",
        "revoke src=<sid> sweeps every edge from that source and replies OK <removed> (Owner only)",
    )
    .input(Input::NONE),
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
    )
    .input(Input::NONE),
    va(
        "disconnect",
        Owner,
        Status,
        Meta,
        OwnerOnly,
        "disconnect dst=<sid> src=<sid> [kind=pull|push|both]: dissolve the session connection \
         src->dst",
        "(kind-filtered ok: kind=pull revokes only the pull half), reply OK <removed> (Owner only)",
    )
    .input(Input::NONE),
    va(
        "flows",
        Owner,
        Lines,
        Meta,
        OwnerOnly,
        "the instance's session-connection graph: OK <n> + `<src> <dst> <op>` per live edge",
        "across EVERY session's table (--json \
         groups per pair: {\"flows\":[{src,dst,ops:[..]}]}). Owner-only",
    )
    .input(Input::NONE),
    va(
        "raise",
        Owner,
        Status,
        Meta,
        OwnerOnly,
        "raise <sid>: raise the window hosting that session and select its tab",
        "(the session-connection Show twin; Owner only)",
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

/// THE ONE RESOLUTION of a request's verb facts: `verb` and `rest`, the text after it (no selector).
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

/// The bridge lane's grammar generation, which the bridge and the instance serving it each state in `hello` (`lane=`) so either can tell which side reads less of the lane: the bridge binary is whatever `[fabric] command` names, not necessarily the instance's own build.
pub const BRIDGE_LANE: u32 = 6;

/// The lane that added `link quarantine torn=<k>` (5 in [`BRIDGE_LANE`]'s list), for both sides: a bridge says it only to an instance at this lane or later, and an instance reads off a bridge's `hello lane=` whether that bridge will ever say it.
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

/// The forward's host went away: the line an aterm forwarding a connection to the aterm hosting its session writes, instead of a bare end of stream, when that host hung up first and no longer serves at `sock` (an update's Commit `_exit`, a crash, a quit).
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

/// Maximum UTF-8 bytes in one control-protocol reply line: the client enforces it on every line, and producers of large single-line payloads stay under it.
pub const MAX_CONTROL_REPLY_LINE_BYTES: usize = 8 << 20;

/// The screen's generation, `(invalidation epoch, content seq)`, spelled `<epoch>.<seq>` on the wire: `status gen=`, `text --json`'s `"gen"`, `agent_gen=`, and the `if-gen=` fence a typed act names.
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

/// The rows fence, `turn if-rows=<first>-<last>:<hex16>`: type only while the visible rows `first..=last` (0-based) still hash to `hash`, checked where `if-gen=` is.
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
    /// Parse the wire spelling `<first>-<last>:<hex16>` (two unsigned decimals with `first <= last`, exactly sixteen hex digits); any other shape is `None`.
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

/// Whether a successful reply for this request may carry exact capture/video retention: [`spec_for`]'s acknowledgement for the text after the verb, so the shipping client and the server share one classification.
#[inline]
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn artifact_reply_requires_ack(verb: &str, request: &str) -> bool {
    spec_for(verb, head(request).rest).is_some_and(|r| r.ack)
}

/// The reply framing of `verb` for the full `request` line: [`spec_for`]'s framing for the text after the verb, so a sub-form (`image read`, `cast drift`, `inbox get`) and a `--json` on a JSON-emitting verb frame as the table says.
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

    /// The rows fence's wire spelling round-trips, refuses other shapes, and hashes the rows
    /// as the screen's `hash=` does.
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

    /// A selector body names something only when it is a non-empty run with no
    /// whitespace or control character: the bare `@` of an empty `$W`, and `@`
    /// followed by blanks, name nothing; every form the help teaches does.
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

    /// The wire spelling round-trips, and every other shape (a `u64` overflow included) is
    /// refused.
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

    /// Under `--json`/`json` every json-capable verb frames as Lines, so the client keeps the
    /// JSON body.
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
        assert_eq!(framing_of("timeline", "@s-a timeline 10 since=3"), Lines);
        assert_eq!(framing_of("resizes", "resizes"), Lines);
        assert_eq!(framing_of("resizes", "@s-a resizes 20 since=4"), Lines);
        assert_eq!(framing_of("trail", "trail"), Lines);
        assert_eq!(framing_of("trail", "@s-a trail 5"), Lines);
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

    /// `entry_lines` wraps every verb within the width (bar a lone long word), indents
    /// continuations to the text column, and re-joins to `help_line` exactly.
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

    /// The fabric rows carry the classes the fabric design's §11.2 gives them; a silent change
    /// to one is a protocol change.
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

    /// `copy` (clipboard exfil) and `settings` (durable config) each have their own op-class,
    /// out of a read-only or keystroke-only edge's reach.
    #[test]
    fn copy_and_settings_have_their_own_op_class() {
        assert_eq!(spec("copy").unwrap().op, OpClass::ClipboardWrite);
        assert_eq!(spec("settings").unwrap().op, OpClass::ConfigWrite);
        assert_eq!(spec("scroll").unwrap().op, OpClass::Read);
        assert_eq!(spec("select").unwrap().op, OpClass::Read);
    }

    /// The `privacy` row's shape is load-bearing: `AnyScopeMeta` so a `--headless` instance
    /// answers it, `Meta` to reject a selector, `Read` because each fact is one `status` already
    /// exposes or one the user's own processes can read, `Lines` for `OK <n>` + rows.
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

    /// The `theme` rows carry the class each sub-form needs (the read forms
    /// Read, the runtime preview lane Write, the forms that rewrite `aterm.toml`
    /// ConfigWrite), and `theme list` and `theme palette` alone flip to Lines
    /// framing.
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

    /// [`spec_for`] reads the longest name a request spells, then the first form of
    /// that row matching what follows: a listed sub-form row wins over its verb's
    /// forms, a word no form names falls to the catch-all, and an `@` word, an
    /// argument or a sub-word moves the lane and authority exactly where the table
    /// says — and nowhere else.
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
        assert_eq!(r("meta", "unset icon").op, Write);
        assert_eq!(r("meta", "").op, Read);
        assert_eq!(r("log", "take").row.name, "log");
        assert!(spec_for("bogus", "").is_none());
    }

    /// THE SHIPPED `help hold` TEXT ENUMERATES THE HALT SET, SO IT IS PINNED TO
    /// THE HALT SET: the rows whose [`Input`] class has [`Input::PTY`].
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
