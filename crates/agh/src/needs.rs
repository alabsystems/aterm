// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Facts for a person (design §10.2, §10.3): one file per need, `<state>/agh/needs/<key>.json`,
//! which aterm-gui shows as an exceptions entry (`Family::Agh`). agh links no
//! aterm-messages, so it writes this small format of its own.

use crate::config::Dirs;

/// One need.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Need {
    /// `[a-z0-9._-]{1,96}`, starting `agh.`: the file's stem and the fact's identity.
    pub key: String,
    /// What needs a look, in one line.
    pub what: String,
    /// Why a person and not agh.
    pub why: String,
    /// The one action.
    pub then: String,
    /// `Fx::wall_secs` when raised.
    pub at_secs: u64,
}

impl Need {
    /// The need `key` with its texts, raised at `at_secs`. `Need` is `#[non_exhaustive]`, so a
    /// caller outside agh builds one here or through the two below.
    #[must_use]
    pub fn new(key: &str, what: &str, why: &str, then: &str, at_secs: u64) -> Need {
        Need {
            key: key.to_string(),
            what: what.to_string(),
            why: why.to_string(),
            then: then.to_string(),
            at_secs,
        }
    }

    /// `agh.config.missing`, with its texts.
    #[must_use]
    pub fn config_missing(at_secs: u64) -> Need {
        Need::new(
            "agh.config.missing",
            "agh.toml is missing on this builder",
            "the lanes route their GitHub identities through it, and a missing file is never \
             read as no identities",
            CONFIG_PROVISION,
            at_secs,
        )
    }

    /// `agh.config.invalid`, its `why` the first fault as `{rule} {what}`.
    #[must_use]
    pub fn config_invalid(first_fault: &str, at_secs: u64) -> Need {
        Need::new(
            "agh.config.invalid",
            "agh.toml is invalid on this builder",
            first_fault,
            "run: agh policy check",
            at_secs,
        )
    }
}

/// The step that provisions a missing agh.toml the lanes can use, from a builder's token
/// files: `agh.config.missing`'s `then`, and the step `atpkg-publish hosts check` names. A
/// profile named `dev` or `release` starts a file that routes alone.
pub const CONFIG_PROVISION: &str = "agh auth add dev|release --file PATH --tier TIER --owner \
                                    OWNER --write for each lane writer, then agh policy grant \
                                    atpkg-publish REPO (docs/RUNBOOK-atpkg-republish.md)";

/// The longest file `list` reads: 8 KiB.
pub const NEED_MAX: usize = 8 << 10;

/// Whether `key` is a need's key: 1 to 96 bytes of `[a-z0-9._-]`, starting `agh.`.
#[cfg(unix)]
fn key_ok(key: &str) -> bool {
    key.len() <= 96
        && key.starts_with("agh.")
        && key.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
}

/// Cut `text` to its first half, on a character boundary, ending in `…`. `false`, and `text`
/// left as it was, when it is too short for a cut to shorten it.
#[cfg(unix)]
fn halve(text: &mut String) -> bool {
    if text.len() <= 8 {
        return false;
    }
    let mut end = text.len() / 2;
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    text.truncate(end);
    text.push('…');
    true
}

/// The file a need's key is kept in.
#[cfg(unix)]
fn file_name(key: &str) -> String {
    format!("{key}.json")
}

