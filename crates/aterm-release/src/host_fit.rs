// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Whether this Mac can hold the release it is about to take the line of.
//!
//! A version has one fleet line, and whichever Mac starts it holds it. The release's memory
//! claims come from measured peaks, and an indivisible one (a single compiler unit of the
//! tree's test compile, the serial parent of the bundle's slices) that is not below what the
//! machine can ever have available waits without end or refuses hours in, at qualify, after the
//! line is taken: the 0.199.0 line was held for a day by a 32 GiB Mac that a 32.8 GiB compiler
//! unit could never fit. [`judge`] asks the capacity ledger's own rule
//! ([`aterm_capacity::never`]) of those claims before any claim, bump or fleet write, so the
//! refusal costs seconds and leaves the line for a Mac that fits.
//!
//! A compiler unit is judged as the tree's test compile claims it
//! (`aterm_verify::compile_width`): its recorded peak and a fifth more
//! ([`aterm_capacity::peak_claim`]), with the driver's envelope spare beside it. Where that can
//! never fit, the compile no longer refuses the unit: it runs it ALONE, every other unit
//! waiting. A Mac that could compile the release's heaviest unit only that way is the one this
//! refuses the line to.
//!
//! It judges what this Mac recorded, and where this Mac recorded nothing, what a cut host
//! measured ([`measured`]): a Mac that has never built the tree holds no record of it, which is
//! the state the 0.199.0 host took its line in, so no record is never a pass. Two needs no
//! memory record names are judged for every Mac: the cores the qualify tree's work needs to end
//! inside one lease of the line ([`least_cores`]), and the disk the release claims on its
//! clone's volume, against what is free there now.

use aterm_capacity::book::{Records, UNIT_STAGE, VERIFY_RECORD};
use aterm_capacity::{Need, Reading};

use crate::release::{RELEASE_COMMAND, Refusal};

/// The memory a compiler unit's claim keeps spare beside it: the driver's envelope
/// ([`aterm_verify::compile_width::DRIVER_CLAIM`]), which is what the unit's own claim carries.
/// A constant, so the answer does not move with the load of the moment.
const MARGIN: u64 = aterm_verify::compile_width::DRIVER_CLAIM;

/// What a qualification of this tree needed where a cut host measured it: the 0.199.0 cut of
/// 2026-11-12 on m86 (36 cores, 256 GiB), read from that account's own records after the cut.
/// They stand in for the record a Mac does not hold, and give the cores bound its numbers.
/// Measure again on a cut host after a release there: the largest kept peak of the
/// `release slice unit` row `aarch64-apple-darwin aterm bin` in the account's
/// `release/slice-unit-memory.json`, of the `TestCompile unit` row `aterm_gui test` in its
/// `verify/stage-memory.json`, and step 4's `pool:` and `tree: passed` lines.
pub(crate) mod measured {
    /// The bundle's heaviest compiler unit, the native `aterm` binary's final link: 32.15 GiB.
    pub(crate) const SLICE_UNIT: u64 = 34_523_393_840;
    /// The tree's heaviest test-compile unit, `aterm_gui test`: 20.30 GiB.
    pub(crate) const TEST_UNIT: u64 = 21_796_409_528;
    /// The cores the qualify tree held, as one Release claim's pool.
    pub(crate) const TREE_CORES: u64 = 36;
    /// How long the qualify tree ran on them: 1h05m.
    pub(crate) const TREE_SECS: u64 = 65 * 60;
}

/// The fewest cores a Mac that takes a line has. The qualify tree's work is at most the pool it
/// held times how long it ran ([`measured::TREE_CORES`] x [`measured::TREE_SECS`], 39
/// core-hours); on fewer cores than this it cannot end inside the longest lease a line's holder
/// is given without a completed row ([`crate::line_claim::MAX_LEASE_S`], four hours), and a
/// holder slower than its lease is one the fleet takes the line from mid-qualify. An upper
/// bound on the work, so it refuses no Mac the lease would keep: 10 cores today.
pub(crate) const fn least_cores() -> u64 {
    (measured::TREE_CORES * measured::TREE_SECS).div_ceil(crate::line_claim::MAX_LEASE_S)
}

/// The host as a refusal and a plan line name it.
fn host_words(reading: &Reading) -> String {
    let memory = match (reading.physical, reading.reachable()) {
        (Some(physical), Some(reach)) if reach < physical => format!(
            "{} of memory ({} of it can ever be available)",
            aterm_capacity::gib(physical),
            aterm_capacity::gib(reach)
        ),
        (Some(physical), _) => format!("{} of memory", aterm_capacity::gib(physical)),
        (None, _) => "memory unread".to_string(),
    };
    format!("{memory}, {} cores", reading.cores)
}

/// The heaviest compiler unit `records` keeps, with what it is: an indivisible claim of the
/// tree's test compile.
fn heaviest_unit(records: &Records) -> Option<Need> {
    records
        .rows()
        .iter()
        .filter(|row| row.key.stage == UNIT_STAGE)
        .filter_map(|row| {
            let peak = row.peaks.iter().map(|peak| peak.bytes).max()?;
            Some((peak, row))
        })
        .max_by_key(|(peak, _)| *peak)
        .map(|(peak, row)| {
            Need::new(format!(
                "the heaviest compiler unit recorded here ({}, in {})",
                row.key.variant.as_deref().unwrap_or("unnamed"),
                row.key.checkout
            ))
            .mem(aterm_capacity::peak_claim(peak))
            .margin(MARGIN)
        })
}

