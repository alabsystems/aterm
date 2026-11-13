// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A configured Mac's secrets vault subscriber, mended at launch
//! (docs/DESIGN-secrets-2026-10-20.md §7), an admin Mac's vault server job whose program
//! is gone (`mend_server`), and managed ALab jobs moved onto the installed release
//! (`mend_held`, §9). Personal vault jobs retain their admin-locker compatibility policy.
//!
//! Joining is System State's Secrets vault row's. On a launch whose setup is finished, or
//! that runs none, and every [`MEND_EVERY`] after, a configured Mac's subscriber is read off
//! the event loop ([`mend`]) and installed again when its plist runs a copy that is gone or
//! does not last (from this aterm), or when this window owes the Mac one ([`OWED_RECORD`]) and
//! it has none (from this aterm) or one that stays down (keeping a lasting program it names); never over the person's off, and only while System
//! State's plan in force keeps the vault in sync ([`MendReach::wanted`]). Nothing is asked or
//! posted.

#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::path::{Path, PathBuf};

#[cfg(unix)]
use aterm_secrets::client::Paths;

/// The record beside `aterm.toml` of the Macs this window owes a subscriber: a join here, or the
/// mend's own install, asked for one and could not install it (this aterm ran from a disk image or
/// an App Translocation copy, or launchd refused).
pub(crate) const OWED_RECORD: &str = "secrets-subscriber-owed";

/// Why this Mac's subscriber is not running, so its secrets do not follow the vault.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Unsynced {
    /// This aterm runs from a copy that goes away (a disk image, an App Translocation copy, a
    /// temporary or cache directory), which a launchd job could not keep starting.
    Fleeting(String),
    /// The install failed, or the subscriber is not there: what failed.
    Failed(String),
}

impl Unsynced {
    /// One line for the log.
    pub(crate) fn why(&self) -> String {
        match self {
            Self::Fleeting(at) => format!(
                "this aterm runs from {at}, which goes away when it quits, so no subscriber was installed"
            ),
            Self::Failed(why) => why.clone(),
        }
    }
}

/// A configured Mac's subscriber, as its launch read leaves it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Subscriber {
    /// Installed (or installed now), naming a copy of aterm that lasts. Whether launchd runs it
    /// is read only on a Mac owed one; elsewhere a job that is down is System State's to show.
    Installed,
    /// Not installed, and nothing owed: turned off, or never asked for.
    Off,
    /// Owed or broken, and not mended: why.
    Missing(Unsynced),
}

/// What the launch read of a configured Mac found and did ([`mend`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Settled {
    /// A locker opened for it (or it reads a plain file).
    pub(crate) member: bool,
    /// Its subscriber.
    pub(crate) subscriber: Subscriber,
    /// Its join never finished: it asked for nothing that will ever let it in.
    pub(crate) unfinished: bool,
}

/// The subscriber's launchd plist, as the launch read finds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Plist {
    /// None there.
    Absent,
    /// There, running a file at a place that lasts, or a program the reader cannot name (a plist
    /// in another shape than `aterm secrets` writes), which is left as it is, running or not.
    Sound,
    /// There, running a program that is gone or does not last (a disk image ejected, an App
    /// Translocation copy, a temporary directory): launchd cannot start it.
    Stale(String),
    /// There, sound and in the shape `aterm secrets` writes (no `Disabled` key), on a Mac owed a
    /// subscriber, but not running past launchd's restart window: an install that wrote the plist
    /// and failed to start it.
    Down,
    /// There, sound and in the shape `aterm secrets` writes, on a Mac owed a subscriber, and
    /// launchd could not say whether it runs: why. Left as it is, and the debt kept.
    Unread(String),
}

/// A configured Mac's setup: local files only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Setup {
    /// A locker opened for it, or it reads a plain file.
    pub(crate) member: bool,
    /// Configured, and no locker has opened yet (`Membership::Pending`).
    pub(crate) pending: bool,
    /// The subscriber's plist.
    pub(crate) plist: Plist,
    /// This Mac is in [`OWED_RECORD`].
    pub(crate) owed: bool,
    /// The person turned the subscriber off (`aterm_secrets::service::subscriber_turned_off`).
    pub(crate) turned_off: bool,
}

/// This Mac's vault configuration, as the mend lane reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Configured {
    /// No vault configured: nothing to mend.
    None,
    /// Configured.
    Some(Setup),
}

