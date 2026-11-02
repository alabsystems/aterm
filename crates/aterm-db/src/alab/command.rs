// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm db alab …` (docs/DESIGN-alab-lab-model-2026-11-01.md §8, §9; step 6): `import`,
//! `check` and `adopt` over `alab/registry`. Every report names keys, columns, counts and
//! source coordinates, never a value; addresses are masked unless `--show-addresses` is given.
//! The import's review (`--review`: a private `$TMPDIR/.ubd/alab-import-*.noindex/` folder opened
//! in amacs and removed once the owner closes it) is the only place a note's text or a losing
//! value is shown, and only on this Mac; a cell shaped like a credential, or one that names a
//! secret, is never shown even there.

use super::{
    Registry, checks,
    import::{
        self, Coord, Finding, Input, KnownMac, MailboxKeys, Plan, Sheet, SheetKind, Step, Vendors,
    },
    registry::{MAILBOX, REGISTRY},
};
use crate::{
    Error, Result,
    att::{self, Handle},
    machines::Label,
    typed::{Ty, check, grid, guard},
};
use aterm_json::{Map, Value, json};
use aterm_secrets::client::Paths;
use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
    process::ExitCode,
};

/// The verbs' help.
pub const USAGE: &str = "aterm db alab — the ALab lab model on Commons (Unix)\n\
  import [--vault-sheet <csv>] [--logins <csv>] [--dry-run|--apply [--first]] [--review]\n\
         [--findings] [--show-addresses]\n\
                    plan the account sheets as registry writes: a dry run unless --apply.\n\
                    Only empty cells, or cells an import wrote, are written; the owner's edits\n\
                    stay and show as findings. Notes and free text never cross: their\n\
                    coordinates do. --review opens a private review in amacs that shows them\n\
                    (never a credential-shaped cell) and waits; it is removed once closed.\n\
                    --apply waits until this Mac holds the registry (a peer's repair round\n\
                    brings it); --first writes where no Mac in the lab holds it yet\n\
  check             seats, labels, logins and mailboxes, then every conflict, dangling\n\
                    reference and unique clash\n\
  adopt [--dry-run|--apply]\n\
                    bind the Macs the vault's swarm roster names to their machine rows, by fleet\n\
                    node, host, then label or alias; never births a row\n\
  The sheets default to ~/.secrets/ai-accounts.csv and ~/ai-account-logins.csv, each read when\n\
  present. Addresses are masked unless --show-addresses is given.\n";

/// Where the importing Mac keeps its mailbox keys: private, beside its vault state.
pub const MAILBOX_KEYS: &str = "alab-mailbox-keys.json";

/// Opens a review file for the owner and returns once they have closed it: the review folder
/// lives only that long.
pub type Review<'a> = &'a mut dyn FnMut(&Path) -> Result<()>;

/// The production review opener: `aterm edit --wait <file>` from this same executable
/// ([`att::amacs`]), which inside aterm opens amacs in an Editor tab beside this one and
/// returns when the tab closes.
///
/// # Errors
/// This executable cannot be found or started, or amacs did not open the file.
pub fn amacs_wait(file: &Path) -> Result<()> {
    att::amacs(file, 1)
}

/// Run `aterm db alab <words>` on this Mac's Commons. `vendors` is the harness's reading of a
/// provider cell, which only the `aterm` executable links; without it `import` refuses.
pub fn main_entry(words: &[String], vendors: Option<&dyn Vendors>) -> ExitCode {
    if words.is_empty() || words.iter().any(|w| w == "--help" || w == "-h") {
        return aterm_uds::stdout::answer_exit("aterm db alab", USAGE, 0);
    }
    let paths = match Paths::live() {
        Ok(paths) => paths,
        Err(_) => {
            let _ = writeln!(std::io::stderr(), "aterm db alab: no vault paths here");
            return ExitCode::from(1);
        }
    };
    let now_ms = aterm_time::unix_millis().unwrap_or(0);
    match run(&paths, words, vendors, &mut amacs_wait, now_ms) {
        Ok(value) => match aterm_json::to_string(&value) {
            Ok(text) => aterm_uds::stdout::answer_exit("aterm db alab", &format!("{text}\n"), 0),
            Err(_) => ExitCode::from(1),
        },
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "aterm db alab: {error}");
            ExitCode::from(1)
        }
    }
}

#[derive(Default)]
struct Args {
    apply: bool,
    first: bool,
    dry_run: bool,
    review: bool,
    findings: bool,
    reveal: bool,
    vault: Option<PathBuf>,
    logins: Option<PathBuf>,
}

