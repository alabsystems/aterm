// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! JOINING THE ALAB SECRETS VAULT AT FIRST LAUNCH (docs/DESIGN-secrets-2026-10-20.md §7).

#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use aterm_messages::exceptions::{Entry, Family};
use aterm_messages::need::{Fingerprint, NoticeKind, Reason, Then, Why};
use aterm_messages::{Message, Severity, tags};
use aterm_secrets::SecretBytes;
#[cfg(unix)]
use aterm_secrets::client::Paths;

use crate::{App, Wake};

/// The once-per-Mac record beside `aterm.toml` of the Macs whose person chose *Don't Ask Again*: one `<host-uuid> <secs>` line per Mac, in the debugger-access launch ask's record form (`debugger_access::read_host_record`), because the config folder may sync between Macs.
pub(crate) const DECLINED_RECORD: &str = "secrets-join-declined";

/// The record beside `aterm.toml` of the Macs this window owes a subscriber: a join here asked for one and could not install it (this aterm ran from a disk image or an App Translocation copy, or launchd refused).
pub(crate) const OWED_RECORD: &str = "secrets-subscriber-owed";

/// How long one probe waits for the vault to answer.
pub(crate) const PROBE_WAIT: Duration = Duration::from_secs(5);

/// The probe lane's first pause after a failure ([`retry_after`]).
pub(crate) const RETRY_FIRST: Duration = Duration::from_secs(15);

/// The probe lane's longest pause ([`retry_after`]).
pub(crate) const RETRY_CAP: Duration = Duration::from_secs(5 * 60);

/// How long the probe lane keeps asking while the vault's name resolves to nothing.
pub(crate) const UNRESOLVED_FOR: Duration = Duration::from_secs(60 * 60);

/// How long a join waits for an admin Mac to approve this one once it has asked.
pub(crate) const JOIN_WAIT: Duration = Duration::from_secs(120);

/// How often the join worker reads, after a pending outcome, whether this Mac is in.
pub(crate) const MEMBER_RECHECK: Duration = Duration::from_secs(30);

/// How often the ask looks for its moment: the launch hand-overs' cadence.
pub(crate) const LOOK_EVERY: Duration = crate::devtools_helper::RECHECK_EVERY;

/// How long after the vault answered the ask waits behind the Full Disk Access and Developer Tools launch decisions.
pub(crate) const LAUNCH_STEPS_FOR: Duration = Duration::from_secs(10 * 60);

/// How long the sheet stays up unanswered: then it is Not Now.
pub(crate) const SHEET_FOR: Duration = Duration::from_secs(300);

/// The development seam that shows the sheet on a Mac with no ALab vault in reach (`lab instance --window`): `found` makes the probe lane answer at once and the join answer pending with a made-up code, with no network, no files and no launchd job.
pub(crate) const LOOK_SEAM: &str = "ATERM_DEBUG_SECRETS_JOIN";

/// How long the probe lane waits after its `failures`-th failure in a row: 15 s, 30 s, 60 s, 120 s, 240 s, then five minutes for as long as it keeps failing.
pub(crate) fn retry_after(failures: u32) -> Duration {
    let doublings = failures.saturating_sub(1).min(8);
    RETRY_FIRST.saturating_mul(1 << doublings).min(RETRY_CAP)
}

/// The vault's host as a person reads it (`alab-secrets.local`), from the default URL.
pub(crate) fn vault_host() -> &'static str {
    let url = aterm_secrets::ALAB_DEFAULT_URL;
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let end = rest.find([':', '/']).unwrap_or(rest.len());
    &rest[..end]
}

/// Whether the sheet's `source` (empty: the default) is the ALab vault's URL, which a bare `aterm secrets join` joins.
pub(crate) fn is_alab_source(source: &str) -> bool {
    let source = source.trim();
    source.is_empty() || source == aterm_secrets::ALAB_DEFAULT_URL
}

/// The ALab vault that answered the probe: the version it serves and its signer's fingerprint (public; for the log).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Found {
    pub(crate) seq: u64,
    pub(crate) fingerprint: String,
}

/// The sheet's three answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Choice {
    /// Join, with the password typed (or none, to be approved by code).
    Join,
    /// Not now: asked again at the next launch.
    NotNow,
    /// Don't ask again on this Mac ([`DECLINED_RECORD`]).
    Never,
}

/// `NSAlertFirstButtonReturn` (`NSAlert.h`): the first button added.
const FIRST_BUTTON: isize = 1000;
/// `NSAlertThirdButtonReturn`: the third.
const THIRD_BUTTON: isize = 1002;

impl Choice {
    /// The answer an `NSAlert` response names: its first, second and third buttons are Join, Not Now and Don't Ask Again.
    pub(crate) const fn of_response(response: isize) -> Self {
        match response {
            FIRST_BUTTON => Self::Join,
            THIRD_BUTTON => Self::Never,
            _ => Self::NotNow,
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Join => "join",
            Self::NotNow => "not_now",
            Self::Never => "never",
        }
    }
}

/// Why this Mac's subscriber is not running, so its secrets do not follow the vault.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Unsynced {
    /// This aterm runs from a copy that goes away (a disk image, an App Translocation copy, a temporary or cache directory): a launchd job naming it would stop starting.
    Fleeting(String),
    /// The install failed, or the subscriber is not there: what failed.
    Failed(String),
}

impl Unsynced {
    /// One line for the log and the digest line's detail.
    pub(crate) fn why(&self) -> String {
        match self {
            Self::Fleeting(at) => format!(
                "this aterm runs from {at}, which goes away when it quits, so no subscriber was installed"
            ),
            Self::Failed(why) => why.clone(),
        }
    }
}

/// How a join ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum JoinEnd {
    /// In, and synced: how many secrets this Mac holds; why the subscriber is not running, if it is not.
    Joined {
        names: usize,
        subscriber: Option<Unsynced>,
    },
    /// Asked; an admin Mac must approve `code` (this Mac asked as `host`); `with_password` when a join password went with the request, so a mistyped one may be why it waits.
    Pending {
        code: String,
        host: String,
        with_password: bool,
        subscriber: Option<Unsynced>,
    },
    /// The join failed: what failed, never a value.
    Failed { why: String },
}

/// A configured Mac's subscriber, as its launch read leaves it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Subscriber {
    /// Installed (or installed now), running a copy of aterm that lasts.
    Running,
    /// Not installed, and nothing owed: turned off, or never asked for.
    Off,
    /// Owed or broken, and not mended: why.
    Missing(Unsynced),
}

/// What the launch read of a configured Mac found and did ([`plan`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Settled {
    /// A locker opened for it (or it reads a plain file).
    pub(crate) member: bool,
    /// Its subscriber.
    pub(crate) subscriber: Subscriber,
    /// Its join never finished: it asked for nothing that will ever let it in.
    pub(crate) unfinished: bool,
}

/// What the probe lane, the sheet and the join worker tell the window (`Wake::SecretsVault`).
#[derive(Debug)]
pub(crate) enum VaultWake {
    /// The probe lane: the ALab vault answers on this LAN; `unfinished` when this Mac is configured for it already, by a join that never finished.
    Found { found: Found, unfinished: bool },
    /// The probe lane, on a Mac with a vault configured: what its read found and did.
    Configured(Settled),
    /// The sheet was answered (its completion handler).
    Answered { id: u64, choice: Choice },
    /// A join ended (the join worker).
    Joined(JoinEnd),
}

/// Where the lane's threads post: the window's queue.
pub(crate) type Post = Arc<dyn Fn(VaultWake) + Send + Sync>;

/// The subscriber's launchd plist, as the launch read finds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Plist {
    /// None there.
    Absent,
    /// There, running a program that is a file at a place that lasts — or one the reader cannot name (a plist in another shape than `aterm secrets` writes), which is left as it is.
    Sound,
    /// There, running a program that is gone or does not last (a disk image ejected, an App Translocation copy, a temporary directory): launchd cannot start it.
    Stale(String),
}

/// A configured Mac's setup: local files only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Setup {
    /// The source is the ALab default (`aterm_secrets::ALAB_DEFAULT_URL`).
    pub(crate) alab: bool,
    /// A locker opened for it, or it reads a plain file.
    pub(crate) member: bool,
    /// Configured, and no locker has opened yet (`Membership::Pending`).
    pub(crate) pending: bool,
    /// The subscriber's plist.
    pub(crate) plist: Plist,
    /// This Mac is in [`OWED_RECORD`].
    pub(crate) owed: bool,
}

/// This Mac's vault configuration, as the probe lane reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Configured {
    /// No vault configured: the lane asks.
    None,
    /// Configured.
    Some(Setup),
}

/// What the launch read of a configured Mac does ([`plan`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Plan {
    /// Install the subscriber (again) now.
    pub(crate) install: bool,
    /// A sound subscriber is there: [`OWED_RECORD`]'s line for this Mac goes.
    pub(crate) paid: bool,
    /// The join never finished.
    pub(crate) unfinished: bool,
}

/// THE LAUNCH READ'S PLAN (pure): the subscriber is installed again when its plist runs a program that is gone or does not last, and installed when this Mac is owed one and has none; a plist with no line owed for it is never touched (its person may have turned it off).
pub(crate) fn plan(setup: &Setup) -> Plan {
    let install = match setup.plist {
        Plist::Stale(_) => true,
        Plist::Absent => setup.owed,
        Plist::Sound => false,
    };
    Plan {
        install,
        paid: setup.owed && setup.plist == Plist::Sound,
        unfinished: setup.pending && setup.plist == Plist::Absent && !setup.owed,
    }
}

/// Whether the lane asks this configured Mac anyway: its join of the ALab vault never finished, so the sheet is the quickest way in (Join reruns it, password and all).
pub(crate) fn asks_again(setup: &Setup) -> bool {
    setup.alab && plan(setup).unfinished
}

/// The program a launchd plist that `aterm_secrets::service::plist_text` wrote runs: the first `<string>` of its `ProgramArguments` array, its XML entities read back.
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

/// XML character data read back: the five predefined entities `plist_text` writes, and
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

/// The subscriber's plist at `plist`, judged by the program it runs.
pub(crate) fn subscriber_plist(plist: &Path) -> Plist {
    match std::fs::symlink_metadata(plist) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Plist::Absent,
        Err(_) => return Plist::Sound,
        Ok(_) => {}
    }
    let Some(program) = aterm_containment::consent::read_plist_text(plist)
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

/// The program a subscriber installed now runs, when it is a copy that lasts (`atpkg::stub::lasting`: not a disk image, an App Translocation copy, a temporary or a cache directory), which a launchd job may name; else where this aterm runs.
pub(crate) fn lasting_program(program: PathBuf) -> Result<PathBuf, Unsynced> {
    if atpkg::stub::lasting(&program) {
        Ok(program)
    } else {
        Err(Unsynced::Fleeting(program.display().to_string()))
    }
}

/// The program a subscriber installed now runs (`service::program_for_service`, which
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

/// This Mac's vault setup under `paths`: `source.toml`, `state.toml` and the subscriber's plist, never the network; `owed` read by the caller ([`OWED_RECORD`]).
#[cfg(unix)]
pub(crate) fn read_configured(paths: &Paths, owed: bool) -> Result<Configured, String> {
    use aterm_secrets::client::{self, Membership};
    let Some(config) = client::config(paths).map_err(|error| error.to_string())? else {
        return Ok(Configured::None);
    };
    let status = client::status(paths).map_err(|error| error.to_string())?;
    let plist =
        aterm_secrets::service::plist_path(&paths.home, aterm_secrets::service::Job::Subscriber);
    Ok(Configured::Some(Setup {
        alab: config.source
            == aterm_secrets::transport::Source::Url(aterm_secrets::ALAB_DEFAULT_URL.to_string()),
        member: matches!(status.membership, Membership::In { .. } | Membership::Plain),
        pending: status.membership == Membership::Pending,
        plist: subscriber_plist(&plist),
        owed,
    }))
}

/// Whether this Mac (`host`) is in [`OWED_RECORD`] beside `config_path`.
pub(crate) fn owed(config_path: Option<&Path>, host: Option<&str>) -> bool {
    match (config_path, host) {
        (Some(config), Some(host)) => {
            crate::debugger_access::host_record_lists(config, OWED_RECORD, host).unwrap_or(false)
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
        crate::debugger_access::note_host_record(
            config,
            OWED_RECORD,
            host,
            aterm_time::unix_secs().unwrap_or(0),
        )
    } else {
        crate::debugger_access::forget_host_record(config, OWED_RECORD, host)
    };
    if let Err(error) = noted {
        aterm_log::warn!("secrets vault: the owed-subscriber record was not updated: {error}");
    }
}

/// A probe that failed: what failed, and whether the vault's name resolves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Missed {
    pub(crate) why: String,
    pub(crate) resolved: bool,
}

