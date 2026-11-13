// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The request journal's records, each one line: `t=<unix ms> op=<word> req=<id>` and the op's own
//! keys in a fixed order, every value percent-encoded ([`aterm_codec::pct`]) and every line at most
//! [`aterm_slog::MAX_RECORD_BYTES`]. The decoders are strict: a line out of shape (torn, foreign,
//! too long, a value not in its canonical encoding) is `None`, which a load skips and counts.
//!
//! [`Rec`] is the journal's (wire and host tiers); [`Said`] the content tier's, kept in its own
//! file so a reader that serves no content never opens it.

use crate::claim::{CLAIM_PATH_BYTES, Claim, ClaimSrc, TouchWhy};
use aterm_codec::pct;
use aterm_slog::MAX_RECORD_BYTES;
use aterm_types::control_verbs::{REQUEST_WHY_BYTES, REQUESTER_ACCOUNT_BYTES, request_id};

/// The most bytes a requester's host takes percent-encoded.
pub const HOST_BYTES: usize = 64;

/// The most bytes a requester's node id takes: a legacy node id's 32 hex digits, which a mesh
/// principal (`n-` and 16 hex digits) is under.
pub const NODE_BYTES: usize = 32;

/// The most bytes an actor token (`by=`) or a tab's sid takes: `aterm_slog`'s `ShortId::MAX`,
/// which every actor token fits.
pub const TOKEN_MAX: usize = 40;

/// The most bytes a reply's mail key takes: a `post key=`'s cap (`aterm_gui_fabric::KEY_MAX`).
pub const KEY_MAX: usize = 64;

/// How a request came in (the design's §5 intake table).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Via {
    /// A fleet drive's plain shell (`spawn cwd=… why=…`).
    FleetShell,
    /// A fleet drive's worker (`spawn agent=…`).
    FleetWorker,
    /// A local `aterm ctl spawn` from an agent's tab.
    CtlSpawn,
}

impl Via {
    /// Its `via=` word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::FleetShell => "fleet-shell",
            Self::FleetWorker => "fleet-worker",
            Self::CtlSpawn => "ctl-spawn",
        }
    }

    fn of(word: &str) -> Option<Self> {
        [Self::FleetShell, Self::FleetWorker, Self::CtlSpawn]
            .into_iter()
            .find(|via| via.word() == word)
    }
}

/// What the spawn that opened a request made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Form {
    /// A plain shell tab.
    Shell,
    /// A worker of this agent word (`agent:claude`).
    Agent(String),
    /// A program worker (`spawn program=`).
    Program,
    /// A connected pair (`spawn connected=…`).
    Connected,
}

impl Form {
    /// Its `form=` word.
    #[must_use]
    pub fn word(&self) -> String {
        match self {
            Self::Shell => "shell".to_owned(),
            Self::Agent(agent) => format!("agent:{agent}"),
            Self::Program => "program".to_owned(),
            Self::Connected => "connected".to_owned(),
        }
    }

    fn of(word: &str) -> Option<Self> {
        match word {
            "shell" => Some(Self::Shell),
            "program" => Some(Self::Program),
            "connected" => Some(Self::Connected),
            _ => word
                .strip_prefix("agent:")
                .filter(|agent| is_agent_word(agent))
                .map(|agent| Self::Agent(agent.to_owned())),
        }
    }
}

/// Whether `agent` is a worker's agent word as a form keeps it: 1 to 16 lowercase letters,
/// digits and `-`.
#[must_use]
pub fn is_agent_word(agent: &str) -> bool {
    (1..=16).contains(&agent.len())
        && agent
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Where a request's why came from (the design's §5: `why=`, or the documented fallback).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WhySrc {
    /// The spawn's own `why=`.
    Given,
    /// The first line of a worker's brief.
    Brief,
    /// The requesting tab's goal line.
    Goal,
    /// The requesting tab's last asked line.
    Asked,
    /// Nothing: the why reads `(none given)`.
    None,
}

impl WhySrc {
    /// Its `why_src=` word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Given => "given",
            Self::Brief => "brief",
            Self::Goal => "goal",
            Self::Asked => "asked",
            Self::None => "none",
        }
    }

    fn of(word: &str) -> Option<Self> {
        [
            Self::Given,
            Self::Brief,
            Self::Goal,
            Self::Asked,
            Self::None,
        ]
        .into_iter()
        .find(|src| src.word() == word)
    }
}

