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
//! and (re)starts the job with `launchctl bootout`/`enable`/`bootstrap`/`kickstart`, as
//! [`super::aterm`]'s vault server does. launchd runs only the clone, never the person-owned
//! bundle.
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
use aterm_uds::app_image;
use aterm_uds::lan::{self, install};
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
}

/// The broker's install as `sys` reads it for the bundle it runs from: `None` off macOS or
/// outside a bundle, else [`install::read`]'s answer.
#[must_use]
pub fn reading(sys: &dyn System) -> Option<Result<install::Reading, String>> {
    if !sys.macos() {
        return None;
    }
    let source = sys.running_app()?;
    Some(install::read(&Through(sys), &source))
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

enum StageError {
    Files(app_image::Error),
    Check(CheckError),
    SourceChanged,
    Lost(String),
}

impl std::fmt::Display for StageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Files(error) => error.fmt(f),
            Self::Check(error) => error.fmt(f),
            Self::SourceChanged => f.write_str("the source changed before image publication"),
            Self::Lost(why) => f.write_str(why),
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

    fn retry(&mut self, error: &StageError) -> bool {
        match error {
            StageError::Check(CheckError::Refused(_))
            | StageError::Files(app_image::Error::Refused(_))
            | StageError::Lost(_) => false,
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
        Ok(hello) if hello.direct.is_some() || hello.protocol != lan::PROTOCOL => {
            Readiness::Pending("the installed root broker has not answered this protocol".into())
        }
        Ok(_) => Readiness::Ready(pid),
        Err(why) => Readiness::Pending(why),
    }
}

/// Full effect-boundary readiness for a root result that otherwise looks short.
/// The GUI's ordinary `reading` remains metadata-only.
pub(crate) fn ready(sys: &dyn System) -> Result<bool, String> {
    let Some(files) = reading(sys) else {
        return Ok(true);
    };
    Ok(files?.current() && matches!(readiness(sys), Readiness::Ready(_)))
}

impl LanBroker {
    fn note(&self, why: Option<crate::init_clt::Unfinished>) {
        *self
            .unfinished
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = why;
    }

    /// Keep the authenticated root process through publication and strict readiness.
    /// Launch and protocol I/O retain their existing per-call deadlines. The retry rate is
    /// capped, with no total wait limit.
    /// Stop or lost custody ends the transaction.
    fn apply_owned(&self, sys: &dyn System, source: &Path) -> Result<(), String> {
        use aterm_secrets::converge::{Last, RunnerRetry};
        let pid = std::process::id();
        let start = sys
            .process_started(pid)
            .filter(|start| *start != 0)
            .ok_or("the accepted root process's generation could not be read")?;
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
        current()?;
        let mut installing = Installing::default();
        let mut retry = RunnerRetry::default();
        let clock = std::time::Instant::now();
        let mut said = None;
        loop {
            let why = match installing.round(self, sys, source, &current) {
                Ok(None) => return Ok(()),
                Ok(Some(why)) => why,
                Err(error) => {
                    if !self.retained_root_run || !installing.retry(&error) {
                        return Err(error.to_string());
                    }
                    error.to_string()
                }
            };
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
                current()?;
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
        let to = format!(
            "{} running in system from a root-owned clone of {}",
            lan::LABEL,
            source.display()
        );
        if !reading.current() {
            return Probe::Change {
                from: reading.words(),
                to,
            };
        }
        match readiness(sys) {
            Readiness::Ready(pid) => Probe::Ok {
                now: format!("{} running in system (pid {pid})", lan::LABEL),
            },
            Readiness::Start => Probe::Change {
                from: "installed, not serving".into(),
                to,
            },
            Readiness::Pending(why) => Probe::Change { from: why, to },
        }
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        self.note(Some(crate::init_clt::Unfinished::Other));
        let source = sys.running_app().ok_or(NO_BUNDLE)?;
        let result = self.apply_owned(sys, &source);
        if result.is_ok() {
            self.note(None);
        }
        result
    }

    fn unfinished(&self) -> Option<crate::init_clt::Unfinished> {
        *self
            .unfinished
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn revert(&self) -> Option<String> {
        Some(format!(
            "{} bootout system/{}; /bin/rm -f {}; /bin/rm -rf {}",
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