/// One probe's answer (`client::probe_default`) as the lane reads it: the ALab vault found; no ALab vault there, either because nothing is served at its URL or because what is served there is not it (another vault, or something that is no vault server, on that name and port), which asking again every five minutes for the life of the process would not change; or a failure to reach it, a miss, for which alone `resolves` is asked (a lookup of its own).
#[cfg(unix)]
pub(crate) fn heard(
    probed: aterm_secrets::Result<aterm_secrets::client::ProbeOutcome>,
    resolves: impl FnOnce() -> bool,
) -> Result<Option<Found>, Missed> {
    use aterm_secrets::client::ProbeOutcome;
    match probed {
        Ok(ProbeOutcome::Found(probed)) => Ok(Some(Found {
            seq: probed.seq,
            fingerprint: probed.fingerprint,
        })),
        Ok(ProbeOutcome::Absent) => Ok(None),
        Ok(ProbeOutcome::NotThisVault(why)) => {
            aterm_log::info!(
                "secrets vault: what answers at {} is not the ALab vault ({why})",
                vault_host()
            );
            Ok(None)
        }
        Err(error) => Err(Missed {
            why: error.to_string(),
            resolved: resolves(),
        }),
    }
}

/// What the probe lane reads and does, injected so its schedule is tested without a LAN, a home, launchd or a clock.
pub(crate) struct ProbeReach {
    /// This Mac's vault configuration, or why it cannot be read.
    pub(crate) configured: Box<dyn Fn() -> Result<Configured, String> + Send>,
    /// Whether this Mac is in [`DECLINED_RECORD`].
    pub(crate) declined: Box<dyn Fn() -> bool + Send>,
    /// One probe of the ALab vault, bounded by the wait it is given: `Ok(None)` when no ALab vault answers there, whatever else does ([`heard`]).
    pub(crate) probe: Box<dyn Fn(Duration) -> Result<Option<Found>, Missed> + Send>,
    /// The pause between tries.
    pub(crate) sleep: Box<dyn Fn(Duration) + Send>,
    /// Install the subscriber from this aterm ([`plan`]'s `install`).
    pub(crate) install: Box<dyn Fn() -> Result<(), Unsynced> + Send>,
    /// Forget this Mac's [`OWED_RECORD`] line ([`plan`]'s `paid`).
    pub(crate) paid: Box<dyn Fn() + Send>,
}

/// How the probe lane ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LaneEnd {
    /// The configuration could not be read: nothing is asked.
    Unreadable(String),
    /// A vault is configured (at the start, or since: a person joined from a terminal).
    Configured,
    /// This Mac's person chose Don't Ask Again.
    Declined,
    /// The ALab vault answered; the window asks at its moment.
    Found,
    /// No ALab vault answers on this LAN: nothing is served at its URL, or what is served there is not it.
    NoVault,
    /// The vault's name resolved to nothing for [`UNRESOLVED_FOR`].
    Unresolved,
}

/// THE PROBE LANE: read the configuration (and settle a configured Mac's subscriber, [`settle`]), then the declined record, then probe until the ALab vault answers or says it is not there, pausing [`retry_after`] between failures and re-reading the configuration after each pause.
pub(crate) fn run_probe_lane(reach: &ProbeReach, post: &dyn Fn(VaultWake)) -> LaneEnd {
    let unfinished = match (reach.configured)() {
        Err(why) => return LaneEnd::Unreadable(why),
        Ok(Configured::None) => false,
        Ok(Configured::Some(setup)) => {
            if !settle(reach, &setup, post) {
                return LaneEnd::Configured;
            }
            true
        }
    };
    if (reach.declined)() {
        return LaneEnd::Declined;
    }
    let mut failures = 0u32;
    let mut unresolved_for = Duration::ZERO;
    let mut told: Option<String> = None;
    loop {
        match (reach.probe)(PROBE_WAIT) {
            Ok(Some(found)) => {
                post(VaultWake::Found { found, unfinished });
                return LaneEnd::Found;
            }
            Ok(None) => return LaneEnd::NoVault,
            Err(missed) => {
                failures = failures.saturating_add(1);
                let pause = retry_after(failures);
                if missed.resolved {
                    unresolved_for = Duration::ZERO;
                } else if unresolved_for >= UNRESOLVED_FOR {
                    return LaneEnd::Unresolved;
                } else {
                    unresolved_for = unresolved_for.saturating_add(pause);
                }
                if told.as_deref() != Some(missed.why.as_str()) {
                    aterm_log::info!(
                        "secrets vault: the ALab vault did not answer ({}); asking again every {}s at most",
                        missed.why,
                        RETRY_CAP.as_secs()
                    );
                    told = Some(missed.why);
                }
                (reach.sleep)(pause);
                match (reach.configured)() {
                    Err(why) => return LaneEnd::Unreadable(why),
                    Ok(Configured::None) => {}
                    Ok(Configured::Some(setup)) if asks_again(&setup) => {}
                    Ok(Configured::Some(setup)) => {
                        settle(reach, &setup, post);
                        return LaneEnd::Configured;
                    }
                }
            }
        }
    }
}

/// A configured Mac's read, acted on ([`plan`]): the owed line forgotten once paid, the
fn settle(reach: &ProbeReach, setup: &Setup, post: &dyn Fn(VaultWake)) -> bool {
    let plan = plan(setup);
    if plan.paid {
        (reach.paid)();
    }
    let subscriber = if plan.install {
        if let Plist::Stale(program) = &setup.plist {
            aterm_log::info!(
                "secrets vault: the subscriber runs {program}, which is gone or does not last; installing it from this aterm"
            );
        }
        match (reach.install)() {
            Ok(()) => Subscriber::Running,
            Err(why) => Subscriber::Missing(why),
        }
    } else if setup.plist == Plist::Sound {
        Subscriber::Running
    } else {
        Subscriber::Off
    };
    post(VaultWake::Configured(Settled {
        member: setup.member,
        subscriber,
        unfinished: plan.unfinished,
    }));
    asks_again(setup)
}

/// Whether this Mac (`host`, the kernel's host UUID) is in the declined record beside `config_path`.
pub(crate) fn declined(config_path: &Path, host: Option<&str>) -> bool {
    host.is_some_and(|host| {
        crate::debugger_access::read_host_record(config_path, DECLINED_RECORD, host)
    })
}

/// Write this Mac into the declined record (durably: the answer is the record).
pub(crate) fn note_declined(
    config_path: &Path,
    host: Option<&str>,
    now: u64,
) -> std::io::Result<()> {
    let Some(host) = host else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "the kernel names no host to record",
        ));
    };
    crate::debugger_access::note_host_record(config_path, DECLINED_RECORD, host, now)
}

/// Start the probe lane: a windowed macOS instance, off the event loop, posting through `post` (which holds what it posts until the loop exists).
pub(crate) fn spawn_probe_lane(headless: bool, config_path: Option<PathBuf>, post: Post) {
    if headless || !cfg!(target_os = "macos") {
        return;
    }
    #[cfg(target_os = "macos")]
    {
        let reach = if aterm_types::dev_seam!(LOOK_SEAM).is_some() {
            look_probe_reach()
        } else {
            live_probe_reach(config_path)
        };
        let spawned = crate::qos::spawn_lane_thread(
            "aterm-secrets-probe",
            crate::qos::Role::Background,
            move || {
                let end = run_probe_lane(&reach, &*post);
                aterm_log::info!("secrets vault: probe lane ended: {end:?}");
            },
        );
        if let Err(error) = spawned {
            aterm_log::warn!("secrets vault: the probe lane did not start: {error}");
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (config_path, post);
}

#[cfg(target_os = "macos")]
fn live_probe_reach(config_path: Option<PathBuf>) -> ProbeReach {
    use aterm_containment::consent::host_uuid;
    let read_config = config_path.clone();
    let declined_config = config_path.clone();
    let install_config = config_path.clone();
    ProbeReach {
        configured: Box::new(move || {
            live_configured(read_config.as_deref(), host_uuid().as_deref())
        }),
        declined: Box::new(move || {
            declined_config
                .as_deref()
                .is_some_and(|config| declined(config, host_uuid().as_deref()))
        }),
        probe: Box::new(|wait| heard(aterm_secrets::client::probe_default(wait), vault_resolves)),
        sleep: Box::new(std::thread::sleep),
        install: Box::new(move || {
            let paths = Paths::live().map_err(|error| Unsynced::Failed(error.to_string()))?;
            install_subscriber(&paths.home, install_config.as_deref())
        }),
        paid: Box::new(move || note_owed(config_path.as_deref(), host_uuid().as_deref(), false)),
    }
}

/// Whether the ALab vault's name resolves to an address now (the resolver the probe's
#[cfg(target_os = "macos")]
fn vault_resolves() -> bool {
    use std::net::ToSocketAddrs as _;
    (vault_host(), aterm_secrets::DEFAULT_PORT)
        .to_socket_addrs()
        .is_ok_and(|mut addrs| addrs.next().is_some())
}

/// [`LOOK_SEAM`]'s probe lane: unconfigured, not declined, and found at once.
#[cfg(target_os = "macos")]
fn look_probe_reach() -> ProbeReach {
    ProbeReach {
        configured: Box::new(|| Ok(Configured::None)),
        declined: Box::new(|| false),
        probe: Box::new(|_| {
            Ok(Some(Found {
                seq: 1,
                fingerprint: "development seam".to_string(),
            }))
        }),
        sleep: Box::new(|_| {}),
        install: Box::new(|| Ok(())),
        paid: Box::new(|| {}),
    }
}

/// This Mac's vault configuration under the live home ([`read_configured`]), with this
#[cfg(target_os = "macos")]
fn live_configured(config_path: Option<&Path>, host: Option<&str>) -> Result<Configured, String> {
    let paths = Paths::live().map_err(|error| error.to_string())?;
    read_configured(&paths, owed(config_path, host))
}

/// Whether a vault is configured under the live home now: `source.toml` alone, the one
#[cfg(target_os = "macos")]
fn live_configured_now() -> bool {
    !matches!(
        Paths::live().and_then(|paths| aterm_secrets::client::config(&paths)),
        Ok(None)
    )
}

/// A join to run: the typed password (wiped when the job drops; `None` to be approved by code), where its end goes, and `aterm.toml`'s path ([`OWED_RECORD`] lives beside it).
pub(crate) struct JoinJob {
    pub(crate) source: String,
    pub(crate) password: Option<SecretBytes>,
    pub(crate) post: Post,
    pub(crate) config: Option<PathBuf>,
}

/// How the sheet and the join reach the platform: the live arms on a windowed macOS instance, the inert ones everywhere else (the test App among them).
#[derive(Clone, Copy)]
pub(crate) struct JoinReach {
    /// Whether a sheet may be raised: a windowed macOS instance.
    pub(crate) sheet: bool,
    /// Start `job` off the event loop; `false` when it could not start (its password is wiped either way).
    pub(crate) run: fn(JoinJob) -> bool,
    /// Whether a vault is configured now ([`live_configured_now`]), read just before the sheet is raised on a Mac that had none.
    pub(crate) configured_now: fn() -> bool,
}

impl std::fmt::Debug for JoinReach {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JoinReach")
            .field("sheet", &self.sheet)
            .finish_non_exhaustive()
    }
}

impl JoinReach {
    /// Nothing is raised and no join runs.
    pub(crate) const fn inert() -> Self {
        Self {
            sheet: false,
            run: inert_join,
            configured_now: always,
        }
    }

    /// The live arms on macOS (or [`LOOK_SEAM`]'s join, when set); inert elsewhere.
    pub(crate) fn live() -> Self {
        #[cfg(target_os = "macos")]
        {
            if aterm_types::dev_seam!(LOOK_SEAM).is_some() {
                Self {
                    sheet: true,
                    run: look_join,
                    configured_now: never,
                }
            } else {
                Self {
                    sheet: true,
                    run: spawn_join,
                    configured_now: live_configured_now,
                }
            }
        }
        #[cfg(not(target_os = "macos"))]
        Self::inert()
    }
}

fn inert_join(job: JoinJob) -> bool {
    drop(job);
    false
}

/// A `configured_now` arm: the inert one, so nothing is ever asked.
const fn always() -> bool {
    true
}

/// A `configured_now` arm: [`LOOK_SEAM`]'s, and the tests', so the ask goes on.
const fn never() -> bool {
    false
}

/// The live join, on a background thread: [`join_vault`] of the sheet's source, then — when
#[cfg(target_os = "macos")]
fn spawn_join(job: JoinJob) -> bool {
    crate::qos::spawn_lane_thread(
        "aterm-secrets-join",
        crate::qos::Role::Background,
        move || {
            let JoinJob {
                source,
                password,
                post,
                config,
            } = job;
            let end = match Paths::live() {
                Ok(paths) => join_vault(&JoinPlan {
                    paths: &paths,
                    source: &source,
                    wait: JOIN_WAIT,
                    password: &|| password.clone(),
                    install: &|| install_subscriber(&paths.home, config.as_deref()),
                }),
                Err(error) => JoinEnd::Failed {
                    why: error.to_string(),
                },
            };
            drop(password);
            let pending = matches!(end, JoinEnd::Pending { .. });
            post(VaultWake::Joined(end));
            if pending {
                watch_membership(config.as_deref(), &*post);
            }
        },
    )
    .is_ok()
}

/// [`LOOK_SEAM`]'s join: pending at once with a made-up code, touching nothing.
#[cfg(target_os = "macos")]
fn look_join(job: JoinJob) -> bool {
    let JoinJob { password, post, .. } = job;
    let with_password = password.is_some();
    drop(password);
    post(VaultWake::Joined(JoinEnd::Pending {
        code: "SEAM-CODE".to_string(),
        host: "development-seam".to_string(),
        with_password,
        subscriber: None,
    }));
    true
}

/// One join, its reach injected: where the vault state lives, the source (empty: the ALab default, pinned by `ALAB_SIGNER`, so no fingerprint is confirmed), how long to wait for the approval, the typed password, and the subscriber's install.
#[cfg(unix)]
pub(crate) struct JoinPlan<'a> {
    pub(crate) paths: &'a Paths,
    pub(crate) source: &'a str,
    pub(crate) wait: Duration,
    pub(crate) password: &'a dyn Fn() -> Option<SecretBytes>,
    pub(crate) install: &'a dyn Fn() -> Result<(), Unsynced>,
}

