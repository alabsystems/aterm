// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The root LAN broker's LaunchDaemon, installed, verified and repaired by setup's one elevation
//! (`docs/DESIGN-lan-broker-2026-11-05.md` §3, §5).
//!
//! As root, [`LanBroker`] clones the app bundle the elevation runs from (the **source**: the
//! `.app` around the root half's canonical executable, which runs as
//! `<app>/Contents/MacOS/aterm machines here apply`) into a root-only staging folder, seals the
//! clone `root:wheel`, strips shared grants, confines its links, and strictly checks it against
//! the loaded root process's designated requirement before swapping it over
//! [`aterm_uds::lan::IMAGE_BUNDLE`] (removing the image it replaced), writes
//! [`aterm_uds::lan::PLIST_PATH`] (`0644 root:wheel`, [`aterm_uds::lan::install::plist_text`]),
//! and (re)starts the job with `launchctl bootout`/`bootstrap`/`kickstart`
//! ([`super::aterm::start_system_job`], which [`super::aterm`]'s vault server starts its job with
//! too, after the `launchctl enable` its own baseline asks for). launchd runs only the clone,
//! never the person-owned bundle.
//!
//! A job launchd lists switched off ([`switched_off`]: its background item turned off in Login
//! Items, or `launchctl disable`) is the person's: the item does not apply to it, and nothing here
//! runs `launchctl enable`, so no run undoes that choice. Putting it back is theirs.
//!
//! The clone is APFS `clonefile(2)`: `/Applications` and `/Library` are one volume, so it takes
//! no space. An app kept on another volume cannot be cloned there (`EXDEV`), and Apple's
//! `ditto` copies it instead ([`crate::port::Cloned::Copied`], which the row's detail says).
//!
//! The apply runs no aterm process: the clone and its seal are system calls in the root half,
//! and its only children are `launchctl` (and `ditto` for a copy), so the elevation's count of
//! root aterm runs (`aterm-lab`'s `mac_nux::root_aterm_runs`) is unchanged by it. The broker
//! launchd starts is a root aterm too, which process accounting records each time it exits (an
//! adoption's restart, a re-elevation's bootout, a crash): the lane takes those out by launchd's
//! own count of the job's ended runs (`mac_nux::broker_runs_ended`, its `runs` less the one
//! running, read as accounting starts and as it stops), so its count stays one per root half.

use super::{Ctx, Item};
use crate::model::{Effect, Probe};
use crate::port::{Cloned, FileId, Scratch, System};
use aterm_keeper::identity::CheckError;
use aterm_types::macos_setup::refused::Failure;
use aterm_uds::app_image;
use aterm_uds::lan::{self, adoption, install};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

/// The `[lan]` table.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Table {
    /// `true` installs the root LAN broker from the app bundle this aterm runs from
    /// ([`LanBroker`]).
    pub broker: Option<bool>,
    /// The built-in root transaction may refresh unadmitted snapshots after operational
    /// staging failures. Ordinary baselines return those failures without rebasing their source.
    #[serde(skip)]
    pub(crate) retained_root_run: bool,
}

/// The items `table` asks for.
///
/// # Errors
/// Never today; the signature is the family contract.
pub fn expand(table: &Table, ctx: &Ctx) -> Result<Vec<Box<dyn Item>>, String> {
    let _ = ctx;
    let mut items: Vec<Box<dyn Item>> = Vec::new();
    if table.broker == Some(true) {
        items.push(Box::new(LanBroker {
            retained_root_run: table.retained_root_run,
            ..LanBroker::default()
        }));
    }
    Ok(items)
}

/// Where the broker's log goes, as a folder.
fn log_dir() -> &'static Path {
    Path::new(lan::LOG_PATH)
        .parent()
        .unwrap_or_else(|| Path::new("/Library/Logs"))
}

/// Why a development build installs no broker.
const NO_BUNDLE: &str = "this aterm runs outside an app bundle (a development build), and the \
                         broker runs only a root-owned clone of an app bundle: an installed \
                         aterm.app's `sudo aterm machines here apply --plain` installs it";

/// [`install::Files`] read through a [`System`], so a test's fake files are what is read: a
/// file is stat'ed first, so a stat that fails reads as that failure.
pub(crate) struct Through<'a>(pub &'a dyn System);

impl install::Files for Through<'_> {
    fn read(&self, path: &Path) -> std::io::Result<Vec<u8>> {
        self.0.stat(path)?;
        self.0.read_file(path)
    }

    fn code_directories(&self, path: &Path) -> std::io::Result<install::CodeDirectories> {
        self.0.code_directories(path)
    }

    fn vm_guest(&self) -> Option<bool> {
        self.0.vm_guest()
    }
}

/// Why a development build owns no broker on a real Mac ([`install::Part::Development`]).
const DEVELOPMENT: &str = "a development build (no team signature) runs the LAN broker only in a \
                           VM guest; a release's elevation installs it on this Mac";

