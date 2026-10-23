// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Transcript-backed confirmation of screen-detected Claude Code usage, context, and login walls.

use aterm_json::Value;
use aterm_phase::{ApiCause, WallKind};

use super::super::policy::turn_end::{Conversation, LeftAnswer, WaitedLimit, ended_under};
use super::super::report::{Offscreen, parse_offscreen};
use super::*;

pub(super) fn conversation_of(tail: &str) -> Conversation {
    tail.lines()
        .rev()
        .find_map(|line| {
            let v = crate::harness::transcript::conversation(line).ok()?;
            if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
                return None;
            }
            v.get("sessionId").map(|id| {
                id.as_str()
                    .filter(|id| !id.is_empty())
                    .map_or(Conversation::Unknown, |id| {
                        Conversation::Named(id.to_owned())
                    })
            })
        })
        .unwrap_or(Conversation::Unknown)
}

pub(super) struct LimitTranscript {
    pub(super) conversation: Conversation,
    pub(super) served: bool,
    pub(super) chosen: Option<String>,
    pub(super) elsewhere: bool,
}

pub(super) const RULE_WALL_CONFIRM: &str = "wall-confirm@v1";

pub(crate) const LAG_TRIES: u32 = 4;

pub(crate) const LAG_STEP: Duration = Duration::from_millis(250);

/// Newest main-chain entry in a Claude Code session transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Entry {
    Nothing,
    Notice {
        text: String,
        error: Option<String>,
        api_error: Option<String>,
        status: Option<u16>,
        limit: Option<String>,
    },
    ToolResult,
    Input,
    Reply,
}

impl Entry {
    fn word(&self) -> String {
        match self {
            Entry::Nothing => "nothing of the conversation's".to_string(),
            Entry::Notice { text, .. } => format!("a notice of its own ({})", clip(text)),
            Entry::ToolResult => "a tool's result".to_string(),
            Entry::Input => "a user turn".to_string(),
            Entry::Reply => "the model's reply".to_string(),
        }
    }
}

/// Summary of the final main-chain entry and turn-closure state of a transcript tail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TranscriptEnd {
    pub(crate) last: Entry,
    pub(crate) closed: bool,
    pub(crate) row: Option<String>,
    pub(crate) at: Option<i64>,
    pub(crate) prompted: bool,
}

impl TranscriptEnd {
    #[must_use]
    pub(super) fn typed(&self) -> Option<String> {
        match &self.last {
            Entry::Notice {
                error, api_error, ..
            } => api_error.clone().or_else(|| error.clone()),
            _ => None,
        }
    }

    #[must_use]
    pub(super) fn notice_at(&self) -> Option<i64> {
        self.at
            .filter(|_| matches!(self.last, Entry::Notice { .. }))
    }

    /// Returns whether the transcript tail has not yet flushed the completed turn shown on screen.
    #[must_use]
    pub(crate) const fn lagging(&self) -> bool {
        !self.closed && !matches!(self.last, Entry::Notice { .. })
    }
}

/// Relevant main-chain transcript row parsed by [`transcript_ends_on`].
pub(crate) enum Row {
    Entry(Entry),
    Closed,
}

/// Scans a JSONL transcript tail to extract its [`TranscriptEnd`].
pub(crate) fn transcript_ends_on(jsonl: &str) -> TranscriptEnd {
    let mut end = TranscriptEnd {
        last: Entry::Nothing,
        closed: false,
        row: None,
        at: None,
        prompted: false,
    };
    for line in jsonl.lines() {
        let Ok(v) = crate::harness::transcript::conversation(line) else {
            continue;
        };
        if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        match row_of(&v) {
            Some(Row::Closed) => end.closed = true,
            Some(Row::Entry(entry)) => {
                if entry == Entry::Input && opens_the_conversation(&v) && end.last != Entry::Nothing
                {
                    continue;
                }
                end = TranscriptEnd {
                    last: entry,
                    closed: false,
                    row: row_id(&v),
                    at: row_time(&v),
                    prompted: false,
                };
            }
            None => end.prompted |= prompt_command(&v),
        }
    }
    end
}

fn prompt_command(v: &Value) -> bool {
    v.get("type").and_then(Value::as_str) == Some("user")
        && v.get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_str)
            .is_some_and(|t| t.trim().starts_with("<command-message>"))
}

fn row_id(v: &Value) -> Option<String> {
    v.get("uuid").and_then(Value::as_str).map(str::to_owned)
}

fn row_time(v: &Value) -> Option<i64> {
    v.get("timestamp")
        .and_then(Value::as_str)
        .and_then(crate::harness::upgrade_models::parse_utc)
        .and_then(|t| i64::try_from(t).ok())
}

pub(super) const LEFT_ANSWER_SLACK_S: i64 = 10;

pub(super) fn served_since_notice(jsonl: &str, notice: &str) -> bool {
    let mut after = false;
    let mut served = false;
    for line in jsonl.lines() {
        let Ok(v) = crate::harness::transcript::conversation(line) else {
            continue;
        };
        if !after {
            after = row_id(&v).as_deref() == Some(notice);
            continue;
        }
        let flag = |key: &str| v.get(key).and_then(Value::as_bool) == Some(true);
        if flag("isSidechain") || v.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let synthetic = v
            .get("message")
            .and_then(|m| m.get("model"))
            .and_then(Value::as_str)
            == Some("<synthetic>");
        if flag("isApiErrorMessage") {
            served = false;
        } else if !synthetic {
            served = true;
        }
    }
    served
}

