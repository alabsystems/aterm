// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **aterm-phase** — what an agent worker (Claude Code, agy, Muse Code,
//! Gemini CLI) is doing RIGHT NOW, from one screen read, as pure functions over the
//! screen's rows.
//!
//! * [`phase::worker_phase`] — busy | prompt | limited | idle | question, read
//!   from the LIVE ZONE around the composer (the status row above its top
//!   rule, the hints under it, the footer under its bottom rule), never from
//!   the transcript above it but for the one row Claude Code carries there
//!   (a `Waiting for N … to finish` over the turns it started itself:
//!   [`phase`]'s module docs);
//! * [`phase::context_left`] — the `<n>% until auto-compact` indicator, when
//!   Claude Code shows it above the composer; [`phase::context_left_shown`] — the
//!   same, or where Claude Code 2.1.305 draws it, on the row under the composer;
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
//!   own suggestion, an armed `/goal`, the worker's last words — and the
//!   launch card: what it names ([`turn::launch_card`]) and where it sits
//!   ([`turn::launch_card_place`], for the harness card drawn around it, with
//!   the build it names: [`turn::launch_card_version`], and its words, by
//!   which the card is held through the vendor's entrance:
//!   [`turn::launch_card_held`]);
//! * [`reader::identify`] / [`reader::read`] — the reader for the session's
//!   PROGRAM (Claude Code, Codex, agy, Muse Code, Gemini CLI, anything else), and one
//!   screen read whole; [`reader::Program::supervisable`] — the programs the
//!   supervisor hosts. Codex is named, never read: aterm's Codex screen lane
//!   was retired on 2026-10-16 ([`reader::CodexReader`]);
//! * [`agy`] — agy's (Google's Antigravity CLI), measured on 1.4.0;
//! * [`muse`] — Muse Code's, measured on 1.4.9-R6297.1 and 1.4.10-R6427.1;
//! * [`gemini`] — Gemini CLI's (Google's), measured on 0.46.0;
//! * [`panel`] — Claude Code's Settings panel (`/usage`'s dialog): whether
//!   it is what the screen shows, by which renderer, and what a row under
//!   its header says of the Usage tab;
//! * [`reset`] — Claude Code's limit reset, in 2.1.304's wording: the kinds
//!   of reset, the grant row of the usage-limit dialog the step may take (the
//!   ONE reset on offer, of a kind it takes, or the first kind of the owner's
//!   list on offer, by its exact label), the confirmation it opens
//!   read exactly or refused, the name it draws each limit type by, the two
//!   pending lines, and the line the vendor answers with;
//! * [`credits`] — usage credits carrying the session, and the vendor's own
//!   warning that a window is filling, read for the record;
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

pub mod agy;
pub mod anchors;
pub mod autocompact;
pub mod credits;
pub mod disk_full;
pub mod gemini;
pub mod login;
pub mod muse;
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
mod wrap;

#[cfg(test)]
#[path = "../tests/support/vendor_drift.rs"]
mod vendor_drift;
#[cfg(all(test, unix))]
#[path = "../tests/support/vendor_files.rs"]
mod vendor_files;

pub use anchors::{ANCHORS, anchor, anchor_text};
pub use autocompact::{Offer, OfferFocus};
pub use login::{
    LoginAtWall, LoginMethod, LoginScreen, Picker as LoginPicker, login_at_wall, login_interrupted,
    login_screen,
};
pub use panel::{
    PanelRenderer, SettingsPanel, panel_tail, settings_panel, usage_tab_mark, usage_unsettled,
};
pub use phase::{
    Phase, background_only, busy_signal, busy_zone_start, context_left, limit_notice, notice_reset,
    opened_by_own_wake, shells_running, transcript_end, turn_over_background, worker_phase,
};
pub use prompt::{
    Opt, Origin, PromptKind, PromptV2, Role, footer_keys, parse_prompt, parse_prompt_v2,
    prompt_box_first_row, prompt_box_span,
};
pub use question::{
    FreeText, QuestionDialog, QuestionFocus, QuestionForm, QuestionOption, QuestionText,
    Recommended,
};
pub use reader::{
    AgyReader, BLANK_GAP, ClaudeReader, CodexReader, GeminiReader, GenericReader, MuseReader,
    Program, Reading, ScreenReader, identify, live_zone_reaches_above, live_zone_start,
    may_host_agent, program_of, read, read_at,
};
pub use reset::{
    ConfirmRead, ResetConfirm, ResetKind, ResetLine, ResetPending, ResetRow, reset_confirm,
    reset_limit_name, reset_line, reset_pending, reset_row, reset_title, spends_reset,
    uses_a_reset,
};
pub use survey::{SurveyBlock, SurveyKind, survey_block};
pub use turn::{
    BannerBuild, CardPlace, LaunchCard, LaunchCardRead, answered_prompt, continuation_suggestion,
    goal_active, idle_compacted, interrupted, launch_card, launch_card_held, launch_card_place,
    launch_card_read, launch_card_scan, launch_card_unmoved, launch_card_version,
    launch_card_words, said_tail, said_whole, turn_history_stop, turn_said_before, turn_whole,
};
pub use wall::{
    ApiCause, AutoContinue, Placement, SinceLimit, Wall, WallKind, WordsAbove, auto_continue_end,
    auto_continue_toast, classify_wall, left_wall, limit_words_above, login_expires_in,
    login_restored, memory_banner_head, since_limit, wall,
};
pub use wrap::line_start;
