// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A compile that stopped on the build directory, not on the source.
//!
//! Cargo starts a crate's dependents once the compiler has written the crate's metadata, before
//! the crate itself is done. A build ended there (an interrupted judgment's, a killed cargo's)
//! can leave a dependent finished against that early metadata while the crate is not: the next
//! build in the directory takes the dependent as fresh, compiles the crate again, and a unit that
//! links both stops on `error[E0460]: found possibly newer version of crate` (measured
//! 2026-11-13: an interrupted `verify land` judgment left `aterm_gui_fabric` finished and
//! `aterm_core` unfinished, and the next judgment's test compile stopped so). Such an error is a
//! state of the build directory and says nothing of the tree, so it is COULD NOT RUN
//! ([`crate::exec::Run::environment_failure`]), and the call that met it is made once more after
//! the files its notes name are removed ([`retried`]): Cargo then builds those units again, and
//! every unit that depends on them after them.

use std::path::{Path, PathBuf};

/// The line [`retried`] puts ahead of the second call's output, which says the retry was made.
pub const RETRIED: &str = "aterm-verify: stale build artifact:";

/// What a compiler said of a stale build artifact ([`find`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stale {
    /// Its first such error line, as it printed it.
    pub error: String,
    /// The artifact files the notes of every such error name, each once, in the order named.
    pub artifacts: Vec<PathBuf>,
}

/// Cargo's line for a unit whose compiler exited non-zero: without it the text is not a failed
/// compile's (a test may print a compiler's error it made up).
const NOT_COMPILED: &str = "error: could not compile `";

/// The stale-artifact error `output` holds (as text, or in Cargo's JSON compiler messages),
/// where it is a failed compile's: `E0460` (a crate
/// found in another version than a dependent was built against), `E0464` (several candidates
/// for one crate), `E0786` (a metadata file that does not read), or `E0463` for a crate another
/// crate depends on (a dependent's metadata names a crate whose file is gone). An `E0463` for a
/// crate the source names itself is the source's (an undeclared dependency, a target with no
/// standard library), and every other error is too: `None`.
#[must_use]
pub fn find(output: &str) -> Option<Stale> {
    let lines: Vec<String> = output.lines().flat_map(rendered).collect();
    if !lines
        .iter()
        .any(|line| line.trim_start().starts_with(NOT_COMPILED))
    {
        return None;
    }
    let mut found: Option<Stale> = None;
    let mut inside = false;
    for line in &lines {
        let text = line.trim_start();
        if text.starts_with("error") || text.starts_with("warning") {
            inside = names_stale(text);
            if inside && found.is_none() {
                found = Some(Stale {
                    error: text.trim_end().to_owned(),
                    artifacts: Vec::new(),
                });
            }
            continue;
        }
        if inside && let Some(stale) = found.as_mut() {
            for path in named_artifacts(text) {
                if !stale.artifacts.contains(&path) {
                    stale.artifacts.push(path);
                }
            }
        }
    }
    found
}

/// The lines of text one line of a compile's output says: itself without its colors, or, where
/// it is one of Cargo's JSON compiler messages (`--message-format=json`, which the cross cells
/// ask for), the lines of the diagnostic that message renders.
fn rendered(line: &str) -> Vec<String> {
    if line.starts_with('{')
        && line.contains("\"compiler-message\"")
        && let Ok(value) = aterm_json::from_str::<aterm_json::Value>(line)
        && let Some(text) = value
            .get("message")
            .and_then(|message| message.get("rendered"))
            .and_then(aterm_json::Value::as_str)
    {
        return text.lines().map(plain).collect();
    }
    vec![plain(line)]
}

/// Whether a diagnostic's first line is a stale-artifact error ([`find`]).
fn names_stale(head: &str) -> bool {
    let coded = |code: &str| {
        head.strip_prefix("error[")
            .and_then(|rest| rest.strip_prefix(code))
            .is_some_and(|rest| rest.starts_with(']'))
    };
    coded("E0460")
        || coded("E0464")
        || coded("E0786")
        || (coded("E0463") && head.contains("` which `") && head.trim_end().ends_with("depends on"))
        || head.contains("found possibly newer version of crate")
}

