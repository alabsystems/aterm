// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE DEVELOPER ROOT: `~/Developer`, the one folder checkouts and worktrees live in on an
//! owner's Mac (owner ruling 2026-10-09).
//!
//! Why one folder. Spotlight imports every text file it indexes, and on macOS 27.0 it
//! attaches a 1,024-byte `_kMDItemPrimaryTextEmbedding` to each (measured 2026-10-08 with
//! `mdls`); the source trees in `$HOME` were all indexed, and taking one tree out of the
//! index took `mds_stores` from 371% CPU to about 50% (the owner's measurement). Apple
//! documents ONE way to keep a folder out of the index: the Search Privacy list in
//! System Settings ▸ Spotlight, which "keeps listed items from being indexed at all". That
//! list is root-owned and has no API, so only the owner can add to it — once per Mac, for
//! one folder, if the code is in one folder. `~/Developer` is the folder Apple already
//! names for it (Xcode's default location for new projects).
//!
//! So this module is three things, and none of them assumes the gesture took:
//!
//! * [`survey`] — the git checkouts and linked worktrees under `$HOME` that are NOT under
//!   the root. Read-only: directory listings and `.git` files, no process spawned. The
//!   root is found by what it IS, never by its spelling ([`root_at`]): on a
//!   case-insensitive volume `~/developer` is the root. A root that is a symbolic link, or
//!   that resolves into a location macOS guards with a consent dialog
//!   ([`crate::protected`]), is never opened: the window's unattended look must not list,
//!   or plant a probe in, a removable or network volume in aterm's name.
//! * [`plan`] and [`execute`] — `aterm pkg devroot move`: what would move and what refuses
//!   it, then the move and its repairs. Explicit, never automatic. A repository moves
//!   WHOLE when it has submodules (their `.git` files and `core.worktree` are relative, and
//!   `git worktree repair` does not touch them), each repository is finished — renamed,
//!   repaired and checked with `git status` in every checkout it has — before the next is
//!   begun, and an interrupt waits for the run to end.
//! * [`spotlight`] — MEASURES whether Spotlight skips the root, with the probe and control
//!   of [`crate::noindex::verify`]; [`spotlight_row`] and [`devroot_row`] are what both
//!   doctors print.
//!
//! The move is a one-time migration per Mac. DELETION POINT: [`plan`], [`execute`],
//! [`Recorded`] and everything only they use (the repair table, the `move` verb,
//! [`crate::linkmode::relink`]) are removed once every owner Mac — m3, m111 and any other
//! machine on the release roster — reports a clean devroot survey (`aterm doctor`'s
//! `devroot:` row `ok`). The survey, the rows and the Spotlight measurement stay: they are
//! how a new checkout outside the root, or a macOS update that changes what Spotlight
//! skips, is noticed.

use std::path::{Path, PathBuf};

use crate::noindex::Verdict;

/// The root's name under `$HOME`.
pub const NAME: &str = "Developer";

/// `~/Developer`, as named (for the folder as it is on disk, [`root_at`]).
#[must_use]
pub fn root(home: &Path) -> PathBuf {
    home.join(NAME)
}

/// The publication engine's checkout when `$PUBLICATION_ENGINE` names none: `~/Developer/publication`
/// once it is there, else the pre-root `~/publication` while it still is, else the root's place
/// (what a missing engine is reported against). DELETION POINT: the `~/publication` arm goes with
/// the move ([`plan`], [`execute`]), once every owner Mac reports a clean survey.
#[must_use]
pub fn publication_engine(home: &Path) -> PathBuf {
    let under_root = root(home).join("publication");
    if under_root.is_dir() {
        return under_root;
    }
    let before_root = home.join("publication");
    if before_root.is_dir() {
        return before_root;
    }
    under_root
}

/// How deep the survey looks for a checkout below `$HOME` (and below the root):
/// `~/<a>/.git` and `~/<a>/<b>/.git` (a folder of checkouts such as `~/src/<repo>`).
pub const DEPTH: usize = 2;

/// The top-level names under `$HOME` the survey never opens besides the folders macOS
/// guards with a consent dialog ([`crate::protected::HOME_FOLDERS`]: a read there raises
/// that dialog in aterm's name). `Library` and `Applications` hold no checkouts and are
/// large; `Public` is the share drop box. Dot-names are skipped by rule: Spotlight
/// already skips a dot-folder (measured), and `~/.cargo`, `~/.rustup` and friends are
/// not checkouts anyone works in.
pub const SKIP_DIRS: &[&str] = &["Library", "Applications", "Public", "node_modules"];

/// Whether the survey leaves the top-level name `name` under `$HOME` unopened.
#[must_use]
pub fn skipped_name(name: &str) -> bool {
    name.starts_with('.')
        || SKIP_DIRS.contains(&name)
        || crate::protected::HOME_FOLDERS.contains(&name)
}

/// Where System Settings keeps the list the owner adds the root to, in words — what both
/// doctors and the window's entry name.
pub const SEARCH_PRIVACY: &str = "System Settings \u{25b8} Spotlight \u{25b8} Search Privacy";

/// `~/Developer` spelled for a person: `~/Developer`.
const ROOT_WORDS: &str = "~/Developer";

// ---------------------------------------------------------------------------------------
// The root, read without opening it
// ---------------------------------------------------------------------------------------

/// What stands at `~/Developer`, read with an `lstat` and, for a real folder, a
/// `realpath` — neither of which macOS's consent services gate ([`crate::protected`]), and
/// neither of which follows a link onto a stalled mount.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum RootIs {
    /// Nothing stands there yet.
    #[default]
    Missing,
    /// A real folder in the home folder.
    Folder,
    /// Something aterm does not open, and why, in words: a symbolic link (onto another
    /// volume, as often as not), a folder that resolves into a location macOS guards with
    /// a consent dialog, or a file.
    NotOpened(String),
}

/// The root as it is: its path as spelled ON DISK (`realpath` answers `~/developer` for
/// `~/Developer` on a case-insensitive volume where the owner made it so; a link keeps
/// its own name), and what it is.
#[must_use]
pub fn root_at(home: &Path) -> (PathBuf, RootIs) {
    let named = root(home);
    let Ok(meta) = std::fs::symlink_metadata(&named) else {
        return (named, RootIs::Missing);
    };
    if meta.file_type().is_symlink() {
        let to = std::fs::read_link(&named).map_or_else(
            |_| "a target that could not be read".to_string(),
            |t| t.display().to_string(),
        );
        let why = format!("{ROOT_WORDS} is a symbolic link to {to}, which aterm does not follow");
        return (named, RootIs::NotOpened(why));
    }
    if !meta.is_dir() {
        let why = format!("{ROOT_WORDS} is a file, not a folder");
        return (named, RootIs::NotOpened(why));
    }
    let real = std::fs::canonicalize(&named).unwrap_or(named);
    let home_real = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    if crate::protected::under_protected_root(&home_real, &real) {
        let why = format!(
            "{ROOT_WORDS} resolves to {}, a location macOS guards with a consent dialog",
            real.display()
        );
        return (real, RootIs::NotOpened(why));
    }
    (real, RootIs::Folder)
}

/// `(device, inode)` of the entry at `path`, a link NOT followed.
#[cfg(unix)]
fn lstat_id(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::symlink_metadata(path)
        .ok()
        .map(|m| (m.dev(), m.ino()))
}

#[cfg(not(unix))]
fn lstat_id(_path: &Path) -> Option<(u64, u64)> {
    None
}

/// The root's identity, for telling whether a path is under it whatever its spelling.
struct RootMark {
    path: PathBuf,
    id: Option<(u64, u64)>,
}

impl RootMark {
    fn of(root: &Path) -> Self {
        Self {
            path: root.to_path_buf(),
            id: lstat_id(root),
        }
    }

    /// Whether `path` is at or under the root: by its spelling, else by the identity of
    /// its first component under `home` — git records a worktree as it was typed, so
    /// `~/DEVELOPER/x` on a case-insensitive volume is under `~/Developer`. An `lstat` of
    /// a name in `$HOME`, never through a link.
    fn holds(&self, home: &Path, path: &Path) -> bool {
        if path.starts_with(&self.path) {
            return true;
        }
        let Some(top) = path
            .strip_prefix(home)
            .ok()
            .and_then(|rel| rel.components().next())
        else {
            return false;
        };
        self.id.is_some() && lstat_id(&home.join(top)) == self.id
    }
}

// ---------------------------------------------------------------------------------------
// The survey
// ---------------------------------------------------------------------------------------

/// One git checkout: a main working tree, or a linked worktree of one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkout {
    /// Where it is.
    pub path: PathBuf,
    /// The repository's common git directory: `<main>/.git`, or a bare repository.
    pub common: PathBuf,
    /// A linked worktree (`git worktree add`): its `.git` is a file naming
    /// `<common>/worktrees/<id>`.
    pub linked: bool,
}

/// What [`survey`] found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Survey {
    /// `$HOME`.
    pub home: PathBuf,
    /// The root, `~/Developer`, spelled as it is on disk ([`root_at`]).
    pub root: PathBuf,
    /// What stands there.
    pub root_is: RootIs,
    /// Checkouts under `$HOME` that are not under the root, by path.
    pub outside: Vec<Checkout>,
    /// Checkouts under the root, by path.
    pub inside: Vec<Checkout>,
}

impl Survey {
    /// Every checkout found, outside then inside.
    pub fn all(&self) -> impl Iterator<Item = &Checkout> {
        self.outside.iter().chain(&self.inside)
    }
}

