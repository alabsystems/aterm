// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! EACH RELEASE SLICE COMPILER CLAIMS ITS OWN MEMORY (`docs/measured/qualify-cores-2026-11-08.md`).
//!
//! A slice's build claimed the peak of its last unit, the fat-LTO link of `aterm` (42 GiB native,
//! 23 GiB compat), for its whole life. On m3 the two left 10.8 GiB for the qualify's tree, whose
//! heavy test compiles waited for the compat slice to end, and the tree took 46m59s of the
//! qualify's 47m52s. Here the slice's Cargo starts every compiler under this executable, its
//! `RUSTC_WRAPPER` ([`Units::env`]). The wrapper ([`entry`]) names the compiler's unit
//! ([`aterm_verify::memory::unit_of`]) and claims that unit's recorded peak, rounded up to a GiB
//! ([`unit_need`]). The claim LEADS ([`aterm_capacity::Need::leads`]): while it waits, the
//! release's other work leaves its room. The wrapper then runs the compiler as Cargo asked, its
//! own variables and `RUSTC_WRAPPER` removed, so the compiler sees the release's environment
//! ([`compiler`]). It keeps a finished compiler's `wait4` peak in the record ([`RECORD`]) where
//! that reaches [`FLOOR`]. A build whose record holds every slice's last unit then has each slice
//! claim only its driver ([`DRIVER`]); one without claims each slice's whole peak as before, and
//! its wrappers only record ([`slice_need`], [`Units::of`]).

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
    /// ([`last_unit`]): the slices' claims are one process's, which each slice's compilers leave
    /// out of their own admission, so a slice that claimed its whole peak would go uncounted by
    /// the other's. Each unit then leaves spare the drivers its build holds at once.
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

/// What `triple`'s slice claims: its driver where its units claim ([`Units::claims`]), else its
/// whole build's peak, rounded up from the 0.174.0 m3 peaks (41.7 and 22.8 GiB).
pub(crate) fn slice_need(
    triple: &str,
    native: bool,
    units: Option<&Units>,
) -> aterm_capacity::Need {
    let need = aterm_capacity::Need::new(format!("release slice {triple}"))
        .rank(aterm_capacity::Rank::Release)
        // An unclaimed load cannot turn a memory shortage into admission ALONE.
        .margin(1);
    if units.is_some_and(Units::claims) {
        need.mem(DRIVER)
    } else {
        need.mem_gib(if native { 42 } else { 23 })
    }
}

/// The slice's last unit, whose record makes its units claim.
fn last_unit() -> aterm_verify::unitgraph::UnitName {
    aterm_verify::unitgraph::UnitName {
        package: "aterm".to_owned(),
        kind: "aterm bin".to_owned(),
    }
}

fn key(triple: &str, unit: &aterm_verify::unitgraph::UnitName) -> aterm_capacity::book::Key {
    aterm_capacity::book::Key {
        stage: STAGE.to_owned(),
        // A release builds from a fresh clone, so no checkout names a slice's units.
        checkout: String::new(),
        crates: Some(vec![unit.package.clone()]),
        variant: Some(format!("{triple} {}", unit.kind)),
    }
}

/// The most `unit` of `triple` held in the record's kept runs.
fn peak(
    records: &aterm_capacity::book::Records,
    triple: &str,
    unit: &aterm_verify::unitgraph::UnitName,
) -> Option<u64> {
    let key = key(triple, unit);
    records
        .rows()
        .iter()
        .find(|row| row.key == key)
        .and_then(|row| row.peaks.iter().map(|p| p.bytes).max())
}

/// What `unit`'s compiler in `triple`'s slice claims: its recorded peak, at least [`FLOOR`],
/// rounded up to a GiB (a slice's whole claim was rounded up from its measured peak alike),
/// leading, with `spare` (its build's drivers, [`Units::of`]) left spare.
pub(crate) fn unit_need(
    triple: &str,
    records: &aterm_capacity::book::Records,
    unit: &aterm_verify::unitgraph::UnitName,
    spare: u64,
) -> aterm_capacity::Need {
    let bytes = peak(records, triple, unit)
        .unwrap_or(FLOOR)
        .max(FLOOR)
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
    let claim = match (&unit, units.spare) {
        (Some(unit), Some(spare)) => {
            let records = aterm_capacity::book::Records::load(&units.record);
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
    let child = command
        .spawn()
        .map_err(|e| format!("start {}: {e}", words.first().map_or("", String::as_str)))?;
    let (status, peak) = wait(child).map_err(|e| format!("wait for the compiler: {e}"))?;
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

/// The compiler's exit and its peak resident bytes (`wait4`'s `ru_maxrss`, its own children's
/// included), `None` where the host does not report it.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn wait(
    mut child: aterm_uds::spawnfd::Child,
) -> std::io::Result<(std::process::ExitStatus, Option<u64>)> {
    let usage = child.wait_with_rusage()?;
    Ok((usage.status, Some(usage.maxrss).filter(|b| *b > 0)))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn wait(
    mut child: aterm_uds::spawnfd::Child,
) -> std::io::Result<(std::process::ExitStatus, Option<u64>)> {
    Ok((child.wait()?, None))
}

#[cfg(test)]
#[path = "slice_units_tests.rs"]
mod tests;
