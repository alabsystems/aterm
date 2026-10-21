// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE SURVEY (`survey`, `docs/DESIGN-sessions-as-subagents-2026-10-15.md`
//! §3): one record per session of this instance — what it runs, what state
//! that is in, who asked it what, what it last said and what it is doing —
//! so a Claude Code session can read every aterm session and say what is
//! happening without ~4 KB of raw screen each.
//!
//! IT READS ONLY WHAT THE SERVER ALREADY PUBLISHES: the session timeline's
//! agent verdict and gauges (the status sweep's), the Claude Code footer's
//! facts and its excerpts (the footer resolver's own tail read,
//! [`ClaudeExcerpts`]), the task record, the session's metadata and turn
//! ledger, and — for a session whose words no transcript excerpt gives (a
//! shell, or an agent other than Claude Code) — its engine's executing
//! command and last rows, under a `try_lock` (a contended engine answers
//! `-`, never a wait). No transcript is read per request.
//!
//! Two levels ([`SurveyLevel`]): FACTS (words, counts and ages: the program
//! and its kind, the agent word and how long it has held it, how long since
//! a person typed, the task record, model, effort, context left, the
//! executing command's word, the tool in flight by name, supervisor,
//! whether attention is raised, and the role `meta set role` stamped when it
//! is a label — one short word, [`role_token`] — redacted: no other text a
//! person or a program wrote) and
//! CONTENT (the user title — the tab name a person set — the OSC title, cwd,
//! branch, the prompt, what the agent said, a shell's command line and last
//! rows, the attention line's words, a role that is no label). The local
//! Owner gets both; a fleet peer (`Scope::Fleet`, the fleet listener's) is
//! answered at the level its machine allows (`[fleet] allow`), never more
//! ([`cmd_survey`]'s `most`).
//!
//! EVERY EXCERPT is capped ([`excerpt::EXCERPT_BYTES`]) and redacted
//! ([`redact`]): a credential-shaped word and a fleet join code — with or
//! without its `atf1-`, its groups in one word or several, on one line or
//! more — are replaced (and the harness caps its excerpts never inside a
//! code, [`excerpt::cap`], so a code reaches the redaction whole or not at
//! all), and a line the privacy check (`redaction`) still finds sensitive is
//! withheld whole. The client escapes what it prints and labels it as text
//! from those sessions (`aterm ctl survey`).
//!
//! THE PROMPT is a tasked session's task (the head of the file its worker's
//! environment names, read once per process by the footer resolver), else
//! the newest prompt in its transcript that the window harness did not type:
//! the turn ledger records the harness's own turns (`by=harness`), and a
//! transcript prompt whose key ([`excerpt::prompt_key`]) one of their texts
//! has is passed over.
//!
//! AGES count through sleep (`AgentPublication::age_ms`,
//! `HumanInputStamp::age_ms`; the task record's wall-clock stamp), so a
//! laptop that slept reports the time that passed.

use std::fmt::Write as _;
use std::path::{Component, Path};

use aterm_agent::harness::excerpt::{self, Excerpts};

use crate::control::pct_encode;
use crate::session_store::{SessionHandle, Store};

mod redaction;

/// The excerpts the footer resolver publishes for a Claude Code process
/// (`SessionTimeline::set_claude_excerpts`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ClaudeExcerpts {
    /// What the tail read of its transcript decided.
    pub(crate) excerpts: Excerpts,
    /// The head of its task, for a worker its environment names a task file
    /// for ([`task_prompt_of_env`]).
    pub(crate) task: Option<String>,
    /// The transcript's length at that read
    /// (`footer::TailCache::transcript_len`): it grows as the conversation is
    /// written, the held deadline's transcript evidence
    /// (`harness_host::look_at_startups`).
    pub(crate) transcript_len: Option<u64>,
}

/// Which of a session's fields a survey answers with ([the module](self)).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SurveyLevel {
    /// Words, counts and ages only.
    Facts,
    /// The facts and the excerpts.
    Content,
}

impl SurveyLevel {
    /// The wire word (`level=`).
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Facts => "facts",
            Self::Content => "content",
        }
    }
}

/// The variable a `spawn agent=` worker's shell names its task file in
/// (§2.1), inherited by the agent it starts.
pub(crate) const TASK_FILE_ENV: &str = "ATERM_TASK_FILE";

/// The most of a task file read for its head.
const TASK_HEAD_BYTES: u64 = 4096;

/// How many of a shell's last non-blank rows the survey quotes.
const SHELL_ROWS: usize = 3;

