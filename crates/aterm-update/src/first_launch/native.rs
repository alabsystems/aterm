// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The installed image takes custody before either launch route can start setup.

use super::{CopyDisposition, Facts, FirstLaunchHost, FirstLaunchOutcome, InstalledCopy};
use crate::bundle::{Bundle, InstallPosture, ParentWrite};
use aterm_uds::spawnfd::{Child, NativeImage as _};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::ffi::OsString;
use std::io::{BufRead as _, Read as _, Write as _};
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

const PROOF_FD: &str = "ATERM_FIRST_LAUNCH_PROOF_FD";
const PROOF_NONCE: &str = "ATERM_FIRST_LAUNCH_PROOF_NONCE";
const PROOF_EXE: &str = "ATERM_FIRST_LAUNCH_PROOF_EXE";
const PROOF_WAIT: Duration = Duration::from_secs(30);

/// Whether the original waits for a command's status or leaves the installed window running.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Entry {
    /// A real window launch, before any window setup or reuse.
    Window,
    /// The CLI setup lane, including a package pass that owes setup; its terminal/status stay.
    Init,
}

/// Prepare a real launch before it may set up this Mac. `None` continues here;
/// `Some` is the installed successor's handoff/status and ends the original.
/// Its caller classifies the operation with its actual parser: queries and a headless
/// engine never enter this path. Bare development images and marked dev apps stay in place.
pub fn prepare(args: Vec<OsString>, entry: Entry) -> Result<Option<ExitCode>, String> {
    if aterm_uds::peer::our_uid() == 0 {
        return Err("run aterm as yourself, without sudo".into());
    }
    let exe = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|error| format!("resolve the running image: {error}"))?;
    let Some(source) = crate::bundle::layout_of(&exe) else {
        return if aterm_stamp::current().release_build {
            Err("aterm cannot install a bare executable: its signed app bundle is missing".into())
        } else {
            Ok(None)
        };
    };
    if crate::bundle::posture_from(&exe) == InstallPosture::Installed
        || crate::bundle::is_dev_marked(&source.app_root)
    {
        return Ok(None);
    }
    let args = if entry == Entry::Window {
        crate::window_successor_args(args.into_iter())
    } else {
        args
    };
    let host = NativeHost::open(source, args)?;
    match super::install_and_relaunch(&host) {
        FirstLaunchOutcome::ProceedInstalled => Ok(None),
        FirstLaunchOutcome::Refused { reason } => Err(reason),
        FirstLaunchOutcome::Relaunched { .. } => {
            let child = host
                .child
                .borrow_mut()
                .take()
                .ok_or("successor custody missing")?;
            let wait = entry == Entry::Init || host.fixture_waits();
            #[cfg(debug_assertions)]
            let fixture = host.fixture.clone();
            // No installed child's init holds the install lock for the duration of setup.
            drop(host);
            if wait {
                let mut child = child;
                let status = child
                    .wait()
                    .map_err(|error| format!("wait for installed aterm: {error}"))?;
                #[cfg(debug_assertions)]
                if let Some(fixture) = fixture {
                    fixture.record_exit(status.code())?;
                }
                use std::os::unix::process::ExitStatusExt as _;
                let code = status
                    .code()
                    .unwrap_or_else(|| 128 + status.signal().unwrap_or(1));
                Ok(Some(ExitCode::from(u8::try_from(code).unwrap_or(1))))
            } else {
                // A proved, committed window deliberately outlives the original image.
                drop(child);
                Ok(Some(ExitCode::SUCCESS))
            }
        }
    }
}

#[derive(Serialize, Deserialize, PartialEq, Eq, Debug)]
struct Proof {
    nonce: String,
    pid: u32,
    birth: u64,
    executable: Vec<u8>,
    arguments_digest: [u8; 32],
}

