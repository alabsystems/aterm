// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Keeping Rust build output out of Spotlight's index (§9, storage hygiene) — the
//! MEASURING half of the pair whose other half is [`crate::freespace`].
//!
//! On 2026-09-01 01:54 WindowServer's main thread blocked 40 s in a synchronous TCC
//! preflight (`com.apple.tcc.preflight.kTCCServiceListenEvent`) waiting on tccd; tccd was
//! TH_UNINT on an APFS volume rwlock contended by 18 rustc processes, syspolicyd
//! (Gatekeeper, 86% CPU, background QoS + IO tier 2) and `mds` grinding 2.0 TB of Rust
//! build output across 127 target dirs. The watchdog killed WindowServer and the GUI
//! session died. Spotlight indexing of build output is ONE OF THE TWO amplifiers that made
//! a busy build fatal. This module removes that one, per-directory, and NEVER by disabling
//! Spotlight globally.
//!
//! # The mechanism, and why nothing here trusts it
//!
//! Measured on macOS 26.6.2 (25G83) on 2026-09-01/02 by A/B test with identical files
//! planted in each location and read back with `mdfind`, every test file confirmed present
//! on disk:
//!
//! * a directory whose name ends `.noindex` is NOT indexed, and neither is its whole
//!   subtree (checked 6 levels deep). A sibling of it stays searchable.
//! * a directory hidden by a leading dot is NOT indexed, and that too is transitive to the
//!   whole subtree (re-measured 2026-09-02) — which is what makes [`scan`]'s pruning of
//!   dot-directories a CORRECTNESS argument and not merely an optimization.
//! * a `.metadata_never_index` marker file in a subdirectory is **INERT**. It is the answer
//!   given in most blog posts and it silently does nothing. An earlier attempt in this same
//!   investigation planted 189 of them and reported success, because its "verification" ran
//!   `mdfind` against directories that were empty or dot-hidden for unrelated reasons.
//!   Nothing here writes one.
//! * a dot in the MIDDLE of a name (`d_.subdot`) has no effect.
//!
//! PROVENANCE: `.noindex` is verified-on-this-machine, NOT a documented Apple API. A search
//! of `/System/Library`, `/Library` and `~/Library` on this machine found ZERO `.noindex`
//! directories, so the common claim that "Xcode uses it" could not be corroborated here. It
//! is an observed behaviour that could change — which is exactly why [`verify`] MEASURES
//! instead of asserting. A utility that pronounced a directory excluded because its name
//! ends in the right five characters would ship the `.metadata_never_index` bug again. The
//! one test that can catch macOS changing this is `manual_probe_round_trip_against_the_real_index`
//! at the bottom of this file; it is `#[ignore]`d because it needs a genuinely indexed
//! location (the developer's own home). Run it after an OS update.
//!
//! # The three answers, and their asymmetry
//!
//! [`Verdict::Indexed`] is PROOF (the index returned the probe). [`Verdict::Excluded`] is a
//! bounded INFERENCE (a miss, relative to a control planted later that DID appear).
//! [`Verdict::Unknown`] exists so the inference is never forced: with no control there is no
//! clock, and a not-yet-indexed file is indistinguishable from an excluded one. Every
//! failure path in [`verify`] lands on `Unknown`, never on `Excluded`.
//!
//! PATHS ARE RESOLVED BEFORE ANYTHING IS MEASURED OR RENAMED. [`verify`] canonicalizes and
//! [`migrate`] refuses a symlink outright, because `target -> /Volumes/fast/realtarget` is
//! what someone with 2 TB of build output actually does: the probe would be written through
//! the link and indexed at its real path, the query would be scoped at the LINK's parent
//! and never see it, and `rename(2)` would move the link while the build output stayed
//! where it was. Both halves would then agree — measured, confident, and wrong — that a
//! tree Spotlight is actively indexing is excluded (measured 2026-09-02).
//!
//! # Git checkouts: the symlink, never the config (2026-09-10)
//!
//! Re-pointing cargo by writing `[build] target-dir` into `.cargo/config.toml` is right
//! for a repo nobody versions and WRONG for a git checkout: the aterm repo's
//! `.cargo/config.toml` is TRACKED (every worktree's with it) and the release cutter
//! (`crates/aterm-release/src/gates.rs::clean_tree`, `git status --porcelain`) refuses a
//! dirty tree — so the first pass after landing would have broken every cut, and an
//! untracked `.cargo/config.toml` in any other checkout is the same `??` row. In a git
//! checkout ([`is_git_checkout`]: a `.git` at the repo root, or `git rev-parse` succeeding
//! for a crate nested inside one) [`apply_one`] therefore renames `target ->
//! target.noindex`, leaves a RELATIVE symlink `target -> target.noindex` where the
//! directory was, and never opens the config: cargo keeps writing to `<repo>/target` and
//! the bytes land under the excluded name. The config edit is kept for repos that are not
//! under git. `git status --porcelain` stays empty because the NEW name — which a
//! `target` pattern does NOT match — is appended to the clone's own `.git/info/exclude`
//! (per clone, outside the working tree, shared by its worktrees) unless git already
//! ignores it, and the LINK is either caught by the same `.gitignore` entry that caught
//! the directory or, when that entry is directory-only (`target/`, `/target-tippy/`,
//! `**/target/` — the shape 20 of the 26 planned dirs on m21 carry, and the aterm
//! checkout's own `/target-tippy/`), which matches a directory and NOT the symlink that
//! replaces it, excluded the same way: the directory is probed with `git check-ignore`
//! BEFORE the rename, and a link that git no longer ignores where the directory was gets
//! its own `.git/info/exclude` line, read back through git. Status is exactly what it
//! was. A repo that did not ignore the directory at all gets the same link and one line
//! saying the link shows as untracked, exactly as the directory did.
//!
//! THE CUTTER ACCEPTS EXACTLY THIS LINK AND NO OTHER (2026-09-12). The release build runs
//! under `<repo>/target` and guards that root against redirection (a git-ignored link
//! there would aim residue GC and the artifact paths anywhere); v0.83.0's cut stopped on
//! the link this pass leaves, and the operator had to undo the migration by hand.
//! `crates/aterm-release/src/buildplan.rs::resolve_release_target_root` now accepts
//! `target -> target.noindex` when the referent is that exact name, the twin is a real
//! directory in the same parent on the same filesystem, and it passes the plain root's
//! ownership and mode checks — then derives every path from the twin. Change the SHAPE
//! this function lays (an absolute link, a different suffix, a hop) and the cutter
//! refuses again.
//!
//! Measured on macOS 26.6.2 (25G83) on 2026-09-10 with a scratch tree under `$HOME`
//! (`repo/target/debug/<token>.txt`, `mdimport`ed, then polled with `mdfind -onlyin`):
//!
//! * the file under the plain `target/` was returned within 1 s of the import;
//! * after `mv target target.noindex && ln -s target.noindex target` the same query
//!   returned nothing within 1 s — the index follows the REAL path, and the rename took
//!   the entry out (the tree was not re-imported);
//! * `mdfind -onlyin repo/target` (the symlink) returned nothing;
//! * a NEW file written THROUGH the symlink and `mdimport`ed by its symlink spelling was
//!   still absent 20 s later, while a control planted in a plain sibling directory at the
//!   same moment was returned within 1 s. A symlink is not a way into the index: `mds`
//!   records real paths, and the real path ends `.noindex`.
//!
//! Non-macOS is a clean no-op in every entry point: [`scan`] returns an empty complete
//! scan, [`migrate`] returns [`Migration::NotApplicable`], [`verify`] returns
//! [`Verdict::NotApplicable`]. Callers need no `cfg` — [`crate::doctor`] has none.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Whether this build can measure or change Spotlight exposure at all.
pub const SUPPORTED: bool = cfg!(target_os = "macos");

/// The suffix that makes a directory and its whole subtree invisible to Spotlight.
pub const NOINDEX_SUFFIX: &str = ".noindex";

/// The first line cargo writes into `<target>/CACHEDIR.TAG` (RFC-ish cache-dir tag).
/// Read off `/Users//example/.cargo-target-m7c/CACHEDIR.TAG` on 2026-09-02.
pub const CACHEDIR_TAG_SIGNATURE: &str = "Signature: 8a477f597d28d172789f06886806bc55";

/// How deep `doctor`'s ambient scan of `$HOME` goes. Three reaches `~/<repo>/target` and
/// `~/src/<repo>/target`, which is where repos actually live; deeper is the deliberate
/// `aterm pkg noindex scan <root>`, which the doctor line names.
pub const DOCTOR_DEPTH: usize = 3;

/// How deep the verb goes when the user names a root — the walk is deliberate, so it may be
/// long, but it is still bounded.
pub const VERB_DEPTH: usize = 6;

/// Directory names never descended into. `Library` and `Applications` hold no cargo output
/// and are large; `node_modules` is the other cheap prune. (`.Trash` is already covered by
/// the dot rule; it is listed because the reader should not have to derive that.)
///
/// THE macOS-PROTECTED FOLDERS ARE PRUNED TOO (2026-09-10). `Documents`, `Desktop`,
/// `Downloads`, `Pictures`, `Movies` and `Music` are the folders macOS guards with a
/// per-folder consent dialog, and it attributes that dialog to the terminal — so the
/// automatic pass at the end of every seed/update walked `$HOME` three levels deep and
/// raised, in aterm's own name, exactly the prompts the consent design exists to
/// consolidate. No cargo `target/` the automatic pass should touch lives in any of them;
/// a user who keeps one there names it to the verb, which is deliberate and may prompt.
const SKIP_DIRS: &[&str] = &[
    "Library",
    "Applications",
    "node_modules",
    ".Trash",
    "Documents",
    "Desktop",
    "Downloads",
    "Pictures",
    "Movies",
    "Music",
];

/// Ceiling on the `CACHEDIR.TAG` read. Cargo's is 177 bytes; anything larger is not
/// cargo's, and reading it as "no tag" is the fail-closed direction — [`migrate`] then
/// refuses the directory rather than renaming something it does not understand.
const MAX_TAG_BYTES: usize = 4096;

// ---------------------------------------------------------------------------------------
// Pure classification (no I/O — the testable core)
// ---------------------------------------------------------------------------------------

/// Why a path is, or is not, invisible to Spotlight — read off the PATH ALONE.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exclusion {
    /// Some component ends `.noindex`. The whole subtree below it is excluded.
    NoindexSuffix,
    /// Some component starts with `.`. Transitive to the whole subtree (measured
    /// 2026-09-02) — so a dot-hidden target dir is ALREADY excluded and needs nothing.
    DotHidden,
    /// Nothing about the name excludes it. This is a CLAIM about the name, not a
    /// measurement of the index — only [`verify`] measures.
    Exposed,
}

/// What `path`'s NAME claims about its Spotlight visibility. Pure; never touches disk.
/// `NoindexSuffix` wins over `DotHidden` when both are present, because it is the form this
/// module creates and the one the report should name.
#[must_use]
pub fn exclusion_of(path: &Path) -> Exclusion {
    let mut dot_hidden = false;
    for c in path.components() {
        let std::path::Component::Normal(name) = c else {
            continue;
        };
        // Lossy is safe for both tests: `.` and `.noindex` are ASCII, and the U+FFFD a
        // lossy conversion can introduce is not.
        let name = name.to_string_lossy();
        if name.ends_with(NOINDEX_SUFFIX) {
            return Exclusion::NoindexSuffix;
        }
        if name.starts_with('.') {
            dot_hidden = true;
        }
    }
    if dot_hidden {
        Exclusion::DotHidden
    } else {
        Exclusion::Exposed
    }
}

/// How a directory was recognized as cargo build output, strongest signal first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Evidence {
    /// `<dir>/CACHEDIR.TAG` opens with [`CACHEDIR_TAG_SIGNATURE`].
    CachedirTag,
    /// `<dir>/.rustc_info.json` is a regular file AND `debug/` or `release/` is a dir.
    /// This arm exists because it is MEASURED: `/Users//example/aterm/target` on 2026-09-02
    /// had `.rustc_info.json` and `debug/` and NO `CACHEDIR.TAG`, so a tag-only rule would
    /// have missed the very tree that motivated the feature.
    RustcInfo,
    /// Both `<dir>/debug` and `<dir>/release` are directories.
    DebugAndRelease,
}

/// Whether `first_line` is cargo's cache-dir tag. Pure, so the signature is pinned by a
/// test rather than by a fixture that could drift.
#[must_use]
pub fn is_cachedir_tag(first_line: &str) -> bool {
    first_line.trim_end().starts_with(CACHEDIR_TAG_SIGNATURE)
}

/// Which of the three signals `dir` carries, or `None`. One `read_dir`-free probe of at
/// most four paths. Recognition is FAIL-CLOSED on purpose: [`migrate`] renames only what
/// this recognizes, so an unrecognized directory is never touched.
#[must_use]
pub fn target_evidence(dir: &Path) -> Option<Evidence> {
    let is_dir = |name: &str| dir.join(name).is_dir();
    if let Ok(text) =
        crate::metadata_io::read_bounded_regular_utf8(&dir.join("CACHEDIR.TAG"), MAX_TAG_BYTES)
        && is_cachedir_tag(text.lines().next().unwrap_or(""))
    {
        return Some(Evidence::CachedirTag);
    }
    let debug = is_dir("debug");
    let release = is_dir("release");
    if (debug || release)
        && std::fs::symlink_metadata(dir.join(".rustc_info.json")).is_ok_and(|m| m.is_file())
    {
        return Some(Evidence::RustcInfo);
    }
    if debug && release {
        return Some(Evidence::DebugAndRelease);
    }
    None
}

/// The migrated form of `dir`: `<name>` -> `<name>.noindex`, in the SAME parent. `None`
/// when `dir` has no file name, or already ends [`NOINDEX_SUFFIX`]. Pure.
///
/// Note the rule is the SUFFIX, not the name: `aterm-cursor-echo-gui-target` migrates to
/// `aterm-cursor-echo-gui-target.noindex` exactly as `target` does. That spelling really
/// exists on this machine.
#[must_use]
pub fn destination(dir: &Path) -> Option<PathBuf> {
    let name = dir.file_name()?;
    if name.to_string_lossy().ends_with(NOINDEX_SUFFIX) {
        return None;
    }
    let mut renamed = name.to_os_string();
    renamed.push(NOINDEX_SUFFIX);
    Some(dir.parent().unwrap_or_else(|| Path::new("")).join(renamed))
}

/// The lines telling the user how to point cargo at `dest`, ready to print one per line.
/// Pure. Names both mechanisms (`CARGO_TARGET_DIR`, `[build] target-dir`) and the
/// `.gitignore` line, because `target/` in an existing ignore file does NOT match
/// `target.noindex/` and the first surprise otherwise is a repo full of untracked objects.
#[must_use]
pub fn cargo_hint(dest: &Path) -> Vec<String> {
    let shown = dest.display();
    let name = dest.file_name().map_or_else(
        || String::from("target.noindex"),
        |n| n.to_string_lossy().into_owned(),
    );
    vec![
        format!("point cargo at it for this shell: export CARGO_TARGET_DIR={shown}"),
        format!("or durably, in .cargo/config.toml: [build] target-dir = \"{shown}\""),
        format!(
            "and add `{name}/` to .gitignore — an existing `target/` line does NOT match \
             `target.noindex/`"
        ),
    ]
}

// ---------------------------------------------------------------------------------------
// Discovery and sizing
// ---------------------------------------------------------------------------------------

/// A wall-clock and entry-count ceiling for a bounded walk. Both are honoured; whichever
/// trips first ends the walk and marks the result incomplete.
#[derive(Debug, Clone, Copy)]
pub struct Budget {
    /// Directory entries the walk may look at before it gives up.
    pub max_entries: usize,
    /// Wall clock the walk may spend before it gives up.
    pub max_wall: Duration,
}

impl Budget {
    /// `doctor`'s ambient discovery ceiling: 20 000 directories, 1.5 s. Directory reads
    /// only — no file stats — so this covers a large home in practice.
    pub const DOCTOR: Self = Self {
        max_entries: 20_000,
        max_wall: Duration::from_millis(1500),
    };
    /// `doctor`'s ceiling for summing exposed bytes, shared across ALL exposed targets:
    /// 200 000 files, 1 s. A 2 TB tree is never fully walked here; the report says
    /// "at least".
    pub const DOCTOR_SIZE: Self = Self {
        max_entries: 200_000,
        max_wall: Duration::from_secs(1),
    };
    /// The verb's ceiling, when the user asked for the answer: 200 000 dirs, 20 s.
    pub const VERB: Self = Self {
        max_entries: 200_000,
        max_wall: Duration::from_secs(20),
    };
    /// The verb's sizing ceiling: 5 000 000 files, 30 s.
    pub const VERB_SIZE: Self = Self {
        max_entries: 5_000_000,
        max_wall: Duration::from_secs(30),
    };
}

/// One discovered cargo target directory.
#[derive(Debug, Clone)]
pub struct Target {
    /// Absolute path as walked. Never a symlink — [`scan`] does not follow them.
    pub path: PathBuf,
    /// Which signal recognized it.
    pub evidence: Evidence,
    /// What its name claims about indexing.
    pub exclusion: Exclusion,
}

impl Target {
    /// Whether Spotlight is (by name) free to index this tree.
    #[must_use]
    pub const fn exposed(&self) -> bool {
        matches!(self.exclusion, Exclusion::Exposed)
    }
}

