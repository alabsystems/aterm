// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm fabric brief`: the fold of the bus mirror (`docs/DESIGN-session-log-2026-10-15.md` §5.5, §7.1).

use std::collections::BTreeMap;
use std::io;
use std::path::Path;
use std::process::ExitCode;

use aterm_slog::bus::{BusSpace, Gone, Seed};
use aterm_slog::{
    Actor, Conf, Fields, Fold, GapWhy, Key, Kind, Lvl, Need, Node, Op, Out, Rec, Src,
};

use crate::bridge::Config;
use crate::fabric::J;
use crate::render::safe;
use crate::transport;

/// The most rows one `Fetch` page asks for.
pub const PAGE_MAX: u32 = 1024;
/// The window's default: one broker scan's worth of records.
pub const WINDOW_DEFAULT: u64 = 65_536;
/// The window's ceiling: sixteen scans.
pub const WINDOW_MAX: u64 = 1 << 20;
/// The most sessions folded.
pub const SESSIONS_MAX: usize = 1024;
/// The default token budget for the whole brief.
pub const BUDGET_DEFAULT: usize = 2000;

/// What `aterm fabric brief` was asked.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BriefOpts {
    /// `@<sid>[@<node>]`: one session.
    pub sid: Option<String>,
    /// The node of `sid`, when named.
    pub node: Option<String>,
    /// `--window <records>`.
    pub window: Option<u64>,
    /// `--budget <tokens>`.
    pub budget: Option<usize>,
    /// `--cap-file <path>`: read under these caps instead of the node's.
    pub cap_files: Vec<String>,
    /// `--json`: the machine form.
    pub json: bool,
}

