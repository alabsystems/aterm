// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The closed vocabularies of a record (design §3.3): operations, levels,
//! node ids, kinds, sources, confidences, actors, and the word lists of every
//! word-valued key. Words are only ever added; a reader maps a word it does
//! not know to `unknown` ([`UNKNOWN`]).

use std::fmt;

/// The word a reader puts in place of a word it does not know.
pub const UNKNOWN: &str = "unknown";

/// What a record does to its node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Op {
    /// A node begins.
    Open,
    /// A node ends (exactly once), carrying its rollup.
    Close,
    /// A point fact attached to an open node.
    Mark,
    /// A more reliable source refines a node.
    Amend,
    /// Records were lost; the count is exact.
    Gap,
    /// A snapshot of the open path (a later phase writes it).
    Snap,
}

impl Op {
    /// Every operation, in wire order.
    pub const ALL: [Op; 6] = [Op::Open, Op::Close, Op::Mark, Op::Amend, Op::Gap, Op::Snap];

    /// The wire word.
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

    /// The operation a wire word names, or `None`: a line whose operation is
    /// unknown is not a record a reader can place.
    #[must_use]
    pub fn from_word(word: &str) -> Option<Op> {
        Op::ALL.into_iter().find(|op| op.word() == word)
    }
}

/// A level of the hierarchy (§2.1), outermost first.
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

/// A node id: a level letter and a number, never reused while the session id
/// lives (§2.2). The session is `-` and the instance `n`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Node {
    /// The node's level.
    pub lvl: Lvl,
    /// Its number within the level (0 for the session and the instance).
    pub n: u32,
}

impl Node {
    /// The session node, `-`.
    pub const SESSION: Node = Node {
        lvl: Lvl::Session,
        n: 0,
    };
    /// The instance node, `n`.
    pub const INSTANCE: Node = Node {
        lvl: Lvl::Instance,
        n: 0,
    };

    /// A numbered node of `lvl`.
    #[must_use]
    pub const fn new(lvl: Lvl, n: u32) -> Node {
        Node { lvl, n }
    }

    /// The node a wire id names (`-`, `n`, `r3`, `t17`, `p204`), or `None`.
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
        /// A record's kind: the node's kind for open, close and amend, the
        /// mark's kind for a mark (§2.1, §3.3). Closed per (level, op)
        /// ([`Kind::fits`]).
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Kind {
            $(#[doc = concat!("`", $word, "`")] $variant,)*
            /// A word this reader does not know.
            Unknown,
        }

        impl Kind {
            /// Every known kind.
            pub const ALL: &'static [Kind] = &[$(Kind::$variant,)*];

            /// The wire word.
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
    // Phase 3B, appended so no kind moves: a run's view of one of its
    // sources (`source field=transcript state=on|partial|off skipped= dropped=`,
    // ruling S31), and a post the broker landed (`mail-landed inbox= off=`: a
    // mark is no node, so the post's `mail` mark cannot be amended, plan §1
    // item 5). A reader without them maps either to `unknown`.
    Source => "source",
    MailLanded => "mail-landed",
    // Phase 5 (step 5C-1, ruling T21), appended so no kind moves: a worker's
    // task moved (`task rev= state= attention= why= code=`, the fact of the
    // timeline's `task-change` row: closed words and counters only). Local:
    // never bus class, never journaled while D3 is off, no brief line. A
    // reader without it maps it to `unknown`.
    Task => "task",
}

impl Kind {
    /// The kind a wire word names; an unknown word is [`Kind::Unknown`].
    #[must_use]
    pub fn from_word(word: &str) -> Kind {
        Kind::ALL
            .iter()
            .copied()
            .find(|k| k.word() == word)
            .unwrap_or(Kind::Unknown)
    }

    /// The level a node of this kind lives at, for a node kind; `None` for
    /// a mark, gap or snap kind.
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

    /// A mark's kind (§3.3's mark list). `agent` and `start` are node kinds
    /// too.
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
        )
    }