/// The result of one bounded walk.
#[derive(Debug, Clone)]
pub struct Scan {
    /// The root walked.
    pub root: PathBuf,
    /// Every target dir found, sorted by path — a stable report order, not `read_dir`'s.
    pub targets: Vec<Target>,
    /// `false` when a budget ran out, so the caller must say "at least" and never
    /// "there are none".
    pub complete: bool,
}

impl Scan {
    /// The exposed subset, in report order.
    pub fn exposed(&self) -> impl Iterator<Item = &Target> {
        self.targets.iter().filter(|t| t.exposed())
    }

    /// `(exposed, hidden)` counts.
    #[must_use]
    pub fn counts(&self) -> (usize, usize) {
        let exposed = self.exposed().count();
        (exposed, self.targets.len() - exposed)
    }
}

/// Walk `root` to `max_depth`, bounded by `budget`, collecting cargo target dirs.
///
/// Rules, each load-bearing: directories only (no file stats — that is [`size_of`]'s job);
/// `symlink_metadata` throughout so a symlink is never followed and never counted twice; a
/// recognized target dir is RECORDED and not descended into (its contents are millions of
/// files and none of them is another target dir); dot-directories are pruned, which is
/// sound because their whole subtree is already excluded (measured 2026-09-02); [`SKIP_DIRS`]
/// pruned; anything unreadable skipped in silence.
///
/// `root` itself is tested first, so `aterm pkg noindex scan ~/aterm/target` answers about
/// the directory the user named rather than about its children.
///
/// On non-macOS returns an empty, `complete` scan — so callers need no `cfg`.
#[must_use]
pub fn scan(root: &Path, max_depth: usize, budget: &Budget) -> Scan {
    let mut out = Scan {
        root: root.to_path_buf(),
        targets: Vec::new(),
        complete: true,
    };
    if !SUPPORTED {
        return out;
    }
    // A ROOT THAT IS NOT A READABLE DIRECTORY IS NOT AN EMPTY TREE. Without this, a typo
    // (`~/srcc` for a `~/src` holding 40 exposed target dirs) walks nothing: `read_dir`
    // fails, the loop drains, and the function returns `targets: []` with `complete: true`
    // — the flag whose whole job is to license the caller to say "there are none". The CLI
    // then prints "no cargo target directories under …" at exit 0: a confident false clean
    // over a path that was never read, the same shape as the 189 inert markers this module
    // exists to prevent. `metadata` (following, not `symlink_metadata`) because a symlinked
    // root is a directory the user deliberately named.
    if !std::fs::metadata(root).is_ok_and(|m| m.is_dir()) {
        out.complete = false;
        return out;
    }
    if let Some(evidence) = target_evidence(root) {
        out.targets.push(Target {
            path: root.to_path_buf(),
            evidence,
            exclusion: exclusion_of(root),
        });
        return out;
    }
    let start = Instant::now();
    let mut entries = 0usize;
    let mut stack: Vec<(PathBuf, usize)> = vec![(root.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read.flatten() {
            if entries >= budget.max_entries || start.elapsed() >= budget.max_wall {
                out.complete = false;
                stack.clear();
                break;
            }
            let path = entry.path();
            // symlink_metadata, never metadata: a symlink to a directory is a symlink here,
            // so `root/loop -> root` is skipped rather than walked forever.
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if !meta.is_dir() {
                continue;
            }
            entries += 1;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') || SKIP_DIRS.contains(&name.as_ref()) {
                continue;
            }
            if let Some(evidence) = target_evidence(&path) {
                out.targets.push(Target {
                    exclusion: exclusion_of(&path),
                    path,
                    evidence,
                });
                continue;
            }
            if depth + 1 < max_depth {
                stack.push((path, depth + 1));
            }
        }
    }
    out.targets.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// Bytes, and whether the walk finished.
#[derive(Debug, Clone, Copy)]
pub struct Size {
    /// Sum of regular-file lengths seen before the budget ran out.
    pub bytes: u64,
    /// `false` when a budget ran out — the caller must render "at least".
    pub complete: bool,
}

/// Sum of regular-file lengths under `dir`, bounded by `budget`. Symlinks count zero and
/// are not followed. `complete: false` means the caller must render "at least".
#[must_use]
pub fn size_of(dir: &Path, budget: &Budget) -> Size {
    let start = Instant::now();
    let mut entries = 0usize;
    let mut bytes = 0u64;
    let complete = sum_into(dir, budget, start, &mut entries, &mut bytes);
    Size { bytes, complete }
}

/// [`size_of`] over several directories sharing ONE budget, so a report over 127 target
/// dirs cannot cost 127 x the ceiling.
#[must_use]
pub fn size_of_all(dirs: &[&Target], budget: &Budget) -> Size {
    let start = Instant::now();
    let mut entries = 0usize;
    let mut bytes = 0u64;
    let mut complete = true;
    for target in dirs {
        if !sum_into(&target.path, budget, start, &mut entries, &mut bytes) {
            complete = false;
            break;
        }
    }
    Size { bytes, complete }
}

/// The shared walk behind [`size_of`] and [`size_of_all`]. `false` = a budget ran out.
fn sum_into(
    dir: &Path,
    budget: &Budget,
    start: Instant,
    entries: &mut usize,
    bytes: &mut u64,
) -> bool {
    let mut stack: Vec<PathBuf> = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in read.flatten() {
            if *entries >= budget.max_entries || start.elapsed() >= budget.max_wall {
                return false;
            }
            *entries += 1;
            let path = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.is_dir() {
                stack.push(path);
            } else if meta.is_file() {
                *bytes = bytes.saturating_add(meta.len());
            }
            // A symlink is neither: it counts zero and is not followed, so a tree cannot be
            // summed twice through one.
        }
    }
    true
}

/// `"41.2 GiB"`, `"at least 41.2 GiB"` when the walk was cut short, or `"unmeasured"` when
/// it was cut short having summed NOTHING. Delegates to [`crate::cost::human_bytes`].
///
/// The third case is not cosmetic: a shared sizing budget spent on the first few of 127
/// target dirs gives every remaining row a zero-length walk, and `"at least 0 B"` is a
/// floor that reads as a measurement of a small tree. It is a row that was never looked at.
#[must_use]
pub fn human_size(size: &Size) -> String {
    if !size.complete && size.bytes == 0 {
        return String::from("unmeasured");
    }
    let rendered = crate::cost::human_bytes(size.bytes);
    if size.complete {
        rendered
    } else {
        let mut s = String::from("at least ");
        s.push_str(&rendered);
        s
    }
}

// ---------------------------------------------------------------------------------------
// Migration
// ---------------------------------------------------------------------------------------

/// What [`migrate`] did, or would do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Migration {
    /// Not macOS. There is no Spotlight here to hide from.
    NotApplicable,
    /// The name already ends [`NOINDEX_SUFFIX`] (or is dot-hidden). Nothing to do — this is
    /// what makes `migrate` idempotent.
    AlreadyExcluded(PathBuf),
    /// `--dry-run`: this is the rename that would happen.
    Planned {
        /// The directory as the user named it.
        from: PathBuf,
        /// Its `.noindex` spelling, in the same parent.
        to: PathBuf,
    },
    /// The rename happened.
    Migrated {
        /// Where the tree was.
        from: PathBuf,
        /// Where it is now.
        to: PathBuf,
    },
}

/// Why a migration did not happen. Every variant leaves the disk EXACTLY as it was.
#[derive(Debug)]
pub enum MigrateError {
    /// None of the three [`Evidence`] signals is present. Refused rather than guessed:
    /// renaming a directory that is not build output is the one way this utility could cost
    /// someone work.
    NotATarget(PathBuf),
    /// The destination already exists. NEVER clobbered, never merged, never deleted.
    DestinationExists(PathBuf),
    /// `dir` is a SYMLINK. `rename(2)` would move the link and leave the build output
    /// exactly where it is — indexed — while every surface reported success.
    Symlink {
        /// The link, as the user named it.
        named: PathBuf,
        /// What it resolves to, when that could be read.
        real: Option<PathBuf>,
    },
    /// The rename itself failed (permissions, a live `cargo` holding the tree).
    Io(String),
}

impl std::fmt::Display for MigrateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotATarget(p) => write!(
                f,
                "{} carries no cargo build-output signal (CACHEDIR.TAG, .rustc_info.json, \
                 or debug/ + release/) — name the target directory itself",
                p.display()
            ),
            Self::DestinationExists(p) => write!(
                f,
                "{} already exists and is never merged into — remove or rename it, then \
                 re-run",
                p.display()
            ),
            Self::Symlink { named, real } => {
                write!(f, "{} is a symlink", named.display())?;
                if let Some(real) = real {
                    write!(f, " to {}", real.display())?;
                }
                write!(
                    f,
                    " — renaming it would move the LINK and leave the build output indexed \
                     where it really lives; name the real directory instead"
                )
            }
            Self::Io(e) => write!(
                f,
                "the rename failed ({e}) — stop any running cargo in that tree and re-run"
            ),
        }
    }
}

impl std::error::Error for MigrateError {}

/// Rename `dir` to `<dir>.noindex` in place.
///
/// SAFE BY CONSTRUCTION: it is one `rename(2)` inside a single parent directory, so it
/// moves no bytes, copies nothing, and deletes nothing; if it fails, nothing changed. It
/// refuses an unrecognized directory ([`MigrateError::NotATarget`]) and refuses to
/// overwrite ([`MigrateError::DestinationExists`]). Re-running it on an already-migrated
/// path is [`Migration::AlreadyExcluded`], not an error.
///
/// It deliberately does NOT leave a `target -> target.noindex` symlink behind: the point is
/// that the user re-points cargo (see [`cargo_hint`]) and knows they did, rather than
/// discovering a year later that a path they believe is `target/` is something else.
///
/// A SYMLINK is refused ([`MigrateError::Symlink`]) rather than followed or renamed.
///
/// # Errors
/// See [`MigrateError`].
pub fn migrate(dir: &Path, dry_run: bool) -> Result<Migration, MigrateError> {
    if !SUPPORTED {
        return Ok(Migration::NotApplicable);
    }
    // A SYMLINK IS REFUSED, and it is checked before the name-based idempotence answer
    // because on a link the name says nothing about the tree it points at.
    //
    // `target_evidence` reads THROUGH a link (`Path::is_dir` follows), so `repo/target ->
    // /Volumes/fast/realtarget` passes every recognition test — and then `fs::rename`
    // renames THE LINK. Confirmed 2026-09-02: afterwards `target.noindex -> /…/realtarget`,
    // the build output had not moved and was still indexed, and the CLI printed "migrated
    // … nothing was copied and nothing was deleted". The user believes the 2 TB amplifier
    // is gone. One `symlink_metadata` (which does NOT follow) buys the honest refusal.
    if std::fs::symlink_metadata(dir).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(MigrateError::Symlink {
            named: dir.to_path_buf(),
            real: std::fs::canonicalize(dir).ok(),
        });
    }
    // Idempotence next, read off the REAL path: `migrate target` run from inside
    // `~/build.noindex`, or through a symlinked ancestor, must answer `AlreadyExcluded`
    // exactly as the absolute spelling does — a `.noindex` (or dot-hidden) ancestor already
    // excludes the whole subtree (measured 2026-09-02), so renaming below it changes
    // nothing. Falls back to the name as given when the path cannot be resolved, which is
    // the pre-existing behaviour.
    let real = std::fs::canonicalize(dir);
    let claim = real.as_deref().unwrap_or(dir);
    if exclusion_of(claim) != Exclusion::Exposed {
        return Ok(Migration::AlreadyExcluded(dir.to_path_buf()));
    }
    if target_evidence(dir).is_none() {
        return Err(MigrateError::NotATarget(dir.to_path_buf()));
    }
    let Some(dest) = destination(dir) else {
        return Err(MigrateError::NotATarget(dir.to_path_buf()));
    };
    // symlink_metadata, not `exists()`: a DANGLING symlink at the destination is still an
    // occupant, and `rename(2)` onto it would replace it.
    if std::fs::symlink_metadata(&dest).is_ok() {
        return Err(MigrateError::DestinationExists(dest));
    }
    if dry_run {
        return Ok(Migration::Planned {
            from: dir.to_path_buf(),
            to: dest,
        });
    }
    match std::fs::rename(dir, &dest) {
        Ok(()) => Ok(Migration::Migrated {
            from: dir.to_path_buf(),
            to: dest,
        }),
        Err(e) => Err(MigrateError::Io(e.to_string())),
    }
}

// ---------------------------------------------------------------------------------------
// Apply — the doctor's remedy, done: migrate + re-point cargo, per repo, idempotent
// ---------------------------------------------------------------------------------------

/// What `apply` did (or would do) about one repo's cargo config after a migration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigNote {
    /// `[build] target-dir` was written (a new `[build]` table, or a key under the
    /// existing one) into this `.cargo/config.toml`.
    Written(PathBuf),
    /// An existing `target-dir` that named the old directory was rewritten.
    Rewritten(PathBuf),
    /// A git checkout (module doc): NO config edit. A relative symlink `link -> <new
    /// name>` stands where the directory was, so cargo's pointer is unchanged; `exclude`
    /// says what keeps `git status` clean about the new name, and `link_exclude` what
    /// keeps it clean about the link: [`ExcludeNote::AlreadyIgnored`] when the entry that
    /// ignored the directory matches the link too, [`ExcludeNote::Added`] when that entry
    /// was directory-only (`target/`) and the link got its own `.git/info/exclude` line,
    /// [`ExcludeNote::NotIgnored`] when the link shows as untracked — as the directory did
    /// in a repo that never ignored it, or because excluding it failed.
    Linked {
        link: PathBuf,
        exclude: ExcludeNote,
        link_exclude: ExcludeNote,
    },
    /// Nothing written, and why: no `Cargo.toml` beside the directory, an existing
    /// `target-dir` pointing elsewhere, or a write/parse failure.
    Untouched(String),
}

/// What [`apply_one`] did about one path — the migrated name, or the link standing
/// where the directory was — in a git checkout's ignore rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExcludeNote {
    /// `git check-ignore` already matched the path (a `target*` pattern, the entry that
    /// ignored the directory, or an earlier pass's line): nothing written.
    AlreadyIgnored,
    /// The path was appended to this exclude file — `.git/info/exclude`, per clone,
    /// outside the working tree, shared by the clone's worktrees.
    Added(PathBuf),
    /// Nothing written, and why (git could not be run, the path could not be related to
    /// the toplevel, the write failed, or — for the link — the directory was never
    /// ignored either): `git status` shows the path until the user excludes it.
    NotIgnored(String),
}

/// How [`apply_one`] keeps cargo pointed at a migrated directory — decided BEFORE the
/// rename, so a dry run can say it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pointer {
    /// A git checkout: a relative symlink `<old name> -> <new name>` where the directory
    /// was; `.cargo/config.toml` is never opened.
    Symlink,
    /// Not under git: `[build] target-dir` goes into this `.cargo/config.toml`.
    Config(PathBuf),
    /// No `Cargo.toml` beside it: nothing to point — the pointer is an env var.
    None,
}

/// One directory's outcome under [`apply_one`] / [`apply_under`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Applied {
    /// Renamed, and cargo (re-)pointed as `config` says.
    Migrated {
        from: PathBuf,
        to: PathBuf,
        config: ConfigNote,
    },
    /// Dry run: the rename that would happen, and how cargo would stay pointed.
    Planned {
        from: PathBuf,
        to: PathBuf,
        pointer: Pointer,
    },
    /// Already `.noindex` (or dot-hidden): nothing to do — a SUCCESS for a loop.
    AlreadyExcluded(PathBuf),
    /// Left alone, with the one reason: a build holds the cargo lock, no repo beside it
    /// (under `--all`), a symlink, a destination in the way, a failed rename.
    Skipped { path: PathBuf, reason: String },
}

impl Applied {
    /// Whether this outcome MOVED a directory this pass — what the `machine-settings:`
    /// count reports.
    #[must_use]
    pub const fn migrated(&self) -> bool {
        matches!(self, Applied::Migrated { .. })
    }
}

/// How many subdirectories [`build_in_progress`] looks into per level. Cargo's layout
/// has a handful (`debug`, `release`, one dir per `--target` triple, and under each of
/// those `build`, `deps`, `incremental`, `examples`, `.fingerprint`); the cap bounds the
/// probe on a directory that is not cargo's at all.
const MAX_LOCK_PROBE_ENTRIES: usize = 64;

/// The directory a build in `dir` holds locked, if one is running: cargo takes an
/// exclusive `flock(2)` on `<target>/<profile>/.cargo-lock` for the whole build (and on
/// `<target>/.cargo-lock` for some ops) — and a `--target <triple>` build one level
/// deeper, on `<target>/<triple>/<profile>/.cargo-lock`, which is why the probe reaches
/// two levels down (bounded by [`MAX_LOCK_PROBE_ENTRIES`] per level; noted in review
/// 2026-09-10). A lock we cannot take non-blockingly is a live build; renaming its tree
/// from under it would strand every path the compiler holds. Probed, never held: the lock
/// is released again immediately. Non-Unix: `None`.
#[must_use]
pub fn build_in_progress(dir: &Path) -> Option<PathBuf> {
    fn subdirs(dir: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .map(|read| {
                read.flatten()
                    .map(|entry| entry.path())
                    .filter(|p| std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir()))
                    .take(MAX_LOCK_PROBE_ENTRIES)
                    .collect()
            })
            .unwrap_or_default()
    }
    let mut candidates = vec![dir.join(".cargo-lock")];
    for profile_or_triple in subdirs(dir) {
        candidates.push(profile_or_triple.join(".cargo-lock"));
        for profile in subdirs(&profile_or_triple) {
            candidates.push(profile.join(".cargo-lock"));
        }
    }
    candidates.into_iter().find(|lock| lock_is_held(lock))
}

