// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Deterministic verb-lane endpoint model for link bridge simulation.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

use super::lock;

/// The endpoint's rules, each switchable for a negative control.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct EpRules {
    /// `deliver` takes one row per offset.
    pub(crate) dedup: bool,
    /// The lane the endpoint names in `hello` (3: `continues=` is read).
    pub(crate) lane: u32,
    /// A changed `link log` forgets offsets at all.
    pub(crate) forgets: bool,
    /// A session holding a generation of the fork's chain AFTER `was` keeps below the fork too.
    pub(crate) chain: bool,
    /// A replaced log (`forgot=1`) forgets every session log's mirror cursor.
    pub(crate) mirror_forgets: bool,
}

impl Default for EpRules {
    fn default() -> Self {
        Self {
            dedup: true,
            lane: aterm_types::control_verbs::BRIDGE_LANE,
            forgets: true,
            chain: true,
            mirror_forgets: true,
        }
    }
}

/// One row of a session's inbox ring.
#[derive(Clone, Debug)]
pub(crate) struct Row {
    pub(crate) id: u64,
    pub(crate) off: u64,
    /// Its offset names a record of the log the endpoint was last told.
    pub(crate) on_log: bool,
    pub(crate) from: String,
    pub(crate) kind: String,
    pub(crate) text: String,
    pub(crate) handled: bool,
}

/// One post a session queued.
#[derive(Clone, Debug)]
pub(crate) struct Post {
    pub(crate) id: u64,
    pub(crate) to: String,
    pub(crate) kind: String,
    pub(crate) re: Option<u64>,
    pub(crate) body: Vec<u8>,
    /// The retirement the bridge sent (`off=<n>[ dup=1]` or `off=- reason=…`).
    pub(crate) retired: Option<String>,
}

/// One receipt a session owes.
#[derive(Clone, Debug)]
pub(crate) struct Receipt {
    pub(crate) rid: u64,
    pub(crate) off: u64,
    pub(crate) verdict: String,
    pub(crate) kind: String,
    pub(crate) from: String,
    pub(crate) earlier: bool,
    pub(crate) retired: Option<String>,
}

/// One session the endpoint hosts.
#[derive(Clone, Debug)]
pub(crate) struct Session {
    pub(crate) sid: String,
    pub(crate) nonce: String,
    pub(crate) rows: Vec<Row>,
    pub(crate) dedup: BTreeSet<u64>,
    next_row: u64,
    pub(crate) posts: Vec<Post>,
    next_post: u64,
    pub(crate) receipts: Vec<Receipt>,
    next_rid: u64,
    /// The fleet hold, with its reason, while one stands.
    pub(crate) fleet_hold: Option<String>,
    /// The log this session's offsets are on.
    pub(crate) bus_log: Option<String>,
    /// The highest addressed offset ever delivered on that log.
    addressed_head: Option<u64>,
    /// The session's log (`aterm_slog`), while the node mirrors it onto the
    /// bus (`[fabric] log = "turns"`).
    pub(crate) log: Option<aterm_slog::Log>,
}

/// A session log's ring in the simulation.
fn log_limits() -> aterm_slog::Limits {
    aterm_slog::Limits::new(16, 1 << 20).expect("a small ring")
}

impl Session {
    fn new(sid: &str, local: u64) -> Self {
        Self {
            log: None,
            sid: sid.to_string(),
            nonce: format!("{local:032x}"),
            rows: Vec::new(),
            dedup: BTreeSet::new(),
            next_row: 1,
            posts: Vec::new(),
            next_post: 1,
            receipts: Vec::new(),
            next_rid: 1,
            fleet_hold: None,
            bus_log: None,
            addressed_head: None,
        }
    }

    /// The addressed refill floor `inbox-seen` reports.
    fn floor(&self) -> Option<u64> {
        self.rows
            .iter()
            .filter(|r| r.on_log && !r.handled)
            .map(|r| r.off)
            .min()
            .or_else(|| self.addressed_head.map(|h| h + 1))
    }

