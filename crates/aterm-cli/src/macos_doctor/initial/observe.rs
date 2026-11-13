// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The read-only pass behind System State's rows (`docs/DESIGN-system-state-2026-10-21.md` §3.5,
//! §5): every local row, managed or not, read through the same items a run applies, in `Plan`
//! mode, which writes nothing. Reads of another app's data can still raise macOS privacy
//! dialogs, so their admission is separate from the run mode. It takes neither the profile
//! lock nor the setup slot, so it runs beside a pass; the caller drops a reading taken while
//! that row's own stage ran.

use super::*;
use aterm_machines::init::WorkstationGroup;
use aterm_machines::model::{Effect, Mode, Outcome};
use aterm_types::macos_setup::{
    Access, Observation, Read, SHARING_OFF, SHARING_OFF_NOTE, SYNC_OFF, Standing, Subject,
};

#[cfg(test)]
mod typed_join_tests;

/// The context for reading checklist rows, not a macOS permission grant.
#[derive(Clone, Copy)]
pub enum ObservationRead {
    /// An automatic refresh: evaluate the current page visibility at each private batch.
    Ordinary {
        /// The window's current frontness and person-presence check, with no privacy query.
        page_front: fn() -> bool,
    },
    /// A person's explicit Check, whose existing read intent survives the queue.
    ExplicitCheck,
}

impl ObservationRead {
    fn admits_private(self) -> bool {
        match self {
            Self::Ordinary { page_front } => page_front(),
            Self::ExplicitCheck => true,
        }
    }
}

/// Read `subjects` for `plan` without changing anything. `network` adds the
/// GitHub repositories row (the network and the token file); without it that row
/// is left out. A row that cannot be read comes back Unknown with the reason in
/// its detail; nothing is guessed. `admission` controls reads that can raise privacy dialogs.
/// A private reader that was not admitted contributes a withheld row
/// ([`Observation::withheld`]): no value and no failure, which retires no earlier evidence. An
/// attempted read that fails contributes an Unknown row, so a caller can retire earlier evidence
/// only for a read that actually ran.
pub fn observe_initial_setup(
    plan: &Plan,
    subjects: &[Subject],
    network: bool,
    admission: ObservationRead,
) -> Vec<Observation> {
    let unread = |subject: Subject, why: &str| Observation {
        detail: Some(why.to_owned()),
        ..Observation::unknown(subject)
    };
    if let Err(why) = ensure_person_scope() {
        return requested_subjects(subjects, network)
            .map(|subject| unread(subject, &why))
            .collect();
    }
    read_batches(subjects, network, admission, |subjects, network| {
        observe_subjects(plan, subjects, network)
    })
}

fn requested_subjects(subjects: &[Subject], network: bool) -> impl Iterator<Item = Subject> + '_ {
    subjects
        .iter()
        .copied()
        .filter(move |subject| network || *subject != Subject::Setting(Setting::Repositories))
}

/// Dispatch an observation's native reads under their own admission.
fn read_batches(
    subjects: &[Subject],
    network: bool,
    admission: ObservationRead,
    mut read: impl FnMut(&[Subject], bool) -> Vec<Observation>,
) -> Vec<Observation> {
    // Each batch has independent admission and native state. An unwind retires only its
    // attempted readings; completed batches and skipped private readers keep their evidence.
    let mut read = |subjects: &[Subject], network| {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| read(subjects, network)))
            .unwrap_or_else(|_| {
                subjects
                    .iter()
                    .map(|subject| Observation {
                        note: Some("This reading did not finish. Check again.".into()),
                        ..Observation::unknown(*subject)
                    })
                    .collect()
            })
    };
    let mut safe = Vec::new();
    let mut workstation = Vec::new();
    let mut spotlight = Vec::new();
    for subject in requested_subjects(subjects, network) {
        match subject {
            _ if !subject.reads_privately() => safe.push(subject),
            Subject::Setting(Setting::Spotlight) => spotlight.push(subject),
            _ => workstation.push(subject),
        }
    }
    let mut out = if safe.is_empty() {
        Vec::new()
    } else {
        read(&safe, network)
    };
    // A native batch may wait on another process or the network. Its entry needs the page's
    // current visibility, not the one a queue or an earlier batch saw. FDA and Plan mode
    // establish no permission to query another app's preferences or protected project paths.
    for private in [workstation, spotlight] {
        if private.is_empty() {
            continue;
        }
        if admission.admits_private() {
            out.extend(read(&private, false));
        } else {
            // Withheld, not failed: no value and no failure is said of a row nothing read, and
            // an earlier reading of it stands (`Read::Withheld`).
            out.extend(private.into_iter().map(Observation::withheld));
        }
    }
    let order = Subject::all();
    out.sort_by_key(|row| order.iter().position(|subject| *subject == row.subject));
    out
}

