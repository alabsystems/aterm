// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Headless paint probe for staging scenes, driving keys, and capturing frames ([`Capture`]).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::Rig;
use super::instance::{
    Budget, CouldNotRun, Ctl, Instance, Scratch, World, could_not_run, field, uint_field,
};
use super::paint::{
    BUDGET_SECS, Capture, Companion, Expect, Ledger, PaintReport, PaintRow, Scene, Split, Style,
    judge,
};
use super::scan::{Cell, RecordingIndex, StatusRow, TakeContext, scan_dir};
use super::starvation::{DEFAULT_LATE_THRESHOLD_US, Sampler, SchedInputs, classify, paint_verdict};

const SAMPLER_TICK: Duration = Duration::from_millis(50);
const VIDEO_KEY_GAP: Duration = Duration::from_millis(120);
const SCREEN_TAIL_ROWS: &str = "tail=12";
const BURST_GAP: Duration = Duration::from_millis(40);
const TAIL_GAP: Duration = Duration::from_millis(50);
const TAIL_SHOTS: usize = 8;
const COLD_SHOTS: usize = 24;
const COLD_GAP: Duration = Duration::from_millis(100);
const CAT_SLOT: Duration = Duration::from_millis(250);
const CAT_TAIL: Duration = Duration::from_secs(1);
const PLANE_DECAY: Duration = Duration::from_secs(1);
const COMPANION_STILLS: usize = 3;
/// The companion plane's caret row and column, zero-based (`ESC [10;10H`).
const PLANE_CARET: &str = "9";

const COMPANION_PLANE: &str = r"printf '\033[2J\033[10;10H'; sleep 30";

/// The take a process makes once, before its first measured row, and throws away: a fresh
/// bundle's first recording and first trail frames start cold (the driven window opens later and
/// a frame hole can pass twice the nominal, which voids the take), a first-use cost like
/// [`super::instance::first_exec`]'s that no measured take should carry.
const WARM_ROW: PaintRow = PaintRow {
    scene: Scene::Prompt,
    keys: "r,a,i,n,b,o,w,k,i,t,t,y",
    expect: Expect::Ink,
    capture: Capture::PinnedVideo,
    style: Style::Flying,
    companion: None,
    split: None,
};

static WARMED: std::sync::Once = std::sync::Once::new();

/// Measure one row.
/// # Errors
/// [`CouldNotRun`] when the take decided nothing.
pub fn measure(rig: &Rig, row: &PaintRow) -> Result<PaintReport, CouldNotRun> {
    let _one = super::ONE_TAKE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    row.validate()?;
    let bench_band = super::instance::Band::take_own();
    super::instance::first_exec(&rig.fake_tui);
    WARMED.call_once(|| warm(rig));
    let started = Instant::now();
    let budget = Budget::from_now(BUDGET_SECS);
    let mut sampler = Sampler::start(SAMPLER_TICK);
    let mut scratch = Scratch::new("atpaint")
        .map_err(|e| CouldNotRun::Refused(format!("cannot make a scratch world: {e}")))?;
    if std::env::var_os("ATERM_PAINT_KEEP").is_some() {
        scratch.keep();
    }
    let take = run_take(rig, row, scratch.path(), &sampler, budget);
    let sched = sampler.finish();
    let take = take.map_err(|e| {
        e.map(|why| {
            format!(
                "{why} [sched_ticks={} sched_late_max_us={} wall_ms={}]",
                sched.ticks,
                sched.late_max_us,
                started.elapsed().as_millis()
            )
        })
    })?;
    let wall_us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
    let (ticks, late_max_us) = sched.deciding();
    let starvation = classify(&SchedInputs {
        ticks,
        late_max_us,
        sampling_hole_us: u64::try_from(take.scan.driven.sampling_hole_us).unwrap_or(0),
        nominal_dt_us: u64::try_from(take.scan.driven.nominal_dt_us).unwrap_or(0),
        wall_us,
        budget_us: BUDGET_SECS * 1_000_000,
        late_threshold_us: DEFAULT_LATE_THRESHOLD_US,
        produced_take: true,
        qos: sched.qos,
    });
    let disposition = paint_verdict(take.shape, starvation.evidence());
    Ok(PaintReport {
        row: *row,
        backend: take.backend,
        scan: take.scan,
        ledger: take.ledger,
        shape: take.shape,
        sched,
        starvation,
        disposition,
        wall_us,
        alt_wait_ms: take.alt_wait_ms,
        typed_gap_ms: take.typed_gap_ms,
        driven_ms: take.driven_ms,
        band: take.band,
        bench_band,
    })
}