/// The head of the task a worker was started with, folded and capped: the
/// file `env` names in [`TASK_FILE_ENV`] — only a regular file directly or
/// deeper under `<state>/tasks/`, named by a plain absolute path (no `..`, no
/// symlink at the end) — read for at most [`TASK_HEAD_BYTES`] and redacted.
/// `None` for no such file.
pub(crate) fn task_prompt_of_env(env: &[String]) -> Option<String> {
    let path = env
        .iter()
        .rev()
        .find_map(|kv| kv.strip_prefix(TASK_FILE_ENV)?.strip_prefix('='))?;
    task_prompt_at(Path::new(path), &crate::task_record::tasks_dir()?)
}

/// [`task_prompt_of_env`] for the file at `path`, which must be under `tasks`.
/// A plain absolute path is its root (on Windows, after its drive or share:
/// `C:\…`) and names, nothing else.
fn task_prompt_at(path: &Path, tasks: &Path) -> Option<String> {
    use std::io::Read as _;
    let plain = path.is_absolute()
        && path.components().all(|c| {
            matches!(
                c,
                Component::Prefix(_) | Component::RootDir | Component::Normal(_)
            )
        });
    if !plain || !path.starts_with(tasks) || path == tasks {
        return None;
    }
    if !std::fs::symlink_metadata(path).ok()?.is_file() {
        return None;
    }
    let mut head = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(TASK_HEAD_BYTES)
        .read_to_end(&mut head)
        .ok()?;
    let text = String::from_utf8_lossy(&head);
    // A head cut at its bound may end inside a word: that word goes, as the
    // excerpt cap drops one, so the redaction judges only whole ones.
    let text = if head.len() as u64 == TASK_HEAD_BYTES {
        text.trim_end_matches(|c: char| !c.is_whitespace())
    } else {
        text.as_ref()
    };
    let folded = excerpt::fold(text);
    // Redacted whole, then capped, as every excerpt is (`excerpt_token`).
    (!folded.is_empty()).then(|| excerpt::cap(&redact(&folded), excerpt::EXCERPT_BYTES))
}

/// What stands for a word the redaction replaced.
const REDACTED_WORD: &str = "[redacted]";

/// What stands for a line the privacy check withholds whole.
const REDACTED_LINE: &str = "[redacted: sensitive]";

/// The fleet code's prefix (`aterm fleet init`): never quoted, whatever
/// follows it.
const FLEET_CODE: &str = "atf1-";

/// `text` with what must not leave the session replaced: each token — split
/// where the privacy check's token-shape rule splits
/// ([`redaction::is_token_break`]) — that carries a fleet code
/// ([`FLEET_CODE`], from the code on) or is credential-shaped
/// ([`redaction::looks_like_raw_credential`], with a path's segments each
/// judged on its own, [`redact_word`]), and each 40-digit hex word cut to a
/// commit id's short form ([`COMMIT_SHOWN`]); then each line the check's
/// phrase rules still find sensitive ([`redaction::contains_sensitive_phrase`]:
/// a `password`, an `api key`, a `token=` assignment, a URL's userinfo) whole.
/// The token-shape rule is the word pass, so it is not run over the line
/// again: an ordinary working directory it would take for a token is kept.
/// Line breaks are kept. The codes are found in the WHOLE text, before it is
/// judged a line at a time: a code a line break splits (a shell's rows, a
/// pasted code wrapped by hand) is one code, and its line breaks follow its
/// replacement.
pub(crate) fn redact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut line = String::new();
    for (is_code, stretch) in code_stretches(text) {
        if is_code {
            line.push_str(FLEET_CODE);
            line.push_str(REDACTED_WORD);
            for _ in stretch.matches('\n') {
                judge_line(&mut line, &mut out);
                out.push('\n');
            }
            continue;
        }
        for (n, part) in stretch.split('\n').enumerate() {
            if n > 0 {
                judge_line(&mut line, &mut out);
                out.push('\n');
            }
            line.push_str(&redact_words(part));
        }
    }
    judge_line(&mut line, &mut out);
    out
}

/// One line of [`redact`]'s words onto `out`, emptied: withheld whole
/// ([`REDACTED_LINE`]) when the privacy check's phrase rules find it
/// sensitive.
fn judge_line(line: &mut String, out: &mut String) {
    if redaction::contains_sensitive_phrase(line) {
        out.push_str(REDACTED_LINE);
    } else {
        out.push_str(line);
    }
    line.clear();
}

/// `text` as its stretches in order, each fleet join code in it — wherever
/// it sits: its secret's groups with or without the `atf1-` before them, in
/// any case, with dashes or white space (a line break too) between them,
/// its `@<host>:<port>` with it ([`aterm_net::fleet_member::code_spans`]) —
/// a stretch of its own (`true`), which the caller replaces by
/// `atf1-[redacted]`. A code is the fleet's secret: the redaction never
/// needs its prefix, or its groups in one word or one line, to find it.
fn code_stretches(text: &str) -> Vec<(bool, &str)> {
    let mut out = Vec::new();
    let mut at = 0;
    for span in code_spans(text) {
        if span.start < at {
            continue;
        }
        out.push((false, &text[at..span.start]));
        out.push((true, &text[span.clone()]));
        at = span.end;
    }
    out.push((false, &text[at..]));
    out
}

