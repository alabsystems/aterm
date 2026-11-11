// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_render::TrailCell;

fn rain_heard(p: &EffectsPipeline) -> (crate::matrix_rain::RainSignal, bool) {
    p.rain.as_deref().expect("rain is on").heard_for_test()
}

fn page_frame(p: &mut EffectsPipeline, term: &mut Terminal, input: &mut RenderInput) {
    p.advance(16.0);
    term.cell_frame_into(input, 6, 20);
    p.apply(term, input, 8, 16);
}

#[test]
fn web_rain_hears_command_completions_and_execute_edges() {
    use crate::matrix_rain::RainSignal;
    for suppressed_alt in [false, true] {
        let mut p = EffectsPipeline::new();
        p.rain_cfg.suppress_in_alt_screen = true;
        enable_classic_rain(&mut p);
        let mut term = Terminal::new(6, 20);
        let mut input = RenderInput::default();
        page_frame(&mut p, &mut term, &mut input);
        let session = term.render_identity();
        assert_eq!(p.rain_latches.last_cmd, Some((session, 0)));
        assert_ne!(rain_heard(&p).0, RainSignal::Execute, "nothing ran yet");

        term.process(b"\x1b]133;A\x07$ \x1b]133;B\x07false\r\n\x1b]133;C\x07");
        if suppressed_alt {
            term.process(b"\x1b[?1049h");
        }
        page_frame(&mut p, &mut term, &mut input);
        assert_eq!(p.rain_latches.shell_executing, Some((session, true)));
        let execute_heard = rain_heard(&p).0 == RainSignal::Execute;
        term.process(b"\x1b]133;D;1\x07");
        page_frame(&mut p, &mut term, &mut input);
        let seq = term.completed_command_seq();
        assert!(seq > 0, "fixture: the shell reported a completion");
        assert_eq!(
            p.rain_latches.last_cmd,
            Some((session, seq)),
            "the latch consumed the completion either way"
        );
        let ember = rain_heard(&p).1;
        if suppressed_alt {
            assert!(
                !execute_heard && !ember,
                "a suspended rain hears neither edge, and the completion stays baselined"
            );
            assert!(
                term.is_alternate_screen(),
                "fixture: the frames were suppressed"
            );
        } else {
            assert!(execute_heard, "the Execute edge reached the weather");
            assert!(ember, "the failure reached the weather: its ember holds");
        }
    }
}

#[test]
fn a_torn_frame_delivers_the_execute_edge_on_the_next_fresh_snapshot() {
    use crate::matrix_rain::RainSignal;
    let mut p = EffectsPipeline::new();
    enable_classic_rain(&mut p);
    let mut term = Terminal::new(6, 20);
    let mut input = RenderInput::default();
    page_frame(&mut p, &mut term, &mut input);
    p.advance(16.0);
    term.cell_frame_into(&mut input, 6, 20);
    term.process(b"\x1b]133;A\x07$ \x1b]133;B\x07sleep 1\r\n\x1b]133;C\x07");
    assert_ne!(
        input.snapshot_seq,
        term.damage_epoch(),
        "fixture: the snapshot is torn"
    );
    p.apply(&mut term, &mut input, 8, 16);
    assert_ne!(
        rain_heard(&p).0,
        RainSignal::Execute,
        "the torn frame drew nothing"
    );
    page_frame(&mut p, &mut term, &mut input);
    assert_eq!(
        rain_heard(&p).0,
        RainSignal::Execute,
        "the edge reached the weather with the next fresh snapshot"
    );
}

#[test]
fn re_enabling_web_rain_replays_no_completion_from_while_it_was_off() {
    for glow_on in [false, true] {
        let mut p = EffectsPipeline::new();
        if glow_on {
            p.set_cursor_glow(
                true,
                "lumen",
                crate::cursor_fx::TrailColor::Automatic,
                None,
                400,
                24,
                0.9,
                0.8,
                true,
                0x0050_FA7B,
            );
        }
        enable_classic_rain(&mut p);
        let mut term = Terminal::new(6, 20);
        let mut input = RenderInput::default();
        page_frame(&mut p, &mut term, &mut input);
        p.set_matrix_rain_enabled(false);
        assert_eq!(p.enabled_any(), glow_on, "fixture: which off arm runs");
        term.process(b"\x1b]133;A\x07$ \x1b]133;B\x07false\r\n\x1b]133;C\x07");
        page_frame(&mut p, &mut term, &mut input);
        term.process(b"\x1b]133;D;1\x07\x1b]133;A\x07$ \x1b]133;B\x07");
        page_frame(&mut p, &mut term, &mut input);
        assert!(term.completed_command_seq() > 0, "fixture: a completion");
        enable_classic_rain(&mut p);
        page_frame(&mut p, &mut term, &mut input);
        assert!(
            !rain_heard(&p).1,
            "glow_on={glow_on}: an off-era failure lights no ember"
        );
        term.process(b"false\r\n\x1b]133;C\x07\x1b]133;D;1\x07");
        page_frame(&mut p, &mut term, &mut input);
        assert!(
            rain_heard(&p).1,
            "glow_on={glow_on}: a failure with the rain on lights the ember"
        );
    }
}

#[test]
fn set_cursor_glow_resolves_through_the_native_config_law() {
    let mut p = EffectsPipeline::new();
    p.set_cursor_glow(
        true,
        "lumen",
        crate::cursor_fx::TrailColor::Automatic,
        None,
        400,
        24,
        f32::NAN,
        f32::INFINITY,
        true,
        0,
    );
    assert_eq!(p.glow_cfg.intensity, 0.0);
    assert_eq!(p.glow_cfg.radius, 0.0);
}

use crate::cursor_trail::TypingCadence;
use aterm_core::render::{FxBlend, FxLayer, FxSlot};
use aterm_core::terminal::TerminalBuilder;
use aterm_render::sprites::FreeSprite;

/// The frame's published companion sprites, in draw order.
fn free_sprites(input: &RenderInput) -> Vec<FreeSprite> {
    aterm_render::sprites::published(&input.fx_layers)
}

/// The frame's published rain rows.
fn rain_rows(input: &RenderInput) -> Vec<FxLayer> {
    input
        .fx_layers
        .iter()
        .filter(|l| l.blend == FxBlend::Linear && l.slot == FxSlot::Under && !l.knock)
        .cloned()
        .collect()
}

fn stamp_effective_terminal_background(term: &Terminal, input: &mut RenderInput) {
    let color = if term.modes().reverse_video() {
        term.default_foreground()
    } else {
        term.default_background()
    };
    input.default_bg = aterm_render::rgb_to_u32([color.r, color.g, color.b]);
}

fn commit_ascii(
    pipeline: &mut EffectsPipeline,
    term: &mut Terminal,
    input: &mut RenderInput,
    ch: &[u8],
    dt_ms: f64,
) -> u64 {
    assert_eq!(ch.len(), 1, "fixture accepts one ASCII byte");
    assert!(
        pipeline.note_committed_char(term, input, char::from(ch[0])),
        "fixture must price the press at its exact cell width"
    );
    term.process(ch);
    pipeline.advance(dt_ms);
    let (rows, cols) = (input.rows, input.cols);
    term.cell_frame_into(input, rows, cols);
    pipeline.apply(term, input, 10, 19)
}

fn validate_cursor_scroll_action(action: &str, event: i64, decision: ContentScrollDelta) {
    let base = aterm_models_effects::cursor_scroll_signal_model();
    let model = aterm_spec::interp::with_consts(&base, &[]);
    let source = model.init_state();
    let mut projected = source.clone();
    projected.insert("event", event);
    match decision {
        ContentScrollDelta::Translate(delta) => {
            assert_eq!(delta, 2, "model fixture uses the exact two-row batch");
            projected.insert("uniform_rows", i64::from(delta));
            projected.insert("decision", 1);
            projected.insert("survivor_y", 1);
            projected.insert("proof_alive", 0);
        }
        ContentScrollDelta::Bands { count, .. } => {
            assert_eq!(count, 1, "model fixture uses the one-band batch");
            projected.insert("epoch", 1);
            projected.insert("band_batches", 1);
            projected.insert("decision", 3);
            projected.insert("survivor_y", 5);
            projected.insert("geometry_alive", 1);
            projected.insert("proof_alive", 0);
        }
        ContentScrollDelta::Invalidate => {
            projected.insert("epoch", 1);
            projected.insert("decision", 2);
            projected.insert("geometry_alive", 0);
            projected.insert("proof_alive", 0);
        }
        other => panic!("model event must produce a host action, got {other:?}"),
    }
    let (admitted, why) = aterm_spec::verify::validate_transition_tiered(
        &base,
        &[],
        &source,
        &projected,
        Some(action),
        "EffectsPipeline cumulative cursor-scroll decision",
    );
    assert!(admitted, "real pipeline decision rejected by model: {why}");

    let mut stranded = projected;
    stranded.insert("decision", 0);
    let (admitted, _) = aterm_spec::verify::validate_transition_tiered(
        &base,
        &[],
        &source,
        &stranded,
        Some(action),
        "EffectsPipeline retained-count negative control",
    );
    assert!(
        !admitted,
        "a stranded no-op scroll decision must be rejected"
    );
}

fn seed_pipeline_trail(
    pipeline: &mut EffectsPipeline,
    term: &mut Terminal,
    row: u16,
) -> (RenderInput, Vec<TrailCell>) {
    pipeline.set_cursor_trail(true, 400, 24, None, 0x0050_FA7B);
    term.process(format!("\x1b[{};2H", row + 1).as_bytes());
    let mut input = term.cell_frame(5, 16);
    pipeline.apply(term, &mut input, 10, 19); // silent scroll baseline + cursor seed
    commit_ascii(pipeline, term, &mut input, b"x", 16.0);
    assert!(
        !input.cursor_trail.is_empty(),
        "fixture must own live geometry"
    );
    let trail = input.cursor_trail.clone();
    (input, trail)
}

fn validate_pipeline_layout_prepare(label: &str) {
    let model = aterm_models_misc::layout_coordinate_reset_model();
    let charged = model.successors("Charge", &model.init_state())[0].clone();
    let changed = model.successors("ChangeCoordinate", &charged)[0].clone();
    let mut prepared = changed.clone();
    prepared.insert("charged", 0);
    prepared.insert("bound_coordinate", changed["coordinate"]);
    prepared.insert("prepared", 1);
    let (ok, why) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &changed,
        &prepared,
        Some("Prepare"),
        label,
    );
    assert!(ok, "real pipeline coordinate reset rejected: {why}");

    let mut stale = prepared;
    stale.insert("charged", 1);
    stale.insert("bound_coordinate", charged["bound_coordinate"]);
    let (ok, _) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &changed,
        &stale,
        Some("Prepare"),
        "EffectsPipeline retained-coordinate negative control",
    );
    assert!(!ok, "a changed pipeline coordinate may not retain geometry");
}

#[test]
fn pipeline_history_uses_the_exact_frame_snapshot_and_retires_cursor_state() {
    let mut term = TerminalBuilder::new()
        .size(5, 16)
        .ring_buffer_size(32)
        .build();
    configure_glass_theme(&mut term);
    let mut pipeline = EffectsPipeline::new();
    rainbow_kitty_glow(&mut pipeline, "rainbow kitty");
    pipeline.set_cursor_pet(true, 0x5EED);
    let (mut input, _) = seed_pipeline_trail(&mut pipeline, &mut term, 2);
    assert!(
        pipeline.cursor.trail.is_active(),
        "fixture owns classic geometry"
    );
    assert!(
        pipeline.cursor.glow.is_active(),
        "fixture owns Glow geometry"
    );
    materialize_pet(&mut pipeline, &mut term, &mut input);
    let pet_brain_pending = pipeline.companion.brain().is_active();
    assert!(pet_brain_pending, "fixture owns a live resident");

    term.process(&b"history\r\n".repeat(12));
    term.scroll_display(1);
    term.cell_frame_into(&mut input, 5, 16);
    assert!(
        input.display_offset > 0,
        "fixture extracted retained history"
    );
    assert!(
        !input.cursor_visible,
        "the exact frame snapshot centrally suppresses the DEC cursor"
    );
    input.cursor_visible = true;
    pipeline.advance(33.0);
    pipeline.apply(&mut term, &mut input, 10, 19);
    assert!(
        !pipeline.cursor.trail.is_active()
            && !pipeline.cursor.glow.is_active()
            && input.cursor_trail.is_empty()
            && input.cursor_glow_add.is_empty()
            && input.glow_halo.is_empty()
            && input.fire_patch.is_empty()
            && input.glow_under.is_empty()
            && input.cursor_fill_override.is_none()
            && !input.cursor_visible,
        "history hard-clears every pipeline-owned cursor channel"
    );
    assert!(
        pipeline.cursor_pet_alpha() == 0
            && pipeline.companion_hit_rect().is_none()
            && free_sprites(&input).is_empty(),
        "history takes the pet off glass on the very first frame"
    );
    assert!(
        pipeline.companion.brain().is_active(),
        "…while its brain is still fading: the pet hides, it is not retired"
    );
    for _ in 0..40 {
        if !pipeline.companion.needs_frames() {
            break;
        }
        pipeline.advance(100.0);
        term.cell_frame_into(&mut input, 5, 16);
        pipeline.apply(&mut term, &mut input, 10, 19);
        assert!(
            free_sprites(&input).is_empty(),
            "history never draws the pet"
        );
    }
    assert!(
        !pipeline.companion.needs_frames() && !pipeline.companion.brain().is_active(),
        "hidden-caret ticks let the resident fade out and release the scheduler"
    );

    let model = aterm_models_effects::cursor_viewport_lifecycle_model();
    let charged = model.successors("ChargeLive", &model.init_state())[0].clone();
    let mut history = charged.clone();
    history.insert("live_viewport", 0);
    history.insert("glow_visible", i64::from(pipeline.cursor.glow.is_active()));
    history.insert(
        "trail_visible",
        i64::from(pipeline.cursor.trail.is_active()),
    );
    history.insert(
        "cursor_body_visible",
        i64::from(input.cursor_fill_override.is_some()),
    );
    history.insert("pet_visible", i64::from(pipeline.cursor_pet_alpha() > 0));
    history.insert("pet_brain_pending", i64::from(pet_brain_pending));
    history.insert("base_cursor_visible", i64::from(input.cursor_visible));
    history.insert(
        "pet_brain_ticked",
        i64::from(!pipeline.companion.brain().is_active()),
    );
    history.insert(
        "scheduler_stuck",
        i64::from(pipeline.companion.needs_frames()),
    );
    let (ok, why) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &charged,
        &history,
        Some("EnterHistory"),
        "EffectsPipeline exact history snapshot",
    );
    assert!(ok, "pipeline history projection rejected: {why}");
    let mut forged_fill = history.clone();
    forged_fill.insert("cursor_body_visible", 1);
    let (ok, _) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &charged,
        &forged_fill,
        Some("EnterHistory"),
        "EffectsPipeline forged-fill negative control",
    );
    assert!(!ok, "a forged cursor fill cannot survive history");
    let mut retained_pet = history.clone();
    retained_pet.insert("pet_visible", 1);
    let (ok, _) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &charged,
        &retained_pet,
        Some("EnterHistory"),
        "EffectsPipeline retained-pet negative control",
    );
    assert!(!ok, "a pet body cannot survive history");
    let mut frozen_brain = history;
    frozen_brain.insert("pet_brain_ticked", 0);
    frozen_brain.insert("scheduler_stuck", 1);
    let (ok, _) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &charged,
        &frozen_brain,
        Some("EnterHistory"),
        "EffectsPipeline frozen-brain negative control",
    );
    assert!(
        !ok,
        "a brain that was not ticked in history pins the scheduler"
    );
}

#[test]
fn every_web_rainbow_kitty_spelling_is_v2() {
    let mut p = EffectsPipeline::new();
    for (style, kitty) in [
        ("rainbow kitty", true),
        ("rainbow kitty pet", true),
        ("nyan rainbow", true),
        ("rainbow kitty v1", false),
        ("kitty v1", false),
    ] {
        rainbow_kitty_glow(&mut p, style);
        let want = if kitty {
            GlowStyle::RainbowKitty
        } else {
            GlowStyle::Lumen
        };
        assert_eq!(p.glow_cfg.style, want, "{style:?}");
    }
    for (style, v2) in [("rainbow kitty", true), ("rainbow kitty v1", false)] {
        let mut p = EffectsPipeline::new();
        rainbow_kitty_glow(&mut p, style);
        let (mut term, mut input) = glass(8, 40);
        for _ in 0..6 {
            commit_ascii(&mut p, &mut term, &mut input, b"k", 33.0);
        }
        assert_eq!(p.cursor.glow.v2_status().is_some(), v2, "{style:?}: v2");
    }
    for (style, named, species) in [
        ("rainbow kitty", true, PetSpecies::Cat),
        ("kitty", true, PetSpecies::Cat),
        ("rainbow kitty pet", true, PetSpecies::Cat),
        ("Rainbow Kitty Underline", true, PetSpecies::Cat),
        ("rainbow dog pet", true, PetSpecies::Dog),
        ("pet dog", true, PetSpecies::Dog),
        ("rainbow kitty flying", false, PetSpecies::Cat),
        ("rainbow", false, PetSpecies::Cat),
        ("lumen", false, PetSpecies::Cat),
    ] {
        rainbow_kitty_glow(&mut p, style);
        assert_eq!(p.pet_style_named, named, "{style:?}: names the pet?");
        assert_eq!(p.companion.species(), species, "{style:?}: which animal");
    }
    for (style, tall) in [
        ("rainbow kitty", true),
        ("rainbow kitty pet", true),
        ("rainbow kitty flying", true),
        ("rainbow kitty tall", true),
        ("rainbow kitty underline", false),
    ] {
        rainbow_kitty_glow(&mut p, style);
        assert_eq!(p.glow_cfg.ribbon_tall, tall, "{style:?}: tall ribbon?");
    }
}

