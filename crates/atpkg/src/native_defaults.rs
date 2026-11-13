// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Delegating an implicit Codex posture to that exact executable's compiled defaults.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

pub(crate) const VERB: &str = "__codex_default_launch";
const LAUNCHER_VAR: &str = "__atpkg_codex_launcher";
#[cfg(unix)]
const MAX_DEFAULTS_BYTES: usize = 64 * 1024;

/// A launcher admitted by this producer, not a claim about the selected Codex's defaults.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Launcher {
    pub path: PathBuf,
    pub pkg: bool,
}

impl Launcher {
    fn at(path: PathBuf) -> Option<Self> {
        if !path.is_absolute()
            || path.to_str()?.chars().any(char::is_control)
            || path
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            return None;
        }
        let pkg = match path.file_name()?.to_str()? {
            "aterm" => true,
            "atpkg" => false,
            _ => return None,
        };
        Some(Self { path, pkg })
    }

    /// A physical path to this image: a retained older helper cannot acquire a new verb.
    pub fn of_current_image(offered: Option<&Path>) -> Option<Self> {
        let offered = std::fs::canonicalize(offered?).ok()?;
        observe(&offered)?;
        if !crate::stub::is_running_image(&offered) {
            return None;
        }
        Self::at(offered)
    }

    pub fn shell_assignment(&self) -> String {
        format!(
            "{LAUNCHER_VAR}={}\n",
            crate::platform::sh_shim_quote(&self.path)
        )
    }

    /// Reconstruct an existing producer for whole-body comparison, never to admit a helper
    /// to a newly generated shim. The comparison rejects any additional shell instructions.
    #[cfg(any(unix, test))]
    pub fn recorded(body: &str) -> Option<Self> {
        Self::at(crate::stub::sh_named_atpkg(body, LAUNCHER_VAR)?)
    }
}

#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
struct Observation {
    path: PathBuf,
    dev: u64,
    ino: u64,
    len: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
}

#[cfg(unix)]
fn observe(candidate: &Path) -> Option<Observation> {
    use std::os::unix::fs::MetadataExt as _;
    let path = std::fs::canonicalize(candidate).ok()?;
    let metadata = std::fs::symlink_metadata(&path).ok()?;
    (metadata.is_file() && metadata.mode() & 0o111 != 0).then(|| Observation {
        path,
        dev: metadata.dev(),
        ino: metadata.ino(),
        len: metadata.len(),
        mtime: (metadata.mtime(), metadata.mtime_nsec()),
        ctime: (metadata.ctime(), metadata.ctime_nsec()),
    })
}

/// Off Unix no executable can be observed (no mode bits, no device and inode), so no
/// observation exists: two launchers never compare equal and none is admitted.
#[cfg(not(unix))]
#[derive(Debug, PartialEq, Eq)]
enum Observation {}

#[cfg(not(unix))]
fn observe(_: &Path) -> Option<Observation> {
    None
}

#[cfg(unix)]
fn native_posture(bytes: &[u8]) -> bool {
    if bytes.len() >= MAX_DEFAULTS_BYTES {
        return false;
    }
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let Ok(defaults) = aterm_toml::from_str::<aterm_toml::Value>(text) else {
        return false;
    };
    defaults
        .get("approval_policy")
        .and_then(aterm_toml::Value::as_str)
        == Some("never")
        && defaults
            .get("sandbox_mode")
            .and_then(aterm_toml::Value::as_str)
            == Some("danger-full-access")
}

/// Probe only the implicit default arm. A symlink retarget, package change, failed
/// probe or legacy defaults retains the existing wrapper policy. A successful decision
/// execs the physical path that was probed, with the caller's original argv0.
pub(crate) fn decide(
    program: &str,
    candidate: &Path,
    args: &[String],
) -> (crate::yolo::Launch, Option<PathBuf>) {
    decide_with(
        program,
        candidate,
        args,
        std::env::var_os("TERM_PROGRAM").as_deref(),
        |physical| {
            let mut command = aterm_uds::spawnfd::command(physical);
            command
                .args(["debug", "packaged-defaults"])
                .env_clear()
                .env("TERM_PROGRAM", "aterm");
            let output = crate::doctor::output_bounded_capturing(
                &mut command,
                crate::doctor::probe_timeout(),
            )?;
            output.status.success().then_some(output.stdout)
        },
    )
}