fn arguments_digest(args: &[OsString]) -> [u8; 32] {
    let mut digest = aterm_digest::Sha256::new();
    digest.update(b"aterm-first-launch-argv-v1\0");
    // Every native macOS target has a 64-bit address space. Lengths are encoded
    // independently so different argument boundaries cannot hash the same byte stream.
    digest.update((args.len() as u64).to_be_bytes());
    for argument in args {
        let bytes = argument.as_bytes();
        digest.update((bytes.len() as u64).to_be_bytes());
        digest.update(bytes);
    }
    digest.finalize()
}

/// Admit the original parent's private socket at the earliest single-threaded entry.
/// The installed child writes its own proof and waits for Commit before any setup/root work.
/// EOF, cancellation or an invalid offer ends the child without consuming a setup episode.
pub fn acknowledge_relaunch(args: &[OsString]) -> Result<(), String> {
    let fd = aterm_log::env::take(PROOF_FD);
    let nonce = aterm_log::env::take(PROOF_NONCE);
    let expected = aterm_log::env::take(PROOF_EXE);
    let (fd, nonce, expected) = match (fd, nonce, expected) {
        (None, None, None) => return Ok(()),
        (Some(fd), Some(nonce), Some(expected)) => (fd, nonce, PathBuf::from(expected)),
        _ => return Err("incomplete first-launch succession offer".into()),
    };
    let fd = fd
        .to_str()
        .and_then(|word| word.parse::<i32>().ok())
        .filter(|number| *number > 2)
        .ok_or("invalid first-launch proof descriptor")?;
    let nonce = nonce
        .into_string()
        .map_err(|_| "invalid first-launch nonce")?;
    if nonce.len() != 32 || !nonce.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("invalid first-launch nonce".into());
    }
    let exe = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|error| error.to_string())?;
    if !expected.is_absolute()
        || exe != expected
        || crate::bundle::posture_from(&exe) != InstallPosture::Installed
    {
        return Err("first-launch successor is not the expected installed image".into());
    }
    // SAFETY: this is the earliest controlled startup, before threads or descriptor reuse.
    // The native parent transfers exclusive ownership of this socket through inherit_guards;
    // the adopter refuses non-sockets and any descriptor opened close-on-exec in this image.
    let socket = unsafe { aterm_uds::spawnfd::adopt_inherited_socket(fd) }
        .map_err(|error| error.to_string())?;
    let mut socket = UnixStream::from(socket);
    socket
        .set_read_timeout(Some(PROOF_WAIT))
        .map_err(|error| error.to_string())?;
    socket
        .set_write_timeout(Some(PROOF_WAIT))
        .map_err(|error| error.to_string())?;
    let pid = std::process::id();
    let birth = aterm_uds::process::read_birth(pid as i32)
        .ok_or("successor birth unreadable")?
        .0;
    let proof = Proof {
        nonce,
        pid,
        birth,
        executable: exe.as_os_str().as_bytes().to_vec(),
        arguments_digest: arguments_digest(args),
    };
    let line = aterm_json::to_vec(&proof).map_err(|error| error.to_string())?;
    #[cfg(debug_assertions)]
    if let Some(fixture) = Fixture::read(&exe)? {
        fixture.record("first-launch-ran.json", &line)?;
        // Outside the bounded wire protocol, this owned debug fixture observes
        // the child's actual OS words independently of the parent's digest.
        fixture.record(
            "first-launch-arguments.json",
            &aterm_json::to_vec(
                &args
                    .iter()
                    .map(|arg| arg.as_bytes().to_vec())
                    .collect::<Vec<_>>(),
            )
            .map_err(|error| error.to_string())?,
        )?;
    }
    socket
        .write_all(&line)
        .and_then(|()| socket.write_all(b"\n"))
        .map_err(|error| error.to_string())?;
    let mut commit = [0];
    socket
        .read_exact(&mut commit)
        .map_err(|error| format!("first-launch parent did not commit: {error}"))?;
    if commit != *b"G" {
        return Err("first-launch parent refused succession".into());
    }
    Ok(())
}