    /// Whether a record of `op` on a node of `lvl` may carry this kind: the
    /// per-(level, op) closure of the vocabulary.
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

/// Where a record's fact was observed (§3.3).
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
    /// Reserved for a later ruling (§4.2).
    Hook,
    /// A word this reader does not know.
    Unknown,
}

impl Src {
    /// Every known source.
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

    /// The wire word.
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

    /// The source a wire word names; unknown is [`Src::Unknown`].
    #[must_use]
    pub fn from_word(word: &str) -> Src {
        Src::ALL
            .into_iter()
            .find(|s| s.word() == word)
            .unwrap_or(Src::Unknown)
    }
}

/// How sure the source is (§3.3, §3.7). Ordinal by [`Conf::rank`]: a claim
/// ranks below every observation, so the confidence law never lets a claim
/// override one.
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
    /// Every known confidence.
    pub const ALL: [Conf; 4] = [Conf::Exact, Conf::Strong, Conf::Heuristic, Conf::Claim];

    /// The wire word.
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

    /// The confidence a wire word names; unknown is [`Conf::Unknown`].
    #[must_use]
    pub fn from_word(word: &str) -> Conf {
        Conf::ALL
            .into_iter()
            .find(|c| c.word() == word)
            .unwrap_or(Conf::Unknown)
    }

    /// The order the confidence law compares by: unknown < claim <
    /// heuristic < strong < exact.
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

    /// The reliability suffix `brief` prints (§7.4): `!` exact, `+` strong,
    /// `~` heuristic, `?` claim (and unknown).
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

/// A short opaque id: a session id, a node or record reference, an offset
/// (`@88231`), a generation. ASCII `[A-Za-z0-9._:@-]`, 1 to 40 bytes, held
/// inline.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ShortId {
    bytes: [u8; ShortId::MAX],
    len: u8,
}

impl ShortId {
    /// The longest id.
    pub const MAX: usize = 40;

    /// `s` as an id, or `None` when it is empty, too long or holds a byte
    /// outside the id alphabet.
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

    /// The id's text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        // Every byte passed `is_id_byte` (ASCII), so this never fails.
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

/// The words of the actors a connection's class names (§2.5, phase 3A ruling
/// R1): never a session's. An [`Actor::Sid`] holding one of them is not one
/// ([`Actor::sid`] makes it [`Actor::Nobody`], and [`Actor::word`] prints such
/// a value as `-`), so no record, carry line or fold snapshot ever turns a
/// name someone chose into one of these actors when it is read back
/// ([`Actor::from_word`] maps each word only to its own actor).
pub const RESERVED_ACTOR_WORDS: &[&str] =
    &["human", "ctl", "harness", "bridge", "aterm", "vendor", "-"];

/// Whether `word` is one of [`RESERVED_ACTOR_WORDS`].
#[must_use]
pub fn is_reserved_actor_word(word: &str) -> bool {
    RESERVED_ACTOR_WORDS.contains(&word)
}

/// Who a record is attributed to (§2.5). The server sets it from the
/// connection, never from a parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Actor {
    /// The window's input path.
    Human,
    /// Any other Owner client: whoever presents the instance token — the
    /// owner's own `aterm ctl`, and equally ANY process running inside an
    /// aterm session (the supervised agent itself among them). Owner class,
    /// but never proven to be the owner ([`Actor::is_proven`]).
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
    /// The wire word (`human ctl harness bridge aterm vendor <sid> -`). A
    /// [`Actor::Sid`] whose text is a reserved word ([`RESERVED_ACTOR_WORDS`])
    /// prints `-`: it was never that actor, and nothing that reads the word
    /// back may take it for one.
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

    /// A session as an actor: [`Actor::Sid`], or [`Actor::Nobody`] for a
    /// reserved word ([`RESERVED_ACTOR_WORDS`]). The one way a host turns a
    /// session's name into an actor.
    #[must_use]
    pub fn sid(id: ShortId) -> Actor {
        if is_reserved_actor_word(id.as_str()) {
            Actor::Nobody
        } else {
            Actor::Sid(id)
        }
    }

