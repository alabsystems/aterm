// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Init disables data-volume indexing in its root transaction. Cache placement also keeps
//! Rust docs and build output out of Spotlight if indexing is enabled outside setup.

use crate::init_observe::{LastReading, Reading, TOOLCHAIN_NOTE};
use crate::items::{Ctx, Item};
use crate::model::{Probe, User};
use crate::port::{Cmd, System};
use aterm_toolchain::layout;
use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

/// The group's id, as `aterm init --only` and every remedy that names it spell it.
pub const ID: &str = "init.spotlight";

/// Apple's indexing manager and the writable macOS data volume.
const MDUTIL: &str = "/usr/bin/mdutil";
const DATA_VOLUME: &str = "/System/Volumes/Data";

/// Read the data volume's indexing state. Unknown or failed output never means disabled.
///
/// # Errors
/// The command failed or did not report exactly one recognized state.
pub(crate) fn indexing(sys: &dyn System) -> Result<bool, String> {
    let ran = sys
        .run_complete_output(&Cmd::new(MDUTIL).args(["-s", DATA_VOLUME]))
        .map_err(|error| error.to_string())?;
    if !ran.success() {
        return Err(format!("Spotlight status: {}", ran.describe()));
    }
    let text = std::str::from_utf8(&ran.stdout).map_err(|error| error.to_string())?;
    match (
        text.lines().any(|line| line.trim() == "Indexing enabled."),
        text.lines().any(|line| line.trim() == "Indexing disabled."),
    ) {
        (true, false) => Ok(true),
        (false, true) => Ok(false),
        _ => Err("Spotlight indexing status is unrecognized".into()),
    }
}

/// Disable indexing in setup's existing root transaction, without moving any user files.
pub(crate) struct Indexing;

impl Item for Indexing {
    fn id(&self) -> String {
        "init.spotlight.indexing".into()
    }
    fn family(&self) -> &'static str {
        "spotlight"
    }
    fn title(&self) -> String {
        "Spotlight indexing of the data volume is disabled".into()
    }
    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        if !sys.macos() {
            return Probe::NotApplicable {
                why: "Spotlight is macOS's".into(),
            };
        }
        match indexing(sys) {
            Ok(true) => Probe::Change {
                from: "data-volume indexing enabled".into(),
                to: "data-volume indexing disabled".into(),
            },
            Ok(false) => Probe::Ok {
                now: "data-volume indexing disabled".into(),
            },
            Err(why) => Probe::Unusable { why },
        }
    }
    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        if !sys.macos() {
            return Err("Spotlight is macOS's".into());
        }
        if sys.euid() != 0 {
            return Err("disabling Spotlight belongs to setup's administrator step".into());
        }
        let ran = sys
            .run(&Cmd::new(MDUTIL).args(["-i", "off", DATA_VOLUME]))
            .map_err(|error| error.to_string())?;
        if ran.success() {
            Ok(())
        } else {
            Err(ran.describe())
        }
    }
}

/// The rustup profile that installs no documentation.
const MINIMAL: &str = "minimal";

/// Where a toolchain keeps `rust-docs`' offline HTML: the one folder rustup's manifest names
/// for the component.
const DOCS_HTML: &str = "share/doc/rust/html";

/// The name `rust-docs`' HTML is set aside under, beside where it was: Spotlight never reads a
/// folder whose name ends in `.noindex`.
const DOCS_ASIDE: &str = "html.noindex";

/// What the removal that runs in the background does, as this user: rustup takes the component
/// out of its records (removing the link that stands where the HTML was), then the HTML set aside
/// goes. `$1` is rustup, `$2` the toolchain and `$3` the folder set aside.
const REMOVE_IN_BACKGROUND: &str =
    "\"$1\" component remove rust-docs --toolchain \"$2\" && /bin/rm -rf -- \"$3\"";

/// Whether the build-cache row searches this login's home folder for the person's own project
/// build folders that Spotlight indexes, which its line notes ([`BuildNoindex`]). Spotlight's
/// answer may place one in a folder macOS guards (Documents, Desktop, Downloads, iCloud Drive,
/// another app's data), and the first read of a file there can put macOS's question about that
/// folder on the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Projects {
    /// Search, and note what Spotlight indexes.
    Noted,
    /// Search nothing, and note none: automatic runs and runs without the person at the
    /// console do not discover paths whose first read can raise a macOS privacy question.
    Unread,
}

