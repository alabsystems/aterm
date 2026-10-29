// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! [`Terminal::replay_fork`] against the straight fold.

use super::{Terminal, TerminalCheckpoint};
use crate::grid::ResizePolicy;
use crate::terminal::ClockReading;
use std::time::Duration;

enum Ev {
    Out(Vec<u8>),
    Resize(u16, u16),
}

fn out(bytes: impl Into<Vec<u8>>) -> Ev {
    Ev::Out(bytes.into())
}

struct Fixture {
    name: &'static str,
    rows: u16,
    cols: u16,
    policy: ResizePolicy,
    ring: Option<usize>,
    events: Vec<Ev>,
}

#[derive(Debug, PartialEq)]
struct Seen {
    rows: Vec<String>,
    cursor: (u16, u16),
    images: Vec<Vec<(usize, aterm_grid::ImageRef)>>,
    checkpoint: Option<TerminalCheckpoint>,
}

fn seen(t: &Terminal) -> Seen {
    let rows = usize::from(t.rows());
    let c = t.cursor();
    Seen {
        rows: (0..rows)
            .map(|r| t.row_text(r).unwrap_or_default())
            .collect(),
        cursor: (c.row, c.col),
        images: (0..rows).map(|r| t.images_row(r)).collect(),
        checkpoint: t.checkpoint_continuing(),
    }
}

fn fixture_epoch() -> aterm_time::Instant {
    ClockReading::now()
        .monotonic
        .checked_add(Duration::from_secs(3600))
        .expect("clock")
}

fn clock(epoch: aterm_time::Instant, i: usize) -> ClockReading {
    let ms = u64::try_from(i).expect("small");
    ClockReading {
        monotonic: epoch.checked_add(Duration::from_millis(ms)).expect("clock"),
        wall_ms: Some(1_700_000_000_000 + ms),
    }
}

impl Fixture {
    fn configure(&self, t: &mut Terminal) {
        t.set_alt_archive_enabled(false);
        t.set_console_host_posture(self.policy);
    }

    fn engine(&self) -> Terminal {
        let mut t = Terminal::new(self.rows, self.cols);
        self.configure(&mut t);
        if self.ring.is_some() {
            t.set_scrollback_line_limit(self.ring);
        }
        t
    }

    fn apply(&self, t: &mut Terminal, epoch: aterm_time::Instant, from: usize) {
        for (i, ev) in self.events.iter().enumerate().skip(from) {
            match ev {
                Ev::Out(bytes) => t.process_at(bytes, clock(epoch, i)),
                Ev::Resize(rows, cols) => t.resize_with_policy(*rows, *cols, self.policy),
            }
        }
    }

    fn at_every_boundary(
        &self,
        copy: impl Fn(&Terminal) -> Option<Terminal>,
    ) -> (Seen, Vec<Option<Seen>>) {
        self.at_every_boundary_from(|| self.engine(), copy)
    }

    fn at_every_boundary_from(
        &self,
        engine: impl Fn() -> Terminal,
        copy: impl Fn(&Terminal) -> Option<Terminal>,
    ) -> (Seen, Vec<Option<Seen>>) {
        let epoch = fixture_epoch();
        let mut straight = engine();
        self.apply(&mut straight, epoch, 0);
        let end = seen(&straight);
        let mut src = engine();
        let mut ends = Vec::new();
        for (k, ev) in self.events.iter().enumerate() {
            match ev {
                Ev::Out(bytes) => src.process_at(bytes, clock(epoch, k)),
                Ev::Resize(rows, cols) => src.resize_with_policy(*rows, *cols, self.policy),
            }
            ends.push(copy(&src).map(|mut copy| {
                self.configure(&mut copy);
                self.apply(&mut copy, epoch, k + 1);
                seen(&copy)
            }));
        }
        (end, ends)
    }
}

fn session() -> Fixture {
    let mut events = Vec::new();
    for i in 0..30 {
        events.push(out(match i % 5 {
            0 => format!("\x1b]8;;https://example.test/{i}\x1b\\link {i}\x1b]8;;\x1b\\\r\n"),
            1 => format!("\x1b[31mred {i}\x1b[m \u{754c}\r\n"),
            2 => format!("e\u{301} combining {i}\r\n"),
            3 => format!("\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467} {i}\r\n"),
            _ => format!("wrap {i} {}\r\n", "x".repeat(40)),
        }));
    }
    events.extend([
        out("\x1b]8;;https://example.test/open\x1b\\open "),
        out("link\x1b]8;;\x1b\\\r\n"),
        out("rep X"),
        out("\x1b[3b\r\n"),
        out("\x1b[#{\x1b[1;32mpushed "),
        out("\x1b[#}popped\r\n"),
        out("\x1b[3g\x1b[1;7H\x1bH\r"),
        out("\ttab\r\n"),
        out("\x1b[2;6r\x1b[6;1H"),
        out("in region 1\nin region 2\nin region 3\n"),
        out("\x1b[r\x1b[4;10H\x1b7\x1b[8;1H"),
        out("\x1b[?2026h\x1b[?1049h\x1b[H\x1b[2J"),
    ]);
    let mut frame = String::new();
    for r in 0..8 {
        frame.push_str(&format!("\x1b[{};1HF{r}", r + 1));
    }
    frame.push_str("\x1b[6;3H\x1b[?2026l");
    events.extend([
        out(frame),
        Ev::Resize(7, 30),
        out("\x1b[?1000h"),
        Ev::Resize(8, 30),
        Ev::Resize(7, 30),
        Ev::Resize(8, 30),
        out("\x1b[3;1H\x1b[Kdiff\x1b[6;3H"),
        out("\x1b[?1049l"),
        Ev::Resize(11, 30),
        Ev::Resize(11, 24),
    ]);
    for i in 0..25 {
        events.push(out(format!("after {i}\r\n")));
    }
    events.push(out("\x1b8tail"));
    Fixture {
        name: "session",
        rows: 8,
        cols: 30,
        policy: ResizePolicy::Native,
        ring: Some(20),
        events,
    }
}

