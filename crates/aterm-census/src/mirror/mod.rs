// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The mirror-proof registry: which aterm items Clean proved through a MIRROR harness (a copy
//! of aterm code in Clean's tree, proved against a SPECS.md clause), and what the code and the
//! clause were when it did.
//!
//! A mirror proof is about a copy, so it says nothing once aterm's code or the clause moves on.
//! [`REGISTRY`] records each proved row with the items its harness copied (the function and
//! every constant, enum or type it reads, as the harness's `// aterm:` header lists them), the
//! digest of each item's tokens ([`item::item_tokens`]: a reformat or a comment edit changes
//! nothing, a changed operator, literal, attribute or variant order does) and the digest of the
//! clause ([`specs::Clause::digest`]). The `mirror-rows` guard recomputes them from the
//! working tree and holds each SPECS.md note to the row it names.
//!
//! The registry is generated (sealed with [`crate::seal`]) by `aterm-census mirror`:
//!
//! ```text
//! aterm-census mirror record --clean R213 --row log_record_cap --crate aterm-log \
//!     --clause record-cap-by-level --harness d2b_rows/log_record_cap.rs --aterm 7543390498 \
//!     --vf5 <Clean's …/vf5_f1 directory> [--source <checkout at 7543390498>]
//! aterm-census mirror reproved --clean R230 --aterm <sha> [--source <tree>] <row>...
//! aterm-census mirror forget <row>...
//! aterm-census mirror audit --vf5 <Clean's …/vf5_f1 directory>
//! ```
//!
//! `record` adds or replaces one row, its items read from its harness's `// aterm:` header
//! (`--vf5`) or given (`--items 'file#path; …'`, for a harness with no header); `reproved`
//! re-records the rows a new Clean entry proved again. Both read the code from a git checkout
//! whose HEAD is the `--aterm` commit (this one, or `--source`), and refuse any other, so the
//! recorded commit is the tree the digests came from (its uncommitted edits aside: record from
//! a clean checkout). Nothing re-records a row on its own: a digest moves only when someone
//! names the row as proved again. `audit` holds the registry to Clean's own record
//! ([`clean`]).

pub mod clean;
pub mod item;
pub mod specs;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::seal;
pub use specs::{CLAIM, Clause, Note, STALE, clauses, notes};

/// The registry, root-relative.
pub const REGISTRY: &str = "crates/aterm-release/trust/mirror-rows.tsv";

/// The clause field of a row whose items no SPECS.md clause row states (its clause is prose,
/// or a plan row): the guard checks its items alone.
pub const NO_CLAUSE: &str = "-";

/// The registry's column header.
const COLUMNS: &str = "clean\trow\tcrate\tclause\taterm\tharness\titems\tdigests\tclause_digest";

/// One item a harness copied: its root-relative file and its path in that file.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Item {
    /// Root-relative (`crates/aterm-log/src/lib.rs`).
    pub file: String,
    /// `name`, `Type::name`, `module::name` or `<Type as Trait>::name`.
    pub path: String,
}

impl Item {
    /// `file#path`.
    ///
    /// # Errors
    /// When there is no `#`, or either side is empty.
    pub fn parse(text: &str) -> Result<Self, String> {
        match text.trim().split_once('#') {
            Some((file, path)) if !file.is_empty() && !path.is_empty() => Ok(Self {
                file: file.to_string(),
                path: path.to_string(),
            }),
            _ => Err(format!("item `{text}` is not `<file>#<path>`")),
        }
    }

    /// `file#path`.
    #[must_use]
    pub fn render(&self) -> String {
        format!("{}#{}", self.file, self.path)
    }
}

/// One proved row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// The Clean DECISIONS entry that proved it (`R213`).
    pub clean: String,
    /// The row's name in Clean's `rows.out.tsv`.
    pub row: String,
    /// The aterm package whose SPECS.md holds the clause.
    pub krate: String,
    /// The SPECS.md clause id, or [`NO_CLAUSE`].
    pub clause: String,
    /// The aterm commit the harness copied the items from.
    pub aterm: String,
    /// The harness, relative to Clean's VF5 F1 `blobs/`.
    pub harness: String,
    /// The items the harness copied.
    pub items: Vec<Item>,
    /// [`item::item_tokens`]' digest of each item as proved, in `items` order.
    pub digests: Vec<String>,
    /// [`Clause::digest`] as proved; [`NO_CLAUSE`] when the row has none.
    pub clause_digest: String,
}

