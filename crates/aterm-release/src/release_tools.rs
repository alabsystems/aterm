// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Frozen, content-addressed release tools and the proof used by an in-place cutter exec.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ledger::{GitCli, GitRunner as _};

pub const CUTTER_VERSION: &str = "aterm-release tool 1";
pub const VERIFY_VERSION: &str = aterm_verify::FROZEN_TOOL_VERSION;
pub const RECORD_ENV: &str = "ATERM_RELEASE_TOOL_RECORD";
pub const FREEZER_ENV: &str = "ATERM_RELEASE_FREEZER_COMMIT";
pub const CONTRACT_ENV: &str = "ATERM_RELEASE_FREEZER_CONTRACT";
pub const REFREEZE_ENV: &str = "ATERM_RELEASE_REFROZEN_COMMIT";
pub const CONTRACT: &str = "cutter.toml/2";
const RECORD: &str = "tool.toml";
const CUTTER_RECORD: &str = "cutter.toml";
const SCHEMA: u32 = 1;
const CUTTER_SCHEMA: u32 = 2;

/// Recovery permission only; none of these launch hints is a content proof.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Recovery {
    #[default]
    Initial,
    Refreeze,
    Refuse,
}

impl Recovery {
    pub fn read(commit: &str) -> Self {
        Self::from_launch(commit, crate::cutter::running_stamp().is_ok(), |name| {
            std::env::var(name).ok()
        })
    }

    fn from_launch(commit: &str, has_stamp: bool, get: impl Fn(&str) -> Option<String>) -> Self {
        // A valid cutter for an earlier candidate is a normal dispatch, not a failed
        // freeze. Do not consume the next candidate's repair on that transition.
        if has_stamp {
            return Self::Initial;
        }
        if get(REFREEZE_ENV).as_deref() == Some(commit) {
            return Self::Refuse;
        }
        if get(RECORD_ENV).is_none() {
            return Self::Initial;
        }
        if get(FREEZER_ENV).as_deref() == Some(commit)
            && get(CONTRACT_ENV).as_deref() == Some(CONTRACT)
        {
            Self::Refuse
        } else {
            // Pre-envelope freezers (including tool.toml) cannot name their contract.
            // Re-freeze, never interpret their proof as evidence for this image.
            Self::Refreeze
        }
    }

    pub fn hand(
        self,
        command: &mut aterm_uds::spawnfd::Command,
        commit: &str,
        freezer: &str,
        previous: Option<&std::ffi::OsStr>,
    ) {
        command
            .env(FREEZER_ENV, freezer)
            .env(CONTRACT_ENV, CONTRACT);
        if self == Self::Refreeze {
            command.env(REFREEZE_ENV, commit);
        } else if let Some(previous) = previous {
            command.env(REFREEZE_ENV, previous);
        } else {
            command.env_remove(REFREEZE_ENV);
        }
    }
}

/// The launch proof needs only the cutter. The verifier is built while the frozen cutter's
/// first steps run, then frozen in the complete tool set before qualification.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cutter {
    schema: u32,
    pub commit: String,
    pub built_from: String,
    pub inputs: Option<crate::cutter_inputs::Inputs>,
    pub tool: Tool,
}

pub fn cutter_record_path(release: &Path) -> PathBuf {
    release.join(aterm_tempfile::QUIET).join(CUTTER_RECORD)
}

impl Cutter {
    fn validate(&self, release: &Path) -> Result<(), String> {
        if self.schema != CUTTER_SCHEMA
            || !aterm_codec::hex::is_lower(&self.commit, 40)
            || !aterm_codec::hex::is_lower(&self.built_from, 40)
            || (self.built_from != self.commit && self.inputs.is_none())
            || self.inputs.as_ref().is_some_and(|inputs| {
                [&inputs.digest, &inputs.sources, &inputs.seal]
                    .iter()
                    .any(|digest| !aterm_codec::hex::is_lower(digest, 64))
            })
            || !aterm_codec::hex::is_lower(&self.tool.sha256, 64)
            || !release.is_absolute()
            || self.tool.path
                != release
                    .join(".ubd/tools")
                    .join(&self.tool.sha256)
                    .join("aterm-release")
        {
            return Err("invalid frozen cutter binding".into());
        }
        real_directory(&release.join(".ubd"), "frozen-tool quiet parent")?;
        real_directory(&release.join(".ubd/tools"), "frozen-tool root")?;
        real_directory(
            self.tool.path.parent().ok_or("no cutter directory")?,
            "frozen cutter directory",
        )?;
        let metadata = std::fs::symlink_metadata(&self.tool.path).map_err(|e| e.to_string())?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || crate::dmg::sha256_file(&self.tool.path)? != self.tool.sha256
        {
            return Err("the frozen cutter's bytes differ from its proof".into());
        }
        Ok(())
    }
}

/// Read the cutter proof and authenticate every binding and its executable bytes.
///
/// # Errors
/// The proof cannot be read or decoded, its shape or bytes differ, or it names another commit.
pub fn recorded_cutter(release: &Path, commit: &str) -> Result<Cutter, String> {
    let cutter = read_cutter(release)?;
    if cutter.commit != commit {
        return Err("the cutter proof names another commit".into());
    }
    Ok(cutter)
}

fn read_cutter(release: &Path) -> Result<Cutter, String> {
    let text =
        crate::durable::read_text(&cutter_record_path(release)).map_err(|e| e.to_string())?;
    let cutter: Cutter = aterm_toml::from_str(&text).map_err(|e| e.to_string())?;
    cutter.validate(release)?;
    Ok(cutter)
}

/// Adopt only after Cargo checked the new graph and every observed input still matches.
/// A source edit which produces identical machine code is still a different input.
pub(crate) fn adopt_cutter(
    release: &Path,
    commit: &str,
    source: &Path,
    observation: &crate::cutter_inputs::Observation,
) -> Result<Option<Cutter>, String> {
    if !observation.all_fresh {
        return Ok(None);
    }
    let Ok(previous) = read_cutter(release) else {
        return Ok(None);
    };
    if previous.inputs.as_ref() != Some(&observation.inputs)
        || crate::dmg::sha256_file(source)? != previous.tool.sha256
    {
        return Ok(None);
    }
    let adopted = Cutter {
        commit: commit.to_owned(),
        ..previous
    };
    adopted.validate(release)?;
    Ok(Some(adopted))
}

