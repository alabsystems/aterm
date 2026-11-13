// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Time formatting, string truncation, shell quoting, and safe scratch I/O.

use std::ffi::OsString;
use std::io;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds since the epoch, with the fraction.
#[must_use]
pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Milliseconds since the epoch.
#[must_use]
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Whole seconds since the epoch.
#[must_use]
pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// `HH:MM:SS.mmm`, local time: the one clock every lab log and timeline is on.
#[must_use]
pub fn stamp(t: f64) -> String {
    #[allow(clippy::cast_possible_truncation)]
    let ms = (t * 1000.0) as i64;
    format!("{}.{:03}", hms(ms.div_euclid(1000)), ms.rem_euclid(1000))
}

/// `HH:MM:SS`, local time.
#[must_use]
pub fn hms(secs: i64) -> String {
    let (_, _, _, h, m, s) = crate::sys::local_time(secs);
    format!("{h:02}:{m:02}:{s:02}")
}

/// `YYYY-MM-DD`, local time.
#[must_use]
pub fn ymd(secs: i64) -> String {
    let (y, mo, d, ..) = crate::sys::local_time(secs);
    format!("{y:04}-{mo:02}-{d:02}")
}

/// `YYYYMMDD-HHMMSS`, local time (the kept-logs directory names).
#[must_use]
pub fn compact_stamp(secs: i64) -> String {
    let (y, mo, d, h, m, s) = crate::sys::local_time(secs);
    format!("{y:04}{mo:02}{d:02}-{h:02}{m:02}{s:02}")
}

/// Folds control characters into spaces, trims ends, and truncates to `n` chars.
#[must_use]
pub fn one_line(s: &str, n: usize) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_ctl = false;
    for c in s.chars() {
        if (c as u32) < 0x20 || c == '\u{7f}' {
            if !in_ctl {
                out.push(' ');
            }
            in_ctl = true;
        } else {
            out.push(c);
            in_ctl = false;
        }
    }
    let t = out.trim();
    if t.chars().count() <= n {
        t.to_string()
    } else {
        let mut cut: String = t.chars().take(n.saturating_sub(1)).collect();
        cut.push('~');
        cut
    }
}