/// The registry's rows, in file order.
///
/// # Errors
/// A first row that is not the column header, a data line without the nine fields, an item
/// that is not `file#path`, digests that do not match the items one for one, or a row name
/// given twice.
pub fn parse(text: &str) -> Result<Vec<Row>, String> {
    let mut rows: Vec<Row> = Vec::new();
    let mut header = false;
    for (index, line) in text.lines().enumerate() {
        let n = index + 1;
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        if !header {
            if line != COLUMNS {
                return Err(format!(
                    "line {n}: the first row is not the `{COLUMNS}` header"
                ));
            }
            header = true;
            continue;
        }
        let f: Vec<&str> = line.split('\t').collect();
        let [
            clean,
            row,
            krate,
            clause,
            aterm,
            harness,
            items,
            digests,
            clause_digest,
        ] = f[..]
        else {
            return Err(format!(
                "line {n}: {} fields, not the 9 of `{COLUMNS}`",
                f.len()
            ));
        };
        let items = items
            .split(';')
            .map(Item::parse)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("line {n}: {e}"))?;
        let digests: Vec<String> = digests.split(';').map(|d| d.trim().to_string()).collect();
        if digests.len() != items.len() {
            return Err(format!(
                "line {n}: {} digests for {} items",
                digests.len(),
                items.len()
            ));
        }
        if rows.iter().any(|r| r.row == row) {
            return Err(format!("line {n}: row `{row}` twice"));
        }
        rows.push(Row {
            clean: clean.to_string(),
            row: row.to_string(),
            krate: krate.to_string(),
            clause: clause.to_string(),
            aterm: aterm.to_string(),
            harness: harness.to_string(),
            items,
            digests,
            clause_digest: clause_digest.to_string(),
        });
    }
    Ok(rows)
}

/// The sealed registry holding `rows`, sorted by package, clause and row.
#[must_use]
pub fn render(rows: &[Row]) -> String {
    let mut sorted: Vec<&Row> = rows.iter().collect();
    sorted.sort_by(|a, b| (&a.krate, &a.clause, &a.row).cmp(&(&b.krate, &b.clause, &b.row)));
    let mut out = String::new();
    out.push_str(
        "# Copyright 2026 Andrew Yates\n\
         # SPDX-License-Identifier: Apache-2.0\n",
    );
    out.push_str(&seal::line("#", "`aterm-census mirror`"));
    out.push('\n');
    out.push_str(
        "#\n\
         # aterm items Clean proved through a mirror harness (a copy of the code in Clean's\n\
         # tree), each row with the digest of every item its harness copied and of its\n\
         # SPECS.md clause as proved. `verify census` (mirror-rows) names a row whose items or\n\
         # clause changed since; `aterm-census mirror reproved` re-records a row Clean proved\n\
         # again, `aterm-census mirror audit` holds the rows to Clean's own record.\n",
    );
    out.push_str(COLUMNS);
    out.push('\n');
    for row in sorted {
        let items: Vec<String> = row.items.iter().map(Item::render).collect();
        let (items, digests) = (items.join("; "), row.digests.join("; "));
        let fields: [&str; 9] = [
            &row.clean,
            &row.row,
            &row.krate,
            &row.clause,
            &row.aterm,
            &row.harness,
            &items,
            &digests,
            &row.clause_digest,
        ];
        out.push_str(&fields.join("\t"));
        out.push('\n');
    }
    seal::stamp(&out).unwrap_or(out)
}

/// FNV-1a 64 of `text`, as 16 hex digits. The fold stays spelled out: to the Trust lane a call to
/// `aterm_hash::fnv1a_64` is an unproved callee. `a_mirrors_digest_is_aterm_hashs_fold` holds it to
/// that.
fn fnv(text: &str) -> String {
    let mut hash = aterm_hash::FNV64_OFFSET;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(aterm_hash::FNV64_PRIME);
    }
    format!("{hash:016x}")
}