/// The broker's install as `sys` reads it for the bundle it runs from ([`install::read`]): `None`
/// off macOS or outside a bundle. One reading decides both what is asked for and what an
/// elevation installs: a development build reads [`install::Part::Development`] outside a VM
/// guest, and an older build of the broker's signer reads a newer image
/// [`install::Part::Kept`].
#[must_use]
pub fn reading(sys: &dyn System) -> Option<Result<install::Reading, String>> {
    if !sys.macos() {
        return None;
    }
    let source = sys.running_app()?;
    Some(install::read(&Through(sys), &source))
}

/// The account's side of the broker's adoption ([`adoption::Expecting`]), read through `sys`:
/// its record in [`System::account_state_dir`] (none for root, whose elevation renews the image
/// itself), the installed broker's hello of any protocol ([`System::installed_lan_broker`]: a
/// broker an update's new protocol finds outdated still names its build; a direct path's is not
/// the broker's), root's own refusal file, and `aterm_log` for the journal.
struct Expecting<'a> {
    sys: &'a dyn System,
    record: Option<PathBuf>,
}

impl<'a> Expecting<'a> {
    fn of(sys: &'a dyn System) -> Self {
        let state = if sys.euid() == 0 {
            None
        } else {
            sys.account_state_dir()
        };
        Self {
            sys,
            record: state.map(|dir| dir.join(adoption::RECORD)),
        }
    }
}

impl adoption::Expecting for Expecting<'_> {
    fn now(&self) -> u64 {
        self.sys.now()
    }

    fn record(&self) -> Option<std::io::Result<Vec<u8>>> {
        self.record.as_ref().map(|path| {
            let mut bytes = self.sys.read_file(path)?;
            bytes.truncate(adoption::RECORD_MAX);
            Ok(bytes)
        })
    }

    fn keep(&self, bytes: &[u8]) -> std::io::Result<()> {
        let path = self
            .record
            .as_ref()
            .ok_or(std::io::ErrorKind::Unsupported)?;
        self.sys.write_file(path, bytes, 0o600)
    }

    fn hello(&self) -> Option<lan::BrokerHello> {
        self.sys
            .installed_lan_broker()
            .ok()
            .filter(|hello| hello.direct.is_none())
    }

    fn refusal(&self, path: &Path) -> std::io::Result<Vec<u8>> {
        self.sys
            .read_owned_file(path, 0, adoption::REFUSAL_MAX, false)
    }

    fn journal(&self, line: &str) {
        aterm_log::info!("{line}");
    }
}

/// [`reading`], with an adopting image judged against what the broker it hears serves
/// ([`adoption::expect`]): [`install::Part::NotAdopted`] where the broker still serves another
/// build than this one [`adoption::LOOKS`] of its looks after this account first saw it, owed
/// with no ask of its own ([`crate::root_steps::Owed::LanBrokerAdoption`]). The broker is asked
/// for its hello only where the image reads adopting or the account's record holds a pair, and
/// an adopting image is asked once to consider this bundle ([`install::consider_for`]).
#[must_use]
pub fn expected_reading(sys: &dyn System) -> Option<Result<install::Reading, String>> {
    let read = reading(sys)?;
    let (Ok(found), Some(source)) = (&read, sys.running_app()) else {
        return Some(read);
    };
    install::consider_for(found);
    Some(Ok(adoption::expect(
        &Expecting::of(sys),
        found.clone(),
        &source,
        lan::hello_build(),
    )))
}

/// The broker's own reason for not adopting the bundle `sys` runs from, from root's records
/// ([`lan::ADOPTION_REFUSED`], or the bundle's own of [`lan::ADOPTION_REFUSED_ASKERS`],
/// [`adoption::refusals_of`]): `None` where there is none, it is not root's own file, or it is
/// another bundle's.
pub(crate) fn adoption_refusal(sys: &dyn System) -> Option<String> {
    let source = sys.running_app()?;
    adoption::refusals_of(&Expecting::of(sys), &source)
        .into_iter()
        .next()
        .map(|refusal| refusal.reason)
}

/// The key of a broker install from the bundle `sys` runs from, as root's record of a refused
/// apply holds it and an account reads it (`aterm_types::macos_setup::refused::broker_key`): the
/// bundle and the code of its executable on disk. `None` outside a bundle or where that code
/// cannot be read.
pub(crate) fn refusal_key(sys: &dyn System) -> Option<String> {
    let source = sys.running_app()?;
    let code = sys
        .code_directories(&source.join(install::BUNDLE_EXECUTABLE))
        .ok()?;
    aterm_types::macos_setup::refused::broker_key(&source, &code)
}

/// `[lan] broker = true`: the root LAN broker installed from the app bundle the elevation runs
/// from (module docs). Applied as root, by the elevation's root half.
#[derive(Default)]
pub struct LanBroker {
    /// How the last apply made the image, for the row's detail.
    made: Mutex<Option<Cloned>>,
    retained_root_run: bool,
    unfinished: Mutex<Option<crate::init_clt::Unfinished>>,
}

