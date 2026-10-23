// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Gemini CLI (`@google/gemini-cli`) exit classification, session witnessing, and relaunch argv.

use std::path::{Component, Path, PathBuf};

use super::upgrade::ArgvRefusal;

/// Program name for Gemini CLI.
pub const PROGRAM: &str = "gemini";

/// Environment variable overriding the parent directory of `.gemini`.
pub const CLI_HOME_ENV: &str = "GEMINI_CLI_HOME";

/// The home it falls back to: `os.homedir()`, `$HOME` on a Unix.
pub const HOME_ENV: &str = "HOME";

/// Environment variable overriding the path to `trustedFolders.json`.
pub const TRUSTED_FOLDERS_ENV: &str = "GEMINI_CLI_TRUSTED_FOLDERS_PATH";

/// The Gemini directory under the home.
pub const GEMINI_DIR: &str = ".gemini";

/// The per-project directories under the Gemini directory.
pub const TMP_DIR: &str = "tmp";

/// The sessions' directory under a project's.
pub const CHATS_DIR: &str = "chats";

/// Marker file inside a project directory recording its root path.
pub const PROJECT_ROOT_FILE: &str = ".project_root";

const FLAG_RESUME: &str = "resume";

const FLAG_PROMPT_INTERACTIVE: &str = "prompt-interactive";

const FLAG_SESSION_ID: &str = "session-id";

const FLAG_SESSION_FILE: &str = "session-file";

/// Exit code returned when Gemini CLI refuses its input (`ExitCodes.FATAL_INPUT_ERROR`).
pub const FATAL_INPUT_ERROR: i64 = 42;

/// Prefix of CLI-injected session context user messages.
pub const SESSION_CONTEXT: &str = "<session_context>";

/// Whether `arg` (`argv[1]` under `node`) names the Gemini CLI entry script.
#[must_use]
pub fn is_gemini_script(arg: &str) -> bool {
    let path = Path::new(arg);
    let names: Vec<&str> = path
        .components()
        .rev()
        .take(4)
        .filter_map(|c| c.as_os_str().to_str())
        .collect();
    matches!(
        names.as_slice(),
        [PROGRAM, ..] | ["gemini.js", "bundle", "gemini-cli", "@google"]
    )
}