fn pager_without_cursor_save() -> Fixture {
    let mut pager = String::from("\x1b[?1047h\x1b[H\x1b[2J");
    for r in 0..6 {
        pager.push_str(&format!("page {r}\r\n"));
    }
    Fixture {
        name: "pager_without_cursor_save",
        rows: 8,
        cols: 30,
        policy: ResizePolicy::Native,
        ring: None,
        events: vec![
            out("$ ls\r\nfile\r\n\x1b[2;4H\x1b7\x1b[3;1H$ pager\r\n"),
            out(pager),
            out("\x1b[7;9H"),
            out("more"),
            out("\x1b]133;A\x07$ "),
            out("ls\r\nfile\r\n"),
        ],
    }
}

fn reads(fixture: &str) -> Vec<Ev> {
    fixture
        .lines()
        .filter(|line| !line.starts_with('#'))
        .map(|line| {
            let mut bytes = Vec::with_capacity(line.len());
            let mut it = line.bytes();
            while let Some(b) = it.next() {
                if b != b'\\' {
                    bytes.push(b);
                    continue;
                }
                bytes.push(match it.next() {
                    Some(b'e') => 0x1b,
                    Some(b'a') => 0x07,
                    Some(b'b') => 0x08,
                    Some(b'r') => b'\r',
                    Some(b'n') => b'\n',
                    Some(b'\\') => b'\\',
                    other => panic!("unknown escape {other:?} in {line:?}"),
                });
            }
            Ev::Out(bytes)
        })
        .collect()
}

fn conpty(name: &'static str, fixture: &str) -> Fixture {
    Fixture {
        name,
        rows: 24,
        cols: 80,
        policy: ResizePolicy::ConPty,
        ring: None,
        events: reads(fixture),
    }
}

fn conpty_fixtures() -> [Fixture; 2] {
    [
        conpty(
            "conpty_killed_pager_then_second_pager",
            include_str!("testdata/conpty_killed_pager_then_second_pager.reads"),
        ),
        conpty(
            "conpty_killed_pager_console_resync",
            include_str!("testdata/conpty_killed_pager_console_resync.reads"),
        ),
    ]
}

fn kitty() -> Fixture {
    let show = |row: u16| {
        let raw = [255u8, 0, 0, 255].repeat(4);
        let mut seq =
            format!("\x1b[{};1H\x1b_Ga=T,f=32,s=2,v=2,I=7,c=2,r=1,q=2;", row + 1).into_bytes();
        seq.extend_from_slice(
            aterm_codec::base64::encode(&raw)
                .expect("encode")
                .as_bytes(),
        );
        seq.extend_from_slice(b"\x1b\\");
        Ev::Out(seq)
    };
    Fixture {
        name: "kitty",
        rows: 10,
        cols: 30,
        policy: ResizePolicy::Native,
        ring: None,
        events: vec![
            out("text before\r\n"),
            show(1),
            out(" after the image\r\n"),
            out("\x1b[4;6H\x1b_Ga=p,I=7,q=2\x1b\\"),
            out(" placed again\r\n"),
            out("\x1b_Ga=d,d=A,q=2\x1b\\"),
            out("\r\nmore text\r\n"),
            show(7),
            out(" done"),
        ],
    }
}

fn sync_left_open() -> Fixture {
    Fixture {
        name: "sync_left_open",
        rows: 8,
        cols: 30,
        policy: ResizePolicy::Native,
        ring: None,
        events: vec![
            out("$ app\r\n"),
            out("\x1b[?2026h\x1b[H\x1b[2Jframe"),
            out("\r\nstill in the frame"),
        ],
    }
}

fn live_images() -> Fixture {
    use crate::terminal::handler_osc_1337::MAX_LIVE_IMAGE_BYTES;
    let side = 1024;
    let raster = side * side * 4;
    let picture = aterm_codec::base64::encode(&crate::terminal::kitty_graphics::zlib_zeros(raster))
        .expect("encode");
    let stamp = |row: usize, col: usize| {
        format!(
            "\x1b[{row};{col}H\x1b_Ga=T,f=32,s={side},v={side},o=z,c=1,r=1,C=1,q=2;{picture}\x1b\\"
        )
    };
    let shown = MAX_LIVE_IMAGE_BYTES / 2 / raster + 1;
    let erased = MAX_LIVE_IMAGE_BYTES / raster - shown;
    let pixel = aterm_codec::base64::encode(&[0u8, 0, 255, 255]).expect("encode");
    Fixture {
        name: "live_images",
        rows: 8,
        cols: 30,
        policy: ResizePolicy::Native,
        ring: None,
        events: vec![
            out((1..=shown).map(|col| stamp(1, col)).collect::<String>()),
            out((0..erased)
                .map(|_| stamp(2, 1) + "\x1b[2K")
                .collect::<String>()),
            out(format!(
                "\x1b[3;1H\x1b_Ga=T,f=32,s=1,v=1,c=1,r=1,C=1,q=2;{pixel}\x1b\\"
            )),
        ],
    }
}

