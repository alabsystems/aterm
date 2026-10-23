// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fold (design §7.1): one pure reduction of a session's records to its current state, the same in the server, in a supervisor and over the bus.

use crate::field::{Key, Val};
use crate::rec::Rec;

#[path = "fold_mail.rs"]
pub mod mail;
#[path = "fold_snap.rs"]
mod snap;
use crate::vocab::{Actor, CLS_WORDS, Conf, Kind, Lvl, Node, Op, Src};
use crate::words::{BoxWord, WallKind, need_rank};
pub use mail::{
    MAIL_LINK_WINDOW_MS, MAX_MAIL_WAITING, MailLink, MailLinkOf, MailWaiting, mail_in_window,
    mail_link_of,
};
pub use snap::SnapError;

/// The most needs the state holds; a less urgent one gives way.
pub const MAX_NEEDS: usize = 8;
/// The most open nodes the fold tracks.
const MAX_TRACKED: usize = 64;
/// A label's default lifetime (§7.4): 600 s.
pub const LABEL_TTL_MS: u64 = 600_000;
/// A label's longest lifetime: 3600 s.
pub const LABEL_TTL_MAX_MS: u64 = 3_600_000;
/// The clause's byte budget (§7.3).
pub const CLAUSE_BYTES: usize = 48;

/// One field of a node as the fold holds it: the value, and the confidence and seq that put it there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Held {
    /// The key.
    pub key: Key,
    /// The value.
    pub val: Val,
    /// The confidence of the record that set it.
    pub conf: Conf,
    /// The seq of that record.
    pub seq: u64,
}

/// A node's fields under the confidence law.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NodeFields(Vec<Held>);

impl NodeFields {
    /// Offer `val` for `key` at `conf` from record `seq`: kept when no value is held, or the held one ranks lower, or ranks the same from an earlier seq ([`confidence_law`]).
    pub fn offer(&mut self, key: Key, val: Val, conf: Conf, seq: u64) {
        if let Some(h) = self.0.iter_mut().find(|h| h.key == key) {
            if confidence_law(h.conf, h.seq, conf, seq) {
                *h = Held {
                    key,
                    val,
                    conf,
                    seq,
                };
            }
            return;
        }
        if self.0.len() < crate::field::MAX_FIELDS * 2 {
            self.0.push(Held {
                key,
                val,
                conf,
                seq,
            });
        }
    }

    /// Hold `val` for `key` at `conf` from record `seq` whatever is held:
    fn put(&mut self, key: Key, val: Val, conf: Conf, seq: u64) {
        let held = Held {
            key,
            val,
            conf,
            seq,
        };
        match self.0.iter_mut().find(|h| h.key == key) {
            Some(h) => *h = held,
            None => self.0.push(held),
        }
    }

    /// Forget `key`.
    pub fn remove(&mut self, key: Key) {
        self.0.retain(|h| h.key != key);
    }

    /// The held fields, in the order they were first offered.
    pub fn iter(&self) -> impl Iterator<Item = &Held> {
        self.0.iter()
    }

    /// The held value of `key`.
    #[must_use]
    pub fn get(&self, key: Key) -> Option<&Val> {
        self.0.iter().find(|h| h.key == key).map(|h| &h.val)
    }

    /// The held entry of `key`.
    #[must_use]
    pub fn held(&self, key: Key) -> Option<&Held> {
        self.0.iter().find(|h| h.key == key)
    }

    /// The held word of `key`.
    #[must_use]
    pub fn word(&self, key: Key) -> Option<&'static str> {
        match self.get(key) {
            Some(Val::Word(w)) => Some(w),
            _ => None,
        }
    }

    /// The held text, token or id of `key`.
    #[must_use]
    pub fn str(&self, key: Key) -> Option<&str> {
        match self.get(key) {
            Some(Val::Tok(t)) => Some(t.as_str()),
            Some(Val::Text(t)) => Some(t.as_str()),
            Some(Val::Id(id)) => Some(id.as_str()),
            Some(Val::Word(w)) => Some(w),
            _ => None,
        }
    }

    /// The held unsigned number of `key`.
    #[must_use]
    pub fn u(&self, key: Key) -> Option<u64> {
        match self.get(key) {
            Some(Val::U(n)) => Some(*n),
            _ => None,
        }
    }

    /// The held signed number of `key`.
    #[must_use]
    pub fn i(&self, key: Key) -> Option<i64> {
        match self.get(key) {
            Some(Val::I(n)) => Some(*n),
            _ => None,
        }
    }
}

/// THE confidence law (§3.7): does an offered value (`conf`, `seq`) replace the held one (`held_conf`, `held_seq`)? Yes when it ranks higher, or ranks the same and is later.
#[must_use]
pub const fn confidence_law(held_conf: Conf, held_seq: u64, conf: Conf, seq: u64) -> bool {
    conf.rank() > held_conf.rank() || (conf.rank() == held_conf.rank() && seq > held_seq)
}

/// An open node in the state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenNode {
    /// The node.
    pub node: Node,
    /// Its kind.
    pub kind: Kind,
    /// Its parent.
    pub par: Option<Node>,
    /// When it opened (wall ms).
    pub t: u64,
    /// Who opened it.
    pub by: Actor,
    /// Where observed.
    pub src: Src,
    /// How sure.
    pub conf: Conf,
    /// Its fields under the confidence law.
    pub f: NodeFields,
}

/// A closed node in the state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClosedNode {
    /// The node.
    pub node: Node,
    /// Its kind.
    pub kind: Kind,
    /// The `out=` word.
    pub out: &'static str,
    /// Its duration.
    pub dur_ms: u64,
    /// When it opened (wall ms), when known.
    pub opened_t: Option<u64>,
    /// When it closed (wall ms).
    pub t: u64,
    /// Who closed it.
    pub by: Actor,
    /// Who opened it, when known.
    pub opened_by: Actor,
    /// Where the close was observed.
    pub src: Src,
    /// How sure.
    pub conf: Conf,
    /// Its open and close fields under the confidence law.
    pub f: NodeFields,
}

/// Something the session needs from someone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Need {
    /// The need word (one of [`crate::vocab::NEED_WORDS`]).
    pub word: &'static str,
    /// The node that raised it (a step, a run, the session, a mark's node).
    pub node: Node,
    /// The kind of the record that raised it: its node's (`cmd` for a shell command's failure, `approval` for a box), or `need` for a mark's.
    pub kind: Kind,
    /// The seq that raised it.
    pub since_seq: u64,
    /// When (wall ms).
    pub t: u64,
    /// Where observed.
    pub src: Src,
    /// How sure.
    pub conf: Conf,
    /// Who raised it.
    pub by: Actor,
    /// The wall's or failure's detail: the reset (`3pm`) or the exit code.
    pub detail: Option<String>,
}

