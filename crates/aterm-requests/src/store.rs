// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The request store: the journal's records folded into requests, and the writes intake makes.
//!
//! A [`Store`] keeps the fold of every record it has read and where it stopped (the journal's
//! device, inode and length). Before each write, under the journal's lock, and before each
//! lookup, it reads only the bytes appended since, or the whole file again where another process
//! replaced it ([`Store::compact`]); so the process a seamless update starts, or a second
//! instance on the same state home, finds every request the other made, and a lost cache costs
//! one read.
//!
//! A requester is its proven node and its actor token together: the token's node head is eight
//! characters, which another Mac could match, so `req=`, a later verb and a requester's close
//! count as the requester's only where both are its own.

use crate::claim::{Claim, ClaimSrc, HeldClaim, MAX_CLAIMS, MAX_ROOTS, TouchWhy};
use crate::record::{
    Body, Brief, EndWhy, Form, HOST_BYTES, Open, Out, Rec, Said, SaidBody, Spent, TaskEnd, Verb,
    Via, WhySrc, is_node_word, is_token,
};
use crate::why::Why;
use aterm_types::control_verbs::{REQUEST_WHY_BYTES, REQUESTER_ACCOUNT_BYTES};
use aterm_uds::line_log::{self, Held, Identity};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io;
use std::path::{Path, PathBuf};

/// The most of the journal a load reads: its newest 64 MiB (the session log journal's budget,
/// `aterm_slog::journal::BUDGET_BYTES`).
pub const READ_MAX: u64 = 64 * 1024 * 1024;

/// The journal is compacted ([`Store::compact`]) once an append leaves it longer than this and at
/// least half of it is `attach` rows a later one supersedes, so a compaction at least halves the
/// file and a journal of live records is never rewritten for its size alone.
pub const JOURNAL_COMPACT: u64 = 16 * 1024 * 1024;

/// How long a leased plain shell may stand undriven with nothing running in it before its
/// request concludes abandoned (the requests design's §6): 30 minutes from the later of its
/// requester's last typed verb and its shell's last command's end.
pub const LEASE_IDLE_MS: u64 = 30 * 60 * 1000;

/// Who asked for a request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct From {
    /// The requester's whole node id: a drive's proven node, or this Mac's for a local tab;
    /// empty where it has none. A request's id names its head ([`crate::id::head_of`]).
    pub node: String,
    /// The requester's Mac as a person reads it.
    pub host: String,
    /// The requester's actor token (`fleet:<head>:<sid|->`, or a local tab's sid).
    pub by: String,
    /// The program in the requesting session.
    pub program: Option<String>,
    /// Its model.
    pub model: Option<String>,
    /// Its account (masked).
    pub account: Option<String>,
}

/// Everything a spawn's intake records when it opens a request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Intake {
    /// How it came in.
    pub via: Via,
    /// What the spawn makes.
    pub form: Form,
    /// Who asked.
    pub from: From,
    /// Its why ([`crate::why_of`]).
    pub why: Why,
    /// A worker's brief.
    pub brief: Option<Brief>,
    /// The requesting tab's goal line (content tier).
    pub goal: Option<String>,
    /// The requesting tab's last asked line (content tier).
    pub asked: Option<String>,
}

/// One of a request's tabs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tab {
    /// Its sid.
    pub sid: String,
    /// When its `tab` record was written (the spawn landed), Unix milliseconds.
    pub opened_ms: u64,
    /// The verbs its requester sent it, counted.
    pub verbs: BTreeMap<Verb, u64>,
    /// How it came out, once it left.
    pub left: Option<Out>,
    /// When its requester last drove it, Unix milliseconds: its `tab` record's time, raised by
    /// each `turn`, `send` and `key` that attached (a `close` is no drive).
    pub last_ms: u64,
    /// A person kept it (Keep as Tab): it is theirs, and counts as released.
    pub kept: bool,
    /// When a person kept it, Unix milliseconds.
    pub kept_ms: Option<u64>,
    /// What its process tree spent, once it left.
    pub spent: Option<Spent>,
    /// How it left, once it did: when, and the close's own words.
    pub gone: Option<Gone>,
}

/// How a request's tab left, as its `left` row says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Gone {
    /// When, Unix milliseconds.
    pub at_ms: u64,
    /// Why (an exit reason's word: `shell-exit`, `ctl-close`, `relaunch`, …).
    pub reason: String,
    /// Its shell's exit code, where one was read.
    pub code: Option<i32>,
    /// Where its task stood, for a worker.
    pub task: Option<TaskEnd>,
}

impl Tab {
    /// Whether it is still its request's: it has not left and no person kept it.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.left.is_none() && !self.kept
    }

    /// How it stands for its request's conclusion: how it left, released where a person kept
    /// it, `None` while it is open.
    #[must_use]
    pub fn out(&self) -> Option<Out> {
        self.left.or(self.kept.then_some(Out::Released))
    }
}

/// Where a request stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Opened; no tab yet.
    Open,
    /// A tab of it is open.
    Running,
    /// No tab is open (each left or was kept), its spawn was refused, its lease ended, or a
    /// relaunch found its tabs gone.
    Concluded(Out),
}

/// Where a request stands for the person who reviews it: [`State`], with a concluded request
/// reviewed or not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// Opened; no tab yet.
    Open,
    /// A tab of it is open.
    Running,
    /// Concluded, and no person has reviewed it.
    Concluded,
    /// Concluded and reviewed.
    Reviewed,
}

impl Stage {
    /// Its `stage=` word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Running => "running",
            Self::Concluded => "concluded",
            Self::Reviewed => "reviewed",
        }
    }
}

/// A person's review of a request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Review {
    /// Who first reviewed it (an actor token: `human`, `ctl` or a shell tab's sid).
    pub by: String,
    /// When, Unix milliseconds.
    pub at_ms: u64,
    /// The newest note a review gave.
    pub note: Option<String>,
}

/// A reply that went to a request's requester.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reply {
    /// Who replied.
    pub by: String,
    /// When, Unix milliseconds.
    pub at_ms: u64,
    /// The mail's key, which its text in the content tier is kept under.
    pub key: String,
    /// Whether anything took the mail.
    pub sent: bool,
}

/// The most replies a request folds: its newest.
pub const MAX_REPLIES: usize = 32;

/// One request, folded from its records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// Its id.
    pub id: String,
    /// When it opened, Unix milliseconds.
    pub opened_ms: u64,
    /// Its `open`.
    pub open: Open,
    /// Its given why (`why_src=given`).
    pub why: Option<String>,
    /// Its worker's brief.
    pub brief: Option<Brief>,
    /// Its tabs, in the order they opened.
    pub tabs: Vec<Tab>,
    /// How and why it concluded.
    pub end: Option<(Out, EndWhy)>,
    /// When it concluded, Unix milliseconds.
    pub ended_ms: Option<u64>,
    /// The folders its spawns opened in, at most [`MAX_ROOTS`]: each look lists them for
    /// folders born while its commands ran.
    pub roots: Vec<String>,
    /// The folders it claims, at most [`MAX_CLAIMS`], in the order claimed.
    pub claims: Vec<HeldClaim>,
    /// Its claimed folders a person worked in after they were claimed.
    pub touched: BTreeSet<String>,
    /// A person's review of it.
    pub review: Option<Review>,
    /// Who kept it (`request keep`) and when, Unix milliseconds: its tabs and folders are a
    /// person's, which no cleanup touches.
    pub kept: Option<(String, u64)>,
    /// The replies that went to its requester, oldest first: its newest [`MAX_REPLIES`].
    pub replies: Vec<Reply>,
}

impl Request {
    /// Where it stands.
    #[must_use]
    pub fn state(&self) -> State {
        match (self.end, self.tabs.is_empty()) {
            (Some((out, _)), _) => State::Concluded(out),
            (None, true) => State::Open,
            (None, false) => State::Running,
        }
    }

    /// Where it stands for its review ([`Stage`]).
    #[must_use]
    pub fn stage(&self) -> Stage {
        match self.state() {
            State::Open => Stage::Open,
            State::Running => Stage::Running,
            State::Concluded(_) if self.review.is_some() => Stage::Reviewed,
            State::Concluded(_) => Stage::Concluded,
        }
    }

    fn tab_mut(&mut self, sid: &str) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|tab| tab.sid == sid)
    }

    /// Its claim of folder `path`.
    #[must_use]
    pub fn claim(&self, path: &str) -> Option<&HeldClaim> {
        self.claims.iter().find(|held| held.claim.path == path)
    }

    /// What its tabs spent, summed over the tabs that left (the largest resident set, the
    /// largest of theirs): `None` for a figure no tab read.
    #[must_use]
    pub fn spent(&self) -> Spent {
        let add = |a: Option<u64>, b: Option<u64>| match (a, b) {
            (Some(a), Some(b)) => Some(a.saturating_add(b)),
            (a, b) => a.or(b),
        };
        self.tabs
            .iter()
            .filter_map(|tab| tab.spent)
            .fold(Spent::default(), |sum, spent| Spent {
                cpu_s: add(sum.cpu_s, spent.cpu_s),
                peak_mb: sum.peak_mb.max(spent.peak_mb),
                written_mb: add(sum.written_mb, spent.written_mb),
            })
    }
}

