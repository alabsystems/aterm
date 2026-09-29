// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE DISK, BEFORE ANYTHING IS BUILT.
//!
//! WHY (2026-09-20/21). The gate builds a pinned snapshot of the caller's tree
//! in several target dirs (`target/`, `target-tippy/`, `target-drivers/`,
//! `target-xtask/`, plus the L0 gate's and the libc oracle's own), and nothing
//! bounded what they held. Measured across incremental
//! `--fast` runs: `target/` grew 36 GB -> 55 GB, `target-tippy/` sat at 16-18 GB
//! and `target-drivers/` at 16-20 GB, on a 926 GB volume that also carries
//! `$HOME/trust` (428 GB) and `~/ay` (195 GB). On 2026-09-20 two contract runs
//! died mid-ladder with `No space left on device`, and their logs, in the
//! snapshot's `.aterm-verify/logs`, name the stage: `13a8494eb`'s
//! (`verify-90487.log`) passed its build and could not start its test stage
//! (`aterm-verify: cannot run …/targo: No space left on device (os error
//! 28)`); `cb770c598`'s first run (`verify-45265.log`) died inside its build
//! (`error: failed to write …/target/debug/deps/…/lib.rmeta: No space left on
//! device`) and then the same way. The rows they printed were `FAIL` rows of a
//! FINDING's severity, and neither recorded the free space it started with: no
//! run before this change read the disk. With `target/`, `target-tippy/` and
//! `target-drivers/` deleted and `CARGO_INCREMENTAL=0` in the environment, the
//! next run of `cb770c598` (`verify-53539.log`) built them cold, and the
//! snapshot measured 23 GiB after it (`du -sh`). Incremental artifacts were
//! most of the bloat, and the gate never needed them — it builds each commit
//! once ([`crate::CHILD_ENV`]).
//!
//! WHERE THE NUMBERS BELOW COME FROM. Verdicts, stage times and `Compiling`
//! counts are read off each run's log in the snapshot's `.aterm-verify/logs`
//! and its receipt in the caller's `.aterm-verify/receipts`. A run is COLD
//! when its main lanes were emptied before it — `cb770c598`'s second run and
//! `b994cadd0`'s, with 769 and 809 `Compiling` lines — and WARM otherwise:
//! `0a45a7446`, `fd0be116b` and `216e2e5cb`, with 43-308. Sizes marked `du
//! -sh`, `du -sk` or `df -h` were read by hand in the sessions that ran those
//! runs; no file in the repo or the snapshot records them.
//!
//! WHY NOT A FLAT FLOOR (2026-09-23). Until then the preflight refused any run
//! with less than 40 GiB free, whatever the lanes already held. That is about
//! a run from EMPTY lanes plus a cold footprint in reserve, and it was charged
//! to warm runs too: the cold run of `b994cadd0` left its lanes at 20.2 GiB
//! (`du -sk`), the volume read 22.2 GiB free five minutes before that run
//! ended, and the flat floor refuses a run at that reading which the estimate
//! — a warm run's growth plus the reserve — puts at 22.0 GiB.
//!
//! WHAT THIS DOES. Before the ladder is planned, [`crate::run`] budgets what
//! THIS run will write and REFUSES — COULD NOT RUN, exit 3, never a skip and
//! never a finding — when the volume holding the run's root has less free. It
//! reads two numbers: the free space (`df -Pk`, [`read_free`]) and what the
//! run's lanes already hold (`du -sk` over [`lane_dirs`], [`measure_lanes`]).
//! The requirement is [`Budget::need`]:
//!
//! ```text
//! need = max(cold - lanes credited, warm growth) + reserve
//! ```
//!
//! A build writes its cold footprint less what warm lanes already hold, a warm
//! build still grows its lanes ([`WARM_GROWTH_BYTES`]), and the reserve is for
//! everything the estimate does not count ([`RESERVE_BYTES`]). Only a
//! snapshot's lanes are credited ([`Owner`]): they are the gate's alone —
//! synced, stamped and locked by it. A snapshot whose lanes hold more than
//! [`LANE_CAP_BYTES`] has them REMOVED before the free space is read
//! ([`crate::snapshot::remove_lanes`]) and is budgeted cold. The `verify: disk
//! …` line prints the free space, the lanes and the requirement with its terms,
//! so every ladder carries arithmetic a reader can check; a refusal adds the
//! regenerable dirs, each sized, and what removing them would buy. A run
//! refused here leaves no receipt, so the last real judgement of the commit
//! stands. `--disk-floor <GiB>` ([`Plan::floor`]) replaces the estimate with
//! exactly that requirement.
//!
//! THE CELLS LANE (2026-09-27). `gate cells-foreign` (a stage of every tier
//! since 2026-09-25) type-checks five foreign cells into target dirs OUTSIDE
//! the root — `xtask` refuses a cells cache inside the workspace it judges — and
//! by default into a per-checkout cache under `~/.cache/aterm/cells` that this
//! preflight neither saw nor bounded: a cold contract run could pass it and
//! then fill the volume. So a snapshot's run points that stage at ITS OWN
//! cells lane, [`cells_lane`]: `<root>-cells.noindex` beside the snapshot, on
//! its volume. [`measure_lanes`] measures it with the target lanes, the cap
//! counts it and [`crate::snapshot::remove_lanes`] removes it with them; it
//! carries no compiler stamp, because rustup's `stable` builds it, not the
//! pinned trustc. Its cold footprint and a version bump's growth are in
//! [`COLD_BYTES`] and [`WARM_GROWTH_BYTES`] ([`CELLS_COLD_BYTES`],
//! [`CELLS_WARM_GROWTH_BYTES`]).
//!
//! WHAT NO PREFLIGHT CAN BUDGET is another writer. The estimate is of THIS
//! run's own writes, and the volume is shared: other sessions' builds, their
//! scratch under `/tmp`, anything. During the warm run of `216e2e5cb` on
//! 2026-09-23 the volume went from 43.5 GiB free (its `verify: disk` line) to
//! 11 GiB 22 minutes in (`df -h`). Its lanes had grown by about 14 GiB by then
//! — the whole snapshot read 37 GiB the day before, with no run between, and
//! its lanes summed 50 GiB at that moment (`du -sh`) — so about 18 GiB of the
//! drop was not its lanes, and WHO wrote it is not recorded. Two candidates are
//! on the record without either being sized against it: four other build dirs
//! on the volume (`~/aterm-h-target-{a..d}.noindex`) held 9.3 GiB then and
//! 31 GiB 77 minutes later (`du -sh`), and this run's own test fixtures, of
//! which only this crate's were measured ([`RESERVE_BYTES`]) — the test stage
//! runs `targo test --workspace`, and the other crates' fixtures were not.
//! Free space even ROSE from 11 to 20 GiB between 14:26:54 and 14:29:31,
//! inside that run's test stage, and nothing records what released it. No
//! reserve that still admits a warm run could cover an unbounded other
//! writer, and the preflight reads once, at the start. What
//! keeps that case honest is downstream: a child that dies of ENOSPC is a
//! COULD NOT RUN row ([`crate::ladder::Report::fail_child`]), so such a run
//! ends COULD NOT RUN, never with a finding about the tree.
//!
//! The free-space read is `df -Pk` and the lanes' is `du -sk`, both POSIX,
//! because this crate has no dependencies on purpose (see its `Cargo.toml`) and
//! std has no `statvfs`. `du` counts allocated blocks and a hard-linked file
//! once, which is nearer than a sum of file lengths to what removing the dirs
//! gives back to `df`. Everything that decides is a pure function of numbers,
//! tested below on synthetic ones.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// One gibibyte.
pub const GIB: u64 = 1 << 30;

/// What a run from EMPTY lanes writes into them: 24 GiB.
///
/// MEASURED after the two cold runs since `CARGO_INCREMENTAL=0` landed on
/// 2026-09-20 — the only runs with size readings (older logs exist; they have
/// none). `cb770c598`'s on 2026-09-20, with
/// `target/`, `target-tippy/` and `target-drivers/` emptied before it and the
/// other lanes kept, left the whole snapshot at 23 GiB (`du -sh`, 7 s after
/// its log's last line). `b994cadd0`'s on 2026-09-23, with every lane emptied
/// before it, left the lanes at 21,220,340 KiB — 20.2 GiB (`du -sk`) — and
/// the whole snapshot at 21 GiB (`du -sh`). 24 GiB is above both, for a
/// workspace that only grows.
///
/// PLUS THE CELLS LANE, 3 GiB ([`CELLS_COLD_BYTES`]) since 2026-09-27: 27 GiB.
pub const COLD_BYTES: u64 = 24 * GIB + CELLS_COLD_BYTES;