/// Find the checkouts under `home` (depth [`DEPTH`], never into a dot-name, a
/// [`skipped_name`] or the root) and under the root, plus every linked worktree a found
/// repository registers under `home` that the walk did not reach (a worktree three levels
/// down is still a worktree outside the root). A worktree nested INSIDE a found checkout
/// (`~/aterm/.claude/worktrees/x`) is not listed on its own: it moves with that checkout.
/// A root aterm does not open ([`RootIs::NotOpened`]) is surveyed for nothing.
///
/// Read-only and spawns nothing. A symlink is never followed and never listed.
#[must_use]
pub fn survey(home: &Path) -> Survey {
    // The real spelling: git records worktree paths resolved (`/private/var/…` for
    // `/var/…`), and every comparison below is against what git wrote.
    let home = &std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    let (root, root_is) = root_at(home);
    let mark = RootMark::of(&root);
    let mut outside = Vec::new();
    find_checkouts(
        home,
        1,
        &mut |dir, depth| (depth == 1 && mark.holds(home, dir)) || skipped_top(dir, depth),
        &mut outside,
    );
    let mut inside = Vec::new();
    if root_is == RootIs::Folder {
        find_checkouts(&root, 1, &mut |_, _| false, &mut inside);
    }
    let read = |paths: Vec<PathBuf>| -> Vec<Checkout> {
        paths.into_iter().map(|path| checkout_at(&path)).collect()
    };
    let mut outside = read(outside);
    let mut inside = read(inside);
    // Registered worktrees the walk did not reach.
    let mut commons: Vec<PathBuf> = outside
        .iter()
        .chain(&inside)
        .map(|c| c.common.clone())
        .collect();
    commons.sort();
    commons.dedup();
    let listed: Vec<PathBuf> = outside
        .iter()
        .chain(&inside)
        .map(|c| c.path.clone())
        .collect();
    for common in &commons {
        // Each worktree it registers, and its main checkout (a worktree the walk found
        // may belong to a main deeper than it looks).
        let main = (common.file_name().is_some_and(|n| n == ".git"))
            .then(|| main_dir(common))
            .into_iter();
        for wt in registered_worktrees(common).into_iter().chain(main) {
            if listed.iter().any(|p| wt.starts_with(p)) {
                continue;
            }
            // Decided before anything stats the worktree: under a root aterm does not
            // open, it is never touched.
            if mark.holds(home, &wt) {
                if root_is == RootIs::Folder && wt.is_dir() {
                    inside.push(checkout_at(&wt));
                }
                continue;
            }
            if let Ok(rel) = wt.strip_prefix(home)
                && rel
                    .components()
                    .next()
                    .and_then(|c| c.as_os_str().to_str())
                    .is_some_and(|top| !skipped_name(top))
                && wt.is_dir()
            {
                outside.push(checkout_at(&wt));
            }
        }
    }
    outside.sort_by(|a, b| a.path.cmp(&b.path));
    outside.dedup_by(|a, b| a.path == b.path);
    inside.sort_by(|a, b| a.path.cmp(&b.path));
    inside.dedup_by(|a, b| a.path == b.path);
    Survey {
        home: home.to_path_buf(),
        root,
        root_is,
        outside,
        inside,
    }
}

/// A directory the home walk never opens: a dot-name anywhere, and at the top level a
/// [`skipped_name`].
fn skipped_top(dir: &Path, depth: usize) -> bool {
    let Some(name) = dir.file_name().and_then(|n| n.to_str()) else {
        return true;
    };
    name.starts_with('.') || (depth == 1 && skipped_name(name))
}

/// Walk `base` to [`DEPTH`] for directories holding a `.git`, never descending into a
/// checkout (its subdirectories are its own), a symlink, or a directory `skip` names.
fn find_checkouts(
    base: &Path,
    depth: usize,
    skip: &mut dyn FnMut(&Path, usize) -> bool,
    out: &mut Vec<PathBuf>,
) {
    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect();
    dirs.sort();
    for dir in dirs {
        if dir
            .file_name()
            .and_then(|n| n.to_str())
            .is_none_or(|n| n.starts_with('.'))
            || skip(&dir, depth)
        {
            continue;
        }
        if std::fs::symlink_metadata(dir.join(".git")).is_ok() {
            out.push(dir);
        } else if depth < DEPTH {
            find_checkouts(&dir, depth + 1, skip, out);
        }
    }
}

/// The git directory a `.git` FILE at `dotgit` names (`gitdir: <path>`, relative to the
/// checkout that holds it), or `None` when it is not such a file.
fn gitdir_named_by(dotgit: &Path) -> Option<PathBuf> {
    let checkout = dotgit.parent()?;
    read_small(dotgit).and_then(|text| {
        text.lines()
            .find_map(|l| l.strip_prefix("gitdir:"))
            .map(|g| resolve(checkout, g.trim()))
    })
}

/// Read `path`'s `.git`: a directory is a main checkout's own; a file names the git
/// directory, whose `commondir` (present for a linked worktree, absent for a submodule)
/// names the repository's common one.
fn checkout_at(path: &Path) -> Checkout {
    let dotgit = path.join(".git");
    let own = Checkout {
        path: path.to_path_buf(),
        common: dotgit.clone(),
        linked: false,
    };
    if std::fs::symlink_metadata(&dotgit).is_ok_and(|m| m.is_dir()) {
        return own;
    }
    let Some(gitdir) = gitdir_named_by(&dotgit) else {
        return own;
    };
    match read_small(&gitdir.join("commondir")) {
        Some(common) => Checkout {
            path: path.to_path_buf(),
            common: resolve(&gitdir, common.trim()),
            linked: true,
        },
        None => Checkout {
            path: path.to_path_buf(),
            common: gitdir,
            linked: false,
        },
    }
}

/// The linked worktrees `common` registers: `<common>/worktrees/<id>/gitdir` names each
/// one's `.git` file (absolute, or relative to its own directory under
/// `worktree.useRelativePaths`).
fn registered_worktrees(common: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(common.join("worktrees")) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .filter_map(|e| {
            let id = e.path();
            let named = read_small(&id.join("gitdir"))?;
            let dotgit = resolve(&id, named.trim());
            dotgit.parent().map(Path::to_path_buf)
        })
        .collect();
    out.sort();
    out
}

/// A small file's text (a `.git` file, `commondir`, `gitdir`): `None` when it is missing,
/// unreadable or over 4 KiB.
fn read_small(path: &Path) -> Option<String> {
    crate::metadata_io::read_bounded_regular_utf8(path, 4096).ok()
}

/// `named` read relative to `base` when it is not absolute, with `.`/`..` folded
/// lexically (`../..` from `<common>/worktrees/<id>` is `<common>`).
fn resolve(base: &Path, named: &str) -> PathBuf {
    let joined = if Path::new(named).is_absolute() {
        PathBuf::from(named)
    } else {
        base.join(named)
    };
    let mut out = PathBuf::new();
    for c in joined.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

// ---------------------------------------------------------------------------------------
// The doctors' rows
// ---------------------------------------------------------------------------------------

/// A doctor row's weight; each doctor spells it in its own marks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// Nothing to do.
    Ok,
    /// Something the owner should do; never a failure.
    Warn,
    /// A fact, or a measurement that could not be made.
    Note,
}

/// One doctor row: its weight and its words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// Ok, warn or note.
    pub level: Level,
    /// The sentence after the mark.
    pub text: String,
}

/// How many checkout paths a row names before it says `+N more`.
const NAMED: usize = 3;

/// The `devroot:` row: whether every checkout under `$HOME` lives under the root. It says
/// what to DO about Spotlight, never that the root is skipped: that is the `spotlight:`
/// row's to measure. A `warn` once the owner has made `~/Developer` (a checkout outside it
/// is then a straggler); before that, a `note` — a Mac that keeps its code elsewhere is
/// not unhealthy.
#[must_use]
pub fn devroot_row(survey: &Survey) -> Row {
    let n = survey.outside.len();
    if n == 0 {
        let inside = survey.inside.len();
        return Row {
            level: Level::Ok,
            text: match inside {
                0 => format!("no git checkout under ~ outside {ROOT_WORDS}"),
                1 => format!("the one git checkout under ~ is in {ROOT_WORDS}"),
                k => format!("all {k} git checkouts under ~ are in {ROOT_WORDS}"),
            },
        };
    }
    let mut names: Vec<String> = survey
        .outside
        .iter()
        .take(NAMED)
        .map(|c| tilde(&survey.home, &c.path))
        .collect();
    if n > NAMED {
        names.push(format!("+{} more", n - NAMED));
    }
    let head = format!(
        "{n} git checkout{} outside {ROOT_WORDS} ({})",
        if n == 1 { "" } else { "s" },
        names.join(", "),
    );
    match &survey.root_is {
        RootIs::NotOpened(why) => Row {
            level: Level::Note,
            text: format!(
                "{head} \u{2014} {why}, so `aterm pkg devroot move` has no folder to move them \
                 into"
            ),
        },
        is => Row {
            level: if *is == RootIs::Folder {
                Level::Warn
            } else {
                Level::Note
            },
            text: format!(
                "{head} \u{2014} {ROOT_WORDS} is the one folder to add to {SEARCH_PRIVACY} \
                 (`spotlight:` measures whether Spotlight skips it); now: `aterm pkg devroot` \
                 (what holds each), then `aterm pkg devroot move --all` with no agent running \
                 in them"
            ),
        },
    }
}

/// What the Spotlight measurement of the root found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Spotlight {
    /// There is no `~/Developer` yet: nothing to measure.
    NoRoot,
    /// The probe's verdict ([`crate::noindex::verify`]).
    Measured(Verdict),
    /// This report does not measure (a test binary plants nothing in a real home, a root
    /// aterm does not open); why.
    NotMeasured(String),
}

