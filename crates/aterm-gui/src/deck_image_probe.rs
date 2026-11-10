// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What `image panel` pays for one frame (M-IMG, `docs/DESIGN-stats-deck-instruments-2026-11-06.md`
//! §12.3 and §16), timed by `benches/deck_image_cost.rs`: the deck laid out from one Snapshot and
//! painted by its own painter ([`crate::stats_deck::deck_paint_kept`], with nothing kept, so its
//! words and header line are read afresh as a call with no window's cache would), rastered whole
//! into an offscreen RGBA buffer ([`crate::tray_raster::rasterize_tray`], the raster the window's
//! deck card takes in `App::splice_stats_deck`), and then encoded as a capture is
//! ([`crate::app_introspect::encode_srgb_rgba8`]). No window, no GPU, no screen capture. A
//! measurement for a person, never a gate: nothing asserts a time.
//!
//! The Snapshot is the A4 desk's, recorded on 2026-11-06
//! (`docs/measured/stats-deck-a4-recordings-2026-11-06/now/`): its strip rings
//! (`now-vitals-strip.json`, read from schema 1 into today's rings) and peaks
//! (`now-vitals-peaks.json`) restored as a launch restores them, then three readings of the
//! `sys --json` overview the desk printed nearest the strip's last write (`now-sys.jsonl`:
//! per-cluster busy, memory classes from `now-sys-mem.txt`, available memory, swap, the volume,
//! network rates, load and processes) absorbed one second apart, so the latest reading sits on the
//! recorded hour.
//!
//! [`DeckImageRig::call`] times a whole call as the app would serve it: the frame on the calling
//! thread (the bench sets it to the main thread's QoS, [`as_main_thread`]), the raster handed to
//! an [`EncodeLane`] spawned as the app spawns `aterm-png-encode`, and the PNG waited for.

use std::path::Path;
use std::time::Instant;

use aterm_json::Value;
use aterm_vitals::model::Snapshot;
use aterm_vitals::{
    ClusterSpec, DiskFacts, DiskLevel, GpuSample, LinkKind, LinkSample, MemSample, Pressure,
    Sample, TaskRoom, Thermal, Throttle, Ticks, Vitals, VolumeSample,
};

use crate::stats_deck::{DeckGeom, DeckInput, DeckStill, StatsBars, deck_paint_kept, deck_px};
use crate::stats_live::DeckLive;
use crate::stats_plan::DeckForm;

/// The process's heap, as the bench's counting allocator keeps it: bytes live now, the most live
/// since the last reset, and the reset (to bytes live now).
#[derive(Clone, Copy)]
pub struct HeapGauge {
    /// Bytes live now.
    pub live: fn() -> usize,
    /// The most bytes live since the last [`Self::reset_peak`].
    pub peak: fn() -> usize,
    /// Start a new peak at the bytes live now.
    pub reset_peak: fn(),
}

/// One frame to render: the deck's form, its width in points, the backing scale, the theme.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DeckImageCase {
    /// `folded` or `full`.
    pub form: &'static str,
    /// The frame's width, pt.
    pub width_pt: f64,
    /// Device pixels per point.
    pub scale: f64,
    /// `dark` (the built-in `Default`) or `light` (`Default Light`).
    pub theme: &'static str,
}

impl DeckImageCase {
    /// Every case M-IMG names: Folded and Full at 1039 and 1440 pt, 1× and 2×, both themes.
    #[must_use]
    pub fn all() -> Vec<Self> {
        let mut out = Vec::new();
        for form in ["folded", "full"] {
            for width_pt in [1039.0, 1440.0] {
                for scale in [1.0, 2.0] {
                    for theme in ["dark", "light"] {
                        out.push(Self {
                            form,
                            width_pt,
                            scale,
                            theme,
                        });
                    }
                }
            }
        }
        out
    }

    fn deck_form(self) -> DeckForm {
        if self.form == "full" {
            DeckForm::Full
        } else {
            DeckForm::Folded
        }
    }

