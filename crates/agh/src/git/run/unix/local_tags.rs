// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Raw tag residency is advice for an exact fetch, never a remote observation.
//! Unsupported representations retain the ordinary cold import. Native failures
//! propagate; only complete, hashed tag chains may reach the original-root walk.

use super::{ChildOut, Children, Error, GitRun, Result, failed, os, unsent};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;

const BODY_MAX: usize = ChildOut::OUTPUT_KEEP / 4;
const BYTES_MAX: usize = 16 * 1024 * 1024;
const OBJECTS_MAX: usize = 4096;
const DEPTH_MAX: usize = 32;
const BATCH_MAX: usize = 128;
const CAPTURE_MAX: usize = ChildOut::OUTPUT_KEEP / 2;

const _: () = assert!(BODY_MAX + 128 <= CAPTURE_MAX);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Tag,
    Commit,
}

impl Kind {
    fn text(self) -> &'static str {
        match self {
            Self::Tag => "tag",
            Self::Commit => "commit",
        }
    }
}

#[derive(Clone)]
struct Wanted {
    id: String,
    kind: Kind,
    depth: usize,
}

struct ObjectSize {
    wanted: Wanted,
    size: usize,
}

fn tag_target(body: &[u8]) -> Option<(String, Kind)> {
    let end = body.windows(2).position(|pair| pair == b"\n\n")?;
    let mut lines = body[..end].split(|byte| *byte == b'\n');
    // Canonical headers only. Other valid encodings still have the cold path.
    let (Some(object), Some(kind), Some(name), Some(tagger), None) = (
        lines.next(),
        lines.next(),
        lines.next(),
        lines.next(),
        lines.next(),
    ) else {
        return None;
    };
    let id = std::str::from_utf8(object.strip_prefix(b"object ")?).ok()?;
    if !crate::path::is_sha(id) {
        return None;
    }
    let kind = match kind.strip_prefix(b"type ")? {
        b"tag" => Kind::Tag,
        b"commit" => Kind::Commit,
        _ => return None,
    };
    let name = std::str::from_utf8(name.strip_prefix(b"tag ")?).ok()?;
    if !crate::path::ref_name_ok(&format!("refs/tags/{name}")) {
        return None;
    }
    if !identity(tagger.strip_prefix(b"tagger ")?) {
        return None;
    }
    Some((id.to_owned(), kind))
}

fn identity(bytes: &[u8]) -> bool {
    let Some(text) = std::str::from_utf8(bytes)
        .ok()
        .filter(|text| !text.chars().any(char::is_control))
    else {
        return false;
    };
    let Some((person_time, zone)) = text.rsplit_once(' ') else {
        return false;
    };
    let zone = zone.as_bytes();
    if zone.len() != 5
        || !matches!(zone[0], b'+' | b'-')
        || !zone[1..].iter().all(u8::is_ascii_digit)
        || &zone[1..3] > b"23"
        || &zone[3..] > b"59"
    {
        return false;
    }
    let Some((person, time)) = person_time.rsplit_once(' ') else {
        return false;
    };
    if time.is_empty()
        || !time.bytes().all(|byte| byte.is_ascii_digit())
        || time.parse::<i64>().is_err()
    {
        return false;
    }
    let Some(person) = person.strip_suffix('>') else {
        return false;
    };
    let Some((name, email)) = person.split_once(" <") else {
        return false;
    };
    !name.trim().is_empty()
        && name.trim() == name
        && !email.is_empty()
        && !email.bytes().any(|byte| byte.is_ascii_whitespace())
        && !name.contains(['<', '>'])
        && !email.contains(['<', '>'])
}

fn inventory(wanted: &[Wanted], out: &ChildOut) -> Option<Vec<ObjectSize>> {
    if out.stdout_may_be_truncated() {
        return None;
    }
    let text = std::str::from_utf8(&out.stdout).ok()?.strip_suffix('\n')?;
    let rows = text.split('\n').collect::<Vec<_>>();
    if rows.len() != wanted.len() {
        return None;
    }
    wanted
        .iter()
        .zip(rows)
        .map(|(wanted, row)| {
            let fields = row.split(' ').collect::<Vec<_>>();
            let [id, kind, size] = fields.as_slice() else {
                return None;
            };
            let size = size
                .parse::<usize>()
                .ok()
                .filter(|size| *size <= BODY_MAX)?;
            (*id == wanted.id && *kind == wanted.kind.text()).then(|| ObjectSize {
                wanted: wanted.clone(),
                size,
            })
        })
        .collect()
}