/// `client::join`, not interactive, the subscriber installed once the request is queued — and after the join when that never ran (a Mac that was in already).
#[cfg(unix)]
pub(crate) fn join_vault(plan: &JoinPlan<'_>) -> JoinEnd {
    use aterm_secrets::client::{self, JoinOptions, JoinOutcome};
    let installed: std::cell::RefCell<Option<Result<(), Unsynced>>> = std::cell::RefCell::new(None);
    let install = || {
        if installed.borrow().is_none() {
            let result = (plan.install)();
            *installed.borrow_mut() = Some(result);
        }
    };
    let say = |line: &str| aterm_log::info!("secrets join: {line}");
    let outcome = client::join(
        plan.paths,
        &JoinOptions {
            source: plan.source.to_string(),
            host: None,
            interactive: false,
            wait: plan.wait,
            confirm: &|_| false,
            password: plan.password,
            requested: &install,
            say: &say,
        },
    );
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            return JoinEnd::Failed {
                why: error.to_string(),
            };
        }
    };
    install();
    let subscriber = installed.into_inner().and_then(Result::err);
    match outcome {
        JoinOutcome::Joined { names, .. } | JoinOutcome::Plain { names } => JoinEnd::Joined {
            names: names.len(),
            subscriber,
        },
        JoinOutcome::Pending { code, host } => JoinEnd::Pending {
            code,
            host,
            with_password: client::asked_with_password(plan.paths),
            subscriber,
        },
    }
}

/// The subscriber (`aterm secrets watch`), as `aterm secrets agent on` installs it: a
#[cfg(target_os = "macos")]
fn install_subscriber(home: &Path, config_path: Option<&Path>) -> Result<(), Unsynced> {
    use aterm_secrets::service::{self, Job};
    let installed = subscriber_program().and_then(|program| {
        service::install(
            home,
            Job::Subscriber,
            &program,
            &["secrets".to_string(), "watch".to_string()],
        )
        .map_err(|error| Unsynced::Failed(error.to_string()))
    });
    match &installed {
        Ok(line) => aterm_log::info!("secrets join: {line}"),
        Err(why) => aterm_log::warn!("secrets join: no subscriber: {}", why.why()),
    }
    note_owed(
        config_path,
        aterm_containment::consent::host_uuid().as_deref(),
        installed.is_err(),
    );
    installed.map(|_| ())
}

/// Why a member's subscriber is not running, from its plist: `None` when it is sound; `running` names where a new one would run from, so a copy that does not last is said.
pub(crate) fn subscriber_gap(
    plist: &Plist,
    running: impl FnOnce() -> Result<PathBuf, Unsynced>,
) -> Option<Unsynced> {
    match plist {
        Plist::Sound => None,
        Plist::Stale(program) => Some(Unsynced::Failed(format!(
            "the subscriber runs {program}, which is gone or does not last"
        ))),
        Plist::Absent => Some(
            running()
                .err()
                .unwrap_or_else(|| Unsynced::Failed("the subscriber is not installed".to_string())),
        ),
    }
}

/// After a pending join: read every [`MEMBER_RECHECK`] whether this Mac is in, and post
#[cfg(target_os = "macos")]
fn watch_membership(config_path: Option<&Path>, post: &dyn Fn(VaultWake)) {
    let host = aterm_containment::consent::host_uuid();
    loop {
        std::thread::sleep(MEMBER_RECHECK);
        match live_configured(config_path, host.as_deref()) {
            Ok(Configured::Some(setup)) if !setup.member => {}
            Ok(Configured::Some(setup)) => {
                let names = Paths::live()
                    .and_then(|paths| aterm_secrets::client::status(&paths))
                    .map_or(0, |status| status.names.len());
                let subscriber = subscriber_gap(&setup.plist, subscriber_program);
                post(VaultWake::Joined(JoinEnd::Joined { names, subscriber }));
                return;
            }
            Ok(Configured::None) | Err(_) => return,
        }
    }
}

/// What the ask is judged on at a look.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AskFacts {
    /// The Full Disk Access or Developer Tools launch decision is still to come (and [`LAUNCH_STEPS_FOR`] has not passed since the vault answered).
    pub(crate) launch_steps: bool,
    /// The debugger-access launch ask is queued, or a debugger change is under way.
    pub(crate) debugger: bool,
    /// A hand-over waits, or aterm's helper panel is up.
    pub(crate) page: bool,
    /// The folder warm-up is walking (a macOS folder dialog may be up).
    pub(crate) warmup: bool,
    /// A password sheet is up (`sudo`'s), or the focused window shows a sheet.
    pub(crate) other_sheet: bool,
    /// The quiet moment (`App::devtools_moment_is_quiet`): an aterm window focused, the owner present but hands off, this process settled.
    pub(crate) quiet: bool,
    /// The owner declined a Security gesture recently.
    pub(crate) owner_quiet: bool,
}

/// What the ask does now ([`admit`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ask {
    /// Raise the sheet.
    Raise,
    /// Look again after [`LOOK_EVERY`].
    Wait(Wait),
}

/// Why the ask looks again later.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Wait {
    BehindLaunchSteps,
    BehindDebuggerAsk,
    BehindPage,
    BehindWarmup,
    BehindSheet,
    NotQuiet,
    OwnerQuiet,
}

impl Wait {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::BehindLaunchSteps => "behind_launch_steps",
            Self::BehindDebuggerAsk => "behind_debugger_ask",
            Self::BehindPage => "behind_page",
            Self::BehindWarmup => "behind_warmup",
            Self::BehindSheet => "behind_sheet",
            Self::NotQuiet => "not_quiet",
            Self::OwnerQuiet => "owner_quiet",
        }
    }
}

/// THE ASK'S ADMISSION.
pub(crate) fn admit(facts: &AskFacts) -> Ask {
    let waits = [
        (facts.launch_steps, Wait::BehindLaunchSteps),
        (facts.debugger, Wait::BehindDebuggerAsk),
        (facts.page, Wait::BehindPage),
        (facts.warmup, Wait::BehindWarmup),
        (facts.other_sheet, Wait::BehindSheet),
        (!facts.quiet, Wait::NotQuiet),
        (facts.owner_quiet, Wait::OwnerQuiet),
    ];
    waits
        .iter()
        .find(|(holds, _)| *holds)
        .map_or(Ask::Raise, |&(_, why)| Ask::Wait(why))
}

/// The App's lane: the vault that answered, the ask's next look, the sheet that is up and the join that runs.
#[derive(Default)]
pub(crate) struct Lane {
    reach: Option<JoinReach>,
    /// The vault the probe lane found, and when.
    found: Option<(Found, Instant)>,
    /// The Mac the vault was found for is configured already (an unfinished join): the
    unfinished: bool,
    /// When the next look is due; `None`: at the next park.
    next_look: Option<Instant>,
    /// What the last look waited on.
    waiting: Option<Wait>,
    /// The sheet that is up, by id, and when it went up.
    sheet: Option<(u64, Instant)>,
    /// The last sheet id minted.
    last_id: u64,
    /// A join is running.
    joining: bool,
    /// The ask is over for this process (answered, or moot).
    over: bool,
    /// The native sheet, while it is up.
    #[cfg(target_os = "macos")]
    pub(crate) native: Option<crate::secrets_sheet::SecretsSheet>,
}

impl std::fmt::Debug for Lane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lane")
            .field("reach", &self.reach)
            .field("found", &self.found)
            .field("unfinished", &self.unfinished)
            .field("waiting", &self.waiting)
            .field("sheet", &self.sheet)
            .field("joining", &self.joining)
            .field("over", &self.over)
            .finish_non_exhaustive()
    }
}

impl Lane {
    /// The lane of an instance: the live arms when `windowed` (on macOS), inert otherwise.
    pub(crate) fn new(windowed: bool) -> Self {
        Self::with_reach(if windowed {
            JoinReach::live()
        } else {
            JoinReach::inert()
        })
    }

    /// The lane with injected arms.
    pub(crate) fn with_reach(reach: JoinReach) -> Self {
        Self {
            reach: Some(reach),
            ..Self::default()
        }
    }

    fn reach(&self) -> JoinReach {
        self.reach.unwrap_or(JoinReach::inert())
    }

    /// The probe lane found the vault (for a Mac configured already by an unfinished join when `unfinished`): the ask looks at the next park.
    pub(crate) fn on_found(&mut self, found: Found, unfinished: bool, now: Instant) {
        if !self.reach().sheet || self.over {
            return;
        }
        self.found = Some((found, now));
        self.unfinished = unfinished;
        self.next_look = None;
    }

    /// Whether a look is due at `now`.
    pub(crate) fn due(&self, now: Instant) -> bool {
        self.found.is_some()
            && !self.over
            && self.sheet.is_none()
            && !self.joining
            && self.next_look.is_none_or(|at| now >= at)
    }

    /// Whether the launch steps may still hold the ask at `now` ([`LAUNCH_STEPS_FOR`]).
    pub(crate) fn launch_steps_may_hold(&self, now: Instant) -> bool {
        self.found
            .as_ref()
            .is_some_and(|(_, at)| now.saturating_duration_since(*at) < LAUNCH_STEPS_FOR)
    }

    /// Whether the Mac is still one to ask, read just before the sheet goes up: one the vault was found for unconfigured is not, once a vault is configured (a person joined from a terminal while the ask waited).
    pub(crate) fn still_asks(&self) -> bool {
        self.unfinished || !(self.reach().configured_now)()
    }

    /// The look waits on `why`: look again after [`LOOK_EVERY`].
    pub(crate) fn wait(&mut self, why: Wait, now: Instant) -> bool {
        self.next_look = Some(now + LOOK_EVERY);
        self.waiting.replace(why) != Some(why)
    }

    /// The park's deadline: the next look, while one is to come.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.next_look
            .filter(|_| self.found.is_some() && !self.over && self.sheet.is_none() && !self.joining)
    }

    /// The park's deadline while a sheet is up: when it has gone unanswered [`SHEET_FOR`].
    pub(crate) fn sheet_deadline(&self) -> Option<Instant> {
        self.sheet.map(|(_, at)| at + SHEET_FOR)
    }

    /// The sheet that is up, when it has gone unanswered [`SHEET_FOR`] at `now`.
    pub(crate) fn expired(&self, now: Instant) -> Option<u64> {
        self.sheet
            .filter(|&(_, at)| now >= at + SHEET_FOR)
            .map(|(id, _)| id)
    }

    /// A new sheet's id.
    pub(crate) fn mint_id(&mut self) -> u64 {
        self.last_id += 1;
        self.last_id
    }

    /// The sheet `id` went up at `now`.
    pub(crate) fn raised(&mut self, id: u64, now: Instant) {
        self.sheet = Some((id, now));
        self.waiting = None;
    }

    /// Whether a sheet is up.
    pub(crate) fn sheet_up(&self) -> bool {
        self.sheet.is_some()
    }

    /// The sheet `id` was answered: `Some(choice)` for the sheet that is up, and the ask is over for this process; `None` for a stale answer.
    pub(crate) fn answered(&mut self, id: u64, choice: Choice) -> Option<Choice> {
        if self.sheet.map(|(up, _)| up) != Some(id) {
            return None;
        }
        self.sheet = None;
        self.over = true;
        Some(choice)
    }

    /// The sheet went with no answer (its window closed, or `sudo`'s sheet needed it): asked again at the next quiet moment.
    pub(crate) fn withdrawn(&mut self) {
        self.sheet = None;
        self.next_look = None;
    }

    /// The ask is moot for this process: nothing is raised.
    pub(crate) fn end(&mut self) {
        self.over = true;
        self.next_look = None;
    }

    /// A join started (`started`) or could not.
    pub(crate) fn join_started(&mut self, started: bool) {
        self.joining = started;
    }

    /// The join ended.
    pub(crate) fn join_ended(&mut self) {
        self.joining = false;
    }

    /// Start `job` through the arm.
    pub(crate) fn run(&self, job: JoinJob) -> bool {
        (self.reach().run)(job)
    }
}

/// The band row's key: the join in flight, then the answer to the press, in place.
pub(crate) const KEY_JOIN: &str = "secrets.join";

/// The digest lines' subject (`privacy.secrets.<cause>`).
const SUBJECT: &str = "secrets";

