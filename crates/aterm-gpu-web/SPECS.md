<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-gpu-web: spec recovery from its 93 unit tests

> Status 2026-10-16, after the merge with the tests-and-gates audit: the rows naming 8 tests it
> deleted are gone, and the spec rows no longer cite those tests. Counts elsewhere in this file
> were taken before.

> 2026-10-20, predictive editing increment 3: the shared `predict_api.rs` gains
> `a_shift_s_moved_text_comes_out_as_triples` (§2), and `predict-facade-gates` says a shift's moved glyph
> is a triple like a typed one.

> 2026-10-20, predictive editing increment 4: every `KeySample` the shared `predict_api.rs` builds hands
> `input_start: None` (`predict-facade-gates`): the web reads no signed prompt marks, so Home, End, Ctrl-A, Ctrl-E
> and Delete stay unmodelled input. No export or test changes.
>
> 2026-10-20, after review of increment 4: every `KeySample` also hands the engine's deferred-wrap flag
> (`wrap_pending`) and no row under the cursor (`below: None`). No export or test changes.

> Status 2026-10-16: this is the crate's migration worklist. Each of the 46 specs below is
> proved through Trust/Clean about the compiled Rust function, and then the tests it
> subsumes are deleted. The 1 test in §4 samples no spec and is to be deleted now.

Source: `crates/aterm-gpu-web` (read only). Every `#[test]` was read: `src/lib.rs` 38,
`src/messages_api.rs` 9, `src/notifications_api.rs` 5, `src/predict_api.rs` 12,
`src/scroll_input_api.rs` 14, `src/scrollback_tiers_api.rs` 2, shared `facade_api.rs` 1, shared `theme_api.rs` 11
and `tests/cpu_gpu_parity.rs` 1, for 93 in all. The production code they reach in `src/lib.rs`, `src/effects_api.rs` and the
API modules was read too. Nothing was compiled or run; the claims below come from reading the
code. Gap numbers refer to the gap table in
`AUDIT-trust-for-test-migration-2026-10-15.md` (historical internal audit) §7.

Result: **46 specs**, covering **92 tests**. **1 test is not a spec sample** (§4).

This crate is a thin wasm facade over `aterm-core`, `aterm-render`, `aterm-gpu`,
`aterm-effects`, `aterm-messages`, `aterm-predict` and `aterm-types`. Most of its tests are
integration samples of those crates. A spec whose essential clause is another crate's
property is marked *(owned by <crate>)*: the gpu-web half is only the delegation clause, which
is cheap, and the substantive clause belongs in that crate's `SPECS.md`. Such a spec is proved
here once the callee's theorem can be cited across crates (gap 12).

## 0. Notation used in the statements

- Paths in `functions` are relative to `crates/aterm-gpu-web/src`. A bare name is a method of
  `AtermGpuTerminal` unless it carries a module, type or crate prefix.
- `T` = `AtermGpuTerminal`. `Reach` = the `T` values reachable from `T::new(..) = Ok` or
  `T::new_from_system(..) = Some` by any sequence of public calls, plus the crate-internal
  `*_at` clocked twins. Test helpers that assign `t.term` directly (e.g. `install_tiered_history`)
  are outside `Reach`.
- "Engine" = `aterm_core::terminal::Terminal`. "Twin" = `aterm-wasm`, the CPU web module, whose
  messages/predict/scroll/notifications glue is a near-copy of this crate's.
- `hvg(t)` = `t.host_visual_gen: u64`, bumped only by `note_host_visual_change`
  (`wrapping_add(1)`). "bump" below means exactly that call.
- `Key(t)` = `FrameGateKey { damage_epoch: t.term.damage_epoch(), host_visual_gen: hvg(t),
  effects_active: t.effects.is_active() }`.
- `Open(t)` = `Key(t).effects_active ∨ t.pending_reflow.is_some() ∨ t.last_present_frac ≠ 0 ∨
  t.scroll_input.frac_px(cell_h) ≠ 0 ∨ t.last_frame_key ≠ Some(Key(t))`. `Gated(t)` = `¬Open(t)`.
- `unpack(x: u32)` = `Rgb { r: (x>>16)&0xff, g: (x>>8)&0xff, b: x&0xff }`;
  `pack(Rgb{r,g,b})` = `(r<<16)|(g<<8)|b`. For `x ≤ 0x00FF_FFFF`, `pack(unpack(x)) = x`.
- `NoEcho(t)` = `t.term.is_alternate_screen() ∨ t.term.kitty_suppresses_predictive_echo()`.
- `frac(s)` = `ScrollInputState.frac_rows: f64`. IEEE-754 binary64 throughout; `trunc`/`round`
  are `f64::trunc`/`f64::round`; `as i32` is Rust's saturating float-to-int cast.
