// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Source-to-spec refinement closure checks across all anchored crates.

use std::collections::BTreeSet;

use aterm_spec::xref::{self, SpecModule};

/// The registered `SpecModule` set: every model linked into this binary.
fn registered_modules() -> Vec<SpecModule> {
    xref::model_registry()
        .into_iter()
        .map(SpecModule::Embedded)
        .collect()
}

/// Each collected list as one line per record, sorted, digested (FNV-1a 64): the line SP.2
/// compares across hosts, whose load-time constructor sections differ (Mach-O
/// `__mod_init_func`, ELF `.init_array`, PE `.CRT$XCU`; `aterm_spec_anchor::registry`).
fn collected_digests(refs: &[&xref::RefinementAnchor], waivers: &[&xref::Waiver]) -> String {
    fn digest(mut lines: Vec<String>) -> String {
        lines.sort_unstable();
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in lines.iter().flat_map(|l| l.bytes().chain(*b"\n")) {
            hash = (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
        }
        format!("{}={hash:016x}", lines.len())
    }
    let anchors = refs.iter().map(|r| {
        format!(
            "{}\t{}\t{}\t{}\t{}\t{}",
            r.machine, r.action, r.rust_method, r.location, r.project, r.entry_id
        )
    });
    let waived = waivers.iter().map(|w| {
        format!(
            "{}\t{}\t{}\t{}\t{}",
            w.machine, w.action, w.rust_method, w.location, w.reason
        )
    });
    let models = xref::model_registry()
        .into_iter()
        .map(|m| format!("{}\n{}\n{}", m.name, m.to_tla(), m.to_cfg()));
    let liveness = xref::liveness_registry()
        .into_iter()
        .map(|(m, l)| format!("{}\n{}", m.name, l.to_tla(&m)));
    format!(
        "collected digests (records=fnv1a64): refinements {} waivers {} models {} liveness {}",
        digest(anchors.collect()),
        digest(waived.collect()),
        digest(models.collect()),
        digest(liveness.collect()),
    )
}

#[test]
fn spec_xref_closure() {
    let _domains = aterm_models::link_all();
    std::hint::black_box(
        aterm_agent::harness::accounts_sync::account_provider_realm_environment as fn(),
    );
    std::hint::black_box(
        aterm_agent::harness::accounts_sync::account_alias_admission_environment as fn(),
    );
    #[cfg(unix)]
    std::hint::black_box(aterm_db::replica::db_repair_policy_environment as fn());
    #[cfg(unix)]
    std::hint::black_box(aterm_db::accounts::claims::db_claim_ring_environment as fn());
    std::hint::black_box(aterm_secrets::need::credential_request_environment as fn());
    std::hint::black_box(
        aterm_agent::harness::codex_app_login::codex_refresh_publication_environment as fn(),
    );
    std::hint::black_box(
        aterm_agent::harness::codex_app_login::codex_refresh_identity_environment as fn(),
    );
    std::hint::black_box(
        aterm_agent::harness::codex_app_login::codex_refresh_authority_environment as fn(),
    );
    std::hint::black_box(
        aterm_agent::harness::codex_app_login::codex_login_publication_environment as fn(),
    );
    std::hint::black_box(
        aterm_agent::harness::codex_app_login::codex_refresh_result_environment as fn(),
    );
    std::hint::black_box(
        aterm_agent::harness::codex_app_login::codex_stored_identity_environment as fn(),
    );
    std::hint::black_box(
        aterm_agent::harness::accounts_pool::account_pool_preparation_environment as fn(),
    );
    // aterm-vz's `VzLifecycle` anchors: one named item in each module that holds them.
    #[cfg(unix)]
    std::hint::black_box((
        aterm_vz::lifecycle::vz_lifecycle_environment as fn(),
        aterm_vz::agent::run as *const (),
        aterm_vz::client::Session::attach as *const (),
        aterm_vz::helper::take as *const (),
        aterm_vz::layers::Layers::prepare as *const (),
        aterm_vz::record::kill_helper as *const (),
    ));
    // aterm-cli's `SetupLedger` anchors, all in macOS code: the waivers' host beside the
    // ledger, and the doctor's entry, which reaches the ledger and the kept-by-person record.
    #[cfg(target_os = "macos")]
    std::hint::black_box((
        aterm_cli::macos_doctor::setup_ledger_environment as fn(),
        aterm_cli::macos_doctor::run as *const (),
    ));
    #[cfg(unix)]
    std::hint::black_box(
        aterm_db::control::Endpoint::exchange
            as fn(
                &aterm_db::control::Endpoint,
                _,
                aterm_db::control::Side,
                &mut aterm_db::control::Repair,
            ) -> aterm_db::Result<aterm_db::control::Report>,
    );
    #[cfg(unix)]
    {
        use aterm_secrets::{
            age::Identity,
            client::Admin,
            commons::{
                journal::Journal,
                roster::{self, Access, Enrollment, Grant},
            },
        };
        std::hint::black_box((
            roster::current as fn(&Journal, &Identity) -> aterm_secrets::Result<Option<Access>>,
            Access::is_current as fn(&Access, &Journal) -> bool,
            Admin::prepare_commons_genesis
                as fn(
                    &Admin,
                    [u8; 16],
                    Vec<Enrollment>,
                ) -> aterm_secrets::Result<aterm_secrets::commons::node::Node>,
            Admin::prepare_commons_rotation
                as fn(
                    &Admin,
                    &Journal,
                    Vec<Grant>,
                ) -> aterm_secrets::Result<aterm_secrets::commons::node::Node>,
            Admin::prepare_commons_retirement
                as fn(
                    &Admin,
                    &Journal,
                    Vec<Grant>,
                    &[[u8; 48]],
                ) -> aterm_secrets::Result<aterm_secrets::commons::node::Node>,
        ));
    }
    #[cfg(unix)]
    std::hint::black_box(
        aterm_secrets::client::Admin::approve_commons_member
            as fn(
                &aterm_secrets::client::Admin,
                &aterm_secrets::commons::journal::Journal,
                &str,
                aterm_secrets::commons::binding::Possession,
            )
                -> aterm_secrets::Result<aterm_secrets::commons::membership::Certificate>,
    );
    #[cfg(unix)]
    {
        use aterm_secrets::commons::constraints::{Catalog, Constraints, Epoch};
        std::hint::black_box((
            Constraints::check_actor as fn(&Constraints, &[u8; 48]) -> aterm_secrets::Result<()>,
            Constraints::check_device_candidate
                as fn(
                    &Constraints,
                    &aterm_secrets::commons::binding::Binding,
                ) -> aterm_secrets::Result<()>,
            Constraints::epoch as fn(&Constraints) -> Epoch,
            Constraints::catalog as fn(&Constraints, [u8; 32], &_) -> Catalog,
        ));
    }
    #[cfg(unix)]
    {
        use aterm_secrets::commons::binding::{Pending, Possession, Request};
        std::hint::black_box((
            Pending::issue
                as fn(&Request, [u8; 16], [u8; 32]) -> aterm_secrets::Result<(Pending, Vec<u8>)>,
            Pending::verify as fn(&mut Pending, &[u8]) -> aterm_secrets::Result<Possession>,
        ));
    }
    #[cfg(unix)]
    std::hint::black_box(
        aterm_secrets::commons::journal::Journal::state
            as fn(
                &aterm_secrets::commons::journal::Journal,
            ) -> aterm_secrets::commons::journal::State,
    );
    #[cfg(unix)]
    std::hint::black_box((
        aterm_secrets::client::merge_diverged_heads
            as fn(
                &[aterm_secrets::client::ObservedHead],
            ) -> aterm_secrets::Result<aterm_secrets::client::MergedHeads>,
        aterm_secrets::client::read_server_heads
            as fn(
                &aterm_secrets::client::Paths,
                &[String],
            ) -> aterm_secrets::Result<Vec<aterm_secrets::client::ObservedHead>>,
        aterm_secrets::client::publish_to_servers
            as fn(
                &aterm_secrets::client::Paths,
                &[String],
                &[u8],
            ) -> aterm_secrets::Result<Vec<(String, aterm_secrets::Result<()>)>>,
        aterm_secrets::client::verify_approved_in_served_head
            as fn(
                &aterm_secrets::client::Paths,
                &aterm_secrets::client::Admin,
                u64,
                &[String],
            ) -> aterm_secrets::Result<()>,
        aterm_secrets::client::steward_ensure_lease
            as fn(
                &aterm_secrets::client::Paths,
                Option<&str>,
                u64,
            ) -> aterm_secrets::Result<aterm_secrets::client::StewardLease>,
        aterm_secrets::client::ledger_publish_environment as fn(),
    ));
    #[cfg(unix)]
    {
        use aterm_secrets::membership_log::{Inserted, Log, Membership, Record};
        std::hint::black_box((
            Log::insert as fn(&mut Log, Record) -> aterm_secrets::Result<Inserted>,
            Log::membership as fn(&Log) -> Membership,
            aterm_secrets::membership_log::membership_log_environment as fn(),
        ));
    }
    #[expect(
        clippy::disallowed_methods,
        reason = "a link root: the window's entry is named, never called"
    )]
    std::hint::black_box((
        aterm_gui::main_entry as fn(Vec<std::ffi::OsString>),
        aterm_core::terminal::Terminal::new as fn(u16, u16) -> aterm_core::terminal::Terminal,
        aterm_keeper::KeeperCore::new as fn(bool) -> aterm_keeper::KeeperCore,
        aterm_keeper::RelaunchBrake::new as fn() -> aterm_keeper::RelaunchBrake,
        aterm_keeper::core::classify_death
            as fn(aterm_keeper::core::DeathEvidence) -> aterm_keeper::core::Verdict,
        aterm_restore::recover::Restored::history_admissible
            as fn(&aterm_restore::recover::Restored) -> bool,
        <aterm_restore::writer::WriteError as std::fmt::Display>::fmt
            as fn(
                &aterm_restore::writer::WriteError,
                &mut std::fmt::Formatter<'_>,
            ) -> std::fmt::Result,
        agh::cli::main_entry as fn(Vec<std::ffi::OsString>) -> std::process::ExitCode,
        agh::policy::route_wall_environment as fn(),
        // agh M2: the ensure-once and git-grant waivers' hosts.
        agh::github::releases::ensure_once_environment as fn(),
        agh::git::grant::git_grant_environment as fn(),
    ));
    let refs: Vec<_> = xref::refinements().collect();
    let waivers: Vec<_> = xref::waivers().collect();

    let machines: BTreeSet<&str> = refs
        .iter()
        .map(|r| r.machine)
        .chain(
            waivers
                .iter()
                .filter(|w| !w.machine.is_empty())
                .map(|w| w.machine),
        )
        .collect();
    eprintln!(
        "spec_xref_closure: collected {} refinement anchor(s) + \
         {} waiver(s) across {} machine(s):",
        refs.len(),
        waivers.len(),
        machines.len()
    );
    for m in &machines {
        let r = refs.iter().filter(|x| x.machine == *m).count();
        let w = waivers.iter().filter(|x| x.machine == *m).count();
        eprintln!("    {m:<16} refinements={r:<3} waivers={w}");
    }
    eprintln!("spec_xref_closure: {}", collected_digests(&refs, &waivers));

    let tm = refs
        .iter()
        .filter(|r| r.machine == "terminal_modes")
        .count();
    assert!(
        tm >= 26,
        "expected >= 26 `terminal_modes` refinement anchors collected cross-crate from \
         aterm-core (Phase 0 must not regress), found {tm}"
    );

    let modules = registered_modules();
    eprintln!("spec_xref_closure: {} registered model(s)", modules.len());

    let report = xref::check_closure(&modules);

    eprintln!("spec_xref_closure: per-machine coverage ledger:");
    for c in &report.coverage {
        eprintln!(
            "  {:<16} ratio={:.3} bound={:<3} waived={:<3} actions={:<3} {}{}",
            c.machine,
            c.ratio(),
            c.bound.len(),
            c.waived.len(),
            c.total_actions,
            if c.active {
                "[ACTIVE]"
            } else {
                "[report-only]"
            },
            if c.active && !c.uncovered.is_empty() {
                format!(" UNCOVERED={:?}", c.uncovered)
            } else {
                String::new()
            },
        );
    }

    assert!(
        report.is_ok(),
        "spec_xref_closure FAILED — source↔spec obligations violated:\n{}",
        report
            .violations
            .iter()
            .map(|v| format!("  [obligation {}] {}", v.obligation, v.message))
            .collect::<Vec<_>>()
            .join("\n")
    );

    eprintln!(
        "spec_xref_closure: GREEN — obligations 1/3/4 hold over every anchor, waiver and model \
         linked here."
    );
}
