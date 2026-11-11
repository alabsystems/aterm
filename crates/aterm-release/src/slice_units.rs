// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! BOTH RELEASE SLICES SPEND ONE MEASURED MEMORY BUDGET.
//!
//! Records distinguish target, unit, compiler build and accounting metric. Darwin combines
//! sampled simultaneous descendant footprints with the compiler's exact kernel lifetime maximum
//! before reaping; RSS remains timeline evidence, never footprint history. Each retained peak
//! gains the capacity margin and GiB rounding. With both final links recorded for the current
//! toolchains, or for a prior compiler under the same accounting metric, the release admits one
//! parent holding the largest compatible unit claim plus every live driver before the qualify
//! tree. Both slices then allocate from a private ledger of exactly that unit budget, so tree work
//! cannot take link memory and two links cannot oversubscribe it.
//! After a bounded normal-pressure shortage with no claimed blocker, the cut can serialize
//! the slices and spend part of the compiler margin on its single driver. The private pool
//! and every compiler claim remain unchanged; this plan refuses unknown units before spawn.
//!
//! An unknown or unidentified compiler consumes the whole private budget for its learning run.
//! With no compatible final-link history, each slice instead learns under an exclusive outer
//! claim; no fixed link estimate or incompatible accounting metric supplies an unmeasured
//! envelope. Prior compiler rows provision admission only; a successful run retains the new exact
//! identity. See
//! `docs/measured/qualify-slice-reservation-2026-11-09.md` and
//! `docs/measured/slice-memory-claim-2026-11-10.md`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The slices' compiler-unit record, under this account's aterm state directory.
pub(crate) const RECORD: &str = "release/slice-unit-memory.json";

/// The least peak a previously unknown unit's record starts from: 1 GiB. A unit once kept keeps
/// every later measured peak, so one grown lighter forgets its old peak after
/// [`aterm_capacity::book::KEPT`] runs.
pub(crate) const FLOOR: u64 = 1 << 30;

/// What one slice driver adds beside the shared compiler-unit budget: Cargo, its build scripts
/// and wrappers.
pub(crate) const DRIVER: u64 = aterm_capacity::peak_claim(FLOOR);

/// The smallest complete-history compiler budget: one margined [`FLOOR`] unit.
pub(crate) const MIN_BUDGET: u64 = DRIVER;

/// The smallest structurally valid two-slice parent. The live detector uses the current record's
/// exact larger need whenever complete history exists.
pub(crate) const MIN_SHARED: u64 = MIN_BUDGET + 2 * DRIVER;

/// The record's kind of work.
const STAGE: &str = "release slice unit";

/// The wrapper's variables: the record's path (which makes this executable a wrapper), the
/// slice's triple and, where its units share one budget, that private ledger and its bytes.
const WRAPPER: &str = "ATERM_RELEASE_UNIT_RECORD";
const TRIPLE: &str = "ATERM_RELEASE_UNIT_TRIPLE";
const BUDGET_LEDGER: &str = "ATERM_RELEASE_UNIT_BUDGET_LEDGER";
const BUDGET_BYTES: &str = "ATERM_RELEASE_UNIT_BUDGET_BYTES";
/// The slice's source root, whose `trust.lock` names its admission estimates ([`toolchain_key`]). Cargo
/// starts a registry package's compiler in that package's own directory, where no lock is
/// found, so the parent names the root rather than each wrapper reading its working directory.
const ROOT: &str = "ATERM_RELEASE_UNIT_ROOT";

/// The line a refused wrapper prints, which Cargo keeps in the slice's log.
const REFUSED: &str = "aterm-release: slice unit admission: ";

/// One build's private compiler budget, shared by both slices.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Budget {
    ledger: PathBuf,
    bytes: u64,
    driver_in_margin: bool,
}