/// How long a lock may read as held before the probe believes it is a build's. A
/// build holds `.cargo-lock` for seconds to hours; a hold this short is not one.
///
/// The short hold that exists: an `flock` lives on the OPEN FILE DESCRIPTION, and a
/// process that spawns a child duplicates every descriptor into it for the window
/// between the fork and the exec that closes the `CLOEXEC` copies — so a lock the
/// holder has just closed stays held until the child of ANY thread in that process
/// has exec'd. Measured 2026-09-12 on the unit test that holds a lock in-process
/// beside 30 sibling tests that spawn `git`: under a full gate's load, 2 of 13 runs
/// saw a lock still held one call after its owner dropped it. The window is
/// milliseconds; the patience is a hundred, polled every ten, and the LAST reading
/// is the verdict — a build that takes the lock mid-window is still caught, a copy
/// that closes mid-window is not mistaken for one.
const LOCK_PROBE_PATIENCE: std::time::Duration = std::time::Duration::from_millis(100);
const LOCK_PROBE_STEP: std::time::Duration = std::time::Duration::from_millis(10);

/// Whether someone else holds `flock(2)` on `lock` — for longer than
/// [`LOCK_PROBE_PATIENCE`], which is what distinguishes a build from a spawn window.
#[cfg(unix)]
fn lock_is_held(lock: &Path) -> bool {
    use std::os::unix::io::AsRawFd as _;
    let Ok(file) = std::fs::OpenOptions::new().read(true).open(lock) else {
        return false;
    };
    let deadline = std::time::Instant::now() + LOCK_PROBE_PATIENCE;
    loop {
        // SAFETY: flock on a valid open descriptor; LOCK_NB makes it return at once.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            // SAFETY: releasing the lock this probe just took on the same descriptor.
            let _ = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
            return false;
        }
        let err = std::io::Error::last_os_error();
        if !matches!(err.raw_os_error(), Some(libc::EWOULDBLOCK)) {
            return false;
        }
        if std::time::Instant::now() >= deadline {
            return true;
        }
        std::thread::sleep(LOCK_PROBE_STEP);
    }
}

#[cfg(not(unix))]
fn lock_is_held(_lock: &Path) -> bool {
    false
}

/// The repo a target dir belongs to: its parent, when that holds a `Cargo.toml`.
/// `None` for a free-standing target dir (`~/.cargo-target-m7c`,
/// `~/aterm-agent-targets/<branch>`), whose pointer is an env var this module cannot see.
#[must_use]
pub fn repo_of(dir: &Path) -> Option<PathBuf> {
    let parent = dir.parent()?;
    parent
        .join("Cargo.toml")
        .is_file()
        .then(|| parent.to_path_buf())
}

/// The `target-dir` VALUE to write for `dest` in `repo`'s config: the bare name when the
/// directory is a direct child of the repo (relative paths in `.cargo/config.toml`
/// resolve against the directory holding `.cargo`, so `target.noindex` follows a moved
/// checkout), the absolute path otherwise.
#[must_use]
pub fn target_dir_value(repo: &Path, dest: &Path) -> String {
    match (dest.parent(), dest.file_name()) {
        (Some(parent), Some(name)) if parent == repo => name.to_string_lossy().into_owned(),
        _ => dest.display().to_string(),
    }
}

/// What [`point_cargo_config`] decided about one config text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigEdit {
    /// The new text, with `[build] target-dir` set to the value (table added if absent).
    Written(String),
    /// The new text, with an existing `target-dir` line rewritten.
    Rewritten(String),
    /// An existing `target-dir` names something other than the old directory: leave
    /// it — the migrated directory was not the one cargo used.
    PointsElsewhere(String),
    /// The edit would not parse (a `[build]` we could not find the end of, a quoting
    /// shape we do not understand): nothing written.
    Unparseable(String),
}

/// Pure: `text` is the repo's `.cargo/config.toml` (or empty when absent), `old_name`
/// the migrated directory's former name, `value` what `target-dir` must now say.
///
/// Three shapes, each measured against a real file: no `[build]` table ⇒ one is
/// appended (`[build]\ntarget-dir = "…"`); a `[build]` table with no `target-dir` ⇒ the
/// key goes on the line after the header; an existing `target-dir` ⇒ rewritten when it
/// named the old directory (bare name, `./name`, or an absolute path ending in it),
/// left alone otherwise. The result is re-parsed as TOML before it is returned, so a
/// file this cannot edit safely is never written.
#[must_use]
pub fn point_cargo_config(text: &str, old_name: &str, value: &str) -> ConfigEdit {
    let key_line = format!(
        "target-dir = \"{}\"",
        value.replace('\\', "\\\\").replace('"', "\\\"")
    );
    let mut out: Vec<String> = Vec::new();
    let mut in_build = false;
    let mut seen_build = false;
    let mut rewrote = false;
    let mut existing_elsewhere: Option<String> = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_build = is_build_header(trimmed);
            if in_build {
                seen_build = true;
            }
            out.push(line.to_string());
            continue;
        }
        if in_build
            && let Some(rest) = trimmed.strip_prefix("target-dir")
            && rest.trim_start().starts_with('=')
        {
            let current = rest
                .trim_start()
                .trim_start_matches('=')
                .trim()
                .trim_matches(|c| c == '"' || c == '\'');
            if names_dir(current, old_name) {
                out.push(key_line.clone());
                rewrote = true;
            } else {
                existing_elsewhere = Some(current.to_string());
                out.push(line.to_string());
            }
            continue;
        }
        out.push(line.to_string());
    }
    if let Some(elsewhere) = existing_elsewhere
        && !rewrote
    {
        return ConfigEdit::PointsElsewhere(elsewhere);
    }
    let mut result = if rewrote {
        out.join("\n")
    } else if seen_build {
        // The key goes right after the `[build]` header, before any comment or key.
        let mut with_key: Vec<String> = Vec::with_capacity(out.len() + 1);
        let mut inserted = false;
        for line in out {
            let is_header = is_build_header(&line);
            with_key.push(line);
            if is_header && !inserted {
                with_key.push(key_line.clone());
                inserted = true;
            }
        }
        with_key.join("\n")
    } else {
        let mut t = out.join("\n");
        if !t.is_empty() && !t.ends_with('\n') {
            t.push('\n');
        }
        if !t.is_empty() {
            t.push('\n');
        }
        t.push_str("[build]\n");
        t.push_str(&key_line);
        t
    };
    if !result.ends_with('\n') {
        result.push('\n');
    }
    if aterm_toml::from_str::<aterm_toml::Value>(&result).is_err() {
        return ConfigEdit::Unparseable(result);
    }
    if rewrote {
        ConfigEdit::Rewritten(result)
    } else {
        ConfigEdit::Written(result)
    }
}

/// Whether a line is the `[build]` table header, read by what it names: a trailing
/// `# comment` and whitespace inside the brackets are TOML, not another table. Byte
/// equality with `[build]` missed `[build] # faster links`, appended a second `[build]`,
/// and the edit was refused as unparseable (2026-09-12, audit K11).
fn is_build_header(line: &str) -> bool {
    let line = line.trim();
    let mut quote: Option<char> = None;
    let mut end = line.len();
    for (i, c) in line.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == '#' => {
                end = i;
                break;
            }
            None => {}
        }
    }
    line[..end]
        .trim()
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .is_some_and(|inner| {
            inner
                .chars()
                .filter(|c| !c.is_whitespace())
                .eq("build".chars())
        })
}

/// Whether a `target-dir` value names the directory called `old_name` in this repo:
/// the bare name, `./name`, or any path whose last component is it.
fn names_dir(value: &str, old_name: &str) -> bool {
    let v = value.trim_end_matches('/');
    v == old_name
        || v.strip_prefix("./") == Some(old_name)
        || Path::new(v).file_name().is_some_and(|n| n == old_name)
}

/// What [`plan_repo_config`] decided, before anything is renamed.
enum RepoConfig {
    /// Write this text to this `.cargo/config.toml`.
    Write {
        config: PathBuf,
        text: String,
        rewritten: bool,
    },
    /// Nothing to write, and the rename is still safe: an existing `target-dir` names
    /// some other directory, so cargo never used this one.
    Leave(String),
}

/// Decide the `[build] target-dir` edit for the migration `from -> to` in `repo`'s
/// `.cargo/config.toml`, touching nothing. `Err` is a config that cannot be pointed —
/// the caller must not rename: until 2026-09-12 this ran AFTER the rename, and a
/// refusal left cargo aimed at a `target/` that was gone, orphaned the renamed tree,
/// and wedged every later pass on "already exists" (audit K11).
fn plan_repo_config(repo: &Path, from: &Path, to: &Path) -> Result<RepoConfig, String> {
    let config = repo.join(".cargo").join("config.toml");
    // Cargo reads a legacy `.cargo/config` in preference to `config.toml`, so an edit to
    // the latter would not move the build — unless the one is a link to the other.
    let legacy = repo.join(".cargo").join("config");
    if std::fs::symlink_metadata(&legacy).is_ok()
        && !std::fs::canonicalize(&legacy)
            .is_ok_and(|l| std::fs::canonicalize(&config).is_ok_and(|c| c == l))
    {
        return Err(format!(
            "{} exists and cargo reads it before config.toml — point cargo at {} yourself",
            legacy.display(),
            to.display()
        ));
    }
    // A linked config is edited through the link (`write_repo_config`); a dangling one
    // would read as absent and be REPLACED by a file of our own, so it is refused.
    if std::fs::symlink_metadata(&config).is_ok_and(|m| m.file_type().is_symlink())
        && std::fs::canonicalize(&config).is_err()
    {
        return Err(format!(
            "{} is a symlink to a file that does not exist — point cargo at {} yourself",
            config.display(),
            to.display()
        ));
    }
    let text = match std::fs::read_to_string(&config) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(format!("{} unreadable ({e})", config.display()));
        }
    };
    let old_name = from
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let value = target_dir_value(repo, to);
    match point_cargo_config(&text, &old_name, &value) {
        ConfigEdit::Written(text) => Ok(RepoConfig::Write {
            config,
            text,
            rewritten: false,
        }),
        ConfigEdit::Rewritten(text) => Ok(RepoConfig::Write {
            config,
            text,
            rewritten: true,
        }),
        ConfigEdit::PointsElsewhere(v) => Ok(RepoConfig::Leave(format!(
            "{} already sets target-dir = {v:?}, which is not the migrated directory — left as is",
            config.display()
        ))),
        ConfigEdit::Unparseable(_) => Err(format!(
            "{} could not be edited into valid TOML — point cargo at {} yourself",
            config.display(),
            to.display()
        )),
    }
}

/// Write a planned config edit into `repo`'s `.cargo/config.toml` (creating `.cargo/`
/// and the file when absent). `Err` is a failed write; the caller rolls the rename back.
fn write_repo_config(repo: &Path, plan: RepoConfig) -> Result<ConfigNote, String> {
    let (config, text, rewritten) = match plan {
        RepoConfig::Write {
            config,
            text,
            rewritten,
        } => (config, text, rewritten),
        RepoConfig::Leave(why) => return Ok(ConfigNote::Untouched(why)),
    };
    if let Err(e) = std::fs::create_dir_all(repo.join(".cargo")) {
        return Err(format!("{}: {e}", repo.join(".cargo").display()));
    }
    // Write to the FILE the config names, never over a link: a symlinked config.toml
    // (shared across checkouts, dotfiles-managed) renamed over became a detached regular
    // copy, and later edits to the shared file stopped reaching this repo (2026-09-12,
    // the audit K12 class). A link that no longer resolves is refused, not replaced.
    let real = match std::fs::symlink_metadata(&config) {
        Ok(m) if m.file_type().is_symlink() => match std::fs::canonicalize(&config) {
            Ok(real) => real,
            Err(e) => return Err(format!("{}: unresolvable symlink ({e})", config.display())),
        },
        _ => config.clone(),
    };
    let Some(real_dir) = real.parent() else {
        return Err(format!("{}: no parent directory", real.display()));
    };
    // Atomic: temp + rename, so a crash mid-write cannot leave cargo a half config. The
    // temp sits beside the REAL file (same filesystem, so the rename replaces it and not
    // the link), and an existing file's mode carries over.
    let tmp = real_dir.join(format!(".config.toml.atpkg-{}.tmp", std::process::id()));
    let mode = std::fs::metadata(&real).ok().map(|m| m.permissions());
    let _ = std::fs::remove_file(&tmp);
    let written = create_config_temp(&tmp)
        .and_then(|mut f| std::io::Write::write_all(&mut f, text.as_bytes()))
        .and_then(|()| match mode {
            Some(perms) => std::fs::set_permissions(&tmp, perms),
            None => new_config_permissions(&tmp),
        });
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("{}: {e}", tmp.display()));
    }
    if let Err(e) = std::fs::rename(&tmp, &real) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("{}: {e}", config.display()));
    }
    Ok(if rewritten {
        ConfigNote::Rewritten(config)
    } else {
        ConfigNote::Written(config)
    })
}

/// Create the cargo config rewrite's temp: exclusively, and born `0600`. It holds the
/// WHOLE config before the real file's mode is applied, so a umask-default temp left a
/// `0600` `config.toml` readable by other users until that chmod (2026-09-12, review of
/// the audit follow-ups; `hooks::create_rc_temp` closed the same window for rc files).
/// The name carries the pid, so two checkouts sharing one linked config cannot collide.
#[cfg(unix)]
fn create_config_temp(tmp: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(tmp)
}

#[cfg(not(unix))]
fn create_config_temp(tmp: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(tmp)
}

/// A config.toml this pass CREATES gets the ordinary `0644` a plain write used to leave
/// under the usual umask, not the temp's private `0600`.
#[cfg(unix)]
fn new_config_permissions(tmp: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(tmp, std::fs::Permissions::from_mode(0o644))
}

#[cfg(not(unix))]
fn new_config_permissions(_tmp: &Path) -> std::io::Result<()> {
    Ok(())
}

/// One bounded `git -C <repo> <args>` (the doctor's 5 s probe clock; stderr dropped).
/// `None` when git could not be spawned or did not finish — every caller reads that as
/// "not known", never as "not a checkout" on its own.
fn git(repo: &Path, args: &[&str]) -> Option<std::process::Output> {
    crate::doctor::output_bounded(
        std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args),
    )
}

/// `git rev-parse <args>` in `repo`, as a path; relative answers are resolved against
/// `repo` (git prints them relative to its `-C` directory).
fn git_path(repo: &Path, args: &[&str]) -> Option<PathBuf> {
    let mut full = vec!["rev-parse"];
    full.extend_from_slice(args);
    let out = git(repo, &full).filter(|o| o.status.success())?;
    let text = String::from_utf8_lossy(&out.stdout);
    let text = text.trim_end();
    if text.is_empty() {
        return None;
    }
    let p = PathBuf::from(text);
    Some(if p.is_absolute() { p } else { repo.join(p) })
}

/// Whether `repo` is inside a git checkout: a `.git` at its root (a directory, or the
/// FILE a linked worktree carries) answers without spawning anything; otherwise
/// `git rev-parse --show-toplevel` decides, so a crate nested inside a checkout counts
/// too. False when git cannot run and there is no `.git` — the config edit is then the
/// fallback, which is the pre-2026-09-10 behaviour.
#[must_use]
pub fn is_git_checkout(repo: &Path) -> bool {
    std::fs::symlink_metadata(repo.join(".git")).is_ok()
        || git(repo, &["rev-parse", "--show-toplevel"]).is_some_and(|o| o.status.success())
}

/// How cargo would be kept pointed at `dir`'s migrated name: the symlink in a git
/// checkout, the config edit elsewhere, nothing for a free-standing directory.
#[must_use]
pub fn pointer_for(repo: Option<&Path>) -> Pointer {
    match repo {
        None => Pointer::None,
        Some(repo) if is_git_checkout(repo) => Pointer::Symlink,
        Some(repo) => Pointer::Config(repo.join(".cargo").join("config.toml")),
    }
}

/// Whether git ignores `path` in `repo`: `Some(true)`/`Some(false)` from
/// `check-ignore`'s 0/1, `None` when git could not answer (not spawnable, timed out,
/// or exit 128 — a path outside the repository).
fn git_ignores(repo: &Path, path: &Path) -> Option<bool> {
    let shown = path.to_string_lossy();
    let out = git(repo, &["check-ignore", "-q", "--", &shown])?;
    match out.status.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}