/// Keep this login's Rust docs and aterm's build caches out of Spotlight, the build-cache row
/// searching for the person's project build folders as `projects` says.
///
/// # Errors
/// Init did not resolve exactly one user.
pub fn items(ctx: &Ctx, projects: Projects) -> Result<Vec<Box<dyn Item>>, String> {
    let [user] = ctx.users.as_slice() else {
        return Err("aterm init Rust toolchain settings require exactly one local user".into());
    };
    let home = Path::new(&user.home);
    // This process's `CARGO_HOME`, `RUSTUP_HOME` and `XDG_CONFIG_HOME` are the user's only where
    // the process runs as that user's login (its `HOME` is theirs): another account's (root's,
    // for a user init set up) name its own folders, and a test's user is no login of the test
    // process's (the Linux lane's guest exports its own `CARGO_HOME`).
    let own = std::env::var_os("HOME").is_some_and(|own| Path::new(&own) == home);
    let set = |name: &str| {
        own.then(|| std::env::var_os(name))
            .flatten()
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let rustup = Rustup {
        user: user.clone(),
        cargo_home: set("CARGO_HOME").unwrap_or_else(|| home.join(".cargo")),
        rustup_home: set("RUSTUP_HOME").unwrap_or_else(|| home.join(".rustup")),
    };
    let pkg = aterm_toolchain::atpkg_prefix(home, set("XDG_CONFIG_HOME").as_deref());
    let builds = BuildNoindex::new(
        user.clone(),
        home.to_path_buf(),
        pkg,
        rustup.cargo_home.clone(),
        projects,
    );
    Ok(vec![
        Box::new(Profile(rustup.clone())),
        Box::new(NoDocs(rustup, LastReading::default())),
        Box::new(builds),
    ])
}

/// The one item of [`items`] that keeps this login's `rust-docs` out of Spotlight
/// (`init.spotlight.rust-docs@<user>`), as bare `aterm doctor` applies it.
///
/// # Errors
/// As [`items`].
pub(crate) fn rust_docs_item(ctx: &Ctx) -> Result<Box<dyn Item>, String> {
    items(ctx, Projects::Unread)?
        .into_iter()
        .find(|item| item.id().starts_with("init.spotlight.rust-docs@"))
        .ok_or_else(|| "aterm init composes no rust-docs item".to_owned())
}

/// How long one Spotlight search of a home folder for build folders stands ([`Inventory`]),
/// found or failed: a setup run, a check and System State's reading share one for this
/// long instead of each searching the home folder again. Check again searches anew
/// ([`search_again`]).
const INVENTORY_AGE: u64 = 5 * 60;

/// What the build cache's `.ubd` folder keeps from macOS's fseventsd: every file event under it
/// (`aterm_tempfile::QUIET`), which fseventsd otherwise parses, hands to each whole-volume
/// watcher and keeps in its history. Observed on macOS 27.0, documented nowhere: a macOS that
/// stops honouring the name loses only that benefit.
const QUIET_LINE: &str = "cargo-build.noindex sits in .ubd, whose file events fseventsd drops \
                          before it delivers or keeps them (observed on macOS 27.0)";

/// The Spotlight query for build folders: the `CACHEDIR.TAG` cargo writes at the top of
/// each build folder, so the answer is a line per build folder (and per other tool's cache
/// tag), never one per artifact (a developer's home holds thousands of `.rmeta` files,
/// over the complete capture).
const BUILD_MARKER: &str = "kMDItemFSName == \"CACHEDIR.TAG\"";

/// What the comment line of the cache tag cargo writes says of its maker: "# This file is a
/// cache directory tag created by cargo."
const MADE_BY_CARGO: &str = "created by cargo";

/// The most a cache tag of cargo's is read for, in bytes: cargo writes under 200.
const TAG_MAX: u64 = 4096;

/// The files cargo keeps at the top of a cargo home, whatever names it: its package
/// cache's locks (`.package-cache`; `.package-cache-mutate` since cargo 1.74) and its
/// global cache tracker (`.global-cache`, since 1.78). A folder holding one is a cargo
/// home, whose `registry` and `git` are cargo's download caches.
const CARGO_HOME_MARKS: [&str; 3] = [".package-cache", ".package-cache-mutate", ".global-cache"];

/// Cargo's download caches in its home, each tagged as a build folder is.
const CARGO_CACHES: [&str; 2] = ["registry", "git"];

/// What one search of a home folder found: the cargo build folders Spotlight indexes, or
/// why it could not say.
type Found = Result<BTreeSet<PathBuf>, String>;

/// The process's one [`Inventory`], which every item outside a test reads.
static SHARED: std::sync::OnceLock<Arc<Inventory>> = std::sync::OnceLock::new();

/// Forget the process's last Spotlight search for build folders, found or failed, so the
/// next reading of the Rust in Spotlight row searches the home folder again: Check again's,
/// which the person pressed to see the Mac as it is now. It never waits on a search under
/// way (one can take a minute), so the window that calls it stays live.
pub fn search_again() {
    if let Some(inventory) = SHARED.get() {
        inventory.forget();
    }
}

/// The last Spotlight search for build folders: the home it searched, when (Unix seconds),
/// the [`Inventory::generation`] it began in, and what it found.
type Kept = (PathBuf, u64, u64, Found);

/// The last Spotlight search for build folders, which readers share ([`INVENTORY_AGE`]).
#[derive(Debug, Default)]
struct Inventory {
    /// The last search, held for its whole run so readers share one search.
    kept: Mutex<Option<Kept>>,
    /// Bumped by [`Self::forget`]: a search that began in an earlier generation is stale.
    /// An atomic, so a forget never waits on the lock a search under way holds.
    generation: AtomicU64,
}

impl Inventory {
    /// The process's one inventory ([`SHARED`]); a test's item keeps its own.
    fn shared() -> Arc<Self> {
        #[cfg(not(test))]
        {
            SHARED.get_or_init(Arc::default).clone()
        }
        #[cfg(test)]
        {
            Arc::default()
        }
    }

    /// Make the search kept stale, and any under way, so the next read searches again
    /// ([`search_again`]). It takes no lock: a search under way holds it.
    fn forget(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    /// What a search of `home` taken at most [`INVENTORY_AGE`] before `now`, and since the
    /// last [`Self::forget`], found, else `search`'s, kept for the next reader: a search
    /// that failed stands as long as one that found, so a home Spotlight cannot answer for
    /// is not searched on every read.
    fn read(&self, home: &Path, now: u64, search: impl FnOnce() -> Found) -> Found {
        let mut kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
        let generation = self.generation.load(Ordering::Acquire);
        if let Some((at_home, at, taken_in, found)) = kept.as_ref()
            && at_home == home
            && *taken_in == generation
            && *at <= now
            && now - at < INVENTORY_AGE
        {
            return found.clone();
        }
        let found = search();
        *kept = Some((home.to_path_buf(), now, generation, found.clone()));
        found
    }
}

/// Keep aterm's cargo build cache and tooling cache in `.noindex` folders.
///
/// atpkg installs each Trust toolchain build as `store/trust/<n>.noindex` with `<n>` a
/// link to it, and its rustup view as `rustup/<name>.noindex` behind `rustup/<name>`, which
/// Spotlight never reads ([`layout`]). It lays a plain build or view there itself, never
/// while it is in use or a program runs from it (a program started from one may still be
/// under macOS's first-exec check, and syspolicyd crashes when that file's folder is
/// renamed mid-check). So a plain one left is aterm's own move still to come, and the item
/// moves nothing: the build in use and its view, which atpkg moves at its next update,
/// are one quiet note ([`TOOLCHAIN_NOTE`]), and a build no longer in use, which atpkg moves
/// at its next pass, goes unsaid. Neither is a step or a change.
///
/// The person's own cargo build folders that Spotlight indexes, whatever their names, are
/// theirs, never aterm's to rename or link: the row only counts them in a quiet note and
/// lists them in its line, and they are no step, no change and nothing the run waits on.
/// A failed search for them costs only the note.
#[derive(Debug)]
struct BuildNoindex {
    user: User,
    home: PathBuf,
    /// atpkg's prefix ([`aterm_toolchain::atpkg_prefix`]): its store and rustup views.
    pkg: PathBuf,
    /// The cargo home this process's environment names where it is the user's own login
    /// (`CARGO_HOME`, else `~/.cargo`),
    /// whose download caches (`registry`, `git`) carry the same cache tag as a build
    /// folder and are none. An app launched from the Dock has launchd's environment, never
    /// the `CARGO_HOME` a shell's startup file sets, so a cargo home is also recognised by
    /// what cargo keeps in it ([`CARGO_HOME_MARKS`]).
    cargo_home: PathBuf,
    /// The last probe's reading, which is System State's.
    reading: LastReading,
    /// Whether the row searches for the person's project build folders ([`Projects`]).
    projects: Projects,
    /// The Spotlight search for project build folders, shared for [`INVENTORY_AGE`].
    inventory: Arc<Inventory>,
}

impl BuildNoindex {
    fn new(
        user: User,
        home: PathBuf,
        pkg: PathBuf,
        cargo_home: PathBuf,
        projects: Projects,
    ) -> Self {
        Self {
            user,
            home,
            pkg,
            cargo_home,
            reading: LastReading::default(),
            projects,
            inventory: Inventory::shared(),
        }
    }

    fn caches_parent(&self) -> PathBuf {
        self.home.join("Library/Caches")
    }

    /// The shared build cache's root ([`aterm_toolchain::build_cache::CACHE_UNDER_HOME`]):
    /// `.noindex` for Spotlight, in a `.ubd` folder for fseventsd ([`QUIET_LINE`]).
    fn cargo_build_noindex(&self) -> PathBuf {
        self.home
            .join(aterm_toolchain::build_cache::CACHE_UNDER_HOME)
    }

    fn tooling_noindex(&self) -> PathBuf {
        self.home.join("Library/Caches/aterm/tooling.noindex")
    }

    fn store(&self) -> PathBuf {
        self.pkg.join(layout::STORE_DIR)
    }

    /// The Trust toolchain atpkg laid where Spotlight still indexes it, by its layout
    /// contract ([`layout`]): the build in use and the rustup views laid from it, which
    /// atpkg moves at its next update; and whether an older build is still plain, which
    /// atpkg moves at its next pass and the row leaves unsaid.
    fn exposed_toolchain(&self) -> (Vec<PathBuf>, bool) {
        let builds = layout::spotlight_exposed_builds(&self.pkg);
        let older = builds.iter().any(|build| !build.active);
        let mut in_use: Vec<PathBuf> = builds
            .into_iter()
            .filter(|build| build.active)
            .map(|build| build.dir)
            .collect();
        in_use.extend(
            layout::spotlight_exposed_views(&self.pkg)
                .into_iter()
                .map(|view| view.dir),
        );
        (in_use, older)
    }

    fn dir_present(sys: &dyn System, path: &Path) -> Result<bool, String> {
        match sys.stat(path) {
            Ok(s) => Ok(s.dir && !s.symlink),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    fn present(sys: &dyn System, path: &Path) -> Result<bool, String> {
        match sys.stat(path) {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// The caches apply makes that are not there yet (none without `~/Library/Caches`).
    fn missing_caches(&self, sys: &dyn System) -> Result<Vec<PathBuf>, String> {
        let mut missing = Vec::new();
        if Self::dir_present(sys, &self.caches_parent())? {
            for dir in [self.cargo_build_noindex(), self.tooling_noindex()] {
                if !Self::dir_present(sys, &dir)? {
                    missing.push(dir);
                }
            }
        }
        Ok(missing)
    }

    /// `folders`, comma-separated.
    fn listed(folders: &[PathBuf]) -> String {
        let names: Vec<String> = folders.iter().map(|d| d.display().to_string()).collect();
        names.join(", ")
    }

    /// The row's line on the toolchain aterm is using, which atpkg moves at its next update.
    fn toolchain_line(in_use: &[PathBuf]) -> String {
        format!("{TOOLCHAIN_NOTE}: {}", Self::listed(in_use))
    }

    /// The person's own cargo build folders under `self.home` that Spotlight indexes, from
    /// the [`Inventory`] (`None` when Spotlight could not be asked, or this row searches
    /// nothing ([`Projects::Unread`]), which costs only the row's note), less any gone since
    /// the search.
    fn spotlit_projects(&self, sys: &dyn System) -> Option<Vec<PathBuf>> {
        if self.projects == Projects::Unread {
            return None;
        }
        let indexed = self
            .inventory
            .read(&self.home, sys.now(), || self.search(sys))
            .ok()?;
        let mut out = Vec::new();
        for cand in indexed {
            if Self::dir_present(sys, &cand).ok()?
                && Self::present(sys, &cand.join("CACHEDIR.TAG")).ok()?
            {
                out.push(cand);
            }
        }
        Some(out)
    }

    /// Whether the cache tag at `tag` is one cargo wrote: a small file whose comment line
    /// names cargo as its maker. An empty one is none, as a pipe or a device reads.
    fn tagged_by_cargo(sys: &dyn System, tag: &Path) -> bool {
        if !sys
            .stat(tag)
            .is_ok_and(|s| !s.dir && !s.symlink && s.len > 0 && s.len <= TAG_MAX)
        {
            return false;
        }
        sys.read_file(tag).is_ok_and(|bytes| {
            String::from_utf8_lossy(&bytes)
                .lines()
                .any(|line| line.starts_with('#') && line.contains(MADE_BY_CARGO))
        })
    }

    /// The paths the cargo home the environment names is reached by: as named, and the
    /// folder it resolves to where that is a link (`~/.cargo` linked to a visible folder),
    /// since Spotlight reports a folder by its own path.
    fn cargo_homes(&self, sys: &dyn System) -> Vec<PathBuf> {
        let mut homes = vec![self.cargo_home.clone()];
        if let Ok(real) = sys.canonicalize(&self.cargo_home)
            && real != self.cargo_home
        {
            homes.push(real);
        }
        homes
    }

    /// Whether `dir` holds what cargo keeps at the top of its home ([`CARGO_HOME_MARKS`]).
    fn holds_cargo_home(sys: &dyn System, dir: &Path) -> bool {
        CARGO_HOME_MARKS
            .iter()
            .any(|mark| sys.stat(&dir.join(mark)).is_ok_and(|s| !s.dir))
    }

    /// Whether `folder` is cargo's own, where it keeps its download caches: never a
    /// project's build folder. Anything in the cargo home the environment names, by any of
    /// its paths (`homes`, [`Self::cargo_homes`]), is; one that holds the home folder
    /// (`CARGO_HOME=$HOME`) leaves out only its caches, `registry` and `git`. So is
    /// anything in the `registry` or `git` of any folder under the home that holds what
    /// cargo keeps at its home's top ([`Self::holds_cargo_home`]): the cargo home a shell
    /// names that this process's environment does not.
    fn cargo_keeps(&self, sys: &dyn System, homes: &[PathBuf], folder: &Path) -> bool {
        let named = homes.iter().any(|home| {
            if self.home.starts_with(home) {
                CARGO_CACHES
                    .into_iter()
                    .any(|cache| folder.starts_with(home.join(cache)))
            } else {
                folder.starts_with(home)
            }
        });
        named
            || folder
                .ancestors()
                .take_while(|dir| *dir != self.home && dir.starts_with(&self.home))
                .any(|dir| {
                    dir.file_name()
                        .is_some_and(|name| CARGO_CACHES.iter().any(|cache| name == *cache))
                        && dir
                            .parent()
                            .is_some_and(|home| Self::holds_cargo_home(sys, home))
                })
    }

    /// Ask Spotlight which cargo build folders under `self.home` it indexes: the folders
    /// holding a [`BUILD_MARKER`] it holds, one line each, that cargo wrote.
    fn search(&self, sys: &dyn System) -> Found {
        let mdfind_bin = Path::new("/usr/bin/mdfind");
        sys.stat(mdfind_bin)
            .map_err(|e| format!("{}: {e}", mdfind_bin.display()))?;
        let cmd = Cmd::new(mdfind_bin).as_user(&self.user).args([
            "-onlyin",
            &self.home.to_string_lossy(),
            "-0",
            BUILD_MARKER,
        ]);
        let ran = sys
            .run_complete_stdout(&cmd)
            .map_err(|e| format!("{}: {e}", cmd.display()))?;
        if !ran.success() {
            return Err(format!("{}: {}", cmd.display(), ran.describe()));
        }
        let text = std::str::from_utf8(&ran.stdout)
            .map_err(|_| "Spotlight inventory is not UTF-8".to_owned())?;
        if !text.is_empty() && !text.ends_with('\0') {
            return Err("Spotlight inventory is not a complete NUL-delimited list".into());
        }
        // The marker sits at the top of its build folder: the folder is its parent, under
        // the home folder and in no `.noindex` folder. Cargo tags its own download caches
        // in its home (`registry`, `git`) the same way: they are no project's build folder.
        let cargo_homes = self.cargo_homes(sys);
        let mut candidates = BTreeSet::new();
        for raw in text.split('\0').filter(|p| !p.is_empty()) {
            let Some(folder) = Path::new(raw).parent() else {
                continue;
            };
            let kept_out = folder
                .components()
                .any(|part| part.as_os_str().to_string_lossy().ends_with(".noindex"));
            if !kept_out
                && folder.starts_with(&self.home)
                && folder != self.home
                && !self.cargo_keeps(sys, &cargo_homes, folder)
            {
                candidates.insert(folder.to_path_buf());
            }
        }
        // A folder counts when cargo wrote its tag, whatever its name (a `CARGO_TARGET_DIR`
        // or `--target-dir` names its own), and once: a build folder inside one already
        // found (a build script's own, under its `OUT_DIR`) is part of it. In path order a
        // folder's descendants follow it, so the last one kept is the only one a later
        // folder can sit inside.
        let mut found = BTreeSet::new();
        for folder in candidates {
            if found
                .last()
                .is_some_and(|outer: &PathBuf| folder.starts_with(outer))
            {
                continue;
            }
            if Self::tagged_by_cargo(sys, &folder.join("CACHEDIR.TAG")) {
                found.insert(folder);
            }
        }
        Ok(found)
    }

    /// The row's line on the person's own project folders Spotlight indexes.
    fn projects_line(projects: &[PathBuf]) -> String {
        format!(
            "Spotlight indexes these project build folders of yours, which aterm leaves as they are: {}",
            Self::listed(projects)
        )
    }
}

impl Item for BuildNoindex {
    fn id(&self) -> String {
        format!("init.spotlight.build-noindex@{}", self.user.name)
    }

    fn family(&self) -> &'static str {
        "spotlight"
    }

    fn title(&self) -> String {
        "Rust build caches and Trust toolchain sysroots stay in .noindex folders, the build \
         cache in a .ubd folder fseventsd drops"
            .into()
    }

    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        self.reading.set(None);
        let has_caches = match Self::dir_present(sys, &self.caches_parent()) {
            Ok(v) => v,
            Err(why) => return Probe::Unusable { why },
        };
        let has_store = match Self::dir_present(sys, &self.store()) {
            Ok(v) => v,
            Err(why) => return Probe::Unusable { why },
        };
        if !has_caches && !has_store {
            return Probe::NotApplicable {
                why: format!(
                    "neither {} nor {} exists",
                    self.caches_parent().display(),
                    self.store().display()
                ),
            };
        }
        let missing = match self.missing_caches(sys) {
            Ok(missing) => missing,
            Err(why) => return Probe::Unusable { why },
        };
        let (in_use, older) = self.exposed_toolchain();
        let projects = self.spotlit_projects(sys).unwrap_or_default();
        // What the row only notes: the toolchain aterm is using, which atpkg moves at its
        // next update, and the person's project folders. Neither is a step or a change. A
        // plain build no longer in use is atpkg's to move at its next pass.
        let mut noted = Vec::new();
        if !in_use.is_empty() {
            noted.push(Self::toolchain_line(&in_use));
        }
        if !projects.is_empty() {
            noted.push(Self::projects_line(&projects));
        }
        let (projects, toolchain) = (projects.len(), !in_use.is_empty());
        if missing.is_empty() {
            let mut now = if in_use.is_empty() && !older {
                "cargo-build.noindex, tooling.noindex, and the Trust toolchain are .noindex"
            } else {
                "cargo-build.noindex and tooling.noindex are .noindex"
            }
            .to_owned();
            now.push_str(&format!("; {QUIET_LINE}"));
            if !noted.is_empty() {
                self.reading.set(Some(Reading::KeptOut {
                    projects,
                    toolchain,
                }));
                now.push_str(&format!("; {}", noted.join("; ")));
            }
            return Probe::Ok { now };
        }
        self.reading.set(Some(Reading::BuildsIndexed {
            projects,
            toolchain,
        }));
        let mut from: Vec<String> = missing
            .iter()
            .filter_map(|dir| dir.file_name())
            .map(|name| format!("{} absent", name.to_string_lossy()))
            .collect();
        from.extend(noted);
        Probe::Change {
            from: from.join("; "),
            to: "cargo-build.noindex in .ubd, and tooling.noindex".to_owned(),
        }
    }

    fn detail(&self, _sys: &dyn System, _ctx: &Ctx) -> Option<String> {
        self.reading.detail()
    }

    /// Make the missing caches. Nothing else is its to move: atpkg moves its toolchain
    /// builds, and the person's own project folders are theirs.
    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        for dir in self.missing_caches(sys)? {
            sys.create_dir_all(&dir, 0o755)
                .map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        Ok(())
    }
}

/// This login's rustup, where it keeps its toolchains.
#[derive(Debug, Clone)]
struct Rustup {
    user: User,
    cargo_home: PathBuf,
    rustup_home: PathBuf,
}

impl Rustup {
    fn bin(&self) -> PathBuf {
        self.cargo_home.join("bin/rustup")
    }

    /// `rustup <args>` as this user, its homes named so it reads the folders probed.
    fn cmd(&self, args: &[&str]) -> Cmd {
        Cmd::new(self.bin())
            .as_user(&self.user)
            .env("CARGO_HOME", self.cargo_home.to_string_lossy())
            .env("RUSTUP_HOME", self.rustup_home.to_string_lossy())
            .args(args)
    }

    /// The trimmed standard output of a run that succeeded.
    fn read(&self, sys: &dyn System, args: &[&str]) -> Result<String, String> {
        let cmd = self.cmd(args);
        let ran = sys
            .run(&cmd)
            .map_err(|e| format!("{}: {e}", cmd.display()))?;
        if !ran.success() {
            return Err(format!("{}: {}", cmd.display(), ran.describe()));
        }
        Ok(ran.out().trim().to_owned())
    }

    /// `Some(why)` when there is no rustup to ask.
    fn absent(&self, sys: &dyn System) -> Result<Option<String>, String> {
        match sys.stat(&self.bin()) {
            Ok(_) => Ok(None),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                Ok(Some(format!("no rustup at {}", self.bin().display())))
            }
            Err(e) => Err(format!("{}: {e}", self.bin().display())),
        }
    }

    /// The toolchains rustup installed that carry `rust-docs`, and where each one's HTML is.
    fn with_docs(&self, sys: &dyn System) -> Result<Vec<Docs>, String> {
        let list = self.read(sys, &["toolchain", "list"])?;
        let mut out = Vec::new();
        for name in list.lines().filter_map(|l| l.split_whitespace().next()) {
            let dir = self.rustup_home.join("toolchains").join(name);
            if sys.stat(&dir).is_ok_and(|s| s.symlink) {
                continue;
            }
            let record = dir.join("lib/rustlib/components");
            let text = match sys.read_file(&record) {
                Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(format!("{}: {e}", record.display())),
            };
            if text
                .lines()
                .any(|c| c == "rust-docs" || c.starts_with("rust-docs-"))
            {
                let html = dir.join(DOCS_HTML);
                // A link in its place stands for the HTML set aside.
                let indexed = sys.read_link(&html).is_err()
                    && match sys.stat(&html) {
                        Ok(stat) => stat.dir && !stat.symlink,
                        Err(e) if e.kind() == io::ErrorKind::NotFound => false,
                        Err(e) => return Err(format!("{}: {e}", html.display())),
                    };
                out.push(Docs {
                    toolchain: name.to_owned(),
                    html,
                    indexed,
                });
            }
        }
        Ok(out)
    }

    /// Set `docs`' HTML aside where Spotlight never reads it, at once: the folder is renamed
    /// `html.noindex` and a link to it stands in its place, so rustup's removal still finds the
    /// folder its manifest names. Then start rustup's removal of the component, and the HTML's,
    /// in the background ([`REMOVE_IN_BACKGROUND`]): removing tens of thousands of files is no
    /// step a setup run waits on.
    fn set_aside(&self, sys: &dyn System, docs: &Docs) -> Result<(), String> {
        let aside = docs.html.with_file_name(DOCS_ASIDE);
        // An aside an earlier run left (its removal failed, then rustup put the docs back) is not
        // this run's to remove or to merge with: the row says where it is.
        match sys.stat(&aside) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Ok(_) => {
                return Err(format!(
                    "{} is there already; remove it, then run this again",
                    aside.display()
                ));
            }
            Err(e) => return Err(format!("{}: {e}", aside.display())),
        }
        sys.rename(&docs.html, &aside)
            .map_err(|e| format!("{} -> {}: {e}", docs.html.display(), aside.display()))?;
        sys.symlink(Path::new(DOCS_ASIDE), &docs.html)
            .map_err(|e| format!("{} -> {DOCS_ASIDE}: {e}", docs.html.display()))?;
        if sys.euid() == 0 {
            // Run as root for this user: the link is theirs, as the folder it replaces was.
            sys.chown(&docs.html, self.user.uid, self.user.gid)
                .map_err(|e| format!("{}: {e}", docs.html.display()))?;
        }
        let removal = Cmd::new("/bin/sh")
            .as_user(&self.user)
            .env("CARGO_HOME", self.cargo_home.to_string_lossy())
            .env("RUSTUP_HOME", self.rustup_home.to_string_lossy())
            .args(["-c", REMOVE_IN_BACKGROUND, "sh"])
            .arg(self.bin())
            .arg(docs.toolchain.as_str())
            .arg(&aside);
        sys.spawn_detached(&removal, Path::new("/dev/null"))
            .map(drop)
            .map_err(|e| format!("{}: {e}", removal.display()))
    }
}

/// A toolchain rustup installed with `rust-docs` ([`Rustup::with_docs`]).
#[derive(Debug)]
struct Docs {
    toolchain: String,
    /// Where its HTML is: [`DOCS_HTML`] in the toolchain.
    html: PathBuf,
    /// The HTML is a plain folder there, which Spotlight indexes. Otherwise it was set aside in
    /// [`DOCS_ASIDE`] behind a link, or is gone, and its removal is rustup's, under way or ended.
    indexed: bool,
}

/// rustup installs its `minimal` profile, so a toolchain installed later brings no docs.
#[derive(Debug)]
struct Profile(Rustup);

impl Item for Profile {
    fn id(&self) -> String {
        format!("init.spotlight.rustup-profile@{}", self.0.user.name)
    }

    fn family(&self) -> &'static str {
        "spotlight"
    }

    fn title(&self) -> String {
        "rustup installs its minimal profile (no offline docs for Spotlight)".into()
    }

    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        match self.0.absent(sys) {
            Ok(Some(why)) => return Probe::NotApplicable { why },
            Ok(None) => {}
            Err(why) => return Probe::Unusable { why },
        }
        match self.0.read(sys, &["show", "profile"]) {
            Err(why) => Probe::Unusable { why },
            Ok(p) if p == MINIMAL => Probe::Ok {
                now: "rustup's profile is minimal".into(),
            },
            Ok(p) => Probe::Change {
                from: format!("rustup's profile is {p}, which installs rust-docs"),
                to: "minimal".into(),
            },
        }
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        self.0.read(sys, &["set", "profile", MINIMAL]).map(drop)
    }

    fn revert(&self) -> Option<String> {
        Some(format!("{} set profile default", self.0.bin().display()))
    }
}