/// Whether `image` and `argv` identify a `node` process running Gemini CLI.
#[must_use]
pub fn is_gemini_process(image: Option<&Path>, argv: &[String]) -> bool {
    image
        .and_then(Path::file_name)
        .and_then(|n| n.to_str())
        .is_some_and(|n| n == "node")
        && argv.get(1).is_some_and(|a| is_gemini_script(a))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Arity {
    Switch,
    One,
    Optional,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Fate {
    Keep,
    Drop,
    Refuse,
    OneShot,
}

const FLAGS: &[(&str, Arity, Fate)] = &[
    ("acp", Arity::Switch, Fate::OneShot),
    ("accept-raw-output-risk", Arity::Switch, Fate::Keep),
    ("admin-policy", Arity::One, Fate::Keep),
    ("allowed-mcp-server-names", Arity::One, Fate::Keep),
    ("allowed-tools", Arity::One, Fate::Keep),
    ("approval-mode", Arity::One, Fate::Keep),
    ("d", Arity::Switch, Fate::Keep),
    ("debug", Arity::Switch, Fate::Keep),
    ("delete-session", Arity::One, Fate::OneShot),
    ("e", Arity::One, Fate::Keep),
    ("experimental-acp", Arity::Switch, Fate::OneShot),
    ("extensions", Arity::One, Fate::Keep),
    ("fake-responses", Arity::One, Fate::Refuse),
    ("fake-responses-non-strict", Arity::One, Fate::Refuse),
    ("h", Arity::Switch, Fate::OneShot),
    ("help", Arity::Switch, Fate::OneShot),
    ("i", Arity::One, Fate::Drop),
    (FLAG_PROMPT_INTERACTIVE, Arity::One, Fate::Drop),
    ("include-directories", Arity::One, Fate::Keep),
    ("l", Arity::Switch, Fate::OneShot),
    ("list-extensions", Arity::Switch, Fate::OneShot),
    ("list-sessions", Arity::Switch, Fate::OneShot),
    ("m", Arity::One, Fate::Keep),
    ("model", Arity::One, Fate::Keep),
    ("o", Arity::One, Fate::OneShot),
    ("output-format", Arity::One, Fate::OneShot),
    ("p", Arity::One, Fate::OneShot),
    ("policy", Arity::One, Fate::Keep),
    ("prompt", Arity::One, Fate::OneShot),
    ("r", Arity::Optional, Fate::Drop),
    ("raw-output", Arity::Switch, Fate::Keep),
    ("record-responses", Arity::One, Fate::Refuse),
    (FLAG_RESUME, Arity::Optional, Fate::Drop),
    ("s", Arity::Switch, Fate::Keep),
    ("sandbox", Arity::Switch, Fate::Keep),
    ("screen-reader", Arity::Switch, Fate::Keep),
    ("session-file", Arity::One, Fate::Drop),
    (FLAG_SESSION_ID, Arity::One, Fate::Drop),
    ("skip-trust", Arity::Switch, Fate::Keep),
    ("v", Arity::Switch, Fate::OneShot),
    ("version", Arity::Switch, Fate::OneShot),
    ("w", Arity::Optional, Fate::Refuse),
    ("worktree", Arity::Optional, Fate::Refuse),
    ("y", Arity::Switch, Fate::Keep),
    ("yolo", Arity::Switch, Fate::Keep),
];

fn flag(name: &str) -> Option<(Arity, Fate)> {
    FLAGS
        .iter()
        .find(|(n, _, _)| *n == name)
        .map(|(_, a, f)| (*a, *f))
}

const SUBCOMMANDS: &[&str] = &[
    "extension",
    "extensions",
    "gemma",
    "hook",
    "hooks",
    "mcp",
    "skill",
    "skills",
];

fn flag_token(tok: &str) -> Option<(&str, Option<&str>)> {
    let bare = tok.strip_prefix("--").or_else(|| tok.strip_prefix('-'))?;
    if bare.is_empty() {
        return None;
    }
    Some(match bare.split_once('=') {
        Some((name, value)) => (name, Some(value)),
        None => (bare, None),
    })
}

fn looks_like_flag(tok: &str) -> bool {
    tok.len() > 1 && tok.starts_with('-')
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Read {
    kept: Vec<String>,
    prompt: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Stop {
    Unknown(String),
    Refuse(String),
    Ends(String),
}

impl From<Stop> for ArgvRefusal {
    fn from(stop: Stop) -> Self {
        match stop {
            Stop::Unknown(name) => ArgvRefusal::UnknownFlag(name),
            Stop::Refuse(what) | Stop::Ends(what) => ArgvRefusal::NotResumable(what),
        }
    }
}

fn read(argv: &[String]) -> Result<Read, Stop> {
    let mut out = Read::default();
    let mut positional_seen = false;
    let mut i = 2;
    while i < argv.len() {
        let tok = argv[i].as_str();
        i += 1;
        if tok == "--" {
            out.prompt |= i < argv.len();
            return Ok(out);
        }
        let Some((name, attached)) = flag_token(tok) else {
            if !positional_seen && SUBCOMMANDS.contains(&tok) {
                return Err(Stop::Ends(tok.to_string()));
            }
            positional_seen = true;
            out.prompt = true;
            continue;
        };
        let Some((arity, fate)) = flag(name) else {
            return Err(Stop::Unknown(name.to_string()));
        };
        out.prompt |= name == "i" || name == FLAG_PROMPT_INTERACTIVE;
        let mut words = vec![tok.to_string()];
        if attached.is_none() {
            match arity {
                Arity::Switch => {
                    if let Some(value) = argv
                        .get(i)
                        .filter(|v| matches!(v.as_str(), "true" | "false"))
                    {
                        words.push(value.clone());
                        i += 1;
                    }
                }
                Arity::One => {
                    let Some(value) = argv.get(i).filter(|v| !looks_like_flag(v)) else {
                        return Err(Stop::Unknown(name.to_string()));
                    };
                    words.push(value.clone());
                    i += 1;
                }
                Arity::Optional => {
                    if let Some(value) = argv.get(i).filter(|v| !looks_like_flag(v)) {
                        words.push(value.clone());
                        i += 1;
                    }
                }
            }
        }
        match fate {
            Fate::Keep => out.kept.extend(words),
            Fate::Drop => {}
            Fate::Refuse => return Err(Stop::Refuse(name.to_string())),
            Fate::OneShot => return Err(Stop::Ends(name.to_string())),
        }
    }
    Ok(out)
}

/// Whether `argv` launches an interactive Gemini CLI TUI rather than a subcommand or one-shot.
#[must_use]
pub fn is_tui_argv(argv: &[String]) -> bool {
    match read(argv) {
        Ok(_) | Err(Stop::Refuse(_)) => true,
        Err(Stop::Ends(_)) => false,
        Err(Stop::Unknown(_)) => !one_shot_anywhere(argv),
    }
}

fn one_shot_anywhere(argv: &[String]) -> bool {
    let words = || argv.iter().skip(2).take_while(|t| t.as_str() != "--");
    words().any(|t| {
        flag_token(t)
            .is_some_and(|(name, _)| flag(name).is_some_and(|(_, fate)| fate == Fate::OneShot))
    }) || words()
        .find(|t| flag_token(t).is_none())
        .is_some_and(|t| SUBCOMMANDS.contains(&t.as_str()))
}

fn flag_value<'a>(argv: &'a [String], names: &[&str]) -> Option<&'a str> {
    let mut found = None;
    let mut i = 2;
    while i < argv.len() {
        let tok = argv[i].as_str();
        i += 1;
        if tok == "--" {
            break;
        }
        let Some((name, attached)) = flag_token(tok) else {
            continue;
        };
        if !names.contains(&name) {
            continue;
        }
        found = match attached {
            Some(value) => Some(value),
            None => {
                let value = argv
                    .get(i)
                    .map(String::as_str)
                    .filter(|v| !looks_like_flag(v));
                if value.is_some() {
                    i += 1;
                }
                value
            }
        };
    }
    found
}

/// Whether `argv` carries an initial prompt (`-i`/`--prompt-interactive` or positional args).
#[must_use]
pub fn carries_first_prompt(argv: &[String]) -> bool {
    read(argv).map_or(true, |r| r.prompt)
}

/// Extract the full session UUID named by `--resume`/`-r` or `--session-id` in `argv`, if any.
#[must_use]
pub fn argv_session(argv: &[String]) -> Option<String> {
    flag_value(argv, &[FLAG_RESUME, "r", FLAG_SESSION_ID])
        .filter(|v| is_session_uuid(v))
        .map(str::to_owned)
}

/// Rewrite launch `argv` into relaunch args carrying preserved flags plus `--resume <session>`.
pub fn rewrite_argv(argv: &[String], session: Option<&str>) -> Result<Vec<String>, ArgvRefusal> {
    let mut out = read(argv).map_err(ArgvRefusal::from)?.kept;
    if let Some(session) = session {
        if !is_session_uuid(session) {
            return Err(ArgvRefusal::NotResumable(format!("session {session}")));
        }
        out.push(format!("--{FLAG_RESUME}"));
        out.push(session.to_string());
    }
    Ok(out)
}

/// Whether `s` is a hyphenated 8-4-4-4-12 hex UUID.
#[must_use]
pub fn is_session_uuid(s: &str) -> bool {
    s.len() == 36 && super::codex_rollout::is_thread_id(s)
}

/// Resolve the `.gemini` directory from `GEMINI_CLI_HOME`, `HOME`, or `fallback`.
#[must_use]
pub fn gemini_dir(cli_home: Option<&str>, home: Option<&str>, fallback: &Path) -> Option<PathBuf> {
    match cli_home.filter(|h| !h.is_empty()) {
        Some(h) if Path::new(h).has_root() => Some(Path::new(h).join(GEMINI_DIR)),
        Some(_) => None,
        None => Some(
            home.map(PathBuf::from)
                .filter(|h| h.has_root())
                .unwrap_or_else(|| fallback.to_path_buf())
                .join(GEMINI_DIR),
        ),
    }
}

/// Lowercase hex SHA-256 of `root`, matching Gemini CLI's `projectHash`.
#[must_use]
pub fn project_hash(root: &str) -> String {
    aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(root.as_bytes()))
}

/// Whether a `.project_root` marker file's contents match `root`.
#[must_use]
pub fn owns(marker: &str, root: &str) -> bool {
    !root.is_empty() && marker.trim() == root
}

/// The chats directory of the project directory `project` (`<tmp>/<slug>`).
#[must_use]
pub fn chats_dir(project: &Path) -> PathBuf {
    project.join(CHATS_DIR)
}

/// Whether `name` is a session file (`session-<minute>-<id8>.jsonl` or `.json`) for `session`.
#[must_use]
pub fn names_session(name: &str, session: &str) -> bool {
    let Some(id8) = session.get(..8) else {
        return false;
    };
    name.starts_with("session-")
        && (name.ends_with(&format!("-{id8}.jsonl")) || name.ends_with(&format!("-{id8}.json")))
}

/// Whether `name` is any session file (`session-…jsonl`).
#[must_use]
pub fn is_session_file(name: &str) -> bool {
    name.starts_with("session-") && name.ends_with(".jsonl")
}

/// Parse an ISO-8601 UTC timestamp (`YYYY-MM-DDTHH:MM:SS.mmmZ`) into Unix milliseconds.
#[must_use]
pub fn stamp_ms(stamp: &str) -> Option<u64> {
    let secs = u64::try_from(aterm_types::rfc3339::parse_utc_fractional(stamp)?).ok()?;
    let frac = stamp
        .strip_suffix('Z')?
        .get(19..)
        .unwrap_or("")
        .trim_start_matches('.');
    let mut ms = 0_u64;
    for (i, d) in frac.bytes().take(3).enumerate() {
        ms += u64::from(d - b'0') * [100, 10, 1][i];
    }
    Some(secs * 1000 + ms)
}

enum Record<'a> {
    Rewind(&'a str),
    Message(Message),
    Set(&'a aterm_json::Value),
    Header(&'a aterm_json::Value),
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Message {
    id: String,
    turn: bool,
    user_or_model: bool,
    at_ms: Option<u64>,
}

fn text_of(content: Option<&aterm_json::Value>) -> String {
    match content {
        Some(aterm_json::Value::String(s)) => s.clone(),
        Some(aterm_json::Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(aterm_json::Value::as_str))
            .collect(),
        _ => String::new(),
    }
}

fn message_of(v: &aterm_json::Value) -> Option<Message> {
    let id = v.get("id")?.as_str()?.to_string();
    let kind = v.get("type").and_then(aterm_json::Value::as_str);
    let turn = match kind {
        Some("gemini") => true,
        Some("user") => !text_of(v.get("content")).starts_with(SESSION_CONTEXT),
        _ => false,
    };
    Some(Message {
        id,
        turn,
        user_or_model: matches!(kind, Some("user" | "gemini")),
        at_ms: v
            .get("timestamp")
            .and_then(aterm_json::Value::as_str)
            .and_then(stamp_ms),
    })
}

fn record_of(v: &aterm_json::Value) -> Record<'_> {
    if let Some(id) = v.get("$rewindTo").and_then(aterm_json::Value::as_str) {
        return Record::Rewind(id);
    }
    if v.get("id").and_then(aterm_json::Value::as_str).is_some() {
        return message_of(v).map_or(Record::Other, Record::Message);
    }
    if let Some(set) = v.get("$set").filter(|s| s.is_object()) {
        return Record::Set(set);
    }
    if v.get("sessionId")
        .and_then(aterm_json::Value::as_str)
        .is_some()
        && v.get("projectHash")
            .and_then(aterm_json::Value::as_str)
            .is_some()
    {
        return Record::Header(v);
    }
    Record::Other
}

fn values(text: &str) -> Vec<aterm_json::Value> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter_map(|l| aterm_json::from_str::<aterm_json::Value>(l).ok())
        .collect()
}

/// Folded state of a single Gemini CLI session file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fold {
    /// `sessionId`.
    pub session: Option<String>,
    /// `projectHash`.
    pub project_hash: Option<String>,
    /// `lastUpdated`, as written (the CLI compares the strings).
    pub last_updated: Option<String>,
    /// Ordered turn IDs (excluding `<session_context>` user messages).
    pub turns: Vec<String>,
    /// Whether the file contains at least one user or model message.
    pub valid: bool,
}

/// [`Fold`] of one session file's `text`.
#[must_use]
pub fn fold(text: &str) -> Fold {
    fn str_of(v: &aterm_json::Value, key: &str) -> Option<String> {
        v.get(key)
            .and_then(aterm_json::Value::as_str)
            .map(str::to_owned)
    }
    let mut out = Fold::default();
    let mut messages: Vec<Message> = Vec::new();
    let put = |messages: &mut Vec<Message>, m: Message| match messages
        .iter_mut()
        .find(|old| old.id == m.id)
    {
        Some(old) => *old = m,
        None => messages.push(m),
    };
    for v in values(text) {
        let meta = |set: &aterm_json::Value, out: &mut Fold| {
            if let Some(s) = str_of(set, "sessionId") {
                out.session = Some(s);
            }
            if let Some(h) = str_of(set, "projectHash") {
                out.project_hash = Some(h);
            }
            if let Some(l) = str_of(set, "lastUpdated") {
                out.last_updated = Some(l);
            }
        };
        match record_of(&v) {
            Record::Rewind(id) => match messages.iter().position(|m| m.id == id) {
                Some(at) => messages.truncate(at),
                None => messages.clear(),
            },
            Record::Message(m) => put(&mut messages, m),
            Record::Set(set) => {
                if let Some(list) = set.get("messages").and_then(aterm_json::Value::as_array) {
                    messages.clear();
                    for m in list.iter().filter_map(message_of) {
                        put(&mut messages, m);
                    }
                }
                meta(set, &mut out);
            }
            Record::Header(h) => {
                meta(h, &mut out);
                if let Some(list) = h.get("messages").and_then(aterm_json::Value::as_array) {
                    for m in list.iter().filter_map(message_of) {
                        put(&mut messages, m);
                    }
                }
            }
            Record::Other => {}
        }
    }
    out.valid = messages.iter().any(|m| m.user_or_model);
    out.turns = messages
        .iter()
        .filter(|m| m.turn)
        .map(|m| m.id.clone())
        .collect();
    out
}

/// Session header record with `sessionId`, `projectHash`, and `startTime`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    /// `sessionId`.
    pub session: String,
    /// `projectHash`.
    pub project_hash: String,
    /// `startTime`, unix milliseconds.
    pub start_ms: u64,
}

