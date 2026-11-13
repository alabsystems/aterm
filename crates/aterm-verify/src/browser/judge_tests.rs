// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const BG: [u8; 4] = [30, 30, 46, 255];
const RED: [u8; 4] = [205, 49, 49, 255];
const WHITE: [u8; 4] = [224, 224, 224, 255];

fn frame(lines: usize) -> Frame {
    let mut rgba: Vec<u8> = BG.repeat(16);
    rgba[..4].copy_from_slice(&RED);
    for l in 0..lines {
        let at = 4 * (4 * (l + 1) % 16);
        rgba[at..at + 4].copy_from_slice(&WHITE);
    }
    Frame {
        width: 4,
        height: 4,
        rgba,
    }
}

fn answer(frames: Vec<Frame>) -> Answer {
    Answer {
        module: "m".to_string(),
        adapter: "ANGLE (test)".to_string(),
        user_agent: String::new(),
        drawn: None,
        readback: Some(frame(2)),
        frames,
        diff: Vec::new(),
        glow: Vec::new(),
        timings: Value::Null,
        init: Some(Init {
            ready_on_return: true,
            probe_rejected: true,
            probe_thrown: "string".to_string(),
            probe_error: "create canvas surface failed".to_string(),
        }),
    }
}

fn run() -> Vec<Frame> {
    (0..3).map(frame).collect()
}

/// The glow style of [`glowing`]'s runs.
const STYLE: &str = "rainbow";

/// A glow scene answer: in [`STYLE`] on each page, bloom on and off, a
/// ramps run laying a ramp on every frame after the prompt (one of them
/// additive) and a quads run laying none, every frame the same; a page
/// named `… art` draws art its plain page does not.
fn glowing(pages: &[&str]) -> Answer {
    let mut a = answer(Vec::new());
    for &page in pages {
        for bloom in [true, false] {
            for ramps in [true, false] {
                let laid = (0..3u64)
                    .map(|k| {
                        if ramps && k > 0 {
                            [2, 4 * k, 1]
                        } else {
                            [0, 0, 0]
                        }
                    })
                    .collect();
                let mut last = frame(2);
                last.rgba[6] = u8::from(bloom);
                last.rgba[10] ^= if page.ends_with(" art") { 0x40 } else { 0 };
                a.glow.push(GlowRun {
                    style: STYLE.to_string(),
                    page: page.to_string(),
                    bloom,
                    ramps,
                    digests: ["prompt", "one", "two"].map(str::to_string).to_vec(),
                    laid,
                    last,
                });
            }
        }
    }
    a
}

#[test]
fn glow_ramps_byte_identical_to_their_quads_pass() {
    let pages = ["dark", "light", "dark art"];
    let art = [("dark art", "dark")];
    let v = glow(&glowing(&pages), &[STYLE], &pages, &art, 3);
    assert!(v.failures.is_empty(), "{:?}", v.failures);
    assert_eq!(v.lines.len(), 8, "{:?}", v.lines);
    // A page whose bloom draws nothing passes while another blooms.
    let mut a = glowing(&["dark", "light"]);
    for r in a.glow.iter_mut().filter(|r| r.page == "light") {
        r.last.rgba[6] = 0;
    }
    let v = glow(&a, &[STYLE], &["dark", "light"], &[], 3);
    assert!(v.failures.is_empty(), "{:?}", v.failures);
    // A style drawn by its quads alone on one page passes while it and
    // that page reach the ramps' route elsewhere, and is named.
    let mut a = glowing(&["dark", "light"]);
    let other = |r: &GlowRun| GlowRun {
        style: "lumen".to_string(),
        ..r.clone()
    };
    let lumen: Vec<GlowRun> = a.glow.iter().map(other).collect();
    a.glow.extend(lumen);
    for r in a
        .glow
        .iter_mut()
        .filter(|r| r.style == "lumen" && r.page == "light")
    {
        r.laid = vec![[0, 0, 0]; 3];
    }
    let v = glow(&a, &[STYLE, "lumen"], &["dark", "light"], &[], 3);
    assert!(v.failures.is_empty(), "{:?}", v.failures);
    assert!(
        v.lines
            .iter()
            .any(|l| l.contains("drawn by its quads alone: [lumen, light page")),
        "{:?}",
        v.lines
    );
}

