// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Rewind's cost on the release binary: parse throughput and resident size with recording on against recording off.

use std::path::{Path, PathBuf};

use super::Rig;
use super::instance::{
    Budget, CouldNotRun, Instance, Scratch, World, could_not_run, field, unanswered,
};
use super::starvation::ShapeVerdict;

/// A take's wall-clock limit: every wait and request on its instances ends with it.
pub const BUDGET_SECS: u64 = 600;

/// ATERM_DESIGN.md §7.2's idle target for the whole app, in bytes. [`CostRow::IdleRss`] reports it on every bundle and judges it only on one that records by default: it measures the app, not recording (an idle instance with recording off misses it too), and design §14.5 R4 holds the default off until an idle recording pane meets it.
pub const APP_IDLE_TARGET_BYTES: u64 = 25_000_000;

/// What a cost row measures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CostRow {
    /// `pairs` ABBA pairs (off, on, on, off) of one pane draining `bytes`, each take a fresh instance: passes when the off arm's median drain time is at least `min_milli` thousandths of the on arm's.
    Throughput {
        bytes: usize,
        pairs: u32,
        min_milli: u64,
    },
    /// One idle pane, `pairs` ABBA pairs (off, on, on, off), each take a fresh instance: passes when the on arm's median resident size is at most `max_extra_bytes` over the off arm's and, on a bundle that records by default (one more instance with no `temporal_recording` key says), at most [`APP_IDLE_TARGET_BYTES`] ([`idle_verdict`]). Each take's app memory ([`app_bytes`]) is reported beside it and judges nothing.
    IdleRss { pairs: u32, max_extra_bytes: u64 },
    /// `panes` panes each draining `bytes` at once: passes when recording on adds at most `max_extra_bytes` of resident size over recording off and the pool holds no more than its limit.
    BusyRss {
        panes: u32,
        bytes: usize,
        max_extra_bytes: u64,
    },
}

/// One row's outcome: its report line and verdict.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CostReport {
    pub line: String,
    pub verdict: ShapeVerdict,
}

/// An instance in its own scratch world, recording or not.
struct Take {
    inst: Instance,
    home: PathBuf,
    _scratch: Scratch,
}

impl Take {
    fn launch(rig: &Rig, record: bool, budget: Budget) -> Result<Self, CouldNotRun> {
        let (take, recording) =
            Self::start(rig, &format!("temporal_recording = {record}"), budget)?;
        if recording != record {
            return could_not_run(format!("the instance's recording is not {record}"));
        }
        Ok(take)
    }

    /// Whether the bundle records a new pane when its config has no `temporal_recording` key.
    fn records_by_default(rig: &Rig, budget: Budget) -> Result<bool, CouldNotRun> {
        Ok(Self::start(rig, "", budget)?.1)
    }

    /// An instance whose config starts with `config_head`, once its first pane is quiet, and whether that pane records.
    fn start(rig: &Rig, config_head: &str, budget: Budget) -> Result<(Self, bool), CouldNotRun> {
        let mut scratch = Scratch::new("atcost")
            .map_err(|e| CouldNotRun::Refused(format!("cannot make a scratch world: {e}")))?;
        if std::env::var_os("ATERM_PAINT_KEEP").is_some() {
            scratch.keep();
        }
        let world = World::prepare(scratch.path(), config_head)
            .map_err(|e| CouldNotRun::Refused(format!("cannot lay out the scratch world: {e}")))?;
        let inst = Instance::launch(&rig.aterm, &world, budget)?;
        inst.ctl.ok(&["await", "idle", "500", "timeout=60000"])?;
        let status = inst.ctl.ok(&["temporal", "status"])?;
        let recording = match field(&status, "enabled") {
            Some("true") => true,
            Some("false") => false,
            _ => return unanswered(format!("temporal status named no enabled= ({status})")),
        };
        Ok((
            Self {
                inst,
                home: world.home,
                _scratch: scratch,
            },
            recording,
        ))
    }

    /// Start `fake-tui --drain` in the pane `target` names (`None`: the first), reporting into `out`.
    fn start_drain(
        &self,
        rig: &Rig,
        target: Option<&str>,
        bytes: usize,
        out: &Path,
    ) -> Result<(), CouldNotRun> {
        let line = format!(
            "{} --drain {bytes} {}",
            rig.fake_tui.display(),
            out.display()
        );
        let mut send = target.map(|t| vec![t]).unwrap_or_default();
        send.extend(["send", line.as_str()]);
        self.inst.ctl.ok(&send)?;
        let mut key = target.map(|t| vec![t]).unwrap_or_default();
        key.extend(["key", "enter"]);
        self.inst.ctl.ok(&key)?;
        Ok(())
    }