/// Native release links in isolation, without executing the product or
/// constructing a release. Distinct leaf metadata forces each link while
/// reusing dependencies. Each take prints the learned production claim.
pub(crate) fn measure(root: &Path, takes: u8) -> Result<(), String> {
    if !cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        return Err(
            "slice-memory currently measures the native Apple Silicon release slice".into(),
        );
    }
    let triple = "aarch64-apple-darwin";
    let wrapper = std::env::current_exe().map_err(|e| e.to_string())?;
    let driver = crate::gates::trust_stage2_bin()
        .map_err(|e| e.to_string())?
        .join("targo");
    let nonce = std::process::id();
    let budget_ledger = root.join("target/.ubd/slice-memory-measure-ledger.noindex");
    for take in 1..=takes {
        let mut units = Units::live(&[triple], 1, &budget_ledger)
            .into_iter()
            .next()
            .ok_or("no account state directory for slice memory")?;
        let records = aterm_capacity::book::Records::load(&units.record);
        let before = unit_need(triple, &records, &last_unit(), DRIVER);
        eprintln!(
            "{}",
            aterm_json::json!({"event":"slice-memory-take", "take":take,
            "record":units.record.display().to_string(), "measured":units.budget.is_some(),
            "claim_bytes":units.budget.as_ref().map(|_| before.mem),
            "memory_identity":toolchain_key(triple)})
        );
        let _claim = aterm_capacity::admit(slice_need(triple, None), aterm_capacity::Admit::Wait)
            .map_err(|e| e.to_string())?;
        // The experiment leaves every other memory claimant out, including
        // while measuring a known unit whose ordinary claim cannot fit beside
        // unclaimed machine load. The production claim is still printed above.
        units.budget = None;
        let mut command = aterm_uds::spawnfd::command(&driver);
        command
            .current_dir(root)
            .args([
                "--unverified",
                "rustc",
                "--locked",
                "--release",
                "-p",
                "aterm",
                "--bin",
                "aterm",
                "--",
            ])
            .arg(format!("-Cmetadata=slice-memory-{nonce}-{take}"))
            .env("CARGO_PROFILE_RELEASE_DEBUG", "1")
            .env(
                "CARGO_TARGET_DIR",
                root.join("target/.ubd/slice-memory.noindex"),
            )
            .env("CARGO_PROFILE_RELEASE_STRIP", "false")
            .env("ATERM_STAMP", "postlink")
            .env("SOURCE_DATE_EPOCH", "0");
        for (name, value) in units.env(&wrapper, root) {
            command.env(name, value);
        }
        let status = command.status().map_err(|e| e.to_string())?;
        if !status.success() {
            return Err(format!("slice memory take {take}: {status}"));
        }
        let records = aterm_capacity::book::Records::load(&units.record);
        let identity = compiler_identity(&[driver.with_file_name("trustc").into_os_string()]);
        let peak = peak_for_key(&records, &measured_key(triple, &last_unit(), &identity))
            .ok_or("link completed without a physical-footprint record")?;
        let after = unit_need(triple, &records, &last_unit(), DRIVER);
        eprintln!(
            "{}",
            aterm_json::json!({"event":"slice-memory-result", "take":take,
            "peak_bytes":peak, "next_claim_bytes":after.mem})
        );
    }
    Ok(())
}

/// One slice's units: the record they keep, its triple and their optional shared private budget.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Units {
    record: PathBuf,
    triple: String,
    budget: Option<Budget>,
}

impl Units {
    /// The units of the slices one build runs, `triples`, at most `at_once` at once, kept at
    /// `record`. Where every slice's final link has an exact or same-metric prior peak, they share
    /// `budget_ledger`; otherwise their wrappers only record while the caller holds an exclusive
    /// learning claim.
    pub(crate) fn of(
        record: &Path,
        triples: &[&str],
        at_once: u64,
        budget_ledger: &Path,
    ) -> Vec<Units> {
        let records = aterm_capacity::book::Records::load(record);
        let bounded = at_once > 0
            && triples
                .iter()
                .all(|triple| admission_peak(&records, triple, &last_unit()).is_some());
        let budget = bounded.then(|| Budget {
            ledger: budget_ledger.to_owned(),
            bytes: retained_budget(&records, triples),
            driver_in_margin: false,
        });
        triples
            .iter()
            .map(|triple| Units {
                record: record.to_owned(),
                triple: (*triple).to_owned(),
                budget: budget.clone(),
            })
            .collect()
    }

    /// The units of `triples`' slices on this account ([`RECORD`], [`Units::of`]); none with no
    /// state directory.
    pub(crate) fn live(triples: &[&str], at_once: u64, budget_ledger: &Path) -> Vec<Units> {
        aterm_types::dirs::account_state_dir()
            .map(|dir| Units::of(&dir.join(RECORD), triples, at_once, budget_ledger))
            .unwrap_or_default()
    }

    /// The slice's triple.
    pub(crate) fn triple(&self) -> &str {
        &self.triple
    }

    /// The private compiler-unit budget, identical for every slice in this build; none while the
    /// retained record is incomplete and exclusive learning remains active.
    pub(crate) fn budget_bytes(&self) -> Option<u64> {
        self.budget.as_ref().map(|budget| budget.bytes)
    }

