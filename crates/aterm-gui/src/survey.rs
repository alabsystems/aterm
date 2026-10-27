// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-session instance survey (`survey` control verb).

use std::fmt::Write as _;
use std::path::{Component, Path};

use aterm_agent::harness::excerpt::{self, Excerpts};

use crate::control::pct_encode;
use crate::session_store::{SessionHandle, Store};

mod redaction;

/// The excerpts the footer resolver publishes for a Claude Code process
/// (`SessionTimeline::set_claude_excerpts`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
/// Excerpts published by the footer resolver for a Claude Code process.
pub(crate) struct ClaudeExcerpts {
    /// Transcript tail excerpts.
    pub(crate) excerpts: Excerpts,
    /// Task prompt head from [`task_prompt_of_env`].
    pub(crate) task: Option<String>,
    /// Transcript byte length at the time of the read.
    pub(crate) transcript_len: Option<u64>,
}

/// What the rollout tap publishes for a codex process (`SessionTimeline::set_codex_survey`):
/// the survey's fields a codex screen does not give.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CodexSurvey {
    /// The rollout's facts ([`aterm_agent::harness::codex_facts`]).
    pub(crate) facts: aterm_agent::harness::codex_facts::CodexFacts,
    /// The branch checked out at the rollout's cwd.
    pub(crate) branch: Option<String>,
    /// Task prompt head from [`task_prompt_of_env`].
    pub(crate) task: Option<String>,
    /// The thread's active goal, from codex-alab's live session record ([`aterm_agent::harness::self_claimed::read_record_goal`]).
    pub(crate) goal: Option<String>,
}

impl CodexSurvey {
    fn excerpts(&self) -> ClaudeExcerpts {
        ClaudeExcerpts {
            excerpts: self.facts.excerpts.clone(),
            task: self.task.clone(),
            transcript_len: None,
        }
    }
}

/// Which of a session's fields a survey answers with ([the module](self)).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Detail level for a session survey ([the module](self)).
pub(crate) enum SurveyLevel {
    /// Words, counts, and ages only.
    Facts,
    /// Facts plus transcript/screen excerpts.
    Content,
}

impl SurveyLevel {
    /// Wire name (`level=`).
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Facts => "facts",
            Self::Content => "content",
        }
    }
}

/// Environment variable naming a spawned agent worker's task file.
pub(crate) const TASK_FILE_ENV: &str = "ATERM_TASK_FILE";

const TASK_HEAD_BYTES: u64 = 4096;

const SHELL_ROWS: usize = 3;

/// Read, fold, and cap the worker task prompt from [`TASK_FILE_ENV`] in `env`.
pub(crate) fn task_prompt_of_env(env: &[String]) -> Option<String> {
    let path = env
        .iter()
        .rev()
        .find_map(|kv| kv.strip_prefix(TASK_FILE_ENV)?.strip_prefix('='))?;
    task_prompt_at(Path::new(path), &crate::task_record::tasks_dir()?)
}

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
    let text = if head.len() as u64 == TASK_HEAD_BYTES {
        text.trim_end_matches(|c: char| !c.is_whitespace())
    } else {
        text.as_ref()
    };
    let folded = excerpt::fold(text);
    (!folded.is_empty()).then(|| excerpt::cap(&redact(&folded), excerpt::EXCERPT_BYTES))
}

const REDACTED_WORD: &str = "[redacted]";

const REDACTED_LINE: &str = "[redacted: sensitive]";

const FLEET_CODE: &str = "atf1-";

/// Redact credential assignments, flags, URL userinfo, bearer headers, and high-entropy
/// tokens in `text`.
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

fn judge_line(line: &mut String, out: &mut String) {
    if redaction::contains_sensitive_phrase(line) {
        out.push_str(REDACTED_LINE);
    } else {
        out.push_str(line);
    }
    line.clear();
}

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

const COMMIT_SHOWN: usize = 12;

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

fn is_hex40(word: &str) -> bool {
    word.len() == 40 && word.bytes().all(|b| b.is_ascii_hexdigit())
}