fn io(what: &Path, e: &std::io::Error) -> String {
    format!("{}: {e}", what.display())
}

enum Readiness {
    Ready(u32),
    Start,
    Pending(String),
}

/// Replacement freshness for this transaction, never a signature verdict.
struct Publication<'a> {
    code: install::CodeDirectories,
    plist: Vec<u8>,
    prior_plist: Option<FileId>,
    complete: bool,
    retired: Option<Scratch<'a>>,
}

struct Prepared<'a> {
    owner: Scratch<'a>,
    path: PathBuf,
    code: install::CodeDirectories,
}

/// An installed-file generation, including a proved absence. This is replacement
/// freshness only: a malformed old image can still be repaired.
#[derive(PartialEq, Eq)]
struct Before {
    image: Option<FileId>,
    plist: Option<FileId>,
}

/// How a broker apply ended short, and what root's record of it says
/// (`aterm_types::macos_setup::refused`, `failure`). [`Failure::Definite`] is a failure taken as
/// the step's own, one that would recur with the same bundle: the strict check or the clone
/// definitely refused it, or every round of the run published the job and it did not serve
/// (launchd refusing it, or not answering about it, each time). [`Failure::NoVerdict`] is a
/// retained run that used up its rounds with no serve where at least one round ended without a
/// verdict (a check unanswered or not run, file I/O, a source that changed): that may be the
/// Mac's passing state, so it stalls the step only once it comes again later in the same boot.
/// `None` says nothing of the bundle: a stop, lost custody, a job switched off, or a run that is
/// not retained and so ended at its first such error.
struct Failed {
    why: String,
    failure: Option<Failure>,
}

impl Failed {
    fn passing(why: impl Into<String>) -> Self {
        Self {
            why: why.into(),
            failure: None,
        }
    }
}

enum StageError {
    Files(app_image::Error),
    Check(CheckError),
    SourceChanged,
    Lost(String),
    /// launchd lists the job switched off ([`switched_off`]), so it is not launched.
    SwitchedOff,
    /// The image is a newer build's this build keeps ([`install::Part::Kept`]): nothing is
    /// published over it ([`kept_job`]), and where launchd cannot say whether it holds its job,
    /// nothing is loaded either ([`Installing::load_kept`]).
    Kept(String),
}

impl std::fmt::Display for StageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Files(error) => error.fmt(f),
            Self::Check(error) => error.fmt(f),
            Self::SourceChanged => f.write_str("the source changed before image publication"),
            Self::Lost(why) => f.write_str(why),
            Self::SwitchedOff => lan::Unavailable::SwitchedOff.fmt(f),
            Self::Kept(why) => f.write_str(why),
        }
    }
}

impl From<app_image::Error> for StageError {
    fn from(error: app_image::Error) -> Self {
        Self::Files(error)
    }
}

fn stage_io(path: &Path, error: std::io::Error) -> StageError {
    app_image::Error::Io(std::io::Error::new(error.kind(), io(path, &error))).into()
}

fn file_generation(sys: &dyn System, path: &Path) -> Result<Option<FileId>, StageError> {
    match sys.stat(path) {
        Ok(_) => sys
            .file_id(path)
            .map(Some)
            .map_err(|error| stage_io(path, error)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(stage_io(path, error)),
    }
}

impl Before {
    fn read(sys: &dyn System) -> Result<Self, StageError> {
        Ok(Self {
            image: file_generation(sys, Path::new(lan::IMAGE_BUNDLE))?,
            plist: file_generation(sys, Path::new(lan::PLIST_PATH))?,
        })
    }

    fn check(&self, sys: &dyn System) -> Result<(), StageError> {
        if Self::read(sys)? != *self {
            return Err(StageError::Lost(
                "another root transaction replaced the LAN broker installation".into(),
            ));
        }
        Ok(())
    }
}

impl Prepared<'_> {
    fn source_current(&self, sys: &dyn System, source: &Path) -> Result<(), StageError> {
        let shape = sys.stat(source).map_err(|error| stage_io(source, error))?;
        if !shape.dir || shape.symlink {
            return Err(
                app_image::Error::Refused("the source is not a real app directory".into()).into(),
            );
        }
        let exe = source.join(install::BUNDLE_EXECUTABLE);
        let code = sys
            .code_directories(&exe)
            .map_err(|error| stage_io(&exe, error))?;
        if code != self.code {
            return Err(StageError::SourceChanged);
        }
        Ok(())
    }
}

