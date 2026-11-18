// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn spec(px: f32, scale: f32, done: u32, total: u32) -> LockupSpec {
    LockupSpec {
        road: true,
        px,
        scale,
        alab: true,
        dark: dark(),
        done,
        total,
        pose: CatPose::Sit,
    }
}

fn roles() -> Roles {
    Roles::from_theme(aterm_render::Theme::default())
}

/// Whether the test's page ([`roles`]) paints on a dark ground.
fn dark() -> bool {
    crate::native_appearance::surface_is_dark(super::super::marks::page_ground(&roles()))
}

/// What a lockup paints for `spec` in a lockup `width` wide with its own images (baked).
fn baked_paint(spec: LockupSpec, width: f32) -> LockupPaint {
    let wanted = spec.wanted(width);
    for (art, w, h) in wanted.keys() {
        art.keep(w, h, art.bake(w, h).into());
    }
    LockupPaint {
        spec,
        images: Some(wanted),
        pending: false,
    }
}

#[test]
fn lockup_values_round_trip() {
    for value in [
        spec(22.0, 2.0, 11, 16),
        LockupSpec {
            road: false,
            alab: false,
            dark: !dark(),
            pose: CatPose::Loaf,
            ..spec(17.0, 1.0, 0, 0)
        },
        LockupSpec {
            pose: CatPose::Worried,
            ..spec(19.0, 1.5, 3, 9)
        },
    ] {
        assert_eq!(LockupSpec::parse(Some(&value.value())), value);
        for images in [
            None,
            Some(Baked {
                spec: LockupSpec {
                    pose: CatPose::Walk,
                    ..value
                },
                width: 640.5,
            }),
        ] {
            for pending in [false, true] {
                let paint = LockupPaint {
                    spec: value,
                    images,
                    pending,
                };
                assert_eq!(LockupPaint::parse(Some(&paint.value())), paint);
            }
        }
    }
    // Unreadable numbers fall back; a count past the total is the total.
    let read = LockupSpec::parse(Some("road,NaN,0,1,1,20,16,99"));
    assert_eq!(
        (read.px, read.scale, read.done, read.total),
        (22.0, 2.0, 16, 16)
    );
    assert_eq!(read.pose, CatPose::ALL[0]);
}

/// The images `paint_lockup` draws in `rect` for `paint`, in order, and their device sizes.
fn images_of(paint: LockupPaint, rect: LogicalRect) -> Vec<(ImageArt, LogicalRect, (u32, u32))> {
    let mut prims = Vec::new();
    paint_lockup(&mut prims, rect, Some(&paint.value()), &roles());
    prims
        .iter()
        .filter_map(|prim| match prim {
            DrawPrim::Image {
                x,
                y,
                w,
                h,
                art,
                device,
                ..
            } => Some((*art, LogicalRect::new(*x, *y, *w, *h), *device)),
            _ => None,
        })
        .collect()
}

/// The images `paint_lockup` draws for `spec` in `rect`, its own images baked.
fn images(value: &LockupSpec, rect: LogicalRect) -> Vec<(ImageArt, LogicalRect)> {
    images_of(baked_paint(*value, rect.width), rect)
        .into_iter()
        .map(|(art, at, _)| (art, at))
        .collect()
}