    /// The card's geometry: the frame's width, the deck's height for its form (the owner's 16 pt
    /// terminal cell rounds the Full deck's), a 12 pt side pad and 13 pt type, as the window's.
    fn geom(self) -> DeckGeom {
        let s = self.scale;
        let cell_px = (16.0 * s).round() as usize;
        DeckGeom {
            w: (self.width_pt * s) as f32,
            h: deck_px(StatsBars::Deck, self.deck_form(), s, cell_px) as f32,
            pad: (12.0 * s) as f32,
            scale: s as f32,
            font_px: (13.0 * s) as f32,
        }
    }
}

/// One frame's cost.
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameCost {
    /// Layout, paint and raster, wall clock, ns.
    pub wall_ns: u64,
    /// The layout and paint alone (words, header, prims), ns.
    pub layout_ns: u64,
    /// The raster alone, ns.
    pub raster_ns: u64,
    /// This thread's CPU over the frame, ns.
    pub thread_cpu_ns: u64,
    /// The process's CPU over the frame, every thread's, ns.
    pub process_cpu_ns: u64,
    /// The heap's high water over the frame, above what was live at its start, bytes.
    pub heap_peak_bytes: usize,
    /// The raster's size, px.
    pub px: (u32, u32),
    /// How many prims the painter drew.
    pub prims: usize,
}

/// One PNG encode's cost.
#[derive(Clone, Copy, Debug, Default)]
pub struct PngCost {
    /// Wall clock, ns.
    pub wall_ns: u64,
    /// This thread's CPU, ns.
    pub thread_cpu_ns: u64,
    /// The process's CPU, every thread's, ns.
    pub process_cpu_ns: u64,
    /// The heap's high water over the encode, above what was live at its start, bytes.
    pub heap_peak_bytes: usize,
    /// The encoded file's size, bytes.
    pub png_bytes: usize,
}

/// One `image panel` call's cost, as the app would serve it: the frame on the calling thread, its
/// raster handed to the encode lane and the PNG waited for.
#[derive(Clone, Copy, Debug, Default)]
pub struct CallCost {
    /// From the frame's start to the PNG back on the calling thread, wall clock, ns.
    pub wall_ns: u64,
    /// The frame's part (layout, paint, raster), wall clock, ns.
    pub frame_ns: u64,
    /// The calling thread's CPU over the frame, ns.
    pub frame_cpu_ns: u64,
    /// The lane thread's CPU over the encode, ns.
    pub encode_cpu_ns: u64,
    /// The process's CPU over the whole call, every thread's, ns.
    pub process_cpu_ns: u64,
    /// The heap's high water over the call, above what was live at its start, bytes.
    pub heap_peak_bytes: usize,
    /// The encoded file's size, bytes.
    pub png_bytes: usize,
}

/// A raster to encode, and its size.
type EncodeJob = (Vec<u8>, u32, u32);
/// The PNG and the lane's CPU over its encode, ns.
type EncodeDone = Result<(Vec<u8>, u64), String>;

