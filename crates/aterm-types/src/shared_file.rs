// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A shared file's face, recognised by its path (docs/DESIGN-shared-files-2026-11-11.md §3), as
//! [`crate::private_edit`] recognises a private edit: every program that holds a path (the
//! Editor tab that saved it, `aterm-file`'s layout) asks here whether it is a face, and of which
//! file, without reading anything.
//!
//! A face is `<root>/<set>/<leaf>.<ext>` under the root `~/.aterm/files`, the name `<set>/<leaf>`
//! in the grammar every shared file is named in ([`well_formed`]: each part
//! `[a-z][a-z0-9-]{0,19}`) and the extension one a face is written with ([`FACE_EXTS`]). The
//! files a commit leaves beside a face (`<leaf>.rejected.<ext>`, `<leaf>.problems`) are no face:
//! a `.` breaks the leaf's grammar and `problems` is no face's extension.

use std::path::{Component, Path};

/// The folders the root sits in under a home: `<home>/.aterm/files`.
pub const ROOT: [&str; 2] = [".aterm", "files"];

/// The extensions a face is written with: a table's TSV.
pub const FACE_EXTS: [&str; 1] = ["tsv"];

/// The most bytes one part of a name holds.
pub const MAX_PART: usize = 20;

/// Whether `bytes[from..to]` is one part of a name: a lower-case letter, then at most 19 of
/// `[a-z0-9-]`.
const fn part_at(bytes: &[u8], from: usize, to: usize) -> bool {
    if from >= to || to - from > MAX_PART || !bytes[from].is_ascii_lowercase() {
        return false;
    }
    let mut i = from + 1;
    while i < to {
        let b = bytes[i];
        if !(b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') {
            return false;
        }
        i += 1;
    }
    true
}

/// Whether `text` is one part of a name.
#[must_use]
pub const fn part(text: &str) -> bool {
    part_at(text.as_bytes(), 0, text.len())
}

/// Where `text`'s one `/` is, when both sides are parts ([`part`]).
#[must_use]
pub const fn split(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut at = None;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'/' {
            if at.is_some() {
                return None;
            }
            at = Some(i);
        }
        i += 1;
    }
    match at {
        Some(slash) if part_at(bytes, 0, slash) && part_at(bytes, slash + 1, bytes.len()) => {
            Some(slash)
        }
        _ => None,
    }
}

/// Whether `text` is a whole name, `<set>/<leaf>`.
#[must_use]
pub const fn well_formed(text: &str) -> bool {
    split(text).is_some()
}

/// The name of the face `<set>/<leaf>.<ext>` that the last two components of `parts` spell.
fn named<'a>(mut parts: impl Iterator<Item = Component<'a>>) -> Option<String> {
    let set = match parts.next()? {
        Component::Normal(set) => set.to_str()?,
        _ => return None,
    };
    let file = match parts.next()? {
        Component::Normal(file) => Path::new(file),
        _ => return None,
    };
    if parts.next().is_some() {
        return None;
    }
    let ext = file.extension()?.to_str()?;
    if !FACE_EXTS.contains(&ext) {
        return None;
    }
    let name = format!("{set}/{}", file.file_stem()?.to_str()?);
    well_formed(&name).then_some(name)
}

/// The shared file whose face `path` is under the root folder `root`: `<root>/<set>/<leaf>.<ext>`
/// with a well-formed name and a face's extension.
#[must_use]
pub fn name_in(root: &Path, path: &Path) -> Option<String> {
    named(path.strip_prefix(root).ok()?.components())
}

/// The shared file whose face `path` is under any home: `…/.aterm/files/<set>/<leaf>.<ext>`.
/// By the path alone, as a saved document's path is judged; the program that acts on a face
/// holds it to this user's own root ([`name_in`]).
#[must_use]
pub fn name_of(path: &Path) -> Option<String> {
    let parts: Vec<Component<'_>> = path.components().collect();
    let [
        ..,
        Component::Normal(dot),
        Component::Normal(files),
        set,
        file,
    ] = parts.as_slice()
    else {
        return None;
    };
    if *dot != ROOT[0] || *files != ROOT[1] {
        return None;
    }
    named([*set, *file].into_iter())
}

#[cfg(test)]
#[path = "shared_file_tests.rs"]
mod tests;