/// Where a join code sits in `text` ([`code_stretches`]); none off unix,
/// where no machine joins a fleet.
#[cfg(unix)]
fn code_spans(text: &str) -> Vec<std::ops::Range<usize>> {
    aterm_net::fleet_member::code_spans(text)
}

/// Where a join code sits in `text` ([`code_stretches`]); none off unix,
/// where no machine joins a fleet.
#[cfg(not(unix))]
fn code_spans(_text: &str) -> Vec<std::ops::Range<usize>> {
    Vec::new()
}

/// [`redact`]'s word pass over one line: the breaks between its tokens kept
/// as they are.
fn redact_words(line: &str) -> String {
    use redaction::is_token_break;
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while !rest.is_empty() {
        let start = rest
            .find(|c: char| !is_token_break(c))
            .unwrap_or(rest.len());
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        let end = rest.find(is_token_break).unwrap_or(rest.len());
        redact_word(&rest[..end], &mut out);
        rest = &rest[end..];
    }
    out
}

/// [`redact_words`] for one token, onto `out`. A token that carries a fleet
/// code keeps what comes before it. A PATH (a token that starts `/`, `~/`,
/// `./` or `../`, or a variable and `/`, as `$HOME/…` in a command line a
/// shell titles its tab with) is judged a segment at a time, so a working
/// directory whose letters, digits and dashes run past the credential rule's
/// length keeps every ordinary segment and loses only one that is
/// credential-shaped.
fn redact_word(word: &str, out: &mut String) {
    let variable = word.starts_with('$') && word.contains('/');
    if let Some(at) = word.to_ascii_lowercase().find(FLEET_CODE) {
        redact_plain(&word[..at], out);
        out.push_str(FLEET_CODE);
        out.push_str(REDACTED_WORD);
    } else if variable || ["/", "~/", "./", "../"].iter().any(|p| word.starts_with(p)) {
        for (n, segment) in word.split('/').enumerate() {
            if n > 0 {
                out.push('/');
            }
            redact_plain(segment, out);
        }
    } else {
        redact_plain(word, out);
    }
}

/// How many digits of a 40-digit hex word [`redact`] keeps: a commit id's
/// short form, which still names the commit in any repository a person works
/// in, and never the whole of a 160-bit secret of the same shape — a fleet
/// key file's text is one (`aterm fleet init`), and so are other services'
/// keys.
const COMMIT_SHOWN: usize = 12;

/// One word or path segment onto `out`: a credential-shaped one
/// ([`redaction::looks_like_raw_credential`]) replaced, unless it is a
/// 40-digit hex word — judged without the punctuation around it, which is
/// copied through, so a credential that ends a sentence (`…EXAMPLE.`) is
/// replaced as one alone is, and judged whole too, for a shape whose own
/// characters end it; then each run of exactly 40 hex digits in it (a git
/// object name, or a secret of that shape, alone or after a `key=` or in an
/// `a..b` range; either case) cut to its first [`COMMIT_SHOWN`] digits and
/// `…`.
fn redact_plain(word: &str, out: &mut String) {
    let start = word
        .find(|c: char| c.is_ascii_alphanumeric())
        .unwrap_or(word.len());
    let end = word
        .rfind(|c: char| c.is_ascii_alphanumeric())
        .map_or(start, |at| at + 1);
    let bare = &word[start..end];
    if !is_hex40(bare) {
        if redaction::looks_like_raw_credential(bare) {
            out.push_str(&word[..start]);
            out.push_str(REDACTED_WORD);
            out.push_str(&word[end..]);
            return;
        }
        if redaction::looks_like_raw_credential(word) {
            out.push_str(REDACTED_WORD);
            return;
        }
    }
    let mut rest = word;
    while !rest.is_empty() {
        let start = rest
            .find(|c: char| c.is_ascii_alphanumeric())
            .unwrap_or(rest.len());
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        let end = rest
            .find(|c: char| !c.is_ascii_alphanumeric())
            .unwrap_or(rest.len());
        let run = &rest[..end];
        if is_hex40(run) {
            out.push_str(&run[..COMMIT_SHOWN]);
            out.push('…');
        } else {
            out.push_str(run);
        }
        rest = &rest[end..];
    }
}

/// Whether `word` is exactly 40 hex digits, in either case: git prints a
/// commit id in lowercase, but a 160-bit secret of the same shape may come
/// uppercase.
fn is_hex40(word: &str) -> bool {
    word.len() == 40 && word.bytes().all(|b| b.is_ascii_hexdigit())
}

