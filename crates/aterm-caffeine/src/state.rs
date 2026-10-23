// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! On-disk caffeine state (`<state dir>/caffeine.state`), encoded as `key=value`
//! lines (`mode=off|forever|until`, `ends=<s?>`, `span=<s?>`, `deadline=<s?>`, `boot=<uuid>`).
//! A timed mode ends at `ends` on the boot clock ([`Clocks::since_boot`]), which only a file of
//! this boot is read against. `deadline` is that end on the wall clock as the save read it:
//! builds before `ends` keep their deadline there, so they read a timed mode this build saves,
//! and a file one of them wrote (no `ends`) is read against the wall clock as it reads now,
//! never as more than its span away, until the mug that takes its mode saves it back with its
//! `ends` ([`take`]). A mode saved in an earlier boot or an unparseable file restores to
//! [`Mode::Off`].

use std::io;
use std::path::Path;

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

/// Decodes state text written by [`encode`] or by a build before `ends`, whose wall-clock
/// deadline is read at `now`, never as more than its span away.
#[must_use]
pub fn decode(text: &str, now: Clocks) -> Option<Saved> {
    parse(text, now).map(|(saved, _)| saved)
}

/// [`decode`], and whether the text keeps a timed mode on the wall clock alone (no `ends`).
fn parse(text: &str, now: Clocks) -> Option<(Saved, bool)> {
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
    let mode = match mode? {
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

/// Reads and decodes at `now` the regular state file at `path` (up to [`READ_CAP`] bytes, `O_NOFOLLOW | O_NONBLOCK` on Unix).
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

/// Atomically writes `saved`, encoded at `now`, to `path` via a sibling temporary file.
///
/// # Errors
///
/// The parent directory cannot be created, or the write or the rename fails.
pub fn write(path: &Path, saved: &Saved, now: Clocks) -> io::Result<()> {
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
        std::fs::rename(&tmp, path)
    })();
    if written.is_err() {
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
    write(
        path,
        &Saved {
            mode,
            boot: crate::instance::boot_id(),
        },
        clocks().unwrap_or_default(),
    )
}

/// Loads and restores the mode at `now` from `path`.
#[must_use]
pub fn load(path: &Path, now: Clocks) -> Mode {
    take(path, now).0
}

/// [`load`], and whether the file keeps a timed mode on the wall clock alone (a build before
/// `ends` wrote it): every read of such a file reads it against the wall clock anew, so one
/// after a change of the date reads a span longer or shorter than the mode restored now, until
/// the mode is saved again.
#[must_use]
pub fn take(path: &Path, now: Clocks) -> (Mode, bool) {
    let parsed = read_text(path).and_then(|text| parse(&text, now));
    let wall_only = parsed.is_some_and(|(_, wall_only)| wall_only);
    let mode = restore(
        parsed.map(|(saved, _)| saved),
        crate::instance::boot_id(),
        now,
    );
    (mode, wall_only)
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

    /// A span saved ten minutes ago, read after the wall clock was set back a day or forward a
    /// day, has the 50 minutes it had; the file says its end on the wall clock as the save read
    /// it, for a build that reads only that.
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

    /// A file a build before `ends` wrote keeps its deadline on the wall clock: it is read
    /// against the wall clock as it reads now, never as more than its span away, and from then
    /// on runs on the boot clock.
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

    /// Only a file with no `ends` is one to save back: read again once the date is set back a
    /// day, it reads a full span where the mode restored now has an hour or nothing left, while
    /// that mode saved reads the time it had.
    #[test]
    fn only_a_file_with_no_ends_is_one_to_save_back() {
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
        assert_eq!(take(&path, NOW), (Mode::Off, false), "no file");
        std::fs::write(&path, earlier(NOW.wall + HOUR)).expect("an earlier build's file");
        assert!(take(&path, NOW).1);
        write(
            &path,
            &Saved {
                mode: Mode::chosen(crate::model::Choice::Hours8, NOW),
                boot: Some(BOOT),
            },
            NOW,
        )
        .expect("write");
        assert!(!take(&path, NOW).1);
    }

    /// The boot clock is the one that counts time asleep (`mach_continuous_time`), bracketed by
    /// two direct reads of it: neither the wall clock nor one that stops in sleep.
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