/// Keep `git status` clean about `path` (the migrated name, or the link left where the
/// directory was): nothing when git already ignores it, else one line — `/<path
/// relative to the toplevel>` under `comment` — appended to the clone's
/// `.git/info/exclude` (`git rev-parse --git-path info/exclude`, which is the COMMON
/// dir's file from a linked worktree too, so every worktree of the clone shares the
/// line). Never `.gitignore`: that is a tracked file, and editing it is the dirty tree
/// this whole path exists to avoid.
fn exclude_in_git(repo: &Path, path: &Path, comment: &str) -> ExcludeNote {
    match git_ignores(repo, path) {
        Some(true) => return ExcludeNote::AlreadyIgnored,
        Some(false) => {}
        None => {
            return ExcludeNote::NotIgnored(
                "git could not be asked — add the new name to .git/info/exclude yourself"
                    .to_string(),
            );
        }
    }
    let Some(toplevel) = git_path(repo, &["--show-toplevel"]) else {
        return ExcludeNote::NotIgnored("git rev-parse --show-toplevel failed".to_string());
    };
    let Some(exclude) = git_path(repo, &["--git-path", "info/exclude"]) else {
        return ExcludeNote::NotIgnored("git rev-parse --git-path info/exclude failed".to_string());
    };
    // `--show-toplevel` is canonical (symlinks resolved), so the path must be canonical
    // too before the one can be stripped off the other — its PARENT: canonicalizing the
    // link itself would resolve it to the new name and exclude the wrong path.
    let real_path = match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => {
            std::fs::canonicalize(parent).map_or_else(|_| path.to_path_buf(), |p| p.join(name))
        }
        _ => path.to_path_buf(),
    };
    let real_top = std::fs::canonicalize(&toplevel).unwrap_or(toplevel);
    let Ok(rel) = real_path.strip_prefix(&real_top) else {
        return ExcludeNote::NotIgnored(format!(
            "{} is not under the toplevel {}",
            real_path.display(),
            real_top.display()
        ));
    };
    let pattern = format!("/{}", rel.display());
    let mut text = match std::fs::read_to_string(&exclude) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return ExcludeNote::NotIgnored(format!("{} unreadable ({e})", exclude.display()));
        }
    };
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(comment);
    text.push('\n');
    text.push_str(&pattern);
    text.push('\n');
    if let Some(parent) = exclude.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        return ExcludeNote::NotIgnored(format!("{}: {e}", parent.display()));
    }
    if let Err(e) = std::fs::write(&exclude, text.as_bytes()) {
        return ExcludeNote::NotIgnored(format!("{}: {e}", exclude.display()));
    }
    // Read back through git: the line is only worth reporting if git agrees.
    match git_ignores(repo, &real_path) {
        Some(true) => ExcludeNote::Added(exclude),
        _ => ExcludeNote::NotIgnored(format!(
            "{pattern} was written to {} but git still does not ignore it",
            exclude.display()
        )),
    }
}

/// The exclude-file comment over the migrated name's line.
const EXCLUDE_NEW_NAME: &str =
    "# atpkg noindex: the cargo target dir migrated out of Spotlight's index";
/// The exclude-file comment over the link's line — written only when the entry that
/// ignored the directory is directory-only and so stops matching at the link.
const EXCLUDE_LINK: &str = "# atpkg noindex: the symlink standing where the directory was — the \
                            ignore entry that matched the directory is directory-only";

/// The git-checkout pointer: a RELATIVE symlink `from -> <file name of to>` (same
/// parent, so a moved checkout keeps working), then the exclude lines: the new name's,
/// and the link's own when `dir_ignored` (git's answer about the DIRECTORY, probed
/// before the rename) was true but the link is not — a directory-only pattern
/// (`target/`) matches the directory and not the symlink that replaces it, and without
/// this line the pass would turn an ignored directory into a `?? target` row. A link
/// that cannot be laid rolls the rename back — a renamed tree with no pointer would
/// break the next build silently — and reports why.
#[cfg(unix)]
fn link_in_place(
    repo: &Path,
    from: &Path,
    to: &Path,
    dir_ignored: Option<bool>,
) -> Result<ConfigNote, String> {
    let Some(name) = to.file_name() else {
        return Err(format!("{} has no file name", to.display()));
    };
    if let Err(e) = std::os::unix::fs::symlink(name, from) {
        return Err(format!(
            "the symlink {} -> {} could not be laid ({e})",
            from.display(),
            Path::new(name).display()
        ));
    }
    let exclude = exclude_in_git(repo, to, EXCLUDE_NEW_NAME);
    let link_exclude = match git_ignores(repo, from) {
        // The entry that ignored the directory matches the link too. An unanswerable
        // git reads as ignored here because the alternative is a warning about a state
        // nothing measured.
        Some(true) | None => ExcludeNote::AlreadyIgnored,
        Some(false) => match dir_ignored {
            Some(true) => match exclude_in_git(repo, from, EXCLUDE_LINK) {
                ExcludeNote::NotIgnored(why) => ExcludeNote::NotIgnored(format!(
                    "the directory was ignored by a directory-only pattern the link does not \
                     match, and excluding the link failed — {why}; git status shows it until it \
                     is excluded"
                )),
                note => note,
            },
            Some(false) => ExcludeNote::NotIgnored(
                "the link shows as untracked, as the directory did".to_string(),
            ),
            None => ExcludeNote::NotIgnored(
                "the link shows as untracked (whether git ignored the directory could not be \
                 asked before the rename)"
                    .to_string(),
            ),
        },
    };
    Ok(ConfigNote::Linked {
        link: from.to_path_buf(),
        exclude,
        link_exclude,
    })
}

#[cfg(not(unix))]
fn link_in_place(
    _repo: &Path,
    from: &Path,
    to: &Path,
    _dir_ignored: Option<bool>,
) -> Result<ConfigNote, String> {
    Err(format!(
        "a symlink {} -> {} is not laid on this platform",
        from.display(),
        to.display()
    ))
}

/// Apply the remedy to ONE target dir: refuse a live build, [`migrate`] it, and keep
/// its repo's cargo pointed at the new name — a symlink in a git checkout, the config
/// edit elsewhere ([`pointer_for`]). `require_repo` (the `--all` walk) skips a
/// free-standing target dir — renaming one whose pointer is an env var this cannot see
/// would break the next build silently; a directory the user NAMED is migrated anyway,
/// with the hint the standalone verb prints. Idempotent: an already-excluded directory
/// is `AlreadyExcluded`, never an error.
#[must_use]
pub fn apply_one(dir: &Path, require_repo: bool, dry_run: bool) -> Applied {
    if !SUPPORTED {
        return Applied::Skipped {
            path: dir.to_path_buf(),
            reason: "not applicable — Spotlight is a macOS index".to_string(),
        };
    }
    // The link a previous pass left (`target -> target.noindex`), or any symlink whose
    // real path is already excluded: a success, not the refusal `migrate` gives a link
    // into an INDEXED tree.
    if std::fs::symlink_metadata(dir).is_ok_and(|m| m.file_type().is_symlink())
        && let Ok(real) = std::fs::canonicalize(dir)
        && exclusion_of(&real) != Exclusion::Exposed
    {
        return Applied::AlreadyExcluded(dir.to_path_buf());
    }
    let repo = repo_of(dir);
    if require_repo && repo.is_none() {
        return Applied::Skipped {
            path: dir.to_path_buf(),
            reason: "not beside a Cargo.toml — its pointer is an env var this cannot re-point; \
                     `aterm pkg noindex apply <dir>` migrates it by name"
                .to_string(),
        };
    }
    if let Some(lock) = build_in_progress(dir) {
        return Applied::Skipped {
            path: dir.to_path_buf(),
            reason: format!("a build holds {} — retried next pass", lock.display()),
        };
    }
    let pointer = pointer_for(repo.as_deref());
    // Git's answer about the DIRECTORY, taken while it still is one: a directory-only
    // ignore pattern (`target/`) stops matching once a symlink stands there, and the
    // link then needs its own exclude line (`link_in_place`).
    let dir_ignored = match (&repo, &pointer) {
        (Some(repo), Pointer::Symlink) if !dry_run => git_ignores(repo, dir),
        _ => None,
    };
    // The config edit is decided BEFORE the rename, so a tree whose cargo cannot be
    // re-pointed is never moved (`plan_repo_config`). Only when the rename would
    // happen: a directory `migrate` refuses or calls excluded keeps that answer.
    let mut config_plan = None;
    if let (Some(repo), Pointer::Config(_)) = (&repo, &pointer)
        && let Ok(Migration::Planned { from, to }) = migrate(dir, true)
    {
        match plan_repo_config(repo, &from, &to) {
            Ok(plan) => config_plan = Some(plan),
            Err(why) => {
                return Applied::Skipped {
                    path: dir.to_path_buf(),
                    reason: format!("{why} — nothing was renamed"),
                };
            }
        }
    }
    match migrate(dir, dry_run) {
        Ok(Migration::Migrated { from, to }) => {
            let config = match (&repo, &pointer) {
                (Some(repo), Pointer::Symlink) => {
                    match link_in_place(repo, &from, &to, dir_ignored) {
                        Ok(note) => note,
                        Err(why) => {
                            return match std::fs::rename(&to, &from) {
                                Ok(()) => Applied::Skipped {
                                    path: dir.to_path_buf(),
                                    reason: format!("{why} — the rename was rolled back"),
                                },
                                Err(e) => Applied::Migrated {
                                    from,
                                    to,
                                    config: ConfigNote::Untouched(format!(
                                        "{why}, and rolling the rename back failed ({e}) — lay the \
                                     link or point cargo at the new name yourself"
                                    )),
                                },
                            };
                        }
                    }
                }
                (Some(repo), _) => {
                    let written = match config_plan {
                        Some(plan) => Ok(plan),
                        None => plan_repo_config(repo, &from, &to),
                    }
                    .and_then(|plan| write_repo_config(repo, plan));
                    match written {
                        Ok(note) => note,
                        Err(why) => {
                            // A renamed tree cargo is not pointed at breaks the next build
                            // silently: roll back, as the symlink arm does.
                            return match std::fs::rename(&to, &from) {
                                Ok(()) => Applied::Skipped {
                                    path: dir.to_path_buf(),
                                    reason: format!("{why} — the rename was rolled back"),
                                },
                                Err(e) => Applied::Migrated {
                                    from,
                                    to,
                                    config: ConfigNote::Untouched(format!(
                                        "{why}, and rolling the rename back failed ({e}) — point \
                                         cargo at the new name yourself"
                                    )),
                                },
                            };
                        }
                    }
                }
                (None, _) => ConfigNote::Untouched(
                    "no Cargo.toml beside it — point cargo at the new name yourself".to_string(),
                ),
            };
            Applied::Migrated { from, to, config }
        }
        Ok(Migration::Planned { from, to }) => Applied::Planned { from, to, pointer },
        Ok(Migration::AlreadyExcluded(p)) => Applied::AlreadyExcluded(p),
        Ok(Migration::NotApplicable) => Applied::Skipped {
            path: dir.to_path_buf(),
            reason: "not applicable".to_string(),
        },
        Err(e) => Applied::Skipped {
            path: dir.to_path_buf(),
            reason: e.to_string(),
        },
    }
}

/// The doctor's scan, then [`apply_one`] over every EXPOSED target dir it found, repos
/// only (`require_repo`). Returns the outcomes in path order and whether the scan was
/// complete (a truncated walk is a floor, and the caller must not call it a census).
#[must_use]
pub fn apply_under(
    root: &Path,
    max_depth: usize,
    budget: &Budget,
    dry_run: bool,
) -> (Vec<Applied>, bool) {
    let found = scan(root, max_depth, budget);
    let outcomes = found
        .exposed()
        .map(|t| apply_one(&t.path, true, dry_run))
        .collect();
    (outcomes, found.complete)
}

// ---------------------------------------------------------------------------------------
// Verification — the empirical probe
// ---------------------------------------------------------------------------------------

/// The clocks the probe runs on.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// How long to wait for the CONTROL to appear before giving up and answering `Unknown`.
    /// 20 s; the control appeared in 1.25–1.5 s on an idle machine on 2026-09-02, and the
    /// whole reason for the ceiling is the machine that is NOT idle.
    pub control_timeout: Duration,
    /// The FLOOR on the margin after the control appears, before the second candidate
    /// query. 2 s. Never the whole margin: see [`Timing::settle_after`] — on the loaded
    /// machine this module was written for, `mds` can be 15–19 s behind, and a fixed 2 s
    /// there is a tenth of the latency the run just measured.
    pub settle: Duration,
    /// How often to re-ask for the control. 250 ms.
    pub poll: Duration,
}

impl Timing {
    /// The measured defaults. See the field docs for where each number came from.
    pub const DEFAULT: Self = Self {
        control_timeout: Duration::from_secs(20),
        settle: Duration::from_secs(2),
        poll: Duration::from_millis(250),
    };

    /// The margin to wait before the SECOND candidate query, given how long the control
    /// actually took: `max(settle, waited)`.
    ///
    /// The soundness argument for `Excluded` is fsevents ordering, which this module calls
    /// a strong heuristic and not a documented contract (see [`plant`]); the margin is what
    /// buys back the slack. A CONSTANT margin makes that slack a fixed 2 s no matter how
    /// far behind `mds` is — and `waited` is the per-run MEASUREMENT of exactly that. On an
    /// idle machine the control appeared in 1.25–1.48 s (2026-09-02) and 2 s is a ~1.4x
    /// margin; on the machine this module exists for (18 rustc processes, syspolicyd at
    /// 86% CPU, `mds` grinding 2.0 TB) `waited` can legitimately be 15–19 s, where a fixed
    /// 2 s is ~0.1x. The bias is structural, not symmetric: the control is one file in a
    /// brand-new quiescent directory, while the probe is one file in a churning target dir
    /// where fsevents coalescing pushes `mds` into a directory rescan rather than a
    /// single-item import — so the control wins that race hardest exactly when a wrong
    /// answer costs the most. Scaling with `waited` keeps the margin proportional to the
    /// latency the run itself observed.
    #[must_use]
    pub fn settle_after(&self, waited: Duration) -> Duration {
        if waited > self.settle {
            waited
        } else {
            self.settle
        }
    }

    /// The worst-case wall clock of one [`verify`]: the control timeout, plus the largest
    /// margin [`Timing::settle_after`] can ask for (a control that appeared at the very
    /// last poll). What the CLI announces, so a reader is never surprised into ^C.
    #[must_use]
    pub fn worst_case(&self) -> Duration {
        self.control_timeout + self.settle_after(self.control_timeout)
    }
}

/// Why exclusion could not be MEASURED. None of these ever reads as "excluded".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unmeasured {
    /// The control file never showed up in the index. Without it there is no clock, so a
    /// probe miss proves nothing. Usually: Spotlight is off for this volume, the parent is
    /// itself inside an excluded or dot-hidden ancestor, or the machine is saturated.
    ControlNeverIndexed {
        /// How long the control was waited for, in seconds.
        waited_secs: u64,
    },
    /// `mdutil -s` says indexing is off for this volume. A refinement of the message only —
    /// the verdict is `Unknown` either way.
    IndexingDisabled(PathBuf),
    /// `dir` has no parent, so there is nowhere to plant a same-volume control.
    NoParent(PathBuf),
    /// The scope both queries would be pointed at is ITSELF excluded by name, so `mdfind`
    /// would answer from a live scan rather than from the index and every hit would be a
    /// false `Indexed`. Nothing under such a parent is exposed anyway.
    ScopeExcluded(PathBuf),
    /// A probe file could not be planted or `mdfind` could not be asked.
    Io(String),
}

/// The answer. See the module header for the asymmetry between these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// MEASURED: the probe was absent from the index while a control planted AFTER it was
    /// present. A bounded inference, not proof.
    Excluded,
    /// PROOF: the index returned the probe. This directory IS being indexed.
    Indexed,
    /// Not measured, and deliberately not guessed.
    Unknown(Unmeasured),
    /// Not macOS.
    NotApplicable,
}

impl std::fmt::Display for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Excluded => f.write_str(
                "excluded — a probe planted here stayed out of the index while a control \
                 planted after it appeared; nothing to do",
            ),
            Self::Indexed => f.write_str(
                "INDEXED — Spotlight is walking this tree; run `aterm pkg noindex migrate \
                 <dir>` to rename it to the .noindex form",
            ),
            Self::Unknown(Unmeasured::ControlNeverIndexed { waited_secs }) => write!(
                f,
                "unknown — the control file was still not indexed after {waited_secs}s, so a \
                 miss here proves nothing; re-run on a less busy machine"
            ),
            Self::Unknown(Unmeasured::IndexingDisabled(p)) => write!(
                f,
                "unknown — Spotlight indexing is off for the volume holding {}, so there is \
                 nothing to measure against; nothing needs migrating there",
                p.display()
            ),
            Self::Unknown(Unmeasured::ScopeExcluded(p)) => write!(
                f,
                "unknown — the parent {} is itself excluded by name (a `.noindex` or \
                 dot-hidden ancestor), so an `mdfind` scoped there answers from a live scan \
                 and not from the index; everything below it is already excluded and \
                 nothing there needs migrating",
                p.display()
            ),
            Self::Unknown(Unmeasured::NoParent(p)) => write!(
                f,
                "unknown — {} has no parent to plant a same-volume control in; name a \
                 directory, not a volume root",
                p.display()
            ),
            Self::Unknown(Unmeasured::Io(e)) => write!(
                f,
                "unknown — the probe could not be run ({e}); check the directory is writable \
                 and re-run"
            ),
            Self::NotApplicable => f.write_str(
                "not applicable — there is no Spotlight index on this platform to hide from",
            ),
        }
    }
}

/// The pure three-state decision, given ONLY what the queries returned — extracted so the
/// race logic is unit-tested without a live index.
///
/// * control never seen -> `Unknown(ControlNeverIndexed)`. NEVER `Excluded`.
/// * either candidate answer is a hit -> `Indexed`. A hit is proof and short-circuits.
/// * either candidate answer is `None` (mdfind could not be asked) -> `Unknown(Io)`.
/// * both candidate answers are a clean miss -> `Excluded`.
#[must_use]
pub fn decide(
    control_seen: bool,
    waited: Duration,
    candidate_first: Option<bool>,
    candidate_second: Option<bool>,
) -> Verdict {
    if !control_seen {
        return Verdict::Unknown(Unmeasured::ControlNeverIndexed {
            waited_secs: waited.as_secs(),
        });
    }
    if candidate_first == Some(true) || candidate_second == Some(true) {
        return Verdict::Indexed;
    }
    if candidate_first.is_none() || candidate_second.is_none() {
        return Verdict::Unknown(Unmeasured::Io(String::from(
            "mdfind could not be asked about the probe",
        )));
    }
    Verdict::Excluded
}

