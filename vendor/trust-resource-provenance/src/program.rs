// SPDX-License-Identifier: Apache-2.0 OR MIT
// Copyright 2026 Andrew Yates

//! Held native program/runtime inputs for a bounded metadata executor.
//!
//! This module owns original read-only files and directory observations. It
//! creates no process, resource grant, source/writer authentication or proof.
//! A custodian must retain it from before suspended birth through actual scope
//! retirement, check the kernel-loaded executable before GO, then validate the
//! same backing before accepting output. The OS loader/system shared cache is
//! an explicit TCB input; this is no kernel or system-library attestation.
//! Path-based runtime loading additionally assumes no concurrent same-UID name
//! writers. Outer ancestors retain identity/owner/ACL checks, not whole-directory
//! timestamps: unrelated sibling activity is not a selected-input change. This
//! is not an atomic namespace/DLL snapshot and cannot replace a stronger image
//! or authenticated compile/test validator.

use std::collections::BTreeMap;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use std::collections::BTreeSet;
use std::ffi::{CString, OsStr, OsString};
use std::fs::{File, Metadata};
use std::io;
use std::os::fd::{AsRawFd as _, FromRawFd as _};
use std::os::unix::ffi::OsStrExt as _;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use std::os::unix::fs::FileExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path, PathBuf};
use std::time::Instant;

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use sha2::{Digest as _, Sha256};

use crate::private::{identity, stamp};
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use crate::schema::hex;
use crate::schema::{FileId, refused};
use crate::{process, require_no_acl_grants};

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const MAX_IMAGE: u64 = 1024 * 1024 * 1024;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const MAX_TOTAL: u64 = 4 * 1024 * 1024 * 1024;
#[cfg(any(test, all(target_os = "macos", target_arch = "aarch64")))]
const MAX_COMMAND_BYTES: usize = 1024 * 1024;
#[cfg(any(test, all(target_os = "macos", target_arch = "aarch64")))]
const MAX_COMMANDS: usize = 4096;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const MAX_RUNTIME: usize = 128;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const MAX_DIRECTORIES: usize = 256;
const MAX_COMPONENTS: usize = 64;
const MAX_PATH: usize = 4096;

pub const RUNTIME_POLICY: &str = "native-metadata/macho-arm64-sysroot-rpath-v1";
pub const INPUT_SCOPE: &str =
    "held-load-command-input-observations/local-same-uid-no-name-writers/system-runtime-tcb";

/// Data from original opened bytes. It is neither a loaded-image capability
/// nor a source, writer, resource, build or proof claim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileObservation {
    pub path: PathBuf,
    pub sha256: String,
    pub bytes: u64,
    pub device: u64,
    pub inode: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProgramObservation {
    pub policy: &'static str,
    pub scope: &'static str,
    pub executable: FileObservation,
    pub runtime: Vec<FileObservation>,
    pub loader_paths: Vec<PathBuf>,
    pub system_dependencies: Vec<String>,
    pub host: String,
    pub boot: String,
}

struct HeldDirectory {
    file: File,
    before: Metadata,
    selected: bool,
}

fn same_directory(before: &Metadata, current: &Metadata, selected: bool) -> bool {
    if selected {
        stamp(before) == stamp(current)
    } else {
        // Outer ancestors can gain unrelated siblings while this probe holds
        // its selected inputs. Their identity/permissions must stay original;
        // this is deliberately no namespace-ABA or atomic snapshot claim.
        identity(before) == identity(current)
            && before.mode() == current.mode()
            && before.uid() == current.uid()
            && before.gid() == current.gid()
    }
}
struct HeldFile {
    file: File,
    before: Metadata,
    observation: FileObservation,
}

/// Opaque live ownership of one original executable and complete supported
/// non-system runtime closure. No metadata record can reconstruct this value.
pub struct HeldProgram {
    root: PathBuf,
    executable: HeldFile,
    runtime: Vec<HeldFile>,
    directories: BTreeMap<PathBuf, HeldDirectory>,
    observation: ProgramObservation,
}

