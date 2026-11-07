// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What starts self-registration (docs/DESIGN-alab-lab-model-2026-11-01.md §6, step 7), and what
//! this Mac reads about itself for it: the Commons runtime's completed repair rounds wake
//! `aterm_db::alab::register::Registrar`, a release build only, so a test, a debug build under a
//! lab or a checkout's `targo run` never publishes a Mac's facts. The readers here are the only
//! code that touches the host; `Registrar::start` keeps the IOPlatformUUID in this process.

use aterm_db::{
    alab::register::{Outcome, Probe, Registered, Registrar, ThisMac},
    runtime::RepairRound,
};
use aterm_secrets::client::Paths;
use std::path::PathBuf;

/// Whether a pass's result is worth a line a shipped build keeps: its log filter stops at `Info`.
/// A pass that wrote or had nothing to do stays at debug, and `aterm db alab check` answers why at
/// any time.
fn speaks(pass: &aterm_db::Result<Registered>) -> bool {
    pass.as_ref().map_or(true, |done| {
        !done.findings.is_empty() || matches!(done.outcome, Outcome::Held | Outcome::QuotaSpent)
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
        .then(|| Registrar::start(paths.clone(), config, probe, log_pass).ok())
        .flatten();
    move |round: RepairRound| {
        super::alab_view::round(&paths, &round);
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

/// Bytes as decimal gigabytes, rounded, the way a disk is sold.
#[cfg(target_os = "macos")]
fn gigabytes(bytes: u64) -> u64 {
    bytes.saturating_add(500_000_000) / 1_000_000_000
}

/// This process's fleet node, read: registering never mints one.
#[cfg(target_os = "macos")]
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

/// This Mac: `None` where it cannot state its model or its hardware, which its row needs. Off
/// macOS there is no IOPlatformUUID, and the registry's rows are one physical Mac each.
#[cfg(target_os = "macos")]
fn probe() -> Option<Probe> {
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

#[cfg(not(target_os = "macos"))]
fn probe() -> Option<Probe> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a shipped build logs: only a pass the owner would want to hear of passes its filter.
    /// NEGATIVE CONTROL: with `speaks` always false, the first assertion fails.
    #[test]
    fn a_held_pass_and_an_error_reach_a_shipped_builds_log() {
        let pass = |outcome, findings| {
            Ok(Registered {
                outcome,
                findings,
                records: 0,
            })
        };
        assert!(speaks(&pass(Outcome::Held, Vec::new())));
        assert!(speaks(&pass(Outcome::QuotaSpent, Vec::new())));
        let refused = aterm_db::alab::register::model_key("no such model!").map(|_| Registered {
            outcome: Outcome::Unchanged,
            findings: Vec::new(),
            records: 0,
        });
        assert!(speaks(&refused));
        assert!(speaks(&pass(
            Outcome::Born { mac: "mc-1".into() },
            vec![aterm_db::alab::checks::Finding {
                kind: "hw-hint-live",
                collection: "machine-facts",
                pks: Vec::new(),
            }],
        )));
        assert!(!speaks(&pass(Outcome::Unchanged, Vec::new())));
        assert!(!speaks(&pass(Outcome::Deferred, Vec::new())));
    }

    /// This Mac's real readings make a row the schema takes, and the IOPlatformUUID stays out of
    /// it. It only reads; the plan is computed and thrown away.
    /// NEGATIVE CONTROL: with `probe` reading `hw.model` as `no such model!`, `plan` refuses it.
    #[cfg(target_os = "macos")]
    #[test]
    fn this_macs_real_readings_make_a_row() {
        use aterm_db::alab::register::{Input, Me, plan};
        let read = probe().expect("this Mac states its model and its hardware");
        let me = Me {
            uuid: "11111111111111111111111111111111".into(),
            label: Some("m7".into()),
            hw_hint: "aaaaaaaaaaaaaaaa".into(),
            enrolled_ms: None,
        };
        let live = std::collections::BTreeSet::new();
        let made = plan(
            &Input {
                me: &me,
                probe: &read,
                live: &live,
                heard: true,
                now_ms: 400 * 86_400_000,
            },
            &Default::default(),
            &Default::default(),
        )
        .expect("every reading fits its column or keeps out of the row");
        assert!(matches!(made.outcome, Outcome::Born { .. }));
        assert!(made.changes.iter().all(|change| {
            !aterm_json::to_string(&change.value)
                .unwrap()
                .contains(&read.platform_uuid)
        }));
    }

    /// The readers' arithmetic: a chip's clusters, a disk rounded (the OS's first number is
    /// `aterm_sysprobe::MachineWords::os_major`'s, tested there).
    /// NEGATIVE CONTROL: with `core_counts` counting every cluster as performance cores, the
    /// first assertion reads `(Some(14), Some(0))`.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_readers_arithmetic() {
        use aterm_sysprobe::{Cluster, CpuLayout};
        let cluster = |name: &str, n: u16| Cluster {
            name: name.to_owned(),
            logical: (0..n).collect(),
        };
        assert_eq!(
            core_counts(&CpuLayout {
                clusters: vec![cluster("Performance", 10), cluster("Efficiency", 4)],
            }),
            (Some(10), Some(4))
        );
        assert_eq!(core_counts(&CpuLayout::single(8)), (None, None));
        assert_eq!(gigabytes(994_662_584_320), 995);
    }
}