/// What the launch read of a configured Mac does ([`plan`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Plan {
    /// Install the subscriber (again) now.
    pub(crate) install: bool,
    /// A sound subscriber is there, or the person turned it off: [`OWED_RECORD`]'s line for
    /// this Mac goes.
    pub(crate) paid: bool,
    /// The join never finished.
    pub(crate) unfinished: bool,
}

/// The launch read's plan (pure): the subscriber is installed again when its plist runs a program
/// that is gone or does not last, and installed when this Mac is owed one and has none or has one
/// that stays down. Nothing else says a subscriber was wanted: one gone or down for no recorded
/// reason may be a person's (an `agent off` older than its record, Login Items, `launchctl
/// disable`), so it is left to System State's Secrets rows. One the person turned off stays off,
/// and the debt is forgotten.
pub(crate) fn plan(setup: &Setup) -> Plan {
    let install = !setup.turned_off
        && match setup.plist {
            Plist::Stale(_) => true,
            Plist::Absent | Plist::Down => setup.owed,
            Plist::Sound | Plist::Unread(_) => false,
        };
    Plan {
        install,
        paid: setup.owed && (setup.plist == Plist::Sound || setup.turned_off),
        unfinished: setup.pending && setup.plist == Plist::Absent && !setup.owed,
    }
}

/// The program a launchd plist that `aterm_secrets::service::plist_text` wrote runs: the first
/// `<string>` of its `ProgramArguments` array, its XML entities read back.
pub(crate) fn plist_program(text: &str) -> Option<String> {
    const KEY: &str = "<key>ProgramArguments</key>";
    let at = text.find(KEY)?;
    let rest = text[at + KEY.len()..]
        .trim_start()
        .strip_prefix("<array>")?
        .trim_start()
        .strip_prefix("<string>")?;
    xml_text(&rest[..rest.find("</string>")?])
}

fn xml_text(raw: &str) -> Option<String> {
    const ENTITIES: [(&str, char); 5] = [
        ("&amp;", '&'),
        ("&lt;", '<'),
        ("&gt;", '>'),
        ("&quot;", '"'),
        ("&apos;", '\''),
    ];
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        let &(entity, c) = ENTITIES.iter().find(|(e, _)| tail.starts_with(e))?;
        out.push(c);
        rest = &tail[entity.len()..];
    }
    out.push_str(rest);
    Some(out)
}

/// The subscriber's plist at `plist`, judged by the program it runs. One a person switched off
/// (a `Disabled` key) is left as it is, whatever it runs.
pub(crate) fn subscriber_plist(plist: &Path) -> Plist {
    match std::fs::symlink_metadata(plist) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Plist::Absent,
        Err(_) => return Plist::Sound,
        Ok(_) => {}
    }
    let Some(program) = aterm_containment::consent::read_plist_text(plist)
        .filter(|text| !text.contains(DISABLED_KEY))
        .as_deref()
        .and_then(plist_program)
    else {
        return Plist::Sound;
    };
    let path = Path::new(&program);
    if path.is_absolute() && path.is_file() && atpkg::stub::lasting(path) {
        Plist::Sound
    } else {
        Plist::Stale(program)
    }
}

/// `program` when it is a copy that lasts (`atpkg::stub::lasting`: not a disk image, an App
/// Translocation copy, a temporary or a cache directory), which a launchd job may name; else
/// [`Unsynced::Fleeting`].
pub(crate) fn lasting_program(program: PathBuf) -> Result<PathBuf, Unsynced> {
    if atpkg::stub::lasting(&program) {
        Ok(program)
    } else {
        Err(Unsynced::Fleeting(program.display().to_string()))
    }
}

/// The program a subscriber installed now runs (`service::program_for_service`, which
/// refuses a copy that does not last): where this aterm runs when that is why
/// ([`lasting_program`]), else what failed.
#[cfg(target_os = "macos")]
fn subscriber_program() -> Result<PathBuf, Unsynced> {
    aterm_secrets::service::program_for_service().map_err(|error| {
        std::env::current_exe()
            .and_then(std::fs::canonicalize)
            .ok()
            .and_then(|exe| lasting_program(exe).err())
            .unwrap_or_else(|| Unsynced::Failed(error.to_string()))
    })
}

