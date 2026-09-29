// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Window-owned artwork: cache reuse, replacement and teardown on both backends.
//! The bounded model describes two owners, each with two possible snapshots.
//! Its mutant reproduces the former renderer-wide slot evicting the other owner.

use super::{GpuRenderer, RenderInput, SceneAtlas, SpriteTex, WindowGpu};
use aterm_core::render::{FreeSampler, FreeSprite, FreeZ, SpriteQuad};
use aterm_core::terminal::Terminal;
use aterm_spec::derive::Model;
use aterm_spec::interp::State;
use std::sync::Arc;

fn ownership_model() -> Model {
    aterm_spec::ty_model! {
        WindowArtworkOwnership {
            const Buggy = 0;
            var a = 0;
            var b = 0;
            var cached_a = 0;
            var cached_b = 0;
            action PaintA1 when (Buggy <= 1) {
                a = 1; cached_a = 1;
                cached_b = if Buggy == 1 { 0 } else { cached_b };
            }
            action PaintA2 when (Buggy <= 1) {
                a = 2; cached_a = 2;
                cached_b = if Buggy == 1 { 0 } else { cached_b };
            }
            action PaintB1 when (Buggy <= 1) {
                b = 1; cached_b = 1;
                cached_a = if Buggy == 1 { 0 } else { cached_a };
            }
            action PaintB2 when (Buggy <= 1) {
                b = 2; cached_b = 2;
                cached_a = if Buggy == 1 { 0 } else { cached_a };
            }
            action ClearA when (Buggy <= 1) { a = 0; cached_a = 0; }
            action CloseB when (Buggy <= 1) { b = 0; cached_b = 0; }
            invariant Owned: cached_a == a && cached_b == b;
        }
    }
}

#[test]
fn window_atlas_ownership_model_proves_and_catches_shared_slot() {
    aterm_spec::verify::prove_and_catch_scalar(&ownership_model(), "window artwork ownership");
}

fn blank_input() -> RenderInput {
    let mut term = Terminal::new(4, 12);
    term.process(b"\x1b[?25l");
    term.cell_frame(4, 12)
}

fn artwork_input(gpu: &GpuRenderer, tag: u8) -> RenderInput {
    let mut input = blank_input();
    let (cw, ch) = gpu.cell_size();
    let (width, height) = ((12 * cw) as u32, (4 * ch) as u32);
    let pixel = [tag, 80, 255 - tag, 255];
    // Both windows deliberately use the same version and dimensions. The
    // immutable source identity, not either value, distinguishes their pixels.
    let atlas = Arc::new(SceneAtlas {
        width,
        height,
        rgba: pixel.repeat((width * height) as usize),
        version: 1,
    });
    input.free_atlas = Some(atlas.clone());
    input.cat_atlas = Some(atlas.clone());
    input.rain_atlas = Some(atlas.clone());
    input.wallpaper = Some(atlas);
    input.free_sprites.push(FreeSprite {
        x: 2,
        y: 2,
        w: 8,
        h: 8,
        ax: 0,
        ay: 0,
        aw: 8,
        ah: 8,
        tint: 0x00ff_ffff,
        alpha: 255,
        flip_x: false,
        z: FreeZ::OverText,
        sampler: FreeSampler::Nearest,
    });
    let quad = SpriteQuad {
        row: 1,
        x: 16,
        y: ch as u16,
        w: 8,
        h: 8,
        ax: 0,
        ay: 0,
        aw: 8,
        ah: 8,
        tint: 0x00ff_ffff,
        alpha: 255,
        flip_x: false,
    };
    input.cat_quads.push(quad);
    input.rain_quads.push(SpriteQuad {
        row: 2,
        y: (2 * ch) as u16,
        ..quad
    });

    // Exercise the fifth window-owned atlas: the packed inline-image plane.
    let mut png = Vec::new();
    {
        let mut enc = aterm_png::Encoder::new(&mut png, 1, 1);
        enc.set_color(aterm_png::ColorType::Rgba);
        enc.set_depth(aterm_png::BitDepth::Eight);
        enc.write_header()
            .unwrap()
            .write_image_data(&pixel)
            .unwrap();
    }
    input.images[3].push((
        0,
        aterm_core::grid::extra::ImageRef {
            image: Arc::new(aterm_core::grid::extra::ImageData {
                bytes: png,
                format: aterm_core::grid::extra::ImageFormat::Png,
                cols: 1,
                rows: 1,
                z_index: 0,
                band_lift_px: 0,
                scaling: aterm_core::grid::extra::ImageScaling::Fit,
                source_rect: None,
            }),
            cell_row: 0,
            cell_col: 0,
            kitty: None,
        },
    ));
    input
}

