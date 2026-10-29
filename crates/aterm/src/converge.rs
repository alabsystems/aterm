// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The converge pass's live ports (`aterm init --converge`, [`aterm_machines::sync`]): each sync
//! kind read and applied through the library that owns it, here where `aterm` composes them.
//!
//! `sync.fabric` turns on a fabric the person never turned off with `aterm fabric on`'s own steps
//! ([`aterm_link::enable::turn_on_by_default`]; never under CI), and renews a broker that runs a
//! build an update replaced, or brings up the job `on` installed where it serves none, with
//! `on`'s broker step alone ([`aterm_link::enable::renew_replaced_broker`]). `sync.peers` joins
//! the open ALab vault from the window's pass with System State's own join
//! ([`aterm_machines::sync::peers`]), on macOS, where System State runs. `sync.shared-data` founds
//! an open vault's Commons on its only admin ([`aterm_secrets::client::commons_foundable`]).
//! `sync.requests` runs this Mac's pass over the lab's credential requests, and the custodian's
//! on a Mac that can sign with the vault's key ([`aterm_db::credentials`]).
//! `sync.shared-data`, `sync.steward.revoke` and `sync.steward.lease` publish to the vault, so at
//! a URL on macOS a pass that publishes on aterm's network waits while Local Network is untold or
//! refused ([`lan_waits`]), and none asks. A subscriber pass handed to the installed app uses that
//! app's answer; a held-copy fallback keeps its job's network, whose rounds reach the vault
//! ([`publish_lan`]).

use aterm_machines::init::{Base, BaseLock};
use aterm_machines::sync::peers::Lan;
use aterm_machines::sync::{
    AccountsRead, FabricRead, FleetDone, FleetRead, IdentityRead, NodeRead, PeersDone, PeersRead,
    Ports, Publisher, RequestsRead, SharedRead, StewardLeaseRead, StewardRevokeRead,
    StewardStandbyServerRead, VaultRead,
};
use aterm_secrets::client::Foundable;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// This Mac, as the converge pass reads and changes it.
pub(crate) struct Live;

/// `sync.fabric` on a Mac with no fabric command, as the renewal reads it.
const FABRIC_OFF: &str = "off: no [fabric] command (`aterm fabric on` turns it on)";

/// Whether this pass runs under CI (`CI` set), where no pass turns the fabric on.
fn under_ci() -> bool {
    std::env::var_os("CI").is_some()
}

/// `sync.fabric`'s read of a fabric with no command, as `unset` reads it: the person's off and a
/// CI run are not this pass's; a fabric never turned off is, where the program the turn-on names
/// for the broker is one that stays: an `aterm` on `$PATH` (`on_path`,
/// [`aterm_link::enable::names_aterm_on_path`]), or `exe`, this process's canonical executable,
/// the installed release app. A build or a held copy found on no `$PATH` would name its own file,
/// which can go, and leave the broker's job a program that no longer starts.
fn fabric_unset(
    unset: Option<aterm_link::enable::Unset>,
    ci: bool,
    on_path: bool,
    exe: Option<&Path>,
) -> FabricRead {
    use aterm_link::enable::Unset;
    let kept = on_path || exe.is_some_and(atpkg::hooks::is_installed_release_exe);
    match unset {
        None => FabricRead::Current("on: [fabric] command is set".to_owned()),
        Some(Unset::PersonOff(why)) => FabricRead::NotApplicable(why),
        Some(Unset::Never(from)) if ci => {
            FabricRead::NotApplicable(format!("{from}: under CI (`CI` set) no pass turns it on"))
        }
        Some(Unset::Never(from)) if !kept => FabricRead::NotApplicable(format!(
            "{from}: this aterm is a build or a copy, not the installed app, and no `aterm` is on \
             $PATH, so no pass names it the broker's program: `aterm fabric on` from the aterm \
             you keep turns it on"
        )),
        Some(Unset::Never(from)) => FabricRead::Unset(from),
    }
}

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

/// Local Network as aterm last heard it: a use of it raises no question once macOS asked aterm
/// before.
#[cfg(target_os = "macos")]
fn lan_heard() -> Lan {
    use aterm_cli::macos_doctor::LocalNetworkHeard;
    match aterm_cli::macos_doctor::local_network_heard() {
        LocalNetworkHeard::On | LocalNetworkHeard::Asked => Lan::Allowed,
        LocalNetworkHeard::Off => Lan::Refused,
        LocalNetworkHeard::Untold => Lan::Untold,
    }
}

/// What stands before `sync.peers`' join besides the vault: System State's plan (the Secrets
/// vault left out, or a Stop holding it, keeps the vault out), a run of its that still owes its
/// Secrets stage (which joins the vault itself), and Local Network as aterm last heard it
/// ([`lan_heard`]).
#[cfg(target_os = "macos")]
fn peers_gate() -> aterm_machines::sync::peers::Gate {
    use aterm_cli::macos_doctor::{
        initial_setup_keeps_vault_sync, initial_setup_owes_secrets_stage,
    };
    let off = match initial_setup_keeps_vault_sync() {
        Ok(true) => None,
        Ok(false) => {
            Some("System State's plan leaves the Secrets vault out, or a Stop holds it".to_owned())
        }
        Err(why) => Some(format!("System State's records cannot be read ({why})")),
    };
    aterm_machines::sync::peers::Gate {
        off,
        owed: initial_setup_owes_secrets_stage().unwrap_or(true),
        lan: lan_heard(),
    }
}

/// Elsewhere System State does not run, and joins no vault.
#[cfg(not(target_os = "macos"))]
fn peers_gate() -> aterm_machines::sync::peers::Gate {
    aterm_machines::sync::peers::Gate {
        off: Some("System State joins a vault on macOS alone".to_owned()),
        owed: false,
        lan: Lan::Untold,
    }
}

/// Elsewhere no Local Network question exists.
#[cfg(not(target_os = "macos"))]
const fn lan_heard() -> Lan {
    Lan::Allowed
}