/// Some 1440 lines in 24 writes with a narrowing, a widening and a re-narrowing between: enough to fill a 1000-line ring, stage rows and evict from a small store.
fn long() -> Fixture {
    let mut events = Vec::new();
    for i in 0..24 {
        let mut chunk = String::new();
        for j in 0..60 {
            chunk.push_str(&format!(
                "\x1b[3{}mline {i}.{j}\x1b[m {}\r\n",
                j % 8,
                "y".repeat(j % 50)
            ));
        }
        events.push(out(chunk));
        match i {
            8 => events.push(Ev::Resize(10, 30)),
            14 => events.push(Ev::Resize(14, 50)),
            18 => events.push(Ev::Resize(14, 41)),
            _ => {}
        }
    }
    Fixture {
        name: "long",
        rows: 12,
        cols: 41,
        policy: ResizePolicy::Native,
        ring: None,
        events,
    }
}

/// Drains the fallback response rate-limit bucket at `t = 0 ms`, then at `t = 1 ms` eats the 1 ms refill with DSR queries and flips OSC 11 under DEC 2031: the drained engine refuses the 9-byte scheme push (`modes.color_scheme` stays dark), while a fresh bucket admits it and latches light.
fn drained_response_bucket() -> Fixture {
    let mut drain = String::from("\x1b]11;#000000\x07");
    drain.push_str(&"\x1b[5n".repeat(24_592));
    let mut probe = "\x1b[5n".repeat(30);
    probe.push_str("\x1b[?2031h\x1b]11;#ffffff\x07");
    Fixture {
        name: "drained_response_bucket",
        rows: 6,
        cols: 30,
        policy: ResizePolicy::Native,
        ring: None,
        events: vec![out(drain), out(probe)],
    }
}

fn fixtures() -> Vec<Fixture> {
    let mut fixtures = vec![
        session(),
        pager_without_cursor_save(),
        kitty(),
        sync_left_open(),
        live_images(),
        drained_response_bucket(),
    ];
    fixtures.extend(conpty_fixtures());
    fixtures
}

#[test]
fn a_fork_folds_the_rest_as_the_straight_fold_does() {
    let mut all = fixtures();
    all.push(long());
    for fx in &all {
        assert_forks_fold_the_straight_end(fx, &fx.at_every_boundary(Terminal::replay_fork));
    }
}

/// Every fork in `at` ends where the straight fold does, and the boundaries that refuse are the expected ones.
fn assert_forks_fold_the_straight_end(fx: &Fixture, at: &(Seen, Vec<Option<Seen>>)) {
    let (end, ends) = at;
    {
        let mut refused = Vec::new();
        for (k, fork_end) in ends.iter().enumerate() {
            match fork_end {
                Some(fork_end) => {
                    assert_eq!(fork_end, end, "{}: forked after {} events", fx.name, k + 1)
                }
                None => refused.push(k + 1),
            }
        }
        let n = fx.events.len();
        match fx.name {
            "session" => {
                let first_resize = fx
                    .events
                    .iter()
                    .position(|e| matches!(e, Ev::Resize(..)))
                    .map(|i| i + 1);
                assert_eq!(
                    refused.first().copied(),
                    first_resize,
                    "{}: {refused:?}",
                    fx.name
                );
            }
            "kitty" | "sync_left_open" | "live_images" | "drained_response_bucket" => {
                assert!(refused.is_empty(), "{}: {refused:?}", fx.name);
            }
            "long" => {
                let after_resize: Vec<usize> = (1..=n)
                    .filter(|&k| matches!(fx.events[k - 1], Ev::Resize(..)))
                    .collect();
                assert!(
                    refused.iter().all(|k| after_resize.contains(k)),
                    "{}: {refused:?}",
                    fx.name
                );
            }
            _ => assert!(refused.len() < n / 4, "{}: {refused:?} of {n}", fx.name),
        }
    }
}

#[test]
fn a_restore_in_place_of_a_fork_folds_another_end() {
    let restore = |t: &Terminal| {
        t.replay_fork()?;
        t.checkpoint_continuing()
            .map(|c| Terminal::from_checkpoint(&c))
    };
    for fx in &fixtures() {
        let (end, ends) = fx.at_every_boundary(restore);
        assert!(
            ends.iter().flatten().any(|restored| restored != &end),
            "{}: every restore ended as the straight fold",
            fx.name
        );
    }
}

