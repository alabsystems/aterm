// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Regenerate shaping properties from one Unicode Character Database version.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

const LIMIT: usize = 0x11_0000;
const GC: [&str; 30] = [
    "Lu", "Ll", "Lt", "Lm", "Lo", "Mn", "Mc", "Me", "Nd", "Nl", "No", "Pc", "Pd", "Ps", "Pe", "Pi",
    "Pf", "Po", "Sm", "Sc", "Sk", "So", "Zs", "Zl", "Zp", "Cc", "Cf", "Cs", "Co", "Cn",
];

/// The generator each output's seal line names (`aterm_census::seal`).
const GENERATOR: &str = "aterm-grapheme's `unicode_tables` example";
const LICENCE_HEADER: &str =
    "// Copyright 2026 Andrew Yates\n// SPDX-License-Identifier: Apache-2.0\n";
const HEADER: &str = "//\n// GENERATED — do not edit by hand. Regenerate with\n";
const LICENSE: &str = "// from the Unicode Character Database, whose data is under the Unicode License v3\n\
// (UNICODE-LICENSE.txt at the repo root).\n\n";
const USAGE: &str = "usage: targo --unverified run -p aterm-grapheme --example unicode_tables --\n\
    mirroring <BidiMirroring.txt> <output.rs>\n\
    props <Scripts.txt> <DerivedGeneralCategory.txt> <PropertyValueAliases.txt> <output.rs>";

/// The licence lines, the unstamped seal line, and the regeneration note's first line.
fn header() -> String {
    format!(
        "{LICENCE_HEADER}{}\n{HEADER}",
        aterm_census::seal::line("//", GENERATOR)
    )
}

fn version(text: &str, name: &str) -> Result<String, String> {
    let prefix = format!("{name}-");
    let value = text
        .lines()
        .find_map(|line| line.split_once(&prefix)?.1.split_once(".txt").map(|x| x.0))
        .ok_or_else(|| format!("missing {name} version header"))?;
    let parts: Vec<_> = value.split('.').collect();
    if parts.len() != 3 || parts.iter().any(|part| part.parse::<u8>().is_err()) {
        return Err(format!("invalid {name} version: {value}"));
    }
    Ok(value.to_owned())
}

fn records(text: &str) -> impl Iterator<Item = Vec<&str>> {
    text.lines().filter_map(|line| {
        let line = line.split('#').next()?.trim();
        (!line.is_empty()).then(|| line.split(';').map(str::trim).collect())
    })
}

fn codepoint(text: &str) -> Result<usize, String> {
    usize::from_str_radix(text, 16)
        .ok()
        .filter(|&point| point < LIMIT)
        .ok_or_else(|| format!("invalid Unicode code point: {text}"))
}

fn span(text: &str) -> Result<std::ops::RangeInclusive<usize>, String> {
    let (a, b) = text.split_once("..").unwrap_or((text, text));
    let (a, b) = (codepoint(a)?, codepoint(b)?);
    (a <= b)
        .then_some(a..=b)
        .ok_or_else(|| format!("reversed Unicode range: {text}"))
}

fn mirroring(text: &str) -> Result<String, String> {
    let version = version(text, "BidiMirroring")?;
    let mut pairs = Vec::new();
    for fields in records(text) {
        let [a, b] = fields.as_slice() else {
            return Err("expected source; mirror in BidiMirroring.txt".into());
        };
        pairs.push((codepoint(a)?, codepoint(b)?));
    }
    pairs.sort_unstable();
    if pairs.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        return Err("duplicate Bidi_Mirroring_Glyph source".into());
    }
    let mut out = header();
    out.push_str(
        "//     targo --unverified run -p aterm-grapheme --example unicode_tables -- \\\n\
//         mirroring <BidiMirroring.txt> crates/aterm-bidi/src/mirroring.rs\n",
    );
    out.push_str(LICENSE);
    writeln!(
        out,
        "/// Every Bidi_Mirroring_Glyph pair of BidiMirroring-{version}.txt, `source, mirror`"
    )
    .unwrap();
    out.push_str(
        "/// flattened and sorted by source: a source is at an even index, its mirror\n\
/// right after it.\n",
    );
    writeln!(
        out,
        "pub(crate) static MIRROR_PAIRS: [u32; {}] = [",
        pairs.len() * 2
    )
    .unwrap();
    for row in pairs.chunks(5) {
        out.push_str("    ");
        for (index, (a, b)) in row.iter().enumerate() {
            if index > 0 {
                out.push(' ');
            }
            write!(out, "0x{a:05X}, 0x{b:05X},").unwrap();
        }
        out.push('\n');
    }
    out.push_str("];\n");
    Ok(out)
}

fn property_values(
    text: &str,
    names: &BTreeMap<String, u8>,
    default: u8,
) -> Result<Vec<u8>, String> {
    let mut values = vec![default; LIMIT];
    for fields in records(text) {
        let [range, name] = fields.as_slice() else {
            return Err("expected range; property value".into());
        };
        let value = names
            .get(*name)
            .ok_or_else(|| format!("unknown property value: {name}"))?;
        values[span(range)?].fill(*value);
    }
    Ok(values)
}