struct Observed {
    wanted: Wanted,
    target: Option<(String, Kind)>,
    body: Vec<u8>,
}

fn bodies(group: &[ObjectSize], out: &ChildOut) -> Option<Vec<Observed>> {
    if out.stdout_may_be_truncated() {
        return None;
    }
    let mut bytes = out.stdout.as_slice();
    let mut links = Vec::with_capacity(group.len());
    for item in group {
        let end = bytes.iter().position(|byte| *byte == b'\n')?;
        let header = format!(
            "{} {} {}",
            item.wanted.id,
            item.wanted.kind.text(),
            item.size
        );
        if bytes.get(..end)? != header.as_bytes() || bytes.get(end + 1 + item.size) != Some(&b'\n')
        {
            return None;
        }
        let body = bytes.get(end + 1..end + 1 + item.size)?;
        let target = match item.wanted.kind {
            Kind::Tag => Some(tag_target(body)?),
            Kind::Commit => None,
        };
        links.push(Observed {
            wanted: item.wanted.clone(),
            target,
            body: body.to_vec(),
        });
        bytes = &bytes[end + 2 + item.size..];
    }
    bytes.is_empty().then_some(links)
}

struct Reader<'a, 'gh> {
    children: &'a Children<'gh>,
    run: &'a GitRun,
    common: &'a str,
    canonical: Vec<u8>,
    serial: usize,
}

impl Reader<'_, '_> {
    fn checked(
        &mut self,
        name: &str,
        args: Vec<OsString>,
        cwd: Option<&Path>,
        input: &[u8],
    ) -> Result<ChildOut> {
        self.serial += 1;
        let key = [
            self.canonical.as_slice(),
            format!("\nstep batch-local-tags {} {name}", self.serial).as_bytes(),
        ]
        .concat();
        let mut all = vec![
            os("--git-dir"),
            os(self.common),
            os("-c"),
            os("core.commitGraph=false"),
        ];
        all.extend(args);
        let out = self
            .children
            .git_input(&all, cwd, &key, input)
            .map_err(unsent)?;
        if out.code != Some(0) {
            return Err(failed(
                &self.run.op,
                &out,
                self.children.gh.redactor(),
                &self.run.slug,
                false,
            )
            .with_next("fetch step: batch-local-tags"));
        }
        Ok(out)
    }

    fn query(&mut self, batch: &[Wanted], content: bool) -> Result<ChildOut> {
        let input = batch
            .iter()
            .map(|item| format!("{}\n", item.id))
            .collect::<String>();
        self.checked(
            if content { "contents" } else { "types" },
            vec![
                os("cat-file"),
                os(if content { "--batch" } else { "--batch-check" }),
            ],
            None,
            input.as_bytes(),
        )
    }