impl Need {
    /// Whether it was OBSERVED, not claimed: raised by a record of its own (a box, a wall, a run's failure), never by a `need` mark, which any author can write.
    #[must_use]
    pub fn observed(&self) -> bool {
        self.src != Src::Mark && self.conf != Conf::Claim
    }
}

/// A consumer's claim (`verdict`, `label`): attributed, never an observation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claim {
    /// The claim's word (`on-track`), for a verdict.
    pub word: Option<&'static str>,
    /// The claim's text, for a label.
    pub text: Option<String>,
    /// Its author.
    pub by: Actor,
    /// When (wall ms).
    pub t: u64,
    /// When it expires (wall ms), for a label.
    pub until: Option<u64>,
    /// The seq that set it.
    pub seq: u64,
    /// The name its author declared for itself (`as=`): a claim about a claim, printed as `as=`, never as `by=`.
    pub as_name: Option<String>,
    /// The record or mail it is about (`re=<seq|@off>`).
    pub re: Option<crate::vocab::ShortId>,
}

impl Claim {
    /// The claim a `verdict` or `label` mark makes: its word or text and
    fn of(r: &Rec, label: bool) -> Option<Claim> {
        let (word, text, until) = if label {
            let text = r.f.get_str(Key::Text)?.to_string();
            let ttl =
                r.f.get_u(Key::Ttl)
                    .map_or(LABEL_TTL_MS, |s| s.saturating_mul(1000))
                    .min(LABEL_TTL_MAX_MS);
            (None, Some(text), Some(r.t.saturating_add(ttl)))
        } else {
            (Some(r.f.get_word(Key::Word)?), None, None)
        };
        let re = match r.f.get(Key::Re) {
            Some(Val::Id(id)) => Some(*id),
            _ => None,
        };
        Some(Claim {
            word,
            text,
            by: r.by,
            t: r.t,
            until,
            seq: r.seq,
            as_name: r.f.get_str(Key::As).map(str::to_string),
            re,
        })
    }
}

/// What the session is doing, by its newest verdict: the agent word, else the shell phase.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Phase {
    /// The word (`busy`, `idle`, `running`, `-`).
    pub word: &'static str,
    /// Where observed.
    pub src: Src,
    /// How sure.
    pub conf: Conf,
    /// When (wall ms).
    pub t: u64,
}

impl Default for Phase {
    fn default() -> Phase {
        Phase {
            word: "-",
            src: Src::Unknown,
            conf: Conf::Unknown,
            t: 0,
        }
    }
}

/// Counters over the whole session, evicted records included.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    /// Turns opened.
    pub turns: u32,
    /// Turns closed `done`.
    pub done: u32,
    /// Turns closed `asked`.
    pub asked: u32,
    /// Turns closed on a wall.
    pub walled: u32,
    /// Turns closed `interrupted`.
    pub interrupted: u32,
    /// Turns closed `error`.
    pub errors: u32,
    /// Turns closed `gone`, `superseded`, `retracted` or `unknown`.
    pub other: u32,
    /// Steps opened.
    pub steps: u32,
    /// Approval steps.
    pub appr: u32,
    /// Approvals closed, by who closed them: human, harness, ctl, other.
    pub appr_by: [u32; 4],
    /// Question steps.
    pub qs: u32,
    /// Input stalls.
    pub stalls: u32,
    /// Tool steps by class ([`CLS_WORDS`] order).
    pub tools: [u32; 10],
    /// Mail delivered.
    pub mail_in: u32,
    /// Mail posted.
    pub mail_out: u32,
    /// Human input bursts.
    pub human: u32,
    /// Records reported lost by gaps.
    pub lost: u64,
    /// Turns that found two or more mails waiting and linked none ([`mail::MailLinkOf::Ambiguous`], phase 5 ruling T10c).
    pub mail_amb: u32,
}

/// A session's current state (§7.1).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionState {
    /// The last seq applied.
    pub as_of: u64,
    /// The epoch.
    pub ep: u32,
    /// Bumps on every applied record: the chrome's cache key.
    pub rev: u64,
    /// The open path: the deepest open run, the open turn, the deepest open step.
    pub open: [Option<OpenNode>; 3],
    /// The newest verdict.
    pub phase: Phase,
    /// Open needs, most urgent first.
    pub needs: [Option<Need>; MAX_NEEDS],
    /// The newest closed run, turn and step.
    pub last: [Option<ClosedNode>; 3],
    /// Counters.
    pub totals: Totals,
    /// Mail delivered and not yet marked seen.
    pub mail_open: u16,
    /// The newest verdict claim, of any author.
    pub verdict: Option<Claim>,
    /// The newest label claim, of any author.
    pub label: Option<Claim>,
    /// The newest verdict claim of a trusted author ([`Actor::is_trusted`]: the harness or the owner), kept beside [`Self::verdict`] so no other author's newer verdict displaces it.
    pub verdict_trusted: Option<Claim>,
    /// The newest label claim of a trusted author: the label `brief` and the clause show when no proven author's stands (§7.3, D6).
    pub label_trusted: Option<Claim>,
    /// The newest verdict claim of a PROVEN author ([`Actor::is_proven`]: the harness or the person), kept beside the trusted one so no `ctl` claim — which any process in a session can make, the agent the harness judges among them — displaces the harness's verdict.
    pub verdict_proven: Option<Claim>,
    /// The newest label claim of a proven author: the label `brief` and the clause show first.
    pub label_proven: Option<Claim>,
    /// The session's own close, once it closed.
    pub closed: Option<ClosedNode>,
    /// The newest record's time (wall ms).
    pub last_t: u64,
}

impl SessionState {
    /// The most urgent need.
    #[must_use]
    pub fn top_need(&self) -> Option<&Need> {
        self.needs.iter().flatten().next()
    }

    /// The most urgent OBSERVED need ([`Need::observed`]): the one `brief`'s session line names.
    #[must_use]
    pub fn top_observed_need(&self) -> Option<&Need> {
        self.needs.iter().flatten().find(|n| n.observed())
    }

    /// The open run.
    #[must_use]
    pub const fn run(&self) -> Option<&OpenNode> {
        self.open[0].as_ref()
    }

    /// The open turn.
    #[must_use]
    pub const fn turn(&self) -> Option<&OpenNode> {
        self.open[1].as_ref()
    }

    /// The deepest open step.
    #[must_use]
    pub const fn step(&self) -> Option<&OpenNode> {
        self.open[2].as_ref()
    }
}

/// How the clause is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// Someone is needed.
    Attention,
    /// Work is under way.
    Working,
    /// Something failed.
    Failed,
    /// At rest.
    Idle,
}

/// The clause's words: at most [`CLAUSE_BYTES`] bytes, held inline.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Words {
    buf: [u8; CLAUSE_BYTES],
    len: u8,
}

