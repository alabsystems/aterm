// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Closed wire vocabularies for `aterm-slog` operations, levels, node IDs, kinds, sources, confidences, and actors.

use std::fmt;

/// Fallback token substituted for unrecognized vocabulary words.
pub const UNKNOWN: &str = "unknown";

/// Record operation on a log node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Op {
    /// Node begins.
    Open,
    /// Node ends with its rollup.
    Close,
    /// Point fact attached to an open node.
    Mark,
    /// Higher-confidence refinement of a node.
    Amend,
    /// Exact count of lost records.
    Gap,
    /// Snapshot of the open ancestor path.
    Snap,
}

impl Op {
    /// All operations in wire order.
    pub const ALL: [Op; 6] = [Op::Open, Op::Close, Op::Mark, Op::Amend, Op::Gap, Op::Snap];

    /// Wire token for this operation.
    #[inline]
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Op::Open => "open",
            Op::Close => "close",
            Op::Mark => "mark",
            Op::Amend => "amend",
            Op::Gap => "gap",
            Op::Snap => "snap",
        }
    }

    /// Parse an [`Op`] from its wire token.
    #[must_use]
    pub fn from_word(word: &str) -> Option<Op> {
        Op::ALL.into_iter().find(|op| op.word() == word)
    }
}

/// Hierarchy level (`n`, `-`, `r<n>`, `t<n>`, `p<n>`), outermost first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Lvl {
    /// The instance's own node log (`n`).
    Instance,
    /// The session (`-`).
    Session,
    /// A command, program or agent run (`r<n>`).
    Run,
    /// A turn of an agent run (`t<n>`).
    Turn,
    /// A step inside a turn (`p<n>`).
    Step,
}

impl Lvl {
    /// The id letter of a numbered level.
    const fn letter(self) -> Option<char> {
        match self {
            Lvl::Run => Some('r'),
            Lvl::Turn => Some('t'),
            Lvl::Step => Some('p'),
            Lvl::Instance | Lvl::Session => None,
        }
    }
}

/// Hierarchy node identifier pairing a [`Lvl`] and ordinal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Node {
    /// The node's level.
    pub lvl: Lvl,
    /// Its number within the level (0 for the session and the instance).
    pub n: u32,
}

impl Node {
    /// Session root node (`-`).
    pub const SESSION: Node = Node {
        lvl: Lvl::Session,
        n: 0,
    };
    /// Instance root node (`n`).
    pub const INSTANCE: Node = Node {
        lvl: Lvl::Instance,
        n: 0,
    };

    /// Construct a numbered node at `lvl`.
    #[inline]
    #[must_use]
    pub const fn new(lvl: Lvl, n: u32) -> Node {
        Node { lvl, n }
    }

    /// Parse a wire node ID (`-`, `n`, `r3`, `t17`, `p204`).
    #[must_use]
    pub fn parse(s: &str) -> Option<Node> {
        match s {
            "-" => return Some(Node::SESSION),
            "n" => return Some(Node::INSTANCE),
            _ => {}
        }
        let mut chars = s.chars();
        let lvl = match chars.next()? {
            'r' => Lvl::Run,
            't' => Lvl::Turn,
            'p' => Lvl::Step,
            _ => return None,
        };
        let digits = chars.as_str();
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let n: u32 = digits.parse().ok()?;
        (n > 0).then_some(Node { lvl, n })
    }
}

impl fmt::Display for Node {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.lvl.letter() {
            Some(letter) => write!(f, "{letter}{}", self.n),
            None if self.lvl == Lvl::Session => f.write_str("-"),
            None => f.write_str("n"),
        }
    }
}

/// Declares the closed `Kind` enum and its word table in one place.
macro_rules! kinds {
    ($($variant:ident => $word:literal,)*) => {
        /// Closed node or mark kind.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Kind {
            $(#[doc = concat!("`", $word, "`")] $variant,)*
            /// A word this reader does not know.
            Unknown,
        }

        impl Kind {
            /// All known [`Kind`] variants.
            pub const ALL: &'static [Kind] = &[$(Kind::$variant,)*];

            /// Wire token for this kind.
            #[must_use]
            pub const fn word(self) -> &'static str {
                match self {
                    $(Kind::$variant => $word,)*
                    Kind::Unknown => UNKNOWN,
                }
            }
        }
    };
}