mod system_command;
pub use system_command::HeldSystemCommand;

fn time(deadline: Instant) -> io::Result<()> {
    if Instant::now() >= deadline { Err(io::ErrorKind::TimedOut.into()) } else { Ok(()) }
}

fn canonical(path: &Path) -> io::Result<()> {
    let bytes = path.as_os_str().as_bytes();
    if bytes.len() > MAX_PATH || !path.is_absolute() || bytes.contains(&0) {
        return Err(refused("native input path is not bounded absolute syntax"));
    }
    let parts = path.components().collect::<Vec<_>>();
    if parts.len() > MAX_COMPONENTS
        || parts.iter().any(|part| !matches!(part, Component::RootDir | Component::Normal(_)))
        || path.as_os_str() != parts.iter().collect::<PathBuf>().as_os_str()
    {
        return Err(refused("native input path is not canonical component syntax"));
    }
    Ok(())
}

fn owned(file: &File, directory: bool, executable: bool) -> io::Result<Metadata> {
    let meta = file.metadata()?;
    let uid = unsafe { libc::geteuid() };
    if !matches!(meta.uid(), 0) && meta.uid() != uid {
        return Err(refused("native input belongs to another account"));
    }
    if meta.mode() & 0o7022 != 0
        || if directory { !meta.is_dir() } else { !meta.is_file() || meta.nlink() == 0 }
        || executable && meta.mode() & 0o111 == 0
    {
        return Err(refused("native input has unsupported kind or writable/special mode"));
    }
    require_no_acl_grants(file)?;
    Ok(meta)
}