/// One excerpt for the wire: redacted, capped, pct-encoded, `-` for none.
fn excerpt_token(text: Option<&str>) -> String {
    match text.map(str::trim).filter(|t| !t.is_empty()) {
        Some(text) => pct_encode(&excerpt::cap(&redact(text), excerpt::EXCERPT_BYTES)),
        None => "-".to_string(),
    }
}

/// A branch for the wire: capped and pct-encoded, a fleet code replaced as
/// [`redact`] replaces one, `-` for none. A branch is a name its repository
/// publishes, never a secret's place, so neither the credential-shape rule
/// nor the phrase rules judge it: they would take
/// `jsmith/JIRA-12345-Fix-the-login-redirect` for a token and
/// `fix/password-reset` for a password.
fn branch_token(text: Option<&str>) -> String {
    match text.map(str::trim).filter(|t| !t.is_empty()) {
        Some(text) => {
            let mut out = String::with_capacity(text.len());
            for (is_code, stretch) in code_stretches(text) {
                // A code, or a stretch from an `atf1-` on, goes; what is
                // before it stays.
                let kept = if is_code {
                    Some("")
                } else {
                    let at = stretch.to_ascii_lowercase().find(FLEET_CODE);
                    at.map(|at| &stretch[..at])
                };
                match kept {
                    Some(kept) => {
                        out.push_str(kept);
                        out.push_str(FLEET_CODE);
                        out.push_str(REDACTED_WORD);
                    }
                    None => out.push_str(stretch),
                }
            }
            pct_encode(&excerpt::cap(&out, excerpt::EXCERPT_BYTES))
        }
        None => "-".to_string(),
    }
}

/// The most bytes of a role facts carry ([`role_token`]).
const ROLE_LABEL_MAX: usize = 32;

/// A role for the wire at `level`. A role is a label someone stamped for
/// others to read (`meta set role`: `operator`, `worker:claude-satcomp`), so
/// FACTS carry it when it is one: a word of at most [`ROLE_LABEL_MAX`]
/// letters, digits and `:_.-`. A role with a space or any other character in
/// it, or longer, is text a person or an agent wrote, which facts never
/// carry: `-` at facts, and at content an excerpt (redacted and capped). A
/// label is redacted too, so a credential-shaped one never leaves.
fn role_token(role: Option<&str>, level: SurveyLevel) -> String {
    let role = role.map(str::trim);
    let label = |role: &&str| {
        (1..=ROLE_LABEL_MAX).contains(&role.len())
            && role
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b":_.-".contains(&b))
    };
    match level {
        SurveyLevel::Facts => excerpt_token(role.filter(label)),
        SurveyLevel::Content => excerpt_token(role),
    }
}

/// A word or short fact for the wire: pct-encoded, `-` for none.
fn word_token(text: Option<&str>) -> String {
    text.filter(|t| !t.is_empty())
        .map_or_else(|| "-".to_string(), pct_encode)
}

/// A count or age for the wire, `-` for none.
fn number_token(n: Option<u64>) -> String {
    n.map_or_else(|| "-".to_string(), |n| n.to_string())
}

/// What a survey's header says of this machine ([`survey_rows`]): read
/// fresh for each survey by the control server ([`Here::now`]), so a rename
/// or a sibling started shows at the next read; fixed by a test.
pub(crate) struct Here {
    /// The name this machine's LAN reaches it by
    /// (`aterm_uds::process::lan_host_name`), the one a fleet member
    /// registers with, so a fleet reader's header names it as its book does.
    pub(crate) host: Option<String>,
    /// How many live aterm instances run here for this user, this one
    /// counted ([`crate::sibling_scan::live_siblings`]).
    pub(crate) instances: usize,
    /// Whether the reader is a node this Mac lets drive it (the step-in's
    /// `[fleet] allow = "drive"`, `drive_from`): the header says `drive=1`
    /// to it alone, so its menu bar offers Take Over on this Mac.
    pub(crate) drive: bool,
}

impl Here {
    /// This machine now, to a reader that may not drive it.
    pub(crate) fn now() -> Here {
        Here {
            host: aterm_uds::process::lan_host_name(),
            instances: 1 + crate::sibling_scan::live_siblings(),
            drive: false,
        }
    }
}

/// `survey [facts|content]`: this instance's survey at the level asked, at
/// most `most` — the local Owner's [`SurveyLevel::Content`], a fleet peer's
/// `[fleet] allow` — which is also the level of a bare `survey`, and within
/// `max` bytes ([`survey_rows`]), its header saying `here`. A peer allowed
/// facts that asks `content` is answered facts, and the header's `level=`
/// says so.
pub(crate) fn cmd_survey(
    store: &Store,
    rest: &str,
    most: SurveyLevel,
    max: usize,
    here: &Here,
) -> String {
    let level = match rest.trim() {
        "" | "content" => most,
        "facts" => SurveyLevel::Facts,
        _ => return "ERR usage: survey [facts|content]\n".to_string(),
    };
    survey_rows(store, level, max, here)
}