impl Words {
    /// `subject · suffix`, the subject cut (never mid-character, never before a combining mark or joiner) until the whole fits; `subject` alone when `suffix` is empty.
    #[must_use]
    pub fn fit(subject: &str, suffix: &str) -> Words {
        let owned;
        let s: &str = if suffix.is_empty() {
            cut_subject(subject, CLAUSE_BYTES)
        } else {
            let tail = format!(" · {suffix}");
            let head = cut_subject(subject, CLAUSE_BYTES.saturating_sub(tail.len()));
            if head.is_empty() || tail.len() >= CLAUSE_BYTES {
                cut_subject(subject, CLAUSE_BYTES)
            } else {
                owned = format!("{head}{tail}");
                &owned
            }
        };
        let mut buf = [0u8; CLAUSE_BYTES];
        buf[..s.len()].copy_from_slice(s.as_bytes());
        Words {
            buf,
            len: s.len() as u8,
        }
    }

    /// The words.
    #[must_use]
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.buf[..usize::from(self.len)]).unwrap_or("")
    }
}

impl std::fmt::Debug for Words {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Words({:?})", self.as_str())
    }
}

/// Whether `c` extends the character before it (a combining mark, a joiner,
const fn is_extend(c: char) -> bool {
    matches!(c,
        '\u{0300}'..='\u{036f}'
        | '\u{1ab0}'..='\u{1aff}'
        | '\u{1dc0}'..='\u{1dff}'
        | '\u{200c}'..='\u{200d}'
        | '\u{20d0}'..='\u{20ff}'
        | '\u{fe00}'..='\u{fe0f}'
        | '\u{fe20}'..='\u{fe2f}'
        | '\u{1f3fb}'..='\u{1f3ff}'
        | '\u{e0100}'..='\u{e01ef}')
}

/// The longest prefix of `s` within `room` bytes cut between clusters.
fn cut_subject(s: &str, room: usize) -> &str {
    if s.len() <= room {
        return s;
    }
    let mut end = 0;
    let mut chars = s.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        let after = at + c.len_utf8();
        if after > room {
            break;
        }
        if !chars.peek().is_some_and(|&(_, n)| is_extend(n)) {
            end = after;
        }
    }
    &s[..end]
}

/// Which row of the clause policy (§7.3) a clause came from: what the tooltip attributes (a label is said to be one; an observation never is).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ClauseRow {
    /// The most urgent need.
    Need,
    /// A live label of a trusted (or, for [`clause_of_proven`], a proven) author.
    Label,
    /// The open tool or subagent step.
    Step,
    /// The open turn.
    Turn,
    /// The open command, or the agent at rest.
    Run,
}

/// The tab's "what is it doing" clause (§7.3): drawn as is, never parsed back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Clause {
    /// The words, at most 48 bytes.
    pub words: Words,
    /// How long the state has held.
    pub age_ms: u64,
    /// How it is drawn.
    pub tone: Tone,
    /// Whose state it is (a label's author, a need's raiser).
    pub by: Actor,
    /// The row it came from (phase 3B).
    pub row: ClauseRow,
    /// The words without the coarse age [`Clause::words`] ends with on a label, step, turn or command row (the same words on a need's): what a clock-free cache may key on, since they move only when the state does (phase 3B).
    pub head: Words,
}

/// What the fold can say without claiming a need.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Hints {
    /// How long since the newest record.
    pub quiet_for_ms: u64,
}

/// A coarse age for the clause: `40s`, `4m`, `3h`.
#[must_use]
pub fn age_short(ms: u64) -> String {
    let s = ms / 1000;
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else {
        format!("{}h", s / 3600)
    }
}

/// [`Fold::bus_facts`]: what a bus fold seeded from a snapshot holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BusFacts {
    /// The epoch.
    pub ep: u32,
    /// The bus class's open path: run, turn, need-bearing step.
    pub open: [Option<(Node, Kind)>; 3],
    /// The open needs, most urgent first: word, node, raiser, source.
    pub needs: Vec<(&'static str, Node, Actor, Src)>,
}

/// [`Fold::bus_totals`]: the counters a bus fold holding a session from its first record agrees on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BusTotals {
    /// Turns opened.
    pub turns: u32,
    /// Turns closed `done`.
    pub done: u32,
    /// Turns closed `asked`.
    pub asked: u32,
    /// Turns closed on a wall.
    pub walled: u32,
    /// Turns closed `interrupted`.
    pub interrupted: u32,
    /// Turns closed `error`.
    pub errors: u32,
    /// Turns closed otherwise.
    pub other: u32,
    /// Approval steps.
    pub appr: u32,
    /// Approvals closed by human, harness, ctl, other.
    pub appr_by: [u32; 4],
    /// Question steps.
    pub qs: u32,
    /// Input stalls.
    pub stalls: u32,
    /// The session's close, once it closed.
    pub closed: Option<&'static str>,
    /// The newest closed run and its outcome.
    pub last_run: Option<(Node, &'static str)>,
    /// The newest closed turn and its outcome.
    pub last_turn: Option<(Node, &'static str)>,
}

/// The fold.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fold {
    st: SessionState,
    tracked: Vec<OpenNode>,
    /// The shell phase, kept apart from the agent word.
    shell_phase: Option<Phase>,
    /// The delivered `kind=task` mails no turn has taken, oldest first
    mail: Vec<MailWaiting>,
    /// THE MAIL LATCH (phase 5's T39):
    in_since_human: bool,
    /// The heuristic mail links of the open turns and the newest closed
    links: Vec<(Node, MailLink)>,
}

impl Fold {
    /// An empty fold of epoch `ep`.
    #[must_use]
    pub fn new(ep: u32) -> Fold {
        let mut f = Fold::default();
        f.st.ep = ep;
        f
    }

    /// The state.
    #[must_use]
    pub const fn state(&self) -> &SessionState {
        &self.st
    }

    /// The fold's estimated heap: its state, tracked nodes, field slots and owned content in fields, needs and claims.
    #[must_use]
    pub fn footprint(&self) -> usize {
        let fields = |f: &NodeFields| {
            f.0.iter()
                .map(|h| std::mem::size_of::<Held>() + h.val.heap_len())
                .sum::<usize>()
        };
        std::mem::size_of::<Fold>()
            + self
                .tracked
                .iter()
                .map(|o| std::mem::size_of::<OpenNode>() + fields(&o.f))
                .sum::<usize>()
            + self
                .st
                .open
                .iter()
                .flatten()
                .map(|o| fields(&o.f))
                .sum::<usize>()
            + self
                .st
                .last
                .iter()
                .flatten()
                .chain(self.st.closed.iter())
                .map(|c| fields(&c.f))
                .sum::<usize>()
            + self
                .st
                .needs
                .iter()
                .flatten()
                .filter_map(|n| n.detail.as_ref())
                .map(String::len)
                .sum::<usize>()
            + self
                .st
                .verdict
                .iter()
                .chain(self.st.label.iter())
                .chain(self.st.verdict_trusted.iter())
                .chain(self.st.label_trusted.iter())
                .chain(self.st.verdict_proven.iter())
                .chain(self.st.label_proven.iter())
                .map(|c| {
                    c.text.as_ref().map_or(0, String::len)
                        + c.as_name.as_ref().map_or(0, String::len)
                })
                .sum::<usize>()
    }