    /// What Cargo's environment gains so that `wrapper`, this executable, starts each compiler
    /// of a build whose source root is `root`.
    pub(crate) fn env(&self, wrapper: &Path, root: &Path) -> Vec<(&'static str, OsString)> {
        let mut env = vec![
            ("RUSTC_WRAPPER", wrapper.as_os_str().to_os_string()),
            (WRAPPER, self.record.clone().into_os_string()),
            (TRIPLE, self.triple.clone().into()),
            (ROOT, root.as_os_str().to_os_string()),
        ];
        if let Some(budget) = &self.budget {
            env.push((BUDGET_LEDGER, budget.ledger.clone().into_os_string()));
            env.push((BUDGET_BYTES, budget.bytes.to_string().into()));
            if budget.driver_in_margin {
                env.push((DRIVER_IN_MARGIN, "1".into()));
            }
        }
        env
    }
}

pub(crate) const SERIAL_PARENT: &str = "release serial slice memory budget";
const DRIVER_IN_MARGIN: &str = "ATERM_RELEASE_DRIVER_IN_MARGIN";

/// A serial parent can spend part of the compiler claims' existing 20% margin on its
/// one driver. Every known unit's raw retained peak is at most 5/6 of its rounded claim;
/// therefore the sum of simultaneous peaks is at most 5/6 of the unchanged private pool.
/// Keep the driver and at least one GiB beside that bound. An unmeasured unit has no such
/// bound, so it claims the whole private pool ([`unit_need`]'s unknown claim) and compiles
/// alone: its learning run assumes only that it peaks below the parent less the driver and
/// that GiB, as the shared plan's learning claim assumes it fits the private pool. Its
/// measured peak is retained, so the next build admits it by the bound above.
pub(crate) fn serial_parent_bytes(budget: u64) -> u64 {
    // Each claim is at least one GiB. Allow one byte per possible simultaneous
    // claim for integer division before summing the per-unit 5/6 bounds.
    // At least one unit is live, so the aggregate also leaves the margin's one-GiB floor.
    // The parent reserves the pages that run, not the private pool's claims: the pool keeps its
    // full size inside it ([`BudgetMachine`]), so no claim shrinks.
    let peaks = budget
        .saturating_sub(budget / (aterm_capacity::PEAK_MARGIN_DIV + 1))
        .saturating_add(budget / FLOOR)
        .min(budget.saturating_sub(aterm_capacity::PEAK_MARGIN_MIN));
    peaks
        .saturating_add(DRIVER)
        .saturating_add(aterm_capacity::PEAK_MARGIN_MIN)
}

pub(crate) fn serial_need(units: &[Units]) -> Option<aterm_capacity::Need> {
    let budget = units.first()?.budget.as_ref()?;
    shared_need(units, 1)?;
    Some(
        aterm_capacity::Need::new(SERIAL_PARENT)
            .rank(aterm_capacity::Rank::Release)
            .mem(serial_parent_bytes(budget.bytes))
            .margin(1),
    )
}

pub(crate) fn use_serial_margin(units: &mut [Units]) {
    for unit in units {
        if let Some(budget) = &mut unit.budget {
            budget.driver_in_margin = true;
        }
    }
}

/// The complete parent reservation for the slices' shared memory pool: its compiler budget and
/// one driver allowance for each slice that can run at once. `None` selects exclusive learning
/// where the retained exact-or-compatible record is incomplete or inconsistent.
pub(crate) fn shared_need(units: &[Units], at_once: u64) -> Option<aterm_capacity::Need> {
    let budget = units.first()?.budget.as_ref()?;
    if at_once == 0
        || units
            .iter()
            .any(|units| units.budget.as_ref() != Some(budget))
    {
        return None;
    }
    Some(
        aterm_capacity::Need::new("release slice memory budget")
            .rank(aterm_capacity::Rank::Release)
            .mem(budget.bytes.saturating_add(DRIVER.saturating_mul(at_once)))
            // An unclaimed load cannot turn a memory shortage into ALONE admission.
            .margin(1),
    )
}

/// The least bounded parent this account's retained slice record requires.
/// The admission detector reads this before releasing compiler tokens, so a new measured peak
/// cannot be authenticated by a stale smaller parent. With no compatible final-link history no
/// shared parent is valid, represented by the unreachable maximum.
pub(crate) fn live_shared_floor(at_once: u64) -> u64 {
    const TRIPLES: &[&str] = &["aarch64-apple-darwin", "x86_64-apple-darwin"];
    let units = Units::live(TRIPLES, at_once, Path::new("unused-private-ledger"));
    shared_need(&units, at_once)
        .map(|need| need.mem)
        .unwrap_or(u64::MAX)
}

/// A slice with a bounded shared parent spends only its driver here. A slice with no compatible
/// final-link history learns under an exclusive outer claim; an unwrapped diagnostic does too.
pub(crate) fn slice_need(triple: &str, units: Option<&Units>) -> aterm_capacity::Need {
    let need = aterm_capacity::Need::new(format!("release slice {triple}"))
        .rank(aterm_capacity::Rank::Release)
        // An unclaimed load cannot turn a memory shortage into admission ALONE.
        .margin(1);
    if units.is_some_and(|units| units.budget.is_some()) {
        need.mem(DRIVER)
    } else {
        need.mem(DRIVER).alone()
    }
}

