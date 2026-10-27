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
            "a person provisions this builder (docs/RUNBOOK-atpkg-republish.md)",
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
            "a person runs: agh policy check",
            at_secs,
        )
    }
}

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
/// key written passes `shapes` unchanged. Raising a key again replaces it. Off unix:
/// `Ok(())`, nothing written.
///
/// # Errors
///
/// InvalidInput for a key outside the pattern, or one `Redactor::shapes` would change;
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
        let dir = dirs.needs();
        crate::config::make_state_subdir(dirs, &dir)?;
        let mut object = Map::new();
        object.insert("at_secs".into(), Value::from(need.at_secs));
        object.insert("key".into(), Value::from(need.key.as_str()));
        object.insert("then".into(), Value::from(Redactor::shapes(&need.then)));
        object.insert("v".into(), Value::from(1u64));
        object.insert("what".into(), Value::from(Redactor::shapes(&need.what)));
        object.insert("why".into(), Value::from(Redactor::shapes(&need.why)));
        let text = aterm_json::to_string(&Value::Object(object))
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        aterm_uds::private_file::write_private(&dir, &file_name(&need.key), text.as_bytes())
    }
    #[cfg(not(unix))]
    {
        let _ = (dirs, need);
        Ok(())
    }
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

    /// A key `Redactor::shapes` would change is refused, so every need `raise` writes is one
    /// `list` returns: the file's stem and its JSON key agree.
    #[test]
    fn a_credential_shaped_key_is_refused_and_every_written_need_lists() {
        let root = aterm_tempfile::Builder::new()
            .prefix("agh-needs-")
            .tempdir()
            .expect("a scratch directory");
        let dirs = Dirs::under(root.path());

        let shaped = format!("agh.{}", agh_fake::token("7"));
        let refused = raise(&dirs, &Need::new(&shaped, "w", "y", "t", 1))
            .expect_err("a credential-shaped key is refused");
        assert_eq!(refused.kind(), std::io::ErrorKind::InvalidInput);
        assert!(!refused.to_string().contains(&shaped));

        raise(
            &dirs,
            &Need::new("agh.intent.3-0123456789ab.stale", "w", "y", "t", 1),
        )
        .expect("a plain key is written");
        let keys: Vec<String> = list(&dirs)
            .expect("the needs list")
            .into_iter()
            .map(|need| need.key)
            .collect();
        assert_eq!(keys, ["agh.intent.3-0123456789ab.stale"]);
    }
}