#[test]
fn a_fork_without_a_carried_piece_folds_another_end() {
    type Strip = fn(&mut Terminal);
    let orphaned_screens = || {
        let mut fixtures = vec![pager_without_cursor_save()];
        fixtures.extend(conpty_fixtures());
        fixtures
    };
    let pieces: [(&str, Strip, Vec<Fixture>); 9] = [
        (
            "the orphaned-screen leave's notes",
            |fork| {
                fork.transient.alt_entered_without_cursor_save = false;
                fork.transient.conhost_alt_screen_left_up = false;
            },
            orphaned_screens(),
        ),
        (
            "the synchronized-update timer",
            Terminal::arm_carried_sync_window,
            vec![sync_left_open()],
        ),
        (
            "the last Kitty id assigned",
            |fork| fork.transient.kitty_last_assigned_id = 0,
            vec![kitty()],
        ),
        (
            "the Kitty serials",
            |fork| fork.transient.kitty_serial = 0,
            vec![kitty()],
        ),
        (
            "the Kitty image store",
            |fork| {
                fork.transient.kitty_images.clear();
                fork.transient.kitty_numbers.clear();
            },
            vec![kitty()],
        ),
        (
            "the live-image charge",
            |fork| fork.transient.live_image_charge = [0; 2],
            vec![live_images()],
        ),
        (
            "its source's restore lineage",
            |fork| fork.holder_carried_at = Some(fork.transient.screen_mutations),
            vec![session()],
        ),
        (
            "the resize hand-back",
            |fork| fork.grid_mut().forget_handback(),
            vec![session()],
        ),
        (
            "the response rate-limit bucket",
            |fork| {
                fork.transient.response_rate_limiter =
                    crate::terminal::response_rate_limiter::ResponseRateLimiter::new();
            },
            vec![drained_response_bucket()],
        ),
    ];
    for (piece, strip, fixtures) in pieces {
        let without = |t: &Terminal| {
            t.replay_fork().map(|mut fork| {
                strip(&mut fork);
                fork
            })
        };
        for fx in &fixtures {
            let (end, ends) = fx.at_every_boundary(without);
            assert!(
                ends.iter().flatten().any(|forked| forked != &end),
                "{}: a fork without {piece} ends as the straight fold",
                fx.name
            );
        }
    }
}

#[test]
fn a_fork_continues_its_sources_restore_lineage() {
    for (before, after, adopt) in [
        (false, false, true),
        (true, false, true),
        (true, true, false),
    ] {
        let label = format!("output before the fork: {before}, after: {after}, adopted: {adopt}");
        let mut src = Terminal::new(6, 30);
        src.process(b"\x1b[1mbefore\x1b[m the restore\r\n");
        src.holder_carried_at = Some(src.transient.screen_mutations);
        if before {
            src.process(b"replayed\r\n");
        }
        let mut fork = src.replay_fork().expect("a fork");
        let end = |t: &mut Terminal| {
            if after {
                t.process(b"more\r\n");
            }
            let adoption = adopt.then(|| t.adopt_holder_after_restore(0, false));
            (adoption, t.checkpoint_continuing().expect("ground"))
        };
        let (src_end, fork_end) = (end(&mut src), end(&mut fork));
        assert_eq!(
            src_end.1.holder.seed_untrusted, before,
            "{label}: premise, the restore is untouched until output"
        );
        assert_eq!(fork_end, src_end, "{label}");
    }
}

/// A replay restore of a keyframe of an engine with no restore pending ends as that engine does after the same output, adopted or not; a plain restore marks its seed untrusted, and an engine whose own restore is pending is not what it continues.
#[test]
fn a_replay_restore_continues_an_engine_with_no_restore_pending() {
    for adopt in [false, true] {
        let mut live = Terminal::new(6, 30);
        live.process(b"\x1b[1mbefore\x1b[m the keyframe\r\n");
        let keyframe = live.checkpoint_continuing().expect("ground");
        let mut replayed = Terminal::replay_from_checkpoint(&keyframe);
        let mut restored = Terminal::from_checkpoint(&keyframe);
        let end = |t: &mut Terminal| {
            t.process(b"more\r\n");
            let adoption = adopt.then(|| t.adopt_holder_after_restore(0, false));
            (adoption, t.checkpoint_continuing().expect("ground"))
        };
        let live_end = end(&mut live);
        assert_eq!(end(&mut replayed), live_end, "adopted: {adopt}");
        if !adopt {
            assert!(!live_end.1.holder.seed_untrusted, "premise");
            assert!(
                end(&mut restored).1.holder.seed_untrusted,
                "a plain restore adds lineage of its own"
            );
        }
    }
    let mut carried = Terminal::new(6, 30);
    carried.holder_carried_at = Some(carried.transient.screen_mutations);
    let keyframe = carried.checkpoint_continuing().expect("ground");
    let mut replayed = Terminal::replay_from_checkpoint(&keyframe);
    for t in [&mut carried, &mut replayed] {
        t.process(b"more\r\n");
    }
    assert_ne!(
        replayed.checkpoint_continuing(),
        carried.checkpoint_continuing(),
        "a pending restore shows once output comes"
    );
}

/// A fresh engine for `fx` on a tiered store kept to `store_lines` lines past its ring, as the Rewind view folds on.
fn bounded(fx: &Fixture, store_lines: usize) -> Terminal {
    let blank = Terminal::new(fx.rows, fx.cols).checkpoint();
    let mut t = Terminal::replay_from_checkpoint_bounded(&blank, store_lines);
    fx.configure(&mut t);
    t
}

#[test]
fn a_fork_of_a_bounded_restore_folds_the_rest_as_the_straight_fold_does() {
    let mut all = fixtures();
    all.push(long());
    for fx in &all {
        let store_lines = if fx.name == "long" { 300 } else { 2048 };
        let premise = bounded(fx, store_lines);
        assert!(
            premise.grid().scrollback().is_some(),
            "premise: a tiered store"
        );
        assert_forks_fold_the_straight_end(
            fx,
            &fx.at_every_boundary_from(|| bounded(fx, store_lines), Terminal::replay_fork),
        );
    }
    let fx = long();
    let mut evicted = bounded(&fx, 300);
    fx.apply(&mut evicted, fixture_epoch(), 0);
    assert!(
        evicted.grid().scrollback_lines() < 1440 - 12,
        "premise: the small store evicted"
    );
}