- `bp` = `term.modes().bracketed_paste()`.
- Floats, `&str` formatting and `Vec`/`String` are refused by E6 today (gaps 3 and 1). Every
  spec is also blocked by gap 0 (the installed seal); it is not repeated in the column.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes` (primary tests; secondary in §2),
`difficulty` (Rust features the proof must handle), `blocking_gaps` (audit §7 numbers).

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
scroll-accumulator	contract	scroll_input_api::ScrollInputState::add_rows	∀ s with frac(s) ∈ (-1,1), ∀ d: f64. If ¬d.is_finite(): result 0 and frac(s') = frac(s). Else let t = frac(s)+d, w = trunc(t): frac(s') = t − w (exact in IEEE) and result = clamp(w, i32::MIN as f64, i32::MAX as f64) as i32. Invariant: frac(s') ∈ (-1,1); default() has frac 0, so frac ∈ (-1,1) on every reachable state. Corollaries: |t| ≥ 2^52 ⇒ frac(s') = 0; truncation is toward zero; sign(result) ∈ {0, sign(t)}; never panics.	scroll_input_api::{fractional_deltas_accumulate_without_flipping, row_flips_at_exactly_plus_and_minus_one, multi_row_deltas_flip_whole_rows_and_bank_the_remainder, direction_reversal_can_cancel_without_a_flip, non_finite_deltas_are_ignored, huge_deltas_saturate_without_panicking}	EASY-MEDIUM: &mut self on a one-field struct; f64 add/sub, is_finite, trunc, clamp, saturating as i32. Needs an IEEE f64 model in Clean (exactness of t − trunc(t)).	2,3,14
scroll-clamp-and-reset	contract	scroll_input_api::ScrollInputState::{clamp_at_history_edges, reset}	∀ s, offset, hist: frac(clamp_at_history_edges(s,offset,hist)) = 0 if (offset = 0 ∧ frac(s) < 0) ∨ (offset ≥ hist ∧ frac(s) > 0), else frac(s). reset: frac(s') = 0. Hence hist = 0 ⇒ frac(s') = 0 ∀ s. Both preserve frac ∈ (-1,1).	scroll_input_api::{reset_drops_the_residual, history_edges_clamp_the_empty_side_only}	EASY: &mut self, f64 and usize comparisons	2,3
scroll-frac-px-and-stamp	contract	scroll_input_api::ScrollInputState::{frac_px, stamp}	∀ s with frac(s) ∈ (-1,1), ∀ h: usize. frac_px(s,0) = 0. For h ≥ 1, with m = i32::try_from(h).unwrap_or(i32::MAX).saturating_sub(1): frac_px(s,h) = clamp(round(−frac(s)·(h as f64)) as i32, −m, m). Hence |frac_px| < h, frac_px(·,1) = 0, sign(frac_px) ∈ {0, −sign(frac)}, frac(s) = 0 ⇒ 0. stamp(s,input,rows,h) sets input.scroll_frac_px = frac_px(s,h), grid_top_row = 0, grid_bot_row = rows, and leaves every other RenderInput field unchanged.	scroll_input_api::{frac_px_negates_rounds_and_clamps_into_the_translate_domain, stamp_fills_the_full_grid_band_every_frame}	MEDIUM: f64 mul/neg/round, usize→f64, i32::try_from, saturating_sub, clamp; &mut RenderInput (large struct; frame on untouched fields)	1,2,3
scroll-export-composition	refinement	scroll_px, scroll_lines_frac, scroll_rows_input, scroll_lines, scroll_to_bottom, scroll_to_top, scroll_frac_rows, scroll_frac_px, build_frame	∀ t ∈ Reach, d. scroll_lines_frac(d): w = add_rows(d); if w ≠ 0 then term.scroll_display(w); then clamp_at_history_edges(display_offset, scrollback_lines) of the post-scroll engine. scroll_px(p) = no-op if cell_height() = 0, else scroll_lines_frac(p / cell_height() as f64). scroll_lines, scroll_to_bottom, scroll_to_top reset the residual, then delegate. scroll_frac_px() = frac_px(cell_height()). build_frame stamps the residual with rows = self.rows (compose_band_into_frame then shifts the band). With the engine contract that scroll_display clamps to [0, scrollback_lines] (owned by aterm-core): the residual is 0 whenever the viewport is parked at an empty side, and two half-cell scroll_px calls move display_offset by exactly 1 when history exists.	scroll_input_api::{scroll_px_accumulates_and_flips_through_the_export, whole_row_navigation_resets_the_residual, history_ends_clamp_the_residual_through_the_export, render_snapshot_carries_the_banked_residual}	MEDIUM: &mut self over a large struct; cross-crate ensures of Terminal::scroll_display/grid; f64 division	1,2,3,4,12
visual-mutators-bump	contract	every unconditional visual setter of lib.rs and effects_api.rs (fonts, line height, ligatures, features, palette and default colours, scrollback limit, theme, selection colours, contrast, opacity, chrome, px, spill veils, cursor style, colour scheme, selection start/word/line/finish/clear, resize, effects focus/visibility/keystroke/typed char, cursor glow/trail/pack, matrix rain, output streak, sparkle); set_cursor_blink_phase, set_cursor_hollow, selection_extend, note_console_input, advance_effects, set_output_streak_theme, note_host_visual_change	(a) ∀ t, args, ∀ unconditional mutator f: hvg(f(t,args)) = hvg(t).wrapping_add(1) on every return path, including Err from font setters (the bump precedes their validation); resize/set_px/set_chrome instead preflight geometry first and leave hvg unchanged on admission Err. (b) set_cursor_blink_phase(v) and set_cursor_hollow(v) bump iff the shadow ≠ Some(v), then set the shadow to Some(v). (c) selection_extend bumps iff term.text_selection() changed. (d) note_console_input(k) bumps iff PetInputKind::parse(k).is_some(). (e) advance_effects and set_output_streak_theme never change hvg. (f) No other function writes host_visual_gen. Corollary with frame-gate: after any bump, Open(t).	lib::{every_visual_mutator_bumps_the_host_visual_generation, effects_config_mutators_bump_the_host_visual_generation}	MEDIUM: &mut self; u64 wrapping_add; ? early return after the bump; frame lemma that callees on disjoint fields (cpu, term, effects, gpu) cannot touch host_visual_gen; TextSelection clone + PartialEq for (c). State per fn, or as one crate-level "only these write hvg" frame lemma.	2,4
frame-gate	contract	open_frame, needs_frame, record_presented_frame, render_headless, build_frame, last_render_skipped	∀ t ∈ Reach. Let t0 = t after open_frame's pumps (pump_reflow_on_render_tick, drain_compress_backlog_on_render). open_frame = None ⇔ Gated(t0), with last_render_gated' = Gated(t0); else Some(Key(t0)). needs_frame(t) = Open(t). After geometry admission, render_headless returns Ok(open_frame = None); when not gated it runs build_frame, then last_frame_key := Some(key), last_present_frac := frame_scratch.scroll_frac_px. Fresh t has last_frame_key = None, so the first render_headless returns Ok(false). Settle lemma (needs engine: take_damage keeps damage_epoch; build_frame changes no hvg): with no public mutator between two render_headless calls, ¬effects_active, pending_reflow = None and frac_px = 0 = scroll_frac_px, the second returns Ok(true). Reopen: an hvg bump, a damage_epoch advance, active effects, a pending reflow or a nonzero residual makes the next call return Ok(false).	lib::frame_gate_skips_settled_ticks_and_reopens_on_every_change_class	MEDIUM: &mut self, Option<struct> derive-PartialEq equality, bool logic; cross-crate Terminal::damage_epoch, EffectsPipeline::is_active, cell_frame_damage_scoped_into	1,2,4,12
webgl-present-result	contract	webgl_present_result	∀ r: Result<(), SurfacePresentFailure>. webgl_present_result(r).is_ok() ⇔ r.is_ok(); Err(f) ↦ Err(format!("WebGL canvas present failed: {f:?}")). A dropped present is never Ok.	lib::webgl_present_result_propagates_every_surface_failure	EASY for the is_ok clause (enum carrier only); the message clause needs a String/Debug model; map_err closure	1,3,13
theme-reset-baseline	contract	apply_terminal_theme_colors, new, new_from_system, set_theme, cursor_color; engine OSC 10/11/12/17/19 and 110/111/112/117/119 (owned by aterm-core)	∀ term, fg, bg, c, s: apply_terminal_theme_colors moves the engine baseline (Terminal::rebase_colors, so a program's OSC 10/11/12/17 sets survive; the palette and selection fg keep their baseline) to fg = unpack(fg), bg = unpack(bg), cursor = Some(unpack(c)), selection bg = Some(unpack(s)). new/new_from_system/set_theme call it with the theme in force, and theme_cursor = c & 0xFFFFFF. cursor_color() = term.cursor_color().map(pack). Engine: after any OSC 10/11/12/17/19 overrides, OSC 110/111/112/117/119 restore exactly those defaults; OSC 119 restores selection fg to the configured default (None here). Hence ∀ t ∈ Reach with no live OSC 12 override, cursor_color() = Some(theme_cursor); after OSC 12;#rrggbb it is Some(0x00rrggbb).	lib::host_theme_colors_are_the_dynamic_color_reset_baseline, notifications_api::cursor_color_follows_osc12_and_resets_on_112, theme_api::set_theme_keeps_program_colours	MEDIUM: u32 shifts/masks, u8 casts, closure; &mut Terminal calls; OSC parse/reset is aterm-core's	2,3,4,12
web-themes	contract	shared theme_api.rs (aterm-wasm): WebThemeState::{resolve,register,unregister,catalog_json,state_json}, install_draw, follow_appearance, set_theme_by_name, register_theme, unregister_theme, theme_catalog, theme_state, effective_minimum_contrast; AtermGpuTerminal::{render_face,install_renderer_theme,install_renderer_selection_fg}	The same statements as aterm-wasm's web-theme-resolve, web-theme-register, web-theme-install, web-theme-appearance-follow and web-contrast-floor, on this crate's terminal: install_renderer_theme sets self.theme, both faces' theme and invalidates the GPU present; effective_minimum_contrast reads the CPU face, which the GPU face mirrors (init rebuilds it from self.theme and the CPU face's settings).	theme_api::resolve_is_atomic_and_loud, theme_api::register_bounds_and_shadowing, theme_api::catalog_json_lists_builtins_then_registered, theme_api::unregister_refuses_the_drawn_theme, theme_api::set_theme_by_name_matches_the_shared_projection_for_every_builtin, theme_api::split_follows_set_color_scheme, theme_api::named_install_keeps_program_colours, theme_api::register_then_select_follows_a_re_registration, theme_api::theme_installs_reopen_the_frame_gate, theme_api::web_translucency_raises_the_effective_floor	MEDIUM: shared body; composition with aterm-core rebase_colors	1,3,4,12
frame-scratch-live-colors	contract	refill_frame_scratch; Terminal::cell_frame_damage_scoped_into (owned by aterm-core); Renderer::render_input (owned by aterm-render)	∀ t ∈ Reach, after refill_frame_scratch: frame_scratch.default_bg = pack(effective blank bg), i.e. the live default bg, or the live default fg under DECSCNM (?5h); every sparse row-tail and blank cell has that bg. cursor_color = pack(live cursor colour), or pack(live default fg) when OSC 21 cursor= made the cursor dynamic (term.cursor_color() = None). selection_bg/selection_fg = the live selection colours, falling back to the configured defaults. Renderer: render_input paints a steady-block cursor cell (DECSCUSR 2) as exactly cw·ch pixels of frame_scratch.cursor_color.	lib::frame_scratch_tracks_live_sparse_blank_and_cursor_colors	HARD: Vec<Vec<RenderCell>>, cross-crate engine and renderer bodies (pixel loop)	1,2,4,7,12
link-at-cells	contract	link_at, classify, osc8_span; Terminal::{smart_word_text_at,painted_cell_logical} (owned by aterm-core)	classify(s) (total over &str): 1 if ascii_lowercase(s) starts with http://, https://, ftp://, file:// or www.; else 2 if s starts with '/', './', '../', '~/', or (s contains '/' ∧ ¬contains "://"); else 3. link_at(r,c), c the PAINTED cell, first takes c := painted_cell_logical(r,c).0 (a DEC double-width line halves it, a BiDi row maps it through the frame's permutation; the facade is aterm-wasm's, whose a_link_is_named_by_the_cell_it_is_painted_in drives it), then: if display_offset = 0 ∧ hyperlink_at(r,c) = Some(u) then Some{url u, span osc8_span(r,c), kind 0}, where osc8_span is the maximal contiguous [s,e) ∋ c of cells sharing the id (or the URL when either id is absent), e ≤ cols. Otherwise with (m,sc,ec) = smart_word_text_at(r,c): None if that is None or classify(m) = 3, else Some{url m, start sc, end ec, kind classify(m)}. Engine: sc ≤ c < ec and m = the graphemes of display cells [sc,ec) of row r, regardless of wide or out-of-order-drawn cells earlier in the row.	lib::a_link_after_wide_characters_is_read_from_its_cells	MEDIUM for classify (&str lowercase/starts_with/contains); osc8_span while loops with a closure; Option; cross-crate smart-selection rules	1,3,4,7,12
gpu-face-retains-host-settings	contract	gpu_face	∀ t ∈ Reach with Renderer::from_bytes(font_bytes) = Ok: gpu_face(t) = Ok(r) with r.px, cell_size, pad, head, line_height, minimum_contrast, background_opacity, cursor_opacity, cursor_blink_phase, cursor_style_override, selection_fg, selection_inactive, selection_inactive_bg and text_shaping equal to t.cpu's, including after a rejected line-height request; and the same fallback chain (set, then extras in order), emoji, bold and symbol faces as t.cpu. Err only when from_bytes or re-applying a retained face fails.	lib::the_gpu_face_is_the_face_the_host_set	MEDIUM: &self, Result ?, ordered Vec iteration, Arc clone; Renderer setter/getter round trips (aterm-render)	1,3,4,7,12
console-input-note	contract	effects_api::{note_console_input, note_typed_char, note_keystroke}, format_paste; EffectsPipeline::{note_console_input, note_committed_char, cursor_pet_console_input_seq} (owned by aterm-effects)	note_console_input(k) = match PetInputKind::parse(k) { None ⇒ false, t unchanged; Some(kind) ⇒ bump; effects.note_console_input(kind); true }. format_paste takes &self, so it changes no state (type-guaranteed). Effects: cursor_pet_console_input_seq increases by exactly 1 per note_console_input and per note_committed_char of a committed scalar (including a zero-advance combining mark, whose return is false), and never on note_keystroke, process or format_paste. cursor_pet_console_input_kind = the last noted kind.	lib::console_input_api_counts_commits_without_replaying_their_echoes	MEDIUM: &str parse to enum, &mut self; the counting clause is a cross-crate state machine	1,2,4,12
format-paste-sanitize-then-wrap	equivalence	format_paste, bracketed_paste_mode; Terminal::format_paste (owned by aterm-core)	∀ t, s: t.format_paste(s) = t.term.format_paste(s); bracketed_paste_mode() = bp. Engine: format_paste(s) = (bp ? "\x1b[200~" : "") ++ fold(strip(s)) ++ (bp ? "\x1b[201~" : ""), where strip removes ESC, C1 U+0080..U+009F and the C0 signal bytes, and fold maps CRLF/LF to CR. Hence when bp the output contains "\x1b[201~" exactly once, as its suffix: strip(s) has no ESC, so no planted terminator survives.	lib::paste_export_wraps_only_sanitized_text_so_a_planted_terminator_stays_inert	MEDIUM: &str→Vec<u8>, char filtering, substring count; quantified over s	1,4,5,7,12
search-meta-agrees	equivalence	search, search_meta	∀ t, q, cs, re with content unchanged between the calls: |search(q,cs,re)| ≡ 0 mod 3 and search_meta(q,cs,re).match_count = min(|search|/3, u32::MAX). q = "" or search_results_opts = Err (invalid regex) ⇒ search = [] and search_meta = {incomplete: false, match_count: 0}; otherwise incomplete = results.incomplete. Needs determinism of indexed_search on unchanged content (owned by aterm-core).	lib::search_meta_agrees_with_search_and_is_silent_on_empty_or_bad_input	MEDIUM: Result let-else, Vec push loop, u32::try_from; cross-crate search	1,2,4,7,12
budgeted-search-refines-one-shot	refinement	search_budgeted, search_budgeted_cancel; Terminal::search_budgeted (owned by aterm-core)	Facade: q = "" ⇒ cancel the in-flight search and return Empty = {matches [], complete, cursor None, search_id None, reset true, counters 0}; an engine Err ⇒ Empty; else each field is the engine step's, with matches flattened to [line, start_col, len] triplets. Engine: ∀ unchanged content, ∀ budget ≥ 1, the chain c0 = None, c_{i+1} = step_i.cursor until complete is finite; the concatenated matches after the last reset = search(q); only the first step has reset = true; search_id is constant along the chain; a complete step has cursor None. A cursor from a different pattern, or a stale one, restarts: reset true, fresh search_id, rows_fed ≤ budget.	lib::search_budgeted_matches_one_shot_and_restarts_when_superseded	HARD: Vec, Option<u64>, struct build; engine loop termination (variant on rows remaining)	1,2,4,5,7,12
dims-mirror-grid	invariant	new, new_from_system, resize, set_px, set_chrome, render, render_offscreen, render_headless; shared frame_geometry_api	Successful construction/resize stores the inputs clamped by core::grid::{MAX_GRID_ROWS,MAX_GRID_COLS}, both at least one and equal to the engine grid. Before grid allocation/mutation, the physical frame including committed message rows must pass Renderer::checked_frame_size (256 MiB per framebuffer); constructor returns Err, native constructor None, or resize Err otherwise. resize/set_px/set_chrome refuse inadmissible proposed geometry before changing grid, metrics, reflow, prediction, frame gate or swapchain. Render/init preflight before building frames or allocating GPU surfaces; offscreen readback also reserves host RGBA fallibly. Font/line-height setters can independently produce geometry a later render refuses until corrected. Browser Result errors throw strings; successful void operations still yield undefined.	lib::oversized_frames_are_refused_before_state_changes	EASY-MEDIUM: struct invariant over every public fn that assigns term/rows/cols; u16 clamp in the engine	2,4,12
scrollback-limit-one-total	contract	set_scrollback_limit; Terminal::set_scrollback_line_limit and indexed search (owned by aterm-core)	Facade: set_scrollback_limit(l) = bump; term.set_scrollback_line_limit(l = 0 ? None : Some(l)). Engine: ∀ l > 0 and every later output stream, scrollback_lines ≤ l, one total across hot ring, staged lines and tiered store. The retained history is the min(l, produced) most recent lines, so scroll_to_top gives display_offset = min(l, produced). A shrink evicts the oldest at once, and the next search returns only absolute rows ≥ the oldest retained row, with no intervening write needed.	lib::{retention_shrink_search_returns_no_evicted_rows, set_scrollback_limit_governs_ring_retention}	HARD: cross-crate ring/tiered store/search-index invalidation; Vec; loops	1,2,4,7,12
encode-key-refines-mode	equivalence	encode_key, encode_key_with_mode, keyboard_mode_bits	∀ t, k, m, e, b: encode_key(k,m,e,b) = encode_dom_key(k,m,e,b, term.keyboard_mode()); encode_key_with_mode(k,m,e,b,bits) = encode_dom_key(k,m,e,b, KeyboardMode::from_bits_truncate(bits as u16)); keyboard_mode_bits() = term.keyboard_mode().bits() as u32. With from_bits_truncate(mode.bits()) = mode: encode_key(k,m,e,b) = encode_key_with_mode(k,m,e,b, t.keyboard_mode_bits()).	lib::encode_key_with_mode_matches_the_instance_encoder	EASY-MEDIUM: bitflags u16 round trip; &str/Option<Vec<u8>> passthrough	1,4,12
keyboard-mode-tracking	contract	Terminal keyboard_mode, DECCKM, kitty push, win32-input-mode handlers (owned by aterm-core); aterm_types::keyboard::encode_dom_key (owned by aterm-types); set_win32_input_enabled	Engine: DECSET ?1 sets and DECRST ?1 clears APP_CURSOR; CSI >1u pushes kitty DISAMBIGUATE; CSI ?9001h sets WIN32_INPUT iff set_win32_input_enabled(true) was called first (fail-closed). encode_dom_key: ArrowUp ↦ CSI A (legacy), SS3 A (APP_CURSOR), CSI 1;(1+mods)A with modifiers; a Release without kitty event types ↦ None; a bare modifier ↦ None; Shift+Enter ↦ CSI 13;2u under DISAMBIGUATE and the win32 key-record down/up pair under WIN32_INPUT.	lib::a_conpty_host_opts_in_to_win32_input_records	HARD: cross-crate parser/mode state and a large match over &str keys	1,3,4,12
scoped-refill-equals-full	equivalence	build_frame, refill_frame_scratch; Terminal::cell_frame_damage_scoped_into / cell_frame_into (owned by aterm-core)	∀ t ∈ Reach, ∀ mutation streams (output, effects ticks, scroll, resize), damage-reading effects on or off: after build_frame, the engine-owned channels of frame_scratch equal cell_frame_into(clone(frame_scratch), rows, cols) (full extraction), and the effects pipeline does not re-take the damage session the refill consumed. Locality: if the only change since the last build is one printable byte echoed on one row (no scroll, wrap, colour or resize), the refill is Scoped{rows_refilled: 1}.	lib::{build_frame_echo_refills_one_row_with_sparkle_and_rain_on, build_frame_scoped_refill_matches_a_full_extraction_over_a_mutation_stream}	HARD: RenderInput (Vec of rows) equality, damage bitsets, &mut self; observes the refill return value (via the #[cfg(test)] last_refill field)	1,2,4,7,12
spill-band-composes-to-frame	contract	SpillBand::{update, rgba, rev, rects} (owned by aterm-render); spill_len, spill_rev, spill_rect_count, spill_ptr, set_chrome	∀ cpu face with pad p, head h, frame (w,H), gh = H − 2p − h: after update, len(rgba) = 4·(w(p+h) + wp + 2p·gh); pad = head = 0 ⇒ len 0. ∀ strip pixel (x,y) with straight-alpha px: (a = 0 ? bg : over_rgb(bg, rgb(px), a)) = render_input_cached(frame)[y·w+x] & 0xFFFFFF. An update with unchanged inputs keeps rev and leaves rects empty; unchanged geometry reuses the buffer (spill_ptr stable). Effects (owned by aterm-effects): after an unwitnessed program cursor leap in serious mode, every band pixel has alpha 0 on every later frame, and cursor_glow_add drains to empty within a bound that is a function of the glow config.	lib::spill_exports_surface_band_content_on_the_cpu_face	HARD: pixel loops over Vec<u8>, integer alpha compositing, buffer-capacity reasoning; effects state machine with injected dt	1,4,7,12
chrome-frame-size	contract	set_chrome, chrome_pad, chrome_head; Renderer::frame_size (owned by aterm-render)	∀ t, p, h: after set_chrome(p,h) = Ok, cpu.pad() = p and cpu.head() = h (so chrome_pad() = p, chrome_head() = h), effects chrome = (p,h), and hvg is bumped. Renderer: frame_size(r,c) = (c·cw + 2p, r·ch + 2p + h). Hence 0/0 restores (c·cw, r·ch) exactly.	lib::set_chrome_sticks_pre_init_and_zero_restores	EASY-MEDIUM: &mut self, u16→usize casts, usize arithmetic (overflow); cross-crate getters	2,3,4,12
swapchain-sized-to-surface-rows	invariant	size_swapchain, resize, set_chrome, note_band_change, init_with_target	init, successful resize/set_chrome and admissible committed band-row changes configure the surface to Renderer::checked_frame_size(rows + band.committed(), cols). An inadmissible band growth leaves the previous surface; the next render returns Err before frame work. The host follows metric changes from zoom, line-height and primary-font updates with resize, as those APIs require.	-	HARD: wasm32-only WebGL2 canvas state (aterm-gpu's GL arm); needs a GpuSurface size model	2,4,8,12
effects-off-identity	contract	EffectsPipeline::{apply, is_active} (owned by aterm-effects); effects_api setters	∀ engine states and dt streams: with every effect disabled (default, or each enable toggled back off), apply leaves cursor_glow_add, cursor_trail, word_decorations, ink, cat_quads and nova_add empty and cat_atlas None; is_effects_active() = false after the next apply; the RenderInput equals that of an instance that never enabled anything, fed the same bytes and dt stream.	-	HARD: cross-crate pipeline over Vecs and Arc atlases; struct equality	1,2,4,12
effects-deterministic	equivalence	EffectsPipeline::{advance, apply} (owned by aterm-effects); advance_effects	∀ config C, byte stream B, dt stream D: two instances built alike and fed (C,B,D) give value-equal RenderInputs at every step (atlases compared by value). The engines read no wall clock or RNG seed outside the dt stream.	lib::effects_snapshots_are_deterministic_and_restore_on_off	HARD: cross-crate; f64 time accumulation; Arc identity vs value	1,2,4,12
mode-getters-delegate	contract	is_alternate_scroll, bracketed_paste_mode, is_app_cursor_mode, mouse_wants_motion, mouse_wants_any_motion, is_focus_event_mode, is_color_scheme_updates_mode; Terminal DECSET/DECRST (owned by aterm-core)	Each getter returns the engine's mode bit, e.g. is_alternate_scroll() = term.modes().alternate_scroll(). Engine: ∀ mode n with a getter, CSI ?n h sets the bit and CSI ?n l clears it; 1007 defaults off.	-	TRIVIAL facade (&self delegation); engine parser clause	4,12
word-separators	contract	set_word_separators, selection_word; SmartSelection::set_word_separators / Terminal::smart_word_at (owned by aterm-core)	After set_word_separators(Some(seps)), the word at (r,c) with no smart rule matching is the maximal run of non-separator chars containing c; after None it is the maximal run of the default word class (alphanumeric or '_'). set_word_separators(x); set_word_separators(None) restores selection_word exactly. selection_word returns that span's selection text; a click on whitespace gives the clicked cell.	lib::set_word_separators_reshapes_double_click_words	HARD: Option<String>, char classification, cross-crate selection model	1,4,7,12
row-queries-display-aware	contract	row_text, row_len, row_is_wrapped; Grid::{row_len, row_is_wrapped, display_row_text} (owned by aterm-core)	∀ t, r, with display_offset o: row_text(r), row_len(r), row_is_wrapped(r) describe display row r, the history line at absolute display origin + r, in whichever tier holds it (ring or tiered store, after any reflow). row_len = last non-blank cell + 1; row_is_wrapped iff the row continues the previous one. All three are None iff r ≥ rows.	lib::scrolled_wrapped_row_len_and_wrap_flag_are_tier_aware	HARD: cross-crate tiered scrollback materialization	1,4,7,12
display-row-cache-transparent	refinement	with_display_row_cell, cell_text, cell_is_wide	∀ t ∈ Reach, r, c, any history of earlier cache fills: cell_text(r,c) = display_row_grapheme_cells(r).and_then(|v| v.get(c)).map(|x| x.0.clone()).unwrap_or_default(), and cell_is_wide(r,c) = the same lookup's .1. The cache is a pure memo. Key soundness (needs engine): equal (is_alternate_screen, active grid content_gen, display_offset, row) imply equal display_row_grapheme_cells(row); the alt bit is needed because main and alt grids have independent content_gen counters.	lib::display_row_cache_distinguishes_main_and_alt_screen	MEDIUM: RefCell borrow_mut, Vec<(String,bool)>, Option tuple key equality, FnOnce closure	1,2,4,12,13
reflow-defer-policy	contract	resize; Terminal::{resize_offloading_scrollback, finish_resize_offload} (owned by aterm-core)	resize(r,c): bump; p = term.resize_offloading_scrollback(r,c). p = Some with p.line_count() ≤ INLINE_REFLOW_MAX_LINES (20000) ⇒ pending_reflow := finish_resize_offload(p.reflow()), grace := 120 when that is Some. p = Some with more lines ⇒ pending_reflow := Some(p), grace := 120. p = None ⇒ pending_reflow unchanged (a stashed job survives a superseding resize). Then rows/cols re-sync to the grid, predict.reset(), size_swapchain(). Engine: p = None with no tiered store or with the store already detached, so a ring-only engine never defers; an inline rewrap keeps scrollback_lines ≥ before.	lib::{resize_without_tiered_store_never_defers, small_tiered_history_rewraps_inline_on_resize}	MEDIUM: Option<PendingScrollbackReflow> moves, &mut self, usize compare; cross-crate	1,2,4,12
reflow-pump-converges	contract	pump_reflow, pump_reflow_budget, reflow_pending; PendingScrollbackReflow::reflow_step (owned by aterm-core)	pump_reflow(): pending = None ⇒ false. Else grace := 0 and step with budget b: InProgress(j) ⇒ pending := Some(j), true; Done(x) ⇒ pending := finish_resize_offload(x), return whether it is Some. Invariant: the return value = reflow_pending() after the call. pump_reflow_budget(0) sets b = 2000, n > 0 sets b = n. Engine: each step with b ≥ 1 strictly decreases the remaining input lines (variant), so every pump schedule, including interleaved resizes, reaches false in finitely many calls; the re-attached history equals the one-shot rewrap and has scrollback_lines ≥ the count before the detach; a superseding width change keeps progress; dropping T with a half-stepped job is panic-free.	lib::{large_tiered_history_defers_rewrap_and_stepped_pumps_reattach_it, resize_mid_stepping_supersedes_without_losing_history}	HARD: enum match on a moved job, Option take; engine loop variant and history equivalence	1,2,4,7,12
reflow-safety-nets	contract	pump_reflow_on_render_tick, pump_reflow_on_output, process, open_frame	pump_reflow_on_render_tick: pending = None ⇒ no-op; grace > 0 ⇒ grace −= 1 with no pump; else pump_reflow(). process(b) = term.process(b), then pump_reflow() iff pending.is_some() ∧ lazy_backlog_len() > REFLOW_BACKLOG_MAX_LINES. Liveness (with reflow-pump-converges): a host that only renders reaches ¬reflow_pending within grace + 1 + N(steps) ticks, and the first grace + 1 ticks after a stash leave it pending; a host that only streams output past the backlog cap converges in finitely many process calls.	lib::{never_pumped_host_completes_via_the_render_grace_net, never_pumped_host_streaming_output_converges_via_the_backlog_net}	MEDIUM: guarded u32 decrement, &mut self; liveness via the engine variant	2,4,7,12
notification-queue-bounded-fifo	contract	notifications_api::{enqueue, drain_notifications_json}, take_notifications	∀ q: enqueue(q,n) = q ++ [n] if |q| < 64, else q (the new one is dropped). drain(q) = None ⇔ q = []; else Some("[" ++ join(",", map(notification_json, q)) ++ "]") and q' = []. Invariant: |q| ≤ 64 on every reachable queue. Hence after k ≥ 64 enqueues with no drain, the drain holds exactly the first 64 in order, and the next drain is None.	notifications_api::queue_is_bounded_and_drops_new_when_full	MEDIUM: Arc<Mutex<VecDeque<_>>> (lock, expect), push_back/drain, String join. Factoring a pure fn over VecDeque removes the Mutex (gaps 8, 13).	1,4,8,13
notification-json-and-wiring	contract	notifications_api::{notification_json, urgency_label, wire_notification_queue}, json_string; Terminal OSC 9/99/777 parse (owned by aterm-core)	notification_json(n) = {"id":F(id),"title":F(title),"body":F(body),"urgency":"L"} with F(None) = null, F(Some(s)) = json_string(s), L = urgency_label (Low ↦ low, Critical ↦ critical, else normal). json_string(s) = '"' ++ escape(s) ++ '"', escaping " \ \n \r \t and other C0 as \u00XX. wire: the OSC 9 callback(m) enqueues {id None, title None, body Some(m), Normal}; the advanced callback(n) enqueues n. Engine: OSC 99 i=7:u=2:p=title;Alert ↦ {id 7, title Alert, body None, Critical}; OSC 777;notify;T;B ↦ {title T, body B, Normal}.	notifications_api::{authorized_osc9_round_trips_through_the_drain, advanced_forms_carry_id_title_body_urgency}	MEDIUM: format!, Option map_or_else, char loop, Send + 'static closures moved into engine callbacks	1,3,4,7,12,13
notification-auth-gate	contract	authorize_notifications; Terminal notification dispatch (owned by aterm-core)	authorize_notifications(b) = term.set_allow_notifications(b). Engine: ∀ input, neither notification callback runs unless allow_notifications is true at dispatch time; the default is false and a revoke re-closes the gate. Hence ∀ t ∈ Reach never authorized, or revoked after its last grant, the bytes processed since add nothing to the queue.	notifications_api::unauthorized_notifications_never_enqueue	EASY facade; engine guard on callback invocation	2,4,12,13
predict-facade-gates	refinement	predict_api::{set_predictive_echo, predict_char, predict_backspace, predict_edit_key, predict_overlay, predict_overlay_cursor, predict_reconcile, predict_next_deadline_ms, predict_reset, predict_session_reset, predict_line_submit, predict_unmodelled_input}, resize	set_predictive_echo(s) = set_mode(PredictMode::parse(s)), then predict.reset() when the mode is Off; parse is case-insensitive and unknown strings parse to Off. predict_char(ch) = NoEcho ⇒ note_unmodelled_input; false, else predict.predict_glyph(ch, KeySample { cursor, coordinate_generation, grid: (cols, 0), row }, now), row = the cursor row projected as the reconcile's observe projects it, read only when mode() ≠ Off ∧ display_offset = 0 ∧ wants_key_row(), else None (a scrolled-back view's row at the cursor's index is history, not the cursor's line; with no row the run proves only over a blank tail). predict_edit_key(code): the same gate (answering 0), else predict_edit(k, KeySample { …, row }, now) for code 1 ↦ Left, 2 ↦ Right, 3 ↦ Backspace with the row read whenever mode() ≠ Off ∧ display_offset = 0, answering Declined ↦ 0, Armed ↦ 1, Cancelled ↦ 2 (any other code ↦ 0; with no signed prompt marks on the web every sample hands input_start = None, so Home, End, Ctrl-A, Ctrl-E and Delete stay unmodelled input). predict_backspace = (predict_edit_key(3) = 2). predict_overlay = NoEcho ⇒ reset; [], else shown = overlay(now); display_offset ≠ 0 ⇒ [], else concat [[row, col, ch as u32] | p ← shown] (a state chain's cells while predict_overlay_cursor is non-empty: paint all or none; a shift's moved glyph is a triple like a typed one). predict_overlay_cursor = NoEcho ∨ display_offset ≠ 0 ⇒ [], else overlay_cursor() as [row, col], or []. predict_reconcile = display_offset ≠ 0 ⇒ overlay(now) for expiry only, else reconcile_seen(cursor, coords, NoEcho, now, observe), observe(r,c) = the cell at (r,c) of logical_row_into(r) as a Seen (Blank for a blank or space, Opaque on a wide half, marks or a DEC double-size row), whose glyph is the plain answer. predict_reset = reset; predict_session_reset = reset_session; resize calls predict.reset(). predict_next_deadline_ms = next_deadline().map(ms until it). Hence (Off ⇒ Predictor::predict_char = false): a fresh or unknown-mode terminal tracks nothing; NoEcho leaves no pending guess and no deadline; a scrolled viewport paints nothing and the ghost returns at the bottom; resize drops every guess.	predict_api::{off_by_default_and_unknown_modes_fail_safe, codex_report_event_types_suppresses_arm_reconcile_and_display, scrolled_back_viewport_paints_no_ghosts, resize_drops_inflight_guesses, a_run_s_first_press_hands_the_predictor_its_row, a_press_while_scrolled_back_hands_no_row, an_edit_key_is_armed_and_its_cursor_handed_over}	MEDIUM: &mut self, real clock (Instant::now is an effect; inject it), FnMut closure capturing a scratch Vec and a memo, Vec<u32> building	1,2,4,8,12,13
predictor-epoch-semantics	contract	aterm_predict::Predictor::{predict_char, predict_backspace, reconcile, overlay, next_deadline, note_line_submit, note_unmodelled_input, set_mode, reset, reset_session} (owned by aterm-predict)	With an injected clock, ∀ predictor states: (1) next_deadline() = None ⇔ no pending guess. (2) Always: overlay = the pending unexpired guesses in order. (3) Adaptive: overlay = [] unless the current epoch has a confirmed echo and the RTT estimate is at or above the display threshold. (4) reconcile: a guess whose cell shows its glyph retires (in order); a different glyph flushes all pending guesses and ends the epoch; no_echo flushes. (5) predict_backspace drops the last own guess and returns whether it did. (6) note_line_submit ends the epoch and keeps pending guesses; note_unmodelled_input ends the epoch and drops them. (7) reset drops guesses and the epoch and keeps the RTT; reset_session also forgets the RTT. (8) set_mode(Off) then reset leaves nothing pending.	predict_api::{always_tracks_paints_and_retires_through_the_export, adaptive_tracks_but_never_shows_an_unconfirmed_line, adaptive_confirmed_echo_retires_the_prediction, line_submit_ends_the_epoch_no_same_row_leak, only_a_line_terminator_carries_the_guesses_in_flight, divergence_and_alt_screen_flush, backspace_cancels_and_disable_is_fully_inert}	HARD: cross-crate state machine over a Vec of guesses, Duration arithmetic, closures	1,2,4,7,12,13
tiered-store-ctor-and-drain	contract	scrollback_tiers_api::{tiered_terminal, register_budget_share}, drain_scrollback_backlog, scrollback_backlog_lines	∀ t from new/new_from_system: term.scrollback().is_some(), scrollback_line_limit() = Some(DEFAULT_LINE_LIMIT), and compress offload is active, so ingest stages lines. drain_scrollback_backlog(n) applies any pending budget share, trickles min(batch, backlog) lines with batch = (n = 0 ? 2048 : n), and returns the remaining backlog saturated to u32. Engine: each call with backlog > 0 strictly lowers the backlog, so `while drain(0) > 0 {}` terminates, and the store's line_count rises by the lines promoted.	scrollback_tiers_api::ctor_attaches_tiered_store_with_the_unified_default_total	MEDIUM: builder, u32::try_from; cross-crate LZ4 tier store	1,2,4,7,12
tier-caps-json	contract	scrollback_tiers_api::render_tier_capabilities_json, tier_capabilities_json	∀ caps: render(caps) = "{\"coldCodec\":\"lz4\",\"diskSpill\":" ++ (caps.disk_spill ? "true" : "false") ++ "}" (ColdTierCodec has the one variant Lz4), and tier_capabilities_json() = render(TierCapabilities::current()) by definition.	scrollback_tiers_api::tier_capabilities_json_pins_the_wire_contract	EASY: format! of a bool, one-variant enum match; needs a String model	1,3
band-compose-identity-and-offset	contract	compose_band_into_frame; messages_api::WebBand::{compose, undo, bleed, committed, next_wake}; messages_tick_at, messages_next_wake_ms_at, notice_at, band_rows, surface_rows	∀ t ∈ Reach. band.committed() = 0 ⇒ build_frame's output equals that of a twin T fed the same calls minus the record-only/no-row posts, and cpu.chrome_bleed() = None (identity law). committed() = k > 0 ⇒ frame_scratch.rows = rows + k, grid_top_row = k, grid_bot_row = rows + k, cells[i+k] = the band-less cells[i], cells[0..k] = the band rows, and chrome_bleed = Some. messages_tick_at returns bit 2 set iff the committed row count changed, iff note_band_change(rows_moved). messages_next_wake_ms_at(now) = ms until band.next_wake(now), or −1 when none is armed or the band is hidden. A held warning (hold = h) gives next_wake = h·1000 ms at post time.	messages_api::the_band_composes_into_the_gpu_frame	HARD: Vec row prepend/undo on RenderInput, Instant arithmetic, cross-crate aterm-messages layout	1,2,4,7,12
band-pet-hit-test	contract	pet_press_px, band_metrics; EffectsPipeline pet geometry (owned by aterm-effects)	∀ t ∈ Reach with the pet drawn (cursor_pet_alpha > 0) and any committed band row count k: let B = the drawn pet body rect in frame px, i.e. the free sprite at natural size offset by (pad, grid_top), where grid_top includes the k band rows. Then pet_press_px(x,y) = 1 at B's centre or feet and 0 half a row above B: the hit test reads the same band-adjusted grid_top the frame draws with.	messages_api::the_pet_is_pressed_where_the_band_draws_it	MEDIUM: f32 geometry, cross-crate sprite placement	1,3,4,12
messages-restore-contract	contract	messages_restore_at, messages_page_at; aterm_messages restore/revision (owned by aterm-messages)	∀ logs L encoded by messages_log_since: on a fresh band the first messages_restore_at(L) answers OK records=|L| skipped=0 dup=0 over=0; every later restore answers ERR restore:. A restored held record has state=stale and adds no band row, no swapchain growth (surface_rows unchanged) and no wake (−1). The next post takes id max(L)+1 and commits a band row. A restore that changes the record count moves the page's rev=, and so does every post.	messages_api::{the_gpu_band_restores_the_kept_log, the_restore_moves_the_pages_rev}	HARD: &str line parsing, Vec<String>, cross-crate message centre	1,2,4,7,12
messages-twin-equivalence	equivalence	messages_api::* (this crate) and aterm-wasm messages_api::* (twin); aterm_messages page/view/wire (owned by aterm-messages)	∀ call sequences over notice/tick/hover/press/visible/details/forced-colors/restore/page/entry/log_since with identical clocks, wall stamps and grid size: every string reply equals the twin's, and every band row in this crate's RenderInput equals the twin's. Engine: the forced palette maps to BandInks::forced(unpack of each slot); a hidden band arms no wake; a restored crash record lists Open log/Manual with actionable=0 and primary=0 and no press reaches it; a lit chip under a still pointer is dropped when the layout echoes, so the frame equals a pointer-less twin's. Definitional once both crates share one glue implementation (§5.4).	messages_api::{the_gpu_band_has_the_twins_exports, the_gpu_page_is_the_twins_page, the_gpu_page_offers_what_the_twin_offers, a_still_pointers_chip_never_lights_the_echo}	HARD as written (two copies of the glue across crates, String formatting); TRIVIAL after the glue is shared	1,2,4,7,12
cpu-gpu-parity	equivalence	aterm_gpu::GpuRenderer::render_input (owned by aterm-gpu); aterm_render::Renderer::render_input (owned by aterm-render); gpu_face	∀ RenderInput x (including band rows and chrome rasters), ∀ face/theme: the GPU frame has the CPU frame's (width, height), and every packed channel differs by at most 8, the difference coming only from round-vs-floor coverage. to_rgba8 expands each 0x00RRGGBB pixel to [r, g, b, 0xff], with length width·height·4.	tests/cpu_gpu_parity.rs::web_cpu_gpu_rgba8_parity, messages_api::the_band_frame_is_one_picture_on_both_web_renderers	VERY HARD: GPU shader execution (Metal natively, GL in the browser lane), driver nondeterminism; needs a shader semantics model. Environment-gated today (skips without a GPU). The to_rgba8 clause alone is EASY-MEDIUM.	1,8,12,13
selection-range-projection	contract	facade_api::{selection_range_for,display_row_to_terminal}; selection_start, selection_extend, selection_range	The shared aterm-wasm implementation maps display rows with saturating subtraction and projects anchors with saturating addition of a checked-narrowed display offset. Partly visible selections clip to the viewport; extreme i32 endpoints stay off-screen without wrapping. Representable viewport endpoints round-trip.	facade_api::hostile_selection_rows_stay_offscreen_without_wrapping	MEDIUM: saturating i32 arithmetic, checked narrowing, project_range and viewport clipping	1,3,4,12
```