/// [`observe_initial_setup`] past its scope check.
fn observe_subjects(plan: &Plan, subjects: &[Subject], network: bool) -> Vec<Observation> {
    let unread = |subject: Subject, why: &str| Observation {
        detail: Some(why.to_owned()),
        ..Observation::unknown(subject)
    };
    let sys = aterm_machines::port::Live::new();
    let mut out = Vec::with_capacity(subjects.len());
    let root = state_root().ok();
    // What aterm last heard at the ALab vault's address, read once for the vault row and the
    // repositories beside it.
    let heard = std::cell::OnceCell::new();
    let here = || *heard.get_or_init(|| heard_for(plan, root.as_deref(), &secrets_retry::Live));
    // Whether `[swarm] enabled = false` turns sharing off, read once for the same two rows: the
    // Secrets stage then joins and syncs nothing, and the repositories run on this Mac's own GitHub
    // credentials ([`vault_not_here`]).
    let swarm = std::cell::OnceCell::new();
    let sharing_off =
        || *swarm.get_or_init(|| matches!(aterm_secrets::swarm::enabled_live(), Ok(false)));

    let mut workstation = Selection::none();
    for subject in subjects {
        if let Subject::Setting(setting) = subject
            && setting.is_workstation()
        {
            workstation.set(*setting, true);
        }
    }
    if !workstation.is_empty() {
        let read = aterm_machines::init::prepare_workstation_selected(
            &sys,
            WorkstationGroup::All,
            workstation,
        )
        .and_then(|prepared| prepared.run_tagged(&sys, Mode::Plan));
        let held = root.as_deref().map_or_else(Selection::none, held_at_login);
        let changed_since = root
            .as_deref()
            .map_or_else(Selection::none, super::kept::changed_since_at);
        for setting in Setting::ALL {
            if !workstation.contains(setting) {
                continue;
            }
            out.push(match &read {
                Ok(rows) => {
                    let rows: Vec<_> = rows
                        .iter()
                        .filter(|(tag, _)| *tag == setting)
                        .map(|(_, row)| row)
                        .collect();
                    changed_since_note(
                        workstation_observation(setting, &rows, held),
                        changed_since.contains(setting),
                    )
                }
                Err(why) => unread(Subject::Setting(setting), why),
            });
        }
    }

    let wants = |setting: Setting| subjects.contains(&Subject::Setting(setting));
    let context = || LiveInitial::live().context();
    if wants(Setting::Spotlight) {
        out.push(
            context()
                .and_then(|ctx| aterm_machines::init::run_initial_spotlight(&sys, &ctx, Mode::Plan))
                .map_or_else(
                    |why| unread(Subject::Setting(Setting::Spotlight), &why),
                    |rows| {
                        aterm_machines::init_observe::observation(
                            Setting::Spotlight,
                            &rows.iter().collect::<Vec<_>>(),
                        )
                    },
                ),
        );
    }
    if wants(Setting::Secrets) {
        out.push(vault_row(sharing_off(), || {
            vault(plan, here(), root.as_deref())
        }));
    }
    if network && wants(Setting::Repositories) {
        out.push(
            context()
                .and_then(|ctx| repositories_heard(&sys, &ctx, plan, sharing_off(), here))
                .unwrap_or_else(|why| unread(Subject::Setting(Setting::Repositories), &why)),
        );
    }

    let access: Vec<Access> = subjects
        .iter()
        .filter_map(|subject| match subject {
            Subject::Access(access) => Some(*access),
            Subject::Setting(_) => None,
            Subject::Sync(_) => None,
        })
        .collect();
    if !access.is_empty() {
        // This approval's steps, for a Not now (§4).
        let records = state_root()
            .and_then(|root| access_revision_at(&root))
            .map(super::access::records)
            .unwrap_or_default();
        let grants = super::access::read(plan.selection, &records);
        out.extend(
            access
                .into_iter()
                .map(|access| super::access::observation(access, &grants, plan.selection)),
        );
    }
    out
}

/// One workstation row's reading from its items' rows, "at login" while this login session
/// holds a change a run made to it for the next login ([`held_at_login`]) and nothing that
/// change set is live already: the Caps Lock key's live mappings read in place
/// ([`aterm_machines::init_observe::live_in_place`]) put it in effect now, so only a row that
/// truly waits for the login (Trackpad scrolling) says so.
fn workstation_observation(
    setting: Setting,
    rows: &[&aterm_machines::model::ItemReport],
    held: Selection,
) -> Observation {
    at_login(
        aterm_machines::init_observe::observation(setting, rows),
        held.contains(setting) && !aterm_machines::init_observe::live_in_place(setting, rows),
    )
}