/// The digest of the item `path` in `src`: [`fnv`] of [`item::item_tokens`].
///
/// # Errors
/// As [`item::item_tokens`].
pub fn item_digest(src: &str, path: &str) -> Result<String, String> {
    item::item_tokens(src, path).map(|t| fnv(&t))
}

/// The digest of the item `path` in an already-parsed file `parsed`.
///
/// # Errors
/// As [`item::item_tokens`].
pub fn item_digest_in(parsed: &item::ParsedFile, path: &str) -> Result<String, String> {
    parsed.item_tokens(path).map(|t| fnv(&t))
}

/// The directory (root-relative) of the package named `name`, from `manifests` (`(path of
/// Cargo.toml, its text)`).
pub fn package_dir<'a>(
    manifests: impl IntoIterator<Item = (&'a str, &'a str)>,
    name: &str,
) -> Option<String> {
    manifests.into_iter().find_map(|(rel, text)| {
        let mut in_package = false;
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                in_package = line == "[package]";
            } else if in_package
                && let Some(value) = line.strip_prefix("name")
                && let Some(value) = value.trim_start().strip_prefix('=')
                && value.trim().trim_matches('"') == name
            {
                return rel.strip_suffix("/Cargo.toml").map(ToString::to_string);
            }
        }
        None
    })
}

/// The `crates/*` and `apps/*` manifests of the tree at `root`.
fn read_manifests(root: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for top in ["crates", "apps"] {
        let Ok(entries) = std::fs::read_dir(root.join(top)) else {
            continue;
        };
        for entry in entries.flatten() {
            let rel = format!("{top}/{}/Cargo.toml", entry.file_name().to_string_lossy());
            if let Ok(text) = crate::tree::read_to_string(&root.join(&rel)) {
                out.push((rel, text));
            }
        }
    }
    out
}

/// What a row reads as in a tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reading {
    /// Each item's digest, in the row's order.
    pub digests: Vec<String>,
    /// The clause's digest, or [`NO_CLAUSE`].
    pub clause_digest: String,
    /// The clause's 1-based line in its SPECS.md, when the row has one.
    pub clause_line: Option<usize>,
}

/// `row` as `read` (root-relative path to text) finds it, `package` being the directory of
/// its crate.
///
/// # Errors
/// When an item's file or the item, the SPECS.md or the clause cannot be read, or the clause
/// id has more than one row.
pub fn reading(
    row: &Row,
    package: &str,
    read: &dyn Fn(&str) -> Option<String>,
) -> Result<Reading, String> {
    let mut digests = Vec::new();
    for item in &row.items {
        let src = read(&item.file).ok_or_else(|| format!("{} does not exist", item.file))?;
        digests.push(item_digest(&src, &item.path).map_err(|e| format!("{}: {e}", item.file))?);
    }
    if row.clause == NO_CLAUSE {
        return Ok(Reading {
            digests,
            clause_digest: NO_CLAUSE.to_string(),
            clause_line: None,
        });
    }
    let specs_path = format!("{package}/SPECS.md");
    let specs = read(&specs_path).ok_or_else(|| format!("{specs_path} does not exist"))?;
    let found: Vec<Clause<'_>> = clauses(&specs)
        .into_iter()
        .filter(|c| c.id == row.clause)
        .collect();
    match &found[..] {
        [clause] => Ok(Reading {
            digests,
            clause_digest: clause.digest(),
            clause_line: Some(clause.line),
        }),
        [] => Err(format!("{specs_path} has no clause `{}`", row.clause)),
        [_, again, ..] => Err(format!(
            "{specs_path}:{} gives clause `{}` a second row: one id, one clause",
            again.line, row.clause
        )),
    }
}

/// What moved in `row` since it was proved, in words, given what the tree reads as; empty
/// when nothing did.
#[must_use]
pub fn moved(row: &Row, now: &Reading) -> Vec<String> {
    let mut out = Vec::new();
    for ((item, was), is) in row.items.iter().zip(&row.digests).zip(&now.digests) {
        if was != is {
            out.push(format!("`{}` changed (proved {was}, now {is})", item.path));
        }
    }
    if now.clause_digest != row.clause_digest {
        out.push(format!(
            "clause `{}` changed (proved {}, now {})",
            row.clause, row.clause_digest, now.clause_digest
        ));
    }
    out
}