/// A later verb from a request's requester on a tab the request owns.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Verb {
    /// `turn`.
    Turn,
    /// `send`.
    Send,
    /// `key`.
    Key,
    /// `close`.
    Close,
}

impl Verb {
    /// Every verb that attaches.
    pub const ALL: [Self; 4] = [Self::Turn, Self::Send, Self::Key, Self::Close];

    /// Its control verb.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Turn => "turn",
            Self::Send => "send",
            Self::Key => "key",
            Self::Close => "close",
        }
    }

    /// The verb a control verb `word` is, where it attaches.
    #[must_use]
    pub fn of(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|verb| verb.word() == word)
    }
}

/// How a tab, or a whole request, came out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Out {
    /// It did what it was for.
    Done,
    /// It failed.
    Failed,
    /// It ended without saying either: its requester left it, or it stood undriven and idle
    /// past its lease, or the aterm hosting it ended.
    Abandoned,
    /// Its requester let it go (a plain tab's close), or a person took it (Keep as Tab).
    Released,
}

impl Out {
    /// Its `out=` word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Abandoned => "abandoned",
            Self::Released => "released",
        }
    }

    fn of(word: &str) -> Option<Self> {
        [Self::Done, Self::Failed, Self::Abandoned, Self::Released]
            .into_iter()
            .find(|out| out.word() == word)
    }
}

/// Where a worker tab's task stood when the tab left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskEnd {
    /// Concluded finished.
    Finished,
    /// Concluded failed.
    Failed,
    /// Not concluded.
    Working,
}

impl TaskEnd {
    /// Its `task=` word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Finished => "finished",
            Self::Failed => "failed",
            Self::Working => "working",
        }
    }

    fn of(word: &str) -> Option<Self> {
        [Self::Finished, Self::Failed, Self::Working]
            .into_iter()
            .find(|task| task.word() == word)
    }
}

/// Why a request concluded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndWhy {
    /// Its last tab left.
    TabsLeft,
    /// The spawn that opened it was refused, so it has no tab.
    SpawnRefused,
    /// A leased plain shell stood undriven with nothing running in it past its lease
    /// ([`crate::store::LEASE_IDLE_MS`]).
    Idle,
    /// A leased plain shell was idle once its requester's Mac no longer answered the fleet.
    RequesterLeft,
    /// The aterm hosting its tabs ended without their leaving (a crash, a power loss), and the
    /// next one does not host them.
    Relaunch,
    /// A person kept its last open tab (Keep as Tab).
    Kept,
}

impl EndWhy {
    /// Its `why=` word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::TabsLeft => "tabs-left",
            Self::SpawnRefused => "spawn-refused",
            Self::Idle => "idle",
            Self::RequesterLeft => "requester-left",
            Self::Relaunch => "relaunch",
            Self::Kept => "kept",
        }
    }

    fn of(word: &str) -> Option<Self> {
        [
            Self::TabsLeft,
            Self::SpawnRefused,
            Self::Idle,
            Self::RequesterLeft,
            Self::Relaunch,
            Self::Kept,
        ]
        .into_iter()
        .find(|why| why.word() == word)
    }
}

/// A worker's brief as a request keeps it: its digest and size (the text stays the worker's).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Brief {
    /// SHA-256, 64 lowercase hex digits.
    pub sha256: String,
    /// Its length.
    pub bytes: u64,
}

impl Brief {
    /// The brief of these bytes.
    #[must_use]
    pub fn of(text: &[u8]) -> Self {
        let digest = aterm_digest::Sha256::digest(text);
        Self {
            sha256: aterm_codec::hex::encode_lower(&digest),
            bytes: text.len() as u64,
        }
    }
}

/// What a request tab's process tree spent, as its leaving records it: each figure a floor
/// (a process that ended between two samples is missed), `None` where nothing was read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Spent {
    /// CPU, whole seconds.
    pub cpu_s: Option<u64>,
    /// The largest resident set, whole MiB.
    pub peak_mb: Option<u64>,
    /// Bytes written to disk, whole MiB.
    pub written_mb: Option<u64>,
}

