// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Handoff fixture staging and landing for release cuts.

use aterm_uds::spawnfd::Command;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use crate::gates::{
    GateTree, HANDOFF_FIXTURE_GUARD, HANDOFF_FIXTURE_README, HANDOFF_FIXTURE_ROOT, HandoffFixtures,
};
use crate::ledger::{Error, GitRunner, Origin, RefWrite, Result, git_ok, rev_parse};

#[path = "fixture_reader_inputs.rs"]
mod reader_inputs;

/// In-tree handoff fixture generator test module path.
pub const GENERATOR_SOURCE: &str = "crates/aterm-gui/src/seamless_fixture_generator_tests.rs";

/// Entry test names for the handoff and rendezvous fixture generators.
pub const GENERATOR_TESTS: [&str; 2] = ["generate_handoff_fixtures", "generate_rendezvous_fixture"];

/// Marker file written when a staging completes.
pub const STAGED_MARK: &str = "staged";

/// Checked-in serde corpus shipped-state answers path.
pub const SHIPPED_STATE_CORPUS: &str =
    "crates/aterm-gui/tests/fixtures/serde-corpus/shipped-state.txt";

/// Staged shipped-state answers filename.
pub const STAGED_ANSWERS: &str = "shipped-state.txt";

/// Environment variable naming where the generator writes [`STAGED_ANSWERS`].
pub const ANSWERS_OUT: &str = "ATERM_SHIPPED_STATE_OUT";

/// Main's field-hold test, executed from the same binary as its desk readers.
pub const FIELD_HOLD_TEST: &str =
    "seamless::carry_tests::fixture_generator::check_landed_handoff_fields";
/// Environment variable naming the release directory the field hold judges.
pub const FIELD_HOLD_RELEASE: &str = "ATERM_HANDOFF_FIELD_HOLD_RELEASE";

/// Release identity pins stamped into generated fixtures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pins {
    /// Release version (`X.Y.0`).
    pub version: String,
    /// Reserved build number.
    pub build: u64,
    /// Full release commit SHA.
    pub commit: String,
}

impl Pins {
    fn short(&self) -> &str {
        self.commit.get(..12).unwrap_or(&self.commit)
    }

    /// `v<version>`, the fixture directory's name.
    #[must_use]
    pub fn tag(&self) -> String {
        format!("v{}", self.version)
    }

    /// Where this identity is staged under the release cache's `root`.
    #[must_use]
    pub fn staged_in(&self, root: &Path) -> PathBuf {
        root.join(format!("{}-{}-{}", self.tag(), self.build, self.short()))
    }

    fn mark(&self) -> String {
        format!(
            "version {}\nbuild {}\ncommit {}\n",
            self.version, self.build, self.commit
        )
    }
}

/// A complete handoff fixture staging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Staged {
    /// `<staging>/v<version>` fixture directory.
    pub dir: PathBuf,
    /// Sorted desk directory names holding `parent.toml`.
    pub desks: Vec<String>,
    /// Whether `rendezvous.toml` is present.
    pub rendezvous: bool,
    /// `<staging>/shipped-state.txt` ([`STAGED_ANSWERS`]).
    pub answers: PathBuf,
    /// Written by this call (else found staged).
    pub fresh: bool,
}

impl Staged {
    /// Formats a transcript summary of this staging.
    #[must_use]
    pub fn words(&self, pins: &Pins) -> String {
        format!(
            "{}'s handoff fixtures {} for build {} ({}): {} desks{} in {}",
            pins.tag(),
            if self.fresh {
                "written by its own producer"
            } else {
                "staged already"
            },
            pins.build,
            pins.short(),
            self.desks.len(),
            if self.rendezvous {
                " and rendezvous.toml"
            } else {
                ""
            },
            self.dir.display()
        )
    }
}

/// Returns the complete staging of `pins` under `root`, if present.
#[must_use]
pub fn staged(root: &Path, pins: &Pins) -> Option<Staged> {
    let at = pins.staged_in(root);
    if crate::durable::read_text(&at.join(STAGED_MARK)).ok()? != pins.mark() {
        return None;
    }
    let dir = at.join(pins.tag());
    let desks = desks_in(&dir).ok()?;
    let answers = at.join(STAGED_ANSWERS);
    (!desks.is_empty() && answers.is_file()).then(|| Staged {
        rendezvous: dir.join("rendezvous.toml").is_file(),
        dir,
        desks,
        answers,
        fresh: false,
    })
}

/// The directories directly under `dir` holding a `parent.toml`, sorted.
fn desks_in(dir: &Path) -> std::io::Result<Vec<String>> {
    let mut desks = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if !entry
            .file_type()
            .map(|ft| ft.is_dir() || ft.is_symlink())
            .unwrap_or(true)
        {
            continue;
        }
        if entry.path().join("parent.toml").is_file() {
            desks.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    desks.sort_unstable();
    Ok(desks)
}

/// Returns the single-threaded libtest arguments for the fixture generator.
#[must_use]
pub fn generator_args() -> Vec<&'static str> {
    let mut args = GENERATOR_TESTS.to_vec();
    args.extend(["--test-threads", "1"]);
    args
}

/// macOS assesses a first exec by path, even if a later compiler would replace its bytes.
fn execution_hold(root: &Path, binary: &Path) -> PathBuf {
    let key = aterm_digest::Sha256::digest(binary.as_os_str().as_encoded_bytes());
    root.join(".ubd/generator-holds.noindex")
        .join(aterm_codec::hex::encode_lower(&key))
}

fn check_execution_hold(root: &Path, binary: &Path) -> Result<()> {
    if let Some(left) = crate::gates::first_exec_hold(&execution_hold(root, binary)) {
        return Err(Error::new(format!(
            "handoff fixtures: {} never showed it ran; execution hold has {} s left",
            binary.display(),
            left.as_secs()
        )));
    }
    Ok(())
}

