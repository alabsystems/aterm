// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Init items that keep Rust docs and aterm's build caches out of Spotlight, and note the build
//! folders it still indexes.

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

/// Keep this login's Rust docs and aterm's build caches out of Spotlight.
///
/// # Errors
/// Init did not resolve exactly one user.
pub fn items(ctx: &Ctx) -> Result<Vec<Box<dyn Item>>, String> {
    let [user] = ctx.users.as_slice() else {
        return Err("aterm init Rust toolchain settings require exactly one local user".into());
    };
    let home = Path::new(&user.home);
    let set = |name: &str| {
        std::env::var_os(name)
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
    items(ctx)?
        .into_iter()
        .find(|item| item.id().starts_with("init.spotlight.rust-docs@"))
        .ok_or_else(|| "aterm init composes no rust-docs item".to_owned())
}

/// How long one Spotlight search of a home folder for build folders stands ([`Inventory`]),
/// found or failed: a setup run, a check and System State's reading share one for this
/// long instead of each searching the home folder again. Check again searches anew
/// ([`search_again`]).
const INVENTORY_AGE: u64 = 5 * 60;

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
    /// The cargo home this process's environment names (`CARGO_HOME`, else `~/.cargo`),
    /// whose download caches (`registry`, `git`) carry the same cache tag as a build
    /// folder and are none. An app launched from the Dock has launchd's environment, never
    /// the `CARGO_HOME` a shell's startup file sets, so a cargo home is also recognised by
    /// what cargo keeps in it ([`CARGO_HOME_MARKS`]).
    cargo_home: PathBuf,
    /// The last probe's reading, which is System State's.
    reading: LastReading,
    /// The Spotlight search for project build folders, shared for [`INVENTORY_AGE`].
    inventory: Arc<Inventory>,
}

impl BuildNoindex {
    fn new(user: User, home: PathBuf, pkg: PathBuf, cargo_home: PathBuf) -> Self {
        Self {
            user,
            home,
            pkg,
            cargo_home,
            reading: LastReading::default(),
            inventory: Inventory::shared(),
        }
    }

    fn caches_parent(&self) -> PathBuf {
        self.home.join("Library/Caches")
    }