/// [`WARM_ROW`]'s take, whatever it finds: it decides nothing, so its result is only said.
fn warm(rig: &Rig) {
    super::instance::first_exec(&rig.aterm);
    let outcome = Scratch::new("atpaint").map_or_else(
        |e| format!("no scratch world: {e}"),
        |scratch| {
            let mut sampler = Sampler::start(SAMPLER_TICK);
            let take = run_take(
                rig,
                &WARM_ROW,
                scratch.path(),
                &sampler,
                Budget::from_now(BUDGET_SECS),
            );
            let _ = sampler.finish();
            take.map_or_else(
                |why| format!("it ended early: {why}"),
                |_| "taken".to_owned(),
            )
        },
    );
    eprintln!("measure warm-up: paint — a take that decides nothing ({outcome})");
}

struct Take {
    backend: String,
    scan: super::scan::ScanReport,
    ledger: Ledger,
    shape: super::starvation::ShapeVerdict,
    alt_wait_ms: u64,
    typed_gap_ms: Option<(u64, u64)>,
    driven_ms: Option<(i64, i64)>,
    band: super::instance::Band,
}

fn config_head(row: &PaintRow) -> String {
    let mut head = row
        .style
        .config_line()
        .unwrap_or("# cursor trail omitted: the shipped default")
        .to_string();
    if row.scene == Scene::ColdStreamer {
        head.push_str("\n[sparkle_words]\nenabled = false");
    }
    head
}

fn run_take(
    rig: &Rig,
    row: &PaintRow,
    root: &Path,
    sampler: &Sampler,
    budget: Budget,
) -> Result<Take, CouldNotRun> {
    let world = World::prepare(root, &config_head(row))
        .map_err(|e| CouldNotRun::Refused(format!("cannot lay out the scratch world: {e}")))?;
    let inst = Instance::launch(&rig.aterm, &world, budget)?;
    let ctl = &inst.ctl;
    if let Some(split) = row.split {
        stage_split(ctl, split)?;
    }
    let alt_wait_ms = stage_scene(rig, &inst, row.scene)?;
    if row.companion.is_some() {
        stage_companion_plane(ctl)?;
    }
    let cell = cell_of(ctl)?;
    if row.scene == Scene::ColdStreamer {
        fence_walking_caret(ctl)?;
    }
    if row.capture == Capture::UnpinnedUnfocused {
        fence_focus_out(ctl)?;
    }
    let capture = match row.capture {
        Capture::PinnedVideo => capture_video(ctl, row, sampler)?,
        Capture::UnpinnedFocused | Capture::UnpinnedUnfocused => capture_burst(ctl, row, sampler)?,
    };
    let metrics = ctl.ok(&["metrics"])?;
    let backend = field(&metrics, "backend")
        .ok_or_else(|| {
            CouldNotRun::Unanswered(format!("ctl metrics reported no backend ({metrics})"))
        })?
        .to_string();
    let fx_under_frames = uint_field(&metrics, "fx_under_frames")
        .ok_or_else(|| CouldNotRun::Unanswered("ctl metrics reported no fx_under_frames".into()))?;
    if backend != "gpu" {
        return could_not_run(format!(
            "these frames were drawn by the {backend} renderer, not the GPU the shipped artifact \
             renders on; log tail:\n{}",
            inst.log_tail()
        ));
    }
    let ctx = TakeContext {
        cell,
        statuses: capture.rows.clone(),
        index: capture.index,
        driven_ms: capture.driven_ms,
    };
    let scan = scan_dir(&capture.frames_dir, &ctx)
        .map_err(|e| CouldNotRun::Refused(format!("the take could not be scanned: {e}")))?;
    let ledger = Ledger {
        rows: capture.rows,
        final_row: capture.final_row,
        fx_under_frames,
    };
    let shape = judge(row, &ledger, &scan)?;
    let typed_gap_ms = typed_gaps(&capture.frames_dir, &capture.typed_shots);
    Ok(Take {
        backend,
        scan,
        ledger,
        shape,
        alt_wait_ms,
        typed_gap_ms,
        driven_ms: capture.driven_ms,
        band: inst.band.clone(),
    })
}