pub(super) fn chosen_since_notice(jsonl: &str, notice: &str) -> Option<String> {
    let set = aterm_phase::anchor("model.set");
    let mut after = false;
    let mut model = false;
    let mut chosen = None;
    for line in jsonl.lines() {
        let Ok(v) = crate::harness::transcript::conversation(line) else {
            continue;
        };
        if !after {
            after = row_id(&v).as_deref() == Some(notice);
            continue;
        }
        if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let text = match v.get("type").and_then(Value::as_str) {
            Some("user") => texts(&v),
            Some("system") if v.get("subtype").and_then(Value::as_str) == Some("local_command") => {
                v.get("content")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned()
            }
            _ => continue,
        };
        let text = text.trim();
        if text.starts_with("<command-name>") {
            model = text.starts_with("<command-name>/model</command-name>");
        } else if let Some(out) = text.strip_prefix("<local-command-stdout>")
            && std::mem::take(&mut model)
            && let Some(words) = out
                .split("</local-command-stdout>")
                .next()
                .and_then(|o| o.trim().strip_prefix(set))
        {
            chosen = Some(words.replace('`', "").trim().to_owned());
        }
    }
    chosen
}

pub(super) const RESET_TIE_SLACK_S: u64 = 3600;

pub(super) fn waited_notice_row(jsonl: &str, waited: &WaitedLimit, reset: i64) -> Option<String> {
    let mut newest = None;
    for line in jsonl.lines() {
        let Ok(v) = crate::harness::transcript::conversation(line) else {
            continue;
        };
        let flag = |key: &str| v.get(key).and_then(Value::as_bool) == Some(true);
        if flag("isSidechain")
            || !flag("isApiErrorMessage")
            || v.get("type").and_then(Value::as_str) != Some("assistant")
        {
            continue;
        }
        let text = texts(&v);
        let quota = v.get("quotaLimits");
        let window = quota
            .and_then(|q| q.get("rateLimitType"))
            .and_then(Value::as_str);
        let resets_at = quota
            .and_then(|q| q.get("resetsAt"))
            .and_then(Value::as_i64);
        let ties = limit_named(&text, window)
            .is_some_and(|kind| waited.named_by(kind, aterm_phase::notice_reset(&text).as_deref()))
            && resets_at.is_none_or(|at| at.abs_diff(reset) <= RESET_TIE_SLACK_S);
        if ties {
            newest = row_id(&v);
        }
    }
    newest
}

pub(super) fn waits_on_background(jsonl: &str, launched: i64) -> bool {
    turn_closed(jsonl, launched) == Some(true)
}

pub(super) fn ended_with_nothing_pending(jsonl: &str, launched: i64) -> bool {
    turn_closed(jsonl, launched) == Some(false)
}

fn turn_closed(jsonl: &str, launched: i64) -> Option<bool> {
    let mut last = Entry::Nothing;
    let mut close: Option<(bool, Option<i64>)> = None;
    let mut person = false;
    for line in jsonl.lines() {
        let Ok(v) = crate::harness::transcript::conversation(line) else {
            continue;
        };
        let flag = |key: &str| v.get(key).and_then(Value::as_bool) == Some(true);
        if flag("isSidechain") {
            continue;
        }
        match row_of(&v) {
            Some(Row::Closed) => {
                let pending = ["pendingBackgroundAgentCount", "pendingWorkflowCount"]
                    .iter()
                    .any(|key| v.get(key).and_then(Value::as_u64).is_some_and(|n| n > 0));
                close = Some((pending, row_time(&v)));
                person = false;
            }
            Some(Row::Entry(entry)) => {
                if entry == Entry::Input && opens_the_conversation(&v) && last != Entry::Nothing {
                    continue;
                }
                last = entry;
                close = None;
                person = false;
            }
            None => {
                person |= close.is_some()
                    && v.get("type").and_then(Value::as_str) == Some("user")
                    && !flag("isMeta")
                    && !flag("isCompactSummary")
                    && !flag("queueTranscriptOnly");
            }
        }
    }
    let (pending, at) = close?;
    (last == Entry::Reply && !person && at.is_some_and(|at| at > launched)).then_some(pending)
}