kinds! {
    Spawn => "spawn",
    Adopt => "adopt",
    Restore => "restore",
    Cmd => "cmd",
    Prog => "prog",
    Agent => "agent",
    Turn => "turn",
    Approval => "approval",
    Question => "question",
    Wall => "wall",
    Survey => "survey",
    Tool => "tool",
    Subagent => "subagent",
    Compact => "compact",
    Stall => "stall",
    Start => "start",
    Status => "status",
    Human => "human",
    Input => "input",
    Mail => "mail",
    MailSeen => "mail-seen",
    Hold => "hold",
    Meta => "meta",
    Cwd => "cwd",
    Title => "title",
    Integration => "integration",
    Notify => "notify",
    Bell => "bell",
    Progress => "progress",
    Ctx => "ctx",
    Goal => "goal",
    Identify => "identify",
    Render => "render",
    Modes => "modes",
    InDoubt => "in-doubt",
    Seen => "seen",
    Story => "story",
    Act => "act",
    Note => "note",
    Verdict => "verdict",
    Label => "label",
    Need => "need",
    Clear => "clear",
    Usage => "usage",
    Handoff => "handoff",
    Link => "link",
    Config => "config",
    Gap => "gap",
    Snap => "snap",
    Source => "source",
    MailLanded => "mail-landed",
    Task => "task",
    State => "state",
    Policy => "policy",
    Topic => "topic",
    Advisory => "advisory",
    Program => "program",
    MailFetch => "mail-fetch",
    Shown => "shown",
    Row => "row",
    MailGap => "mail-gap",
}

impl Kind {
    /// Parse a [`Kind`] from its wire token, mapping unknown tokens to [`Kind::Unknown`].
    #[must_use]
    pub fn from_word(word: &str) -> Kind {
        Kind::ALL
            .iter()
            .copied()
            .find(|k| k.word() == word)
            .unwrap_or(Kind::Unknown)
    }

    /// Hierarchy level for a node kind, or `None` for mark/gap/snap kinds.
    #[inline]
    #[must_use]
    pub const fn node_level(self) -> Option<Lvl> {
        match self {
            Kind::Spawn | Kind::Adopt | Kind::Restore => Some(Lvl::Session),
            Kind::Cmd | Kind::Prog | Kind::Agent => Some(Lvl::Run),
            Kind::Turn => Some(Lvl::Turn),
            Kind::Approval
            | Kind::Question
            | Kind::Wall
            | Kind::Survey
            | Kind::Tool
            | Kind::Subagent
            | Kind::Compact
            | Kind::Stall => Some(Lvl::Step),
            Kind::Start => Some(Lvl::Instance),
            _ => None,
        }
    }

    /// Whether this kind is valid on a [`Op::Mark`] record.
    #[inline]
    #[must_use]
    pub const fn is_mark(self) -> bool {
        matches!(
            self,
            Kind::Status
                | Kind::Agent
                | Kind::Human
                | Kind::Input
                | Kind::Mail
                | Kind::MailSeen
                | Kind::Hold
                | Kind::Meta
                | Kind::Cwd
                | Kind::Title
                | Kind::Integration
                | Kind::Notify
                | Kind::Bell
                | Kind::Progress
                | Kind::Ctx
                | Kind::Goal
                | Kind::Identify
                | Kind::Render
                | Kind::Modes
                | Kind::InDoubt
                | Kind::Seen
                | Kind::Story
                | Kind::Act
                | Kind::Note
                | Kind::Verdict
                | Kind::Label
                | Kind::Need
                | Kind::Clear
                | Kind::Usage
                | Kind::Handoff
                | Kind::Link
                | Kind::Config
                | Kind::Start
                | Kind::Source
                | Kind::MailLanded
                | Kind::Task
                | Kind::State
                | Kind::Policy
                | Kind::Topic
                | Kind::Advisory
                | Kind::Program
                | Kind::MailFetch
                | Kind::Shown
                | Kind::Row
                | Kind::MailGap
        )
    }

    /// Whether this kind is valid for operation `op` at level `lvl`.
    #[must_use]
    pub fn fits(self, op: Op, lvl: Lvl) -> bool {
        match op {
            Op::Open | Op::Close | Op::Amend => self.node_level() == Some(lvl),
            Op::Mark => self.is_mark(),
            Op::Gap => self == Kind::Gap,
            Op::Snap => self == Kind::Snap,
        }
    }
}

