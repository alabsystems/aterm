// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Unprivileged `put` and `put-file` agent verbs for writing into user directories.

use crate::port::System;
use std::io::{Read, Write};
use std::path::{Component, Path};
use std::process::ExitCode;

/// Maximum payload size accepted by `put-file`.
const MAX_PUT_FILE: u64 = 16 << 20;

/// `agent put <sha256>`.
pub fn put(sys: &dyn System, sha: &str, stdin: &mut dyn Read, out: &mut dyn Write) -> ExitCode {
    if !super::is_sha256(sha) {
        return super::refuse(out, "put takes a lowercase SHA-256 hex digest");
    }
    if sys.euid() == 0 {
        return super::refuse(out, "put runs as the admin, never as root");
    }
    let me = match super::me(sys) {
        Ok(u) => u,
        Err(why) => return super::refuse(out, &format!("put: {why}")),
    };
    put_as(sys, &me, sha, stdin, out)
}

/// Ensure `home` is a non-root, dot-free absolute directory and neither `home` nor any existing
/// component along `target` inside `home` is a symbolic link or non-directory ancestor.
fn check_home_and_walk(
    sys: &dyn System,
    home: &Path,
    target: &Path,
    target_is_dir: bool,
) -> Result<(), String> {
    let valid_home = home.is_absolute()
        && home.parent().is_some()
        && !home
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir));
    if !valid_home {
        return Err(format!(
            "{} is not a non-root absolute home directory",
            home.display()
        ));
    }
    if !target.starts_with(home) || (!target_is_dir && target == home) {
        return Err(format!(
            "{} is outside {}",
            target.display(),
            home.display()
        ));
    }
    let rel = target
        .strip_prefix(home)
        .map_err(|_| format!("{} is outside {}", target.display(), home.display()))?;
    let check_component = |p: &Path, must_be_dir: bool| -> Result<(), String> {
        match sys.stat(p) {
            Ok(st) if st.symlink => Err(format!("{} is a symbolic link", p.display())),
            Ok(st) if must_be_dir && !st.dir => Err(format!("{} is not a directory", p.display())),
            Ok(_) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("{}: {e}", p.display())),
        }
    };
    let mut cur = home.to_path_buf();
    check_component(&cur, true)?;
    let parts: Vec<Component<'_>> = rel.components().collect();
    for (idx, comp) in parts.iter().enumerate() {
        cur.push(comp.as_os_str());
        let is_last = idx + 1 == parts.len();
        check_component(&cur, target_is_dir || !is_last)?;
    }
    Ok(())
}

/// [`put`] for the resolved caller `me`: at most
/// [`MAX_ARTIFACT_BYTES`](crate::controller::artifacts::MAX_ARTIFACT_BYTES), the most the
/// controller downloads of one artifact.
pub fn put_as(
    sys: &dyn System,
    me: &crate::model::User,
    sha: &str,
    stdin: &mut dyn Read,
    out: &mut dyn Write,
) -> ExitCode {
    put_at_most(
        sys,
        me,
        sha,
        crate::controller::artifacts::MAX_ARTIFACT_BYTES,
        stdin,
        out,
    )
}