    /// Apply one record.
    pub fn apply(&mut self, r: &Rec) {
        let reset = starts_epoch(r, self.st.ep);
        if r.seq <= self.st.as_of && !reset {
            return;
        }
        self.st.as_of = r.seq;
        self.st.rev = self.st.rev.saturating_add(1);
        self.st.last_t = self.st.last_t.max(r.t);
        match r.op {
            Op::Open => self.on_open(r),
            Op::Close => self.on_close(r),
            Op::Mark => self.on_mark(r),
            Op::Amend => self.on_amend(r),
            Op::Gap => self.on_gap(r, reset),
            Op::Snap => {}
        }
    }

    /// THE HEURISTIC MAIL LINK of turn `node` (phase 5, [`mail`]): the delivered mail it most likely acted on, always [`Conf::Heuristic`]; held for the open turns and the newest closed one.
    #[must_use]
    pub fn mail_link(&self, node: Node) -> Option<MailLink> {
        self.links.iter().find(|(n, _)| *n == node).map(|(_, l)| *l)
    }

    /// Every heuristic mail link the fold holds ([`Fold::mail_link`]), for the local `brief` to print (6C-3, U5).
    #[must_use]
    pub fn mail_links(&self) -> &[(Node, MailLink)] {
        &self.links
    }

    /// The delivered `kind=task` mails no turn has taken, oldest first.
    #[must_use]
    pub fn mail_waiting(&self) -> &[MailWaiting] {
        &self.mail
    }

    /// THE MAIL LATCH ([`Fold`]'s `in_since_human`): mail was delivered (`mail dir=in`) since the last `human` mark of an input that reached the program.
    #[must_use]
    pub const fn in_since_human(&self) -> bool {
        self.in_since_human
    }

    /// A turn opened: link it to the one mail waiting in its window, or
    fn link_turn(&mut self, r: &Rec) {
        let decided = mail_link_of(&self.mail, r);
        match decided {
            MailLinkOf::Link(off) => self.links.push((
                r.node,
                MailLink {
                    off,
                    conf: Conf::Heuristic,
                },
            )),
            MailLinkOf::Ambiguous => {
                self.st.totals.mail_amb = self.st.totals.mail_amb.saturating_add(1);
            }
            MailLinkOf::None => {}
        }
        mail::after_turn(&mut self.mail, r, decided);
    }

    /// Keep the links of the open turns and the newest closed one.
    fn prune_links(&mut self) {
        let last = self.st.last[1].as_ref().map(|c| c.node);
        let tracked = &self.tracked;
        self.links
            .retain(|(n, _)| Some(*n) == last || tracked.iter().any(|o| o.node == *n));
    }

    /// The tab clause at `now_wall` (§7.3): the first matching row wins.
    #[must_use]
    pub fn clause(&self, now_wall: u64) -> Option<Clause> {
        clause_of(&self.st, now_wall)
    }

    /// Hints at `now_wall`: never a need.
    #[must_use]
    pub const fn hints(&self, now_wall: u64) -> Hints {
        Hints {
            quiet_for_ms: now_wall.saturating_sub(self.st.last_t),
        }
    }

    /// THE BUS'S OPEN PATH: the deepest open run, the open turn and the deepest open step of the bus class (a step a need is raised from) — never a tool, subagent or compact step, which stays local, even when one is open beneath it.
    #[must_use]
    pub fn bus_open_path(&self) -> [Option<(Node, Kind)>; 3] {
        let deepest = |want: &dyn Fn(&OpenNode) -> bool| {
            self.tracked
                .iter()
                .rev()
                .find(|o| want(o))
                .map(|o| (o.node, o.kind))
        };
        [
            deepest(&|o| o.node.lvl == Lvl::Run),
            deepest(&|o| o.node.lvl == Lvl::Turn),
            deepest(&|o| o.node.lvl == Lvl::Step && crate::log::is_bus_step(o.kind)),
        ]
    }

    /// SEED A LATE READER FROM A BUS SNAPSHOT (phase 4, §5.5): this fold becomes the state `snap` (an `op=snap` body's record) names — its open path, its open needs, its mail counts and epoch — as of the snap's seq, so the records after it apply as they would have.
    pub fn seed_snap(&mut self, snap: &Rec) -> bool {
        if snap.op != Op::Snap {
            return false;
        }
        let ep = snap
            .f
            .get_u(Key::Epoch)
            .and_then(|e| u32::try_from(e).ok())
            .unwrap_or(self.st.ep);
        let mut seeded = Fold::new(ep);
        let mut par = Node::SESSION;
        for item in snap.f.get_str(Key::Open).unwrap_or("").split(',') {
            let Some((node, kind)) = item.split_once(':') else {
                continue;
            };
            let (Some(node), kind) = (Node::parse(node), Kind::from_word(kind)) else {
                continue;
            };
            if kind.node_level() != Some(node.lvl) || seeded.tracked.len() >= MAX_TRACKED {
                continue;
            }
            seeded.tracked.push(OpenNode {
                node,
                kind,
                par: Some(par),
                t: snap.t,
                by: Actor::Nobody,
                src: Src::Unknown,
                conf: Conf::Unknown,
                f: NodeFields::default(),
            });
            par = node;
        }
        for item in snap.f.get_str(Key::Needs).unwrap_or("").split(',') {
            let mut it = item.split('.');
            let (Some(word), Some(node), Some(kind), Some(by), Some(src)) =
                (it.next(), it.next(), it.next(), it.next(), it.next())
            else {
                continue;
            };
            let (Some(word), Some(node)) = (
                crate::vocab::lookup(crate::vocab::NEED_WORDS, word),
                Node::parse(node),
            ) else {
                continue;
            };
            seeded.raise(Need {
                word,
                node,
                kind: Kind::from_word(kind),
                since_seq: snap.seq,
                t: snap.t,
                src: Src::from_word(src),
                conf: Conf::Unknown,
                by: Actor::from_word(by),
                detail: None,
            });
        }
        let count = |k: Key| {
            snap.f
                .get_u(k)
                .map_or(0, |n| u32::try_from(n).unwrap_or(u32::MAX))
        };
        seeded.st.totals.mail_in = count(Key::MailIn);
        seeded.st.totals.mail_out = count(Key::MailOut);
        seeded.st.as_of = snap.seq;
        seeded.st.last_t = snap.t;
        seeded.st.rev = 1;
        seeded.reproject();
        *self = seeded;
        true
    }

