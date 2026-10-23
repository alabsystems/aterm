// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm fabric brief`: the fold of the bus mirror (`docs/DESIGN-session-log-2026-10-15.md` §5.5, §7.1).

use std::collections::BTreeMap;
use std::io;
use std::path::Path;
use std::process::ExitCode;

use aterm_slog::bus::BusSpace;

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
    /// A retained session, shared by the roster and the record fold.
    fn session_mut(&mut self, node: &str, sid: &str) -> io::Result<&mut Session> {
        let key = (node.to_string(), sid.to_string());
        if !self.sessions.contains_key(&key) && self.sessions.len() >= SESSIONS_MAX {
            return Err(io::Error::other(format!(
                "fabric brief exceeds {SESSIONS_MAX} sessions; narrow it with @<sid>[@<node>]"
            )));
        }
        Ok(self.sessions.entry(key).or_default())
    }

    /// One roster row, as the last-value walk delivers it.
    fn presence(&mut self, node: &str, sid: &str, body: &[u8]) -> io::Result<()> {
        if sid == "node" {
            let gone = state_of(body).as_deref() == Some("gone");
            let inc = inc_of(body);
            if gone || inc.is_some() {
                let known =
                    self.node_inc.contains_key(node) || self.nodes_gone.iter().any(|n| n == node);
                if !known && self.node_inc.len() + self.nodes_gone.len() >= SESSIONS_MAX {
                    return Err(io::Error::other(format!(
                        "fabric brief exceeds {SESSIONS_MAX} nodes; narrow it with @<sid>@<node>"
                    )));
                }
                self.node_inc.remove(node);
                self.nodes_gone.retain(|n| n != node);
                if gone {
                    self.nodes_gone.push(node.to_string());
                } else if let Some(inc) = inc {
                    self.node_inc.insert(node.to_string(), inc);
                }
            }
        } else {
            let s = self.session_mut(node, sid)?;
            s.presence = state_of(body);
            s.presence_inc = inc_of(body);
        }
        Ok(())
    }

    /// One `ev` record into its `(node, sid, ep)` view.
    fn ingest(
        &mut self,
        fleet: &str,
        want: Option<&str>,
        off: u64,
        subject: &str,
        body: &[u8],
    ) -> io::Result<()> {
        let Some((node, sid)) = owner(fleet, subject, "ev") else {
            return Ok(());
        };
        if want.is_some_and(|want| want != sid) {
            return Ok(());
        }
        let Ok(b) = aterm_slog::decode_bus(&String::from_utf8_lossy(body)) else {
            self.opaque += 1;
            return Ok(());
        };
        let s = self.session_mut(node, sid)?;
        s.newest_off = s.newest_off.max(off);
        s.spaces
            .entry(b.ep)
            .or_insert_with(|| BusSpace::new(b.ep))
            .offer(b);
        Ok(())
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
    let (mut conn, _closer) = transport::connect(&cfg.broker, crate::fabric::IO_TIMEOUT)
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
    Ok(r)
}

/// The smallest share a session's lines below its head are worth rendering.
const BODY_MIN_TOKENS: usize = 24;

/// The tokens one line costs, its newline included.
const fn line_tokens(line: &str) -> usize {
    aterm_slog::tokens(line.len() + 1)
}

/// Why a session's fold is partial, beyond where it was seeded: its close
/// never reached the bus although its node or its own presence says it is
/// gone.
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
/// missing although its node or its own presence says it is gone.
fn partial_why(
    r: &Read,
    node: &str,
    s: &Session,
    seed: aterm_slog::bus::Seed,
    closed: bool,
) -> Vec<&'static str> {
    let mut why: Vec<&'static str> = Vec::new();
    if let Some(w) = seed.why() {
        why.push(w);
    }
    if let Some(w) = gone_without_close(r, node, s).filter(|_| !closed) {
        why.push(w);
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
        let Some((ep, space)) = s.spaces.iter().next_back() else {
            continue;
        };
        let (fold, seed) = space.fold_seeded();
        let st = fold.state();
        let why = partial_why(r, node, s, seed, st.closed.is_some());
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
            let b = aterm_slog::brief(sid, space.records(), None, st, &[], room, now_ms);
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
    let (ep, space) = s.spaces.iter().next_back()?;
    let (fold, seed) = space.fold_seeded();
    let st = fold.state();
    let why = partial_why(r, node, s, seed, st.closed.is_some());
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
    let obj = J::Obj(vec![
        ("node", J::s(&safe(node, 48))),
        ("sid", J::s(&safe(sid, 48))),
        ("this", J::Bool(this == Some(node.as_str()))),
        ("ep", J::Num(u64::from(*ep))),
        ("need", J::opt_s(need.map(|n| n.word))),
        ("detail", J::opt_s(need.and_then(|n| n.detail.as_deref()))),
        ("open", J::Arr(open)),
        ("mail_in", J::Num(u64::from(st.totals.mail_in))),
        ("mail_out", J::Num(u64::from(st.totals.mail_out))),
        ("last_seq", J::Num(st.as_of)),
        ("age_s", J::opt_n(age_s)),
        ("partial", J::Arr(why.into_iter().map(J::s).collect())),
    ]);
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