/// Copy and prove a cutter without waiting for a verifier build. The caller checks clean R
/// before this call and again before publishing the proof.
///
/// # Errors
/// The root or commit is invalid, storage or copying fails, a directory or source is a
/// symlink, the copied bytes differ, or the exact successful first exec cannot be proved.
pub fn stage_cutter(
    release: &Path,
    commit: &str,
    source: &Path,
    log: &Path,
    inputs: Option<crate::cutter_inputs::Inputs>,
) -> Result<Cutter, String> {
    let digest = {
        let _phase = crate::cut_timing::Timer::start(log, "cutter-source-hash");
        crate::dmg::sha256_file(source)?
    };
    let root = release.join(".ubd/tools");
    let dir = root.join(&digest);
    if !release.is_absolute() || !aterm_codec::hex::is_lower(commit, 40) {
        return Err("the cutter needs an absolute release root and full commit".into());
    }
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    real_directory(&release.join(".ubd"), "frozen-tool quiet parent")?;
    real_directory(&root, "frozen-tool root")?;
    let cutter = Cutter {
        schema: CUTTER_SCHEMA,
        commit: commit.into(),
        built_from: commit.into(),
        inputs,
        tool: Tool {
            path: dir.join("aterm-release"),
            sha256: digest,
        },
    };
    match std::fs::symlink_metadata(&dir) {
        Ok(_) => cutter.validate(release)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let _phase = crate::cut_timing::Timer::start(log, "cutter-copy-sync");
            let stage = aterm_tempfile::Builder::new()
                .prefix(".cutter-")
                .suffix(".noindex")
                .tempdir_in(&root)
                .map_err(|e| e.to_string())?;
            copy_executable(source, &stage.path().join("aterm-release"))?;
            crate::durable::sync_dir(stage.path()).map_err(|e| e.to_string())?;
            std::fs::rename(stage.path(), &dir).map_err(|e| e.to_string())?;
            crate::durable::sync_dir(&root).map_err(|e| e.to_string())?;
        }
        Err(error) => return Err(error.to_string()),
    }
    {
        let _phase = crate::cut_timing::Timer::start(log, "cutter-copy-hash");
        cutter.validate(release)?;
    }
    {
        let _phase = crate::cut_timing::Timer::start(log, "cutter-first-exec");
        prove_exec(&cutter.tool.path, CUTTER_VERSION)?;
    }
    Ok(cutter)
}

/// Publish the cutter proof only after the caller's second clean-tree check.
///
/// # Errors
/// The cutter binding or bytes are invalid, serialization fails, or the durable write fails.
pub fn publish_cutter(release: &Path, cutter: &Cutter) -> Result<(), String> {
    cutter.validate(release)?;
    let text = aterm_toml::to_string(cutter).map_err(|e| e.to_string())?;
    crate::durable::write_atomic_durable(&cutter_record_path(release), text.as_bytes(), 0o600)
        .map_err(|e| e.to_string())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tool {
    pub path: PathBuf,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolSet {
    schema: u32,
    pub commit: String,
    pub set_sha256: String,
    pub cutter: Tool,
    pub verify: Tool,
}

impl ToolSet {
    fn new(commit: &str, cutter: Tool, verify: Tool) -> Self {
        let set_sha256 = set_digest(&cutter.sha256, &verify.sha256);
        Self {
            schema: SCHEMA,
            commit: commit.to_owned(),
            set_sha256,
            cutter,
            verify,
        }
    }

    fn validate_shape(&self, release: &Path) -> Result<(), String> {
        if self.schema != SCHEMA || !aterm_codec::hex::is_lower(&self.commit, 40) {
            return Err("the frozen-tool record has an unsupported schema or commit".into());
        }
        if !aterm_codec::hex::is_lower(&self.set_sha256, 64)
            || self.set_sha256 != set_digest(&self.cutter.sha256, &self.verify.sha256)
        {
            return Err("the frozen-tool record's set digest is invalid".into());
        }
        let dir = release.join(".ubd/tools").join(&self.set_sha256);
        for (name, tool) in [
            ("aterm-release", &self.cutter),
            ("aterm-verify", &self.verify),
        ] {
            if !aterm_codec::hex::is_lower(&tool.sha256, 64)
                || tool.path != dir.join(name)
                || !tool.path.is_absolute()
            {
                return Err(format!("the frozen {name} locator is invalid"));
            }
        }
        Ok(())
    }

    fn validate_bytes(&self) -> Result<(), String> {
        let final_dir = self
            .cutter
            .path
            .parent()
            .ok_or("the frozen-tool set has no directory")?;
        let tools_dir = final_dir
            .parent()
            .ok_or("the frozen-tool set has no tools directory")?;
        real_directory(
            tools_dir
                .parent()
                .ok_or("the frozen-tool root has no quiet parent")?,
            "frozen-tool quiet parent",
        )?;
        real_directory(tools_dir, "frozen-tool root")?;
        real_directory(final_dir, "frozen-tool set")?;
        for (name, tool) in [
            ("aterm-release", &self.cutter),
            ("aterm-verify", &self.verify),
        ] {
            let metadata = std::fs::symlink_metadata(&tool.path)
                .map_err(|e| format!("read frozen {name} {}: {e}", tool.path.display()))?;
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err(format!(
                    "frozen {name} {} is not a regular file",
                    tool.path.display()
                ));
            }
            let found = crate::dmg::sha256_file(&tool.path)?;
            if found != tool.sha256 {
                return Err(format!(
                    "frozen {name} {} hashes to {found}, not {}",
                    tool.path.display(),
                    tool.sha256
                ));
            }
        }
        Ok(())
    }
}

fn real_directory(path: &Path, what: &str) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|e| format!("inspect {what} {}: {e}", path.display()))?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        Ok(())
    } else {
        Err(format!("{what} {} is not a real directory", path.display()))
    }
}

pub(crate) fn set_digest(cutter: &str, verify: &str) -> String {
    use aterm_digest::Sha256;
    let mut digest = Sha256::new();
    digest.update(b"aterm release frozen tools v1\0");
    digest.update(cutter.as_bytes());
    digest.update(b"\0");
    digest.update(verify.as_bytes());
    aterm_codec::hex::encode_lower(&digest.finalize())
}

#[must_use]
pub fn record_path(release: &Path) -> PathBuf {
    release.join(aterm_tempfile::QUIET).join(RECORD)
}

/// Read and authenticate the frozen tools for `commit`.
pub fn recorded(release: &Path, commit: &str) -> Result<ToolSet, String> {
    let path = record_path(release);
    let text = crate::durable::read_text(&path)
        .map_err(|e| format!("read frozen-tool record {}: {e}", path.display()))?;
    let tools: ToolSet = aterm_toml::from_str(&text)
        .map_err(|e| format!("parse frozen-tool record {}: {e}", path.display()))?;
    tools.validate_shape(release)?;
    if tools.commit != commit {
        return Err(format!(
            "the frozen tools are for {}, not {commit}",
            tools.commit
        ));
    }
    tools.validate_bytes()?;
    Ok(tools)
}

