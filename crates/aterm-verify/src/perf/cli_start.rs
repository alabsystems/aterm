// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `cli-start` floor lane: the instructions one aterm process start retires, and how much of
//! them loading the dylibs aterm links costs.
//!
//! Every agent's `aterm ctl` call, and every bridge, broker and notifier relaunch, is a fresh
//! start of the one shipped executable. The lane times none of them by the clock: it counts what
//! the kernel's per-thread counters say each start retired. Each launch is spawned through
//! [`aterm_uds::spawnfd::command`], observed exited without being reaped (`waitid` with
//! `WNOWAIT`, [`aterm_uds::spawnfd::Child::exited`], woken by the kernel's exit report), read
//! while the zombie still holds its usage (`proc_pid_rusage`'s `RUSAGE_INFO_V4`:
//! `ri_instructions`, `ri_cycles`, and its user and system time), and only then reaped.
//! Instructions retired move far less with the machine's load than time does, so the lane takes
//! no quiet claim and reports the load it met beside its numbers, and the wall time from spawn to
//! exit, which moves with it.
//!
//! The arms, each launched `--samples` times (default 50), every round of them in turn and the
//! order reversed every other round (A B B A), so a drift falls on every arm alike:
//!
//! - `version`: `aterm --version`;
//! - `ctl-help`: `aterm ctl --help`;
//! - `ctl-status`: `aterm ctl --sock <sock> @<sid> status` against a headless `aterm-lab
//!   instance` of the same aterm, its first session;
//! - the controls, the programs of `aterm-start-controls`, whose `main`s do no work:
//!   `start-control-none`, `start-control-all` and `start-control-cli` are empty `main`s, and
//!   `start-control-binds`' only hands its table of addresses to `black_box`.
//!   `start-control-none` links nothing beyond what every Rust program links;
//!   `start-control-all` links every dylib aterm's load commands name; `start-control-cli` all of
//!   those but the window-only frameworks ([`WINDOW_ONLY`]); `start-control-binds` what `all`
//!   links, with chained fixups, and holds the address of every symbol aterm's chained fixups
//!   import ([`chained_imports`]). The lane builds them at every run with the flags [`link_flag`]
//!   makes of aterm's load commands and the names its import table gives, so the controls follow
//!   what aterm links and binds and nothing lists it by hand, and it holds each control's own
//!   load commands to what it was built to load ([`mislinked`]). The `binds` control is built on
//!   its own, after the others: where it cannot be built, or aterm's imports cannot be read, it
//!   is NOT MEASURED and the rest of the run goes on;
//! - the stops ([`STOPS`]), aterm launched with a verb its `main` refuses at a known point before
//!   any verb's work: `main` at `main`'s first call, `prelude` once the front door installed its
//!   hooks and routed argv past its aliases. Each is known by its refusal, its exit code and what
//!   it writes to stderr, which its probe launch must end with ([`ended_elsewhere`]).
//!
//! The split of `aterm --version`: `none` is the trivial start any program pays; `all` over it
//! is loading aterm's dylibs; aterm over `all` is aterm's own start (its fixups, static
//! initializers and the verb); `all` over `cli` is what taking the window-only frameworks out of
//! the link would save a CLI start, which is less than what they cost alone, because the dylibs
//! `cli` keeps load most of them in turn. The stops split aterm's own start in turn: `main` over
//! `all` is loading aterm's image and starting the Rust runtime, `prelude` over `main` the front
//! door's hooks and argv, and the start over `prelude` its verb. `binds` over `all` is the part
//! of the image's load that binding aterm's imports costs, which dyld resolves symbol by symbol
//! at every start (most of it, `docs/measured/cli-start-own-2026-11-04.md`): those beyond the
//! Rust runtime's own, which `all` binds too and so drop out of the difference. Every arm's one
//! unmeasured probe launch runs with `DYLD_PRINT_LIBRARIES`, and the report attributes the split
//! by the images each one loaded: the window-only frameworks `cli` loads all the same, the
//! images `all` loads and `cli` does not, and what aterm loads beyond `all`.
//!
//! The controls link; they do not insert. `DYLD_INSERT_LIBRARIES` naming aterm's dylibs into
//! the bare control loads the same 588 images as linking them, yet retired 39.03 M instructions
//! to the linked control's 29.53 M in one run (`docs/measured/cli-start-2026-11-04.md`).
//!
//! Judged: each of the three starts' ([`GATED`]) median instructions against a ceiling, its
//! committed baseline times [`CEILING_RATIO`] (`perf-baseline-cli-start.json`, `--record
//! cli-start` rewrites it), and only they feed the trend. A gated arm the run did not measure is
//! NOT JUDGED, by name and why. The controls are the OS's costs and the stops are refusals no
//! caller makes: both split the starts, and are reported, never judged.

use std::path::{Path, PathBuf};

use aterm_json::Value;

use super::trend::utc_date;
use super::{LaneRun, Status, TrendSample};

/// The lane's name: the CLI's, the trend ledger's and the baseline's.
pub(crate) const LANE: &str = "cli-start";

/// Launches per arm when `--samples` names none.
pub(crate) const DEFAULT_SAMPLES: usize = 50;

/// A gated arm passes while its median is at most its baseline times this. A start's
/// instructions vary by a few percent run to run; a quarter more is a new cost.
pub(crate) const CEILING_RATIO: f64 = 1.25;

/// The committed baseline's file under [`super::floors::golden_dir`].
const BASELINE_FILE: &str = "perf-baseline-cli-start.json";

/// The `_comment` a first recording writes.
const COMMENT: &str = "aterm CLI-START baseline: the median instructions retired, in millions, \
                       of one fresh aterm process per arm (`aterm --version`, `aterm ctl --help`, \
                       `aterm ctl --sock <headless lab instance> @<sid> status`), read by \
                       proc_pid_rusage (RUSAGE_INFO_V4) before the reap, release build. \
                       SMALLER-IS-BETTER: a median above baseline x ceiling_ratio fails. The \
                       lane runs at the load it finds and reports it. \
                       Re-record with targo --unverified verify perf --record cli-start.";

/// The package of the control programs.
const CONTROLS: &str = "aterm-start-controls";

/// The controls the split rests on: each one's name and its program's.
const CONTROL_BINS: [(&str, &str); 3] = [
    ("none", "start-control-none"),
    ("all", "start-control-all"),
    ("cli", "start-control-cli"),
];

/// The `binds` control, which prices aterm's imports where the lane can build it: its name and
/// its program's.
const BINDS_BIN: (&str, &str) = ("binds", "start-control-binds");

/// The frameworks only aterm's window uses, by short name: the `cli` control links every dylib
/// aterm links but these. They are what E08's step 2 would defer behind `dlopen`; the report
/// names those the `cli` control loads all the same, through the dylibs it keeps.
const WINDOW_ONLY: [&str; 8] = [
    "AppKit",
    "AudioToolbox",
    "Carbon",
    "CoreText",
    "CoreVideo",
    "ImageIO",
    "Metal",
    "QuartzCore",
];

/// The gated arms: each one's name and its key in the baseline.
const GATED: [(&str, &str); 3] = [
    ("version", "version_minstr"),
    ("ctl-help", "ctl_help_minstr"),
    ("ctl-status", "ctl_status_minstr"),
];

/// A stop: aterm launched with `verb` and no other word, which its `main` refuses at a point
/// before any verb's work, exiting `exit` and writing `says` to stderr (nothing, where it is
/// empty).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Stop {
    pub name: &'static str,
    pub verb: &'static str,
    pub exit: i32,
    pub says: &'static str,
}

/// The stops.
///
/// - `main`: `aterm __build_hold` with no words, refused by `main`'s first call with 2 and
///   nothing written, so the launch is aterm's image loaded, the Rust runtime started and its
///   exit, nothing of `main`'s.
/// - `prelude`: `aterm __tcc-open` with no path, refused with 1 and its one stderr line once the
///   front door installed its hooks and routed argv past its aliases and the verbs ahead of it.
///
/// The code alone does not place a stop: 2 is also aterm's usage refusal of a word no route
/// takes (`aterm: unknown command '…'`), which an aterm without the hold verb gives only past
/// its whole front door. A stop whose probe launch ends with another code or writes anything else
/// to stderr is not where it should be ([`ended_elsewhere`]), and the lane refuses the run.
const STOPS: [Stop; 2] = [
    Stop {
        name: "main",
        verb: aterm_uds::build_front::HOLD_VERB,
        exit: 2,
        says: "",
    },
    Stop {
        name: "prelude",
        verb: aterm_containment::consent::OPEN_PROBE_VERB,
        exit: 1,
        says: aterm_containment::consent::OPEN_PROBE_REFUSAL,
    },
];

/// Why a probe launch of the arm `name`, which must exit `exit` and, where `says` names it, write
/// just that to stderr, ended elsewhere: it exited `got` and wrote `stderr`, read without the
/// lines dyld wrote (`dyld[<pid>]: …`). `None` when it ended where it should; where `says` is
/// `None`, only its code is held.
pub(crate) fn ended_elsewhere(
    name: &str,
    exit: i32,
    says: Option<&str>,
    got: Option<i32>,
    stderr: &str,
) -> Option<String> {
    let own: Vec<&str> = stderr.lines().filter(|l| !l.starts_with("dyld[")).collect();
    let own = own.join("\n");
    let own = own.trim_end();
    if got == Some(exit) && says.is_none_or(|says| own == says) {
        return None;
    }
    Some(match says {
        Some(says) => format!(
            "{name} exited {got:?} and wrote `{own}`, not {exit} and `{says}`: it did not stop \
             where it should"
        ),
        None => format!("{name} exited {got:?}, not {exit}: {own}"),
    })
}