impl BriefOpts {
    /// `aterm fabric brief …`'s words read.
    /// # Errors
    /// An unknown flag, a missing value, or a value out of its bound.
    pub fn parse(rest: &[String]) -> Result<BriefOpts, String> {
        let mut o = BriefOpts::default();
        let mut it = rest.iter();
        while let Some(w) = it.next() {
            let mut value = || {
                it.next()
                    .cloned()
                    .ok_or_else(|| format!("{w} needs a value"))
            };
            match w.as_str() {
                "--window" => {
                    let v = value()?;
                    let n: u64 = (!v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
                        .then(|| v.parse().ok())
                        .flatten()
                        .filter(|n| (1..=WINDOW_MAX).contains(n))
                        .ok_or_else(|| format!("--window {}: 1..={WINDOW_MAX}", safe(&v, 32)))?;
                    o.window = Some(n);
                }
                "--budget" => {
                    let v = value()?;
                    let n: usize = (!v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
                        .then(|| v.parse().ok())
                        .flatten()
                        .filter(|n| (100..=100_000).contains(n))
                        .ok_or_else(|| format!("--budget {}: 100..=100000", safe(&v, 32)))?;
                    o.budget = Some(n);
                }
                "--cap-file" => o.cap_files.push(value()?),
                "--json" => o.json = true,
                at if at.starts_with('@') && o.sid.is_none() => {
                    let mut parts = at[1..].splitn(2, '@');
                    let sid = parts.next().unwrap_or_default();
                    if !crate::subject::is_principal(sid) {
                        return Err(format!("{} is not a session", safe(at, 64)));
                    }
                    o.sid = Some(sid.to_string());
                    if let Some(node) = parts.next() {
                        if !crate::enable::is_node_id(node) {
                            return Err(format!("{} is not a node id", safe(node, 64)));
                        }
                        o.node = Some(node.to_string());
                    }
                }
                other => return Err(format!("unknown brief word {}", safe(other, 64))),
            }
        }
        Ok(o)
    }
}

/// One session as the brief holds it: its spaces' views, newest last.
#[derive(Default)]
pub(crate) struct Session {
    pub(crate) spaces: BTreeMap<u32, BusSpace>,
    presence: Option<String>,
    /// The incarnation its presence row was written under (`inc=`).
    presence_inc: Option<u64>,
    /// The newest incarnation of its node that hosted it, as far as this read saw: the
    /// greatest `inc=` of a row of its presence that did not say `exited` ([`retired`]).
    lived_inc: Option<u64>,
    /// The earliest incarnation a row of its presence said `exited` under, as far as this read
    /// saw ([`withdrawn`]): the one that hosted it, when its bridge withdrew it (a quit, its
    /// own exit), or a later one, when that one's ghost sweep retired the row it left live.
    exited_inc: Option<u64>,
    /// The offset of the first record of a successor on its node whose restore names it
    /// (`restore prev=<sid>`, [`restores`]): a new shell stands in the pane it held, after a
    /// relaunch (a crash's or a quit's) or a reopened tab. Whether it died is [`restore_says_died`].
    successor: Option<u64>,
    /// The greatest seq of a body offered to each of its spaces, whether or not the space took
    /// it (one outside this build's bus class is held unfolded): a derived record is numbered
    /// past it ([`derive_close`]).
    pub(crate) offered: BTreeMap<u32, u64>,
    newest_off: u64,
}

/// What the read of the bus came to: each session's spaces and presence, and each node's
/// presence. `aterm fabric brief` reads one, and `aterm link notify --on need` keeps one as
/// it follows the bus ([`Read::following`]).
#[derive(Default)]
pub(crate) struct Read {
    head: u64,
    from: u64,
    scanned: u64,
    pages: u64,
    pub(crate) opaque: u64,
    pub(crate) sessions: BTreeMap<(String, String), Session>,
    /// The nodes whose own row is their will ([`crate::glance::is_will`]).
    nodes_gone: Vec<String>,
    /// Each live node's incarnation, from its own presence row.
    node_inc: BTreeMap<String, u64>,
    /// When each node in [`Self::node_inc`] or [`Self::nodes_gone`] was last heard from: the
    /// count of node rows read when its own row was ([`Self::node_rows`]).
    node_heard: BTreeMap<String, u64>,
    /// The node rows read so far.
    node_rows: u64,
    /// At [`SESSIONS_MAX`] sessions (or nodes), forget the one heard from least recently
    /// instead of refusing the next: a follower's bound, where a one-shot brief asks to be
    /// narrowed. A session was heard at its newest `ev` record, a node at its newest row.
    evicts: bool,
}

impl Read {
    /// The read a follower keeps ([`Self::evicts`]).
    pub(crate) fn following() -> Read {
        Read {
            evicts: true,
            ..Read::default()
        }
    }

    /// A retained session, shared by the roster and the record fold.
    fn session_mut(&mut self, node: &str, sid: &str) -> io::Result<&mut Session> {
        let key = (node.to_string(), sid.to_string());
        if !self.sessions.contains_key(&key) && self.sessions.len() >= SESSIONS_MAX {
            if !self.evicts {
                return Err(io::Error::other(format!(
                    "fabric brief exceeds {SESSIONS_MAX} sessions; narrow it with @<sid>[@<node>]"
                )));
            }
            let stalest = self
                .sessions
                .iter()
                .min_by_key(|(_, s)| s.newest_off)
                .map(|(k, _)| k.clone());
            if let Some(stalest) = stalest {
                self.sessions.remove(&stalest);
            }
        }
        Ok(self.sessions.entry(key).or_default())
    }

    /// One roster row, as the last-value walk delivers it.
    pub(crate) fn presence(&mut self, node: &str, sid: &str, body: &[u8]) -> io::Result<()> {
        if sid == "node" {
            let gone = state_of(body)
                .as_deref()
                .is_some_and(crate::glance::is_will);
            let inc = inc_of(body);
            if gone || inc.is_some() {
                let known =
                    self.node_inc.contains_key(node) || self.nodes_gone.iter().any(|n| n == node);
                if !known && self.node_inc.len() + self.nodes_gone.len() >= SESSIONS_MAX {
                    if !self.evicts {
                        return Err(io::Error::other(format!(
                            "fabric brief exceeds {SESSIONS_MAX} nodes; narrow it with \
                             @<sid>@<node>"
                        )));
                    }
                    let stalest = self
                        .node_heard
                        .iter()
                        .min_by_key(|(_, at)| **at)
                        .map(|(n, _)| n.clone());
                    if let Some(stalest) = stalest {
                        self.node_inc.remove(&stalest);
                        self.nodes_gone.retain(|n| *n != stalest);
                        self.node_heard.remove(&stalest);
                    }
                }
                self.node_inc.remove(node);
                self.nodes_gone.retain(|n| n != node);
                self.node_rows += 1;
                self.node_heard.insert(node.to_string(), self.node_rows);
                if gone {
                    self.nodes_gone.push(node.to_string());
                } else if let Some(inc) = inc {
                    self.node_inc.insert(node.to_string(), inc);
                }
            }
        } else {
            let s = self.session_mut(node, sid)?;
            let (state, inc) = (state_of(body), inc_of(body));
            s.row(state.as_deref(), inc);
            s.presence = state;
            s.presence_inc = inc;
        }
        Ok(())
    }

    /// An earlier row of a session's presence than the one [`Self::presence`] took (a one-shot
    /// read's window, [`lived_history`]): only the incarnation it names, as one that hosted the
    /// session or one it exited under, for a session the read holds.
    fn earlier_presence(&mut self, node: &str, sid: &str, body: &[u8]) {
        if let Some(s) = self.sessions.get_mut(&(node.to_string(), sid.to_string())) {
            s.row(state_of(body).as_deref(), inc_of(body));
        }
    }

    /// One `ev` record into its `(node, sid, ep)` view; and, when it is a successor's restore
    /// that names a session of its node the read holds for the first time ([`restores`]), that
    /// session's `(node, sid)`. A read narrowed to one sid still reads its node's other
    /// sessions' restores for it.
    pub(crate) fn ingest(
        &mut self,
        fleet: &str,
        want: Option<&str>,
        off: u64,
        subject: &str,
        body: &[u8],
    ) -> io::Result<Option<(String, String)>> {
        let Some((node, sid)) = owner(fleet, subject, "ev") else {
            return Ok(None);
        };
        let mine = want.is_none_or(|want| want == sid);
        if !mine && !body.windows(RESTORE_WORD.len()).any(|w| w == RESTORE_WORD) {
            return Ok(None);
        }
        let decoded = aterm_slog::decode_bus(&String::from_utf8_lossy(body));
        let named = decoded
            .as_ref()
            .ok()
            .and_then(|b| restores(&b.rec))
            .filter(|prev| *prev != sid && want.is_none_or(|want| want == *prev))
            .and_then(|prev| self.restored(node, prev, off));
        if !mine {
            return Ok(named);
        }
        let Ok(b) = decoded else {
            self.opaque += 1;
            return Ok(named);
        };
        let s = self.session_mut(node, sid)?;
        s.newest_off = s.newest_off.max(off);
        let (ep, seq) = (b.ep, b.rec.seq.min(aterm_slog::SEQ_LIMIT));
        let top = s.offered.entry(ep).or_default();
        *top = (*top).max(seq);
        s.spaces
            .entry(ep)
            .or_insert_with(|| BusSpace::new(ep))
            .offer(b);
        Ok(named)
    }

    /// A successor's restore at `off` names session `prev` of `node`: its key, when the read
    /// holds it and no earlier restore named it.
    fn restored(&mut self, node: &str, prev: &str, off: u64) -> Option<(String, String)> {
        let key = (node.to_string(), prev.to_string());
        let s = self.sessions.get_mut(&key)?;
        if s.successor.is_some() {
            return None;
        }
        s.successor = Some(off);
        Some(key)
    }
}

impl Session {
    /// A row of this session's presence read: an incarnation that hosted it, or, when it says
    /// `exited`, one it exited under.
    fn row(&mut self, state: Option<&str>, inc: Option<u64>) {
        let Some(inc) = inc else {
            return;
        };
        if state == Some("exited") {
            self.exited_inc = Some(self.exited_inc.map_or(inc, |e| e.min(inc)));
        } else {
            self.lived_inc = Some(self.lived_inc.map_or(inc, |l| l.max(inc)));
        }
    }
}

/// The token every successor's restore body carries (`k=restore`), which a read narrowed to
/// another sid looks for before it decodes a body.
const RESTORE_WORD: &[u8] = b"k=restore";

/// The session a successor's record names as the one it restores: the `open - restore` or
/// `amend - restore` with `prev=<sid>` that a restore's new shell writes on its own session
/// node (aterm-gui's `note_restored`) when it fills the pane the old session held: at every
/// cold relaunch, a crash's or a clean quit's, and when a closed tab is reopened, so the record
/// alone says nothing of how the old session ended ([`restore_says_died`]). A seamless update
/// keeps the session's own sid and names no `prev=`.
fn restores(rec: &Rec) -> Option<&str> {
    (rec.node == Node::SESSION && rec.k == Kind::Restore && matches!(rec.op, Op::Open | Op::Amend))
        .then(|| rec.f.get_str(Key::Prev))
        .flatten()
}

/// The `(node, sid)` of `/f/<F>/pub/<node>/<sid>/<leaf>`.
pub(crate) fn owner<'a>(fleet: &str, subject: &'a str, leaf: &str) -> Option<(&'a str, &'a str)> {
    let rest = subject.strip_prefix("/f/")?.strip_prefix(fleet)?;
    let mut it = rest.strip_prefix("/pub/")?.split('/');
    let (node, sid, l) = (it.next()?, it.next()?, it.next()?);
    (l == leaf && it.next().is_none()).then_some((node, sid))
}

/// The `state=` word of a presence body.
fn state_of(body: &[u8]) -> Option<String> {
    String::from_utf8_lossy(body)
        .split_whitespace()
        .find_map(|t| t.strip_prefix("state="))
        .map(|w| safe(w, 16))
}

/// The `inc=` of a presence body.
fn inc_of(body: &[u8]) -> Option<u64> {
    String::from_utf8_lossy(body)
        .split_whitespace()
        .find_map(|t| t.strip_prefix("inc="))
        .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|v| v.parse().ok())
}

