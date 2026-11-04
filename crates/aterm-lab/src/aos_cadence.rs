// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aos-cadence`: typing latency to the glass while a recording holds each typed key for its
//! program's own echo (ATERM-KI-1025-1), at a person's cadence, for two builds in ABBA order.

use std::path::Path;
use std::time::{Duration, Instant};

use crate::aos_repair::{CONFIG, fields, parse_status};
use crate::instance::{self, Instance, Launch, World, take_flag, take_value};
use crate::stop::{self, Stopped};
use crate::{Fail, Res};

/// The verb's own help (`aos-cadence --help`).
pub const HELP: &str = "\
usage: targo --unverified lab aos-cadence --a PATH --b PATH [--no-attach] [--keep]

OPENS ONE WINDOW AT A TIME, four in all. Each take is a fresh private windowed aterm whose tab runs a program that
reads raw with the tty's echo off and echoes each byte itself (`stty -icanon -echo && exec cat`), the momentum glow
on. The take attaches a recording (`aos attach`), so every typed key is held for the proof of its own echo, then types
the same 158 keys (four lines of words, each ended by Enter) at a person's cadence: `key <c>` on one held control
connection, the gaps drawn from a fixed seed between 100 and 240 ms. It then reads `aos status`, `aos trace` and
`metrics percentiles`, and stops the instance by its pid. The takes run builds A, B, B, A, in that order.

Each take prints one MEASURED line: its build, whether the recording was still attached and the keys it logged, and
n_present, present_p50/p95/p99_ms (PTY output to glass) and n_input, input_p50/p95/p99_ms (key to the next content
present). The figures are measurements, never gates.

  --a PATH     build A, run first and last (an aterm executable; a release build with --features dev-seams times
               the shipped code)
  --b PATH     build B, run second and third
  --no-attach  the control: the same takes with no recording attached, so no key is ever held
  --keep       keep each take's scratch world

exit: 0 every take typed its keys and, attached, kept its recording to the end; 1 a take failed or a step never
happened (a one-minute hang detector); 2 usage; 3 not run
";

/// The tab's program: it reads raw with the tty's echo off and echoes each byte itself, as a
/// line editor does. Output processing stays on, so the Enter it echoes starts a line.
const PROGRAM: &str = "stty -icanon -echo && exec /bin/cat";

/// What a take types: four lines of words, each ended by Enter, all short of the grid's margin
/// (an echo that wraps stops a recording).
pub const TEXT: [&str; 4] = [
    "the quick brown fox jumps over a lazy dog",
    "pack my box with five dozen liquor jugs",
    "how vexingly quick daft zebras jump",
    "sphinx of black quartz judge my vow now",
];

/// The grid: wide enough that no line of [`TEXT`] wraps, tall enough that none scrolls.
const GRID: [&str; 6] = ["--columns", "72", "--lines", "14", "--font-px", "22"];

/// A person's cadence: each gap between two keys is uniform in this range, in ms.
const GAP_MS: (u64, u64) = (100, 240);
/// The cadence's seed: every take types the same gaps.
const SEED: u64 = 0x5EED_CADE_2026_1030;
/// How long the last key's echo and frames get before the take is read.
const SETTLE: Duration = Duration::from_millis(500);
/// A wait for something that must happen: a hang detector, never a budget.
const HANG: Duration = Duration::from_secs(60);

/// The `metrics percentiles` figures a take reports, in order.
pub const FIGURES: [&str; 8] = [
    "n_present",
    "present_p50_ms",
    "present_p95_ms",
    "present_p99_ms",
    "n_input",
    "input_p50_ms",
    "input_p95_ms",
    "input_p99_ms",
];

/// The builds' order: A, B, B, A.
pub const ORDER: [char; 4] = ['A', 'B', 'B', 'A'];

/// The keys a take types, in order: each char of [`TEXT`], and `'\n'` (Enter) after each line.
#[must_use]
pub fn keys() -> Vec<char> {
    TEXT.iter()
        .flat_map(|line| line.chars().chain(std::iter::once('\n')))
        .collect()
}