/// The slice's last unit, whose record makes its units claim.
fn last_unit() -> aterm_verify::unitgraph::UnitName {
    aterm_verify::unitgraph::UnitName {
        package: "aterm".to_owned(),
        kind: "aterm bin".to_owned(),
    }
}

fn measured_key(
    triple: &str,
    unit: &aterm_verify::unitgraph::UnitName,
    identity: &str,
) -> aterm_capacity::book::Key {
    aterm_capacity::book::Key {
        stage: STAGE.to_owned(),
        // A release builds from a fresh clone, so no checkout names a slice's units.
        checkout: String::new(),
        crates: Some(vec![unit.package.clone()]),
        variant: Some(format!("{triple} {} {identity}", unit.kind)),
    }
}

/// The requested build identifies historical admission estimates. Measurements instead
/// identify the executable that ran ([`compiler_identity`]). Old RSS records deliberately
/// do not match physical-footprint records.
fn toolchain_key(triple: &str) -> String {
    let lock = lock_root()
        .and_then(|dir| aterm_toolchain::trust_lock::find(&dir))
        .and_then(|found| found.lock.ok());
    let metric = accounting_name();
    match lock {
        Some(lock) if triple == "x86_64-apple-darwin" => {
            format!("{metric} stock-{}", lock.stock_toolchain)
        }
        Some(lock) => format!("{metric} trust-{}-{}", lock.build, lock.tree_root),
        None => format!("{metric} unknown"),
    }
}

fn accounting_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "phys-footprint-tree-v1"
    } else {
        "maxrss-v1"
    }
}

/// The lock supplies admission estimates, not evidence of the executable Cargo chose.
/// Host units and compat units use the same probe of their actual compiler. An executable
/// that cannot identify itself stays unknown rather than inheriting the requested lock.
fn compiler_identity(args: &[OsString]) -> String {
    let version = (|| {
        // Cargo may name a workspace wrapper followed by rustc. Keep that prefix and
        // all selectors (including --sysroot): wrappers can select rustc by its arguments.
        // rustc answers -vV without compiling even when compile arguments are present.
        let mut command = compiler(args).ok()?;
        command.arg("-vV");
        aterm_uds::deadline::output_until_complete_stdout_with_limit(
            &mut command,
            std::time::Instant::now() + std::time::Duration::from_secs(60),
            64 * 1024,
        )
        .ok()
        .filter(|out| out.status.success())
    })();
    let metric = accounting_name();
    match version {
        Some(out)
            if crate::buildplan::commit_hash_of(&String::from_utf8_lossy(&out.stdout))
                .is_some() =>
        {
            let digest = aterm_codec::hex::encode_lower(
                ring::digest::digest(&ring::digest::SHA256, &out.stdout).as_ref(),
            );
            format!("{metric} compiler-vv-{digest}")
        }
        _ => format!("{metric} unknown"),
    }
}

/// Where the toolchain lock is found: a wrapper's [`ROOT`], else this process's working
/// directory (the parent's, which starts no compiler itself).
fn lock_root() -> Option<PathBuf> {
    std::env::var_os(ROOT)
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
}

fn peak_for_key(
    records: &aterm_capacity::book::Records,
    key: &aterm_capacity::book::Key,
) -> Option<u64> {
    records
        .rows()
        .iter()
        .find(|row| &row.key == key)
        .and_then(|row| row.peaks.iter().map(|p| p.bytes).max())
}

/// Apply the shared capacity margin to a physical peak and round it up to a GiB once.
fn claim_bytes(peak: u64) -> u64 {
    aterm_capacity::peak_claim(peak.max(FLOOR))
        .div_ceil(FLOOR)
        .saturating_mul(FLOOR)
}

/// The current accounting metric. A missing toolchain lock cannot authenticate an old row.
fn accounting_metric(triple: &str) -> Option<String> {
    let identity = toolchain_key(triple);
    (!identity.ends_with(" unknown"))
        .then(|| identity.split_whitespace().next().map(str::to_owned))
        .flatten()
}

