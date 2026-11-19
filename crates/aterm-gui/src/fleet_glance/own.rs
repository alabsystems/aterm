// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! This Mac's own reading for the fleet tables (FP-10): its deck's newest vitals as the look a
//! survey answer carries, so this Mac's column, pairs, verdicts and load are what its own deck
//! reads, never its survey answer's look, which reads its rates only when asked and so is unread
//! on a first answer or after a gap.

use aterm_db::fleet::read::{Limits, Load, Look, Own, Pressure as LookPressure};
use aterm_vitals::strain::{self, Now, Strain};
use aterm_vitals::{Pressure, Resource, Snapshot};

/// Bytes in a MiB.
const MIB: f64 = 1_048_576.0;

/// `snap` as this Mac's own reading: each figure from the deck row that holds it, and unread
/// (`None`, never zero) where that row is unread.
///
/// - The pairs are the deck's rows: cores busy against cores, memory in use against physical
///   memory, the GPU's busy share of its cores, the home volume's bytes used against its size,
///   and the Network row's receiving and sending (each a 10 s mean, as every row's now is)
///   against the reference link's driver rate.
/// - The load is the deck's own dial ([`Strain::of`]) in hundredths, unread where the CPU's
///   busy share is (a level from the GPU alone would draw a CPU nobody measured as calm), and
///   its depth past capacity judged only where the run queue and the swap traffic are read and
///   it is over or the pressure is read, as the survey's look judges it.
/// - The verdicts are the deck's rules: the CPU's one over rule ([`strain::cpu_over_depth`]),
///   the memory pressure raised, the GPU at [`aterm_capacity::machine_look::GPU_LIMIT_PCT`] or
///   more busy, and the home volume's free space under the disk watch's cleaning line (its
///   emergency line where no cleaning line is in force on the home volume).
pub(crate) fn own_of(snap: &Snapshot) -> Own {
    let row = |resource: Resource| &snap.resources[resource.index()];
    let read = |value: Option<f64>| value.filter(|value| value.is_finite() && *value >= 0.0);
    let whole = |value: f64| value.round() as u64;
    let now = Now::of(snap);
    let cpu = row(Resource::Cpu);
    let cores = read(cpu.capacity).filter(|cores| *cores > 0.0);
    let busy = read(cpu.now).filter(|_| cores.is_some());
    let memory = row(Resource::Memory);
    let disk = row(Resource::Disk);
    let disk_total = read(disk.capacity).filter(|total| *total > 0.0);
    let disk_free = disk_total
        .zip(read(disk.now))
        .map(|(total, used)| (total - used).max(0.0));
    let gpu_pct = read(now.gpu).map(|share| whole(share.clamp(0.0, 1.0) * 100.0));
    let network = row(Resource::Network);
    let kbps = |bytes_a_second: Option<f64>| read(bytes_a_second).map(|b| whole(b * 8.0 / 1_000.0));
    let pressure = match now.pressure {
        Pressure::Normal => Some(LookPressure::Normal),
        Pressure::Warn => Some(LookPressure::Warn),
        Pressure::Critical => Some(LookPressure::Critical),
        Pressure::Unknown => None,
    };
    let strain = Strain::of(snap);
    let over = strain.over;
    let judged = now.queue.is_some()
        && now.swap_bps.is_some()
        && (over > 0.0 || now.pressure != Pressure::Unknown);
    let x100 = |share: f32| whole((f64::from(share)).clamp(0.0, 1.0) * 100.0);
    let disk_line = snap
        .facts
        .disk
        .as_ref()
        .and_then(|facts| facts.floor.filter(|floor| *floor > 0).or(facts.emergency));
    let look = Look {
        cores: cores.map(whole),
        load1_x100: read(snap.facts.load1).map(|load| whole(load * 100.0)),
        cpu_busy_x100: busy.map(|busy| whole(busy * 100.0)),
        mem_total_mb: read(memory.capacity)
            .filter(|total| *total > 0.0)
            .map(|total| whole(total / MIB)),
        mem_used_mb: read(memory.now).map(|used| whole(used / MIB)),
        disk_total_mb: disk_total.map(|total| whole(total / MIB)),
        gpu_cores: snap
            .gpu
            .as_ref()
            .and_then(|gpu| gpu.cores)
            .filter(|cores| *cores > 0)
            .map(u64::from),
        gpu_busy_pct: gpu_pct,
        net_link_kbps: snap
            .facts
            .link
            .as_ref()
            .and_then(|link| link.rate_bps)
            .filter(|rate| *rate > 0)
            .map(|rate| rate / 1_000),
        net_rx_kbps: kbps(network.now),
        net_tx_kbps: kbps(network.second.as_ref().and_then(|sending| sending.now)),
        mem_pressure: pressure,
        limits: Limits {
            cpu: busy
                .zip(cores)
                .zip(read(now.queue))
                .map(|((busy, cores), queue)| {
                    strain::cpu_over_depth(Some(busy / cores), Some(cores), Some(queue)).is_some()
                }),
            mem: pressure.map(|pressure| pressure != LookPressure::Normal),
            disk: disk_free
                .zip(disk_line)
                .map(|(free, line)| free < line as f64),
            gpu: gpu_pct.map(|pct| pct >= u64::from(aterm_capacity::machine_look::GPU_LIMIT_PCT)),
        },
        load: strain
            .level
            .filter(|_| now.cpu.is_some())
            .map(|level| Load {
                level_x100: x100(level),
                over_x100: judged.then(|| x100(over)),
            }),
        ..Look::UNREAD
    };
    Own {
        look,
        home_free_mb: disk_free.map(|free| whole(free / MIB)),
    }
}