    /// Forget the offsets of another log.
    fn forget(&mut self, keep_below: Option<u64>) {
        let fork = keep_below.unwrap_or(0);
        let kept = |off: u64| off < fork;
        for r in &mut self.rows {
            r.on_log &= kept(r.off);
        }
        for r in &mut self.receipts {
            r.earlier |= !kept(r.off);
        }
        self.dedup.retain(|off| kept(*off));
        self.addressed_head = self.addressed_head.and_then(|h| {
            if kept(h) {
                Some(h)
            } else {
                fork.checked_sub(1)
            }
        });
        for p in &mut self.posts {
            if p.retired.is_none() && p.re.is_some_and(|re| !kept(re)) {
                p.retired = Some("off=- reason=log-replaced".to_string());
            }
        }
    }
}

/// The endpoint's state.
#[derive(Debug, Default)]
pub(crate) struct EpState {
    pub(crate) rules: EpRules,
    /// Live sessions by local id.
    pub(crate) sessions: BTreeMap<u64, Session>,
    /// Sessions that left, kept for the oracle.
    pub(crate) gone: Vec<Session>,
    /// The log the endpoint was last told of, and its fork.
    log: Option<String>,
    /// Every request line, cut, for the trace.
    pub(crate) requests: Vec<String>,
    /// The node mirrors its sessions' logs.
    pub(crate) mirror: bool,
    /// How many torn state files the bridges said they set aside (`link set-aside files=<n>`).
    pub(crate) set_aside_told: u64,
    /// The newest count of torn files a bridge said its quarantine still holds.
    pub(crate) held_said: Option<u64>,
    /// What the newest bridge said of its mirror.
    pub(crate) mirror_said: String,
    /// How many times a bridge said its mirror stopped for its pin.
    pub(crate) pin_stops: u64,
    /// How many of aterm's last words a bridge acknowledged.
    pub(crate) byes_acked: u64,
    /// Every alias key a take named.
    pub(crate) take_keys: std::collections::BTreeSet<String>,
    /// The world's clock as the sessions last stamped a record.
    stamp: aterm_slog::Stamp,
}

/// THE ENDPOINT, shared by the scheduler and the thread that serves the lane.
#[derive(Debug, Default)]
pub(crate) struct Endpoint {
    pub(crate) state: Mutex<EpState>,
}