/// Extract all session headers with a valid `startTime` from `text`.
#[must_use]
pub fn headers(text: &str) -> Vec<Header> {
    values(text)
        .iter()
        .filter_map(|v| match record_of(v) {
            Record::Header(h) => Some(Header {
                session: h.get("sessionId")?.as_str()?.to_string(),
                project_hash: h.get("projectHash")?.as_str()?.to_string(),
                start_ms: h
                    .get("startTime")
                    .and_then(aterm_json::Value::as_str)
                    .and_then(stamp_ms)?,
            }),
            _ => None,
        })
        .collect()
}

/// Timestamp and header summary of a session file for [`witness`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Scan {
    /// Every header with a `startTime` ([`headers`]).
    pub headers: Vec<Header>,
    /// The session it folds to ([`Fold::session`]).
    pub session: Option<String>,
    /// Latest message `timestamp` or `$set.lastUpdated` in Unix milliseconds.
    pub latest_ms: Option<u64>,
    /// Session ID of a trailing `$set` switch with no subsequent stamped write.
    pub trailing_switch: Option<String>,
}

/// [`Scan`] of one session file's `text`.
#[must_use]
pub fn scan(text: &str) -> Scan {
    let vals = values(text);
    let mut out = Scan {
        headers: headers(text),
        session: fold(text).session,
        ..Scan::default()
    };
    let stamp = |set: &aterm_json::Value, key: &str| {
        set.get(key)
            .and_then(aterm_json::Value::as_str)
            .and_then(stamp_ms)
    };
    for v in &vals {
        let (at, stamped, switch) = match record_of(v) {
            Record::Message(m) => (m.at_ms, m.at_ms.is_some(), None),
            Record::Set(set) => {
                let at = stamp(set, "lastUpdated");
                let switch = set.get("sessionId").and_then(aterm_json::Value::as_str);
                (at, at.is_some(), switch)
            }
            Record::Header(h) => (None, stamp(h, "startTime").is_some(), None),
            Record::Rewind(_) | Record::Other => (None, false, None),
        };
        if let Some(at) = at {
            out.latest_ms = Some(out.latest_ms.map_or(at, |was| was.max(at)));
        }
        if stamped {
            out.trailing_switch = None;
        } else if let Some(session) = switch {
            out.trailing_switch = Some(session.to_string());
        }
    }
    out
}

