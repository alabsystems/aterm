// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! On-disk acaffeine state (`<state dir>/acaffeine.state`): `key=value` lines
//! (`mode=off|forever|until`, `ends=<secs>`, `span=<secs>`, `deadline=<secs>`, `boot=<uuid>`).
//!
//! A timed mode ends at `ends` on the boot clock ([`Clocks::since_boot`]), so only a file of this
//! boot is read against it. `deadline` is that end on the wall clock as the save read it, which a
//! file of an earlier boot (or one with no `ends`, written by hand) is read against; the mug saves
//! it again once the date changes ([`refresh`]). A file read on the wall clock is read against it
//! now, never as more than its span away, until the mug that takes it saves it back
//! ([`take_saving`]). Missing or unparseable state restores to [`Mode::Forever`]; any other mode
//! is the one saved, in any boot: an off (a person's: a click, Quit, `aterm acaffeine off` or
//! `quit`, or the end of a span they picked) lasts until it is turned on again.
//!
//! Keep Awake with Lid Closed is a file of its own beside it (`<state dir>/acaffeine.lid`,
//! `lid=on|off`, [`load_lid`]): the person's standing choice, of no boot and no deadline, which
//! no save of the mode reads or writes, so neither can undo the other's change. A missing file
//! (every install from before the choice existed) or any other text reads off.

use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::model::{Clocks, Mode};

/// Maximum state file byte size.
pub const READ_CAP: u64 = 4096;

/// Persisted acaffeine state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Saved {
    /// The mode.
    pub mode: Mode,
    /// Boot UUID (`kern.bootsessionuuid`, [`crate::instance::boot_id`]), if known.
    pub boot: Option<u128>,
}

/// Formats `boot` as an upper-case UUID string matching `kern.bootsessionuuid`
/// (`astream_sys::boot_id_text`, the inverse of [`parse_boot`]'s reader).
#[must_use]
pub fn boot_text(boot: u128) -> String {
    astream_sys::boot_id_text(boot.to_be_bytes(), false)
}

/// Parses a non-zero boot UUID string via `astream_sys::parse_boot_id`.
#[must_use]
pub fn parse_boot(text: &str) -> Option<u128> {
    astream_sys::parse_boot_id(text.as_bytes()).map(u128::from_be_bytes)
}

/// Both clocks, read now: the boot clock (`aterm_uds::process::continuous_ms`) and the wall
/// clock; `None` when the boot clock cannot be read or the wall clock reads before 1970.
#[must_use]
pub fn clocks() -> Option<Clocks> {
    Some(Clocks {
        since_boot: aterm_uds::process::continuous_ms()? / 1000,
        wall: aterm_time::unix_secs()?,
    })
}

/// Encodes `saved` as `key=value` lines, a timed mode's `deadline` (its end on the wall clock)
/// as `now` reads it.
#[must_use]
pub fn encode(saved: &Saved, now: Clocks) -> String {
    let mut text = match saved.mode {
        Mode::Off => "mode=off\n".to_string(),
        Mode::Forever => "mode=forever\n".to_string(),
        Mode::Until { deadline, span } => format!(
            "mode=until\nends={deadline}\nspan={span}\ndeadline={}\n",
            now.wall_at(deadline)
        ),
    };
    if let Some(boot) = saved.boot {
        text.push_str(&format!("boot={}\n", boot_text(boot)));
    }
    text
}

/// Decodes state text written by [`encode`]; a text with no `ends` (written by hand) has its
/// wall-clock deadline read at `now`, never as more than its span away.
#[must_use]
pub fn decode(text: &str, now: Clocks) -> Option<Saved> {
    parse(text, now, |_| true).map(|(saved, _)| saved)
}

/// The keys of a state file's text, each as the last line naming it says.
struct Fields<'a> {
    mode: &'a str,
    ends: Option<u64>,
    deadline: Option<u64>,
    span: Option<u64>,
    boot: Option<u128>,
}

/// The [`Fields`] of `text`; `None` for a line that is not `key=value`, a number or a boot that
/// does not parse, or no `mode`.
fn fields(text: &str) -> Option<Fields<'_>> {
    let (mut mode, mut ends, mut deadline, mut span, mut boot) = (None, None, None, None, None);
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line.split_once('=')?;
        let number = || {
            (!value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()))
                .then(|| value.parse::<u64>().ok())
                .flatten()
        };
        match key {
            "mode" => mode = Some(value),
            "ends" => ends = Some(number()?),
            "deadline" => deadline = Some(number()?),
            "span" => span = Some(number()?),
            "boot" => boot = Some(parse_boot(value)?),
            _ => {}
        }
    }
    Some(Fields {
        mode: mode?,
        ends,
        deadline,
        span,
        boot,
    })
}