## 2. Test → spec map (TSV; all 93)

`primary` is the spec that retires the test. `also` lists other specs the same test samples.
Files are relative to `crates/aterm-gpu-web`.

```tsv
file	test	primary	also
shared facade_api.rs	hostile_selection_rows_stay_offscreen_without_wrapping	selection-range-projection
shared theme_api.rs	catalog_json_lists_builtins_then_registered	web-themes	
shared theme_api.rs	named_install_keeps_program_colours	web-themes	theme-reset-baseline
shared theme_api.rs	register_bounds_and_shadowing	web-themes	
shared theme_api.rs	register_then_select_follows_a_re_registration	web-themes	
shared theme_api.rs	resolve_is_atomic_and_loud	web-themes	
shared theme_api.rs	set_theme_by_name_matches_the_shared_projection_for_every_builtin	web-themes	
shared theme_api.rs	set_theme_keeps_program_colours	theme-reset-baseline	
shared theme_api.rs	split_follows_set_color_scheme	web-themes	
shared theme_api.rs	theme_installs_reopen_the_frame_gate	web-themes	
shared theme_api.rs	unregister_refuses_the_drawn_theme	web-themes	
shared theme_api.rs	web_translucency_raises_the_effective_floor	web-themes	
src/lib.rs	every_visual_mutator_bumps_the_host_visual_generation	visual-mutators-bump	
src/lib.rs	effects_config_mutators_bump_the_host_visual_generation	visual-mutators-bump	
src/lib.rs	frame_gate_skips_settled_ticks_and_reopens_on_every_change_class	frame-gate	visual-mutators-bump, scroll-export-composition
src/lib.rs	webgl_present_result_propagates_every_surface_failure	webgl-present-result	
src/lib.rs	host_theme_colors_are_the_dynamic_color_reset_baseline	theme-reset-baseline	
src/lib.rs	frame_scratch_tracks_live_sparse_blank_and_cursor_colors	frame-scratch-live-colors	theme-reset-baseline
src/lib.rs	a_link_after_wide_characters_is_read_from_its_cells	link-at-cells	
src/lib.rs	the_gpu_face_is_the_face_the_host_set	gpu-face-retains-host-settings	
src/lib.rs	console_input_api_counts_commits_without_replaying_their_echoes	console-input-note	
src/lib.rs	paste_export_wraps_only_sanitized_text_so_a_planted_terminator_stays_inert	format-paste-sanitize-then-wrap	
src/lib.rs	search_meta_agrees_with_search_and_is_silent_on_empty_or_bad_input	search-meta-agrees	
src/lib.rs	search_budgeted_matches_one_shot_and_restarts_when_superseded	budgeted-search-refines-one-shot	
src/lib.rs	oversized_frames_are_refused_before_state_changes	dims-mirror-grid	frame-gate
src/lib.rs	retention_shrink_search_returns_no_evicted_rows	scrollback-limit-one-total	
src/lib.rs	set_scrollback_limit_governs_ring_retention	scrollback-limit-one-total	
src/lib.rs	encode_key_with_mode_matches_the_instance_encoder	encode-key-refines-mode	
src/lib.rs	a_conpty_host_opts_in_to_win32_input_records	keyboard-mode-tracking	
src/lib.rs	build_frame_echo_refills_one_row_with_sparkle_and_rain_on	scoped-refill-equals-full	
src/lib.rs	build_frame_scoped_refill_matches_a_full_extraction_over_a_mutation_stream	scoped-refill-equals-full	
src/lib.rs	spill_exports_surface_band_content_on_the_cpu_face	spill-band-composes-to-frame	chrome-frame-size
src/lib.rs	set_chrome_sticks_pre_init_and_zero_restores	chrome-frame-size	
src/lib.rs	effects_snapshots_are_deterministic_and_restore_on_off	effects-deterministic	effects-off-identity
src/lib.rs	set_word_separators_reshapes_double_click_words	word-separators	
src/lib.rs	scrolled_wrapped_row_len_and_wrap_flag_are_tier_aware	row-queries-display-aware	
src/lib.rs	display_row_cache_distinguishes_main_and_alt_screen	display-row-cache-transparent	
src/lib.rs	resize_without_tiered_store_never_defers	reflow-defer-policy	
src/lib.rs	small_tiered_history_rewraps_inline_on_resize	reflow-defer-policy	
src/lib.rs	large_tiered_history_defers_rewrap_and_stepped_pumps_reattach_it	reflow-pump-converges	reflow-defer-policy
src/lib.rs	resize_mid_stepping_supersedes_without_losing_history	reflow-pump-converges	reflow-defer-policy
src/lib.rs	never_pumped_host_completes_via_the_render_grace_net	reflow-safety-nets	reflow-pump-converges, frame-gate
src/lib.rs	never_pumped_host_streaming_output_converges_via_the_backlog_net	reflow-safety-nets	reflow-pump-converges
src/messages_api_tests.rs	the_band_composes_into_the_gpu_frame	band-compose-identity-and-offset	
crates/aterm-wasm/src/messages_api.rs	the_pet_is_pressed_where_the_band_draws_it	band-pet-hit-test	band-compose-identity-and-offset
src/messages_api_tests.rs	the_gpu_band_restores_the_kept_log	messages-restore-contract	swapchain-sized-to-surface-rows
crates/aterm-wasm/src/messages_api.rs	the_restore_moves_the_pages_rev	messages-restore-contract	messages-twin-equivalence
src/messages_api_tests.rs	the_gpu_band_has_the_twins_exports	messages-twin-equivalence	
src/messages_api_tests.rs	the_gpu_page_is_the_twins_page	messages-twin-equivalence	
src/messages_api_tests.rs	the_gpu_page_offers_what_the_twin_offers	messages-twin-equivalence	
crates/aterm-wasm/src/messages_api.rs	a_still_pointers_chip_never_lights_the_echo	messages-twin-equivalence	
src/messages_api_tests.rs	the_band_frame_is_one_picture_on_both_web_renderers	cpu-gpu-parity	band-compose-identity-and-offset
crates/aterm-wasm/src/notifications_api.rs	queue_is_bounded_and_drops_new_when_full	notification-queue-bounded-fifo	
crates/aterm-wasm/src/notifications_api.rs	authorized_osc9_round_trips_through_the_drain	notification-json-and-wiring	notification-auth-gate, notification-queue-bounded-fifo
crates/aterm-wasm/src/notifications_api.rs	advanced_forms_carry_id_title_body_urgency	notification-json-and-wiring	notification-auth-gate
crates/aterm-wasm/src/notifications_api.rs	unauthorized_notifications_never_enqueue	notification-auth-gate	
crates/aterm-wasm/src/notifications_api.rs	cursor_color_follows_osc12_and_resets_on_112	theme-reset-baseline	
crates/aterm-wasm/src/predict_api.rs	off_by_default_and_unknown_modes_fail_safe	predict-facade-gates	
crates/aterm-wasm/src/predict_api.rs	codex_report_event_types_suppresses_arm_reconcile_and_display	predict-facade-gates	predictor-epoch-semantics
crates/aterm-wasm/src/predict_api.rs	scrolled_back_viewport_paints_no_ghosts	predict-facade-gates	
crates/aterm-wasm/src/predict_api.rs	resize_drops_inflight_guesses	predict-facade-gates	
crates/aterm-wasm/src/predict_api.rs	an_edit_key_is_armed_and_its_cursor_handed_over	predict-facade-gates	predictor-epoch-semantics
crates/aterm-wasm/src/predict_api.rs	a_shift_s_moved_text_comes_out_as_triples	predict-facade-gates	predictor-epoch-semantics
crates/aterm-wasm/src/predict_api.rs	always_tracks_paints_and_retires_through_the_export	predictor-epoch-semantics	predict-facade-gates
crates/aterm-wasm/src/predict_api.rs	adaptive_tracks_but_never_shows_an_unconfirmed_line	predictor-epoch-semantics	
crates/aterm-wasm/src/predict_api.rs	adaptive_confirmed_echo_retires_the_prediction	predictor-epoch-semantics	
crates/aterm-wasm/src/predict_api.rs	line_submit_ends_the_epoch_no_same_row_leak	predictor-epoch-semantics	
crates/aterm-wasm/src/predict_api.rs	only_a_line_terminator_carries_the_guesses_in_flight	predictor-epoch-semantics	
crates/aterm-wasm/src/predict_api.rs	divergence_and_alt_screen_flush	predictor-epoch-semantics	predict-facade-gates
crates/aterm-wasm/src/predict_api.rs	backspace_cancels_and_disable_is_fully_inert	predictor-epoch-semantics	predict-facade-gates
crates/aterm-wasm/src/scroll_input_api.rs	fractional_deltas_accumulate_without_flipping	scroll-accumulator	
crates/aterm-wasm/src/scroll_input_api.rs	row_flips_at_exactly_plus_and_minus_one	scroll-accumulator	
crates/aterm-wasm/src/scroll_input_api.rs	multi_row_deltas_flip_whole_rows_and_bank_the_remainder	scroll-accumulator	
crates/aterm-wasm/src/scroll_input_api.rs	direction_reversal_can_cancel_without_a_flip	scroll-accumulator	
crates/aterm-wasm/src/scroll_input_api.rs	non_finite_deltas_are_ignored	scroll-accumulator	
crates/aterm-wasm/src/scroll_input_api.rs	huge_deltas_saturate_without_panicking	scroll-accumulator	
crates/aterm-wasm/src/scroll_input_api.rs	reset_drops_the_residual	scroll-clamp-and-reset	
crates/aterm-wasm/src/scroll_input_api.rs	history_edges_clamp_the_empty_side_only	scroll-clamp-and-reset	
crates/aterm-wasm/src/scroll_input_api.rs	frac_px_negates_rounds_and_clamps_into_the_translate_domain	scroll-frac-px-and-stamp	
crates/aterm-wasm/src/scroll_input_api.rs	stamp_fills_the_full_grid_band_every_frame	scroll-frac-px-and-stamp	
crates/aterm-wasm/src/scroll_input_api.rs	scroll_px_accumulates_and_flips_through_the_export	scroll-export-composition	scroll-accumulator
crates/aterm-wasm/src/scroll_input_api.rs	whole_row_navigation_resets_the_residual	scroll-export-composition	scroll-clamp-and-reset
crates/aterm-wasm/src/scroll_input_api.rs	history_ends_clamp_the_residual_through_the_export	scroll-export-composition	scroll-clamp-and-reset
src/scroll_input_api_tests.rs	render_snapshot_carries_the_banked_residual	scroll-export-composition	scroll-frac-px-and-stamp
crates/aterm-wasm/src/scrollback_tiers_api.rs	ctor_attaches_tiered_store_with_the_unified_default_total	tiered-store-ctor-and-drain	
crates/aterm-wasm/src/scrollback_tiers_api.rs	tier_capabilities_json_pins_the_wire_contract	tier-caps-json	
tests/cpu_gpu_parity.rs	web_cpu_gpu_rgba8_parity	cpu-gpu-parity	
```

