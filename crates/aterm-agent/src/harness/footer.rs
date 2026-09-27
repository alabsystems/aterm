// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CLAUDE CODE FOOTER — what aterm paints in place of Claude Code's
//! permission-mode row (owner direction, 2026-09-24): one row, three marks,
//! Codex-style —
//!
//! ```text
//!   ◆ Opus 5.5 xhigh   ⌂ aterm   ⎇ main
//! ```
//!
//! model and effort under one mark, then the repository, then the branch.
//! The vendor row it replaces (`⏵⏵ bypass permissions on (shift+tab to
//! cycle) · ← for agents`) cannot be switched off from outside: measured on
//! 2.1.282, no setting or environment variable removes it, and a statusLine
//! only ADDS a row above it — even one that prints nothing reserves a blank
//! row. So the facts do not come from a statusLine at all. They come from
//! files Claude Code already keeps:
//!
//! * `<claude dir>/sessions/<pid>.json` maps the Claude Code PROCESS to its
//!   session id and working directory — which is how a pane finds ITS
//!   transcript when several sessions share one directory ([`session_of_pid`]);
//! * the session's transcript carries the model (`message.model`) and the
//!   effort (`effort`) of every turn ([`tail_facts`]) — but the transcript is
//!   the CONVERSATION's, shared by every process that resumed it, so only a
//!   row written since THIS process started speaks for it;
//! * the branch is read live from `.git/HEAD` ([`git_head`]), so a checkout
//!   shows at once rather than at the next turn.
//!
//! Nothing here is written anywhere, and nothing reaches into the user's
//! Claude settings — the retired "decision B" install is not revived.
//!
//! The permission MODE is not dropped with the row: when it is anything but
//! bypass, the footer keeps the vendor's own pill for it ([`plan_row`]), so
//! shift+tab into plan mode still shows. Only the steady-state bypass line —
//! the part the owner asked to hide — disappears.
//!
//! VERSION DRIFT. Claude Code is replaced under a running aterm, and a
//! session can be relaunched onto a newer build mid-tab (`harness::upgrade`).
//! Everything here reads the vendor's output as a THIRD PARTY's: a row this
//! planner does not recognise is left exactly as the vendor drew it, a file
//! whose shape moved yields no facts rather than guessed ones, and facts are
//! keyed to one PROCESS (its pid AND its start time, [`parse_session_entry`]),
//! so a relaunched build is a new process whose facts are read afresh — and a
//! `--resume`d one shows no model until it has answered, never the model of
//! the process before it ([`tail_facts`]).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use aterm_json::Value;

/// How much of a transcript's END is read for the latest model and effort.
/// One assistant turn with a large tool result can be hundreds of KiB, so the
/// tail is generous; a transcript whose last half-MiB holds no main-thread
/// assistant row simply shows no model until the next turn writes one.
pub const TAIL_BYTES: u64 = 512 * 1024;

/// Longest transcript line [`tail_facts`] will parse. A row past this is a
/// tool result, never a model or effort carrier worth a large parse.
const MAX_LINE_BYTES: usize = 256 * 1024;

/// Longest `sessions/<pid>.json` read: the file is a few hundred bytes.
const MAX_SESSION_FILE_BYTES: u64 = 64 * 1024;

/// How many directories [`git_head`] climbs looking for `.git`.
const MAX_GIT_CLIMB: usize = 64;

/// Longest `HEAD` or `.git` file read: both are one short line.
const MAX_GIT_FILE_BYTES: u64 = 4096;

/// Longest Claude `settings.json` read for the thinking setting.
const MAX_SETTINGS_BYTES: u64 = 256 * 1024;

/// The mark before model + effort.
pub const MODEL_MARK: char = '\u{25C6}'; // ◆
/// The mark before the repository.
pub const REPO_MARK: char = '\u{2302}'; // ⌂
/// The mark before the branch.
pub const BRANCH_MARK: char = '\u{2387}'; // ⎇

/// What one Claude Code process's registry file says about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEntry {
    /// The session id — the transcript's file stem.
    pub session_id: String,
    /// The directory the session runs in.
    pub cwd: PathBuf,
    /// The running build's version, as Claude Code records it (`2.1.283`),
    /// when the file carries one.
    pub version: Option<String>,
    /// The process's start (unix seconds), from the file's `procStart` — the
    /// floor [`tail_facts`] takes where the kernel's own start cannot be read.
    pub proc_start: Option<u64>,
    /// When the running IMAGE registered (unix seconds), from the file's
    /// `startedAt` (milliseconds, `Date.now()` at registration). Claude Code
    /// can relaunch itself in place — `execve` with `--resume` (its provider
    /// setup's restart, 2.1.283) — which keeps the pid AND the kernel's start
    /// time, while every image writes its own `startedAt`: the floor moves
    /// with the image ([`facts_for_pid`]).
    pub started_at: Option<u64>,
}

/// The facts the footer shows. Every one is optional: a fresh session has no
/// transcript row yet, a directory may not be a repository, and the footer
/// shows what it knows rather than a placeholder.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct FooterFacts {
    /// The model's display name (`Opus 5.5`).
    pub model: Option<String>,
    /// The effort level (`xhigh`).
    pub effort: Option<String>,
    /// The repository's directory name.
    pub repo: Option<String>,
    /// The checked-out branch, or a short commit id when HEAD is detached.
    pub branch: Option<String>,
    /// The session's thinking setting (`alwaysThinkingEnabled` in its Claude
    /// `settings.json`, on when absent) — not shown in the footer's text; the
    /// thinking light reads it (`harness::lights`).
    pub thinking: Option<bool>,
    /// The Claude Code build this process runs (`2.1.283`) — not shown; it
    /// names the vendor build in the host's drift log when a row it draws
    /// is not one this build reads.
    pub version: Option<String>,
}

/// One marked value in the footer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    /// The mark ([`MODEL_MARK`], [`REPO_MARK`], [`BRANCH_MARK`]).
    pub mark: char,
    /// The value after it.
    pub text: String,
}

