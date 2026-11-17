// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What starts self-registration (docs/DESIGN-alab-lab-model-2026-11-01.md §6, step 7), and what
//! this machine reads about itself for it: the Commons runtime's completed repair rounds wake
//! `aterm_db::alab::register::Registrar`, a release build only, so a test, a debug build under a
//! lab or a checkout's `targo run` never publishes a machine's facts. The readers here, and on
//! Linux `aterm_sysprobe::linux::Root`'s, are the only code that touches the host;
//! `Registrar::start` keeps the IOPlatformUUID (on Linux `/etc/machine-id`) in this process.

use aterm_db::{
    alab::register::{Outcome, Probe, Registered, Registrar, ThisMac},
    runtime::RepairRound,
};
use aterm_secrets::client::Paths;
use std::path::PathBuf;

/// Whether a pass's result is worth a line a shipped build keeps: its log filter stops at `Info`.
/// A pass that closed a machine row of this Mac's chain into the one it is speaks, since it ended a
/// row the owner sees; any other pass that wrote or had nothing to do stays at debug, and
/// `aterm db alab check` answers why at any time.
fn speaks(pass: &aterm_db::Result<Registered>) -> bool {
    pass.as_ref().map_or(true, |done| {
        !done.findings.is_empty()
            || matches!(
                done.outcome,
                Outcome::Held | Outcome::QuotaSpent | Outcome::Closed { .. }
            )
    })
}

/// One pass's line, by key and never by value.
fn log_pass(pass: &aterm_db::Result<Registered>) {
    let spoken = speaks(pass);
    match pass {
        Ok(done) if spoken => aterm_log::info!(
            "lab self-registration: {:?}, {} record(s), findings {:?}",
            done.outcome,
            done.records,
            done.findings
        ),
        Ok(done) => aterm_log::debug!(
            "lab self-registration: {:?}, {} record(s)",
            done.outcome,
            done.records
        ),
        Err(why) => aterm_log::info!("lab self-registration: {why}"),
    }
}

/// The repair-round hook for one Commons runtime: inert where this process does not publish.
pub(crate) fn reports(paths: Paths, config: PathBuf) -> impl Fn(RepairRound) + Send + 'static {
    let registrar = super::alab_view::holds()
        .then(|| Registrar::start(paths, config, probe, log_pass).ok())
        .flatten();
    move |round: RepairRound| {
        if let Some(registrar) = &registrar {
            registrar.round(&round);
        }
    }
}

/// This Mac and its config file, for `aterm db alab check`, which only reads.
pub(crate) fn this_mac() -> Option<ThisMac> {
    Some(ThisMac {
        probe: probe()?,
        config: aterm_types::dirs::aterm_config_path()?,
    })
}

/// Each cluster's cores: an `Efficiency` cluster's as efficiency cores and the others' as
/// performance cores; a machine that names no clusters states neither.
#[cfg(target_os = "macos")]
fn core_counts(layout: &aterm_sysprobe::CpuLayout) -> (Option<u64>, Option<u64>) {
    if layout.clusters.iter().all(|cluster| cluster.name == "CPU") {
        return (None, None);
    }
    let (mut performance, mut efficiency) = (0, 0);
    for cluster in &layout.clusters {
        let cores = cluster.logical.len() as u64;
        if cluster.name.eq_ignore_ascii_case("efficiency") {
            efficiency += cores;
        } else {
            performance += cores;
        }
    }
    (Some(performance), Some(efficiency))
}

/// This process's fleet node, read: registering never mints one.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn fleet_node() -> Option<String> {
    let state = aterm_types::dirs::state_dir()?;
    aterm_net::fleet_member::read_node_identity(&state)
        .ok()
        .flatten()
        .map(|identity| aterm_net::fleet_member::node_principal(identity.key())[2..].to_owned())
}

/// What one command printed, trimmed, with a one-minute hang detector.
#[cfg(target_os = "macos")]
fn output(program: &str, args: &[&str]) -> Option<String> {
    let mut command = aterm_uds::spawnfd::command(program);
    command.args(args);
    let out = aterm_uds::deadline::output_within(&mut command, std::time::Duration::from_secs(60))
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let mut text = String::from_utf8(out.stdout).ok()?;
    let trimmed_len = text.trim_end().len();
    text.truncate(trimmed_len);
    let leading = text.len() - text.trim_start().len();
    if leading > 0 {
        text.drain(..leading);
    }
    (!text.is_empty()).then_some(text)
}

/// This Mac: `None` where it cannot state its model or its hardware, which its row needs.
#[cfg(target_os = "macos")]
fn probe() -> Option<Probe> {
    use aterm_db::alab::register::gigabytes;
    use aterm_machines::hardware::{ioreg_value, uuid_shaped};
    let platform_uuid = output("/usr/sbin/ioreg", &["-rd1", "-c", "IOPlatformExpertDevice"])
        .and_then(|text| ioreg_value(&text, "IOPlatformUUID"))
        .filter(|uuid| uuid_shaped(uuid))?;
    let hw_model = output("/usr/sbin/sysctl", &["-n", "hw.model"])?;
    let words = aterm_sysprobe::machine_words();
    let os_major = words.os_major();
    let (cores_p, cores_e) = core_counts(&aterm_sysprobe::cpu_layout());
    let hosts = [
        output("/usr/sbin/scutil", &["--get", "ComputerName"]),
        output("/usr/sbin/scutil", &["--get", "LocalHostName"]),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<String>>();
    Some(Probe {
        hw_model,
        chip: words.cpu,
        cores_p,
        cores_e,
        gpu_cores: aterm_sysprobe::gpu()
            .and_then(|gpu| gpu.cores)
            .map(u64::from),
        ram_gb: aterm_sysprobe::physical_memory_mib().map(|mib| (mib + 512) / 1024),
        disk_gb: aterm_sysprobe::volume(std::path::Path::new("/"))
            .map(|volume| gigabytes(volume.total)),
        arch: Some(std::env::consts::ARCH.to_owned()),
        os_major,
        hosts,
        node: fleet_node(),
        platform_uuid,
    })
}

/// This Linux machine ([`Probe::linux`] over `/`), with its fleet node: `None` where it cannot
/// state its model or its `/etc/machine-id`.
#[cfg(target_os = "linux")]
fn probe() -> Option<Probe> {
    Some(Probe {
        node: fleet_node(),
        ..Probe::linux(&aterm_sysprobe::linux::Root::system())?
    })
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn probe() -> Option<Probe> {
    None
}

#[cfg(test)]
#[path = "alab_register_tests.rs"]
mod tests;