impl Scan {
    /// Whether this file has activity or an un-accounted trailing switch at or after `start_ms`.
    #[must_use]
    pub fn moved(
        &self,
        start_ms: u64,
        mtime_ms: Option<u64>,
        resumed: &dyn Fn(&str) -> bool,
    ) -> bool {
        self.latest_ms.is_some_and(|at| at >= start_ms)
            || self.trailing_switch.as_deref().is_some_and(|session| {
                mtime_ms.is_none_or(|at| at >= start_ms) && !resumed(session)
            })
    }
}

/// Whether every `$set` switch onto `session` across `files` has a matching `--resume` header.
#[must_use]
pub fn resumes_account_for_switches(files: &[ChatsFile<'_>], session: &str) -> bool {
    let mine: Vec<&ChatsFile<'_>> = files
        .iter()
        .filter(|f| names_session(f.name, session))
        .collect();
    if mine.is_empty() || mine.iter().any(|f| !f.name.ends_with(".jsonl")) {
        return false;
    }
    let (mut headers, mut switches) = (0_usize, 0_usize);
    for f in mine {
        for v in values(f.text) {
            match record_of(&v) {
                Record::Header(h) => {
                    headers += usize::from(
                        h.get("sessionId").and_then(aterm_json::Value::as_str) == Some(session),
                    );
                }
                Record::Set(set) => {
                    switches += usize::from(
                        set.get("sessionId").and_then(aterm_json::Value::as_str) == Some(session),
                    );
                }
                Record::Message(_) | Record::Rewind(_) | Record::Other => {}
            }
        }
    }
    switches < headers
}

/// Session file name, contents, and modification time for [`witness`] and [`precheck`].
#[derive(Clone, Copy, Debug)]
pub struct ChatsFile<'a> {
    /// Its name in the chats directory.
    pub name: &'a str,
    /// Its words.
    pub text: &'a str,
    /// When it was last written, unix milliseconds (`None`: unread).
    pub mtime_ms: Option<u64>,
}