impl Publication<'_> {
    fn check_image(&self, sys: &dyn System) -> Result<(), StageError> {
        let image = Path::new(lan::IMAGE_EXECUTABLE);
        if sys
            .code_directories(image)
            .map_err(|error| stage_io(image, error))?
            != self.code
        {
            return Err(StageError::Lost(
                "another root transaction replaced the LAN broker image".into(),
            ));
        }
        Ok(())
    }

    fn check(&self, sys: &dyn System) -> Result<(), StageError> {
        self.check_image(sys)?;
        let path = Path::new(lan::PLIST_PATH);
        if sys.read_file(path).map_err(|error| stage_io(path, error))? != self.plist {
            return Err(StageError::Lost(
                "another root transaction replaced the LAN broker job".into(),
            ));
        }
        Ok(())
    }

    fn finish(&mut self, sys: &dyn System) -> Result<(), StageError> {
        if self.complete {
            return Ok(());
        }
        self.check_image(sys)?;
        let plist = Path::new(lan::PLIST_PATH);
        // An earlier write may have landed before its error. Only our intended
        // bytes or the original generation may be completed, never a foreign job.
        if file_generation(sys, plist)? != self.prior_plist {
            let shape = sys.stat(plist).map_err(|error| stage_io(plist, error))?;
            let ours = !shape.dir
                && !shape.symlink
                && sys.read_file(plist).is_ok_and(|bytes| bytes == self.plist);
            if !ours {
                return Err(StageError::Lost(
                    "another root transaction replaced the LAN broker job".into(),
                ));
            }
        }
        let log = log_dir();
        sys.create_dir_all(log, 0o755)
            .map_err(|error| stage_io(log, error))?;
        if let Some(parent) = plist.parent() {
            sys.create_dir_all(parent, 0o755)
                .map_err(|error| stage_io(parent, error))?;
        }
        sys.write_file(plist, &self.plist, 0o644)
            .map_err(|error| stage_io(plist, error))?;
        sys.chown(plist, 0, 0)
            .map_err(|error| stage_io(plist, error))?;
        self.check(sys)?;
        self.complete = true;
        if let Some(retired) = &mut self.retired {
            if let Err(error) = retired.close() {
                aterm_log::warn!("LAN broker retired image cleanup: {error}");
            } else {
                self.retired = None;
            }
        }
        Ok(())
    }
}

/// Local custody of one accepted root run, not saved authorization. Only a
/// successful strict check moves an owned candidate to the publication phase.
#[derive(Default)]
struct Installing<'a> {
    before: Option<Before>,
    prepared: Option<Prepared<'a>>,
    published: Option<Publication<'a>>,
    launched: bool,
}

impl<'a> Installing<'a> {
    fn round(
        &mut self,
        item: &LanBroker,
        sys: &'a dyn System,
        source: &Path,
        current: &dyn Fn() -> Result<(), String>,
    ) -> Result<Option<String>, StageError> {
        current().map_err(StageError::Lost)?;
        if self.published.is_none()
            && let Ok(reading) = install::read(&Through(sys), source)
            && let install::Part::Kept { image, ours, .. } = reading.image
        {
            return self.load_kept(sys, source, image, ours, current);
        }
        if self.published.is_none() {
            if self.before.is_none() {
                self.before = Some(Before::read(sys)?);
            }
            let before = self.before.as_ref().expect("read installed generations");
            before.check(sys)?;
            if self.prepared.is_none() {
                self.prepared = Some(item.prepare(sys, source, current)?);
            }
            let prepared = self.prepared.as_ref().expect("owned candidate");
            current().map_err(StageError::Lost)?;
            before.check(sys)?;
            prepared.source_current(sys, source)?;
            sys.check_app_copy(&prepared.path)
                .map_err(StageError::Check)?;
            current().map_err(StageError::Lost)?;
            prepared.source_current(sys, source)?;
            before.check(sys)?;
            // The probe read the image; the effect reads it again: nothing publishes this build's
            // image over a newer one launchd holds.
            if let Some(why) = kept_job(sys, source) {
                return Err(StageError::Kept(why));
            }
            let prepared = self.prepared.take().expect("strictly checked candidate");
            self.published = Some(item.publish(sys, source, prepared, before)?);
        }
        let published = self.published.as_mut().expect("an owned publication");
        current().map_err(StageError::Lost)?;
        published.finish(sys)?;
        current().map_err(StageError::Lost)?;
        published.check(sys)?;
        // Publishing a new image still owes its initial launch: an older process
        // with a compatible protocol cannot complete this transaction before that.
        let observed = if self.launched {
            readiness(sys)
        } else {
            Readiness::Start
        };
        current().map_err(StageError::Lost)?;
        published.check(sys)?;
        let why = match observed {
            Readiness::Ready(_) => return Ok(None),
            Readiness::Pending(why) if self.launched => why,
            Readiness::Start | Readiness::Pending(_) => {
                current().map_err(StageError::Lost)?;
                published.check(sys)?;
                // The probe read no switch-off; a person may switch it off while this run holds
                // root.
                if switched_off(sys) == Ok(true) {
                    return Err(StageError::SwitchedOff);
                }
                match super::aterm::start_system_job(sys, Path::new(lan::PLIST_PATH), lan::LABEL) {
                    Ok(()) => {
                        self.launched = true;
                        current().map_err(StageError::Lost)?;
                        published.check(sys)?;
                        let observed = readiness(sys);
                        current().map_err(StageError::Lost)?;
                        published.check(sys)?;
                        match observed {
                            Readiness::Ready(_) => return Ok(None),
                            Readiness::Start => "the broker job has not started yet".into(),
                            Readiness::Pending(why) => why,
                        }
                    }
                    Err(why) => why,
                }
            }
        };
        Ok(Some(why))
    }

