// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Where the compiler frontend permits a wrapper, compiler memory belongs to each running unit,
//! not to an immutable Cargo width. Frontends which exclude wrappers retain a whole-stage bound.

use std::cell::{Cell, RefCell};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

const UNIT_BOOK: &str = "ATERM_VERIFY_UNIT_BOOK";
const UNIT_CHECKOUT: &str = "ATERM_VERIFY_UNIT_CHECKOUT";
const UNIT_MODE: &str = "ATERM_VERIFY_UNIT_MODE";
const UNIT_PENDING: &str = "ATERM_VERIFY_UNIT_PENDING";
const UNIT_RUN: &str = "ATERM_VERIFY_UNIT_RUN";
const UNIT_STAGE: &str = "ATERM_VERIFY_UNIT_STAGE";
const UNIT_UNKNOWN: &str = "ATERM_VERIFY_UNIT_UNKNOWN";
const WRAPPER: &str = "ATERM_VERIFY_UNIT_WRAPPER";
const TEST_UNIT_RECORD: &str = "test-compile-unit-memory.json";

/// The parent's envelope while its compiler units claim their own recorded peaks. The unit
/// floor's peak margin is exactly 2 GiB; keep this named for release's leading-claim detector.
pub const DRIVER_CLAIM: u64 = 2 << 30;

/// The smallest enforceable whole-stage Tippy claim. Branded Tippy owns its compiler wrapper,
/// so the verifier cannot split this envelope into compiler-unit claims. The largest retained
/// peak before this bound was introduced was 24.636 GiB.
pub const TIPPY_CLAIM_FLOOR: u64 = 28 << 30;

const UNIT_ENVIRONMENT: [&str; 8] = [
    UNIT_BOOK,
    UNIT_CHECKOUT,
    UNIT_MODE,
    UNIT_PENDING,
    UNIT_RUN,
    UNIT_STAGE,
    UNIT_UNKNOWN,
    WRAPPER,
];

const REFUSED: &str = "aterm-verify: compiler admission: ";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UnitMode {
    Claim,
    Record,
}

impl UnitMode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Claim => "claim",
            Self::Record => "record",
        }
    }

    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "claim" => Ok(Self::Claim),
            "record" => Ok(Self::Record),
            _ => Err(format!("unknown unit mode {value:?}")),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Units {
    pub(crate) book: PathBuf,
    pub(crate) checkout: String,
    pub(crate) mode: UnitMode,
    /// This run's wrapper observations. They are not claimable until the successful stage meter
    /// promotes the maximum of maxrss and attributed footprint into `book`.
    pub(crate) pending: PathBuf,
    pub(crate) run: String,
    pub(crate) stage: String,
    pub(crate) unknown: u64,
    pub(crate) wrapper: PathBuf,
}

impl Units {
    fn environment(&self) -> Vec<(OsString, OsString)> {
        let environment = vec![
            (
                "RUSTC_WRAPPER".into(),
                self.wrapper.clone().into_os_string(),
            ),
            (WRAPPER.into(), "1".into()),
            (UNIT_BOOK.into(), self.book.clone().into_os_string()),
            (UNIT_CHECKOUT.into(), self.checkout.clone().into()),
            (UNIT_MODE.into(), self.mode.as_str().into()),
            (UNIT_PENDING.into(), self.pending.clone().into_os_string()),
            (UNIT_RUN.into(), self.run.clone().into()),
            (UNIT_STAGE.into(), self.stage.clone().into()),
            (UNIT_UNKNOWN.into(), self.unknown.to_string().into()),
        ];
        environment
    }
}

thread_local! {
    static JOBS: Cell<Option<u32>> = const { Cell::new(None) };
    static UNITS: RefCell<Option<Units>> = const { RefCell::new(None) };
    static FLAGS: RefCell<Option<String>> = const { RefCell::new(None) };
}

pub(crate) fn current() -> Option<u32> {
    JOBS.with(Cell::get)
}

