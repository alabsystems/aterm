// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The windowed `scroll` arena lane: 100k+ line fill, scrub sweep, and jump-to-top.

use std::io::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use aterm_json::Value;
use aterm_uds::spawnfd::{Stdio, command};

use super::{
    Group, Halt, Ledger, Scratch, arena_dir, await_socket, build_aterm, ctl_out, macos_version,
    preflight, private, without_aterm_env,
};
use crate::perf::trend::utc_date;
use crate::perf::{LaneRun, arena::Args};

const GLYPHS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789+/-=";

/// Line `n` of the fill: its number, then 40 glyphs rotated by `n`.
pub(crate) fn fill_line(n: u64) -> String {
    let body: String = (0..40u64)
        .map(|c| GLYPHS[((n + c) % GLYPHS.len() as u64) as usize] as char)
        .collect();
    format!("{n} {body}\n")
}

fn fill_file(lines: u64) -> Result<PathBuf, String> {
    let path = arena_dir()?.join(format!("scroll_fill_{lines}.txt"));
    if !path.is_file() {
        eprintln!("generating the {lines}-line fill corpus");
        let part = path.with_extension("part");
        let mut f = std::io::BufWriter::new(
            std::fs::File::create(&part).map_err(|e| format!("{}: {e}", part.display()))?,
        );
        for n in 0..lines {
            f.write_all(fill_line(n).as_bytes())
                .map_err(|e| format!("{}: {e}", part.display()))?;
        }
        f.flush().map_err(|e| format!("{}: {e}", part.display()))?;
        std::fs::rename(&part, &path).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(path)
}

/// `scroll`'s bare report, `OK <offset> <max>`: the scrollback depth.
pub(crate) fn depth(report: &str) -> Option<u64> {
    report.split_whitespace().nth(2)?.parse().ok()
}

fn json(text: &str) -> Result<Value, String> {
    aterm_json::from_str(text.trim()).map_err(|e| format!("metrics --json: {e}: {text:?}"))
}

fn field(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_f64)
        .map_or_else(|| "?".into(), |f| format!("{f}"))
}

/// One window's frames as the house quotes latency: `n_present` and its p50/p95/p99 from
/// `metrics percentiles`, then the render time's, so no `max_` stands alone. Present latency is
/// output to present, and a scroll through the scrollback writes no output, so a window with no
/// sample says so rather than quoting its empty percentiles as zeros.
fn presents(pct: &Value) -> String {
    let q = |n: &str, stem: &str| {
        if pct.get(n).and_then(Value::as_f64) == Some(0.0) {
            return format!("{n}=0");
        }
        format!(
            "{n}={} {stem}_p50/p95/p99={}/{}/{}ms",
            field(pct, n),
            field(pct, &format!("{stem}_p50_ms")),
            field(pct, &format!("{stem}_p95_ms")),
            field(pct, &format!("{stem}_p99_ms"))
        )
    };
    format!("{} {}", q("n_present", "present"), q("n_render", "render"))
}

/// How long a window has to draw the frame a scroll asked for: a hang detector, not a budget.
const FRAME_HANG: Duration = Duration::from_secs(60);

