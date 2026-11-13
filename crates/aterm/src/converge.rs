// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The converge pass's live ports (`aterm init --converge`, [`aterm_machines::sync`]): each sync
//! kind read and applied through the library that owns it, here where `aterm` composes them.
//!
//! `sync.fabric` turns on a fabric the person never turned off with `aterm fabric on`'s own steps
//! ([`aterm_link::enable::turn_on_by_default_with`]; never under CI), and renews a broker that
//! runs a build an update replaced, or brings up the job `on` installed where it serves none, with
//! `on`'s broker step alone ([`aterm_link::enable::renew_replaced_broker_with`]). With the mesh on
//! (`aterm_types::mesh_switch::MESH`, read once into [`Live`]) it installs, renews and restarts no
//! broker job: the node service (`systems.alab.aterm-node`) hosts `bus.log` and renews it, and
//! retires the job an earlier build installed, so the turn-on's steps write no job and the
//! renewal reads left as it is. `sync.peers` joins
//! the open ALab vault from the window's pass with System State's own join
//! ([`aterm_machines::sync::peers`]), on macOS, where System State runs. `sync.shared-data` founds
//! an open vault's Commons on its only admin ([`aterm_secrets::client::commons_foundable`]).
//! `sync.requests` runs this Mac's pass over the lab's credential requests, and the custodian's
//! on a Mac that can sign with the vault's key ([`aterm_db::credentials`]).
//! `sync.clone-tuning` tunes each clone of the vault's inventory as its repository declares
//! ([`aterm_machines::clone_tuning`]), reading the inventory on disk, under System State's holds
//! as they stand when it reads and when it applies ([`setup_holds`]).
//! `sync.shared-data`, `sync.steward.revoke` and `sync.steward.lease` publish to the vault, so for
//! a vault on the LAN, which aterm reaches through the LAN broker on macOS, a pass waits while the
//! broker does not answer ([`publish_waits`]).
//! `sync.awake` starts the installed app's aCaffeine mug when none runs, so the subscriber's pass
//! keeps a Mac with no window awake ([`aterm_acaffeine::cli::keep_installed_awake`], which first
//! retires the old `aterm caffeine` name's login jobs no aterm will run again), retires those old
//! login jobs directly when the mug already runs ([`aterm_acaffeine::cli::retire_old_jobs`]), and
//! never turns a person's off on: neither keep-awake's off nor, at the subscriber's pass, Enable
//! at Startup's.

use aterm_machines::init::{Base, BaseLock};
use aterm_machines::sync::{
    AccountsRead, AwakeRead, FabricRead, FleetDone, FleetRead, GithubRead, IdentityRead, NodeRead,
    PeersDone, PeersRead, Ports, ReleaseRead, RequestsRead, SharedDone, SharedRead,
    StewardLeaseRead, StewardRevokeRead, StewardStandbyServerRead, VaultRead,
};
use aterm_secrets::client::{Foundable, NotFounded};
use aterm_types::machine_name::{GeneratedNames, Number};
use std::path::{Path, PathBuf};
use std::time::Duration;

mod repository_tools;

/// This Mac, as the converge pass reads and changes it.
pub(crate) struct Live {
    /// Who keeps the fabric's broker (`sync.fabric`): [`aterm_link::enable::Service::Node`] with
    /// the mesh on, where the node service hosts `bus.log` and the pass installs or renews no
    /// broker job, else the platform's supervisor; read once ([`Live::here`]).
    fabric_service: aterm_link::enable::Service,
}

impl Live {
    /// The pass as this build runs it: the fabric's broker kept as
    /// `aterm_types::mesh_switch::MESH` says ([`aterm_link::enable::Service::default_for`]).
    pub(crate) const fn here() -> Self {
        Self {
            fabric_service: aterm_link::enable::Service::default_for(
                aterm_types::mesh_switch::MESH,
            ),
        }
    }
}

/// `sync.fabric` on a Mac with no fabric command, as the renewal reads it.
const FABRIC_OFF: &str = "off: no [fabric] command (`aterm fabric on` turns it on)";

/// `sync.awake`'s read of what macOS holds ([`aterm_acaffeine::held::Held`]): `installed` finds
/// the installed app a mug starts from (in `/Applications` or `~/Applications`, with the
/// `acaffeine` verb), asked only when keep-awake is on and no mug runs here.
///
/// # Errors
/// The mug runs and the power manager gave no answer, or no mug runs and the installed app
/// did not answer whether it has the verb.
fn awake_of(
    installed: impl FnOnce() -> aterm_acaffeine::launchd::Installed,
    held: aterm_acaffeine::held::Held,
) -> Result<AwakeRead, String> {
    use aterm_acaffeine::held::Held;
    use aterm_acaffeine::launchd::Installed;
    let because = |why: Option<String>| why.map_or_else(String::new, |why| format!(": {why}"));
    let off = || {
        AwakeRead::NotApplicable(
            "aCaffeine is off by your choice (`aterm acaffeine on` turns it on)".to_owned(),
        )
    };
    Ok(match held {
        Held::NotHere(why) => AwakeRead::NotApplicable(why),
        Held::Off => off(),
        Held::Held { pid } => AwakeRead::Current(format!(
            "the mug (pid {pid}) holds macOS's idle-sleep assertion"
        )),
        Held::Paused { pid, why, .. } => AwakeRead::Current(format!(
            "the mug (pid {pid}) runs, paused while {}",
            match why {
                aterm_acaffeine::model::Pause::LidClosed => "the lid is closed",
                aterm_acaffeine::model::Pause::Asleep => "this Mac is in a dark wake",
            }
        )),
        Held::NotHeld { pid, why } => AwakeRead::Asking(format!(
            "the mug (pid {pid}) runs and asks macOS again every minute{}",
            because(why)
        )),
        Held::Stopped { why } => match installed() {
            Installed::At(_) => AwakeRead::Due(format!("no aCaffeine mug runs{}", because(why))),
            Installed::Absent(why) => AwakeRead::NotApplicable(why),
            Installed::Unanswered(why) => return Err(why),
        },
        Held::Unread { pid, why } => {
            return Err(format!("the mug (pid {pid}) runs; {why}"));
        }
    })
}

/// How long `sync.awake`'s apply waits for the mug it started to hold its assertion, so the
/// read back finds it held.
const AWAKE_SETTLE: std::time::Duration = std::time::Duration::from_secs(20);

/// Whether this pass runs under CI (`CI` set), where no pass turns the fabric on.
fn under_ci() -> bool {
    std::env::var_os("CI").is_some()
}

/// The file this account's installed command resolves to, where an installer recorded one
/// that is safe to read and its executable is there; `None` otherwise.
fn declared_command() -> Option<PathBuf> {
    let home = aterm_types::dirs::home_dir()?;
    aterm_types::installed_command::selected(&home, aterm_uds::peer::our_uid())
        .ok()
        .flatten()?
        .executable()
        .canonicalize()
        .ok()
}

