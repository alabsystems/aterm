// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Golden trace export and replay tests for `aos_momentum` ([`RECORDED`]).

use std::path::PathBuf;
use std::time::Duration;

use aterm_effects::aos_momentum::{MomentumCode, MomentumHost, MomentumTrace, target_name};
use aterm_effects::cursor_glow::Geom;
use aterm_effects::cursor_momentum::{
    MOMENTUM_GLOW_RADIUS_CELLS, MOMENTUM_GLOW_TAU_S, MomentumGlowConfig,
};
use aterm_time::Instant;

/// The build id the story attaches under.
const BUILD: &str = "aos-momentum-trace-fixture";

/// The targets whose file is checked in.
const RECORDED: &[&str] = &["aarch64-macos"];

/// A 20 Hz effect lane (inside the profile's frame intervals), so the story stays small.
const LANE: Duration = Duration::from_millis(50);

fn geom() -> Geom {
    Geom {
        cw: 10,
        ch: 20,
        rows: 24,
        cols: 80,
        origin_x: 0,
        origin_y: 0,
        win_w: 800,
        win_h: 480,
        head: 0,
    }
}

fn cfg(dark: bool) -> MomentumGlowConfig {
    MomentumGlowConfig {
        enabled: true,
        intensity: 1.0,
        tau_s: MOMENTUM_GLOW_TAU_S,
        radius_cells: MOMENTUM_GLOW_RADIUS_CELLS,
        base: 0x0050_FA7B,
        dark_theme: dark,
        hue: 0x0050_FA7B,
        theme_fg: if dark { 0x00C8_C8D2 } else { 0x0024_292F },
        theme_bg: if dark { 0x0010_1016 } else { 0x00FF_FFFF },
        block: true,
        glyph_span: None,
    }
}

/// The story, attached at `t0` under the shipped code.
fn story(t0: Instant) -> MomentumHost {
    let mut host = MomentumHost::default();
    host.aos_attach(t0, MomentumCode::Shipped, BUILD, 1);
    let (mut out, mut under) = (Vec::new(), Vec::new());
    let mut now = t0 + Duration::from_millis(3);
    for (page, dark) in [true, false].into_iter().enumerate() {
        if page == 1 {
            host.reset();
        }
        let c = cfg(dark);
        let row = 4 + 2 * u16::try_from(page).unwrap_or(0);
        let mut col = 10;
        for _ in 0..6 {
            host.on_key(now, MOMENTUM_GLOW_TAU_S);
            col += 1;
            out.clear();
            under.clear();
            host.tick(Some((row, col)), now, geom(), &c, &mut out, &mut under);
            now += Duration::from_millis(70);
        }
        loop {
            let next = if host.is_active() {
                Some(now + LANE)
            } else if host.settling() {
                host.next_change_deadline(now, LANE)
            } else {
                None
            };
            let Some(next) = next else { break };
            assert!(next > now, "the story's lane re-armed at or before now");
            now = next;
            out.clear();
            under.clear();
            host.tick(Some((row, col)), now, geom(), &c, &mut out, &mut under);
        }
        now += Duration::from_millis(250);
    }
    host
}

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/aos-momentum")
        .join(format!("{}.trace", target_name()))
}

#[test]
fn the_story_exports_the_checked_in_trace_and_the_checked_in_trace_replays_to_its_head() {
    let t0 = Instant::now();
    let host = story(t0);
    let trace = host.aos_trace().expect("the story's glow is attached");
    let bytes = trace.encode();
    let path = fixture();
    if std::env::var_os("ATERM_BLESS_GOLDEN").is_some_and(|v| v == "1") {
        let dir = path.parent().expect("the fixture has a directory");
        std::fs::create_dir_all(dir).expect("the fixture directory is made");
        std::fs::write(&path, &bytes).expect("the fixture is written");
        eprintln!("blessed {} ({} bytes)", path.display(), bytes.len());
    }
    let target = target_name();
    let golden = match std::fs::read(&path) {
        Ok(golden) => golden,
        Err(e) if !RECORDED.contains(&target.as_str()) => {
            eprintln!(
                "skip: no momentum trace is checked in for {target} ({}: {e}); record one with ATERM_BLESS_GOLDEN=1",
                path.display()
            );
            return;
        }
        Err(e) => panic!(
            "missing momentum trace {} for {target}: {e}; record it with ATERM_BLESS_GOLDEN=1",
            path.display()
        ),
    };
    let first_difference = golden.iter().zip(&bytes).position(|(a, b)| a != b);
    assert!(
        golden == bytes,
        "the story exported {} bytes that are not the checked-in {} (first difference at byte {first_difference:?}); \
         fix the change, or re-record with ATERM_BLESS_GOLDEN=1 and say why in the commit; \
         re-recording aarch64-macos.trace moves the M4 corpus, which is a new acceptance policy version (ITEMS_V1's doc)",
        bytes.len(),
        golden.len()
    );

    let read = MomentumTrace::decode(&golden).expect("the checked-in trace decodes");
    assert_eq!(read, trace);
    let elsewhere = Instant::now() + Duration::from_secs(7);
    let replayed = read
        .replay(elsewhere, BUILD)
        .expect("the checked-in trace replays on another epoch");
    assert_eq!(replayed.state_digest, read.head_state);
    assert_eq!(replayed.observation_chain, read.head_chain);
    assert_eq!(replayed.code, MomentumCode::Shipped);
    assert_eq!((replayed.log_epoch, replayed.first_cursor), (1, 0));
    assert_eq!(replayed.counts.keys, 12);
    assert_eq!(replayed.counts.resets, 1);
    assert!(replayed.counts.ticks > 12 && replayed.counts.queries > 0);
    assert_eq!(
        host.aos_status().state_digest,
        Some(read.head_state.short()),
        "the head the file records is the live state's"
    );
}