/// How a tab left, as the host knows it at the close.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Left {
    /// The exit reason's word (`shell-exit`, `ctl-close`, `ui-close`, `window-close`, `reaper`,
    /// `unknown`).
    pub reason: String,
    /// Its shell's exit code, where one was read.
    pub code: Option<i32>,
    /// Where its task stood, for a worker.
    pub task: Option<TaskEnd>,
    /// Who closed it, as an actor token.
    pub by: String,
    /// The node of who closed it: a drive's proven node, this Mac's for a closer here; `None`
    /// where it is not known.
    pub node: Option<String>,
    /// What its process tree spent, where it was read.
    pub spent: Option<Spent>,
}

impl Left {
    /// How the tab came out for a request whose requester is `node`'s `by`: a worker by its task
    /// (finished done, failed failed, still working abandoned); a plain tab released where its
    /// requester (that node and that token both) closed it, done where its shell exited 0,
    /// failed where its shell exited otherwise, and abandoned for any other end (a person's or a
    /// window's close, a quit, the reaper).
    #[must_use]
    pub fn out(&self, node: &str, by: &str) -> Out {
        match self.task {
            Some(TaskEnd::Finished) => Out::Done,
            Some(TaskEnd::Failed) => Out::Failed,
            Some(TaskEnd::Working) => Out::Abandoned,
            None => match (self.reason.as_str(), self.code) {
                ("ctl-close", _) if self.by == by && self.node.as_deref() == Some(node) => {
                    Out::Released
                }
                ("shell-exit", Some(0)) => Out::Done,
                ("shell-exit", Some(_)) => Out::Failed,
                _ => Out::Abandoned,
            },
        }
    }
}

/// Why a `req=` cannot join a request.
#[derive(Debug)]
pub enum Refusal {
    /// This Mac holds no request of that id.
    Unknown,
    /// Another requester's.
    NotYours,
    /// It concluded.
    Concluded,
    /// The store could not be read.
    Store(io::Error),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown => f.write_str("no request of that id on this Mac"),
            Self::NotYours => f.write_str("another requester's request"),
            Self::Concluded => f.write_str("the request has concluded"),
            Self::Store(error) => write!(f, "the request store: {error}"),
        }
    }
}

/// Why a person's act on a request (a review, a keep, a reply) was refused.
#[derive(Debug)]
pub enum ActRefusal {
    /// This Mac holds no request of that id.
    Unknown,
    /// It has not concluded: a review judges how it came out.
    Running,
    /// The line is not one a record keeps: a note or reply that is not one visible line
    /// ([`crate::why::line_of`]), or a key no `post` takes.
    Shape(&'static str),
    /// The store could not be read or written.
    Store(io::Error),
}

impl std::fmt::Display for ActRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown => f.write_str("no request of that id on this Mac"),
            Self::Running => {
                f.write_str("the request has not concluded, and a review judges how it came out")
            }
            Self::Shape(why) => f.write_str(why),
            Self::Store(error) => write!(f, "the request store: {error}"),
        }
    }
}

/// What a review wrote ([`Store::review`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reviewed {
    /// The request is reviewed now.
    New,
    /// It was reviewed before; this review's note replaced the note.
    Noted,
    /// It was reviewed before, and this review says nothing new: nothing was written.
    Already,
}

/// What a keep found ([`Store::keep_request`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeptRequest {
    /// Its tabs still open, which the host keeps as tabs (Keep as Tab).
    pub tabs: Vec<String>,
    /// It was kept before: nothing was written.
    pub already: bool,
}

/// What a request said in the content tier ([`Store::said`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SaidOf {
    /// The why a fallback took, and where from.
    pub why: Option<(String, WhySrc)>,
    /// The requesting tab's goal line at intake.
    pub goal: Option<String>,
    /// The requesting tab's last asked line at intake.
    pub asked: Option<String>,
    /// Each reply's text by its key, oldest first: the newest [`MAX_REPLIES`].
    pub replies: Vec<(String, String)>,
}

/// The content tier as last read: where the read stopped, and each request's words.
#[derive(Default)]
struct SaidFold {
    seen: Option<Identity>,
    of: HashMap<String, SaidOf>,
}

impl SaidFold {
    fn apply(&mut self, said: Said) {
        let of = self.of.entry(said.id).or_default();
        match said.body {
            SaidBody::Why { text, src } => of.why = Some((text, src)),
            SaidBody::Goal { text } => of.goal = Some(text),
            SaidBody::Asked { text } => of.asked = Some(text),
            SaidBody::Reply { text, key } => {
                if of.replies.len() >= MAX_REPLIES {
                    of.replies.remove(0);
                }
                of.replies.push((key, text));
            }
        }
    }
}

/// The note or reply `text` as a record keeps it: one visible line ([`crate::why::line_of`]),
/// refused rather than cut where it is more.
fn one_line(text: &str, what: &'static str) -> Result<String, ActRefusal> {
    let kept = crate::why::line_of(text).ok_or(ActRefusal::Shape(what))?;
    if kept != text.trim() {
        return Err(ActRefusal::Shape(what));
    }
    Ok(kept)
}

/// A folder a requester, or a request tab's caller, asks to claim ([`Store::claim`]).
#[derive(Clone, Copy, Debug)]
pub struct ClaimAsk<'a> {
    /// The request it names (`req=`), where it named one.
    pub id: Option<&'a str>,
    /// The folder, absolute.
    pub path: &'a str,
    /// The caller's node: a drive's proven node, this Mac's for a caller here.
    pub node: &'a str,
    /// The caller's actor token.
    pub by: &'a str,
    /// The tab the caller runs in, where it runs in one.
    pub tab: Option<&'a str>,
    /// When the folder was born, where the filesystem says.
    pub born_ms: Option<u64>,
    /// The command spans the requests' writer holds, each with its tab: whose they are is the
    /// store's to say, and [`crate::claim::alone`] judges the birth by them.
    pub ran: &'a [Ran<'a>],
}

/// A command span of tab `tab`, as a claim's birth is judged by ([`ClaimAsk::ran`]).
#[derive(Clone, Copy, Debug)]
pub struct Ran<'a> {
    /// The tab.
    pub tab: &'a str,
    /// When its command ran.
    pub span: crate::claim::Span,
    /// Whether its end was seen: a span whose tab left, or whose block the read passed, while it
    /// ran contests another's folder up to when that was seen, and never accounts for its own.
    pub whole: bool,
}

/// What a claim did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Claimed {
    /// Request `.0` claims the folder now.
    New(String),
    /// Request `.0` claimed it before.
    Already(String),
    /// It lies inside a folder request `id` claims, which covers it: nothing was written.
    Inside {
        /// The request.
        id: String,
        /// The claimed folder it lies in.
        claim: String,
    },
}

/// Why a claim was refused.
#[derive(Debug)]
pub enum ClaimRefusal {
    /// The caller named no request and runs in no tab of one.
    NoRequest,
    /// This Mac holds no request of that id.
    Unknown,
    /// Another requester's request.
    NotYours,
    /// The caller's tab is request `.0`'s, not the one it named.
    OtherTab(String),
    /// Request `.0` claims the folder, a folder holding it, or a folder in it.
    Taken(String),
    /// The request claims [`MAX_CLAIMS`] folders already.
    Full,
    /// The path cannot be recorded ([`crate::claim::normal`]).
    Path(&'static str),
    /// The store could not be read or written.
    Store(io::Error),
}

impl std::fmt::Display for ClaimRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoRequest => f.write_str(
                "this caller asks for no request (name req=<id> of one of yours, or run it in \
                 one of its tabs)",
            ),
            Self::Unknown => f.write_str("no request of that id on this Mac"),
            Self::NotYours => f.write_str("another requester's request"),
            Self::OtherTab(id) => write!(f, "this tab is {id}'s"),
            Self::Taken(id) => write!(f, "{id} claims it, a folder holding it, or a folder in it"),
            Self::Full => write!(f, "the request claims {MAX_CLAIMS} folders already"),
            Self::Path(why) => write!(f, "the path {why}"),
            Self::Store(error) => write!(f, "the request store: {error}"),
        }
    }
}

/// A folder a birth scan found ([`Store::born`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Born {
    /// The request whose command ran at its birth.
    pub id: String,
    /// The folder ([`crate::claim::normal`] form).
    pub path: String,
    /// `cwd` for a root of the request, else `born`.
    pub src: ClaimSrc,
    /// The tab whose command ran at its birth.
    pub tab: Option<String>,
    /// Its birth, Unix milliseconds.
    pub born_ms: u64,
    /// Whether its birth is its request's alone ([`crate::claim::alone`]).
    pub alone: Option<bool>,
}

