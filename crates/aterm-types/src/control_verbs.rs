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

/// The longest control request line the server reads, its newline not counted
/// (a frame's body is bounded on its own). A longer line is answered `ERR`
/// [`request_line_too_long`] and the connection closed; the client refuses one
/// before sending it. One constant for both ends.
pub const MAX_REQUEST_LINE: usize = 64 * 1024;

/// The longest task a WORKER is given (`spawn agent=claude … len=<n>`, its
/// task the n raw bytes after the line): the server refuses a longer frame
/// unread and closes, and the client refuses one before sending it. One
/// constant for both ends.
pub const MAX_TASK_BYTES: usize = 64 * 1024;

/// The longest `timeout=` a `turn` over another Mac's drive takes (the
/// step-in): the driven Mac cuts a drive connection at 30 s, the dial and
/// the reply inside it. `aterm ctl --host` refuses a longer one before it
/// dials, and the driven Mac's drive gate refuses one that comes anyway — a
/// turn that names none, whose own default is minutes, included. One
/// constant for both ends.
pub const DRIVE_TURN_MAX_MS: u64 = 22_000;

/// The line a server answers a worker's spawn head (`spawn … agent=claude
/// len=<n>`) with when it will read the n bytes of task that follow, its
/// newline included. The client sends the task only after this line: a server
/// that does not know the frame answers the head with an `ERR` and never sees
/// a byte of the task, which it would otherwise serve, line by line, as
/// requests. Neither `OK` nor `ERR`, so no server says it to anything else.
#[must_use]
pub fn task_frame_go(len: usize) -> String {
    format!("GO {len}\n")
}

/// Why a request line past [`MAX_REQUEST_LINE`] is refused, in the server's
/// `ERR` line and in the client's refusal before it sends one.
#[must_use]
pub fn request_line_too_long() -> String {
    format!("request line too long (max {MAX_REQUEST_LINE} bytes)")
}

/// Why a selector that names nothing is refused, in the server's `ERR` line and
/// in the client's refusal before it sends one: a bare `@` (what `@$W` becomes
/// when `$W` is empty) or an `@` whose body is blank
/// ([`selector_body_names_something`]). It is never read as the active session
/// (`@.` says that), so a caller whose sid went missing is told so instead of
/// typing into, or stopping, the person's focused tab.
pub const EMPTY_SELECTOR: &str = "ERR usage: empty or malformed selector: an @ names a session \
     (@self, @<sid>, @<n>; @. is the active tab)";

/// Whether a selector's body (what follows its `@`) names something: it is not
/// empty and holds no whitespace or control character. One rule for both ends
/// ([`EMPTY_SELECTOR`]).
#[must_use]
pub fn selector_body_names_something(body: &str) -> bool {
    !body.is_empty() && !body.chars().any(|c| c.is_whitespace() || c.is_control())
}

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
    /// to the instance (window/chrome/controls/open/settings/invoke/…). `tab`,
    /// `spawn`, `panes`, `theme` and `edit` are the exception: an explicit
    /// `@<sid>` aims `tab` and `spawn` at the window HOSTING that session
    /// (`cmd_tab_aimed` / `cmd_spawn(.., Some(session))`), `panes` at the window
    /// whose active tab shows it (`cmd_panes`), `theme`'s session forms (`theme
    /// session …`, `theme palette session`) at that session, and `edit` opens its
    /// view beside that session's tab (`cmd_edit`).
    App,
    /// Self-scoped / fleet-wide; a selector is rejected (sessions/who/grant/version/…).
    Meta,
}

/// Which of the control server's lanes a request costs (`aterm-gui`'s
/// `control_lanes.rs`). It decides only whose time the request takes: a waiting
/// request on an RPC lane holds one of the few lanes every quick request shares.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lane {
    /// Answers at once: an RPC lane.
    Rpc,
    /// Mostly waits — for an event, a settle, the owner, the bridge — or hands the
    /// connection to a relay: a wait lane.
    Wait,
    /// A wait lane when an explicit `@<sid>` other than `@self` aims it, else an
    /// RPC lane: a `paste` into a session not on screen waits, bounded, for its
    /// bytes to reach the program, and a flagless one goes to the front tab.
    WaitWhenAimed,
}

/// What a verb does with a `--json`/`json` token in its request.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Json {
    /// The token is an ordinary argument: a payload verb (`send --json`, `search
    /// json`, `meta set title json`) receives it verbatim.
    None,
    /// The verb has no structured form, and its grammar never carries the token as
    /// data, so the server answers `ERR json: not supported for <verb>`.
    Refused,
    /// The server answers JSON — most wrap it in `json_ok` (`OK 1` and one object),
    /// `log` answers one object per record — so under the token the reply frames as
    /// [`Framing::Lines`] whatever the row's own framing.
    Emits,
}

/// What follows a request's line on the stream before it is answered. The server
/// reads the body a line announces before it serves the line, and a lane serves a
/// request only once all of it has come.
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

/// What a verb's request does to a session's input, as the server's input gates
/// ask it: a set of the flags below. Every gate reads its flag here, so the fleet
/// halt, the unread-input gate, the self-feed floor, the drive lease, the
/// idempotency key, the guard, the front-surface fence, the `seq=` stamp, the
/// cursor-licence fence and the App lane's `unread=ok` agree on which verbs they
/// ask by construction. A row that can mutate declares its class, [`Input::NONE`]
/// included, with the argument for it at the row, so a new writing verb is
/// classified before it compiles (the checks after [`VERBS`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Input(u16);

impl Input {
    /// No input gate asks the verb.
    pub const NONE: Self = Self(0);
    /// A fleet halt refuses it (design §5.3): it can reach a session's PTY, signal
    /// its child, retire a live session, or re-aim the seam that writes. Exempt on
    /// purpose: `post`, `inbox seen`, `meta set`, `lease` and every read, because a
    /// halted agent must still be able to ask why it is halted, mark the notice
    /// seen and escalate. The physical keyboard is on no socket seam at all, so a
    /// human at the glass keeps typing: a halt stops drivers.
    pub const PTY: Self = Self(1);
    /// Writes input bytes to a session's PTY, so the unread-input gate asks it. The
    /// halt's verbs that reach a PTY without writing input (a winsize, a signal, a
    /// hangup, which session the keyboard drives) are the remedies for a program
    /// that is not reading, and stay answerable while that gate refuses.
    pub const WRITES: Self = Self(1 << 1);
    /// Self-targeted, it passes the per-session self-feed floor, so a raw client
    /// cannot drive an output→observe→write storm on its own session.
    pub const FLOORED: Self = Self(1 << 2);
    /// Refused while a `turn` or another connection's hard drive lease holds the
    /// session, so raw writes cannot corrupt a turn's output.
    pub const LEASED: Self = Self(1 << 3);
    /// Takes a leading `id=<epoch>:<producer>:<seq>` idempotency key (§11.2's set):
    /// every keyed verb can put bytes on the PTY, and no other verb may claim a
    /// mark. A leading `id=` on any other verb is its content.
    pub const KEYED: Self = Self(1 << 4);
    /// Takes a leading `if=<re>` guard, the `if-gen=`/`if-fp=`/`if-human=` fences
    /// and `unread=ok`: the keystroke-shaped writes a driver answers a prompt with.
    pub const GUARDED: Self = Self(1 << 5);
    /// Can drive a modal overlay or native Settings tab in front through the App
    /// input seam (or races a human in one), so a scoped caller's flagless request
    /// is checked against the front surface first.
    pub const FRONT_DRIVING: Self = Self(1 << 6);
    /// A successful reply carries ` seq=<content_seq>`, the baseline `await seq`
    /// waits past.
    pub const STAMPS_SEQ: Self = Self(1 << 7);
    /// An authorized attempt, malformed or not, is a newer input boundary for the
    /// cursor's move licence: it can inject bytes, signal the child, or move the
    /// cursor's coordinate space.
    pub const FENCES: Self = Self(1 << 8);
    /// An App-lane gesture that takes a leading `unread=ok`, lifting the
    /// unread-input gate for itself.
    pub const APP_UNREAD: Self = Self(1 << 9);
    /// Its argument is the program's own bytes (`send`, `feed`, a frame's body),
    /// not a gesture: a self-targeted request is delivered pinned to the session
    /// it was authorized against, never to an overlay or another tab that came to
    /// the front, and a background write stamps the input-arrival instant as a
    /// raw write does. A gesture (`key`, `paste`, `mouse`, …) from the Owner's
    /// flagless request follows the front.
    pub const PINNED: Self = Self(1 << 10);
    /// Takes a leading `if-supervisor=<holder>` fence: the verbs a supervisor types into
    /// its session with. On `paste`, whose body is raw, it is the one leading option.
    pub const SUPERVISED: Self = Self(1 << 11);
    /// Takes a leading `cause=@<off>|<seq>` naming the mail or session-log record the
    /// input acts on, checked against what the session holds before a byte is written.
    pub const CAUSE: Self = Self(1 << 12);

    /// These flags and `other`'s.
    #[must_use]
    pub const fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Whether every flag of `flags` is set.
    #[must_use]
    pub const fn contains(self, flags: Self) -> bool {
        self.0 & flags.0 == flags.0
    }
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
    /// The lane the request costs, unless one of its forms says otherwise.
    pub lane: Lane,
    /// Whether a successful reply retains an exact artifact (a capture's file, a
    /// recording's frames) the client must acknowledge after consuming it
    /// ([`ARTIFACT_REPLY_CHALLENGE_PREFIX`]), unless a form says otherwise.
    pub ack: bool,
    /// What the verb does with a `--json`/`json` token.
    pub json: Json,
    /// The verb's sub-forms, in match order: the first whose [`When`] matches the
    /// request decides ([`spec_for`]). Not catalog rows — a form's help is its row's.
    pub forms: &'static [Form],
    /// What follows the request's line on the stream.
    pub body: Body,
    /// What the request does to a session's input ([`Input`]); `None` only on a read
    /// row that declares nothing ([`VerbSpec::input_class`] reads it as
    /// [`Input::NONE`]). A listed sub-form row has its verb's class, since the gates
    /// ask by verb.
    pub input: Option<Input>,
    /// Whether a FLEET peer may run the verb: a connection the fleet listener
    /// authenticated (`[fleet]`, `docs/DESIGN-sessions-as-subagents-2026-10-15.md`
    /// §5), served under the server's `Scope::Fleet`. The server's gate, at the top of
    /// its request path, refuses such a connection every verb whose row leaves this
    /// unset, every `@` selector and every frame, and closes it — but for a drive
    /// connection (§5.1, the step-in: a node `aterm fleet allow drive --from` named,
    /// proved by its node key), whose drive gate first runs its own allowlist of worker
    /// lines (`@<sid>` lines on a hosted session, `hold`, `spawn`'s task frame, `await
    /// task`) as the Owner's, whatever this field says. Unset by the row
    /// constructors, so a new verb is refused to the fleet until its row is marked
    /// ([`VerbSpec::for_fleet`]): fail-closed for every verb this table will grow.
    pub fleet: bool,
}

impl VerbSpec {
    /// The row's [`Input`] class: what it declares, else [`Input::NONE`].
    #[must_use]
    pub const fn input_class(&self) -> Input {
        match self.input {
            Some(input) => input,
            None => Input::NONE,
        }
    }
}

/// What a [`Form`] matches in a request's TAIL: the text after the verb, and after
/// the sub-word too when the request names a listed sub-form row (`inbox get`).
/// Words are split on whitespace.
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
    /// There is any word at all. As a row's last form, it catches every word the
    /// forms before it did not name, so an unknown word fails closed.
    Args,
}

/// One sub-form of a verb: the [`When`] that picks it out of a request, and each
/// fact in which it differs from its row (`None`: the row's).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Form {
    /// What the form matches in the request's tail.
    pub when: When,
    /// The authority class the form needs instead of its row's.
    pub op: Option<OpClass>,
    /// Only an owner-class connection may run the form, whatever its row's
    /// [`Access`]: no fine op expresses what it reaches.
    pub owner_only: bool,
    /// How the form's reply is framed instead of its row's.
    pub framing: Option<Framing>,
    /// The lane the form costs instead of its row's.
    pub lane: Option<Lane>,
    /// Whether the form's reply needs the artifact acknowledgement, instead of its
    /// row's.
    pub ack: Option<bool>,
}

/// [`HOLD_COST`] as a literal, so the `hold` entry and the window's Take Over
/// tool tip quote it through `concat!`: one sentence, never two that drift.
#[macro_export]
macro_rules! hold_cost {
    () => {
        "While any tab is held, agents on this aterm cannot use invoke, tab, pane, hwkey, \
         pointer or confirm, or open the palette, a tab menu, the Autopilot menu or Overview, \
         and a sudo with no terminal cannot ask for its password in any tab."
    };
}
/// WHAT A HOLD COSTS THE WHOLE INSTANCE, in the one sentence every surface that
/// states it uses (`docs/DESIGN-owner-surfaces-2026-10-16.md` §4): Take Over's
/// tool tip, the menu bar's You hold section and the `hold` entry. While any
/// session is held, the instance-wide halt refuses the verbs that resolve no
/// session and sudo's password relay (`askpass`), and the cards whose rows are
/// acts do not open.
pub const HOLD_COST: &str = hold_cost!();

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
/// [close]` opens and closes; `gestures` the one only a gesture opens (it
/// needs an intent the wire cannot give). `window` photographs and
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
        /// The Overview (`docs/DESIGN-owner-surfaces-2026-10-16.md` §6, was
        /// the Connection Map); `open overview` raises it on the front window,
        /// `controls overview` reads the open card's groups, chips and manages
        /// lines, and every wire read of it is Owner-gated. `connections`, the
        /// map's keyword, still names it.
        Overview = "overview" | "connections",
        /// The native Settings `/updates` route, projected from its compiled
        /// semantic frame while that route is open.
        Update = "update" | "software-update";
    gestures:
        /// The session picker (`invoke NewTabWithIdentity`); `controls
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

// The rows' authority facts agree with one another, checked when this crate
// compiles: a request is routed on its verb's row before its sub-form is known, so
// a listed sub-form row (`inbox get`) shares its verb's access and target; an
// `Owner` class has no fine op, so only a scope gate can hold it; a `Scoped` verb is
// checked per target, so it has one; and a JSON emitter only reads.
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

// The rows' input classes ([`Input`]) agree with their ops and with one another,
// checked when this crate compiles. A row that can mutate declares its class, so a
// new writing verb is put inside the halt or argued outside it at its row before it
// ships. A read reaches no PTY and writes no input. The unread-input gate asks only
// verbs the halt refuses, and every other input gate, like the pinned delivery,
// only verbs that write input. The App lane's `unread=ok` is an App verb's, inside
// the halt. A listed sub-form row has its verb's class, since the gates ask by verb.
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

/// The budget for the short catalog's rows put together (every [`catalog_lines`] row,
/// `\n`-terminated): what discovering a verb costs, whatever the table grows to.
///
/// The bound is on the SHAPE (one row per verb, first sentence only, capped at
/// `summary_max_chars!()`, 96), so a new verb raises it by about its own row; never reword
/// unrelated verbs' help to make room. Checked at compile time with the summary cap.
pub const SHORT_CATALOG_MAX_BYTES: usize = 13171;

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
    /// This row, served to a fleet peer too (the `fleet` field).
    #[must_use]
    pub const fn for_fleet(mut self) -> Self {
        self.fleet = true;
        self
    }

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

use crate::control_request::head;
use Access::{AnyScopeMeta, BridgeOnly, OwnerOnly, Scoped};
use Framing::{Bytes, Lines, Push, Status};
use Lane::{Rpc, Wait, WaitWhenAimed};
use OpClass::{ClipboardWrite, ConfigWrite, Owner, Read, Signal, Write};
use Target::{App, Meta, Session};
use When::{AnyOf, Args, AtWord, Flag, Word};