/// The largest retained peak for this exact target, package and unit under the current
/// accounting metric, across compiler identities. Admission can use older observations while
/// each successful compile still records its peak under its exact current identity.
fn admission_peak(
    records: &aterm_capacity::book::Records,
    triple: &str,
    unit: &aterm_verify::unitgraph::UnitName,
) -> Option<u64> {
    let metric = accounting_metric(triple)?;
    let prefix = format!("{triple} {} ", unit.kind);
    records
        .rows()
        .iter()
        .filter(|row| {
            row.key.stage == STAGE
                && row.key.checkout.is_empty()
                && row.key.crates.as_deref() == Some(std::slice::from_ref(&unit.package))
                && row.key.variant.as_deref().is_some_and(|variant| {
                    variant.strip_prefix(&prefix).is_some_and(|identity| {
                        identity.split_whitespace().next() == Some(metric.as_str())
                    })
                })
        })
        .flat_map(|row| row.peaks.iter().map(|peak| peak.bytes))
        .filter(|peak| *peak > 0)
        .max()
}

fn compatible_variant(triple: &str, variant: &str) -> bool {
    let Some(metric) = accounting_metric(triple) else {
        return false;
    };
    variant.starts_with(&format!("{triple} ")) && variant.contains(&format!(" {metric} "))
}

/// The private unit pool kept by `records`: the largest margined claim retained for any
/// configured slice under its current accounting metric. A prior compiler can provision this
/// conservative bound; stale RSS or another metric cannot authenticate or inflate it.
fn retained_budget(records: &aterm_capacity::book::Records, triples: &[&str]) -> u64 {
    records
        .rows()
        .iter()
        .filter(|row| {
            row.key.stage == STAGE
                && row.key.checkout.is_empty()
                && row.key.variant.as_deref().is_some_and(|variant| {
                    triples
                        .iter()
                        .any(|triple| compatible_variant(triple, variant))
                })
        })
        .flat_map(|row| row.peaks.iter().map(|peak| claim_bytes(peak.bytes)))
        .max()
        .unwrap_or(0)
        .max(MIN_BUDGET)
}

/// What `unit`'s compiler in `triple`'s slice spends from the private pool: its largest
/// same-metric retained peak with the shared margin and GiB rounding. A unit with no such
/// row takes `unknown`, the exact whole private compiler budget, until its first successful run
/// records a peak. Every slice unit leads at Release rank and the final link is terminal-critical,
/// but it asks no further margin: the admitted parent owns these bytes.
pub(crate) fn unit_need(
    triple: &str,
    records: &aterm_capacity::book::Records,
    unit: &aterm_verify::unitgraph::UnitName,
    unknown: u64,
) -> aterm_capacity::Need {
    let bytes = admission_peak(records, triple, unit).map_or(unknown, claim_bytes);
    let need = aterm_capacity::Need::new(format!(
        "release slice {triple} unit {} {}",
        unit.package, unit.kind
    ))
    .mem(bytes)
    .rank(aterm_capacity::Rank::Release)
    .leads();
    if *unit == last_unit() {
        need.critical_path()
    } else {
        need
    }
}

/// The private ledger's machine: its memory is the fixed compiler budget, while the live process
/// table still proves claim holders alive or gone. Process footprints are deliberately zero in
/// this accounting domain: the parent reservation owns the real pages already, and each private
/// claim must retain its complete declared share rather than receiving host-memory credit.
#[derive(Clone, Copy, Debug)]
struct BudgetMachine {
    bytes: u64,
}

impl BudgetMachine {
    const fn new(bytes: u64) -> BudgetMachine {
        BudgetMachine { bytes }
    }
}

impl aterm_capacity::Machine for BudgetMachine {
    fn read(&self, _path: Option<&Path>) -> aterm_capacity::Reading {
        aterm_capacity::Reading {
            cores: 1,
            load1: Some(0.0),
            load5: Some(0.0),
            available: Some(self.bytes),
            // A need equal to the budget is admissible, not an impossible request for every
            // reachable byte of a physical host.
            physical: Some(self.bytes.saturating_add(1)),
            wired: None,
            swap_used: Some(0),
            swap_total: Some(0),
            volume: None,
            pressure: None,
            gpu_busy: None,
        }
    }

    fn procs(&self) -> Option<aterm_capacity::Procs> {
        let sweep = aterm_sysprobe::processes()?;
        Some(aterm_capacity::Procs::of(
            sweep
                .procs
                .into_iter()
                .map(|process| {
                    let reaped = process.child_cpu_ns.unwrap_or(0);
                    aterm_capacity::Proc {
                        pid: process.pid,
                        ppid: process.ppid,
                        footprint: 0,
                        charged: 0,
                        written: process.disk_written.unwrap_or(0),
                        cpu_ns: process.cpu_ns.unwrap_or(0).saturating_add(reaped),
                        runnable_ns: process.runnable_ns.unwrap_or(0).saturating_add(reaped),
                    }
                })
                .collect(),
        ))
    }

