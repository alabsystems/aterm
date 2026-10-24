// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! On-disk caffeine state (`<state dir>/caffeine.state`): `key=value` lines
//! (`mode=off|forever|until`, `ends=<secs>`, `span=<secs>`, `deadline=<secs>`, `boot=<uuid>`).
//!
//! A timed mode ends at `ends` on the boot clock ([`Clocks::since_boot`]), so only a file of this
//! boot is read against it. `deadline` is that end on the wall clock as the save read it, for
//! builds that predate `ends`; the mug saves it again once the date changes ([`refresh`]). A file
//! with no `ends` is read against the wall clock now, never as more than its span away, until the
//! mug that takes it saves it back ([`take_saving`]). A mode saved in an earlier boot, or an
//! unparseable file, restores to [`Mode::Off`].

use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::model::{Clocks, Mode};

/// Maximum state file byte size.
pub const READ_CAP: u64 = 4096;

/// Persisted caffeine state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Saved {
    /// The mode.
    pub mode: Mode,
    /// Boot UUID (`kern.bootsessionuuid`, [`crate::instance::boot_id`]), if known.
    pub boot: Option<u128>,
}

/// Formats `boot` as an upper-case UUID string matching `kern.bootsessionuuid`.
#[must_use]
pub fn boot_text(boot: u128) -> String {
    let hex = format!("{boot:032X}");
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
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

/// Decodes state text written by [`encode`]; a text with no `ends` (an older build's) has its
/// wall-clock deadline read at `now`, never as more than its span away.
#[must_use]
pub fn decode(text: &str, now: Clocks) -> Option<Saved> {
    parse(text, now).map(|(saved, _)| saved)
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
        let number = || value.parse::<u64>().ok();
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

/// [`decode`], and whether the text keeps a timed mode on the wall clock alone (no `ends`).
fn parse(text: &str, now: Clocks) -> Option<(Saved, bool)> {
    let Fields {
        mode,
        ends,
        deadline,
        span,
        boot,
    } = fields(text)?;
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

/// Restores `saved` at `now` if its boot matches `boot_now` and its deadline has not passed.
#[inline]
#[must_use]
pub const fn restore(saved: Option<Saved>, boot_now: Option<u128>, now: Clocks) -> Mode {
    match (saved, boot_now) {
        (
            Some(Saved {
                mode,
                boot: Some(then),
            }),
            Some(now_boot),
        ) if now_boot == then => mode.settled(now),
        _ => Mode::Off,
    }
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
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut text = String::new();
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
/// it renames between the two; dropping it unlocks.
struct Held(std::fs::File);

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
fn hold_with(dir: &Path, wait: Duration, mut pause: impl FnMut()) -> Option<Held> {
    let file = std::fs::File::open(dir).ok()?;
    let until = Instant::now() + wait;
    loop {
        #[expect(
            clippy::disallowed_methods,
            reason = "the guard's own take: its Drop releases by LOCK_UN"
        )]
        let taken = file.try_lock();
        match taken {
            Ok(()) => return Some(Held(file)),
            Err(std::fs::TryLockError::WouldBlock) if Instant::now() < until => pause(),
            Err(_) => return None,
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
    std::fs::create_dir_all(parent)?;
    #[cfg(unix)]
    let (tmp, mut file) = aterm_uds::atomic_file::create_temp_beside(path, 0o600)?;
    #[cfg(not(unix))]
    let (tmp, mut file) = {
        let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
        tmp_name.push(format!(".{}.tmp", std::process::id()));
        let tmp = parent.join(tmp_name);
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        (tmp, file)
    };
    let written = (|| {
        #[cfg(unix)]
        file.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
        file.write_all(encode(saved, now).as_bytes())?;
        aterm_durable::ordered(&file)?;
        landing();
        let _held = hold(parent, wait);
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
/// every read of it restores it anew: a timed mode on the wall clock alone (no `ends`), or a mode
/// of this boot the restore changed (a deadline gone, or more than its span and [`DRIFT_SECS`]
/// away). A deadline past its span by no more than [`DRIFT_SECS`] (a writer's fresh span read at
/// clocks a moment behind its own) is brought in only in the mode answered, and the file is left
/// as written: that writer posts its notification after its save, and the take it brings reads
/// the span as written.
#[must_use]
pub fn take(path: &Path, now: Clocks) -> (Mode, Option<String>) {
    let Some(text) = read_text(path) else {
        return (Mode::Off, None);
    };
    let parsed = parse(&text, now);
    let boot = crate::instance::boot_id();
    let mode = restore(parsed.map(|(saved, _)| saved), boot, now);
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
/// whether it was saved. This keeps `deadline` true for a build that reads the end on the wall
/// clock alone. The file is read only for a timed mode.
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
mod tests {
    use super::*;
    use crate::model::HOUR;

    const NOW: Clocks = Clocks {
        since_boot: 300_000,
        wall: 1_790_000_000,
    };
    const DAY: u64 = 24 * HOUR;
    const BOOT: u128 = 0x4D5A_1B2C_3E4F_5061_7283_94A5_B6C7_D8E9;

    #[test]
    fn every_mode_round_trips() {
        for mode in [
            Mode::Off,
            Mode::Forever,
            Mode::Until {
                deadline: NOW.since_boot + HOUR,
                span: HOUR,
            },
        ] {
            for boot in [None, Some(BOOT)] {
                let saved = Saved { mode, boot };
                assert_eq!(decode(&encode(&saved, NOW), NOW), Some(saved), "{saved:?}");
            }
        }
    }

    #[test]
    fn the_text_is_the_documented_lines() {
        let saved = Saved {
            mode: Mode::Until {
                deadline: 303_000,
                span: 3600,
            },
            boot: Some(BOOT),
        };
        assert_eq!(
            encode(&saved, NOW),
            "mode=until\nends=303000\nspan=3600\ndeadline=1790003000\nboot=4D5A1B2C-3E4F-5061-7283-94A5B6C7D8E9\n"
        );
    }

    #[test]
    fn a_boot_is_a_uuid_and_nothing_else() {
        assert_eq!(boot_text(BOOT), "4D5A1B2C-3E4F-5061-7283-94A5B6C7D8E9");
        assert_eq!(boot_text(1), "00000000-0000-0000-0000-000000000001");
        for boot in [1, BOOT, u128::MAX] {
            assert_eq!(parse_boot(&boot_text(boot)), Some(boot), "{boot:x}");
        }
        assert_eq!(
            parse_boot("4d5a1b2c-3e4f-5061-7283-94a5b6c7d8e9"),
            Some(BOOT),
            "either case"
        );
        for bad in [
            "",
            "1789990000",
            "4D5A1B2C-3E4F-5061-7283-94A5B6C7D8E",
            "4D5A1B2C-3E4F-5061-7283-94A5B6C7D8E9A",
            "+D5A1B2C-3E4F-5061-7283-94A5B6C7D8E9",
            "4D5A1B2C-3E4F-5061-7283-94A5B6C7D8EG",
            "00000000-0000-0000-0000-000000000000",
        ] {
            assert_eq!(parse_boot(bad), None, "{bad:?}");
        }
        assert_eq!(decode("mode=forever\nboot=1789990000\n", NOW), None);
        let zero = format!("mode=forever\nboot={}\n", boot_text(0));
        assert_eq!(decode(&zero, NOW), None);
        assert_eq!(restore(decode(&zero, NOW), Some(0), NOW), Mode::Off);
        let this = format!("mode=forever\nboot={}\n", boot_text(BOOT));
        assert_eq!(
            restore(decode(&this, NOW), Some(BOOT), NOW),
            Mode::Forever,
            "CONTROL"
        );
    }

    #[test]
    fn a_file_this_crate_did_not_write_is_none() {
        for text in [
            "",
            "mode=on\n",
            "mode=until\nspan=3600\n",
            "mode=until\ndeadline=x\nspan=3600\n",
            "mode=until\nends=x\nspan=3600\n",
            "mode=until\nends=303000\n",
            "mode=forever\nboot=-1\n",
            "garbage\n",
            "mode\n",
        ] {
            assert_eq!(decode(text, NOW), None, "{text:?}");
        }
        assert_eq!(
            decode("# note\n\nmode=forever\nflavour=mocha\n", NOW),
            Some(Saved {
                mode: Mode::Forever,
                boot: None
            })
        );
    }

    #[test]
    fn a_restart_of_the_mac_or_a_passed_deadline_starts_off() {
        let forever = Saved {
            mode: Mode::Forever,
            boot: Some(BOOT),
        };
        assert_eq!(restore(Some(forever), Some(BOOT), NOW), Mode::Forever);
        assert_eq!(restore(Some(forever), Some(BOOT ^ 1), NOW), Mode::Off);
        assert_eq!(restore(Some(forever), None, NOW), Mode::Off);
        assert_eq!(
            restore(
                Some(Saved {
                    boot: None,
                    ..forever
                }),
                Some(BOOT),
                NOW
            ),
            Mode::Off
        );
        assert_eq!(restore(None, Some(BOOT), NOW), Mode::Off);
        let timed = Saved {
            mode: Mode::Until {
                deadline: NOW.since_boot + 10,
                span: HOUR,
            },
            boot: Some(BOOT),
        };
        assert_eq!(restore(Some(timed), Some(BOOT), NOW), timed.mode);
        assert_eq!(restore(Some(timed), Some(BOOT), NOW + 10), Mode::Off);
    }

    /// A saved span keeps its time through the wall clock set back or forward a day, and the file
    /// keeps its end on the wall clock for builds that read only that.
    #[test]
    fn a_saved_span_keeps_its_time_through_a_change_of_the_date() {
        let saved = Saved {
            mode: Mode::chosen(crate::model::Choice::Hours1, NOW),
            boot: Some(BOOT),
        };
        let text = encode(&saved, NOW);
        assert!(text.contains("\ndeadline=1790003600\n"), "{text}");
        for wall in [NOW.wall - DAY + 600, NOW.wall + 600, NOW.wall + DAY + 600] {
            let later = Clocks {
                since_boot: NOW.since_boot + 600,
                wall,
            };
            let mode = restore(decode(&text, later), Some(BOOT), later);
            assert_eq!(mode.remaining(later), Some(HOUR - 600), "{wall}");
        }
    }

    /// A file with no `ends` is read against the wall clock now, never as more than its span away,
    /// and from then on runs on the boot clock.
    #[test]
    fn an_earlier_builds_file_restores_bounded_by_its_span() {
        let earlier = |deadline: u64| {
            format!(
                "mode=until\ndeadline={deadline}\nspan=3600\nboot={}\n",
                boot_text(BOOT)
            )
        };
        let read = |text: &str| decode(text, NOW).map(|saved| saved.mode);
        let left = |text: &str| restore(decode(text, NOW), Some(BOOT), NOW).remaining(NOW);
        let ten_minutes_in = earlier(NOW.wall + HOUR - 600);
        assert_eq!(left(&ten_minutes_in), Some(HOUR - 600));
        let set_back = earlier(NOW.wall + DAY + HOUR - 600);
        assert_eq!(
            read(&set_back),
            Some(Mode::chosen(crate::model::Choice::Hours1, NOW)),
            "a clock set back a day after the save"
        );
        assert_eq!(left(&set_back), Some(HOUR));
        assert_eq!(left(&earlier(NOW.wall - 1)), None, "a deadline gone");
        let mode = restore(decode(&ten_minutes_in, NOW), Some(BOOT), NOW);
        let back = Clocks {
            since_boot: NOW.since_boot + 60,
            wall: NOW.wall - DAY + 60,
        };
        assert_eq!(mode.remaining(back), Some(HOUR - 660));
        let saved = encode(
            &Saved {
                mode,
                boot: Some(BOOT),
            },
            NOW,
        );
        assert!(saved.contains(&format!("\nends={}\n", NOW.since_boot + HOUR - 600)));
    }

    /// A file with no `ends` is one to save back, whatever its boot: re-read after the date is set
    /// back it would read a full span, while the mode saved reads the time it had.
    #[test]
    fn a_file_with_no_ends_is_one_to_save_back() {
        let earlier = |deadline: u64| {
            format!(
                "mode=until\ndeadline={deadline}\nspan={}\nboot={}\n",
                8 * HOUR,
                boot_text(BOOT)
            )
        };
        let wall_only = |text: &str, at: Clocks| parse(text, at).map(|(_, wall_only)| wall_only);
        let left = |text: &str, at: Clocks| restore(decode(text, at), Some(BOOT), at).remaining(at);
        let back = Clocks {
            since_boot: NOW.since_boot + 600,
            wall: NOW.wall - DAY + 600,
        };
        for (deadline, left_now) in [(NOW.wall + HOUR, Some(HOUR)), (NOW.wall - 1, None)] {
            let text = earlier(deadline);
            assert_eq!(wall_only(&text, NOW), Some(true), "{deadline}");
            assert_eq!(left(&text, NOW), left_now, "{deadline}");
            assert_eq!(left(&text, back), Some(8 * HOUR), "{deadline}: read anew");
            let saved = encode(
                &Saved {
                    mode: restore(decode(&text, NOW), Some(BOOT), NOW),
                    boot: Some(BOOT),
                },
                NOW,
            );
            assert_eq!(wall_only(&saved, back), Some(false), "{deadline}");
            assert_eq!(
                left(&saved, back),
                left_now.map(|left| left - 600),
                "{deadline}"
            );
        }
        for text in ["mode=off\n", "mode=forever\n"] {
            assert_eq!(wall_only(text, NOW), Some(false), "{text:?}");
        }
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("caffeine.state");
        assert_eq!(take(&path, NOW), (Mode::Off, None), "no file");
        std::fs::write(&path, earlier(NOW.wall + HOUR)).expect("an earlier build's file");
        assert_eq!(take(&path, NOW).1, Some(earlier(NOW.wall + HOUR)));
        write(
            &path,
            &Saved {
                mode: Mode::chosen(crate::model::Choice::Hours8, NOW),
                boot: Some(BOOT),
            },
            NOW,
        )
        .expect("write");
        assert_eq!(take(&path, NOW).1, None);
    }

    /// A file of this boot whose restore changes its mode is saved back once: a deadline a day
    /// away comes in to its hour, and one gone is off. One past its span by no more than
    /// [`DRIFT_SECS`] is brought in but left as it is.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_restore_that_changes_this_boots_mode_is_saved_back() {
        let this = crate::instance::boot_id().expect("this boot");
        let file = |ends: u64, boot: u128| {
            format!(
                "mode=until\nends={ends}\nspan={HOUR}\ndeadline={}\nboot={}\n",
                NOW.wall_at(ends),
                boot_text(boot)
            )
        };
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("caffeine.state");
        let hour = Mode::chosen(crate::model::Choice::Hours1, NOW);
        for (ends, boot, want, resaved) in [
            (NOW.since_boot + DAY, this, hour, true),
            (NOW.since_boot - 1, this, Mode::Off, true),
            (NOW.since_boot + HOUR, this, hour, false),
            (NOW.since_boot + HOUR + DRIFT_SECS, this, hour, false),
            (NOW.since_boot + HOUR + DRIFT_SECS + 1, this, hour, true),
            (NOW.since_boot + DAY, this ^ 1, Mode::Off, false),
        ] {
            let text = file(ends, boot);
            std::fs::write(&path, &text).expect("state file");
            assert_eq!(
                take(&path, NOW),
                (want, resaved.then_some(text)),
                "{ends} {boot:x}"
            );
        }
        std::fs::write(&path, file(NOW.since_boot + DAY, this)).expect("state file");
        assert_eq!(take_saving(&path, NOW).expect("saved back"), hour);
        let later = NOW + HOUR / 2;
        assert_eq!(load(&path, later).remaining(later), Some(HOUR / 2));
        assert_eq!(take(&path, later).1, None, "saved once");
    }

    /// A writer's change landing before the save-back is kept; a writer landing at every
    /// save-back ends the takes at the last of [`TAKE_ROUNDS`], unsaved.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_writers_change_landing_before_the_save_back_is_kept() {
        let this = boot_text(crate::instance::boot_id().expect("this boot"));
        let earlier = |deadline: u64| {
            format!(
                "mode=until\ndeadline={deadline}\nspan={}\nboot={this}\n",
                8 * HOUR
            )
        };
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("caffeine.state");
        std::fs::write(&path, earlier(NOW.wall + HOUR)).expect("an earlier build's file");
        let mut landed = 0;
        let taken = take_saving_with(&path, NOW, |path| {
            landed += 1;
            save_at(path, Mode::Off, NOW).expect("the writer's off");
        })
        .expect("taken");
        assert_eq!(taken, Mode::Off);
        assert_eq!(landed, 1, "the writer's file needs no save-back");
        assert_eq!(
            std::fs::read_to_string(&path).expect("state"),
            format!("mode=off\nboot={this}\n")
        );
        std::fs::write(&path, earlier(NOW.wall + HOUR)).expect("an earlier build's file");
        let mut landed = 0;
        let taken = take_saving_with(&path, NOW, |path| {
            landed += 1;
            std::fs::write(path, earlier(NOW.wall + HOUR - landed)).expect("another");
        })
        .expect("taken");
        assert_eq!(landed, TAKE_ROUNDS as u64);
        assert_eq!(taken.remaining(NOW), Some(HOUR - (landed - 1)));
        assert_eq!(
            std::fs::read_to_string(&path).expect("state"),
            earlier(NOW.wall + HOUR - landed)
        );
    }

    /// A writer's `on 1h` landing before a save-back, on clocks up to [`DRIFT_SECS`] past the
    /// take's, is left as written; further past, the take's hour is saved in.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_writers_span_taken_at_clocks_behind_its_own_is_left_as_written() {
        let this = boot_text(crate::instance::boot_id().expect("this boot"));
        let earlier = format!(
            "mode=until\ndeadline={}\nspan={}\nboot={this}\n",
            NOW.wall + HOUR,
            8 * HOUR
        );
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("caffeine.state");
        let hour = Mode::chosen(crate::model::Choice::Hours1, NOW);
        for lead in [0, 1, 2, DRIFT_SECS, DRIFT_SECS + 1] {
            std::fs::write(&path, &earlier).expect("an earlier build's file");
            let writers = NOW + lead;
            let on = Mode::chosen(crate::model::Choice::Hours1, writers);
            let (mut landed, mut text) = (0, String::new());
            let taken = take_saving_with(&path, NOW, |path| {
                landed += 1;
                if landed == 1 {
                    save_at(path, on, writers).expect("the writer's on 1h");
                    text = std::fs::read_to_string(path).expect("the writer's file");
                }
            })
            .expect("taken");
            assert_eq!(taken, hour, "{lead}");
            let after = std::fs::read_to_string(&path).expect("state");
            if lead <= DRIFT_SECS {
                assert_eq!(after, text, "{lead}");
                assert_eq!(load(&path, writers), on, "{lead}");
            } else {
                assert_eq!(after, encode(&this_boots(hour), NOW), "{lead}");
            }
        }
    }

    /// A save holds its directory from the flush over its last read and its rename, then lets go;
    /// with the hold taken elsewhere, a save whose wait is up renames without it.
    #[cfg(unix)]
    #[test]
    fn a_save_renames_under_its_directorys_hold() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("caffeine.state");
        let free = || hold(dir.path(), Duration::ZERO).is_some();
        let forever = Saved {
            mode: Mode::Forever,
            boot: Some(BOOT),
        };
        let (mut at_landing, mut at_rename) = (false, true);
        let written = write_unless(
            &path,
            &forever,
            NOW,
            HOLD_WAIT,
            || at_landing = free(),
            || {
                at_rename = free();
                false
            },
        );
        assert!(written.expect("written"));
        assert!(at_landing, "CONTROL: free before the hold");
        assert!(!at_rename, "held over the last read and the rename");
        assert!(free(), "let go once saved");
        let off = Saved {
            mode: Mode::Off,
            ..forever
        };
        let elsewhere = hold(dir.path(), Duration::ZERO).expect("free");
        let written = write_unless(&path, &off, NOW, Duration::ZERO, || {}, || false);
        assert!(written.expect("written without it"));
        drop(elsewhere);
        assert_eq!(read(&path, NOW), Some(off));
    }

    /// A hold another description has is waited for while it lasts, and no longer than the wait.
    #[cfg(unix)]
    #[test]
    fn a_hold_waits_for_its_holder_and_no_longer() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let mut elsewhere = Some(hold(dir.path(), Duration::ZERO).expect("free"));
        let none = hold_with(dir.path(), Duration::ZERO, || {
            panic!("a pause with no wait")
        });
        assert!(none.is_none(), "no wait");
        let mut pauses = 0;
        let held = hold_with(dir.path(), Duration::from_secs(60), || {
            pauses += 1;
            elsewhere = None;
        });
        assert!(held.is_some(), "taken once its holder let go");
        assert_eq!(pauses, 1);
        assert!(hold(dir.path(), Duration::ZERO).is_none(), "and held");
    }

    /// A refresh reads the file only for a timed mode: off, and on with no end, keep no end on
    /// the wall clock.
    #[test]
    fn a_refresh_reads_the_file_only_for_a_timed_mode() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("caffeine.state");
        let hour = Mode::chosen(crate::model::Choice::Hours1, NOW);
        for (mode, want) in [(Mode::Off, 0), (Mode::Forever, 0), (hour, 1)] {
            save_at(&path, mode, NOW).expect("saved");
            let mut reads = 0;
            let read = |path: &Path| {
                reads += 1;
                read_text(path)
            };
            let saved = refresh_with(&path, mode, NOW, read, || {});
            assert!(!saved.expect("refreshed"), "{mode:?}");
            assert_eq!(reads, want, "{mode:?}");
        }
    }

    /// A writer whose change lands before a refresh's save keeps it: the mode the mug holds,
    /// saved before the date was set back a day, is not saved again over the writer's off.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_writers_change_landing_before_a_refresh_is_kept() {
        let this = boot_text(crate::instance::boot_id().expect("this boot"));
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("caffeine.state");
        let mode = Mode::chosen(crate::model::Choice::Hours8, NOW);
        save_at(&path, mode, NOW).expect("the mug's save");
        let back = Clocks {
            since_boot: NOW.since_boot + 600,
            wall: NOW.wall - DAY + 600,
        };
        let mut landed = 0;
        let saved = refresh_with(&path, mode, back, read_text, || {
            landed += 1;
            save_at(&path, Mode::Off, back).expect("the writer's off");
        });
        assert!(!saved.expect("refreshed"));
        assert_eq!(
            std::fs::read_to_string(&path).expect("state"),
            format!("mode=off\nboot={this}\n")
        );
        assert_eq!(landed, 1);
    }

    /// The boot clock counts time asleep: [`clocks`] reads between two direct reads of
    /// `continuous_ms`, so it is neither the wall clock nor one that stops in sleep.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_boot_clock_counts_time_asleep() {
        let direct = || aterm_uds::process::continuous_ms().expect("the continuous clock") / 1000;
        let wall = || aterm_time::unix_secs().expect("after the epoch");
        let (before, wall_before) = (direct(), wall());
        let now = clocks().expect("both clocks");
        let (after, wall_after) = (direct(), wall());
        assert!(
            (before..=after).contains(&now.since_boot),
            "{before} {now:?} {after}"
        );
        assert!(
            (wall_before..=wall_after).contains(&now.wall),
            "{wall_before} {now:?} {wall_after}"
        );
    }

    #[test]
    fn write_then_read_and_an_oversized_file_reads_as_none() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested").join("caffeine.state");
        assert_eq!(read(&path, NOW), None);
        let saved = Saved {
            mode: Mode::Forever,
            boot: Some(BOOT),
        };
        write(&path, &saved, NOW).expect("write");
        assert_eq!(read(&path, NOW), Some(saved));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().expect("parent"))
            .expect("read_dir")
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(leftovers, [std::ffi::OsString::from("caffeine.state")]);
        std::fs::write(
            &path,
            format!("mode=forever\n#{}\n", "x".repeat(READ_CAP as usize)),
        )
        .expect("big");
        assert_eq!(read(&path, NOW), None);
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt as _;
            use std::os::unix::fs::{MetadataExt as _, symlink};

            let outside = dir.path().join("keep");
            std::fs::write(&outside, b"unrelated bytes").expect("outside");
            let old_stage =
                path.with_file_name(format!("caffeine.state.{}.tmp", std::process::id()));
            for linked in [false, true] {
                if linked {
                    std::fs::hard_link(&outside, &old_stage).expect("hardlink");
                } else {
                    symlink(&outside, &old_stage).expect("symlink");
                }
                let before = std::fs::metadata(&outside).expect("outside metadata");
                write(&path, &saved, NOW).expect("write past an unowned old stage");
                assert_eq!(read(&path, NOW), Some(saved));
                assert_eq!(
                    std::fs::read(&outside).expect("outside bytes"),
                    b"unrelated bytes"
                );
                let after = std::fs::metadata(&outside).expect("outside metadata");
                assert_eq!(
                    (after.dev(), after.ino(), after.mode(), after.len()),
                    (before.dev(), before.ino(), before.mode(), before.len())
                );
                let stage = std::fs::symlink_metadata(&old_stage).expect("old stage retained");
                assert_eq!(stage.file_type().is_symlink(), !linked);
                std::fs::remove_file(&old_stage).expect("own old stage");
            }
            let alias = dir.path().join("state-link");
            symlink(&path, &alias).expect("state link");
            assert_eq!(read(&alias, NOW), None);
            assert_eq!(read(path.parent().expect("parent"), NOW), None);
            let fifo = dir.path().join("state-fifo");
            let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).expect("no NUL");
            // SAFETY: the name is an exclusively owned scratch path; the call creates a FIFO.
            assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0, "mkfifo");
            assert_eq!(read(&fifo, NOW), None);
            assert_eq!(
                read(&path, NOW),
                Some(saved),
                "the ordinary state still reads"
            );
        }
    }
}