/// One arm's launches, as measured.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Takes {
    pub instructions: Vec<f64>,
    pub cycles: Vec<f64>,
    pub wall_ms: Vec<f64>,
    pub user_ms: Vec<f64>,
    pub system_ms: Vec<f64>,
    pub resident_mb: Vec<f64>,
}

/// One arm's summary: medians and the instruction range.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Summary {
    pub n: usize,
    pub instructions: f64,
    pub min: f64,
    pub max: f64,
    pub cycles: f64,
    pub wall_ms: f64,
    pub user_ms: f64,
    pub system_ms: f64,
    /// The resident size the kernel recorded for it at its exit, in MiB.
    pub resident_mb: f64,
}

impl Takes {
    pub(crate) fn summary(&self) -> Option<Summary> {
        if self.instructions.is_empty() {
            return None;
        }
        let low = self
            .instructions
            .iter()
            .copied()
            .fold(f64::INFINITY, f64::min);
        let high = self
            .instructions
            .iter()
            .copied()
            .fold(f64::NEG_INFINITY, f64::max);
        Some(Summary {
            n: self.instructions.len(),
            instructions: super::stats::median(&self.instructions),
            min: low,
            max: high,
            cycles: super::stats::median(&self.cycles),
            wall_ms: super::stats::median(&self.wall_ms),
            user_ms: super::stats::median(&self.user_ms),
            system_ms: super::stats::median(&self.system_ms),
            resident_mb: super::stats::median(&self.resident_mb),
        })
    }
}

/// A measured arm: its name, what it links (a control's own load commands; empty for aterm's
/// arms), the images its probe launch loaded (its main executable left out; none when dyld
/// printed none) and its summary.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Measured {
    pub name: String,
    pub loads: Vec<String>,
    pub images: Vec<String>,
    pub summary: Summary,
}

/// What the split takes each aterm start over, as medians in instructions: the controls', and
/// the `binds` control's and the stops' (`(main, prelude)`) where the run measured them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Bases {
    pub none: f64,
    pub all: f64,
    pub cli: f64,
    pub binds: Option<f64>,
    pub stops: Option<(f64, f64)>,
}

/// The split of one aterm start by the controls and the stops, in instructions.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Split {
    pub start: f64,
    pub trivial: f64,
    pub frameworks: f64,
    pub deferral: f64,
    pub own: f64,
    /// aterm's own start by the stops, where the run measured both: loading aterm's image and
    /// starting the Rust runtime (`main` - `all`), the front door's prelude (`prelude` -
    /// `main`) and the verb (the start - `prelude`).
    pub own_by_stop: Option<[f64; 3]>,
    /// What binding aterm's imports costs (`binds` - `all`), where the run measured it: a part
    /// of the image's load.
    pub binding: Option<f64>,
}

impl Split {
    /// `start`'s median split over `bases`.
    pub(crate) fn of(start: f64, bases: &Bases) -> Split {
        let all = bases.all;
        Split {
            start,
            trivial: bases.none,
            frameworks: all - bases.none,
            deferral: all - bases.cli,
            own: start - all,
            own_by_stop: bases
                .stops
                .map(|(main, prelude)| [main - all, prelude - main, start - prelude]),
            binding: bases.binds.map(|binds| binds - all),
        }
    }

    /// `part` as a percentage of the start.
    pub(crate) fn share(&self, part: f64) -> f64 {
        if self.start > 0.0 {
            100.0 * part / self.start
        } else {
            0.0
        }
    }

    /// Its parts as the report prints them: each one's label and its instructions, aterm's own
    /// by the stops and the binding last where it has them.
    pub(crate) fn rows(&self) -> Vec<(&'static str, f64)> {
        let mut rows = vec![
            ("trivial start (none)", self.trivial),
            ("frameworks (all - none)", self.frameworks),
            ("  window-only deferral (all - cli)", self.deferral),
            ("aterm's own (it - all)", self.own),
        ];
        if let Some([image, prelude, verb]) = self.own_by_stop {
            rows.push(("  image and runtime (main - all)", image));
            if let Some(binding) = self.binding {
                rows.push(("    of it, its imports (binds - all)", binding));
            }
            rows.extend([
                ("  front door prelude (prelude - main)", prelude),
                ("  the verb (it - prelude)", verb),
            ]);
        } else if let Some(binding) = self.binding {
            rows.push(("  of it, its imports (binds - all)", binding));
        }
        rows
    }
}

/// A dylib's short name: its file name (`AppKit`, `libobjc.A.dylib`).
pub(crate) fn short(load: &str) -> &str {
    load.rsplit('/').next().unwrap_or(load)
}

/// What `paths` names beyond `base`, by short name, in `paths`' order.
pub(crate) fn beyond<'a>(paths: &'a [String], base: &[String]) -> Vec<&'a str> {
    paths
        .iter()
        .filter(|l| !base.contains(l))
        .map(|l| short(l))
        .collect()
}

/// The linker flag that makes a program load `dylib`, an install name from aterm's load
/// commands: `-needed_framework` for a framework of `/System/Library/Frameworks`, `-needed-l` for
/// a library of `/usr/lib`; why not, for any other.
pub(crate) fn link_flag(dylib: &str) -> Result<String, String> {
    let name = if let Some(rest) = dylib.strip_prefix("/System/Library/Frameworks/") {
        rest.split_once(".framework/")
            .map(|(name, _)| name)
            .filter(|name| !name.contains('/') && short(dylib) == *name)
            .map(|name| format!("-Wl,-needed_framework,{name}"))
    } else {
        dylib
            .strip_prefix("/usr/lib/lib")
            .filter(|file| !file.contains('/'))
            .and_then(|file| file.strip_suffix(".dylib"))
            .and_then(|stem| stem.split('.').next())
            .filter(|name| !name.is_empty())
            .map(|name| format!("-Wl,-needed-l{name}"))
    };
    name.ok_or_else(|| {
        format!(
            "no control can link {dylib}: it is neither a framework of /System/Library/Frameworks \
             nor a library of /usr/lib"
        )
    })
}

/// What the `cli` control links of `aterm`'s dylibs: all but [`WINDOW_ONLY`], in aterm's order.
pub(crate) fn cli_of(aterm: &[String]) -> Vec<String> {
    aterm
        .iter()
        .filter(|d| !WINDOW_ONLY.contains(&short(d)))
        .cloned()
        .collect()
}

/// The linker flags for `dylibs`, one each, joined as the controls' build script reads them.
pub(crate) fn link_flags(dylibs: &[String]) -> Result<String, String> {
    let flags = dylibs
        .iter()
        .map(|d| link_flag(d))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(flags.join(" "))
}

/// Why the control `name`, whose load commands name `got`, is not the program it was built to
/// be, one that loads `want` and what `none` loads: what it misses and what it adds, by short
/// name; `None` when it is that program.
pub(crate) fn mislinked(
    name: &str,
    got: &[String],
    want: &[String],
    none: &[String],
) -> Option<String> {
    let missing: Vec<&str> = want
        .iter()
        .filter(|d| !got.contains(d))
        .map(|d| short(d))
        .collect();
    let extra: Vec<&str> = got
        .iter()
        .filter(|d| !want.contains(d) && !none.contains(d))
        .map(|d| short(d))
        .collect();
    if missing.is_empty() && extra.is_empty() {
        return None;
    }
    Some(format!(
        "the `{name}` control is not the program it was built to be: it misses {} and adds {}, \
         so its split would be of another program",
        listed(&missing),
        listed(&extra)
    ))
}

/// The images a launch under `DYLD_PRINT_LIBRARIES` loaded, read from what dyld wrote to its
/// stderr (`dyld[<pid>]: <<uuid>> <path>`), in load order, the first (its main executable) left
/// out. Any other line is not dyld's and is passed over.
pub(crate) fn images_of(stderr: &str) -> Vec<String> {
    stderr
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("dyld[")?.split_once("]: ")?.1;
            let path = match rest.strip_prefix('<') {
                Some(tagged) => tagged.split_once("> ")?.1,
                None => rest,
            };
            path.starts_with('/').then(|| path.to_string())
        })
        .skip(1)
        .collect()
}