/// MEASURE whether Spotlight skips the root: a probe planted in it and a control planted
/// after it in a plain folder beside it ([`crate::noindex::verify`], up to
/// [`crate::noindex::Timing::worst_case`]). Both are removed whatever the answer. A root
/// aterm does not open ([`RootIs::NotOpened`]) is not measured: the probe would be planted
/// through a link, or where macOS asks the owner first.
#[must_use]
pub fn spotlight(home: &Path) -> Spotlight {
    if !crate::noindex::SUPPORTED {
        return Spotlight::Measured(Verdict::NotApplicable);
    }
    match root_at(home) {
        (_, RootIs::Missing) => Spotlight::NoRoot,
        (_, RootIs::NotOpened(why)) => Spotlight::NotMeasured(why),
        (root, RootIs::Folder) => Spotlight::Measured(crate::noindex::verify(
            &root,
            &crate::noindex::Timing::DEFAULT,
        )),
    }
}

/// The `spotlight:` row, decided from the measurement alone (never from the root's name
/// or a remembered answer). `None` where there is no Spotlight.
#[must_use]
pub fn spotlight_row(spotlight: &Spotlight) -> Option<Row> {
    let (level, text) = match spotlight {
        Spotlight::NoRoot => (
            Level::Note,
            format!("{ROOT_WORDS} does not exist yet, so there is nothing to measure"),
        ),
        Spotlight::NotMeasured(why) => (Level::Note, format!("not measured ({why})")),
        Spotlight::Measured(Verdict::NotApplicable) => return None,
        Spotlight::Measured(Verdict::Excluded) => (
            Level::Ok,
            format!(
                "{ROOT_WORDS} is skipped by Spotlight (measured: a probe stayed out of the \
                 index while a later control appeared)"
            ),
        ),
        Spotlight::Measured(Verdict::Indexed) => (
            Level::Warn,
            format!(
                "Spotlight indexes {ROOT_WORDS} (measured: the probe was found) — fix: add \
                 {ROOT_WORDS} to {SEARCH_PRIVACY}"
            ),
        ),
        Spotlight::Measured(unknown @ Verdict::Unknown(_)) => (Level::Note, unknown.to_string()),
    };
    Some(Row { level, text })
}

/// `path` spelled from `~` when it is under `home`.
fn tilde(home: &Path, path: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rel) if rel.as_os_str().is_empty() => "~".to_string(),
        Ok(rel) => format!("~/{}", rel.display()),
        Err(_) => path.display().to_string(),
    }
}

// ---------------------------------------------------------------------------------------
// The move — DELETION POINT: see the module header.
// ---------------------------------------------------------------------------------------

/// Which checkouts `aterm pkg devroot move` was asked to move.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    /// `--all`: every checkout [`survey`] found outside the root.
    All,
    /// The checkouts named, each one the survey found outside the root.
    Named(Vec<PathBuf>),
}

/// One directory the move renames into the root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Move {
    /// Where it is.
    pub from: PathBuf,
    /// Where it goes: the same path below the root that it had below `$HOME`.
    pub to: PathBuf,
    /// Its repository's common git directory, as it is now.
    pub common: PathBuf,
}

/// Why a worktree of a repository with submodules does not move with the rest of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Apart {
    /// `move <dir>...` did not name it.
    NotNamed,
    /// It is outside the home folder (an agent's `/private/tmp/claude-*` worktree).
    OutsideHome,
    /// It is under a top-level folder of `$HOME` the survey never opens (named).
    Skipped(String),
    /// It is already under the root.
    InRoot,
}

/// Why one checkout cannot move now. Every one is a refusal of the whole run, named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Blocker {
    /// `git status --porcelain` printed `rows` lines: work nobody committed.
    Dirty {
        /// How many rows.
        rows: usize,
    },
    /// A coding agent (Claude Code, Codex, Gemini) works in it: they keep state keyed by
    /// the path and rewrite it as they run.
    Agent {
        /// The process.
        pid: u32,
        /// Which agent.
        program: String,
        /// Where, when not in the checkout itself: another checkout of the same
        /// repository, or its git directory — both write into the git directory that
        /// moves.
        at: Option<PathBuf>,
    },
    /// A process uses it: its working directory, its executable or an open file.
    InUse {
        /// The process.
        pid: u32,
        /// Its executable's name.
        program: String,
        /// `cwd`, `exe` or `fd`.
        how: &'static str,
        /// Where, when not in the checkout itself ([`Blocker::Agent::at`]).
        at: Option<PathBuf>,
    },
    /// A git operation holds a lock in its git directory.
    GitLock(PathBuf),
    /// Something already stands at the destination.
    Destination(PathBuf),
    /// Claude Code keeps state for BOTH the old and the new path: renaming one onto the
    /// other would lose one of them.
    AgentStateClash(PathBuf),
    /// What it is could not be read (`git status` did not answer, the process table could
    /// not be listed): refused, never guessed.
    Unread(String),
    /// A checkout of the same repository that would not move with it, in a repository
    /// with submodules: their links are relative, so a repository split across two places
    /// loses them.
    Split {
        /// The checkout left behind.
        checkout: PathBuf,
        /// It is the repository's main checkout (so the worktree is the one to remake).
        main: bool,
        /// Why it would stay.
        apart: Apart,
    },
    /// The rename itself cannot happen (another volume, a folder that cannot be written);
    /// why.
    Unmovable(String),
}

impl std::fmt::Display for Blocker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let shared = |at: &Path| {
            format!(
                "{}, which shares this repository's git directory",
                at.display()
            )
        };
        match self {
            Self::Dirty { rows } => write!(
                f,
                "{rows} uncommitted change{} (`git status`) — commit or remove {}",
                if *rows == 1 { "" } else { "s" },
                if *rows == 1 { "it" } else { "them" }
            ),
            Self::Agent { pid, program, at } => match at {
                None => write!(
                    f,
                    "{program} (pid {pid}) works in it — quit that session first"
                ),
                Some(at) => write!(
                    f,
                    "{program} (pid {pid}) works in {} — quit that session first",
                    shared(at)
                ),
            },
            Self::InUse {
                pid,
                program,
                how,
                at,
            } => {
                let what = match *how {
                    "cwd" => "its working directory is",
                    "exe" => "it runs from",
                    _ => "it has a file open in",
                };
                let place = at.as_deref().map_or_else(|| "it".to_string(), shared);
                write!(
                    f,
                    "{program} (pid {pid}): {what} {place} — end it or leave the folder"
                )
            }
            Self::GitLock(lock) => write!(
                f,
                "{} exists — a git command is running, or one died; let it finish or remove \
                 the lock",
                lock.display()
            ),
            Self::Destination(to) => write!(f, "{} already exists", to.display()),
            Self::AgentStateClash(new) => write!(
                f,
                "Claude Code already keeps state at {} for the new path — merge or remove one",
                new.display()
            ),
            Self::Unread(why) => write!(f, "could not be read ({why})"),
            Self::Split {
                checkout,
                main,
                apart,
            } => {
                let (place, stays) = match apart {
                    Apart::NotNamed => ("is not named".to_string(), false),
                    Apart::OutsideHome => ("is outside your home folder".to_string(), true),
                    Apart::Skipped(top) => {
                        (format!("is in ~/{top}, which aterm never opens"), true)
                    }
                    Apart::InRoot => (format!("is already in {ROOT_WORDS}"), true),
                };
                let remedy = match (stays, main) {
                    (false, _) => "name it too, or use --all".to_string(),
                    // The main checkout stays: the worktree is the one to make again.
                    (true, true) => format!(
                        "make this worktree again under {ROOT_WORDS} instead — commit or push \
                         its work, delete it, run `git worktree prune` in {} and `git worktree \
                         add` it there",
                        checkout.display()
                    ),
                    (true, false) => "commit or push its work, delete it and run `git worktree \
                                      prune` here first"
                        .to_string(),
                };
                write!(
                    f,
                    "{}, {} of the same repository, {place} — a repository with submodules \
                     moves whole (their links are relative, and `git worktree repair` does not \
                     mend them): {remedy}",
                    checkout.display(),
                    if *main {
                        "the main checkout"
                    } else {
                        "a worktree"
                    }
                )
            }
            Self::Unmovable(why) => write!(f, "{why}"),
        }
    }
}

/// One of Claude Code's path-keyed project directories (`~/.claude/projects/<key>`) whose
/// key names a path the move renames.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentDir {
    /// The checkout being moved that the key's path is in.
    pub checkout: PathBuf,
    /// The directory under the old key.
    pub from: PathBuf,
    /// The same directory under the new path's key.
    pub to: PathBuf,
}

/// One `aterm pkg link` dev link whose checkout the move renames: its bin links and its
/// marker are laid again from the new place ([`crate::linkmode::relink`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relink {
    /// The linked program.
    pub program: String,
    /// The checkout its marker names now.
    pub from: PathBuf,
    /// Where that checkout will be.
    pub to: PathBuf,
}

/// What aterm's own tools record about a checkout's path OUTSIDE git, read for [`plan`]:
/// the `aterm pkg link` dev links (the program, and the checkout its marker names) and the
/// `[packages.links]` checkout rows of `aterm.toml` (the program, the value as written, and
/// the path it names). The first are repaired; the second is the owner's file, so the plan
/// says what to change in it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Recorded {
    /// `(program, checkout)` per dev link.
    pub dev_links: Vec<(String, PathBuf)>,
    /// `(program, value, path)` per `[packages.links]` checkout row.
    pub config_links: Vec<(String, String, PathBuf)>,
}