/// What `gate cells-foreign` writes into an EMPTY [`cells_lane`]: 3 GiB.
///
/// MEASURED 2026-09-27 on this machine (M5 Max, load average 40-65 from other
/// sessions), `gate cells-foreign` at `b4b147abc` into an empty
/// `$ATERM_CELL_TARGET_DIR` with `CARGO_INCREMENTAL=0`, the environment every
/// gate child has ([`crate::CHILD_ENV`]): 2,751,212 KiB (`du -sk`) — 2.6 GiB,
/// over the five cells linux-arm 0.90, win 0.66, win-arm 0.66, wasm-gpu 0.24
/// and wasm-cpu 0.16 GiB — in 432 s. The same run with incremental ON, the
/// default of a hand-run `gate cells-foreign`, wrote 12,650,048 KiB (12.1
/// GiB) in 394 s: three quarters of it `incremental/`, which is why the
/// per-checkout caches under `~/.cache/aterm/cells` read 12-46 GB and the
/// gate's lane reads 2.6. 3 GiB is above the measurement.
pub const CELLS_COLD_BYTES: u64 = 3 * GIB;

/// What a warm run adds to the [`cells_lane`] when every first-party crate
/// rebuilds: 2 GiB.
///
/// Cargo keeps the old artifacts beside the new, so a workspace version bump
/// (the growth [`WARM_GROWTH_BYTES`] budgets) adds a second copy of the
/// first-party ones. MEASURED 2026-09-27 on the lane of [`CELLS_COLD_BYTES`]:
/// the files under first-party `<name>-<hash>` artifact, build and fingerprint
/// entries hold 1.00 GiB of its 2.62. A new `stable` rustc rehashes every unit
/// and adds a whole cold footprint, which this does not cover, as the target
/// lanes' budget does not cover a third-party rebuild. 2 GiB is above the
/// measurement, which is a projection from one lane, not a measured bump.
pub const CELLS_WARM_GROWTH_BYTES: u64 = 2 * GIB;

/// What the Trust verification lane writes while it runs, and removes: 6 GiB,
/// IN FLIGHT ([`Budget::in_flight`]).
///
/// `targo trust` gives every unit of a run a unique compiler-session flag and
/// accepts no warm cache as evidence, so nothing it builds is reused: each run
/// re-checks each selected library's whole dependency graph into scratch slots
/// under `target-trust/`, which `tools/trust-gate-all.sh` empties before the run
/// and removes after it. So the lane holds nothing at rest: its bytes are a term
/// of what a run NEEDS free, never of what the held lanes may grow to — counted
/// in the cold footprint and the warm growth (as it was for a few hours on
/// 2026-09-27), it raised [`LANE_CAP_BYTES`] by 12 GiB for lanes that hold none
/// of it (review, the same day). MEASURED 2026-09-27: one slot peaked at 1.7 GiB
/// (`du -sh`) checking aterm-gui's whole graph, and the script runs at most
/// three slots on this machine (`--jobs auto`, one per six cores): 5.1 GiB,
/// read up to 6.
pub const TRUST_LANE_BYTES: u64 = 6 * GIB;

/// What a run on WARM lanes still adds to them: 16 GiB.
///
/// Warm lanes grow, because cargo deletes no artifact a later build stops
/// using. MEASURED over the three warm runs since `CARGO_INCREMENTAL=0` landed
/// on 2026-09-20 (the only runs with size readings), as the snapshot's size
/// before and after each (`du -sh`, no other run between the two readings):
/// `0a45a7446` (2026-09-20, one commit past the lanes' last build) left it at
/// 23 GiB, as it found it; `fd0be116b` (2026-09-21, 55 commits past) took it
/// from 23 to 37 GiB; `216e2e5cb` (2026-09-23, 292 commits past) from 37 GiB
/// to lanes summing 49.4 GiB 11 minutes after it ended. 16 GiB is above the
/// largest, 14 GiB, read to the whole GiB. It is the floor of the estimate, so
/// lanes that already hold a whole cold footprint still budget a run's growth.
///
/// Both large ones crossed a workspace version bump (0.89.0 -> 0.90.0,
/// 0.90.0 -> 0.91.0), which rebuilds every first-party crate, so they are what
/// a warm run adds when all of the workspace's own code rebuilds. A warm run
/// that rebuilds the third-party crates as well can add up to a cold
/// footprint, and this budget does not cover it: such a run that fills the
/// volume ends COULD NOT RUN.
///
/// PLUS THE CELLS LANE's, 2 GiB ([`CELLS_WARM_GROWTH_BYTES`]) since
/// 2026-09-27: 18 GiB.
pub const WARM_GROWTH_BYTES: u64 = 16 * GIB + CELLS_WARM_GROWTH_BYTES;

/// Room kept free beyond what the run writes into its lanes: 6 GiB.
///
/// A MARGIN, NOT A MEASUREMENT, for this run's own writes outside its lanes
/// and for the error in the two estimates above, which are read to the whole
/// GiB. (The foreign cells' target dirs are not among those writes since
/// 2026-09-27: they are the [`cells_lane`], counted with the lanes.) Of those
/// writes, measured: its ladder log (2.4-3.1 MB for each
/// complete run in the snapshot's `.aterm-verify/logs`), its receipt (under 1
/// KiB), and this crate's own test fixtures under `/tmp` and `$TMPDIR`, which
/// peaked at 3.8 MiB and left 3.3 MiB behind (`du -sk`, sampled 420 times
/// through one `targo --unverified test -p aterm-verify` here, 2026-09-23). Not
/// measured: the other crates' test fixtures, which write there too. It is
/// NOT room for other writers on the volume, which no preflight can budget
/// (the module doc says what happens instead).
pub const RESERVE_BYTES: u64 = 6 * GIB;

/// The most a snapshot's lanes may hold when a run starts: 45 GiB,
/// [`COLD_BYTES`] plus [`WARM_GROWTH_BYTES`] — a cold footprint and one warm
/// run's growth (40 GiB until the cells lane joined them on 2026-09-27). The
/// trust lane's in-flight bytes are not in it: that lane holds none at rest.
///
/// Nothing else bounds them: a warm run grows the lanes, and only a new
/// compiler empties them ([`crate::snapshot`]'s prune). Over the cap they are
/// removed before the run, which is budgeted cold. At the growth measured
/// above that makes every third run cold: a cold run leaves about 21 GiB, the
/// next adds about 14 to 35, the next about 13 to 48, which is over the cap.
///
/// The cost is that run being cold. Each run's wall time, from its log's
/// creation to its receipt: cold, 38 min 12 s (`cb770c598`, FAIL) and 35 min
/// 19 s (`b994cadd0`, PASS); warm, 32 min 3 s (`0a45a7446`, PASS), 37 min
/// 31 s (`fd0be116b`, PASS) and 38 min 16 s (`216e2e5cb`, FAIL). Five runs on
/// a machine other sessions share do not separate the two. The build stage of
/// those runs alone took 137.0 s and 215.5 s cold, and 39.0 s, 108.7 s and
/// 246.6 s warm.
pub const LANE_CAP_BYTES: u64 = COLD_BYTES + WARM_GROWTH_BYTES;

/// How long `du -sk` may run before the lanes count as unmeasured: 60 s.
///
/// MEASURED 2026-09-23 on this machine: 0.55 s over the snapshot's lanes
/// (20.2 GiB in 38,427 entries); 1.95 s over a 21.9 GiB target dir of 109,419
/// entries read for the first time; 2.60 s over five target dirs holding 59.6
/// GiB in 302,032 entries, re-read. `du` walks entries, not bytes. A
/// measurement that misses this deadline — or fails — credits nothing, and the
/// run is budgeted cold: the direction that refuses more, never less.
pub const DU_DEADLINE: Duration = Duration::from_secs(60);

/// The lane target dirs that do not sit at the root as `target` or `target-*`.
///
/// THE ONE LIST. [`lane_dirs`] reads it, and [`crate::snapshot`] stamps and
/// removes what that returns rather than keeping its own copy: the two were
/// separate until 2026-09-21, and
/// `libc-oracle/target-symgate` was in one and not the other, so a refused
/// operator was told to delete "every one of them" and left a stamped,
/// regenerable lane behind.
pub const NESTED_LANE_DIRS: [&str; 3] = [
    "tools/freeze-safety-gate/target",
    "libc-oracle/target",
    "libc-oracle/target-symgate",
];

/// The preflight's numbers: the constants above in a real run, scaled down to
/// bytes by the gate's own tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budget {
    /// What a run from empty lanes writes ([`COLD_BYTES`]).
    pub cold: u64,
    /// What a run on warm lanes still adds ([`WARM_GROWTH_BYTES`]).
    pub warm_growth: u64,
    /// What a run writes and removes again before it ends — the trust lane's
    /// scratch slots ([`TRUST_LANE_BYTES`]): needed free, never held.
    pub in_flight: u64,
    /// Room kept free beyond both ([`RESERVE_BYTES`]).
    pub reserve: u64,
    /// Lanes over this are removed before the run ([`LANE_CAP_BYTES`]).
    pub lane_cap: u64,
}

impl Budget {
    /// The measured budget every real run takes.
    pub const MEASURED: Self = Self {
        cold: COLD_BYTES,
        warm_growth: WARM_GROWTH_BYTES,
        in_flight: TRUST_LANE_BYTES,
        reserve: RESERVE_BYTES,
        lane_cap: LANE_CAP_BYTES,
    };

