// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shipment evidence comes from each release's own immutable object, never the
//! candidate's mutable copies of older sections.

use crate::ledger::{Error, GitRunner, Result, git_read_ok};

/// Refuse an entry of `text`'s `section` that an earlier release (a tag in `catalog` older than
/// `version`) shipped, judged by that release's own `CHANGELOG.md` object.
pub(crate) fn gate(
    git: &dyn GitRunner,
    version: &str,
    text: &str,
    section: &str,
    catalog: &str,
) -> Result<()> {
    let current = crate::gates::version_triple(version)
        .ok_or_else(|| Error::new("changelog shipment audit: invalid candidate version"))?;
    let tags = crate::gates::tag_commits(catalog);
    let releases: Vec<_> = tags
        .iter()
        .filter_map(|(tag, oid)| {
            let name = tag.strip_prefix('v')?;
            let found = crate::gates::version_triple(name)?;
            (found < current).then_some((oid.as_str(), name))
        })
        .collect();
    if releases.len() > 4096 {
        return Err(Error::new(
            "changelog shipment audit: more than 4096 release tags",
        ));
    }
    let input: String = releases
        .iter()
        .map(|(tag, _)| format!("{tag}^{{commit}}:{}\n", super::CHANGELOG_FILE))
        .collect();
    if input.is_empty() {
        return Ok(());
    }
    let out = git.git_input(
        &[
            "--no-lazy-fetch",
            "--no-replace-objects",
            "cat-file",
            "--batch",
        ],
        input.as_bytes(),
    )?;
    if !out.success() || out.stdout.len() > 256 * 1024 * 1024 {
        return Err(Error::new(
            "changelog shipment audit: unreadable or oversized release objects",
        ));
    }
    let keys: std::collections::BTreeSet<_> = super::note_keys(text, section).into_iter().collect();
    let mut rest = out.stdout.as_slice();
    for (tag, release) in releases {
        let newline = rest
            .iter()
            .position(|b| *b == b'\n')
            .ok_or_else(|| Error::new("changelog shipment audit: incomplete batch header"))?;
        let head = std::str::from_utf8(&rest[..newline])
            .map_err(|_| Error::new("changelog shipment audit: invalid batch header"))?;
        rest = &rest[newline + 1..];
        // Early releases can predate CHANGELOG.md. Their absent file announces
        // no entries; a missing required source object is refused by Git.
        if head.ends_with(" missing") {
            let tree = git_read_ok(
                git,
                &[
                    "--no-lazy-fetch",
                    "--no-replace-objects",
                    "ls-tree",
                    &format!("{tag}^{{commit}}"),
                    "--",
                    super::CHANGELOG_FILE,
                ],
            )?;
            if !tree.stdout.is_empty() {
                return Err(Error::new("changelog shipment audit: missing release blob"));
            }
            continue;
        }
        let fields: Vec<_> = head.split_whitespace().collect();
        let size = match fields.as_slice() {
            [_, "blob", size] => size.parse::<usize>().ok(),
            _ => None,
        }
        .filter(|size| *size <= 8 * 1024 * 1024)
        .ok_or_else(|| Error::new("changelog shipment audit: invalid or oversized blob"))?;
        let body = rest
            .get(..size)
            .ok_or_else(|| Error::new("changelog shipment audit: incomplete blob"))?;
        let old = std::str::from_utf8(body)
            .map_err(|_| Error::new("changelog shipment audit: non-UTF-8 release notes"))?;
        let old_section = if super::has_section(old, release) {
            release
        } else {
            "Unreleased"
        };
        let entries: std::collections::BTreeSet<_> =
            super::note_keys(old, old_section).into_iter().collect();
        for key in entries {
            if keys.contains(&key) {
                return Err(Error::new(format!(
                    "{}: [{section}] repeats an entry shipped by v{release}: {key}\nfix: remove this already-shipped copy from [{section}]",
                    super::CHANGELOG_FILE
                )));
            }
        }
        rest = rest
            .get(size..)
            .and_then(|tail| tail.strip_prefix(b"\n"))
            .ok_or_else(|| Error::new("changelog shipment audit: invalid batch ending"))?;
    }
    if !rest.is_empty() {
        return Err(Error::new("changelog shipment audit: extra batch data"));
    }
    Ok(())
}