fn read(cfg: &Config, opts: &BriefOpts) -> io::Result<Read> {
    let broker = safe(&cfg.broker, 256);
    let (mut conn, _closer) = transport::connect(&cfg.broker, crate::fabric::io_timeout())
        .map_err(|e| io::Error::new(e.kind(), format!("the broker at {broker}: {e}")))?;
    let caps = if opts.cap_files.is_empty() {
        &cfg.cap_files
    } else {
        &opts.cap_files
    };
    transport::attach_cap_files(&mut conn, caps)?;
    let fleet = &cfg.fleet;
    let ev = match (&opts.node, &opts.sid) {
        (Some(node), Some(sid)) => format!("/f/{fleet}/pub/{node}/{sid}/ev"),
        _ => format!("/f/{fleet}/pub/*/*/ev"),
    };
    let mut r = Read::default();
    transport::walk_last(&mut conn, &format!("/f/{fleet}/pub/*/*/presence"), |rec| {
        let (_, subject, body) = rec;
        if let Some((node, sid)) = owner(fleet, subject, "presence") {
            if opts.node.as_deref().is_some_and(|want| want != node) {
                return Ok(());
            }
            if sid == "node" || opts.sid.as_deref().is_none_or(|want| want == sid) {
                r.presence(node, sid, body)?;
            }
        }
        Ok(())
    })?;
    let (_, (_, head)) = conn.fetch(0, &ev, 0)?;
    let window = opts.window.unwrap_or(WINDOW_DEFAULT);
    r.head = head;
    r.from = head.saturating_sub(window);
    let mut at = r.from;
    while at < head && r.pages < WINDOW_MAX / u64::from(PAGE_MAX) + 16 {
        let ((rows, (next, _)), floor) = transport::fetch_kept(&mut conn, at, &ev, PAGE_MAX)?;
        if let Some(floor) = floor {
            r.from = r.from.max(floor);
        }
        r.pages += 1;
        r.scanned = next.saturating_sub(r.from);
        for (off, subject, body) in rows {
            r.ingest(fleet, opts.sid.as_deref(), off, &subject, &body)?;
        }
        if next <= at {
            break;
        }
        at = next;
    }
    if r.sessions.values().any(hosts_unknown) {
        lived_history(&mut conn, fleet, opts, &mut r)?;
    }
    Ok(r)
}