/// Copy two completed builds into their immutable content-addressed directory and prove both
/// copies execute. The record is not published until [`publish`] after the caller's second
/// clean-tree check.
pub fn stage(
    release: &Path,
    commit: &str,
    cutter: &Path,
    verify: &Path,
) -> Result<ToolSet, String> {
    if !aterm_codec::hex::is_lower(commit, 40) {
        return Err(format!("release commit {commit:?} is not a full commit id"));
    }
    let cutter_sha = crate::dmg::sha256_file(cutter)?;
    let verify_sha = crate::dmg::sha256_file(verify)?;
    let prototype = ToolSet::new(
        commit,
        Tool {
            path: PathBuf::new(),
            sha256: cutter_sha,
        },
        Tool {
            path: PathBuf::new(),
            sha256: verify_sha,
        },
    );
    let tools_dir = release.join(".ubd/tools");
    let final_dir = tools_dir.join(&prototype.set_sha256);
    let tools = ToolSet {
        cutter: Tool {
            path: final_dir.join("aterm-release"),
            ..prototype.cutter
        },
        verify: Tool {
            path: final_dir.join("aterm-verify"),
            ..prototype.verify
        },
        ..prototype
    };
    tools.validate_shape(release)?;
    std::fs::create_dir_all(&tools_dir)
        .map_err(|e| format!("create {}: {e}", tools_dir.display()))?;
    real_directory(&release.join(".ubd"), "frozen-tool quiet parent")?;
    real_directory(&tools_dir, "frozen-tool root")?;
    match std::fs::symlink_metadata(&final_dir) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            tools.validate_bytes()?;
        }
        Ok(_) => {
            return Err(format!(
                "frozen-tool set {} is not a real directory",
                final_dir.display()
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let staged = aterm_tempfile::Builder::new()
                .prefix(".tools-")
                .suffix(".noindex")
                .tempdir_in(&tools_dir)
                .map_err(|e| format!("stage frozen tools in {}: {e}", tools_dir.display()))?;
            copy_executable(cutter, &staged.path().join("aterm-release"))?;
            copy_executable(verify, &staged.path().join("aterm-verify"))?;
            for (name, expected) in [
                ("aterm-release", &tools.cutter.sha256),
                ("aterm-verify", &tools.verify.sha256),
            ] {
                let copied = staged.path().join(name);
                let found = crate::dmg::sha256_file(&copied)?;
                if &found != expected {
                    return Err(format!(
                        "staged frozen {name} hashes to {found}, not {expected}"
                    ));
                }
            }
            crate::durable::sync_dir(staged.path()).map_err(|e| e.to_string())?;
            std::fs::rename(staged.path(), &final_dir)
                .map_err(|e| format!("publish frozen tools {}: {e}", final_dir.display()))?;
            crate::durable::sync_dir(&tools_dir).map_err(|e| e.to_string())?;
            tools.validate_bytes()?;
        }
        Err(error) => {
            return Err(format!(
                "inspect frozen-tool set {}: {error}",
                final_dir.display()
            ));
        }
    }
    prove_exec(&tools.cutter.path, CUTTER_VERSION)?;
    prove_exec(&tools.verify.path, VERIFY_VERSION)?;
    Ok(tools)
}

#[cfg(unix)]
fn copy_executable(from: &Path, to: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt as _;
    let (mut source, metadata) = crate::durable::open_regular_file_with_meta(from)
        .map_err(|e| format!("open tool {}: {e}", from.display()))?;
    if !metadata.is_file() {
        return Err(format!("tool {} is not a regular file", from.display()));
    }
    let mut target = crate::durable::open_create_new(to)
        .map_err(|e| format!("create frozen tool {}: {e}", to.display()))?;
    std::io::copy(&mut source, &mut target)
        .map_err(|e| format!("copy frozen tool {}: {e}", to.display()))?;
    target
        .set_permissions(std::fs::Permissions::from_mode(0o755))
        .map_err(|e| format!("chmod frozen tool {}: {e}", to.display()))?;
    crate::durable::sync_all(&target)
        .map_err(|e| format!("flush frozen tool {}: {e}", to.display()))
}

#[cfg(not(unix))]
fn copy_executable(_from: &Path, _to: &Path) -> Result<(), String> {
    Err("frozen release tools require Unix executable custody".into())
}

#[cfg(unix)]
fn prove_exec(path: &Path, line: &str) -> Result<(), String> {
    let deadline = std::time::Instant::now() + aterm_uds::deadline::FIRST_EXEC_HANG;
    prove_exec_until(path, line, deadline, &mut |_| false)
}

/// Prove the version run through complete EOF and a successful owning wait, with no output
/// reader thread. `stop` is a test seam that can end the wait after observing output without a
/// timing budget; production never asks it to stop.
#[cfg(unix)]
fn prove_exec_until(
    path: &Path,
    line: &str,
    deadline: std::time::Instant,
    stop: &mut dyn FnMut(&[u8]) -> bool,
) -> Result<(), String> {
    use aterm_uds::spawnfd::Stdio;
    use std::io::Read as _;
    use std::os::fd::AsFd as _;

    struct OwnedChild(aterm_uds::spawnfd::Child);
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            if !self.0.is_reaped() {
                aterm_uds::deadline::settle_child(&mut self.0);
            }
        }
    }

    let mut command = aterm_uds::spawnfd::command(path);
    command
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    let mut child = OwnedChild(
        command
            .spawn()
            .map_err(|e| format!("first exec of {}: {e}", path.display()))?,
    );
    let mut stdout = child
        .0
        .stdout
        .take()
        .ok_or_else(|| format!("first exec of {} has no stdout", path.display()))?;
    let flags = stdout
        .as_fd()
        .try_clone_to_owned()
        .map_err(|e| format!("observe first exec of {}: {e}", path.display()))?;
    let mut bytes = Vec::new();
    let mut overflow = false;
    let mut eof = false;
    let mut status = None;
    let mut exit = aterm_uds::deadline::ExitWait::new(&child.0);
    let read = aterm_uds::ready::with_nonblocking(flags.as_fd(), || {
        let mut chunk = [0u8; 512];
        loop {
            let before = bytes.len();
            loop {
                match stdout.read(&mut chunk) {
                    Ok(0) => {
                        eof = true;
                        break;
                    }
                    Ok(n) => {
                        let keep = n.min(4096usize.saturating_sub(bytes.len()));
                        bytes.extend_from_slice(&chunk[..keep]);
                        overflow |= keep != n;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(error) => return Err(error),
                }
            }
            if bytes.len() != before && stop(&bytes) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "proof stopped after observing output",
                ));
            }
            if status.is_none() {
                status = child.0.try_wait()?;
            }
            if eof && status.is_some() {
                return Ok(());
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "version run did not exit with complete stdout",
                ));
            }
            if eof {
                exit.wait(remaining);
            } else if aterm_uds::ready::any_until(&[stdout.as_fd()], deadline)?.is_empty() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "version run did not exit with complete stdout",
                ));
            }
        }
    });
    drop(stdout);
    read.map_err(|e| format!("first exec of {}: {e}", path.display()))?;
    let status =
        status.ok_or_else(|| format!("first exec of {} was not reaped", path.display()))?;
    let expected = format!("{line}\n");
    if overflow || bytes != expected.as_bytes() {
        return Err(format!(
            "first exec of {} printed {:?}, not the exact line {line:?}",
            path.display(),
            String::from_utf8_lossy(&bytes)
        ));
    }
    if !status.success() {
        return Err(format!("first exec of {} ended {status}", path.display()));
    }
    Ok(())
}