/// THE ROAD STARTS AFTER THE WORDS AND THE CAT RIDES IT TO THE RUN'S PROGRESS (§4.5): the road's
/// image starts past the words, the cat's feet stand at `p` of their way along it, and nothing
/// the cat draws reaches back over the words. NEGATIVE CONTROL: as steps finish, the cat moves.
#[test]
fn the_cat_rides_the_road_from_the_words_to_the_far_end_as_steps_finish() {
    let rect = LogicalRect::new(24.0, 10.0, 852.0, 170.0);
    let start = spec(22.0, 2.0, 0, 16);
    let words_end = rect.x + start.words_end();
    let mut cats = Vec::new();
    for done in [0, 8, 16] {
        let value = spec(22.0, 2.0, done, 16);
        let drawn = images(&value, rect);
        assert_eq!(drawn.len(), 2, "a road and a cat");
        let (ImageArt::Road(road), road_at) = drawn[0] else {
            panic!("the road first: {drawn:?}");
        };
        let (ImageArt::Cat(_), cat_at) = drawn[1] else {
            panic!("then the cat: {drawn:?}");
        };
        assert!(road_at.x > words_end, "the road starts past the words");
        assert!(
            road_at.right() <= rect.right() + 0.5,
            "and ends inside the lockup"
        );
        // The cat's feet, in points, against where the road says they stand.
        let unit = road_at.height / design::ROAD_H;
        let (fx, _) = road.road().feet();
        let feet = cat_at.x + design::CAT_FEET.0 * unit;
        let want = road_at.x + (fx - design::ROAD_X) * unit;
        assert!(
            (feet - want).abs() <= 1.0,
            "{done}: feet {feet}, want {want}"
        );
        // Whatever the cat's box overlaps of the words' line, it draws nothing there.
        let words_line = LogicalRect::new(
            rect.x,
            rect.y + (design::BASELINE - 20.0) * unit,
            words_end - rect.x,
            20.0 * unit,
        );
        if let Some(cross) = words_line.intersect(cat_at) {
            let part = crate::widget::ArtSpan {
                left: (cross.x - cat_at.x) / cat_at.width,
                top: (cross.y - cat_at.y) / cat_at.height,
                right: (cross.right() - cat_at.x) / cat_at.width,
                bottom: (cross.bottom() - cat_at.y) / cat_at.height,
            };
            assert_eq!(
                drawn[1].0.mask().drawn(part),
                None,
                "{done}: the cat draws over the words"
            );
        }
        cats.push(feet);
    }
    assert!(cats[0] < cats[1] && cats[1] < cats[2], "{cats:?}");
    assert!(
        ((cats[1] - cats[0]) - (cats[2] - cats[1])).abs() < 1.5,
        "halfway is halfway"
    );
}

/// The lockup's images stand on whole device pixels at the size they are baked at, and the
/// view bakes ahead exactly what the painter draws.
#[test]
fn the_images_sit_on_whole_device_pixels_and_match_what_is_baked_ahead() {
    for (px, scale) in [
        (17.0, 1.0),
        (22.0, 1.0),
        (22.0, 1.5),
        (22.0, 2.0),
        (22.0, 3.0),
    ] {
        let value = spec(px, scale, 5, 16);
        let width = 700.0;
        let rect = LogicalRect::new(16.0, 8.0, width, value.needed_height());
        let drawn = images_of(baked_paint(value, width), rect);
        assert_eq!(drawn.len(), 2);
        let device = |v: f32| v * scale;
        for (_, at, _) in &drawn {
            for v in [at.x, at.y, at.width, at.height] {
                assert!(
                    (device(v) - device(v).round()).abs() < 0.01,
                    "{px} {scale}: {at:?}"
                );
            }
        }
        let keys = value.wanted(width).keys();
        for (art, at, size) in &drawn {
            let key = (
                *art,
                device(at.width).round() as u32,
                device(at.height).round() as u32,
            );
            assert_eq!((key.1, key.2), *size, "{px} {scale}: drawn at its size");
            assert!(keys.contains(&key), "{px} {scale}: {key:?} is baked ahead");
        }
        assert!(value.least_width() > value.words_end());
        assert!(
            value.needed_height() > px * 6.0,
            "the design's box: {}",
            value.needed_height()
        );
    }
}

/// A new step of the run never waits on a bake: the road and the cat painted last, laid out
/// after the same words at the same size, stay until the step's own are kept, and images baked
/// for the other ground never stand in (new words or a new size are
/// `new_words_or_a_new_size_paint_the_words_alone_until_their_road_lands`). NEGATIVE CONTROL:
/// once the step's own are kept, they draw.
#[test]
fn a_new_step_waits_for_its_bakes_and_the_last_one_stays_meanwhile() {
    let width = 300.0;
    let last = LockupSpec {
        pose: CatPose::Walk,
        ..spec(9.0, 1.0, 3, 8)
    }
    .wanted(width);
    let wanted = LockupSpec {
        pose: CatPose::LookUp,
        ..spec(9.0, 1.0, 4, 8)
    };
    let keep = |baked: Baked| {
        for (art, w, h) in baked.keys() {
            art.keep(w, h, art.bake(w, h).into());
        }
    };
    // Nothing kept: the words alone.
    assert_eq!(wanted.paint(width, Some(last)).images, None);
    keep(last);
    assert_eq!(wanted.paint(width, Some(last)).images, Some(last));
    // Its words are the moment's own all along.
    assert_eq!(wanted.paint(width, Some(last)).spec, wanted);
    keep(wanted.wanted(width));
    assert_eq!(
        wanted.paint(width, Some(last)).images,
        Some(wanted.wanted(width))
    );
    // Images baked for the other ground are not drawn on this one.
    let other = LockupSpec {
        dark: !wanted.dark,
        ..wanted
    };
    assert_eq!(other.paint(width, Some(last)).images, None);
}

