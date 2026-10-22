// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Metadata-only transcript reads. The whole row is still validated, including
//! discarded content, but a large tool input/result never becomes a Value tree.
//! Keep Value's tolerant types and last-key-wins rule: vendor fields with the
//! wrong shape read as absent, rather than invalidating unrelated metadata.

use std::fmt;

use aterm_json::{Map, Value};
use serde::de::{DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};

#[derive(Clone, Copy)]
enum Shape {
    Scalar,
    Object(&'static [Field]),
    /// A list, each element read by the shape: a message's content as its
    /// [`CONTENT_PART`]s ([`CONTENT`]), its [`CALL_PART`]s ([`CALLS`]) or its
    /// [`EXCERPT_PART`]s ([`EXCERPT`]).
    Seq(&'static Shape),
    /// [`Shape::Seq`] that keeps ONLY a list: any other value (a prompt's
    /// plain-string content) is validated and read as null, never copied —
    /// the step fold's [`STEP_PARTS`], which keeps no text (ruling S6).
    Parts(&'static Shape),
    /// [`Shape::Object`] that keeps ONLY an object: any other value (a bare
    /// string where a part belongs) is validated and read as null — the
    /// step fold's [`STEP_PART`].
    Record(&'static [Field]),
}

type Field = (&'static str, Shape);

const USAGE: Shape = Shape::Object(&[
    ("type", Shape::Scalar),
    ("subtype", Shape::Scalar),
    ("durationMs", Shape::Scalar),
    ("isSidechain", Shape::Scalar),
    ("isApiErrorMessage", Shape::Scalar),
    ("timestamp", Shape::Scalar),
    (
        "message",
        Shape::Object(&[
            ("id", Shape::Scalar),
            ("model", Shape::Scalar),
            ("usage", MESSAGE_USAGE),
        ]),
    ),
]);

/// A message's `usage` as the spend fold reads it ([`USAGE`]) and the step
/// fold counts a vendor turn's tokens from it ([`STEPS`]).
const MESSAGE_USAGE: Shape = Shape::Object(&[
    ("input_tokens", Shape::Scalar),
    ("output_tokens", Shape::Scalar),
    ("cache_creation_input_tokens", Shape::Scalar),
    ("cache_read_input_tokens", Shape::Scalar),
    // The cache writes by lifetime: one-hour writes list
    // higher (`usage::TokenUsage::cache_write_1h`).
    (
        "cache_creation",
        Shape::Object(&[
            ("ephemeral_5m_input_tokens", Shape::Scalar),
            ("ephemeral_1h_input_tokens", Shape::Scalar),
        ]),
    ),
    // What the price table does not price, so the spend
    // fold counts the message as approximate
    // (`spend::approx`): fast mode, US-only inference,
    // web search, a tier other than the standard one, any
    // iteration but a plain message — an advisor's, a
    // server-side fallback's (each entry's `type` only).
    ("speed", Shape::Scalar),
    ("inference_geo", Shape::Scalar),
    ("service_tier", Shape::Scalar),
    (
        "server_tool_use",
        Shape::Object(&[("web_search_requests", Shape::Scalar)]),
    ),
    (
        "iterations",
        Shape::Seq(&Shape::Object(&[("type", Shape::Scalar)])),
    ),
]);

const METADATA_FIELDS: [Field; 7] = [
    ("type", Shape::Scalar),
    ("isSidechain", Shape::Scalar),
    ("timestamp", Shape::Scalar),
    ("version", Shape::Scalar),
    ("cwd", Shape::Scalar),
    ("effort", Shape::Scalar),
    ("message", Shape::Object(&[("model", Shape::Scalar)])),
];
const METADATA: Shape = Shape::Object(&METADATA_FIELDS);
const COMMAND_FIELDS: [Field; 7] = {
    let mut fields = METADATA_FIELDS;
    fields[6] = (
        "message",
        Shape::Object(&[("model", Shape::Scalar), ("content", Shape::Scalar)]),
    );
    fields
};
const COMMAND: Shape = Shape::Object(&COMMAND_FIELDS);
const CONTENT_PART: Shape = Shape::Object(&[("type", Shape::Scalar), ("text", Shape::Scalar)]);
/// A message's content: its parts' type and text, or the plain string.
const CONTENT: Shape = Shape::Seq(&CONTENT_PART);
/// A tool call or a tool's result as the approval loop reads it
/// (`supervise::policy::box_call::pending_calls`): its kind, the call's id,
/// its tool, the file it names and a Bash call's `command` (the rm breaker's
/// too-many box is judged by it, `supervise::policy::box_command`), and the
/// id of the call a result answers. Never the call's other arguments
/// (`old_string`, `new_string`, `content`) nor the result's content.
const CALL_PART: Shape = Shape::Object(&[
    ("type", Shape::Scalar),
    ("id", Shape::Scalar),
    ("name", Shape::Scalar),
    ("tool_use_id", Shape::Scalar),
    (
        "input",
        Shape::Object(&[
            ("file_path", Shape::Scalar),
            ("notebook_path", Shape::Scalar),
            ("command", Shape::Scalar),
        ]),
    ),
]);
/// A row as the approval loop reads it for the calls still waiting on a
/// result: its type, its own `uuid` and its `parentUuid` (the conversation's
/// order, which a first turn's file does not keep), whether it is a
/// subagent's, and its content's calls and results (a prompt's plain string
/// kept as it is).
const CALLS: Shape = Shape::Object(&[
    ("type", Shape::Scalar),
    ("uuid", Shape::Scalar),
    ("parentUuid", Shape::Scalar),
    ("isSidechain", Shape::Scalar),
    (
        "message",
        Shape::Object(&[("content", Shape::Seq(&CALL_PART))]),
    ),
]);
/// A call or a result as the flight fold reads it (`super::flight`): its
/// kind, the call's id and tool, the id of the call a result answers and
/// whether it answers with an error, and of the call's arguments only the
/// two that bound how long it may run — its declared `timeout` and whether
/// it went to the background. Never what it runs, writes or returns; a part
/// that is no object is read as null.
const FLIGHT_PART: Shape = Shape::Record(&[
    ("type", Shape::Scalar),
    ("id", Shape::Scalar),
    ("name", Shape::Scalar),
    ("tool_use_id", Shape::Scalar),
    ("is_error", Shape::Scalar),
    (
        "input",
        Shape::Record(&[
            ("timeout", Shape::Scalar),
            ("run_in_background", Shape::Scalar),
        ]),
    ),
]);
/// A row as the flight fold reads it: its type and time, its message's id and
/// stop reason, and its content's [`FLIGHT_PART`]s (a prompt's plain string
/// validated and read as null, never copied).
const FLIGHT: Shape = Shape::Object(&[
    ("type", Shape::Scalar),
    ("timestamp", Shape::Scalar),
    (
        "message",
        Shape::Object(&[
            ("id", Shape::Scalar),
            ("stop_reason", Shape::Scalar),
            ("content", Shape::Parts(&FLIGHT_PART)),
        ]),
    ),
]);
/// What is kept of a limit row's `quotaLimits`: whether the request was
/// rejected, the reset it names, which of the account's limits it was, and
/// what it says of the account's usage credits — whether they are on
/// (`overageStatus`), why not (`overageDisabledReason`), when they come back
/// (`overageResetsAt`) and whether they were in use (`isUsingOverage`), what
/// the accounts recorder keeps of them (`upgrade::CreditsWord`). MEASURED on
/// this machine's transcripts (2026-10-01: every one of the ten rows that
/// carry the key, Claude Code 2.1.273 and 2.1.283; 2.1.286's row is the
/// accounts design §1's): `{"status":"rejected","resetsAt":<unix s, an
/// integer>,"unifiedRateLimitFallbackAvailable":false,
/// "rateLimitType":"five_hour" | "seven_day","overageStatus":…,
/// "overageDisabledReason":…,"isUsingOverage":false}`, five of 2.1.273's
/// with three `lowPriority…` keys besides. None of those other keys is read
/// by anything here, and none is kept.
const QUOTA_LIMITS: Shape = Shape::Object(&[
    ("status", Shape::Scalar),
    ("resetsAt", Shape::Scalar),
    ("rateLimitType", Shape::Scalar),
    ("overageStatus", Shape::Scalar),
    ("overageDisabledReason", Shape::Scalar),
    ("overageResetsAt", Shape::Scalar),
    ("isUsingOverage", Shape::Scalar),
]);
const CONVERSATION: Shape = Shape::Object(&[
    ("type", Shape::Scalar),
    // `null` on a conversation's first row; read by `supervise/wall_confirm.rs`.
    ("parentUuid", Shape::Scalar),
    // A confirmed limit notice is remembered by its row id; only a reply after that row ends the
    // wait (`supervise/wall_confirm.rs`'s `served_since_notice`).
    ("uuid", Shape::Scalar),
    // The same tail names the conversation that owns a limit notice.
    ("sessionId", Shape::Scalar),
    ("isSidechain", Shape::Scalar),
    ("isMeta", Shape::Scalar),
    ("isCompactSummary", Shape::Scalar),
    ("queueTranscriptOnly", Shape::Scalar),
    ("isApiErrorMessage", Shape::Scalar),
    ("error", Shape::Scalar),
    // A notice's typed kind (since 2.1.295) and the status it answered: what
    // names the wall a notice is where its words do not
    // (`supervise/wall_confirm.rs`'s `transcript_wall`).
    ("apiError", Shape::Scalar),
    ("apiErrorStatus", Shape::Scalar),
    ("timestamp", Shape::Scalar),
    ("subtype", Shape::Scalar),
    // A `turn_duration` row's count of the background agents and dynamic
    // workflows the turn ended waiting on: the supervisor's carried wait
    // (`supervise/wall_confirm.rs`'s `waits_on_background`).
    ("pendingBackgroundAgentCount", Shape::Scalar),
    ("pendingWorkflowCount", Shape::Scalar),
    // …and how long that turn worked: what a person's exchange retried at
    // a wall is bounded by (`upgrade::LastTurn::names`).
    ("durationMs", Shape::Scalar),
    ("content", Shape::Scalar),
    // Who asked a user row's turn (`task-notification`, `auto-continuation`,
    // `peer`, `human`): Claude Code's own wakes reopen no finished task
    // (`upgrade::FinishedScan`).
    ("origin", Shape::Object(&[("kind", Shape::Scalar)])),
    // The turn a user row was written in: a row that asks nothing under the
    // running turn's id (a Stop hook's feedback) goes on that turn, and a
    // prompt typed while the agent worked is recorded only as an attachment
    // absorbed into it (`queued_command`; `upgrade::FinishedScan`).
    ("promptId", Shape::Scalar),
    // The scheduled task a `/loop`'s wake fired for, stamped on the wake's
    // user row (the 2.1.252 and 2.1.267 bundles), and the turn's origin
    // where Claude Code names one (`scheduled`, `auto_continuation`, `peer`;
    // never a Stop hook's feedback): a turn of its own on a row with no
    // `promptId` (`upgrade::begins_unnumbered`).
    ("scheduledTaskId", Shape::Scalar),
    ("turnOrigin", Shape::Scalar),
    (
        "attachment",
        Shape::Object(&[
            ("type", Shape::Scalar),
            ("prompt", CONTENT),
            ("origin", Shape::Object(&[("kind", Shape::Scalar)])),
        ]),
    ),
    // A system row's level, and the build that wrote a row: the resumed
    // build's warning that something did not come back
    // (`upgrade::resumed_build_warned`).
    ("level", Shape::Scalar),
    ("version", Shape::Scalar),
    ("quotaLimits", QUOTA_LIMITS),
    (
        "message",
        Shape::Object(&[
            ("model", Shape::Scalar),
            ("stop_reason", Shape::Scalar),
            ("content", CONTENT),
        ]),
    ),
]);

/// A tool call or a tool's result as the session log's step fold reads it
/// (`harness::steps`): its kind, the call's id and tool, the id of the call
/// a result answers and whether it failed. Never a call's `input`, never a
/// result's `content`, never a text part's `text` (ruling S6).
const STEP_PART: Shape = Shape::Record(&[
    ("type", Shape::Scalar),
    ("id", Shape::Scalar),
    ("name", Shape::Scalar),
    ("tool_use_id", Shape::Scalar),
    ("is_error", Shape::Scalar),
]);
/// A message's content as the step fold keeps it: its parts, and nothing
/// of a plain-string content (a prompt) or of a bare string among the
/// parts, each read as null.
const STEP_PARTS: Shape = Shape::Parts(&STEP_PART);
/// What the step fold reads of every row (addendum A8): [`USAGE`]'s keys,
/// the row's own id, its parent's, the vendor turn's (`promptId`), its
/// meta and compaction flags, who asked (`origin.kind`), whether Claude Code
/// began the turn of its own on a row with no `promptId` (`turnOrigin`,
/// `scheduledTaskId`: `upgrade::begins_unnumbered_by`), and the build and
/// effort that wrote it. [`STEPS`] adds the content's parts; this, read for
/// a row that holds no call or result, never visits the content at all.
const STEPS_BARE_FIELDS: [Field; 18] = [
    ("type", Shape::Scalar),
    ("subtype", Shape::Scalar),
    ("durationMs", Shape::Scalar),
    ("isSidechain", Shape::Scalar),
    ("isApiErrorMessage", Shape::Scalar),
    ("timestamp", Shape::Scalar),
    ("uuid", Shape::Scalar),
    ("parentUuid", Shape::Scalar),
    ("promptId", Shape::Scalar),
    ("isMeta", Shape::Scalar),
    ("isCompactSummary", Shape::Scalar),
    ("queueTranscriptOnly", Shape::Scalar),
    ("origin", Shape::Object(&[("kind", Shape::Scalar)])),
    ("turnOrigin", Shape::Scalar),
    ("scheduledTaskId", Shape::Scalar),
    ("version", Shape::Scalar),
    ("effort", Shape::Scalar),
    (
        "message",
        Shape::Object(&[
            ("id", Shape::Scalar),
            ("model", Shape::Scalar),
            ("usage", MESSAGE_USAGE),
        ]),
    ),
];
const STEPS_BARE: Shape = Shape::Object(&STEPS_BARE_FIELDS);
/// [`STEPS_BARE`] and the content's parts ([`STEP_PARTS`]): for a row
/// whose raw bytes hold a call's or a result's key (`steps::wants_parts`).
const STEPS_FIELDS: [Field; 18] = {
    let mut fields = STEPS_BARE_FIELDS;
    fields[17] = (
        "message",
        Shape::Object(&[
            ("id", Shape::Scalar),
            ("model", Shape::Scalar),
            ("usage", MESSAGE_USAGE),
            ("content", STEP_PARTS),
        ]),
    );
    fields
};
const STEPS: Shape = Shape::Object(&STEPS_FIELDS);

/// What the survey's excerpts read of a part ([`excerpt`]): its kind, a text
/// part's text, a tool call's name and id, a tool result's call id. Never a
/// tool's input, a result's content or an image.
const EXCERPT_PART: [Field; 5] = [
    ("type", Shape::Scalar),
    ("text", Shape::Scalar),
    ("name", Shape::Scalar),
    ("id", Shape::Scalar),
    ("tool_use_id", Shape::Scalar),
];
const EXCERPT: Shape = Shape::Object(&[
    ("type", Shape::Scalar),
    ("isSidechain", Shape::Scalar),
    ("isMeta", Shape::Scalar),
    ("isCompactSummary", Shape::Scalar),
    ("queueTranscriptOnly", Shape::Scalar),
    ("isApiErrorMessage", Shape::Scalar),
    (
        "message",
        Shape::Object(&[
            ("id", Shape::Scalar),
            ("model", Shape::Scalar),
            ("content", Shape::Seq(&Shape::Object(&EXCERPT_PART))),
        ]),
    ),
]);

/// What the worker's result reader keeps of a row (`super::result`): whose
/// row it is and which message it belongs to (`uuid`, `sessionId`,
/// `message.id` — Claude Code writes one row per content block, all under
/// the message's one id), when it was written (`timestamp`: a turn begun
/// after the done point is no turn of that done's), the flags that make it
/// no one's words, and the text blocks. Tool inputs and results are
/// validated, never kept.
const EXCHANGE: Shape = Shape::Object(&[
    ("type", Shape::Scalar),
    ("uuid", Shape::Scalar),
    ("timestamp", Shape::Scalar),
    ("sessionId", Shape::Scalar),
    ("isSidechain", Shape::Scalar),
    ("isMeta", Shape::Scalar),
    ("isCompactSummary", Shape::Scalar),
    ("queueTranscriptOnly", Shape::Scalar),
    ("isApiErrorMessage", Shape::Scalar),
    (
        "message",
        Shape::Object(&[
            ("id", Shape::Scalar),
            ("model", Shape::Scalar),
            ("content", CONTENT),
        ]),
    ),
]);

/// One token count of a Codex turn, in OpenAI's classes: `input_tokens`
/// holds `cached_input_tokens`, and `output_tokens` holds
/// `reasoning_output_tokens`.
const CODEX_TOKENS: Shape = Shape::Object(&[
    ("input_tokens", Shape::Scalar),
    ("cached_input_tokens", Shape::Scalar),
    ("cache_write_input_tokens", Shape::Scalar),
    ("output_tokens", Shape::Scalar),
    ("reasoning_output_tokens", Shape::Scalar),
    ("total_tokens", Shape::Scalar),
]);

/// What the Codex spend fold reads of a rollout line
/// (`harness::codex_rollout::TurnReader`): its stamp, a `session_meta`'s
/// thread, fork parent and history mode, a `thread_settings_applied`'s
/// thread, a `turn_context`'s model and service tier, and a `token_count`'s
/// running total and last turn. Instructions, tool payloads, messages, the
/// thread settings and the rate limits are validated, never materialized.
const CODEX_SPEND: Shape = Shape::Object(&[
    ("type", Shape::Scalar),
    ("timestamp", Shape::Scalar),
    (
        "payload",
        Shape::Object(&[
            ("type", Shape::Scalar),
            ("id", Shape::Scalar),
            ("thread_id", Shape::Scalar),
            ("forked_from_id", Shape::Scalar),
            ("history_mode", Shape::Scalar),
            ("model", Shape::Scalar),
            ("service_tier", Shape::Scalar),
            (
                "info",
                Shape::Object(&[
                    ("total_token_usage", CODEX_TOKENS),
                    ("last_token_usage", CODEX_TOKENS),
                ]),
            ),
        ]),
    ),
]);

struct CodexSpend(Value);

impl<'de> Deserialize<'de> for CodexSpend {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        CODEX_SPEND.deserialize(de).map(Self)
    }
}

pub(crate) fn codex_spend(line: &str) -> aterm_json::Result<Value> {
    aterm_json::from_str::<CodexSpend>(line).map(|v| v.0)
}

/// What the Codex limits reader reads of a rollout's first line
/// (`ai::codex_limits::opening_account`): its stamp, and of its
/// `session_meta` the thread, its conversation, its fork parent, its history
/// mode and the two ids the account key is made from (hashed by the reader,
/// never kept). The base instructions (about 20 KB) are validated, never
/// materialized.
const CODEX_META: Shape = Shape::Object(&[
    ("type", Shape::Scalar),
    ("timestamp", Shape::Scalar),
    (
        "payload",
        Shape::Object(&[
            ("id", Shape::Scalar),
            ("session_id", Shape::Scalar),
            ("forked_from_id", Shape::Scalar),
            ("history_mode", Shape::Scalar),
            ("creator_account_id", Shape::Scalar),
            ("creator_user_id", Shape::Scalar),
        ]),
    ),
]);

struct CodexMeta(Value);

impl<'de> Deserialize<'de> for CodexMeta {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        CODEX_META.deserialize(de).map(Self)
    }
}

pub(crate) fn codex_meta(line: &str) -> aterm_json::Result<Value> {
    aterm_json::from_str::<CodexMeta>(line).map(|v| v.0)
}

/// One window of a `token_count`'s `rate_limits` slot.
const CODEX_WINDOW: Shape = Shape::Object(&[
    ("used_percent", Shape::Scalar),
    ("window_minutes", Shape::Scalar),
    ("resets_at", Shape::Scalar),
]);

/// What the Codex limits reader reads of a `token_count`
/// (`ai::codex_limits::rollout_rate_limits`): its stamp and its
/// `rate_limits`, every key Codex 0.163.0 writes there but
/// `normal_model_slug` (design F10). `individual_limit` is kept only as a
/// scalar: no capture has carried anything but null. The turn's counts
/// (`info`) are validated, never materialized.
const CODEX_LIMITS: Shape = Shape::Object(&[
    ("type", Shape::Scalar),
    ("timestamp", Shape::Scalar),
    (
        "payload",
        Shape::Object(&[
            ("type", Shape::Scalar),
            (
                "rate_limits",
                Shape::Object(&[
                    ("limit_id", Shape::Scalar),
                    ("limit_name", Shape::Scalar),
                    ("primary", CODEX_WINDOW),
                    ("secondary", CODEX_WINDOW),
                    (
                        "credits",
                        Shape::Object(&[
                            ("has_credits", Shape::Scalar),
                            ("unlimited", Shape::Scalar),
                            ("balance", Shape::Scalar),
                        ]),
                    ),
                    ("individual_limit", Shape::Scalar),
                    ("spend_control_reached", Shape::Scalar),
                    ("plan_type", Shape::Scalar),
                    ("rate_limit_reached_type", Shape::Scalar),
                ]),
            ),
        ]),
    ),
]);

struct CodexLimits(Value);

impl<'de> Deserialize<'de> for CodexLimits {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        CODEX_LIMITS.deserialize(de).map(Self)
    }
}

pub(crate) fn codex_limits(line: &str) -> aterm_json::Result<Value> {
    aterm_json::from_str::<CodexLimits>(line).map(|v| v.0)
}

/// What the session log's Codex step fold reads of a rollout line
/// (`harness::codex_steps`): its kind and stamp, and of its payload the
/// kind, the thread a `session_meta` is (`id`), its fork parent and build,
/// a `thread_settings_applied`'s thread, a turn's id, a call's id, tool and
/// namespace, a `turn_context`'s model and effort, a turn's duration, the
/// closed word of a turn's error (never its message), and a `token_count`'s
/// running total and last turn. A call's arguments and input, a result's
/// output, messages, instructions, settings, a compaction's history and the
/// rate limits are validated, never materialized (ruling S6).
const CODEX_STEPS: Shape = Shape::Object(&[
    ("type", Shape::Scalar),
    ("timestamp", Shape::Scalar),
    (
        "payload",
        Shape::Object(&[
            ("type", Shape::Scalar),
            ("id", Shape::Scalar),
            ("forked_from_id", Shape::Scalar),
            ("cli_version", Shape::Scalar),
            ("thread_id", Shape::Scalar),
            ("turn_id", Shape::Scalar),
            ("call_id", Shape::Scalar),
            ("name", Shape::Scalar),
            ("namespace", Shape::Scalar),
            ("model", Shape::Scalar),
            ("effort", Shape::Scalar),
            ("duration_ms", Shape::Scalar),
            (
                "error",
                Shape::Object(&[("codex_error_info", Shape::Scalar)]),
            ),
            (
                "info",
                Shape::Object(&[
                    ("total_token_usage", CODEX_TOKENS),
                    ("last_token_usage", CODEX_TOKENS),
                ]),
            ),
        ]),
    ),
]);

struct CodexSteps(Value);

impl<'de> Deserialize<'de> for CodexSteps {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        CODEX_STEPS.deserialize(de).map(Self)
    }
}

pub(crate) fn codex_steps(line: &str) -> aterm_json::Result<Value> {
    aterm_json::from_str::<CodexSteps>(line).map(|v| v.0)
}

struct Usage(Value);
struct Metadata<const CONTENT: bool>(Value);
struct Conversation(Value);
struct Exchange(Value);

impl<'de> Deserialize<'de> for Exchange {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        EXCHANGE.deserialize(de).map(Self)
    }
}
struct Excerpt(Value);
struct Calls(Value);
struct Steps<const PARTS: bool>(Value);
struct Flight(Value);