/// Why the start-time rule names no session ([`witness`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoWitness {
    /// The project's chats directory was not found or not read.
    NoChats,
    /// No session header was stamped at or after the process start time.
    NoHeader,
    /// The only header after process start was stamped past [`LATE_HEADER_MS`] and not in argv.
    LateHeader,
    /// Multiple session headers were stamped since the process started.
    TwoHeaders,
    /// Another Gemini CLI process in the same project started within [`START_WINDOW_MS`].
    StartedTogether,
    /// The header's session ID is not a full UUID or belongs to another project.
    NotTyped,
    /// The argv names another session than the header does.
    ArgvDisagrees,
    /// Another session's file moved since the process started.
    AnotherMoved,
}

impl NoWitness {
    /// One word for the step and the ledger.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            NoWitness::NoChats => "no-chats",
            NoWitness::NoHeader => "no-header",
            NoWitness::LateHeader => "late-header",
            NoWitness::TwoHeaders => "two-headers",
            NoWitness::StartedTogether => "started-together",
            NoWitness::NotTyped => "not-typed",
            NoWitness::ArgvDisagrees => "argv-disagrees",
            NoWitness::AnotherMoved => "another-moved",
        }
    }

    /// What aterm could not tell, in a person's words.
    #[must_use]
    pub const fn said(self) -> &'static str {
        match self {
            NoWitness::NoChats => "its project's session files could not be read",
            NoWitness::NoHeader => "no session file of its project was begun after it started",
            NoWitness::LateHeader => {
                "the one session file of its project begun after it started was begun too late to \
                 be its own"
            }
            NoWitness::TwoHeaders => {
                "two sessions of its project were begun after it started (a /clear, or another \
                 Gemini CLI)"
            }
            NoWitness::StartedTogether => {
                "another Gemini CLI started in its project at the same moment"
            }
            NoWitness::NotTyped => "its session's id is no id aterm types back",
            NoWitness::ArgvDisagrees => "its launch named another session than its file does",
            NoWitness::AnotherMoved => {
                "another session of its project moved after it started (an in-TUI /resume)"
            }
        }
    }
}