#[test]
fn glow_ramps_that_differ_from_their_quads_fail_and_each_control_bites() {
    let pages = ["dark", "dark art"];
    let fails = |a: &Answer, says: &str| {
        let v = glow(a, &[STYLE], &pages, &[("dark art", "dark")], 3);
        assert!(
            v.failures.iter().any(|f| f.contains(says)),
            "{says}: {:?}",
            v.failures
        );
    };
    let mut a = glowing(&pages);
    a.glow[0].digests[1] = "moved".to_string();
    fails(&a, "differ from the quads on frames 1");
    let mut a = glowing(&pages);
    a.glow[0].last.rgba[0] ^= 1;
    fails(&a, "the last frame's ramps differ from its quads");
    let mut a = glowing(&pages);
    for r in a.glow.iter_mut().filter(|r| r.ramps) {
        r.laid = vec![[0, 0, 0]; 3];
    }
    fails(&a, "laid no lit ramp on any page");
    let mut a = glowing(&pages);
    for r in a
        .glow
        .iter_mut()
        .filter(|r| r.ramps && r.page == "dark art")
    {
        r.laid = vec![[0, 0, 0]; 3];
    }
    fails(&a, "no style's ramps run laid a lit ramp");
    let mut a = glowing(&pages);
    a.glow[1].laid[2] = [1, 3, 0];
    fails(&a, "it is no oracle");
    let mut a = glowing(&pages);
    for r in a.glow.iter_mut().filter(|r| r.bloom) {
        for l in &mut r.laid {
            l[2] = 0;
        }
    }
    fails(&a, "no item list was read by the bloom or its crown");
    let mut a = glowing(&pages);
    a.glow[0].style = "comet".to_string();
    fails(&a, "sent no ramps and quads runs");
    let mut a = glowing(&pages);
    for r in &mut a.glow {
        r.digests[2] = "prompt".to_string();
    }
    fails(&a, "the glow drew nothing");
    let mut a = glowing(&pages);
    for r in &mut a.glow {
        r.last.rgba[6] = 0;
    }
    fails(&a, "bloom on drew the frame bloom off drew on every page");
    let mut a = glowing(&pages);
    a.glow.retain(|r| r.bloom);
    fails(&a, "sent no ramps and quads runs");
    let mut a = glowing(&pages);
    for r in a.glow.iter_mut().filter(|r| r.page == "dark art") {
        r.last.rgba[10] ^= 0x40;
    }
    fails(&a, "the art drew nothing");
}

#[test]
fn frames_within_the_tolerance_pass_with_the_control_held() {
    let mut gpu = run();
    for f in &mut gpu {
        f.rgba[5] += TOLERANCE;
    }
    let v = judge(&answer(run()), &answer(gpu), 3);
    assert!(v.failures.is_empty(), "{:?}", v.failures);
    assert!(
        v.lines
            .last()
            .is_some_and(|l| l.contains("negative control held"))
    );
}

#[test]
fn a_frame_past_the_tolerance_fails_and_names_itself() {
    let mut gpu = run();
    gpu[2].rgba[5] += TOLERANCE + 1;
    let v = judge(&answer(run()), &answer(gpu), 3);
    assert_eq!(v.failures.len(), 1, "{:?}", v.failures);
    assert!(v.failures[0].starts_with("frame 2: the WebGL2 frame differs"));
}