    /// A VIEW THAT LOST RECORDS (phase 4, `crate::bus`), folded from its start up to `snap` — a snapshot past every record it lost: the open needs as of the snapshot are the snapshot's (a claim or a failed run's close the view never held raised one; a clear it never held lifted one), and the rest of this fold (its totals, its close, its open nodes) is what the held records made of it.
    pub fn reseed_needs(&mut self, snap: &Rec) -> bool {
        let mut seeded = Fold::new(self.st.ep);
        if !seeded.seed_snap(snap) {
            return false;
        }
        self.st.needs = seeded.st.needs;
        self.reproject();
        true
    }

    /// WHAT A BUS FOLD AND THIS ONE MUST AGREE ON (phase 4, design §5.5's projection): the epoch, the open path and the open needs of the bus class, an edge's actor as `alias` names it on the bus.
    #[must_use]
    pub fn bus_facts(
        &self,
        alias: &dyn Fn(&crate::vocab::ShortId) -> Option<crate::vocab::ShortId>,
    ) -> BusFacts {
        let bus_by = |by: Actor| match by {
            Actor::Sid(sid) => alias(&sid).map_or(Actor::Nobody, Actor::sid),
            other => other,
        };
        BusFacts {
            ep: self.st.ep,
            open: self.bus_open_path(),
            needs: self
                .st
                .needs
                .iter()
                .flatten()
                .map(|n| (n.word, n.node, bus_by(n.by), n.src))
                .collect(),
        }
    }

    /// The counters a bus fold that holds a session's records from its first agrees on with the local one: turns by outcome, the need-bearing steps, who closed the approvals, and the session's close.
    #[must_use]
    pub fn bus_totals(&self) -> BusTotals {
        let t = &self.st.totals;
        BusTotals {
            turns: t.turns,
            done: t.done,
            asked: t.asked,
            walled: t.walled,
            interrupted: t.interrupted,
            errors: t.errors,
            other: t.other,
            appr: t.appr,
            appr_by: t.appr_by,
            qs: t.qs,
            stalls: t.stalls,
            closed: self.st.closed.as_ref().map(|c| c.out),
            last_run: self.st.last[0].as_ref().map(|c| (c.node, c.out)),
            last_turn: self.st.last[1].as_ref().map(|c| (c.node, c.out)),
        }
    }

    fn reproject(&mut self) {
        let deepest = |lvl: Lvl| {
            self.tracked
                .iter()
                .rev()
                .find(|o| o.node.lvl == lvl)
                .cloned()
        };
        self.st.open = [deepest(Lvl::Run), deepest(Lvl::Turn), deepest(Lvl::Step)];
    }

    fn on_open(&mut self, r: &Rec) {
        let mut f = NodeFields::default();
        for (k, v) in r.f.iter() {
            f.offer(*k, v.clone(), r.conf, r.seq);
        }
        if r.node.lvl == Lvl::Session {
            self.st.closed = None;
            return;
        }
        if self.tracked.len() >= MAX_TRACKED {
            self.tracked.remove(0);
        }
        let open = OpenNode {
            node: r.node,
            kind: r.k,
            par: r.par,
            t: r.t,
            by: r.by,
            src: r.src,
            conf: r.conf,
            f,
        };
        let continued = (r.node.lvl == Lvl::Step)
            .then(|| r.f.get_str(Key::Cont).and_then(Node::parse))
            .flatten();
        match r.k {
            Kind::Turn => {
                self.st.totals.turns = self.st.totals.turns.saturating_add(1);
                self.link_turn(r);
            }
            _ if continued.is_some() => {}
            Kind::Approval | Kind::Question | Kind::Wall | Kind::Stall | Kind::Tool => {
                self.count_step(r);
            }
            _ if r.node.lvl == Lvl::Step => {
                self.st.totals.steps = self.st.totals.steps.saturating_add(1);
            }
            _ => {}
        }
        if let Some(word) = need_raised(r) {
            let detail = r.f.get_str(Key::Detail).map(str::to_string);
            let since_t = continued
                .and_then(|c| {
                    self.st.last[2]
                        .as_ref()
                        .filter(|l| l.node == c)
                        .and_then(|l| l.opened_t)
                })
                .unwrap_or(r.t);
            self.raise(Need {
                word,
                node: r.node,
                kind: r.k,
                since_seq: r.seq,
                t: since_t,
                src: r.src,
                conf: r.conf,
                by: r.by,
                detail,
            });
        }
        self.tracked.push(open);
        self.prune_links();
        self.reproject();
    }

    fn count_step(&mut self, r: &Rec) {
        let t = &mut self.st.totals;
        t.steps = t.steps.saturating_add(1);
        match r.k {
            Kind::Approval => t.appr = t.appr.saturating_add(1),
            Kind::Question => t.qs = t.qs.saturating_add(1),
            Kind::Stall => t.stalls = t.stalls.saturating_add(1),
            Kind::Tool => {
                if let Some(i) =
                    r.f.get_word(Key::Cls)
                        .and_then(|c| CLS_WORDS.iter().position(|w| *w == c))
                {
                    t.tools[i] = t.tools[i].saturating_add(1);
                }
            }
            _ => {}
        }
    }

    fn on_close(&mut self, r: &Rec) {
        let out = r.out().unwrap_or(crate::vocab::UNKNOWN);
        let opened = self
            .tracked
            .iter()
            .position(|o| o.node == r.node)
            .map(|i| self.tracked.remove(i));
        let node = r.node;
        let mut ended = vec![node];
        let mut walk = vec![node];
        while let Some(parent) = walk.pop() {
            let children: Vec<Node> = self
                .tracked
                .iter()
                .filter(|o| o.par == Some(parent))
                .map(|o| o.node)
                .collect();
            self.tracked.retain(|o| o.par != Some(parent));
            ended.extend(&children);
            walk.extend(children);
        }
        let mut f = opened.as_ref().map(|o| o.f.clone()).unwrap_or_default();
        let amended = f.held(Key::Out).cloned();
        for (k, v) in r.f.iter() {
            f.offer(*k, v.clone(), r.conf, r.seq);
        }
        let out = match amended.filter(|h| amend_refines_out(r.conf, r.seq, h.conf, h.seq)) {
            Some(Held {
                val: Val::Word(w), ..
            }) => w,
            _ => {
                if f.word(Key::Out) != Some(out) {
                    f.put(Key::Out, Val::Word(out), r.conf, r.seq);
                }
                out
            }
        };
        let closed = ClosedNode {
            node,
            kind: r.k,
            out,
            dur_ms: r.f.get_u(Key::DurMs).unwrap_or(0),
            opened_t: opened.as_ref().map(|o| o.t),
            t: r.t,
            by: r.by,
            opened_by: opened.as_ref().map_or(Actor::Nobody, |o| o.by),
            src: r.src,
            conf: r.conf,
            f,
        };
        self.drop_needs(|n| ended.contains(&n.node) && n.word != "exited");
        match node.lvl {
            Lvl::Run => {
                let kind = r.k;
                self.drop_needs(|n| lifts_failure(kind, out, node, n));
                if let Some(word) = need_raised(r) {
                    let detail = r.f.get(Key::Code).and_then(|v| match v {
                        Val::I(c) => Some(c.to_string()),
                        _ => None,
                    });
                    self.raise(Need {
                        word,
                        node,
                        kind,
                        since_seq: r.seq,
                        t: r.t,
                        src: r.src,
                        conf: r.conf,
                        by: r.by,
                        detail,
                    });
                }
                self.st.last[0] = Some(closed);
            }
            Lvl::Turn => {
                let b = turn_bucket(&mut self.st.totals, out);
                *b = b.saturating_add(1);
                self.st.last[1] = Some(closed);
            }
            Lvl::Step => {
                if r.k == Kind::Approval {
                    let slot = match r.by {
                        Actor::Human => 0,
                        Actor::Harness => 1,
                        Actor::Ctl => 2,
                        _ => 3,
                    };
                    let by = &mut self.st.totals.appr_by[slot];
                    *by = by.saturating_add(1);
                }
                self.st.last[2] = Some(closed);
            }
            Lvl::Session => {
                self.tracked.clear();
                self.drop_needs(|n| n.word != "exited");
                if let Some(word) = need_raised(r) {
                    self.raise(Need {
                        word,
                        node,
                        kind: r.k,
                        since_seq: r.seq,
                        t: r.t,
                        src: r.src,
                        conf: r.conf,
                        by: r.by,
                        detail: None,
                    });
                }
                self.st.closed = Some(closed);
            }
            Lvl::Instance => {}
        }
        self.prune_links();
        self.reproject();
    }