/// A command a person ran in a claimed folder ([`Store::touched`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Touch {
    /// The request that claims the folder.
    pub id: String,
    /// The claimed folder.
    pub path: String,
    /// The tab the command ran in.
    pub tab: String,
    /// Its block.
    pub blk: u64,
    /// Why it counts as a person's.
    pub why: TouchWhy,
}

impl Open {
    /// Whether the requester is node `node`'s `by`.
    fn is_from(&self, node: &str, by: &str) -> bool {
        self.by == by && self.node == node
    }
}

/// How request `request` concludes once its tabs' outcomes are `outs` (`None` for a tab still
/// open, [`Tab::out`]), where no spawn joining it is pending: failed with no tab (its spawn was
/// refused); otherwise once no tab is open ([`outcome`]). `None` while a tab is open or it has
/// concluded.
fn conclusion(request: &Request, outs: &[Option<Out>]) -> Option<(Out, EndWhy)> {
    if request.end.is_some() {
        return None;
    }
    if outs.is_empty() {
        return Some((Out::Failed, EndWhy::SpawnRefused));
    }
    let outs: Vec<Out> = outs.iter().copied().collect::<Option<_>>()?;
    Some((outcome(&outs), EndWhy::TabsLeft))
}

/// A request's outcome over its tabs' `outs`: failed where any failed, done where all were done,
/// released where every one was done or released and one was released, abandoned otherwise.
fn outcome(outs: &[Out]) -> Out {
    if outs.contains(&Out::Failed) {
        Out::Failed
    } else if outs.iter().all(|out| *out == Out::Done) {
        Out::Done
    } else if outs
        .iter()
        .all(|out| matches!(out, Out::Done | Out::Released))
    {
        Out::Released
    } else {
        Out::Abandoned
    }
}

/// What one look at a host's leased plain shells found ([`Store::lease`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Look {
    /// How long a lease stands undriven and idle before it ends: [`LEASE_IDLE_MS`], which a test
    /// shortens.
    pub idle_after_ms: u64,
    /// Each request-owned tab this process hosts and looked at, by sid.
    pub tabs: BTreeMap<String, TabNow>,
    /// What a fresh fleet read reached; `None` where no fresh, whole read says (no read, a
    /// failed or stale one, this Mac in no fleet, or a read with a Mac it reached but cannot
    /// name or did not keep).
    pub answered: Option<Answered>,
}

/// The Macs a fresh, whole fleet read reached ([`Look::answered`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Answered {
    /// When the read was made, Unix milliseconds.
    pub read_at_ms: u64,
    /// The node ids of every Mac it reached, this one's own aside.
    pub nodes: BTreeSet<String>,
}

/// One leased tab as its host found it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TabNow {
    /// Nothing runs in it.
    pub idle: bool,
    /// When its shell's last command ended, Unix milliseconds, where its shell says.
    pub quiet_since_ms: Option<u64>,
}

/// A request a write concluded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ended {
    /// Its id.
    pub id: String,
    /// How.
    pub out: Out,
    /// Why.
    pub why: EndWhy,
    /// Every tab it held.
    pub tabs: Vec<String>,
}

/// Whether requester node `node` has left the fleet as `answered` says, its tabs quiet since
/// `quiet_ms`: a read made after that quiet reached Macs, every one of `node`'s form (a mesh
/// principal, `n-…`, or a legacy id), and not it, in either case. A read naming a node of the
/// other form says nothing of it, since one Mac may be known by either; nor does a read from
/// before the requester's last drive, no read, or no node.
fn requester_left(node: &str, answered: Option<&Answered>, quiet_ms: u64) -> bool {
    let Some(answered) = answered else {
        return false;
    };
    if node.is_empty() || answered.read_at_ms <= quiet_ms || answered.nodes.is_empty() {
        return false;
    }
    let mesh = |id: &str| {
        id.get(..2)
            .is_some_and(|head| head.eq_ignore_ascii_case("n-"))
    };
    answered
        .nodes
        .iter()
        .all(|other| mesh(other) == mesh(node) && !other.eq_ignore_ascii_case(node))
}

/// The folder of `claimed` that holds `path` (itself excluded) and the request that claims it.
fn covering(claimed: &HashMap<String, String>, path: &str) -> Option<(String, String)> {
    let claim = crate::claim::inside(path, claimed.keys().map(String::as_str))?;
    let owner = claimed.get(claim)?;
    Some((claim.to_owned(), owner.clone()))
}

/// The request that claims a folder of `claimed` in `path` (itself excluded), where one does.
fn holding(claimed: &HashMap<String, String>, path: &str) -> Option<String> {
    let claim = crate::claim::within(path, claimed.keys().map(String::as_str))?;
    claimed.get(claim).cloned()
}

/// The fold: every request read, and which request owns each tab.
#[derive(Default)]
struct Fold {
    requests: BTreeMap<String, Request>,
    tabs: HashMap<String, String>,
    /// Which request claims each claimed folder: the first claim of a folder wins, and a later
    /// one of it, of a folder in it or of a folder holding it applies to nothing, so every reader
    /// folds the same owner and no claim holds another.
    claimed: HashMap<String, String>,
    /// The bytes of the `attach` rows read that a later one of the same tab and verb
    /// supersedes, which a compaction drops (each counted at its successor's length).
    stale: u64,
}

impl Fold {
    /// Fold `rec`, a line of `bytes` with its newline, in; `false` where it applies to nothing
    /// (an `open` of an id already open, any other record of an id never opened, or of a tab the
    /// request lacks).
    fn apply(&mut self, rec: Rec, bytes: u64) -> bool {
        let Rec { t_ms, id, body } = rec;
        if let Body::Open(open) = body {
            if self.requests.contains_key(&id) {
                return false;
            }
            self.requests.insert(
                id.clone(),
                Request {
                    id,
                    opened_ms: t_ms,
                    open,
                    why: None,
                    brief: None,
                    tabs: Vec::new(),
                    end: None,
                    ended_ms: None,
                    roots: Vec::new(),
                    claims: Vec::new(),
                    touched: BTreeSet::new(),
                    review: None,
                    kept: None,
                    replies: Vec::new(),
                },
            );
            return true;
        }
        let Some(request) = self.requests.get_mut(&id) else {
            return false;
        };
        match body {
            Body::Open(_) => return false,
            Body::Why { text } => request.why = Some(text),
            Body::Brief(brief) => request.brief = Some(brief),
            Body::Tab { tab, .. } => {
                if request.tab_mut(&tab).is_none() {
                    request.tabs.push(Tab {
                        sid: tab.clone(),
                        opened_ms: t_ms,
                        verbs: BTreeMap::new(),
                        left: None,
                        last_ms: t_ms,
                        kept: false,
                        kept_ms: None,
                        spent: None,
                        gone: None,
                    });
                }
                self.tabs.insert(tab, id);
            }
            Body::Attach { tab, verb, n, .. } => {
                let Some(tab) = request.tab_mut(&tab) else {
                    return false;
                };
                let count = tab.verbs.entry(verb).or_default();
                if *count > 0 {
                    self.stale += bytes;
                }
                *count = (*count).max(n);
                if verb != Verb::Close {
                    tab.last_ms = tab.last_ms.max(t_ms);
                }
            }
            Body::Left {
                tab,
                reason,
                code,
                task,
                out,
            } => {
                let Some(tab) = request.tab_mut(&tab) else {
                    return false;
                };
                tab.left = Some(out);
                tab.gone = Some(Gone {
                    at_ms: t_ms,
                    reason,
                    code,
                    task,
                });
            }
            Body::Keep { tab, .. } => {
                let Some(tab) = request.tab_mut(&tab) else {
                    return false;
                };
                tab.kept = true;
                tab.kept_ms = Some(t_ms);
            }
            Body::End { out, why } => {
                request.end = Some((out, why));
                request.ended_ms = Some(t_ms);
            }
            Body::Root { path, .. } => {
                if request.roots.contains(&path) || request.roots.len() >= MAX_ROOTS {
                    return false;
                }
                request.roots.push(path);
            }
            Body::Claim(claim) => {
                if request.claims.len() >= MAX_CLAIMS
                    || self.claimed.contains_key(&claim.path)
                    || covering(&self.claimed, &claim.path).is_some()
                    || holding(&self.claimed, &claim.path).is_some()
                {
                    return false;
                }
                self.claimed.insert(claim.path.clone(), id);
                request.claims.push(HeldClaim { claim, at_ms: t_ms });
            }
            Body::Touched { path, .. } => {
                if request.claim(&path).is_none() {
                    return false;
                }
                request.touched.insert(path);
            }
            Body::Spent { tab, spent } => {
                let Some(tab) = request.tab_mut(&tab) else {
                    return false;
                };
                tab.spent = Some(spent);
            }
            Body::Review { note, by } => match &mut request.review {
                Some(review) => {
                    if note.is_some() {
                        review.note = note;
                    }
                }
                None => {
                    request.review = Some(Review {
                        by,
                        at_ms: t_ms,
                        note,
                    });
                }
            },
            Body::Kept { by } => {
                if request.kept.is_some() {
                    return false;
                }
                request.kept = Some((by, t_ms));
            }
            Body::Reply { key, sent, by } => {
                if request.replies.len() >= MAX_REPLIES {
                    request.replies.remove(0);
                }
                request.replies.push(Reply {
                    by,
                    at_ms: t_ms,
                    key,
                    sent,
                });
            }
        }
        true
    }
}

