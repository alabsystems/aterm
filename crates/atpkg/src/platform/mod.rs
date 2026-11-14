// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! OS-primitive abstraction.

use std::io;
use std::path::{Path, PathBuf};

/// Rename only where nothing stands at the destination, replacing no entry.
#[cfg(unix)]
pub(crate) fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    aterm_dirfd::renameat_with(
        aterm_dirfd::CWD,
        from,
        aterm_dirfd::CWD,
        to,
        aterm_dirfd::RenameFlags::NOREPLACE,
    )
    .map_err(io::Error::from)
}

/// Off Unix, directories cannot be renamed over an existing entry.
#[cfg(not(unix))]
pub(crate) fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    if std::fs::symlink_metadata(to).is_ok() {
        return Err(io::ErrorKind::AlreadyExists.into());
    }
    std::fs::rename(from, to)
}

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
    open_create_new_write(path, 0o600)
}

/// Exclusively create a fresh temporary beside `path`, without opening or removing any
/// occupied name. Only the successfully created file belongs to this operation; callers may
/// rename or clean that returned path. A dead writer's legacy name is left intact.
pub(crate) fn create_temp_beside(path: &Path, mode: u32) -> io::Result<(PathBuf, std::fs::File)> {
    let name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "temporary target has no file name",
        )
    })?;
    for _ in 0..8 {
        let salt = aterm_uds::rand::hex_token::<8>()?;
        let temp = path.with_file_name(format!(
            ".{}.tmp.{}.{salt}",
            name.to_string_lossy(),
            std::process::id()
        ));
        match open_create_new_write(&temp, mode) {
            Ok(file) => return Ok((temp, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "temporary names are occupied; existing files were left intact",
    ))
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
/// target. When the target sits in a store build and the env is non-empty, the build's
/// sidecar is recorded in the same step: a launcher that reaches the binary by its own
/// path reads its environment from there ([`crate::shim_env::of_store_binary`]), never from
/// this shim's text, so the two records must not be left to separate callers to keep in
/// step. An empty env records nothing: the build's declaration stands over a shim laid
/// plain ([`crate::ops::exec_env`]), and only a landing undeclares.
pub fn install_shim_env(
    build_bin_dir: &Path,
    tool: &crate::store::ToolName,
    shim: &Path,
    env: &crate::shim_env::ShimEnv,
) -> io::Result<()> {
    let target = build_bin_dir.join(tool.exe_file());
    install_shim_to_env(shim, &target, env)?;
    if !env.is_empty()
        && let Some(build_dir) = crate::shim_env::store_build_of(&target)
    {
        crate::shim_env::write_sidecar(&build_dir, env)?;
    }
    Ok(())
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
    let clean: String = message
        .chars()
        .map(|c| match c {
            '\u{2014}' => '-',
            '\r' | '\n' | '\0' => ' ',
            c if !c.is_ascii() || c.is_ascii_control() => '?',
            c => c,
        })
        .collect();
    let mut s = String::from("@echo ");
    s.push_str(&cmd_echo_escape(&clean));
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
pub fn twin_prelude(
    program: &str,
    prefix: &Path,
    atpkg: Option<&Path>,
    verbs: &[&str],
    yolo: &crate::yolo::Policy,
) -> String {
    let launcher = if program == "codex" && !cfg!(windows) {
        crate::native_defaults::Launcher::of_current_image(atpkg)
    } else {
        None
    };
    twin_prelude_with_launcher(program, prefix, atpkg, verbs, yolo, launcher.as_ref())
}

/// Re-render a recorded producer during whole-body inspection. Only `twin_prelude`
/// admits a launcher to newly published shims.
pub(crate) fn twin_prelude_with_launcher(
    program: &str,
    prefix: &Path,
    atpkg: Option<&Path>,
    verbs: &[&str],
    yolo: &crate::yolo::Policy,
    launcher: Option<&crate::native_defaults::Launcher>,
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
    s.push_str(&sh_yolo_prelude_with_launcher(
        program, yolo, verbs, launcher,
    ));
    #[cfg(any(unix, test))]
    s.push_str(&sh_harness_prelude(program, prefix));
    s
}

/// THE HARNESS HAND-OFF of an agent program's twin ([`crate::harness::twin_harness`]): once a
/// LIVE build of the harness is installed, the twin execs `<prefix>/bin/<tool> run --` with the
/// argv the yolo block left, so the session runs under the harness
/// (`docs/DESIGN-harness-repos-2026-10-24.md` §11 D32); `--no-alab-harness`,
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
#[cfg(test)]
#[must_use]
pub(crate) fn sh_yolo_prelude(
    program: &str,
    policy: &crate::yolo::Policy,
    skip_extra: &[&str],
) -> String {
    sh_yolo_prelude_with_launcher(program, policy, skip_extra, None)
}

fn sh_yolo_prelude_with_launcher(
    program: &str,
    policy: &crate::yolo::Policy,
    skip_extra: &[&str],
    launcher: Option<&crate::native_defaults::Launcher>,
) -> String {
    let launcher = launcher.filter(|_| program == "codex");
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
    let mut s = String::new();
    if launcher.is_some() {
        s.push_str("# atpkg: the exact Codex executable decides implicit permission defaults when its compiled defaults support them; legacy builds keep the wrapper flags.\n");
    } else {
        s.push_str("# atpkg: yolo by default — this name runs ");
        s.push_str(program);
        s.push_str(" with every approval and sandbox off (");
        s.push_str(&spelled);
        s.push_str(
            "); `--yolo` ahead of a literal `--` spells the same, a launch that types \
         its own posture runs as typed, and the command as run is printed on stderr \
         (aterm help pkg).\n",
        );
    }
    if let Some(launcher) = launcher {
        s.push_str(&launcher.shell_assignment());
        s.push_str("__atpkg_codex_default=0\n");
    }
    let delegate = |s: &mut String| {
        if launcher.is_some() {
            s.push_str(
                "  if [ -n \"$__atpkg_say\" ] && [ -x \"$__atpkg_codex_launcher\" ]; then\n    \
                 __atpkg_codex_default=1\n    __atpkg_say=\n  fi\n",
            );
        }
    };
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
    if crate::yolo::cli_name(program) == "codex" {
        s.push_str(
            "__atpkg_cfg_next=0\n\
             for __atpkg_a in \"$@\"; do\n  \
             case \"$__atpkg_a\" in --) break ;; esac\n  \
             if [ \"$__atpkg_cfg_next\" = 1 ]; then\n    \
             __atpkg_cfg_value=$__atpkg_a\n    __atpkg_cfg_next=0\n  \
             else\n    case \"$__atpkg_a\" in\n      \
             '-c'|'--config') __atpkg_cfg_next=1; continue ;;\n      \
             '--config='*) __atpkg_cfg_value=${__atpkg_a#--config=} ;;\n      \
             '-c'?*) __atpkg_cfg_value=${__atpkg_a#-c}; __atpkg_cfg_value=${__atpkg_cfg_value#=} ;;\n      \
             *) continue ;;\n    esac\n  fi\n  \
             case \"$__atpkg_cfg_value\" in\n    *=*)\n      \
             __atpkg_cfg_key=${__atpkg_cfg_value%%=*}\n      \
             __atpkg_cfg_key=\"${__atpkg_cfg_key#\"${__atpkg_cfg_key%%[![:space:]]*}\"}\"\n      \
             __atpkg_cfg_key=\"${__atpkg_cfg_key%\"${__atpkg_cfg_key##*[![:space:]]}\"}\"\n      \
             case \"$__atpkg_cfg_key\" in sandbox_mode|approval_policy) __atpkg_posture=1 ;; esac\n      \
             ;;\n  esac\ndone\n",
        );
    }
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
        if launcher.is_some() {
            s.push('\n');
            delegate(&mut s);
            s.push_str("  if [ -n \"$__atpkg_say\" ]; then set -- ");
        } else {
            s.push_str("\n  set -- ");
        }
        s.push_str(&spelled);
        s.push_str(if launcher.is_some() {
            " \"$@\"; fi\n"
        } else {
            " \"$@\"\n"
        });
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
        delegate(&mut s);
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
    let launcher = crate::native_defaults::Launcher::recorded(prelude);
    let native_exec = |target: &str| {
        launcher.as_ref().map_or_else(String::new, |launcher| {
            format!(
                "if [ \"$__atpkg_codex_default\" = 1 ]; then exec {} {}{} 'codex' {target} -- \"$@\"; fi\n",
                sh_shim_quote(&launcher.path),
                if launcher.pkg { "pkg " } else { "" },
                crate::native_defaults::VERB,
            )
        })
    };
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
        if launcher.is_some() {
            s.push_str("[ -L ");
            s.push_str(&launch);
            s.push_str(" ] && [ -x ");
            s.push_str(&launch);
            s.push_str(" ] && ");
            s.push_str(&native_exec(&launch));
        }
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
        let mut guards = String::new();
        for (test, path) in [
            ("[ ! -h ", &r),
            ("[ -f ", &r),
            ("[ -x ", &r),
            ("[ -f ", &target_q),
            ("[ ! -h ", &m),
            ("[ -f ", &m),
        ] {
            guards.push_str(test);
            guards.push_str(path);
            guards.push_str(" ] && ");
        }
        if launcher.is_some() {
            s.push_str(&guards);
            s.push_str(&native_exec(&r));
        }
        s.push_str(&guards);
        s.push_str("exec ");
        s.push_str(&r);
        s.push_str(" \"$@\"\n");
    }
    let target = target_q;
    s.push_str(&native_exec(&target));
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

/// Decode one exact forward line written by the Unix shim producer. Only its single-quote
/// spelling is accepted; shell expansion, commands and differently quoted operands are not.
#[must_use]
pub fn sh_exec_target(line: &str) -> Option<PathBuf> {
    let quoted = line.strip_prefix("exec ")?.strip_suffix(" \"$@\"")?;
    let inner = quoted.strip_prefix('\'')?.strip_suffix('\'')?;
    let target = inner.replace("'\\''", "'");
    (sh_quote_str(&target) == quoted).then(|| PathBuf::from(target))
}

mod native_claude_restart;
#[cfg(unix)]
mod probe_shim;
#[cfg(unix)]
mod shim_producer;
pub use native_claude_restart::{NativeClaudeRestart, native_claude_restart};
#[cfg(unix)]
pub(crate) use probe_shim::executable as executable_for_current_user;
#[cfg(unix)]
pub use probe_shim::{ProbeForwarder, ProbeImage, ProbeShim, probe_shim};

/// The target a Unix shim execs (its first `exec '…' "$@"` line).
#[cfg(any(unix, test))]
pub(crate) fn parse_sh_shim_target(content: &str) -> Option<PathBuf> {
    for line in content.lines() {
        let line = line.trim();
        if let Some(target) = sh_exec_target(line) {
            return Some(target);
        }
    }
    None
}

/// Bound for reading a shim of either dialect before parsing it.
pub(crate) const MAX_SHIM_BYTES: usize = 64 * 1024;

/// What a single read of a shim learns: its resolved target (through its body or, on Unix, a
/// symlink), its exported environment (`NONE` for a link, tombstone, stub, or absent file), and
/// its UTF-8 regular-file body when `shim` is a regular file within [`MAX_SHIM_BYTES`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShimRead {
    /// The target `shim` forwards to ([`resolve_shim`]).
    pub target: Option<PathBuf>,
    /// The environment `shim` exports ([`shim_env_of`]).
    pub env: crate::shim_env::ShimEnv,
    /// The UTF-8 regular-file body of `shim`, when `shim` is a regular file within [`MAX_SHIM_BYTES`].
    pub body: Option<String>,
}

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
#[path = "mod_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "mod_sh_shim_tests.rs"]
mod sh_shim_tests;