fn sprites(win: &WindowGpu) -> [&SpriteTex; 4] {
    [
        &win.cat_atlas,
        &win.free_atlas,
        &win.rain_atlas,
        &win.wallpaper_tex,
    ]
    .map(|s| s.as_deref().expect("fixture must build every sprite atlas"))
}

/// Retain the actual GPU handles while alternating frames: a spurious rebuild
/// cannot pass by reusing the address of a texture just freed by the test.
struct Handles {
    #[cfg(wgpu_arm)]
    wgpu: [wgpu::BindGroup; 5],
    #[cfg(target_os = "macos")]
    metal: Vec<crate::metal::resources::SealedTexture>,
}

impl Handles {
    fn capture(win: &WindowGpu, native: bool) -> Self {
        let _ = native;
        Self {
            #[cfg(wgpu_arm)]
            wgpu: {
                let s = sprites(win);
                [
                    s[0].bind.clone(),
                    s[1].bind.clone(),
                    s[2].bind.clone(),
                    s[3].bind.clone(),
                    win.image_plane.as_ref().unwrap().bind.clone(),
                ]
            },
            #[cfg(target_os = "macos")]
            metal: if native {
                sprites(win)
                    .into_iter()
                    .map(|s| {
                        s.metal_atlas
                            .get()
                            .expect("native sprite upload")
                            .tex
                            .clone_handle()
                    })
                    .chain(std::iter::once(
                        win.image_plane
                            .as_ref()
                            .unwrap()
                            .metal_atlas
                            .as_ref()
                            .expect("native image upload")
                            .tex
                            .clone_handle(),
                    ))
                    .collect()
            } else {
                Vec::new()
            },
        }
    }

    fn assert_retained(&self, win: &WindowGpu, native: bool) {
        let actual = Self::capture(win, native);
        #[cfg(wgpu_arm)]
        assert_eq!(
            self.wgpu, actual.wgpu,
            "wgpu must reuse every window-owned atlas"
        );
        #[cfg(target_os = "macos")]
        assert_eq!(
            self.metal.iter().map(|t| t.obj().id()).collect::<Vec<_>>(),
            actual
                .metal
                .iter()
                .map(|t| t.obj().id())
                .collect::<Vec<_>>(),
            "Metal must reuse every window-owned atlas"
        );
    }
}

fn observed(model_state: &State, windows: &[WindowGpu; 2]) -> State {
    let mut state = model_state.clone();
    for (name, win) in ["cached_a", "cached_b"].into_iter().zip(windows) {
        let tags = [
            &win.cat_atlas,
            &win.free_atlas,
            &win.rain_atlas,
            &win.wallpaper_tex,
        ]
        .map(|slot| {
            slot.as_ref()
                .map_or(0, |s| if s.src.rgba[0] == 40 { 1 } else { 2 })
        });
        assert!(tags.iter().all(|tag| *tag == tags[0]));
        state.insert(name, tags[0]);
    }
    state
}