/// Formats `s` as a single POSIX shell word.
#[must_use]
pub fn shell_quote(s: &str) -> String {
    let safe = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "@%+=:,./_-".contains(c));
    if safe {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// Formats `<prefix>:$PATH` as a single POSIX shell word (`"<escaped>:$PATH"`, or
/// `'<prefix>':"$PATH"` when `prefix` holds a newline).
#[must_use]
pub fn shell_prepend_path(prefix: &str) -> String {
    if prefix.contains('\n') {
        return format!("{}:\"$PATH\"", shell_quote(prefix));
    }
    let mut out = String::with_capacity(prefix.len() + 8);
    out.push('"');
    for c in prefix.chars() {
        if matches!(c, '\\' | '"' | '$' | '`') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push_str(":$PATH\"");
    out
}

/// Unquotes a shell word from `env.sh`.
#[must_use]
pub fn shell_unquote(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\'' => {
                for d in it.by_ref() {
                    if d == '\'' {
                        break;
                    }
                    out.push(d);
                }
            }
            '"' => {
                while let Some(d) = it.next() {
                    match d {
                        '"' => break,
                        '\\' => {
                            if let Some(e) = it.next() {
                                out.push(e);
                            }
                        }
                        _ => out.push(d),
                    }
                }
            }
            '$' if it.peek() == Some(&'\'') => {
                it.next();
                while let Some(d) = it.next() {
                    match d {
                        '\'' => break,
                        '\\' => match it.next() {
                            Some('n') => out.push('\n'),
                            Some('t') => out.push('\t'),
                            Some('r') => out.push('\r'),
                            Some('e' | 'E') => out.push('\u{1b}'),
                            Some('x') => {
                                let mut hex = String::new();
                                for _ in 0..2 {
                                    if it.peek().is_some_and(char::is_ascii_hexdigit) {
                                        if let Some(h) = it.next() {
                                            hex.push(h);
                                        }
                                    } else {
                                        break;
                                    }
                                }
                                if let Ok(b) = u8::from_str_radix(&hex, 16) {
                                    out.push(char::from(b));
                                } else {
                                    out.push_str("\\x");
                                }
                            }
                            Some(e) => out.push(e),
                            None => {}
                        },
                        _ => out.push(d),
                    }
                }
            }
            '\\' => {
                if let Some(e) = it.next() {
                    out.push(e);
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// A `targo` started at the checkout with `args` (more may follow) that builds as `targo` typed
/// there does, so each reuses the other's build: the one that runs the lab (`$CARGO`, the store's
/// `targo` past the shim), handed the shim's `build.build-dir`
/// (`atpkg::build_cache::with_build_dir_from`), without which a nested build compiled the graph
/// again into `<checkout>/target/debug` (`mac dmg`'s benches: 48 crates, 110 s, 1.0 GB,
/// 2026-10-21); and without what `cargo run` said about the lab
/// (`atpkg::build_cache::CARGO_RUN_ENV`), which build scripts track.
#[must_use]
pub fn targo(args: &[&str]) -> aterm_uds::spawnfd::Command {
    let repo = &crate::paths::paths().repo;
    let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
    let placed =
        atpkg::build_cache::with_build_dir_from(&args, atpkg::build_cache::Tool::Targo, repo);
    let build_dir = placed
        .as_deref()
        .filter(|placed| atpkg::build_cache::loses_config(placed, atpkg::build_cache::Tool::Targo))
        .and_then(atpkg::build_cache::directory_from_placed_args)
        .map(str::to_owned);
    let args = placed.unwrap_or(args);
    let mut cmd =
        aterm_uds::spawnfd::command(std::env::var_os("CARGO").unwrap_or_else(|| "targo".into()));
    cmd.args(args).current_dir(repo);
    if let Some(build_dir) = build_dir {
        cmd.env(atpkg::build_cache::BUILD_DIR_ENV, build_dir);
    }
    clear_cargo_run_env(&mut cmd, std::env::vars_os().map(|(name, _)| name));
    cmd
}

/// Remove Cargo's run metadata from a nested build's inherited environment.
fn clear_cargo_run_env(
    cmd: &mut aterm_uds::spawnfd::Command,
    names: impl IntoIterator<Item = OsString>,
) {
    for name in names {
        if name
            .to_str()
            .is_some_and(atpkg::build_cache::said_by_cargo_run)
        {
            cmd.env_remove(name);
        }
    }
}

/// A heavy build's memory claim ([`claim_build`]): held until the build ends, `None` where
/// nothing was claimed.
pub type BuildClaim = Option<Box<atpkg::build_admit::Held>>;

/// CLAIM THE MEMORY OF A HEAVY BUILD the lab is about to start in `cwd`
/// (`docs/DESIGN-adaptive-build-2026-10-24.md`, BUILD-ADMIT), as a plain build's is
/// ([`atpkg::build_admit::admit`]): the `targo` call `args`, or, for an alias that runs a build of
/// its own past the shim, the build `like` names (`ship dev-app` builds as `--unverified build
/// --release -p aterm`). `None` where it claims nothing: a covering claim of this process or an
/// ancestor's holds it (a verify stage's, the build that runs this lab's test), this process holds
/// a claim of its own, which a second would wait on (a lane's VM claim: said), or the call is no
/// build that claims.
///
/// # Errors
/// An interrupt ended its wait, or admission refused the build (said): the ledger is missing or
/// failed, a nested build could wait on its parent's unproven lock, or a look that could not wait
/// was short. The build is not to start.
pub fn claim_build(args: &[&str], like: Option<&[&str]>, cwd: &Path) -> Result<BuildClaim, String> {
    claim_build_at(
        args,
        like,
        cwd,
        &atpkg::build_admit::Seat::live(),
        &mut |line| eprintln!("aterm-lab: {line}"),
    )
}

/// [`claim_build`] at `seat`, saying through `say`: what a test sets.
///
/// # Errors
/// As [`claim_build`].
pub fn claim_build_at(
    args: &[&str],
    like: Option<&[&str]>,
    cwd: &Path,
    seat: &atpkg::build_admit::Seat<'_>,
    say: &mut dyn FnMut(&str),
) -> Result<BuildClaim, String> {
    use atpkg::build_admit::{Admission, admit, classify};
    let words: Vec<String> = like
        .unwrap_or(args)
        .iter()
        .map(|a| (*a).to_owned())
        .collect();
    let Some(build) = classify(&words, atpkg::build_cache::Tool::Targo) else {
        return Ok(None);
    };
    match admit(&build, cwd, seat, say) {
        Admission::Claimed(held) => Ok(Some(held)),
        Admission::Stopped(e) => Err(format!("its memory claim was {e}")),
        Admission::Refused(why) => Err(why),
        Admission::Covered(_) | Admission::Own(_) => Ok(None),
    }
}

/// A claimed build's tree, metered while it runs ([`Metered::start`]).
pub struct Metered {
    meter: aterm_capacity::tree_meter::TreeMeter,
    held: Box<atpkg::build_admit::Held>,
}

impl Metered {
    /// Meter the build `pid` leads, under `claim`; `None` where nothing was claimed. Only
    /// [`spawn_claimed`] starts one, once its claim is held.
    fn start(pid: u32, claim: BuildClaim) -> Option<Metered> {
        claim.map(|held| Metered {
            meter: aterm_capacity::tree_meter::TreeMeter::start(pid),
            held,
        })
    }

    /// The build ended `end`: keep its peak (never below `floor`, its reaped tree's largest
    /// resident peak) as [`atpkg::build_admit::Ended::keep`] judges it, write its row, and let its
    /// claim go.
    pub fn end(self, end: atpkg::build_admit::BuildEnd, floor: u64) {
        #[cfg(test)]
        tests::ENDED.with(|ended| ended.borrow_mut().push(end));
        let peak = self.meter.stop(floor);
        self.held
            .end(end, peak, &mut |line| eprintln!("aterm-lab: {line}"));
    }

    /// The build's wait came back `waited`: end the claim ([`Self::end`]) with the status it read,
    /// or, where the wait failed, as an end nothing is known of (`outcome=unknown`), never let go
    /// unended. `floor` is as [`Self::end`] takes it.
    fn end_waited(self, waited: Option<std::process::ExitStatus>, floor: u64) {
        use atpkg::build_admit::BuildEnd;
        self.end(waited.map_or(BuildEnd::Unknown, BuildEnd::Status), floor);
    }
}

/// START `cmd`, the build [`claim_build`] claimed `claim` for, UNDER ITS CLAIM: taking the claim,
/// the spawn comes after it, so the build directory's lock the build takes is taken under it
/// (the order `aterm_models_atpkg::atpkg_build_admit_model` holds), and the build is handed the
/// claim's cover marker ([`aterm_capacity::COVER_ENV`]), so a build it starts claims nothing
/// more. The child, and its meter while a claim is held ([`Metered::end`] keeps the peak and lets
/// the claim go).
///
/// # Errors
/// The spawn's: the claim is let go.
pub fn spawn_claimed(
    cmd: &mut aterm_uds::spawnfd::Command,
    claim: BuildClaim,
) -> io::Result<(aterm_uds::spawnfd::Child, Option<Metered>)> {
    if let Some(held) = &claim {
        cmd.env(
            aterm_capacity::COVER_ENV,
            held.claim().record().cover_marker(),
        );
    }
    let child = cmd.spawn()?;
    let metered = Metered::start(child.id(), claim);
    Ok((child, metered))
}

/// Run `cmd`, the build [`claim_build`] claimed `claim` for, to its end ([`spawn_claimed`],
/// [`wait_claimed`]): metered while a claim is held, its peak kept as
/// `atpkg::build_admit::Held::end` keeps one. Its status.
///
/// # Errors
/// The spawn's or the wait's.
pub fn status_claimed(
    cmd: &mut aterm_uds::spawnfd::Command,
    claim: BuildClaim,
) -> io::Result<std::process::ExitStatus> {
    let (mut child, metered) = spawn_claimed(cmd, claim)?;
    wait_claimed(&mut child, metered)
}

/// WAIT FOR `child`, a build [`spawn_claimed`] started under `metered`'s claim, AND END THE CLAIM
/// whatever the wait came to ([`Metered::end`]): with the status the wait read, its reaped tree's
/// resident peak the floor, or, where it failed, as an end nothing is known of, with no floor. A
/// wait fails where this process lost the child: a debugger that attaches to the build takes it
/// as its own until it lets go, and a wait that looks in that time finds no child (`ECHILD`). Its
/// status.
///
/// # Errors
/// The wait's.
pub fn wait_claimed(
    child: &mut aterm_uds::spawnfd::Child,
    metered: Option<Metered>,
) -> io::Result<std::process::ExitStatus> {
    let waited = child.wait_with_rusage();
    if let Some(m) = metered {
        let usage = waited.as_ref().ok();
        m.end_waited(usage.map(|u| u.status), usage.map_or(0, |u| u.maxrss));
    }
    waited.map(|u| u.status)
}

/// [`status_claimed`], its stdout and stderr captured ([`output_waited`]).
///
/// # Errors
/// The spawn's, reading's or the wait's.
pub fn output_claimed(
    cmd: &mut aterm_uds::spawnfd::Command,
    claim: BuildClaim,
) -> io::Result<std::process::Output> {
    let (child, metered) = spawn_claimed(
        cmd.stdout(aterm_uds::spawnfd::Stdio::piped())
            .stderr(aterm_uds::spawnfd::Stdio::piped()),
        claim,
    )?;
    output_waited(child, metered)
}

/// [`wait_claimed`] for a child whose stdout and stderr are piped: read both to their ends, wait,
/// and end the claim whatever the read and the wait came to, with no floor (this wait reads no
/// resident peak). A build that exits while a debugger holds it closes its pipes before the
/// debugger hands it back, so the wait after the reads can find no child (`ECHILD`).
///
/// # Errors
/// Reading's or the wait's.
pub fn output_waited(
    child: aterm_uds::spawnfd::Child,
    metered: Option<Metered>,
) -> io::Result<std::process::Output> {
    let out = child.wait_with_output();
    if let Some(m) = metered {
        m.end_waited(out.as_ref().ok().map(|o| o.status), 0);
    }
    out
}

/// Atomically replaces `path` with `bytes` via [`atomic_write_mode`], through no link: the
/// writer for the lab's own scratch. A file in a folder the person named is [`write_out`].
///
/// # Errors
/// As [`atomic_write_mode`].
pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    atomic_write_mode(path, bytes, 0o644)
}

/// Atomically replaces `path` with `bytes` and `mode` via [`crate::sys::put`], which walks
/// `path` from `/` through no link: a link a program under test planted in the lab's own
/// scratch (its HOME, a world's folders) never redirects the write into another file.
///
/// # Errors
/// A relative `path`, a link on the way, or the write or the rename failing.
pub fn atomic_write_mode(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    crate::sys::put(path, mode, &crate::sys::PutSrc::Bytes(bytes))
}

/// Atomically replaces `path` with `bytes` in a folder the person named (a gallery's or a
/// report's output, the checkout) via [`aterm_uds::atomic_file::write`]: the folders on the way
/// are theirs and may be links, as `/tmp` is; a link at `path` itself is refused. The lab's own
/// scratch is written with [`atomic_write`].
///
/// # Errors
/// A link at `path`, or the write or the rename failing.
pub fn write_out(path: &Path, bytes: &[u8]) -> io::Result<()> {
    aterm_uds::atomic_file::write(path, bytes, 0o644)
}

/// Creates or truncates a regular file at `path` for writing with
/// `O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC`.
///
/// # Errors
/// Returns an error if `path` cannot be opened, is a symlink, or is not a regular file.
pub fn create_regular_file(path: &Path) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    if !f.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    f.set_len(0)?;
    Ok(f)
}

/// Opens or creates a regular file at `path` for appending with
/// `O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC`.
///
/// # Errors
/// Returns an error if `path` cannot be opened, is a symlink, or is not a regular file.
pub fn append_regular_file(path: &Path) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    if !f.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    Ok(f)
}

/// Exclusively creates a regular file at `path` for writing with `mode` and
/// `O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC`.
///
/// # Errors
/// Returns an error if `path` already exists or cannot be created.
pub fn create_new_regular_file(path: &Path, mode: u32) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
}

/// Copies a regular file at `from` (`O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC`) to a fresh or
/// truncated regular file at `to` (`O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC`), preserving permissions.
///
/// # Errors
/// Returns an error if `from` or `to` is a symlink or non-regular file, or if reading/writing fails.
pub fn copy_regular_file(from: impl AsRef<Path>, to: impl AsRef<Path>) -> io::Result<u64> {
    let (mut reader, meta) = aterm_uds::regular_file::open(from.as_ref())?;
    let mut writer = create_regular_file(to.as_ref())?;
    let copied = std::io::copy(&mut reader, &mut writer)?;
    writer.set_permissions(meta.permissions())?;
    Ok(copied)
}

/// Unlinks any existing entry at `path` and opens a fresh regular file with `O_CREAT | O_EXCL`
/// and `O_NOFOLLOW | O_CLOEXEC` (without `O_NONBLOCK`, so it is safe to pass as child stdio).
///
/// # Errors
/// Returns an error if `path` cannot be created exclusively.
pub fn create_stdio_file(path: &Path) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let _ = std::fs::remove_file(path);
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
}