/// [`heaviest_unit`] for a Mac that recorded none: the cut host's ([`measured::TEST_UNIT`]),
/// claimed the same way.
fn measured_unit() -> Need {
    Need::new(format!(
        "the tree's heaviest compiler unit as a cut host measured it (aterm_gui test, a {} \
         peak; this Mac has recorded none)",
        aterm_capacity::gib(measured::TEST_UNIT)
    ))
    .mem(aterm_capacity::peak_claim(measured::TEST_UNIT))
    .margin(MARGIN)
}

/// The bundle slices' serial parent for a Mac whose compiler-unit book is not measured: the
/// cut host's heaviest slice unit ([`measured::SLICE_UNIT`]) as the slices claim it, with the
/// one live driver beside it ([`crate::slice_units::serial_parent_bytes`]).
fn measured_serial() -> Need {
    Need::new(format!(
        "the bundle's heaviest compiler unit as a cut host measured it (the native aterm \
         binary's link, a {} peak, claimed with its slice driver; this Mac has recorded none)",
        aterm_capacity::gib(measured::SLICE_UNIT)
    ))
    .mem(crate::slice_units::serial_parent_bytes(
        crate::slice_units::claim_bytes(measured::SLICE_UNIT),
    ))
}

/// Why `cores` are too few to take a line ([`least_cores`]); none for a reading with no count.
fn too_few_cores(cores: u32) -> Option<String> {
    let least = least_cores();
    (cores > 0 && u64::from(cores) < least).then(|| {
        format!(
            "the qualify tree ran {} on the {} cores it was measured on, at most {} core-hours \
             of work, which {cores} cores cannot end inside the {} a line's holder is given \
             without a completed row: needs {least} cores",
            crate::release::fmt_dur(measured::TREE_SECS),
            measured::TREE_CORES,
            (measured::TREE_CORES * measured::TREE_SECS).div_ceil(3600),
            crate::release::fmt_dur(crate::line_claim::MAX_LEASE_S),
        )
    })
}

/// Why the release's disk claim `disk` (bytes, with what sized it) does not fit the clone's
/// volume as `reading` reads it now; none where it fits or no volume was read.
fn too_little_disk(disk: Option<&(u64, String)>, reading: &Reading) -> Option<String> {
    let (bytes, basis) = disk?;
    let volume = reading.volume.as_ref()?;
    (volume.above_floor() < *bytes).then(|| {
        format!(
            "the release claims {} of disk on {} ({basis}), which has {} free above its {} floor",
            aterm_capacity::gib(*bytes),
            volume.mount.display(),
            aterm_capacity::gib(volume.above_floor()),
            aterm_capacity::gib(volume.floor)
        )
    })
}

/// Judge `reading`'s machine against what the release needs of a Mac that holds its line:
/// `units`, this Mac's verify records, and `serial`, the bundle slices' serial parent where its
/// book is measured, each replaced by a cut host's measurement where this Mac has none
/// ([`measured`]); the cores of [`least_cores`]; and `disk`, the release's disk claim with
/// what sized it, against the free space of the volume `reading` read. `Ok` is the line step 1
/// says; `Err` the refusal, which names every shortfall with its numbers, and the host.
pub(crate) fn judge(
    reading: &Reading,
    units: &Records,
    serial: Option<Need>,
    disk: Option<(u64, String)>,
) -> Result<String, Refusal> {
    let host = host_words(reading);
    let unit = heaviest_unit(units).unwrap_or_else(measured_unit);
    let serial = serial.unwrap_or_else(measured_serial);
    let short: Vec<String> = [&unit, &serial]
        .into_iter()
        .filter_map(|need| aterm_capacity::never(need, reading))
        .chain(too_few_cores(reading.cores))
        .chain(too_little_disk(disk.as_ref(), reading))
        .collect();
    if !short.is_empty() {
        return Err(Refusal::new(
            format!(
                "this Mac cannot hold the release whose line it would take — {}. The host: \
                 {host}",
                short.join("; ")
            ),
            format!(
                "start or adopt the line on a Mac that fits: {RELEASE_COMMAND} there (`aterm \
                 ctl survey --fleet` lists the fleet's Macs), or leave it to the pump on the \
                 cut host ({RELEASE_COMMAND} --pump), which takes the line itself; a shortfall \
                 of disk alone ends when space is freed on that volume; a line this Mac \
                 already holds still resumes here"
            ),
        ));
    }
    Ok(format!(
        "host: {host}; {} ({}), the slices' parent ({}), {} cores or more{} fit",
        unit.what,
        aterm_capacity::gib(unit.mem),
        aterm_capacity::gib(serial.mem),
        least_cores(),
        disk.map_or_else(String::new, |(bytes, _)| format!(
            " and the release's {} of disk",
            aterm_capacity::gib(bytes)
        ))
    ))
}

/// [`judge`] of this Mac as it is: the live reading with the volume of the clone `repo`, this
/// account's verify records, the slices' serial parent from the compiler-unit book, and the
/// disk the release will claim (`disk`, with what sized it).
pub(crate) fn live(repo: &std::path::Path, disk: (u64, String)) -> Result<String, Refusal> {
    use aterm_capacity::Machine as _;
    let reading = aterm_capacity::Live.read(Some(repo));
    let units = aterm_types::dirs::account_state_dir()
        .map(|state| Records::load(&state.join(VERIFY_RECORD)))
        .unwrap_or_default();
    let slices = crate::slice_units::Units::live(
        &[crate::buildplan::ARM64, crate::buildplan::X86_64],
        2,
        std::path::Path::new("unused-host-fit-private-slice-ledger"),
    );
    judge(
        &reading,
        &units,
        crate::slice_units::serial_need(&slices),
        Some(disk),
    )
}

#[cfg(test)]
#[path = "host_fit_tests.rs"]
mod tests;