/// The symbols a Mach-O imports, by its chained fixups' import table (`LC_DYLD_CHAINED_FIXUPS`),
/// in table order: the binds dyld resolves at its load. A universal file is read at its slice for
/// `cpu` (a Mach-O `cputype`). Why not, for a file with no such slice or no chained fixups (an
/// x86_64 aterm links the older opcodes), or a table this reader does not take.
pub(crate) fn chained_imports(bytes: &[u8], cpu: u32) -> Result<Vec<String>, String> {
    const MH_MAGIC_64: u32 = 0xfeed_facf;
    const FAT_MAGIC: u32 = 0xcafe_babe;
    const FAT_MAGIC_64: u32 = 0xcafe_babf;
    const LC_DYLD_CHAINED_FIXUPS: u32 = 0x8000_0034;
    let le = |at: usize| -> Option<u32> {
        Some(u32::from_le_bytes(
            bytes.get(at..at.checked_add(4)?)?.try_into().ok()?,
        ))
    };
    let be = |at: usize| -> Option<u32> {
        Some(u32::from_be_bytes(
            bytes.get(at..at.checked_add(4)?)?.try_into().ok()?,
        ))
    };
    let be64 = |at: usize| -> Option<u64> {
        Some(u64::from_be_bytes(
            bytes.get(at..at.checked_add(8)?)?.try_into().ok()?,
        ))
    };
    let at = |v: u64| usize::try_from(v).ok();
    let magic = be(0).ok_or("not a Mach-O: shorter than its magic")?;
    let slice = if magic == FAT_MAGIC || magic == FAT_MAGIC_64 {
        let wide = magic == FAT_MAGIC_64;
        let arches = be(4).ok_or("a universal header with no count")?;
        let stride = if wide { 32 } else { 20 };
        (0..arches as usize)
            .map(|i| 8 + i * stride)
            .find(|&arch| be(arch) == Some(cpu))
            .and_then(|arch| {
                if wide {
                    at(be64(arch + 8)?)
                } else {
                    at(u64::from(be(arch + 8)?))
                }
            })
            .ok_or_else(|| format!("no slice for cputype {cpu:#x}"))?
    } else {
        0
    };
    if le(slice) != Some(MH_MAGIC_64) {
        return Err("not a 64-bit Mach-O".into());
    }
    if le(slice + 4) != Some(cpu) {
        return Err(format!("not a Mach-O for cputype {cpu:#x}"));
    }
    let ncmds = le(slice + 16).ok_or("a header cut short")?;
    let mut cmd = slice + 32;
    let mut fixups = None;
    for _ in 0..ncmds {
        let (Some(kind), Some(size)) = (le(cmd), le(cmd + 4)) else {
            return Err("a load command cut short".into());
        };
        if kind == LC_DYLD_CHAINED_FIXUPS {
            let off = le(cmd + 8).ok_or("LC_DYLD_CHAINED_FIXUPS cut short")?;
            fixups = Some(slice + off as usize);
            break;
        }
        if size < 8 {
            return Err("a load command smaller than its header".into());
        }
        cmd += size as usize;
    }
    let head = fixups.ok_or("no chained fixups (LC_DYLD_CHAINED_FIXUPS)")?;
    let field = |i: usize| le(head + 4 * i).ok_or("the chained fixups header is cut short");
    let (imports, symbols, count, format, compressed) =
        (field(2)?, field(3)?, field(4)?, field(5)?, field(6)?);
    if compressed != 0 {
        return Err(format!("symbol names in format {compressed} (compressed)"));
    }
    let (stride, name_of): (usize, fn(u64) -> u64) = match format {
        // DYLD_CHAINED_IMPORT: lib_ordinal 8, weak_import 1, name_offset 23.
        1 => (4, |e| e >> 9),
        // DYLD_CHAINED_IMPORT_ADDEND: the same, then an i32 addend.
        2 => (8, |e| (e & 0xffff_ffff) >> 9),
        // DYLD_CHAINED_IMPORT_ADDEND64: lib_ordinal 16, weak_import 1, reserved 15,
        // name_offset 32, then a u64 addend.
        3 => (16, |e| e >> 32),
        other => return Err(format!("imports in format {other}")),
    };
    let names = head + symbols as usize;
    (0..count as usize)
        .map(|i| {
            let entry = head + imports as usize + i * stride;
            let raw = if format == 3 {
                le(entry)
                    .zip(le(entry + 4))
                    .map(|(low, high)| u64::from(low) | (u64::from(high) << 32))
            } else {
                le(entry).map(u64::from)
            }
            .ok_or_else(|| format!("import {i} is past the file's end"))?;
            let start = names + at(name_of(raw)).ok_or("a name offset past usize")?;
            let name = bytes
                .get(start..)
                .and_then(|rest| rest.split(|b| *b == 0).next())
                .filter(|name| !name.is_empty())
                .ok_or_else(|| format!("import {i} names no symbol"))?;
            String::from_utf8(name.to_vec()).map_err(|_| format!("import {i}'s name is not UTF-8"))
        })
        .collect()
}

/// The sid of the first session a `ctl ls` reply lists: a row's third word (`<pid> <local>
/// <sid> …`).
pub(crate) fn sid_of_ls(listed: &str) -> Option<String> {
    listed
        .lines()
        .find_map(|line| line.split_whitespace().nth(2))
        .map(str::to_string)
}

/// Millions, as the report and the baseline write instructions.
fn m(v: f64) -> f64 {
    v / 1e6
}

/// `names` joined, or `none`.
fn listed(names: &[&str]) -> String {
    if names.is_empty() {
        "none".to_string()
    } else {
        names.join(", ")
    }
}

/// The report: one row per arm, the split of each aterm arm and the images that attribute it.
pub(crate) fn report(arms: &[Measured]) -> String {
    let mut s = String::from(
        "    arm                  instr M  (min-max)          cycles M  user ms   sys ms  wall ms  \
         rss MiB\n",
    );
    for a in arms {
        let x = a.summary;
        s.push_str(&format!(
            "    {:<20} {:>7.2}  ({:>6.2}-{:<6.2})    {:>7.2}  {:>7.2}  {:>7.2}  {:>7.2}  {:>7.2}\n",
            a.name,
            m(x.instructions),
            m(x.min),
            m(x.max),
            m(x.cycles),
            x.user_ms,
            x.system_ms,
            x.wall_ms,
            x.resident_mb
        ));
    }
    let find = |name: &str| arms.iter().find(|a| a.name == name);
    let (Some(none), Some(all), Some(cli)) = (find("none"), find("all"), find("cli")) else {
        return s;
    };
    let bases = Bases {
        none: none.summary.instructions,
        all: all.summary.instructions,
        cli: cli.summary.instructions,
        binds: find("binds").map(|binds| binds.summary.instructions),
        stops: find("main")
            .zip(find("prelude"))
            .map(|(main, prelude)| (main.summary.instructions, prelude.summary.instructions)),
    };
    let starts: Vec<&Measured> = GATED.iter().filter_map(|(arm, _)| find(arm)).collect();
    let splits: Vec<Split> = starts
        .iter()
        .map(|start| Split::of(start.summary.instructions, &bases))
        .collect();
    s.push_str("    split, instr M and share of the start:  ");
    for start in &starts {
        s.push_str(&format!(" {:>15}", start.name));
    }
    s.push('\n');
    let labels: Vec<&str> = splits
        .first()
        .map(|x| x.rows().into_iter().map(|(label, _)| label).collect())
        .unwrap_or_default();
    for (row, label) in labels.iter().enumerate() {
        s.push_str(&format!("      {label:<38}"));
        for split in &splits {
            let v = split.rows()[row].1;
            s.push_str(&format!(" {:>7.2} {:>4.0}%  ", m(v), split.share(v)));
        }
        s.push('\n');
    }
    s.push_str(&format!(
        "      images at launch: none {}, all {}, cli {}\n",
        none.images.len(),
        all.images.len(),
        cli.images.len()
    ));
    let deferred: Vec<String> = all
        .loads
        .iter()
        .filter(|l| !cli.loads.contains(l))
        .cloned()
        .collect();
    let kept: Vec<String> = deferred
        .iter()
        .filter(|l| cli.images.contains(l))
        .cloned()
        .collect();
    s.push_str(&format!(
        "      window-only frameworks aterm links: {}\n",
        listed(&beyond(&deferred, &[]))
    ));
    s.push_str(&format!(
        "      of them, `cli` loads through the dylibs it keeps: {}\n",
        listed(&beyond(&kept, &[]))
    ));
    let dropped = beyond(&all.images, &cli.images);
    s.push_str(&format!(
        "      the {} images `all` loads and `cli` does not: {}\n",
        dropped.len(),
        listed(&dropped)
    ));
    for start in &starts {
        if start.images.is_empty() {
            s.push_str(&format!(
                "      {}: dyld printed none of its images (it ignores DYLD_* variables for a \
                 hardened or restricted program)\n",
                start.name
            ));
            continue;
        }
        s.push_str(&format!(
            "      {}: {} images; beyond `all`'s: {}",
            start.name,
            start.images.len(),
            listed(&beyond(&start.images, &all.images))
        ));
        let fewer = beyond(&all.images, &start.images);
        if !fewer.is_empty() {
            s.push_str(&format!(
                "; `all` loads and it does not: {}",
                listed(&fewer)
            ));
        }
        s.push('\n');
    }
    s
}

/// The gated medians, in millions, keyed as the baseline keys them.
pub(crate) fn gated(arms: &[Measured]) -> Vec<(&'static str, f64)> {
    GATED
        .iter()
        .filter_map(|(arm, key)| {
            arms.iter()
                .find(|a| a.name == *arm)
                .map(|a| (*key, m(a.summary.instructions)))
        })
        .collect()
}

/// The baseline file's text for `values`.
pub(crate) fn baseline_json(values: &[(&str, f64)], comment: &str) -> String {
    let mut s = format!(
        "{{\n  \"_comment\": {},\n",
        aterm_json::to_string(&Value::String(comment.to_string()))
            .unwrap_or_else(|_| "\"\"".to_string())
    );
    for (key, v) in values {
        s.push_str(&format!("  \"{key}\": {v:.3},\n"));
    }
    s.push_str(&format!("  \"ceiling_ratio\": {CEILING_RATIO:.3}\n}}\n"));
    s
}