/// Unlinks any existing entry at `path` and writes `bytes` into a fresh regular file.
///
/// # Errors
/// Returns an error if `path` cannot be created or written.
pub fn write_fresh(path: &Path, bytes: impl AsRef<[u8]>) -> io::Result<()> {
    use std::io::Write as _;
    create_stdio_file(path)?.write_all(bytes.as_ref())
}

/// Unlinks any existing entry at `path`, writes `bytes` into a fresh regular file, and sets `mode`
/// on the open file descriptor.
///
/// # Errors
/// Returns an error if `path` cannot be created, written, or have its mode set.
pub fn write_fresh_mode(path: &Path, bytes: impl AsRef<[u8]>, mode: u32) -> io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt as _;
    let mut file = create_stdio_file(path)?;
    file.write_all(bytes.as_ref())?;
    file.set_permissions(std::fs::Permissions::from_mode(mode))
}

/// Sets `mode` on directory `dir` via an `O_DIRECTORY | O_NOFOLLOW` descriptor, refusing symlinks.
///
/// # Errors
/// Returns an error if `dir` is not a directory or its permissions cannot be set.
pub fn set_dir_mode_nofollow(dir: &Path, mode: u32) -> io::Result<()> {
    aterm_uds::private_file::set_dir_mode_nofollow(dir, mode)
}

