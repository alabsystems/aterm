// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Platform-neutral UTF-16 command-line quoting (MSVCRT/`CommandLineToArgvW` rules) and
//! `CreateProcessW` environment-block construction.

const QUOTE: u16 = b'"' as u16;
const BACKSLASH: u16 = b'\\' as u16;
const SPACE: u16 = b' ' as u16;
const TAB: u16 = b'\t' as u16;

/// Flatten UTF-16 `argv` slices into one NUL-terminated UTF-16 command line under the MSVCRT
/// quoting rules. `argv[0]` must not contain `'"'` (Windows file names forbid `'"'`, and MSVCRT
/// parses `argv[0]` with literal backslashes via [`append_argv0_units`]); `argv[1..]` escape
/// quotes and pre-quote backslashes via [`append_quoted_units`].
pub(crate) fn build_command_line_units<I, S>(argv: I) -> Vec<u16>
where
    I: IntoIterator<Item = S>,
    S: AsRef<[u16]>,
{
    let mut out: Vec<u16> = Vec::new();
    for (i, arg) in argv.into_iter().enumerate() {
        let units = arg.as_ref();
        if i == 0 {
            append_argv0_units(units, &mut out);
        } else {
            out.push(SPACE);
            append_quoted_units(units, &mut out);
        }
    }
    out.push(0);
    out
}

/// Quote `argv[0]` under MSVCRT/`CommandLineToArgvW`'s program-name rule: backslashes in `argv[0]`
/// are always literal, so trailing backslashes inside quotes must NOT be doubled. Precondition:
/// `units` does not contain `'"'`.
pub(crate) fn append_argv0_units(units: &[u16], out: &mut Vec<u16>) {
    let needs_quotes =
        units.is_empty() || units.iter().any(|&u| u == SPACE || u == TAB || u == QUOTE);
    if !needs_quotes {
        out.extend_from_slice(units);
        return;
    }
    out.push(QUOTE);
    out.extend_from_slice(units);
    out.push(QUOTE);
}

/// Quote `argv[i]` (`i >= 1`) under MSVCRT/`CommandLineToArgvW` argument rules: a run of `n`
/// backslashes before `'"'` becomes `2n + 1` backslashes plus `'"'`, a trailing run inside quotes
/// becomes `2n` backslashes before the closing `'"'`, and other backslashes stay literal.
pub(crate) fn append_quoted_units(units: &[u16], out: &mut Vec<u16>) {
    let needs_quotes =
        units.is_empty() || units.iter().any(|&u| u == SPACE || u == TAB || u == QUOTE);
    if !needs_quotes {
        out.extend_from_slice(units);
        return;
    }
    out.push(QUOTE);
    let mut i = 0;
    while i < units.len() {
        let mut n_backslashes = 0usize;
        while i < units.len() && units[i] == BACKSLASH {
            n_backslashes += 1;
            i += 1;
        }
        if i == units.len() {
            out.extend(std::iter::repeat_n(BACKSLASH, n_backslashes * 2));
        } else if units[i] == QUOTE {
            out.extend(std::iter::repeat_n(BACKSLASH, n_backslashes * 2 + 1));
            out.push(QUOTE);
            i += 1;
        } else {
            out.extend(std::iter::repeat_n(BACKSLASH, n_backslashes));
            out.push(units[i]);
            i += 1;
        }
    }
    out.push(QUOTE);
}

/// Build the `CreateProcessW` UTF-16 environment block (`KEY=VAL\0 … \0\0`), deduped
/// ASCII-case-insensitively with the last occurrence winning and sorted ASCII-case-insensitively
/// by key.
pub(crate) fn build_env_block_units<K, V>(pairs: &[(K, V)]) -> Vec<u16>
where
    K: AsRef<[u16]>,
    V: AsRef<[u16]>,
{
    let mut kept: Vec<(Vec<u16>, usize)> = Vec::new();
    for (idx, (k, _)) in pairs.iter().enumerate() {
        let folded: Vec<u16> = k.as_ref().iter().copied().map(fold_upper).collect();
        match kept.iter_mut().find(|(f, _)| *f == folded) {
            Some(slot) => slot.1 = idx,
            None => kept.push((folded, idx)),
        }
    }
    kept.sort_by(|a, b| a.0.cmp(&b.0));
    let mut block: Vec<u16> = Vec::new();
    for (_, idx) in &kept {
        let (k, v) = &pairs[*idx];
        block.extend_from_slice(k.as_ref());
        block.push(b'=' as u16);
        block.extend_from_slice(v.as_ref());
        block.push(0);
    }
    if kept.is_empty() {
        block.push(0);
    }
    block.push(0);
    block
}

/// Fold ASCII `'a'..='z'` to uppercase in UTF-16.
#[inline]
pub(crate) const fn fold_upper(u: u16) -> u16 {
    if u >= (b'a' as u16) && u <= (b'z' as u16) {
        u - 32
    } else {
        u
    }
}

#[cfg(test)]
#[path = "cmdline_units_tests.rs"]
mod tests;