fn run_ownership_replay(native: bool) {
    let mut gpu = match GpuRenderer::new(18.0, aterm_render::Theme::default()) {
        Ok(gpu) => gpu,
        Err(e) => {
            crate::stderr_line!("SKIP: window atlas GPU fixture unavailable: {e}");
            return;
        }
    };
    #[cfg(target_os = "macos")]
    if !native {
        gpu.disarm_metal_for_test();
    }
    gpu.set_bloom(false);
    gpu.set_shimmer(false);
    let mut windows = [WindowGpu::new(), WindowGpu::new()];
    let inputs = [artwork_input(&gpu, 40), artwork_input(&gpu, 220)];
    let weak = inputs
        .each_ref()
        .map(|input| Arc::downgrade(input.free_atlas.as_ref().unwrap()));
    let model = ownership_model();
    let mut state = model.init_state();
    let mut pixels: [Option<Vec<u32>>; 2] = [None, None];
    let mut handles: [Option<Handles>; 2] = [None, None];
    for (idx, action) in [
        (0, "PaintA1"),
        (1, "PaintB2"),
        (0, "PaintA1"),
        (1, "PaintB2"),
        (0, "PaintA1"),
        (1, "PaintB2"),
    ] {
        let prior = state.clone();
        assert!(model.fire(action, &mut state));
        let frame = gpu
            .try_render_input(&mut windows[idx], &inputs[idx], None)
            .unwrap();
        #[cfg(target_os = "macos")]
        assert_eq!(
            gpu.last_frame_arm_metal, native,
            "exercise the requested backend"
        );
        let actual = observed(&state, &windows);
        assert_eq!(actual, state);
        assert!(aterm_spec::interp::admits(&model, &prior, &actual).is_some());
        match &pixels[idx] {
            Some(first) => assert_eq!(&frame.pixels, first, "alternating windows changed pixels"),
            None => pixels[idx] = Some(frame.pixels),
        }
        match &handles[idx] {
            Some(first) => first.assert_retained(&windows[idx], native),
            None => handles[idx] = Some(Handles::capture(&windows[idx], native)),
        }
    }
    assert_ne!(
        pixels[0], pixels[1],
        "different atlas pixels must reach the frame"
    );
    let mut evicted = state.clone();
    evicted.insert("cached_a", 0);
    assert!(
        aterm_spec::interp::admits(&model, &state, &evicted).is_none(),
        "the old global-slot eviction must fail conformance"
    );
    // A fresh source with the same version/dimensions must replace A, without
    // disturbing B's resident textures. Its pixels intentionally match B.
    let replacement = artwork_input(&gpu, 220);
    let replacement_weak = Arc::downgrade(replacement.free_atlas.as_ref().unwrap());
    let prior = state.clone();
    let frame = gpu
        .try_render_input(&mut windows[0], &replacement, None)
        .unwrap();
    assert_eq!(Some(frame.pixels), pixels[1]);
    assert!(model.fire("PaintA2", &mut state));
    assert_eq!(observed(&state, &windows), state);
    assert!(aterm_spec::interp::admits(&model, &prior, &state).is_some());
    handles[1]
        .as_ref()
        .unwrap()
        .assert_retained(&windows[1], native);
    drop(handles);
    drop(inputs);
    assert!(
        weak[0].upgrade().is_none(),
        "replacement must release the old source"
    );
    drop(replacement);

    // Disable artwork without closing A, then close B while keeping the shared
    // renderer alive. Neither action may leave a renderer-owned source pin.
    let prior = state.clone();
    gpu.try_render_input(&mut windows[0], &blank_input(), None)
        .unwrap();
    assert!(model.fire("ClearA", &mut state));
    assert_eq!(observed(&state, &windows), state);
    assert!(aterm_spec::interp::admits(&model, &prior, &state).is_some());
    assert!(windows[0].image_plane.is_none());
    assert!(
        replacement_weak.upgrade().is_none(),
        "disabled artwork source must be released"
    );
    let prior = state.clone();
    windows[1] = WindowGpu::new();
    assert!(model.fire("CloseB", &mut state));
    assert_eq!(observed(&state, &windows), state);
    assert!(aterm_spec::interp::admits(&model, &prior, &state).is_some());
    assert!(
        weak[1].upgrade().is_none(),
        "closed window's source must be released"
    );
    // Reopening does not inherit the closed window's cached pixels/identity.
    for _ in 0..3 {
        let input = artwork_input(&gpu, 40);
        let source = Arc::downgrade(input.free_atlas.as_ref().unwrap());
        let prior = state.clone();
        let frame = gpu.try_render_input(&mut windows[1], &input, None).unwrap();
        assert_eq!(Some(frame.pixels), pixels[0]);
        assert!(model.fire("PaintB1", &mut state));
        assert_eq!(observed(&state, &windows), state);
        assert!(aterm_spec::interp::admits(&model, &prior, &state).is_some());
        drop(input);
        windows[1] = WindowGpu::new();
        let prior = state.clone();
        assert!(model.fire("CloseB", &mut state));
        assert_eq!(observed(&state, &windows), state);
        assert!(aterm_spec::interp::admits(&model, &prior, &state).is_some());
        assert!(source.upgrade().is_none());
    }
    #[cfg(target_os = "macos")]
    if native {
        let live = gpu.metal_arm.as_mut().unwrap().live_mut();
        for atlas in [
            super::DrawAtlas::Cat,
            super::DrawAtlas::Free,
            super::DrawAtlas::Rain,
            super::DrawAtlas::Wallpaper,
            super::DrawAtlas::Image,
        ] {
            assert!(
                live.atlases[atlas as usize].is_none(),
                "window artwork must not be retained in shared renderer slots"
            );
        }
    }
}

#[test]
fn window_atlas_wgpu_reuses_distinct_artwork_and_releases_closed_windows() {
    run_ownership_replay(false);
}

#[cfg(target_os = "macos")]
#[test]
fn window_atlas_metal_reuses_distinct_artwork_and_releases_closed_windows() {
    run_ownership_replay(true);
}