struct NativeHost {
    source: Bundle,
    args: Vec<OsString>,
    facts: Facts,
    _lock: aterm_update_core::FileLock,
    source_build: u64,
    initial_destination: Option<DestinationIdentity>,
    child: RefCell<Option<Child>>,
    #[cfg(debug_assertions)]
    fixture: Option<Fixture>,
}

#[derive(PartialEq, Eq)]
struct DestinationIdentity {
    directory: FileIdentity,
    executable: Option<FileIdentity>,
    plist: Option<FileIdentity>,
}

type FileIdentity = (u64, u64, u32, u64, i64, i64);

fn file_identity(path: &Path) -> Result<Option<FileIdentity>, String> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => Ok(Some((
            meta.dev(),
            meta.ino(),
            meta.uid(),
            meta.len(),
            meta.mtime(),
            meta.mtime_nsec(),
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("read app identity at {}: {error}", path.display())),
    }
}

fn destination_identity(app: &Path) -> Result<Option<DestinationIdentity>, String> {
    let Some(directory) = file_identity(app)? else {
        return Ok(None);
    };
    let metadata = std::fs::symlink_metadata(app).map_err(|error| error.to_string())?;
    let real_directory = metadata.is_dir() && !metadata.file_type().is_symlink();
    Ok(Some(DestinationIdentity {
        directory,
        executable: if real_directory {
            file_identity(&app.join("Contents/MacOS/aterm"))?
        } else {
            None
        },
        plist: if real_directory {
            file_identity(&app.join("Contents/Info.plist"))?
        } else {
            None
        },
    }))
}

