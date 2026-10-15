// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm conn`: the human CLI for session connections
//! (docs/design/SESSION_CONNECTIONS.md §6.1). Shows and manages which sessions
//! pull from or push to each other, from inside any aterm session or against
//! the latest instance from outside one.
//!
//! A presentation layer over the §6 wire verbs (`connect`, `disconnect`,
//! `flows`, `raise`, `spawn connected=`, `open connections`): each subverb
//! frames one request per wire act through the crate's shared plumbing (socket
//! resolution, token handshake, bounded reads, reply framing), so `aterm conn
//! add` and `aterm ctl connect dst=… src=…` are the same act byte for byte.
//!
//! Peers are selectors (`@self`, `@<sid>`, `@<local-id>`), never titles.
//! Outside an aterm session the `@self` forms refuse with an error naming
//! `$ATERM_PARENT_SESSION_ID`; everything else targets the latest instance, as
//! `aterm ctl` does.

use std::env;
use std::io::{self, BufReader, Write};
use std::process::ExitCode;

/// The conn subverbs completion offers after `aterm conn`, shared with
/// [`crate::fish_completion`].
pub(crate) const CONN_SUBVERBS: &str = "ls add set rm spawn show map help";

/// The `conn help` page. `conn` manages the standing wiring and hands the
/// reader the `aterm ctl @<sid> turn …` drive line; the pull/push verbs stay on
/// `aterm ctl` (§6.1).
const CONN_USAGE: &str = "\
aterm conn — session connections: standing pull/push wiring between sessions.

A connection lets one session drive another as a human could, at minimum:
push = keystrokes on the peer's PTY (plus the ^C signal), pull = read what a
human would see (the rendered screen). Kinds: pull | push | both (default).

USAGE:
    aterm conn [--sock PATH | --pid PID] [<subverb> ...]

SUBVERBS:
    (none)              THIS session's connections, one line each:
                        \u{21e5} outgoing (this session drives the peer),
                        \u{21e4} incoming (the peer drives this session),
                        \u{21c6} both ways — then kind, peer sid, peer title
    ls [--json]         every session connection in the instance
    add <sel> [--kind pull|push|both] [--to-me | --from <sel>]
                        connect. Default @self -> <sel> (\"I take control of
                        it\"); --to-me inverts (<sel> -> @self: invite a
                        controller); --from <sel2> wires the third-party pair
                        <sel2> -> <sel>
    set <sel> --kind pull|push|both [--to-me | --from <sel>]
                        make the connection exactly --kind; a half not named
                        is removed
    rm <sel> [--kind pull|push|both] [--to-me | --from <sel>]
                        disconnect (kind-filtered: --kind pull removes only
                        the pull half)
    spawn controlled|controller [--tab|--window] [--of <sel>]
                        spawn a new session wired both ways: controlled = @of
                        drives it; controller = it drives @of (its shell gets
                        ATERM_OBSERVE_SESSION_ID). --window by default; --of
                        is @self inside aterm, required outside
    show <sel>          raise the peer's window and select its tab
    map                 open the GUI connection map
    help                this help

SELECTORS:
    @self               this session ($ATERM_PARENT_SESSION_ID)
    @<sid>              a stable session id (discover with `aterm ctl ls`)
    @<local-id>         the <local> column of `aterm ctl ls` (ids are per instance)
    Titles are NOT selectors (ambiguous). Outside an aterm session the @self
    forms refuse with an error naming $ATERM_PARENT_SESSION_ID; everything
    else targets the latest instance, exactly like `aterm ctl`.

DRIVING A CONNECTED PEER (the connection is standing wiring; driving is ctl):
    aterm ctl @<sid> turn 'message'     one verified human-grade interaction
    aterm ctl @<sid> text               read what the peer shows

`aterm conn` is the same act as `aterm ctl connect/disconnect/flows/raise/spawn
connected=/open connections`.
";

/// Per-op socket deadline for the conn verbs. Every §6 verb answers
/// synchronously (none blocks like `await`), so a short bound keeps a wedged
/// instance from stalling the CLI, with margin for a loaded machine's window
/// spawn.
const CONN_DEADLINE: std::time::Duration = std::time::Duration::from_secs(60);

/// The `aterm conn` verb: `argv` (past the `conn` token) in, exit code out.
/// The `aterm` binary dispatches here beside `ctl`/`pkg`/`fleet`/`drive`.
pub fn conn_main_entry(argv: Vec<std::ffi::OsString>) -> ExitCode {
    match conn_real_main(argv) {
        Ok(code) => code,
        // `aterm conn ls | head`: the reader has what it wanted.
        Err(e) if super::is_stdout_closed(&e) => ExitCode::SUCCESS,
        Err(e) => {
            let _ = conn_stderr_line(&e.to_string());
            ExitCode::FAILURE
        }
    }
}

/// Write `"aterm conn: <msg>\n"` to stderr — the conn-prefixed twin of the
/// crate's [`super::stderr_line`], so a failure names the verb the user typed,
/// rendered for a terminal as it is ([`super::stderr_prefixed`]).
fn conn_stderr_line(msg: &str) -> io::Result<()> {
    super::stderr_prefixed("aterm conn: ", msg)
}

/// A usage error: `InvalidInput` with a hand-shaped message (surfaced by
/// [`conn_main_entry`] with the `aterm conn:` prefix, exit FAILURE).
fn usage(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg.to_string())
}

/// Parse flags, resolve the socket, and dispatch the subverb.
fn conn_real_main(argv: Vec<std::ffi::OsString>) -> io::Result<ExitCode> {
    let mut args = super::utf8_args(argv)?.into_iter();
    let mut sock: Option<String> = None;
    let mut pid: Option<u32> = None;
    let mut rest: Vec<String> = Vec::new();
    while let Some(arg) = args.next() {
        if arg == "-h" || arg == "--help" {
            return print_usage();
        } else if arg == "--sock" {
            sock = Some(args.next().ok_or_else(|| usage("--sock requires a PATH"))?);
        } else if let Some(p) = arg.strip_prefix("--sock=") {
            sock = Some(p.to_string());
        } else if arg == "--pid" {
            let v = args.next().ok_or_else(|| usage("--pid requires a PID"))?;
            pid = Some(super::parse_pid(&v)?);
        } else if let Some(v) = arg.strip_prefix("--pid=") {
            pid = Some(super::parse_pid(v)?);
        } else {
            // First positional is the subverb; the remainder is its argument list.
            rest.push(arg);
            rest.extend(args.by_ref());
            break;
        }
    }
    // `help` needs no socket — answer before resolution so it works with no
    // instance running at all (the discoverability path).
    if rest.first().map(String::as_str) == Some("help") {
        return print_usage();
    }
    let self_sid = env::var(super::SELF_SID_ENV).ok().filter(|s| !s.is_empty());
    let (path, origin) = super::resolve_target(sock, pid, self_sid.clone())?;
    let wire = ConnWire { path, origin };
    match rest.first().map(String::as_str) {
        None => cmd_status(&wire, self_sid.as_deref()),
        Some("ls") => cmd_ls(&wire, &rest[1..]),
        Some("add") => cmd_add_set(&wire, self_sid.as_deref(), "add", &rest[1..]),
        Some("set") => cmd_add_set(&wire, self_sid.as_deref(), "set", &rest[1..]),
        Some("rm") => cmd_rm(&wire, self_sid.as_deref(), &rest[1..]),
        Some("spawn") => cmd_spawn(&wire, self_sid.as_deref(), &rest[1..]),
        Some("show") => cmd_show(&wire, self_sid.as_deref(), &rest[1..]),
        Some("map") => cmd_map(&wire),
        Some(other) => Err(usage(&format!(
            "unknown subverb '{other}' (run `aterm conn help`)"
        ))),
    }
}