/// [`decode`], reading `ends` only where `ends_counts` says the file's boot clock is the one
/// `now` reads (an `ends` of another boot counts on a clock that started again), and whether the
/// text keeps a timed mode on the wall clock alone.
fn parse(
    text: &str,
    now: Clocks,
    ends_counts: impl FnOnce(Option<u128>) -> bool,
) -> Option<(Saved, bool)> {
    let Fields {
        mode,
        ends,
        deadline,
        span,
        boot,
    } = fields(text)?;
    let ends = ends.filter(|_| ends_counts(boot));
    let mode = match mode {
        "off" => Mode::Off,
        "forever" => Mode::Forever,
        "until" => {
            let span = span?;
            let deadline = match (ends, deadline) {
                (Some(ends), _) => ends,
                (None, Some(wall)) => now
                    .since_boot
                    .saturating_add(wall.saturating_sub(now.wall).min(span)),
                (None, None) => return None,
            };
            Mode::Until { deadline, span }
        }
        _ => return None,
    };
    let wall_only = matches!(mode, Mode::Until { .. }) && ends.is_none();
    Some((Saved { mode, boot }, wall_only))
}

/// The slack, in seconds, before [`refresh`] saves a file whose wall-clock `deadline` has moved
/// off its end, and before [`take`] answers a deadline past its span as one to save back: more
/// than whole-second readings of the two clocks, or of one clock in two processes, wander apart.
pub const DRIFT_SECS: u64 = 5;

/// Whether `text` holds `mode`, a timed mode of this boot (`boot`), saved before a change of the
/// date: its `ends`, `span` and `boot` are the mode's, and its `deadline` is missing or more than
/// [`DRIFT_SECS`] from where the wall clock at `now` puts that end.
fn drifted(text: &str, mode: Mode, boot: Option<u128>, now: Clocks) -> bool {
    let Mode::Until {
        deadline: ends,
        span,
    } = mode
    else {
        return false;
    };
    fields(text).is_some_and(|fields| {
        boot.is_some()
            && fields.boot == boot
            && fields.mode == "until"
            && fields.ends == Some(ends)
            && fields.span == Some(span)
            && fields
                .deadline
                .is_none_or(|wall| wall.abs_diff(now.wall_at(ends)) > DRIFT_SECS)
    })
}

/// Restores `saved` at `now`: missing or unreadable state is forever, and any other mode is the
/// one saved, settled at `now`, in this boot or an earlier one: an off lasts, and a span that has
/// ended is off.
#[inline]
#[must_use]
pub const fn restore(saved: Option<Saved>, now: Clocks) -> Mode {
    match saved {
        Some(Saved { mode, .. }) => mode.settled(now),
        None => Mode::Forever,
    }
}

/// Whether the mode saved in `path` is on at `now`, as written ([`read`]): not as a restore
/// answers it ([`load`]), which is forever for a file that is missing or cannot be read. A mug or
/// a verb that has just saved off asks this to tell a writer's on that came after its save.
#[must_use]
pub fn saved_on(path: &Path, now: Clocks) -> bool {
    read(path, now).is_some_and(|saved| saved.mode.is_on(now))
}

/// Reads and decodes at `now` the regular state file at `path` (up to [`READ_CAP`] bytes,
/// `O_NOFOLLOW | O_NONBLOCK` on Unix).
#[must_use]
pub fn read(path: &Path, now: Clocks) -> Option<Saved> {
    decode(&read_text(path)?, now)
}