impl Recorded {
    /// Read them: the link markers of the store at `layout`, and `aterm.toml`.
    #[must_use]
    pub fn read(layout: Option<&crate::store::Layout>, home: &Path) -> Self {
        let dev_links = layout.map_or_else(Vec::new, |layout| {
            crate::linkmode::linked_programs(layout)
                .into_iter()
                .filter_map(|program| {
                    let checkout = crate::linkmode::linked_checkout(layout, &program)?;
                    Some((program, checkout))
                })
                .collect()
        });
        let config_links = crate::config::load()
            .links
            .into_iter()
            .filter_map(
                |(program, value)| match crate::config::classify_link(&value, Some(home)) {
                    crate::config::LinkTarget::Checkout(path) => Some((program, value, path)),
                    _ => None,
                },
            )
            .collect();
        Self {
            dev_links,
            config_links,
        }
    }
}

/// What `aterm pkg devroot move` would do, and what refuses it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    /// The renames, by `from`.
    pub moves: Vec<Move>,
    /// Each checkout that cannot move now, and why.
    pub blocked: Vec<(PathBuf, Blocker)>,
    /// `~/.claude/projects/<key>` directories whose key names a moved path.
    pub agent_dirs: Vec<AgentDir>,
    /// `aterm pkg link` dev links whose checkout moves.
    pub relinks: Vec<Relink>,
    /// What the move cannot fix and the owner should expect (an x.py/LLVM build tree that
    /// recorded absolute paths reconfigures; an `aterm.toml` row to edit).
    pub notes: Vec<String>,
    /// Processes the scan could not read (platform agents refuse a same-uid reader);
    /// counted, never a blocker.
    pub unreadable: usize,
}

/// The processes using any of `dirs` (each canonical) — the live scan is
/// [`aterm_uds::holders::scan_path_users`] skipping this process; a test names its own.
pub type Holders<'a> = &'a dyn Fn(&[PathBuf]) -> std::io::Result<aterm_uds::holders::PathScan>;

/// The live [`Holders`]: every process of this uid but this one.
///
/// # Errors
///
/// The process listing failing.
pub fn live_holders(dirs: &[PathBuf]) -> std::io::Result<aterm_uds::holders::PathScan> {
    aterm_uds::holders::scan_path_users(dirs, &[std::process::id()])
}

/// The moves grouped by repository — `(common git directory, indices into moves)` — in
/// the order each repository first appears.
fn families(moves: &[Move]) -> Vec<(PathBuf, Vec<usize>)> {
    let mut out: Vec<(PathBuf, Vec<usize>)> = Vec::new();
    for (i, m) in moves.iter().enumerate() {
        match out.iter_mut().find(|(common, _)| *common == m.common) {
            Some((_, idx)) => idx.push(i),
            None => out.push((m.common.clone(), vec![i])),
        }
    }
    out
}

/// Every checkout of the repository whose common git directory is `common`, as git
/// registers them: its main checkout (none for a bare repository) and each linked
/// worktree that is there — a worktree nested inside another of them left out (it goes
/// where that one goes).
fn members(common: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if common.file_name().is_some_and(|n| n == ".git")
        && let Some(main) = common.parent()
        && main.is_dir()
    {
        out.push(main.to_path_buf());
    }
    out.extend(
        registered_worktrees(common)
            .into_iter()
            .filter(|wt| wt.is_dir()),
    );
    out.sort();
    out.dedup();
    let all = out.clone();
    out.retain(|p| !all.iter().any(|q| q != p && p.starts_with(q)));
    out
}

/// Whether the repository at `common` has submodules: a submodule git directory under it
/// (`modules/`, its own or a worktree's), or a `.gitmodules` in one of its checkouts.
fn has_submodules(common: &Path, members: &[PathBuf]) -> bool {
    if common.join("modules").is_dir() {
        return true;
    }
    let admin = std::fs::read_dir(common.join("worktrees"))
        .map(|entries| entries.flatten().any(|e| e.path().join("modules").is_dir()))
        .unwrap_or(false);
    admin || members.iter().any(|m| m.join(".gitmodules").is_file())
}

/// `p` at its new place when it is at or under one of `moves`' `from` (never with a
/// trailing `/`: `join("")` would add one, and a config value is compared byte for byte).
fn map_through(moves: &[Move], p: &Path) -> Option<PathBuf> {
    moves.iter().find_map(|m| {
        let rel = p.strip_prefix(&m.from).ok()?;
        Some(if rel.as_os_str().is_empty() {
            m.to.clone()
        } else {
            m.to.join(rel)
        })
    })
}

/// The device of the entry at `path`, a link not followed.
fn lstat_dev(path: &Path) -> Option<u64> {
    lstat_id(path).map(|(dev, _)| dev)
}

/// Why the rename of `m` cannot happen, checked before anything moves so a run never
/// stops halfway through a repository: `from` on another volume than the root's home
/// (`rename(2)` fails with `EXDEV`), a folder it leaves that cannot be written, or a
/// destination folder that cannot be made or written.
fn unmovable(home: &Path, m: &Move) -> Option<String> {
    let home_dev = lstat_dev(home);
    if lstat_dev(&m.from) != home_dev {
        return Some(format!(
            "{} is on another volume than your home folder, and a move between volumes is a \
             copy — move it by hand",
            m.from.display()
        ));
    }
    if let Some(parent) = m.from.parent()
        && !crate::platform::dir_writable_by_caller(parent)
    {
        return Some(format!(
            "{} cannot be written, so nothing can be moved out of it",
            parent.display()
        ));
    }
    // The nearest folder that already stands on the way to the destination: the rename
    // lands in it, or in folders made in it.
    let mut at = m.to.parent();
    while let Some(dir) = at {
        if std::fs::symlink_metadata(dir).is_ok() {
            if !dir.is_dir() {
                return Some(format!("{} is not a folder", dir.display()));
            }
            if lstat_dev(dir) != home_dev {
                return Some(format!(
                    "{} is on another volume than your home folder, and a move between \
                     volumes is a copy",
                    dir.display()
                ));
            }
            if !crate::platform::dir_writable_by_caller(dir) {
                return Some(format!("{} cannot be written", dir.display()));
            }
            return None;
        }
        at = dir.parent();
    }
    None
}

/// Why `member`, a checkout of a repository being moved, would stay where it is.
fn apart(survey: &Survey, mark: &RootMark, member: &Path) -> Apart {
    let Ok(rel) = member.strip_prefix(&survey.home) else {
        return Apart::OutsideHome;
    };
    if mark.holds(&survey.home, member) {
        return Apart::InRoot;
    }
    match rel.components().next().and_then(|c| c.as_os_str().to_str()) {
        Some(top) if skipped_name(top) => Apart::Skipped(top.to_string()),
        _ => Apart::NotNamed,
    }
}