    /// `max(cold - credited, warm_growth) + in_flight + reserve`: the free space a run
    /// whose lanes already hold `credited` bytes needs. Saturating, so no
    /// synthetic number can wrap it into a small one.
    #[must_use]
    pub fn need(&self, credited: u64) -> u64 {
        self.cold
            .saturating_sub(credited)
            .max(self.warm_growth)
            .saturating_add(self.in_flight)
            .saturating_add(self.reserve)
    }
}

/// What the preflight read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reading {
    /// Bytes an unprivileged writer may still take on the volume.
    Free(u64),
    /// The volume could not be measured, and why.
    Unknown(String),
}

/// Read the free space on the volume holding `path`.
#[must_use]
pub fn read_free(path: &Path) -> Reading {
    let out = match Command::new("df").arg("-Pk").arg(path).output() {
        Ok(o) => o,
        Err(e) => return Reading::Unknown(format!("cannot run df -Pk: {e}")),
    };
    if !out.status.success() {
        return Reading::Unknown(format!(
            "df -Pk {} failed: {}",
            path.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    parse_df(&text).map_or_else(
        || {
            Reading::Unknown(format!(
                "df -Pk printed no Available column: {:?}",
                text.trim()
            ))
        },
        Reading::Free,
    )
}

/// The `Available` column of `df -Pk` output, in bytes.
///
/// ANCHOR ON `Capacity`, NEVER ON A FIELD COUNT. `-P` fixes the column ORDER —
/// `Filesystem 1024-blocks Used Available Capacity Mounted on` — and nothing
/// else. It does NOT promise six whitespace-separated fields: the first column
/// may hold spaces and so may the last. This is not hypothetical on the machine
/// the gate runs on. `df -Pk /System/Volumes/Data/home` prints
///
/// ```text
/// Filesystem    1024-blocks Used Available Capacity  Mounted on
/// map auto_home           0    0         0   100%    /System/Volumes/Data/home
/// ```
///
/// — seven fields, because macOS autofs names the source `map auto_home`. Read
/// by position, the fourth field is `Used`, not `Available`, and on a volume
/// with real numbers that returns a plausible WRONG answer: the gate would be
/// told it had the used bytes free and would start building on a full disk,
/// which is the one outcome this module exists to prevent. A number that is
/// merely absent is safe here — [`decide`] refuses on [`Reading::Unknown`] —
/// so an unreadable row must yield `None`, never a guess.
///
/// `Capacity` is the one field that is a percentage, and `Available` is the
/// field before it. An NFS or sshfs export, an autofs map, and a mount point
/// with spaces all parse correctly that way.
#[must_use]
pub fn parse_df(text: &str) -> Option<u64> {
    let row = text.lines().nth(1)?;
    let fields: Vec<&str> = row.split_whitespace().collect();
    let capacity = fields.iter().position(|f| is_percentage(f))?;
    let kib: u64 = fields.get(capacity.checked_sub(1)?)?.parse().ok()?;
    kib.checked_mul(1024)
}

/// A `df` `Capacity` cell: digits then `%`, and nothing else. Spelled out so a
/// filesystem or mount name that merely CONTAINS a `%` cannot be mistaken for
/// the column the reading is anchored to.
fn is_percentage(field: &str) -> bool {
    field
        .strip_suffix('%')
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// What the run's lanes held when the preflight measured them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Lanes {
    /// Each of [`lane_dirs`], root-relative, with the bytes `du -sk` gave it.
    Measured(Vec<(PathBuf, u64)>),
    /// Why they were not measured.
    Unknown(String),
}

impl Lanes {
    /// Their total, when they were measured.
    #[must_use]
    pub fn total(&self) -> Option<u64> {
        match self {
            Lanes::Measured(dirs) => Some(dirs.iter().fold(0, |t, (_, b)| t.saturating_add(*b))),
            Lanes::Unknown(_) => None,
        }
    }
}

/// `du -sk` over the run's lanes ([`lane_dirs`], root-relative, then the
/// [`cells_lane`] where it exists, absolute), within [`DU_DEADLINE`].
#[must_use]
pub fn measure_lanes(root: &Path) -> Lanes {
    let mut dirs = lane_dirs(root);
    dirs.extend(existing_cells_lane(root));
    if dirs.is_empty() {
        return Lanes::Measured(Vec::new());
    }
    let mut du = Command::new("du");
    du.arg("-sk").arg("--").args(&dirs).current_dir(root);
    let out = match output_within(du, DU_DEADLINE) {
        Ok(out) => out,
        Err(why) => return Lanes::Unknown(format!("du -sk {why}")),
    };
    if !out.status.success() {
        return Lanes::Unknown(format!(
            "du -sk failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
                .lines()
                .next()
                .unwrap_or("")
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    match parse_du(&text, dirs.len()) {
        Some(kib) => Lanes::Measured(
            dirs.into_iter()
                .zip(kib.into_iter().map(|k| k.saturating_mul(1024)))
                .collect(),
        ),
        None => Lanes::Unknown(format!(
            "du -sk printed no size for each of {} dirs: {:?}",
            dirs.len(),
            text.trim()
        )),
    }
}

/// The sizes `du -sk` printed, in KiB, one line per operand in operand order:
/// the size, blanks, the path. `None` unless there is exactly one line per
/// lane and each starts with a number — a path with a newline in it, or a line
/// this cannot read, is a measurement it does not have, never a guess.
#[must_use]
pub fn parse_du(text: &str, lanes: usize) -> Option<Vec<u64>> {
    let sizes: Vec<u64> = text
        .lines()
        .map(|l| l.split_whitespace().next()?.parse().ok())
        .collect::<Option<_>>()?;
    (sizes.len() == lanes).then_some(sizes)
}

/// `cmd`'s output, or why there is none: it could not be run, or it was still
/// running at `deadline` and was killed and reaped. Both pipes are drained on
/// threads, so a child with a lot to say cannot block on a full pipe and run
/// out the clock; a killed child's drains are left to end when its pipes
/// close, never waited for. The wait polls like [`crate::exec`]'s stage
/// children do: std has no wait with a deadline, and this crate has no `libc`.
pub(crate) fn output_within(mut cmd: Command, deadline: Duration) -> Result<Output, String> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not be run: {e}"))?;
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let started = Instant::now();
    let mut nap = Duration::from_millis(1);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {}
            Err(e) => break Err(format!("could not be waited for: {e}")),
        }
        let Some(left) = deadline.checked_sub(started.elapsed()) else {
            break Err(format!(
                "did not finish within {:.1} s",
                deadline.as_secs_f64()
            ));
        };
        std::thread::sleep(nap.min(left));
        nap = (nap * 2).min(Duration::from_millis(25));
    };
    match status {
        Ok(status) => Ok(Output {
            status,
            stdout: stdout.join().unwrap_or_default(),
            stderr: stderr.join().unwrap_or_default(),
        }),
        Err(why) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(why)
        }
    }
}

fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut p) = pipe {
            let _ = p.read_to_end(&mut bytes);
        }
        bytes
    })
}

/// Whose lanes the run's are. Since 2026-09-27 every run's are a snapshot's
/// (the in-place mode, whose lanes were the caller's own caches and neither
/// credited nor removed, is gone).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    /// A snapshot's: the gate's alone — synced, stamped and locked by it, so
    /// what they hold is what earlier gate runs built there for this one to
    /// reuse, and the cap may remove them.
    Snapshot,
}

/// The preflight's decision before any byte moves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    /// The numbers the estimate was made with.
    pub budget: Budget,
    /// `--disk-floor`: the requirement, with the estimate skipped.
    pub floor: Option<u64>,
    /// What the lanes held, or why that is unknown.
    pub held: Result<u64, String>,
    /// Whose lanes they are.
    pub owner: Owner,
    /// Remove the lanes before the free space is read: a snapshot's, measured
    /// over the cap. Decided with or without `--disk-floor`, which moves the
    /// requirement and not the bound on the lanes.
    pub remove: bool,
    /// One sentence per lane the removal could not remove, filled in by the
    /// caller that acted on [`Plan::remove`].
    pub unremoved: Vec<String>,
    /// The bytes credited against [`Budget::cold`].
    pub credited: u64,
    /// The free space this run requires.
    pub need: u64,
}

/// The plan, pure over what was measured: the lanes are credited only when a
/// snapshot's were measured and stay, and the requirement is the floor when
/// one was given and [`Budget::need`] of the credit otherwise.
#[must_use]
pub fn plan(budget: Budget, floor: Option<u64>, lanes: &Lanes, owner: Owner) -> Plan {
    let held = lanes.total().ok_or_else(|| match lanes {
        Lanes::Unknown(why) => why.clone(),
        Lanes::Measured(_) => String::new(),
    });
    let remove = owner == Owner::Snapshot && held.as_ref().is_ok_and(|h| *h > budget.lane_cap);
    let credited = match (&held, owner) {
        (Ok(h), Owner::Snapshot) if !remove => *h,
        _ => 0,
    };
    Plan {
        budget,
        floor,
        held,
        owner,
        remove,
        unremoved: Vec::new(),
        credited,
        need: floor.unwrap_or_else(|| budget.need(credited)),
    }
}