    /// The actor a wire word names: a known word (each only its own actor),
    /// else a sid-shaped token, else `-`. A reserved word never decodes to
    /// [`Actor::Sid`].
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

    /// The owner class: the person at the window and an Owner client
    /// (`ctl`). Which of them may lift another author's need is
    /// [`Actor::overrides`].
    #[must_use]
    pub const fn is_owner(&self) -> bool {
        matches!(self, Actor::Human | Actor::Ctl)
    }

    /// A claim's TRUSTED author (design §7.3, phase 3A ruling R13): the
    /// harness or the owner class. Their newest verdict and label are kept
    /// beside the newest of any author, so no other author displaces them,
    /// and a label reaches `brief` and the clause only from one of them.
    #[must_use]
    pub const fn is_trusted(&self) -> bool {
        matches!(self, Actor::Harness | Actor::Human | Actor::Ctl)
    }

    /// A PROVEN trusted author: one whose connection proves it distinct
    /// from every process inside a session — the harness (an in-memory
    /// token) and the person at the window. `ctl` is not one: the instance
    /// token is every in-session client's, the supervised agent's included,
    /// so the AI a harness judges writes as `ctl`. A proven author's newest
    /// verdict and label are kept in a slot of their own, which no `ctl`
    /// claim displaces, and only a proven author or the person lifts a need
    /// one raised ([`Actor::overrides`]).
    #[must_use]
    pub const fn is_proven(&self) -> bool {
        matches!(self, Actor::Harness | Actor::Human)
    }

    /// Whether a `clear` from `self` lifts a need `raiser` raised, beside
    /// the raiser's own (THE CLEAR RULE's authority, `fold::clear_lifts`):
    /// the person lifts any; an Owner client (`ctl`) lifts any but a proven
    /// author's ([`Actor::is_proven`]), so the agent a harness supervises
    /// cannot lift the harness's need; no other author lifts another's.
    #[must_use]
    pub fn overrides(&self, raiser: &Actor) -> bool {
        match self {
            Actor::Human => true,
            Actor::Ctl => !raiser.is_proven(),
            _ => false,
        }
    }
}

// ---------------------------------------------------------------------------
// Word lists. Each is the closed vocabulary of a word-valued key.
// ---------------------------------------------------------------------------

/// The approval box kinds: `aterm_phase::PromptKind::name()`'s words, and
/// Bash with the read-only verdict `prompt_detail` writes (§3.3), and the
/// words of kinds the reader no longer names, kept so an older log still
/// decodes (`rate-nudge`, `model-pick`, `goal-resume`: Codex's boxes, whose
/// reader was retired on 2026-10-16).
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
    "other",
];

/// The bare wall kinds: `aterm_phase::WallKind::name()`'s words.
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

/// Tool step classes (§3.3). The table mapping vendor tool names to them
/// lives in the harness's transcript reader, never here.
pub const CLS_WORDS: &[&str] = &[
    "shell", "edit", "write", "read", "search", "web", "agent", "plan", "mcp", "other",
];

/// Every `out=` word: run, turn, step and session outcomes (§3.3; the
/// session's are `ExitReason`'s tokens and `gone`).
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

/// Every need word (§3.3), `approval:<kind>` carrying the box kind without
/// Bash's read-only verdict.
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
];