/// A thread spawned as the app spawns its `aterm-png-encode` lane
/// ([`crate::qos::spawn_lane_thread`] with [`crate::qos::Role::Background`], fed through a sync
/// channel), encoding each raster it is sent with the capture encoder and sending the PNG back.
pub struct EncodeLane {
    jobs: Option<std::sync::mpsc::SyncSender<EncodeJob>>,
    done: std::sync::mpsc::Receiver<EncodeDone>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl EncodeLane {
    /// The lane, its thread started.
    ///
    /// # Errors
    /// When the thread cannot be spawned.
    pub fn start() -> Result<Self, String> {
        let (jobs, rx) = std::sync::mpsc::sync_channel::<EncodeJob>(1);
        let (tx, done) = std::sync::mpsc::channel::<EncodeDone>();
        let thread = crate::qos::spawn_lane_thread(
            "aterm-png-encode".to_string(),
            crate::qos::Role::Background,
            move || {
                while let Ok((rgba, w, h)) = rx.recv() {
                    let c0 = thread_cpu_ns();
                    let png = crate::app_introspect::encode_srgb_rgba8(w, h, |writer| {
                        writer.write_image_data(&rgba)
                    });
                    let cpu = thread_cpu_ns().saturating_sub(c0);
                    drop(rgba);
                    if tx.send(png.map(|p| (p, cpu))).is_err() {
                        break;
                    }
                }
            },
        )
        .map_err(|e| format!("encode lane: {e}"))?;
        Ok(Self {
            jobs: Some(jobs),
            done,
            thread: Some(thread),
        })
    }

    /// `rgba` (`w` × `h`) encoded on the lane, waited for: the PNG and the lane's CPU over it.
    fn encode(&self, rgba: Vec<u8>, w: u32, h: u32) -> EncodeDone {
        let gone = || "encode lane gone".to_string();
        self.jobs
            .as_ref()
            .ok_or_else(gone)?
            .send((rgba, w, h))
            .map_err(|_| gone())?;
        self.done.recv().map_err(|_| gone())?
    }
}

impl Drop for EncodeLane {
    fn drop(&mut self) {
        self.jobs = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Run the calling thread at the app's main-thread QoS ([`crate::qos::Role::Interactive`], as
/// the app's run loop sets it), where `image panel`'s frame would be painted.
pub fn as_main_thread() {
    crate::qos::set_self(crate::qos::Role::Interactive);
}

/// The recorded Snapshot and what it was built from.
pub struct DeckImageRig {
    snap: Snapshot,
    /// What the Snapshot holds, in words, for the results file.
    pub about: String,
}

/// A CPU clock's reading, ns; 0 where unread.
fn cpu_clock_ns(clock: libc::clockid_t) -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid, writable `timespec` that outlives the call.
    if unsafe { libc::clock_gettime(clock, &raw mut ts) } != 0 {
        return 0;
    }
    u64::try_from(ts.tv_sec).unwrap_or(0) * 1_000_000_000 + u64::try_from(ts.tv_nsec).unwrap_or(0)
}

/// The calling thread's CPU time, ns; 0 where unread.
fn thread_cpu_ns() -> u64 {
    cpu_clock_ns(libc::CLOCK_THREAD_CPUTIME_ID)
}

/// The process's CPU time, every thread's, ns; 0 where unread.
fn process_cpu_ns() -> u64 {
    cpu_clock_ns(libc::CLOCK_PROCESS_CPUTIME_ID)
}

/// This process's physical footprint now and its lifetime high water, bytes (macOS
/// `ri_phys_footprint` and `ri_lifetime_max_phys_footprint`, through aterm-sysprobe's read of its
/// process group); `(0, 0)` where unread.
#[must_use]
pub fn footprint_bytes() -> (u64, u64) {
    let pid = std::process::id();
    // SAFETY: `getpgrp` takes no arguments and always succeeds.
    let pgid = unsafe { libc::getpgrp() };
    u32::try_from(pgid)
        .ok()
        .and_then(aterm_sysprobe::group_usage)
        .and_then(|members| members.into_iter().find(|m| m.pid == pid))
        .map_or((0, 0), |m| (m.footprint, m.peak_footprint))
}

fn ns(d: std::time::Duration) -> u64 {
    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
}

const GIB: f64 = 1_073_741_824.0;

/// The recorded memory classes at 10:57 (`now/now-sys-mem.txt`), GiB: app, wired, compressed,
/// cache, free; swap used of total, MiB; the kernel's `memorystatus_level`.
const MEM_GIB: [f64; 5] = [4.86, 3.38, 4.07, 4.80, 14.08];
const SWAP_MIB: (f64, f64) = (181.0, 1024.0);
const MEMORYSTATUS_LEVEL_PCT: u8 = 74;
/// The desk's process limits (§6.1: 720 of 8000).
const PROCS_MAX: u64 = 8000;

impl DeckImageRig {
    /// The rig on the A4 recordings in `dir` (`docs/measured/stats-deck-a4-recordings-2026-11-06`),
    /// its UI faces installed as a window's backend installs them.
    ///
    /// # Errors
    /// When a recorded file is missing or does not parse.
    pub fn recorded(dir: &Path) -> Result<Self, String> {
        let read = |name: &str| {
            std::fs::read_to_string(dir.join(name)).map_err(|e| format!("{name}: {e}"))
        };
        let strip = schema1::strip_of(&read("now/now-vitals-strip.json")?)
            .ok_or("now/now-vitals-strip.json does not parse as a schema-1 strip")?;
        let peaks = crate::vitals_host::peaks_of(&read("now/now-vitals-peaks.json")?)
            .ok_or("now/now-vitals-peaks.json does not parse as peaks")?;
        let last = strip.last_wall_ms;
        let overview = read("now/now-sys.jsonl")?
            .lines()
            .filter_map(|l| aterm_json::from_str::<Value>(l.trim()).ok())
            .rfind(|v| {
                v.get("kind").and_then(Value::as_str) == Some("sys.overview")
                    && v.pointer("/as_of/unix_ms")
                        .and_then(Value::as_i64)
                        .is_some_and(|t| t <= last)
            })
            .ok_or("now/now-sys.jsonl holds no overview at or before the strip's last write")?;
        let at = overview
            .pointer("/as_of/unix_ms")
            .and_then(Value::as_i64)
            .unwrap_or(last);
        let mut vitals = Vitals::new();
        vitals.restore_strip_history(strip, last);
        vitals.restore_peaks(peaks);
        let mut snap = None;
        for k in 1..=3_u64 {
            snap = Some(vitals.absorb(&reading(&overview, last, k)?));
        }
        let snap = snap.ok_or("no reading absorbed")?;
        crate::tray_raster::set_chrome_fonts(None, None, None);
        let g = |p: &str| {
            overview
                .pointer(p)
                .and_then(Value::as_f64)
                .unwrap_or(f64::NAN)
        };
        let about = format!(
            "A4 desk strip restored at its last write {last} (unix ms), peaks restored, then three \
             readings 1 s apart of the overview at {at}: cpu_mc {} (Super {} / Efficiency {}), \
             load1 {:.2}, mem used {:.2} GiB, swap {:.0} MiB, procs {}, net rx {} B/s tx {} B/s",
            g("/global/cpu_mc"),
            g("/global/cpu_by_level_mc/0"),
            g("/global/cpu_by_level_mc/1"),
            g("/global/load_m/0") / 1000.0,
            g("/global/mem_used_bytes") / GIB,
            SWAP_MIB.0,
            g("/global/procs"),
            g("/global/net_rx_bytes_per_s"),
            g("/global/net_tx_bytes_per_s"),
        );
        Ok(Self { snap, about })
    }