/// Print [`CONN_USAGE`] and exit SUCCESS.
fn print_usage() -> io::Result<ExitCode> {
    let stdout = io::stdout();
    let mut out = super::StdoutSink::new(stdout.lock());
    out.write_all(CONN_USAGE.as_bytes())?;
    Ok(ExitCode::SUCCESS)
}

// --- the wire ---------------------------------------------------------------

/// One resolved control-socket target the subverbs frame their requests on.
struct ConnWire {
    path: String,
    /// How `path` was chosen — `--sock`/`--pid` pin it — which decides what a
    /// connect failure on it may offer instead (`super::connect_error`).
    origin: super::TargetOrigin,
}

/// One reply: the trimmed status line, plus the body rows when the verb is
/// Lines-framed per the shared `control_verbs` table.
struct Reply {
    status: String,
    lines: Vec<String>,
}

impl Reply {
    fn is_ok(&self) -> bool {
        self.status == "OK" || self.status.starts_with("OK ")
    }

    /// Everything after `OK ` (empty for a bare `OK`).
    fn tail(&self) -> &str {
        self.status.strip_prefix("OK ").unwrap_or("")
    }
}

/// Map an `ERR …` reply to an error carrying the server's own line (surfaced
/// as `aterm conn: ERR …`), so subverbs handle only the OK shape.
fn require_ok(reply: Reply) -> io::Result<Reply> {
    if reply.is_ok() {
        Ok(reply)
    } else {
        Err(io::Error::other(reply.status))
    }
}

impl ConnWire {
    /// One framed request and reply: authenticate, send the joined parts as
    /// one line, read the status and, when the framing table says the verb
    /// streams `OK <n>` + n lines, the body rows. Uses [`super::exchange`]'s
    /// bounded-read plumbing but returns the reply for rendering. A
    /// [`CONN_DEADLINE`] that fires after connecting reports as
    /// [`super::converse`] does (`no reply from <sock> within 60s …` or `reply
    /// from <sock> stopped part-way: …`).
    fn request(&self, parts: &[String]) -> io::Result<Reply> {
        super::validate_request_parts(parts)?;
        let mut request = parts.join(" ");
        request.push('\n');
        let path = aterm_uds::latest::resolve(&self.path);
        super::converse(&path, self.origin, Some(CONN_DEADLINE), |stream, token| {
            stream.set_read_timeout(Some(CONN_DEADLINE))?;
            stream.set_write_timeout(Some(CONN_DEADLINE))?;
            super::send_request(&stream, token, &request)?;
            let mut reader = BufReader::new(&stream);
            let mut status = String::new();
            if super::read_status_line(&mut reader, &mut status)? == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "server closed the connection without responding",
                ));
            }
            let status = status.trim_end_matches(['\r', '\n']).to_string();
            let verb = super::forwarded_verb(parts).unwrap_or_default();
            let mut lines = Vec::new();
            if aterm_types::control_verbs::framing_of(&verb, &request)
                == aterm_types::control_verbs::Framing::Lines
                && let Some(tail) = status.strip_prefix("OK ")
            {
                let count = super::stream_count(tail)
                    .ok_or_else(|| super::malformed_header_error(&status))?;
                for _ in 0..count {
                    let mut line = String::new();
                    if super::read_bounded_line(&mut reader, &mut line)? == 0 {
                        return Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "server hung up before the complete response",
                        ));
                    }
                    lines.push(line.trim_end_matches(['\r', '\n']).to_string());
                }
            }
            Ok(Reply { status, lines })
        })
    }

    /// The instance's `sessions` listing, parsed — the sid⇄local-id⇄title
    /// index the selectors and renders resolve against.
    fn sessions(&self) -> io::Result<Vec<SessionRow>> {
        let reply = require_ok(self.request(&["sessions".to_string()])?)?;
        Ok(parse_sessions_lines(&reply.lines))
    }
}

/// A lazily-fetched [`ConnWire::sessions`] index: fetched at most once, and
/// only when a subverb actually needs it (an `@<local-id>` selector, or a
/// title render).
struct LazySessions<'a> {
    wire: &'a ConnWire,
    rows: Option<Vec<SessionRow>>,
}

impl<'a> LazySessions<'a> {
    fn new(wire: &'a ConnWire) -> Self {
        Self { wire, rows: None }
    }

    fn get(&mut self) -> io::Result<&[SessionRow]> {
        if self.rows.is_none() {
            self.rows = Some(self.wire.sessions()?);
        }
        Ok(self.rows.as_deref().unwrap_or_default())
    }
}

// --- selectors --------------------------------------------------------------

/// The classified shape of one conn selector token.
#[derive(Debug, PartialEq, Eq)]
enum SelTok<'a> {
    /// `@self` / `@env` — this session, from `$ATERM_PARENT_SESSION_ID`.
    SelfSid,
    /// `@<sid>` — a stable session id, used verbatim.
    Sid(&'a str),
    /// `@<local-id>` — resolved to a sid through the `sessions` index.
    Local(u64),
}