## 3. Coverage

- 93 tests read. 92 map to one of the 46 specs; 1 is not a spec sample (§4).
- Biggest collapses: `predictor-epoch-semantics` (7 primary), `scroll-accumulator` (6),
  `reflow-pump-converges`, `predict-facade-gates`, `messages-twin-equivalence` and
  `scroll-export-composition` (4 each).
- 28 of the 45 specs carry an *(owned by …)* clause. Their facade halves are cheap; the
  substantive halves migrate with `aterm-core`, `aterm-render`, `aterm-gpu`, `aterm-effects`,
  `aterm-messages`, `aterm-predict` and `aterm-types`, and their tests here are likely
  duplicates of those crates' tests and of `aterm-wasm`'s (§5.6).
- Facade-local specs with no cross-crate clause (the cheap proofs): the four `scroll-*`
  `ScrollInputState` specs, `visual-mutators-bump`, `webgl-present-result`, `tier-caps-json`,
  `notification-queue-bounded-fifo`, `display-row-cache-transparent`, `encode-key-refines-mode`
  (given the bitflags round trip), and the facade halves of `frame-gate`, `reflow-*` and
  `predict-facade-gates`.

## 4. Tests that are not spec samples

| test | why | action |
|---|---|---|
| predict_api::session_reset_forgets_rtt_while_coordinate_reset_preserves_it | Timing-dependent: real `std::thread::sleep(20ms)` and `Instant::now` make a confirmation "slow", and a scheduler stall can make the "immediate local echo" case look slow, flipping the last assertion. The property (reset keeps the RTT, reset_session forgets it) is clause (7) of `predictor-epoch-semantics`, provable with aterm-predict's injected clock; the facade halves (`predict_reset = reset`, `predict_session_reset = reset_session`) are in `predict-facade-gates`. | **delete** |