    /// The inks a window on `theme`'s band solves, and that band's ground as an opaque backdrop.
    fn inks(theme: &str) -> (aterm_vitals::ink::VitalsInks, [u8; 4]) {
        let name = if theme == "light" {
            "Default Light"
        } else {
            "Default"
        };
        let scheme = aterm_types::scheme::builtin(name).unwrap_or_default();
        // The five hues the band borrows, as a window on this scheme passes them.
        let rgb = |c: aterm_types::Rgb| [c.r, c.g, c.b];
        let ansi = aterm_messages::ink::AnsiHues {
            red: rgb(scheme.ansi[1]),
            green: rgb(scheme.ansi[2]),
            yellow: rgb(scheme.ansi[3]),
            blue: rgb(scheme.ansi[4]),
            cyan: rgb(scheme.ansi[6]),
        };
        let band = crate::chrome_band::band_colors(crate::chrome_band::ChromePalette {
            theme: aterm_render::Theme::from(&scheme),
            ansi: Some(ansi),
            authored: aterm_messages::tokens::AuthoredChrome::NONE,
        });
        let inks = crate::stats_deck::cached_inks(band.bar_bg, band.value, band.accent);
        let g = inks.ground;
        (inks, [g[0], g[1], g[2], 0xFF])
    }

    fn input(&self, case: DeckImageCase, inks: aterm_vitals::ink::VitalsInks) -> DeckInput<'_> {
        DeckInput {
            network: None,
            network_place: None,
            network_target: crate::network_deck::Target::Overview,
            lab: None,
            fleet: None,
            snap: Some(&self.snap),
            prev_snap: None,
            tab_panes: Vec::new(),
            tab_label: "Tab 1".into(),
            tab_count: 1,
            inks,
            dwc: false,
            marks: Vec::new(),
            plate: None,
            form: case.deck_form(),
            contrast: false,
            locale: crate::stats_deck::host_locale(Some(&self.snap)),
            live: DeckLive::STILL,
        }
    }

