// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A cached bundle can outlive its release journal: a process or a service may still use it.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[path = "artifact_marker.rs"]
mod marker;
#[cfg(any(target_os = "macos", test))]
#[path = "artifact_service.rs"]
mod service;

// Replacing an unfinished same-key build needs a complete use observation.
// Scheduling delay is no evidence that its original identity or custody changed.
pub(super) const OBSERVATION_HANG: Duration = Duration::from_secs(60);
// Optional pruning can leave old entries for the next pass without refusing a build.
pub(super) const PRUNE_BUDGET: Duration = Duration::from_secs(5);
#[cfg(any(target_os = "macos", test))]
const MAX_UNITS: usize = 1024;

#[derive(Clone)]
struct Reference {
    program: PathBuf,
    source: String,
}

/// Deletion is optional. Every unreadable use is a reason to retain, including when a
/// damaged same-key entry would otherwise be replaced by the next build.
pub(super) fn remove_unused(dir: &Path) -> Result<bool, String> {
    remove_unused_until(dir, Instant::now() + OBSERVATION_HANG)
}

pub(super) fn remove_unused_until(dir: &Path, deadline: Instant) -> Result<bool, String> {
    remove_observed(dir, deadline, || references(deadline))
}

fn remove_observed(
    dir: &Path,
    deadline: Instant,
    mut read: impl FnMut() -> Result<Vec<Reference>, String>,
) -> Result<bool, String> {
    let before = match std::fs::symlink_metadata(dir) {
        Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => meta,
        Ok(_) => return Err("artifact entry is not a regular directory".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("artifact directory could not be read: {error}")),
    };
    for _ in 0..2 {
        remaining(deadline)?;
        if let Some(left) = marker::hold(dir)? {
            return Err(format!(
                "an unproved first execution retains it for {} s",
                left.as_secs()
            ));
        }
        let mut canonical_dir = None;
        for reference in read()? {
            remaining(deadline)?;
            if points_into_cached(&reference.program, dir, &mut canonical_dir)? {
                return Err(format!(
                    "{} still references this artifact",
                    reference.source
                ));
            }
        }
    }
    let after = std::fs::symlink_metadata(dir)
        .map_err(|error| format!("artifact changed before removal: {error}"))?;
    if !same_directory(&before, &after) {
        return Err("artifact directory changed before removal".into());
    }
    if marker::hold(dir)?.is_some() {
        return Err("a first execution started during the artifact observation".into());
    }
    remaining(deadline)?;
    std::fs::remove_dir_all(dir)
        .map(|()| true)
        .map_err(|error| error.to_string())
}

#[cfg(unix)]
fn same_directory(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    b.is_dir() && !b.file_type().is_symlink() && (a.dev(), a.ino()) == (b.dev(), b.ino())
}

#[cfg(not(unix))]
fn same_directory(_: &std::fs::Metadata, _: &std::fs::Metadata) -> bool {
    false
}

#[cfg(test)]
fn points_into(program: &Path, dir: &Path) -> Result<bool, String> {
    points_into_cached(program, dir, &mut None)
}

fn points_into_cached(
    program: &Path,
    dir: &Path,
    canonical_dir: &mut Option<PathBuf>,
) -> Result<bool, String> {
    if !program.is_absolute() {
        return Err("a service or process executable path is relative".into());
    }
    // Preserve the lexical spelling too: the kernel may still name an image which was
    // unlinked, or a service may refer to a not-yet-restored file below this directory.
    if descendant(program, dir) {
        return Ok(true);
    }
    let canonical_dir = match canonical_dir {
        Some(cached) => cached.as_path(),
        None => {
            let resolved = dir.canonicalize().map_err(|error| error.to_string())?;
            canonical_dir.insert(resolved).as_path()
        }
    };
    if descendant(program, canonical_dir) {
        return Ok(true);
    }
    Ok(descendant(&resolve_missing(program)?, canonical_dir))
}