/// Compiler widths and source-worker widths, also part of their memory record keys. Census
/// and forge charge inside their bounded workers; their waiting coordinators hold no token.
pub(crate) fn stage_jobs(
    stage: crate::plan::StageId,
    pool_grant: Option<u32>,
    ceiling: Option<u32>,
) -> Option<u32> {
    use crate::plan::StageId;
    pool_grant.and_then(|grant| {
        if matches!(
            stage,
            StageId::Census | StageId::Forge | StageId::ExportScan
        ) {
            Some(1)
        } else {
            crate::memory::compiles(stage)
                .then(|| ceiling.map_or(grant, |ceiling| ceiling.min(grant)).max(1))
        }
    })
}

pub(crate) fn jobs_for(ctx: &crate::Ctx, stage: crate::plan::StageId) -> Option<u32> {
    stage_jobs(
        stage,
        ctx.pool.as_deref().map(aterm_capacity::Jobserver::grant),
        ctx.build_jobs_ceiling(),
    )
}

/// Pooled observations must not lower an unpooled compile's cold estimate.
pub(crate) fn record_key(mut key: crate::memory::Key, jobs: Option<u32>) -> crate::memory::Key {
    if let Some(jobs) = jobs {
        let width = format!("compiler jobs {jobs}");
        key.variant = Some(match key.variant {
            Some(variant) => format!("{variant}; {width}"),
            None => width,
        });
    }
    key
}

pub(crate) fn with_jobs<T>(jobs: Option<u32>, run: impl FnOnce() -> T) -> T {
    struct Restore(Option<u32>);
    impl Drop for Restore {
        fn drop(&mut self) {
            JOBS.with(|slot| slot.set(self.0));
        }
    }
    let _restore = Restore(JOBS.with(|slot| slot.replace(jobs)));
    run()
}

/// Give every Cargo in this stage the same pool and, when present, the unit admission wrapper.
/// Resolution queries reach the real compiler directly; only named compiler units claim memory.
pub(crate) fn configure(command: &mut aterm_uds::spawnfd::Command) {
    FLAGS.with(|slot| {
        if let Some(flags) = &*slot.borrow() {
            command
                .env("MAKEFLAGS", flags)
                .env("CARGO_MAKEFLAGS", flags);
        }
    });
    UNITS.with(|slot| {
        if let Some(units) = &*slot.borrow() {
            command.envs(units.environment());
        }
    });
}

/// The stable name of a stage's driver-only claim.
#[must_use]
pub fn driver_label(title: &str) -> String {
    format!("verify {title} compiler driver")
}

pub(crate) fn run<T>(
    pool: Option<&aterm_capacity::Jobserver>,
    jobs: Option<u32>,
    units: Option<&Units>,
    stopped: &dyn Fn() -> bool,
    run: impl FnOnce() -> T,
) -> std::io::Result<T> {
    struct Restore {
        flags: Option<String>,
        units: Option<Units>,
    }
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = FLAGS.with(|slot| slot.replace(self.flags.take()));
            let _ = UNITS.with(|slot| slot.replace(self.units.take()));
        }
    }
    let _restore = Restore {
        flags: FLAGS.with(|slot| slot.borrow().clone()),
        units: UNITS.with(|slot| slot.replace(units.cloned())),
    };
    // Charge only Cargo's implicit job. Every additional job borrows from this very FIFO,
    // including after either slice ends. There is no private fixed-width token reservation.
    let token = match (pool, jobs) {
        (Some(pool), Some(_)) => Some(pool.acquire(stopped)?),
        _ => None,
    };
    let _ = FLAGS.with(|slot| {
        slot.replace(
            token
                .as_ref()
                .and_then(|_| pool.map(aterm_capacity::Jobserver::flags)),
        )
    });
    Ok(with_jobs(jobs, run))
}