/// The text of the regular state file at `path`, as [`read`] reads it.
fn read_text(path: &Path) -> Option<String> {
    use std::io::Read as _;
    if !crate::paths::parent_is_real_dir(path) {
        return None;
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let file = options.open(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > READ_CAP {
        return None;
    }
    let mut text =
        String::with_capacity(usize::try_from(meta.len().saturating_add(1)).unwrap_or(0));
    file.take(READ_CAP + 1).read_to_string(&mut text).ok()?;
    if text.len() as u64 > READ_CAP {
        return None;
    }
    Some(text)
}

/// How long a save waits for another save's [`Held`] on its directory before it renames without
/// it; a hold lasts a re-read and a rename.
const HOLD_WAIT: Duration = Duration::from_secs(1);

/// How often a save waiting for a [`Held`] tries it again.
const HOLD_POLL: Duration = Duration::from_millis(1);

/// A save's `flock` on the state file's directory (whose inode no rename of the file replaces),
/// held from the flush over a [`save_over`]'s re-read and the rename, so no other save that takes
/// it renames between the two; dropping it unlocks. A verb's change of mode holds one on another
/// directory ([`hold_dir`]).
pub(crate) struct Held(std::fs::File);

impl Drop for Held {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// The [`Held`] on `dir`, waited for up to `wait` while another description holds it; `None`
/// once `wait` is up, or where the directory cannot be opened or locked (Windows' `File::open`
/// opens no directory).
fn hold(dir: &Path, wait: Duration) -> Option<Held> {
    hold_with(dir, wait, || std::thread::sleep(HOLD_POLL))
}

/// [`hold`], with `pause` run between tries.
fn hold_with(dir: &Path, wait: Duration, pause: impl FnMut()) -> Option<Held> {
    hold_dir(dir, wait, pause).ok().flatten()
}

/// The [`Held`] on `dir`, waited for up to `wait` while another description holds it, with
/// `pause` run between tries: `None` once `wait` is up. A verb's change of mode holds one on its
/// own directory ([`crate::paths::Paths::session_dir`]).
///
/// # Errors
///
/// The directory cannot be opened (Windows' `File::open` opens none) or locked.
pub(crate) fn hold_dir(
    dir: &Path,
    wait: Duration,
    mut pause: impl FnMut(),
) -> io::Result<Option<Held>> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(
            libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_DIRECTORY | libc::O_CLOEXEC,
        );
    }
    let file = aterm_uds::kept_out(|| options.open(dir))?;
    let until = Instant::now().checked_add(wait);
    loop {
        #[expect(
            clippy::disallowed_methods,
            reason = "the guard's own take: its Drop releases by LOCK_UN"
        )]
        let taken = file.try_lock();
        match taken {
            Ok(()) => return Ok(Some(Held(file))),
            Err(std::fs::TryLockError::WouldBlock) if until.is_none_or(|u| Instant::now() < u) => {
                pause();
            }
            Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
            Err(std::fs::TryLockError::Error(e)) => return Err(e),
        }
    }
}

/// Atomically writes `saved`, encoded at `now`, to `path` via a sibling temporary file, renamed
/// over it under the directory's [`Held`], or without it once [`HOLD_WAIT`] is up.
///
/// # Errors
///
/// The parent directory cannot be created, or the write or the rename fails.
pub fn write(path: &Path, saved: &Saved, now: Clocks) -> io::Result<()> {
    write_unless(path, saved, now, HOLD_WAIT, || {}, || false).map(drop)
}

/// [`write()`], unless `changed`, asked once the new text is flushed and the directory's
/// [`Held`] (waited for up to `wait`) is taken, just before the rename, says the file is no
/// longer the one to replace: whether it was written. `landing` runs between the flush and the
/// hold.
fn write_unless(
    path: &Path,
    saved: &Saved,
    now: Clocks,
    wait: Duration,
    landing: impl FnOnce(),
    changed: impl FnOnce() -> bool,
) -> io::Result<bool> {
    use std::io::Write as _;
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("the state file has no directory"))?;
    if !parent.as_os_str().is_empty() {
        crate::paths::ensure_real_dir(parent)?;
    }
    #[cfg(unix)]
    let (tmp, mut file) = aterm_uds::atomic_file::create_temp_beside(path, 0o600)?;
    #[cfg(not(unix))]
    let (tmp, mut file) = {
        let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
        tmp_name.push(format!(".{}.tmp", std::process::id()));
        let tmp = parent.join(tmp_name);
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.mode(0o600)
                .custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS);
        }
        let file = opts.open(&tmp)?;
        (tmp, file)
    };
    let written = (|| {
        #[cfg(unix)]
        file.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
        file.write_all(encode(saved, now).as_bytes())?;
        aterm_durable::ordered(&file)?;
        landing();
        let _held = hold(parent, wait);
        if !parent.as_os_str().is_empty() && !crate::paths::existing_real_dir(parent)? {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("{} does not exist", parent.display()),
            ));
        }
        if changed() {
            return Ok(false);
        }
        std::fs::rename(&tmp, path).map(|()| true)
    })();
    if !matches!(written, Ok(true)) {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// The text of Keep Awake with Lid Closed `on`.
#[must_use]
pub fn encode_lid(on: bool) -> String {
    format!("lid={}\n", if on { "on" } else { "off" })
}

/// Keep Awake with Lid Closed as `text` says it: on only when the last `lid=` line says `on`;
/// any other text, a file of an older build's format among them, is off.
#[must_use]
pub fn decode_lid(text: &str) -> bool {
    text.lines()
        .filter_map(|line| line.trim().strip_prefix("lid="))
        .next_back()
        .is_some_and(|value| value == "on")
}

/// Keep Awake with Lid Closed as saved in `path` (read as [`read`] reads the mode file): off for
/// a file that is missing or cannot be read.
#[must_use]
pub fn load_lid(path: &Path) -> bool {
    read_text(path).is_some_and(|text| decode_lid(&text))
}

/// Save Keep Awake with Lid Closed `on` to `path`, whole, through a sibling temporary.
///
/// # Errors
///
/// The parent directory cannot be made or has gone, or the write or the rename fails.
pub fn save_lid(path: &Path, on: bool) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("the lid file has no directory"))?;
    if !parent.as_os_str().is_empty() {
        crate::paths::ensure_real_dir(parent)?;
    }
    #[cfg(unix)]
    {
        aterm_uds::atomic_file::write_checked(path, encode_lid(on).as_bytes(), 0o600, || {
            if parent.as_os_str().is_empty() || crate::paths::existing_real_dir(parent)? {
                Ok(())
            } else {
                Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("{} does not exist", parent.display()),
                ))
            }
        })
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, encode_lid(on))
    }
}

