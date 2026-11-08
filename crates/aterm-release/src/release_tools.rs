// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Frozen, content-addressed release tools and the proof used by an in-place cutter exec.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ledger::{GitCli, GitRunner as _};

pub const CUTTER_VERSION: &str = "aterm-release tool 1";
pub const VERIFY_VERSION: &str = aterm_verify::FROZEN_TOOL_VERSION;
pub const RECORD_ENV: &str = "ATERM_RELEASE_TOOL_RECORD";
const RECORD: &str = "tool.toml";
const SCHEMA: u32 = 1;

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
        let dir = release.join("tools").join(&self.set_sha256);
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

fn set_digest(cutter: &str, verify: &str) -> String {
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
    release.join(RECORD)
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
    let tools_dir = release.join("tools");
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

/// Validate the explicit proof handed to a copied cutter at startup. With no proof variable,
/// answer `Ok(None)` so hand-run verbs retain the dep-info judgment.
pub fn proved_running(exe: &Path) -> Result<Option<String>, String> {
    let Some(record) = std::env::var_os(RECORD_ENV) else {
        return Ok(None);
    };
    let record = PathBuf::from(record);
    if !record.is_absolute() || record.file_name().and_then(|n| n.to_str()) != Some(RECORD) {
        return Err("the frozen-tool proof does not name an absolute tool.toml".into());
    }
    let release = record
        .parent()
        .ok_or("the frozen-tool proof has no release directory")?;
    let text = crate::durable::read_text(&record)
        .map_err(|e| format!("read frozen-tool proof {}: {e}", record.display()))?;
    let tools: ToolSet = aterm_toml::from_str(&text)
        .map_err(|e| format!("parse frozen-tool proof {}: {e}", record.display()))?;
    tools.validate_shape(release)?;
    tools.validate_bytes()?;
    let actual = std::fs::canonicalize(exe)
        .map_err(|e| format!("resolve running cutter {}: {e}", exe.display()))?;
    let recorded = std::fs::canonicalize(&tools.cutter.path)
        .map_err(|e| format!("resolve frozen cutter {}: {e}", tools.cutter.path.display()))?;
    if actual != recorded {
        return Err(format!(
            "the running cutter {} is not the recorded frozen cutter {}",
            actual.display(),
            recorded.display()
        ));
    }
    let repo = std::env::current_dir().map_err(|e| format!("name the release clone: {e}"))?;
    clean_at(&repo, &tools.commit)?;
    Ok(Some(tools.commit))
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
        let frozen = release.join("tools").join(&tools.set_sha256);
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
        mutant.cutter.path = release.join("tools").join(&tools.set_sha256).join("other");
        write_record(&release, &mutant);
        assert!(recorded(&release, COMMIT).is_err(), "cutter path mutation");

        let mut mutant = tools.clone();
        mutant.verify.path = release.join("tools").join(&tools.set_sha256).join("other");
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
                std::fs::create_dir_all(&release).unwrap();
                symlink(&outside, release.join("tools")).unwrap();
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
                std::fs::create_dir_all(release.join("tools")).unwrap();
                symlink(&outside, release.join("tools").join(prototype.set_sha256)).unwrap();
            }

            let error = stage(&release, COMMIT, &cutter, &verify).unwrap_err();
            assert!(error.contains("not a real directory"), "{error}");
            assert!(std::fs::read_dir(&outside).unwrap().next().is_none());
        }
    }
}