/// One-word probe token: `atpkgnoindexprobe` + hex nanos + hex pid. Pure, so its
/// query-safety is pinned by a test. `[a-z0-9]+` by construction, which is what lets it be
/// interpolated into an `mdfind` predicate with no quoting question, and what stops
/// Spotlight's tokenizer from splitting it.
#[must_use]
pub fn probe_token(nanos: u128, pid: u32) -> String {
    format!("atpkgnoindexprobe{nanos:016x}{pid:08x}")
}

/// The name of the control file that goes with `token` — a DIFFERENT exact name, so the
/// `kMDItemFSName ==` query for one can never answer for the other.
fn control_file_name(token: &str) -> String {
    let mut s = String::from(token);
    s.push_str("ctl");
    s
}

/// The directory an `mdfind` query for `dir`'s contents must be SCOPED at: `dir.parent()`.
///
/// NEVER `dir` itself. MEASURED 2026-09-02 on macOS 26.6.2: with a probe file planted in
/// `~/mdx-probe-scratch/target.noindex/`,
///   `mdfind -onlyin ~/mdx-probe-scratch/target.noindex 'kMDItemFSName == "<probe>"'`
/// PRINTED THE FILE, while the same predicate scoped at `~/mdx-probe-scratch`, at `~`, and
/// unscoped all returned nothing. Pointing `-onlyin` AT an excluded directory makes `mdfind`
/// answer from a live scan rather than from the index, and a utility that scoped its query
/// the obvious way would have reported every correctly-excluded directory as `Indexed` —
/// the mirror image of the `.metadata_never_index` false success this feature exists to
/// prevent. Scoping both queries at the shared parent also makes them symmetric: the control
/// proves that exact scope is indexed.
#[must_use]
pub fn verify_scope(dir: &Path) -> Option<&Path> {
    match dir.parent() {
        // `Path::new("target").parent()` is `Some("")`, NOT `None` — per std's own
        // documentation only a path terminating in a root or a prefix gives `None`. The
        // empty path is not a directory, and MEASURED 2026-09-02 on macOS 26.6.2, with an
        // indexed file present:
        //   mdfind -onlyin ''  'kMDItemFSName == "<token>"'  ->  '' , exit 0
        //   mdfind -onlyin .   'kMDItemFSName == "<token>"'  ->  /Users//…/<token>
        // Exit 0 with empty stdout is a CLEAN MISS to `spotlight_query`, so an empty scope
        // makes even the control unfindable: `aterm pkg noindex verify target` would burn
        // the whole timeout and answer `Unknown` on a perfectly idle machine, advising a
        // re-run that can never work. `.` is the directory the empty parent MEANS.
        // [`verify`] canonicalizes before it reaches here; this keeps the pure, public
        // function right for every other caller.
        Some(p) if p.as_os_str().is_empty() => Some(Path::new(".")),
        other => other,
    }
}

/// MEASURE whether `dir` is excluded from Spotlight. See [`decide`] for the answers and the
/// module header for why a bare probe would be worthless.
///
/// `dir` is CANONICALIZED first, and everything below — the scope, the probe, the control
/// — is derived from the real path. Plants a probe file in it, then a control file in a
/// plain-named sibling directory under its parent — PROBE FIRST, CONTROL SECOND, and that
/// order is the whole mechanism (see [`plant`]). Waits for the control, then queries the
/// candidate twice with a margin between that scales with how long the control took
/// ([`Timing::settle_after`]). Both queries are scoped at the shared parent
/// ([`verify_scope`]). Every probe file and the control directory are removed by a `Drop`
/// guard armed BEFORE anything is written, so a panic, an early return, or a failure
/// halfway through planting cannot litter a user's repo.
///
/// On non-macOS returns [`Verdict::NotApplicable`] without touching the disk.
#[must_use]
pub fn verify(dir: &Path, timing: &Timing) -> Verdict {
    if !SUPPORTED {
        return Verdict::NotApplicable;
    }
    // CANONICALIZE FIRST — the probe and the query must be about the same directory.
    //
    // `plant` writes THROUGH `dir`, so with `repo/target -> /Volumes/fast/realtarget` the
    // probe's real, indexed path is under `/Volumes/fast`, while a scope taken from the
    // LINK's parent is `repo`. MEASURED 2026-09-02 on macOS 26.6.2 with exactly that shape:
    //   mdfind (unscoped)          -> /Users//…/elsewhere/realtarget/<token>   (INDEXED)
    //   mdfind -onlyin …/repo      -> ''
    // while the control, a real path under `repo`, indexed in ~1.5 s. Control seen, both
    // candidate queries a clean miss, verdict `Excluded` — a MEASURED, confident false
    // negative for a tree Spotlight was actively indexing, which is the exact laundered
    // answer this module exists to make impossible. `ln -s /Volumes/fast/target target` is
    // what someone with 2 TB of build output actually does, so this is not a corner.
    // Canonicalizing also makes a relative operand absolute, which is what keeps
    // `verify_scope` off the empty path.
    let dir = match std::fs::canonicalize(dir) {
        Ok(real) => real,
        Err(e) => return Verdict::Unknown(Unmeasured::Io(e.to_string())),
    };
    let dir = dir.as_path();
    let Some(scope) = verify_scope(dir) else {
        return Verdict::Unknown(Unmeasured::NoParent(dir.to_path_buf()));
    };
    // The scope must itself be indexABLE, or both queries are meaningless in the dangerous
    // direction. The doc on `verify_scope` records the measurement: pointing `-onlyin` at
    // an excluded directory makes `mdfind` answer from a LIVE SCAN rather than from the
    // index. For `verify ~/build.noindex/target` the scope is `~/build.noindex`, so the
    // control is found instantly, the probe is found by the same live scan, and the verdict
    // is `Indexed` for a doubly-excluded tree — while `migrate` answers `AlreadyExcluded`,
    // leaving the user in an unresolvable loop. The control-directory NAME is already
    // guarded against this trap below; the scope must be too.
    if exclusion_of(scope) != Exclusion::Exposed {
        return Verdict::Unknown(Unmeasured::ScopeExcluded(scope.to_path_buf()));
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let token = probe_token(nanos, std::process::id());
    let control_name = control_file_name(&token);
    let mut control_dir_name = String::from("atpkg-control-");
    control_dir_name.push_str(&token);
    // A PLAIN name: no leading dot, and it must not end `.noindex`, or the control would be
    // excluded too and the clock would never start.
    let control_dir = scope.join(control_dir_name);

    // ARM THE GUARD BEFORE ANYTHING IS WRITTEN. Both paths are known without planting, and
    // `plant` writes the probe BEFORE it creates the control directory — so a guard built
    // from `plant`'s return value does not exist on the path where the probe landed and the
    // control write failed (ENOSPC on the 2 TB machine this feature targets, or a parent
    // the user cannot write). CONFIRMED: with the parent chmod 0o500, `verify` returned
    // `Unknown(Io("Permission denied"))` and left an `atpkgnoindexprobe<hex>` file inside
    // the user's target dir forever — exactly the litter this module's doc promises against.
    // Both removals are no-ops for something that was never created.
    let _cleanup = Cleanup {
        probe: dir.join(&token),
        control_dir: control_dir.clone(),
    };
    if let Err(e) = plant(dir, &control_dir, &token) {
        return Verdict::Unknown(Unmeasured::Io(e.to_string()));
    }

    // THE RACE, and why the control exists.
    //
    // A miss from `mdfind` has two possible causes that look identical from here: the file
    // is excluded, or the indexer has simply not reached it yet. `mds` is asynchronous and
    // on a loaded machine (the machine this whole module exists for) it can be many seconds
    // behind. So the probe alone can only ever produce a guess.
    //
    // The control is the clock. It is written AFTER the probe, into a plainly-named sibling
    // under the SAME parent — same volume, same fsevents stream. When the control turns up
    // in the index, the indexer has already been offered the probe's earlier event. Only
    // then is the probe's absence evidence, and even then it is an INFERENCE, never proof:
    // hence the settle margin and the second query below, and hence `Unknown` when the
    // control never arrives at all.
    let start = Instant::now();
    let mut control_seen = false;
    while start.elapsed() < timing.control_timeout {
        if indexed(scope, &control_name) == Some(true) {
            control_seen = true;
            break;
        }
        std::thread::sleep(timing.poll);
    }
    let waited = start.elapsed();

    if !control_seen {
        // The verdict is `Unknown` either way; asking `mdutil` only refines WHICH sentence
        // the user reads, so its own failure changes nothing.
        if crate::platform::spotlight_indexing_enabled(scope) == Some(false) {
            return Verdict::Unknown(Unmeasured::IndexingDisabled(scope.to_path_buf()));
        }
        return decide(false, waited, None, None);
    }

    let first = indexed(scope, &token);
    if first == Some(true) {
        // A hit is proof; there is nothing a second query could add.
        return decide(true, waited, first, None);
    }
    // The margin scales with the latency this run just measured, not with a constant that
    // was calibrated on an idle machine — see [`Timing::settle_after`].
    std::thread::sleep(timing.settle_after(waited));
    let second = indexed(scope, &token);
    decide(true, waited, first, second)
}

/// Plant the probe, then the control — IN THAT ORDER, which is why this is one function and
/// not two calls at the call site.
///
/// A miss is only evidence relative to something that WAS indexed later. Because the probe's
/// write lands on the volume's fsevents stream strictly before the control's, the control
/// becoming visible means the indexer has already been offered the probe's event. That
/// per-volume ordering is a strong heuristic, not a documented contract (fsevents coalesce
/// per directory and `mds` has more than one worker), which is what the [`Timing::settle`]
/// margin and the SECOND candidate query buy. If the heuristic ever breaks, the error lands
/// on the safe side: a late-arriving probe reads as `Indexed`, a false alarm the user
/// re-runs. The dangerous direction — a false `Excluded` — needs the probe's event to be
/// delayed past BOTH the control and the settle.
fn plant(dir: &Path, control_dir: &Path, token: &str) -> std::io::Result<(PathBuf, PathBuf)> {
    let probe = dir.join(token);
    // Real bytes, not an empty file: an importer with nothing to import is one more way a
    // miss could mean something other than "excluded".
    std::fs::write(&probe, b"atpkg noindex probe\n")?;
    std::fs::create_dir_all(control_dir)?;
    let control = control_dir.join(control_file_name(token));
    std::fs::write(&control, b"atpkg noindex control\n")?;
    Ok((probe, control))
}

/// Removes the probe file and the whole control directory on drop, on every path.
struct Cleanup {
    /// The probe file planted inside the candidate directory.
    probe: PathBuf,
    /// The control directory this module created, removed whole.
    control_dir: PathBuf,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        // Both are best-effort and both are unconditional: a probe left behind in someone's
        // repo is exactly the litter that makes a hygiene tool untrusted.
        let _ = std::fs::remove_file(&self.probe);
        let _ = std::fs::remove_dir_all(&self.control_dir);
    }
}

/// One `mdfind` question: is a file named `filename` in the index under `scope`?
/// `None` means the question could not be asked and must never read as "no".
fn indexed(scope: &Path, filename: &str) -> Option<bool> {
    crate::platform::spotlight_query(scope, filename)
}