/// Each fixture restored at every keyframe boundary onto an unbounded tiered store and onto one kept to `store_lines`, both folded through the rest.
fn bounded_against_unbounded(fx: &Fixture, store_lines: usize) -> Vec<(Terminal, Terminal)> {
    let epoch = fixture_epoch();
    let mut src = fx.engine();
    let mut out = Vec::new();
    for (k, ev) in fx.events.iter().enumerate() {
        match ev {
            Ev::Out(bytes) => src.process_at(bytes, clock(epoch, k)),
            Ev::Resize(rows, cols) => src.resize_with_policy(*rows, *cols, fx.policy),
        }
        let Some(keyframe) = src.checkpoint_continuing() else {
            continue;
        };
        let mut unbounded = Terminal::replay_from_checkpoint(&keyframe);
        let mut kept = Terminal::replay_from_checkpoint_bounded(&keyframe, store_lines);
        for t in [&mut unbounded, &mut kept] {
            fx.configure(t);
            fx.apply(t, epoch, k + 1);
        }
        out.push((kept, unbounded));
    }
    out
}

#[test]
fn a_bounded_restore_folds_the_rest_as_the_unbounded_restore_does() {
    let mut all = fixtures();
    all.push(long());
    for fx in &all {
        let compared = bounded_against_unbounded(fx, 2048);
        assert!(!compared.is_empty(), "{}: no keyframe boundary", fx.name);
        for (k, (kept, unbounded)) in compared.iter().enumerate() {
            assert_eq!(
                seen(kept),
                seen(unbounded),
                "{}: restored at boundary {k}",
                fx.name
            );
        }
    }
    let fx = long();
    let small = bounded_against_unbounded(&fx, 300);
    let mut renumbered = 0;
    for (k, (kept, unbounded)) in small.iter().enumerate() {
        let (Some(mut kept), Some(unbounded)) =
            (kept.checkpoint_visible(), unbounded.checkpoint_visible())
        else {
            panic!("long: at ground at boundary {k}");
        };
        if kept.absolute_row_counter != unbounded.absolute_row_counter {
            renumbered += 1;
            kept.absolute_row_counter = unbounded.absolute_row_counter;
        }
        assert_eq!(
            kept, unbounded,
            "long: what a witness compares but the row counter, restored at boundary {k}"
        );
    }
    assert!(
        renumbered > 0,
        "the known gap: a narrowing raises the row counter over the history each store keeps, so a small store renumbers differently"
    );
    assert!(
        small
            .iter()
            .any(|(kept, unbounded)| seen(kept).checkpoint != seen(unbounded).checkpoint),
        "negative control: a store kept to 300 lines holds other history"
    );
}

#[test]
fn a_fork_verifies_the_marks_its_source_does() {
    const KEY: [u8; 32] = [0x6B; 32];
    let id = crate::terminal::ShellIntegrationNonce(KEY).to_hex();
    let mark = |m: &str| format!("\x1b]133;{m};id={id}\x07");
    // One clock for every engine, so a mark's recorded time cannot tell them apart.
    let epoch = fixture_epoch();
    let mut src = Terminal::new(6, 30);
    src.authorize_shell_integration(KEY);
    src.set_require_shell_integration_nonce(true);
    src.process_at(
        format!("{}$ {}ls\r\n{}", mark("A"), mark("B"), mark("C")).as_bytes(),
        clock(epoch, 0),
    );
    let mut fork = src.replay_fork().expect("at ground with no image");
    let mut keyless = Terminal::from_checkpoint(&src.checkpoint());
    let rest = format!("file\r\n{}{}$ {}", mark("D;0"), mark("A"), mark("B"));
    for t in [&mut src, &mut fork, &mut keyless] {
        t.process_at(rest.as_bytes(), clock(epoch, 1));
    }
    assert_eq!(
        seen(&fork),
        seen(&src),
        "the fork verifies the marks after it"
    );
    assert_ne!(
        seen(&keyless).checkpoint,
        seen(&src).checkpoint,
        "negative control: an engine without the key drops them"
    );
}

