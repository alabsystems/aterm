// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The converge pass's live ports (`aterm init --converge`, [`aterm_machines::sync`]): each sync
//! kind read and applied through the library that owns it, here where `aterm` composes them.
//!
//! `sync.fabric` renews a broker that runs a build an update replaced with `aterm fabric on`'s
//! own broker step ([`aterm_link::enable::renew_replaced_broker`]), and never turns the fabric
//! on. `sync.peers` joins the open ALab vault from the window's pass with System State's own join
//! ([`aterm_machines::sync::peers`]), on macOS, where System State runs.

use aterm_machines::init::{Base, BaseLock};
use aterm_machines::sync::{
    AccountsRead, FabricRead, FleetDone, FleetRead, PeersDone, PeersRead, Ports, SharedRead,
    VaultRead,
};
use std::path::{Path, PathBuf};

/// This Mac, as the converge pass reads and changes it.
pub(crate) struct Live;

/// `sync.fabric` on a Mac with no fabric command: the pass never turns the fabric on.
const FABRIC_OFF: &str = "off: no [fabric] command (`aterm fabric on` turns it on)";

/// The vault's local paths, its errors as text.
fn vault_paths() -> Result<aterm_secrets::client::Paths, String> {
    aterm_secrets::client::Paths::live().map_err(|why| why.to_string())
}

/// The configured vault, `None` where none is.
fn vault_config(
    paths: &aterm_secrets::client::Paths,
) -> Result<Option<aterm_secrets::client::Config>, String> {
    aterm_secrets::client::config(paths).map_err(|why| why.to_string())
}

/// What stands before `sync.peers`' join besides the vault: System State's plan (the Secrets
/// vault left out, or a Stop holding it, keeps the vault out) and Local Network as aterm last
/// heard it, a look raising no question once macOS asked aterm before.
#[cfg(target_os = "macos")]
fn peers_gate() -> aterm_machines::sync::peers::Gate {
    use aterm_cli::macos_doctor::{LocalNetworkHeard, initial_setup_keeps_vault_sync};
    use aterm_machines::sync::peers::{Gate, Lan};
    let off = match initial_setup_keeps_vault_sync() {
        Ok(true) => None,
        Ok(false) => {
            Some("System State's plan leaves the Secrets vault out, or a Stop holds it".to_owned())
        }
        Err(why) => Some(format!("System State's records cannot be read ({why})")),
    };
    let lan = match aterm_cli::macos_doctor::local_network_heard() {
        LocalNetworkHeard::On | LocalNetworkHeard::Asked => Lan::Allowed,
        LocalNetworkHeard::Off => Lan::Refused,
        LocalNetworkHeard::Untold => Lan::Untold,
    };
    Gate { off, lan }
}

/// Elsewhere System State does not run, and joins no vault.
#[cfg(not(target_os = "macos"))]
fn peers_gate() -> aterm_machines::sync::peers::Gate {
    aterm_machines::sync::peers::Gate {
        off: Some("System State joins a vault on macOS alone".to_owned()),
        lan: aterm_machines::sync::peers::Lan::Untold,
    }
}

/// The accounts' places: the harness environment, the home and the roster.
fn accounts_env() -> Result<(aterm_agent::harness::cli::Env, PathBuf, PathBuf), String> {
    let env = aterm_agent::harness::cli::Env::from_process()?;
    let home = env.home.clone().ok_or("no user home directory")?;
    let roster = env.accounts_roster().ok_or("no config directory")?;
    Ok((env, home, roster))
}

