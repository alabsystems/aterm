// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn words(w: &[&str]) -> Vec<String> {
    w.iter().map(|w| (*w).to_owned()).collect()
}

/// A take's CPU splits by thread: the main thread (the least id at the start), the
/// recording's capture threads (born during the take, from 0), the app's others, and what
/// no sample saw (a thread that ended between samples), which sums back to the process's.
#[test]
fn a_takes_cpu_splits_by_thread_and_sums_to_the_process() {
    let t = |tid: u64, name: &str, ns: u64| crate::sys::ThreadCpu {
        tid,
        name: name.to_owned(),
        ns,
    };
    let mut take = CpuSampler::begin(1_000, &[t(5, "", 400), t(9, "aterm-render", 100)]);
    take.sample(
        Some(1_500),
        &[
            t(5, "", 600),
            t(9, "aterm-render", 130),
            t(12, "aterm-video-convert", 50),
        ],
    );
    take.sample(
        Some(2_000),
        &[
            t(5, "", 700),
            t(12, "aterm-video-convert", 90),
            t(13, "aterm-video-convert", 20),
        ],
    );
    let split = take.split();
    assert_eq!(split.process, 1_000);
    assert_eq!(split.main, 300, "the main thread's own");
    assert_eq!(split.capture, 110, "both capture threads, each from 0");
    assert_eq!(
        split.others,
        BTreeMap::from([("aterm-render".to_owned(), 30)]),
        "a thread gone by the end keeps what its last sample saw"
    );
    assert_eq!(split.app(), 330);
    assert_eq!(split.unseen(), 1_000 - 300 - 110 - 30);
    let line = split_line(&split, 10.0);
    assert!(
        line.contains("capture threads (aterm-video*) 0.000 s"),
        "{line}"
    );
}

#[test]
fn the_takes_and_the_geometry_parse() {
    let a = parse_args(&words(&["/tmp/out"])).expect("parses");
    assert_eq!(a.takes, Take::ALL.to_vec());
    assert_eq!((a.secs, a.settle, a.repeat), (10, 15, 1));
    let a = parse_args(&words(&[
        "--dial", "1,2,3,4", "--repeat", "3", "/tmp/out", "swing", "idle", "swing",
    ]))
    .expect("parses");
    assert_eq!(a.takes, vec![Take::Swing, Take::Idle]);
    assert_eq!((a.geom.cx, a.geom.r1, a.repeat), (1.0, 4.0, 3));
    for bad in [
        &["/tmp/out", "dance"][..],
        &[][..],
        &["--dial", "1,2", "/tmp/out"][..],
        &["--secs", "0", "/tmp/out"][..],
        &["--what", "/tmp/out"][..],
    ] {
        assert!(parse_args(&words(bad)).is_err(), "{bad:?}");
    }
}

#[test]
fn only_the_swing_scripts_a_strain_and_only_nodeck_drops_the_deck() {
    for t in Take::ALL {
        let env = t.env();
        let has = |k: &str| env.iter().any(|(key, _)| key == k);
        assert_eq!(has("ATERM_DEV_STRAIN_SCRIPT"), t == Take::Swing, "{t:?}");
        assert_eq!(
            has("ATERM_DEV_STATS_DECK_HEADLESS"),
            t != Take::NoDeck,
            "{t:?}"
        );
        assert_eq!(
            t.config().contains("stats_bars = \"sill\""),
            t == Take::Sill
        );
    }
}

/// A frame `w`×`h`, black, with a white needle from the centre at `deg` clockwise from up.
fn needle_frame(w: usize, h: usize, g: &Geom, deg: f64) -> Vec<u8> {
    let mut px = vec![0u8; w * h * 4];
    let (s, c) = deg.to_radians().sin_cos();
    let mut r = g.r0;
    while r <= g.r1 {
        let (x, y) = ((g.cx + s * r).round(), (g.cy - c * r).round());
        let at = (y as usize * w + x as usize) * 4;
        px[at..at + 4].copy_from_slice(&[255, 255, 255, 255]);
        r += 0.25;
    }
    px
}

#[test]
fn the_needle_reads_back_its_angle_and_the_frames_sort_by_what_moved() {
    let g = Geom::FOLDED_2X;
    let (w, h) = (400, 420);
    for deg in [-120.0, -65.5, 0.0, 30.0, 110.0] {
        let (a, wt) = needle_angle(&needle_frame(w, h, &g, deg), w, &g);
        let a = a.expect("ink");
        assert!((a - deg).abs() < 1.5, "{deg}: read {a}");
        assert!(wt > 0.0);
    }
    let a = needle_frame(w, h, &g, -60.0);
    let b = needle_frame(w, h, &g, -50.0);
    assert_eq!(classify(&a, &a, w, &g), Kind::Identical);
    assert_eq!(classify(&a, &b, w, &g), Kind::Dial);
    let mut c = a.clone();
    let at = (300 * w + 300) * 4;
    c[at] = 9;
    assert_eq!(classify(&a, &c, w, &g), Kind::Rest);
    let mut d = a.clone();
    d[(10 * w + 10) * 4 + 1] = 9;
    assert_eq!(classify(&a, &d, w, &g), Kind::Outside);
    let mut e = b;
    e[at] = 9;
    assert_eq!(classify(&a, &e, w, &g), Kind::DialRest);
    let mut alpha_only = a.clone();
    alpha_only[at + 3] = 7;
    assert_eq!(classify(&a, &alpha_only, w, &g), Kind::Identical);
}

/// The needle's cadence counts only frames that move it: on a 60 Hz grid, moving frames one
/// and two periods apart sit on it, one 5 ms late is off it by 5 ms, a dial frame that moves
/// no needle is counted apart, and a gap over 200 ms (between sweeps) is no cadence.
#[test]
fn the_needle_cadence_reads_only_the_frames_that_move_it() {
    let row = |t_us: i64, kind: Kind, angle: f64| Row {
        n: 0,
        t_us,
        kind,
        angle: Some(angle),
        weight: 1.0,
    };
    let p = 16_667;
    let frames = Frames {
        rows: vec![
            row(0, Kind::Identical, 0.0),
            row(p, Kind::Dial, 1.0),
            row(2 * p, Kind::Dial, 2.0),
            row(4 * p, Kind::Dial, 3.0),
            row(4 * p + 1_500, Kind::Dial, 3.0),
            row(5 * p + 5_000, Kind::Dial, 4.0),
            row(5 * p + 400_000, Kind::Dial, 5.0),
        ],
    };
    let line = frames.needle_cadence(60);
    assert!(
        line.starts_with("needle cadence on the 60 Hz grid: 5 moving frames, 3 gaps <200ms"),
        "{line}"
    );
    assert!(line.contains("max=5.00 ms, 1 over 2 ms"), "{line}");
    assert!(line.ends_with("1 dial frames move no needle"), "{line}");
}

#[test]
fn percentiles_take_the_nearest_rank_map_3_took() {
    assert!((pct(&[5.0, 1.0, 3.0, 2.0], 50.0) - 3.0).abs() < 1e-9);
    assert!((pct(&[1.0, 2.0, 3.0, 4.0, 5.0], 95.0) - 5.0).abs() < 1e-9);
    assert!(pct(&[], 50.0).is_nan());
}