/// Observation source for a log record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Src {
    /// OSC 133 / 633 shell integration.
    Shell,
    /// The status classifier.
    Status,
    /// The screen reader's verdict.
    Screen,
    /// A vendor transcript.
    Transcript,
    /// A control-socket verb.
    Ctl,
    /// The input path.
    Input,
    /// The fabric.
    Fabric,
    /// Session metadata.
    Meta,
    /// The terminal engine.
    Engine,
    /// aterm itself (lifecycle, synthetic closes).
    Aterm,
    /// A `mark` written back by a consumer.
    Mark,
    /// Reserved hook source.
    Hook,
    /// A word this reader does not know.
    Unknown,
}

impl Src {
    /// All known [`Src`] variants.
    pub const ALL: [Src; 12] = [
        Src::Shell,
        Src::Status,
        Src::Screen,
        Src::Transcript,
        Src::Ctl,
        Src::Input,
        Src::Fabric,
        Src::Meta,
        Src::Engine,
        Src::Aterm,
        Src::Mark,
        Src::Hook,
    ];

    /// Wire token for this source.
    #[inline]
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Src::Shell => "shell",
            Src::Status => "status",
            Src::Screen => "screen",
            Src::Transcript => "transcript",
            Src::Ctl => "ctl",
            Src::Input => "input",
            Src::Fabric => "fabric",
            Src::Meta => "meta",
            Src::Engine => "engine",
            Src::Aterm => "aterm",
            Src::Mark => "mark",
            Src::Hook => "hook",
            Src::Unknown => UNKNOWN,
        }
    }

    /// Parse a [`Src`] from its wire token, mapping unknown tokens to [`Src::Unknown`].
    #[must_use]
    pub fn from_word(word: &str) -> Src {
        Src::ALL
            .into_iter()
            .find(|s| s.word() == word)
            .unwrap_or(Src::Unknown)
    }
}

/// Confidence level of a log record (`Exact`, `Strong`, `Heuristic`, `Claim`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Conf {
    /// Read off a record that says so (a transcript, an exit status).
    Exact,
    /// Observed directly.
    Strong,
    /// Inferred.
    Heuristic,
    /// Self-reported through `mark`.
    Claim,
    /// A word this reader does not know.
    Unknown,
}

impl Conf {
    /// All known [`Conf`] variants.
    pub const ALL: [Conf; 4] = [Conf::Exact, Conf::Strong, Conf::Heuristic, Conf::Claim];

    /// Wire token for this confidence.
    #[inline]
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Conf::Exact => "exact",
            Conf::Strong => "strong",
            Conf::Heuristic => "heuristic",
            Conf::Claim => "claim",
            Conf::Unknown => UNKNOWN,
        }
    }

    /// Parse a [`Conf`] from its wire token, mapping unknown tokens to [`Conf::Unknown`].
    #[must_use]
    pub fn from_word(word: &str) -> Conf {
        Conf::ALL
            .into_iter()
            .find(|c| c.word() == word)
            .unwrap_or(Conf::Unknown)
    }

    /// Ordinal rank (`unknown < claim < heuristic < strong < exact`).
    #[inline]
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            Conf::Unknown => 0,
            Conf::Claim => 1,
            Conf::Heuristic => 2,
            Conf::Strong => 3,
            Conf::Exact => 4,
        }
    }

    /// Single-character reliability suffix printed by `brief` (`!`, `+`, `~`, `?`).
    #[inline]
    #[must_use]
    pub const fn suffix(self) -> char {
        match self {
            Conf::Exact => '!',
            Conf::Strong => '+',
            Conf::Heuristic => '~',
            Conf::Claim | Conf::Unknown => '?',
        }
    }
}

/// Fixed-capacity ASCII identifier (`[A-Za-z0-9._:@-]`, up to 40 bytes).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ShortId {
    bytes: [u8; ShortId::MAX],
    len: u8,
}

impl ShortId {
    /// Maximum byte length of a [`ShortId`].
    pub const MAX: usize = 40;