/// Whether a session with no close of it on the bus, whose last row says `exited` or that a
/// successor's restore names, has no row the read knows that says which incarnation hosted it:
/// the last-value walk hands a one-shot read only a session's newest row (and none once its
/// bridge gave a departed session's row back), so whether a later incarnation retired it
/// ([`retired`]), or the one that hosted it withdrew it ([`withdrawn`]), takes the session's
/// earlier rows ([`lived_history`]).
fn hosts_unknown(s: &Session) -> bool {
    (exited(s) || s.successor.is_some())
        && s.lived_inc.is_none()
        && newest_fold(s).is_some_and(|(_, _, fold, _)| fold.state().closed.is_none())
}

/// The presence rows in the read's window (`from` to `head`), for the incarnations that hosted
/// each session the read holds and those it exited under ([`Read::earlier_presence`]). Read
/// only when some session needs them ([`hosts_unknown`]): a row older than the window, or one
/// retention gave up, is not seen, and a session whose hosting incarnation the read then does
/// not know derives no close.
fn lived_history(
    conn: &mut transport::Conn,
    fleet: &str,
    opts: &BriefOpts,
    r: &mut Read,
) -> io::Result<()> {
    let presence = format!("/f/{fleet}/pub/*/*/presence");
    let (mut at, mut pages) = (r.from, 0u64);
    while at < r.head && pages < WINDOW_MAX / u64::from(PAGE_MAX) + 16 {
        let ((rows, (next, _)), _) = transport::fetch_kept(conn, at, &presence, PAGE_MAX)?;
        pages += 1;
        for (_, subject, body) in rows {
            let Some((node, sid)) = owner(fleet, &subject, "presence") else {
                continue;
            };
            if sid != "node" && opts.node.as_deref().is_none_or(|want| want == node) {
                r.earlier_presence(node, sid, &body);
            }
        }
        if next <= at {
            break;
        }
        at = next;
    }
    Ok(())
}

/// The smallest share a session's lines below its head are worth rendering.
const BODY_MIN_TOKENS: usize = 24;

/// The tokens one line costs, its newline included.
const fn line_tokens(line: &str) -> usize {
    aterm_slog::tokens(line.len() + 1)
}

/// Why a session's fold is partial, beyond where it was seeded: its close
/// never reached the bus although its node or its own presence says it is
/// gone (the words are [`Gone`]'s, which the Overview reads too): what
/// [`gone_settled`] says, or a node that relaunched since the session's
/// presence said it was live.
fn gone_without_close(r: &Read, node: &str, s: &Session) -> Option<Gone> {
    gone_settled(r, node, s).or_else(|| {
        let earlier = match (s.presence_inc, r.node_inc.get(node)) {
            (Some(mine), Some(now)) => mine < *now,
            _ => false,
        };
        (s.presence.as_deref() == Some("live") && earlier).then_some(Gone::NodeGone)
    })
}

/// The half of [`gone_without_close`] that no later row of the session undoes
/// on its own: the node's own row is its will (`state=away`, or the legacy
/// `gone`, [`crate::glance::is_will`]), or the session's presence says it
/// exited. A relaunch is left out: a bridge's attach writes its node's
/// row under the new incarnation before it republishes its sessions' rows, so
/// for that moment every session it carries reads as gone (a brief judges one
/// read; a follower that judged it would clear and page again every standing
/// need at each reattach).
pub(crate) fn gone_settled(r: &Read, node: &str, s: &Session) -> Option<Gone> {
    if r.nodes_gone.iter().any(|n| n == node) {
        return Some(Gone::NodeGone);
    }
    exited(s).then_some(Gone::NoClose)
}

/// Whether the session's own presence says it exited ([`gone_settled`]'s `no-close`).
pub(crate) fn exited(s: &Session) -> bool {
    s.presence.as_deref() == Some("exited")
}

/// Whether the node's own row is its will ([`gone_settled`]).
pub(crate) fn node_gone(r: &Read, node: &str) -> bool {
    r.nodes_gone.iter().any(|n| n == node)
}