/// `sync.accounts`' read over these places: off where `config`'s `[harness] accounts` says
/// `"off"` (read first, so an off Mac reads nothing else), no source where none is configured
/// or found, due where the roster is missing or a source it is made from changed
/// (`needs_init_accounts_sync`, the setup's own rule), else current.
fn accounts_at(
    config: Option<&Path>,
    home: &Path,
    state: &Path,
    roster: &Path,
    now: i64,
) -> AccountsRead {
    use aterm_agent::harness::accounts::Mode;
    use aterm_agent::harness::accounts_sync::{has_init_accounts_source, needs_init_accounts_sync};
    let (harness, _) = aterm_agent::supervise::config::SupervisorConfig::from_path(config);
    if harness.accounts == Mode::Off {
        return AccountsRead::Off;
    }
    if !has_init_accounts_source(home, state, roster, now) {
        return AccountsRead::NoSource(
            "no fleet accounts source is configured or found on this Mac".to_owned(),
        );
    }
    if needs_init_accounts_sync(home, state, roster, now) {
        AccountsRead::Due("the roster is missing, or a source it is made from changed".to_owned())
    } else {
        AccountsRead::Current("the roster is in step with its sources".to_owned())
    }
}

/// Whether `dir`, the control socket folder, holds an aterm instance's socket
/// (`aterm-<pid>.sock`) whose process `alive` says lives, this process's own left out: every
/// instance in its own login runs the accounts' upkeep.
fn window_in(dir: &Path, alive: &dyn Fn(u32) -> bool) -> bool {
    let own = std::process::id();
    std::fs::read_dir(dir).is_ok_and(|entries| {
        entries.flatten().any(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.ends_with(".sock")
                && aterm_types::control_socket::instance_pid(&name)
                    .is_some_and(|pid| pid != own && alive(pid))
        })
    })
}

impl Ports for Live {
    fn background(&self) {
        aterm_machines::sync::background_self();
    }

    fn own_account(&self) -> bool {
        aterm_machines::sessions::is_live_account()
    }

    fn at_console(&self) -> Result<bool, String> {
        aterm_machines::init::console_owned(&aterm_machines::port::Live::new())
    }

    fn state_dir(&self) -> Option<PathBuf> {
        aterm_types::dirs::account_state_dir()
    }

    fn build(&self) -> String {
        aterm_types::version::build_id()
    }

    fn routes(&self) -> Vec<String> {
        aterm_secrets::server::route_sources()
            .iter()
            .map(|route| route.map_or_else(|| "-".to_owned(), |ip| ip.to_string()))
            .collect()
    }

    fn now_ms(&self) -> u64 {
        aterm_time::unix_millis().unwrap_or(0)
    }

    fn installed_release(&self) -> bool {
        std::env::current_exe()
            .and_then(|exe| exe.canonicalize())
            .is_ok_and(|exe| atpkg::hooks::is_installed_release_exe(&exe))
    }

    fn window_runs(&self) -> bool {
        aterm_uds::control_socket_dir()
            .is_some_and(|dir| window_in(&dir, &aterm_uds::process::pid_alive))
    }

    fn base(&self, lock: BaseLock) -> Base {
        crate::doctor::sync_base(lock)
    }

    fn vault(&self) -> Result<VaultRead, String> {
        use aterm_secrets::service::{Job, State};
        let paths = vault_paths()?;
        let Some(config) = vault_config(&paths)? else {
            return Ok(VaultRead::NoVault);
        };
        if aterm_secrets::service::subscriber_turned_off(&paths.home) {
            return Ok(VaultRead::Off);
        }
        let name = &config.name;
        Ok(
            match aterm_secrets::service::state(&paths.home, Job::Subscriber)
                .map_err(|why| why.to_string())?
            {
                State::Installed { pid: Some(pid), .. } => {
                    let program =
                        aterm_secrets::service::kept_program(&paths.home, Job::Subscriber)
                            .map_or_else(
                                || "its program unread".to_owned(),
                                |program| program.display().to_string(),
                            );
                    VaultRead::Runs(format!(
                        "vault {name}'s subscriber runs (pid {pid}, {program})"
                    ))
                }
                State::Installed { pid: None, .. } => VaultRead::Idle(format!(
                    "vault {name}'s subscriber is installed and not running"
                )),
                State::Absent => {
                    VaultRead::Idle(format!("vault {name}'s subscriber is not installed"))
                }
            },
        )
    }