/// Remove the temporaries that no write will rename beside the mode, Keep Awake with Lid Closed
/// and the update bridge's pid file, under this name and the old one ([`crate::legacy`]), and beside the mug's trouble record
/// ([`crate::trouble`]), with
/// [`aterm_uds::atomic_file::sweep_temps_beside`] past
/// [`aterm_uds::atomic_file::ABANDONED_AFTER`]; the job plists' are
/// [`crate::launchd::sweep_plist_temps`]'s. A process that ends while a thread of its own is
/// saving leaves one: 0.145.0 started aCaffeine for every window argument it then refused, and
/// left one in most runs of an old `aterm caffeine` login job. Each file, with how many of its
/// temporaries went, or its sweep's first error once the rest were tried, after which some may
/// have gone.
#[cfg(unix)]
#[must_use]
pub fn sweep_temps(paths: &crate::paths::Paths) -> Vec<(std::path::PathBuf, io::Result<usize>)> {
    let files = [
        paths.state_file(),
        paths.lid_file(),
        paths.bridge_file(),
        paths.trouble_file(),
        crate::legacy::state_file(paths),
        crate::legacy::bridge_file(paths),
    ];
    match crate::paths::existing_real_dir(&paths.state_dir) {
        Ok(true) => files
            .into_iter()
            .map(|file| {
                let after = aterm_uds::atomic_file::ABANDONED_AFTER;
                let swept = aterm_uds::atomic_file::sweep_temps_beside(&file, after);
                (file, swept)
            })
            .collect(),
        Ok(false) => files.into_iter().map(|file| (file, Ok(0))).collect(),
        Err(e) => {
            let (kind, msg) = (e.kind(), e.to_string());
            files
                .into_iter()
                .map(|file| (file, Err(io::Error::new(kind, msg.clone()))))
                .collect()
        }
    }
}

/// Saves `mode` to `path`, stamped with this boot ([`crate::instance::boot_id`]) and encoded at
/// the [`clocks`] as they read now.
///
/// # Errors
///
/// As [`write()`].
pub fn save(path: &Path, mode: Mode) -> io::Result<()> {
    save_at(path, mode, clocks().unwrap_or_default())
}

/// [`save`], encoded at `now`.
///
/// # Errors
///
/// As [`write()`].
pub fn save_at(path: &Path, mode: Mode, now: Clocks) -> io::Result<()> {
    write(path, &this_boots(mode), now)
}

/// `mode`, stamped with this boot.
fn this_boots(mode: Mode) -> Saved {
    Saved {
        mode,
        boot: crate::instance::boot_id(),
    }
}

/// [`save_at`], unless the file at `path` no longer holds `was` once the new text is flushed:
/// whether it was saved, so a writer that changed the file since keeps its change. The re-read
/// and the rename run under the directory's [`Held`], so no other save lands between them unless
/// its [`HOLD_WAIT`] ran out; a build that takes no hold can still land there (a window of about
/// 0.1 ms on APFS) and be overwritten.
///
/// # Errors
///
/// As [`write()`].
pub fn save_over(path: &Path, mode: Mode, was: &str, now: Clocks) -> io::Result<bool> {
    save_over_with(path, mode, was, now, || {})
}