/// A session's newest seq space folded from what the bus carried: its epoch, the
/// space, the fold and where the fold started ([`BusSpace::fold_seeded`]). The
/// brief's lines and `--json` objects read it through [`settled_fold`] with
/// either signal of a death (the JSON `need` is
/// [`aterm_slog::SessionState::top_observed_need`]), and so does `aterm link
/// notify --on need` with a successor's restore only, which judges every
/// observed need the fold names, not only the top one (`crate::notify_need`'s
/// `named`).
pub(crate) fn newest_fold(s: &Session) -> Option<(u32, &BusSpace, Fold, Seed)> {
    let (ep, space) = s.spaces.iter().next_back()?;
    let (fold, seed) = space.fold_seeded();
    Some((*ep, space, fold, seed))
}

/// Why a reader knows a session died although its close never reached the bus (fix unit E9,
/// finding E2E-5): a later record says the process that wrote its space is gone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Died {
    /// A successor on its node names it as the session it restores ([`restores`]), and the
    /// incarnation that hosted it ended without withdrawing it ([`restore_says_died`]): its
    /// aterm died and a relaunch put a new shell in its pane.
    Restored,
    /// Its own presence says `exited` under a later incarnation of its node than any this read
    /// saw host it ([`retired`]): a later bridge retired the row of a session it never carried.
    Retired,
}

/// Which signals a reader derives a dead session's close on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Derive {
    /// `aterm fabric brief`'s text and `--json` (and so the Overview, which reads the JSON):
    /// either signal.
    Shown,
    /// `aterm link notify --on need`: a successor's restore only, the one signal a page may
    /// rest on. A row a ghost sweep wrote shows in the brief and pages nothing.
    Paged,
}

/// Whether the session's own presence says it exited under a later incarnation than the newest
/// that hosted it ([`Died::Retired`]). Its node's bridge rewrites every session it hosts under
/// each new incarnation and says `exited` under the one it hosted it in, so a later one is a
/// bridge that never carried it: a relaunch's ghost sweep retiring the row of a session its
/// predecessor left. A read that never saw a row of it that did not say `exited` cannot tell.
fn retired(s: &Session) -> bool {
    exited(s) && matches!((s.presence_inc, s.lived_inc), (Some(at), Some(lived)) if at > lived)
}

/// Whether an incarnation that hosted the session said it `exited`: its bridge withdrew it (a
/// quit's withdrawal, or its own exit) under an `inc=` no later than the newest that hosted it.
/// A ghost sweep says `exited` only under a later one ([`retired`]).
fn withdrawn(s: &Session) -> bool {
    matches!((s.exited_inc, s.lived_inc), (Some(at), Some(lived)) if at <= lived)
}

/// WHETHER A SUCCESSOR'S RESTORE SAYS THE SESSION DIED ([`Died::Restored`]). aterm writes the
/// restore whenever a new shell fills the pane a session held: after a crash, after every clean
/// quit's relaunch, and when a closed tab is reopened. Only the crash leaves the session's row
/// as its bridge last wrote it, so the restore counts when a successor names it, the read knows
/// an incarnation that hosted it, that incarnation ended (its node's row is its will, or says a
/// later `inc=`), and none that hosted it withdrew it ([`withdrawn`]). A quit withdraws every
/// session it carried before its node relaunches (the bridge's `withdraw`), whose closes stay in
/// the journal (`quit_closes`); a reopened tab's successor runs under the closed session's own
/// incarnation, whose close its mirror still publishes. A read that knows no incarnation that
/// hosted the session cannot tell a quit from a crash, and takes no restore for one.
fn restore_says_died(r: &Read, node: &str, s: &Session) -> bool {
    let Some(lived) = s.lived_inc else {
        return false;
    };
    let ended = node_gone(r, node) || r.node_inc.get(node).is_some_and(|now| *now > lived);
    s.successor.is_some() && ended && !withdrawn(s)
}

/// Why the session died with no close on the bus, of the signals `by` takes: a successor's
/// restore first.
fn died(r: &Read, node: &str, s: &Session, by: Derive) -> Option<Died> {
    if restore_says_died(r, node, s) {
        Some(Died::Restored)
    } else if by == Derive::Shown && retired(s) {
        Some(Died::Retired)
    } else {
        None
    }
}

/// A session's newest seq space folded as a reader shows it ([`settled_fold`]).
pub(crate) struct Settled<'a> {
    /// The space's epoch.
    pub(crate) ep: u32,
    /// The space.
    pub(crate) space: &'a BusSpace,
    /// The fold, the derived records applied.
    pub(crate) fold: Fold,
    /// Where the fold started ([`BusSpace::fold_seeded`]).
    pub(crate) seed: Seed,
    /// The fold's head before anything was derived: what a reader hands out as `as_of=` and
    /// `last_seq`, never a seq of a record it derived (§5.4's rule for the journal's reader).
    pub(crate) as_of: u64,
    /// Why its close was derived; `None` when no signal holds or its close reached the bus.
    pub(crate) died: Option<Died>,
    /// The records derived, numbered past the space's head ([`derive_close`]).
    pub(crate) derived: Vec<Rec>,
}