    fn fleet(&self) -> Result<FleetRead, String> {
        use aterm_agent::{FleetCheck, FleetSync};
        Ok(match aterm_agent::check_from_cache()? {
            FleetCheck::Idle(sync) => FleetRead::Idle(sync.report()),
            FleetCheck::Current => FleetRead::Current(FleetSync::Current.report()),
            FleetCheck::Due(why) => FleetRead::Due(why),
        })
    }

    fn sync_fleet(&self) -> Result<FleetDone, String> {
        Ok(match aterm_agent::sync_from_cache()? {
            aterm_agent::FleetSync::Blocked(why) => FleetDone::Blocked(why),
            synced => FleetDone::Synced(synced.report()),
        })
    }

    fn shared_data(&self) -> Result<SharedRead, String> {
        use aterm_secrets::commons::Observation;
        let config = aterm_types::dirs::aterm_config_path()
            .ok_or("no config path: neither $XDG_CONFIG_HOME nor $HOME is set")?;
        if !aterm_db::runtime::enabled(&config).map_err(|why| why.to_string())? {
            return Ok(SharedRead::Off);
        }
        let paths = vault_paths()?;
        Ok(
            match aterm_secrets::commons::observe(&paths).map_err(|why| why.to_string())? {
                Observation::Unavailable(reason) => {
                    SharedRead::Unavailable(format!("{} ({})", reason.explanation(), reason.code()))
                }
                Observation::Visible(snapshot) => SharedRead::Visible(format!(
                    "vault {}'s Commons control, seq {}: this Mac enrolls itself while a window runs",
                    snapshot.vault(),
                    snapshot.sequence()
                )),
            },
        )
    }

    fn accounts(&self) -> Result<AccountsRead, String> {
        let (env, home, roster) = accounts_env()?;
        Ok(accounts_at(
            env.config.as_deref(),
            &home,
            &env.state,
            &roster,
            env.now,
        ))
    }

    fn resync_accounts(&self) -> Result<String, String> {
        let (env, home, roster) = accounts_env()?;
        let resync = aterm_agent::harness::accounts_sync::resync_for_setup(
            &home, &env.state, &roster, env.now, false,
        );
        if let Some(why) = resync.failed {
            return Err(why);
        }
        if !resync.cli_failed.is_empty() {
            let failed: Vec<String> = resync
                .cli_failed
                .iter()
                .map(|(cli, why)| format!("{}: {why}", cli.id()))
                .collect();
            return Err(failed.join("; "));
        }
        Ok(match (resync.synced, resync.changed) {
            (true, true) => "resynced the roster from its sources",
            (true, false) => "read the roster's sources; nothing changed",
            (false, _) => "no sync was called for",
        }
        .to_owned())
    }

    fn fabric(&self) -> Result<FabricRead, String> {
        use aterm_link::enable::{BrokerRenewal, Renew};
        Ok(
            match aterm_link::enable::renew_replaced_broker(Renew::Look)? {
                BrokerRenewal::Off => FabricRead::NotApplicable(FABRIC_OFF.to_owned()),
                BrokerRenewal::Current(now) => FabricRead::Current(now),
                BrokerRenewal::Kept(why) => FabricRead::NotApplicable(why),
                BrokerRenewal::Due(which) | BrokerRenewal::Renewed(which) => {
                    FabricRead::Replaced(which)
                }
            },
        )
    }

    fn renew_broker(&self) -> Result<String, String> {
        use aterm_link::enable::{BrokerRenewal, Renew};
        Ok(
            match aterm_link::enable::renew_replaced_broker(Renew::Apply)? {
                BrokerRenewal::Renewed(said) => format!("renewed the broker on this build: {said}"),
                BrokerRenewal::Off => format!("nothing renewed: {FABRIC_OFF}"),
                BrokerRenewal::Current(now) | BrokerRenewal::Due(now) => {
                    format!("nothing renewed: {now}")
                }
                BrokerRenewal::Kept(why) => format!("left as it is: {why}"),
            },
        )
    }

