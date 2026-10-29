// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! OS-primitive abstraction.

use std::io;
use std::path::{Path, PathBuf};

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

/// What the kernel says of when a pid's process started ([`process_birth`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Birth {
    /// Kernel start-time stamp used alongside the pid to detect reuse.
    At(u64),
    /// Nothing lives under the pid: no such process, or a zombie.
    Gone,
    /// The kernel would not say.
    Unreadable,
}

impl Birth {
    /// The start time, when the kernel gave one.
    #[must_use]
    pub const fn at(self) -> Option<u64> {
        match self {
            Self::At(born) => Some(born),
            Self::Gone | Self::Unreadable => None,
        }
    }
}

/// The private-directory creator.
pub use aterm_update_core::ensure_private_dir;

/// The advisory exclusive file lock.
pub use aterm_update_core::FileLock;

/// Create `path` exclusively (`create_new`: an existing file is never opened and truncated).
pub(crate) fn create_private_new(path: &Path) -> io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}

/// Create `dir` exclusively ([`io::ErrorKind::AlreadyExists`] if present).
pub(crate) fn make_dir_born(dir: &Path, mode: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::DirBuilderExt::mode(&mut std::fs::DirBuilder::new(), mode)
            .create(dir)?;
        set_mode(dir, mode)
    }
    #[cfg(not(unix))]
    {
        let _ = mode;
        std::fs::create_dir(dir)
    }
}

/// [`make_dir_born`] for `dir` and every missing ancestor, top down.
pub(crate) fn make_dirs_born(dir: &Path, mode: u32) -> io::Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    if let Some(parent) = dir.parent()
        && !parent.as_os_str().is_empty()
    {
        make_dirs_born(parent, mode)?;
    }
    match make_dir_born(dir, mode) {
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists && dir.is_dir() => Ok(()),
        made => made,
    }
}

/// Make one open file or directory durable.
pub(crate) fn sync_durable(f: &std::fs::File) -> io::Result<()> {
    aterm_durable::durable(f)
}

/// macOS's QoS launcher.
#[cfg(unix)]
const TASKPOLICY: &str = aterm_uds::os_path!("taskpolicy");

/// The argv head that starts `program` at UTILITY QoS for a detached background pass.
#[cfg(unix)]
pub(crate) fn utility_clamped(program: &std::ffi::OsStr) -> Vec<std::ffi::OsString> {
    utility_clamped_with(program, Path::new(TASKPOLICY))
}

/// [`utility_clamped`] over an explicit launcher.
#[cfg(unix)]
fn utility_clamped_with(program: &std::ffi::OsStr, taskpolicy: &Path) -> Vec<std::ffi::OsString> {
    let path = Path::new(program);
    if path.is_absolute()
        && crate::toolchain::is_executable_file(path)
        && crate::toolchain::is_executable_file(taskpolicy)
    {
        vec![
            taskpolicy.into(),
            "-c".into(),
            "utility".into(),
            program.into(),
        ]
    } else {
        vec![program.into()]
    }
}

/// Start `program args` DETACHED.
/// # Errors
/// The middle shell could not be started, or did not exit 0.
pub fn spawn_detached(
    program: &std::ffi::OsStr,
    args: &[std::ffi::OsString],
    env: Option<(&str, &str)>,
) -> io::Result<()> {
    spawn_detached_at(program, args, env, true)
}

/// [`spawn_detached`] for the one pass a person waits on.
/// # Errors
/// As [`spawn_detached`].
pub fn spawn_detached_waited(
    program: &std::ffi::OsStr,
    args: &[std::ffi::OsString],
    env: Option<(&str, &str)>,
) -> io::Result<()> {
    spawn_detached_at(program, args, env, false)
}

fn spawn_detached_at(
    program: &std::ffi::OsStr,
    args: &[std::ffi::OsString],
    env: Option<(&str, &str)>,
    clamped: bool,
) -> io::Result<()> {
    #[cfg(unix)]
    {
        let mut command = aterm_uds::spawnfd::command("/bin/sh");
        if let Some((name, value)) = env {
            command.env(name, value);
        }
        let head = if clamped {
            utility_clamped(program)
        } else {
            vec![program.into()]
        };
        let status = command
            .args(["-c", "\"$@\" &", "sh"])
            .args(head)
            .args(args)
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .stderr(aterm_uds::spawnfd::Stdio::null())
            .process_group(0)
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!("the detaching shell {status}")))
        }
    }
    #[cfg(not(unix))]
    {
        let _ = clamped;
        let mut command = aterm_uds::spawnfd::command(program);
        if let Some((name, value)) = env {
            command.env(name, value);
        }
        command
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map(drop)
    }
}

/// Install a `bin/` shim at `shim` (from [`crate::store::Layout::shim`], never a hand join)
/// pointing at `tool`'s executable in a build's `bin/` directory.
#[cfg(test)]
pub(crate) fn install_shim(
    build_bin_dir: &Path,
    tool: &crate::store::ToolName,
    shim: &Path,
) -> io::Result<()> {
    install_shim_to(shim, &build_bin_dir.join(tool.exe_file()))
}

/// `install_shim` whose wrapper also exports `env` ([`crate::shim_env`]) before it execs the
/// target.
pub fn install_shim_env(
    build_bin_dir: &Path,
    tool: &crate::store::ToolName,
    shim: &Path,
    env: &crate::shim_env::ShimEnv,
) -> io::Result<()> {
    install_shim_to_env(shim, &build_bin_dir.join(tool.exe_file()), env)
}

/// The `(shim path, target)` form of `install_shim`, for callers that already hold the target path.
pub fn install_shim_to(shim: &Path, target: &Path) -> io::Result<()> {
    install_shim_to_env(shim, target, &crate::shim_env::ShimEnv::NONE)
}

/// The shim [`install_shim_env`] would lay.
pub fn shim_executable_env(
    build_bin_dir: &Path,
    tool: &crate::store::ToolName,
    shim: &Path,
    env: &crate::shim_env::ShimEnv,
) -> io::Result<crate::lay::Executable> {
    shim_executable_to_env(shim, &build_bin_dir.join(tool.exe_file()), env)
}

/// The tail of every `.cmd` forward line after the target's closing quote.
#[cfg(any(windows, test))]
pub(crate) const CMD_FORWARD_TAIL: &str = " %* & @exit /b";

/// The tail of a legacy `.cmd` shim's last line: `@"<target>" %*` alone.
pub(crate) const CMD_LEGACY_FORWARD_TAIL: &str = " %*";

/// The first line of every `.cmd` this crate writes.
pub(crate) const CMD_FRAME_HEAD: &str = "@goto :main\r\n";

const CMD_MAIN_LABEL: &str = "main";

const CMD_PADDING_LINE: &str =
    "::::::::::::::::::::::::::::::::::::::::::::::::::::::::::::::::::::::::::::::\r\n";

const CMD_PADDING_LINES: usize = 51;

const CMD_PADDING_EXIT: &str = "@exit /b\r\n";

/// The byte offset at which [`CMD_PADDING_EXIT`] starts: head + padding = 13 + 4080 = 4093.
pub(crate) const CMD_PADDING_END_BYTES: usize =
    CMD_FRAME_HEAD.len() + CMD_PADDING_LINES * CMD_PADDING_LINE.len();

/// The frame's whole cost per file: head, padding, exit, `:main` line — 4110 bytes.
pub(crate) const CMD_FRAME_BYTES: usize =
    CMD_PADDING_END_BYTES + CMD_PADDING_EXIT.len() + 1 + CMD_MAIN_LABEL.len() + 2;

/// The longest `.cmd` the legacy writer could have laid, from its own caps.
pub(crate) const CMD_LEGACY_FILE_BOUND_BYTES: usize = crate::shim_env::MAX_SHIM_ENV
    * ("@set \"".len() + crate::shim_env::MAX_ENTRY_BYTES + "\"\r\n".len())
    + "@\"".len()
    + CMD_LEGACY_TARGET_BOUND_BYTES
    + "\"".len()
    + CMD_LEGACY_FORWARD_TAIL.len()
    + "\r\n".len();

/// The longest target a legacy shim's forward could carry.
pub(crate) const CMD_LEGACY_TARGET_BOUND_BYTES: usize = 3 * 260;

/// The shortest line the legacy writer could lay.
pub(crate) const CMD_LEGACY_MIN_LINE_BYTES: usize = "@set \"A=b\"\r\n".len();

const _: () = assert!(CMD_PADDING_END_BYTES >= CMD_LEGACY_FILE_BOUND_BYTES);
const _: () = assert!(CMD_FRAME_HEAD.len() - 2 <= CMD_LEGACY_MIN_LINE_BYTES);

/// `body` behind the resume-proof frame ([`CMD_FRAME_HEAD`]).
pub(crate) fn cmd_framed(body: &str) -> String {
    let mut s = String::with_capacity(CMD_FRAME_BYTES + body.len());
    s.push_str(CMD_FRAME_HEAD);
    for _ in 0..CMD_PADDING_LINES {
        s.push_str(CMD_PADDING_LINE);
    }
    s.push_str(CMD_PADDING_EXIT);
    s.push(':');
    s.push_str(CMD_MAIN_LABEL);
    s.push_str("\r\n");
    s.push_str(body);
    s
}

/// What `cmd.exe` reads when it resumes a batch file at byte `offset`.
#[cfg(test)]
pub(crate) fn cmd_resumed_line(file: &[u8], offset: usize) -> Option<String> {
    if offset >= file.len() {
        return None;
    }
    let rest = &file[offset..];
    let end = rest.iter().position(|&b| b == b'\n').unwrap_or(rest.len());
    let line = rest[..end].strip_suffix(b"\r").unwrap_or(&rest[..end]);
    Some(String::from_utf8_lossy(line).into_owned())
}

/// Whether a resumed line does nothing and leaves the program's ERRORLEVEL alone.
#[cfg(test)]
pub(crate) fn cmd_resumed_line_is_inert(line: Option<&str>) -> bool {
    match line {
        None => true,
        Some(l) => l.is_empty() || l.bytes().all(|b| b == b':') || l == "@exit /b",
    }
}

/// The frame alone ([`cmd_framed`] of nothing).
#[cfg(test)]
pub(crate) fn cmd_frame() -> String {
    cmd_framed("")
}

/// The body of a Windows bin shim.
#[cfg(test)]
pub(crate) fn cmd_shim_content(target: &Path) -> String {
    cmd_shim_content_env(target, &crate::shim_env::ShimEnv::NONE)
}

/// [`cmd_shim_content`] with the shim's exported environment ahead of the forward line.
#[cfg(any(windows, test))]
pub(crate) fn cmd_shim_content_env(target: &Path, env: &crate::shim_env::ShimEnv) -> String {
    cmd_framed(&cmd_shim_body_env(target, env))
}

/// The unframed shim body (the `@set` lines and the forward line) that [`cmd_shim_content_env`]
/// frames and [`cmd_shim_content_twin`] lays behind a prelude.
#[cfg(any(windows, test))]
fn cmd_shim_body_env(target: &Path, env: &crate::shim_env::ShimEnv) -> String {
    let mut s = String::new();
    for (name, value) in env.entries() {
        s.push_str("@set \"");
        s.push_str(name);
        s.push('=');
        s.push_str(value);
        s.push_str("\"\r\n");
    }
    s.push_str("@\"");
    s.push_str(&target.to_string_lossy());
    s.push('"');
    s.push_str(CMD_FORWARD_TAIL);
    s.push_str("\r\n");
    s
}

/// Whether every entry of `env` can sit inside `@set "NAME=VALUE"` without breaking out.
#[cfg(any(windows, test))]
pub(crate) fn cmd_env_is_injection_safe(env: &crate::shim_env::ShimEnv) -> bool {
    env.entries().iter().all(|(n, v)| {
        !n.chars()
            .chain(v.chars())
            .any(|c| matches!(c, '"' | '%' | '\r' | '\n' | '\0'))
    })
}

/// The environment a Windows shim written by [`cmd_shim_content_env`] exports.
#[cfg(any(windows, test))]
pub(crate) fn parse_cmd_shim_env(content: &str) -> crate::shim_env::ShimEnv {
    let mut raw: Vec<String> = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("@set \"")
            && let Some(entry) = rest.strip_suffix('"')
        {
            raw.push(entry.to_string());
        }
    }
    crate::shim_env::ShimEnv::admit(&raw).unwrap_or_default()
}

/// Whether `target` can sit inside `@"<target>" %* & @exit /b` without breaking out.
#[cfg(any(windows, test))]
pub(crate) fn cmd_target_is_injection_safe(target: &Path) -> bool {
    !target
        .to_string_lossy()
        .chars()
        .any(|c| matches!(c, '"' | '%' | '\r' | '\n' | '\0'))
}

/// The body of a Windows **tombstone** shim.
#[cfg(any(windows, test))]
pub(crate) fn cmd_tombstone_content(message: &str) -> String {
    let mut s = String::from("@echo ");
    s.push_str(&cmd_echo_escape(message));
    s.push_str(" 1>&2\r\n@exit /b 70\r\n");
    cmd_framed(&s)
}

/// Escape a string for safe embedding in a `cmd.exe` `echo` argument.
#[cfg(any(windows, test))]
fn cmd_echo_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '%' => out.push_str("%%"),
            '\u{2014}' => out.push('-'),
            '^' | '&' | '<' | '>' | '|' | '(' | ')' | '"' => {
                out.push('^');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

/// The forward target of a Windows bin shim (`@"<target>" %* & @exit /b`).
#[cfg(any(windows, test))]
pub(crate) fn parse_cmd_shim_target(content: &str) -> Option<PathBuf> {
    for line in content.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("@\"")
            && let Some(end) = rest.find('"')
            && matches!(&rest[end + 1..], CMD_FORWARD_TAIL | CMD_LEGACY_FORWARD_TAIL)
        {
            return Some(PathBuf::from(&rest[..end]));
        }
    }
    None
}

/// `path` without a Windows verbatim prefix (`\\?\C:\…` → `C:\…`, `\\?\UNC\srv\sh` → `\\srv\sh`).
#[must_use]
pub(crate) fn strip_verbatim_prefix(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix("\\\\?\\UNC\\") {
        let mut out = String::from("\\\\");
        out.push_str(rest);
        return PathBuf::from(out);
    }
    if let Some(rest) = s.strip_prefix("\\\\?\\") {
        return PathBuf::from(rest);
    }
    path.to_path_buf()
}

/// [`cmd_shim_content_env`] with `prelude` (an `agents/` twin's [`twin_prelude`], empty on Windows;
/// empty for every `bin/` shim) ahead of the `@set` and forward lines.
#[cfg(any(windows, test))]
pub(crate) fn cmd_shim_content_twin(
    target: &Path,
    env: &crate::shim_env::ShimEnv,
    prelude: &str,
) -> String {
    let mut s = String::from(prelude);
    s.push_str(&cmd_shim_body_env(target, env));
    cmd_framed(&s)
}

/// The note line of the legacy `.cmd` landing prelude ([`cmd_landing_prelude`]).
#[cfg(test)]
const CMD_LANDING_NOTE: &str = "@rem atpkg agents twin: while a newer build of this program is landing, atpkg __landing \
     waits for it and then runs the new one (aterm help pkg).\r\n";

/// The label the legacy `.cmd` landing prelude jumps to past its hand-over.
#[cfg(test)]
const CMD_LANDING_LABEL: &str = "store";