/// Judge `values` against `baseline` (the committed file's JSON), key by key: a value above its
/// baseline times the file's `ceiling_ratio` fails; a key the baseline lacks is not judged.
pub(crate) fn judge(values: &[(&'static str, f64)], baseline: &Value) -> LaneRun {
    let ratio = super::floors::number(baseline, "ceiling_ratio").unwrap_or(CEILING_RATIO);
    let mut run = LaneRun::pass();
    let mut judged = 0;
    for (key, v) in values {
        match super::floors::number(baseline, key).filter(|b| *b > 0.0) {
            None => {
                eprintln!("    {key}: NOT JUDGED — the baseline has no positive {key}");
                run.unjudged.push(format!("{LANE}/{key} (no baseline)"));
            }
            Some(b) => {
                judged += 1;
                let ceiling = b * ratio;
                if *v <= ceiling {
                    eprintln!("    {key}: GREEN — {v:.2} M <= ceiling {ceiling:.2} M");
                } else {
                    eprintln!("    {key}: FAILED — {v:.2} M > ceiling {ceiling:.2} M");
                    run.add_failure(format!(
                        "{key} {v:.2} M > ceiling {ceiling:.2} M (baseline {b:.2} x {ratio:.2})"
                    ));
                }
            }
        }
    }
    if judged == 0 && run.status == Status::Pass {
        return LaneRun::not_judged("no gated arm has a baseline");
    }
    run
}

/// The refusal `--record` meets when the aterm measured is not this tree's own build (`built`
/// false: an aterm `--binary` names), whose baseline it would otherwise rewrite.
fn record_refused(record: bool, built: bool) -> Option<LaneRun> {
    (record && !built).then(|| {
        LaneRun::fail(
            "REFUSING to record an aterm --binary names: the baseline is of this tree's own \
             release build, which --record cli-start builds; drop --binary",
        )
    })
}

/// Record or judge the gated medians against `baseline` (the committed file's path), and hand
/// them to the trend (smaller is better) when the aterm measured is this tree's own build
/// (`built`): the trend keys its rows by the tree, so an aterm `--binary` names is judged but
/// feeds it nothing, and never records. A gated arm `arms` lacks is NOT JUDGED, named with the
/// reason `unmeasured` gives for it (`(arm, why)`), and a recording refuses it.
fn conclude(
    arms: &[Measured],
    unmeasured: &[(String, String)],
    record: bool,
    built: bool,
    baseline: &Path,
) -> LaneRun {
    conclude_using(
        arms,
        unmeasured,
        record,
        built,
        baseline,
        super::publication::publish,
    )
}

fn conclude_using(
    arms: &[Measured],
    unmeasured: &[(String, String)],
    record: bool,
    built: bool,
    baseline: &Path,
    publish: super::publication::Publisher,
) -> LaneRun {
    if let Some(refused) = record_refused(record, built) {
        return refused;
    }
    let values = gated(arms);
    let missing: Vec<String> = GATED
        .iter()
        .filter(|(arm, _)| !arms.iter().any(|a| a.name == *arm))
        .map(|(arm, key)| {
            let why = unmeasured
                .iter()
                .find(|(name, _)| name == arm)
                .map_or("no launch of it was measured", |(_, why)| why.as_str());
            eprintln!("    {key}: NOT JUDGED — not measured: {why}");
            format!("{LANE}/{key} (not measured: {why})")
        })
        .collect();
    let previous = crate::read_regular_text(baseline)
        .ok()
        .and_then(|t| aterm_json::from_str::<Value>(t.trim()).ok())
        .filter(Value::is_object);
    let mut run = if record {
        if !missing.is_empty() {
            return LaneRun::fail(format!(
                "REFUSING to record — {}; {} left untouched",
                missing.join("; "),
                baseline.display()
            ));
        }
        let (comment, _) = super::floors::carried_comment(previous.as_ref(), COMMENT, &utc_date());
        match publish(baseline, &baseline_json(&values, &comment)) {
            Ok(()) => {
                eprintln!("  RECORDED baseline -> {}", baseline.display());
                LaneRun {
                    status: Status::Recorded(baseline.to_path_buf()),
                    ..LaneRun::pass()
                }
            }
            Err(e) => LaneRun::fail(format!("could not write {}: {e}", baseline.display())),
        }
    } else {
        match previous {
            Some(base) => judge(&values, &base),
            None => {
                eprintln!(
                    "  {LANE}: no baseline at {} — judged against nothing (record with --record \
                     {LANE}).",
                    baseline.display()
                );
                LaneRun::not_judged("no baseline")
            }
        }
    };
    run.unjudged.extend(missing);
    if !built {
        eprintln!(
            "  {LANE}: the trend is not fed: --binary names an aterm this tree did not build"
        );
        return run;
    }
    run.samples = values
        .iter()
        .map(|(key, v)| TrendSample {
            lane: LANE,
            metric: (*key).to_string(),
            value: *v,
            inverted: true,
            at: Vec::new(),
        })
        .collect();
    run
}

/// The JSON `--json-out` writes: every arm's summary, what it links (a control's load commands)
/// and how many images it loaded, and the load the lane met.
fn json(arms: &[Measured], aterm: &Path, load: &str) -> Value {
    let mut rows = aterm_json::Map::new();
    for a in arms {
        let x = a.summary;
        let count = |n: usize| Value::from(u64::try_from(n).unwrap_or(u64::MAX));
        let mut row = aterm_json::Map::new();
        row.insert("n".into(), count(x.n));
        row.insert("instructions_median".into(), Value::from(x.instructions));
        row.insert("instructions_min".into(), Value::from(x.min));
        row.insert("instructions_max".into(), Value::from(x.max));
        row.insert("cycles_median".into(), Value::from(x.cycles));
        row.insert("wall_ms_median".into(), Value::from(x.wall_ms));
        row.insert("user_ms_median".into(), Value::from(x.user_ms));
        row.insert("system_ms_median".into(), Value::from(x.system_ms));
        row.insert("resident_mb_median".into(), Value::from(x.resident_mb));
        row.insert(
            "loads".into(),
            Value::Array(a.loads.iter().map(|l| Value::from(l.as_str())).collect()),
        );
        row.insert("images".into(), count(a.images.len()));
        rows.insert(a.name.clone(), Value::Object(row));
    }
    let mut out = aterm_json::Map::new();
    out.insert("lane".into(), Value::from(LANE));
    out.insert("aterm".into(), Value::from(aterm.display().to_string()));
    out.insert("load_average".into(), Value::from(load));
    out.insert("arms".into(), Value::Object(rows));
    Value::Object(out)
}

/// What the lane was asked: the aterm to measure (a fresh release build when `None`), the
/// launches per arm, where its JSON goes.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Args {
    pub binary: Option<PathBuf>,
    pub samples: Option<usize>,
    pub json_out: Option<PathBuf>,
}

/// What one run measured: every arm, the aterm it launched, the load average the launches met,
/// and each gated arm it could not measure, with why (`(arm, why)`).
struct Measurement {
    arms: Vec<Measured>,
    aterm: PathBuf,
    load: String,
    unmeasured: Vec<(String, String)>,
}

/// The lane (macOS only: the perf module mounts it there and skips it elsewhere).
pub(crate) fn run(args: &Args, record: bool) -> LaneRun {
    let built = args.binary.is_none();
    if let Some(refused) = record_refused(record, built) {
        return refused;
    }
    match live::measure(args) {
        Ok(Measurement {
            arms,
            aterm,
            load,
            unmeasured,
        }) => {
            eprint!("{}", report(&arms));
            if let Some(path) = &args.json_out {
                let text = json(&arms, &aterm, &load).to_string();
                if let Err(e) = super::publication::publish(path, &(text + "\n")) {
                    return LaneRun::fail(format!("could not write {}: {e}", path.display()));
                }
                eprintln!("  json -> {}", path.display());
            }
            let baseline = super::floors::golden_dir().join(BASELINE_FILE);
            conclude(&arms, &unmeasured, record, built, &baseline)
        }
        Err(why) => why,
    }
}

mod live {
    //! The launches: the builds, the headless instance the `ctl-status` arm asks, and the
    //! counted spawns.

    use std::ffi::OsString;
    use std::io::BufRead as _;
    use std::path::{Path, PathBuf};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use aterm_uds::deadline::ExitWait;
    use aterm_uds::spawnfd::{Child, Stdio, command};

    use super::super::arena::{self, without_aterm_env};
    use super::super::{LaneRun, driver, workspace_root};
    use super::{Args, BINDS_BIN, CONTROL_BINS, CONTROLS, DEFAULT_SAMPLES, Measured, Takes};

    /// How long one launch may run before the lane calls it hung: a start takes milliseconds,
    /// and `ctl status` answers within its own default timeout.
    const HUNG: Duration = Duration::from_secs(60);

    /// How long `aterm-lab instance` may take to print its ready line: its own one-minute
    /// boot bound plus a one-minute hang detector.
    const INSTANCE_HUNG: Duration = Duration::from_secs(120);

    /// One arm: its name, the program and arguments it launches, what it links (a control's
    /// own load commands; empty for aterm's arms), the exit code every launch of it must end
    /// with (a stop's refusal; 0 for every other arm) and, for a stop, what its probe launch must
    /// write to stderr ([`super::Stop::says`]).
    struct Arm {
        name: String,
        program: PathBuf,
        args: Vec<OsString>,
        loads: Vec<String>,
        exit: i32,
        says: Option<&'static str>,
    }

    /// What one launch retired, how long it took from spawn to its exit, and its resident size
    /// in bytes at the exit.
    struct Take {
        instructions: u64,
        cycles: u64,
        user: Duration,
        system: Duration,
        wall: Duration,
        resident: u64,
    }

    /// Build, probe every arm, launch each `samples` times, and summarize: the arms, the aterm
    /// measured, the load average the launches met and the gated arm it could not measure.
    pub(super) fn measure(args: &Args) -> Result<super::Measurement, LaneRun> {
        let aterm = match &args.binary {
            Some(path) => path.clone(),
            None => arena::build_aterm().map_err(LaneRun::fail)?.bin,
        };
        if !aterm.is_file() {
            return Err(LaneRun::fail(format!(
                "{} is not a file; build it, or name one with --binary",
                aterm.display()
            )));
        }
        let aterm_loads = loads(&aterm).map_err(LaneRun::fail)?;
        let aterm_imports = match imports(&aterm) {
            Ok(names) => Some(names),
            Err(why) => {
                eprintln!("  binds: NOT MEASURED — {why}");
                None
            }
        };
        let controls =
            build_controls(&aterm_loads, aterm_imports.as_deref()).map_err(LaneRun::fail)?;
        let mut arms = vec![
            Arm {
                name: "version".into(),
                program: aterm.clone(),
                args: vec!["--version".into()],
                loads: Vec::new(),
                exit: 0,
                says: None,
            },
            Arm {
                name: "ctl-help".into(),
                program: aterm.clone(),
                args: vec!["ctl".into(), "--help".into()],
                loads: Vec::new(),
                exit: 0,
                says: None,
            },
        ];
        let mut unmeasured = Vec::new();
        let instance = match Instance::start(&aterm) {
            Ok(instance) => {
                arms.push(Arm {
                    name: "ctl-status".into(),
                    program: aterm.clone(),
                    args: vec![
                        "ctl".into(),
                        "--sock".into(),
                        instance.sock.clone().into(),
                        format!("@{}", instance.sid).into(),
                        "status".into(),
                    ],
                    loads: Vec::new(),
                    exit: 0,
                    says: None,
                });
                Some(instance)
            }
            Err(why) => {
                let why = format!("the headless instance: {why}");
                eprintln!("  ctl-status: NOT MEASURED — {why}");
                unmeasured.push(("ctl-status".to_string(), why));
                None
            }
        };
        arms.extend(super::STOPS.iter().map(|stop| Arm {
            name: stop.name.into(),
            program: aterm.clone(),
            args: vec![stop.verb.into()],
            loads: Vec::new(),
            exit: stop.exit,
            says: Some(stop.says),
        }));
        arms.extend(controls);
        let mut images = Vec::with_capacity(arms.len());
        for arm in &arms {
            images.push(probe(arm).map_err(LaneRun::fail)?);
        }
        let samples = args.samples.unwrap_or(DEFAULT_SAMPLES);
        let load_before = super::super::bench_build::load_average();
        eprintln!(
            "  {} arms x {samples} launches of {} and its controls, A B B A, at load {load_before}",
            arms.len(),
            aterm.display()
        );
        let mut takes: Vec<Takes> = arms.iter().map(|_| Takes::default()).collect();
        for round in 0..samples {
            let order: Vec<usize> = if round % 2 == 0 {
                (0..arms.len()).collect()
            } else {
                (0..arms.len()).rev().collect()
            };
            for i in order {
                let take = launch(&arms[i]).map_err(LaneRun::fail)?;
                takes[i].instructions.push(take.instructions as f64);
                takes[i].cycles.push(take.cycles as f64);
                takes[i].wall_ms.push(take.wall.as_secs_f64() * 1e3);
                takes[i].user_ms.push(take.user.as_secs_f64() * 1e3);
                takes[i].system_ms.push(take.system.as_secs_f64() * 1e3);
                takes[i]
                    .resident_mb
                    .push(take.resident as f64 / (1024.0 * 1024.0));
            }
        }
        let load_after = super::super::bench_build::load_average();
        drop(instance);
        let load = format!("{load_before} -> {load_after}");
        eprintln!("  load average: {load}");
        let measured = arms
            .into_iter()
            .zip(takes)
            .zip(images)
            .filter_map(|((arm, takes), images)| {
                takes.summary().map(|summary| Measured {
                    name: arm.name,
                    loads: arm.loads,
                    images,
                    summary,
                })
            })
            .collect();
        Ok(super::Measurement {
            arms: measured,
            aterm,
            load,
            unmeasured,
        })
    }

    /// The dylibs the Mach-O at `path` loads, in load-command order.
    fn loads(path: &Path) -> Result<Vec<String>, String> {
        atpkg::macho::load_names_at(path)
            .map(|names| names.once().loads)
            .map_err(|e| format!("cannot read the load commands of {}: {e}", path.display()))
    }

    /// The symbols the Mach-O at `path` imports, at this host's slice ([`super::chained_imports`]).
    fn imports(path: &Path) -> Result<Vec<String>, String> {
        const CPU_TYPE_ARM64: u32 = 0x0100_000c;
        const CPU_TYPE_X86_64: u32 = 0x0100_0007;
        let cpu = if cfg!(target_arch = "aarch64") {
            CPU_TYPE_ARM64
        } else {
            CPU_TYPE_X86_64
        };
        let bytes =
            std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        super::chained_imports(&bytes, cpu)
            .map_err(|why| format!("the imports of {}: {why}", path.display()))
    }

    /// `targo --unverified build --release -p aterm-start-controls`, its `all` control linking
    /// every dylib in `aterm` (aterm's load commands), its `cli` control all but the window-only
    /// ones and its `binds` control what `all` links and every symbol in `imports` (aterm's):
    /// each control's arm, held to its own load commands. The `binds` control is built by
    /// itself, after the others, and only it is optional: without `imports` (an aterm whose
    /// imports cannot be read) it is not built, and where its build fails it is NOT MEASURED,
    /// by why, and the run goes on without it.
    fn build_controls(aterm: &[String], imports: Option<&[String]>) -> Result<Vec<Arm>, String> {
        let cli = super::cli_of(aterm);
        let all_flags = super::link_flags(aterm)?;
        let cli_flags = super::link_flags(&cli)?;
        let imports = imports.map(|names| names.join(" "));
        // Both builds set the same variables, so the second reuses the first's build script run.
        let env = [
            ("ATERM_START_CONTROL_ALL", all_flags.as_str()),
            ("ATERM_START_CONTROL_CLI", cli_flags.as_str()),
            ("ATERM_START_CONTROL_BINDS", all_flags.as_str()),
            (
                "ATERM_START_CONTROL_IMPORTS",
                imports.as_deref().unwrap_or(""),
            ),
        ];
        let built = |bins: &[(&str, &str)]| -> Result<Vec<Arm>, String> {
            let mut args = vec![
                "--release",
                "-q",
                "-p",
                CONTROLS,
                "--message-format=json-render-diagnostics",
            ];
            for (_, bin) in bins {
                args.extend(["--bin", bin]);
            }
            let messages = build(&args, &env)?;
            bins.iter()
                .map(|(name, bin)| {
                    let program =
                        super::super::bench_build::built_executable(&messages, bin, "bin")?;
                    Ok(Arm {
                        name: (*name).to_string(),
                        loads: loads(&program)?,
                        program,
                        args: Vec::new(),
                        exit: 0,
                        says: None,
                    })
                })
                .collect()
        };
        let mut arms = built(&CONTROL_BINS)?;
        if imports.is_some() {
            match built(&[BINDS_BIN]) {
                Ok(binds) => arms.extend(binds),
                Err(why) => eprintln!("  binds: NOT MEASURED — {why}"),
            }
        }
        let none = arms[0].loads.clone();
        for (arm, want) in arms.iter().zip([&none[..], aterm, &cli[..], aterm]) {
            if let Some(why) = super::mislinked(&arm.name, &arm.loads, want, &none) {
                return Err(why);
            }
        }
        Ok(arms)
    }

    /// One `targo --unverified build` in the workspace, with `env` set for it: cargo's
    /// messages on success.
    fn build(args: &[&str], env: &[(&str, &str)]) -> Result<String, String> {
        let driver = driver::cargo_driver()?;
        eprintln!("  $ {}", driver.display_workspace("build", args));
        let mut cmd = driver.workspace_command("build");
        cmd.args(args)
            .current_dir(workspace_root())
            .stderr(Stdio::inherit());
        for (key, value) in env {
            cmd.env(key, value);
        }
        let out = cmd
            .output()
            .map_err(|e| format!("could not run targo: {e}"))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(format!(
                "`targo build {}` exited {:?}",
                args.join(" "),
                out.status.code()
            ))
        }
    }

    /// A command for `arm` as every launch of it runs: no inherited `ATERM_*` override and no
    /// inherited `DYLD_*` variable.
    fn command_of(arm: &Arm) -> aterm_uds::spawnfd::Command {
        let mut cmd = command(&arm.program);
        cmd.args(&arm.args);
        without_aterm_env(&mut cmd);
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("DYLD_") {
                cmd.env_remove(key);
            }
        }
        cmd
    }

    /// Launch `arm` once unmeasured, its output kept and `DYLD_PRINT_LIBRARIES` set: it must
    /// end with its code and, a stop, with its refusal's stderr ([`super::ended_elsewhere`];
    /// `ctl-status` must answer `OK`), the program's first exec is behind it before any launch
    /// is counted, and what it gives back is the images it loaded. Every counted launch runs the
    /// same program on the same words, so the probe is where a stop is placed; the counted ones
    /// are held to their code alone, their output discarded.
    fn probe(arm: &Arm) -> Result<Vec<String>, String> {
        let mut cmd = command_of(arm);
        cmd.env("DYLD_PRINT_LIBRARIES", "1");
        let out = aterm_uds::deadline::output_within(&mut cmd, HUNG)
            .map_err(|e| format!("{}: {e}", arm.name))?;
        let said = String::from_utf8_lossy(&out.stdout);
        let told = String::from_utf8_lossy(&out.stderr);
        if let Some(why) =
            super::ended_elsewhere(&arm.name, arm.exit, arm.says, out.status.code(), &told)
        {
            return Err(why);
        }
        if arm.name == "ctl-status" && !said.starts_with("OK") {
            return Err(format!(
                "ctl-status answered `{}`",
                said.lines().next().unwrap_or("")
            ));
        }
        Ok(super::images_of(&told))
    }

    /// Launch `arm` once: what it retired and its resident size, read before the reap, and its
    /// spawn-to-exit time.
    fn launch(arm: &Arm) -> Result<Take, String> {
        let mut cmd = command_of(arm);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let started = Instant::now();
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("{}: could not spawn: {e}", arm.name))?;
        let exited = await_exit(&mut child);
        let wall = started.elapsed();
        if let Err(why) = exited {
            aterm_uds::deadline::kill_within(&mut child, HUNG);
            return Err(format!("{}: {why}", arm.name));
        }
        let usage = rusage(child.id());
        let status = child
            .wait()
            .map_err(|e| format!("{}: could not reap: {e}", arm.name))?;
        if status.code() != Some(arm.exit) {
            return Err(format!("{} exited {status}, not {}", arm.name, arm.exit));
        }
        let usage = usage.ok_or_else(|| format!("{}: proc_pid_rusage named no usage", arm.name))?;
        Ok(Take {
            instructions: usage.ri_instructions,
            cycles: usage.ri_cycles,
            user: mach(usage.ri_user_time),
            system: mach(usage.ri_system_time),
            wall,
            resident: usage.ri_resident_size,
        })
    }

    /// Block until `child` has exited, leaving it unreaped, or say why not within [`HUNG`].
    fn await_exit(child: &mut Child) -> Result<(), String> {
        let hung = Instant::now() + HUNG;
        let mut exit = ExitWait::new(child);
        loop {
            if child.exited().map_err(|e| format!("waitid: {e}"))? {
                return Ok(());
            }
            let left = hung.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(format!("it did not exit in {HUNG:?}"));
            }
            exit.wait(left);
        }
    }

    /// A span of `rusage`'s Mach ticks (`ri_user_time`, `ri_system_time`), by the host's
    /// timebase; ticks as nanoseconds where the timebase cannot be read.
    fn mach(ticks: u64) -> Duration {
        static TIMEBASE: std::sync::OnceLock<(u32, u32)> = std::sync::OnceLock::new();
        let (numer, denom) = *TIMEBASE.get_or_init(|| {
            let mut info = libc::mach_timebase_info { numer: 0, denom: 0 };
            // SAFETY: `info` is writable for the one struct the call fills.
            let rc = unsafe { libc::mach_timebase_info(&raw mut info) };
            if rc == 0 && info.numer > 0 && info.denom > 0 {
                (info.numer, info.denom)
            } else {
                (1, 1)
            }
        });
        let nanos = u128::from(ticks) * u128::from(numer) / u128::from(denom);
        Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
    }

    /// What `pid` (an exited, unreaped child) used, by `proc_pid_rusage`'s `RUSAGE_INFO_V4`.
    fn rusage(pid: u32) -> Option<libc::rusage_info_v4> {
        let pid = libc::c_int::try_from(pid).ok()?;
        let mut info = std::mem::MaybeUninit::<libc::rusage_info_v4>::zeroed();
        // SAFETY: `info` is writable for one `rusage_info_v4`, the flavor asked for, and the
        // kernel keeps no pointer to it.
        let rc = unsafe {
            libc::proc_pid_rusage(
                pid,
                libc::RUSAGE_INFO_V4,
                info.as_mut_ptr().cast::<libc::rusage_info_t>(),
            )
        };
        // SAFETY: zeroed at creation, so initialized whatever the call did.
        (rc == 0).then(|| unsafe { info.assume_init() })
    }

    /// A headless `aterm-lab instance` of the aterm measured, held for the `ctl-status` arm and
    /// ended (`SIGTERM` to the lab, which ends its instance) on drop.
    struct Instance {
        lab: Child,
        sock: PathBuf,
        sid: String,
    }

    impl Instance {
        fn start(aterm: &Path) -> Result<Instance, String> {
            let messages = build(
                &[
                    "-q",
                    "-p",
                    "aterm-lab",
                    "--bin",
                    "aterm-lab",
                    "--message-format=json-render-diagnostics",
                ],
                &[],
            )?;
            let lab = super::super::bench_build::built_executable(&messages, "aterm-lab", "bin")?;
            let mut cmd = command(&lab);
            cmd.arg("instance")
                .arg("--aterm")
                .arg(aterm)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit());
            eprintln!("  $ {} instance --aterm {}", lab.display(), aterm.display());
            let mut child = cmd
                .spawn()
                .map_err(|e| format!("could not spawn aterm-lab: {e}"))?;
            let stdout = child
                .stdout
                .take()
                .ok_or("aterm-lab's stdout was not piped")?;
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                use std::io::Read as _;
                for line in std::io::BufReader::new(stdout.take(1024 * 1024)).lines() {
                    let Ok(line) = line else { break };
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            });
            let sock = match ready(&rx) {
                Ok(sock) => sock,
                Err(why) => {
                    aterm_uds::deadline::kill_within(&mut child, HUNG);
                    return Err(why);
                }
            };
            let mut instance = Instance {
                lab: child,
                sock,
                sid: String::new(),
            };
            instance.sid = first_session(aterm, &instance.sock)?;
            Ok(instance)
        }
    }

    /// The socket of the instance `aterm-lab instance` says is up (`instance pid=… sock=<path>
    /// …`), within [`INSTANCE_HUNG`].
    fn ready(lines: &mpsc::Receiver<String>) -> Result<PathBuf, String> {
        let hung = Instant::now() + INSTANCE_HUNG;
        loop {
            let left = hung.saturating_duration_since(Instant::now());
            match lines.recv_timeout(left) {
                Ok(line) => {
                    if let Some(sock) = line.strip_prefix("instance ").and_then(|rest| {
                        rest.split_whitespace()
                            .find_map(|w| w.strip_prefix("sock="))
                    }) {
                        return Ok(PathBuf::from(sock));
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(format!(
                        "aterm-lab instance said nothing ready in {INSTANCE_HUNG:?}"
                    ));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err("aterm-lab instance exited before its instance was ready".into());
                }
            }
        }
    }

    /// The first session `ls` lists on the instance at `sock`: its sid, a row's third word
    /// (`<pid> <local> <sid> …`).
    fn first_session(aterm: &Path, sock: &Path) -> Result<String, String> {
        let listed = arena::ctl_out(aterm, sock, &["ls"])?;
        super::sid_of_ls(&listed)
            .ok_or_else(|| format!("`ctl ls` named no session: {}", listed.trim_end()))
    }

    impl Drop for Instance {
        fn drop(&mut self) {
            let _ = self.lab.terminate();
            let hung = Instant::now() + HUNG;
            let mut exit = ExitWait::new(&self.lab);
            while !self.lab.exited().unwrap_or(true) {
                let left = hung.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    eprintln!("  aterm-lab instance outlived its SIGTERM by {HUNG:?}; killing it");
                    aterm_uds::deadline::kill_within(&mut self.lab, HUNG);
                    return;
                }
                exit.wait(left);
            }
            let _ = self.lab.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arm(name: &str, instructions: f64, links: &[&str], images: &[&str]) -> Measured {
        Measured {
            name: name.to_string(),
            loads: owned(links),
            images: owned(images),
            summary: Summary {
                n: 1,
                instructions,
                min: instructions,
                max: instructions,
                cycles: instructions / 2.0,
                wall_ms: 1.0,
                user_ms: 1.0,
                system_ms: 1.0,
                resident_mb: 1.0,
            },
        }
    }

    const SYS: &str = "/usr/lib/libSystem.B.dylib";
    const OBJC: &str = "/usr/lib/libobjc.A.dylib";
    const APPKIT: &str = "/System/Library/Frameworks/AppKit.framework/Versions/C/AppKit";
    const HITOOLBOX: &str = "/System/Library/Frameworks/Carbon.framework/Versions/A/Frameworks/\
                             HIToolbox.framework/Versions/A/HIToolbox";
    const CORETEXT: &str = "/System/Library/Frameworks/CoreText.framework/Versions/A/CoreText";
    const FOUNDATION: &str =
        "/System/Library/Frameworks/Foundation.framework/Versions/C/Foundation";

    /// The gated arms, each at `instructions`.
    fn starts(instructions: f64) -> Vec<Measured> {
        GATED
            .iter()
            .map(|(name, _)| arm(name, instructions, &[], &[]))
            .collect()
    }

    /// A scratch directory under the run's `TMPDIR`, removed on drop.
    fn scratch() -> aterm_tempfile::TempDir {
        aterm_tempfile::tempdir().expect("a scratch directory")
    }

    #[test]
    fn the_status_arm_asks_the_first_session_ls_lists() {
        let listed = "0 0 s-93b14b966d5e7c08438d - alive aterm meta=0\n\
                      0 1 s-0123456789abcdef0123 - alive aterm meta=0\n";
        assert_eq!(sid_of_ls(listed).as_deref(), Some("s-93b14b966d5e7c08438d"));
        assert_eq!(sid_of_ls(""), None);
        assert_eq!(sid_of_ls("0 0\n"), None);
    }

    /// The controls alone: no stop and no `binds` control measured.
    const CONTROLS_ONLY: Bases = Bases {
        none: 15e6,
        all: 32e6,
        cli: 29e6,
        binds: None,
        stops: None,
    };

    #[test]
    fn the_split_takes_the_controls_from_the_start() {
        let split = Split::of(50e6, &CONTROLS_ONLY);
        assert_eq!(split.frameworks, 17e6);
        assert_eq!(split.deferral, 3e6);
        assert_eq!(split.own, 18e6);
        assert_eq!((split.own_by_stop, split.binding), (None, None));
        assert_eq!(split.rows().len(), 4);
        assert!((split.share(split.frameworks) - 34.0).abs() < 1e-9);
        let zero = Bases {
            none: 0.0,
            all: 0.0,
            cli: 0.0,
            ..CONTROLS_ONLY
        };
        assert_eq!(Split::of(0.0, &zero).share(1.0), 0.0);
    }

    /// The stops cut aterm's own start into three parts that add up to it: to `main` over
    /// `all`, the prelude over `main`, the verb over the prelude; the `binds` control prices
    /// the binding inside the first.
    #[test]
    fn the_stops_split_aterms_own_start_into_parts_that_add_up() {
        let bases = Bases {
            binds: Some(41e6),
            stops: Some((44e6, 47e6)),
            ..CONTROLS_ONLY
        };
        let split = Split::of(50e6, &bases);
        assert_eq!(split.own_by_stop, Some([12e6, 3e6, 3e6]));
        assert_eq!(split.binding, Some(9e6));
        let [image, prelude, verb] = split.own_by_stop.expect("stops");
        assert_eq!(image + prelude + verb, split.own);
        let rows = split.rows();
        assert_eq!(rows.len(), 8);
        assert_eq!(rows[4], ("  image and runtime (main - all)", 12e6));
        assert_eq!(rows[5], ("    of it, its imports (binds - all)", 9e6));
        assert_eq!(rows[7], ("  the verb (it - prelude)", 3e6));
    }

    /// What dyld writes first under `DYLD_PRINT_LIBRARIES`, which no stop's refusal is held to.
    const DYLD: &str = "dyld[501]: <4C4C4410-5555-3144-A1B2-C3D4E5F60718> /tmp/aterm\n\
                        dyld[501]: <4C4C4411-5555-3144-A1B2-C3D4E5F60718> /usr/lib/libz.dylib\n";

    /// A stop is placed by its refusal, its code and what it writes, not its code alone: an aterm
    /// without the hold verb exits 2 too, from its usage refusal past the whole front door.
    #[test]
    fn a_stop_is_known_by_its_refusal_not_its_code_alone() {
        let [main, prelude] = STOPS;
        let ends = |stop: Stop, got: i32, stderr: &str| {
            ended_elsewhere(stop.name, stop.exit, Some(stop.says), Some(got), stderr)
        };
        assert_eq!(ends(main, 2, DYLD), None);
        let usage = format!(
            "{DYLD}aterm: unknown command '{}' (try --help)\n",
            main.verb
        );
        let why = ends(main, 2, &usage).expect("a usage refusal is not the hold verb's");
        assert!(why.contains("unknown command"), "{why}");
        assert_eq!(ends(prelude, 1, &format!("{DYLD}{}\n", prelude.says)), None);
        assert!(ends(prelude, 1, DYLD).is_some());
        assert!(ends(prelude, 0, &format!("{}\n", prelude.says)).is_some());
        // An arm with nothing to say is held to its code alone.
        assert_eq!(ended_elsewhere("version", 0, None, Some(0), "x\n"), None);
        assert!(ended_elsewhere("version", 0, None, Some(1), "x\n").is_some());
    }

    /// A thin arm64 Mach-O whose chained fixups import `names` in `format` (1, 2 or 3), laid
    /// out as ld lays one out: the header and one `LC_DYLD_CHAINED_FIXUPS`, then its header,
    /// its import table and its NUL-terminated names.
    fn chained(names: &[&str], format: u32) -> Vec<u8> {
        const ARM64: u32 = 0x0100_000c;
        let le = |out: &mut Vec<u8>, v: u32| out.extend_from_slice(&v.to_le_bytes());
        let mut out = Vec::new();
        for v in [0xfeed_facf, ARM64, 0, 2, 1, 16, 0, 0] {
            le(&mut out, v);
        }
        let data = 48;
        for v in [0x8000_0034, 16, data, 0] {
            le(&mut out, v);
        }
        let stride = [0, 4, 8, 16][format as usize];
        let imports = 28;
        let symbols = imports + stride * names.len() as u32;
        for v in [0, 0, imports, symbols, names.len() as u32, format, 0] {
            le(&mut out, v);
        }
        let mut offset = 1;
        let mut text = vec![0u8];
        for name in names {
            match format {
                1 => le(&mut out, 1 | (offset << 9)),
                2 => {
                    le(&mut out, 1 | (offset << 9));
                    le(&mut out, 0);
                }
                _ => {
                    for v in [1, offset, 0, 0] {
                        le(&mut out, v);
                    }
                }
            }
            text.extend_from_slice(name.as_bytes());
            text.push(0);
            offset += name.len() as u32 + 1;
        }
        out.extend_from_slice(&text);
        out
    }

    /// The import table is read in all three of dyld's formats, from a thin file and from a
    /// universal one's slice for the host's CPU; a file with no chained fixups, or none for that
    /// CPU, is refused by why.
    /// NEGATIVE CONTROL: reading format 1's name offset from bit 8 (`e >> 8`) fails with
    /// `left: Err("import 2 names no symbol")`.
    #[test]
    fn a_mach_os_imports_are_read_from_its_chained_fixups() {
        const ARM64: u32 = 0x0100_000c;
        let names = ["_write", "_OBJC_CLASS_$_NSApplication", "__tlv_bootstrap"];
        for format in [1, 2, 3] {
            assert_eq!(
                chained_imports(&chained(&names, format), ARM64).as_deref(),
                Ok(&names.map(str::to_string)[..]),
                "format {format}"
            );
        }
        let thin = chained(&names, 1);
        let mut fat = Vec::new();
        for v in [
            0xcafe_babe_u32,
            2,
            0x0100_0007,
            3,
            4096,
            0,
            12,
            ARM64,
            0,
            4096,
        ] {
            fat.extend_from_slice(&v.to_be_bytes());
        }
        fat.extend_from_slice(&(thin.len() as u32).to_be_bytes());
        fat.extend_from_slice(&14u32.to_be_bytes());
        fat.resize(4096, 0);
        fat.extend_from_slice(&thin);
        assert_eq!(chained_imports(&fat, ARM64).map(|n| n.len()), Ok(3));
        assert_eq!(
            chained_imports(&fat, 0x0100_0012),
            Err("no slice for cputype 0x1000012".to_string())
        );
        let mut bare = thin;
        bare[32..36].copy_from_slice(&0x19u32.to_le_bytes());
        assert_eq!(
            chained_imports(&bare, ARM64),
            Err("no chained fixups (LC_DYLD_CHAINED_FIXUPS)".to_string())
        );
    }

    fn owned(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|p| (*p).to_string()).collect()
    }

    /// Each dylib aterm's load commands name becomes the flag that links it: a top-level system
    /// framework by name, a `/usr/lib` library by its stem; anything else is refused by name.
    #[test]
    fn a_dylib_is_linked_by_the_flag_its_install_name_gives() {
        assert_eq!(
            link_flag(APPKIT).as_deref(),
            Ok("-Wl,-needed_framework,AppKit")
        );
        assert_eq!(link_flag(OBJC).as_deref(), Ok("-Wl,-needed-lobjc"));
        assert_eq!(link_flag(SYS).as_deref(), Ok("-Wl,-needed-lSystem"));
        assert_eq!(
            link_flag("/usr/lib/libc++.1.dylib").as_deref(),
            Ok("-Wl,-needed-lc++")
        );
        for refused in [
            HITOOLBOX,
            "/usr/lib/swift/libswiftCore.dylib",
            "@rpath/libfoo.dylib",
            "/usr/lib/lib.dylib",
        ] {
            let why = link_flag(refused).expect_err(refused);
            assert!(why.contains(refused), "{why}");
        }
    }

    /// The controls come from aterm's load commands at every run: `all` links every dylib aterm
    /// does and `cli` all but the window-only frameworks, so a framework aterm starts or stops
    /// linking changes both.
    #[test]
    fn the_controls_follow_aterms_load_commands() {
        let aterm = owned(&[APPKIT, FOUNDATION, OBJC, CORETEXT, SYS]);
        assert_eq!(cli_of(&aterm), owned(&[FOUNDATION, OBJC, SYS]));
        assert_eq!(
            link_flags(&aterm).as_deref(),
            Ok(
                "-Wl,-needed_framework,AppKit -Wl,-needed_framework,Foundation -Wl,-needed-lobjc \
                 -Wl,-needed_framework,CoreText -Wl,-needed-lSystem"
            )
        );
        let security = "/System/Library/Frameworks/Security.framework/Versions/A/Security";
        let grown = owned(&[APPKIT, FOUNDATION, OBJC, CORETEXT, SYS, security]);
        assert_eq!(cli_of(&grown), owned(&[FOUNDATION, OBJC, SYS, security]));
        assert!(
            link_flags(&grown)
                .expect("flags")
                .ends_with(" -Wl,-needed_framework,Security")
        );
        assert!(link_flags(&owned(&[APPKIT, HITOOLBOX])).is_err());
    }

    /// A control whose own load commands are not what it was built to load (a flag the linker
    /// dropped, a build without the lane's flags) fails the lane, naming what it misses and adds;
    /// what the bare control loads is not counted against it.
    #[test]
    fn a_control_that_links_other_than_it_was_built_to_is_named() {
        let none = owned(&[SYS]);
        let want = owned(&[APPKIT, OBJC, SYS]);
        assert_eq!(
            mislinked("all", &owned(&[SYS, APPKIT, OBJC]), &want, &none),
            None
        );
        assert_eq!(mislinked("none", &none, &[], &none), None);
        let bare = mislinked("all", &none, &want, &none).expect("a finding");
        assert!(
            bare.contains("`all` control") && bare.contains("misses AppKit, libobjc.A.dylib"),
            "{bare}"
        );
        let more = mislinked("cli", &owned(&[SYS, APPKIT, OBJC, CORETEXT]), &want, &none)
            .expect("a finding");
        assert!(more.contains("misses none and adds CoreText"), "{more}");
    }

    #[test]
    fn dyld_s_printed_images_are_read_without_the_main_executable() {
        let told = "dyld[22789]: <9F0BFFE1-66F1-3089-90BD-8832CDA4568A> /x/target/release/ctl\n\
                    dyld[22789]: <4FDC9AA6-B344-37FE-B8C6-A4C94A038F57> /usr/lib/libSystem.B.dylib\n\
                    a line of the program's own\n\
                    dyld[22789]: /usr/lib/libobjc.A.dylib\n";
        assert_eq!(images_of(told), [SYS, OBJC]);
        assert_eq!(images_of("no dyld here\n"), Vec::<String>::new());
    }

    #[test]
    fn a_median_above_its_ceiling_fails_and_a_missing_key_is_unjudged() {
        let base = aterm_json::from_str::<Value>(&baseline_json(
            &[("version_minstr", 40.0), ("ctl_help_minstr", 40.0)],
            "c",
        ))
        .expect("the baseline parses");
        let green = judge(
            &[("version_minstr", 50.0), ("ctl_help_minstr", 41.0)],
            &base,
        );
        assert_eq!(green.status, Status::Pass);
        let red = judge(&[("version_minstr", 50.1)], &base);
        assert!(red.failed(), "{red:?}");
        let half = judge(
            &[("version_minstr", 41.0), ("ctl_status_minstr", 9.0)],
            &base,
        );
        assert_eq!(half.status, Status::Pass);
        assert_eq!(half.unjudged, ["cli-start/ctl_status_minstr (no baseline)"]);
        let none = judge(&[("ctl_status_minstr", 9.0)], &base);
        assert!(matches!(none.status, Status::NotJudged(_)), "{none:?}");
    }

    /// Only the starts are gated: a stop and the `binds` control are reported, but give no
    /// baseline key and no trend row.
    #[test]
    fn the_gated_medians_are_in_millions_and_only_the_starts() {
        let arms = vec![
            arm("version", 54e6, &[], &[]),
            arm("none", 15e6, &[], &[SYS]),
            arm("ctl-status", 60e6, &[], &[]),
            arm("main", 44e6, &[], &[]),
            arm("prelude", 47e6, &[], &[]),
            arm("binds", 41e6, &[], &[SYS]),
        ];
        assert_eq!(
            gated(&arms),
            vec![("version_minstr", 54.0), ("ctl_status_minstr", 60.0)]
        );
        let dir = scratch();
        let trended: Vec<String> = conclude(&arms, &[], false, true, &dir.path().join("none"))
            .samples
            .into_iter()
            .map(|s| s.metric)
            .collect();
        assert_eq!(trended, ["version_minstr", "ctl_status_minstr"]);
        let text = report(&arms);
        assert!(text.contains("version"), "{text}");
        assert!(
            !text.contains("split"),
            "no split without the controls: {text}"
        );
    }

    #[test]
    fn interrupted_cli_publication_preserves_history() {
        let dir = scratch();
        let path = dir.path().join(BASELINE_FILE);
        let arms = starts(50e6);
        assert_eq!(
            conclude(&arms, &[], true, true, &path).status,
            Status::Recorded(path.clone())
        );
        let old = std::fs::read(&path).unwrap();
        assert!(
            conclude_using(
                &arms,
                &[],
                true,
                true,
                &path,
                super::super::publication::interrupted
            )
            .failed()
        );
        assert_eq!(std::fs::read(&path).unwrap(), old);
    }

    /// A gated arm the run could not measure (the headless instance never came up) is NOT
    /// JUDGED by name and why, so the verdict cannot say every lane was judged; the arms it
    /// measured are still judged and trended, and a recording refuses to write a baseline
    /// without it.
    #[test]
    fn a_gated_arm_not_measured_is_named_not_judged() {
        let dir = scratch();
        let baseline = dir.path().join(BASELINE_FILE);
        let three = [
            ("version_minstr", 50.0),
            ("ctl_help_minstr", 50.0),
            ("ctl_status_minstr", 50.0),
        ];
        let written = baseline_json(&three, "c");
        std::fs::write(&baseline, &written).expect("the baseline is written");
        let measured: Vec<Measured> = starts(50e6)
            .into_iter()
            .filter(|a| a.name != "ctl-status")
            .collect();
        let why = vec![(
            "ctl-status".to_string(),
            "the headless instance: aterm-lab instance said nothing ready".to_string(),
        )];
        let run = conclude(&measured, &why, false, true, &baseline);
        assert_eq!(run.status, Status::Pass, "{run:?}");
        assert_eq!(
            run.unjudged,
            [
                "cli-start/ctl_status_minstr (not measured: the headless instance: aterm-lab \
                 instance said nothing ready)"
            ]
        );
        assert_eq!(run.samples.len(), 2, "{run:?}");
        let (text, _) = super::super::verdict(&[(LANE, &run, 1.0)]);
        assert!(!text.contains("GREEN — every lane"), "{text}");
        assert!(
            text.contains("NOT JUDGED: cli-start/ctl_status_minstr (not measured"),
            "{text}"
        );
        let unexplained = conclude(&measured, &[], false, true, &baseline);
        assert_eq!(
            unexplained.unjudged,
            ["cli-start/ctl_status_minstr (not measured: no launch of it was measured)"]
        );
        let refused = conclude(&measured, &why, true, true, &baseline);
        assert!(refused.failed(), "{refused:?}");
        assert_eq!(
            std::fs::read_to_string(&baseline).expect("the baseline reads"),
            written
        );
        let whole = conclude(&starts(50e6), &[], false, true, &baseline);
        assert!(whole.unjudged.is_empty(), "{whole:?}");
    }

    /// An aterm `--binary` names is judged but never feeds the trend (whose rows are keyed by
    /// this tree), and a recording of it refuses before it writes; this tree's own build
    /// records.
    #[test]
    fn only_this_trees_own_build_feeds_the_trend_or_records() {
        let dir = scratch();
        let baseline = dir.path().join(BASELINE_FILE);
        let own = conclude(&starts(50e6), &[], false, true, &baseline);
        assert_eq!(own.samples.len(), 3, "{own:?}");
        assert!(own.samples.iter().all(|s| s.inverted && s.lane == LANE));
        let named = conclude(&starts(50e6), &[], false, false, &baseline);
        assert!(named.samples.is_empty(), "{named:?}");
        assert_eq!(named.status, own.status);
        let refused = conclude(&starts(50e6), &[], true, false, &baseline);
        match &refused.status {
            Status::Fail(why) => assert!(why[0].contains("REFUSING to record"), "{why:?}"),
            other => panic!("a --binary aterm was recorded: {other:?}"),
        }
        assert!(
            !baseline.exists(),
            "the refusal wrote {}",
            baseline.display()
        );
        let recorded = conclude(&starts(50e6), &[], true, true, &baseline);
        assert_eq!(recorded.status, Status::Recorded(baseline.clone()));
        assert!(baseline.is_file());
    }

    #[test]
    fn the_report_splits_each_start_and_attributes_it_by_the_images() {
        let arms = vec![
            arm(
                "version",
                50e6,
                &[],
                &[SYS, FOUNDATION, CORETEXT, APPKIT, HITOOLBOX, OBJC],
            ),
            arm("none", 15e6, &[], &[SYS]),
            arm(
                "all",
                32e6,
                &[APPKIT, FOUNDATION, CORETEXT],
                &[SYS, APPKIT, HITOOLBOX, FOUNDATION, CORETEXT],
            ),
            arm("cli", 29e6, &[FOUNDATION], &[SYS, FOUNDATION, CORETEXT]),
        ];
        let text = report(&arms);
        let row = |label: &str| {
            text.lines()
                .find(|l| l.trim_start().starts_with(label))
                .unwrap_or_else(|| panic!("no `{label}` row: {text}"))
                .split_whitespace()
                .rev()
                .take(2)
                .collect::<Vec<_>>()
        };
        assert_eq!(row("frameworks (all - none)"), ["34%", "17.00"]);
        assert_eq!(row("window-only deferral (all - cli)"), ["6%", "3.00"]);
        assert_eq!(row("aterm's own (it - all)"), ["36%", "18.00"]);
        assert!(
            text.contains("images at launch: none 1, all 5, cli 3\n"),
            "{text}"
        );
        assert!(
            text.contains("window-only frameworks aterm links: AppKit, CoreText\n"),
            "{text}"
        );
        assert!(
            text.contains("of them, `cli` loads through the dylibs it keeps: CoreText\n"),
            "{text}"
        );
        assert!(
            text.contains("the 2 images `all` loads and `cli` does not: AppKit, HIToolbox\n"),
            "{text}"
        );
        assert!(
            text.contains("version: 6 images; beyond `all`'s: libobjc.A.dylib\n"),
            "{text}"
        );
        assert!(
            !text.contains("image and runtime"),
            "no stops, no rows: {text}"
        );
        let mut stopped = arms;
        stopped.push(arm("main", 44e6, &[], &[]));
        stopped.push(arm("prelude", 47e6, &[], &[]));
        let text = report(&stopped);
        let row = |label: &str| {
            text.lines()
                .find(|l| l.trim_start().starts_with(label))
                .unwrap_or_else(|| panic!("no `{label}` row: {text}"))
                .split_whitespace()
                .rev()
                .take(2)
                .collect::<Vec<_>>()
        };
        assert_eq!(row("image and runtime (main - all)"), ["24%", "12.00"]);
        assert_eq!(row("front door prelude (prelude - main)"), ["6%", "3.00"]);
        assert_eq!(row("the verb (it - prelude)"), ["6%", "3.00"]);
    }

    #[test]
    fn a_summary_is_the_median_and_the_range() {
        let takes = Takes {
            instructions: vec![3.0, 1.0, 2.0],
            cycles: vec![1.0, 1.0, 4.0],
            wall_ms: vec![0.5, 0.7, 0.6],
            user_ms: vec![0.2, 0.1, 0.3],
            system_ms: vec![0.4, 0.4, 0.4],
            resident_mb: vec![9.0, 7.0, 8.0],
        };
        let s = takes.summary().expect("a summary");
        assert_eq!(
            (s.n, s.instructions, s.min, s.max, s.cycles),
            (3, 2.0, 1.0, 3.0, 1.0)
        );
        assert_eq!((s.user_ms, s.system_ms, s.resident_mb), (0.2, 0.4, 8.0));
        assert_eq!(Takes::default().summary(), None);
    }
}
