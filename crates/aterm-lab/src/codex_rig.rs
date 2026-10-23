// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CODEX RIG: what a lab run of a REAL Codex in a private headless aterm needs.

use std::io::{BufRead as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aterm_sysprobe::kproc;

use crate::pyjson::{J, PY};

/// The instance's `sandbox-exec` profile.
#[must_use]
pub fn jail_policy(port: u16) -> String {
    format!(
        "(version 1)(allow default)(deny network-outbound (remote ip))(deny network-inbound (local ip))\
         (deny network-outbound (literal \"/private/var/run/mDNSResponder\"))\
         (allow network-outbound (remote ip \"localhost:{port}\"))\n{}",
        crate::jail::PREFERENCES
    )
}

/// The owner's homes fenced, for a profile that otherwise allows every file ([`jail_policy`]).
/// # Errors
/// No home, or one that is not an absolute non-root path a profile can quote.
pub fn home_guards(homes: &[String]) -> Result<String, String> {
    if homes.is_empty() {
        return Err("no home to fence".into());
    }
    let mut rules: Vec<String> = Vec::new();
    for h in homes {
        if !Path::new(h).is_absolute() || h == "/" || !crate::paths::profile_safe(h) {
            return Err(format!(
                "{h} cannot be fenced: a home must be an absolute non-root path a profile can quote"
            ));
        }
        for (kind, path) in crate::protected::protected_paths(h, crate::protected::CCSOCKS) {
            let rule = format!("(deny file-read* file-write* ({} \"{path}\"))", kind.word());
            if !rules.contains(&rule) {
                rules.push(rule);
            }
        }
        rules.push(format!("(deny file-write* (subpath \"{h}\"))"));
    }
    Ok(rules.concat())
}

/// Lay the stand-ins into `bin`.
/// # Errors
/// The copy or the link could not be made.
pub fn stand_ins(bin: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(bin)?;
    let lab = bin.join("aterm-lab");
    std::fs::copy(&crate::paths::paths().exe, &lab)?;
    std::fs::hard_link(&lab, bin.join("ps"))?;
    Ok(lab)
}

/// Every process's command line, as `ps -axo pid=,command=` prints it.
#[must_use]
pub fn commands() -> Vec<(i32, String)> {
    aterm_uds::spawnfd::command("/bin/ps")
        .args(["-axo", "pid=,command="])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter_map(|l| {
                    let l = l.trim_start();
                    let (pid, cmd) = l.split_once(' ')?;
                    Some((pid.parse().ok()?, cmd.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Every process a run started.
#[must_use]
pub fn ours(root: i32, scratch: &Path) -> Vec<i32> {
    let s = scratch.to_string_lossy().into_owned();
    let me = i32::try_from(std::process::id()).unwrap_or(0);
    let mut roots: Vec<i32> = vec![root];
    roots.extend(
        commands()
            .into_iter()
            .filter(|(p, c)| *p != me && c.contains(&s))
            .map(|(p, _)| p),
    );
    let table = crate::procs::table();
    let mut all: Vec<i32> = Vec::new();
    for r in roots {
        all.extend(crate::procs::descendants_in(&table, r));
        all.push(r);
    }
    all.retain(|&p| p > 1 && p != me);
    all.sort_unstable();
    all.dedup();
    all
}

/// End [`ours`]: `SIGTERM`, a second, then `SIGKILL`, each by pid.
pub fn end_all(root: i32, scratch: &Path) {
    let all = ours(root, scratch);
    for &p in &all {
        let _ = crate::sys::kill(p, libc::SIGTERM);
    }
    std::thread::sleep(Duration::from_secs(1));
    for &p in &all {
        let _ = crate::sys::kill(p, libc::SIGKILL);
    }
}

/// One answer of the fake model.
#[derive(Clone, Debug)]
pub struct Answered {
    pub status: u16,
    pub kind: &'static str,
    pub body: String,
    /// Extra response headers.
    pub headers: Vec<(String, String)>,
    /// How long the answer is held before it is sent.
    pub delay: Duration,
    /// What the request was, for the log (`-` when the answer names nothing).
    pub tag: String,
    /// A controller may hold the response until its input fixture is ready.
    pub gate: Option<Arc<ResponseGate>>,
}

/// A response fixture released explicitly, instead of racing a timed delay.
#[derive(Debug, Default)]
pub struct ResponseGate {
    open: Mutex<bool>,
    changed: std::sync::Condvar,
}

impl ResponseGate {
    pub fn release(&self) {
        *self
            .open
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        self.changed.notify_all();
    }

    fn wait(&self) -> bool {
        let open = self
            .open
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (open, _) = self
            .changed
            .wait_timeout_while(open, Duration::from_secs(60), |open| !*open)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *open
    }
}

impl Answered {
    /// `(status, content type, body)` sent at once, tagged `-`.
    #[must_use]
    pub fn now((status, kind, body): (u16, &'static str, String)) -> Self {
        Self {
            status,
            kind,
            body,
            headers: Vec::new(),
            delay: Duration::ZERO,
            tag: "-".to_string(),
            gate: None,
        }
    }

    /// A stream of `items` ([`sse`]).
    #[must_use]
    pub fn items(items: Vec<J>) -> Self {
        Self::now((200, "text/event-stream", sse(items)))
    }
}

/// What the fake model answers each POST's parsed body with.
pub type Answer = Arc<dyn Fn(&J) -> Answered + Send + Sync>;

/// An assistant message item holding `text`.
#[must_use]
pub fn message_item(text: &str) -> J {
    J::obj([
        ("type", J::s("message")),
        ("id", J::s(format!("msg_{}", nanos()))),
        ("role", J::s("assistant")),
        ("status", J::s("completed")),
        (
            "content",
            J::Arr(vec![J::obj([
                ("type", J::s("output_text")),
                ("text", J::s(text)),
                ("annotations", J::Arr(Vec::new())),
            ])]),
        ),
    ])
}

/// A function call item: the tool `name` called with `args`.
#[must_use]
pub fn call_item(name: &str, args: &J) -> J {
    let t = nanos();
    J::obj([
        ("type", J::s("function_call")),
        ("id", J::s(format!("fc_{t}"))),
        ("call_id", J::s(format!("call_{t}"))),
        ("name", J::s(name)),
        ("status", J::s("completed")),
        ("arguments", J::s(args.py())),
    ])
}

/// The Responses API's stream of one response holding `items`.
#[must_use]
pub fn sse(items: Vec<J>) -> String {
    let rid = format!("resp_{}", nanos());
    let usage = J::obj([
        ("input_tokens", J::Int(1)),
        (
            "input_tokens_details",
            J::obj([("cached_tokens", J::Int(0))]),
        ),
        ("output_tokens", J::Int(1)),
        (
            "output_tokens_details",
            J::obj([("reasoning_tokens", J::Int(0))]),
        ),
        ("total_tokens", J::Int(2)),
    ]);
    let mut events = vec![(
        "response.created",
        J::obj([
            ("type", J::s("response.created")),
            ("response", J::obj([("id", J::s(rid.clone()))])),
        ]),
    )];
    for (index, item) in items.iter().enumerate() {
        events.push((
            "response.output_item.done",
            J::obj([
                ("type", J::s("response.output_item.done")),
                ("output_index", J::Int(i64::try_from(index).unwrap_or(0))),
                ("item", item.clone()),
            ]),
        ));
    }
    events.push((
        "response.completed",
        J::obj([
            ("type", J::s("response.completed")),
            (
                "response",
                J::obj([
                    ("id", J::s(rid)),
                    ("status", J::s("completed")),
                    ("output", J::Arr(items)),
                    ("usage", usage),
                ]),
            ),
        ]),
    ));
    events
        .iter()
        .map(|(e, d)| format!("event: {e}\ndata: {}\n\n", d.py()))
        .collect()
}

/// The fake Responses API on `listener`.
pub fn serve(listener: TcpListener, log: Arc<Mutex<ApiLog>>, answer: Answer) {
    for conn in listener.incoming().flatten() {
        let (log, answer) = (Arc::clone(&log), Arc::clone(&answer));
        std::thread::spawn(move || serve_one(conn, &log, &answer));
    }
}

/// Owned request and successful-response journals.
pub struct ApiLog {
    path: PathBuf,
    clock: Instant,
    next_request: i64,
}

impl ApiLog {
    /// Start a fresh proof's journal.
    #[must_use]
    pub const fn new(path: PathBuf, clock: Instant) -> Self {
        Self {
            path,
            clock,
            next_request: 1,
        }
    }

    /// Called while holding the same mutex as every append.
    /// # Errors
    /// A journal cannot be read or contains malformed JSON.
    pub fn snapshot(&self) -> std::io::Result<(Vec<J>, Vec<J>)> {
        let read = |path: &Path| {
            let text = match std::fs::read_to_string(path) {
                Ok(text) => text,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
                Err(e) => return Err(e),
            };
            text.lines()
                .map(|line| {
                    J::parse(line)
                        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
                })
                .collect()
        };
        Ok((read(&self.path)?, read(&response_log(&self.path))?))
    }

    fn append(path: &Path, row: &J) -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        writeln!(f, "{}", row.dump(PY))
    }

    fn request(&mut self, mut fields: Vec<(String, J)>) -> std::io::Result<i64> {
        let id = self.next_request;
        self.next_request = id
            .checked_add(1)
            .ok_or_else(|| std::io::Error::other("model request identity exhausted"))?;
        fields.extend([
            ("event".into(), J::s("request")),
            ("request_id".into(), J::Int(id)),
            ("mono_ms".into(), J::Int(monotonic_ms(self.clock))),
        ]);
        Self::append(&self.path, &J::Obj(fields))?;
        Ok(id)
    }

    fn response_written(&self, id: i64, status: u16) -> std::io::Result<()> {
        Self::append(
            &response_log(&self.path),
            &J::obj([
                ("event", J::s("response_written")),
                ("request_id", J::Int(id)),
                ("status", J::Int(i64::from(status))),
                ("mono_ms", J::Int(monotonic_ms(self.clock))),
            ]),
        )
    }
}

/// Reply witnesses live beside the unchanged one-row-per-request journal.
#[must_use]
pub fn response_log(request_log: &Path) -> PathBuf {
    request_log.with_extension("responses.log")
}

/// Milliseconds on the clock shared by the model server and its observer.
#[must_use]
pub fn monotonic_ms(clock: Instant) -> i64 {
    i64::try_from(clock.elapsed().as_millis()).unwrap_or(i64::MAX)
}

fn serve_one(conn: TcpStream, log: &Mutex<ApiLog>, answer: &Answer) {
    let Ok(w) = conn.try_clone() else {
        return;
    };
    let mut w = w;
    let mut r = std::io::BufReader::new(conn);
    while let Ok(Some(req)) = crate::stub::read_request(&mut r) {
        let (a, request_id) = if req.method == "POST" {
            let parsed = J::parse_bytes(&req.body).unwrap_or(J::Obj(Vec::new()));
            let (user, tool) = request_view(&parsed);
            let a = answer(&parsed);
            let tail: String = {
                let chars: Vec<char> = user.chars().collect();
                chars[chars.len().saturating_sub(300)..].iter().collect()
            };
            let tool_output = match &tool {
                Some(J::Str(s)) => J::s(s.clone()),
                Some(other) => J::s(other.py()),
                None => J::s("null"),
            };
            let (n, turn) = request_marks(&parsed);
            let said = said_of(&a.body);
            let hold_ms = i64::try_from(a.delay.as_millis()).unwrap_or(i64::MAX);
            let at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis());
            let J::Obj(fields) = J::obj([
                ("t", J::Int(i64::try_from(at).unwrap_or(0))),
                ("tag", J::s(a.tag.clone())),
                ("u", J::s(tail)),
                ("u_full", J::s(user)),
                ("model", parsed.get("model").cloned().unwrap_or(J::Null)),
                (
                    "access_programs",
                    parsed.get("access_programs").cloned().unwrap_or(J::Null),
                ),
                ("tool_output", tool_output),
                ("status", J::Int(i64::from(a.status))),
                ("n", J::Int(i64::try_from(n).unwrap_or(i64::MAX))),
                ("turn", J::s(turn)),
                ("said", J::s(said)),
                ("hold_ms", J::Int(hold_ms)),
            ]) else {
                unreachable!()
            };
            let Some(id) = log.lock().ok().and_then(|mut l| l.request(fields).ok()) else {
                return;
            };
            (a, Some(id))
        } else {
            (
                Answered::now((
                    200,
                    "application/json",
                    r#"{"object":"list","data":[{"id":"fake-model"}],"models":[]}"#.to_string(),
                )),
                None,
            )
        };
        if !a.delay.is_zero() {
            std::thread::sleep(a.delay);
        }
        if let Some(gate) = &a.gate
            && !gate.wait()
        {
            return;
        }
        if write_response(&mut w, &a, || {
            if let Some(id) = request_id {
                log.lock()
                    .map_err(|_| std::io::Error::other("model log poisoned"))?
                    .response_written(id, a.status)?;
            }
            Ok(())
        })
        .is_err()
        {
            return;
        }
    }
}

/// The witness is published only after the complete response and flush
/// succeeded.
fn write_response(
    w: &mut impl std::io::Write,
    a: &Answered,
    written: impl FnOnce() -> std::io::Result<()>,
) -> std::io::Result<()> {
    let reason = match a.status {
        200 => "OK",
        429 => "Too Many Requests",
        _ => "Error",
    };
    let extra: String = a
        .headers
        .iter()
        .map(|(k, v)| format!("{k}: {v}\r\n"))
        .collect();
    let head = format!(
        "HTTP/1.1 {} {reason}\r\ncontent-type: {}\r\n{extra}content-length: {}\r\n\r\n",
        a.status,
        a.kind,
        a.body.len()
    );
    w.write_all(head.as_bytes())?;
    w.write_all(a.body.as_bytes())?;
    w.flush()?;
    written()
}

/// The text of the assistant messages a streamed answer ([`sse`]) holds, one line each.
#[must_use]
pub fn said_of(body: &str) -> String {
    body.lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .filter_map(|d| J::parse(d).ok())
        .filter(|d| d.get("type").and_then(J::as_str) == Some("response.output_item.done"))
        .filter_map(|d| d.get("item").cloned())
        .filter(|it| it.get("type").and_then(J::as_str) == Some("message"))
        .flat_map(|it| {
            it.get("content")
                .and_then(J::as_arr)
                .unwrap_or_default()
                .iter()
                .filter_map(|c| c.get("text").and_then(J::as_str).map(str::to_string))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// How many input items a Responses request carries, and the Codex turn it belongs to.
#[must_use]
pub fn request_marks(body: &J) -> (usize, String) {
    let n = body.get("input").and_then(J::as_arr).map_or(0, |i| i.len());
    let turn = body
        .get("client_metadata")
        .and_then(|m| m.get("turn_id"))
        .and_then(J::as_str)
        .unwrap_or("-")
        .to_string();
    (n, turn)
}

/// The last user text of a Responses request, and its last input item when that is a tool's output.
#[must_use]
pub fn request_view(body: &J) -> (String, Option<J>) {
    let input = body.get("input").and_then(J::as_arr).unwrap_or_default();
    let mut texts: Vec<String> = Vec::new();
    for it in input {
        if it.get("role").and_then(J::as_str) != Some("user") {
            continue;
        }
        match it.get("content") {
            Some(J::Str(t)) => texts.push(t.clone()),
            Some(J::Arr(parts)) => texts.extend(
                parts
                    .iter()
                    .filter(|p| matches!(p, J::Obj(_)))
                    .map(|p| p.get("text").and_then(J::as_str).unwrap_or("").to_string()),
            ),
            _ => {}
        }
    }
    let after_tool = input
        .last()
        .filter(|l| l.get("type").and_then(J::as_str) == Some("function_call_output"))
        .map(|l| l.get("output").cloned().unwrap_or(J::Null));
    (texts.pop().unwrap_or_default(), after_tool)
}

/// Now, in nanoseconds since the epoch: a unique enough id.
#[must_use]
pub fn nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos())
}

/// `ps [-A|-e] [-p PIDS] -o FIELD[=],…` from the kernel's process records.
#[must_use]
pub fn ps_main(args: &[String]) -> i32 {
    let (fields, rows) = match ps_rows(args, kproc::records, kproc::record) {
        Ok(chosen) => chosen,
        Err(why) => {
            eprintln!("ps: {why}");
            return 1;
        }
    };
    let now = crate::util::now_secs();
    let mut out = String::new();
    for r in &rows {
        out.push_str(&ps_line(r, &fields, now));
        out.push('\n');
    }
    let _ = std::io::stdout().write_all(out.as_bytes());
    i32::from(rows.is_empty())
}

/// The `-o` fields `args` ask for and the records they choose, as `ps` chooses them: every
/// process (`all`) with `-A` or `-e` or with no `-p`, else each pid `-p` lists (`one`), pid 0
/// never. A `-p` list holds pids split by commas and spaces; an empty one, or a word that is no
/// pid, is refused.
fn ps_rows(
    args: &[String],
    all: impl FnOnce() -> Option<Vec<kproc::Record>>,
    one: impl FnMut(u32) -> Option<kproc::Record>,
) -> Result<(Vec<String>, Vec<kproc::Record>), String> {
    let (mut fields, mut pids, mut every) = (Vec::new(), None::<Vec<u32>>, false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-A" | "-e" => every = true,
            "-o" => fields.extend(
                it.next()
                    .map(String::as_str)
                    .unwrap_or("")
                    .split(',')
                    .filter(|f| !f.is_empty())
                    .map(|f| f.trim_end_matches('=').to_string()),
            ),
            "-p" => {
                let listed = pids.get_or_insert_with(Vec::new);
                for piece in it.next().map(String::as_str).unwrap_or("").split(',') {
                    if piece.trim().is_empty() {
                        return Err("Invalid (zero-length) process id".to_string());
                    }
                    for word in piece.split_whitespace() {
                        let pid = word
                            .parse::<u32>()
                            .map_err(|_| format!("Invalid process id: {word}"))?;
                        listed.push(pid);
                    }
                }
            }
            _ => {}
        }
    }
    let rows = match pids.filter(|_| !every) {
        Some(pids) => pids.into_iter().filter_map(one).collect(),
        None => all().unwrap_or_default(),
    };
    Ok((fields, rows.into_iter().filter(|r| r.pid > 0).collect()))
}

/// One `ps -o` line for `r`, its `fields` in order, `etime` measured at `now` (unix seconds).
fn ps_line(r: &kproc::Record, fields: &[String], now: i64) -> String {
    let start = i64::try_from(r.start_s).unwrap_or(0);
    fields
        .iter()
        .map(|f| match f.as_str() {
            "pid" => r.pid.to_string(),
            "ppid" => r.ppid.to_string(),
            "pgid" => r.pgid.to_string(),
            "tpgid" => r.tpgid.to_string(),
            "tty" | "tt" => crate::sys::tty_name(r.tty),
            "lstart" => lstart(start),
            "etime" => etime(now - start),
            _ => r.comm.clone(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// `ps`'s `lstart`: `%a %b %e %H:%M:%S %Y` in local time (`TZ` honoured).
#[must_use]
pub fn lstart(secs: i64) -> String {
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let (y, mo, d, h, mi, s) = crate::sys::local_time(secs);
    let mo = usize::try_from(mo).unwrap_or(1).clamp(1, 12);
    format!(
        "{} {} {d:>2} {h:02}:{mi:02}:{s:02} {y}",
        DAYS[weekday(y, mo, d)],
        MONTHS[mo - 1]
    )
}

/// The weekday of a civil date, 0 = Sunday (Sakamoto).
fn weekday(y: i32, m: usize, d: u32) -> usize {
    const T: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let y = if m < 3 { y - 1 } else { y };
    let d = i32::try_from(d).unwrap_or(1);
    let w = (y + y / 4 - y / 100 + y / 400 + T[m - 1] + d).rem_euclid(7);
    usize::try_from(w).unwrap_or(0)
}

/// `ps`'s `etime`: `[[dd-]hh:]mm:ss`.
#[must_use]
pub fn etime(secs: i64) -> String {
    let s = secs.max(0);
    let (d, h, m, s) = (s / 86_400, s / 3600 % 24, s / 60 % 60, s % 60);
    match (d, h) {
        (0, 0) => format!("{m:02}:{s:02}"),
        (0, _) => format!("{h:02}:{m:02}:{s:02}"),
        _ => format!("{d:02}-{h:02}:{m:02}:{s:02}"),
    }
}

/// A do-nothing MCP server (`aterm-lab mcp-standin`).
#[must_use]
pub fn mcp_main(args: &[String]) -> i32 {
    let echo = args.first().map(String::as_str) == Some("echo");
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else {
            return 0;
        };
        let Some(reply) = mcp_reply(&line, echo) else {
            continue;
        };
        if writeln!(out, "{reply}").is_err() || out.flush().is_err() {
            return 0;
        }
    }
    0
}

/// [`mcp_main`]'s answer to one JSON-RPC line: `None` for no request (a
/// notification, or no JSON).
fn mcp_reply(line: &str, echo: bool) -> Option<String> {
    let m = J::parse(line).ok()?;
    let id = m.get("id").cloned()?;
    let result = match m.get("method").and_then(J::as_str) {
        Some("initialize") => J::obj([
            (
                "protocolVersion",
                m.get("params")
                    .and_then(|p| p.get("protocolVersion"))
                    .cloned()
                    .unwrap_or_else(|| J::s("2025-06-18")),
            ),
            ("capabilities", J::obj([("tools", J::Obj(Vec::new()))])),
            (
                "serverInfo",
                J::obj([("name", J::s("live")), ("version", J::s("1"))]),
            ),
        ]),
        Some("tools/list") if echo => J::obj([("tools", J::Arr(vec![echo_tool()]))]),
        Some("tools/list") => J::obj([("tools", J::Arr(Vec::new()))]),
        Some("tools/call") if echo => {
            let text = m
                .get("params")
                .and_then(|p| p.get("arguments"))
                .and_then(|a| a.get("text"))
                .and_then(J::as_str)
                .unwrap_or_default();
            J::obj([(
                "content",
                J::Arr(vec![J::obj([
                    ("type", J::s("text")),
                    ("text", J::s(format!("echo: {text}"))),
                ])]),
            )])
        }
        _ => J::Obj(Vec::new()),
    };
    Some(J::obj([("jsonrpc", J::s("2.0")), ("id", id), ("result", result)]).py())
}

/// The one tool `mcp-standin echo` serves: `lab_echo`, a text in, the text
/// back.
fn echo_tool() -> J {
    J::obj([
        ("name", J::s(MCP_ECHO_TOOL)),
        (
            "description",
            J::s("Echo the text back (the lab's MCP stand-in)."),
        ),
        (
            "inputSchema",
            J::obj([
                ("type", J::s("object")),
                (
                    "properties",
                    J::obj([("text", J::obj([("type", J::s("string"))]))]),
                ),
                ("required", J::Arr(vec![J::s("text")])),
            ]),
        ),
    ])
}

/// The tool `mcp-standin echo` serves.
pub const MCP_ECHO_TOOL: &str = "lab_echo";

#[cfg(test)]
mod tests {
    use super::*;

    /// A `ps -o` line is the kernel record's fields in the order asked, a process with no
    /// terminal at `??` and its elapsed time from its start.
    #[test]
    fn a_ps_line_reads_the_kernel_record() {
        let r = kproc::Record {
            pid: 4242,
            ppid: 1,
            pgid: 4240,
            tpgid: 0,
            tty: None,
            uid: 501,
            comm: "zsh".to_string(),
            start_s: 1_000,
            start_us: 7,
            zombie: false,
        };
        let fields: Vec<String> = ["pid", "ppid", "pgid", "tty", "etime", "comm"]
            .iter()
            .map(|f| (*f).to_string())
            .collect();
        assert_eq!(ps_line(&r, &fields, 1_065), "4242 1 4240 ?? 01:05 zsh");
        assert_eq!(ps_line(&r, &["tpgid".to_string()], 1_065), "0");
    }

    /// `-p` chooses exactly the pids it lists, as `ps` does: pid 0 (`kernel_task`, which the
    /// kernel's table does hold) is never a row, and a list that names no pid is refused rather
    /// than read as no `-p` at all. Without `-p`, or with `-A`, every process is chosen.
    #[test]
    fn the_pid_list_chooses_exactly_its_pids() {
        let record = |pid: u32| kproc::Record {
            pid,
            ppid: 1,
            pgid: 0,
            tpgid: 0,
            tty: None,
            uid: 501,
            comm: format!("p{pid}"),
            start_s: 1,
            start_us: 0,
            zombie: false,
        };
        let every = || Some(vec![record(0), record(1), record(7), record(8)]);
        let chosen = |args: &[&str]| {
            let args: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
            ps_rows(&args, every, |pid| Some(record(pid)))
                .map(|(_, rows)| rows.iter().map(|r| r.pid).collect::<Vec<_>>())
        };
        assert_eq!(chosen(&["-p", "7,8", "-o", "pid="]), Ok(vec![7, 8]));
        assert_eq!(chosen(&["-p", "7  8"]), Ok(vec![7, 8]), "spaces split too");
        assert_eq!(chosen(&["-p", "7", "-p", "8"]), Ok(vec![7, 8]));
        assert_eq!(chosen(&["-p", "0"]), Ok(vec![]), "pid 0 is no row");
        assert_eq!(
            chosen(&["-o", "pid="]),
            Ok(vec![1, 7, 8]),
            "no -p: every process"
        );
        assert_eq!(chosen(&["-A", "-p", "7"]), Ok(vec![1, 7, 8]));
        for refused in ["", "-1", "abc", "7,,8"] {
            assert!(chosen(&["-p", refused]).is_err(), "-p {refused:?}");
        }
        assert!(chosen(&["-p"]).is_err(), "-p with no list");
        let (fields, _) = ps_rows(&["-o".to_string(), "pid=,ppid=".to_string()], every, |_| {
            None
        })
        .expect("fields");
        assert_eq!(fields, ["pid", "ppid"]);
    }

    #[test]
    fn a_request_reads_its_last_user_text_and_a_trailing_tool_output() {
        let body = J::parse(
            r#"{"input":[{"role":"user","content":"first"},{"role":"assistant","content":"x"},
                {"role":"user","content":[{"type":"input_text","text":"a"},{"type":"input_text","text":"b"}]},
                {"type":"function_call_output","output":"done"}]}"#,
        )
        .unwrap();
        let (user, tool) = request_view(&body);
        assert_eq!(user, "b");
        assert_eq!(tool, Some(J::s("done")));
        assert_eq!(
            request_view(&J::parse("{}").unwrap()),
            (String::new(), None)
        );
        assert_eq!(request_marks(&body), (4, "-".to_string()));
        let turned = J::parse(
            r#"{"input":[{"role":"user","content":"x"}],
                "client_metadata":{"turn_id":"t-2","thread_id":"th"}}"#,
        )
        .unwrap();
        assert_eq!(request_marks(&turned), (1, "t-2".to_string()));
        assert_eq!(
            request_marks(&J::parse("{}").unwrap()),
            (0, "-".to_string())
        );
    }

    #[test]
    fn a_reply_s_text_is_its_messages_and_nothing_else() {
        let body = sse(vec![
            message_item("Nothing left: all of it.\nNEEDS YOU: nothing"),
            call_item("exec_command", &J::obj([("cmd", J::s("true"))])),
        ]);
        assert_eq!(
            said_of(&body),
            "Nothing left: all of it.\nNEEDS YOU: nothing"
        );
        let call = sse(vec![call_item("exec_command", &J::obj([]))]);
        assert_eq!(said_of(&call), "");
        assert_eq!(said_of(r#"{"error":{"type":"usage_limit_reached"}}"#), "");
    }

    #[test]
    fn a_stream_carries_each_item_once_and_completes_with_all_of_them() {
        let body = sse(vec![
            message_item("hi"),
            call_item("exec_command", &J::obj([("cmd", J::s("true"))])),
        ]);
        let data: Vec<J> = body
            .lines()
            .filter_map(|l| l.strip_prefix("data: "))
            .filter_map(|d| J::parse(d).ok())
            .collect();
        let kinds: Vec<&str> = data
            .iter()
            .filter_map(|d| d.get("type").and_then(J::as_str))
            .collect();
        assert_eq!(
            kinds,
            [
                "response.created",
                "response.output_item.done",
                "response.output_item.done",
                "response.completed"
            ]
        );
        let output = data[3]
            .get("response")
            .and_then(|r| r.get("output"))
            .and_then(J::as_arr)
            .map_or(0, <[J]>::len);
        assert_eq!(output, 2);
        assert!(jail_policy(4242).contains("localhost:4242"));

        struct Broken {
            left: usize,
            fail_flush: bool,
        }
        impl std::io::Write for Broken {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if self.left == 0 {
                    return Err(std::io::Error::other("injected write failure"));
                }
                let n = self.left.min(bytes.len());
                self.left -= n;
                Ok(n)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                if self.fail_flush {
                    Err(std::io::Error::other("injected flush failure"))
                } else {
                    Ok(())
                }
            }
        }
        let dir = aterm_tempfile::tempdir().expect("exclusive response journal");
        let mut log = ApiLog::new(dir.path().join("api.log"), Instant::now());
        let answer = Answered::now((200, "text/event-stream", body));
        let mut bytes = Vec::new();
        let first = log
            .request(vec![
                ("tag".into(), J::s("same")),
                ("status".into(), J::Int(200)),
            ])
            .unwrap();
        write_response(&mut bytes, &answer, || {
            log.response_written(first, answer.status)
        })
        .unwrap();
        assert!(bytes.ends_with(answer.body.as_bytes()));
        for (left, fail_flush) in [(0, false), (bytes.len() - 1, false), (usize::MAX, true)] {
            let id = log
                .request(vec![
                    ("tag".into(), J::s("same")),
                    ("status".into(), J::Int(200)),
                ])
                .unwrap();
            assert_ne!(id, first, "repeated tags have distinct identities");
            let mut broken = Broken { left, fail_flush };
            assert!(
                write_response(&mut broken, &answer, || log
                    .response_written(id, answer.status))
                .is_err()
            );
        }
        let (requests, witnesses) = log.snapshot().unwrap();
        assert_eq!(requests.len(), 4, "the legacy log remains request-only");
        assert_eq!(witnesses.len(), 1);
        assert_eq!(witnesses[0].get("request_id"), Some(&J::Int(first)));
        assert_eq!(
            witnesses[0].get("event").and_then(J::as_str),
            Some("response_written")
        );
        assert_eq!(witnesses[0].get("status"), Some(&J::Int(200)));
        assert!(
            witnesses[0]
                .get("mono_ms")
                .and_then(J::as_i64)
                .is_some_and(|t| t >= 0)
        );
        std::fs::write(&log.path, "torn request\n").unwrap();
        assert!(
            log.snapshot().is_err(),
            "bad evidence must not vanish from a quiet verdict"
        );
    }

    #[test]
    fn ps_times_read_as_ps_prints_them() {
        assert_eq!(etime(59), "00:59");
        assert_eq!(etime(3 * 3600 + 61), "03:01:01");
        assert_eq!(etime(2 * 86_400 + 5), "02-00:00:05");
        assert_eq!(weekday(2026, 10, 15), 4, "a Thursday");
        assert_eq!(weekday(2000, 1, 1), 6, "a Saturday");
    }

    /// `mcp-standin echo` serves its one tool, `lab_echo`, and answers a call
    /// of it with the text back; plain `mcp-standin` (Codex's) still lists
    /// none and answers a call with `{}`.
    #[test]
    fn the_mcp_standin_echoes_only_when_asked_to() {
        let list = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;
        let call = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"lab_echo","arguments":{"text":"hi"}}}"#;
        let parse = |s: Option<String>| J::parse(&s.expect("an answer")).expect("JSON");
        let tools = parse(mcp_reply(list, true));
        let tool = tools
            .get("result")
            .and_then(|r| r.get("tools"))
            .and_then(|t| match t {
                J::Arr(a) => a.first().cloned(),
                _ => None,
            })
            .expect("one tool");
        assert_eq!(tool.get("name").and_then(J::as_str), Some(MCP_ECHO_TOOL));
        let echoed = parse(mcp_reply(call, true)).py();
        assert!(echoed.contains("echo: hi"), "{echoed}");
        assert!(parse(mcp_reply(list, false)).py().contains("\"tools\": []"));
        let plain = parse(mcp_reply(call, false)).py();
        assert!(!plain.contains("echo:"), "the control: {plain}");
        assert_eq!(
            mcp_reply(
                r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
                true
            ),
            None
        );
        assert_eq!(mcp_reply("not json", true), None);
    }
}