/// A `const`-friendly [`VerbSpec`] constructor for a normally-`Scoped` verb (keeps
/// the table terse; the vast majority of rows). A row's other facts default to an
/// RPC lane, no acknowledgement, no JSON form and no sub-forms; the chained setters
/// below state the exceptions.
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
    // Skip: iterator absent std bodies.
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
        lane: Rpc,
        ack: false,
        json: Json::None,
        forms: &[],
        body: Body::None,
        input: None,
        fleet: false,
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
         prints a rustc-shaped release for cargo's sake, and `trust=` is where it says its own. \
         A FLEET peer (`[fleet]`) may run it: a fleet read runs only the verbs whose row is \
         marked `fleet` (`help --json`), and any other verb, an `@` selector or a frame is \
         answered `ERR denied: …` and the connection closed. At `[fleet] allow = \"drive\"`, \
         a connection from a node `aterm fleet allow drive --from` named, proved by that \
         node's own key, also runs the worker's verbs as the Owner's: `@<sid> \
         status|text|screen|result|turn|key|send` on a session this instance hosts, `hold \
         <sid> on|off`, `spawn agent=claude` with its task frame, `await task` and `@<sid> \
         close` of a worker it spawned (`aterm help fleet`, DRIVE).",
    )
    // Outside the halt: a build-provenance string.
    .input(Input::NONE)
    .for_fleet(),
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
         pct-encoded. apply_posture: automatic, automatic-idle, waits-terminal (a window \
         that kept the image it launched with and has no terminal open: it installs once a \
         terminal tab opens), manual-config, disabled-env, retry-wait, manual-only, \
         handoff-unavailable, applying, unreconciled, none, disabled, unknown. apply_phase: prefer-idle, prefer-output-gap, keys-only, land \
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
    )
    // `check` stages a build and `apply` hands off or replaces the bundle on disk:
    // owner-only like `invoke ApplyUpdate`, while bare `update`/`update status`
    // is a read for any scope. `check` waits on the network for tens of seconds.
    .forms(&[
        form(Word("check")).owner_only().lane(Wait),
        form(Word("apply")).owner_only(),
    ])
    // Outside the halt: `status` and `check` are reads; `apply` re-execs the instance,
    // ending every session and every in-memory hold with it (a residual), and is Owner-only,
    // answered before any session resolves, with a build already staged on disk.
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
    // Outside the halt: the catalog; a halted agent must be able to read it.
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
    // Outside the halt: the `help` alias.
    .input(Input::NONE),
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
         then asks for an administrator password in its own dialog. Once per Mac, a \
         hardened aterm also raises that alert by itself, at a quiet moment after it starts, to \
         ask whether to allow it; nothing a session sends raises or answers it, and \
         `[privacy] notice = false` keeps it from coming up. Running \
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
         the switch, with no lock on tccd's file; neither raises a dialog or writes anything. \
         `unlisted` is tccd's `Unknown (None)`, its \
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
    )
    .json(Json::Emits),
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
         `order=display`'s \"order\" and \"display_map\", and, beside a null \
         human_ms only, \"unkeyed_ms\":<ms>, which end the object (how long the host has \
         served the session with no person's input, a seamless update's carried time \
         included; an adopted shell nothing was carried for counts from the successor's \
         Commit)",
    )
    .json(Json::Emits),
    v(
        "screen",
        Read,
        Lines,
        Session,
        "screen [--json] [since=<etag>]: the full styled grid as one lossless JSON frame.",
        "The frame ends with \"etag\":\"<16 hex>\", a fingerprint of everything it is a \
         function of (the screen generation, every byte of output fed, the dimensions and \
         line sizes, the cursor, colours, palette and selection, and the style policy a \
         config reload sets — bold as bright, faint opacity). The frame is the live screen, \
         so scrolling the viewport back through history moves neither the frame nor the \
         etag (`image` shows the viewport). `since=<etag>` sends that fingerprint back: while \
         none of it has moved the reply is `OK 1` then \
         {\"unchanged\":true,\"seq\":<n>,\"gen\":\"<epoch>.<seq>\",\"etag\":\"<etag>\"} \
         instead of the frame, and any other etag gets the full frame — a poll of a settled \
         screen costs no grid read. The frame is one line within the reply-line cap \
         (MAX_CONTROL_REPLY_LINE_BYTES, 8 MiB): past it the images are `truncated` (their \
         real `nbytes` kept; `image read` serves each), and a grid too big even so (about \
         35,000 cells) answers `ERR screen: frame too large (<n> bytes > <cap>)`. Any other \
         argument is `ERR usage: screen [--json] [since=<etag>]`",
    )
    .json(Json::Emits),
    v(
        "line",
        Read,
        Status,
        Session,
        "line <n>: one physical line of scrollback+screen",
        "n is an absolute row in the same coordinates as search. ERR evicted means the row \
         is older than retained history; ERR unreadable means a retained row could not be \
         read; ERR out of range means the row is beyond the live screen's end.",
    )
    .json(Json::Refused),
    v(
        "lines",
        Read,
        Status,
        Session,
        // The optional field rides in the detail: the full row reads exactly as
        // one string would, and the short catalog (at its budget) stays as it was.
        "OK <scrollback-line-count>",
        "[truncated=<lost>]",
    )
    .json(Json::Refused),
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
    )
    .json(Json::Refused),
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
         OpenConsole (aterm.toml `conpty`) the console host is made to leave the dead screen \
         first and the tab leaves on the switch it forwards, so the two stay in step; \
         `ERR timeout` means the host did not leave within 2 s and the tab is still on the \
         dead screen (it leaves with the host's switch if that comes later). Under \
         Windows' own console host the console stays on the dead app's screen, and nothing a \
         terminal sends moves it: the primary rows are all in the scrollback, the screen starts blank \
         with the cursor where the console has it, and the next full-screen program's exit \
         shows the screen as it was when the killed app started, every row since kept in the \
         scrollback just above it. A console program that writes `ESC[?1049l` brings the \
         console back at once (pwsh: `[Console]::Write(\"$([char]27)[?1049l\")`). Anything \
         after the verb is `ERR usage: mainscreen`",
    )
    // Outside the halt: leaves an alternate screen a killed app left up: it puts no bytes on
    // a PTY, signals nothing and retires no session, and the modes it releases (mouse
    // reporting, application cursor keys and keypad) send the program less, never more.
    // Aimed at a live full-screen app it misplaces that app's frame, which is display, not
    // driving.
    .input(Input::NONE)
    // Under OpenConsole it waits, up to 2 s, for the console host to forward its leave of
    // a dead app's screen (`alt_screen_resync`).
    .lane(Wait),
    v(
        "cell",
        Read,
        Status,
        Session,
        "cell <r> <c>: OK <grapheme%enc> <fg> <bg> <attrs>[ link=]",
        "",
    )
    .json(Json::Refused),
    v(
        "cursor",
        Read,
        Status,
        Session,
        "OK <row> <col> <visible> <style>",
        "",
    )
    .json(Json::Emits),
    v(
        "dims",
        Read,
        Status,
        Session,
        "OK <rows> <cols> <px_w> <px_h>",
        "then keyed facts, read by key: session= cell_w= cell_h= font_px= scale= \
         window=<id|none> (the hosting window) window_rows= window_cols=, frame/surface sizes, \
         bands, crops, geometry= and present_retry_*; --json carries the same",
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
        "- the live default colours, then what programs set over the theme: `palette_sets=allowed|\
         refused` (the session's OSC 4/21 gate), `refused=1` once it refused a palette set, \
         `program_palette=<idx>:<rrggbb>,…` (ascending; `-` for none) and `program_slots=` from \
         fg,bg,cursor,selection_bg,selection_fg (`-` for none). A program's colours live for the \
         session; `reset` puts them back.",
    )
    .json(Json::Refused),
    // The live theme. The bare form and `theme list` READ; the transient preview
    // lane (`preview`/`revert`) is runtime-only like `rain`, so it is Write; the
    // forms that rewrite `aterm.toml` (`set`, `commit`) are ConfigWrite like
    // `settings set`. The forms raise the class by the sub-form word the parser
    // reads, so no spelling of a write form passes as a read.
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
    )
    .forms(&[
        // `OK <n>` + n theme rows (`list`) or palette slots (`palette`, the instance's or
        // the aimed session's); every other form answers one status line.
        form(Word("list")).framing(Lines),
        form(Word("palette")).framing(Lines),
        form(Word("status")),
        form(Word("revert")).op(Write),
        // `commit`, and any word no form names: fail closed to the durable write.
        form(Args).op(ConfigWrite),
    ]),
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
    )
    // Outside the halt: draws another theme's colours for a while (`revert` ends it): a
    // look, the class of `rain`. It puts no bytes on a PTY, and a program's own colour sets
    // survive it.
    .input(Input::NONE),
    // One session's own theme (runtime only, carried by an update and a restore). Its
    // row is the verb's App row, since a request routes on the verb's row before its
    // sub-form is known; `theme` is aimed like `tab` and `spawn`, so `@<sid>` names the
    // session. The bare form and `status` read; `set` and `clear` change what the session
    // draws, Write like a preview.
    v(
        "theme session",
        Read,
        Status,
        App,
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
    )
    .forms(&[
        form(Word("status")),
        // `set`, `clear`, and any word no form names: the session's colours change.
        form(Args).op(Write),
    ]),
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
    )
    // Outside the halt: writes `theme` to `aterm.toml` through the `settings set` lane,
    // `settings`' exemption: no bytes on a PTY, no session retired.
    .input(Input::NONE),
    v(
        "search",
        Read,
        Lines,
        Session,
        "search <pat> [case] [regex]: find in history; OK <n>[ incomplete…], \"<row> <col> <len>\" each",
        "(searches the newest search_history_lines lines, default 100000; incomplete is \
         followed by every cause that applies: capped: the list stopped at 100000 matches; \
         rewrapping: a resize holds the history away, only the screen was searched; \
         long-line: a very long line was not searched to its end (a regex ran out of scan \
         budget on it, or it is a history line past the 1 MiB search reads of a row); moving: \
         the screen switched, resized or renumbered its rows during the search (output alone \
         does not), so a row may name another line now; search again; older: older history \
         was not searched either. incomplete \
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
    )
    .json(Json::Refused),
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
    )
    .json(Json::Refused)
    // Outside the halt: moves the SELECTION to the OS clipboard and types nothing. It is
    // half of the screen-to-PTY chain, and the halt cuts that chain at the other half,
    // `invoke Paste`.
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
        "trim drops the trailing all-blank rows (`OK <n> trimmed=<k>`, n = the rows sent); any \
         other tail is `ERR usage: blocktext <id> [trim]`",
    )
    .json(Json::Refused),
    // seeing: pixels, recording, history
    v(
        "image",
        Read,
        Status,
        Session,
        "image [path] : capture the APPLICATION-RENDERED CLIENT FRAME -> PNG, reply OK <w> <h> \
         <path>",
        "(bare filename confined to the runtime images/ dir; auto-named when omitted). In a \
         window the frame is bound to a successful application-present transaction: an opaque \
         window's is the destination that present submitted (present-only passes included), cut \
         to the frame (a Display-P3 or f16 destination converted to sRGB); image plain, a \
         translucent window and headless are semantic-renderer artifacts. While a frame \
         overflows its window (a transient resize) <w> <h> is the part the window shows. \
         Platform compositor visibility and scanout are not observed. \
         image --bytes = return the PNG base64'd OVER THE WIRE (OK 1 + `<w> <h> <nbytes> \
         <base64>`) instead of a server-local path — for a client that cannot read the server's files. \
         image --meta = add the frame's pixel fingerprint, phase, theme and per-leaf identity: OK <w> <h> \
         image-meta image-meta-version=3 <key=value…> path=\"<path>\" (with --bytes: OK 2, the \
         image-meta row, then the PNG row; frame-phase=destination for a submitted destination, \
         presented or staged for a renderer artifact; `identity=unavailable` when the frame's \
         provenance was not retained). image plain = bare pixels; image read = inline OSC-1337 images \
         as base64. @<sid> captures the app-render frame of the window showing that session. \
         The frame is the whole window (sibling panes, tab titles, overlays), so an edge token \
         is refused it (`ERR denied`); it reads its session with image read, text or screen",
    )
    // A file capture retains its exact-name guard until the client acknowledges
    // the reply.
    .ack()
    .forms(&[
        // Inline OSC-1337 images as base64 rows: a terminal projection with no
        // capture job or retained payload slot.
        form(Word("read")).framing(Lines).ack(false),
        // The PNG base64'd on one line (`OK 1` + `<w> <h> <nbytes> <base64>`),
        // not the `OK <w> <h> <path>` of a file capture; the flag may sit
        // anywhere in the tail. An in-memory capture keeps its admission only
        // through the bounded write and flush, so it needs no acknowledgement.
        form(Flag(&["--bytes", "bytes"])).framing(Lines).ack(false),
    ]),
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
         NewTabWithIdentity` for session-picker. Compositor visibility and scanout are not \
         observed"
        ),
    )
    .ack()
    // The Overview (and the Connection Map's old `connections` keyword) lists every
    // session of the instance: owner-only like `flows`.
    .forms(&[form(AnyOf(&["overview", "connections"])).owner_only()]),
    v(
        "video",
        Read,
        Status,
        App,
        "record N seconds (0.5..=60) of the front window's WSI-SUBMITTED destination frames",
        "-> frame_NNNN.png + index.json (same-clock timestamps; compositor visibility and scanout are not observed). Flags: full | keys (owner-only keystroke log: hardware input, plus socket input aimed at the tab ON SCREEN — `key`, `ctrl`, `send`, `feed`, `paste`, flagless OR an explicit `@<sid>` naming the front tab — each stamped on the frame clock. A verb aimed at a BACKGROUND session egresses on the control thread and CANNOT be logged (`@self` expands to that when the driving session is not front), and input that lands on a WINDOW this take is not capturing (the front window changed mid-take — an `aterm ctl spawn` alone does it) has no frame here that could answer it; those attempts are COUNTED instead and reported as `unlogged_inputs=` on the reply line, live as `unlogged=` on `video status`, and in index.json meta (with the window share broken out as `unlogged_other_window`), so an empty inputs[] is never ambiguous and a logged row is never a key the recorded window never saw. The log holds 1024 rows; attempts on the recorded window past them are counted the same way, broken out as `unlogged_over_cap` beside `inputs_cap`, so a full inputs[] is never read as the whole take. Drive the FRONT tab when you need key->frame latency) | pace (keep redraws flowing) | fps=<n> (cap capture rate, 1..=120) | budget=<MiB> (frame-store RAM, 64..=4096, default 512). Every recording carries >=1 baseline keyframe; retention converges to 8 eligible completed recordings while preserving fresh/live handoffs. `video status` = one-line read of the in-flight recording (recording= mode= elapsed_ms= frames= resized= keys=, and for a keys take the RUNNING inputs= unlogged= so a driver learns mid-take that it is driving an unloggable path); `video stop` = finalize it now — owner-only, like `keys`, because it truncates the take. `video frames [count=N]` = no capture; list the newest recording's N highest-delta (most-changed) frames as `frame n= delta= t_us= seq= <path>` rows, so an AI pulls just the eventful key frames instead of every PNG (default 8, max 64); a frame's delta counts the 8x8 tiles that differ from the previous captured frame (0 for the first, every tile after a resize), and its fp is a 53-bit hash of its pixels (equal fp = identical pixels within one take). index.json meta reports honest coverage: head_truncated/evicted_frames/ring_skipped/covered_us vs requested_ms, plus keys_requested/inputs_logged/unlogged_inputs. key->captured-frame latency = first recorded submitted destination containing the glyph minus inputs[].t_us (an inputs[] row is `ch` for a character or `key` for a named key like ArrowUp/Escape — NOT key->photon and NOT comparable to a keystroke-sampled latency: the recorder can only see a key's effect on its NEXT captured frame, so at fps=N no reading goes below ~1000/N ms however fast the terminal is. index.json `analysis` publishes that floor with every number (per row capture_floor_ms/at_capture_floor; per take capture_floor_p50_ms, capture_interval_p50_ms, attempts_outpace_readings, capture_verdict), and a reading is the first captured frame whose pixels differ at all, so a cursor blink before the echo counts as the response — use `metrics percentiles` input_p50/p95/p99_ms when you want typing latency); cadence gaps = frames[].t_us deltas vs ~16667, and a paced or headless present-real take also publishes ticks[] (every instant the loop's redraw PROVED the screen unchanged) so an unchanged screen's gaps read as quiet, not as missed samples, while a tick that neither presented nor proved it (a stall) stays a hole",
    )
    // A take runs for its seconds (up to 60) and then writes its frames, and a
    // result advertises retained server-local paths.
    .lane(Wait)
    .ack()
    .forms(&[
        // Ordinary in-memory control replies about the take in flight.
        form(AnyOf(&["status", "stop"])).lane(Rpc).ack(false),
        // No capture: `OK <n>` + the newest recording's frame rows, each a path.
        form(Word("frames")).framing(Lines).lane(Rpc),
    ]),
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
    )
    // Outside the halt: posts a row on the pull-down status bars or a note to their record
    // (chrome, not the grid), and it is Owner-only besides.
    .input(Input::NONE),
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
         [progress=<n>/100|busy=1] [load=<network|disk|cpu>] [reveal=pending] \
         [since_ms=<ms>] [need=<now|working|notice|later>]`. A bare <n> keeps the newest n (default 64, max 512); since=<id> keeps the OLDEST n above the id \
         (default all), so paging with the last id you saw never skips a row; tag= filters; sev= is a floor (sev=warn is warn and error); \
         need=later keeps the exceptions to review, open (state=review) and filed; \
         live keeps the unretired rows that are on or queued for the band (an open exception is \
         not one: ask need=later live). at= is the wall clock at ingress; since_ms= is on rows \
         retired by this process only. glass= is the band row (0 = top) or -; reveal=pending is a live \
         row whose grace (a standing ask's two seconds) has not run, not yet eligible for the band. detail= is the \
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
    )
    // Outside the halt: records, shows or ends a message on the band (chrome, not the grid),
    // and it is Owner-only besides. `act` presses a row's capsule, which reaches nothing an
    // Owner verb does not already (`update apply`, `open`, `spawn`).
    .input(Input::NONE),
    // `story` records the harness's choice on the session (`status story=`).
    // Owner-only, Write op-class because it mutates App state, Session target
    // because it names the session it is about.
    va(
        "story",
        Write,
        Status,
        Session,
        OwnerOnly,
        "story chose [<text>]: record what the harness chose",
        "The verb is the CLOSED SET `chose` — anything else is `ERR usage` (the seven other \
         words no process sent went with the presence row, 2026-10-16) — and the text is \
         optional, at most 96 bytes, no control byte. `chose` is the window harness's \
         supervisor answering Claude Code's question dialog by policy (`[harness] \
         answer_questions`, or the session's `meta set questions`), its text the policy word. \
         It is a RECORD: one story point (`status story=` counts it, `status level=story` \
         until a person's key in the calm session reads it); nothing is drawn, flashed or \
         played. Reply `OK story=<n>`, the point's seq. Takes the ordinary `@<sid>` \
         selector; bare, it is the connection's own session. Owner-only.",
    )
    // Outside the halt: records the harness's choice on the session, no byte on a PTY,
    // and it is Owner-only besides.
    .input(Input::NONE),
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
    )
    // Outside the halt: asks the window to bring the owner to System Settings' Developer
    // Tools list or show aterm's panel beside it (chrome and another app, not the grid); it
    // never turns a switch on there, and it is Owner-only besides: the class of `raise`.
    .input(Input::NONE),
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
    )
    // Outside the halt: reads which OS light/dark appearance the theme follows. Its one
    // write form exists only in a `dev-seams` build, standing in for a desktop light/dark
    // flip, which a halt does not stop either: what it can put on a PTY is the advisory `CSI
    // ? 997 ; Ps n` that same OS flip sends a mode-2031 subscriber.
    .input(Input::NONE),
    // The text scale native pages lay out at, and (development builds only)
    // one injected in place of the OS's. `Write` for the injection: it lays
    // out every native page again.
    v(
        "text-scale",
        Write,
        Status,
        App,
        "text-scale [0.85..2.0|os]: the text scale native pages lay out at",
        "Answers `text_scale=<f> source=<os|injected>`: the text scale Settings and the other \
         native pages lay out at (the system's text size where the host reports one; macOS \
         reports 1.0) and whether it is the OS's or one a development build injected. \
         `text-scale <0.85..2.0>` injects a scale in place of the OS's, re-samples and answers \
         the same line, so a lab can see the layouts larger text draws; `text-scale os` clears \
         it. Both need a development build (`dev-seams`) and are refused otherwise; \
         `$ATERM_TEXT_SCALE` injects one from launch.",
    )
    // Outside the halt: reads the text scale native pages lay out at; its write forms exist
    // only in a `dev-seams` build and lay out chrome again, never a byte on a PTY.
    .input(Input::NONE),
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
         last reported (`-`: none since launch; only macOS reports one), how many sessions journal \
         a crash-restore region and how many of their history rings memory pressure took away \
         (the largest under `warn` until half is gone, every one under `critical`; a crash of a \
         shed session restores its screen exactly and counts the scrollback above the keyframe's \
         few hundred lines lost), the RAM every region costs this window, and in how many \
         milliseconds the next shed ring comes back (a minute after the pressure eased, then one \
         a second). Each session's `status` says `journal_ring=<live|shedding|shed>` (`shedding` \
         while its old region is still in memory: still to be replaced, or replaced but not yet \
         handed to the keeper) and `journal_bytes=`, and `metrics` carries `journal_resident_bytes=`. `pressure \
         normal|warn|critical` injects a level as macOS's memory-pressure source reports one \
         (the scrollback is shed too) and answers the same line once the journal applied it; it \
         needs a development build (`dev-seams`) on macOS and is refused otherwise.",
    )
    // Outside the halt: reads, and in a development build injects, a memory-pressure
    // level; the shed takes history rings, never a byte on a PTY.
    .input(Input::NONE),
    // The harness card's constellation pin (`aterm-gui`'s `card_verb`): read,
    // and (debug builds only) pinned, so two builds' cards can be compared
    // pixel for pixel. `Write` for the pin: it re-arms every card on show.
    v(
        "card",
        Write,
        Status,
        App,
        "card [seed=<u64>|seed=off]: pin the card's stars (debug builds)",
        "Bare, answers `OK seed=<u64|off> rearmed=0`: the constellation the harness card's \
         stars are seeded with, or `off` while each card seeds its own from its session and \
         its process. `card seed=<u64>` pins every card's stars to that seed and re-arms each \
         card on show, so its next frames are drawn from it — the same stars in every build \
         and every run, for an A/B of the card's pixels (`targo --unverified lab instance \
         --pre 'card seed=42' --image ...`); `card seed=off` gives the cards their own seeds \
         back. Each answers `OK seed=<u64|off> rearmed=<cards>`; the pin needs a debug build \
         and is refused otherwise.",
    )
    // Chrome only: the debug pin re-arms cards, never writes a PTY.
    .input(Input::NONE),

    // The harness's login notices (`aterm-gui`'s `login_notice_verb`): listed,
    // and (development builds only) raised for the front tab through the
    // host's own path, so each one's rendering can be imaged headless.
    // `Write` for the raise: it puts a row on the band.
    v(
        "login-notice",
        Write,
        Status,
        App,
        "login-notice [<code> [<label>]|disk [spent|inodes|off]|end|back]: harness login notices",
        "Bare, answers `OK notices=<code>:<id>,…`: every notice the harness raises when \
         Claude Code's lost login, its renewal or an account move cannot finish by itself \
         (`aterm help harness`). `login-notice <code> [<label>]` (a code or an id from that \
         list; `<label>` a roster label, `main` by default) raises it for the first window's \
         first tab as the harness does — the band's standing ask, or for \
         `account-signed-out` and `renewal-wrong-account` the accounts line. \
         `login-notice disk [spent|inodes|off]` raises the disk-full alert as the harness \
         does when the volume runs out of room: `spent` (the default) once cleanup took \
         every build cache it may, `inodes` with the volume out of inodes, `off` with \
         cleanup off (`[disk] auto_free_gib = 0`). `login-notice end` takes them down, as \
         the loop does when its point moves on, and `login-notice back` says the tab's \
         login is back, as the loop does once a loss: a loss's notices are one row in \
         Settings > Messages, in its latest words, closed fixed once the login is back. \
         Each answers `OK sid=<tab> notice=<id|disk-full|end|back>`; all need a \
         development build (`dev-seams`) and are refused otherwise.",
    )
    // Outside the halt: lists, and in a development build raises, a login notice on the
    // band: chrome, not the grid.
    .input(Input::NONE),
    // Where Settings ▸ Security's MACOS ACCESS posture comes from
    // (`aterm-gui`'s `security_posture`), and (development builds' headless
    // instances only) a named fixture staged in its place, so the block can be
    // imaged and audited headless. `Write` for the stage: it redraws Security.
    v(
        "security-posture",
        Write,
        Status,
        App,
        "security-posture [list|off|<name>]: Settings ▸ Security's posture source",
        "Bare, answers `OK posture=<host|off|name> views=<n>`: where the MACOS ACCESS block's \
         posture comes from — the windowed macOS host's probes (`host`), nothing (`off`: a \
         headless instance, a build off macOS), or a staged fixture's name — and how many \
         Settings views draw the block. `security-posture list` answers `OK postures=<name>,…`. \
         `security-posture <name>` stages that fixture and `security-posture off` takes it off, \
         each answering the bare line; a staged posture runs no probe and its buttons reach no \
         OS. Both need a development build (`dev-seams`) and a headless instance, and are \
         refused otherwise.",
    )
    // Outside the halt: reads, and on a development build's headless instance stages, the
    // Security page's posture source: a redraw of chrome; a staged posture reaches no OS.
    .input(Input::NONE),
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
    )
    .lane(Wait)
    // The sudo askpass relay: a held session must not escalate to root. The serve loop
    // intercepts it before dispatch and asks the halt there. It writes no input.
    .input(Input::PTY),
    v(
        "chrome",
        Read,
        Lines,
        App,
        "the front window's native UI (toolbar + menu bar) and its presence line",
        "The last line is `presence rim=<none|stuck|decide|yours> pressing=<@sid|-> \
         pill=\"<sentence>\"` — what the human sees of the front window's NEEDS BORDER and its \
         titlebar pill: the border (`stuck` and `decide` are the window's most pressing \
         session's published need, `status needs=`'s severity, from any tab or pane; `yours` \
         is the front session's person hold with nothing needing them), the session it is lit \
         for, and the pill's whole sentence as of the read (its age included; `\"\"` with no \
         border, or with the View menu's needs switch, `[presence] rim`, off). The quoted value escapes \
         `\"` and `\\`. The pill carries the need's words as `status needs=` publishes them, \
         never a mail body or a limit message. Off macOS there is no menu bar: the `menu \
         \"<title>\"` lines are the app's menu model, less the rows this platform cannot act \
         on (the two update rows where there is no updater); `controls menu` is the live \
         palette, with each row's enabled state. A macOS `--headless` instance has no menu \
         bar either and prints `menu (none)`: reading the bar would create the application \
         object a headless process never builds; `controls menu` still serves the menu \
         model. Nor does it build a toolbar: after `toolbar (none)` it prints the \
         `toolbar-tabs` line the front window's strip would show, from its own tab model \
         (each tab's `states`, `\"done\"` among them for a finished task nobody has looked \
         at). On Windows a `strip-version shown=<yes|no> …` \
         line follows the tab-model line: the version label at the right end of the tab strip \
         (the macOS Version menu's title without its colour emoji, ` ↑` while an update is \
         staged), the strip `cols=` that `pointer move -1 <col>` takes and the `action=` a \
         click fires, or `reason=no-strip|not-laid-out|no-ui-face|strip-full` when it is not \
         shown (`not-laid-out`: no frame has laid the strip out yet — a headless instance \
         lays it out when it composes one, e.g. for `image`). Before \
         the presence line comes `statusitem title=<❯|❯•|❯N> needs=<n> stuck=<n> \
         exceptions=<n> done=<n> fleet=<off|unread|reading|failed|as_of:<unix ms>>`: the menu-bar item's \
         mark (`❯N` counts the needs: this aterm's, a sibling aterm's from a scan under 45 s \
         old, and a fresh fleet read's; red while `stuck=` is not 0; `❯•` is something to look at and nothing to do now), \
         `done=` its Done for you rows (a finished worker nobody here waits on, and a \
         supervised agent's task that landed, under Land, while nobody looked at its \
         tab: gone once someone does, goes to it from the row or reads its result, or a turn \
         opens its task again), and its fleet read \
         (`off` where no fleet glance runs: in no fleet, on a headless instance, or before \
         the menu-bar item is installed; `unread` in a fleet not read yet), readable without \
         a menu bar. On macOS the `statusmenu` lines between it and the presence line are \
         that item's menu as its own builder composes it now, from the same glance: what a \
         click on ❯ would show, read with no click and nothing of the menu changed (headless \
         too, where no item is drawn). One line per row, in its order: `statusmenu \
         \"<label>\" enabled=<yes|no>`, a row that acts adding `checked=<yes|no>` and, where \
         it has a hover, `tip=\"<text>\"`; `statusmenu ---` a separator; a submenu's row \
         ends `submenu`, and its rows follow it indented two spaces a level (the Fleet \
         row's as its own open composes them, from the fleet glance's last read). Labels \
         and tips are quoted as the `menu` lines' titles are. \
         Read-only.",
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
            " — the roster the verb's own usage error enumerates (`connections`, the Connection \
         Map's old keyword, still reads the Overview). `controls autopilot-menu` lists the open Autopilot menu's rows by their \
         stable keys (`autopilot-menu/mode/<word>`, `autopilot-menu/land-in/<minutes>`, \
         `autopilot-menu/land-at`, `autopilot-menu/land-cancel`, `autopilot-menu/settings`); \
         a place a row left while it was open is `autopilot-menu gap index=<i>`, blank and \
         inert, so the rows after it keep their frame rows. `controls overview` is \
         Owner-escalated; the rest answer at any scope. Off macOS `front` \
         ends with `confirm open=<true|false>`: a close or quit waiting on an answer in the \
         window, `by=` who asked. When a control client asked, `answer=\"confirm yes\"` goes \
         ahead and `cancel=\"confirm no\"` keeps it; when a person asked, both are `-` and \
         the person answers it."
        ),
    )
    // The Overview (and its old `connections` keyword) is owner-only like `flows`.
    .forms(&[form(AnyOf(&["overview", "connections"])).owner_only()]),
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
         geom=<CxR> evicted=<n> dropped=<n|-> cut=<quiescent|racy|file> folds=<f> \
         fold_events=<e> ms=<ms>[ carried=0 from_build=<n|->][ law=ambiguous][ undo=off][ \
         computed=client]`, then a `run t=<t> n=<k> \
         gap_ms=<ms> geom=<CxR>><CxR>>… net=<zero|CxR> first_out_ms=<ms|-> clear_ms=<ms|-> \
         culprit=<0|1>` line per LISTED \
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
         seeds disagree (`seed=alt` for a recording that starts mid-app), and `unknown \
         law=ambiguous` when the resize laws its header names (two aterms wrote \
         `aterm_resize_undo: 2`) disagree; a `--screen` settles either. At most two analyses \
         fold at once per instance (`ERR busy` after 10 s); `folds=` counts the engines \
         folded and `fold_events=` the events they applied (a variant replays only the events \
         after the anchor, from a copy of the faithful replay taken there). The one `aterm` \
         binary computes it client-side against a server without it (`computed=client`), and \
         offline: `aterm ctl cast drift --file <cast> [--screen <text>] [--until <t>] \
         [seed=…]`. `carried=0 from_build=`: the recording starts at a seamless update, so \
         the screen before it is in no replay. `undo=off`: the replays dropped the engine's resize \
         undo (a flap with nothing output between its halves moves nothing), as \
         an older aterm did — always against a server without the verb, which predates the \
         undo too, and offline only when that alone reproduces the `--screen`",
    )
    .forms(&[
        form(Word("frames")).framing(Lines),
        // `OK <n> verdict=…` + n `run`/`row`/`cursor` lines, not a byte count; an
        // older server answers the bare cast, told apart by the header's
        // `verdict=`. It waits up to 60 ms per cut for the recording to hold the
        // screen, then replays it through fresh engines: seconds of lane time on a
        // long recording.
        form(Word("drift")).framing(Lines).lane(Wait),
    ]),
    v(
        "temporal",
        Read,
        Bytes,
        Session,
        "temporal [status|<tick>] [trim]: the screen reconstructed at a past instant",
        "(needs temporal_recording=true; `temporal status` reports the reachable tick range \
         (`reachable_from=` through `latest_tick=`; `reachable_from=-` when the spine's end \
         does not replay), `lost_resizes=` (resizes the spine will never hold), \
         `dropped_input=` (input slices not recorded: each is a break no replay crosses, \
         closed by a keyframe of the live screen at the program's next output), \
         `dropped_replies=` (reply instants not recorded; no replay reads one) and \
         `pending_resizes=` (resizes \
         not on it yet: they reach it at the program's next output, and a replay at the \
         spine's end adds ` pending_resizes=<n>` to its header meanwhile). trim drops the \
         trailing all-blank rows — byte-framed, so `OK <nbytes> trimmed=<k>` counts the \
         trimmed body; `status` has no rows, so `status trim` is `ERR usage`",
    )
    // One `OK enabled=…` line, not the screen reconstruction's byte body.
    .forms(&[form(Word("status")).framing(Status)]),
    v(
        "history",
        Read,
        Lines,
        Session,
        "history [<n>] [since=<id>]: the turn LEDGER",
        "- id/submitted/status/dur_ms/seq/hash/by/arch/text per completed turn; by= is who typed \
         it: harness (aterm's window harness, typing under its own hand; a carried record: the \
         previous instance's), ctl (an owner-token client), bridge (the fabric bridge), the \
         caller's own sid (an edge-scoped connection), or - (no caller can be named: a turn \
         carried from an older aterm, an edge whose grant is gone); fleet is reserved. status= \
         is settled, timeout or hangup for a `turn`, and pressed where an Enter was written and \
         nothing awaited after it: a text the harness typed with `send` and submitted with \
         `key enter`, or a `turn settle=submitted`. It keeps the newest 512 records, evicting \
         the oldest — the harness's oldest while its own are more than half of them, so the \
         harness's continuations push out another caller's turn only while those fill half the \
         ring or more; everyone else's newest 255 always stay. \
         arch=<origin>:<last> is the alt-screen archive's mark when the turn started, for \
         `offscreen since=`. An aterm self-update carries the ledger and the id count to the new \
         instance when it can: ids keep rising, and the carried records print carried=1 before \
         text= (their started_ms and seq are the old instance's; by= is kept); a ledger it could \
         not carry whole adds unheld_below=<id> to the OK header when that floor is above \
         since=, until the turns it lost would have aged out of the ledger anyway (records \
         below it may be gone). With the PTY keeper on, a restore after a crash carries the \
         ledger too: each session's crash-restore region holds its newest records that fit 64 \
         KiB (a few hundred of long prompts), in memory only, rewritten at most every two \
         seconds while turns land (`status journal_ledger_lag=` counts the records it does not \
         hold, 0 once it holds them all). Its records print carried=1, and unheld_below= is \
         past every id the crashed instance used, since a turn from its last two seconds, or \
         the whole ledger of a session whose region was being rewritten at the crash, can be \
         missing. New ids go on above it: each region also holds the instance's id count, \
         stored as each id is minted. A crash that leaves no session with a journal (status \
         journal=off) leaves no count, and ids start over",
    )
    .json(Json::Refused),
    // `meta` reads/writes the USER-settable session metadata. Base op-class Read
    // (the bare form is a pure metadata readout); the `meta set`/`meta unset`
    // sub-forms are WRITES, which the row's form records (session-scoped state
    // like `lease` — NOT ConfigWrite: nothing durable on disk is rewritten).
    v(
        "meta",
        Read,
        Status,
        Session,
        "meta -> OK title= user_title= description= icon= role= attention= cwd= state=",
        "attention_owner= attention_owners= attention_kinds= supervisor= supervisor_proven= \
         questions= agent_cwd= trusted_cwd= autopilot= land_at= autopilot_by= autopilot_since= \
         (pct-encoded; '-' = unset; read \
         fields by key; trusted_cwd= is the one folder a worker's `spawn agent=claude` trusted \
         for it, which no `meta set` writes). `meta set \
         <title|description|icon|role|attention|questions|autopilot> <text...>` / `meta unset \
         <field>` set or clear the USER metadata (write-gated; user title outranks the OSC \
         title in tab labels; a `role` is a label, the first the menu-bar status item names a \
         tab by; an `attention` entry that asks (the bare entry, a `kind=ask` one, or a keyed \
         one naming no kind outside the `limited:` rule below) is the typed need the menu-bar \
         status item counts (`❯N`), where a `kind=wait` entry is a record it does not count — \
         the server's own `aterm` stall entry (read as `frozen` or `stopped`) and the live \
         upgrade's `upgrade` entry (told in the window's band) excepted; caps: \
         title 120B, description 1024B, icon 64B, role 64B, attention 256B). KEYED ATTENTION: \
         `meta set attention owner=<k> [kind=ask|wait] <text...>` / `meta unset attention \
         owner=<k>` write only owner <k>'s entry (<k>: one token of 1..64 bytes from \
         [A-Za-z0-9._:@/-]; text 200B), so a supervisor and a human raise and clear their own \
         escalations independently; the bare form is owner `-` and clears only its own. kind= \
         says what the entry is: `ask` (the owner is needed: `status needs=ask`) or `wait` (the \
         writer waits something out by itself, as the window harness does a usage limit it \
         continues after the reset: a record, never a need); only a keyed entry names one (a \
         bare `kind=` is `ERR usage`), the bare entry is an ask, and a keyed one with none is an \
         ask unless its text opens with `limited:` while the harness answers the agent's wall. \
         `attention_kinds=` lists `<ask|wait|->:<owner>` per entry, oldest first (`-` = no kind \
         named; the bare owner is a raw `-` here, where `attention_owner=` spells it `%2D`). At most 8 owners, one slot kept for the bare \
         owner (a new keyed owner past 7 is `ERR attention owners full`); `attention=`, the tab chrome, \
         `sessions meta=1` and the `meta` event show the MOST RECENTLY SET entry, and \
         `attention_owner=` names its owner (`%2D` = the bare owner) of `attention_owners=` \
         holding one, while the menu bar's row (`status needs=ask`) and, while that is the \
         need, the survey's attention excerpt show the most recent ASK, so a newer wait never \
         hides an older ask. A seamless update carries every owner's entry (the server's own stall \
         entry is derived again); a restore carries only the bare one — a keyed owner \
         re-asserts its own. The window harness keeps owner `needs` (kind=ask) for the lines \
         of an agent's report that start `NEEDS YOU:`, raised at the point and cleared at \
         `NEEDS YOU: nothing`, someone else's turn, Off or its loop's end, while it carries the \
         agent on (`aterm help harness`). SUPERVISOR: `meta set supervisor <holder> [ttl=<ms>]` / `meta unset \
         supervisor [holder=<holder>]` (Owner-only; an edge gets `ERR denied`) shows a running supervisor as \
         `supervisor=` in `meta`, `status` and `sessions`. Without ttl= the claim lasts while \
         the connection that set it keeps serving requests (it closes, or turns into a \
         subscribe stream: cleared); with ttl=<1..600000> it is a lease that lapses unless \
         re-set — the spelling for one-request-per-connection clients; at the lapse the claim \
         is removed with a `meta-change field=supervisor value=-` event and a box it was \
         holding reaches the menu bar (and a desktop notification with desktop_alerts = true). While a claim is live the agent's \
         own prompts and questions raise no menu row or notification: the supervisor answers \
         them or escalates with `meta set attention owner=<k>` (a hold voids the claim for \
         them). A wall (`status needs=`): a lost login always raises one; a usage, model or \
         spend limit raises one only once its reset has passed (or when it names none this Mac \
         can place), and never while the window harness answers it or the supervisor loop's \
         own `kind=wait` entry (owner `supervisor`) waits it out under a live supervisor claim \
         (any other writer's wait is a record that waits no wall out); a memory banner the host \
         restarts and an unreachable API never raise one; any other wall raises one unless, \
         with no hold standing, the window harness answers it or, for an overload, an API error \
         or a full context, another holder's live claim retries it (a hold parks every harness; \
         a limit's answer outlives it). Another holder's live \
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
         session's work, over the `[harness] autopilot` default: `cruise` keeps it moving for \
         ever, toward more complete and more correct work (every turn end continued; a report \
         that says done gets a prompt to land what is open and find more; nothing the agent \
         says ends it), `land` lands ALL of the work onto the remote main branch (the land \
         instruction, then a nudge at every turn end with no bound, the land answer to a \
         question asked in prose; a question dialog is answered as under `cruise`) and ends \
         only at a land act's reply that defends in a few lines starting `Nothing left:` that \
         nothing is left, naming no open work and no `NEEDS YOU:` need, over a repository \
         `git` finds holding nothing the remote main lacks — `task=landed`, nothing more \
         typed until someone else's turn (`land` turned to `cruise` reopens it), \
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
         `OK land_at=<unix s> local=<date>T<HH:MM><offset>`. Every `OK` of a write of the \
         word comes once the crash journal's image that carries it is on disk, where a desk \
         is journaled (`restore_session`, a windowed unix launch), so a kill sent at the \
         reply cannot lose it; past 5 s it says `journaled=0` (the word stands, no image on \
         disk carries it yet). Only `cruise` takes `until=` \
         (`ERR until= goes with cruise only: usage: …`), and any write of the word without it, \
         the same word included, and `meta unset autopilot` cancel the timer; at the time the \
         window writes `land` only if the stored word is still that timed `cruise`, so a hand \
         write always wins. `autopilot=` is \
         printed on EVERY bare `meta`, `-` while unset (the default decides), and after it \
         `land_at=`, the instant a timed Cruise lands (unix seconds; `-` while none), then who \
         set the word and when: `autopilot_by=<human:chip|human:border|human:menu|\
         human:settings|human:every|ctl|harness|bridge|aterm|restored|->` (the window's \
         chip, composer border, Autopilot menu, Settings > Harness row or its Every session \
         row; an Owner client; aterm's harness; the fabric; the timer's fire; a word carried \
         by an older layout; never set — a session's own edge connection may not write the \
         word) and \
         `autopilot_since=<unix s|->` — a setter stays beside a word unset, and each move of \
         the word, over the wire too, leaves the session's `harness` record naming its \
         setter (`set over aterm ctl`); the master \
         switch (`[harness] enabled = false`) wins over the word. It counts toward `sessions \
         meta=1` and survives a restore and an update's handoff; the footer's Autopilot chip \
         and Settings > Harness write the same word. No `window=` here, \
         for the reason `status` gives: `meta` is polled and the window lives on the main thread \
         — ask `sessions`/`ls` (one hop for the whole fleet) or `dims`",
    )
    // `meta set`/`meta unset` write session metadata; bare `meta` reads.
    .forms(&[form(AnyOf(&["set", "unset"])).op(Write)]),
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
         `rules_file` text as the window's own continuation is, then `questions=<ask|\
         recommended|long-term|short-term>`, resolved: the session's own `meta questions` word, \
         else `ask` under `answer_questions = false`, else `question_priority`. After it, \
         additive, the FOREVER CRUISE / FOREVER LAND words exactly as the window's own loop \
         types them for this session, pct-encoded: `done_check_text=` (Cruise's \
         defend-and-find-more prompt, `continue-done-check@v1`, without the suffixes a point \
         adds), `land_text=` (`land@v1`), `land_nudge_text=` (`land-nudge@v1`) and \
         `land_answer_text=` (`land-answer@v1`), each composed with the `rules_file` text as \
         the loop composes the act, then `needs_hint_text=` (the NEEDS YOU hint after an \
         answer) and `needs_ack_text=` (the ack once per new set of needs), the two suffixes, \
         bare (the loop types them inside an act, before its one copy of the rules). The \
         session's Autopilot word is `status autopilot=`, not a key here (`autopilot=` is the \
         table's default). An edge token reads only `OK effective=<on|off>`. Read-only. Any \
         other form is `ERR usage: harness policy`.",
    ),
    // `status` is the READ-ONLY Subject+Status record (RFC: Tab Subject &
    // Status §8) — what a session IS and what it is DOING, classified entirely
    // locally. Versioned so any consumer (a script, an agent, the harness) can
    // reject a schema it does not know; there is no write sub-form, so it has
    // no forms.
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
         fabric_link_age_ms=<n|-> fleet=<off|listening|retrying|refused:<why>> \
         identity=<name|-> \
         hand=<-|turn:<id>[:<holder>]|lease:<holder>|driving:<sid>> hand_pid=<pid|-> \
         level=<quiet|note|story|driving|driven|attention|limited|hold> story=<n> \
         why=<-|prompt,question,escalation,mail> \
         needs=<frozen|stopped|stalled|login|memory|wall|ask|approval|question|-> \
         hold_by=<person|remote|local|bridge|-> typed_by=<pct|-> typed_ms=<ms|-> \
         path=<frozen|live> \
         program=<name|-> \
         agent=<busy|prompt|question|wall:<kind>|idle|survey|unknown|-> agent_detail=<pct|-> \
         agent_rev=<n> \
         agent_since_ms=<ms> agent_gen=<e.s|-> agent_fp=<hex16|-> \
         os_appearance=<dark|light> theme_side=<dark|light|-> reported_scheme=<dark|light|-> \
         scheme_from=<theme|program|-> bg_dark=<0|1|-> \
         program_state=<idle|working|blocked|done|error[:<kind>]|-> last_act=<act|-> \
         [conpty=<openconsole|inbox|->] integration=<on|off|degraded|-> \
         input=<-|clear|pending|typeahead|stalled|stopped> input_bytes=<n|-> \
         input_wait_ms=<ms|-> fg_rss_mb=<n|-> \
         disk=<unknown|ok|low|short|emergency|-> disk_free_mb=<n|-> disk_room_mb=<n|-> \
         disk_age_ms=<n|-> supervisor=<pct|-> supervisor_proven=<0|1> \
         autopilot=<off|cruise|land|-> \
         human_ms=<ms|-> human_seq=<n> \
         launch=<person|since|typed|boxed|other|pending|none> launch_seq=<n|-> \
         path_evidence=<measured:<ms>|unconfirmed:<ms>|carried|-> copy=<managed|foreign|-> \
         upgrade=<-|<state>/<to>/<why>/<age>> history_lost=<n> \
         integration_rev=<current|stale:<hex16>|frozen|-> \
         born=<cold|spare-parked|spare-warming|spare-ready> [spare_age_ms=<ms>] \
         render=<ok|displaced|desync-risk|unverified|unjudged|-> resizes=<n> flaps=<n> \
         land_at=<unix-s|-> task=<open|landed|theirs|-> pause=<unix-s|-> \
         task_state=<working|done|exited|stalled|-> task_rev=<n> \
         log_seq=<n> log_ep=<e> run=<r<n>|-> turn=<t<n>|-> \
         step=<p<n>|-> need=<word|-> need_since_ms=<ms|-> editing=<n> gen=<e.s|-> seq=<n|-> hash=<hex16|->. \
         `editing=` counts the open `edit` tickets whose program waits in this session (an \
         `edit … wait=0` is not one); while it is not 0, `send`, `key`, `ctrl`, `feed`, \
         `feed-bin`, `mouse`, `paste`, `paste-bin`, `turn` and `focus` answer `ERR busy editing \
         ticket=<t> view=<v>`, for any caller, and so do `hwkey`, `invoke Paste` and every \
         `pointer` but `pointer status` while this session's tab is the one on screen. \
         `disk=` is the volume holding the session's working directory as the disk watch \
         last measured it (ok, low below its warning, short below its floor, emergency), \
         `disk_free_mb=` its free MiB, `disk_room_mb=` its room (what one round of this \
         machine's builds takes) and `disk_age_ms=` the reading's age; `-` while no reading \
         holds it (not measured yet, a remote or automounted mount). \
         `log_seq=`/`log_ep=` are the session log's head and epoch as a reader is shown them \
         (`log since=<log_seq> ep=<log_ep>` and `await log … since= ep=` read what comes after), \
         `run=`/`turn=`/`step=` its open path, `need=` the most urgent need it OBSERVED \
         (`approval:<kind>`, `question`, `wall:<kind>`, `unresponsive`, `failure`, `escalated`, \
         `exited`; a need a `mark` claimed is `brief`'s `claim need`, never this) and \
         `need_since_ms=` its age; `-` when none. \
         `render=` is the resize ledger's verdict (see `resizes`): \
         `displaced` while a resize has moved or cut alt-screen content and the engine still owes \
         it (until a full clear or screen switch repaints it, an undo hands the rows back, or \
         the size comes back and the epoch is judged), `desync-risk` once, 1 s on, the app has drawn over a \
         net-zero flap's displaced content without clearing (between its halves or after), \
         `unverified` when it drew over a net-changed run's with no clear seen (repainted or \
         not; `cast drift` decides, a size that later came back, however long after, weighed \
         as a flap), `ok` for a flap the engine's resize undo restored, and `unjudged` when \
         the ENGINE owes rows or cut cells (its render debt) with the geometry back and 1 s passed but the \
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
         the band) — `0` when every line crossed. `task_state=`/`task_rev=` are the session's \
         task record's state and rev (`task`; `-`/`0` for a session with no task). \
         `fleet=` is the INSTANCE's fleet listener (`[fleet] listen`): `off` when none is \
         configured, `listening`, `retrying` while another process holds the port (the \
         port is the lock) or the address is not this machine's yet (tried again every \
         2 s), or `refused:<why>` with why one of `nested` (an aterm inside another never \
         listens), `listen` (not a numeric IP:port), `allow` (not facts|content), \
         `no-key`/`key-mode`/`key-link`/`key-format`/`key-unreadable` (`<state>/fleet.key` \
         missing; another user's, open to other users or hard-linked; a symlink; not one \
         secret, or not a file in a state directory; unreadable — read again every 2 s, \
         while listening too, where a changed or removed key cuts every fleet \
         connection), `bind` (another bind error), `stopped`, `thread` and `unsupported` \
         (no fleet listener on this platform). \
         `seq=`/`hash=` STAMP THE LIVE SCREEN: the terminal's \
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
         `hold=1` means a drive halt stands on this session — the person's Take Over, the \
         owner's local hold, the fleet's, or several: every PTY-reaching verb answers `ERR \
         halted reason=<r> origin=<person|remote|fleet|local>` (the person's when it stands, \
         else a remote node's, else the fleet's; the reason is on that line and on the `hold` \
         record in `events`, whose `hold_origin=` lists every origin standing, and `hold_by=` \
         names whose); an \
         `origin=person` hold is the person's at the window and only their Hand Back lifts it \
         (`hold <sid> off` answers `OK hold=1` and lifts nothing of theirs: wait, or ask them), \
         an `origin=local` hold is the owner's own and `hold <sid> off` lifts it, a fleet hold \
         only the bridge lifts, and the session stays held until every one has. \
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
         `hand=` is whose hand is on the keyboard: `turn:<id>` \
         a peer's open `turn` (`:<holder>` names the driver by its `meta role` or short sid \
         when the turn came over an edge — the edge's source; an Owner-token turn names \
         nobody, whatever edges stand), `lease:<holder>` a cooperative drive lease, \
         `driving:<sid>` this session's own open turn into another, `-` nobody. `hand_pid=` is \
         the kernel's pid of a live drive lease's taker (`lease status pid=`; `-` for no hand, \
         a `turn`, a lapsed lease, or a taker no kernel named): a `lease:aterm-harness@<pid>` \
         is aterm's own hand only when `hand_pid=` is that aterm's pid, since any client can \
         spell the name. `level=` is a record of severity, ascending \
         `quiet < note < story < driving < driven < attention < limited < hold` (`limited` \
         is an agent at a wall nobody answers: one the in-window harness resumes by itself \
         reads `note`; `story`: something happened since the human last looked; the window's \
         border reads `needs=`, never the level); `story=` is the seq of \
         the newest story point (`0` = nothing ever happened; `ctl story` replies with it). \
         All three are read from the presence slot the wakes keep current — no lock, no \
         classifier — so they cost the poll nothing. `why=` says what put the session at \
         `level=attention`, comma-joined in that order: `prompt` (an approval box), `question` \
         (the agent asked), `escalation` (a typed `meta attention`), `mail` (an unread \
         task/ask with no hand on the session); `-` at any other level. \
         `needs=` is what the session needs of its owner NOW, the one verdict the menu bar, \
         the survey and a fleet reader read (most pressing first; stuck: `frozen` not reading \
         input, `stopped` stopped with input queued, `stalled` its task stalled, `login`, \
         `memory` the memory banner nobody restarts, `wall` any other wall nobody answers — a \
         usage, model or spend limit only once its reset has passed or when it names none this \
         Mac can place; decide: `ask` an attention entry asking for you (the bare entry, a keyed \
         one of `kind=ask`, or a keyed one naming no kind — one opening `limited:` while the \
         harness answers the wall is a wait; the server's own `aterm` stall entry and the live \
         upgrade's `upgrade` entry excepted), `approval` a box and `question` a question no \
         harness answers — a held tab's are nobody's), `-` when nothing does: a box the harness \
         presses, a limit it waits out or the supervisor loop's own `kind=wait` (owner \
         `supervisor`) waits out under a live claim, an unanswered limit before its reset, an \
         overload, API error or full context the harness or another holder's live claim \
         retries with no hold standing, an unreachable API and a hold are never a need. \
         `hold_by=` names whose hold \
         stands (`person` the window's Take Over, `remote` another Mac's node over the fleet, \
         `local` an Owner's, `bridge` the bus's; the person's first where several do), where \
         `hold=` says only whether one does. \
         `typed_by=` names who last typed into it over this socket (an `OK` that typed, from \
         `send`, `key`, `ctrl`, `feed`, `paste`, their `-bin` forms or `turn`, and an `invoke \
         Paste` that pasted at once; never an `OK dup=1`, nor the person's keys, nor `hwkey`'s, \
         which go the person's way to whatever tab has focus): the tab whose shell started the \
         client, by its role, else the tab name a person set, else its session id (never a \
         title its program set, nor a word aterm credits by), else the client's process \
         (`python3 (pid 4242)`); `aterm` for aterm's own harness; `the message bus` for the \
         bridge; `<host> (node <8 hex>) over the fleet` for another Mac this one lets drive it \
         (the step-in: its host as this Mac's book names it, which is that Mac's own word, and \
         the node it proved), and no tab is named so; over an edge, the client that presented \
         its token on the session it was minted for (its src: `python3 (pid 4242) on \
         lab-sweep's edge`), or that session alone \
         when its own shell started the client; `typed_ms=` how long ago. \
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
         task's face as its loop last told this window's harness (`landed`: under Land, a \
         clean `Nothing left:` defense answered a land act and all of the work is on the \
         remote main branch; `theirs`: a person's own exchange; `open` otherwise, which \
         Cruise always is, since nothing ends it), printed only while this window's harness holds the live claim; \
         `-` otherwise. `pause=`, under the same rule, is the instant (unix seconds) the \
         harness's pause after short turns ends — its back-off, which the chip's title says \
         as `pausing until 18:41 after 3 short turns` — `-` while none stands. \
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
         (spawned/state-change/title-change/cwd-change/meta-change/agent-change/task-change \
         (`rev= state= attention=<0|1> [why=] [exit=]`, a worker's task record moving, see \
         `task`)/status-change \
         (`phase= outcome= exit_code= signal= confidence=`, one per transition that survived \
         dwell)/human/modes-restored \
         — the last is the PTY reader handing the terminal back after a foreground program lost it \
         with modes armed: `from=<pgid> to=<pgid> program=<name|-> reverted=<csv> bytes=<n>`, or a \
         manual `reset`: `reason=manual source=<ctl|menu> reverted=<csv|-> bytes=<n>`) AND \
         the fabric/messaging \
         kinds the same ring records (hold/inbox/inbox-seen/post/fetch/post-landed/topic), and \
         `render` — the resize ledger entering or leaving `desync-risk` (`desync-risk run=<r> \
         displaced=<+-k> net=zero at=<ms>` / `healed run=<r> after_ms=<ms>`, see `resizes`; a \
         healthy resize records nothing here), `harness-act` (`act=<act> holder=<holder>`, \
         an act a self-supervising agent recorded) and `program-state` (`state=<idle|working|\
         blocked|done|error|-> kind=<permission|question|auth|->`, one per move of the codex \
         in front's OSC 7501 report as the status sweep honoured it, `state=- kind=-` when \
         it goes away; display only, read with the rows' `t=` as the program's turn ledger) \
         — monotonic ids, rising on across an aterm \
         self-update (whose successor's first row is `handoff`), a drop-oldest \
         ring of 512 rows in which `program-state` rows hold at most half: past that a new \
         one evicts the oldest `program-state` row, not the oldest row. The two rows a session records as it is retired — `closing reason= by=` and the \
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
        "log [<node>|n] [lvl=] [since= ep=] [n=] [kinds=] [class=turn] [tier=content] [--json]: THE LOG",
        "- what the session did, as a tree: the session (`-`), its runs (`r<n>`: a shell \
         command between OSC 133 C and D, a program in front, an agent), an agent run's turns \
         (`t<n>`) and a turn's steps (`p<n>`: an approval box, a question dialog, a wall, an \
         input stall, the survey), with point marks: `human` (a person's input burst, and \
         their first input to reach the program after mail came in; `reached=0` when the \
         input it marks reached no program — a view scroll, a selection, a wheel no program \
         is sent, a composition — so it dismissed no box and answers no mail), `input` (a \
         control input: `verb= [act=] [re=] [dropped= [pool=shared]] [cause= cause_k=]`, see \
         `help send`) and `task` (a worker's task moved: `rev= state= attention= [why=] \
         [code=<exit>]`) on the \
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
         reads it only if it could read the session live. A seamless update carries it (see \
         `handoff` above). It is written to disk on macOS and Linux, following restore_session, \
         unless `[session_log] journal = false` (on by default; a headless instance keeps none), \
         so `log @<sid>` also answers, owner only, a session of an earlier process; and \
         its turn and bus class reach the fabric only with `[fabric] log = \"turns\"` (off by \
         default; `aterm fabric brief` reads that mirror). `log n` reads the INSTANCE's node log \
         instead (Owner-only, like `brief all`; no selector; answered with no terminal): 1024 \
         records at node `n`, oldest evicted first, in memory only (never journaled, carried or \
         mirrored) — `open n start prev_end=<clean|handoff|killed|signal|panic|running|unknown|\
         none|->` (how the run before this one ended, read off the recovery ledger and written \
         as an `amend n start` once read; `-` until then, and for a launch that never reads it; \
         a signal's number in `code=`), `mark n link state=<stalled|connected|disconnected> \
         gen=<n>` at each stored fabric link transition (`by=bridge` for the bridge's word, \
         `by=aterm` for an attach or a loss), and `mark n config key=<key>` for each config key \
         a reload moved: the key's name only, never its value (a key no setting names, or past \
         16 in one reload, is counted, `config n=<k>`); `since= ep= n= kinds= --json` as above, \
         `ep=` this process's opaque node-log epoch",
    )
    .json(Json::Emits),
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
         A turn that names no `cause=` and probably answered the one `kind=task` mail \
         delivered just before it says so as `mail=@<off>~` (an open turn, or the newest closed \
         one): a guess, never without its `~`. \
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
    )
    // Outside the halt: a claim on the session log. It puts no byte on a PTY, signals
    // nothing and retires no session; acting is `key`/`send`/`turn`, which the halt asks.
    .input(Input::NONE),
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
         per epoch that is open or owed rows or cells — one kind token first, parse by key. `resize <id> t=<ms> run=<r> ord=<n> from=<CxR> to=<CxR> alt=<0|1> sync=<0|1> \
         trimmed=<k> demoted=<k> pushed=<k> revealed=<k> appended=<k> restored=<k> shift=<+-k> \
         cut=<k> site=<input|window|chrome|term|pass|redraw|ctl-cross|-> at=<pct file:line|-> \
         win=<CxR|-> chrome=<k|->`: what the ENGINE did to the rows (a demote moves every row \
         up, an append or a trim moves nothing; `cut=` drawn columns a narrowing with no \
         rewrap took off the right edge; `restored=` rows the engine's resize undo \
         handed back, a quiet alt-screen flap's demoted rows (also in `revealed=`) or pushed \
         ones; `ord=` the engine's resize ordinal), who asked \
         (`site=` the entry point that ran the window pass — the input dispatch a ctl `resize` \
         arrives by, the OS window, its chrome rows (tab strip, message band), a \
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
         `epoch <e> t=<ms> geom=<CxR>><CxR> shift=<+-k> pushed=<k> cut=<k> drew=<0|1> \
         class=<agent|learned|unknown> verdict=<open|desync-risk|unjudged|healed|restored> \
         by=<app|undo|nudge|-> tried=<0|1>`: the second judge, which does not depend on run shape. \
         The engine counts what it owes the alternate screen since the last repaint from \
         scratch (`shift` rows moved up, `pushed` rows lost off the bottom, `cut` drawn columns \
         a narrowing took that the grow back left blank; zeroed by a full clear, a screen \
         switch, a reset; exact however far the journal overran), an epoch \
         opens at the first resize after that with the geometry it began at, and it is at \
         risk when anything is owed, the geometry is BACK at its start and 1 s passed since the \
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
         term_wait_redraw/term_wait_press, echo, present_glass, out_req, ime, status_glance \
         (one build of the menu-bar item's glance, max_status_glance_ms= its worst), \
         need_publish (one session's need verdict published, max_need_publish_ms= its worst) and \
         tx_chunk/tx_last (one hold of a session's timeline lock applying transcript steps: a \
         full 32-event chunk, and a batch's last hold; max_tx_hold_ms= the worst of either), plus \
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
         — a run with `cursor_trail_style = \"off\"` and `cursor_momentum_glow = false` reads 0/0.00 for its \
         whole life, and a non-zero \
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
         nothing will close — and pty_kits= (the masters opened ahead for the next spawns, \
         which no session owns yet; the process's open /dev/ptmx descriptors number \
         pty_masters + pty_kits + spare_ptys), then identity_primes_running= (the identity \
         re-prime passes, a spawn's or the window launch's, still writing an identity's \
         primer files; 0 after a spawn's reply means none is). IT NAMES WHO OWES THE TIME: echo_* high with input_* low means the PROGRAM \
         is behind (a TUI sharing its scheduling band with a build has been measured echoing at \
         p99 150ms while aterm wrote every key at p99 6.29ms), the reverse means aterm is; a \
         lone max_present_latency_ms is not evidence of either. READ THE SLICES HONESTLY: present_* and input_* are OPEN INTERVALS closed by the next qualifying present, so any stretch in which nothing presented is INSIDE the number (only a 5 s discard bounds it) — a multi-second present_latency means \"nothing presented for that long\", not \"a frame took that long\"; input_* closes on the next CONTENT present (for a key a native page such as Settings takes, the present of the page's new pixels), which under concurrent streaming output may be a log-line frame rather than the key's echo, so it can read low. input_* counts EVERY key a content present answered (up to 32 waiting per window; the overflow is input_edges_dropped), so keys that never echo — a password prompt — each book their wait until the next content present (under the 5 s discard): measure typing latency in a program that echoes. Quote n_* with the percentiles, never a lone last_/max_, and note both stop at application-present return (no compositor selection, scanout or photons)",
    )
    .json(Json::Emits),
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
         [timeout=<ms>] [submit=<key|none|guarded:<re>>] [settle=match:<re>|gone:<re>|submitted] [submit_window=<ms>] \
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
         settle=submitted waits for no settle: the reply comes the moment the submit is \
         verified (status=submitted, the screen as it stands then), for a screen that never \
         goes quiet (Claude Code 2.1.301 animates its logo at an idle prompt, so an idle settle \
         spends the whole timeout) when the caller waits its own way next — a follow-up into a \
         worker, then `await task @<w>:<task_rev>`; an unverified submit is status=timeout as \
         ever, and submit=none with it is the usage line. submit_verify=auto (the default) \
         verifies a submit into a session read as Claude Code whose composer shows the typed \
         text (its echo waited for as the screen moves, within the echo's 5 s cap; a text not \
         shown by then, or folded into a `[Pasted text …]` chip, keeps seq) by that text LEAVING \
         the composer (its dim placeholder is no text; or a box or menu taking its place, judged \
         at the window's close), a signal its animated logo never gives; at another program's prompt by \
         seq (the first content advance after the press), which a repaint of its own can be, so \
         submitted=1 there says the screen moved, not that the Enter landed; at a shell prompt \
         by block (a command started). A turn into a worker (a session with a task record) closes \
         its verdict with task_rev=<n>, the record's rev as the Enter found it: `await task \
         @<w>:<n>` answers on this turn's own done, one marked before the wait was armed \
         included, and the done the Enter found is the only one the turn opens again. A turn \
         into an agent (its published verdict not `-`) closes with agent_rev=<n>, the \
         verdict's rev as the Enter found it: `await agent <words> since=<n>` answers only on \
         one published after it. \
         submit=guarded:<re> submits with Enter only while the row holding the CURSOR still \
         matches <re> — where a submit lands: a composer holds the cursor, an approval box does \
         not, and Claude Code's transcript repeats the composer's `❯` at column 0 — checked and \
         pressed under ONE terminal lock hold (the `key if=` delivery), so text typed into a \
         composer is not submitted into whatever replaced it; a miss on the first press leaves \
         the text typed and answers `OK 0 turn skipped reason=guard submitted=0 seq=<n> id=<n>` \
         (zero rows), a miss on a re-press ends the presses, and the verdict gains \
         `pressed=<n>`, the Enters written (`submitted=0 pressed=1` = written, not verified — \
         never retry it blindly). A hold set while the turn runs (`hold`, the person's Take Over) \
         is asked again before each key, paste and Enter: before any byte lands the turn answers \
         `ERR halted …` as it would have arriving; after, it stops there, pressing nothing more, \
         Enter included: `OK 0 turn skipped reason=halted submitted=0 seq=<n> id=<n>`, a typed \
         turn's with typed=<k> pasted=<p>. Reply: verdict line \
         (id=/submitted=<0|1>/status=settled|timeout|submitted/seq=/dur_ms=/hash=<FNV16 of settled screen>[/task_rev=][/agent_rev=]) \
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
    )
    .lane(Wait)
    // A turn's reply is its settled screen, so it carries no `seq=`.
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
        "— one holder at a time, TTL-expiring (default 30000ms, max 600000), surfaced in `who` as \
         driving=lease:<holder>. Forms: lease [status] | lease acquire [ttl=<ms>] [holder=<name>] \
         [hard] [here] | lease release [holder=<name>] [force] [here]. acquire refuses a live \
         `turn` or a DIFFERENT holder's live lease (steals a lapsed one; same holder renews, over \
         the connection that renews); `here` keeps either to the asking connection: an acquire \
         refuses the same holder's live lease held over another one rather than move it, a \
         named release frees only the asker's own (`force`, or a lapsed lease, frees any), and \
         a lease taken `here` ends with its connection \
         (a holder that hangs up frees it at once). A live lease also blocks another connection's \
         `turn` from stomping it. COOPERATIVE by default: ADVISORY for raw send/key/feed (use \
         `turn` for HARD arbitration) — the coordination signal cooperating agents check before \
         driving. \
         `hard` makes it a HOLD: every OTHER connection's send/key/ctrl/feed/mouse/paste/turn \
         (and feed-bin) is refused `ERR busy lease=<holder>`, while the holder's own connection \
         writes on — its own `turn` runs under it and hands it back; `status` says hard=1. \
         `status` also says pid=<n>: the kernel's pid of the process that took the lease (the \
         peer of the direct connection its acquire came on, re-read at each renewal), never \
         the holder= text, which any client can spell; absent where no kernel named one (the \
         bridge, a relay). A person's keyboard is never blocked. A lease does not cross an aterm self-update: the \
         new instance starts with none, and its holder acquires it again there",
    )
    // Outside the halt: takes or reports the keyboard; it puts no bytes on a PTY.
    .input(Input::NONE),
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
         ACT: a leading act=approve|deny|answer (`send` and `key`; once, else ERR usage and \
         nothing is written) says what this input means for the approval box or question it \
         dismisses: the box's close in `log` reads out=approved|declined|answered only from the \
         harness, every other author's out=passed by=<it>; an input binds only the box open \
         when it was admitted (or the first to open within 500 ms, none closing between), and \
         one whose box closed before its bytes went writes its mark with re=<that box>. An \
         aterm older than it types act= as TEXT: send one only where `help send` names it. \
         Every write verb's input is noted in the session log as `mark - input verb= by=` once \
         per author's 30 s burst, never its bytes, with cause= and cause_k=mail|rec when it \
         names one. An acted input is marked every time, with act=, while its author's mark \
         tokens last (those `mark` spends: 1/s with bursts of 20; a burst's first input spends \
         none): one its author is short of is counted, not marked, and that author's next \
         acted mark says how many as dropped=<n>. The authors past the 16 named slots \
         (sessions, `-`, `aterm`, a vendor) share one bucket's tokens, not their counts: up to \
         32 of them keep their own; past those, the least recently counted one's count goes \
         to a shared pool, which one of them with no count of its own says as dropped=<n> \
         pool=shared, never as its own",
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
    )
    .lane(WaitWhenAimed)
    .input(DRIVEN.with(Input::STAMPS_SEQ).with(Input::SUPERVISED)),
    v(
        "key",
        Write,
        Status,
        Session,
        "key [id=<key>] [if=<re>] <name>: send a named key (enter/tab/up/...)",
        "— delivered beneath the window's shortcuts on every platform: `key ctrl+shift+t` \
         sends ^T to the shell and opens no tab, and no chord opens the find bar; the window's \
         own commands are `invoke <action>` (names via `controls menu`), `tab new` and `find \
         open`. Accepts a leading id=<epoch>:<producer>:<seq> idempotency key (see `send`), \
         and a leading act=approve|deny|answer naming what the press means for the box it \
         dismisses (see `send`'s ACT). \
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
         a lone signal character (key ctrl+c, ctrl c, feed 03, a one-byte feed-bin frame — \
         the tty's own intr, quit and susp) while the tty has ISIG on (a cbreak program) \
         and the kernel can take it now (no spill ahead of it, room in the queue), because \
         the line discipline turns it into a signal as it is written and it never queues — \
         in raw mode it is a byte like any other, refused into a stalled program, and \
         `signal int` is the interrupt. A leading \
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
        "(macOS only: a real NSEvent posted to this app; elsewhere it answers `ERR hwkey: \
         hardware-path injection is implemented on macOS only`), so it takes the SAME winit \
         path a physical keypress takes and carries the NSEvent-queue backdate. Use this — NOT \
         `key` — to measure typing latency: `key` posts straight to the main thread and is \
         born already dequeued, so it cannot see OS-level key queueing (the drawable park) \
         at all. Replies `OK posted=<n> key=<0|1>` — events handed to the OS queue, not \
         bytes written, and whether the window was the key window of the active app (only \
         there does the input method compose); read the result with `metrics percentiles` \
         (`n_key_write` moves only on this path). `shape=press` builds each event from a \
         CoreGraphics key event, as a key press is, so the layout gives its characters and \
         Option dead keys compose as typed. Refused like every input verb while input waits unread; a leading \
         unread=ok presses anyway. Refused (`ERR hwkey`) while a palette, Overview or tab \
         menu is open on the front window: its keys would press that card's rows as the \
         person's own (`help hold`)",
    )
    // `hwkey` posts a real NSEvent onto the OS event queue, the path a physical keypress
    // takes: a halt that stopped `send` and `key` and let it through would leave the one
    // route indistinguishable from fingers on the keyboard.
    .input(Input::PTY.with(Input::WRITES).with(Input::APP_UNREAD)),
    v(
        "feed-bin",
        Write,
        Status,
        Session,
        "length-prefixed raw bytes to the PTY",
        "`feed-bin <n> [id=<key>]` then <n> raw bytes. The optional idempotency key (see `send`) \
         rides the HEADER, not the payload: a frame whose key is refused still consumes its \
         announced bytes, so the stream stays framed and the next request line is the client's",
    )
    .body(Body::Counted { keyed: true })
    // A frame writes the shell's PTY, never a front overlay, so the front-surface fence does
    // not ask it; its reply carries no `seq=`.
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
        "(bracketed-paste guards as DEC 2004 stands when the frame is written + control-byte \
         sanitize + LF->CR); the binary twin of `paste`, framing contract included",
    )
    .lane(Wait)
    .body(Body::Counted { keyed: false })
    // As `feed-bin`, with paste semantics and no key.
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
        "pointer [move <r> <c>|move key=<k>|click|wheel dy=<n>|leave|status|mods <m>]: drive the POINTER",
        "— `mouse move` posts an engine `InputEvent` and never touches the window's pointer, so \
         nothing it does makes a link hover, a divider cursor or a tab-strip highlight happen. \
         This drives `App::on_cursor_moved` — the identical function `WindowEvent::CursorMoved` \
         calls — after mapping the cell to the centre pixel of the frame it is drawn in, and \
         `pointer leave` drives `on_cursor_left` (what `WindowEvent::CursorLeft` calls). On a \
         native view (Settings) `move key=<ui-key>` aims at that control's centre in the frame \
         presented (the centre of the part a press takes, where a list's edge cuts it), \
         `move xy=<x>,<y>` at a point in the view's logical coordinates; a window \
         with no glass (headless, no video take) commits the frame a capture of it shows first, \
         so the pointer lands on what `image` renders, and the reply is \
         `OK at=native view=<id> x=<x> y=<y> key=<ui-key|-> px=<x>,<y>` (`key=` the control a \
         press there takes: `-` over none, as over a switch still saving the change a click \
         made), or `ERR` where no \
         presented native frame is under the pointer; before replying that window commits the \
         frame again, and glass presents its next frame as its next redraw would, so the reply \
         reads the page its event left (`ERR the page under the pointer changed …` if that \
         present fails). Reply: \
         `OK at=<row>,<col> px=<x>,<y>` is where the pointer ACTUALLY resolved, read back from \
         the window AFTER the real path ran, in window (not pane-local) cells, and the window \
         pixel it is at — so it states where the pointer IS rather than repeating the request, \
         whoever put it there (a person's or a seat's pointer too). A NEGATIVE row is a chrome row above the \
         grid — `move -1 <c>` the one right above row 0, the message band's last row when one is \
         up — and reads back negative too. `click` presses and releases the left button where \
         the pointer is, through the functions `WindowEvent::MouseInput` calls (a band capsule, \
         a tab), each on macOS after the `CursorMoved` at the same pixel that AppKit sends \
         ahead of a press and a release; `ERR` before any move. `wheel dy=<n> [dx=<n>] \
         [unit=lines|pt] [key=<ui-key>|xy=<x>,<y>]` turns the wheel where the pointer is, or \
         where `key=`/`xy=` move it first, through the function `WindowEvent::MouseWheel` calls \
         (on macOS after the `CursorMoved` AppKit sends ahead of a wheel): `dy` down and `dx` \
         right, toward a list's end, in a wheel's lines (a fraction banks toward the next whole \
         one) or with `unit=pt` a trackpad's points, then the lift that ends its gesture; at \
         least one non-zero, each at most 10000; `ERR` with no position and no aim. An open \
         picker scrolls and a Settings list scrolls by its own step, as under a hand's wheel. \
         `click` is Owner-only, as `notice` is, and a click on \
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
    )
    // `click` presses whatever is under the pointer, every message-band capsule
    // included: owner-only like `notice act`. `mods` arms the modifiers the window
    // reads for keys and clicks (with `link` a plain click opens a link): owner-only
    // like the click it arms.
    .forms(&[form(AnyOf(&["click", "mods"])).owner_only()])
    // `pointer move` drives `App::on_cursor_moved`, the entry point winit's `CursorMoved`
    // calls, and under DEC 1000/1002/1003 the terminal reports that motion to the program.
    // It was once exempted from the halt as synthesizing no mouse event, which was false.
    .input(Input::PTY.with(Input::WRITES).with(Input::APP_UNREAD)),
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
    )
    .input(Input::PTY.with(Input::FENCES)),
    v("focus", Write, Status, Session, "send focus in/out", "")
    // `focus` writes the DEC 1004 focus reports (`\x1b[I`/`\x1b[O`) to the PTY when the
    // program asked for them, the sole focus-report egress, so the halt and the unread-input
    // gate ask it. The self-feed floor and the drive lease do not.
    .input(Input::PTY.with(Input::WRITES).with(Input::FRONT_DRIVING).with(Input::FENCES)),
    v(
        "scroll",
        Read,
        Status,
        Session,
        "scroll the view (up|down|top|bottom|N|prev/next-prompt)",
        "A bare `scroll` mutates nothing and reports the current position.",
    )
    // A driven scroll moves the cursor's coordinate space (the viewport), so it fences the
    // move licence; it reaches no PTY.
    .input(Input::FENCES),
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
    )
    .input(Input::PTY.with(Input::FENCES)),
    v(
        "restart",
        Signal,
        Status,
        Session,
        "restart [pid=<n>] [if-stalled] [quiet=<s>]: restart the tab's agent in place",
        "- the agent the window's harness supervises or watches in this tab (Claude Code, agy, \
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
    )
    // The explicit restart ends the agent in the tab: halted as `signal` is.
    .input(Input::PTY.with(Input::FENCES)),
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
    )
    // The manual reset moves the session's terminal modes out from under whatever runs
    // there, and `reset flush` drops its input queue: a driver's act, which a halt stops.
    // The human's escape hatch is the menu row, which is no socket verb (its `invoke` twin
    // is halted). It writes no input: it is the remedy for the stuck modes that filled the
    // queue.
    .input(Input::PTY),
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
    )
    // `tab close [N]` retires the tab's session (`ExitReason::CtlClose`), the act `close` is
    // halted for. The whole verb is halted, not its `close` form, because a set keyed on the
    // verb and a gate keyed on its arguments are the pair of literals that disagreed before.
    // A driver cannot navigate tabs while a halt stands; the human still can.
    .input(Input::PTY),
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
    )
    // `pane <dir>` puts no bytes anywhere, but it re-aims the seam that does: the focused
    // pane decides which session the window's keyboard and every flagless front-routed verb
    // drive, so a halted driver could redirect the next keystrokes, a person's included. The
    // human moves between panes from the keyboard as always.
    .input(Input::PTY),
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
             <target>` (`OK closed <target>`). `front` is always open; session-picker \
             opens only from its gesture (`invoke NewTabWithIdentity`); `window <target>` \
             photographs and `controls <target>` reads each one. While any session is held, \
             opening `menu`, `tab-menu`, `overview` or `autopilot-menu` answers `ERR halted` \
             as `invoke` does (see `hold`)"
        ),
    )
    // Outside the halt as a verb: opens a native tab app (settings, markdown, editor) or a
    // surface of the front window: no PTY, no session. Opening a card whose rows are acts
    // while a session is held is refused by its own gate (`fabric::open_halt_refusal`).
    .input(Input::NONE),
    v(
        "edit",
        Write,
        Status,
        App,
        "open a file in a fresh Editor tab beside the caller: @<sid> edit <file-uri> [line=<L>[:<C>]]",
        "[create=1] [for=<word>] [wait=0]. The window hosting `<sid>` opens a NEW Editor view of \
         the file in a tab right of `<sid>`'s, caret on line L (column C), and answers `OK edit \
         ticket=<t> view=<v> doc=<d> uri=<canonical-uri>` once it is installed; a view of the \
         file already open is never reused or focused. It takes focus when `<sid>`'s tab is the \
         front window's active tab, else it opens behind with the attention mark. Then wait \
         with `await edit ticket=<t>`. Until the ticket resolves (or its waiter is gone 5 s), \
         it holds `<sid>`: typing into `<sid>` answers `ERR busy editing` (see `status`'s \
         `editing=`). `wait=0` says nothing will wait (`amacs --no-wait`), so it holds nothing. \
         Owner scope only (`ERR scope`); `ERR unsupported \
         headless`, `ERR no-window` (no window hosts `<sid>`), `ERR refused \
         <not-regular|too-large|invalid-utf8|symlink|file-provider|missing|bad-uri|unreadable>`, \
         `ERR timeout ticket=<t>` after 30 s (the ticket is cancelled and a late open is \
         dropped). `create=1` (a missing file starts empty) is not served yet: `ERR \
         unsupported create`",
    )
    // It waits, up to 30 s, for the main thread to install the view.
    .lane(Wait)
    // Outside the halt: opens a new Editor view of a file in a tab beside the caller's and
    // answers once it is installed: no PTY, no session, as `open app editor` is.
    .input(Input::NONE),
    v(
        "act",
        Write,
        Status,
        App,
        "dispatch an exact semantic native-app action: act app/v1 view <view-id> <ui-key> <action>",
        "[value]",
    )
    // Outside the halt: dispatches a semantic action against a native app view, not the
    // grid.
    .input(Input::NONE),
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
        "answering `OK saved: <key> = <value>`, `OK removed: <key>; now <effective>` (an unset, or a set to the key's default) or `OK <key>: unchanged`",
    )
    // Outside the halt: rewrites `aterm.toml`, a greater authority than Write, but it puts
    // no bytes on a PTY and retires no session.
    .input(Input::NONE),
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
    )
    // `invoke Paste` writes the OS clipboard into the front tab's PTY
    // (`App::paste_clipboard` -> `deliver_paste`), and `invoke SelectAll` + `copy` chooses
    // those bytes off the session's own screen first, so the halt refuses every `invoke`.
    // Only an action whose `MenuAction::writes_pty_input` says so writes input; that needs
    // the argument, so the unread-input gate asks it beside this column.
    .input(Input::PTY.with(Input::APP_UNREAD)),
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
    )
    // `confirm yes` runs the close or quit an `invoke` parked in the window, retiring every
    // session it closes, so a question parked before the halt is not the one close a halted
    // driver can still finish. `confirm no` is refused with it: one verb, one answer.
    .input(Input::PTY),
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
    )
    // Outside the halt: a visual effect on the window.
    .input(Input::NONE),
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
    // Runtime-only, one-shot, edge-gated: ARMS a rainbow-kitty celebration
    // that fires on the session's next named edge (a green command block, a
    // keyed Enter, or the codex in front reporting OSC 7501 `done`) — latch,
    // don't act. Nothing durable is written and nothing lights until the edge;
    // `fx status` is the read face.
    v(
        "fx",
        Write,
        Status,
        App,
        "inspect or arm a one-shot rainbow-kitty celebration on the front session",
        "(fx [status|celebrate [sig=<char>] [bars=<1..4>] [on=green|enter|done]]; \
         `celebrate` LATCHES a sing-along that fires on that session's next exit-0 command \
         block — OSC 133/633 `D` status 0, the same Enter-armed verdict edge the pet and the \
         music box read — or, `on=enter`, on its next keyed Enter (a key event or raw bytes \
         ending in a newline; `send`/`turn` count, program output never does) — or, \
         `on=done`, on the next `done` the codex in front of it reports (OSC 7501 \
         `state=done`, as `status program_state=` shows it: only while the program in front \
         names `codex` or is a runtime that hosts one (npm's `node`, `bun`), read by the \
         status sweep, so a turn shorter than one sweep may not be seen; a `done` spends the \
         arm whether or not anyone is looking: it plays in the arming window over whichever \
         tab is in front, and an unfocused window draws its first frame only, with no later \
         bar and no sound). It runs `bars` \
         1.6 s bars (default 2, max 4): the spine driven to full, a fan of 5/7/9/11 stars per \
         bar, then a drop fan and a beat-quantised \
         outro on do so the song ends on the bar line. `sig=` picks the key by character \
         (default `C`; every character is a distinct verse). Refused while an arm is pending, \
         inside the 30 s cooldown of the last arm, or when the cursor trail is disabled, \
         uses another style, or Serious Mode suppresses companions. Every form answers the same status line: `armed=none|green|enter|done sig= bars= \
         live= bar= drive= cooldown_ms= style=` — `live` is a fired run on the stage, `bar` \
         its current bar, `cooldown_ms` the wait before the next arm is admitted)",
    )
    // Outside the halt: arms or reports a window effect; it puts no bytes on a PTY and
    // cannot synthesize the key or command completion that fires it.
    .input(Input::NONE),
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
    )
    // Outside the halt: chooses which collected cat rides the cursor: a look, like `rain`
    // and `fx`. It writes the machine-owned toy ledger, which is why it is Write, but a
    // halted agent changing the cat changes nothing the halted program can see.
    .input(Input::NONE),
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
         cells the ribbon retracted), `motion` (Classic only: the classic \
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
         credits= swallowed_no_echo= park_returns= park_flushed= issue= comet_cells= \
         comet_alpha_max=` (every gate \
         from the config knob to the glass, in the order the frame path walks them, plus \
         the cumulative tally the ring has forgotten — `licensed=0 declined>0` blames the \
         licence and names why, `licensed>0` over a dark screen blames everything \
         downstream of it). `resolved=` is the style the spelling resolved to: a typo reads \
         the default it fell back to, and `off` reads `off`; `issue=` says whether the \
         spelling was honoured as written (`unknown` or `none`). `ribbon_look=` is `none` on every \
         style but rainbow kitty, which alone draws the ribbon. `comet_cells=` / \
         `comet_alpha_max=` are the comet's opaque CELL BED on the last tick — how many \
         cells it laid and the brightest one's coverage byte (0-255, after load shedding, \
         the brightness dial and the bed's legibility share under the letters): the bed is not aurora light, so `glow_active=false` \
         beside `comet_cells>0` is a bed still fading on the glass. `sound_seam=` is the KEY-TIME CLICK, adjudicated \
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
    // Outside the halt: toggles the drop-target highlight.
    .input(Input::NONE),
    // session lifecycle
    v(
        "spawn",
        Write,
        Status,
        App,
        "spawn [window=] [raise=<t|f>] [cwd=] [split=<v|h>] [identity=] [connected=…|agent=…]: OK <sid>",
        "mints a session: a tab, or `split=v|h` divides the focused pane; the sid is immediately \
         addressable. `theme=<name>` gives the newborn its own theme (`theme session`), checked \
         first: one that does not draw is `ERR theme \"<name>\": <why>` and nothing is spawned. AIM it with window=<id> (an id from `inspect app/v1 tabs`) or `@<sid> \
         spawn` (the window hosting <sid>; window= wins); split= then divides THAT window's \
         focused pane. raise= defaults to true when no window was named (the `aterm new-tab` \
         attach contract) and FALSE when one was - an agent aiming at a background window is \
         not asking to see it; say raise=true to insist. A tab spawned without the raise keeps \
         its window's active tab: the newborn opens behind it, so the person's view stays put. \
         Unknown id: `ERR no such window <id>`. \
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
         credentials). `identities` lists and forgets them. A WORKER: `spawn … agent=claude \
         [model=<m>] [autopilot=<land|cruise>] [from=<sid>] len=<n>`, answered `GO <n>`, then <n> raw bytes, its task \
         (`aterm ctl spawn agent=claude [model=<m>] [cwd=<p>] -` reads the task from stdin, \
         names the folder it runs in as cwd=, or makes a relative cwd= absolute against it \
         (a folder with a space goes quoted), and - run in a session - names that session \
         from= and, with no \
         selector, --sock or --pid, aims at it, `@self`) - Owner-only. The task is sent only \
         behind the GO, so an instance that predates workers never reads it; one aimed at a \
         session another instance hosts is refused, unsent. The newborn runs the ordinary \
         login shell (zsh, bash or fish; another is refused by name). The task \
         - 1 byte to 64 KiB, no NUL; past that the reply is `ERR spawn agent=claude: the task \
         is <n> bytes (max 65536)` and the connection closes - is written with one line of \
         fact appended to a 0600 file under `<state>/tasks/` that the shell's ATERM_TASK_FILE \
         names, and at the shell's first prompt aterm types ONE line, `claude [--model <m>] \
         -- \"$(command cat -- \"$ATERM_TASK_FILE\")\"`: the task is the agent's first prompt, \
         no byte of it is typed, and the shell stays under the agent (the pane outlives it). \
         Before the reply the newborn's `task` record reads `working`, from= the session that \
         started it, however it is aimed (none: `ctl`); a shell that ends or draws no first \
         prompt within 20 s, or a fish older than 3.4 (spawned, then checked), is `stalled \
         why=shell`, with nothing typed (an update during that wait leaves it `working`, \
         nothing typed, for the startup deadline below). A worker whose record still reads \
         `working` as its starter left it and that shows no Claude Code composer within 120 s \
         of its since_ms= (counted, after a seamless update or with supervision back on, from \
         no sooner than the harness's first look since) is `stalled why=startup` by the window \
         harness, what holds its screen named in the supervisor's attention in the same move (a \
         folder-trust dialog, a box, a login or onboarding screen, no agent at all); one that \
         starts after all is marked `working` again, and that attention goes when it starts or \
         another mark (its done, its exit) ends the stall. A started worker held 120 s (the same \
         clock) at a screen the harness has no act for, nothing moving, is `stalled why=held` \
         likewise (`task` says when). When the starter is a session here running \
         Claude Code, its claude program rides the line, and so do its Claude home and login \
         (CLAUDE_CONFIG_DIR, CLAUDE_SECURESTORAGE_CONFIG_DIR; aterm strips them from every \
         shell it spawns) when the worker runs under the starter's own identity; under another \
         (identity=<name>, identity=-, or an aimed session's) only the program rides, and the \
         home in the worker's own shell stands (that identity's, or the default one). The \
         window harness answers Claude Code's folder-trust dialog Yes in the cwd= named, for \
         this worker alone (its canonical form; one not read within 250 ms — or not read at \
         all while four folder checks, shells starting on a hung mount among them, are still \
         waiting — trusts nothing), but that Yes is Claude Code's own answer and Claude Code \
         keeps it for the folder (`hasTrustDialogAccepted` in its `.claude.json`, `~/` or the \
         worker's CLAUDE_CONFIG_DIR): every later Claude Code started there under that login \
         is trusted with no dialog; \
         `[harness] trust_roots` is not widened. `ERR unsupervised: <switch>` \
         when the window harness would not carry the worker, so nothing would finish the \
         task, and nothing is spawned: `[harness] enabled = false` (or `headless = false` in a \
         headless instance), `[harness.claude] enabled = false`, or `[harness] autopilot = \
         \"off\"` (the owner's default keeps the harness's hands off). A worker's session \
         gets the Autopilot word `land` (`autopilot=cruise` names cruise): Land carries it \
         until all of its work is on the remote main branch and a reply defends that in a \
         few lines starting `Nothing left:`, which is its `done`; a Cruise worker is carried \
         for ever and never reads `done` (`meta set autopilot land` turns it to land); a worker that \
         cannot start once spawned, or whose reply the server's 30 s main-thread deadline \
         dropped, is closed again, its task file removed, with nothing typed - a client that \
         gives up sooner (its own --timeout, killed) is not covered: its worker runs, and the \
         same task sent again runs twice; a worker's task file is removed when its session \
         closes or aterm quits (a seamless update keeps it, and so does an update's candidate \
         that quits before its Commit, for the parent it hands back to; a crash, and an update that \
         relaunches the new build in place — its tabs back as fresh shells — leave it under \
         `<state>/tasks/`); `agent=codex` is `ERR agent codex: not yet`; model= is \
         [A-Za-z0-9._-]{1,64}",
    )
    // Outside the halt: MINTS a session rather than driving or retiring one. The new session
    // is held the moment the bridge attaches it (`apply_fleet_halts`), and halting `spawn`
    // would refuse a human's `aterm new-tab`, which arrives on this seam indistinguishable
    // from a driver's. Residual: between the spawn and that attach the new session is
    // ungoverned.
    .input(Input::NONE),
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
    )
    .input(Input::PTY),
    // `task` reads a worker's task record; the `task mark` sub-form WRITES it
    // and is Owner-only by its form (a manager acts on the record, so no edge
    // may forge one).
    v(
        "task",
        Read,
        Status,
        Session,
        "task [mark <state> …]: a worker's task record: OK task rev= state= from= since_ms=",
        "result_bytes= [why=] [exit=] [result_path=] agent= attention= (read fields by key), or \
         `OK task -` for a session with no task. state: `working` (the agent has it), `done` \
         (the worker's work LANDED: under Land, a land act's reply defended in a few lines \
         starting `Nothing left:` that all of it is on the remote main branch, naming no open \
         work and no need, over a repository `git` finds holding nothing the remote main \
         lacks; a done reply alone, or the retired `ATERM-TASK-DONE`, is none, and a Cruise \
         worker is never done), `exited` (the agent is gone and no relaunch follows; \
         exit= its code, where its shell's block recorded one) or `stalled` (it never showed \
         its composer: why=startup 120 s after its task's start, what holds its screen named \
         in attention=, marked `working` again where it starts after all; why=shell, its \
         shell drew no first prompt; why=held, started, then 120 s at a screen the harness has \
         no act for with nothing moving, what it shows in attention=, `working why=moved` once \
         it moves). A \
         question or a wall is no task state: agent= and attention= are the session's own, as \
         `sessions` prints them. from= the global sid that started the task, or `ctl`; \
         since_ms= since the state was entered; why= one of landed|exit|shell|startup|held|\
         turn|moved (`marker` and `done-check`, which no build writes now, still read in a \
         record an older build carried); result_path= (pct-encoded) the file under `<state>/tasks/` \
         the result is in. rev= moves on every change of the record, and when the \
         supervisor's attention (`meta set attention owner=supervisor`) or the worker's \
         needs (`owner=needs`, its `NEEDS YOU:` lines) are raised or cleared \
         on the session: the one edge `await task` waits on. `status` and `sessions` carry \
         `task_state=<state|-> task_rev=<n>`; `subscribe … events` pushes `EVENT <local> task \
         rev=<n> state=<s> attention=<0|1> [why=<w>] [exit=<n>]`; a seamless update carries \
         the record. `task \
         mark <working|done|exited|stalled> [from=<s-…|ctl>] [why=<word>] [exit=<n>] \
         [result_bytes=<n>] [result_path=<pct>]` (Owner-only; an edge gets `ERR denied`; \
         result_path= at most 1024 bytes decoded) sets it, and answers the record after it \
         with ` changed=<0|1>`. Any Owner-token holder may mark; the window harness is the \
         writer of `done` (its result snapshotted under `<state>/tasks/`, read with `result`) \
         and `exited`. The window harness reads the record when its loop starts: on a \
         worker marked `done why=landed` under Land it takes that landing on (no land act or \
         continuation is typed into it until someone else's turn), except at a wall, which ended someone else's turn \
         after the done and is acted on, the record marked `working why=turn`; a `done` an \
         older build marked (`why=marker|done-check`) is taken on as nothing; and a worker's \
         land acts, cruise prompts and answers go without the short-turn back-off for its \
         first 3 short turns in a row (its manager waits). A `turn` that anyone but the harness types into a \
         `done` worker moves it back to `working why=turn` at once — the done its Enter found, \
         whose rev the turn's verdict names (task_rev=), that done marked again since included \
         (a `result`'s lag repair, an attention), never one entered since; a later done the \
         harness reads on someone else's `send`, `paste` \
         or keyboard turn is marked `working why=turn`, then `done` with its own result. A \
         mark is the WHOLE new state (a fact it leaves out is cleared; \
         from= is kept when not given); the same state with the same facts changes nothing \
         (`changed=0`, no rev), so a writer marking `done` again after a restart wakes no \
         one. A mark that arrives while a seamless update commits the session waits: it lands \
         here once the update stands down, goes unanswered when the update lands (send it \
         again; the successor answers), and past 10 s answers `ERR busy update-committing` \
         with nothing moved",
    )
    // `task mark` writes the record a manager acts on: owner-only, like `meta set
    // supervisor`; bare `task` reads.
    .forms(&[form(Word("mark")).owner_only()]),
    // `result` reads a worker's result: the snapshot its task record names.
    v(
        "result",
        Read,
        Lines,
        Session,
        "result [full]: a worker's result, the report it ended its task with",
        "(DESIGN-sessions-as-subagents §2.3): snapshotted from its Claude Code transcript by the \
         window harness as the task was marked `done` — once the end row is written of the \
         turn the done was read on (the newest a prompt or a background command's \
         `<task-notification>` began before the harness read the done on the screen; never one \
         begun by a row written after that read), \
         waited for up to 5 s, a turn that writes no end row ending where the next one starts \
         — and never read again for that done (a turn typed into the done \
         worker cannot change it; a later done gets its own), save one case: a snapshot that \
         found that turn's reply not yet written (`no-reply`, `no-marker`) is read again by \
         each `result` while the miss stands, for THAT turn alone, and a reply found there is \
         the result from then on (the record marked again with its result_bytes=: one new rev, \
         which wakes an `await task` once). Answers `OK <n> source=transcript session=<id> \
         uuid=<row> bytes=<n>` + the text as n lines: at most 24,000 characters, then a last \
         line `… truncated; full: <path>` (its file under `<state>/tasks/`, removed with the \
         worker's session: read it before closing the worker). Landed, the result is the \
         final message of the turn before the land act, then that of the land act's reply, its \
         `Nothing left:` defense (a reply not yet ended on one is `no-reply`); a line that is \
         only the retired `ATERM-TASK-DONE` is dropped wherever it stands (an older build's \
         done by that marker or its done check, carried, reads as that build read it). `result \
         full`: the whole exchange since the task's user row, read from the transcript now \
         (each prompt as `> <text>`, the harness's own continuations and land acts among them), \
         its last 24,000 characters, led by a line naming the transcript where earlier turns \
         are cut; ` full=1` on the header. `ERR no task` for a session with no task; `ERR \
         result unavailable why=<word>`: the task's state while it has no result (`working`, \
         `stalled`, `exited`); why the harness read none (`no-transcript`, `no-reply`, \
         `no-marker`, `empty`, `no-check`, `no-report`), or none was written (`no-snapshot`); \
         a result file that cannot be read (`unreadable`); for `full`, a transcript that holds \
         no prompt (`no-prompt`) — never a screen read in its place. `aterm ctl` prints the \
         header on stderr and the text on stdout",
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
    )
    .lane(Wait)
    .json(Json::Refused),
    v(
        "await",
        Read,
        Status,
        Session,
        "await <idle|seq|match|gone|block|inbox|consent|momentum|agent|task|log|sys> … [timeout=]: wait",
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
         command completed), `await sys <free>=<size>|free>=room|disk=<word>[,…]> [and <term>] \
         [vol=<path>]` — THE MACHINE: free space on the volume of the session's working \
         directory (a <size> needs its unit: B, KB, MB, GB, TB, KiB, MiB, GiB, TiB; `room` is \
         `status disk_room_mb=`), or its level (ok, low, short, emergency); latched, so a \
         condition already true answers at once; a `free` wait below its need registers a \
         DEMAND, and the disk watch frees build caches nobody uses (tiers 1-3, never a recent \
         cache) until it is met: `OK sys free_bytes= disk= vol= freed_bytes= held_ms=0 as_of=`, \
         `ERR unmet free_bytes= need_bytes= vol= freed_bytes= why=<spent|held-back|cleanup-off|\
         settings|not-reclaimed> hogs= largest_bytes=` (what is left is not aterm's to free), \
         `ERR unavailable free why=<unsupported|remote|automounted|no-mount|unreadable|\
         watch-stalled|too-large|no-watch|room-unknown>` (automounted: a mount the \
         automounter made, which a read could mount or block on; no-mount: no volume of the \
         mount table holds the path — a drive letter with no volume behind it, a disk \
         unplugged; no-watch: `[harness]` is off, so no watch would free it; room-unknown: \
         `room` on a volume whose room is not measured), \
         `OK timeout` (exit 124: run it again); an edge-scoped caller gets the wait with no \
         demand; `await momentum <floor 0..=1>` — THE YIELD: latch once the typing \
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
         omitted. `await inbox since=<id> topic=<t>` narrows it to ONE CHANNEL: a broadcast \
         row on that topic (it reached this session because it ran `topic add <t>`, and \
         carries `topic=<t>`). A broadcast is usually a `note`; naming the topic accepts every \
         kind on it, so no `kinds=note` is needed; an explicit `kinds=` still narrows it, and a \
         topic outside `[a-z0-9][a-z0-9._-]{0,31}` (the `post to=say:<t>` grammar) is \
         `ERR usage`. It sleeps through addressed mail, which a bare `await inbox` or the next \
         `inbox` still finds. Nothing aterm types wakes an agent for mail: this wait, run as a \
         background command, is how a session chooses to be pushed to — its exit is the wake. \
         The client's `aterm ctl await inbox …` re-arms it past the server's bound with the \
         same since= until its own timeout= (else --timeout, else 3600 s), so it exits at news, \
         or with `OK timeout` (exit 124) at that deadline. \
         `await agent <word>[,<word>…] [since=<rev>]` — the SERVER'S agent verdict (`status \
         agent=`, words busy|prompt|question|idle|survey|unknown|-, and `wall` for any wall or \
         `wall:<kind>` for one): LATCHED, so a verdict already in \
         the set answers at once, else it parks until the status sweep publishes one (no \
         watcher armed, no screen read by the waiter); with since=, only a verdict whose rev \
         is past it answers — a turn's agent_rev=, so the idle from before its submit, which \
         the sweep (at most 4 a second) may not have republished when a settle=submitted turn \
         answers, is no answer. Answers `OK agent <word> rev=<n>`, and an unknown word or a \
         since= that is no rev is `ERR usage`; the client re-arms it as `await inbox`. \
         `await task [since=<rev>]` — the session's TASK \
         RECORD (`task`): EDGE-TRIGGERED on its rev, it answers once the rev is past since= \
         (default: the rev when the wait is armed, so news already read never answers twice; \
         since=0 takes news from before the wait) and the record has news — state done (its \
         work landed), exited or stalled (a stall's attention= rides its own rev: why=startup never \
         started, why=held started and stuck where nothing moves), or the supervisor's \
         attention or the worker's `NEEDS YOU:` needs raised. `await task \
         @<sel>[:<rev>][,...] [any|all] [since=<rev>]`, with NO leading selector, waits on \
         sessions of this instance (one that does not resolve fails the request) until the \
         first has news (`any`, the default) or every one has (`all`); `@<sel>:<rev>` gives \
         that target its own since=. `task` is the first word after `await` in both forms. \
         A target that showed news keeps it, and its row, until the wait answers; a wait \
         armed again reads every record as it stands then. Either form answers `OK <n>` then \
         one row per target in request order (a session named twice answers twice), \
         `@<sid> rev=<n> state=<working|done|exited|stalled|-|closed> agent=<word> \
         attention=<pct|-> result_bytes=<n>` (`-`: no task; closed: its session closed while \
         waited on), and at the deadline the same rows under `OK <n> status=timeout`. The \
         client's `aterm ctl await task @a,@b,...` takes sessions of every instance on this \
         machine: it asks each hosting instance at once, combines any/all, re-arms past the \
         600 s bound with each target's rev from the start until its own --timeout (default \
         3600 s for this form), follows a self-update, keeps a worker whose session closed \
         (or that the update's successor does not host) as its `closed` row, prints the \
         rows, and exits 124 on a timeout. With --sock/--pid every target is that one \
         instance's: a sibling's sid there is `ERR no such session`. And \
         `await consent` \
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
         hold=0|1`. Every form but `await task` answers `ERR exited` once its session exits or \
         is closed, and \
         `OK timeout` otherwise, after 30 s by \
         default (`await consent` 300 s; max 600 s), which the client exits 124 on. \
         Each parked wait holds one of the 64 wait slots every tab shares (past them, one of 4 \
         request slots; then `ERR control server busy`), so park at most one per driver. \
         `await log need|open=<lvl>|close=<lvl>|kind=<k> [since=<seq> ep=<e>]` parks on the \
         SESSION LOG (<lvl> session|run|turn|step; `need` is a record that raises one: an \
         approval, question, wall or stall step's open, a run closed `out=fail`, the session's \
         close, a `need` mark, whose claim shows `conf=claim`; a comma list of up to 4, \
         `need,close=turn`, is one wait that any member answers, and a list holding `need` \
         latches as `need` does) and answers `OK seq=<n> ep=<e> \
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
         `await log close=turn`; for a prompt to answer, `await log need since=<seq> ep=<e>`. \
         `await edit ticket=<t>|uri=<file-uri> [timeout=<ms>]` waits on an `edit` ticket: \
         `OK edit done result=<finished|closed|aborted> saved=<0|1> seq=<n>` once its view is \
         gone (a dirty close only after the save is durable; `saved=1` when a save landed during \
         the edit and covers the view's last text, `seq=` the durable checkpoint), never because \
         another view of the file closed; `uri=` (any spelling of the file: symlinks are \
         resolved) waits until no view an `edit` opened of the file remains, or, where this \
         instance issued no ticket for it (a successor after an update), until no Editor view \
         of it remains. Owner scope only (`ERR scope`). `ERR unknown ticket` for a ticket this \
         instance never issued or has forgotten (it keeps the last 64), `OK timeout` (re-arm), \
         `ERR hangup`",
    )
    .lane(Wait)
    // `await task` answers `OK <n>` + one row per target (its timeout too), not the one
    // status line every other `await` answers.
    .forms(&[form(Word("task")).framing(Lines)]),
    v(
        "wait",
        Read,
        Status,
        Session,
        "wait [<ms>]: block until the running command completes (OSC 133)",
        "Answers `OK complete <id> exit=<code|->`; with nothing running it returns the newest \
         completed block at once. `ERR exited` once the session exits or is closed; `OK timeout` \
         after 30 s by default (max 600 s); without shell integration it can only time out or end.",
    )
    .lane(Wait)
    .json(Json::Refused),
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
         are metadata, never a body), `EVENT <local> turn <id> submitted=<0|1> status=<s> \
         dur_ms=<n> by=<who>` per ledger record (`history`'s `by=`: `harness` is the window \
         harness's own), `EVENT <local> agent <word> rev=<n> gen=<e.s> fp=<hex16>` each \
         time the server's agent \
         verdict moves (`status agent=`), `EVENT <local> task rev=<n> state=<s> \
         attention=<0|1> [why=<w>] [exit=<n>]` each time a worker's task record moves \
         (`task`), `EVENT <local> status phase=<p> outcome=<o> \
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
         reader that must know what it does not watch goes by the acks it holds; `EVENT * \
         fabric-retire <sid>` for a `fabric retire` request the bridge is to act on; and `EVENT * \
         bye <quit|handoff>` as the instance quits or commits a seamless update, the word its \
         bridge withdraws its sessions by, or not) and is OWNER-ONLY \
         because it reports \
         the whole roster, not just your targets — a scoped edge asking for it gets `ERR denied`; \
         add `timestamps` (alias `ts`) INSIDE <streams> (`cells,ts`; trailing is `ERR unknown \
         subscribe arg`) to prefix frames with `T <local|*> <t_us>` lines (video's clock) so the \
         stream is a timed frame source — at most one per channel per wake, tagged `<local>` for \
         session frames and `*` for `sessions` events, so the second token is not always numeric; \
         each `T` is when the frames under it were read (a `BYTES` burst's arrival), so on a \
         channel that mixes `bytes` with content a later `T` can be smaller: never assume `T` \
         increases; \
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
         `ReadScreen`. Each subscription holds one of the instance's subscription lanes until \
         it ends, apart from the 64 wait slots: a sixteenth of its descriptor limit (16 at the \
         usual 256, never fewer than 4); past them it is answered `ERR subscription capacity \
         busy; retry`, so follow many sessions with one comma list or `@*`",
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
         pushed on `subscribe … mail` with `topic=<t>` too; `await inbox since=<id> topic=<t>` \
         latches on one. OWNER, not Read: adding a topic \
         changes what reaches an agent's inbox, which is the halt's authority class rather \
         than a read's. The set survives a bridge restart and a seamless update, and so do its \
         unhandled broadcasts: the bridge keeps each topic's oldest one the session has not \
         marked handled, and a relaunch that keeps the session offers the topic again from \
         there, in order, each row once. A session \
         opts into at most 32 topics: an `add` of a new one past that answers \
         `ERR topics full (max 32): drop one first` and changes nothing (every opt-in rides \
         the session across an update, which carries a bounded record).",
    )
    // One status line each, not the listing's rows: read as Lines, the topic
    // would be taken for a row count.
    .forms(&[form(AnyOf(&["add", "drop"])).framing(Status)])
    // Outside the halt: arranges future deliveries into this session's inbox ring, and a
    // delivery is already something a halt does not stop (`deliver` and `post` are exempt).
    // A topic reaches no PTY, moves no keyboard and re-aims no seam that writes; refusing it
    // would only stop an operator arming a held session to hear the all-clear.
    .input(Input::NONE),
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
    )
    // Outside the halt: writes a one-use key file and authorizes the key for a degraded tab.
    // The line that reads the file is a `turn`, which the halt refuses, and a key whose line
    // never runs is taken back with its file.
    .input(Input::NONE),
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
    )
    // `inbox get @<off>` parks for the bridge's next drain.
    .forms(&[form(AtWord).lane(Wait)]),
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
    )
    // Outside the halt: records a decision; §5.3 names it exempt.
    .input(Input::NONE),
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
         subjects) — is the bridge RETIRING the post, and the wait is released with that \
         verdict rather than a timeout. `unroutable` includes an address whose presence row \
         says `exited` — the session ended, or its instance quit and withdrew it — even one \
         this bridge routed to before; a session whose NODE reads `away` (its bridge went \
         silent: killed, crashed, its link lost) still routes, and the post waits on the bus \
         for the node. `outbox` then omits the row (it lists only rows that are \
         not dead), so no bridge drains it again: this one is reported as its reason, and \
         re-posting is right once the address is. The body is inline text up to 4 KiB, or `len=<n>` followed \
         by that many raw bytes up to 256 KiB. There is no `to=fleet`: a node holds no fleet \
         write grant, and an agent may only ASK a human to halt. REFUSED to an edge-token \
         connection — a write-input edge over one session would otherwise speak AS that session, \
         and the sender of a post must be attested by the instance Owner. Exempt from `hold`.",
    )
    .lane(Wait)
    .body(Body::PostLen)
    // Outside the halt: the escalation path: an agent may always ask a human to lift the
    // halt.
    .input(Input::NONE),
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
    )
    // Outside the halt: the notice explaining a halt arrives on this verb; halting the
    // bridge plane would halt the only party that can lift a halt.
    .input(Input::NONE),
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
        "link up rtt=<ms> | down reason=<token> [rtt=<ms>] | log <id> | bye <quit|handoff>: the link",
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
         log switch (bus.log replaced, repaired or restored, or another broker dialled), and every \
         session forgets the offsets it holds — its `off=` dedup, the `inbox get @<off>` reads it \
         answered (one still parked answers `ERR fetch failed reason=log-replaced`) and the \
         offset threading of the rows and posts it already holds — answering `OK forgot=1`; \
         otherwise `OK`; a log switch also starts the bus mirror's cursors over. `continues=<off> \
         was=<id>` (lane 3, said only to an instance whose `hello` named it) makes it a MOVED \
         GENERATION of `was` — a power loss under a weak tier, a poisoned log's reopen, a \
         repair's cut — that names the same records below `<off>`: a session on `was` forgets \
         only what names an offset at or above it (a queued reply, an owed receipt, a parked \
         read, a dedup entry, a row's threading) and keeps the rest; a session on any other log \
         forgets everything. `was=` must be an earlier generation of the same log. `mirror \
         on|stopped|capped [reason=<token>]` is the bus mirror's state (session log phase 4, \
         only from a bridge whose `hello` said `log=turns`), which `fabric status` prints as \
         `mirror=` beside `bridge_log=turns`. `link bye <quit|handoff>` is the bridge \
         TAKING the instance's last word, the `EVENT * bye <word>` the instance pushes before \
         it closes the lanes — `quit` at the end of a quit, `handoff` at a seamless update's \
         Commit: a quit waits for this ack (1 s at most) and retires the bridge supervisor, \
         and the bridge then WITHDRAWS every session it advertised, one `state=exited` \
         presence row each, so a post to one is refused at its sender (`ERR unroutable`); on \
         `handoff` it withdraws none, because the successor re-attaches the same sids. \
         Answered like a report: `OK`, or `OK stale=1` from a ghost lane. `ERR usage` for \
         anything else.",
    )
    // Outside the halt: the bridge reporting its own broker link; it puts no bytes on a PTY,
    // and a halt that silenced it would hide the one fact that says why mail is not moving.
    .input(Input::NONE),
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
    )
    // Outside the halt: the bridge naming its lane grammar and build; it reaches no session,
    // and refusing it would leave the instance unable to say which bridge serves it.
    .input(Input::NONE),
    va(
        "hold",
        Write,
        Status,
        Meta,
        OwnerOnly,
        "hold <sid> on|off [reason=<pct>] [origin=fleet|local]: the drive halt, local or fleet",
        concat!(
            "`OK hold=<0|1>`: whether the session is held AFTER the act. An act on the owner's \
             slot (`origin=local`) is answered once the crash journal's image that carries it is \
             on disk where a desk is journaled (`restore_session`, a windowed unix launch), so a \
             kill sent at the reply cannot lose it — past 5 s with `journaled=0` beside it (the \
             hold stands, no image on disk carries it yet); an act on the fleet's slot is answered \
             at once (the fleet's halt is kept on the bus, and every attach applies it again). TWO \
             ORIGINS, EACH ITS OWN SLOT, AND THE CONNECTION CHOOSES: a session holds a set of origins, `on` sets the \
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
             cannot lift is the fleet's. A THIRD SLOT IS THE PERSON'S (`origin=person`): Take Over \
             at the window (Agents \u{25b8} Take Over This Tab, the tab's menu, `t` in Overview) sets \
             it with `reason=taken-over` and Hand Back lifts it, and nothing on the wire can — \
             `origin=person` from any connection is `ERR usage`, an Owner or bridge `off` lifts \
             only its own slot and answers `OK hold=1` while the person's stands, and `invoke \
             TakeOver|HandBack` is `ERR denied`. Where no person's hold stands, the person's Hand \
             Back at the window lifts the owner's local slot as an Owner `off` would (it confers \
             nothing an Owner client lacks), never the fleet's. A FOURTH SLOT IS ANOTHER MAC'S \
             (`origin=remote`, the step-in): `hold <sid> on|off` over the fleet's drive op \
             (`aterm ctl --host <mac> hold <sid> on`, from a node this Mac's `aterm fleet allow \
             drive --from` names) sets and lifts it for the node that proved itself, and no \
             local connection names it (`origin=remote` is `ERR usage`). While it stands every \
             write to the session but that node's own answers `ERR halted … origin=remote`; it \
             is refused while the person's hold stands or another node's does, the person's Take \
             Over takes it away, their Hand Back lifts it where no person's or client's hold \
             stands, and no update carries it. An edge token is `ERR denied` \
             either way, and a selector is rejected: the session is the argument. While on, every \
             PTY-reaching verb resolving to that session answers `ERR halted reason=<r> \
             origin=<person|remote|fleet|local>` (the person's when it stands, else a remote node's, \
             else the fleet's, the one the session's own agent cannot lift, else the owner's) from \
             ANY scope — `send key ctrl feed feed-bin paste paste-bin mouse \
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
             `pointer`, `tab` and `confirm` resolve no session, and neither does `askpass` (sudo's \
             password relay, which `aterm-askpass` asks for whatever tab ran `sudo`), so they are \
             refused while ANY session on the instance is \
             held — including the aimed `@<sid> tab …` form, which drives the window HOSTING that \
             session and therefore answers `ERR halted` whenever any session of that instance is \
             held, not only the one it names. So is `open` of a card whose rows are acts — `open \
             menu|palette`, `open tab-menu`, `open autopilot-menu`, `open overview|connections` \
             (closing one stays answerable) — since each is `invoke`'s rows behind a key. That instance-wide halt is \
             deliberate under a person's Take Over: it is what keeps an agent from `hwkey`ing a \
             menu shortcut, `pointer`-clicking Hand Back or opening a card to press it, and its \
             cost, in the words Take Over's tool tip and the menu bar's You hold section use: \"",
            hold_cost!(),
            "\" `spawn`, `survey`, `await`, `result` and typing into other tabs still work. A card already open when the hold began is guarded by whose gesture \
             it is: a control client's `key`, `text` or `pointer` into it cannot press Take Over or \
             Hand Back, nor stop a held tab's worker (`ERR halted`, as `signal` would answer). \
             `hwkey` posts real keys that carry no such mark, so it answers `ERR hwkey` while a \
             palette, Overview or tab menu is open on its window, hold or no hold. \
             `post`, `inbox seen`, \
             `meta set`, `lease` and every read verb stay answerable, and the physical keyboard is untouched: a \
             halt stops drivers, not humans. When the bridge connection closes, a hold that stands \
             stays, and every other session that bridge ever delivered to or held KEEPS RUNNING with \
             `status hold_advisory=fabric-lost`; under `[fabric] \
             hold_on_bridge_lost = true` the instance holds each of them with `reason=fabric-lost \
             origin=fleet` instead — the halt must not depend on a killable process staying alive — \
             and, being fleet-origin, only a reconnecting bridge's `hold off` lifts it, leaving an \
             owner's local hold beside it standing. An \
             Owner-issued hold does not make a session bridge-governed: a bridge dying afterwards \
             touches what IT governed, not what the owner did. Every hold, every origin, SURVIVES a seamless update: \
             the new process puts each back in its slot, reason and origin as they stood, before the adopted \
             session answers a single verb (its `timeline` row says `carried=1`), and a hold that \
             moves while the update is taking over makes that attempt stand down, every session \
             left as it was (an automatic update tries again later), rather than be lost. In the \
             last instant of a takeover — once the update has compared the holds and is handing \
             the session over — a `hold` waits for it instead: it is answered as usual if the \
             attempt stands down, the connection closes with no answer if the new process took \
             over (retry: the new process answers), and past 10 s it is `ERR busy \
             update-committing` with nothing moved. Never `OK` for a hold the new process did not \
             adopt. A quit ends the sessions, so a relaunch after one carries nothing (a deliberate \
             restart hands a taken-over tab back); a crash the PTY \
             keeper recovers a session from carries its holds as an update does, as the session's \
             last META delivered to the keeper named it. The pump attempts a sample every quarter \
             second; a busy lock, a full queue or a failed link can leave an older sample, so a \
             hold changed since that sample may not survive the crash."
        ),
    )
    // Outside the halt: the verb that lifts the halt.
    .input(Input::NONE),
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
    )
    // Outside the halt: retires an outbound post the bridge already published; refusing it
    // would strand the body and re-publish it forever.
    .input(Input::NONE),
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
    )
    // Outside the halt: moves the bus mirror's cursors and writes nothing to a PTY; a
    // halt that stopped it would stall the mirror, not a driver.
    .input(Input::NONE),
    // sessions, presence & capability — the OwnerOnly access declares the owner
    // gate IN the table (the dispatch reads it, no hardcoded verb list). `who`
    // keeps its `Read` op-class (a fleet-wide presence readout) yet is Owner-gated:
    // op-class and scope-gate are orthogonal, which is exactly why `access` is a
    // separate field.
    // `fabric`: the bridge SUPERVISOR of a running instance. Session-independent
    // (answered before session resolution, so a windowless instance can be
    // attached), Owner-only because arming the supervisor starts the process
    // that holds `Scope::Bridge` — and which process that is is the config's to
    // say, not the caller's (`fabric attach` arms only what aterm.toml names).
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
         the instance was launched with — joined by spaces and pct-encoded, as every build has \
         written it, and `argv=` the same argv as a pct-encoded line of words whose own \
         whitespace or `%` is escaped (`%20`, `%25`), so a word of `[fabric] command`'s argv \
         form with a space in it is one word there. \
         `fabric attach [<command...>]` arms the supervisor NOW, \
         without a relaunch, and ONLY WITH WHAT THE INSTANCE'S CONFIG NAMES: with no argument the \
         `[fabric] command` its aterm.toml names now (the file is read again, so a command `aterm \
         fabric on` wrote after launch counts), else the one it was LAUNCHED with (`ERR fabric \
         no command` when neither names one); given words, they are the argv, split on \
         whitespace and never through a shell, read as written or with each word pct-decoded (a \
         word of the argv form with a space in it arrives as `%20`), and one of those readings \
         must BE one of those two commands — any other is `ERR fabric not configured \
         command=<pct>`, nothing \
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
    )
    // Outside the halt: arms the bridge supervisor, the process that carries a halt to this
    // instance; a held instance with no bridge is the one that needs a bridge to hear `hold
    // off`.
    .input(Input::NONE),
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
         how long ago it finished). Then `land_at=<unix-s|-> \
         task=<open|landed|theirs|-> pause=<unix-s|->`, the agent tab's timed \
         Cruise, its task's face and the harness's pause after short turns as `status` prints \
         them, and `task_state=<working|done|exited|stalled|-> task_rev=<n>`, the session's task \
         record (`task`; `-`/`0` with none). One main-thread hop per call, not per session; the \
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
         hold_origin=<person|remote|fleet|local, comma-joined, or -> detail=<pct|-> agent=<word|->` \
         row per readable session; `hold_origin=` says whose holds stand (`fleet,local`: the \
         fleet's halt beside the owner's own; `person`: the person's Take Over; `remote`: another \
         Mac's drive), which `status` does not, so the bridge never lifts the owner's own, the \
         person's or another Mac's. The sid and nonce fence \
         local-id reuse; an \
         unreadable session has no row, so the bridge retries its ordinary `@sid status`",
    )
    // Outside the halt: the roster; a read in Owner clothing.
    .input(Input::NONE),
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
    // `survey` is the glance over every session: one record each, read only from
    // what the sessions publish. Its own row rather than more `sessions`
    // columns, because its content level (the OSC title, prompts, last words)
    // must stay off any path that answers facts alone. The one owner-only row
    // the fleet reads: a fleet peer is answered at its machine's level.
    va(
        "survey",
        Read,
        Lines,
        Meta,
        OwnerOnly,
        "survey [facts|content]: every session's program, state, task, prompt, last words, tool in flight",
        "Owner-only, and a FLEET peer's (`[fleet]`) at the level that machine allows (`[fleet] \
         allow`), never more: a peer allowed facts that asks `content` is answered facts. `OK \
         <n>`, one `instance pid= host=<pct> version= sessions=<n> level=<facts|content> \
         cut=<n> at_ms=<unix ms> node=<32 hex|-> instances=<n>` header (`host=` the name its \
         LAN reaches this machine by, `node=` this machine's fleet node, which a fleet reader \
         checks against the node its book holds for the address it dialled and against the \
         node the connection proved, refusing a reply that names another, `instances=` the \
         live aterm instances here, a fleet peer reading only the one holding the fleet \
         port), then one `session` row per session in local-id order: \
         `local= sid= state= program=<pct|-> program_kind=<agent|shell|program|-> \
         agent=<word|-> agent_detail=<pct|-> agent_ms=<ms> human_ms=<ms|-> bg=<n|-> \
         task=<state|-> task_rev=<n> task_from=<pct|-> task_ms=<ms|-> model=<pct|-> \
         effort=<pct|-> ctx_left_pct=<n|-> detail=<pct|-> doing=<pct|-> \
         role=<pct|-> supervisor=<pct|-> attention=<1|-> needs=<word|-> hold_by=<word|-> \
         held_from=<32 hex|-> typed_by=<pct|-> typed_ms=<ms|->` \
         (the FACTS: words, counts and ages, and `role=` the role `meta set role` stamped \
         when it is a label - one word of at most 32 letters, digits and `:_.-`, redacted - \
         else `-`; no other text a person or a program wrote: `attention=1` a raised \
         attention line; `needs=` and `hold_by=` the session's published need and hold, \
         `held_from=` the node of the Mac whose drive holds it over the fleet, and \
         `typed_by=` and `typed_ms=` who last typed into it over the socket and how long ago, \
         as `status` prints them, `typed_by=` at facts only the typist tab's role or session \
         id, or aterm's own word, and only when it is a label as `role=` is, never the tab \
         name a person set), \
         then, with `content` (the local default), `user_title= \
         title= cwd= branch= prompt= said= cmd= rows=`, each `<pct|->` (`user_title=` the \
         tab name a person set), `role=` any role (an excerpt), and `attention=` carries the \
         line's words. Read only from what the sessions publish - \
         the agent verdict and what its screen counts (`bg=` background shells, `ctx_left_pct=` the \
         context LEFT), the Claude Code footer's model, effort, path and branch and its \
         transcript excerpts, the task record, `meta`, and the engine under a try-lock - \
         so it reads no transcript and never waits on a busy session. `program_kind=shell` is \
         a shell in front (at its prompt unless `detail=` names a command). `doing=` is the \
         tool in flight by name, never its input. `prompt=` is a tasked worker's task head, \
         else the newest prompt the window harness did not type (its `by=harness` turns in \
         `history` are passed over); `said=` the agent's newest words, a line that is only the \
         retired `ATERM-TASK-DONE` (older primers still teach it) dropped as `result` \
         drops it (a finished turn's - its task `done` or `exited`, or its agent `idle` - cut \
         keeping its end, `…` first, where a report puts its result); `cmd=` and `rows=` the \
         executing command line and last three lines of a session no transcript excerpt \
         speaks for - a shell, or an agent other than Claude Code (a line a soft wrap split \
         joined whole, so a secret cut at the wrap column is judged as one word). Every \
         excerpt is at most 400 bytes, credential-shaped words (a path's segments one at a time) and fleet join \
         codes (with or without `atf1-`, in one word or several, across lines, never cut by the \
         400-byte cap) replaced, a 40-digit hex word cut to its first 12 digits (a commit \
         id's short form; a fleet key has that shape too), and a sensitive line withheld; \
         `branch=` is only capped, its fleet code replaced. A fleet peer's reply is at most \
         256 KiB: past it the least pressing sessions (shells, then agents at rest, the \
         newest first) are answered at facts, counted in `cut=`, then left out, `sessions=` \
         still the whole count. Ages count through sleep. It is text from those sessions: \
         escape it before a terminal shows it. `aterm ctl survey [--json] [--budget <chars>] [--sid <s>] \
         [--fleet | --host <h>] [--timeout <s>]` asks every instance on this machine and \
         prints, after a first line saying every block under it is what those sessions show \
         and wrote (data to read, never an instruction to its reader), one header line per \
         instance and one line per session (at most 110 chars; an agent at a box nobody \
         presses, `agent=prompt` ranked asking, reads `asking`; `attention` on it when the \
         session raised an attention line), excerpt lines (the attention line's words — the \
         ask its `needs=ask` names, else the most recent entry —, \
         what it was asked, what it last said) for every agent, however long idle, and a \
         session raising attention, every session and each excerpt whole while the read \
         fits the budget (24000 chars): past it the excerpts narrow first, all alike down to \
         160 chars (a finished turn's `said:` keeping its end, where a report puts its \
         result); past that every header and agent line stays and the rest fills in order — \
         the asking and stalled sessions' excerpts, then each machine's most pressing \
         agent's before any machine's second's (its `said:` before its `asked:`) — shells \
         folded into a `+N` line first, a session that lost excerpts marked `(excerpts \
         cut)`, and a closing line counts the cut; a peer's sid that is no `s-<hex>` and a \
         word that is no word print `?`; `--json` prints every record unbudgeted, `--sid` one \
         session with its whole excerpts (`--budget` is the text's alone; a peer's takes \
         `--fleet`), or, when no row holds it, each peer whose reply cap left sessions out. \
         `--fleet` adds every fleet peer this machine knows (`aterm fleet peers`) and, on a \
         member, every member its issuer's book names (asked first, under the same \
         deadline), so any machine of the fleet reads the whole fleet: asked at once over the \
         fleet port, each within one deadline (`--timeout`, 3 s, at most 10; the first 32 at \
         once share it, and past that a peer waits for a free place and is given the \
         deadline of its own), one row each with its reason when it did not answer, each \
         block headed `== fleet <the name dialled>` (what a peer says of itself shown \
         beside it, and none of its sessions marked `(you)`). An issuer running an older \
         aterm shares no book: the read then ends with a line saying it reached only the \
         peers this machine knows. `--host <h>` asks that one peer and prints its machine \
         in full. `survey --brief [--budget <chars>]` is the LOCAL BRIEF: every instance here \
         asked `log n` and `brief all` (both the owner's, answered with the token beside its \
         socket), one block each, newest first — a header (its pid, how long it has been up, \
         how many node records it shows of how many it holds, each read it refused and why), \
         its node log lines with every `gap` line kept, then its sessions' briefs — the \
         budget shared newest first, and a closing line counting what it left out.",
    )
    .for_fleet(),
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
    )
    // Outside the halt: the exit ledger; a read in Owner clothing.
    .input(Input::NONE),
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
         session carries it, `ERR identity in use spawning=<k>` while k holds stand on it: \
         a spawn holds its identity from its admission, and a shell adopted with the label \
         from its adoption, until its tab is closed, so a tab not yet live counts and so \
         does one still closing, and k counts holds, not spawns (one spawn holds it twice \
         while its shell starts); a spawn asked for while the forget runs waits until the \
         identity is gone, and a named one then makes it anew, empty. Else `OK \
         removed=<pct> left=keychain` - the tree is gone, and with it the credentials the \
         agents keep as files (a reply that adds `tombstone=<pct> why=<pct>` names the \
         folder the tree was renamed to and could not be emptied: the identity is gone all \
         the same, and the next `forget` in that aterm, or once it has exited any aterm's \
         next `forget` or launch, sweeps it), but a login an agent keeps in the macOS \
         keychain is NOT: sign out in the agent first (`/logout`); aterm never reads, names \
         or deletes a keychain item it did not create. Names fold to lowercase. Unknown: \
         `ERR no such identity <name>`; anything else: `ERR usage: identities \
         [<name>|forget <name> [confirm=<name>]]`. A read except `forget`, which removes a \
         directory and never a session; a shell adopted from a build without the label \
         reads as no user of its identity (`sessions=0`), and a process that outlives its \
         tab (an agent still exiting, a disowned job) holds nothing: close the tab and let \
         what it ran exit before forgetting",
    )
    // One status line (`OK removed=…`, or the confirm/in-use `ERR`), not rows.
    .forms(&[form(Word("forget")).framing(Status)])
    // Outside the halt: lists the agent identities on disk, and `forget` removes one such
    // directory, refused while any live session carries it or a spawn holds it: never a
    // session.
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
    // Outside the halt: reports this connection's own scope.
    .input(Input::NONE),
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
    )
    // `family <sid>` walks the tree from an arbitrary node.
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
        "`grant <src-sid> <op>` mints an edge from <src-sid> onto this session and replies \
         `OK <token-hex>` (the caller delivers it); <op> is read-screen, write-input, signal, \
         config-write, clipboard-write or derive-loop. A <src-sid> that is one of the session \
         log's actor words (human ctl harness bridge aterm vendor -) is refused `ERR reserved`, \
         and the log names an edge's acts by its source only when that is a minted sid (`-` \
         otherwise). `connect` sets whole connections; `revoke` removes",
    )
    // Outside the halt: mints an edge token; the halt is scope-blind, so a token minted
    // under a halt still cannot type into a held session.
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
    // Outside the halt: removes authority; strictly de-escalating.
    .input(Input::NONE),
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
    )
    // Outside the halt: mints the session-connection ops (a capability graph edge), not a
    // live pipe; the minted edge meets the same scope-blind halt.
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
    // Outside the halt: dissolves such an edge; de-escalating.
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
    // Outside the halt: reads the connection graph.
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
    // Outside the halt: raises a window and selects a tab: it moves the human's focus and
    // puts no bytes anywhere.
    .input(Input::NONE),
];

/// The spec for `verb`, or `None` for an unknown verb.
#[must_use]
pub fn spec(verb: &str) -> Option<&'static VerbSpec> {
    VERBS.iter().find(|s| s.name == verb)
}

/// `verb`'s [`Input`] class ([`VerbSpec::input_class`]), [`Input::NONE`] for an
/// unknown verb. A listed sub-form row has its verb's class, so the verb word alone
/// decides.
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
    /// Only an owner-class connection may run the request, whatever its row's
    /// [`Access`].
    pub owner_only: bool,
    /// How the reply is framed, a `--json` on a [`Json::Emits`] row included.
    pub framing: Framing,
    /// The lane the request costs.
    pub lane: Lane,
    /// Whether a successful reply needs the artifact acknowledgement.
    pub ack: bool,
}

/// THE ONE RESOLUTION of a request's verb facts: `verb` and `rest`, the text after
/// it (no selector). The row is the longest name the request spells — the listed
/// sub-form row `"<verb> <first word of rest>"` (`inbox get`, `theme set`), else
/// `verb`'s own — and the first of that row's forms whose [`When`] matches the tail
/// (what follows the sub-word for a sub-form row, all of `rest` otherwise, past an
/// App-lane gesture's leading `unread=ok`) lays its facts over the row's. `None` for
/// an unknown verb. Framing, acknowledgement, lane,
/// authority and JSON handling are all read here, so the client that parses a reply
/// and the server that writes it cannot classify a request two ways.
#[must_use]
// Skip: iterator absent std bodies.
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
    // An App-lane gesture's leading `unread=ok` is stripped before the verb
    // parses ([`Input::APP_UNREAD`]), so it never hides the form after it.
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
// Skip: iterator absent std bodies.
#[cfg_attr(trust_verify, trust::skip)]
pub fn json_flag(rest: &str) -> bool {
    rest.split_whitespace()
        .any(|t| t == "--json" || t == "json")
}

/// The verbs that answer JSON under `--json`/`json` ([`Json::Emits`]), in table
/// order — the list `help` advertises.
// Skip: iterator absent std bodies.
#[cfg_attr(trust_verify, trust::skip)]
pub fn json_verbs() -> impl Iterator<Item = &'static str> {
    VERBS
        .iter()
        .filter(|s| matches!(s.json, Json::Emits))
        .map(|s| s.name)
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
/// an instance at lane 1 answers `ERR usage`. `link bye <quit|handoff>` needed
/// no bump: a bridge says it only to take the `EVENT * bye <word>` an instance
/// pushes, and an instance that pushes it reads it; a bridge from before it
/// reads that event past. 3, `link log <identity> continues=<off> was=<identity>`
/// (a MOVED GENERATION of the log the endpoint was told before, and the offset
/// below which the two name the same records), which an instance at lane 2
/// answers `ERR usage` — and a bridge reads that refusal as an attach that
/// failed, so a lane-2 instance is told the bare identity and forgets every
/// offset, as it did before lineage.
pub const BRIDGE_LANE: u32 = 3;

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
/// retention: [`spec_for`]'s acknowledgement for the text after the verb, so the
/// shipping client and the server share one classification. The full `request`
/// line is read like [`framing_of`]'s. An `ERR` never needs the acknowledgement.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn artifact_reply_requires_ack(verb: &str, request: &str) -> bool {
    spec_for(verb, head(request).rest).is_some_and(|r| r.ack)
}

/// The reply framing of `verb` for the full `request` line: [`spec_for`]'s framing
/// for the text after the verb, so a sub-form (`image read`, `cast drift`, `inbox
/// get`) and a `--json` on a JSON-emitting verb frame as the table says. The line is
/// read by the request grammar the server reads it by ([`head`]), so a leading
/// `@<selector>` is skipped. Unknown verbs are `Status`.
#[must_use]
// Skip: iterator absent std bodies.
#[cfg_attr(trust_verify, trust::skip)]
pub fn framing_of(verb: &str, request: &str) -> Framing {
    spec_for(verb, head(request).rest).map_or(Status, |r| r.framing)
}

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
        assert_eq!(framing_of("image", "@s-a image bytes"), Lines);
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
        // `topic` and `topic ls` list `OK <n>` + rows; `add`/`drop` answer one
        // status line, which a client reading Lines would take as a row count.
        assert_eq!(framing_of("topic", "topic"), Lines);
        assert_eq!(framing_of("topic", "topic ls"), Lines);
        assert_eq!(framing_of("topic", "topic add build ttl=60"), Status);
        assert_eq!(framing_of("topic", "@s-a topic drop build"), Status);
        // `await task` answers a row per target (its timeout too); every other
        // `await` and the record itself (`task`, `task mark`) are one line.
        assert_eq!(framing_of("await", "await task @s-a,@s-b all"), Lines);
        assert_eq!(framing_of("await", "@s-a await task since=3"), Lines);
        assert_eq!(framing_of("await", "await agent idle"), Status);
        assert_eq!(framing_of("await", "await inbox since=1"), Status);
        // `edit` and `await edit` each answer one status line (`OK edit …`).
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
        // `result` answers `OK <n>` + the text's lines, `result full` too.
        assert_eq!(framing_of("result", "@s-a result"), Lines);
        assert_eq!(framing_of("result", "result full"), Lines);
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
    /// framing. Selector-aware, like every other sub-form rule.
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
        // `theme session` routes on the verb's App row; its bare form and `status`
        // read, and `set`/`clear` write the session's colours.
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
        // The catch-all is case-sensitive and last: `SET` is no listed row, so the
        // write it might be fails closed to ConfigWrite; `status` is named first.
        let upper = r("theme", "SET Nord");
        assert_eq!((upper.row.name, upper.op), ("theme", ConfigWrite));
        assert_eq!(r("theme", "set Nord").row.name, "theme set");
        assert_eq!(r("theme", "preview Nord").op, Write);
        assert_eq!(r("theme", "revert").op, Write);
        assert_eq!(r("theme", "status").op, Read);
        assert_eq!(r("theme", "").op, Read);
        // Lanes: the form, then the row; a sub-word on its own changes nothing.
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
        // `edit` waits for its view to be installed, `await edit` for it to close.
        assert_eq!(r("edit", "file:///tmp/x.txt line=3").lane, Wait);
        assert_eq!(r("await", "edit ticket=1").lane, Wait);
        // Owner-only forms, and the rows they leave open.
        assert!(r("update", "check").owner_only && r("update", "apply").owner_only);
        assert_eq!(r("update", "check").lane, Wait);
        assert!(!r("update", "status").owner_only && !r("update", "").owner_only);
        assert!(r("family", "s-1").owner_only && !r("family", "").owner_only);
        assert!(r("pointer", "click").owner_only && !r("pointer", "move 3 4").owner_only);
        assert!(r("pointer", "mods link").owner_only);
        // An App-lane gesture's leading `unread=ok` is read past before the form
        // is matched, and only on a row that takes it: `update` takes none, so
        // there the same word hides the owner-only form behind it.
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
        // A verb that is a prefix of another name is not its sub-form row.
        assert_eq!(r("log", "take").row.name, "log");
        assert!(spec_for("bogus", "").is_none());
    }

    /// THE SHIPPED `help hold` TEXT ENUMERATES THE HALT SET, SO IT IS PINNED TO
    /// THE HALT SET: the rows whose [`Input`] class has [`Input::PTY`].
    ///
    /// aterm has no evidence manifest: the `hold` row's detail IS the claim of
    /// record for what a fleet halt refuses, and a driver author's back-off
    /// classes are written from it. It had already drifted by three verbs
    /// (`hwkey` and `pane` from a main merge, `pointer` from a fix) while the
    /// set's own check went on passing, because that check never read the help.
    ///
    /// SET EQUALITY, both directions: a driver that parses a documented verb
    /// which is in fact refused, and a driver that is refused a verb the
    /// documentation placed outside the set, are the same outage.
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