    /// The microseconds a drain reported into `out`.
    fn drained_us(&self, out: &Path) -> Result<u64, CouldNotRun> {
        self.inst.ctl.await_event("the drain's report", || {
            std::fs::read_to_string(out)
                .ok()
                .and_then(|s| s.trim().parse::<u64>().ok())
        })
    }

    /// The instance's resident size in bytes, once its panes are quiet.
    fn settled_rss(&self) -> Result<u64, CouldNotRun> {
        Ok(self.settled()?.rss)
    }

    /// The instance's resident size and app memory ([`app_bytes`]), once its panes are quiet.
    fn settled(&self) -> Result<Settled, CouldNotRun> {
        self.inst
            .ctl
            .ok(&["await", "idle", "2000", "timeout=120000"])?;
        std::thread::sleep(std::time::Duration::from_secs(2));
        let pid = self.inst.pid();
        Ok(Settled {
            rss: rss_bytes(pid)?,
            app: app_bytes(pid)?,
        })
    }
}

/// One settled take's two sizes, in bytes.
struct Settled {
    /// `ps`'s resident size ([`rss_bytes`]).
    rss: u64,
    /// The app's own memory ([`app_bytes`]).
    app: u64,
}

/// The memory `pid` holds for itself, in bytes, which the idle row reports beside `ps`'s resident size. On macOS
/// that is its physical footprint (`proc_pid_rusage`'s `ri_phys_footprint`, Activity Monitor's
/// "Memory"): its dirty private pages, compressed ones too, and none of the clean file-backed
/// pages (aterm's own code, mostly) that `ps`'s resident size also counts, which the kernel
/// can drop at any time and which move with the rest of the Mac's memory pressure. Elsewhere it
/// is the resident size.
#[cfg(target_os = "macos")]
fn app_bytes(pid: u32) -> Result<u64, CouldNotRun> {
    let id = libc::c_int::try_from(pid)
        .map_err(|_| CouldNotRun::Refused(format!("pid {pid} is out of range")))?;
    let mut info = std::mem::MaybeUninit::<libc::rusage_info_v4>::zeroed();
    // SAFETY: `info` is a zeroed `rusage_info_v4`, the struct `RUSAGE_INFO_V4` fills; the
    // kernel writes at most its size, and it is read only on success.
    let rc = unsafe {
        libc::proc_pid_rusage(
            id,
            libc::RUSAGE_INFO_V4,
            info.as_mut_ptr().cast::<libc::rusage_info_t>(),
        )
    };
    if rc != 0 {
        return Err(CouldNotRun::Refused(format!(
            "proc_pid_rusage named no footprint for {pid}"
        )));
    }
    // SAFETY: zeroed is a valid `rusage_info_v4` (plain integers), and the kernel filled it.
    Ok(unsafe { info.assume_init() }.ri_phys_footprint)
}

/// The resident size, where the platform names no footprint.
#[cfg(not(target_os = "macos"))]
fn app_bytes(pid: u32) -> Result<u64, CouldNotRun> {
    rss_bytes(pid)
}

/// `ps`'s resident size of `pid`, in bytes.
fn rss_bytes(pid: u32) -> Result<u64, CouldNotRun> {
    let out = aterm_uds::spawnfd::command(Path::new("/bin/ps"))
        .env_clear()
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .output()
        .map_err(|e| CouldNotRun::Refused(format!("cannot run ps: {e}")))?;
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<u64>()
        .map(|kib| kib * 1024)
        .map_err(|_| CouldNotRun::Refused(format!("ps named no resident size for {pid}")))
}

/// The median of `v` (the lower of the middle two).
#[must_use]
pub fn median(v: &[u64]) -> u64 {
    let mut s = v.to_vec();
    s.sort_unstable();
    s.get((s.len().saturating_sub(1)) / 2).copied().unwrap_or(0)
}

/// The off arm's median over the on arm's, in thousandths (1000: recording costs nothing).
#[must_use]
pub fn ratio_milli(off: &[u64], on: &[u64]) -> u64 {
    let on = median(on);
    if on == 0 {
        return 0;
    }
    median(off).saturating_mul(1000) / on
}

/// What recording adds to an idle instance's resident size: the on arm's median less the off arm's, 0 when the on arm is the smaller.
#[must_use]
pub fn added_bytes(off: &[u64], on: &[u64]) -> u64 {
    median(on).saturating_sub(median(off))
}