fn stage_split(ctl: &Ctl, split: Split) -> Result<(), CouldNotRun> {
    let arg = match split {
        Split::V => "split=v",
        Split::H => "split=h",
    };
    ctl.ok(&["spawn", arg])?;
    ctl.await_event("the window becoming a two-pane split", || {
        let reply = ctl.call(&["panes"]);
        (field(reply.line(), "panes") == Some("2")).then_some(())
    })?;
    ctl.ok(&["ready"])?;
    Ok(())
}

fn scene_missed(inst: &Instance, why: CouldNotRun) -> CouldNotRun {
    let screen = match inst.ctl.call(&["text", SCREEN_TAIL_ROWS]) {
        reply if reply.ok() => reply.stdout,
        reply => format!("(unreadable: {})", reply.stderr.trim()),
    };
    why.map(|why| {
        format!(
            "{why}\n--- the screen's last rows ---\n{}\n--- the instance log's tail ---\n{}",
            screen.trim_end(),
            inst.log_tail()
        )
    })
}

fn stage_scene(rig: &Rig, inst: &Instance, scene: Scene) -> Result<u64, CouldNotRun> {
    let ctl = &inst.ctl;
    let Some(shape) = scene.tui() else {
        ctl.ok(&["await", "match", r"^\$", "timeout=60000"])
            .map_err(|e| scene_missed(inst, e))?;
        let modes = ctl.ok(&["modes"])?;
        if field(&modes, "alt_screen") != Some("false") {
            return could_not_run(format!(
                "the prompt scene is not on the main screen ({modes})"
            ));
        }
        return Ok(0);
    };
    let launch = format!("'{}' --shape {shape}", rig.fake_tui.display());
    ctl.ok(&["send", &launch])?;
    ctl.ok(&["key", "enter"])?;
    let asked = Instant::now();
    ctl.await_event(
        &format!("the {shape} scene reaching the alt screen"),
        || {
            let reply = ctl.call(&["modes"]);
            (field(reply.line(), "alt_screen") == Some("true")).then_some(())
        },
    )
    .map_err(|e| scene_missed(inst, e))?;
    let waited = u64::try_from(asked.elapsed().as_millis()).unwrap_or(u64::MAX);
    let persistent = match shape {
        "claude" => Some("fake.claude"),
        "streamer" => Some("streaming.tokens"),
        _ => None,
    };
    if let Some(pattern) = persistent {
        ctl.ok(&["await", "match", pattern, "timeout=60000"])
            .map_err(|e| scene_missed(inst, e))?;
    }
    Ok(waited)
}

fn stage_companion_plane(ctl: &Ctl) -> Result<(), CouldNotRun> {
    ctl.ok(&["send", COMPANION_PLANE])?;
    ctl.ok(&["key", "enter"])?;
    ctl.await_event("the companion plane", || {
        let reply = ctl.call(&["cursor"]);
        let mut it = reply.line().split_whitespace();
        (it.next() == Some("OK")
            && it.next() == Some(PLANE_CARET)
            && it.next() == Some(PLANE_CARET))
        .then_some(())
    })?;
    std::thread::sleep(PLANE_DECAY);
    Ok(())
}

fn cell_of(ctl: &Ctl) -> Result<Cell, CouldNotRun> {
    let dims = ctl.ok(&["dims"])?;
    let w = uint_field(&dims, "cell_w")
        .and_then(|v| usize::try_from(v).ok())
        .unwrap_or(0);
    let h = uint_field(&dims, "cell_h")
        .and_then(|v| usize::try_from(v).ok())
        .unwrap_or(0);
    Cell::new(w, h)
        .ok_or_else(|| CouldNotRun::Refused(format!("ctl dims reported no positive cell ({dims})")))
}

fn caret(ctl: &Ctl) -> Result<(String, String, String), CouldNotRun> {
    let line = ctl.ok(&["cursor"])?;
    let parts: Vec<&str> = line.split_whitespace().collect();
    match parts[..] {
        ["OK", row, col, visible, ..] => Ok((row.into(), col.into(), visible.into())),
        _ => could_not_run(format!("ctl cursor did not answer ({line})")),
    }
}