    /// One frame of `case`: layout, paint and raster, timed, its heap read through `heap`.
    #[must_use]
    pub fn frame(&self, case: DeckImageCase, heap: HeapGauge) -> FrameCost {
        let (inks, backdrop) = Self::inks(case.theme);
        let g = case.geom();
        let base = (heap.live)();
        (heap.reset_peak)();
        let (t0, c0, p0) = (Instant::now(), thread_cpu_ns(), process_cpu_ns());
        let input = self.input(case, inks);
        let (prims, hits, out) = deck_paint_kept(&g, &input, &mut DeckStill::default());
        let t1 = Instant::now();
        let (rgba, pw, ph) =
            crate::tray_raster::rasterize_tray(&prims, g.w as u32, g.h as u32, 1.0, backdrop);
        let n = prims.len();
        drop((prims, hits, out, input));
        let t2 = Instant::now();
        let (c1, p1) = (thread_cpu_ns(), process_cpu_ns());
        let peak = (heap.peak)().saturating_sub(base);
        std::hint::black_box(&rgba);
        drop(rgba);
        FrameCost {
            wall_ns: ns(t2 - t0),
            layout_ns: ns(t1 - t0),
            raster_ns: ns(t2 - t1),
            thread_cpu_ns: c1.saturating_sub(c0),
            process_cpu_ns: p1.saturating_sub(p0),
            heap_peak_bytes: peak,
            px: (pw, ph),
            prims: n,
        }
    }

    /// `case`'s raster, untimed, for [`Self::png`].
    #[must_use]
    pub fn raster(&self, case: DeckImageCase) -> (Vec<u8>, u32, u32) {
        let (inks, backdrop) = Self::inks(case.theme);
        let g = case.geom();
        let input = self.input(case, inks);
        let (prims, ..) = deck_paint_kept(&g, &input, &mut DeckStill::default());
        crate::tray_raster::rasterize_tray(&prims, g.w as u32, g.h as u32, 1.0, backdrop)
    }

    /// One `image panel` call of `case`, timed whole: the frame on this thread (layout, paint,
    /// raster), then its raster handed to `lane` and the PNG waited for, as the reply waits for
    /// it. The control socket, the file write and the reply are not in it. The PNG with it.
    ///
    /// # Errors
    /// When the encoder refuses the raster or the lane has gone.
    pub fn call(
        &self,
        case: DeckImageCase,
        lane: &EncodeLane,
        heap: HeapGauge,
    ) -> Result<(CallCost, Vec<u8>), String> {
        let (inks, backdrop) = Self::inks(case.theme);
        let g = case.geom();
        let base = (heap.live)();
        (heap.reset_peak)();
        let (t0, c0, p0) = (Instant::now(), thread_cpu_ns(), process_cpu_ns());
        let input = self.input(case, inks);
        let (prims, hits, out) = deck_paint_kept(&g, &input, &mut DeckStill::default());
        let (rgba, pw, ph) =
            crate::tray_raster::rasterize_tray(&prims, g.w as u32, g.h as u32, 1.0, backdrop);
        drop((prims, hits, out, input));
        let (t1, c1) = (Instant::now(), thread_cpu_ns());
        let (png, encode_cpu_ns) = lane.encode(rgba, pw, ph)?;
        let (t2, p2) = (Instant::now(), process_cpu_ns());
        let peak = (heap.peak)().saturating_sub(base);
        Ok((
            CallCost {
                wall_ns: ns(t2 - t0),
                frame_ns: ns(t1 - t0),
                frame_cpu_ns: c1.saturating_sub(c0),
                encode_cpu_ns,
                process_cpu_ns: p2.saturating_sub(p0),
                heap_peak_bytes: peak,
                png_bytes: png.len(),
            },
            png,
        ))
    }

