// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Lend exactly the source Git's effective object directories without consuming
//! another level of its alternate recursion budget.

use std::os::unix::ffi::OsStringExt as _;
use std::path::{Path, PathBuf};

use super::{ChildOut, Error, Result};

fn framing() -> Error {
    Error::refused("source object directories have unsupported framing")
}

/// `count-objects -v` C-quotes paths containing control bytes, quotes,
/// backslashes or non-ASCII bytes. Decode only Git's complete quoted form;
/// interpreting a quoted newline as another inventory row would widen reads.
fn directory(raw: &[u8]) -> Result<PathBuf> {
    let bytes = if let Some(quoted) = raw.strip_prefix(b"\"") {
        let quoted = quoted.strip_suffix(b"\"").ok_or_else(framing)?;
        let mut bytes = Vec::new();
        let mut at = 0;
        while at < quoted.len() {
            let byte = quoted[at];
            at += 1;
            match byte {
                b'"' | 0..=31 | 127 => return Err(framing()),
                b'\\' => {
                    let escape = *quoted.get(at).ok_or_else(framing)?;
                    at += 1;
                    bytes.push(match escape {
                        b'a' => 7,
                        b'b' => 8,
                        b't' => b'\t',
                        b'n' => b'\n',
                        b'v' => 11,
                        b'f' => 12,
                        b'r' => b'\r',
                        b'"' | b'\\' => escape,
                        b'0'..=b'3' => {
                            let next = quoted.get(at..at + 2).ok_or_else(framing)?;
                            if !next.iter().all(|byte| matches!(byte, b'0'..=b'7')) {
                                return Err(framing());
                            }
                            at += 2;
                            (escape - b'0') * 64 + (next[0] - b'0') * 8 + (next[1] - b'0')
                        }
                        _ => return Err(framing()),
                    });
                }
                _ => bytes.push(byte),
            }
        }
        bytes
    } else {
        if raw.iter().any(|byte| matches!(byte, 0..=31 | 127)) {
            return Err(framing());
        }
        raw.to_vec()
    };
    if bytes.contains(&0) {
        return Err(framing());
    }
    let path = PathBuf::from(std::ffi::OsString::from_vec(bytes));
    if !path.is_absolute() {
        return Err(framing());
    }
    Ok(path)
}

fn directories(out: &ChildOut) -> Result<Vec<PathBuf>> {
    if out.stdout_may_be_truncated() || !out.stdout.ends_with(b"\n") {
        return Err(Error::refused(
            "source object directory inventory is incomplete",
        ));
    }
    let mut paths = Vec::new();
    for line in out.stdout[..out.stdout.len() - 1].split(|byte| *byte == b'\n') {
        if let Some(raw) = line.strip_prefix(b"alternate: ") {
            let path = directory(raw)?;
            if !paths.contains(&path) {
                paths.push(path);
            }
        } else {
            let Some(colon) = line.iter().position(|byte| *byte == b':') else {
                return Err(framing());
            };
            let (key, value) = (&line[..colon], &line[colon + 1..]);
            let keys: &[&[u8]] = &[
                b"count",
                b"size",
                b"in-pack",
                b"packs",
                b"size-pack",
                b"prune-packable",
                b"garbage",
                b"size-garbage",
            ];
            if !keys.contains(&key)
                || value.first() != Some(&b' ')
                || value.len() < 2
                || !value[1..].iter().all(u8::is_ascii_digit)
            {
                return Err(framing());
            }
        }
    }
    Ok(paths)
}

pub(super) fn lend(
    scratch: Option<&Path>,
    common: &str,
    inventory: impl FnOnce() -> Result<ChildOut>,
) -> Result<()> {
    let Some(scratch) = scratch else {
        return Ok(());
    };
    let source = Path::new(common).join("objects");
    let mut alternates = format!("{}\n", source.display());
    // Preserve the ordinary one-line view and its ref-tip discovery when the
    // source has no alternate graph. An unreadable file never means absent.
    match std::fs::metadata(source.join("info/alternates")) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(Error::local(format!("source object view: {error}"))),
        Ok(_) => {
            let paths = directories(&inventory()?)?;
            if !paths.is_empty() {
                let views = aterm_tempfile::TempDir::new_in(scratch)
                    .map_err(|error| Error::local(format!("private object views: {error}")))?;
                for (index, path) in paths.iter().enumerate() {
                    let view = views.path().join(index.to_string());
                    std::fs::create_dir(&view)
                        .map_err(|error| Error::local(format!("private object view: {error}")))?;
                    // These aliases never expose source info/alternates. Giving
                    // the real directories directly to Git would recursively
                    // admit stores beyond the original source's depth limit.
                    for name in std::iter::once("pack".to_owned())
                        .chain((0..256).map(|byte| format!("{byte:02x}")))
                    {
                        std::os::unix::fs::symlink(path.join(&name), view.join(&name)).map_err(
                            |error| Error::local(format!("private object alias: {error}")),
                        )?;
                    }
                    let view = view
                        .to_str()
                        .filter(|s| !s.contains(['\n', '\r']))
                        .ok_or_else(framing)?;
                    alternates.push_str(view);
                    alternates.push('\n');
                }
                // The enclosing per-run scratch owns cleanup, including while
                // its alternates file is temporarily removed for membership.
                let _ = views.keep();
            }
        }
    }
    std::fs::write(scratch.join("objects/info/alternates"), alternates)
        .map_err(|error| Error::local(format!("private object view: {error}")))
}

#[cfg(test)]
#[path = "object_view_tests.rs"]
mod tests;