/// Runs the in-tree fixture generator under `pins` into `root`, unless already staged.
///
/// Refuses when the generator does not pass both its tests (one off macOS, which has no
/// rendezvous; a desk the tree's guard cannot adopt as written fails the first), when a desk
/// `REQUIRED_DESKS` asks of this release is missing, when a desk's `parent.toml` was not
/// written as `pins`, and when it wrote no shipped-state answers for the release's desks
/// ([`STAGED_ANSWERS`], at `$ATERM_SHIPPED_STATE_OUT`).
///
/// # Errors
/// Each refusal above, no `rendezvous.toml` on macOS, an unidentifiable or dirty candidate,
/// an unreadable handoff guard, a missing generator that cannot be compiled or indexed,
/// a generator that cannot run, an execution hold, and staging or index I/O failures.
pub fn stage(
    targo: &Path,
    tree: &Path,
    root: &Path,
    pins: &Pins,
    run: &mut dyn FnMut(&mut Command) -> std::io::Result<Output>,
) -> Result<Staged> {
    let physical_tree = aterm_verify::physical(tree);
    let tree = physical_tree.as_path();
    if let Some(found) = staged(root, pins) {
        return Ok(found);
    }
    let io = |what: &str, path: &Path, e: std::io::Error| {
        Error::new(format!("handoff fixtures: {what} {}: {e}", path.display()))
    };
    if let Ok(entries) = fs::read_dir(root) {
        let prefix = format!("{}-", pins.tag());
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with(&prefix) {
                let path = entry.path();
                crate::gates::clear_path(&path).map_err(|e| io("cannot clear", &path, e))?;
            }
        }
    }
    let at = pins.staged_in(root);
    let dir = at.join(pins.tag());
    fs::create_dir_all(&dir).map_err(|e| io("cannot create", &dir, e))?;
    let (target, layout) = crate::release_steps::verify_layout(tree);
    let index_path = target.join("verify/test-executables.json");
    let (tree_id, compiler) = aterm_verify::test_index::identities(tree, targo)
        .map_err(|e| io("cannot identify candidate", tree, e))?;
    let select = |index: &aterm_verify::test_index::Index| {
        index.fixture_binary(&tree_id, &compiler, tree).cloned()
    };
    let previous = aterm_verify::test_index::Index::read(&index_path);
    if let Some(previous) = &previous {
        for entry in &previous.binaries {
            if entry.kind == "lib"
                && entry.name == "aterm_gui"
                && entry.manifest_path == tree.join("crates/aterm-gui/Cargo.toml")
            {
                // Check before Cargo can invalidate or replace a stale artifact.
                check_execution_hold(root, &entry.path)?;
            }
        }
    }
    let entry = match previous.and_then(|i| select(&i)) {
        Some(entry) => entry,
        None => {
            let mut compile = crate::gates::aterm_gui_lib_compile(targo, tree, &target);
            if let Some(layout) = &layout {
                compile.arg("--config").arg(layout);
            }
            let output = run(&mut compile).map_err(|e| io("cannot compile generator", targo, e))?;
            if !output.status.success() {
                return Err(Error::new(format!(
                    "handoff fixtures: generator compile failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                )));
            }
            let index = aterm_verify::test_index::Index::from_messages(
                tree_id.clone(),
                compiler.clone(),
                &String::from_utf8_lossy(&output.stdout),
            )
            .map_err(|e| io("cannot index generator", &index_path, e))?;
            let entry = select(&index).ok_or_else(|| Error::new("handoff fixtures: compile did not report the candidate's GUI library test binary"))?;
            index
                .write(&index_path)
                .map_err(|e| io("cannot save generator index", &index_path, e))?;
            entry
        }
    };
    // Holds belong to this executable alone. No directory-wide marker can attest to
    // another binary, and no fixture run clears or deletes compiler outputs.
    let hold = execution_hold(root, &entry.path);
    fs::create_dir_all(&hold).map_err(|e| io("cannot create execution hold", &hold, e))?;
    check_execution_hold(root, &entry.path)?;
    stamp_hold(&hold).map_err(|e| io("cannot stamp execution hold", &hold, e))?;
    println!(
        "==> handoff fixtures: {} build {} ({}) directly from indexed binary {}",
        pins.tag(),
        pins.build,
        pins.short(),
        entry.path.display()
    );
    let mut command = aterm_uds::spawnfd::command(&entry.path);
    command
        .args(generator_args())
        .current_dir(tree)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .env(
            "CARGO_MANIFEST_DIR",
            entry
                .manifest_path
                .parent()
                .ok_or_else(|| Error::new("generator manifest has no parent"))?,
        )
        .env("ATERM_HANDOFF_FIXTURE_VERSION", &pins.version)
        .env("ATERM_BUILD_GIT_COMMIT", pins.short())
        .env("ATERM_BUILD_GIT_COMMIT_FULL", &pins.commit)
        .env("SOURCE_DATE_EPOCH", pins.build.to_string())
        .env("ATERM_HANDOFF_FIXTURE_OUT", &dir)
        .env("ATERM_RENDEZVOUS_FIXTURE_OUT", &dir)
        .env(ANSWERS_OUT, at.join(STAGED_ANSWERS));
    let outcome = run(&mut command);
    if outcome.as_ref().is_ok_and(|out| {
        let (passed, failed, _) =
            crate::gates::libtest_tally(&String::from_utf8_lossy(&out.stdout));
        passed + failed > 0
    }) {
        clear_marker(&hold.join(aterm_uds::deadline::EXEC_STAMP))
            .map_err(|e| io("cannot clear execution hold", &hold, e))?;
    } else {
        stamp_hold(&hold).map_err(|e| io("cannot renew execution hold", &hold, e))?;
    }
    let output = outcome.map_err(|e| io("cannot run generator", &entry.path, e))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let (passed, failed, failing) = crate::gates::libtest_tally(&stdout);
    let expected = if cfg!(target_os = "macos") {
        GENERATOR_TESTS.len()
    } else {
        1
    };
    let refuse = |why: String| {
        Error::new(format!(
            "the release's handoff fixture generator ({GENERATOR_SOURCE}) refuses: {why}\n\
             nothing was claimed or published. Its own consumer must adopt every desk its \
             producer writes: fix the producer, the consumer or the generator on main \
             (run it as {HANDOFF_FIXTURE_README} says)"
        ))
    };
    if !output.status.success() || failed > 0 {
        return Err(refuse(if failing.is_empty() {
            format!(
                "it did not pass, and no test failed by name — the build or the run stopped \
                 first:\n{}",
                crate::gates::build_error_lines(&String::from_utf8_lossy(&output.stderr))
            )
        } else {
            format!("{} test(s) failed: {}", failing.len(), failing.join(", "))
        }));
    }
    if passed != expected {
        return Err(refuse(format!(
            "{passed} of its {expected} test(s) ran — was one renamed?"
        )));
    }
    let desks = desks_in(&dir).map_err(|e| io("cannot read", &dir, e))?;
    let guard_path = tree.join(HANDOFF_FIXTURE_GUARD);
    let guard =
        crate::durable::read_text(&guard_path).map_err(|e| io("cannot read", &guard_path, e))?;
    let mut missing = Vec::new();
    for required in crate::gates::required_desks(&guard)? {
        if crate::gates::release_at_least(&pins.version, &required.from)?
            && !desks.contains(&required.desk)
        {
            missing.push(required.desk);
        }
    }
    if !missing.is_empty() {
        return Err(refuse(format!(
            "it wrote no {} (REQUIRED_DESKS asks every release from its floor on; it wrote: \
             {})",
            missing.join(", "),
            desks.join(", ")
        )));
    }
    let rendezvous = dir.join("rendezvous.toml").is_file();
    if cfg!(target_os = "macos") && !rendezvous {
        return Err(refuse("it wrote no rendezvous.toml".to_string()));
    }
    let answers = at.join(STAGED_ANSWERS);
    let prefix = format!("{}/", pins.tag());
    if !crate::durable::read_text(&answers)
        .is_ok_and(|text| text.lines().any(|l| l.starts_with(&prefix)))
    {
        return Err(refuse(format!(
            "it wrote no shipped-state answers for {}'s desks (${ANSWERS_OUT}, {})",
            pins.tag(),
            answers.display()
        )));
    }
    let expected_lines = [
        format!("producer_version = \"{}\"", pins.version),
        format!("producer_commit = \"{}\"", pins.short()),
        format!("outgoing_build = {}", pins.build),
    ];
    for desk in &desks {
        let parent = dir.join(desk).join("parent.toml");
        let text = crate::durable::read_text(&parent).map_err(|e| io("cannot read", &parent, e))?;
        for line in &expected_lines {
            if !text.lines().any(|l| l == line) {
                return Err(refuse(format!(
                    "{desk}'s parent.toml lacks `{line}`: it was not written as the shipped \
                     binary (is the tree's version {}?)",
                    pins.version
                )));
            }
        }
    }
    let mark = at.join(STAGED_MARK);
    crate::durable::write_fresh(&mark, pins.mark()).map_err(|e| io("cannot write", &mark, e))?;
    Ok(Staged {
        dir,
        desks,
        rendezvous,
        answers,
        fresh: true,
    })
}

fn clear_marker(path: &Path) -> std::io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn stamp_hold(target: &Path) -> std::io::Result<()> {
    // A retained marker can be a link. Unlink its directory entry before writing,
    // so renewing the hold never overwrites a file outside the compiler cache.
    clear_marker(&target.join(aterm_uds::deadline::EXEC_STAMP))?;
    aterm_uds::deadline::try_stamp_exec(target)
}

/// Inspects release `version`'s handoff fixtures at `commit`, if present.
///
/// # Errors
/// Returns an error if the fixture directory exists but fails validation, or on git failure.
pub fn on_commit(
    git: &dyn GitRunner,
    commit: &str,
    version: &str,
) -> Result<Option<HandoffFixtures>> {
    let tree = GateTree::Commit { git, commit };
    let dir = format!("{HANDOFF_FIXTURE_ROOT}/v{version}");
    if tree.desks(&dir)?.is_none() {
        return Ok(None);
    }
    crate::gates::handoff_fixtures_of(&tree, version)
        .map(Some)
        .map_err(|e| {
            Error::new(format!(
                "{} holds {dir}/, but not as the next cut's gate needs it: {e} — that \
                 directory is not this release's to overwrite; finish it by hand \
                 ({HANDOFF_FIXTURE_README})",
                commit.get(..12).unwrap_or(commit)
            ))
        })
}

/// Appends one `PINNED_DESKS` row per desk of `v<version>` to `guard`.
///
/// # Errors
/// Returns an error if `v<version>` is already pinned or `PINNED_DESKS` cannot be updated.
pub fn with_pinned_rows(guard: &str, version: &str, desks: &[String]) -> Result<String> {
    let tag = format!("v{version}");
    let unreadable = |why: &str| {
        Error::new(format!(
            "handoff fixtures: cannot add {tag}'s rows to PINNED_DESKS in \
             {HANDOFF_FIXTURE_GUARD} ({why})"
        ))
    };
    if crate::gates::pinned_desks(guard)?
        .iter()
        .any(|(release, _)| *release == tag)
    {
        return Err(unreadable("it pins the release already"));
    }
    let table = guard
        .find("const PINNED_DESKS")
        .ok_or_else(|| unreadable("no `const PINNED_DESKS`"))?;
    let open = guard[table..]
        .find("= &[")
        .map(|at| table + at)
        .ok_or_else(|| unreadable("no `= &[` after it"))?;
    let close = guard[open..]
        .find("\n];")
        .map(|at| open + at + 1)
        .ok_or_else(|| unreadable("no `];` line closing it"))?;
    let rows: String = desks
        .iter()
        .map(|desk| format!("    (\"{tag}\", \"{desk}\"),\n"))
        .collect();
    let out = format!("{}{rows}{}", &guard[..close], &guard[close..]);
    let pinned: Vec<String> = crate::gates::pinned_desks(&out)?
        .into_iter()
        .filter(|(release, _)| *release == tag)
        .map(|(_, desk)| desk)
        .collect();
    if pinned != desks {
        return Err(unreadable("the rows did not read back"));
    }
    Ok(out)
}

/// Splices release `tag`'s cases from `staged` into `corpus` and returns the updated text and case count.
///
/// # Errors
/// `staged` holds no case of `tag`, or holds them apart, or first; `corpus` records `tag`
/// already, or holds no line, or more than one, with the case id they go after.
pub fn with_release_answers(corpus: &str, staged: &str, tag: &str) -> Result<(String, usize)> {
    let prefix = format!("{tag}/");
    let refuse = |why: String| Error::new(format!("{tag}'s shipped-state answers: {why}"));
    fn key(line: &str) -> &str {
        line.split_once('\t').map_or(line, |(id, _)| id)
    }
    let lines: Vec<&str> = staged.lines().collect();
    let first = lines
        .iter()
        .position(|line| line.starts_with(&prefix))
        .ok_or_else(|| refuse("the staged answers hold no case of its desks".to_string()))?;
    let count = lines[first..]
        .iter()
        .take_while(|line| line.starts_with(&prefix))
        .count();
    if lines[first + count..]
        .iter()
        .any(|line| line.starts_with(&prefix))
    {
        return Err(refuse(
            "the staged answers hold its cases apart, not as one block".to_string(),
        ));
    }
    let Some(before) = first.checked_sub(1).map(|at| key(lines[at])) else {
        return Err(refuse(
            "the staged answers put its cases before the file's header".to_string(),
        ));
    };
    let mut out: Vec<&str> = corpus.lines().collect();
    if out.iter().any(|line| line.starts_with(&prefix)) {
        return Err(refuse(format!(
            "{SHIPPED_STATE_CORPUS} records {tag} already"
        )));
    }
    let anchors: Vec<usize> = out
        .iter()
        .enumerate()
        .filter(|(_, line)| key(line) == before)
        .map(|(at, _)| at)
        .collect();
    let [anchor] = anchors.as_slice() else {
        return Err(refuse(format!(
            "{SHIPPED_STATE_CORPUS} holds {} lines with the case id they go after ({before:?}), \
             not one",
            anchors.len()
        )));
    };
    out.splice(
        anchor + 1..anchor + 1,
        lines[first..first + count].iter().copied(),
    );
    let mut text = out.join("\n");
    text.push('\n');
    Ok((text, count))
}

/// Builds a git tree object from `base` with each `(path, blob)` in `files` replaced.
///
/// # Errors
/// Returns an error if a path traverses a file or git fails.
pub fn tree_with(
    git: &dyn GitRunner,
    base: Option<&str>,
    files: &[(&str, &str)],
) -> Result<String> {
    let mut entries: std::collections::BTreeMap<Vec<u8>, Vec<u8>> =
        std::collections::BTreeMap::new();
    if let Some(base) = base {
        for entry in git_ok(git, &["ls-tree", "-z", base])?
            .stdout
            .split(|b| *b == 0)
            .filter(|e| !e.is_empty())
        {
            let tab = entry
                .iter()
                .position(|b| *b == b'\t')
                .ok_or_else(|| Error::new(format!("git ls-tree {base}: an entry with no name")))?;
            entries.insert(entry[tab + 1..].to_vec(), entry.to_vec());
        }
    }
    let mut subdirs: std::collections::BTreeMap<&str, Vec<(&str, &str)>> =
        std::collections::BTreeMap::new();
    for (path, blob) in files {
        match path.split_once('/') {
            None => {
                entries.insert(
                    path.as_bytes().to_vec(),
                    format!("100644 blob {blob}\t{path}").into_bytes(),
                );
            }
            Some((dir, rest)) => subdirs.entry(dir).or_default().push((rest, blob)),
        }
    }
    for (dir, inner) in subdirs {
        let sub_base = match entries.get(dir.as_bytes()) {
            None => None,
            Some(entry) => {
                let entry = String::from_utf8_lossy(entry);
                let mut fields = entry.split([' ', '\t']);
                match (fields.next(), fields.next(), fields.next()) {
                    (Some("040000"), Some("tree"), Some(oid)) => Some(oid.to_string()),
                    _ => {
                        return Err(Error::new(format!(
                            "{dir} is not a directory in {}",
                            base.unwrap_or("the tree")
                        )));
                    }
                }
            }
        };
        let sub = tree_with(git, sub_base.as_deref(), &inner)?;
        entries.insert(
            dir.as_bytes().to_vec(),
            format!("040000 tree {sub}\t{dir}").into_bytes(),
        );
    }
    let mut listing = Vec::new();
    for entry in entries.values() {
        listing.extend_from_slice(entry);
        listing.push(0);
    }
    let tree = git.git_input(&["mktree", "-z"], &listing)?;
    if !tree.success() {
        return Err(Error::new(format!(
            "git mktree failed (exit {}): {}",
            tree.status,
            tree.stderr_utf8().trim()
        )));
    }
    Ok(tree.stdout_utf8().trim().to_string())
}

/// Every file under `dir` as (its `/`-separated path relative to `dir`, its bytes), sorted.
fn files_under(dir: &Path) -> Result<Vec<(String, Vec<u8>)>> {
    let mut out = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), String::new())];
    while let Some((at, rel)) = stack.pop() {
        let entries =
            fs::read_dir(&at).map_err(|e| Error::new(format!("read {}: {e}", at.display())))?;
        for entry in entries {
            let entry = entry.map_err(|e| Error::new(format!("read {}: {e}", at.display())))?;
            let ft = entry
                .file_type()
                .map_err(|e| Error::new(format!("read {}: {e}", entry.path().display())))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = if rel.is_empty() {
                name
            } else {
                format!("{rel}/{name}")
            };
            if ft.is_dir() {
                stack.push((entry.path(), path));
            } else if ft.is_file() {
                let entry_path = entry.path();
                let bytes = crate::durable::read_file(&entry_path)
                    .map_err(|e| Error::new(format!("read {}: {e}", entry_path.display())))?;
                out.push((path, bytes));
            }
        }
    }
    out.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// Main's interpretation of a proposed fixture landing.