/// Creates `dir` and any missing ancestors with mode `0700` and narrows `dir` to `0700` without
/// following symlinks.
///
/// # Errors
/// Returns an error if `dir` cannot be created or narrowed to `0700`.
pub fn create_private_dir_all(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    set_dir_mode_nofollow(dir, 0o700)
}

/// Reads a scratch file via [`crate::sys::scratch_read`], or returns empty.
#[must_use]
pub fn scratch_text(path: &Path) -> String {
    match crate::sys::scratch_read(path) {
        Ok(Some(b)) => String::from_utf8_lossy(&b).into_owned(),
        Ok(None) => String::new(),
        Err(e) => {
            eprintln!("aterm-lab: {e}");
            String::new()
        }
    }
}

/// Maximum size (256 MiB) accepted when reading a regular file in the lab.
pub const MAX_LAB_FILE_BYTES: u64 = 256 << 20;

/// Maximum size (1 GiB) accepted when reading an executable whole ([`read_program`]). A vendor's
/// build (Claude Code's is 304 MiB, Muse Code's 404 MiB) or a debug build of aterm (515 MiB) is
/// past [`MAX_LAB_FILE_BYTES`].
pub const MAX_LAB_PROGRAM_BYTES: u64 = 1 << 30;

/// Opens a regular file at `path` with `O_NONBLOCK | O_CLOEXEC | flags` and returns its metadata.
fn open_regular_with_meta(
    path: &Path,
    flags: libc::c_int,
) -> io::Result<(std::fs::File, std::fs::Metadata)> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC | flags)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    Ok((file, meta))
}