/// THE SURVEY OF THIS INSTANCE at `level`, its header saying `here`, as the
/// `survey` verb answers it
/// (Lines-framed): `OK <n>`, then one `instance …` header, then one
/// `session …` row per hosted session in local-id order. The seam a fleet
/// listener answers through, with the level its machine allows.
///
/// ```text
/// instance pid=<pid> host=<pct> version=<v> sessions=<n> level=<facts|content> cut=<n>
///   at_ms=<unix ms> node=<32 hex|-> instances=<n> [drive=1]
/// session local=<n> sid=<s-…> state=<s> program=<pct|-> program_kind=<agent|shell|program|->
///   agent=<word|-> agent_detail=<pct|-> agent_ms=<ms> human_ms=<ms|-> bg=<n|-> task=<state|->
///   task_rev=<n> task_from=<pct|-> task_ms=<ms|-> model=<pct|-> effort=<pct|->
///   ctx_left_pct=<n|-> detail=<pct|-> doing=<pct|-> role=<pct|->  (facts: a label only)
///   supervisor=<pct|-> attention=<1|->        (facts: whether attention is raised)
///   needs=<word|-> hold_by=<word|-> held_from=<32 hex|-> typed_by=<pct|-> typed_ms=<ms|->
///   [user_title= title= cwd= branch= prompt= said= cmd= rows=]  (content only; each <pct|->)
/// ```
///
/// At content, `attention=` is the attention line's words (`<pct|->`).
/// `held_from=` is the node of the Mac whose drive holds the session over
/// the fleet (the step-in's remote slot), whatever slot `hold_by=` names
/// first: the reader whose node it is lists it as a hold it placed.
/// `host=` is the name this machine's LAN reaches it by
/// (`aterm_uds::process::lan_host_name`), and `instances=` how many live
/// aterm instances run here for this user, this one counted: a fleet peer
/// reads this instance alone (the one holding the fleet port), and the
/// reader says the others were not read. `drive=1` ends it for a fleet
/// reader this Mac lets drive it ([`Here::drive`]).
///
/// THE WHOLE REPLY IS AT MOST `max` BYTES — a fleet peer reads no more
/// (`aterm_net::fleet::REPLY_MAX`); the local Owner's is unbounded. Past it
/// the sessions least worth reading ([`Row::keep`], the newest first among
/// equals) are answered at facts, one by one — `cut=` counts the rows sent
/// so — and past that their rows are left out, which `sessions=` against
/// the rows that follow says. A busy machine reads degraded, never refused.
pub(crate) fn survey_rows(store: &Store, level: SurveyLevel, max: usize, here: &Here) -> String {
    let snapshot = store.read().unwrap_or_else(|p| p.into_inner()).snapshot();
    let now_us = crate::metrics::now_us();
    let now_unix_ms = aterm_time::unix_millis().unwrap_or(0);
    let mut rows: Vec<Option<Row>> = snapshot
        .iter()
        .map(|h| Some(session_row(h, level, now_us, now_unix_ms)))
        .collect();
    let host = word_token(here.host.as_deref());
    let version = pct_encode(aterm_types::version::app_version());
    let node = node_token();
    let instances = here.instances;
    let drive = if here.drive { " drive=1" } else { "" };
    let head = |cut: usize, sent: usize| {
        format!(
            "OK {}\ninstance pid={} host={host} version={version} sessions={} level={} \
             cut={cut} at_ms={now_unix_ms} node={node} instances={instances}{drive}\n",
            sent + 1,
            std::process::id(),
            snapshot.len(),
            level.word(),
        )
    };
    let size = |rows: &[Option<Row>]| -> usize {
        rows.iter().flatten().map(|row| row.text.len() + 1).sum()
    };
    let sent = |rows: &[Option<Row>]| rows.iter().flatten().count();
    let cut = |rows: &[Option<Row>]| rows.iter().flatten().filter(|row| row.cut).count();
    let fits =
        |rows: &[Option<Row>], total: usize| head(cut(rows), sent(rows)).len() + total <= max;
    // The least worth reading first; the newest first among equals.
    let mut order: Vec<usize> = (0..rows.len()).collect();
    order.sort_by_key(|&at| {
        (
            rows[at].as_ref().map_or(0, |row| row.keep),
            std::cmp::Reverse(at),
        )
    });
    let mut total = size(&rows);
    if level == SurveyLevel::Content {
        for &at in &order {
            if fits(&rows, total) {
                break;
            }
            let mut facts = session_row(&snapshot[at], SurveyLevel::Facts, now_us, now_unix_ms);
            facts.cut = true;
            total = total - rows[at].as_ref().map_or(0, |row| row.text.len()) + facts.text.len();
            rows[at] = Some(facts);
        }
    }
    for &at in &order {
        if fits(&rows, total) {
            break;
        }
        total -= rows[at].take().map_or(0, |row| row.text.len() + 1);
    }
    let mut out = head(cut(&rows), sent(&rows));
    for row in rows.into_iter().flatten() {
        out.push_str(&row.text);
        out.push('\n');
    }
    out
}

