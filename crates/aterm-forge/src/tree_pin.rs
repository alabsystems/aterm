// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tree pins (`[OB-1]`): a first-party vendored directory copied whole from one tag of another
//! repository, held to the git tree id its pin file names.
//!
//! The pin file is the one alab-harness's `tools/pin` writes, byte for byte:
//! `tag=<vX.Y.Z>\ntree=<40 hex>\n`. What is held is the index: the subtree git would commit at
//! the directory, so a staged repin verifies before it is committed, as [`crate::astream_pin`]
//! judges the index's gitlink. The tree is written (`git write-tree --prefix=<dir>/`) from a
//! private index that holds only the index's entries under the directory (`ls-files --stage`),
//! so the check neither waits on nor dies at another git process's `index.lock`, never rewrites
//! the index it reads, and judges the copy alone: a merge left unresolved elsewhere in the
//! checkout, which makes git refuse to write any tree of the whole index, does not touch it. The
//! working copy may then hold no unstaged, untracked or ignored path under the directory or at
//! the pin, so what cargo builds is what the index holds. Git tree ids are equal exactly when
//! every path, mode and byte is, so one comparison covers the copy.
//!
//! Whether the pinned tag is that tree upstream is the upstream's own `tools/check-pin`'s
//! question: a clean clone of this repository has no upstream checkout to ask.

use aterm_uds::output::CommandOutput as _;
use aterm_uds::spawnfd::command;
use std::path::Path;

/// The tool that writes a pin file and its copy, named in every repair.
const PIN_TOOL: &str = "~/alab-harness/tools/pin";
/// How many offending paths a refusal lists before it counts the rest.
const LISTED: usize = 8;

/// A roster directory whose copy holds: the index carries exactly the tree its pin names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreePin {
    /// The roster row's directory, `vendor/<dir>`.
    pub dir: &'static str,
    /// Its pin file, repo-relative.
    pub pin: &'static str,
    /// The release tag the pin names.
    pub tag: String,
    /// The git tree id the pin names and the index holds.
    pub tree: String,
}

/// Why a tree pin was not held.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreePinError {
    /// The copy is not its pinned tree, the working copy changes it, or the pin is missing or
    /// garbled. The text names the repair.
    Fail(String),
    /// Git could not answer: it would not run, or the root is in no git checkout it reads.
    CouldNotRun(String),
}

/// What a pin file names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Pin {
    pub(crate) tag: String,
    pub(crate) tree: String,
}

/// Every pinned row of [`crate::provenance::FIRST_PARTY_VENDORED`], held to its pin; empty where
/// the roster does not describe `root` ([`crate::provenance::applies_to`]).
///
/// # Errors
/// The first row whose pin does not hold, with its repair, or why git could not judge it.
pub fn tree_pins(root: &Path) -> Result<Vec<TreePin>, TreePinError> {
    if !crate::provenance::applies_to(root) {
        return Ok(Vec::new());
    }
    crate::provenance::FIRST_PARTY_VENDORED
        .iter()
        .filter_map(|row| Some((row.dir, row.pin?)))
        .map(|(dir, pin)| {
            hold(root, dir, pin).map(|Pin { tag, tree }| TreePin {
                dir,
                pin,
                tag,
                tree,
            })
        })
        .collect()
}