/// This Mac's vault setup under `paths`: `source.toml`, `state.toml` and the subscriber's plist,
/// never the network; `owed` is read by the caller ([`OWED_RECORD`]).
#[cfg(unix)]
pub(crate) fn read_configured(paths: &Paths, owed: bool) -> Result<Configured, String> {
    use aterm_secrets::client::{self, Membership};
    if client::config(paths)
        .map_err(|error| error.to_string())?
        .is_none()
    {
        return Ok(Configured::None);
    }
    let status = client::status(paths).map_err(|error| error.to_string())?;
    let plist =
        aterm_secrets::service::plist_path(&paths.home, aterm_secrets::service::Job::Subscriber);
    let plist = match subscriber_plist(&plist) {
        Plist::Sound if owed && own_plist(&plist) => match stays_down(&paths.home) {
            Ok(true) => Plist::Down,
            Ok(false) => Plist::Sound,
            Err(why) => Plist::Unread(why),
        },
        read => read,
    };
    Ok(Configured::Some(Setup {
        member: matches!(status.membership, Membership::In { .. } | Membership::Plain),
        pending: status.membership == Membership::Pending,
        plist,
        owed,
        turned_off: aterm_secrets::service::subscriber_turned_off(&paths.home),
    }))
}

/// A launchd plist's key that keeps launchd from loading it: a person's off.
const DISABLED_KEY: &str = "<key>Disabled</key>";

/// Whether `plist` is in the shape `aterm secrets` writes (its program named) with no `Disabled`
/// key: a plist a person wrote or switched off by hand is never written over.
#[cfg(unix)]
fn own_plist(plist: &Path) -> bool {
    aterm_containment::consent::read_plist_text(plist)
        .is_some_and(|text| plist_program(&text).is_some() && !text.contains(DISABLED_KEY))
}

/// Whether the subscriber does not run and stays down past launchd's restart window
/// (`aterm_secrets::service::back_within_restart_window`), or why launchd could not say.
#[cfg(unix)]
fn stays_down(home: &Path) -> Result<bool, String> {
    use aterm_secrets::service::{self, Job, State};
    let mut running = || {
        service::state(home, Job::Subscriber)
            .map(|state| matches!(state, State::Installed { pid: Some(_), .. }))
    };
    if running().map_err(|error| error.to_string())? {
        return Ok(false);
    }
    service::back_within_restart_window(
        &mut None,
        &mut std::time::Instant::now,
        &mut running,
        &mut std::thread::sleep,
    )
    .map(|back| !back)
    .map_err(|error| error.to_string())
}

/// Whether this Mac (`host`) is in [`OWED_RECORD`] beside `config_path`.
pub(crate) fn owed(config_path: Option<&Path>, host: Option<&str>) -> bool {
    match (config_path, host) {
        (Some(config), Some(host)) => {
            crate::host_record::lists(config, OWED_RECORD, host).unwrap_or(false)
        }
        _ => false,
    }
}

/// Write this Mac into [`OWED_RECORD`] (`still_owed`) or take it out.
pub(crate) fn note_owed(config_path: Option<&Path>, host: Option<&str>, still_owed: bool) {
    let (Some(config), Some(host)) = (config_path, host) else {
        return;
    };
    let noted = if still_owed {
        crate::host_record::note(
            config,
            OWED_RECORD,
            host,
            aterm_time::unix_secs().unwrap_or(0),
        )
    } else {
        crate::host_record::forget(config, OWED_RECORD, host)
    };
    if let Err(error) = noted {
        aterm_log::warn!("secrets vault: the owed-subscriber record was not updated: {error}");
    }
}

type MendGuard = dyn Fn(&mut dyn FnMut()) -> bool + Send;

/// What the mend reads and does, injected so it is tested without a home or launchd.
pub(crate) struct MendReach {
    /// Whether System State's plan in force keeps this Mac's vault in sync now: its Secrets
    /// vault group at Set up, and no person's Stop holding its run (`crate::secrets_sync_wanted`).
    /// While it does not, the mend reads and changes nothing.
    pub(crate) wanted: Box<dyn Fn() -> bool + Send>,
    /// Recheck and keep that choice current through the mend's effects after its read.
    /// `false` invokes nothing and leaves no result.
    pub(crate) while_wanted: Box<MendGuard>,
    /// This Mac's vault configuration, or why it cannot be read.
    pub(crate) configured: Box<dyn Fn() -> Result<Configured, String> + Send>,
    /// Install the subscriber again ([`plan`]'s `install`), from this aterm unless its plist
    /// names a lasting program, which it keeps: `false` when it was left off, the person
    /// having turned it off since the read.
    pub(crate) install: Box<dyn Fn() -> Result<bool, Unsynced> + Send>,
    /// Forget this Mac's [`OWED_RECORD`] line ([`plan`]'s `paid`).
    pub(crate) paid: Box<dyn Fn() + Send>,
}

