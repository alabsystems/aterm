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

/// The name `sync.identity` gives this Mac at `config_path` (its chosen machine number, else
/// `[identity] name`, else the vault roster's label) and its projected number, or `None`
/// where no name applies (swarm off, `[identity] name = false`, no label).
#[cfg(target_os = "macos")]
fn identity_target(
    config_path: &Path,
    state: Option<&Path>,
    paths: &aterm_secrets::client::Paths,
    hostnames: &[String],
    roster: Option<&aterm_secrets::swarm_roster::SwarmRoster>,
) -> Result<Option<IdentityTarget>, String> {
    let snapshot =
        aterm_uds::atomic_file::ConfigFile::open(config_path).map_err(|e| e.to_string())?;
    let Some(mut config) = read_enabled_identity_snapshot(&snapshot)? else {
        return Ok(None);
    };
    if config.name == NameChoice::Off {
        return Ok(None);
    }
    let label = projected_machine_label(
        state,
        &mut config.name,
        name_label(paths, roster, hostnames)?,
    )?;
    Ok(identity_target_from(&config.name, label))
}

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
/// person's newer policy that happens to resolve to the same name. Nicknames and unrelated
/// configuration are absent, so their edits leave this work admitted.
#[derive(Debug, PartialEq, Eq)]
struct IdentityProjection {
    target: IdentityTarget,
    choice: NameChoice,
    machines: [Option<String>; 2],
    chosen: Option<aterm_types::machine_name::Number>,
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
    Ok(
        identity_target_from(&identity.name, label).map(|target| IdentityProjection {
            target,
            choice,
            machines: identity.machines,
            chosen,
        }),
    )
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

/// The number the root half of the one password names the Mac by
/// ([`aterm_machines::init_clt::CommandLineTools::with_name`]): its chosen machine number, else
/// the roster's label (no hostname fallback), unless identity naming is off or names this Mac
/// separately. `None` while neither source names a number.
#[cfg(target_os = "macos")]
pub(crate) fn mac_name_label() -> Option<String> {
    let config = aterm_types::dirs::aterm_config_path()?;
    let paths = vault_paths().ok()?;
    let roster = read_name_roster(&paths).ok()?;
    match identity_target(
        &config,
        aterm_types::dirs::account_state_dir().as_deref(),
        &paths,
        &[],
        roster.as_ref(),
    ) {
        Ok(Some((target, Some(label)))) if target == label => Some(label),
        _ => None,
    }
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
    let pending = label
        .as_deref()
        .filter(|label| cfg!(target_os = "macos") && *label == target)
        .and_then(|label| own_name_pending(&target, Some(label), own.get_or_insert_with(own_names)))
        .map(|why| {
            MacNamePending::with_rule(
                why,
                aterm_machines::root_steps::rule_in_place(&aterm_machines::port::Live::new()),
            )
        });
    Ok(MacName::Named {
        target,
        fleet,
        pending,
    })
}

/// Where any of the Mac's own names (`ComputerName`, `LocalHostName` or `HostName`) does not
/// accept the projected `label` and `target` is that number: what names it. macOS lets only root
/// set them: setup's one early Mac password
/// sets them where a number was known by then (its argv carries it), and otherwise leaves the
/// narrow rule through which `sync.identity` sets them with no password once admission gives the
/// number ([`aterm_machines::root_steps::name_by_rule`], [`own_name_by_rule`]). `None` where all
/// match, or a separate `[identity] name` differs from the projected number.
pub(crate) fn own_name_pending(
    target: &str,
    label: Option<&str>,
    own: &OwnNames,
) -> Option<String> {
    let label = label.filter(|label| *label == target)?;
    let (which, _, now) = own
        .each()
        .into_iter()
        .find(|(_, slot, name)| name.is_none_or(|name| !slot.accepts_projection(label, name)))?;
    let now = now.unwrap_or("unread");
    Some(format!(
        "this Mac's own name is {now} ({which}), not {label}"
    ))
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
    label: &str,
    current: &dyn Fn() -> Result<(), String>,
) -> Option<String> {
    if aterm_machines::root_steps::rule_in_place(sys) != Some(true) {
        return None;
    }
    match aterm_machines::root_steps::name_by_rule(sys, label, current) {
        Ok(set) if set.is_empty() => None,
        Ok(set) => Some(format!("this Mac's {} now {label}", set.join(", "))),
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
    label: Option<&str>,
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
    /// in the background band, where its `Background` launchd job ran them
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
        let own = label.as_deref().filter(|_| cfg!(target_os = "macos"));
        let names = if own == Some(target.as_str()) {
            names.get_or_insert_with(own_names)
        } else {
            names.get_or_insert_with(OwnNames::default)
        };
        let read = with_fleet_name(read, &target, own, &fleet, names);
        // An admission after setup's one password: the rule it left names the Mac, so the
        // pass is due while the Mac's own names are not yet the label.
        let pending = own_name_pending(&target, own, names).and(own);
        #[cfg(target_os = "macos")]
        let by_rule = pending.is_some()
            && aterm_machines::root_steps::rule_in_place(&aterm_machines::port::Live::new())
                == Some(true);
        #[cfg(not(target_os = "macos"))]
        let by_rule = false;
        Ok(own_name_due(read, pending, by_rule))
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
        let (target, os_label) = &projection.target;
        #[cfg(not(target_os = "macos"))]
        let _ = (&mut names, os_label);
        let fleet = aterm_agent::follow_fleet_name(
            target,
            &|word| roster_gives(roster.as_ref(), roster_label.as_deref(), word),
            &current,
        )?;
        current()?;
        #[cfg(target_os = "macos")]
        if let Some(label) = os_label
            .as_deref()
            .filter(|label| *label == target.as_str())
            .and_then(|label| {
                own_name_pending(target, Some(label), names.get_or_insert_with(own_names))
                    .and(Some(label))
            })
            && let Some(named) =
                own_name_by_rule(&aterm_machines::port::Live::new(), label, &current)
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

    fn owed_clone(&self) -> Result<aterm_machines::init::OwedClone, String> {
        let state = self
            .state_dir()
            .ok_or("no state directory holds the clone aterm init owes")?;
        let sys = aterm_machines::port::Live::new();
        let ctx = repository_context(&sys)?;
        let paths = vault_paths()?;
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
                )
            }
            None => aterm_machines::init::owed_clone(&sys, &state, &setup_holds()),
        }
    }

    fn clone_owed(
        &self,
        work: aterm_secrets::converge::RepositoryWork,
        execution: Option<aterm_machines::sync::repository_refresh::Execution>,
    ) -> Result<String, String> {
        let state = self
            .state_dir()
            .ok_or("no state directory holds the clone aterm init owes")?;
        let sys = aterm_machines::port::Live::new();
        let ctx = repository_context(&sys)?;
        let paths = vault_paths()?;
        let Some(config) = repository_vault(&paths, &sys, &ctx)? else {
            return aterm_machines::init::clone_owed_in_pass(&sys, &state, work, execution);
        };
        let captured =
            repository_inputs(&paths, &config, &sys, &ctx)?.ok_or(REPOSITORIES_SYNCING)?;
        aterm_machines::init::clone_owed_captured(
            &sys,
            &state,
            work,
            captured.inputs.with_repository_execution(execution),
            move || {
                // A first clone may take a long time. The person's Stop, repository switch or
                // profile change must hold every later effect, not just the next sync pass.
                if setup_holds().holds(aterm_types::macos_setup::Setting::Repositories)
                    || !aterm_machines::init_github::uses_vault_inventory(
                        &aterm_machines::port::Live::new(),
                        &ctx,
                    )?
                {
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

/// `sync.requests`'s read: where this Mac's `needs` stand, and whether the pass is due (Commons
/// on, this Mac a Writer). A ledger it cannot read is said, not the read's failure: the pass
/// still runs, so the custodian's half decides for the lab and the member's half fails the item
/// naming the ledger.
fn requests_read(
    needs: Result<Vec<aterm_secrets::need::Need>, String>,
    enabled: impl FnOnce() -> Result<bool, String>,
    writer: impl FnOnce() -> bool,
) -> Result<RequestsRead, String> {
    let standing = match needs {
        Ok(needs) => aterm_secrets::need::status_line(&needs)
            .unwrap_or_else(|| "no needs on this Mac".to_owned()),
        Err(why) => format!("this Mac's need ledger cannot be read ({why})"),
    };
    if !enabled()? {
        return Ok(RequestsRead::Idle(format!(
            "off: [commons] enabled = false ({standing})"
        )));
    }
    if !writer() {
        return Ok(RequestsRead::Idle(format!(
            "this Mac is not a Commons Writer, so the lab's table is not reached ({standing})"
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
        Err(why) => format!("this Mac's pass failed: {why}"),
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
mod tests {
    use super::*;

    /// `sync.identity` reads the fleet name beside the `aterm.toml` keys: a fleet name still due
    /// makes the row due even where the keys are current, and the Mac's own names that differ
    /// from the roster's label are recorded, made due only where the rule setup's one Mac
    /// password left is in place to name them with no password (no pass asks a password of its
    /// own). NEGATIVE CONTROLS: a fleet name in place leaves current keys current, a Mac already
    /// called by its label records nothing about its own names, a person's own `[identity]
    /// name` never names the Mac's own names, and without the rule a current read stays current.
    #[test]
    fn the_identity_row_reads_the_fleet_name_and_records_the_macs_own_name() {
        use aterm_agent::FleetName;
        let keys = || IdentityRead::Current("label m42 in [ai] and [fleet] machine".to_owned());
        let named_as = |local_host: &str, computer: &str| OwnNames {
            computer: Some(computer.to_owned()),
            local_host: Some(local_host.to_owned()),
            host: Some(local_host.to_owned()),
        };
        let own = named_as("Andrews-MacBook-Pro", "Andrews-MacBook-Pro");
        let due = FleetName::Due {
            from: "node-3fa2".to_owned(),
        };
        match with_fleet_name(keys(), "m42", Some("m42"), &due, &own) {
            IdentityRead::Due { from, to } => {
                assert_eq!(from, "fleet name node-3fa2");
                assert!(to.contains("fleet name m42"), "{to}");
                assert!(to.contains(own.local_host.as_deref().unwrap()), "{to}");
            }
            other => panic!("{other:?}"),
        }
        match with_fleet_name(keys(), "m42", Some("m42"), &FleetName::Current, &own) {
            IdentityRead::Current(now) => {
                assert!(now.contains("fleet name m42"), "{now}");
                assert!(
                    now.contains(
                        "this Mac's own name is Andrews-MacBook-Pro (Local hostname), not m42"
                    ),
                    "{now}"
                );
            }
            other => panic!("{other:?}"),
        }
        // A half-applied rename (one `[names]` row refused) or a Computer Name renamed in System
        // Settings still names the Mac otherwise, and so does a name scutil could not read.
        for half in [
            named_as("m42", "Andrew\u{2019}s MacBook Pro"),
            named_as("Andrews-MacBook-Pro", "m42"),
            OwnNames {
                computer: None,
                local_host: Some("m42".to_owned()),
                host: Some("m42".to_owned()),
            },
        ] {
            match with_fleet_name(keys(), "m42", Some("m42"), &FleetName::Current, &half) {
                IdentityRead::Current(now) => {
                    assert!(now.contains("own name is"), "{half:?}: {now}");
                }
                other => panic!("{other:?}"),
            }
        }
        let named = named_as("m42", "m42");
        match with_fleet_name(keys(), "m42", Some("m42"), &FleetName::Current, &named) {
            IdentityRead::Current(now) => assert!(!now.contains("own name"), "{now}"),
            other => panic!("{other:?}"),
        }
        match with_fleet_name(keys(), "studio", Some("m42"), &FleetName::Current, &own) {
            IdentityRead::Current(now) => assert!(!now.contains("own name"), "{now}"),
            other => panic!("{other:?}"),
        }
        let keys_due = IdentityRead::Due {
            from: "[ai] machine = unset".to_owned(),
            to: "label m42 in [ai] machine".to_owned(),
        };
        match with_fleet_name(keys_due, "m42", Some("m42"), &due, &named) {
            IdentityRead::Due { from, .. } => {
                assert_eq!(from, "[ai] machine = unset, fleet name node-3fa2");
            }
            other => panic!("{other:?}"),
        }
        let taken = FleetName::Taken("node-77".to_owned());
        assert!(matches!(
            with_fleet_name(keys(), "m42", Some("m42"), &taken, &named),
            IdentityRead::Current(_)
        ));
        let off = IdentityRead::NotApplicable("off: [identity] name = false".to_owned());
        assert_eq!(
            with_fleet_name(off.clone(), "m42", Some("m42"), &due, &own),
            off
        );
        let current = with_fleet_name(keys(), "m42", Some("m42"), &FleetName::Current, &own);
        match own_name_due(current.clone(), Some("m42"), true) {
            IdentityRead::Due { from, to } => {
                assert_eq!(from, "this Mac's own names");
                assert!(
                    to.ends_with("own names m42, through setup's naming rule"),
                    "{to}"
                );
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(own_name_due(current.clone(), Some("m42"), false), current);
        assert_eq!(own_name_due(current.clone(), None, true), current);
        assert_eq!(own_name_due(off.clone(), Some("m42"), true), off);
    }

    #[test]
    fn hostname_only_drift_is_visible_and_makes_the_identity_repair_due() {
        use aterm_agent::FleetName;

        let keys = IdentityRead::Current("label m86 in [ai] and [fleet] machine".to_owned());
        for host in [Some("m184"), None] {
            let own = OwnNames {
                computer: Some("m86".to_owned()),
                local_host: Some("m86".to_owned()),
                host: host.map(str::to_owned),
            };
            let pending = own_name_pending("m86", Some("m86"), &own)
                .expect("HostName still needs repair while the other two names match");
            assert!(pending.contains("(Hostname)"), "{pending}");
            assert!(pending.contains(host.unwrap_or("unread")), "{pending}");
            let row = with_fleet_name(keys.clone(), "m86", Some("m86"), &FleetName::Current, &own);
            assert!(matches!(&row, IdentityRead::Current(words) if words.contains(&pending)));
            assert!(matches!(
                own_name_due(row, Some("m86"), true),
                IdentityRead::Due { .. }
            ));
            assert_eq!(
                own_name_pending("studio", Some("m86"), &own),
                None,
                "a separate identity name still leaves OS naming alone"
            );
        }
        let repaired = OwnNames {
            computer: Some("m86".to_owned()),
            local_host: Some("m86".to_owned()),
            host: Some("m86".to_owned()),
        };
        assert_eq!(own_name_pending("m86", Some("m86"), &repaired), None);
        assert_eq!(
            own_name_due(keys.clone(), None, true),
            keys,
            "all three repaired names leave the current identity row current"
        );

        let mut aliased = OwnNames {
            computer: Some("m183".to_owned()),
            local_host: Some("m183-2".to_owned()),
            host: Some("m183".to_owned()),
        };
        assert_eq!(
            own_name_pending("m183", Some("m183"), &aliased),
            None,
            "a numbered Bonjour alias retains the admitted machine number"
        );
        for name in ["m183-x", "m183-02", "m183-1", "m183-2-extra", "m184-2"] {
            aliased.local_host = Some(name.to_owned());
            assert!(
                own_name_pending("m183", Some("m183"), &aliased).is_some(),
                "{name} is not the accepted local projection of m183"
            );
        }
        aliased.computer = Some("m184".to_owned());
        aliased.host = Some("m184".to_owned());
        aliased.local_host = Some("m183-2".to_owned());
        assert!(
            own_name_pending("m184", Some("m184"), &aliased).is_some(),
            "an earlier number's alias cannot fulfill a new naming intent"
        );
    }

    /// `sync.awake` plans a start only where one keeps the Mac awake: never over a person's off
    /// or without an installed app that has the verb (in `/Applications` or `~/Applications`),
    /// and never for a mug that runs and asks macOS again itself. The installed app is not asked
    /// about over an off or a running mug; a stopped mug's candidate that did not answer fails
    /// the read for the next pass.
    #[test]
    fn the_awake_row_plans_a_start_only_where_one_helps() {
        use aterm_acaffeine::held::Held;
        use aterm_acaffeine::launchd::Installed;
        let stopped = || Held::Stopped {
            why: Some("the menu bar refused the mug".to_owned()),
        };
        let at = || {
            Installed::At(PathBuf::from(
                "/Users//a/Applications/aterm.app/Contents/MacOS/aterm",
            ))
        };
        let absent = || Installed::Absent("aterm.app is not installed".to_owned());
        let unasked = || -> Installed { panic!("no start: the installed app is not asked") };
        assert!(matches!(
            awake_of(at, stopped()),
            Ok(AwakeRead::Due(why)) if why.contains("the menu bar refused the mug")
        ));
        assert!(matches!(
            awake_of(unasked, Held::Held { pid: 7 }),
            Ok(AwakeRead::Current(_))
        ));
        assert!(matches!(
            awake_of(unasked, Held::Off),
            Ok(AwakeRead::NotApplicable(_))
        ));
        assert!(matches!(
            awake_of(unasked, Held::NotHere("a private root".to_owned())),
            Ok(AwakeRead::NotApplicable(_))
        ));
        assert!(matches!(
            awake_of(absent, stopped()),
            Ok(AwakeRead::NotApplicable(_))
        ));
        assert!(
            awake_of(
                || Installed::Unanswered("did not answer `help acaffeine`".to_owned()),
                stopped()
            )
            .is_err()
        );
        assert!(matches!(
            awake_of(unasked, Held::NotHeld { pid: 7, why: None }),
            Ok(AwakeRead::Asking(_))
        ));
        assert!(matches!(
            awake_of(
                unasked,
                Held::Paused {
                    pid: 7,
                    why: aterm_acaffeine::model::Pause::LidClosed,
                    lingering: false
                }
            ),
            Ok(AwakeRead::Current(why)) if why.contains("lid is closed")
        ));
        assert!(
            awake_of(
                unasked,
                Held::Unread {
                    pid: 7,
                    why: "hung".to_owned()
                }
            )
            .is_err()
        );
    }

    /// `sync.requests` runs the custodian's pass whatever this Mac's own pass did, and says
    /// both: a member pass that fails (a damaged ledger on the vault's admin Mac) still lets the
    /// custodian decide for the lab. A copy or publication that failed in the custodian's pass
    /// fails the pass, naming the error, as the publish's own error did.
    /// NEGATIVE CONTROL: a pass that returns on the member pass's error never runs the
    /// custodian's; one that reads only the counts calls a failed publication a pass that went
    /// through.
    #[test]
    fn a_failing_member_pass_still_runs_the_custodians_and_a_failed_copy_fails_the_pass() {
        use aterm_db::credentials::{Custodian, Pass};
        let ran = std::cell::Cell::new(false);
        let lapsed = || {
            ran.set(true);
            Ok(Some(Custodian {
                retired: vec![([1; 16], aterm_secrets::need::Reason::Lapsed)],
                ..Custodian::default()
            }))
        };
        let said = requests_pass(|| Err("needs.toml: damaged".to_owned()), lapsed).unwrap_err();
        assert!(ran.get(), "{said}");
        assert!(
            said.contains("needs.toml: damaged") && said.contains("1 retired"),
            "{said}"
        );
        let failed = Custodian {
            failed: vec![([2; 16], "the vault's publication failed: refused".into())],
            ..Custodian::default()
        };
        let said = requests_pass(|| Ok(Pass::default()), || Ok(Some(failed))).unwrap_err();
        assert!(
            said.contains("1 failed") && said.contains("publication failed: refused"),
            "{said}"
        );
        assert_eq!(
            requests_pass(|| Ok(Pass::default()), || Ok(None)),
            Ok("0 posted, 0 answered, 0 decided here, 0 offered, 0 swept".to_owned())
        );
    }

    /// A need ledger `sync.requests` cannot read (damaged, on the vault's admin Mac) leaves the
    /// pass due, saying so, so its apply runs and the custodian's half still decides for the
    /// lab; the probe failing instead would read as unusable and run nothing.
    /// NEGATIVE CONTROL: the same ledger on a Mac that is no Writer, or with Commons off, stays
    /// idle, and a config that cannot be read still fails the read.
    #[test]
    fn a_need_ledger_that_cannot_be_read_still_runs_the_requests_pass() {
        let damaged = || Err("needs.toml: not a need ledger aterm wrote".to_owned());
        let due = requests_read(damaged(), || Ok(true), || true);
        assert!(
            matches!(&due, Ok(RequestsRead::Due(said)) if said.contains("needs.toml: not a need ledger")),
            "{due:?}"
        );
        assert!(matches!(
            requests_read(damaged(), || Ok(true), || false),
            Ok(RequestsRead::Idle(_))
        ));
        assert!(matches!(
            requests_read(damaged(), || Ok(false), || true),
            Ok(RequestsRead::Idle(_))
        ));
        assert!(requests_read(Ok(Vec::new()), || Err("aterm.toml".to_owned()), || true).is_err());
        assert_eq!(
            requests_read(Ok(Vec::new()), || Ok(true), || true),
            Ok(RequestsRead::Due("no needs on this Mac".to_owned()))
        );
    }

    /// `[harness] accounts = "off"` turns the pass's accounts row off before anything else is
    /// read; the same Mac without it reads its accounts (here: no source in a scratch home).
    /// NEGATIVE CONTROL: a read that ignores the switch reads the off Mac as having no source.
    #[test]
    fn harness_accounts_off_turns_the_accounts_row_off() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let (home, state) = (scratch.path().join("home"), scratch.path().join("state"));
        let config = scratch.path().join("aterm.toml");
        let roster = scratch.path().join("accounts.toml");
        aterm_uds::atomic_file::write(&config, b"[harness]\naccounts = \"off\"\n", 0o600).unwrap();
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
        aterm_uds::atomic_file::write(&config, configured.as_bytes(), 0o600).unwrap();
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
        aterm_uds::atomic_file::write(
            &config,
            format!("{configured}[harness]\naccounts='off'\n").as_bytes(),
            0o600,
        )
        .unwrap();
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
    /// and nothing waits (the LAN broker answers, or the vault is not on the LAN); a founding
    /// held for a person (an old approver) waits whatever the broker says. A read that is not
    /// this Mac's to found names why; a member's reads as the observation says.
    /// NEGATIVE CONTROL: a read that skips `waits` founds while the broker is absent; one that
    /// reads a held founding as not this Mac's leaves it out of the pass's log line.
    #[test]
    fn a_founding_waits_for_the_lan_broker_and_names_why_it_is_not_this_macs() {
        let unseen = || "no Commons control entry is visible (control-not-visible)".to_owned();
        let found = "vault alab is open and has no Commons control";
        let due = || Foundable::Due(found.to_owned());
        let free = |_: &str| None;
        let absent = |what: &str| Some(format!("the LAN broker is not installed, so {what} waits"));
        assert_eq!(
            unseen_read(unseen(), due(), free),
            SharedRead::Foundable(found.to_owned())
        );
        let read = unseen_read(unseen(), due(), absent);
        assert!(
            matches!(&read, SharedRead::Deferred(why)
                if why.starts_with(found) && why.contains("the founding waits")),
            "{read:?}"
        );
        let held = "vault alab: this Mac's approver (/x/aterm, aterm 0.142.0) shares no root";
        assert_eq!(
            unseen_read(unseen(), Foundable::Waiting(held.to_owned()), free),
            SharedRead::Deferred(held.to_owned())
        );
        assert_eq!(
            unseen_read(unseen(), Foundable::Waiting(held.to_owned()), absent),
            SharedRead::Deferred(held.to_owned())
        );
        for foundable in [Foundable::NotAdmin, Foundable::Founded] {
            assert_eq!(
                unseen_read(unseen(), foundable, free),
                SharedRead::Unavailable(unseen())
            );
        }
        assert_eq!(
            unseen_read(
                unseen(),
                Foundable::Elsewhere("vault alab: it names 2 admins".to_owned()),
                free
            ),
            SharedRead::Unavailable(format!("{}; vault alab: it names 2 admins", unseen()))
        );
        assert!(
            matches!(
                unseen_read(unseen(), Foundable::Busy, free),
                SharedRead::Unavailable(why) if why.starts_with(&unseen())
            ),
            "a busy read founds nothing and fails nothing"
        );
    }

    /// A state that stays busy past the wait reads busy, never unavailable (which a read-back
    /// reads as a failure), and reads nothing else; any other unavailable reading but an unseen
    /// control record reads unavailable, reading no founding.
    /// NEGATIVE CONTROL: a busy observation read as unavailable fails the founding read back.
    #[test]
    fn a_busy_state_reads_busy_and_reads_nothing_else() {
        use aterm_secrets::commons::{Observation, Unavailable};
        let no_vault = || -> Result<Option<aterm_secrets::client::Config>, String> {
            panic!("this read reads no vault")
        };
        let never = || -> Result<Foundable, String> { panic!("this read reads no founding") };
        let no_wait = |_: &aterm_secrets::client::Config, _: &str| -> Option<String> {
            panic!("this read asks nothing a founding waits on")
        };
        let read = shared_read(
            Observation::Unavailable(Unavailable::Busy),
            no_vault,
            never,
            no_wait,
        )
        .unwrap();
        assert!(
            matches!(&read, SharedRead::Busy(why) if why.contains("(vault-busy)")),
            "{read:?}"
        );
        let unconfigured = shared_read(
            Observation::Unavailable(Unavailable::Unconfigured),
            no_vault,
            never,
            no_wait,
        )
        .unwrap();
        assert!(
            matches!(&unconfigured, SharedRead::Unavailable(why) if why.contains("(vault-unconfigured)")),
            "{unconfigured:?}"
        );
    }

    /// The apply's report names what it founded or finished; a founding that stands for a person
    /// waits for them, and one that may pass is the next pass's error.
    /// NEGATIVE CONTROL: a report that reads a standing refusal as passing promises it a retry.
    #[test]
    fn the_apply_says_what_it_founded_and_what_stands() {
        let done = |published: bool, approved: &[&str]| {
            Ok(aterm_secrets::client::CommonsInit {
                network: [7; 16],
                authority: [1; 32],
                genesis: [2; 32],
                eligible: 3,
                published,
                approved: approved.iter().map(|host| (*host).to_owned()).collect(),
            })
        };
        let said = |done| match found_said(done) {
            Ok(SharedDone::Done(said)) => said,
            other => panic!("{other:?}"),
        };
        let network = "07".repeat(16);
        assert_eq!(
            said(done(true, &[])),
            format!("founded Commons network {network}")
        );
        assert!(said(done(true, &["m1", "m2"])).ends_with(
            "approving m1, m2: this Mac as its founder, the rest by the vault's open policy"
        ));
        assert!(said(done(false, &[])).contains("founded already"));
        assert_eq!(
            found_said(Err(NotFounded::Stands("answered 403".to_owned()))),
            Ok(SharedDone::Waiting("answered 403".to_owned()))
        );
        assert_eq!(
            found_said(Err(NotFounded::Again("timed out".to_owned()))),
            Err("timed out".to_owned())
        );
    }

    /// Only a publication to a vault on the LAN (a `.local` or single-label name, a link-local
    /// address or one on an attached subnet) waits for the LAN broker, which is read only then; a vault at a public
    /// URL or at a path waits for nothing.
    /// NEGATIVE CONTROL: a `publish_waits` that gates every URL defers the public vault.
    #[test]
    fn only_a_vault_on_the_lan_waits_for_the_lan_broker() {
        use aterm_secrets::transport::Source;
        let config = |source: Source| aterm_secrets::client::Config {
            source,
            mirrors: Vec::new(),
            name: "alab".to_owned(),
            signer: None,
            followed: Vec::new(),
            host: "m1".to_owned(),
        };
        for (source, lan) in [
            (
                Source::Url("http://alab-secrets.local:7484/v1/alab".into()),
                true,
            ),
            (Source::Url("http://m11:7484/v1/alab".into()), true),
            (
                Source::Url("http://169.254.10.20:7484/v1/alab".into()),
                true,
            ),
            (Source::Url("http://[fe80::1]:7484/v1/alab".into()), true),
            (Source::Url("https://vault.example/v1/alab".into()), false),
            (Source::Url("https://8.8.8.8/v1/alab".into()), false),
            (Source::Path("/Volumes/Keys/team.vault".into()), false),
        ] {
            let reads = std::cell::Cell::new(0);
            let waits = publish_waits(&config(source.clone()), "the founding", || {
                reads.set(reads.get() + 1);
                Some("the LAN broker is not installed".to_owned())
            });
            assert_eq!(waits.is_some(), lan, "{source:?}: {waits:?}");
            assert_eq!(reads.get(), u32::from(lan), "{source:?}");
            assert_eq!(
                publish_waits(&config(source.clone()), "the founding", || None),
                None,
                "{source:?}"
            );
        }
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
    /// process (so macOS attributes what it does to aterm.app), out of the background
    /// band the held copy starts its other children in (so the pass it runs lifts the fleet
    /// book's lock holder).
    /// NEGATIVE CONTROLS: a hand-off that keeps the held copy responsible, or that stays in the
    /// band, fails here.
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
        assert!(
            command.leaves_background_band(),
            "the pass handed off lifts its book lock's holder"
        );
    }

    /// `sync.node`'s read and its apply both answer a process that is not a shell of the
    /// account's own, as a test binary under its private state root is, with that reason and
    /// nothing else: neither reaches the job `systems.alab.aterm-node` or its unit file under
    /// `$HOME`, which is still the account's here.
    /// NEGATIVE CONTROL: without the gate in `through_node_job`, both go on to `sync.node`'s own
    /// gates and answer another way (this test root's state is in no fleet).
    #[test]
    fn sync_node_leaves_the_job_alone_outside_the_accounts_own_shell() {
        let why = aterm_types::dirs::not_own_account().expect("a test binary's private root");
        assert_eq!(
            Live::here().node(),
            Ok(NodeRead::NotApplicable(why.to_owned()))
        );
        assert_eq!(
            Live::here().ensure_node(),
            Ok(format!("left as it is: {why}"))
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
        let record = scratch.path().join("state/converge/identity-written.toml");
        aterm_uds::atomic_file::write(&config, b"[harness]\naccounts = \"wall\"\n", 0o600).unwrap();

        // No vault / personal vault without roster -> NotApplicable, despite ~/.aterm/machine.toml.
        assert!(matches!(
            identity_at(&config, &record, &paths, &["Mac-mini-M4.local".to_owned()]).unwrap(),
            IdentityRead::NotApplicable(_)
        ));

        // Roster-resolved label -> Due, then the apply -> Current, keeping [harness].
        let due = identity_owed(&config, &record, Some("m112")).unwrap().read;
        assert!(
            matches!(&due, IdentityRead::Due { from, to } if from.contains("unset") && to.contains("m112")),
            "{due:?}"
        );
        let said = write_identity_at(&config, &record, Some("m112")).unwrap();
        assert!(said.contains("m112"), "{said}");
        assert_eq!(
            identity_owed(&config, &record, Some("m112")).unwrap().read,
            IdentityRead::Current("label m112 in [ai] and [fleet] machine".to_owned())
        );
        let written = std::fs::read_to_string(&config).unwrap();
        let parsed: aterm_toml::Table = aterm_toml::from_str(&written).unwrap();
        assert_eq!(parsed["ai"]["machine"].as_str(), Some("m112"));
        assert_eq!(parsed["fleet"]["machine"].as_str(), Some("m112"));
        assert_eq!(parsed["harness"]["accounts"].as_str(), Some("wall"));

        // [swarm] enabled = false stands aside as NotApplicable.
        aterm_uds::atomic_file::write(&config, b"[swarm]\nenabled = false\n", 0o600).unwrap();
        assert_eq!(
            identity_at(&config, &record, &paths, &["m15.local".to_owned()]).unwrap(),
            IdentityRead::NotApplicable("off: [swarm] enabled = false".to_owned())
        );
    }

    /// The machine keys `config` holds after a pass, in `[ai]`, `[fleet]` order.
    fn machines(config: &Path) -> [Option<String>; 2] {
        read_identity_config(config).unwrap().machines
    }

    #[test]
    fn a_chosen_machine_number_drives_existing_identity_and_os_projections() {
        use aterm_agent::machine_name::file::{self, MachineName};
        use aterm_types::machine_name::{Class, Nickname, Number};

        let scratch = aterm_tempfile::tempdir().unwrap();
        let state = scratch.path().join("state");
        aterm_uds::private_file::create_private_dir(&state).unwrap();
        let mut name = MachineName::unnumbered(Nickname::parse("MissBunny").unwrap(), 1);
        name.num = Some(Number::parse("m86").unwrap());
        name.class = Class::Chosen;
        file::read_or_create(&state, || Ok(name)).unwrap();
        let mut choice = NameChoice::Roster;
        let label =
            projected_machine_label(Some(&state), &mut choice, Some("m184".to_owned())).unwrap();
        assert_eq!(label.as_deref(), Some("m86"));
        let config = scratch.path().join("aterm.toml");
        let record = state.join("converge/identity-written.toml");
        aterm_uds::atomic_file::write(
            &config,
            b"[ai]\nmachine = \"m184\"\n[fleet]\nmachine = \"m184\"\n",
            0o600,
        )
        .unwrap();
        // The roster's previous label still proves these unrecorded projections were ours.
        write_identity_after_read(
            &config,
            Some(&state),
            &record,
            label.as_deref(),
            &|word| word == "m184",
            || {},
        )
        .unwrap();
        assert_eq!(machines(&config), [Some("m86".into()), Some("m86".into())]);
        let (target, os_label) = identity_target_from(&NameChoice::Roster, label.clone()).unwrap();
        let old = OwnNames {
            computer: Some("m184".into()),
            local_host: Some("m184".into()),
            host: Some("m184".into()),
        };
        assert!(own_name_pending(&target, os_label.as_deref(), &old).is_some());
        let current = OwnNames {
            computer: Some(target.clone()),
            local_host: Some(target.clone()),
            host: Some(target),
        };
        assert!(own_name_pending("m86", os_label.as_deref(), &current).is_none());
        // A newer Rename supersedes the earlier identity string, including the runtime guard
        // an older installed build needed. Turning naming off remains a hold.
        file::replace(&state, |name| {
            name.num = Some(Number::parse("m87").unwrap());
            Ok(())
        })
        .unwrap();
        aterm_uds::atomic_file::write(
            &config,
            b"[identity]\nname = \"m86\"\n[ai]\nmachine = \"m86\"\n[fleet]\nmachine = \"m86\"\n",
            0o600,
        )
        .unwrap();
        let mut choice = NameChoice::Yours("m86".into());
        let label =
            projected_machine_label(Some(&state), &mut choice, Some("m184".into())).unwrap();
        let (target, os_label) = identity_target_from(&choice, label.clone()).unwrap();
        assert_eq!(target, "m87");
        assert_eq!(os_label.as_deref(), Some("m87"));
        assert!(own_name_pending(&target, os_label.as_deref(), &current).is_some());
        write_identity_after_read(
            &config,
            Some(&state),
            &record,
            label.as_deref(),
            &|_| false,
            || {},
        )
        .unwrap();
        assert_eq!(machines(&config), [Some("m87".into()), Some("m87".into())]);
        let mut off = NameChoice::Off;
        let label = projected_machine_label(Some(&state), &mut off, None).unwrap();
        assert!(identity_target_from(&off, label).is_none());
    }

    #[test]
    fn automatic_machine_numbers_leave_roster_naming_in_charge() {
        use aterm_agent::machine_name::file::{self, MachineName};
        use aterm_types::machine_name::{Class, Nickname, Number};

        let scratch = aterm_tempfile::tempdir().unwrap();
        let state = scratch.path();
        let fallback = || Some("m184".to_owned());
        let mut choice = NameChoice::Roster;
        assert_eq!(
            projected_machine_label(None, &mut choice, fallback()).unwrap(),
            fallback()
        );
        assert_eq!(
            projected_machine_label(Some(state), &mut choice, fallback()).unwrap(),
            fallback()
        );
        let mut name = MachineName::unnumbered(Nickname::parse("Heron").unwrap(), 1);
        name.num = Some(Number::parse("m8").unwrap());
        file::read_or_create(state, || Ok(name)).unwrap();
        for class in [Class::Auto, Class::Seeded] {
            file::replace(state, |held| {
                held.class = class;
                Ok(())
            })
            .unwrap();
            assert_eq!(
                projected_machine_label(Some(state), &mut choice, fallback()).unwrap(),
                fallback()
            );
        }
    }

    #[test]
    fn automatic_system_names_keep_their_source_current_after_each_read() {
        use aterm_agent::machine_name::file::{self, MachineName};
        use aterm_types::machine_name::{Class, Nickname, Number};

        #[derive(Clone, Copy)]
        enum Change {
            Config(&'static str),
            Number,
            UnreadName,
            NicknameAndUnrelated,
            None,
        }

        let scratch = aterm_tempfile::tempdir().unwrap();
        for (case, field, change, completed) in [
            (
                "identity-off",
                "ComputerName",
                Change::Config("[identity]\nname = false\n"),
                0,
            ),
            (
                "swarm-off",
                "LocalHostName",
                Change::Config("[swarm]\nenabled = false\n"),
                1,
            ),
            (
                "same-target-new-policy",
                "ComputerName",
                Change::Config(
                    "[identity]\nname = \"m86\"\n[ai]\nmachine = \"m86\"\n[fleet]\nmachine = \"m86\"\n",
                ),
                0,
            ),
            (
                "new-machine-key",
                "HostName",
                Change::Config("[ai]\nmachine = \"studio\"\n[fleet]\nmachine = \"m86\"\n"),
                2,
            ),
            ("new-number", "ComputerName", Change::Number, 0),
            ("unread-name", "ComputerName", Change::UnreadName, 0),
            (
                "unread-policy",
                "ComputerName",
                Change::Config("[identity\n"),
                0,
            ),
            (
                "nickname-and-unrelated",
                "LocalHostName",
                Change::NicknameAndUnrelated,
                3,
            ),
            ("unchanged", "ComputerName", Change::None, 3),
        ] {
            let dir = scratch.path().join(case);
            aterm_uds::private_file::create_private_dir(&dir).unwrap();
            let state = dir.join("state");
            aterm_uds::private_file::create_private_dir(&state).unwrap();
            let mut name = MachineName::unnumbered(Nickname::parse("MissBunny").unwrap(), 1);
            name.num = Some(Number::parse("m86").unwrap());
            name.class = Class::Chosen;
            file::read_or_create(&state, || Ok(name)).unwrap();
            let config = dir.join("aterm.toml");
            let record = state.join("converge/identity-written.toml");
            aterm_uds::atomic_file::write(&config, b"", 0o600).unwrap();
            write_identity_after_read(
                &config,
                Some(&state),
                &record,
                Some("m184"),
                &|_| true,
                || {},
            )
            .unwrap();
            let projection = projection_target_now(&config, Some(&state), Some("m184".into()))
                .unwrap()
                .unwrap();
            assert_eq!(projection.target.0, "m86");
            let sys = aterm_machines::port::Fake::root();
            sys.stat_of(
                aterm_machines::root_steps::ADMITTED_NAME_RULE,
                aterm_machines::port::Stat {
                    uid: 0,
                    gid: 0,
                    mode: 0o440,
                    dir: false,
                    symlink: false,
                    len: 1,
                },
            );
            let config_to_change = config.clone();
            let state_to_change = state.clone();
            let read = format!("/usr/sbin/scutil --get {field}");
            let changed_on = read.clone();
            sys.simulate(move |_, cmd| {
                let key = cmd.display();
                if key == changed_on {
                    // The read returns only after the real source changes; every effect is fake.
                    match change {
                        Change::Config(text) => {
                            aterm_uds::atomic_file::write(
                                &config_to_change,
                                text.as_bytes(),
                                0o600,
                            )
                            .unwrap();
                        }
                        Change::Number => {
                            file::replace(&state_to_change, |held| {
                                held.num = Some(Number::parse("m87").unwrap());
                                Ok(())
                            })
                            .unwrap();
                        }
                        Change::UnreadName => {
                            aterm_uds::private_file::write_private(
                                &state_to_change,
                                file::FILE,
                                b"unread name",
                            )
                            .unwrap();
                        }
                        Change::NicknameAndUnrelated => {
                            file::replace(&state_to_change, |held| {
                                held.nick = Nickname::parse("Heron").unwrap();
                                held.rev += 1;
                                Ok(())
                            })
                            .unwrap();
                            let mut text = std::fs::read_to_string(&config_to_change).unwrap();
                            text.push_str(
                                "\n[harness]\naccounts = \"wall\"\n[identity]\ngit = false\n",
                            );
                            aterm_uds::atomic_file::write(
                                &config_to_change,
                                text.as_bytes(),
                                0o600,
                            )
                            .unwrap();
                        }
                        Change::None => {}
                    }
                }
                if key.starts_with("/usr/sbin/scutil --get ") {
                    Some(aterm_machines::port::Ran::ok("m184\n"))
                } else if key.starts_with("/usr/bin/sudo -n /usr/sbin/scutil --set ") {
                    Some(aterm_machines::port::Ran::ok(""))
                } else {
                    None
                }
            });
            let current = || projection.check_current(&config, Some(&state), Some("m184".into()));
            let said = own_name_by_rule(&sys, &projection.target.0, &current);
            let calls = sys.calls();
            assert!(calls.contains(&read), "{case}: the source change must run");
            let writes: Vec<_> = calls
                .iter()
                .filter(|call| call.contains(" --set "))
                .cloned()
                .collect();
            let expected: Vec<_> = aterm_machines::root_steps::NAME_PREFS
                .into_iter()
                .take(completed)
                .map(|field| format!("/usr/bin/sudo -n /usr/sbin/scutil --set {field} m86"))
                .collect();
            assert_eq!(
                writes, expected,
                "{case}: {said:?}; no write may outlive its naming source"
            );
        }
    }

    /// Design §3 principle 4, "the person's choices win": a machine key the person set (here
    /// before aterm ever wrote one, so no record names it) is kept as they set it, on every pass;
    /// only the unset key takes the roster's label, and the row reads settled, never a change.
    /// NEGATIVE CONTROL: a pass that writes the label wherever it differs overwrites `studio`.
    #[test]
    fn sync_identity_keeps_a_machine_name_the_person_set() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let config = scratch.path().join("aterm.toml");
        let record = scratch.path().join("state/converge/identity-written.toml");
        aterm_uds::atomic_file::write(&config, b"[ai]\nmachine = \"studio\"\n", 0o600).unwrap();
        assert_eq!(
            identity_owed(&config, &record, Some("m112")).unwrap().read,
            IdentityRead::Due {
                from: "[fleet] machine = unset".to_owned(),
                to: "label m112 in [fleet] machine; [ai] machine = studio kept as you set it"
                    .to_owned(),
            }
        );
        write_identity_at(&config, &record, Some("m112")).unwrap();
        assert_eq!(
            machines(&config),
            [Some("studio".to_owned()), Some("m112".to_owned())]
        );
        for _ in 0..2 {
            assert_eq!(
                identity_owed(&config, &record, Some("m112")).unwrap().read,
                IdentityRead::Current(
                    "label m112; [ai] machine = studio kept as you set it".to_owned()
                )
            );
            let said = write_identity_at(&config, &record, Some("m112")).unwrap();
            assert!(said.starts_with("left as it is"), "{said}");
            assert_eq!(machines(&config)[0].as_deref(), Some("studio"));
        }
        // A person who changes aterm's own write later keeps theirs too.
        std::fs::write(
            &config,
            "[ai]\nmachine = \"studio\"\n[fleet]\nmachine = \"desk\"\n",
        )
        .unwrap();
        write_identity_at(&config, &record, Some("m113")).unwrap();
        assert_eq!(
            machines(&config),
            [Some("studio".to_owned()), Some("desk".to_owned())]
        );
    }

    /// The config snapshot that made a key owed must still stand when the edit lands: a
    /// later person choice, an Off, or a file that appeared is not a fresh write baseline.
    /// NEGATIVE CONTROL: reopening after the decision overwrites the person's choice and
    /// records that overwrite as aterm's own.
    #[test]
    fn sync_identity_refuses_a_choice_changed_after_its_read() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let ordinary = scratch.path().join("ordinary.toml");
        let ordinary_record = scratch.path().join("ordinary-written.toml");
        write_identity_at(&ordinary, &ordinary_record, Some("m112")).unwrap();
        assert_eq!(
            machines(&ordinary),
            [Some("m112".into()), Some("m112".into())]
        );
        assert_eq!(read_written(&ordinary_record).unwrap(), machines(&ordinary));

        let mut outcomes = Vec::new();
        for (name, present, owned, changed) in [
            ("unset-key", true, false, "[ai]\nmachine = \"studio\"\n"),
            ("own-key", false, true, "[fleet]\nmachine = \"desk\"\n"),
            (
                "identity-name",
                true,
                false,
                "[identity]\nname = \"studio\"\n",
            ),
            ("identity-off", true, false, "[identity]\nname = false\n"),
            ("swarm-off", true, false, "[swarm]\nenabled = false\n"),
            ("appeared", false, false, "[ai]\nmachine = \"new-choice\"\n"),
        ] {
            let dir = scratch.path().join(name);
            std::fs::create_dir(&dir).unwrap();
            let config = dir.join("aterm.toml");
            let record = dir.join("identity-written.toml");
            let backup = dir.join("aterm.toml.bak");
            if present {
                aterm_uds::atomic_file::write(&config, b"# the observed config\n", 0o600).unwrap();
            }
            if owned {
                write_identity_at(&config, &record, Some("m111")).unwrap();
                assert_eq!(
                    machines(&config),
                    [Some("m111".into()), Some("m111".into())]
                );
            }
            let record_before = std::fs::read(&record).ok();
            let backup_before = std::fs::read(&backup).ok();
            let mut reached = false;
            let result =
                write_identity_after_read(&config, None, &record, Some("m112"), &|_| true, || {
                    reached = true;
                    std::fs::write(&config, changed).unwrap();
                });
            assert!(
                reached,
                "{name}: the ordinary read must actually owe a write"
            );
            outcomes.push((
                name,
                result.is_err(),
                std::fs::read_to_string(&config).unwrap() == changed,
                std::fs::read(&record).ok() == record_before,
                std::fs::read(&backup).ok() == backup_before,
            ));
        }
        assert!(
            outcomes
                .iter()
                .all(|(_, refused, kept, record, backup)| *refused && *kept && *record && *backup),
            "a changed config must be refused before its choice, record or backup is written: {outcomes:?}"
        );
    }

    /// A key that still holds exactly what aterm last wrote is aterm's: when the roster's label
    /// moves, the pass moves it too.
    #[test]
    fn sync_identity_moves_its_own_write_when_the_label_moves() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let config = scratch.path().join("aterm.toml");
        let record = scratch.path().join("state/converge/identity-written.toml");
        write_identity_at(&config, &record, Some("m112")).unwrap();
        let due = identity_owed(&config, &record, Some("m113")).unwrap().read;
        assert!(
            matches!(&due, IdentityRead::Due { from, to }
                if from == "[ai] machine = m112, [fleet] machine = m112"
                    && to == "label m113 in [ai] machine and [fleet] machine"),
            "{due:?}"
        );
        write_identity_at(&config, &record, Some("m113")).unwrap();
        assert_eq!(
            machines(&config),
            [Some("m113".to_owned()), Some("m113".to_owned())]
        );
    }

    /// `[identity] name = false` turns the row off: nothing is read as due and nothing written.
    #[test]
    fn sync_identity_is_off_where_identity_name_is_false() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let home = scratch.path().join("home");
        let paths = aterm_secrets::client::Paths::for_home(&home);
        let config = scratch.path().join("aterm.toml");
        let record = scratch.path().join("state/converge/identity-written.toml");
        aterm_uds::atomic_file::write(&config, b"[identity]\nname = false\n", 0o600).unwrap();
        let off = IdentityRead::NotApplicable("off: [identity] name = false".to_owned());
        assert_eq!(
            identity_at(&config, &record, &paths, &["m15.local".to_owned()]).unwrap(),
            off
        );
        assert_eq!(
            identity_owed(&config, &record, Some("m112")).unwrap().read,
            off
        );
        let said = write_identity_at(&config, &record, Some("m112")).unwrap();
        assert_eq!(said, "left as it is: off: [identity] name = false");
        assert_eq!(machines(&config), [None, None]);
        assert!(!record.exists());
    }

    /// `[identity] name = "<name>"` is the person's name for this Mac and wins over the roster's
    /// label; `[identity] git = false` is said plainly (no pass writes git's identity).
    #[test]
    fn sync_identity_writes_the_persons_identity_name_over_the_roster_label() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let config = scratch.path().join("aterm.toml");
        let record = scratch.path().join("state/converge/identity-written.toml");
        aterm_uds::atomic_file::write(
            &config,
            b"[identity]\nname = \"studio\"\ngit = false\n",
            0o600,
        )
        .unwrap();
        let due = identity_owed(&config, &record, Some("m112")).unwrap().read;
        assert!(
            matches!(&due, IdentityRead::Due { to, .. }
                if to.starts_with("your name studio in [ai] machine and [fleet] machine")
                    && to.contains("[identity] git = false")),
            "{due:?}"
        );
        write_identity_at(&config, &record, Some("m112")).unwrap();
        assert_eq!(
            machines(&config),
            [Some("studio".to_owned()), Some("studio".to_owned())]
        );
        assert_eq!(
            identity_owed(&config, &record, Some("m112")).unwrap().read,
            IdentityRead::Current(
                "your name studio in [ai] and [fleet] machine; [identity] git = false, and no \
                 pass writes git's identity"
                    .to_owned()
            )
        );
        // Without the name, aterm's own write of `studio` takes the roster's label again.
        std::fs::write(
            &config,
            "[ai]\nmachine = \"studio\"\n[fleet]\nmachine = \"studio\"\n",
        )
        .unwrap();
        write_identity_at(&config, &record, Some("m112")).unwrap();
        assert_eq!(
            machines(&config),
            [Some("m112".to_owned()), Some("m112".to_owned())]
        );
    }

    /// `sync.identity` keeps the person's machine name, and only theirs: the roster labels an
    /// aterm before the record wrote (v0.146 and v0.147, both keys alike, no record) are aterm's,
    /// so when the roster's label moves the pass moves them; a pass cut off between recording
    /// its write and making it leaves the key at a value the record names, so its own earlier
    /// write never reads as the person's either. NEGATIVE CONTROL: a pair that is no roster
    /// label, with no record, is the person's.
    #[test]
    fn an_aterm_written_label_is_not_read_as_the_persons() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let config = scratch.path().join("aterm.toml");
        let record = scratch.path().join("state/converge/identity-written.toml");
        let snapshot = aterm_uds::atomic_file::ConfigFile::open(&config).unwrap();
        // An earlier aterm's write of the label, with no record.
        write_machine_label(&snapshot, "m112", &["ai", "fleet"]).unwrap();
        assert!(!record.exists());
        let read = identity_owed(&config, &record, Some("m113")).unwrap().read;
        assert!(
            matches!(&read, IdentityRead::Due { from, .. }
                if from == "[ai] machine = m112, [fleet] machine = m112"),
            "aterm's own label reads as the person's once the roster's moves: {read:?}"
        );
        // This aterm's write, cut off after its record: the record names both labels.
        record_written(&record, &["ai", "fleet"], "m113", &machines(&config)).unwrap();
        let read = identity_owed(&config, &record, Some("m114")).unwrap().read;
        assert!(matches!(read, IdentityRead::Due { .. }), "{read:?}");
        write_identity_at(&config, &record, Some("m114")).unwrap();
        assert_eq!(
            machines(&config),
            [Some("m114".to_owned()), Some("m114".to_owned())]
        );
        // NEGATIVE CONTROL: the person's own pair, with no record, is kept.
        let theirs = scratch.path().join("theirs.toml");
        let none = scratch.path().join("none/identity-written.toml");
        std::fs::write(
            &theirs,
            "[ai]\nmachine = \"studio\"\n[fleet]\nmachine = \"studio\"\n",
        )
        .unwrap();
        assert!(matches!(
            identity_owed(&theirs, &none, Some("m113")).unwrap().read,
            IdentityRead::Current(_)
        ));
    }

    /// A name the person sets back in Settings (`[ai] machine`, the field Settings edits) after
    /// the roster moved is theirs: the record's earlier values cover only a pass cut off between
    /// recording its write and making it, so once that write landed they go, and a value aterm
    /// wrote and then replaced is no longer aterm's.
    #[test]
    fn a_name_the_person_sets_back_stays_theirs() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let config = scratch.path().join("aterm.toml");
        let record = scratch.path().join("state/converge/identity-written.toml");
        write_identity_at(&config, &record, Some("m112")).unwrap();
        // The roster moves this Mac to m113, and the pass follows it: both keys land.
        write_identity_at(&config, &record, Some("m113")).unwrap();
        assert_eq!(
            machines(&config),
            [Some("m113".to_owned()), Some("m113".to_owned())]
        );
        // The person keeps the Mac's old name for their AI sessions.
        std::fs::write(
            &config,
            "[ai]\nmachine = \"m112\"\n[fleet]\nmachine = \"m113\"\n",
        )
        .unwrap();
        let owed = identity_owed(&config, &record, Some("m113")).unwrap();
        assert!(
            owed.write.is_none(),
            "the person's [ai] machine = m112 reads as aterm's: {:?}",
            owed.read
        );
    }

    /// With no record, a pair of alike roster labels is aterm's (v0.146 and v0.147 wrote them)
    /// only where the vault's roster gives that label to this Mac; any other such pair, a label
    /// the person typed that looks like a roster label among them, is theirs and kept.
    #[test]
    fn an_unrecorded_pair_is_aterms_only_where_the_roster_gives_it_to_this_mac() {
        let pair = [Some("m112".to_owned()), Some("m112".to_owned())];
        assert!(unrecorded_label(&pair, &|label| label == "m112"));
        assert!(!unrecorded_label(&pair, &|_| false));
        let scratch = aterm_tempfile::tempdir().unwrap();
        let config = scratch.path().join("aterm.toml");
        let record = scratch.path().join("state/converge/identity-written.toml");
        std::fs::write(
            &config,
            "[ai]\nmachine = \"m112\"\n[fleet]\nmachine = \"m112\"\n",
        )
        .unwrap();
        let snapshot = aterm_uds::atomic_file::ConfigFile::open(&config).unwrap();
        let theirs = identity_owed_in(&snapshot, &record, Some("m113"), &|_| false).unwrap();
        assert!(theirs.write.is_none(), "{:?}", theirs.read);
        let ours =
            identity_owed_in(&snapshot, &record, Some("m113"), &|label| label == "m112").unwrap();
        assert!(ours.write.is_some(), "{:?}", ours.read);
    }

    /// NEGATIVE CONTROL: `read_history`, `record_written`, `confirm_written`, `restore_record`,
    /// and `write_identity_at` refuse symlinked `state` or `state/converge` directories without
    /// reading, overwriting, or unlinking files in the symlink's target.
    #[cfg(unix)]
    #[test]
    fn identity_record_refuses_symlinked_state_and_converge_directories() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let outside = scratch.path().join("outside");
        std::fs::create_dir_all(outside.join("converge")).unwrap();
        let outside_record = outside.join("converge/identity-written.toml");
        std::fs::write(&outside_record, "ai = \"m112\"\nfleet = \"m112\"\n").unwrap();

        let config = scratch.path().join("aterm.toml");
        let linked_state = scratch.path().join("linked-state");
        std::os::unix::fs::symlink(&outside, &linked_state).unwrap();
        let via_state = linked_state.join("converge/identity-written.toml");

        assert!(identity_owed(&config, &via_state, Some("m113")).is_err());
        assert!(record_written(&via_state, &["ai", "fleet"], "m113", &[None, None]).is_err());
        assert!(confirm_written(&via_state, &["ai"]).is_err());
        assert!(restore_record(&via_state, None).is_err());
        assert!(restore_record(&via_state, Some(b"ai = \"m999\"\n")).is_err());
        assert!(write_identity_at(&config, &via_state, Some("m113")).is_err());
        assert_eq!(
            std::fs::read_to_string(&outside_record).unwrap(),
            "ai = \"m112\"\nfleet = \"m112\"\n"
        );

        let real_state = scratch.path().join("real-state");
        std::fs::create_dir(&real_state).unwrap();
        std::os::unix::fs::symlink(outside.join("converge"), real_state.join("converge")).unwrap();
        let via_converge = real_state.join("converge/identity-written.toml");

        assert!(identity_owed(&config, &via_converge, Some("m113")).is_err());
        assert!(restore_record(&via_converge, None).is_err());
        assert!(write_identity_at(&config, &via_converge, Some("m113")).is_err());
        assert_eq!(
            std::fs::read_to_string(&outside_record).unwrap(),
            "ai = \"m112\"\nfleet = \"m112\"\n"
        );
    }
}
