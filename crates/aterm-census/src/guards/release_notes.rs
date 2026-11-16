// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Admit the same entry bytes the release assembles, before they land. Unlike
//! optional source lookups, an unreadable fragment is a finding, never absence.
use super::{Finding, Rule, Tree};
use crate::release_notes::{FRAGMENT_DIR, MAX_FRAGMENT_BYTES, fragment_findings};
use std::io::{self, Read as _};
use std::path::Path;

pub(super) const RELEASE_NOTES: Rule = Rule {
    id: "release-notes",
    why: "each changes/<kind>/<name>.md is one plain UTF-8 entry the release can ship as written",
    scope: &[],
    check,
};

fn report(path: &str, message: String) -> Finding {
    let (line, detail) = message
        .strip_prefix(path)
        .and_then(|tail| tail.strip_prefix(" line "))
        .and_then(|tail| tail.split_once(": "))
        .and_then(|(line, detail)| line.parse().ok().map(|line| (line, detail.to_string())))
        .unwrap_or_else(|| {
            (
                0,
                message
                    .strip_prefix(&format!("{path}: "))
                    .unwrap_or(&message)
                    .to_string(),
            )
        });
    Finding {
        rule: RELEASE_NOTES.id,
        path: path.to_string(),
        line,
        detail,
    }
}

fn check(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    let roots = match crate::tree::read_dir(tree.root) {
        Ok(roots) => roots,
        Err(error) => {
            out.push(report(
                FRAGMENT_DIR,
                format!("cannot list checkout: {error}"),
            ));
            return;
        }
    };
    let mut pending = Vec::new();
    for root in roots {
        match root {
            Ok(path)
                if path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.eq_ignore_ascii_case(FRAGMENT_DIR)) =>
            {
                pending.push(path)
            }
            Ok(_) => {}
            Err(error) => out.push(report(
                FRAGMENT_DIR,
                format!("cannot read checkout entry: {error}"),
            )),
        }
    }
    // The census's scope is the captured checkout, not a second Git reader: it
    // covers all tracked entries and also any present untracked entry here.
    // Walk dot/nested directories too; an invalid path must not hide a fragment.
    while let Some(path) = pending.pop() {
        let rel = path.strip_prefix(tree.root).ok().and_then(Path::to_str);
        let Some(rel) = rel else {
            out.push(report(
                FRAGMENT_DIR,
                format!(
                    "entry path is not repository-relative UTF-8: {}",
                    path.display()
                ),
            ));
            continue;
        };
        let rel = rel.replace(std::path::MAIN_SEPARATOR, "/");
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_dir() => {
                if rel.eq_ignore_ascii_case(FRAGMENT_DIR) && rel != FRAGMENT_DIR {
                    out.extend(
                        crate::release_notes::path_findings(&rel)
                            .into_iter()
                            .map(|message| report(&rel, message)),
                    );
                }
                match crate::tree::read_dir(&path) {
                    Ok(entries) => {
                        for entry in entries {
                            match entry {
                                Ok(path) => pending.push(path),
                                Err(error) => out.push(report(
                                    &rel,
                                    format!("cannot read directory entry: {error}"),
                                )),
                            }
                        }
                    }
                    Err(error) => out.push(report(
                        &rel,
                        format!("cannot list entry directory: {error}"),
                    )),
                }
            }
            Ok(_) => match plain_text(&path) {
                Ok(text) => out.extend(
                    fragment_findings(&rel, &text)
                        .into_iter()
                        .map(|message| report(&rel, message)),
                ),
                Err(error) => out.push(report(
                    &rel,
                    format!("cannot read plain, non-executable UTF-8 entry: {error}"),
                )),
            },
            Err(error) => out.push(report(&rel, format!("cannot inspect entry: {error}"))),
        }
    }
}

fn plain_text(path: &Path) -> io::Result<String> {
    crate::inputs::before(path, "file");
    if !std::fs::symlink_metadata(path)?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    let (file, _) = crate::tree::open_regular(path)?;
    let metadata = file.metadata()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o111 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "executable file",
            ));
        }
    }
    if metadata.len() > MAX_FRAGMENT_BYTES as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "fragment exceeds the release byte bound",
        ));
    }
    let mut text = String::new();
    file.take(MAX_FRAGMENT_BYTES as u64 + 1)
        .read_to_string(&mut text)?;
    if text.len() > MAX_FRAGMENT_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "fragment grew past the release byte bound",
        ));
    }
    Ok(text)
}

#[cfg(test)]
#[path = "release_notes_tests.rs"]
mod tests;