    fn cargo_build_noindex(&self) -> PathBuf {
        self.home.join("Library/Caches/aterm/cargo-build.noindex")
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
    /// the [`Inventory`] (`None` when Spotlight could not be asked, which costs only the
    /// row's note), less any gone since the search.
    fn spotlit_projects(&self, sys: &dyn System) -> Option<Vec<PathBuf>> {
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
        "Rust build caches and Trust toolchain sysroots stay in .noindex folders".into()
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
            to: "cargo-build.noindex and tooling.noindex".to_owned(),
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
mod tests {
    use super::*;
    use crate::engine::{self, NoJournal};
    use crate::items::defaults::tests::{ctx, user};
    use crate::model::{Mode, Outcome};
    use crate::port::{Fake, Ran};

    const ENV: &str = "alice$ CARGO_HOME=/Users//alice/.cargo RUSTUP_HOME=/Users//alice/.rustup /Users//alice/.cargo/bin/rustup";
    const STABLE: &str =
        "/Users//alice/.rustup/toolchains/stable-aarch64-apple-darwin/lib/rustlib/components";
    const PINNED: &str =
        "/Users//alice/.rustup/toolchains/1.97.1-aarch64-apple-darwin/lib/rustlib/components";
    const WITH_DOCS: &[u8] =
        b"cargo-aarch64-apple-darwin\nrust-docs-aarch64-apple-darwin\nrustc-aarch64-apple-darwin\n";
    const WITHOUT: &[u8] = b"cargo-aarch64-apple-darwin\nrustc-aarch64-apple-darwin\n";
    const STABLE_HTML: &str =
        "/Users//alice/.rustup/toolchains/stable-aarch64-apple-darwin/share/doc/rust/html";
    const STABLE_ASIDE: &str =
        "/Users//alice/.rustup/toolchains/stable-aarch64-apple-darwin/share/doc/rust/html.noindex";

    fn context() -> Ctx {
        ctx(&[user("alice", 501)])
    }

    fn run(sys: &Fake, mode: Mode) -> Vec<crate::model::ItemReport> {
        let alice = user("alice", 501);
        let rustup = Rustup {
            user: alice,
            cargo_home: "/Users//alice/.cargo".into(),
            rustup_home: "/Users//alice/.rustup".into(),
        };
        let items: Vec<Box<dyn Item>> = vec![
            Box::new(Profile(rustup.clone())),
            Box::new(NoDocs(rustup, LastReading::default())),
        ];
        engine::run(&items, mode, sys, &context(), &mut NoJournal)
    }

    /// A Mac whose rustup reports `profile` first, with stable's `rust-docs` HTML in place when
    /// its components record lists them.
    fn mac(profile: &str, stable: &[u8]) -> Fake {
        let sys = Fake::root();
        if stable == WITH_DOCS {
            sys.create_dir_all(Path::new(&format!("{STABLE_HTML}/std")), 0o755)
                .unwrap();
            sys.file(&format!("{STABLE_HTML}/std/index.html"), b"<html>");
        }
        sys.file("/Users//alice/.cargo/bin/rustup", b"")
            .on(&format!("{ENV} show profile"), Ran::ok(profile))
            .on(
                &format!("{ENV} toolchain list"),
                Ran::ok("stable-aarch64-apple-darwin\n1.97.1-aarch64-apple-darwin (active, default)\ntrust\n"),
            )
            .file(STABLE, stable)
            .file(PINNED, WITHOUT)
            .link(
                "/Users//alice/.rustup/toolchains/trust",
                "/Users//alice/Library/Application Support/aterm/pkg/rustup/trust",
            );
        sys
    }

    #[test]
    fn a_default_profile_and_stables_docs_are_planned_and_nothing_runs() {
        let sys = mac("default\n", WITH_DOCS);
        let rows = run(&sys, Mode::Plan);
        assert_eq!(rows[0].outcome, Outcome::Planned, "{rows:?}");
        assert_eq!(rows[1].outcome, Outcome::Planned, "{rows:?}");
        assert_eq!(
            rows[1].from.as_deref(),
            Some("rust-docs installed in stable-aarch64-apple-darwin")
        );
        assert_eq!(Reading::of(&rows[1]), Some(Reading::DocsInstalled));
        assert!(
            !sys.calls().iter().any(|c| c.contains(" set ")
                || c.contains(" remove ")
                || c.starts_with("rename ")
                || c.starts_with("symlink ")
                || c.starts_with("detach ")),
            "{:?}",
            sys.calls()
        );
    }

    /// Apply sets the profile, and keeps stable's docs out of Spotlight at once: the HTML is
    /// renamed `html.noindex` with a link to it in its place, and rustup's removal of the
    /// component, then of the HTML, starts in the background; no removal runs in the pass. Both
    /// rows read back done at once.
    #[test]
    fn apply_sets_the_profile_and_the_docs_aside_at_once_and_removes_them_in_the_background() {
        let sys = mac("default\n", WITH_DOCS);
        let set = format!("{ENV} set profile minimal");
        sys.on(&format!("{ENV} show profile"), Ran::ok("minimal\n"))
            .on(&set, Ran::ok(""));
        let rows = run(&sys, Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
        assert_eq!(rows[1].outcome, Outcome::Changed, "{rows:?}");
        let calls = sys.calls();
        assert_eq!(calls.iter().filter(|c| **c == set).count(), 1, "{calls:?}");
        let rename = format!("rename {STABLE_HTML} {STABLE_ASIDE}");
        let link = format!("symlink {STABLE_HTML} -> html.noindex");
        let removal = format!(
            "detach alice$ CARGO_HOME=/Users//alice/.cargo RUSTUP_HOME=/Users//alice/.rustup \
             /bin/sh -c {} sh /Users//alice/.cargo/bin/rustup stable-aarch64-apple-darwin \
             {STABLE_ASIDE} >> /dev/null",
            crate::port::shell_word(REMOVE_IN_BACKGROUND)
        );
        let at = |want: &str| calls.iter().position(|c| c == want);
        let (rename, link, removal) = (at(&rename), at(&link), at(&removal));
        assert!(
            rename.is_some() && rename < link && link < removal,
            "set aside, linked, then the removal in the background: {calls:?}"
        );
        assert!(
            !calls
                .iter()
                .any(|c| c.starts_with(ENV) && c.contains(" component remove ")),
            "no removal runs in the pass: {calls:?}"
        );
        assert!(
            !calls
                .iter()
                .any(|c| c.contains("--toolchain trust") || c.contains("1.97.1")),
            "{calls:?}"
        );
        assert_eq!(
            sys.read_file(Path::new(&format!("{STABLE_ASIDE}/std/index.html")))
                .unwrap(),
            b"<html>"
        );
        // Read again, the docs set aside are kept out and nothing runs again.
        let again = run(&sys, Mode::Apply);
        assert_eq!(again[1].outcome, Outcome::Ok, "{again:?}");
        assert_eq!(Reading::of(&again[1]), None);
        assert_eq!(
            sys.calls()
                .iter()
                .filter(|c| c.starts_with("detach "))
                .count(),
            1,
            "{:?}",
            sys.calls()
        );
    }

    /// An aside an earlier run left is not this run's to remove: the row fails, says where it
    /// is, and moves nothing.
    #[test]
    fn an_aside_already_there_fails_the_row_and_moves_nothing() {
        let sys = mac("minimal\n", WITH_DOCS);
        sys.create_dir_all(Path::new(STABLE_ASIDE), 0o755).unwrap();
        let rows = run(&sys, Mode::Apply);
        assert_eq!(rows[1].outcome, Outcome::Failed, "{rows:?}");
        assert!(
            rows[1]
                .why
                .as_deref()
                .is_some_and(|why| why.contains(STABLE_ASIDE)),
            "{rows:?}"
        );
        assert!(
            !sys.calls().iter().any(|c| c.starts_with("rename ")
                || c.starts_with("symlink ")
                || c.starts_with("detach ")
                || c.contains(" component remove ")),
            "{:?}",
            sys.calls()
        );
    }

    #[test]
    fn a_minimal_mac_is_ok_and_one_without_rustup_has_nothing_to_do() {
        let sys = mac("minimal\n", WITHOUT);
        let rows = run(&sys, Mode::Apply);
        assert!(rows.iter().all(|r| r.outcome == Outcome::Ok), "{rows:?}");
        let bare = Fake::root();
        let rows = run(&bare, Mode::Apply);
        assert!(
            rows.iter().all(|r| r.outcome == Outcome::NotApplicable),
            "{rows:?}"
        );
        assert!(bare.calls().is_empty(), "{:?}", bare.calls());
    }

    const MDFIND: &str =
        "alice$ /usr/bin/mdfind -onlyin /Users//alice -0 'kMDItemFSName == \"CACHEDIR.TAG\"'";
    const TARGET: &str = "/Users//alice/work/demo/target";
    /// The cache tag cargo writes at the top of a build folder.
    const CARGO_TAG: &[u8] = b"Signature: 8a477f597d28d172789f06886806bc55\n# This file is a cache directory tag created by cargo.\n# For information about cache directory tags see https://bford.info/cachedir/\n";

    /// A scratch atpkg prefix: the item reads its store and rustup views on disk, by the
    /// layout contract (`aterm_toolchain::layout`), never through the fake.
    struct Pkg(aterm_tempfile::TempDir);

    impl Pkg {
        fn new() -> Self {
            Self(aterm_tempfile::tempdir().unwrap())
        }

        fn path(&self) -> &Path {
            self.0.path()
        }

        /// `rustup/<name>` or `store/trust/<n>`, with its Rust sysroot.
        fn plain(&self, at: &str) -> String {
            let dir = self.path().join(at);
            std::fs::create_dir_all(dir.join("lib/rustlib")).unwrap();
            dir.display().to_string()
        }

        /// `<at>.noindex` with `<at>` a link to it, as atpkg lays one aside.
        fn aside(&self, at: &str) {
            let dir = self.path().join(at);
            let name = dir.file_name().unwrap().to_str().unwrap().to_owned();
            std::fs::create_dir_all(
                dir.with_file_name(format!("{name}.noindex"))
                    .join("lib/rustlib"),
            )
            .unwrap();
            std::os::unix::fs::symlink(format!("{name}.noindex"), &dir).unwrap();
        }

        /// `store/trust/current`, a link to build `n`.
        fn current(&self, n: &str) {
            std::os::unix::fs::symlink(n, self.path().join("store/trust/current")).unwrap();
        }

        fn items(&self) -> Vec<Box<dyn Item>> {
            vec![Box::new(BuildNoindex::new(
                user("alice", 501),
                "/Users//alice".into(),
                self.path().to_path_buf(),
                "/Users//alice/.cargo".into(),
            ))]
        }
    }

    /// A Mac with aterm's caches made and `found`, each a folder holding cargo's cache tag,
    /// with what Spotlight's search for them answers.
    fn with_build_folders(found: &[&str]) -> (Fake, Ran) {
        let sys = Fake::root();
        for name in ["cargo-build.noindex", "tooling.noindex"] {
            sys.create_dir_all(
                &Path::new("/Users//alice/Library/Caches/aterm").join(name),
                0o755,
            )
            .unwrap();
        }
        let mut reply = String::new();
        for folder in found {
            sys.create_dir_all(Path::new(&format!("{folder}/debug")), 0o755)
                .unwrap();
            sys.file(&format!("{folder}/CACHEDIR.TAG"), CARGO_TAG);
            reply.push_str(&format!("{folder}/CACHEDIR.TAG\0"));
        }
        sys.file("/usr/bin/mdfind", b"");
        (sys, Ran::ok(&reply))
    }

    /// [`with_build_folders`], whose search finds them.
    fn spotlit(found: &[&str]) -> Fake {
        let (sys, found) = with_build_folders(found);
        sys.on(MDFIND, found);
        sys
    }

    /// A Mac whose caches the item makes are `made`, with one project `target` of the
    /// person's that Spotlight indexes.
    fn with_a_spotlit_project(made: bool) -> Fake {
        let sys = Fake::root();
        sys.create_dir_all(Path::new("/Users//alice/Library/Caches"), 0o755)
            .unwrap();
        if made {
            for name in ["cargo-build.noindex", "tooling.noindex"] {
                sys.create_dir_all(
                    &Path::new("/Users//alice/Library/Caches/aterm").join(name),
                    0o755,
                )
                .unwrap();
            }
        }
        sys.create_dir_all(Path::new(&format!("{TARGET}/debug")), 0o755)
            .unwrap();
        sys.file(&format!("{TARGET}/CACHEDIR.TAG"), CARGO_TAG);
        sys.file("/usr/bin/mdfind", b"")
            .on(MDFIND, Ran::ok(&format!("{TARGET}/CACHEDIR.TAG\0")));
        sys
    }

    /// The item on a Mac with no atpkg prefix.
    fn alice_builds() -> Vec<Box<dyn Item>> {
        cargo_home_builds("/Users//alice/.cargo")
    }

    /// [`alice_builds`] with `CARGO_HOME` at `cargo_home`.
    fn cargo_home_builds(cargo_home: &str) -> Vec<Box<dyn Item>> {
        vec![Box::new(BuildNoindex::new(
            user("alice", 501),
            "/Users//alice".into(),
            "/Users//alice/Library/Application Support/aterm/pkg".into(),
            cargo_home.into(),
        ))]
    }

    /// No rename and no link: what the item never does to a folder.
    fn moved_nothing(sys: &Fake) {
        assert!(
            !sys.calls()
                .iter()
                .any(|c| c.starts_with("rename ") || c.contains("/bin/ln")),
            "{:#?}",
            sys.calls()
        );
    }

    /// The Rust in Spotlight row these rows read as.
    fn row_of(rows: &[crate::model::ItemReport]) -> aterm_types::macos_setup::Observation {
        let refs: Vec<&crate::model::ItemReport> = rows.iter().collect();
        crate::init_observe::observation(aterm_types::macos_setup::Setting::Spotlight, &refs)
    }

    /// The item makes aterm's two caches and nothing else: the Trust toolchain build in use,
    /// left plain in the store, is atpkg's move to make at its next update, so the plan
    /// names it in its line and promises only the caches, and the build stays as it is.
    #[test]
    fn build_noindex_makes_its_caches_and_only_notes_the_toolchain_build_in_use() {
        let sys = Fake::root();
        sys.file("/usr/bin/mdfind", b"").on(MDFIND, Ran::ok(""));
        sys.create_dir_all(Path::new("/Users//alice/Library/Caches"), 0o755)
            .unwrap();
        let pkg = Pkg::new();
        let build = pkg.plain("store/trust/15698");
        pkg.current("15698");
        let items = pkg.items();
        let planned = engine::run(&items, Mode::Plan, &sys, &context(), &mut NoJournal);
        assert_eq!(planned[0].outcome, Outcome::Planned, "{planned:?}");
        assert_eq!(
            Reading::of(&planned[0]),
            Some(Reading::BuildsIndexed {
                projects: 0,
                toolchain: true,
            })
        );
        assert!(
            planned[0]
                .from
                .as_deref()
                .is_some_and(|f| f.contains("cargo-build.noindex absent") && f.contains(&build)),
            "{planned:?}"
        );
        assert_eq!(
            planned[0].to.as_deref(),
            Some("cargo-build.noindex and tooling.noindex"),
            "the plan promises only what apply makes"
        );
        let rows = engine::run(&items, Mode::Apply, &sys, &context(), &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
        assert_eq!(rows[0].stood_aside, None);
        assert!(
            std::fs::symlink_metadata(&build).is_ok_and(|m| m.is_dir()),
            "the build stays where atpkg left it"
        );
        moved_nothing(&sys);
        assert!(
            sys.stat(Path::new(
                "/Users//alice/Library/Caches/aterm/cargo-build.noindex"
            ))
            .is_ok_and(|s| s.dir)
        );
    }

    /// A Mac with the Trust toolchain atpkg installed: the build in use, left plain in the
    /// store, is aterm's own move still to come, a quiet note on an in-place row and never
    /// Needs you, on every pass ("no user required"), and the stage finishes. A `<n>` that
    /// is a link to `<n>.noindex`, as atpkg now installs one, is kept out already and noted
    /// by no one.
    #[test]
    fn an_atpkg_toolchain_build_is_a_note_and_never_needs_you() {
        let sys = with_a_spotlit_project(true);
        let pkg = Pkg::new();
        let build = pkg.plain("store/trust/15698");
        pkg.current("15698");
        let items = pkg.items();
        for mode in [Mode::Plan, Mode::Apply, Mode::Apply, Mode::Plan] {
            let rows = engine::run(&items, mode, &sys, &context(), &mut NoJournal);
            assert_eq!(rows[0].outcome, Outcome::Ok, "{mode:?}: {rows:?}");
            assert_eq!(rows[0].stood_aside, None);
            assert_eq!(
                Reading::of(&rows[0]),
                Some(Reading::KeptOut {
                    projects: 1,
                    toolchain: true,
                })
            );
            let read = row_of(&rows);
            assert_eq!(
                read.standing,
                aterm_types::macos_setup::Standing::InPlace,
                "{mode:?}: {read:?}"
            );
            assert!(
                read.note.as_deref().is_some_and(|note| note.starts_with(
                    "aterm moves the Rust toolchain it is using out of Spotlight at its next \
                     update; Spotlight indexes 1 of your project build folders"
                )),
                "{read:?}"
            );
            assert!(
                read.detail.as_deref().is_some_and(|d| d.contains(&build)),
                "the build stands behind the note: {read:?}"
            );
        }
        moved_nothing(&sys);

        // atpkg's layout: `<n>.noindex`, with `<n>` a link to it.
        let linked = spotlit(&[]);
        let pkg = Pkg::new();
        pkg.aside("store/trust/15698");
        pkg.current("15698");
        let rows = engine::run(
            &pkg.items(),
            Mode::Apply,
            &linked,
            &context(),
            &mut NoJournal,
        );
        assert_eq!(rows[0].outcome, Outcome::Ok, "{rows:?}");
        assert_eq!(Reading::of(&rows[0]), None, "nothing to note");
        assert_eq!(row_of(&rows).note, None);
    }

    /// The person's indexed project `target` is a quiet note with the folder named in its detail,
    /// never a change, and is never renamed or linked whatever stands beside it.
    #[test]
    fn a_spotlit_project_folder_of_the_persons_is_a_note_and_never_touched() {
        let sys = with_a_spotlit_project(true);
        let items = alice_builds();
        for mode in [Mode::Plan, Mode::Apply] {
            let rows = engine::run(&items, mode, &sys, &context(), &mut NoJournal);
            assert_eq!(rows[0].outcome, Outcome::Ok, "{mode:?}: {rows:?}");
            assert_eq!(
                Reading::of(&rows[0]),
                Some(Reading::KeptOut {
                    projects: 1,
                    toolchain: false,
                })
            );
            let line = format!(
                "Spotlight indexes these project build folders of yours, which aterm leaves as they are: {TARGET}"
            );
            assert!(
                rows[0].to.as_deref().is_some_and(|to| to.ends_with(&line)),
                "{rows:?}"
            );
            let read = row_of(&rows);
            assert_eq!(
                read.standing,
                aterm_types::macos_setup::Standing::InPlace,
                "{read:?}"
            );
            assert_eq!(
                read.note.as_deref(),
                Some(crate::init_observe::projects_note(1).as_str())
            );
            assert!(
                read.detail.as_deref().is_some_and(|d| d.ends_with(&line)),
                "the folder stands behind the note: {read:?}"
            );
        }
        // A `.noindex` beside it changes nothing: it is still theirs, still indexed.
        let kept = format!("{TARGET}/retained");
        sys.file(&kept, b"existing build bytes");
        sys.create_dir_all(Path::new(&format!("{TARGET}.noindex")), 0o755)
            .unwrap();
        let rows = engine::run(&items, Mode::Apply, &sys, &context(), &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::Ok, "{rows:?}");
        assert_eq!(
            sys.read_file(Path::new(&kept)).unwrap(),
            b"existing build bytes"
        );
        assert!(sys.stat(Path::new(TARGET)).unwrap().dir);
        moved_nothing(&sys);

        // Spotlight not answering costs the note, never aterm's own caches.
        let census = |reply| {
            let sys = Fake::root();
            sys.create_dir_all(Path::new("/Users//alice/Library/Caches"), 0o755)
                .unwrap();
            sys.file("/usr/bin/mdfind", b"").on(MDFIND, reply);
            sys
        };
        let incomplete = census(Ran::ok(""));
        incomplete.refuse_complete_stdout(MDFIND, "complete capture unavailable");
        let missing = Fake::root();
        missing
            .create_dir_all(Path::new("/Users//alice/Library/Caches"), 0o755)
            .unwrap();
        for unread in [
            census(Ran::exit(1, "census failed")),
            census(Ran::ok("/Users//alice/work/demo/target/CACHEDIR.TAG")),
            incomplete,
            missing,
        ] {
            let items = alice_builds();
            let rows = engine::run(&items, Mode::Apply, &unread, &context(), &mut NoJournal);
            assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
            assert_eq!(
                Reading::of(&rows[0]),
                None,
                "made and read back in place, with no project counted"
            );
            assert!(
                unread
                    .stat(Path::new(
                        "/Users//alice/Library/Caches/aterm/cargo-build.noindex"
                    ))
                    .is_ok_and(|s| s.dir)
            );
        }
    }

    /// Spotlight is asked for build folders by `BUILD_MARKER`, and a search that fails (here, over
    /// the complete capture) stands its age like one that found.
    #[test]
    fn spotlight_is_asked_for_build_folders_and_a_failed_search_stands_its_age() {
        let sys = with_a_spotlit_project(true);
        let items = alice_builds();
        let searches = |sys: &Fake| sys.calls().iter().filter(|c| c.contains("mdfind")).count();
        let rows = engine::run(&items, Mode::Plan, &sys, &context(), &mut NoJournal);
        assert_eq!(
            sys.calls().iter().filter(|c| c.as_str() == MDFIND).count(),
            1,
            "{:#?}",
            sys.calls()
        );
        assert_eq!(
            Reading::of(&rows[0]),
            Some(Reading::KeptOut {
                projects: 1,
                toolchain: false,
            })
        );

        let mut big = String::new();
        let mut n = 0;
        while big.len() <= aterm_uds::deadline::OUTPUT_KEEP {
            big.push_str(&format!("/Users//alice/work/p{n:06}/target/CACHEDIR.TAG\0"));
            n += 1;
        }
        let mut over = Fake::root();
        for name in ["cargo-build.noindex", "tooling.noindex"] {
            over.create_dir_all(
                &Path::new("/Users//alice/Library/Caches/aterm").join(name),
                0o755,
            )
            .unwrap();
        }
        over.file("/usr/bin/mdfind", b"").on(MDFIND, Ran::ok(&big));
        let items = alice_builds();
        for _ in 0..3 {
            let rows = engine::run(&items, Mode::Plan, &over, &context(), &mut NoJournal);
            assert_eq!(rows[0].outcome, Outcome::Ok, "{rows:?}");
            assert_eq!(Reading::of(&rows[0]), None, "the note goes, nothing else");
        }
        assert_eq!(searches(&over), 1, "a failed search stands its age too");
        over.now += INVENTORY_AGE;
        engine::run(&items, Mode::Plan, &over, &context(), &mut NoJournal);
        assert_eq!(searches(&over), 2);
    }

    /// One search of the home folder stands for every probe for [`INVENTORY_AGE`]: a run's
    /// plan, apply and read-back, and the page's reading, ask Spotlight once.
    #[test]
    fn spotlight_is_searched_once_per_inventory_age() {
        let mut sys = with_a_spotlit_project(true);
        let items = alice_builds();
        let searches = |sys: &Fake| sys.calls().iter().filter(|c| c.contains("mdfind")).count();
        engine::run(&items, Mode::Apply, &sys, &context(), &mut NoJournal);
        engine::run(&items, Mode::Plan, &sys, &context(), &mut NoJournal);
        assert_eq!(searches(&sys), 1, "{:#?}", sys.calls());
        sys.now += INVENTORY_AGE - 1;
        engine::run(&items, Mode::Plan, &sys, &context(), &mut NoJournal);
        assert_eq!(searches(&sys), 1);
        sys.now += 1;
        let rows = engine::run(&items, Mode::Plan, &sys, &context(), &mut NoJournal);
        assert_eq!(
            searches(&sys),
            2,
            "an inventory past its age is taken again"
        );
        assert_eq!(
            Reading::of(&rows[0]),
            Some(Reading::KeptOut {
                projects: 1,
                toolchain: false,
            })
        );
        // A clock that went back reads again.
        sys.now -= 10;
        engine::run(&items, Mode::Plan, &sys, &context(), &mut NoJournal);
        assert_eq!(searches(&sys), 3);
    }

    /// Spotlight's build folders are the folders cargo tagged, whatever their names (a
    /// `CARGO_TARGET_DIR` or `--target-dir` names its own), each counted once: a build
    /// folder inside one found (a build script's own under its `OUT_DIR`) is part of it.
    /// NEGATIVE CONTROL: a folder another tool tagged is no build folder of cargo's, and
    /// a cargo build folder inside it still counts.
    #[test]
    fn a_cargo_build_folder_counts_once_whatever_its_name() {
        let read = |sys: &Fake| {
            let rows = engine::run(&alice_builds(), Mode::Plan, sys, &context(), &mut NoJournal);
            Reading::of(&rows[0])
        };
        let projects = |projects| {
            Some(Reading::KeptOut {
                projects,
                toolchain: false,
            })
        };
        assert_eq!(read(&spotlit(&["/Users//alice/brtgt"])), projects(1));
        let nested = format!("{TARGET}/debug/build/pmt-1/out/target");
        assert_eq!(read(&spotlit(&[TARGET, &nested])), projects(1));
        let beside = "/Users//alice/work/demo/target-main";
        assert_eq!(read(&spotlit(&[TARGET, &nested, beside])), projects(2));
        let other_tool = "/Users//alice/.cache/pip";
        let pip = |sys: &Fake| {
            sys.file(
                &format!("{other_tool}/CACHEDIR.TAG"),
                b"Signature: 8a477f597d28d172789f06886806bc55\n# This file is a cache directory tag created by pip.\n",
            );
        };
        let sys = spotlit(&[other_tool]);
        pip(&sys);
        assert_eq!(read(&sys), None, "nothing of cargo's to note");
        let inside = format!("{other_tool}/target");
        let sys = spotlit(&[other_tool, &inside]);
        pip(&sys);
        assert_eq!(read(&sys), projects(1));
    }

    /// Cargo tags its download caches in its home (`registry`, `git`) exactly as it tags a
    /// build folder. NEGATIVE CONTROL: with a visible `CARGO_HOME`, which Spotlight indexes,
    /// they are cargo's own and never counted as the person's project build folders; a
    /// project's beside them still is. A cargo home that holds the home folder leaves out
    /// only those caches.
    #[test]
    fn cargos_download_caches_are_no_project_build_folders() {
        let read = |items: Vec<Box<dyn Item>>, sys: &Fake| {
            let rows = engine::run(&items, Mode::Plan, sys, &context(), &mut NoJournal);
            Reading::of(&rows[0])
        };
        let caches = ["/Users//alice/cargo/registry", "/Users//alice/cargo/git"];
        let sys = spotlit(&caches);
        assert_eq!(
            read(cargo_home_builds("/Users//alice/cargo"), &sys),
            None,
            "nothing of the person's to note"
        );
        let sys = spotlit(&[caches[0], caches[1], TARGET]);
        assert_eq!(
            read(cargo_home_builds("/Users//alice/cargo"), &sys),
            Some(Reading::KeptOut {
                projects: 1,
                toolchain: false,
            })
        );
        let caches = ["/Users//alice/registry", "/Users//alice/git"];
        let sys = spotlit(&[caches[0], caches[1], TARGET]);
        assert_eq!(
            read(cargo_home_builds("/Users//alice"), &sys),
            Some(Reading::KeptOut {
                projects: 1,
                toolchain: false,
            })
        );
    }

    /// System State reads inside the app, whose environment a launch from the Dock gives
    /// it names no `CARGO_HOME`: a visible cargo home a shell's startup file names
    /// (`~/cargo`) is still cargo's, by the files cargo keeps at its top, whichever it
    /// wrote (`.package-cache`, `.global-cache`), and its download caches are no project
    /// build folders. So are those of `~/.cargo` linked to a visible folder, which
    /// Spotlight reports by the folder's own path. NEGATIVE CONTROLS: a `registry` and a
    /// `git` cargo tagged beside neither (a target folder of that name) are build folders
    /// like any other, and a project's beside the caches always counts.
    #[test]
    fn a_cargo_home_the_environment_does_not_name_is_still_cargos() {
        let read = |sys: &Fake| {
            let rows = engine::run(&alice_builds(), Mode::Plan, sys, &context(), &mut NoJournal);
            Reading::of(&rows[0])
        };
        let projects = |projects| {
            Some(Reading::KeptOut {
                projects,
                toolchain: false,
            })
        };
        let visible = ["/Users//alice/cargo/registry", "/Users//alice/cargo/git"];
        for mark in [".package-cache", ".global-cache"] {
            let sys = spotlit(&[visible[0], visible[1], TARGET]);
            sys.file(&format!("/Users//alice/cargo/{mark}"), b"");
            assert_eq!(read(&sys), projects(1), "{mark}");
        }
        let sys = spotlit(&[visible[0], visible[1], TARGET]);
        assert_eq!(read(&sys), projects(3), "nothing of cargo's beside them");
        let real = [
            "/Users//alice/dev/cargo/registry",
            "/Users//alice/dev/cargo/git",
        ];
        let sys = spotlit(&[real[0], real[1], TARGET]);
        sys.link("/Users//alice/.cargo", "/Users//alice/dev/cargo");
        assert_eq!(read(&sys), projects(1), "~/.cargo is a link to them");
        let sys = spotlit(&[real[0], real[1], TARGET]);
        assert_eq!(
            read(&sys),
            projects(3),
            "no link, nothing of cargo's beside them"
        );
    }

    /// Check again ([`search_again`]) never waits on a search under way, which holds the
    /// inventory's lock for as long as Spotlight takes (a minute, or more): the window that
    /// presses it stays live. The search under way began before it, so the next reading
    /// searches anew. The wait here is a hang detector: the stand-in search is held on a
    /// channel until the forget has returned.
    #[test]
    fn check_again_never_waits_on_a_search_under_way() {
        use std::sync::mpsc;
        const HANG: std::time::Duration = std::time::Duration::from_secs(60);
        let home = Path::new("/Users//alice");
        let inventory = Arc::new(Inventory::default());
        let (started, searching) = mpsc::channel();
        let (release, held) = mpsc::channel::<()>();
        let search = {
            let inventory = Arc::clone(&inventory);
            std::thread::spawn(move || {
                inventory.read(home, 0, || {
                    started.send(()).unwrap();
                    held.recv().unwrap();
                    Ok(BTreeSet::new())
                })
            })
        };
        searching.recv_timeout(HANG).expect("the search began");
        let (forgot, forgetting) = mpsc::channel();
        let forget = {
            let inventory = Arc::clone(&inventory);
            std::thread::spawn(move || {
                inventory.forget();
                forgot.send(()).unwrap();
            })
        };
        let returned = forgetting.recv_timeout(HANG);
        release.send(()).unwrap();
        search.join().unwrap().unwrap();
        forget.join().unwrap();
        returned.expect("Check again waited on the search under way");
        let searched = std::cell::Cell::new(false);
        inventory
            .read(home, 0, || {
                searched.set(true);
                Ok(BTreeSet::new())
            })
            .unwrap();
        assert!(searched.get(), "a search begun before Check again is stale");
    }

    /// The build `store/trust/current` names is the one in use, which atpkg never moves
    /// until an update makes another current, and the rustup view laid from it moves with
    /// the next update too: one note says so, never "older". A plain build no longer in
    /// use, atpkg's to move at its next pass, goes unsaid. With nothing plain left, the row
    /// says the toolchain is kept out, with no note.
    #[test]
    fn only_the_toolchain_in_use_is_noted() {
        let sys = spotlit(&[]);
        let read = |pkg: &Pkg| {
            let rows = engine::run(&pkg.items(), Mode::Plan, &sys, &context(), &mut NoJournal);
            assert_eq!(rows[0].outcome, Outcome::Ok, "{rows:?}");
            (row_of(&rows), rows[0].to.clone().unwrap_or_default())
        };
        let pkg = Pkg::new();
        let older = pkg.plain("store/trust/15600");
        let build = pkg.plain("store/trust/15698");
        pkg.current("15698");
        let view = pkg.plain("rustup/trust");
        let (row, line) = read(&pkg);
        assert_eq!(row.note.as_deref(), Some(TOOLCHAIN_NOTE), "{row:?}");
        assert!(
            line.contains(&build) && line.contains(&view) && !line.contains(&older),
            "{line}"
        );
        assert!(line.contains(&format!("{TOOLCHAIN_NOTE}: ")), "{line}");

        // The build in use laid aside, its view still plain: the view is the note.
        let pkg = Pkg::new();
        pkg.plain("store/trust/15600");
        pkg.aside("store/trust/15698");
        pkg.current("15698");
        let view = pkg.plain("rustup/trust");
        let (row, line) = read(&pkg);
        assert_eq!(row.note.as_deref(), Some(TOOLCHAIN_NOTE), "{row:?}");
        assert!(line.contains(&view), "{line}");

        // Both laid aside: the older plain build is no note, and no "kept out" is claimed.
        let pkg = Pkg::new();
        pkg.plain("store/trust/15600");
        pkg.aside("store/trust/15698");
        pkg.current("15698");
        pkg.aside("rustup/trust");
        let (row, line) = read(&pkg);
        assert_eq!(row.note, None, "{row:?}");
        assert_eq!(line, "cargo-build.noindex and tooling.noindex are .noindex");

        // Nothing plain: kept out, no note.
        std::fs::remove_dir_all(pkg.path().join("store/trust/15600")).unwrap();
        let (row, line) = read(&pkg);
        assert_eq!(row.note, None, "{row:?}");
        assert_eq!(
            line,
            "cargo-build.noindex, tooling.noindex, and the Trust toolchain are .noindex"
        );
    }

    /// Check again searches Spotlight anew ([`search_again`]): a search kept from minutes
    /// ago, found or failed, would leave the row's note as it was.
    #[test]
    fn check_again_searches_spotlight_anew() {
        let (sys, found) = with_build_folders(&[TARGET]);
        sys.on(MDFIND, Ran::exit(1, "Spotlight is rebuilding its index"))
            .on(MDFIND, found);
        let item = BuildNoindex {
            inventory: SHARED.get_or_init(Arc::default).clone(),
            ..BuildNoindex::new(
                user("alice", 501),
                "/Users//alice".into(),
                "/Users//alice/Library/Application Support/aterm/pkg".into(),
                "/Users//alice/.cargo".into(),
            )
        };
        let items: Vec<Box<dyn Item>> = vec![Box::new(item)];
        let read = || {
            let rows = engine::run(&items, Mode::Plan, &sys, &context(), &mut NoJournal);
            Reading::of(&rows[0])
        };
        let searches = || sys.calls().iter().filter(|c| c.as_str() == MDFIND).count();
        assert_eq!(read(), None, "the search failed");
        assert_eq!((read(), searches()), (None, 1), "and stands its age");
        search_again();
        assert_eq!(
            (read(), searches()),
            (
                Some(Reading::KeptOut {
                    projects: 1,
                    toolchain: false,
                }),
                2
            )
        );
    }
}