/// The mend: a configured Mac's read, acted on ([`plan`]): the owed line forgotten once paid and
/// the subscriber installed when the plan says so. `None` while System State's plan in force
/// does not keep the vault in sync ([`MendReach::wanted`]), for a Mac with no vault configured,
/// or one whose configuration cannot be read (logged).
pub(crate) fn mend(reach: &MendReach) -> Option<Settled> {
    if !(reach.wanted)() {
        return None;
    }
    let setup = match (reach.configured)() {
        Ok(Configured::Some(setup)) => setup,
        Ok(Configured::None) => return None,
        Err(why) => {
            aterm_log::info!("secrets vault: the configuration could not be read: {why}");
            return None;
        }
    };
    let mut settled = None;
    let kept = (reach.while_wanted)(&mut || {
        let plan = plan(&setup);
        if plan.paid {
            (reach.paid)();
        }
        let subscriber = if plan.install {
            match &setup.plist {
                Plist::Stale(program) => aterm_log::info!(
                    "secrets vault: the subscriber runs {program}, which is gone or does not last; installing it from this aterm"
                ),
                Plist::Down => aterm_log::info!(
                    "secrets vault: the subscriber stays down; installing it again (it keeps a program it names that lasts)"
                ),
                Plist::Absent | Plist::Sound | Plist::Unread(_) => {}
            }
            match (reach.install)() {
                Ok(true) => Subscriber::Installed,
                Ok(false) => Subscriber::Off,
                Err(why) => Subscriber::Missing(why),
            }
        } else {
            match &setup.plist {
                Plist::Sound => Subscriber::Installed,
                Plist::Unread(why) if !setup.turned_off => Subscriber::Missing(Unsynced::Failed(
                    format!("launchd could not say whether the subscriber runs: {why}"),
                )),
                _ => Subscriber::Off,
            }
        };
        settled = Some(Settled {
            member: setup.member,
            subscriber,
            unfinished: plan.unfinished,
        });
    });
    kept.then_some(settled).flatten()
}

/// One line for `aterm.log` of what [`mend`] found.
pub(crate) fn settled_line(settled: &Settled) -> String {
    let subscriber = match &settled.subscriber {
        Subscriber::Installed => "installed".to_string(),
        Subscriber::Off => "off".to_string(),
        Subscriber::Missing(why) => format!("missing ({})", why.why()),
    };
    format!(
        "secrets vault: {}; subscriber {subscriber}{}",
        if settled.member { "in" } else { "not in yet" },
        if settled.unfinished {
            "; the join never finished (System State's Secrets vault row joins it)"
        } else {
            ""
        }
    )
}

/// How often the mend reads the subscriber again after the launch's read: a plist that comes to
/// run a copy that is gone, or a debt this window takes on, while aterm runs is mended within
/// this.
pub(crate) const MEND_EVERY: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// Whether the mend reads again after `settled` (the read before it, `before`): not for a copy
/// of aterm that goes away, which only a person moves, nor after an install that failed twice
/// the same way (launchd refusing a label the person disabled, say), which this launch would
/// only repeat; the next launch tries again.
pub(crate) fn mends_again(before: Option<&Settled>, settled: Option<&Settled>) -> bool {
    match settled.map(|settled| &settled.subscriber) {
        Some(Subscriber::Missing(Unsynced::Fleeting(_))) => false,
        Some(failed @ Subscriber::Missing(Unsynced::Failed(_))) => {
            before.map(|before| &before.subscriber) != Some(failed)
        }
        _ => true,
    }
}