    /// Load in place the image of build `image` this build (`ours`) keeps
    /// ([`install::Part::Kept`]), publishing nothing over it: where launchd holds its job, running
    /// or between two of its starts, it is ready as it is; where launchd holds none, the plist a
    /// build wrote stays (the newer build's, whatever it says), this build's is written only where
    /// none is (an exclusive create, so a newer build's written since stays), and the job is
    /// loaded and started without a `bootout` ([`super::aterm::load_system_job`]), so a newer
    /// build's own restart of it is never taken down. An older build that published its own image
    /// here would move the broker down.
    fn load_kept(
        &mut self,
        sys: &dyn System,
        source: &Path,
        image: u64,
        ours: u64,
        current: &dyn Fn() -> Result<(), String>,
    ) -> Result<Option<String>, StageError> {
        let held = super::aterm::system_job_pid(sys, lan::LABEL);
        match held {
            Ok(Some(_)) => return Ok(None),
            Ok(None) if self.launched => {
                return Ok(Some("the broker job has not started yet".into()));
            }
            Ok(None) => {}
            Err(why) => {
                return Err(StageError::Kept(format!(
                    "the broker's image is build {image}, newer than this build's {ours}, and \
                     launchd could not say whether it holds its job ({why}): this build's \
                     elevation leaves it as it is"
                )));
            }
        }
        // The probe read no switch-off; a person may switch it off while this run holds root.
        if switched_off(sys) == Ok(true) {
            return Err(StageError::SwitchedOff);
        }
        current().map_err(StageError::Lost)?;
        let plist = Path::new(lan::PLIST_PATH);
        if let Some(parent) = plist.parent() {
            sys.create_dir_all(parent, 0o755)
                .map_err(|error| stage_io(parent, error))?;
        }
        let log = log_dir();
        sys.create_dir_all(log, 0o755)
            .map_err(|error| stage_io(log, error))?;
        match sys.create_new(plist, install::plist_text(source).as_bytes(), 0o644) {
            Ok(()) => sys
                .chown(plist, 0, 0)
                .map_err(|error| stage_io(plist, error))?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(stage_io(plist, error)),
        }
        current().map_err(StageError::Lost)?;
        let why = match super::aterm::load_system_job(sys, plist, lan::LABEL) {
            Ok(()) => {
                self.launched = true;
                match super::aterm::system_job_pid(sys, lan::LABEL) {
                    Ok(Some(_)) => return Ok(None),
                    Ok(None) => "the broker job has not started yet".to_owned(),
                    Err(why) => why,
                }
            }
            Err(why) => why,
        };
        Ok(Some(why))
    }

    fn retry(&mut self, error: &StageError) -> bool {
        match error {
            StageError::Check(CheckError::Refused(_))
            | StageError::Files(app_image::Error::Refused(_))
            | StageError::Lost(_)
            | StageError::SwitchedOff
            | StageError::Kept(_) => false,
            StageError::Check(CheckError::Unanswered(_)) => true,
            StageError::Check(CheckError::Unchecked(_))
            | StageError::Files(app_image::Error::Io(_))
            | StageError::SourceChanged => {
                // A resource-only source repair can keep the executable unchanged.
                // Refresh the unadmitted snapshot rather than caching a negative.
                self.prepared = None;
                true
            }
        }
    }
}

/// A loaded pid alone is not proof that the broker's protocol is serving.
fn readiness(sys: &dyn System) -> Readiness {
    let pid = match super::aterm::system_job_pid(sys, lan::LABEL) {
        Ok(Some(Some(pid))) => pid,
        Ok(_) => return Readiness::Start,
        Err(why) => return Readiness::Pending(why),
    };
    match sys.installed_lan_broker() {
        Ok(hello) if hello.direct.is_some() => {
            Readiness::Pending("the installed root broker has not answered this protocol".into())
        }
        Ok(hello) if hello.protocol != lan::PROTOCOL => Readiness::Pending(
            lan::Unavailable::Outdated {
                ours: lan::PROTOCOL,
                theirs: hello.protocol,
                build: hello.build,
            }
            .to_string(),
        ),
        Ok(_) => Readiness::Ready(pid),
        Err(why) => Readiness::Pending(why),
    }
}

