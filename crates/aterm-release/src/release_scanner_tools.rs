// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The publication engine owns scanner discovery, functional probes and byte validation.
//! This caller retains its bounded report outside the candidate and carries the locator.

use aterm_uds::spawnfd::{Command, command};
use std::path::{Path, PathBuf};

pub(super) const ENV: &str = "PUBLISH_SCANNER_TOOLS";
const MAX_REPORT: usize = 64 * 1024;
// The engine gives each of its sequential functional probes a one-minute hang detector.
const WITHIN: std::time::Duration = std::time::Duration::from_secs(240);

pub(super) type Probe = fn(&mut Command) -> Result<std::process::Output, String>;

pub(super) struct Report {
    path: PathBuf,
}

impl Report {
    pub(super) fn apply(&self, cmd: &mut Command) {
        cmd.env(ENV, &self.path);
    }
}

/// Preserve the actual publication command's environment, including removals and
/// alternate export PATH, without carrying its operation or an older report.
fn probe_command(publication: &Command) -> Command {
    let mut cmd = command(publication.get_program());
    if let Some(dir) = publication.get_current_dir() {
        cmd.current_dir(dir);
    }
    for (name, value) in publication.get_envs() {
        match value {
            Some(value) => {
                cmd.env(name, value);
            }
            None => {
                cmd.env_remove(name);
            }
        }
    }
    cmd.env_remove(ENV).arg("scanner-preflight");
    cmd
}

pub(super) fn run(cmd: &mut Command) -> Result<std::process::Output, String> {
    aterm_uds::deadline::output_until_complete_stdout(cmd, crate::durable::deadline_after(WITHIN))
        .map_err(|error| format!("engine scanner probes could not finish: {error}"))
}

pub(super) fn prepare(
    publication: &Command,
    destination: &Path,
    candidates: &[&Path],
    probe: impl FnOnce(&mut Command) -> Result<std::process::Output, String>,
) -> Result<Report, String> {
    let path = outside_candidate(destination, candidates)?;
    let mut cmd = probe_command(publication);
    let output = probe(&mut cmd)?;
    if !output.status.success() {
        let detail = [&output.stderr, &output.stdout]
            .into_iter()
            .map(|bytes| {
                String::from_utf8_lossy(&bytes[bytes.len().saturating_sub(4096)..]).into_owned()
            })
            .collect::<Vec<_>>()
            .join("\n");
        return Err(format!(
            "engine scanner probes failed ({})\n{detail}",
            output.status
        ));
    }
    validate(&output.stdout)?;
    crate::durable::write_atomic_durable(&path, &output.stdout, 0o600)
        .map_err(|error| format!("retain scanner report: {error}"))?;
    Ok(Report { path })
}

fn outside_candidate(destination: &Path, candidates: &[&Path]) -> Result<PathBuf, String> {
    if !destination.is_absolute() {
        return Err("scanner report must have an absolute path outside the candidate".into());
    }
    let parent = destination.parent().ok_or("scanner report has no parent")?;
    let parent = parent
        .canonicalize()
        .map_err(|error| format!("scanner report directory: {error}"))?;
    for candidate in candidates {
        let candidate = candidate
            .canonicalize()
            .map_err(|error| format!("candidate directory: {error}"))?;
        if parent.starts_with(candidate) {
            return Err("scanner report would change the candidate tree".into());
        }
    }
    Ok(parent.join(
        destination
            .file_name()
            .ok_or("scanner report has no filename")?,
    ))
}