fn decide_with(
    program: &str,
    candidate: &Path,
    args: &[String],
    term_program: Option<&OsStr>,
    probe: impl FnOnce(&Path) -> Option<Vec<u8>>,
) -> (crate::yolo::Launch, Option<PathBuf>) {
    let decision = crate::yolo::decide(program, args);
    if matches!(decision, crate::yolo::Launch::Default(_))
        && let Some(physical) = selected_native_target_with(program, candidate, term_program, probe)
    {
        (crate::yolo::Launch::AsTyped, Some(physical))
    } else {
        (decision, None)
    }
}

#[cfg(unix)]
fn selected_native_target_with(
    program: &str,
    candidate: &Path,
    term_program: Option<&OsStr>,
    probe: impl FnOnce(&Path) -> Option<Vec<u8>>,
) -> Option<PathBuf> {
    if program != "codex"
        || !term_program
            .and_then(OsStr::to_str)
            .is_some_and(|term| term.trim().eq_ignore_ascii_case("aterm"))
    {
        return None;
    }
    let before = observe(candidate)?;
    if !crate::alab::exe_is_alab(&before.path) || !native_posture(&probe(&before.path)?) {
        return None;
    }
    (observe(candidate)? == before && crate::alab::exe_is_alab(&before.path)).then_some(before.path)
}

#[cfg(not(unix))]
fn selected_native_target_with(
    _: &str,
    _: &Path,
    _: Option<&OsStr>,
    _: impl FnOnce(&Path) -> Option<Vec<u8>>,
) -> Option<PathBuf> {
    None
}

/// The exact-candidate handoff. It never resolves a configured prefix or retries another
/// launch after exec: any error is final, so a user's program cannot run twice.
pub(crate) fn run_hidden(rest: &[OsString]) -> ExitCode {
    let [program, candidate, separator, args @ ..] = rest else {
        eprintln!("atpkg: {VERB}: expected codex <absolute executable> -- [args…]");
        return ExitCode::from(2);
    };
    let candidate = Path::new(candidate);
    if program != "codex" || separator != "--" || !candidate.is_absolute() {
        eprintln!("atpkg: {VERB}: expected codex <absolute executable> -- [args…]");
        return ExitCode::from(2);
    }
    let printable: Vec<String> = args
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let (decision, native) = decide("codex", candidate, &printable);
    let mut forwarded = Vec::new();
    match decision {
        crate::yolo::Launch::Flag(expanded) => {
            eprintln!("{}", crate::yolo::echo_line("codex", &expanded));
            forwarded.extend(expanded.into_iter().map(OsString::from));
        }
        crate::yolo::Launch::Default(expanded) => {
            eprintln!("{}", crate::yolo::default_echo_line("codex", &expanded));
            forwarded.extend(crate::yolo::flags_of("codex").iter().map(OsString::from));
            forwarded.extend_from_slice(args);
        }
        crate::yolo::Launch::AsTyped => forwarded.extend_from_slice(args),
    }
    let target = native.as_deref().unwrap_or(candidate);
    #[expect(
        clippy::disallowed_types,
        reason = "an exact shim handoff becomes the tool by exec, preserving caller descriptors"
    )]
    let mut command = std::process::Command::new(target);
    aterm_uds::spawn::set_flags(&mut command, 0);
    command.args(forwarded);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.arg0(candidate);
    }
    let error = crate::platform::exec_or_run(&mut command);
    eprintln!("atpkg: failed to exec {}: {error}", candidate.display());
    ExitCode::from(127)
}

#[cfg(all(test, unix))]
#[path = "native_defaults_tests.rs"]
mod tests;