fn excerpt_token(text: Option<&str>) -> String {
    match text.map(str::trim).filter(|t| !t.is_empty()) {
        Some(text) => pct_encode(&excerpt::cap(&redact(text), excerpt::EXCERPT_BYTES)),
        None => "-".to_string(),
    }
}

fn branch_token(text: Option<&str>) -> String {
    match text.map(str::trim).filter(|t| !t.is_empty()) {
        Some(text) => {
            let mut out = String::with_capacity(text.len());
            for (is_code, stretch) in code_stretches(text) {
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

const ROLE_LABEL_MAX: usize = 32;

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

fn word_token(text: Option<&str>) -> String {
    text.filter(|t| !t.is_empty())
        .map_or_else(|| "-".to_string(), pct_encode)
}

fn number_token(n: Option<u64>) -> String {
    n.map_or_else(|| "-".to_string(), |n| n.to_string())
}

/// Host-level survey header fields ([`survey_rows`]).
pub(crate) struct Here {
    /// LAN hostname for this machine.
    pub(crate) host: Option<String>,
    /// Live `aterm` instance count for this user.
    pub(crate) instances: usize,
    /// Whether the reader is permitted to drive this machine.
    pub(crate) drive: bool,
}

impl Here {
    /// Snapshot [`Here`] for a non-driving local reader.
    pub(crate) fn now() -> Here {
        Here {
            host: aterm_uds::process::lan_host_name(),
            instances: 1 + crate::sibling_scan::live_siblings(),
            drive: false,
        }
    }
}

/// Handle `survey [facts|content]` at the requested level, capped to `most`.
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

/// Render the instance survey at `level` with header `here` and byte cap `max`.
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

fn node_token() -> String {
    #[cfg(unix)]
    if let Some(node) = aterm_types::dirs::state_dir()
        .and_then(|dir| aterm_net::fleet_member::read_node_id(&dir).ok().flatten())
    {
        return node;
    }
    "-".to_owned()
}

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

/// What a row says of the agent's usage: the context left and the plan's windows used, in
/// percent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Usage {
    ctx_left: Option<u8>,
    five_hour: Option<u8>,
    seven_day: Option<u8>,
}

impl Usage {
    /// The context left as the screen shows it (`screen`, Claude Code's own indicator), else as
    /// Claude Code's usage tap last counted it (`tapped`), else as codex's rollout counts it; the
    /// windows from the tap, else the rollout.
    fn of(
        screen: Option<u8>,
        tapped: Option<&aterm_agent::harness::tap::Reading>,
        codex: Option<&aterm_agent::harness::codex_facts::CodexFacts>,
    ) -> Usage {
        let window =
            |w: Option<aterm_agent::harness::tap::Window>| w.and_then(|w| u8::try_from(w.pct).ok());
        Usage {
            ctx_left: screen
                .or_else(|| tapped.and_then(|r| r.ctx_left))
                .or_else(|| codex.and_then(|c| c.ctx_left_pct)),
            five_hour: tapped
                .and_then(|r| window(r.five_hour))
                .or_else(|| codex.and_then(|c| c.usage_5h_pct)),
            seven_day: tapped
                .and_then(|r| window(r.seven_day))
                .or_else(|| codex.and_then(|c| c.usage_7d_pct)),
        }
    }
}

/// The reading Claude Code's usage tap wrote for its process `pgid` (the group's leader) in tab
/// `sid` since the process started (`started`, Unix seconds) ([`aterm_agent::harness::tap::Reading::read`]).
fn claude_tap(sid: &str, pgid: i32, started: u64) -> Option<aterm_agent::harness::tap::Reading> {
    let dir = aterm_agent::harness::tap::rl_dir()?;
    let pid = u32::try_from(pgid).ok()?;
    let started = i64::try_from(started).ok()?;
    aterm_agent::harness::tap::Reading::read(&dir, sid, pid, started).map(|(reading, _)| reading)
}

fn session_row(h: &SessionHandle, level: SurveyLevel, now_us: u64, now_unix_ms: u64) -> Row {
    let (
        agent,
        gauges,
        task,
        footer,
        claude,
        excerpts,
        codex,
        shown,
        needs,
        needs_by,
        hold_by,
        ask,
        goal,
    ) = {
        let timeline = h.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        let need = timeline.need();
        (
            timeline.agent().clone(),
            timeline.gauges(),
            timeline.task().cloned(),
            timeline.claude_footer_shown(),
            timeline.claude_footer_identity(),
            timeline.claude_excerpts().cloned(),
            timeline.codex_survey().cloned(),
            timeline.program_shown(),
            need.needs_word(),
            // Where the need came from: the program's own report (a program-claim need) is a
            // claim, said so in its own field; a harness's `NEEDS YOU:` report, which its agent
            // carries on past, says `harness`, so a sibling's or a fleet peer's Needs you lists
            // it after the work that has stopped (`docs/DESIGN-harness-autopilot-2026-10-15.md`
            // §14.7).
            match need.need.as_ref() {
                Some(need) if need.report.is_some() => "program",
                Some(need) if need.harness.is_some() => "harness",
                _ => "-",
            },
            need.hold_word(),
            need.need
                .as_ref()
                .filter(|need| need.word == aterm_types::needs::NeedWord::Ask)
                .map(|need| need.key),
            timeline.goal().map(str::to_owned),
        )
    };
    let (user_title, role, supervisor, attention) = {
        let meta = h.ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
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
    // A reader that names no state (codex's) leaves the word to what the program says itself.
    let (agent_word, agent_ms) = match shown.filter(|_| agent.word == "unknown") {
        Some(shown) => (
            crate::program_state::survey_word(shown.state),
            shown.age_ms(),
        ),
        None => (agent.word, agent.age_ms()),
    };
    let codex_facts = codex.as_ref().map(|c| &c.facts);
    let tapped = claude.and_then(|(pgid, started)| claude_tap(h.sid.as_str(), pgid, started?));
    let usage = Usage::of(gauges.ctx_pct, tapped.as_ref(), codex_facts);
    let excerpts = excerpts.or_else(|| codex.as_ref().map(CodexSurvey::excerpts));
    let engine = engine_read(h, level == SurveyLevel::Content && excerpts.is_none());
    let human = h.ctx.human_input.age_ms(now_us);
    let task_ms = task
        .as_ref()
        .map(|t| now_unix_ms.saturating_sub(t.since_unix_ms));
    let doing = excerpts.as_ref().and_then(|e| e.excerpts.doing.as_deref());
    let keep = if attention.is_some()
        || needs != "-"
        || matches!(agent_word, "busy" | "prompt" | "question")
        || agent_word.starts_with("wall:")
    {
        2
    } else {
        u8::from(is_agent)
    };
    let attention = match level {
        SurveyLevel::Facts => attention.as_ref().map_or("-", |_| "1").to_string(),
        SurveyLevel::Content => excerpt_token(attention.as_deref()),
    };
    let goal = match level {
        SurveyLevel::Facts => goal.as_ref().map_or("-", |_| "1").to_string(),
        SurveyLevel::Content => excerpt_token(goal.as_deref()),
    };
    let typed = h.ctx.typed.last();
    let typed_by = typed.as_ref().and_then(|t| match level {
        SurveyLevel::Facts => t.by.facts(),
        SurveyLevel::Content => Some(t.by.words()),
    });
    let held_from = h.ctx.fabric.holds().remote().map(|hold| hold.node.clone());
    let mut row = format!(
        "session local={} sid={} state={} program={} program_kind={} agent={} agent_detail={} \
         agent_ms={} human_ms={} bg={} task={} task_rev={} task_from={} task_ms={} model={} \
         effort={} ctx_left_pct={} usage_5h_pct={} usage_7d_pct={} detail={} doing={} role={} \
         supervisor={} \
         attention={attention} goal={goal} needs={needs} needs_by={needs_by} hold_by={hold_by} \
         held_from={} typed_by={} typed_ms={} {}",
        h.local_id,
        h.sid.as_str(),
        h.state.as_str(),
        word_token(agent.program.as_deref()),
        program_kind(is_agent, agent.program.as_deref()),
        agent_word,
        word_token(agent.detail.as_deref()),
        agent_ms,
        number_token(human),
        number_token(gauges.shells.map(u64::from)),
        task.as_ref().map_or("-", |t| t.state.as_str()),
        task.as_ref().map_or(0, |t| t.rev),
        word_token(task.as_ref().map(|t| t.from.as_str())),
        number_token(task_ms),
        word_token(
            footer
                .as_ref()
                .and_then(|f| f.model.as_deref())
                .or_else(|| codex_facts.and_then(|c| c.model.as_deref()))
        ),
        word_token(
            footer
                .as_ref()
                .and_then(|f| f.effort.as_deref())
                .or_else(|| codex_facts.and_then(|c| c.effort.as_deref()))
        ),
        number_token(usage.ctx_left.map(u64::from)),
        number_token(usage.five_hour.map(u64::from)),
        number_token(usage.seven_day.map(u64::from)),
        word_token(engine.detail.as_deref()),
        word_token(doing),
        role_token(role.as_deref(), level),
        word_token(supervisor.as_deref()),
        word_token(held_from.as_deref()),
        role_token(typed_by.as_deref(), level),
        number_token(typed.map(|t| now_unix_ms.saturating_sub(t.at_unix_ms))),
        h.ctx.bells.wire(now_unix_ms),
    );
    if level == SurveyLevel::Content {
        let cwd = footer
            .as_ref()
            .and_then(|f| f.path.clone())
            .or_else(|| {
                codex_facts
                    .and_then(|c| c.cwd.as_deref()?.to_str())
                    .map(crate::app_tabs::home_abbreviated)
            })
            .or(engine.cwd);
        let prompt = prompt_of(h, task.is_some(), excerpts.as_ref());
        let finished = matches!(
            task.as_ref().map(|t| t.state),
            Some(crate::task_record::TaskState::Done | crate::task_record::TaskState::Exited)
        ) || agent_word == "idle";
        let said = excerpts.as_ref().and_then(|e| {
            if finished {
                e.excerpts.said_end.as_deref()
            } else {
                e.excerpts.said.as_deref()
            }
        });
        let _ = write!(
            row,
            " user_title={} title={} cwd={} branch={} prompt={} said={} cmd={} rows={}",
            excerpt_token(user_title.as_deref()),
            excerpt_token(engine.title.as_deref()),
            excerpt_token(cwd.as_deref()),
            branch_token(
                footer
                    .as_ref()
                    .and_then(|f| f.branch.as_deref())
                    .or_else(|| codex.as_ref().and_then(|c| c.branch.as_deref()))
            ),
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

fn engine_read(h: &SessionHandle, shell: bool) -> EngineRead {
    let read = |t: &aterm_core::terminal::Terminal| {
        use crate::cwd_native::ReportedCwd as _;
        let title = crate::tab_label::without_console_program_path(t.title());
        let whole = !crate::frame_unfinished_now(t);
        let rows = (shell && whole).then(|| last_rows(t)).flatten();
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

const LINE_ROWS_MAX: i32 = 64;

fn last_rows(t: &aterm_core::terminal::Terminal) -> Option<String> {
    let mut lines: Vec<String> = Vec::with_capacity(SHELL_ROWS);
    let mut r = i32::from(t.rows()) - 1;
    while r >= 0 && lines.len() < SHELL_ROWS {
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

fn prompt_of(h: &SessionHandle, tasked: bool, excerpts: Option<&ClaudeExcerpts>) -> Option<String> {
    let excerpts = excerpts?;
    if tasked && let Some(task) = &excerpts.task {
        return Some(task.clone());
    }
    let harness = harness_keys(&h.ctx.turns.lock().unwrap_or_else(|p| p.into_inner()));
    newest_prompt(&excerpts.excerpts, &harness)
}

/// Newest prompt in `excerpts` whose key is not in `harness`.
pub(crate) fn newest_prompt(excerpts: &Excerpts, harness: &[u64]) -> Option<String> {
    excerpts
        .prompts
        .iter()
        .find(|p| !harness.contains(&p.key))
        .map(|p| p.text.clone())
}

/// Prompt keys ([`excerpt::prompt_key`]) of texts typed by the window harness.
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
