// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE SESSION LOG ACROSS A SEAMLESS UPDATE, host side (design §5.2, phase
//! 2A): the producers' own state, which rides the log's carry as its `host`
//! lines (`aterm_slog::Log::carry` passes them through unread), and the
//! successor's adoption of a carried log.
//!
//! Records and the fold are not enough for the successor to go on as the
//! predecessor would have: a command running across the update must close
//! with its own D (its block, its run), an agent mid-turn must stay in that
//! turn (its run, its tracker and the settle it may have armed), and a name
//! the resolver gave just before the update must still open its run. So the
//! carry holds, one line each:
//!
//! ```text
//! state v=1 origin=<k> opened=<wall> root=<pgid> last_blk=<id|-> csnap=<base>,<cursor|-> cmd_end=<wall|-> arrivals=<n>
//! cmd blk=<id> node=<r> wall_from=<wall|-> child_end=<wall|->
//! fg pgid=<p> node=<r> name_fp=<hex>         (the group's latest name, fingerprinted)
//! agent pgid=<p> node=<r> who=<program> by_screen=<0|1>
//! tracker …                                         (aterm_slog::TurnTracker::carry_line)
//! typed input=<seq>,<wall> [input_by=<actor>] [input_cause=<id>] ctl=<seq>,<actor>,<ledger>,<wall> [ctl_cause=<id>]
//! pending pgid=<p> name=<pct|-> word=<w> detail=<pct|-> at=<wall> seq=<n>
//! ```
//!
//! Every stamp crosses as its wall time and is rebased onto the successor's
//! clocks (`aterm_slog::rebase`); an input or `ctl turn` older than the
//! attribution window is left out at the export and dropped at the adoption
//! (it can attribute nothing). Each line is read on its own: one that does
//! not read, or names a node the carried log does not hold open, drops that
//! piece only, and RECONCILE then closes, innermost first, `out=gone`, every
//! node the log holds open that no piece holds, so a partial host state never
//! leaves a node open for good. A line about the session's own open instant
//! is never needed for correctness: the successor's `opened_wall` is the
//! carried one when the `state` line reads, else the adoption instant.

use aterm_slog::{
    ATTRIBUTION_MS, Actor, CarryAdopt, CarryError, CarryStats, Close, Conf, Kind, Log, Lvl, Node,
    Out, ShortId, Src, Stamp, TurnTracker, rebase,
};
#[cfg(any(unix, test))]
use aterm_slog::{CarryBudget, CarryOut, CarrySnapshot, PreparedCarry};

use super::{AgentRun, CSnap, CmdRun, FgRun, LogCarry, PendingName, SessionLog, Typed};

/// What the handoff worker takes of a session's log under its timeline lock
/// ([`SessionLog::carry_source`]): the log's snapshot and the producers'
/// lines. Cut and encoded after the lock is released ([`Self::encode`]).
#[cfg(any(unix, test))]
#[derive(Clone, Debug)]
pub(crate) struct SlogCarrySource {
    snapshot: CarrySnapshot,
    host: Vec<String>,
}

#[cfg(any(unix, test))]
impl SlogCarrySource {
    /// The carry within `budget`, or `None` for a closed session or a
    /// mandatory part exceeding its bounds. Each call encodes anew; the
    /// handoff's export prepares once ([`Self::prepare`]).
    #[cfg(test)]
    pub(crate) fn encode(&self, budget: CarryBudget) -> Option<CarryOut> {
        self.snapshot.encode(budget, &self.host)
    }

    /// Every record's carry line encoded, once, after the timeline's lock is
    /// released: the source every cut of this session's carry is made from.
    pub(crate) fn prepare(self) -> PreparedSlogCarry {
        PreparedSlogCarry {
            prepared: self.snapshot.prepare(),
            host: self.host,
        }
    }
}