/// `sync.fabric`'s read of a fabric with no command, as `unset` reads it: the person's off and a
/// CI run are not this pass's; a fabric never turned off is, where the program the turn-on names
/// for the broker is one that stays: an `aterm` on `$PATH` (`on_path`,
/// [`aterm_link::enable::names_aterm_on_path`]), or `exe`, this process's canonical executable,
/// the installed release app or the file the account's installed command resolves to
/// (`declared`, [`aterm_types::installed_command`]: where an install script, not an app bundle,
/// placed aterm, and a job's `$PATH` may not reach). A build or a held copy found on no `$PATH`
/// would name its own file, which can go, and leave the broker's job a program that no longer
/// starts.
fn fabric_unset(
    unset: Option<aterm_link::enable::Unset>,
    ci: bool,
    on_path: bool,
    exe: Option<&Path>,
    declared: Option<&Path>,
) -> FabricRead {
    use aterm_link::enable::Unset;
    let kept = on_path
        || exe.is_some_and(|exe| {
            atpkg::hooks::is_installed_release_exe(exe) || declared == Some(exe)
        });
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

const REPOSITORIES_SYNCING: &str = "the selected secrets vault changed or is still syncing; repositories wait for a complete version";

/// Repository inputs belong to the current account even when its subscriber has no console.
/// Capture needs no keyboard, desktop, host inventory or subprocess.
fn repository_context(
    sys: &dyn aterm_machines::port::System,
) -> Result<aterm_machines::items::Ctx, String> {
    let identity = sys.identity()?;
    if identity.user.uid != sys.euid() || sys.euid() == 0 {
        return Err("repository setup must run as its local user, without sudo".into());
    }
    Ok(aterm_machines::items::Ctx {
        facts: aterm_machines::model::HostFacts::default(),
        users: vec![identity.user],
        console: None,
        host_name: String::new(),
        cache_dir: PathBuf::new(),
        disrupt: false,
    })
}

/// Explicit personal profiles keep their own source. A vault-managed inventory is read only
/// when that configured vault has admitted and completely materialized this account.
fn repository_vault(
    paths: &aterm_secrets::client::Paths,
    sys: &dyn aterm_machines::port::System,
    ctx: &aterm_machines::items::Ctx,
) -> Result<Option<aterm_secrets::client::Config>, String> {
    if !aterm_machines::init_github::uses_vault_inventory(sys, ctx)? {
        return Ok(None);
    }
    vault_config(paths)
}

/// Capture the default source once under full readiness. All later Git work runs
/// outside this lock, and its caller fences publication with the retained snapshot.
fn repository_inputs(
    paths: &aterm_secrets::client::Paths,
    config: &aterm_secrets::client::Config,
    sys: &dyn aterm_machines::port::System,
    ctx: &aterm_machines::items::Ctx,
) -> Result<
    Option<
        aterm_secrets::client::Materialized<aterm_machines::init_github::InitialRepositoryInputs>,
    >,
    String,
> {
    aterm_secrets::client::with_materialization_ready_for_entries(
        paths,
        config,
        &aterm_machines::init_github::VAULT_INPUT_ENTRIES,
        || {
            aterm_machines::init_github::InitialRepositoryInputs::capture(sys, ctx)
                .map_err(aterm_secrets::Error::new)
        },
    )
    .map_err(|why| why.to_string())
}

/// What stands before `sync.peers`' join besides the vault: System State's plan (the Secrets
/// vault left out, or a Stop holding it, keeps the vault out), a run of its that still owes its
/// Secrets stage (which joins the vault itself), and a LAN broker that does not answer
/// ([`aterm_machines::lan_reach::broker_unready`]), through which the look reaches the ALab
/// vault's `.local` address.
#[cfg(target_os = "macos")]
fn peers_gate() -> aterm_machines::sync::peers::Gate {
    aterm_machines::sync::peers::Gate {
        off: vault_held(),
        owed: aterm_cli::macos_doctor::initial_setup_owes_secrets_stage().unwrap_or(true),
        broker: aterm_machines::lan_reach::broker_unready(),
    }
}

/// Why System State's plan holds this Mac's vault out ([`vault_hold`] of
/// `initial_setup_keeps_vault_sync`): the one reading `sync.peers`' gate and every other Sync row
/// ([`Ports::vault_held`]) share.
#[cfg(target_os = "macos")]
fn vault_held() -> Option<String> {
    vault_hold(aterm_cli::macos_doctor::initial_setup_keeps_vault_sync())
}

/// Elsewhere no System State runs, so nothing holds the vault.
#[cfg(not(target_os = "macos"))]
const fn vault_held() -> Option<String> {
    None
}

/// What System State holds now (`initial_setup_holds`): `sync.clone` forgets the clone `aterm
/// init` owed only for GitHub repositories left as they are since the run that owes it began,
/// never for one that run went past because the person named the group (`aterm init --only
/// github`), and says so in `aterm init`'s held line; a Stop, or records a newer aterm saved,
/// hold the clone and keep it owed ([`aterm_machines::init::owed_clone`]).
#[cfg(target_os = "macos")]
pub(super) fn setup_holds() -> aterm_machines::init::Holds {
    aterm_cli::macos_doctor::initial_setup_holds()
}

/// Elsewhere no System State runs, so nothing holds the repositories.
#[cfg(not(target_os = "macos"))]
pub(super) const fn setup_holds() -> aterm_machines::init::Holds {
    aterm_machines::init::Holds::NONE
}

/// The folder whose records [`setup_holds`] reads, where it reads any.
#[cfg(target_os = "macos")]
pub(super) fn setup_records() -> Option<std::path::PathBuf> {
    aterm_cli::macos_doctor::initial_setup_records()
}

/// Elsewhere [`setup_holds`] reads nothing.
#[cfg(not(target_os = "macos"))]
pub(super) const fn setup_records() -> Option<std::path::PathBuf> {
    None
}

/// The plan's answer as a hold: `keeps` false (the Secrets vault left out, or a Stop holding
/// setup) holds the vault, and so do records that cannot be read, since a Stop may be among them.
#[cfg(target_os = "macos")]
fn vault_hold(keeps: Result<bool, String>) -> Option<String> {
    match keeps {
        Ok(true) => None,
        Ok(false) => {
            Some("System State's plan leaves the Secrets vault out, or a Stop holds it".to_owned())
        }
        Err(why) => Some(format!("System State's records cannot be read ({why})")),
    }
}

/// Elsewhere System State does not run, and joins no vault.
#[cfg(not(target_os = "macos"))]
fn peers_gate() -> aterm_machines::sync::peers::Gate {
    aterm_machines::sync::peers::Gate {
        off: Some("System State joins a vault on macOS alone".to_owned()),
        owed: false,
        broker: None,
    }
}

/// Why `what`, a publication to `config`'s vault (a founding of its Commons, the steward's revoke
/// repair or lease), waits: a vault on the LAN ([`aterm_machines::lan_reach::on_the_lan`]) is
/// reached through the LAN broker, so it waits while the broker does not answer (`broken`,
/// [`aterm_machines::lan_reach::broker_unready`]'s reading, asked only then); a vault at a path,
/// or at a URL off the LAN, waits for nothing.
fn publish_waits(
    config: &aterm_secrets::client::Config,
    what: &str,
    broken: impl FnOnce() -> Option<String>,
) -> Option<String> {
    let aterm_secrets::transport::Source::Url(url) = &config.source else {
        return None;
    };
    if !aterm_machines::lan_reach::on_the_lan(url) {
        return None;
    }
    broken().map(|why| format!("{why}, so {what} waits for it"))
}

/// `sync.shared-data`'s read of a Mac no control record reaches (`unseen` says so):
/// `foundable`, `waits` saying why a founding would wait ([`publish_waits`]). Its vault's only
/// admin founds an open vault's Commons once nothing waits and nothing else holds it for a
/// person ([`Foundable::Waiting`]: an approver that shares no root, which waits, naming
/// it); one that is not this Mac's to found reads unavailable, naming why where the read says,
/// as does a read the vault's busy state lock put off to the next pass; and any other Mac (a
/// member, or an admin whose root is published) reads unavailable as the observation says.
fn unseen_read(
    unseen: String,
    foundable: Foundable,
    waits: impl FnOnce(&str) -> Option<String>,
) -> SharedRead {
    match foundable {
        Foundable::NotAdmin | Foundable::Founded => SharedRead::Unavailable(unseen),
        Foundable::Elsewhere(why) => SharedRead::Unavailable(format!("{unseen}; {why}")),
        Foundable::Waiting(why) => SharedRead::Deferred(why),
        Foundable::Busy => SharedRead::Unavailable(format!(
            "{unseen}; the vault's state is changing, so the next pass reads whether this Mac \
             founds it"
        )),
        Foundable::Due(due) => match waits("the founding") {
            None => SharedRead::Foundable(due),
            Some(why) => SharedRead::Deferred(format!("{due}; {why}")),
        },
    }
}

/// `sync.shared-data`'s read of `observed`, the vault's Commons control as this Mac sees it once
/// a sync that held the vault's state lock let it go, waited for up to
/// [`aterm_secrets::commons::SETTLE`] ([`aterm_secrets::commons::observe_until_settled`]):
/// a state still busy reads busy and reads nothing else, which fails no founding read back over
/// it; a Mac no control record reaches reads as [`unseen_read`] says, through its vault's config
/// (`vault`), whether it founds (`foundable`) and what a founding waits on (`waits`); one it
/// reaches reads visible.
fn shared_read(
    observed: aterm_secrets::commons::Observation,
    vault: impl FnOnce() -> Result<Option<aterm_secrets::client::Config>, String>,
    foundable: impl FnOnce() -> Result<Foundable, String>,
    waits: impl FnOnce(&aterm_secrets::client::Config, &str) -> Option<String>,
) -> Result<SharedRead, String> {
    use aterm_secrets::commons::{Observation, Unavailable};
    Ok(match observed {
        Observation::Unavailable(Unavailable::Busy) => SharedRead::Busy(format!(
            "{} ({}): the next pass reads it",
            Unavailable::Busy.explanation(),
            Unavailable::Busy.code()
        )),
        Observation::Unavailable(reason) => {
            let unseen = format!("{} ({})", reason.explanation(), reason.code());
            let vault = match reason {
                Unavailable::ControlNotVisible => vault()?,
                _ => None,
            };
            match vault {
                Some(vault) => unseen_read(unseen, foundable()?, |what| waits(&vault, what)),
                None => SharedRead::Unavailable(unseen),
            }
        }
        Observation::Visible(snapshot) => SharedRead::Visible(format!(
            "vault {}'s Commons control, seq {}: this Mac enrolls itself while a window runs",
            snapshot.vault(),
            snapshot.sequence()
        )),
    })
}

/// What `sync.shared-data`'s apply says of `done`: what it founded or finished; a founding that
/// stands until a person changes something waits for them; one that may pass by itself is the
/// error the next pass tries again.
fn found_said(
    done: Result<aterm_secrets::client::CommonsInit, NotFounded>,
) -> Result<SharedDone, String> {
    let done = match done {
        Ok(done) => done,
        Err(NotFounded::Stands(why)) => return Ok(SharedDone::Waiting(why)),
        Err(NotFounded::Again(why)) => return Err(why),
    };
    let network = aterm_codec::hex::encode_lower(&done.network);
    Ok(SharedDone::Done(
        match (done.published, done.approved.as_slice()) {
            (true, []) => format!("founded Commons network {network}"),
            (true, approved) => format!(
                "founded Commons network {network}, approving {}: this Mac as its founder, \
                 the rest by the vault's open policy",
                approved.join(", ")
            ),
            (false, _) => format!("finished Commons network {network}, founded already"),
        },
    ))
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
/// apply's resync would sync (`read_resync`, its own rule: a missing roster, a source that
/// changed, another CLI's pool with a row to add), else current.
fn accounts_at(
    config: Option<&Path>,
    home: &Path,
    state: &Path,
    roster: &Path,
    now: i64,
) -> AccountsRead {
    use aterm_agent::harness::accounts_sync::{ResyncRead, accounts_sync_off, read_resync};
    if accounts_sync_off(config) {
        return AccountsRead::Off;
    }
    match read_resync(home, state, roster, now) {
        ResyncRead::NoSource => AccountsRead::NoSource(
            "no fleet accounts source is configured or found on this Mac".to_owned(),
        ),
        ResyncRead::Due(why) => AccountsRead::Due(why),
        ResyncRead::Current => {
            AccountsRead::Current("the roster is in step with its sources".to_owned())
        }
    }
}

/// The Mac's own names setup's naming rule sets (`ComputerName`, `LocalHostName` and
/// `HostName`), as `scutil --get` reads them now: `None` where a read failed, the name is unset,
/// or the platform has none.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct OwnNames {
    /// `ComputerName`: what Finder, AirDrop and Sharing show.
    pub(crate) computer: Option<String>,
    /// `LocalHostName`: the Bonjour `.local` name.
    pub(crate) local_host: Option<String>,
    /// `HostName`: the configured hostname.
    pub(crate) host: Option<String>,
}

impl OwnNames {
    /// All three names, `LocalHostName` first.
    fn each(
        &self,
    ) -> [(
        &'static str,
        aterm_types::machine_name::OsSlot,
        Option<&str>,
    ); 3] {
        use aterm_types::machine_name::OsSlot;
        [
            (
                "Local hostname",
                OsSlot::MacLocalHostName,
                self.local_host.as_deref(),
            ),
            (
                "Computer Name",
                OsSlot::MacComputerName,
                self.computer.as_deref(),
            ),
            ("Hostname", OsSlot::MacHostName, self.host.as_deref()),
        ]
    }
}

/// [`OwnNames`] as `scutil` reads them on this Mac (none elsewhere).
fn own_names() -> OwnNames {
    #[cfg(target_os = "macos")]
    {
        let get = |key: &str| {
            let mut cmd = aterm_uds::spawnfd::command("/usr/sbin/scutil");
            cmd.args(["--get", key]);
            let output =
                aterm_uds::deadline::output_within(&mut cmd, std::time::Duration::from_secs(2))
                    .ok()
                    .filter(|output| output.status.success())?;
            let text = std::str::from_utf8(&output.stdout).ok()?.trim();
            (!text.is_empty()).then(|| text.to_owned())
        };
        OwnNames {
            computer: get("ComputerName"),
            local_host: get("LocalHostName"),
            host: get("HostName"),
        }
    }
    #[cfg(not(target_os = "macos"))]
    OwnNames::default()
}

/// Local hostnames consulted as a last-resort fallback (`LocalHostName`, `ComputerName` from
/// `own`, then the LAN and kernel host names) when `aterm-swarm-roster` has not yet assigned a row
/// for this Mac.
fn local_hostnames(own: &OwnNames) -> Vec<String> {
    let mut out = Vec::with_capacity(4);
    let mut push = |candidate: Option<&str>| {
        if let Some(name) = candidate {
            let trimmed = name.trim();
            if !trimmed.is_empty() && !out.iter().any(|existing| existing == trimmed) {
                out.push(trimmed.to_owned());
            }
        }
    };
    push(own.local_host.as_deref());
    push(own.computer.as_deref());
    push(aterm_uds::process::lan_host_name().as_deref());
    push(aterm_uds::process::kernel_host_name().as_deref());
    out
}

/// The two `aterm.toml` keys `sync.identity` keeps: `[ai] machine` and `[fleet] machine`.
const MACHINE_KEYS: [&str; 2] = ["ai", "fleet"];

/// What `[identity] name` says about this Mac's name: its earlier explicit name applies until
/// Rename This Mac chooses a number; `name = false` turns identity naming off.
#[derive(Debug, Clone, PartialEq, Eq)]
enum NameChoice {
    /// Not set (or `true`): the swarm roster's label names this Mac.
    Roster,
    /// `name = false`: no pass names this Mac.
    Off,
    /// An earlier explicit name, which wins over the roster until a machine number is chosen.
    Yours(String),
}

/// What `aterm.toml` holds for `sync.identity`: the two machine keys, `[identity] name` and
/// whether `[identity] git = false` stands.
#[derive(Debug, Clone, PartialEq, Eq)]
struct IdentityConfig {
    /// `[ai] machine` and `[fleet] machine`, in [`MACHINE_KEYS`] order (`None` when unset).
    machines: [Option<String>; 2],
    name: NameChoice,
    /// `[identity] git = false`: the person keeps git's identity as theirs.
    git_off: bool,
}

/// Read what `sync.identity` reads from `config_path` (an absent file holds nothing).
#[cfg(test)]
fn read_identity_config(config_path: &Path) -> Result<IdentityConfig, String> {
    let config =
        aterm_uds::atomic_file::ConfigFile::open(config_path).map_err(|e| e.to_string())?;
    read_identity_snapshot(&config)
}

/// Read identity choices from `config`, or `None` when `[swarm] enabled = false`.
fn read_enabled_identity_snapshot(
    config: &aterm_uds::atomic_file::ConfigFile,
) -> Result<Option<IdentityConfig>, String> {
    let Some(text) = config.text().map_err(|e| e.to_string())? else {
        return Ok(Some(IdentityConfig {
            machines: [None, None],
            name: NameChoice::Roster,
            git_off: false,
        }));
    };
    let doc: aterm_toml::Table = aterm_toml::from_str(text)
        .map_err(|e| format!("aterm.toml is not valid TOML, so it is left as it was: {e}"))?;
    if !aterm_secrets::swarm::enabled_in_table(&doc).map_err(|e| format!("aterm.toml: {e}"))? {
        return Ok(None);
    }
    read_identity_table(&doc).map(Some)
}

/// Read identity choices from the snapshot an apply will compare before it writes.
fn read_identity_snapshot(
    config: &aterm_uds::atomic_file::ConfigFile,
) -> Result<IdentityConfig, String> {
    let Some(text) = config.text().map_err(|e| e.to_string())? else {
        return Ok(IdentityConfig {
            machines: [None, None],
            name: NameChoice::Roster,
            git_off: false,
        });
    };
    let doc: aterm_toml::Table = aterm_toml::from_str(text)
        .map_err(|e| format!("aterm.toml is not valid TOML, so it is left as it was: {e}"))?;
    read_identity_table(&doc)
}

fn read_identity_table(doc: &aterm_toml::Table) -> Result<IdentityConfig, String> {
    let mut read = IdentityConfig {
        machines: [None, None],
        name: NameChoice::Roster,
        git_off: false,
    };
    let table = |section: &str| -> Result<Option<&aterm_toml::Table>, String> {
        doc.get(section)
            .map(|item| {
                item.as_table()
                    .ok_or_else(|| format!("aterm.toml's `{section}` is no table; edit it by hand"))
            })
            .transpose()
    };
    for (slot, section) in read.machines.iter_mut().zip(MACHINE_KEYS) {
        *slot = table(section)?
            .and_then(|table| table.get("machine"))
            .and_then(aterm_toml::Value::as_str)
            .map(str::to_owned);
    }
    if let Some(identity) = table("identity")? {
        read.name = match identity.get("name") {
            None | Some(aterm_toml::Value::Boolean(true)) => NameChoice::Roster,
            Some(aterm_toml::Value::Boolean(false)) => NameChoice::Off,
            Some(aterm_toml::Value::String(name)) if !name.trim().is_empty() => {
                NameChoice::Yours(name.trim().to_owned())
            }
            Some(_) => {
                return Err(
                    "aterm.toml's [identity] name is neither a name nor false; edit it by hand"
                        .to_owned(),
                );
            }
        };
        read.git_off = matches!(identity.get("git"), Some(aterm_toml::Value::Boolean(false)));
    }
    Ok(read)
}

/// The side record of what `sync.identity` wrote into each machine key, under the account's
/// state directory: its last write, and the ones before it ([`EARLIER`]). A key that still holds
/// one of those is aterm's to update, and any other value is the person's. An absent record
/// means this aterm wrote nothing; only a pair of roster labels an earlier aterm wrote with no
/// record reads as aterm's then ([`unrecorded_label`]).
fn identity_record() -> Result<PathBuf, String> {
    aterm_types::dirs::account_state_dir()
        .map(|state| {
            state
                .join(aterm_converge::FileJournal::DIR)
                .join("identity-written.toml")
        })
        .ok_or_else(|| "no state directory to keep what sync.identity wrote".to_owned())
}

/// The record's table of the values aterm wrote into each key before its last write, newest
/// last: a pass cut off between recording a write and making it leaves the key at one of these,
/// which stays aterm's.
const EARLIER: &str = "earlier";

/// How many earlier writes the record keeps for each key.
const EARLIER_KEPT: usize = 8;

fn is_owned_plain_dir(meta: &std::fs::Metadata) -> bool {
    if !meta.file_type().is_dir() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if meta.uid() != aterm_uds::peer::our_uid() {
            return false;
        }
    }
    true
}

