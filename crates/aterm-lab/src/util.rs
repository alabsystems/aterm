// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Small shared pieces: the wall clock and its local spellings, the one-line
//! cut the logs use, shell quoting for the scripts the lab writes, and the
//! atomic write every edited file goes through.

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
    // Truncation is the rule here, as the logs always wrote it.
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

/// A string on one line: every run of control characters is one space, the
/// ends trimmed, and a string longer than `n` characters cut to `n - 1` and `~`.
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

/// `s` as one POSIX shell word: bare when it is only safe characters, else in
/// single quotes (a quote inside spelled `'\''`).
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

/// The words of one shell assignment line as the lab writes `env.sh` (and as
/// `printf %q` wrote it before): single and double quotes, backslash escapes
/// and `$'…'`. A line that is not one word yields what it reads.
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

/// Replace the absolute `path` with `bytes` by a rename, through no link on
/// its way ([`crate::sys::put`]): what the lab writes into a directory jailed
/// code may write lands where it names, never where a planted link points.
///
/// # Errors
/// A link on the way, or the write or the rename failing.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    crate::sys::put(path, 0o644, &crate::sys::PutSrc::Bytes(bytes))
}

/// The text of a file jailed code may write, read through no link on its whole
/// path and only when it is this user's regular file with one name
/// ([`crate::sys::scratch_read`]); empty when there is none or it is refused.
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

/// Copy a scratch file through the checked descriptor, keeping its mode and
/// modification time. Links anywhere on its path, second names and other
/// users' files are refused by the same reader as live scratch inputs.
pub fn keep_file(from: &Path, to: &Path) {
    use std::os::unix::fs::PermissionsExt as _;

    let Ok(Some(mut source)) = crate::sys::scratch_file(from) else {
        return;
    };
    let Ok(meta) = source.metadata() else {
        return;
    };
    let mut bytes = Vec::new();
    if io::Read::read_to_end(&mut source, &mut bytes).is_err() {
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

/// Copy a directory tree with its files' times, as `cp -Rp` does, but no
/// link: jailed code can plant one anywhere in a tree the lab keeps (to
/// `~/.codex`, say), so a link is never followed and never kept (aterm and
/// the agents make none there), the tree's own root included.
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

/// Every regular file under `dir`, into `out`: each directory is listed
/// through no link on its whole path ([`crate::sys::scratch_dir`]), so a link
/// planted anywhere in a scratch tree jailed code writes — or swapped in
/// while it is walked — is never followed; each file is then read through
/// [`scratch_text`].
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

/// Python's `repr` of a string (the stub log's `%r`): single quotes unless the
/// text holds one and no double quote.
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
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                out.push_str(&format!("\\x{:02x}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push(q);
    out
}

/// `YYYY-MM-DDTHH:MM:SS.000000Z`: a unix second in UTC, as Codex's `auth.json` stamps `last_refresh`.
#[must_use]
pub fn utc_stamp(secs: i64) -> String {
    format!("{}.000000Z", utc_civil(secs))
}

/// `YYYY-MM-DDTHH:MM:SS.000Z`: a unix second in UTC, as the claude.ai usage body spells `resets_at`.
#[must_use]
pub fn utc_iso(secs: i64) -> String {
    format!("{}.000Z", utc_civil(secs))
}

/// `YYYY-MM-DDTHH:MM:SS` of a unix second in UTC.
fn utc_civil(secs: i64) -> String {
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // civil-from-days (Howard Hinnant's algorithm), the proleptic Gregorian calendar
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
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in text.trim_end_matches('=').bytes() {
        let v = u32::try_from(B64URL.iter().position(|&b| b == c)?).ok()?;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((acc >> bits) & 0xff).ok()?);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
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
        assert_eq!(super::b64url_decode("not base64!"), None);
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
        // what `printf %q` wrote before the port
        assert_eq!(
            shell_unquote(r"/a/Application\ Support/b"),
            "/a/Application Support/b"
        );
        assert_eq!(shell_unquote(r"$'a\nb'"), "a\nb");
    }

    #[test]
    fn py_repr_picks_the_quote_python_picks() {
        assert_eq!(py_repr("hi"), "'hi'");
        assert_eq!(py_repr("it's"), "\"it's\"");
        assert_eq!(py_repr("a\nb"), "'a\\nb'");
    }
}