/// What a run of the items of `tagged` (each with the System State row it was built for) read
/// of `selected`'s rows, as System State's readings of them, dated `at` (from before that run's
/// read started): each row with any item is its whole cohort's native reading
/// ([`aterm_machines::init_observe::readback_observation`]: a part held, unread or failed stays
/// part of it, so it reads short of in place), "at login" while this login session holds a
/// change an earlier run made to it for the next login ([`at_login`]). A row with no item reads
/// nothing back: no stage record or exit code stands in for a reading (design §3.5).
pub(in crate::macos_doctor) fn read_back(
    tagged: &[(Setting, &aterm_machines::model::ItemReport)],
    selected: Selection,
    at: u64,
) -> Vec<Observation> {
    let mut held = None;
    Setting::ALL
        .into_iter()
        .filter(|setting| selected.contains(*setting))
        .filter_map(|setting| {
            let rows: Vec<_> = tagged
                .iter()
                .filter(|(member, _)| *member == setting)
                .map(|(_, row)| *row)
                .collect();
            if rows.is_empty() {
                return None;
            }
            let observation = aterm_machines::init_observe::readback_observation(setting, &rows);
            let observation = if setting.is_workstation() {
                let held = *held.get_or_insert_with(|| {
                    state_root().map_or_else(|_| Selection::none(), |root| held_at_login(&root))
                });
                at_login(
                    observation,
                    held.contains(setting)
                        && !aterm_machines::init_observe::live_in_place(setting, &rows),
                )
            } else {
                observation
            };
            Some(observation.stamped(at))
        })
        .collect()
}

/// A row a run changed for macOS to take up at the next login reads "at login" while the value
/// it wrote still reads in place, until this login session ends; any other reading is kept.
fn at_login(observation: Observation, held: bool) -> Observation {
    if !held || observation.standing != Standing::InPlace {
        return observation;
    }
    Observation {
        standing: Standing::AtLogin,
        now: Some(observation.wanted.clone()),
        ..observation
    }
}

/// A row whose key changed after aterm set it and read it back ([`super::kept`]) reads
/// [`aterm_types::macos_setup::CHANGED_SINCE`] while it still differs: no automatic pass sets it
/// again, so the page says it changed rather than that it will change, and never whose change it
/// is, which nothing tells. Any other reading is kept.
fn changed_since_note(observation: Observation, kept: bool) -> Observation {
    if !kept || observation.standing != Standing::Differs {
        return observation;
    }
    Observation {
        note: Some(aterm_types::macos_setup::CHANGED_SINCE.to_owned()),
        ..observation
    }
}

/// What the Secrets vault row reads, from the local records alone.
#[derive(Clone, Debug, PartialEq, Eq)]
enum VaultRead {
    /// No vault is configured for this login.
    Unconfigured,
    /// Another vault, or another signer, than the plan names.
    OtherVault,
    /// Asked to join a vault other than ALab's, or one at a path, which no Allow reaches; an admin
    /// Mac has not approved it yet: `approve` is the command that lets it in, when this Mac has a
    /// key ([`client::Status::approve_command`]: its code, or at a path, which has no queue, its
    /// key and name).
    /// `with_password` when a join password went with the request
    /// (`client::asked_with_password`): only then can a mistyped one be why it waits.
    Pending {
        approve: Option<String>,
        with_password: bool,
    },
    /// Asked to join the default ALab vault at a URL. On the ALab LAN admission and retries run
    /// automatically; off it nothing lets this Mac in by itself, so the note follows where this
    /// Mac is (`at`, its network place: [`aterm_secrets::alab_join::JoinPlace`]) and names no admin
    /// and no alert either way.
    /// `with_password` as for [`Self::Pending`].
    Asking {
        with_password: bool,
        at: aterm_secrets::alab_join::JoinPlace,
    },
    /// Asked to join the ALab vault, and a member let it in: it holds that member's `Allowed`
    /// ([`aterm_secrets::client::holds_allowed`]) and waits for the vault to record it.
    Allowed,
    /// Asked to join with a join password an admin Mac refused
    /// (`client::password_refused`): only the password typed again lets it in, so the row's
    /// fields open for it.
    Refused,
    /// Asked to join, and the person turned sync off since
    /// ([`aterm_secrets::service::subscriber_off_path`]), so the subscriber that finishes an
    /// approved join does not run. It reads Sync off in place, as [`Self::TurnedOff`] does.
    PendingOff { approve: Option<String> },
    /// A member whose subscriber runs; `cache_ready` meets the vault's local ownership checks.
    Member { cache_ready: bool },
    /// A member whose subscriber, the process that keeps `~/.secrets` in sync, is installed but
    /// has stayed down past launchd's restart window. Nothing starts it again by itself: the
    /// Secrets stage installs it, so Set up now does.
    Stopped,
    /// Its vault let it go with no act against it that it can read: a rotation left it out
    /// ([`aterm_secrets::client::dropped`], one-mesh E2). Every member that read the rotation
    /// refuses its key, so the Secrets stage plans no join, and it comes back only as a new Mac,
    /// its vault folder set aside: a person's step, never Set up now's.
    Dropped,
    /// A member with no subscriber installed and no record that the person turned it off. The
    /// Secrets stage installs it, so Set up now does.
    NotSetUp,
    /// A member whose subscriber does not run because the person turned it off
    /// (`aterm_secrets::service::subscriber_off_path`: `aterm secrets agent off`, or
    /// a `join` or `create` with `--no-agent`): their choice, the row's wanted
    /// state (design §4), which setup leaves be.
    TurnedOff,
    /// A member whose subscriber is not running and is never installed from here: this aterm runs
    /// from a copy that goes away (a disk image, an App Translocation copy, a temporary or cache
    /// directory), which a launchd job must not name (`aterm_secrets::service::transient`). Only a
    /// person moves aterm.
    Fleeting,
    /// The vault dropped this Mac.
    Revoked,
}