/// The artifact files one line of a diagnostic names: what follows the first `: ` that an
/// absolute path follows, to the line's end (`crate `a`: /path`, `candidate #1: /path`; on
/// Windows `candidate #1: C:\path`, whose drive's colon no space follows; a path may hold
/// spaces), and what stands in quotes, each an absolute path of a library or metadata file.
fn named_artifacts(line: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut keep = |text: &str| {
        let path = Path::new(text.trim());
        if path.is_absolute() && is_artifact(path) {
            out.push(path.to_path_buf());
        }
    };
    if let Some(rest) = line
        .match_indices(": ")
        .map(|(at, sep)| &line[at + sep.len()..])
        .find(|rest| Path::new(rest).is_absolute())
    {
        keep(rest);
    }
    for quote in ['\'', '`', '"'] {
        for (index, piece) in line.split(quote).enumerate() {
            if index % 2 == 1 {
                keep(piece);
            }
        }
    }
    out
}

/// Whether `path` is named as a compiled library or its metadata.
fn is_artifact(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| matches!(ext, "rlib" | "rmeta" | "dylib" | "so"))
}

/// `line` without the escape sequences a colored diagnostic carries.
fn plain(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        if chars.next() == Some('[') {
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
        }
    }
    out
}

impl Stale {
    /// Why the child that printed it is no finding about the tree, for its NOT RUN row:
    /// `retried` when the call had already been made again after its named files were removed.
    #[must_use]
    pub fn reason(&self, retried: bool) -> String {
        let named = if self.artifacts.is_empty() {
            "it names no artifact file".to_owned()
        } else {
            self.artifacts
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let again = if retried {
            ", again after the files it named were removed and the call was made once more"
        } else {
            ""
        };
        format!(
            "the compile stopped on a stale artifact in its build directory{again}, which says \
             nothing of the source: `{}` ({named})",
            self.error
        )
    }

    /// Remove the files it names that are a Cargo unit's library or metadata under one of
    /// `roots` (the run's build directories, canonical): what was removed. A file anywhere else
    /// (a toolchain's own library), a link, a directory and any other name stay.
    #[must_use]
    pub fn remove(&self, roots: &[PathBuf]) -> Vec<PathBuf> {
        self.artifacts
            .iter()
            .filter(|path| removable(path, roots) && std::fs::remove_file(path).is_ok())
            .cloned()
            .collect()
    }
}

/// Whether `path` is a regular file named as a Cargo unit's output (`<name>-<16 hex>.<ext>`) in
/// a directory under one of `roots`.
fn removable(path: &Path, roots: &[PathBuf]) -> bool {
    let unit = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .and_then(|stem| stem.rsplit_once('-'))
        .is_some_and(|(name, hash)| {
            !name.is_empty()
                && hash.len() == 16
                && hash
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        });
    unit && is_artifact(path)
        && std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_file())
        && path.parent().is_some_and(|dir| {
            let dir = aterm_uds::runs::canonical(dir);
            roots.iter().any(|root| dir.starts_with(root))
        })
}

/// Make `call`, and where its run ([`crate::exec::Run`], read through `run_of`) stopped on a
/// stale artifact ([`find`]) whose named files under `roots` could be removed
/// ([`Stale::remove`]), make it once more: the second call's result, its output led by a
/// [`RETRIED`] line that names the error and what was removed. A second such stop is that
/// result as it is, which its row reads as COULD NOT RUN naming the artifact
/// ([`Stale::reason`]). Any other failure, and a stale error that names nothing removable, is
/// the first call's result: nothing is run twice.
pub(crate) fn retried<T>(
    roots: &[PathBuf],
    mut call: impl FnMut() -> T,
    run_of: impl Fn(&mut T) -> &mut crate::exec::Run,
) -> T {
    let mut first = call();
    let run = run_of(&mut first);
    let Some(stale) = (!run.ok).then(|| find(&run.output)).flatten() else {
        return first;
    };
    let removed = stale.remove(roots);
    if removed.is_empty() {
        return first;
    }
    let note = format!(
        "{RETRIED} `{}`; removed {} and made the call once more",
        stale.error,
        removed
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    eprintln!("verify: {note}");
    let mut second = call();
    let run = run_of(&mut second);
    run.output = format!("{note}\n{}", run.output);
    second
}

#[cfg(test)]
#[path = "stale_artifact_tests.rs"]
mod tests;