impl<'de> Deserialize<'de> for Flight {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        FLIGHT.deserialize(de).map(Self)
    }
}

impl<'de> Deserialize<'de> for Usage {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        USAGE.deserialize(de).map(Self)
    }
}

impl<'de, const CONTENT: bool> Deserialize<'de> for Metadata<CONTENT> {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let shape = if CONTENT { COMMAND } else { METADATA };
        shape.deserialize(de).map(Self)
    }
}

impl<'de> Deserialize<'de> for Conversation {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        CONVERSATION.deserialize(de).map(Self)
    }
}

impl<'de> Deserialize<'de> for Excerpt {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        EXCERPT.deserialize(de).map(Self)
    }
}

impl<'de> Deserialize<'de> for Calls {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        CALLS.deserialize(de).map(Self)
    }
}

impl<'de, const PARTS: bool> Deserialize<'de> for Steps<PARTS> {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let shape = if PARTS { STEPS } else { STEPS_BARE };
        shape.deserialize(de).map(Self)
    }
}

pub(super) fn usage(line: &str) -> aterm_json::Result<Value> {
    aterm_json::from_str::<Usage>(line).map(|v| v.0)
}

pub(super) fn metadata(line: &str) -> aterm_json::Result<Value> {
    aterm_json::from_str::<Metadata<false>>(line).map(|v| v.0)
}