/// A member's subscriber, as `aterm_secrets::service::state` reads its job and
/// the person's record beside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Subscriber {
    Running,
    /// Installed, and down past launchd's restart window ([`subscriber_now`]).
    Stopped,
    /// Not installed.
    Removed,
    /// Not running, and the person turned it off
    /// (`aterm_secrets::service::subscriber_turned_off`).
    TurnedOff,
}

/// A member's reading, from its local `cache_ready`, its `subscriber`, and `exe` (the
/// program this aterm runs from, `None` when unknown).
fn member_read(cache_ready: bool, subscriber: Subscriber, exe: Option<&Path>) -> VaultRead {
    match subscriber {
        Subscriber::Running => VaultRead::Member { cache_ready },
        Subscriber::TurnedOff => VaultRead::TurnedOff,
        _ if exe.is_some_and(aterm_secrets::service::transient) => VaultRead::Fleeting,
        Subscriber::Stopped => VaultRead::Stopped,
        Subscriber::Removed => VaultRead::NotSetUp,
    }
}

/// When this process first saw the subscriber installed but not running, while
/// it has not run since: a later read past the restart window takes it as
/// stopped at once ([`subscriber_now`]).
static SUBSCRIBER_DOWN_SINCE: std::sync::Mutex<Option<std::time::Instant>> =
    std::sync::Mutex::new(None);

/// This login's subscriber now. One read installed but not running may be launchd's gap between
/// two `KeepAlive` runs, so it is read again until it runs or stays down past
/// `aterm_secrets::service::RESTART_WINDOW` since first seen down ([`SUBSCRIBER_DOWN_SINCE`]).
fn subscriber_now(home: &Path) -> Result<Subscriber, String> {
    use aterm_secrets::service::{self, Job, State};
    let running = || -> Result<Option<bool>, String> {
        Ok(
            match service::state(home, Job::Subscriber).map_err(|error| error.to_string())? {
                State::Installed { pid: Some(_), .. } => Some(true),
                State::Installed { pid: None, .. } => Some(false),
                State::Absent => None,
            },
        )
    };
    let first = running()?;
    let mut since = SUBSCRIBER_DOWN_SINCE.lock().ok().and_then(|since| *since);
    let subscriber = match first {
        Some(true) => Subscriber::Running,
        _ if service::subscriber_turned_off(home) => Subscriber::TurnedOff,
        None => Subscriber::Removed,
        Some(false) => {
            let back = service::back_within_restart_window(
                &mut since,
                &mut std::time::Instant::now,
                &mut || running().map(|state| state == Some(true)),
                &mut std::thread::sleep,
            )?;
            if back {
                Subscriber::Running
            } else {
                Subscriber::Stopped
            }
        }
    };
    if subscriber != Subscriber::Stopped {
        since = None;
    }
    if let Ok(mut down) = SUBSCRIBER_DOWN_SINCE.lock() {
        *down = since;
    }
    Ok(subscriber)
}

/// Whether the person turned this login's subscriber off and it does not run: read at
/// once, with no wait for launchd, since a record says enough.
fn sync_turned_off(home: &Path) -> bool {
    use aterm_secrets::service::{self, Job, State};
    service::subscriber_turned_off(home)
        && !matches!(
            service::state(home, Job::Subscriber),
            Ok(State::Installed { pid: Some(_), .. })
        )
}

/// What aterm last heard at the ALab vault's address ([`scope::heard_at`]), for a `plan` that names
/// that vault: only its address is probed, so a plan naming another vault hears nothing there.
fn heard_for(
    plan: &Plan,
    root: Option<&Path>,
    reads: &dyn secrets_retry::Reads,
) -> scope::VaultHere {
    if plan.vault_url == aterm_types::macos_setup::DEFAULT_VAULT_URL {
        scope::heard_at(root, reads)
    } else {
        scope::VaultHere::Unasked
    }
}