    fn peers(&self, look: bool) -> Result<PeersRead, String> {
        let ops = aterm_machines::init_secrets::LiveSecrets::unattended(&vault_paths()?.home);
        #[cfg(target_os = "macos")]
        let looks: &dyn Fn() -> aterm_machines::sync::peers::Heard =
            &aterm_cli::macos_doctor::look_for_the_alab_vault;
        #[cfg(not(target_os = "macos"))]
        let looks: &dyn Fn() -> aterm_machines::sync::peers::Heard =
            &|| aterm_machines::sync::peers::Heard::Unreached("not macOS".to_owned());
        aterm_machines::sync::peers::read(&ops, &peers_gate(), look.then_some(looks))
    }

    fn join_peers(&self) -> Result<PeersDone, String> {
        let sys = aterm_machines::port::Live::new();
        let ctx = aterm_machines::init::initial_setup_context(&sys)?;
        let ops = aterm_machines::init_secrets::unattended(&ctx)?;
        let mut journal = aterm_machines::port::System::journal(&sys)?;
        aterm_machines::sync::peers::join(&sys, &ctx, ops, journal.as_mut())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `[harness] accounts = "off"` turns the pass's accounts row off before anything else is
    /// read; the same Mac without it reads its accounts (here: no source in a scratch home).
    /// NEGATIVE CONTROL: a read that ignores the switch reads the off Mac as having no source.
    #[test]
    fn harness_accounts_off_turns_the_accounts_row_off() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let (home, state) = (scratch.path().join("home"), scratch.path().join("state"));
        let config = scratch.path().join("aterm.toml");
        let roster = scratch.path().join("accounts.toml");
        std::fs::write(&config, "[harness]\naccounts = \"off\"\n").unwrap();
        assert_eq!(
            accounts_at(Some(&config), &home, &state, &roster, 1_800_000_000),
            AccountsRead::Off
        );
        std::fs::write(&config, "[harness]\naccounts = \"wall\"\n").unwrap();
        assert!(matches!(
            accounts_at(Some(&config), &home, &state, &roster, 1_800_000_000),
            AccountsRead::NoSource(_)
        ));
        assert!(
            !home.exists() && !state.exists() && !roster.exists(),
            "the read wrote nothing"
        );
    }

    /// A window runs where the control socket folder holds a live instance's socket: not a dead
    /// one's, not a token alone, not the latest-instance link, and not this process's own.
    /// NEGATIVE CONTROL: a read that skips the liveness check takes a dead instance's socket
    /// for a window.
    #[test]
    fn a_window_runs_only_where_a_live_instance_holds_a_socket() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let dir = scratch.path();
        let (live, dead) = (4_242_u32, 4_243_u32);
        let alive = |pid: u32| pid == live;
        assert!(!window_in(dir, &alive), "an empty folder");
        assert!(!window_in(&dir.join("missing"), &alive), "no folder at all");
        for name in [
            aterm_types::control_socket::instance_sock_name(dead),
            aterm_types::control_socket::instance_token_name(live),
            aterm_types::control_socket::LATEST_SOCK_FILE.to_owned(),
            aterm_types::control_socket::instance_sock_name(std::process::id()),
        ] {
            std::fs::write(dir.join(name), b"").unwrap();
        }
        assert!(
            !window_in(dir, &|pid| pid == live || pid == std::process::id()),
            "a dead instance, a token, the link and this process are no window"
        );
        std::fs::write(
            dir.join(aterm_types::control_socket::instance_sock_name(live)),
            b"",
        )
        .unwrap();
        assert!(window_in(dir, &alive));
    }
}