/// What a publication to `config`'s vault (a founding of its Commons, the steward's revoke repair
/// or lease) goes through, out on `by`'s network: a vault at a path is no network use; at a URL,
/// the vault subscriber's held-copy fallback publishes on its job's network, whose own rounds
/// reach that vault. A subscriber pass handed to the installed app uses aterm's network and its
/// own Local Network answer, as every other app pass does ([`lan_heard`]).
fn publish_lan(config: &aterm_secrets::client::Config, by: Publisher) -> Lan {
    let url = matches!(config.source, aterm_secrets::transport::Source::Url(_));
    publish_through(url, by, lan_heard)
}

/// [`publish_lan`] for a vault at a URL (`url`) or a path, `heard` reading aterm's answer.
fn publish_through(url: bool, by: Publisher, heard: impl FnOnce() -> Lan) -> Lan {
    if url && by == Publisher::Aterm {
        heard()
    } else {
        Lan::Allowed
    }
}

/// Why `what`, a publication to the vault, waits on macOS's Local Network answer `lan`: `None`
/// where a use of it raises no question.
fn lan_waits(lan: Lan, what: &str) -> Option<String> {
    match lan {
        Lan::Allowed => None,
        Lan::Refused => Some(format!(
            "macOS does not let aterm use the local network (Don't Allow, or turned off since), \
             so {what} waits: System Settings > Privacy & Security > Local Network turns aterm on"
        )),
        Lan::Untold => Some(format!(
            "macOS has not asked aterm about the local network yet, so {what} waits for System \
             State's Local Network row to ask; no pass raises the question"
        )),
    }
}