    fn hashes(&mut self, group: &[Observed]) -> Result<bool> {
        // Only owned data files are staged. The checked child has fully settled
        // before this directory drops, including on cancellation or failure.
        let parent = self.children.scratch_git.as_deref().and_then(Path::parent);
        let private = match parent {
            Some(parent) => aterm_tempfile::TempDir::new_in(parent),
            None => aterm_tempfile::TempDir::new(),
        }
        .map_err(|error| Error::local(format!("local tag hash storage: {error}")))?;
        for (index, object) in group.iter().enumerate() {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(private.path().join(index.to_string()))
                .map_err(|error| Error::local(format!("local tag hash body: {error}")))?;
            file.write_all(&object.body)
                .map_err(|error| Error::local(format!("local tag hash body: {error}")))?;
        }
        for kind in [Kind::Tag, Kind::Commit] {
            let mut input = String::new();
            let mut expected = String::new();
            for (index, object) in group
                .iter()
                .enumerate()
                .filter(|(_, object)| object.wanted.kind == kind)
            {
                input.push_str(&format!("{index}\n"));
                expected.push_str(&object.wanted.id);
                expected.push('\n');
            }
            if input.is_empty() {
                continue;
            }
            // Decimal relative filenames and this private cwd avoid pathname
            // quoting. No -w, filters or caller object/ref changes are allowed.
            // Git's checked native hashing preserves its SHA1DC authority.
            let out = self.checked(
                kind.text(),
                vec![
                    os("hash-object"),
                    os("--no-filters"),
                    os("-t"),
                    os(kind.text()),
                    os("--stdin-paths"),
                ],
                Some(private.path()),
                input.as_bytes(),
            )?;
            if out.stdout_may_be_truncated() || out.stdout != expected.as_bytes() {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

pub(super) fn resident(
    children: &Children<'_>,
    run: &GitRun,
    common: &str,
    roots: &[&str],
) -> Result<bool> {
    if roots.is_empty() {
        return Ok(true);
    }
    if roots.len() > OBJECTS_MAX {
        return Ok(false);
    }
    let mut kinds = roots
        .iter()
        .map(|id| ((*id).to_owned(), Kind::Tag))
        .collect::<BTreeMap<_, _>>();
    let mut pending = kinds
        .keys()
        .map(|id| Wanted {
            id: id.clone(),
            kind: Kind::Tag,
            depth: 0,
        })
        .collect::<Vec<_>>();
    let mut links = BTreeMap::<String, Option<String>>::new();
    let mut bytes = 0_usize;
    let mut reader = Reader {
        children,
        run,
        common,
        canonical: crate::fx::canon::git_request(run),
        serial: 0,
    };
    while !pending.is_empty() {
        let mut next = Vec::new();
        for batch in pending.chunks(BATCH_MAX) {
            let out = reader.query(batch, false)?;
            let Some(sized) = inventory(batch, &out) else {
                return Ok(false);
            };
            let mut start = 0;
            while start < sized.len() {
                let mut end = start;
                let mut capture = 0;
                while end < sized.len() && capture + sized[end].size + 128 <= CAPTURE_MAX {
                    capture += sized[end].size + 128;
                    end += 1;
                }
                let group = &sized[start..end];
                let Some(total) = group
                    .iter()
                    .try_fold(bytes, |total, item| total.checked_add(item.size))
                else {
                    return Ok(false);
                };
                if total > BYTES_MAX {
                    return Ok(false);
                }
                bytes = total;
                let wanted = group
                    .iter()
                    .map(|item| item.wanted.clone())
                    .collect::<Vec<_>>();
                let out = reader.query(&wanted, true)?;
                let Some(observed) = bodies(group, &out) else {
                    return Ok(false);
                };
                if !reader.hashes(&observed)? {
                    return Ok(false);
                }
                for Observed { wanted, target, .. } in observed {
                    links.insert(wanted.id, target.as_ref().map(|(id, _)| id.clone()));
                    if let Some((id, kind)) = target {
                        let depth = wanted.depth + 1;
                        if depth > DEPTH_MAX {
                            return Ok(false);
                        }
                        if let Some(known) = kinds.get(&id) {
                            if *known != kind {
                                return Ok(false);
                            }
                        } else {
                            if kinds.len() == OBJECTS_MAX {
                                return Ok(false);
                            }
                            kinds.insert(id.clone(), kind);
                            next.push(Wanted { id, kind, depth });
                        }
                    }
                }
                start = end;
            }
        }
        pending = next;
    }
    // Shared suffixes cannot hide an overlong chain from a later root.
    for root in roots {
        let mut id = *root;
        let mut seen = BTreeSet::new();
        for depth in 0..=DEPTH_MAX {
            if !seen.insert(id) {
                return Ok(false);
            }
            match links.get(id) {
                Some(None) => break,
                Some(Some(target)) if depth < DEPTH_MAX => id = target,
                _ => return Ok(false),
            }
        }
    }
    Ok(true)
}

#[cfg(test)]
#[path = "local_tags_tests.rs"]
mod tests;