#[test]
fn a_replay_restore_with_carry_folds_the_rest_as_the_straight_fold_does() {
    let unbounded = |fx: &Fixture| {
        let blank = Terminal::new(fx.rows, fx.cols).checkpoint();
        let mut t = Terminal::replay_from_checkpoint(&blank);
        if fx.ring.is_some() {
            t.set_scrollback_line_limit(fx.ring);
        }
        fx.configure(&mut t);
        t
    };
    let restore_with_carry = |t: &Terminal| {
        t.replay_fork()?;
        let c = t.checkpoint_continuing()?;
        let carry = t.capture_replay_carry();
        let mut r = Terminal::replay_from_checkpoint(&c);
        r.apply_replay_carry(&carry);
        Some(r)
    };
    let mut all = fixtures();
    all.push(long());
    for fx in &all {
        let at = fx.at_every_boundary_from(|| unbounded(fx), restore_with_carry);
        assert_forks_fold_the_straight_end(fx, &at);
    }

    // Without `replay_fork`'s `kitty_idle` gate, both `ReplayCarry` and `PaneStepCarry`
    // (through canonical encode/decode) fold `kitty` and `live_images` across every boundary.
    let origin = fixture_epoch();
    for fx in [kitty(), live_images()] {
        let (end_rc, ends_rc) = fx.at_every_boundary_from(
            || unbounded(&fx),
            |t| {
                let c = t.checkpoint_continuing()?;
                let carry = t.capture_replay_carry();
                let mut r = Terminal::replay_from_checkpoint(&c);
                r.apply_replay_carry(&carry);
                Some(r)
            },
        );
        for (k, fork_end) in ends_rc.iter().enumerate() {
            let fork_end = fork_end
                .as_ref()
                .unwrap_or_else(|| panic!("{}: ReplayCarry refused boundary {}", fx.name, k + 1));
            assert_eq!(
                fork_end,
                &end_rc,
                "{}: ReplayCarry after {} events",
                fx.name,
                k + 1
            );
        }

        let (end_pc, ends_pc) = fx.at_every_boundary_from(
            || unbounded(&fx),
            |t| {
                let c = t.checkpoint_continuing()?;
                let carry = crate::terminal::PaneStepCarry::capture(t, origin, usize::MAX);
                let wire = carry.canonical();
                let decoded = crate::terminal::PaneStepCarry::from_canonical(&wire)
                    .expect("PaneStepCarry canonical roundtrip");
                assert_eq!(decoded, carry, "PaneStepCarry equality");
                let mut r = Terminal::replay_from_checkpoint(&c);
                fx.configure(&mut r);
                r.apply_pane_step_carry(&decoded, origin);
                Some(r)
            },
        );
        for (k, fork_end) in ends_pc.iter().enumerate() {
            let fork_end = fork_end
                .as_ref()
                .unwrap_or_else(|| panic!("{}: PaneStepCarry refused boundary {}", fx.name, k + 1));
            assert_eq!(
                fork_end,
                &end_pc,
                "{}: PaneStepCarry after {} events",
                fx.name,
                k + 1
            );
        }
    }

    // Mid-sequence inside an open mode-2026 frame: `replay_fork` refuses off Ground, while
    // `checkpoint_continuing` + `capture_replay_carry` carries both the partial CSI and `sync_start`.
    let epoch = fixture_epoch();
    let mut live = Terminal::new(6, 30);
    live.process_at(b"\x1b[?2026h\x1b[31", clock(epoch, 0));
    assert!(live.replay_fork().is_none(), "premise: off Ground");
    let mid_cp = live.checkpoint_continuing().expect("carried CSI");
    let mid_carry = live.capture_replay_carry();
    let mut carried = Terminal::replay_from_checkpoint(&mid_cp);
    carried.apply_replay_carry(&mid_carry);
    let mut bare = Terminal::replay_from_checkpoint(&mid_cp);
    for t in [&mut live, &mut carried, &mut bare] {
        t.process_at(b"mred", clock(epoch, 500));
    }
    assert_eq!(seen(&carried), seen(&live), "mid-sequence sync window");
    assert_ne!(
        seen(&bare),
        seen(&live),
        "negative control: re-arming at Instant::now() expires against the recorded clock"
    );

    // PolicyEngine response and palette buckets ride `ReplayCarry`, not a checkpoint.
    let mut drain_bucket = b"\x1b]11;#000000\x07\x1b[?2031h".to_vec();
    for _ in 0..20_000 {
        drain_bucket.extend_from_slice(b"\x1b[6n");
    }
    for _ in 0..8 {
        drain_bucket.extend_from_slice(b"\x1b[5n");
    }
    let mut policy_live = Terminal::new(6, 30);
    policy_live.apply_policy_engine(aterm_policy::engine::PolicyEngine::new(
        aterm_policy::profiles::standard(),
    ));
    policy_live.process_at(&drain_bucket, clock(epoch, 0));
    let _ = policy_live.take_response();
    let pol_cp = policy_live.checkpoint_continuing().expect("ground");
    let pol_carry = policy_live.capture_replay_carry();
    let mut pol_carried = Terminal::replay_from_checkpoint(&pol_cp);
    pol_carried.apply_replay_carry(&pol_carry);
    let mut pol_bare = Terminal::replay_from_checkpoint(&pol_cp);
    for t in [&mut policy_live, &mut pol_carried, &mut pol_bare] {
        t.process_at(b"\x1b]11;#ffffff\x07", clock(epoch, 0));
    }
    assert_eq!(seen(&pol_carried), seen(&policy_live), "policy bucket");
    assert_ne!(
        seen(&pol_bare),
        seen(&policy_live),
        "negative control: bare replay starts with an unthrottled bucket"
    );

    // Pending `authorize_shell_integration_on_first_mark` re-key: `nonce_in_use()` is `None`
    // before the first signed mark, so `checkpoint_carry` carries no nonce while `ReplayCarry` does.
    const PENDING_KEY: [u8; 32] = [0x9C; 32];
    let hex = crate::terminal::ShellIntegrationNonce(PENDING_KEY).to_hex();
    let signed = format!("\x1b]133;A;id={hex}\x07$ ");
    let mut pending_live = Terminal::new(6, 30);
    pending_live.set_require_shell_integration_nonce(true);
    pending_live.authorize_shell_integration_on_first_mark(PENDING_KEY);
    assert!(pending_live.carried_shell_integration_nonce().is_none());
    let pend_cp = pending_live.checkpoint_continuing().expect("ground");
    let pend_carry = pending_live.capture_replay_carry();
    let mut pend_carried = Terminal::replay_from_checkpoint(&pend_cp);
    pend_carried.apply_replay_carry(&pend_carry);
    let mut pend_bare = Terminal::replay_from_checkpoint(&pend_cp);
    for t in [&mut pending_live, &mut pend_carried, &mut pend_bare] {
        t.process_at(signed.as_bytes(), clock(epoch, 0));
    }
    assert_eq!(seen(&pend_carried), seen(&pending_live), "pending re-key");
    assert_ne!(
        seen(&pend_bare),
        seen(&pending_live),
        "negative control: bare replay drops the first signed mark"
    );

    // Pending restore lineage (`Untouched` and `Touched`): `ReplayCarry` preserves both, whereas
    // `replay_from_checkpoint` (`None`) loses `Untouched` once output or adoption follows.
    for (before, after, adopt) in [
        (false, false, true),
        (false, true, false),
        (true, false, true),
        (true, true, false),
    ] {
        let label = format!("before: {before}, after: {after}, adopt: {adopt}");
        let mut src = Terminal::new(6, 30);
        src.note_holder_cut(crate::terminal::HolderCut {
            to: Some(7),
            kind: crate::terminal::CutKind::Group,
            from_alive: false,
        });
        src.process(b"\x1b[1mbefore\x1b[m the restore\r\n");
        src.holder_carried_at = Some(src.transient.screen_mutations);
        if before {
            src.process(b"replayed\r\n");
        }
        let cp = src.checkpoint_continuing().expect("ground");
        let carry = src.capture_replay_carry();
        let mut with_carry = Terminal::replay_from_checkpoint(&cp);
        with_carry.apply_replay_carry(&carry);
        let mut without_carry = Terminal::replay_from_checkpoint(&cp);
        let finish = |t: &mut Terminal| {
            if after {
                t.process(b"more\r\n");
            }
            let adoption = adopt.then(|| t.adopt_holder_after_restore(7, true));
            (adoption, t.checkpoint_continuing().expect("ground"))
        };
        let src_end = finish(&mut src);
        assert_eq!(finish(&mut with_carry), src_end, "{label}");
        if !before {
            assert_ne!(
                finish(&mut without_carry),
                src_end,
                "{label}: negative control without carry"
            );
        }
    }
}