/// A `/model` command additionally needs its plain-string content. Block
/// arrays (tool calls/results) still need no materialized content.
pub(super) fn command(line: &str) -> aterm_json::Result<Value> {
    aterm_json::from_str::<Metadata<true>>(line).map(|v| v.0)
}

/// The upgrade/task readers additionally need text blocks and the vendor's
/// own error/limit metadata, but never tool arguments, images, or results.
pub(crate) fn conversation(line: &str) -> aterm_json::Result<Value> {
    aterm_json::from_str::<Conversation>(line).map(|v| v.0)
}

/// The survey's excerpts ([`super::excerpt`]) need a row's discriminants, its
/// message id, text parts and tool call names and ids, but never a tool's
/// input or result.
pub(super) fn excerpt(line: &str) -> aterm_json::Result<Value> {
    aterm_json::from_str::<Excerpt>(line).map(|v| v.0)
}

/// The worker's result reader (`super::result`) needs each row's own id, its
/// conversation's, its message's id and text blocks, never tool payloads.
pub(super) fn exchange(line: &str) -> aterm_json::Result<Value> {
    aterm_json::from_str::<Exchange>(line).map(|v| v.0)
}

/// The approval loop needs which calls wait on a result in the conversation's
/// order, their tools, the files they name and a Bash call's command, but
/// never what a call writes or a result holds.
pub(crate) fn calls(line: &str) -> aterm_json::Result<Value> {
    aterm_json::from_str::<Calls>(line).map(|v| v.0)
}