    fn lineage(&self, pid: u32) -> Option<aterm_capacity::Procs> {
        aterm_capacity::Machine::lineage(&aterm_capacity::Live, pid)
    }

    fn waits(&self) -> Vec<aterm_toolchain::waits::Waiting> {
        aterm_capacity::Machine::waits(&aterm_capacity::Live)
    }
}

fn budget_from_environment() -> Result<Option<Budget>, String> {
    let ledger = std::env::var_os(BUDGET_LEDGER);
    let bytes = std::env::var_os(BUDGET_BYTES);
    match (ledger, bytes) {
        (None, None) => Ok(None),
        (Some(ledger), Some(bytes)) => {
            let bytes = bytes
                .to_str()
                .and_then(|bytes| bytes.parse::<u64>().ok())
                .filter(|bytes| *bytes >= MIN_BUDGET)
                .ok_or("the private unit budget is invalid")?;
            Ok(Some(Budget {
                ledger: ledger.into(),
                bytes,
                driver_in_margin: std::env::var_os(DRIVER_IN_MARGIN).is_some(),
            }))
        }
        _ => Err("the private unit budget is incomplete".to_owned()),
    }
}

/// Entry for Cargo's rustc wrapper: `None` unless this process is one ([`WRAPPER`]); else the
/// compiler's exit code, or 2 where its unit could not claim its memory.
#[must_use]
pub(crate) fn entry(args: &[OsString]) -> Option<u8> {
    let record = std::env::var_os(WRAPPER)?;
    if std::env::var_os(ROOT).is_none() {
        eprintln!("{REFUSED}the wrapper's environment names no source root");
        return Some(2);
    }
    let budget = match budget_from_environment() {
        Ok(budget) => budget,
        Err(why) => {
            eprintln!("{REFUSED}{why}");
            return Some(2);
        }
    };
    let units = Units {
        record: record.into(),
        triple: std::env::var(TRIPLE).unwrap_or_default(),
        budget,
    };
    let package = std::env::var("CARGO_PKG_NAME").ok();
    let run = || {
        let ledger = units
            .budget
            .as_ref()
            .map(|budget| aterm_capacity::Ledger::at(&budget.ledger));
        let machine = units
            .budget
            .as_ref()
            .map(|budget| BudgetMachine::new(budget.bytes));
        wrap(
            args,
            package.as_deref(),
            &units,
            ledger
                .as_ref()
                .zip(machine.as_ref())
                .map(|(ledger, machine)| (ledger, machine as &dyn aterm_capacity::Machine)),
        )
    };
    Some(run().unwrap_or_else(|why| {
        eprintln!("{REFUSED}{why}");
        2
    }))
}

/// The compiler Cargo asked for, `args[0]` with the rest of `args`, its environment the
/// wrapper's less the wrapper's own variables and `RUSTC_WRAPPER`. It is a pass-through
/// ([`aterm_uds::spawnfd::transparent`]): it inherits the descriptors Cargo gave the wrapper,
/// the jobserver's pipes among them where Cargo made its own jobserver
/// (`--jobserver-fds=4,6`).
pub(crate) fn compiler(args: &[OsString]) -> Result<aterm_uds::spawnfd::Command, String> {
    let program = args.first().ok_or("missing compiler")?;
    #[cfg(unix)]
    let mut command = aterm_uds::spawnfd::transparent(program).into_command();
    #[cfg(not(unix))]
    let mut command = aterm_uds::spawnfd::transparent(program);
    command.args(&args[1..]);
    for name in [
        WRAPPER,
        TRIPLE,
        BUDGET_LEDGER,
        BUDGET_BYTES,
        DRIVER_IN_MARGIN,
        ROOT,
        "RUSTC_WRAPPER",
    ] {
        command.env_remove(name);
    }
    Ok(command)
}

