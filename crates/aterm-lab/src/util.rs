// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Time formatting, string truncation, shell quoting, and safe scratch I/O.

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
    let args =
        atpkg::build_cache::with_build_dir_from(&args, atpkg::build_cache::Tool::Targo, repo)
            .unwrap_or(args);
    let mut cmd =
        aterm_uds::spawnfd::command(std::env::var_os("CARGO").unwrap_or_else(|| "targo".into()));
    cmd.args(args).current_dir(repo);
    for (name, _) in std::env::vars_os() {
        if name
            .to_str()
            .is_some_and(atpkg::build_cache::said_by_cargo_run)
        {
            cmd.env_remove(name);
        }
    }
    cmd
}

/// Atomically replaces `path` with `bytes` via [`crate::sys::put`].
///
/// # Errors
/// A link on the way, or the write or the rename failing.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    crate::sys::put(path, 0o644, &crate::sys::PutSrc::Bytes(bytes))
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

/// Copies a scratch file through a checked descriptor, preserving mode and mtime.
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
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                out.push_str(&format!("\\x{:02x}", c as u32))
            }
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
    fn a_nested_targo_starts_at_the_checkout_without_what_cargo_said_about_the_lab() {
        // CONTROL: cargo hands this test binary what it hands the lab under `targo run`
        // (`OUT_DIR` too: aterm-lab has a build script).
        for name in ["CARGO_PKG_NAME", "CARGO_MANIFEST_DIR", "OUT_DIR"] {
            assert!(std::env::var_os(name).is_some(), "{name}");
        }
        let cmd = super::targo(&["--unverified", "build"]);
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
        assert!(
            args.ends_with(&["--unverified".to_owned(), "build".to_owned()]),
            "{args:?}"
        );
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