/// The Claude Code directory of ONE process, from that process's own
/// environment at exec: `CLAUDE_CONFIG_DIR` when set, else `$HOME/.claude` —
/// Claude Code's rule. Never aterm's environment: aterm strips `CLAUDE_*`
/// from the shells it spawns, so its own value says nothing about a session's.
/// A relative `CLAUDE_CONFIG_DIR` resolves against a directory this cannot
/// know, so it answers `None` rather than a guess.
pub fn claude_dir_of(config_dir: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    if let Some(dir) = config_dir.filter(|d| !d.is_empty()) {
        let dir = PathBuf::from(dir);
        return dir.is_absolute().then_some(dir);
    }
    let home = PathBuf::from(home.filter(|h| !h.is_empty())?);
    home.is_absolute().then(|| home.join(".claude"))
}

/// Open `path` for reading only if it is a REGULAR file, judged on the open
/// handle (no check-then-open race). The open itself is non-blocking, so a
/// FIFO planted where a `HEAD` or a transcript should be cannot park the
/// reader thread forever.
fn open_regular(path: &Path) -> Option<File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path).ok()?;
    file.metadata().ok()?.is_file().then_some(file)
}

/// At most `cap` bytes of the regular file at `path`, as text.
fn read_small(path: &Path, cap: u64) -> Option<String> {
    let mut text = String::new();
    open_regular(path)?
        .take(cap)
        .read_to_string(&mut text)
        .ok()?;
    Some(text)
}

/// The registry entry for Claude Code process `pid`, or `None` when that pid
/// is not a Claude Code session (or its file cannot be read). `started` is the
/// kernel's start time of `pid` (unix seconds) where the caller could read it
/// — see [`parse_session_entry`].
pub fn session_of_pid(claude_dir: &Path, pid: u32, started: Option<u64>) -> Option<SessionEntry> {
    let path = claude_dir.join("sessions").join(format!("{pid}.json"));
    parse_session_entry(&read_small(&path, MAX_SESSION_FILE_BYTES)?, pid, started)
}

/// [`session_of_pid`]'s parse, pure. The file must be THIS process's, twice
/// over: it names the same `pid` it is filed under, and — when the caller
/// knows the process's start time and the file carries Claude Code's
/// `procStart` — the two name the same second. A registry row left behind by
/// a dead process whose pid was reused fails the second test even when it
/// passes the first.
pub fn parse_session_entry(text: &str, pid: u32, started: Option<u64>) -> Option<SessionEntry> {
    let value: Value = aterm_json::from_str(text).ok()?;
    let obj = value.as_object()?;
    if obj.get("pid").and_then(Value::as_u64) != Some(u64::from(pid)) {
        return None;
    }
    let recorded = obj.get("procStart").and_then(Value::as_str);
    if let (Some(started), Some(recorded)) = (started, recorded)
        && squash(recorded) != lstart_utc(started)
    {
        return None;
    }
    let session_id = obj.get("sessionId").and_then(Value::as_str)?;
    if session_id.is_empty()
        || !session_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return None;
    }
    let cwd = obj.get("cwd").and_then(Value::as_str)?;
    let version = obj
        .get("version")
        .and_then(Value::as_str)
        .filter(|v| is_word(v))
        .map(str::to_owned);
    Some(SessionEntry {
        session_id: session_id.to_owned(),
        cwd: PathBuf::from(cwd),
        version,
        proc_start: recorded.and_then(crate::harness::upgrade_models::parse_lstart),
        started_at: obj
            .get("startedAt")
            .and_then(Value::as_u64)
            .map(|ms| ms / 1000),
    })
}