/// Full effect-boundary readiness for a root result that otherwise looks short.
/// The GUI's ordinary `reading` remains metadata-only. A job launchd lists switched off is the
/// person's, and leaves nothing for the result to wait on.
///
/// A development build's broker is none of its own ([`install::Part::Development`]): ready. A
/// newer image this build keeps ([`install::Part::Kept`]) is ready while launchd holds its job,
/// with or without a pid (between two of its starts), whatever protocol it answers.
pub(crate) fn ready(sys: &dyn System) -> Result<bool, String> {
    let Some(files) = reading(sys) else {
        return Ok(true);
    };
    let files = files?;
    if files.image == install::Part::Development {
        return Ok(true);
    }
    let serving = match files.image {
        install::Part::Kept { .. } => {
            matches!(super::aterm::system_job_pid(sys, lan::LABEL), Ok(Some(_)))
        }
        _ => matches!(readiness(sys), Readiness::Ready(_)),
    };
    Ok((files.in_place() && serving) || switched_off(sys) == Ok(true))
}

/// What launchd holds of the broker's job ([`aterm_uds::launchd::JobState`], from
/// `launchctl print`): `None` where `launchctl` could not say.
pub(crate) fn job_state(sys: &dyn System) -> Option<aterm_uds::launchd::JobState> {
    use aterm_uds::launchd::JobState;
    super::aterm::system_job_pid(sys, lan::LABEL)
        .ok()
        .map(|job| match job {
            Some(Some(_)) => JobState::Running,
            Some(None) => JobState::Idle,
            None => JobState::Unloaded,
        })
}

/// Why nothing may be published over the image, read again for the bundle `source` this build
/// runs from just before it would be: it is a newer build's this build keeps
/// ([`install::Part::Kept`]), whatever launchd holds of its job (one it holds none of is loaded in
/// place, [`Installing::load_kept`]), or the image cannot be read. `None` where the image is no
/// such one.
fn kept_job(sys: &dyn System, source: &Path) -> Option<String> {
    let reading = match install::read(&Through(sys), source) {
        Ok(reading) => reading,
        Err(why) => return Some(format!("the broker's image could not be read again: {why}")),
    };
    let install::Part::Kept { image, ours, .. } = reading.image else {
        return None;
    };
    Some(format!(
        "the broker's image is build {image}, newer than this build's {ours}: this build's \
         elevation never replaces it with an older one"
    ))
}

/// Whether launchd lists the broker's job switched off (`launchctl print-disabled system`,
/// [`aterm_uds::launchd::job_switch_in`]): its background item turned off in Login Items, or
/// `launchctl disable`. That choice is the person's, and stays theirs. A job with no row is not
/// switched off: the broker's plist sets no `Disabled` key.
///
/// # Errors
/// `launchctl` could not be asked, failed, or printed no readable switch for the job, in one
/// line.
pub(crate) fn switched_off(sys: &dyn System) -> Result<bool, String> {
    use aterm_uds::launchd::{JobSwitch, job_switch_in};
    let cmd = super::person::print_disabled_cmd();
    match sys.run(&cmd) {
        Ok(ran) if ran.success() => job_switch_in(&ran.out(), lan::LABEL)
            .map(|switch| switch == JobSwitch::Off)
            .map_err(|why| format!("{}: {why}", cmd.display())),
        Ok(ran) => Err(format!("{}: {}", cmd.display(), ran.describe())),
        Err(e) => Err(format!("{}: {e}", cmd.display())),
    }
}

/// The most rounds one root run makes to publish, launch and reach the broker before it gives up
/// with the last round's reason (launchd's own words where launchd refused the job): with
/// [`aterm_secrets::converge::RunnerRetry`]'s pauses between them, about two and a half minutes.
const ROUNDS: u32 = 6;

impl LanBroker {
    fn note(&self, why: Option<crate::init_clt::Unfinished>) {
        *self
            .unfinished
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = why;
    }