/// `sync.shared-data`'s read of a Mac no control record reaches (`unseen` says so):
/// `foundable`, through `lan`. Its vault's only admin founds an open vault's Commons once Local
/// Network raises no question (Refused and Untold wait on a person) and nothing else holds it
/// for a person ([`Foundable::Waiting`]: an approver that shares no root, which waits, naming
/// it); one that is not this Mac's to found reads unavailable, naming why where the read says,
/// as does a read the vault's busy state lock put off to the next pass; and any other Mac (a
/// member, or an admin whose root is published) reads unavailable as the observation says.
fn unseen_read(unseen: String, foundable: Foundable, lan: Lan) -> SharedRead {
    match foundable {
        Foundable::NotAdmin | Foundable::Founded => SharedRead::Unavailable(unseen),
        Foundable::Elsewhere(why) => SharedRead::Unavailable(format!("{unseen}; {why}")),
        Foundable::Waiting(why) => SharedRead::Deferred(why),
        Foundable::Busy => SharedRead::Unavailable(format!(
            "{unseen}; the vault's state is changing, so the next pass reads whether this Mac \
             founds it"
        )),
        Foundable::Due(due) => match lan_waits(lan, "the founding") {
            None => SharedRead::Foundable(due),
            Some(why) => SharedRead::Deferred(format!("{due}; {why}")),
        },
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
/// `"off"` (`accounts_sync_off`, the switch every automatic sync reads; read first, so an off
/// Mac reads nothing else), no source where none is configured or found, due wherever the
/// apply's resync would sync (`resync_due`, its own rule: a missing roster, a source that
/// changed, another CLI's pool with a row to add), else current.
fn accounts_at(
    config: Option<&Path>,
    home: &Path,
    state: &Path,
    roster: &Path,
    now: i64,
) -> AccountsRead {
    use aterm_agent::harness::accounts_sync::{
        accounts_sync_off, has_resync_accounts_source, resync_due,
    };
    if accounts_sync_off(config) {
        return AccountsRead::Off;
    }
    if !has_resync_accounts_source(home, state, roster, now) {
        return AccountsRead::NoSource(
            "no fleet accounts source is configured or found on this Mac".to_owned(),
        );
    }
    match resync_due(home, state, roster, now) {
        Some(why) => AccountsRead::Due(why),
        None => AccountsRead::Current("the roster is in step with its sources".to_owned()),
    }
}

/// Whether `aterm.toml` at `config_path` explicitly turns swarm off (`[swarm] enabled = false`).
fn swarm_disabled_at(config_path: &Path) -> Result<bool, String> {
    let config =
        aterm_uds::atomic_file::ConfigFile::open(config_path).map_err(|e| e.to_string())?;
    let Some(text) = config.text().map_err(|e| e.to_string())? else {
        return Ok(false);
    };
    aterm_secrets::swarm::enabled_in_toml(text)
        .map(|on| !on)
        .map_err(|e| format!("aterm.toml: {e}"))
}

/// Local hostnames consulted as a last-resort fallback (`LocalHostName`, `ComputerName`, LAN and
/// kernel host names) when `aterm-swarm-roster` has not yet assigned a row for this Mac.
fn local_hostnames() -> Vec<String> {
    let mut out = Vec::new();
    let mut push = |candidate: Option<String>| {
        if let Some(name) = candidate {
            let trimmed = name.trim();
            if !trimmed.is_empty() && !out.iter().any(|existing| existing == trimmed) {
                out.push(trimmed.to_owned());
            }
        }
    };
    #[cfg(target_os = "macos")]
    for key in ["LocalHostName", "ComputerName"] {
        let mut cmd = aterm_uds::spawnfd::command("/usr/sbin/scutil");
        cmd.args(["--get", key]);
        if let Ok(output) = cmd.output()
            && output.status.success()
            && let Ok(text) = std::str::from_utf8(&output.stdout)
        {
            push(Some(text.trim().to_owned()));
        }
    }
    push(aterm_uds::process::lan_host_name());
    push(aterm_uds::process::kernel_host_name());
    out
}

/// Read `[ai] machine` and `[fleet] machine` from `config_path`, returning `(ai_machine,
/// fleet_machine)` (`None` when the file or key is absent).
fn read_config_machines(config_path: &Path) -> Result<(Option<String>, Option<String>), String> {
    let config =
        aterm_uds::atomic_file::ConfigFile::open(config_path).map_err(|e| e.to_string())?;
    let Some(text) = config.text().map_err(|e| e.to_string())? else {
        return Ok((None, None));
    };
    let doc: aterm_toml::Table = aterm_toml::from_str(text)
        .map_err(|e| format!("aterm.toml is not valid TOML, so it is left as it was: {e}"))?;
    let get = |section: &str| -> Result<Option<String>, String> {
        let Some(item) = doc.get(section) else {
            return Ok(None);
        };
        let table = item
            .as_table()
            .ok_or_else(|| format!("aterm.toml's `{section}` is no table; edit it by hand"))?;
        Ok(table
            .get("machine")
            .and_then(aterm_toml::Value::as_str)
            .map(str::to_owned))
    };
    Ok((get("ai")?, get("fleet")?))
}

/// `sync.identity`'s read at `config_path` given `paths` and fallback `hostnames`.
fn identity_at(
    config_path: &Path,
    paths: &aterm_secrets::client::Paths,
    hostnames: &[String],
) -> Result<IdentityRead, String> {
    if swarm_disabled_at(config_path)? {
        return Ok(IdentityRead::NotApplicable(
            "off: [swarm] enabled = false".to_owned(),
        ));
    }
    let label = aterm_secrets::client::resolve_local_swarm_label(paths, hostnames)
        .map_err(|why| why.to_string())?;
    identity_from_label(config_path, label.as_deref())
}

/// Compare `config_path`'s `[ai] machine` and `[fleet] machine` against resolved `label`.
fn identity_from_label(config_path: &Path, label: Option<&str>) -> Result<IdentityRead, String> {
    let Some(label) = label else {
        return Ok(IdentityRead::NotApplicable(
            "no swarm label applies on this Mac (no ALab vault or swarm roster)".to_owned(),
        ));
    };
    let (ai, fleet) = read_config_machines(config_path)?;
    let target = format!("label {label} in [ai] and [fleet] machine");
    if ai.as_deref() == Some(label) && fleet.as_deref() == Some(label) {
        return Ok(IdentityRead::Current(target));
    }
    let shown = |opt: Option<&str>| opt.map_or_else(|| "unset".to_owned(), str::to_owned);
    Ok(IdentityRead::Due {
        from: format!(
            "[ai] machine = {}, [fleet] machine = {}",
            shown(ai.as_deref()),
            shown(fleet.as_deref())
        ),
        to: target,
    })
}

/// Atomically write `[ai] machine = label` and `[fleet] machine = label` into `config_path`,
/// preserving unrelated tables and comments.
fn write_machine_label_at(config_path: &Path, label: &str) -> Result<String, String> {
    use aterm_toml::edit::{DocumentMut, Item, Table, Value};
    use aterm_uds::atomic_file::{ConfigFile, EditLanded};

    let config = ConfigFile::open(config_path).map_err(|e| e.to_string())?;
    let text = config.text().map_err(|e| e.to_string())?.unwrap_or("");
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e| format!("aterm.toml is not valid TOML, so it is left as it was: {e}"))?;
    for section in ["ai", "fleet"] {
        let root = doc.as_table_mut();
        if root.get(section).is_none_or(Item::is_none) {
            root.insert(section, Item::Table(Table::new()));
        }
        let table = root
            .get_mut(section)
            .and_then(Item::as_table_like_mut)
            .ok_or_else(|| format!("aterm.toml's `{section}` is no table; edit it by hand"))?;
        table.insert("machine", Item::Value(Value::from(label)));
    }
    let edited = doc.to_string();
    let after: aterm_toml::Table = aterm_toml::from_str(&edited)
        .map_err(|e| format!("edited aterm.toml would not be valid TOML: {e}"))?;
    for section in ["ai", "fleet"] {
        let got = after
            .get(section)
            .and_then(aterm_toml::Value::as_table)
            .and_then(|t| t.get("machine"))
            .and_then(aterm_toml::Value::as_str);
        if got != Some(label) {
            return Err(format!(
                "edited aterm.toml did not read back `{section}.machine = {label:?}`"
            ));
        }
    }
    if text != edited
        && let Err(e) = config.lock().and_then(|edit| edit.replace(&edited))
        && EditLanded::of(&e).is_none()
    {
        return Err(e.to_string());
    }
    Ok(format!(
        "wrote {label} into [ai] machine and [fleet] machine"
    ))
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

/// The installed release aterm.app's program in `apps_dirs` (the system folder first, as atpkg
/// looks), resolved, unless `own` (this process's program, resolved) is an installed release
/// itself, that app or another: where the subscriber's pass hands off to
/// ([`Ports::installed_release_elsewhere`]).
fn release_elsewhere(own: Option<&Path>, apps_dirs: &[PathBuf]) -> Option<PathBuf> {
    if own.is_some_and(atpkg::hooks::is_installed_release_exe) {
        return None;
    }
    apps_dirs
        .iter()
        .filter_map(|apps| {
            apps.join("aterm.app/Contents/MacOS/aterm")
                .canonicalize()
                .ok()
        })
        .find(|exe| atpkg::hooks::is_installed_release_exe(exe))
}

/// The installed app's run of a held copy's subscriber pass: `program args` on the held copy's
/// own stdio (null under the subscriber's runner; a person's terminal gets the rows), as its own
/// responsible process (the one place the converge pass disclaims responsibility, so macOS asks
/// about its Local Network use as aterm.app's, not the held copy's that started it).
fn hand_off_command(program: &Path, args: &[String]) -> aterm_uds::spawnfd::Command {
    let mut command = aterm_uds::spawnfd::command(program);
    command.args(args).disclaim_responsibility(true);
    command
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

    fn sleep(&self, duration: Duration) {
        if !duration.is_zero() {
            std::thread::sleep(duration);
        }
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

    fn swarm_enabled(&self) -> Result<bool, String> {
        aterm_secrets::swarm::enabled_live().map_err(|why| why.to_string())
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

    fn identity(&self) -> Result<IdentityRead, String> {
        let config = aterm_types::dirs::aterm_config_path()
            .ok_or("no config path: neither $XDG_CONFIG_HOME nor $HOME is set")?;
        identity_at(&config, &vault_paths()?, &local_hostnames())
    }

    fn sync_identity(&self) -> Result<String, String> {
        let config = aterm_types::dirs::aterm_config_path()
            .ok_or("no config path: neither $XDG_CONFIG_HOME nor $HOME is set")?;
        if swarm_disabled_at(&config)? {
            return Ok("left as it is: [swarm] enabled = false".to_owned());
        }
        let Some(label) =
            aterm_secrets::client::resolve_local_swarm_label(&vault_paths()?, &local_hostnames())
                .map_err(|why| why.to_string())?
        else {
            return Ok("no swarm label applies on this Mac".to_owned());
        };
        write_machine_label_at(&config, &label)
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
        match aterm_agent::try_sync_from_cache() {
            Ok(aterm_agent::FleetSync::Blocked(why)) => Ok(FleetDone::Blocked(why)),
            Ok(synced) => Ok(FleetDone::Synced(synced.report())),
            Err(err) if err.at_capacity => Ok(FleetDone::AtCapacity(err.why)),
            Err(err) => Err(err.why),
        }
    }

    fn node(&self) -> Result<NodeRead, String> {
        #[cfg(unix)]
        {
            use aterm_agent::node_service::{
                LiveSupervisor, SyncNodeState, read_for_sync_with, resolve_installed_program,
            };
            let home = aterm_types::dirs::home_dir().ok_or("no user home directory")?;
            let state = self.state_dir().ok_or("no state directory")?;
            let config = aterm_types::dirs::aterm_config_path();
            let current_exe = std::env::current_exe().and_then(|p| p.canonicalize()).ok();
            let program =
                resolve_installed_program(current_exe.as_deref(), self.installed_release(), &home);
            Ok(
                match read_for_sync_with(
                    &home,
                    &state,
                    config.as_deref(),
                    program.as_deref(),
                    under_ci(),
                    &LiveSupervisor,
                )? {
                    SyncNodeState::NotApplicable(why) => NodeRead::NotApplicable(why),
                    SyncNodeState::Current(now) => NodeRead::Current(now),
                    SyncNodeState::Due { from, to } => NodeRead::Due { from, to },
                },
            )
        }
        #[cfg(not(unix))]
        {
            Ok(NodeRead::NotApplicable(
                "systems.alab.aterm-node runs on macOS and Linux alone".to_owned(),
            ))
        }
    }

    fn ensure_node(&self) -> Result<String, String> {
        #[cfg(unix)]
        {
            use aterm_agent::node_service::{
                LiveSupervisor, ensure_for_sync_with, resolve_installed_program,
            };
            let home = aterm_types::dirs::home_dir().ok_or("no user home directory")?;
            let state = self.state_dir().ok_or("no state directory")?;
            let config = aterm_types::dirs::aterm_config_path();
            let current_exe = std::env::current_exe().and_then(|p| p.canonicalize()).ok();
            let program =
                resolve_installed_program(current_exe.as_deref(), self.installed_release(), &home);
            ensure_for_sync_with(
                &home,
                &state,
                config.as_deref(),
                program.as_deref(),
                under_ci(),
                &LiveSupervisor,
            )
        }
        #[cfg(not(unix))]
        {
            Ok("nothing changed: systems.alab.aterm-node runs on macOS and Linux alone".to_owned())
        }
    }

    fn steward_revoke(&self, by: Publisher) -> Result<StewardRevokeRead, String> {
        use aterm_secrets::client::{StewardRevoke, steward_revoke_check};
        let paths = vault_paths()?;
        Ok(
            match steward_revoke_check(&paths).map_err(|why| why.to_string())? {
                StewardRevoke::Idle(why) => StewardRevokeRead::Idle(why),
                StewardRevoke::Current(now) | StewardRevoke::Done(now) => {
                    StewardRevokeRead::Current(now)
                }
                StewardRevoke::Due { found, makes } => match vault_config(&paths)? {
                    Some(config) => {
                        match lan_waits(publish_lan(&config, by), "the steward's revoke cascade") {
                            None => StewardRevokeRead::Due { found, makes },
                            Some(why) => StewardRevokeRead::Deferred(format!("{found}; {why}")),
                        }
                    }
                    None => StewardRevokeRead::Idle("no vault is configured".to_owned()),
                },
            },
        )
    }

    fn apply_steward_revoke(&self) -> Result<String, String> {
        use aterm_secrets::client::{StewardRevoke, steward_finish_revokes};
        let paths = vault_paths()?;
        Ok(
            match steward_finish_revokes(&paths).map_err(|why| why.to_string())? {
                StewardRevoke::Done(said)
                | StewardRevoke::Current(said)
                | StewardRevoke::Idle(said)
                | StewardRevoke::Due { found: said, .. } => said,
            },
        )
    }

    fn steward_lease(&self, by: Publisher) -> Result<StewardLeaseRead, String> {
        use aterm_secrets::client::{StewardLease, steward_lease_check};
        let paths = vault_paths()?;
        let now_secs = aterm_time::unix_secs().unwrap_or(0);
        Ok(
            match steward_lease_check(&paths, None, now_secs).map_err(|why| why.to_string())? {
                StewardLease::Idle(why) => StewardLeaseRead::Idle(why),
                StewardLease::Current(now) | StewardLease::Done(now) => {
                    StewardLeaseRead::Current(now)
                }
                StewardLease::Due { found, makes } => match vault_config(&paths)? {
                    Some(config) => {
                        match lan_waits(publish_lan(&config, by), "the steward's lease") {
                            None => StewardLeaseRead::Due { found, makes },
                            Some(why) => StewardLeaseRead::Deferred(format!("{found}; {why}")),
                        }
                    }
                    None => StewardLeaseRead::Idle("no vault is configured".to_owned()),
                },
            },
        )
    }

    fn apply_steward_lease(&self) -> Result<String, String> {
        use aterm_secrets::client::{StewardLease, steward_ensure_lease};
        let paths = vault_paths()?;
        let now_secs = aterm_time::unix_secs().unwrap_or(0);
        Ok(
            match steward_ensure_lease(&paths, None, now_secs).map_err(|why| why.to_string())? {
                StewardLease::Done(said)
                | StewardLease::Current(said)
                | StewardLease::Idle(said)
                | StewardLease::Due { found: said, .. } => said,
            },
        )
    }

    fn steward_standby_server(&self) -> Result<StewardStandbyServerRead, String> {
        use aterm_secrets::client::{StewardStandbyServer, steward_standby_server_check};
        let paths = vault_paths()?;
        Ok(
            match steward_standby_server_check(&paths, None).map_err(|why| why.to_string())? {
                StewardStandbyServer::Idle(why) => StewardStandbyServerRead::Idle(why),
                StewardStandbyServer::Current(now) | StewardStandbyServer::Done(now) => {
                    StewardStandbyServerRead::Current(now)
                }
                StewardStandbyServer::Due { found, makes } => {
                    StewardStandbyServerRead::Due { found, makes }
                }
            },
        )
    }

    fn apply_steward_standby_server(&self) -> Result<String, String> {
        use aterm_secrets::client::{StewardStandbyServer, steward_ensure_standby_server};
        let paths = vault_paths()?;
        Ok(
            match steward_ensure_standby_server(&paths, None).map_err(|why| why.to_string())? {
                StewardStandbyServer::Done(said)
                | StewardStandbyServer::Current(said)
                | StewardStandbyServer::Idle(said)
                | StewardStandbyServer::Due { found: said, .. } => said,
            },
        )
    }

    fn shared_data(&self, by: Publisher) -> Result<SharedRead, String> {
        use aterm_secrets::commons::{Observation, Unavailable};
        let config = aterm_types::dirs::aterm_config_path()
            .ok_or("no config path: neither $XDG_CONFIG_HOME nor $HOME is set")?;
        if !aterm_db::runtime::enabled(&config).map_err(|why| why.to_string())? {
            return Ok(SharedRead::Off);
        }
        let paths = vault_paths()?;
        Ok(
            match aterm_secrets::commons::observe(&paths).map_err(|why| why.to_string())? {
                Observation::Unavailable(reason) => {
                    let unseen = format!("{} ({})", reason.explanation(), reason.code());
                    let vault = match reason {
                        Unavailable::ControlNotVisible => vault_config(&paths)?,
                        _ => None,
                    };
                    match vault {
                        Some(vault) => unseen_read(
                            unseen,
                            aterm_secrets::client::commons_foundable(&paths)
                                .map_err(|why| why.to_string())?,
                            publish_lan(&vault, by),
                        ),
                        None => SharedRead::Unavailable(unseen),
                    }
                }
                Observation::Visible(snapshot) => SharedRead::Visible(format!(
                    "vault {}'s Commons control, seq {}: this Mac enrolls itself while a window runs",
                    snapshot.vault(),
                    snapshot.sequence()
                )),
            },
        )
    }

    fn requests(&self) -> Result<RequestsRead, String> {
        let config = aterm_types::dirs::aterm_config_path()
            .ok_or("no config path: neither $XDG_CONFIG_HOME nor $HOME is set")?;
        let paths = vault_paths()?;
        let needs = aterm_secrets::need::peek(&paths).map_err(|why| why.to_string())?;
        let standing = aterm_secrets::need::status_line(&needs)
            .unwrap_or_else(|| "no needs on this Mac".to_owned());
        if !aterm_db::runtime::enabled(&config).map_err(|why| why.to_string())? {
            return Ok(RequestsRead::Idle(format!(
                "off: [commons] enabled = false ({standing})"
            )));
        }
        if !aterm_secrets::commons::bootstrap::writer_here(&paths) {
            return Ok(RequestsRead::Idle(format!(
                "this Mac is not a Commons Writer, so the lab's table is not reached ({standing})"
            )));
        }
        Ok(RequestsRead::Due(standing))
    }

    fn pass_requests(&self) -> Result<String, String> {
        let paths = vault_paths()?;
        let pass = aterm_db::credentials::pass(&paths).map_err(|why| why.to_string())?;
        let mut said = format!(
            "{} posted, {} answered, {} decided here, {} offered, {} swept",
            pass.posted, pass.answered, pass.decided, pass.offered, pass.swept
        );
        if let Some(custodian) =
            aterm_db::credentials::custodian_pass(&paths).map_err(|why| why.to_string())?
        {
            said.push_str(&format!(
                "; custodian: {} published, {} handed to a person, {} retired, {} waiting",
                custodian.published.len(),
                custodian.asked.len(),
                custodian.retired.len(),
                custodian.waiting.len()
            ));
        }
        Ok(said)
    }

    fn found_commons(&self) -> Result<String, String> {
        let done = aterm_secrets::client::admin_commons_found_by_default(&vault_paths()?)
            .map_err(|why| why.to_string())?;
        let network = aterm_codec::hex::encode_lower(&done.network);
        Ok(match (done.published, done.approved.as_slice()) {
            (true, []) => format!("founded Commons network {network}"),
            (true, approved) => format!(
                "founded Commons network {network}, approving {}: this Mac as its founder, \
                 the rest by the vault's open policy",
                approved.join(", ")
            ),
            (false, _) => format!("finished Commons network {network}, founded already"),
        })
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
                BrokerRenewal::Off => fabric_unset(
                    aterm_link::enable::unset_state()?,
                    under_ci(),
                    aterm_link::enable::names_aterm_on_path(),
                    std::env::current_exe()
                        .and_then(|exe| exe.canonicalize())
                        .ok()
                        .as_deref(),
                ),
                BrokerRenewal::Current(now) => FabricRead::Current(now),
                BrokerRenewal::Kept(why) => FabricRead::NotApplicable(why),
                BrokerRenewal::Due(which) | BrokerRenewal::Renewed(which) => FabricRead::Due(which),
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

    fn turn_on_fabric(&self) -> Result<String, String> {
        use aterm_link::enable::ByDefault;
        if under_ci() {
            return Ok("nothing turned on: under CI no pass turns the fabric on".to_owned());
        }
        Ok(match aterm_link::enable::turn_on_by_default()? {
            ByDefault::TurnedOn(said) => format!("turned the fabric on: {said}"),
            ByDefault::On => "nothing turned on: the fabric is on".to_owned(),
            ByDefault::Left(why) => format!("left as it is: {why}"),
        })
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

    fn log(&self, line: &str) {
        aterm_log::info!("{line}");
    }

    fn installed_release_elsewhere(&self) -> Option<PathBuf> {
        if !cfg!(target_os = "macos") {
            return None;
        }
        let own = std::env::current_exe()
            .and_then(|exe| exe.canonicalize())
            .ok();
        let mut apps = vec![PathBuf::from("/Applications")];
        apps.extend(aterm_types::dirs::home_dir().map(|home| home.join("Applications")));
        release_elsewhere(own.as_deref(), &apps)
    }

    fn run_elsewhere(&self, program: &Path, args: &[String]) -> Result<Option<i32>, String> {
        let mut child = hand_off_command(program, args)
            .spawn()
            .map_err(|why| why.to_string())?;
        let status = child.wait().map_err(|why| why.to_string())?;
        Ok(status.code())
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

    #[test]
    fn a_default_kimi_pool_is_a_converge_source_without_a_claude_source() {
        use aterm_agent::harness::{accounts, accounts_sync};

        let scratch = aterm_tempfile::tempdir().unwrap();
        let (home, state, config_dir) = (
            scratch.path().join("home"),
            scratch.path().join("state"),
            scratch.path().join("config"),
        );
        let secrets = home.join(".secrets");
        aterm_types::fs_restricted::ensure_private_dir(&secrets).unwrap();
        std::fs::create_dir_all(&config_dir).unwrap();
        let config = config_dir.join("aterm.toml");
        let roster = config_dir.join("accounts.toml");
        let configured = "[fleet]\nmachine = \"m16\"\n";
        std::fs::write(&config, configured).unwrap();
        std::fs::write(&roster, "").unwrap();
        let kimi = accounts::roster_beside_for(&config, accounts::Cli::Kimi).unwrap();
        let source = secrets.join(accounts_sync::cli_fleet_file(accounts::Cli::Kimi));
        let fleet = concat!(
            "[[account]]\nlabel = \"kimi-1\"\nemail = \"kimi-one@example.com\"\n",
            "machines = [\"m16\"]\nregion = \"global\"\n",
        );
        std::fs::write(&source, fleet).unwrap();
        let now = 1_800_000_000;
        assert!(
            !accounts_sync::has_init_accounts_source(&home, &state, &roster, now),
            "the fixture has no Claude source"
        );
        std::fs::write(&config, "[fleet]\nmachine = \"m22\"\n").unwrap();
        assert!(
            matches!(
                accounts_at(Some(&config), &home, &state, &roster, now),
                AccountsRead::NoSource(_)
            ),
            "a default pool does not admit an unlisted machine"
        );
        std::fs::write(&config, configured).unwrap();
        assert!(
            matches!(
                accounts_at(Some(&config), &home, &state, &roster, now),
                AccountsRead::Due(why) if why.contains("kimi")
            ),
            "a listed Kimi pool is due by default without a local roster"
        );

        std::fs::write(
            &config,
            format!("{configured}[harness]\naccounts = \"off\"\n"),
        )
        .unwrap();
        assert_eq!(
            accounts_at(Some(&config), &home, &state, &roster, now),
            AccountsRead::Off,
            "the explicit off switch still wins"
        );
        std::fs::write(&config, configured).unwrap();
        let read = accounts_at(Some(&config), &home, &state, &roster, now);
        assert_eq!(std::fs::read_to_string(&source).unwrap(), fleet);
        assert!(std::fs::read(&roster).unwrap().is_empty());
        assert!(!kimi.exists(), "the probe creates no local Kimi roster");
        assert!(!state.exists(), "the probes write no account state");
        assert!(matches!(&read, AccountsRead::Due(_)), "{read:?}");

        // Retained state without a roster records a pool this machine left.
        let store = accounts::Store::at_for(&state, accounts::Cli::Kimi, &home);
        let mut left = accounts::State::default();
        left.accounts
            .insert("kimi-1".into(), accounts::Account::default());
        store.save(&left).unwrap();
        let before = std::fs::read(store.state_file()).unwrap();
        assert!(
            matches!(
                accounts_at(Some(&config), &home, &state, &roster, now + 1),
                AccountsRead::NoSource(_)
            ),
            "the default does not rejoin a pool this machine left"
        );
        assert_eq!(std::fs::read(store.state_file()).unwrap(), before);
        assert!(!kimi.exists(), "the left pool keeps its roster absent");
    }

    #[test]
    fn a_default_codex_pool_reads_and_applies_without_either_accounts_roster() {
        use aterm_agent::harness::{accounts, accounts_sync};

        let scratch = aterm_tempfile::tempdir().unwrap();
        let (home, state, config_dir) = (
            scratch.path().join("home"),
            scratch.path().join("state"),
            scratch.path().join("config"),
        );
        let secrets = home.join(".secrets");
        aterm_types::fs_restricted::ensure_private_dir(&secrets).unwrap();
        std::fs::create_dir_all(&config_dir).unwrap();
        let config = config_dir.join("aterm.toml");
        let roster = config_dir.join("accounts.toml");
        let codex = accounts::roster_beside_for(&config, accounts::Cli::Codex).unwrap();
        let configured = "[fleet]\nmachine = \"m16\"\n";
        let source = secrets.join(accounts_sync::cli_fleet_file(accounts::Cli::Codex));
        let fleet =
            "[[account]]\nlabel='codex-1'\nemail='codex-one@example.test'\nmachines=['m16']\n";
        std::fs::write(&source, fleet).unwrap();
        std::fs::write(&config, format!("{configured}[harness]\naccounts='off'\n")).unwrap();
        let now = 1_800_000_000;
        assert_eq!(
            accounts_at(Some(&config), &home, &state, &roster, now),
            AccountsRead::Off
        );
        assert!(!roster.exists() && !codex.exists() && !state.exists());

        std::fs::write(&config, configured).unwrap();
        assert!(!accounts_sync::has_init_accounts_source(
            &home, &state, &roster, now
        ));
        let before = accounts_at(Some(&config), &home, &state, &roster, now);
        let due = accounts_sync::resync_due(&home, &state, &roster, now);
        assert!(!roster.exists() && !codex.exists() && !state.exists());
        let applied = accounts_sync::resync_for_setup(&home, &state, &roster, now, false);
        let after = accounts_at(Some(&config), &home, &state, &roster, now + 1);
        assert!(
            applied.synced
                && applied.changed
                && applied.failed.is_none()
                && applied.cli_failed.is_empty(),
            "{applied:?}"
        );
        assert!(std::fs::read_to_string(&codex).unwrap().contains("codex-1"));
        assert!(!roster.exists() && !config_dir.join(accounts_sync::FLEET_ROSTER_FILE).exists());
        for cli in [accounts::Cli::Grok, accounts::Cli::Kimi] {
            assert!(!accounts::roster_beside_for(&config, cli).unwrap().exists());
        }
        assert_eq!(std::fs::read_to_string(source).unwrap(), fleet);
        assert!(
            matches!(before, AccountsRead::Due(_))
                && due.as_deref().is_some_and(|why| why.contains("codex"))
                && matches!(after, AccountsRead::Current(_)),
            "before={before:?}; due={due:?}; applied={applied:?}; after={after:?}"
        );
    }

    /// A fabric with no command reads as `unset` says: the person's off and a CI run are left
    /// alone, a fabric never turned off is the pass's to turn on, where the program it names for
    /// the broker stays (an `aterm` on `$PATH`, or this process the installed app); a build, a
    /// copy on a mounted volume or an unread executable found on no `$PATH` leaves it, since it
    /// would name its own file. The live port passes only its process's reads.
    /// NEGATIVE CONTROLS: a read that ignores `CI` hands a CI run's pass a fabric to turn on; one
    /// that ignores the program hands a worktree build's pass a broker job naming that build; one
    /// that reads only `$PATH` leaves the installed app's fabric off.
    #[test]
    fn a_fabric_never_turned_off_is_the_passs_outside_ci() {
        use aterm_link::enable::Unset;
        let never = || Some(Unset::Never("off: never turned off".to_owned()));
        let app = Path::new("/Applications/aterm.app/Contents/MacOS/aterm");
        let build = Path::new("/Users//x/aterm-wt.noindex/target/debug/aterm");
        let mounted = Path::new("/Volumes/aterm/aterm.app/Contents/MacOS/aterm");
        assert!(matches!(
            fabric_unset(never(), false, true, Some(build)),
            FabricRead::Unset(_)
        ));
        assert!(matches!(
            fabric_unset(never(), false, false, Some(app)),
            FabricRead::Unset(_)
        ));
        assert!(matches!(
            fabric_unset(never(), true, true, Some(app)),
            FabricRead::NotApplicable(why) if why.contains("CI")
        ));
        for exe in [Some(build), Some(mounted), None] {
            assert!(
                matches!(
                    fabric_unset(never(), false, false, exe),
                    FabricRead::NotApplicable(why) if why.contains("not the installed app")
                ),
                "{exe:?}"
            );
        }
        assert!(matches!(
            fabric_unset(Some(Unset::PersonOff("off by your choice".to_owned())), false, true, Some(app)),
            FabricRead::NotApplicable(why) if why == "off by your choice"
        ));
        assert!(matches!(
            fabric_unset(None, false, false, None),
            FabricRead::Current(_)
        ));
    }

    /// A Mac no control record reaches founds its vault's Commons only where the read says due
    /// and Local Network raises no question: refused or untold waits on a person, as a founding
    /// held for a person (an old approver) does, whatever Local Network says. A read that is not
    /// this Mac's to found names why; a member's reads as the observation says.
    /// NEGATIVE CONTROL: a read that skips the Local Network gate founds where macOS never asked;
    /// one that reads a held founding as not this Mac's leaves it out of the pass's log line.
    #[test]
    fn a_founding_waits_for_local_network_and_names_why_it_is_not_this_macs() {
        let unseen = || "no Commons control entry is visible (control-not-visible)".to_owned();
        let found = "vault alab is open and has no Commons control";
        let due = || Foundable::Due(found.to_owned());
        assert_eq!(
            unseen_read(unseen(), due(), Lan::Allowed),
            SharedRead::Foundable(found.to_owned())
        );
        for (lan, says) in [
            (Lan::Refused, "Don't Allow"),
            (Lan::Untold, "has not asked"),
        ] {
            let read = unseen_read(unseen(), due(), lan);
            assert!(
                matches!(&read, SharedRead::Deferred(why) if why.starts_with(found) && why.contains(says)),
                "{lan:?}: {read:?}"
            );
        }
        let held = "vault alab: this Mac's approver (/x/aterm, aterm 0.142.0) shares no root";
        for lan in [Lan::Allowed, Lan::Refused, Lan::Untold] {
            assert_eq!(
                unseen_read(unseen(), Foundable::Waiting(held.to_owned()), lan),
                SharedRead::Deferred(held.to_owned()),
                "{lan:?}"
            );
        }
        for foundable in [Foundable::NotAdmin, Foundable::Founded] {
            assert_eq!(
                unseen_read(unseen(), foundable, Lan::Allowed),
                SharedRead::Unavailable(unseen())
            );
        }
        assert_eq!(
            unseen_read(
                unseen(),
                Foundable::Elsewhere("vault alab: it names 2 admins".to_owned()),
                Lan::Allowed
            ),
            SharedRead::Unavailable(format!("{}; vault alab: it names 2 admins", unseen()))
        );
        assert!(
            matches!(
                unseen_read(unseen(), Foundable::Busy, Lan::Allowed),
                SharedRead::Unavailable(why) if why.starts_with(&unseen())
            ),
            "a busy read founds nothing and fails nothing"
        );
    }

    /// A held copy's subscriber pass hands off to the installed release app: the first one the
    /// folders hold (the system folder first), never to the program that runs, and never from a
    /// program that is an installed release itself (another copy of the app included).
    /// NEGATIVE CONTROL: a lookup that does not ask whether its own program is an installed
    /// release hands an installed app's pass to another installed app.
    #[test]
    fn a_held_copy_hands_off_to_the_installed_app_and_an_installed_app_to_none() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let root = scratch.path().canonicalize().unwrap();
        let app = |dir: &str| {
            let macos = root.join(dir).join("aterm.app/Contents/MacOS");
            std::fs::create_dir_all(&macos).unwrap();
            std::fs::write(macos.join("aterm"), b"").unwrap();
            macos.join("aterm")
        };
        let (system, user) = (root.join("Applications"), root.join("home/Applications"));
        let held = root.join("share/aterm-secrets/aterm");
        assert_eq!(
            release_elsewhere(Some(&held), &[system.clone(), user.clone()]),
            None,
            "no app"
        );

        let in_user = app("home/Applications");
        let in_system = app("Applications");
        let both = [system, user];
        assert_eq!(
            release_elsewhere(Some(&held), &both),
            Some(in_system.clone())
        );
        assert_eq!(release_elsewhere(None, &both), Some(in_system.clone()));
        assert_eq!(
            release_elsewhere(Some(&in_system), &both),
            None,
            "the app itself"
        );
        assert_eq!(
            release_elsewhere(Some(&in_user), &both),
            None,
            "another installed app"
        );
        let dev = root.join("dev/aterm (dev).app/Contents/MacOS/aterm");
        assert_eq!(release_elsewhere(Some(&dev), &both), Some(in_system));
    }

    /// The hand-off runs the installed app with the held copy's words, as its own responsible
    /// process (so macOS asks about its Local Network use as aterm.app's).
    /// NEGATIVE CONTROL: a hand-off that keeps the held copy responsible fails here.
    #[test]
    fn the_hand_off_runs_the_app_as_its_own_responsible_process() {
        let args = ["init", "--converge", "--trigger", "subscriber"].map(str::to_owned);
        let program = Path::new("/Applications/aterm.app/Contents/MacOS/aterm");
        let command = hand_off_command(program, &args);
        assert_eq!(command.get_program(), program.as_os_str());
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            args.iter().map(std::ffi::OsStr::new).collect::<Vec<_>>()
        );
        assert!(command.get_disclaim_responsibility());
    }

    /// The installed app's publications use its Local Network answer, including a subscriber
    /// pass handed to it. A held-copy fallback keeps its job's network; a path vault uses none.
    /// Untold and refused answers on the app's network defer the founding; allowed leaves it due.
    /// NEGATIVE CONTROLS: a bypass admits an untold publication, and gating the fallback reads
    /// an answer its job does not use.
    #[test]
    fn only_aterms_network_waits_on_its_local_network_answer() {
        for heard in [Lan::Untold, Lan::Refused, Lan::Allowed] {
            for (url, by, gated) in [
                (true, Publisher::Aterm, true),
                (true, Publisher::Subscriber, false),
                (false, Publisher::Aterm, false),
                (false, Publisher::Subscriber, false),
            ] {
                let reads = std::cell::Cell::new(0);
                let lan = publish_through(url, by, || {
                    reads.set(reads.get() + 1);
                    heard
                });
                let want = if gated { heard } else { Lan::Allowed };
                assert_eq!(lan, want, "url={url} {by:?} {heard:?}");
                assert_eq!(
                    reads.get(),
                    u32::from(gated),
                    "only this app's answer is read"
                );
                let commons = unseen_read(
                    "no Commons control".to_owned(),
                    Foundable::Due("found Commons".to_owned()),
                    lan,
                );
                if want == Lan::Allowed {
                    assert!(matches!(commons, SharedRead::Foundable(_)));
                } else {
                    assert!(matches!(commons, SharedRead::Deferred(_)));
                }
            }
        }
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

    /// `sync.identity` writes the resolved swarm label into `[ai] machine` and `[fleet] machine`
    /// while preserving unrelated settings, ignores `~/.aterm/machine.toml`, and leaves personal
    /// vaults and `[swarm] enabled = false` untouched.
    /// NEGATIVE CONTROL: reading `~/.aterm/machine.toml` would adopt `m22` onto a clone or
    /// personal vault instead of using the swarm roster (or `NotApplicable`).
    #[test]
    fn sync_identity_writes_ai_and_fleet_machine_and_ignores_legacy_machine_toml() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let home = scratch.path().join("home");
        std::fs::create_dir_all(home.join(".aterm")).unwrap();
        // Legacy file from F7: must never be read by sync.identity.
        std::fs::write(home.join(".aterm/machine.toml"), "id = \"m22\"\n").unwrap();
        let paths = aterm_secrets::client::Paths::for_home(&home);
        let config = scratch.path().join("aterm.toml");
        std::fs::write(&config, "[harness]\naccounts = \"wall\"\n").unwrap();

        // No vault / personal vault without roster -> NotApplicable, despite ~/.aterm/machine.toml.
        assert!(matches!(
            identity_at(&config, &paths, &["Mac-mini-M4.local".to_owned()]).unwrap(),
            IdentityRead::NotApplicable(_)
        ));

        // Roster-resolved label -> Due, then write_machine_label_at -> Current, keeping [harness].
        let due = identity_from_label(&config, Some("m112")).unwrap();
        assert!(
            matches!(&due, IdentityRead::Due { from, to } if from.contains("unset") && to.contains("m112")),
            "{due:?}"
        );
        let said = write_machine_label_at(&config, "m112").unwrap();
        assert!(said.contains("m112"), "{said}");
        assert_eq!(
            identity_from_label(&config, Some("m112")).unwrap(),
            IdentityRead::Current("label m112 in [ai] and [fleet] machine".to_owned())
        );
        let written = std::fs::read_to_string(&config).unwrap();
        let parsed: aterm_toml::Table = aterm_toml::from_str(&written).unwrap();
        assert_eq!(parsed["ai"]["machine"].as_str(), Some("m112"));
        assert_eq!(parsed["fleet"]["machine"].as_str(), Some("m112"));
        assert_eq!(parsed["harness"]["accounts"].as_str(), Some("wall"));

        // [swarm] enabled = false stands aside as NotApplicable.
        std::fs::write(&config, "[swarm]\nenabled = false\n").unwrap();
        assert_eq!(
            identity_at(&config, &paths, &["m15.local".to_owned()]).unwrap(),
            IdentityRead::NotApplicable("off: [swarm] enabled = false".to_owned())
        );
    }
}