/// The `word=` vocabulary: the agent verdict words, `act` words, `verdict`
/// words and need words a `need`/`clear` mark names.
pub const WORD_WORDS: &[&str] = &[
    // agent verdict
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
    // act
    "approve",
    "deny",
    "answer",
    "continue",
    "relaunch",
    "escalate",
    "interrupt",
    "dispatch",
    // verdict
    "on-track",
    "off-track",
    "blocked",
    "done",
    "failed",
    "needs-human",
    "unsure",
    // needs a mark raises or clears
    "review",
    "decision",
    "help",
    "escalated",
    "failure",
    "unresponsive",
    "exited",
    // story: the closed set `aterm ctl story <verb>` tells
    // (`aterm_messages::presence::StoryVerb::TOLD_WORDS`; phase 3B)
    "approved",
    "dismissed",
    "reconnected",
    "timeout",
    "exit",
    "compacted",
    "warned",
    "chose",
    // an inbox receipt's verdict (`aterm_types::fabric_grammar::VERDICTS`)
    "handled",
    "refused",
    "deferred",
];

/// Why records were lost (`gap why=`).
pub const WHY_WORDS: &[&str] = &[
    "handoff",
    "crash",
    "pruned",
    "evicted",
    "reset",
    "unread",
    "dropped",
    "unknown",
    // phase 5 (5C-1): what moved a worker's task (`task why=`), the closed
    // `aterm-gui` task vocabulary, held by `task_record`'s tests (T16)
    "marker",
    "done-check",
    "exit",
    "shell",
    "startup",
    "held",
    "turn",
    "moved",
];

/// The input verbs an `input` mark names.
pub const VERB_WORDS: &[&str] = &["send", "key", "paste", "feed", "turn", "unknown"];

/// What a control input says it means for the box it dismisses (`key`/`send
/// act=`, ruling T1): [`crate::ActWord`]'s words, in its order.
pub const ACT_WORDS: &[&str] = &["approve", "deny", "answer"];

/// A mail's direction.
pub const DIR_WORDS: &[&str] = &["in", "out", "unknown"];

/// A mail's kind: every kind `deliver` accepts
/// (`aterm_types::fabric_grammar::KINDS`, held by `vocab_mirror.rs`).
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

/// The phases a `status` mark names (the status classifier's words).
pub const PHASE_WORDS: &[&str] = &[
    "idle", "running", "waiting", "prompt", "exited", "unknown", "starting", "quiet",
];

/// Shell-integration, progress, render and source states.
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
    // phase 3B: a degraded shell integration (`ShellIntegrationPosture`), and
    // the resize ledger's render verdicts
    "degraded",
    "desync-risk",
    "healed",
    "unjudged",
    // phase 5 (5C-1): a worker's task state (`task state=`), the closed
    // `aterm-gui` task states, held by `task_record`'s tests (T16)
    "working",
    "done",
    "exited",
    "stalled",
    // phase 5 (5J-2): the fabric link's states (`link state=` on the node
    // log), `aterm_gui_fabric::fabric_state`'s words; `stalled` is above
    "connected",
    "disconnected",
    "absent",
];

/// How the previous run of this instance ended (`start prev_end=` on the
/// node log, 5J): `aterm_update::recovery_ledger::EndClass`'s row tokens, a
/// caught signal as `signal` with its number in `code=`, and `-` while the
/// recovery ledger has not been read (a launch that never reads it, a
/// headless or dev one, keeps `-`).
pub const END_WORDS: &[&str] = &[
    "clean", "handoff", "killed", "signal", "panic", "running", "unknown", "none", "-",
];

/// What a `cause=` names (`cause_k=`): mail by its offset, or a record.
pub const CAUSE_WORDS: &[&str] = &["mail", "rec", "unknown"];

/// How a duration was measured: `dur=wall` across an update.
pub const DUR_WORDS: &[&str] = &["mono", "wall", "unknown"];

/// The static word `word` names in `list`, or `None`.
#[must_use]
pub fn lookup(list: &'static [&'static str], word: &str) -> Option<&'static str> {
    list.iter().copied().find(|w| *w == word)
}

/// The static word `word` names in `list`, or [`UNKNOWN`].
#[must_use]
pub fn lookup_or_unknown(list: &'static [&'static str], word: &str) -> &'static str {
    lookup(list, word).unwrap_or(UNKNOWN)
}