    /// Keep the authenticated root process through publication and strict readiness.
    /// Launch and protocol I/O retain their existing per-call deadlines. The retry rate is
    /// capped, and at most [`ROUNDS`] rounds run: a launch launchd still refuses, or a broker
    /// still not serving, then ends the transaction with that round's reason.
    /// Stop, lost custody or a job switched off ends the transaction.
    fn apply_owned(&self, sys: &dyn System, source: &Path) -> Result<(), Failed> {
        use aterm_secrets::converge::{Last, RunnerRetry};
        let pid = std::process::id();
        let start = sys
            .process_started(pid)
            .filter(|start| *start != 0)
            .ok_or_else(|| {
                Failed::passing("the accepted root process's generation could not be read")
            })?;
        let _stops = sys.defer_stops();
        let current = || -> Result<(), String> {
            if sys.stopped().is_some() {
                self.note(Some(crate::init_clt::Unfinished::Stopped));
                return Err("the accepted root setup was stopped".into());
            }
            if sys.euid() != 0 || sys.process_started(pid) != Some(start) {
                return Err("the accepted root setup no longer owns its process generation".into());
            }
            Ok(())
        };
        current().map_err(Failed::passing)?;
        let mut installing = Installing::default();
        let mut retry = RunnerRetry::default();
        let clock = std::time::Instant::now();
        let mut said = None;
        let mut rounds = 0_u32;
        // The rounds that published the image and job and saw the job not serve.
        let mut unserved = 0_u32;
        loop {
            let why = match installing.round(self, sys, source, &current) {
                Ok(None) => return Ok(()),
                Ok(Some(why)) => {
                    unserved += 1;
                    why
                }
                Err(error) => {
                    if !self.retained_root_run || !installing.retry(&error) {
                        return Err(Failed {
                            failure: matches!(
                                error,
                                StageError::Check(CheckError::Refused(_))
                                    | StageError::Files(app_image::Error::Refused(_))
                            )
                            .then_some(Failure::Definite),
                            why: error.to_string(),
                        });
                    }
                    error.to_string()
                }
            };
            rounds += 1;
            if rounds >= ROUNDS {
                return Err(Failed {
                    why: format!(
                        "the LAN broker is not serving after {rounds} attempts in this root run, \
                         the last: {why}; `sudo aterm machines here apply --plain` tries again"
                    ),
                    failure: Some(if unserved == rounds {
                        Failure::Definite
                    } else {
                        Failure::NoVerdict
                    }),
                });
            }
            if said.as_ref() != Some(&why) {
                aterm_log::warn!("LAN broker: {why}; retaining this root transaction for recovery");
                said = Some(why);
            }
            // RunnerRetry supplies the existing capped rate. The wait is local
            // to this accepted process, with no saved authorization or new timer.
            let elapsed = || u64::try_from(clock.elapsed().as_millis()).unwrap_or(u64::MAX);
            retry.reaped(Last::Failed, elapsed());
            let mut left = retry
                .remaining(elapsed())
                .expect("failed retry has a deadline");
            while !left.is_zero() {
                current().map_err(Failed::passing)?;
                let tick = left.min(Duration::from_secs(2));
                sys.sleep(tick);
                left = left.saturating_sub(tick);
            }
            retry.started();
        }
    }

    fn prepare<'a>(
        &self,
        sys: &'a dyn System,
        source: &Path,
        current: &dyn Fn() -> Result<(), String>,
    ) -> Result<Prepared<'a>, StageError> {
        current().map_err(StageError::Lost)?;
        let image_dir = Path::new(lan::IMAGE_DIR);
        // Every folder on the image's way is root's with no shared write, so no
        // other account can move the image or put another in its place.
        for dir in image_dir
            .ancestors()
            .take(2)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            sys.create_dir_all(dir, 0o755)
                .map_err(|error| stage_io(dir, error))?;
            sys.root_own(dir, false)?;
        }
        let owner = Scratch::new_in(sys, image_dir, "lan-image")
            .map_err(|error| stage_io(image_dir, error))?;
        sys.root_own(owner.path(), false)?;
        let staged = owner.path().join("aterm.app");
        current().map_err(StageError::Lost)?;
        let made = sys.clone_tree(source, &staged)?;
        *self
            .made
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(made);
        current().map_err(StageError::Lost)?;
        sys.root_own(&staged, true)?;
        let staged_exe = staged.join(install::BUNDLE_EXECUTABLE);
        Ok(Prepared {
            owner,
            path: staged,
            code: sys
                .code_directories(&staged_exe)
                .map_err(|error| stage_io(&staged_exe, error))?,
        })
    }

    fn publish<'a>(
        &self,
        sys: &'a dyn System,
        source: &Path,
        prepared: Prepared<'a>,
        before: &Before,
    ) -> Result<Publication<'a>, StageError> {
        let Prepared { owner, path, code } = prepared;
        let plist = install::plist_text(source).into_bytes();
        let image = Path::new(lan::IMAGE_BUNDLE);
        match sys.exchange(&path, image) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                sys.rename(&path, image)
                    .map_err(|error| stage_io(image, error))?;
            }
            Err(error) => return Err(stage_io(image, error)),
        }
        // No fallible operation follows the image rename before custody moves
        // into Publication. A later plist/readiness retry never clones it again.
        Ok(Publication {
            code,
            plist,
            prior_plist: before.plist,
            complete: false,
            retired: Some(owner),
        })
    }
}

impl Item for LanBroker {
    fn id(&self) -> String {
        "lan.broker".to_owned()
    }