/// Hold the copy at `dir` to the pin file `pin`, both relative to `root`.
pub(crate) fn hold(root: &Path, dir: &str, pin: &str) -> Result<Pin, TreePinError> {
    let repin = |tag: &str| {
        format!(
            "`{PIN_TOOL} {} {tag}` copies the tag whole and writes the pin; then \
             `git add -f {dir} {pin}`",
            root.display()
        )
    };
    let text = match crate::read_regular_text(root.join(pin)) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(TreePinError::Fail(format!(
                "`{pin}` does not exist, and the roster pins `{dir}` by it. Repair: {}.",
                repin("<tag>")
            )));
        }
        Err(e) => {
            return Err(TreePinError::Fail(format!(
                "`{pin}` cannot be read as a regular file ({e}), and the roster pins `{dir}` by \
                 it. Repair: {}.",
                repin("<tag>")
            )));
        }
    };
    let pinned = parse_pin(&text).map_err(|why| {
        TreePinError::Fail(format!(
            "`{pin}` is not the two lines {PIN_TOOL} writes (`tag=vX.Y.Z`, then `tree=` and 40 \
             lowercase hex digits, each ending in a newline): {why}. Repair: {}.",
            repin("<tag>")
        ))
    })?;
    let prefix = git(root, &["rev-parse", "--show-prefix"], None, None).map_err(|e| match e {
        Git::Spawn(why) | Git::Refused(why) => TreePinError::CouldNotRun(format!(
            "the tree pin of `{dir}` needs the git checkout at {}: {why}",
            root.display()
        )),
    })?;
    let prefix = prefix.trim_end_matches('\n');
    // The index's entries under the copy alone, each `mode object stage<TAB>path` ended by a NUL,
    // the path from the repository's top, as `update-index --index-info` takes them back.
    let entries = git(
        root,
        &["ls-files", "--stage", "-z", "--full-name", "--", dir],
        None,
        None,
    )
    .map_err(|e| match e {
        Git::Spawn(why) => TreePinError::CouldNotRun(why),
        Git::Refused(why) => {
            TreePinError::Fail(format!("the index cannot be read at `{dir}`: {why}"))
        }
    })?;
    if entries.is_empty() {
        return Err(TreePinError::Fail(format!(
            "the index tracks nothing at `{dir}`: the copy is not staged. Repair: {}.",
            repin(&pinned.tag)
        )));
    }
    let conflicted = unmerged(&entries);
    if !conflicted.is_empty() {
        return Err(TreePinError::Fail(format!(
            "the index holds an unresolved merge inside `{dir}` ({}): never resolve the copy by \
             hand. Take one side whole and stage it (`git checkout --ours -- {dir}` or \
             `--theirs`, then `git add -f {dir} {pin}`), or re-copy a tag: {}.",
            listed(&conflicted),
            repin(&pinned.tag)
        )));
    }
    let scratch = aterm_tempfile::tempdir().map_err(|e| {
        TreePinError::CouldNotRun(format!("no scratch folder for a private index: {e}"))
    })?;
    let private = scratch.path().join("index");
    git(
        root,
        &["update-index", "-z", "--index-info"],
        Some(&private),
        Some(entries.as_bytes()),
    )
    .map_err(|e| match e {
        Git::Spawn(why) | Git::Refused(why) => TreePinError::CouldNotRun(format!(
            "cannot write `{dir}`'s index entries into a private index: {why}"
        )),
    })?;
    let have = match git(
        root,
        &["write-tree", &format!("--prefix={prefix}{dir}/")],
        Some(&private),
        None,
    ) {
        Ok(out) => out.trim().to_owned(),
        Err(Git::Spawn(why)) => return Err(TreePinError::CouldNotRun(why)),
        Err(Git::Refused(why)) => {
            return Err(TreePinError::Fail(format!(
                "git cannot write `{dir}` as a tree from the index's entries ({why}). Repair: {}.",
                repin(&pinned.tag)
            )));
        }
    };
    if have != pinned.tree {
        return Err(TreePinError::Fail(format!(
            "the index holds `{dir}` as tree {have}, but `{pin}` names tree {} ({}): the copy is \
             not that tag byte for byte. Never edit it here: change alab-harness and tag a \
             release. Repair: {} (it refuses while `{dir}` holds uncommitted changes, so drop \
             them first).",
            pinned.tree,
            pinned.tag,
            repin(&pinned.tag)
        )));
    }
    let status = git(
        root,
        &[
            "--no-optional-locks",
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=normal",
            "--ignored=matching",
            "--",
            dir,
            pin,
        ],
        None,
        None,
    )
    .map_err(|e| match e {
        Git::Spawn(why) => TreePinError::CouldNotRun(why),
        Git::Refused(why) => {
            TreePinError::Fail(format!("the working copy of `{dir}` is unreadable: {why}"))
        }
    })?;
    let changed = unstaged(&status);
    if !changed.is_empty() {
        return Err(TreePinError::Fail(format!(
            "the working copy changes `{dir}` or `{pin}` past what the index holds (unstaged, \
             untracked or ignored: {}), so a build would read bytes the pin does not name. \
             Never edit or build inside it: restore it (`git restore -- {dir} {pin}`, \
             `git clean -dfx -- {dir}`), or re-copy the tag: {}.",
            listed(&changed),
            repin(&pinned.tag)
        )));
    }
    Ok(pinned)
}

/// `paths` joined by commas, at most [`LISTED`] of them, then how many more.
fn listed(paths: &[String]) -> String {
    let mut out = paths
        .iter()
        .take(LISTED)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    if paths.len() > LISTED {
        out.push_str(&format!(" and {} more", paths.len() - LISTED));
    }
    out
}

/// The paths of `git ls-files --stage -z` entries (`mode object stage<TAB>path`, each ended by a
/// NUL) at a stage other than 0: an unresolved merge's sides, each path once, in index order. An
/// entry this cannot read counts as one, whole.
fn unmerged(entries: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for entry in entries.split('\0').filter(|e| !e.is_empty()) {
        let (stage, path) = match entry.split_once('\t') {
            Some((head, path)) => (head.rsplit(' ').next(), path),
            None => (None, entry),
        };
        if stage != Some("0") && out.last().is_none_or(|last| last != path) {
            out.push(path.to_owned());
        }
    }
    out
}