/// Opens a regular file at `path` with `O_NONBLOCK | O_CLOEXEC` and returns its metadata.
pub fn open_regular_file_with_meta(path: &Path) -> io::Result<(std::fs::File, std::fs::Metadata)> {
    open_regular_with_meta(path, 0)
}

/// Opens a regular file at `path` with `O_NONBLOCK | O_CLOEXEC`.
pub fn open_regular_file(path: &Path) -> io::Result<std::fs::File> {
    open_regular_file_with_meta(path).map(|(file, _)| file)
}

fn read_regular_with_meta(
    file: std::fs::File,
    meta: std::fs::Metadata,
    limit: u64,
) -> io::Result<(Vec<u8>, std::fs::Metadata)> {
    use io::Read as _;
    let mib = limit >> 20;
    if meta.len() > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("file exceeds {mib} MiB limit"),
        ));
    }
    let cap = usize::try_from(meta.len().min(limit).saturating_add(1)).unwrap_or(0);
    let mut buf = Vec::with_capacity(cap);
    file.take(limit + 1).read_to_end(&mut buf)?;
    if buf.len() as u64 > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("file grew past {mib} MiB limit while reading"),
        ));
    }
    Ok((buf, meta))
}

/// Reads a regular file at `path` up to [`MAX_LAB_FILE_BYTES`] and returns its metadata.
pub fn read_regular_file_with_meta(path: &Path) -> io::Result<(Vec<u8>, std::fs::Metadata)> {
    let (file, meta) = open_regular_with_meta(path, 0)?;
    read_regular_with_meta(file, meta, MAX_LAB_FILE_BYTES)
}

