// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! M-IMG (`docs/DESIGN-stats-deck-instruments-2026-11-06.md` §16): what `image panel` pays for one
//! frame, the deck from the A4 desk's recorded Snapshot laid out, painted and rastered offscreen
//! ([`aterm_gui::deck_image_probe`]), at Folded and Full, 1039 and 1440 pt, 1× and 2×, both themes,
//! and then that raster's PNG encode. Each frame's wall clock, this thread's CPU, the process's CPU
//! and the heap's high water over it (this binary's counting allocator) are written per frame. The
//! cases run round-robin, one frame of each per round, so a shared desk's load falls on every case
//! alike; `aterm ctl @self sys --json` is read before the first round, every 25 rounds and after the
//! last, and each row names the reading it ran under. A measurement for a person, never a gate.
//!
//! ```text
//! targo --unverified build -p aterm-gui --profile opt-test --features bench-support --bench deck_image_cost
//! <build-dir>/opt-test/build/aterm-gui/<hash>/out/deck_image_cost-<hash> <recordings-dir> <out-dir> [frames] [encodes] [run] [calls]
//! ```
//!
//! `<recordings-dir>` is `docs/measured/stats-deck-a4-recordings-2026-11-06`; `<out-dir>` gets
//! `frames-<run>.csv`, `png-<run>.csv`, `summary-<run>.jsonl` and one PNG a case. With `calls`
//! above 0, the bench then times that many whole calls a case, round-robin
//! ([`aterm_gui::deck_image_probe::DeckImageRig::call`]: the frame on this thread at the app's
//! main-thread QoS, the PNG on an encode lane spawned as the app's is), into `calls-<run>.csv`.
//! The binary keeps its Cargo name (`deck_image_cost-<16 hex>`), so it runs on a test binary's
//! scratch root.

#[cfg(unix)]
mod heap {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

    /// The system allocator, counting the bytes it holds and their high water.
    pub(crate) struct Counting;

    static LIVE: AtomicUsize = AtomicUsize::new(0);
    static PEAK: AtomicUsize = AtomicUsize::new(0);

    fn grew(by: usize) {
        let now = LIVE.fetch_add(by, Relaxed).saturating_add(by);
        PEAK.fetch_max(now, Relaxed);
    }

    fn shrank(by: usize) {
        LIVE.fetch_sub(by, Relaxed);
    }

    // SAFETY: every call forwards to `System` with the caller's arguments unchanged and returns
    // what it returned; the counters only add up sizes.
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            // SAFETY: the caller's contract for `alloc` is passed through to `System`.
            let p = unsafe { System.alloc(layout) };
            if !p.is_null() {
                grew(layout.size());
            }
            p
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            // SAFETY: the caller's contract for `alloc_zeroed` is passed through to `System`.
            let p = unsafe { System.alloc_zeroed(layout) };
            if !p.is_null() {
                grew(layout.size());
            }
            p
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            // SAFETY: `ptr` came from this allocator, which is `System`, with `layout`.
            unsafe { System.dealloc(ptr, layout) };
            shrank(layout.size());
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            // SAFETY: the caller's contract for `realloc` is passed through to `System`.
            let q = unsafe { System.realloc(ptr, layout, new_size) };
            if !q.is_null() {
                if new_size >= layout.size() {
                    grew(new_size - layout.size());
                } else {
                    shrank(layout.size() - new_size);
                }
            }
            q
        }
    }

    pub(crate) fn live() -> usize {
        LIVE.load(Relaxed)
    }

    pub(crate) fn peak() -> usize {
        PEAK.load(Relaxed)
    }

    pub(crate) fn reset_peak() {
        PEAK.store(LIVE.load(Relaxed), Relaxed);
    }
}

#[cfg(unix)]
#[global_allocator]
static HEAP: heap::Counting = heap::Counting;

