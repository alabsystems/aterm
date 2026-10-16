// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The release's own handoff fixtures: what this release's producer writes, recorded by its
//! own cut and landed on main for the next release's handoff fixture gate
//! ([`crate::gates::handoff_fixture_gate_at`]). Only release N's producer can write the
//! bytes that check N+1's consumer (`crates/aterm-gui/tests/fixtures/handoff/README.md`),
//! so N's cut writes them in two halves:
//!
//! * [`stage`], before the claim (beside the `ship release` build): the in-tree
//!   generator ([`GENERATOR_SOURCE`], two `#[ignore]`d libtests) runs in the clone under
//!   the reserved identity's pins (release commit C and build n, and the prover's
//!   `TRUST_STAGE2_BIN`) into the release cache, self-checking every desk against the
//!   release's own consumer. A refusal, or a missing required desk, refuses the release
//!   with nothing claimed.
//! * [`land`], after the cut: one commit on origin/main's tip adding `vX.Y.0/`
//!   and its `PINNED_DESKS` rows, pushed as a compare-and-swap like the claim's
//!   ([`crate::ledger::claim`]). A tip that already holds them is a no-op, so a re-run
//!   resumes.
//!
//! A staging is keyed by the reserved identity
//! (`<cache>/handoff-fixtures/v<version>-<n>-<C12>/`) and its mark ([`STAGED_MARK`]) is
//! written last, so a re-reserved identity or a half-written staging is written afresh.

use aterm_uds::spawnfd::Command;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use crate::gates::{
    GateTree, HANDOFF_FIXTURE_GUARD, HANDOFF_FIXTURE_GUARD_TARGET, HANDOFF_FIXTURE_README,
    HANDOFF_FIXTURE_ROOT, HandoffFixtures,
};
use crate::ledger::{Error, GitRunner, Result, git_ok, rev_parse};

/// The generator, compiled into every `aterm-gui` library test build.
pub const GENERATOR_SOURCE: &str = "crates/aterm-gui/src/seamless_fixture_generator_tests.rs";

/// Its two entry tests: the desks, and the rendezvous record (macOS only, as the
/// rendezvous is).
pub const GENERATOR_TESTS: [&str; 2] = ["generate_handoff_fixtures", "generate_rendezvous_fixture"];

/// The mark a complete staging carries, written last: the identity it was written as.
pub const STAGED_MARK: &str = "staged";

/// The identity the fixtures are written as: what the shipped binary stamps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pins {
    /// `X.Y.0`.
    pub version: String,
    /// The reserved build number n.
    pub build: u64,
    /// The release commit C (full): the commit the tag names and the binary stamps.
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

/// A complete staging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Staged {
    /// `<staging>/v<version>`: what lands as `crates/aterm-gui/tests/fixtures/handoff/v<version>`.
    pub dir: PathBuf,
    /// Its desks (each a directory holding a `parent.toml`), sorted.
    pub desks: Vec<String>,
    /// It holds `rendezvous.toml`.
    pub rendezvous: bool,
    /// Written by this call (else found staged).
    pub fresh: bool,
}