impl NativeHost {
    fn open(source: Bundle, args: Vec<OsString>) -> Result<Self, String> {
        #[cfg(debug_assertions)]
        let fixture = Fixture::read(&source.exe)?;
        let defaults = (
            PathBuf::from("/Applications/aterm.app"),
            aterm_types::dirs::home_dir().map(|home| home.join("Applications/aterm.app")),
        );
        #[cfg(debug_assertions)]
        let (applications_dest, home_dest) = fixture
            .as_ref()
            .map(Fixture::destinations)
            .unwrap_or(defaults);
        #[cfg(not(debug_assertions))]
        let (applications_dest, home_dest) = defaults;
        // Validate the original too; a runnable bundle alone does not establish the team pin.
        #[cfg(debug_assertions)]
        if let Some(fixture) = &fixture {
            fixture.verify(&source.app_root)?;
        } else {
            crate::verify::verify_bundle_policy(&source.app_root, crate::effective_team_id())?;
        }
        #[cfg(not(debug_assertions))]
        crate::verify::verify_bundle_policy(&source.app_root, crate::effective_team_id())?;
        let staging =
            crate::paths::Staging::resolve().ok_or("cannot resolve private update custody")?;
        let lock = aterm_update_core::FileLock::acquire_within(
            &staging.apply_lock,
            crate::install::APPLY_LOCK_WAIT,
        )
        .map_err(|error| format!("first-launch update custody: {error}"))?;
        let running_build = crate::verify::bundle_build_number(&source.app_root)?;
        let mut host = Self {
            facts: Facts {
                posture: crate::bundle::posture_from(&source.exe),
                can_write_applications: false,
                applications_dest,
                home_dest,
                installed: None,
            },
            source,
            args,
            _lock: lock,
            source_build: running_build,
            initial_destination: None,
            child: RefCell::new(None),
            #[cfg(debug_assertions)]
            fixture,
        };
        // Opening a verified installed app needs no installation destination. Inspect both
        // normal locations before any write probe or directory creation for a new copy.
        for candidate in [&host.facts.applications_dest]
            .into_iter()
            .chain(host.facts.home_dest.as_ref())
        {
            let metadata = match std::fs::symlink_metadata(candidate) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(format!("inspect installed aterm: {error}")),
            };
            if metadata.is_dir()
                && !metadata.file_type().is_symlink()
                && !crate::bundle::is_dev_marked(candidate)
                && candidate
                    .canonicalize()
                    .is_ok_and(|canonical| canonical == *candidate)
                && crate::verify::bundle_build_number(candidate)
                    .is_ok_and(|build| build >= running_build)
                && host.verify(candidate).is_ok()
            {
                host.facts.installed = Some(InstalledCopy {
                    app_root: candidate.clone(),
                    at_least_running: true,
                    verified: Ok(()),
                });
                break;
            }
        }
        if host.facts.installed.is_some() {
            return Ok(host);
        }
        let mut can_write_applications =
            match crate::bundle::probe_parent_write(&host.facts.applications_dest) {
                ParentWrite::Writable => true,
                ParentWrite::NotWritable(_) => false,
                ParentWrite::Passing(reason) => return Err(reason),
            };
        if let Ok(metadata) = std::fs::symlink_metadata(&host.facts.applications_dest)
            && (metadata.uid() != aterm_uds::peer::our_uid()
                || (metadata.is_dir()
                    && !metadata.file_type().is_symlink()
                    && crate::bundle::is_dev_marked(&host.facts.applications_dest)))
        {
            can_write_applications = false;
        }
        host.facts.can_write_applications = can_write_applications;
        let dest = host
            .facts
            .install_dest()
            .ok_or("no writable Applications folder")?;
        let parent = dest.parent().ok_or("installation has no parent folder")?;
        #[cfg(debug_assertions)]
        if host.fixture.is_some() {
            aterm_update_core::ensure_private_dir(parent).map_err(|error| error.to_string())?;
        } else {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("create Applications folder: {error}"))?;
        }
        #[cfg(not(debug_assertions))]
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("create Applications folder: {error}"))?;
        if parent.canonicalize().map_err(|error| error.to_string())? != parent {
            return Err("Applications folder must have a canonical path without symlinks".into());
        }
        host.initial_destination = destination_identity(&dest)?;
        match std::fs::symlink_metadata(&dest) {
            Ok(metadata) => {
                if metadata.is_dir()
                    && !metadata.file_type().is_symlink()
                    && crate::bundle::is_dev_marked(&dest)
                {
                    return Err(
                        "installation destination is a development app; leaving it untouched"
                            .into(),
                    );
                }
                let verified = if metadata.is_dir() && !metadata.file_type().is_symlink() {
                    host.verify(&dest)
                } else {
                    Err("installed name is not a real app directory".into())
                };
                let at_least_running = verified.is_ok()
                    && crate::verify::bundle_build_number(&dest)
                        .is_ok_and(|build| build >= running_build);
                let copy = InstalledCopy {
                    app_root: dest,
                    at_least_running,
                    verified,
                };
                if !copy.at_least_running && metadata.uid() != aterm_uds::peer::our_uid() {
                    return Err(
                        "installation destination belongs to another account; leaving it untouched"
                            .into(),
                    );
                }
                host.facts.installed = Some(copy);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("inspect installation destination: {error}")),
        }
        Ok(host)
    }

    fn fixture_waits(&self) -> bool {
        #[cfg(debug_assertions)]
        {
            self.fixture.is_some()
        }
        #[cfg(not(debug_assertions))]
        {
            false
        }
    }
}

impl FirstLaunchHost for NativeHost {
    fn facts(&self) -> Facts {
        self.facts.clone()
    }

    fn verify(&self, app: &Path) -> Result<(), String> {
        #[cfg(debug_assertions)]
        if let Some(fixture) = &self.fixture {
            return fixture.verify(app);
        }
        crate::verify::verify_bundle_policy(app, crate::effective_team_id())
    }