/// Run the compiler `args` names for the unit it compiles in `package`
/// ([`aterm_verify::memory::unit_of`]): under its own private-budget claim where `units` claim,
/// and its peak kept where it finishes successfully. A query (`-vV`, `--print`)
/// compiles nothing and runs at once. A unit larger than its admitted parent budget is refused;
/// it cannot turn an under-reservation into an ALONE run outside that parent.
fn wrap(
    args: &[OsString],
    package: Option<&str>,
    units: &Units,
    admission: Option<(&aterm_capacity::Ledger, &dyn aterm_capacity::Machine)>,
) -> Result<u8, String> {
    let mut command = compiler(args)?;
    let words: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let query = words.iter().any(|w| w.starts_with("--print") || w == "-vV");
    let unit = (!query)
        .then(|| {
            aterm_verify::memory::unit_of(
                words.iter().cloned(),
                package.map(|p| format!("CARGO_PKG_NAME={p}")),
            )
        })
        .flatten();
    let queued = timestamp();
    let claim = match (query, &units.budget, admission) {
        (true, _, _) | (false, None, _) => None,
        (false, Some(_), None) => {
            return Err("the private unit budget has no admission ledger".to_owned());
        }
        (false, Some(budget), Some((ledger, machine))) => {
            let records = aterm_capacity::book::Records::load(&units.record);
            // Recheck against the slice's source root. A snapshot can carry a different lock
            // from its orchestration checkout, or the book can have changed since the parent
            // chose its budget.
            if admission_peak(&records, &units.triple, &last_unit()).is_none() {
                return Err(
                    "no compatible footprint for the final link; rerun the slice alone to learn it"
                        .into(),
                );
            }
            let required = retained_budget(&records, &[units.triple.as_str()]);
            if required > budget.bytes {
                return Err(format!(
                    "the retained slice footprint now requires {}, beyond the admitted {} parent budget",
                    aterm_capacity::size(required),
                    aterm_capacity::size(budget.bytes)
                ));
            }
            let need = unit.as_ref().map_or_else(
                || {
                    aterm_capacity::Need::new(format!(
                        "release slice {} unidentified compiler",
                        units.triple
                    ))
                    .mem(budget.bytes)
                },
                |unit| unit_need(&units.triple, &records, unit, budget.bytes),
            );
            let bytes = need.mem;
            let claim = aterm_capacity::admit_with(
                ledger,
                machine,
                need,
                aterm_capacity::Admit::Wait,
                aterm_capacity::Pace::default(),
                &mut |line| eprintln!("{line}"),
            )
            .map_err(|e| e.to_string())?;
            if let Some(unit) = &unit {
                eprintln!(
                    "==> [{}] admitted {} {} with {} from the shared slice budget",
                    units.triple,
                    unit.package,
                    unit.kind,
                    aterm_capacity::size(bytes),
                );
            } else {
                eprintln!(
                    "==> [{}] admitted unidentified compiler with {} from the shared slice budget",
                    units.triple,
                    aterm_capacity::size(bytes),
                );
            }
            Some(claim)
        }
    };
    let identity = (!query && unit.is_some()).then(|| compiler_identity(args));
    let started = timestamp();
    let child = command
        .spawn()
        .map_err(|e| format!("start {}: {e}", words.first().map_or("", String::as_str)))?;
    let meter = aterm_capacity::tree_meter::TreeMeter::start(child.id());
    if let Some(unit) = &unit {
        let mut event = unit_event(units, unit, "started", queued, started, None, None);
        if let aterm_json::Value::Object(fields) = &mut event {
            fields.insert("compiler_pid".to_owned(), child.id().into());
            fields.insert(
                "memory_waiting_s".to_owned(),
                aterm_json::json!(claim.as_ref().map(|claim| claim.waited().as_secs_f64())),
            );
            fields.insert(
                "memory_waited_for".to_owned(),
                wait_evidence(claim.as_ref().and_then(aterm_capacity::Claim::waited_for)),
            );
        }
        eprintln!("{event}");
    }
    let CompilerUsage {
        status,
        peak: compiler_peak,
        rss,
        cpu,
    } = wait(child).map_err(|e| format!("wait for the compiler: {e}"))?;
    let peak = Some(meter.stop(compiler_peak.unwrap_or(0))).filter(|p| *p > 0);
    if let Some(unit) = &unit {
        let mut event = unit_event(
            units,
            unit,
            "ended",
            queued,
            timestamp(),
            cpu,
            Some(status.success()),
        );
        if let aterm_json::Value::Object(fields) = &mut event {
            fields.insert("peak_bytes".into(), aterm_json::json!(peak));
            fields.insert(
                "compiler_peak_bytes".into(),
                aterm_json::json!(compiler_peak),
            );
            fields.insert("maxrss_bytes".into(), aterm_json::json!(rss));
            fields.insert(
                "memory_identity".into(),
                identity.as_deref().unwrap_or("unknown").into(),
            );
        }
        eprintln!("{event}");
    }
    drop(claim);
    // A compiler that failed or was killed stopped short of its peak.
    if status.success()
        && let (Some(unit), Some(peak)) = (&unit, peak)
        && let Err(e) = keep(units, unit, peak, identity.as_deref().unwrap_or("unknown"))
    {
        // The compile stands; only its next claim lacks this run's peak.
        eprintln!(
            "==> [{}] {} {}: peak not kept: {e}",
            units.triple, unit.package, unit.kind
        );
    }
    Ok(status
        .code()
        .and_then(|c| u8::try_from(c).ok())
        .unwrap_or(1))
}