impl Staged {
    /// The transcript's words for it.
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

/// The staging of `pins` under `root`, when it is complete.
#[must_use]
pub fn staged(root: &Path, pins: &Pins) -> Option<Staged> {
    let at = pins.staged_in(root);
    if fs::read_to_string(at.join(STAGED_MARK)).ok()? != pins.mark() {
        return None;
    }
    let dir = at.join(pins.tag());
    let desks = desks_in(&dir).ok()?;
    (!desks.is_empty()).then(|| Staged {
        rendezvous: dir.join("rendezvous.toml").is_file(),
        dir,
        desks,
        fresh: false,
    })
}

/// The directories directly under `dir` holding a `parent.toml`, sorted.
fn desks_in(dir: &Path) -> std::io::Result<Vec<String>> {
    let mut desks = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.path().join("parent.toml").is_file() {
            desks.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    desks.sort();
    Ok(desks)
}

/// The libtest arguments the generator runs with: its two tests by name, which are
/// `#[ignore]`d, one at a time (both install the process's build stamp).
#[must_use]
pub fn generator_args() -> Vec<&'static str> {
    let mut args = GENERATOR_TESTS.to_vec();
    args.extend(["--ignored", "--test-threads", "1"]);
    args
}

/// Stage the release's fixtures: run the in-tree generator in `tree` (the cut tree) with
/// `targo`, under `pins`, into `root` ([`crate::snapshot::CacheRoot::handoff_fixtures`]),
/// unless that identity is staged already. `run` is the process runner, so a test can
/// hand it any outcome.
///
/// Refuses when the generator does not pass both its tests (one off macOS, which has no
/// rendezvous), when a desk `REQUIRED_DESKS` asks of this release is missing, and when a
/// desk's `parent.toml` was not written as `pins`.
///
/// # Errors
/// Each refusal above, and a staging directory that cannot be written.
pub fn stage(
    targo: &Path,
    tree: &Path,
    root: &Path,
    pins: &Pins,
    run: &mut dyn FnMut(&mut Command) -> std::io::Result<Output>,
) -> Result<Staged> {
    if let Some(found) = staged(root, pins) {
        return Ok(found);
    }
    let io = |what: &str, path: &Path, e: std::io::Error| {
        Error::new(format!("handoff fixtures: {what} {}: {e}", path.display()))
    };
    // A new identity: every older staging of this version is a reservation passed.
    if let Ok(entries) = fs::read_dir(root) {
        let prefix = format!("{}-", pins.tag());
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with(&prefix) {
                crate::gates::clear_path(&entry.path())
                    .map_err(|e| io("cannot clear", &entry.path(), e))?;
            }
        }
    }
    let at = pins.staged_in(root);
    let dir = at.join(pins.tag());
    fs::create_dir_all(&dir).map_err(|e| io("cannot create", &dir, e))?;
    let target = tree.join(HANDOFF_FIXTURE_GUARD_TARGET);
    crate::gates::clear_path(&target).map_err(|e| io("cannot clear", &target, e))?;
    println!(
        "==> handoff fixtures: {} build {} ({}) written by its own producer (targo \
         --unverified test -p aterm-gui --lib -- {})",
        pins.tag(),
        pins.build,
        pins.short(),
        generator_args().join(" ")
    );
    let mut command = crate::gates::aterm_gui_lib_test(targo, tree, &target, &generator_args());
    command
        .env("ATERM_BUILD_GIT_COMMIT", pins.short())
        .env("ATERM_BUILD_GIT_COMMIT_FULL", &pins.commit)
        .env("SOURCE_DATE_EPOCH", pins.build.to_string())
        .env("ATERM_HANDOFF_FIXTURE_OUT", &dir)
        .env("ATERM_RENDEZVOUS_FIXTURE_OUT", &dir);
    let outcome = run(&mut command);
    if let Err(error) = crate::gates::clear_path(&target) {
        println!(
            "==> handoff fixtures: could not remove {} ({error}); remove it by hand",
            target.display()
        );
    }
    let output = outcome.map_err(|e| {
        Error::new(format!(
            "handoff fixtures: cannot run {}: {e}",
            targo.display()
        ))
    })?;
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
    let guard = fs::read_to_string(&guard_path).map_err(|e| io("cannot read", &guard_path, e))?;
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
    for desk in &desks {
        let parent = dir.join(desk).join("parent.toml");
        let text = fs::read_to_string(&parent).map_err(|e| io("cannot read", &parent, e))?;
        for line in [
            format!("producer_version = \"{}\"", pins.version),
            format!("producer_commit = \"{}\"", pins.short()),
            format!("outgoing_build = {}", pins.build),
        ] {
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
    fs::write(&mark, pins.mark()).map_err(|e| io("cannot write", &mark, e))?;
    Ok(Staged {
        dir,
        desks,
        rendezvous,
        fresh: true,
    })
}

/// What `commit` holds of release `version`'s fixtures: `None` without a `v<version>/`
/// directory, else what the next cut's gate finds there. A directory the gate refuses is
/// an error: it is someone else's, never this release's to overwrite.
///
/// # Errors
/// Such a directory, and a git failure.
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

/// `guard` (the source of [`HANDOFF_FIXTURE_GUARD`]) with one `PINNED_DESKS` row per desk
/// of `v<version>` appended. Refuses a table that pins the release already or that does
/// not read back.
///
/// # Errors
/// As above.
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

/// `base`'s tree (none: empty) with each `(path, blob)` of `files` set, at any depth, made
/// from git objects alone, with no worktree or index.
///
/// # Errors
/// A path through a file, and any git failure.
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
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = if rel.is_empty() {
                name
            } else {
                format!("{rel}/{name}")
            };
            if entry.path().is_dir() {
                stack.push((entry.path(), path));
            } else {
                let bytes = fs::read(entry.path())
                    .map_err(|e| Error::new(format!("read {}: {e}", entry.path().display())))?;
                out.push((path, bytes));
            }
        }
    }
    out.sort();
    Ok(out)
}