fn fence_walking_caret(ctl: &Ctl) -> Result<(), CouldNotRun> {
    let first = caret(ctl)?;
    let moved = ctl.await_event("the cold streamer's caret advancing", || {
        caret(ctl)
            .ok()
            .filter(|c| (&c.0, &c.1) != (&first.0, &first.1))
    })?;
    if first.2 != "1" || moved.2 != "1" {
        return could_not_run(format!(
            "the cold streamer's caret is hidden (visible {} / {}): a hidden caret is trivially dark",
            first.2, moved.2
        ));
    }
    Ok(())
}

fn fence_focus_out(ctl: &Ctl) -> Result<(), CouldNotRun> {
    ctl.ok(&["focus", "out"])?;
    let rain = ctl.ok(&["rain", "status"])?;
    if field(&rain, "focused") != Some("false") {
        return could_not_run(format!(
            "ctl focus out did not take: the window still reports focus ({rain})"
        ));
    }
    Ok(())
}

struct Captured {
    frames_dir: PathBuf,
    rows: Option<Vec<StatusRow>>,
    final_row: Option<StatusRow>,
    index: Option<RecordingIndex>,
    driven_ms: Option<(i64, i64)>,
    /// Each typed key's first shot, in order.
    typed_shots: Vec<usize>,
}

fn pace_until(deadline: Instant) {
    let now = Instant::now();
    if deadline > now {
        std::thread::sleep(deadline - now);
    }
}

fn elapsed_ms(reply: &str) -> Option<i64> {
    field(reply, "elapsed_ms").and_then(|v| v.parse().ok())
}

fn capture_video(ctl: &Ctl, row: &PaintRow, sampler: &Sampler) -> Result<Captured, CouldNotRun> {
    sampler.open_window();
    let secs = row.record_secs().to_string();
    let recorder = {
        let ctl = ctl.clone();
        std::thread::spawn(move || ctl.call(&["video", &secs, "full", "pace"]))
    };
    let opened = ctl.await_event("the recording's first frame", || {
        if recorder.is_finished() {
            return Some(None);
        }
        let line = ctl.call(&["video", "status"]).line().to_string();
        (field(&line, "recording") == Some("true")
            && uint_field(&line, "frames").is_some_and(|n| n >= 1))
        .then_some(Some(line))
    });
    let mut driven_ms = None;
    if let Ok(Some(opened)) = &opened
        && !row.keys.is_empty()
    {
        let lo = elapsed_ms(opened);
        for (i, key) in row.keys().iter().enumerate() {
            if i > 0 {
                std::thread::sleep(VIDEO_KEY_GAP);
            }
            let _ = ctl.call(&["key", key]);
        }
        let hi = elapsed_ms(ctl.call(&["video", "status"]).line());
        driven_ms = lo.zip(hi);
    }
    let reply = recorder
        .join()
        .map_err(|_| CouldNotRun::Refused("the recording thread panicked".into()))?;
    sampler.close_window();
    if opened?.is_none() && !row.keys.is_empty() {
        return could_not_run(format!(
            "ctl video ended before its first frame was seen, so no key was typed inside it ({} {})",
            reply.line(),
            reply.stderr.trim()
        ));
    }
    let line = reply.line().to_string();
    let index_path = line
        .split_once(" /")
        .filter(|_| line.starts_with("OK frames="))
        .map(|(_, rest)| PathBuf::from(format!("/{rest}")))
        .ok_or_else(|| {
            CouldNotRun::Refused(format!(
                "ctl video reported no take ({line} {})",
                reply.stderr.trim()
            ))
        })?;
    let index = RecordingIndex::load(&index_path).map_err(CouldNotRun::Refused)?;
    let frames_dir = index_path
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| CouldNotRun::Refused("the recording index has no directory".into()))?;
    let final_row = Some(ctl.call(&["trail", "status"]).line().to_string())
        .filter(|l| l.starts_with("trail "))
        .map(StatusRow::new);
    Ok(Captured {
        frames_dir,
        rows: None,
        final_row,
        index: Some(index),
        driven_ms,
        typed_shots: Vec::new(),
    })
}