/// An idle row's verdict: passes when both arms were taken, recording adds at most `max_extra` resident bytes ([`added_bytes`]), and, when the bundle records by default, the on arm's median app footprint is at most [`APP_IDLE_TARGET_BYTES`]. A bundle that records only when asked is not judged on the app's own size.
#[must_use]
pub fn idle_verdict(
    off: &[u64],
    on: &[u64],
    on_app: &[u64],
    max_extra: u64,
    records_by_default: bool,
) -> ShapeVerdict {
    verdict(
        !off.is_empty()
            && !on.is_empty()
            && added_bytes(off, on) <= max_extra
            && (!records_by_default
                || (!on_app.is_empty() && median(on_app) <= APP_IDLE_TARGET_BYTES)),
    )
}

/// Measure one row.
/// # Errors
/// [`CouldNotRun`] when an instance did not start, a request failed, or a drain never reported.
pub fn measure(rig: &Rig, row: &CostRow) -> Result<CostReport, CouldNotRun> {
    let _one = super::ONE_TAKE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let budget = Budget::from_now(BUDGET_SECS);
    match *row {
        CostRow::Throughput {
            bytes,
            pairs,
            min_milli,
        } => {
            let (mut off, mut on) = (Vec::new(), Vec::new());
            for _ in 0..pairs {
                for record in [false, true, true, false] {
                    let take = Take::launch(rig, record, budget)?;
                    let out = take.home.join("drain.us");
                    take.start_drain(rig, None, bytes, &out)?;
                    let us = take.drained_us(&out)?;
                    if record { &mut on } else { &mut off }.push(us);
                }
            }
            let milli = ratio_milli(&off, &on);
            Ok(CostReport {
                line: format!(
                    "COST throughput bytes={bytes} pairs={pairs} off_us={off:?} on_us={on:?} \
                     off_median_us={} on_median_us={} ratio_milli={milli} min_milli={min_milli}",
                    median(&off),
                    median(&on)
                ),
                verdict: verdict(milli >= min_milli),
            })
        }
        CostRow::IdleRss {
            pairs,
            max_extra_bytes,
        } => {
            let by_default = Take::records_by_default(rig, budget)?;
            let (mut off, mut on) = (Vec::new(), Vec::new());
            let (mut off_app, mut on_app) = (Vec::new(), Vec::new());
            for _ in 0..pairs {
                for record in [false, true, true, false] {
                    let took = Take::launch(rig, record, budget)?.settled()?;
                    if record { &mut on } else { &mut off }.push(took.rss);
                    if record { &mut on_app } else { &mut off_app }.push(took.app);
                }
            }
            let on_median = median(&on);
            let on_app_median = median(&on_app);
            Ok(CostReport {
                line: format!(
                    "COST idle_rss pairs={pairs} off_bytes={off:?} on_bytes={on:?} \
                     off_median_bytes={} on_median_bytes={on_median} extra_bytes={} \
                     max_extra_bytes={max_extra_bytes} off_app_bytes={off_app:?} \
                     on_app_bytes={on_app:?} off_app_median_bytes={} \
                     on_app_median_bytes={} records_by_default={by_default} \
                     app_target_bytes={APP_IDLE_TARGET_BYTES} app_target_met={}",
                    median(&off),
                    added_bytes(&off, &on),
                    median(&off_app),
                    on_app_median,
                    on_app_median <= APP_IDLE_TARGET_BYTES,
                ),
                verdict: idle_verdict(&off, &on, &on_app, max_extra_bytes, by_default),
            })
        }
        CostRow::BusyRss {
            panes,
            bytes,
            max_extra_bytes,
        } => {
            let (off, _) = busy(rig, false, panes, bytes, budget)?;
            let (on, pool) = busy(rig, true, panes, bytes, budget)?;
            let (used, limit) = pool;
            let extra = on.saturating_sub(off);
            Ok(CostReport {
                line: format!(
                    "COST busy_rss panes={panes} bytes={bytes} off_bytes={off} on_bytes={on} \
                     extra_bytes={extra} max_extra_bytes={max_extra_bytes} pool_used={used} \
                     pool_limit={limit}"
                ),
                verdict: verdict(extra <= max_extra_bytes && used <= limit),
            })
        }
    }
}