/// Plan the move of `selection` out of `survey`. Never touches anything: it reads
/// `git status`, the git locks, the destinations, the process table (`holders`),
/// `~/.claude/projects` and what `recorded` says aterm's tools hold.
///
/// # Errors
///
/// A named directory that is not a checkout the survey found outside the root, or a root
/// aterm does not open ([`RootIs::NotOpened`]).
pub fn plan(
    survey: &Survey,
    selection: &Selection,
    holders: Holders<'_>,
    recorded: &Recorded,
) -> Result<Plan, String> {
    if let RootIs::NotOpened(why) = &survey.root_is {
        return Err(format!(
            "{why}: the move renames checkouts into a {ROOT_WORDS} folder in your home folder, \
             so it moves nothing"
        ));
    }
    let chosen: Vec<&Checkout> = match selection {
        Selection::All => survey.outside.iter().collect(),
        Selection::Named(dirs) => {
            let mut chosen = Vec::new();
            for dir in dirs {
                let real = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.clone());
                let Some(c) = survey
                    .outside
                    .iter()
                    .find(|c| c.path == real || c.path == *dir)
                else {
                    return Err(format!(
                        "{} is not a git checkout outside {} (`aterm pkg devroot` lists them)",
                        dir.display(),
                        tilde(&survey.home, &survey.root)
                    ));
                };
                if !chosen.contains(&c) {
                    chosen.push(c);
                }
            }
            chosen
        }
    };
    let home = &survey.home;
    let mark = RootMark::of(&survey.root);
    let mut plan = Plan::default();
    for c in &chosen {
        let Ok(rel) = c.path.strip_prefix(home) else {
            continue;
        };
        plan.moves.push(Move {
            from: c.path.clone(),
            to: survey.root.join(rel),
            common: c.common.clone(),
        });
    }
    let families = families(&plan.moves);
    // A repository with submodules moves whole: every checkout it registers moves in this
    // run, or nothing of it does.
    let mut family_dirs: Vec<(PathBuf, PathBuf)> = Vec::new();
    for (common, idx) in &families {
        let first = plan.moves[idx[0]].from.clone();
        let members = members(common);
        if has_submodules(common, &members) {
            for member in &members {
                if idx.iter().any(|&i| member.starts_with(&plan.moves[i].from)) {
                    continue;
                }
                plan.blocked.push((
                    first.clone(),
                    Blocker::Split {
                        checkout: member.clone(),
                        main: *member == main_dir(common),
                        apart: apart(survey, &mark, member),
                    },
                ));
            }
        }
        family_dirs.extend(members.into_iter().map(|m| (m, first.clone())));
        family_dirs.push((common.clone(), first));
    }
    plan.agent_dirs = agent_dirs(home, &plan.moves);
    // Each check, per move: the rename itself, the destination, the git locks, `git status`.
    let statuses = statuses(&plan.moves);
    for (m, status) in plan.moves.iter().zip(statuses) {
        if let Some(why) = unmovable(home, m) {
            plan.blocked.push((m.from.clone(), Blocker::Unmovable(why)));
        }
        if std::fs::symlink_metadata(&m.to).is_ok() {
            plan.blocked
                .push((m.from.clone(), Blocker::Destination(m.to.clone())));
        }
        if let Some(lock) = git_lock(&m.from) {
            plan.blocked.push((m.from.clone(), Blocker::GitLock(lock)));
        }
        match status {
            Ok(0) => {}
            Ok(rows) => plan.blocked.push((m.from.clone(), Blocker::Dirty { rows })),
            Err(why) => plan.blocked.push((m.from.clone(), Blocker::Unread(why))),
        }
        if let Some(note) = rebuild_note(home, &m.from) {
            plan.notes.push(note);
        }
        if m.from
            .file_name()
            .is_some_and(|n| n.to_string_lossy().ends_with(".noindex"))
        {
            plan.notes.push(format!(
                "{}: a tree aterm's gate or release cutter made beside its checkout, which it \
                 makes again — if anything holds it, delete it (then `git worktree prune` in \
                 its repository) rather than free it",
                tilde(home, &m.from)
            ));
        }
    }
    for dir in &plan.agent_dirs {
        if std::fs::symlink_metadata(&dir.to).is_ok() {
            plan.blocked.push((
                dir.checkout.clone(),
                Blocker::AgentStateClash(dir.to.clone()),
            ));
        }
    }
    // What aterm's tools record outside git.
    for (program, checkout) in &recorded.dev_links {
        let real = std::fs::canonicalize(checkout).unwrap_or_else(|_| checkout.clone());
        if let Some(to) =
            map_through(&plan.moves, &real).or_else(|| map_through(&plan.moves, checkout))
        {
            plan.relinks.push(Relink {
                program: program.clone(),
                from: checkout.clone(),
                to,
            });
        }
    }
    for (program, value, path) in &recorded.config_links {
        if let Some(to) = map_through(&plan.moves, path) {
            plan.notes.push(format!(
                "aterm.toml's [packages.links] {program} = \"{value}\" names the old place — \
                 change it to \"{}\" after the move (until then `aterm pkg update` leaves the \
                 moved link alone, and says so)",
                tilde(home, &to)
            ));
        }
    }
    // Who uses them: one scan over every directory the run renames, every other checkout
    // and the git directory of each repository it moves (a process there writes into the
    // git directory that moves), and the agent folders.
    let mut dirs: Vec<PathBuf> = plan.moves.iter().map(|m| m.from.clone()).collect();
    dirs.extend(family_dirs.iter().map(|(d, _)| d.clone()));
    dirs.extend(plan.agent_dirs.iter().map(|d| d.from.clone()));
    dirs.sort();
    dirs.dedup();
    if !dirs.is_empty() {
        match holders(&dirs) {
            Ok(scan) => {
                plan.unreadable = scan.unreadable.len();
                for user in scan.users {
                    // The kernel may spell a data-volume path through its firmlink.
                    let used = user
                        .path
                        .strip_prefix("/System/Volumes/Data")
                        .map_or_else(|_| user.path.clone(), |rel| Path::new("/").join(rel));
                    let (owner, at) =
                        if let Some(m) = plan.moves.iter().find(|m| used.starts_with(&m.from)) {
                            (m.from.clone(), None)
                        } else if let Some((dir, first)) =
                            family_dirs.iter().find(|(d, _)| used.starts_with(d))
                        {
                            (first.clone(), Some(dir.clone()))
                        } else if let Some(d) =
                            plan.agent_dirs.iter().find(|d| used.starts_with(&d.from))
                        {
                            (d.checkout.clone(), None)
                        } else {
                            (used.clone(), None)
                        };
                    let (agent, program) = process_name(user.pid);
                    plan.blocked.push((
                        owner,
                        if agent {
                            Blocker::Agent {
                                pid: user.pid,
                                program,
                                at,
                            }
                        } else {
                            Blocker::InUse {
                                pid: user.pid,
                                program,
                                how: user.how,
                                at,
                            }
                        },
                    ));
                }
            }
            Err(e) => {
                for m in &plan.moves {
                    plan.blocked.push((
                        m.from.clone(),
                        Blocker::Unread(format!("the running processes could not be read: {e}")),
                    ));
                }
            }
        }
    }
    plan.blocked.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(plan)
}

/// The names an agent's executable, exec path or `argv[0..2]` carries as a path
/// component: Claude Code (its store build, the native installer's `claude/versions/<v>`,
/// npm's `@anthropic-ai/claude-code`), Codex and Gemini.
const AGENTS: &[&str] = &["claude", "claude-code", "codex", "gemini"];

/// Whether `pid` is a coding agent, and the name to show for it.
fn process_name(pid: u32) -> (bool, String) {
    let exe = match aterm_uds::holders::exe_of(pid) {
        aterm_uds::holders::Exe::At(p) => Some(p),
        _ => None,
    };
    let args = crate::caller_shell::process_args(pid);
    let mut candidates: Vec<String> = Vec::new();
    if let Some(exe) = &exe {
        candidates.push(exe.display().to_string());
    }
    if let Some(args) = &args {
        candidates.push(args.exec_path.clone());
        candidates.extend(args.argv.iter().take(2).cloned());
    }
    let agent = candidates.iter().find_map(|c| {
        Path::new(c).components().find_map(|part| {
            let s = part.as_os_str().to_str()?;
            AGENTS
                .iter()
                .find(|a| s.eq_ignore_ascii_case(a))
                .map(|a| if *a == "claude-code" { "claude" } else { a })
        })
    });
    if let Some(agent) = agent {
        return (true, agent.to_string());
    }
    let shown = args
        .as_ref()
        .and_then(|a| a.argv.first().cloned())
        .or_else(|| exe.as_ref().map(|p| p.display().to_string()))
        .map_or_else(
            || "a process".to_string(),
            |p| {
                Path::new(&p)
                    .file_name()
                    .map_or(p.clone(), |n| n.to_string_lossy().into_owned())
            },
        );
    (false, shown)
}

/// The `git status --porcelain` row count of each move's checkout ([`status_of`]).
fn statuses(moves: &[Move]) -> Vec<Result<usize, String>> {
    let dirs: Vec<PathBuf> = moves.iter().map(|m| m.from.clone()).collect();
    status_of(&dirs)
}

/// The `git status --porcelain` row count in each of `dirs`, eight at a time (a large
/// tree's status takes seconds); `Err` when git did not answer, or failed.
fn status_of(dirs: &[PathBuf]) -> Vec<Result<usize, String>> {
    let mut out: Vec<Result<usize, String>> = Vec::with_capacity(dirs.len());
    for chunk in dirs.chunks(8) {
        std::thread::scope(|s| {
            let handles: Vec<_> = chunk
                .iter()
                .map(|dir| s.spawn(move || status_rows(dir)))
                .collect();
            for h in handles {
                out.push(
                    h.join()
                        .unwrap_or_else(|_| Err("the status reader panicked".to_string())),
                );
            }
        });
    }
    out
}

/// How long one `git status` of one checkout may take before the checkout is refused as
/// unread: a large tree with submodules answers in seconds; a minute is a hang.
const STATUS_WITHIN: std::time::Duration = std::time::Duration::from_secs(60);

/// `git status --porcelain` in `dir`: its row count.
fn status_rows(dir: &Path) -> Result<usize, String> {
    let out = git_output(dir, &["status", "--porcelain"], STATUS_WITHIN)
        .ok_or_else(|| "`git status` did not answer within a minute".to_string())?;
    if !out.status.success() {
        return Err(format!(
            "`git status` failed: {}",
            String::from_utf8_lossy(&out.stderr)
                .lines()
                .next()
                .unwrap_or("no message")
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).lines().count())
}

/// `git -C <dir> <args>` with no optional locks and no fsmonitor daemon (a read must not
/// start a watcher that outlives it), none of the caller's `GIT_*` overrides, and in a
/// process group of its own — the ^C typed at the terminal is the move's to hold
/// ([`execute`]), never a half-written git config's — within `within`; stdout and stderr
/// captured. `None` when git could not be run or did not finish.
fn git_output(
    dir: &Path,
    args: &[&str],
    within: std::time::Duration,
) -> Option<std::process::Output> {
    let mut cmd = aterm_uds::spawnfd::command("git");
    cmd.arg("--no-optional-locks")
        .arg("-C")
        .arg(dir)
        .args(["-c", "core.fsmonitor=false"])
        .args(args);
    #[cfg(unix)]
    cmd.process_group(0);
    for var in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
    ] {
        cmd.env_remove(var);
    }
    crate::doctor::output_bounded_capturing(&mut cmd, within)
}

/// A git lock in `dir`'s own git directory (`index.lock`, `HEAD.lock`), if one stands.
fn git_lock(dir: &Path) -> Option<PathBuf> {
    let c = checkout_at(dir);
    let gitdir = if c.linked {
        gitdir_named_by(&dir.join(".git"))?
    } else {
        c.common
    };
    ["index.lock", "HEAD.lock"]
        .iter()
        .map(|name| gitdir.join(name))
        .find(|lock| std::fs::symlink_metadata(lock).is_ok())
}