impl Spent {
    /// Whether nothing was read.
    #[must_use]
    pub const fn is_unread(&self) -> bool {
        self.cpu_s.is_none() && self.peak_mb.is_none() && self.written_mb.is_none()
    }

    /// The larger of each figure of `self` and `other`, where either was read.
    #[must_use]
    pub fn max(self, other: Self) -> Self {
        let max = |a: Option<u64>, b: Option<u64>| match (a, b) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        Self {
            cpu_s: max(self.cpu_s, other.cpu_s),
            peak_mb: max(self.peak_mb, other.peak_mb),
            written_mb: max(self.written_mb, other.written_mb),
        }
    }
}

/// A request's `open`: who asked, how, and where its why came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Open {
    /// How it came in.
    pub via: Via,
    /// What its spawn made.
    pub form: Form,
    /// The requester's whole node id, proven by its node key for a drive (an id names only its
    /// head, [`crate::id::head_of`]); empty for a requester on a Mac with none.
    pub node: String,
    /// The requester's Mac as a person reads it; empty where unknown.
    pub host: String,
    /// The requester as every other record names it (`fleet:<head>:<sid|->`, or a local tab's
    /// sid): the key a later verb and `req=` match on.
    pub by: String,
    /// The program in the requesting session.
    pub program: Option<String>,
    /// Its model.
    pub model: Option<String>,
    /// Its account (masked).
    pub account: Option<String>,
    /// Where the why came from.
    pub why_src: WhySrc,
}

/// One journal record's op and its keys.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Body {
    /// The request opened.
    Open(Open),
    /// Its given why (`why_src=given`): the requester's one line, host tier.
    Why {
        /// The line.
        text: String,
    },
    /// Its worker's brief.
    Brief(Brief),
    /// A tab its spawn made.
    Tab {
        /// The tab's sid.
        tab: String,
        /// Who asked for it.
        by: String,
    },
    /// A later verb from its requester on one of its tabs.
    Attach {
        /// The tab.
        tab: String,
        /// The verb.
        verb: Verb,
        /// How many of that verb on that tab, this one included.
        n: u64,
        /// Its requester.
        by: String,
    },
    /// One of its tabs left.
    Left {
        /// The tab.
        tab: String,
        /// Why it left (an exit reason's word: `shell-exit`, `ctl-close`, `ui-close`, …).
        reason: String,
        /// Its shell's exit code, where one was read.
        code: Option<i32>,
        /// Where its task stood, for a worker.
        task: Option<TaskEnd>,
        /// How the tab came out.
        out: Out,
    },
    /// A person kept one of its tabs (Keep as Tab): the tab is theirs, and no later verb of the
    /// requester attaches to it.
    Keep {
        /// The tab.
        tab: String,
        /// Who kept it.
        by: String,
    },
    /// It concluded.
    End {
        /// How.
        out: Out,
        /// Why.
        why: EndWhy,
    },
    /// A folder one of its spawns opened in, which each look lists for folders born in it.
    Root {
        /// The folder ([`crate::claim::normal`] form).
        path: String,
        /// The tab whose spawn opened in it: written last, so a row cut inside the path (whose
        /// every prefix is a path too) lacks it and reads as torn.
        tab: String,
    },
    /// A folder it claims.
    Claim(Claim),
    /// A person worked in one of its claimed folders.
    Touched {
        /// The claimed folder.
        path: String,
        /// The tab the command ran in.
        tab: String,
        /// The command's block in that tab.
        blk: u64,
        /// Why it counts as a person's.
        why: TouchWhy,
    },
    /// What one of its tabs' process trees spent, written as it left: its tab last, so a row cut
    /// inside a figure lacks it and reads as torn.
    Spent {
        /// The tab.
        tab: String,
        /// The figures.
        spent: Spent,
    },
    /// A person reviewed it (Mark Reviewed, `request review`): its note before its `by`, so a
    /// row cut inside the note lacks the token and reads as torn.
    Review {
        /// The note, one line ([`crate::why::line_of`]).
        note: Option<String>,
        /// Who reviewed it.
        by: String,
    },
    /// A person kept it (`request keep`): its tabs and folders are theirs, and no cleanup ever
    /// touches them.
    Kept {
        /// Who kept it.
        by: String,
    },
    /// A reply went to its requester (`request reply`), its text in the content tier under the
    /// same key ([`SaidBody::Reply`]).
    Reply {
        /// The mail's idempotency key.
        key: String,
        /// Whether anything took the mail.
        sent: bool,
        /// Who replied.
        by: String,
    },
}