/// The GitHub repositories row for `plan`, read as their stage runs ([`repositories::observe`]):
/// through the plan's vault where Secrets is in the plan, but on this Mac's own GitHub credentials
/// where `[swarm] enabled = false` turns sharing off (`sharing_off`), or what aterm last heard at
/// the ALab vault's address (`here`, [`heard_for`], read only then) leaves it off this network, as
/// the stage runs them beside a Secrets try that left it so ([`vault_not_here`]).
fn repositories_heard(
    sys: &dyn aterm_machines::port::System,
    ctx: &aterm_machines::items::Ctx,
    plan: &Plan,
    sharing_off: bool,
    here: impl FnOnce() -> scope::VaultHere,
) -> Result<Observation, String> {
    let vault = super::Stage::Secrets.selected(plan).then_some(plan);
    super::repositories::observe(
        sys,
        ctx,
        vault,
        vault.is_some() && (sharing_off || here().not_here()),
    )
}

/// The Secrets vault row: [`vault_sharing_off`] while `[swarm] enabled = false` turns sharing off
/// (`sharing_off`), which reads no membership, else what `read` reads ([`vault`]), or why it could
/// not ([`vault_unread`]).
fn vault_row(sharing_off: bool, read: impl FnOnce() -> Result<Observation, String>) -> Observation {
    if sharing_off {
        return vault_sharing_off();
    }
    read().unwrap_or_else(|why| vault_unread(&why))
}

/// The Secrets vault row while `[swarm] enabled = false` turns sharing with the person's other
/// Macs off: in place at [`SHARING_OFF`], their choice and so what the row wants, whatever this
/// Mac's membership reads, with [`SHARING_OFF_NOTE`] its one note. The Secrets stage joins and
/// syncs nothing then ([`automatic_secrets_rows`]) and a running subscriber waits for the switch,
/// so no reading here is a change Set up now could make.
fn vault_sharing_off() -> Observation {
    Observation {
        subject: Subject::Setting(Setting::Secrets),
        standing: Standing::InPlace,
        now: Some(SHARING_OFF.to_owned()),
        after_login: None,
        wanted: SHARING_OFF.to_owned(),
        note: Some(SHARING_OFF_NOTE.to_owned()),
        detail: None,
        read: Read::Unstamped,
    }
}

/// The Secrets vault row for `plan`, from this Mac's vault records and what aterm last heard at the
/// ALab vault's address (`here`, [`heard_for`]), with the run's records in `root`.
fn vault(plan: &Plan, here: scope::VaultHere, root: Option<&Path>) -> Result<Observation, String> {
    use aterm_secrets::client::{self, Membership, Paths};
    let paths = Paths::live().map_err(|error| error.to_string())?;
    let status = client::status(&paths).map_err(|error| error.to_string())?;
    let read = match (
        check::selected_vault(plan, &paths.home, &status)?,
        &status.membership,
    ) {
        (_, Membership::Unconfigured) => VaultRead::Unconfigured,
        (None, _) => VaultRead::OtherVault,
        (Some(_), Membership::Pending) if client::password_refused(&paths) => VaultRead::Refused,
        (Some(_), Membership::Pending) => pending_read(&paths, &status, join_place_now),
        (Some(_), Membership::Revoked { .. }) if client::dropped(&paths) => VaultRead::Dropped,
        (Some(_), Membership::Revoked { .. }) => VaultRead::Revoked,
        (Some(config), Membership::In { .. } | Membership::Plain) => member_read(
            client::materialization_ready_for(&paths, config).map_err(|error| error.to_string())?,
            subscriber_now(&paths.home)?,
            std::env::current_exe()
                .and_then(std::fs::canonicalize)
                .ok()
                .as_deref(),
        ),
    };
    Ok(vault_heard(&read, plan.selection, here, root))
}

/// Where this Mac is for its own ALab join's words, from its network place
/// ([`aterm_secrets::alab_join::JoinPlace::of`]); not known in a unit test (the state root and
/// route are the person's), off Unix, or with no state directory.
fn join_place_now() -> aterm_secrets::alab_join::JoinPlace {
    #[cfg(all(unix, not(test)))]
    if let Some(state) = aterm_types::dirs::state_dir() {
        return aterm_secrets::alab_join::JoinPlace::of(&aterm_netplace::read(&state));
    }
    aterm_secrets::alab_join::JoinPlace::Unknown
}