/// The note for an x.py/LLVM build tree under `dir/build`: CMake records absolute paths,
/// so a moved tree reconfigures (hours of CPU for LLVM). `None` when there is none.
fn rebuild_note(home: &Path, dir: &Path) -> Option<String> {
    fn find(dir: &Path, depth: usize) -> bool {
        if dir.join("CMakeCache.txt").is_file() {
            return true;
        }
        depth > 0
            && std::fs::read_dir(dir).is_ok_and(|entries| {
                entries
                    .flatten()
                    .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                    .any(|e| find(&e.path(), depth - 1))
            })
    }
    let build = dir.join("build");
    (build.is_dir() && find(&build, 4)).then(|| {
        format!(
            "{}: its CMake build tree recorded absolute paths, so x.py/LLVM reconfigures \
             (a long rebuild) after the move",
            tilde(home, dir)
        )
    })
}

/// Claude Code's key for a project path: every character that is not an ASCII letter or
/// digit becomes `-`, one per UTF-16 unit (`/Users//u/Developer/aterm` →
/// `-Users-u-Developer-aterm`).
#[must_use]
pub fn claude_key(path: &Path) -> String {
    let mut key = String::new();
    for c in path.to_string_lossy().chars() {
        if c.is_ascii_alphanumeric() {
            key.push(c);
        } else {
            key.extend(std::iter::repeat_n('-', c.len_utf16()));
        }
    }
    key
}

/// `~/.claude/projects/<key>` renames for `moves`: each moved path's own key, and the key
/// of every subdirectory a session was started in, found by walking the checkout guided by
/// the key's remainder (a key is lossy, so `-Users-u-aterm-wa` is a subdirectory `wa` of
/// `~/aterm` only when `~/aterm/wa` exists; otherwise it is another checkout's, and left).
/// A key that is exactly one moved path's is that path's, never another's subdirectory.
fn agent_dirs(home: &Path, moves: &[Move]) -> Vec<AgentDir> {
    let projects = home.join(".claude").join("projects");
    let Ok(entries) = std::fs::read_dir(&projects) else {
        return Vec::new();
    };
    let mut keys: Vec<String> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| e.file_name().to_str().map(str::to_owned))
        .collect();
    keys.sort();
    let mut out = Vec::new();
    let mut taken: Vec<String> = Vec::new();
    for m in moves {
        let old = claude_key(&m.from);
        if keys.contains(&old) {
            out.push(AgentDir {
                checkout: m.from.clone(),
                from: projects.join(&old),
                to: projects.join(claude_key(&m.to)),
            });
            taken.push(old);
        }
    }
    for m in moves {
        let old = claude_key(&m.from);
        for key in keys.iter().filter(|k| !taken.contains(k)) {
            let Some(rest) = key.strip_prefix(&old).and_then(|r| r.strip_prefix('-')) else {
                continue;
            };
            if let Some(sub) = subdir_for(&m.from, rest, 8) {
                out.push(AgentDir {
                    checkout: m.from.clone(),
                    from: projects.join(key),
                    to: projects.join(claude_key(&m.to.join(sub))),
                });
            }
        }
    }
    out.sort_by(|a, b| a.from.cmp(&b.from));
    out.dedup_by(|a, b| a.from == b.from);
    out
}

/// The subdirectory of `dir` whose key is `rest` (the part of a project key after `dir`'s
/// own), walked by the key itself: only entries whose encoded name begins `rest` are
/// opened, to `depth`.
fn subdir_for(dir: &Path, rest: &str, depth: usize) -> Option<PathBuf> {
    if depth == 0 {
        return None;
    }
    let mut names: Vec<std::ffi::OsString> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.file_name())
        .collect();
    names.sort();
    for name in names {
        let k = claude_key(Path::new(&name));
        if rest == k {
            return Some(PathBuf::from(name));
        }
        if let Some(deeper) = rest.strip_prefix(&k).and_then(|r| r.strip_prefix('-'))
            && let Some(found) = subdir_for(&dir.join(&name), deeper, depth - 1)
        {
            return Some(PathBuf::from(name).join(found));
        }
    }
    None
}

/// What [`execute`] did, line by line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// One line per thing done.
    pub done: Vec<String>,
    /// One line per thing that did not land. Non-empty is exit 1.
    pub failed: Vec<String>,
    /// An interrupt (SIGINT, SIGTERM or SIGHUP) that arrived during the run and was held
    /// until it ended: the caller says so, then ends by it ([`deliver`]).
    pub held_signal: Option<i32>,
}

/// Move `plan` (which must have no blocker) and repair what names the old paths, ONE
/// REPOSITORY AT A TIME — each finished before the next is begun, so a run that stops
/// leaves every repository either where it was or moved and mended:
///
/// 1. its checkouts are renamed into the root (one `rename(2)` each on the home volume:
///    nothing is copied) — all of them, or, when one rename fails, none (the ones done are
///    put back, and the run stops);
/// 2. `git worktree repair` from its main checkout, naming every registered worktree whose
///    path moved — both directions of every link;
/// 3. the git config values (`config`, `config.worktree`) and `objects/info/alternates`
///    lines that name a moved path, in every git directory of every surveyed repository:
///    its common one, each worktree's, and each submodule's wherever it lives
///    (`<common>/modules/**`, `<common>/worktrees/*/modules/**`, or out of tree, as the
///    gate snapshot's `.aterm-verify/modules`) — a cut clone's
///    `aterm-release.receiptsFrom`, `core.hooksPath`, a remote that is a local clone, a
///    `--reference` alternate;
/// 4. Claude Code's `~/.claude/projects/<key>` directories, renamed to the new key —
///    never the transcripts' contents;
/// 5. the `aterm pkg link` dev links into it, laid again from the new place (`layout`,
///    the store, which the caller holds locked);
/// 6. the publication engine's registry rows (`<root>/publication/registry/repos.toml`
///    `path = "~/<name>"`) that name a moved checkout;
/// 7. the check: `git status` answers in every checkout of the repository, and of every
///    repository step 3 rewrote — anything else is a failure, named.
///
/// SIGINT, SIGTERM and SIGHUP are held for the whole run ([`Report::held_signal`]), and
/// git runs in its own process group, so a ^C can never stop a repository halfway.
#[must_use]
pub fn execute(survey: &Survey, plan: &Plan, layout: Option<&crate::store::Layout>) -> Report {
    debug_assert!(
        plan.blocked.is_empty(),
        "execute is never handed a blocked plan"
    );
    let (mut report, held) = signals::holding(|| run(survey, plan, layout));
    report.held_signal = held;
    report
}

/// [`execute`]'s run, with the interrupts held.
fn run(survey: &Survey, plan: &Plan, layout: Option<&crate::store::Layout>) -> Report {
    let home = &survey.home;
    let mut report = Report::default();
    // Every git directory of every surveyed repository, with that repository's common
    // directory, by the paths they have before the run.
    let gitdirs = gitdirs_of(survey);
    let mut moved: Vec<Move> = Vec::new();
    for (common, idx) in families(&plan.moves) {
        let family: Vec<&Move> = idx.iter().map(|&i| &plan.moves[i]).collect();
        let here = |p: &Path| map_through(&moved, p).unwrap_or_else(|| p.to_path_buf());
        // Before anything of it moves: every checkout of this repository, and of each
        // repository whose git files name one of its paths, and whether each answers
        // `git status` NOW — so the check after it judges the move, never a checkout that
        // was broken before it (a stale gate snapshot).
        let owned: Vec<Move> = family.iter().map(|m| (*m).clone()).collect();
        let mut touched: Vec<PathBuf> = vec![here(&common)];
        touched.extend(
            gitdirs
                .iter()
                .filter(|(gitdir, _)| mentions_any(&here(gitdir), &owned))
                .map(|(_, owner)| here(owner)),
        );
        touched.sort();
        touched.dedup();
        let before = census(&touched);
        // 1. The renames: all or none.
        if let Err(why) = rename_all(home, &family, &mut report) {
            report.failed.push(why);
            break;
        }
        moved.extend(family.iter().map(|m| (*m).clone()));
        let map = |p: &Path| map_through(&moved, p);
        let now = map(&common).unwrap_or_else(|| common.clone());
        // 2. Worktree links.
        repair_worktrees(home, &now, &map, &mut report);
        // 3. Paths recorded in git's own files, in every repository.
        for (gitdir, _) in &gitdirs {
            let at = map(gitdir).unwrap_or_else(|| gitdir.clone());
            repair_git_files(home, &at, &moved, &mut report);
        }
        // 4. Claude Code's path-keyed project directories, for the checkouts that moved.
        for dir in &plan.agent_dirs {
            if !family.iter().any(|m| m.from == dir.checkout) {
                continue;
            }
            match std::fs::rename(&dir.from, &dir.to) {
                Ok(()) => report.done.push(format!(
                    "renamed {} -> {}",
                    tilde(home, &dir.from),
                    tilde(home, &dir.to)
                )),
                Err(e) => report
                    .failed
                    .push(format!("could not rename {} ({e})", tilde(home, &dir.from))),
            }
        }
        // 5. Dev links into the checkouts that moved.
        for relink in &plan.relinks {
            if !family.iter().any(|m| relink.to.starts_with(&m.to)) {
                continue;
            }
            let done = layout.map_or_else(
                || Err("no package store to re-point it in".to_string()),
                |layout| {
                    crate::linkmode::relink(layout, &relink.program, &relink.to)
                        .map_err(|e| e.to_string())
                },
            );
            match done {
                Ok(_) => report.done.push(format!(
                    "re-laid the dev link of {} from {}",
                    relink.program,
                    tilde(home, &relink.to)
                )),
                Err(e) => report.failed.push(format!(
                    "the dev link of {} still names {} ({e}) — `aterm pkg unlink {}`, then \
                     `aterm pkg link {} {}`",
                    relink.program,
                    tilde(home, &relink.from),
                    relink.program,
                    relink.program,
                    tilde(home, &relink.to)
                )),
            }
        }
        // 6. The publication engine's registry.
        repair_registry(survey, &map, &mut report);
        // 7. Every checkout that answered `git status` before still does, where it is now.
        check_whole(home, &before, &map, &mut report);
    }
    report
}

