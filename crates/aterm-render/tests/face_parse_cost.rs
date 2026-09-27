// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHAT PARSING A FACE COSTS, first-party against `fontdue` — measured in this
//! process, with a counting allocator, on the same bytes.
//!
//! The numbers matter beyond bragging: the whole deferred-parse design
//! (`LazyFontdue`, `FallbackFace::from_path_bytes`, the styled tier's
//! materialise-on-first-draw) exists because the parse it deferred cost SECONDS
//! and TENS OF MEGABYTES per broad face. Retiring `fontdue` changed both terms by
//! orders of magnitude, and a design justified by a number that moved that far
//! deserves the new number written down rather than inferred.
//!
//! WHY IT IS AN INEQUALITY AND NOT A THRESHOLD. Absolute megabytes and
//! milliseconds depend on the machine, the allocator and the optimization level,
//! so a constant here would be a flake generator. What is machine-independent is
//! the SHAPE: fontdue converts every glyph's outline to line segments at parse
//! time and keeps them; the first-party face touches no outline at all. So it
//! must be strictly cheaper on both axes, by a wide margin, on every face — and
//! that is what is asserted. `--ignored --nocapture` prints the table.
//!
//! The allocator shim counts the LIVE bytes of everything that goes through
//! Rust's global allocator, which is every `Vec`/`Box`/`HashMap`/`Arc` in both
//! implementations. It cannot see mapped files or allocations a C library makes
//! directly; neither implementation has any.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicI64, Ordering};

use aterm_render::font::{Font, FontSettings};

const DEJAVU: &[u8] = include_bytes!("../assets/DejaVuSansMono.ttf");
const NERD: &[u8] = include_bytes!("../assets/SymbolsNerdFontMono-Regular.ttf");

/// Live bytes, in this process, across every thread.
static LIVE: AtomicI64 = AtomicI64::new(0);

/// A pass-through allocator that keeps a running live-byte total.
struct Counting;

// SAFETY: every method forwards to `System` with the same layout it was given
// and returns exactly what `System` returned; the counter is a side effect on an
// atomic and touches no allocation.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            LIVE.fetch_add(layout.size() as i64, Ordering::Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size() as i64, Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            LIVE.fetch_add(new_size as i64 - layout.size() as i64, Ordering::Relaxed);
        }
        p
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc_zeroed(layout) };
        if !p.is_null() {
            LIVE.fetch_add(layout.size() as i64, Ordering::Relaxed);
        }
        p
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// Live-heap delta and the parse's TIME for one parse, with the parsed value
/// dropped INSIDE the measurement of neither: the value is handed back so the
/// caller reads the delta while it is still alive, which is the number that
/// matters (what a resident face costs), not the transient peak.
///
/// THE TIME IS THIS THREAD'S CPU TIME ([`parse_clock_ms`]), not the wall clock.
/// The parse runs on this one thread, so its CPU time IS its cost; the wall
/// clock adds every moment the thread sat descheduled, which is the machine's
/// load and not either parser's shape. Measured 2026-09-26 on the owner's
/// machine, the full verify gate beside a peer's test run: `Apple Symbols.ttf`
/// read 50.372 ms (first-party) against 62.529 ms (fontdue) on the wall clock
/// — 1.24x, under [`TIME_MARGIN`], on a face that reads 1.7x-2.3x quiet — and
/// the guard went red on an unrelated change; the same test alone passed three
/// runs of three. The minimum of [`TIME_REPS`] rounds cannot help when the
/// load outlasts all of them.
fn cost<T>(build: impl FnOnce() -> T) -> (T, i64, f64) {
    let before = LIVE.load(Ordering::Relaxed);
    let t0 = parse_clock_ms();
    let value = build();
    let elapsed = parse_clock_ms() - t0;
    let after = LIVE.load(Ordering::Relaxed);
    (value, after - before, elapsed)
}

