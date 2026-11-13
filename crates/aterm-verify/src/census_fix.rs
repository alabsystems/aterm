// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! `verify census --fix`: move the inline test modules a build without tests reads out of line
//! (`aterm_census::outline`), format what it wrote, then the census judges the result.

use std::path::Path;

use crate::Ctx;
use crate::ladder::Report;

/// Write every plan `aterm_census::outline` makes for the checkout's inline-test-module findings,
/// and format each file it wrote. A file it cannot write is left as it was and ends the run, as an
/// interrupt does between two files, after what was written before is formatted and reported.
pub(crate) fn run(ctx: &Ctx, r: &mut Report) {
    run_until(ctx, r, &crate::stopping);
}

/// [`run`], asking `stopping` before each file whether the run is ending.
fn run_until(ctx: &Ctx, r: &mut Report, stopping: &dyn Fn() -> bool) {
    let found = match aterm_census::unread_at(&ctx.root) {
        Ok(found) => found,
        Err(why) => {
            r.cannot_run(format!("census --fix — {why}"));
            return;
        }
    };
    let mut stayed = Vec::new();
    let mut written = Vec::new();
    let mut moved = 0usize;
    let mut parents = 0usize;
    let mut failed = None;
    let mut located = Vec::new();
    for entry in found {
        if stopping() {
            failed = Some("census --fix was interrupted".to_string());
            break;
        }
        let unread = match entry {
            Ok(unread) => unread,
            Err((rel, why)) => {
                stayed.push(format!("{rel}: {why}"));
                continue;
            }
        };
        if crate::fmt::EXCLUDED.contains(&unread.rel.as_str()) {
            stayed.push(format!(
                "{}: its layout is its content, so nothing moves it",
                unread.rel
            ));
            continue;
        }
        match apply(&ctx.root, &unread) {
            Ok((plan, files)) => {
                for name in &plan.located {
                    located.push(format!(
                        "{}: `{name}` moved; its body reads its own place (file!, line!, a caller \
                         Location), which now names its new file: run its tests",
                        unread.rel
                    ));
                }
                for (name, why) in &plan.refused {
                    stayed.push(if name.is_empty() {
                        format!("{}: {why}", unread.rel)
                    } else {
                        format!("{}: `{name}` stays inline: {why}", unread.rel)
                    });
                }
                if !files.is_empty() {
                    parents += 1;
                    moved += files.len();
                    written.push(unread.rel.clone());
                    written.extend(files);
                }
            }
            Err(why) => {
                failed = Some(format!("census --fix — {}: {why}", unread.rel));
                break;
            }
        }
    }
    if !located.is_empty() {
        r.raw(located.join("\n"));
    }
    if !stayed.is_empty() {
        r.raw(stayed.join("\n"));
    }
    if let Some(bin) = ctx.tools.trustfmt.clone() {
        let unread = crate::fmt::write(ctx, &bin, &written);
        if !unread.is_empty() {
            r.raw(unread.join("\n"));
            r.cannot_run("census --fix could not format every file it wrote (above)");
        }
    } else if !written.is_empty() {
        r.skip(format!(
            "census --fix formatting — {}",
            ctx.tools.missing_trustfmt_label()
        ));
    }
    let summary = format!(
        "census --fix: {moved} test module{} moved out of line from {parents} file{}; {} left \
         (above)",
        if moved == 1 { "" } else { "s" },
        if parents == 1 { "" } else { "s" },
        stayed.len()
    );
    match failed {
        Some(failed) => {
            r.raw(summary);
            r.cannot_run(failed);
        }
        None => r.pass(summary),
    }
}

/// Plan `unread`'s file and write the plan: its new files first, so the file that mounts them
/// never names one that is not there, then the file itself, replaced whole by a rename. Where a
/// write fails, the new files it made are removed and the file is as it was. Returns the plan and
/// the files written.
fn apply(
    root: &Path,
    unread: &aterm_census::Unread,
) -> Result<(aterm_census::outline::Plan, Vec<String>), String> {
    let path = root.join(&unread.rel);
    let text = std::fs::read_to_string(&path).map_err(|e| format!("cannot read it: {e}"))?;
    let plan = aterm_census::outline::plan(
        &unread.rel,
        &unread.package,
        &unread.owns_dir,
        &text,
        &|rel| root.join(rel).exists(),
    );
    let Some(parent) = &plan.parent else {
        return Ok((plan, Vec::new()));
    };
    let mut files: Vec<String> = Vec::new();
    let undo = |files: &[String]| {
        for rel in files {
            // Each is a file this call made new; one already gone leaves nothing to undo.
            drop(std::fs::remove_file(root.join(rel)));
        }
    };
    for (rel, body) in &plan.files {
        let at = root.join(rel);
        let made = at
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| write_new(&at, body));
        if let Err(e) = made {
            undo(&files);
            return Err(format!("cannot write {rel}: {e}"));
        }
        files.push(rel.clone());
    }
    if let Err(e) = replace(&path, parent) {
        undo(&files);
        return Err(format!("cannot rewrite it: {e}"));
    }
    Ok((plan, files))
}

/// Write `text` to a file at `at` that must not exist yet; one cut short is removed.
fn write_new(at: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(at)?;
    file.write_all(text.as_bytes()).inspect_err(|_| {
        drop(std::fs::remove_file(at));
    })
}

/// Replace the file at `at` with `text`, keeping its permissions: written beside it, then renamed
/// over it, so a reader sees the old text or the new, never part of one. The file beside it is
/// this tool's alone, so one a run killed before its rename left is written over.
fn replace(at: &Path, text: &str) -> std::io::Result<()> {
    let name = at
        .file_name()
        .ok_or_else(|| std::io::Error::other("no file name"))?;
    let mut next = name.to_os_string();
    next.push(".census-fix.noindex");
    let next = at.with_file_name(next);
    let kept = std::fs::metadata(at)?.permissions();
    match std::fs::remove_file(&next) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    write_new(&next, text)?;
    std::fs::set_permissions(&next, kept)
        .and_then(|()| std::fs::rename(&next, at))
        .inspect_err(|_| {
            drop(std::fs::remove_file(&next));
        })
}

#[cfg(test)]
#[path = "census_fix_tests.rs"]
mod tests;