#[test]
fn unbooked_image_bytes_counts_kitty_store_variants_and_visible_images_without_double_counting_history()
 {
    let mut t = Terminal::new(4, 20);
    t.set_cell_pixel_size(10, 20);
    assert_eq!(t.unbooked_image_bytes(), 0);

    // 1. Place an inline OSC 1337 image (`ABCDEFG`) on row 0 (visible, not yet in ring/scrollback).
    t.process(b"\x1b]1337;File=inline=1;width=2;height=1;preserveAspectRatio=0:QUJDREVGRw==\x1b\\");
    let inline_bytes = t.unbooked_image_bytes();
    assert!(inline_bytes >= 7, "{inline_bytes}");

    // 2. Load a Kitty 2x1 RGBA image (8 bytes) into `i=3` and place it at two sizes so a cached
    //    scaled variant is also retained on `transient.kitty_variants`.
    t.process(
        b"\x1b_Ga=t,f=32,s=2,v=1,i=3,q=2;////AAAA//8=\x1b\\\
          \x1b[2;1H\x1b_Ga=p,i=3,p=1,c=1,r=1,q=2\x1b\\\
          \x1b[2;4H\x1b_Ga=p,i=3,p=2,c=3,r=1,q=2\x1b\\",
    );
    let _ = t.images_row(1);
    let with_store_and_visible = t.unbooked_image_bytes();
    assert!(
        with_store_and_visible > inline_bytes + 8,
        "includes inline + Kitty source + scaled variant without double-counting shared Arcs: {with_store_and_visible}"
    );
    let fork = t.replay_fork().expect("fork with Kitty store and variants");
    assert_eq!(fork.unbooked_image_bytes(), with_store_and_visible);

    // 3. Scroll row 0's inline image off screen into history: `grid().memory_used()` now books its
    //    payload capacity, while `unbooked_image_bytes()` drops `inline_bytes`.
    let grid_before = t.grid().memory_used();
    t.process(b"\x1b[4;1H\r\n\r\n\r\n\r\n");
    assert!(t.grid().memory_used() > grid_before);
    assert_eq!(
        t.unbooked_image_bytes(),
        with_store_and_visible - inline_bytes
    );
}