/// Milliseconds of CPU time the calling thread has run
/// (`CLOCK_THREAD_CPUTIME_ID`), for [`cost`]: time spent descheduled does not
/// count.
#[cfg(unix)]
fn parse_clock_ms() -> f64 {
    let mut stamp = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `stamp` is initialized and writable for this one call.
    let rc = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut stamp) };
    assert_eq!(rc, 0, "the thread CPU clock is readable");
    std::time::Duration::new(
        u64::try_from(stamp.tv_sec).expect("a non-negative CPU time"),
        u32::try_from(stamp.tv_nsec).expect("nanoseconds under a second"),
    )
    .as_secs_f64()
        * 1e3
}

/// Off unix there is no thread CPU clock in `libc`: the wall clock since the
/// first reading, with the load-sensitivity [`cost`] describes.
#[cfg(not(unix))]
fn parse_clock_ms() -> f64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_secs_f64()
        * 1e3
}

/// THE GUARD'S CLOCK DOES NOT COUNT TIME THE THREAD IS OFF THE CPU — the
/// failure [`cost`] records. Measured on the owner's machine with the real
/// parses: this test process STOPPED for 400 ms (`kill -STOP` / `-CONT`) inside
/// the first-party parse of `STHeiti Light.ttc` read 0.961x fontdue on the wall
/// clock — the guard would fail on an unchanged tree — and 1.343x on this
/// clock, its quiet value. The property is pinned here without stopping the
/// process (a helper lost between the stop and the resume would leave the whole
/// test binary stopped): a thread asleep is off the CPU exactly as a preempted
/// one is, so work, a 300 ms sleep, and work again must advance
/// [`parse_clock_ms`] by the work alone. NEGATIVE CONTROL: with the wall clock
/// in [`parse_clock_ms`] this fails ("the time off the CPU was counted").
#[cfg(unix)]
#[test]
fn the_parse_clock_does_not_count_time_the_thread_is_off_the_cpu() {
    const OFF_MS: u64 = 300;
    let work = |n: u64| {
        let mut x: u64 = 1;
        for i in 0..n {
            x = std::hint::black_box(x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(i));
        }
        x
    };
    let wall = std::time::Instant::now();
    let cpu = parse_clock_ms();
    work(2_000_000);
    std::thread::sleep(std::time::Duration::from_millis(OFF_MS));
    work(2_000_000);
    let (cpu_ms, wall_ms) = (parse_clock_ms() - cpu, wall.elapsed().as_secs_f64() * 1e3);
    assert!(
        wall_ms - cpu_ms >= OFF_MS as f64 * 0.8,
        "the time off the CPU was counted: {cpu_ms:.1} ms on the parse clock across \
         {wall_ms:.1} ms of wall time with a {OFF_MS} ms sleep inside it"
    );
}

/// Faces to weigh: the two embedded ones always, plus whatever broad system
/// faces this machine happens to have (they are where the old numbers came
/// from, and they are the ones that made startup slow).
fn subjects() -> Vec<(String, Vec<u8>)> {
    let mut out = vec![
        ("DejaVuSansMono (embedded)".to_string(), DEJAVU.to_vec()),
        ("SymbolsNerdFontMono (embedded)".to_string(), NERD.to_vec()),
    ];
    for path in [
        "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
        "/System/Library/Fonts/STHeiti Light.ttc",
        "/System/Library/Fonts/Apple Symbols.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    ] {
        if let Ok(bytes) = std::fs::read(path) {
            out.push((format!("{path} ({} MB)", bytes.len() / 1_048_576), bytes));
        }
    }
    out
}

/// Timing rounds per face; the byte readings come from round 0 alone, so this
/// costs only wall clock (~7.5 s to ~19 s).
const TIME_REPS: usize = 3;