/// Classify a selector token, refusing all but the three specific-session
/// forms. A bare word is refused as a title (§6.1), and `@.` is refused
/// because an authority act must not default to whichever tab is active.
fn classify_selector(tok: &str) -> io::Result<SelTok<'_>> {
    let Some(body) = tok.strip_prefix('@') else {
        if tok.starts_with("s-") && tok.len() > 2 {
            return Err(usage(&format!("a session id needs an @: @{tok}")));
        }
        return Err(usage(&format!(
            "'{tok}' is not a selector — titles are ambiguous; use @self, @<sid>, or @<local-id>"
        )));
    };
    match body {
        "self" | "env" => Ok(SelTok::SelfSid),
        "." => Err(usage(
            "@. (the active tab) retargets on every tab switch — name the session: @self, @<sid>, or @<local-id>",
        )),
        b if b.starts_with("s-") && b.len() > 2 => Ok(SelTok::Sid(b)),
        b => match b.parse::<u64>() {
            Ok(n) => Ok(SelTok::Local(n)),
            Err(_) => Err(usage(&format!(
                "'{tok}' is not a selector; use @self, @<sid>, or @<local-id>"
            ))),
        },
    }
}

/// Resolve a selector token to a sid. `lookup` maps a local id to a sid
/// ([`LazySessions`] on the wire, a fixture in tests), so the refusals are
/// testable without a socket.
fn resolve_selector_with(
    tok: &str,
    self_sid: Option<&str>,
    lookup: &mut dyn FnMut(u64) -> io::Result<Option<String>>,
) -> io::Result<String> {
    match classify_selector(tok)? {
        SelTok::SelfSid => self_sid
            .map(str::to_string)
            .ok_or_else(super::self_selector_error),
        SelTok::Sid(sid) => Ok(sid.to_string()),
        SelTok::Local(n) => lookup(n)?.ok_or_else(|| {
            usage(&format!(
                "no session with local id {n} (see `aterm ctl ls`)"
            ))
        }),
    }
}

/// [`resolve_selector_with`] against the live `sessions` index.
fn resolve_selector(
    tok: &str,
    self_sid: Option<&str>,
    sessions: &mut LazySessions<'_>,
) -> io::Result<String> {
    resolve_selector_with(tok, self_sid, &mut |n| {
        Ok(sessions
            .get()?
            .iter()
            .find(|r| r.local == n)
            .map(|r| r.sid.clone()))
    })
}

// --- argument parsing (pure, unit-tested) -----------------------------------

/// The parsed shape of an `add`/`set`/`rm` invocation.
#[derive(Debug, PartialEq, Eq)]
struct PairSpec {
    sel: String,
    kind: Option<String>,
    to_me: bool,
    from: Option<String>,
}

/// Parse `add`/`set`/`rm` arguments: one positional selector plus the
/// direction/kind flags. `--to-me` and `--from` are mutually exclusive.
fn parse_pair_args(sub: &str, args: &[String]) -> io::Result<PairSpec> {
    let mut sel: Option<String> = None;
    let mut kind: Option<String> = None;
    let mut to_me = false;
    let mut from: Option<String> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--kind" => {
                let v = it
                    .next()
                    .ok_or_else(|| usage("--kind requires pull, push, or both"))?;
                kind = Some(parse_kind(v)?);
            }
            "--to-me" => to_me = true,
            "--from" => {
                let v = it
                    .next()
                    .ok_or_else(|| usage("--from requires a selector"))?;
                from = Some(v.clone());
            }
            k => {
                if let Some(v) = k.strip_prefix("--kind=") {
                    kind = Some(parse_kind(v)?);
                } else if let Some(v) = k.strip_prefix("--from=") {
                    from = Some(v.to_string());
                } else if k.starts_with("--") {
                    return Err(usage(&format!("unknown flag '{k}' for `conn {sub}`")));
                } else if sel.is_some() {
                    return Err(usage(&format!(
                        "conn {sub} takes exactly one peer selector"
                    )));
                } else {
                    sel = Some(k.to_string());
                }
            }
        }
    }
    let sel = sel.ok_or_else(|| {
        usage(&format!(
            "conn {sub} needs a peer selector (@self, @<sid>, or @<local-id>)"
        ))
    })?;
    if to_me && from.is_some() {
        return Err(usage("--to-me and --from are mutually exclusive"));
    }
    Ok(PairSpec {
        sel,
        kind,
        to_me,
        from,
    })
}

/// Validate a `--kind` value against the closed vocabulary the wire accepts.
fn parse_kind(v: &str) -> io::Result<String> {
    match v {
        "pull" | "push" | "both" => Ok(v.to_string()),
        _ => Err(usage("kind must be pull, push, or both")),
    }
}

/// The §6.1 direction rules, as `(dst, src)` selector tokens for the wire's
/// `connect dst=<sid> src=<sid>` (src drives dst; rows land in dst's edge
/// table):
///
/// * default: `@self -> <sel>`, take control of it (src=self, dst=sel);
/// * `--to-me`: `<sel> -> @self`, invite a controller (src=sel, dst=self);
/// * `--from <f>`: `<f> -> <sel>`, wire any third-party pair.
fn direction_pair(sel: &str, to_me: bool, from: Option<&str>) -> (String, String) {
    if to_me {
        ("@self".to_string(), sel.to_string())
    } else if let Some(f) = from {
        (sel.to_string(), f.to_string())
    } else {
        (sel.to_string(), "@self".to_string())
    }
}

/// The parsed shape of a `spawn` invocation.
#[derive(Debug, PartialEq, Eq)]
struct SpawnSpec {
    role: String,
    place: &'static str,
    of: Option<String>,
}

/// Parse `spawn controlled|controller [--tab|--window] [--of <sel>]`.
/// `--window` is the default; the role is mandatory because it decides which
/// endpoint holds the minted authority.
fn parse_spawn_args(args: &[String]) -> io::Result<SpawnSpec> {
    let mut role: Option<String> = None;
    let mut place: &'static str = "window";
    let mut of: Option<String> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "controlled" | "controller" if role.is_none() => role = Some(a.clone()),
            "--tab" => place = "tab",
            "--window" => place = "window",
            "--of" => {
                let v = it.next().ok_or_else(|| usage("--of requires a selector"))?;
                of = Some(v.clone());
            }
            k => {
                if let Some(v) = k.strip_prefix("--of=") {
                    of = Some(v.to_string());
                } else {
                    return Err(usage(&format!(
                        "conn spawn takes controlled|controller [--tab|--window] [--of <sel>], got '{k}'"
                    )));
                }
            }
        }
    }
    let role = role.ok_or_else(|| usage("conn spawn needs a role: controlled or controller"))?;
    Ok(SpawnSpec { role, place, of })
}

// --- wire-reply parsing (pure, unit-tested) ---------------------------------

/// One `sessions` row this module cares about.
#[derive(Debug, PartialEq, Eq)]
struct SessionRow {
    local: u64,
    sid: String,
    title: String,
}