/// `panes` panes of one instance each draining `bytes` at once: its settled resident size and the pool's `(used, limit)`.
fn busy(
    rig: &Rig,
    record: bool,
    panes: u32,
    bytes: usize,
    budget: Budget,
) -> Result<(u64, (u64, u64)), CouldNotRun> {
    let take = Take::launch(rig, record, budget)?;
    let mut targets = vec![None];
    for _ in 1..panes {
        let sid = take.inst.ctl.ok(&["spawn", "window=0", "raise=false"])?;
        let sid = sid
            .strip_prefix("OK ")
            .and_then(|s| s.split_whitespace().next())
            .ok_or_else(|| CouldNotRun::Refused(format!("spawn named no session ({sid})")))?;
        targets.push(Some(format!("@{sid}")));
    }
    for target in targets.iter().flatten() {
        take.inst
            .ctl
            .ok(&[target, "await", "idle", "500", "timeout=60000"])?;
    }
    let outs: Vec<PathBuf> = (0..targets.len())
        .map(|n| take.home.join(format!("drain-{n}.us")))
        .collect();
    for (target, out) in targets.iter().zip(&outs) {
        take.start_drain(rig, target.as_deref(), bytes, out)?;
    }
    for out in &outs {
        take.drained_us(out)?;
    }
    let rss = take.settled_rss()?;
    let status = take.inst.ctl.ok(&["temporal", "status"])?;
    let pool = |key| field(&status, key).and_then(|v| v.parse::<u64>().ok());
    let pool = match (pool("pool_used"), pool("pool_limit")) {
        (Some(used), Some(limit)) => (used, limit),
        _ if !record => (0, 0),
        _ => return unanswered(format!("temporal status named no pool ({status})")),
    };
    Ok((rss, pool))
}