fn check_existing_record_dir(dir: &Path) -> Result<bool, String> {
    match std::fs::symlink_metadata(dir) {
        Ok(meta) if is_owned_plain_dir(&meta) => Ok(true),
        Ok(_) => Err(format!(
            "{}: not this user's plain directory",
            dir.display()
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("{}: {e}", dir.display())),
    }
}

fn check_record_dirs(record: &Path, create: bool) -> Result<(), String> {
    let Some(dir) = record.parent().filter(|p| !p.as_os_str().is_empty()) else {
        return Ok(());
    };
    if dir.file_name() == Some(std::ffi::OsStr::new(aterm_converge::FileJournal::DIR))
        && let Some(state_dir) = dir.parent().filter(|p| !p.as_os_str().is_empty())
        && !check_existing_record_dir(state_dir)?
        && create
    {
        std::fs::create_dir_all(state_dir).map_err(|e| format!("{}: {e}", state_dir.display()))?;
        if !check_existing_record_dir(state_dir)? {
            return Err(format!(
                "{}: not this user's plain directory",
                state_dir.display()
            ));
        }
    }
    if !check_existing_record_dir(dir)? && create {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        if !check_existing_record_dir(dir)? {
            return Err(format!(
                "{}: not this user's plain directory",
                dir.display()
            ));
        }
    }
    Ok(())
}

/// What `record` says aterm wrote into each key, in [`MACHINE_KEYS`] order: its last write
/// first, then the earlier ones newest first; `None` where there is no record.
fn read_history(record: &Path) -> Result<Option<[Vec<String>; 2]>, String> {
    check_record_dirs(record, false)?;
    let text = match aterm_uds::atomic_file::read_regular_text(
        record,
        aterm_uds::atomic_file::MAX_CONFIG_BYTES,
    ) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", record.display())),
    };
    let doc: aterm_toml::Table = aterm_toml::from_str(&text)
        .map_err(|e| format!("{} is not valid TOML: {e}", record.display()))?;
    let earlier = doc.get(EARLIER).and_then(aterm_toml::Value::as_table);
    Ok(Some(MACHINE_KEYS.map(|key| {
        let mut written: Vec<String> = doc
            .get(key)
            .and_then(aterm_toml::Value::as_str)
            .map(str::to_owned)
            .into_iter()
            .collect();
        if let Some(values) = earlier
            .and_then(|earlier| earlier.get(key))
            .and_then(aterm_toml::Value::as_array)
        {
            written.extend(
                values
                    .iter()
                    .rev()
                    .filter_map(aterm_toml::Value::as_str)
                    .map(str::to_owned),
            );
        }
        written
    })))
}

/// What `record` says aterm last wrote into each key, in [`MACHINE_KEYS`] order.
#[cfg(test)]
fn read_written(record: &Path) -> Result<[Option<String>; 2], String> {
    Ok(read_history(record)?.map_or([None, None], |history| {
        history.map(|written| written.into_iter().next())
    }))
}

/// Record that aterm writes `label` into each of `keys`, keeping what it wrote into the others
/// and, as earlier writes ([`EARLIER`]), what it wrote before, with what each of `keys` holds
/// now (`now`, in [`MACHINE_KEYS`] order): a key it writes is unset or aterm's own.
fn record_written(
    record: &Path,
    keys: &[&str],
    label: &str,
    now: &[Option<String>; 2],
) -> Result<(), String> {
    use aterm_toml::edit::{Array, DocumentMut, Item, Table, Value};
    let before = read_history(record)?.unwrap_or_default();
    let mut doc = DocumentMut::new();
    let mut earlier = Table::new();
    for ((key, mut written), now) in MACHINE_KEYS.into_iter().zip(before).zip(now) {
        if keys.contains(&key) {
            for value in now.iter().map(String::as_str).chain([label]) {
                written.retain(|was| was != value);
                written.insert(0, value.to_owned());
            }
        }
        written.truncate(EARLIER_KEPT + 1);
        let mut values = written.into_iter();
        if let Some(last) = values.next() {
            doc.as_table_mut()
                .insert(key, Item::Value(Value::from(last)));
        }
        let mut older: Vec<String> = values.collect();
        if !older.is_empty() {
            older.reverse();
            earlier.insert(
                key,
                Item::Value(Value::Array(older.into_iter().collect::<Array>())),
            );
        }
    }
    if earlier.iter().next().is_some() {
        doc.as_table_mut().insert(EARLIER, Item::Table(earlier));
    }
    check_record_dirs(record, true)?;
    aterm_uds::atomic_file::write(record, doc.to_string().as_bytes(), 0o600)
        .map_err(|e| format!("{}: {e}", record.display()))
}

/// Confirm at `record` that aterm's last write into each of `keys` landed: the values the record
/// kept from before it ([`EARLIER`]) go, so only that write reads as aterm's.
fn confirm_written(record: &Path, keys: &[&str]) -> Result<(), String> {
    use aterm_toml::edit::{DocumentMut, Item};
    check_record_dirs(record, false)?;
    let text =
        aterm_uds::atomic_file::read_regular_text(record, aterm_uds::atomic_file::MAX_CONFIG_BYTES)
            .map_err(|e| format!("{}: {e}", record.display()))?;
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e| format!("{} is not valid TOML: {e}", record.display()))?;
    let Some(earlier) = doc
        .as_table_mut()
        .get_mut(EARLIER)
        .and_then(Item::as_table_like_mut)
    else {
        return Ok(());
    };
    for key in keys {
        earlier.remove(key);
    }
    if earlier.is_empty() {
        doc.as_table_mut().remove(EARLIER);
    }
    aterm_uds::atomic_file::write(record, doc.to_string().as_bytes(), 0o600)
        .map_err(|e| format!("{}: {e}", record.display()))
}

/// Put `record` back as it was (`before`, `None` where there was none) after a write it named
/// did not land.
fn restore_record(record: &Path, before: Option<&[u8]>) -> Result<(), String> {
    check_record_dirs(record, before.is_some())?;
    match before {
        Some(bytes) => aterm_uds::atomic_file::write(record, bytes, 0o600),
        None => match std::fs::remove_file(record) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        },
    }
    .map_err(|e| format!("{}: {e}", record.display()))
}

/// Whether `machines` is a pair an aterm before the record wrote (v0.146 and v0.147 labelled
/// with none): both keys hold the same swarm roster label (`m<number>`), and the vault's roster
/// gives that label to this Mac, now or as one of its earlier labels (`ours`). Read only where
/// there is no record; any other pair is the person's: a guess that is wrong keeps a name, never
/// overwrites one.
fn unrecorded_label(machines: &[Option<String>; 2], ours: &dyn Fn(&str) -> bool) -> bool {
    match machines {
        [Some(ai), Some(fleet)] => {
            ai == fleet
                && ai
                    .strip_prefix('m')
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
                && ours(ai)
        }
        _ => false,
    }
}

/// One bounded roster snapshot for a name-follow pass.
fn read_name_roster(
    paths: &aterm_secrets::client::Paths,
) -> Result<Option<aterm_secrets::swarm_roster::SwarmRoster>, String> {
    aterm_secrets::client::swarm_roster_until(
        paths,
        std::time::Instant::now() + std::time::Duration::from_millis(250),
    )
    .map_err(|why| format!("swarm roster unread: {why}"))
}