impl Endpoint {
    pub(crate) fn new(rules: EpRules, mirror: bool) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(EpState {
                rules,
                mirror,
                ..EpState::default()
            }),
        })
    }

    /// A session joins (a tab opened): it is listed from now on, and its log holds its open.
    pub(crate) fn create(&self, local: u64, sid: &str, at: aterm_slog::Stamp) {
        let mut s = lock(&self.state);
        s.stamp = at;
        let mut session = Session::new(sid, local);
        session.bus_log.clone_from(&s.log);
        if s.mirror {
            let mut log = aterm_slog::Log::with_limits(1, log_limits());
            log.open_session(
                aterm_slog::Kind::Spawn,
                aterm_slog::Src::Aterm,
                aterm_slog::Conf::Exact,
                aterm_slog::Actor::Aterm,
                aterm_slog::Fields::new(),
                at,
            )
            .expect("a new log opens its session");
            session.log = Some(log);
        }
        s.sessions.insert(local, session);
    }

    /// A session leaves (its tab closed): its log goes with it.
    pub(crate) fn exit(&self, local: u64) -> Option<String> {
        let mut s = lock(&self.state);
        let mut session = s.sessions.remove(&local)?;
        session.log = None;
        let sid = session.sid.clone();
        s.gone.push(session);
        Some(sid)
    }

    /// aterm loses power: every session goes.
    pub(crate) fn power_cut(&self) {
        let mut s = lock(&self.state);
        let sessions = std::mem::take(&mut s.sessions);
        s.gone.extend(sessions.into_values().map(|mut x| {
            x.log = None;
            x
        }));
        s.log = None;
    }

    /// Session `local` appends to its log.
    pub(crate) fn append_log(&self, local: u64, bus: bool, at: aterm_slog::Stamp) -> bool {
        let mut s = lock(&self.state);
        s.stamp = at;
        let Some(log) = s.sessions.get_mut(&local).and_then(|x| x.log.as_mut()) else {
            return false;
        };
        let mark = if bus {
            aterm_slog::Mark::new(
                aterm_slog::Kind::Seen,
                aterm_slog::Src::Status,
                aterm_slog::Conf::Strong,
            )
        } else {
            aterm_slog::Mark::new(
                aterm_slog::Kind::Bell,
                aterm_slog::Src::Engine,
                aterm_slog::Conf::Exact,
            )
        };
        log.mark(mark, at).is_ok()
    }

    /// The sid of local session `local`, if it lives.
    pub(crate) fn sid(&self, local: u64) -> Option<String> {
        lock(&self.state)
            .sessions
            .get(&local)
            .map(|s| s.sid.clone())
    }

    /// The live locals.
    pub(crate) fn locals(&self) -> Vec<u64> {
        lock(&self.state).sessions.keys().copied().collect()
    }

    /// Session `local` queues a post.
    pub(crate) fn post(&self, local: u64, to: &str, kind: &str, text: &str) -> Option<u64> {
        let mut s = lock(&self.state);
        let session = s.sessions.get_mut(&local)?;
        let id = session.next_post;
        session.next_post += 1;
        session.posts.push(Post {
            id,
            to: to.to_string(),
            kind: kind.to_string(),
            re: None,
            body: text.as_bytes().to_vec(),
            retired: None,
        });
        Some(id)
    }

    /// Session `local` marks every row seen.
    pub(crate) fn seen(&self, local: u64, verdict: Option<&str>) -> Option<String> {
        let mut s = lock(&self.state);
        let session = s.sessions.get_mut(&local)?;
        let newest = session.rows.last()?.id;
        let mut decided = None;
        if let Some(v) = verdict {
            let decided_offs: BTreeSet<u64> = session.receipts.iter().map(|r| r.off).collect();
            if let Some(row) = session
                .rows
                .iter()
                .rev()
                .find(|r| r.kind == "ask" && r.on_log && !decided_offs.contains(&r.off))
            {
                let rid = session.next_rid;
                session.next_rid += 1;
                session.receipts.push(Receipt {
                    rid,
                    off: row.off,
                    verdict: v.to_string(),
                    kind: row.kind.clone(),
                    from: row.from.clone(),
                    earlier: false,
                    retired: None,
                });
                decided = Some(v.to_string());
            }
        }
        for r in &mut session.rows {
            r.handled = true;
        }
        let floor = session.floor();
        let mut line = format!(
            "EVENT {local} inbox-seen {newest} floor={} seen={newest}",
            floor.map_or_else(|| "-".to_string(), |f| f.to_string())
        );
        if let Some(v) = decided {
            line.push_str(&format!(" verdict={v}"));
        }
        Some(line)
    }

    /// Serve the verb lane on `far` (the bridge holds the other end) until the bridge closes it.
    pub(crate) fn serve(self: &Arc<Self>, far: UnixStream) {
        let ep = Arc::clone(self);
        std::thread::Builder::new()
            .name("sim-endpoint".to_string())
            .spawn(move || {
                let Ok(read_half) = far.try_clone() else {
                    return;
                };
                let mut out = far;
                let mut reader = BufReader::new(read_half);
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line) {
                        Ok(0) | Err(_) => return,
                        Ok(_) => {}
                    }
                    let request = line.trim_end_matches('\n');
                    let reply = ep.answer(request);
                    if out.write_all(reply.as_bytes()).is_err() {
                        return;
                    }
                }
            })
            .expect("the endpoint's thread");
    }

    /// One request's whole reply, framed as the verb table frames it.
    fn answer(&self, request: &str) -> String {
        let mut s = lock(&self.state);
        let cut: String = request.chars().take(160).collect();
        s.requests.push(cut);
        let mut words = request.split_whitespace();
        let verb = words.next().unwrap_or("");
        match verb {
            "hello" => {
                let build = request
                    .split_whitespace()
                    .find_map(|t| t.strip_prefix("build="))
                    .unwrap_or("-")
                    .to_string();
                let asked = request.split_whitespace().any(|t| t == "log=turns");
                let log = if asked && s.mirror { " log=turns" } else { "" };
                format!("OK lane={} build={build}{log}\n", s.rules.lane)
            }
            "log-take" if s.mirror => s.log_take(words),
            "log-landed" if s.mirror => s.log_landed(words),
            "outbox" => match words.next() {
                None | Some("earlier-receipts=1") => {
                    let frame = s.outbox_frame();
                    format!("OK {}\n{frame}", frame.len())
                }
                Some("sent") => s.retire_post(words),
                Some(_) => "ERR usage: outbox\n".to_string(),
            },
            "sessions" => match words.next() {
                Some("bridge") => {
                    let mut r = format!("OK {}\n", s.sessions.len());
                    for (local, session) in &s.sessions {
                        r.push_str(&format!(
                            "{local} {} nonce={}\n",
                            session.sid, session.nonce
                        ));
                    }
                    r
                }
                _ => "ERR denied\n".to_string(),
            },
            "deliver" => s.deliver(words),
            "hold" => s.hold(words),
            "link" => match words.next() {
                Some("log") => s.link_log(words),
                Some("bye") => match (words.next(), words.next()) {
                    (Some("quit" | "handoff"), None) => {
                        s.byes_acked += 1;
                        "OK\n".to_string()
                    }
                    _ => "ERR usage\n".to_string(),
                },
                Some("set-aside") if s.rules.lane >= 4 => {
                    match words.next().and_then(|w| w.strip_prefix("files=")) {
                        Some(n) => {
                            s.set_aside_told += n.parse::<u64>().unwrap_or(0);
                            "OK\n".to_string()
                        }
                        None => "ERR usage\n".to_string(),
                    }
                }
                Some("set-aside") => "ERR usage\n".to_string(),
                Some("quarantine") if s.rules.lane >= 5 => {
                    let torn = words
                        .next()
                        .and_then(|w| w.strip_prefix("torn="))
                        .and_then(|n| n.parse::<u64>().ok());
                    match (torn, words.next()) {
                        (Some(n), None) => {
                            s.held_said = Some(n);
                            "OK\n".to_string()
                        }
                        _ => "ERR usage\n".to_string(),
                    }
                }
                Some("quarantine") => "ERR usage\n".to_string(),
                Some("mirror") => {
                    s.mirror_said = words.collect::<Vec<_>>().join(" ");
                    if s.mirror_said == "stopped reason=pin" {
                        s.pin_stops += 1;
                    }
                    "OK\n".to_string()
                }
                _ => "OK\n".to_string(),
            },
            v if v.starts_with('@') => {
                let sid = &v[1..];
                match words.collect::<Vec<_>>().as_slice() {
                    ["topic", "ls"] => "OK 0\n".to_string(),
                    ["meta"] if s.session_by_sid(sid).is_some() => "OK\n".to_string(),
                    ["status"] => match s.session_by_sid(sid) {
                        Some(session) => {
                            format!("OK hold={}\n", u8::from(session.fleet_hold.is_some()))
                        }
                        None => "ERR no such session\n".to_string(),
                    },
                    _ => "ERR denied\n".to_string(),
                }
            }
            _ => "ERR denied\n".to_string(),
        }
    }
}