    fn install_copy(&self, dest: &Path, replace: Option<&Path>) -> Result<CopyDisposition, String> {
        let parent = dest.parent().ok_or("installation has no parent folder")?;
        let staged = aterm_tempfile::Builder::new()
            .prefix(".aterm-first-launch-")
            .tempdir_in(parent)
            .map_err(|error| error.to_string())?;
        let candidate = staged.path().join("aterm.app");
        if let Err(error) = aterm_tempfile::clone_tree(&self.source.app_root, &candidate) {
            if !matches!(error.raw_os_error(), Some(libc::EXDEV | libc::ENOTSUP)) {
                return Err(format!("clone running app: {error}"));
            }
            if let Err(reason) =
                crate::install::copy_service_image(&self.source.app_root, &candidate)
            {
                let kept = staged.keep();
                return Err(format!(
                    "{reason}; copy custody retained at {}",
                    kept.display()
                ));
            }
        }
        self.verify(&candidate)?;
        let current = destination_identity(dest)?;
        if let Some(current) = &current {
            let metadata = std::fs::symlink_metadata(dest).map_err(|error| error.to_string())?;
            let real_directory = metadata.is_dir() && !metadata.file_type().is_symlink();
            if real_directory && crate::bundle::is_dev_marked(dest) {
                return Err(
                    "installation destination became a development app; leaving it untouched"
                        .into(),
                );
            }
            if real_directory
                && self.verify(dest).is_ok()
                && crate::verify::bundle_build_number(dest)
                    .is_ok_and(|build| build >= self.source_build)
            {
                if destination_identity(dest)?.as_ref() != Some(current) {
                    return Err("installed copy changed during its final verification".into());
                }
                return Ok(CopyDisposition::UseExisting);
            }
            if metadata.uid() != aterm_uds::peer::our_uid() {
                return Err(
                    "installation destination changed account ownership; leaving it untouched"
                        .into(),
                );
            }
        }
        if current != self.initial_destination || destination_identity(dest)? != current {
            return Err("installation destination changed while the candidate was prepared; leaving it untouched".into());
        }
        if let Some(replace) = replace {
            if replace != dest {
                return Err("replacement does not name the installation destination".into());
            }
            let metadata = std::fs::symlink_metadata(dest).map_err(|error| error.to_string())?;
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                crate::install::checked_bundle_exchange(
                    &candidate,
                    dest,
                    "install verified first-launch bundle",
                    aterm_durable::Flush::Durable,
                )?;
                if let Err(reason) = self.move_aside(&candidate) {
                    let kept = staged.keep();
                    crate::warn(&format!(
                        "{reason}; predecessor retained at {}",
                        kept.display()
                    ));
                }
                return Ok(CopyDisposition::Installed);
            }
            self.move_aside(dest)?;
        }
        crate::sys::rename_exclusive(&candidate, dest)
            .map_err(|error| format!("promote verified first-launch bundle: {error}"))?;
        let flushed = aterm_durable::Flush::Durable
            .dir_best_effort(parent)
            .map_err(|error| error.to_string())?;
        if let aterm_durable::DirFlushed::Unavailable(error) = flushed {
            crate::warn(&format!(
                "first-launch app installed; directory flush unavailable: {error}"
            ));
        }
        Ok(CopyDisposition::Installed)
    }

    fn move_aside(&self, app: &Path) -> Result<PathBuf, String> {
        let parent = self
            .facts
            .install_dest()
            .and_then(|dest| dest.parent().map(Path::to_path_buf))
            .ok_or("installation parent missing")?;
        let aside = parent.join(format!(
            "aterm (previous {}).app",
            aterm_uds::rand::hex_token::<16>().map_err(|error| error.to_string())?
        ));
        crate::sys::rename_exclusive(app, &aside)
            .map_err(|error| format!("retain previous app: {error}"))?;
        Ok(aside)
    }

    fn clear_quarantine(&self, app: &Path) -> Result<(), String> {
        crate::verify::status_bounded_with_stderr_observed(
            aterm_uds::spawnfd::command("/usr/bin/xattr")
                .args(["-dr", "com.apple.quarantine"])
                .arg(app),
            "clear verified app quarantine",
            PROOF_WAIT,
        )
        .map_err(|failure| failure.message)
        .and_then(|(status, stderr)| {
            if status.success() {
                Ok(())
            } else {
                Err(format!("clear verified app quarantine: {stderr}"))
            }
        })
    }

    fn relaunch(&self, app: &Path) -> Result<(), String> {
        // Both initial OpenInstalled and a late UseExisting take the same fresh admission.
        // The apply lock excludes the cooperating updater; identity catches another publisher.
        let before = destination_identity(app)?.ok_or("installed successor disappeared")?;
        let metadata = std::fs::symlink_metadata(app).map_err(|error| error.to_string())?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || crate::bundle::is_dev_marked(app)
        {
            return Err("installed successor is not an eligible app directory".into());
        }
        self.verify(app)?;
        if crate::verify::bundle_build_number(app)? < self.source_build {
            return Err("installed successor became older than the running copy".into());
        }
        if destination_identity(app)?.as_ref() != Some(&before) {
            return Err("installed successor changed during its final verification".into());
        }
        let executable = app
            .join("Contents/MacOS/aterm")
            .canonicalize()
            .map_err(|error| error.to_string())?;
        if !executable.starts_with(app.canonicalize().map_err(|error| error.to_string())?) {
            return Err("installed executable escapes its bundle".into());
        }
        let nonce = aterm_uds::rand::hex_token::<16>().map_err(|error| error.to_string())?;
        let (mut parent, successor) = UnixStream::pair().map_err(|error| error.to_string())?;
        parent
            .set_read_timeout(Some(PROOF_WAIT))
            .map_err(|error| error.to_string())?;
        parent
            .set_write_timeout(Some(PROOF_WAIT))
            .map_err(|error| error.to_string())?;
        let mut command = aterm_uds::spawnfd::command(&executable);
        command.args(&self.args).native_image(true);
        command
            .inherit_guards(vec![OwnedFd::from(successor)])
            .map_err(|error| error.to_string())?;
        let descriptor = command
            .guard_targets()
            .next()
            .ok_or("proof socket inheritance missing")?;
        command
            .env(PROOF_FD, descriptor.to_string())
            .env(PROOF_NONCE, &nonce)
            .env(PROOF_EXE, &executable);
        let mut pending = Unconfirmed(Some(command.spawn().map_err(|error| error.to_string())?));
        drop(command); // The parent must not retain the child's socket endpoint.
        let child = pending.0.as_ref().ok_or("successor custody missing")?;
        let expected = Proof {
            nonce,
            pid: child.id(),
            birth: aterm_uds::process::read_birth(child.id() as i32)
                .ok_or("successor birth unreadable")?
                .0,
            executable: executable.as_os_str().as_bytes().to_vec(),
            arguments_digest: arguments_digest(&self.args),
        };
        let expected_line = aterm_json::to_vec(&expected).map_err(|error| error.to_string())?;
        // The valid child's proof is determined by our owned child and launch words.
        // Bound the read by that exact serialization plus its newline, not by argv size.
        let bound = expected_line
            .len()
            .checked_add(1)
            .ok_or("proof size overflow")?;
        let mut line = Vec::new();
        std::io::BufReader::new((&mut parent).take(bound as u64))
            .read_until(b'\n', &mut line)
            .map_err(|error| format!("read installed successor proof: {error}"))?;
        if line.last() != Some(&b'\n') {
            return Err("installed successor did not write a complete proof".into());
        }
        let proof: Proof = aterm_json::from_slice(&line)
            .map_err(|error| format!("invalid installed successor proof: {error}"))?;
        if proof != expected {
            return Err("installed successor proof did not match its native child custody".into());
        }
        #[cfg(debug_assertions)]
        if let Some(fixture) = &self.fixture {
            fixture.record(
                "first-launch-receipt.json",
                &aterm_json::to_vec(&proof).map_err(|error| error.to_string())?,
            )?;
        }
        parent
            .write_all(b"G")
            .map_err(|error| format!("commit installed successor: {error}"))?;
        *self.child.borrow_mut() = pending.0.take();
        Ok(())
    }
}