impl Body {
    const fn op(&self) -> &'static str {
        match self {
            Self::Open(_) => "open",
            Self::Why { .. } => "why",
            Self::Brief(_) => "brief",
            Self::Tab { .. } => "tab",
            Self::Attach { .. } => "attach",
            Self::Left { .. } => "left",
            Self::Keep { .. } => "keep",
            Self::End { .. } => "end",
            Self::Root { .. } => "root",
            Self::Claim(_) => "claim",
            Self::Touched { .. } => "touched",
            Self::Spent { .. } => "spent",
            Self::Review { .. } => "review",
            Self::Kept { .. } => "kept",
            Self::Reply { .. } => "reply",
        }
    }
}

/// One journal record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rec {
    /// When, Unix milliseconds.
    pub t_ms: u64,
    /// The request.
    pub id: String,
    /// What.
    pub body: Body,
}

/// One content-tier record's op and its keys.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SaidBody {
    /// The why a fallback took (`why_src` brief, goal or asked).
    Why {
        /// The line.
        text: String,
        /// Where it came from.
        src: WhySrc,
    },
    /// The requesting tab's goal line at intake.
    Goal {
        /// The line.
        text: String,
    },
    /// The requesting tab's last asked line at intake.
    Asked {
        /// The line.
        text: String,
    },
    /// A reply's text, under its journal row's key ([`Body::Reply`]), which follows it: a row cut
    /// inside the text lacks the key and reads as torn.
    Reply {
        /// The line.
        text: String,
        /// The reply's key.
        key: String,
    },
}

/// One content-tier record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Said {
    /// When, Unix milliseconds.
    pub t_ms: u64,
    /// The request.
    pub id: String,
    /// What.
    pub body: SaidBody,
}

fn opt(value: Option<&str>) -> String {
    value.map_or_else(|| "-".to_owned(), pct::encode)
}

fn head(t_ms: u64, op: &str, id: &str) -> String {
    format!("t={t_ms} op={op} req={id}")
}

impl Rec {
    /// The record's line, no newline. A record built from values in their shapes fits the line
    /// cap ([`crate::store::Store`] clamps what it records); [`Self::parse`] refuses one that
    /// does not.
    #[must_use]
    pub fn line(&self) -> String {
        let mut line = head(self.t_ms, self.body.op(), &self.id);
        let rest = match &self.body {
            Body::Open(open) => format!(
                "via={} form={} node={} host={} by={} program={} model={} account={} why_src={}",
                open.via.word(),
                open.form.word(),
                if open.node.is_empty() {
                    "-"
                } else {
                    &open.node
                },
                if open.host.is_empty() {
                    "-".to_owned()
                } else {
                    pct::encode(&open.host)
                },
                open.by,
                opt(open.program.as_deref()),
                opt(open.model.as_deref()),
                opt(open.account.as_deref()),
                open.why_src.word(),
            ),
            Body::Why { text } => format!("text={}", pct::encode(text)),
            Body::Brief(brief) => format!("sha256={} bytes={}", brief.sha256, brief.bytes),
            Body::Tab { tab, by } => format!("tab={tab} by={by}"),
            Body::Attach { tab, verb, n, by } => {
                format!("tab={tab} verb={} n={n} by={by}", verb.word())
            }
            Body::Left {
                tab,
                reason,
                code,
                task,
                out,
            } => format!(
                "tab={tab} reason={reason} code={} task={} out={}",
                code.map_or_else(|| "-".to_owned(), |code| code.to_string()),
                task.map_or("-", TaskEnd::word),
                out.word()
            ),
            Body::Keep { tab, by } => format!("tab={tab} by={by}"),
            Body::End { out, why } => format!("out={} why={}", out.word(), why.word()),
            Body::Root { path, tab } => format!("path={} tab={tab}", pct::encode(path)),
            Body::Claim(claim) => format!(
                "path={} src={} by={} tab={} born_ms={} alone={}",
                pct::encode(&claim.path),
                claim.src.word(),
                claim.by,
                claim.tab.as_deref().unwrap_or("-"),
                num_or_dash(claim.born_ms),
                claim
                    .alone
                    .map_or("-", |alone| if alone { "1" } else { "0" }),
            ),
            Body::Touched {
                path,
                tab,
                blk,
                why,
            } => format!(
                "path={} tab={tab} blk={blk} why={}",
                pct::encode(path),
                why.word()
            ),
            Body::Spent { tab, spent } => format!(
                "cpu_s={} peak_mb={} written_mb={} tab={tab}",
                num_or_dash(spent.cpu_s),
                num_or_dash(spent.peak_mb),
                num_or_dash(spent.written_mb),
            ),
            Body::Review { note, by } => format!("note={} by={by}", opt(note.as_deref())),
            Body::Kept { by } => format!("by={by}"),
            Body::Reply { key, sent, by } => {
                format!("key={key} sent={} by={by}", if *sent { "1" } else { "0" })
            }
        };
        line.push(' ');
        line.push_str(&rest);
        line
    }