fn rainbow_kitty_glow(p: &mut EffectsPipeline, style: &str) {
    p.set_cursor_glow(
        true,
        style,
        crate::cursor_fx::TrailColor::Automatic,
        None,
        400,
        24,
        0.9,
        0.8,
        true,
        0x0050_FA7B,
    );
}

fn pet_pipeline(seed: u64) -> EffectsPipeline {
    let mut p = EffectsPipeline::new();
    rainbow_kitty_glow(&mut p, "rainbow kitty");
    p.set_cursor_pet(true, seed);
    p
}

fn glass(rows: u16, cols: u16) -> (Terminal, RenderInput) {
    let mut term = Terminal::new(rows, cols);
    configure_glass_theme(&mut term);
    let input = term.cell_frame(usize::from(rows), usize::from(cols));
    (term, input)
}

fn configure_glass_theme(term: &mut Terminal) {
    let theme = aterm_render::Theme::default();
    let rgb = |color: u32| {
        aterm_core::terminal::Rgb::new(
            ((color >> 16) & 0xff) as u8,
            ((color >> 8) & 0xff) as u8,
            (color & 0xff) as u8,
        )
    };
    term.set_default_foreground(rgb(theme.fg));
    term.set_default_background(rgb(theme.bg));
    term.set_default_selection_background(Some(rgb(theme.selection)));
}

fn type_line(
    p: &mut EffectsPipeline,
    term: &mut Terminal,
    input: &mut RenderInput,
    text: &[u8],
    dt_ms: f64,
) -> u64 {
    let mut fp = 0;
    for b in text {
        fp = commit_ascii(p, term, input, &[*b], dt_ms);
    }
    fp
}

fn idle(
    p: &mut EffectsPipeline,
    term: &mut Terminal,
    input: &mut RenderInput,
    frames: usize,
    dt_ms: f64,
) -> u64 {
    let (rows, cols) = (input.rows, input.cols);
    let mut fp = 0;
    for _ in 0..frames {
        p.advance(dt_ms);
        term.cell_frame_into(input, rows, cols);
        fp = p.apply(term, input, 10, 19);
    }
    fp
}

const CELL_H: u16 = 19;

const fn pet_natural_size(cell_h: u16) -> (u16, u16) {
    let nat_h = (crate::kitty_pet::ART_ROWS * cell_h as f32).round();
    let nat_w = (nat_h * crate::kitty_pet::ART_ASPECT).round();
    (nat_w as u16, nat_h as u16)
}

fn pet_body_sprite(free: &[FreeSprite], cell_h: u16) -> Option<FreeSprite> {
    let nat = pet_natural_size(cell_h);
    free.iter().find(|s| (s.art.w(), s.art.h()) == nat).cloned()
}

fn run_command_block(
    p: &mut EffectsPipeline,
    term: &mut Terminal,
    input: &mut RenderInput,
    exit: i32,
) -> u64 {
    term.process(b"\r\n\x1b]133;C\x07out\r\n");
    term.process(format!("\x1b]133;D;{exit}\x07").as_bytes());
    term.process(b"\x1b]133;A\x07$ \x1b]133;B\x07");
    idle(p, term, input, 1, 33.0)
}