fn parse(verb: &str, words: &[String]) -> Result<Args> {
    let mut args = Args::default();
    let mut words = words.iter();
    while let Some(word) = words.next() {
        let mut value = |name: &str| {
            words
                .next()
                .map(PathBuf::from)
                .ok_or_else(|| Error::new(&format!("{name} needs a value")))
        };
        match (verb, word.as_str()) {
            ("import" | "adopt", "--apply") => args.apply = true,
            ("import", "--first") => args.first = true,
            ("import" | "adopt", "--dry-run") => args.dry_run = true,
            ("import", "--review") => args.review = true,
            ("import", "--no-review") => args.review = false,
            ("import" | "adopt", "--findings") => args.findings = true,
            (_, "--show-addresses") => args.reveal = true,
            ("import", "--vault-sheet") => args.vault = Some(value("--vault-sheet")?),
            ("import", "--logins") => args.logins = Some(value("--logins")?),
            _ => {
                return Err(Error::new(&format!(
                    "{verb}: unknown word {} (see `aterm db alab --help`)",
                    if word.starts_with("--") {
                        word.as_str()
                    } else {
                        "<operand>"
                    }
                )));
            }
        }
    }
    if args.apply && args.dry_run {
        return Err(Error::new(&format!(
            "{verb}: --apply and --dry-run are opposites"
        )));
    }
    if args.first && !args.apply {
        return Err(Error::new(&format!("{verb}: --first goes with --apply")));
    }
    Ok(args)
}

/// Run one verb against the Commons at `paths`, reading `now_ms` as this Mac's clock.
///
/// # Errors
/// The words are not a verb, a sheet cannot be read, or the registry cannot be read or written.
pub fn run(
    paths: &Paths,
    words: &[String],
    vendors: Option<&dyn Vendors>,
    review: Review<'_>,
    now_ms: u64,
) -> Result<Value> {
    let verb = words
        .first()
        .map(String::as_str)
        .ok_or_else(|| Error::new("missing alab verb"))?;
    let args = parse(verb, &words[1..])?;
    match verb {
        "import" => {
            let vendors = vendors.ok_or_else(|| {
                Error::new("the importer reads the sheets the harness's way; run it as `aterm db alab import`")
            })?;
            import(paths, &args, vendors, review, now_ms)
        }
        "check" => {
            let (view, usage) = Handle::at(paths.clone(), &att::REGISTRY).usage()?;
            let (lab, generic) = checks::lab(&view.cells);
            let table = att::REGISTRY.table;
            let mut findings: Vec<Value> = generic.iter().map(generic_json).collect();
            findings.extend(
                att::capacity(table, &view, &usage)
                    .iter()
                    .map(|found| att::command::capacity_json(table, found, &usage.actor)),
            );
            Ok(json!({
                "complete": view.complete,
                "refused": view.refused.iter().map(|id| aterm_codec::hex::encode_lower(id)).collect::<Vec<_>>(),
                "lab": lab.iter().map(|f| json!({"finding": f.kind, "collection": f.collection, "pks": f.pks})).collect::<Vec<_>>(),
                "findings": findings,
            }))
        }
        "adopt" => adopt(paths, &args, now_ms),
        other => Err(Error::new(&format!(
            "unknown alab verb {other} (see `aterm db alab --help`)"
        ))),
    }
}

fn generic_json(finding: &check::Finding) -> Value {
    match finding {
        check::Finding::Conflict {
            collection,
            pk,
            column,
        } => {
            json!({"finding": "conflict", "collection": collection, "pk": pk, "column": column})
        }
        check::Finding::Dangling {
            collection,
            pk,
            column,
            target,
        } => {
            json!({"finding": "dangling", "collection": collection, "pk": pk, "column": column, "target": target})
        }
        check::Finding::Unique {
            collection,
            columns,
            pks,
        } => {
            json!({"finding": "unique", "collection": collection, "columns": columns, "pks": pks})
        }
        check::Finding::Raw {
            collection,
            pk,
            column,
        } => {
            json!({"finding": "raw", "collection": collection, "pk": pk, "column": column})
        }
    }
}

/// The sheet at `explicit`, or the default file under `home` when it exists.
fn sheet(kind: SheetKind, explicit: Option<&Path>, default: PathBuf) -> Result<Option<Sheet>> {
    let path = match explicit {
        Some(path) => path.to_path_buf(),
        None if default.is_file() => default,
        None => return Ok(None),
    };
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| Error::new("a sheet's file name is not UTF-8"))?;
    let bytes = crate::import::read_bounded(&path)?;
    Sheet::parse(kind, name, &bytes).map(Some)
}