    /// [`Self::line`]'s inverse, strict.
    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        let (t_ms, op, id, mut keys) = Keys::head(line)?;
        let body = match op {
            "open" => {
                let open = Open {
                    via: Via::of(keys.take("via")?)?,
                    form: Form::of(keys.take("form")?)?,
                    node: match keys.take("node")? {
                        "-" => String::new(),
                        node => Some(node).filter(|node| is_node_word(node))?.to_owned(),
                    },
                    host: match keys.take("host")? {
                        "-" => String::new(),
                        host => text(host, HOST_BYTES)?,
                    },
                    by: token(keys.take("by")?)?,
                    program: requester_word(keys.take("program")?)?,
                    model: requester_word(keys.take("model")?)?,
                    account: match keys.take("account")? {
                        "-" => None,
                        account => Some(text(account, REQUESTER_ACCOUNT_BYTES)?),
                    },
                    why_src: WhySrc::of(keys.take("why_src")?)?,
                };
                Body::Open(open)
            }
            "why" => Body::Why {
                text: text(keys.take("text")?, REQUEST_WHY_BYTES)?,
            },
            "brief" => Body::Brief(Brief {
                sha256: keys
                    .take("sha256")
                    .filter(|hex| hex.len() == 64 && aterm_codec::hex::is_lower(hex, 64))?
                    .to_owned(),
                bytes: number(keys.take("bytes")?)?,
            }),
            "tab" => Body::Tab {
                tab: sid(keys.take("tab")?)?,
                by: token(keys.take("by")?)?,
            },
            "attach" => Body::Attach {
                tab: sid(keys.take("tab")?)?,
                verb: Verb::of(keys.take("verb")?)?,
                n: number(keys.take("n")?).filter(|n| *n > 0)?,
                by: token(keys.take("by")?)?,
            },
            "left" => Body::Left {
                tab: sid(keys.take("tab")?)?,
                reason: keys
                    .take("reason")
                    .filter(|reason| {
                        (1..=16).contains(&reason.len())
                            && reason.bytes().all(|b| b.is_ascii_lowercase() || b == b'-')
                    })?
                    .to_owned(),
                code: match keys.take("code")? {
                    "-" => None,
                    code => Some(code.parse().ok().filter(|n: &i32| n.to_string() == code)?),
                },
                task: match keys.take("task")? {
                    "-" => None,
                    task => Some(TaskEnd::of(task)?),
                },
                out: Out::of(keys.take("out")?)?,
            },
            "keep" => Body::Keep {
                tab: sid(keys.take("tab")?)?,
                by: token(keys.take("by")?)?,
            },
            "end" => Body::End {
                out: Out::of(keys.take("out")?)?,
                why: EndWhy::of(keys.take("why")?)?,
            },
            "root" => Body::Root {
                path: path(keys.take("path")?)?,
                tab: sid(keys.take("tab")?)?,
            },
            "claim" => Body::Claim(Claim {
                path: path(keys.take("path")?)?,
                src: ClaimSrc::of(keys.take("src")?)?,
                by: token(keys.take("by")?)?,
                tab: match keys.take("tab")? {
                    "-" => None,
                    tab => Some(sid(tab)?),
                },
                born_ms: maybe_number(keys.take("born_ms")?)?,
                alone: match keys.take("alone")? {
                    "-" => None,
                    "1" => Some(true),
                    "0" => Some(false),
                    _ => return None,
                },
            }),
            "touched" => Body::Touched {
                path: path(keys.take("path")?)?,
                tab: sid(keys.take("tab")?)?,
                blk: number(keys.take("blk")?)?,
                why: TouchWhy::of(keys.take("why")?)?,
            },
            "spent" => {
                let spent = Spent {
                    cpu_s: maybe_number(keys.take("cpu_s")?)?,
                    peak_mb: maybe_number(keys.take("peak_mb")?)?,
                    written_mb: maybe_number(keys.take("written_mb")?)?,
                };
                Body::Spent {
                    tab: sid(keys.take("tab")?)?,
                    spent,
                }
            }
            "review" => Body::Review {
                note: match keys.take("note")? {
                    "-" => None,
                    note => Some(line_text(note)?),
                },
                by: token(keys.take("by")?)?,
            },
            "kept" => Body::Kept {
                by: token(keys.take("by")?)?,
            },
            "reply" => Body::Reply {
                key: mail_key(keys.take("key")?)?,
                sent: match keys.take("sent")? {
                    "1" => true,
                    "0" => false,
                    _ => return None,
                },
                by: token(keys.take("by")?)?,
            },
            _ => return None,
        };
        keys.done()?;
        Some(Self { t_ms, id, body })
    }
}