/// A [`SlogCarrySource`] whose records are encoded: each cut copies lines.
#[cfg(any(unix, test))]
#[derive(Clone, Debug)]
pub(crate) struct PreparedSlogCarry {
    prepared: PreparedCarry,
    host: Vec<String>,
}

#[cfg(any(unix, test))]
impl PreparedSlogCarry {
    /// Only the mandatory carry, for the handoff's skeleton reservation.
    pub(crate) fn skeleton(&self) -> Option<CarryOut> {
        self.prepared.skeleton(&self.host)
    }

    /// The carry within `budget`, or `None` for a closed session or a
    /// mandatory part exceeding its bounds.
    pub(crate) fn encode(&self, budget: CarryBudget) -> Option<CarryOut> {
        self.prepared.encode(budget, &self.host)
    }

    /// The bus mirror's cursor and head under the fence this carry's take
    /// set (`aterm_slog::PreparedCarry::mirror_prior`); `None` for a log
    /// nothing mirrored.
    pub(crate) fn mirror_prior(&self) -> Option<aterm_slog::Prior> {
        self.prepared.mirror_prior()
    }

    /// The detail past the skeleton, at most: its line bytes and records
    /// (`aterm_slog::PreparedCarry::detail_bytes`), known without encoding
    /// any of it.
    pub(crate) fn detail(&self) -> (usize, usize) {
        (self.prepared.detail_bytes(), self.prepared.detail_records())
    }
}

#[cfg(any(unix, test))]
fn opt_wall(v: Option<u64>) -> String {
    v.map_or_else(|| "-".to_string(), |w| w.to_string())
}

/// `v` percent-encoded, cut first to at most `max` bytes at a char boundary
/// (`-` for none): the free text a `pending` line carries (a program's name,
/// a verdict's word and detail) is the screen's or the resolver's, of any
/// length, and a line past the carry's per-line bound would be left out
/// whole (`aterm_slog::CARRY_MAX_LINE`). With [`PENDING_NAME_MAX`],
/// [`PENDING_WORD_MAX`] and [`PENDING_DETAIL_MAX`], a `pending` line is within
/// it even if every byte takes three.
#[cfg(any(unix, test))]
fn pct_or_dash(v: Option<&str>, max: usize) -> String {
    match v {
        Some(s) => {
            let mut cut = s.len().min(max);
            while !s.is_char_boundary(cut) {
                cut -= 1;
            }
            let mut out = String::new();
            aterm_codec::pct::encode_into(&mut out, &s[..cut]);
            out
        }
        None => "-".to_string(),
    }
}

/// The most bytes of a waiting name's program name that cross (more than any
/// program name a resolver gives).
pub(super) const PENDING_NAME_MAX: usize = 200;
/// The most bytes of a waiting name's verdict word that cross (a word is a
/// few letters).
pub(super) const PENDING_WORD_MAX: usize = 32;
/// The most bytes of a waiting name's verdict detail that cross (a box's
/// kind or a wall's reset; the run records at most `Key::Detail`'s 48).
pub(super) const PENDING_DETAIL_MAX: usize = 64;
// A `pending` line at its widest, every byte taking three, its numbers at
// their widest, within the receiver's per-line bound with its `host ` tag.
const _: () = assert!(
    "host pending pgid=-2147483648 name= word= detail= at=18446744073709551615 seq=18446744073709551615"
        .len()
        + 3 * (PENDING_NAME_MAX + PENDING_WORD_MAX + PENDING_DETAIL_MAX)
        <= aterm_slog::CARRY_MAX_LINE
);

fn kv<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split(' ')
        .filter(|t| !t.is_empty())
        .skip(1)
        .filter_map(|t| t.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v)
}

fn num<T: std::str::FromStr>(line: &str, key: &str) -> Option<T> {
    kv(line, key).and_then(|v| v.parse().ok())
}

/// `-` or a number.
fn opt_num(line: &str, key: &str) -> Option<Option<u64>> {
    match kv(line, key)? {
        "-" => Some(None),
        v => v.parse().ok().map(Some),
    }
}

