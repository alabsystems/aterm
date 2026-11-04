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

pub(super) const LOOKUP_BUDGET: Duration = Duration::from_secs(5);
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
    remove_unused_until(dir, Instant::now() + LOOKUP_BUDGET)
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
        for reference in read()? {
            remaining(deadline)?;
            if points_into(&reference.program, dir)? {
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

fn points_into(program: &Path, dir: &Path) -> Result<bool, String> {
    if !program.is_absolute() {
        return Err("a service or process executable path is relative".into());
    }
    // Preserve the lexical spelling too: the kernel may still name an image which was
    // unlinked, or a service may refer to a not-yet-restored file below this directory.
    if descendant(program, dir) {
        return Ok(true);
    }
    let canonical_dir = dir.canonicalize().map_err(|error| error.to_string())?;
    if descendant(program, &canonical_dir) {
        return Ok(true);
    }
    Ok(descendant(&resolve_missing(program)?, &canonical_dir))
}

fn descendant(path: &Path, directory: &Path) -> bool {
    let form = |path: &Path| {
        path.strip_prefix("/System/Volumes/Data")
            .map_or_else(|_| path.to_path_buf(), |tail| Path::new("/").join(tail))
    };
    // macOS exposes a data-volume vnode under either spelling. This is the same
    // conservative alias used by the shared path-holder reader.
    path.starts_with(directory) || form(path).starts_with(form(directory))
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
        let words: Vec<_> = line.split_whitespace().collect();
        if words.len() != 3 {
            return Err("the loaded service list has an unfamiliar row".into());
        }
        let label = words[2].trim_matches(['"', ';']);
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
mod tests {
    use super::*;

    fn remove_with(
        dir: &Path,
        read: impl FnMut() -> Result<Vec<Reference>, String>,
    ) -> Result<bool, String> {
        remove_observed(dir, Instant::now() + aterm_spec::wait::MUST_HAPPEN, read)
    }

    fn artifact(root: &Path, name: &str) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(dir.join("aterm.app/Contents/MacOS")).unwrap();
        std::fs::write(dir.join("aterm.app/Contents/MacOS/aterm"), "image").unwrap();
        dir
    }

    fn reference(program: PathBuf) -> Reference {
        Reference {
            program,
            source: "test service".into(),
        }
    }

    #[test]
    fn live_and_configured_artifacts_stay_and_unused_artifacts_go() {
        let root = aterm_tempfile::tempdir().unwrap();
        let live = artifact(root.path(), "live");
        let configured = artifact(root.path(), "configured");
        let unused = artifact(root.path(), "unused");
        let refs = vec![
            reference(live.join("aterm.app/Contents/MacOS/aterm")),
            reference(configured.join("missing-image")),
        ];
        assert!(remove_with(&live, || Ok(refs.clone())).is_err());
        assert!(remove_with(&configured, || Ok(refs.clone())).is_err());
        let removed = remove_with(&unused, || Ok(refs.clone()));
        #[cfg(unix)]
        {
            assert!(removed.unwrap());
            assert!(!unused.exists());
        }
        #[cfg(not(unix))]
        {
            assert!(removed.is_err(), "directory identity is unavailable");
            assert!(unused.exists());
        }
        assert!(live.exists() && configured.exists());
    }

    #[test]
    fn incomplete_or_late_observation_never_removes_an_artifact() {
        let root = aterm_tempfile::tempdir().unwrap();
        let dir = artifact(root.path(), "entry");
        assert!(remove_with(&dir, || Err("unreadable process or service".into())).is_err());
        let mut count = 0;
        assert!(
            remove_with(&dir, || {
                count += 1;
                Ok(if count == 1 {
                    vec![]
                } else {
                    vec![reference(dir.join("aterm.app/Contents/MacOS/aterm"))]
                })
            })
            .is_err()
        );
        assert_eq!(count, 2);
        assert!(dir.exists());
    }

    #[test]
    fn an_expired_observation_removes_nothing() {
        let root = aterm_tempfile::tempdir().unwrap();
        let dir = artifact(root.path(), "entry");
        assert!(
            remove_observed(&dir, Instant::now(), || panic!("expired before reading")).is_err()
        );
        assert!(dir.exists());
    }

    #[test]
    fn a_replaced_directory_or_new_first_execution_is_never_removed() {
        let root = aterm_tempfile::tempdir().unwrap();
        let dir = artifact(root.path(), "entry");
        let moved = root.path().join("old-entry");
        let mut count = 0;
        assert!(
            remove_with(&dir, || {
                count += 1;
                if count == 2 {
                    std::fs::rename(&dir, &moved).unwrap();
                    artifact(root.path(), "entry");
                }
                Ok(vec![])
            })
            .is_err()
        );
        assert!(dir.exists() && moved.exists());
        let mut count = 0;
        assert!(
            remove_with(&dir, || {
                count += 1;
                if count == 2 {
                    aterm_uds::deadline::stamp_exec(&dir);
                }
                Ok(vec![])
            })
            .is_err()
        );
        assert!(dir.exists());
    }

    #[test]
    fn first_execution_is_retained_and_sibling_prefix_is_not_a_reference() {
        let root = aterm_tempfile::tempdir().unwrap();
        let held = artifact(root.path(), "entry");
        aterm_uds::deadline::stamp_exec(&held);
        assert!(remove_with(&held, || Ok(vec![])).is_err());
        let sibling = artifact(root.path(), "entry-other");
        assert!(!points_into(&sibling.join("aterm.app/Contents/MacOS/aterm"), &held).unwrap());
        assert!(held.exists());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn symlink_and_real_service_unit_preserve_the_target() {
        let root = aterm_tempfile::tempdir().unwrap();
        let dir = artifact(root.path(), "entry");
        let alias = root.path().join("alias");
        std::os::unix::fs::symlink(dir.join("aterm.app/Contents/MacOS/aterm"), &alias).unwrap();
        assert!(points_into(&alias, &dir).unwrap());
        let parent_alias = root.path().join("alias-directory");
        std::os::unix::fs::symlink(&dir, &parent_alias).unwrap();
        assert!(points_into(&parent_alias.join("missing-image"), &dir).unwrap());
        let services = root.path().join("LaunchAgents");
        std::fs::create_dir(&services).unwrap();
        let text = format!(
            "<plist><dict><key>ProgramArguments</key><array><string>{}</string><string>node</string><string>serve</string></array></dict></plist>",
            alias.display()
        );
        std::fs::write(services.join("systems.alab.aterm-node.plist"), text).unwrap();
        assert!(remove_with(&dir, || configured(std::slice::from_ref(&services))).is_err());
        std::fs::write(services.join("systems.alab.aterm-node.plist"), "broken").unwrap();
        assert!(configured(&[services]).is_err());
        assert!(dir.exists());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn an_inactive_broker_keeps_its_configured_artifact() {
        let root = aterm_tempfile::tempdir().unwrap();
        let dir = artifact(root.path(), "entry");
        let services = root.path().join("LaunchAgents");
        std::fs::create_dir(&services).unwrap();
        let program = dir.join("aterm.app/Contents/MacOS/aterm");
        let unit = services.join("systems.alab.astream-broker.plist");
        let text = format!(
            "<plist><dict><key>ProgramArguments</key><array><string>{}</string><string>broker</string></array></dict></plist>",
            program.display()
        );
        std::fs::write(&unit, &text).unwrap();
        let references = configured(&[services]).unwrap();
        assert_eq!(references.len(), 1);
        assert_eq!(references[0].program, program);
        assert!(remove_with(&dir, || Ok(references.clone())).is_err());
        assert!(dir.exists());
        assert_eq!(std::fs::read_to_string(unit).unwrap(), text);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn an_explicit_configured_program_retains_its_artifact_when_argv0_differs() {
        let root = aterm_tempfile::tempdir().unwrap();
        let unused = artifact(root.path(), "unused");
        assert!(remove_with(&unused, || Ok(vec![])).unwrap());
        assert!(!unused.exists());
        let services = root.path().join("LaunchAgents");
        std::fs::create_dir(&services).unwrap();
        let unit = services.join("systems.alab.aterm-node.plist");
        let ordinary = artifact(root.path(), "ordinary");
        let ordinary_program = ordinary.join("aterm.app/Contents/MacOS/aterm");
        let xml = |program: Option<&Path>, argv0: &Path| {
            format!(
                "<plist><dict>{}<key>ProgramArguments</key><array><string>{}</string></array></dict></plist>",
                program.map_or_else(String::new, |program| format!(
                    "<key>Program</key><string>{}</string>",
                    aterm_codec::xml::escape(&program.display().to_string())
                )),
                aterm_codec::xml::escape(&argv0.display().to_string())
            )
        };
        std::fs::write(&unit, xml(None, &ordinary_program)).unwrap();
        assert!(remove_with(&ordinary, || configured(std::slice::from_ref(&services))).is_err());
        assert!(
            ordinary.exists(),
            "ordinary ProgramArguments retains its artifact"
        );

        let dir = artifact(root.path(), "explicit-program");
        let program = dir.join("aterm.app/Contents/MacOS/aterm");
        let text = xml(Some(&program), Path::new("/usr/bin/true"));
        std::fs::write(&unit, &text).unwrap();
        let removed = remove_with(&dir, || configured(std::slice::from_ref(&services)));
        assert!(
            dir.exists(),
            "Program selects the actual executable: {removed:?}"
        );
        assert!(removed.is_err());
        assert_eq!(std::fs::read_to_string(unit).unwrap(), text);
    }

    #[cfg(unix)]
    #[test]
    fn malformed_exec_evidence_never_becomes_an_expired_first_execution() {
        let root = aterm_tempfile::tempdir().unwrap();
        let unused = artifact(root.path(), "unused");
        assert!(remove_with(&unused, || Ok(vec![])).unwrap());
        assert!(!unused.exists());
        for (label, bytes) in [
            ("blank", b"".as_slice()),
            ("invalid", b"not-a-clock".as_slice()),
        ] {
            let dir = artifact(root.path(), label);
            let stamp = dir.join(aterm_uds::deadline::EXEC_STAMP);
            std::fs::write(&stamp, bytes).unwrap();
            // A retry may overwrite an existing marker without updating its old parent mtime.
            // Do not let malformed current evidence borrow that old age and authorize removal.
            std::fs::File::open(&dir)
                .unwrap()
                .set_times(
                    std::fs::FileTimes::new().set_modified(std::time::SystemTime::UNIX_EPOCH),
                )
                .unwrap();
            let removed = remove_with(&dir, || Ok(vec![]));
            assert!(
                dir.exists(),
                "unreadable exec evidence must retain {label}: {removed:?}"
            );
            assert!(removed.is_err());
            assert_eq!(std::fs::read(stamp).unwrap(), bytes);
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn a_writerless_exec_fifo_retains_the_artifact_without_blocking() {
        use std::os::unix::ffi::OsStrExt as _;
        #[cfg(target_os = "macos")]
        type Mode = u16;
        #[cfg(target_os = "linux")]
        type Mode = u32;
        unsafe extern "C" {
            fn mkfifo(path: *const std::ffi::c_char, mode: Mode) -> std::ffi::c_int;
        }
        let root = aterm_tempfile::tempdir().unwrap();
        let dir = artifact(root.path(), "fifo-marker");
        let path = std::ffi::CString::new(
            dir.join(aterm_uds::deadline::EXEC_STAMP)
                .as_os_str()
                .as_bytes(),
        )
        .unwrap();
        // SAFETY: path is a live NUL-terminated owned scratch name; the cfg-specific Mode
        // matches this platform's mode_t, and mkfifo keeps no pointer after the call.
        assert_eq!(unsafe { mkfifo(path.as_ptr(), 0o600) }, 0);
        let observed = dir.clone();
        let (send, received) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _ = send.send(remove_with(&observed, || Ok(vec![])));
        });
        // A regression must fail at the hang detector rather than blocking the test runner's
        // join. Only inert scratch is involved; this test is never run on the OLD reader.
        let result = received
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("nonblocking marker observation within the one-minute hang detector");
        worker.join().unwrap();
        assert!(result.is_err());
        assert!(dir.exists());
    }

    #[cfg(unix)]
    #[test]
    fn path_bytes_and_data_volume_aliases_remain_references() {
        use std::os::unix::ffi::OsStringExt as _;
        let root = aterm_tempfile::tempdir().unwrap();
        let dir = root
            .path()
            .join(std::ffi::OsString::from_vec(b"entry-\xff".to_vec()));
        // APFS rejects such names; lexical retention also covers a missing image,
        // so this byte-level check needs no filesystem entry.
        assert!(points_into(&dir.join("missing-image"), &dir).unwrap());
        assert!(descendant(
            Path::new("/System/Volumes/Data/Users//a/cache/entry/aterm"),
            Path::new("/Users//a/cache/entry")
        ));
        assert!(!descendant(
            Path::new("/System/Volumes/Data/Users//a/cache/entry-other/aterm"),
            Path::new("/Users//a/cache/entry")
        ));
    }

    #[test]
    fn loaded_jobs_are_selected_without_reading_unrelated_environment() {
        assert_eq!(labels("services = {\n 7 0 systems.alab.aterm-node\n 8 0 another.job\n 9 0 systems.alab.aterm-secrets\n 0 0 systems.alab.astream-broker.n-1\n 0 0 com.aterm.aterm.acaffeine\n 0 0 systems.alab.aterm-auto-alab\n}").unwrap(), vec!["com.aterm.aterm.acaffeine", "systems.alab.astream-broker.n-1", "systems.alab.aterm-auto-alab", "systems.alab.aterm-node", "systems.alab.aterm-secrets"]);
        assert!(labels("unknown output").is_err());
        assert!(labels("services = {\n 7 0 systems.alab.aterm-node").is_err());
        assert!(labels("services = {\n unexpected\n}").is_err());
        assert_eq!(loaded_program("job = {\n\tprogram = /Applications/aterm.app/Contents/MacOS/aterm\n\tenvironment = {\n\t\tSAFE = value\n\t}\n}").unwrap(), Path::new("/Applications/aterm.app/Contents/MacOS/aterm"));
        assert!(loaded_program("state = waiting").is_err());
        assert!(loaded_program("\tprogram = /one\n\tprogram = /two").is_err());
        assert!(
            loaded_program("job = {\n\tenvironment = {\n\t\tprogram = /injected\n\t}\n}").is_err()
        );
    }

    #[test]
    #[cfg(target_os = "macos")]
    #[ignore = "read-only smoke against this Mac's configured and loaded managed services"]
    fn this_macs_managed_service_references() {
        let references =
            service_references(Instant::now() + aterm_spec::wait::MUST_HAPPEN).unwrap();
        assert!(
            !references.is_empty(),
            "this smoke requires existing managed services"
        );
        for reference in references {
            assert!(reference.program.is_absolute());
            println!("{}: {}", reference.source, reference.program.display());
        }
    }
}