/// Start the mend: a windowed macOS instance, off the event loop, at once and every
/// [`MEND_EVERY`], each change it finds logged once. `config_path` is `aterm.toml`'s, beside
/// which [`OWED_RECORD`] lives. Whether the thread started; never joined.
pub(crate) fn spawn_mend_lane(headless: bool, config_path: Option<PathBuf>) -> bool {
    if headless || !cfg!(target_os = "macos") {
        return false;
    }
    #[cfg(target_os = "macos")]
    {
        let reach = live_mend_reach(config_path);
        let spawned = crate::qos::spawn_lane_thread(
            "aterm-secrets-mend",
            crate::qos::Role::Background,
            move || {
                let mut before: Option<Settled> = None;
                let mut failed_moves = Vec::new();
                loop {
                    mend_held((reach.wanted)(), &mut failed_moves);
                    mend_server();
                    let settled = mend(&reach);
                    let line = settled.as_ref().map(settled_line);
                    if let Some(line) = &line
                        && before.as_ref().map(settled_line).as_ref() != Some(line)
                    {
                        aterm_log::info!("{line}");
                    }
                    if !mends_again(before.as_ref(), settled.as_ref()) {
                        return;
                    }
                    before = settled;
                    std::thread::sleep(MEND_EVERY);
                }
            },
        );
        match spawned {
            Ok(_) => true,
            Err(error) => {
                aterm_log::warn!("secrets vault: the mend did not start: {error}");
                false
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = config_path;
        false
    }
}

/// The vault server's job, mended when the program it names is gone (an `aterm.app` moved or
/// deleted, which launchd can then never start again): installed again from this aterm, where
/// this aterm lasts, with the arguments it had (`aterm_secrets::service::repair_server`). Left
/// as it is while the system LaunchDaemon `aterm machines apply` installs is there, installed or
/// running: that daemon serves this Mac's vaults on the port the job would take, so the job is
/// not installed beside it. Its own job, not the subscriber's: an admin Mac serves whatever
/// System State's plan says of its sync. Logged; nothing is asked or posted.
#[cfg(target_os = "macos")]
fn mend_server() {
    let Ok(paths) = Paths::live() else {
        return;
    };
    match aterm_secrets::service::repair_server(
        &paths.home,
        &aterm_secrets::service::program_for_service,
    ) {
        Ok(Some(line)) => {
            aterm_log::info!("secrets vault: the server's program was gone; {line}");
        }
        Ok(None) => {}
        Err(error) => {
            aterm_log::warn!("secrets vault: the server's job could not be repaired: {error}");
        }
    }
}

/// The vault's server and approver, moved off a held program (an admin's `--program`
/// snapshot) onto this installed release under the managed ALab update or admin-locker
/// compatibility policy, and only where no person pinned the job
/// (`aterm_secrets::client::mend_held`); the subscriber only while System State's plan keeps
/// the vault in sync (`subscriber`). A job whose mend failed this launch (`failed`,
/// [`held_pass`]) is not tried again until the next. A move is logged with why it moved
/// (`aterm_secrets::client::HeldMoved`); nothing is asked or posted.
#[cfg(target_os = "macos")]
fn mend_held(subscriber: bool, failed: &mut Vec<aterm_secrets::service::Job>) {
    use aterm_secrets::client::HeldMend;
    use aterm_secrets::service::Job;
    let Ok(paths) = Paths::live() else {
        return;
    };
    let jobs: &[Job] = if subscriber {
        &[Job::Server, Job::Subscriber]
    } else {
        &[Job::Server]
    };
    let mut mend = |job| {
        if job != Job::Subscriber {
            return aterm_secrets::client::mend_held(&paths, job).map_err(|e| e.to_string());
        }
        let mut result = Ok(HeldMend::NotHeld);
        crate::secrets_sync_while_wanted(&mut || {
            result = aterm_secrets::client::mend_subscriber(&paths).map_err(|e| e.to_string());
        });
        result
    };
    for (job, mended) in held_pass(jobs, failed, &mut mend) {
        match mended {
            Ok(HeldMend::Moved(line, moved)) => aterm_log::info!(
                "secrets vault: the {} job now runs this aterm ({}); {line}",
                job.label(),
                moved.why()
            ),
            Ok(HeldMend::NotHeld | HeldMend::Kept(_) | HeldMend::Unread(_)) => {}
            Err(error) => aterm_log::warn!(
                "secrets vault: the {} job's held aterm could not be moved onto this one: \
                 {error}; this launch does not try it again",
                job.label()
            ),
        }
    }
}

/// What the window's mend did with one held job (`aterm_secrets::client::mend_held`), or why
/// it failed.
#[cfg(unix)]
type HeldMended = Result<aterm_secrets::client::HeldMend, String>;

/// One pass of [`mend_held`] over `jobs`, `mend` judging and moving one: a job in `failed` is
/// left as it is, and one whose mend fails joins `failed`, so this launch never tries it again,
/// as [`mends_again`] ends the subscriber's mend after a failure it would only repeat. A move
/// that keeps failing (a bootstrap launchd keeps refusing) would otherwise boot the running job
/// out at every [`MEND_EVERY`], cutting the server's long-polls each time; the next launch tries
/// it again.
#[cfg(unix)]
fn held_pass(
    jobs: &[aterm_secrets::service::Job],
    failed: &mut Vec<aterm_secrets::service::Job>,
    mend: &mut dyn FnMut(aterm_secrets::service::Job) -> HeldMended,
) -> Vec<(aterm_secrets::service::Job, HeldMended)> {
    let mut mended = Vec::new();
    for &job in jobs {
        if failed.contains(&job) {
            continue;
        }
        let outcome = mend(job);
        if outcome.is_err() {
            failed.push(job);
        }
        mended.push((job, outcome));
    }
    mended
}

#[cfg(target_os = "macos")]
fn live_mend_reach(config_path: Option<PathBuf>) -> MendReach {
    use aterm_containment::consent::host_uuid;
    let read_config = config_path.clone();
    let install_config = config_path.clone();
    MendReach {
        wanted: Box::new(crate::secrets_sync_wanted),
        while_wanted: Box::new(crate::secrets_sync_while_wanted),
        configured: Box::new(move || {
            let paths = Paths::live().map_err(|error| error.to_string())?;
            read_configured(&paths, owed(read_config.as_deref(), host_uuid().as_deref()))
        }),
        install: Box::new(move || {
            let paths = Paths::live().map_err(|error| Unsynced::Failed(error.to_string()))?;
            install_subscriber(&paths.home, install_config.as_deref())
        }),
        paid: Box::new(move || note_owed(config_path.as_deref(), host_uuid().as_deref(), false)),
    }
}

/// The subscriber (`aterm secrets watch`), as `aterm secrets agent on` installs it: a
/// launchd user agent running this aterm's bundle executable, when that is a copy that
/// lasts ([`lasting_program`]), unless the job runs another aterm that lasts, which it keeps
/// (`aterm_secrets::service::kept_program`, a held snapshot). A repair, never a person's On
/// (`aterm_secrets::service::repair_subscriber`): an `agent off` since the read keeps it off
/// (`false`), and its record stays.
#[cfg(target_os = "macos")]
fn install_subscriber(home: &Path, config_path: Option<&Path>) -> Result<bool, Unsynced> {
    install_subscriber_with(
        &LiveSubscriber(home),
        config_path,
        aterm_containment::consent::host_uuid,
    )
}

/// The service step [`install_subscriber_with`] takes: a repair, `None` when the person turned
/// the subscriber off, its line otherwise.
#[cfg(any(target_os = "macos", test))]
trait SubscriberInstaller {
    fn repair(&self) -> Result<Option<String>, Unsynced>;
}

#[cfg(target_os = "macos")]
struct LiveSubscriber<'a>(&'a Path);

#[cfg(target_os = "macos")]
impl SubscriberInstaller for LiveSubscriber<'_> {
    fn repair(&self) -> Result<Option<String>, Unsynced> {
        let job = aterm_secrets::service::Job::Subscriber;
        let program = match aterm_secrets::service::kept_program(self.0, job) {
            Some(program) => Ok(program),
            None => subscriber_program(),
        };
        program.and_then(|program| {
            aterm_secrets::service::repair_subscriber(
                self.0,
                &program,
                &["secrets".to_string(), "watch".to_string()],
            )
            .map_err(|error| Unsynced::Failed(error.to_string()))
        })
    }
}

/// [`install_subscriber`] through `installer`: logged, and this Mac (`host`) owed one while it
/// fails ([`OWED_RECORD`]).
#[cfg(any(target_os = "macos", test))]
fn install_subscriber_with(
    installer: &impl SubscriberInstaller,
    config_path: Option<&Path>,
    host: impl FnOnce() -> Option<String>,
) -> Result<bool, Unsynced> {
    let installed = installer.repair();
    match &installed {
        Ok(Some(line)) => aterm_log::info!("secrets vault: {line}"),
        Ok(None) => aterm_log::info!("secrets vault: the subscriber was turned off; left off"),
        Err(why) => aterm_log::warn!("secrets vault: no subscriber: {}", why.why()),
    }
    note_owed(config_path, host().as_deref(), installed.is_err());
    installed.map(|line| line.is_some())
}

#[cfg(all(test, unix))]
#[path = "secrets_join/mend_tests.rs"]
mod subscriber_mend_tests;

#[cfg(test)]
#[path = "secrets_join_tests.rs"]
mod tests;