/// Whether one of `gitdir`'s files that step 3 rewrites (`config`, `config.worktree`,
/// `objects/info/alternates`) so much as mentions a path `moves` renames.
fn mentions_any(gitdir: &Path, moves: &[Move]) -> bool {
    [
        gitdir.join("config"),
        gitdir.join("config.worktree"),
        gitdir.join("objects").join("info").join("alternates"),
    ]
    .iter()
    .filter_map(|f| std::fs::read_to_string(f).ok())
    .any(|text| {
        moves
            .iter()
            .any(|m| text.contains(m.from.to_string_lossy().as_ref()))
    })
}

/// Every checkout of each repository in `commons` (by common git directory), and
/// whether it answers `git status` now.
fn census(commons: &[PathBuf]) -> Vec<(PathBuf, Result<usize, String>)> {
    let mut checkouts: Vec<PathBuf> = commons.iter().flat_map(|c| members(c)).collect();
    checkouts.sort();
    checkouts.dedup();
    let answers = status_of(&checkouts);
    checkouts.into_iter().zip(answers).collect()
}

/// Rename every move of one repository, or none: when one fails, the ones already done
/// are put back, newest first. `Err` says what failed (and anything that could not be put
/// back); nothing after it is moved.
fn rename_all(home: &Path, family: &[&Move], report: &mut Report) -> Result<(), String> {
    let mut done: Vec<&Move> = Vec::new();
    let mut failure: Option<String> = None;
    for m in family {
        if let Some(parent) = m.to.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            failure = Some(format!("could not create {} ({e})", parent.display()));
            break;
        }
        match std::fs::rename(&m.from, &m.to) {
            Ok(()) => done.push(m),
            Err(e) => {
                failure = Some(format!("could not move {} ({e})", tilde(home, &m.from)));
                break;
            }
        }
    }
    let Some(failure) = failure else {
        for m in done {
            report.done.push(format!(
                "moved {} -> {}",
                tilde(home, &m.from),
                tilde(home, &m.to)
            ));
        }
        return Ok(());
    };
    let mut stuck: Vec<String> = Vec::new();
    for m in done.iter().rev() {
        if let Err(e) = std::fs::rename(&m.to, &m.from) {
            stuck.push(format!(
                "{} could not be put back from {} ({e})",
                tilde(home, &m.from),
                tilde(home, &m.to)
            ));
        }
    }
    Err(if stuck.is_empty() {
        format!("{failure}; its repository was left where it was, and nothing after it moved")
    } else {
        format!(
            "{failure}; nothing after it moved, and its repository is SPLIT: {}",
            stuck.join("; ")
        )
    })
}

/// Step 2: `git worktree repair` from the main checkout of the repository whose common
/// git directory is now `now`, naming each registered worktree that moved (its registry
/// still names the old paths). Nothing to do for a repository with no linked worktree.
fn repair_worktrees(
    home: &Path,
    now: &Path,
    map: &dyn Fn(&Path) -> Option<PathBuf>,
    report: &mut Report,
) {
    let worktrees = registered_worktrees(now);
    if worktrees.is_empty() {
        return;
    }
    let moved_wts: Vec<PathBuf> = worktrees.iter().filter_map(|w| map(w)).collect();
    let main = main_dir(now);
    let mut args: Vec<String> = vec!["worktree".into(), "repair".into()];
    args.extend(moved_wts.iter().map(|p| p.display().to_string()));
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    match git_output(&main, &argv, STATUS_WITHIN) {
        Some(out) if out.status.success() => report.done.push(format!(
            "repaired the worktree links of {}",
            tilde(home, &main)
        )),
        other => report.failed.push(format!(
            "{}: `git worktree repair` failed ({}) — run it there by hand, naming each moved \
             worktree",
            tilde(home, &main),
            other.map_or_else(
                || "it did not answer".to_string(),
                |o| String::from_utf8_lossy(&o.stderr).trim().to_string()
            )
        )),
    }
}

/// Step 7: `git status` again in every checkout [`census`] read before the renames, at
/// its new place. One that answered then and not now is a FAILURE, named; one that did not
/// answer then either is said as that, never laid at the move's door.
fn check_whole(
    home: &Path,
    before: &[(PathBuf, Result<usize, String>)],
    map: &dyn Fn(&Path) -> Option<PathBuf>,
    report: &mut Report,
) {
    let now: Vec<PathBuf> = before
        .iter()
        .map(|(dir, _)| map(dir).unwrap_or_else(|| dir.clone()))
        .collect();
    let mut whole = 0usize;
    for ((dir, was), is) in now
        .iter()
        .zip(before.iter().map(|(_, s)| s))
        .zip(status_of(&now))
    {
        match (was, is) {
            (_, Ok(_)) => whole += 1,
            (Ok(_), Err(why)) => report.failed.push(format!(
                "{} is not whole after the move: {why} — mend it before working in it",
                tilde(home, dir)
            )),
            (Err(_), Err(why)) => report.done.push(format!(
                "{} did not answer `git status` before the move either ({why})",
                tilde(home, dir)
            )),
        }
    }
    if whole > 0 {
        report.done.push(format!(
            "checked {whole} checkout{} with `git status`: each answers where it is now",
            if whole == 1 { "" } else { "s" }
        ));
    }
}

/// The directory `git worktree repair` runs in for the repository whose common git
/// directory is `common`: its main checkout (`<main>/.git`'s parent), or the bare
/// repository itself.
fn main_dir(common: &Path) -> PathBuf {
    if common.file_name().is_some_and(|n| n == ".git")
        && let Some(parent) = common.parent()
    {
        return parent.to_path_buf();
    }
    common.to_path_buf()
}

/// How deep a submodule's name (`vendor/astream` is two folders under `modules/`) and a
/// submodule's own submodules are followed.
const MODULE_DEPTH: usize = 8;

/// Every git directory of every repository `survey` found, with the repository's common
/// directory: the common directory, each worktree's administrative directory
/// (`<common>/worktrees/<id>`), every submodule git directory under either
/// (`modules/**`), and the submodule git directory each checkout's `.gitmodules` leads to
/// wherever it lives (the gate snapshot keeps its own in `.aterm-verify/modules`).
fn gitdirs_of(survey: &Survey) -> Vec<(PathBuf, PathBuf)> {
    let mut out: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut commons: Vec<PathBuf> = survey.all().map(|c| c.common.clone()).collect();
    commons.sort();
    commons.dedup();
    for common in &commons {
        let mut dirs = vec![common.clone()];
        if let Ok(entries) = std::fs::read_dir(common.join("worktrees")) {
            dirs.extend(
                entries
                    .flatten()
                    .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                    .map(|e| e.path()),
            );
        }
        let mut modules = Vec::new();
        for dir in &dirs {
            module_dirs(&dir.join("modules"), MODULE_DEPTH, &mut modules);
        }
        dirs.extend(modules);
        for checkout in members(common) {
            submodule_gitdirs(&checkout, MODULE_DEPTH, &mut dirs);
        }
        out.extend(dirs.into_iter().map(|d| (d, common.clone())));
    }
    out.sort();
    out.dedup_by(|a, b| a.0 == b.0);
    out
}

/// The git directories under `dir` (a `modules/` folder): a folder holding `HEAD` is one,
/// and its own `modules/` is walked in turn; any other folder is a component of a
/// submodule name with slashes in it.
fn module_dirs(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        if !e.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let path = e.path();
        if path.join("HEAD").is_file() {
            module_dirs(&path.join("modules"), depth - 1, out);
            out.push(path);
        } else {
            module_dirs(&path, depth - 1, out);
        }
    }
}

/// The git directory each submodule of the checkout at `checkout` names in its `.git`
/// file (`.gitmodules`' `path = …` rows), its own submodules' in turn.
fn submodule_gitdirs(checkout: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth == 0 {
        return;
    }
    let Ok(text) =
        crate::metadata_io::read_bounded_regular_utf8(&checkout.join(".gitmodules"), 256 * 1024)
    else {
        return;
    };
    for line in text.lines() {
        let Some(value) = line
            .trim()
            .strip_prefix("path")
            .map(str::trim_start)
            .and_then(|r| r.strip_prefix('='))
        else {
            continue;
        };
        let sub = checkout.join(value.trim().trim_matches('"'));
        if let Some(gitdir) = gitdir_named_by(&sub.join(".git")) {
            out.push(gitdir);
            submodule_gitdirs(&sub, depth - 1, out);
        }
    }
}

/// A config value or alternates line at its new place, when it names a moved path: an
/// absolute path, or a `file://` URL of one. `None` for anything else (a relative value —
/// a submodule's `core.worktree` — moves with what it is relative to).
fn map_value(value: &str, moved: &[Move]) -> Option<String> {
    if let Some(rest) = value.strip_prefix("file://") {
        return map_through(moved, Path::new(rest)).map(|p| format!("file://{}", p.display()));
    }
    let path = Path::new(value);
    if !path.is_absolute() {
        return None;
    }
    map_through(moved, path).map(|p| p.display().to_string())
}

