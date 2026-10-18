// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm fabric brief` (`docs/DESIGN-session-log-2026-10-15.md` §5.5, session
//! log phase 4): a bounded fold of the session logs the bus mirror put on the
//! fleet's `ev` faces, from the BUS ALONE — no control socket, no Owner token,
//! no file written — under whatever caps this host's `[fabric] command` (or
//! `--cap-file`) reads `pub/>` with.
//!
//! THE READER'S VIEW IS THE LAW (ruling Q0): each body is decoded
//! (`aterm_slog::decode_bus`) into the view of its `(node, sid, ep)` — one per
//! seq space (ruling Q9: a newer space, a crash recovery's or an uncarried
//! update's, is a fresh fold, partial until it holds its first record or a
//! snapshot) — which shows each `(ep, seq)` once, unions the covers, and
//! folds in seq order whatever order the bus holds the bodies in. A `v=1`
//! body (the bridge's own verdicts) and any other version is counted
//! `opaque=` and left alone; a `v=2` body that is not what this build's
//! mirror publishes (a record outside its bus class, a cover past any seq)
//! is `foreign=` and never folded, and a key above the bus's tier is
//! stripped (`aterm_slog::bus`). A well-formed record outside this build's
//! bus class still holds its `ms` — one whose kind, operation or node level
//! this build does not know among them: a newer writer's wider class or new
//! word is no hole.
//!
//! BOUNDED: the window is the last `--window` records of the log (65,536 by
//! default, at most 1,048,576); it is read in pages of at most [`PAGE_MAX`]
//! rows (each page's scan the broker's `FETCH_SCAN_MAX`), continuing on the
//! page's mark until the window is read (ruling Q9), so one reply is at most
//! a page of bodies (each at most 512 bytes: half a MiB). At most
//! [`SESSIONS_MAX`] sessions are folded, and the whole is held to `--budget`
//! tokens (`used=` counts every line under the header): each session, newest
//! first, gets its share of what is left, its head line alone when its share
//! holds no more, and the sessions past the budget are `more=sessions:<k>`.
//!
//! A session without its close is `partial why=node-gone` (ruling Q4) when
//! its node's presence ended (`state=gone`, its will) or its own presence
//! row is an earlier incarnation's than the node's live one (a crash or a
//! quit, then a relaunch that never re-announced it), and `partial
//! why=no-close` when its own presence says it exited: a quit, a crash or a
//! lost tail took what the bus never got. One that lost records no snapshot
//! it holds folds past is `partial why=lost`. An `ms` neither a record nor a
//! cover holds, from the first the window reaches (the whole space when the
//! window reaches the log's start or the space's), is printed `holes=`, loss
//! nobody said (the broker's clamp, a retention); the window's own cut is
//! `partial why=no-start`, never a hole.

use std::collections::BTreeMap;
use std::io;
use std::process::ExitCode;

use aterm_slog::bus::BusSpace;

use crate::bridge::Config;
use crate::render::safe;
use crate::transport;

/// The most rows one `Fetch` page asks for (ruling Q9).
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
}