/// The legacy `.cmd` landing prelude an old Windows `agents\` twin carries ahead of its exports.
#[cfg(test)]
#[must_use]
pub(crate) fn cmd_landing_prelude(
    program: &str,
    prefix: &Path,
    marker: &Path,
    atpkg: &Path,
) -> String {
    let (Some(marker), Some(atpkg), Some(prefix)) = (
        cmd_embedded_path(marker),
        cmd_embedded_path(atpkg),
        cmd_embedded_path(prefix),
    ) else {
        return String::new();
    };
    if !cmd_target_is_injection_safe(Path::new(program)) {
        return String::new();
    }
    let mut s = String::from(CMD_LANDING_NOTE);
    s.push_str("@if not exist \"");
    s.push_str(&marker);
    s.push_str("\" goto ");
    s.push_str(CMD_LANDING_LABEL);
    s.push_str("\r\n@if not exist \"");
    s.push_str(&atpkg);
    s.push_str("\" goto ");
    s.push_str(CMD_LANDING_LABEL);
    s.push_str("\r\n@\"");
    s.push_str(&atpkg);
    s.push_str("\" ");
    s.push_str(crate::landing::HIDDEN_VERB);
    s.push_str(" \"");
    s.push_str(program);
    s.push_str("\" \"");
    s.push_str(&prefix);
    s.push_str("\" --");
    s.push_str(CMD_FORWARD_TAIL);
    s.push_str("\r\n:");
    s.push_str(CMD_LANDING_LABEL);
    s.push_str("\r\n");
    s
}

/// A path as the legacy `.cmd` landing prelude embedded it.
#[cfg(test)]
fn cmd_embedded_path(path: &Path) -> Option<String> {
    let stripped = strip_verbatim_prefix(path);
    let trimmed = stripped
        .to_string_lossy()
        .trim_end_matches(['\\', '/'])
        .to_string();
    if trimmed.is_empty()
        || trimmed.ends_with(':')
        || !cmd_target_is_injection_safe(Path::new(&trimmed))
    {
        return None;
    }
    Some(trimmed)
}

/// THE PRELUDE of an agent program's `agents/` twin and its `bin/` shim.
#[must_use]
pub(crate) fn twin_prelude(
    program: &str,
    prefix: &Path,
    atpkg: Option<&Path>,
    verbs: &[&str],
    yolo: &crate::yolo::Policy,
) -> String {
    if cfg!(windows) {
        return String::new();
    }
    let mut s = sh_selfupdate_prelude(program, prefix, crate::stub::named(atpkg), verbs);
    // The sign-in block reads the launch as typed (it skips `--no-harness` itself), ahead of
    // the yolo block, so a launch it stops never prints the yolo block's `running` line.
    if let Some(row) = crate::vendor_direct::spec(program)
        && let Some(gate) = &row.signin_gate
    {
        let skip: Vec<&str> = gate
            .local
            .iter()
            .chain(row.self_update_verbs.iter())
            .copied()
            .collect();
        s.push_str(&sh_signin_prelude(program, row.product, gate, &skip));
    }
    s.push_str(&sh_yolo_prelude(program, yolo, verbs));
    #[cfg(any(unix, test))]
    s.push_str(&sh_harness_prelude(program, prefix));
    s
}

/// THE HARNESS HAND-OFF of an agent program's twin ([`crate::harness::twin_harness`]): once a
/// LIVE build of the harness is installed, the twin execs `<prefix>/bin/<tool> run --` with the
/// argv the yolo block left, so the session runs under the harness
/// (`docs/DESIGN-harness-repos-2026-10-24.md` §9 step 6, §11 D32); `--no-alab-harness`,
/// `--no-harness` (each sets `ATERM_NO_HARNESS=1` above) or the variable itself runs the vendor
/// alone, and so does a harness that is not installed.
///
/// Live means what [`resolve_shim`] and the claim's image rule read (`crates/aterm-gui/src/
/// harness_claim.rs`): `bin/<tool>` is an atpkg shim whose `exec '<target>' "$@"` line names an
/// executable file inside a ready store build of the harness (`store/<harness>/<digits>/` with
/// `<digits>.ready`), or anywhere while the harness is dev-linked (its link marker exists), or it
/// is a link to an executable file. A tombstone (a yanked or revoked harness: no `exec` line,
/// [`crate::activate::install_tombstone_shim`]), a shim whose build was removed or never finished,
/// and a target whose path holds a quote all fall through to the vendor, so the harness never
/// stands between the person and a working vendor build. Empty for an agent with no such
/// harness.
#[cfg(any(unix, test))]
pub(crate) fn sh_harness_prelude(program: &str, prefix: &Path) -> String {
    let Some((harness, tool)) = crate::harness::twin_harness(program) else {
        return String::new();
    };
    let layout = crate::store::Layout {
        prefix: prefix.to_path_buf(),
    };
    let exe = sh_shim_quote(&prefix.join("bin").join(tool));
    let store = sh_shim_quote(layout.program_current(harness).parent().unwrap_or(prefix));
    let marker = sh_shim_quote(&layout.link_marker(harness));
    format!(
        "# atpkg: {harness}, the harness, runs this session once a live build of it is installed \
         (`--no-alab-harness` runs {program} alone); a yanked, removed or unfinished one never stands in the way.\n\
         if [ \"${{ATERM_NO_HARNESS-}}\" != 1 ] && [ -f {exe} ] && [ -x {exe} ]; then\n\
         \x20 __alab=\n\
         \x20 if [ -h {exe} ]; then __alab={exe}\n\
         \x20 else\n\
         \x20   while IFS= read -r __alab_l || [ -n \"$__alab_l\" ]; do\n\
         \x20     case $__alab_l in \"exec '\"*\"' \\\"\\$@\\\"\") __alab=${{__alab_l#\"exec '\"}}; __alab=${{__alab%\"' \\\"\\$@\\\"\"}} ;; esac\n\
         \x20   done < {exe}\n\
         \x20   case $__alab in\n\
         \x20     *\"'\"*) __alab= ;;\n\
         \x20     {store}/*) __alab_b=${{__alab#{store}/}}; __alab_b=${{__alab_b%%/*}}\n\
         \x20       case $__alab_b in ''|*[!0-9]*) __alab= ;; *) [ -f {store}/\"$__alab_b\".ready ] || __alab= ;; esac ;;\n\
         \x20     *) [ -f {marker} ] || __alab= ;;\n\
         \x20   esac\n\
         \x20 fi\n\
         \x20 if [ -n \"$__alab\" ] && [ -f \"$__alab\" ] && [ -x \"$__alab\" ]; then exec {exe} run -- \"$@\"; fi\n\
         fi\n"
    )
}

/// Whether `name` may be embedded in the sign-in block as a variable name: `[A-Z_][A-Z0-9_]*`.
fn is_shell_safe_env_name(name: &str) -> bool {
    name.bytes()
        .next()
        .is_some_and(|b| b.is_ascii_uppercase() || b == b'_')
        && name
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

/// Whether `part` may be embedded in the sign-in block as one path component.
fn is_shell_safe_component(part: &str) -> bool {
    !part.is_empty()
        && part != "."
        && part != ".."
        && part
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// THE SIGN-IN BLOCK of a program's twin and `bin/` shim ([`crate::vendor_direct::SigninGate`]):
/// under aterm (`ATERM_CHILD=1`, no `ATERM_NO_HARNESS` and no `--no-harness` or
/// `--no-alab-harness` ahead of a literal `--`), an interactive launch (no one-shot option
/// and no subcommand of `skip` before a literal `--`) that its environment and files say is
/// signed out runs `aterm harness signin --cli <cli> --wait` first, and runs the program only
/// once that exits 0; otherwise it exits 69 with one line. Outside aterm it does nothing.
/// Renders empty where any word it would embed is not shell-safe (fail closed: no block,
/// never a half-quoted one).
#[must_use]
pub(crate) fn sh_signin_prelude(
    program: &str,
    product: &str,
    gate: &crate::vendor_direct::SigninGate,
    skip: &[&str],
) -> String {
    let crate::vendor_direct::SigninGate {
        cli,
        credential_env,
        login_file_env,
        home_env,
        home_default,
        login_file,
        one_shot,
        local: _,
    } = *gate;
    if crate::store::ToolName::new(program).is_none()
        || !crate::selfupdate::is_shell_safe_verb(program)
        || !crate::selfupdate::is_shell_safe_verb(cli)
        || !credential_env
            .iter()
            .chain([login_file_env, home_env].iter())
            .all(|n| is_shell_safe_env_name(n))
        || !is_shell_safe_component(home_default)
        || !is_shell_safe_component(login_file)
        || one_shot.is_empty()
        || !one_shot
            .iter()
            .all(|f| crate::yolo::is_shell_safe_marker(f))
        || !skip
            .iter()
            .all(|w| crate::selfupdate::is_shell_safe_verb(w))
        || product.is_empty()
        || !product
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b' ' | b'.' | b'-'))
    {
        return String::new();
    }
    let mut s = String::from("# atpkg: sign-in gate \u{2014} under aterm a signed-out ");
    s.push_str(program);
    s.push_str(" is signed in once for this machine by `aterm harness signin --cli ");
    s.push_str(cli);
    s.push_str(
        "` (sandboxed: one sign-in and one need, no page opened) before an interactive launch, \
         and runs only once that signs it in (aterm help pkg).\n",
    );
    let words = crate::yolo::word_options(program);
    let alts = |list: &[&str]| -> Option<String> {
        let mut seen: Vec<String> = Vec::new();
        for f in list {
            if !crate::yolo::is_shell_safe_marker(f) {
                return None;
            }
            for spelled in std::iter::once(sh_quote_str(f)).chain(
                f.starts_with("--")
                    .then(|| format!("{}=*", sh_quote_str(f))),
            ) {
                if !seen.contains(&spelled) {
                    seen.push(spelled);
                }
            }
        }
        Some(seen.join("|"))
    };
    let Some(once) = alts(one_shot) else {
        return String::new();
    };
    let value_options: Vec<&str> = words
        .values
        .iter()
        .copied()
        .filter(|v| !one_shot.contains(v))
        .collect();
    let (Some(takes), Some(maybe)) = (alts(&value_options), alts(words.optional)) else {
        return String::new();
    };
    s.push_str(
        "if [ \"${ATERM_CHILD-}\" = 1 ] && [ -z \"${ATERM_NO_HARNESS-}\" ]; then\n  \
         __atpkg_gate=1\n  \
         __atpkg_v=0\n  \
         __atpkg_first=1\n  \
         for __atpkg_a in \"$@\"; do\n    \
         if [ \"$__atpkg_v\" = 1 ]; then __atpkg_v=0; continue; fi\n    \
         if [ \"$__atpkg_v\" = 2 ]; then\n      \
         __atpkg_v=0\n      \
         case \"$__atpkg_a\" in -*) ;; *) continue ;; esac\n    \
         fi\n    \
         case \"$__atpkg_a\" in\n      \
         --) break ;;\n      \
         '--no-harness'|'--no-alab-harness') __atpkg_gate=0; break ;;\n      ",
    );
    s.push_str(&once);
    s.push_str(") __atpkg_gate=0; break ;;\n");
    // A `--opt=value` spelling carries its own value, so only the bare one takes the next word.
    let bare = |alt: &str| {
        alt.split('|')
            .filter(|a| !a.ends_with("=*"))
            .collect::<Vec<_>>()
            .join("|")
    };
    if !value_options.is_empty() {
        s.push_str("      ");
        s.push_str(&bare(&takes));
        s.push_str(") __atpkg_v=1 ;;\n");
    }
    if !words.optional.is_empty() {
        s.push_str("      ");
        s.push_str(&bare(&maybe));
        s.push_str(") __atpkg_v=2 ;;\n");
    }
    s.push_str("      -*) ;;\n      *)\n        if [ \"$__atpkg_first\" = 1 ]; then\n          __atpkg_first=0\n");
    if !skip.is_empty() {
        let mut uniq: Vec<&str> = Vec::new();
        for w in skip {
            if !uniq.contains(w) {
                uniq.push(w);
            }
        }
        s.push_str("          case \"$__atpkg_a\" in ");
        s.push_str(
            &uniq
                .iter()
                .map(|w| sh_quote_str(w))
                .collect::<Vec<_>>()
                .join("|"),
        );
        s.push_str(") __atpkg_gate=0; break ;; esac\n");
    }
    s.push_str("        fi\n        ;;\n");
    s.push_str("    esac\n  done\n  if [ \"$__atpkg_gate\" = 1 ]; then\n    if ");
    s.push_str(
        &credential_env
            .iter()
            .map(|n| format!("[ -n \"${{{n}-}}\" ]"))
            .collect::<Vec<_>>()
            .join(" || "),
    );
    s.push_str("; then\n      __atpkg_gate=0\n");
    // A login grok refused carries `<file>.refused` at least as new as itself (aterm's
    // `harness::grok_signin::mark_refused`), and is signed out until a newer login replaces it.
    let file_test = |path: &str| {
        format!(
            "[ -f \"{path}\" ] && [ -s \"{path}\" ] && {{ [ ! -e \"{path}.refused\" ] || [ \"{path}\" -nt \"{path}.refused\" ]; }}"
        )
    };
    s.push_str(&format!(
        "    elif [ -n \"${{{login_file_env}-}}\" ]; then\n      \
         if {}; then __atpkg_gate=0; fi\n",
        file_test(&format!("${login_file_env}"))
    ));
    s.push_str(&format!(
        "    elif [ -n \"${{{home_env}-}}\" ]; then\n      \
         if {}; then __atpkg_gate=0; fi\n",
        file_test(&format!("${home_env}/{login_file}"))
    ));
    s.push_str(&format!(
        "    elif [ -n \"${{HOME-}}\" ] && {}; then\n      __atpkg_gate=0\n    fi\n  fi\n",
        file_test(&format!("$HOME/{home_default}/{login_file}"))
    ));
    let missing = format!(
        "{program}: signed out of {product}, and aterm (which signs this machine in once) is \
         not on PATH; nothing was started"
    );
    let refused = format!(
        "{program}: not signed in to {product} (aterm harness signin --cli {cli} says why \
         above); nothing was started"
    );
    s.push_str(
        "  if [ \"$__atpkg_gate\" = 1 ]; then\n    \
         __atpkg_aterm=$(command -v aterm 2>/dev/null) || __atpkg_aterm=\n    \
         if [ -z \"$__atpkg_aterm\" ]; then\n      printf '%s\\n' ",
    );
    s.push_str(&sh_quote_str(&missing));
    s.push_str(" 1>&2\n      exit 69\n    fi\n    if ! \"$__atpkg_aterm\" harness signin --cli ");
    s.push_str(&sh_quote_str(cli));
    s.push_str(" --wait; then\n      printf '%s\\n' ");
    s.push_str(&sh_quote_str(&refused));
    s.push_str(" 1>&2\n      exit 69\n    fi\n  fi\nfi\n");
    s
}

/// The Unix `bin/` shim body: a `/bin/sh` stub that `exec`s the store binary.
#[cfg(test)]
pub(crate) fn sh_shim_content(target: &Path) -> String {
    sh_shim_content_env(target, &crate::shim_env::ShimEnv::NONE)
}

/// The note ahead of an env-carrying shim's `export` lines.
#[cfg(any(unix, test))]
const SH_SHIM_ENV_NOTE: &str =
    "# shim_env is aterm's policy for this program: only this managed copy runs with it.\n";

/// [`sh_shim_content`] with the shim's exported environment ([`crate::shim_env`]) ahead of the
/// `exec`.
#[cfg(test)]
pub(crate) fn sh_shim_content_env(target: &Path, env: &crate::shim_env::ShimEnv) -> String {
    sh_shim_content_routed(target, env, None)
}

/// The note ahead of the guard line of a shim routed through a [`crate::compat`] exec root.
#[cfg(any(unix, test))]
const SH_SHIM_ROUTE_NOTE: &str = "# atpkg exec root: this build ships bin/rustc as a separate copy of trustc, which its \
     tippy refuses; the same tools run from a clone of the build where rustc holds trustc's bytes (aterm pkg doctor).\n";

/// `sh_shim_content_env` with an optional route: `None` is that shim byte for byte.
#[cfg(any(unix, test))]
pub(crate) fn sh_shim_content_routed(
    target: &Path,
    env: &crate::shim_env::ShimEnv,
    route: Option<&Path>,
) -> String {
    sh_shim_body(target, env, route, None, "")
}