/// Keep a bounded source walk's single-worker memory key while its actual worker owns the
/// token. A coordinator holding that token while waiting would deadlock a one-slot pool.
pub(crate) fn run_stage<T>(
    stage: crate::plan::StageId,
    pool: Option<&aterm_capacity::Jobserver>,
    jobs: Option<u32>,
    units: Option<&Units>,
    stopped: &dyn Fn() -> bool,
    work: impl FnOnce() -> T,
) -> std::io::Result<T> {
    if matches!(
        stage,
        crate::plan::StageId::Census | crate::plan::StageId::Forge
    ) {
        run(pool, None, None, stopped, || with_jobs(jobs, work))
    } else {
        run(pool, jobs, units, stopped, work)
    }
}

pub(crate) fn claim(
    ctx: &crate::Ctx,
    spec: &crate::plan::StageSpec,
    wait: crate::Admitter,
) -> Result<Option<crate::ResourceClaim>, String> {
    let Some(pool) = &ctx.pool else {
        return Ok(None);
    };
    if ctx.background {
        return Ok(None);
    }
    let Some(book) = &ctx.memory else {
        return Ok(None);
    };
    let rank = if ctx.release {
        aterm_capacity::Rank::Release
    } else {
        aterm_capacity::Rank::Ordinary
    };
    if spec.id == crate::plan::StageId::Tippy {
        let bytes = spec.mem.max(TIPPY_CLAIM_FLOOR);
        let claim = wait(
            aterm_capacity::Need::new(format!("verify {}", spec.title))
                .rank(rank)
                .mem(bytes)
                .covers()
                .ends_when_stuck(crate::STUCK_ENDS)
                .margin(1),
            aterm_capacity::Admit::Wait,
        )
        .map_err(|e| e.to_string())?;
        return Ok(Some(crate::ResourceClaim {
            claim: std::sync::Arc::new(claim),
            said: format!(
                "memory: claimed {} for the full compile; branded Tippy owns its compiler wrapper",
                aterm_capacity::size(bytes)
            ),
            units: None,
        }));
    }
    if spec.id != crate::plan::StageId::TestCompile {
        return Ok(None);
    }
    let stage = crate::shape::UNIT_STAGE;
    let unknown = spec.mem.max(aterm_capacity::COLD_CLAIM);
    let backing = book.path.as_deref().map(|path| unit_book(path, spec.id));
    let records = backing
        .as_deref()
        .map(crate::memory::Records::load)
        .unwrap_or_default();
    let measured = has_units(&records, stage);
    let jobs = ctx
        .build_jobs_ceiling()
        .unwrap_or_else(|| pool.grant())
        .min(pool.grant());
    debug_assert_eq!(
        DRIVER_CLAIM,
        aterm_capacity::peak_claim(crate::memory::UNIT_FLOOR)
    );
    // No valid unit history means no evidence for a smaller memory envelope. That cold run still
    // installs the wrapper in record-only mode, so one success calibrates the next run.
    let bytes = if measured { DRIVER_CLAIM } else { unknown };
    let what = if measured {
        driver_label(&spec.title)
    } else {
        format!("verify {}", spec.title)
    };
    let claim = wait(
        aterm_capacity::Need::new(what)
            .rank(rank)
            .mem(bytes)
            .covers()
            .ends_when_stuck(crate::STUCK_ENDS)
            .margin(1),
        aterm_capacity::Admit::Wait,
    )
    .map_err(|e| e.to_string())?;
    let marker = claim.record().cover_marker();
    let units = backing
        .map(|path| {
            Ok::<Units, String>(Units {
                book: path,
                checkout: ctx.root.display().to_string(),
                mode: if measured {
                    UnitMode::Claim
                } else {
                    UnitMode::Record
                },
                pending: pending_book(&ctx.scratch, spec.id, &marker),
                run: marker,
                stage: stage.to_owned(),
                unknown,
                wrapper: std::env::current_exe().map_err(|e| e.to_string())?,
            })
        })
        .transpose()?;
    Ok(Some(crate::ResourceClaim {
        claim: std::sync::Arc::new(claim),
        said: format!(
            "memory: claimed {} for {}; compiler width ceiling {jobs}, active width logged at each unit admission",
            aterm_capacity::size(bytes),
            if measured {
                "the compiler driver; each unit claims its own envelope"
            } else {
                "the full compile while its unit history is calibrated"
            },
        ),
        units,
    }))
}