#[cfg(unix)]
fn main() -> std::process::ExitCode {
    match run::main() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("deck_image_cost: {why}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(not(unix))]
fn main() {
    eprintln!("deck_image_cost: macOS and Linux only (it reads thread CPU with clock_gettime)");
}

#[cfg(unix)]
mod run {
    use std::fmt::Write as _;
    use std::io::Write as _;
    use std::path::{Path, PathBuf};

    use aterm_gui::deck_image_probe::{
        CallCost, DeckImageCase, DeckImageRig, EncodeLane, FrameCost, HeapGauge, PngCost,
        as_main_thread, footprint_bytes,
    };
    use aterm_json::Value;

    /// Untimed frames of every case before the timed rounds.
    const WARM: usize = 5;
    /// Rounds between two `sys` readings.
    const SYS_EVERY: usize = 25;

    const GAUGE: HeapGauge = HeapGauge {
        live: super::heap::live,
        peak: super::heap::peak,
        reset_peak: super::heap::reset_peak,
    };

    fn label(c: &DeckImageCase) -> String {
        format!("{}/{}/{}x/{}", c.form, c.width_pt, c.scale, c.theme)
    }

    /// One `aterm ctl @self sys --json` reading: `(load1, busy cores, thermal word)`, as JSON.
    fn sys_reading(ix: usize, at_round: usize) -> String {
        let t_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let out = aterm_uds::spawnfd::command("aterm")
            .args(["ctl", "@self", "sys", "--json"])
            .output();
        let doc = out.ok().and_then(|o| {
            let text = String::from_utf8_lossy(&o.stdout).into_owned();
            text.lines()
                .find(|l| l.trim_start().starts_with('{'))
                .and_then(|l| aterm_json::from_str::<Value>(l).ok())
        });
        let f = |p: &str| {
            doc.as_ref()
                .and_then(|d| d.pointer(p))
                .and_then(Value::as_f64)
        };
        let num =
            |v: Option<f64>, div: f64| v.map_or("null".to_string(), |v| format!("{:.3}", v / div));
        let thermal = doc
            .as_ref()
            .and_then(|d| d.pointer("/global/thermal"))
            .and_then(Value::as_str)
            .map_or("null".to_string(), |s| format!("\"{s}\""));
        format!(
            "{{\"kind\":\"sys\",\"ix\":{ix},\"before_round\":{at_round},\"unix_ms\":{t_ms},\"load1\":{},\"load5\":{},\"busy_cores\":{},\"cores\":{},\"thermal\":{thermal},\"mem_pressure\":{}}}",
            num(f("/global/load_m/0"), 1000.0),
            num(f("/global/load_m/1"), 1000.0),
            num(f("/global/cpu_mc"), 1000.0),
            num(f("/global/cpu_cores"), 1.0),
            doc.as_ref()
                .and_then(|d| d.pointer("/global/mem_pressure"))
                .and_then(Value::as_str)
                .map_or("null".to_string(), |s| format!("\"{s}\"")),
        )
    }

    fn pct(v: &mut [f64], q: f64) -> f64 {
        v.sort_by(f64::total_cmp);
        v.get(((v.len().max(1) - 1) as f64 * q).round() as usize)
            .copied()
            .unwrap_or(f64::NAN)
    }

    fn us(ns: u64) -> f64 {
        ns as f64 / 1000.0
    }

    /// `"name":{"p50":…,"p95":…,"max":…}` of `values`.
    fn stat(name: &str, mut values: Vec<f64>) -> String {
        let max = values.iter().copied().fold(f64::NAN, f64::max);
        format!(
            "\"{name}\":{{\"p50\":{:.1},\"p95\":{:.1},\"max\":{max:.1}}}",
            pct(&mut values, 0.5),
            pct(&mut values, 0.95)
        )
    }

    pub(super) fn main() -> Result<(), String> {
        let args: Vec<String> = std::env::args()
            .skip(1)
            .filter(|a| !a.starts_with("--"))
            .collect();
        let (Some(rec), Some(out)) = (args.first(), args.get(1)) else {
            return Err(
                "usage: deck_image_cost <recordings-dir> <out-dir> [frames] [encodes] [run] [calls]"
                    .into(),
            );
        };
        let frames: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(150);
        let encodes: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(100);
        let run = args.get(4).map_or("1", String::as_str);
        let calls: usize = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(0);
        let out = PathBuf::from(out);
        std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
        let footprint_start = footprint_bytes();
        let rig = DeckImageRig::recorded(Path::new(rec))?;
        let cases = DeckImageCase::all();
        let mut summary = String::new();
        let _ = writeln!(
            summary,
            "{{\"kind\":\"run\",\"run\":\"{run}\",\"frames\":{frames},\"encodes\":{encodes},\"calls\":{calls},\"warm\":{WARM},\"pid\":{},\"snapshot\":{:?},\"footprint_before_rig\":{},\"footprint_peak_before_rig\":{},\"footprint_after_rig\":{},\"footprint_peak_after_rig\":{}}}",
            std::process::id(),
            rig.about,
            footprint_start.0,
            footprint_start.1,
            footprint_bytes().0,
            footprint_bytes().1
        );
        let mut sys_ix = 0;
        let sys = |ix: usize, round: usize, summary: &mut String| {
            let line = sys_reading(ix, round);
            eprintln!("  {line}");
            let _ = writeln!(summary, "{line}");
        };
        sys(sys_ix, 0, &mut summary);

        // The process's first frame: cold glyph caches, as an `image panel` that comes first.
        let cold = rig.frame(cases[0], GAUGE);
        let _ = writeln!(
            summary,
            "{{\"kind\":\"cold\",\"case\":\"{}\",\"wall_us\":{:.1},\"layout_us\":{:.1},\"raster_us\":{:.1},\"thread_cpu_us\":{:.1},\"heap_peak_bytes\":{}}}",
            label(&cases[0]),
            us(cold.wall_ns),
            us(cold.layout_ns),
            us(cold.raster_ns),
            us(cold.thread_cpu_ns),
            cold.heap_peak_bytes
        );
        for c in &cases {
            for _ in 0..WARM {
                std::hint::black_box(rig.frame(*c, GAUGE));
            }
        }

        let mut rows = String::from(
            "case,i,sys,wall_us,layout_us,raster_us,thread_cpu_us,process_cpu_us,heap_peak_kib\n",
        );
        let mut costs: Vec<Vec<FrameCost>> = vec![Vec::with_capacity(frames); cases.len()];
        for round in 0..frames {
            if round > 0 && round % SYS_EVERY == 0 {
                sys_ix += 1;
                sys(sys_ix, round, &mut summary);
            }
            for (k, c) in cases.iter().enumerate() {
                let f = rig.frame(*c, GAUGE);
                let _ = writeln!(
                    rows,
                    "{},{round},{sys_ix},{:.1},{:.1},{:.1},{:.1},{:.1},{:.1}",
                    label(c),
                    us(f.wall_ns),
                    us(f.layout_ns),
                    us(f.raster_ns),
                    us(f.thread_cpu_ns),
                    us(f.process_cpu_ns),
                    f.heap_peak_bytes as f64 / 1024.0
                );
                costs[k].push(f);
            }
        }
        sys_ix += 1;
        sys(sys_ix, frames, &mut summary);
        let footprint_frames = footprint_bytes();

        // The encodes: one raster a case, encoded `encodes` times, round-robin as the frames.
        let rasters: Vec<(Vec<u8>, u32, u32)> = cases.iter().map(|c| rig.raster(*c)).collect();
        let mut png_rows = String::from(
            "case,i,sys,wall_us,thread_cpu_us,process_cpu_us,heap_peak_kib,png_bytes\n",
        );
        let mut pngs: Vec<Vec<PngCost>> = vec![Vec::with_capacity(encodes); cases.len()];
        for (k, (rgba, w, h)) in rasters.iter().enumerate() {
            let (_, png) = DeckImageRig::png(rgba, *w, *h, GAUGE)?;
            let name = label(&cases[k]).replace('/', "-");
            std::fs::write(out.join(format!("{name}.png")), png)
                .map_err(|e| format!("{name}.png: {e}"))?;
        }
        for round in 0..encodes {
            if round > 0 && round % SYS_EVERY == 0 {
                sys_ix += 1;
                sys(sys_ix, frames + round, &mut summary);
            }
            for (k, (rgba, w, h)) in rasters.iter().enumerate() {
                let (p, png) = DeckImageRig::png(rgba, *w, *h, GAUGE)?;
                drop(png);
                let _ = writeln!(
                    png_rows,
                    "{},{round},{sys_ix},{:.1},{:.1},{:.1},{:.1},{}",
                    label(&cases[k]),
                    us(p.wall_ns),
                    us(p.thread_cpu_ns),
                    us(p.process_cpu_ns),
                    p.heap_peak_bytes as f64 / 1024.0,
                    p.png_bytes
                );
                pngs[k].push(p);
            }
        }
        sys_ix += 1;
        sys(sys_ix, frames + encodes, &mut summary);

        // The calls: each one frame on this thread, at the main thread's QoS, and its PNG on a
        // lane spawned as the app's `aterm-png-encode` is, timed whole, round-robin as the frames.
        let mut call_rows = String::from(
            "case,i,sys,wall_us,frame_us,frame_cpu_us,encode_cpu_us,process_cpu_us,heap_peak_kib,png_bytes\n",
        );
        let mut call_costs: Vec<Vec<CallCost>> = vec![Vec::with_capacity(calls); cases.len()];
        if calls > 0 {
            as_main_thread();
            let lane = EncodeLane::start()?;
            for c in &cases {
                for _ in 0..WARM {
                    std::hint::black_box(rig.call(*c, &lane, GAUGE)?);
                }
            }
            let start_round = frames + encodes;
            for round in 0..calls {
                if round > 0 && round % SYS_EVERY == 0 {
                    sys_ix += 1;
                    sys(sys_ix, start_round + round, &mut summary);
                }
                for (k, c) in cases.iter().enumerate() {
                    let (x, png) = rig.call(*c, &lane, GAUGE)?;
                    drop(png);
                    let _ = writeln!(
                        call_rows,
                        "{},{round},{sys_ix},{:.1},{:.1},{:.1},{:.1},{:.1},{:.1},{}",
                        label(c),
                        us(x.wall_ns),
                        us(x.frame_ns),
                        us(x.frame_cpu_ns),
                        us(x.encode_cpu_ns),
                        us(x.process_cpu_ns),
                        x.heap_peak_bytes as f64 / 1024.0,
                        x.png_bytes
                    );
                    call_costs[k].push(x);
                }
            }
            drop(lane);
            sys_ix += 1;
            sys(sys_ix, start_round + calls, &mut summary);
        }

        for (k, c) in cases.iter().enumerate() {
            let x = &call_costs[k];
            if x.is_empty() {
                continue;
            }
            let col = |g: fn(&CallCost) -> u64| x.iter().map(|v| us(g(v))).collect::<Vec<_>>();
            let line = format!(
                "{{\"kind\":\"call\",\"case\":\"{}\",\"n\":{},{},{},{},{},{},{},\"png_bytes\":{}}}",
                label(c),
                x.len(),
                stat("wall_us", col(|v| v.wall_ns)),
                stat("frame_us", col(|v| v.frame_ns)),
                stat("frame_cpu_us", col(|v| v.frame_cpu_ns)),
                stat("encode_cpu_us", col(|v| v.encode_cpu_ns)),
                stat("process_cpu_us", col(|v| v.process_cpu_ns)),
                stat(
                    "heap_peak_kib",
                    x.iter()
                        .map(|v| v.heap_peak_bytes as f64 / 1024.0)
                        .collect()
                ),
                x.first().map_or(0, |v| v.png_bytes),
            );
            println!("{line}");
            let _ = writeln!(summary, "{line}");
        }

        for (k, c) in cases.iter().enumerate() {
            let f = &costs[k];
            let p = &pngs[k];
            if f.is_empty() || p.is_empty() {
                continue;
            }
            let px = f.first().map_or((0, 0), |f| f.px);
            let col = |g: fn(&FrameCost) -> u64| f.iter().map(|x| us(g(x))).collect::<Vec<_>>();
            let pcol = |g: fn(&PngCost) -> u64| p.iter().map(|x| us(g(x))).collect::<Vec<_>>();
            let line = format!(
                "{{\"kind\":\"case\",\"case\":\"{}\",\"px\":[{},{}],\"prims\":{},\"n\":{},{},{},{},{},{},{},\"n_png\":{},{},{},{},\"png_bytes\":{}}}",
                label(c),
                px.0,
                px.1,
                f.first().map_or(0, |f| f.prims),
                f.len(),
                stat("wall_us", col(|x| x.wall_ns)),
                stat("layout_us", col(|x| x.layout_ns)),
                stat("raster_us", col(|x| x.raster_ns)),
                stat("thread_cpu_us", col(|x| x.thread_cpu_ns)),
                stat("process_cpu_us", col(|x| x.process_cpu_ns)),
                stat(
                    "heap_peak_kib",
                    f.iter()
                        .map(|x| x.heap_peak_bytes as f64 / 1024.0)
                        .collect()
                ),
                p.len(),
                stat("png_wall_us", pcol(|x| x.wall_ns)),
                stat("png_thread_cpu_us", pcol(|x| x.thread_cpu_ns)),
                stat(
                    "png_heap_peak_kib",
                    p.iter()
                        .map(|x| x.heap_peak_bytes as f64 / 1024.0)
                        .collect()
                ),
                p.first().map_or(0, |x| x.png_bytes),
            );
            println!("{line}");
            let _ = writeln!(summary, "{line}");
        }
        let _ = writeln!(
            summary,
            "{{\"kind\":\"footprint\",\"after_frames\":{},\"peak_after_frames\":{},\"end\":{},\"peak_end\":{}}}",
            footprint_frames.0,
            footprint_frames.1,
            footprint_bytes().0,
            footprint_bytes().1
        );
        let write = |name: String, text: &str| -> Result<(), String> {
            let path = out.join(&name);
            std::fs::File::create(&path)
                .and_then(|mut f| f.write_all(text.as_bytes()))
                .map_err(|e| format!("{name}: {e}"))
        };
        write(format!("frames-{run}.csv"), &rows)?;
        write(format!("png-{run}.csv"), &png_rows)?;
        if calls > 0 {
            write(format!("calls-{run}.csv"), &call_rows)?;
        }
        write(format!("summary-{run}.jsonl"), &summary)?;
        Ok(())
    }
}