impl Said {
    /// The record's line, no newline.
    #[must_use]
    pub fn line(&self) -> String {
        let (op, rest) = match &self.body {
            SaidBody::Why { text, src } => (
                "why",
                format!("text={} src={}", pct::encode(text), src.word()),
            ),
            SaidBody::Goal { text } => ("goal", format!("text={}", pct::encode(text))),
            SaidBody::Asked { text } => ("asked", format!("text={}", pct::encode(text))),
            SaidBody::Reply { text, key } => {
                ("reply", format!("text={} key={key}", pct::encode(text)))
            }
        };
        format!("{} {rest}", head(self.t_ms, op, &self.id))
    }

    /// [`Self::line`]'s inverse, strict.
    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        let (t_ms, op, id, mut keys) = Keys::head(line)?;
        let body = match op {
            "why" => SaidBody::Why {
                text: text(keys.take("text")?, REQUEST_WHY_BYTES)?,
                src: WhySrc::of(keys.take("src")?)
                    .filter(|src| matches!(src, WhySrc::Brief | WhySrc::Goal | WhySrc::Asked))?,
            },
            "goal" => SaidBody::Goal {
                text: text(keys.take("text")?, REQUEST_WHY_BYTES)?,
            },
            "asked" => SaidBody::Asked {
                text: text(keys.take("text")?, REQUEST_WHY_BYTES)?,
            },
            "reply" => SaidBody::Reply {
                text: line_text(keys.take("text")?)?,
                key: mail_key(keys.take("key")?)?,
            },
            _ => return None,
        };
        keys.done()?;
        Some(Self { t_ms, id, body })
    }
}

/// A line's `k=v` words, taken in order.
struct Keys<'a> {
    words: std::str::Split<'a, char>,
}

impl<'a> Keys<'a> {
    /// The line's `t=`, `op=` and `req=`, and the keys after them; `None` for a line over the cap
    /// or out of that shape.
    fn head(line: &'a str) -> Option<(u64, &'a str, String, Self)> {
        if line.len() > MAX_RECORD_BYTES {
            return None;
        }
        let mut keys = Self {
            words: line.split(' '),
        };
        let t_ms = number(keys.take("t")?)?;
        let op = keys.take("op")?;
        let id = request_id(keys.take("req")?).ok()?.to_owned();
        Some((t_ms, op, id, keys))
    }