/// Minimum separation between two starts in the same project for unambiguous header attribution.
pub const START_WINDOW_MS: u64 = 2_000;

/// Slack added to process start time readings on platforms with second-granularity start times.
pub const START_READ_SLACK_MS: u64 = if cfg!(target_vendor = "apple") {
    0
} else {
    2_000
};

/// Maximum delay after process start within which a process's own session header is expected.
pub const LATE_HEADER_MS: u64 = START_WINDOW_MS + START_READ_SLACK_MS;

/// Identify the session UUID owned by a Gemini CLI process started at `start_ms`.
pub fn witness(
    files: &[ChatsFile<'_>],
    start_ms: u64,
    root_hash: &str,
    argv_session: Option<&str>,
    started_together: bool,
    resumed: &dyn Fn(&str) -> bool,
) -> Result<String, NoWitness> {
    if started_together {
        return Err(NoWitness::StartedTogether);
    }
    let scans: Vec<(Scan, Option<u64>)> =
        files.iter().map(|f| (scan(f.text), f.mtime_ms)).collect();
    let since: Vec<&Header> = scans
        .iter()
        .flat_map(|(s, _)| &s.headers)
        .filter(|h| h.start_ms >= start_ms)
        .collect();
    let header = match since.as_slice() {
        [] => return Err(NoWitness::NoHeader),
        [one] => *one,
        _ => return Err(NoWitness::TwoHeaders),
    };
    if header.start_ms > start_ms.saturating_add(LATE_HEADER_MS)
        && argv_session != Some(header.session.as_str())
    {
        return Err(NoWitness::LateHeader);
    }
    if !is_session_uuid(&header.session) || header.project_hash != root_hash {
        return Err(NoWitness::NotTyped);
    }
    if argv_session.is_some_and(|s| s != header.session) {
        return Err(NoWitness::ArgvDisagrees);
    }
    let moved = scans.iter().any(|(s, mtime)| {
        s.session.as_deref() != Some(header.session.as_str()) && s.moved(start_ms, *mtime, resumed)
    });
    if moved {
        return Err(NoWitness::AnotherMoved);
    }
    Ok(header.session.clone())
}

/// Pre-resume validation verdict for `gemini --resume <id>` ([`precheck`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Precheck {
    /// The file it would load holds every turn of the session: typed.
    Safe {
        /// The file it would load.
        pick: String,
        /// How many turns it holds.
        turns: usize,
    },
    /// The session took no turn in any of its files: nothing to resume.
    NoTurns,
    /// No loadable file exists for the session.
    NoFile,
    /// The newest file for the session is missing turns present in another file for the same ID.
    Unsafe {
        /// The file it would load.
        pick: String,
        /// How many turns only other files hold.
        missing: usize,
    },
    /// A file for the session could not be folded (e.g., legacy `.json`).
    Unreadable(String),
}