/// This machine's fleet node id for the header's `node=` — which a fleet
/// reader checks against the node its book holds for the address it dialled,
/// and against the node this listener proved, refusing a reply that names
/// another (`aterm_net::fleet_member::survey_all`) — as `aterm fleet
/// init|join` wrote it, never minted here; `-` for none, or one that cannot
/// be read. Read fresh here and at the listener's start, from the one state
/// directory: the listener binds again when the file changes
/// (`fleet_listen::watch_fleet_key`).
fn node_token() -> String {
    #[cfg(unix)]
    if let Some(node) = aterm_types::dirs::state_dir()
        .and_then(|dir| aterm_net::fleet_member::read_node_id(&dir).ok().flatten())
    {
        return node;
    }
    "-".to_owned()
}

/// One session's row ([`session_row`]) and how much its excerpts are worth
/// keeping when a reply must shrink ([`survey_rows`]).
struct Row {
    /// The `session …` line, no newline.
    text: String,
    /// Least first: `0` a session no agent reads (a shell's command and
    /// rows), `1` an agent at rest, `2` an agent at work, asking or at a
    /// wall, or any session raising attention.
    keep: u8,
    /// Answered at facts to fit the reply (`cut=`).
    cut: bool,
}

/// One session's `session …` row ([`survey_rows`]).
fn session_row(h: &SessionHandle, level: SurveyLevel, now_us: u64, now_unix_ms: u64) -> Row {
    // One timeline read for everything the server publishes on it.
    let (agent, gauges, task, footer, excerpts, needs, hold_by, ask) = {
        let timeline = h.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        let need = timeline.need();
        (
            timeline.agent().clone(),
            timeline.gauges(),
            timeline.task().cloned(),
            timeline.claude_footer_shown(),
            timeline.claude_excerpts().cloned(),
            // The published need verdict's words (`crate::needs`): never its
            // text, so a facts row carries no byte of an attention line.
            need.needs_word(),
            need.hold_word(),
            need.need
                .as_ref()
                .filter(|need| need.word == aterm_types::needs::NeedWord::Ask)
                .map(|need| need.key),
        )
    };
    let (user_title, role, supervisor, attention) = {
        let meta = h.ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
        // The attention line is the entry the session is ranked by where its
        // need is an ask (the most recent ask, which a newer wait does not
        // hide), else the most recent entry.
        let asked = ask.and_then(|key| {
            meta.attention_owners
                .most_recent_where(|text| crate::needs::ask_key(text) == key)
        });
        (
            meta.user_title.clone(),
            meta.role.clone(),
            meta.live_supervisor(now_us).map(str::to_owned),
            asked.map(str::to_owned).or_else(|| meta.attention.clone()),
        )
    };
    let is_agent = agent.reader.is_some();
    // The engine, under a try_lock: a contended one answers `-`. Its command
    // line and last rows are a session's words wherever no transcript gives
    // them: a shell's, and an agent's that publishes no excerpts (a Gemini
    // CLI, Muse Code, agy or Codex: only Claude Code's footer reads a
    // transcript), whose rows are redacted as a shell's are.
    let engine = engine_read(h, level == SurveyLevel::Content && excerpts.is_none());
    let human = h.ctx.human_input.age_ms(now_us);
    let task_ms = task
        .as_ref()
        .map(|t| now_unix_ms.saturating_sub(t.since_unix_ms));
    let doing = excerpts.as_ref().and_then(|e| e.excerpts.doing.as_deref());
    let keep = if attention.is_some()
        || needs != "-"
        || matches!(agent.word, "busy" | "prompt" | "question")
        || agent.word.starts_with("wall:")
    {
        2
    } else {
        u8::from(is_agent)
    };
    let attention = match level {
        // Facts carry no free text: whether a line is raised, not its words.
        SurveyLevel::Facts => attention.as_ref().map_or("-", |_| "1").to_string(),
        SurveyLevel::Content => excerpt_token(attention.as_deref()),
    };
    // Who last typed into it over the socket (S9), as `status` names it. At
    // facts only the typist tab's role or session id, or a word aterm gave,
    // and only a label by the role's rule: never the tab name a person set,
    // which this row carries only as content (`user_title=`).
    let typed = h.ctx.typed.last();
    let typed_by = typed.as_ref().and_then(|t| match level {
        SurveyLevel::Facts => t.by.facts(),
        SurveyLevel::Content => Some(t.by.words()),
    });
    // The node whose drive holds it over the fleet, whichever slot the
    // `hold_by=` word names first: what that Mac reads to list the holds it
    // placed, whatever the session's need (`status_item`'s Held from here).
    let held_from = h.ctx.fabric.holds().remote().map(|hold| hold.node.clone());
    let mut row = format!(
        "session local={} sid={} state={} program={} program_kind={} agent={} agent_detail={} \
         agent_ms={} human_ms={} bg={} task={} task_rev={} task_from={} task_ms={} model={} \
         effort={} ctx_left_pct={} detail={} doing={} role={} supervisor={} \
         attention={attention} needs={needs} hold_by={hold_by} held_from={} typed_by={} \
         typed_ms={}",
        h.local_id,
        h.sid.as_str(),
        h.state.as_str(),
        word_token(agent.program.as_deref()),
        program_kind(is_agent, agent.program.as_deref()),
        agent.word,
        word_token(agent.detail.as_deref()),
        agent.age_ms(),
        number_token(human),
        number_token(gauges.shells.map(u64::from)),
        task.as_ref().map_or("-", |t| t.state.as_str()),
        task.as_ref().map_or(0, |t| t.rev),
        word_token(task.as_ref().map(|t| t.from.as_str())),
        number_token(task_ms),
        word_token(footer.as_ref().and_then(|f| f.model.as_deref())),
        word_token(footer.as_ref().and_then(|f| f.effort.as_deref())),
        number_token(gauges.ctx_pct.map(u64::from)),
        word_token(engine.detail.as_deref()),
        word_token(doing),
        role_token(role.as_deref(), level),
        word_token(supervisor.as_deref()),
        word_token(held_from.as_deref()),
        role_token(typed_by.as_deref(), level),
        number_token(typed.map(|t| now_unix_ms.saturating_sub(t.at_unix_ms))),
    );
    if level == SurveyLevel::Content {
        let cwd = footer.as_ref().and_then(|f| f.path.clone()).or(engine.cwd);
        let prompt = prompt_of(h, task.is_some(), excerpts.as_ref());
        // A FINISHED TURN'S WORDS ARE ITS REPORT, and a report ends with its
        // result: cut, it keeps its end (`…` first), by the rule the client
        // reads the same two facts by (its task `done` or `exited`, or its agent
        // `idle`), so a report past the cap still shows the reader its result.
        // A turn in flight keeps its head.
        let finished = matches!(
            task.as_ref().map(|t| t.state),
            Some(crate::task_record::TaskState::Done | crate::task_record::TaskState::Exited)
        ) || agent.word == "idle";
        let said = excerpts.as_ref().and_then(|e| {
            if finished {
                e.excerpts.said_end.as_deref()
            } else {
                e.excerpts.said.as_deref()
            }
        });
        // The user title is the tab name a person typed: text, so content.
        let _ = write!(
            row,
            " user_title={} title={} cwd={} branch={} prompt={} said={} cmd={} rows={}",
            excerpt_token(user_title.as_deref()),
            excerpt_token(engine.title.as_deref()),
            excerpt_token(cwd.as_deref()),
            branch_token(footer.as_ref().and_then(|f| f.branch.as_deref())),
            excerpt_token(prompt.as_deref()),
            excerpt_token(said),
            excerpt_token(engine.command.as_deref()),
            excerpt_token(engine.rows.as_deref()),
        );
    }
    Row {
        text: row,
        keep,
        cut: false,
    }
}