## 5. Implied specs that look wrong, or disagree with the code

1. **No test samples `swapchain-sized-to-surface-rows`.** The spec is blocked on a
   GPU-surface model (gap 8), and no source-text count of `.frame_size(` lines stands in for
   it: such a count breaks on harmless refactors and passes a semantic bug spelled the same
   way.
2. **`needs_frame`'s doc may overclaim.** It says "false is authoritative: the following
   render() will skip". But `open_frame` first runs `drain_compress_backlog_on_render`, which
   applies a pending global budget share (`term.set_memory_budget`) and trickles staged lines.
   If either advances `damage_epoch`, `needs_frame() = false` is followed by a drawn frame.
   Prove that those calls never move the epoch (which `frame-gate` then states), or weaken the
   doc.
3. **Latent real-clock dependence in the predict tests.**
   `always_tracks_paints_and_retires_through_the_export`, `backspace_cancels_and_disable_is_fully_inert`
   and `only_a_line_terminator_carries_the_guesses_in_flight` assert a non-empty overlay right
   after `predict_char` on the monotonic clock; a stall past the ~250 ms guess expiry empties
   it. The state-machine claims belong in aterm-predict with an injected clock, and the facade
   should take its clock through an injected field so `predict-facade-gates` is about a pure
   function.
4. **The messages glue is copy-pasted between the two web crates.** The `messages_api.rs` twin
   tests (`the_gpu_band_has_the_twins_exports`, `the_gpu_page_is_the_twins_page`,
   `the_gpu_page_offers_what_the_twin_offers`, `a_still_pointers_chip_never_lights_the_echo`,
   `the_restore_moves_the_pages_rev`) are self-described abridged copies of aterm-wasm's tests
   over a copied module. The general fix is one shared glue implementation; these tests then
   become duplicates (delete) and `messages-twin-equivalence` becomes definitional.