struct Burst<'a> {
    ctl: &'a Ctl,
    shots: usize,
    dir: Option<PathBuf>,
    rows: Vec<StatusRow>,
}

impl Burst<'_> {
    fn shot(&mut self) -> Result<(), CouldNotRun> {
        let name = format!("shot_{:04}.png", self.shots);
        let reply = self.ctl.ok(&["image", &name])?;
        let mut parts = reply.splitn(4, ' ');
        let path = match (parts.next(), parts.next(), parts.next(), parts.next()) {
            (Some("OK"), Some(_), Some(_), Some(path)) => PathBuf::from(path),
            _ => return could_not_run(format!("ctl image answered no frame ({reply})")),
        };
        if self.dir.is_none() {
            self.dir = path.parent().map(Path::to_path_buf);
        }
        let status = self.ctl.ok(&["trail", "status"])?;
        if !status.starts_with("trail ") {
            return could_not_run(format!("ctl trail status answered no row ({status})"));
        }
        self.rows.push(StatusRow::new(status));
        self.shots += 1;
        Ok(())
    }
}

fn capture_burst(ctl: &Ctl, row: &PaintRow, sampler: &Sampler) -> Result<Captured, CouldNotRun> {
    sampler.open_window();
    let mut burst = Burst {
        ctl,
        shots: 0,
        dir: None,
        rows: Vec::new(),
    };
    burst.shot()?;
    let keys = row.keys();
    let mut typed_shots = Vec::with_capacity(keys.len());
    if keys.is_empty() {
        for _ in 0..COLD_SHOTS {
            burst.shot()?;
            std::thread::sleep(COLD_GAP);
        }
    } else if row.companion == Some(Companion::Cat) {
        let mut tail_end = Instant::now();
        for key in &keys {
            let slot = Instant::now();
            tail_end = slot + CAT_TAIL;
            let _ = ctl.call(&["key", key]);
            typed_shots.push(burst.shots);
            burst.shot()?;
            if slot.elapsed() < CAT_SLOT / 2 {
                std::thread::sleep(BURST_GAP);
                burst.shot()?;
            }
            pace_until(slot + CAT_SLOT);
        }
        for _ in 0..TAIL_SHOTS {
            burst.shot()?;
            std::thread::sleep(TAIL_GAP);
            if Instant::now() >= tail_end {
                break;
            }
        }
    } else {
        for key in &keys {
            let _ = ctl.call(&["key", key]);
            typed_shots.push(burst.shots);
            burst.shot()?;
            std::thread::sleep(BURST_GAP);
            burst.shot()?;
            std::thread::sleep(BURST_GAP);
        }
        for _ in 0..TAIL_SHOTS {
            burst.shot()?;
            std::thread::sleep(TAIL_GAP);
        }
    }
    if row.companion.is_some() && !keys.is_empty() {
        ctl.ok(&["key", "ctrl+c"])?;
        ctl.ok(&["await", "match", r"^\$.?$", "timeout=60000"])?;
        stage_companion_plane(ctl)?;
        for _ in 0..COMPANION_STILLS {
            burst.shot()?;
            std::thread::sleep(TAIL_GAP);
        }
    }
    sampler.close_window();
    let frames_dir = burst
        .dir
        .ok_or_else(|| CouldNotRun::Refused("the image burst produced no frames".into()))?;
    Ok(Captured {
        frames_dir,
        rows: Some(burst.rows),
        final_row: None,
        index: None,
        driven_ms: None,
        typed_shots,
    })
}

fn typed_gaps(dir: &Path, typed: &[usize]) -> Option<(u64, u64)> {
    let stamps: Vec<std::time::SystemTime> = typed
        .iter()
        .map(|i| std::fs::metadata(dir.join(format!("shot_{i:04}.png"))).and_then(|m| m.modified()))
        .collect::<Result<_, _>>()
        .ok()?;
    let mut gaps: Vec<u64> = stamps
        .windows(2)
        .filter_map(|w| w[1].duration_since(w[0]).ok())
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .collect();
    gaps.sort_unstable();
    let worst = *gaps.last()?;
    Some((gaps[gaps.len().div_ceil(2) - 1], worst))
}