impl Plan {
    /// What the lanes held, and what became of them.
    fn lanes_clause(&self) -> String {
        match (&self.held, self.owner) {
            (Err(why), _) => format!("build dirs unmeasured ({why}), so none credited"),
            (Ok(h), Owner::Snapshot) if self.remove && self.unremoved.is_empty() => format!(
                "build dirs {}, over the {} cap, so removed before the free space was read",
                gib(*h),
                gib(self.budget.lane_cap)
            ),
            (Ok(h), Owner::Snapshot) if self.remove => format!(
                "build dirs {}, over the {} cap; removing them failed as the `verify: build \
                 dirs` line(s) above say, and none are credited",
                gib(*h),
                gib(self.budget.lane_cap)
            ),
            (Ok(h), Owner::Snapshot) => format!("build dirs {}", gib(*h)),
        }
    }

    /// The requirement, with the terms that produced it.
    fn need_clause(&self) -> String {
        match self.floor {
            Some(f) => format!("need {} (--disk-floor: exactly this, no estimate)", gib(f)),
            None => format!(
                "need {} = max({} cold - {} credited, {} warm growth) + {} in flight + {} reserve",
                gib(self.need),
                gib(self.budget.cold),
                gib(self.credited),
                gib(self.budget.warm_growth),
                gib(self.budget.in_flight),
                gib(self.budget.reserve)
            ),
        }
    }

    /// The requirement as the refusal names it.
    fn need_noun(&self) -> String {
        match self.floor {
            Some(f) => format!("{} --disk-floor", gib(f)),
            None => format!("{} this run needs", gib(self.need)),
        }
    }
}

/// The preflight's decision, pure over the reading and the plan: `Ok` to
/// proceed, or the sentence of the COULD-NOT-RUN row. A volume that cannot be
/// measured refuses too — a gate that cannot tell whether it can finish does
/// not start, for the same reason a reader that cannot judge does not admit.
///
/// # Errors
/// The ladder row's label, naming the free amount and the requirement (or why
/// the volume could not be read). The terms of the requirement are on the
/// `verify: disk …` line above it ([`header_line`]).
pub fn decide(reading: &Reading, plan: &Plan, root: &Path) -> Result<(), String> {
    match reading {
        Reading::Free(free) if *free >= plan.need => Ok(()),
        Reading::Free(free) => Err(format!(
            "disk: {} free on the volume holding {}, under the {} — nothing was built",
            gib(*free),
            root.display(),
            plan.need_noun()
        )),
        Reading::Unknown(why) => Err(format!(
            "disk: the free space on the volume holding {} could not be read ({why}), so the {} \
             could not be checked — nothing was built",
            root.display(),
            plan.need_noun()
        )),
    }
}

/// The `verify: disk …` header line: the free space, the lanes and the
/// requirement with its terms, so the record of every run says how much room
/// it started with and a reader can check the sum.
#[must_use]
pub fn header_line(reading: &Reading, plan: &Plan, root: &Path) -> String {
    let free = match reading {
        Reading::Free(free) => format!(
            "{} free on the volume holding {}",
            gib(*free),
            root.display()
        ),
        Reading::Unknown(why) => format!(
            "free space on the volume holding {} could not be read ({why})",
            root.display()
        ),
    };
    format!(
        "verify: disk {free}; {}; {}\n",
        plan.lanes_clause(),
        plan.need_clause()
    )
}

/// The caller tree's incremental caches, newest-first by size: `target*/…/incremental`
/// directories OUTSIDE this run's root.
///
/// They are the cheapest bytes on the volume to give back, and the reason is
/// structural rather than a guess: every child of this gate compiles with
/// `CARGO_INCREMENTAL=0` ([`crate::CHILD_ENV`]), so nothing in a gate run ever
/// reads them — only the caller's own `targo build` does, and it rebuilds them
/// on demand. MEASURED 2026-09-21 on this machine: the caller tree held 47 GB
/// of which 26 GB was `target.noindex/debug/incremental`; removing it returned
/// 19 GB and cost one warm dev rebuild, while removing the run's own lanes
/// would have returned 7.7 GB and made the next contract run cold. That
/// asymmetry is why [`remedy`] names these first.
#[must_use]
pub fn caller_incremental_dirs(caller: &Path) -> Vec<(PathBuf, u64)> {
    let mut out: Vec<(PathBuf, u64)> = Vec::new();
    let Ok(entries) = std::fs::read_dir(caller) else {
        return out;
    };
    for target in entries.flatten() {
        if !target.file_name().to_string_lossy().starts_with("target") {
            continue;
        }
        if !target.path().symlink_metadata().is_ok_and(|m| m.is_dir()) {
            continue;
        }
        // `target*/<profile>/incremental`, one level down: debug, release, and
        // any custom profile. Deeper is not a cargo layout.
        let Ok(profiles) = std::fs::read_dir(target.path()) else {
            continue;
        };
        for profile in profiles.flatten() {
            let dir = profile.path().join("incremental");
            if dir.symlink_metadata().is_ok_and(|m| m.is_dir()) {
                let bytes = dir_bytes(&dir);
                if bytes > 0 {
                    out.push((dir, bytes));
                }
            }
        }
    }
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out
}

/// The remedy block under the refusal: the caller's free bytes first, then
/// what the run's own lanes hold, each named, with what removing them would
/// buy against the requirement — every one of them is regenerable.
#[must_use]
pub fn remedy(
    root: &Path,
    plan: &Plan,
    lanes: &Lanes,
    reading: &Reading,
    caller: Option<&Path>,
) -> String {
    let mut s = String::new();
    // The free bytes first. A human under the requirement reaches for whatever
    // the message names, and until 2026-09-21 the only thing it named was this
    // run's own warm lanes. The caller's incremental caches cost nothing to
    // lose and are usually larger.
    let free_first: Vec<(PathBuf, u64)> = caller
        .filter(|c| *c != root)
        .map(caller_incremental_dirs)
        .unwrap_or_default();
    if !free_first.is_empty() {
        let total: u64 = free_first.iter().map(|(_, b)| *b).sum();
        s.push_str(&format!(
            "  FIRST, and it costs nothing: the calling tree holds {} of incremental caches \
             that no gate run reads (every child compiles with CARGO_INCREMENTAL=0). Removing \
             them loses one warm dev rebuild, not a contract run:\n",
            gib(total)
        ));
        for (d, b) in &free_first {
            s.push_str(&format!("      {:>10}  {}\n", gib(*b), d.display()));
        }
        s.push_str("  Only if that is not enough:\n");
    }
    s.push_str(&lanes_remedy(root, plan, lanes, reading));
    s.push_str(&match plan.floor {
        Some(_) => "  or free space elsewhere on the volume, or lower --disk-floor: a run that \
                    does run out of space mid-ladder is COULD NOT RUN, never a verdict about the \
                    tree."
            .to_string(),
        None => format!(
            "  or free space elsewhere on the volume. The requirement is what this run writes \
             plus a reserve: a cold footprint less what its build dirs already hold, but never \
             less than a warm run still adds to them (cargo deletes no artifact a later build \
             stops using), plus the {} its trust lane writes and removes again, and a {} reserve \
             for its own writes outside them. What other writers put on the volume while it \
             runs is not in it: no preflight can budget that, and a run that runs out of space \
             ends COULD NOT RUN. --disk-floor <GiB> replaces the estimate for one run.",
            gib(plan.budget.in_flight),
            gib(plan.budget.reserve)
        ),
    });
    s
}