#[test]
fn a_failed_command_hushes_the_caret_jump_fanfare_while_the_pet_grieves() {
    for exit in [0, 1] {
        let mut p = pet_pipeline(7);
        let (mut term, mut input) = glass(24, 80);
        term.process(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        idle(&mut p, &mut term, &mut input, 1, 33.0);
        materialize_pet(&mut p, &mut term, &mut input);
        type_line(&mut p, &mut term, &mut input, b"kkkkkkkk", 60.0);
        assert!(
            p.cursor.glow.typing_momentum(p.now()) > 0.0,
            "fixture: the run earned momentum before the command ran"
        );

        run_command_block(&mut p, &mut term, &mut input, exit);
        assert_eq!(
            p.companion.grieving(),
            exit != 0,
            "exit {exit}: the droop is owed exactly on a failure"
        );
        term.process(b"\r\n$ ");
        let (momentum, v2_disp) = {
            idle(&mut p, &mut term, &mut input, 1, 33.0);
            (
                p.cursor.glow.typing_momentum(p.now()),
                p.cursor.glow.momentum_display(),
            )
        };
        if exit == 0 {
            assert!(
                momentum > 0.0 && v2_disp > 0.0,
                "control: a success keeps the earned momentum ({momentum}) and v2 spine ({v2_disp}) through the jump"
            );
        } else {
            assert!(p.companion.grieving(), "the droop outlasts one frame");
            assert_eq!(
                (momentum, v2_disp),
                (0.0, 0.0),
                "a grieving pet hushes the fanfare: neither v1 momentum nor v2 spine fuels the jump meteor"
            );
            for _ in 0..6 {
                idle(&mut p, &mut term, &mut input, 1, 33.0);
                if !p.companion.grieving() {
                    break;
                }
                assert_eq!(p.cursor.glow.typing_momentum(p.now()), 0.0);
                assert_eq!(p.cursor.glow.momentum_display(), 0.0);
            }
        }
    }
}

#[test]
fn the_pets_contrast_cursor_is_the_stamped_cursor_colour_or_the_terminals() {
    assert_eq!(pet_contrast_cursor(0x0011_2233, 0x00AA_BBCC), 0x0011_2233);
    assert_eq!(
        pet_contrast_cursor(0xFF11_2233, 0x00AA_BBCC),
        0x0011_2233,
        "a stamped colour's high byte is not ink"
    );
    assert_eq!(
        pet_contrast_cursor(aterm_core::render::COLOR_UNSET, 0x00AA_BBCC),
        0x00AA_BBCC,
        "the sentinel falls back to the resolved colour it was handed"
    );

    let (mut term, _input) = glass(24, 80);
    let fg = term.default_foreground();
    assert_eq!(
        terminal_cursor_color(&term),
        aterm_render::rgb_to_u32([fg.r, fg.g, fg.b]),
        "no OSC 12: the terminal's default foreground, as native resolves it"
    );
    term.process(b"\x1b]12;#00ff00\x07");
    assert_eq!(
        terminal_cursor_color(&term),
        0x0000_FF00,
        "a live OSC 12 is the cursor colour the pet contrasts against"
    );
}

#[test]
fn only_the_owner_fence_rebaselines_the_command_probe_not_a_hide_or_a_resize() {
    let mut p = pet_pipeline(7);
    let (mut term, mut input) = glass(24, 80);
    term.process(b"\x1b]133;A\x07$ \x1b]133;B\x07");
    idle(&mut p, &mut term, &mut input, 1, 33.0);
    p.set_effects_visibility("hidden");
    p.set_effects_visibility("focused");
    run_command_block(&mut p, &mut term, &mut input, 1);
    assert!(
        p.companion.grieving(),
        "a presentability edge keeps the latch: the failure is felt"
    );

    let mut p = pet_pipeline(7);
    let (mut term, mut input) = glass(24, 80);
    term.process(b"\x1b]133;A\x07$ \x1b]133;B\x07");
    idle(&mut p, &mut term, &mut input, 1, 33.0);
    term.process(b"\r\n\x1b]133;C\x07out\r\n\x1b]133;D;1\x07\x1b]133;A\x07$ \x1b]133;B\x07");
    p.advance(33.0);
    term.cell_frame_into(&mut input, 24, 80);
    p.apply(&mut term, &mut input, 12, 24);
    assert!(
        p.companion.grieving(),
        "a resize retires the pet's SURFACE, never its command stream: \
         the failure that just landed is still the pet's news"
    );

    let mut p = pet_pipeline(7);
    let (mut term, mut input) = glass(24, 80);
    term.process(b"\x1b]133;A\x07$ \x1b]133;B\x07");
    idle(&mut p, &mut term, &mut input, 1, 33.0);
    term.process(b"\r\n\x1b]133;C\x07out\r\n\x1b]133;D;1\x07\x1b]133;A\x07$ \x1b]133;B\x07");
    term.process(b"\x1b[?1049h");
    p.advance(33.0);
    term.cell_frame_into(&mut input, 24, 80);
    p.apply(&mut term, &mut input, 10, 19);
    assert!(
        !p.companion.grieving(),
        "the owner fence re-baselines the probe: the old screen's \
         completion is not the new owner's news"
    );
}

fn materialize_pet(p: &mut EffectsPipeline, term: &mut Terminal, input: &mut RenderInput) {
    for _ in 0..40 {
        commit_ascii(p, term, input, b"k", 33.0);
        if p.cursor_pet_alpha() > 0 {
            break;
        }
    }
    assert!(
        p.cursor_pet_alpha() > 0,
        "fixture: the resident reached the glass"
    );
    assert!(
        pet_body_sprite(&free_sprites(input), CELL_H).is_some(),
        "fixture: its body rides free_sprites"
    );
    assert!(p.companion_hit_rect().is_some(), "fixture: it is clickable");
}

#[test]
fn cursor_pet_off_leaves_every_channel_and_fingerprint_untouched() {
    let mut plain = EffectsPipeline::new();
    rainbow_kitty_glow(&mut plain, "rainbow kitty");
    let mut off = EffectsPipeline::new();
    rainbow_kitty_glow(&mut off, "rainbow kitty");
    off.set_cursor_pet(false, 7);
    assert!(!off.cursor_pet_enabled());
    assert!(!off.note_pointer_px(f32::NAN, 1.0), "poison is dropped");
    assert!(
        off.note_pointer_px(30.0, 40.0),
        "the pointer is a host fact, pet or no pet"
    );
    let (mut ta, mut ia) = glass(8, 40);
    let (mut tb, mut ib) = glass(8, 40);
    for step in 0..40u32 {
        let fa = commit_ascii(&mut plain, &mut ta, &mut ia, b"k", 33.0);
        let fb = commit_ascii(&mut off, &mut tb, &mut ib, b"k", 33.0);
        assert_eq!(
            fa, fb,
            "step {step}: the fold never learns of a pet that is off"
        );
        assert!(free_sprites(&ia).is_empty() && free_sprites(&ib).is_empty());
        assert_eq!(off.cursor_pet_alpha(), 0);
        assert_eq!(off.companion_hit_rect(), None);
        assert_eq!(off.press_px(30.0, 40.0), PressOutcome::Pass);
        assert_eq!(plain.is_active(), off.is_active());
    }
    assert!(
        plain.is_active(),
        "non-vacuous: the trail is live while the pet is off"
    );

    let mut none = EffectsPipeline::new();
    none.set_cursor_pet(true, 7);
    let (mut term, mut input) = glass(8, 40);
    assert_eq!(none.apply(&mut term, &mut input, 10, 19), 0);
    assert!(free_sprites(&input).is_empty());
    assert!(!none.is_active() && !none.enabled_any());
    assert_eq!(none.cursor_pet_alpha(), 0);
    assert!(matches!(none.wake(), Wake::Idle));
    assert!(
        none.cursor_pet_enabled(),
        "the opt-in is remembered for when an owner arrives"
    );
}

#[test]
fn cursor_pet_walks_beside_the_caret_then_sleeps_and_releases_cadence() {
    let mut p = pet_pipeline(7);
    let (mut term, mut input) = glass(8, 40);
    let mut on_glass_at = None;
    for frame in 1..=12u32 {
        commit_ascii(&mut p, &mut term, &mut input, b"k", 33.0);
        if pet_body_sprite(&free_sprites(&input), CELL_H).is_some() {
            on_glass_at = Some(frame);
            break;
        }
    }
    let frame = on_glass_at.expect("the resident fades in beside the first caret");
    assert!(
        f64::from(frame) * 33.0 <= 400.0,
        "…within its 0.30 s fade-in"
    );
    assert!(p.cursor_pet_alpha() > 0 && p.companion_hit_rect().is_some());
    assert!(
        p.is_active() && p.companion.needs_frames(),
        "a fading-in cat claims the frame lane"
    );
    assert!(matches!(p.wake(), Wake::Frames));
    let x_before = pet_body_sprite(&free_sprites(&input), CELL_H)
        .expect("on glass")
        .x;

    type_line(&mut p, &mut term, &mut input, b"itty walks with me", 33.0);
    idle(&mut p, &mut term, &mut input, 45, 33.0);
    let x_after = pet_body_sprite(&free_sprites(&input), CELL_H)
        .expect("still on glass")
        .x;
    assert!(
        x_after > x_before,
        "the pet followed the caret to the right"
    );

    let fp = idle(&mut p, &mut term, &mut input, 350, 100.0);
    assert!(
        !p.companion.needs_frames(),
        "the resident released the lane"
    );
    assert!(!p.is_active(), "…and nothing else holds it");
    assert_eq!(p.next_deadline_ms(), None);
    assert!(matches!(p.wake(), Wake::Idle));
    let body =
        pet_body_sprite(&free_sprites(&input), CELL_H).expect("a sleeping cat is still drawn");
    assert_eq!(body.alpha, 255, "opaque, at rest");
    assert_eq!(p.cursor_pet_alpha(), 255);
    let sprites = free_sprites(&input).clone();
    assert_ne!(fp, 0, "a drawn resident folds a non-zero key");
    for _ in 0..3 {
        assert_eq!(
            p.apply(&mut term, &mut input, 10, 19),
            fp,
            "no advance ⇒ the same fold"
        );
        assert_eq!(free_sprites(&input), sprites, "byte-stable sprites");
    }
}

#[test]
fn cursor_pet_retires_when_the_style_stops_naming_it() {
    for other in ["lumen", "rainbow kitty flying"] {
        let mut p = pet_pipeline(7);
        let (mut term, mut input) = glass(8, 40);
        materialize_pet(&mut p, &mut term, &mut input);
        let worn = p.companion.brain().worn_pair().expect("dressed");
        rainbow_kitty_glow(&mut p, other);
        idle(&mut p, &mut term, &mut input, 1, 33.0);
        assert!(
            pet_body_sprite(&free_sprites(&input), CELL_H).is_none(),
            "{other}: nothing drawn on the very next frame"
        );
        assert_eq!(p.cursor_pet_alpha(), 0);
        assert_eq!(p.companion_hit_rect(), None);
        assert!(
            !p.companion.brain().is_active(),
            "{other}: retired (a switch), not fading (an exit)"
        );
        assert!(!p.companion.needs_frames(), "{other}: no owed frames");
        assert!(
            p.cursor_pet_enabled(),
            "the opt-in survives; only the owner went"
        );
        rainbow_kitty_glow(&mut p, "rainbow kitty");
        materialize_pet(&mut p, &mut term, &mut input);
        assert_eq!(
            p.companion.brain().worn_pair(),
            Some(worn),
            "{other}: back wearing the seed's coat"
        );
    }

    let mut p = pet_pipeline(7);
    let (mut term, mut input) = glass(8, 40);
    materialize_pet(&mut p, &mut term, &mut input);
    p.set_cursor_glow(
        false,
        "rainbow kitty",
        crate::cursor_fx::TrailColor::Automatic,
        None,
        400,
        24,
        0.9,
        0.8,
        true,
        0x0050_FA7B,
    );
    idle(&mut p, &mut term, &mut input, 2, 33.0);
    assert!(free_sprites(&input).is_empty());
    assert!(!p.companion.brain().is_active() && !p.companion.needs_frames());
    assert!(
        !p.is_active() && !p.enabled_any(),
        "the trail master off is all-off again"
    );
}

#[test]
fn cursor_pet_hides_in_history_and_when_unfocused_but_keeps_its_identity() {
    let mut p = pet_pipeline(7);
    let mut term = TerminalBuilder::new()
        .size(8, 40)
        .ring_buffer_size(64)
        .build();
    configure_glass_theme(&mut term);
    let mut input = term.cell_frame(8, 40);
    materialize_pet(&mut p, &mut term, &mut input);
    let worn = p.companion.brain().worn_pair().expect("dressed");

    term.process(&b"history\r\n".repeat(20));
    term.scroll_display(1);
    term.cell_frame_into(&mut input, 8, 40);
    assert!(input.display_offset > 0, "fixture: scrolled into history");
    let mut released = false;
    for _ in 0..40 {
        idle(&mut p, &mut term, &mut input, 1, 100.0);
        assert!(free_sprites(&input).is_empty());
        if !p.companion.needs_frames() {
            released = true;
            break;
        }
    }
    assert!(
        released && !p.companion.brain().is_active(),
        "the hidden brain faded to zero and released the lane"
    );
    assert_eq!(
        p.companion.brain().worn_pair(),
        Some(worn),
        "identity survives history"
    );
    term.scroll_to_bottom();
    materialize_pet(&mut p, &mut term, &mut input);
    assert_eq!(
        p.companion.brain().worn_pair(),
        Some(worn),
        "back in the same coat"
    );

    p.set_focused(false);
    idle(&mut p, &mut term, &mut input, 1, 33.0);
    assert_eq!(p.cursor_pet_alpha(), 0);
    assert!(free_sprites(&input).is_empty());
    assert!(
        !p.companion.brain().is_active() && !p.companion.needs_frames(),
        "an unfocused surface retires the body at once and owes no frames"
    );
    assert_eq!(p.companion.brain().worn_pair(), Some(worn));
    p.set_focused(true);
    materialize_pet(&mut p, &mut term, &mut input);
    assert_eq!(
        p.companion.brain().worn_pair(),
        Some(worn),
        "refocus: the same coat"
    );

    p.set_effects_visibility("hidden");
    assert!(!p.companion.brain().is_active() && !p.companion.needs_frames());
    assert_eq!(p.companion_hit_rect(), None);
    p.set_effects_visibility("focused");
    materialize_pet(&mut p, &mut term, &mut input);
    assert_eq!(
        p.companion.brain().worn_pair(),
        Some(worn),
        "a fresh sighting, same cat"
    );
}

#[test]
fn cursor_pet_body_rect_is_handed_to_the_word_engine() {
    let drive = |with_pet: bool| -> (bool, bool) {
        let mut p = EffectsPipeline::new();
        rainbow_kitty_glow(&mut p, "rainbow kitty");
        p.set_sparkle_enabled(true);
        p.set_sparkle_feline("cat", true, true, false);
        if with_pet {
            p.set_cursor_pet(true, 7);
        }
        let (mut term, mut input) = glass(8, 40);
        term.process(b"\r\n\r\na nice ");
        type_line(&mut p, &mut term, &mut input, b"kitty", 33.0);
        let nat = pet_natural_size(CELL_H);
        let (mut saw_pet, mut saw_other_animal, mut stacked) = (false, false, false);
        for _ in 0..90 {
            idle(&mut p, &mut term, &mut input, 1, 100.0);
            let body = pet_body_sprite(&free_sprites(&input), CELL_H);
            saw_pet |= body.is_some();
            for s in &free_sprites(&input) {
                // An animal: at least a cell tall (motes are smaller)
                // and not the pet's own tile.
                if s.art.h() < CELL_H || (s.art.w(), s.art.h()) == nat {
                    continue;
                }
                saw_other_animal = true;
                if let Some(b) = &body {
                    let ox = (s.x + i32::from(s.w)).min(b.x + i32::from(b.w)) - s.x.max(b.x);
                    let oy = (s.y + i32::from(s.h)).min(b.y + i32::from(b.h)) - s.y.max(b.y);
                    stacked |= ox > 0 && oy > 0 && ox * oy * 3 >= i32::from(s.w) * i32::from(s.h);
                }
            }
        }
        assert_eq!(saw_pet, with_pet, "the pet is drawn exactly when opted in");
        (saw_other_animal, stacked)
    };
    let (other_animal, stacked) = drive(true);
    assert!(
        !other_animal,
        "the word the pet answers grows no ambient twin"
    );
    assert!(!stacked, "nothing stacks on the pet's body");
    let (control, _) = drive(false);
    assert!(control, "control: with no companion the word-cat peeks");
}

#[test]
fn web_pipeline_reads_off_and_aliases_like_the_native_host() {
    let mut p = EffectsPipeline::new();
    rainbow_kitty_glow(&mut p, "rainbow kitty");
    assert!(p.glow_cfg.enabled);
    assert_eq!(p.glow_cfg.style, GlowStyle::RainbowKitty);
    assert!(p.pet_style_named, "the bare name names the pet");
    rainbow_kitty_glow(&mut p, "nyan rainbow");
    assert_eq!(p.glow_cfg.style, GlowStyle::RainbowKitty);
    assert!(!p.pet_style_named, "nyan rainbow keeps the flying head");
    rainbow_kitty_glow(&mut p, "classic mono");
    assert_eq!(p.glow_cfg.style, GlowStyle::Classic);
    assert!(
        p.glow_cfg.classic_mono,
        "the old spelling still draws the mono face"
    );
    rainbow_kitty_glow(&mut p, "off");
    assert!(!p.glow_cfg.enabled, "`off` disables the trail");
    assert!(!p.pet_style_named);
    rainbow_kitty_glow(&mut p, "lumen");
    assert!(p.glow_cfg.enabled && p.glow_cfg.style == GlowStyle::Lumen);
    rainbow_kitty_glow(&mut p, "plasma");
    assert!(p.glow_cfg.enabled && p.glow_cfg.style == GlowStyle::Lumen);
}

#[test]
fn pointer_and_petting_latch_only_and_need_a_tick() {
    let mut p = pet_pipeline(7);
    let (mut term, mut input) = glass(8, 40);
    materialize_pet(&mut p, &mut term, &mut input);
    idle(&mut p, &mut term, &mut input, 350, 100.0);
    assert!(
        !p.companion.needs_frames(),
        "fixture: asleep, lane released"
    );

    assert!(p.note_pointer_px(100.0, 50.0));
    assert!(
        !p.note_pointer_px(100.0, 50.0),
        "a still pointer costs no render"
    );
    assert!(!p.note_pointer_px(f32::NAN, 50.0), "poison is dropped");
    assert!(p.note_pointer_leave());
    assert!(!p.note_pointer_leave());

    let rect @ (x0, x1, y0, y1) = p.companion_hit_rect().expect("a sleeping cat is clickable");
    assert_eq!(
        p.press_px((x1 + 40) as f32, (y1 + 40) as f32),
        PressOutcome::Pass,
        "outside the body the host keeps routing"
    );
    assert_eq!(p.companion.brain().pending_pets(), 0);
    let sprites = free_sprites(&input).clone();
    let alpha = p.cursor_pet_alpha();
    assert_eq!(
        p.press_px((x0 + x1) as f32 / 2.0, (y0 + y1) as f32 / 2.0),
        PressOutcome::Pet,
        "inside the body the press is consumed"
    );
    assert_eq!(p.companion.brain().pending_pets(), 1, "one stroke queued");
    assert!(
        p.companion.needs_frames() && p.is_active(),
        "the latch asks for the frame that consumes it"
    );
    assert!(matches!(p.wake(), Wake::Frames));
    assert_eq!(p.cursor_pet_alpha(), alpha, "nothing moved without a tick");
    assert_eq!(p.companion_hit_rect(), Some(rect));
    assert_eq!(free_sprites(&input), sprites);
    let mut consumed_after = None;
    for frame in 1..=180u32 {
        idle(&mut p, &mut term, &mut input, 1, 33.0);
        if p.companion.brain().pending_pets() == 0 {
            consumed_after = Some(frame);
            break;
        }
    }
    assert!(
        consumed_after.is_some_and(|f| f >= 1),
        "the stroke is consumed by a tick, on the ground"
    );
}

#[test]
fn a_typed_trick_latches_through_the_pipeline_and_needs_ticks() {
    use aterm_lexicon::Trick;

    let mut p = pet_pipeline(7);
    let (mut term, mut input) = glass(8, 40);
    materialize_pet(&mut p, &mut term, &mut input);
    idle(&mut p, &mut term, &mut input, 60, 50.0);
    assert_eq!(p.companion.brain().pending_trick(), None);

    let now = p.now();
    p.companion_mut().note_trick(now, Trick::Roll, false);
    assert_eq!(p.companion.brain().pending_trick(), Some(Trick::Roll));
    assert!(
        p.companion.needs_frames() && p.is_active(),
        "the latch asks for the frames that perform it"
    );
    p.companion_mut().revoke_trick();
    assert_eq!(p.companion.brain().pending_trick(), None);
    p.companion_mut().revoke_trick(); // idempotent: the listener revokes with nothing pending

    let sprites = free_sprites(&input).clone();
    let now = p.now();
    p.companion_mut().note_trick(now, Trick::Roll, true);
    assert_eq!(p.companion.brain().pending_trick(), Some(Trick::Roll));
    assert!(matches!(p.wake(), Wake::Frames));
    assert_eq!(free_sprites(&input), sprites, "a note draws nothing");
    let mut rolled = false;
    for _ in 0..120 {
        idle(&mut p, &mut term, &mut input, 1, 33.0);
        rolled |= p.companion.brain().pose_name().starts_with("pet_roll");
    }
    assert_eq!(
        p.companion.brain().pending_trick(),
        None,
        "consumed by ticks"
    );
    assert!(rolled, "and performed: the authored roll reached the glass");

    for pet_talk in [false, true] {
        let mut p = pet_pipeline(7);
        let (mut term, mut input) = glass(24, 80);
        term.process(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        idle(&mut p, &mut term, &mut input, 1, 33.0);
        materialize_pet(&mut p, &mut term, &mut input);
        if pet_talk {
            let now = p.now();
            p.companion_mut().note_trick_submit(now);
        }
        run_command_block(&mut p, &mut term, &mut input, 127);
        assert_eq!(
            p.companion.grieving(),
            !pet_talk,
            "pet talk {pet_talk}: only an ordinary failure is grieved"
        );
    }
}

#[test]
fn pointer_under_host_rows_is_read_where_the_frame_draws_the_pet() {
    use crate::kitty_pet::PetAction;
    use aterm_core::render::HostRowPixels;
    use aterm_core::terminal::RenderCell;

    // The same resident on the same frames with no pointer: what its
    // content comes to without the stroke (an arrival's content tires).
    let mut unstroked = pet_pipeline(7);
    let (mut uterm, mut uinput) = glass(8, 40);
    materialize_pet(&mut unstroked, &mut uterm, &mut uinput);
    idle(&mut unstroked, &mut uterm, &mut uinput, 60, 50.0);
    unstroked.set_host_rows_px(CELL_H);
    idle(&mut unstroked, &mut uterm, &mut uinput, 21, 50.0);
    let mut p = pet_pipeline(7);
    let (mut term, mut input) = glass(8, 40);
    materialize_pet(&mut p, &mut term, &mut input);
    idle(&mut p, &mut term, &mut input, 60, 50.0);
    let emitted = pet_body_sprite(&free_sprites(&input), CELL_H).expect("resident on glass");
    p.set_host_rows_px(CELL_H);
    idle(&mut p, &mut term, &mut input, 1, 50.0);
    assert_eq!(
        pet_body_sprite(&free_sprites(&input), CELL_H).map(|s| s.y),
        Some(emitted.y),
        "the emission does not move: the compose moves it"
    );
    let band_row = vec![RenderCell::default(); input.cols];
    let mut drawn = input.clone();
    drawn.prepend_host_rows(
        1,
        std::iter::once(band_row.as_slice()),
        usize::from(CELL_H),
        0,
        &mut Vec::new(),
        HostRowPixels::Translate,
    );
    let body = pet_body_sprite(&free_sprites(&drawn), CELL_H).expect("the body is drawn");
    assert_eq!(body.y, emitted.y + i32::from(CELL_H), "drawn one row lower");
    let (x0, w) = (body.x as f32, f32::from(body.w));
    let y = body.y as f32 + f32::from(body.h) / 2.0;
    let (rx0, rx1, ry0, ry1) = p.companion_hit_rect().expect("clickable");
    assert_eq!(
        (rx0, rx1, ry0, ry1),
        (
            body.x,
            body.x + i32::from(body.w),
            body.y,
            body.y + i32::from(body.h)
        ),
        "the hit rect is the drawn body, in frame px"
    );

    let mut answered = false;
    for step in 1..=20 {
        let x = x0 + w * (0.20 + 0.60 * step as f32 / 20.0);
        assert!(p.note_pointer_px(x, y));
        idle(&mut p, &mut term, &mut input, 1, 50.0);
        answered |= p.companion.brain().action() == PetAction::Purr;
    }
    assert!(
        answered,
        "a stroke across the drawn body must reach the brain's stroke detector"
    );
    assert!(
        p.companion.brain().content() > unstroked.companion.brain().content() + 0.05,
        "the reaction was earned by pointer travel over the drawn body"
    );
    let cx = x0 + w / 2.0;
    assert_eq!(
        p.press_px(cx, body.y as f32 - f32::from(CELL_H) / 2.0),
        PressOutcome::Pass,
        "half a row above the drawn body the host keeps routing"
    );
    assert_eq!(
        p.press_px(cx, y),
        PressOutcome::Pet,
        "a press on the drawn body pets"
    );
}

#[test]
fn reduced_motion_pins_the_web_pet_at_its_station() {
    let jump = |reduced: bool| {
        let mut p = pet_pipeline(7);
        if reduced {
            p.set_reduced_motion(true);
        }
        let (mut term, mut input) = glass(12, 60);
        materialize_pet(&mut p, &mut term, &mut input);
        idle(&mut p, &mut term, &mut input, 60, 33.0);
        let before = pet_body_sprite(&free_sprites(&input), CELL_H).expect("drawn");
        term.process(b"\x1b[10;40H");
        idle(&mut p, &mut term, &mut input, 1, 33.0);
        let first = pet_body_sprite(&free_sprites(&input), CELL_H).expect("drawn");
        idle(&mut p, &mut term, &mut input, 60, 33.0);
        let later = pet_body_sprite(&free_sprites(&input), CELL_H).expect("drawn");
        (before, first, later)
    };
    let (before, first, later) = jump(true);
    assert_ne!(
        (first.x, first.y),
        (before.x, before.y),
        "the station moved"
    );
    assert_eq!(
        (first.x, first.y),
        (later.x, later.y),
        "…in ONE frame: pinned, no walk"
    );
    assert_eq!(
        first.y + i32::from(first.h),
        10 * i32::from(CELL_H),
        "feet on the new baseline: no arc"
    );
    let (_, first, later) = jump(false);
    assert_ne!(
        (first.x, first.y),
        (later.x, later.y),
        "control: full motion is still travelling one frame after the jump"
    );

    let mut p = EffectsPipeline::new();
    p.set_sparkle_reduced_motion(true);
    assert!(p.deco_cfg.reduced_motion);
    p.set_matrix_rain_reduced_motion(false);
    assert!(!p.deco_cfg.reduced_motion);
}

#[test]
fn next_deadline_ms_is_none_while_the_pet_moves_and_exact_while_it_breathes() {
    let mut p = pet_pipeline(7);
    enable_classic_rain(&mut p);
    assert_eq!(
        p.next_deadline_ms(),
        Some(83.0),
        "rain alone: an exact wake"
    );
    assert!(!p.companion.needs_frames());
    let (mut term, mut input) = glass(8, 40);
    materialize_pet(&mut p, &mut term, &mut input);
    assert!(
        p.companion.needs_frames(),
        "fixture: the resident is still moving"
    );
    assert_eq!(
        p.next_deadline_ms(),
        None,
        "a moving pet needs display-rAF cadence"
    );
    assert!(p.is_active());
    assert!(matches!(p.wake(), Wake::Frames));

    let mut settled = false;
    for _ in 0..200 {
        idle(&mut p, &mut term, &mut input, 1, 50.0);
        if !p.companion.needs_frames()
            && !p.cursor.trail.is_active()
            && !p.cursor.glow.is_active()
            && !p.cursor.bodies_active()
            && !p.decos.is_active(p.now())
        {
            settled = true;
            break;
        }
    }
    assert!(
        settled,
        "fixture: the resident and every other frame lane settle within 10 s"
    );
    assert!(
        p.companion.next_change_deadline(p.now()).is_some(),
        "settled: the resident names its next visible step"
    );
    let ms = p
        .next_deadline_ms()
        .expect("settled: an exact wake, not the frame train");
    assert!(
        ms > 0.0 && ms <= 83.0,
        "the earliest of the pet's offer and rain's wake: {ms} ms"
    );
    assert!(p.is_active(), "a breathing pet keeps the scheduler armed");
    assert!(matches!(p.wake(), Wake::At(_)));

    idle(&mut p, &mut term, &mut input, 350, 100.0);
    assert!(
        !p.companion.needs_frames(),
        "asleep: the pet forces nothing"
    );
    assert!(
        p.companion.next_change_deadline(p.now()).is_none(),
        "deep sleep: nothing left to offer"
    );
    assert!(
        p.next_deadline_ms().is_some() || !p.is_active(),
        "what remains is rain's own verdict — an exact wake or idle"
    );
    assert!(!matches!(p.wake(), Wake::Frames));
}

#[test]
fn pipeline_rows_metrics_and_chrome_changes_reset_seeded_geometry() {
    #[derive(Clone, Copy, PartialEq)]
    enum Prep {
        Subject,
        FreshFamily,
        KeepBodies,
    }
    type Frames = Vec<(Vec<GlowQuad>, Vec<TrailCell>)>;
    let run = |style: &str, case: &str, prep: Prep| -> (EffectsPipeline, Frames) {
        let label = format!("{style}/{case}");
        let mut term = Terminal::new(5, 16);
        let mut pipeline = EffectsPipeline::new();
        pipeline.set_cursor_glow(
            true,
            style,
            crate::cursor_fx::TrailColor::Automatic,
            None,
            400,
            24,
            0.9,
            0.8,
            true,
            0x0050_FA7B,
        );
        let (mut input, _) = seed_pipeline_trail(&mut pipeline, &mut term, 2);
        assert!(
            pipeline.cursor.trail.is_active() && pipeline.cursor.glow.is_active(),
            "{label}: fixture owns retained geometry"
        );
        let now = pipeline.now();
        match prep {
            Prep::Subject => {}
            Prep::FreshFamily => {
                let old = std::mem::take(&mut pipeline.cursor);
                pipeline.cursor = crate::cursor_fx::CursorFx {
                    glow: old.glow,
                    momentum: old.momentum,
                    cadence: old.cadence,
                    ..Default::default()
                };
                pipeline.cursor.glow.curtain(now);
            }
            Prep::KeepBodies => {
                pipeline.cursor.glow.curtain(now);
                pipeline.cursor.trail.reset();
            }
        }
        let (cell_w, cell_h) = match case {
            "rows" => {
                term.resize(6, 16);
                input = term.cell_frame(6, 16);
                (10, 19)
            }
            "cell-metrics" => (11, 20),
            "chrome-origin" => {
                pipeline.set_chrome(3, 7);
                (10, 19)
            }
            _ => unreachable!(),
        };
        if prep == Prep::KeepBodies {
            pipeline.cursor_coordinate_space = Some(CursorCoordinateSpace {
                rows: input.rows,
                cols: input.cols,
                cell_w,
                cell_h,
                chrome_pad: pipeline.chrome_pad,
                chrome_head: pipeline.chrome_head,
            });
        }
        let mut frames = Frames::new();
        for frame in 0..6 {
            if frame > 0 {
                pipeline.advance(16.0);
                let (rows, cols) = (input.rows, input.cols);
                term.cell_frame_into(&mut input, rows, cols);
            }
            pipeline.apply(&mut term, &mut input, cell_w, cell_h);
            frames.push((input.cursor_glow_add.clone(), input.cursor_trail.clone()));
        }
        (pipeline, frames)
    };
    let styles = [
        "lumen",
        "phaser",
        "rainbow kitty",
        "sparkle",
        "fire",
        "laser",
        "beam",
        "water",
        "comet",
        "classic",
    ];
    for case in ["rows", "cell-metrics", "chrome-origin"] {
        for style in styles {
            let (subject, drawn) = run(style, case, Prep::Subject);
            let (_, fresh) = run(style, case, Prep::FreshFamily);
            assert!(
                !subject.cursor.trail.is_active()
                    && (style == "rainbow kitty" || !subject.cursor.glow.is_active()),
                "{style}/{case}: changed coordinate space retained a seeded survivor"
            );
            assert!(
                !fresh[0].0.is_empty(),
                "{style}/{case}: fixture — the fresh family draws the caret's body"
            );
            assert!(
                drawn == fresh,
                "{style}/{case}: the frames after the space change are not a fresh family's"
            );
            if matches!(style, "water" | "comet") {
                let (_, kept) = run(style, case, Prep::KeepBodies);
                assert!(
                    kept != fresh,
                    "{style}/{case}: control — a body that kept its geometry must draw \
                     differently"
                );
            }
        }
        validate_pipeline_layout_prepare(case);
    }
}

#[test]
fn pipeline_translates_real_zero_and_saturated_scrollback_exactly() {
    for cap in [0usize, 1] {
        let mut term = TerminalBuilder::new()
            .size(5, 16)
            .ring_buffer_size(cap)
            .build();
        if cap == 1 {
            term.process(b"\x1b[5;1H\n");
            assert_eq!(term.grid().scrollback_lines(), 1);
        }
        let mut pipeline = EffectsPipeline::new();
        let (mut input, before_trail) = seed_pipeline_trail(&mut pipeline, &mut term, 3);
        let before_signal = term.content_scroll_state();

        term.process(b"\x1b[5;1H\n\n\x1b[2;3H");
        let after_signal = term.content_scroll_state();
        assert_eq!(
            content_scroll_decision(Some(before_signal), after_signal),
            ContentScrollDelta::Translate(2)
        );
        assert_eq!(term.grid().scrollback_lines(), cap);

        term.cell_frame_into(&mut input, 5, 16);
        pipeline.apply(&mut term, &mut input, 10, 19);
        assert!(
            !before_trail.is_empty(),
            "cap={cap}: fixture must be charged"
        );
        assert!(
            pipeline.cursor.trail.is_active(),
            "cap={cap}: an unowned parser generation must not destroy earned light"
        );
        let translated: Vec<usize> = input.cursor_trail.iter().map(|c| c.row).collect();
        let expected: Vec<usize> = before_trail
            .iter()
            .map(|c| c.row.saturating_sub(2))
            .collect();
        assert_eq!(
            translated, expected,
            "cap={cap}: the comet must translate UP by the scrolled rows, cell for cell"
        );

        if cap == 0 {
            validate_cursor_scroll_action("UniformAtCap", 1, ContentScrollDelta::Translate(2));
        }
    }
}

#[test]
fn pipeline_real_alt_uniform_translates_while_ambiguous_motion_invalidates() {
    fn margined_region_scroll(term: &mut Terminal) {
        term.process(b"\x1b[?69h\x1b[2;6s\x1b[2;4r\x1b[3;1H\x1b[S\x1b[r\x1b[?69l\x1b[3;3H");
    }
    fn alt_scroll(term: &mut Terminal) {
        term.process(b"\x1b[5;1H\n\n\x1b[3;3H");
    }
    fn direct_reset(term: &mut Terminal) {
        term.reset();
    }
    fn checkpoint_restore(term: &mut Terminal) {
        let checkpoint = Terminal::new(5, 16).checkpoint();
        term.restore_checkpoint(&checkpoint);
    }

    type ScrollMutationCase = (
        &'static str,
        i64,
        bool,
        ContentScrollDelta,
        fn(&mut Terminal),
    );
    let cases: [ScrollMutationCase; 4] = [
        (
            "RegionInvalidation",
            2,
            false,
            ContentScrollDelta::Invalidate,
            margined_region_scroll,
        ),
        (
            "AltUniform",
            3,
            true,
            ContentScrollDelta::Translate(2),
            alt_scroll,
        ),
        (
            "ResetInvalidation",
            4,
            false,
            ContentScrollDelta::Invalidate,
            direct_reset,
        ),
        (
            "RestoreInvalidation",
            5,
            false,
            ContentScrollDelta::Invalidate,
            checkpoint_restore,
        ),
    ];

    for (action, event, enter_alt, expected, mutate) in cases {
        let mut term = Terminal::new(5, 16);
        if enter_alt {
            term.process(b"\x1b[?1049h");
        }
        let mut pipeline = EffectsPipeline::new();
        if enter_alt {
            pipeline.set_cursor_glow(
                true,
                "lumen",
                crate::cursor_fx::TrailColor::Automatic,
                None,
                400,
                24,
                0.7,
                0.6,
                true,
                0x0050_FA7B,
            );
        }
        let (mut input, before_trail) = seed_pipeline_trail(&mut pipeline, &mut term, 2);
        let before = term.content_scroll_state();
        mutate(&mut term);
        let after = term.content_scroll_state();
        let decision = content_scroll_decision(Some(before), after);
        assert_eq!(decision, expected, "{action}");
        validate_cursor_scroll_action(action, event, decision);

        term.cell_frame_into(&mut input, 5, 16);
        pipeline.apply(&mut term, &mut input, 10, 19);
        if let ContentScrollDelta::Translate(rows) = expected {
            assert!(
                pipeline.cursor.trail.is_active(),
                "{action} destroyed licensed resident light"
            );
            let expected_cells: Vec<TrailCell> = before_trail
                .iter()
                .filter_map(|cell| {
                    cell.row
                        .checked_sub(usize::from(rows))
                        .map(|row| TrailCell { row, ..*cell })
                })
                .collect();
            assert_eq!(
                input.cursor_trail, expected_cells,
                "{action} must translate the licensed comet exactly with alt content"
            );
        } else {
            assert!(
                input.cursor_trail.is_empty(),
                "{action} retained stale cells"
            );
            assert!(
                !pipeline.cursor.trail.is_active(),
                "{action} retained trail state"
            );
        }

        if enter_alt {
            let scripted = pipeline.now();
            pipeline.cursor.glow.note_synthetic_typed(scripted, 1);
            pipeline.cursor.trail.note_synthetic_typed(scripted);
            term.grid_mut().cursor_forward(6);
            pipeline.advance(16.0);
            term.cell_frame_into(&mut input, 5, 16);
            pipeline.apply(&mut term, &mut input, 10, 19);
            assert!(
                !input.cursor_trail.is_empty(),
                "alt context must survive the translated scroll for classic classification"
            );
            assert!(
                !input.cursor_glow_add.is_empty() || !input.glow_halo.is_empty(),
                "alt context must survive the translated scroll for glow classification"
            );
        }
    }
}

#[test]
fn a_band_batch_translates_the_pipeline_engines_instead_of_resetting_them() {
    let mut term = Terminal::new(5, 16);
    let mut pipeline = EffectsPipeline::new();
    let (mut input, before_trail) = seed_pipeline_trail(&mut pipeline, &mut term, 3);
    assert!(!before_trail.is_empty(), "fixture must own live geometry");
    let before = term.content_scroll_state();

    term.process(b"\x1b[2;4r\x1b[4;1H\n\x1b[r\x1b[3;3H");
    let after = term.content_scroll_state();
    let decision = content_scroll_decision(Some(before), after);
    assert_eq!(
        decision,
        ContentScrollDelta::Bands {
            first_seq: before.band_seq,
            count: 1,
        },
        "one region scroll is one explained band move"
    );
    assert_eq!(
        after.invalidation_epoch,
        before.invalidation_epoch + 1,
        "the epoch still bumps: a consumer that cannot translate bands \
         still reads discard"
    );
    assert_eq!(after.band(before.band_seq).delta, -1);
    validate_cursor_scroll_action("RegionBandMove", 6, decision);

    term.cell_frame_into(&mut input, 5, 16);
    pipeline.apply(&mut term, &mut input, 10, 19);
    assert!(
        pipeline.cursor.trail.is_active(),
        "an explained band must not destroy earned light"
    );
    let expected: Vec<TrailCell> = before_trail
        .iter()
        .map(|cell| TrailCell {
            row: cell.row - 1,
            ..*cell
        })
        .collect();
    assert_eq!(
        input.cursor_trail, expected,
        "every cell inside the band moves up exactly one row, cell for cell"
    );
}

#[test]
fn pipeline_scroll_baseline_regressions_and_wide_deltas_fail_closed() {
    let mut pipeline = EffectsPipeline::new();
    let baseline = ContentScrollState {
        uniform_up_rows: 10,
        invalidation_epoch: 4,
        ..Default::default()
    };
    let now = pipeline.now();
    assert_eq!(
        pipeline.sync_scroll(baseline, now),
        ScrollChange::default(),
        "a first observation is silent"
    );
    assert_eq!(
        pipeline.scroll_baseline(),
        Some(baseline),
        "…and it is the baseline"
    );
    assert_eq!(pipeline.sync_scroll(baseline, now), ScrollChange::default());

    for invalid in [
        ContentScrollState {
            uniform_up_rows: 9,
            invalidation_epoch: 4,
            ..Default::default()
        },
        ContentScrollState {
            uniform_up_rows: 10,
            invalidation_epoch: 3,
            ..Default::default()
        },
        ContentScrollState {
            uniform_up_rows: 10 + u64::from(u16::MAX) + 1,
            invalidation_epoch: 4,
            ..Default::default()
        },
    ] {
        assert_eq!(
            content_scroll_decision(Some(baseline), invalid),
            ContentScrollDelta::Invalidate
        );
    }
}

#[test]
fn glow_polarity_tracks_live_osc11_and_decscnm_background() {
    let mut pipeline = EffectsPipeline::new();
    pipeline.set_cursor_glow(
        true,
        "fire",
        crate::cursor_fx::TrailColor::Automatic,
        None,
        400,
        24,
        1.0,
        0.9,
        true,
        0x0050_FA7B,
    );
    let mut term = Terminal::new(2, 8);

    term.process(b"\x1b]11;rgb:ff/ff/ff\x07");
    let mut input = term.cell_frame(2, 8);
    stamp_effective_terminal_background(&term, &mut input);
    pipeline.apply(&mut term, &mut input, 10, 19);
    assert!(
        !pipeline.glow_cfg.dark_theme,
        "a live light OSC 11 ground selects the light-theme treatment"
    );

    term.process(b"\x1b]10;rgb:ee/ee/ee\x07\x1b]11;rgb:01/02/03\x07\x1b[?5h");
    term.cell_frame_into(&mut input, 2, 8);
    stamp_effective_terminal_background(&term, &mut input);
    pipeline.apply(&mut term, &mut input, 10, 19);
    assert!(
        !pipeline.glow_cfg.dark_theme,
        "DECSCNM makes the live foreground the effective light ground"
    );

    term.process(b"\x1b[?5l");
    term.cell_frame_into(&mut input, 2, 8);
    stamp_effective_terminal_background(&term, &mut input);
    pipeline.apply(&mut term, &mut input, 10, 19);
    assert!(
        pipeline.glow_cfg.dark_theme,
        "leaving DECSCNM restores the dark OSC 11 ground"
    );

    term.process(b"\x1b]11;rgb:80/80/80\x07");
    term.cell_frame_into(&mut input, 2, 8);
    stamp_effective_terminal_background(&term, &mut input);
    pipeline.apply(&mut term, &mut input, 10, 19);
    assert_eq!(
        pipeline.glow_cfg.dark_theme,
        aterm_render::theme_is_dark(input.default_bg),
        "mid-gray uses the same polarity as the rendered chrome"
    );

    input.default_bg = aterm_core::render::COLOR_UNSET;
    pipeline.apply(&mut term, &mut input, 10, 19);
    assert!(
        pipeline.glow_cfg.dark_theme,
        "an unstamped legacy host retains the dark-ground fallback"
    );
}

/// The web host latches no light scheme (§15.2 1.11): typed on with the tall rainbow kitty
/// on Default Light's page, every frame's config and frame light stay OFF, no light arm
/// draws and no light words are left. The negative control: a light the config carried
/// would be the frame's light.
#[test]
fn the_web_pipeline_draws_no_light_on_a_light_page() {
    use crate::cursor_glow::{Light, Live};

    let mut pipeline = EffectsPipeline::new();
    pipeline.set_cursor_glow(
        true,
        "rainbow kitty tall",
        crate::cursor_fx::TrailColor::Automatic,
        None,
        650,
        24,
        1.0,
        0.6,
        true,
        0x0050_FA7B,
    );
    let mut term = Terminal::new(4, 24);
    term.process(b"\x1b]10;rgb:2b/2d/33\x07\x1b]11;rgb:f6/f7/f9\x07");
    let mut input = term.cell_frame(4, 24);
    let mut frame = |pipeline: &mut EffectsPipeline, term: &mut Terminal, key: u8| {
        assert!(pipeline.note_committed_char(term, &input, char::from(key)));
        term.process(&[key]);
        pipeline.advance(16.0);
        term.cell_frame_into(&mut input, 4, 24);
        stamp_effective_terminal_background(term, &mut input);
        pipeline.apply(term, &mut input, 10, 19);
        pipeline.now()
    };
    for &key in b"light page" {
        let now = frame(&mut pipeline, &mut term, key);
        assert!(
            !pipeline.glow_cfg.dark_theme,
            "Default Light's page is light"
        );
        assert_eq!(pipeline.glow_cfg.light, Light::OFF);
        assert_eq!(pipeline.frame_light(now), Light::OFF);
        assert!(pipeline.light_arms().is_empty());
        assert!(pipeline.light_words(pipeline.t0).words().is_empty());
    }

    pipeline.glow_cfg.light = Light::at(Live::Glass);
    let now = frame(&mut pipeline, &mut term, b'!');
    assert_eq!(pipeline.frame_light(now), Light::at(Live::Glass));
}

#[test]
fn native_streak_polarity_tracks_live_background() {
    let mut pipeline = EffectsPipeline::for_window();
    let mut term = Terminal::new(2, 8);
    let mut input = term.cell_frame(2, 8);
    let place = crate::pane::PanePlace {
        row_off: 0,
        col_off: 0,
        rows: 2,
        cols: 8,
        win_rows: 2,
        win_cols: 8,
        cell_w: 10,
        cell_h: 20,
    };
    let t0 = std::time::Instant::now();
    let mut knobs = StreakKnobs {
        now: t0,
        intensity: 1.0,
        tail: 9,
        max_streaks: 3,
        idle_secs: 10.0,
    };
    let mut quads = Vec::new();
    let mut frame = |input: &RenderInput, content_seq, knobs| {
        quads.clear();
        pipeline.streak_pane(
            &StreakPane {
                session: 17,
                slot: 0,
                place,
                echo: StreakEcho::default(),
                snapshot: input,
                content_seq,
                sound: false,
            },
            knobs,
            NovaInto::Staged(&mut quads),
        );
        quads.clone()
    };
    assert!(
        frame(&input, 1, knobs).is_empty(),
        "a blank baseline draws nothing"
    );
    term.process(b"hello");
    term.cell_frame_into(&mut input, 2, 8);
    knobs.now = t0 + Duration::from_millis(350);
    for bg in [
        0x0001_0203,
        0x00FF_FFFF,
        aterm_core::render::COLOR_UNSET,
        0x0080_8080,
    ] {
        input.default_bg = bg;
        let drawn = frame(&input, 2, knobs);
        assert!(!drawn.is_empty(), "the actual arrival draws at {bg:#010x}");
        if aterm_render::theme_is_dark(bg) {
            assert!(drawn.iter().all(|q| q.alpha == 0), "dark is additive");
        } else {
            assert!(
                drawn.iter().any(|q| q.alpha > 0),
                "light chrome at {bg:#010x} gets visible source-over streaks"
            );
        }
    }
}

fn stamp_cursor_color(term: &Terminal, input: &mut RenderInput, fallback: u32) {
    input.cursor_color = term.cursor_color().map_or(fallback, |color| {
        aterm_render::rgb_to_u32([color.r, color.g, color.b])
    });
}

#[test]
fn automatic_wake_colors_follow_osc12_resets_and_theme_baselines() {
    let theme_cursor = 0x0011_2233;
    let mut pipeline = EffectsPipeline::new();
    pipeline.set_cursor_glow(
        true,
        "lumen",
        crate::cursor_fx::TrailColor::Automatic,
        None,
        400,
        24,
        1.0,
        0.9,
        true,
        theme_cursor,
    );
    pipeline.set_cursor_trail(true, 400, 24, None, theme_cursor);
    let mut term = Terminal::new(2, 8);
    term.set_default_cursor_color(Some(aterm_core::terminal::Rgb::new(0x11, 0x22, 0x33)));

    term.process(b"\x1b]12;rgb:aa/00/11\x07");
    let mut input = term.cell_frame(2, 8);
    stamp_cursor_color(&term, &mut input, theme_cursor);
    pipeline.apply(&mut term, &mut input, 10, 19);
    assert_eq!(pipeline.glow_cfg.color, 0x00AA_0011);
    assert_eq!(pipeline.glow_cfg.accent, brighten(0x00AA_0011, 1.5));
    assert_eq!(input.cursor_trail_color, 0x00AA_0011);

    term.process(b"\x1b]112\x07");
    term.cell_frame_into(&mut input, 2, 8);
    stamp_cursor_color(&term, &mut input, theme_cursor);
    pipeline.apply(&mut term, &mut input, 10, 19);
    assert_eq!(pipeline.glow_cfg.color, theme_cursor);
    assert_eq!(input.cursor_trail_color, theme_cursor);

    let new_theme_cursor = 0x0044_5566;
    term.set_default_cursor_color(Some(aterm_core::terminal::Rgb::new(0x44, 0x55, 0x66)));
    term.cell_frame_into(&mut input, 2, 8);
    stamp_cursor_color(&term, &mut input, new_theme_cursor);
    pipeline.apply(&mut term, &mut input, 10, 19);
    assert_eq!(
        pipeline.glow_cfg.color, new_theme_cursor,
        "an automatic wake follows a live theme change without reconfiguration"
    );
    assert_eq!(input.cursor_trail_color, new_theme_cursor);
}

#[test]
fn explicit_wake_colors_and_laser_ignore_live_cursor_recolors() {
    let mut pipeline = EffectsPipeline::new();
    pipeline.set_cursor_glow(
        true,
        "lumen",
        crate::cursor_fx::TrailColor::Pinned(0x0001_0203),
        None,
        400,
        24,
        1.0,
        0.9,
        true,
        0x0011_2233,
    );
    pipeline.set_cursor_trail(true, 400, 24, Some(0x0004_0506), 0x0011_2233);
    let mut term = Terminal::new(2, 8);
    term.process(b"\x1b]12;rgb:aa/bb/cc\x07");
    let mut input = term.cell_frame(2, 8);
    stamp_cursor_color(&term, &mut input, 0x0011_2233);
    pipeline.apply(&mut term, &mut input, 10, 19);
    assert_eq!(pipeline.glow_cfg.color, 0x0001_0203);
    assert_eq!(pipeline.glow_cfg.accent, brighten(0x0001_0203, 1.5));
    assert_eq!(input.cursor_trail_color, 0x0004_0506);

    pipeline.set_cursor_glow(
        true,
        "laser",
        crate::cursor_fx::TrailColor::Automatic,
        None,
        400,
        24,
        1.0,
        0.9,
        true,
        0x0011_2233,
    );
    pipeline.set_cursor_trail(true, 400, 24, None, 0x0011_2233);
    term.cell_frame_into(&mut input, 2, 8);
    stamp_cursor_color(&term, &mut input, 0x0011_2233);
    pipeline.apply(&mut term, &mut input, 10, 19);
    assert_eq!(
        pipeline.glow_cfg.color,
        crate::cursor_glow::LASER_DEFAULT_COLOR,
        "Laser keeps its canonical storm-violet default hue"
    );
    assert_eq!(
        input.cursor_trail_color,
        crate::cursor_glow::LASER_DEFAULT_COLOR,
        "an automatic opaque trail wears the glow's violet, as the native comet body does"
    );
}

#[test]
fn the_page_colours_its_cursor_as_a_native_window_does() {
    use crate::cursor_fx::TrailColor;
    let (theme_cursor, osc12, chosen) = (0x0011_2233, 0x00AA_BBCC, 0x00E0_218A);
    let mut checked = 0;
    for row in crate::cursor_glow::spellings::TRAIL_STYLES {
        if row.style.is_none() {
            continue;
        }
        for setting in [
            TrailColor::Automatic,
            TrailColor::Cursor,
            TrailColor::Pinned(chosen),
        ] {
            let mut pipeline = EffectsPipeline::new();
            pipeline.set_cursor_glow(
                true,
                row.name,
                setting,
                None,
                400,
                24,
                1.0,
                0.6,
                true,
                theme_cursor,
            );
            pipeline.set_cursor_trail(true, 400, 24, None, theme_cursor);
            let mut term = Terminal::new(2, 8);
            let mut input = term.cell_frame(2, 8);
            for (osc, live, pinned) in [
                (&b""[..], theme_cursor, false),
                (&b"\x1b]12;rgb:aa/bb/cc\x07"[..], osc12, true),
                (&b"\x1b]112\x07"[..], theme_cursor, false),
            ] {
                term.process(osc);
                term.cell_frame_into(&mut input, 2, 8);
                stamp_cursor_color(&term, &mut input, theme_cursor);
                pipeline.apply(&mut term, &mut input, 10, 19);
                let mut glow = crate::cursor_fx::resolve_glow_config(
                    crate::cursor_fx::GlowKnobs {
                        enabled: true,
                        color: setting.knob(theme_cursor),
                        accent: None,
                        duration_ms: 400,
                        length: 24,
                        intensity: 1.0,
                        radius: 0.6,
                        ring: true,
                    },
                    crate::cursor_fx::GlowSpelling::parse(row.name),
                    theme_cursor,
                    true,
                    pipeline.glow_cfg.theme_fg,
                    pipeline.glow_cfg.theme_bg,
                    0.5,
                );
                let mut trail = pipeline.trail_cfg;
                crate::cursor_fx::follow_live_cursor(
                    &mut glow, &mut trail, live, pinned, setting, false, None,
                );
                let case = format!("{} {setting:?} live {live:06x}", row.name);
                assert_eq!(pipeline.glow_cfg.color, glow.color, "{case}");
                assert_eq!(pipeline.glow_cfg.accent, glow.accent, "{case}");
                assert_eq!(pipeline.trail_cfg.color, trail.color, "{case}");
                assert_eq!(input.cursor_trail_color, glow.color, "{case}");
                checked += 1;
            }
        }
    }
    assert!(checked > 100, "{checked}");

    let mut pipeline = EffectsPipeline::new();
    pipeline.set_cursor_glow(
        true,
        "laser",
        TrailColor::Automatic,
        None,
        400,
        24,
        1.0,
        0.6,
        true,
        theme_cursor,
    );
    pipeline.set_cursor_trail(true, 400, 24, Some(0x0004_0506), theme_cursor);
    let mut term = Terminal::new(2, 8);
    let mut input = term.cell_frame(2, 8);
    stamp_cursor_color(&term, &mut input, theme_cursor);
    pipeline.apply(&mut term, &mut input, 10, 19);
    assert_eq!(input.cursor_trail_color, 0x0004_0506);
    assert_eq!(
        pipeline.glow_cfg.color,
        crate::cursor_glow::LASER_DEFAULT_COLOR
    );

    assert!(pipeline.set_cursor_glow_color("cursor"));
    pipeline.apply(&mut term, &mut input, 10, 19);
    assert_eq!(pipeline.glow_cfg.color, theme_cursor);
    assert!(!pipeline.set_cursor_glow_color("violetish"));
    assert_eq!(pipeline.glow_trail_color, TrailColor::Cursor);
}

#[test]
fn an_automatic_comet_keeps_its_hue_until_osc12_pins_the_cursor() {
    let theme_cursor = 0x0011_2233;
    let mut pipeline = EffectsPipeline::new();
    pipeline.set_cursor_glow(
        true,
        "comet",
        crate::cursor_fx::TrailColor::Automatic,
        None,
        400,
        24,
        1.0,
        0.9,
        true,
        theme_cursor,
    );
    let mut term = Terminal::new(2, 8);
    term.set_default_cursor_color(Some(aterm_core::terminal::Rgb::new(0x11, 0x22, 0x33)));
    let comet = crate::cursor_glow::COMET_DEFAULT_COLOR;

    let mut input = term.cell_frame(2, 8);
    stamp_cursor_color(&term, &mut input, theme_cursor);
    pipeline.apply(&mut term, &mut input, 10, 19);
    assert_eq!(
        pipeline.glow_cfg.color, comet,
        "the theme's seed is not a request"
    );
    assert_eq!(pipeline.glow_cfg.accent, brighten(comet, 1.5));

    term.process(b"\x1b]12;rgb:aa/00/11\x07");
    term.cell_frame_into(&mut input, 2, 8);
    stamp_cursor_color(&term, &mut input, theme_cursor);
    pipeline.apply(&mut term, &mut input, 10, 19);
    assert_eq!(pipeline.glow_cfg.color, 0x00AA_0011, "OSC 12 pins it");
    assert_eq!(pipeline.glow_cfg.accent, brighten(0x00AA_0011, 1.5));

    term.process(b"\x1b]112\x07");
    term.cell_frame_into(&mut input, 2, 8);
    stamp_cursor_color(&term, &mut input, theme_cursor);
    pipeline.apply(&mut term, &mut input, 10, 19);
    assert_eq!(
        pipeline.glow_cfg.color, comet,
        "OSC 112 hands the comet its own hue back"
    );
}

#[test]
fn batched_keystrokes_spread_across_the_advance_delta() {
    let mut p = EffectsPipeline::new();
    for _ in 0..6 {
        p.note_keystroke();
    }
    p.advance(360.0);
    let spread = p.cursor.cadence.intensity(p.now());

    let mut hand = TypingCadence::default();
    let t0 = p.t0;
    for i in 1..=6u64 {
        hand.on_keystroke(t0 + Duration::from_millis(60 * i));
    }
    let paced = hand.intensity(t0 + Duration::from_millis(360));
    assert!(
        (spread - paced).abs() < 1e-4,
        "spread replay {spread} must match the true-cadence heat {paced}"
    );

    let mut collapsed = TypingCadence::default();
    let end = t0 + Duration::from_millis(360);
    for _ in 0..6 {
        collapsed.on_keystroke(end);
    }
    assert!(
        spread < collapsed.intensity(end),
        "the spread must ignite less than a zero-gap collapse"
    );
}

#[test]
fn chrome_geom_identity_and_padded_derivation() {
    let mut p = EffectsPipeline::new();
    let g = p.chrome_geom(24, 80, 9, 18);
    assert_eq!((g.origin_x, g.origin_y), (0, 0));
    assert_eq!((g.win_w, g.win_h), (80 * 9, 24 * 18));
    assert_eq!(g.head, 0);

    p.set_chrome(12, 30);
    let g = p.chrome_geom(24, 80, 9, 18);
    assert_eq!((g.origin_x, g.origin_y), (12, 42));
    assert_eq!((g.win_w, g.win_h), (80 * 9 + 24, 24 * 18 + 24 + 30));
    assert_eq!(g.head, 30);

    p.set_chrome(0, 0);
    let g = p.chrome_geom(24, 80, 9, 18);
    assert_eq!((g.origin_x, g.origin_y, g.head), (0, 0, 0));
    assert_eq!((g.win_w, g.win_h), (80 * 9, 24 * 18));
}

#[test]
fn fire_glow_splices_field_streams_into_the_head_band() {
    let (cell_w, cell_h) = (10usize, 19usize);
    let (pad, head) = (12u16, 30u16);
    let origin_y = pad + head; // grid top; the head band is above it
    let mut p = EffectsPipeline::new();
    p.set_chrome(pad, head);
    p.set_cursor_glow(
        true,
        "ember",
        crate::cursor_fx::TrailColor::Automatic,
        None,
        400,
        64,
        1.0,
        2.0,
        true,
        0x00FF_8833,
    );
    let mut term = Terminal::new(12, 40);
    let mut input = term.cell_frame(12, 40);
    p.apply(&mut term, &mut input, cell_w, cell_h);
    let (mut any_patch, mut band_patch, mut any_companion) = (false, false, false);
    for _ in 0..3 {
        for ch in [b"a", b"b", b"c", b"d"] {
            commit_ascii(&mut p, &mut term, &mut input, ch, 30.0);
            any_patch |= !input.fire_patch.is_empty();
            band_patch |= input.fire_patch.iter().any(|q| q.y < origin_y);
            any_companion |= !input.glow_under.is_empty()
                || !input.char_fg.is_empty()
                || !input.glow_halo.is_empty()
                || !input.fire_halo.is_empty();
        }
    }
    assert!(any_patch, "a burning fire must splice fire_patch quads");
    assert!(
        band_patch,
        "under chrome the flame field must rise above the grid (y < pad+head)"
    );
    assert!(
        any_companion,
        "a burn must splice at least one companion stream (halo/under/char/rim)"
    );
}

#[test]
fn effects_off_and_non_fire_styles_leave_fire_streams_empty() {
    let mut p = EffectsPipeline::new();
    let mut term = Terminal::new(6, 20);
    let mut input = term.cell_frame(6, 20);
    input.fire_patch.push(aterm_core::render::FirePatch {
        row: 0,
        x: 1,
        y: 1,
        w: 8,
        h: 8,
        base_y: 20,
        peak_h: 16,
        phase: 7,
        temp: 200,
        strength: 200,
        lean: 0,
        cov_cap: 200,
        cell_h: 19,
        mode: aterm_core::render::FireMode::Add,
    });
    input.glow_halo.push(RainHalo::default());
    input.glow_under.push(GlowQuad::default());
    input.char_fg.push(aterm_core::render::CharFg {
        row: 0,
        col: 0,
        fg: 0x00FF_0000,
    });
    input.fire_halo.push(aterm_core::render::FireHaloCell {
        row: 0,
        col: 0,
        strength: 128,
    });
    input.cursor_fill_override = Some(0x00FE_DCBA);
    let fp = p.apply(&mut term, &mut input, 10, 19);
    assert_eq!(fp, 0, "all-off apply is the identity fingerprint");
    assert!(input.fire_patch.is_empty(), "stale fire_patch cleared");
    assert!(input.glow_halo.is_empty(), "stale glow_halo cleared");
    assert!(input.glow_under.is_empty(), "stale glow_under cleared");
    assert!(input.char_fg.is_empty(), "stale char_fg cleared");
    assert!(input.fire_halo.is_empty(), "stale fire_halo cleared");
    assert_eq!(
        input.cursor_fill_override, None,
        "all-off apply clears a stale forge cursor fill"
    );

    p.set_cursor_glow(
        true,
        "lumen",
        crate::cursor_fx::TrailColor::Automatic,
        None,
        400,
        24,
        0.9,
        0.9,
        true,
        0x0050_FA7B,
    );
    for ch in [b"a", b"b", b"c"] {
        term.process(ch);
        p.advance(30.0);
        input = term.cell_frame(6, 20);
        p.apply(&mut term, &mut input, 10, 19);
        assert!(input.fire_patch.is_empty(), "lumen never emits fire_patch");
        assert!(input.glow_under.is_empty(), "lumen never emits glow_under");
        assert!(input.char_fg.is_empty(), "lumen never emits char_fg");
        assert!(input.fire_halo.is_empty(), "lumen never emits fire_halo");
    }
}

#[test]
fn unfocused_pane_emits_no_glow_but_focused_one_does() {
    let mut p = EffectsPipeline::new();
    p.set_cursor_glow(
        true,
        "lumen",
        crate::cursor_fx::TrailColor::Automatic,
        None,
        400,
        24,
        0.9,
        0.9,
        true,
        0x0050_FA7B,
    );
    let configured = p.glow_cfg.intensity;
    let mut term = Terminal::new(6, 20);
    let mut input = term.cell_frame(6, 20);
    p.apply(&mut term, &mut input, 10, 19);
    let (mut lit, mut fp_focused) = (false, 0u64);
    for ch in [b"a", b"b", b"c"] {
        fp_focused = commit_ascii(&mut p, &mut term, &mut input, ch, 30.0);
        lit |= !input.cursor_glow_add.is_empty();
    }
    assert!(lit, "a focused pane emits cursor light");
    assert_ne!(fp_focused, 0, "emitted light folds a nonzero fingerprint");

    p.set_focused(false);
    for ch in [b"d", b"e", b"f"] {
        let fp = commit_ascii(&mut p, &mut term, &mut input, ch, 30.0);
        assert!(
            input.cursor_glow_add.is_empty(),
            "an unfocused pane emits no aurora"
        );
        assert!(input.glow_halo.is_empty(), "no radial halo while unfocused");
        assert!(input.fire_patch.is_empty(), "no fire field while unfocused");
        assert!(
            input.glow_under.is_empty(),
            "no under-ink flame while unfocused"
        );
        assert!(input.char_fg.is_empty(), "no charred ink while unfocused");
        assert!(
            input.fire_halo.is_empty(),
            "no contrast halo while unfocused"
        );
        assert_eq!(fp, 0, "an unfocused pane folds the identity fingerprint");
    }
    assert_eq!(
        p.glow_cfg.intensity, configured,
        "the stored intensity survives an unfocused stretch byte-unchanged"
    );
    p.set_focused(true);
    let mut relit = false;
    for ch in [b"g", b"h", b"i"] {
        commit_ascii(&mut p, &mut term, &mut input, ch, 30.0);
        relit |= !input.cursor_glow_add.is_empty();
    }
    assert!(relit, "refocus restores the light");
}

#[test]
fn unfocused_glow_settles_so_the_host_stops_rebooking_the_pane() {
    let mut p = EffectsPipeline::new();
    p.set_cursor_glow(
        true,
        "lumen",
        crate::cursor_fx::TrailColor::Automatic,
        None,
        400,
        24,
        0.9,
        0.9,
        true,
        0x0050_FA7B,
    );
    let mut term = Terminal::new(6, 20);
    let mut input = term.cell_frame(6, 20);
    p.apply(&mut term, &mut input, 10, 19);
    for ch in [b"a", b"b", b"c"] {
        commit_ascii(&mut p, &mut term, &mut input, ch, 30.0);
    }
    assert!(p.is_active(), "a live focused glow holds the engine active");

    p.set_focused(false);
    for _ in 0..2 {
        p.advance(16.0);
        let mut input = term.cell_frame(6, 20);
        p.apply(&mut term, &mut input, 10, 19);
    }
    assert!(
        !p.is_active(),
        "an idle unfocused pane settles so the shared rAF can drop it"
    );
}

#[test]
fn hidden_visibility_hard_drains_cursor_state_before_refocus_without_apply() {
    let mut p = EffectsPipeline::new();
    p.set_cursor_glow(
        true,
        "lumen",
        crate::cursor_fx::TrailColor::Automatic,
        None,
        400,
        24,
        0.9,
        0.9,
        true,
        0x0050_FA7B,
    );
    p.set_cursor_trail(true, 400, 24, None, 0x0050_FA7B);
    let g = Geom {
        cw: 8,
        ch: 16,
        rows: 6,
        cols: 20,
        origin_x: 0,
        origin_y: 0,
        win_w: 160,
        win_h: 96,
        head: 0,
    };
    let now = Instant::now();
    p.cursor.glow.tick(
        Some((2, 2)),
        now,
        &p.glow_cfg,
        g,
        &mut p.cursor.glow_scratch,
    );
    p.cursor
        .trail
        .tick(Some((2, 2)), now, &p.trail_cfg, &mut p.cursor.trail_scratch);
    let moved = now + Duration::from_millis(1);
    p.cursor.glow.note_synthetic_move(moved);
    p.cursor.trail.note_synthetic_move(moved);
    p.cursor.glow.tick(
        Some((2, 3)),
        moved,
        &p.glow_cfg,
        g,
        &mut p.cursor.glow_scratch,
    );
    p.cursor.trail.tick(
        Some((2, 3)),
        moved,
        &p.trail_cfg,
        &mut p.cursor.trail_scratch,
    );
    assert!(p.cursor.glow.is_active() && p.cursor.trail.is_active());

    let pending = moved + Duration::from_millis(1);
    p.note_keystroke();
    p.cursor.glow.note_synthetic_move(pending);
    p.cursor.trail.note_synthetic_move(pending);
    assert!(
        p.cursor.glow.move_licensed(pending),
        "the scripted note licenses"
    );
    p.set_effects_visibility("hidden");
    assert!(!p.cursor.glow.is_active() && !p.cursor.trail.is_active());
    assert!(
        !p.cursor.glow.move_licensed(pending),
        "a hidden teardown spends every license term"
    );
    assert!(p.cursor.glow_scratch.is_empty() && p.cursor.trail_scratch.is_empty());
    assert_eq!(p.pending_keys, 0, "hidden drops queued cursor-key replay");
    assert_eq!(p.cursor.cadence.sample(pending), (0.0, 0.0));

    p.set_effects_visibility("focused");
    let refocused = pending + Duration::from_millis(1);
    assert_eq!(
        p.cursor.glow.tick(
            Some((2, 3)),
            refocused,
            &p.glow_cfg,
            g,
            &mut p.cursor.glow_scratch,
        ),
        0
    );
    assert_eq!(
        p.cursor.trail.tick(
            Some((2, 3)),
            refocused,
            &p.trail_cfg,
            &mut p.cursor.trail_scratch,
        ),
        0
    );
    assert!(p.cursor.glow_scratch.is_empty() && p.cursor.trail_scratch.is_empty());
}

fn typed_page(style: &str, reduced: bool) -> (EffectsPipeline, bool) {
    let mut p = EffectsPipeline::new();
    p.set_cursor_glow(
        true,
        style,
        crate::cursor_fx::TrailColor::Automatic,
        None,
        400,
        24,
        0.9,
        0.9,
        true,
        0x0050_FA7B,
    );
    p.set_reduced_motion(reduced);
    let mut term = Terminal::new(6, 20);
    let mut input = term.cell_frame(6, 20);
    p.apply(&mut term, &mut input, 10, 19);
    let mut lit = false;
    for ch in b"abcdefghij" {
        commit_ascii(&mut p, &mut term, &mut input, &[*ch], 30.0);
        lit |= p.cursor_block_fill().is_some()
            || !input.cursor_glow_add.is_empty()
            || input.cursor_effect_style_override.is_some()
            || !input.cursor_trail.is_empty();
    }
    (p, lit)
}

const BODY_STYLES: [&str; 4] = ["rainbow kitty", "water", "lumen", "comet"];

#[test]
fn reduced_motion_turns_off_the_page_cursor_family() {
    for style in BODY_STYLES {
        let (_, lit) = typed_page(style, false);
        assert!(lit, "{style}: control — a fast hand lights the family");
        let (p, lit) = typed_page(style, true);
        assert!(!lit, "{style}: reduced motion lit the cursor family");
        assert!(
            !p.cursor.bodies_active(),
            "{style}: reduced motion left a body animating"
        );
    }
}

#[test]
fn a_page_keeps_its_wake_until_the_momentum_glow_is_cold() {
    let mut p = EffectsPipeline::new();
    p.set_cursor_glow(
        true,
        "classic",
        crate::cursor_fx::TrailColor::Automatic,
        None,
        400,
        24,
        0.9,
        0.9,
        true,
        0x0050_FA7B,
    );
    let mut term = Terminal::new(6, 20);
    let mut input = term.cell_frame(6, 20);
    p.apply(&mut term, &mut input, 10, 19);
    for ch in b"abcdefghij" {
        commit_ascii(&mut p, &mut term, &mut input, &[*ch], 60.0);
    }
    assert!(
        p.cursor_block_fill()
            .is_some_and(|owned| owned.owner == crate::cursor_glow::BlockFillOwner::Momentum),
        "fixture: the typed caret is the momentum glow's"
    );
    let (mut held_by_settle, mut frames) = (false, 0u32);
    loop {
        let dt_ms = match p.wake() {
            Wake::Frames => 16.7,
            Wake::At(at) => {
                let ms = at.saturating_duration_since(p.now()).as_secs_f64() * 1000.0;
                assert!(ms > 0.0, "a named wake at or before now is a busy re-arm");
                ms
            }
            Wake::Idle => break,
        };
        held_by_settle |= p.cursor.momentum.settling() && !p.cursor.bodies_active();
        assert!(
            !p.cursor.momentum.settling() || p.is_active(),
            "a settling glow keeps the page active, or the frame gate skips its step"
        );
        idle(&mut p, &mut term, &mut input, 1, dt_ms);
        frames += 1;
        assert!(frames < 5_000, "the page never went idle");
    }
    assert!(
        held_by_settle,
        "negative control: the frame cadence alone let go of a hot glow"
    );
    assert!(
        !p.cursor.momentum.is_active() && !p.cursor.momentum.settling(),
        "idle only once the glow is cold"
    );
    assert!(
        p.cursor_block_fill().is_none(),
        "the last frame before idle returned the caret to the terminal"
    );
}

#[test]
fn hidden_visibility_retires_the_bodies_before_the_next_apply() {
    for style in BODY_STYLES {
        let (mut p, _) = typed_page(style, false);
        assert!(
            p.cursor.bodies_active(),
            "{style}: control — the typed page has a live body"
        );
        p.set_effects_visibility("hidden");
        assert!(
            !p.cursor.bodies_active(),
            "{style}: a hidden page kept a live body"
        );
        assert!(!p.is_active(), "{style}: a hidden page asked for frames");
    }
}

#[test]
fn refocus_resumes_without_a_phantom_comet() {
    let mut p = EffectsPipeline::new();
    p.set_cursor_trail(true, 260, 24, None, 0x0050_FA7B);
    let mut term = Terminal::new(6, 20);
    term.process(b"ab");
    p.advance(30.0);
    let mut input = term.cell_frame(6, 20);
    p.apply(&mut term, &mut input, 10, 19);

    p.set_focused(false);
    for ch in [b"c", b"d", b"e", b"f", b"g", b"h"] {
        term.process(ch);
        p.advance(30.0);
        let mut input = term.cell_frame(6, 20);
        p.apply(&mut term, &mut input, 10, 19);
    }

    p.set_focused(true);
    p.advance(30.0);
    let mut input = term.cell_frame(6, 20);
    p.apply(&mut term, &mut input, 10, 19);
    assert!(
        input.cursor_trail.is_empty(),
        "refocus at a stationary cursor sweeps no cells"
    );
}

#[test]
fn pipeline_typed_keys_light_and_cold_program_motion_stays_dark() {
    let mut typed = EffectsPipeline::new();
    typed.set_cursor_trail(true, 260, 24, None, 0x0050_FA7B);
    let mut typed_term = Terminal::new(6, 20);
    let mut typed_input = typed_term.cell_frame(6, 20);
    typed.apply(&mut typed_term, &mut typed_input, 10, 19); // seed cursor

    for _ in 0..4 {
        typed.note_keystroke();
        typed_term.process(b"x");
    }
    typed.advance(16.0);
    typed_term.cell_frame_into(&mut typed_input, 6, 20);
    typed.apply(&mut typed_term, &mut typed_input, 10, 19);
    assert!(
        !typed_input.cursor_trail.is_empty(),
        "an rAF batch of real presses licenses its coalesced sweep"
    );

    let mut cold = EffectsPipeline::new();
    cold.set_cursor_trail(true, 260, 24, None, 0x0050_FA7B);
    let mut cold_term = Terminal::new(6, 20);
    let mut cold_input = cold_term.cell_frame(6, 20);
    cold.apply(&mut cold_term, &mut cold_input, 10, 19); // seed cursor
    cold_term.process(b"\x1b[1;5H");
    cold.advance(16.0);
    cold_term.cell_frame_into(&mut cold_input, 6, 20);
    cold.apply(&mut cold_term, &mut cold_input, 10, 19);
    assert!(
        cold_input.cursor_trail.is_empty(),
        "unwitnessed program CUP movement cannot mint a classic comet"
    );
    assert!(!cold.cursor.trail.is_active());
}

#[test]
fn unfocused_trail_clears_its_comet_but_still_publishes_the_colour() {
    let mut p = EffectsPipeline::new();
    p.set_cursor_trail(true, 260, 24, None, 0x0050_FA7B);
    let mut term = Terminal::new(6, 20);
    let mut input = term.cell_frame(6, 20);
    p.apply(&mut term, &mut input, 10, 19);
    let mut swept = false;
    for ch in [b"a", b"b", b"c"] {
        commit_ascii(&mut p, &mut term, &mut input, ch, 30.0);
        swept |= !input.cursor_trail.is_empty();
    }
    assert!(swept, "a focused pane sweeps a comet");
    assert!(
        p.cursor.trail.is_active(),
        "sparks are live before the gate"
    );

    p.set_focused(false);
    p.advance(30.0);
    term.cell_frame_into(&mut input, 6, 20);
    p.apply(&mut term, &mut input, 10, 19);
    assert!(
        input.cursor_trail.is_empty(),
        "the gate clears the comet while its sparks would still be alive"
    );
    assert!(
        !p.cursor.trail.is_active(),
        "the trail settles on its first dark tick"
    );
    assert_ne!(
        input.cursor_trail_color, 0,
        "the colour is still published while unfocused"
    );

    p.advance(1000.0);
    term.cell_frame_into(&mut input, 6, 20);
    p.apply(&mut term, &mut input, 10, 19);
    assert_eq!(
        input.cursor_trail_color, 0x0050_FA7B,
        "the colour is keyed on the stored flag, not the focus-gated local"
    );
}

#[test]
fn focus_does_not_alter_word_decoration_emission() {
    let drive = |focused: bool| {
        let mut p = EffectsPipeline::new();
        p.set_sparkle_enabled(true);
        p.set_focused(focused);
        let mut term = Terminal::new(4, 24);
        term.process(b"\r\n\r\na nice kitty");
        let mut input = RenderInput::default();
        p.advance(10.0);
        term.cell_frame_into(&mut input, 4, 24);
        p.apply(&mut term, &mut input, 10, 20);
        let mut frames = Vec::new();
        for _ in 0..60 {
            p.advance(100.0);
            term.cell_frame_into(&mut input, 4, 24);
            p.apply(&mut term, &mut input, 10, 20);
            frames.push((
                free_sprites(&input).len(),
                input.word_decorations.clone(),
                input.ink.clone(),
            ));
        }
        frames
    };
    let focused = drive(true);
    let unfocused = drive(false);
    assert!(
        focused.iter().any(|(sprites, _, _)| *sprites > 0),
        "the focused cat actually played (else this proves nothing)"
    );
    assert_eq!(
        focused, unfocused,
        "decorations are out of scope for the focus gate (latch-at-birth)"
    );
}

#[test]
fn the_page_floors_its_caret_against_the_cell_under_it() {
    let fill_on = |reverse: bool| {
        let mut p = EffectsPipeline::new();
        p.set_cursor_glow(
            true,
            "fire",
            crate::cursor_fx::TrailColor::Automatic,
            None,
            400,
            24,
            1.0,
            0.9,
            true,
            0x0050_FA7B,
        );
        let mut term = Terminal::new(6, 20);
        term.process(if reverse {
            b"ab\x1b[7mx\x1b[27m\x1b[1;3H"
        } else {
            b"abx\x1b[1;3H"
        });
        let mut input = term.cell_frame(6, 20);
        p.apply(&mut term, &mut input, 10, 19);
        let cell = aterm_render::rgb_to_u32(input.cells[0][2].bg);
        (
            input
                .cursor_fill_override
                .expect("the forge owns the caret"),
            cell,
        )
    };
    let (fill, cell) = fill_on(true);
    let (plain, page_cell) = fill_on(false);
    assert_ne!(
        cell, page_cell,
        "SGR 7 gives the caret's cell its own ground"
    );
    let ratio = crate::color_math::contrast_ratio(fill, cell);
    assert!(
        ratio >= crate::cursor_fx::CARET_PAGE_MIN_CONTRAST,
        "the ember {fill:06x} on the reverse cell {cell:06x} reads {ratio:.2}:1"
    );
    assert!(
        crate::color_math::contrast_ratio(plain, cell) < crate::cursor_fx::CARET_PAGE_MIN_CONTRAST,
        "control: the page-floored ember {plain:06x} is under the floor on the reverse cell"
    );
}

#[test]
fn fire_forge_cursor_fill_is_spliced_and_cleared_on_toggle() {
    let mut p = EffectsPipeline::new();
    p.set_cursor_glow(
        true,
        "fire",
        crate::cursor_fx::TrailColor::Automatic,
        None,
        400,
        24,
        1.0,
        0.9,
        true,
        0x0050_FA7B,
    );
    let mut term = Terminal::new(6, 20);
    let mut input = term.cell_frame(6, 20);
    p.apply(&mut term, &mut input, 10, 19);
    let page = if input.default_bg == aterm_core::render::COLOR_UNSET {
        COLD_THEME_BG
    } else {
        input.default_bg & 0x00FF_FFFF
    };
    assert_eq!(
        input.cursor_fill_override,
        p.cursor.glow.forge_fill().map(|raw| {
            crate::cursor_fx::floor_block_fill(
                crate::cursor_glow::BlockFill {
                    owner: crate::cursor_glow::BlockFillOwner::Forge,
                    fill: raw,
                    base: None,
                },
                page,
                terminal_cursor_color(&term),
                None,
                false,
            )
            .fill
        }),
        "Fire publishes its warm-metal cursor fill, held to the page floor"
    );
    assert!(
        input.cursor_fill_override.is_some(),
        "Fire's cold forge fill is deliberately still visible"
    );

    p.set_cursor_glow(
        true,
        "lumen",
        crate::cursor_fx::TrailColor::Automatic,
        None,
        400,
        24,
        1.0,
        0.9,
        true,
        0x0050_FA7B,
    );
    term.cell_frame_into(&mut input, 6, 20);
    p.apply(&mut term, &mut input, 10, 19);
    assert_ne!(
        p.cursor_block_fill().map(|owned| owned.owner),
        Some(crate::cursor_glow::BlockFillOwner::Forge),
        "switching away from Fire clears the reused frame's forge fill"
    );
    assert_eq!(
        input.cursor_fill_override,
        p.cursor_block_fill().map(|owned| owned.fill),
        "the reused frame carries the new owner's fill, not the forge's"
    );
}

fn composer_toggle_model() -> aterm_spec::derive::Model {
    use aterm_spec::ty_model;
    ty_model! {
        ComposerToggleGate {
            const Buggy = 0;
            var host = 0;
            var present = 0;
            var engine = 0;
            action Key {
                host = 1;
                engine = if present == 1 { 1 } else { 0 };
            }
            action TurnStart {
                host = 0;
                engine = 0;
            }
            action Enable {
                present = 1;
                engine = if Buggy == 1 { 0 } else { host };
            }
            action Disable {
                present = 0;
                engine = 0;
            }
            invariant BooleanBounds: host <= 1 && present <= 1 && engine <= 1;
            invariant EngineMatchesHost: engine == if present == 1 { host } else { 0 };
        }
    }
}

#[test]
fn composer_toggle_model_proves_and_real_pipeline_conforms() {
    let model = composer_toggle_model();
    aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
    let mut state = model.init_state();
    let mut pipeline = EffectsPipeline::new();
    for action in ["Key", "Enable", "Disable", "Enable", "TurnStart", "Key"] {
        match action {
            "Key" => pipeline.note_keystroke(),
            "TurnStart" => pipeline
                .note_matrix_rain_signal(crate::matrix_rain::RainSignal::TurnStart as u32, 4),
            "Enable" => pipeline.set_matrix_rain_enabled(true),
            "Disable" => pipeline.set_matrix_rain_enabled(false),
            _ => unreachable!(),
        }
        assert!(model.fire(action, &mut state));
        assert_eq!(i64::from(pipeline.rain_material_editing), state["host"]);
        assert_eq!(i64::from(pipeline.rain.is_some()), state["present"]);
        let engine_editing = pipeline
            .rain
            .as_deref()
            .is_some_and(MatrixRain::material_editing_for_test);
        assert_eq!(i64::from(engine_editing), state["engine"]);
    }
}

/// The trail's published streams lay no light on a word icon's drawn texels, while the sprite
/// box's transparent margin keeps its light. Negative control: unshaded, the same frames lay
/// light on the face.
#[test]
fn the_published_trail_lays_no_light_on_a_word_icon() {
    use aterm_core::render::FxTexels;
    use aterm_render::sprites::FreeZ;
    // (frames with an icon, lit face texels, lit margin texels).
    let run = |unshaded: bool| -> (usize, usize, usize) {
        let mut p = EffectsPipeline::new();
        rainbow_kitty_glow(&mut p, "rainbow kitty");
        p.set_sparkle_enabled(true);
        p.word_icons_unshaded = unshaded;
        let (mut term, mut input) = glass(8, 40);
        // The caret on the last row, so the Return scrolls the `fox` up
        // into a box no keep-out held yet (`enter-bottom`).
        term.process(b"\r\n\r\n\r\n\r\n\r\n\r\n\r\n");
        idle(&mut p, &mut term, &mut input, 2, 33.0);
        type_line(&mut p, &mut term, &mut input, b"the quick brown fox", 33.0);
        idle(&mut p, &mut term, &mut input, 45, 33.0);
        p.note_console_input(PetInputKind::Submit);
        term.process(b"\r\n");
        let (mut framed, mut face, mut margin) = (0usize, 0usize, 0usize);
        for _ in 0..30 {
            idle(&mut p, &mut term, &mut input, 1, 16.0);
            let icons: Vec<_> = free_sprites(&input)
                .into_iter()
                .filter(|s| s.z == FreeZ::UnderText && !s.is_blank())
                .collect();
            if icons.is_empty() {
                continue;
            }
            framed += 1;
            // Whether window pixel (x, y) is an icon's texel at least
            // half opaque (`None`: off every icon's visible box).
            let texel = |x: i32, y: i32| -> Option<bool> {
                icons.iter().find_map(|s| {
                    let v = s.visible();
                    if !(v.x0..v.x1).contains(&x) || !(v.y0..v.y1).contains(&y) {
                        return None;
                    }
                    let (dx, dy) = (x - s.x, y - s.y);
                    let (w, h) = (i32::from(s.w), i32::from(s.h));
                    let (aw, ah) = (i32::from(s.art.w()), i32::from(s.art.h()));
                    let mx = if s.flip_x { w - 1 - dx } else { dx };
                    let sx = ((2 * mx + 1) * aw / (2 * w)).min(aw - 1) as usize;
                    let sy = ((2 * dy + 1) * ah / (2 * h)).min(ah - 1) as usize;
                    let t = s.art.texel(sx, sy);
                    Some(match s.art.texels() {
                        FxTexels::Index8 => t != 0,
                        _ => t >> 24 >= 128,
                    })
                })
            };
            let mut tally = |x: u16, y: u16, w: u16, h: u16| {
                for py in i32::from(y)..i32::from(y) + i32::from(h) {
                    for px in i32::from(x)..i32::from(x) + i32::from(w) {
                        match texel(px, py) {
                            Some(true) => face += 1,
                            Some(false) => margin += 1,
                            None => {}
                        }
                    }
                }
            };
            for q in input.cursor_glow_add.iter().chain(&input.glow_under) {
                if (q.color | q.color2) & 0x00FF_FFFF != 0 || q.alpha | q.alpha2 != 0 {
                    tally(q.x, q.y, q.w, q.h);
                }
            }
            for h in &input.glow_halo {
                tally(h.x, h.y, h.w, h.h);
            }
        }
        (framed, face, margin)
    };
    let (framed, face, margin) = run(false);
    let (control_framed, control_face, _) = run(true);
    assert!(
        framed > 10 && control_framed > 10,
        "fixture: the word pass drew the `fox` ({framed}, {control_framed} frames)"
    );
    assert!(
        control_face > 0,
        "control: unshaded, the Return's light lies on the face"
    );
    assert_eq!(face, 0, "the published trail lays light on the face");
    assert!(
        margin > 0,
        "the box's transparent margin keeps its light (no notch beside the face)"
    );
}

/// A FRAME WITHOUT CELLS NAMES ITS WORD ICONS (looks7 M10): a host that
/// hands no cells runs the word pass from the terminal, and the icons it
/// draws reach the next frame's sky, ribbon and embers as the grid's
/// pass's do, where they were never named and the sky drew its stars on
/// the `fox`. Negative control: unnamed, the glow holds none of them.
#[test]
fn a_frame_without_cells_names_its_word_icons() {
    use aterm_render::sprites::FreeZ;
    // (frames that drew an icon, frames whose icons the glow does not hold).
    let run = |unnamed: bool| -> (usize, usize) {
        let mut p = EffectsPipeline::new();
        rainbow_kitty_glow(&mut p, "rainbow kitty");
        p.set_sparkle_enabled(true);
        p.word_icons_unnamed = unnamed;
        let (mut term, mut input) = glass(8, 40);
        term.process(b"\r\n\r\n\r\n\r\n\r\n\r\n\r\n");
        idle(&mut p, &mut term, &mut input, 2, 33.0);
        type_line(&mut p, &mut term, &mut input, b"the quick brown fox", 33.0);
        idle(&mut p, &mut term, &mut input, 45, 33.0);
        p.note_console_input(PetInputKind::Submit);
        term.process(b"\r\n");
        let (rows, cols) = (input.rows, input.cols);
        let (mut framed, mut unheld) = (0, 0);
        for _ in 0..30 {
            p.advance(16.0);
            term.cell_frame_into(&mut input, rows, cols);
            input.cells.clear();
            input.line_sizes.clear();
            let _ = p.apply(&mut term, &mut input, 10, 19);
            let drawn = free_sprites(&input)
                .iter()
                .filter(|s| s.z == FreeZ::UnderText && !s.is_blank())
                .count();
            framed += usize::from(drawn > 0);
            unheld += usize::from(drawn > 0 && p.cursor.glow.word_icon_boxes().len() != drawn);
        }
        (framed, unheld)
    };
    let (framed, unheld) = run(false);
    assert!(
        framed > 10,
        "fixture: the word pass drew the `fox` in {framed} frames"
    );
    assert_eq!(
        unheld, 0,
        "{unheld} of {framed} frames' icons are not the glow's"
    );
    let (control_framed, control_unheld) = run(true);
    assert!(
        control_framed > 10 && control_unheld == control_framed,
        "control: unnamed, {control_unheld} of {control_framed} frames' icons are not the glow's"
    );
}

#[test]
fn the_page_samples_a_peeking_cats_palette_over_its_footprint() {
    let mut p = EffectsPipeline::new();
    p.set_sparkle_enabled(true);
    let mut term = Terminal::new(4, 20);
    term.process(b"\r\n\r\nkitty");
    let mut input = RenderInput::default();
    term.cell_frame_into(&mut input, 4, 20);
    let blank = input.cells[2][0];
    for (r, line) in input.cells.iter_mut().enumerate() {
        line.resize(20, aterm_core::terminal::RenderCell { ch: ' ', ..blank });
        for cell in line {
            cell.bg = if r == 0 { [248, 248, 248] } else { [4, 4, 4] };
        }
    }
    p.advance(10.0);
    p.apply(&mut term, &mut input, 10, 20);
    let (cats, _) = p.decos.scan_verdicts();
    let [key] = cats[..] else {
        panic!("one feline occurrence, got {cats:?}");
    };
    assert_eq!(
        key.background, 4,
        "the footprint spans the light row above the word: {key:?}"
    );
}

#[test]
fn the_page_keeps_a_caret_touching_collision_form_provisional() {
    let mut p = EffectsPipeline::new();
    p.set_sparkle_enabled(true);
    p.set_sparkle_languages(&["ro"]);
    let mut term = Terminal::new(2, 48);
    let mut input = RenderInput::default();
    term.process(b"fut");
    term.cell_frame_into(&mut input, 2, 48);
    p.advance(10.0);
    p.apply(&mut term, &mut input, 10, 20);
    assert!(
        !p.decos.scan_verdicts().1,
        "`fut` under the caret is provisional"
    );

    term.process(b" ");
    term.cell_frame_into(&mut input, 2, 48);
    p.advance(10.0);
    p.apply(&mut term, &mut input, 10, 20);
    assert!(p.decos.scan_verdicts().1, "the delimiter commits it");
}

#[test]
fn v3_web_resolver_pins_classes_and_custom_specs() {
    let mut p = EffectsPipeline::new();
    p.set_sparkle_enabled(true);
    p.set_sparkle_classes(true, true, true);
    let cfg = &p.sparkle.as_ref().expect("on").cfg;
    assert!(cfg.profanity && cfg.feline && cfg.emphasis);
    p.set_sparkle_ink(false, 0.75, 2200, false);
    assert!(!p.sparkle.as_ref().unwrap().cfg.emphasis);
    p.set_sparkle_custom_specs(Some(
        "[[sparkle_words.custom]]\nwords = [\"ultrathink\"]\nink = { colorway = \"rainbow\" }\n"
            .to_string(),
    ));
    let cfg = &p.sparkle.as_ref().unwrap().cfg;
    assert!(
        cfg.emphasis && cfg.spec_table.has_custom(),
        "custom specs keep emphasis scanning with ink off (v3 §6)"
    );
    p.set_sparkle_ink(true, 0.75, 2200, false);
    let mut term = Terminal::new(4, 32);
    term.process(b"go ultrathink now");
    let mut input = RenderInput::default();
    p.advance(10.0);
    term.cell_frame_into(&mut input, 4, 32);
    p.apply(&mut term, &mut input, 10, 20);
    assert_eq!(
        input.ink.len(),
        10,
        "the custom rainbow word inks its 10 lead cells"
    );
    p.set_sparkle_custom_specs(Some("[[sparkle_words.custom".to_string()));
    assert!(
        p.sparkle.as_ref().unwrap().cfg.spec_table.has_custom(),
        "malformed fragment keeps the previous specs"
    );
    assert!(
        p.sparkle_lexicon_warnings()
            .iter()
            .any(|w| w.starts_with("sparkle_words.custom:")),
        "the parse diagnostic surfaces instead of vanishing"
    );
    p.set_sparkle_custom_specs(None);
    assert!(!p.sparkle.as_ref().unwrap().cfg.spec_table.has_custom());
    assert!(
        p.sparkle_lexicon_warnings().is_empty(),
        "a clear drops the malformed-fragment warning"
    );
}

#[test]
fn lexicon_warnings_surface_and_respect_cjk_single_char_opt_in() {
    let mut p = EffectsPipeline::new();
    assert!(
        p.sparkle_lexicon_warnings().is_empty(),
        "off: no lexicon, no warnings"
    );
    p.set_sparkle_enabled(true);
    assert!(
        p.sparkle_lexicon_warnings().is_empty(),
        "the builtin lexicon is conflict-free"
    );
    p.set_sparkle_custom_specs(Some(
        "[[sparkle_words.custom]]\nwords = [\"犬\", \"abc猫\"]\nink = { colorway = \"rainbow\" }\n"
            .to_string(),
    ));
    let warns = p.sparkle_lexicon_warnings();
    assert!(
        warns
            .iter()
            .any(|w| w.contains('犬') && w.contains("cjk_single_char")),
        "FIX 7: the web path surfaces the single-char-CJK conflict, got {warns:?}"
    );
    assert!(
        warns.iter().any(|w| w.contains("abc猫")),
        "FIX 7: the web path surfaces the mixed-script drop, got {warns:?}"
    );
    p.set_sparkle_feline("cat", true, true, true);
    let warns = p.sparkle_lexicon_warnings();
    assert!(
        !warns.iter().any(|w| w.contains('犬')),
        "FIX 8: cjk_single_char = true satisfies the warning, got {warns:?}"
    );
    assert!(
        warns.iter().any(|w| w.contains("abc猫")),
        "unrelated warnings survive the opt-in filter, got {warns:?}"
    );
    p.set_sparkle_feline("cat", true, true, false);
    assert!(
        p.sparkle_lexicon_warnings()
            .iter()
            .any(|w| w.contains('犬'))
    );
    p.set_sparkle_enabled(false);
    assert!(p.sparkle_lexicon_warnings().is_empty());
}

#[test]
fn v3_reset_table_knob_setters_hard_reset_and_alt_suppression_freezes() {
    let mut p = EffectsPipeline::new();
    p.set_sparkle_enabled(true);
    p.set_sparkle_alt_screen_suppression(true);
    p.set_focused(true);
    let mut term = Terminal::new(4, 24);
    term.process(b"\r\n\r\na nice kitty");
    let mut input = RenderInput::default();

    p.advance(10.0);
    term.cell_frame_into(&mut input, 4, 24);
    p.apply(&mut term, &mut input, 10, 20);
    let mut saw_cat = false;
    for _ in 0..60 {
        p.advance(100.0);
        term.cell_frame_into(&mut input, 4, 24);
        p.apply(&mut term, &mut input, 10, 20);
        saw_cat |= !free_sprites(&input).is_empty();
    }
    assert!(saw_cat, "the peek actually played");
    assert!(
        free_sprites(&input).is_empty(),
        "the one-shot is spent (zero sprites at rest)"
    );

    p.set_sparkle_feline("cat", true, true, false);
    p.advance(10.0);
    term.cell_frame_into(&mut input, 4, 24);
    p.apply(&mut term, &mut input, 10, 20); // release present (reveal 0)
    let mut replayed = false;
    for _ in 0..10 {
        p.advance(100.0);
        term.cell_frame_into(&mut input, 4, 24);
        p.apply(&mut term, &mut input, 10, 20);
        replayed |= !free_sprites(&input).is_empty();
    }
    assert!(
        replayed,
        "web knob setters are hard_reset(): the one-shot replays (§1.1 table)"
    );

    let sprites_before = free_sprites(&input).clone();
    assert!(
        !sprites_before.is_empty(),
        "mid-peek before the alt round-trip"
    );
    term.process(b"\x1b[?1049h"); // enter the alternate screen
    p.advance(100.0);
    term.cell_frame_into(&mut input, 4, 24);
    p.apply(&mut term, &mut input, 10, 20);
    assert!(
        free_sprites(&input).is_empty(),
        "suppressed: no decorations"
    );
    term.process(b"\x1b[?1049l"); // long suspension, then back
    p.advance(5000.0);
    term.cell_frame_into(&mut input, 4, 24);
    p.apply(&mut term, &mut input, 10, 20);
    assert_eq!(
        free_sprites(&input).len(),
        sprites_before.len(),
        "thaw resumes the peek (no mass replay, and it did not silently end)"
    );
    // The art, its box and the window it is seen through (the peek's
    // reveal), not where the idle breath has it.
    let peek = |s: &FreeSprite| {
        let v = s.visible();
        (s.w, s.h, s.art.id(), v.y0 - s.y, v.x1 - v.x0, v.y1 - v.y0)
    };
    for (after, before) in free_sprites(&input).iter().zip(sprites_before.iter()) {
        assert_eq!(
            peek(after),
            peek(before),
            "thaw resumes the peek exactly where it froze (no mass replay)"
        );
        assert!(
            (after.x - before.x).abs() <= 2 && (after.y - before.y).abs() <= 2,
            "…and resumes in place, within the idle breath ({:?} vs {:?})",
            (after.x, after.y),
            (before.x, before.y)
        );
    }
}

fn synthetic_empty_frame(term: &mut Terminal, input: &mut RenderInput) {
    use aterm_core::terminal::{RenderCell, UnderlineStyle};
    term.cell_frame_into(input, 24, 80);
    let bg = input.default_bg;
    let space = RenderCell {
        ch: ' ',
        fg: [0xD0, 0xD0, 0xD0],
        bg: [(bg >> 16) as u8, (bg >> 8) as u8, bg as u8],
        wide: false,
        emoji_presentation: false,
        text_presentation: false,
        bold: false,
        italic: false,
        underline: UnderlineStyle::None,
        strikethrough: false,
        overline: false,
        underline_color: None,
        overline_color: None,
        script: aterm_core::terminal::Script::Normal,
    };
    for row in &mut input.cells {
        row.clear();
        row.resize(80, space);
    }
}

fn enable_classic_rain(pipeline: &mut EffectsPipeline) {
    pipeline.rain_cfg.output_material = false;
    pipeline.set_matrix_rain_enabled(true);
}

fn rain_until_visible(p: &mut EffectsPipeline, term: &mut Terminal, input: &mut RenderInput) {
    for _ in 0..120 {
        p.note_keystroke();
        p.advance(50.0);
        synthetic_empty_frame(term, input);
        p.apply(term, input, 10, 20);
        if !rain_rows(input).is_empty() {
            return;
        }
    }
    panic!("rain never appeared on an empty 24x80 grid");
}

#[test]
fn output_streak_parks_until_its_exact_settle_deadline() {
    let mut p = EffectsPipeline::new();
    let t0 = p.now();
    let cfg = StreakConfig {
        enabled: true,
        sound: true,
        idle_secs: 10.0,
        ..StreakConfig::default()
    };
    let geom = p.chrome_geom(24, 80, 10, 20);
    let mut streak = OutputStreak::new(17);
    assert!(!streak.note_output(1, &[(4, 0, 40)], t0, false));
    let spawned = t0 + Duration::from_millis(350);
    assert!(streak.note_output(2, &[(4, 0, 40)], spawned, false));
    let mut out = Vec::new();
    let opened = streak.tick(spawned, geom, &cfg, &mut out);
    assert_eq!(
        opened.cue.map(|cue| cue.sound),
        Some(crate::output_streak::StreakSound::Shimmer)
    );

    for step in 1..=4 {
        out.clear();
        streak.tick(
            spawned + Duration::from_millis(250 * step),
            geom,
            &cfg,
            &mut out,
        );
    }
    let parked = spawned + Duration::from_secs(1);
    assert!(!streak.is_active(), "the last visible comet has retired");
    let due = streak
        .next_change_deadline(parked, cfg.idle_secs)
        .expect("the open episode still owes one Settle");
    assert!(due > parked);

    p.clock = parked.duration_since(p.t0);
    p.streak_cfg = cfg;
    p.streak = Some(Box::new(streak));
    match p.wake() {
        Wake::At(at) => assert_eq!(at, due),
        other => panic!("parked episode must own one exact wake, got {other:?}"),
    }

    let fired = due + Duration::from_millis(1);
    p.clock = fired.duration_since(p.t0);
    out.clear();
    let settled = p
        .streak
        .as_deref_mut()
        .expect("streak retained through deadline")
        .tick(fired, geom, &cfg, &mut out);
    assert_eq!(
        settled.cue.map(|cue| cue.sound),
        Some(crate::output_streak::StreakSound::Settle)
    );
    assert!(matches!(p.wake(), Wake::Idle));
}

#[test]
fn the_page_streak_lights_the_nova_channel_through_apply() {
    let mut term = Terminal::new(24, 80);
    let mut input = RenderInput::default();
    let mut p = EffectsPipeline::new();
    p.set_output_streak(true, 1.0, 9, 3, 10, false, 7);
    p.set_output_streak_theme(true, 0x0000_0000, 0x00FF_FFFF, 0x0088_C0D0);
    let mut lit = 0usize;
    for i in 0..40 {
        p.advance(40.0);
        term.process(format!("line {i}\r\n").as_bytes());
        term.cell_frame_into(&mut input, 24, 80);
        p.apply(&mut term, &mut input, 10, 20);
        lit = lit.max(input.nova_add.len());
    }
    assert!(lit > 0, "the streak must actually put light in nova_add");

    p.set_reduced_motion(true);
    for i in 40..80 {
        p.advance(40.0);
        term.process(format!("line {i}\r\n").as_bytes());
        term.cell_frame_into(&mut input, 24, 80);
        p.apply(&mut term, &mut input, 10, 20);
        assert!(
            input.nova_add.is_empty(),
            "reduced motion must retire the streak"
        );
        assert!(!p.is_active(), "a reduced streak must not request frames");
    }
    p.set_reduced_motion(false);
    let mut restored = false;
    for i in 80..120 {
        p.advance(40.0);
        term.process(format!("line {i}\r\n").as_bytes());
        term.cell_frame_into(&mut input, 24, 80);
        p.apply(&mut term, &mut input, 10, 20);
        restored |= !input.nova_add.is_empty();
    }
    assert!(
        restored,
        "clearing reduced motion restores the configured streak"
    );

    p.set_output_streak(true, 0.35, 9, 3, 10, false, 7);
    assert!((p.streak_cfg.intensity - 0.35).abs() < f32::EPSILON);
    p.set_output_streak_reduced_motion(true);
    assert!(
        (p.streak_cfg.intensity - 0.35).abs() < f32::EPSILON,
        "streak reduced-motion gate must not clobber configured intensity"
    );
    p.set_output_streak(true, 0.4, 9, 3, 10, false, 7);
    for i in 120..140 {
        p.advance(40.0);
        term.process(format!("line {i}\r\n").as_bytes());
        term.cell_frame_into(&mut input, 24, 80);
        p.apply(&mut term, &mut input, 10, 20);
        assert!(
            input.nova_add.is_empty(),
            "set_output_streak while reduced must stay dark"
        );
        assert!(!p.is_active(), "reduced streak must stay disarmed");
    }
    p.set_output_streak_reduced_motion(false);
    assert!(
        (p.streak_cfg.intensity - 0.4).abs() < f32::EPSILON,
        "clearing streak reduced-motion restores configured intensity rather than 1.0"
    );
    let mut restored_dim = false;
    for i in 140..180 {
        p.advance(40.0);
        term.process(format!("line {i}\r\n").as_bytes());
        term.cell_frame_into(&mut input, 24, 80);
        p.apply(&mut term, &mut input, 10, 20);
        restored_dim |= !input.nova_add.is_empty();
    }
    assert!(
        restored_dim,
        "clearing streak reduced-motion restores emission"
    );
}

#[test]
fn rain_enabled_emits_and_repeat_apply_is_byte_stable() {
    let mut p = EffectsPipeline::new();
    enable_classic_rain(&mut p);
    assert!(p.matrix_rain_enabled());
    assert!(p.is_active(), "CALM weather holds the engine active");
    let mut term = Terminal::new(24, 80);
    let mut input = RenderInput::default();
    rain_until_visible(&mut p, &mut term, &mut input);
    let quads = rain_rows(&input).clone();
    let adds = input.rain_add.clone();
    let atlas_version = p.rain.as_ref().map(|r| r.atlas_version());
    let fp1 = p.apply(&mut term, &mut input, 10, 20);
    let fp2 = p.apply(&mut term, &mut input, 10, 20);
    assert_ne!(fp1, 0, "nonempty emission folds a nonzero fp");
    assert_eq!(fp1, fp2, "no advance ⇒ stable fold");
    assert_eq!(rain_rows(&input), quads, "byte-stable quads");
    assert_eq!(input.rain_add, adds, "byte-stable halos");
    assert_eq!(
        p.rain.as_ref().map(|r| r.atlas_version()),
        atlas_version,
        "no tick ⇒ no rebake"
    );
}

#[test]
fn rain_only_uses_engine_deadline_while_frame_effects_keep_raf() {
    let mut p = EffectsPipeline::new();
    enable_classic_rain(&mut p);
    assert_eq!(p.next_deadline_ms(), Some(83.0), "CALM rain is 12 Hz");

    p.advance(30.0);
    assert_eq!(
        p.next_deadline_ms(),
        Some(53.0),
        "unrelated redraws retain the partial rain period"
    );

    p.set_cursor_trail(true, 260, 24, None, 0x0050_FA7B);
    let now = p.now();
    p.cursor
        .trail
        .tick(Some((4, 4)), now, &p.trail_cfg, &mut p.cursor.trail_scratch);
    p.advance(1.0);
    let now = p.now();
    p.cursor.trail.note_synthetic_move(now);
    p.cursor
        .trail
        .tick(Some((4, 8)), now, &p.trail_cfg, &mut p.cursor.trail_scratch);
    assert!(
        !p.cursor.trail_scratch.is_empty(),
        "the synthetic motion arm must make the deadline control non-vacuous"
    );
    assert!(p.is_active());
    assert_eq!(
        p.next_deadline_ms(),
        None,
        "a live motion effect still requests display-rAF cadence"
    );
}

#[test]
fn embedded_pipeline_freezes_literal_tape_across_multiline_draft() {
    let mut p = EffectsPipeline::new();
    p.set_matrix_rain_enabled(true);
    let mut term = Terminal::new(24, 80);
    let mut input = RenderInput::default();
    term.process(b"\x1b[HREAL OUTPUT 2468\x1b[24;1H");
    term.cell_frame_into(&mut input, 24, 80);
    p.apply(&mut term, &mut input, 10, 20);

    let initial = p
        .rain
        .as_deref()
        .expect("enabled rain engine")
        .literal_material_chars_for_test()
        .to_vec();
    for required in ['R', 'E', 'A', 'L', 'O', 'U', 'T', '2', '4', '6', '8'] {
        assert!(
            initial.contains(&required),
            "initial tape contains real output glyph {required:?}"
        );
    }

    for row in 10..17 {
        p.note_keystroke();
        term.process(format!("\x1b[{};1H> private zqjx draft 13579", row + 1).as_bytes());
        term.cell_frame_into(&mut input, 24, 80);
        p.apply(&mut term, &mut input, 10, 20);
    }
    let during_edit = p
        .rain
        .as_deref()
        .expect("enabled rain engine")
        .literal_material_chars_for_test();
    assert_eq!(
        during_edit,
        initial.as_slice(),
        "unsent edits freeze the tape"
    );
    for draft_only in ['p', 'r', 'i', 'v', 'a', 't', 'e', 'z', 'q', 'j', 'x'] {
        assert!(
            !during_edit.contains(&draft_only),
            "draft-only glyph {draft_only:?} never enters literal material"
        );
    }

    p.note_matrix_rain_signal(crate::matrix_rain::RainSignal::TurnStart as u32, 4);
    term.process(b"\x1b[2J\x1b[HREAL response z\x1b[24;1H");
    term.cell_frame_into(&mut input, 24, 80);
    p.apply(&mut term, &mut input, 10, 20);
    let after_submit = p
        .rain
        .as_deref()
        .expect("enabled rain engine")
        .literal_material_chars_for_test();
    assert!(
        after_submit.contains(&'z'),
        "TurnStart re-enables sampling for subsequent real output"
    );
}

#[test]
fn live_classic_to_literal_switch_resamples_without_grid_damage() {
    let mut p = EffectsPipeline::new();
    p.set_matrix_rain(
        30,
        6,
        4,
        6,
        None,
        None,
        "matrix",
        None,
        320,
        12,
        false,
        true,
        true,
        false,
        7,
        0x0011_1318,
        0x00D0_D0D0,
    );
    p.set_matrix_rain_enabled(true);
    let mut term = Terminal::new(24, 80);
    term.process(b"\x1b[HREAL output\x1b[24;1H> draft\x1b[24;8H");
    let mut input = RenderInput::default();
    term.cell_frame_into(&mut input, 24, 80);
    p.apply(&mut term, &mut input, 10, 20);
    let epoch = term.damage_epoch();

    p.set_matrix_rain(
        30,
        6,
        4,
        6,
        None,
        None,
        "matrix",
        None,
        320,
        12,
        false,
        true,
        true,
        true,
        7,
        0x0011_1318,
        0x00D0_D0D0,
    );
    term.cell_frame_into(&mut input, 24, 80);
    assert_eq!(input.snapshot_seq, epoch, "fixture has no new grid damage");
    p.apply(&mut term, &mut input, 10, 20);
    let chars = p
        .rain
        .as_deref()
        .expect("enabled rain")
        .literal_material_chars_for_test();
    assert!(chars.contains(&'R') && chars.contains(&'o'));
}

#[test]
fn matrix_rain_theme_updates_cached_and_live_config() {
    let mut p = EffectsPipeline::new();
    p.set_matrix_rain_enabled(true);
    p.set_matrix_rain_theme(0x0012_3456, 0x00AB_CDEF, 0x0013_5799);
    assert_eq!(p.rain_cfg.default_bg, 0x0012_3456);
    assert_eq!(p.rain_cfg.theme_fg, 0x00AB_CDEF);
    assert_eq!(p.rain_cfg.theme_accent, 0x0013_5799);
    let live = p.rain.as_deref().expect("enabled rain").config_for_test();
    assert_eq!(live.default_bg, 0x0012_3456);
    assert_eq!(live.theme_fg, 0x00AB_CDEF);
    assert_eq!(live.theme_accent, 0x0013_5799);
}

#[test]
fn scrollback_reading_skips_scan_and_live_return_rescans() {
    let mut p = EffectsPipeline::new();
    enable_classic_rain(&mut p);
    let mut term = Terminal::new(6, 20);
    for line in 0..20 {
        term.process(format!("line {line}\r\n").as_bytes());
    }
    let mut input = RenderInput::default();
    term.cell_frame_into(&mut input, 6, 20);
    p.apply(&mut term, &mut input, 8, 16);
    let live_epoch = term.damage_epoch();
    assert!(!p.rain.as_deref().unwrap().needs_rescan(live_epoch));

    term.scroll_display(3);
    term.cell_frame_into(&mut input, 6, 20);
    let scrolled_epoch = term.damage_epoch();
    p.apply(&mut term, &mut input, 8, 16);
    assert!(
        p.rain.as_deref().unwrap().needs_rescan(scrolled_epoch),
        "translated scrollback frame is never copied or scanned"
    );

    term.scroll_display(-1000);
    term.cell_frame_into(&mut input, 6, 20);
    let returned_epoch = term.damage_epoch();
    p.apply(&mut term, &mut input, 8, 16);
    assert!(
        !p.rain.as_deref().unwrap().needs_rescan(returned_epoch),
        "returning live rebuilds from the fresh coherent frame"
    );
}

#[test]
fn rain_toggle_off_clears_stale_channels_on_every_off_path() {
    let mut p = EffectsPipeline::new();
    enable_classic_rain(&mut p);
    let mut term = Terminal::new(24, 80);
    let mut input = RenderInput::default();
    rain_until_visible(&mut p, &mut term, &mut input);

    p.set_matrix_rain_enabled(false);
    assert!(!p.is_active(), "dropping the engine disarms the host loop");
    let fp = p.apply(&mut term, &mut input, 10, 20);
    assert_eq!(fp, 0);
    assert!(rain_rows(&input).is_empty());
    assert!(input.rain_add.is_empty());

    p.set_matrix_rain_enabled(true);
    rain_until_visible(&mut p, &mut term, &mut input);
    p.set_matrix_rain_enabled(false);
    p.set_cursor_glow(
        true,
        "lumen",
        crate::cursor_fx::TrailColor::Automatic,
        None,
        260,
        24,
        0.7,
        0.6,
        true,
        0x0050_FA7B,
    );
    p.advance(16.0);
    term.cell_frame_into(&mut input, 24, 80);
    p.apply(&mut term, &mut input, 10, 20);
    assert!(rain_rows(&input).is_empty());
    assert!(input.rain_add.is_empty());
}

#[test]
fn rain_torn_snapshot_clears_channels_and_fresh_frame_resumes() {
    let mut p = EffectsPipeline::new();
    enable_classic_rain(&mut p);
    let mut term = Terminal::new(24, 80);
    let mut input = RenderInput::default();
    rain_until_visible(&mut p, &mut term, &mut input);

    term.process(b"tear"); // grid damage after the snapshot: `input` is stale
    p.apply(&mut term, &mut input, 10, 20);
    assert!(rain_rows(&input).is_empty(), "torn frame: no rain quads");
    assert!(input.rain_add.is_empty(), "torn frame: no rain halos");

    rain_until_visible(&mut p, &mut term, &mut input);
}

#[test]
fn apply_routes_v2_offer_and_drains_companion_impulse_to_resident_pet() {
    let mut p = EffectsPipeline::new();
    p.set_cursor_pet(true, 0x5EED);
    p.set_cursor_glow(
        true,
        "rainbow kitty",
        crate::cursor_fx::TrailColor::Automatic,
        None,
        260,
        24,
        0.9,
        0.6,
        true,
        0x0050_FA7B,
    );
    let mut term = Terminal::new(24, 80);
    term.process(b"\x1b[4;3H");
    let mut input = RenderInput::default();
    for _ in 0..30 {
        p.advance(16.0);
        term.cell_frame_into(&mut input, 24, 80);
        p.apply(&mut term, &mut input, 8, 16);
    }
    assert_eq!(p.companion().brain().pending_v2_offer(), (false, false));

    p.advance(16.0);
    p.note_console_input(PetInputKind::Navigate);
    term.process(b"\x1b[4;30H");
    term.cell_frame_into(&mut input, 24, 80);
    p.apply(&mut term, &mut input, 8, 16);

    assert!(
        p.companion().brain().pending_v2_offer().0,
        "apply must offer v2's meteor arrival perk edge to the resident pet"
    );
    assert!(
        p.cursor_mut().glow.take_companion_impulse().is_none(),
        "apply must drain v2's companion impulse after offering it to the resident pet"
    );
}

/// looks10 L10, THE FLYING HEAD NAMES ITS BODY: nyan's kitty, earned at the
/// ribbon's head, drew no box `trail status` named, so `lab trail-companion`
/// could not hold it off the letters and the caret as it holds the pet. The
/// head notes the body it drew ([`crate::cursor_glow::CursorGlow::note_head_body`]):
/// on the window's canvas the sprite's own box, on a pane's once the pane's
/// sprites are placed in the window grid, moved with them. Negative
/// controls: the next frame's word pass opens with none, and a head drawn
/// at no opacity names none.
#[test]
fn the_flying_head_names_the_body_it_drew() {
    use crate::word_decorations::{EffectGeom, KittyCursorFrame, KittyCursorLayout};
    let g = EffectGeom {
        cell_w: 10,
        cell_h: 20,
        rows: 6,
        cols: 20,
    };
    let cursor = (3u16, 5u16);
    let frame = |alpha: u8| KittyCursorFrame {
        geom: g,
        cursor,
        look: crate::kitty_registry::KittyLook::default(),
        colors: crate::cat_baker::CatColorKey::default(),
        bob: 0.0,
        lift: 0.0,
        alpha,
        pose: crate::kitty_cursor::CatPose::STILL,
        sing: 0.0,
        notes: [None; crate::kitty_sing::MAX_NOTES],
    };
    let mut p = EffectsPipeline::default();
    let draw = |p: &mut EffectsPipeline, ink: CanvasInk, alpha: u8| {
        let placement = p
            .decos_mut()
            .prepare_kitty_cursor_placement(
                KittyCursorLayout {
                    geom: g,
                    cursor,
                    look: crate::kitty_registry::KittyLook::default(),
                    bob: 0.0,
                    lift: 0.0,
                },
                crate::kitty_cursor::CursorCatPlacementFrame {
                    sampled_at: Instant::now(),
                    fold: None,
                    facing_left: false,
                },
                crate::kitty_cursor::CatPose::STILL,
            )
            .expect("a placeable head");
        p.draw_flying_head(ink, frame(alpha), placement)
    };
    let sprite_box = |s: &FreeSprite| (s.x, s.x + i32::from(s.w), s.y, s.y + i32::from(s.h));
    p.open_words();
    assert!(
        draw(&mut p, CanvasInk::Window, 255).is_some(),
        "the head draws"
    );
    let body = p
        .free_scratch
        .last()
        .map(sprite_box)
        .expect("its body sprite");
    assert_eq!(p.cursor().glow.head_body_window_box(), Some(body));
    p.open_words();
    assert_eq!(
        p.cursor().glow.head_body_window_box(),
        None,
        "control: the next word pass opens with no head"
    );
    p.clear_pane_sprites();
    assert!(
        draw(&mut p, CanvasInk::Pane, 255).is_some(),
        "the pane's head draws"
    );
    assert_eq!(
        p.cursor().glow.head_body_window_box(),
        None,
        "a pane's head waits for its place"
    );
    p.place_pane_sprites(crate::pane::PanePlace {
        row_off: 2,
        col_off: 30,
        rows: 6,
        cols: 20,
        win_rows: 10,
        win_cols: 60,
        cell_w: 10,
        cell_h: 20,
    });
    let placed = p
        .free_scratch
        .last()
        .map(sprite_box)
        .expect("the placed body");
    assert_eq!(p.cursor().glow.head_body_window_box(), Some(placed));
    assert_eq!((placed.0 - body.0, placed.2 - body.2), (300, 40));
    p.open_words();
    let _ = draw(&mut p, CanvasInk::Window, 0);
    assert_eq!(
        p.cursor().glow.head_body_window_box(),
        None,
        "control: a head at no opacity names no body"
    );
    p.advance(1e-300);
    p.advance(1e300);
    let _ = p.now();
    let _ = p.wake();
}