/// Whether this snapshot gives `pair` to this Mac under its current `label`.
fn roster_gives(
    roster: Option<&aterm_secrets::swarm_roster::SwarmRoster>,
    label: Option<&str>,
    pair: &str,
) -> bool {
    let Some(label) = label else {
        return false;
    };
    roster.is_some_and(|roster| {
        roster
            .member_by_label_or_alias(pair)
            .is_some_and(|member| member.label == label)
    })
}

/// What `sync.identity` owes `aterm.toml`: how the row reads, and, where it is due, the name it
/// writes and the keys that take it.
struct IdentityOwed {
    read: IdentityRead,
    write: Option<(String, Vec<&'static str>)>,
}

type IdentityTarget = (String, Option<String>);

/// A number explicitly chosen through the machine-name editor is the local naming source.
/// Older, automatically assigned names remain evidence for ownership of existing projections.
/// Reading an absent file never creates a name or consults the vault. Callers inject the state
/// directory, so tests and isolated instances never read the person's machine-name file.
fn projected_machine_label(
    state: Option<&Path>,
    choice: &mut NameChoice,
    roster_label: Option<String>,
) -> Result<Option<String>, String> {
    if *choice == NameChoice::Off {
        return Ok(roster_label);
    }
    Ok(projected_label(
        choice,
        chosen_machine_number(state)?,
        roster_label,
    ))
}

fn chosen_machine_number(
    state: Option<&Path>,
) -> Result<Option<aterm_types::machine_name::Number>, String> {
    let Some(state) = state else {
        return Ok(None);
    };
    Ok(aterm_agent::machine_name::file::read(state)
        .map_err(|why| format!("this machine's name is unread: {why}"))?
        .filter(|name| name.class == aterm_types::machine_name::Class::Chosen)
        .and_then(|name| name.num))
}

/// Generate OS targets only from the canonical local identity, or a provisional
/// roster number before that record exists. An unread or mismatched record is no fallback.
fn generated_os_names(
    state: Option<&Path>,
    target: &str,
    label: Option<&str>,
) -> Result<Option<GeneratedNames>, String> {
    let Some(label) = label.filter(|label| *label == target) else {
        return Ok(None);
    };
    let Ok(number) = Number::parse(label) else {
        return Ok(None);
    };
    if number.to_string() != label {
        return Ok(None);
    }
    let Some(state) = state else {
        return Ok(None);
    };
    let name = aterm_agent::machine_name::file::read(state)
        .map_err(|why| format!("this machine's name is unread: {why}"))?;
    match name {
        None => Ok(Some(GeneratedNames::number_only(number))),
        Some(name) if name.num == Some(number) => Ok(Some(GeneratedNames::new(number, &name.nick))),
        Some(_) => Err("the canonical machine number differs from the OS naming source".into()),
    }
}

fn projected_label(
    choice: &mut NameChoice,
    chosen: Option<aterm_types::machine_name::Number>,
    roster_label: Option<String>,
) -> Option<String> {
    if chosen.is_some() && *choice != NameChoice::Off {
        // Rename This Mac is the newer explicit naming act; the older identity string is a
        // projection source, while `false` remains an explicit hold on identity naming.
        *choice = NameChoice::Roster;
    }
    chosen.map(|number| number.to_string()).or(roster_label)
}

fn identity_target_from(choice: &NameChoice, label: Option<String>) -> Option<IdentityTarget> {
    match (choice, label) {
        (NameChoice::Off, _) | (_, None) => None,
        (NameChoice::Yours(name), label) => Some((name.clone(), label)),
        (NameChoice::Roster, Some(label)) => Some((label.clone(), Some(label))),
    }
}

/// The naming source a pass may project. Comparing only its resolved target would miss a
/// person's newer policy that happens to resolve to the same name. Generated OS targets
/// include the saved nickname; unrelated configuration leaves this work admitted.
#[derive(Debug, PartialEq, Eq)]
struct IdentityProjection {
    target: IdentityTarget,
    choice: NameChoice,
    machines: [Option<String>; 2],
    chosen: Option<aterm_types::machine_name::Number>,
    names: Option<GeneratedNames>,
}

impl IdentityProjection {
    /// Read the policy again, then the canonical source last, after any blocking projection
    /// reads and immediately before a write. This read acquires no config, roster or book lock.
    fn check_current(
        &self,
        config: &Path,
        state: Option<&Path>,
        roster_label: Option<String>,
    ) -> Result<(), String> {
        if projection_target_now(config, state, roster_label)?.as_ref() == Some(self) {
            Ok(())
        } else {
            Err("this machine's naming choice changed during synchronization".to_owned())
        }
    }
}

/// The saved identity used by ordinary init after root returned and by `aterm name sync`.
/// This is OS-only: no roster lookup, policy write, fleet publication or fallback number.
#[cfg(any(target_os = "macos", test))]
fn synchronize_saved_name_with(
    sys: &dyn aterm_machines::port::System,
    state: &Path,
    config: &Path,
    current: &dyn Fn() -> Result<(), String>,
) -> Result<Option<String>, String> {
    use aterm_agent::machine_name::{file, publish};
    use aterm_machines::{model::Read, port::Cmd};
    current()?;
    let Some(saved) = file::read(state).map_err(|why| why.to_string())? else {
        return Ok(None);
    };
    let Some(number) = saved
        .num
        .filter(|_| saved.migration != Some(file::Migration::Waiting))
    else {
        return Ok(None);
    };
    let word = number.to_string();
    let names = GeneratedNames::new(number, &saved.nick);
    let Some(projection) = projection_target_now(config, Some(state), Some(word.clone()))? else {
        return Ok(None);
    };
    if projection.names.is_none() {
        return Ok(None);
    }
    if projection.names.as_ref() != Some(&names) {
        return Err("the saved machine number is not the current OS naming source".into());
    }
    let check = || {
        current()?;
        let now = file::read(state).map_err(|why| why.to_string())?;
        if !now.as_ref().is_some_and(|now| {
            now.num == Some(number)
                && now.nick == saved.nick
                && now.migration != Some(file::Migration::Waiting)
        }) {
            return Err("this machine's saved naming choice changed during synchronization".into());
        }
        projection.check_current(config, Some(state), Some(word.clone()))
    };
    struct Names<'a> {
        sys: &'a dyn aterm_machines::port::System,
        current: &'a dyn Fn() -> Result<(), String>,
    }
    impl publish::SystemNames for Names<'_> {
        fn read(&self) -> [Option<String>; 3] {
            aterm_machines::root_steps::NAME_PREFS.map(|pref| {
                self.sys
                    .run(&Cmd::new(aterm_machines::items::names::SCUTIL).args(["--get", pref]))
                    .ok()
                    .and_then(
                        |ran| match aterm_machines::items::names::read_scutil(&ran) {
                            Read::Value(name) => Some(name),
                            _ => None,
                        },
                    )
            })
        }
        fn apply(
            &self,
            _state: &Path,
            _config: &Path,
            names: &GeneratedNames,
        ) -> Result<(), String> {
            aterm_machines::root_steps::name_by_rule(self.sys, names, self.current).map(|_| ())
        }
    }
    publish::synchronize_system_names(
        state,
        config,
        &names,
        &Names {
            sys,
            current: &check,
        },
        &|| check().map_err(publish::Pending::Policy),
    )
    .map_err(|why| why.phrase())?;
    Ok(Some(format!(
        "macOS names match {} / {}",
        names.computer_name(),
        names.host_name()
    )))
}

/// Synchronize the saved macOS naming intent without entering setup or asking for authority.
/// The explicit CLI shares init's OS-only adapter and its fresh per-slot source checks.
pub(crate) fn sync_saved_name() -> Result<Option<String>, String> {
    #[cfg(target_os = "macos")]
    {
        use aterm_machines::port::System as _;
        let ports = Live::here();
        if !ports.own_account() {
            return Err("run in your own login and home directory".into());
        }
        let sys = aterm_machines::port::Live::new();
        let _stops = sys.defer_stops();
        name_sync_current(&sys, &|| aterm_machines::init::Holds::NONE)?;
        let state = ports
            .state_dir()
            .ok_or("no current account state directory")?;
        let config =
            aterm_types::dirs::aterm_config_path().ok_or("no current account configuration")?;
        synchronize_saved_name_with(&sys, &state, &config, &|| {
            name_sync_current(&sys, &setup_holds)
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("saved system-name synchronization is supported on macOS only".into())
    }
}

/// Naming has no repository selection: only a global Stop/newer record and the saved
/// naming projection can hold it. Agents in this account's graphic session need no prompt.
#[cfg(any(target_os = "macos", test))]
fn name_sync_current(
    sys: &dyn aterm_machines::port::System,
    holds: &dyn Fn() -> aterm_machines::init::Holds,
) -> Result<(), String> {
    if sys.euid() == 0 || aterm_machines::console::remote(sys) {
        return Err("macOS naming waits for this Mac's console account".into());
    }
    aterm_machines::init::name_current(sys, holds)
}

/// Read the naming source after the config write, retaining its semantic policy and chosen
/// number so every external writer can check it again under its existing effect boundary.
fn projection_target_now(
    config_path: &Path,
    state: Option<&Path>,
    roster_label: Option<String>,
) -> Result<Option<IdentityProjection>, String> {
    let snapshot =
        aterm_uds::atomic_file::ConfigFile::open(config_path).map_err(|e| e.to_string())?;
    let Some(mut identity) = read_enabled_identity_snapshot(&snapshot)? else {
        return Ok(None);
    };
    if identity.name == NameChoice::Off {
        return Ok(None);
    }
    let choice = identity.name.clone();
    let chosen = chosen_machine_number(state)?;
    let label = projected_label(&mut identity.name, chosen, roster_label);
    let Some(target) = identity_target_from(&identity.name, label) else {
        return Ok(None);
    };
    let names = generated_os_names(state, &target.0, target.1.as_deref())?;
    Ok(Some(IdentityProjection {
        target,
        choice,
        machines: identity.machines,
        chosen,
        names,
    }))
}

fn name_label(
    paths: &aterm_secrets::client::Paths,
    roster: Option<&aterm_secrets::swarm_roster::SwarmRoster>,
    hostnames: &[String],
) -> Result<Option<String>, String> {
    if let Some(roster) = roster {
        let identity =
            aterm_secrets::client::read_machine_identity(paths).map_err(|why| why.to_string())?;
        return Ok(identity.and_then(|identity| {
            roster
                .member_by_recipient(&identity.recipient().to_bech32())
                .filter(|member| !roster.is_member_denied(member))
                .map(|member| member.label.clone())
        }));
    }
    aterm_secrets::client::resolve_local_swarm_label(paths, hostnames)
        .map_err(|why| why.to_string())
}

/// A Mac name still to set, with the repair path the existing naming-rule reading admits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MacNamePending {
    /// The existing naming rule admits the next automatic repair attempt.
    ThroughRule(String),
    /// Setup still needs its one elevation to leave the naming rule.
    Elevation(String),
    /// Naming-rule metadata could not be read; no additional password is justified.
    Unread(String),
    /// macOS assigned a collision alias; retrying the same setter cannot resolve it.
    Collision(String),
}

impl MacNamePending {
    fn with_rule(why: String, rule: Option<bool>) -> Self {
        match rule {
            Some(true) => Self::ThroughRule(why),
            Some(false) => Self::Elevation(why),
            None => Self::Unread(why),
        }
    }
}

/// This Mac's one name as `aterm status` reads it ([`mac_names`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MacName {
    /// No name applies (why): swarm off, or `[identity] name = false`.
    Off(String),
    /// The vault's roster gave this Mac no label yet: it gives one at admission.
    Unlabelled,
    /// The name `sync.identity` gives the Mac (`target`), where its fleet name stands against it,
    /// and what still names the Mac's own names ([`own_name_pending`]).
    Named {
        target: String,
        fleet: aterm_agent::FleetName,
        pending: Option<MacNamePending>,
    },
}