    fn take(&mut self, key: &str) -> Option<&'a str> {
        let word = self.words.next()?;
        let value = word.strip_prefix(key)?.strip_prefix('=')?;
        (!value.is_empty()).then_some(value)
    }

    fn done(mut self) -> Option<()> {
        self.words.next().is_none().then_some(())
    }
}

fn number(value: &str) -> Option<u64> {
    value.parse().ok().filter(|n: &u64| n.to_string() == value)
}

/// `-` or a number in its canonical form.
fn maybe_number(value: &str) -> Option<Option<u64>> {
    match value {
        "-" => Some(None),
        value => number(value).map(Some),
    }
}

fn num_or_dash(value: Option<u64>) -> String {
    value.map_or_else(|| "-".to_owned(), |n| n.to_string())
}

/// A claimed folder: a percent-encoded path in its canonical encoding, within
/// [`CLAIM_PATH_BYTES`], already in [`crate::claim::normal`] form.
fn path(value: &str) -> Option<String> {
    let decoded = text(value, CLAIM_PATH_BYTES)?;
    (crate::claim::normal(&decoded).ok()? == decoded).then_some(decoded)
}

/// A percent-encoded value in its canonical encoding, decoded, within `cap` encoded bytes.
fn text(value: &str, cap: usize) -> Option<String> {
    let decoded = pct::decode(value);
    (value.len() <= cap && pct::encode(&decoded) == value).then_some(decoded)
}

/// A person's one line (a review's note, a reply's text): a percent-encoded value within
/// [`REQUEST_WHY_BYTES`] that [`crate::why::line_of`] keeps as it is.
fn line_text(value: &str) -> Option<String> {
    let decoded = text(value, REQUEST_WHY_BYTES)?;
    (crate::why::line_of(&decoded).as_deref() == Some(decoded.as_str())).then_some(decoded)
}

/// A reply's mail key ([`is_mail_key`]).
fn mail_key(value: &str) -> Option<String> {
    is_mail_key(value).then(|| value.to_owned())
}

/// Whether `key` is a mail key as a `post key=` takes one: 1 to [`KEY_MAX`] ASCII letters,
/// digits, `.`, `_`, `:` and `-`.
#[must_use]
pub fn is_mail_key(key: &str) -> bool {
    (1..=KEY_MAX).contains(&key.len())
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

/// A tab's sid: a session aterm minted.
fn sid(value: &str) -> Option<String> {
    aterm_types::control_verbs::is_minted_sid(value).then(|| value.to_owned())
}

/// An actor token ([`is_token`]).
fn token(value: &str) -> Option<String> {
    is_token(value).then(|| value.to_owned())
}

/// Whether `value` is an actor token as every record of a request names one, whole: a session
/// aterm minted (`s-` and 20 hex digits); a fleet drive's `fleet:<head>:<sid|->`, its head 1 to
/// 8 bytes of an id; or one of `human`, `ctl`, `harness`, `bridge` and `-`. Holding each to its
/// whole shape is what refuses a row cut inside it.
#[must_use]
pub fn is_token(value: &str) -> bool {
    use aterm_types::control_verbs::is_minted_sid;
    if is_minted_sid(value) || matches!(value, "human" | "ctl" | "harness" | "bridge" | "-") {
        return true;
    }
    let Some((head, sid)) = value
        .strip_prefix("fleet:")
        .and_then(|rest| rest.split_once(':'))
    else {
        return false;
    };
    (1..=8).contains(&head.len())
        && head
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'@' | b'-'))
        && (sid == "-" || is_minted_sid(sid))
        && value.len() <= TOKEN_MAX
}

/// Whether `node` is a node id as an `open` keeps it: 1 to [`NODE_BYTES`] ASCII letters, digits
/// and `-`, and not `-` alone (which reads as none).
#[must_use]
pub fn is_node_word(node: &str) -> bool {
    (1..=NODE_BYTES).contains(&node.len())
        && node != "-"
        && node.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// A requester's program or model: `-`, or a word in the requester's token shape.
fn requester_word(value: &str) -> Option<Option<String>> {
    match value {
        "-" => Some(None),
        word => aterm_types::control_verbs::requester_token(word)
            .filter(|token| token == word)
            .map(Some),
    }
}

#[cfg(test)]
#[path = "record_tests.rs"]
mod tests;