/// The commit the git checkout at `tree` has checked out, read from its files (a worktree's
/// `.git` file included, a ref resolved through the refs or `packed-refs`).
fn head_commit(tree: &Path) -> Result<String, String> {
    let dot = tree.join(".git");
    let gitdir = if dot.is_dir() {
        dot
    } else {
        let text = crate::tree::read_to_string(&dot)
            .map_err(|e| format!("{} is not a git checkout ({e})", tree.display()))?;
        let dir = text
            .trim()
            .strip_prefix("gitdir: ")
            .ok_or_else(|| format!("{}: no `gitdir:` line", dot.display()))?;
        tree.join(dir)
    };
    let common = crate::tree::read_to_string(&gitdir.join("commondir"))
        .map_or_else(|_| gitdir.clone(), |c| gitdir.join(c.trim()));
    let head = crate::tree::read_to_string(&gitdir.join("HEAD"))
        .map_err(|e| format!("{}: {e}", gitdir.join("HEAD").display()))?;
    let head = head.trim();
    let Some(name) = head.strip_prefix("ref: ") else {
        return Ok(head.to_string());
    };
    for dir in [&gitdir, &common] {
        if let Ok(sha) = crate::tree::read_to_string(&dir.join(name)) {
            return Ok(sha.trim().to_string());
        }
    }
    crate::tree::read_to_string(&common.join("packed-refs"))
        .ok()
        .and_then(|packed| {
            packed.lines().find_map(|l| {
                l.split_once(' ')
                    .filter(|(_, r)| *r == name)
                    .map(|(sha, _)| sha.to_string())
            })
        })
        .ok_or_else(|| format!("{}: cannot resolve `{name}`", tree.display()))
}