/// The flight fold's read of a row (`super::flight`): which calls of a
/// subagent wait on a result, their tools and declared timeouts, never what
/// a call runs or a result holds.
pub(crate) fn flight(line: &str) -> aterm_json::Result<Value> {
    aterm_json::from_str::<Flight>(line).map(|v| v.0)
}

/// The step fold's read of a row that holds a call or a result: ids,
/// times, flags, usage and the content's parts, never any text.
pub(crate) fn steps(line: &str) -> aterm_json::Result<Value> {
    aterm_json::from_str::<Steps<true>>(line).map(|v| v.0)
}

/// The step fold's read of any other row: [`steps`] without the content,
/// which the parser passes over without building it.
pub(crate) fn steps_bare(line: &str) -> aterm_json::Result<Value> {
    aterm_json::from_str::<Steps<false>>(line).map(|v| v.0)
}

/// EVERY METADATA READ THE HARNESS MAKES OF ONE CLAUDE CODE TRANSCRIPT ROW:
/// the row as each reader's shape above keeps it, in a fixed order (`None`
/// where the line is no row that shape reads). Every reader here decides
/// what it decides of a row from one of these (module docs: a row is never
/// read whole), so two rows whose reads are equal read alike to each of
/// them. What a reduction of a transcript is held to (`aterm-lab reduce`,
/// session log ruling U7): a closed word one of these keeps is a word a
/// committed reader reads, and is kept; a word none keeps is read by
/// nothing here.
#[must_use]
pub fn claude_reads(line: &str) -> Vec<Option<Value>> {
    [
        usage,
        metadata,
        command,
        conversation,
        excerpt,
        exchange,
        calls,
        steps,
        steps_bare,
    ]
    .iter()
    .map(|read| read(line).ok())
    .collect()
}