/// How much cheaper the first-party parse must be in TIME, and where the number
/// comes from — measured on this box, at both profiles, rather than chosen.
///
/// It was `1.5`, justified in this file by "at opt-level 0 the ratio is ~1.9x".
/// That average is not the guard's operating point; the WORST face is, and the
/// worst face misses it. Measured fd/ours on
/// `/System/Library/Fonts/STHeiti Light.ttc` (53 MB, the widest collection this
/// machine carries):
///
/// ```text
///   opt-level 0, quiet     1.502  1.503  1.514  1.516  1.521
///   opt-level 0, 20-way build running concurrently
///                          1.610  1.601  1.500
///   opt-level 2            3.08
///   every OTHER face, opt-level 0     1.72x - 2.34x
///   every OTHER face, opt-level 2     3.90x - 5.59x
/// ```
///
/// So `1.5` sat ON the worst reading, with under 1% of headroom, and a
/// concurrent build had already driven it under (1.493, the RED this replaces).
///
/// WHY opt-level 0 IS THE HARD CASE, and why that is an artifact rather than a
/// property: unoptimized, the first-party cmap walk runs ~21x slower than its
/// optimized self while fontdue's allocation-bound outline conversion runs only
/// ~10x slower than ITS optimized self. The default `cargo test` profile
/// therefore erodes exactly the advantage this guard measures — the shipped
/// profile shows the real shape at 3.1x-5.6x — and the test cannot see which
/// profile it is in.
///
/// RE-MEASURED 2026-09-27 on main (7a93e792a), five rounds each, both clocks
/// agreeing to 0.1%: `STHeiti Light.ttc` 1.327x-1.341x, `Apple Symbols.ttf`
/// 1.54x, `Arial Unicode.ttf` 2.04x. The widest face's operating point has
/// fallen from ~1.50x to ~1.33x-1.38x since this margin was set, so its
/// headroom over `1.25` is ~7-10%, not 20%.
///
/// ATTRIBUTED the same day, not by reading history but by an interleaved A/B
/// on ONE machine and one font file, quiet (load 2-5 on 18 cores), three or
/// more rounds per build: `8a968d21e`, where the 1.50 above was derived,
/// reads 1.43x-1.44x on this box (the 1.50 was another machine's reading);
/// the same tree rebuilt with only main's `[profile.dev] debug =
/// "line-tables-only"` (01ac0f37a, 2026-09-25) reads 1.37x, as main does.
/// Full DWARF made both parses slower at the test profile — fontdue's more,
/// so dropping it narrowed the ratio — and the parse body, the hasher and
/// both crates' versions are unchanged since. So none of the fall is the
/// parse; the next one would be, and is what this guard exists to notice.
///
/// `1.25` is set BELOW the worst reading at the worst profile with 20% of room,
/// and it still catches the regression this exists for: if the first-party face
/// ever started converting outlines at parse time, its time would converge on
/// fontdue's and the ratio would fall toward 1.0, failing here immediately and
/// failing the (far wider) memory limb harder still.
const TIME_MARGIN: f64 = 1.25;