5. **A magic drain bound.** `spill_exports_surface_band_content_on_the_cpu_face` asserts the
   aurora drains within `DRAIN_FRAMES = 125` frames (2 s of injected dt), against a commented
   "~740 ms" envelope, and the bound is unrelated to the configured `duration_ms = 400`. The spec
   states the drain bound as a function of the glow config, not as 125 frames.
6. **Integration samples of other crates.** Most `lib.rs` tests (search, scrollback limit,
   keyboard mode, word separators, row queries, scoped refill, frame-scratch colours, theme
   reset) reach aterm-core, aterm-render, aterm-effects or aterm-types through one-line
   delegations. The gpu-web half of each spec is trivial; the substantive clause belongs in the
   owning crate's `SPECS.md`, and `aterm-wasm` likely samples the same properties.
7. **Coverage witnesses are not spec.** The `build_frame_*` tests observe the
   `#[cfg(test)]`-only field `last_refill`. It records a production return value, so the spec is
   real, but the `scoped >= 5` / `full >= 2` counts are witnesses for the chosen stream and are
   not part of `scoped-refill-equals-full`.
8. **Vacuous passes.** Several tests silently `return` when `new_from_system` finds no system
   font or `GpuContext::new` fails, so on such a machine they pass without checking anything.
   This is one more reason to replace them with proofs. The paste test already avoids it by
   using the bundled face.