fn opt_text(line: &str, key: &str) -> Option<Option<String>> {
    match kv(line, key)? {
        "-" => Some(None),
        v => Some(Some(aterm_codec::pct::decode(v))),
    }
}

#[cfg(any(unix, test))]
fn origin_word(k: Kind) -> &'static str {
    k.word()
}

/// The host line that tells a successor this process journals the session up
/// to the carried head (phase 2B). A 2A receiver skips it (`read_host`'s
/// unknown tags).
pub(crate) const JOURNAL_HOST_LINE: &str = "journal v=1";

impl SessionLog {
    /// THE HANDOFF WORKER'S TAKE of this log, under the timeline lock: the
    /// log's carry snapshot and the producers' lines; and from now on the
    /// log's readers are fenced at it (`aterm_slog::Log::fence_reads`) until
    /// the update commits (this process exits) or stands down
    /// ([`Self::lift_read_fence`]).
    #[cfg(any(unix, test))]
    pub(crate) fn carry_source(&mut self) -> SlogCarrySource {
        let now = self.now();
        // The program's newest state its bucket held is written before the
        // fence: the successor's bucket starts full and holds none of it.
        // So is the newest shown tuple (6F-9), which the carry then names.
        self.flush_program(now);
        self.flush_shown(now);
        // The bus mirror's state first (`session_log_mirror::sync`): a
        // replaced bus log's cursor never crosses, and while the node mirrors
        // the carry numbers the bus class even of a log no take reached.
        let snapshot = self.mirror_log().carry_snapshot();
        let host = self.host_lines(now);
        self.log.fence_reads();
        SlogCarrySource { snapshot, host }
    }

    /// The update stood down: this process goes on, and its readers read on.
    #[cfg(any(unix, test))]
    pub(crate) fn lift_read_fence(&mut self) {
        self.log.lift_read_fence();
    }

    /// The producers' state as carry lines (this module's format), at `now`.
    /// First, when this process's journal writer is running (it drains each
    /// fenced log to its fence before the Commit), `journal v=1`: the
    /// successor's journal starts after the carried head. First, because a
    /// carry keeps host lines only within its bound.
    #[cfg(any(unix, test))]
    pub(crate) fn host_lines(&self, now: Stamp) -> Vec<String> {
        let mut out = Vec::new();
        if crate::session_log_journal::journals_to_fence() {
            out.push(JOURNAL_HOST_LINE.to_string());
        }
        let csnap = self.c_snap.map_or_else(
            || "-".to_string(),
            |c| format!("{},{}", c.base, opt_wall(c.cursor)),
        );
        // The remaining marks' last values ride as additive tokens (words, tenths and fingerprints, never text).
        out.push(format!(
            "state v=1 origin={} opened={} root={} last_blk={} csnap={csnap} cmd_end={} arrivals={}{}",
            origin_word(self.origin),
            opt_wall(self.opened_wall),
            self.root_pgid,
            opt_wall(self.last_blk),
            opt_wall(self.cmd_end.map(|s| s.wall_ms)),
            self.arrivals,
            self.observed.carry_tokens()
        ));
        if let Some(c) = self.cmd {
            out.push(format!(
                "cmd blk={} node={} wall_from={} child_end={}",
                c.blk,
                c.node,
                opt_wall(c.wall_from),
                opt_wall(c.child_end.map(|s| s.wall_ms))
            ));
        }
        if let Some(f) = &self.fg {
            out.push(format!(
                "fg pgid={} node={} name_fp={:x}",
                f.pgid, f.node, f.name_fp
            ));
        }
        if let Some(a) = &self.agent {
            out.push(format!(
                "agent pgid={} node={} who={} by_screen={}",
                a.pgid,
                a.node,
                a.who.name(),
                u8::from(a.by_screen)
            ));
            out.push(a.tracker.carry_line(now));
        }
        // Only what can still attribute a turn crosses.
        let fresh = |at: Stamp| aterm_slog::attributes(at, now);
        let input = self.typed.input.filter(|(.., at)| fresh(*at));
        let ctl = self.typed.ctl.filter(|(.., at)| fresh(*at));
        if input.is_some() || ctl.is_some() {
            let mut s = String::from("typed");
            // The input's sender and cause ride as their own tokens (phase
            // 3A), so a 2A line (none: a person's) still reads.
            if let Some((seq, by, cause, at)) = input {
                s.push_str(&format!(" input={seq},{}", at.wall_ms));
                if by != Actor::Human {
                    s.push_str(&format!(" input_by={}", by.word()));
                }
                if let Some(c) = cause {
                    s.push_str(&format!(" input_cause={c}"));
                }
            }
            if let Some((seq, by, ledger, cause, at)) = ctl {
                s.push_str(&format!(" ctl={seq},{},{ledger},{}", by.word(), at.wall_ms));
                if let Some(c) = cause {
                    s.push_str(&format!(" ctl_cause={c}"));
                }
            }
            out.push(s);
        }
        // The waiting names last: each line is read on its own, and the
        // lines above, present whenever their producer holds state, fit the
        // carry's host bytes at their worst (`session_log_tests`), so a
        // burst of wide names costs only names.
        for p in &self.pending {
            out.push(format!(
                "pending pgid={} name={} word={} detail={} at={} seq={}",
                p.pgid,
                pct_or_dash(p.name.as_deref(), PENDING_NAME_MAX),
                pct_or_dash(Some(&p.word), PENDING_WORD_MAX),
                pct_or_dash(p.detail.as_deref(), PENDING_DETAIL_MAX),
                p.at.wall_ms,
                p.seq
            ));
        }
        out
    }