/// [`MacName`] from local state alone: `aterm.toml`, the vault's roster and the fleet name on
/// disk, and the Mac's own names. It writes nothing and asks nothing.
///
/// # Errors
/// No config path, or a reader failed.
pub(crate) fn mac_names() -> Result<MacName, String> {
    let config = aterm_types::dirs::aterm_config_path()
        .ok_or("no config path: neither $XDG_CONFIG_HOME nor $HOME is set")?;
    let snapshot = aterm_uds::atomic_file::ConfigFile::open(&config).map_err(|e| e.to_string())?;
    let Some(mut identity) = read_enabled_identity_snapshot(&snapshot)? else {
        return Ok(MacName::Off("swarm off".to_owned()));
    };
    if matches!(identity.name, NameChoice::Off) {
        return Ok(MacName::Off("[identity] name = false".to_owned()));
    }
    let paths = vault_paths()?;
    let roster = read_name_roster(&paths)?;
    let mut own = None;
    let hostnames = if roster.is_some() {
        Vec::new()
    } else {
        local_hostnames(own.insert(own_names()))
    };
    let roster_label = name_label(&paths, roster.as_ref(), &hostnames)?;
    let label = projected_machine_label(
        aterm_types::dirs::account_state_dir().as_deref(),
        &mut identity.name,
        roster_label.clone(),
    )?;
    let Some((target, label)) = identity_target_from(&identity.name, label) else {
        return Ok(MacName::Unlabelled);
    };
    let fleet = aterm_agent::fleet_name_for(&target, &|word| {
        roster_gives(roster.as_ref(), roster_label.as_deref(), word)
    })?;
    let generated = generated_os_names(
        aterm_types::dirs::account_state_dir().as_deref(),
        &target,
        label.as_deref(),
    )?;
    let pending = generated
        .as_ref()
        .filter(|_| cfg!(target_os = "macos"))
        .and_then(|names| {
            let own = own.get_or_insert_with(own_names);
            own_name_pending(&target, Some(names), own).map(|why| {
                if own_name_repairable(&target, Some(names), own) {
                    MacNamePending::with_rule(
                        why,
                        aterm_machines::root_steps::rule_in_place(
                            &aterm_machines::port::Live::new(),
                        ),
                    )
                } else {
                    MacNamePending::Collision(why)
                }
            })
        });
    Ok(MacName::Named {
        target,
        fleet,
        pending,
    })
}

/// Where any of the Mac's own names (`ComputerName`, `LocalHostName` or `HostName`) does not
/// accept the projected `label` and `target` is that number: what names it. macOS lets only root
/// set them: setup's one early Mac password leaves the narrow rule, then the
/// ordinary init completion or `sync.identity` reads its current source and sets them
/// without another password once admission gives the number ([`aterm_machines::root_steps::name_by_rule`], [`own_name_by_rule`]). `None` where all
/// match, or a separate `[identity] name` differs from the projected number.
pub(crate) fn own_name_pending(
    target: &str,
    names: Option<&GeneratedNames>,
    own: &OwnNames,
) -> Option<String> {
    let names = names.filter(|names| names.number().to_string() == target)?;
    let each = own.each();
    let (which, slot, now) = each
        .into_iter()
        .find(|(_, slot, name)| name.is_none_or(|name| !names.accepts(*slot, name)))
        .or_else(|| {
            each.into_iter()
                .find(|(_, slot, name)| name.is_some_and(|name| names.collision(*slot, name)))
        })?;
    let expected = names.for_slot(slot)?;
    let now = now.unwrap_or("unread");
    if names.collision(slot, now) {
        Some(format!(
            "{which} reports {now} instead of {expected}; a name collision remains, so automatic setup leaves that alias in place"
        ))
    } else {
        Some(format!(
            "this Mac's own name is {now} ({which}), not {expected}"
        ))
    }
}

fn own_name_repairable(target: &str, names: Option<&GeneratedNames>, own: &OwnNames) -> bool {
    names
        .filter(|names| names.number().to_string() == target)
        .is_some_and(|names| {
            own.each()
                .into_iter()
                .any(|(_, slot, now)| now.is_none_or(|now| !names.accepts(slot, now)))
        })
}

/// `read` where the Mac's own names are still owed `label` and the rule that sets them with no
/// password is in place (`by_rule`): a read that is current otherwise is due, so the pass's
/// apply names the Mac ([`own_name_by_rule`]). Any other read stays as it is.
fn own_name_due(read: IdentityRead, label: Option<&str>, by_rule: bool) -> IdentityRead {
    match (read, label) {
        (IdentityRead::Current(now), Some(label)) if by_rule => IdentityRead::Due {
            from: "this Mac's own names".to_owned(),
            to: format!("{now}; own names {label}, through setup's naming rule"),
        },
        (read, _) => read,
    }
}

/// Name this Mac `label` through the rule setup's elevation left
/// ([`aterm_machines::root_steps::name_by_rule`]), as this account, never asking for a password:
/// what the pass's row says of it, `None` where no rule is in place or nothing changed.
#[cfg(any(target_os = "macos", test))]
fn own_name_by_rule(
    sys: &dyn aterm_machines::port::System,
    names: &GeneratedNames,
    current: &dyn Fn() -> Result<(), String>,
) -> Option<String> {
    if aterm_machines::root_steps::rule_in_place(sys) != Some(true) {
        return None;
    }
    match aterm_machines::root_steps::name_by_rule(sys, names, current) {
        Ok(set) if set.is_empty() => None,
        Ok(set) => Some(format!(
            "this Mac's {} now follow {} / {}",
            set.join(", "),
            names.computer_name(),
            names.host_name()
        )),
        Err(why) => Some(format!("this Mac's own names left as they are: {why}")),
    }
}

/// `read` (`sync.identity`'s `aterm.toml` keys) with the fleet name beside it (`fleet`, against
/// `target`), and, where the Mac's own names (`LocalHostName`, `ComputerName` and `HostName`) are
/// not yet the roster's `label`, the record of what names it ([`own_name_pending`]): no pass asks
/// for a password of its own.
fn with_fleet_name(
    read: IdentityRead,
    target: &str,
    label: Option<&GeneratedNames>,
    fleet: &aterm_agent::FleetName,
    own: &OwnNames,
) -> IdentityRead {
    use aterm_agent::FleetName;
    let now = match fleet {
        FleetName::Current | FleetName::Due { .. } => format!("fleet name {target}"),
        FleetName::Kept(name) => format!("fleet name {name} kept as you set it"),
        FleetName::Taken(other) => format!("fleet name left as it is: {target} names {other}"),
        FleetName::NotApplicable(why) => format!("fleet name left as it is: {why}"),
    };
    let host = own_name_pending(target, label, own)
        .map(|pending| format!("; {pending}"))
        .unwrap_or_default();
    match (read, fleet) {
        (IdentityRead::NotApplicable(why), _) => IdentityRead::NotApplicable(why),
        (IdentityRead::Current(keys), FleetName::Due { from }) => IdentityRead::Due {
            from: format!("fleet name {from}"),
            to: format!("{keys}; {now}{host}"),
        },
        (IdentityRead::Due { from, to }, FleetName::Due { from: was }) => IdentityRead::Due {
            from: format!("{from}, fleet name {was}"),
            to: format!("{to}; {now}{host}"),
        },
        (IdentityRead::Current(keys), _) => IdentityRead::Current(format!("{keys}; {now}{host}")),
        (IdentityRead::Due { from, to }, _) => IdentityRead::Due {
            from,
            to: format!("{to}; {now}{host}"),
        },
    }
}

/// `sync.identity`'s read at `config_path` given `paths` and fallback `hostnames`, with what aterm
/// last wrote at `record`.
#[cfg(test)]
fn identity_at(
    config_path: &Path,
    record: &Path,
    paths: &aterm_secrets::client::Paths,
    hostnames: &[String],
) -> Result<IdentityRead, String> {
    Ok(identity_owed_at(config_path, record, paths, hostnames)?.read)
}

/// [`identity_at`], with what an apply writes.
#[cfg(test)]
fn identity_owed_at(
    config_path: &Path,
    record: &Path,
    paths: &aterm_secrets::client::Paths,
    hostnames: &[String],
) -> Result<IdentityOwed, String> {
    let roster = read_name_roster(paths)?;
    Ok(identity_owed_from(config_path, None, record, paths, hostnames, roster.as_ref())?.0)
}

#[cfg(test)]
fn identity_owed_from(
    config_path: &Path,
    state: Option<&Path>,
    record: &Path,
    paths: &aterm_secrets::client::Paths,
    hostnames: &[String],
    roster: Option<&aterm_secrets::swarm_roster::SwarmRoster>,
) -> Result<(IdentityOwed, Option<IdentityTarget>, Option<String>), String> {
    let off = |why: &str| {
        (
            IdentityOwed {
                read: IdentityRead::NotApplicable(why.to_owned()),
                write: None,
            },
            None,
            None,
        )
    };
    let snapshot =
        aterm_uds::atomic_file::ConfigFile::open(config_path).map_err(|e| e.to_string())?;
    let Some(config) = read_enabled_identity_snapshot(&snapshot)? else {
        return Ok(off("off: [swarm] enabled = false"));
    };
    if config.name == NameChoice::Off {
        return Ok(off("off: [identity] name = false"));
    }
    identity_owed_from_config(config, state, record, paths, hostnames, roster)
}

fn identity_owed_from_config(
    mut config: IdentityConfig,
    state: Option<&Path>,
    record: &Path,
    paths: &aterm_secrets::client::Paths,
    hostnames: &[String],
    roster: Option<&aterm_secrets::swarm_roster::SwarmRoster>,
) -> Result<(IdentityOwed, Option<IdentityTarget>, Option<String>), String> {
    let roster_label = name_label(paths, roster, hostnames)?;
    let label = projected_machine_label(state, &mut config.name, roster_label.clone())?;
    let owed = identity_owed_for_config(&config, record, label.as_deref(), &|pair| {
        roster_gives(roster, roster_label.as_deref(), pair)
    })?;
    let target = identity_target_from(&config.name, label);
    Ok((owed, target, roster_label))
}

/// Compare `config_path`'s `[ai] machine` and `[fleet] machine` with the name this Mac goes by:
/// the person's `[identity] name`, else the resolved `label` (none applies where no label does,
/// since the name is ALab's). A key is owed only where it is unset or still holds what aterm
/// wrote (`record`); a key the person set is kept as they set it. A test's: with no record, an
/// alike pair of roster labels is this Mac's ([`unrecorded_label`]).
#[cfg(test)]
fn identity_owed(
    config_path: &Path,
    record: &Path,
    label: Option<&str>,
) -> Result<IdentityOwed, String> {
    let config =
        aterm_uds::atomic_file::ConfigFile::open(config_path).map_err(|e| e.to_string())?;
    identity_owed_in(&config, record, label, &|_| true)
}

fn format_machine_keys(keys: &[&str]) -> &'static str {
    match keys {
        ["ai"] => "[ai] machine",
        ["fleet"] => "[fleet] machine",
        ["ai", "fleet"] => "[ai] machine and [fleet] machine",
        _ => "",
    }
}

/// Decide which keys are owed from the same config snapshot the write must still match, where
/// `ours` says which roster labels the vault's roster gives this Mac ([`unrecorded_label`]).
/// What aterm wrote is its last write, and while that write is unconfirmed the values before it
/// ([`EARLIER`]): once a write landed ([`confirm_written`]), a key back at an earlier value is
/// the person's choice.
#[cfg(test)]
fn identity_owed_in(
    snapshot: &aterm_uds::atomic_file::ConfigFile,
    record: &Path,
    label: Option<&str>,
    ours: &dyn Fn(&str) -> bool,
) -> Result<IdentityOwed, String> {
    let config = read_identity_snapshot(snapshot)?;
    identity_owed_for_config(&config, record, label, ours)
}

