// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! On-disk caffeine state (`<state dir>/caffeine.state`), encoded as `key=value`
//! lines (`mode=off|forever|until`, `deadline=<s?>`, `span=<s?>`, `boot=<uuid>`).
//! A mode saved in an earlier boot or an unparseable file restores to [`Mode::Off`].

use std::io;
use std::path::Path;

use crate::model::Mode;

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

/// Encodes `saved` as `key=value` lines.
#[must_use]
pub fn encode(saved: &Saved) -> String {
    let mut text = match saved.mode {
        Mode::Off => "mode=off\n".to_string(),
        Mode::Forever => "mode=forever\n".to_string(),
        Mode::Until { deadline, span } => format!("mode=until\ndeadline={deadline}\nspan={span}\n"),
    };
    if let Some(boot) = saved.boot {
        text.push_str(&format!("boot={}\n", boot_text(boot)));
    }
    text
}

/// Decodes state text written by [`encode`].
#[must_use]
pub fn decode(text: &str) -> Option<Saved> {
    let (mut mode, mut deadline, mut span, mut boot) = (None, None, None, None);
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line.split_once('=')?;
        let number = || value.parse::<u64>().ok();
        match key {
            "mode" => mode = Some(value),
            "deadline" => deadline = Some(number()?),
            "span" => span = Some(number()?),
            "boot" => boot = Some(parse_boot(value)?),
            _ => {}
        }
    }
    let mode = match mode? {
        "off" => Mode::Off,
        "forever" => Mode::Forever,
        "until" => Mode::Until {
            deadline: deadline?,
            span: span?,
        },
        _ => return None,
    };
    Some(Saved { mode, boot })
}

/// Restores `saved` at `now` if its boot matches `boot_now` and its deadline has not passed.
#[inline]
#[must_use]
pub const fn restore(saved: Option<Saved>, boot_now: Option<u128>, now: u64) -> Mode {
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

/// Reads and decodes the regular state file at `path` (up to [`READ_CAP`] bytes, `O_NOFOLLOW | O_NONBLOCK` on Unix).
#[must_use]
pub fn read(path: &Path) -> Option<Saved> {
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
    decode(&text)
}

/// Atomically writes `saved` to `path` via a sibling temporary file.
///
/// # Errors
///
/// The parent directory cannot be created, or the write or the rename fails.
pub fn write(path: &Path, saved: &Saved) -> io::Result<()> {
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
        file.write_all(encode(saved).as_bytes())?;
        aterm_durable::ordered(&file)?;
        std::fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// Saves `mode` to `path`, stamped with this boot ([`crate::instance::boot_id`]).
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
    )
}

/// Loads and restores the mode at `now` from `path`.
#[must_use]
pub fn load(path: &Path, now: u64) -> Mode {
    restore(read(path), crate::instance::boot_id(), now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::HOUR;

    const NOW: u64 = 1_790_000_000;
    const BOOT: u128 = 0x4D5A_1B2C_3E4F_5061_7283_94A5_B6C7_D8E9;

    #[test]
    fn every_mode_round_trips() {
        for mode in [
            Mode::Off,
            Mode::Forever,
            Mode::Until {
                deadline: NOW + HOUR,
                span: HOUR,
            },
        ] {
            for boot in [None, Some(BOOT)] {
                let saved = Saved { mode, boot };
                assert_eq!(decode(&encode(&saved)), Some(saved), "{saved:?}");
            }
        }
    }

    #[test]
    fn the_text_is_the_documented_lines() {
        let saved = Saved {
            mode: Mode::Until {
                deadline: 1_790_003_600,
                span: 3600,
            },
            boot: Some(BOOT),
        };
        assert_eq!(
            encode(&saved),
            "mode=until\ndeadline=1790003600\nspan=3600\nboot=4D5A1B2C-3E4F-5061-7283-94A5B6C7D8E9\n"
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
        assert_eq!(decode("mode=forever\nboot=1789990000\n"), None);
        let zero = format!("mode=forever\nboot={}\n", boot_text(0));
        assert_eq!(decode(&zero), None);
        assert_eq!(restore(decode(&zero), Some(0), NOW), Mode::Off);
        let this = format!("mode=forever\nboot={}\n", boot_text(BOOT));
        assert_eq!(
            restore(decode(&this), Some(BOOT), NOW),
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
            "mode=forever\nboot=-1\n",
            "garbage\n",
            "mode\n",
        ] {
            assert_eq!(decode(text), None, "{text:?}");
        }
        assert_eq!(
            decode("# note\n\nmode=forever\nflavour=mocha\n"),
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
                deadline: NOW + 10,
                span: HOUR,
            },
            boot: Some(BOOT),
        };
        assert_eq!(restore(Some(timed), Some(BOOT), NOW), timed.mode);
        assert_eq!(restore(Some(timed), Some(BOOT), NOW + 10), Mode::Off);
    }

    #[test]
    fn write_then_read_and_an_oversized_file_reads_as_none() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested").join("caffeine.state");
        assert_eq!(read(&path), None);
        let saved = Saved {
            mode: Mode::Forever,
            boot: Some(BOOT),
        };
        write(&path, &saved).expect("write");
        assert_eq!(read(&path), Some(saved));
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
        assert_eq!(read(&path), None);
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
                write(&path, &saved).expect("write past an unowned old stage");
                assert_eq!(read(&path), Some(saved));
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
            assert_eq!(read(&alias), None);
            assert_eq!(read(path.parent().expect("parent")), None);
            let fifo = dir.path().join("state-fifo");
            let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).expect("no NUL");
            // SAFETY: the name is an exclusively owned scratch path; the call creates a FIFO.
            assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0, "mkfifo");
            assert_eq!(read(&fifo), None);
            assert_eq!(read(&path), Some(saved), "the ordinary state still reads");
        }
    }
}