/// The Secrets row's read of this Mac's join while it waits (`status`, with the records under
/// `paths`). The default ALab vault at a URL admits a Mac automatically on the ALab LAN, so its
/// words name no person step and no admin there, and off it say what lets this Mac in, as `at`
/// reads where this Mac is, only for that join ([`VaultRead::Asking`], and
/// [`VaultRead::Allowed`] once the vault has let it in); another vault, or one at a path, which
/// automatic admission does not reach, by the approve an admin Mac runs
/// ([`aterm_secrets::client::Status::approve_command`]).
fn pending_read(
    paths: &aterm_secrets::client::Paths,
    status: &aterm_secrets::client::Status,
    at: impl FnOnce() -> aterm_secrets::alab_join::JoinPlace,
) -> VaultRead {
    use aterm_secrets::client;
    use aterm_secrets::transport::Source;
    let allow = status.config.as_ref().is_some_and(|config| {
        matches!(config.source, Source::Url(_)) && client::alab_vault(config)
    });
    let approve = status.approve_command().filter(|_| !allow);
    let with_password = client::asked_with_password(paths);
    if sync_turned_off(&paths.home) {
        VaultRead::PendingOff { approve }
    } else if allow && client::holds_allowed(paths) {
        VaultRead::Allowed
    } else if allow {
        VaultRead::Asking {
            with_password,
            at: at(),
        }
    } else {
        VaultRead::Pending {
            approve,
            with_password,
        }
    }
}

/// [`vault_observation`] of `read` for `selection`, with what aterm last heard at the ALab vault's
/// address for its plan (`here`, [`heard_for`]), and whether a join password the person typed is
/// held for a try there ([`scope::join_typed`]), as the records in `root` say.
fn vault_heard(
    read: &VaultRead,
    selection: Selection,
    here: scope::VaultHere,
    root: Option<&Path>,
) -> Observation {
    let here = here.note();
    // Only a row the probe left a note on can wait for a join, so only then is the run read.
    let typed = here.is_some() && root.is_some_and(scope::join_typed);
    vault_observation(read, selection, here, typed)
}

/// The Secrets vault row's note when aterm cannot read this Mac's vault settings (a
/// root-owned `source.toml` from a `sudo aterm secrets …`, a newer format, a state file it
/// cannot parse): calm, since the rest of setup goes ahead without them (System State design
/// §3.3), and in the page's words, never the file or the error, which `aterm secrets status`
/// prints.
pub(super) const VAULT_UNREAD: &str =
    "aterm cannot read this Mac's vault settings; aterm secrets status says why";

/// The Secrets vault row aterm could not read (`why`, kept as its detail for `aterm doctor`'s
/// eyes): not read, with [`VAULT_UNREAD`] as its one calm note.
fn vault_unread(why: &str) -> Observation {
    Observation {
        note: Some(VAULT_UNREAD.to_owned()),
        detail: Some(why.to_owned()),
        ..Observation::unknown(Subject::Setting(Setting::Secrets))
    }
}

/// The note of a member Mac whose subscriber runs and whose credentials are not
/// all written yet: what it waits on.
const SYNCING: &str = "Waits for the vault to finish syncing this Mac";

/// The note of a member Mac whose subscriber's job is installed but not running:
/// nothing restarts it by itself, and Set up now (the Secrets stage) starts it.
const STOPPED: &str = "The sync is not running; Set up now starts it";

/// The note of a member Mac with no subscriber installed and no record that the person turned
/// it off. It names no command, since several leave it so.
const NOT_SET_UP: &str = "Not set up to sync this Mac; Set up now installs it";

/// The note of a member Mac whose subscriber the person turned off
/// (`aterm_secrets::service::subscriber_off_path`): their choice, said as theirs.
/// The row's quiet Turn On Sync button turns it back on.
const TURNED_OFF: &str = "You turned sync off on this Mac";

/// The note of a Mac that asked to join and whose sync the person turned off since:
/// the join lands only once sync is on again.
const PENDING_OFF: &str = "You turned sync off, so an approved join lands once it's on";

/// The note of a member Mac whose subscriber cannot run from this transient copy.
/// A normal released app installs itself before setup continues; a read-only check
/// does not start that installation. The menu line carries the note too.
const FLEETING: &str = "Open the released aterm app to install it automatically and continue setup";

/// The note of a Mac that asked to join and whose approval code this Mac does not
/// hold: an admin Mac approves it, and the subscriber finishes the join.
const APPROVAL: &str = "Waits for an admin Mac to approve this Mac";

/// What a pending join sent with a password adds to its note: a mistyped password matches
/// nothing, and Join with it retyped under the row's disclosure sends a new proof.
const RETYPE: &str = "or retype the join password";

/// What heads the command that approves exactly a pending Mac on an admin Mac
/// (`aterm secrets approve <code>`, or `<age1…> --name <host>` at a path).
const APPROVE: &str = "On an admin Mac:";

/// The note of a Mac a member let into the ALab vault, which waits for the vault to record it.
const ALLOWED: &str = aterm_types::macos_setup::VAULT_ALLOWED_NOTE;

/// The value of a join an admin Mac refused for its join password: its fields open to take it
/// again (no note, so `vault_shown` opens them).
const WRONG_PASSWORD: &str = "Wrong password";

/// A dropped Mac's note ([`VaultRead::Dropped`]): the way back, naming no path, which the page
/// never shows (`aterm secrets status` names the folder).
const DROPPED: &str = "A rotation left this Mac out; set its vault folder aside to join again as \
                       a new Mac (aterm secrets status says where)";