fn packed(out: &mut String, name: &str, doc: &str, values: &[u8]) {
    let runs: Vec<u32> = values
        .iter()
        .enumerate()
        .filter(|&(index, value)| index == 0 || *value != values[index - 1])
        .map(|(index, &value)| ((index as u32) << 8) | u32::from(value))
        .collect();
    writeln!(
        out,
        "{doc}\npub(crate) static {name}: [u32; {}] = [",
        runs.len()
    )
    .unwrap();
    for row in runs.chunks(8) {
        out.push_str("    ");
        for (index, entry) in row.iter().enumerate() {
            if index > 0 {
                out.push(' ');
            }
            write!(out, "0x{entry:08X},").unwrap();
        }
        out.push('\n');
    }
    out.push_str("];\n");
}

fn props(scripts: &str, categories: &str, aliases: &str) -> Result<String, String> {
    let ucd_version = version(scripts, "Scripts")?;
    if version(categories, "DerivedGeneralCategory")? != ucd_version
        || version(aliases, "PropertyValueAliases")? != ucd_version
    {
        return Err("Unicode input versions differ".into());
    }
    let mut iso = BTreeMap::from([("Unknown".to_owned(), "Zzzz".to_owned())]);
    for fields in records(aliases) {
        if let ["sc", code, name, ..] = fields.as_slice() {
            if code.len() != 4 || !code.bytes().all(|byte| byte.is_ascii_alphabetic()) {
                return Err(format!("invalid ISO 15924 code: {code}"));
            }
            iso.insert((*name).to_owned(), (*code).to_owned());
        }
    }
    let mut names = BTreeSet::from(["Unknown".to_owned()]);
    for fields in records(scripts) {
        let [_, name] = fields.as_slice() else {
            return Err("expected range; script".into());
        };
        if !iso.contains_key(*name) {
            return Err(format!("no ISO 15924 alias for {name}"));
        }
        names.insert((*name).to_owned());
    }
    let mut names: Vec<_> = names.into_iter().collect();
    names.sort_by_key(|name| &iso[name]);
    let indices: BTreeMap<String, u8> = names
        .iter()
        .enumerate()
        .map(|(index, name)| {
            u8::try_from(index)
                .map(|index| (name.clone(), index))
                .map_err(|_| "more than 256 scripts")
        })
        .collect::<Result<_, _>>()?;
    let script_values = property_values(scripts, &indices, indices["Unknown"])?;
    let category_indices = GC
        .iter()
        .enumerate()
        .map(|(index, &name)| (name.to_owned(), index as u8))
        .collect();
    let category_values = property_values(categories, &category_indices, 29)?;
    let mut out = header();
    out.push_str(
        "//     targo --unverified run -p aterm-grapheme --example unicode_tables -- \\\n\
//         props <Scripts.txt> <DerivedGeneralCategory.txt> <PropertyValueAliases.txt> \\\n\
//         crates/aterm-grapheme/src/tables/props.rs\n",
    );
    out.push_str(LICENSE);
    out.push_str(
        "//! The `Script` and `General_Category` properties as run tables: each entry is\n\
//! `start << 8 | value` for a run of code points that starts at `start` and\n\
//! ends where the next entry starts (the last runs to U+10FFFF), sorted, so a\n\
//! lookup is a binary search for the last start at or below the code point.\n\n",
    );
    out.push_str("/// The Unicode version these tables were generated from.\n");
    let numbers = ucd_version.split('.').collect::<Vec<_>>().join(", ");
    writeln!(
        out,
        "pub const UNICODE_VERSION: (u8, u8, u8) = ({numbers});\n"
    )
    .unwrap();
    writeln!(
        out,
        "/// The ISO 15924 code of every script Scripts-{ucd_version}.txt names, plus `Zzzz`;"
    )
    .unwrap();
    out.push_str("/// a script run's value indexes this list.\n");
    writeln!(
        out,
        "pub(crate) static SCRIPT_CODES: [[u8; 4]; {}] = [",
        names.len()
    )
    .unwrap();
    for row in names.chunks(9) {
        out.push_str("    ");
        for (index, name) in row.iter().enumerate() {
            if index > 0 {
                out.push(' ');
            }
            write!(out, "*b\"{}\",", iso[name]).unwrap();
        }
        out.push('\n');
    }
    out.push_str("];\n\n");
    packed(
        &mut out,
        "SCRIPT_RUNS",
        &format!(
            "/// `Script` runs (Scripts-{ucd_version}.txt): the value indexes [`SCRIPT_CODES`]."
        ),
        &script_values,
    );
    out.push('\n');
    packed(
        &mut out,
        "GENERAL_CATEGORY_RUNS",
        &format!(
            "/// `General_Category` runs (DerivedGeneralCategory-{ucd_version}.txt): the value is a\n/// `GeneralCategory` discriminant."
        ),
        &category_values,
    );
    Ok(out)
}

fn read(path: &std::ffi::OsStr) -> std::io::Result<String> {
    std::fs::read_to_string(Path::new(path))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let (out, destination) = match args.as_slice() {
        [mode, source, destination] if mode == "mirroring" => {
            (mirroring(&read(source)?)?, destination)
        }
        [mode, scripts, categories, aliases, destination] if mode == "props" => (
            props(&read(scripts)?, &read(categories)?, &read(aliases)?)?,
            destination,
        ),
        _ => return Err(USAGE.into()),
    };
    std::fs::write(Path::new(destination), aterm_census::seal::stamp(&out)?)?;
    Ok(())
}