#[cfg(not(unix))]
fn prove_exec(_path: &Path, _line: &str) -> Result<(), String> {
    Err("frozen release tool execution requires Unix".into())
}

/// Durably publish the record after the source tree was checked a second time.
pub fn publish(release: &Path, tools: &ToolSet) -> Result<PathBuf, String> {
    tools.validate_shape(release)?;
    tools.validate_bytes()?;
    let path = record_path(release);
    let text =
        aterm_toml::to_string(tools).map_err(|e| format!("serialize frozen-tool record: {e}"))?;
    crate::durable::write_atomic_durable(&path, text.as_bytes(), 0o600)
        .map_err(|e| format!("write frozen-tool record {}: {e}", path.display()))?;
    Ok(path)
}

/// The proof this process started under, for a child that runs this same frozen image (step 7's
/// `cut --sign`, step 8's `cut`): the [`RECORD_ENV`] record [`proved_running`] validated, and
/// only while this process's stamp is that proof ([`crate::cutter::Stamp::Proved`]). A child
/// is scrubbed of every inherited record and freezer envelope
/// ([`crate::release_steps`]'s `scrub_names`), and without this record the frozen copy falls
/// back to a dep-info judgment no copy can pass, since cargo writes no `.d` beside it. The
/// exec-only envelope ([`FREEZER_ENV`], [`CONTRACT_ENV`], [`REFREEZE_ENV`]) is never handed
/// with it.
pub fn proof_for_this_image() -> Option<std::ffi::OsString> {
    match crate::cutter::running_stamp() {
        Ok(crate::cutter::Stamp::Proved(_)) => std::env::var_os(RECORD_ENV),
        _ => None,
    }
}

/// Validate the explicit proof handed to a copied cutter at startup. With no proof variable,
/// answer `Ok(None)` so hand-run verbs retain the dep-info judgment.
pub fn proved_running(exe: &Path) -> Result<Option<String>, String> {
    let Some(record) = std::env::var_os(RECORD_ENV) else {
        return Ok(None);
    };
    let record = PathBuf::from(record);
    let repo = std::env::current_dir().map_err(|e| format!("name the release clone: {e}"))?;
    let args: Option<Vec<_>> = std::env::args_os()
        .skip(1)
        .map(|arg| arg.into_string().ok())
        .collect();
    proved_running_at(exe, &record, &repo, args.as_deref()).map(Some)
}

fn proved_running_at(
    exe: &Path,
    record: &Path,
    repo: &Path,
    args: Option<&[String]>,
) -> Result<String, String> {
    let cutter = proved_cutter(exe, record)?;
    if !args.is_some_and(reads_candidate_elsewhere) {
        clean_at(repo, &cutter.commit)?;
    }
    Ok(cutter.commit)
}

/// Only a parsed release preview can leave the caller's HEAD alone. Its frozen executable
/// is still content-proved above; its readers use the owned candidate checkout instead.
fn reads_candidate_elsewhere(args: &[String]) -> bool {
    matches!(crate::cli::parse(args), Ok(crate::cli::Cmd::Release(opts))
        if opts.dry_run && !opts.forget && opts.abandon.is_none() && !opts.prefreeze)
}

fn proved_cutter(exe: &Path, record: &Path) -> Result<Cutter, String> {
    if !record.is_absolute() || record.file_name().and_then(|n| n.to_str()) != Some(CUTTER_RECORD) {
        return Err("the frozen-tool proof does not name an absolute cutter.toml".into());
    }
    let quiet = record
        .parent()
        .filter(|parent| parent.file_name() == Some(std::ffi::OsStr::new(aterm_tempfile::QUIET)))
        .ok_or("the frozen-tool proof has no quiet parent")?;
    let release = quiet
        .parent()
        .ok_or("the frozen-tool proof has no release directory")?;
    let text = crate::durable::read_text(record)
        .map_err(|e| format!("read frozen-tool proof {}: {e}", record.display()))?;
    let cutter: Cutter = aterm_toml::from_str(&text)
        .map_err(|e| format!("parse frozen-tool proof {}: {e}", record.display()))?;
    cutter.validate(release)?;
    let actual = std::fs::canonicalize(exe)
        .map_err(|e| format!("resolve running cutter {}: {e}", exe.display()))?;
    let recorded = std::fs::canonicalize(&cutter.tool.path)
        .map_err(|e| format!("resolve frozen cutter {}: {e}", cutter.tool.path.display()))?;
    if actual != recorded {
        return Err(format!(
            "the running cutter {} is not the recorded frozen cutter {}",
            actual.display(),
            recorded.display()
        ));
    }
    Ok(cutter)
}