/// What kind of program a session runs in front, for a reader that must not
/// keep its own list of shells: `agent` (an identified agent reads it),
/// `shell` (`quit_safety::is_shell_name`), `program` (anything else), `-`
/// (none seen yet).
fn program_kind(is_agent: bool, program: Option<&str>) -> &'static str {
    match program {
        _ if is_agent => "agent",
        Some(name) if crate::quit_safety::is_shell_name(name) => "shell",
        Some(_) => "program",
        None => "-",
    }
}

/// What the survey reads off a session's engine ([`engine_read`]).
#[derive(Default)]
struct EngineRead {
    /// The executing command's word (`session_status::executing_detail`).
    detail: Option<String>,
    /// The OSC title, the console's program path dropped.
    title: Option<String>,
    /// The engine's native cwd, home as `~`.
    cwd: Option<String>,
    /// The executing command line (asked only with `shell`).
    command: Option<String>,
    /// The last [`SHELL_ROWS`] non-blank rows (asked only with `shell`).
    rows: Option<String>,
}

/// One `try_lock` of `h`'s engine: the facts every row carries and, with
/// `shell` (a session no transcript excerpt speaks for), its command line
/// and last rows. A contended engine reads
/// nothing — the survey runs on a control thread and never parks behind the
/// PTY reader.
fn engine_read(h: &SessionHandle, shell: bool) -> EngineRead {
    let read = |t: &aterm_core::terminal::Terminal| {
        use crate::cwd_native::ReportedCwd as _;
        let title = crate::tab_label::without_console_program_path(t.title());
        let rows = shell.then(|| last_rows(t)).flatten();
        EngineRead {
            detail: crate::session_status::executing_detail(t),
            title: (!title.is_empty()).then(|| title.to_owned()),
            cwd: t
                .native_working_directory()
                .filter(|cwd| !cwd.is_empty())
                .map(|cwd| crate::app_tabs::home_abbreviated(&cwd)),
            command: shell
                .then(|| crate::session_status::executing_command_line(t))
                .flatten(),
            rows,
        }
    };
    match h.term.try_lock() {
        Ok(t) => read(&t),
        Err(std::sync::TryLockError::Poisoned(p)) => read(&p.into_inner()),
        Err(std::sync::TryLockError::WouldBlock) => EngineRead::default(),
    }
}