struct Unconfirmed(Option<Child>);
impl Drop for Unconfirmed {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            aterm_uds::deadline::kill_within(child, Duration::from_secs(1));
        }
    }
}

// The only alternate policy is compiled out of every release build. It accepts
// structurally signed lab images solely inside this canonical, owner-only fixture.
#[cfg(debug_assertions)]
#[derive(Clone)]
struct Fixture {
    root: PathBuf,
}

#[cfg(debug_assertions)]
impl Fixture {
    fn read(executable: &Path) -> Result<Option<Self>, String> {
        let Some(root) = std::env::var_os("ATERM_DEBUG_FIRST_LAUNCH_ROOT") else {
            return Ok(None);
        };
        if !aterm_types::dirs::runs_under_private_state_root() {
            return Err("first-launch fixture requires private state".into());
        }
        let root = PathBuf::from(root);
        if !root.is_absolute() || root.canonicalize().map_err(|error| error.to_string())? != root {
            return Err("first-launch fixture root must be canonical".into());
        }
        aterm_uds::private_file::check_private_dir(&root).map_err(|error| error.to_string())?;
        let fixture = Self { root };
        for relative in ["Applications", "home", "state"] {
            fixture.private_directory(&fixture.root.join(relative))?;
        }
        let home_applications = fixture.root.join("home/Applications");
        match std::fs::symlink_metadata(&home_applications) {
            Ok(_) => fixture.private_directory(&home_applications)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
        if std::env::var_os("HOME").map(PathBuf::from) != Some(fixture.root.join("home")) {
            return Err("first-launch fixture HOME escapes its root".into());
        }
        if let Some(updates) = std::env::var_os("ATERM_UPDATE_ROOT").filter(|path| !path.is_empty())
            && Path::new(&updates) != fixture.root.join("state")
        {
            return Err("first-launch fixture update custody escapes its root".into());
        }
        let permitted = [
            fixture
                .root
                .join("AppTranslocation/source/aterm.app/Contents/MacOS/aterm"),
            fixture
                .root
                .join("Applications/aterm.app/Contents/MacOS/aterm"),
            fixture
                .root
                .join("home/Applications/aterm.app/Contents/MacOS/aterm"),
        ];
        if !permitted.iter().any(|path| path == executable) || !fixture.owns(executable)? {
            return Err("first-launch fixture image escapes its owned source/destination".into());
        }
        let state = PathBuf::from(
            std::env::var_os("ATERM_STATE_HOME").ok_or("fixture state root missing")?,
        );
        if state != fixture.root.join("state")
            || state.canonicalize().map_err(|error| error.to_string())?
                != fixture.root.join("state")
        {
            return Err("first-launch fixture state escapes its root".into());
        }
        Ok(Some(fixture))
    }

    fn owns(&self, path: &Path) -> Result<bool, String> {
        let canonical = path.canonicalize().map_err(|error| error.to_string())?;
        Ok(canonical == path && canonical.starts_with(&self.root))
    }

    fn private_directory(&self, path: &Path) -> Result<(), String> {
        let metadata = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || path.canonicalize().map_err(|error| error.to_string())? != path
        {
            return Err("first-launch fixture directory escapes its canonical root".into());
        }
        aterm_uds::private_file::check_private_dir(path).map_err(|error| error.to_string())
    }

    fn destinations(&self) -> (PathBuf, Option<PathBuf>) {
        (
            self.root.join("Applications/aterm.app"),
            Some(self.root.join("home/Applications/aterm.app")),
        )
    }

    fn verify(&self, app: &Path) -> Result<(), String> {
        if !self.owns(app)? {
            return Err("fixture verification escapes its canonical root".into());
        }
        crate::verify::verify_bundle_policy(app, "")
    }

    fn record(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        aterm_uds::atomic_file::write(&self.root.join(name), bytes, 0o600)
            .map_err(|error| error.to_string())
    }

    fn record_exit(&self, code: Option<i32>) -> Result<(), String> {
        self.record(
            "first-launch-exit.json",
            &aterm_json::to_vec(&code).map_err(|error| error.to_string())?,
        )
    }
}

#[cfg(all(test, debug_assertions))]
mod tests {
    use super::*;