/// Parse the exact bytes `tools/pin` writes: `tag=<tag>\ntree=<tree>\n`, the tag a release tag
/// as `tools/pin` accepts one and the tree 40 lowercase hex digits, and nothing else.
pub(crate) fn parse_pin(text: &str) -> Result<Pin, String> {
    let mut lines = text.split_inclusive('\n');
    let mut line = |key: &str| -> Result<String, String> {
        let Some(line) = lines.next() else {
            return Err(format!("it has no `{key}=` line"));
        };
        let Some(body) = line.strip_suffix('\n') else {
            return Err(format!("its line {line:?} does not end in a newline"));
        };
        body.strip_prefix(key)
            .and_then(|rest| rest.strip_prefix('='))
            .map(str::to_owned)
            .ok_or_else(|| format!("its line {body:?} is not the `{key}=` line"))
    };
    let tag = line("tag")?;
    let tree = line("tree")?;
    if let Some(extra) = lines.next() {
        return Err(format!("it goes on past the tree line: {extra:?}"));
    }
    if !release_tag(&tag) {
        return Err(format!("`{tag}` is no release tag (vX.Y.Z)"));
    }
    if tree.len() != 40
        || !tree
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(format!("`{tree}` is not 40 lowercase hex digits"));
    }
    Ok(Pin { tag, tree })
}

/// `tools/pin`'s own test of a release tag, its shell pattern `v[0-9]*.[0-9]*.[0-9]*`: a `v`, a
/// digit, then two later dots each followed by a digit.
fn release_tag(tag: &str) -> bool {
    let Some(rest) = tag.strip_prefix('v') else {
        return false;
    };
    let bytes = rest.as_bytes();
    // A dot at `i` followed by a digit, at or after `from`.
    let dot_digit = |from: usize| {
        (from..bytes.len().saturating_sub(1))
            .find(|&i| bytes[i] == b'.' && bytes[i + 1].is_ascii_digit())
    };
    bytes.first().is_some_and(u8::is_ascii_digit)
        && dot_digit(1).is_some_and(|first| dot_digit(first + 2).is_some())
}

/// The paths `git status --porcelain=v1 -z` reports changed past the index: unstaged edits and
/// deletions, untracked (`??`) and ignored (`!!`) paths, and unmerged entries; that is, every
/// entry whose worktree column is not blank. A staged change is the index's own and is judged
/// by the tree.
fn unstaged(status: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut entries = status.split('\0').filter(|e| !e.is_empty());
    while let Some(entry) = entries.next() {
        let (Some(index), Some(worktree), Some(path)) =
            (entry.chars().next(), entry.chars().nth(1), entry.get(3..))
        else {
            out.push(entry.to_owned());
            continue;
        };
        // A rename or copy is followed by the path it came from, as its own entry.
        if matches!(index, 'R' | 'C') {
            entries.next();
        }
        if worktree != ' ' {
            out.push(path.to_owned());
        }
    }
    out
}

/// A failed git invocation.
enum Git {
    /// Git did not run at all.
    Spawn(String),
    /// Git ran and refused, with what it said.
    Refused(String),
}

/// Run git in `root` with the caller's repository redirects removed, so it answers for the
/// checkout at `root`; with `index`, against that index file instead of the checkout's; with
/// `input`, those bytes on its standard input.
fn git(
    root: &Path,
    args: &[&str],
    index: Option<&Path>,
    input: Option<&[u8]>,
) -> Result<String, Git> {
    let mut cmd = command(aterm_uds::os_path!("git"));
    cmd.args(args).current_dir(root);
    for var in crate::direct_vendor::REPO_ENV {
        cmd.env_remove(var);
    }
    if let Some(index) = index {
        cmd.env("GIT_INDEX_FILE", index);
    }
    let spawn = |e: std::io::Error| Git::Spawn(format!("cannot run `git {}`: {e}", args.join(" ")));
    let out = match input {
        None => cmd.output_owned().map_err(spawn)?,
        Some(input) => {
            use aterm_uds::spawnfd::Stdio;
            use std::io::Write as _;
            let mut child = cmd
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(spawn)?;
            // Dropped once written, so git reads the end of its input. A write git did not take
            // (it refused before reading) is no error here: its exit and stderr say why.
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(input);
            }
            child.wait_with_output().map_err(spawn)?
        }
    };
    if !out.status.success() {
        return Err(Git::Refused(format!(
            "`git {}` refused: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    String::from_utf8(out.stdout)
        .map_err(|e| Git::Refused(format!("`git {}` printed non-UTF-8: {e}", args.join(" "))))
}

#[cfg(test)]
#[path = "tree_pin_tests.rs"]
pub(crate) mod tests;