/// THE GUARD. Strictly cheaper on both axes, on every face available.
///
/// The margins measure the structural difference (outlines converted eagerly
/// versus not converted at all) and not the profile it happens to run under:
/// 2x on memory, [`TIME_MARGIN`] on time, each derived at its own definition.
/// The MEMORY ratio is a property of what each implementation KEEPS, barely
/// moves with the profile (it reads 5.8x on the widest face and 22.6x on the
/// narrowest against a 2x bound), and is the limb carrying most of the catch.
/// The time ratio swings from 1.5x to 5.6x with the optimization level alone,
/// which is why its margin is the looser one and why [`TIME_MARGIN`] writes out
/// where it was measured.
///
/// # Why the time is a MINIMUM over [`TIME_REPS`] rounds, not one shot
///
/// It was one shot, and one shot is a coin flip on the widest face this box
/// has. Measured quiet, `/System/Library/Fonts/STHeiti Light.ttc` (53 MB) runs
/// 1861/1238.9, 1936.9/1288.3 and 1940.5/1281.7 ms — ratios of 1.502, 1.503 and
/// 1.514 against a 1.5x margin, i.e. under 1% of headroom — while every other
/// face sits at 1.7x-2.3x. Under a concurrent build the same row read
/// 1926.3/1289.9 = 1.493 and the guard went RED, which is the gate reporting
/// the machine's load rather than the code's shape.
///
/// The estimator is the fix, not the margin. Scheduler preemption, cache
/// pressure and a neighbouring compile only ever ADD time, so the minimum of
/// several rounds is the closest available reading of the true cost and it is
/// the one that does not move with what else is running. The 1.5x claim is
/// unchanged; only the noise under it is. Since 2026-09-27 the CLOCK takes out
/// preemption itself (thread CPU time, [`cost`]) — three rounds could not when
/// the load outlasted all three — and the minimum is left the cache and memory
/// traffic a neighbour adds to the parse's own time.
///
/// The rounds also ALTERNATE which implementation goes first, which the comment
/// below has always promised and the code did not do: the allocator's arena
/// state is not symmetric between the two orders, and measuring only one order
/// flatters whichever ran second.
#[test]
fn the_first_party_face_parses_cheaper_than_fontdue_on_every_face() {
    let subjects = subjects();
    assert!(subjects.len() >= 2, "no faces to weigh");
    let mut rows = Vec::new();
    for (name, bytes) in &subjects {
        // The SHARING constructor, measured with the file's handle already in
        // hand — the shape every byte store in the crate uses. The delta against
        // the copying row below is exactly the file.
        let shared: std::sync::Arc<[u8]> = std::sync::Arc::from(bytes.as_slice());
        let (adopted, adopted_bytes, adopted_ms) = cost(|| {
            Font::from_shared_slice(shared.clone(), FontSettings::default())
                .expect("first-party face parses")
        });
        drop(adopted);
        // ROUND 0 carries the BYTE readings: `cost` reads the live-heap delta
        // while the parsed value is still alive, so the two faces have to be
        // held here rather than dropped inside a loop. fontdue first, ours
        // second; the later rounds swap that order.
        let (theirs, their_bytes, mut their_ms) = cost(|| {
            fontdue::Font::from_bytes(bytes.as_slice(), fontdue::FontSettings::default())
                .expect("fontdue parses")
        });
        let (ours, our_bytes, mut our_ms) = cost(|| {
            Font::from_bytes(bytes.as_slice(), FontSettings::default()).expect("first-party parses")
        });
        // THE SURVEY'S REFERENCE POINT. `docs/THIRD_PARTY_ROAD_TO_ZERO.md`
        // priced this row as "1 438-1 799 ms per face for fontdue against 43 us
        // for `ttf_parser::Face::parse`", and that ratio is the largest
        // non-LOC result claimed for Phase A — so it is measured here rather
        // than quoted. `Face::parse` validates the table directory and defers
        // everything else, which is the FLOOR any first-party face must sit
        // above: this crate's `Font` adds the cmap enumeration and the `glyf`
        // bbox table on top of exactly this call.
        let (ttf, ttf_bytes, mut ttf_ms) =
            cost(|| ttf_parser::Face::parse(bytes.as_slice(), 0).expect("ttf-parser parses"));
        // Read something out of the parsed face so the parse cannot be
        // optimized away, then let it fall out of scope — `Face` borrows the
        // bytes and owns no heap, so there is nothing to drop.
        let ttf_upem = ttf.units_per_em();
        // The big faces are held one at a time from here on: fontdue keeps
        // 323 MB for the 53 MB collection, and three rounds' worth alive at
        // once would measure the allocator rather than the parse.
        drop(theirs);
        drop(ours);
        // THE REMAINING TIMING ROUNDS. Each keeps the MINIMUM, and each swaps
        // which implementation runs first so neither is only ever measured on
        // an arena the other warmed. Nothing here reads a byte delta — round 0
        // already did, and these values are dropped before the next parse.
        for rep in 1..TIME_REPS {
            let (round_our_ms, round_their_ms) = if rep % 2 == 1 {
                let (o, _, om) = cost(|| {
                    Font::from_bytes(bytes.as_slice(), FontSettings::default())
                        .expect("first-party parses")
                });
                drop(o);
                let (t, _, tm) = cost(|| {
                    fontdue::Font::from_bytes(bytes.as_slice(), fontdue::FontSettings::default())
                        .expect("fontdue parses")
                });
                drop(t);
                (om, tm)
            } else {
                let (t, _, tm) = cost(|| {
                    fontdue::Font::from_bytes(bytes.as_slice(), fontdue::FontSettings::default())
                        .expect("fontdue parses")
                });
                drop(t);
                let (o, _, om) = cost(|| {
                    Font::from_bytes(bytes.as_slice(), FontSettings::default())
                        .expect("first-party parses")
                });
                drop(o);
                (om, tm)
            };
            our_ms = our_ms.min(round_our_ms);
            their_ms = their_ms.min(round_their_ms);
            let (f, _, fm) =
                cost(|| ttf_parser::Face::parse(bytes.as_slice(), 0).expect("ttf-parser parses"));
            assert!(f.units_per_em() > 0, "{name}: ttf-parser read a zero upem");
            ttf_ms = ttf_ms.min(fm);
        }
        assert!(ttf_upem > 0, "{name}: ttf-parser read a zero upem");
        assert!(
            our_bytes * 2 < their_bytes,
            "{name}: the first-party face kept {our_bytes} B against fontdue's \
             {their_bytes} B — the eager-outline gap is gone"
        );
        assert!(
            our_ms * TIME_MARGIN < their_ms,
            "{name}: the first-party parse took {our_ms:.3} ms against fontdue's \
             {their_ms:.3} ms — under the {TIME_MARGIN}x this guard derives (see \
             `TIME_MARGIN`), which is the shape claim collapsing, not the box \
             being busy: both figures are already the MINIMUM of {TIME_REPS} \
             order-alternating rounds"
        );
        assert!(
            adopted_bytes < our_bytes,
            "{name}: the sharing constructor kept {adopted_bytes} B and the \
             copying one {our_bytes} B — the adoption is not happening"
        );
        // The floor and the ceiling of this row, as an ORDER-OF-MAGNITUDE
        // statement rather than a threshold: the lazy table-directory parse is
        // at least 50x cheaper than converting every outline in the file. The
        // survey's own ratio is ~33 000x (1.4 s against 43 us) and was taken at
        // opt-level 0; 50x is what survives an optimized build on the smallest
        // face here, and it is still the structural claim.
        assert!(
            ttf_ms * 50.0 < their_ms,
            "{name}: ttf-parser's table-directory parse took {ttf_ms:.4} ms \
             against fontdue's {their_ms:.3} ms — under the 50x the deferred \
             design assumes"
        );
        assert!(
            ttf_ms <= our_ms + 1e-9,
            "{name}: the first-party face ({our_ms:.4} ms) parsed FASTER than \
             the bare `Face::parse` ({ttf_ms:.4} ms) it is built on — the \
             measurement is not measuring what it says"
        );
        assert!(
            adopted_bytes * 4 < bytes.len() as i64 || bytes.len() < 4 * 1024 * 1024,
            "{name}: adopting the handle still kept {adopted_bytes} B against a \
             {} B file — that looks like a copy",
            bytes.len()
        );
        rows.push((
            name.clone(),
            adopted_bytes,
            our_bytes,
            their_bytes,
            ttf_bytes,
            our_ms,
            their_ms,
            ttf_ms,
        ));
        let _ = adopted_ms;
    }
    // The optimization level is printed WITH the numbers, because this row's
    // headline moved by an order of magnitude between profiles: fontdue's
    // eager outline conversion is allocation-bound and barely optimizes, while
    // the cmap walk that replaced it optimizes very well. A millisecond figure
    // from this table quoted without its profile is a wrong number.
    eprintln!(
        "\nPARSE COST — live heap and wall time, same bytes, same process, \
         debug-assertions {}. THE OPTIMIZATION LEVEL IS NOT VISIBLE FROM HERE \
         and it moves the millisecond columns by ~10x: re-run under \
         `CARGO_PROFILE_TEST_OPT_LEVEL=2` and compare before quoting one.",
        if cfg!(debug_assertions) { "ON" } else { "off" }
    );
    eprintln!(
        "{:<46} {:>9} {:>9} {:>11} {:>10} {:>9} {:>9} {:>8}",
        "FACE", "adopt kB", "copy kB", "fontdue kB", "ttf-p ms", "ours ms", "fd ms", "fd/ttf-p"
    );
    for (name, ab, ob, tb, _ttfb, om, tm, ttfm) in rows {
        eprintln!(
            "{name:<46} {:>9} {:>9} {:>11} {ttfm:>10.4} {om:>9.3} {tm:>9.3} {:>8.0}x",
            ab / 1024,
            ob / 1024,
            tb / 1024,
            tm / ttfm.max(1e-9)
        );
    }
}