#[derive(Debug, Clone)]
pub struct Reading {
    /// Complete corpus rendered by main, never the release line's reader.
    pub answers: std::result::Result<String, String>,
    /// Verdict including failing test names or why the checks could not run.
    pub checks: String,
}

/// One leased source path per clone, retained across fixture landings.
pub struct MainReader {
    repo: PathBuf,
    scratch: PathBuf,
    _lease: aterm_update_core::FileLock,
    last_binary: Option<PathBuf>,
    last_reading: Option<(String, Pins, PathBuf, reader_inputs::Inputs, Reading)>,
    failures: Vec<String>,
}

impl MainReader {
    /// Lease the clone's persistent, isolated reader workspace.
    ///
    /// # Errors
    /// Returns git discovery, directory creation or exclusive lease failures.
    pub fn new(repo: &Path) -> std::io::Result<Self> {
        let git = crate::ledger::GitCli::new(repo);
        let dir = git_ok(&git, &["rev-parse", "--absolute-git-dir"])
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        let scratch = PathBuf::from(dir.stdout_utf8().trim()).join(".ubd/main-reader.noindex");
        fs::create_dir_all(&scratch)?;
        let lease = aterm_update_core::FileLock::acquire_within(
            &scratch.join("lease"),
            std::time::Duration::ZERO,
        )?;
        let last_binary = match fs::read_to_string(scratch.join("last-binary")) {
            Ok(path) => Some(PathBuf::from(path)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        Ok(Self {
            repo: repo.to_path_buf(),
            scratch,
            _lease: lease,
            last_binary,
            last_reading: None,
            failures: Vec::new(),
        })
    }

    /// Record main's answers and exercise its desk readers. During this landing,
    /// reuse a reading only across changes confined to Cargo-unrelated packages.
    /// Failures remain named even when changed reader inputs require a new judgment.
    #[must_use]
    pub fn read(
        &mut self,
        targo: &Path,
        preview: &str,
        pins: &Pins,
        run: &mut dyn FnMut(&mut Command) -> std::io::Result<Output>,
    ) -> Reading {
        let started = std::time::Instant::now();
        let git = crate::ledger::GitCli::new(&self.repo);
        if let Some((previous, identity, compiler, inputs, reading)) = &self.last_reading
            && identity == pins
            && compiler == targo
            && self
                .last_binary
                .as_ref()
                .is_none_or(|binary| check_execution_hold(&self.scratch, binary).is_ok())
            && git_ok(
                &git,
                &[
                    "diff",
                    "--no-renames",
                    "--name-only",
                    "-z",
                    previous,
                    preview,
                ],
            )
            .is_ok_and(|diff| {
                diff.stdout
                    .split(|b| *b == 0)
                    .filter(|p| !p.is_empty())
                    .all(|path| {
                        std::str::from_utf8(path).is_ok_and(|path| inputs.ignores(Path::new(path)))
                    })
            })
        {
            let mut reused = reading.clone();
            reused.checks.push_str(&format!(
                "; reused unchanged reader inputs from {previous}, comparison {:.3}s",
                started.elapsed().as_secs_f64()
            ));
            return reused;
        }
        let mut timings = Vec::new();
        let mut answers = Err("main reader did not record answers".to_string());
        let mut inputs = None;
        let result = (|| -> Result<String> {
            if let Some(binary) = &self.last_binary {
                check_execution_hold(&self.scratch, binary)?;
            }
            let tree = self.scratch.join("main");
            let git = crate::ledger::GitCli::new(&self.repo);
            let tree_arg = tree.to_string_lossy().into_owned();
            let local = crate::ledger::GitCli::new(&tree);
            if tree.join(".git").exists() {
                // This workspace is exclusively ours. Discard previous generated
                // answers and interrupted runs before judging the new preview.
                git_ok(&local, &["checkout", "--force", "--detach", preview])?;
                git_ok(&local, &["clean", "-ffdx"])?;
            } else {
                git_ok(&git, &["worktree", "add", "--detach", &tree_arg, preview])?;
            }
            git_ok(
                &local,
                &["submodule", "update", "--init", "--recursive", "--force"],
            )?;
            // Metadata covers normal, build and dev edges, including transitive
            // dependencies. Failure only disables reuse; it never supplies a verdict.
            if tree.join("Cargo.toml").is_file() {
                let mut metadata = aterm_uds::spawnfd::command(targo);
                metadata
                    .args(["metadata", "--locked", "--offline", "--format-version", "1"])
                    .current_dir(&tree)
                    .env_remove("RUSTFLAGS")
                    .env_remove("RUSTDOCFLAGS")
                    .env_remove("CARGO_ENCODED_RUSTFLAGS")
                    .env_remove("RUSTC")
                    .env_remove("RUSTC_BOOTSTRAP")
                    .env_remove("RUSTUP_TOOLCHAIN")
                    .env_remove("CARGO_TARGET_DIR")
                    .env_remove("CARGO_BUILD_TARGET");
                if let Ok(output) = run(&mut metadata)
                    && output.status.success()
                {
                    inputs = reader_inputs::Inputs::from_metadata(&output.stdout, &tree);
                }
            }
            // The stable source path keeps its own lane across releases; no live
            // directory is shared with the cut and no cross-root seeding is needed.
            let (target, layout) = crate::release_steps::verify_layout(&self.repo);
            let mut compile = crate::gates::aterm_gui_lib_compile(targo, &tree, &target);
            if let Some(layout) = layout {
                compile.arg("--config").arg(layout);
            }
            let compile_start = std::time::Instant::now();
            let compiled =
                run(&mut compile).map_err(|e| Error::new(format!("main reader compile: {e}")))?;
            let units =
                aterm_verify::compile_account::read(&String::from_utf8_lossy(&compiled.stdout))
                    .units;
            timings.push(format!(
                "compile {:.3}s ({} fresh, {} compiled, {} unknown)",
                compile_start.elapsed().as_secs_f64(),
                units.fresh,
                units.compiled,
                units.unknown
            ));
            if !compiled.status.success() {
                return Err(Error::new(format!(
                    "main reader compile failed: {}",
                    crate::gates::build_error_lines(&String::from_utf8_lossy(&compiled.stderr))
                )));
            }
            let index = aterm_verify::test_index::Index::from_messages(
                preview.to_string(),
                String::new(),
                &String::from_utf8_lossy(&compiled.stdout),
            )
            .map_err(|e| Error::new(format!("main reader executable index: {e}")))?;
            let binary = index
                .binaries
                .iter()
                .find(|entry| entry.kind == "lib" && entry.name == "aterm_gui")
                .ok_or_else(|| Error::new("main reader compile reported no GUI lib test binary"))?;
            let output = self.scratch.join(STAGED_ANSWERS);
            clear_marker(&output).map_err(|e| Error::new(format!("clear main answers: {e}")))?;
            let command = || {
                let mut cmd = aterm_uds::spawnfd::command(&binary.path);
                cmd.current_dir(&tree)
                    .env("CARGO_MANIFEST_DIR", tree.join("crates/aterm-gui"))
                    .env_remove("ATERM_BLESS_SERDE_CORPUS")
                    .stdin(aterm_uds::spawnfd::Stdio::null());
                cmd
            };
            let record_start = std::time::Instant::now();
            let hold = execution_hold(&self.scratch, &binary.path);
            let hold_error = |e| Error::new(format!("main reader execution hold: {e}"));
            fs::create_dir_all(&hold).map_err(hold_error)?;
            stamp_hold(&hold).map_err(hold_error)?;
            fs::write(
                self.scratch.join("last-binary"),
                binary.path.as_os_str().as_encoded_bytes(),
            )
            .map_err(hold_error)?;
            self.last_binary = Some(binary.path.clone());
            let outcome = run(command()
                .args([
                    "--ignored",
                    "--exact",
                    "serde_corpus_tests::record_shipped_state_for_landing",
                ])
                .env(ANSWERS_OUT, &output));
            if outcome.as_ref().is_ok_and(|out| {
                let (passed, failed, _) =
                    crate::gates::libtest_tally(&String::from_utf8_lossy(&out.stdout));
                passed + failed > 0
            }) {
                clear_marker(&hold.join(aterm_uds::deadline::EXEC_STAMP)).map_err(hold_error)?;
            } else {
                stamp_hold(&hold).map_err(hold_error)?;
            }
            let recorded = outcome.map_err(|e| Error::new(format!("main reader record: {e}")))?;
            timings.push(format!(
                "record {:.3}s",
                record_start.elapsed().as_secs_f64()
            ));
            let (passed, failed, _) =
                crate::gates::libtest_tally(&String::from_utf8_lossy(&recorded.stdout));
            if !recorded.status.success() || passed != 1 || failed != 0 {
                return Err(Error::new(format!(
                    "main reader recording failed: {} {}",
                    String::from_utf8_lossy(&recorded.stdout),
                    String::from_utf8_lossy(&recorded.stderr)
                )));
            }
            let rendered = crate::durable::read_text(&output)
                .map_err(|e| Error::new(format!("main reader answers: {e}")))?;
            let corpus_path = tree.join(SHIPPED_STATE_CORPUS);
            let corpus = crate::durable::read_text(&corpus_path)
                .map_err(|e| Error::new(format!("main corpus: {e}")))?;
            let (merged, _) = with_release_answers(&corpus, &rendered, &pins.tag())?;
            fs::write(corpus_path, merged)
                .map_err(|e| Error::new(format!("main corpus write: {e}")))?;
            answers = Ok(rendered);
            // Judge the producer facts with main's hold, using this release's own
            // baseline. A red is recorded alongside the readers and cannot stop
            // already-public producer bytes from landing.
            let hold_start = std::time::Instant::now();
            let field_hold = match run(command()
                .args(["--ignored", "--exact", FIELD_HOLD_TEST])
                .env(
                    FIELD_HOLD_RELEASE,
                    tree.join(HANDOFF_FIXTURE_ROOT).join(pins.tag()),
                )) {
                Ok(out) => {
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    let (passed, failed, _) = crate::gates::libtest_tally(&stdout);
                    if out.status.success() && passed == 1 && failed == 0 {
                        "field hold: PASS".to_string()
                    } else {
                        format!(
                            "field hold: FAIL (passed {passed}, failed {failed}): {} {}",
                            stdout.split_whitespace().collect::<Vec<_>>().join(" "),
                            String::from_utf8_lossy(&out.stderr)
                                .split_whitespace()
                                .collect::<Vec<_>>()
                                .join(" ")
                        )
                    }
                }
                Err(e) => format!("field hold: COULD NOT RUN: {e}"),
            };
            timings.push(format!(
                "field hold {:.3}s",
                hold_start.elapsed().as_secs_f64()
            ));
            // These modules own the enabled readers of tests/fixtures/handoff,
            // including their indirect helpers and adoption negative controls.
            let test_start = std::time::Instant::now();
            let tested = run(command().args(consumer_args()))
                .map_err(|e| Error::new(format!("main desk consumers: {e}; {field_hold}")))?;
            timings.push(format!("tests {:.3}s", test_start.elapsed().as_secs_f64()));
            let stdout = String::from_utf8_lossy(&tested.stdout);
            let (passed, failed, failing) = crate::gates::libtest_tally(&stdout);
            if !tested.status.success() || failed > 0 || passed == 0 {
                return Err(Error::new(format!(
                    "FAIL: {} (passed {passed}, failed {failed}); {}; {field_hold}",
                    failing.join(", "),
                    String::from_utf8_lossy(&tested.stderr)
                )));
            }
            if field_hold == "field hold: PASS" {
                Ok(format!("PASS ({passed} desk-reader tests); {field_hold}"))
            } else {
                Ok(format!("{field_hold}; PASS ({passed} desk-reader tests)"))
            }
        })();
        let mut reading = Reading {
            answers,
            checks: format!(
                "{}; {}; {:.3}s after-cut",
                result.unwrap_or_else(|e| e.to_string()),
                timings.join(", "),
                started.elapsed().as_secs_f64()
            ),
        };
        if !reading.checks.starts_with("PASS") && !self.failures.contains(&reading.checks) {
            self.failures.push(reading.checks.clone());
        }
        for failure in &self.failures {
            if failure != &reading.checks {
                reading
                    .checks
                    .push_str(&format!("; earlier main reader: {failure}"));
            }
        }
        self.last_reading = inputs.map(|inputs| {
            (
                preview.to_string(),
                pins.clone(),
                targo.to_path_buf(),
                inputs,
                reading.clone(),
            )
        });
        reading
    }
}

/// The modules containing enabled shipped-desk readers, with a bounded thread width.
#[must_use]
pub fn consumer_args() -> [&'static str; 6] {
    [
        "serde_corpus_tests::",
        "restore::tests::",
        "seamless::fixture_tests::",
        "restore_claude_auth_tests::",
        "--test-threads",
        "1",
    ]
}

/// Disposition of shipped-state answers during a fixture landing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answers {
    /// Number of cases recorded in [`SHIPPED_STATE_CORPUS`].
    Recorded(usize),
    /// Answers omitted from the commit with the reason why.
    Unrecorded(String),
}

impl Answers {
    fn words(&self) -> String {
        match self {
            Answers::Recorded(cases) => format!("with its {cases} shipped-state answers"),
            Answers::Unrecorded(why) => format!(
                "WITHOUT its shipped-state answers ({why}): main's \
                 state_written_by_shipped_producers_answers_as_recorded fails until they are \
                 recorded (ATERM_BLESS_SERDE_CORPUS=1 targo --unverified test -p aterm-gui --lib \
                 -- state_written_by_shipped_producers)"
            ),
        }
    }
}

/// The commit that adds `staged`, its `PINNED_DESKS` rows and, where `tip`'s corpus takes
/// them, its shipped-state answers on top of `tip`.
fn fixture_commit(
    git: &dyn GitRunner,
    tip: &str,
    staged: &Staged,
    pins: &Pins,
    reading: &Reading,
) -> Result<(String, Answers)> {
    let guard = git_ok(
        git,
        &[
            "cat-file",
            "blob",
            &format!("{tip}:{HANDOFF_FIXTURE_GUARD}"),
        ],
    )?
    .stdout_utf8();
    let guard = with_pinned_rows(&guard, &pins.version, &staged.desks)?;
    let mut files: Vec<(String, Vec<u8>)> = files_under(&staged.dir)?
        .into_iter()
        .map(|(rel, bytes)| {
            (
                format!("{HANDOFF_FIXTURE_ROOT}/{}/{rel}", pins.tag()),
                bytes,
            )
        })
        .collect();
    files.push((HANDOFF_FIXTURE_GUARD.to_string(), guard.into_bytes()));
    let answers = reading
        .answers
        .as_ref()
        .map_err(Error::new)
        .and_then(|answers| {
            let corpus = git_ok(
                git,
                &["cat-file", "blob", &format!("{tip}:{SHIPPED_STATE_CORPUS}")],
            )?
            .stdout_utf8();
            with_release_answers(&corpus, answers, &pins.tag())
        });
    let answers = match answers {
        Ok((corpus, cases)) => {
            files.push((SHIPPED_STATE_CORPUS.to_string(), corpus.into_bytes()));
            Answers::Recorded(cases)
        }
        Err(why) => Answers::Unrecorded(why.to_string()),
    };
    let mut blobs = Vec::new();
    for (path, bytes) in &files {
        let at = format!("--path={path}");
        let blob = git.git_input(&["hash-object", "-w", "--stdin", at.as_str()], bytes)?;
        if !blob.success() {
            return Err(Error::new(format!(
                "git hash-object {path} failed (exit {}): {}",
                blob.status,
                blob.stderr_utf8().trim()
            )));
        }
        blobs.push((path.as_str(), blob.stdout_utf8().trim().to_string()));
    }
    let base = rev_parse(git, &format!("{tip}^{{tree}}"))?;
    let edits: Vec<(&str, &str)> = blobs.iter().map(|(p, b)| (*p, b.as_str())).collect();
    let tree = tree_with(git, Some(&base), &edits)?;
    let recorded = match &answers {
        Answers::Recorded(cases) => format!(
            "Its {cases} cases in the serde corpus's sweep of every shipped\nproducer's bytes \
             ({SHIPPED_STATE_CORPUS}),\nas main's reader reads them, ride with them.\n"
        ),
        Answers::Unrecorded(why) => format!(
            "Its cases in the serde corpus's sweep of every shipped producer's\nbytes \
             ({SHIPPED_STATE_CORPUS})\nare NOT recorded here: {why}\n"
        ),
    };
    let message = format!(
        "test(handoff): {tag}'s producer fixtures, written by its own cut\n\n\
         {desks} desks{rendezvous} from build {build} ({short}), written before the claim \
         by the\nin-tree generator ({GENERATOR_SOURCE}) in the\nrelease's cut tree, and \
         the PINNED_DESKS rows that pin them. The next cut's\nhandoff fixture gate reads \
         them ({HANDOFF_FIXTURE_README}).\n{recorded}\nMain reader checks: {checks}\n",
        checks = reading.checks,
        tag = pins.tag(),
        desks = staged.desks.len(),
        rendezvous = if staged.rendezvous {
            " and rendezvous.toml"
        } else {
            ""
        },
        build = pins.build,
        short = pins.short(),
    );
    let commit = git_ok(git, &["commit-tree", &tree, "-p", tip, "-m", &message])?
        .stdout_utf8()
        .trim()
        .to_string();
    Ok((commit, answers))
}

/// Outcome of [`land`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Landed {
    /// `origin/main` already held the release's fixtures at `tip`.
    Already { tip: String, desks: Vec<String> },
    /// Fixtures were committed and pushed to `origin/main`.
    Pushed {
        commit: String,
        /// Number of reader judgments, excluding disjoint compositions.
        attempt: u32,
        answers: Answers,
        /// Main's consumer test verdict; a red never prevents landing producer truth.
        checks: String,
    },
}