pub(crate) fn unit_book(path: &Path, id: crate::plan::StageId) -> PathBuf {
    path.with_file_name(match id {
        crate::plan::StageId::TestCompile => TEST_UNIT_RECORD,
        _ => unreachable!("only unit-admitted stages have a unit book"),
    })
}

fn pending_book(scratch: &Path, id: crate::plan::StageId, marker: &str) -> PathBuf {
    let stage = match id {
        crate::plan::StageId::TestCompile => "test-compile",
        _ => unreachable!("only unit-admitted stages have a pending book"),
    };
    scratch.join(format!("{stage}-unit-memory-{marker}.pending.json"))
}

/// Merge the attributing stage meter's stronger footprint/lifetime peaks into the wrapper's
/// per-process rusage records after a successful stage. The wrapper records every successful
/// named compiler; the meter can see a higher compressed-memory lifetime peak. Either instrument
/// may win for a unit, and a recent heavy peak stands against a later light observation.
pub(crate) fn keep_meter_units(
    book: &crate::memory::Book,
    id: crate::plan::StageId,
    units: &Units,
    peaks: &std::collections::BTreeMap<crate::unitgraph::UnitName, u64>,
) -> std::io::Result<usize> {
    if id != crate::plan::StageId::TestCompile {
        return Ok(0);
    }
    let stage = crate::shape::UNIT_STAGE;
    let Some(path) = book.path.as_deref().map(|path| unit_book(path, id)) else {
        return Ok(0);
    };
    if units.stage != stage || units.book != path {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "compiler unit promotion does not match its stage book",
        ));
    }
    // Wrapper-only maxrss is not enough evidence on macOS: compressed memory can make it lower
    // than lifetime physical footprint. A passing stage promotes only the intersection of
    // successful wrapper units and units its attributing meter saw, taking the stronger of the
    // two instruments. A failed stage never calls here, and its run-private pending file can
    // therefore never enable driver-only admission.
    let pending = crate::memory::Records::load(&units.pending);
    let mut proven = std::collections::BTreeMap::new();
    for row in pending
        .rows()
        .iter()
        .filter(|row| valid_unit_row(row, stage))
    {
        let Some([package]) = row.key.crates.as_deref() else {
            continue;
        };
        let Some(kind) = row.key.variant.as_deref() else {
            continue;
        };
        let unit = crate::unitgraph::UnitName {
            package: package.clone(),
            kind: kind.to_owned(),
        };
        let Some(peak) = row.peaks.iter().map(|peak| peak.bytes).max() else {
            continue;
        };
        if let Some(attributed) = peaks.get(&unit) {
            let entry = proven.entry(unit).or_insert(0);
            *entry = (*entry).max(peak.max(*attributed));
        }
    }
    let at_ms = aterm_time::unix_millis().unwrap_or(0);
    let mut kept = 0;
    for (unit, peak) in &proven {
        let key = crate::shape::compiler_unit_key(stage, &units.checkout, unit);
        if crate::memory::Records::keep_unless_light(&path, key, *peak, at_ms)? {
            kept += 1;
        }
    }
    Ok(kept)
}

#[cfg(test)]
pub(crate) fn keep_current_unit(
    unit: &crate::unitgraph::UnitName,
    peak: u64,
) -> Result<Option<PathBuf>, String> {
    UNITS.with(|slot| {
        let units = slot.borrow();
        let Some(units) = units.as_ref() else {
            return Ok(None);
        };
        keep_unit(units, unit, peak)?;
        Ok(Some(units.pending.clone()))
    })
}