/// Two owners can select either immutable snapshot. The physical upload count
/// is the number of distinct live sources, including after one owner leaves.
/// The mutant is the old per-window allocation of an already resident source.
fn sharing_model() -> Model {
    aterm_spec::ty_model! {
        SharedWindowArtwork {
            const Buggy = 0;
            var a = 0;
            var b = 0;
            var uploads = 0;
            action PaintA1 when (Buggy <= 1) {
                a = 1;
                uploads = if b == 0 { 1 } else {
                    if b == 1 && Buggy == 0 { 1 } else { 2 }
                };
            }
            action PaintA2 when (Buggy <= 1) {
                a = 2;
                uploads = if b == 0 { 1 } else {
                    if b == 2 && Buggy == 0 { 1 } else { 2 }
                };
            }
            action PaintB1 when (Buggy <= 1) {
                b = 1;
                uploads = if a == 0 { 1 } else {
                    if a == 1 && Buggy == 0 { 1 } else { 2 }
                };
            }
            action PaintB2 when (Buggy <= 1) {
                b = 2;
                uploads = if a == 0 { 1 } else {
                    if a == 2 && Buggy == 0 { 1 } else { 2 }
                };
            }
            action ClearA when (Buggy <= 1) {
                a = 0; uploads = if b == 0 { 0 } else { 1 };
            }
            action CloseB when (Buggy <= 1) {
                b = 0; uploads = if a == 0 { 0 } else { 1 };
            }
            invariant Shared: uploads == if a == 0 {
                if b == 0 { 0 } else { 1 }
            } else {
                if b == 0 || a == b { 1 } else { 2 }
            };
        }
    }
}

#[test]
fn shared_window_atlas_model_proves_and_catches_duplicate_uploads() {
    aterm_spec::verify::prove_and_catch_scalar(&sharing_model(), "shared window artwork");
}

fn shared_observation(model_state: &State, windows: &[WindowGpu; 2]) -> State {
    let mut state = model_state.clone();
    let mut uploads: Vec<&Arc<SpriteTex>> = Vec::new();
    for (name, win) in ["a", "b"].into_iter().zip(windows) {
        let slots = [
            &win.cat_atlas,
            &win.free_atlas,
            &win.rain_atlas,
            &win.wallpaper_tex,
        ];
        let mut tags = Vec::new();
        for slot in slots {
            tags.push(slot.as_ref().map_or(0, |texture| {
                if !uploads.iter().any(|prior| Arc::ptr_eq(prior, texture)) {
                    uploads.push(texture);
                }
                if texture.src.rgba[0] == 40 { 1 } else { 2 }
            }));
        }
        assert!(tags.iter().all(|tag| *tag == tags[0]));
        state.insert(name, tags[0]);
    }
    state.insert("uploads", uploads.len() as i64);
    state
}