/// The lanes' part of [`remedy`].
fn lanes_remedy(root: &Path, plan: &Plan, lanes: &Lanes, reading: &Reading) -> String {
    let mut s = String::new();
    let sized = match lanes {
        Lanes::Measured(sized) => sized,
        Lanes::Unknown(why) => {
            s.push_str(&format!(
                "  this run's build dirs could not be sized ({why}); every one of them is \
                 regenerable:\n"
            ));
            for d in lane_dirs(root) {
                s.push_str(&format!("      {}/\n", d.display()));
            }
            return s;
        }
    };
    if sized.is_empty() {
        s.push_str(&match plan.floor {
            Some(_) => format!(
                "  this run's root {} holds no build dirs yet, so it has none of its own to \
                 remove.\n",
                root.display()
            ),
            None => format!(
                "  this run's root {} holds no build dirs yet, so the run is budgeted cold: it \
                 writes its whole footprint, and the volume does not have room for that plus \
                 the reserve.\n",
                root.display()
            ),
        });
        return s;
    }
    let held = lanes.total().unwrap_or(0);
    let cold = plan.budget.need(0);
    s.push_str(&format!(
        "  this run's build dirs hold {}, all of it regenerable",
        gib(held)
    ));
    match (plan.owner, plan.floor) {
        _ if plan.remove => s.push_str(&format!(
            " — what is left after the removal of build dirs over the {} cap",
            gib(plan.budget.lane_cap)
        )),
        (Owner::Snapshot, None) => {
            s.push_str(&format!(
                ". Removing them gives that back, and the next run is then cold and needs {} \
                 ({} cold + {} in flight + {} reserve)",
                gib(cold),
                gib(plan.budget.cold),
                gib(plan.budget.in_flight),
                gib(plan.budget.reserve)
            ));
            if let Reading::Free(free) = reading {
                let after = free.saturating_add(held);
                if after >= cold {
                    s.push_str(&format!(": {} would be free — enough", gib(after)));
                } else {
                    s.push_str(&format!(
                        ": {} would be free — still {} short",
                        gib(after),
                        gib(cold - after)
                    ));
                }
            }
        }
        (_, Some(_)) => s.push_str("; removing them gives that back"),
    }
    s.push_str(":\n");
    for (d, b) in sized {
        s.push_str(&format!("      {:>10}  {}/\n", gib(*b), d.display()));
    }
    s
}

/// The CELLS LANE of the run rooted at `root`: `<name>-cells.noindex` BESIDE
/// it, where `<name>` is the root's own name less a `.noindex` suffix — so the
/// default snapshot `<caller>-verify.noindex` has `<caller>-verify-cells.noindex`.
/// `None` for a root with no name (`/`).
///
/// A snapshot's run hands it to `gate cells-foreign` as
/// `$ATERM_CELL_TARGET_DIR` (`stages::foreign_cells`). Beside the root, not in
/// it: `xtask` refuses a cells cache inside the workspace it judges. On the
/// root's volume, so the free space this preflight reads is the space it
/// fills. `.noindex`, as the snapshot is, to keep Spotlight out of it.
#[must_use]
pub fn cells_lane(root: &Path) -> Option<PathBuf> {
    let name = root.file_name()?.to_string_lossy().into_owned();
    let base = name.strip_suffix(".noindex").unwrap_or(&name);
    Some(root.with_file_name(format!("{base}-cells.noindex")))
}

/// [`cells_lane`] when it exists as a real directory — a symlink there points
/// somewhere else and is neither measured nor removed.
#[must_use]
pub fn existing_cells_lane(root: &Path) -> Option<PathBuf> {
    cells_lane(root).filter(|d| d.symlink_metadata().is_ok_and(|m| m.is_dir()))
}

/// The run's lanes, root-relative: the directory `target` and every `target-*`
/// directory at the root (a symlink is not counted — it may point at another
/// volume), plus [`NESTED_LANE_DIRS`] where they exist beneath no symlink.
/// Sorted, so the remedy is stable.
///
/// THE ONE DEFINITION of a lane. [`crate::snapshot`] stamps exactly these, the
/// preflight measures exactly these and the cap removes exactly these
/// ([`crate::snapshot::remove_lanes`]), so what the preflight counts is what
/// the cap deletes — with the [`cells_lane`] beside the root added to the
/// measure and the removal, and left out of the stamps (rustup's `stable`
/// builds it, so a new trustc says nothing about it). A root directory whose name merely STARTS with `target` —
/// `targets/`, `target_x/`, a dev `target.noindex/` — is not a lane: no run
/// stamps it, and the cap as first written (2026-09-23, before it was
/// committed) would have deleted it.
#[must_use]
pub fn lane_dirs(root: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            name == "target" || name.starts_with("target-")
        })
        .filter(|e| e.path().symlink_metadata().is_ok_and(|m| m.is_dir()))
        .map(|e| PathBuf::from(e.file_name()))
        .collect();
    out.extend(
        NESTED_LANE_DIRS
            .iter()
            .map(PathBuf::from)
            .filter(|d| real_dirs_all_the_way(root, d)),
    );
    out.sort();
    out
}

/// Every component of `rel` under `root` is a real directory, none a symlink:
/// a lane beneath a link is a directory somewhere else, never this run's.
fn real_dirs_all_the_way(root: &Path, rel: &Path) -> bool {
    let mut at = root.to_path_buf();
    rel.components().all(|c| {
        at.push(c);
        at.symlink_metadata().is_ok_and(|m| m.is_dir())
    })
}

/// The bytes of every regular file under `dir`, symlinks not followed. Best
/// effort: an entry that cannot be read counts nothing. Walked only for a
/// refusal, where seconds spent naming the remedy cost a run that was not
/// going to happen anyway.
#[must_use]
pub fn dir_bytes(dir: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let Ok(m) = e.path().symlink_metadata() else {
                continue;
            };
            if m.is_dir() {
                stack.push(e.path());
            } else if m.is_file() {
                total = total.saturating_add(m.len());
            }
        }
    }
    total
}

