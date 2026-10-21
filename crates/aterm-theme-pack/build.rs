// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Embed the community theme pack (`contrib/themes/pack/`).
//!
//! Reads `manifest.tsv`, refuses a row whose licence the policy does not
//! admit, whose id the themes folder could not hold, whose id or name
//! another row answers to, or whose upstream or licence file is missing,
//! and writes `$OUT_DIR/pack.rs`: the rows as `PackTheme`s (sorted by id,
//! ignoring ASCII case), the withdrawn ids with the theme standing for each,
//! every licence text the rows, the collection and the built-in ports name,
//! and the collection's identity. A built-in port's licence file is derived from its source
//! (`LICENSES/<spdx>-<repo>.txt`, [`port_licence_file`]), so no second list
//! names them; and a file in the pack folder that nothing names refuses the
//! build (TP-FOLDER, TP-PORTS). File text is pulled in with `include_str!`, so the compiler
//! tracks it too. A row whose `<id>.conf` is not there yet gets empty text
//! and a warning: the bless step (`ATERM_BLESS_GOLDEN=1 targo --unverified
//! test -p aterm-theme-pack`) writes it.

#[path = "src/policy.rs"]
#[expect(
    dead_code,
    reason = "the build reads only the admission half of the policy"
)]
mod policy;

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// The manifest's header row, column for column.
const HEADER: [&str; 10] = [
    "id",
    "upstream",
    "appearance",
    "pair",
    "author",
    "project",
    "project_url",
    "licence",
    "licence_files",
    "name",
];

struct Row {
    line: usize,
    cols: Vec<String>,
}

/// The licence file of a built-in port whose source is `project`
/// (`owner/repo`) under `spdx`: `LICENSES/<spdx>-<repo>.txt`.
fn port_licence_file(project: &str, spdx: &str) -> String {
    let repo = project.rsplit('/').next().unwrap_or(project);
    format!("LICENSES/{spdx}-{repo}.txt")
}

/// The entries of the pack folder (one subfolder deep, relative to it) that
/// nothing names: a `.conf` with no row, an upstream file no row converts, a
/// licence file not in `licences`, or anything else but the README, the
/// manifest and ATTRIBUTION.md.
fn unlisted(pack: &Path, rows: &[Row], licences: &BTreeSet<String>) -> Vec<String> {
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(pack).unwrap_or_else(|e| panic!("{}: {e}", pack.display())) {
        let entry = entry.expect("a pack entry");
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.path().is_dir() {
            for inner in std::fs::read_dir(entry.path()).expect("a pack subfolder") {
                let inner = inner.expect("a pack entry");
                entries.push(format!("{name}/{}", inner.file_name().to_string_lossy()));
            }
        } else {
            entries.push(name);
        }
    }
    entries.sort();
    entries
        .into_iter()
        .filter(|entry| {
            let named = if let Some(stem) = entry.strip_suffix(".conf") {
                rows.iter().any(|row| row.cols[0] == stem)
            } else if let Some(file) = entry.strip_prefix("upstream/") {
                rows.iter().any(|row| row.cols[1] == file)
            } else if entry.starts_with("LICENSES/") {
                licences.contains(entry)
            } else {
                matches!(
                    entry.as_str(),
                    "README.md" | "manifest.tsv" | "ATTRIBUTION.md"
                )
            };
            !named
        })
        .collect()
}