fn run_sharing_replay(native: bool) {
    let mut gpu = match GpuRenderer::new(18.0, aterm_render::Theme::default()) {
        Ok(gpu) => gpu,
        Err(e) => {
            crate::stderr_line!("SKIP: shared artwork GPU fixture unavailable: {e}");
            return;
        }
    };
    #[cfg(target_os = "macos")]
    if !native {
        gpu.disarm_metal_for_test();
    }
    gpu.set_bloom(false);
    gpu.set_shimmer(false);
    let mut windows = [WindowGpu::new(), WindowGpu::new()];
    let inputs = [artwork_input(&gpu, 40), artwork_input(&gpu, 220)];
    let model = sharing_model();
    let mut state = model.init_state();
    let mut pixels: [Option<Vec<u32>>; 2] = [None, None];
    let mut first_upload = None;
    let mut first_handles = None;
    for (owner, source, action) in [
        (0, Some(0), "PaintA1"),
        (1, Some(0), "PaintB1"),
        (0, Some(1), "PaintA2"),
        (1, Some(0), "PaintB1"),
        (1, Some(1), "PaintB2"),
        (0, None, "ClearA"),
        (1, None, "CloseB"),
        (0, Some(0), "PaintA1"),
        (1, Some(0), "PaintB1"),
        (1, None, "CloseB"),
        (0, None, "ClearA"),
    ] {
        let prior = state.clone();
        assert!(model.fire(action, &mut state));
        match source {
            Some(source) => {
                let frame = gpu
                    .try_render_input(&mut windows[owner], &inputs[source], None)
                    .unwrap();
                #[cfg(target_os = "macos")]
                assert_eq!(gpu.last_frame_arm_metal, native);
                match &pixels[source] {
                    Some(expected) => assert_eq!(&frame.pixels, expected),
                    None => pixels[source] = Some(frame.pixels),
                }
                let handles = Handles::capture(&windows[owner], native);
                #[cfg(wgpu_arm)]
                assert!(
                    handles.wgpu[..4]
                        .iter()
                        .all(|bind| *bind == handles.wgpu[0])
                );
                #[cfg(target_os = "macos")]
                if native {
                    assert!(
                        handles.metal[..4]
                            .iter()
                            .all(|tex| tex.obj().id() == handles.metal[0].obj().id())
                    );
                }
                match first_upload.as_ref() {
                    None => {
                        first_upload =
                            Some(Arc::downgrade(windows[owner].free_atlas.as_ref().unwrap()));
                        first_handles = Some(handles);
                    }
                    Some(upload) if source == 0 && upload.upgrade().is_some() => {
                        // The second owner and its unchanged repaint must use the
                        // very same native texture/wgpu bind, not just equal pixels.
                        let first = first_handles.as_ref().unwrap();
                        #[cfg(wgpu_arm)]
                        assert_eq!(first.wgpu[..4], handles.wgpu[..4]);
                        #[cfg(target_os = "macos")]
                        assert_eq!(
                            first
                                .metal
                                .iter()
                                .take(4)
                                .map(|t| t.obj().id())
                                .collect::<Vec<_>>(),
                            handles
                                .metal
                                .iter()
                                .take(4)
                                .map(|t| t.obj().id())
                                .collect::<Vec<_>>(),
                        );
                    }
                    Some(_) => {}
                }
            }
            None if owner == 0 => {
                gpu.try_render_input(&mut windows[owner], &blank_input(), None)
                    .unwrap();
            }
            None => windows[owner] = WindowGpu::new(),
        }
        let observed = shared_observation(&state, &windows);
        assert_eq!(observed, state);
        assert!(aterm_spec::interp::admits(&model, &prior, &observed).is_some());
        if action == "PaintB2" {
            assert!(
                first_upload.as_ref().unwrap().upgrade().is_none(),
                "replacing the last owner frees the shared upload even while source and GPU handles survive"
            );
        }
    }
    assert_ne!(
        pixels[0], pixels[1],
        "the source pixels must reach the frame"
    );
    let mut shared = model.init_state();
    assert!(model.fire("PaintA1", &mut shared));
    assert!(model.fire("PaintB1", &mut shared));
    let mut duplicate = shared.clone();
    duplicate.insert("uploads", 2);
    assert!(
        aterm_spec::interp::admits(&model, &shared, &duplicate).is_none(),
        "the former duplicate allocation must be rejected by conformance"
    );
    assert!(
        gpu.sprite_textures
            .recent
            .iter()
            .all(|entry| entry.upgrade().is_none())
    );

    // Overflow only forgets a sharing candidate: it cannot evict a window's
    // resident texture. Repeated distinct sources also bound dead metadata.
    let mut owners = Vec::new();
    let keep = inputs[0].free_atlas.as_ref().unwrap().clone();
    for _ in 0..super::SPRITE_TEXTURE_LOOKUP_LIMIT + 3 {
        let mut win = WindowGpu::new();
        let input = RenderInput {
            free_atlas: Some(Arc::new(SceneAtlas {
                width: 1,
                height: 1,
                rgba: vec![40, 80, 215, 255],
                version: 1,
            })),
            ..RenderInput::default()
        };
        gpu.ensure_free_atlas(&mut win, &input);
        owners.push(win);
        assert!(gpu.sprite_textures.recent.len() <= super::SPRITE_TEXTURE_LOOKUP_LIMIT);
    }
    let first = owners[0].free_atlas.as_ref().unwrap().clone();
    let first_source = Arc::downgrade(&first.src);
    let same = RenderInput {
        free_atlas: Some(first.src.clone()),
        ..RenderInput::default()
    };
    gpu.ensure_free_atlas(&mut owners[0], &same);
    assert!(Arc::ptr_eq(owners[0].free_atlas.as_ref().unwrap(), &first));
    drop(same);
    drop(first);
    drop(owners);
    assert!(first_source.upgrade().is_none());
    // A new source triggers dead-weak pruning; no old source or upload survives.
    assert!(gpu.sprite_textures.find(&keep).is_none());
    assert!(gpu.sprite_textures.recent.is_empty());
}

#[test]
fn window_atlas_wgpu_shares_sources_and_releases_the_last_owner() {
    run_sharing_replay(false);
}

#[cfg(target_os = "macos")]
#[test]
fn window_atlas_metal_shares_sources_and_releases_the_last_owner() {
    run_sharing_replay(true);
}
