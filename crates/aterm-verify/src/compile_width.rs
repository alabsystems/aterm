// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Compiler memory belongs to each running unit, not to an immutable Cargo width.

use std::cell::{Cell, RefCell};
use std::ffi::OsString;
use std::path::PathBuf;

const UNIT_BOOK: &str = "ATERM_VERIFY_UNIT_BOOK";
const UNIT_RUN: &str = "ATERM_VERIFY_UNIT_RUN";
const WRAPPER: &str = "ATERM_VERIFY_UNIT_WRAPPER";
const UNIT_LABEL: &str = "verify compiler unit ";

const REFUSED: &str = "aterm-verify: compiler admission: ";

#[derive(Clone)]
struct Units {
    book: PathBuf,
    run: String,
    wrapper: PathBuf,
}

thread_local! {
    static JOBS: Cell<Option<u32>> = const { Cell::new(None) };
    static UNITS: RefCell<Option<Units>> = const { RefCell::new(None) };
    static FLAGS: RefCell<Option<String>> = const { RefCell::new(None) };
}

pub(crate) fn current() -> Option<u32> {
    JOBS.with(Cell::get)
}

/// Every compiling stage joins the release FIFO, including stages whose memory admission
/// keeps a whole-stage envelope. Unit memory admission is independent of core accounting.
pub(crate) fn stage_jobs(
    stage: crate::plan::StageId,
    pool_slots: Option<u32>,
    ceiling: Option<u32>,
) -> Option<u32> {
    pool_slots
        .filter(|_| crate::memory::compiles(stage))
        .map(|slots| ceiling.map_or(slots, |ceiling| ceiling.min(slots)).max(1))
}

pub(crate) fn jobs_for(ctx: &crate::Ctx, stage: crate::plan::StageId) -> Option<u32> {
    stage_jobs(
        stage,
        ctx.pool.as_ref().map(aterm_capacity::Jobserver::slots),
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

/// Give every Cargo in this stage the same pool and the unit admission wrapper. Resolution
/// queries reach the real compiler directly; only named compiler units claim memory.
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
            command
                .env("RUSTC_WRAPPER", &units.wrapper)
                .env(WRAPPER, "1")
                .env(UNIT_BOOK, &units.book)
                .env(UNIT_RUN, &units.run);
        }
    });
}

pub(crate) fn run<T>(
    pool: Option<&aterm_capacity::Jobserver>,
    jobs: Option<u32>,
    stopped: &dyn Fn() -> bool,
    run: impl FnOnce() -> T,
) -> std::io::Result<T> {
    struct Restore(Option<String>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = FLAGS.with(|slot| slot.replace(self.0.take()));
            let _ = UNITS.with(|slot| slot.replace(None));
        }
    }
    let _restore = Restore(FLAGS.with(|slot| slot.borrow().clone()));
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

pub(crate) fn claim(
    ctx: &crate::Ctx,
    spec: &crate::plan::StageSpec,
    wait: crate::Admitter,
) -> Result<Option<crate::ResourceClaim>, String> {
    let Some(pool) = &ctx.pool else {
        return Ok(None);
    };
    if spec.id != crate::plan::StageId::TestCompile || ctx.background {
        return Ok(None);
    }
    let Some(book) = &ctx.memory else {
        return Ok(None);
    };
    let _ = UNITS.with(|slot| slot.replace(None));
    let records = book.records();
    let measured = has_units(&records);
    let jobs = ctx
        .build_jobs_ceiling()
        .unwrap_or(pool.slots())
        .min(pool.slots());
    // No unit history means no evidence for a smaller memory envelope.
    let bytes = if measured {
        aterm_capacity::peak_claim(crate::memory::UNIT_FLOOR)
    } else {
        spec.mem
    };
    let claim = wait(
        aterm_capacity::Need::new(format!("verify {}", spec.title))
            .mem(bytes)
            .covers()
            .ends_when_stuck(crate::STUCK_ENDS)
            .margin(1),
        aterm_capacity::Admit::Wait,
    )
    .map_err(|e| e.to_string())?;
    if measured {
        let path = book
            .path
            .clone()
            .ok_or_else(|| "unit records have no backing book".to_owned())?;
        let wrapper = std::env::current_exe().map_err(|e| e.to_string())?;
        let _ = UNITS.with(|slot| {
            slot.replace(Some(Units {
                book: path,
                run: claim.record().cover_marker(),
                wrapper,
            }))
        });
    }
    Ok(Some(crate::ResourceClaim {
        claim: std::sync::Arc::new(claim),
        said: format!(
            "memory: claimed {} for {}; compiler width ceiling {jobs}, active width logged at each unit admission",
            aterm_capacity::size(bytes),
            if measured {
                "the compiler driver; each unit claims its own envelope"
            } else {
                "the full compile (no unit history)"
            },
        ),
    }))
}

