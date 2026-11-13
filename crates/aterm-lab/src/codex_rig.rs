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

const DENY_IP: &str = "(version 1)(allow default)(deny network-outbound (remote ip))(deny network-inbound (local ip))\
    (deny network-outbound (literal \"/private/var/run/mDNSResponder\"))";

/// An instance's sandbox profile with no inbound or outbound IP exception.
#[must_use]
pub fn no_ip_jail_policy() -> String {
    format!("{DENY_IP}\n{}", crate::jail::PREFERENCES)
}

/// The instance's `sandbox-exec` profile with its model's loopback port allowed.
#[must_use]
pub fn jail_policy(port: u16) -> String {
    format!(
        "{DENY_IP}(allow network-outbound (remote ip \"localhost:{port}\"))\n{}",
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
    crate::util::copy_regular_file(&crate::paths::paths().exe, &lab)?;
    std::fs::hard_link(&lab, bin.join("ps"))?;
    Ok(lab)
}

/// Every process a run started: `root`, each process whose command line names `scratch`, and
/// what descends from them, in one read of the process table ([`crate::procs::commands`]).
#[must_use]
pub fn ours(root: i32, scratch: &Path) -> Vec<i32> {
    let s = scratch.to_string_lossy().into_owned();
    let me = i32::try_from(std::process::id()).unwrap_or(0);
    let rows = crate::procs::commands();
    let mut roots: Vec<i32> = vec![root];
    roots.extend(
        rows.iter()
            .filter(|(p, _, c)| *p != me && c.contains(&s))
            .map(|(p, ..)| *p),
    );
    let table: Vec<(i32, i32)> = rows.iter().map(|(p, pp, _)| (*p, *pp)).collect();
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
    while let Ok((conn, _)) = aterm_uds::accept(
        &listener,
        aterm_uds::AcceptWait::Forever,
        aterm_uds::AcceptedMode::Blocking,
    ) {
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
            let text = match crate::util::read_regular_text(path) {
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
        let mut f = crate::util::append_regular_file(path)?;
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

/// `ps [-A|-e] [-p PIDS] -o FIELD[=],…` from the kernel's process records, each column as
/// `/bin/ps` prints it to a pipe, one space apart where `ps` pads each to its widest value.
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
    let locale = ps_locale(|name| std::env::var(name).ok(), crate::sys::locale_codeset);
    let read = Readers {
        tty: &crate::sys::tty_name,
        comm: &|r: &kproc::Record| in_locale(locale, |l| kproc::ps_comm(r, l)),
        args: &|r: &kproc::Record| in_locale(locale, |l| kproc::ps_args(r, l)),
    };
    let mut out = Vec::new();
    for r in &rows {
        out.extend(ps_line(r, &fields, now, &read));
        out.push(b'\n');
    }
    let _ = std::io::stdout().write_all(&out);
    i32::from(rows.is_empty())
}

/// A `-o` column of the stand-in ([`ps_line`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    Pid,
    Ppid,
    Pgid,
    Tpgid,
    /// The terminal's name less its `tty` or `cua` head (`s003`).
    Tt,
    /// The terminal's whole name (`ttys003`).
    Tty,
    Lstart,
    Etime,
    /// `argv[0]`.
    Comm,
    /// The kernel's short name (`p_comm`).
    Ucomm,
    /// The whole command line (`args`).
    Args,
    /// The whole command line, cut as `comm` is where another column follows (`command`).
    Command,
    /// `Z` for a zombie, as `ps`'s `stat` leads with it (what Codex's daemon reads of the column);
    /// any other state unread, `-`.
    Stat,
    /// A keyword the stand-in has no reader for: `-`, no value, never another column's.
    Unread,
}

impl Field {
    /// The bytes `ps` prints of this column where another column follows it: `comm` and
    /// `command` 16, `args` 64, cut even inside a character. Every other column is whole, and so
    /// is the last.
    fn cut(self) -> Option<usize> {
        match self {
            Field::Comm | Field::Command => Some(16),
            Field::Args => Some(64),
            _ => None,
        }
    }
}

/// Each `ps` keyword the stand-in reads, and its column.
const FIELDS: &[(&str, Field)] = &[
    ("pid", Field::Pid),
    ("ppid", Field::Ppid),
    ("pgid", Field::Pgid),
    ("tpgid", Field::Tpgid),
    ("tt", Field::Tt),
    ("tty", Field::Tty),
    ("lstart", Field::Lstart),
    ("etime", Field::Etime),
    ("comm", Field::Comm),
    ("ucomm", Field::Ucomm),
    ("args", Field::Args),
    ("command", Field::Command),
    ("stat", Field::Stat),
];

/// The `-o` fields `args` ask for and the records they choose, as `ps` chooses them: every
/// process (`all`) with `-A` or `-e` or with no `-p`, else each pid `-p` lists (`one`), pid 0
/// never. An `-o` list holds keywords split by commas and spaces, each with its header after an
/// `=`; a keyword [`FIELDS`] lacks is [`Field::Unread`]. A `-p` list holds pids split the same
/// way; an empty one, or a word that is no pid, is refused.
fn ps_rows(
    args: &[String],
    all: impl FnOnce() -> Option<Vec<kproc::Record>>,
    one: impl FnMut(u32) -> Option<kproc::Record>,
) -> Result<(Vec<Field>, Vec<kproc::Record>), String> {
    let (mut fields, mut pids, mut every) = (Vec::new(), None::<Vec<u32>>, false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-A" | "-e" => every = true,
            "-o" => {
                let list = it.next().map(String::as_str).unwrap_or("");
                for word in list.split(|c: char| c == ',' || c.is_ascii_whitespace()) {
                    let word = word.split_once('=').map_or(word, |(keyword, _)| keyword);
                    if word.is_empty() {
                        continue;
                    }
                    fields.push(
                        FIELDS
                            .iter()
                            .find(|(name, _)| *name == word)
                            .map_or(Field::Unread, |&(_, field)| field),
                    );
                }
            }
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

/// What [`ps_line`] reads past the kernel's record: a terminal's name ([`crate::sys::tty_name`]
/// for a live process), and `comm` and `args` ([`kproc::ps_comm`] and [`kproc::ps_args`] in
/// `ps`'s locale, [`in_locale`]).
struct Readers<'a> {
    tty: &'a dyn Fn(Option<i32>) -> String,
    comm: &'a dyn Fn(&kproc::Record) -> String,
    args: &'a dyn Fn(&kproc::Record) -> String,
}

/// One `ps -o` line for `r`: its `fields` in order, one space apart, `etime` measured at `now`
/// (unix seconds), what the record does not hold read by `read`, and a column another follows cut
/// where `ps` cuts it ([`Field::cut`]).
fn ps_line(r: &kproc::Record, fields: &[Field], now: i64, read: &Readers<'_>) -> Vec<u8> {
    let start = i64::try_from(r.start_s).unwrap_or(0);
    let mut line = Vec::new();
    for (at, f) in fields.iter().enumerate() {
        let value = match f {
            Field::Pid => r.pid.to_string(),
            Field::Ppid => r.ppid.to_string(),
            Field::Pgid => r.pgid.to_string(),
            Field::Tpgid => r.tpgid.to_string(),
            Field::Tt => short_tty(&(read.tty)(r.tty)).to_string(),
            Field::Tty => (read.tty)(r.tty),
            Field::Lstart => lstart(start),
            Field::Etime => etime(now - start),
            Field::Comm => (read.comm)(r),
            Field::Ucomm => r.comm.clone(),
            Field::Args | Field::Command => (read.args)(r),
            Field::Stat if r.zombie => "Z".to_string(),
            Field::Stat | Field::Unread => "-".to_string(),
        };
        let mut value = value.into_bytes();
        if let Some(cut) = f.cut().filter(|_| at + 1 < fields.len()) {
            value.truncate(cut);
        }
        if at > 0 {
            line.push(b' ');
        }
        line.extend(value);
    }
    line
}

/// `ps`'s `tt` of the terminal named `name`: the name less a `tty` or `cua` head.
fn short_tty(name: &str) -> &str {
    name.strip_prefix("tty")
        .or_else(|| name.strip_prefix("cua"))
        .unwrap_or(name)
}

/// The locale categories `ps`'s `setlocale(LC_ALL, "")` sets, by the variables that name them.
const LOCALE_CATEGORIES: [&str; 6] = [
    "LC_COLLATE",
    "LC_CTYPE",
    "LC_MESSAGES",
    "LC_MONETARY",
    "LC_NUMERIC",
    "LC_TIME",
];

/// The rendering `ps` prints `comm` and `args` in: the locale its `setlocale(LC_ALL, "")` sets
/// from the environment read through `var`. Each category is the first of `LC_ALL`, its own
/// variable and `LANG` set to a non-empty value, else `C`; a name `codeset` cannot load for its
/// category (it answers the loaded locale's codeset, or `None`) fails the whole call, which leaves
/// `ps` in C. Otherwise `LC_CTYPE`'s codeset decides: `UTF-8` is the UTF-8 rendering, `US-ASCII`
/// (C's and POSIX's) the C one, and any other (`ISO8859-1`, `SJIS`) neither (`None`,
/// [`in_locale`]).
fn ps_locale(
    var: impl Fn(&str) -> Option<String>,
    codeset: impl Fn(&str, &str) -> Option<String>,
) -> Option<kproc::Locale> {
    let mut ctype = String::new();
    for category in LOCALE_CATEGORIES {
        let name = ["LC_ALL", category, "LANG"]
            .into_iter()
            .find_map(|name| var(name).filter(|value| !value.is_empty()))
            .unwrap_or_else(|| "C".to_string());
        let Some(set) = codeset(category, &name) else {
            return Some(kproc::Locale::C);
        };
        if category == "LC_CTYPE" {
            ctype = set;
        }
    }
    match ctype.as_str() {
        "UTF-8" => Some(kproc::Locale::Utf8),
        "US-ASCII" => Some(kproc::Locale::C),
        _ => None,
    }
}

/// `read` (a `comm` or `args` reader) in `ps`'s rendering `locale` ([`ps_locale`]). In a codeset
/// with no rendering here (`None`), the value both renderings agree on, which is ASCII text that
/// every codeset prints alike, else `-`.
fn in_locale(locale: Option<kproc::Locale>, read: impl Fn(kproc::Locale) -> String) -> String {
    match locale {
        Some(locale) => read(locale),
        None => {
            let c = read(kproc::Locale::C);
            if c == read(kproc::Locale::Utf8) {
                c
            } else {
                "-".to_string()
            }
        }
    }
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
#[path = "codex_rig_tests.rs"]
mod tests;
