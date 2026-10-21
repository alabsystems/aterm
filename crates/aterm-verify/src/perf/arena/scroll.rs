// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `scroll` lane (ARENA-SCROLL, windowed): fill 100k+ lines, then drive a
//! wheel-scrub and page sweep and a jump-to-top over the control socket and
//! read the frame pacing back (`max_frame_gap_ms`, `slow_frames`,
//! `max_frame_render_ms`, render p99). Frame pacing is observable only in a
//! real window; the headless engine half is the `scroll-scrub` floor lane.
//!
//! The aterm side is scripted here. The ghostty side is owner-manual by rule
//! zero (screen-record a matched scrub and frame-analyse it: its scrollback
//! exposes no metrics verb), so the ledger leaves it a row.

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

/// The lane's scratch `aterm.toml`: retention above the fill, no restored
/// window, the update and package lanes off, and the audible bell off (this
/// window is never the owner's terminal, so a BEL its shell rings must not
/// play their alert sound).
const CONFIG: &str = "scrollback_lines = 200000\nrestore_session = false\nbell_sound = false\n\n\
                      [update]\nenabled = false\n[packages]\nenabled = false\n";

/// The `scroll` lane.
pub(crate) fn run(args: &Args) -> LaneRun {
    super::outcome(run_inner(args))
}

fn run_inner(args: &Args) -> Result<String, Halt> {
    // Deeper than the 100k tiered limit, so the cold (compressed) tier is deep.
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
    // Retention above the fill is configuration, set before launch.
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
    // The shell is up and quiet: an event, not a settle.
    c(&["await", "idle", "1000", "timeout=60000"])?;
    let backend = json(&c(&["metrics", "--json"])?)?
        .get("backend")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    eprintln!("backend: {backend}");

    // A trailing literal backslash-n is the socket's Enter.
    c(&["send", &format!("cat {}\\n", fill.display())])?;
    // The fill is done when the output has been quiet for two seconds.
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
    // Force a full visible-row decode of the oldest cold tier.
    let shot = root.join("top.png");
    let _ = c(&["image", &shot.display().to_string()]);
    let jump = json(&c(&["metrics", "--json"])?)?;
    group.retire()?;

    let scrub_row = format!(
        "max_frame_gap={}ms slow_frames={} max_frame_render={}ms render_p99={}ms",
        field(&scrub, "max_frame_gap_ms"),
        field(&scrub, "slow_frames"),
        field(&scrub, "max_frame_render_ms"),
        field(&scrub_pct, "render_p99_ms")
    );
    let jump_row = format!(
        "round-trip {jump_ms:.1}ms, first frame max_frame_render={}ms",
        field(&jump, "max_frame_render_ms")
    );
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

    /// The lane's window starts with the audible bell off (aterm's default
    /// is on) and its retention above the fill, as one document aterm's
    /// parser reads.
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
    fn the_depth_is_the_third_field_of_the_scroll_report() {
        assert_eq!(depth("OK 0 120031"), Some(120_031));
        assert_eq!(depth("ERR nope"), None);
    }
}
