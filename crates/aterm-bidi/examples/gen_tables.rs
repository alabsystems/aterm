// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Regenerates `aterm-bidi` Unicode tables and test subsets from the UCD.

use std::path::Path;
use std::process::ExitCode;

const USAGE: &str = "usage: gen_tables tables <DerivedBidiClass.txt> <BidiBrackets.txt>\n       \
                     gen_tables subset <BidiTest.txt> <BidiCharacterTest.txt>";

/// Every `Bidi_Class` long name the UCD writes, with its abbreviation (the `BidiClass` variant).
const NAMES: &[(&str, &str)] = &[
    ("Left_To_Right", "L"),
    ("Right_To_Left", "R"),
    ("Arabic_Letter", "AL"),
    ("European_Number", "EN"),
    ("European_Separator", "ES"),
    ("European_Terminator", "ET"),
    ("Arabic_Number", "AN"),
    ("Common_Separator", "CS"),
    ("Nonspacing_Mark", "NSM"),
    ("Boundary_Neutral", "BN"),
    ("Paragraph_Separator", "B"),
    ("Segment_Separator", "S"),
    ("White_Space", "WS"),
    ("Other_Neutral", "ON"),
    ("Left_To_Right_Embedding", "LRE"),
    ("Left_To_Right_Override", "LRO"),
    ("Right_To_Left_Embedding", "RLE"),
    ("Right_To_Left_Override", "RLO"),
    ("Pop_Directional_Format", "PDF"),
    ("Left_To_Right_Isolate", "LRI"),
    ("Right_To_Left_Isolate", "RLI"),
    ("First_Strong_Isolate", "FSI"),
    ("Pop_Directional_Isolate", "PDI"),
];

const HEADER: &str = "// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// GENERATED — do not edit by hand. Regenerate with
//     targo --unverified run -p aterm-bidi --example gen_tables -- tables <DerivedBidiClass.txt> <BidiBrackets.txt>
// from the Unicode Character Database, whose data is under the Unicode License v3
// (UNICODE-LICENSE.txt at the repo root).

use crate::BidiClass;
use crate::BidiClass::*;

";

/// One past the last code point.
const CODE_SPACE: usize = 0x11_0000;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let done = match args.as_slice() {
        [verb, derived, brackets] if verb == "tables" => tables(&read(derived), &read(brackets))
            .and_then(|text| write(&crate_dir.join("src/tables.rs"), &text)),
        [verb, bidi_test, character_test] if verb == "subset" => {
            let data = crate_dir.join("tests/data");
            cut_bidi_test(&read(bidi_test))
                .and_then(|text| write(&data.join("BidiTest-subset.txt"), &text))
                .and_then(|()| cut_character_test(&read(character_test)))
                .and_then(|text| write(&data.join("BidiCharacterTest-subset.txt"), &text))
        }
        _ => Err(USAGE.to_string()),
    };
    match done {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("gen_tables: {why}");
            ExitCode::FAILURE
        }
    }
}

/// The file at `path`, or the reason it cannot be read.
fn read(path: &str) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))
}

fn write(path: &Path, text: &str) -> Result<(), String> {
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

/// A `XXXX` or `XXXX..YYYY` code point field.
fn span(field: &str) -> Result<(usize, usize), String> {
    let hex = |s: &str| usize::from_str_radix(s, 16).map_err(|e| format!("`{s}`: {e}"));
    match field.split_once("..") {
        Some((a, b)) => Ok((hex(a)?, hex(b)?)),
        None => hex(field).map(|a| (a, a)),
    }
}

/// The abbreviation of a long `Bidi_Class` name.
fn abbreviation(long: &str) -> Result<&'static str, String> {
    NAMES
        .iter()
        .find(|(name, _)| *name == long)
        .map(|(_, short)| *short)
        .ok_or_else(|| format!("`{long}` is no Bidi_Class name"))
}

/// The UCD version `DerivedBidiClass-<version>.txt` names in its header.
fn ucd_version(text: &str) -> Result<&str, String> {
    let missing = || "no `DerivedBidiClass-<version>.txt` in the header".to_string();
    let at = text.find("DerivedBidiClass-").ok_or_else(missing)? + "DerivedBidiClass-".len();
    let rest = &text[at..];
    let run = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(rest.len());
    match rest[..run].strip_suffix('.') {
        Some(version) if rest[run..].starts_with("txt") && !version.is_empty() => Ok(version),
        _ => Err(missing()),
    }
}

/// `line` without its `#` comment, trimmed.
fn data(line: &str) -> &str {
    line.split('#').next().unwrap_or("").trim()
}