fn descendant(path: &Path, directory: &Path) -> bool {
    fn form(p: &Path) -> &Path {
        p.strip_prefix("/System/Volumes/Data")
            .or_else(|_| p.strip_prefix("/"))
            .unwrap_or(p)
    }
    // macOS exposes a data-volume vnode under either spelling. This is the same
    // conservative alias used by the shared path-holder reader.
    path.starts_with(directory)
        || (path.is_absolute() == directory.is_absolute()
            && form(path).starts_with(form(directory)))
}

fn resolve_missing(program: &Path) -> Result<PathBuf, String> {
    let mut prefix = program;
    let mut suffix = Vec::new();
    for _ in 0..256 {
        match prefix.canonicalize() {
            Ok(mut resolved) => {
                for part in suffix.iter().rev() {
                    resolved.push(part);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match std::fs::symlink_metadata(prefix) {
                    Ok(_) => return Err("a referenced executable has an unresolved link".into()),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(format!("a referenced executable is unreadable: {e}")),
                }
                suffix.push(
                    prefix
                        .file_name()
                        .ok_or("a referenced path cannot be resolved")?
                        .to_owned(),
                );
                prefix = prefix.parent().ok_or("a referenced path has no parent")?;
            }
            Err(error) => {
                return Err(format!(
                    "a referenced executable cannot be resolved: {error}"
                ));
            }
        }
    }
    Err("a referenced path exceeds the resolution bound".into())
}

fn references(deadline: Instant) -> Result<Vec<Reference>, String> {
    remaining(deadline)?;
    let mut references: Vec<_> = atpkg::gc::running_processes()
        .ok_or("the process list is unreadable; artifact use is unknown")?
        .into_iter()
        .map(|(pid, path)| Reference {
            program: path,
            source: format!("pid {pid}"),
        })
        .collect();
    remaining(deadline)?;
    references.extend(service_references(deadline)?);
    remaining(deadline)?;
    Ok(references)
}

fn remaining(deadline: Instant) -> Result<(), String> {
    if Instant::now() >= deadline {
        Err("artifact use observation reached its deadline".into())
    } else {
        Ok(())
    }
}

#[cfg(any(target_os = "macos", test))]
fn managed(label: &str) -> bool {
    label.starts_with("systems.alab.") || label.starts_with("com.aterm.")
}

#[cfg(any(target_os = "macos", test))]
fn label_shape(label: &str) -> bool {
    label
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b".-_".contains(&b))
}

#[cfg(target_os = "macos")]
fn service_references(deadline: Instant) -> Result<Vec<Reference>, String> {
    let home = aterm_uds::account::home().ok_or("the account home is unreadable")?;
    let mut references = configured(&[
        home.join("Library/LaunchAgents"),
        PathBuf::from("/Library/LaunchAgents"),
        PathBuf::from("/Library/LaunchDaemons"),
    ])?;
    let uid = aterm_uds::peer::our_uid();
    for domain in [format!("gui/{uid}"), format!("user/{uid}"), "system".into()] {
        // A successful domain read also covers loaded jobs whose plist was removed.
        // Refusal (including an unavailable domain) conservatively retains the cache.
        let text = launchctl_print(&domain, deadline)?;
        for label in labels(&text)? {
            let target = format!("{domain}/{label}");
            let text = launchctl_print(&target, deadline)?;
            references.push(Reference {
                program: loaded_program(&text)?,
                source: format!("loaded service {target}"),
            });
        }
    }
    Ok(references)
}

#[cfg(not(target_os = "macos"))]
fn service_references(_: Instant) -> Result<Vec<Reference>, String> {
    Err("loaded service references cannot be completely observed on this platform".into())
}