/// Remove this stage run's private wrapper observations after its report has either promoted
/// them or refused them. No later run may inherit an unproven maxrss observation.
pub(crate) fn discard_pending(units: &Units) -> std::io::Result<()> {
    match std::fs::remove_file(&units.pending) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn valid_unit_row(row: &crate::memory::Row, stage: &str) -> bool {
    row.key.stage == stage
        && matches!(row.key.crates.as_deref(), Some([package]) if !package.is_empty())
        && row
            .key
            .variant
            .as_deref()
            .is_some_and(|kind| !kind.is_empty())
        && row.peaks.iter().any(|peak| peak.bytes > 0)
}

fn has_units(records: &crate::memory::Records, stage: &str) -> bool {
    records.rows().iter().any(|row| valid_unit_row(row, stage))
}

fn unit_peak(
    records: &crate::memory::Records,
    stage: &str,
    unit: &crate::unitgraph::UnitName,
) -> Option<u64> {
    records
        .rows()
        .iter()
        .filter(|row| {
            valid_unit_row(row, stage)
                && row.key.crates.as_deref() == Some(std::slice::from_ref(&unit.package))
                && row.key.variant.as_deref() == Some(unit.kind.as_str())
        })
        .flat_map(|row| row.peaks.iter().map(|peak| peak.bytes))
        .filter(|peak| *peak > 0)
        .max()
        .map(|peak| peak.max(crate::memory::UNIT_FLOOR))
}

/// Entry for Cargo's rustc wrapper. The marker is removed from the compiler child, so an
/// unrelated invocation of this CLI is never mistaken for a compiler.
#[must_use]
pub fn wrapper_entry(args: &[OsString]) -> Option<u8> {
    entry_on(args, &aterm_capacity::Live, None)
}

fn entry_on(
    args: &[OsString],
    machine: &dyn aterm_capacity::Machine,
    ledger: Option<&aterm_capacity::Ledger>,
) -> Option<u8> {
    std::env::var_os(WRAPPER)?;
    Some(match wrapper(args, machine, ledger) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("{REFUSED}{error}");
            2
        }
    })
}

/// Cargo preserves the wrapper's refusal line even though it replaces its exit code. A real
/// compiler diagnostic still makes the compile red when another unit refused admission.
pub(crate) fn refusal(run: &crate::exec::Run, stdout: &str) -> Option<String> {
    if run.ok || run.hung.is_some() || crate::libtest::has_ceiling_kill(&run.output) {
        return None;
    }
    let compiler_error = stdout
        .lines()
        .filter_map(|line| aterm_json::from_str::<aterm_json::Value>(line).ok())
        .any(|value| {
            value.get("reason").and_then(aterm_json::Value::as_str) == Some("compiler-message")
                && value
                    .get("message")
                    .and_then(|message| message.get("level"))
                    .and_then(aterm_json::Value::as_str)
                    == Some("error")
        });
    if compiler_error {
        return None;
    }
    run.output.lines().find_map(|line| {
        line.strip_prefix(REFUSED)
            .filter(|why| !why.is_empty())
            .map(str::to_owned)
    })
}

