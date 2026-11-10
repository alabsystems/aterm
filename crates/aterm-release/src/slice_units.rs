// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Release compilers claim their measured unit peaks plus the capacity margin,
//! rounded up to GiB. Records distinguish target, unit, compiler build and metric;
//! Darwin combines sampled simultaneous descendant footprints with kernel
//! lifetime maxima, including the compiler's final counter read before reaping.
//! RSS remains in the timeline for comparison, never as footprint history.
//!
//! A build with every slice's final link recorded claims only its drivers and
//! admits units separately, leading. Otherwise each slice learns alone; no fixed
//! link estimate or incompatible history supplies an unmeasured envelope.
//! See `docs/measured/slice-memory-claim-2026-11-10.md`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The slices' heavy units' record, under this account's aterm state directory.
pub(crate) const RECORD: &str = "release/slice-unit-memory.json";

/// The least peak a unit's record starts from: 1 GiB, so it holds the heavy compilers only. A
/// unit once kept keeps every later peak, so one grown lighter forgets its old peak after
/// [`aterm_capacity::book::KEPT`] runs.
pub(crate) const FLOOR: u64 = 1 << 30;

/// What a slice claims beside its units' own claims: Cargo, the build scripts it runs, and the
/// wrappers. A unit's claim leaves spare every driver its build holds at once, since the ledger
/// leaves a child's ancestors' claims out of the child's admission ([`Units::of`]).
pub(crate) const DRIVER: u64 = aterm_capacity::peak_claim(FLOOR);

/// The record's kind of work.
const STAGE: &str = "release slice unit";

/// The wrapper's variables: the record's path (which makes this executable a wrapper), the
/// slice's triple, and, where its units claim, the bytes each leaves spare.
const WRAPPER: &str = "ATERM_RELEASE_UNIT_RECORD";
const TRIPLE: &str = "ATERM_RELEASE_UNIT_TRIPLE";
const CLAIMS: &str = "ATERM_RELEASE_UNIT_CLAIMS";

/// The line a refused wrapper prints, which Cargo keeps in the slice's log.
const REFUSED: &str = "aterm-release: slice unit admission: ";

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
    for take in 1..=takes {
        let units = Units::live(&[triple], 1);
        let units = units
            .first()
            .ok_or("no account state directory for slice memory")?;
        let records = aterm_capacity::book::Records::load(&units.record);
        let before = unit_need(triple, &records, &last_unit(), DRIVER);
        eprintln!(
            "{}",
            aterm_json::json!({"event":"slice-memory-take", "take":take,
            "record":units.record.display().to_string(), "measured":units.claims(),
            "claim_bytes":units.claims().then_some(before.mem),
            "memory_identity":toolchain_key(triple)})
        );
        let _claim = aterm_capacity::admit(slice_need(triple, None), aterm_capacity::Admit::Wait)
            .map_err(|e| e.to_string())?;
        // The experiment leaves every other memory claimant out, including
        // while measuring a known unit whose ordinary claim cannot fit beside
        // unclaimed machine load. The production claim is still printed above.
        let mut units = units.clone();
        units.spare = None;
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
        for (name, value) in units.env(&wrapper) {
            command.env(name, value);
        }
        let status = command.status().map_err(|e| e.to_string())?;
        if !status.success() {
            return Err(format!("slice memory take {take}: {status}"));
        }
        let records = aterm_capacity::book::Records::load(&units.record);
        let peak = peak(&records, triple, &last_unit())
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

/// One slice's units: the record they keep, the slice's triple, and, where they claim, the
/// drivers each leaves spare.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Units {
    record: PathBuf,
    triple: String,
    spare: Option<u64>,
}

impl Units {
    /// The units of the slices one build runs, `triples`, `at_once` of them at a time, kept at
    /// `record`. They claim only where it holds every one's last unit, `aterm`'s link
    /// ([`last_unit`]): the slices' claims share a process, whose claims a child
    /// compiler excludes from its admission. Mixing isolated training and unit
    /// admission would therefore leave the training slice unprotected from the
    /// other's compilers. Each unit leaves every concurrent driver spare.
    pub(crate) fn of(record: &Path, triples: &[&str], at_once: u64) -> Vec<Units> {
        let records = aterm_capacity::book::Records::load(record);
        let claims = triples
            .iter()
            .all(|triple| peak(&records, triple, &last_unit()).is_some());
        let spare = claims.then(|| DRIVER.saturating_mul(at_once));
        triples
            .iter()
            .map(|triple| Units {
                record: record.to_owned(),
                triple: (*triple).to_owned(),
                spare,
            })
            .collect()
    }