const fn verdict(pass: bool) -> ShapeVerdict {
    if pass {
        ShapeVerdict::Pass
    } else {
        ShapeVerdict::Fail
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ratio_is_the_off_median_over_the_on_median() {
        assert_eq!(median(&[5, 1, 3]), 3);
        assert_eq!(median(&[4, 1, 3, 2]), 2);
        assert_eq!(ratio_milli(&[970, 1000, 990], &[1000, 1000, 1010]), 990);
        assert_eq!(ratio_milli(&[1], &[]), 0, "no on arm passes nothing");
    }

    /// One pane's Rewind budget, the idle row's bound.
    const PANE: u64 = 8 << 20;

    #[test]
    fn the_idle_row_judges_what_recording_adds_and_the_app_only_once_recording_is_the_default() {
        // e11bf220e1's take: the app is twice §7.2's target with recording on and off.
        let (off, on) = ([50_266_112], [50_806_784]);
        assert_eq!(added_bytes(&off, &on), 540_672);
        assert_eq!(
            idle_verdict(&off, &on, &[], PANE, false),
            ShapeVerdict::Pass
        );
        assert_eq!(
            idle_verdict(&off, &on, &on, PANE, true),
            ShapeVerdict::Fail,
            "a bundle that records by default is held to the 25 MB idle target"
        );
        assert_eq!(
            idle_verdict(&[20_000_000], &[24_000_000], &[24_000_000], PANE, true),
            ShapeVerdict::Pass,
            "one that meets it passes"
        );
        assert_eq!(
            idle_verdict(&off, &[59_000_000], &[], PANE, false),
            ShapeVerdict::Fail,
            "8.7 MB added is past one pane's budget"
        );
        assert_eq!(
            idle_verdict(&[50_000_000], &[49_000_000], &[], 0, false),
            ShapeVerdict::Pass,
            "a smaller on arm adds nothing"
        );
        assert_eq!(
            idle_verdict(&[], &[1], &[], PANE, false),
            ShapeVerdict::Fail
        );
        assert_eq!(
            idle_verdict(&[1], &[], &[], PANE, false),
            ShapeVerdict::Fail,
            "no take passes nothing"
        );
        assert_eq!(
            idle_verdict(&[35_000_000], &[35_100_000], &[11_000_000], PANE, true),
            ShapeVerdict::Pass,
            "clean file-backed pages count toward resident size, not the app footprint"
        );
    }

    /// The app reading is the footprint, not the resident size: a 64 MiB file this process
    /// maps and reads is resident and clean, so `ps` counts it and the footprint does not;
    /// 64 MiB of memory it writes is dirty, and the footprint counts it. The readings run in a
    /// child of this test binary that runs this test alone: in the shared test process, the
    /// other tests' own allocations moved the footprint between two readings by more than the
    /// bound.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_app_reading_counts_dirty_pages_and_not_clean_file_pages() {
        const CHILD: &str = "ATERM_TEST_APP_READING_CHILD";
        const HANG: std::time::Duration = std::time::Duration::from_secs(60);
        let test = "measure::rewind_cost::tests::the_app_reading_counts_dirty_pages_and_not_clean_file_pages";
        if std::env::var_os(CHILD).is_none() {
            let mut cmd =
                aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary"));
            cmd.args([test, "--exact", "--nocapture", "--test-threads=1"])
                .env(CHILD, "1");
            let out = aterm_uds::deadline::output_within(&mut cmd, HANG).expect("the child ends");
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert_eq!(
                out.status.code(),
                Some(0),
                "{stdout}{}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(
                stdout
                    .lines()
                    .any(|line| line.starts_with("test result: ok. 1 passed;")),
                "the child ran no test:\n{stdout}"
            );
            return;
        }
        const LEN: usize = 64 << 20;
        const PAGE: usize = 16 << 10;
        let pid = std::process::id();
        let dir = aterm_tempfile::TempDir::new().expect("a scratch directory");
        let path = dir.path().join("clean.bin");
        {
            use std::io::Write as _;
            let mut out = std::fs::File::create(&path).expect("the file");
            let chunk = vec![7u8; 1 << 20];
            for _ in 0..LEN >> 20 {
                out.write_all(&chunk).expect("the file's bytes");
            }
        }
        let file = std::fs::File::open(&path).expect("the file");
        let (rss_0, app_0) = (rss_bytes(pid).expect("rss"), app_bytes(pid).expect("app"));
        // SAFETY: a fresh read-only private mapping of `LEN` bytes of an open file of that
        // length; it is unmapped below and nothing else holds it.
        let map = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                LEN,
                libc::PROT_READ,
                libc::MAP_PRIVATE,
                std::os::fd::AsRawFd::as_raw_fd(&file),
                0,
            )
        };
        assert_ne!(map, libc::MAP_FAILED, "the mapping");
        let mut sum = 0u64;
        for off in (0..LEN).step_by(PAGE) {
            // SAFETY: `off < LEN`, inside the live mapping.
            sum += u64::from(unsafe { std::ptr::read_volatile(map.cast::<u8>().add(off)) });
        }
        assert_eq!(sum, 7 * (LEN / PAGE) as u64, "fixture: every page was read");
        let (rss_1, app_1) = (rss_bytes(pid).expect("rss"), app_bytes(pid).expect("app"));
        // SAFETY: the mapping made above, `LEN` long, unmapped once.
        assert_eq!(unsafe { libc::munmap(map, LEN) }, 0, "the unmap");
        // Fresh anonymous pages (a heap buffer could reuse pages the file's write dirtied).
        // SAFETY: a new private anonymous read-write mapping of `LEN` bytes, unmapped below.
        let anon = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                LEN,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            )
        };
        assert_ne!(anon, libc::MAP_FAILED, "the anonymous mapping");
        for off in (0..LEN).step_by(PAGE) {
            // SAFETY: `off < LEN`, inside the live writable mapping.
            unsafe { std::ptr::write_volatile(anon.cast::<u8>().add(off), 1) };
        }
        let app_2 = app_bytes(pid).expect("app");
        // SAFETY: the anonymous mapping made above, `LEN` long, unmapped once.
        assert_eq!(unsafe { libc::munmap(anon, LEN) }, 0, "the anonymous unmap");
        assert!(
            rss_1 >= rss_0 + (48 << 20),
            "fixture: the read file pages are resident ({rss_0} -> {rss_1})"
        );
        assert!(
            app_1 < app_0 + (16 << 20),
            "the app reading counted clean file pages ({app_0} -> {app_1})"
        );
        assert!(
            app_2 >= app_1 + (48 << 20),
            "the app reading missed 64 MiB of written memory ({app_1} -> {app_2})"
        );
    }

    #[test]
    fn a_low_take_in_either_arm_does_not_decide_the_row() {
        // Design §14.5 As built (R4): six runs' takes, each arm with one low instance (41.45 MB on, 37.81 MB off).
        let on = [
            48_910_000, 48_710_000, 41_450_000, 48_450_000, 48_530_000, 48_610_000,
        ];
        let off = [
            48_170_000, 48_120_000, 37_810_000, 48_840_000, 48_460_000, 48_910_000,
        ];
        assert_eq!(added_bytes(&off, &on), 360_000);
        assert_eq!(
            idle_verdict(&off, &on, &[], PANE, false),
            ShapeVerdict::Pass
        );
        // fd2a2d37fc's one pair put its on take 12 MB under its off take (26.9 against 38.9 MB); the same spread with the off take low fails a row judged on one pair.
        assert_eq!(
            idle_verdict(&[26_900_000], &[38_900_000], &[], PANE, false),
            ShapeVerdict::Fail
        );
    }
}
