// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Integration test suite for `aterm-core`.
//!
//! Run a single module with `targo --unverified test -p aterm-core --test core_suite -- <file>::`.

mod alt_archive_claude_like;
mod chunked_process_equivalence;
mod codex_insert_history_scrollback;
mod color_scheme_follows_background;
mod conformance_altscreen;
mod conformance_budgeted_search;
mod conformance_hyperlink_scheme_cap;
mod conformance_observe;
mod conformance_read_image_seq;
mod conformance_recording;
mod conformance_repaint_nudge;
mod control_string_cancel;
mod empty_param_default;
mod fuzz_alt_screen;
mod fuzz_process_never_panics;
mod grid_column_agreement;
mod half_drawn_frame;
mod handoff_alt_screen_checkpoint;
mod handoff_colour_and_shell_carry;
mod handoff_parser_carry;
mod handoff_saved_cursor_after_shrink;
mod history_shed_alt_screen;
mod image_horizon_offload;
mod image_survives_column_reflow;
mod image_survives_scrollback;
mod kitty_chunked_load;
mod kitty_graphics_delete;
mod osc_param_tail;
mod palette_refusal_told_once;
mod primary_flap_with_output;
mod reload_keeps_program_colours;
mod replay_corpus_probe;
mod replay_offset_determinism;
mod resize_preserves_non_bmp;
mod resize_quiet_flap_identity;
mod resize_rewrap_fidelity;
mod ris_preserves_host_policy;
mod saved_cursor_follows_resize;
mod scroll_pin_across_alt_screen;
mod scrollback_content_integrity;
mod scrollback_preserves_extras;
mod scrollback_ring_materialize_parity;
mod scrolled_back_frame_materialize_count;
mod selection_custody_conformance;
mod selection_paint_vs_copy;
mod selection_scrollback_highlight;
mod sgr_rendition_parity;
mod shell_mark_rows_across_resize;
mod tab_stops_across_grid_seams;
mod unicode_content_generation;

// OSC 1337 `File=` fixtures shared by the `image_*` modules.
#[path = "support/osc1337.rs"]
mod osc1337;