    /// THE CARRIED ADOPTION: continue the predecessor's log from its carry
    /// at the next epoch, its seqs above the manifest's count, and its
    /// producers' state as far as each line reads; then [`Self::reconcile`].
    /// The handoff mark names the predecessor's `build` when the update said
    /// it (phase 6, 6F-7). The caller has the uncarried path for an `Err`.
    pub(crate) fn adopt_carried(
        &mut self,
        lines: &[String],
        ids: LogCarry,
        seq: u64,
        build: Option<u64>,
        at: Stamp,
    ) -> Result<CarryStats, CarryError> {
        let a = CarryAdopt {
            ep: ids.ep.saturating_add(1),
            next_seq: seq.saturating_add(1),
            ids: ids.ids,
            limits: super::ring_limits(),
            at,
        };
        let (log, host, stats) = Log::from_carry_built(lines, a, build)?;
        // A carried successor's epoch is its predecessor's plus one,
        // held without being issued: every later issue is above it.
        self.epochs.note(log.ep());
        self.replace_log(log);
        self.origin = Kind::Adopt;
        // The predecessor journaled the session up to the carried head:
        // this process's journal starts after it. Without the
        // line (its journal off, stopped, or an older build) this process
        // journals the carried records itself; a reader shows each once.
        if host.iter().any(|l| l == JOURNAL_HOST_LINE) {
            self.journal_start = Some(stats.head);
        }
        self.read_host(&host, at);
        self.reconcile(at);
        self.resync();
        Ok(stats)
    }