/// The commit that adds `staged` and its `PINNED_DESKS` rows on top of `tip`.
fn fixture_commit(git: &dyn GitRunner, tip: &str, staged: &Staged, pins: &Pins) -> Result<String> {
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
    let message = format!(
        "test(handoff): {tag}'s producer fixtures, written by its own cut\n\n\
         {desks} desks{rendezvous} from build {build} ({short}), written before the claim \
         by the\nin-tree generator ({GENERATOR_SOURCE}) in the\nrelease's cut tree, and \
         the PINNED_DESKS rows that pin them. The next cut's\nhandoff fixture gate reads \
         them ({HANDOFF_FIXTURE_README}).\n",
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
    Ok(
        git_ok(git, &["commit-tree", &tree, "-p", tip, "-m", &message])?
            .stdout_utf8()
            .trim()
            .to_string(),
    )
}

/// How [`land`] ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Landed {
    /// origin/main held them already (at this tip): nothing was pushed.
    Already { tip: String, desks: Vec<String> },
    /// This commit is origin/main now, pushed on the `attempt`th try.
    Pushed { commit: String, attempt: u32 },
}

impl Landed {
    /// The transcript's words for it.
    #[must_use]
    pub fn words(&self, pins: &Pins) -> String {
        match self {
            Landed::Already { tip, desks } => format!(
                "{}'s handoff fixtures are on main already ({} desks at {})",
                pins.tag(),
                desks.len(),
                tip.get(..12).unwrap_or(tip)
            ),
            Landed::Pushed { commit, attempt } => format!(
                "{}'s handoff fixtures landed on main as {}{}",
                pins.tag(),
                commit.get(..12).unwrap_or(commit),
                if *attempt > 1 {
                    format!(" (try {attempt}: main moved meanwhile)")
                } else {
                    String::new()
                }
            ),
        }
    }
}

/// Land a staging on origin/main: fetch; when the tip already holds the release's fixtures
/// ([`on_commit`]) do nothing; else commit `staged` and its `PINNED_DESKS` rows on the tip
/// and push it as a fast-forward of main. A non-fast-forward rejection is a lost race:
/// rebuild on the winner's tip, up to `max_attempts` tries; any other push failure stops at
/// once. A push is verified as the claim's is: origin/main is the commit, or has it.
///
/// # Errors
/// Each stop above, a tip whose fixture directory for the release is someone else's
/// ([`on_commit`]), and any git failure.
pub fn land(
    git: &dyn GitRunner,
    staged: &Staged,
    pins: &Pins,
    max_attempts: u32,
) -> Result<Landed> {
    let mut attempt = 0_u32;
    loop {
        attempt += 1;
        git_ok(git, &["fetch", "origin", "main"]).map_err(|e| {
            Error::new(format!(
                "handoff fixtures: cannot reach origin to land them: {e}"
            ))
        })?;
        let tip = rev_parse(git, "origin/main")?;
        if let Some(found) = on_commit(git, &tip, &pins.version)? {
            return Ok(Landed::Already {
                tip,
                desks: found.desks,
            });
        }
        let commit = fixture_commit(git, &tip, staged, pins)?;
        let push = git.git(&["push", "origin", &format!("{commit}:refs/heads/main")])?;
        if push.success() {
            git_ok(git, &["fetch", "origin", "main"])?;
            let now = rev_parse(git, "origin/main")?;
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
            return Ok(Landed::Pushed { commit, attempt });
        }
        let why = push.stderr_utf8().trim().to_string();
        let lost_race = why.contains("non-fast-forward")
            || why.contains("fetch first")
            || why.contains("[rejected]");
        if !lost_race {
            return Err(Error::new(format!(
                "handoff fixtures: git push origin main failed (not a fast-forward rejection, \
                 so retrying cannot help): {why}"
            )));
        }
        if attempt >= max_attempts {
            return Err(Error::new(format!(
                "handoff fixtures: main moved under the push {attempt} times; run the release \
                 again, which lands them on the tip it finds. Last rejection: {why}"
            )));
        }
    }
}