impl Settled<'_> {
    /// Whether the fold holds a close the bus carried.
    pub(crate) fn closed_on_bus(&self) -> bool {
        self.died.is_none() && self.fold.state().closed.is_some()
    }

    /// The detail a reader shows beside need `n`: the need's own, but `gone` for the `exited` a
    /// derived close raised.
    pub(crate) fn detail(&self, n: &Need) -> Option<String> {
        match self.derived.last() {
            Some(close) if n.since_seq == close.seq => {
                Some(Out::gone(Lvl::Session).word().to_string())
            }
            _ => n.detail.clone(),
        }
    }
}

/// THE DEAD SESSION'S CLOSE, DERIVED AT READ TIME (fix unit E9, finding E2E-5; DESIGN §5.5):
/// [`newest_fold`], and, where a signal `by` takes says the session died ([`Died`]) and that
/// space holds no close of it, the fold of a clone with the closes §5.4's journal reader
/// derives for a crashed chain applied ([`derive_close`]): it reads `closed out=gone`, nothing
/// open, and the `exited` that close raises. Never written and never published: no process
/// writes into another process's chain, and a successor does not know the space's head. Derived
/// again at every read, so a later read that holds the close the bus carried shows that close.
/// A node's will alone derives nothing (a reattach may restore the session), nor does the
/// relaunch moment (a session live under an older `inc=` than its node's), nor a restore after
/// a quit withdrew the session or while its own incarnation lives ([`restore_says_died`]).
pub(crate) fn settled_fold<'a>(
    r: &Read,
    node: &str,
    s: &'a Session,
    by: Derive,
) -> Option<Settled<'a>> {
    let (ep, space, mut fold, seed) = newest_fold(s)?;
    let as_of = fold.state().as_of;
    let died = died(r, node, s, by).filter(|_| fold.state().closed.is_none());
    let derived = if died.is_some() {
        derive_close(space, s.offered.get(&ep).copied().unwrap_or(0), &fold)
    } else {
        Vec::new()
    };
    for r in &derived {
        fold.apply(r);
    }
    Some(Settled {
        ep,
        space,
        fold,
        seed,
        as_of,
        died,
        derived,
    })
}

/// The most node closes a derivation writes ([`derive_close`]): a fold tracks at most 64 open
/// nodes (aterm-slog's `MAX_TRACKED`), and each close ends at least one of them.
const CLOSES_MAX: usize = 64;

/// The records §5.4's journal reader derives for a chain whose writer died (aterm-slog's
/// `journal` view `build`: `gap_after(GapWhy::Crash, ..)`, then `close_session(..
/// SessionOut::Gone ..)`): one `gap why=crash after=<head>`, a close `out=gone` of each node
/// the fold holds open, innermost first (each bus step, then each turn, then each run: the
/// fold's open path again after each close, [`Fold::bus_open_path`], since two runs may be
/// open side by side), and the session's close `out=gone`, all by aterm at `src=aterm
/// conf=exact` (the journal's: the doubt is the reader's, and the brief says it beside them in
/// its `partial` word), stamped at the space's newest record, and numbered past the space's head
/// in its own epoch: past every seq the read offered it (`offered`, a body outside this build's
/// bus class among them, which the space holds unfolded), its snapshot's and the fold's, so
/// none takes the seq of a body the space holds (no body past [`aterm_slog::SEQ_LIMIT`] is
/// offered, and at most [`CLOSES_MAX`] closes are derived, so the count never reaches the top
/// of `u64`; it saturates rather than wrap all the same).
fn derive_close(space: &BusSpace, offered: u64, fold: &Fold) -> Vec<Rec> {
    let st = fold.state();
    let head = space
        .records()
        .map(|r| r.seq)
        .last()
        .unwrap_or(0)
        .max(space.snap().map_or(0, |snap| snap.seq))
        .max(st.as_of)
        .max(offered);
    let (t, mut seq) = (st.last_t, head);
    let mut rec = |op: Op, node: Node, k: Kind, f: Fields| {
        seq = seq.saturating_add(1);
        Rec {
            seq,
            t,
            op,
            node,
            par: None,
            k,
            src: Src::Aterm,
            conf: Conf::Exact,
            by: Actor::Aterm,
            f,
        }
    };
    let gone = |lvl: Lvl| {
        Fields::new()
            .word(Key::Out, Out::gone(lvl).word())
            .unwrap_or_default()
    };
    let crash = Fields::new()
        .word(Key::Why, GapWhy::Crash.word())
        .and_then(|f| f.u(Key::After, head))
        .unwrap_or_default();
    let gap = rec(Op::Gap, Node::SESSION, Kind::Gap, crash);
    let mut work = fold.clone();
    work.apply(&gap);
    let mut out = vec![gap];
    let mut path = work.bus_open_path();
    for _ in 0..CLOSES_MAX {
        let Some((node, kind)) = path.iter().rev().flatten().next().copied() else {
            break;
        };
        let close = rec(Op::Close, node, kind, gone(node.lvl));
        work.apply(&close);
        out.push(close);
        let next = work.bus_open_path();
        if next == path {
            break;
        }
        path = next;
    }
    let kind = space
        .records()
        .find(|r| r.op == Op::Open && r.node == Node::SESSION)
        .map_or(Kind::Unknown, |r| r.k);
    out.push(rec(Op::Close, Node::SESSION, kind, gone(Lvl::Session)));
    out
}