/// `aterm-census mirror …` over the checkout at `root`: the text to print, or why it failed.
///
/// # Errors
/// A usage error, an unreadable row, a source tree at another commit than `--aterm`, a
/// registry that cannot be read or written, or (`audit`) what the audit found.
pub fn cli(root: &Path, args: &[String]) -> Result<String, String> {
    let (verb, rest) = args.split_first().ok_or_else(usage)?;
    let mut flags: BTreeMap<&str, &str> = BTreeMap::new();
    let mut names: Vec<&str> = Vec::new();
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        if let Some(flag) = arg.strip_prefix("--") {
            let value = it.next().ok_or_else(|| format!("--{flag} takes a value"))?;
            flags.insert(flag, value);
        } else {
            names.push(arg);
        }
    }
    let path = root.join(REGISTRY);
    let mut rows = match crate::tree::read_to_string(&path) {
        Ok(text) => parse(&text).map_err(|e| format!("{REGISTRY}: {e}"))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(format!("cannot read {REGISTRY}: {e}")),
    };
    let need = |flag: &str| {
        flags
            .get(flag)
            .map(|v| (*v).to_string())
            .ok_or_else(|| format!("{verb} needs --{flag}"))
    };
    let source: PathBuf = flags
        .get("source")
        .map_or_else(|| root.to_path_buf(), PathBuf::from);
    let at_commit = |aterm: &str| -> Result<Vec<(String, String)>, String> {
        let head = head_commit(&source)?;
        if aterm.len() < 7 || !head.starts_with(aterm) {
            return Err(format!(
                "{} is at {head}, not --aterm {aterm}: read the code from a checkout of the \
                 commit the proof copied (`--source`)",
                source.display()
            ));
        }
        Ok(read_manifests(&source))
    };
    let read = |rel: &str| crate::tree::read_to_string(&source.join(rel)).ok();
    let mut log = String::new();
    match verb.as_str() {
        "record" => {
            let mut row = Row {
                clean: need("clean")?,
                row: need("row")?,
                krate: need("crate")?,
                clause: need("clause")?,
                aterm: need("aterm")?,
                harness: need("harness")?,
                items: Vec::new(),
                digests: Vec::new(),
                clause_digest: String::new(),
            };
            let manifests = at_commit(&row.aterm)?;
            row.items = match (flags.get("items"), flags.get("vf5")) {
                (Some(items), _) => items
                    .split(';')
                    .map(Item::parse)
                    .collect::<Result<_, _>>()?,
                (None, Some(vf5)) => header_of(Path::new(vf5), &row.harness, &read)?,
                (None, None) => {
                    return Err(
                        "record needs --items, or --vf5 to read them from the harness's header"
                            .to_string(),
                    );
                }
            };
            let package = package_in(&manifests, &row.krate)?;
            let now = reading(&row, &package, &read)?;
            (row.digests, row.clause_digest) = (now.digests, now.clause_digest);
            let _ = writeln!(
                log,
                "recorded {} ({} over aterm {})",
                row.row, row.clean, row.aterm
            );
            rows.retain(|r| r.row != row.row);
            rows.push(row);
        }
        "reproved" => {
            let (clean, aterm) = (need("clean")?, need("aterm")?);
            if names.is_empty() {
                return Err("reproved names the rows Clean proved again".to_string());
            }
            let manifests = at_commit(&aterm)?;
            for name in &names {
                let row = rows
                    .iter_mut()
                    .find(|r| r.row == *name)
                    .ok_or_else(|| format!("no row `{name}` in {REGISTRY}"))?;
                let now = reading(row, &package_in(&manifests, &row.krate)?, &read)?;
                (row.digests, row.clause_digest) = (now.digests, now.clause_digest);
                row.clean.clone_from(&clean);
                row.aterm.clone_from(&aterm);
                let _ = writeln!(log, "re-recorded {name} as {clean} over aterm {aterm}");
            }
        }
        "forget" => {
            for name in &names {
                let before = rows.len();
                rows.retain(|r| r.row != *name);
                if rows.len() == before {
                    return Err(format!("no row `{name}` in {REGISTRY}"));
                }
                let _ = writeln!(log, "forgot {name}");
            }
        }
        "audit" => {
            let (found, compared) = clean::audit(&rows, Path::new(&need("vf5")?));
            return if found.is_empty() {
                Ok(format!(
                    "audit: the {} rows agree with Clean's rows.out.tsv; {compared} of them \
                     watch the items their harness header names, {} have a harness with no \
                     header (their items unchecked)\n",
                    rows.len(),
                    rows.len() - compared
                ))
            } else {
                Err(format!("audit:\n  {}", found.join("\n  ")))
            };
        }
        _ => return Err(usage()),
    }
    let rendered = render(&rows);
    (|| -> std::io::Result<()> {
        use std::io::Write as _;
        let _ = std::fs::remove_file(&path);
        crate::tree::open_create_new(&path)?.write_all(rendered.as_bytes())
    })()
    .map_err(|e| format!("cannot write {REGISTRY}: {e}"))?;
    Ok(log)
}

/// The items the harness `harness` (under `vf5/blobs`) names in its `// aterm:` header, each
/// glob expanded to the items of its file (as `read` finds it) whose names it matches.
fn header_of(
    vf5: &Path,
    harness: &str,
    read: &dyn Fn(&str) -> Option<String>,
) -> Result<Vec<Item>, String> {
    let path = vf5.join("blobs").join(harness);
    let text =
        crate::tree::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let header = clean::header_items(&text)
        .ok_or_else(|| format!("{} has no `// aterm:` header: pass --items", path.display()))?;
    let mut items = Vec::new();
    for want in header {
        let item = Item::parse(&want)?;
        let Some(prefix) = item.path.strip_suffix('*') else {
            items.push(item);
            continue;
        };
        let src = read(&item.file).ok_or_else(|| format!("{} does not exist", item.file))?;
        let matched: Vec<String> = item::item_names(&src)
            .into_iter()
            .filter(|n| n.starts_with(prefix))
            .collect();
        if matched.is_empty() {
            return Err(format!("`{want}` matches no item of {}", item.file));
        }
        items.extend(matched.into_iter().map(|path| Item {
            file: item.file.clone(),
            path,
        }));
    }
    Ok(items)
}

fn package_in(manifests: &[(String, String)], name: &str) -> Result<String, String> {
    package_dir(
        manifests.iter().map(|(r, t)| (r.as_str(), t.as_str())),
        name,
    )
    .ok_or_else(|| format!("no package named `{name}` under crates/ or apps/"))
}