/// Parse `sessions` body rows (`<local> <sid> <parent|-> <state> <title>
/// [meta=…]`, title pct-encoded). Malformed rows are skipped; an empty title
/// (whose token vanishes under whitespace splitting) reads as `""`.
fn parse_sessions_lines(lines: &[String]) -> Vec<SessionRow> {
    let mut out = Vec::new();
    for line in lines {
        let mut toks = line.split_whitespace();
        let (Some(local), Some(sid), Some(_parent), Some(_state)) =
            (toks.next(), toks.next(), toks.next(), toks.next())
        else {
            continue;
        };
        let Ok(local) = local.parse::<u64>() else {
            continue;
        };
        let title = match toks.next() {
            None => String::new(),
            Some(t) if t.starts_with("meta=") => String::new(),
            Some(t) => aterm_codec::pct::decode(t),
        };
        out.push(SessionRow {
            local,
            sid: sid.to_string(),
            title,
        });
    }
    out
}

/// One grouped `flows --json` pair: every op the source holds over the
/// destination.
#[derive(Debug, PartialEq, Eq)]
struct FlowPair {
    src: String,
    dst: String,
    ops: Vec<String>,
}

/// The next `"<key>":"<value>"` member's value in `s`, plus the remainder.
/// The values extracted (sids and `Op` tokens) are ASCII that `json_escape`
/// never rewrites, so a plain quote scan is exact.
fn str_member<'a>(s: &'a str, key: &str) -> Option<(String, &'a str)> {
    let i = s.find(key)?;
    let r = &s[i + key.len()..];
    let e = r.find('"')?;
    Some((r[..e].to_string(), &r[e + 1..]))
}

/// Parse a `flows --json` body by an anchored key scan over `cmd_flows`' fixed
/// emit order (`{"src":..,"dst":..,"ops":[..]}` per pair), keeping the crate
/// free of a JSON dependency.
fn parse_flows_json(body: &str) -> Vec<FlowPair> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some((src, r)) = str_member(rest, "{\"src\":\"") {
        let Some((dst, r)) = str_member(r, "\"dst\":\"") else {
            break;
        };
        let Some(i) = r.find("\"ops\":[") else {
            break;
        };
        let r = &r[i + 7..];
        let Some(end) = r.find(']') else {
            break;
        };
        let ops = r[..end]
            .split(',')
            .filter_map(|t| {
                let t = t.trim().trim_matches('"');
                (!t.is_empty()).then(|| t.to_string())
            })
            .collect();
        out.push(FlowPair { src, dst, ops });
        rest = &r[end..];
    }
    out
}

/// Parse an `edges --json` body (`{"edges":[{"src":..,"dst":..,"op":..},…],
/// "dst":"<self>"}`) into `(src, dst, op)` rows. The object anchor keeps the
/// trailing self-`dst` member from reading as a row.
fn parse_edges_json(body: &str) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some((src, r)) = str_member(rest, "{\"src\":\"") {
        let Some((dst, r)) = str_member(r, "\"dst\":\"") else {
            break;
        };
        let Some((op, r)) = str_member(r, "\"op\":\"") else {
            break;
        };
        out.push((src, dst, op));
        rest = r;
    }
    out
}

// --- rendering (pure, unit-tested) ------------------------------------------

/// Classify an op set as a kind: `read-screen` is pull, `write-input`/`signal`
/// push. `other` covers a raw-granted row outside the connection vocabulary
/// (e.g. a bare `derive-loop`).
fn kind_of_ops(ops: &[String]) -> &'static str {
    let pull = ops.iter().any(|o| o == "read-screen");
    let push = ops.iter().any(|o| o == "write-input" || o == "signal");
    match (pull, push) {
        (true, true) => "both",
        (true, false) => "pull",
        (false, true) => "push",
        (false, false) => "other",
    }
}

/// The quoted title for `sid`, or `-` for a sid this instance does not know.
fn title_of(index: &[SessionRow], sid: &str) -> String {
    index
        .iter()
        .find(|r| r.sid == sid)
        .map(|r| format!("\"{}\"", r.title))
        .unwrap_or_else(|| "-".to_string())
}

/// Render the no-arg status rows: one line per connection, peers sorted by
/// sid. A peer wired both ways with the same kind collapses to one `⇆` line;
/// otherwise two lines (`⇥` outgoing before `⇤` incoming).
fn render_self_rows(
    inbound: &[(String, Vec<String>)],
    outbound: &[(String, Vec<String>)],
    index: &[SessionRow],
) -> Vec<String> {
    use std::collections::BTreeMap;
    let mut peers: BTreeMap<&str, (Option<&'static str>, Option<&'static str>)> = BTreeMap::new();
    for (dst, ops) in outbound {
        peers.entry(dst).or_default().0 = Some(kind_of_ops(ops));
    }
    for (src, ops) in inbound {
        peers.entry(src).or_default().1 = Some(kind_of_ops(ops));
    }
    let mut rows = Vec::new();
    for (peer, (out_kind, in_kind)) in peers {
        let title = title_of(index, peer);
        match (out_kind, in_kind) {
            (Some(o), Some(i)) if o == i => {
                rows.push(format!("\u{21c6} {o:<5} @{peer} {title}"));
            }
            (out_kind, in_kind) => {
                if let Some(o) = out_kind {
                    rows.push(format!("\u{21e5} {o:<5} @{peer} {title}"));
                }
                if let Some(i) = in_kind {
                    rows.push(format!("\u{21e4} {i:<5} @{peer} {title}"));
                }
            }
        }
    }
    rows
}

/// Render the `ls` rows: one line per directed pair, in the wire's
/// `(src, dst)` order, titles resolved where this instance knows the sid.
fn render_ls_rows(pairs: &[FlowPair], index: &[SessionRow]) -> Vec<String> {
    pairs
        .iter()
        .map(|p| {
            format!(
                "@{} -> @{}  {}  {} -> {}",
                p.src,
                p.dst,
                kind_of_ops(&p.ops),
                title_of(index, &p.src),
                title_of(index, &p.dst),
            )
        })
        .collect()
}

// --- subverbs ---------------------------------------------------------------