/// The window's `metrics percentiles` once a frame has rendered since its last reset, read every
/// 20 ms for up to `hang`. The ctl `scroll` reply comes once the input is applied, before the
/// frame it asks for is drawn, so a read straight after it can miss the jump's one frame.
fn await_rendered(
    hang: Duration,
    mut read: impl FnMut() -> Result<Value, String>,
) -> Result<Value, String> {
    let deadline = Instant::now() + hang;
    loop {
        let pct = read()?;
        if pct
            .get("n_render")
            .and_then(Value::as_f64)
            .is_some_and(|n| n >= 1.0)
        {
            return Ok(pct);
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "the window rendered no frame for the scroll within {}s",
                hang.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

const CONFIG: &str = "scrollback_lines = 200000\nrestore_session = false\nbell_sound = false\n\n\
                      [update]\nenabled = false\n[packages]\nenabled = false\n";

/// The `scroll` lane.
pub(crate) fn run(args: &Args) -> LaneRun {
    super::outcome(run_inner(args))
}

fn run_inner(args: &Args) -> Result<String, Halt> {
    let lines = args.fill_lines.unwrap_or(120_000);
    let steps = args.scrub_steps.unwrap_or(250);
    let (machine, ledgers) = preflight(args.dev_smoke)?;
    let aterm = build_aterm()?;
    let fill = fill_file(lines)?;

    let run = Scratch::new("arena-scroll")?;
    let sockets = Scratch::sockets()?;
    let root = run.path();
    let home = root.join("home");
    let config = root.join("config");
    let runtime = root.join("runtime");
    for d in [&home, &config.join("aterm"), &runtime] {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
        private(d)?;
    }
    std::fs::write(config.join("aterm/aterm.toml"), CONFIG)
        .map_err(|e| format!("could not write the config: {e}"))?;
    let sock = sockets.sock("scroll.sock")?;
    let log = std::fs::File::create(root.join("gui.log")).map_err(|e| format!("log: {e}"))?;
    let log_err = log.try_clone().map_err(|e| format!("log: {e}"))?;
    let mut cmd = command(&aterm.bin);
    without_aterm_env(&mut cmd)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &config)
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("SHELL", "/bin/sh")
        .arg("--window")
        .arg("--control-sock")
        .arg(&sock)
        .args(["--columns", "110", "--lines", "32"])
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(log_err);
    let mut group = Group::spawn(&mut cmd)?;
    await_socket(&mut group, &sock, Duration::from_secs(60))?;
    let c = |verb: &[&str]| ctl_out(&aterm.bin, &sock, verb);
    c(&["await", "idle", "1000", "timeout=60000"])?;
    let backend = json(&c(&["metrics", "--json"])?)?
        .get("backend")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    eprintln!("backend: {backend}");

    c(&["send", &format!("cat {}\\n", fill.display())])?;
    c(&["await", "idle", "2000", "timeout=600000"])?;
    let depth_lines = depth(&c(&["scroll"])?);
    eprintln!(
        "scrollback depth after fill: {} lines",
        depth_lines.map_or_else(|| "unknown".into(), |d| d.to_string())
    );

    c(&["scroll", "bottom"])?;
    c(&["metrics", "reset"])?;
    for _ in 0..steps {
        c(&["scroll", "3"])?;
    }
    for _ in 0..40 {
        c(&["scroll", "up"])?;
    }
    let scrub = json(&c(&["metrics", "--json"])?)?;
    let scrub_pct = json(&c(&["metrics", "percentiles", "--json"])?)?;

    c(&["scroll", "bottom"])?;
    c(&["metrics", "reset"])?;
    let t0 = Instant::now();
    c(&["scroll", "top"])?;
    let jump_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let jump_pct = await_rendered(FRAME_HANG, || {
        json(&c(&["metrics", "percentiles", "--json"])?)
    })?;
    let shot = root.join("top.png");
    let _ = c(&["image", &shot.display().to_string()]);
    group.retire()?;

    let scrub_row = format!(
        "{} max_frame_gap={}ms slow_frames={}",
        presents(&scrub_pct),
        field(&scrub, "max_frame_gap_ms"),
        field(&scrub, "slow_frames")
    );
    let jump_row = format!("round-trip {jump_ms:.1}ms, {}", presents(&jump_pct));
    eprintln!(
        "== ARENA-SCROLL (aterm side) ==\n  scrub sweep : {scrub_row}\n  jump-to-top : {jump_row}"
    );
    let date = utc_date();
    let depth_text = depth_lines.map_or_else(|| "?".into(), |d| d.to_string());
    let entry = ledgers.write(
        "scroll",
        &Ledger {
            date: &date,
            dim: "ARENA-SCROLL (100k+-line scrub + jump-to-top, windowed)",
            machine: &machine.label(),
            macos: &macos_version(),
            aterm: &aterm.provenance(&format!("release, shipping default; backend={backend}")),
            competitors: "ghostty tip — MANUAL: screen-record a matched 100k-line scrub + \
                          jump-to-top, frame-analyse (its scrollback has no metrics verb)",
        },
        &format!(
            "| scrub sweep ({steps}x wheel + 40x page) | aterm {scrub_row} | ghostty: (screen-recording) | depth {depth_text} lines |\n\
             | jump-to-top | aterm {jump_row} | ghostty: (screen-recording) | worst-case oldest cold-tier decode |\n"
        ),
    )?;
    Ok(format!("ledger entry {}", entry.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fill_is_deterministic_and_rotates() {
        assert_eq!(fill_line(0), "0 abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMN\n");
        assert_eq!(fill_line(1), "1 bcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNO\n");
        assert_eq!(fill_line(66), fill_line(0).replacen('0', "66", 1));
    }

    #[test]
    fn the_scroll_window_starts_with_the_audible_bell_off() {
        let doc = aterm_toml::from_str::<aterm_toml::Value>(CONFIG).expect("one document");
        assert_eq!(
            doc.get("bell_sound").and_then(aterm_toml::Value::as_bool),
            Some(false)
        );
        assert_eq!(
            doc.get("scrollback_lines")
                .and_then(aterm_toml::Value::as_integer),
            Some(200_000)
        );
        assert_eq!(
            doc.get("update")
                .and_then(|t| t.get("enabled"))
                .and_then(aterm_toml::Value::as_bool),
            Some(false)
        );
    }

    #[test]
    fn a_window_is_quoted_by_its_present_and_render_percentiles() {
        let pct = json(
            r#"{"n_present":250,"present_p50_ms":4.1,"present_p95_ms":6.25,"present_p99_ms":8,
                "n_render":250,"render_p50_ms":0.5,"render_p95_ms":1,"render_p99_ms":1.44}"#,
        )
        .expect("percentiles");
        assert_eq!(
            presents(&pct),
            "n_present=250 present_p50/p95/p99=4.1/6.25/8ms \
             n_render=250 render_p50/p95/p99=0.5/1/1.44ms"
        );
        let scrolled = json(
            r#"{"n_present":0,"present_p50_ms":0,"present_p95_ms":0,"present_p99_ms":0,
                "n_render":178,"render_p50_ms":0.39,"render_p95_ms":0.46,"render_p99_ms":0.72}"#,
        )
        .expect("a scroll's percentiles");
        assert_eq!(
            presents(&scrolled),
            "n_present=0 n_render=178 render_p50/p95/p99=0.39/0.46/0.72ms"
        );
        assert_eq!(
            presents(&json("{}").expect("empty")),
            "n_present=? present_p50/p95/p99=?/?/?ms n_render=? render_p50/p95/p99=?/?/?ms"
        );
    }

    #[test]
    fn the_jump_is_read_once_its_frame_has_rendered() {
        let mut seen = [0.0, 0.0, 1.0].into_iter();
        let mut reads = 0;
        let pct = await_rendered(FRAME_HANG, || {
            reads += 1;
            json(&format!("{{\"n_render\":{}}}", seen.next().unwrap_or(2.0)))
        })
        .expect("the third read has the frame");
        assert_eq!(
            (reads, pct.get("n_render").and_then(Value::as_f64)),
            (3, Some(1.0))
        );
        let e = await_rendered(Duration::ZERO, || json("{\"n_render\":0}"))
            .expect_err("no frame by the wait");
        assert!(e.contains("no frame") && e.ends_with("within 0s"), "{e}");
        let e = await_rendered(Duration::ZERO, || Err("no socket".to_string()))
            .expect_err("a failed read");
        assert_eq!(e, "no socket");
    }

    #[test]
    fn the_depth_is_the_third_field_of_the_scroll_report() {
        assert_eq!(depth("OK 0 120031"), Some(120_031));
        assert_eq!(depth("ERR nope"), None);
    }
}