/// Reads a regular file at `path` up to [`MAX_LAB_FILE_BYTES`].
pub fn read_regular_file(path: &Path) -> io::Result<Vec<u8>> {
    read_regular_file_with_meta(path).map(|(buf, _)| buf)
}

/// Reads the executable at `path` whole, up to [`MAX_LAB_PROGRAM_BYTES`].
pub fn read_program(path: &Path) -> io::Result<Vec<u8>> {
    let (file, meta) = open_regular_with_meta(path, 0)?;
    read_regular_with_meta(file, meta, MAX_LAB_PROGRAM_BYTES).map(|(buf, _)| buf)
}

/// Reads a retained regular artifact without following a linked final entry.
pub fn read_artifact(path: &Path) -> io::Result<Vec<u8>> {
    let (file, meta) = open_regular_with_meta(path, libc::O_NOFOLLOW)?;
    read_regular_with_meta(file, meta, MAX_LAB_FILE_BYTES).map(|(buf, _)| buf)
}

/// Reads UTF-8 text from a regular file at `path` up to [`MAX_LAB_FILE_BYTES`].
pub fn read_regular_text(path: &Path) -> io::Result<String> {
    String::from_utf8(read_regular_file(path)?)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Incremental `(dev, ino, offset)` tail cursor for append-only newline-delimited files.
///
/// Tracks the file's `(st_dev, st_ino)` identity and the byte offset after the last complete
/// `\n`-terminated line read so far. When `st_size == offset` on the same `(dev, ino)`, polling
/// performs zero `read(2)` syscalls and zero allocations. When the file is replaced or truncated,
/// `reset` is returned as `true` and reading restarts from byte `0`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LineTailCursor {
    id: Option<(u64, u64)>,
    offset: u64,
}

impl LineTailCursor {
    fn poll_opened(
        &mut self,
        mut file: std::fs::File,
        meta: &std::fs::Metadata,
    ) -> io::Result<(bool, String)> {
        use io::{Read as _, Seek as _, SeekFrom};
        use std::os::unix::fs::MetadataExt as _;

        let len = meta.len();
        if len > MAX_LAB_FILE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "file exceeds 256 MiB limit",
            ));
        }
        let id = (meta.dev(), meta.ino());
        let reset = self.id != Some(id) || len < self.offset;
        if reset {
            self.id = Some(id);
            self.offset = 0;
        }
        if len == self.offset {
            return Ok((reset, String::new()));
        }
        if self.offset > 0 {
            file.seek(SeekFrom::Start(self.offset))?;
        }
        let rem = MAX_LAB_FILE_BYTES - self.offset;
        let cap = usize::try_from((len - self.offset).min(rem).saturating_add(1)).unwrap_or(0);
        let mut buf = Vec::with_capacity(cap);
        file.take(rem + 1).read_to_end(&mut buf)?;
        if buf.len() as u64 > rem {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "file grew past 256 MiB limit while reading",
            ));
        }
        let complete_len = buf.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
        buf.truncate(complete_len);
        self.offset += complete_len as u64;
        Ok((reset, String::from_utf8_lossy(&buf).into_owned()))
    }

    /// Polls newly appended complete lines from a scratch file via [`crate::sys::scratch_file_with_meta`].
    pub fn poll_scratch_text(&mut self, path: &Path) -> io::Result<(bool, String)> {
        let Some((file, meta)) = crate::sys::scratch_file_with_meta(path)? else {
            let reset = self.id.take().is_some() || self.offset != 0;
            self.offset = 0;
            return Ok((reset, String::new()));
        };
        self.poll_opened(file, &meta)
    }
}