#[cfg(any(target_os = "macos", test))]
#[cfg_attr(
    all(test, not(target_os = "macos")),
    expect(
        dead_code,
        reason = "the tests that read service directories run on macOS alone"
    )
)]
fn configured(directories: &[PathBuf]) -> Result<Vec<Reference>, String> {
    let mut references = Vec::new();
    for directory in directories {
        match std::fs::symlink_metadata(directory) {
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
            Ok(_) => return Err("a service directory is not a regular directory".into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("service directory is unreadable: {error}")),
        }
        let entries = std::fs::read_dir(directory).map_err(|error| error.to_string())?;
        for (count, entry) in entries.enumerate() {
            if count >= MAX_UNITS {
                return Err("service directory exceeds the observation bound".into());
            }
            let entry = entry.map_err(|error| error.to_string())?;
            let name = entry.file_name();
            let name = name.to_str().ok_or("a service file name is unreadable")?;
            let Some(label) = name.strip_suffix(".plist") else {
                continue;
            };
            if !managed(label) {
                continue;
            }
            if !label_shape(label) {
                return Err("a configured service label is ambiguous".into());
            }
            if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
                return Err(format!("configured service {label} is not a regular file"));
            }
            let text = crate::durable::read_regular_text(&entry.path(), 256 * 1024)
                .map_err(|_| format!("configured service {label} is unreadable"))?;
            let program = service::program(&text).map_err(|why| {
                format!("configured service {label} has an unreadable executable: {why}")
            })?;
            references.push(Reference {
                program,
                source: format!("configured service {label}"),
            });
        }
    }
    Ok(references)
}

#[cfg(target_os = "macos")]
fn launchctl_print(target: &str, deadline: Instant) -> Result<String, String> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err("loaded service observation reached its deadline".into());
    }
    let mut command = aterm_uds::spawnfd::command(aterm_secrets::service::LAUNCHCTL);
    command
        .args(["print", target])
        .stdin(aterm_uds::spawnfd::Stdio::null());
    let output = aterm_uds::deadline::output_until_complete_stdout(&mut command, deadline)
        .map_err(|_| "loaded service observation could not finish")?;
    if !output.status.success() {
        return Err("loaded service observation is incomplete".into());
    }
    String::from_utf8(output.stdout).map_err(|_| "loaded service observation is not UTF-8".into())
}

#[cfg(any(target_os = "macos", test))]
fn labels(text: &str) -> Result<Vec<String>, String> {
    let mut labels = std::collections::BTreeSet::new();
    let mut lines = text.lines();
    if !lines.any(|line| line.trim() == "services = {") {
        return Err("the loaded service list has no complete section".into());
    }
    let mut complete = false;
    for (count, line) in lines.enumerate() {
        let line = line.trim();
        if matches!(line, "}" | "};") {
            complete = true;
            break;
        }
        if line.is_empty() {
            continue;
        }
        if count >= MAX_UNITS {
            return Err("loaded service list exceeds the observation bound".into());
        }
        let mut words = line.split_whitespace();
        let (Some(_), Some(_), Some(raw_label), None) =
            (words.next(), words.next(), words.next(), words.next())
        else {
            return Err("the loaded service list has an unfamiliar row".into());
        };
        let label = raw_label.trim_matches(['"', ';']);
        if managed(label) {
            if !label_shape(label) {
                return Err("loaded service label is ambiguous".into());
            }
            labels.insert(label.to_owned());
        }
    }
    if !complete {
        return Err("the loaded service list is incomplete".into());
    }
    Ok(labels.into_iter().collect())
}

#[cfg(any(target_os = "macos", test))]
fn loaded_program(text: &str) -> Result<PathBuf, String> {
    let mut programs = text
        .lines()
        .filter_map(|line| line.strip_prefix("\tprogram = "));
    let program = programs
        .next()
        .ok_or("loaded service has no executable path")?;
    if programs.next().is_some()
        || program.contains(['\\', '"'])
        || !Path::new(program).is_absolute()
    {
        return Err("loaded service executable is ambiguous".into());
    }
    Ok(PathBuf::from(program))
}

#[cfg(test)]
#[path = "artifact_use_tests.rs"]
mod tests;