#[test]
fn a_readback_that_misses_the_last_frame_fails() {
    let mut off = answer(run());
    if let Some(rb) = off.readback.as_mut() {
        rb.rgba[5] += TOLERANCE + 1;
    }
    let v = judge(&answer(run()), &off, 3);
    assert!(
        v.failures[0].starts_with("readback: render_offscreen() differs"),
        "{:?}",
        v.failures
    );
    let mut stale = answer(run());
    stale.readback = Some(frame(1));
    let v = judge(&answer(run()), &stale, 3);
    assert!(
        v.failures
            .iter()
            .any(|f| f.starts_with("NEGATIVE CONTROL: the readback")),
        "{:?}",
        v.failures
    );
    let mut none = answer(run());
    none.readback = None;
    let v = judge(&answer(run()), &none, 3);
    assert!(v.failures[0].contains("no render_offscreen() readback"));
}

#[test]
fn a_blank_present_fails_the_red_check() {
    let mut gpu = run();
    gpu[1].rgba[..4].copy_from_slice(&BG);
    let v = judge(&answer(run()), &answer(gpu), 3);
    assert!(
        v.failures
            .iter()
            .any(|f| f.starts_with("frame 1: no red glyph")),
        "{:?}",
        v.failures
    );
}

#[test]
fn a_page_that_stops_presenting_trips_the_negative_control() {
    let cpu = answer(run());
    let gpu = answer(vec![frame(0), frame(0), frame(1)]);
    let v = judge(&cpu, &gpu, 3);
    assert!(
        v.failures
            .iter()
            .any(|f| f.starts_with("NEGATIVE CONTROL: GPU frame 2")),
        "{:?}",
        v.failures
    );
}

#[test]
fn missing_frames_a_missing_adapter_and_a_size_mismatch_fail() {
    let mut gpu = answer(vec![frame(0)]);
    gpu.adapter.clear();
    let v = judge(&answer(run()), &gpu, 3);
    assert!(
        v.failures
            .iter()
            .any(|f| f.contains("gpu page answered 1 frames"))
    );
    assert!(v.failures.iter().any(|f| f.contains("no adapter")));
    let mut wide = frame(0);
    wide.width = 8;
    wide.rgba = wide.rgba.repeat(2);
    let v = judge(&answer(vec![frame(0)]), &answer(vec![wide]), 1);
    assert!(
        v.failures
            .iter()
            .any(|f| f.contains("the GPU canvas is 8x4, the CPU frame 4x4")),
        "{:?}",
        v.failures
    );
}

#[test]
fn an_init_that_is_not_synchronous_or_does_not_reject_fails() {
    let judged = |edit: fn(&mut Init)| {
        let mut gpu = answer(run());
        if let Some(init) = gpu.init.as_mut() {
            edit(init);
        }
        judge(&answer(run()), &gpu, 3).failures
    };
    assert!(judged(|_| {}).is_empty());
    assert!(judged(|i| i.ready_on_return = false)[0].contains("not synchronous"));
    assert!(judged(|i| i.probe_rejected = false)[0].contains("did not reject"));
    assert!(judged(|i| i.probe_thrown = "object".to_string())[0].contains("of type object"));
    let mut gpu = answer(run());
    gpu.init = None;
    assert!(judge(&answer(run()), &gpu, 3).failures[0].contains("no init report"));
}

#[test]
fn a_cpu_frame_that_moves_with_the_backend_fails() {
    fn refs(frames: &[Frame]) -> Vec<(String, &Frame)> {
        frames
            .iter()
            .enumerate()
            .map(|(k, f)| (format!("parity-{k}"), f))
            .collect()
    }
    let (a, b) = (run(), run());
    let mut gpu = run();
    gpu[1].rgba[5] += 3;
    let first_gpu = run();
    let v = across("metal", &a, &b, &refs(&first_gpu), &refs(&gpu));
    assert!(v.failures.is_empty(), "{:?}", v.failures);
    assert!(
        v.lines
            .iter()
            .any(|l| l.ends_with("parity-0 0 (0), parity-1 3 (1), parity-2 0 (0)")),
        "{:?}",
        v.lines
    );
    let mut c = run();
    c[2].rgba[5] += 1;
    let v = across("metal", &a, &c, &refs(&first_gpu), &refs(&gpu));
    assert_eq!(
        v.failures,
        ["CPU frame 2 differs from the metal session's (max byte delta 1)"]
    );
}