    /// One encode of `rgba` (`w` × `h`) as the capture encoder writes it, timed; the PNG with it.
    ///
    /// # Errors
    /// When the encoder refuses the raster.
    pub fn png(rgba: &[u8], w: u32, h: u32, heap: HeapGauge) -> Result<(PngCost, Vec<u8>), String> {
        let base = (heap.live)();
        (heap.reset_peak)();
        let (t0, c0, p0) = (Instant::now(), thread_cpu_ns(), process_cpu_ns());
        let png =
            crate::app_introspect::encode_srgb_rgba8(w, h, |writer| writer.write_image_data(rgba))?;
        let t1 = Instant::now();
        let (c1, p1) = (thread_cpu_ns(), process_cpu_ns());
        let peak = (heap.peak)().saturating_sub(base);
        Ok((
            PngCost {
                wall_ns: ns(t1 - t0),
                thread_cpu_ns: c1.saturating_sub(c0),
                process_cpu_ns: p1.saturating_sub(p0),
                heap_peak_bytes: peak,
                png_bytes: png.len(),
            },
            png,
        ))
    }
}

/// The `k`th reading (1, 2, 3) of the recorded `overview`, `k` seconds after `last` (unix ms): every
/// counter grown at its recorded rate since a zero at `last`, so each reading's rates are the
/// recording's.
fn reading(overview: &Value, last: i64, k: u64) -> Result<Sample, String> {
    let n = |p: &str| {
        overview
            .pointer(p)
            .and_then(Value::as_f64)
            .ok_or_else(|| format!("the overview has no {p}"))
    };
    let super_mc = n("/global/cpu_by_level_mc/0")?;
    let eff_mc = n("/global/cpu_by_level_mc/1")?;
    // The desk's layout (`now/now-sys-cores.txt`): Efficiency on cpus 0–5, Super on 6–9.
    let clusters = vec![
        ClusterSpec {
            name: "Super".into(),
            logical: vec![6, 7, 8, 9],
        },
        ClusterSpec {
            name: "Efficiency".into(),
            logical: (0..6).collect(),
        },
    ];
    let ms = k * 1000;
    let cores = (0..10_u16)
        .map(|cpu| {
            let pm = if cpu >= 6 {
                super_mc / 4.0
            } else {
                eff_mc / 6.0
            };
            Ticks {
                busy: (pm * k as f64) as u64,
                total: ms,
            }
        })
        .collect();
    let gib = |v: f64| (v * GIB) as u64;
    let total = n("/global/mem_total_bytes")? as u64;
    // The recording's build named the volume's f_bavail `disk_available_bytes` and its f_bfree
    // `disk_free_bytes`; free is f_bavail, and reserved what f_bfree holds beyond it.
    let writable = n("/global/disk_available_bytes")? as u64;
    let free_blocks = n("/global/disk_free_bytes")? as u64;
    let mem = MemSample {
        total,
        app: gib(MEM_GIB[0]),
        wired: gib(MEM_GIB[1]),
        compressed: Some(gib(MEM_GIB[2])),
        cached: gib(MEM_GIB[3]),
        free: gib(MEM_GIB[4]),
        swap_used: (SWAP_MIB.0 * 1_048_576.0) as u64,
        swap_total: (SWAP_MIB.1 * 1_048_576.0) as u64,
        available: n("/global/mem_available_bytes")? as u64,
        pressure: Pressure::Normal,
        memorystatus_level_pct: Some(MEMORYSTATUS_LEVEL_PCT),
        ..MemSample::default()
    };
    let secs = k as f64;
    let link = LinkSample {
        name: "en0".into(),
        kind: LinkKind::Ethernet,
        up: true,
        counts: true,
        rx: (n("/global/net_rx_bytes_per_s")? * secs) as u64,
        tx: (n("/global/net_tx_bytes_per_s")? * secs) as u64,
        rate_bps: Some(1_000_000_000),
        ..LinkSample::default()
    };
    let load = [
        n("/global/load_m/0")? / 1000.0,
        n("/global/load_m/1")? / 1000.0,
        n("/global/load_m/2")? / 1000.0,
    ];
    let at_unix_ms = last + i64::try_from(ms).unwrap_or(0);
    let mono_ms = 1_000_000 + ms;
    Ok(Sample {
        at_ns: mono_ms * 1_000_000,
        cores: Some(cores),
        clusters,
        mem: Some(mem),
        gpu: Some(GpuSample {
            device_pct: Some(u8::try_from(n("/global/gpu_pm")? as u64 / 10).unwrap_or(0)),
            cores: Some(10),
            name: Some("Apple M5".into()),
            unified: true,
            ..GpuSample::default()
        }),
        volume: Some(VolumeSample {
            total: n("/global/disk_total_bytes")? as u64,
            free: writable,
            reserved: free_blocks.saturating_sub(writable),
            mount: "/".into(),
        }),
        self_pid: std::process::id(),
        at_unix_ms,
        mono_ms,
        cadence_ms: 1000,
        machine_only: true,
        load1: Some(load[0]),
        load: Some(load),
        links: Some(vec![link]),
        room: TaskRoom {
            procs: Some(n("/global/procs")? as u64),
            procs_max: Some(PROCS_MAX),
            ..TaskRoom::default()
        },
        disk: Some(DiskFacts {
            level: Some(DiskLevel::Ok),
            watch: true,
            ..DiskFacts::default()
        }),
        thermal: Some(Thermal {
            level: Throttle::Light,
            raw: 1,
            low_power: Some(false),
            exact: true,
        }),
        cpu_brand: Some("Apple M5".into()),
        ..Sample::default()
    })
}

/// The A4 recording's strip (`now-vitals-strip.json`), written at schema 1, the whole-file
/// strip that F4′'s append-only checkpoint retired and that a launch now restores nothing from
/// (`vitals_strip.rs`'s `LEGACY_FILE`). The probe reads it as the deck that recorded it held
/// it, in today's ring shape: a 10 s bucket `(k, sum, n, max, over_n)` becomes an [`Agg`] whose
/// `n` readings each stood for one second (schema 1's one-second slot) up to its bucket's
/// length, its over time `over_n` such seconds, its mean `sum / n` and its least reading that
/// mean, which schema 1 never kept and no strip cell draws; a reading `(at, v, over)` weighs one
/// second. Schema 1 fed each ten-minute bucket every reading, so every 10 s bucket it holds is
/// folded already. Its Processes rings held the process count, which `proc_count` now holds
/// beside them; the count of this user's processes it kept (`yours`) F4′ retired.
mod schema1 {
    use aterm_vitals::{
        Agg, GapKind, Resource, SavedBucket, SavedReading, SavedRing, SavedStrip, StripEvent,
        StripEventKind, TimeGap,
    };