/// THE HELD ROAD STARTS WHERE THESE WORDS END (§4.5): while a moment's images bake, the road and
/// the cat painted last stand in for them only where they were laid out after the same words at
/// the same size. A moment whose words grew (`aterm` became `aterm · ALab` mid-run, as the
/// Secrets row came into place) or are set a size up (a resize past a size step) paints its
/// words alone until its own images land: the last road starts where the old words ended, inside
/// the new ones. NEGATIVE CONTROL: the next step of the same words at the same size holds the
/// last road and cat.
#[test]
fn new_words_or_a_new_size_paint_the_words_alone_until_their_road_lands() {
    let width = 852.0;
    let laid = |alab, px| LockupSpec {
        alab,
        ..spec(px, 2.0, 5, 16)
    };
    let step = |spec: LockupSpec| LockupSpec { done: 6, ..spec };
    let before = laid(false, 16.0);
    let last = baked_paint(before, width).images.expect("baked");
    let next = step(before);
    assert!(
        !next.wanted(width).ready(),
        "the next step is not baked yet"
    );
    assert_eq!(
        next.paint(width, Some(last)).images,
        Some(last),
        "control: the same words at the same size hold the last road"
    );
    for now in [step(laid(true, 16.0)), step(laid(false, 22.0))] {
        assert!(!now.wanted(width).ready(), "{now:?}: not baked yet");
        assert!(
            last.spec.road_x() < now.words_end(),
            "{now:?}: the last road starts at {}, inside these words, which end at {}",
            last.spec.road_x(),
            now.words_end()
        );
        let paint = now.paint(width, Some(last));
        assert_eq!(paint.images, None, "{now:?}: the words alone");
        assert!(paint.pending, "{now:?}: while its own images bake");
        let own = baked_paint(now, width).images;
        assert_eq!(
            now.paint(width, Some(last)).images,
            own,
            "{now:?}: its own images draw once kept"
        );
    }
}

/// A LIVE RESIZE AND A NEW DISPLAY NEVER BAKE WHERE THEY PAINT (§4.5): a lockup grown wider or
/// narrower at the same words' size, or on a display of another scale, whose own images are not
/// baked yet paints the road and the cat it painted last, one to one as they were baked, clipped
/// to the node; painting that frame keeps nothing new and leaves the new size unbaked (that the
/// raster draws an image not kept as nothing, and bakes none, is `tray_raster`'s
/// `an_image_not_kept_draws_nothing_and_bakes_nothing`). NEGATIVE CONTROL: the held road draws
/// (its pixels show over the ground), and once the new size's images are kept they draw.
#[test]
fn a_width_or_scale_change_paints_the_last_images_and_never_bakes() {
    let before = spec(22.0, 2.0, 5, 16);
    let width = 700.0;
    let last = baked_paint(before, width).images.expect("baked");
    for now in [
        // Wider, the road a step longer.
        (spec(22.0, 2.0, 5, 16), 900.0),
        // Narrower.
        (spec(22.0, 2.0, 5, 16), 560.0),
        // The window moved to a 1x display.
        (spec(22.0, 1.0, 5, 16), 700.0),
    ] {
        let (spec_now, width_now) = now;
        let wanted = spec_now.wanted(width_now);
        assert!(!wanted.ready(), "{now:?}: the new size is not baked yet");
        let kept = crate::widget::images_kept_bytes();
        let paint = spec_now.paint(width_now, Some(last));
        assert_eq!(
            paint.images,
            Some(last),
            "{now:?}: the last images are held"
        );
        let rect = LogicalRect::new(24.0, 8.0, width_now, spec_now.needed_height());
        let mut prims = Vec::new();
        paint_lockup(&mut prims, rect, Some(&paint.value()), &roles());
        let drawn: Vec<(ImageArt, (u32, u32))> = prims
            .iter()
            .filter_map(|prim| match prim {
                DrawPrim::Image { art, device, .. } => Some((*art, *device)),
                _ => None,
            })
            .collect();
        assert_eq!(
            drawn,
            last.keys().map(|(art, w, h)| (art, (w, h))).to_vec(),
            "{now:?}: one to one as baked"
        );
        // Clipped to the node.
        assert!(prims.iter().any(|prim| matches!(
            prim,
            DrawPrim::ClipPush { x, y, w, h } if (*x, *y, *w, *h) == (rect.x, rect.y, rect.width, rect.height)
        )));
        let ground = [0x10, 0x12, 0x18, 0xFF];
        let scale = spec_now.scale;
        let (w, h) = (
            ((rect.right() + 24.0) * scale) as u32,
            ((rect.bottom() + 8.0) * scale) as u32,
        );
        let pixels = crate::tray_raster::rasterize_tray_pixels(&prims, w, h, scale, ground);
        let mut alone = Vec::new();
        let words = LockupPaint {
            spec: spec_now,
            images: None,
            pending: true,
        };
        paint_lockup(&mut alone, rect, Some(&words.value()), &roles());
        let alone = crate::tray_raster::rasterize_tray_pixels(&alone, w, h, scale, ground);
        assert_eq!(
            crate::widget::images_kept_bytes(),
            kept,
            "{now:?}: painting the frame kept nothing new"
        );
        assert!(!wanted.ready(), "{now:?}: and baked nothing");
        let painted = |pixels: &[u8]| {
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|p| p[..3].iter().zip(&ground).any(|(a, b)| a.abs_diff(*b) > 40))
                .count()
        };
        let (held, words) = (painted(&pixels), painted(&alone));
        assert!(
            held > words * 3,
            "{now:?}: the held road and cat draw ({held} pixels, the words alone {words})"
        );
    }
    // Baked at last, the new width draws.
    let wanted = baked_paint(before, 900.0).images.expect("baked");
    assert_eq!(before.paint(900.0, Some(last)).images, Some(wanted));
}