    /// The units of `triples`' slices on this account ([`RECORD`], [`Units::of`]); none with no
    /// state directory.
    pub(crate) fn live(triples: &[&str], at_once: u64) -> Vec<Units> {
        aterm_types::dirs::account_state_dir()
            .map(|dir| Units::of(&dir.join(RECORD), triples, at_once))
            .unwrap_or_default()
    }

    /// The slice's triple.
    pub(crate) fn triple(&self) -> &str {
        &self.triple
    }

    /// Whether they claim their own memory.
    pub(crate) const fn claims(&self) -> bool {
        self.spare.is_some()
    }

    /// What Cargo's environment gains so that `wrapper`, this executable, starts each compiler.
    pub(crate) fn env(&self, wrapper: &Path) -> Vec<(&'static str, OsString)> {
        let mut env = vec![
            ("RUSTC_WRAPPER", wrapper.as_os_str().to_os_string()),
            (WRAPPER, self.record.clone().into_os_string()),
            (TRIPLE, self.triple.clone().into()),
        ];
        if let Some(spare) = self.spare {
            env.push((CLAIMS, spare.to_string().into()));
        }
        env
    }
}

/// A measured slice claims its driver. An unmeasured slice learns in isolation:
/// an RSS record or another compiler build cannot establish its memory envelope.
pub(crate) fn slice_need(triple: &str, units: Option<&Units>) -> aterm_capacity::Need {
    let need = aterm_capacity::Need::new(format!("release slice {triple}"))
        .rank(aterm_capacity::Rank::Release)
        // An unclaimed load cannot turn a memory shortage into admission ALONE.
        .margin(1);
    if units.is_some_and(Units::claims) {
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

pub(crate) fn key(
    triple: &str,
    unit: &aterm_verify::unitgraph::UnitName,
) -> aterm_capacity::book::Key {
    aterm_capacity::book::Key {
        stage: STAGE.to_owned(),
        // A release builds from a fresh clone, so no checkout names a slice's units.
        checkout: String::new(),
        crates: Some(vec![unit.package.clone()]),
        variant: Some(format!("{triple} {} {}", unit.kind, toolchain_key(triple))),
    }
}

/// The build is part of the measurement, as is the accounting metric. Old RSS
/// records deliberately do not match physical-footprint records. The compat
/// slice uses the stock compiler pinned by the same lock.
fn toolchain_key(triple: &str) -> String {
    let lock = std::env::current_dir()
        .ok()
        .and_then(|dir| aterm_toolchain::trust_lock::find(&dir))
        .and_then(|found| found.lock.ok());
    let metric = if cfg!(target_os = "macos") {
        "phys-footprint-tree-v1"
    } else {
        "maxrss-v1"
    };
    match lock {
        Some(lock) if triple == "x86_64-apple-darwin" => {
            format!("{metric} stock-{}", lock.stock_toolchain)
        }
        Some(lock) => format!("{metric} trust-{}-{}", lock.build, lock.tree_root),
        None => format!("{metric} unknown"),
    }
}

/// The most `unit` of `triple` held in the record's kept runs.
fn peak(
    records: &aterm_capacity::book::Records,
    triple: &str,
    unit: &aterm_verify::unitgraph::UnitName,
) -> Option<u64> {
    let key = key(triple, unit);
    if key
        .variant
        .as_ref()
        .is_some_and(|v| v.ends_with(" unknown"))
    {
        return None;
    }
    records
        .rows()
        .iter()
        .find(|row| row.key == key)
        .and_then(|row| row.peaks.iter().map(|p| p.bytes).max())
}

/// What `unit`'s compiler in `triple`'s slice claims: its recorded peak, at least [`FLOOR`],
/// plus 20% (at least 1 GiB), rounded up to a GiB,
/// leading, with `spare` (its build's drivers, [`Units::of`]) left spare.
pub(crate) fn unit_need(
    triple: &str,
    records: &aterm_capacity::book::Records,
    unit: &aterm_verify::unitgraph::UnitName,
    spare: u64,
) -> aterm_capacity::Need {
    let bytes = aterm_capacity::peak_claim(peak(records, triple, unit).unwrap_or(FLOOR).max(FLOOR))
        .div_ceil(FLOOR)
        .saturating_mul(FLOOR);
    aterm_capacity::Need::new(format!(
        "release slice {triple} unit {} {}",
        unit.package, unit.kind
    ))
    .rank(aterm_capacity::Rank::Release)
    .mem(bytes)
    .margin(spare)
    .leads()
}

/// Entry for Cargo's rustc wrapper: `None` unless this process is one ([`WRAPPER`]); else the
/// compiler's exit code, or 2 where its unit could not claim its memory.
#[must_use]
pub(crate) fn entry(args: &[OsString]) -> Option<u8> {
    let record = std::env::var_os(WRAPPER)?;
    let units = Units {
        record: record.into(),
        triple: std::env::var(TRIPLE).unwrap_or_default(),
        spare: std::env::var(CLAIMS).ok().and_then(|s| s.parse().ok()),
    };
    let package = std::env::var("CARGO_PKG_NAME").ok();
    let run = || {
        let ledger = aterm_capacity::Ledger::user().ok_or("no capacity ledger")?;
        wrap(
            args,
            package.as_deref(),
            &units,
            &ledger,
            &aterm_capacity::Live,
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
    for name in [WRAPPER, TRIPLE, CLAIMS, "RUSTC_WRAPPER"] {
        command.env_remove(name);
    }
    Ok(command)
}

/// Run the compiler `args` names for the unit it compiles in `package`
/// ([`aterm_verify::memory::unit_of`]): under its own claim where `units` claim, and its peak
/// kept where it finished and is heavy or kept already. A query (`-vV`, `--print`) compiles
/// nothing and runs at once. A claim this host can never hold beside its spare asks to run ALONE
/// ([`aterm_capacity::Need::alone`]), as verify's stages do: beside nothing else, the drivers
/// have the rest of the machine.
fn wrap(
    args: &[OsString],
    package: Option<&str>,
    units: &Units,
    ledger: &aterm_capacity::Ledger,
    machine: &dyn aterm_capacity::Machine,
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
    let claim = match (&unit, units.spare) {
        (Some(unit), Some(spare)) => {
            let records = aterm_capacity::book::Records::load(&units.record);
            // Recheck in the compiler's own working directory. A snapshot can
            // carry a different lock from its orchestration checkout, or the
            // book can have been replaced since the driver chose unit claims.
            if peak(&records, &units.triple, &last_unit()).is_none() {
                return Err("no matching toolchain footprint for the final link; rerun the slice alone to learn it".into());
            }
            let need = unit_need(&units.triple, &records, unit, spare);
            let bytes = need.mem;
            let admit = |need| {
                aterm_capacity::admit_with(
                    ledger,
                    machine,
                    need,
                    aterm_capacity::Admit::Wait,
                    aterm_capacity::Pace::default(),
                    &mut |line| eprintln!("{line}"),
                )
            };
            let claim = match admit(need.clone()) {
                Err(aterm_capacity::Error::Never(_)) => admit(need.alone()),
                admitted => admitted,
            }
            .map_err(|e| e.to_string())?;
            eprintln!(
                "==> [{}] admitted {} {} with {}{}",
                units.triple,
                unit.package,
                unit.kind,
                aterm_capacity::size(bytes),
                if claim.alone().is_some() {
                    ", alone"
                } else {
                    ""
                }
            );
            Some(claim)
        }
        _ => None,
    };
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
                toolchain_key(&units.triple).into(),
            );
        }
        eprintln!("{event}");
    }
    drop(claim);
    // A compiler that failed or was killed stopped short of its peak.
    if status.success()
        && let (Some(unit), Some(peak)) = (&unit, peak)
        && let Err(e) = keep(units, unit, peak)
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

/// Keep `peak` for `unit` where it reaches [`FLOOR`] or the unit is kept already.
fn keep(units: &Units, unit: &aterm_verify::unitgraph::UnitName, peak: u64) -> std::io::Result<()> {
    let key = key(&units.triple, unit);
    let at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    aterm_capacity::book::Records::update(&units.record, |records| {
        let kept = records.rows().iter().any(|row| row.key == key);
        let keeps = peak >= FLOOR || kept;
        if keeps {
            records.push(key, peak, at_ms);
        }
        keeps
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