/// The Secrets vault row in §3.3's words: Not joined, Awaiting code, Other vault, Out of sync,
/// Not syncing, Not set up, Local cache ready, or Sync off (the person's choice, so in place). A stopped or
/// uninstalled subscriber differs, with a note only on a row `selection` manages. `here` is the
/// note of what aterm last heard at the ALab vault's address when the plan names that vault
/// ([`scope::heard_at`], [`scope::VaultHere::note`]), kept on a managed row not joined. Where the
/// run in force holds a join password the person typed (`typed`, [`scope::join_typed`]), which it
/// keeps for a try that reaches the vault, the row waits with it. Otherwise the join is aterm's
/// own, which joins a vault only where one answers, so the row stands not joined, in place,
/// whatever other groups `selection` sets up.
fn vault_observation(
    read: &VaultRead,
    selection: Selection,
    here: Option<&str>,
    typed: bool,
) -> Observation {
    let subject = Subject::Setting(Setting::Secrets);
    let managed = selection.contains(Setting::Secrets);
    let person = if managed {
        Standing::NeedsYou
    } else {
        Standing::Differs
    };
    let (standing, now, note): (Standing, &str, Option<String>) = match read {
        VaultRead::Member { cache_ready: true } => (
            Standing::InPlace,
            Setting::Secrets.wanted(),
            Some(aterm_types::macos_setup::VAULT_LOCAL_READY_NOTE.to_owned()),
        ),
        VaultRead::Member { cache_ready: false } => {
            (Standing::Waiting, "Out of sync", Some(SYNCING.to_owned()))
        }
        VaultRead::Stopped => (
            Standing::Differs,
            "Not syncing",
            managed.then(|| STOPPED.to_owned()),
        ),
        VaultRead::NotSetUp => (
            Standing::Differs,
            "Not set up",
            managed.then(|| NOT_SET_UP.to_owned()),
        ),
        // The person's choice: what the row wants, so no change to make.
        VaultRead::TurnedOff => (Standing::InPlace, SYNC_OFF, Some(TURNED_OFF.to_owned())),
        VaultRead::PendingOff { approve } => (
            Standing::InPlace,
            SYNC_OFF,
            Some(match approve {
                Some(approve) => format!(
                    "You turned sync off; once it's on, {approve} on an admin Mac finishes the join"
                ),
                None => PENDING_OFF.to_owned(),
            }),
        ),
        // This observation does not launch the released app's automatic installation.
        VaultRead::Fleeting => (person, "Out of sync", Some(FLEETING.to_owned())),
        VaultRead::Pending {
            approve: Some(approve),
            with_password,
        } => (
            person,
            "Awaiting code",
            Some(if *with_password {
                format!("{APPROVE} {approve}, {RETYPE}")
            } else {
                format!("{APPROVE} {approve}")
            }),
        ),
        VaultRead::Pending {
            approve: None,
            with_password,
        } => (
            Standing::Waiting,
            "Awaiting code",
            Some(if *with_password {
                format!("Waits for an admin Mac, {RETYPE}")
            } else {
                APPROVAL.to_owned()
            }),
        ),
        // Default ALab admission stays automatic on the ALab LAN, including after the first
        // wait; off it the note says what lets this Mac in. The row waits, calm, either way.
        VaultRead::Asking { with_password, at } => {
            let joining = aterm_secrets::alab_join::asking_note(*at);
            (
                Standing::Waiting,
                aterm_types::macos_setup::VAULT_ASKING,
                Some(if *with_password {
                    format!("{joining}, {RETYPE}")
                } else {
                    joining.to_owned()
                }),
            )
        }
        // Nobody's to do: the vault records this Mac by itself, so the row waits, calm.
        VaultRead::Allowed => (
            Standing::Waiting,
            aterm_types::macos_setup::VAULT_ALLOWED,
            Some(ALLOWED.to_owned()),
        ),
        VaultRead::Refused => (person, WRONG_PASSWORD, None),
        // Set up now's Secrets stage joins nothing here: the way back is a person's.
        VaultRead::Dropped => (person, "Dropped", Some(DROPPED.to_owned())),
        // A server at the ALab vault's address answered with none ([`scope::NO_VAULT`]): aterm's
        // own join has none to join, so the row waits on nothing on any plan. A look nothing
        // answered ([`scope::NOT_REACHED`], every Mac off the ALab LAN) proves nothing: no join
        // happened and the Secrets stage asks again by itself, so the row is asked
        // ([`Standing::Asking`]): never in place, and never a step anyone waits on, which no
        // answer may ever end (H15, measured finding 5). A join the person typed a password for
        // waits for that vault.
        VaultRead::Unconfigured if managed && here.is_some() => (
            if typed {
                Standing::Waiting
            } else if here == Some(scope::NOT_REACHED) {
                Standing::Asking
            } else {
                Standing::InPlace
            },
            "Not joined",
            here.map(str::to_owned),
        ),
        VaultRead::Unconfigured | VaultRead::Revoked => (Standing::Differs, "Not joined", None),
        VaultRead::OtherVault => (Standing::Differs, "Other vault", None),
    };
    // A row in place wants what it reads: local cache ready, the person's opt-out (as for a grant,
    // `access::honor_opt_out`), or Not joined where a server there answered with no vault.
    let wanted = if standing == Standing::InPlace {
        now
    } else {
        subject.wanted()
    };
    Observation {
        subject,
        standing,
        now: Some(now.to_owned()),
        after_login: None,
        wanted: wanted.to_owned(),
        note,
        detail: None,
        read: Read::Unstamped,
    }
}

