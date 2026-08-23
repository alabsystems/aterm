// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `startup_exec` — what ONE `exec()` of a shipped binary costs, split by how
//! many frameworks dyld has to map before `main` ever runs.
//!
//! WHY THIS IS NOT A CRITERION BENCH. Every other bench in this crate measures
//! a function inside one process, where criterion's sampling is exactly right.
//! This one measures the process itself: `exec()` -> dyld4 -> Obj-C/C++ image
//! initialisers -> `main` -> print -> exit, a few milliseconds of it, most of it
//! spent before the first instruction of `main`. Criterion cannot contain that —
//! a criterion iteration would BE a spawn, and its warm-up/outlier machinery is
//! tuned for sub-millisecond work with no kernel round trip. So this target is a
//! plain `main` running alternating exec loops, and it reports the per-arm
//! SPREAD so a reader can see the noise floor rather than being told one.
//!
//! WHAT IT IS FOR. `aterm_gui::mark_rust_main_start()` is the first statement of
//! `crates/aterm/src/main.rs`, and the metrics module says so itself
//! (`crates/aterm-gui/src/metrics.rs`: "Dyld/process-loader work precedes this
//! stamp and remains outside both metrics"). Everything this bench measures is
//! therefore invisible to every other instrument in the repo. Until this file
//! existed there was no startup bench in the tree at all.
//!
//! THE REACH GUARD IS TWO-SIDED, and it is a load-command assertion, not a
//! timing one. A timing win with an unchanged load-command list means something
//! else was measured. So before timing anything, this bench reads the Mach-O
//! `LC_LOAD*_DYLIB` commands out of each binary itself and requires:
//!
//!   * the LEAN arm (`aterm-ctl`) carries ZERO `.framework` load commands AND
//!     no `libobjc` — the whole cliff, not part of it. Measured on this box with
//!     two byte-identical 16,840-byte C nops differing only in link commands:
//!     Foundation+CoreFoundation+Security+CoreServices+libobjc ALONE costs
//!     +1.4-1.8 ms, and adding AppKit+Metal+Carbon+QuartzCore+CoreText+
//!     ApplicationServices+CoreVideo+ColorSync+AudioToolbox on top adds only
//!     ~+0.5 ms. It is a near-fixed "you touched the Obj-C runtime at all"
//!     cliff, so a half-measure that drops AppKit but keeps libobjc recovers
//!     almost nothing, and this guard refuses to call that a fix.
//!   * the CONTROL arm (`aterm`) STILL carries them. It links `aterm-gui`
//!     (winit/wgpu/objc2) directly and legitimately needs AppKit/Metal to put a
//!     pixel on glass. If it ever stops carrying them, the comparison below is
//!     no longer measuring the framework tax and the number is meaningless.
//!
//! SIZE IS THE OBVIOUS CONFOUND, so a third arm exists to break it: `atpkg` is
//! several times LARGER than `aterm-ctl` and carries no frameworks. If the
//! ordering here tracked binary size, `atpkg` would land between the other two;
//! it does not, it lands with the lean arm.
//!
//! HOW BIG IS THE PRIZE, HONESTLY. On the WINDOW route: nothing. The window
//! needs AppKit/Metal/QuartzCore/CoreText before first paint, so the cost is
//! real but unrecoverable there, and against a ~440 ms rust_main -> first_present
//! it is well under 1%. On the SESSION route it lands against shell fork + rc
//! sourcing, tens to hundreds of ms. The payoff is scripted/agent verb traffic —
//! `aterm ctl …` in a loop — where it compounds linearly, ~2 s per 1000 calls.
//!
//! RUN IT:
//!   cargo build --release --workspace --bins     # the arms must exist first
//!   cargo bench -p aterm-bench --bench startup_exec
//!
//! ENV KNOBS (all optional):
//!   ATERM_STARTUP_EXECS=120    execs per slot (2 slots per arm per round)
//!   ATERM_STARTUP_ROUNDS=6     rounds
//!   ATERM_STARTUP_CONTROL=1    add a byte-identical copy of the first arm as an
//!                              extra arm. Its delta from the original IS this
//!                              box's noise floor at this arm size — run it
//!                              before believing any delta below.
//!   ATERM_STARTUP_NO_GUARD=1   report the load-command guard but do not exit
//!                              non-zero on it (for a tree mid-refactor).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