// ---------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt as _;

    /// A synthetic root under `temp_dir()`. Module tag + per-test label + PID, so parallel
    /// tests and concurrent runs cannot collide. Nothing here reads the environment or the
    /// user's real home, and no test below queries Spotlight: on macOS `temp_dir()` is
    /// `/private/var/folders/…/T/`, which is typically not indexed at all, so an index query
    /// there would be meaningless AND flaky. The race logic is tested through [`decide`].
    fn scratch(label: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("atpkg-noindex-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        root
    }

    /// A directory carrying cargo's real cache-dir tag.
    fn tagged_target(at: &Path) {
        std::fs::create_dir_all(at).unwrap();
        let mut tag = String::from(CACHEDIR_TAG_SIGNATURE);
        tag.push_str("\n# This file is a cache directory tag created by cargo.\n");
        std::fs::write(at.join("CACHEDIR.TAG"), tag.as_bytes()).unwrap();
    }

    // --- APPLY ------------------------------------------------------------------------

    // The pure config editor over the three shapes a real `.cargo/config.toml` takes,
    // plus the two refusals. The aterm repo's own config (a `[target.'cfg(…)']` table,
    // `[alias]`, no `[build]`) is the appended case.
    #[test]
    fn point_cargo_config_covers_absent_table_existing_table_and_existing_key() {
        // No file at all.
        assert_eq!(
            point_cargo_config("", "target", "target.noindex"),
            ConfigEdit::Written("[build]\ntarget-dir = \"target.noindex\"\n".to_string())
        );
        // Tables but no [build]: appended after a blank line, other tables untouched.
        let aterm = "[target.'cfg(trust_verify)']\nrustflags = [\"-Ztrust-verify=off\"]\n\n\
                     [alias]\nship = \"run --release -p aterm-release --\"\n";
        let ConfigEdit::Written(t) = point_cargo_config(aterm, "target", "target.noindex") else {
            panic!("appended");
        };
        assert!(t.starts_with(aterm), "{t}");
        assert!(
            t.ends_with("\n[build]\ntarget-dir = \"target.noindex\"\n"),
            "{t}"
        );
        // An existing [build] with other keys: the key lands right under the header.
        let with_build = "[build]\njobs = 4\n\n[alias]\nx = \"y\"\n";
        let ConfigEdit::Written(t) = point_cargo_config(with_build, "target", "target.noindex")
        else {
            panic!("inserted");
        };
        assert_eq!(
            t,
            "[build]\ntarget-dir = \"target.noindex\"\njobs = 4\n\n[alias]\nx = \"y\"\n"
        );
        // An existing target-dir naming the old directory (three spellings) is rewritten.
        for old in ["target", "./target", "/Users//x/repo/target/"] {
            let text = format!("[build]\ntarget-dir = \"{old}\"\n");
            assert_eq!(
                point_cargo_config(&text, "target", "target.noindex"),
                ConfigEdit::Rewritten("[build]\ntarget-dir = \"target.noindex\"\n".to_string()),
                "{old}"
            );
        }
        // An existing target-dir pointing elsewhere is left alone.
        assert_eq!(
            point_cargo_config("[build]\ntarget-dir = \"/Volumes/fast/t\"\n", "target", "x"),
            ConfigEdit::PointsElsewhere("/Volumes/fast/t".to_string())
        );
        // A target-dir key OUTSIDE [build] is not cargo's and is ignored.
        let ConfigEdit::Written(t) =
            point_cargo_config("[other]\ntarget-dir = \"z\"\n", "target", "target.noindex")
        else {
            panic!("written");
        };
        assert!(t.contains("[other]\ntarget-dir = \"z\"\n"), "{t}");
        // The [build] header is recognized by what it names, not by its exact bytes: a
        // trailing comment or inner spaces used to miss the table, append a second
        // `[build]`, and refuse as unparseable AFTER the rename (2026-09-12, audit K11).
        let ConfigEdit::Written(t) = point_cargo_config(
            "[build] # faster links\njobs = 1\n",
            "target",
            "target.noindex",
        ) else {
            panic!("inserted under a commented header");
        };
        assert!(
            t.contains("[build] # faster links\ntarget-dir = \"target.noindex\"\n"),
            "{t}"
        );
        assert_eq!(t.matches("[build]").count(), 1, "{t}");
        assert_eq!(
            point_cargo_config("[ build ]\njobs = 1\n", "target", "target.noindex"),
            ConfigEdit::Written(
                "[ build ]\ntarget-dir = \"target.noindex\"\njobs = 1\n".to_string()
            )
        );
        // A sub-table is not the build table.
        let ConfigEdit::Written(t) =
            point_cargo_config("[build.x]\ny = 1\n", "target", "target.noindex")
        else {
            panic!("appended beside a sub-table");
        };
        assert!(
            t.ends_with("\n[build]\ntarget-dir = \"target.noindex\"\n"),
            "{t}"
        );
        // Something that cannot be made to parse is refused, never written.
        assert!(matches!(
            point_cargo_config("[build\nbroken = ", "target", "t"),
            ConfigEdit::Unparseable(_)
        ));
        // The value is relative when the target is a direct child of the repo.
        assert_eq!(
            target_dir_value(Path::new("/r"), Path::new("/r/target.noindex")),
            "target.noindex"
        );
        assert_eq!(
            target_dir_value(Path::new("/r"), Path::new("/elsewhere/target.noindex")),
            "/elsewhere/target.noindex"
        );
    }

    // `apply_one` over a synthetic repo: the rename, the config write (with the
    // relative name), idempotence on the second run, and the build-output sentinel
    // (`.metadata_never_index`, which cli.rs reads four levels above a seed dir)
    // travelling with the tree so the reader and the writers keep agreeing.
    #[test]
    fn apply_one_migrates_points_cargo_and_is_idempotent() {
        if !SUPPORTED {
            let root = scratch("apply-na");
            assert!(matches!(
                apply_one(&root, false, false),
                Applied::Skipped { .. }
            ));
            return;
        }
        let root = scratch("apply-one");
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        let target = repo.join("target");
        tagged_target(&target);
        // `aterm-release` writes the sentinel into the directory it produces the
        // bundle IN (`target/release/`), and cli.rs reads it four levels above the
        // seed dir: seed → Resources → Contents → aterm.app → release.
        let seed = target.join("release/aterm.app/Contents/Resources/seed");
        std::fs::create_dir_all(&seed).unwrap();
        std::fs::write(target.join("release/.metadata_never_index"), "").unwrap();
        // Dry run first: nothing moves, the plan names the config.
        match apply_one(&target, true, true) {
            Applied::Planned { from, to, pointer } => {
                assert_eq!(from, target);
                assert_eq!(to, repo.join("target.noindex"));
                assert_eq!(pointer, Pointer::Config(repo.join(".cargo/config.toml")));
            }
            other => panic!("{other:?}"),
        }
        assert!(target.is_dir() && !repo.join("target.noindex").exists());
        // The real thing.
        let out = apply_one(&target, true, false);
        let Applied::Migrated { from, to, config } = out else {
            panic!("{out:?}");
        };
        assert!(out_is(&from, &target) && to == repo.join("target.noindex"));
        assert_eq!(config, ConfigNote::Written(repo.join(".cargo/config.toml")));
        assert!(!target.exists() && to.is_dir());
        assert_eq!(
            std::fs::read_to_string(repo.join(".cargo/config.toml")).unwrap(),
            "[build]\ntarget-dir = \"target.noindex\"\n"
        );
        // The sentinel moved with the tree, four levels above the seed dir.
        let moved_seed = to.join("release/aterm.app/Contents/Resources/seed");
        assert!(
            moved_seed
                .ancestors()
                .nth(4)
                .is_some_and(|d| d.join(".metadata_never_index").exists()),
            "the build-output sentinel travels with the renamed tree"
        );
        // Idempotent: the new name is already excluded; the config is not rewritten.
        assert_eq!(
            apply_one(&to, true, false),
            Applied::AlreadyExcluded(to.clone())
        );
        assert_eq!(
            std::fs::read_to_string(repo.join(".cargo/config.toml")).unwrap(),
            "[build]\ntarget-dir = \"target.noindex\"\n"
        );
        // A free-standing target dir is skipped under the walk, migrated by name.
        let free = root.join("free-target");
        tagged_target(&free);
        assert!(matches!(
            apply_one(&free, true, false),
            Applied::Skipped { ref reason, .. } if reason.contains("not beside a Cargo.toml")
        ));
        assert!(free.is_dir());
        let out = apply_one(&free, false, false);
        assert!(
            matches!(out, Applied::Migrated { ref config, .. } if matches!(config, ConfigNote::Untouched(_))),
            "{out:?}"
        );
        assert!(root.join("free-target.noindex").is_dir());
        let _ = std::fs::remove_dir_all(&root);
    }

    // A `.cargo/config.toml` that is a symlink (a config shared across checkouts, a
    // dotfiles-managed one) is edited THROUGH the link: the temp is renamed over the file
    // the link names, never over the link, which would leave a detached regular copy and
    // stop later edits to the shared file reaching this repo (2026-09-12, the K12 class).
    // A dangling link is refused before anything is renamed, never replaced by a file.
    #[cfg(unix)]
    #[test]
    fn repo_config_is_written_through_a_symlinked_config_toml() {
        let root = scratch("config-symlink");
        let repo = root.join("repo");
        let shared = root.join("shared");
        std::fs::create_dir_all(repo.join(".cargo")).unwrap();
        std::fs::create_dir_all(&shared).unwrap();
        let real = shared.join("config.toml");
        std::fs::write(&real, "[alias]\nb = \"build\"\n").unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
        let link = repo.join(".cargo").join("config.toml");
        std::os::unix::fs::symlink("../../shared/config.toml", &link).unwrap();
        let (from, to) = (repo.join("target"), repo.join("target.noindex"));

        let plan = plan_repo_config(&repo, &from, &to).expect("a linked config plans");
        let note = write_repo_config(&repo, plan).expect("and writes");
        assert!(matches!(note, ConfigNote::Written(_)), "{note:?}");
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "config.toml must stay a symlink"
        );
        let text = std::fs::read_to_string(&real).unwrap();
        assert!(
            text.starts_with("[alias]") && text.contains("target-dir = \"target.noindex\""),
            "the edit lands in the link's target: {text}"
        );
        assert_eq!(
            std::fs::metadata(&real).unwrap().permissions().mode() & 0o777,
            0o600,
            "the real file keeps its mode"
        );
        let strays: Vec<_> = [repo.join(".cargo"), shared.clone()]
            .iter()
            .flat_map(|d| std::fs::read_dir(d).unwrap().filter_map(Result::ok))
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("atpkg"))
            .collect();
        assert!(strays.is_empty(), "temp files left: {strays:?}");

        // Dangling: refused at plan time, the link left exactly as it was.
        let dangling = root.join("dangling");
        std::fs::create_dir_all(dangling.join(".cargo")).unwrap();
        let dlink = dangling.join(".cargo").join("config.toml");
        std::os::unix::fs::symlink(root.join("not-checked-out.toml"), &dlink).unwrap();
        let refused = plan_repo_config(
            &dangling,
            &dangling.join("target"),
            &dangling.join("target.noindex"),
        );
        assert!(refused.is_err(), "a dangling config link is refused");
        assert!(
            std::fs::symlink_metadata(&dlink)
                .unwrap()
                .file_type()
                .is_symlink()
                && !root.join("not-checked-out.toml").exists()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    // A config that cannot be pointed must refuse BEFORE the rename. Until 2026-09-12 the
    // non-git branch renamed first and then reported `cargo: NOT re-pointed`, leaving
    // cargo aimed at a `target/` that no longer existed: the next build rebuilt an
    // indexed `target/`, the renamed tree was orphaned, and every later pass skipped
    // silently on "already exists" (audit K11). Three ways in: a config the editor
    // cannot make parse, a legacy `.cargo/config` cargo would prefer over what gets
    // written, and a write that fails after the rename (which rolls the rename back).
    #[test]
    fn apply_one_rolls_the_rename_back_when_the_cargo_config_cannot_be_pointed() {
        if !SUPPORTED {
            return;
        }
        let root = scratch("apply-unpointable");
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join(".cargo")).unwrap();
        std::fs::write(repo.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        let target = repo.join("target");
        tagged_target(&target);
        let noindex = repo.join("target.noindex");

        // (1) Unparseable once edited: a top-level dotted `build.` key already defines
        // the table, so an appended `[build]` header is a duplicate.
        let cfg = "build.jobs = 8\n";
        std::fs::write(repo.join(".cargo/config.toml"), cfg).unwrap();
        for dry_run in [false, false, true] {
            let out = apply_one(&target, true, dry_run);
            assert!(
                matches!(out, Applied::Skipped { ref reason, .. }
                    if reason.contains("valid TOML") && !reason.contains("already exists")),
                "dry_run={dry_run}: {out:?}"
            );
            assert!(target.is_dir(), "dry_run={dry_run}: target/ must stay put");
            assert!(!noindex.exists(), "dry_run={dry_run}: nothing renamed");
            assert_eq!(
                std::fs::read_to_string(repo.join(".cargo/config.toml")).unwrap(),
                cfg
            );
        }

        // (2) A legacy `.cargo/config`: cargo reads it first, so a written config.toml
        // would not move the build.
        std::fs::remove_file(repo.join(".cargo/config.toml")).unwrap();
        std::fs::write(repo.join(".cargo/config"), "[build]\njobs = 1\n").unwrap();
        let out = apply_one(&target, true, false);
        assert!(
            matches!(out, Applied::Skipped { ref reason, .. } if reason.contains(".cargo/config")),
            "{out:?}"
        );
        assert!(target.is_dir() && !noindex.exists());
        assert!(!repo.join(".cargo/config.toml").exists());
        std::fs::remove_file(repo.join(".cargo/config")).unwrap();

        // (3) The write fails after the rename: `.cargo/` read-only. Rolled back.
        #[cfg(unix)]
        {
            let cargo_dir = repo.join(".cargo");
            std::fs::set_permissions(&cargo_dir, std::fs::Permissions::from_mode(0o500)).unwrap();
            // Root writes through 0500; the case only means something when it cannot.
            if std::fs::write(cargo_dir.join("probe"), "").is_err() {
                let out = apply_one(&target, true, false);
                assert!(
                    matches!(out, Applied::Skipped { ref reason, .. } if reason.contains("rolled back")),
                    "{out:?}"
                );
                assert!(target.is_dir() && !noindex.exists());
                assert!(!repo.join(".cargo/config.toml").exists());
            }
            std::fs::set_permissions(&cargo_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    fn out_is(a: &Path, b: &Path) -> bool {
        a == b
    }

    // A build holding cargo's lock refuses the migration — probed with a real
    // `flock(2)` on `<target>/debug/.cargo-lock`, released, then the migration goes.
    #[cfg(unix)]
    #[test]
    fn config_temp_is_never_created_wider_than_0600() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = scratch("config-temp");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".config.toml.atpkg-1.tmp");
        let f = create_config_temp(&path).unwrap();
        assert_eq!(
            f.metadata().unwrap().permissions().mode() & 0o777,
            0o600,
            "the temp must be private from the instant it exists"
        );
        drop(f);
        assert!(
            create_config_temp(&path).is_err(),
            "exclusive: an existing temp is never opened and truncated"
        );
        new_config_permissions(&path).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644,
            "a config this pass creates gets the ordinary mode"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_live_cargo_lock_refuses_the_migration_until_released() {
        use std::os::unix::io::AsRawFd as _;
        let root = scratch("apply-lock");
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        let target = repo.join("target");
        tagged_target(&target);
        std::fs::create_dir_all(target.join("debug")).unwrap();
        let lock_path = target.join("debug/.cargo-lock");
        std::fs::write(&lock_path, "").unwrap();
        assert_eq!(
            build_in_progress(&target),
            None,
            "an unheld lock file is not a build"
        );
        let held = std::fs::OpenOptions::new()
            .write(true)
            .open(&lock_path)
            .unwrap();
        // SAFETY: flock on an open descriptor owned by this test.
        assert_eq!(unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX) }, 0);
        assert_eq!(build_in_progress(&target), Some(lock_path.clone()));
        if SUPPORTED {
            let out = apply_one(&target, true, false);
            assert!(
                matches!(out, Applied::Skipped { ref reason, .. } if reason.contains("a build holds")),
                "{out:?}"
            );
            assert!(target.is_dir(), "nothing moved under a live build");
        }
        drop(held);
        assert_eq!(build_in_progress(&target), None);
        // A `--target <triple>` build locks one level deeper — seen too (review
        // 2026-09-10), and a plain file at that depth that nobody holds is not a build.
        let deep = target.join("aarch64-apple-darwin/release/.cargo-lock");
        std::fs::create_dir_all(deep.parent().unwrap()).unwrap();
        std::fs::write(&deep, "").unwrap();
        assert_eq!(build_in_progress(&target), None);
        let held = std::fs::OpenOptions::new().write(true).open(&deep).unwrap();
        // SAFETY: flock on an open descriptor owned by this test.
        assert_eq!(unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX) }, 0);
        assert_eq!(build_in_progress(&target), Some(deep.clone()));
        drop(held);
        assert_eq!(build_in_progress(&target), None);
        if SUPPORTED {
            assert!(apply_one(&target, true, false).migrated());
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `git` with the user's global and system config masked, so a signing or hooks
    /// setting on the developer's machine cannot shape the fixture.
    #[cfg(unix)]
    fn git_fixture(dir: &Path, args: &[&str]) -> bool {
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    #[cfg(unix)]
    fn git_status(dir: &Path) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["status", "--porcelain"])
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// THE DESIGN RULE (module doc, 2026-09-10): a git checkout is NEVER dirtied. The
    /// fixture is the aterm repo's shape — a TRACKED `.cargo/config.toml`, `target` in
    /// `.gitignore` — plus a linked worktree (a `.git` FILE) and a crate nested inside
    /// the checkout (no `.git` beside it). After `apply` in each: `target` is a relative
    /// symlink to `target.noindex`, the config is byte-identical, and `git status
    /// --porcelain` is EMPTY — the release cutter's own dirty test. A repo that does not
    /// ignore `target` still gets no config write and is told the link is untracked.
    /// Then the shape the first cut of this got WRONG: a repo whose ignore entries are
    /// directory-only (`target/`, `/target-tippy/` — the aterm checkout's own
    /// `.gitignore:55`, and 20 of the 26 planned dirs on m21). Those match the directory
    /// and NOT the symlink, so without the link's own exclude line the pass turned an
    /// ignored directory into a `?? target` row that `clean_tree` would refuse.
    #[cfg(unix)]
    #[test]
    fn a_git_checkout_gets_a_symlink_and_stays_clean_never_a_config_edit() {
        if !SUPPORTED {
            return;
        }
        let root = scratch("apply-git");
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join(".cargo")).unwrap();
        if !git_fixture(&repo, &["init", "-q", "--template=", "-b", "main"]) {
            eprintln!("git is not runnable here; the checkout fixture is skipped");
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
        let config = repo.join(".cargo/config.toml");
        let config_text = "[alias]\nx = \"y\"\n";
        std::fs::write(&config, config_text).unwrap();
        std::fs::write(repo.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        std::fs::write(repo.join(".gitignore"), "target\n").unwrap();
        assert!(git_fixture(&repo, &["add", "."]));
        assert!(git_fixture(&repo, &["commit", "-q", "-m", "fixture"]));
        let target = repo.join("target");
        tagged_target(&target);
        std::fs::write(target.join("debug-artifact"), b"payload").unwrap();
        assert_eq!(git_status(&repo), "", "the fixture starts clean");
        assert!(is_git_checkout(&repo));
        assert_eq!(pointer_for(Some(&repo)), Pointer::Symlink);

        // Dry run: the plan names the symlink, and nothing moves or is written.
        match apply_one(&target, true, true) {
            Applied::Planned { from, to, pointer } => {
                assert_eq!(from, target);
                assert_eq!(to, repo.join("target.noindex"));
                assert_eq!(pointer, Pointer::Symlink);
            }
            other => panic!("{other:?}"),
        }
        assert!(std::fs::symlink_metadata(&target).unwrap().is_dir());
        assert_eq!(git_status(&repo), "");

        // The real thing.
        let out = apply_one(&target, true, false);
        let Applied::Migrated {
            from,
            to,
            config: note,
        } = out
        else {
            panic!("{out:?}");
        };
        assert_eq!(from, target);
        assert_eq!(to, repo.join("target.noindex"));
        let ConfigNote::Linked {
            link,
            exclude,
            link_exclude,
        } = note
        else {
            panic!("a git checkout is linked, never config-edited: {note:?}");
        };
        assert_eq!(link, target);
        assert_eq!(
            link_exclude,
            ExcludeNote::AlreadyIgnored,
            "`target` (no slash) in .gitignore matches the link too"
        );
        let ExcludeNote::Added(exclude_file) = exclude else {
            panic!("the new name is not matched by `target`, so it is excluded: {exclude:?}");
        };
        assert!(
            exclude_file.starts_with(repo.join(".git")),
            "outside the working tree: {}",
            exclude_file.display()
        );
        assert!(
            std::fs::read_to_string(&exclude_file)
                .unwrap()
                .contains("/target.noindex\n")
        );
        assert_eq!(
            std::fs::read_link(&target).unwrap(),
            Path::new("target.noindex"),
            "a RELATIVE link"
        );
        assert!(to.is_dir());
        assert_eq!(
            std::fs::read(target.join("debug-artifact")).unwrap(),
            b"payload",
            "cargo's path still reaches the tree, through the link"
        );
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            config_text,
            "the tracked config is byte-identical"
        );
        assert!(!repo.join(".cargo/.config.toml.atpkg-tmp").exists());
        assert_eq!(
            git_status(&repo),
            "",
            "the working tree is CLEAN after apply"
        );

        // Idempotent from either spelling; the exclude file is not appended to again.
        let exclude_once = std::fs::read_to_string(&exclude_file).unwrap();
        assert_eq!(
            apply_one(&target, true, false),
            Applied::AlreadyExcluded(target.clone()),
            "the link a previous pass left is a success, not a symlink refusal"
        );
        assert_eq!(
            apply_one(&to, true, false),
            Applied::AlreadyExcluded(to.clone())
        );
        assert_eq!(
            std::fs::read_to_string(&exclude_file).unwrap(),
            exclude_once
        );
        assert_eq!(git_status(&repo), "");

        // A linked worktree: `.git` is a FILE, the exclude is the clone's shared one,
        // so the second checkout's name is already ignored.
        let wt = root.join("wt");
        assert!(git_fixture(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                &wt.display().to_string(),
                "-b",
                "wt"
            ]
        ));
        assert!(
            std::fs::symlink_metadata(wt.join(".git"))
                .unwrap()
                .is_file()
        );
        tagged_target(&wt.join("target"));
        assert_eq!(git_status(&wt), "");
        let out = apply_one(&wt.join("target"), true, false);
        assert!(
            matches!(
                &out,
                Applied::Migrated {
                    config: ConfigNote::Linked {
                        exclude: ExcludeNote::AlreadyIgnored,
                        link_exclude: ExcludeNote::AlreadyIgnored,
                        ..
                    },
                    ..
                }
            ),
            "{out:?}"
        );
        assert_eq!(
            std::fs::read_link(wt.join("target")).unwrap(),
            Path::new("target.noindex")
        );
        assert_eq!(
            std::fs::read_to_string(wt.join(".cargo/config.toml")).unwrap(),
            config_text,
            "the worktree's checked-out config is byte-identical"
        );
        assert_eq!(git_status(&wt), "", "the worktree is CLEAN after apply");

        // A crate nested inside the checkout: no `.git` beside it, git rev-parse says
        // it is inside one, and the exclude line is relative to the TOPLEVEL.
        let nested = repo.join("crates/inner");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("Cargo.toml"), "[package]\nname = \"i\"\n").unwrap();
        std::fs::write(nested.join("lib.rs"), "").unwrap();
        assert!(git_fixture(&repo, &["add", "."]));
        assert!(git_fixture(&repo, &["commit", "-q", "-m", "nested"]));
        tagged_target(&nested.join("target"));
        assert!(!nested.join(".git").exists());
        assert!(is_git_checkout(&nested));
        let out = apply_one(&nested.join("target"), true, false);
        assert!(
            matches!(
                &out,
                Applied::Migrated {
                    config: ConfigNote::Linked {
                        exclude: ExcludeNote::Added(_),
                        ..
                    },
                    ..
                }
            ),
            "{out:?}"
        );
        assert!(
            std::fs::read_to_string(&exclude_file)
                .unwrap()
                .contains("/crates/inner/target.noindex\n")
        );
        assert!(!nested.join(".cargo").exists());
        assert_eq!(
            git_status(&repo),
            "",
            "the checkout is CLEAN after the nested apply"
        );

        // A checkout that does NOT ignore `target`: still no config, the link is laid,
        // the new name is excluded, and the note says the link shows as untracked —
        // exactly as the directory did before.
        let bare = root.join("bare");
        std::fs::create_dir_all(&bare).unwrap();
        assert!(git_fixture(
            &bare,
            &["init", "-q", "--template=", "-b", "main"]
        ));
        std::fs::write(bare.join("Cargo.toml"), "[package]\nname = \"b\"\n").unwrap();
        assert!(git_fixture(&bare, &["add", "."]));
        assert!(git_fixture(&bare, &["commit", "-q", "-m", "fixture"]));
        tagged_target(&bare.join("target"));
        assert_eq!(git_status(&bare), "?? target/\n", "untracked before");
        let out = apply_one(&bare.join("target"), true, false);
        assert!(
            matches!(
                &out,
                Applied::Migrated {
                    config: ConfigNote::Linked {
                        exclude: ExcludeNote::Added(_),
                        link_exclude: ExcludeNote::NotIgnored(why),
                        ..
                    },
                    ..
                } if why == "the link shows as untracked, as the directory did"
            ),
            "{out:?}"
        );
        assert!(
            !bare.join(".cargo").exists(),
            "no config edit, gitignore or not"
        );
        assert_eq!(
            git_status(&bare),
            "?? target\n",
            "the link is untracked as the directory was; the new name is not"
        );

        // Directory-only ignore entries: `target/` and `/target-tippy/` match the
        // directories and NOT the symlinks that replace them (measured: `git
        // check-ignore` answers 0 for the directory, 1 for the link). The pass probes
        // the directory before the rename and gives each link its own exclude line, so
        // status is exactly what it was — empty — and a second apply is `AlreadyExcluded`
        // with the exclude file untouched.
        let slashed = root.join("slashed");
        std::fs::create_dir_all(&slashed).unwrap();
        assert!(git_fixture(
            &slashed,
            &["init", "-q", "--template=", "-b", "main"]
        ));
        std::fs::write(slashed.join("Cargo.toml"), "[package]\nname = \"s\"\n").unwrap();
        std::fs::write(slashed.join(".gitignore"), "target/\n/target-tippy/\n").unwrap();
        assert!(git_fixture(&slashed, &["add", "."]));
        assert!(git_fixture(&slashed, &["commit", "-q", "-m", "fixture"]));
        let s_target = slashed.join("target");
        let s_tippy = slashed.join("target-tippy");
        tagged_target(&s_target);
        tagged_target(&s_tippy);
        assert_eq!(
            git_status(&slashed),
            "",
            "the directory-only patterns ignore both DIRECTORIES"
        );
        assert_eq!(git_ignores(&slashed, &s_target), Some(true));
        assert_eq!(git_ignores(&slashed, &s_tippy), Some(true));
        let mut s_exclude = None;
        for (dir, new_name) in [
            (&s_target, "target.noindex"),
            (&s_tippy, "target-tippy.noindex"),
        ] {
            let out = apply_one(dir, true, false);
            let Applied::Migrated {
                config:
                    ConfigNote::Linked {
                        link,
                        exclude: ExcludeNote::Added(exclude_file),
                        link_exclude: ExcludeNote::Added(link_file),
                    },
                ..
            } = out
            else {
                panic!("the new name AND the link are excluded: {out:?}");
            };
            assert_eq!(&link, dir);
            assert_eq!(
                link_file, exclude_file,
                "the link's line lands in the same exclude file"
            );
            assert!(exclude_file.starts_with(slashed.join(".git")));
            assert_eq!(std::fs::read_link(dir).unwrap(), Path::new(new_name));
            assert_eq!(
                git_ignores(&slashed, dir),
                Some(true),
                "read back through git: the link is ignored again"
            );
            assert_eq!(
                git_status(&slashed),
                "",
                "the tree is as clean after {} as before — no `?? {}` row",
                dir.display(),
                dir.file_name().unwrap().to_string_lossy()
            );
            s_exclude = Some(exclude_file);
        }
        let s_exclude = s_exclude.unwrap();
        let s_text = std::fs::read_to_string(&s_exclude).unwrap();
        for line in [
            "/target.noindex",
            "/target",
            "/target-tippy.noindex",
            "/target-tippy",
        ] {
            assert!(
                s_text.lines().any(|l| l == line),
                "{line} is in the exclude file:\n{s_text}"
            );
        }
        assert!(
            !slashed.join(".cargo").exists(),
            "no config edit here either"
        );
        // Idempotent: the links a previous pass left are a success, and nothing is
        // appended again.
        assert_eq!(
            apply_one(&s_target, true, false),
            Applied::AlreadyExcluded(s_target.clone())
        );
        assert_eq!(
            apply_one(&s_tippy, true, false),
            Applied::AlreadyExcluded(s_tippy.clone())
        );
        assert_eq!(
            std::fs::read_to_string(&s_exclude).unwrap(),
            s_text,
            "the second apply re-appends nothing"
        );
        assert_eq!(
            git_status(&slashed),
            "",
            "still clean after the second apply"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    // The walk: exposed repo targets migrate, a free-standing one is skipped with its
    // reason, an already-hidden one is not touched, and the count of MIGRATED is what
    // the pass reports.
    #[test]
    fn apply_under_migrates_repo_targets_and_reports_only_what_moved() {
        if !SUPPORTED {
            return;
        }
        let root = scratch("apply-under");
        for name in ["a", "b"] {
            let repo = root.join(name);
            std::fs::create_dir_all(&repo).unwrap();
            std::fs::write(repo.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
            tagged_target(&repo.join("target"));
        }
        tagged_target(&root.join("free-target"));
        tagged_target(&root.join("c/target.noindex"));
        let (outcomes, complete) = apply_under(&root, DOCTOR_DEPTH, &Budget::VERB, false);
        assert!(complete);
        let migrated = outcomes.iter().filter(|o| o.migrated()).count();
        assert_eq!(migrated, 2, "{outcomes:?}");
        assert!(root.join("a/target.noindex").is_dir());
        assert!(root.join("b/target.noindex").is_dir());
        assert!(root.join("free-target").is_dir(), "free-standing: skipped");
        assert!(root.join("c/target.noindex").is_dir());
        assert!(outcomes.iter().any(
            |o| matches!(o, Applied::Skipped { path, .. } if path == &root.join("free-target"))
        ));
        // Second pass: nothing exposed but the free one; zero migrated.
        let (outcomes, _) = apply_under(&root, DOCTOR_DEPTH, &Budget::VERB, false);
        assert_eq!(outcomes.iter().filter(|o| o.migrated()).count(), 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    // --- PURE -------------------------------------------------------------------------

    #[test]
    fn exclusion_reads_the_name_never_the_index() {
        assert_eq!(exclusion_of(Path::new("/a/b/target")), Exclusion::Exposed);
        assert_eq!(
            exclusion_of(Path::new("/a/b/target.noindex")),
            Exclusion::NoindexSuffix
        );
        // The measured whole-subtree row: a file six levels down inside a `.noindex`
        // directory was not indexed either (2026-09-02).
        assert_eq!(
            exclusion_of(Path::new("/a/b/target.noindex/deep/6/levels/down/x")),
            Exclusion::NoindexSuffix
        );
        // The measured transitive dot row, re-measured 2026-09-02 — this is what makes
        // `scan`'s dot pruning a correctness rule.
        assert_eq!(
            exclusion_of(Path::new("/a/.cargo-target-m7c/debug")),
            Exclusion::DotHidden
        );
        // A dot in the MIDDLE does nothing — the row a naive `contains('.')` gets wrong.
        assert_eq!(
            exclusion_of(Path::new("/a/d_.subdot/target")),
            Exclusion::Exposed
        );
    }

    #[test]
    fn cargos_cachedir_signature_is_pinned_by_its_measured_bytes() {
        // Read off /Users//example/.cargo-target-m7c/CACHEDIR.TAG on 2026-09-02.
        assert!(is_cachedir_tag(
            "Signature: 8a477f597d28d172789f06886806bc55"
        ));
        assert!(
            is_cachedir_tag("Signature: 8a477f597d28d172789f06886806bc55\r"),
            "a CRLF-written tag is still cargo's tag"
        );
        assert!(!is_cachedir_tag("Signature: deadbeef"));
        assert!(!is_cachedir_tag(""), "an empty first line is not a tag");
    }

    #[test]
    fn destination_appends_the_suffix_and_is_idempotent() {
        assert_eq!(
            destination(Path::new("/a/b/target")),
            Some(PathBuf::from("/a/b/target.noindex"))
        );
        // The rule is the SUFFIX, not the name — that spelling really exists on this
        // machine.
        assert_eq!(
            destination(Path::new("/a/b/aterm-cursor-echo-gui-target")),
            Some(PathBuf::from("/a/b/aterm-cursor-echo-gui-target.noindex"))
        );
        assert_eq!(
            destination(Path::new("/a/b/target.noindex")),
            None,
            "an already-migrated path has no destination, which is what makes migrate \
             idempotent"
        );
        assert_eq!(destination(Path::new("/")), None, "a root has no file name");
    }

    #[test]
    fn the_query_is_scoped_at_the_parent_never_at_the_candidate() {
        // The regression test for the MEASURED false positive (2026-09-02, macOS 26.6.2):
        //   mdfind -onlyin ~/mdx-probe-scratch/target.noindex 'kMDItemFSName == "<probe>"'
        // PRINTED the file, while the same predicate scoped at ~/mdx-probe-scratch, at ~,
        // and unscoped all returned nothing. Pointing -onlyin AT an excluded directory makes
        // mdfind answer from a live scan instead of the index, so scoping at the candidate
        // would have reported every correctly-excluded directory as Indexed.
        assert_eq!(
            verify_scope(Path::new("/a/b/target.noindex")),
            Some(Path::new("/a/b"))
        );
        assert_eq!(verify_scope(Path::new("/")), None);
    }

    #[test]
    fn probe_token_is_one_query_safe_word() {
        let t = probe_token(1_756_000_000_123_456_789, 4242);
        assert!(
            t.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()),
            "the token is interpolated into an mdfind predicate with no quoting: {t}"
        );
        assert!((20..=48).contains(&t.len()), "{t} has length {}", t.len());
        for bad in ['"', ' ', '.', '-'] {
            assert!(
                !t.contains(bad),
                "{bad:?} would either need quoting or let Spotlight's tokenizer split the \
                 name: {t}"
            );
        }
        assert_ne!(
            t,
            probe_token(1_756_000_000_123_456_790, 4242),
            "two probes in the same process must not collide"
        );
    }

    #[test]
    fn decide_never_infers_exclusion_without_a_control() {
        // The whole point of the control. Without it a miss is indistinguishable from a
        // file the indexer has not reached yet.
        let v = decide(false, Duration::from_secs(20), Some(false), Some(false));
        assert!(
            matches!(
                v,
                Verdict::Unknown(Unmeasured::ControlNeverIndexed { waited_secs: 20 })
            ),
            "{v}"
        );
    }

    #[test]
    fn decide_is_asymmetric_a_hit_is_proof_a_miss_is_an_inference() {
        let d = Duration::from_secs(2);
        assert!(
            matches!(decide(true, d, Some(true), None), Verdict::Indexed),
            "a hit short-circuits"
        );
        assert!(
            matches!(decide(true, d, Some(false), Some(true)), Verdict::Indexed),
            "a late hit is still proof"
        );
        let v = decide(true, d, None, None);
        assert!(matches!(v, Verdict::Unknown(Unmeasured::Io(_))), "{v}");
        let v = decide(true, d, Some(false), None);
        assert!(
            matches!(v, Verdict::Unknown(Unmeasured::Io(_))),
            "an unaskable question is never a miss: {v}"
        );
        let v = decide(true, d, Some(false), Some(false));
        assert!(matches!(v, Verdict::Excluded), "{v}");
    }

    #[test]
    fn human_size_says_at_least_when_the_walk_was_cut_short() {
        let cut = human_size(&Size {
            bytes: 44_240_539_648,
            complete: false,
        });
        assert!(cut.starts_with("at least "), "{cut}");
        let whole = human_size(&Size {
            bytes: 44_240_539_648,
            complete: true,
        });
        assert!(
            !whole.contains("at least"),
            "a complete walk states a number: {whole}"
        );
    }

    #[test]
    fn cargo_hint_names_both_mechanisms_and_the_gitignore_trap() {
        let text = cargo_hint(Path::new("/a/b/target.noindex")).join("\n");
        assert!(text.contains("CARGO_TARGET_DIR"), "{text}");
        assert!(text.contains("target-dir"), "{text}");
        assert!(text.contains("[build]"), "{text}");
        assert!(
            text.contains("`target/`") && text.contains("`target.noindex/`"),
            "an existing gitignore line does not match the new name, and the first surprise \
             otherwise is a repo full of untracked objects: {text}"
        );
    }

    // --- FILESYSTEM (temp root, no Spotlight) ------------------------------------------

    #[test]
    fn a_target_dir_is_recognized_by_any_of_its_three_signals() {
        let root = scratch("evidence");
        let a = root.join("a");
        tagged_target(&a);
        assert_eq!(target_evidence(&a), Some(Evidence::CachedirTag));

        // The MEASURED shape of /Users//example/aterm/target on 2026-09-02: .rustc_info.json
        // and debug/, and NO CACHEDIR.TAG. A tag-only rule would have missed the very tree
        // that motivated the feature.
        let b = root.join("b");
        std::fs::create_dir_all(b.join("debug")).unwrap();
        std::fs::write(b.join(".rustc_info.json"), b"{}").unwrap();
        assert_eq!(target_evidence(&b), Some(Evidence::RustcInfo));

        let c = root.join("c");
        std::fs::create_dir_all(c.join("debug")).unwrap();
        std::fs::create_dir_all(c.join("release")).unwrap();
        assert_eq!(target_evidence(&c), Some(Evidence::DebugAndRelease));

        let d = root.join("d");
        std::fs::create_dir_all(d.join("debug")).unwrap();
        assert_eq!(
            target_evidence(&d),
            None,
            "debug/ alone is not build output"
        );

        let e = root.join("e");
        std::fs::create_dir_all(&e).unwrap();
        std::fs::write(e.join("CACHEDIR.TAG"), b"Signature: deadbeef\n").unwrap();
        assert_eq!(
            target_evidence(&e),
            None,
            "recognition is fail-closed: a wrong signature is not cargo's"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    // `scan` and `migrate` are honest no-ops off macOS (see
    // `every_entry_point_is_an_honest_no_op`), so their behavioural tests only have
    // something to assert where the behaviour exists.

    #[cfg(target_os = "macos")]
    #[test]
    fn scan_records_a_target_and_does_not_descend_into_it() {
        let root = scratch("scan-record");
        let target = root.join("proj/target");
        tagged_target(&target);
        tagged_target(&target.join("debug/deps/nested/target"));
        let s = scan(&root, VERB_DEPTH, &Budget::VERB);
        assert_eq!(
            s.targets.len(),
            1,
            "a target dir holds millions of files and none of them is another target dir: \
             {:?}",
            s.targets
        );
        assert_eq!(s.targets[0].path, target);
        assert!(s.complete);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn scan_prunes_dot_directories_because_their_subtree_is_already_excluded() {
        // Measured 2026-09-02: a dot-hidden directory's WHOLE subtree is absent from the
        // index. So pruning here is a correctness argument (there is nothing exposed down
        // there to report), not merely an optimization.
        let root = scratch("scan-dot");
        tagged_target(&root.join(".hidden/proj/target"));
        let s = scan(&root, VERB_DEPTH, &Budget::VERB);
        assert!(
            s.targets.is_empty(),
            "nothing under a dot-hidden ancestor is exposed: {:?}",
            s.targets
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn scan_never_follows_a_symlink() {
        let root = scratch("scan-loop");
        tagged_target(&root.join("proj/target"));
        std::os::unix::fs::symlink(&root, root.join("loop")).unwrap();
        let s = scan(&root, VERB_DEPTH, &Budget::VERB);
        assert_eq!(
            s.targets.len(),
            1,
            "a symlink is never followed and never counted twice: {:?}",
            s.targets
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn scan_reports_incomplete_when_a_budget_runs_out() {
        let root = scratch("scan-budget");
        for n in 0..5 {
            std::fs::create_dir_all(root.join(format!("p{n}/sub"))).unwrap();
        }
        let s = scan(
            &root,
            VERB_DEPTH,
            &Budget {
                max_entries: 1,
                max_wall: Duration::from_secs(20),
            },
        );
        assert!(
            !s.complete,
            "a truncated walk must say so, or the report is a census it did not take"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn size_of_sums_regular_files_and_stops_at_its_budget() {
        let root = scratch("size");
        let big = root.join("big");
        std::fs::create_dir_all(&big).unwrap();
        for n in 0..3 {
            std::fs::write(big.join(format!("f{n}")), vec![b'x'; 1024]).unwrap();
        }
        let whole = size_of(&big, &Budget::VERB_SIZE);
        assert!(whole.complete, "the whole tree fits in the verb budget");
        assert!(whole.bytes >= 3072, "{} bytes", whole.bytes);

        let cut = size_of(
            &big,
            &Budget {
                max_entries: 1,
                max_wall: Duration::from_secs(30),
            },
        );
        assert!(!cut.complete, "a truncated sum must say so");

        #[cfg(unix)]
        {
            let linked = root.join("linked");
            std::fs::create_dir_all(&linked).unwrap();
            std::os::unix::fs::symlink(big.join("f0"), linked.join("ptr")).unwrap();
            let s = size_of(&linked, &Budget::VERB_SIZE);
            assert_eq!(s.bytes, 0, "a symlink counts zero and is not followed");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn migrate_renames_once_and_is_a_no_op_the_second_time() {
        let root = scratch("migrate-once");
        let target = root.join("target");
        tagged_target(&target);
        std::fs::write(target.join("artifact"), b"payload").unwrap();
        let dest = root.join("target.noindex");

        let first = migrate(&target, false).unwrap();
        assert_eq!(
            first,
            Migration::Migrated {
                from: target.clone(),
                to: dest.clone()
            }
        );
        assert!(dest.is_dir() && !target.exists());
        assert_eq!(
            std::fs::read(dest.join("artifact")).unwrap(),
            b"payload",
            "a rename(2) moves no bytes and loses none"
        );

        let gone = migrate(&target, false);
        assert!(
            matches!(gone, Err(MigrateError::NotATarget(_))),
            "the original path is gone, so it carries no evidence"
        );
        assert_eq!(
            migrate(&dest, false).unwrap(),
            Migration::AlreadyExcluded(dest.clone()),
            "re-running on a migrated path is idempotent, not an error"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn migrate_never_clobbers_an_existing_destination() {
        let root = scratch("migrate-clobber");
        let target = root.join("target");
        tagged_target(&target);
        std::fs::write(target.join("mine"), b"source").unwrap();
        let dest = root.join("target.noindex");
        tagged_target(&dest);
        std::fs::write(dest.join("theirs"), b"destination").unwrap();

        let e = migrate(&target, false);
        assert!(
            matches!(e, Err(MigrateError::DestinationExists(_))),
            "{e:?}"
        );
        assert!(
            target.is_dir() && dest.is_dir(),
            "never clobber, never merge, never delete"
        );
        assert_eq!(std::fs::read(target.join("mine")).unwrap(), b"source");
        assert_eq!(
            std::fs::read(dest.join("theirs")).unwrap(),
            b"destination",
            "never clobber, never merge, never delete"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn migrate_refuses_a_directory_it_did_not_recognize() {
        // Fail-closed: renaming a directory that is not build output is the one way this
        // utility could cost someone work.
        let root = scratch("migrate-refuse");
        let plain = root.join("notes");
        std::fs::create_dir_all(&plain).unwrap();
        std::fs::write(plain.join("thesis.txt"), b"years of work").unwrap();

        let e = migrate(&plain, false);
        assert!(matches!(e, Err(MigrateError::NotATarget(_))), "{e:?}");
        assert!(plain.is_dir(), "the disk is exactly as it was");
        assert_eq!(
            std::fs::read(plain.join("thesis.txt")).unwrap(),
            b"years of work"
        );
        assert!(
            !root.join("notes.noindex").exists(),
            "no destination was even created"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn migrate_moves_no_bytes() {
        use std::os::unix::fs::MetadataExt as _;
        let root = scratch("migrate-inode");
        let target = root.join("target");
        tagged_target(&target);
        std::fs::write(target.join("artifact"), b"payload").unwrap();
        let before = std::fs::metadata(target.join("artifact")).unwrap();
        let (ino, dev) = (before.ino(), before.dev());

        migrate(&target, false).unwrap();

        let after = std::fs::metadata(root.join("target.noindex/artifact")).unwrap();
        assert_eq!(
            (after.ino(), after.dev()),
            (ino, dev),
            "same inode on the same device — that is what proves it is one rename(2) inside \
             one parent and not a copy of 2 TB"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_probe_is_planted_before_the_control_so_a_control_hit_bounds_the_probes_chance() {
        // The ordering IS the mechanism: the probe's write reaches the volume's fsevents
        // stream strictly before the control's, so a control that turns up in the index
        // means the indexer has already been offered the probe's event. That ordering
        // cannot be asserted at runtime inside `verify`, so it is asserted where it is
        // created. APFS timestamps are nanosecond, so `<=` holds.
        let root = scratch("plant-order");
        let candidate = root.join("target");
        std::fs::create_dir_all(&candidate).unwrap();
        let control_dir = root.join("atpkg-control-probe");
        let (probe, control) = plant(&candidate, &control_dir, "atpkgnoindexprobe0000").unwrap();
        let p = std::fs::metadata(&probe).unwrap().modified().unwrap();
        let c = std::fs::metadata(&control).unwrap().modified().unwrap();
        assert!(p <= c, "probe first, control second — always");
        assert_ne!(
            probe.file_name(),
            control.file_name(),
            "the two exact-name queries must never answer for each other"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_cleanup_guard_removes_the_probe_even_on_an_early_return() {
        let root = scratch("cleanup");
        let candidate = root.join("target");
        std::fs::create_dir_all(&candidate).unwrap();
        let control_dir = root.join("atpkg-control-probe");
        let (probe, control) = plant(&candidate, &control_dir, "atpkgnoindexprobe0001").unwrap();
        {
            let _guard = Cleanup {
                probe: probe.clone(),
                control_dir: control_dir.clone(),
            };
        }
        assert!(
            !probe.exists(),
            "a probe left in someone's repo is exactly the litter that makes a hygiene tool \
             untrusted"
        );
        assert!(
            !control.exists() && !control_dir.exists(),
            "the control dir goes whole"
        );
        assert!(candidate.is_dir(), "and nothing else is touched");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn verify_scope_never_answers_the_empty_path() {
        // `Path::new("target").parent()` is `Some("")`, and `mdfind -onlyin ''` is a clean
        // MISS (measured 2026-09-02), so the empty scope would make even the control
        // unfindable — `verify target` would burn the whole timeout and answer `Unknown`
        // on an idle machine, forever.
        assert_eq!(verify_scope(Path::new("target")), Some(Path::new(".")));
        assert_eq!(verify_scope(Path::new("./target")), Some(Path::new(".")));
        assert_eq!(verify_scope(Path::new(".")), Some(Path::new(".")));
        assert_eq!(
            verify_scope(Path::new("/a/b/target")),
            Some(Path::new("/a/b")),
            "an absolute path is untouched"
        );
    }

    #[test]
    fn the_settle_margin_scales_with_the_latency_the_run_measured() {
        let t = Timing::DEFAULT;
        // The idle machine: the floor governs (control seen in 1.25-1.48 s on 2026-09-02).
        assert_eq!(t.settle_after(Duration::from_millis(1400)), t.settle);
        // The machine this module exists for: 18 rustc processes and `mds` grinding 2.0 TB.
        // A fixed 2 s there is ~0.1x the latency the run just measured, and the margin is
        // the ONLY thing standing between an fsevents-ordering heuristic and a false
        // `Excluded`.
        assert_eq!(
            t.settle_after(Duration::from_secs(18)),
            Duration::from_secs(18)
        );
        assert_eq!(
            t.worst_case(),
            Duration::from_secs(40),
            "what the CLI must announce, so a reader is never surprised into ^C"
        );
    }

    #[test]
    fn human_size_says_unmeasured_when_the_walk_summed_nothing() {
        // The shared sizing budget spent on the first few of 127 target dirs gives every
        // remaining row a zero-length walk. "at least 0 B" is a floor that reads as a
        // measurement of a small tree.
        let none = human_size(&Size {
            bytes: 0,
            complete: false,
        });
        assert_eq!(none, "unmeasured", "a row nothing was read from says so");
        assert_eq!(
            human_size(&Size {
                bytes: 0,
                complete: true
            }),
            crate::cost::human_bytes(0),
            "a COMPLETE walk of an empty tree really did measure zero"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn scan_of_a_missing_root_is_incomplete_never_an_empty_census() {
        // `aterm pkg noindex scan ~/srcc` (one-character typo for a `~/src` holding 40
        // exposed target dirs) must never render as "no cargo target directories".
        let root = scratch("scan-missing");
        let s = scan(&root.join("does-not-exist"), VERB_DEPTH, &Budget::VERB);
        assert!(s.targets.is_empty());
        assert!(
            !s.complete,
            "`complete` licenses the caller to say `there are none`; a path that was never \
             read has not earned it"
        );
        // A regular file named as a root is the same mistake wearing a different hat.
        std::fs::write(root.join("file"), b"x").unwrap();
        assert!(!scan(&root.join("file"), VERB_DEPTH, &Budget::VERB).complete);
        assert!(
            scan(&root, VERB_DEPTH, &Budget::VERB).complete,
            "a real, readable root still reports a complete census"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn migrate_refuses_a_symlink_instead_of_renaming_the_link() {
        // `ln -s /Volumes/fast/target target` is exactly what someone with 2 TB of build
        // output does. `target_evidence` reads THROUGH the link, so recognition passes and
        // `rename(2)` would move the LINK — leaving the build output where it was, still
        // indexed, while every printed line said the migration succeeded.
        let root = scratch("migrate-symlink");
        let real = root.join("elsewhere/realtarget");
        tagged_target(&real);
        let link = root.join("repo/target");
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let e = migrate(&link, false);
        let Err(MigrateError::Symlink { .. }) = &e else {
            panic!("a symlink must be refused, not renamed: {e:?}");
        };
        let text = e.unwrap_err().to_string();
        assert!(text.contains("symlink"), "{text}");
        assert!(
            text.contains(&real.display().to_string()),
            "the refusal names the real directory to run this on instead: {text}"
        );
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the disk is exactly as it was"
        );
        assert!(
            !root.join("repo/target.noindex").exists(),
            "nothing was renamed"
        );
        // A dry run is refused on the same evidence, before it can print a plan.
        assert!(matches!(
            migrate(&link, true),
            Err(MigrateError::Symlink { .. })
        ));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn migrate_reads_the_exclusion_claim_off_the_real_path() {
        // `exclusion_of` is pure and reads COMPONENTS, so a spelling that hides the
        // excluded ancestor — a relative operand from inside `build.noindex`, or a
        // symlinked ancestor as here — would otherwise perform a rename that changes
        // nothing: the whole subtree below a `.noindex` component is already excluded.
        let root = scratch("migrate-through-link");
        let real = root.join("build.noindex/target");
        tagged_target(&real);
        std::os::unix::fs::symlink(root.join("build.noindex"), root.join("link")).unwrap();
        let named = root.join("link/target");

        assert_eq!(
            migrate(&named, false).unwrap(),
            Migration::AlreadyExcluded(named.clone()),
            "the ancestor already excludes this subtree"
        );
        assert!(
            !root.join("build.noindex/target.noindex").exists(),
            "and nothing was renamed"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn verify_refuses_to_measure_through_an_excluded_parent() {
        // MEASURED: pointing `-onlyin` AT an excluded directory makes `mdfind` answer from
        // a live scan rather than from the index, so the probe would be "found" and the
        // verdict would be `Indexed` for a doubly-excluded tree — while `migrate` answers
        // `AlreadyExcluded`. That pair is an unresolvable loop for the reader. No probe is
        // planted here at all, so this is fast and touches nothing.
        let root = scratch("verify-excluded-scope");
        let target = root.join("build.noindex/target");
        tagged_target(&target);
        let v = verify(&target, &Timing::DEFAULT);
        let Verdict::Unknown(Unmeasured::ScopeExcluded(p)) = &v else {
            panic!("an excluded scope can only answer Unknown: {v}");
        };
        assert_eq!(
            p,
            &std::fs::canonicalize(root.join("build.noindex")).unwrap()
        );
        assert!(
            !v.to_string().contains("INDEXED"),
            "the one answer this must never give: {v}"
        );
        assert_eq!(
            std::fs::read_dir(&target).unwrap().count(),
            1,
            "nothing was planted — only cargo's own CACHEDIR.TAG is in there"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_plant_that_fails_halfway_still_leaves_no_probe_behind() {
        // `plant` writes the PROBE first and the control second, so a guard built from its
        // return value does not exist on the path where the probe landed and the control
        // write failed — ENOSPC on the 2 TB machine this targets, or an unwritable parent.
        // The probe is then permanent litter inside the user's target dir, which is
        // exactly what this module's doc promises against.
        if crate::platform::our_uid() == 0 {
            return; // root bypasses the mode bits this test depends on
        }
        let root = scratch("plant-halfway");
        let target = root.join("target");
        tagged_target(&target);
        // r-x: the probe can still be written INTO `target`, but the control directory
        // cannot be created beside it.
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o500)).unwrap();

        let v = verify(&target, &Timing::DEFAULT);
        assert!(
            matches!(v, Verdict::Unknown(Unmeasured::Io(_))),
            "a probe that could not be run is never a measurement — and any OTHER Unknown \
             here means this test never reached the plant it is about: {v}"
        );
        let litter: Vec<PathBuf> = std::fs::read_dir(&target)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("atpkgnoindexprobe"))
            })
            .collect();
        assert!(
            litter.is_empty(),
            "a probe left in someone's repo is exactly the litter that makes a hygiene \
             tool untrusted: {litter:?}"
        );

        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn every_entry_point_is_an_honest_no_op() {
        assert!(!SUPPORTED);
        let root = scratch("noop");
        let target = root.join("target");
        tagged_target(&target);

        assert_eq!(verify(&target, &Timing::DEFAULT), Verdict::NotApplicable);
        assert_eq!(migrate(&target, false).unwrap(), Migration::NotApplicable);
        assert!(
            target.is_dir(),
            "a no-op renames nothing — the directory is still where it was"
        );
        let s = scan(&root, VERB_DEPTH, &Budget::VERB);
        assert!(
            s.targets.is_empty() && s.complete,
            "an empty, COMPLETE scan"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The ONLY test that can catch macOS changing the `.noindex` behaviour this whole
    /// module rests on — `.noindex` is observed on this machine, not a documented API. It is
    /// `#[ignore]`d because it needs a genuinely indexed location (the invoking user's real
    /// home; `temp_dir()` is typically not indexed at all) and takes seconds. Run it after
    /// an OS update:
    /// `cargo test -p atpkg --lib manual_probe_round_trip -- --ignored --nocapture`
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "manual probe: needs a live Spotlight index and writes under the invoking user's home"]
    fn manual_probe_round_trip_against_the_real_index() {
        let Some(home) = aterm_types::dirs::home_dir() else {
            return;
        };
        let root = home.join(format!("atpkg-noindex-manual-{}", std::process::id()));
        let plain = root.join("plain");
        let hidden = root.join("hidden.noindex");
        std::fs::create_dir_all(&plain).unwrap();
        std::fs::create_dir_all(&hidden).unwrap();

        let v = verify(&plain, &Timing::DEFAULT);
        assert!(
            matches!(v, Verdict::Indexed),
            "a plainly-named directory under an indexed home IS indexed: {v}"
        );
        let v = verify(&hidden, &Timing::DEFAULT);
        assert!(
            matches!(v, Verdict::Excluded),
            "a .noindex directory is not — if this fails, macOS changed the behaviour this \
             module measures: {v}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// THE AUTOMATIC WALK NEVER OPENS A macOS-PROTECTED FOLDER (2026-09-10): the
    /// pass at the end of every seed/update walks `$HOME`, and macOS attributes
    /// the per-folder consent dialog to the terminal — so pruning these is what
    /// keeps aterm from raising, in its own name, the prompts its consent design
    /// exists to consolidate. A root the user NAMES is still walked.
    #[test]
    fn the_home_walk_prunes_every_macos_protected_folder() {
        let tmp = std::env::temp_dir().join(format!("aterm-noindex-skip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        for name in [
            "Documents",
            "Desktop",
            "Downloads",
            "Pictures",
            "Movies",
            "Music",
            "src",
        ] {
            let target = tmp.join(name).join("proj").join("target");
            std::fs::create_dir_all(&target).unwrap();
            std::fs::write(target.join("CACHEDIR.TAG"), CACHEDIR_TAG_SIGNATURE).unwrap();
        }
        let walked = scan(&tmp, 4, &Budget::VERB);
        let found: Vec<String> = walked
            .targets
            .iter()
            .map(|t| {
                t.path
                    .strip_prefix(&tmp)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert!(
            found.iter().any(|p| p.starts_with("src/")),
            "an ordinary directory is still walked: {found:?}"
        );
        for name in [
            "Documents",
            "Desktop",
            "Downloads",
            "Pictures",
            "Movies",
            "Music",
        ] {
            assert!(
                !found.iter().any(|p| p.starts_with(name)),
                "{name} must never be descended into by the automatic pass: {found:?}"
            );
        }
        // Named directly, the same folder IS walked — the user asked.
        let named = scan(&tmp.join("Documents"), 4, &Budget::VERB);
        assert!(!named.targets.is_empty(), "a named root is deliberate");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