fn wrapper(
    args: &[OsString],
    machine: &dyn aterm_capacity::Machine,
    ledger: Option<&aterm_capacity::Ledger>,
) -> Result<u8, String> {
    let units = units_from_environment()?;
    let compiler = args.first().ok_or_else(|| "missing compiler".to_owned())?;
    let mut command = aterm_uds::spawnfd::command(compiler);
    command
        .args(&args[1..])
        .env_remove(WRAPPER)
        .env_remove("RUSTC_WRAPPER");
    for name in UNIT_ENVIRONMENT {
        command.env_remove(name);
    }
    let query = args
        .iter()
        .any(|arg| arg.to_string_lossy().starts_with("--print"));
    let unit = (!query)
        .then(|| {
            crate::memory::unit_of(
                args.iter().map(|s| s.to_string_lossy().into_owned()),
                std::env::var("CARGO_PKG_NAME")
                    .ok()
                    .map(|p| format!("CARGO_PKG_NAME={p}")),
            )
        })
        .flatten();
    let mut claim = None;
    if !query && units.mode == UnitMode::Claim {
        let records = crate::memory::Records::load(&units.book);
        if !has_units(&records, &units.stage) {
            return Err("unit memory history disappeared before the compiler started".to_owned());
        }
        let peak = unit
            .as_ref()
            .and_then(|unit| unit_peak(&records, &units.stage, unit));
        let bytes = peak.map_or(units.unknown, aterm_capacity::peak_claim);
        let owned = ledger
            .is_none()
            .then(aterm_capacity::Ledger::user)
            .flatten();
        let ledger = ledger
            .or(owned.as_ref())
            .ok_or_else(|| "no capacity ledger".to_owned())?;
        let label = unit_label(&units);
        let name = unit.as_ref().map_or_else(
            || "unidentified compiler".to_owned(),
            |unit| format!("{} {}", unit.package, unit.kind),
        );
        let held = admit_unit(
            ledger,
            machine,
            aterm_capacity::Need::new(format!("{label}{name}"))
                .mem(bytes)
                .covers()
                // The ledger excludes ancestor claims from a child's own admission.
                // Leave the driver's envelope spare explicitly on each such decision.
                .margin(DRIVER_CLAIM)
                .ends_when_stuck(crate::STUCK_ENDS),
        )?;
        let width = ledger
            .list()
            .map_err(|e| e.to_string())?
            .iter()
            .filter(|row| row.held.is_some() && row.need.what.starts_with(&label))
            .count();
        eprintln!(
            "compiler width now {width}: admitted {name} with {} (ceiling {})",
            aterm_capacity::size(bytes),
            std::env::var("CARGO_BUILD_JOBS").unwrap_or_else(|_| "pool".to_owned())
        );
        command.env(aterm_capacity::COVER_ENV, held.record().cover_marker());
        claim = Some(held);
    }
    #[cfg(all(
        target_pointer_width = "64",
        any(target_vendor = "apple", target_os = "linux")
    ))]
    let (status, peak) = {
        let mut child = command.spawn().map_err(|e| e.to_string())?;
        let usage = child.wait_with_rusage().map_err(|e| e.to_string())?;
        (usage.status, Some(usage.maxrss))
    };
    #[cfg(not(all(
        target_pointer_width = "64",
        any(target_vendor = "apple", target_os = "linux")
    )))]
    let (status, peak) = (command.status().map_err(|e| e.to_string())?, None);
    drop(claim);
    if status.success()
        && let (Some(unit), Some(peak)) = (&unit, peak)
        && let Err(error) = keep_unit(&units, unit, peak)
    {
        eprintln!("aterm-verify: compiler memory was not kept: {error}");
    }
    Ok(status
        .code()
        .and_then(|c| u8::try_from(c).ok())
        .unwrap_or(1))
}

fn units_from_environment() -> Result<Units, String> {
    let required_path = |name: &str| {
        let path = std::env::var_os(name)
            .map(PathBuf::from)
            .ok_or_else(|| format!("missing {name}"))?;
        path.is_absolute()
            .then_some(path)
            .ok_or_else(|| format!("{name} is not absolute"))
    };
    let required = |name: &str| {
        std::env::var(name)
            .map_err(|_| format!("missing or non-Unicode {name}"))
            .and_then(|value| {
                (!value.is_empty())
                    .then_some(value)
                    .ok_or_else(|| format!("empty {name}"))
            })
    };
    let stage = required(UNIT_STAGE)?;
    if stage != crate::shape::UNIT_STAGE {
        return Err(format!("unknown unit stage {stage:?}"));
    }
    let unknown = required(UNIT_UNKNOWN)?
        .parse::<u64>()
        .ok()
        .filter(|bytes| *bytes >= aterm_capacity::COLD_CLAIM)
        .ok_or_else(|| format!("{UNIT_UNKNOWN} is not a full cold claim"))?;
    Ok(Units {
        book: required_path(UNIT_BOOK)?,
        checkout: required(UNIT_CHECKOUT)?,
        mode: UnitMode::parse(&required(UNIT_MODE)?)?,
        pending: required_path(UNIT_PENDING)?,
        run: required(UNIT_RUN)?,
        stage,
        unknown,
        wrapper: std::env::current_exe().map_err(|e| e.to_string())?,
    })
}