fn validate(bytes: &[u8]) -> Result<(), String> {
    let invalid = || "engine returned an unsupported or malformed scanner report".to_owned();
    if bytes.len() > MAX_REPORT {
        return Err("engine scanner report exceeds 64 KiB".into());
    }
    let value: aterm_json::Value = aterm_json::from_slice(bytes).map_err(|_| invalid())?;
    let object = value.as_object().ok_or_else(invalid)?;
    if object.len() != 3
        || value["schema"].as_u64() != Some(1)
        || value["kind"].as_str() != Some("publication-scanner-tools")
    {
        return Err(invalid());
    }
    let tools = value["tools"]
        .as_object()
        .filter(|tools| !tools.is_empty())
        .ok_or_else(invalid)?;
    // Names and their required set belong to the engine. Validate the wire
    // shape here; the engine revalidates the selected executable bytes at use.
    for tool in tools.values() {
        let entry = tool.as_object().ok_or_else(invalid)?;
        let path = tool["path"].as_str().ok_or_else(invalid)?;
        let hash = tool["sha256"].as_str().ok_or_else(invalid)?;
        if entry.len() != 2
            || !Path::new(path).is_absolute()
            || path.contains('\0')
            || hash.len() != 64
            || !hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(invalid());
        }
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn test_output() -> std::process::Output {
    #[cfg(unix)]
    use std::os::unix::process::ExitStatusExt as _;
    #[cfg(windows)]
    use std::os::windows::process::ExitStatusExt as _;
    std::process::Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: aterm_json::json!({"schema":1,"kind":"publication-scanner-tools",
                "tools":{"engine-owned-name":{"path":std::env::current_exe().unwrap(),"sha256":"a".repeat(64)}}}).to_string().into_bytes(),
            stderr: Vec::new(),
        }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_uses_exact_publication_environment_and_stays_outside_candidate() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let candidate = dir.path().join("candidate");
        std::fs::create_dir(&candidate).unwrap();
        let mut publication = command("/engine/bin/pub");
        publication
            .current_dir(&candidate)
            .env("PATH", "/actual/export/path")
            .env("PUBLICATION_ENGINE", "/scratch/engine")
            .env("ATPKG_KEYS", "/selected/helper")
            .env(ENV, "/old/report.json")
            .env_remove("RUSTFLAGS")
            .arg("stage");
        let report = prepare(
            &publication,
            &dir.path().join("report.json"),
            &[&candidate],
            |cmd| {
                assert_eq!(cmd.get_program(), publication.get_program());
                assert_eq!(cmd.get_current_dir(), publication.get_current_dir());
                assert_eq!(
                    cmd.get_args().collect::<Vec<_>>(),
                    [std::ffi::OsStr::new("scanner-preflight")]
                );
                for (name, value) in publication.get_envs().filter(|(name, _)| *name != ENV) {
                    assert!(cmd.get_envs().any(|pair| pair == (name, value)));
                }
                assert!(
                    cmd.get_envs()
                        .any(|(name, value)| name == ENV && value.is_none())
                );
                Ok(test_output())
            },
        )
        .unwrap();
        let mut stage = publication;
        report.apply(&mut stage);
        assert!(
            stage
                .get_envs()
                .any(|(name, value)| name == ENV && value == Some(report.path.as_os_str()))
        );
        assert_eq!(std::fs::read_dir(&candidate).unwrap().count(), 0);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(report.path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn unsafe_location_and_failed_or_malformed_probes_never_produce_handoff() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let candidate = dir.path().join("candidate");
        std::fs::create_dir(&candidate).unwrap();
        let cmd = command("/engine/bin/pub");
        assert!(
            prepare(
                &cmd,
                &candidate.join("report.json"),
                &[&candidate],
                |_| panic!("must refuse before launching")
            )
            .is_err()
        );
        for bytes in [
            b"old engine help".as_slice(),
            b"{}",
            b"{\"schema\":1,\"kind\":\"publication-scanner-tools\",\"tools\":{}}",
        ] {
            let result = prepare(&cmd, &dir.path().join("report.json"), &[&candidate], |_| {
                let mut out = test_output();
                out.stdout = bytes.to_vec();
                Ok(out)
            });
            assert!(result.is_err());
            assert!(!dir.path().join("report.json").exists());
        }
        assert!(
            prepare(&cmd, &dir.path().join("report.json"), &[&candidate], |_| {
                Err("missing or broken engine".into())
            })
            .is_err()
        );
        assert!(
            prepare(&cmd, &dir.path().join("report.json"), &[&candidate], |_| {
                let mut out = test_output();
                out.stdout = vec![b' '; MAX_REPORT + 1];
                Ok(out)
            })
            .is_err()
        );
        #[cfg(unix)]
        {
            let alias = dir.path().join("candidate-alias");
            std::os::unix::fs::symlink(&candidate, &alias).unwrap();
            assert!(
                prepare(&cmd, &alias.join("report.json"), &[&candidate], |_| panic!(
                    "alias must refuse before launching"
                ))
                .is_err()
            );
        }
    }

    #[test]
    fn malformed_tool_identity_is_not_a_preflight_pass() {
        let absolute = std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        for (path, hash) in [
            ("relative/tool".to_owned(), "a".repeat(64)),
            (absolute.clone(), "A".repeat(64)),
            (absolute, "a".repeat(63)),
        ] {
            let bytes = aterm_json::json!({"schema":1,"kind":"publication-scanner-tools",
                "tools":{"engine-owned-name":{"path":path,"sha256":hash}}})
            .to_string();
            assert!(validate(bytes.as_bytes()).is_err());
        }
    }
}