    /// Validate and construct a [`ShortId`] from `s`.
    #[must_use]
    pub fn new(s: &str) -> Option<ShortId> {
        let raw = s.as_bytes();
        if raw.is_empty() || raw.len() > Self::MAX || !raw.iter().all(|&b| is_id_byte(b)) {
            return None;
        }
        let mut bytes = [0u8; Self::MAX];
        bytes[..raw.len()].copy_from_slice(raw);
        Some(ShortId {
            bytes,
            len: raw.len() as u8,
        })
    }

    /// Identifier as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..usize::from(self.len)]).unwrap_or("-")
    }
}

/// A byte of the id alphabet.
const fn is_id_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'@' | b'-')
}

impl fmt::Debug for ShortId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ShortId({:?})", self.as_str())
    }
}

impl fmt::Display for ShortId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Reserved connection-class actor tokens that cannot be used as session IDs.
pub const RESERVED_ACTOR_WORDS: &[&str] =
    &["human", "ctl", "harness", "bridge", "aterm", "vendor", "-"];

/// Whether `word` is in [`RESERVED_ACTOR_WORDS`].
#[must_use]
pub fn is_reserved_actor_word(word: &str) -> bool {
    RESERVED_ACTOR_WORDS.contains(&word)
}

/// Attributed actor for a log record, derived from the connection class.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Actor {
    /// The window's input path.
    Human,
    /// Any other Owner client presenting the instance token.
    Ctl,
    /// The in-GUI harness.
    Harness,
    /// The fabric bridge.
    Bridge,
    /// The server itself.
    Aterm,
    /// The agent vendor (a transcript row).
    Vendor,
    /// A session, by its sid (an edge-token connection).
    Sid(ShortId),
    /// Not attributable: `-`.
    Nobody,
}

impl Actor {
    /// Wire token for this actor.
    #[must_use]
    pub fn word(&self) -> &str {
        match self {
            Actor::Human => "human",
            Actor::Ctl => "ctl",
            Actor::Harness => "harness",
            Actor::Bridge => "bridge",
            Actor::Aterm => "aterm",
            Actor::Vendor => "vendor",
            Actor::Sid(sid) if is_reserved_actor_word(sid.as_str()) => "-",
            Actor::Sid(sid) => sid.as_str(),
            Actor::Nobody => "-",
        }
    }

    /// Construct [`Actor::Sid`], mapping reserved actor words to [`Actor::Nobody`].
    #[must_use]
    pub fn sid(id: ShortId) -> Actor {
        if is_reserved_actor_word(id.as_str()) {
            Actor::Nobody
        } else {
            Actor::Sid(id)
        }
    }

    /// Parse an [`Actor`] from its wire token.
    #[must_use]
    pub fn from_word(word: &str) -> Actor {
        match word {
            "human" => Actor::Human,
            "ctl" => Actor::Ctl,
            "harness" => Actor::Harness,
            "bridge" => Actor::Bridge,
            "aterm" => Actor::Aterm,
            "vendor" => Actor::Vendor,
            "-" => Actor::Nobody,
            other => ShortId::new(other).map_or(Actor::Nobody, Actor::sid),
        }
    }

    /// Whether this actor belongs to the owner class (`Human` or `Ctl`).
    #[inline]
    #[must_use]
    pub const fn is_owner(&self) -> bool {
        matches!(self, Actor::Human | Actor::Ctl)
    }

    /// Whether this actor is trusted to author claims (`Harness`, `Human`, or `Ctl`).
    #[inline]
    #[must_use]
    pub const fn is_trusted(&self) -> bool {
        matches!(self, Actor::Harness | Actor::Human | Actor::Ctl)
    }

    /// Whether this actor's connection proves it is outside any session (`Harness` or `Human`).
    #[inline]
    #[must_use]
    pub const fn is_proven(&self) -> bool {
        matches!(self, Actor::Harness | Actor::Human)
    }

    /// Whether a `clear` from `self` can lift a need raised by `raiser`.
    #[inline]
    #[must_use]
    pub const fn overrides(&self, raiser: &Actor) -> bool {
        match self {
            Actor::Human => true,
            Actor::Ctl => !raiser.is_proven(),
            _ => false,
        }
    }
}