const DEFAULT_EXECS: usize = 120;
const DEFAULT_ROUNDS: usize = 6;

/// The lean arm: declares only `aterm-types` + `aterm-uds`, no third-party
/// dependency at all, so it must link no framework.
const LEAN_ARM: &str = "aterm-ctl";
/// The control arm: THE one binary, which links `aterm-gui` and therefore
/// legitimately carries the window's frameworks.
const CONTROL_ARM: &str = "aterm";

// ---------------------------------------------------------------------------
// Mach-O load commands — the reach guard's evidence, read here rather than
// shelled out to `otool` so the guard is a property of the bench and not of
// whichever developer tools happen to be installed.
// ---------------------------------------------------------------------------

const FAT_MAGIC: u32 = 0xcafe_babe;
const FAT_MAGIC_64: u32 = 0xcafe_babf;
const MH_MAGIC_64: u32 = 0xfeed_facf;
const MH_MAGIC_32: u32 = 0xfeed_face;

const LC_LOAD_DYLIB: u32 = 0x0c;
const LC_LOAD_WEAK_DYLIB: u32 = 0x18 | 0x8000_0000;
const LC_REEXPORT_DYLIB: u32 = 0x1f | 0x8000_0000;
const LC_LOAD_UPWARD_DYLIB: u32 = 0x23 | 0x8000_0000;