pub fn clean_at(repo: &Path, commit: &str) -> Result<(), String> {
    let git = GitCli::new(repo);
    let out = git
        .git(&["rev-parse", "HEAD^{commit}"])
        .map_err(|e| format!("resolve release HEAD: {e}"))?;
    let head = out.stdout_utf8().trim().to_owned();
    if !out.success() || head != commit {
        return Err(format!("the release clone is at {head}, not {commit}"));
    }
    let dirty = crate::gates::dirty_paths(repo).map_err(|e| e.to_string())?;
    if dirty.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "the release clone is dirty at {commit}: {}",
            dirty.into_iter().take(5).collect::<Vec<_>>().join(", ")
        ))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

    /// A /1 freezer cannot supply /2's built_from binding. Repair the old proof once;
    /// a failed repair is not permission for another compile/exec cycle.
    #[test]
    fn a_v1_cutter_record_gets_one_v2_repair() {
        let temp = aterm_tempfile::tempdir().unwrap();
        let release = temp.path().join("release");
        let source = tool_source(temp.path(), "skew-cutter", CUTTER_VERSION, 0);
        let cutter = stage_cutter(
            &release,
            COMMIT,
            &source,
            &release.with_extension("events"),
            None,
        )
        .unwrap();
        let mut legacy = aterm_toml::to_string(&cutter).unwrap();
        legacy = legacy
            .lines()
            .filter(|line| !line.starts_with("built_from ="))
            .map(|line| {
                if line == "schema = 2" {
                    "schema = 1"
                } else {
                    line
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(cutter_record_path(&release), legacy).unwrap();
        assert!(recorded_cutter(&release, COMMIT).is_err());
        let old_launch = |name: &str| match name {
            RECORD_ENV => Some(cutter_record_path(&release).to_string_lossy().into_owned()),
            FREEZER_ENV => Some(COMMIT.into()),
            CONTRACT_ENV => Some("cutter.toml/1".into()),
            _ => None,
        };
        assert_eq!(
            Recovery::from_launch(COMMIT, false, old_launch),
            Recovery::Refreeze
        );

        // The new writer supplies the missing binding. This is usable evidence, rather
        // than a default value invented by the reader of an incompatible proof.
        publish_cutter(&release, &cutter).unwrap();
        assert_eq!(recorded_cutter(&release, COMMIT).unwrap(), cutter);
        let mut repaired = aterm_uds::spawnfd::command("/usr/bin/true");
        Recovery::Refreeze.hand(&mut repaired, COMMIT, COMMIT, None);
        let handed = repaired.get_envs().collect::<Vec<_>>();
        let read = |name: &str| {
            handed
                .iter()
                .find(|(key, _)| *key == name)
                .and_then(|(_, value)| *value)
                .map(|value| value.to_string_lossy().into_owned())
                .or_else(|| old_launch(name))
        };
        assert_eq!(Recovery::from_launch(COMMIT, false, read), Recovery::Refuse);
        assert_eq!(Recovery::from_launch(COMMIT, true, read), Recovery::Initial);
    }

    #[test]
    fn a_local_phase_is_recorded_when_its_operation_returns_an_error() {
        let temp = aterm_tempfile::tempdir().unwrap();
        let log = temp.path().join("events");
        let operation = || -> Result<(), ()> {
            let _phase = crate::cut_timing::Timer::start(&log, "failed-operation");
            Err(())
        };
        assert!(operation().is_err());
        let text = std::fs::read_to_string(&log).unwrap();
        let line = text.lines().last().unwrap();
        let event: aterm_json::Value =
            aterm_json::from_str(line.trim().strip_prefix("release-event: ").unwrap()).unwrap();
        assert_eq!(event["event"].as_str(), Some("cut-gate-finished"));
        assert_eq!(event["gate"].as_str(), Some("failed-operation"));
        assert!(event["execution_s"].as_f64().is_some());
        assert!(event.get("outcome").is_none(), "telemetry is no verdict");
    }

    /// NEGATIVE CONTROLS: the old tool.toml path must not veto repair; own-writer failures
    /// and a second failure must not repair; a marker for a prior candidate cannot veto R.
    #[test]
    fn launch_contract_bounds_recovery_without_authenticating_the_proof() {
        let old = "f".repeat(40);
        let decide = |fields: &[(&str, &str)]| {
            Recovery::from_launch(COMMIT, false, |name| {
                fields
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| value.to_string())
            })
        };
        assert_eq!(decide(&[]), Recovery::Initial);
        assert_eq!(
            decide(&[(RECORD_ENV, "/release/tool.toml")]),
            Recovery::Refreeze
        );
        let own = [
            (RECORD_ENV, "/release/cutter.toml"),
            (FREEZER_ENV, COMMIT),
            (CONTRACT_ENV, CONTRACT),
        ];
        assert_eq!(decide(&own), Recovery::Refuse);
        assert_eq!(
            decide(&[
                (RECORD_ENV, "/release/cutter.toml"),
                (FREEZER_ENV, &old),
                (CONTRACT_ENV, CONTRACT)
            ]),
            Recovery::Refreeze
        );
        assert_eq!(
            decide(&[
                (RECORD_ENV, "/release/cutter.toml"),
                (FREEZER_ENV, COMMIT),
                (CONTRACT_ENV, "older")
            ]),
            Recovery::Refreeze
        );
        assert_eq!(
            decide(&[(RECORD_ENV, "/release/tool.toml"), (REFREEZE_ENV, COMMIT)]),
            Recovery::Refuse
        );
        assert_eq!(decide(&[(REFREEZE_ENV, COMMIT)]), Recovery::Refuse);
        assert_eq!(
            decide(&[(RECORD_ENV, "/release/tool.toml"), (REFREEZE_ENV, &old)]),
            Recovery::Refreeze
        );
        assert_eq!(
            Recovery::from_launch(COMMIT, true, |name| match name {
                RECORD_ENV => Some("/release/cutter.toml".into()),
                FREEZER_ENV | REFREEZE_ENV => Some(COMMIT.into()),
                CONTRACT_ENV => Some(CONTRACT.into()),
                _ => None,
            }),
            Recovery::Initial,
            "a valid stamp dispatches normally even after a prior repair"
        );
        for (recovery, prior, expected) in [
            (Recovery::Initial, None, None),
            (Recovery::Refreeze, None, Some(COMMIT)),
            (Recovery::Initial, Some(COMMIT), Some(COMMIT)),
        ] {
            let mut command = aterm_uds::spawnfd::command("/usr/bin/true");
            recovery.hand(
                &mut command,
                COMMIT,
                COMMIT,
                prior.map(std::ffi::OsStr::new),
            );
            let env = command
                .get_envs()
                .collect::<std::collections::BTreeMap<_, _>>();
            assert_eq!(
                env[std::ffi::OsStr::new(REFREEZE_ENV)],
                expected.map(std::ffi::OsStr::new)
            );
            assert_eq!(
                env[std::ffi::OsStr::new(FREEZER_ENV)],
                Some(std::ffi::OsStr::new(COMMIT))
            );
            assert_eq!(
                env[std::ffi::OsStr::new(CONTRACT_ENV)],
                Some(std::ffi::OsStr::new(CONTRACT))
            );
        }
    }

    /// NEGATIVE CONTROL: accepting the legacy proof bypasses the re-freeze; trusting the
    /// repair hint instead of the new proof accepts the deliberately mutated image.
    #[test]
    fn legacy_tool_record_needs_a_new_content_proof() {
        let temp = aterm_tempfile::tempdir().unwrap();
        let (release, _, _, old) = staged_fixture(temp.path());
        publish(&release, &old).unwrap();
        assert!(proved_cutter(&old.cutter.path, &record_path(&release)).is_err());
        let repaired = stage_cutter(
            &release,
            COMMIT,
            &old.cutter.path,
            &release.with_extension("events"),
            None,
        )
        .unwrap();
        publish_cutter(&release, &repaired).unwrap();
        assert_eq!(
            proved_cutter(&repaired.tool.path, &cutter_record_path(&release)).unwrap(),
            repaired
        );
        std::fs::write(&repaired.tool.path, b"invalid replacement").unwrap();
        assert!(proved_cutter(&repaired.tool.path, &cutter_record_path(&release)).is_err());
    }

    /// NEGATIVE CONTROLS: requiring a verifier prevents the first proof; accepting another
    /// commit, path, schema or changed image makes one of the mutated bindings pass.
    #[test]
    fn a_cutter_can_be_frozen_before_the_verifier_but_every_binding_is_required() {
        let temp = aterm_tempfile::tempdir().unwrap();
        let release = temp.path().join("release");
        let source = tool_source(temp.path(), "cutter-only", CUTTER_VERSION, 0);
        let cutter = stage_cutter(
            &release,
            COMMIT,
            &source,
            &release.with_extension("events"),
            None,
        )
        .unwrap();
        assert!(!record_path(&release).exists());
        publish_cutter(&release, &cutter).unwrap();
        assert_eq!(recorded_cutter(&release, COMMIT).unwrap(), cutter);
        assert!(recorded_cutter(&release, &"f".repeat(40)).is_err());
        for field in 0..3 {
            let mut mutant = cutter.clone();
            match field {
                0 => mutant.schema += 1,
                1 => mutant.tool.path = source.clone(),
                _ => mutant.tool.sha256 = "0".repeat(64),
            }
            assert!(publish_cutter(&release, &mutant).is_err());
        }
        std::fs::write(&cutter.tool.path, b"mutated after first exec completed").unwrap();
        assert!(recorded_cutter(&release, COMMIT).is_err());
        assert!(
            stage_cutter(
                &release,
                COMMIT,
                &source,
                &release.with_extension("events"),
                None
            )
            .is_err()
        );
    }

    /// Byte equality alone cannot authorize adoption: each digest component, Cargo's
    /// freshness and the prior immutable copy must independently remain valid.
    #[test]
    fn adoption_binds_both_commits_and_rejects_changed_or_unproved_inputs() {
        let temp = aterm_tempfile::tempdir().unwrap();
        let release = temp.path().join("release");
        let source = tool_source(temp.path(), "adopt-cutter", CUTTER_VERSION, 0);
        let inputs = crate::cutter_inputs::Inputs {
            digest: "1".repeat(64),
            sources: "2".repeat(64),
            seal: "3".repeat(64),
        };
        let original = stage_cutter(
            &release,
            COMMIT,
            &source,
            &release.with_extension("events"),
            Some(inputs.clone()),
        )
        .unwrap();
        publish_cutter(&release, &original).unwrap();
        let next = "b".repeat(40);
        let mut observation = crate::cutter_inputs::Observation {
            inputs,
            all_fresh: true,
        };
        let adopted = adopt_cutter(&release, &next, &source, &observation)
            .unwrap()
            .unwrap();
        assert_eq!(adopted.commit, next);
        assert_eq!(adopted.built_from, COMMIT);
        assert_eq!(adopted.tool, original.tool);
        publish_cutter(&release, &adopted).unwrap();
        assert_eq!(recorded_cutter(&release, &next).unwrap(), adopted);
        for field in 0..3 {
            let mut changed = observation.inputs.clone();
            match field {
                0 => changed.digest = "4".repeat(64),
                1 => changed.sources = "4".repeat(64),
                _ => changed.seal = "4".repeat(64),
            }
            let changed = crate::cutter_inputs::Observation {
                inputs: changed,
                all_fresh: true,
            };
            assert!(
                adopt_cutter(&release, &"c".repeat(40), &source, &changed)
                    .unwrap()
                    .is_none()
            );
        }
        observation.all_fresh = false;
        assert!(
            adopt_cutter(&release, &next, &source, &observation)
                .unwrap()
                .is_none()
        );
        observation.all_fresh = true;
        let different = tool_source(temp.path(), "different-image", CUTTER_VERSION, 1);
        assert!(
            adopt_cutter(&release, &next, &different, &observation)
                .unwrap()
                .is_none()
        );
        let mut unproved = original;
        unproved.inputs = None;
        publish_cutter(&release, &unproved).unwrap();
        observation.all_fresh = true;
        assert!(
            adopt_cutter(&release, &next, &source, &observation)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn only_a_parsed_release_preview_can_start_away_from_its_frozen_commit() {
        let temp = aterm_tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        crate::ledger::testing::init(
            &repo,
            &[
                ("user.name", "Fixture"),
                ("user.email", "fixture@example.invalid"),
            ],
        );
        let git = GitCli::new(&repo);
        crate::ledger::git_ok(&git, &["commit", "--allow-empty", "-qm", "checkout"]).unwrap();
        let candidate = "1".repeat(40);
        let source = tool_source(temp.path(), "source", CUTTER_VERSION, 0);
        let release = temp.path().join("release");
        let cutter = stage_cutter(
            &release,
            &candidate,
            &source,
            &temp.path().join("log"),
            None,
        )
        .unwrap();
        publish_cutter(&release, &cutter).unwrap();
        let read = |args: &[&str]| {
            proved_running_at(
                &cutter.tool.path,
                &cutter_record_path(&release),
                &repo,
                Some(&args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>()),
            )
        };
        assert_eq!(
            read(&["release", "--dry-run", "--mac-only"]).unwrap(),
            candidate
        );
        // Negative controls: another dry verb, a real cut, an invalid preview and absent
        // arguments all retain startup's clean/exact-checkout requirement.
        for args in [
            &["release", "--mac-only"][..],
            &["cut", "--dry-run"],
            &["release", "--dry-run", "--unknown"],
            &["release", "--dry-run", "--forget"],
        ] {
            assert!(read(args).is_err(), "{args:?}");
        }
        assert!(
            proved_running_at(
                &cutter.tool.path,
                &cutter_record_path(&release),
                &repo,
                None
            )
            .is_err()
        );
    }

    fn tool_source(dir: &Path, name: &str, version: &str, exit: i32) -> PathBuf {
        assert!(!version.contains('\''), "the shell literal stays quoted");
        let path = dir.join(name);
        let body = format!(
            "#!/bin/sh\n\
             [ \"$#\" -eq 1 ] && [ \"$1\" = \"--version\" ] || exit 64\n\
             printf '%s\\n' '{version}'\n\
             exit {exit}\n"
        );
        aterm_tempfile::stand_in(&path, &body).expect("make tool stand-in");
        // On macOS stand-ins are symlinks into a shared executable cache. Production custody
        // correctly refuses symlink sources, so give it the regular file the stand-in names.
        std::fs::canonicalize(path).expect("resolve regular stand-in")
    }

    fn staged_fixture(base: &Path) -> (PathBuf, PathBuf, PathBuf, ToolSet) {
        let release = base.join("release");
        let cutter = tool_source(base, "source-cutter", CUTTER_VERSION, 0);
        let verify = tool_source(base, "source-verify", VERIFY_VERSION, 0);
        let tools = stage(&release, COMMIT, &cutter, &verify).expect("stage frozen tools");
        (release, cutter, verify, tools)
    }

    fn write_record(release: &Path, tools: &ToolSet) {
        let text = aterm_toml::to_string(tools).expect("serialize record");
        std::fs::write(record_path(release), text).expect("write record mutation");
    }

    fn relative_to_current(path: &Path) -> PathBuf {
        let from_path = std::fs::canonicalize(std::env::current_dir().unwrap()).unwrap();
        let to_path = std::fs::canonicalize(path).unwrap();
        let from: Vec<_> = from_path.components().collect();
        let to: Vec<_> = to_path.components().collect();
        let shared = from
            .iter()
            .zip(&to)
            .take_while(|(left, right)| left == right)
            .count();
        assert!(shared > 0, "Unix absolute paths share their root");
        let mut relative = PathBuf::new();
        for _ in shared..from.len() {
            relative.push("..");
        }
        for part in &to[shared..] {
            relative.push(part.as_os_str());
        }
        assert!(!relative.is_absolute());
        relative
    }

    #[test]
    fn frozen_tools_are_copied_hashed_proved_and_recorded() {
        use std::os::unix::fs::PermissionsExt as _;

        let temp = aterm_tempfile::tempdir().expect("tempdir");
        let (release, cutter, verify, tools) = staged_fixture(temp.path());

        assert!(!record_path(&release).exists());
        assert_eq!(tools.commit, COMMIT);
        assert_eq!(
            tools.set_sha256,
            set_digest(&tools.cutter.sha256, &tools.verify.sha256)
        );
        let frozen = release.join(".ubd/tools").join(&tools.set_sha256);
        assert_eq!(tools.cutter.path, frozen.join("aterm-release"));
        assert_eq!(tools.verify.path, frozen.join("aterm-verify"));
        for (source, tool) in [(&cutter, &tools.cutter), (&verify, &tools.verify)] {
            assert_eq!(
                std::fs::read(source).expect("read source"),
                std::fs::read(&tool.path).expect("read frozen copy")
            );
            assert_eq!(crate::dmg::sha256_file(source).unwrap(), tool.sha256);
            assert_eq!(crate::dmg::sha256_file(&tool.path).unwrap(), tool.sha256);
            assert_eq!(
                std::fs::metadata(&tool.path)
                    .expect("frozen metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o755
            );
        }

        let record = publish(&release, &tools).expect("publish record");
        assert_eq!(record, record_path(&release));
        assert_eq!(
            std::fs::metadata(&record)
                .expect("record metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(recorded(&release, COMMIT).unwrap(), tools);
    }

    #[test]
    fn a_mutated_frozen_copy_is_refused_and_never_replaced() {
        use std::io::Write as _;

        let temp = aterm_tempfile::tempdir().expect("tempdir");
        for mutate_cutter in [true, false] {
            let case = temp
                .path()
                .join(if mutate_cutter { "cutter" } else { "verify" });
            std::fs::create_dir(&case).unwrap();
            let (release, cutter, verify, tools) = staged_fixture(&case);
            publish(&release, &tools).expect("publish valid record");
            assert_eq!(recorded(&release, COMMIT).unwrap(), tools);

            let changed = if mutate_cutter {
                &tools.cutter
            } else {
                &tools.verify
            };
            let unchanged = if mutate_cutter {
                &tools.verify
            } else {
                &tools.cutter
            };
            let mut copied = std::fs::OpenOptions::new()
                .append(true)
                .open(&changed.path)
                .expect("open frozen tool for mutation");
            copied.write_all(b"\n# mutation\n").unwrap();
            #[expect(
                clippy::disallowed_methods,
                reason = "the mutation fixture must finish changing frozen bytes before their content is proved"
            )]
            copied.sync_all().unwrap();
            drop(copied);
            let mutated = std::fs::read(&changed.path).unwrap();

            assert!(recorded(&release, COMMIT).is_err());
            assert!(publish(&release, &tools).is_err());
            assert!(stage(&release, COMMIT, &cutter, &verify).is_err());
            assert_eq!(std::fs::read(&changed.path).unwrap(), mutated);
            assert_eq!(
                crate::dmg::sha256_file(&unchanged.path).unwrap(),
                unchanged.sha256
            );
        }
    }

    #[test]
    fn the_manifest_refuses_each_mutated_binding_and_unknown_fields() {
        let temp = aterm_tempfile::tempdir().expect("tempdir");
        let (release, _, _, tools) = staged_fixture(temp.path());
        publish(&release, &tools).expect("publish valid record");

        let mut mutant = tools.clone();
        mutant.schema += 1;
        write_record(&release, &mutant);
        assert!(recorded(&release, COMMIT).is_err(), "schema mutation");

        let mut mutant = tools.clone();
        mutant.commit = "1".repeat(40);
        write_record(&release, &mutant);
        assert!(recorded(&release, COMMIT).is_err(), "commit mutation");

        let mut mutant = tools.clone();
        mutant.set_sha256 = "0".repeat(64);
        write_record(&release, &mutant);
        assert!(recorded(&release, COMMIT).is_err(), "set digest mutation");

        let mut mutant = tools.clone();
        mutant.cutter.path = release
            .join(".ubd/tools")
            .join(&tools.set_sha256)
            .join("other");
        write_record(&release, &mutant);
        assert!(recorded(&release, COMMIT).is_err(), "cutter path mutation");

        let mut mutant = tools.clone();
        mutant.verify.path = release
            .join(".ubd/tools")
            .join(&tools.set_sha256)
            .join("other");
        write_record(&release, &mutant);
        assert!(recorded(&release, COMMIT).is_err(), "verify path mutation");

        let mut text = aterm_toml::to_string(&tools).expect("serialize valid record");
        text.push_str("\nunknown = true\n");
        std::fs::write(record_path(&release), text).unwrap();
        assert!(recorded(&release, COMMIT).is_err(), "unknown field");

        write_record(&release, &tools);
        assert_eq!(recorded(&release, COMMIT).unwrap(), tools);
    }

    #[test]
    fn first_exec_requires_the_exact_line_and_a_successful_exit() {
        let temp = aterm_tempfile::tempdir().expect("tempdir");
        let exact = tool_source(temp.path(), "exact", CUTTER_VERSION, 0);
        assert!(prove_exec(&exact, CUTTER_VERSION).is_ok());

        let wrong = tool_source(temp.path(), "wrong", "not the cutter version", 0);
        let error = prove_exec(&wrong, CUTTER_VERSION).unwrap_err();
        assert!(error.contains("not the exact line"), "{error}");

        let failed = tool_source(temp.path(), "failed", CUTTER_VERSION, 23);
        let error = prove_exec(&failed, CUTTER_VERSION).unwrap_err();
        assert!(error.contains("ended"), "{error}");

        let extra = temp.path().join("extra");
        aterm_tempfile::stand_in(
            &extra,
            &format!("#!/bin/sh\nprintf '%s\\n' '{}' extra\n", CUTTER_VERSION),
        )
        .unwrap();
        let extra = std::fs::canonicalize(extra).unwrap();
        let error = prove_exec(&extra, CUTTER_VERSION).unwrap_err();
        assert!(error.contains("not the exact line"), "{error}");
    }

    /// Printing the proof line is insufficient until the process exits and stdout reaches EOF.
    /// The output loop has no reader thread: this test stops it synchronously once the line is
    /// observed, and return proves cleanup cannot leave such a reader across the later exec.
    /// NEGATIVE CONTROL: returning at the line, or calling `wait` after a detached line reader,
    /// either accepts this non-exit or never returns.
    #[test]
    fn a_version_line_without_exit_is_refused_with_no_reader_left() {
        let temp = aterm_tempfile::tempdir().unwrap();
        let path = temp.path().join("line-then-hang");
        aterm_tempfile::stand_in(
            &path,
            &format!(
                "#!/bin/sh\nprintf '%s\\n' '{}'\nwhile :; do sleep 60; done\n",
                CUTTER_VERSION
            ),
        )
        .unwrap();
        let path = std::fs::canonicalize(path).unwrap();
        let expected = format!("{CUTTER_VERSION}\n");
        let mut observed = false;
        let error = prove_exec_until(
            &path,
            CUTTER_VERSION,
            std::time::Instant::now() + aterm_uds::deadline::FIRST_EXEC_HANG,
            &mut |bytes| {
                observed |= bytes == expected.as_bytes();
                observed
            },
        )
        .unwrap_err();
        assert!(
            observed,
            "the child wrote the exact line before it was stopped"
        );
        assert!(error.contains("stopped after observing output"), "{error}");
    }

    #[test]
    fn staging_requires_first_exec_proof_from_both_copies() {
        let temp = aterm_tempfile::tempdir().expect("tempdir");
        for wrong_cutter in [true, false] {
            let case = temp
                .path()
                .join(if wrong_cutter { "cutter" } else { "verify" });
            std::fs::create_dir(&case).unwrap();
            let release = case.join("release");
            let cutter = tool_source(
                &case,
                "source-cutter",
                if wrong_cutter {
                    "wrong cutter"
                } else {
                    CUTTER_VERSION
                },
                0,
            );
            let verify = tool_source(
                &case,
                "source-verify",
                if wrong_cutter {
                    VERIFY_VERSION
                } else {
                    "wrong verify"
                },
                0,
            );

            assert!(stage(&release, COMMIT, &cutter, &verify).is_err());
            assert!(!record_path(&release).exists());
        }
    }

    #[test]
    fn staging_refuses_a_relative_release_root_before_writing_it() {
        let temp = aterm_tempfile::tempdir().expect("tempdir");
        let release = relative_to_current(temp.path()).join("release");
        let cutter = tool_source(temp.path(), "source-cutter", CUTTER_VERSION, 0);
        let verify = tool_source(temp.path(), "source-verify", VERIFY_VERSION, 0);

        assert!(stage(&release, COMMIT, &cutter, &verify).is_err());
        assert!(!release.exists());
    }

    #[test]
    fn copy_refuses_a_symlink_source_without_creating_a_destination() {
        use std::os::unix::fs::symlink;

        let temp = aterm_tempfile::tempdir().expect("tempdir");
        let source = tool_source(temp.path(), "source", CUTTER_VERSION, 0);
        let linked = temp.path().join("linked-source");
        let destination = temp.path().join("destination");
        symlink(source, &linked).expect("make source symlink");

        assert!(copy_executable(&linked, &destination).is_err());
        assert!(!destination.exists());
    }

    /// Content-addressed leaf names do not make a symlinked storage root or set immutable. Both
    /// are refused before a tool can be written through them. Replacing `symlink_metadata` with
    /// `exists` follows either link and makes these negative controls fail.
    #[test]
    fn staging_refuses_symlinked_storage_directories() {
        use std::os::unix::fs::symlink;

        for link_root in [true, false] {
            let temp = aterm_tempfile::tempdir().unwrap();
            let release = temp.path().join("release");
            let outside = temp.path().join("compiler-owned");
            std::fs::create_dir_all(&outside).unwrap();
            let cutter = tool_source(temp.path(), "source-cutter", CUTTER_VERSION, 0);
            let verify = tool_source(temp.path(), "source-verify", VERIFY_VERSION, 0);
            if link_root {
                std::fs::create_dir_all(release.join(".ubd")).unwrap();
                symlink(&outside, release.join(".ubd/tools")).unwrap();
            } else {
                let prototype = ToolSet::new(
                    COMMIT,
                    Tool {
                        path: PathBuf::new(),
                        sha256: crate::dmg::sha256_file(&cutter).unwrap(),
                    },
                    Tool {
                        path: PathBuf::new(),
                        sha256: crate::dmg::sha256_file(&verify).unwrap(),
                    },
                );
                std::fs::create_dir_all(release.join(".ubd/tools")).unwrap();
                symlink(
                    &outside,
                    release.join(".ubd/tools").join(prototype.set_sha256),
                )
                .unwrap();
            }

            let error = stage(&release, COMMIT, &cutter, &verify).unwrap_err();
            assert!(error.contains("not a real directory"), "{error}");
            assert!(std::fs::read_dir(&outside).unwrap().next().is_none());
        }
    }
}