impl Landed {
    /// Formats a transcript summary of the landing.
    #[must_use]
    pub fn words(&self, pins: &Pins) -> String {
        match self {
            Landed::Already { tip, desks } => format!(
                "{}'s handoff fixtures are on main already ({} desks at {})",
                pins.tag(),
                desks.len(),
                tip.get(..12).unwrap_or(tip)
            ),
            Landed::Pushed {
                commit,
                attempt,
                answers,
                checks,
            } => format!(
                "{}'s handoff fixtures landed on main as {}{}, {}; main reader: {}",
                pins.tag(),
                commit.get(..12).unwrap_or(commit),
                if *attempt > 1 {
                    format!(" ({attempt} reader judgments: fixture files changed meanwhile)")
                } else {
                    String::new()
                },
                answers.words(),
                checks
            ),
        }
    }
}

/// Commits and pushes `staged` fixtures onto `origin/main` with compare-and-swap retry.
///
/// A native compare-and-swap conflict composes onto the winner's tip when its changed
/// files are disjoint from the fixture edit and Git's merge is clean. Overlapping edits
/// re-run the reader; a merge conflict or transport failure stops at once. There is no
/// attempt limit: another writer's progress does not invalidate this release's fixtures.
/// A push that succeeds is verified: origin/main is the commit, or has it.
///
/// # Errors
/// Each stop above, a push origin/main does not hold, a tip whose fixture directory for the
/// release is someone else's ([`on_commit`]) or whose `PINNED_DESKS` cannot take the
/// release's rows ([`with_pinned_rows`]), a staged fixture directory that cannot be read,
/// and any git failure.
pub fn land(
    git: &dyn GitRunner,
    origin: &dyn Origin,
    staged: &Staged,
    pins: &Pins,
    read_main: &mut dyn FnMut(&str) -> Reading,
) -> Result<Landed> {
    let mut attempt = 0_u32;
    let mut tip = origin.refresh_main().map_err(|e| {
        Error::new(format!(
            "handoff fixtures: cannot reach origin to land them: {e}"
        ))
    })?;
    loop {
        attempt = attempt.saturating_add(1);
        if let Some(found) = on_commit(git, &tip, &pins.version)? {
            return Ok(Landed::Already {
                tip,
                desks: found.desks,
            });
        }
        let pending = Reading {
            answers: Err("main reader has not run".into()),
            checks: "NOT RUN".into(),
        };
        let (preview, _) = fixture_commit(git, &tip, staged, pins, &pending)?;
        let reading = read_main(&preview);
        let (mut commit, answers) = fixture_commit(git, &tip, staged, pins, &reading)?;
        let checked = preview;
        loop {
            let written = origin.write("refs/heads/main", Some(&tip), &commit).map_err(|why| {
            Error::new(format!("handoff fixtures: native main write failed; refusing to reinterpret it as a known compare-and-swap conflict: {why}"))
        })?;
            if written == RefWrite::Written {
                let now = origin.refresh_main().map_err(Error::new)?;
                let holds = now == commit
                    || git
                        .git(&["merge-base", "--is-ancestor", &commit, &now])?
                        .success();
                if !holds {
                    return Err(Error::new(format!(
                        "handoff fixtures: the push of {commit} succeeded, and origin/main ({now}) \
                     does not hold it — run the release again, which lands them if they are \
                     still missing"
                    )));
                }
                return Ok(Landed::Pushed {
                    commit,
                    attempt,
                    answers,
                    checks: reading.checks,
                });
            }
            let next = origin.refresh_main().map_err(Error::new)?;
            if next == tip {
                return Err(Error::new(
                    "handoff fixtures: compare-and-swap conflict without main advancing",
                ));
            }
            if let Some(found) = on_commit(git, &next, &pins.version)? {
                return Ok(Landed::Already {
                    tip: next,
                    desks: found.desks,
                });
            }
            let merged = git_ok(
                git,
                &[
                    "merge-tree",
                    "--write-tree",
                    "--no-messages",
                    &commit,
                    &next,
                ],
            )?
            .stdout_utf8();
            let tree = merged
                .lines()
                .next()
                .ok_or_else(|| Error::new("handoff fixtures: merge returned no tree"))?;
            let files = |from: &str, to: &str| -> Result<std::collections::BTreeSet<Vec<u8>>> {
                Ok(git_ok(
                    git,
                    &[
                        "diff-tree",
                        "-r",
                        "-z",
                        "--name-only",
                        "--no-renames",
                        "--ignore-submodules=none",
                        from,
                        to,
                    ],
                )?
                .stdout
                .split(|byte| *byte == 0)
                .filter(|path| !path.is_empty())
                .map(<[u8]>::to_vec)
                .collect())
            };
            if !files(&tip, &commit)?.is_disjoint(&files(&tip, &next)?) {
                tip = next;
                break;
            }
            let message = format!(
                "test(handoff): compose {}'s checked fixtures onto main\n\nFixture-Reader-Checked: {checked}\nFixture-Composed-Onto: {next}\nMain reader checks: {}\n",
                pins.tag(),
                reading.checks
            );
            commit = git_ok(
                git,
                &[
                    "commit-tree",
                    tree,
                    "-p",
                    &commit,
                    "-p",
                    &next,
                    "-m",
                    &message,
                ],
            )?
            .stdout_utf8()
            .trim()
            .to_owned();
            tip = next;
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::files_under;

    #[test]
    fn files_under_ignores_symlinks() {
        let dir = aterm_tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/a.txt"), b"ok").unwrap();
        std::os::unix::fs::symlink(dir.path(), dir.path().join("sub/cycle")).unwrap();
        std::os::unix::fs::symlink(
            dir.path().join("sub/a.txt"),
            dir.path().join("sub/link.txt"),
        )
        .unwrap();
        let found = files_under(dir.path()).unwrap();
        assert_eq!(found, vec![("sub/a.txt".to_string(), b"ok".to_vec())]);
    }
}