/// Write `need` as `{"at_secs":N,"key":"…","then":"…","v":1,"what":"…","why":"…"}` (sorted
/// keys), whole and 0600, through `aterm_uds::private_file::write_private`, in
/// `dirs.needs()`, made with its parents by `config::make_state_subdir(dirs, &dirs.needs())`,
/// so a need is written on a state directory nothing has made yet. `what`, `why` and `then`
/// pass `Redactor::shapes`. The key names the file and is what `list` matches it by, so it is
/// refused rather than redacted: one that `shapes` would change is InvalidInput, and every
/// key written passes `shapes` unchanged. Raising a key again replaces it. The file is never
/// over `NEED_MAX`, the most `list` (and aterm-gui) reads: while it would be, the longest of
/// the redacted `what`, `why` and `then` is cut in half on a character boundary and ends in
/// `…`, so a raise that answers `Ok` posts a need `list` returns. Off unix: `Ok(())`, nothing
/// written.
///
/// # Errors
///
/// InvalidInput for a key outside the pattern, or one `Redactor::shapes` would change, or
/// (which no key of the pattern reaches) a need that no cut fits in `NEED_MAX`;
/// `make_state_subdir`'s error; the write's error.
pub fn raise(dirs: &Dirs, need: &Need) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use aterm_json::{Map, Value};

        use crate::redact::Redactor;

        if !key_ok(&need.key) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "{} is not a need's key: [a-z0-9._-]{{1,96}}, starting agh.",
                    Redactor::shapes(&need.key)
                ),
            ));
        }
        // The key names the file and is what `list` and `clear` find it by, so it is refused,
        // never redacted: a key `shapes` would change is written by no one.
        if Redactor::shapes(&need.key) != need.key {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "a need's key is shaped like a credential; it names the file and is never \
                 redacted",
            ));
        }
        // Redacted before any cut, so a cut never leaves part of a token the Redactor would
        // no longer see; each build redacts again, so a cut's own text is judged too.
        let mut texts = [
            Redactor::shapes(&need.what),
            Redactor::shapes(&need.why),
            Redactor::shapes(&need.then),
        ];
        let text = loop {
            let [what, why, then] = &texts;
            let mut object = Map::new();
            object.insert("at_secs".into(), Value::from(need.at_secs));
            object.insert("key".into(), Value::from(need.key.as_str()));
            object.insert("then".into(), Value::from(Redactor::shapes(then)));
            object.insert("v".into(), Value::from(1u64));
            object.insert("what".into(), Value::from(Redactor::shapes(what)));
            object.insert("why".into(), Value::from(Redactor::shapes(why)));
            let text = aterm_json::to_string(&Value::Object(object))
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
            if text.len() <= NEED_MAX {
                break text;
            }
            let longest = texts.iter_mut().max_by_key(|t| t.len());
            if !longest.is_some_and(halve) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!(
                        "a need's file would be {} bytes; agh lists needs of at most 8 KiB",
                        text.len()
                    ),
                ));
            }
        };
        let dir = dirs.needs();
        crate::config::make_state_subdir(dirs, &dir)?;
        aterm_uds::private_file::write_private(&dir, &file_name(&need.key), text.as_bytes())
    }
    #[cfg(not(unix))]
    {
        let _ = (dirs, need);
        Ok(())
    }
}

/// Whether the need `key` has a file now (one `symlink_metadata` of `<needs>/<key>.json`),
/// so a caller that raises the same fact on every pass writes it once. A key outside the
/// pattern has none. The journal's stale need is its one caller, and unix-only as it is.
#[cfg(unix)]
#[must_use]
pub(crate) fn raised(dirs: &Dirs, key: &str) -> bool {
    key_ok(key) && std::fs::symlink_metadata(dirs.needs().join(file_name(key))).is_ok()
}

/// Remove the need `key`. `Ok(false)` when there was none. Off unix: `Ok(false)`.
///
/// # Errors
///
/// The removal's error.
pub fn clear(dirs: &Dirs, key: &str) -> std::io::Result<bool> {
    #[cfg(unix)]
    {
        if !key_ok(key) {
            return Ok(false);
        }
        aterm_uds::private_file::remove_private(&dirs.needs(), &file_name(key))
    }
    #[cfg(not(unix))]
    {
        let _ = (dirs, key);
        Ok(false)
    }
}

/// Every need, by key. A file that is not a v1 need, or is over `NEED_MAX`, is skipped. A
/// missing directory is an empty list. Off unix: empty.
///
/// # Errors
///
/// The directory's listing error.
pub fn list(dirs: &Dirs) -> std::io::Result<Vec<Need>> {
    #[cfg(unix)]
    {
        let dir = dirs.needs();
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut needs = Vec::new();
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let Some(stem) = name.strip_suffix(".json") else {
                continue;
            };
            if !key_ok(stem) {
                continue;
            }
            if let Some(need) = read_need(&dir, name).filter(|need| need.key == stem) {
                needs.push(need);
            }
        }
        needs.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(needs)
    }
    #[cfg(not(unix))]
    {
        let _ = dirs;
        Ok(Vec::new())
    }
}