/// The `n` gaps before each key after the first, from [`SEED`]: the same for every take.
#[must_use]
pub fn gaps(n: usize) -> Vec<Duration> {
    let mut x = SEED | 1;
    let span = GAP_MS.1 - GAP_MS.0 + 1;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            Duration::from_millis(GAP_MS.0 + x % span)
        })
        .collect()
}

/// The `key` verb's body for `c`.
#[must_use]
pub fn key_body(c: char) -> String {
    match c {
        ' ' => "space".to_owned(),
        '\n' => "enter".to_owned(),
        c => c.to_string(),
    }
}

/// What one take read.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Take {
    /// `aos status`'s `attached` after the last key (always false with no recording).
    pub attached: bool,
    /// `aos trace`'s `keys`, attached.
    pub logged: Option<u64>,
    /// [`FIGURES`]' values as `metrics percentiles` printed them (`?` where it printed none).
    pub figures: Vec<String>,
}

/// One take's MEASURED line.
#[must_use]
pub fn measured(take: usize, build: char, attach: bool, typed: usize, t: &Take) -> String {
    let mut line = format!(
        "MEASURED take={take} build={build} attach={attach} typed={typed} attached={} logged={}",
        t.attached,
        t.logged.map_or_else(|| "-".to_owned(), |k| k.to_string())
    );
    for (name, value) in FIGURES.iter().zip(&t.figures) {
        line.push_str(&format!(" {name}={value}"));
    }
    line
}

/// Whether a take is good: with a recording, it was still attached after the last key.
#[must_use]
pub fn take_ok(attach: bool, t: &Take) -> bool {
    !attach || t.attached
}

/// Why a take ended early.
enum Halt {
    Stopped(i32),
    Broke(String),
}

impl From<Stopped> for Halt {
    fn from(s: Stopped) -> Self {
        Self::Stopped(s.0)
    }
}

impl From<String> for Halt {
    fn from(msg: String) -> Self {
        Self::Broke(msg)
    }
}

/// One request on the held connection, its reply line.
fn ask(
    wire: &mut aterm_agent::RelayClient<aterm_uds::CtlStream>,
    request: &str,
) -> Result<String, Halt> {
    wire.request_line(request).map_err(|e| {
        Halt::Broke(format!(
            "`{request}`: the held control connection failed: {e}"
        ))
    })
}

/// Reads `status` until the tab's program is `cat` (the `stty` before it ran): a hang detector.
fn await_cat(wire: &mut aterm_agent::RelayClient<aterm_uds::CtlStream>) -> Result<(), Halt> {
    let start = Instant::now();
    loop {
        let reply = ask(wire, "status")?;
        if fields(&reply).get("program") == Some(&"cat") {
            return Ok(());
        }
        if start.elapsed() >= HANG {
            return Err(Halt::Broke(format!(
                "the tab's program never became cat within {} s: {reply}",
                HANG.as_secs()
            )));
        }
        stop::sleep(Duration::from_millis(100))?;
    }
}

/// The take's drive on a ready instance.
fn drive(
    wire: &mut aterm_agent::RelayClient<aterm_uds::CtlStream>,
    attach: bool,
) -> Result<Take, Halt> {
    await_cat(wire)?;
    if attach {
        let reply = ask(wire, "aos attach")?;
        if !parse_status(&reply).is_ok_and(|s| s.attached) {
            return Err(Halt::Broke(format!("aos attach: {reply}")));
        }
    }
    let keys = keys();
    let gaps = gaps(keys.len());
    let start = Instant::now();
    let mut due = start;
    for (c, gap) in keys.iter().zip(std::iter::once(Duration::ZERO).chain(gaps)) {
        due += gap;
        let now = Instant::now();
        if due > now {
            stop::sleep(due - now)?;
        }
        let request = format!("key {}", key_body(*c));
        let reply = ask(wire, &request)?;
        if !reply.starts_with("OK") {
            return Err(Halt::Broke(format!("{request} refused: {reply}")));
        }
    }
    stop::sleep(SETTLE)?;
    let status = ask(wire, "aos status")?;
    let attached = parse_status(&status)
        .map_err(|e| Halt::Broke(format!("aos status: {e}: {status}")))?
        .attached;
    let logged = if attached {
        let trace = ask(wire, "aos trace")?;
        fields(&trace).get("keys").and_then(|k| k.parse().ok())
    } else {
        None
    };
    let line = ask(wire, "metrics percentiles")?;
    let f = fields(&line);
    let figures = FIGURES
        .iter()
        .map(|k| f.get(k).copied().unwrap_or("?").to_owned())
        .collect();
    Ok(Take {
        attached,
        logged,
        figures,
    })
}