    /// A bucket `(k, sum, n, max, over_n)`.
    type BucketDoc = (i64, f64, u16, f64, u16);
    const SLOT_MS: u32 = 1_000;
    const BUCKET_MS: u32 = 10_000;
    const LONG_BUCKET_MS: u32 = 600_000;

    #[derive(Default, serde::Deserialize)]
    struct RingDoc {
        #[serde(default)]
        buckets: Vec<BucketDoc>,
        #[serde(default)]
        recent: Vec<(i64, f64, bool)>,
        #[serde(default)]
        long: Vec<BucketDoc>,
    }

    #[derive(serde::Deserialize)]
    struct StripDoc {
        schema: u32,
        last_wall_ms: i64,
        clean_stop: bool,
        caps: Vec<Option<f64>>,
        rings: Vec<RingDoc>,
        #[serde(default)]
        net_tx: RingDoc,
        #[serde(default)]
        pressure: RingDoc,
        #[serde(default)]
        aterm: Vec<RingDoc>,
        #[serde(default)]
        gaps: Vec<(i64, i64, String)>,
        #[serde(default)]
        events: Vec<(i64, String, String)>,
    }

    fn agg_of((k, sum, n, max, over_n): BucketDoc, len_ms: u32) -> Option<SavedBucket> {
        let mean = sum / f64::from(n);
        let tau_ms = (u32::from(n) * SLOT_MS).min(len_ms);
        let agg = Agg {
            tau_ms,
            n,
            over_ms: (u32::from(over_n.min(n)) * SLOT_MS).min(tau_ms),
            mean,
            min: mean.min(max),
            max,
        };
        agg.is_sound().then_some((k, agg))
    }