fn timestamp() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// The resource host's final waiting look, retained with the unit event so even a wait shorter
/// than the pilot's sampling interval names every claim that held it. The claim order is its
/// durable `(start_ms, pid, seq)` identity; `claimed_bytes` is the holder's declared envelope and
/// `reserved_bytes` what still blocked this waiter after process-tree credit.
fn wait_evidence(shortfall: Option<&aterm_capacity::Shortfall>) -> aterm_json::Value {
    let Some(shortfall) = shortfall else {
        return aterm_json::Value::Null;
    };
    aterm_json::json!({
        "reason":shortfall.to_string(),
        "holders":shortfall.holders.iter().map(|holder| aterm_json::json!({
            "pid":holder.pid,
            "what":holder.what,
            "held":holder.held,
            "age_s":holder.age.as_secs_f64(),
            "claimed_bytes":holder.claimed,
            "reserved_bytes":holder.mem,
            "alone":holder.alone,
            "order":{
                "start_ms":holder.order.0,
                "pid":holder.order.1,
                "seq":holder.order.2,
            },
        })).collect::<Vec<_>>(),
    })
}

/// One compiler's timeline event. Epoch seconds align slices with the tree's event stream;
/// the wrapper PID distinguishes repeated invocations of the same Cargo unit.
fn unit_event(
    units: &Units,
    unit: &aterm_verify::unitgraph::UnitName,
    phase: &str,
    queued: f64,
    at: f64,
    cpu: Option<f64>,
    success: Option<bool>,
) -> aterm_json::Value {
    aterm_json::json!({
        "event": "release-slice-unit", "phase": phase,
        "triple": units.triple, "package": unit.package, "unit": unit.kind,
        "pid": std::process::id(), "queued_unix_s": queued, "at_unix_s": at,
        "cpu_s": cpu, "success": success,
    })
}

/// Keep `peak`; a previously unknown unit starts at no less than [`FLOOR`]. Thus one successful
/// learning run makes even a light unit known, while later observations remain measurements.
fn keep(
    units: &Units,
    unit: &aterm_verify::unitgraph::UnitName,
    peak: u64,
    identity: &str,
) -> std::io::Result<()> {
    let key = measured_key(&units.triple, unit, identity);
    let at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    aterm_capacity::book::Records::update(&units.record, |records| {
        let kept = records.rows().iter().any(|row| row.key == key);
        records.push(key, if kept { peak } else { peak.max(FLOOR) }, at_ms);
        true
    })
    .map(|_| ())
}

/// The owning wait's counters. `peak` is physical footprint on Darwin and
/// resident peak off it; `rss` remains distinct for comparison on Darwin.
struct CompilerUsage {
    status: std::process::ExitStatus,
    peak: Option<u64>,
    rss: Option<u64>,
    cpu: Option<f64>,
}

/// Wait without reaping first: Darwin retains the lifetime maximum physical
/// footprint on the zombie. Read it while the child handle still pins its pid,
/// then reap for RSS and CPU accounting. No sampling interval can miss a peak.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn wait(mut child: aterm_uds::spawnfd::Child) -> std::io::Result<CompilerUsage> {
    child.wait_exited()?;
    #[cfg(target_os = "macos")]
    let peak = footprint(child.id());
    let usage = child.wait_with_rusage()?;
    let rss = Some(usage.maxrss).filter(|b| *b > 0);
    #[cfg(not(target_os = "macos"))]
    let peak = rss;
    Ok(CompilerUsage {
        status: usage.status,
        peak,
        rss,
        cpu: Some(usage.cpu.as_secs_f64()),
    })
}

#[cfg(target_os = "macos")]
fn footprint(pid: u32) -> Option<u64> {
    let mut info = std::mem::MaybeUninit::<libc::rusage_info_v4>::zeroed();
    // SAFETY: info has the layout and writable size requested by RUSAGE_INFO_V4;
    // the caller holds the owned, unreaped child, so the pid cannot be reused.
    let rc = unsafe {
        libc::proc_pid_rusage(
            pid.try_into().ok()?,
            libc::RUSAGE_INFO_V4,
            info.as_mut_ptr().cast::<libc::rusage_info_t>(),
        )
    };
    if rc != 0 {
        return None;
    }
    // SAFETY: the successful kernel call initialized the integer-only record.
    let peak = unsafe { info.assume_init() }.ri_lifetime_max_phys_footprint;
    (peak > 0).then_some(peak)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn wait(mut child: aterm_uds::spawnfd::Child) -> std::io::Result<CompilerUsage> {
    Ok(CompilerUsage {
        status: child.wait()?,
        peak: None,
        rss: None,
        cpu: None,
    })
}

#[cfg(test)]
#[path = "slice_units_tests.rs"]
mod tests;