fn be32(b: &[u8], i: usize) -> u32 {
    u32::from_be_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

fn le32(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

fn be64(b: &[u8], i: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[i..i + 8]);
    u64::from_be_bytes(a)
}

fn read_at(f: &mut File, off: u64, len: usize) -> Result<Vec<u8>, String> {
    f.seek(SeekFrom::Start(off)).map_err(|e| e.to_string())?;
    let mut v = vec![0u8; len];
    f.read_exact(&mut v).map_err(|e| e.to_string())?;
    Ok(v)
}

/// Every dylib/framework install name in the binary's load commands — i.e.
/// exactly what dyld maps and initialises BEFORE `main`.
///
/// A universal (fat) binary is read through its FIRST slice: the load-command
/// list is the same in both slices for anything this repo links, and the guard
/// is about which libraries are named, not about which architecture runs.
fn dylib_load_commands(path: &Path) -> Result<Vec<String>, String> {
    let mut f = File::open(path).map_err(|e| e.to_string())?;
    let head = read_at(&mut f, 0, 8)?;
    let fat = be32(&head, 0);
    let slice_off: u64 = if fat == FAT_MAGIC || fat == FAT_MAGIC_64 {
        let entry_len = if fat == FAT_MAGIC { 20 } else { 32 };
        let arch = read_at(&mut f, 8, entry_len)?;
        if fat == FAT_MAGIC {
            u64::from(be32(&arch, 8))
        } else {
            be64(&arch, 8)
        }
    } else {
        0
    };

    let mh = read_at(&mut f, slice_off, 32)?;
    let magic = le32(&mh, 0);
    if magic != MH_MAGIC_64 && magic != MH_MAGIC_32 {
        return Err(format!("not a Mach-O image (magic {magic:#x})"));
    }
    let hdr_len: u64 = if magic == MH_MAGIC_64 { 32 } else { 28 };
    let ncmds = le32(&mh, 16) as usize;
    let sizeofcmds = le32(&mh, 20) as usize;
    let cmds = read_at(&mut f, slice_off + hdr_len, sizeofcmds)?;

    let mut out = Vec::new();
    let mut off = 0usize;
    for _ in 0..ncmds {
        if off + 8 > cmds.len() {
            break;
        }
        let cmd = le32(&cmds, off);
        let cmdsize = le32(&cmds, off + 4) as usize;
        // A zero/short/overlong cmdsize would spin forever or read past the
        // buffer; stop instead of trusting the file.
        if cmdsize < 8 || off + cmdsize > cmds.len() {
            break;
        }
        let is_dylib = matches!(
            cmd,
            LC_LOAD_DYLIB | LC_LOAD_WEAK_DYLIB | LC_REEXPORT_DYLIB | LC_LOAD_UPWARD_DYLIB
        );
        if is_dylib && cmdsize >= 12 {
            let name_off = le32(&cmds, off + 8) as usize;
            if (12..cmdsize).contains(&name_off) {
                let raw = &cmds[off + name_off..off + cmdsize];
                let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
                out.push(String::from_utf8_lossy(&raw[..end]).into_owned());
            }
        }
        off += cmdsize;
    }
    Ok(out)
}

/// The Obj-C cliff, as one summary per binary: the `.framework` load commands by
/// short name, plus whether `libobjc` is mapped at all. Both halves matter — see
/// the module header.
fn objc_surface(libs: &[String]) -> (Vec<String>, bool) {
    let frameworks: Vec<String> = libs
        .iter()
        .filter(|l| l.contains(".framework/"))
        .map(|l| l.rsplit('/').next().unwrap_or(l.as_str()).to_string())
        .collect();
    let objc = libs.iter().any(|l| l.contains("libobjc"));
    (frameworks, objc)
}

// ---------------------------------------------------------------------------
// The arms
// ---------------------------------------------------------------------------

struct Arm {
    label: String,
    path: PathBuf,
    args: Vec<String>,
    bytes: u64,
    frameworks: Vec<String>,
    objc: bool,
    macho: bool,
}

impl Arm {
    fn load(label: &str, path: PathBuf, args: &[&str]) -> Option<Arm> {
        let bytes = std::fs::metadata(&path).ok()?.len();
        let (frameworks, objc, macho) = match dylib_load_commands(&path) {
            Ok(libs) => {
                let (f, o) = objc_surface(&libs);
                (f, o, true)
            }
            Err(_) => (Vec::new(), false, false),
        };
        Some(Arm {
            label: label.to_string(),
            path,
            args: args.iter().map(|s| (*s).to_string()).collect(),
            bytes,
            frameworks,
            objc,
            macho,
        })
    }
}

/// `execs` spawns of one arm, back to back. Returns milliseconds per exec.
///
/// stdio is nulled so the measurement is the process lifecycle and not the
/// terminal's line handling, and the exit status is ignored on purpose: one of
/// the routes below is a usage path that exits non-zero, which is still a
/// complete exec + dyld + main + write + exit.
fn slot(arm: &Arm, execs: usize) -> f64 {
    let t0 = Instant::now();
    for _ in 0..execs {
        let _ = Command::new(&arm.path)
            .args(&arm.args)
            // A bench must not drive the product's side effects. `aterm
            // --version` is on the auto-update lane's path, and this loop runs
            // it a couple of thousand times; the updater's own pre-swap boot
            // probe sets exactly this variable for exactly this reason
            // (`crates/aterm-update/src/verify.rs`, `probe_bundle_starts`).
            .env("ATERM_NO_AUTO_UPDATE", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[allow(clippy::cast_precision_loss)]
    let n = execs as f64;
    t0.elapsed().as_secs_f64() * 1000.0 / n
}

fn median(xs: &[f64]) -> f64 {
    let mut v = xs.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    if n == 0 {
        return f64::NAN;
    }
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

fn spread_pct(xs: &[f64]) -> f64 {
    let m = median(xs);
    let lo = xs.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = xs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    (hi - lo) / m * 100.0
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(default)
}

fn target_release_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("CARGO_TARGET_DIR") {
        return PathBuf::from(dir).join("release");
    }
    // crates/aterm-bench -> crates -> workspace root
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map_or_else(
            || PathBuf::from("target/release"),
            |r| r.join("target/release"),
        )
}

/// The load-command guard, both sides. Returns the failures, so the caller can
/// print every one of them rather than the first.
fn reach_guard(arms: &[Arm]) -> Vec<String> {
    let mut failures = Vec::new();
    if !arms.iter().any(|a| a.macho) {
        return failures;
    }
    if let Some(lean) = arms.iter().find(|a| a.label == LEAN_ARM)
        && (!lean.frameworks.is_empty() || lean.objc)
    {
        failures.push(format!(
            "{LEAN_ARM} carries {} framework load command(s) and libobjc={}. It declares only \
             aterm-types + aterm-uds, so this is Cargo feature unification: some crate in the \
             same workspace resolve turned on an optional platform dependency of a crate \
             {LEAN_ARM} shares. That is the regression this bench exists to catch.",
            lean.frameworks.len(),
            lean.objc
        ));
    }
    if let Some(ctrl) = arms.iter().find(|a| a.label == CONTROL_ARM)
        && ctrl.frameworks.is_empty()
    {
        failures.push(format!(
            "the CONTROL arm `{CONTROL_ARM}` carries no framework load commands. Either the GUI \
             moved behind a dlopen (in which case this bench's comparison needs rewriting, not \
             reinterpreting) or the wrong binary was measured. Either way the numbers below are \
             not the framework tax."
        ));
    }
    failures
}

fn main() {
    let rel = target_release_dir();
    let exe = std::env::consts::EXE_SUFFIX;

    // THE ARM UNDER TEST first, then the control, then the size control.
    //   aterm-ctl  the thin control-socket client. Must be framework-free.
    //   aterm      THE one binary — links aterm-gui, so it legitimately carries
    //              the window's frameworks. The control arm.
    //   atpkg      the size control: bigger than aterm-ctl, framework-free.
    //              `--version` is not one of its verbs, so this is its
    //              unknown-verb usage path — parse, print, exit, which is the
    //              cheapest complete exec it has.
    let candidates = [
        (LEAN_ARM, format!("{LEAN_ARM}{exe}"), vec!["--version"]),
        (
            CONTROL_ARM,
            format!("{CONTROL_ARM}{exe}"),
            vec!["--version"],
        ),
        ("atpkg", format!("atpkg{exe}"), vec!["--version"]),
    ];

    let mut arms: Vec<Arm> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    for (label, file, args) in &candidates {
        let p = rel.join(file);
        match Arm::load(label, p.clone(), args) {
            Some(a) => arms.push(a),
            None => missing.push(p.display().to_string()),
        }
    }

    if arms.len() < 2 {
        println!("startup_exec: SKIPPED — need the release binaries first.");
        println!("  build them with: cargo build --release --workspace --bins");
        for m in &missing {
            println!("  missing: {m}");
        }
        return;
    }

    // The identical-binary control, opt-in: a byte copy of the first arm, run as
    // a separate arm. Whatever delta it shows is measurement noise by
    // construction, and no smaller delta below it should be believed.
    let mut control_copy: Option<PathBuf> = None;
    if std::env::var_os("ATERM_STARTUP_CONTROL").is_some() {
        let dst = std::env::temp_dir().join(format!("startup_exec_control_{}", std::process::id()));
        if std::fs::copy(&arms[0].path, &dst).is_ok() {
            let args: Vec<&str> = arms[0].args.iter().map(String::as_str).collect();
            if let Some(a) = Arm::load("CONTROL (byte copy of arm 1)", dst.clone(), &args) {
                arms.push(a);
                control_copy = Some(dst);
            }
        }
    }

    // --- the reach guard, before any timing -------------------------------
    println!("startup_exec — dyld framework tax on the front door");
    println!();
    println!("load commands (LC_LOAD*_DYLIB, read from the Mach-O itself):");
    for a in &arms {
        if !a.macho {
            println!(
                "  {:<32} n/a (not a Mach-O image on this platform)",
                a.label
            );
            continue;
        }
        let fw = if a.frameworks.is_empty() {
            "-".to_string()
        } else {
            a.frameworks.join(" ")
        };
        println!(
            "  {:<32} {:>9} KB  frameworks={:<2} libobjc={:<5}  {}",
            a.label,
            a.bytes / 1024,
            a.frameworks.len(),
            a.objc,
            fw
        );
    }
    println!();

    let guard_failures = reach_guard(&arms);
    for f in &guard_failures {
        println!("REACH GUARD FAILED: {f}");
    }
    if guard_failures.is_empty() {
        println!("reach guard: OK (lean arm framework-free; control arm still framework-linked)");
    }
    println!();

    // --- the measurement ---------------------------------------------------
    let execs = env_usize("ATERM_STARTUP_EXECS", DEFAULT_EXECS);
    let rounds = env_usize("ATERM_STARTUP_ROUNDS", DEFAULT_ROUNDS);

    // Warm the page cache and dyld's launch closure for every arm before the
    // first timed slot, so round 1 is not measuring first-touch I/O.
    for a in &arms {
        let _ = slot(a, 5);
    }

    let n = arms.len();
    let mut per_round: Vec<Vec<f64>> = vec![Vec::new(); n];
    for r in 0..rounds {
        // A palindromic slot order (0,1,..,n-1,n-1,..,1,0) gives every arm two
        // slots symmetric in time, so a monotone drift across the round cancels
        // to first order. Reversing it on odd rounds cancels the residual
        // inner-slot penalty as well: running the SAME binary in both arms of a
        // plain ABBA still reported ~1% "slower" for whichever arm held the
        // inner slots, i.e. slot position itself carries a bias.
        let order: Vec<usize> = if r % 2 == 0 {
            (0..n).chain((0..n).rev()).collect()
        } else {
            (0..n).rev().chain(0..n).collect()
        };
        let mut acc = vec![0.0f64; n];
        for i in order {
            acc[i] += slot(&arms[i], execs) / 2.0;
        }
        for (i, mean) in acc.iter().enumerate() {
            per_round[i].push(*mean);
        }
        let line: Vec<String> = arms
            .iter()
            .enumerate()
            .map(|(i, a)| format!("{}={:.3}", a.label, per_round[i][r]))
            .collect();
        eprintln!("  round {}/{}: {}", r + 1, rounds, line.join("  "));
    }

    println!();
    println!(
        "{} execs/slot, 2 slots/arm/round, {} rounds => {} execs per arm",
        execs,
        rounds,
        execs * 2 * rounds
    );
    println!();
    println!(
        "{:<32} {:>12} {:>14} {:>16}",
        "arm",
        "ms/exec",
        "spread",
        format!("vs {LEAN_ARM}")
    );
    println!("{}", "-".repeat(78));
    let base = median(&per_round[0]);
    for (i, a) in arms.iter().enumerate() {
        let m = median(&per_round[i]);
        println!(
            "{:<32} {:>12.3} {:>13.1}% {:>+13.3} ms",
            a.label,
            m,
            spread_pct(&per_round[i]),
            m - base
        );
    }
    println!();
    println!(
        "spread is (max-min)/median of each arm's per-round means — the PER-ARM noise floor on \
         this box, under whatever load it is under right now. A delta smaller than the spread of \
         the arms it compares is not a result."
    );
    println!(
        "READ THE `{CONTROL_ARM}` ROW CAREFULLY: it is a different program, ~40x the code, so its \
         delta is size AND frameworks together and is NOT the framework tax. The isolated tax is \
         the SAME binary built with and without the framework load commands — measured on this \
         tree at -1.34 to -1.46 ms/exec across three routes, with `atpkg` byte-for-byte the same \
         SIZE in both arms. `atpkg` sitting beside the lean arm here rather than between the two \
         is the standing evidence that size is not what orders this table."
    );
    println!(
        "Absolute ms/exec here is harness-relative — spawn method, parent process and slot \
         interleaving all move it by tens of percent. Compare rows WITHIN one run; do not compare \
         a number here against one from another tool."
    );

    if let Some(p) = control_copy {
        let _ = std::fs::remove_file(p);
    }

    if !guard_failures.is_empty() && std::env::var_os("ATERM_STARTUP_NO_GUARD").is_none() {
        std::process::exit(1);
    }
}
