// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The read-only pass behind System State's rows
//! (`docs/DESIGN-system-state-2026-10-21.md` §3.5, §5): every local row, managed
//! or not, read through the same items a run applies, in `Plan` mode, which
//! writes nothing and raises no prompt. GitHub repositories, which reads the
//! network and the keychain, is read only when the caller asks for it; the
//! caller also leaves out the Messages and FaceTime rows unless that group is
//! set up or the person pressed Check again, because they read the person's
//! own account state.
//!
//! It never takes the profile lock or the setup slot, so it runs beside a pass;
//! the caller drops a reading taken while that row's own stage ran.

use super::*;
use aterm_machines::init::WorkstationGroup;
use aterm_machines::model::{Effect, Mode, Outcome};
use aterm_types::macos_setup::{Access, Observation, Standing, Subject};

/// Read `subjects` for `plan` without changing anything. `network` adds the
/// GitHub repositories row (the network and the keychain); without it that row
/// is left out. A row that cannot be read comes back Unknown with the reason in
/// its detail; nothing is guessed.
pub fn observe_initial_setup(plan: &Plan, subjects: &[Subject], network: bool) -> Vec<Observation> {
    let unread = |subject: Subject, why: &str| Observation {
        detail: Some(why.to_owned()),
        ..Observation::unknown(subject)
    };
    if let Err(why) = ensure_person_scope() {
        return subjects
            .iter()
            .map(|subject| unread(*subject, &why))
            .collect();
    }
    observe_subjects(plan, subjects, network)
}