fn identity_owed_for_config(
    config: &IdentityConfig,
    record: &Path,
    label: Option<&str>,
    ours: &dyn Fn(&str) -> bool,
) -> Result<IdentityOwed, String> {
    let Some(label) = label else {
        return Ok(IdentityOwed {
            read: IdentityRead::NotApplicable(
                "no swarm label applies on this Mac (no ALab vault or swarm roster)".to_owned(),
            ),
            write: None,
        });
    };
    let (target, whose) = match &config.name {
        NameChoice::Off => {
            return Ok(IdentityOwed {
                read: IdentityRead::NotApplicable("off: [identity] name = false".to_owned()),
                write: None,
            });
        }
        NameChoice::Yours(name) => (name.clone(), format!("your name {name}")),
        NameChoice::Roster => (label.to_owned(), format!("label {label}")),
    };
    let history = read_history(record)?;
    let unrecorded = history.is_none() && unrecorded_label(&config.machines, ours);
    let written = history.unwrap_or_default();
    let mut owed: Vec<(&'static str, &str)> = Vec::new();
    let mut kept = Vec::new();
    for ((key, now), wrote) in MACHINE_KEYS.into_iter().zip(&config.machines).zip(&written) {
        match now.as_deref() {
            Some(now) if now == target => {}
            None => owed.push((key, "unset")),
            Some(now) if unrecorded || wrote.iter().any(|value| value == now) => {
                owed.push((key, now));
            }
            Some(now) => kept.push(format!("[{key}] machine = {now}")),
        }
    }
    let git = if config.git_off {
        "; [identity] git = false, and no pass writes git's identity"
    } else {
        ""
    };
    let kept = if kept.is_empty() {
        String::new()
    } else {
        format!("; {} kept as you set it", kept.join(", "))
    };
    if owed.is_empty() {
        let now = if kept.is_empty() {
            format!("{whose} in [ai] and [fleet] machine{git}")
        } else {
            format!("{whose}{kept}{git}")
        };
        return Ok(IdentityOwed {
            read: IdentityRead::Current(now),
            write: None,
        });
    }
    let keys: Vec<&'static str> = owed.iter().map(|(key, _)| *key).collect();
    Ok(IdentityOwed {
        read: IdentityRead::Due {
            from: owed
                .iter()
                .map(|(key, now)| format!("[{key}] machine = {now}"))
                .collect::<Vec<_>>()
                .join(", "),
            to: format!("{whose} in {}{kept}{git}", format_machine_keys(&keys)),
        },
        write: Some((target, keys)),
    })
}

/// `sync.identity`'s apply at `config_path`: write the name each owed key takes
/// ([`identity_owed_in`]), and record it at `record` as aterm's own. A test's: with no record,
/// an alike pair of roster labels is this Mac's.
#[cfg(test)]
fn write_identity_at(
    config_path: &Path,
    record: &Path,
    label: Option<&str>,
) -> Result<String, String> {
    write_identity_after_read(config_path, None, record, label, &|_| true, || {})
}

/// [`write_identity_at`] where `ours` says which roster labels the vault's roster gives this Mac
/// ([`unrecorded_label`]), with an action after its decision, so a test can put a competing
/// Settings edit between that read and the write without a timed race.
fn write_identity_after_read(
    config_path: &Path,
    state: Option<&Path>,
    record: &Path,
    label: Option<&str>,
    ours: &dyn Fn(&str) -> bool,
    after_read: impl FnOnce(),
) -> Result<String, String> {
    let config =
        aterm_uds::atomic_file::ConfigFile::open(config_path).map_err(|e| e.to_string())?;
    let mut snapshot = read_identity_snapshot(&config)?;
    let label = projected_machine_label(state, &mut snapshot.name, label.map(str::to_owned))?;
    let owed = identity_owed_for_config(&snapshot, record, label.as_deref(), ours)?;
    let Some((target, keys)) = owed.write else {
        return Ok(match owed.read {
            IdentityRead::NotApplicable(why) => format!("left as it is: {why}"),
            IdentityRead::Current(now) | IdentityRead::Due { to: now, .. } => {
                format!("left as it is: {now}")
            }
        });
    };
    after_read();
    // The record first: a pass cut off between the two writes leaves a key aterm's record
    // names, never aterm's own write read as the person's. A write that does not land puts the
    // record back.
    check_record_dirs(record, false)?;
    let before =
        aterm_uds::atomic_file::read_regular(record, aterm_uds::atomic_file::MAX_CONFIG_BYTES)
            .ok()
            .map(|(b, _)| b);
    record_written(record, &keys, &target, &snapshot.machines).map_err(|why| {
        format!("could not record what aterm writes, so aterm.toml is left as it was: {why}")
    })?;
    if let Err(why) = write_machine_label(&config, &target, &keys) {
        return Err(match restore_record(record, before.as_deref()) {
            Ok(()) => why,
            Err(unrestored) => format!("{why}; and the record could not be put back: {unrestored}"),
        });
    }
    // The write landed: what the keys held before it is no longer aterm's to put right, so a
    // person who sets one back keeps it. A record left unconfirmed only keeps reading those
    // values as aterm's, as a write cut off does.
    let _ = confirm_written(record, &keys);
    Ok(format!(
        "wrote {target} into {}",
        format_machine_keys(&keys)
    ))
}

/// Write `machine = label` into each of `sections` (`ai`, `fleet`) of the snapshot, preserving
/// unrelated tables and comments. The config service's locked replacement compares the
/// original bytes instead of treating a later edit as a fresh baseline.
fn write_machine_label(
    config: &aterm_uds::atomic_file::ConfigFile,
    label: &str,
    sections: &[&str],
) -> Result<(), String> {
    use aterm_toml::edit::{DocumentMut, Item, Table, Value};
    use aterm_uds::atomic_file::EditLanded;

    let text = config.text().map_err(|e| e.to_string())?.unwrap_or("");
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e| format!("aterm.toml is not valid TOML, so it is left as it was: {e}"))?;
    for section in sections {
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
    for section in sections {
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
    Ok(())
}

/// Whether `dir`, the control socket folder, holds an aterm instance's socket
/// (`aterm-<pid>.sock`) whose process `alive` says lives, this process's own left out: every
/// instance in its own login runs the accounts' upkeep.
fn window_in(dir: &Path, alive: &dyn Fn(u32) -> bool) -> bool {
    let own = std::process::id();
    std::fs::read_dir(dir).is_ok_and(|entries| {
        entries.flatten().any(|entry| {
            entry.file_name().to_str().is_some_and(|name| {
                name.ends_with(".sock")
                    && aterm_types::control_socket::instance_pid(name)
                        .is_some_and(|pid| pid != own && alive(pid))
            })
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
/// responsible process (the one place the converge pass disclaims responsibility, so macOS
/// attributes what it does to aterm.app, not to the held copy that started it), and out of
/// the background band the held copy starts its other children in ([`Ports::background`]), as
/// the pass it runs takes the fleet book's lock.
fn hand_off_command(program: &Path, args: &[String]) -> aterm_uds::spawnfd::Command {
    let mut command = aterm_uds::spawnfd::command(program);
    command
        .args(args)
        .disclaim_responsibility(true)
        .leave_background_band(true);
    command
}

/// A port of `sync.node` (C20), [`aterm_agent::node_service::read_for_sync_with`] or
/// [`aterm_agent::node_service::ensure_for_sync_with`]: over a home and a state root,
/// `aterm.toml`, the installed release `aterm` the job runs, whether `CI` is set, and the
/// supervisor that reaches the job.
type NodePort<T> = fn(
    &Path,
    &Path,
    Option<&Path>,
    Option<&Path>,
    bool,
    &dyn aterm_agent::node_service::ServiceSupervisor,
) -> Result<T, String>;

impl Live {
    /// `port` over this account's home and state, `aterm.toml` and the installed release
    /// `aterm`, reaching the job through [`aterm_agent::node_service::supervisor_here`], as
    /// `aterm node` and `fleet leave` do. Where this process is not a shell of the account's own
    /// ([`aterm_types::dirs::not_own_account`]: a private state root, root, or another `HOME`),
    /// `left` answers with that reason instead, before anything is read: such a process asks no
    /// `launchctl` or `systemctl` about the job and writes and removes no unit file under the
    /// home it was given, which may be the account's.
    ///
    /// # Errors
    /// No home directory or no state directory, or `port`'s own.
    fn through_node_job<T>(&self, port: NodePort<T>, left: fn(String) -> T) -> Result<T, String> {
        if let Some(why) = aterm_types::dirs::not_own_account() {
            return Ok(left(why.to_owned()));
        }
        let home = aterm_types::dirs::home_dir().ok_or("no user home directory")?;
        let state = self.state_dir().ok_or("no state directory")?;
        let current_exe = std::env::current_exe().and_then(|p| p.canonicalize()).ok();
        let program = aterm_agent::node_service::resolve_installed_program(
            current_exe.as_deref(),
            self.installed_release(),
            &home,
        );
        port(
            &home,
            &state,
            aterm_types::dirs::aterm_config_path().as_deref(),
            program.as_deref(),
            under_ci(),
            aterm_agent::node_service::supervisor_here(),
        )
    }
}

impl Ports for Live {
    /// Every thread of the pass held as it starts where its starter ran the pass, so the fleet
    /// book's lock lifts its holder to the default class while the rest of the pass yields: the
    /// vault subscriber's, its runner having left the subscriber's band
    /// ([`aterm_secrets::converge::runner`]), at the background class with every child it starts
    /// in the background band, matching the subscriber's ordinary-child policy
    /// ([`aterm_gui::hold_process_as_housekeeping`],
    /// [`aterm_uds::spawnfd::start_children_in_background_band`]); a window's, the first
    /// launch's and a person's at utility ([`aterm_gui::hold_process_as_background_work`]).
    fn background(&self, trigger: aterm_machines::sync::Trigger) {
        if trigger == aterm_machines::sync::Trigger::Subscriber {
            aterm_gui::hold_process_as_housekeeping();
            aterm_uds::spawnfd::start_children_in_background_band();
        } else {
            aterm_gui::hold_process_as_background_work();
        }
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
        aterm_stamp::build_id()
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

    fn refresh_vault_subscriber(&self) -> Result<Option<String>, String> {
        use aterm_secrets::client::HeldMend;
        let paths = vault_paths()?;
        if !vault_config(&paths)?
            .as_ref()
            .is_some_and(aterm_secrets::client::alab_vault)
            || aterm_secrets::service::subscriber_turned_off(&paths.home)
        {
            return Ok(None);
        }
        let mut result = Ok(None);
        let mut refresh = || {
            result = aterm_secrets::client::mend_subscriber(&paths)
                .map(|mended| match mended {
                    HeldMend::Moved(line, _) => Some(line),
                    _ => None,
                })
                .map_err(|why| why.to_string());
        };
        #[cfg(target_os = "macos")]
        crate::while_vault_sync_kept(&mut refresh);
        #[cfg(not(target_os = "macos"))]
        refresh();
        result
    }

    fn window_runs(&self) -> bool {
        aterm_uds::control_socket_dir()
            .is_some_and(|dir| window_in(&dir, &aterm_uds::process::pid_alive))
    }

    fn vault_held(&self) -> Option<String> {
        vault_held()
    }

    fn swarm_enabled(&self) -> Result<bool, String> {
        aterm_secrets::swarm::enabled_live().map_err(|why| why.to_string())
    }

    fn base(&self, lock: BaseLock) -> Base {
        crate::doctor::sync_base(lock)
    }

    fn repository_prerequisites(&self) -> Option<Box<dyn aterm_converge::Item + '_>> {
        Some(Box::new(repository_tools::Prerequisites::live()))
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
        let config_path = aterm_types::dirs::aterm_config_path()
            .ok_or("no config path: neither $XDG_CONFIG_HOME nor $HOME is set")?;
        let snapshot =
            aterm_uds::atomic_file::ConfigFile::open(&config_path).map_err(|e| e.to_string())?;
        let Some(config) = read_enabled_identity_snapshot(&snapshot)? else {
            return Ok(IdentityRead::NotApplicable(
                "off: [swarm] enabled = false".to_owned(),
            ));
        };
        if config.name == NameChoice::Off {
            return Ok(IdentityRead::NotApplicable(
                "off: [identity] name = false".to_owned(),
            ));
        }
        let paths = vault_paths()?;
        let roster = read_name_roster(&paths)?;
        let mut names = None;
        let hostnames = if roster.is_some() {
            Vec::new()
        } else {
            local_hostnames(names.insert(own_names()))
        };
        let (owed, target, roster_label) = identity_owed_from_config(
            config,
            self.state_dir().as_deref(),
            &identity_record()?,
            &paths,
            &hostnames,
            roster.as_ref(),
        )?;
        let read = owed.read;
        let Some((target, label)) = target else {
            return Ok(read);
        };
        let fleet = aterm_agent::fleet_name_for(&target, &|word| {
            roster_gives(roster.as_ref(), roster_label.as_deref(), word)
        })?;
        // Only a Mac's own names are aterm's to align (the root half is macOS's): elsewhere
        // the record names none.
        let generated = generated_os_names(self.state_dir().as_deref(), &target, label.as_deref())?;
        let own = generated.as_ref().filter(|_| cfg!(target_os = "macos"));
        let names = if own.is_some() {
            names.get_or_insert_with(own_names)
        } else {
            names.get_or_insert_with(OwnNames::default)
        };
        let read = with_fleet_name(read, &target, own, &fleet, names);
        // An admission after setup's one password: the rule it left names the Mac, so the
        // pass is due while the Mac's own names are not yet the label.
        let pending = own_name_repairable(&target, own, names)
            .then_some(own)
            .flatten()
            .map(GeneratedNames::computer_name);
        #[cfg(target_os = "macos")]
        let by_rule = pending.is_some()
            && aterm_machines::root_steps::rule_in_place(&aterm_machines::port::Live::new())
                == Some(true);
        #[cfg(not(target_os = "macos"))]
        let by_rule = false;
        Ok(own_name_due(read, pending, by_rule))
    }

    fn init_name(
        &self,
        current: &dyn Fn() -> Result<(), String>,
    ) -> Result<Option<String>, String> {
        #[cfg(target_os = "macos")]
        {
            use aterm_machines::port::System as _;
            let sys = aterm_machines::port::Live::new();
            if sys.euid() == 0 || aterm_machines::console::remote(&sys) {
                return Err("macOS naming waits for this Mac's console account".into());
            }
            let state = self
                .state_dir()
                .ok_or("no current account state directory")?;
            let config =
                aterm_types::dirs::aterm_config_path().ok_or("no current account configuration")?;
            synchronize_saved_name_with(&sys, &state, &config, current)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = current;
            Ok(None)
        }
    }

    fn sync_identity(&self) -> Result<String, String> {
        let config = aterm_types::dirs::aterm_config_path()
            .ok_or("no config path: neither $XDG_CONFIG_HOME nor $HOME is set")?;
        let snapshot =
            aterm_uds::atomic_file::ConfigFile::open(&config).map_err(|e| e.to_string())?;
        let Some(mut identity) = read_enabled_identity_snapshot(&snapshot)? else {
            return Ok("left as it is: [swarm] enabled = false".to_owned());
        };
        if identity.name == NameChoice::Off {
            return Ok("left as it is: [identity] name = false".to_owned());
        }
        let paths = vault_paths()?;
        let roster = read_name_roster(&paths)?;
        let mut names = None;
        let hostnames = if roster.is_some() {
            Vec::new()
        } else {
            local_hostnames(names.insert(own_names()))
        };
        let roster_label = name_label(&paths, roster.as_ref(), &hostnames)?;
        let label = projected_machine_label(
            self.state_dir().as_deref(),
            &mut identity.name,
            roster_label.clone(),
        )?;
        let keys = write_identity_after_read(
            &config,
            self.state_dir().as_deref(),
            &identity_record()?,
            label.as_deref(),
            &|pair| roster_gives(roster.as_ref(), roster_label.as_deref(), pair),
            || {},
        )?;
        // The fleet and OS projections follow the chosen machine number, or the roster while
        // no explicit number is held. The roster still identifies older writes as ours.
        let state = self.state_dir();
        let Some(projection) =
            projection_target_now(&config, state.as_deref(), roster_label.clone())?
        else {
            return Ok(keys);
        };
        let current = || projection.check_current(&config, state.as_deref(), roster_label.clone());
        let target = &projection.target.0;
        #[cfg(not(target_os = "macos"))]
        let _ = &mut names;
        let fleet = aterm_agent::follow_fleet_name(
            target,
            &|word| roster_gives(roster.as_ref(), roster_label.as_deref(), word),
            &current,
        )?;
        current()?;
        #[cfg(target_os = "macos")]
        if let Some(generated) = projection.names.as_ref().filter(|generated| {
            own_name_repairable(target, Some(generated), names.get_or_insert_with(own_names))
        }) && let Some(named) =
            own_name_by_rule(&aterm_machines::port::Live::new(), generated, &current)
        {
            return Ok(format!("{keys}; {fleet}; {named}"));
        }
        Ok(format!("{keys}; {fleet}"))
    }

    fn fleet(&self) -> Result<FleetRead, String> {
        use aterm_agent::{FleetCheck, FleetSync};
        Ok(match aterm_agent::check_from_cache()? {
            FleetCheck::Idle(sync) => FleetRead::Idle(sync.report()),
            FleetCheck::Current => FleetRead::Current(FleetSync::Current.report()),
            FleetCheck::Due(why) => FleetRead::Due(why),
            FleetCheck::Blocked(why) => FleetRead::Blocked(why),
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
        // The node service's mesh gate also covers local fabric hosting without a fleet key;
        // read and apply share that policy, including CI and the person's explicit node off.
        use aterm_agent::node_service::{SyncNodeState, read_for_sync_with};
        Ok(
            match self.through_node_job(read_for_sync_with, SyncNodeState::NotApplicable)? {
                SyncNodeState::NotApplicable(why) => NodeRead::NotApplicable(why),
                SyncNodeState::Current(now) => NodeRead::Current(now),
                SyncNodeState::Due { from, to } => NodeRead::Due { from, to },
            },
        )
    }

    fn ensure_node(&self) -> Result<String, String> {
        self.through_node_job(aterm_agent::node_service::ensure_for_sync_with, |why| {
            format!("left as it is: {why}")
        })
    }

    fn steward_revoke(&self) -> Result<StewardRevokeRead, String> {
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
                        match publish_waits(
                            &config,
                            "the steward's revoke cascade",
                            aterm_machines::lan_reach::broker_unready,
                        ) {
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

    fn steward_lease(&self) -> Result<StewardLeaseRead, String> {
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
                        match publish_waits(
                            &config,
                            "the steward's lease",
                            aterm_machines::lan_reach::broker_unready,
                        ) {
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

    fn shared_data(&self) -> Result<SharedRead, String> {
        let config = aterm_types::dirs::aterm_config_path()
            .ok_or("no config path: neither $XDG_CONFIG_HOME nor $HOME is set")?;
        if !aterm_db::runtime::enabled(&config).map_err(|why| why.to_string())? {
            return Ok(SharedRead::Off);
        }
        let paths = vault_paths()?;
        // A sync that holds the vault's state lock for a moment (the subscriber applying the
        // version a founding just published) is waited out, so a founding read back is read
        // settled; one held past the wait reads busy, which fails no founding.
        shared_read(
            aterm_secrets::commons::observe_until_settled(
                &paths,
                std::time::Instant::now() + aterm_secrets::commons::SETTLE,
            )
            .map_err(|why| why.to_string())?,
            || vault_config(&paths),
            || aterm_secrets::client::commons_foundable(&paths).map_err(|why| why.to_string()),
            |vault, what| publish_waits(vault, what, aterm_machines::lan_reach::broker_unready),
        )
    }

    fn requests(&self) -> Result<RequestsRead, String> {
        let config = aterm_types::dirs::aterm_config_path()
            .ok_or("no config path: neither $XDG_CONFIG_HOME nor $HOME is set")?;
        let paths = vault_paths()?;
        requests_read(
            aterm_secrets::need::peek(&paths).map_err(|why| why.to_string()),
            || aterm_db::runtime::enabled(&config).map_err(|why| why.to_string()),
            || aterm_secrets::commons::bootstrap::writer_here(&paths),
        )
    }

    fn pass_requests(&self) -> Result<String, String> {
        let paths = vault_paths()?;
        requests_pass(
            || aterm_db::credentials::pass(&paths).map_err(|why| why.to_string()),
            || aterm_db::credentials::custodian_pass(&paths).map_err(|why| why.to_string()),
        )
    }

    fn found_commons(&self) -> Result<SharedDone, String> {
        found_said(aterm_secrets::client::admin_commons_found_by_default(
            &vault_paths()?,
        ))
    }

    fn release(&self) -> Result<ReleaseRead, String> {
        use aterm_secrets::client::release::{SigningStatus, signing_status};
        let paths = aterm_secrets::client::Paths::live().map_err(|error| error.to_string())?;
        Ok(
            match signing_status(&paths).map_err(|error| error.to_string())? {
                SigningStatus::NotApplicable(why) => ReleaseRead::NotApplicable(why),
                SigningStatus::Pending(why) => ReleaseRead::Due(why),
                SigningStatus::Ready(now) => ReleaseRead::Current(now),
            },
        )
    }

    fn sync_release(&self) -> Result<String, String> {
        let paths = aterm_secrets::client::Paths::live().map_err(|error| error.to_string())?;
        aterm_secrets::client::release::sync_signing(&paths).map_err(|error| error.to_string())
    }

    fn github(&self) -> Result<GithubRead, String> {
        aterm_machines::init_github::vault_git_credentials(&aterm_machines::port::Live::new())
    }

    fn sync_github(&self, journaled: &str) -> Result<String, String> {
        aterm_machines::init_github::sync_vault_git_credentials(
            &aterm_machines::port::Live::new(),
            journaled,
        )
    }

    fn owed_clone(
        &self,
        readback: Option<&aterm_machines::init_github::RepositoryReadback>,
        work: aterm_secrets::converge::RepositoryWork,
    ) -> Result<aterm_machines::init::OwedClone, String> {
        let state = self
            .state_dir()
            .ok_or("no state directory holds the clone aterm init owes")?;
        let sys = aterm_machines::port::Live::new();
        let ctx = repository_context(&sys)?;
        let paths = vault_paths()?;
        if !setup_holds().holds(aterm_types::macos_setup::Setting::Repositories)
            && aterm_machines::init_github::RepositoryCredentialWithdrawal::capture(&sys, &ctx)?
                .is_some()
        {
            return Ok(aterm_machines::init::OwedClone::Due(
                "the default repository credential request is no longer needed".into(),
            ));
        }
        match repository_vault(&paths, &sys, &ctx)? {
            Some(config) => {
                let Some(captured) = repository_inputs(&paths, &config, &sys, &ctx)? else {
                    return Ok(aterm_machines::init::OwedClone::Waiting(
                        REPOSITORIES_SYNCING.into(),
                    ));
                };
                aterm_machines::init::owed_clone_captured(
                    &sys,
                    &state,
                    &setup_holds(),
                    captured.inputs,
                    || {
                        captured
                            .snapshot
                            .is_current_now(&paths)
                            .map_err(|why| why.to_string())
                    },
                    readback,
                    work,
                )
            }
            None => aterm_machines::init::owed_clone_in_pass(
                &sys,
                &state,
                &setup_holds(),
                readback,
                work,
            ),
        }
    }

    fn owed_clone_control(&self) -> Result<Option<String>, String> {
        let state = self
            .state_dir()
            .ok_or("no state directory holds the clone aterm init owes")?;
        aterm_machines::init::owed_clone_control(
            &aterm_machines::port::Live::new(),
            &state,
            &setup_holds(),
        )
    }

    fn clone_owed(
        &self,
        work: aterm_secrets::converge::RepositoryWork,
        execution: Option<aterm_machines::sync::repository_refresh::Execution>,
    ) -> Result<aterm_machines::init::CloneOwedOutcome, String> {
        let state = self
            .state_dir()
            .ok_or("no state directory holds the clone aterm init owes")?;
        let sys = aterm_machines::port::Live::new();
        let ctx = repository_context(&sys)?;
        let paths = vault_paths()?;
        let intent = aterm_machines::init::OwedCloneIntent::capture(&sys, &state, &setup_holds())?;
        let current_state = state.clone();
        if let Some(withdrawal) =
            aterm_machines::init_github::RepositoryCredentialWithdrawal::capture(&sys, &ctx)?
        {
            return aterm_machines::init::withdraw_repository_credential(
                &sys,
                &state,
                withdrawal,
                move || {
                    let holds = setup_holds();
                    Ok(
                        !holds.holds(aterm_types::macos_setup::Setting::Repositories)
                            && intent.current(
                                &aterm_machines::port::Live::new(),
                                &current_state,
                                &holds,
                            )?,
                    )
                },
            )
            .map(|said| aterm_machines::init::CloneOwedOutcome {
                said,
                readback: None,
            });
        }
        let Some(config) = repository_vault(&paths, &sys, &ctx)? else {
            let inputs =
                aterm_machines::init_github::InitialRepositoryInputs::capture_ready(&sys, &ctx)?
                    .with_repository_execution(execution);
            let current_inputs = inputs.clone();
            return aterm_machines::init::clone_owed_captured(
                &sys,
                &state,
                work,
                inputs,
                move || {
                    intent.current_inputs(
                        &aterm_machines::port::Live::new(),
                        &current_state,
                        &setup_holds(),
                        &current_inputs,
                    )
                },
            );
        };
        let captured =
            repository_inputs(&paths, &config, &sys, &ctx)?.ok_or(REPOSITORIES_SYNCING)?;
        let current_inputs = captured.inputs.clone();
        aterm_machines::init::clone_owed_captured(
            &sys,
            &state,
            work,
            captured.inputs.with_repository_execution(execution),
            move || {
                // A first clone may take a long time. The person's Stop, repository switch or
                // profile change must hold every later effect, not just the next sync pass.
                if !intent.current_inputs(
                    &aterm_machines::port::Live::new(),
                    &current_state,
                    &setup_holds(),
                    &current_inputs,
                )? || !aterm_machines::init_github::uses_vault_inventory(
                    &aterm_machines::port::Live::new(),
                    &ctx,
                )? {
                    return Ok(false);
                }
                captured
                    .snapshot
                    .is_current_now(&paths)
                    .map_err(|why| why.to_string())
            },
        )
    }

    fn forget_owed_clone(&self) -> Result<(), String> {
        let state = self
            .state_dir()
            .ok_or("no state directory holds the clone aterm init owes")?;
        aterm_machines::init::forget_owed_clone(&state)
    }

    fn clone_tuning(&self) -> Result<aterm_machines::clone_tuning::Reading, String> {
        let state = self
            .state_dir()
            .ok_or("no state directory holds the clones' tuning record")?;
        let sys = aterm_machines::port::Live::new();
        let ctx = repository_context(&sys)?;
        aterm_machines::clone_tuning::read(&sys, &ctx, &state, &setup_holds())
    }

    fn tune_clones(&self) -> Result<aterm_machines::clone_tuning::Tuned, String> {
        let state = self
            .state_dir()
            .ok_or("no state directory holds the clones' tuning record")?;
        let sys = aterm_machines::port::Live::new();
        let ctx = repository_context(&sys)?;
        aterm_machines::clone_tuning::apply(&sys, &ctx, &state, &setup_holds())
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

    fn fabric_needs_node(&self) -> bool {
        self.fabric_service == aterm_link::enable::Service::Node
    }

    fn fabric(&self) -> Result<FabricRead, String> {
        use aterm_link::enable::{BrokerRenewal, Renew};
        Ok(
            match aterm_link::enable::renew_replaced_broker_with(Renew::Look, self.fabric_service)?
            {
                BrokerRenewal::Off => fabric_unset(
                    aterm_link::enable::unset_state()?,
                    under_ci(),
                    aterm_link::enable::names_aterm_on_path(),
                    std::env::current_exe()
                        .and_then(|exe| exe.canonicalize())
                        .ok()
                        .as_deref(),
                    declared_command().as_deref(),
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
            match aterm_link::enable::renew_replaced_broker_with(Renew::Apply, self.fabric_service)?
            {
                BrokerRenewal::Renewed(said) => format!("renewed the broker on this build: {said}"),
                BrokerRenewal::Off => format!("nothing renewed: {FABRIC_OFF}"),
                BrokerRenewal::Current(now) | BrokerRenewal::Due(now) => {
                    format!("nothing renewed: {now}")
                }
                BrokerRenewal::Kept(why) => format!("left as it is: {why}"),
            },
        )
    }

    fn awake(&self) -> Result<AwakeRead, String> {
        awake_of(
            aterm_acaffeine::cli::installed_app,
            aterm_acaffeine::held::held(),
        )
    }

    fn awake_startup_off(&self) -> bool {
        aterm_acaffeine::cli::installed_startup_turned_off()
    }

    fn awake_retired_jobs(&self) -> Vec<String> {
        aterm_acaffeine::cli::retired_old_jobs()
    }

    fn retire_awake_old_jobs(&self) -> Result<String, String> {
        aterm_acaffeine::cli::retire_old_jobs()
    }

    fn keep_awake(&self) -> Result<String, String> {
        use aterm_acaffeine::cli::Kept;
        use aterm_acaffeine::held::Held;
        let kept = aterm_acaffeine::cli::keep_installed_awake();
        match kept {
            Kept::NotHere(why) | Kept::Failed(why) => return Err(why),
            Kept::Off => return Ok(kept.line()),
            Kept::Running(_) => {}
        }
        let until = std::time::Instant::now() + AWAKE_SETTLE;
        while !matches!(
            aterm_acaffeine::held::held(),
            Held::Held { .. } | Held::Paused { .. }
        ) && std::time::Instant::now() < until
        {
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        Ok(kept.line())
    }

    fn turn_on_fabric(&self) -> Result<String, String> {
        use aterm_link::enable::ByDefault;
        if under_ci() {
            return Ok("nothing turned on: under CI no pass turns the fabric on".to_owned());
        }
        Ok(
            match aterm_link::enable::turn_on_by_default_with(self.fabric_service)? {
                ByDefault::TurnedOn(said) => format!("turned the fabric on: {said}"),
                ByDefault::On => "nothing turned on: the fabric is on".to_owned(),
                ByDefault::Left(why) => format!("left as it is: {why}"),
            },
        )
    }

    fn peers(&self, can_join: bool) -> Result<PeersRead, String> {
        let ops = aterm_machines::init_secrets::LiveSecrets::unattended(&vault_paths()?.home);
        aterm_machines::sync::peers::read(&ops, &peers_gate(), can_join)
    }

    fn join_peers(&self) -> Result<PeersDone, String> {
        #[cfg(target_os = "macos")]
        let authorization = match aterm_cli::macos_doctor::BackgroundVaultAuthorization::capture() {
            Ok(authorization) => authorization,
            Err(why) => {
                return Ok(PeersDone::Deferred(aterm_converge::StoodAside::Waiting(
                    why,
                )));
            }
        };
        let current = || {
            #[cfg(target_os = "macos")]
            authorization.check()?;
            let gate = peers_gate();
            if let Some(why) = gate.off {
                return Err(why);
            }
            if gate.owed {
                return Err(aterm_machines::sync::peers::STAGE_OWED.to_owned());
            }
            gate.broker.map_or(Ok(()), Err)
        };
        if let Err(why) = current() {
            return Ok(PeersDone::Deferred(aterm_converge::StoodAside::Waiting(
                why,
            )));
        }
        let sys = aterm_machines::port::Live::new();
        let ctx = aterm_machines::init::initial_setup_context(&sys)?;
        let ops = aterm_machines::init_secrets::unattended(&ctx)?;
        let mut journal = aterm_machines::port::System::journal(&sys)?;
        aterm_machines::sync::peers::join(&sys, &ctx, ops, journal.as_mut(), &current)
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

/// `sync.requests`'s read: where this machine's `needs` stand, and whether the pass is due
/// (Commons on, this machine a Writer), each named in its platform's word
/// ([`aterm_types::machine_name::OS_NOUN`]). A ledger it cannot read is said, not the read's
/// failure: the pass still runs, so the custodian's half decides for the lab and the member's half
/// fails the item naming the ledger.
fn requests_read(
    needs: Result<Vec<aterm_secrets::need::Need>, String>,
    enabled: impl FnOnce() -> Result<bool, String>,
    writer: impl FnOnce() -> bool,
) -> Result<RequestsRead, String> {
    use aterm_types::machine_name::OS_NOUN;
    let standing = match needs {
        Ok(needs) => aterm_secrets::need::status_line(&needs)
            .unwrap_or_else(|| format!("no needs on this {OS_NOUN}")),
        Err(why) => format!("this {OS_NOUN}'s need ledger cannot be read ({why})"),
    };
    if !enabled()? {
        return Ok(RequestsRead::Idle(format!(
            "off: [commons] enabled = false ({standing})"
        )));
    }
    if !writer() {
        return Ok(RequestsRead::Idle(format!(
            "this {OS_NOUN} is not a Commons Writer, so the lab's table is not reached ({standing})"
        )));
    }
    Ok(RequestsRead::Due(standing))
}

/// `sync.requests`'s apply: this Mac's `member` pass over the lab's table, then the
/// `custodian`'s, which runs whatever the first did: it reads the table and the vault, not this
/// Mac's ledger, so a member pass that fails on the vault's admin Mac does not hold up the lab's
/// decisions. The line says both; a failure in either, or a request whose copy or publication
/// failed, fails the item with it.
fn requests_pass(
    member: impl FnOnce() -> Result<aterm_db::credentials::Pass, String>,
    custodian: impl FnOnce() -> Result<Option<aterm_db::credentials::Custodian>, String>,
) -> Result<String, String> {
    use std::fmt::Write as _;
    let member = member();
    let custodian = custodian();
    let mut failed = member.is_err();
    let mut said = match member {
        Ok(pass) => format!(
            "{} posted, {} answered, {} decided here, {} offered, {} swept",
            pass.posted, pass.answered, pass.decided, pass.offered, pass.swept
        ),
        Err(why) => format!(
            "this {}'s pass failed: {why}",
            aterm_types::machine_name::OS_NOUN
        ),
    };
    match custodian {
        Ok(None) => {}
        Ok(Some(custodian)) => {
            failed |= !custodian.failed.is_empty();
            let _ = write!(said, "; {}", custodian.summary());
        }
        Err(why) => {
            failed = true;
            let _ = write!(said, "; the custodian's pass failed: {why}");
        }
    }
    if failed { Err(said) } else { Ok(said) }
}

#[cfg(test)]
#[path = "converge_tests.rs"]
mod tests;