/// `bytes` as `N.N GiB`.
#[must_use]
pub fn gib(bytes: u64) -> String {
    // Tenths of a GiB in integer arithmetic, so no float formatting decides a
    // ladder byte — widened so `bytes * 10` cannot overflow, and never
    // `bytes / (GIB / 10)`: that unit truncates short of a real tenth, and
    // rounded 40 GiB - 1 byte UP to "40.0" (measured by the test below).
    let tenths = u128::from(bytes) * 10 / u128::from(GIB);
    format!("{}.{} GiB", tenths / 10, tenths % 10)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MACOS: &str = "Filesystem   1024-blocks      Used Available Capacity  Mounted on\n\
                         /dev/disk3s5   971350180 862653008  83991144    92%    /System/Volumes/Data\n";
    const LINUX: &str = "Filesystem     1024-blocks      Used Available Capacity Mounted on\n\
                         /dev/nvme0n1p2   981876212 812345678 119548922      88% /\n";

    /// Lanes measured at `bytes` in one `target/` dir.
    fn held(bytes: u64) -> Lanes {
        Lanes::Measured(vec![(PathBuf::from("target"), bytes)])
    }

    #[test]
    fn the_available_column_is_read_in_bytes_whatever_the_mount_point_is_called() {
        assert_eq!(parse_df(MACOS), Some(83_991_144 * 1024));
        assert_eq!(parse_df(LINUX), Some(119_548_922 * 1024));
        // A mount point with spaces sits last, so it cannot shift the column.
        let spaced = "Filesystem 1024-blocks Used Available Capacity Mounted on\n\
                      //u@h/share 100 40 60 40% /Volumes/My Share Name\n";
        assert_eq!(parse_df(spaced), Some(60 * 1024));
    }

    /// THE COLUMN IS FOUND, NOT COUNTED — the reading that would have started a
    /// gate on a full volume.
    ///
    /// `-P` fixes the column ORDER and nothing else, so the FIRST column may
    /// hold spaces too. macOS autofs really does name one `map auto_home`
    /// (`df -Pk /System/Volumes/Data/home` on this machine), which makes the
    /// row seven fields wide; read by position the fourth is `Used`. Given real
    /// numbers that is not a missing answer but a WRONG one, roughly the used
    /// bytes reported as free — and [`decide`] would have said `Ok`. A row this
    /// code cannot anchor must yield `None`, because `None` refuses.
    #[test]
    fn a_filesystem_column_with_spaces_does_not_shift_the_reading_onto_used() {
        // The shape, verbatim from this machine (zero-sized autofs map).
        let autofs = "Filesystem    1024-blocks Used Available Capacity  Mounted on\n\
                      map auto_home           0    0         0   100%    /System/Volumes/Data/home\n";
        assert_eq!(parse_df(autofs), Some(0));
        // The same shape with real numbers: a 900 GiB volume with 12 GiB free.
        let real = "Filesystem    1024-blocks      Used Available Capacity  Mounted on\n\
                    map auto_home   943718400 931135488  12582912   99%    /Users//x\n";
        assert_eq!(
            parse_df(real),
            Some(12_582_912 * 1024),
            "counting fields from the left reads Used (888 GiB) and starts the gate"
        );
        let cold = plan(Budget::MEASURED, None, &held(0), Owner::Snapshot);
        assert!(
            decide(
                &Reading::Free(parse_df(real).expect("parsed")),
                &cold,
                Path::new("/Users//x")
            )
            .is_err(),
            "12 GiB free must refuse a cold run's 30 GiB"
        );
        // Spaces at BOTH ends at once.
        let both = "Filesystem 1024-blocks Used Available Capacity Mounted on\n\
                    map auto_home 100 40 60 40% /Volumes/My Share Name\n";
        assert_eq!(parse_df(both), Some(60 * 1024));
    }

    /// A `%` that is not the `Capacity` cell cannot be mistaken for it, and a
    /// row with no percentage at all reads as nothing rather than as a number
    /// from the wrong column.
    #[test]
    fn only_a_digits_then_percent_cell_anchors_the_reading() {
        assert!(is_percentage("40%"));
        assert!(is_percentage("100%"));
        assert!(!is_percentage("%"));
        assert!(!is_percentage("40"));
        assert!(!is_percentage("4%0"));
        assert!(!is_percentage("/Volumes/50%off"));
        let named = "Filesystem 1024-blocks Used Available Capacity Mounted on\n\
                     /dev/disk1 100 40 60 40% /Volumes/50%off\n";
        assert_eq!(parse_df(named), Some(60 * 1024));
        let no_capacity = "Filesystem 1024-blocks Used Available Mounted on\n\
                           /dev/disk1 100 40 60 /mnt\n";
        assert_eq!(parse_df(no_capacity), None);
    }

    #[test]
    fn anything_that_is_not_a_df_table_reads_as_nothing() {
        for text in [
            "",
            "Filesystem 1024-blocks Used Available Capacity Mounted on\n",
            "garbage\nmore garbage\n",
            "h\n/dev/x 1 2 notanumber 3% /\n",
        ] {
            assert_eq!(parse_df(text), None, "{text:?}");
        }
    }

    /// THE ESTIMATE, on synthetic numbers: `max(cold - credited, warm) +
    /// in_flight + reserve`, and never a wrapped one. The in-flight term is paid
    /// cold AND warm — the trust lane's slots are written every run.
    #[test]
    fn the_requirement_is_the_cold_footprint_less_the_lanes_but_never_less_than_a_warm_run() {
        let b = Budget {
            cold: 100,
            warm_growth: 30,
            in_flight: 5,
            reserve: 7,
            lane_cap: 1000,
        };
        assert_eq!(b.need(0), 112, "cold: the whole footprint");
        assert_eq!(b.need(40), 72, "partly warm: the rest of it");
        assert_eq!(b.need(70), 42, "at cold - warm the two terms meet");
        assert_eq!(
            b.need(95),
            42,
            "warm: a run's growth, however full the lanes"
        );
        assert_eq!(b.need(u64::MAX), 42);
        let huge = Budget {
            cold: u64::MAX,
            warm_growth: 0,
            in_flight: 0,
            reserve: u64::MAX,
            lane_cap: u64::MAX,
        };
        assert_eq!(huge.need(0), u64::MAX, "saturates, never wraps small");
    }

    /// THE MEASURED BUDGET against the case that motivated it: the snapshot's
    /// lanes holding the 20.2 GiB `b994cadd0`'s cold run left (21,220,340 KiB
    /// by `du -sk`, 2026-09-23). The flat 40 GiB floor refused the next run at
    /// the 22.2 GiB the volume then read; the estimate asked for a warm run's
    /// growth plus the reserve, 22.0 GiB, and admitted it. Since the cells lane
    /// joined the lanes (2026-09-27) a warm run grows them 2 GiB more, so the
    /// same lanes need 24.0 GiB; and the trust lane's scratch slots, written and
    /// removed by every run, 6 GiB more: 30.0 GiB. That 22.2 GiB reading is now
    /// refused, and the estimate still asks 10 GiB less than the flat floor did.
    #[test]
    fn a_warm_snapshot_needs_its_growth_not_the_flat_floor() {
        let root = Path::new("/Users//x/aterm-verify.noindex");
        let warm = plan(
            Budget::MEASURED,
            None,
            &held(21_220_340 * 1024),
            Owner::Snapshot,
        );
        assert!(!warm.remove);
        assert_eq!(warm.credited, 21_220_340 * 1024);
        assert_eq!(
            warm.need,
            WARM_GROWTH_BYTES + TRUST_LANE_BYTES + RESERVE_BYTES
        );
        assert_eq!(gib(warm.need), "30.0 GiB");
        // One byte over the 30.2 GiB boundary: `GIB / 5` alone is just under a
        // real fifth.
        let read = 30 * GIB + GIB / 5 + 1;
        assert_eq!(gib(read), "30.2 GiB");
        assert_eq!(decide(&Reading::Free(read), &warm, root), Ok(()));
        assert_eq!(decide(&Reading::Free(30 * GIB), &warm, root), Ok(()));
        let why = decide(&Reading::Free(30 * GIB - 1), &warm, root).expect_err("under 30");
        assert!(
            why.starts_with("disk: 29.9 GiB free on the volume holding /Users//x/"),
            "{why}"
        );
        assert!(why.contains("under the 30.0 GiB this run needs"), "{why}");
        assert!(why.ends_with("nothing was built"), "{why}");
        // The 2026-09-23 reading, short by the cells and trust lanes.
        assert!(decide(&Reading::Free(22 * GIB + GIB / 5 + 1), &warm, root).is_err());

        // From empty lanes the same budget asks for the whole footprint.
        let cold = plan(Budget::MEASURED, None, &held(0), Owner::Snapshot);
        assert_eq!(cold.need, COLD_BYTES + TRUST_LANE_BYTES + RESERVE_BYTES);
        assert_eq!(gib(cold.need), "39.0 GiB");
        assert!(decide(&Reading::Free(30 * GIB), &cold, root).is_err());
        assert_eq!(decide(&Reading::Free(39 * GIB), &cold, root), Ok(()));
        assert!(decide(&Reading::Free(39 * GIB - 1), &cold, root).is_err());
    }

    /// THE CELLS LANE IS IN THE BUDGET (2026-09-27): `gate cells-foreign`'s
    /// cold footprint and growth are terms of the cold footprint and of a warm
    /// run's growth, and so of the cap — before this the stage wrote 2.6 GiB
    /// (12 GiB with incremental on) that no term counted.
    #[test]
    fn the_cells_lane_is_a_term_of_the_cold_footprint_the_growth_and_the_cap() {
        assert_eq!(COLD_BYTES, 24 * GIB + CELLS_COLD_BYTES);
        assert_eq!(WARM_GROWTH_BYTES, 16 * GIB + CELLS_WARM_GROWTH_BYTES);
        assert_eq!(gib(CELLS_COLD_BYTES), "3.0 GiB");
        assert_eq!(gib(CELLS_WARM_GROWTH_BYTES), "2.0 GiB");
        // Above what was measured: 2,751,212 KiB cold, 1.00 GiB first-party.
        const { assert!(CELLS_COLD_BYTES > 2_751_212 * 1024) };
        const { assert!(CELLS_WARM_GROWTH_BYTES > GIB) };
    }

    /// OVER THE CAP a snapshot's lanes are removed and the run is budgeted
    /// cold; at the cap they stay and are credited. In place nothing is ever
    /// removed or credited, and lanes that could not be measured credit
    /// nothing — both are budgeted cold, the direction that refuses more.
    #[test]
    fn lanes_over_the_cap_are_removed_and_budgeted_cold_and_only_a_snapshots() {
        let b = Budget::MEASURED;
        let at_cap = plan(b, None, &held(LANE_CAP_BYTES), Owner::Snapshot);
        assert!(!at_cap.remove, "at the cap is not over it");
        assert_eq!(at_cap.credited, LANE_CAP_BYTES);
        assert_eq!(
            at_cap.need,
            WARM_GROWTH_BYTES + TRUST_LANE_BYTES + RESERVE_BYTES
        );

        let over = plan(b, None, &held(LANE_CAP_BYTES + 1), Owner::Snapshot);
        assert!(over.remove);
        assert_eq!(over.credited, 0, "removed lanes are not credited");
        assert_eq!(over.need, COLD_BYTES + TRUST_LANE_BYTES + RESERVE_BYTES);

        let unknown = plan(
            b,
            None,
            &Lanes::Unknown("du -sk did not finish within 60.0 s".into()),
            Owner::Snapshot,
        );
        assert!(
            !unknown.remove,
            "an unmeasured tree is not known to be over"
        );
        assert_eq!(unknown.credited, 0);
        assert_eq!(unknown.need, COLD_BYTES + TRUST_LANE_BYTES + RESERVE_BYTES);

        // The cap is the sum it is documented as — the held lanes' — and the
        // trust lane's in-flight bytes are not in it (review, 2026-09-27: they
        // had raised it to 57 GiB for lanes that hold none of them).
        assert_eq!(LANE_CAP_BYTES, COLD_BYTES + WARM_GROWTH_BYTES);
        assert_eq!(gib(LANE_CAP_BYTES), "45.0 GiB");
    }

    /// `--disk-floor` is exactly its number — no estimate, whatever the lanes
    /// hold — and it moves the requirement only: over-cap lanes are still
    /// removed. A floor of zero refuses nothing a volume can be read for.
    #[test]
    fn the_disk_floor_replaces_the_estimate_and_nothing_else() {
        let root = Path::new("/r");
        let floor = plan(
            Budget::MEASURED,
            Some(40 * GIB),
            &held(20 * GIB),
            Owner::Snapshot,
        );
        assert_eq!(floor.need, 40 * GIB, "no credit for warm lanes");
        assert_eq!(decide(&Reading::Free(40 * GIB), &floor, root), Ok(()));
        let why = decide(&Reading::Free(40 * GIB - 1), &floor, root).expect_err("one byte under");
        assert!(why.starts_with("disk: 39.9 GiB free"), "{why}");
        assert!(why.contains("under the 40.0 GiB --disk-floor"), "{why}");

        let zero = plan(
            Budget::MEASURED,
            Some(0),
            &held(LANE_CAP_BYTES + 1),
            Owner::Snapshot,
        );
        assert_eq!(zero.need, 0);
        assert!(
            zero.remove,
            "the floor does not lift the bound on the lanes"
        );
        assert_eq!(decide(&Reading::Free(0), &zero, root), Ok(()));

        // A volume that cannot be read refuses under any requirement.
        let why = decide(
            &Reading::Unknown("cannot run df -Pk: gone".into()),
            &zero,
            root,
        )
        .expect_err("unmeasurable refuses");
        assert!(
            why.contains("could not be read (cannot run df -Pk: gone)"),
            "{why}"
        );
        assert!(
            why.contains("0.0 GiB --disk-floor could not be checked"),
            "{why}"
        );
    }

    /// The header line carries every term, so a reader can redo the sum from
    /// the record alone — for each way the lanes can stand.
    #[test]
    fn the_header_line_carries_the_free_space_the_lanes_and_the_arithmetic() {
        let root = Path::new("/r");
        let free = Reading::Free(22 * GIB + GIB / 5 + 1);
        let warm = plan(
            Budget::MEASURED,
            None,
            &held(20 * GIB + GIB / 5 + 1),
            Owner::Snapshot,
        );
        assert_eq!(
            header_line(&free, &warm, root),
            "verify: disk 22.2 GiB free on the volume holding /r; build dirs 20.2 GiB; need 30.0 \
             GiB = max(27.0 GiB cold - 20.2 GiB credited, 18.0 GiB warm growth) + 6.0 GiB in \
             flight + 6.0 GiB reserve\n"
        );

        let mut over = plan(Budget::MEASURED, None, &held(60 * GIB), Owner::Snapshot);
        let line = header_line(&free, &over, root);
        assert!(
            line.contains(
                "; build dirs 60.0 GiB, over the 45.0 GiB cap, so removed before the free \
                           space was read; need 39.0 GiB = max(27.0 GiB cold - 0.0 GiB credited"
            ),
            "{line}"
        );
        over.unremoved = vec!["target not removed: busy".into()];
        let line = header_line(&free, &over, root);
        assert!(
            line.contains("; removing them failed as the `verify: build dirs` line(s) above say"),
            "{line}"
        );
        assert!(!line.contains("so removed"), "{line}");

        let unknown = plan(
            Budget::MEASURED,
            None,
            &Lanes::Unknown("du -sk failed".into()),
            Owner::Snapshot,
        );
        assert!(
            header_line(&free, &unknown, root).contains(
                "; build dirs unmeasured (du -sk failed), so none credited; need 39.0 GiB"
            )
        );
        let floor = plan(Budget::MEASURED, Some(0), &held(GIB), Owner::Snapshot);
        let line = header_line(&free, &floor, root);
        assert!(
            line.ends_with("; need 0.0 GiB (--disk-floor: exactly this, no estimate)\n"),
            "{line}"
        );
        let line = header_line(&Reading::Unknown("no df".into()), &floor, root);
        assert!(
            line.starts_with(
                "verify: disk free space on the volume holding /r could not be read (no df); "
            ),
            "{line}"
        );
        assert_eq!(line.matches('\n').count(), 1);
    }

    #[test]
    fn sizes_print_in_tenths_of_a_gib_without_a_float() {
        assert_eq!(gib(0), "0.0 GiB");
        assert_eq!(gib(GIB), "1.0 GiB");
        assert_eq!(gib(GIB / 10 - 1), "0.0 GiB");
        assert_eq!(gib(55 * GIB + GIB * 3 / 10 + 1), "55.3 GiB");
        assert_eq!(
            gib(55 * GIB + GIB / 10 * 3),
            "55.2 GiB",
            "three truncated tenths are under 0.3"
        );
        assert_eq!(gib(23 * GIB + GIB - 1), "23.9 GiB");
    }

    /// `du -sk` prints one size per operand, and anything else is a
    /// measurement this does not have.
    #[test]
    fn du_output_is_one_size_per_lane_or_nothing() {
        assert_eq!(
            parse_du("14875876\ttarget\n910232\ttarget-tippy\n", 2),
            Some(vec![14_875_876, 910_232])
        );
        // POSIX specifies blanks between the columns, not a tab.
        assert_eq!(parse_du("12 target\n", 1), Some(vec![12]));
        assert_eq!(parse_du("12\ttarget\n", 2), None, "a lane with no line");
        assert_eq!(
            parse_du("12\ttarget\nweird\n", 1),
            None,
            "a path with a newline"
        );
        assert_eq!(parse_du("", 1), None);
        assert_eq!(parse_du("", 0), Some(Vec::new()));
    }

    /// The live measurement: what `du -sk` gives each lane dir, at least the
    /// bytes its files hold, and nothing for a tree with no lanes.
    #[cfg(unix)]
    #[test]
    fn the_lanes_are_measured_by_du_one_size_per_dir() {
        let tmp = crate::mktemp_dir("atv-disk-du").expect("mktemp");
        assert_eq!(measure_lanes(&tmp), Lanes::Measured(Vec::new()));
        std::fs::create_dir_all(tmp.join("target/debug")).expect("mkdir");
        std::fs::create_dir_all(tmp.join("libc-oracle/target")).expect("mkdir");
        std::fs::write(tmp.join("target/debug/big"), vec![1u8; 256 * 1024]).expect("write");
        let Lanes::Measured(sized) = measure_lanes(&tmp) else {
            panic!("du -sk did not measure {}", tmp.display());
        };
        let names: Vec<String> = sized.iter().map(|(d, _)| d.display().to_string()).collect();
        assert_eq!(names, ["libc-oracle/target", "target"]);
        assert!(sized[1].1 >= 256 * 1024, "{sized:?}");
        assert!(sized[1].1 < 1024 * 1024, "{sized:?}");
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// THE CELLS LANE sits BESIDE the root, is named from it, and is measured
    /// with the lanes when it is a real directory — never through a symlink,
    /// which points somewhere this run does not own.
    #[cfg(unix)]
    #[test]
    fn the_cells_lane_sits_beside_the_root_and_is_measured_with_the_lanes() {
        assert_eq!(
            cells_lane(Path::new("/Users//x/aterm-verify.noindex")),
            Some(PathBuf::from("/Users//x/aterm-verify-cells.noindex"))
        );
        assert_eq!(
            cells_lane(Path::new("/s/snap")),
            Some(PathBuf::from("/s/snap-cells.noindex"))
        );
        assert_eq!(cells_lane(Path::new("/")), None);

        let base = crate::mktemp_dir("atv-disk-cells").expect("mktemp");
        let snap = base.join("c-verify.noindex");
        std::fs::create_dir_all(snap.join("target/debug")).expect("mkdir");
        std::fs::write(snap.join("target/debug/a"), vec![1u8; 64 * 1024]).expect("write");
        // No cells lane yet: the target lanes alone.
        let Lanes::Measured(sized) = measure_lanes(&snap) else {
            panic!("du -sk did not measure {}", snap.display());
        };
        assert_eq!(sized.len(), 1, "{sized:?}");
        let cells = base.join("c-verify-cells.noindex");
        std::fs::create_dir_all(cells.join("win/debug")).expect("mkdir");
        std::fs::write(cells.join("win/debug/b"), vec![1u8; 512 * 1024]).expect("write");
        let Lanes::Measured(sized) = measure_lanes(&snap) else {
            panic!("du -sk did not measure {}", snap.display());
        };
        let names: Vec<PathBuf> = sized.iter().map(|(d, _)| d.clone()).collect();
        assert_eq!(names, [PathBuf::from("target"), cells.clone()]);
        assert!(sized[1].1 >= 512 * 1024, "{sized:?}");
        assert!(
            Lanes::Measured(sized.clone()).total().expect("measured") >= 576 * 1024,
            "the cells lane counts toward the total the cap judges: {sized:?}"
        );
        // A link in its place is not measured.
        std::fs::rename(&cells, base.join("elsewhere")).expect("mv");
        std::os::unix::fs::symlink(base.join("elsewhere"), &cells).expect("symlink");
        assert_eq!(existing_cells_lane(&snap), None);
        let Lanes::Measured(sized) = measure_lanes(&snap) else {
            panic!("du -sk did not measure {}", snap.display());
        };
        assert_eq!(sized.len(), 1, "{sized:?}");
        std::fs::remove_dir_all(&base).ok();
    }

    /// THE BOUND: a child still running at the deadline is killed and reaped,
    /// and the deadline is what is reported — the measurement never holds a
    /// run for longer than it was given.
    #[cfg(unix)]
    #[test]
    fn a_child_past_its_deadline_is_killed_and_named() {
        let mut slow = Command::new("sleep");
        slow.arg("30");
        let started = Instant::now();
        let got = output_within(slow, Duration::from_millis(200));
        assert_eq!(
            got.map(|o| o.status.success()),
            Err("did not finish within 0.2 s".into())
        );
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the child was not killed at its deadline: {:?}",
            started.elapsed()
        );
        let mut quick = Command::new("/bin/sh");
        quick.args(["-c", "echo out; echo err >&2"]);
        let out = output_within(quick, Duration::from_secs(30)).expect("ran");
        assert!(out.status.success());
        assert_eq!(out.stdout, b"out\n");
        assert_eq!(out.stderr, b"err\n");
        let gone = output_within(Command::new("/no/such/program"), Duration::from_secs(1));
        assert!(
            gone.as_ref()
                .is_err_and(|e| e.starts_with("could not be run")),
            "{gone:?}"
        );
    }

    /// The remedy names the run's OWN dirs: `target` and the root-level
    /// `target-*` directories and the nested lane dirs, never a file or a
    /// symlink that happens to carry the name, a root directory whose name
    /// only starts with `target`, nor a nested lane beneath a symlink.
    #[cfg(unix)]
    #[test]
    fn the_lane_dirs_are_the_ones_that_exist_and_nothing_that_merely_sounds_like_one() {
        let tmp = crate::mktemp_dir("atv-disk").expect("mktemp");
        for d in [
            "target/debug/deps",
            "target-tippy/debug",
            "target-drivers",
            "libc-oracle/target",
            "tools/freeze-safety-gate/target/x",
            "crates/target-not-a-lane",
            "elsewhere/freeze-safety-gate/target",
            "targets",
            "target_x",
            "target.noindex/debug",
        ] {
            std::fs::create_dir_all(tmp.join(d)).expect("mkdir");
        }
        std::fs::write(tmp.join("target/debug/deps/libx.rlib"), vec![0u8; 3000]).expect("write");
        std::fs::write(tmp.join("target/debug/deps/x.d"), vec![0u8; 500]).expect("write");
        std::fs::write(tmp.join("target-tippy/debug/y.rmeta"), vec![0u8; 700]).expect("write");
        std::fs::write(tmp.join("target-notes.txt"), "a file, not a lane\n").expect("write");
        std::os::unix::fs::symlink(tmp.join("target"), tmp.join("target-elsewhere"))
            .expect("symlink");

        let dirs = lane_dirs(&tmp);
        let names: Vec<String> = dirs.iter().map(|d| d.display().to_string()).collect();
        assert_eq!(
            names,
            [
                "libc-oracle/target",
                "target",
                "target-drivers",
                "target-tippy",
                "tools/freeze-safety-gate/target"
            ]
        );
        assert_eq!(dir_bytes(&tmp.join("target")), 3500);
        assert_eq!(dir_bytes(&tmp.join("target-tippy")), 700);
        assert_eq!(dir_bytes(&tmp.join("target-drivers")), 0);
        assert_eq!(
            dir_bytes(&tmp.join("absent")),
            0,
            "an unreadable dir counts nothing"
        );

        let lanes = measure_lanes(&tmp);
        let p = plan(Budget::MEASURED, None, &lanes, Owner::Snapshot);
        let text = remedy(&tmp, &p, &lanes, &Reading::Free(GIB), None);
        assert!(text.contains("regenerable"), "{text}");
        assert!(
            text.contains("  target/\n") && text.contains("  target-tippy/\n"),
            "{text}"
        );
        for not_a_lane in [
            "target-notes.txt",
            "target-elsewhere",
            "targets/",
            "target_x/",
        ] {
            assert!(!text.contains(not_a_lane), "{not_a_lane}: {text}");
        }
        assert!(!text.contains("target.noindex"), "{text}");
        assert!(
            text.contains("the next run is then cold and needs 39.0 GiB"),
            "{text}"
        );
        assert!(text.contains("still "), "1 GiB free is short of 39: {text}");
        assert!(text.contains("--disk-floor <GiB>"), "{text}");

        // A nested lane beneath a symlink is somewhere else, never this run's.
        std::fs::remove_dir_all(tmp.join("tools")).expect("rm");
        std::os::unix::fs::symlink(tmp.join("elsewhere"), tmp.join("tools")).expect("symlink");
        assert!(
            !lane_dirs(&tmp).contains(&PathBuf::from("tools/freeze-safety-gate/target")),
            "{:?}",
            lane_dirs(&tmp)
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn a_root_with_no_lane_dirs_still_gets_a_remedy() {
        let tmp = crate::mktemp_dir("atv-disk-empty").expect("mktemp");
        let lanes = measure_lanes(&tmp);
        let p = plan(Budget::MEASURED, None, &lanes, Owner::Snapshot);
        let text = remedy(&tmp, &p, &lanes, &Reading::Free(GIB), None);
        assert!(text.contains("holds no build dirs yet"), "{text}");
        assert!(text.contains("budgeted cold"), "{text}");
        let floor = plan(Budget::MEASURED, Some(40 * GIB), &lanes, Owner::Snapshot);
        let text = remedy(&tmp, &floor, &lanes, &Reading::Free(GIB), None);
        assert!(text.contains("lower --disk-floor"), "{text}");
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// What removing the lanes would buy is arithmetic over the same numbers
    /// the header printed: enough when the freed bytes cover a cold run, and
    /// the shortfall when they do not.
    #[test]
    fn the_remedy_prices_removing_the_lanes_against_a_cold_run() {
        let root = Path::new("/nonexistent/root");
        let lanes = held(20 * GIB);
        let warm = plan(Budget::MEASURED, None, &lanes, Owner::Snapshot);
        let text = remedy(root, &warm, &lanes, &Reading::Free(20 * GIB), None);
        assert!(
            text.contains(
                "this run's build dirs hold 20.0 GiB, all of it regenerable. Removing \
                           them gives that back, and the next run is then cold and needs 39.0 GiB \
                           (27.0 GiB cold + 6.0 GiB in flight + 6.0 GiB reserve): 40.0 GiB would be \
                           free — enough:\n"
            ),
            "{text}"
        );
        assert!(text.contains("      20.0 GiB  target/\n"), "{text}");
        let text = remedy(root, &warm, &lanes, &Reading::Free(5 * GIB), None);
        assert!(
            text.contains("25.0 GiB would be free — still 14.0 GiB short"),
            "{text}"
        );
    }

    /// The free bytes come first, and they are named as free. A human under the
    /// requirement deletes what the message names, so what it names first
    /// decides whether the next contract run is warm or cold.
    #[test]
    fn the_remedy_names_the_callers_incremental_caches_before_its_own_warm_lanes() {
        let caller = crate::mktemp_dir("atv-disk-caller").expect("mktemp");
        let root = crate::mktemp_dir("atv-disk-root").expect("mktemp");
        std::fs::create_dir_all(caller.join("target.noindex/debug/incremental")).expect("mk");
        std::fs::write(
            caller.join("target.noindex/debug/incremental/blob"),
            vec![0u8; 4096],
        )
        .expect("write");
        std::fs::create_dir_all(root.join("target-tippy")).expect("mk");
        std::fs::write(root.join("target-tippy/x.rmeta"), vec![0u8; 512]).expect("write");
        let lanes = measure_lanes(&root);
        let p = plan(Budget::MEASURED, None, &lanes, Owner::Snapshot);
        let free = Reading::Free(GIB);

        let text = remedy(&root, &p, &lanes, &free, Some(&caller));
        let free_at = text.find("costs nothing").expect(&text);
        let lanes_at = text.find("target-tippy/").expect(&text);
        assert!(free_at < lanes_at, "free bytes must be named first: {text}");
        assert!(text.contains("incremental"), "{text}");
        assert!(text.contains("CARGO_INCREMENTAL=0"), "{text}");
        assert!(text.contains("Only if that is not enough"), "{text}");

        // With no caller named there is no second tree, so nothing is claimed.
        let alone = remedy(&root, &p, &lanes, &free, None);
        assert!(!alone.contains("costs nothing"), "{alone}");
        assert!(alone.contains("regenerable"), "{alone}");

        // A caller with no incremental caches says nothing about them either.
        let bare = crate::mktemp_dir("atv-disk-bare").expect("mktemp");
        let quiet = remedy(&root, &p, &lanes, &free, Some(&bare));
        assert!(!quiet.contains("costs nothing"), "{quiet}");

        for d in [caller, root, bare] {
            std::fs::remove_dir_all(&d).ok();
        }
    }

    /// The live read on this machine: a real number on a real volume, or a
    /// named reason — never a panic and never a silent zero.
    #[test]
    fn the_live_read_answers_with_bytes_or_with_a_reason() {
        match read_free(Path::new(".")) {
            Reading::Free(n) => assert!(n > 0, "df read zero bytes free for the cwd's volume"),
            Reading::Unknown(why) => assert!(!why.is_empty()),
        }
        let gone = read_free(Path::new("/no/such/dir/anywhere"));
        assert!(
            matches!(gone, Reading::Unknown(ref why) if why.contains("df -Pk")),
            "{gone:?}"
        );
    }
}
