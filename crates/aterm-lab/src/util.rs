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
/// a claim of its own, which a second would wait on (a lane's VM claim: said), the ledger failed
/// (said), or the call is no build that claims.
///
/// # Errors
/// An interrupt ended its wait: the build is not to start.
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
mod tests {
    use aterm_capacity::{Admit, Fixed, Ledger, Pace, Reading};
    use atpkg::build_admit::BuildEnd;

    const GIB: u64 = 1 << 30;

    /// A seat on `dir`'s scratch ledger and records, on a 24 GiB machine, asked by `pid`.
    fn seat<'m>(dir: &std::path::Path, m: &'m Fixed, pid: u32) -> atpkg::build_admit::Seat<'m> {
        atpkg::build_admit::Seat {
            ledger: Some(Ledger::at(dir.join("ledger"))),
            machine: m,
            own: Some(dir.join("targo-memory.json")),
            verify: Some(dir.join("stage-memory.json")),
            mode: Admit::Wait,
            pace: Pace::default(),
            pid,
            marker: None,
        }
    }

    /// A heavy build claims as a plain one, an alias as the build it is said to run, and a
    /// claimed build that succeeded keeps its peak (here its reaped shell's floor, more than 0).
    /// NEGATIVE CONTROL: the alias named as nothing else claims nothing, and a build that fails
    /// with a peak below what it claimed keeps none.
    #[test]
    fn a_heavy_build_claims_and_keeps_its_peak_as_a_plain_build_does() {
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let m = Fixed::new(Reading::of_gib(24, 20, 14));
        let s = seat(dir.path(), &m, std::process::id().wrapping_add(21));
        let records = || aterm_capacity::book::Records::load(&dir.path().join("targo-memory.json"));
        let dev_app = ["--unverified", "ship", "dev-app"];
        let like = ["--unverified", "build", "--release", "-p", "aterm"];
        let none = super::claim_build_at(&dev_app, None, dir.path(), &s, &mut |_| {});
        assert!(
            matches!(none, Ok(None)),
            "an alias claims nothing by itself"
        );
        let run = |script: &str| {
            let claim = super::claim_build_at(&dev_app, Some(&like), dir.path(), &s, &mut |_| {})
                .expect("admitted")
                .expect("claimed");
            assert_eq!(claim.claim().record().need.what, "targo build -p aterm");
            assert_eq!(claim.claim().record().need.mem, 12 * GIB, "cold");
            let mut cmd = aterm_uds::spawnfd::command("/bin/sh");
            cmd.args(["-c", script]);
            super::status_claimed(&mut cmd, Some(claim)).expect("ran")
        };
        assert_eq!(run("exit 1").code(), Some(1));
        assert!(
            records().rows().is_empty(),
            "a failed build below its claim keeps nothing"
        );
        assert!(run("exit 0").success());
        let kept = records();
        assert_eq!(kept.rows().len(), 1);
        assert_eq!(kept.rows()[0].key.variant.as_deref(), Some("release"));
        assert!(kept.rows()[0].peaks[0].bytes > 0);
    }

    /// A claimed lab build starts under its claim ([`super::spawn_claimed`]): a stand-in that
    /// looks for its parent's claim (a file `<start>-<ppid>-<seq>.claim`) at its start finds it,
    /// and is handed that claim's cover marker. NEGATIVE CONTROL (by hand: `spawn_claimed`
    /// letting the claim go before it spawns): it finds none, and is handed no marker.
    #[test]
    fn a_claimed_lab_build_starts_under_its_claim() {
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let m = Fixed::new(Reading::of_gib(24, 20, 14));
        let s = seat(dir.path(), &m, std::process::id().wrapping_add(31));
        let args = ["--unverified", "build", "-p", "aterm-lab", "--bin", "x"];
        let claim = super::claim_build_at(&args, None, dir.path(), &s, &mut |_| {})
            .expect("admitted")
            .expect("claimed");
        let ledger = dir.path().join("ledger");
        let marker = claim.claim().record().cover_marker();
        let mut cmd = aterm_uds::spawnfd::command("/bin/sh");
        cmd.args([
            "-c",
            &format!(
                "echo \"${{ATERM_CAPACITY_COVER-unset}}\"; \
                 for f in '{}'/*-\"$PPID\"-*.claim; do [ -e \"$f\" ] && {{ echo held; exit 0; }}; \
                 done; echo none",
                ledger.display()
            ),
        ])
        .env_remove(aterm_capacity::COVER_ENV)
        .stdout(aterm_uds::spawnfd::Stdio::piped());
        let (child, metered) = super::spawn_claimed(&mut cmd, Some(claim)).expect("spawned");
        let out = super::output_waited(child, metered).expect("its end");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            format!("{marker}\nheld\n")
        );
    }

    thread_local! {
        /// How [`super::Metered::end`] ended each claim this thread ended, in turn.
        pub(super) static ENDED: std::cell::RefCell<Vec<BuildEnd>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }

    /// This thread's claim ends since the last call.
    fn ended() -> Vec<BuildEnd> {
        ENDED.with(|ended| std::mem::take(&mut *ended.borrow_mut()))
    }

    /// A claimed lab build's claim ends ([`super::Metered::end`], which writes the row of a claim
    /// held in this user's own ledger) whatever its wait came to ([`super::wait_claimed`],
    /// [`super::output_waited`]): a build this process lost leaves the wait with `ECHILD`, and its
    /// claim ends as an end nothing is known of (`outcome=unknown`).
    /// A debugger's attach is such a loss; here the test reaps the child behind its handle's back,
    /// which leaves the handle's wait the same `ECHILD` (macOS hands out pids in turn, so the pid
    /// names no other child by then). NEGATIVE CONTROL: the same build, kept, ends its claim with
    /// the status its wait read.
    #[test]
    fn a_claimed_build_whose_wait_fails_still_ends_its_claim() {
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let m = Fixed::new(Reading::of_gib(24, 20, 14));
        let s = seat(dir.path(), &m, std::process::id().wrapping_add(41));
        let args = ["--unverified", "build", "-p", "aterm-lab", "--bin", "x"];
        let start = |piped: bool| {
            let claim = super::claim_build_at(&args, None, dir.path(), &s, &mut |_| {})
                .expect("admitted")
                .expect("claimed");
            let mut cmd = aterm_uds::spawnfd::command("/bin/sh");
            cmd.args(["-c", "echo out; echo err >&2; exit 3"]);
            if piped {
                cmd.stdout(aterm_uds::spawnfd::Stdio::piped())
                    .stderr(aterm_uds::spawnfd::Stdio::piped());
            } else {
                cmd.stdout(aterm_uds::spawnfd::Stdio::null())
                    .stderr(aterm_uds::spawnfd::Stdio::null());
            }
            super::spawn_claimed(&mut cmd, Some(claim)).expect("spawned")
        };
        let lose = |child: &aterm_uds::spawnfd::Child| {
            let pid = i32::try_from(child.id()).expect("a pid");
            let mut raw = 0;
            // SAFETY: `raw` is a live local `waitpid` writes; the pid is this test's own child.
            #[expect(
                unsafe_code,
                reason = "waitpid(2): reap the child behind its handle's back"
            )]
            let reaped = unsafe { libc::waitpid(pid, &raw mut raw, 0) };
            assert_eq!(reaped, pid);
        };
        let exit_3 = |end: &BuildEnd| matches!(end, BuildEnd::Status(s) if s.code() == Some(3));
        // Ends an earlier test left on this thread (libtest runs a test on its own thread when it
        // can spawn none) are not this test's.
        ended();

        let (mut child, metered) = start(false);
        assert_eq!(
            super::wait_claimed(&mut child, metered)
                .expect("kept")
                .code(),
            Some(3)
        );
        let (child, metered) = start(true);
        let out = super::output_waited(child, metered).expect("kept");
        assert_eq!(
            (out.status.code(), &out.stdout[..]),
            (Some(3), &b"out\n"[..])
        );
        let kept = ended();
        assert!(kept.len() == 2 && kept.iter().all(exit_3), "{kept:?}");

        let (mut child, metered) = start(false);
        lose(&child);
        let waited = super::wait_claimed(&mut child, metered).expect_err("lost");
        assert_eq!(waited.raw_os_error(), Some(libc::ECHILD), "{waited}");
        let (child, metered) = start(true);
        lose(&child);
        let waited = super::output_waited(child, metered).expect_err("lost");
        assert_eq!(waited.raw_os_error(), Some(libc::ECHILD), "{waited}");
        assert_eq!(ended(), [BuildEnd::Unknown, BuildEnd::Unknown]);
    }

    /// A lab process that holds a claim of its own (a lane's VM) builds unclaimed and says so: a
    /// second claim would wait on its own. NEGATIVE CONTROL: once it lets go, the build claims.
    #[test]
    fn a_lab_holding_a_claim_of_its_own_builds_unclaimed() {
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let m = Fixed::new(Reading::of_gib(24, 20, 14));
        let me = std::process::id();
        let vm = aterm_capacity::admit_with(
            &Ledger::at(dir.path().join("ledger")),
            &m,
            aterm_capacity::Need::new("lab vm boot").mem(4 * GIB),
            Admit::Wait,
            Pace::default(),
            &mut |_| {},
        )
        .expect("the VM's claim");
        let s = seat(dir.path(), &m, me);
        let args = ["--unverified", "build", "-p", "aterm-lab", "--bin", "x"];
        let mut said = Vec::new();
        let got = super::claim_build_at(&args, None, dir.path(), &s, &mut |l| {
            said.push(l.to_owned());
        });
        assert!(matches!(got, Ok(None)), "{got:?}");
        assert!(said.iter().any(|l| l.contains("lab vm boot")), "{said:?}");
        drop(vm);
        let got = super::claim_build_at(&args, None, dir.path(), &s, &mut |_| {});
        assert!(matches!(got, Ok(Some(_))), "{got:?}");
    }

    #[test]
    fn a_nested_targo_starts_at_the_checkout_without_what_cargo_said_about_the_lab() {
        // Inject the inherited names: a runner need not provide Cargo's build-script OUT_DIR.
        let names = [
            "CARGO_PKG_NAME",
            "CARGO_MANIFEST_DIR",
            "OUT_DIR",
            "PATH",
            "HOME",
        ];
        let mut cmd = super::targo(&["--unverified", "build"]);
        for name in names {
            cmd.env(name, "inherited");
        }
        super::clear_cargo_run_env(&mut cmd, names.map(std::ffi::OsString::from));
        let removed: Vec<String> = cmd
            .get_envs()
            .filter(|(_, v)| v.is_none())
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        for name in ["CARGO_PKG_NAME", "CARGO_MANIFEST_DIR", "OUT_DIR"] {
            assert!(removed.iter().any(|r| r == name), "{name} in {removed:?}");
        }
        assert!(
            !removed.iter().any(|r| r == "PATH" || r == "HOME"),
            "{removed:?}"
        );
        assert_eq!(
            cmd.get_current_dir(),
            Some(crate::paths::paths().repo.as_path())
        );
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let verb = args
            .iter()
            .position(|a| a == "build")
            .expect("build command");
        assert!(args[..verb].iter().any(|a| a == "--unverified"), "{args:?}");
    }

    #[test]
    fn archived_scratch_files_keep_times_but_refuse_other_names() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};
        use std::path::PathBuf;

        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let d = Scratch(
            std::fs::canonicalize(crate::sys::make_temp_dir("/tmp/lab-archive.XXXXXX").unwrap())
                .unwrap(),
        );
        let input = d.0.join("scratch");
        let output = d.0.join("archive");
        std::fs::create_dir_all(input.join("nested")).unwrap();
        let normal = input.join("nested/normal");
        std::fs::write(&normal, "keep this log").unwrap();
        std::fs::set_permissions(&normal, std::fs::Permissions::from_mode(0o640)).unwrap();
        let modified = UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        std::fs::File::options()
            .write(true)
            .open(&normal)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        let outside = d.0.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("private"), "do not archive").unwrap();
        std::fs::hard_link(outside.join("private"), input.join("hard")).unwrap();
        symlink(outside.join("private"), input.join("linked")).unwrap();
        symlink(&outside, input.join("linked-dir")).unwrap();

        keep_tree(&input, &output);
        let copied = output.join("nested/normal");
        assert_eq!(std::fs::read_to_string(&copied).unwrap(), "keep this log");
        let meta = std::fs::metadata(&copied).unwrap();
        assert_eq!(meta.modified().unwrap(), modified);
        assert_eq!(meta.permissions().mode() & 0o777, 0o640);
        assert!(
            !output.join("hard").exists(),
            "a second name is not a lab log"
        );
        assert!(!output.join("linked").exists());
        assert!(!output.join("linked-dir").exists());

        keep_file(
            &input.join("linked-dir/private"),
            &output.join("via-parent"),
        );
        assert!(
            !output.join("via-parent").exists(),
            "no link anywhere on the source path"
        );
        assert_eq!(
            std::fs::read_to_string(outside.join("private")).unwrap(),
            "do not archive"
        );
    }

    #[test]
    fn utc_stamps_are_the_calendar_day() {
        assert_eq!(super::utc_stamp(0), "1970-01-01T00:00:00.000000Z");
        assert_eq!(
            super::utc_stamp(1_792_091_823),
            "2026-10-15T19:17:03.000000Z"
        );
        assert_eq!(super::utc_stamp(951_782_400), "2000-02-29T00:00:00.000000Z");
    }

    #[test]
    fn base64url_round_trips_without_padding() {
        for s in [
            "",
            "f",
            "fo",
            "foo",
            "foob",
            "fooba",
            "foobar",
            "{\"lab\":\"A\"}",
        ] {
            let e = super::b64url(s.as_bytes());
            assert!(!e.contains('='), "{e}");
            assert_eq!(
                super::b64url_decode(&e).as_deref(),
                Some(s.as_bytes()),
                "{s}"
            );
        }
        assert_eq!(super::b64url(b"\xfb\xff"), "-_8");
        assert_eq!(super::b64url_decode("Zg==").as_deref(), Some(&b"f"[..]));
        assert_eq!(super::b64url_decode("Zm8=").as_deref(), Some(&b"fo"[..]));
        for bad in [
            "not base64!",
            "A",
            "ABCDE",
            "Zh",
            "Zm9",
            "Zg=",
            "Zm8==",
            "===",
        ] {
            assert_eq!(super::b64url_decode(bad), None, "{bad:?}");
        }
    }

    use super::*;

    #[test]
    fn one_line_folds_control_runs_and_cuts_long_text() {
        assert_eq!(one_line("  a\n\n\tb  ", 70), "a b");
        assert_eq!(one_line("abcdef", 4), "abc~");
        assert_eq!(one_line("abcd", 4), "abcd");
    }

    #[test]
    fn shell_words_round_trip_through_quote_and_unquote() {
        for s in [
            "plain",
            "with space",
            "it's",
            "/Users//x/Library/Application Support/a b",
            "",
        ] {
            assert_eq!(shell_unquote(&shell_quote(s)), s, "{s:?}");
        }
        assert_eq!(
            shell_unquote(r"/a/Application\ Support/b"),
            "/a/Application Support/b"
        );
        assert_eq!(shell_unquote(r"$'a\nb'"), "a\nb");
        assert_eq!(shell_unquote(r"$'a\x20\x09\x7fb'"), "a \t\x7fb");
        assert_eq!(
            shell_prepend_path("/s/runs/t/shim"),
            "\"/s/runs/t/shim:$PATH\""
        );
        assert_eq!(
            shell_prepend_path(r#"/s/"a"$b`c`\d/shim"#),
            r#""/s/\"a\"\$b\`c\`\\d/shim:$PATH""#
        );
        assert_eq!(
            shell_prepend_path("/s/a\nb/shim"),
            "'/s/a\nb/shim':\"$PATH\""
        );
    }

    #[test]
    fn py_repr_picks_the_quote_python_picks() {
        assert_eq!(py_repr("hi"), "'hi'");
        assert_eq!(py_repr("it's"), "\"it's\"");
        assert_eq!(py_repr("a\nb"), "'a\\nb'");
        assert_eq!(py_repr("❯\u{a0}hi\u{80}\u{2028}"), "'❯\\xa0hi\\x80\\u2028'");
    }

    /// An executable past the lab's ordinary limit reads whole, as Claude Code's and a debug
    /// aterm are. NEGATIVE CONTROL: the ordinary read refuses that file, and a file past the
    /// program limit is refused before a byte is read.
    #[test]
    fn read_program_takes_what_the_ordinary_limit_refuses() {
        let dir = std::fs::canonicalize(
            crate::sys::make_temp_dir("/tmp/lab-program-read.XXXXXX").unwrap(),
        )
        .unwrap();
        let file = dir.join("program");
        let sparse = |len| {
            std::fs::File::create(&file).unwrap().set_len(len).unwrap();
        };
        sparse(MAX_LAB_FILE_BYTES + 1);
        assert!(read_regular_file(&file).is_err());
        assert_eq!(
            read_program(&file).unwrap().len() as u64,
            MAX_LAB_FILE_BYTES + 1
        );
        sparse(MAX_LAB_PROGRAM_BYTES + 1);
        assert!(read_program(&file).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn read_regular_file_and_text_refuse_non_regular_entries() {
        let dir = std::fs::canonicalize(
            crate::sys::make_temp_dir("/tmp/lab-regular-read.XXXXXX").unwrap(),
        )
        .unwrap();
        let file = dir.join("ok.txt");
        std::fs::write(&file, "hello\n").unwrap();
        assert_eq!(read_regular_text(&file).unwrap(), "hello\n");
        assert_eq!(read_regular_file(&file).unwrap(), b"hello\n");
        assert_eq!(read_artifact(&file).unwrap(), b"hello\n");
        let linked = dir.join("linked.txt");
        std::os::unix::fs::symlink(&file, &linked).unwrap();
        assert_eq!(read_regular_text(&linked).unwrap(), "hello\n");
        assert!(read_artifact(&linked).is_err());
        let err = read_regular_file(&dir).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        let fifo = dir.join("fifo");
        assert!(
            aterm_uds::spawnfd::command("/usr/bin/mkfifo")
                .arg(&fifo)
                .status()
                .unwrap()
                .success()
        );
        let err = read_regular_file(&fifo).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn line_tail_cursor_reads_deltas_holds_partial_lines_and_resets_on_replace() {
        use std::io::Write as _;
        let dir = std::fs::canonicalize(
            crate::sys::make_temp_dir("/tmp/lab-tail-cursor.XXXXXX").unwrap(),
        )
        .unwrap();
        let file = dir.join("journal.jsonl");
        let mut cursor = LineTailCursor::default();
        assert_eq!(
            cursor.poll_scratch_text(&file).unwrap(),
            (false, String::new())
        );

        std::fs::write(&file, "line-1\npartial").unwrap();
        assert_eq!(
            cursor.poll_scratch_text(&file).unwrap(),
            (true, "line-1\n".to_string())
        );
        // Idle poll with only the un-terminated trailing line reads 0 complete lines.
        assert_eq!(
            cursor.poll_scratch_text(&file).unwrap(),
            (false, String::new())
        );

        // Completing the partial line + adding a second line yields both without re-reading line-1.
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&file)
            .unwrap();
        f.write_all(b"-2\nline-3\n").unwrap();
        drop(f);
        assert_eq!(
            cursor.poll_scratch_text(&file).unwrap(),
            (false, "partial-2\nline-3\n".to_string())
        );
        assert_eq!(
            cursor.poll_scratch_text(&file).unwrap(),
            (false, String::new())
        );

        // Atomic replacement (new inode / shorter length) triggers reset = true.
        atomic_write(&file, b"replaced-1\n").unwrap();
        assert_eq!(
            cursor.poll_scratch_text(&file).unwrap(),
            (true, "replaced-1\n".to_string())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