fn unit_label(units: &Units) -> String {
    format!("verify {} compiler unit {} ", units.stage, units.run)
}

fn keep_unit(units: &Units, unit: &crate::unitgraph::UnitName, peak: u64) -> Result<(), String> {
    let at_ms = aterm_time::unix_millis().unwrap_or(0);
    let key = crate::shape::compiler_unit_key(&units.stage, &units.checkout, unit);
    crate::memory::Records::keep_unless_light(
        &units.pending,
        key,
        peak.max(crate::memory::UNIT_FLOOR),
        at_ms,
    )
    .map_err(|e| format!("{}: {e}", units.pending.display()))?;
    Ok(())
}

/// A queued heavy unit must not reserve memory against a ready smaller unit. Each look
/// uses the ordinary ledger atomically, then gives up its queue place while it cannot fit.
/// The separate wait mark keeps the compiler's ceiling and stall accounting informed.
fn admit_unit(
    ledger: &aterm_capacity::Ledger,
    machine: &dyn aterm_capacity::Machine,
    need: aterm_capacity::Need,
) -> Result<aterm_capacity::Claim, String> {
    if let Some(why) = aterm_capacity::never(&need, &machine.read(None)) {
        return Err(format!("{why:?}"));
    }
    let interrupt = aterm_toolchain::interrupt::Watch::enter();
    let mut mark = None;
    let mut said = None;
    loop {
        if let Some(signal) = interrupt.take() {
            return Err(format!(
                "unit wait ended by {}",
                aterm_toolchain::interrupt::name(signal)
            ));
        }
        let waiter = aterm_capacity::Waiter::enter(ledger, machine, need.clone())
            .map_err(|e| e.to_string())?;
        // Two consecutive looks are the ledger's evidence for a stuck holder. Giving
        // up the queue position must not reset that evidence before its second look.
        let polled = match waiter.poll(machine) {
            aterm_capacity::Polled::Waiting(waiter, _) => waiter.poll(machine),
            result => result,
        };
        match polled {
            aterm_capacity::Polled::Admitted(claim) => return Ok(claim),
            aterm_capacity::Polled::Waiting(waiter, short) => {
                let text = format!("{}: {short}", need.what);
                let stuck = waiter.stuck().cloned();
                drop(waiter);
                let now = std::time::Instant::now();
                if said
                    .is_none_or(|last: std::time::Instant| now.duration_since(last).as_secs() >= 60)
                {
                    eprintln!("{text}");
                    said = Some(now);
                }
                if let Some(mark) = &mut mark {
                    aterm_toolchain::waits::Mark::say_stuck(mark, &text, stuck.as_ref());
                } else {
                    mark = aterm_toolchain::waits::Mark::begin(&text);
                }
                if stuck.is_some() {
                    return Err(format!("unit holders made no progress: {text}"));
                }
            }
            aterm_capacity::Polled::Gone(error) => return Err(error.to_string()),
            aterm_capacity::Polled::Unqualified(issue) => return Err(format!("{issue:?}")),
        }
        interrupt.nap(std::time::Duration::from_millis(250), &|| false);
    }
}

#[cfg(test)]
#[path = "compile_width_tests.rs"]
mod tests;