/// The v1 need in `dir/name`, or `None` for a file that is not one, is over `NEED_MAX`, or
/// does not pass `read_private`.
#[cfg(unix)]
fn read_need(dir: &std::path::Path, name: &str) -> Option<Need> {
    use aterm_json::Value;
    let bytes = aterm_uds::private_file::read_private(dir, name, NEED_MAX).ok()?;
    let value: Value = aterm_json::from_slice(&bytes).ok()?;
    if value.get("v").and_then(Value::as_u64) != Some(1) {
        return None;
    }
    let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_string);
    Some(Need {
        key: text("key")?,
        what: text("what")?,
        why: text("why")?,
        then: text("then")?,
        at_secs: value.get("at_secs").and_then(Value::as_u64)?,
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A scratch root.
    fn scratch() -> aterm_tempfile::TempDir {
        aterm_tempfile::Builder::new()
            .prefix("agh-needs-")
            .tempdir()
            .expect("a scratch directory")
    }

    /// The mode bits of `path`.
    fn mode(path: &std::path::Path) -> u32 {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::metadata(path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777
    }

    #[test]
    fn needs_are_written_whole_and_listed() {
        let root = scratch();
        let dirs = Dirs::under(root.path());
        let b = Need::new("agh.b", "what b", "why b", "then b", 2);
        let a = Need::new("agh.a", "what a", "why a", "then a", 1);
        raise(&dirs, &b).expect("b");
        raise(&dirs, &a).expect("a");
        assert_eq!(list(&dirs).expect("list"), [a.clone(), b.clone()]);

        assert_eq!(mode(&dirs.needs()), 0o700);
        for (name, text) in [
            (
                "agh.a.json",
                r#"{"at_secs":1,"key":"agh.a","then":"then a","v":1,"what":"what a","why":"why a"}"#,
            ),
            (
                "agh.b.json",
                r#"{"at_secs":2,"key":"agh.b","then":"then b","v":1,"what":"what b","why":"why b"}"#,
            ),
        ] {
            let file = dirs.needs().join(name);
            assert_eq!(mode(&file), 0o600, "{name}");
            assert_eq!(std::fs::read_to_string(&file).expect("the need"), text);
        }

        let again = Need::new("agh.a", "what a2", "why a2", "then a2", 3);
        raise(&dirs, &again).expect("again");
        assert_eq!(list(&dirs).expect("list"), [again, b.clone()]);

        aterm_uds::private_file::write_private(&dirs.needs(), "agh.v2.json", br#"{"v":2}"#)
            .expect("a v2 file");
        let long = format!(
            r#"{{"at_secs":1,"key":"agh.long","then":"t","v":1,"what":"{}","why":"y"}}"#,
            "w".repeat(NEED_MAX)
        );
        aterm_uds::private_file::write_private(&dirs.needs(), "agh.long.json", long.as_bytes())
            .expect("a long file");
        assert_eq!(list(&dirs).expect("list").len(), 2);

        assert!(raised(&dirs, "agh.a"));
        assert!(clear(&dirs, "agh.a").expect("clear"));
        assert!(!raised(&dirs, "agh.a"));
        assert!(!clear(&dirs, "agh.a").expect("clear again"));
        assert!(!raised(&dirs, "agh.UPPER"));

        for key in ["needs.x", "agh.UPPER", "agh. space"] {
            let e = raise(&dirs, &Need::new(key, "w", "y", "t", 1)).expect_err(key);
            assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput, "{key}");
        }
        let shaped = format!("agh.{}", agh_fake::token("7"));
        let e = raise(&dirs, &Need::new(&shaped, "w", "y", "t", 1)).expect_err("shaped");
        assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput);
        assert!(!e.to_string().contains(&shaped));
        assert!(!dirs.needs().join(format!("{shaped}.json")).exists());

        let token = agh_fake::token("7");
        let leaky = Need::new("agh.leak", &format!("saw {token}"), "y", "t", 1);
        raise(&dirs, &leaky).expect("leak");
        let text = std::fs::read_to_string(dirs.needs().join("agh.leak.json")).expect("the need");
        assert!(
            text.contains("saw [redacted]") && !text.contains(&token),
            "{text}"
        );
    }

    #[test]
    fn an_over_long_need_is_cut_to_a_file_list_reads() {
        let root = scratch();
        let dirs = Dirs::under(root.path());
        let token = agh_fake::token("7");
        // Plain bytes, two-byte characters, and control bytes JSON writes as six each.
        let what = "w".repeat(9000);
        let why = format!("{token} {}", "é".repeat(5000));
        let then = "\u{1}".repeat(3000);
        let need = Need::new("agh.long", &what, &why, &then, 4);
        raise(&dirs, &need).expect("raised");
        let file = dirs.needs().join("agh.long.json");
        let len = std::fs::read(&file).expect("the need").len();
        assert!(len <= NEED_MAX, "{len}");
        let listed = list(&dirs).expect("list");
        let [got] = listed.as_slice() else {
            panic!("one need: {listed:?}");
        };
        assert_eq!((got.key.as_str(), got.at_secs), ("agh.long", 4));
        for (cut, whole) in [(&got.what, &what), (&got.why, &why), (&got.then, &then)] {
            let head = cut.strip_suffix('…').unwrap_or(cut);
            assert!(!head.is_empty() && head.len() < whole.len(), "{cut:?}");
        }
        assert!(got.what.starts_with('w'));
        assert!(!got.why.contains(&token) && got.why.starts_with("[redacted]"));
        assert!(raised(&dirs, "agh.long"));
    }

    #[test]
    fn a_need_is_raised_into_a_fresh_state_directory() {
        let root = scratch();
        let dirs = Dirs::under(&root.path().join("fresh"));
        assert!(!dirs.state.exists());
        assert!(dirs.state.parent().is_some_and(|p| !p.exists()));
        let need = Need::new("agh.fresh", "w", "y", "t", 1);
        raise(&dirs, &need).expect("raised");
        assert_eq!(list(&dirs).expect("list"), [need]);
        assert_eq!(mode(&dirs.state), 0o700);
        assert_eq!(mode(&dirs.needs()), 0o700);
        assert_eq!(mode(&dirs.needs().join("agh.fresh.json")), 0o600);
    }
}