    /// Put back the producers' state from its carry lines, each on its own:
    /// one that does not read, or names a node the carried log does not hold
    /// open as that piece would, is dropped.
    fn read_host(&mut self, host: &[String], at: Stamp) {
        // Without the `state` line the session's open instant is the
        // adoption's: a block that finished before it is passed over (the
        // predecessor logged it, or nobody can say), and every later one is
        // logged.
        self.opened_wall = Some(at.wall_ms);
        let open_run = |log: &Log, n: Node| log.is_open(n) && n.lvl == Lvl::Run;
        let mut tracker_line: Option<&str> = None;
        for line in host {
            let tag = line.split(' ').next().unwrap_or("");
            match tag {
                "state" if kv(line, "v") == Some("1") => {
                    let mut read = || -> Option<()> {
                        let opened = opt_num(line, "opened")?;
                        let root: i32 = num(line, "root")?;
                        let last_blk = opt_num(line, "last_blk")?;
                        let cmd_end = opt_num(line, "cmd_end")?;
                        let arrivals: u64 = num(line, "arrivals")?;
                        let csnap = match kv(line, "csnap")? {
                            "-" => None,
                            v => {
                                let (base, cursor) = v.split_once(',')?;
                                Some(CSnap {
                                    base: base.parse().ok()?,
                                    cursor: match cursor {
                                        "-" => None,
                                        c => Some(c.parse().ok()?),
                                    },
                                })
                            }
                        };
                        let origin = match kv(line, "origin")? {
                            "spawn" => Kind::Spawn,
                            "restore" => Kind::Restore,
                            _ => Kind::Adopt,
                        };
                        // Read whole, then put back.
                        self.opened_wall = Some(opened.unwrap_or(at.wall_ms));
                        self.root_pgid = root;
                        self.last_blk = last_blk;
                        self.cmd_end = cmd_end.map(|w| rebase(w, at));
                        self.arrivals = arrivals;
                        self.c_snap = csnap;
                        self.origin = origin;
                        Some(())
                    };
                    if read().is_none() {
                        aterm_log::warn!(
                            "update adopt: a session log's carried producer state did not read; \
                             it goes on from the adoption"
                        );
                    }
                    // The remaining marks' last values (phase 3B): absent
                    // from an older line, so unknown, and written afresh.
                    self.observed
                        .read_carry(|k| kv(line, k).map(str::to_string));
                }
                "cmd" => {
                    self.cmd = read_cmd(line, &self.log, at);
                }
                "fg" => self.fg = read_fg(line, &self.log),
                "agent" => {
                    let read = || -> Option<(i32, Node, aterm_phase::Program, bool)> {
                        let node = Node::parse(kv(line, "node")?)?;
                        let who = aterm_phase::program_of(kv(line, "who")?)?;
                        let by_screen = match kv(line, "by_screen")? {
                            "0" => false,
                            "1" => true,
                            _ => return None,
                        };
                        open_run(&self.log, node).then_some((
                            num(line, "pgid")?,
                            node,
                            who,
                            by_screen,
                        ))
                    };
                    if let Some((pgid, node, who, by_screen)) = read() {
                        self.agent = Some(AgentRun {
                            pgid,
                            node,
                            who,
                            by_screen,
                            tracker: TurnTracker::new(node),
                        });
                        self.note_agent_cmd(node);
                    }
                }
                "tracker" => tracker_line = Some(line.as_str()),
                "pending" => {
                    let read = || -> Option<PendingName> {
                        Some(PendingName {
                            pgid: num(line, "pgid")?,
                            name: opt_text(line, "name")?,
                            word: opt_text(line, "word")?.unwrap_or_else(|| "-".to_string()),
                            detail: opt_text(line, "detail")?,
                            at: rebase(num(line, "at")?, at),
                            seq: num(line, "seq")?,
                        })
                    };
                    if let Some(p) = read()
                        && self.pending.len() < super::PENDING_NAMES_MAX
                    {
                        self.pending.push(p);
                    }
                }
                "typed" => {
                    let fresh = |w: u64| at.wall_ms.saturating_sub(w) <= ATTRIBUTION_MS;
                    let mut typed = Typed::default();
                    if let Some(v) = kv(line, "input")
                        && let Some((seq, w)) = v.split_once(',')
                        && let (Ok(seq), Ok(w)) = (seq.parse::<u64>(), w.parse::<u64>())
                        && fresh(w)
                    {
                        let by = kv(line, "input_by").map_or(Actor::Human, Actor::from_word);
                        let cause = kv(line, "input_cause").and_then(ShortId::new);
                        typed.input = Some((seq, by, cause, rebase(w, at)));
                    }
                    if let Some(v) = kv(line, "ctl") {
                        let parts: Vec<&str> = v.split(',').collect();
                        if let [seq, by, ledger, w] = parts[..]
                            && let (Ok(seq), Ok(ledger), Ok(w)) =
                                (seq.parse::<u64>(), ledger.parse::<u64>(), w.parse::<u64>())
                            && fresh(w)
                        {
                            let cause = kv(line, "ctl_cause").and_then(ShortId::new);
                            typed.ctl =
                                Some((seq, Actor::from_word(by), ledger, cause, rebase(w, at)));
                        }
                    }
                    self.typed = typed;
                }
                _ => {}
            }
        }
        // The tracker belongs to the agent run it names.
        if let Some(a) = self.agent.as_mut() {
            match tracker_line
                .and_then(|l| TurnTracker::from_carry_line(l, at))
                .filter(|t| t.run() == a.node)
            {
                Some(t) => a.tracker = t,
                // No tracker: the run stays; its turns start over, a turn the
                // carried log holds open under it closes in the reconcile.
                None => a.tracker = TurnTracker::new(a.node),
            }
        }
    }