#[test]
fn a_worker_must_draw_where_it_says_and_what_the_page_drew() {
    let place = |w: [&str; 3]| {
        Some(Where {
            scope: w[0].to_string(),
            canvas: w[1].to_string(),
            diff_canvas: w[2].to_string(),
        })
    };
    let at = |w| {
        let mut a = answer(run());
        a.diff = vec![frame(0), frame(1)];
        a.drawn = place(w);
        a
    };
    let (page, off) = (at(DOCUMENT), at(WORKER));
    let v = worker(&page, &off);
    assert!(v.failures.is_empty(), "{:?}", v.failures);
    assert!(v.lines[0].contains("its 5 frames"), "{:?}", v.lines);
    let mut moved = at(WORKER);
    moved.diff[1].rgba[5] += 1;
    assert_eq!(
        worker(&page, &moved).failures,
        ["diff-1: the worker's frame differs from the gpu page's (max byte delta 1, 1 bytes)"]
    );
    let mut on_page = at(WORKER);
    on_page.drawn = place(["Window", "HTMLCanvasElement", "OffscreenCanvas"]);
    let v = worker(&page, &on_page);
    assert_eq!(v.failures.len(), 1, "{:?}", v.failures);
    assert!(
        v.failures[0].starts_with("the worker drew in Window"),
        "{:?}",
        v.failures
    );
    let mut silent = at(WORKER);
    silent.drawn = None;
    assert!(worker(&page, &silent).failures[0].contains("sent no `where`"));
    let mut elsewhere = at(WORKER);
    elsewhere.adapter = "SwiftShader".to_string();
    assert!(worker(&page, &elsewhere).failures[0].contains("the worker drew on"));
}

#[test]
fn an_answer_parses_its_frames_and_refuses_a_short_one() {
    let rgba = aterm_codec::base64::encode(&[1, 2, 3, 4, 5, 6, 7, 8]).expect("encode");
    let v = aterm_json::json!({
        "module": "aterm-wasm", "adapter": "cpu", "timings": {"total_ms": 1.5},
        "frames": [{"width": 2, "height": 1, "rgba": rgba}]
    });
    let a = parse(&v).expect("parses");
    assert_eq!(a.frames[0].rgba, [1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(a.module, "aterm-wasm");
    let short = aterm_json::json!({
        "module": "m", "adapter": "a", "frames": [{"width": 3, "height": 1, "rgba": rgba}]
    });
    assert_eq!(
        parse(&short),
        Err("frame 0 is 3x1 but carries 8 bytes".to_string())
    );
    assert!(parse(&aterm_json::json!({"module": "m"})).is_err());
    let gpu = aterm_json::json!({
        "module": "aterm-gpu-web", "adapter": "ANGLE", "frames": [],
        "init": {"ready_on_return": true,
                 "probe": {"rejected": true, "thrown": "string", "error": "e"}}
    });
    assert_eq!(
        parse(&gpu).map(|a| a.init),
        Ok(answer(vec![]).init.map(|i| Init {
            probe_error: "e".to_string(),
            ..i
        }))
    );
    let no_probe = aterm_json::json!({
        "module": "m", "adapter": "a", "frames": [], "init": {"ready_on_return": true}
    });
    assert!(parse(&no_probe).is_err());
    let placed = aterm_json::json!({
        "module": "m", "adapter": "a", "frames": [], "user_agent": "Firefox/155.0",
        "where": {"scope": "DedicatedWorkerGlobalScope", "canvas": "OffscreenCanvas",
                  "diff_canvas": "OffscreenCanvas"}
    });
    let a = parse(&placed).expect("parses");
    assert_eq!(a.user_agent, "Firefox/155.0");
    assert_eq!(drawn_in("worker", &a, WORKER), None);
    let half = aterm_json::json!({
        "module": "m", "adapter": "a", "frames": [], "where": {"scope": "Window"}
    });
    assert!(parse(&half).is_err());
}