/// Copies a scratch file through a checked descriptor, preserving mode and mtime.
pub fn keep_file(from: &Path, to: &Path) {
    use io::Read as _;
    use std::os::unix::fs::PermissionsExt as _;

    let Ok(Some((source, meta))) = crate::sys::scratch_file_with_meta(from) else {
        return;
    };
    if meta.len() > MAX_LAB_FILE_BYTES {
        return;
    }
    let cap = usize::try_from(meta.len().min(MAX_LAB_FILE_BYTES).saturating_add(1)).unwrap_or(0);
    let mut bytes = Vec::with_capacity(cap);
    if source
        .take(MAX_LAB_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() as u64 > MAX_LAB_FILE_BYTES
    {
        return;
    }
    if crate::sys::put(
        to,
        meta.permissions().mode(),
        &crate::sys::PutSrc::Bytes(&bytes),
    )
    .is_ok()
        && let (Ok(m), Ok(Some(f))) = (meta.modified(), crate::sys::scratch_file(to))
    {
        let _ = f.set_modified(m);
    }
}

/// Recursively copies a directory tree without following or copying symlinks.
pub fn keep_tree(from: &Path, to: &Path) {
    if !std::fs::symlink_metadata(from).is_ok_and(|m| m.file_type().is_dir()) {
        return;
    }
    let Ok(rd) = std::fs::read_dir(from) else {
        return;
    };
    let _ = std::fs::create_dir_all(to);
    for e in rd.flatten() {
        let (src, dst) = (e.path(), to.join(e.file_name()));
        match e.file_type() {
            Ok(t) if t.is_symlink() => {}
            Ok(t) if t.is_dir() => keep_tree(&src, &dst),
            Ok(_) => keep_file(&src, &dst),
            Err(_) => {}
        }
    }
}

/// Collects regular files under `dir` via [`crate::sys::scratch_dir`] for [`scratch_text`].
pub fn walk_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(Some(entries)) = crate::sys::scratch_dir(dir) else {
        return;
    };
    for (name, kind) in entries {
        let p = dir.join(name);
        match kind {
            crate::sys::EntryKind::Dir => walk_files(&p, out),
            crate::sys::EntryKind::File => out.push(p),
        }
    }
}

/// Formats `s` like Python's `repr`.
#[must_use]
pub fn py_repr(s: &str) -> String {
    let q = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::new();
    out.push(q);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == q => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || (0x7f..=0xa0).contains(&(c as u32)) => {
                out.push_str(&format!("\\x{:02x}", c as u32))
            }
            '\u{2028}' | '\u{2029}' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(q);
    out
}

/// Formats a unix second in UTC as `YYYY-MM-DDTHH:MM:SS.000000Z`.
#[must_use]
pub fn utc_stamp(secs: i64) -> String {
    format!("{}.000000Z", utc_civil(secs))
}

/// Formats a unix second in UTC as `YYYY-MM-DDTHH:MM:SS.000Z`.
#[must_use]
pub fn utc_iso(secs: i64) -> String {
    format!("{}.000Z", utc_civil(secs))
}

/// `YYYY-MM-DDTHH:MM:SS` of a unix second in UTC.
#[must_use]
pub fn utc_civil(secs: i64) -> String {
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

const B64URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// `bytes` in base64url without padding (a JWT segment).
#[must_use]
pub fn b64url(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |acc, (i, &b)| acc | (u32::from(b) << (16 - 8 * i)));
        for i in 0..=chunk.len() {
            let idx = usize::try_from((n >> (18 - 6 * i)) & 63).unwrap_or(0);
            out.push(char::from(B64URL[idx]));
        }
    }
    out
}

/// A base64url text (padding optional) as bytes; `None` when it is not one.
#[must_use]
pub fn b64url_decode(text: &str) -> Option<Vec<u8>> {
    let body = text.trim_end_matches('=');
    let pad = text.len() - body.len();
    if body.len() % 4 == 1 || pad > 2 || (pad > 0 && !(body.len() + pad).is_multiple_of(4)) {
        return None;
    }
    let mut out = Vec::with_capacity(body.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in body.bytes() {
        let v = u32::try_from(B64URL.iter().position(|&b| b == c)?).ok()?;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((acc >> bits) & 0xff).ok()?);
        }
    }
    if (acc & ((1 << bits) - 1)) != 0 {
        return None;
    }
    Some(out)
}

#[cfg(test)]
#[path = "util_tests.rs"]
mod tests;