/// One take of `aterm`: a fresh windowed instance, driven, then stopped by its pid.
fn take(aterm: &Path, attach: bool, keep: bool) -> Result<Take, Halt> {
    let top: Vec<String> = CONFIG.iter().map(|l| (*l).to_string()).collect();
    let config = format!(
        "{}{}",
        instance::config_with(&top),
        crate::linux_proof::DECK_OFF
    );
    let mut world = World::new(&config)?;
    if keep {
        world.keep();
    }
    let mut args: Vec<String> = GRID.iter().map(|s| (*s).to_string()).collect();
    args.extend(["-e", "/bin/sh", "-c", PROGRAM].map(str::to_string));
    let mut inst = Instance::start(
        &world,
        Launch {
            aterm: aterm.to_path_buf(),
            headless: false,
            args,
            env: world.env(),
            cwd: world.home.clone(),
            sandbox: None,
            reroute: false,
        },
    )?;
    println!(
        "aos-cadence: OPENED A WINDOW: {} (keeper pid {}, sock {})",
        aterm.display(),
        inst.pid,
        inst.sock.display()
    );
    let result = inst
        .await_socket(instance::BOOT)
        .map_err(|f| match f.stopped {
            Some(sig) => Halt::Stopped(sig),
            None => Halt::Broke(f.msg.unwrap_or_else(|| "the instance did not boot".into())),
        })
        .and_then(|()| {
            let ready = inst.ctl(&["ready"]);
            if ready.code != 0 {
                return Err(Halt::Broke(format!("not ready: {}", ready.first())));
            }
            let _ = inst.ctl(&["await", "idle", "500", "timeout=60000"]);
            let mut wire = inst.connect().map_err(Halt::Broke)?;
            drive(&mut wire, attach)
        });
    inst.stop();
    result
}

/// `aos-cadence`.
///
/// # Errors
/// Usage (2), not run (3), or interrupted.
pub fn cmd_aos_cadence(args: &[String]) -> Res {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{HELP}");
        return Ok(0);
    }
    stop::install();
    let mut a = args.to_vec();
    let build_a = take_value(&mut a, "--a")?;
    let build_b = take_value(&mut a, "--b")?;
    let attach = !take_flag(&mut a, "--no-attach");
    let keep = take_flag(&mut a, "--keep");
    if let Some(extra) = a.first() {
        return Err(Fail::usage(format!(
            "aos-cadence: unexpected '{extra}' (--help)"
        )));
    }
    let (Some(build_a), Some(build_b)) = (build_a, build_b) else {
        return Err(Fail::usage(
            "aos-cadence: --a PATH and --b PATH are both required",
        ));
    };
    let resolve = |p: String| instance::resolve_aterm(Some(p)).map_err(|e| Fail::exit(3, e));
    let (build_a, build_b) = (resolve(build_a)?, resolve(build_b)?);
    let typed = keys().len();
    let mut ok = true;
    for (i, build) in ORDER.iter().enumerate() {
        let aterm = if *build == 'A' { &build_a } else { &build_b };
        match take(aterm, attach, keep) {
            Ok(t) => {
                println!("{}", measured(i + 1, *build, attach, typed, &t));
                ok &= take_ok(attach, &t);
            }
            Err(Halt::Stopped(sig)) => {
                return Err(Fail::die(format!(
                    "aos-cadence: interrupted (signal {sig})"
                )));
            }
            Err(Halt::Broke(why)) => {
                println!("FAIL take={} build={build}: {why}", i + 1);
                ok = false;
            }
        }
    }
    Ok(if ok { 0 } else { 1 })
}

#[cfg(test)]
#[path = "aos_cadence_tests.rs"]
mod tests;