    fn on_mark(&mut self, r: &Rec) {
        let word = r.f.get_word(Key::Word);
        match r.k {
            Kind::Need => {
                // A mark's need is the session's: it outlives the node the
                if let Some(w) = need_raised(r) {
                    self.raise(Need {
                        word: w,
                        node: Node::SESSION,
                        kind: r.k,
                        since_seq: r.seq,
                        t: r.t,
                        src: r.src,
                        conf: r.conf,
                        by: r.by,
                        detail: None,
                    });
                }
            }
            Kind::Clear => {
                let (by, src) = (r.by, r.src);
                self.drop_needs(|n| clear_lifts(src, by, word, n));
            }
            Kind::Seen => self.drop_needs(|n| n.word == "failure"),
            Kind::Verdict | Kind::Label => {
                let label = r.k == Kind::Label;
                if let Some(claim) = Claim::of(r, label) {
                    let (trusted, proven) = (claim.by.is_trusted(), claim.by.is_proven());
                    let (any, kept, held) = if label {
                        (
                            &mut self.st.label,
                            &mut self.st.label_trusted,
                            &mut self.st.label_proven,
                        )
                    } else {
                        (
                            &mut self.st.verdict,
                            &mut self.st.verdict_trusted,
                            &mut self.st.verdict_proven,
                        )
                    };
                    if proven {
                        *held = Some(claim.clone());
                    }
                    if trusted {
                        *kept = Some(claim.clone());
                    }
                    *any = Some(claim);
                }
            }
            Kind::Mail => match r.f.get_word(Key::Dir) {
                Some("in") => {
                    self.st.totals.mail_in = self.st.totals.mail_in.saturating_add(1);
                    self.st.mail_open = self.st.mail_open.saturating_add(1);
                    self.in_since_human = true;
                    if let (Some("task"), Some(off)) =
                        (r.f.get_word(Key::MailKind), r.f.get_u(Key::Off))
                    {
                        if self.mail.len() >= MAX_MAIL_WAITING {
                            self.mail.remove(0);
                        }
                        self.mail.push(MailWaiting { off, t: r.t });
                    }
                }
                Some("out") => {
                    self.st.totals.mail_out = self.st.totals.mail_out.saturating_add(1);
                }
                _ => {}
            },
            Kind::MailSeen if r.f.get_u(Key::Moved) != Some(0) => {
                self.st.mail_open = self.st.mail_open.saturating_sub(1);
            }
            Kind::Handoff => {
                if let Some(e) = r.f.get_u(Key::Epoch)
                    && e > u64::from(self.st.ep)
                {
                    self.st.ep = u32::try_from(e).unwrap_or(u32::MAX);
                }
            }
            Kind::Human => {
                self.st.totals.human = self.st.totals.human.saturating_add(1);
                if r.f.get_u(Key::Reached) != Some(0) {
                    self.mail.clear();
                    self.in_since_human = false;
                }
            }
            Kind::Agent => {
                if let Some(w) = word {
                    self.st.phase = Phase {
                        word: w,
                        src: r.src,
                        conf: r.conf,
                        t: r.t,
                    };
                }
            }
            Kind::Status => {
                if let Some(p) = r.f.get_word(Key::Phase) {
                    let phase = Phase {
                        word: p,
                        src: r.src,
                        conf: r.conf,
                        t: r.t,
                    };
                    if self.st.phase.src != Src::Screen || self.st.phase.word == "-" {
                        self.st.phase = phase.clone();
                    }
                    self.shell_phase = Some(phase);
                }
            }
            _ => {}
        }
    }

    fn on_amend(&mut self, r: &Rec) {
        let node = r.node;
        if let Some(o) = self.tracked.iter_mut().find(|o| o.node == node) {
            for (k, v) in r.f.iter() {
                o.f.offer(*k, v.clone(), r.conf, r.seq);
            }
        } else if let Some(c) = self.st.last.iter_mut().flatten().find(|c| c.node == node) {
            for (k, v) in r.f.iter().filter(|(k, _)| *k != Key::Out) {
                c.f.offer(*k, v.clone(), r.conf, r.seq);
            }
            if let Some(new) = r.out() {
                let (held_conf, held_seq) =
                    c.f.held(Key::Out).map_or((c.conf, 0), |h| (h.conf, h.seq));
                if amend_refines_out(held_conf, held_seq, r.conf, r.seq) {
                    c.f.offer(Key::Out, Val::Word(new), r.conf, r.seq);
                    let old = std::mem::replace(&mut c.out, new);
                    if node.lvl == Lvl::Turn && old != new {
                        turn_out_rebucket(&mut self.st.totals, old, new);
                    }
                }
            }
        }
        for n in self.st.needs.iter_mut().flatten() {
            if need_takes_amend(n, r) {
                n.src = r.src;
                n.conf = r.conf;
            }
        }
        self.reproject();
    }

    fn on_gap(&mut self, r: &Rec, reset: bool) {
        self.st.totals.lost = self
            .st
            .totals
            .lost
            .saturating_add(r.f.get_u(Key::Lost).unwrap_or(0));
        if reset {
            if let Some(ep) = r.f.get_u(Key::Epoch) {
                self.st.ep = u32::try_from(ep).unwrap_or(u32::MAX);
            }
            self.tracked.clear();
            self.drop_needs(|n| n.node.lvl == Lvl::Step);
            self.mail.clear();
            self.in_since_human = false;
            self.prune_links();
            self.reproject();
        }
    }