/// A COLD FIRST FRAME DRAWS THE WORDS ALONE (§4.5): with nothing baked, the lockup paints its
/// chevron and words and no image, and painting it keeps nothing.
#[test]
fn a_cold_first_frame_draws_the_words_and_no_image() {
    let cold = spec(22.0, 2.0, 0, 16);
    let paint = cold.paint(852.0, None);
    assert_eq!(paint.images, None);
    let rect = LogicalRect::new(0.0, 0.0, 852.0, cold.needed_height());
    let mut prims = Vec::new();
    paint_lockup(&mut prims, rect, Some(&paint.value()), &roles());
    assert!(
        !prims
            .iter()
            .any(|prim| matches!(prim, DrawPrim::Image { .. }))
    );
    assert!(
        prims
            .iter()
            .any(|prim| matches!(prim, DrawPrim::Text { .. }))
    );
    assert_eq!(
        crate::widget::images_kept_bytes(),
        0,
        "painting it kept nothing"
    );
}

/// THE WORDS KEEP THEIR HIERARCHY (§4.5): `aterm` in the brand face's strong weight (SF Mono
/// Bold, Iosevka Term Bold where SF Mono is missing), `ALab` and the separator in its regular
/// (SF Mono Medium, Iosevka Term Regular), each quieter. NEGATIVE CONTROL: the strong weight is
/// a heavier face than the regular (its stems wider at the same size).
#[test]
fn aterm_is_bold_and_alab_and_the_separator_quieter() {
    let value = spec(22.0, 2.0, 0, 16);
    let rect = LogicalRect::new(0.0, 0.0, 852.0, value.needed_height());
    let mut prims = Vec::new();
    paint_lockup(
        &mut prims,
        rect,
        Some(&value.paint(852.0, None).value()),
        &roles(),
    );
    // The words are drawn a character at a time; each run is one weight, face and ink.
    let mut words: Vec<(String, TextWeight, TextFace, crate::widget::Rgba)> = Vec::new();
    for prim in &prims {
        if let DrawPrim::Text {
            s,
            weight,
            face,
            color,
            ..
        } = prim
        {
            match words.last_mut() {
                Some(run) if (run.1, run.2, run.3) == (*weight, *face, *color) => run.0.push_str(s),
                _ => words.push((s.clone(), *weight, *face, *color)),
            }
        }
    }
    let roles = roles();
    assert_eq!(
        words,
        vec![
            (
                "aterm".to_owned(),
                TextWeight::Bold,
                TextFace::Brand(TextWeight::Bold),
                rgba(roles.text_primary, 255)
            ),
            (
                "·".to_owned(),
                TextWeight::Regular,
                TextFace::Brand(TextWeight::Regular),
                rgba(roles.text_tertiary, 255)
            ),
            (
                "ALab".to_owned(),
                TextWeight::Regular,
                TextFace::Brand(TextWeight::Regular),
                rgba(roles.text_secondary, 255)
            ),
        ]
    );
    // The bold inks more of the same word than the regular: a heavier face, not a fallback.
    let ink = |face: TextFace| {
        let prims = vec![text_prim(
            2.0,
            30.0,
            "aterm".to_owned(),
            TypeStep::Body.px(22.0),
            TextWeight::Regular,
            face,
            [255, 255, 255, 255],
        )];
        crate::tray_raster::rasterize_tray_pixels(&prims, 200, 40, 1.0, [0, 0, 0, 255])
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| u32::from(p[0]))
            .sum::<u32>()
    };
    let (bold, regular) = (
        ink(TextFace::Brand(TextWeight::Bold)),
        ink(TextFace::Brand(TextWeight::Regular)),
    );
    assert!(regular > 0, "the brand face draws");
    assert!(
        bold as f32 > regular as f32 * 1.25,
        "bold {bold} against regular {regular}"
    );
}