/// The record of rows a run changed for macOS to take up at the next login, and
/// the login session it was written in.
const AT_LOGIN: &str = "at-login.json";

/// This login: the audit session macOS gives each login (`getaudit_addr`), with
/// the boot it belongs to, so a session id reused after a restart is a new one.
fn login_session() -> Option<String> {
    #[repr(C)]
    struct TerminalId {
        port: i32,
        kind: u32,
        address: [u32; 4],
    }
    /// `struct auditinfo_addr` from `<bsm/audit.h>` (48 bytes).
    #[repr(C)]
    struct AuditInfo {
        user: u32,
        success: u32,
        failure: u32,
        terminal: TerminalId,
        session: i32,
        flags: u64,
    }
    unsafe extern "C" {
        fn getaudit_addr(info: *mut AuditInfo, length: libc::c_int) -> libc::c_int;
    }
    let mut info = std::mem::MaybeUninit::<AuditInfo>::zeroed();
    let length = libc::c_int::try_from(std::mem::size_of::<AuditInfo>()).ok()?;
    // SAFETY: `info` is live, exclusively borrowed and `length` bytes long; the call
    // reads this process's own audit record and needs no privilege.
    if unsafe { getaudit_addr(info.as_mut_ptr(), length) } != 0 {
        return None;
    }
    // SAFETY: every field is an integer, so the zeroed value the call filled is valid.
    let info = unsafe { info.assume_init() };
    let mut boot = libc::timeval {
        tv_sec: 0,
        tv_usec: 0,
    };
    let mut size = std::mem::size_of::<libc::timeval>();
    // SAFETY: the name is NUL-terminated, and `boot` and `size` are live and
    // exclusively borrowed for the call, which writes at most `size` bytes.
    let read = unsafe {
        libc::sysctlbyname(
            c"kern.boottime".as_ptr(),
            (&raw mut boot).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    (read == 0).then(|| format!("{}.{}:{}", boot.tv_sec, boot.tv_usec, info.session))
}

/// The settings `report` changed for macOS to take up at the next login or restart.
pub(in crate::macos_doctor) fn changed_at_login<'a>(
    rows: impl IntoIterator<Item = (Setting, &'a aterm_machines::model::ItemReport)>,
) -> Selection {
    let mut settings = Selection::none();
    for (setting, row) in rows {
        if row.outcome == Outcome::Changed && matches!(row.effect, Effect::Login | Effect::Restart)
        {
            settings.set(setting, true);
        }
    }
    settings
}

/// Remember `settings` as changed for the next login, with this login's id.
pub(in crate::macos_doctor) fn note_at_login(root: &Path, settings: Selection) {
    if settings.is_empty() {
        return;
    }
    let Some(session) = login_session() else {
        return;
    };
    let mut held = held_at_login_in(root, &session);
    for setting in Setting::ALL {
        if settings.contains(setting) {
            held.set(setting, true);
        }
    }
    // A lost note costs only a row that reads Will change until the next login.
    let _ = write_json(
        &root.join(AT_LOGIN),
        &json!({"profile":PROFILE,"session":session,"settings":held.bits()}),
    );
}

/// The settings held "at login" in this login session.
/// A Restore keeps this evidence: originals that read differently do not receive the
/// annotation, while a failed Restore leaves unchanged settings pending until login.
fn held_at_login(root: &Path) -> Selection {
    login_session().map_or_else(Selection::none, |session| held_at_login_in(root, &session))
}

fn held_at_login_in(root: &Path, session: &str) -> Selection {
    crate::macos_doctor::read_record_bytes(&root.join(AT_LOGIN))
        .ok()
        .and_then(|bytes| aterm_json::from_slice::<Value>(&bytes).ok())
        .filter(|value| {
            value["profile"].as_str() == Some(PROFILE) && value["session"].as_str() == Some(session)
        })
        .and_then(|value| value["settings"].as_u64())
        .and_then(|bits| u16::try_from(bits).ok())
        .and_then(Selection::from_bits)
        .unwrap_or_else(Selection::none)
}

#[cfg(test)]
#[path = "observe_tests.rs"]
mod tests;