    /// Close, innermost first and `out=gone`, every node the adopted log
    /// holds open that no producer holds (nor anything beneath it): a carried
    /// command whose block line did not read, a turn no tracker knows. What a
    /// producer holds stays open and closes when its own signal comes.
    fn reconcile(&mut self, at: Stamp) {
        let mut held: Vec<Node> = Vec::new();
        held.extend(self.cmd.map(|c| c.node));
        held.extend(self.fg.as_ref().map(|f| f.node));
        if let Some(a) = &self.agent {
            held.push(a.node);
            let view = a.tracker.view(at.mono_ms);
            held.extend(view.turn);
            held.extend(view.step.map(|(n, ..)| n));
        }
        let holds = |log: &Log, n: Node| held.iter().any(|h| *h == n || descends_in(log, *h, n));
        let open: Vec<Node> = self.log.open_nodes().iter().map(|e| e.node).collect();
        for n in open.into_iter().rev() {
            if self.log.is_open(n) && !holds(&self.log, n) {
                let _ = self.close_node(
                    n,
                    Close::new(Out::gone(n.lvl), Src::Aterm, Conf::Exact).by(Actor::Aterm),
                    at,
                );
            }
        }
    }
}

/// A carried `cmd` line, when it reads and names a `cmd` run `log` holds
/// open.
fn read_cmd(line: &str, log: &Log, at: Stamp) -> Option<CmdRun> {
    let c = CmdRun {
        blk: num(line, "blk")?,
        node: Node::parse(kv(line, "node")?)?,
        wall_from: opt_num(line, "wall_from")?,
        child_end: opt_num(line, "child_end")?.map(|w| rebase(w, at)),
    };
    (log.is_open(c.node) && log.open_entry(c.node).is_some_and(|e| e.kind == Kind::Cmd))
        .then_some(c)
}

/// A carried `fg` line, when it reads and names a run `log` holds open.
fn read_fg(line: &str, log: &Log) -> Option<FgRun> {
    let f = FgRun {
        pgid: num(line, "pgid")?,
        node: Node::parse(kv(line, "node")?)?,
        name_fp: u64::from_str_radix(kv(line, "name_fp")?, 16).ok()?,
    };
    (log.is_open(f.node) && f.node.lvl == Lvl::Run).then_some(f)
}

/// Whether the open node `n` lies under the open node `root` in `log`.
fn descends_in(log: &Log, n: Node, root: Node) -> bool {
    let mut at = n;
    for _ in 0..=aterm_slog::MAX_OPEN {
        match log.open_entry(at) {
            Some(e) if e.parent == root => return true,
            Some(e) => at = e.parent,
            None => return false,
        }
    }
    false
}