/// Parses a JSON transcript object into a main-chain [`Row`], if applicable.
pub(crate) fn row_of(v: &Value) -> Option<Row> {
    match v.get("type").and_then(Value::as_str) {
        Some("assistant") if v.get("isApiErrorMessage").and_then(Value::as_bool) == Some(true) => {
            Some(Row::Entry(Entry::Notice {
                text: texts(v),
                error: v.get("error").and_then(Value::as_str).map(str::to_owned),
                api_error: v.get("apiError").and_then(Value::as_str).map(str::to_owned),
                status: v
                    .get("apiErrorStatus")
                    .and_then(Value::as_u64)
                    .and_then(|s| u16::try_from(s).ok()),
                limit: v
                    .get("quotaLimits")
                    .and_then(|q| q.get("rateLimitType"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            }))
        }
        Some("assistant") => Some(Row::Entry(Entry::Reply)),
        Some("user") => user_entry(v).map(Row::Entry),
        Some("system") => match v.get("subtype").and_then(Value::as_str) {
            Some("turn_duration") => Some(Row::Closed),
            Some("local_command") => v
                .get("content")
                .and_then(Value::as_str)
                .and_then(command_notice)
                .map(Row::Entry),
            _ => None,
        },
        _ => None,
    }
}

fn opens_the_conversation(v: &Value) -> bool {
    v.get("parentUuid").is_some_and(Value::is_null)
}

fn user_entry(v: &Value) -> Option<Entry> {
    let flag = |key: &str| v.get(key).and_then(Value::as_bool) == Some(true);
    if flag("isMeta") || flag("isCompactSummary") || flag("queueTranscriptOnly") {
        return None;
    }
    match v.get("message").and_then(|m| m.get("content")) {
        Some(Value::String(text)) => (!local_command(text)).then_some(Entry::Input),
        Some(Value::Array(parts))
            if parts
                .iter()
                .any(|p| p.get("type").and_then(Value::as_str) == Some("tool_result")) =>
        {
            Some(Entry::ToolResult)
        }
        _ => Some(Entry::Input),
    }
}

fn local_command(text: &str) -> bool {
    let t = text.trim();
    t == "/compact"
        || t.starts_with("/compact ")
        || [
            "<command-name>",
            "<command-message>",
            "<local-command-stdout>",
            "<local-command-stderr>",
        ]
        .iter()
        .any(|open| t.starts_with(open))
}

fn command_notice(content: &str) -> Option<Entry> {
    let t = content.trim();
    let inner = ["<local-command-stderr>", "<local-command-stdout>"]
        .iter()
        .find_map(|open| t.strip_prefix(open))?;
    let inner = inner
        .split("</local-command-")
        .next()
        .unwrap_or(inner)
        .trim();
    (aterm_phase::classify_wall(inner) == Some(WallKind::Context)).then(|| Entry::Notice {
        text: inner.to_owned(),
        error: None,
        api_error: None,
        status: None,
        limit: None,
    })
}

fn texts(v: &Value) -> String {
    match v.get("message").and_then(|m| m.get("content")) {
        Some(Value::String(t)) => t.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

/// Policy action class (`Limit`, `Compact`, `Login`, or `Retry`) for a [`WallKind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Class {
    Limit,
    Context,
    Auth,
    Api,
    Memory,
}

/// Maps a [`WallKind`] to its policy [`Class`].
pub(crate) const fn class(kind: WallKind) -> Class {
    if kind.reads_limited() {
        return Class::Limit;
    }
    match kind {
        WallKind::Context => Class::Context,
        WallKind::Auth => Class::Auth,
        WallKind::Memory => Class::Memory,
        _ => Class::Api,
    }
}

fn error_class(error: &str) -> Option<Class> {
    match error {
        "authentication_failed" => Some(Class::Auth),
        "rate_limit" | "billing_error" => Some(Class::Limit),
        "server_error"
        | "overloaded"
        | "unknown"
        | "invalid_request"
        | "model_not_found"
        | "max_output_tokens"
        | "account_on_hold"
        | "verification_required"
        | "oauth_org_not_allowed"
        | "cloud_credential_error" => Some(Class::Api),
        _ => None,
    }
}

const TERMINAL_API_ERRORS: &[&str] = &[
    "tool_history_mismatch",
    "dlp_request_denied",
    "safety_monitor_blocked",
    "claude_code_version_too_old",
    "effort_requires_thinking",
    "advisor_incompatible",
    "pdf_too_large",
    "pdf_password_protected",
    "field_not_granted",
    "org_disabled_credential",
    "api_key_auth_disabled",
    "invalid_credential_header",
    "autocompact_thrashing",
];

const LOGIN_API_ERRORS: &[&str] = &["gateway_signin_required", "gateway_session_expired"];

fn signs_in(api_error: Option<&str>) -> bool {
    api_error.is_some_and(|a| LOGIN_API_ERRORS.contains(&a))
}

fn lost_login(error: Option<&str>, api_error: Option<&str>) -> bool {
    signs_in(api_error)
        || (error.and_then(error_class) == Some(Class::Auth)
            && !api_error.is_some_and(|a| TERMINAL_API_ERRORS.contains(&a)))
}

const REFUSED_ERRORS: &[&str] = &[
    "invalid_request",
    "model_not_found",
    "account_on_hold",
    "verification_required",
    "oauth_org_not_allowed",
];

/// Returns the [`WallKind`] when a closed transcript turn ended on Claude Code's own error notice.
#[must_use]
pub(crate) fn transcript_wall(end: &TranscriptEnd) -> Option<WallKind> {
    if !end.closed {
        return None;
    }
    notice_wall(&end.last)
}

fn notice_wall(entry: &Entry) -> Option<WallKind> {
    let Entry::Notice {
        text,
        error,
        api_error,
        status,
        ..
    } = entry
    else {
        return None;
    };
    match aterm_phase::classify_wall(text) {
        Some(WallKind::ApiError {
            code: None,
            cause: ApiCause::Server,
            ..
        }) => {}
        Some(WallKind::ApiError { .. })
            if aterm_phase::wall::named_by_status_alone(text)
                && lost_login(error.as_deref(), api_error.as_deref()) =>
        {
            return Some(WallKind::Auth);
        }
        Some(kind) => return Some(kind),
        None if aterm_phase::wall::not_a_wall(text) => return None,
        None if ["wall.compact_images", "wall.compact_attachments"]
            .iter()
            .any(|id| {
                text.to_lowercase()
                    .contains(&aterm_phase::anchor(id).to_lowercase())
            }) =>
        {
            return Some(WallKind::Context);
        }
        None => {}
    }
    if signs_in(api_error.as_deref()) {
        return Some(WallKind::Auth);
    }
    let refused = WallKind::ApiError {
        code: *status,
        retryable: false,
        cause: ApiCause::Request,
    };
    if api_error
        .as_deref()
        .is_some_and(|a| TERMINAL_API_ERRORS.contains(&a))
    {
        return Some(refused);
    }
    match error.as_deref() {
        Some(e) if REFUSED_ERRORS.contains(&e) => return Some(refused),
        Some("billing_error") => return Some(WallKind::Spend),
        _ => {}
    }
    if error.as_deref().and_then(error_class) == Some(Class::Auth) {
        return Some(WallKind::Auth);
    }
    if status.is_some_and(|c| (400..500).contains(&c) && !matches!(c, 401 | 408 | 409 | 429 | 451))
    {
        return Some(refused);
    }
    Some(WallKind::ApiError {
        code: *status,
        retryable: status.is_none_or(|c| matches!(c, 408 | 409 | 429) || c >= 500),
        cause: ApiCause::Server,
    })
}

const SHOWN_WORDS: usize = 6;

fn shows(said: &str, text: &str) -> bool {
    let want: Vec<&str> = text.split_whitespace().take(SHOWN_WORDS).collect();
    !want.is_empty()
        && said
            .split_whitespace()
            .take(SHOWN_WORDS)
            .eq(want.iter().copied())
}

/// Returns whether `end` confirms a screen-detected wall of kind `screen`.
#[must_use]
pub(crate) fn confirms(screen: WallKind, end: &TranscriptEnd) -> bool {
    let Entry::Notice {
        text,
        error,
        api_error,
        ..
    } = &end.last
    else {
        return false;
    };
    let named = aterm_phase::classify_wall(text);
    let fallback = matches!(
        named,
        Some(WallKind::ApiError {
            code: None,
            cause: ApiCause::Server,
            ..
        })
    );
    let notice = match named {
        Some(kind) if !fallback => Some(class(kind)),
        _ => error
            .as_deref()
            .and_then(error_class)
            .or_else(|| named.map(class)),
    };
    let login = || {
        class(screen) == Class::Auth
            && aterm_phase::wall::named_by_status_alone(text)
            && lost_login(error.as_deref(), api_error.as_deref())
    };
    notice == Some(class(screen)) || login()
}

fn refined(w: &aterm_phase::Wall, end: &TranscriptEnd) -> Option<aterm_phase::Wall> {
    let unnamed = matches!(
        w.kind,
        WallKind::ApiError {
            code: None,
            cause: ApiCause::Server,
            ..
        }
    );
    let Entry::Notice {
        text, api_error, ..
    } = &end.last
    else {
        return None;
    };
    let by_status = matches!(w.kind, WallKind::ApiError { code: Some(_), .. })
        && aterm_phase::wall::named_by_status_alone(text);
    if !(unnamed || by_status) || !shows(&w.message, text) {
        return None;
    }
    let login = signs_in(api_error.as_deref());
    let kind = notice_wall(&end.last).filter(|k| {
        *k != w.kind
            && if by_status {
                *k == WallKind::Auth
            } else {
                class(*k) == class(w.kind) || (login && *k == WallKind::Auth)
            }
    })?;
    Some(aterm_phase::Wall { kind, ..w.clone() })
}

fn place_standing(reading: &mut aterm_phase::Reading, wall: aterm_phase::Wall) {
    reading.phase = Phase::Limited {
        message: wall.message.clone(),
        reset: wall.reset.clone(),
    };
    reading.wall = Some(wall);
}

fn named_by(w: &aterm_phase::Wall, end: &TranscriptEnd) -> Option<Named> {
    let Entry::Notice { text, limit, .. } = &end.last else {
        return None;
    };
    if class(w.kind) != Class::Limit {
        return None;
    }
    let kind = limit_named(text, limit.as_deref())?;
    if std::mem::discriminant(&kind) == std::mem::discriminant(&w.kind) {
        return None;
    }
    let text = limit_words(text, limit.as_deref());
    let message = if limit::resumes_by_itself(&w.message) {
        format!("{text} \u{2014} {}", w.message)
    } else {
        text
    };
    Some(Named { kind, message })
}

/// Classifies the specific [`WallKind`] named by a usage-limit notice row.
pub(crate) fn limit_named(text: &str, limit: Option<&str>) -> Option<WallKind> {
    let typed = limit.and_then(|limit| match limit {
        "five_hour" => Some(WallKind::UsageSession),
        "seven_day" => Some(WallKind::UsageWeekly),
        "seven_day_sonnet" | "seven_day_opus" | "seven_day_overage_included" => {
            Some(WallKind::ModelBucket { consent: false })
        }
        "overage" => Some(WallKind::Spend),
        _ => None,
    });
    match aterm_phase::classify_wall(text).filter(WallKind::reads_limited) {
        Some(WallKind::UsageSession) if !limit::names_a_session_window(text) => {
            typed.or(Some(WallKind::UsageSession))
        }
        Some(kind) => Some(kind),
        None => typed,
    }
}

pub(super) fn found_named(kind: WallKind, text: &str, limit: Option<&str>) -> (WallKind, String) {
    match limit_named(text, limit).filter(|_| class(kind) == Class::Limit) {
        Some(named) => (named, limit_words(text, limit)),
        None => (kind, one_line(text)),
    }
}

pub(super) fn limit_words(text: &str, limit: Option<&str>) -> String {
    let words = one_line(text);
    let bucket = |l: &&str| {
        matches!(
            limit_named(text, Some(l)),
            Some(WallKind::ModelBucket { .. })
        ) && super::super::policy::turn_end::bucket_model(&words).is_none()
            && super::super::policy::turn_end::bucket_model(l).is_some()
    };
    match limit.filter(bucket) {
        Some(limit) => format!("{words} \u{2014} {limit}"),
        None => words,
    }
}

pub(in crate::supervise) fn words_kind(words: &str) -> Option<WallKind> {
    if let Some((text, limit)) = words.rsplit_once(" \u{2014} ")
        && limit_words(text, Some(limit)) == words
    {
        return limit_named(text, Some(limit));
    }
    aterm_phase::classify_wall(words)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Named {
    pub(super) kind: WallKind,
    pub(super) message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Check {
    Decide,
    AsDecided,
}

#[derive(Debug, Clone)]
pub(super) struct Refusal {
    key: String,
    line: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct LeftRead {
    pub(super) seq: u64,
    pub(super) answer: LeftAnswer,
    pub(super) dated: bool,
}

#[derive(Debug, Clone, Default)]
pub(super) struct StandingRead {
    asked_for: Option<(Option<u32>, Option<String>)>,
    found: Option<Standing>,
    placed: Option<u64>,
}

#[derive(Debug, Clone)]
struct Standing {
    wall: aterm_phase::Wall,
    until: Option<u64>,
    row: Option<String>,
    said: Option<String>,
    conversation: Conversation,
    after_turn: bool,
}

#[derive(Debug, Clone)]
pub(super) struct Verdict {
    pub(super) seq: u64,
    refused: Option<Refusal>,
    found: Option<aterm_phase::Wall>,
    named: Option<Named>,
    pub(super) conversation: Option<Conversation>,
}

fn is_owner_row(row: &str) -> bool {
    !row.is_empty() && !row.starts_with(char::is_whitespace)
}

pub(super) fn same_rows(a: &[String], b: &[String]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.trim_end() == y.trim_end())
}

pub(super) const OWNER_ROWS: u64 = 400;

pub(super) type RowsAbove = (Vec<String>, Option<u64>);
impl<C: Ctl> Session<'_, C> {
    pub(super) fn confirm_wall(
        &mut self,
        reading: &mut aterm_phase::Reading,
        screen: &Screen,
        opts: &SuperviseOpts,
        check: Check,
    ) {
        let seq = screen.seq;
        let decided = self.wall_verdict.as_ref().filter(|v| v.seq == seq);
        let (refused, found, named) = match (check, decided) {
            (Check::AsDecided, Some(v)) => (v.refused.is_some(), v.found.clone(), v.named.clone()),
            _ => {
                self.wall_conversation = Some((
                    seq,
                    Conversation::unread(opts.idle_host.is_some(), reading.program),
                ));
                self.turn_end.notice_typed_was(None);
                self.turn_end.notice_written_at(None);
                let mut refined_wall = None;
                let (refused, named) =
                    match self.judge_wall(reading, screen, opts, &mut refined_wall) {
                        Ok(named) => (None, named),
                        Err(refusal) => (Some(refusal), None),
                    };
                let found = refined_wall.or_else(|| self.transcript_found(reading, screen, opts));
                let out = refused.is_some();
                if !out {
                    self.wall_refused_said = None;
                }
                self.wall_verdict = Some(Verdict {
                    seq,
                    refused,
                    found: found.clone(),
                    named: named.clone(),
                    conversation: None,
                });
                (out, found, named)
            }
        };
        apply_verdict(reading, screen, refused, found, named);
    }

    pub(super) fn wall_as_decided(&self, reading: &mut aterm_phase::Reading, screen: &Screen) {
        if let Some(v) = self.wall_verdict.as_ref().filter(|v| v.seq == screen.seq) {
            apply_verdict(
                reading,
                screen,
                v.refused.is_some(),
                v.found.clone(),
                v.named.clone(),
            );
        }
    }

    pub(super) fn carry_point(&mut self, old: u64, new: u64) {
        if let Some(v) = self.wall_verdict.as_mut().filter(|v| v.seq == old) {
            v.seq = new;
        }
        if let Some(l) = self.left_read.as_mut().filter(|l| l.seq == old) {
            l.seq = new;
        }
        if let Some((seq, _)) = self
            .wall_conversation
            .as_mut()
            .filter(|(seq, _)| *seq == old)
        {
            *seq = new;
        }
        if self.standing_read.placed == Some(old) {
            self.standing_read.placed = Some(new);
        }
    }

    fn judge_wall(
        &mut self,
        reading: &aterm_phase::Reading,
        screen: &Screen,
        opts: &SuperviseOpts,
        refined_wall: &mut Option<aterm_phase::Wall>,
    ) -> Result<Option<Named>, Refusal> {
        let wall = match reading.program {
            Program::Claude => reading.wall.as_ref().filter(|w| w.kind != WallKind::Memory),
            Program::Codex | Program::Agy | Program::Muse | Program::Gemini | Program::Generic => {
                None
            }
        };
        let Some(w) = wall else {
            return Ok(None);
        };
        let host = opts.idle_host.as_ref();
        let left = self
            .left_read
            .as_ref()
            .filter(|l| l.seq == screen.seq)
            .map(|l| (self.unix_of(l.answer.since), l.answer.screen_tells));
        let untold = left.is_some_and(|(_, tells)| !tells);
        let kept = self.limit_notice_row.take();
        let unsaid = |s: &mut Self| {
            s.limit_notice_row = kept.clone();
            Err(Refusal {
                key: format!("{} left, untold", w.kind.name()),
                line: None,
            })
        };
        let why = match host.and_then(|h| h.transcript_tail()) {
            None if untold => return unsaid(self),
            None if self.under_call_on_screen(w, screen) => {
                "read under a tool's call, and no transcript to confirm it".to_string()
            }
            None => return Ok(None),
            Some(mut tail) => {
                let mut end = transcript_ends_on(&tail);
                for _ in 0..LAG_TRIES {
                    if !end.lagging() {
                        break;
                    }
                    self.fx.sleep(LAG_STEP);
                    match host.and_then(|h| h.transcript_tail_again()) {
                        Some(next) => {
                            end = transcript_ends_on(&next);
                            tail = next;
                        }
                        None => break,
                    }
                }
                self.turn_end.notice_typed_was(end.typed());
                self.turn_end.notice_written_at(end.notice_at());
                let older = left.is_some_and(|(since, tells)| {
                    end.at
                        .map_or(!tells, |at| at < since.saturating_sub(LEFT_ANSWER_SLACK_S))
                });
                if confirms(w.kind, &end) && !older {
                    self.wall_conversation = Some((screen.seq, conversation_of(&tail)));
                    *refined_wall = refined(w, &end);
                    let named = named_by(w, &end);
                    self.limit_notice_row = end.row.filter(|_| class(w.kind) == Class::Limit);
                    if let Some(l) = self.left_read.as_mut().filter(|l| l.seq == screen.seq) {
                        l.dated = end.at.is_some();
                    }
                    return Ok(named);
                }
                if untold {
                    return unsaid(self);
                }
                if older {
                    "the transcript's notice was written before the turn it would answer"
                        .to_string()
                } else {
                    format!("the transcript ends on {}", end.last.word())
                }
            }
        };
        let words = one_line(&w.message);
        Err(Refusal {
            key: format!("{} {words} {why}", w.kind.name()),
            line: Some(format!(
                "SKIPPED seq={} rule={RULE_WALL_CONFIRM} unconfirmed wall: {} — {why}: {}",
                screen.seq,
                w.kind.name(),
                clip(&words)
            )),
        })
    }

    pub(super) fn transcript_found(
        &mut self,
        reading: &aterm_phase::Reading,
        screen: &Screen,
        opts: &SuperviseOpts,
    ) -> Option<aterm_phase::Wall> {
        let claude = match reading.program {
            Program::Claude => true,
            Program::Codex | Program::Agy | Program::Muse | Program::Gemini | Program::Generic => {
                false
            }
        };
        if reading.wall.is_some() || !claude || reading.phase != Phase::Idle {
            return None;
        }
        let host = opts.idle_host.as_ref()?;
        let said = aterm_phase::phase::last_said_notice(&screen.rows)
            .filter(|s| !aterm_phase::wall::not_a_wall(&s.text))?;
        let mut tail = host.transcript_tail()?;
        let mut end = transcript_ends_on(&tail);
        let error_row = said.placement == aterm_phase::Placement::ErrorRow;
        let behind = |end: &TranscriptEnd| {
            error_row
                && end.prompted
                && matches!(&end.last, Entry::Notice { text, .. } if !shows(&said.text, text))
        };
        let unsaid = error_row
            && ((!end.closed && matches!(end.last, Entry::Input | Entry::ToolResult))
                || behind(&end));
        let tries = if said.shaped {
            LAG_TRIES
        } else {
            u32::from(unsaid)
        };
        for _ in 0..tries {
            if !end.lagging() && !behind(&end) {
                break;
            }
            self.fx.sleep(LAG_STEP);
            match host.transcript_tail_again() {
                Some(next) => {
                    end = transcript_ends_on(&next);
                    tail = next;
                }
                None => break,
            }
        }
        let names = |end: &TranscriptEnd| match &end.last {
            Entry::Notice { text, .. } => {
                shows(&said.text, text)
                    || (said.hook_words
                        && aterm_phase::classify_wall(&said.text).is_some_and(|k| confirms(k, end)))
            }
            _ => false,
        };
        for _ in 0..LAG_TRIES {
            let open = !end.closed && names(&end);
            if !open {
                break;
            }
            self.fx.sleep(LAG_STEP);
            match host.transcript_tail_again() {
                Some(next) => {
                    end = transcript_ends_on(&next);
                    tail = next;
                }
                None => break,
            }
        }
        self.turn_end.notice_typed_was(end.typed());
        self.turn_end.notice_written_at(end.notice_at());
        if !names(&end) {
            return None;
        }
        let Entry::Notice { text, limit, .. } = &end.last else {
            return None;
        };
        let (kind, message) = found_named(transcript_wall(&end)?, text, limit.as_deref());
        self.wall_conversation = Some((screen.seq, conversation_of(&tail)));
        Some(aterm_phase::Wall {
            kind,
            message,
            reset: aterm_phase::notice_reset(text),
            row: said.row,
            placement: said.placement,
            under_call: false,
        })
    }

    pub(super) fn read_standing_limit(
        &mut self,
        reading: &mut aterm_phase::Reading,
        screen: &Screen,
        worked: bool,
        opts: &SuperviseOpts,
        check: Check,
    ) -> bool {
        if check == Check::AsDecided {
            if self.standing_read.placed == Some(screen.seq)
                && reading.wall.is_none()
                && let Some(found) = self.standing_read.found.clone()
            {
                place_standing(reading, found.wall);
            }
            return false;
        }
        self.standing_read.placed = None;
        let Some(host) = opts.idle_host.as_ref() else {
            return false;
        };
        match reading.program {
            Program::Claude => {}
            Program::Codex | Program::Agy | Program::Muse | Program::Gemini | Program::Generic => {
                return false;
            }
        }
        let said = aterm_phase::phase::last_said_row(&screen.rows).map(str::to_owned);
        let whom = (host.agent_pid(), host.agent_conversation());
        let unasked = self.standing_read.asked_for.as_ref() != Some(&whom);
        if unasked || worked {
            self.standing_read.found = None;
        }
        let open = reading.wall.is_none() && matches!(reading.phase, Phase::Idle | Phase::Question);
        let again = !unasked
            && !worked
            && self
                .standing_read
                .found
                .as_ref()
                .is_some_and(|f| f.said != said || f.after_turn);
        let steps = host.wants();
        let asked = open && (unasked || again || steps || worked);
        if asked {
            self.standing_read.found = host.standing_limit().and_then(|limit| {
                let kind = limit_named(&limit.text, limit.window.as_deref())?;
                let message = limit_words(&limit.text, limit.window.as_deref());
                Some(Standing {
                    wall: aterm_phase::Wall {
                        kind,
                        reset: aterm_phase::notice_reset(&one_line(&limit.text)),
                        message,
                        row: aterm_phase::phase::last_said_index(&screen.rows).unwrap_or(0),
                        placement: aterm_phase::Placement::Gutter,
                        under_call: false,
                    },
                    until: limit.until,
                    row: limit.row,
                    said: said.clone(),
                    conversation: host
                        .agent_conversation()
                        .filter(|id| !id.is_empty())
                        .map_or(Conversation::Unknown, Conversation::Named),
                    after_turn: worked,
                })
            });
            self.standing_read.asked_for = Some((host.agent_pid(), host.agent_conversation()));
        }
        if open
            && self.standing_read.found.as_ref().is_some_and(|f| {
                ended_under(
                    aterm_phase::since_limit(&screen.rows).as_ref(),
                    f.wall.kind,
                    limit::ResetClock::of(f.wall.reset.as_deref()),
                )
            })
        {
            self.standing_read.found = None;
            return false;
        }
        if open && let Some(found) = self.standing_read.found.clone() {
            self.standing_read.placed = Some(screen.seq);
            self.wall_conversation = Some((screen.seq, found.conversation));
            self.limit_notice_row = found.row;
            place_standing(reading, found.wall);
            return !asked;
        }
        false
    }

    /// The reset of the limit only the transcript shows, placed on `seq`'s screen, unix seconds: where its row names it (`quotaLimits.resetsAt`).
    pub(super) fn standing_until(&self, seq: u64) -> Option<i64> {
        let found = self
            .standing_read
            .found
            .as_ref()
            .filter(|_| self.standing_read.placed == Some(seq))?;
        found.until.and_then(|at| i64::try_from(at).ok())
    }

    pub(super) fn limit_transcript(&self, opts: &SuperviseOpts) -> LimitTranscript {
        let Some(host) = opts.idle_host.as_ref() else {
            return LimitTranscript {
                conversation: Conversation::Hostless,
                served: false,
                chosen: None,
                elsewhere: false,
            };
        };
        let Some(tail) = host.transcript_tail() else {
            return LimitTranscript {
                conversation: Conversation::Unknown,
                served: false,
                chosen: None,
                elsewhere: false,
            };
        };
        let kept = self.limit_notice_row.as_deref();
        let row = match (kept, self.turn_end.waited_limit()) {
            (Some(notice), _) => Some(notice.to_owned()),
            (None, Some(w)) => waited_notice_row(&tail, &w, self.unix_of(w.at)),
            (None, None) => None,
        };
        let elsewhere = kept.is_some_and(|id| {
            !tail.lines().any(|line| {
                crate::harness::transcript::conversation(line)
                    .ok()
                    .and_then(|v| row_id(&v))
                    .as_deref()
                    == Some(id)
            })
        });
        LimitTranscript {
            conversation: conversation_of(&tail),
            served: row
                .as_ref()
                .is_some_and(|row| served_since_notice(&tail, row)),
            chosen: row.as_ref().and_then(|row| chosen_since_notice(&tail, row)),
            elsewhere,
        }
    }

    #[cfg(test)]
    pub(super) fn notice_elsewhere(&self, opts: &SuperviseOpts) -> bool {
        self.limit_transcript(opts).elsewhere
    }

    pub(super) fn transcript_wait(&mut self, reader: &dyn ScreenReader, screen: &Screen) -> bool {
        let rows = &screen.rows;
        let claude = match reader.program() {
            Program::Claude => true,
            Program::Codex | Program::Agy | Program::Muse | Program::Gemini | Program::Generic => {
                false
            }
        };
        if !claude
            || !aterm_phase::phase::status_unseen(rows)
            || aterm_phase::phase::last_said_row(rows)
                .is_some_and(|r| r.trim_start().starts_with('❯'))
        {
            self.transcript_wait_seq = None;
            return false;
        }
        if self.transcript_wait_seq == Some(screen.seq) {
            return true;
        }
        let waits = self.stall_host.as_ref().is_some_and(|host| {
            host.transcript_tail().is_some_and(|tail| {
                host.launched_at()
                    .is_some_and(|at| waits_on_background(&tail, at))
            })
        });
        self.transcript_wait_seq = waits.then_some(screen.seq);
        waits
    }

    pub(super) fn transcript_wait_over(
        &mut self,
        reader: &dyn ScreenReader,
        screen: &Screen,
    ) -> bool {
        let claude = match reader.program() {
            Program::Claude => true,
            Program::Codex | Program::Agy | Program::Muse | Program::Gemini | Program::Generic => {
                false
            }
        };
        if !claude || !aterm_phase::phase::carried_wait_alone(&screen.rows) {
            self.transcript_over_seq = None;
            return false;
        }
        if self.transcript_over_seq == Some(screen.seq) {
            return true;
        }
        let over = self.stall_host.as_ref().is_some_and(|host| {
            host.transcript_tail().is_some_and(|tail| {
                host.launched_at()
                    .is_some_and(|at| ended_with_nothing_pending(&tail, at))
            })
        });
        self.transcript_over_seq = over.then_some(screen.seq);
        over
    }

    fn under_call_on_screen(&mut self, w: &aterm_phase::Wall, screen: &Screen) -> bool {
        if w.under_call {
            return true;
        }
        if w.placement != aterm_phase::Placement::Gutter {
            return false;
        }
        let (rows, cursor_col, wall) = if screen.first == 0 {
            (screen.rows.clone(), screen.cursor_col, w.clone())
        } else {
            let Ok(whole) = self.grid() else {
                return true;
            };
            let read = self
                .reader(&whole.rows)
                .read(&whole.rows, Some(whole.cursor_col));
            match read.wall {
                Some(x) if !x.under_call && x.kind == w.kind && x.message == w.message => {
                    (whole.rows, whole.cursor_col, x)
                }
                _ => return true,
            }
        };
        if !aterm_phase::phase::owner_above_rows(&rows, wall.row) {
            return false;
        }
        let Ok(Some(mut joined)) = self.rows_up_to_owner(&rows) else {
            return true;
        };
        joined.extend(rows);
        let read = self.reader(&joined).read(&joined, Some(cursor_col));
        read.wall
            .is_none_or(|x| x.under_call || x.kind != wall.kind || x.message != wall.message)
    }

    pub(super) fn rows_up_to_owner(
        &mut self,
        rows: &[String],
    ) -> Result<Option<Vec<String>>, Fail> {
        self.rows_up_to(rows, &is_owner_row)
    }

    pub(super) fn rows_up_to(
        &mut self,
        rows: &[String],
        stop: &dyn Fn(&str) -> bool,
    ) -> Result<Option<Vec<String>>, Fail> {
        Ok(self.rows_up_to_at(rows, stop)?.map(|(above, _)| above))
    }

    pub(super) fn rows_up_to_at(
        &mut self,
        rows: &[String],
        stop: &dyn Fn(&str) -> bool,
    ) -> Result<Option<RowsAbove>, Fail> {
        if self.alt_screen_now()? {
            return Ok(self.archived_up_to(rows, stop)?.map(|above| (above, None)));
        }
        let screen_first = |s: &mut Self| -> Result<Option<u64>, Fail> {
            let r = s.call(&["search"])?;
            if s.unserved(&r) {
                return Err(Fail::Lost(format!("search failed: {}", r.stderr.trim())));
            }
            Ok(r.ok()
                .then(|| super::box_head::search_screen_first(&r))
                .flatten())
        };
        let Some(first) = screen_first(self)? else {
            return Ok(None);
        };
        if first == 0 {
            return Ok(Some((Vec::new(), Some(first))));
        }
        let mut above = Vec::new();
        let mut at = first;
        while at > 0 {
            if first - at >= OWNER_ROWS {
                return Ok(None);
            }
            at -= 1;
            let r = self.call(&["line", &at.to_string()])?;
            if self.unserved(&r) {
                return Err(Fail::Lost(format!("line failed: {}", r.stderr.trim())));
            }
            if !r.ok() {
                return Ok(None);
            }
            let line = r.stdout.lines().next().unwrap_or("");
            let text = line.strip_prefix("OK").unwrap_or(line);
            let text = text.strip_prefix(' ').unwrap_or(text).to_string();
            let owner = stop(&text);
            above.push(text);
            if owner {
                break;
            }
        }
        if screen_first(self)? != Some(first) || !same_rows(&self.grid()?.rows, rows) {
            return Ok(None);
        }
        above.reverse();
        Ok(Some((above, Some(first))))
    }

    fn archived_up_to(
        &mut self,
        rows: &[String],
        stop: &dyn Fn(&str) -> bool,
    ) -> Result<Option<Vec<String>>, Fail> {
        let read = |s: &mut Self, args: &[&str]| -> Result<Option<Offscreen>, Fail> {
            let mut call = vec!["offscreen"];
            call.extend_from_slice(args);
            let r = s.call(&call)?;
            if s.unserved(&r) {
                return Err(Fail::Lost(format!("offscreen failed: {}", r.stderr.trim())));
            }
            Ok(r.ok().then(|| parse_offscreen(&r).ok()).flatten())
        };
        let Some(head) = read(self, &["tail=1"])? else {
            return Ok(None);
        };
        let since = format!("since={}", head.last.saturating_sub(OWNER_ROWS));
        let tail = format!("tail={OWNER_ROWS}");
        let Some(off) = read(self, &[&since, &tail, "screen=1"])? else {
            return Ok(None);
        };
        let whole = off.alt
            && off.enabled
            && off.lost == 0
            && off.breaks == 0
            && off.back == 0
            && off.origin == head.origin
            && same_rows(&off.screen, rows);
        if !whole {
            return Ok(None);
        }
        Ok(off
            .archived
            .iter()
            .rposition(|r| stop(r))
            .map(|owner| off.archived[owner..].to_vec()))
    }

    pub(super) fn point_as_decided(&self, turn: &mut Turn) {
        let verdict = self
            .wall_verdict
            .as_ref()
            .filter(|v| v.seq == turn.screen.seq);
        let refused = verdict.is_some_and(|v| v.refused.is_some());
        let found = verdict
            .and_then(|v| v.found.as_ref())
            .filter(|w| w.kind.reads_limited());
        let named = verdict.and_then(|v| v.named.as_ref());
        if refused && matches!(turn.phase, Phase::Limited { .. }) {
            turn.phase = Phase::Idle;
        } else if let Some(w) = found.filter(|_| turn.phase == Phase::Idle) {
            turn.phase = Phase::Limited {
                message: w.message.clone(),
                reset: w.reset.clone(),
            };
        } else if let Some(n) = named
            && let Phase::Limited { message, .. } = &mut turn.phase
        {
            // The limit the transcript named: its words, the screen's reset.
            message.clone_from(&n.message);
        } else if !refused
            && turn.phase == Phase::Idle
            && let Some(l) = self.left_read.as_ref().filter(|l| l.seq == turn.screen.seq)
        {
            turn.phase = Phase::Limited {
                message: l.answer.wall.message.clone(),
                reset: l.answer.wall.reset.clone(),
            };
        }
        if matches!(turn.phase, Phase::Idle | Phase::Question)
            && self.standing_read.placed == Some(turn.screen.seq)
            && let Some(found) = self.standing_read.found.as_ref()
        {
            turn.phase = Phase::Limited {
                message: found.wall.message.clone(),
                reset: found.wall.reset.clone(),
            };
        }
    }

    pub(super) fn say_wall_refused(&mut self, review: &mut dyn Review) {
        if let Some(r) = self.wall_verdict.as_ref().and_then(|v| v.refused.as_ref())
            && let Some(line) = r.line.as_deref()
            && self.wall_refused_said.as_deref() != Some(r.key.as_str())
        {
            review.note(line);
            self.wall_refused_said = Some(r.key.clone());
        }
    }

    pub(super) fn wall_refusal_over(&mut self) {
        self.wall_refused_said = None;
    }
}

fn apply_verdict(
    reading: &mut aterm_phase::Reading,
    screen: &Screen,
    refused: bool,
    found: Option<aterm_phase::Wall>,
    named: Option<Named>,
) {
    // named by the notice that confirmed it ([`refined`]): the same row.
    if let Some(w) = found.filter(|f| {
        reading
            .wall
            .as_ref()
            .is_none_or(|w| w.row == f.row && w.message == f.message)
    }) {
        if w.kind.reads_limited() && reading.phase == Phase::Idle {
            reading.phase = Phase::Limited {
                message: w.message.clone(),
                reset: w.reset.clone(),
            };
        }
        reading.wall = Some(w);
        return;
    }
    if let Some(n) = named
        && let Some(w) = reading.wall.as_mut()
    {
        w.kind = n.kind;
        w.message.clone_from(&n.message);
        if let Phase::Limited { message, .. } = &mut reading.phase {
            *message = n.message;
        }
    }
    if !refused {
        return;
    }
    reading.wall = None;
    if matches!(reading.phase, Phase::Limited { .. }) {
        reading.phase = Phase::Idle;
    }
    if reading.phase == Phase::Idle {
        reading.phase_authoritative &= aterm_phase::phase::has_composer_frame(&screen.rows);
    }
}

#[cfg(test)]
#[path = "wall_confirm_tests.rs"]
mod tests;