/// Closed `box=` approval prompt kind vocabulary.
pub const BOX_WORDS: &[&str] = &[
    "bash",
    "bash:read-only",
    "bash:not-read-only",
    "edit",
    "write",
    "overwrite",
    "read",
    "workflow",
    "trust",
    "powershell",
    "fetch",
    "network",
    "browser",
    "skill",
    "monitor",
    "tool",
    "question",
    "plan-enter",
    "plan-exit",
    "held-message",
    "goal-proposal",
    "computer-use",
    "read-outside-setting",
    "usage-limit",
    "model-switch",
    "autocompact-offer",
    "rate-nudge",
    "model-pick",
    "goal-resume",
    "mcp-server",
    "auto-mode-setup",
    "other",
];

/// Closed bare wall-kind vocabulary.
pub const WALL_WORDS: &[&str] = &[
    "usage-session",
    "usage-weekly",
    "model-bucket",
    "spend",
    "context",
    "auth",
    "api-error",
    "overloaded",
    "memory",
];

/// Closed `cls=` tool step class vocabulary.
pub const CLS_WORDS: &[&str] = &[
    "shell", "edit", "write", "read", "search", "web", "agent", "plan", "mcp", "other",
];

/// Closed `out=` outcome vocabulary.
pub const OUT_WORDS: &[&str] = &[
    "ok",
    "fail",
    "signal",
    "gone",
    "superseded",
    "retracted",
    "unknown",
    "done",
    "asked",
    "wall:usage-session",
    "wall:usage-weekly",
    "wall:model-bucket",
    "wall:spend",
    "wall:context",
    "wall:auth",
    "wall:api-error",
    "wall:overloaded",
    "wall:memory",
    "interrupted",
    "error",
    "approved",
    "declined",
    "answered",
    "dismissed",
    "passed",
    "shell-exit",
    "ctl-close",
    "ui-close",
    "window-close",
    "app-quit",
];

/// Closed need vocabulary (`need` and `clear` marks).
pub const NEED_WORDS: &[&str] = &[
    "approval:bash",
    "approval:edit",
    "approval:write",
    "approval:overwrite",
    "approval:read",
    "approval:workflow",
    "approval:trust",
    "approval:powershell",
    "approval:fetch",
    "approval:network",
    "approval:browser",
    "approval:skill",
    "approval:monitor",
    "approval:tool",
    "approval:plan-enter",
    "approval:plan-exit",
    "approval:held-message",
    "approval:goal-proposal",
    "approval:computer-use",
    "approval:read-outside-setting",
    "approval:usage-limit",
    "approval:model-switch",
    "approval:autocompact-offer",
    "approval:rate-nudge",
    "approval:model-pick",
    "approval:goal-resume",
    "approval:mcp-server",
    "approval:auto-mode-setup",
    "approval:other",
    "question",
    "wall:usage-session",
    "wall:usage-weekly",
    "wall:model-bucket",
    "wall:spend",
    "wall:context",
    "wall:auth",
    "wall:api-error",
    "wall:overloaded",
    "wall:memory",
    "unresponsive",
    "failure",
    "escalated",
    "exited",
    "review",
    "decision",
    "help",
    "unknown",
    "frozen",
    "stopped",
    "stalled",
    "login",
    "memory",
    "wall",
    "ask",
    "approval",
];

/// Closed `word=` vocabulary across agent verdicts, acts, and needs.
pub const WORD_WORDS: &[&str] = &[
    "busy",
    "prompt",
    "question",
    "idle",
    "survey",
    "unknown",
    "-",
    "wall:usage-session",
    "wall:usage-weekly",
    "wall:model-bucket",
    "wall:spend",
    "wall:context",
    "wall:auth",
    "wall:api-error",
    "wall:overloaded",
    "wall:memory",
    "wall:unresponsive",
    "approve",
    "deny",
    "answer",
    "continue",
    "relaunch",
    "escalate",
    "interrupt",
    "dispatch",
    "landed",
    "on-track",
    "off-track",
    "blocked",
    "done",
    "failed",
    "needs-human",
    "unsure",
    "review",
    "decision",
    "help",
    "escalated",
    "failure",
    "unresponsive",
    "exited",
    "approved",
    "dismissed",
    "reconnected",
    "timeout",
    "exit",
    "compacted",
    "warned",
    "chose",
    "handled",
    "refused",
    "deferred",
];

/// Closed `gap why=` loss-reason vocabulary.
pub const WHY_WORDS: &[&str] = &[
    "handoff",
    "crash",
    "pruned",
    "evicted",
    "reset",
    "unread",
    "dropped",
    "unknown",
    "landed",
    "marker",
    "done-check",
    "exit",
    "shell",
    "startup",
    "held",
    "turn",
    "moved",
    "mail",
    "burst",
    "not-carried",
    "log-replaced",
];