/// The no-arg form: this session's incoming and outgoing connections. Inbound
/// from its own edge table (`@self edges --json`), outbound from `flows
/// --json` filtered to this session as source; titles from `sessions`.
fn cmd_status(wire: &ConnWire, self_sid: Option<&str>) -> io::Result<ExitCode> {
    let sid = self_sid.ok_or_else(|| {
        usage("not inside an aterm session ($ATERM_PARENT_SESSION_ID unset); every connection: aterm conn ls")
    })?;
    let edges = require_ok(wire.request(&[
        format!("@{sid}"),
        "edges".to_string(),
        "--json".to_string(),
    ])?)?;
    let flows = require_ok(wire.request(&["flows".to_string(), "--json".to_string()])?)?;
    let index = wire.sessions()?;

    // Group the inbound per-op rows by source; the flows pairs arrive grouped.
    let mut inbound: Vec<(String, Vec<String>)> = Vec::new();
    for (src, _dst, op) in parse_edges_json(edges.lines.first().map(String::as_str).unwrap_or("")) {
        match inbound.iter_mut().find(|(s, _)| *s == src) {
            Some((_, ops)) => ops.push(op),
            None => inbound.push((src, vec![op])),
        }
    }
    let outbound: Vec<(String, Vec<String>)> =
        parse_flows_json(flows.lines.first().map(String::as_str).unwrap_or(""))
            .into_iter()
            .filter(|p| p.src == sid)
            .map(|p| (p.dst, p.ops))
            .collect();

    let rows = render_self_rows(&inbound, &outbound, &index);
    if rows.is_empty() {
        super::print_stdout_line(&format!("no session connections for @{sid}"))?;
        super::print_stdout_line("create one:")?;
        super::print_stdout_line(
            "  aterm conn add @<sid>        take control of a session (pull+push)",
        )?;
        super::print_stdout_line(
            "  aterm conn spawn controlled  spawn a new session this one controls",
        )?;
        return Ok(ExitCode::SUCCESS);
    }
    for row in &rows {
        super::print_stdout_line(row)?;
    }
    // The ready-to-paste drive hint (§6.1: introspection referenced, not
    // duplicated) — for the first peer this session can push into.
    if let Some((peer, _)) = outbound
        .iter()
        .find(|(_, ops)| matches!(kind_of_ops(ops), "push" | "both"))
    {
        super::print_stdout_line("")?;
        super::print_stdout_line(&format!("drive it: aterm ctl @{peer} turn 'your message'"))?;
    }
    Ok(ExitCode::SUCCESS)
}

/// `conn ls [--json]`: the instance's whole connection graph (`flows`).
/// `--json` passes the wire object through; the default prints one line per
/// directed pair with titles.
fn cmd_ls(wire: &ConnWire, args: &[String]) -> io::Result<ExitCode> {
    let mut json = false;
    for a in args {
        match a.as_str() {
            "--json" | "json" => json = true,
            other => return Err(usage(&format!("conn ls takes only --json, got '{other}'"))),
        }
    }
    let reply = require_ok(wire.request(&["flows".to_string(), "--json".to_string()])?)?;
    let body = reply.lines.first().map(String::as_str).unwrap_or("");
    if json {
        super::print_stdout_line(body)?;
        return Ok(ExitCode::SUCCESS);
    }
    let pairs = parse_flows_json(body);
    if pairs.is_empty() {
        // The zero-result stderr convention (see `exchange`): stdout stays
        // clean for parsers, a human still sees "worked, nothing there".
        conn_stderr_line("no session connections")?;
        return Ok(ExitCode::SUCCESS);
    }
    let index = wire.sessions()?;
    for row in render_ls_rows(&pairs, &index) {
        super::print_stdout_line(&row)?;
    }
    Ok(ExitCode::SUCCESS)
}

/// The refusal for the default direction (`@self -> <sel>`) outside an aterm
/// session: the driving side was implied, so name the flag that supplies it.
const NO_IMPLIED_DRIVER: &str = "not inside an aterm session ($ATERM_PARENT_SESSION_ID unset); name the driving session with --from <sel>";

/// Resolve an `add`/`set`/`rm` spec to `(dst, src)` sids. The default
/// direction's `@self` is implied, not typed, so outside a session it refuses
/// with [`NO_IMPLIED_DRIVER`] rather than an `@self` error.
fn resolve_pair(
    spec: &PairSpec,
    self_sid: Option<&str>,
    sessions: &mut LazySessions<'_>,
) -> io::Result<(String, String)> {
    let (dst_tok, src_tok) = direction_pair(&spec.sel, spec.to_me, spec.from.as_deref());
    let dst = resolve_selector(&dst_tok, self_sid, sessions)?;
    if !spec.to_me && spec.from.is_none() && self_sid.is_none() {
        return Err(usage(NO_IMPLIED_DRIVER));
    }
    let src = resolve_selector(&src_tok, self_sid, sessions)?;
    Ok((dst, src))
}

/// The server's refusals, restated with the resolved ends they concern
/// (`src` is `None` for a single-session verb). `None` for any other line,
/// which passes through as the server wrote it.
fn refusal(status: &str, src: Option<&str>, dst: &str) -> Option<String> {
    Some(match (status, src) {
        ("ERR no such session", _) => {
            format!("no session @{dst} in this instance (see `aterm ctl ls`)")
        }
        ("ERR exited", _) => format!("@{dst} has exited"),
        ("ERR self-loop", _) => format!("@{dst} cannot connect to itself"),
        ("ERR no such connection", Some(src)) => format!("no connection @{src} -> @{dst}"),
        ("ERR denied", _) => "denied: aterm conn needs owner access to this instance".to_string(),
        _ => return None,
    })
}

/// [`require_ok`], with a known refusal restated by [`refusal`].
fn require_ok_for(reply: Reply, src: Option<&str>, dst: &str) -> io::Result<Reply> {
    if reply.is_ok() {
        return Ok(reply);
    }
    Err(io::Error::other(
        refusal(&reply.status, src, dst).unwrap_or(reply.status),
    ))
}

/// What `conn rm` did, from the `--kind` asked and the revoked-row count.
/// `--kind both` is the whole connection, the same as no `--kind`.
fn rm_outcome(src: &str, dst: &str, kind: Option<&str>, revoked: usize) -> String {
    match kind {
        None | Some("both") => format!("disconnected @{src} -> @{dst}"),
        Some(k) if revoked > 0 => format!("removed {k} from @{src} -> @{dst}"),
        Some(k) => no_live_half(src, dst, k),
    }
}

/// A kind-filtered `conn rm` that found no live row of that kind. The count
/// says nothing about the recorded pair, which may still have changed shape.
fn no_live_half(src: &str, dst: &str, kind: &str) -> String {
    format!("@{src} -> @{dst}: no live {kind} half to remove")
}

/// What `conn spawn` did: where the newborn lives and which way it is wired.
fn spawn_outcome(new: &str, role: &str, place: &str, of: &str) -> String {
    if role == "controller" {
        format!("spawned @{new} in a new {place}; it drives @{of} (both)")
    } else {
        format!("spawned @{new} in a new {place}; @{of} drives it (both)")
    }
}