impl Precheck {
    /// One word for the step and the ledger.
    #[must_use]
    pub const fn word(&self) -> &'static str {
        match self {
            Precheck::Safe { .. } => "safe",
            Precheck::NoTurns => "no-turns",
            Precheck::NoFile => "no-file",
            Precheck::Unsafe { .. } => "unsafe",
            Precheck::Unreadable(_) => "unreadable",
        }
    }
}

/// Verify that the file Gemini CLI selects for `session` holds all turns across all its files.
#[must_use]
pub fn precheck(files: &[ChatsFile<'_>], session: &str) -> Precheck {
    let mut sorted: Vec<&ChatsFile<'_>> = files
        .iter()
        .filter(|f| names_session(f.name, session))
        .collect();
    sorted.sort_by(|a, b| a.name.cmp(b.name));
    if let Some(legacy) = sorted.iter().find(|f| !f.name.ends_with(".jsonl")) {
        return Precheck::Unreadable(legacy.name.to_string());
    }
    let folds: Vec<(&str, Fold)> = sorted
        .iter()
        .map(|f| (f.name, fold(f.text)))
        .filter(|(_, fold)| fold.session.as_deref() == Some(session))
        .collect();
    let mut pick: Option<&(&str, Fold)> = None;
    for f in folds.iter().filter(|(_, fold)| fold.valid) {
        let newer = match pick {
            None => true,
            Some((_, best)) => {
                f.1.last_updated.as_deref().unwrap_or("")
                    > best.last_updated.as_deref().unwrap_or("")
            }
        };
        if newer {
            pick = Some(f);
        }
    }
    let Some((name, best)) = pick else {
        return Precheck::NoFile;
    };
    let all: std::collections::BTreeSet<&str> = folds
        .iter()
        .flat_map(|(_, f)| f.turns.iter().map(String::as_str))
        .collect();
    if all.is_empty() {
        return Precheck::NoTurns;
    }
    let held: std::collections::BTreeSet<&str> = best.turns.iter().map(String::as_str).collect();
    let missing = all.difference(&held).count();
    if missing == 0 {
        Precheck::Safe {
            pick: (*name).to_string(),
            turns: held.len(),
        }
    } else {
        Precheck::Unsafe {
            pick: (*name).to_string(),
            missing,
        }
    }
}

/// Classification of how a Gemini CLI process exited.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitKind {
    /// Clean exit (`0` or `130`), never relaunched.
    Clean,
    /// Stopped by signal (`SIGSTOP`/`SIGTSTP`), still alive.
    Stopped,
    /// Refused by the CLI at startup (`41`, `42`, `44`, `52`..=`55`), never retried.
    Refused,
    /// Crashed (`1` or `128 + signal` for an uncatchable signal), eligible for relaunch.
    Crashed,
    /// Any other exit status, not relaunched.
    Unwitnessed,
    /// Exit `42` when an in-app restart re-ran `--session-id <uuid>` against its own first file.
    Restarted,
}

impl ExitKind {
    /// One word for the step and the ledger.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            ExitKind::Clean => "clean",
            ExitKind::Stopped => "stopped",
            ExitKind::Refused => "refused",
            ExitKind::Crashed => "crashed",
            ExitKind::Unwitnessed => "unwitnessed",
            ExitKind::Restarted => "restarted",
        }
    }
}

#[cfg(unix)]
const CRASH_SIGNALS: [libc::c_int; 6] = [
    libc::SIGKILL,
    libc::SIGSEGV,
    libc::SIGABRT,
    libc::SIGBUS,
    libc::SIGILL,
    libc::SIGFPE,
];
#[cfg(not(unix))]
const CRASH_SIGNALS: [i32; 0] = [];

/// Classify a shell exit status from a Gemini CLI block into [`ExitKind`].
#[must_use]
pub fn exit_kind(status: i64) -> ExitKind {
    match status {
        0 | 130 => ExitKind::Clean,
        41 | 42 | 44 | 52..=55 => ExitKind::Refused,
        1 => ExitKind::Crashed,
        status if super::upgrade_agy::stopped_status(status) => ExitKind::Stopped,
        status
            if CRASH_SIGNALS
                .iter()
                .any(|&signal| status == 128 + i64::from(signal)) =>
        {
            ExitKind::Crashed
        }
        _ => ExitKind::Unwitnessed,
    }
}

/// Classify the exit status of a relaunch line whose Gemini CLI process never took over the tab.
#[must_use]
pub fn line_exit_kind(status: i64) -> ExitKind {
    match status {
        1 | 126 | 127 => ExitKind::Refused,
        status => exit_kind(status),
    }
}