## 6. Proof order (easiest high-value first)

No spec here is blocked by gap 0 alone: every one needs at least floats, `&mut self` or a
non-scalar carrier. The order below takes the smallest gap sets first and the specs others
build on.

1. **The `ScrollInputState` family**: `scroll-accumulator`, `scroll-clamp-and-reset`,
   `scroll-frac-px-and-stamp` (gaps 2, 3, plus 14 and 1 for the stamp half). This is pure scalar
   `f64` arithmetic on a one-field struct, with no loops and no cross-crate calls, and it retires
   10 tests. Factoring each method into a pure `fn(f64, …) -> (f64, i32)` that the `&mut self`
   method calls removes gap 2 for the core; the remaining work is an IEEE binary64 model in Clean
   (`t − trunc(t)` exact, saturating `as i32`). Once it lands, the facade half of
   `scroll-export-composition` (4 more tests) needs only gap 4 plus aterm-core's
   `scroll_display` clamp.
2. **`tier-caps-json`** (gaps 1, 3). This is one `format!` of a bool through a one-variant enum,
   the smallest first `String`-carrier case in the crate. It is also a good probe for gap 1's
   `String` model.
3. **`visual-mutators-bump`** (gaps 2, 4). This is a `u64::wrapping_add` and a frame lemma over
   disjoint fields. It retires 2 tests, including the one that enumerates every setter, and it is
   the lemma `frame-gate` (2 tests) stands on. `frame-gate` then follows, with a `damage_epoch`
   citation from aterm-core (gap 12) that also settles §5.2.
4. **`notification-queue-bounded-fifo`** (gaps 1, 4; 8 and 13 disappear after factoring a pure
   `enqueue(&mut VecDeque, n)` out of the `Arc<Mutex<…>>`). This is a bounded FIFO with an
   obvious invariant `|q| ≤ 64`, and it is the first `VecDeque` content-model case.
5. **`webgl-present-result`**, `is_ok` clause (gaps 1, 3; enum carrier only). The message
   clause waits for the `String` model.

After these, the delegation specs (`mode-getters-delegate`, `encode-key-refines-mode`,
`dims-mirror-grid`, `notification-auth-gate`, `chrome-frame-size`) are trivial on the facade
side and wait on gap 12 and their owning crates' proofs. `cpu-gpu-parity` and
`swapchain-sized-to-surface-rows` come last (gap 8).