/// The note line of the legacy `sh` landing prelude ([`sh_landing_prelude`]).
#[cfg(all(test, unix))]
const SH_LANDING_NOTE: &str = "# atpkg agents twin: while a newer build of this program is landing, `atpkg __landing` \
     waits for it and then runs the new one (aterm help pkg).\n";

/// The legacy `sh` landing prelude an old Unix `agents/` twin carries after its self-update block.
#[cfg(all(test, unix))]
#[must_use]
pub(crate) fn sh_landing_prelude(
    program: &str,
    prefix: &Path,
    marker: &Path,
    atpkg: &Path,
) -> String {
    let mut operands = sh_quote_str(program);
    operands.push(' ');
    operands.push_str(&sh_quote_str(&prefix.to_string_lossy()));
    let mut s = String::from(SH_LANDING_NOTE);
    s.push_str("if [ -f ");
    s.push_str(&sh_quote_str(&marker.to_string_lossy()));
    s.push_str(" ]; then\n  __atpkg=");
    s.push_str(&sh_quote_str(&atpkg.to_string_lossy()));
    s.push_str("\n  if [ -x \"$__atpkg\" ]; then exec \"$__atpkg\" ");
    s.push_str(crate::landing::HIDDEN_VERB);
    s.push(' ');
    s.push_str(&operands);
    s.push_str(" -- \"$@\"; fi\nfi\n");
    s
}

const SH_SELFUPDATE_NOTE: &str =
    "` — aterm updates this copy from its vendor and verifies every build here (aterm help pkg).\n";

/// The self-update block of an agent program's `agents/` twin and its `bin/` shim.
#[must_use]
pub(crate) fn sh_selfupdate_prelude(
    program: &str,
    prefix: &Path,
    atpkg: &Path,
    verbs: &[&str],
) -> String {
    if verbs.is_empty()
        || !verbs
            .iter()
            .all(|v| crate::selfupdate::is_shell_safe_verb(v))
        || crate::store::ToolName::new(program).is_none()
        || !crate::selfupdate::is_shell_safe_verb(program)
    {
        return String::new();
    }
    let patterns = verbs.join("|");
    let guard = crate::selfupdate::REENTRY_VAR;
    let mut s = String::from("# atpkg: `");
    s.push_str(program);
    s.push(' ');
    s.push_str(&patterns);
    s.push_str("` is answered by `atpkg ");
    s.push_str(crate::selfupdate::HIDDEN_VERB);
    s.push_str(SH_SELFUPDATE_NOTE);
    s.push_str("case \"$1\" in\n  ");
    s.push_str(&patterns);
    s.push_str(")\n    __atpkg=");
    s.push_str(&sh_quote_str(&atpkg.to_string_lossy()));
    s.push_str("\n    if [ -x \"$__atpkg\" ] && [ \"${");
    s.push_str(guard);
    s.push_str("-}\" != \"$$\" ]; then ");
    s.push_str(guard);
    s.push_str("=$$ exec \"$__atpkg\" ");
    s.push_str(crate::selfupdate::HIDDEN_VERB);
    s.push(' ');
    s.push_str(&sh_quote_str(program));
    s.push(' ');
    s.push_str(&sh_quote_str(&prefix.to_string_lossy()));
    s.push_str(" -- \"$@\"; fi\n    ;;\nesac\n");
    s
}

/// THE `--yolo` BLOCK of an agent program's twin and `bin/` shim ([`crate::yolo`]).
#[must_use]
pub(crate) fn sh_yolo_prelude(
    program: &str,
    policy: &crate::yolo::Policy,
    skip_extra: &[&str],
) -> String {
    let crate::yolo::Policy {
        flags,
        markers,
        after,
        skip,
    } = *policy;
    if flags.is_empty()
        || markers.is_empty()
        || !flags.iter().all(|f| crate::yolo::is_shell_safe_flag(f))
        || !markers.iter().all(|m| crate::yolo::is_shell_safe_marker(m))
        || !after
            .iter()
            .chain(skip.iter())
            .chain(skip_extra.iter())
            .all(|w| crate::selfupdate::is_shell_safe_verb(w))
        || crate::store::ToolName::new(program).is_none()
        || !crate::selfupdate::is_shell_safe_verb(program)
    {
        return String::new();
    }
    let skip_all: Vec<&str> = skip
        .iter()
        .copied()
        .chain(skip_extra.iter().copied().filter(|w| !skip.contains(w)))
        .collect();
    let spelled = flags
        .iter()
        .map(|f| sh_quote_str(f))
        .collect::<Vec<_>>()
        .join(" ");
    let words = !after.is_empty() || !skip_all.is_empty();
    let fresh = |indent: &str| {
        let mut f = String::new();
        for (i, flag) in flags.iter().enumerate() {
            f.push_str(indent);
            f.push_str("if [ \"$__atpkg_p");
            f.push_str(&i.to_string());
            f.push_str("\" = 0 ]; then __atpkg_p");
            f.push_str(&i.to_string());
            f.push_str("=1; set -- \"$@\" ");
            f.push_str(&sh_quote_str(flag));
            f.push_str("; fi\n");
        }
        f
    };
    let say = |prefix: &str| {
        let mut v = String::from("\"");
        v.push_str(prefix);
        v.push_str(&sh_quote_str(program));
        v.push('"');
        v
    };
    let mut s = String::from("# atpkg: yolo by default — this name runs ");
    s.push_str(program);
    s.push_str(" with every approval and sandbox off (");
    s.push_str(&spelled);
    s.push_str(
        "); `--yolo` ahead of a literal `--` spells the same, a launch that types \
         its own posture runs as typed, and the command as run is printed on stderr \
         (aterm help pkg).\n",
    );
    s.push_str(
        "__atpkg_no_harness=0\n\
         __atpkg_nh_stop=0\n\
         __atpkg_n=$#\n\
         while [ \"$__atpkg_n\" -gt 0 ]; do\n  \
         __atpkg_n=$((__atpkg_n - 1))\n  \
         if [ \"$__atpkg_nh_stop\" = 0 ] && { [ \"$1\" = \"--no-harness\" ] || [ \"$1\" = \"--no-alab-harness\" ]; }; then\n    \
         __atpkg_no_harness=1\n    \
         shift\n  \
         else\n    \
         if [ \"$1\" = \"--\" ]; then __atpkg_nh_stop=1; fi\n    \
         set -- \"$@\" \"$1\"\n    \
         shift\n  \
         fi\n\
         done\n\
         if [ \"$__atpkg_no_harness\" = 1 ]; then ATERM_NO_HARNESS=1; export ATERM_NO_HARNESS; fi\n",
    );
    s.push_str("__atpkg_yolo=0\n__atpkg_posture=0\n");
    s.push_str("__atpkg_have_word=0\n__atpkg_word=\n__atpkg_word_at=0\n__atpkg_word_ok=1\n");
    if !crate::yolo::word_options(program).optional.is_empty() {
        s.push_str("__atpkg_maybe=0\n__atpkg_gap=0\n__atpkg_joins=0\n");
    }
    for i in 0..flags.len() {
        s.push_str("__atpkg_p");
        s.push_str(&i.to_string());
        s.push_str("=0\n");
    }
    s.push_str(
        "for __atpkg_a in \"$@\"; do\n  \
         case \"$__atpkg_a\" in\n    \
         --) break ;;\n    \
         --yolo) __atpkg_yolo=1 ;;\n",
    );
    for (i, flag) in flags.iter().enumerate() {
        s.push_str("    ");
        s.push_str(&sh_quote_str(flag));
        s.push_str(") __atpkg_p");
        s.push_str(&i.to_string());
        s.push_str("=1; __atpkg_posture=1 ;;\n");
    }
    let marker_pattern = |m: &str| {
        if m.ends_with('=') || (m.len() == 2 && m.starts_with('-') && !m.starts_with("--")) {
            let mut p = sh_quote_str(m);
            p.push('*');
            p
        } else {
            let mut p = sh_quote_str(m);
            p.push('|');
            p.push_str(&sh_quote_str(&format!("{m}=")));
            p.push('*');
            p
        }
    };
    let rest: Vec<String> = markers
        .iter()
        .filter(|m| !flags.contains(m))
        .map(|m| marker_pattern(m))
        .collect();
    if !rest.is_empty() {
        s.push_str("    ");
        s.push_str(&rest.join("|"));
        s.push_str(") __atpkg_posture=1 ;;\n");
    }
    s.push_str("  esac\ndone\n");
    let crate::yolo::WordOptions {
        values,
        optional,
        switches,
        own_yolo,
    } = crate::yolo::word_options(program);
    let quoted = |words: &[&str]| {
        words
            .iter()
            .map(|f| sh_quote_str(f))
            .collect::<Vec<_>>()
            .join("|")
    };
    let standalone: Vec<&str> = std::iter::once(crate::yolo::WORD)
        .chain(own_yolo.iter().copied())
        .chain(flags.iter().copied())
        .chain(switches.iter().copied())
        .collect();
    let removable: Vec<&str> = std::iter::once(crate::yolo::WORD)
        .chain(own_yolo.iter().copied())
        .collect();
    s.push_str(
        "__atpkg_at=0\n__atpkg_value=0\n\
         for __atpkg_a in \"$@\"; do\n  \
         __atpkg_at=$((__atpkg_at + 1))\n  \
         if [ \"$__atpkg_a\" = \"--\" ]; then break; fi\n  \
         if [ \"$__atpkg_value\" = 1 ]; then __atpkg_value=0; continue; fi\n",
    );
    if !optional.is_empty() {
        // An optional value takes a bare next token; a `--yolo` (or an own spelling) right
        // after a bare one, then a bare word, is a removal that would join them (`yolo::Stop`).
        s.push_str(
            "  if [ \"$__atpkg_maybe\" = 1 ]; then\n    \
             __atpkg_maybe=0\n    \
             case \"$__atpkg_a\" in\n      \
             -*) ;;\n      \
             *) continue ;;\n    \
             esac\n    \
             case \"$__atpkg_a\" in\n      ",
        );
        s.push_str(&quoted(&removable));
        s.push_str(
            ") __atpkg_gap=1 ;;\n      \
             *) __atpkg_gap=0 ;;\n    \
             esac\n  \
             elif [ \"$__atpkg_gap\" = 1 ]; then\n    \
             case \"$__atpkg_a\" in\n      ",
        );
        s.push_str(&quoted(&removable));
        s.push_str(
            ") ;;\n      \
             -*) __atpkg_gap=0 ;;\n      \
             *) __atpkg_joins=1; __atpkg_word_ok=0; break ;;\n    \
             esac\n  \
             fi\n",
        );
    }
    s.push_str("  case \"$__atpkg_a\" in\n    ");
    s.push_str(&quoted(&standalone));
    s.push_str(") ;;\n");
    if !values.is_empty() {
        s.push_str("    ");
        s.push_str(&quoted(values));
        s.push_str(") __atpkg_value=1 ;;\n");
    }
    if !optional.is_empty() {
        s.push_str("    ");
        s.push_str(&quoted(optional));
        s.push_str(") __atpkg_maybe=1 ;;\n");
    }
    if !values.is_empty() || !optional.is_empty() {
        s.push_str("    ");
        s.push_str(
            &values
                .iter()
                .chain(optional.iter())
                .map(|f| marker_pattern(f))
                .collect::<Vec<_>>()
                .join("|"),
        );
        s.push_str(") ;;\n");
    }
    s.push_str(
        "    -*) __atpkg_word_ok=0; break ;;\n    \
         *) __atpkg_have_word=1; __atpkg_word=$__atpkg_a; \
         __atpkg_word_at=$__atpkg_at; break ;;\n  \
         esac\ndone\n\
         if [ \"$__atpkg_value\" = 1 ]; then __atpkg_word_ok=0; fi\n",
    );
    if !after.is_empty() {
        s.push_str(
            "__atpkg_after=0\n\
             if [ \"$__atpkg_have_word\" = 1 ]; then\n  \
             case \"$__atpkg_word\" in\n    ",
        );
        s.push_str(
            &after
                .iter()
                .map(|w| sh_quote_str(w))
                .collect::<Vec<_>>()
                .join("|"),
        );
        s.push_str(") __atpkg_after=1 ;;\n  esac\nfi\n");
    }
    s.push_str("__atpkg_say=\nif [ \"$__atpkg_yolo\" = 1 ]");
    if !after.is_empty() {
        s.push_str(" && [ \"$__atpkg_word_ok\" = 1 ]");
    }
    if !optional.is_empty() {
        s.push_str(" && [ \"$__atpkg_joins\" = 0 ]");
    }
    s.push_str("; then\n  __atpkg_say=");
    s.push_str(&say(crate::yolo::ECHO_PREFIX));
    s.push_str(
        "\n  \
         __atpkg_stop=0\n  __atpkg_at=0\n",
    );
    if !after.is_empty() {
        s.push_str("  __atpkg_pending=0\n");
    }
    s.push_str(
        "  __atpkg_n=$#\n  \
         while [ \"$__atpkg_n\" -gt 0 ]; do\n    \
         __atpkg_n=$((__atpkg_n - 1))\n    \
         __atpkg_at=$((__atpkg_at + 1))\n    \
         if [ \"$__atpkg_stop\" = 0 ] && [ \"$1\" = \"--yolo\" ]; then\n      \
         shift\n",
    );
    if after.is_empty() {
        s.push_str(&fresh("      "));
    } else {
        s.push_str(
            "      if [ \"$__atpkg_after\" = 1 ] && [ \"$__atpkg_at\" -lt \"$__atpkg_word_at\" ]; then\n        \
             __atpkg_pending=1\n      \
             else\n",
        );
        s.push_str(&fresh("        "));
        s.push_str("      fi\n");
    }
    if !own_yolo.is_empty() {
        // `yolo::decide` drops the program's own spellings of the posture ahead of the first
        // word, and none past an option the scan does not know.
        s.push_str(
            "    elif [ \"$__atpkg_stop\" = 0 ] && [ \"$__atpkg_word_ok\" = 1 ] && \
             { [ \"$__atpkg_have_word\" = 0 ] || [ \"$__atpkg_at\" -lt \"$__atpkg_word_at\" ]; } && \
             { ",
        );
        s.push_str(
            &own_yolo
                .iter()
                .map(|w| format!("[ \"$1\" = {} ]", sh_quote_str(w)))
                .collect::<Vec<_>>()
                .join(" || "),
        );
        s.push_str("; }; then\n      shift\n");
    }
    s.push_str(
        "    else\n      \
         if [ \"$1\" = \"--\" ]; then __atpkg_stop=1; fi\n",
    );
    if after.is_empty() {
        s.push_str(
            "      set -- \"$@\" \"$1\"\n      \
             shift\n",
        );
    } else {
        s.push_str(
            "      set -- \"$@\" \"$1\"\n      \
             shift\n      \
             if [ \"$__atpkg_pending\" = 1 ] && [ \"$__atpkg_at\" = \"$__atpkg_word_at\" ]; then\n        \
             __atpkg_pending=0\n",
        );
        s.push_str(&fresh("          "));
        s.push_str("      fi\n");
    }
    s.push_str(
        "    fi\n  done\nelif [ \"$__atpkg_posture\" = 0 ] && [ \"$__atpkg_word_ok\" = 1 ]; then\n",
    );
    if !words {
        s.push_str("  __atpkg_say=");
        s.push_str(&say(crate::yolo::DEFAULT_ECHO_PREFIX));
        s.push_str("\n  set -- ");
        s.push_str(&spelled);
        s.push_str(" \"$@\"\n");
    } else {
        if skip_all.is_empty() {
            s.push_str("  __atpkg_say=");
            s.push_str(&say(crate::yolo::DEFAULT_ECHO_PREFIX));
            s.push('\n');
        } else {
            s.push_str(
                "  if [ \"$__atpkg_have_word\" = 1 ]; then\n    case \"$__atpkg_word\" in\n      ",
            );
            s.push_str(
                &skip_all
                    .iter()
                    .map(|w| sh_quote_str(w))
                    .collect::<Vec<_>>()
                    .join("|"),
            );
            s.push_str(") ;;\n      *) __atpkg_say=");
            s.push_str(&say(crate::yolo::DEFAULT_ECHO_PREFIX));
            s.push_str(" ;;\n    esac\n  else\n    __atpkg_say=");
            s.push_str(&say(crate::yolo::DEFAULT_ECHO_PREFIX));
            s.push_str("\n  fi\n");
        }
        if after.is_empty() {
            s.push_str("  if [ -n \"$__atpkg_say\" ]; then\n    set -- ");
            s.push_str(&spelled);
            s.push_str(" \"$@\"\n  fi\n");
        } else {
            s.push_str(
                "  if [ -n \"$__atpkg_say\" ] && [ \"$__atpkg_after\" = 0 ]; then\n    \
                 set -- ",
            );
            s.push_str(&spelled);
            s.push_str(
                " \"$@\"\n  \
                 elif [ -n \"$__atpkg_say\" ]; then\n    \
                 __atpkg_n=$#\n    \
                 __atpkg_at=0\n    \
                 while [ \"$__atpkg_n\" -gt 0 ]; do\n      \
                 __atpkg_n=$((__atpkg_n - 1))\n      \
                 __atpkg_at=$((__atpkg_at + 1))\n      \
                 set -- \"$@\" \"$1\"\n      \
                 if [ \"$__atpkg_at\" = \"$__atpkg_word_at\" ]; then\n        \
                 set -- \"$@\" ",
            );
            s.push_str(&spelled);
            s.push_str(
                "\n      \
                 fi\n      \
                 shift\n    \
                 done\n  \
                 fi\n",
            );
        }
    }
    s.push_str(
        "fi\n\
         if [ -n \"$__atpkg_say\" ]; then\n  \
         printf '%s' \"$__atpkg_say\" 1>&2\n  \
         for __atpkg_a in \"$@\"; do\n    \
         printf \" '\" 1>&2\n    \
         __atpkg_rest=$__atpkg_a\n    \
         while case \"$__atpkg_rest\" in *\"'\"*) true ;; *) false ;; esac; do\n      \
         printf '%s' \"${__atpkg_rest%%\"'\"*}\" 1>&2\n      \
         printf '%s' \"'\\\\''\" 1>&2\n      \
         __atpkg_rest=${__atpkg_rest#*\"'\"}\n    \
         done\n    \
         printf '%s' \"$__atpkg_rest\" 1>&2\n    \
         printf \"'\" 1>&2\n  \
         done\n  \
         printf '\\n' 1>&2\n\
         fi\n",
    );
    s
}