/// Runs of whitespace collapsed to one space, ends trimmed.
fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A unix time as Claude Code records `procStart` — `ps -o lstart=` in UTC
/// and the C locale — whitespace-squashed: `Wed Sep 23 20:13:55 2026`.
pub fn lstart_utc(secs: u64) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (y, m, d) = aterm_types::rfc3339::civil_from_days(i64::try_from(days).unwrap_or(0));
    let weekday = DAYS[usize::try_from(days % 7).unwrap_or(0)];
    let month = MONTHS[usize::try_from(m - 1).unwrap_or(0).min(11)];
    format!(
        "{weekday} {month} {d} {:02}:{:02}:{:02} {y}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Claude Code's project directory name for `cwd`: every character that is
/// not ASCII alphanumeric becomes `-` (`/Users//ana/aterm` → `-Users-ana-aterm`).
pub fn project_slug(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// The transcript of `entry`: `<claude dir>/projects/<slug>/<id>.jsonl`, and
/// when the slug guess misses (a vendor spelling change), a scan of the
/// project directories for the id. `None` until the session writes one.
pub fn transcript_path(claude_dir: &Path, entry: &SessionEntry) -> Option<PathBuf> {
    let projects = claude_dir.join("projects");
    let file = format!("{}.jsonl", entry.session_id);
    let direct = projects.join(project_slug(&entry.cwd)).join(&file);
    if direct.is_file() {
        return Some(direct);
    }
    std::fs::read_dir(&projects)
        .ok()?
        .filter_map(Result::ok)
        .map(|dir| dir.path().join(&file))
        .find(|candidate| candidate.is_file())
}

/// The latest main-thread model and effort in the last [`TAIL_BYTES`] of the
/// transcript at `path`, written no earlier than `since` ([`tail_facts`]).
pub fn read_tail_facts(path: &Path, since: Option<u64>) -> Option<TailFacts> {
    let mut file = open_regular(path)?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::with_capacity(usize::try_from(len - start).unwrap_or(0));
    file.take(TAIL_BYTES).read_to_end(&mut bytes).ok()?;
    // A tail that starts mid-file starts mid-line: that first fragment is
    // never a whole row.
    let body = if start > 0 {
        match bytes.iter().position(|&b| b == b'\n') {
            Some(nl) => &bytes[nl + 1..],
            None => &[][..],
        }
    } else {
        &bytes[..]
    };
    Some(tail_facts(body, since))
}

/// What the end of a transcript says about the model and effort.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TailFacts {
    /// The model id of the latest main-thread assistant row (`claude-opus-5-5`).
    pub model_id: Option<String>,
    /// The latest `effort` any row carries (`xhigh`).
    pub effort: Option<String>,
}

/// The row Claude Code writes when a `/model` choice is MADE (2.1.201 through
/// 2.1.283): a `user` row whose content starts with this tag. A cancelled
/// picker writes none. Its result is prose, which this reader never parses.
const MODEL_COMMAND: &str = "<command-name>/model</command-name>";

/// [`read_tail_facts`]'s scan, pure: rows newest first, stopping once both
/// facts are known. SIDECHAIN rows (a subagent's turns) are skipped, and so is
/// a `<synthetic>` model — neither is the model this session is talking to.
///
/// `since` is the process's start (unix seconds). The transcript belongs to
/// the conversation, and a `--resume`d process appends to the file its
/// predecessors wrote — so a row stamped before `since` is a predecessor's,
/// and so is every row above it (the file is in append order, and everything
/// this process writes comes after its fork; the stamps themselves are not
/// monotonic, which is why the order and not the clock is the argument). The
/// scan stops there: before this process has answered, the footer shows no
/// model rather than the one it replaced. A row with no stamp is not
/// attributed to anyone. A `/model` choice made since the newest answer
/// voids both facts — the model is unknown until the next answer names it.
/// `None` (no start known at all) reads the tail as it is — the caller
/// decides whether that may be shown ([`facts_for_pid`] does not).
pub fn tail_facts(body: &[u8], since: Option<u64>) -> TailFacts {
    let mut facts = TailFacts::default();
    for raw in body.rsplit(|&b| b == b'\n') {
        if facts.model_id.is_some() && facts.effort.is_some() {
            break;
        }
        if raw.is_empty() || raw.len() > MAX_LINE_BYTES {
            continue;
        }
        let Ok(line) = std::str::from_utf8(raw) else {
            continue;
        };
        // Cheap prefilters: most rows carry neither fact, and a parse per row
        // over half a MiB of tool output is the cost worth skipping.
        let wants_model = facts.model_id.is_none() && line.contains("\"assistant\"");
        let wants_effort = facts.effort.is_none() && line.contains("\"effort\"");
        let may_switch = line.contains(MODEL_COMMAND);
        if !wants_model && !wants_effort && !may_switch {
            continue;
        }
        let Ok(value) = aterm_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(obj) = value.as_object() else {
            continue;
        };
        if obj.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        if let Some(since) = since {
            match obj
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(crate::harness::upgrade_models::parse_utc)
            {
                Some(at) if at < since => break,
                Some(_) => {}
                None => continue,
            }
        }
        if may_switch
            && obj.get("type").and_then(Value::as_str) == Some("user")
            && obj
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(Value::as_str)
                .is_some_and(|c| c.trim_start().starts_with(MODEL_COMMAND))
        {
            break;
        }
        if wants_effort
            && let Some(effort) = obj.get("effort").and_then(Value::as_str)
            && is_word(effort)
        {
            facts.effort = Some(effort.to_owned());
        }
        if wants_model && obj.get("type").and_then(Value::as_str) == Some("assistant") {
            let model = obj
                .get("message")
                .and_then(|m| m.get("model"))
                .and_then(Value::as_str);
            if let Some(model) = model.filter(|m| !m.starts_with('<') && is_word(m)) {
                facts.model_id = Some(model.to_owned());
            }
        }
    }
    facts
}

/// A value safe to paint as-is: short, and letters, digits, `-`, `.`, `_`,
/// `[`, `]` only — a transcript is written by a program, but it is still text
/// that ends up in a chrome row.
fn is_word(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '[' | ']'))
}

/// A model id's display name, the vendor's own spelling:
/// `claude-opus-5-5` → `Opus 5.5`, `claude-fable-5-1` → `Fable 5.1`,
/// `claude-sonnet-5` → `Sonnet 5`, `claude-haiku-4-5-20251001` → `Haiku 4.5`
/// (a trailing date is dropped), `claude-opus-5-5[1m]` → `Opus 5.5`. An id
/// that does not have that shape is shown as it is.
pub fn model_display(id: &str) -> String {
    let base = id.split('[').next().unwrap_or(id);
    let Some(rest) = base.strip_prefix("claude-") else {
        return id.to_owned();
    };
    let mut parts = rest.split('-');
    let Some(family) = parts
        .next()
        .filter(|f| f.chars().all(|c| c.is_ascii_alphabetic()))
    else {
        return id.to_owned();
    };
    let version: Vec<&str> = parts
        .filter(|p| p.len() < 8 && p.chars().all(|c| c.is_ascii_digit()))
        .collect();
    let mut name = String::with_capacity(family.len() + 6);
    let mut chars = family.chars();
    if let Some(first) = chars.next() {
        name.extend(first.to_uppercase());
        name.push_str(chars.as_str());
    }
    if !version.is_empty() {
        name.push(' ');
        name.push_str(&version.join("."));
    }
    name
}

/// The repository (its top directory's name) and the branch that `cwd` is in,
/// read straight from `.git` — no `git` process. A worktree's `.git` FILE
/// (`gitdir: …`) is followed; a detached HEAD shows its first seven hex
/// digits. `None` outside a repository.
pub fn git_head(cwd: &Path) -> Option<(String, String)> {
    let mut dir = cwd;
    for _ in 0..MAX_GIT_CLIMB {
        let dot_git = dir.join(".git");
        if let Some(git_dir) = resolve_git_dir(&dot_git) {
            let repo = printable(&dir.file_name()?.to_string_lossy())?;
            let head = read_small(&git_dir.join("HEAD"), MAX_GIT_FILE_BYTES)?;
            return Some((repo, branch_of_head(&head)?));
        }
        dir = dir.parent()?;
    }
    None
}

/// The git directory `.git` names: itself when it is a directory, the
/// `gitdir:` target when it is a worktree's file.
fn resolve_git_dir(dot_git: &Path) -> Option<PathBuf> {
    let meta = std::fs::metadata(dot_git).ok()?;
    if meta.is_dir() {
        return Some(dot_git.to_path_buf());
    }
    let text = read_small(dot_git, MAX_GIT_FILE_BYTES)?;
    let target = text.lines().next()?.strip_prefix("gitdir:")?.trim();
    let target = Path::new(target);
    Some(if target.is_absolute() {
        target.to_path_buf()
    } else {
        dot_git.parent()?.join(target)
    })
}

