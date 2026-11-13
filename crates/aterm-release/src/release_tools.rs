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
#[path = "release_tools_tests.rs"]
mod tests;