    /// Add a need in precedence order; when full, the least urgent (newest
    fn raise(&mut self, need: Need) {
        let mut all: Vec<Need> = self.st.needs.iter_mut().filter_map(Option::take).collect();
        all.retain(|n| !(n.node == need.node && n.word == need.word && n.by == need.by));
        all.push(need);
        all.sort_by_key(|n| (need_precedence(n), n.since_seq));
        all.truncate(MAX_NEEDS);
        for (slot, n) in self.st.needs.iter_mut().zip(all) {
            *slot = Some(n);
        }
    }

    fn drop_needs(&mut self, mut gone: impl FnMut(&Need) -> bool) {
        let kept: Vec<Need> = self
            .st
            .needs
            .iter_mut()
            .filter_map(Option::take)
            .filter(|n| !gone(n))
            .collect();
        for (slot, n) in self.st.needs.iter_mut().zip(kept) {
            *slot = Some(n);
        }
    }
}

/// Whether the close of run `closed` (kind `kind`, outcome `out`) lifts `need`: THE failure-supersede rule.
#[must_use]
pub fn lifts_failure(kind: Kind, out: &str, closed: Node, need: &Need) -> bool {
    matches!(out, "ok" | "fail")
        && need.word == "failure"
        && need.node.lvl == Lvl::Run
        && need.kind == kind
        && need.node != closed
}

/// THE OUTCOME REFINEMENT (§2.3, §3.7): whether an amend's `out=` at (`conf`, `seq`) replaces the outcome a closed node holds at (`held_conf`, `held_seq`).
#[must_use]
pub const fn amend_refines_out(held_conf: Conf, held_seq: u64, conf: Conf, seq: u64) -> bool {
    matches!(conf, Conf::Exact) && confidence_law(held_conf, held_seq, conf, seq)
}

/// The totals' bucket a turn closed `out` is counted in.
fn turn_bucket<'a>(t: &'a mut Totals, out: &str) -> &'a mut u32 {
    match out {
        "done" => &mut t.done,
        "asked" => &mut t.asked,
        "interrupted" => &mut t.interrupted,
        "error" => &mut t.errors,
        w if w.starts_with("wall:") => &mut t.walled,
        _ => &mut t.other,
    }
}

/// A held turn's outcome refined from `old` to `new` ([`amend_refines_out`]): it leaves its old bucket and joins the new one, so the totals still count each closed turn once.
pub fn turn_out_rebucket(t: &mut Totals, old: &str, new: &str) {
    let from = turn_bucket(t, old);
    *from = from.saturating_sub(1);
    let to = turn_bucket(t, new);
    *to = to.saturating_add(1);
}

/// Whether amend `r` raises `need` to its own source and confidence (phase 3B, A5): an exact `confirmed=1` of the step that raised it — the transcript confirmed the wall the screen saw — and never anything below exact.
#[must_use]
pub fn need_takes_amend(need: &Need, r: &Rec) -> bool {
    r.op == Op::Amend
        && need.node == r.node
        && need.node.lvl == Lvl::Step
        && r.conf == Conf::Exact
        && r.f.get_u(Key::Confirmed) == Some(1)
        && r.conf.rank() > need.conf.rank()
}

/// Whether `r` starts an epoch newer than `ep`: a `gap why=handoff` (or `why=reset`) naming `epoch=` above it.
#[must_use]
pub fn starts_epoch(r: &Rec, ep: u32) -> bool {
    r.op == Op::Gap
        && matches!(r.f.get_word(Key::Why), Some("handoff" | "reset"))
        && r.f.get_u(Key::Epoch).is_some_and(|e| e > u64::from(ep))
}

/// The need words a `need` mark may raise (§7.4): `review`, `decision`, `help`, and `escalated` from the owner's attention flag (`src=meta`, §7.1).
#[must_use]
pub fn mark_may_raise(src: Src, word: &str) -> bool {
    matches!(word, "review" | "decision" | "help") || (word == "escalated" && src == Src::Meta)
}

/// THE NEED A RECORD RAISES (§7.1), or `None`: a step's open ([`step_need`]), a run's close `out=fail` (`failure`), the session's close (`exited`), or a `need` mark [`mark_may_raise`] admits.
#[must_use]
pub fn need_raised(r: &Rec) -> Option<&'static str> {
    match (r.op, r.node.lvl) {
        (Op::Open, Lvl::Step) => step_need(r),
        (Op::Close, Lvl::Run) => (r.out() == Some("fail")).then_some("failure"),
        (Op::Close, Lvl::Session) => Some("exited"),
        (Op::Mark, _) if r.k == Kind::Need => {
            r.f.get_word(Key::Word).filter(|w| mark_may_raise(r.src, w))
        }
        _ => None,
    }
}

/// THE CLEAR RULE (§7.1, §3.7): whether a `clear` from `by` at `src` (naming need `word`, or every need) lifts `need`.
#[must_use]
pub fn clear_lifts(src: Src, by: Actor, word: Option<&str>, need: &Need) -> bool {
    word.is_none_or(|w| need.word == w)
        && need.node.lvl != Lvl::Step
        && (need.by == by || by.overrides(&need.by))
        && (src != Src::Mark || need.src == Src::Mark)
}

/// THE NEED PRECEDENCE (§7.1), lower first: a need raised by a mark ranks last whatever its word, so a claim never outranks an observation — a proven author's ([`Actor::is_proven`]) first among claims, an Owner client's next, any other author's last, so a flood of other authors' needs gives way before the harness's when the needs are full; every other need ranks by its word ([`need_rank`]).
#[must_use]
pub fn need_precedence(n: &Need) -> u8 {
    if n.src == Src::Mark || (n.conf == Conf::Claim && n.src != Src::Meta) {
        if n.by.is_proven() {
            7
        } else if n.by == Actor::Ctl {
            8
        } else {
            9
        }
    } else {
        need_rank(n.word)
    }
}

/// The need a step's open raises (§7.1): `approval:<kind>`, `question`, `wall:<kind>`, or `unresponsive` for an input stall.
#[must_use]
pub fn step_need(r: &Rec) -> Option<&'static str> {
    if r.op != Op::Open || r.node.lvl != Lvl::Step {
        return None;
    }
    match r.k {
        Kind::Approval => Some(
            r.f.get_word(Key::Box)
                .map_or("approval:other", |b| BoxWord::from_wire(b).need()),
        ),
        Kind::Question => Some("question"),
        Kind::Wall => {
            r.f.get_word(Key::Wall)
                .and_then(WallKind::from_name)
                .map(WallKind::wire)
        }
        Kind::Stall => Some("unresponsive"),
        _ => None,
    }
}