/// [`put_as`], refusing standard input longer than `cap` bytes with nothing kept.
fn put_at_most(
    sys: &dyn System,
    me: &crate::model::User,
    sha: &str,
    cap: u64,
    stdin: &mut dyn Read,
    out: &mut dyn Write,
) -> ExitCode {
    if !super::is_sha256(sha) {
        return super::refuse(out, "put takes a lowercase SHA-256 hex digest");
    }
    let home = Path::new(&me.home);
    let dir = super::user_inbox(&me.home);
    if let Err(why) = check_home_and_walk(sys, home, &dir, true) {
        return super::refuse(out, &format!("put: {why}"));
    }
    if let Err(e) = sys.create_dir_all(&dir, 0o700) {
        return super::refuse(out, &format!("put: {}: {e}", dir.display()));
    }
    let partial = dir.join(format!(".{sha}.partial"));
    let target = dir.join(sha);
    let mut capped = stdin.take(cap.saturating_add(1));
    match sys.write_stream(&partial, &mut capped, 0o600) {
        Ok(written) if written > cap => {
            let _ = sys.remove(&partial);
            return super::refuse(
                out,
                &format!("put: more than {cap} bytes on standard input"),
            );
        }
        Ok(_) => {}
        Err(e) => {
            let _ = sys.remove(&partial);
            return super::refuse(out, &format!("put: {}: {e}", partial.display()));
        }
    }
    match sys.sha256(&partial) {
        Ok(got) if got == sha => {}
        Ok(got) => {
            let _ = sys.remove(&partial);
            return super::refuse(out, &format!("put: the bytes hash to {got}, not {sha}"));
        }
        Err(e) => {
            let _ = sys.remove(&partial);
            return super::refuse(out, &format!("put: {e}"));
        }
    }
    if let Err(e) = sys.rename(&partial, &target) {
        let _ = sys.remove(&partial);
        return super::refuse(out, &format!("put: {}: {e}", target.display()));
    }
    let _ = writeln!(out, "ok {sha}");
    ExitCode::SUCCESS
}

/// `agent put-file <path> <mode-octal>`: standard input, atomically, into `path` inside
/// the caller's own home. Missing parent directories are made 0700.
pub fn put_file(
    sys: &dyn System,
    path: &Path,
    mode: &str,
    stdin: &mut dyn Read,
    out: &mut dyn Write,
) -> ExitCode {
    if sys.euid() == 0 {
        return super::refuse(
            out,
            "put-file writes as the user whose home it is, never as root",
        );
    }
    let Some(mode) = (!mode.is_empty() && mode.bytes().all(|b| matches!(b, b'0'..=b'7')))
        .then(|| u32::from_str_radix(mode, 8).ok())
        .flatten()
    else {
        return super::refuse(out, "put-file: the mode is not octal");
    };
    if mode & !0o777 != 0 {
        return super::refuse(out, "put-file: only permission bits (0777) may be set");
    }
    let plain = path.is_absolute()
        && !path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir));
    if !plain {
        return super::refuse(out, "put-file: the path must be absolute, with no . or ..");
    }
    let me = match super::me(sys) {
        Ok(u) => u,
        Err(why) => return super::refuse(out, &format!("put-file: {why}")),
    };
    put_file_as(sys, &me, path, mode, stdin, out)
}

/// [`put_file`] for the resolved caller `me`, after the root and argument checks.
fn put_file_as(
    sys: &dyn System,
    me: &crate::model::User,
    path: &Path,
    mode: u32,
    stdin: &mut dyn Read,
    out: &mut dyn Write,
) -> ExitCode {
    let home = Path::new(&me.home);
    if let Err(why) = check_home_and_walk(sys, home, path, false) {
        return super::refuse(out, &format!("put-file: {why}"));
    }
    let mut bytes = Vec::new();
    if let Err(e) = stdin.take(MAX_PUT_FILE + 1).read_to_end(&mut bytes) {
        return super::refuse(out, &format!("put-file: standard input: {e}"));
    }
    if bytes.len() as u64 > MAX_PUT_FILE {
        return super::refuse(out, "put-file: more than 16 MiB on standard input");
    }
    if let Some(parent) = path.parent()
        && sys.stat(parent).is_err()
        && let Err(e) = sys.create_dir_all(parent, 0o700)
    {
        return super::refuse(out, &format!("put-file: {}: {e}", parent.display()));
    }
    match sys.write_file(path, &bytes, mode) {
        Ok(()) => {
            let _ = writeln!(out, "ok {}", path.display());
            ExitCode::SUCCESS
        }
        Err(e) => super::refuse(out, &format!("put-file: {}: {e}", path.display())),
    }
}

#[cfg(test)]
#[path = "inbox_tests.rs"]
mod tests;