/// [`save_over`], with `landing` run where a writer's change would land unseen but for the read.
fn save_over_with(
    path: &Path,
    mode: Mode,
    was: &str,
    now: Clocks,
    landing: impl FnOnce(),
) -> io::Result<bool> {
    write_unless(path, &this_boots(mode), now, HOLD_WAIT, landing, || {
        read_text(path).as_deref() != Some(was)
    })
}

/// Loads and restores the mode at `now` from `path`.
#[must_use]
pub fn load(path: &Path, now: Clocks) -> Mode {
    take(path, now).0
}

/// [`load`], and the text the mode was restored from when the file is one to save back because
/// every read of it restores it anew: a timed mode on the wall clock alone (no `ends`, or an
/// `ends` of an earlier boot), or a mode of this boot the restore changed (a deadline gone, or
/// more than its span and [`DRIFT_SECS`] away). A deadline past its span by no more than [`DRIFT_SECS`] (a writer's fresh span read at
/// clocks a moment behind its own) is brought in only in the mode answered, and the file is left
/// as written: that writer posts its notification after its save, and the take it brings reads
/// the span as written.
#[must_use]
pub fn take(path: &Path, now: Clocks) -> (Mode, Option<String>) {
    let Some(text) = read_text(path) else {
        return (Mode::Forever, None);
    };
    let boot = crate::instance::boot_id();
    let parsed = parse(&text, now, |saved| boot.is_some() && saved == boot);
    let mode = restore(parsed.map(|(saved, _)| saved), now);
    let resave = parsed.is_some_and(|(saved, wall_only)| {
        wall_only
            || (boot.is_some()
                && saved.boot == boot
                && saved.mode != mode
                && !brought_in_by_drift(saved.mode, mode))
    });
    (mode, resave.then_some(text))
}

/// Whether `restored` is the timed mode `saved` brought in to its span by no more than
/// [`DRIFT_SECS`].
fn brought_in_by_drift(saved: Mode, restored: Mode) -> bool {
    matches!(
        (saved, restored),
        (Mode::Until { deadline: was, .. }, Mode::Until { deadline, .. })
            if was.abs_diff(deadline) <= DRIFT_SECS
    )
}

/// How many times [`take_saving`] reads the file: each read after the first is of a writer's
/// change that landed before the save-back of the one before.
const TAKE_ROUNDS: usize = 3;

/// [`take`], saving the mode back when the file is one to ([`save_over`]). A file a writer
/// changed before the save-back is taken in turn, up to [`TAKE_ROUNDS`] reads; the mode of the
/// last is answered unsaved when its writer changed the file once more, and the notification
/// that writer posts brings the next take.
///
/// # Errors
///
/// As [`write()`].
pub fn take_saving(path: &Path, now: Clocks) -> io::Result<Mode> {
    take_saving_with(path, now, |_| {})
}

/// [`take_saving`], with `landing` run at each save-back as [`save_over_with`] runs it.
fn take_saving_with(path: &Path, now: Clocks, mut landing: impl FnMut(&Path)) -> io::Result<Mode> {
    let mut mode = Mode::Off;
    for _ in 0..TAKE_ROUNDS {
        let (taken, was) = take(path, now);
        mode = taken;
        let Some(was) = was else {
            break;
        };
        if save_over_with(path, mode, &was, now, || landing(path))? {
            break;
        }
    }
    Ok(mode)
}

/// Saves `mode` again, encoded at `now`, when the file at `path` holds it as a timed mode saved
/// before a change of the date ([`drifted`]), unless a writer changes it first ([`save_over`]):
/// whether it was saved. This keeps `deadline` true for a read after a restart, which reads the
/// end on the wall clock alone. The file is read only for a timed mode.
///
/// # Errors
///
/// As [`write()`].
pub fn refresh(path: &Path, mode: Mode, now: Clocks) -> io::Result<bool> {
    refresh_with(path, mode, now, read_text, || {})
}

/// [`refresh`], reading the file with `read`, with `landing` run at the save as
/// [`save_over_with`] runs it.
fn refresh_with(
    path: &Path,
    mode: Mode,
    now: Clocks,
    read: impl FnOnce(&Path) -> Option<String>,
    landing: impl FnOnce(),
) -> io::Result<bool> {
    if !matches!(mode, Mode::Until { .. }) {
        return Ok(false);
    }
    match read(path) {
        Some(text) if drifted(&text, mode, crate::instance::boot_id(), now) => {
            save_over_with(path, mode, &text, now, landing)
        }
        _ => Ok(false),
    }
}

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