fn usage() -> String {
    "usage: aterm-census mirror record --clean <R> --row <row> --crate <package> \
     --clause <id|-> --harness <blobs path> (--items '<file>#<path>; …' | --vf5 <dir>) \
     --aterm <sha> [--source <checkout>]\n\
     \x20      aterm-census mirror reproved --clean <R> --aterm <sha> [--source <checkout>] \
     <row>...\n\
     \x20      aterm-census mirror forget <row>...\n\
     \x20      aterm-census mirror audit --vf5 <Clean's …/vf5_f1 directory>\n"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mirror table holds these digests, so the fold must not move.
    #[test]
    fn a_mirrors_digest_is_aterm_hashs_fold() {
        assert_eq!(fnv(""), format!("{:016x}", aterm_hash::FNV64_OFFSET));
        assert_eq!(fnv("foobar"), "85944171f73967e8");
        assert_eq!(
            fnv("foobar"),
            format!("{:016x}", aterm_hash::fnv1a_64(b"foobar"))
        );
    }

    fn row() -> Row {
        Row {
            clean: "R1".into(),
            row: "r".into(),
            krate: "k".into(),
            clause: NO_CLAUSE.into(),
            aterm: "abc".into(),
            harness: "d2b_rows/r.rs".into(),
            items: vec![
                Item::parse("crates/k/src/lib.rs#f").expect("an item"),
                Item::parse("crates/k/src/lib.rs#<A as From<bool>>::from").expect("an item"),
            ],
            digests: vec!["0123456789abcdef".into(), "fedcba9876543210".into()],
            clause_digest: NO_CLAUSE.into(),
        }
    }

    #[test]
    fn the_registry_round_trips_sealed() {
        let text = render(&[row()]);
        assert!(matches!(seal::check(&text), seal::Seal::Holds { .. }));
        assert_eq!(parse(&text), Ok(vec![row()]));
    }

    #[test]
    fn a_malformed_registry_is_refused() {
        let text = render(&[row()]);
        let line = text.lines().last().expect("a row");
        let headless = text.replace(&format!("{COLUMNS}\n"), "");
        assert!(parse(&headless).is_err(), "no header");
        assert!(parse(&format!("{text}{line}\n")).is_err(), "a row twice");
        let short = text.replace("; fedcba9876543210", "");
        assert!(parse(&short).is_err(), "one digest for two items");
        assert!(
            parse(&text.replace("lib.rs#f", "lib.rs f")).is_err(),
            "an item with no #"
        );
    }

    #[test]
    fn a_package_is_found_by_its_name() {
        let manifests = [
            ("crates/x/Cargo.toml", "[package]\nname = \"aterm-x\"\n"),
            (
                "crates/y/Cargo.toml",
                "[dependencies]\nname = \"aterm-y\"\n",
            ),
        ];
        assert_eq!(
            package_dir(manifests, "aterm-x").as_deref(),
            Some("crates/x")
        );
        assert_eq!(package_dir(manifests, "aterm-y"), None);
    }

    #[test]
    fn the_source_must_be_the_commit_recorded() {
        let dir = std::env::temp_dir().join(format!("aterm-census-mirror-{}", std::process::id()));
        let git = dir.join(".git");
        std::fs::create_dir_all(git.join("refs/heads")).expect("a scratch repo");
        std::fs::write(git.join("HEAD"), "ref: refs/heads/main\n").expect("HEAD");
        let sha = "0123456789abcdef0123456789abcdef01234567";
        std::fs::write(
            git.join("packed-refs"),
            format!("# pack\n{sha} refs/heads/main\n"),
        )
        .expect("packed-refs");
        assert_eq!(head_commit(&dir).as_deref(), Ok(sha));
        std::fs::write(git.join("refs/heads/main"), "fedcba\n").expect("a loose ref");
        assert_eq!(head_commit(&dir).as_deref(), Ok("fedcba"));
        let args: Vec<String> = [
            "reproved", "--clean", "R2", "--aterm", "0123456", "--source",
        ]
        .iter()
        .map(ToString::to_string)
        .chain([dir.display().to_string(), "r".to_string()])
        .collect();
        let refused = cli(&dir, &args).expect_err("HEAD is fedcba, not 0123456");
        assert!(refused.contains("not --aterm 0123456"), "{refused}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