    fn signed_bundle(root: &Path, relative: &str, build: u64) -> Bundle {
        let app_root = root.join(relative);
        let exe = app_root.join("Contents/MacOS/aterm");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        let stand_in = aterm_uds::deadline::stand_in(Path::new("/bin/echo"));
        aterm_tempfile::clone_tree(&stand_in, &exe).unwrap();
        std::fs::write(
            app_root.join("Contents/Info.plist"),
            format!(
                "<?xml version=\"1.0\"?><plist version=\"1.0\"><dict>\
             <key>CFBundleIdentifier</key><string>com.aterm.first-launch-producer</string>\
             <key>CFBundleExecutable</key><string>aterm</string>\
             <key>CFBundlePackageType</key><string>APPL</string>\
             <key>CFBundleVersion</key><string>{build}</string></dict></plist>"
            ),
        )
        .unwrap();
        let signed = aterm_uds::deadline::output_within(
            aterm_uds::spawnfd::command("/usr/bin/codesign")
                .args(["--force", "--sign", "-"])
                .arg(&app_root),
            aterm_spec::wait::MUST_HAPPEN,
        )
        .unwrap();
        assert!(signed.status.success(), "{signed:?}");
        Bundle { app_root, exe }
    }

    /// A real publisher exchanges newer B after facts named older A. The native
    /// preparation must preserve B and return the existing-copy relaunch disposition.
    #[test]
    fn a_newer_destination_published_after_facts_is_preserved_for_relaunch() {
        let root = aterm_tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let source = signed_bundle(&root_path, "AppTranslocation/source/aterm.app", 10);
        let old = signed_bundle(&root_path, "Applications/aterm.app", 9);
        let newer = signed_bundle(&root_path, "newer.app", 11);
        let initial_destination = destination_identity(&old.app_root).unwrap();
        let staging = crate::paths::Staging::scratch("first-launch-newer-publisher");
        let lock = aterm_update_core::FileLock::acquire_within(&staging.apply_lock, Duration::ZERO)
            .unwrap();
        let mut host = NativeHost {
            source,
            args: Vec::new(),
            facts: Facts {
                posture: InstallPosture::Translocated,
                can_write_applications: true,
                applications_dest: old.app_root.clone(),
                home_dest: None,
                installed: Some(InstalledCopy {
                    app_root: old.app_root.clone(),
                    at_least_running: false,
                    verified: Ok(()),
                }),
            },
            _lock: lock,
            source_build: 10,
            initial_destination,
            child: RefCell::new(None),
            fixture: Some(Fixture { root: root_path }),
        };
        crate::install::checked_bundle_exchange(
            &newer.app_root,
            &old.app_root,
            "test concurrent publisher",
            aterm_durable::Flush::Durable,
        )
        .unwrap();
        let published = destination_identity(&old.app_root).unwrap();
        assert!(matches!(
            host.install_copy(&old.app_root, Some(&old.app_root))
                .unwrap(),
            CopyDisposition::UseExisting
        ));
        assert!(
            destination_identity(&old.app_root).unwrap() == published,
            "native preparation replaced the newer published image"
        );
        assert_eq!(
            crate::verify::bundle_build_number(&old.app_root).unwrap(),
            11
        );
        assert!(
            host.child.borrow().is_none(),
            "installation preparation executes no image"
        );
        // The other admission route selected B in its facts. A publisher can
        // still replace its path before dispatch; a signed older image must not run.
        host.facts.installed = Some(InstalledCopy {
            app_root: old.app_root.clone(),
            at_least_running: true,
            verified: Ok(()),
        });
        crate::install::checked_bundle_exchange(
            &newer.app_root,
            &old.app_root,
            "test stale existing-copy selection",
            aterm_durable::Flush::Durable,
        )
        .unwrap();
        let outcome = super::super::install_and_relaunch(&host);
        if !matches!(&outcome, FirstLaunchOutcome::Refused { reason } if reason.contains("became older"))
        {
            let kept = root.keep();
            panic!(
                "stale existing copy reached dispatch; image custody retained at {}: {outcome:?}",
                kept.display()
            );
        }
        assert!(host.child.borrow().is_none());
    }
}