/// [`observe_initial_setup`] past its scope check.
fn observe_subjects(plan: &Plan, subjects: &[Subject], network: bool) -> Vec<Observation> {
    let unread = |subject: Subject, why: &str| Observation {
        detail: Some(why.to_owned()),
        ..Observation::unknown(subject)
    };
    let sys = aterm_machines::port::Live::new();
    let mut out = Vec::with_capacity(subjects.len());

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
        let held = state_root()
            .ok()
            .map_or_else(Selection::none, |root| held_at_login(&root));
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
                    at_login(
                        aterm_machines::init_observe::observation(setting, &rows),
                        held.contains(setting),
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
        out.push(
            vault(plan).unwrap_or_else(|why| unread(Subject::Setting(Setting::Secrets), &why)),
        );
    }
    if network && wants(Setting::Repositories) {
        out.push(
            context()
                .and_then(|ctx| {
                    super::repositories::observe(
                        &sys,
                        &ctx,
                        super::Stage::Secrets.selected(plan).then_some(plan),
                    )
                })
                .unwrap_or_else(|why| unread(Subject::Setting(Setting::Repositories), &why)),
        );
    }

    let access: Vec<Access> = subjects
        .iter()
        .filter_map(|subject| match subject {
            Subject::Access(access) => Some(*access),
            Subject::Setting(_) => None,
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

/// A row a run changed for macOS to take up at the next login reads ✓ "at login"
/// while the value it wrote still reads in place, until this login session ends.
/// Only an in-place reading is marked: a row that reads differently since (a
/// Restore, a change by hand), or could not be read, says so; a guessed ✓ is
/// never shown.
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

/// What the Secrets vault row reads, from the local records alone.
#[derive(Clone, Debug, PartialEq, Eq)]
enum VaultRead {
    /// No vault is configured for this login.
    Unconfigured,
    /// Another vault, or another signer, than the plan names.
    OtherVault,
    /// Asked to join; an admin Mac has not approved it yet (its code, if known).
    /// `with_password` when a join password went with the request
    /// (`client::asked_with_password`): only then can a mistyped one be why it waits.
    Pending {
        code: Option<String>,
        with_password: bool,
    },
    /// Asked to join, and the person turned sync off since
    /// ([`aterm_secrets::service::subscriber_off_path`]): the subscriber that finishes an
    /// approved join does not run, by their choice. It reads Sync off in place, as
    /// [`Self::TurnedOff`] does, with what that means for the join and the row's quiet
    /// Turn On Sync.
    PendingOff { code: Option<String> },
    /// A member whose subscriber runs; `synced` once every file is written.
    Member { synced: bool },
    /// A member whose subscriber, the process that keeps `~/.secrets` in sync, is
    /// installed but has stayed down past launchd's restart window
    /// (`aterm_secrets::service::RESTART_WINDOW`). Nothing starts it again by itself: the Secrets stage
    /// installs it, so Set up now does.
    Stopped,
    /// A member with no subscriber installed and no record that the person turned
    /// it off: a `join` or `create` that installed none, a hand that removed it, or
    /// a join made from a copy of aterm that went away. The Secrets stage installs
    /// it, so Set up now does.
    NotSetUp,
    /// A member whose subscriber does not run because the person turned it off
    /// (`aterm_secrets::service::subscriber_off_path`: `aterm secrets agent off`, or
    /// a `join` or `create` with `--no-agent`): their choice, the row's wanted
    /// state (design §4), which setup leaves be.
    TurnedOff,
    /// A member whose subscriber is not running and is never installed from here:
    /// this aterm runs from a copy that goes away (a disk image, an App Translocation
    /// copy), which a launchd job must not name (`aterm_secrets::service::transient`,
    /// the Secrets stage's own rule). Only a person moves aterm.
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

/// A member's reading: whether every file is `written`, its `subscriber`, and
/// `exe`, the program this aterm runs from (`None` when unknown). A subscriber
/// that does not run is no sync to wait for: nothing starts it but the Secrets
/// stage, which installs it unless the person turned it off or this aterm runs
/// from a copy that goes away.
fn member_read(written: bool, subscriber: Subscriber, exe: Option<&Path>) -> VaultRead {
    match subscriber {
        Subscriber::Running => VaultRead::Member { synced: written },
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

/// This login's subscriber now. One read installed but not running may be
/// launchd's gap between two runs of a job that ended (`KeepAlive`), so it is
/// read again until it runs or has stayed down past launchd's restart window since it
/// was first seen down; until then the row does not flip to a change to make.
/// The first read that finds it down waits for that at most once.
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

fn vault(plan: &Plan) -> Result<Observation, String> {
    use aterm_secrets::client::{self, Membership, Paths};
    let paths = Paths::live().map_err(|error| error.to_string())?;
    let status = client::status(&paths).map_err(|error| error.to_string())?;
    let read = match (
        check::selected_vault(plan, &paths.home, &status)?,
        &status.membership,
    ) {
        (_, Membership::Unconfigured) => VaultRead::Unconfigured,
        (None, _) => VaultRead::OtherVault,
        (Some(_), Membership::Pending) if sync_turned_off(&paths.home) => VaultRead::PendingOff {
            code: status.machine.as_ref().map(|(_, code)| code.to_string()),
        },
        (Some(_), Membership::Pending) => VaultRead::Pending {
            code: status.machine.as_ref().map(|(_, code)| code.to_string()),
            with_password: client::asked_with_password(&paths),
        },
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
    // Only the ALab vault's address is probed: a plan naming another vault gets
    // no word about this network from that probe.
    let here = (plan.vault_url == aterm_types::macos_setup::DEFAULT_VAULT_URL)
        .then(scope::vault_here)
        .and_then(scope::VaultHere::note);
    Ok(vault_observation(
        &read,
        plan.selection.contains(Setting::Secrets),
        here,
    ))
}

/// The note of a member Mac whose subscriber runs and whose credentials are not
/// all written yet: what it waits on.
const SYNCING: &str = "Waits for the vault to finish syncing this Mac";

/// The note of a member Mac whose subscriber's job is installed but not running:
/// nothing restarts it by itself, and Set up now (the Secrets stage) starts it.
const STOPPED: &str = "The sync is not running; Set up now starts it";

/// The note of a member Mac with no subscriber installed and no record that the
/// person turned it off: it never syncs by itself, whatever left it so, and Set up
/// now (the Secrets stage) installs it. It names no command: several leave it so.
const NOT_SET_UP: &str = "Not set up to sync this Mac; Set up now installs it";

/// The value of a member Mac whose subscriber the person turned off, which is
/// also what the row wants then (design §4: an explicit opt-out is the row's
/// wanted state).
const SYNC_OFF: &str = "Sync off";

/// The note of a member Mac whose subscriber the person turned off
/// (`aterm_secrets::service::subscriber_off_path`): their choice, said as theirs.
/// The row's quiet Turn On Sync button turns it back on.
const TURNED_OFF: &str = "You turned sync off on this Mac";

/// The note of a Mac that asked to join and whose sync the person turned off since:
/// an admin Mac's approval lands only once sync is on again.
const PENDING_OFF: &str = "You turned sync off, so an approved join lands once it's on";

/// The note of a member Mac whose subscriber cannot run from the copy of aterm
/// that is open (a disk image): the person's step, after which the Secrets stage
/// installs it. The menu line carries it too.
const FLEETING: &str = "Move aterm to Applications and open it from there";

/// The note of a Mac that asked to join and whose approval code this Mac does not
/// hold: an admin Mac approves it, and the subscriber finishes the join.
const APPROVAL: &str = "Waits for an admin Mac to approve this Mac";

/// What a pending join that went with a password adds: its proof matched nothing
/// if the password was mistyped, and Join with the password typed again under the
/// row's disclosure sends a new one. Said once, in the row's note, never as an alert.
const RETYPE: &str = "or retype the join password";

/// The Secrets vault row in §3.3's words: Not joined, Awaiting code, Other vault,
/// Out of sync, Not syncing, Not set up, or In sync, and Sync off, which the person
/// chose and so reads in place. A member whose sync process does not run, not by
/// the person's choice, differs (Set up now starts it), and only a managed row
/// says so, since Set up now leaves a group left as is alone. `here` is what the last probe of the ALab vault heard
/// when the plan names it ([`scope::VaultHere::note`]): a row not joined then waits
/// with that note instead of asking again before the next launch. Every row that
/// waits says what on (design §3.3).
fn vault_observation(read: &VaultRead, managed: bool, here: Option<&str>) -> Observation {
    let subject = Subject::Setting(Setting::Secrets);
    let person = if managed {
        Standing::NeedsYou
    } else {
        Standing::Differs
    };
    let (standing, now, note): (Standing, &str, Option<String>) = match read {
        VaultRead::Member { synced: true } => (Standing::InPlace, "In sync", None),
        VaultRead::Member { synced: false } => {
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
        VaultRead::PendingOff { code } => (
            Standing::InPlace,
            SYNC_OFF,
            Some(match code {
                Some(code) => format!(
                    "You turned sync off; once it's on, approving {code} on an admin Mac finishes the join"
                ),
                None => PENDING_OFF.to_owned(),
            }),
        ),
        // Only a person moves aterm: nothing retries its way out of a disk image.
        VaultRead::Fleeting => (person, "Out of sync", Some(FLEETING.to_owned())),
        VaultRead::Pending {
            code: Some(code),
            with_password,
        } => (
            person,
            "Awaiting code",
            Some(if *with_password {
                format!("Approve on an admin Mac: {code}, {RETYPE}")
            } else {
                format!("Approve on an admin Mac: {code}")
            }),
        ),
        VaultRead::Pending {
            code: None,
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
        VaultRead::Unconfigured if managed && here.is_some() => {
            (Standing::Waiting, "Not joined", here.map(str::to_owned))
        }
        VaultRead::Unconfigured | VaultRead::Revoked => (Standing::Differs, "Not joined", None),
        VaultRead::OtherVault => (Standing::Differs, "Other vault", None),
    };
    // An opt-out reads in place at the person's value, which is the wanted one
    // then, as a grant the person opted out of does (`access::honor_opt_out`).
    let wanted = if matches!(read, VaultRead::TurnedOff | VaultRead::PendingOff { .. }) {
        SYNC_OFF
    } else {
        subject.wanted()
    };
    Observation {
        subject,
        standing,
        now: Some(now.to_owned()),
        wanted: wanted.to_owned(),
        note,
        detail: None,
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

/// Forget every change held for the next login (Restore put the values back).
pub(in crate::macos_doctor) fn forget_at_login(root: &Path) {
    // A record left behind marks only rows that still read as the run wrote them.
    let _ = std::fs::remove_file(root.join(AT_LOGIN));
}

/// The settings held "at login" in this login session.
fn held_at_login(root: &Path) -> Selection {
    login_session().map_or_else(Selection::none, |session| held_at_login_in(root, &session))
}

fn held_at_login_in(root: &Path, session: &str) -> Selection {
    std::fs::read(root.join(AT_LOGIN))
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
mod tests {
    use super::*;

    #[test]
    fn a_change_for_the_next_login_holds_only_in_the_session_that_made_it() {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut caps = Selection::none();
        caps.set(Setting::CapsLock, true);
        write_json(
            &root.path().join(AT_LOGIN),
            &json!({"profile":PROFILE,"session":"1.0:7","settings":caps.bits()}),
        )
        .unwrap();
        assert_eq!(held_at_login_in(root.path(), "1.0:7"), caps);
        assert_eq!(held_at_login_in(root.path(), "1.0:8"), Selection::none());

        let differs = Observation {
            standing: Standing::Differs,
            now: Some("Caps Lock".into()),
            ..Observation::unknown(Subject::Setting(Setting::CapsLock))
        };
        // A row that reads differently since the change (a Restore, a change by
        // hand) says so: only the value the run wrote, read in place, is "at login".
        assert_eq!(at_login(differs.clone(), true), differs);
        let unread = Observation::unknown(Subject::Setting(Setting::CapsLock));
        assert_eq!(at_login(unread.clone(), true), unread);
        let written = Observation {
            standing: Standing::InPlace,
            now: Some("Control".into()),
            ..Observation::unknown(Subject::Setting(Setting::CapsLock))
        };
        let held = at_login(written.clone(), true);
        assert_eq!(held.standing, Standing::AtLogin);
        assert_eq!(held.now.as_deref(), Some("Control"));
        assert_eq!(at_login(written.clone(), false), written);
    }

    #[test]
    fn this_login_has_one_readable_session_id() {
        let first = login_session();
        assert!(
            first.is_some(),
            "getaudit_addr and kern.boottime read without privilege"
        );
        assert_eq!(login_session(), first);
    }

    #[test]
    fn the_vault_row_says_what_the_records_hold_in_its_own_words() {
        let words = |read: VaultRead, here: Option<&str>| {
            let seen = vault_observation(&read, true, here);
            (seen.standing, seen.now.unwrap(), seen.note)
        };
        assert_eq!(
            words(VaultRead::Member { synced: true }, None),
            (Standing::InPlace, "In sync".into(), None)
        );
        assert_eq!(
            words(VaultRead::Member { synced: false }, None),
            (
                Standing::Waiting,
                "Out of sync".into(),
                Some(SYNCING.into())
            )
        );
        let pending = |code: Option<&str>, with_password| VaultRead::Pending {
            code: code.map(str::to_owned),
            with_password,
        };
        assert_eq!(
            words(pending(None, false), None),
            (
                Standing::Waiting,
                "Awaiting code".into(),
                Some(APPROVAL.into())
            )
        );
        assert_eq!(
            words(pending(Some("abcd-efgh"), false), None),
            (
                Standing::NeedsYou,
                "Awaiting code".into(),
                Some("Approve on an admin Mac: abcd-efgh".into())
            )
        );
        // What the probe heard is a note, and only the words it can stand behind:
        // nothing answering is not proof there is no vault.
        for heard in [scope::VaultHere::Absent, scope::VaultHere::Unreached] {
            assert_eq!(
                words(VaultRead::Unconfigured, heard.note()),
                (
                    Standing::Waiting,
                    "Not joined".into(),
                    heard.note().map(str::to_owned)
                )
            );
        }
        assert_eq!(scope::VaultHere::Absent.note(), Some(scope::NO_VAULT));
        assert_eq!(scope::VaultHere::Unreached.note(), Some(scope::NOT_REACHED));
        assert_eq!(scope::VaultHere::Found.note(), None);
        assert_eq!(scope::VaultHere::Unasked.note(), None);
        assert_eq!(
            words(VaultRead::Unconfigured, None),
            (Standing::Differs, "Not joined".into(), None)
        );
        // A row nobody manages makes no claim about this network.
        let unmanaged = vault_observation(&VaultRead::Unconfigured, false, Some(scope::NO_VAULT));
        assert_eq!(
            (unmanaged.standing, unmanaged.note),
            (Standing::Differs, None)
        );
        assert_eq!(
            words(VaultRead::OtherVault, None),
            (Standing::Differs, "Other vault".into(), None)
        );
        for read in [
            VaultRead::Unconfigured,
            VaultRead::OtherVault,
            pending(None, true),
            VaultRead::Revoked,
            VaultRead::Member { synced: false },
            VaultRead::Stopped,
            VaultRead::NotSetUp,
            VaultRead::TurnedOff,
            VaultRead::PendingOff { code: None },
            VaultRead::Fleeting,
        ] {
            let now = vault_observation(&read, true, None).now.unwrap();
            assert!(now.chars().count() <= 14, "{now}");
        }
    }

    /// Every Secrets read, managed or not, with and without a vault this network
    /// did not answer for.
    fn every_vault_observation() -> Vec<(VaultRead, Observation)> {
        let mut out = Vec::new();
        for read in [
            VaultRead::Unconfigured,
            VaultRead::OtherVault,
            VaultRead::Pending {
                code: None,
                with_password: false,
            },
            VaultRead::Pending {
                code: None,
                with_password: true,
            },
            VaultRead::Pending {
                code: Some("abcd-efgh".into()),
                with_password: false,
            },
            VaultRead::Pending {
                code: Some("abcd-efgh".into()),
                with_password: true,
            },
            VaultRead::Member { synced: false },
            VaultRead::Member { synced: true },
            VaultRead::Stopped,
            VaultRead::NotSetUp,
            VaultRead::TurnedOff,
            VaultRead::PendingOff { code: None },
            VaultRead::PendingOff {
                code: Some("abcd-efgh".into()),
            },
            VaultRead::Fleeting,
            VaultRead::Revoked,
        ] {
            for managed in [true, false] {
                for here in [None, scope::VaultHere::Absent.note()] {
                    out.push((read.clone(), vault_observation(&read, managed, here)));
                }
            }
        }
        out
    }

    /// EVERY VAULT ROW THAT WAITS SAYS WHAT ON (design §3.3): a sync to finish, an
    /// admin Mac's approval, or a vault this network did not answer for. The page
    /// never shows a waiting row with nothing under it.
    #[test]
    fn every_waiting_vault_row_says_what_it_waits_on() {
        let mut waiting = 0;
        for (read, seen) in every_vault_observation() {
            if seen.standing != Standing::Waiting {
                continue;
            }
            waiting += 1;
            let note = seen
                .note
                .unwrap_or_else(|| panic!("{read:?} waits on nothing"));
            assert!(note.chars().count() <= 52, "{note}");
            assert!(!note.contains('/') && !note.contains('~'), "{note}");
        }
        assert!(waiting >= 4, "the waiting rows were read: {waiting}");
    }

    /// A JOIN WAITING FOR AN ADMIN ON A MAC WHOSE SYNC THE PERSON TURNED OFF reads as
    /// their choice, as a member's Sync off does: in place at Sync off, which is what the
    /// row wants then (so the page offers Turn On Sync), with a note that says an
    /// approval lands only once sync is on. It never asks for an approval it cannot use.
    #[test]
    fn a_pending_join_with_sync_off_reads_sync_off_and_says_what_that_means() {
        for (code, note) in [
            (None, PENDING_OFF.to_owned()),
            (
                Some("abcd-efgh".to_owned()),
                "You turned sync off; once it's on, approving abcd-efgh on an admin Mac finishes the join".to_owned(),
            ),
        ] {
            for managed in [true, false] {
                let seen = vault_observation(&VaultRead::PendingOff { code: code.clone() }, managed, None);
                assert_eq!(
                    (seen.standing, seen.now.as_deref(), seen.wanted.as_str()),
                    (Standing::InPlace, Some(SYNC_OFF), SYNC_OFF)
                );
                assert_eq!(seen.note.as_deref(), Some(note.as_str()));
            }
        }
    }

    /// AN ATERM RUN FROM A DISK IMAGE SAYS TO MOVE IT (the Secrets stage's
    /// `fleeting_program_row`, in the row's words): a member whose subscriber is not
    /// running, opened from a copy that goes away, needs the person (only they move
    /// aterm), and its note, which the menu line carries too, says what to do. The same
    /// member whose subscriber runs only waits for a sync; opened from Applications,
    /// a subscriber that does not run is a change Set up now makes.
    #[test]
    fn a_vault_member_run_from_a_disk_image_says_to_move_aterm_to_applications() {
        let image = Path::new("/Volumes/aterm/aterm.app/Contents/MacOS/aterm");
        let installed = Path::new("/Applications/aterm.app/Contents/MacOS/aterm");
        for off in [Subscriber::Stopped, Subscriber::Removed] {
            assert_eq!(member_read(true, off, Some(image)), VaultRead::Fleeting);
            assert_eq!(member_read(false, off, Some(image)), VaultRead::Fleeting);
        }
        assert_eq!(
            member_read(false, Subscriber::Running, Some(image)),
            VaultRead::Member { synced: false }
        );
        assert_eq!(
            member_read(true, Subscriber::Stopped, Some(installed)),
            VaultRead::Stopped
        );
        assert_eq!(
            member_read(true, Subscriber::Stopped, None),
            VaultRead::Stopped
        );
        // The person turned it off: their choice, wherever aterm runs from.
        assert_eq!(
            member_read(true, Subscriber::TurnedOff, Some(image)),
            VaultRead::TurnedOff
        );
        assert_eq!(
            member_read(true, Subscriber::Running, Some(installed)),
            VaultRead::Member { synced: true }
        );
        let seen = vault_observation(&VaultRead::Fleeting, true, None);
        assert_eq!(
            (seen.standing, seen.now.as_deref(), seen.note.as_deref()),
            (Standing::NeedsYou, Some("Out of sync"), Some(FLEETING))
        );
        assert!(FLEETING.contains("Applications"));
        // A Secrets group left as is asks nothing of anyone.
        assert_eq!(
            vault_observation(&VaultRead::Fleeting, false, None).standing,
            Standing::Differs
        );
    }

    /// A SYNC THAT DOES NOT RUN IS A CHANGE, NOT A WAIT, UNLESS THE PERSON TURNED
    /// IT OFF: nothing starts a member's stopped or never-installed subscriber by
    /// itself, so the row differs, the headline counts it and Set up now, whose
    /// Secrets stage installs the subscriber, starts it; a subscriber with no job
    /// says it is not set up, naming no command (several leave it so). One the
    /// person turned off (`aterm secrets agent off`, a `--no-agent` join or create)
    /// is their choice: it reads Sync off in place, at the value it wants, so it is
    /// no change to make. Only a running subscriber with files still to write waits
    /// for the sync, and a group left as is gets no word of a change.
    #[test]
    fn a_vault_member_whose_sync_does_not_run_is_a_change_set_up_now_makes() {
        let installed = Some(Path::new("/Applications/aterm.app/Contents/MacOS/aterm"));
        let subject = Subject::Setting(Setting::Secrets);
        let words = |read: VaultRead, managed| {
            let seen = vault_observation(&read, managed, None);
            (seen.standing, seen.now.unwrap(), seen.note)
        };
        for written in [true, false] {
            assert_eq!(
                words(member_read(written, Subscriber::Stopped, installed), true),
                (
                    Standing::Differs,
                    "Not syncing".into(),
                    Some(STOPPED.into())
                ),
                "written {written}"
            );
            assert_eq!(
                words(member_read(written, Subscriber::Removed, installed), true),
                (
                    Standing::Differs,
                    "Not set up".into(),
                    Some(NOT_SET_UP.into())
                ),
                "written {written}"
            );
            let off = vault_observation(
                &member_read(written, Subscriber::TurnedOff, installed),
                true,
                None,
            );
            assert_eq!(
                (off.standing, off.now.as_deref(), off.note.as_deref()),
                (Standing::InPlace, Some(SYNC_OFF), Some(TURNED_OFF)),
                "written {written}"
            );
            assert_eq!(
                off.wanted, SYNC_OFF,
                "the person's choice is the wanted state"
            );
            assert_ne!(off.wanted, subject.wanted());
        }
        for note in [STOPPED, NOT_SET_UP, TURNED_OFF] {
            assert!(!note.contains("aterm secrets"), "{note}");
            assert!(note.chars().count() <= 52, "{note}");
        }
        assert_eq!(
            words(member_read(false, Subscriber::Running, installed), true),
            (
                Standing::Waiting,
                "Out of sync".into(),
                Some(SYNCING.into())
            )
        );
        for off in [Subscriber::Stopped, Subscriber::Removed] {
            let (standing, _, note) = words(member_read(true, off, installed), false);
            assert_eq!((standing, note), (Standing::Differs, None), "{off:?}");
        }
    }

    /// A MISTYPED JOIN PASSWORD HAS A WAY OUT, SAID ONCE (main's `RETYPE`): a pending
    /// join that went with a password says the password can be typed again, beside the
    /// approval code or the wait; one that went without says nothing of a password.
    #[test]
    fn a_pending_join_sent_with_a_password_says_it_can_be_retyped() {
        for (read, seen) in every_vault_observation() {
            let VaultRead::Pending { with_password, .. } = read else {
                continue;
            };
            let Some(note) = seen.note else {
                assert_eq!(seen.standing, Standing::Differs, "unmanaged");
                continue;
            };
            assert_eq!(note.contains(RETYPE), with_password, "{note}");
            assert!(!note.contains("aterm secrets"), "{note}");
        }
        let code = VaultRead::Pending {
            code: Some("abcd-efgh".into()),
            with_password: true,
        };
        assert_eq!(
            vault_observation(&code, true, None).note.as_deref(),
            Some("Approve on an admin Mac: abcd-efgh, or retype the join password")
        );
    }
}