/// The causes of the digest lines.
pub(crate) const CAUSE_PENDING: &str = "join-pending";
pub(crate) const CAUSE_FAILED: &str = "join-failed";
pub(crate) const CAUSE_UNFINISHED: &str = "join-unfinished";
pub(crate) const CAUSE_UNSYNCED: &str = "not-synced";

/// The key of the digest line with `cause` ([`Entry::key`]'s spelling).
pub(crate) fn entry_key(cause: &str) -> String {
    Fingerprint::new(Family::Privacy.stem(), SUBJECT, cause)
        .as_str()
        .to_string()
}

/// One line for the band: control characters dropped, capped.
fn clean(text: &str) -> String {
    atpkg::progress::sanitize_for_tty(text, 160)
}

/// `n secrets` (`1 secret`).
fn secrets_words(n: usize) -> String {
    if n == 1 {
        "1 secret".to_string()
    } else {
        format!("{n} secrets")
    }
}

/// The join in flight, from the press until its answer replaces it: the vault at the sheet's `source` (empty: the default).
pub(crate) fn joining_row(source: &str) -> Message {
    let source = source.trim();
    let lower = source.to_ascii_lowercase();
    let line = if is_alab_source(source) {
        format!("asking the vault at {}", vault_host())
    } else if lower.starts_with("http://") || lower.starts_with("https://") {
        format!(
            "asking the vault at {}",
            clean(&aterm_secrets::transport::describe(source))
        )
    } else {
        "reading the vault file".to_string()
    };
    Message::new(tags::PRIVACY, Severity::Info, "Joining the secrets vault")
        .line(line)
        .meter(aterm_messages::Meter::busy(""))
        .working(JOIN_WAIT + Duration::from_secs(60))
        .key(KEY_JOIN)
}

/// The answer to Join: in, and synced; `follows` when the subscriber keeps it so.
pub(crate) fn joined_notice(names: usize, follows: bool) -> Message {
    let held = format!("{} in ~/.secrets", secrets_words(names));
    Message::new(tags::PRIVACY, Severity::Info, "Joined the secrets vault")
        .line(if follows {
            format!("{held}; this Mac keeps them in sync")
        } else {
            held
        })
        .notice(NoticeKind::Answer)
        .key(KEY_JOIN)
}

/// What fixes a mistyped join password on a Mac that waits with one: its proof matched
const RETYPE: &str = "a mistyped join password is fixed by running aterm secrets join in a terminal, adding the vault's URL if not the ALab one (it asks for the password again)";

/// What the digest line of a join that failed or never finished, or of a subscriber that
fn sync_in_system_state() -> String {
    format!(
        "In {}, Apply with the vault row on and the Mac settings rows off",
        crate::message_reporters::SYSTEM_STATE_MENU
    )
}

/// The details line that says which rows [`sync_in_system_state`] means and why: the Mac
fn rows_named() -> String {
    use aterm_types::macos_setup::Setting;
    let mut mac = Setting::ALL
        .iter()
        .filter(|setting| setting.is_workstation());
    let first = mac.next().map_or("", |setting| setting.label());
    let last = mac.next_back().map_or(first, |setting| setting.label());
    format!(
        "The Mac settings rows run from {first} to {last}, which Apply runs again, the Dock and sign-outs too; leave the rest as they are (with {} off, aterm stops asking for them)",
        Setting::Permissions.label()
    )
}

/// The same join from a terminal, beside [`sync_in_system_state`]: it names the vault's URL for
const JOIN_AGAIN: &str =
    "In a terminal, aterm secrets join does the same, adding the vault's URL if not the ALab one";

/// The same subscriber install from a terminal, beside [`sync_in_system_state`].
const AGENT_ON: &str = "In a terminal, aterm secrets agent on installs it too";

/// The answer to Join: asked, and an admin Mac approves `code`; [`RETYPE`] when a join password went with the request.
pub(crate) fn pending_notice(code: &str, with_password: bool) -> Message {
    let message = Message::new(
        tags::PRIVACY,
        Severity::Info,
        "Asked to join the secrets vault",
    )
    .line(format!(
        "an admin Mac approves code {}; this Mac joins by itself then",
        clean(code)
    ))
    .line(format!(
        "on an admin Mac: aterm secrets approve {}",
        clean(code)
    ));
    if with_password {
        message.line(RETYPE)
    } else {
        message
    }
    .notice(NoticeKind::Answer)
    .key(KEY_JOIN)
}

/// The answer to Join: it failed, and why (what failed, never a value).
pub(crate) fn failed_notice(why: &str) -> Message {
    Message::new(
        tags::PRIVACY,
        Severity::Warn,
        "Could not join the secrets vault",
    )
    .line(clean(why))
    .notice(NoticeKind::Answer)
    .key(KEY_JOIN)
}

/// The digest line while an admin Mac has not approved this one; [`RETYPE`] when a join password went with the request.
pub(crate) fn pending_entry(code: &str, host: &str, with_password: bool) -> Entry {
    let code = clean(code);
    let entry = Entry::new(
        Family::Privacy,
        SUBJECT,
        CAUSE_PENDING,
        "This Mac needs vault approval",
        Why::OnlyYou(Reason::new(
            "only an admin Mac can let this Mac into the secrets vault",
        )),
        Then::step(format!("On an admin Mac run aterm secrets approve {code}")),
    )
    .more(format!(
        "This Mac asked as {}, code {code}; it joins by itself once approved.",
        clean(host)
    ));
    if with_password {
        entry.more(format!(
            "{}.",
            crate::update_words::capitalize_first(RETYPE)
        ))
    } else {
        entry
    }
    .log_line(format!("secrets join-pending code={code}"))
}

/// The digest line after a join that failed.
pub(crate) fn failed_entry(why: &str) -> Entry {
    Entry::new(
        Family::Privacy,
        SUBJECT,
        CAUSE_FAILED,
        "Joining the secrets vault failed",
        Why::YourAction(Reason::new("the secrets vault did not let this Mac in")),
        Then::step(sync_in_system_state()),
    )
    .severity(Severity::Warn)
    .more(clean(why))
    .more(rows_named())
    .more(JOIN_AGAIN)
    .log_line("secrets join-failed")
}

/// The digest line for a Mac set up for a vault whose join never finished: it never asked, or nothing is left to finish it (aterm quit mid-join, a request the vault refused).
pub(crate) fn unfinished_entry() -> Entry {
    Entry::new(
        Family::Privacy,
        SUBJECT,
        CAUSE_UNFINISHED,
        "The vault join did not finish",
        Why::YourAction(Reason::new(
            "this Mac was set up for a secrets vault but nothing will let it in",
        )),
        Then::step(sync_in_system_state()),
    )
    .severity(Severity::Warn)
    .more("A join was cut off before it asked the vault or installed its subscriber.")
    .more(rows_named())
    .more(JOIN_AGAIN)
    .log_line("secrets join-unfinished")
}

/// The digest line for a Mac that joined (or asked) but whose subscriber is not running, so its secrets do not follow the vault: the remedy follows the reason.
pub(crate) fn unsynced_entry(why: &Unsynced) -> Entry {
    let (reason, step) = match why {
        Unsynced::Fleeting(_) => (
            "aterm runs from a copy that goes away, so it installed no subscriber",
            "Move aterm to Applications and open it from there".to_string(),
        ),
        Unsynced::Failed(_) => (
            "aterm could not install the subscriber that keeps ~/.secrets in sync",
            sync_in_system_state(),
        ),
    };
    let entry = Entry::new(
        Family::Privacy,
        SUBJECT,
        CAUSE_UNSYNCED,
        "Secrets do not stay in sync",
        Why::YourAction(Reason::new(reason)),
        Then::step(step),
    )
    .severity(Severity::Warn);
    match why {
        Unsynced::Fleeting(at) => entry.more(format!(
            "This aterm runs from {}. Opened from Applications, it installs the subscriber by itself.",
            clean(at)
        )),
        Unsynced::Failed(why) => entry.more(clean(why)).more(rows_named()).more(AGENT_ON),
    }
    .log_line("secrets not-synced")
}

impl App {
    /// Where the join worker posts: the event loop's proxy (none on the test App).
    fn secrets_post(&self) -> Option<Post> {
        let proxy = self.proxy.clone()?;
        Some(Arc::new(move |wake| {
            let _ = proxy.send_event(Wake::SecretsVault(wake));
        }))
    }

    /// One of the lane's events (`Wake::SecretsVault`).
    pub(crate) fn on_secrets_vault(&mut self, wake: VaultWake) {
        match wake {
            VaultWake::Found { found, unfinished } => {
                aterm_log::info!(
                    "secrets vault: the ALab vault answers on this LAN (seq {}, signer {})",
                    found.seq,
                    found.fingerprint
                );
                self.secrets_join
                    .on_found(found, unfinished, Instant::now());
            }
            VaultWake::Configured(settled) => self.secrets_settled(&settled),
            VaultWake::Answered { id, choice } => self.answer_secrets_sheet(id, choice),
            VaultWake::Joined(end) => self.secrets_join_ended(end),
        }
    }

    /// A configured Mac's launch read: the lines that no longer hold end, the ones that do
    fn secrets_settled(&mut self, settled: &Settled) {
        if settled.member {
            self.clear_secrets_join_lines();
        }
        match &settled.subscriber {
            Subscriber::Running => self.except_clear(&entry_key(CAUSE_UNSYNCED)),
            Subscriber::Missing(why) => {
                aterm_log::warn!("secrets vault: {}", why.why());
                self.except(&unsynced_entry(why));
            }
            Subscriber::Off => {}
        }
        if settled.unfinished {
            aterm_log::warn!("secrets vault: this Mac's join never finished");
            self.except(&unfinished_entry());
        }
    }

    /// Setup's Secrets stage read this Mac in its vault, synced, with its subscriber running (System State's Apply, [`App::publish_system_state`]): every vault line ends, as at a launch read that finds the same.
    pub(crate) fn secrets_synced_by_setup(&mut self) {
        aterm_log::info!("secrets vault: setup synced this Mac, and its subscriber runs");
        self.secrets_settled(&Settled {
            member: true,
            subscriber: Subscriber::Running,
            unfinished: false,
        });
    }

    /// The lines a join that let this Mac in ends.
    fn clear_secrets_join_lines(&mut self) {
        for cause in [CAUSE_PENDING, CAUSE_FAILED, CAUSE_UNFINISHED] {
            self.except_clear(&entry_key(cause));
        }
    }

    /// THE ASK'S LOOK, from the hand-over lane's turn after the debugger-access launch ask's: a sheet unanswered for [`SHEET_FOR`] goes as Not Now; then [`admit`] decides on what this gathers; `quiet` is [`App::devtools_moment_is_quiet`] (a test's stand-in).
    pub(crate) fn look_for_secrets_moment(
        &mut self,
        now: Instant,
        quiet: fn(&Self, Instant) -> bool,
    ) {
        if self.workstation_setup.unified() {
            return;
        }
        if let Some(id) = self.secrets_join.expired(now) {
            self.expire_secrets_sheet(id);
        }
        if !self.secrets_join.due(now) {
            return;
        }
        let privacy = self.config.privacy_probe_gate().permits() && self.config.privacy_notice();
        let fda_decided = self.consent_launch.fda() != crate::consent_launch::FdaLaunch::Undecided;
        let mut devtools_decided = self.devtools_helper.reading().is_some();
        if privacy && fda_decided && !devtools_decided && self.any_os_window_focused() {
            devtools_decided = self.devtools_launch_read_done();
        }
        let facts = AskFacts {
            launch_steps: privacy
                && (!fda_decided || !devtools_decided)
                && self.secrets_join.launch_steps_may_hold(now),
            debugger: self.debugger_launch.queued() || self.debugger_change.under_way(),
            page: self.devtools_helper.panel().is_some()
                || self.devtools_helper.fda_waiting()
                || self.devtools_helper.devtools_waiting(),
            warmup: self.consent_warmup.is_live(),
            other_sheet: self.other_sheet_up(),
            quiet: quiet(self, now),
            owner_quiet: self
                .owner_quiet
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .quiet_now(),
        };
        match admit(&facts) {
            Ask::Wait(why) => {
                if self.secrets_join.wait(why, now) {
                    aterm_log::debug!("secrets vault: the ask waits ({})", why.as_str());
                }
            }
            Ask::Raise => self.raise_secrets_ask(now),
        }
    }

    /// The sheet `id` went unanswered for [`SHEET_FOR`]: it ends, as Not Now.
    fn expire_secrets_sheet(&mut self, id: u64) {
        #[cfg(target_os = "macos")]
        let _ = self.take_secrets_sheet(id, false);
        if self.secrets_join.answered(id, Choice::NotNow).is_some() {
            aterm_log::info!(
                "secrets vault: the ask went unanswered for {}s; asked again at the next launch",
                SHEET_FOR.as_secs()
            );
        }
    }