/// HIGH CONTRAST AND SERIOUS MODE SHOW ONE PROGRESS (§4.9's rubric): the caret stays after the
/// words, and the capsule after it fills as steps finish, full at done; neither the road nor
/// the cat is drawn. NEGATIVE CONTROL: with the road, the images draw.
#[test]
fn the_words_alone_show_one_progress_and_a_full_track_at_done() {
    let width = 852.0;
    let text = |done| LockupSpec {
        road: false,
        ..spec(22.0, 2.0, done, 16)
    };
    let carets: Vec<f32> = [0, 8, 16].map(|done| text(done).caret_x()).to_vec();
    assert!(
        carets.iter().all(|x| (x - carets[0]).abs() < 0.01),
        "{carets:?}"
    );
    let rect = LogicalRect::new(0.0, 0.0, width, 48.0);
    assert!(images(&text(8), rect).is_empty(), "the words alone");
    assert_eq!(
        images(&spec(22.0, 2.0, 8, 16), rect).len(),
        2,
        "with the road"
    );
    let capsule = |done| {
        let mut prims = Vec::new();
        paint_lockup(&mut prims, rect, Some(&text(done).value()), &roles());
        let capsules: Vec<(f32, f32, f32)> = prims
            .iter()
            .filter_map(|prim| match prim {
                DrawPrim::Capsule { x, w, frac, .. } => Some((*x, *w, *frac)),
                _ => None,
            })
            .collect();
        assert_eq!(capsules.len(), 1, "one progress");
        capsules[0]
    };
    let (start, half, done) = (capsule(0), capsule(8), capsule(16));
    assert_eq!(
        (start.0, start.1),
        (done.0, done.1),
        "the track stands still"
    );
    assert_eq!((start.2, half.2, done.2), (0.0, 0.5, 1.0), "and fills");
    assert!(done.0 + done.1 >= width - 0.01, "to the end of the lockup");
}

#[test]
fn a_mac_outside_alab_says_aterm_alone() {
    let alab = spec(22.0, 2.0, 0, 1);
    let elsewhere = LockupSpec {
        alab: false,
        ..alab
    };
    assert_eq!(alab.words(), "aterm · ALab");
    assert_eq!(elsewhere.words(), "aterm");
    assert!(elsewhere.least_width() < alab.least_width());
}

/// THE WORDS SPAN THE DESIGN'S WIDTH (§4.5): set in SF Mono, `aterm` bold, with the design's
/// letter spacing, `aterm · ALab` runs about 193 units from where the words begin (the design's
/// measure at Mono 22), so the road starts where the design starts it. NEGATIVE
/// CONTROL: the words alone, without the spacing, are 3.6 units shorter.
#[cfg(target_os = "macos")]
#[test]
fn the_words_span_the_designs_width() {
    let value = spec(22.0, 2.0, 0, 16);
    let unit = value.px / design::DESIGN_PX;
    let words = (value.words_end() / unit) - design::WORDS_X;
    assert!(
        (words - 193.0).abs() <= 2.0,
        "`aterm · ALab` spans {words:.1} units at Mono 22"
    );
    let bare: f32 = value
        .runs()
        .iter()
        .map(|(text, weight, _)| {
            crate::tray_raster::ui_text_width_for(TextFace::Brand(*weight), text, value.words_px())
        })
        .sum::<f32>()
        / unit;
    assert!(
        (words - bare - 12.0 * LETTER_SPACING).abs() < 0.5,
        "the spacing: {words:.1} against {bare:.1}"
    );
    let road = value.road_x() / unit;
    assert!(
        (road - design::ROAD_X).abs() <= 3.0,
        "the road starts at {road:.1} units, the design's {}",
        design::ROAD_X
    );
}