#[test]
fn kitty_media_admitted_carries_across_replay_fork_and_replay_carry() {
    let epoch = aterm_time::Instant::now();
    let mut live = Terminal::new(4, 20);
    live.set_kitty_file_resolver(|_, _| None);
    assert!(live.kitty_file_resolver_installed());

    let cp = live.checkpoint_continuing().expect("ground");
    let carry = live.capture_replay_carry();
    assert!(carry.kitty_media_admitted());

    let mut fork = live.replay_fork().expect("fork");
    assert!(fork.kitty_file_resolver_installed());

    let mut carried = Terminal::replay_from_checkpoint(&cp);
    carried.apply_replay_carry(&carry);
    assert!(carried.kitty_file_resolver_installed());

    let mut bare = Terminal::replay_from_checkpoint(&cp);
    assert!(!bare.kitty_file_resolver_installed());

    // Capability query `a=q,t=f` answers `OK` (no host read) and missing-file `a=T,t=f` answers
    // `EINVAL:bad image data` when admitted, vs `ENOTSUPPORTED` on an uncarried replay.
    let probe = b"\x1b_Gi=31,a=q,t=f\x1b\\\x1b_Gi=32,a=T,t=f,f=32,s=1,v=1;L21pc3Npbmc=\x1b\\";
    for engine in [&mut live, &mut fork, &mut carried, &mut bare] {
        engine.process_at(probe, clock(epoch, 0));
    }
    let expected = b"\x1b_Gi=31;OK\x1b\\\x1b_Gi=32;EINVAL:bad image data\x1b\\".to_vec();
    let live_reply = live.take_response().expect("live reply");
    assert_eq!(live_reply, expected);
    assert_eq!(fork.take_response().as_ref(), Some(&live_reply));
    assert_eq!(carried.take_response().as_ref(), Some(&live_reply));
    assert_eq!(
        fork.capture_replay_carry().earliest_instant(),
        live.capture_replay_carry().earliest_instant()
    );

    let bare_reply = bare.take_response().expect("bare reply");
    assert_ne!(
        bare_reply, live_reply,
        "negative control: bare replay answers ENOTSUPPORTED instead of OK / EINVAL"
    );
}

#[test]
fn host_io_ordinal_advances_only_when_live_host_reads_succeed() {
    use crate::terminal::{ClipboardAccess, ClipboardOperation};
    use aterm_types::{WindowOperation, WindowResponse};

    let epoch = aterm_time::Instant::now();
    let mut term = Terminal::new(4, 20);
    assert_eq!(term.host_io_ordinal(), 0);

    // Direct `t=d`, disabled `t=f`, capability queries `a=q,t=f`, and missing/malformed `t=f`
    // leave `host_io_ordinal` unmoved because pure replay reproduces them identically.
    term.process_at(
        b"\x1b_Gi=1,a=T,f=32,s=1,v=1;////\x1b\\\x1b_Gi=2,a=T,t=f,f=32,s=1,v=1;L21pc3Npbmc=\x1b\\",
        clock(epoch, 0),
    );
    assert_eq!(term.host_io_ordinal(), 0);

    term.set_kitty_file_resolver(|_, name| match name {
        "/good.raw" => Some(vec![0xff, 0, 0, 0xff]),
        "/bad.raw" => Some(vec![0xff]), // wrong byte length for 1x1 RGBA -> None
        _ => None,
    });
    term.process_at(
        b"\x1b_Gi=3,a=q,t=f\x1b\\\
          \x1b_Gi=4,a=T,t=f,f=32,s=1,v=1;L21pc3Npbmc=\x1b\\\
          \x1b_Gi=5,a=T,t=f,f=32,s=1,v=1;L2JhZC5yYXc=\x1b\\",
        clock(epoch, 10),
    );
    assert_eq!(term.host_io_ordinal(), 0);

    // Successful `t=f` transmit (`L2dvb2QucmF3` = "/good.raw") and `a=f` frame load each bump it.
    term.process_at(
        b"\x1b_Gi=6,a=t,t=f,f=32,s=1,v=1;L2dvb2QucmF3\x1b\\",
        clock(epoch, 20),
    );
    assert_eq!(term.host_io_ordinal(), 1);
    term.process_at(
        b"\x1b_Gi=6,a=f,t=f,f=32,s=1,v=1,z=40;L2dvb2QucmF3\x1b\\",
        clock(epoch, 30),
    );
    assert_eq!(term.host_io_ordinal(), 2);

    // Denied or empty-returning `OSC 52 ; ?` leaves it at 2; an answered query advances to 3.
    term.process_at(b"\x1b]52;c;?\x07", clock(epoch, 40));
    assert_eq!(term.host_io_ordinal(), 2);
    term.authorize_clipboard_access(ClipboardAccess::Query);
    term.set_clipboard_callback(|op| match op {
        ClipboardOperation::Query { .. } => None,
        _ => None,
    });
    term.process_at(b"\x1b]52;c;?\x07", clock(epoch, 50));
    assert_eq!(term.host_io_ordinal(), 2);

    term.set_clipboard_callback(|op| match op {
        ClipboardOperation::Query { .. } => Some("clip".to_string()),
        _ => None,
    });
    let _ = term.take_response();
    term.process_at(b"\x1b]52;c;?\x07", clock(epoch, 2_000));
    assert_eq!(term.host_io_ordinal(), 3);

    // RIS (`ESC c`) preserves the monotonic ordinal, and a `Some(WindowResponse)` advances it.
    term.process_at(b"\x1bc", clock(epoch, 2_100));
    assert_eq!(term.host_io_ordinal(), 3);
    term.modes_mut().allow_window_ops = true;
    term.set_window_callback(|op| match op {
        WindowOperation::ReportWindowState => Some(WindowResponse::WindowState(false)),
        _ => None,
    });
    term.process_at(b"\x1b[11t", clock(epoch, 4_000));
    assert_eq!(term.host_io_ordinal(), 4);
}