/// The Macs the vault's swarm roster names: label, aliases and fleet node.
fn known_macs(paths: &Paths) -> Vec<KnownMac> {
    let Ok(Some(roster)) = aterm_secrets::client::swarm_roster(paths) else {
        return Vec::new();
    };
    roster
        .members
        .iter()
        .map(|member| KnownMac {
            label: Label::parse(&member.label).ok(),
            aliases: member
                .aliases
                .iter()
                .filter_map(|a| Label::parse(a).ok())
                .collect(),
            node: member
                .node_id
                .clone()
                .filter(|node| aterm_codec::hex::is_lower(node, 32)),
            hosts: Vec::new(),
        })
        .collect()
}

fn keys_path(paths: &Paths) -> PathBuf {
    paths.dir.join(MAILBOX_KEYS)
}

fn read_keys(paths: &Paths) -> Result<MailboxKeys> {
    match std::fs::read(keys_path(paths)) {
        Ok(bytes) => {
            let value = aterm_json::from_slice(&bytes)
                .map_err(|_| Error::new("the mailbox key file is not JSON"))?;
            MailboxKeys::from_json(&value)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(MailboxKeys::default()),
        Err(_) => Err(Error::new("the mailbox key file cannot be read")),
    }
}

fn save_keys(paths: &Paths, keys: &MailboxKeys) -> Result<()> {
    let text = aterm_json::to_vec(&keys.to_json())
        .map_err(|_| Error::new("the mailbox keys could not be encoded"))?;
    aterm_types::fs_restricted::ensure_private_dir(&paths.dir)
        .map_err(|_| Error::new("the vault state folder could not be made private"))?;
    aterm_uds::private_file::write_private(&paths.dir, MAILBOX_KEYS, &text)
        .map_err(|_| Error::new("the mailbox keys could not be saved"))
}

fn import(
    paths: &Paths,
    args: &Args,
    vendors: &dyn Vendors,
    review: Review<'_>,
    now_ms: u64,
) -> Result<Value> {
    let sheets: Vec<Sheet> = [
        sheet(
            SheetKind::Vault,
            args.vault.as_deref(),
            paths.home.join(".secrets").join("ai-accounts.csv"),
        )?,
        sheet(
            SheetKind::Logins,
            args.logins.as_deref(),
            paths.home.join("ai-account-logins.csv"),
        )?,
    ]
    .into_iter()
    .flatten()
    .collect();
    if sheets.is_empty() {
        return Err(Error::new(
            "no sheet to import: give --vault-sheet or --logins, or put the sheets in their places",
        ));
    }
    let macs = known_macs(paths);
    let keys = read_keys(paths)?;
    let input = Input {
        sheets: &sheets,
        macs: &macs,
        vendors,
        keys: keys.clone(),
        now_ms,
        births: true,
    };
    let handle = Handle::at(paths.clone(), &att::REGISTRY);
    if args.apply {
        // A Mac that holds none of the registry would plan every row as new, write concurrent
        // candidates over every owner edit and mint duplicate mailboxes. Checked before the
        // registry is declared here, and again under the writer.
        let unheld = || {
            Error::new(
                "this Mac holds none of the registry yet: import where it is held, let a peer's \
                 repair round bring it here, or pass --first where no Mac in the lab holds it",
            )
        };
        if !args.first && handle.view().map_or(true, |view| view.ops.is_empty()) {
            return Err(unheld());
        }
        // The compiled providers, CLIs and plans first, in records of their own under this
        // Mac's quota of automatic records, so every account's `provider` names a row. A seed
        // pass that cannot write (this Mac's automatic records are spent) leaves the import to
        // go ahead; any Mac's later pass fills the same absent cells.
        let seeded = Registry::at(paths.clone()).seed();
        let (plan, applied) = handle.commit_derived(true, |view| {
            if view.ops.is_empty() && !args.first {
                return Err(unheld());
            }
            let plan = import::plan(&input, &view.cells)?;
            // New mailbox keys beside mailbox rows this Mac has no keys for would be duplicate
            // mailboxes: those keys stay on the Mac that imported first (until step 11 keeps
            // them in the vault).
            let known = keys.pks();
            if plan.keys.len() > keys.len()
                && view
                    .project(&MAILBOX)
                    .rows
                    .keys()
                    .any(|pk| !known.contains(pk.as_str()))
            {
                return Err(Error::new(
                    "the registry holds mailboxes whose keys are not on this Mac: import on the \
                     Mac that imported them",
                ));
            }
            if plan.keys != keys {
                // Before any record names a minted mailbox key, so a re-run finds it.
                save_keys(paths, &plan.keys)?;
            }
            let rows = plan.rows.clone();
            Ok((plan, rows))
        })?;
        let mut report = summary(&plan, &sheets, &macs, "held", args.findings);
        insert(&mut report, "dry_run", json!(false));
        insert(&mut report, "records", json!(applied.records));
        insert(&mut report, "written", json!(applied.cells));
        insert(
            &mut report,
            "retried",
            json!(applied.operations.len().saturating_sub(applied.records)),
        );
        match seeded {
            Ok(seeded) => insert(&mut report, "seeds", json!(seeded.records)),
            Err(error) => insert(&mut report, "seeds", json!(error.to_string())),
        }
        return Ok(report);
    }
    let (cells, registry) = match handle.view() {
        Ok(view) if view.complete => (view.cells, "held"),
        Ok(view) => (view.cells, "incomplete here"),
        Err(_) => (Default::default(), "not held here"),
    };
    let plan = import::plan(&input, &cells)?;
    let mut report = summary(&plan, &sheets, &macs, registry, args.findings);
    insert(&mut report, "dry_run", json!(true));
    let records = crate::att::pack(plan.rows.clone())?.len();
    insert(&mut report, "records", json!(records));
    let seeds = crate::att::pack(super::seeds::rows_for(&cells)?)?.len();
    insert(&mut report, "seeds", json!(seeds));
    if args.review {
        // The folder lives while the owner looks: dropping it removes it, however the review
        // ends, so no note's text is left behind in a temporary folder.
        let folder = write_review(&plan, &sheets, args.reveal)?;
        for file in ["findings.tsv", "planned.tsv"] {
            review(&folder.path().join(file))?;
        }
    }
    insert(&mut report, "review", json!(args.review));
    Ok(report)
}

fn adopt(paths: &Paths, args: &Args, now_ms: u64) -> Result<Value> {
    struct NoSheets;
    impl Vendors for NoSheets {
        fn mark(&self, _: &str, _: &str, _: &str) -> import::Mark {
            import::Mark::Absent
        }
    }
    let macs = known_macs(paths);
    let input = Input {
        sheets: &[],
        macs: &macs,
        vendors: &NoSheets,
        keys: MailboxKeys::default(),
        now_ms,
        births: false,
    };
    let handle = Handle::at(paths.clone(), &att::REGISTRY);
    if args.apply {
        let (plan, applied) = handle.commit_derived(false, |view| {
            let plan = import::plan(&input, &view.cells)?;
            let rows = plan.rows.clone();
            Ok((plan, rows))
        })?;
        let mut report = summary(&plan, &[], &macs, "held", args.findings);
        insert(&mut report, "dry_run", json!(false));
        insert(&mut report, "records", json!(applied.records));
        insert(&mut report, "written", json!(applied.cells));
        return Ok(report);
    }
    let view = handle.view()?;
    let plan = import::plan(&input, &view.cells)?;
    let mut report = summary(&plan, &[], &macs, "held", args.findings);
    insert(&mut report, "dry_run", json!(true));
    insert(
        &mut report,
        "records",
        json!(crate::att::pack(plan.rows.clone())?.len()),
    );
    Ok(report)
}

fn insert(report: &mut Value, key: &str, value: Value) {
    if let Value::Object(object) = report {
        object.insert(key.to_owned(), value);
    }
}

fn at_json(at: &Coord, sheets: &[Sheet]) -> Value {
    match sheets.get(at.sheet) {
        Some(sheet) => json!({"file": sheet.name, "row": at.row, "col": at.col}),
        None => json!({"file": "swarm roster", "row": at.row}),
    }
}

/// Counts, finding kinds and sources: never a value.
fn summary(plan: &Plan, sheets: &[Sheet], macs: &[KnownMac], registry: &str, list: bool) -> Value {
    let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
    for finding in &plan.findings {
        *kinds.entry(finding.kind.word()).or_default() += 1;
    }
    let counts: Map = plan
        .counts
        .iter()
        .map(|(name, c)| {
            (
                (*name).to_owned(),
                json!({"rows": c.rows, "new": c.new_rows, "cells": c.cells,
                "unchanged": c.unchanged, "kept": c.kept}),
            )
        })
        .collect();
    let mut report = json!({
        "registry": registry,
        "sources": sheets.iter().map(|s| json!({"kind": s.kind.word(), "name": s.name, "rows": s.rows.len()})).collect::<Vec<_>>(),
        "known_macs": macs.len(),
        "counts": counts,
        "findings": kinds,
        "mailbox_keys": plan.keys.len(),
    });
    if list {
        let listed: Vec<Value> = plan
            .findings
            .iter()
            .map(|f: &Finding| {
                json!({"finding": f.kind.word(), "collection": f.collection, "pk": f.pk,
                    "column": f.column, "at": f.at.iter().map(|a| at_json(a, sheets)).collect::<Vec<_>>()})
            })
            .collect();
        insert(&mut report, "finding_list", json!(listed));
    }
    report
}

/// What the review shows in place of a cell shaped like a credential or naming a secret.
pub const WITHHELD: &str = "(withheld: shaped like a secret)";

/// A sheet cell for the local review: every word holding `@` masked unless `reveal`; a cell
/// shaped like a credential or naming a secret is never shown, not even here.
fn review_text(text: &str, reveal: bool) -> String {
    if guard::withheld(text) {
        return WITHHELD.to_owned();
    }
    let flat: String = text
        .chars()
        .map(|c| if c == '\t' || c.is_control() { ' ' } else { c })
        .collect();
    if reveal {
        return flat;
    }
    flat.split(' ')
        .map(|word| {
            if word.contains('@') {
                grid::mask(word)
            } else {
                word.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn tsv(rows: &[Vec<String>]) -> String {
    let mut out = String::new();
    for row in rows {
        let cells: Vec<String> = row
            .iter()
            .map(|c| {
                c.chars()
                    .map(|c| if c == '\t' || c.is_control() { ' ' } else { c })
                    .collect()
            })
            .collect();
        out.push_str(&cells.join("\t"));
        out.push('\n');
    }
    out
}

/// The private review folder, removed when the returned handle drops: `findings.tsv` (each
/// finding with the text at its coordinates, the only place that text is shown, never for a
/// credential-shaped cell) and `planned.tsv` (each register the import would write, leave or
/// find unchanged).
fn write_review(plan: &Plan, sheets: &[Sheet], reveal: bool) -> Result<aterm_tempfile::TempDir> {
    let folder = aterm_tempfile::Builder::new()
        .prefix("alab-import-")
        .suffix(".noindex")
        .tempdir()
        .map_err(|_| Error::new("the review's private folder could not be made"))?;
    let mut findings = vec![
        [
            "finding",
            "collection",
            "pk",
            "column",
            "file",
            "row",
            "col",
            "text",
        ]
        .map(str::to_owned)
        .to_vec(),
    ];
    for finding in &plan.findings {
        let base = [
            finding.kind.word().to_owned(),
            finding.collection.unwrap_or_default().to_owned(),
            finding.pk.clone().unwrap_or_default(),
            finding.column.clone().unwrap_or_default(),
        ];
        if finding.at.is_empty() {
            findings.push([base.to_vec(), vec![String::new(); 4]].concat());
        }
        for at in &finding.at {
            let (file, text) = match sheets.get(at.sheet) {
                // A credential-shaped cell is never read, so it is never shown either.
                Some(s) if finding.kind == import::Kind::CredentialShaped => {
                    (s.name.clone(), WITHHELD.to_owned())
                }
                Some(s) => (
                    s.name.clone(),
                    review_text(s.text(at).unwrap_or_default(), reveal),
                ),
                None if at.sheet == import::ROSTER => ("swarm roster".to_owned(), String::new()),
                None => (String::new(), String::new()),
            };
            findings.push(
                [
                    base.to_vec(),
                    vec![file, at.row.to_string(), at.col.to_string(), text],
                ]
                .concat(),
            );
        }
    }
    let mut planned = vec![
        ["collection", "pk", "column", "value", "step"]
            .map(str::to_owned)
            .to_vec(),
    ];
    for item in &plan.planned {
        let column = item.column.split('.').next().unwrap_or_default();
        let ty = REGISTRY
            .collection(item.collection)
            .and_then(|c| c.column(column))
            .map_or(Ty::Json(0), |c| c.ty);
        let shown = match (&ty, item.column.contains('.')) {
            (_, true) => item.column.split('.').nth(1).unwrap_or_default().to_owned(),
            (ty, false) => grid::show(ty, &item.x, reveal),
        };
        let step = match item.step {
            Step::Write => "write",
            Step::Unchanged => "unchanged",
            Step::Kept => "kept",
        };
        planned.push(vec![
            item.collection.to_owned(),
            item.pk.clone(),
            item.column.clone(),
            shown,
            step.to_owned(),
        ]);
    }
    for (name, rows) in [("findings.tsv", &findings), ("planned.tsv", &planned)] {
        aterm_uds::private_file::write_private(folder.path(), name, tsv(rows).as_bytes())
            .map_err(|_| Error::new("the review could not be written"))?;
    }
    Ok(folder)
}

#[cfg(test)]
mod tests;