/// Step 3 for one git directory `gitdir` (as it is now): every value of its `config` and
/// `config.worktree` that names a moved path, by exact value (`--fixed-value`, multi-valued
/// keys included), and every such line of its `objects/info/alternates`. A file that does
/// not even mention a moved path is never handed to git.
fn repair_git_files(home: &Path, gitdir: &Path, moved: &[Move], report: &mut Report) {
    let mentions = |text: &str| {
        moved
            .iter()
            .any(|m| text.contains(m.from.to_string_lossy().as_ref()))
    };
    for name in ["config", "config.worktree"] {
        let file = gitdir.join(name);
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        if !mentions(&text) {
            continue;
        }
        let file_arg = file.display().to_string();
        let Some(out) = git_output(
            gitdir,
            &["config", "--file", &file_arg, "--null", "--list"],
            STATUS_WITHIN,
        ) else {
            report.failed.push(format!(
                "{}: `git config --list` did not answer",
                tilde(home, &file)
            ));
            continue;
        };
        if !out.status.success() {
            report.failed.push(format!(
                "{}: `git config --list` failed ({})",
                tilde(home, &file),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
            continue;
        }
        let listed = String::from_utf8_lossy(&out.stdout).into_owned();
        for entry in listed.split('\0') {
            let Some((key, value)) = entry.split_once('\n') else {
                continue;
            };
            let Some(new) = map_value(value, moved) else {
                continue;
            };
            let done = git_output(
                gitdir,
                &[
                    "config",
                    "--file",
                    &file_arg,
                    "--replace-all",
                    "--fixed-value",
                    key,
                    &new,
                    value,
                ],
                STATUS_WITHIN,
            )
            .is_some_and(|o| o.status.success());
            if done {
                report.done.push(format!(
                    "{}: {key} now names {}",
                    tilde(home, &file),
                    tilde(home, Path::new(&new))
                ));
            } else {
                report.failed.push(format!(
                    "{}: could not rewrite {key} ({value} -> {new})",
                    tilde(home, &file)
                ));
            }
        }
    }
    let alternates = gitdir.join("objects").join("info").join("alternates");
    if let Ok(text) = std::fs::read_to_string(&alternates)
        && mentions(&text)
    {
        let mut rewrote = false;
        let lines: Vec<String> = text
            .lines()
            .map(|line| match map_value(line.trim(), moved) {
                Some(new) => {
                    rewrote = true;
                    new
                }
                None => line.to_string(),
            })
            .collect();
        if rewrote {
            let mut body = lines.join("\n");
            body.push('\n');
            match write_replacing(&alternates, &body) {
                Ok(()) => report
                    .done
                    .push(format!("rewrote {}", tilde(home, &alternates))),
                Err(e) => report.failed.push(format!(
                    "could not rewrite {} ({e})",
                    tilde(home, &alternates)
                )),
            }
        }
    }
}

/// The publication engine's registry, `<root>/publication/registry/repos.toml`: each
/// `path = "~/<rel>"` row whose `~/<rel>` moved now names `~/Developer/<rel>`. The engine
/// reads the row to find the checkout it publishes; the file is the engine's, tracked in
/// its own repository, so the report says to commit it there.
fn repair_registry(survey: &Survey, map: &dyn Fn(&Path) -> Option<PathBuf>, report: &mut Report) {
    let path = survey
        .root
        .join("publication")
        .join("registry")
        .join("repos.toml");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    let mut rows = 0usize;
    let mut out = String::with_capacity(text.len() + 64);
    for line in text.split_inclusive('\n') {
        let rewritten = line
            .trim_start()
            .strip_prefix("path")
            .map(str::trim_start)
            .and_then(|r| r.strip_prefix('='))
            .map(str::trim)
            .and_then(|v| v.strip_prefix('"'))
            .and_then(|v| v.split_once('"'))
            .and_then(|(value, _)| {
                let full = value
                    .strip_prefix("~/")
                    .map_or_else(|| PathBuf::from(value), |rel| survey.home.join(rel));
                let new = map(&full)?;
                Some(line.replacen(value, &tilde(&survey.home, &new), 1))
            });
        match rewritten {
            Some(new) => {
                rows += 1;
                out.push_str(&new);
            }
            None => out.push_str(line),
        }
    }
    if rows == 0 {
        return;
    }
    match write_replacing(&path, &out) {
        Ok(()) => report.done.push(format!(
            "{}: {rows} path row{} now name{} {ROOT_WORDS} — commit {} in the engine checkout",
            tilde(&survey.home, &path),
            if rows == 1 { "" } else { "s" },
            if rows == 1 { "s" } else { "" },
            if rows == 1 { "it" } else { "them" },
        )),
        Err(e) => report.failed.push(format!(
            "could not rewrite {} ({e})",
            tilde(&survey.home, &path)
        )),
    }
}

/// Write `text` to `path` through a sibling temp file and a rename, keeping the mode.
fn write_replacing(path: &Path, text: &str) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("devroot-{}", std::process::id()));
    std::fs::write(&tmp, text)?;
    if let Ok(meta) = std::fs::metadata(path) {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// End this process by `sig`, the interrupt [`execute`] held ([`Report::held_signal`]), as
/// it would have ended had the move not held it: the default action, which for SIGINT,
/// SIGTERM and SIGHUP ends it without a core. Returns only where there are no signals.
pub fn deliver(sig: i32) {
    signals::deliver(sig);
}

/// The interrupts [`execute`] holds: SIGINT (the ^C of a person at the terminal), SIGTERM
/// (a supervisor's end) and SIGHUP (a closed tab), each recorded while the run lasts and
/// handed back to the caller once it ends. A signal this process was started ignoring
/// (`nohup`) stays ignored.
#[cfg(unix)]
mod signals {
    use std::sync::atomic::{AtomicI32, Ordering};

    /// The first interrupt that arrived while held; `0` for none.
    static HELD: AtomicI32 = AtomicI32::new(0);

    /// One hold at a time: the dispositions are the whole process's, so a second hold
    /// overlapping the first would put back the handler the first is about to remove.
    static ONE: std::sync::Mutex<()> = std::sync::Mutex::new(());

    extern "C" fn hold(sig: libc::c_int) {
        let _ = HELD.compare_exchange(0, sig, Ordering::SeqCst, Ordering::SeqCst);
    }

    pub(super) fn holding<T>(run: impl FnOnce() -> T) -> (T, Option<i32>) {
        let _one = ONE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        HELD.store(0, Ordering::SeqCst);
        let handler = hold as extern "C" fn(libc::c_int) as libc::sighandler_t;
        // SAFETY: `signal(2)` with a handler that makes one atomic store, which is
        // async-signal-safe; the previous dispositions are kept to be restored below.
        let previous = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP]
            .map(|sig| (sig, unsafe { libc::signal(sig, handler) }));
        for (sig, was) in &previous {
            if *was == libc::SIG_IGN {
                // SAFETY: puts back the `SIG_IGN` this process was started with.
                unsafe { libc::signal(*sig, libc::SIG_IGN) };
            }
        }
        let value = run();
        for (sig, was) in previous {
            if was != libc::SIG_ERR {
                // SAFETY: puts back the disposition replaced above.
                unsafe { libc::signal(sig, was) };
            }
        }
        let held = HELD.swap(0, Ordering::SeqCst);
        (value, (held != 0).then_some(held))
    }

    pub(super) fn deliver(sig: i32) {
        // SAFETY: the default action for the three signals held ends the process without
        // a core; `raise` delivers it to this thread before it returns.
        unsafe {
            libc::signal(sig, libc::SIG_DFL);
            libc::raise(sig);
        }
    }
}

#[cfg(not(unix))]
mod signals {
    pub(super) fn holding<T>(run: impl FnOnce() -> T) -> (T, Option<i32>) {
        (run(), None)
    }

    pub(super) fn deliver(_sig: i32) {}
}

/// The plan as lines for a person: each move, each blocker under its checkout, the
/// agent-state renames, the dev links, and the notes.
#[must_use]
pub fn render_plan(survey: &Survey, plan: &Plan) -> Vec<String> {
    let home = &survey.home;
    // A blocker names paths whole; under the home they read from `~`.
    let in_home = format!("{}/", home.display());
    let said = |b: &Blocker| b.to_string().replace(&in_home, "~/");
    let mut lines = Vec::new();
    for m in &plan.moves {
        let blockers: Vec<&Blocker> = plan
            .blocked
            .iter()
            .filter(|(p, _)| *p == m.from)
            .map(|(_, b)| b)
            .collect();
        lines.push(format!(
            "{} {} -> {}",
            if blockers.is_empty() {
                "move   "
            } else {
                "BLOCKED"
            },
            tilde(home, &m.from),
            tilde(home, &m.to)
        ));
        for b in blockers {
            lines.push(format!("          {}", said(b)));
        }
    }
    // A blocker no move owns (never expected: every scanned path is a move's, a moving
    // repository's, or an agent folder's) is still said, so a refusal is never silent.
    for (path, b) in &plan.blocked {
        if !plan.moves.iter().any(|m| m.from == *path) {
            lines.push(format!("BLOCKED {}: {}", tilde(home, path), said(b)));
        }
    }
    for dir in &plan.agent_dirs {
        lines.push(format!(
            "rename  {} -> {}",
            tilde(home, &dir.from),
            tilde(home, &dir.to)
        ));
    }
    for relink in &plan.relinks {
        lines.push(format!(
            "relink  {}: {} -> {}",
            relink.program,
            tilde(home, &relink.from),
            tilde(home, &relink.to)
        ));
    }
    for note in &plan.notes {
        lines.push(format!("note    {note}"));
    }
    if plan.unreadable > 0 {
        lines.push(format!(
            "note    {} process{} of yours could not be read (system agents refuse it); \
             none of those is counted as using a checkout",
            plan.unreadable,
            if plan.unreadable == 1 { "" } else { "es" }
        ));
    }
    lines
}

#[cfg(test)]
#[path = "devroot_tests.rs"]
mod tests;