/// `conn add` / `conn set`: both are the declarative wire `connect`; `set`
/// requires `--kind`.
fn cmd_add_set(
    wire: &ConnWire,
    self_sid: Option<&str>,
    sub: &str,
    args: &[String],
) -> io::Result<ExitCode> {
    let spec = parse_pair_args(sub, args)?;
    if sub == "set" && spec.kind.is_none() {
        return Err(usage("conn set requires --kind pull|push|both"));
    }
    let mut sessions = LazySessions::new(wire);
    let (dst, src) = resolve_pair(&spec, self_sid, &mut sessions)?;
    let kind = spec.kind.unwrap_or_else(|| "both".to_string());
    // The wire replies with the minted capability hexes; the human CLI keeps
    // bearer tokens off the terminal (the record store holds them, and a
    // driver needing raw tokens uses `aterm ctl connect`).
    require_ok_for(
        wire.request(&[
            "connect".to_string(),
            format!("dst={dst}"),
            format!("src={src}"),
            format!("kind={kind}"),
        ])?,
        Some(&src),
        &dst,
    )?;
    let did = if sub == "set" { "set" } else { "connected" };
    super::print_stdout_line(&format!("{did} @{src} -> @{dst} ({kind})"))?;
    Ok(ExitCode::SUCCESS)
}

/// `conn rm` — the wire `disconnect`, kind-filtered when `--kind` is given.
fn cmd_rm(wire: &ConnWire, self_sid: Option<&str>, args: &[String]) -> io::Result<ExitCode> {
    let spec = parse_pair_args("rm", args)?;
    let mut sessions = LazySessions::new(wire);
    let (dst, src) = resolve_pair(&spec, self_sid, &mut sessions)?;
    let mut parts = vec![
        "disconnect".to_string(),
        format!("dst={dst}"),
        format!("src={src}"),
    ];
    if let Some(kind) = &spec.kind {
        parts.push(format!("kind={kind}"));
    }
    let reply = wire.request(&parts)?;
    // A kind-filtered sweep that finds no row of that kind answers `no such
    // connection` even while the other half is live.
    if let Some(k) = spec.kind.as_deref().filter(|k| *k != "both")
        && reply.status == "ERR no such connection"
    {
        return Err(io::Error::other(no_live_half(&src, &dst, k)));
    }
    let reply = require_ok_for(reply, Some(&src), &dst)?;
    let revoked = reply
        .tail()
        .split_whitespace()
        .next()
        .and_then(|n| n.parse::<usize>().ok())
        .unwrap_or(0);
    super::print_stdout_line(&rm_outcome(&src, &dst, spec.kind.as_deref(), revoked))?;
    Ok(ExitCode::SUCCESS)
}

/// `conn spawn`: the wire `spawn connected=…`. `--of` defaults to `@self`
/// inside aterm and is required outside.
fn cmd_spawn(wire: &ConnWire, self_sid: Option<&str>, args: &[String]) -> io::Result<ExitCode> {
    let spec = parse_spawn_args(args)?;
    if spec.of.is_none() && self_sid.is_none() {
        return Err(usage(
            "conn spawn outside an aterm session requires --of <sel> ($ATERM_PARENT_SESSION_ID unset)",
        ));
    }
    let of_tok = spec.of.clone().unwrap_or_else(|| "@self".to_string());
    let mut sessions = LazySessions::new(wire);
    let of = resolve_selector(&of_tok, self_sid, &mut sessions)?;
    let reply = require_ok_for(
        wire.request(&[
            "spawn".to_string(),
            format!("connected={}", spec.role),
            format!("place={}", spec.place),
            format!("of={of}"),
        ])?,
        None,
        &of,
    )?;
    let new = reply.tail().split_whitespace().next().unwrap_or("-");
    super::print_stdout_line(&spawn_outcome(new, &spec.role, spec.place, &of))?;
    Ok(ExitCode::SUCCESS)
}

/// `conn show <sel>` — the wire `raise`: bring the peer's window forward and
/// select its tab.
fn cmd_show(wire: &ConnWire, self_sid: Option<&str>, args: &[String]) -> io::Result<ExitCode> {
    let [sel] = args else {
        return Err(usage("conn show takes exactly one selector"));
    };
    let mut sessions = LazySessions::new(wire);
    let sid = resolve_selector(sel, self_sid, &mut sessions)?;
    require_ok_for(
        wire.request(&["raise".to_string(), sid.clone()])?,
        None,
        &sid,
    )?;
    super::print_stdout_line(&format!("raised @{sid}"))?;
    Ok(ExitCode::SUCCESS)
}