impl EpState {
    fn session_by_sid(&mut self, sid: &str) -> Option<&mut Session> {
        self.sessions.values_mut().find(|s| s.sid == sid)
    }

    /// Every queued post, then every owed receipt.
    fn outbox_frame(&self) -> String {
        let mut frame = String::new();
        let queued: Vec<Vec<&Post>> = self
            .sessions
            .values()
            .map(|s| s.posts.iter().filter(|p| p.retired.is_none()).collect())
            .collect();
        let deepest = queued.iter().map(Vec::len).max().unwrap_or(0);
        for depth in 0..deepest {
            for (session, posts) in self.sessions.values().zip(&queued) {
                let Some(p) = posts.get(depth) else {
                    continue;
                };
                frame.push_str(&format!(
                    "post sid={} id={} to={} kind={}",
                    session.sid, p.id, p.to, p.kind
                ));
                if let Some(re) = p.re {
                    frame.push_str(&format!(" re={re}"));
                }
                frame.push_str(&format!(" len={}\n", p.body.len()));
                frame.push_str(&String::from_utf8_lossy(&p.body));
            }
        }
        for session in self.sessions.values() {
            for r in session.receipts.iter().filter(|r| r.retired.is_none()) {
                frame.push_str(&format!(
                    "receipt sid={} rid={} off={} verdict={} kind={} from={}{}\n",
                    session.sid,
                    r.rid,
                    r.off,
                    r.verdict,
                    r.kind,
                    r.from,
                    if r.earlier { " log=earlier" } else { "" }
                ));
            }
        }
        frame
    }