/// The branch a `HEAD` file names, pure.
pub fn branch_of_head(head: &str) -> Option<String> {
    let head = head.trim();
    if let Some(reference) = head.strip_prefix("ref:") {
        let reference = reference.trim();
        let name = reference.strip_prefix("refs/heads/").unwrap_or(reference);
        return printable(name);
    }
    (head.len() >= 7 && head.chars().all(|c| c.is_ascii_hexdigit())).then(|| head[..7].to_owned())
}

/// A name from the filesystem, fit for a chrome row: non-empty, at most 128
/// chars, and no control characters (a directory or ref name may hold any
/// byte but `/` and NUL).
fn printable(name: &str) -> Option<String> {
    (!name.is_empty() && name.chars().count() <= 128 && !name.chars().any(char::is_control))
        .then(|| name.to_owned())
}

/// Everything the footer shows for Claude Code process `pid`, from the files
/// named in the module header. Bounded reads only — one small JSON file, one
/// transcript tail, one `HEAD` — but still file I/O: callers keep it off the
/// event loop. `started` is the pid-reuse guard [`parse_session_entry`] takes,
/// and — with the registry's own `procStart` behind it, and the running
/// image's `startedAt` above both — the floor under the transcript rows that
/// may speak for this process ([`tail_facts`]): with no start known at all,
/// model and effort stay out rather than risk a predecessor's.
pub fn facts_for_pid(claude_dir: &Path, pid: u32, started: Option<u64>) -> Option<FooterFacts> {
    let entry = session_of_pid(claude_dir, pid, started)?;
    let floor = match (started.or(entry.proc_start), entry.started_at) {
        (Some(process), Some(image)) => Some(process.max(image)),
        (process, image) => process.or(image),
    };
    let tail = floor
        .and_then(|since| {
            transcript_path(claude_dir, &entry).and_then(|path| read_tail_facts(&path, Some(since)))
        })
        .unwrap_or_default();
    let (repo, branch) = match git_head(&entry.cwd) {
        Some((repo, branch)) => (Some(repo), Some(branch)),
        None => (None, None),
    };
    let thinking = read_small(&claude_dir.join("settings.json"), MAX_SETTINGS_BYTES)
        .as_deref()
        .map_or(Some(true), crate::harness::lights::thinking_setting);
    Some(FooterFacts {
        model: tail.model_id.as_deref().map(model_display),
        effort: tail.effort,
        repo,
        branch,
        thinking,
        version: entry.version,
    })
}

/// The footer's marked values, in order: model and effort under one mark,
/// then repository, then branch. A value the facts lack is left out, not
/// shown as a placeholder.
pub fn segments(facts: &FooterFacts) -> Vec<Segment> {
    let mut out = Vec::with_capacity(3);
    let model_effort = match (&facts.model, &facts.effort) {
        (Some(model), Some(effort)) => Some(format!("{model} {effort}")),
        (Some(one), None) | (None, Some(one)) => Some(one.clone()),
        (None, None) => None,
    };
    if let Some(text) = model_effort {
        out.push(Segment {
            mark: MODEL_MARK,
            text,
        });
    }
    if let Some(repo) = &facts.repo {
        out.push(Segment {
            mark: REPO_MARK,
            text: repo.clone(),
        });
    }
    if let Some(branch) = &facts.branch {
        out.push(Segment {
            mark: BRANCH_MARK,
            text: branch.clone(),
        });
    }
    out
}

/// Where, in a screen of `rows`, Claude Code's permission-mode row is: under
/// the composer's bottom rule (`aterm_phase::phase::composer_bottom`), before
/// the first blank row, the one that opens with a mode pill. Found by
/// STRUCTURE — a column-0 composer framed by full-width rules is only ever
/// Claude Code's — and never by the `(shift+tab to cycle)` hint, whose key a
/// user can rebind.
pub fn mode_row(rows: &[String]) -> Option<usize> {
    let bottom = aterm_phase::phase::composer_bottom(rows)?;
    (bottom + 1..rows.len())
        .take_while(|&i| !rows[i].trim().is_empty())
        .find(|&i| plan_row(&rows[i]).is_some())
}

/// The vendor's mode pills: glyph, indicator. `⏸` goes only with plan and
/// manual mode, `⏵⏵` with the rest — the pairing the 2.1.282 mode table draws.
const PILLS: [(&str, &str); 6] = [
    ("\u{23F5}\u{23F5}", "bypass permissions"),
    ("\u{23F5}\u{23F5}", "auto mode"),
    ("\u{23F5}\u{23F5}", "accept edits"),
    ("\u{23F5}\u{23F5}", "don't ask"),
    ("\u{23F8}", "plan mode"),
    ("\u{23F8}", "manual mode"),
];

/// The permission-mode indicator (`auto mode`) of the pill `row` opens with,
/// or `None` when it opens with none — the one reading of the pill the footer
/// and the lights (`harness::lights`) share.
#[must_use]
pub fn pill_indicator(row: &str) -> Option<&'static str> {
    let rest = row.strip_prefix("  ")?;
    PILLS
        .iter()
        .find(|(glyph, indicator)| rest.starts_with(&format!("{glyph} {indicator} on")))
        .map(|(_, indicator)| *indicator)
}

/// Whether `row` OPENS like a mode pill — a pill glyph after the indent —
/// whatever its indicator says. With [`pill_indicator`] answering `None`,
/// that is a pill this build does not know: the vendor has renamed or added a
/// mode, and the host logs it (`claude_footer`'s drift note) instead of
/// letting the lights go quietly grey.
#[must_use]
pub fn opens_with_pill_glyph(row: &str) -> bool {
    row.strip_prefix("  ")
        .is_some_and(|rest| PILLS.iter().any(|(glyph, _)| rest.starts_with(glyph)))
}

/// Whether `row` is a KNOWN pill cut short — a narrow pane shows only the
/// head of `⏵⏵ bypass permissions on`, e.g. `⏵⏵ bypass permissi`. That is
/// not the vendor's drift; it is geometry.
#[must_use]
pub fn is_cut_pill(row: &str) -> bool {
    let Some(rest) = row.strip_prefix("  ") else {
        return false;
    };
    let rest = rest.trim_end();
    !rest.is_empty()
        && PILLS
            .iter()
            .any(|(glyph, indicator)| format!("{glyph} {indicator} on").starts_with(rest))
}