/// Return `--session-id <uuid>` from `argv` when `status` is [`FATAL_INPUT_ERROR`] (`42`).
#[must_use]
pub fn restart_id(status: i64, argv: &[String]) -> Option<&str> {
    let names_another = argv
        .iter()
        .skip(2)
        .take_while(|t| t.as_str() != "--")
        .filter_map(|t| flag_token(t))
        .any(|(name, _)| [FLAG_RESUME, "r", FLAG_SESSION_FILE].contains(&name));
    if status != FATAL_INPUT_ERROR || names_another {
        return None;
    }
    flag_value(argv, &[FLAG_SESSION_ID]).filter(|id| is_session_uuid(id))
}

/// Collect `GEMINI_CLI_HOME`, non-default `HOME`, and `GEMINI_CLI_TRUSTED_FOLDERS_PATH`.
#[must_use]
pub fn relaunch_env<'a>(
    get: impl Fn(&str) -> Option<&'a str>,
    user_home: &Path,
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Some(h) = get(HOME_ENV).filter(|h| Path::new(h).has_root() && Path::new(h) != user_home)
    {
        out.push((HOME_ENV.to_string(), h.to_string()));
    }
    if let Some(h) = get(CLI_HOME_ENV).filter(|h| !h.is_empty()) {
        out.push((CLI_HOME_ENV.to_string(), h.to_string()));
    }
    if let Some(f) = get(TRUSTED_FOLDERS_ENV).filter(|f| Path::new(f).has_root()) {
        out.push((TRUSTED_FOLDERS_ENV.to_string(), f.to_string()));
    }
    out
}

/// Format [`relaunch_env`] pairs as shell-quoted `NAME=VALUE` words for a relaunch line.
#[must_use]
pub fn line_env(env: &[(String, String)]) -> Vec<(&str, &str)> {
    env.iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect()
}

/// [`relaunch_env`]'s pairs as a record keeps them (`NAME=VALUE`).
#[must_use]
pub fn env_words(env: &[(String, String)]) -> Vec<String> {
    env.iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect()
}

/// Parse `NAME=VALUE` words back into [`relaunch_env`] pairs (excluding trusted folders).
#[must_use]
pub fn env_of_words(words: &[String]) -> Vec<(String, String)> {
    words
        .iter()
        .filter_map(|w| w.split_once('='))
        .filter(|(name, _)| *name == HOME_ENV || *name == CLI_HOME_ENV)
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect()
}

/// Extract an absolute [`TRUSTED_FOLDERS_ENV`] path from recorded `NAME=VALUE` words.
#[must_use]
pub fn trusted_folders_of_words(words: &[String]) -> Option<&str> {
    words
        .iter()
        .filter_map(|w| w.split_once('='))
        .find(|(name, _)| *name == TRUSTED_FOLDERS_ENV)
        .map(|(_, value)| value)
        .filter(|value| Path::new(value).has_root())
}

/// Build the shell command shown to manually resume `session` in `root`, if safely quotable.
#[must_use]
pub fn told_line(session: &str, env: &[(String, String)], root: &str) -> Option<String> {
    if !is_session_uuid(session) {
        return None;
    }
    let plain = |value: &str| {
        !value.is_empty()
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_./:@%+,=".contains(&b))
    };
    let word = |value: &str| {
        if plain(value) {
            value.to_string()
        } else {
            super::upgrade::quote(super::upgrade::Dialect::Zsh, value)
        }
    };
    if root.chars().any(char::is_control) {
        return None;
    }
    let mut said = String::new();
    if names_one_folder(Path::new(root)) {
        said.push_str("cd ");
        said.push_str(&word(root));
        said.push_str(" && ");
    }
    for (name, value) in env.iter().filter(|(name, _)| name != TRUSTED_FOLDERS_ENV) {
        if value.chars().any(char::is_control) {
            return None;
        }
        said.push_str(name);
        said.push('=');
        said.push_str(&word(value));
        said.push(' ');
    }
    said.push_str(PROGRAM);
    said.push_str(&format!(" --{FLAG_RESUME} {session}"));
    Some(said)
}

fn names_one_folder(path: &Path) -> bool {
    path.has_root()
        && path.components().all(|c| match c {
            Component::RootDir | Component::Normal(_) => true,
            Component::Prefix(p) => matches!(p.kind(), std::path::Prefix::Disk(_)),
            Component::CurDir | Component::ParentDir => false,
        })
}

/// The version a Gemini CLI install's `package.json` names (`"version"`).
#[must_use]
pub fn package_version(json: &str) -> Option<String> {
    let v: aterm_json::Value = aterm_json::from_str(json).ok()?;
    if v.get("name").and_then(aterm_json::Value::as_str) != Some("@google/gemini-cli") {
        return None;
    }
    v.get("version")
        .and_then(aterm_json::Value::as_str)
        .filter(|s| !s.is_empty() && !s.chars().any(char::is_control))
        .map(str::to_owned)
}

#[cfg(test)]
#[path = "upgrade_gemini_tests.rs"]
mod tests;