fn openat(parent: i32, name: &OsStr, directory: bool) -> io::Result<File> {
    let name = CString::new(name.as_bytes()).map_err(|_| refused("native path contains NUL"))?;
    let flags = libc::O_RDONLY
        | libc::O_CLOEXEC
        | libc::O_NOFOLLOW
        | libc::O_NONBLOCK
        | if directory { libc::O_DIRECTORY } else { 0 };
    // SAFETY: parent is a held directory/AT_FDCWD; no create or write operation.
    let fd = unsafe { libc::openat(parent, name.as_ptr(), flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful openat returned this new owned description.
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn directory(path: &Path, deadline: Instant) -> io::Result<File> {
    canonical(path)?;
    time(deadline)?;
    let mut dir = openat(libc::AT_FDCWD, OsStr::new("/"), true)?;
    owned(&dir, true, false)?;
    for part in path.components() {
        if let Component::Normal(name) = part {
            time(deadline)?;
            dir = openat(dir.as_raw_fd(), name, true)?;
            owned(&dir, true, false)?;
        }
    }
    Ok(dir)
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn hold_directory(
    directories: &mut BTreeMap<PathBuf, HeldDirectory>,
    path: &Path,
    root: &Path,
    deadline: Instant,
) -> io::Result<()> {
    canonical(path)?;
    let mut walked = PathBuf::from("/");
    for part in std::iter::once(Component::RootDir).chain(path.components().skip(1)) {
        if let Component::Normal(name) = part {
            walked.push(name);
        }
        time(deadline)?;
        if let Some(held) = directories.get(&walked) {
            if !same_directory(&held.before, &held.file.metadata()?, held.selected) {
                return Err(refused("held native directory changed"));
            }
            continue;
        }
        if directories.len() >= MAX_DIRECTORIES {
            return Err(refused("native runtime directory population exceeds bound"));
        }
        let file = directory(&walked, deadline)?;
        let before = owned(&file, true, false)?;
        directories.insert(
            walked.clone(),
            HeldDirectory { file, before, selected: walked.starts_with(root) },
        );
    }
    Ok(())
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn file(
    directories: &mut BTreeMap<PathBuf, HeldDirectory>,
    path: &Path,
    executable: bool,
    root: &Path,
    deadline: Instant,
) -> io::Result<(HeldFile, Vec<u8>)> {
    canonical(path)?;
    let parent = path.parent().ok_or_else(|| refused("native input has no parent"))?;
    hold_directory(directories, parent, root, deadline)?;
    let dir = &directories.get(parent).ok_or_else(|| refused("native input parent absent"))?.file;
    let file = openat(
        dir.as_raw_fd(),
        path.file_name().ok_or_else(|| refused("native filename absent"))?,
        false,
    )?;
    let before = owned(&file, false, executable)?;
    if before.len() == 0 || before.len() > MAX_IMAGE {
        return Err(refused("native image size exceeds supported bound"));
    }
    let mut hash = Sha256::new();
    let mut offset = 0u64;
    let mut header = Vec::new();
    let mut bytes = [0u8; 65536];
    while offset < before.len() {
        time(deadline)?;
        let wanted = (before.len() - offset).min(bytes.len() as u64) as usize;
        let count = file.read_at(&mut bytes[..wanted], offset)?;
        if count == 0 {
            return Err(refused("native image shortened during binding"));
        }
        hash.update(&bytes[..count]);
        let keep = count.min((MAX_COMMAND_BYTES + 32).saturating_sub(header.len()));
        header.extend_from_slice(&bytes[..keep]);
        offset += count as u64;
    }
    time(deadline)?;
    if file.read_at(&mut bytes[..1], offset)? != 0 || stamp(&file.metadata()?) != stamp(&before) {
        return Err(refused("native image changed during binding"));
    }
    let observation = FileObservation {
        path: path.to_owned(),
        sha256: hex(&hash.finalize()),
        bytes: before.len(),
        device: before.dev(),
        inode: before.ino(),
    };
    Ok((HeldFile { file, before, observation }, header))
}

impl HeldProgram {
    /// Open the exact native executable and supported runtime once. The expected
    /// digest is an equality assertion, never writer or release authority.
    pub fn open(
        root: &Path,
        program: &Path,
        expected: &[u8; 32],
        deadline: Instant,
    ) -> io::Result<Self> {
        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        {
            let _ = (root, program, expected, deadline);
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "native metadata runtime profile is unavailable",
            ));
        }
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        Self::open_macho(root, program, expected, deadline)
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    fn open_macho(
        root: &Path,
        program: &Path,
        expected: &[u8; 32],
        deadline: Instant,
    ) -> io::Result<Self> {
        canonical(root)?;
        canonical(program)?;
        if program.parent() != Some(root.join("bin").as_path()) {
            return Err(refused("native metadata executable must be directly inside root/bin"));
        }
        let mut directories = BTreeMap::new();
        let (executable, header) = file(&mut directories, program, true, root, deadline)?;
        if executable.observation.sha256 != hex(expected) {
            return Err(refused("native executable digest differs from original request"));
        }
        let commands = macho::commands(&header, true)?;
        let mut loader_paths = Vec::new();
        for path in [root.join("lib"), root.join("lib/rustlib/aarch64-apple-darwin/lib")] {
            match hold_directory(&mut directories, &path, root, deadline) {
                Ok(()) => loader_paths.push(path),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        if loader_paths.iter().any(|path| path.as_os_str().as_bytes().contains(&b':')) {
            return Err(refused("native runtime search root contains loader separator"));
        }
        let mut pending = commands.loads;
        let mut system = BTreeSet::new();
        let mut runtime = Vec::new();
        let mut paths = BTreeSet::new();
        let mut total = executable.before.len();
        let mut next = 0;
        while next < pending.len() {
            time(deadline)?;
            let load = &pending[next];
            next += 1;
            let system_load = macho::system(load);
            let leaf = if system_load {
                Path::new(load)
                    .file_name()
                    .ok_or_else(|| refused("system runtime has no filename"))?
            } else {
                let leaf = load
                    .strip_prefix("@rpath/")
                    .filter(|name| {
                        !name.is_empty() && !name.contains('/') && *name != "." && *name != ".."
                    })
                    .ok_or_else(|| refused("non-system runtime requires a plain @rpath leaf"))?;
                OsStr::new(leaf)
            };
            let mut selected = None;
            for base in &loader_paths {
                let path = base.join(leaf);
                // Do not hash repeated dependencies or create more handles.
                if paths.contains(&path) {
                    selected = Some(None);
                    break;
                }
                match file(&mut directories, &path, false, root, deadline) {
                    Ok(value) => {
                        selected = Some(Some(value));
                        break;
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
            }
            if system_load {
                if selected.is_some() {
                    return Err(refused("sysroot shadows an OS runtime dependency"));
                }
                system.insert(load.clone());
                continue;
            }
            let Some(selected) = selected else {
                return Err(refused("non-system runtime dependency is absent from selected roots"));
            };
            let Some((held, header)) = selected else {
                continue;
            };
            if runtime.len() >= MAX_RUNTIME {
                return Err(refused("native runtime population exceeds bound"));
            }
            total = total
                .checked_add(held.before.len())
                .ok_or_else(|| refused("native runtime byte count overflow"))?;
            if total > MAX_TOTAL {
                return Err(refused("native runtime byte population exceeds bound"));
            }
            let commands = macho::commands(&header, false)?;
            if pending.len().saturating_add(commands.loads.len()) > MAX_COMMANDS {
                return Err(refused("native runtime edge population exceeds bound"));
            }
            pending.extend(commands.loads);
            paths.insert(held.observation.path.clone());
            runtime.push(held);
        }
        runtime.sort_by(|left, right| left.observation.path.cmp(&right.observation.path));
        let (host, boot) = process::host_boot()?;
        let observation = ProgramObservation {
            policy: RUNTIME_POLICY,
            scope: INPUT_SCOPE,
            executable: executable.observation.clone(),
            runtime: runtime.iter().map(|held| held.observation.clone()).collect(),
            loader_paths,
            system_dependencies: system.into_iter().collect(),
            host,
            boot,
        };
        let held = Self { root: root.to_owned(), executable, runtime, directories, observation };
        held.validate(deadline)?;
        Ok(held)
    }

    pub fn observation(&self) -> &ProgramObservation {
        &self.observation
    }
    pub fn program(&self) -> &Path {
        &self.executable.observation.path
    }
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The sole non-system loader override comes from the same held closure.
    /// The execution adapter must clear its complete ambient environment first.
    pub fn runtime_environment(&self) -> io::Result<Vec<(OsString, OsString)>> {
        if self.observation.loader_paths.is_empty() {
            return Ok(Vec::new());
        }
        let value = std::env::join_paths(&self.observation.loader_paths)
            .map_err(|_| refused("native runtime search path cannot be represented"))?;
        Ok(vec![("DYLD_LIBRARY_PATH".into(), value)])
    }

    /// Recheck original held descriptions, every selected path and all original
    /// ancestor directories. No changed input is reopened as a replacement.
    pub fn validate(&self, deadline: Instant) -> io::Result<()> {
        time(deadline)?;
        if process::host_boot()? != (self.observation.host.clone(), self.observation.boot.clone()) {
            return Err(refused("native program host/boot changed"));
        }
        for (path, held) in &self.directories {
            time(deadline)?;
            let current = directory(path, deadline)?;
            if !same_directory(&held.before, &owned(&held.file, true, false)?, held.selected)
                || !same_directory(&held.before, &owned(&current, true, false)?, held.selected)
            {
                return Err(refused("original native runtime directory changed"));
            }
        }
        for held in std::iter::once(&self.executable).chain(&self.runtime) {
            time(deadline)?;
            let path = &held.observation.path;
            let parent = self
                .directories
                .get(path.parent().ok_or_else(|| refused("native parent absent"))?)
                .ok_or_else(|| refused("held native parent absent"))?;
            let current = openat(
                parent.file.as_raw_fd(),
                path.file_name().ok_or_else(|| refused("native filename absent"))?,
                false,
            )?;
            if stamp(&owned(&held.file, false, false)?) != stamp(&held.before)
                || stamp(&owned(&current, false, false)?) != stamp(&held.before)
                || identity(&current.metadata()?) != FileId::of_metadata(&held.before)
            {
                return Err(refused("original native executable/runtime backing changed"));
            }
        }
        time(deadline)
    }

    /// Read the support declaration through the original held image. This
    /// pre-GO predicate grants no execution, custody, SDK or source authority;
    /// original loaded-image validation and actual live Ready remain required.
    pub fn require_resolver_protocol_marker(
        &self,
        _expected: &[u8; 64],
        deadline: Instant,
    ) -> io::Result<()> {
        self.validate(deadline)?;
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        protocol_marker::require(
            &self.executable.file,
            protocol_marker::Profile::Resolver,
            _expected,
            deadline,
        )?;
        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "native resolver image declarations require macOS arm64",
        ));
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        self.validate(deadline)
    }

    /// Read the distinct workload declaration through this same held image.
    /// This support predicate never establishes live readiness, original CPU
    /// lease-set/SDK/output custody, execution, source or proof authority.
    pub fn require_workload_protocol_marker(
        &self,
        _expected: &[u8; 64],
        deadline: Instant,
    ) -> io::Result<()> {
        self.validate(deadline)?;
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        protocol_marker::require(
            &self.executable.file,
            protocol_marker::Profile::Workload,
            _expected,
            deadline,
        )?;
        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "native workload image declarations require macOS arm64",
        ));
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        self.validate(deadline)
    }

    /// Require the separate paired qualification declaration in this same
    /// original held image. Workload-v1 support cannot negotiate Invocation60
    /// or Ready48/v2. This observation grants no startup or execution authority.
    pub fn require_paired_protocol_marker(
        &self,
        _expected: &[u8; 64],
        deadline: Instant,
    ) -> io::Result<()> {
        self.validate(deadline)?;
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        protocol_marker::require(
            &self.executable.file,
            protocol_marker::Profile::PairedQualification,
            _expected,
            deadline,
        )?;
        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "native paired qualification image declarations require macOS arm64",
        ));
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        self.validate(deadline)
    }

    /// Require the distinct compiler member-original/extra/return declaration
    /// from this same held executable. Generic resource-v3 and root-workload
    /// support are insufficient. This grants no CPU, SDK, GO or proof authority.
    pub fn require_workload_member_protocol_marker(
        &self,
        _expected: &[u8; 64],
        deadline: Instant,
    ) -> io::Result<()> {
        self.validate(deadline)?;
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        protocol_marker::require(
            &self.executable.file,
            protocol_marker::Profile::WorkloadMember,
            _expected,
            deadline,
        )?;
        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "native workload compiler declarations require macOS arm64",
        ));
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        self.validate(deadline)
    }

    /// Check the actual original suspended child, before its custodian permits
    /// GO. This observes loaded executable identity, never execution or proof.
    pub fn validate_loaded(&self, child: process::Identity, deadline: Instant) -> io::Result<()> {
        self.validate(deadline)?;
        if !process::image_matches(child, &self.executable.file, self.program())? {
            return Err(refused("suspended member loaded another native image"));
        }
        self.validate(deadline)
    }
}

#[cfg(any(test, all(target_os = "macos", target_arch = "aarch64")))]
mod macho;
#[cfg(any(test, all(target_os = "macos", target_arch = "aarch64")))]
mod protocol_marker;

pub mod produced;

#[cfg(all(test, target_os = "macos", target_arch = "aarch64"))]
mod tests;

#[cfg(all(test, not(all(target_os = "macos", target_arch = "aarch64"))))]
mod unsupported_tests {
    use super::*;

    #[test]
    fn unavailable_native_profile_refuses_without_opening_inputs() {
        let error = HeldProgram::open(
            Path::new("/absent-native-metadata-root"),
            Path::new("/absent-native-metadata-root/bin/tool"),
            &[0; 32],
            Instant::now(),
        )
        .err()
        .expect("unsupported platform cannot construct held program custody");
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    }
}