/// `conn map` — the wire `open connections`: the GUI connection map.
fn cmd_map(wire: &ConnWire) -> io::Result<ExitCode> {
    require_ok(wire.request(&["open".to_string(), "connections".to_string()])?)?;
    super::print_stdout_line("opened the connection map")?;
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|t| t.to_string()).collect()
    }

    /// The §6.1 direction rules: default is "I take control" (src=@self,
    /// dst=peer), `--to-me` inverts, `--from` wires a third-party pair.
    #[test]
    fn direction_mapping_produces_the_right_dst_src_pairs() {
        assert_eq!(
            direction_pair("@s-b", false, None),
            ("@s-b".to_string(), "@self".to_string()),
            "default: @self -> <sel>"
        );
        assert_eq!(
            direction_pair("@s-b", true, None),
            ("@self".to_string(), "@s-b".to_string()),
            "--to-me: <sel> -> @self"
        );
        assert_eq!(
            direction_pair("@s-b", false, Some("@s-c")),
            ("@s-b".to_string(), "@s-c".to_string()),
            "--from: <from> -> <sel>"
        );
    }

    #[test]
    fn pair_args_parse_flags_and_refuse_conflicts() {
        let spec = parse_pair_args("add", &s(&["@s-b", "--kind", "pull", "--to-me"])).unwrap();
        assert_eq!(
            spec,
            PairSpec {
                sel: "@s-b".to_string(),
                kind: Some("pull".to_string()),
                to_me: true,
                from: None,
            }
        );
        // `--kind=`/`--from=` forms parse too.
        let spec = parse_pair_args("rm", &s(&["--kind=push", "@7", "--from=@s-c"])).unwrap();
        assert_eq!(spec.kind.as_deref(), Some("push"));
        assert_eq!(spec.from.as_deref(), Some("@s-c"));
        assert_eq!(spec.sel, "@7");
        // Refusals: conflicting direction flags, a bad kind, no selector,
        // two selectors, an unknown flag.
        assert!(parse_pair_args("add", &s(&["@s-b", "--to-me", "--from", "@s-c"])).is_err());
        assert!(parse_pair_args("add", &s(&["@s-b", "--kind", "sideways"])).is_err());
        assert!(parse_pair_args("add", &s(&[])).is_err());
        assert!(parse_pair_args("add", &s(&["@s-b", "@s-c"])).is_err());
        assert!(parse_pair_args("add", &s(&["@s-b", "--force"])).is_err());
    }

    /// Titles are never selectors (a bare word is a usage error naming the
    /// three forms), and `@.` is refused.
    #[test]
    fn selectors_refuse_titles_and_the_active_tab() {
        for bad in ["build", "worker (2)"] {
            let e = classify_selector(bad).unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
            assert!(
                e.to_string().contains("titles are ambiguous"),
                "title refusal names the reason: {e}"
            );
        }
        // A bare sid is a sid missing its @, not a title.
        let e = classify_selector("s-abc").unwrap_err();
        assert!(e.to_string().contains("@s-abc"), "got {e}");
        assert!(!e.to_string().contains("titles"), "got {e}");
        let e = classify_selector("@.").unwrap_err();
        assert!(e.to_string().contains("active tab"), "got {e}");
        // The three real forms classify.
        assert_eq!(classify_selector("@self").unwrap(), SelTok::SelfSid);
        assert_eq!(classify_selector("@env").unwrap(), SelTok::SelfSid);
        assert_eq!(classify_selector("@s-ab12").unwrap(), SelTok::Sid("s-ab12"));
        assert_eq!(classify_selector("@7").unwrap(), SelTok::Local(7));
        // A malformed @-form is refused, not guessed.
        assert!(classify_selector("@sideways").is_err());
    }

    /// Outside an aterm session (`$ATERM_PARENT_SESSION_ID` unset), `@self`
    /// forms refuse with the documented error naming the variable.
    #[test]
    fn self_selector_outside_aterm_refuses_with_the_env_var_named() {
        let mut no_lookup = |_n: u64| -> io::Result<Option<String>> {
            panic!("@self resolution must not hit the wire")
        };
        let e = resolve_selector_with("@self", None, &mut no_lookup).unwrap_err();
        assert!(
            e.to_string().contains("ATERM_PARENT_SESSION_ID"),
            "must name the env var: {e}"
        );
        // Inside: expands to the sid; @<sid> passes through; @<local> resolves
        // via the index and a miss is a clear error.
        let ok = resolve_selector_with("@self", Some("s-me"), &mut no_lookup).unwrap();
        assert_eq!(ok, "s-me");
        let ok = resolve_selector_with("@s-peer", None, &mut no_lookup).unwrap();
        assert_eq!(ok, "s-peer");
        let mut lookup =
            |n: u64| -> io::Result<Option<String>> { Ok((n == 3).then(|| "s-three".to_string())) };
        assert_eq!(
            resolve_selector_with("@3", None, &mut lookup).unwrap(),
            "s-three"
        );
        let e = resolve_selector_with("@9", None, &mut lookup).unwrap_err();
        assert!(e.to_string().contains("no session with local id 9"), "{e}");
    }

    #[test]
    fn spawn_args_parse_role_place_and_of() {
        assert_eq!(
            parse_spawn_args(&s(&["controlled"])).unwrap(),
            SpawnSpec {
                role: "controlled".to_string(),
                place: "window",
                of: None,
            },
            "--window is the default place"
        );
        let spec = parse_spawn_args(&s(&["controller", "--tab", "--of", "@s-w"])).unwrap();
        assert_eq!(spec.role, "controller");
        assert_eq!(spec.place, "tab");
        assert_eq!(spec.of.as_deref(), Some("@s-w"));
        let spec = parse_spawn_args(&s(&["controller", "--of=@4"])).unwrap();
        assert_eq!(spec.of.as_deref(), Some("@4"));
        // The role is mandatory and closed; junk is refused.
        assert!(parse_spawn_args(&s(&[])).is_err());
        assert!(parse_spawn_args(&s(&["supervisor"])).is_err());
        assert!(parse_spawn_args(&s(&["controlled", "extra"])).is_err());
    }

    /// The key-scan JSON parsers against fixtures shaped exactly like the
    /// server emitters (`cmd_flows` / `cmd_edges_json`).
    #[test]
    fn flows_and_edges_json_parse_the_wire_shapes() {
        let flows = "{\"flows\":[\
            {\"src\":\"s-a\",\"dst\":\"s-b\",\"ops\":[\"read-screen\",\"write-input\",\"signal\"]},\
            {\"src\":\"s-b\",\"dst\":\"s-a\",\"ops\":[\"read-screen\"]}]}";
        assert_eq!(
            parse_flows_json(flows),
            vec![
                FlowPair {
                    src: "s-a".to_string(),
                    dst: "s-b".to_string(),
                    ops: s(&["read-screen", "write-input", "signal"]),
                },
                FlowPair {
                    src: "s-b".to_string(),
                    dst: "s-a".to_string(),
                    ops: s(&["read-screen"]),
                },
            ]
        );
        assert!(parse_flows_json("{\"flows\":[]}").is_empty());

        // The trailing self-dst member must not read as a row.
        let edges = "{\"edges\":[\
            {\"src\":\"s-op\",\"dst\":\"s-me\",\"op\":\"write-input\"},\
            {\"src\":\"s-op\",\"dst\":\"s-me\",\"op\":\"signal\"}],\"dst\":\"s-me\"}";
        assert_eq!(
            parse_edges_json(edges),
            vec![
                (
                    "s-op".to_string(),
                    "s-me".to_string(),
                    "write-input".to_string()
                ),
                ("s-op".to_string(), "s-me".to_string(), "signal".to_string()),
            ]
        );
        assert!(parse_edges_json("{\"edges\":[],\"dst\":\"s-me\"}").is_empty());
    }

    #[test]
    fn sessions_lines_parse_and_percent_decode_titles() {
        let rows = parse_sessions_lines(&s(&[
            "0 s-aa11 - running cargo%20build meta=0",
            "1 s-bb22 s-aa11 exited operator meta=1",
            // An empty title pct-encodes to nothing, so meta= slides left.
            "2 s-cc33 - running meta=0",
            "garbage line",
        ]));
        assert_eq!(
            rows,
            vec![
                SessionRow {
                    local: 0,
                    sid: "s-aa11".to_string(),
                    title: "cargo build".to_string(),
                },
                SessionRow {
                    local: 1,
                    sid: "s-bb22".to_string(),
                    title: "operator".to_string(),
                },
                SessionRow {
                    local: 2,
                    sid: "s-cc33".to_string(),
                    title: String::new(),
                },
            ]
        );
    }

    /// Kind classification: pull = read-screen, push = write-input/signal,
    /// both = both halves, off-vocabulary = `other`.
    #[test]
    fn kinds_classify_from_the_op_set() {
        assert_eq!(kind_of_ops(&s(&["read-screen"])), "pull");
        assert_eq!(kind_of_ops(&s(&["write-input", "signal"])), "push");
        assert_eq!(kind_of_ops(&s(&["signal"])), "push");
        assert_eq!(
            kind_of_ops(&s(&["read-screen", "write-input", "signal"])),
            "both"
        );
        assert_eq!(kind_of_ops(&s(&["derive-loop"])), "other");
    }

    /// The no-arg render against a fixed fixture: outgoing ⇥, incoming ⇤, a
    /// symmetric pair collapses to ⇆, titles percent-decoded, foreign sids `-`.
    #[test]
    fn status_rows_render_glyphs_kinds_and_titles() {
        let index = parse_sessions_lines(&s(&[
            "0 s-me - running me meta=0",
            "1 s-worker - running cargo%20build meta=0",
            "2 s-peer - running peer meta=0",
        ]));
        // Outbound: both over the worker, pull over the peer.
        let outbound = vec![
            (
                "s-worker".to_string(),
                s(&["read-screen", "write-input", "signal"]),
            ),
            ("s-peer".to_string(), s(&["read-screen"])),
        ];
        // Inbound: the peer holds pull over us (symmetric with our pull ⇒ ⇆),
        // and a foreign supervisor pushes into us (unknown to `sessions`).
        let inbound = vec![
            ("s-peer".to_string(), s(&["read-screen"])),
            ("s-ghost".to_string(), s(&["write-input", "signal"])),
        ];
        let rows = render_self_rows(&inbound, &outbound, &index);
        assert_eq!(
            rows,
            vec![
                "\u{21e4} push  @s-ghost -".to_string(),
                "\u{21c6} pull  @s-peer \"peer\"".to_string(),
                "\u{21e5} both  @s-worker \"cargo build\"".to_string(),
            ]
        );
        // Asymmetric kinds never collapse: both directions stay visible.
        let rows = render_self_rows(
            &[("s-x".to_string(), s(&["read-screen"]))],
            &[(
                "s-x".to_string(),
                s(&["read-screen", "write-input", "signal"]),
            )],
            &[],
        );
        assert_eq!(
            rows,
            vec![
                "\u{21e5} both  @s-x -".to_string(),
                "\u{21e4} pull  @s-x -".to_string(),
            ]
        );
    }

    #[test]
    fn ls_rows_render_pairs_with_titles() {
        let index = parse_sessions_lines(&s(&["0 s-a - running alpha meta=0"]));
        let pairs = vec![FlowPair {
            src: "s-a".to_string(),
            dst: "s-b".to_string(),
            ops: s(&["read-screen"]),
        }];
        assert_eq!(
            render_ls_rows(&pairs, &index),
            vec!["@s-a -> @s-b  pull  \"alpha\" -> -".to_string()]
        );
    }

    /// Server refusals name the resolved ends; anything else passes through.
    #[test]
    fn refusals_name_the_resolved_sessions() {
        let r = |st: &str, src: Option<&str>| refusal(st, src, "s-b");
        assert_eq!(
            r("ERR no such session", Some("s-a")).as_deref(),
            Some("no session @s-b in this instance (see `aterm ctl ls`)")
        );
        assert_eq!(
            r("ERR exited", Some("s-a")).as_deref(),
            Some("@s-b has exited")
        );
        assert_eq!(
            r("ERR self-loop", Some("s-b")).as_deref(),
            Some("@s-b cannot connect to itself")
        );
        assert_eq!(
            r("ERR no such connection", Some("s-a")).as_deref(),
            Some("no connection @s-a -> @s-b")
        );
        assert!(r("ERR denied", None).unwrap().contains("owner access"));
        assert_eq!(
            r("ERR headless", None),
            None,
            "unknown refusals pass through"
        );
    }

    /// `conn rm` says what is left: the whole pair, one half, or nothing.
    #[test]
    fn rm_outcome_states_what_changed() {
        assert_eq!(
            rm_outcome("s-a", "s-b", None, 1),
            "disconnected @s-a -> @s-b"
        );
        assert_eq!(
            rm_outcome("s-a", "s-b", Some("both"), 3),
            "disconnected @s-a -> @s-b"
        );
        assert_eq!(
            rm_outcome("s-a", "s-b", Some("pull"), 1),
            "removed pull from @s-a -> @s-b"
        );
        assert_eq!(
            rm_outcome("s-a", "s-b", Some("pull"), 0),
            "@s-a -> @s-b: no live pull half to remove"
        );
    }

    /// `conn spawn` says which session drives which.
    #[test]
    fn spawn_outcome_states_the_direction() {
        assert_eq!(
            spawn_outcome("s-n", "controlled", "tab", "s-me"),
            "spawned @s-n in a new tab; @s-me drives it (both)"
        );
        assert_eq!(
            spawn_outcome("s-n", "controller", "window", "s-me"),
            "spawned @s-n in a new window; it drives @s-me (both)"
        );
    }

    /// The help page documents every subverb in `CONN_SUBVERBS`.
    #[test]
    fn usage_documents_every_subverb() {
        for sub in CONN_SUBVERBS.split_whitespace() {
            assert!(CONN_USAGE.contains(sub), "usage must document `{sub}`");
        }
    }

    /// The fish completion offers the conn subverbs on the `aterm
    /// --completions fish` script, gated behind a seen `conn` like the `ctl`
    /// verbs, and not on the `aterm-ctl` script (which has no `conn` verb).
    #[test]
    fn fish_completion_offers_the_conn_subverbs() {
        let script = crate::front_door_completion_script("fish", &["ctl", "conn"], &[])
            .expect("fish script");
        assert!(
            script.contains("-a 'ctl conn'"),
            "the `conn` verb completes on `aterm`: {script}"
        );
        assert!(
            script.contains("__fish_seen_subcommand_from conn"),
            "the subverbs are gated behind a seen `conn`: {script}"
        );
        assert!(
            script.contains(CONN_SUBVERBS),
            "every conn subverb completes"
        );
        assert!(
            !crate::completion_script("fish")
                .expect("fish script")
                .contains(CONN_SUBVERBS),
            "the aterm-ctl script does not advertise a verb aterm-ctl lacks"
        );
    }
}
