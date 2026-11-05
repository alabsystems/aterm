// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHAT A MEASURED CODEX BUILD SHOWED (gaps 20, 25): the keys and the closed words beside their
//! keys that a codex-alab build's committed rollout reductions hold, read by one scanner that
//! `build.rs` mounts (`CODEX_SHOWN`, `super::codex_steps`) and the lab reads too (a proof whose
//! aterm was rebuilt after the reductions were written). Std only: `build.rs` compiles it alone.

use std::path::Path;

/// Whether `s` is a key as the scanner takes one: snake case, `[a-z][a-z0-9_]*`.
fn snake_key(s: &[u8]) -> bool {
    s.first().is_some_and(u8::is_ascii_lowercase)
        && s.iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_')
}

/// Whether `s` is a closed word as the scanner takes one: snake case with no digit,
/// `[a-z][a-z_]*`. A digit marks what the reducer remapped or kept as a number (an id such as
/// `call_r0010`, a time, a version), which is never a closed word.
fn snake_word(s: &[u8]) -> bool {
    s.first().is_some_and(u8::is_ascii_lowercase)
        && s.iter().all(|b| b.is_ascii_lowercase() || *b == b'_')
}

/// The JSON string opening at `line[at]` (a `"`): its raw bytes (a backslash escapes the next
/// byte, kept as written) and the index past its closing quote.
fn string_at(line: &[u8], at: usize) -> (&[u8], usize) {
    let mut i = at + 1;
    while i < line.len() {
        match line[i] {
            b'\\' => i += 2,
            b'"' => return (&line[at + 1..i], i + 1),
            _ => i += 1,
        }
    }
    (&line[(at + 1).min(line.len())..], line.len())
}

/// The index of the first byte at or after `i` that is no blank.
fn past_blanks(line: &[u8], mut i: usize) -> usize {
    while line
        .get(i)
        .is_some_and(|b| matches!(b, b' ' | b'\t' | b'\r'))
    {
        i += 1;
    }
    i
}

/// WHAT `texts` SHOW: of every line that is not a reduction's own `#` line, each string followed
/// by `:` that is snake case (`K`, [`snake_key`]), and, where the value after that `:` is a
/// string that is a closed word (`V`, [`snake_word`]), `K=V` too. Returned as
/// `"\n<tok>\n<tok>…\n"`, sorted and once each (`"\n"` for none), so a token is found whole by
/// `\n<tok>\n`. Ids, times, versions, numbers and blanked `""` never match a word.
#[must_use]
pub fn words(texts: &[&str]) -> String {
    let mut out: Vec<String> = Vec::new();
    for line in texts
        .iter()
        .flat_map(|t| t.lines())
        .filter(|l| !l.starts_with('#'))
    {
        let b = line.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if b[i] != b'"' {
                i += 1;
                continue;
            }
            let (key, after) = string_at(b, i);
            let colon = past_blanks(b, after);
            if b.get(colon) != Some(&b':') {
                i = after;
                continue;
            }
            let value = past_blanks(b, colon + 1);
            let keyed = snake_key(key);
            if keyed {
                out.push(String::from_utf8_lossy(key).into_owned());
            }
            if b.get(value) != Some(&b'"') {
                i = value;
                continue;
            }
            let (word, past) = string_at(b, value);
            if keyed && snake_word(word) {
                out.push(format!(
                    "{}={}",
                    String::from_utf8_lossy(key),
                    String::from_utf8_lossy(word)
                ));
            }
            i = past;
        }
    }
    out.sort();
    out.dedup();
    let mut said = String::from("\n");
    for t in &out {
        said.push_str(t);
        said.push('\n');
    }
    said
}

/// The build a reduction's own first line names (`# codex <build> · lab reduce …`).
#[must_use]
pub fn build_of(head: &str) -> Option<&str> {
    head.strip_prefix("# codex ")?
        .split(' ')
        .next()
        .filter(|b| !b.is_empty() && *b != "-")
}

/// [`words`] over the `codex-alab-*` reductions in `steps` whose first line names `build`
/// (`"\n"` when none does, or `steps` cannot be read).
#[must_use]
pub fn shown_in(steps: &Path, build: &str) -> String {
    let mut texts: Vec<String> = Vec::new();
    let Ok(entries) = std::fs::read_dir(steps) else {
        return words(&[]);
    };
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !(name.starts_with("codex-alab-") && name.ends_with(".jsonl")) {
            continue;
        }
        use std::io::Read as _;
        const MAX_SHOWN_BYTES: u64 = 16 * 1024 * 1024;
        let mut opts = std::fs::OpenOptions::new();
        opts.read(true);
        #[cfg(target_os = "macos")]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.custom_flags(0x0004 | 0x0100 | 0x0100_0000);
        }
        #[cfg(all(target_os = "linux", any(target_arch = "aarch64", target_arch = "arm")))]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.custom_flags(0o100_000 | 0o4000 | 0o2_000_000);
        }
        #[cfg(all(
            target_os = "linux",
            not(any(target_arch = "aarch64", target_arch = "arm"))
        ))]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.custom_flags(0o400_000 | 0o4000 | 0o2_000_000);
        }
        let Ok(file) = opts.open(entry.path()) else {
            continue;
        };
        let Ok(meta) = file.metadata() else {
            continue;
        };
        if !meta.is_file() || meta.len() > MAX_SHOWN_BYTES {
            continue;
        }
        let cap = usize::try_from(meta.len().min(MAX_SHOWN_BYTES))
            .unwrap_or(0)
            .saturating_add(1);
        let mut text = String::with_capacity(cap);
        if file
            .take(MAX_SHOWN_BYTES + 1)
            .read_to_string(&mut text)
            .is_err()
            || text.len() as u64 > MAX_SHOWN_BYTES
        {
            continue;
        }
        if build_of(text.lines().next().unwrap_or("")) == Some(build) {
            texts.push(text);
        }
    }
    let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
    words(&refs)
}