    fn family(&self) -> &'static str {
        "lan"
    }

    fn title(&self) -> String {
        "the LAN broker, so aterm reaches this network's Macs without a macOS question".to_owned()
    }

    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        if !sys.macos() {
            return Probe::NotApplicable {
                why: "only macOS asks about local network access, so only a Mac runs the broker"
                    .to_owned(),
            };
        }
        let Some(source) = sys.running_app() else {
            return Probe::NotApplicable {
                why: NO_BUNDLE.to_owned(),
            };
        };
        let reading = match install::read(&Through(sys), &source) {
            Ok(reading) => reading,
            Err(why) => return Probe::Unusable { why },
        };
        if reading.image == install::Part::Development {
            return Probe::NotApplicable {
                why: DEVELOPMENT.to_owned(),
            };
        }
        let to = format!(
            "{} running in system from a root-owned clone of {}",
            lan::LABEL,
            source.display()
        );
        // A newer build's image this build keeps stays, whatever its hello says and whatever its
        // plist (the newer build's): ready while launchd holds its job, running or between two of
        // launchd's starts. A `launchctl` that cannot say measures nothing. A job launchd does not
        // hold at all is loaded in place ([`Installing::load_kept`]): this build's elevation never
        // replaces a newer image with its own.
        if let install::Part::Kept { image, ours, .. } = &reading.image {
            let kept = |held: String| Probe::Ok {
                now: format!(
                    "{} {held} in system from build {image}, newer than this build's {ours}",
                    lan::LABEL
                ),
            };
            match super::aterm::system_job_pid(sys, lan::LABEL) {
                Ok(Some(Some(pid))) => return kept(format!("running (pid {pid})")),
                Ok(Some(None)) => return kept("loaded, which launchd starts again,".to_owned()),
                Err(why) => {
                    return Probe::Unusable {
                        why: format!(
                            "launchd could not say whether the broker of build {image}, newer than \
                             this build's {ours}, runs: {why}"
                        ),
                    };
                }
                Ok(None) => {
                    if switched_off(sys) == Ok(true) {
                        return Probe::NotApplicable {
                            why: lan::Unavailable::SwitchedOff.to_string(),
                        };
                    }
                    return Probe::Change {
                        from: format!(
                            "build {image}'s image, newer than this build's {ours}, which launchd \
                             does not hold"
                        ),
                        to: format!(
                            "{} running in system from build {image}'s image, loaded in place",
                            lan::LABEL
                        ),
                    };
                }
            }
        }
        let from = if reading.current() {
            match readiness(sys) {
                Readiness::Ready(pid) => {
                    return Probe::Ok {
                        now: format!("{} running in system (pid {pid})", lan::LABEL),
                    };
                }
                Readiness::Start => "installed, not serving".into(),
                Readiness::Pending(why) => why,
            }
        } else {
            reading.words()
        };
        // A change would boot out, replace or start the job: never one the person switched off.
        // Where launchd cannot say, the change stands, and the launch never enables the job.
        if switched_off(sys) == Ok(true) {
            return Probe::NotApplicable {
                why: lan::Unavailable::SwitchedOff.to_string(),
            };
        }
        Probe::Change { from, to }
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        self.note(Some(crate::init_clt::Unfinished::Other));
        use aterm_types::macos_setup::refused::LAN_BROKER;
        let source = sys.running_app().ok_or(NO_BUNDLE)?;
        // The build the apply begins with is the one a later ask would read again.
        let key = refusal_key(sys);
        match self.apply_owned(sys, &source) {
            Ok(()) => {
                self.note(None);
                crate::root_steps::clear_refusal(sys, LAN_BROKER);
                // Where the image is now the source, the broker's record of not adopting it is
                // over; a newer image loaded in place is no source's.
                if install::read(&Through(sys), &source)
                    .is_ok_and(|reading| reading.image == install::Part::Current)
                {
                    crate::root_steps::clear_refusal(sys, lan::ADOPTION_REFUSED);
                }
                Ok(())
            }
            Err(failed) => {
                if let (Some(failure), Some(key)) = (failed.failure, key) {
                    crate::root_steps::record_refusal(sys, LAN_BROKER, &key, failure);
                }
                Err(failed.why)
            }
        }
    }

    fn unfinished(&self) -> Option<crate::init_clt::Unfinished> {
        *self
            .unfinished
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn revert(&self) -> Option<String> {
        Some(format!(
            "{} disable system/{} && ({} bootout system/{}; /bin/rm -f {}; /bin/rm -rf {})",
            super::sharing::LAUNCHCTL,
            lan::LABEL,
            super::sharing::LAUNCHCTL,
            lan::LABEL,
            crate::port::shell_word(lan::PLIST_PATH),
            crate::port::shell_word(lan::IMAGE_BUNDLE),
        ))
    }

    fn effect(&self) -> Effect {
        Effect::Now
    }

    fn detail(&self, _sys: &dyn System, _ctx: &Ctx) -> Option<String> {
        match *self
            .made
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            Some(Cloned::Copied) => Some(
                "the app is on another volume than /Library, so its image is a full copy, not a \
                 clone"
                    .to_owned(),
            ),
            _ => None,
        }
    }
}

#[cfg(test)]
#[path = "lan_tests.rs"]
mod tests;