/// `src/tables.rs` from `DerivedBidiClass.txt` and `BidiBrackets.txt`.
fn tables(
    derived: &Result<String, String>,
    brackets: &Result<String, String>,
) -> Result<String, String> {
    let (derived, brackets) = (derived.as_deref()?, brackets.as_deref()?);
    let version = ucd_version(derived)?;
    let mut class = vec!["L"; CODE_SPACE];
    let mut assign = |(a, b): (usize, usize), value: &'static str| {
        class
            .get_mut(a..=b)
            .map(|cells| cells.fill(value))
            .ok_or_else(|| format!("{a:04X}..{b:04X} is outside the code space"))
    };
    for line in derived.lines() {
        let Some(missing) = line.strip_prefix("# @missing: ") else {
            continue;
        };
        let (field, value) = missing
            .split_once("; ")
            .ok_or_else(|| format!("malformed @missing line `{line}`"))?;
        let value: String = value
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        assign(span(field)?, abbreviation(&value)?)?;
    }
    for line in derived.lines().map(data).filter(|l| !l.is_empty()) {
        let [field, value] = fields(line)?;
        let value = match NAMES.iter().find(|(_, short)| *short == value) {
            Some((_, short)) => *short,
            None => abbreviation(value)?,
        };
        assign(span(field)?, value)?;
    }
    let mut ranges = Vec::new();
    let mut start = 0;
    for c in 1..=CODE_SPACE {
        if c == CODE_SPACE || class[c] != class[start] {
            if class[start] != "L" {
                ranges.push((start, c - 1, class[start]));
            }
            start = c;
        }
    }
    let mut pairs = Vec::new();
    for line in brackets.lines().map(data).filter(|l| !l.is_empty()) {
        let [open, close, kind] = fields(line)?;
        if kind == "o" {
            pairs.push((span(open)?.0, span(close)?.0));
        }
    }
    let mut out = String::from(HEADER);
    out.push_str(&format!(
        "// Generated from the Unicode Character Database {version} (extracted/DerivedBidiClass.txt,\n\
         // including its @missing defaults) by crates/aterm-bidi/examples/gen_tables.rs. Every code point\n\
         // not inside a listed range is `L`. Sorted, non-overlapping, binary-searched.\n\
         pub(crate) const BIDI_CLASS_RANGES: &[(u32, u32, BidiClass)] = &[\n"
    ));
    for (a, b, v) in &ranges {
        out.push_str(&format!("    (0x{a:04X}, 0x{b:04X}, {v}),\n"));
    }
    out.push_str("];\n\n");
    out.push_str(
        "/// Every opening paired bracket (BidiBrackets.txt, `o`) with its closing pair, sorted by opener.\n\
         pub(crate) const BRACKET_PAIRS: &[(u32, u32)] = &[\n",
    );
    let mut by_open = pairs.clone();
    by_open.sort_unstable();
    for (a, b) in &by_open {
        out.push_str(&format!("    (0x{a:04X}, 0x{b:04X}),\n"));
    }
    out.push_str("];\n\n");
    out.push_str(
        "/// The same pairs keyed by closer: `(close, open)`, sorted by closer.\n\
         pub(crate) const BRACKET_CLOSERS: &[(u32, u32)] = &[\n",
    );
    pairs.sort_by_key(|&(_, close)| close);
    for (a, b) in &pairs {
        out.push_str(&format!("    (0x{b:04X}, 0x{a:04X}),\n"));
    }
    out.push_str("];\n");
    Ok(out)
}

/// The `;`-separated fields of a data line, trimmed: exactly `N` of them.
fn fields<const N: usize>(line: &str) -> Result<[&str; N], String> {
    let parts: Vec<&str> = line.split(';').map(str::trim).collect();
    parts
        .try_into()
        .map_err(|parts: Vec<&str>| format!("`{line}` has {} fields, not {N}", parts.len()))
}

/// `BidiCharacterTest-subset.txt`.
fn cut_character_test(text: &Result<String, String>) -> Result<String, String> {
    let lines: Vec<&str> = text.as_deref()?.lines().collect();
    let marker = lines
        .iter()
        .position(|l| l.contains("Permutations of sequences"))
        .ok_or("no `Permutations of sequences` block in BidiCharacterTest.txt")?;
    let mut out: Vec<&str> = lines[..marker].to_vec();
    out.push("# Generated permutation cases: every 40th line (examples/gen_tables.rs subset).");
    out.extend(
        lines[marker..]
            .iter()
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .step_by(40),
    );
    Ok(out.join("\n") + "\n")
}

/// `BidiTest-subset.txt`.
fn cut_bidi_test(text: &Result<String, String>) -> Result<String, String> {
    let mut out: Vec<&str> = Vec::new();
    let mut seen = 0usize;
    for line in text.as_deref()?.lines() {
        if line.is_empty() || line.starts_with('#') {
            if seen == 0 {
                out.push(line);
            }
            continue;
        }
        if line.starts_with('@') {
            out.push(line);
            continue;
        }
        if seen.is_multiple_of(150) {
            out.push(line);
        }
        seen += 1;
    }
    Ok(out.join("\n") + "\n")
}