/// One session the read holds, by its `(node, sid)`.
type Held<'a> = (&'a (String, String), &'a Session);

/// The sessions a brief shows, newest first: every one the read holds a
/// seq space of, at most [`SESSIONS_MAX`]; the rest's count beside them.
fn shown(r: &Read) -> (Vec<Held<'_>>, usize) {
    let mut shown: Vec<Held<'_>> = r
        .sessions
        .iter()
        .filter(|(_, s)| !s.spaces.is_empty())
        .collect();
    shown.sort_by_key(|(_, s)| std::cmp::Reverse(s.newest_off));
    let more = shown.len().saturating_sub(SESSIONS_MAX);
    shown.truncate(SESSIONS_MAX);
    (shown, more)
}

/// Why a session's newest fold is partial: its seed's word, then its close
/// missing although its node or its own presence says it is gone, or, for a
/// close a read derived ([`settled_fold`]), that its close never reached the
/// bus: `no-close` or `node-gone` as its rows say, else `node-gone` (a
/// successor's restore is a relaunch of its node's).
fn partial_why(r: &Read, node: &str, s: &Session, f: &Settled<'_>) -> Vec<&'static str> {
    let mut why: Vec<&'static str> = Vec::new();
    if let Some(w) = f.seed.why() {
        why.push(w);
    }
    if !f.closed_on_bus() {
        match gone_without_close(r, node, s) {
            Some(gone) => why.push(gone.word()),
            None if f.died.is_some() => why.push(Gone::NodeGone.word()),
            None => {}
        }
    }
    why
}

/// The brief's text: a header, then each session's lines, newest first.
fn render(cfg: &Config, opts: &BriefOpts, r: &Read, now_ms: u64) -> String {
    let budget = opts.budget.unwrap_or(BUDGET_DEFAULT);
    let (shown, mut more) = shown(r);
    let budget_lines = budget.saturating_sub(line_tokens("more=sessions:99999"));
    let (mut lines, mut used, mut partial, mut folded) = (Vec::new(), 0usize, 0usize, 0usize);
    let (mut dup, mut reordered, mut foreign) = (0u64, 0u64, 0u64);
    for (i, ((node, sid), s)) in shown.iter().enumerate() {
        let Some(f) = settled_fold(r, node, s, Derive::Shown) else {
            continue;
        };
        let (ep, space, st) = (f.ep, f.space, f.fold.state());
        let why = partial_why(r, node, s, &f);
        let from = if r.from == 0 || space.holds_start() {
            1
        } else {
            space.min_ms()
        };
        let holes = space.holes_from(from);
        let mut head = format!(
            "@{}@{} {} ep={ep} as_of={}",
            safe(sid, 48),
            safe(node, 48),
            s.presence.as_deref().unwrap_or("-"),
            f.as_of
        );
        if !why.is_empty() {
            head.push_str(&format!(" partial why={}", why.join(",")));
        }
        if !holes.is_empty() {
            let h: Vec<String> = holes.iter().map(|(a, b)| format!("{a}..{b}")).collect();
            head.push_str(&format!(" holes={}", h.join(",")));
        }
        if !space.covers().is_empty() {
            head.push_str(&format!(" lost={}", space.covered()));
        }
        if s.spaces.len() > 1 {
            head.push_str(&format!(" spaces={}", s.spaces.len()));
        }
        let left = budget_lines.saturating_sub(used);
        let head_t = line_tokens(&head);
        if head_t > left {
            more += shown.len() - i;
            break;
        }
        let share = (left / (shown.len() - i)).max(head_t);
        let mut body: Vec<String> = Vec::new();
        let mut body_t = 0usize;
        let room = share - head_t;
        if room >= BODY_MIN_TOKENS {
            let window = space.records().chain(&f.derived);
            let b = aterm_slog::brief(sid, window, None, st, &[], room, now_ms);
            body = b.text.lines().skip(1).map(|l| safe(l, 512)).collect();
            body_t = body.iter().map(|l| line_tokens(l)).sum();
            if body_t > room {
                body.clear();
                body_t = 0;
            }
        }
        if !why.is_empty() {
            partial += 1;
        }
        folded += 1;
        dup += space.dup;
        reordered += space.reordered;
        foreign += space.foreign;
        used += head_t + body_t;
        lines.push(head);
        lines.extend(body);
    }
    if more > 0 {
        let line = format!("more=sessions:{more}");
        used += line_tokens(&line);
        lines.push(line);
    }
    let header = format!(
        "OK {} fabric-brief v=2 fleet={} head=@{} from=@{} scanned={} pages={} sessions={} \
         partial={partial} opaque={} foreign={foreign} dup={dup} reordered={reordered} \
         budget={budget} used={used}",
        lines.len(),
        safe(&cfg.fleet, 32),
        r.head,
        r.from,
        r.scanned,
        r.pages,
        folded,
        r.opaque
    );
    let mut out = header;
    for l in lines {
        out.push('\n');
        out.push_str(&l);
    }
    out
}

/// The open path's level words, run first ([`aterm_slog::SessionState::open`]).
const OPEN_LEVELS: [&str; 3] = ["run", "turn", "step"];

/// The keys of each `sessions` object `aterm fabric brief --json` writes, in its order: the one
/// list [`session_json`] writes (its values are an array of this list's length, so a key and
/// its value come together or the build fails) and `aterm fabric --help` prints
/// ([`crate::fabric::usage`]).
pub const SESSION_KEYS: [&str; 12] = [
    "node", "sid", "this", "ep", "need", "detail", "open", "mail_in", "mail_out", "last_seq",
    "age_s", "partial",
];

/// One session's object of the machine form (a closed schema of ids, closed
/// words and counts), from its newest space's fold at `now_ms`, and whether
/// that fold is partial.
fn session_json(
    r: &Read,
    key: &(String, String),
    s: &Session,
    now_ms: u64,
    this: Option<&str>,
) -> Option<(J, bool)> {
    let (node, sid) = key;
    let f = settled_fold(r, node, s, Derive::Shown)?;
    let (ep, st) = (f.ep, f.fold.state());
    let why = partial_why(r, node, s, &f);
    let need = st.top_observed_need();
    let open = st
        .open
        .iter()
        .zip(OPEN_LEVELS)
        .filter_map(|(n, level)| {
            n.as_ref()
                .map(|n| J::Obj(vec![("level", J::s(level)), ("kind", J::s(n.kind.word()))]))
        })
        .collect();
    let age_s = (st.last_t > 0).then(|| now_ms.saturating_sub(st.last_t) / 1000);
    let partial = !why.is_empty();
    let values: [J; SESSION_KEYS.len()] = [
        J::s(&safe(node, 48)),
        J::s(&safe(sid, 48)),
        J::Bool(this == Some(node.as_str())),
        J::Num(u64::from(ep)),
        J::opt_s(need.map(|n| n.word)),
        J::opt_s(need.and_then(|n| f.detail(n)).as_deref()),
        J::Arr(open),
        J::Num(u64::from(st.totals.mail_in)),
        J::Num(u64::from(st.totals.mail_out)),
        J::Num(f.as_of),
        J::opt_n(age_s),
        J::Arr(why.into_iter().map(J::s).collect()),
    ];
    let obj = J::Obj(SESSION_KEYS.into_iter().zip(values).collect());
    Some((obj, partial))
}

/// The brief's machine form (`--json`): one object, its
/// `sessions` newest first while their objects fit the budget, the rest
/// counted `more`.
fn render_json(
    cfg: &Config,
    opts: &BriefOpts,
    r: &Read,
    now_ms: u64,
    this: Option<&str>,
) -> String {
    let budget = opts.budget.unwrap_or(BUDGET_DEFAULT);
    let (shown, mut more) = shown(r);
    let (mut sessions, mut used, mut partial) = (Vec::new(), 0usize, 0u64);
    let (mut dup, mut reordered, mut foreign) = (0u64, 0u64, 0u64);
    for (i, (key, s)) in shown.iter().enumerate() {
        let Some((obj, is_partial)) = session_json(r, key, s, now_ms, this) else {
            continue;
        };
        let mut one = String::new();
        obj.render(&mut one);
        let t = line_tokens(&one);
        if used + t > budget {
            more += shown.len() - i;
            break;
        }
        if let Some((_, space)) = s.spaces.iter().next_back() {
            dup += space.dup;
            reordered += space.reordered;
            foreign += space.foreign;
        }
        used += t;
        partial += u64::from(is_partial);
        sessions.push(obj);
    }
    let mut out = String::new();
    J::Obj(vec![
        ("v", J::Num(2)),
        ("fleet", J::s(&safe(&cfg.fleet, 32))),
        ("head", J::Num(r.head)),
        ("from", J::Num(r.from)),
        ("scanned", J::Num(r.scanned)),
        ("pages", J::Num(r.pages)),
        ("partial", J::Num(partial)),
        ("opaque", J::Num(r.opaque)),
        ("foreign", J::Num(foreign)),
        ("dup", J::Num(dup)),
        ("reordered", J::Num(reordered)),
        ("budget", J::Num(budget as u64)),
        ("used", J::Num(used as u64)),
        ("more", J::Num(more as u64)),
        ("sessions", J::Arr(sessions)),
    ])
    .render(&mut out);
    out
}

/// `aterm fabric brief [@<sid>[@<node>]] [--window <n>] [--budget <tokens>] [--cap-file <path>]`.
#[must_use]
pub fn brief_main(opts: &BriefOpts) -> ExitCode {
    let cfg = match crate::fabric::configured_for_reads() {
        Ok(cfg) => cfg,
        Err(code) => return code,
    };
    match read(&cfg, opts) {
        Ok(r) => {
            let text = if opts.json {
                let this = crate::enable::read_node(Path::new(&cfg.state_dir));
                render_json(&cfg, opts, &r, crate::now_ms(), this.as_deref())
            } else {
                render(&cfg, opts, &r, crate::now_ms())
            };
            aterm_uds::stdout::answer_exit("aterm fabric brief", &format!("{text}\n"), 0)
        }
        Err(e) => {
            eprintln!("aterm fabric brief: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
#[path = "fabric_brief_tests.rs"]
mod tests;