/// What a tool class reads as in the clause.
fn tool_words(cls: Option<&str>) -> &'static str {
    match cls {
        Some("edit" | "write") => "editing",
        Some("shell") => "running shell",
        Some("read") => "reading",
        Some("search") => "searching",
        Some("web") => "browsing",
        Some("agent") => "subagent",
        Some("plan") => "planning",
        Some("mcp") => "using a tool",
        _ => "working",
    }
}

/// THE LABEL A READER IS SHOWN (§7.3, D6): a proven author's live label ([`Actor::is_proven`]: the harness's), else a trusted author's (the owner class, `ctl` among it), at `now_wall`; never another author's.
#[must_use]
pub fn shown_label(st: &SessionState, now_wall: u64) -> Option<&Claim> {
    let live = |c: &&Claim| c.text.is_some() && c.until.is_some_and(|u| u > now_wall);
    st.label_proven
        .as_ref()
        .filter(|c| c.by.is_proven())
        .filter(live)
        .or_else(|| {
            st.label_trusted
                .as_ref()
                .filter(|c| c.by.is_trusted())
                .filter(live)
        })
}

/// The verdict claims a reader is shown, in order: a proven author's newest (the harness's), the newest trusted one when it is another claim, then the newest of any author when that is yet another.
#[must_use]
pub fn shown_verdicts(st: &SessionState) -> Vec<&Claim> {
    let mut out: Vec<&Claim> = Vec::with_capacity(3);
    for c in [
        st.verdict_proven.as_ref().filter(|c| c.by.is_proven()),
        st.verdict_trusted.as_ref().filter(|c| c.by.is_trusted()),
        st.verdict.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        if out.iter().all(|o| o.seq != c.seq) {
            out.push(c);
        }
    }
    out
}

/// The label a PROVEN author's slot shows at `now_wall` (the harness's or the person's), never a `ctl` client's or another author's: what a person's tooltip may show.
#[must_use]
pub fn shown_proven_label(st: &SessionState, now_wall: u64) -> Option<&Claim> {
    st.label_proven
        .as_ref()
        .filter(|c| c.by.is_proven())
        .filter(|c| c.text.is_some() && c.until.is_some_and(|u| u > now_wall))
}

/// THE clause policy (§7.3), a pure function of the state and `now_wall`.
#[must_use]
pub fn clause_of(st: &SessionState, now_wall: u64) -> Option<Clause> {
    clause_with(st, now_wall, shown_label)
}

/// [`clause_of`] with only a PROVEN author's label admitted ([`shown_proven_label`]): a `ctl` label neither shows nor hides the step or turn row beneath it.
#[must_use]
pub fn clause_of_proven(st: &SessionState, now_wall: u64) -> Option<Clause> {
    clause_with(st, now_wall, shown_proven_label)
}

/// THE NEED ROW'S WORDS (§7.3): what the clause says of a need `word` with its `detail` (a wall's reset, a failure's exit code), and its tone — `approval · bash`, `limit · 3pm`, `failed · exit 2` — or `None` for a word the clause gives no row (it reads on to the label and the open path).
#[must_use]
pub fn need_words(word: &str, detail: Option<&str>) -> Option<(Words, Tone)> {
    match word {
        w if w.starts_with("approval:") => Some((
            Words::fit("approval", w.trim_start_matches("approval:")),
            Tone::Attention,
        )),
        "question" => Some((Words::fit("question", ""), Tone::Attention)),
        "wall:auth" => Some((Words::fit("logged out", ""), Tone::Attention)),
        w if w.starts_with("wall:") => {
            let kind = WallKind::from_name(w);
            if kind.is_some_and(WallKind::is_limit) {
                Some((Words::fit("limit", detail.unwrap_or("")), Tone::Attention))
            } else {
                Some((
                    Words::fit("wall", w.trim_start_matches("wall:")),
                    Tone::Attention,
                ))
            }
        }
        "unresponsive" => Some((Words::fit("not responding", ""), Tone::Attention)),
        "failure" => {
            let words = match detail {
                Some(code) => Words::fit("failed", &format!("exit {code}")),
                None => Words::fit("failed", ""),
            };
            Some((words, Tone::Failed))
        }
        _ => None,
    }
}

/// The clause policy over the label rule `label`.
fn clause_with(
    st: &SessionState,
    now_wall: u64,
    label: fn(&SessionState, u64) -> Option<&Claim>,
) -> Option<Clause> {
    let age = |t: u64| now_wall.saturating_sub(t);
    let mk = |words: Words, age_ms: u64, tone: Tone, by: Actor| Clause {
        words,
        age_ms,
        tone,
        by,
        row: ClauseRow::Need,
        head: words,
    };
    let aged = |subject: &str, a: u64, tone: Tone, by: Actor, row: ClauseRow| Clause {
        words: Words::fit(subject, &age_short(a)),
        age_ms: a,
        tone,
        by,
        row,
        head: Words::fit(subject, ""),
    };
    if let Some(n) = st.top_need()
        && let Some((words, tone)) = need_words(n.word, n.detail.as_deref())
    {
        return Some(mk(words, age(n.t), tone, n.by));
    }
    if let Some(l) = label(st, now_wall)
        && let Some(text) = &l.text
    {
        let tone = if st.turn().is_some() {
            Tone::Working
        } else {
            Tone::Idle
        };
        return Some(aged(text, age(l.t), tone, l.by, ClauseRow::Label));
    }
    if let Some(step) = st.step() {
        match step.kind {
            Kind::Tool => {
                let words = tool_words(step.f.word(Key::Cls));
                return Some(aged(
                    words,
                    age(step.t),
                    Tone::Working,
                    step.by,
                    ClauseRow::Step,
                ));
            }
            Kind::Subagent => {
                let c = mk(
                    Words::fit("subagent", ""),
                    age(step.t),
                    Tone::Working,
                    step.by,
                );
                return Some(Clause {
                    row: ClauseRow::Step,
                    ..c
                });
            }
            _ => {}
        }
    }
    if let Some(turn) = st.turn() {
        return Some(aged(
            "working",
            age(turn.t),
            Tone::Working,
            turn.by,
            ClauseRow::Turn,
        ));
    }
    if let Some(run) = st.run() {
        if run.kind == Kind::Cmd {
            let detail = run
                .f
                .str(Key::Detail)
                .or(run.f.str(Key::Prog))
                .unwrap_or("running");
            return Some(aged(
                detail,
                age(run.t),
                Tone::Working,
                run.by,
                ClauseRow::Run,
            ));
        }
        if run.kind == Kind::Agent {
            let interrupted = st.last[1].as_ref().is_some_and(|t| t.out == "interrupted");
            let (word, since) = match &st.last[1] {
                Some(t) => (if interrupted { "interrupted" } else { "idle" }, t.t),
                None => ("idle", run.t),
            };
            let c = mk(Words::fit(word, ""), age(since), Tone::Idle, run.by);
            return Some(Clause {
                row: ClauseRow::Run,
                ..c
            });
        }
    }
    None
}