/// The most screen rows one logical line of [`last_rows`] joins: past it the
/// line is withheld whole ([`REDACTED_LINE`]), so the work under the engine's
/// lock stays bounded and no part of a line too long to judge leaves.
const LINE_ROWS_MAX: i32 = 64;

/// The last [`SHELL_ROWS`] non-blank LOGICAL lines that end on `t`'s screen,
/// oldest first, trailing blanks trimmed; `None` for a blank screen. A line a
/// soft wrap split across rows — its head in the scrollback, too — is joined
/// back into one before [`redact`] judges it, which judges a line at a time:
/// a `password=` or a 40-digit hex key split at the wrap column is one word
/// again, and a pane narrower than a secret cuts nothing out of the rule.
fn last_rows(t: &aterm_core::terminal::Terminal) -> Option<String> {
    let mut lines: Vec<String> = Vec::with_capacity(SHELL_ROWS);
    let mut r = i32::from(t.rows()) - 1;
    while r >= 0 && lines.len() < SHELL_ROWS {
        // Walked from the bottom, `r` is always its line's last row.
        let (first, last) = t.logical_line_span(r);
        let line = if last - first >= LINE_ROWS_MAX {
            REDACTED_LINE.to_owned()
        } else {
            logical_line(t, first, last)
        };
        if !line.is_empty() {
            lines.push(line);
        }
        r = first - 1;
    }
    lines.reverse();
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// Rows `first..=last` of `t` (negative rows are its scrollback) as one line:
/// each as written, trailing blanks included — a wrapped row's last cells are
/// the line's — control characters as spaces, and the end trimmed.
fn logical_line(t: &aterm_core::terminal::Terminal, first: i32, last: i32) -> String {
    let mut line = String::new();
    for row in first..=last {
        if let Some(text) = t.written_line_text(row) {
            line.extend(text.chars().map(aterm_control::wire::visible_char));
        }
    }
    line.truncate(line.trim_end().len());
    line
}

/// The session's prompt: its task's head for a session with a task record,
/// else the newest transcript prompt the window harness did not type —
/// passed over when its key is one a `by=harness` turn ledger record's text
/// has ([`harness_keys`]).
fn prompt_of(h: &SessionHandle, tasked: bool, excerpts: Option<&ClaudeExcerpts>) -> Option<String> {
    let excerpts = excerpts?;
    if tasked && let Some(task) = &excerpts.task {
        return Some(task.clone());
    }
    let harness = harness_keys(&h.ctx.turns.lock().unwrap_or_else(|p| p.into_inner()));
    newest_prompt(&excerpts.excerpts, &harness)
}

/// The newest of `excerpts`' prompts whose key is not in `harness`.
pub(crate) fn newest_prompt(excerpts: &Excerpts, harness: &[u64]) -> Option<String> {
    excerpts
        .prompts
        .iter()
        .find(|p| !harness.contains(&p.key))
        .map(|p| p.text.clone())
}

/// The keys ([`excerpt::prompt_key`]) of the texts the window harness typed,
/// per the turn ledger's `by=harness` records.
pub(crate) fn harness_keys(ledger: &crate::turn_ledger::TurnLedger) -> Vec<u64> {
    let mut keys: Vec<u64> = ledger
        .since(None)
        .filter(|r| r.by == crate::turn_ledger::TurnBy::Harness)
        .map(|r| excerpt::prompt_key(&r.text))
        .collect();
    keys.sort_unstable();
    keys.dedup();
    keys
}

#[cfg(test)]
#[path = "survey_tests.rs"]
mod tests;