fn has_units(records: &crate::memory::Records) -> bool {
    records
        .rows()
        .iter()
        .any(|row| row.key.stage == crate::shape::UNIT_STAGE && !row.peaks.is_empty())
}

fn unit_peak(records: &crate::memory::Records, unit: &crate::unitgraph::UnitName) -> u64 {
    records
        .rows()
        .iter()
        .filter(|row| {
            row.key.stage == crate::shape::UNIT_STAGE
                && row.key.crates.as_deref() == Some(std::slice::from_ref(&unit.package))
                && row.key.variant.as_deref() == Some(unit.kind.as_str())
        })
        .flat_map(|row| row.peaks.iter().map(|peak| peak.bytes))
        .max()
        .unwrap_or(crate::memory::UNIT_FLOOR)
        .max(crate::memory::UNIT_FLOOR)
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
    let compiler = args.first().ok_or_else(|| "missing compiler".to_owned())?;
    let mut command = aterm_uds::spawnfd::command(compiler);
    command
        .args(&args[1..])
        .env_remove(WRAPPER)
        .env_remove("RUSTC_WRAPPER");
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
    if let Some(unit) = unit {
        let book = std::env::var_os(UNIT_BOOK).ok_or_else(|| "missing unit book".to_owned())?;
        let run = std::env::var(UNIT_RUN).map_err(|e| e.to_string())?;
        let records = crate::memory::Records::load(std::path::Path::new(&book));
        if !has_units(&records) {
            return Err("unit memory history disappeared before the compiler started".to_owned());
        }
        let bytes = aterm_capacity::peak_claim(unit_peak(&records, &unit));
        let owned = ledger
            .is_none()
            .then(aterm_capacity::Ledger::user)
            .flatten();
        let ledger = ledger
            .or(owned.as_ref())
            .ok_or_else(|| "no capacity ledger".to_owned())?;
        let label = format!("{UNIT_LABEL}{run} ");
        let held = admit_unit(
            ledger,
            machine,
            aterm_capacity::Need::new(format!("{label}{} {}", unit.package, unit.kind))
                .mem(bytes)
                .covers()
                // The ledger excludes ancestor claims from a child's own admission.
                // Leave the driver's envelope spare explicitly on each such decision.
                .margin(aterm_capacity::peak_claim(crate::memory::UNIT_FLOOR))
                .ends_when_stuck(crate::STUCK_ENDS),
        )?;
        let width = ledger
            .list()
            .map_err(|e| e.to_string())?
            .iter()
            .filter(|row| row.held.is_some() && row.need.what.starts_with(&label))
            .count();
        eprintln!(
            "compiler width now {width}: admitted {} {} with {} (ceiling {})",
            unit.package,
            unit.kind,
            aterm_capacity::size(bytes),
            std::env::var("CARGO_BUILD_JOBS").unwrap_or_else(|_| "pool".to_owned())
        );
        command.env(aterm_capacity::COVER_ENV, held.record().cover_marker());
        claim = Some(held);
    }
    let status = command.status().map_err(|e| e.to_string())?;
    drop(claim);
    Ok(status
        .code()
        .and_then(|c| u8::try_from(c).ok())
        .unwrap_or(1))
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