impl BriefOpts {
    /// `aterm fabric brief …`'s words read.
    ///
    /// # Errors
    ///
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
                    let n: u64 = v
                        .parse()
                        .ok()
                        .filter(|n| (1..=WINDOW_MAX).contains(n))
                        .ok_or_else(|| format!("--window {}: 1..={WINDOW_MAX}", safe(&v, 32)))?;
                    o.window = Some(n);
                }
                "--budget" => {
                    let v = value()?;
                    let n: usize = v
                        .parse()
                        .ok()
                        .filter(|n| (100..=100_000).contains(n))
                        .ok_or_else(|| format!("--budget {}: 100..=100000", safe(&v, 32)))?;
                    o.budget = Some(n);
                }
                "--cap-file" => o.cap_files.push(value()?),
                at if at.starts_with('@') && o.sid.is_none() => {
                    let mut parts = at[1..].splitn(2, '@');
                    let sid = parts.next().unwrap_or_default();
                    if !crate::subject::is_principal(sid) {
                        return Err(format!("{} is not a session", safe(at, 64)));
                    }
                    o.sid = Some(sid.to_string());
                    if let Some(node) = parts.next() {
                        if !crate::join::is_node_id(node) {
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
struct Session {
    spaces: BTreeMap<u32, BusSpace>,
    presence: Option<String>,
    /// The incarnation its presence row was written under (`inc=`).
    presence_inc: Option<u64>,
    newest_off: u64,
}

/// What the read of the bus came to.
#[derive(Default)]
struct Read {
    head: u64,
    from: u64,
    scanned: u64,
    pages: u64,
    opaque: u64,
    sessions: BTreeMap<(String, String), Session>,
    nodes_gone: Vec<String>,
    /// Each live node's incarnation, from its own presence row.
    node_inc: BTreeMap<String, u64>,
}

impl Read {
    /// One `ev` record into its `(node, sid, ep)` view.
    fn ingest(&mut self, fleet: &str, want: Option<&str>, off: u64, subject: &str, body: &[u8]) {
        let Some((node, sid)) = owner(fleet, subject, "ev") else {
            return;
        };
        if want.is_some_and(|want| want != sid) {
            return;
        }
        let Ok(b) = aterm_slog::decode_bus(&String::from_utf8_lossy(body)) else {
            self.opaque += 1;
            return;
        };
        let key = (node.to_string(), sid.to_string());
        if !self.sessions.contains_key(&key) && self.sessions.len() >= SESSIONS_MAX {
            return;
        }
        let s = self.sessions.entry(key).or_default();
        s.newest_off = s.newest_off.max(off);
        s.spaces
            .entry(b.ep)
            .or_insert_with(|| BusSpace::new(b.ep))
            .offer(b);
    }
}

/// The `(node, sid)` of `/f/<F>/pub/<node>/<sid>/<leaf>`.
fn owner<'a>(fleet: &str, subject: &'a str, leaf: &str) -> Option<(&'a str, &'a str)> {
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
        .and_then(|v| v.parse().ok())
}

fn read(cfg: &Config, opts: &BriefOpts) -> io::Result<Read> {
    let broker = safe(&cfg.broker, 256);
    let (mut conn, _closer) =
        transport::connect(&cfg.transport, &cfg.broker, crate::fabric::IO_TIMEOUT)
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
    // THE ROSTER, from presence's last values: who is live, who exited, and
    // which nodes' presence ended (their will fired).
    transport::walk_last(&mut conn, &format!("/f/{fleet}/pub/*/*/presence"), |rec| {
        let (_, subject, body) = rec;
        if let Some((node, sid)) = owner(fleet, subject, "presence") {
            if sid == "node" {
                if state_of(body).as_deref() == Some("gone") {
                    r.nodes_gone.push(node.to_string());
                } else if let Some(inc) = inc_of(body) {
                    r.node_inc.insert(node.to_string(), inc);
                }
            } else if opts.sid.as_deref().is_none_or(|want| want == sid) {
                let s = r
                    .sessions
                    .entry((node.to_string(), sid.to_string()))
                    .or_default();
                s.presence = state_of(body);
                s.presence_inc = inc_of(body);
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
        let (rows, (next, _)) = conn.fetch(at, &ev, PAGE_MAX)?;
        r.pages += 1;
        r.scanned = next.saturating_sub(r.from);
        for (off, subject, body) in rows {
            r.ingest(fleet, opts.sid.as_deref(), off, &subject, &body);
        }
        if next <= at {
            break;
        }
        at = next;
    }
    Ok(r)
}

/// The smallest share a session's lines below its head are worth rendering.
const BODY_MIN_TOKENS: usize = 24;

/// The tokens one line costs, its newline included.
fn line_tokens(line: &str) -> usize {
    aterm_slog::tokens(line.len() + 1)
}

/// Why a session's fold is partial, beyond where it was seeded: its close
/// never reached the bus although its node or its own presence says it is
/// gone (the module doc).
fn gone_without_close(r: &Read, node: &str, s: &Session) -> Option<&'static str> {
    if r.nodes_gone.iter().any(|n| n == node) {
        return Some("node-gone");
    }
    if s.presence.as_deref() == Some("exited") {
        return Some("no-close");
    }
    let earlier = match (s.presence_inc, r.node_inc.get(node)) {
        (Some(mine), Some(now)) => mine < *now,
        _ => false,
    };
    (s.presence.as_deref() == Some("live") && earlier).then_some("node-gone")
}

/// The brief's text: a header, then each session's lines, newest first.
fn render(cfg: &Config, opts: &BriefOpts, r: &Read, now_ms: u64) -> String {
    let budget = opts.budget.unwrap_or(BUDGET_DEFAULT);
    let mut shown: Vec<(&(String, String), &Session)> = r
        .sessions
        .iter()
        .filter(|(_, s)| !s.spaces.is_empty())
        .collect();
    shown.sort_by_key(|(_, s)| std::cmp::Reverse(s.newest_off));
    let mut more = shown.len().saturating_sub(SESSIONS_MAX);
    shown.truncate(SESSIONS_MAX);
    // The `more=` line's own room is kept back from the start.
    let budget_lines = budget.saturating_sub(line_tokens("more=sessions:99999"));
    let (mut lines, mut used, mut partial, mut folded) = (Vec::new(), 0usize, 0usize, 0usize);
    let (mut dup, mut reordered, mut foreign) = (0u64, 0u64, 0u64);
    for (i, ((node, sid), s)) in shown.iter().enumerate() {
        let Some((ep, space)) = s.spaces.iter().next_back() else {
            continue;
        };
        let (fold, seed) = space.fold_seeded();
        let st = fold.state();
        let mut why: Vec<&str> = Vec::new();
        if let Some(w) = seed.why() {
            why.push(w);
        }
        if let Some(w) = gone_without_close(r, node, s).filter(|_| st.closed.is_none()) {
            why.push(w);
        }
        // Loss nobody said, from the first `ms` the window reaches: one that
        // does not reach the space's start (nor the log's) cut it, which
        // `no-start` says.
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
            st.as_of
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
        // THE BUDGET, held whole: this session's share of what is left.
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
            let b = aterm_slog::brief(sid, space.records(), None, st, room, now_ms);
            // The session's own lines, not its reply header.
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

/// `aterm fabric brief [@<sid>[@<node>]] [--window <n>] [--budget <tokens>]
/// [--cap-file <path>]`.
#[must_use]
pub fn brief_main(opts: &BriefOpts) -> ExitCode {
    let cfg = match crate::fabric::configured_for_reads() {
        Ok(cfg) => cfg,
        Err(code) => return code,
    };
    match read(&cfg, opts) {
        Ok(r) => {
            let text = render(&cfg, opts, &r, crate::now_ms());
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