/// The hints the footer drops: static key help, not state. Everything else on
/// the row — `esc to interrupt`, `1 shell`, a limit notice, `← 2 agents` — is
/// live status and stays, in the vendor's own cells.
const STATIC_HINTS: [&str; 2] = ["\u{2190} for agents", "? for shortcuts"];

/// One piece of the rewritten row, in painting order. Ranges are CHAR columns
/// of the vendor row (the mode row is single-width text), so the painter can
/// copy those cells with their own colours.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    /// The footer's three marked values ([`segments`]).
    Footer,
    /// A mode pill that is not bypass, kept as the vendor drew it.
    Mode(std::ops::Range<usize>),
    /// A live-status item, kept as the vendor drew it, after a ` · `.
    Item(std::ops::Range<usize>),
    /// Right-aligned text past a wide gap (`/rc active`), kept in place.
    Tail(std::ops::Range<usize>),
}

/// How to rewrite the mode row `row`, or `None` when it is not one. The plan
/// always opens with [`Piece::Footer`]; the bypass pill, its
/// `(<key> to cycle)` hint and the [`STATIC_HINTS`] are what it leaves out.
///
/// Only a row that OPENS WITH A PILL is planned. The pill-less row — default
/// mode's `? for shortcuts`, which becomes `esc to clear` the moment the
/// composer holds text — is left to the vendor: rewriting it would make the
/// footer blink with every first keystroke, and a footer with no pill must
/// keep meaning exactly one thing, bypass.
pub fn plan_row(row: &str) -> Option<Vec<Piece>> {
    let chars: Vec<char> = row.chars().collect();
    let text = |r: std::ops::Range<usize>| chars[r].iter().collect::<String>();
    if chars.len() < 3 || chars[0] != ' ' || chars[1] != ' ' || chars[2] == ' ' {
        return None;
    }
    let mut at = 2;
    let mut plan = vec![Piece::Footer];
    // The pill: `<glyph> <indicator> on`.
    let rest: String = chars[at..].iter().collect();
    let (len, indicator) = PILLS.iter().find_map(|(glyph, indicator)| {
        let head = format!("{glyph} {indicator} on");
        rest.starts_with(&head)
            .then(|| (head.chars().count(), *indicator))
    })?;
    if indicator != "bypass permissions" {
        plan.push(Piece::Mode(at..at + len));
    }
    at += len;
    // The dim `(<key> to cycle)` hint that follows a pill.
    let after: String = chars[at..].iter().collect();
    if let Some(hint) = after.strip_prefix(" (") {
        let close = hint.find(')')?;
        if hint[..close].ends_with("to cycle") {
            at += 2 + hint[..=close].chars().count();
        }
    }
    // Items: ` · item` until a wide gap; then a right-aligned tail.
    while at < chars.len() {
        if !chars[at..].starts_with(&[' ', '\u{00B7}', ' ']) {
            break;
        }
        let start = at + 3;
        let mut end = start;
        while end < chars.len()
            && !chars[end..].starts_with(&[' ', '\u{00B7}', ' '])
            && !chars[end..].starts_with(&[' ', ' ', ' '])
        {
            end += 1;
        }
        let item = text(start..end);
        if !item.is_empty() && !STATIC_HINTS.contains(&item.as_str()) {
            plan.push(Piece::Item(start..end));
        }
        at = end;
    }
    let tail_start = (at..chars.len()).find(|&i| chars[i] != ' ');
    if let Some(ts) = tail_start {
        plan.push(Piece::Tail(ts..chars.len()));
    }
    Some(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_ids_read_as_the_vendor_spells_them() {
        assert_eq!(model_display("claude-opus-5-5"), "Opus 5.5");
        assert_eq!(model_display("claude-opus-5"), "Opus 5");
        assert_eq!(model_display("claude-fable-5-1"), "Fable 5.1");
        assert_eq!(model_display("claude-sonnet-5"), "Sonnet 5");
        assert_eq!(model_display("claude-haiku-4-5-20251001"), "Haiku 4.5");
        assert_eq!(model_display("claude-opus-5-5[1m]"), "Opus 5.5");
        assert_eq!(model_display("gpt-5-codex"), "gpt-5-codex");
    }

    #[test]
    fn a_registry_entry_must_name_the_pid_it_is_filed_under() {
        let text =
            r#"{"pid":7214,"sessionId":"28b6a7bd-7021","cwd":"/Users//ana/aterm","status":"busy"}"#;
        assert_eq!(
            parse_session_entry(text, 7214, None),
            Some(SessionEntry {
                session_id: "28b6a7bd-7021".into(),
                cwd: PathBuf::from("/Users//ana/aterm"),
                version: None,
                proc_start: None,
                started_at: None,
            })
        );
        assert_eq!(
            parse_session_entry(text, 9999, None),
            None,
            "a stale row left by a reused pid is not this process's"
        );
        let hostile = r#"{"pid":7,"sessionId":"../../etc/passwd","cwd":"/"}"#;
        assert_eq!(
            parse_session_entry(hostile, 7, None),
            None,
            "the id becomes a file name"
        );
    }

    #[test]
    fn the_slug_matches_claude_codes_project_directories() {
        assert_eq!(project_slug(Path::new("/home/dev/proj")), "-home-dev-proj");
        assert_eq!(project_slug(Path::new("/tmp/a.b c")), "-tmp-a-b-c");
    }

    #[test]
    fn the_tail_takes_the_newest_main_thread_model_and_effort() {
        let body = concat!(
            r#"{"type":"assistant","effort":"high","message":{"model":"claude-opus-5"}}"#,
            "\n",
            r#"{"type":"assistant","effort":"xhigh","message":{"model":"claude-opus-5-5"}}"#,
            "\n",
            r#"{"type":"assistant","isSidechain":true,"effort":"low","message":{"model":"claude-haiku-4-5"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"model":"<synthetic>"}}"#,
            "\n",
            r#"{"type":"user","effort":"xhigh"}"#,
            "\n",
        );
        let facts = tail_facts(body.as_bytes(), None);
        assert_eq!(facts.model_id.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(facts.effort.as_deref(), Some("xhigh"));
        assert_eq!(
            tail_facts(b"", None),
            TailFacts::default(),
            "a fresh session knows neither"
        );
    }

    /// A `--resume`d process appends to its predecessors' transcript: the
    /// structure of the 2.1.278 → 2.1.280 boundary of a real conversation
    /// (an Opus 5 answer, the new process's start, then its Opus 5.5 answer).
    #[test]
    fn a_resumed_process_shows_no_model_until_it_answers() {
        let old = r#"{"type":"assistant","timestamp":"2026-09-22T16:44:57.101Z","effort":"high","message":{"model":"claude-opus-5"}}"#;
        let own = r#"{"type":"assistant","timestamp":"2026-09-24T04:15:32.412Z","effort":"xhigh","message":{"model":"claude-opus-5-5"}}"#;
        let start = crate::harness::upgrade_models::parse_utc("2026-09-24T04:13:26Z").unwrap();
        let before = format!("{old}\n");
        assert_eq!(
            tail_facts(before.as_bytes(), Some(start)),
            TailFacts::default(),
            "the previous process's answer does not speak for this one"
        );
        // Negative control: without the floor, the stale model is read.
        assert_eq!(
            tail_facts(before.as_bytes(), None).model_id.as_deref(),
            Some("claude-opus-5")
        );
        assert_eq!(
            tail_facts(before.as_bytes(), Some(start - 3 * 86_400))
                .model_id
                .as_deref(),
            Some("claude-opus-5"),
            "a row written after the start is this process's"
        );
        let after = format!("{old}\n{own}\n");
        let facts = tail_facts(after.as_bytes(), Some(start));
        assert_eq!(facts.model_id.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(facts.effort.as_deref(), Some("xhigh"));
    }

    /// A `/model` choice made since the newest answer: the model is unknown
    /// until the next answer names it. Only the command row itself counts.
    #[test]
    fn a_model_choice_since_the_last_answer_voids_model_and_effort() {
        let answer = r#"{"type":"assistant","timestamp":"2026-09-24T04:13:30.000Z","effort":"xhigh","message":{"model":"claude-opus-5"}}"#;
        let command = r#"{"type":"user","timestamp":"2026-09-24T04:13:31.000Z","message":{"role":"user","content":"<command-name>/model</command-name>\n            <command-message>model</command-message>\n            <command-args></command-args>"}}"#;
        let next = r#"{"type":"assistant","timestamp":"2026-09-24T04:15:32.000Z","effort":"xhigh","message":{"model":"claude-opus-5-5"}}"#;
        let start = crate::harness::upgrade_models::parse_utc("2026-09-24T04:13:26Z");
        assert_eq!(
            tail_facts(format!("{answer}\n{command}\n").as_bytes(), start),
            TailFacts::default()
        );
        // Negative control: the same tail without the command row.
        assert_eq!(
            tail_facts(format!("{answer}\n").as_bytes(), start)
                .model_id
                .as_deref(),
            Some("claude-opus-5")
        );
        assert_eq!(
            tail_facts(format!("{answer}\n{command}\n{next}\n").as_bytes(), start)
                .model_id
                .as_deref(),
            Some("claude-opus-5-5"),
            "the answer after the choice names the new model"
        );
        // The tag quoted anywhere but at the start of a person's row is text.
        for quoted in [
            r#"{"type":"assistant","timestamp":"2026-09-24T04:13:31.000Z","message":{"model":"<synthetic>","content":"<command-name>/model</command-name>"}}"#,
            r#"{"type":"user","timestamp":"2026-09-24T04:13:31.000Z","message":{"content":[{"type":"tool_result","content":"<command-name>/model</command-name>"}]}}"#,
            r#"{"type":"user","timestamp":"2026-09-24T04:13:31.000Z","message":{"content":"why does <command-name>/model</command-name> exist"}}"#,
        ] {
            assert_eq!(
                tail_facts(format!("{answer}\n{quoted}\n").as_bytes(), start)
                    .model_id
                    .as_deref(),
                Some("claude-opus-5"),
                "{quoted}"
            );
        }
    }

    #[test]
    fn an_unstamped_row_is_not_attributed() {
        let unstamped =
            r#"{"type":"assistant","effort":"xhigh","message":{"model":"claude-opus-5-5"}}"#;
        let stamped = r#"{"type":"assistant","timestamp":"2026-09-24T04:15:32.000Z","effort":"xhigh","message":{"model":"claude-opus-5-5"}}"#;
        let start = crate::harness::upgrade_models::parse_utc("2026-09-24T04:13:26Z");
        assert_eq!(
            tail_facts(format!("{unstamped}\n").as_bytes(), start),
            TailFacts::default()
        );
        assert_eq!(
            tail_facts(format!("{stamped}\n").as_bytes(), start)
                .model_id
                .as_deref(),
            Some("claude-opus-5-5")
        );
    }

    /// The floor is the kernel's start, else the registry's `procStart` (the
    /// path where the kernel cannot be asked); with neither, no model.
    #[test]
    fn facts_for_pid_floors_at_the_kernel_else_the_registry_start() {
        let root = std::env::temp_dir().join(format!("aterm-footer-floor-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let cwd = root.join("work");
        std::fs::create_dir_all(&cwd).unwrap();
        let claude = root.join("claude");
        std::fs::create_dir_all(claude.join("sessions")).unwrap();
        let project = claude.join("projects").join(project_slug(&cwd));
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join("0b5e7a11-f100.jsonl"),
            concat!(
                r#"{"type":"assistant","timestamp":"2026-09-22T16:44:57.000Z","effort":"high","message":{"model":"claude-opus-5"}}"#,
                "\n"
            ),
        )
        .unwrap();
        let start = crate::harness::upgrade_models::parse_utc("2026-09-24T04:13:26Z").unwrap();
        let entry = |proc_start: Option<u64>| {
            let stamp = proc_start.map_or(String::new(), |s| {
                format!(r#","procStart":"{}""#, lstart_utc(s))
            });
            let text = format!(
                r#"{{"pid":4242,"sessionId":"0b5e7a11-f100","cwd":"{}"{stamp}}}"#,
                cwd.display()
            );
            std::fs::write(claude.join("sessions/4242.json"), text).unwrap();
        };
        let model =
            |started: Option<u64>| facts_for_pid(&claude, 4242, started).and_then(|f| f.model);
        entry(Some(start));
        assert_eq!(model(Some(start)), None, "the kernel's start floors it");
        assert_eq!(model(None), None, "the registry's start floors it");
        entry(None);
        assert_eq!(model(None), None, "no start known: no model");
        // Negative control: a floor older than the answer reads it.
        entry(Some(start - 3 * 86_400));
        assert_eq!(model(None).as_deref(), Some("Opus 5"));
        // An image exec'd in place (same pid, same kernel start) registers
        // its own `startedAt`: the floor moves with it.
        let old_start = start - 3 * 86_400;
        std::fs::write(
            claude.join("sessions/4242.json"),
            format!(
                r#"{{"pid":4242,"sessionId":"0b5e7a11-f100","cwd":"{}","procStart":"{}","startedAt":{}}}"#,
                cwd.display(),
                lstart_utc(old_start),
                start * 1000 + 250
            ),
        )
        .unwrap();
        assert_eq!(
            model(Some(old_start)),
            None,
            "the pre-exec image's answer does not speak for the new one"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_head_names_its_branch_or_a_short_commit() {
        assert_eq!(
            branch_of_head("ref: refs/heads/main\n").as_deref(),
            Some("main")
        );
        assert_eq!(
            branch_of_head("ref: refs/heads/feat/footer\n").as_deref(),
            Some("feat/footer")
        );
        assert_eq!(
            branch_of_head("4a2573a96c0ffee4a2573a96c0ffee4a2573a96c\n").as_deref(),
            Some("4a2573a")
        );
        assert_eq!(branch_of_head("garbage"), None);
    }

    #[test]
    fn git_head_follows_a_worktree_file_and_climbs_to_the_top() {
        let root = std::env::temp_dir().join(format!("aterm-footer-git-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let main = root.join("repo");
        std::fs::create_dir_all(main.join(".git")).unwrap();
        std::fs::write(main.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::create_dir_all(main.join("crates/deep")).unwrap();
        assert_eq!(
            git_head(&main.join("crates/deep")),
            Some(("repo".to_owned(), "main".to_owned()))
        );
        let wt_git = root.join("gitdirs/wt");
        std::fs::create_dir_all(&wt_git).unwrap();
        std::fs::write(wt_git.join("HEAD"), "ref: refs/heads/feature\n").unwrap();
        let wt = root.join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".git"), format!("gitdir: {}\n", wt_git.display())).unwrap();
        assert_eq!(git_head(&wt), Some(("wt".to_owned(), "feature".to_owned())));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_footer_is_three_marks_in_order_and_skips_what_it_lacks() {
        let full = FooterFacts {
            model: Some("Opus 5.5".into()),
            effort: Some("xhigh".into()),
            repo: Some("aterm".into()),
            branch: Some("main".into()),
            thinking: None,
            version: None,
        };
        assert_eq!(
            segments(&full),
            vec![
                Segment {
                    mark: MODEL_MARK,
                    text: "Opus 5.5 xhigh".into()
                },
                Segment {
                    mark: REPO_MARK,
                    text: "aterm".into()
                },
                Segment {
                    mark: BRANCH_MARK,
                    text: "main".into()
                },
            ]
        );
        let fresh = FooterFacts {
            repo: Some("aterm".into()),
            branch: Some("main".into()),
            ..FooterFacts::default()
        };
        assert_eq!(segments(&fresh).len(), 2, "no model until the first turn");
    }

    /// The rows are verbatim from the phase fixtures and the 2.1.282 probe.
    #[test]
    fn the_bypass_pill_and_its_hints_go_and_live_status_stays() {
        let bypass = "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle) \u{00B7} \u{2190} for agents";
        assert_eq!(plan_row(bypass), Some(vec![Piece::Footer]));
        let busy = "  \u{23F5}\u{23F5} bypass permissions on \u{00B7} 1 monitor \u{00B7} \u{2190} for agents \u{00B7} \u{2193} to manage";
        let plan = plan_row(busy).unwrap();
        let kept: Vec<String> = plan
            .iter()
            .filter_map(|p| match p {
                Piece::Item(r) => Some(busy.chars().skip(r.start).take(r.len()).collect()),
                _ => None,
            })
            .collect();
        assert_eq!(kept, ["1 monitor", "\u{2193} to manage"]);
        let auto = "  \u{23F5}\u{23F5} auto mode on (shift+tab to cycle) \u{00B7} esc to interrupt \u{00B7} \u{2190} for agents        /rc active";
        let plan = plan_row(auto).unwrap();
        assert!(
            matches!(plan[1], Piece::Mode(_)),
            "a non-bypass mode keeps its pill: {plan:?}"
        );
        assert!(plan.iter().any(|p| matches!(p, Piece::Item(r) if auto.chars().skip(r.start).take(r.len()).collect::<String>() == "esc to interrupt")));
        assert!(
            matches!(plan.last(), Some(Piece::Tail(_))),
            "the right-aligned tail stays: {plan:?}"
        );
        let plan_mode = "  \u{23F8} plan mode on (shift+tab to cycle)";
        assert!(matches!(plan_row(plan_mode).unwrap()[1], Piece::Mode(_)));
        let manual = "  \u{23F8} manual mode on (shift+tab to cycle)";
        assert!(
            matches!(plan_row(manual).unwrap()[1], Piece::Mode(_)),
            "manual mode is a mode the footer shows, never reads as bypass"
        );
    }

    /// The pill-less row is the vendor's: default mode's hint turns into
    /// `esc to clear` as soon as the composer holds text, so painting over it
    /// would blink the footer on and off with typing — and a footer without a
    /// pill must keep meaning bypass.
    #[test]
    fn a_row_without_a_pill_is_left_to_the_vendor() {
        assert_eq!(plan_row("  ? for shortcuts"), None);
        assert_eq!(plan_row("  esc to clear"), None);
        assert_eq!(plan_row("  ! for shell mode"), None);
    }

    /// `procStart` is `ps -o lstart=` in UTC; the guard compares it to the
    /// kernel's start time rendered the same way.
    #[test]
    fn a_reused_pid_is_refused_by_its_start_time() {
        // 1790194435 = Wed Sep 23 2026 20:13:55 UTC (the SESSION_9162 fixture).
        assert_eq!(lstart_utc(1_790_194_435), "Wed Sep 23 20:13:55 2026");
        assert_eq!(lstart_utc(0), "Thu Jan 1 00:00:00 1970");
        let text = r#"{"pid":9162,"sessionId":"03396a15-856e","cwd":"/w","procStart":"Thu Sep  3 20:13:55 2026"}"#;
        let sep3 = 1_790_194_435 - 20 * 86_400;
        assert!(
            parse_session_entry(text, 9162, Some(sep3)).is_some(),
            "`ps` pads a one-digit day; the compare squashes it"
        );
        assert_eq!(
            parse_session_entry(text, 9162, Some(sep3 + 1)),
            None,
            "a different process that holds the same pid"
        );
        assert!(
            parse_session_entry(text, 9162, None).is_some(),
            "no kernel answer: the pid match stands alone"
        );
        assert_eq!(
            parse_session_entry(text, 9162, None).and_then(|e| e.proc_start),
            Some(sep3),
            "the registry's start is kept as the transcript floor"
        );
    }

    #[test]
    fn the_claude_dir_is_the_processs_own() {
        assert_eq!(
            claude_dir_of(Some("/cfg/claude"), Some("/Users//ana")),
            Some(PathBuf::from("/cfg/claude"))
        );
        assert_eq!(
            claude_dir_of(Some(""), Some("/Users//ana")),
            Some(PathBuf::from("/Users//ana/.claude")),
            "an empty override is no override"
        );
        assert_eq!(claude_dir_of(Some("rel/dir"), Some("/Users//ana")), None);
        assert_eq!(claude_dir_of(None, None), None);
    }

    /// A FIFO where `HEAD` should be must not park the reader thread.
    #[cfg(unix)]
    #[test]
    fn a_fifo_is_not_read() {
        let dir = std::env::temp_dir().join(format!("aterm-footer-fifo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fifo = dir.join("HEAD");
        let c = std::ffi::CString::new(fifo.to_string_lossy().as_bytes()).unwrap();
        // SAFETY: a NUL-terminated path we own; mkfifo only creates a node.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        assert_eq!(read_small(&fifo, MAX_GIT_FILE_BYTES), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A pill glyph with an indicator this build does not know is the
    /// vendor's drift, and is told apart from rows that are no pill at all.
    #[test]
    fn a_renamed_pill_still_opens_with_a_pill_glyph() {
        let renamed = "  \u{23F5}\u{23F5} turbo mode on (shift+tab to cycle)";
        assert!(opens_with_pill_glyph(renamed));
        assert_eq!(pill_indicator(renamed), None);
        assert!(opens_with_pill_glyph("  \u{23F8} manual mode on"));
        assert!(!opens_with_pill_glyph("  ? for shortcuts"));
        assert!(!opens_with_pill_glyph(
            "    \u{23F5}\u{23F5} indented tool output"
        ));
        // A narrow pane's cut of a known pill is geometry, not drift.
        assert!(is_cut_pill("  \u{23F5}\u{23F5} bypass permissi"));
        assert!(is_cut_pill("  \u{23F5}\u{23F5} bypass permissions o"));
        assert!(!is_cut_pill(renamed));
    }

    #[test]
    fn nothing_else_is_a_mode_row() {
        assert_eq!(
            plan_row("\u{276F} Try \"refactor gpu_matches_cpu.rs\""),
            None
        );
        assert_eq!(plan_row("  bypass permissions on"), None, "no pill glyph");
        assert_eq!(
            plan_row("    \u{23F5}\u{23F5} bypass permissions on"),
            None,
            "tool output is indented deeper"
        );
        assert_eq!(
            plan_row("  \u{23F8} auto mode on"),
            None,
            "the glyph pairs with its indicator"
        );
    }

    /// EVERY RECORDED CLAUDE CODE SCREEN in `aterm-phase`'s fixtures (measured
    /// sessions and the 2.1.280 box captures): a screen with a composer and a
    /// mode row gets a plan whose kept pieces are live status, never the bypass
    /// pill or a static hint; a screen with a box up (no composer) gets none.
    #[test]
    fn every_recorded_screen_plans_honestly() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../aterm-phase/src/fixtures");
        let mut planned = 0;
        for entry in std::fs::read_dir(&dir).expect("the phase fixtures") {
            let path = entry.unwrap().path();
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let rows: Vec<String> = text.lines().map(str::to_owned).collect();
            let Some(i) = mode_row(&rows) else {
                continue;
            };
            let plan = plan_row(&rows[i]).expect("mode_row only returns planned rows");
            assert_eq!(plan[0], Piece::Footer, "{}", path.display());
            for piece in &plan[1..] {
                let (Piece::Item(r) | Piece::Mode(r) | Piece::Tail(r)) = piece else {
                    panic!("one footer per row: {plan:?}");
                };
                let kept: String = rows[i].chars().skip(r.start).take(r.len()).collect();
                assert!(
                    !kept.contains("bypass permissions"),
                    "{}: {kept:?}",
                    path.display()
                );
                assert!(
                    !STATIC_HINTS.contains(&kept.as_str()),
                    "{}: {kept:?}",
                    path.display()
                );
            }
            planned += 1;
        }
        assert!(
            planned >= 10,
            "the corpus must exercise the planner: {planned}"
        );
    }

    /// Found under the composer's bottom rule only — a quoted copy of the row
    /// in the conversation above the frame is not the footer.
    #[test]
    fn the_mode_row_is_found_under_the_composer_and_nowhere_else() {
        let rule = "\u{2500}".repeat(40);
        let quoted = "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle)";
        let screen: Vec<String> = vec![
            quoted.into(),
            String::new(),
            rule.clone(),
            "\u{276F} ".into(),
            rule.clone(),
            "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle) \u{00B7} \u{2190} for agents".into(),
        ];
        assert_eq!(mode_row(&screen), Some(5));
        let no_frame: Vec<String> = vec![quoted.into(), "$ ".into()];
        assert_eq!(mode_row(&no_frame), None, "no composer, no footer");
        let with_statusline: Vec<String> = vec![
            rule.clone(),
            "\u{276F} ".into(),
            rule,
            "  my statusline".into(),
            "  \u{23F5}\u{23F5} auto mode on".into(),
        ];
        assert_eq!(
            mode_row(&with_statusline),
            Some(4),
            "a statusLine row may sit between"
        );
    }
}