    fn retire_post<'a>(&mut self, mut words: impl Iterator<Item = &'a str>) -> String {
        let (Some(sid), Some(id)) = (
            words.next(),
            words.next().and_then(|n| n.parse::<u64>().ok()),
        ) else {
            return "ERR usage: outbox sent\n".to_string();
        };
        let rest: Vec<&str> = words.collect();
        let Some(session) = self.session_by_sid(sid) else {
            return "ERR no such session\n".to_string();
        };
        if let Some(p) = session.posts.iter_mut().find(|p| p.id == id) {
            if p.retired.is_none() {
                p.retired = Some(rest.join(" "));
            }
        }
        "OK\n".to_string()
    }

    fn deliver<'a>(&mut self, mut words: impl Iterator<Item = &'a str>) -> String {
        let Some(sid) = words.next() else {
            return "ERR usage: deliver\n".to_string();
        };
        let toks: Vec<&str> = words.collect();
        let field = |key: &str| {
            toks.iter()
                .find_map(|t| t.strip_prefix(key).and_then(|v| v.strip_prefix('=')))
        };
        let dedup = self.rules.dedup;
        let Some(session) = self.session_by_sid(sid) else {
            return "ERR no such session\n".to_string();
        };
        if let Some(rid) = field("receipt").and_then(|n| n.parse::<u64>().ok()) {
            let off = field("off").unwrap_or("-");
            if let Some(r) = session.receipts.iter_mut().find(|r| r.rid == rid) {
                if r.retired.is_none() {
                    r.retired = Some(format!("off={off}"));
                }
            }
            return "OK\n".to_string();
        }
        if field("fetched").is_some() {
            return "OK\n".to_string();
        }
        let Some(off) = field("off").and_then(|n| n.parse::<u64>().ok()) else {
            return "ERR invalid deliver\n".to_string();
        };
        if dedup && session.dedup.contains(&off) {
            let id = session
                .rows
                .iter()
                .rev()
                .find(|r| r.off == off && r.on_log)
                .map_or(0, |r| r.id);
            return format!("OK {id}\n");
        }
        let id = session.next_row;
        session.next_row += 1;
        let text = field("text").map(crate::pct::decode).unwrap_or_default();
        session.rows.push(Row {
            id,
            off,
            on_log: true,
            from: field("from").unwrap_or("-").to_string(),
            kind: field("kind").unwrap_or("note").to_string(),
            text,
            handled: false,
        });
        session.dedup.insert(off);
        if field("topic").is_none() {
            session.addressed_head = Some(session.addressed_head.map_or(off, |h| h.max(off)));
        }
        format!("OK {id}\n")
    }

    /// `log-take n=<k> …`.
    fn log_take<'a>(&mut self, words: impl Iterator<Item = &'a str>) -> String {
        let words: Vec<&str> = words.collect();
        let n: usize = words
            .iter()
            .find_map(|t| t.strip_prefix("n="))
            .and_then(|n| n.parse().ok())
            .unwrap_or(aterm_slog::TAKE_MAX);
        let key = words
            .iter()
            .find_map(|t| t.strip_prefix("key="))
            .unwrap_or_default();
        self.take_keys.insert(key.to_string());
        let at = self.stamp;
        let mut logs: Vec<(&String, &mut aterm_slog::Log)> = self
            .sessions
            .values_mut()
            .filter_map(|s| s.log.as_mut().map(|l| (&s.sid, l)))
            .collect();
        logs.sort_by(|a, b| a.0.cmp(b.0));
        let (mut body, mut k, mut more) = (String::new(), 0usize, false);
        for (sid, log) in logs {
            if k == n {
                more = true;
                break;
            }
            let copy = log.bus_copy(n - k, at);
            more |= copy.more();
            for s in copy.encode(aterm_slog::Tier::Wire, &|_| None).slots {
                body.push_str(&format!("{sid} {} {}\n", s.at.word(), s.body));
                k += 1;
            }
        }
        let more = if more { " more=1" } else { "" };
        format!("OK {} slots={k}{more}\n{body}", body.len())
    }

    /// `log-landed <sid> <cursor> …`.
    fn log_landed<'a>(&mut self, words: impl Iterator<Item = &'a str>) -> String {
        let toks: Vec<&str> = words.collect();
        let mut answers = Vec::new();
        for pair in toks.chunks(2) {
            let [sid, at] = pair else {
                return "ERR usage: log-landed\n".to_string();
            };
            let Some(at) = aterm_slog::BusCursor::parse(at) else {
                return "ERR usage: log-landed\n".to_string();
            };
            let word = match self.session_by_sid(sid).and_then(|s| s.log.as_mut()) {
                Some(log) => match log.land(at) {
                    Ok(aterm_slog::Landed::Moved) => "ok",
                    Ok(aterm_slog::Landed::Stale) => "stale",
                    Err(aterm_slog::LandRefused::Fenced) => "fenced",
                    Err(aterm_slog::LandRefused::Ahead) => "ahead",
                },
                None => "gone",
            };
            answers.push(word);
        }
        format!("OK {}\n", answers.join(" "))
    }

    fn hold<'a>(&mut self, mut words: impl Iterator<Item = &'a str>) -> String {
        let Some(sid) = words.next() else {
            return "ERR usage: hold\n".to_string();
        };
        let on = words.next() == Some("on");
        let reason = words
            .find_map(|t| t.strip_prefix("reason="))
            .unwrap_or("fleet-halt")
            .to_string();
        let Some(session) = self.session_by_sid(sid) else {
            return "ERR no such session\n".to_string();
        };
        session.fleet_hold = on.then_some(reason);
        format!("OK hold={}\n", u8::from(on))
    }

    /// `link log <id> [continues=<f> was=<old>]`: a change forgets the
    /// sessions' offsets; from lane 3 on, a session holding `old` keeps those
    /// below `f`.
    fn link_log<'a>(&mut self, mut words: impl Iterator<Item = &'a str>) -> String {
        let Some(ident) = words.next().map(str::to_string) else {
            return "ERR usage: link log\n".to_string();
        };
        let toks: Vec<&str> = words.collect();
        let field = |key: &str| {
            toks.iter()
                .find_map(|t| t.strip_prefix(key).and_then(|v| v.strip_prefix('=')))
        };
        let continues = if self.rules.lane >= 3 {
            field("continues")
                .and_then(|f| f.parse::<u64>().ok())
                .zip(field("was").map(str::to_string))
        } else {
            None
        };
        let forgets = self.rules.forgets;
        let chain = self.rules.chain;
        let mut changed = self.log.as_ref().is_some_and(|was| *was != ident);
        for session in self.sessions.values_mut() {
            if session.bus_log.as_deref() == Some(ident.as_str()) {
                continue;
            }
            let forget = match session.bus_log.as_deref() {
                None => changed || !session.dedup.is_empty(),
                Some(was) => was != ident,
            };
            if forget && forgets {
                let keep_below = continues
                    .as_ref()
                    .filter(|(_, was)| {
                        session.bus_log.as_deref().is_some_and(|held| {
                            held == was.as_str() || (chain && in_chain(was, held, &ident))
                        })
                    })
                    .map(|(f, _)| *f);
                session.forget(keep_below);
                changed = true;
            }
            session.bus_log = Some(ident.clone());
        }
        self.log = Some(ident);
        if changed {
            // AND THE MIRROR'S CURSORS, every one: what the bus held of each
            // log was another log's (aterm-gui's `forget_landed`).
            if self.rules.mirror_forgets {
                for log in self.sessions.values_mut().filter_map(|s| s.log.as_mut()) {
                    log.forget_mirror();
                }
            }
            "OK forgot=1\n".to_string()
        } else {
            "OK\n".to_string()
        }
    }
}

/// Whether a session holding `held` keeps below a fork a bridge names from `was` to `now`.
fn in_chain(was: &str, held: &str, now: &str) -> bool {
    let split = |s: &str| -> Option<(String, u64)> {
        let (id, generation) = s.split_once('.')?;
        Some((id.to_string(), generation.parse().ok()?))
    };
    if held == was {
        return true;
    }
    match (split(was), split(held), split(now)) {
        (Some((a, w)), Some((b, h)), Some((c, n))) => a == b && b == c && w < h && h < n,
        _ => false,
    }
}