/// No toolchain rustup installed carries `rust-docs`; the last probe's reading is
/// System State's.
#[derive(Debug)]
struct NoDocs(Rustup, LastReading);

impl Item for NoDocs {
    fn id(&self) -> String {
        format!("init.spotlight.rust-docs@{}", self.0.user.name)
    }

    fn family(&self) -> &'static str {
        "spotlight"
    }

    fn title(&self) -> String {
        "no rustup toolchain carries rust-docs (offline HTML Spotlight indexes)".into()
    }

    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        self.1.set(None);
        match self.0.absent(sys) {
            Ok(Some(why)) => return Probe::NotApplicable { why },
            Ok(None) => {}
            Err(why) => return Probe::Unusable { why },
        }
        let docs = match self.0.with_docs(sys) {
            Ok(docs) => docs,
            Err(why) => return Probe::Unusable { why },
        };
        let names = |indexed: bool| {
            docs.iter()
                .filter(|docs| docs.indexed == indexed)
                .map(|docs| docs.toolchain.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        if docs.iter().any(|docs| docs.indexed) {
            self.1.set(Some(Reading::DocsInstalled));
            return Probe::Change {
                from: format!("rust-docs installed in {}", names(true)),
                to: format!("set aside as {DOCS_ASIDE} at once, then removed in the background"),
            };
        }
        Probe::Ok {
            now: if docs.is_empty() {
                "no rustup toolchain carries rust-docs".into()
            } else {
                format!(
                    "rust-docs in {} is set aside as {DOCS_ASIDE}, which Spotlight never reads; \
                     rustup removes it in the background",
                    names(false)
                )
            },
        }
    }

    fn detail(&self, _sys: &dyn System, _ctx: &Ctx) -> Option<String> {
        self.1.detail()
    }

    /// Set each indexed toolchain's docs aside at once and remove them in the background
    /// ([`Rustup::set_aside`]): the journal's one entry names both.
    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        for docs in self.0.with_docs(sys)? {
            if docs.indexed {
                self.0.set_aside(sys, &docs)?;
            }
        }
        Ok(())
    }

    fn revert(&self) -> Option<String> {
        Some(format!(
            "while the background removal has not run, replace each toolchain's \
             {DOCS_HTML} link with its {DOCS_ASIDE}; once it has, {} component add rust-docs \
             --toolchain <each toolchain it was removed from>",
            self.0.bin().display()
        ))
    }
}

#[cfg(test)]
#[path = "init_spotlight_tests.rs"]
mod tests;
