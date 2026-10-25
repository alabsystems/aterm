// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **aterm-phase**.

pub mod agents_panel;
pub mod agy;
pub mod anchors;
pub mod autocompact;
pub mod codex;
pub mod credits;
pub mod disk_full;
pub mod gemini;
pub mod grok;
pub mod harness_version;
pub mod kimi;
pub mod login;
pub mod muse;
pub mod notice;
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
pub use harness_version::{
    CliHarness, HarnessVersion, cli_harness_of, harness_version_of, harness_version_str_of,
};
pub use login::{
    LoginAtWall, LoginMethod, LoginScreen, Picker as LoginPicker, login_at_wall, login_interrupted,
    login_screen,
};
pub use notice::{NoticePlace, NoticeScan, update_notice_scan, update_notice_unmoved};
pub use panel::{
    PanelRenderer, SettingsPanel, panel_tail, settings_panel, usage_tab_mark, usage_unsettled,
};
pub use phase::{
    ApiRetryRow, Phase, RetryCause, api_retry_row, background_only, busy_signal, busy_zone_start,
    context_left, limit_notice, notice_reset, opened_by_own_wake, shells_running, transcript_end,
    turn_over_background, worker_phase,
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
    AgyReader, BLANK_GAP, ClaudeReader, CodexReader, GeminiReader, GenericReader, GrokReader,
    KimiReader, MuseReader, Program, Reading, ScreenReader, identify, live_zone_reaches_above,
    live_zone_start, may_host_agent, program_of, read, read_at, shell_alone,
};
pub use reset::{
    ConfirmRead, ResetConfirm, ResetKind, ResetLine, ResetPending, ResetRow, reset_confirm,
    reset_limit_name, reset_line, reset_pending, reset_row, reset_title, spends_reset,
    uses_a_reset,
};
pub use survey::{SurveyBlock, SurveyKind, survey_block};
pub use turn::{
    BannerBuild, CardPlace, CardWords, LaunchCard, LaunchCardRead, WAND_FLARE_DARK,
    WAND_FLARE_LIGHT, WAND_FLARE_TOLERANCE, answered_prompt, continuation_suggestion, goal_active,
    idle_compacted, interrupted, launch_card, launch_card_held, launch_card_place,
    launch_card_read, launch_card_scan, launch_card_scrolled, launch_card_scrolled_unmoved,
    launch_card_unmoved, launch_card_version, launch_card_words, said_tail, said_whole,
    turn_history_stop, turn_said_before, turn_whole, wand_flare,
};
pub use wall::{
    ApiCause, AutoContinue, Placement, SinceLimit, Wall, WallKind, WordsAbove, auto_continue_end,
    auto_continue_toast, classify_wall, left_wall, limit_words_above, login_expires_in,
    login_restored, memory_banner_head, since_limit, wall,
};
pub use wrap::line_start;