/// [`sh_shim_content_routed`] with `prelude` (an agent program's [`twin_prelude`], else empty)
/// right after the header comment.
#[cfg(any(unix, test))]
pub(crate) fn sh_shim_content_twin(
    target: &Path,
    env: &crate::shim_env::ShimEnv,
    route: Option<&Path>,
    prelude: &str,
) -> String {
    sh_shim_body(target, env, route, None, prelude)
}

/// An admitted ALab primary runs through its stable exec entry.
#[cfg(unix)]
pub(crate) fn sh_shim_content_managed(
    target: &Path,
    env: &crate::shim_env::ShimEnv,
    launch: &Path,
    prelude: &str,
) -> String {
    sh_shim_body(target, env, None, Some(launch), prelude)
}

/// The one body every Unix shim renders through.
#[cfg(any(unix, test))]
fn sh_shim_body(
    target: &Path,
    env: &crate::shim_env::ShimEnv,
    route: Option<&Path>,
    launch: Option<&Path>,
    prelude: &str,
) -> String {
    let mut s = String::from(
        "#!/bin/sh\n# atpkg shim — exec so the tool authenticates at its real path.\n",
    );
    s.push_str(prelude);
    if !env.is_empty() {
        s.push_str(SH_SHIM_ENV_NOTE);
    }
    for (name, value) in env.entries() {
        s.push_str("export ");
        s.push_str(name);
        s.push('=');
        s.push_str(&sh_quote_str(value));
        s.push('\n');
    }
    if cfg!(target_os = "macos")
        && let Some(block) = target
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(crate::build_cache::shim_block)
    {
        s.push_str(&block);
    }
    let target_q = sh_shim_quote(target);
    if let Some(launch) = launch {
        let launch = sh_shim_quote(launch);
        s.push_str("[ -L ");
        s.push_str(&launch);
        s.push_str(" ] && [ -x ");
        s.push_str(&launch);
        s.push_str(" ] && exec ");
        s.push_str(&launch);
        s.push_str(" \"$@\"\n");
        s.push_str("export ATPKG_MANAGED_LAUNCH_PID=\"$$\"\nexport ATPKG_MANAGED_LAUNCH_PATH=");
        s.push_str(&launch);
        s.push_str("\nexport ATPKG_MANAGED_LAUNCH_TARGET=");
        s.push_str(&target_q);
        s.push('\n');
    }
    if let Some(route) = route {
        let marker = route
            .parent()
            .and_then(Path::parent)
            .map_or_else(|| route.to_path_buf(), crate::compat::root_marker);
        let (r, m) = (sh_shim_quote(route), sh_shim_quote(&marker));
        s.push_str(SH_SHIM_ROUTE_NOTE);
        for (test, path) in [
            ("[ ! -h ", &r),
            ("[ -f ", &r),
            ("[ -x ", &r),
            ("[ -f ", &target_q),
            ("[ ! -h ", &m),
            ("[ -f ", &m),
        ] {
            s.push_str(test);
            s.push_str(path);
            s.push_str(" ] && ");
        }
        s.push_str("exec ");
        s.push_str(&r);
        s.push_str(" \"$@\"\n");
    }
    let target = target_q;
    s.push_str("exec ");
    s.push_str(&target);
    s.push_str(" \"$@\"\n");
    s
}

/// The environment a Unix shim exports.
#[cfg(any(unix, test))]
pub(crate) fn parse_sh_shim_env(content: &str) -> crate::shim_env::ShimEnv {
    let mut raw: Vec<String> = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("export ") else {
            continue;
        };
        let Some((name, quoted)) = rest.split_once("='") else {
            continue;
        };
        if matches!(
            name,
            "ATPKG_MANAGED_LAUNCH_PID"
                | "ATPKG_MANAGED_LAUNCH_PATH"
                | "ATPKG_MANAGED_LAUNCH_TARGET"
        ) {
            continue;
        }
        let Some(value) = quoted.strip_suffix('\'') else {
            continue;
        };
        let mut entry = String::from(name);
        entry.push('=');
        entry.push_str(&value.replace("'\\''", "'"));
        raw.push(entry);
    }
    crate::shim_env::ShimEnv::admit(&raw).unwrap_or_default()
}

/// Single-quote a path for the `sh` stub, escaping embedded quotes POSIX-style.
#[cfg(any(unix, test))]
pub(crate) fn sh_shim_quote(target: &Path) -> String {
    sh_quote_str(&target.to_string_lossy())
}

/// [`sh_shim_quote`] over a string.
pub(crate) fn sh_quote_str(s: &str) -> String {
    let mut out = String::from("'");
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// The target a Unix shim execs (its first `exec '…' "$@"` line).
#[cfg(any(unix, test))]
pub(crate) fn parse_sh_shim_target(content: &str) -> Option<PathBuf> {
    for line in content.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("exec '")
            && let Some(end) = rest.rfind("' \"$@\"")
        {
            return Some(PathBuf::from(rest[..end].replace("'\\''", "'")));
        }
    }
    None
}

/// Bound for reading a shim of either dialect before parsing it.
pub(crate) const MAX_SHIM_BYTES: usize = 64 * 1024;

/// Read the Windows `.cmd` shim through the bounded, symlink-refusing metadata seam.
#[cfg(any(windows, test))]
pub(crate) fn read_cmd_shim_target(path: &Path) -> Option<PathBuf> {
    let content = crate::metadata_io::read_bounded_regular_utf8(path, MAX_SHIM_BYTES).ok()?;
    parse_cmd_shim_target(&content)
}