/// Coverage of an inspection's retained journal view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InspectionStatus {
    /// Rows that could not be parsed or applied.
    pub skipped_rows: u64,
    /// No bounded read omitted bytes from this fold's history. A partial fold remains
    /// conservatively incomplete until it is reloaded without a read gap.
    pub complete: bool,
}

/// The request store of one folder ([`crate::DIR`] in the state home).
pub struct Store {
    journal: PathBuf,
    said: PathBuf,
    /// The journal as last read: its file, and the length read up to (the end of its last whole
    /// line).
    seen: Option<Identity>,
    fold: Fold,
    skipped: u64,
    /// A bounded read omitted part of this fold's history.
    read_gap: bool,
    /// [`JOURNAL_COMPACT`]; a test lowers it.
    compact_at: u64,
    /// The `req=` joins this process checked whose tab has not landed or been refused yet, by
    /// request: a request with one is not concluded when its last tab leaves, so a join that
    /// passed always finds its request open. Tabs live in one process, which writes their
    /// leaving, so this need not be shared.
    joining: HashMap<String, u32>,
    /// The content tier as last read ([`Self::said`]), read only by a reader that serves it.
    said_fold: SaidFold,
}

/// The store of folder `dir`, created private where missing, its journal read.
///
/// # Errors
/// The folder could not be made, or the journal not read.
pub fn open(dir: &Path) -> io::Result<Store> {
    Store::open(dir)
}

impl Store {
    /// [`open`].
    ///
    /// # Errors
    /// As [`open`].
    pub fn open(dir: &Path) -> io::Result<Self> {
        make_private_dir(dir)?;
        let mut store = Self {
            journal: dir.join(crate::JOURNAL),
            said: dir.join(crate::SAID),
            seen: None,
            fold: Fold::default(),
            skipped: 0,
            read_gap: false,
            compact_at: JOURNAL_COMPACT,
            joining: HashMap::new(),
            said_fold: SaidFold::default(),
        };
        store.refresh()?;
        Ok(store)
    }

    /// The lines read that were no record, or applied to none: torn, foreign, over-long or
    /// out-of-shape rows, which are skipped and never fatal.
    #[must_use]
    pub const fn skipped(&self) -> u64 {
        self.skipped
    }