    /// Whether another sheet holds the screen: `sudo`'s password sheet.
    fn other_sheet_up(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            crate::askpass::PROMPT_INPUT.is_open()
        }
        #[cfg(not(target_os = "macos"))]
        false
    }

    /// Raise the sheet now, on a Mac that is still one to ask ([`Lane::still_asks`]); a
    fn raise_secrets_ask(&mut self, now: Instant) {
        if !self.secrets_join.still_asks() {
            self.secrets_join.end();
            aterm_log::info!("secrets vault: a vault was configured meanwhile; not asked");
            return;
        }
        #[cfg(target_os = "macos")]
        match self.raise_secrets_sheet() {
            Ok(id) => {
                self.secrets_join.raised(id, now);
                aterm_log::info!("secrets vault: asked to join (sheet {id})");
            }
            Err(why) => {
                if self.secrets_join.wait(Wait::BehindSheet, now) {
                    aterm_log::debug!("secrets vault: the sheet was not raised: {why}");
                }
            }
        }
        #[cfg(not(target_os = "macos"))]
        let _ = self.secrets_join.wait(Wait::BehindSheet, now);
    }

    /// The sheet was answered: Join starts the join with the typed password (empty: to be
    fn answer_secrets_sheet(&mut self, id: u64, choice: Choice) {
        #[cfg(target_os = "macos")]
        let typed = self.take_secrets_sheet_with_source(id, choice == Choice::Join);
        let Some(choice) = self.secrets_join.answered(id, choice) else {
            return;
        };
        aterm_log::info!("secrets vault: the ask was answered: {}", choice.as_str());
        match choice {
            Choice::Join => {
                #[cfg(target_os = "macos")]
                let (source, password) =
                    typed.unwrap_or_else(|| (aterm_secrets::ALAB_DEFAULT_URL.to_owned(), None));
                #[cfg(not(target_os = "macos"))]
                let (source, password): (String, Option<SecretBytes>) =
                    (aterm_secrets::ALAB_DEFAULT_URL.to_owned(), None);
                self.start_secrets_join(source, password);
            }
            Choice::NotNow => {}
            Choice::Never => self.decline_secrets_join(),
        }
    }

    /// Start the join; the band shows it in flight until its answer replaces the row.
    pub(crate) fn start_secrets_join(&mut self, source: String, password: Option<SecretBytes>) {
        let Some(post) = self.secrets_post() else {
            return;
        };
        let row = joining_row(&source);
        let started = self.secrets_join.run(JoinJob {
            source,
            password,
            post,
            config: self.consent_card_config.clone(),
        });
        self.secrets_join.join_started(started);
        if started {
            self.post_message(row);
        } else {
            self.secrets_join_ended(JoinEnd::Failed {
                why: "the join could not start".to_string(),
            });
        }
    }

    /// Write this Mac into [`DECLINED_RECORD`], off the event loop (the write is durable).
    fn decline_secrets_join(&mut self) {
        let Some(config) = self.consent_card_config.clone() else {
            aterm_log::warn!("secrets vault: no config folder to record Don't Ask Again in");
            return;
        };
        let spawned = crate::qos::spawn_lane_thread(
            "aterm-secrets-decline",
            crate::qos::Role::Background,
            move || {
                let host = aterm_containment::consent::host_uuid();
                let now = aterm_time::unix_secs().unwrap_or(0);
                match note_declined(&config, host.as_deref(), now) {
                    Ok(()) => aterm_log::info!("secrets vault: this Mac is not asked again"),
                    Err(error) => aterm_log::warn!(
                        "secrets vault: Don't Ask Again was not recorded ({error}); the next launch asks again"
                    ),
                }
            },
        );
        if let Err(error) = spawned {
            aterm_log::warn!("secrets vault: Don't Ask Again was not recorded: {error}");
        }
    }

    /// A join ended: the answer to the press on the band, in place of the row in flight, and the digest lines that now hold or no longer do.
    pub(crate) fn secrets_join_ended(&mut self, end: JoinEnd) {
        self.secrets_join.join_ended();
        let subscriber = match &end {
            JoinEnd::Joined { names, subscriber } => {
                aterm_log::info!(
                    "secrets vault: joined; {} in ~/.secrets",
                    secrets_words(*names)
                );
                self.post_message(joined_notice(*names, subscriber.is_none()));
                self.clear_secrets_join_lines();
                subscriber.as_ref()
            }
            JoinEnd::Pending {
                code,
                host,
                with_password,
                subscriber,
            } => {
                aterm_log::info!("secrets vault: asked to join as {host}, code {code}");
                self.post_message(pending_notice(code, *with_password));
                self.except_clear(&entry_key(CAUSE_FAILED));
                self.except_clear(&entry_key(CAUSE_UNFINISHED));
                self.except(&pending_entry(code, host, *with_password));
                subscriber.as_ref()
            }
            JoinEnd::Failed { why } => {
                aterm_log::warn!("secrets vault: the join failed: {why}");
                self.post_message(failed_notice(why));
                self.except(&failed_entry(why));
                return;
            }
        };
        match subscriber {
            Some(why) => self.except(&unsynced_entry(why)),
            None => self.except_clear(&entry_key(CAUSE_UNSYNCED)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// The probe lane's schedule: 15 s, doubling, capped at five minutes, for as long as
    #[test]
    fn the_backoff_doubles_from_fifteen_seconds_to_five_minutes() {
        let secs: Vec<u64> = (1..=9).map(|n| retry_after(n).as_secs()).collect();
        assert_eq!(secs, [15, 30, 60, 120, 240, 300, 300, 300, 300]);
        assert_eq!(retry_after(0), RETRY_FIRST, "no failure yet still pauses");
        assert_eq!(retry_after(u32::MAX), RETRY_CAP);
    }

    /// What one scripted lane run did: its posts, pauses, probes, installs and paid debts.
    #[derive(Default)]
    struct Script {
        posts: Vec<String>,
        pauses: Vec<Duration>,
        probes: usize,
        installs: usize,
        paid: usize,
    }

    /// The next scripted answer: each in turn, the last one repeating.
    fn next<T: Clone>(queue: &Mutex<Vec<T>>) -> T {
        let mut queue = queue.lock().unwrap();
        if queue.len() > 1 {
            queue.remove(0)
        } else {
            queue[0].clone()
        }
    }

    /// A lane over scripted reads: `configured` answers in turn (the last repeats), and so
    fn lane_with(
        configured: Vec<Result<Configured, String>>,
        declined: bool,
        probes: Vec<Result<Option<Found>, Missed>>,
        install: Result<(), Unsynced>,
    ) -> (LaneEnd, Script) {
        let script = Arc::new(Mutex::new(Script::default()));
        let configured = Arc::new(Mutex::new(configured));
        let probes = Arc::new(Mutex::new(probes));
        let reach = ProbeReach {
            configured: Box::new({
                let configured = Arc::clone(&configured);
                move || next(&configured)
            }),
            declined: Box::new(move || declined),
            probe: Box::new({
                let script = Arc::clone(&script);
                let probes = Arc::clone(&probes);
                move |wait| {
                    assert_eq!(wait, PROBE_WAIT);
                    script.lock().unwrap().probes += 1;
                    next(&probes)
                }
            }),
            sleep: Box::new({
                let script = Arc::clone(&script);
                move |pause| script.lock().unwrap().pauses.push(pause)
            }),
            install: Box::new({
                let script = Arc::clone(&script);
                move || {
                    script.lock().unwrap().installs += 1;
                    install.clone()
                }
            }),
            paid: Box::new({
                let script = Arc::clone(&script);
                move || script.lock().unwrap().paid += 1
            }),
        };
        let posted = Arc::clone(&script);
        let end = run_probe_lane(&reach, &move |wake| {
            posted.lock().unwrap().posts.push(format!("{wake:?}"));
        });
        drop(reach);
        let script = Arc::try_unwrap(script).ok().unwrap().into_inner().unwrap();
        (end, script)
    }

    fn lane(
        configured: Vec<Result<Configured, String>>,
        declined: bool,
        probes: Vec<Result<Option<Found>, Missed>>,
    ) -> (LaneEnd, Script) {
        lane_with(configured, declined, probes, Ok(()))
    }

    fn found() -> Found {
        Found {
            seq: 7,
            fingerprint: "SHA256:test".to_string(),
        }
    }

    fn found_wake(unfinished: bool) -> String {
        format!(
            "{:?}",
            VaultWake::Found {
                found: found(),
                unfinished
            }
        )
    }

    fn settled_wake(member: bool, subscriber: Subscriber, unfinished: bool) -> String {
        format!(
            "{:?}",
            VaultWake::Configured(Settled {
                member,
                subscriber,
                unfinished
            })
        )
    }

    /// A configured Mac's setup: in, on the ALab vault, its subscriber sound.
    fn setup() -> Setup {
        Setup {
            alab: true,
            member: true,
            pending: false,
            plist: Plist::Sound,
            owed: false,
        }
    }

    /// A Mac whose join was cut off: configured for the ALab vault, not in, no subscriber.
    fn cut_off() -> Setup {
        Setup {
            member: false,
            pending: true,
            plist: Plist::Absent,
            ..setup()
        }
    }

    const NONE: Result<Configured, String> = Ok(Configured::None);

    fn refused() -> Result<Option<Found>, Missed> {
        Err(Missed {
            why: "No route to host".to_string(),
            resolved: true,
        })
    }

    fn unresolved() -> Result<Option<Found>, Missed> {
        Err(Missed {
            why: "nodename nor servname provided, or not known".to_string(),
            resolved: false,
        })
    }

    /// WHEN TO PROBE: only a Mac with no vault configured (or an unfinished join, below)
    #[test]
    fn only_an_unconfigured_mac_that_has_not_declined_probes() {
        let member = Ok(Configured::Some(setup()));
        let (end, script) = lane(vec![member], false, vec![Ok(Some(found()))]);
        assert_eq!(end, LaneEnd::Configured);
        assert_eq!((script.probes, script.installs), (0, 0));
        assert_eq!(
            script.posts,
            [settled_wake(true, Subscriber::Running, false)]
        );

        let (end, script) = lane(vec![NONE], true, vec![Ok(Some(found()))]);
        assert_eq!(end, LaneEnd::Declined);
        assert_eq!((script.probes, script.posts.len()), (0, 0));

        let (end, script) = lane(
            vec![Err("source.toml: no name".to_string())],
            false,
            vec![Ok(Some(found()))],
        );
        assert_eq!(end, LaneEnd::Unreadable("source.toml: no name".to_string()));
        assert_eq!((script.probes, script.posts.len()), (0, 0));
    }

    /// A vault that answers is posted at once; no ALab vault there ends the lane quietly.
    #[test]
    fn an_answer_is_posted_and_no_vault_ends_the_lane() {
        let (end, script) = lane(vec![NONE], false, vec![Ok(Some(found()))]);
        assert_eq!(end, LaneEnd::Found);
        assert_eq!(script.probes, 1);
        assert!(script.pauses.is_empty());
        assert_eq!(script.posts, [found_wake(false)]);

        let (end, script) = lane(vec![NONE], false, vec![Ok(None)]);
        assert_eq!(end, LaneEnd::NoVault);
        assert_eq!((script.probes, script.posts.len()), (1, 0));
    }

    /// SOMETHING THAT IS NOT THE ALAB VAULT answering at its name and port (another vault,
    #[cfg(unix)]
    #[test]
    fn an_answer_that_is_not_the_alab_vault_ends_the_lane() {
        use aterm_secrets::client::{ProbeOutcome, Probed};
        let no_lookup = || -> bool { panic!("an answer needs no lookup") };
        let not_it = || {
            heard(
                Ok(ProbeOutcome::NotThisVault(
                    "envelope: signed by a key the pin does not endorse".to_string(),
                )),
                no_lookup,
            )
        };
        assert_eq!(not_it(), Ok(None));
        let (end, script) = lane(vec![NONE], false, vec![not_it()]);
        assert_eq!(end, LaneEnd::NoVault);
        assert_eq!(script.probes, 1);
        assert!(script.pauses.is_empty(), "never asked again this launch");
        assert!(script.posts.is_empty());

        assert_eq!(heard(Ok(ProbeOutcome::Absent), no_lookup), Ok(None));
        let probed = Probed {
            seq: 7,
            fingerprint: "SHA256:test".to_string(),
        };
        assert_eq!(
            heard(Ok(ProbeOutcome::Found(probed)), no_lookup),
            Ok(Some(found()))
        );
        for resolves in [true, false] {
            assert_eq!(
                heard(Err(aterm_secrets::Error::new("No route to host")), || {
                    resolves
                }),
                Err(Missed {
                    why: "No route to host".to_string(),
                    resolved: resolves,
                })
            );
        }
    }

    /// LOCAL NETWORK CONSENT: the first probes fail until the person allows aterm, and the
    #[test]
    fn failures_are_retried_on_the_schedule_until_the_vault_answers() {
        let mut probes: Vec<_> = (0..7).map(|_| refused()).collect();
        probes.push(Ok(Some(found())));
        let (end, script) = lane(vec![NONE], false, probes);
        assert_eq!(end, LaneEnd::Found);
        assert_eq!(script.probes, 8);
        let secs: Vec<u64> = script.pauses.iter().map(Duration::as_secs).collect();
        assert_eq!(secs, [15, 30, 60, 120, 240, 300, 300]);
        assert_eq!(script.posts, [found_wake(false)]);

        let mut probes: Vec<_> = (0..40).map(|_| refused()).collect();
        probes.push(Ok(Some(found())));
        let (end, script) = lane(vec![NONE], false, probes);
        assert_eq!(end, LaneEnd::Found);
        assert!(script.pauses.iter().sum::<Duration>() > UNRESOLVED_FOR * 2);
    }

    /// NOT THE ALAB LAN: a name that resolves to nothing — a network with no such host, or
    #[test]
    fn a_name_that_never_resolves_ends_the_lane_after_an_hour() {
        let (end, script) = lane(vec![NONE], false, vec![unresolved()]);
        assert_eq!(end, LaneEnd::Unresolved);
        let paused: Duration = script.pauses.iter().sum();
        assert!(paused >= UNRESOLVED_FOR, "{paused:?}");
        assert!(paused < UNRESOLVED_FOR + RETRY_CAP, "{paused:?}");
        assert_eq!(script.probes, script.pauses.len() + 1);
        assert!(script.posts.is_empty());

        let mut probes: Vec<_> = (0..10).map(|_| unresolved()).collect();
        probes.push(Ok(Some(found())));
        let (end, _) = lane(vec![NONE], false, probes);
        assert_eq!(end, LaneEnd::Found);

        let mut probes: Vec<_> = (0..12).map(|_| unresolved()).collect();
        probes.push(refused());
        probes.push(unresolved());
        let (end, script) = lane(vec![NONE], false, probes);
        assert_eq!(end, LaneEnd::Unresolved);
        assert!(script.pauses.iter().sum::<Duration>() > UNRESOLVED_FOR + RETRY_CAP);
    }

    /// A person who joins from a terminal while the lane waits ends it: nothing is asked,
    #[test]
    fn a_join_from_a_terminal_meanwhile_ends_the_lane() {
        let joined = Ok(Configured::Some(Setup {
            member: false,
            pending: true,
            ..setup()
        }));
        let (end, script) = lane(vec![NONE, NONE, joined], false, vec![refused()]);
        assert_eq!(end, LaneEnd::Configured);
        assert_eq!(script.probes, 2);
        assert_eq!(script.pauses.len(), 2);
        assert_eq!(
            script.posts,
            [settled_wake(false, Subscriber::Running, false)]
        );
    }

    /// AN UNFINISHED JOIN (a join cut off after it wrote its source: aterm quit, the
    #[test]
    fn an_unfinished_join_is_told_and_the_alab_vault_offered_again() {
        let unfinished = || Ok(Configured::Some(cut_off()));
        let (end, script) = lane(
            vec![unfinished()],
            false,
            vec![refused(), Ok(Some(found()))],
        );
        assert_eq!(end, LaneEnd::Found);
        assert_eq!(script.probes, 2, "the re-read of an unfinished Mac goes on");
        assert_eq!(
            script.posts,
            [settled_wake(false, Subscriber::Off, true), found_wake(true)]
        );

        let (end, script) = lane(vec![unfinished()], true, vec![Ok(Some(found()))]);
        assert_eq!(end, LaneEnd::Declined);
        assert_eq!(script.posts, [settled_wake(false, Subscriber::Off, true)]);

        let elsewhere = Ok(Configured::Some(Setup {
            alab: false,
            ..cut_off()
        }));
        let (end, script) = lane(vec![elsewhere], false, vec![Ok(Some(found()))]);
        assert_eq!(end, LaneEnd::Configured);
        assert_eq!(script.probes, 0);
        assert_eq!(script.posts, [settled_wake(false, Subscriber::Off, true)]);
    }

    /// THE SUBSCRIBER'S REPAIR at launch: a plist running a copy that is gone or does not
    #[test]
    fn a_stale_or_owed_subscriber_is_installed_at_launch() {
        let stale = Ok(Configured::Some(Setup {
            plist: Plist::Stale("/Volumes/aterm/aterm.app/Contents/MacOS/aterm".into()),
            ..setup()
        }));
        let (end, script) = lane(vec![stale.clone()], false, vec![Ok(None)]);
        assert_eq!((end, script.installs), (LaneEnd::Configured, 1));
        assert_eq!(
            script.posts,
            [settled_wake(true, Subscriber::Running, false)]
        );

        let fleeting = Unsynced::Fleeting("/Volumes/aterm/aterm.app/Contents/MacOS/aterm".into());
        let (_, script) = lane_with(vec![stale], false, vec![Ok(None)], Err(fleeting.clone()));
        assert_eq!(
            script.posts,
            [settled_wake(true, Subscriber::Missing(fleeting), false)]
        );

        let owed = Ok(Configured::Some(Setup {
            plist: Plist::Absent,
            owed: true,
            ..setup()
        }));
        let (_, script) = lane(vec![owed], false, vec![Ok(None)]);
        assert_eq!((script.installs, script.paid), (1, 0));

        let paid = Ok(Configured::Some(Setup {
            owed: true,
            ..setup()
        }));
        let (_, script) = lane(vec![paid], false, vec![Ok(None)]);
        assert_eq!((script.installs, script.paid), (0, 1));

        let off = Ok(Configured::Some(Setup {
            plist: Plist::Absent,
            ..setup()
        }));
        let (_, script) = lane(vec![off], false, vec![Ok(None)]);
        assert_eq!((script.installs, script.paid), (0, 0));
        assert_eq!(script.posts, [settled_wake(true, Subscriber::Off, false)]);
    }

    /// THE PLAN, case by case: only a stale plist, or an absent one that is owed, is
    #[test]
    fn the_launch_plan_installs_only_what_is_broken_or_owed() {
        let stale = Plist::Stale("/gone".to_string());
        for plist in [Plist::Absent, Plist::Sound, stale] {
            for owed in [false, true] {
                for pending in [false, true] {
                    let read = Setup {
                        alab: true,
                        member: !pending,
                        pending,
                        plist: plist.clone(),
                        owed,
                    };
                    let got = plan(&read);
                    let want = Plan {
                        install: matches!(plist, Plist::Stale(_))
                            || (plist == Plist::Absent && owed),
                        paid: plist == Plist::Sound && owed,
                        unfinished: pending && plist == Plist::Absent && !owed,
                    };
                    assert_eq!(got, want, "{read:?}");
                    assert_eq!(asks_again(&read), want.unfinished);
                    assert!(!asks_again(&Setup {
                        alab: false,
                        ..read
                    }));
                }
            }
        }
    }

    /// The plist's program is read back exactly as `aterm secrets` writes it, entities
    #[cfg(unix)]
    #[test]
    fn the_plists_program_is_read_back_as_service_writes_it() {
        use aterm_secrets::service;
        let program = Path::new("/Applications/a&b <c> \"d\" 'e'.app/Contents/MacOS/aterm");
        let text = service::plist_text(
            service::Job::Subscriber.label(),
            program,
            &["secrets".to_string(), "watch".to_string()],
            Path::new("/Users//x/.config/aterm/secrets/x.log"),
        );
        assert_eq!(plist_program(&text).as_deref(), program.to_str());
        assert_eq!(plist_program("<plist><dict></dict></plist>"), None);
        assert_eq!(
            plist_program("<key>ProgramArguments</key><string>/x</string>"),
            None,
            "not an array"
        );
        assert_eq!(
            plist_program("<key>ProgramArguments</key><array><string>/a&#47;b</string>"),
            None,
            "an entity plist_text never writes"
        );
    }

    /// THE PLIST ON DISK: absent; sound when it runs a file at a lasting place; stale when
    #[cfg(unix)]
    #[test]
    fn a_plist_is_judged_by_the_program_it_runs() {
        use aterm_secrets::service;
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let plist = dir.path().join("job.plist");
        assert_eq!(subscriber_plist(&plist), Plist::Absent);
        let write = |program: &Path| {
            let text = service::plist_text("x", program, &[], Path::new("/tmp/x.log"));
            std::fs::write(&plist, text).expect("plist");
        };
        write(Path::new("/bin/sh"));
        assert_eq!(subscriber_plist(&plist), Plist::Sound);
        let gone = Path::new("/Applications/no-such-aterm.app/Contents/MacOS/aterm");
        write(gone);
        assert_eq!(
            subscriber_plist(&plist),
            Plist::Stale(gone.display().to_string())
        );
        let scratch = dir.path().join("aterm");
        std::fs::write(&scratch, b"").expect("program");
        write(&scratch);
        assert_eq!(
            subscriber_plist(&plist),
            Plist::Stale(scratch.display().to_string()),
            "a temporary directory does not last"
        );
        std::fs::write(&plist, b"bplist00").expect("binary");
        assert_eq!(subscriber_plist(&plist), Plist::Sound, "left as it is");
    }

    /// THE COPY A SUBSCRIBER MAY NAME: one that lasts. A disk image and an App
    #[test]
    fn only_a_lasting_copy_is_named_by_the_subscriber() {
        for fleeting in [
            "/Volumes/aterm/aterm.app/Contents/MacOS/aterm",
            "/private/var/folders/ab/xyz/T/AppTranslocation/0A1B/d/aterm.app/Contents/MacOS/aterm",
        ] {
            assert_eq!(
                lasting_program(PathBuf::from(fleeting)),
                Err(Unsynced::Fleeting(fleeting.to_string()))
            );
        }
        let installed = PathBuf::from("/Applications/aterm.app/Contents/MacOS/aterm");
        assert_eq!(lasting_program(installed.clone()), Ok(installed));
    }

    /// A member's subscriber, after a pending join: sound is none; absent says where this
    #[test]
    fn a_members_subscriber_gap_is_read_from_its_plist() {
        let never = || -> Result<PathBuf, Unsynced> { panic!("not asked") };
        assert_eq!(subscriber_gap(&Plist::Sound, never), None);
        let fleeting = Unsynced::Fleeting("/Volumes/aterm/aterm.app".into());
        assert_eq!(
            subscriber_gap(&Plist::Absent, || Err(fleeting.clone())),
            Some(fleeting)
        );
        assert_eq!(
            subscriber_gap(&Plist::Absent, || Ok(PathBuf::from(
                "/Applications/aterm.app"
            ))),
            Some(Unsynced::Failed("the subscriber is not installed".into()))
        );
        assert!(matches!(
            subscriber_gap(&Plist::Stale("/gone".into()), never),
            Some(Unsynced::Failed(why)) if why.contains("/gone")
        ));
    }

    /// THE DECLINED RECORD: absent is not declined; Don't Ask Again names this Mac (and
    #[test]
    fn the_declined_record_names_this_mac_only() {
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let config = dir.path().join("aterm.toml");
        assert!(!declined(&config, Some("MAC-A")));
        note_declined(&config, Some("MAC-A"), 100).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join(DECLINED_RECORD)).unwrap(),
            "MAC-A 100\n"
        );
        assert!(declined(&config, Some("MAC-A")));
        assert!(
            !declined(&config, Some("MAC-B")),
            "another Mac on a synced config"
        );
        assert!(!declined(&config, None), "no host: asked");
        assert_eq!(
            note_declined(&config, None, 1).map_err(|e| e.kind()),
            Err(std::io::ErrorKind::NotFound)
        );
        assert!(!crate::debugger_access::read_asked(&config, "MAC-A"));
    }

    /// THE OWED RECORD: noted for this Mac only, forgotten once paid (the file goes with
    #[test]
    fn the_owed_record_is_noted_forgotten_and_fails_toward_owing_nothing() {
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let config = dir.path().join("aterm.toml");
        let record = dir.path().join(OWED_RECORD);
        assert!(!owed(Some(&config), Some("MAC-A")));
        note_owed(Some(&config), Some("MAC-A"), true);
        note_owed(Some(&config), Some("MAC-B"), true);
        assert!(owed(Some(&config), Some("MAC-A")) && owed(Some(&config), Some("MAC-B")));
        assert!(!owed(Some(&config), Some("MAC-C")));
        note_owed(Some(&config), Some("MAC-A"), false);
        assert!(!owed(Some(&config), Some("MAC-A")));
        assert!(
            owed(Some(&config), Some("MAC-B")),
            "another Mac's debt stays"
        );
        note_owed(Some(&config), Some("MAC-B"), false);
        assert!(!record.exists(), "a record naming no Mac goes");
        note_owed(Some(&config), Some("MAC-B"), false);

        std::fs::write(&record, "not a record").unwrap();
        assert!(
            !owed(Some(&config), Some("MAC-A")),
            "unreadable: nothing owed"
        );
        assert!(
            crate::debugger_access::read_host_record(&config, OWED_RECORD, "MAC-A"),
            "where the declined record's reader fails toward named"
        );
        note_owed(Some(&config), Some("MAC-A"), false);
        assert_eq!(
            std::fs::read_to_string(&record).unwrap(),
            "not a record",
            "a record that cannot be read is left as found"
        );
        assert!(!owed(None, Some("MAC-A")) && !owed(Some(&config), None));
    }

    /// One fact set to hold, in a case of the admission test.
    type SetFact = fn(&mut AskFacts);

    fn quiet_facts() -> AskFacts {
        AskFacts {
            launch_steps: false,
            debugger: false,
            page: false,
            warmup: false,
            other_sheet: false,
            quiet: true,
            owner_quiet: false,
        }
    }

    /// WHEN TO ASK: behind the launch steps, the debugger-access ask, a page, the folder
    #[test]
    fn the_ask_waits_for_every_launch_step_then_the_quiet_moment() {
        assert_eq!(admit(&quiet_facts()), Ask::Raise);
        let cases: [(SetFact, Wait); 7] = [
            (|f| f.launch_steps = true, Wait::BehindLaunchSteps),
            (|f| f.debugger = true, Wait::BehindDebuggerAsk),
            (|f| f.page = true, Wait::BehindPage),
            (|f| f.warmup = true, Wait::BehindWarmup),
            (|f| f.other_sheet = true, Wait::BehindSheet),
            (|f| f.quiet = false, Wait::NotQuiet),
            (|f| f.owner_quiet = true, Wait::OwnerQuiet),
        ];
        for (set, why) in cases {
            let mut facts = quiet_facts();
            set(&mut facts);
            assert_eq!(admit(&facts), Ask::Wait(why), "{why:?}");
        }
        let mut all = quiet_facts();
        for (set, _) in cases {
            set(&mut all);
        }
        assert_eq!(admit(&all), Ask::Wait(Wait::BehindLaunchSteps));
    }

    fn sheet_reach() -> JoinReach {
        JoinReach {
            sheet: true,
            run: |job| {
                drop(job);
                true
            },
            configured_now: never,
        }
    }

    /// The lane: nothing is due until the vault answers; a wait looks again after
    #[test]
    fn the_lane_asks_once_per_process() {
        let t0 = Instant::now();
        let mut lane = Lane::with_reach(sheet_reach());
        assert!(!lane.due(t0), "nothing found yet");
        lane.on_found(found(), false, t0);
        assert!(lane.due(t0));
        assert!(lane.wait(Wait::NotQuiet, t0), "a new wait is logged");
        assert!(!lane.wait(Wait::NotQuiet, t0), "the same wait is not");
        assert!(!lane.due(t0 + LOOK_EVERY - Duration::from_millis(1)));
        assert_eq!(lane.next_deadline(), Some(t0 + LOOK_EVERY));
        assert!(lane.due(t0 + LOOK_EVERY));

        let id = lane.mint_id();
        lane.raised(id, t0);
        assert!(lane.sheet_up() && !lane.due(t0 + LOOK_EVERY) && lane.next_deadline().is_none());
        lane.withdrawn();
        assert!(
            lane.due(t0 + LOOK_EVERY),
            "a sheet that went unanswered is asked again"
        );

        let id = lane.mint_id();
        lane.raised(id, t0);
        assert_eq!(lane.answered(id + 1, Choice::Join), None, "a stale answer");
        assert_eq!(lane.answered(id, Choice::NotNow), Some(Choice::NotNow));
        assert!(!lane.due(t0 + LOOK_EVERY * 10), "over for this process");
        lane.on_found(found(), false, t0);
        assert!(
            !lane.due(t0 + LOOK_EVERY * 10),
            "a later answer of the probe asks nothing"
        );
    }

    /// A SHEET NOBODY ANSWERS holds synthetic input fenced off, so it has a deadline: the
    #[test]
    fn an_unanswered_sheet_expires() {
        let t0 = Instant::now();
        let mut lane = Lane::with_reach(sheet_reach());
        lane.on_found(found(), false, t0);
        assert_eq!(lane.sheet_deadline(), None);
        #[cfg(target_os = "macos")]
        assert_eq!(
            SHEET_FOR,
            crate::askpass::ASKPASS_DEADLINE,
            "sudo's sheet's"
        );
        let id = lane.mint_id();
        lane.raised(id, t0);
        assert_eq!(lane.sheet_deadline(), Some(t0 + SHEET_FOR));
        assert_eq!(lane.expired(t0 + SHEET_FOR - Duration::from_secs(1)), None);
        assert_eq!(lane.expired(t0 + SHEET_FOR), Some(id));
        assert_eq!(lane.answered(id, Choice::NotNow), Some(Choice::NotNow));
        assert_eq!(
            (lane.sheet_deadline(), lane.expired(t0 + SHEET_FOR)),
            (None, None)
        );
        assert!(!lane.due(t0 + SHEET_FOR), "not asked again this launch");
    }

    /// A MAC CONFIGURED WHILE THE ASK WAITED (a join from a terminal) is not asked: the
    #[test]
    fn a_mac_configured_meanwhile_is_not_asked() {
        let t0 = Instant::now();
        let configured = JoinReach {
            configured_now: always,
            ..sheet_reach()
        };
        let mut lane = Lane::with_reach(configured);
        lane.on_found(found(), false, t0);
        assert!(!lane.still_asks());
        lane.end();
        assert!(!lane.due(t0) && lane.next_deadline().is_none());

        let mut unfinished = Lane::with_reach(configured);
        unfinished.on_found(found(), true, t0);
        assert!(unfinished.still_asks());
        assert!(Lane::with_reach(sheet_reach()).still_asks());
    }

    /// The launch steps hold the ask for [`LAUNCH_STEPS_FOR`] from the vault's answer at
    #[test]
    fn the_launch_steps_hold_for_a_bounded_time_and_an_inert_lane_asks_nothing() {
        let t0 = Instant::now();
        let mut lane = Lane::with_reach(sheet_reach());
        lane.on_found(found(), false, t0);
        assert!(lane.launch_steps_may_hold(t0 + LAUNCH_STEPS_FOR - Duration::from_secs(1)));
        assert!(!lane.launch_steps_may_hold(t0 + LAUNCH_STEPS_FOR));

        let mut inert = Lane::with_reach(JoinReach::inert());
        inert.on_found(found(), false, t0);
        assert!(!inert.due(t0) && inert.next_deadline().is_none());
        assert!(!Lane::default().due(t0));
        assert!(!inert.still_asks(), "the inert arm asks nothing");
        let (post, _) = recorder();
        assert!(
            !inert.run(JoinJob {
                source: aterm_secrets::ALAB_DEFAULT_URL.to_owned(),
                password: Some(SecretBytes::from(b"synthetic-test-value".as_slice())),
                post,
                config: None,
            }),
            "the inert arm runs nothing"
        );
    }

    fn recorder() -> (Post, Arc<Mutex<Vec<String>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let into = Arc::clone(&seen);
        (
            Arc::new(move |wake| into.lock().unwrap().push(format!("{wake:?}"))),
            seen,
        )
    }

    /// The sheet's buttons, by `NSAlert` response: anything but Join and Don't Ask Again
    #[test]
    fn the_sheet_answers_map_from_the_alert_response() {
        assert_eq!(Choice::of_response(1000), Choice::Join);
        assert_eq!(Choice::of_response(1001), Choice::NotNow);
        assert_eq!(Choice::of_response(1002), Choice::Never);
        assert_eq!(Choice::of_response(-1000), Choice::NotNow, "ended by aterm");
        assert_eq!(Choice::of_response(0), Choice::NotNow);
    }

    #[test]
    fn the_vault_host_is_read_off_the_default_url() {
        assert_eq!(vault_host(), "alab-secrets.local");
        assert!(aterm_secrets::ALAB_DEFAULT_URL.contains(vault_host()));
        assert!(
            aterm_secrets::ALAB_DEFAULT_URL.contains(&format!(":{}/", aterm_secrets::DEFAULT_PORT))
        );
    }

    /// THE OUTCOME WORDS pass the message rules: the band rows are an answer to the press
    #[test]
    fn the_outcome_words_pass_the_message_rules() {
        use crate::message_reporters::{Attention, attention};
        assert_eq!(attention(&joined_notice(12, true)), Ok(Attention::Answer));
        assert_eq!(attention(&joined_notice(12, false)), Ok(Attention::Answer));
        assert_eq!(
            attention(&pending_notice("ABCD2345", true)),
            Ok(Attention::Answer)
        );
        assert_eq!(
            attention(&failed_notice("no route")),
            Ok(Attention::Failure)
        );
        assert_eq!(attention(&joining_row("")), Ok(Attention::Progress));
        for msg in [
            joined_notice(1, true),
            pending_notice("ABCD2345", true),
            failed_notice("x"),
            joining_row(""),
        ] {
            assert_eq!(
                msg.key.as_deref(),
                Some(KEY_JOIN),
                "one row, restated in place"
            );
        }
        let joined = format!("{:?}", joined_notice(1, true).detail);
        assert!(joined.contains("1 secret in ~/.secrets"), "{joined}");
        assert!(joined.contains("keeps them in sync"), "{joined}");
        let unsynced = format!("{:?}", joined_notice(3, false).detail);
        assert!(unsynced.contains("3 secrets") && !unsynced.contains("in sync"));
        let pending = format!("{:?}", pending_notice("ABCD2345", true).detail);
        assert!(
            pending.contains("aterm secrets approve ABCD2345"),
            "{pending}"
        );
        let retype = "mistyped join password is fixed by running aterm secrets join";
        assert!(pending.contains(retype), "{pending}");
        let digest = format!(
            "{:?}",
            pending_entry("ABCD2345", "m99", true).message().unwrap()
        );
        assert!(
            digest.contains(&format!("A {retype}")),
            "a sentence: {digest}"
        );

        let dmg = Unsynced::Fleeting("/Volumes/aterm/aterm.app/Contents/MacOS/aterm".into());
        for (entry, cause) in [
            (pending_entry("ABCD2345", "m99", true), CAUSE_PENDING),
            (failed_entry("the vault refused"), CAUSE_FAILED),
            (unfinished_entry(), CAUSE_UNFINISHED),
            (
                unsynced_entry(&Unsynced::Failed("launchctl failed".into())),
                CAUSE_UNSYNCED,
            ),
            (unsynced_entry(&dmg), CAUSE_UNSYNCED),
        ] {
            assert_eq!(entry.key(), entry_key(cause));
            assert!(entry.key().starts_with(&Family::Privacy.prefix()));
            let msg = entry
                .message()
                .unwrap_or_else(|fault| panic!("{cause}: {fault:?}"));
            assert!(matches!(msg.need(), aterm_messages::need::Need::Later(_)));
        }
        let shown = format!(
            "{:?}",
            pending_entry("ABCD2345", "m99", true).message().unwrap()
        );
        assert!(shown.contains("aterm secrets approve ABCD2345") && shown.contains("m99"));
        let shown = format!("{:?}", unsynced_entry(&dmg).message().unwrap());
        assert!(
            shown.contains("Move aterm to Applications") && shown.contains("/Volumes/aterm"),
            "{shown}"
        );
    }

    /// A JOIN OF ANOTHER VAULT (the sheet's URL edited) is told as one: the pending answer
    #[test]
    fn a_join_of_another_vault_is_told_by_its_own_url() {
        let url = "https://vault.example/v1/x?k=private-query#private-pin";
        assert!(
            is_alab_source("") && is_alab_source(&format!(" {} ", aterm_secrets::ALAB_DEFAULT_URL))
        );
        assert!(!is_alab_source(url));
        let typed = [
            format!("{:?}", pending_notice("ABCD2345", true).detail),
            format!(
                "{:?}",
                pending_entry("ABCD2345", "m99", true).message().unwrap()
            ),
        ];
        for told in &typed {
            assert!(
                told.contains("running aterm secrets join in a terminal, adding the vault's URL if not the ALab one"),
                "{told}"
            );
        }
        let by_code = [
            format!("{:?}", pending_notice("ABCD2345", false).detail),
            format!(
                "{:?}",
                pending_entry("ABCD2345", "m99", false).message().unwrap()
            ),
        ];
        for told in &by_code {
            assert!(!told.contains("mistyped"), "{told}");
        }
        let row = joining_row(url);
        let shown = format!("{row:?}");
        assert!(
            shown.contains("asking the vault at https://vault.example/v1/x"),
            "{shown}"
        );
        assert!(!shown.contains("private"), "{shown}");
        for typed in [
            "https://t0ken-private@vault.example/v1/x",
            "https://u5er-private:pa55-private@vault.example/v1/x",
        ] {
            let shown = format!("{:?}", joining_row(typed));
            assert!(!shown.contains("private"), "{shown}");
        }
        for path in ["/Volumes/v/team.vault", "~/team.vault"] {
            let shown = format!("{:?}", joining_row(path));
            assert!(shown.contains("reading the vault file"), "{shown}");
        }
        for title in [
            row.title,
            joined_notice(1, true).title,
            pending_notice("ABCD2345", false).title,
            failed_notice("x").title,
        ] {
            assert!(!title.contains("ALab"), "{title}");
        }
        for entry in [failed_entry("x"), unfinished_entry()] {
            let shown = format!("{:?}", entry.message().unwrap());
            assert!(shown.contains("adding the vault's URL"), "{shown}");
        }
    }

    /// THE DIGEST LINES SEND THE PERSON WHERE THE REMEDY IS, AND TO NOTHING MORE. A join
    #[test]
    fn the_vault_lines_name_system_state_only_where_it_has_the_remedy() {
        use aterm_types::macos_setup::Setting;
        let words = |entry: &Entry| {
            let msg = entry
                .message()
                .unwrap_or_else(|fault| panic!("{}: {fault:?}", entry.key()));
            let aterm_messages::need::Need::Later(ask) = msg.need() else {
                panic!("{}: a digest line", entry.key());
            };
            (ask.then().words().to_string(), msg.detail[2..].to_vec())
        };
        let vault: Vec<_> = Setting::ALL
            .iter()
            .filter(|setting| setting.label().contains("vault"))
            .collect();
        assert_eq!(vault, [&Setting::Secrets]);
        let mac = Setting::ALL
            .iter()
            .take_while(|setting| setting.is_workstation())
            .count();
        assert_eq!(
            mac,
            Setting::ALL.iter().filter(|s| s.is_workstation()).count()
        );
        assert!(mac > 1);
        let span = format!(
            "from {} to {}",
            Setting::ALL[0].label(),
            Setting::ALL[mac - 1].label()
        );
        assert_eq!(
            span,
            "from Use Caps Lock as Control to Sign out of FaceTime"
        );
        for (entry, terminal) in [
            (failed_entry("the vault refused"), "aterm secrets join"),
            (unfinished_entry(), "aterm secrets join"),
            (
                unsynced_entry(&Unsynced::Failed("launchctl failed".into())),
                "aterm secrets agent on",
            ),
        ] {
            let (then, more) = words(&entry);
            assert_eq!(
                then,
                "In aterm \u{25B8} System State\u{2026}, Apply with the vault row on and the Mac settings rows off",
                "{}",
                entry.key()
            );
            assert_eq!(then, sync_in_system_state(), "the step is never cut");
            assert!(!then.contains("only"), "{then}");
            assert!(
                more.iter().any(|line| line.contains(&span)
                    && line.contains("Apply runs again")
                    && line.contains("leave the rest as they are")
                    && line.contains(Setting::Permissions.label())),
                "{more:?}"
            );
            assert!(
                more.iter()
                    .all(|line| line.chars().count() <= aterm_messages::DETAIL_LINE_CAP),
                "{more:?}"
            );
            assert!(
                more.iter()
                    .any(|line| line.starts_with("In a terminal, ") && line.contains(terminal)),
                "{more:?}"
            );
        }
        let dmg = Unsynced::Fleeting("/Volumes/aterm/aterm.app/Contents/MacOS/aterm".into());
        for entry in [pending_entry("ABCD2345", "m99", true), unsynced_entry(&dmg)] {
            let (then, _) = words(&entry);
            assert!(!then.contains("System State"), "{then}");
        }
    }

    /// The App's side on the test App (no AppKit, no proxy): the outcomes post the answer
    #[test]
    fn the_app_tells_each_outcome_and_ends_what_no_longer_holds() {
        let mut app = App::headless_for_test();
        let open = |app: &App| {
            let mut keys = app.messages.digest_keys_under("privacy.secrets.");
            keys.sort();
            keys
        };
        app.on_secrets_vault(VaultWake::Found {
            found: found(),
            unfinished: false,
        });
        assert!(
            !app.secrets_join.due(Instant::now()),
            "the test App's lane is inert"
        );

        app.on_secrets_vault(VaultWake::Joined(JoinEnd::Failed {
            why: "alab-secrets.local: no route".to_string(),
        }));
        assert_eq!(open(&app), [entry_key(CAUSE_FAILED)]);
        let row = app
            .messages
            .live_by_key(KEY_JOIN)
            .expect("the answer is on the band");
        assert_eq!(row.msg.title, "Could not join the secrets vault");

        app.on_secrets_vault(VaultWake::Configured(Settled {
            member: false,
            subscriber: Subscriber::Off,
            unfinished: true,
        }));
        assert_eq!(
            open(&app),
            [entry_key(CAUSE_FAILED), entry_key(CAUSE_UNFINISHED)]
        );

        app.on_secrets_vault(VaultWake::Joined(JoinEnd::Pending {
            code: "ABCD2345".to_string(),
            host: "m99".to_string(),
            with_password: false,
            subscriber: Some(Unsynced::Fleeting("/Volumes/aterm/aterm.app".into())),
        }));
        assert_eq!(
            open(&app),
            [entry_key(CAUSE_PENDING), entry_key(CAUSE_UNSYNCED)]
        );

        app.on_secrets_vault(VaultWake::Configured(Settled {
            member: true,
            subscriber: Subscriber::Running,
            unfinished: false,
        }));
        assert!(
            open(&app).is_empty(),
            "in, and subscribed: nothing left to do"
        );

        app.on_secrets_vault(VaultWake::Configured(Settled {
            member: true,
            subscriber: Subscriber::Missing(Unsynced::Failed("launchctl refused".into())),
            unfinished: false,
        }));
        assert_eq!(open(&app), [entry_key(CAUSE_UNSYNCED)]);

        app.on_secrets_vault(VaultWake::Joined(JoinEnd::Joined {
            names: 4,
            subscriber: None,
        }));
        assert!(open(&app).is_empty());
        let row = app
            .messages
            .live_by_key(KEY_JOIN)
            .expect("the answer is on the band");
        assert_eq!(row.msg.title, "Joined the secrets vault");
    }

    /// THE REMEDY'S APPLY ENDS THE LINES IT MENDS. A setup report (System State's status)
    #[test]
    fn a_setup_run_that_syncs_the_vault_ends_its_lines() {
        let mut app = App::headless_for_test();
        let open = |app: &App| {
            let mut keys = app.messages.digest_keys_under("privacy.secrets.");
            keys.sort();
            keys
        };
        let row = |name: &str, status: &str| crate::SetupStatusRow {
            name: name.into(),
            status: status.into(),
            detail: "injected".into(),
        };
        let report = |rows: Vec<crate::SetupStatusRow>| crate::SetupStatus {
            rows,
            ..crate::SetupStatus::default()
        };
        let raise_all = |app: &mut App| {
            app.on_secrets_vault(VaultWake::Joined(JoinEnd::Pending {
                code: "ABCD2345".into(),
                host: "m99".into(),
                with_password: false,
                subscriber: None,
            }));
            app.on_secrets_vault(VaultWake::Joined(JoinEnd::Failed {
                why: "the vault refused".into(),
            }));
            app.on_secrets_vault(VaultWake::Configured(Settled {
                member: false,
                subscriber: Subscriber::Missing(Unsynced::Failed("launchctl refused".into())),
                unfinished: true,
            }));
        };
        let raised = [
            entry_key(CAUSE_FAILED),
            entry_key(CAUSE_PENDING),
            entry_key(CAUSE_UNFINISHED),
            entry_key(CAUSE_UNSYNCED),
        ];
        raise_all(&mut app);
        assert_eq!(open(&app), raised);

        let recorded = || crate::SetupStatusRow {
            name: "Secrets vault".into(),
            status: "applied".into(),
            detail: String::new(),
        };
        for unproven in [
            report(vec![recorded()]),
            report(vec![
                row("Secrets vault", "ok"),
                row("Secrets subscriber", "blocked"),
            ]),
            report(vec![row("Secrets vault", "blocked")]),
            report(vec![
                recorded(),
                row("Secrets vault", "applied"),
                row("Secrets vault", "blocked"),
            ]),
            crate::SetupStatus {
                check: Some(crate::SetupCheck {
                    plan: aterm_types::macos_setup::Plan::default(),
                    rows: vec![
                        row("Check: Secrets vault", "ok"),
                        row("Check: Secrets subscriber", "ok"),
                    ],
                }),
                ..crate::SetupStatus::default()
            },
        ] {
            app.publish_system_state(unproven.clone());
            assert_eq!(open(&app), raised, "{unproven:?}");
        }

        let synced = report(vec![
            row("Selected Mac settings", "applied"),
            row("Secrets vault", "ok"),
        ]);
        app.publish_system_state(synced.clone());
        assert!(open(&app).is_empty(), "{:?}", open(&app));

        raise_all(&mut app);
        app.publish_system_state(synced.clone());
        assert_eq!(open(&app), raised);
        app.publish_system_state(report(Vec::new()));
        app.publish_system_state(synced);
        assert!(open(&app).is_empty(), "{:?}", open(&app));
        raise_all(&mut app);
        app.publish_system_state(report(Vec::new()));
        app.publish_system_state(report(vec![recorded(), row("Secrets vault", "applied")]));
        assert!(open(&app).is_empty(), "{:?}", open(&app));
    }

    /// A loopback vault: an admin's home that created it, the server, and its pinned URL.
    #[cfg(unix)]
    struct Vault {
        root: aterm_tempfile::TempDir,
        server: std::cell::RefCell<Option<aterm_secrets::server::Running>>,
        admin: Paths,
        pinned: String,
        password: SecretBytes,
    }

    #[cfg(unix)]
    impl Vault {
        fn new() -> Self {
            use aterm_secrets::server;
            let root = aterm_tempfile::tempdir().expect("scratch");
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
            let running = server::start(
                listener,
                &server::Options {
                    listen: "127.0.0.1:0".parse().expect("addr"),
                    store: root.path().join("store"),
                    advertise: None,
                    pins: Vec::new(),
                    hold: Duration::from_secs(50),
                    keep_awake: false,
                },
            )
            .expect("the server starts");
            let url = format!("http://{}/v1/gui", running.addr);
            let admin = Paths::for_home(&root.path().join("admin"));
            let import = admin.home.join(".secrets");
            std::fs::create_dir_all(&import).expect("admin secrets");
            let value = format!(
                "synthetic-{}",
                aterm_uds::rand::hex_token::<6>().expect("random")
            );
            std::fs::write(import.join("probe_token"), value).expect("secret");
            std::fs::set_permissions(
                import.join("probe_token"),
                std::os::unix::fs::PermissionsExt::from_mode(0o600),
            )
            .expect("chmod");
            let created = aterm_secrets::client::create(&admin, &url, Some(&import), Some("ma"))
                .expect("create");
            let password = aterm_secrets::client::admin_open(&admin)
                .expect("the admin opens its vault")
                .doc
                .join_password()
                .expect("create makes a join password");
            Self {
                pinned: format!("{url}#{}", created.signer),
                root,
                server: std::cell::RefCell::new(Some(running)),
                admin,
                password,
            }
        }

        fn home(&self, name: &str) -> Paths {
            let paths = Paths::for_home(&self.root.path().join(name));
            std::fs::create_dir_all(&paths.home).expect("home");
            paths
        }

        fn stop(&self) {
            if let Some(running) = self.server.borrow_mut().take() {
                running.stop();
            }
        }
    }

    #[cfg(unix)]
    impl Drop for Vault {
        fn drop(&mut self) {
            self.stop();
        }
    }

    /// THE JOIN THE SHEET STARTS, against a real vault on loopback (a scratch home, a test
    #[cfg(unix)]
    #[test]
    fn the_join_runs_against_a_loopback_vault() {
        let vault = Vault::new();
        let machine = vault.home("mb");
        let installs = std::cell::Cell::new(0);
        let install = || {
            installs.set(installs.get() + 1);
            Ok(())
        };
        let typed = || Some(vault.password.clone());
        let join = |paths: &Paths, password: &dyn Fn() -> Option<SecretBytes>| {
            join_vault(&JoinPlan {
                paths,
                source: &vault.pinned,
                wait: Duration::ZERO,
                password,
                install: &install,
            })
        };

        let end = join(&machine, &typed);
        assert!(
            matches!(
                &end,
                JoinEnd::Pending {
                    with_password: true,
                    subscriber: None,
                    ..
                }
            ),
            "{end:?}"
        );
        assert_eq!(installs.get(), 1, "installed once the request is queued");
        assert_eq!(
            read_configured(&machine, false).expect("read"),
            Configured::Some(Setup {
                alab: false,
                member: false,
                pending: true,
                plist: Plist::Absent,
                owed: false,
            }),
            "the recorder wrote no plist"
        );

        let approved = aterm_secrets::client::auto_approve(&vault.admin).expect("auto-approve");
        assert_eq!(approved.approved.len(), 1, "{:?}", approved.approved);
        let end = join(&machine, &typed);
        assert_eq!(
            end,
            JoinEnd::Joined {
                names: 1,
                subscriber: None
            }
        );
        assert_eq!(installs.get(), 2, "installed after a join that was in");

        let fleeting = Unsynced::Fleeting("/Volumes/aterm/aterm.app/Contents/MacOS/aterm".into());
        let other = vault.home("mc");
        let end = join_vault(&JoinPlan {
            paths: &other,
            source: &vault.pinned,
            wait: Duration::ZERO,
            password: &typed,
            install: &|| Err(fleeting.clone()),
        });
        assert!(
            matches!(&end, JoinEnd::Pending { subscriber: Some(why), .. } if *why == fleeting),
            "{end:?}"
        );

        let cut = vault.home("md");
        let cut_off = || {
            vault.stop();
            Some(vault.password.clone())
        };
        let end = join(&cut, &cut_off);
        assert!(matches!(end, JoinEnd::Failed { .. }), "{end:?}");
        let Configured::Some(read) = read_configured(&cut, false).expect("read") else {
            panic!("the join wrote its source before it failed");
        };
        assert!(plan(&read).unfinished, "{read:?}");
        assert!(!asks_again(&read), "not the ALab vault: told, not offered");
    }
}