/// [`claude_reads`] for a Codex rollout's line: the spend fold's, the
/// limits reader's (its first line and its `token_count`s) and the step
/// fold's shapes.
#[must_use]
pub fn codex_reads(line: &str) -> Vec<Option<Value>> {
    [codex_spend, codex_meta, codex_limits, codex_steps]
        .iter()
        .map(|read| read(line).ok())
        .collect()
}

impl<'de> DeserializeSeed<'de> for Shape {
    type Value = Value;

    fn deserialize<D: Deserializer<'de>>(self, de: D) -> Result<Value, D::Error> {
        de.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Shape {
    type Value = Value;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a transcript value")
    }

    fn visit_unit<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_bool<E>(self, v: bool) -> Result<Value, E> {
        Ok(self.scalar(Value::Bool(v)))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Value, E> {
        Ok(self.scalar(Value::from(v)))
    }

    fn visit_i64<E>(self, v: i64) -> Result<Value, E> {
        Ok(self.scalar(Value::from(v)))
    }

    fn visit_f64<E>(self, v: f64) -> Result<Value, E> {
        Ok(self.scalar(Value::from(v)))
    }

    fn visit_str<E>(self, v: &str) -> Result<Value, E> {
        if let Shape::Parts(_) | Shape::Record(_) = self {
            return Ok(Value::Null);
        }
        Ok(Value::String(v.to_owned()))
    }

    fn visit_string<E>(self, v: String) -> Result<Value, E> {
        Ok(self.scalar(Value::String(v)))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        if let Shape::Seq(element) | Shape::Parts(element) = self {
            let mut kept = Vec::new();
            while let Some(one) = seq.next_element_seed(*element)? {
                kept.push(one);
            }
            return Ok(Value::Array(kept));
        }
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(Value::Null)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let (Shape::Object(fields) | Shape::Record(fields)) = self else {
            while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
            return Ok(Value::Null);
        };
        let mut kept = Map::new();
        while let Some(field) = map.next_key_seed(FieldName(fields))? {
            if let Some((name, shape)) = field {
                kept.insert(name.to_owned(), map.next_value_seed(shape)?);
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(Value::Object(kept))
    }
}

impl Shape {
    /// A scalar as this shape keeps it: as it is, but under
    /// [`Shape::Parts`] and [`Shape::Record`], which keep only a list and
    /// only an object.
    fn scalar(self, v: Value) -> Value {
        match self {
            Shape::Parts(_) | Shape::Record(_) => Value::Null,
            _ => v,
        }
    }
}

/// Resolve a key without allocating a String for every discarded field.
struct FieldName(&'static [Field]);

impl<'de> DeserializeSeed<'de> for FieldName {
    type Value = Option<Field>;

    fn deserialize<D: Deserializer<'de>>(self, de: D) -> Result<Self::Value, D::Error> {
        de.deserialize_identifier(self)
    }
}

impl<'de> Visitor<'de> for FieldName {
    type Value = Option<Field>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a transcript field name")
    }

    fn visit_str<E>(self, key: &str) -> Result<Self::Value, E> {
        Ok(self.0.iter().find(|(name, _)| *name == key).copied())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(value: Value, shape: Shape) -> Value {
        match (value, shape) {
            (Value::Array(parts), Shape::Seq(element) | Shape::Parts(element)) => {
                Value::Array(parts.into_iter().map(|p| project(p, *element)).collect())
            }
            (_, Shape::Parts(_)) => Value::Null,
            (Value::Object(mut obj), Shape::Object(fields) | Shape::Record(fields)) => {
                Value::Object(
                    fields
                        .iter()
                        .filter_map(|&(name, shape)| {
                            obj.remove(name)
                                .map(|v| (name.to_owned(), project(v, shape)))
                        })
                        .collect(),
                )
            }
            (Value::Object(_) | Value::Array(_), _) | (_, Shape::Record(_)) => Value::Null,
            (scalar, _) => scalar,
        }
    }

    #[test]
    fn projections_match_full_values_including_vendor_type_drift_and_duplicates() {
        for value in [
            "null",
            "true",
            "false",
            "0",
            "-1",
            "-0",
            "1.5",
            "1e3",
            "[]",
            "{}",
            r#""text\n\uD83D\uDE80""#,
            r#"{"model":"opus","content":[{"type":"tool_use","input":{"rows":[1,2]}}],"usage":{"input_tokens":42}}"#,
            r#"{"model":"opus","content":[{"type":"tool_result","content":[{"text":"not a prompt"}]},null,12,"scalar",["nested"],{"type":"text","text":"first"},{"type":"text","text":"second"}]}"#,
            r#"{"content":[{"type":"tool_use","id":"toolu_1","name":"Edit","input":{"file_path":"/p/notes.txt","old_string":"seed","new_string":"sprout","file_path":{"wrong":1}}},{"type":"tool_result","tool_use_id":"toolu_1","content":[{"type":"text","text":"done"}]},{"type":"tool_use","id":2,"name":["Bash"],"input":"sed -i"}]}"#,
        ] {
            for text in [
                value.to_owned(),
                format!(r#"{{"type":{value},"payload":{value}}}"#),
                format!(
                    r#"{{"payload":{{"type":{value},"role":{value},"cwd":{value},"model":{value},"sandbox_policy":{{"type":{value}}}}}}}"#
                ),
                format!(
                    r#"{{"type":"ignored","t\u0079pe":"turn_context","payload":null,"payload":{{"cwd":"/old","c\u0077d":{value},"sandbox_policy":{{"type":"read-only","type":{value}}}}}}}"#
                ),
                format!(
                    r#"{{"message":{value},"effort":{value},"cwd":{value},"isApiErrorMessage":{value},"timestamp":{value}}}"#
                ),
                format!(r#"{{"message":{{"model":{value},"usage":{value},"content":{value}}}}}"#),
                format!(
                    r#"{{"message":{{"usage":{{"iterations":[{value},{{"type":{value},"input_tokens":1}}],"speed":{value}}}}}}}"#
                ),
                format!(r#"{{"message":{{"usage":{{"iterations":{value}}}}}}}"#),
                format!(
                    r#"{{"payload":{{"forked_from_id":{value},"service_tier":{value},"info":{value}}}}}"#
                ),
                format!(
                    r#"{{"type":"response_item","payload":{{"type":"function_call","call_id":{value},"name":{value},"namespace":{value},"arguments":{value},"turn_id":{value},"cli_version":{value},"effort":{value},"duration_ms":{value},"error":{value}}}}}"#
                ),
                format!(
                    r#"{{"payload":{{"type":"task_complete","error":{{"message":{value},"codex_error_info":{value},"codex_error_info":"usage_limit_exceeded"}},"last_agent_message":{value}}}}}"#
                ),
                format!(
                    r#"{{"payload":{{"info":{{"total_token_usage":{value},"last_token_usage":{{"input_tokens":{value},"total_tokens":3,"total_tokens":{value}}},"rate_limits":{value}}}}}}}"#
                ),
                format!(
                    r#"{{"payload":{{"type":{value},"id":{value},"thread_id":{value},"history_mode":{value},"creator_account_id":{value},"creator_user_id":"u","rate_limits":{{"limit_id":{value},"primary":{{"used_percent":{value},"resets_at":3,"resets_at":{value}}},"secondary":{value},"credits":{{"balance":{value}}},"individual_limit":{value},"normal_model_slug":{value}}}}}}}"#
                ),
                format!(
                    r#"{{"type":"ignored","t\u0079pe":"assistant","message":null,"message":{{"model":"opus","usage":{{"input_tokens":3,"input_tokens":{value}}}}}}}"#
                ),
            ] {
                let full: Value = aterm_json::from_str(&text).expect("fixture");
                assert_eq!(usage(&text).expect("usage"), project(full.clone(), USAGE));
                assert_eq!(
                    metadata(&text).expect("metadata"),
                    project(full.clone(), METADATA)
                );
                assert_eq!(
                    command(&text).expect("command"),
                    project(full.clone(), COMMAND)
                );
                assert_eq!(
                    conversation(&text).expect("conversation"),
                    project(full.clone(), CONVERSATION)
                );
                assert_eq!(calls(&text).expect("calls"), project(full.clone(), CALLS));
                assert_eq!(steps(&text).expect("steps"), project(full.clone(), STEPS));
                assert_eq!(
                    steps_bare(&text).expect("bare steps"),
                    project(full.clone(), STEPS_BARE)
                );
                assert_eq!(
                    exchange(&text).expect("exchange"),
                    project(full.clone(), EXCHANGE)
                );
                assert_eq!(
                    excerpt(&text).expect("excerpt"),
                    project(full.clone(), EXCERPT)
                );
                assert_eq!(
                    codex_spend(&text).expect("Codex spend"),
                    project(full.clone(), CODEX_SPEND)
                );
                assert_eq!(
                    codex_meta(&text).expect("Codex meta"),
                    project(full.clone(), CODEX_META)
                );
                assert_eq!(
                    codex_limits(&text).expect("Codex limits"),
                    project(full.clone(), CODEX_LIMITS)
                );
                assert_eq!(
                    codex_steps(&text).expect("Codex steps"),
                    project(full, CODEX_STEPS)
                );
            }
        }
    }

    #[test]
    fn discarded_fields_are_still_fully_validated() {
        for bad in [
            r#""\uD800""#,
            r#""\x""#,
            "[1,]",
            r#"{"a":1,}"#,
            "01",
            "1e9999",
            "true false",
        ] {
            for name in ["unknown", "message"] {
                let text = format!(r#"{{"type":"assistant","{name}":{bad}}}"#);
                assert!(usage(&text).is_err(), "{text}");
                assert!(metadata(&text).is_err(), "{text}");
                assert!(conversation(&text).is_err(), "{text}");
                assert!(excerpt(&text).is_err(), "{text}");
                assert!(exchange(&text).is_err(), "{text}");
                assert!(calls(&text).is_err(), "{text}");
                assert!(steps(&text).is_err(), "{text}");
                assert!(steps_bare(&text).is_err(), "{text}");
                assert!(codex_spend(&text).is_err(), "{text}");
                assert!(codex_meta(&text).is_err(), "{text}");
                assert!(codex_limits(&text).is_err(), "{text}");
                assert!(codex_steps(&text).is_err(), "{text}");
            }
        }
        let deep = format!("{}0{}", "[".repeat(130), "]".repeat(130));
        assert!(metadata(&format!(r#"{{"unknown":{deep}}}"#)).is_err());
    }
}

#[path = "transcript_corpus_tests.rs"]
#[cfg(test)]
pub(crate) mod corpus_tests;
