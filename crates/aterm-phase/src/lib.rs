// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **aterm-phase** — what an agent worker (Claude Code, Codex) is doing RIGHT
//! NOW, from one screen read, as pure functions over the screen's rows.
//!
//! * [`phase::worker_phase`] — busy | prompt | limited | idle | question, read
//!   from the LIVE ZONE around the composer (the status row above its top
//!   rule, the hints under it, the footer under its bottom rule), never from
//!   the transcript above it;
//! * [`phase::context_left`] — the `<n>% until auto-compact` indicator, when
//!   Claude Code shows it;
//! * [`phase::survey_open`], [`survey::survey_block`], [`phase::transcript_end`],
//!   [`phase::last_said_row`] — the rest of the live-zone geometry the supervisor's report views use,
//!   and [`phase::busy_zone_start`], the row its waits on a busy row start
//!   at;
//! * [`prompt::parse_prompt`] — the approval box, parsed; [`prompt::parse_prompt_v2`]
//!   — the same box with its title, command rows, option roles and focus, how
//!   an option is chosen and what cancelling does; its kind named by its title
//!   for every box Claude Code 2.1.282 draws, the footerless ones included,
//!   and its header's origin ([`prompt::parse_header`]: a workflow, a
//!   subagent, a remote agent, a plugin);
//! * [`question`] — the question dialog (AskUserQuestion) read whole: its
//!   tabs, its question, its options and their `(Recommended)` marks, the
//!   free-text and chat rows by position, the multi-select button, the
//!   preview form and the review tab ([`prompt::PromptV2::question_dialog`]);
//! * [`wall::wall`] — the wall a turn ended on, by kind (a usage window, a
//!   model bucket, spend, a full context, an expired login, an API error,
//!   overload), where `worker_phase` has only `limited` for the usage kinds
//!   and reads the rest `idle` — and Claude Code's critical-memory banner
//!   ([`wall::memory_wall`]), which the reader keeps even under a busy
//!   spinner;
//! * [`turn`] — what a finished turn leaves for the next one: Claude Code's
//!   own suggestion, an armed `/goal`, the worker's last words, a stalled
//!   token count;
//! * [`reader::identify`] / [`reader::read`] — the reader for the session's
//!   PROGRAM (Claude Code, Codex, anything else), and one screen read whole;
//!   [`reader::Program::supervisable`] — the programs the supervisor hosts;
//! * [`codex`] — Codex's grammar, measured on codex 0.156.1;
//! * [`panel`] — Claude Code's Settings panel (`/usage`'s dialog): whether
//!   it is what the screen shows, by which renderer, and what a row under
//!   its header says of the Usage tab;
//! * [`reset`] — Claude Code's limit reset: the grant row of the usage-limit
//!   dialog by its exact label, the confirmation it opens read exactly or
//!   refused, the name it draws each limit type by, the two pending lines,
//!   and the line the vendor answers with;
//! * [`anchors::ANCHORS`] — the literal strings other crates guard on.
//!
//! ## Why a crate of its own
//!
//! ONE reader for every face: the server (`aterm-gui`) classifies each agent
//! session's screen and publishes the verdict (`status agent=`, the `EVENT
//! <sid> agent …` push, relayed by `aterm-link` as `phase=`), and `aterm drive
//! phase` must print the same answer, so both link this dependency-free
//! crate. `aterm-agent` re-exports it under its `supervise::phase` paths.
//!
//! Every rule in it was paid for by a misclassification in a real session
//! (the measurements are in the doc comments and the tests); the fixtures
//! under [`prompt::fixtures`] are the screens those sessions showed, each
//! newer one with its program, version and provenance on its first line.

pub mod anchors;
pub mod codex;
pub mod panel;
pub mod phase;
pub mod prompt;
pub mod question;
pub mod reader;
pub mod reset;
pub mod survey;
mod text;
pub mod turn;
pub mod wall;

pub use anchors::{ANCHORS, Anchor, AnchorKind, CODEX_ANCHORS, anchor, anchor_text};
pub use panel::{
    PanelRenderer, SettingsPanel, panel_tail, settings_panel, usage_tab_mark, usage_unsettled,
};
pub use phase::{
    Busy, Phase, Zone, background_only, busy_signal, busy_zone_start, context_left, is_placeholder,
    limit_notice, notice_reset, survey_open, transcript_end, worker_phase,
};
pub use prompt::{
    Cancel, CancelEffect, Header, Opt, Origin, OwnerReview, Prompt, PromptKind, PromptV2,
    ReviewKind, RmBreaker, RmBreakerKind, RmCommand, Role, Select, footer_keys, parse_header,
    parse_prompt, parse_prompt_v2, prompt_box_first_row, prompt_box_span, rm_breaker_of,
};
pub use question::{
    ButtonRow, ChatRow, FreeText, QuestionDialog, QuestionFocus, QuestionForm, QuestionOption,
    QuestionTab, QuestionText, Recommended, ReviewRow,
};
pub use reader::{
    AGENT_RUNTIMES, BLANK_GAP, ClaudeReader, CodexReader, GenericReader, Program, Reading,
    ScreenReader, identify, live_zone_reaches_above, live_zone_start, may_host_agent, program_of,
    read, read_at,
};
pub use reset::{
    ConfirmRead, ResetConfirm, ResetLine, ResetPending, ResetRow, reset_confirm, reset_limit_name,
    reset_line, reset_pending, reset_row, spends_reset,
};
pub use survey::{SurveyBlock, SurveyKind, ask_card, drafts_card, survey_block};
pub use turn::{
    LaunchCard, Progress, continuation_suggestion, goal_active, interrupted, launch_card,
    said_tail, status_row_progress, status_row_stall,
};
pub use wall::{
    ApiCause, Placement, Wall, WallKind, classify_wall, login_restored, memory_banner_head,
    memory_wall, wall,
};