/// The environment the Windows `.cmd` shim at `path` exports ([`parse_cmd_shim_env`]).
#[cfg(any(windows, test))]
pub(crate) fn read_cmd_shim_env(path: &Path) -> crate::shim_env::ShimEnv {
    match crate::metadata_io::read_bounded_regular_utf8(path, MAX_SHIM_BYTES) {
        Ok(content) => parse_cmd_shim_env(&content),
        Err(_) => crate::shim_env::ShimEnv::NONE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The clamp's decision.
    #[cfg(unix)]
    #[test]
    fn only_an_absolute_executable_runs_behind_the_utility_clamp() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = crate::testdir::dir("atpkg-clamp-");
        let launcher = dir.join("taskpolicy");
        std::fs::write(&launcher, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
        let plain = dir.join("plain");
        std::fs::write(&plain, "").unwrap();
        let argv = |program: &Path, launcher: &Path| -> Vec<String> {
            utility_clamped_with(program.as_os_str(), launcher)
                .into_iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect()
        };
        let l = launcher.to_string_lossy().into_owned();
        assert_eq!(
            argv(Path::new("/bin/sh"), &launcher),
            [l.as_str(), "-c", "utility", "/bin/sh"]
        );
        let missing = dir.join("no-such-tool");
        for (program, why) in [
            (Path::new("sh"), "a relative program"),
            (missing.as_path(), "a missing program"),
            (plain.as_path(), "a file with no execute bit"),
            (&*dir, "a directory"),
        ] {
            assert_eq!(
                argv(program, &launcher),
                [program.to_string_lossy().into_owned()],
                "{why} is started bare, so it fails as itself"
            );
        }
        assert_eq!(
            argv(Path::new("/bin/sh"), &dir.join("no-launcher")),
            ["/bin/sh"],
            "no launcher, no clamp"
        );
    }

    /// The live contract.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_detached_program_starts_at_utility_and_a_waited_one_at_the_default_band() {
        type Spawn =
            fn(&std::ffi::OsStr, &[std::ffi::OsString], Option<(&str, &str)>) -> io::Result<()>;
        let pri_of = |spawn: Spawn| -> i32 {
            let dir = crate::testdir::dir("atpkg-detached-band-");
            let said = dir.join("pri");
            let script = format!(
                "/bin/ps -o pri= -p $$ > '{0}.tmp' && mv '{0}.tmp' '{0}'",
                said.display()
            );
            spawn("/bin/sh".as_ref(), &["-c".into(), script.into()], None).unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
            let pri = loop {
                if let Ok(text) = std::fs::read_to_string(&said) {
                    break text;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "the detached program never ran"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            };
            pri.trim()
                .parse()
                .unwrap_or_else(|_| panic!("no priority in {pri:?}"))
        };
        let pri = pri_of(spawn_detached);
        assert!(
            pri <= 20,
            "a detached pass ran at pri {pri}, not utility (20)"
        );
        let pri = pri_of(spawn_detached_waited);
        assert!(
            pri > 20,
            "a first run's detached pass ran at pri {pri}, below the default band (31)"
        );
    }

    /// Observed at creation: the temp is private from the instant it exists.
    #[cfg(unix)]
    #[test]
    fn a_private_temp_is_never_created_wider_than_0600() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = crate::testdir::dir("atpkg-private-new-");
        let path = dir.join(".zshrc.atpkg-1.tmp");
        let f = create_private_new(&path).unwrap();
        assert_eq!(
            f.metadata().unwrap().permissions().mode() & 0o777,
            0o600,
            "the temp must be private from the instant it exists"
        );
        drop(f);
        assert!(
            create_private_new(&path).is_err(),
            "exclusive: an existing temp is never opened and truncated"
        );
    }

    #[test]
    fn cmd_shim_content_wraps_target_and_forwards_args() {
        let c = cmd_shim_content(Path::new("C:\\store\\ay\\18\\bin\\ay.exe"));
        let mut want = cmd_frame();
        want.push_str("@\"C:\\store\\ay\\18\\bin\\ay.exe\" %* & @exit /b\r\n");
        assert_eq!(c, want);
    }

    #[test]
    fn cmd_shim_round_trips_through_parse() {
        let target = PathBuf::from("C:\\store\\ay\\18\\bin\\ay.exe");
        let content = cmd_shim_content(&target);
        assert_eq!(parse_cmd_shim_target(&content), Some(target));
    }

    #[test]
    fn tombstone_content_has_no_forward_target_and_exits_70() {
        let c = cmd_tombstone_content("atpkg: ay was yanked/revoked — run `aterm pkg update`");
        assert!(c.contains("1>&2"), "notice goes to stderr: {c}");
        assert!(c.contains("exit /b 70"), "exits 70: {c}");
        assert!(c.is_ascii(), "a .cmd tombstone is ASCII: {c}");
        assert_eq!(parse_cmd_shim_target(&c), None);
    }

    #[test]
    fn cmd_echo_escape_neutralizes_metacharacters() {
        let esc = cmd_echo_escape("a&b|c>d<e^f%g\"h");
        assert_eq!(esc, "a^&b^|c^>d^<e^^f%%g^\"h");
    }

    #[test]
    fn cmd_shim_with_env_sets_then_forwards_and_round_trips() {
        let target = PathBuf::from("C:\\store\\claude\\2026082701\\bin\\claude.exe");
        let env = crate::shim_env::ShimEnv::admit(&[
            "DISABLE_AUTOUPDATER=1".to_string(),
            "B=two words".to_string(),
        ])
        .unwrap();
        let c = cmd_shim_content_env(&target, &env);
        let mut want = cmd_frame();
        want.push_str(
            "@set \"DISABLE_AUTOUPDATER=1\"\r\n@set \"B=two words\"\r\n\
             @\"C:\\store\\claude\\2026082701\\bin\\claude.exe\" %* & @exit /b\r\n",
        );
        assert_eq!(c, want);
        assert_eq!(parse_cmd_shim_target(&c), Some(target.clone()));
        assert_eq!(parse_cmd_shim_env(&c), env);
        assert_eq!(
            cmd_shim_content_env(&target, &crate::shim_env::ShimEnv::NONE),
            cmd_shim_content(&target),
            "no env: the shim every manifest without the key gets"
        );
        assert_eq!(
            parse_cmd_shim_env(&cmd_shim_content(&target)),
            crate::shim_env::ShimEnv::NONE
        );
        assert_eq!(
            parse_cmd_shim_env(&cmd_tombstone_content("atpkg: x was yanked")),
            crate::shim_env::ShimEnv::NONE
        );
        assert_eq!(
            parse_cmd_shim_env(
                "@set \"DISABLE_AUTOUPDATER=1\"\r\n@set \"PATH=C:\\x\"\r\n@\"C:\\a.exe\" %* & @exit /b\r\n"
            ),
            crate::shim_env::ShimEnv::NONE
        );
        assert!(cmd_env_is_injection_safe(&env));
        assert!(cmd_env_is_injection_safe(&crate::shim_env::ShimEnv::NONE));
        let root = crate::testdir::dir("atpkg-cmd-shim-env-");
        let path = root.join("claude.cmd");
        std::fs::write(&path, &c).unwrap();
        assert_eq!(read_cmd_shim_env(&path), env);
        assert_eq!(read_cmd_shim_target(&path), Some(target));
        assert_eq!(
            read_cmd_shim_env(&root.join("absent.cmd")),
            crate::shim_env::ShimEnv::NONE
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// On a real Windows host the backend writes, resolves and reads the env-carrying `.cmd`.
    #[cfg(windows)]
    #[test]
    fn windows_backend_lays_and_reads_an_env_shim() {
        let root = crate::testdir::dir("atpkg-win-env-shim-");
        let target = root.join("claude.exe");
        std::fs::write(&target, b"x").unwrap();
        let shim = root.join("claude.cmd");
        let env = crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".to_string()]).unwrap();
        install_shim_to_env(&shim, &target, &env).unwrap();
        assert_eq!(
            std::fs::read_to_string(&shim).unwrap(),
            cmd_shim_content_env(&target, &env)
        );
        assert_eq!(resolve_shim(&shim), Some(target.clone()));
        assert_eq!(shim_env_of(&shim), env);
        install_shim_to(&shim, &target).unwrap();
        assert_eq!(shim_env_of(&shim), crate::shim_env::ShimEnv::NONE);
        assert_eq!(resolve_shim(&shim), Some(target));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn parse_cmd_shim_target_keys_on_the_forward_tail_and_reads_the_legacy_one() {
        assert_eq!(parse_cmd_shim_target("not a shim\r\n"), None);
        assert_eq!(parse_cmd_shim_target(""), None);
        let target = PathBuf::from("C:\\a.exe");
        assert_eq!(
            parse_cmd_shim_target("@\"C:\\a.exe\" %* & @exit /b\r\n"),
            Some(target.clone())
        );
        assert_eq!(
            parse_cmd_shim_target("@\"C:\\a.exe\" %*\r\n"),
            Some(target),
            "a legacy shim still resolves until it is re-laid"
        );
        for not_a_forward in [
            "@\"C:\\a.exe\"\r\n",
            "@\"C:\\a.exe\" %1\r\n",
            "@\"C:\\a.exe\" %* & @exit /b 3\r\n",
            "@\"C:\\a.exe\" %* & exit /b\r\n",
            "@\"C:\\a.exe\" %*& @exit /b\r\n",
            "@\"C:\\a.exe\" __landing \"claude\" \"C:\\pkg\" -- %* & @exit /b\r\n",
        ] {
            assert_eq!(
                parse_cmd_shim_target(not_a_forward),
                None,
                "{not_a_forward:?}"
            );
        }
    }

    #[test]
    fn a_legacy_cmd_twin_with_the_landing_prelude_still_resolves_to_the_store() {
        let prefix = Path::new("C:\\Program Files (x86)\\aterm\\pkg");
        let marker = Path::new("C:\\Program Files (x86)\\aterm\\pkg\\landing\\claude");
        let atpkg = Path::new("C:\\Program Files (x86)\\aterm\\app\\atpkg.exe");
        let target = Path::new(
            "C:\\Program Files (x86)\\aterm\\pkg\\store\\claude\\2026091601\\bin\\claude.exe",
        );
        let env = crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".to_string()]).unwrap();
        let prelude = cmd_landing_prelude("claude", prefix, marker, atpkg);
        assert_eq!(
            prelude,
            "@rem atpkg agents twin: while a newer build of this program is landing, atpkg __landing \
             waits for it and then runs the new one (aterm help pkg).\r\n\
             @if not exist \"C:\\Program Files (x86)\\aterm\\pkg\\landing\\claude\" goto store\r\n\
             @if not exist \"C:\\Program Files (x86)\\aterm\\app\\atpkg.exe\" goto store\r\n\
             @\"C:\\Program Files (x86)\\aterm\\app\\atpkg.exe\" __landing \"claude\" \
             \"C:\\Program Files (x86)\\aterm\\pkg\" -- %* & @exit /b\r\n\
             :store\r\n"
        );
        assert_eq!(prelude.matches("atpkg.exe").count(), 2);
        let twin = cmd_shim_content_twin(target, &env, &prelude);
        let mut want = cmd_frame();
        want.push_str(&prelude);
        want.push_str("@set \"DISABLE_AUTOUPDATER=1\"\r\n");
        want.push_str(
            "@\"C:\\Program Files (x86)\\aterm\\pkg\\store\\claude\\2026091601\\bin\\claude.exe\" \
             %* & @exit /b\r\n",
        );
        assert_eq!(twin, want);
        for line in twin.split("\r\n").filter(|l| !l.is_empty()) {
            assert!(
                line.starts_with('@') || line.starts_with(':'),
                "silent: {line}"
            );
            assert!(
                !line.ends_with('(') && !line.starts_with(')'),
                "no block: {line}"
            );
            if line.starts_with("@\"") {
                assert!(
                    line.ends_with(CMD_FORWARD_TAIL),
                    "ends the batch on the line that runs the program: {line}"
                );
            } else {
                assert!(
                    !line.contains("exit /b") || line == CMD_PADDING_EXIT.trim_end(),
                    "no exit line of its own past the frame's: {line}"
                );
            }
            assert!(!line.starts_with("@call"), "no call: {line}");
        }
        assert!(!twin.contains("errorlevel"), "{twin}");
        assert_eq!(
            twin.matches(" & @exit /b").count(),
            2,
            "the hand-over and the forward, each ending the batch: {twin}"
        );
        assert_eq!(
            twin.matches("\r\n@exit /b\r\n").count(),
            1,
            "the frame's padding exit, the one exit line of its own: {twin}"
        );
        assert!(!twin.contains('\n') || twin.matches("\r\n").count() == twin.matches('\n').count());
        assert!(!twin.contains("where atpkg"));
        assert!(!twin.contains("\\\""), "{twin}");
        assert_eq!(parse_cmd_shim_target(&twin), Some(target.to_path_buf()));
        assert_eq!(parse_cmd_shim_env(&twin), env);
        assert_eq!(
            parse_cmd_shim_target(
                "@\"C:\\app\\atpkg.exe\" __landing \"claude\" \"C:\\pkg\" -- %* & @exit /b\r\n"
            ),
            None,
            "the hand-over line alone is no forward line"
        );
        assert_eq!(
            parse_cmd_shim_target("@\"C:\\a.exe\"\r\n"),
            None,
            "a quoted program with no forward tail is not the shim's forward"
        );
        assert_eq!(
            strip_verbatim_prefix(Path::new("\\\\?\\UNC\\srv\\share\\aterm\\atpkg.exe")),
            PathBuf::from("\\\\srv\\share\\aterm\\atpkg.exe")
        );
        assert_eq!(
            strip_verbatim_prefix(Path::new("\\\\?\\C:\\aterm\\atpkg.exe")),
            PathBuf::from("C:\\aterm\\atpkg.exe")
        );
        assert_eq!(
            strip_verbatim_prefix(Path::new("/usr/local/aterm/atpkg")),
            PathBuf::from("/usr/local/aterm/atpkg")
        );
        assert!(twin.find("__landing").unwrap() < twin.find("@set ").unwrap());
        assert!(twin.trim_end().ends_with("\" %* & @exit /b"));
        assert!(!cmd_shim_content_env(target, &env).contains("__landing"));
        assert_eq!(
            cmd_shim_content_twin(target, &env, ""),
            cmd_shim_content_env(target, &env)
        );
        let root = crate::testdir::dir("atpkg-cmd-twin-");
        let path = root.join("claude.cmd");
        std::fs::write(&path, &twin).unwrap();
        assert_eq!(read_cmd_shim_target(&path), Some(target.to_path_buf()));
        assert_eq!(read_cmd_shim_env(&path), env);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_cmd_twin_renders_no_prelude_and_is_the_plain_framed_shim() {
        let prefix = Path::new("C:\\Program Files (x86)\\aterm\\pkg");
        let atpkg = Path::new("C:\\Program Files (x86)\\aterm\\app\\atpkg.exe");
        let target = Path::new(
            "C:\\Program Files (x86)\\aterm\\pkg\\store\\claude\\2026091601\\bin\\claude.exe",
        );
        if cfg!(windows) {
            assert_eq!(
                twin_prelude(
                    "claude",
                    prefix,
                    Some(atpkg),
                    &["update", "upgrade", "install"],
                    &crate::yolo::policy_of("claude")
                ),
                "",
                "on Windows the twin carries no prelude"
            );
        }
        let env = crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".to_string()]).unwrap();
        let twin = cmd_shim_content_twin(target, &env, "");
        assert_eq!(
            twin,
            cmd_shim_content_env(target, &env),
            "the plain framed shim"
        );
        assert_eq!(twin.matches(" & @exit /b").count(), 1, "{twin}");
        assert_eq!(twin.matches("\r\n@exit /b\r\n").count(), 1, "{twin}");
        assert!(!twin.contains("%~1"), "{twin}");
        assert!(!twin.contains(crate::selfupdate::HIDDEN_VERB), "{twin}");
        assert!(!twin.contains(crate::landing::HIDDEN_VERB), "{twin}");
        assert_eq!(parse_cmd_shim_target(&twin), Some(target.to_path_buf()));
        assert_eq!(parse_cmd_shim_env(&twin), env);
    }

    #[test]
    fn a_legacy_cmd_re_laid_while_executing_resumes_into_the_frame_at_every_offset() {
        fn legacy(target: &Path, env: &crate::shim_env::ShimEnv) -> String {
            let mut s = String::new();
            for (n, v) in env.entries() {
                s.push_str("@set \"");
                s.push_str(n);
                s.push('=');
                s.push_str(v);
                s.push_str("\"\r\n");
            }
            s.push_str("@\"");
            s.push_str(&target.to_string_lossy());
            s.push('"');
            s.push_str(CMD_LEGACY_FORWARD_TAIL);
            s.push_str("\r\n");
            s
        }
        let prefix = Path::new("C:\\Program Files (x86)\\aterm\\pkg");
        let marker = Path::new("C:\\Program Files (x86)\\aterm\\pkg\\landing\\claude");
        let atpkg = Path::new("C:\\Program Files (x86)\\aterm\\app\\atpkg.exe");
        let target = Path::new(
            "C:\\Program Files (x86)\\aterm\\pkg\\store\\claude\\2026091601\\bin\\claude.exe",
        );
        let none = crate::shim_env::ShimEnv::NONE;
        let env = crate::shim_env::ShimEnv::admit(&[
            "DISABLE_AUTOUPDATER=1".to_string(),
            "B=two words".to_string(),
        ])
        .unwrap();
        let longest_raw: Vec<String> = (0..crate::shim_env::MAX_SHIM_ENV)
            .map(|i| format!("V{i}={}", "x".repeat(crate::shim_env::MAX_ENTRY_BYTES - 3)))
            .collect();
        let longest_env = crate::shim_env::ShimEnv::admit(&longest_raw).unwrap();
        let longest_target = PathBuf::from(format!(
            "C:\\{}\\claude.exe",
            "p".repeat(CMD_LEGACY_TARGET_BOUND_BYTES - "C:\\\\claude.exe".len())
        ));
        assert_eq!(
            longest_target.to_string_lossy().len(),
            CMD_LEGACY_TARGET_BOUND_BYTES
        );
        let prelude = cmd_landing_prelude("claude", prefix, marker, atpkg);
        assert!(!prelude.is_empty());
        let cases = [
            (
                "legacy plain twin -> framed twin behind the prelude",
                legacy(target, &none),
                cmd_shim_content_twin(target, &none, &prelude),
            ),
            (
                "legacy env shim -> framed env shim",
                legacy(target, &env),
                cmd_shim_content_env(target, &env),
            ),
            (
                "longest legacy shim -> its framed successor",
                legacy(&longest_target, &longest_env),
                cmd_shim_content_env(&longest_target, &longest_env),
            ),
        ];
        assert_eq!(
            cases[2].1.len(),
            CMD_LEGACY_FILE_BOUND_BYTES,
            "the longest case IS the derived bound"
        );
        for (what, old, new) in &cases {
            assert_eq!(
                parse_cmd_shim_target(old),
                parse_cmd_shim_target(new),
                "{what}"
            );
            assert_eq!(parse_cmd_shim_env(old), parse_cmd_shim_env(new), "{what}");
            let old = old.as_bytes();
            let new = new.as_bytes();
            assert!(
                old.len() <= CMD_PADDING_END_BYTES,
                "{what}: {} bytes",
                old.len()
            );
            let ends: Vec<usize> = old
                .iter()
                .enumerate()
                .filter(|&(_, &b)| b == b'\n')
                .map(|(i, _)| i + 1)
                .collect();
            assert_eq!(ends.last().copied(), Some(old.len()), "{what}");
            assert!(
                ends.iter().all(|&e| e >= CMD_LEGACY_MIN_LINE_BYTES),
                "{what}: {ends:?}"
            );
            for offset in 0..=old.len() {
                let line = cmd_resumed_line(new, offset);
                if offset == 0 {
                    assert_eq!(
                        line.as_deref(),
                        Some("@goto :main"),
                        "{what}: the fresh run"
                    );
                    continue;
                }
                if offset < CMD_FRAME_HEAD.len() - 2 {
                    assert!(
                        !ends.contains(&offset),
                        "{what}: an old line ends at {offset}"
                    );
                    continue;
                }
                assert!(
                    cmd_resumed_line_is_inert(line.as_deref()),
                    "{what}: a resume at offset {offset} of {} reads {line:?}",
                    old.len()
                );
            }
            let at_eof = cmd_resumed_line(new, old.len()).unwrap();
            assert!(
                !at_eof.is_empty() && at_eof.bytes().all(|b| b == b':'),
                "{what}: {at_eof:?}"
            );
            for end in &ends {
                assert!(cmd_resumed_line_is_inert(
                    cmd_resumed_line(new, *end).as_deref()
                ));
            }
        }
        let framed = cmd_shim_content(target);
        for offset in CMD_FRAME_HEAD.len() - 2..=CMD_PADDING_END_BYTES {
            let line = cmd_resumed_line(framed.as_bytes(), offset);
            assert!(
                cmd_resumed_line_is_inert(line.as_deref()),
                "{offset}: {line:?}"
            );
        }
        assert_eq!(
            cmd_resumed_line(framed.as_bytes(), CMD_PADDING_END_BYTES).as_deref(),
            Some("@exit /b")
        );
        assert_eq!(
            cmd_resumed_line(framed.as_bytes(), CMD_FRAME_HEAD.len() - 2).as_deref(),
            Some(""),
            "the head's CRLF reads empty"
        );
        assert_eq!(
            cmd_resumed_line(framed.as_bytes(), 1).as_deref(),
            Some("goto :main"),
            "why the head must sit where no old line can end"
        );
        let padding_end = framed.find("@exit /b").unwrap();
        assert!(cases[2].1.len() <= padding_end, "{}", cases[2].1.len());
        assert_eq!(
            cmd_resumed_line(framed.as_bytes(), cases[2].1.len()).as_deref(),
            Some(&":".repeat(78)[(cases[2].1.len() - CMD_FRAME_HEAD.len()) % 80..]),
            "the bound's own end-of-file offset, inside the padding"
        );
        let mut bare = prelude.clone();
        bare.push_str(&cmd_shim_body_env(target, &none));
        let old_len = cases[0].1.len();
        let line = cmd_resumed_line(bare.as_bytes(), old_len).unwrap();
        assert!(
            !cmd_resumed_line_is_inert(Some(&line)),
            "laid bare, the resume lands in the prelude: {line:?}"
        );
        assert!(framed.trim_end().ends_with(CMD_FORWARD_TAIL));
    }

    #[test]
    fn cmd_target_injection_guard_rejects_quote_percent_newline() {
        assert!(cmd_target_is_injection_safe(Path::new(
            "C:\\Users\\me\\AppData\\Local\\aterm\\pkg\\store\\ay\\18\\bin\\ay.exe"
        )));
        assert!(cmd_target_is_injection_safe(Path::new(
            "/managed/store/ay/18/bin/ay"
        )));
        assert!(!cmd_target_is_injection_safe(Path::new(
            "C:\\x\\\"&calc.exe\"\\ay.exe"
        )));
        assert!(!cmd_target_is_injection_safe(Path::new(
            "C:\\x\\%APPDATA%\\ay.exe"
        )));
        assert!(!cmd_target_is_injection_safe(Path::new(
            "C:\\x\\ay.exe\r\n@calc"
        )));
    }

    #[test]
    fn cmd_shim_reader_rejects_sparse_oversize() {
        let root = crate::testdir::dir("atpkg-cmd-shim-sparse-");
        let path = root.join("ay.cmd");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len((MAX_SHIM_BYTES + 1) as u64).unwrap();
        assert!(read_cmd_shim_target(&path).is_none());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn cmd_shim_reader_rejects_fifo_and_symlink_without_blocking() {
        use std::os::unix::ffi::OsStrExt as _;

        let root = crate::testdir::dir("atpkg-cmd-shim-special-");
        let path = root.join("ay.cmd");
        let path_c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: `path_c` is a live NUL-terminated path in our private fixture.
        assert_eq!(unsafe { libc::mkfifo(path_c.as_ptr(), 0o600) }, 0);
        assert!(read_cmd_shim_target(&path).is_none());
        std::fs::remove_file(&path).unwrap();
        let target = root.join("target.cmd");
        std::fs::write(&target, cmd_shim_content(Path::new("C:\\ay.exe"))).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(read_cmd_shim_target(&path).is_none());
        std::fs::remove_dir_all(&root).unwrap();
    }
}

#[cfg(test)]
mod sh_shim_tests {
    use super::*;

    #[test]
    fn the_sh_shim_execs_the_real_path_and_round_trips() {
        let target = Path::new("/prefix/store/trust/5520/bin/targo");
        let body = sh_shim_content(target);
        assert!(body.starts_with("#!/bin/sh\n"), "{body}");
        assert!(
            body.contains("exec '/prefix/store/trust/5520/bin/targo' \"$@\""),
            "the stub must EXEC (replacing the process image) so the tool authenticates \
             at its real path, and forward args verbatim: {body}"
        );
        assert_eq!(parse_sh_shim_target(&body).as_deref(), Some(target));
    }

    #[test]
    fn a_tombstone_is_not_an_installed_shim() {
        let notice = "#!/bin/sh\necho 'atpkg: ay was yanked' 1>&2\nexit 70\n";
        assert_eq!(parse_sh_shim_target(notice), None);
    }

    #[test]
    fn the_sh_shim_with_env_exports_then_execs_and_round_trips() {
        let target = Path::new("/prefix/store/claude/2026082701/bin/claude");
        let env = crate::shim_env::ShimEnv::admit(&[
            "DISABLE_AUTOUPDATER=1".to_string(),
            "B=it's two words".to_string(),
        ])
        .unwrap();
        let body = sh_shim_content_env(target, &env);
        assert!(
            body.contains(
                "\nexport DISABLE_AUTOUPDATER='1'\n\
                 export B='it'\\''s two words'\n\
                 exec '/prefix/store/claude/2026082701/bin/claude' \"$@\"\n"
            ),
            "{body}"
        );
        assert_eq!(parse_sh_shim_target(&body).as_deref(), Some(target));
        assert_eq!(parse_sh_shim_env(&body), env);
        assert_eq!(
            sh_shim_content_env(target, &crate::shim_env::ShimEnv::NONE),
            sh_shim_content(target),
            "no env: the shim every manifest without the key gets, byte for byte"
        );
        assert_eq!(
            parse_sh_shim_env(&sh_shim_content(target)),
            crate::shim_env::ShimEnv::NONE
        );
        assert_eq!(
            parse_sh_shim_env("#!/bin/sh\necho 'atpkg: ay was yanked' 1>&2\nexit 70\n"),
            crate::shim_env::ShimEnv::NONE
        );
        assert_eq!(
            parse_sh_shim_env(
                "#!/bin/sh\nexport DISABLE_AUTOUPDATER='1'\nexport PATH='/x'\nexec '/a' \"$@\"\n"
            ),
            crate::shim_env::ShimEnv::NONE
        );
    }

    /// The exports reach the exec'd program, and a plain re-lay drops them.
    #[cfg(unix)]
    #[test]
    fn the_laid_wrapper_exports_into_the_exec_d_program() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = crate::testdir::dir("atpkg-sh-env-shim-");
        let target = root.join("claude");
        std::fs::write(
            &target,
            b"#!/bin/sh\nprintf '%s|%s\\n' \"${DISABLE_AUTOUPDATER:-unset}\" \"$1\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
        let shim = root.join("shim");
        let env = crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".to_string()]).unwrap();
        install_shim_to_env(&shim, &target, &env).unwrap();
        assert_eq!(resolve_shim(&shim).as_deref(), Some(target.as_path()));
        assert_eq!(shim_env_of(&shim), env);
        let run_shim = || {
            aterm_spec::wait::past_text_busy(|| {
                std::process::Command::new(&shim)
                    .arg("arg")
                    .env_remove("DISABLE_AUTOUPDATER")
                    .output()
            })
            .unwrap()
        };
        let out = run_shim();
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout), "1|arg\n");
        install_shim_to(&shim, &target).unwrap();
        assert_eq!(shim_env_of(&shim), crate::shim_env::ShimEnv::NONE);
        let out = run_shim();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "unset|arg\n");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn an_embedded_quote_cannot_escape_the_stub() {
        let nasty = Path::new("/prefix/store/o'ny/1/bin/x");
        let body = sh_shim_content(nasty);
        assert!(body.contains(r"'\''"), "quote is POSIX-escaped: {body}");
        assert_eq!(parse_sh_shim_target(&body).as_deref(), Some(nasty));
    }

    #[test]
    fn the_routed_shim_is_exact_and_parses_as_the_plain_one() {
        let target = Path::new("/p/store/trust/8595/bin/tippy");
        let route = Path::new("/p/compat/trust/8595/bin/tippy");
        let none = crate::shim_env::ShimEnv::NONE;
        let body = sh_shim_content_routed(target, &none, Some(route));
        assert_eq!(parse_sh_shim_target(&body).as_deref(), Some(target));
        assert_eq!(parse_sh_shim_env(&body), none);
        assert_eq!(
            sh_shim_content_routed(target, &none, None),
            sh_shim_content(target),
            "no route: today's shim, byte for byte"
        );

        let env = crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".to_string()]).unwrap();
        let body = sh_shim_content_routed(target, &env, Some(route));
        assert!(
            body.find("export DISABLE_AUTOUPDATER='1'").unwrap()
                < body.find("exec '/p/compat/").unwrap(),
            "both execs inherit the exports: {body}"
        );
        assert_eq!(parse_sh_shim_target(&body).as_deref(), Some(target));
        assert_eq!(parse_sh_shim_env(&body), env);
        assert_eq!(
            sh_shim_content_routed(target, &env, None),
            sh_shim_content_env(target, &env)
        );
        assert_eq!(
            body.lines()
                .filter(|l| l.trim_start().starts_with("exec '"))
                .count(),
            1
        );
    }

    #[test]
    fn a_quote_in_the_prefix_is_escaped_in_every_slot_of_the_guard() {
        let target = Path::new("/it's/store/trust/1/bin/targo");
        let route = Path::new("/it's/compat/trust/1/bin/targo");
        let body = sh_shim_content_routed(target, &crate::shim_env::ShimEnv::NONE, Some(route));
        assert_eq!(
            body.matches(r"'/it'\''s/compat/trust/1/bin/targo'").count(),
            4
        );
        assert_eq!(
            body.matches(r"'/it'\''s/store/trust/1/bin/targo'").count(),
            2
        );
        let marker = if cfg!(windows) {
            r"'/it'\''s/compat/trust/1\.atpkg-root'"
        } else {
            r"'/it'\''s/compat/trust/1/.atpkg-root'"
        };
        assert_eq!(body.matches(marker).count(), 2);
        assert!(!body.contains("/it's/"), "no raw quote survives: {body}");
        assert_eq!(parse_sh_shim_target(&body).as_deref(), Some(target));
    }

    /// The guard under a real `/bin/sh` (and `/bin/dash` where present).
    #[cfg(unix)]
    #[test]
    fn the_guard_runs_the_root_only_when_the_marked_root_stands() {
        use std::os::unix::fs::PermissionsExt as _;
        let scratch = crate::testdir::dir("atpkg-sh-route-");
        let base = scratch.join("pre fix's");
        let store_bin = base.join("store").join("trust").join("8595").join("bin");
        let root_bin = base.join("compat").join("trust").join("8595").join("bin");
        std::fs::create_dir_all(&store_bin).unwrap();
        std::fs::create_dir_all(&root_bin).unwrap();
        std::fs::create_dir_all(base.join("bin")).unwrap();
        let store = store_bin.join("tippy");
        let route = root_bin.join("tippy");
        std::fs::write(
            &store,
            b"#!/bin/sh\nprintf '%s|%s|%s|%s|%s\\n' \"$0\" \"$#\" \"$1\" \"$2\" \
              \"${DISABLE_AUTOUPDATER:-unset}\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o755)).unwrap();
        let env = crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".to_string()]).unwrap();
        let shim = base.join("bin").join("tippy");
        std::fs::write(&shim, sh_shim_content_routed(&store, &env, Some(&route))).unwrap();
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();

        let shells: Vec<&str> = ["/bin/sh", "/bin/dash"]
            .into_iter()
            .filter(|s| Path::new(s).is_file())
            .collect();
        assert!(shells.contains(&"/bin/sh"), "every Unix has /bin/sh");
        let run = |shell: &str| -> String {
            let out = std::process::Command::new(shell)
                .arg(&shim)
                .arg("a b")
                .arg("it's \"q\"")
                .env_clear()
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{shell}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8(out.stdout).unwrap()
        };
        let ran = |path: &Path| format!("{}|2|a b|it's \"q\"|1\n", path.display());

        let marker = crate::compat::root_marker(&base.join("compat").join("trust").join("8595"));
        for (what, marked, runs_route) in [
            ("clone", true, true),
            ("byte copy", true, true),
            ("clone", false, false),
            ("clone behind a symlinked marker", false, false),
            ("symlink", true, false),
            ("not executable", true, false),
            ("missing", true, false),
        ] {
            let _ = std::fs::remove_file(&route);
            let _ = std::fs::remove_file(&marker);
            match what {
                "clone" | "clone behind a symlinked marker" => {
                    crate::clone::clone_file(&store, &route).unwrap();
                }
                "byte copy" => std::fs::copy(&store, &route).map(|_| ()).unwrap(),
                "symlink" => std::os::unix::fs::symlink(&store, &route).unwrap(),
                "not executable" => {
                    std::fs::copy(&store, &route).unwrap();
                    std::fs::set_permissions(&route, std::fs::Permissions::from_mode(0o644))
                        .unwrap();
                }
                _ => {}
            }
            if marked {
                std::fs::write(&marker, b"atpkg exec root v1\n").unwrap();
            } else if what == "clone behind a symlinked marker" {
                std::os::unix::fs::symlink(&store, &marker).unwrap();
            }
            let expected = if runs_route { &route } else { &store };
            for shell in &shells {
                assert_eq!(
                    run(shell),
                    ran(expected),
                    "{shell}, route is a {what}, marker {marked}"
                );
            }
        }
        std::fs::remove_dir_all(base.join("compat")).unwrap();
        for shell in &shells {
            assert_eq!(run(shell), ran(&store), "{shell}, no root");
        }
        std::fs::remove_dir_all(base.parent().unwrap()).unwrap();
    }

    #[test]
    fn the_self_update_block_is_exact_is_the_whole_prelude_and_breaks_no_reader() {
        let prefix = Path::new("/Users//u/Library/Application Support/aterm/pkg");
        let atpkg = Path::new("/Applications/aterm.app/Contents/MacOS/atpkg");
        let target = Path::new(
            "/Users//u/Library/Application Support/aterm/pkg/store/claude/2026091902/bin/claude",
        );
        let verbs = crate::selfupdate::verbs_of("claude");
        let block = sh_selfupdate_prelude("claude", prefix, atpkg, verbs);
        for code in [
            "\ncase \"$1\" in\n  update|upgrade|install)\n",
            "\n    __atpkg='/Applications/aterm.app/Contents/MacOS/atpkg'\n",
            "__ATPKG_SELFUPDATE_PID=$$ exec \"$__atpkg\" __selfupdate 'claude' \
             '/Users//u/Library/Application Support/aterm/pkg' -- \"$@\"; fi\n    ;;\nesac\n",
        ] {
            assert!(block.contains(code), "{code:?} in {block}");
        }
        let codex =
            sh_selfupdate_prelude("codex", prefix, atpkg, crate::selfupdate::verbs_of("codex"));
        assert!(codex.contains("\n  update)\n"), "{codex}");
        assert!(codex.contains("__selfupdate 'codex' "), "{codex}");
        assert_eq!(sh_selfupdate_prelude("claude", prefix, atpkg, &[]), "");
        assert_eq!(
            sh_selfupdate_prelude("ay", prefix, atpkg, crate::selfupdate::verbs_of("ay")),
            "",
            "a program with no row has no verbs and no block"
        );
        assert_eq!(
            sh_selfupdate_prelude("muse", prefix, atpkg, crate::selfupdate::verbs_of("muse")),
            "",
            "a rostered program with no updater has no block"
        );
        assert_eq!(
            sh_selfupdate_prelude("claude", prefix, atpkg, &["update", "up|date)"]),
            ""
        );
        assert_eq!(
            sh_selfupdate_prelude("claude", prefix, atpkg, &["Update"]),
            ""
        );
        assert_eq!(
            sh_selfupdate_prelude("claude", prefix, atpkg, &["esac"]),
            ""
        );
        assert_eq!(
            sh_selfupdate_prelude("claude", prefix, atpkg, &["update", "esac"]),
            ""
        );
        assert_eq!(sh_selfupdate_prelude("cl'aude", prefix, atpkg, verbs), "");
        assert_eq!(sh_selfupdate_prelude("cl aude", prefix, atpkg, verbs), "");
        assert_eq!(
            sh_selfupdate_prelude("sudo", prefix, atpkg, verbs),
            "",
            "not a tool name"
        );
        let quoted = sh_selfupdate_prelude(
            "claude",
            Path::new("/it's/pkg"),
            Path::new("/it's/app/atpkg"),
            verbs,
        );
        assert!(quoted.contains("__atpkg='/it'\\''s/app/atpkg'"), "{quoted}");
        assert!(
            quoted.contains("__selfupdate 'claude' '/it'\\''s/pkg' -- \"$@\"; fi"),
            "{quoted}"
        );
        assert!(!quoted.contains("/it's/"), "{quoted}");
        let want = block.clone();
        let twin_pre = twin_prelude(
            "claude",
            prefix,
            Some(atpkg),
            verbs,
            &crate::yolo::Policy::NONE,
        );
        if cfg!(windows) {
            assert_eq!(twin_pre, "", "the .cmd twin renders no prelude");
        } else {
            assert_eq!(twin_pre, want);
        }
        assert!(!want.contains(crate::landing::HIDDEN_VERB), "{want}");
        assert!(!want.contains("landing"), "{want}");
        assert_eq!(
            twin_prelude(
                "claude",
                prefix,
                Some(atpkg),
                &[],
                &crate::yolo::Policy::NONE
            ),
            "",
            "no verbs, no yolo policy: no prelude — the twin is the plain shim"
        );
        let env = crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".to_string()]).unwrap();
        let twin = sh_shim_content_twin(target, &env, None, &want);
        assert_eq!(
            twin.lines()
                .filter(|l| l.trim().starts_with("exec '"))
                .count(),
            1,
            "{twin}"
        );
        assert!(
            !want.lines().any(|l| l.trim().starts_with("export ")),
            "{want}"
        );
        assert_eq!(parse_sh_shim_target(&twin).as_deref(), Some(target));
        assert_eq!(parse_sh_shim_env(&twin), env);
        assert!(!twin.contains("pending-program stub"), "{twin}");
        assert!(
            twin.find("case \"$1\"").unwrap() < twin.find("export DISABLE_AUTOUPDATER").unwrap(),
            "{twin}"
        );
        assert!(twin.trim_end().ends_with("\"$@\""));
        assert!(!sh_shim_content_env(target, &env).contains("__selfupdate"));
        assert_eq!(
            sh_shim_content_twin(target, &env, None, ""),
            sh_shim_content_env(target, &env)
        );
    }

    /// THE HARNESS HAND-OFF EXECUTED: grok's laid twin execs `<prefix>/bin/grok-alab run --` with
    /// the argv its yolo block left once a LIVE grok-alab is installed (a shim into a ready store
    /// build, or into its checkout while dev-linked), runs the store build alone under
    /// `--no-alab-harness`, `--no-harness` or `ATERM_NO_HARNESS=1`, before grok-alab is installed,
    /// and whenever `bin/grok-alab` is not live: a yanked/revoked tombstone, a shim whose build was
    /// removed or never marked ready, a checkout shim with no dev link. No other agent's twin
    /// carries the hand-off.
    #[cfg(unix)]
    #[test]
    fn the_grok_twin_hands_its_session_to_grok_alab_once_installed() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = crate::testdir::dir("atpkg-sh-harness-twin-");
        let script = |path: &Path, tag: &str| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(
                path,
                format!("#!/bin/sh\nprintf '{tag}'\nprintf '<%s>' \"$@\"\nprintf '\\n'\n"),
            )
            .unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        let target = root.join("store-build");
        script(&target, "V");
        let prefix = root.join("Application Support").join("pkg");
        let layout = crate::store::Layout {
            prefix: prefix.clone(),
        };
        std::fs::create_dir_all(prefix.join("bin")).unwrap();
        let prelude = twin_prelude("grok", &prefix, None, &[], &crate::yolo::policy_of("grok"));
        // `ATERM_NO_HARNESS` is read twice, once per block that `--no-harness` takes away: the
        // hand-off's guard, and the sign-in block's guard (its own test below). Each is there once.
        let reads = |also: &str| {
            prelude
                .lines()
                .filter(|l| l.contains("ATERM_NO_HARNESS-") && l.contains(also))
                .count()
        };
        assert_eq!(reads("bin/grok-alab'"), 1, "the hand-off: {prelude}");
        assert_eq!(reads("ATERM_CHILD-"), 1, "the sign-in guard: {prelude}");
        assert_eq!(reads(""), 2, "{prelude}");
        let twin = root.join("grok");
        std::fs::write(
            &twin,
            sh_shim_content_twin(&target, &crate::shim_env::ShimEnv::NONE, None, &prelude),
        )
        .unwrap();
        std::fs::set_permissions(&twin, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(resolve_shim(&twin).as_deref(), Some(target.as_path()));
        // The twin as laid (its `#!/bin/sh`), and under every other POSIX shell here (Linux's
        // /bin/sh is dash), each answering alike. Outside aterm and with no `aterm` on PATH, so
        // grok's sign-in block (its own test below) never runs here, and never reaches a real
        // `aterm` on a signed-out machine.
        let shells: Vec<&str> = ["/bin/dash", "/usr/bin/dash", "/bin/bash"]
            .into_iter()
            .filter(|s| Path::new(s).exists())
            .collect();
        let run = |args: &[&str], no_harness: bool| {
            let once = |shell: Option<&str>| {
                let out = aterm_spec::wait::past_text_busy(|| {
                    let mut cmd = match shell {
                        Some(sh) => {
                            let mut c = std::process::Command::new(sh);
                            c.arg(&twin);
                            c
                        }
                        None => std::process::Command::new(&twin),
                    };
                    cmd.args(args)
                        .env_remove("ATERM_NO_HARNESS")
                        .env_remove("ATERM_CHILD")
                        .env("PATH", "/usr/bin:/bin");
                    if no_harness {
                        cmd.env("ATERM_NO_HARNESS", "1");
                    }
                    cmd.output()
                })
                .unwrap();
                assert!(out.status.success(), "{shell:?} {args:?}: {out:?}");
                String::from_utf8_lossy(&out.stdout).into_owned()
            };
            let laid = once(None);
            for sh in &shells {
                assert_eq!(once(Some(sh)), laid, "{sh} {args:?}");
            }
            laid
        };
        let argv = ["-m", "grok-4", "fix it"];
        let alone = run(&argv, false);
        assert!(
            alone.starts_with("V<"),
            "not installed: the store build: {alone}"
        );
        // A live grok-alab: atpkg's shim into a ready store build.
        let alab_bin = prefix.join("bin").join("grok-alab");
        let build_dir = layout.build_dir("grok-alab", 1_000);
        let build_exe = build_dir.join("bin").join("grok-alab");
        script(&build_exe, "H");
        let ready = prefix.join("store").join("grok-alab").join("1000.ready");
        std::fs::write(&ready, "ok\n").unwrap();
        let lay_shim = |to: &Path| {
            std::fs::write(&alab_bin, sh_shim_content(to)).unwrap();
            std::fs::set_permissions(&alab_bin, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert_eq!(resolve_shim(&alab_bin).as_deref(), Some(to));
        };
        lay_shim(&build_exe);
        let handed = run(&argv, false);
        assert_eq!(
            handed,
            format!("H<run><-->{}", alone.strip_prefix('V').unwrap()),
            "the harness gets `run --` and the very argv the store build got"
        );
        assert_eq!(
            run(&["--no-alab-harness", "-m", "grok-4", "fix it"], false),
            alone
        );
        assert_eq!(
            run(&["--no-harness", "-m", "grok-4", "fix it"], false),
            alone
        );
        assert_eq!(run(&argv, true), alone);
        // A yanked or revoked grok-alab: atpkg's tombstone at bin/grok-alab (executable, exits 70,
        // no exec line) never stands between the person and grok.
        crate::platform::install_tombstone_shim(
            &alab_bin,
            "atpkg: grok-alab was yanked/revoked — run `aterm pkg update`",
        )
        .unwrap();
        assert!(
            resolve_shim(&alab_bin).is_none(),
            "a tombstone execs nothing"
        );
        assert_eq!(run(&argv, false), alone, "a tombstone: grok alone");
        // A shim whose build is not marked ready, then one whose build was removed.
        lay_shim(&build_exe);
        assert_eq!(run(&argv, false), handed, "the control: live again");
        std::fs::remove_file(&ready).unwrap();
        assert_eq!(run(&argv, false), alone, "an unfinished build: grok alone");
        std::fs::write(&ready, "ok\n").unwrap();
        std::fs::remove_dir_all(&build_dir).unwrap();
        assert_eq!(run(&argv, false), alone, "a removed build: grok alone");
        // A shim into a checkout runs only while grok-alab is dev-linked.
        let checkout_exe = root.join("checkout").join("target").join("grok-alab");
        script(&checkout_exe, "H");
        lay_shim(&checkout_exe);
        assert_eq!(run(&argv, false), alone, "no dev link: grok alone");
        let marker = layout.link_marker("grok-alab");
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        std::fs::write(&marker, "program = \"grok-alab\"\n").unwrap();
        assert_eq!(run(&argv, false), handed, "dev-linked: the harness");
        for other in ["claude", "codex", "agy", "muse", "kimi"] {
            let prelude = twin_prelude(other, &prefix, None, &[], &crate::yolo::policy_of(other));
            assert!(
                !prelude.contains("grok-alab") && !prelude.contains(" run -- "),
                "{other}"
            );
        }
    }

    /// THE YOLO BLOCK EXECUTED: laid twins over a store build that prints its argv.
    #[cfg(unix)]
    #[test]
    fn the_yolo_block_expands_prints_and_reaches_the_exec() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = crate::testdir::dir("atpkg-sh-yolo-twin-");
        let target = root.join("store-build");
        std::fs::write(&target, b"#!/bin/sh\nprintf '<%s>' \"$@\"\nprintf '\\n'\n").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
        let prefix = root.join("pkg");
        let lay = |program: &str| {
            let prelude = twin_prelude(
                program,
                &prefix,
                None,
                &[],
                &crate::yolo::policy_of(program),
            );
            assert!(
                !prelude.lines().any(|l| l.trim().starts_with("exec '")),
                "{prelude}"
            );
            assert!(
                !prelude.lines().any(|l| l.trim().starts_with("export ")),
                "{prelude}"
            );
            assert!(!prelude.contains("__selfupdate"), "{prelude}");
            let twin = root.join(program);
            std::fs::write(
                &twin,
                sh_shim_content_twin(&target, &crate::shim_env::ShimEnv::NONE, None, &prelude),
            )
            .unwrap();
            std::fs::set_permissions(&twin, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert_eq!(resolve_shim(&twin).as_deref(), Some(target.as_path()));
            twin
        };
        // Outside aterm, so grok's sign-in block (its own test below) stays out of these runs.
        let run = |twin: &Path, args: &[&str]| {
            let out = aterm_spec::wait::past_text_busy(|| {
                std::process::Command::new(twin)
                    .args(args)
                    .env_remove("ATERM_CHILD")
                    .output()
            })
            .unwrap();
            assert!(out.status.success(), "{args:?}: {out:?}");
            (
                String::from_utf8_lossy(&out.stdout).into_owned(),
                String::from_utf8_lossy(&out.stderr).into_owned(),
            )
        };
        let claude = lay("claude");
        assert_eq!(
            run(&claude, &["-p", "hello"]),
            (
                "<--dangerously-skip-permissions><-p><hello>\n".into(),
                "aterm: yolo (default): running 'claude' \
                 '--dangerously-skip-permissions' '-p' 'hello'\n"
                    .into()
            )
        );
        for args in [
            &["--permission-mode", "plan"][..],
            &["--permission-mode=auto"][..],
            &["--dangerously-skip-permissions", "-p", "x"][..],
        ] {
            let (stdout, stderr) = run(&claude, args);
            assert_eq!(stderr, "", "{args:?}");
            assert!(!stdout.contains("aterm:"), "{args:?}: {stdout}");
        }
        assert_eq!(
            run(&claude, &["--permission-mode", "plan"]).0,
            "<--permission-mode><plan>\n"
        );
        let codex = lay("codex");
        for args in [
            &["-sread-only", "exec", "x"][..],
            &["-a=never", "exec", "x"][..],
            &["-c", "sandbox_mode=\"read-only\"", "exec", "x"][..],
            &["resume", "thread-id"][..],
        ] {
            let (stdout, stderr) = run(&codex, args);
            assert_eq!(stderr, "", "{args:?}");
            assert!(
                !stdout.contains("--dangerously-bypass-approvals-and-sandbox"),
                "{args:?}: {stdout}"
            );
        }
        let (stdout, stderr) = run(
            &claude,
            &["--model", "opus", "--yolo", "-p", "it's $x `y`", ""],
        );
        assert_eq!(
            stdout,
            "<--model><opus><--dangerously-skip-permissions><-p><it's $x `y`><>\n"
        );
        let typed: Vec<String> = ["--model", "opus", "--yolo", "-p", "it's $x `y`", ""]
            .iter()
            .map(|w| (*w).to_string())
            .collect();
        let crate::yolo::Launch::Flag(expanded) = crate::yolo::decide("claude", &typed) else {
            panic!("flag launch");
        };
        assert_eq!(
            stderr,
            format!("{}\n", crate::yolo::echo_line("claude", &expanded))
        );
        let (stdout, stderr) = run(&claude, &["--", "--yolo"]);
        assert_eq!(stdout, "<--dangerously-skip-permissions><--><--yolo>\n");
        assert_eq!(
            stderr,
            "aterm: yolo (default): running 'claude' '--dangerously-skip-permissions' \
             '--' '--yolo'\n"
        );
        let muse = lay("muse");
        let behind = "<exec><--disable-approval><--disable-sandbox><--trust-workspace>\
                      <--disable-request-user-input><x>\n";
        assert_eq!(run(&muse, &["exec", "x"]).0, behind);
        assert_eq!(run(&muse, &["--yolo", "exec", "x"]).0, behind);
        assert_eq!(run(&muse, &["exec", "--yolo", "x"]).0, behind);
        assert_eq!(
            run(&muse, &["skills", "list"]),
            ("<skills><list>\n".into(), String::new())
        );
        assert_eq!(
            run(&muse, &["--approval-mode", "never"]),
            ("<--approval-mode><never>\n".into(), String::new())
        );
        let (stdout, stderr) = run(&muse, &[]);
        assert_eq!(
            stdout,
            "<--disable-approval><--disable-sandbox><--trust-workspace>\
             <--disable-request-user-input>\n"
        );
        assert_eq!(
            stderr,
            format!(
                "{}\n",
                crate::yolo::default_echo_line(
                    "muse",
                    &crate::yolo::flags_of("muse")
                        .iter()
                        .map(|f| (*f).to_string())
                        .collect::<Vec<_>>()
                )
            )
        );
        for args in [
            &["--model", "opus", "resume", "thread-id"][..],
            &["-m", "opus", "fork", "thread-id"][..],
            &["--model=opus", "resume", "thread-id"][..],
            &["-mopus", "fork", "thread-id"][..],
            &["--cd", "/owned/path", "resume", "thread-id"][..],
            &["--new-option", "value", "resume", "thread-id"][..],
            &["--image", "one.png", "two.png", "resume", "thread-id"][..],
        ] {
            assert_eq!(
                run(&codex, args),
                (
                    format!(
                        "{}\n",
                        args.iter().map(|a| format!("<{a}>")).collect::<String>()
                    ),
                    String::new()
                ),
                "{args:?}"
            );
        }
        assert_eq!(
            run(&codex, &["--model", "resume", "fix this"]).0,
            "<--dangerously-bypass-approvals-and-sandbox><--model><resume><fix this>\n"
        );
        for args in [
            &["--model", "exec", "exec", "x"][..],
            &["--yolo", "--model", "exec", "exec", "x"][..],
            &["--model", "exec", "--yolo", "exec", "x"][..],
        ] {
            assert_eq!(
                run(&muse, args).0,
                "<--model><exec><exec><--disable-approval><--disable-sandbox>\
                 <--trust-workspace><--disable-request-user-input><x>\n",
                "{args:?}"
            );
        }
        assert_eq!(
            run(&muse, &["--new-option", "value", "--yolo", "exec", "x"]),
            (
                "<--new-option><value><--yolo><exec><x>\n".into(),
                String::new()
            )
        );
        let agy = lay("agy");
        assert_eq!(
            run(&claude, &["--no-harness", "-p", "hello"]).0,
            "<--dangerously-skip-permissions><-p><hello>\n"
        );
        assert_eq!(
            run(&agy, &["--no-harness", "hello"]).0,
            "<--dangerously-skip-permissions><hello>\n"
        );
        assert_eq!(
            run(&agy, &["--no-alab-harness", "--", "--no-harness"]).0,
            "<--dangerously-skip-permissions><--><--no-harness>\n"
        );
        // grok and kimi: the twin runs exactly what `decide` says, printing the line it prints
        // (their self-update verbs are the update intercept's, which this twin is laid without).
        let grok = lay("grok");
        let kimi = lay("kimi");
        for (program, twin, args) in [
            ("grok", &grok, &[][..]),
            ("grok", &grok, &["-m", "grok-4", "fix it"][..]),
            ("grok", &grok, &["--resume", "agent", "--json"][..]),
            ("grok", &grok, &["--yolo", "agent", "-p", "hi"][..]),
            ("grok", &grok, &["--cwd", "/w", "--yolo"][..]),
            ("grok", &grok, &["--permission-mode", "plan"][..]),
            ("grok", &grok, &["agent", "--sandbox=strict"][..]),
            ("grok", &grok, &["--unknown", "--yolo", "agent"][..]),
            ("grok", &grok, &["mcp", "list"][..]),
            ("grok", &grok, &["cursor-worker", "start"][..]),
            ("grok", &grok, &["--effort", "high", "fix it"][..]),
            ("grok", &grok, &["-w", "login"][..]),
            ("grok", &grok, &["-w", "feat", "agent", "-p", "hi"][..]),
            ("grok", &grok, &["-wfeat", "--resume", "agent"][..]),
            ("grok", &grok, &["-w", "--yolo"][..]),
            ("grok", &grok, &["-w", "--yolo", "agent", "-p", "hi"][..]),
            ("grok", &grok, &["-r", "--yolo", "--yolo", "fix it"][..]),
            ("grok", &grok, &["-r", "--no-plan", "--yolo", "agent"][..]),
            (
                "grok",
                &grok,
                &["--yolo", "--dangerously-skip-permissions", "agent"][..],
            ),
            ("kimi", &kimi, &[][..]),
            ("kimi", &kimi, &["--yolo"][..]),
            ("kimi", &kimi, &["-m", "k2", "--yolo", "--", "--yolo"][..]),
            ("kimi", &kimi, &["--agent", "coder", "hello"][..]),
            ("kimi", &kimi, &["-y"][..]),
            ("kimi", &kimi, &["-phi"][..]),
            ("kimi", &kimi, &["--session=abc"][..]),
            ("kimi", &kimi, &["--unknown", "--yolo"][..]),
            ("kimi", &kimi, &["acp"][..]),
            ("kimi", &kimi, &["--yolo", "-y"][..]),
            ("kimi", &kimi, &["--yes", "--yolo", "--auto-approve"][..]),
            (
                "kimi",
                &kimi,
                &["-y", "-m", "k2", "--yolo", "--add-dir", "/w"][..],
            ),
            ("kimi", &kimi, &["--yolo", "upgrade", "-y"][..]),
            ("kimi", &kimi, &["--yolo", "-y", "--", "-y"][..]),
            ("kimi", &kimi, &["--yolo", "-S", "-y", "login"][..]),
            ("kimi", &kimi, &["--yolo", "-S", "abc", "-y"][..]),
            ("kimi", &kimi, &["--unknown", "--yolo", "-y"][..]),
        ] {
            let typed: Vec<String> = args.iter().map(|w| (*w).to_string()).collect();
            let (ran, said) = match crate::yolo::decide(program, &typed) {
                crate::yolo::Launch::AsTyped => (typed, String::new()),
                crate::yolo::Launch::Flag(v) => {
                    let line = format!("{}\n", crate::yolo::echo_line(program, &v));
                    (v, line)
                }
                crate::yolo::Launch::Default(v) => {
                    let line = format!("{}\n", crate::yolo::default_echo_line(program, &v));
                    (v, line)
                }
            };
            let shown: String = ran.iter().map(|a| format!("<{a}>")).collect();
            assert_eq!(
                run(twin, args),
                (format!("{shown}\n"), said),
                "{program} {args:?}"
            );
        }
        assert_eq!(
            run(&grok, &["agent", "-p", "hi"]).0,
            "<agent><--always-approve><-p><hi>\n"
        );
        assert_eq!(run(&kimi, &["--yolo"]).0, "<--auto>\n");
        assert_eq!(run(&kimi, &["-y", "--yolo"]).0, "<--auto>\n");
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// THE SIGN-IN BLOCK EXECUTED: grok's laid twin over a store build that prints its argv,
    /// with a stand-in `aterm` on `PATH` that records each call and exits as told. Under aterm a
    /// signed-out interactive launch runs `aterm harness signin --cli grok --wait` before the
    /// exec and runs grok only on its exit 0; a one-shot option, a subcommand, a signed-in
    /// environment or a launch outside aterm never calls it.
    #[cfg(unix)]
    #[test]
    fn the_signin_block_gates_a_signed_out_interactive_grok_on_aterm() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = crate::testdir::dir("atpkg-sh-signin-twin-");
        let target = root.join("store-build");
        std::fs::write(&target, b"#!/bin/sh\nprintf '<%s>' \"$@\"\nprintf '\\n'\n").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let calls = root.join("aterm.calls");
        let code = root.join("aterm.exit");
        let aterm = bin.join("aterm");
        std::fs::write(
            &aterm,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexit \"$(cat '{}')\"\n",
                calls.display(),
                code.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&aterm, std::fs::Permissions::from_mode(0o755)).unwrap();
        let home = root.join("home");
        std::fs::create_dir_all(home.join(".grok")).unwrap();
        let prelude = twin_prelude(
            "grok",
            &root.join("pkg"),
            None,
            &[],
            &crate::yolo::policy_of("grok"),
        );
        assert!(prelude.contains("# atpkg: sign-in gate"), "{prelude}");
        assert!(
            !prelude.lines().any(|l| l.trim().starts_with("exec '")),
            "{prelude}"
        );
        assert!(
            !prelude.lines().any(|l| l.trim().starts_with("export ")),
            "{prelude}"
        );
        let twin = root.join("grok");
        std::fs::write(
            &twin,
            sh_shim_content_twin(&target, &crate::shim_env::ShimEnv::NONE, None, &prelude),
        )
        .unwrap();
        std::fs::set_permissions(&twin, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(resolve_shim(&twin).as_deref(), Some(target.as_path()));

        let path_with = format!("{}:/usr/bin:/bin", bin.display());
        // One launch: (exit code, stdout, stderr, aterm's calls).
        let run = |args: &[&str], env: &[(&str, &str)], exit: u8| {
            let _ = std::fs::remove_file(&calls);
            std::fs::write(&code, exit.to_string()).unwrap();
            let out = aterm_spec::wait::past_text_busy(|| {
                let mut cmd = std::process::Command::new(&twin);
                cmd.args(args)
                    .env_clear()
                    .env("PATH", &path_with)
                    .env("HOME", &home);
                for (k, v) in env {
                    cmd.env(k, v);
                }
                cmd.output()
            })
            .unwrap();
            (
                out.status.code(),
                String::from_utf8_lossy(&out.stdout).into_owned(),
                String::from_utf8_lossy(&out.stderr).into_owned(),
                std::fs::read_to_string(&calls).unwrap_or_default(),
            )
        };
        let child = [("ATERM_CHILD", "1")];
        let signin = "harness signin --cli grok --wait\n";

        // Signed out, interactive: aterm runs first, and grok only on its exit 0.
        for args in [
            &[][..],
            &["fix it"][..],
            &["-m", "grok-4", "fix it"][..],
            &["-m", "login"][..],
            &["-w", "login"][..],
            &["-w", "feat", "--resume"][..],
            &["--effort=high", "fix it"][..],
            &["--", "login"][..],
            &["fix it", "login"][..],
            // Subcommands that open the interactive screen, or may, are gated too: `dashboard`
            // is in the yolo row's skip list (it takes no permission flag), not a local one.
            &["dashboard"][..],
            &["-m", "grok-4", "dashboard"][..],
            &["sessions"][..],
            &["-w", "feat", "sessions"][..],
            &["models"][..],
            &["agent", "stdio"][..],
            &["some-future-verb"][..],
        ] {
            let (exit, stdout, stderr, called) = run(args, &child, 0);
            assert_eq!(exit, Some(0), "{args:?}: {stderr}");
            assert_eq!(called, signin, "{args:?}");
            assert!(stdout.starts_with('<'), "{args:?}: grok ran: {stdout:?}");
            let (exit, stdout, stderr, called) = run(args, &child, 1);
            assert_eq!(exit, Some(69), "{args:?}: {stderr}");
            assert_eq!(called, signin, "{args:?}");
            assert_eq!(stdout, "", "{args:?}: grok never ran");
            assert_eq!(
                stderr,
                "grok: not signed in to Grok Build (aterm harness signin --cli grok says why \
                 above); nothing was started\n",
                "{args:?}"
            );
        }
        // aterm missing: one line, exit 69, nothing run.
        let (exit, stdout, stderr, called) = {
            let _ = std::fs::remove_file(&calls);
            let out = std::process::Command::new(&twin)
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", &home)
                .env("ATERM_CHILD", "1")
                .output()
                .unwrap();
            (
                out.status.code(),
                String::from_utf8_lossy(&out.stdout).into_owned(),
                String::from_utf8_lossy(&out.stderr).into_owned(),
                std::fs::read_to_string(&calls).unwrap_or_default(),
            )
        };
        assert_eq!(
            (exit, stdout.as_str(), called.as_str()),
            (Some(69), "", ""),
            "{stderr}"
        );
        assert_eq!(
            stderr,
            "grok: signed out of Grok Build, and aterm (which signs this machine in once) is \
             not on PATH; nothing was started\n"
        );

        // One-shot options, subcommands and `--no-harness` never call aterm, and grok runs.
        for args in [
            &["login"][..],
            &["logout"][..],
            &["login", "--device-auth"][..],
            &["--help"][..],
            &["-h"][..],
            &["--version"][..],
            &["-v"][..],
            &["-p", "hi"][..],
            &["--print=hi"][..],
            &["--single", "hi"][..],
            &["-m", "grok-4", "-p", "hi"][..],
            &["fix it", "-p", "hi"][..],
            &["update"][..],
            &["mcp", "list"][..],
            &["doctor"][..],
            &["leader", "kill"][..],
            &["-w", "feat", "inspect"][..],
            &["--no-harness"][..],
            &["--no-alab-harness", "fix it"][..],
        ] {
            let (exit, stdout, stderr, called) = run(args, &child, 1);
            assert_eq!(exit, Some(0), "{args:?}: {stderr}");
            assert_eq!(called, "", "{args:?}");
            assert!(stdout.starts_with('<'), "{args:?}: {stdout:?}");
        }

        // Signed in by its environment or files: aterm is never called.
        let auth = home.join(".grok").join("auth.json");
        let elsewhere = root.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let pooled = elsewhere.join("pooled.json");
        std::fs::write(elsewhere.join("auth.json"), b"{}").unwrap();
        std::fs::write(&pooled, b"{}").unwrap();
        let elsewhere_s = elsewhere.display().to_string();
        let pooled_s = pooled.display().to_string();
        for env in [
            &[("GROK_AUTH", "x")][..],
            &[("XAI_API_KEY", "x")][..],
            &[("GROK_HOME", elsewhere_s.as_str())][..],
            &[("GROK_AUTH_PATH", pooled_s.as_str())][..],
            &[("ATERM_NO_HARNESS", "1")][..],
        ] {
            let mut all = child.to_vec();
            all.extend_from_slice(env);
            let (exit, stdout, _, called) = run(&[], &all, 1);
            assert_eq!((exit, called.as_str()), (Some(0), ""), "{env:?}");
            assert!(stdout.starts_with('<'), "{env:?}");
        }
        // An empty login file is signed out; the program's own home file signs it in.
        std::fs::write(&auth, b"").unwrap();
        assert_eq!(run(&[], &child, 1).0, Some(69));
        std::fs::write(&auth, b"{}").unwrap();
        let (exit, _, _, called) = run(&[], &child, 1);
        assert_eq!((exit, called.as_str()), (Some(0), ""));
        // A login grok refused (`auth.json.refused` at least as new) is signed out, and the gate
        // asks aterm; a newer login signs it in again.
        let marker = home.join(".grok").join("auth.json.refused");
        std::fs::write(&marker, b"").unwrap();
        let marked = std::fs::metadata(&marker).unwrap().modified().unwrap();
        std::fs::File::options()
            .write(true)
            .open(&auth)
            .unwrap()
            .set_modified(marked - std::time::Duration::from_secs(5))
            .unwrap();
        let (exit, stdout, _, called) = run(&[], &child, 1);
        assert_eq!(
            (exit, stdout.as_str(), called.as_str()),
            (Some(69), "", signin)
        );
        std::fs::File::options()
            .write(true)
            .open(&auth)
            .unwrap()
            .set_modified(marked + std::time::Duration::from_secs(5))
            .unwrap();
        let (exit, _, _, called) = run(&[], &child, 1);
        assert_eq!((exit, called.as_str()), (Some(0), ""), "a newer login");
        std::fs::remove_file(&marker).unwrap();
        // A named login file is authoritative: an empty one is signed out even beside a
        // signed-in home.
        let empty = root.join("empty.json");
        std::fs::write(&empty, b"").unwrap();
        let empty_s = empty.display().to_string();
        let (exit, _, _, called) = run(&[], &[child[0], ("GROK_AUTH_PATH", empty_s.as_str())], 1);
        assert_eq!((exit, called.as_str()), (Some(69), signin));
        std::fs::remove_file(&auth).unwrap();

        // Outside aterm the twin is unchanged: signed out, it runs grok and never calls aterm.
        for args in [&[][..], &["fix it"][..]] {
            let (exit, stdout, _, called) = run(args, &[], 1);
            assert_eq!((exit, called.as_str()), (Some(0), ""), "{args:?}");
            assert!(stdout.starts_with('<'), "{args:?}");
            let (_, stdout, _, _) = run(args, &[("ATERM_CHILD", "0")], 1);
            assert!(stdout.starts_with('<'), "{args:?}");
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_signin_block_renders_for_grok_alone_and_fail_closed() {
        let gate = crate::vendor_direct::GROK_SIGNIN_GATE;
        let grok = sh_signin_prelude("grok", "Grok Build", &gate, &["login"]);
        assert!(grok.starts_with("# atpkg: sign-in gate"), "{grok}");
        assert!(
            grok.contains("harness signin --cli 'grok' --wait"),
            "{grok}"
        );
        for program in ["claude", "codex", "kimi", "muse", "agy"] {
            let prelude = twin_prelude(
                program,
                Path::new("/p"),
                None,
                &[],
                &crate::yolo::policy_of(program),
            );
            assert!(!prelude.contains("sign-in gate"), "{program}");
        }
        use crate::vendor_direct::SigninGate;
        for bad in [
            SigninGate {
                cli: "Grok",
                ..gate
            },
            SigninGate { cli: "g'k", ..gate },
            SigninGate {
                credential_env: &["GROK AUTH"],
                ..gate
            },
            SigninGate {
                credential_env: &["$(x)"],
                ..gate
            },
            SigninGate {
                login_file_env: "a-b",
                ..gate
            },
            SigninGate {
                home_env: "",
                ..gate
            },
            SigninGate {
                home_default: "..",
                ..gate
            },
            SigninGate {
                home_default: "a/b",
                ..gate
            },
            SigninGate {
                login_file: "a\"b",
                ..gate
            },
            SigninGate {
                one_shot: &[],
                ..gate
            },
            SigninGate {
                one_shot: &["--x'y"],
                ..gate
            },
            SigninGate {
                one_shot: &["*"],
                ..gate
            },
        ] {
            assert_eq!(
                sh_signin_prelude("grok", "Grok Build", &bad, &["login"]),
                "",
                "{bad:?}"
            );
        }
        assert_eq!(sh_signin_prelude("grok", "Grok's", &gate, &[]), "");
        assert_eq!(sh_signin_prelude("grok", "", &gate, &[]), "");
        assert_eq!(
            sh_signin_prelude("grok", "Grok Build", &gate, &["esac"]),
            ""
        );
        assert_eq!(sh_signin_prelude("grok", "Grok Build", &gate, &["a|b"]), "");
        assert_eq!(sh_signin_prelude("../x", "Grok Build", &gate, &[]), "");
    }

    #[test]
    fn the_yolo_block_renders_fail_closed() {
        use crate::yolo::Policy;
        let ok = Policy {
            flags: &["--ok"],
            markers: &["--ok"],
            ..Policy::NONE
        };
        assert_eq!(sh_yolo_prelude("claude", &Policy::NONE, &[]), "");
        assert_eq!(
            sh_yolo_prelude("claude", &Policy { markers: &[], ..ok }, &[]),
            "",
            "no markers: the default cannot tell a decided launch"
        );
        assert_eq!(
            sh_yolo_prelude(
                "claude",
                &Policy {
                    flags: &["--ok", "not-a-flag"],
                    ..ok
                },
                &[]
            ),
            ""
        );
        assert_eq!(
            sh_yolo_prelude(
                "claude",
                &Policy {
                    markers: &["--yo lo"],
                    ..ok
                },
                &[]
            ),
            ""
        );
        assert_eq!(
            sh_yolo_prelude(
                "claude",
                &Policy {
                    after: &["ex'ec"],
                    ..ok
                },
                &[]
            ),
            ""
        );
        assert_eq!(sh_yolo_prelude("Claude", &ok, &[]), "");
        assert_eq!(sh_yolo_prelude("../claude", &ok, &[]), "");
        let block = sh_yolo_prelude("muse", &crate::yolo::policy_of("muse"), &[]);
        let note = block.lines().next().unwrap();
        assert!(
            note.starts_with("# ")
                && note.contains(" muse ")
                && note.contains(
                    "('--disable-approval' '--disable-sandbox' '--trust-workspace' \
                     '--disable-request-user-input')"
                ),
            "{note}"
        );
        assert!(block.contains(crate::yolo::ECHO_PREFIX), "{block}");
        assert!(block.contains(crate::yolo::DEFAULT_ECHO_PREFIX), "{block}");
    }
}