/// Closed `input verb=` vocabulary.
pub const VERB_WORDS: &[&str] = &["send", "key", "paste", "feed", "turn", "unknown"];

/// Closed `act=` control-input action vocabulary.
pub const ACT_WORDS: &[&str] = &["approve", "deny", "answer"];

/// Closed `pool=` rate-limiter pool vocabulary.
pub const POOL_WORDS: &[&str] = &["shared"];

/// Closed `mail dir=` direction vocabulary.
pub const DIR_WORDS: &[&str] = &["in", "out", "unknown"];

/// Closed `mail kind=` vocabulary.
pub const MAIL_KIND_WORDS: &[&str] = &[
    "task",
    "ask",
    "answer",
    "report",
    "note",
    "unknown",
    "control",
    "ack",
    "expired",
    "undeliverable",
];

/// Closed `status phase=` vocabulary.
pub const PHASE_WORDS: &[&str] = &[
    "idle", "running", "waiting", "prompt", "exited", "unknown", "starting", "quiet",
];

/// Closed `state=` vocabulary for shell, progress, render, and source marks.
pub const STATE_WORDS: &[&str] = &[
    "on",
    "off",
    "partial",
    "normal",
    "error",
    "paused",
    "indeterminate",
    "none",
    "unknown",
    "degraded",
    "desync-risk",
    "healed",
    "unjudged",
    "working",
    "done",
    "exited",
    "stalled",
    "connected",
    "disconnected",
    "absent",
    "spawning",
    "alive",
    "closed",
    "idle",
    "blocked",
];

/// Closed `origin=` hold-origin vocabulary.
pub const ORIGIN_WORDS: &[&str] = &["person", "remote", "fleet", "local"];

/// Closed `program ask=` blocked-reason vocabulary.
pub const ASK_WORDS: &[&str] = &["permission", "question", "auth"];

/// Closed `topic change=` vocabulary.
pub const CHANGE_WORDS: &[&str] = &["add", "drop"];

/// Closed `render net=` geometry delta vocabulary.
pub const NET_WORDS: &[&str] = &["zero", "changed"];

/// Closed `render class=` program-class vocabulary.
pub const CLASS_WORDS: &[&str] = &["agent", "learned", "unknown"];

/// Closed `shown sev=` severity vocabulary.
pub const SEV_WORDS: &[&str] = &["stuck", "decide"];

/// Closed `shown held=` hold-owner vocabulary.
pub const HELD_WORDS: &[&str] = &["person", "remote", "local", "bridge"];

/// Closed `task fact_rule=` vocabulary: the rule of the fact word a worker's task record carries
/// (aterm-agent's `occupancy::RuleId::name`, whose ordinal is `fact_n=`). Append only.
pub const FACT_RULE_WORDS: &[&str] = &["wait-nudge@v1", "hold-room@v1", "wake@v1"];

/// Closed `render healed_by=` vocabulary.
pub const HEALED_BY_WORDS: &[&str] = &["app", "undo", "nudge"];

/// Closed `modes source=` manual-reset source vocabulary.
pub const SOURCE_WORDS: &[&str] = &["ctl", "menu"];

/// Closed `start prev_end=` previous-run exit classification vocabulary.
pub const END_WORDS: &[&str] = &[
    "clean", "handoff", "killed", "signal", "panic", "running", "unknown", "none", "-",
];

/// Closed `cause_k=` cause kind vocabulary.
pub const CAUSE_WORDS: &[&str] = &["mail", "rec", "unknown"];

/// Closed `dur=` duration clock basis vocabulary.
pub const DUR_WORDS: &[&str] = &["mono", "wall", "unknown"];

/// Look up `word` in static vocabulary `list`.
#[must_use]
pub fn lookup(list: &'static [&'static str], word: &str) -> Option<&'static str> {
    list.iter().copied().find(|w| *w == word)
}

/// Look up `word` in `list`, returning [`UNKNOWN`] when absent.
#[must_use]
pub fn lookup_or_unknown(list: &'static [&'static str], word: &str) -> &'static str {
    lookup(list, word).unwrap_or(UNKNOWN)
}