    /// The request `id`, as this store last read it.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&Request> {
        self.fold.requests.get(id)
    }

    /// What request `id` said in the content tier ([`crate::SAID`]): its fallback why, the
    /// requesting tab's goal and asked lines, and its replies' text, after any row another
    /// process appended. The content tier is read only here, so a reader that serves no content
    /// never opens it. A read that fails answers from what this store last read.
    pub fn said(&mut self, id: &str) -> Option<SaidOf> {
        let _ = self.refresh_said();
        self.said_fold.of.get(id).cloned()
    }

    /// A person (`by`, an actor token) reviewed request `id` at `at_ms`, with `note` where they
    /// gave one (one visible line with no double quote and not `-`, which the record writes for
    /// none; refused where it is more). The first review records who and when; a later one with
    /// a new note replaces the note, and one with none is [`Reviewed::Already`] and writes
    /// nothing.
    ///
    /// # Errors
    /// [`ActRefusal::Unknown`]; [`ActRefusal::Running`] for a request not concluded;
    /// [`ActRefusal::Shape`] for a note that is not one line, holds a double quote or is `-`;
    /// the journal's.
    pub fn review(
        &mut self,
        at_ms: u64,
        id: &str,
        by: &str,
        note: Option<&str>,
    ) -> Result<Reviewed, ActRefusal> {
        const NOTE: &str = "a note is one visible line of at most 200 characters with no double \
                            quote, and not `-`";
        let note = note.map(|note| one_line(note, NOTE)).transpose()?;
        // The record writes no note as `-`: a note of `-` would read back as none.
        if note.as_deref() == Some("-") {
            return Err(ActRefusal::Shape(NOTE));
        }
        let mut held = line_log::hold(&self.journal).map_err(ActRefusal::Store)?;
        self.refresh_held(&mut held).map_err(ActRefusal::Store)?;
        let request = self.fold.requests.get(id).ok_or(ActRefusal::Unknown)?;
        if request.end.is_none() {
            return Err(ActRefusal::Running);
        }
        let reviewed = match (&request.review, &note) {
            (None, _) => Reviewed::New,
            (Some(review), Some(note)) if review.note.as_ref() != Some(note) => Reviewed::Noted,
            (Some(_), _) => return Ok(Reviewed::Already),
        };
        self.write(
            &mut held,
            vec![Rec {
                t_ms: at_ms,
                id: id.to_owned(),
                body: Body::Review {
                    note,
                    by: token_or_dash(by),
                },
            }],
        )
        .map_err(ActRefusal::Store)?;
        Ok(reviewed)
    }

    /// A person (`by`) kept request `id` at `at_ms`: its `kept`, once, whatever its state; its
    /// tabs and folders are theirs, and no cleanup touches them. Returns its tabs still open,
    /// which the host keeps as tabs (each writing its own `keep`, [`Self::keep`]).
    ///
    /// # Errors
    /// [`ActRefusal::Unknown`]; the journal's.
    pub fn keep_request(
        &mut self,
        at_ms: u64,
        id: &str,
        by: &str,
    ) -> Result<KeptRequest, ActRefusal> {
        let mut held = line_log::hold(&self.journal).map_err(ActRefusal::Store)?;
        self.refresh_held(&mut held).map_err(ActRefusal::Store)?;
        let request = self.fold.requests.get(id).ok_or(ActRefusal::Unknown)?;
        let tabs: Vec<String> = request
            .tabs
            .iter()
            .filter(|tab| tab.is_open())
            .map(|tab| tab.sid.clone())
            .collect();
        if request.kept.is_some() {
            return Ok(KeptRequest {
                tabs,
                already: true,
            });
        }
        self.write(
            &mut held,
            vec![Rec {
                t_ms: at_ms,
                id: id.to_owned(),
                body: Body::Kept {
                    by: token_or_dash(by),
                },
            }],
        )
        .map_err(ActRefusal::Store)?;
        Ok(KeptRequest {
            tabs,
            already: false,
        })
    }

    /// A reply `text` (one visible line) went, or did not (`sent`), from `by` to request `id`'s
    /// requester under mail key `key`, at `at_ms`: its text in the content tier first, then its
    /// journal row, as [`Self::open_request`] orders them.
    ///
    /// # Errors
    /// [`ActRefusal::Unknown`]; [`ActRefusal::Shape`] for text that is not one line or a key no
    /// `post` takes; either log's.
    pub fn reply(
        &mut self,
        at_ms: u64,
        id: &str,
        by: &str,
        key: &str,
        sent: bool,
        text: &str,
    ) -> Result<(), ActRefusal> {
        let text = one_line(
            text,
            "a reply is one visible line of at most 200 characters with no double quote",
        )?;
        if !crate::record::is_mail_key(key) {
            return Err(ActRefusal::Shape("the reply's key is no post key"));
        }
        let mut held = line_log::hold(&self.journal).map_err(ActRefusal::Store)?;
        self.refresh_held(&mut held).map_err(ActRefusal::Store)?;
        if !self.fold.requests.contains_key(id) {
            return Err(ActRefusal::Unknown);
        }
        let said = Said {
            t_ms: at_ms,
            id: id.to_owned(),
            body: SaidBody::Reply {
                text,
                key: key.to_owned(),
            },
        }
        .line();
        let mut held_said = line_log::hold(&self.said).map_err(ActRefusal::Store)?;
        held_said.append(&[&said]).map_err(ActRefusal::Store)?;
        aterm_durable::ordered(held_said.file()).map_err(ActRefusal::Store)?;
        drop(held_said);
        self.write(
            &mut held,
            vec![Rec {
                t_ms: at_ms,
                id: id.to_owned(),
                body: Body::Reply {
                    key: key.to_owned(),
                    sent,
                    by: token_or_dash(by),
                },
            }],
        )
        .map_err(ActRefusal::Store)
    }

    /// Every request, including those with no tab, after any record another process appended,
    /// with the read's coverage: how many rows were skipped, and whether the fold holds the whole
    /// journal. It never opens the content tier ([`Self::said`]).
    ///
    /// # Errors
    /// The journal could not be read: unlike [`Self::requests`], a reader that answers a person
    /// refuses rather than answer from what this store last read.
    pub fn inspect(&mut self) -> io::Result<(impl Iterator<Item = &Request>, InspectionStatus)> {
        self.refresh()?;
        Ok((
            self.fold.requests.values(),
            InspectionStatus {
                skipped_rows: self.skipped,
                complete: !self.read_gap,
            },
        ))
    }

    /// Open a request for `intake` at `at_ms`, minting its id under the journal's lock: the
    /// content tier goes to [`crate::SAID`] first, then the journal's `open` (and its `why` and
    /// `brief`), so a request is in the journal only with what it said beside it.
    ///
    /// # Errors
    /// The minute's id counter is exhausted, or either log's lock, read, write or flush fails.
    pub fn open_request(&mut self, at_ms: u64, intake: Intake) -> io::Result<String> {
        let mut held = line_log::hold(&self.journal)?;
        self.refresh_held(&mut held)?;
        let head = crate::id::head_of(&intake.from.node);
        let prefix = crate::id::prefix(at_ms, &head);
        let n = self
            .fold
            .requests
            .range(prefix.clone()..)
            .take_while(|(id, _)| id.starts_with(&prefix))
            .filter_map(|(id, _)| crate::id::count_of(id, &prefix))
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| io::Error::other("request id counter exhausted for this minute"))?;
        let id = crate::id::nth(&prefix, n);
        let rec = |body| Rec {
            t_ms: at_ms,
            id: id.clone(),
            body,
        };
        let said_rec = |body| Said {
            t_ms: at_ms,
            id: id.clone(),
            body,
        };
        let Intake {
            via,
            mut form,
            from,
            why,
            brief,
            goal,
            asked,
        } = intake;
        if matches!(&form, Form::Agent(agent) if !crate::record::is_agent_word(agent)) {
            form = Form::Program;
        }
        let mut said = Vec::new();
        let mut recs = vec![rec(Body::Open(Open {
            via,
            form,
            node: if is_node_word(&from.node) {
                from.node
            } else {
                String::new()
            },
            host: clamp(&from.host, HOST_BYTES),
            by: token_or_dash(&from.by),
            program: from.program.filter(|word| is_requester_word(word)),
            model: from.model.filter(|word| is_requester_word(word)),
            account: from
                .account
                .map(|account| clamp(&account, REQUESTER_ACCOUNT_BYTES))
                .filter(|account| !account.is_empty() && account != "-"),
            why_src: why.src,
        }))];
        let why_text = clamp(&why.text, REQUEST_WHY_BYTES);
        match why.src {
            WhySrc::Given => recs.push(rec(Body::Why { text: why_text })),
            WhySrc::Brief | WhySrc::Goal | WhySrc::Asked => said.push(said_rec(SaidBody::Why {
                text: why_text,
                src: why.src,
            })),
            WhySrc::None => {}
        }
        if let Some(brief) = brief {
            recs.push(rec(Body::Brief(brief)));
        }
        let line = |text: Option<String>| {
            text.as_deref()
                .and_then(crate::why::line_of)
                .map(|text| clamp(&text, REQUEST_WHY_BYTES))
        };
        if let Some(text) = line(goal) {
            said.push(said_rec(SaidBody::Goal { text }));
        }
        if let Some(text) = line(asked) {
            said.push(said_rec(SaidBody::Asked { text }));
        }
        if !said.is_empty() {
            let lines: Vec<String> = said.iter().map(Said::line).collect();
            let mut held_said = line_log::hold(&self.said)?;
            held_said.append(&lines.iter().map(String::as_str).collect::<Vec<_>>())?;
            aterm_durable::ordered(held_said.file())?;
        }
        self.write(&mut held, recs)?;
        Ok(id)
    }

    /// Whether a spawn by node `node`'s `by` with `req=<id>` joins request `id`: it must be open
    /// on this Mac, that requester's own, and not concluded. Nothing is written until its tab
    /// lands ([`Self::tab`]) or its spawn is refused ([`Self::refused`]), each with `joined`;
    /// until then the request does not conclude.
    ///
    /// # Errors
    /// Why it cannot.
    pub fn join(&mut self, id: &str, node: &str, by: &str) -> Result<(), Refusal> {
        self.refresh().map_err(Refusal::Store)?;
        let request = self.fold.requests.get(id).ok_or(Refusal::Unknown)?;
        if !request.open.is_from(node, by) {
            return Err(Refusal::NotYours);
        }
        if request.end.is_some() {
            return Err(Refusal::Concluded);
        }
        *self.joining.entry(id.to_owned()).or_default() += 1;
        Ok(())
    }

    /// A joined spawn of request `id` settled: one fewer pending.
    fn settled(&mut self, id: &str) {
        if let Some(n) = self.joining.get_mut(id) {
            *n -= 1;
            if *n == 0 {
                self.joining.remove(id);
            }
        }
    }

    /// Record tab `tab`, which `by`'s spawn made, as request `id`'s (`joined` where the spawn
    /// named it with `req=`, [`Self::join`]): the first moves it to running. `root` is the folder
    /// the spawn opened in, as the host judged it ([`crate::claim::normal`] form): in the same append, a `root`
    /// where the request has not recorded it and holds fewer than [`MAX_ROOTS`].
    ///
    /// # Errors
    /// The journal's error; or `InvalidInput` where `id` is unknown or concluded, or `tab` is no
    /// session aterm minted.
    pub fn tab(
        &mut self,
        at_ms: u64,
        id: &str,
        tab: &str,
        by: &str,
        joined: bool,
        root: Option<&str>,
    ) -> io::Result<()> {
        if joined {
            self.settled(id);
        }
        let mut held = line_log::hold(&self.journal)?;
        self.refresh_held(&mut held)?;
        let live = self
            .fold
            .requests
            .get(id)
            .is_some_and(|request| request.end.is_none());
        if !live || !aterm_types::control_verbs::is_minted_sid(tab) {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        let rec = |body| Rec {
            t_ms: at_ms,
            id: id.to_owned(),
            body,
        };
        let mut recs = vec![rec(Body::Tab {
            tab: tab.to_owned(),
            by: token_or_dash(by),
        })];
        let new_root = root
            .filter(|root| crate::claim::normal(root).is_ok_and(|normal| normal == *root))
            .filter(|root| {
                self.fold.requests.get(id).is_some_and(|request| {
                    !request.roots.iter().any(|had| had == root) && request.roots.len() < MAX_ROOTS
                })
            });
        if let Some(root) = new_root {
            recs.push(rec(Body::Root {
                path: root.to_owned(),
                tab: tab.to_owned(),
            }));
        }
        self.write(&mut held, recs)
    }

    /// Claim the folder `ask` names for a request: the request its caller's tab is (`ask.tab`,
    /// open), or the one it names (`ask.id`), which must then be the caller's own (that node and
    /// that token both). A folder another request claims, or one inside such a folder, is that
    /// request's; one inside a folder this request claims is covered already and writes
    /// nothing. Any request state may claim: a concluded request's folders are still its own.
    ///
    /// # Errors
    /// Why it was refused ([`ClaimRefusal`]).
    pub fn claim(&mut self, at_ms: u64, ask: ClaimAsk<'_>) -> Result<Claimed, ClaimRefusal> {
        let path = crate::claim::normal(ask.path).map_err(ClaimRefusal::Path)?;
        let mut held = line_log::hold(&self.journal).map_err(ClaimRefusal::Store)?;
        self.refresh_held(&mut held).map_err(ClaimRefusal::Store)?;
        let of_tab = ask.tab.and_then(|tab| {
            let id = self.fold.tabs.get(tab)?;
            let request = self.fold.requests.get(id)?;
            request
                .tabs
                .iter()
                .any(|t| t.sid == tab && t.is_open())
                .then(|| id.clone())
        });
        let id = match (ask.id, of_tab) {
            (Some(named), Some(own)) if named != own => return Err(ClaimRefusal::OtherTab(own)),
            (_, Some(own)) => own,
            (Some(named), None) => {
                let request = self.fold.requests.get(named).ok_or(ClaimRefusal::Unknown)?;
                if !request.open.is_from(ask.node, ask.by) {
                    return Err(ClaimRefusal::NotYours);
                }
                named.to_owned()
            }
            (None, None) => return Err(ClaimRefusal::NoRequest),
        };
        if let Some(owner) = self.fold.claimed.get(&path) {
            return if *owner == id {
                Ok(Claimed::Already(id))
            } else {
                Err(ClaimRefusal::Taken(owner.clone()))
            };
        }
        if let Some((claim, owner)) = self.covering(&path) {
            return if owner == id {
                Ok(Claimed::Inside { id, claim })
            } else {
                Err(ClaimRefusal::Taken(owner))
            };
        }
        if let Some(owner) = self.holding(&path) {
            return Err(ClaimRefusal::Taken(owner));
        }
        if self
            .fold
            .requests
            .get(&id)
            .is_none_or(|request| request.claims.len() >= MAX_CLAIMS)
        {
            return Err(ClaimRefusal::Full);
        }
        let tab = ask
            .tab
            .filter(|tab| aterm_types::control_verbs::is_minted_sid(tab))
            .map(str::to_owned);
        let alone = ask.born_ms.and_then(|born| {
            let (mut mine, mut others) = (Vec::new(), Vec::new());
            for ran in ask.ran {
                if self.fold.tabs.get(ran.tab) == Some(&id) {
                    if ran.whole {
                        mine.push(ran.span);
                    }
                } else {
                    others.push(ran.span);
                }
            }
            crate::claim::alone(born, &mine, &others)
        });
        self.write(
            &mut held,
            vec![Rec {
                t_ms: at_ms,
                id: id.clone(),
                body: Body::Claim(Claim {
                    path,
                    src: ClaimSrc::Named,
                    by: token_or_dash(ask.by),
                    tab,
                    born_ms: ask.born_ms,
                    alone,
                }),
            }],
        )
        .map_err(ClaimRefusal::Store)?;
        Ok(Claimed::New(id))
    }

    /// The claimed folder that holds `path` (itself excluded) and the request that claims it.
    fn covering(&self, path: &str) -> Option<(String, String)> {
        covering(&self.fold.claimed, path)
    }

    /// The request that claims a folder in `path` (itself excluded), where one does.
    fn holding(&self, path: &str) -> Option<String> {
        holding(&self.fold.claimed, path)
    }

    /// Record the folders a birth scan found ([`Born`]) as their requests' claims, `by` each
    /// request's requester: a folder already claimed, inside a claimed folder or holding one, is
    /// skipped, as is one past its request's [`MAX_CLAIMS`]. Returns each folder claimed and its request.
    ///
    /// # Errors
    /// The journal's: then nothing was claimed.
    pub fn born(&mut self, at_ms: u64, found: &[Born]) -> io::Result<Vec<(String, String)>> {
        if found.is_empty() {
            return Ok(Vec::new());
        }
        let mut held = line_log::hold(&self.journal)?;
        self.refresh_held(&mut held)?;
        let mut taken: Vec<&str> = Vec::new();
        let mut counts: HashMap<&str, usize> = HashMap::new();
        let mut recs = Vec::new();
        let mut claimed = Vec::new();
        for born in found {
            let Some(request) = self.fold.requests.get(&born.id) else {
                continue;
            };
            let count = counts
                .entry(born.id.as_str())
                .or_insert(request.claims.len());
            let recordable = crate::claim::normal(&born.path).is_ok_and(|p| p == born.path);
            if !recordable
                || *count >= MAX_CLAIMS
                || self.fold.claimed.contains_key(&born.path)
                || self.covering(&born.path).is_some()
                || self.holding(&born.path).is_some()
                || crate::claim::inside(&born.path, taken.iter().copied()).is_some()
                || crate::claim::within(&born.path, taken.iter().copied()).is_some()
            {
                continue;
            }
            *count += 1;
            taken.push(&born.path);
            recs.push(Rec {
                t_ms: at_ms,
                id: born.id.clone(),
                body: Body::Claim(Claim {
                    path: born.path.clone(),
                    src: born.src,
                    by: request.open.by.clone(),
                    tab: born
                        .tab
                        .clone()
                        .filter(|tab| aterm_types::control_verbs::is_minted_sid(tab)),
                    born_ms: Some(born.born_ms),
                    alone: born.alone,
                }),
            });
            claimed.push((born.path.clone(), born.id.clone()));
        }
        if !recs.is_empty() {
            self.write(&mut held, recs)?;
        }
        Ok(claimed)
    }

    /// Record the commands a person ran in claimed folders ([`Touch`]): one `touched` per
    /// request and folder, a folder the request does not claim skipped. Returns how many were
    /// written.
    ///
    /// # Errors
    /// The journal's: then none was.
    pub fn touched(&mut self, at_ms: u64, hits: &[Touch]) -> io::Result<usize> {
        if hits.is_empty() {
            return Ok(0);
        }
        let mut held = line_log::hold(&self.journal)?;
        self.refresh_held(&mut held)?;
        let mut seen: BTreeSet<(&str, &str)> = BTreeSet::new();
        let mut recs = Vec::new();
        for hit in hits {
            let Some(request) = self.fold.requests.get(&hit.id) else {
                continue;
            };
            if request.claim(&hit.path).is_none()
                || request.touched.contains(&hit.path)
                || !aterm_types::control_verbs::is_minted_sid(&hit.tab)
                || !seen.insert((hit.id.as_str(), hit.path.as_str()))
            {
                continue;
            }
            recs.push(Rec {
                t_ms: at_ms,
                id: hit.id.clone(),
                body: Body::Touched {
                    path: hit.path.clone(),
                    tab: hit.tab.clone(),
                    blk: hit.blk,
                    why: hit.why,
                },
            });
        }
        let n = recs.len();
        if n > 0 {
            self.write(&mut held, recs)?;
        }
        Ok(n)
    }

    /// Every claimed folder and the request that claims it, after any record another process
    /// appended. A read that fails answers from what this store last read.
    pub fn claims(&mut self) -> impl Iterator<Item = (&str, &str)> {
        let _ = self.refresh();
        self.fold
            .claimed
            .iter()
            .map(|(path, id)| (path.as_str(), id.as_str()))
    }

    /// The spawn of request `id` (`joined` as for [`Self::tab`]) was refused. A request left with
    /// no tab and no join pending concludes failed (`why=spawn-refused`); a joined request whose
    /// tabs all left while this join was pending concludes by them (`why=tabs-left`); any other
    /// is left as it is.
    ///
    /// # Errors
    /// The journal's.
    pub fn refused(&mut self, at_ms: u64, id: &str, joined: bool) -> io::Result<()> {
        if joined {
            self.settled(id);
        }
        let mut held = line_log::hold(&self.journal)?;
        self.refresh_held(&mut held)?;
        if self.joining.contains_key(id) {
            return Ok(());
        }
        let Some((out, why)) = self.fold.requests.get(id).and_then(|request| {
            let outs: Vec<Option<Out>> = request.tabs.iter().map(Tab::out).collect();
            conclusion(request, &outs)
        }) else {
            return Ok(());
        };
        self.write(
            &mut held,
            vec![Rec {
                t_ms: at_ms,
                id: id.to_owned(),
                body: Body::End { out, why },
            }],
        )
    }

    /// The request that owns tab `tab` and its requester's node and actor token, read after any
    /// record another process appended. A read that fails answers from what this store last
    /// read.
    pub fn owner(&mut self, tab: &str) -> Option<(String, String, String)> {
        let _ = self.refresh();
        let id = self.fold.tabs.get(tab)?;
        let request = self.fold.requests.get(id)?;
        Some((
            id.clone(),
            request.open.node.clone(),
            request.open.by.clone(),
        ))
    }

    /// Record `verb` from node `node`'s `by` on tab `tab`, where a request owns the tab and that
    /// is the request's requester, and the tab has not left (a `close` is recorded after its tab
    /// left, which the close itself did): `true` where it was recorded. Anyone else's verb
    /// writes nothing.
    ///
    /// # Errors
    /// The journal's.
    pub fn attach(
        &mut self,
        at_ms: u64,
        tab: &str,
        verb: Verb,
        node: &str,
        by: &str,
    ) -> io::Result<bool> {
        let mut held = line_log::hold(&self.journal)?;
        self.refresh_held(&mut held)?;
        let Some(id) = self.fold.tabs.get(tab).cloned() else {
            return Ok(false);
        };
        let Some(request) = self.fold.requests.get(&id) else {
            return Ok(false);
        };
        let Some(open_tab) = request.tabs.iter().find(|t| t.sid == tab) else {
            return Ok(false);
        };
        // A close is recorded even where its tab's leaving was written first: the main thread
        // retires the tab before the close's reply reaches the connection that asked. A kept tab
        // is a person's, which takes nothing more.
        let over = request.end.is_some() || open_tab.left.is_some();
        if !request.open.is_from(node, by) || (over && verb != Verb::Close) || open_tab.kept {
            return Ok(false);
        }
        let n = open_tab.verbs.get(&verb).copied().unwrap_or(0) + 1;
        self.write(
            &mut held,
            vec![Rec {
                t_ms: at_ms,
                id,
                body: Body::Attach {
                    tab: tab.to_owned(),
                    verb,
                    n,
                    by: by.to_owned(),
                },
            }],
        )?;
        Ok(true)
    }

    /// Tab `tab` left as `left` says: its `left` record, and where it was its request's last open
    /// tab and no `req=` join of it is pending ([`Self::join`]), the request's `end`
    /// ([`outcome`]), which this returns. Nothing for a tab no request owns, one a person kept,
    /// or one of a concluded request (a lease's end leaves its tab standing).
    ///
    /// # Errors
    /// The journal's.
    pub fn left(&mut self, at_ms: u64, tab: &str, left: &Left) -> io::Result<Option<Out>> {
        let mut held = line_log::hold(&self.journal)?;
        self.refresh_held(&mut held)?;
        let Some(id) = self.fold.tabs.get(tab).cloned() else {
            return Ok(None);
        };
        let Some(request) = self.fold.requests.get(&id) else {
            return Ok(None);
        };
        let rec = |body| Rec {
            t_ms: at_ms,
            id: id.clone(),
            body,
        };
        let this = request.tabs.iter().find(|t| t.sid == tab);
        // Its spend is the request's whatever the request's state: a lease's end leaves the
        // tab standing, and its later leaving still says what it spent.
        let mut recs: Vec<Rec> = left
            .spent
            .filter(|spent| !spent.is_unread() && this.is_some_and(|t| t.spent.is_none()))
            .map(|spent| {
                rec(Body::Spent {
                    tab: tab.to_owned(),
                    spent,
                })
            })
            .into_iter()
            .collect();
        if request.end.is_some() || this.is_none_or(|t| !t.is_open()) {
            if !recs.is_empty() {
                self.write(&mut held, recs)?;
            }
            return Ok(None);
        }
        let out = left.out(&request.open.node, &request.open.by);
        recs.push(rec(Body::Left {
            tab: tab.to_owned(),
            reason: left.reason.clone(),
            code: left.code,
            task: left.task,
            out,
        }));
        let outs: Vec<Option<Out>> = request
            .tabs
            .iter()
            .map(|t| if t.sid == tab { Some(out) } else { t.out() })
            .collect();
        let ended = if self.joining.contains_key(&id) {
            None
        } else {
            conclusion(request, &outs).map(|(out, _)| out)
        };
        if let Some(out) = ended {
            recs.push(rec(Body::End {
                out,
                why: EndWhy::TabsLeft,
            }));
        }
        self.write(&mut held, recs)?;
        Ok(ended)
    }

    /// A lease's look at `at_ms` ([`Look`]): each open request of a fleet drive's plain shell
    /// (`via=fleet-shell`, no `req=` join pending) whose every open tab this look found idle
    /// concludes abandoned, where its requester's Mac left the fleet ([`requester_left`],
    /// `why=requester-left`) or its quiet (the latest of each open tab's last drive,
    /// [`Tab::last_ms`], and its shell's last command's end) is `look.idle_after_ms` old
    /// (`why=idle`). Only its `end` is written: its tabs stay, and their later leaving writes
    /// nothing. A request with an open tab the look did not find (hosted elsewhere, or gone) is
    /// the relaunch sweep's ([`Self::sweep`]). Returns what it concluded.
    ///
    /// # Errors
    /// The journal's: then nothing concluded.
    pub fn lease(&mut self, at_ms: u64, look: &Look) -> io::Result<Vec<Ended>> {
        if look.tabs.is_empty() {
            return Ok(Vec::new());
        }
        let mut held = line_log::hold(&self.journal)?;
        self.refresh_held(&mut held)?;
        let mut ended = Vec::new();
        for request in self.fold.requests.values() {
            if request.open.via != Via::FleetShell
                || request.end.is_some()
                || self.joining.contains_key(&request.id)
            {
                continue;
            }
            let open: Vec<&Tab> = request.tabs.iter().filter(|tab| tab.is_open()).collect();
            let quiet = open.iter().try_fold(0_u64, |quiet, tab| {
                let now = look.tabs.get(&tab.sid).filter(|now| now.idle)?;
                Some(quiet.max(tab.last_ms).max(now.quiet_since_ms.unwrap_or(0)))
            });
            let Some(quiet) = quiet.filter(|_| !open.is_empty()) else {
                continue;
            };
            let why = if requester_left(&request.open.node, look.answered.as_ref(), quiet) {
                EndWhy::RequesterLeft
            } else if at_ms >= quiet.saturating_add(look.idle_after_ms) {
                EndWhy::Idle
            } else {
                continue;
            };
            ended.push(Ended {
                id: request.id.clone(),
                out: Out::Abandoned,
                why,
                tabs: request.tabs.iter().map(|tab| tab.sid.clone()).collect(),
            });
        }
        if !ended.is_empty() {
            let recs = ended
                .iter()
                .map(|end| Rec {
                    t_ms: at_ms,
                    id: end.id.clone(),
                    body: Body::End {
                        out: end.out,
                        why: end.why,
                    },
                })
                .collect();
            self.write(&mut held, recs)?;
        }
        Ok(ended)
    }

    /// A person kept tab `tab` (Keep as Tab), `by` the actor who did: where a request owns it
    /// and it is open (it has not left, and was not kept before), its `keep`, whatever the
    /// request's state, and where it was the request's last open tab and no `req=` join of it is
    /// pending, the request's `end` (`why=kept`, a kept tab counting released), which this
    /// returns. Nothing for any other tab.
    ///
    /// # Errors
    /// The journal's.
    pub fn keep(&mut self, at_ms: u64, tab: &str, by: &str) -> io::Result<Option<Ended>> {
        let mut held = line_log::hold(&self.journal)?;
        self.refresh_held(&mut held)?;
        let Some(id) = self.fold.tabs.get(tab).cloned() else {
            return Ok(None);
        };
        let Some(request) = self.fold.requests.get(&id) else {
            return Ok(None);
        };
        if !request.tabs.iter().any(|t| t.sid == tab && t.is_open()) {
            return Ok(None);
        }
        let rec = |body| Rec {
            t_ms: at_ms,
            id: id.clone(),
            body,
        };
        let mut recs = vec![rec(Body::Keep {
            tab: tab.to_owned(),
            by: token_or_dash(by),
        })];
        let outs: Vec<Option<Out>> = request
            .tabs
            .iter()
            .map(|t| {
                if t.sid == tab {
                    Some(Out::Released)
                } else {
                    t.out()
                }
            })
            .collect();
        let ended = if self.joining.contains_key(&id) {
            None
        } else {
            conclusion(request, &outs).map(|(out, _)| Ended {
                id: id.clone(),
                out,
                why: EndWhy::Kept,
                tabs: request.tabs.iter().map(|t| t.sid.clone()).collect(),
            })
        };
        if let Some(end) = &ended {
            recs.push(rec(Body::End {
                out: end.out,
                why: end.why,
            }));
        }
        self.write(&mut held, recs)?;
        Ok(ended)
    }

    /// The sweep a new process makes once its restore settled, at `at_ms`, where it was born at
    /// `born_ms` and hosts the tabs `hosted` and no other process on this state home hosts any:
    /// each open request opened before `born_ms` with no `req=` join pending writes `left`
    /// (`reason=relaunch`, abandoned) for each open tab it does not host, and concludes
    /// (`why=relaunch`) once none is open; one that never got a tab concludes abandoned. A
    /// request opened since `born_ms` is this process's own, its spawn perhaps still in flight,
    /// and is never touched. Returns what it concluded.
    ///
    /// # Errors
    /// The journal's: then nothing was written.
    pub fn sweep(
        &mut self,
        at_ms: u64,
        born_ms: u64,
        hosted: &BTreeSet<String>,
    ) -> io::Result<Vec<Ended>> {
        let mut held = line_log::hold(&self.journal)?;
        self.refresh_held(&mut held)?;
        let mut recs = Vec::new();
        let mut ended = Vec::new();
        for request in self.fold.requests.values() {
            if request.end.is_some()
                || request.opened_ms >= born_ms
                || self.joining.contains_key(&request.id)
            {
                continue;
            }
            let rec = |body| Rec {
                t_ms: at_ms,
                id: request.id.clone(),
                body,
            };
            let mut outs = Vec::new();
            for tab in &request.tabs {
                if tab.is_open() && !hosted.contains(&tab.sid) {
                    recs.push(rec(Body::Left {
                        tab: tab.sid.clone(),
                        reason: "relaunch".to_owned(),
                        code: None,
                        task: None,
                        out: Out::Abandoned,
                    }));
                    outs.push(Some(Out::Abandoned));
                } else {
                    outs.push(tab.out());
                }
            }
            let out = if outs.is_empty() {
                Some(Out::Abandoned)
            } else {
                outs.into_iter()
                    .collect::<Option<Vec<Out>>>()
                    .map(|outs| outcome(&outs))
            };
            if let Some(out) = out {
                recs.push(rec(Body::End {
                    out,
                    why: EndWhy::Relaunch,
                }));
                ended.push(Ended {
                    id: request.id.clone(),
                    out,
                    why: EndWhy::Relaunch,
                    tabs: request.tabs.iter().map(|tab| tab.sid.clone()).collect(),
                });
            }
        }
        if !recs.is_empty() {
            self.write(&mut held, recs)?;
        }
        Ok(ended)
    }

    /// Every request, after any record another process appended. A read that fails answers from
    /// what this store last read.
    pub fn requests(&mut self) -> impl Iterator<Item = &Request> {
        let _ = self.refresh();
        self.fold.requests.values()
    }

    /// Every request with a tab, after any record another process appended, each with its tabs:
    /// what a host's index of which tab is whose is built from. A read that fails answers from
    /// what this store last read.
    pub fn placements(&mut self) -> impl Iterator<Item = &Request> {
        let _ = self.refresh();
        self.fold
            .requests
            .values()
            .filter(|request| !request.tabs.is_empty())
    }

    /// Append `recs` under `held` (already refreshed), finish them with an ordered flush, fold
    /// them in, and compact where the journal passed [`Self::compact_at`] and at least half of it
    /// is superseded `attach` rows ([`JOURNAL_COMPACT`]).
    fn write(&mut self, held: &mut Held, recs: Vec<Rec>) -> io::Result<()> {
        let lines: Vec<String> = recs.iter().map(Rec::line).collect();
        held.append(&lines.iter().map(String::as_str).collect::<Vec<_>>())?;
        aterm_durable::ordered(held.file())?;
        for (rec, line) in recs.into_iter().zip(&lines) {
            self.fold.apply(rec, line.len() as u64 + 1);
        }
        let now = held.identity()?;
        self.seen = Some(now);
        if now.len > self.compact_at && self.fold.stale.saturating_mul(2) >= now.len {
            self.compact(held, now.len)?;
        }
        Ok(())
    }

    /// Rewrite the journal of `len` bytes (under `held`, refreshed), all of it, keeping every
    /// record but each tab's earlier `attach`es of a verb: the last carries the count. Another
    /// store finds the file replaced and reads it again whole.
    fn compact(&mut self, held: &mut Held, len: u64) -> io::Result<()> {
        let bytes = held.read_from(0, len)?;
        let text = String::from_utf8_lossy(&bytes);
        let recs: Vec<Rec> = text.lines().filter_map(Rec::parse).collect();
        let mut last: HashMap<(&str, &str, Verb), usize> = HashMap::new();
        for (at, rec) in recs.iter().enumerate() {
            if let Body::Attach { tab, verb, .. } = &rec.body {
                last.insert((rec.id.as_str(), tab.as_str(), *verb), at);
            }
        }
        let mut out = String::new();
        for (at, rec) in recs.iter().enumerate() {
            let keep = match &rec.body {
                Body::Attach { tab, verb, .. } => {
                    last.get(&(rec.id.as_str(), tab.as_str(), *verb)) == Some(&at)
                }
                _ => true,
            };
            if keep {
                out.push_str(&rec.line());
                out.push('\n');
            }
        }
        aterm_uds::atomic_file::write_flushed(
            &self.journal,
            out.as_bytes(),
            0o600,
            Some(aterm_durable::Flush::Ordered),
        )?;
        self.seen = Some(line_log::identity_of(&std::fs::symlink_metadata(
            &self.journal,
        )?));
        self.fold.stale = 0;
        Ok(())
    }

    /// Read what was appended since this store last read, without the lock: only whole lines
    /// are taken, so a row being written is read next time.
    fn refresh(&mut self) -> io::Result<()> {
        let meta = match std::fs::symlink_metadata(&self.journal) {
            Ok(meta) => meta,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if self.seen.is_some() {
                    self.reset();
                }
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        let now = line_log::identity_of(&meta);
        if self.seen == Some(now) {
            return Ok(());
        }
        let (file, meta) = aterm_uds::regular_file::open(&self.journal)?;
        let now = line_log::identity_of(&meta);
        self.take(now, read_of(&file))
    }

    fn refresh_held(&mut self, held: &mut Held) -> io::Result<()> {
        let mut now = held.identity()?;
        if self.seen == Some(now) {
            return Ok(());
        }
        // Append completes an interrupted final delimiter. Fold that completed row before
        // deciding what to append, so allocation and ownership match a cold reader's view.
        if now.len > 0 && held.read_from(now.len - 1, 1)? != b"\n" {
            held.append(&[])?;
            aterm_durable::ordered(held.file())?;
            now = held.identity()?;
        }
        self.take(now, |offset, max| held.read_from(offset, max))
    }

    fn reset(&mut self) {
        self.fold = Fold::default();
        self.skipped = 0;
        self.read_gap = false;
        self.seen = None;
    }

    /// Fold in the whole lines of the journal `now` that this store has not read: from where it
    /// stopped, or from the start (its newest [`READ_MAX`] bytes) where the file is another or
    /// shrank.
    fn take(
        &mut self,
        now: Identity,
        read: impl FnMut(u64, u64) -> io::Result<Vec<u8>>,
    ) -> io::Result<()> {
        let tail = whole_lines(self.seen, now, read)?;
        if tail.restart {
            self.reset();
        }
        self.read_gap |= tail.gap;
        for line in tail.lines() {
            let applied = std::str::from_utf8(line)
                .ok()
                .and_then(Rec::parse)
                .is_some_and(|rec| self.fold.apply(rec, line.len() as u64 + 1));
            if !applied {
                self.skipped += 1;
            }
        }
        self.seen = Some(tail.seen);
        Ok(())
    }

    /// Read what the content tier gained since this store last read it, without the lock, as
    /// [`Self::refresh`] reads the journal.
    fn refresh_said(&mut self) -> io::Result<()> {
        let meta = match std::fs::symlink_metadata(&self.said) {
            Ok(meta) => meta,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.said_fold = SaidFold::default();
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        if self.said_fold.seen == Some(line_log::identity_of(&meta)) {
            return Ok(());
        }
        let (file, meta) = aterm_uds::regular_file::open(&self.said)?;
        let now = line_log::identity_of(&meta);
        let tail = whole_lines(self.said_fold.seen, now, read_of(&file))?;
        if tail.restart {
            self.said_fold.of.clear();
        }
        for line in tail.lines() {
            if let Some(said) = std::str::from_utf8(line).ok().and_then(Said::parse) {
                self.said_fold.apply(said);
            }
        }
        self.said_fold.seen = Some(tail.seen);
        Ok(())
    }
}

/// What a read of a log's whole lines took ([`whole_lines`]).
struct Tail {
    /// The read starts over: the file is another, or shrank, or was never read.
    restart: bool,
    /// The read left the log's start out: it started over inside a file longer than
    /// [`READ_MAX`]. (A read cut at [`READ_MAX`] past where it started leaves nothing out: the
    /// next read takes up at [`Self::seen`].)
    gap: bool,
    bytes: Vec<u8>,
    /// The whole lines lie at `bytes[skip..whole]`.
    skip: usize,
    whole: usize,
    /// Where the next read starts.
    seen: Identity,
}

impl Tail {
    /// Each whole line, not empty, without its newline.
    fn lines(&self) -> impl Iterator<Item = &[u8]> {
        self.bytes[self.skip..self.whole]
            .split(|b| *b == b'\n')
            .filter(|line| !line.is_empty())
    }
}

/// The whole lines of log `now` that a reader stopped at `seen` has not read: from where it
/// stopped, or from the start (the newest [`READ_MAX`] bytes) where the file is another or
/// shrank; a read that starts inside the file drops the line it starts in, and a row being
/// written is read next time.
fn whole_lines(
    seen: Option<Identity>,
    now: Identity,
    mut read: impl FnMut(u64, u64) -> io::Result<Vec<u8>>,
) -> io::Result<Tail> {
    let (from, restart) = match seen {
        Some(seen) if seen.dev == now.dev && seen.ino == now.ino && now.len >= seen.len => {
            (seen.len, false)
        }
        _ => (0, true),
    };
    let start = if from == 0 {
        now.len.saturating_sub(READ_MAX)
    } else {
        from
    };
    let bytes = read(start, now.len.saturating_sub(start).min(READ_MAX))?;
    let gap = start > from;
    let skip = if start > from {
        bytes
            .iter()
            .position(|b| *b == b'\n')
            .map_or(bytes.len(), |at| at + 1)
    } else {
        0
    };
    let whole = bytes[skip..]
        .iter()
        .rposition(|b| *b == b'\n')
        .map_or(skip, |at| skip + at + 1);
    Ok(Tail {
        restart,
        gap,
        bytes,
        skip,
        whole,
        seen: Identity {
            len: start + whole as u64,
            ..now
        },
    })
}

/// A reader of `file` from an offset, at most a length.
fn read_of(file: &std::fs::File) -> impl FnMut(u64, u64) -> io::Result<Vec<u8>> + '_ {
    move |offset, max| {
        use std::io::{Read as _, Seek as _};
        let mut file = file;
        file.seek(io::SeekFrom::Start(offset))?;
        let mut out = Vec::new();
        file.take(max).read_to_end(&mut out)?;
        Ok(out)
    }
}

/// `value` cut to `cap` encoded bytes at a character boundary.
fn clamp(value: &str, cap: usize) -> String {
    aterm_slog::field::clamp_encoded(value, cap).to_owned()
}

/// Whether `word` is a requester's program or model in its shape
/// ([`aterm_types::control_verbs::requester_token`]).
fn is_requester_word(word: &str) -> bool {
    aterm_types::control_verbs::requester_token(word).is_some_and(|token| token == word)
}

/// `by` where it is a token a record keeps, else `-`.
fn token_or_dash(by: &str) -> String {
    if is_token(by) {
        by.to_owned()
    } else {
        "-".to_owned()
    }
}

#[cfg(unix)]
fn make_private_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    if std::fs::symlink_metadata(dir).is_ok_and(|meta| meta.is_dir()) {
        return Ok(());
    }
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    match dir.parent() {
        Some(parent) => aterm_durable::ordered_dir(parent),
        None => Ok(()),
    }
}

#[cfg(not(unix))]
fn make_private_dir(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