fn main() {
    let manifest_dir =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets it"));
    // A directory literal (trailing '/'): aterm-verify seeds this crate from
    // any changed file beneath it.
    let pack = manifest_dir.join("../../contrib/themes/pack/");
    let pack = pack
        .canonicalize()
        .unwrap_or_else(|e| panic!("{}: {e}", pack.display()));
    println!("cargo:rerun-if-changed={}", pack.display());
    let manifest_path = pack.join("manifest.tsv");
    println!("cargo:rerun-if-changed={}", manifest_path.display());
    let manifest = std::fs::read_to_string(&manifest_path)
        .unwrap_or_else(|e| panic!("{}: {e}", manifest_path.display()));
    let at = |line: usize| format!("{}:{line}", manifest_path.display());

    let mut collection: Option<(String, String, String)> = None;
    let mut withdrawn: Vec<(usize, String, String)> = Vec::new();
    let mut header_seen = false;
    let mut rows: Vec<Row> = Vec::new();
    for (index, raw) in manifest.lines().enumerate() {
        let line = index + 1;
        if raw.trim().is_empty() || raw.starts_with('#') {
            continue;
        }
        let cols: Vec<String> = raw.split('\t').map(str::to_string).collect();
        if let Some(directive) = cols[0].strip_prefix('@') {
            match (directive, cols.len()) {
                ("collection", 4) if collection.is_none() => {
                    collection = Some((cols[1].clone(), cols[2].clone(), cols[3].clone()));
                }
                ("withdrawn", 3) => withdrawn.push((line, cols[1].clone(), cols[2].clone())),
                _ => panic!("{}: unknown or malformed directive {raw:?}", at(line)),
            }
            continue;
        }
        if !header_seen {
            assert_eq!(
                cols,
                HEADER,
                "{}: the header row must be {:?}",
                at(line),
                HEADER.join("\t")
            );
            header_seen = true;
            continue;
        }
        assert_eq!(
            cols.len(),
            HEADER.len(),
            "{}: a row has {} tab-separated columns",
            at(line),
            HEADER.len()
        );
        rows.push(Row { line, cols });
    }
    let (repo, commit, collection_licence) =
        collection.unwrap_or_else(|| panic!("{}: no @collection directive", at(0)));
    assert!(
        commit.len() == 40 && commit.bytes().all(|b| b.is_ascii_hexdigit()),
        "{}: the @collection commit must be a full sha, not {commit:?}",
        at(0)
    );

    let mut licence_files: BTreeSet<String> = BTreeSet::new();
    let checked_file = |name: &str, line: usize| -> PathBuf {
        let path = pack.join(name);
        assert!(
            !name.contains("..") && !name.starts_with('/') && path.is_file(),
            "{}: {name:?} is not a file in contrib/themes/pack/",
            at(line)
        );
        println!("cargo:rerun-if-changed={}", path.display());
        path
    };
    let mut licence = |name: &str, line: usize| {
        assert!(
            name.starts_with("LICENSES/"),
            "{}: a licence file lives in LICENSES/, not {name:?}",
            at(line)
        );
        checked_file(name, line);
        licence_files.insert(name.to_string());
    };
    licence(&collection_licence, 0);
    // Every built-in port's source names its licence file by rule; a
    // built-in without a source is one of aterm's own designs.
    let mut ports: Vec<(String, String)> = Vec::new();
    for builtin in aterm_types::scheme::builtin_theme_list() {
        let Some(source) = builtin.source else {
            assert!(
                builtin.signature || builtin.name.starts_with("Default"),
                "{} is a port with no source",
                builtin.name
            );
            continue;
        };
        assert!(
            policy::admitted(source.licence),
            "{}: its source {} is under {}, which the pack does not admit",
            builtin.name,
            source.project,
            source.licence
        );
        if ports.iter().any(|(project, _)| project == source.project) {
            continue;
        }
        let file = port_licence_file(source.project, source.licence);
        licence(&file, 0);
        ports.push((source.project.to_string(), file));
    }
    // What `find` answers to: every id, and every name the chooser shows.
    let mut ids = BTreeSet::new();
    for row in &rows {
        let c = &row.cols;
        let (id, line) = (&c[0], row.line);
        if let Err(why) = aterm_types::scheme::validate_user_theme_name(id) {
            panic!("{}: {id:?} cannot be a theme id: {why}", at(line));
        }
        assert!(
            ids.insert(id.to_ascii_lowercase()),
            "{}: {id:?} is listed twice",
            at(line)
        );
    }
    for row in &rows {
        let c = &row.cols;
        let (id, name, line) = (&c[0], &c[9], row.line);
        assert!(
            !name.trim().is_empty(),
            "{}: name is a name or '-'",
            at(line)
        );
        assert!(
            name == "-" || (name != id && ids.insert(name.to_ascii_lowercase())),
            "{}: {id:?}'s name {name:?} is its id (write '-') or another row's",
            at(line)
        );
    }
    for (line, id, theme) in &withdrawn {
        assert!(
            !ids.contains(&id.to_ascii_lowercase()),
            "{}: withdrawn {id:?} is still a row's id or name",
            at(*line)
        );
        assert!(
            rows.iter().any(|row| row.cols[0] == *theme)
                || aterm_types::scheme::builtin_theme(theme).is_some(),
            "{}: {theme:?}, standing for withdrawn {id:?}, is no pack id or built-in",
            at(*line)
        );
    }
    for row in &rows {
        let c = &row.cols;
        let (id, line) = (&c[0], row.line);
        assert!(
            matches!(c[2].as_str(), "dark" | "light"),
            "{}: appearance is dark or light, not {:?}",
            at(line),
            c[2]
        );
        assert!(
            policy::admitted(&c[7]),
            "{}: {id:?} is under {:?}, which the pack does not admit (admitted: {})",
            at(line),
            c[7],
            policy::ADMITTED_LICENCES.join(", ")
        );
        for (col, name) in [
            (1, "upstream"),
            (4, "author"),
            (5, "project"),
            (6, "project_url"),
        ] {
            assert!(!c[col].is_empty(), "{}: {name} is empty", at(line));
        }
        assert!(
            !c[1].contains('/'),
            "{}: upstream names a file in upstream/",
            at(line)
        );
        let upstream = format!("upstream/{}", c[1]);
        checked_file(&upstream, line);
        for file in c[8].split(';') {
            licence(file, line);
        }
    }
    for row in &rows {
        let pair = &row.cols[3];
        assert!(!pair.is_empty(), "{}: pair is a theme or '-'", at(row.line));
    }
    let unlisted = unlisted(&pack, &rows, &licence_files);
    assert!(
        unlisted.is_empty(),
        "contrib/themes/pack/ holds files nothing names (a row, the collection or a \
         built-in port): {unlisted:?}"
    );

    let mut out = String::new();
    let lit = |path: &Path| format!("include_str!({:?})", path.display().to_string());
    rows.sort_by(|a, b| {
        a.cols[0]
            .to_ascii_lowercase()
            .cmp(&b.cols[0].to_ascii_lowercase())
    });
    out.push_str("static THEMES: &[PackTheme] = &[\n");
    for row in &rows {
        let c = &row.cols;
        let conf = pack.join(format!("{}.conf", c[0]));
        let text = if conf.is_file() {
            println!("cargo:rerun-if-changed={}", conf.display());
            lit(&conf)
        } else {
            println!(
                "cargo:warning={}.conf is not generated yet: run ATERM_BLESS_GOLDEN=1 targo --unverified test -p aterm-theme-pack",
                c[0]
            );
            "\"\"".to_string()
        };
        let appearance = if c[2] == "light" { "Light" } else { "Dark" };
        let pair = if c[3] == "-" {
            "None".to_string()
        } else {
            format!("Some({:?})", c[3])
        };
        let files: Vec<String> = c[8].split(';').map(|f| format!("{f:?}")).collect();
        let name = if c[9] == "-" { &c[0] } else { &c[9] };
        let _ = writeln!(
            out,
            "    PackTheme {{ id: {:?}, name: {name:?}, text: {text}, upstream: {:?}, \
             appearance: aterm_types::Appearance::{appearance}, pair: {pair}, author: {:?}, \
             project: {:?}, project_url: {:?}, licence: {:?}, licence_files: &[{}] }},",
            c[0],
            c[1],
            c[4],
            c[5],
            c[6],
            c[7],
            files.join(", ")
        );
    }
    out.push_str("];\n\nstatic WITHDRAWN: &[(&str, &str)] = &[\n");
    for (_, id, theme) in &withdrawn {
        let _ = writeln!(out, "    ({id:?}, {theme:?}),");
    }
    out.push_str("];\n\nstatic LICENCES: &[(&str, &str)] = &[\n");
    for file in &licence_files {
        let _ = writeln!(out, "    ({file:?}, {}),", lit(&pack.join(file)));
    }
    out.push_str("];\n\nstatic PORTS: &[(&str, &str)] = &[\n");
    for (project, file) in &ports {
        let _ = writeln!(out, "    ({project:?}, {file:?}),");
    }
    let _ = writeln!(
        out,
        "];\n\nconst COLLECTION: Collection = Collection {{ repo: {repo:?}, commit: {commit:?}, \
         licence_file: {collection_licence:?} }};"
    );
    let dest = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets it")).join("pack.rs");
    std::fs::write(&dest, out).unwrap_or_else(|e| panic!("{}: {e}", dest.display()));
}