    fn ring_of(doc: RingDoc) -> SavedRing {
        let buckets: Vec<SavedBucket> = doc
            .buckets
            .into_iter()
            .filter_map(|b| agg_of(b, BUCKET_MS))
            .collect();
        let folded_below = buckets
            .last()
            .map_or(SavedRing::default().folded_below, |&(k, _)| {
                k.saturating_add(1)
            });
        SavedRing {
            buckets,
            recent: doc
                .recent
                .into_iter()
                .map(|(at, v, over)| -> SavedReading { (at, v, over, SLOT_MS) })
                .collect(),
            long: doc
                .long
                .into_iter()
                .filter_map(|b| agg_of(b, LONG_BUCKET_MS))
                .collect(),
            folded_below,
        }
    }

    fn rings_of(docs: Vec<RingDoc>) -> [SavedRing; 7] {
        let mut rings: [SavedRing; 7] = std::array::from_fn(|_| SavedRing::default());
        for (dst, src) in rings.iter_mut().zip(docs) {
            *dst = ring_of(src);
        }
        rings
    }

    fn gap_kind_of(word: &str) -> Option<GapKind> {
        match word {
            "paused" => Some(GapKind::Paused),
            "sleep" => Some(GapKind::Sleep),
            "crash" => Some(GapKind::Crash),
            _ => None,
        }
    }

    fn event_kind_of(word: &str) -> Option<StripEventKind> {
        match word {
            "sleep_wake" => Some(StripEventKind::SleepWake),
            "pressure_spike" => Some(StripEventKind::PressureSpike),
            "restart" => Some(StripEventKind::Restart),
            "crash" => Some(StripEventKind::Crash),
            _ => None,
        }
    }

    /// The strip in `text`; `None` unless it parses at schema 1 with a reading.
    pub(super) fn strip_of(text: &str) -> Option<SavedStrip> {
        let doc: StripDoc = aterm_json::from_str(text).ok()?;
        if doc.schema != 1 || doc.last_wall_ms <= 0 {
            return None;
        }
        let mut caps = [None; 7];
        for (dst, src) in caps.iter_mut().zip(doc.caps) {
            *dst = src.filter(|x| x.is_finite() && *x > 0.0);
        }
        let rings = rings_of(doc.rings);
        let procs = Resource::ALL
            .iter()
            .position(|&r| r == Resource::Processes)?;
        Some(SavedStrip {
            last_wall_ms: doc.last_wall_ms,
            clean_stop: doc.clean_stop,
            caps,
            proc_count: rings[procs].clone(),
            rings,
            net_tx: ring_of(doc.net_tx),
            pressure: ring_of(doc.pressure),
            aterm: rings_of(doc.aterm),
            proc_starts: Some(false),
            time_gaps: doc
                .gaps
                .into_iter()
                .filter_map(|(start_ms, end_ms, kind)| {
                    (end_ms > start_ms).then_some(TimeGap {
                        start_ms,
                        end_ms,
                        kind: gap_kind_of(&kind)?,
                    })
                })
                .collect(),
            strip_events: doc
                .events
                .into_iter()
                .filter_map(|(at_unix_ms, kind, label)| {
                    Some(StripEvent {
                        at_unix_ms,
                        kind: event_kind_of(&kind)?,
                        label,
                    })
                })
                .collect(),
            ..SavedStrip::default()
        })
    }
}
