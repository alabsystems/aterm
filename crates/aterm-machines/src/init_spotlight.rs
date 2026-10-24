// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Rustup minimal-profile and `rust-docs` cleanup to keep toolchains out of Spotlight.

use crate::init_observe::{LastReading, Reading};
use crate::items::{Ctx, Item};
use crate::model::{Probe, User};
use crate::port::{Cmd, System};
use aterm_converge::StoodAside;
use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

/// The rustup profile that installs no documentation.
const MINIMAL: &str = "minimal";

/// Keep this login's Rust toolchains from feeding Spotlight.
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
    Ok(vec![
        Box::new(Profile(rustup.clone())),
        Box::new(NoDocs(rustup, LastReading::default())),
        Box::new(BuildNoindex::new(user.clone(), home.to_path_buf())),
    ])
}

/// How long one Spotlight search of a home folder for build folders stands ([`Inventory`]):
/// it reads every build artifact Spotlight holds there, so a setup run, a check and System
/// State's reading share one for this long instead of each searching the home folder again.
const INVENTORY_AGE: u64 = 5 * 60;

/// The last Spotlight search for build folders: the home it searched, when (Unix seconds),
/// and the `target` and `build` folders it found indexed.
#[derive(Debug, Default)]
struct Inventory(Mutex<Option<(PathBuf, u64, BTreeSet<PathBuf>)>>);

impl Inventory {
    /// The process's one inventory; a test's item keeps its own.
    fn shared() -> Arc<Self> {
        #[cfg(not(test))]
        {
            static SHARED: std::sync::OnceLock<Arc<Inventory>> = std::sync::OnceLock::new();
            SHARED.get_or_init(Arc::default).clone()
        }
        #[cfg(test)]
        {
            Arc::default()
        }
    }

    /// The folders a search of `home` taken at most [`INVENTORY_AGE`] before `now` found,
    /// else `search`'s, kept for the next reader. A search that fails is kept by no one.
    fn read(
        &self,
        home: &Path,
        now: u64,
        search: impl FnOnce() -> Result<BTreeSet<PathBuf>, String>,
    ) -> Result<BTreeSet<PathBuf>, String> {
        let mut kept = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((at_home, at, found)) = kept.as_ref()
            && at_home == home
            && *at <= now
            && now - at < INVENTORY_AGE
        {
            return Ok(found.clone());
        }
        let found = search()?;
        *kept = Some((home.to_path_buf(), now, found.clone()));
        Ok(found)
    }
}

/// Keep aterm's cargo build cache and tooling cache in `.noindex` folders, and name the
/// Trust toolchain builds Spotlight still indexes for the person to move. It never moves
/// them itself: a program someone started from one may still be under macOS's first-exec
/// check, and syspolicyd crashes when that file's folder is renamed mid-check. A kill
/// leaves no process and a run changes no folder, so nothing it could read shows that
/// check is over.
///
/// The person's own project build folders (`target`, `build`) that Spotlight indexes are
/// theirs, never aterm's to rename or link: the row only counts them in a quiet note
/// ([`Reading::ProjectsIndexed`]) and lists them in its line, and they are no step, no
/// change and nothing the run waits on. A failed search for them costs only the note.
#[derive(Debug)]
struct BuildNoindex {
    user: User,
    home: PathBuf,
    /// The folders the last apply named for the person to move.
    aside: Mutex<Option<StoodAside>>,
    /// The last probe's reading, which is System State's.
    reading: LastReading,
    /// The Spotlight search for project build folders, shared for [`INVENTORY_AGE`].
    inventory: Arc<Inventory>,
}

impl BuildNoindex {
    fn new(user: User, home: PathBuf) -> Self {
        Self {
            user,
            home,
            aside: Mutex::new(None),
            reading: LastReading::default(),
            inventory: Inventory::shared(),
        }
    }

    fn set_aside(&self, why: Option<StoodAside>) {
        *self
            .aside
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = why;
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

    fn trust_store(&self) -> PathBuf {
        self.home
            .join("Library/Application Support/aterm/pkg/store/trust")
    }

    /// Numeric build directories in `store/trust/<build>` not yet symlinked to `<build>.noindex`.
    fn plain_trust_builds(&self, sys: &dyn System) -> Result<Vec<String>, String> {
        let store = self.trust_store();
        let entries = match sys.list_dir(&store) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(format!("{}: {e}", store.display())),
        };
        let mut plain = Vec::new();
        for name in entries {
            if name.is_empty() || !name.bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            let path = store.join(&name);
            match sys.stat(&path) {
                Ok(s) if s.dir && !s.symlink => {
                    let noindex = store.join(format!("{name}.noindex"));
                    if Self::present(sys, &noindex)? {
                        return Err(format!(
                            "both {} and {} exist; left unchanged",
                            path.display(),
                            noindex.display()
                        ));
                    }
                    plain.push(name);
                }
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("{}: {e}", path.display())),
            }
        }
        plain.sort();
        Ok(plain)
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

    /// What Spotlight indexes that is the person's to move: the plain Trust toolchain
    /// builds.
    fn to_move(&self, sys: &dyn System) -> Result<Vec<PathBuf>, String> {
        let store = self.trust_store();
        Ok(self
            .plain_trust_builds(sys)?
            .iter()
            .map(|name| store.join(name))
            .collect())
    }

    /// `folders`, comma-separated.
    fn listed(folders: &[PathBuf]) -> String {
        let names: Vec<String> = folders.iter().map(|d| d.display().to_string()).collect();
        names.join(", ")
    }

    /// The line that names `folders` for the person to move.
    fn for_the_person(folders: &[PathBuf]) -> String {
        format!(
            "{} stay where Spotlight indexes them, since macOS may still be checking a program \
             someone started from one; rename each to its .noindex name and link the old name \
             to it once nothing has been started from it for {} minutes of awake time, or after \
             a restart",
            Self::listed(folders),
            crate::first_exec::minutes(aterm_uds::deadline::FIRST_EXEC_HANG)
        )
    }

    /// The person's own Cargo/Rust `target` or `build` folders under `self.home` that
    /// Spotlight indexes, from the [`Inventory`] (`None` when Spotlight could not be
    /// asked, which costs only the row's note).
    fn spotlit_projects(&self, sys: &dyn System) -> Option<Vec<PathBuf>> {
        let indexed = self
            .inventory
            .read(&self.home, sys.now(), || self.search(sys))
            .ok()?;
        let mut out = Vec::new();
        for cand in indexed {
            let built = |name: &str| Self::present(sys, &cand.join(name));
            if Self::dir_present(sys, &cand).ok()?
                && (built("CACHEDIR.TAG").ok()? || built("debug").ok()? || built("release").ok()?)
            {
                out.push(cand);
            }
        }
        Some(out)
    }

    /// Ask Spotlight which `target` or `build` folders under `self.home` it indexes.
    fn search(&self, sys: &dyn System) -> Result<BTreeSet<PathBuf>, String> {
        let mdfind_bin = Path::new("/usr/bin/mdfind");
        sys.stat(mdfind_bin)
            .map_err(|e| format!("{}: {e}", mdfind_bin.display()))?;
        let cmd = Cmd::new(mdfind_bin).as_user(&self.user).args([
            "-onlyin",
            &self.home.to_string_lossy(),
            "-0",
            "kMDItemFSName == \"*.rmeta\"",
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
        let home_prefix = format!("{}/", self.home.to_string_lossy());
        let mut candidates = BTreeSet::new();
        for raw in text.split('\0').filter(|p| !p.is_empty()) {
            if raw.split('/').any(|c| c.ends_with(".noindex")) {
                continue;
            }
            let Some(rel) = raw.strip_prefix(&home_prefix) else {
                continue;
            };
            let parts: Vec<&str> = rel.split('/').collect();
            if parts.len() < 2 {
                continue;
            }
            for target_name in ["target", "build"] {
                if let Some(idx) = parts[..parts.len() - 1]
                    .iter()
                    .position(|c| *c == target_name)
                {
                    let mut root = self.home.clone();
                    for seg in &parts[..=idx] {
                        root.push(seg);
                    }
                    candidates.insert(root);
                    break;
                }
            }
        }
        Ok(candidates)
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
        let has_store = match Self::dir_present(sys, &self.trust_store()) {
            Ok(v) => v,
            Err(why) => return Probe::Unusable { why },
        };
        if !has_caches && !has_store {
            return Probe::NotApplicable {
                why: format!(
                    "neither {} nor {} exists",
                    self.caches_parent().display(),
                    self.trust_store().display()
                ),
            };
        }
        let missing = match self.missing_caches(sys) {
            Ok(missing) => missing,
            Err(why) => return Probe::Unusable { why },
        };
        let folders = match self.to_move(sys) {
            Ok(folders) => folders,
            Err(why) => return Probe::Unusable { why },
        };
        let projects = self.spotlit_projects(sys).unwrap_or_default();
        let indexed = Reading::BuildsIndexed {
            projects: projects.len(),
        };
        if missing.is_empty() {
            return if folders.is_empty() {
                let mut now =
                    "cargo-build.noindex, tooling.noindex, and trust store builds are .noindex"
                        .to_owned();
                if !projects.is_empty() {
                    self.reading.set(Some(Reading::ProjectsIndexed {
                        count: projects.len(),
                    }));
                    now.push_str(&format!("; {}", Self::projects_line(&projects)));
                }
                Probe::Ok { now }
            } else {
                self.reading.set(Some(indexed));
                Probe::Person {
                    step: "spotlight.build-folders".into(),
                    how: Self::for_the_person(&folders),
                }
            };
        }
        self.reading.set(Some(indexed));
        let mut from: Vec<String> = missing
            .iter()
            .filter_map(|dir| dir.file_name())
            .map(|name| format!("{} absent", name.to_string_lossy()))
            .collect();
        let mut to = "cargo-build.noindex and tooling.noindex".to_owned();
        if !folders.is_empty() {
            let listed = Self::listed(&folders);
            from.push(format!("{listed} indexed by Spotlight"));
            to.push_str(&format!(", with {listed} left for you to move"));
        }
        if !projects.is_empty() {
            from.push(Self::projects_line(&projects));
        }
        Probe::Change {
            from: from.join("; "),
            to,
        }
    }

    fn detail(&self, _sys: &dyn System, _ctx: &Ctx) -> Option<String> {
        self.reading.detail()
    }

    /// Make the missing caches; name the Trust toolchain builds Spotlight indexes for the
    /// person and stand aside on them ([`StoodAside::Waiting`]), since only they can tell
    /// when nothing runs from one. The person's own project folders are not its to touch.
    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        self.set_aside(None);
        // Admit the store's inventory before creating any cache directory.
        let folders = self.to_move(sys)?;
        for dir in self.missing_caches(sys)? {
            sys.create_dir_all(&dir, 0o755)
                .map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        if !folders.is_empty() {
            self.set_aside(Some(StoodAside::Waiting(Self::for_the_person(&folders))));
        }
        Ok(())
    }

    fn stood_aside(&self) -> Option<StoodAside> {
        self.aside
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
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

    /// The toolchains rustup installed that carry `rust-docs`.
    fn with_docs(&self, sys: &dyn System) -> Result<Vec<String>, String> {
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
                out.push(name.to_owned());
            }
        }
        Ok(out)
    }
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
        match self.0.with_docs(sys) {
            Err(why) => Probe::Unusable { why },
            Ok(docs) if docs.is_empty() => Probe::Ok {
                now: "no rustup toolchain carries rust-docs".into(),
            },
            Ok(docs) => {
                self.1.set(Some(Reading::DocsInstalled));
                Probe::Change {
                    from: format!("rust-docs installed in {}", docs.join(", ")),
                    to: "removed".into(),
                }
            }
        }
    }

    fn detail(&self, _sys: &dyn System, _ctx: &Ctx) -> Option<String> {
        self.1.detail()
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        for name in self.0.with_docs(sys)? {
            self.0.read(
                sys,
                &["component", "remove", "rust-docs", "--toolchain", &name],
            )?;
        }
        Ok(())
    }

    fn revert(&self) -> Option<String> {
        Some(format!(
            "{} component add rust-docs --toolchain <each toolchain it was removed from>",
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

    /// A Mac whose rustup reports `profile` first.
    fn mac(profile: &str, stable: &[u8]) -> Fake {
        let sys = Fake::root();
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
            !sys.calls()
                .iter()
                .any(|c| c.contains(" set ") || c.contains(" remove ")),
            "{:?}",
            sys.calls()
        );
    }

    #[test]
    fn apply_sets_the_profile_removes_the_docs_and_reads_both_back() {
        let sys = mac("default\n", WITH_DOCS);
        let set = format!("{ENV} set profile minimal");
        let remove =
            format!("{ENV} component remove rust-docs --toolchain stable-aarch64-apple-darwin");
        sys.on(&format!("{ENV} show profile"), Ran::ok("minimal\n"))
            .on(&set, Ran::ok(""))
            .on(&remove, Ran::ok(""))
            .on_write(&remove, Path::new(STABLE), WITHOUT);
        let rows = run(&sys, Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
        assert_eq!(rows[1].outcome, Outcome::Changed, "{rows:?}");
        let calls = sys.calls();
        assert_eq!(calls.iter().filter(|c| **c == set).count(), 1, "{calls:?}");
        assert_eq!(
            calls.iter().filter(|c| **c == remove).count(),
            1,
            "{calls:?}"
        );
        assert!(
            !calls
                .iter()
                .any(|c| c.contains("--toolchain trust") || c.contains("--toolchain 1.97.1")),
            "{calls:?}"
        );
    }

    #[test]
    fn a_removal_that_does_not_read_back_fails() {
        let sys = mac("minimal\n", WITH_DOCS);
        let remove =
            format!("{ENV} component remove rust-docs --toolchain stable-aarch64-apple-darwin");
        sys.on(&remove, Ran::ok(""));
        let rows = run(&sys, Mode::Apply);
        assert_eq!(rows[1].outcome, Outcome::Failed, "{rows:?}");
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

    #[test]
    fn build_noindex_plans_cache_dirs_and_refuses_a_trust_build_beside_its_noindex() {
        let alice = user("alice", 501);
        let item = BuildNoindex::new(alice, "/Users//alice".into());
        let sys = Fake::root();
        sys.file("/usr/bin/mdfind", b"").on(
            "alice$ /usr/bin/mdfind -onlyin /Users//alice -0 'kMDItemFSName == \"*.rmeta\"'",
            Ran::ok(""),
        );
        sys.create_dir_all(Path::new("/Users//alice/Library/Caches"), 0o755)
            .unwrap();
        sys.create_dir_all(
            Path::new("/Users//alice/Library/Application Support/aterm/pkg/store/trust"),
            0o755,
        )
        .unwrap();
        sys.file(
            "/Users//alice/Library/Application Support/aterm/pkg/store/trust/15698",
            b"",
        );
        sys.stat_of(
            "/Users//alice/Library/Application Support/aterm/pkg/store/trust/15698",
            crate::port::Stat {
                uid: 501,
                gid: 20,
                mode: 0o755,
                dir: true,
                symlink: false,
                len: 0,
            },
        );
        let items: Vec<Box<dyn Item>> = vec![Box::new(item)];
        let planned = engine::run(&items, Mode::Plan, &sys, &context(), &mut NoJournal);
        assert_eq!(planned[0].outcome, Outcome::Planned, "{planned:?}");
        assert_eq!(
            Reading::of(&planned[0]),
            Some(Reading::BuildsIndexed { projects: 0 })
        );
        assert!(
            planned[0]
                .from
                .as_deref()
                .is_some_and(|f| f.contains("cargo-build.noindex absent") && f.contains("15698")),
            "{planned:?}"
        );
        let build = "/Users//alice/Library/Application Support/aterm/pkg/store/trust/15698";
        let kept = format!("{build}/retained");
        sys.file(&kept, b"existing toolchain bytes");
        sys.create_dir_all(Path::new(&format!("{build}.noindex")), 0o755)
            .unwrap();
        assert!(matches!(
            items[0].probe(&sys, &context()),
            Probe::Unusable { .. }
        ));
        assert!(items[0].apply(&sys, &context()).is_err());
        assert_eq!(
            sys.read_file(Path::new(&kept)).unwrap(),
            b"existing toolchain bytes"
        );
        assert!(
            sys.stat(Path::new(
                "/Users//alice/Library/Caches/aterm/cargo-build.noindex"
            ))
            .is_err()
        );
    }

    const MDFIND: &str =
        "alice$ /usr/bin/mdfind -onlyin /Users//alice -0 'kMDItemFSName == \"*.rmeta\"'";
    const TARGET: &str = "/Users//alice/work/demo/target";

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
        sys.file(&format!("{TARGET}/CACHEDIR.TAG"), b"Signature:");
        sys.file("/usr/bin/mdfind", b"").on(
            MDFIND,
            Ran::ok(&format!("{TARGET}/debug/deps/libdemo.rmeta\0")),
        );
        sys
    }

    fn alice_builds() -> Vec<Box<dyn Item>> {
        vec![Box::new(BuildNoindex::new(
            user("alice", 501),
            "/Users//alice".into(),
        ))]
    }

    #[test]
    fn a_build_folder_a_program_may_still_run_from_is_the_persons_to_move_and_never_planned() {
        const STORE: &str = "/Users//alice/Library/Application Support/aterm/pkg/store/trust";
        let run_as = |sys: &Fake, mode: Mode| {
            engine::run(&alice_builds(), mode, sys, &context(), &mut NoJournal)
        };
        let run = |sys: &Fake| run_as(sys, Mode::Apply);
        let caches = |sys: &Fake| {
            ["cargo-build.noindex", "tooling.noindex"]
                .iter()
                .all(|name| {
                    sys.stat(&Path::new("/Users//alice/Library/Caches/aterm").join(name))
                        .is_ok_and(|s| s.dir)
                })
        };

        let sys = with_a_spotlit_project(false);
        sys.create_dir_all(Path::new(STORE), 0o755).unwrap();
        let build = format!("{STORE}/15698");
        sys.file(&build, b"").stat_of(
            &build,
            crate::port::Stat {
                uid: 501,
                gid: 20,
                mode: 0o755,
                dir: true,
                symlink: false,
                len: 0,
            },
        );
        let planned = run_as(&sys, Mode::Plan);
        assert_eq!(planned[0].outcome, Outcome::Planned, "{planned:?}");
        assert_eq!(
            planned[0].to.as_deref(),
            Some(
                format!(
                    "cargo-build.noindex and tooling.noindex, with {build} left for you to move"
                )
                .as_str()
            ),
            "the plan promises only what apply makes, and nothing of the person's project"
        );
        let rows = run(&sys);
        assert_eq!(rows[0].outcome, Outcome::Blocked, "{rows:?}");
        assert!(
            !sys.calls()
                .iter()
                .any(|c| c.starts_with("rename ") || c.contains("/bin/ln")),
            "{:#?}",
            sys.calls()
        );
        let Some(StoodAside::Waiting(why)) = &rows[0].stood_aside else {
            panic!("it waits on the person, not on another run's lock: {rows:?}");
        };
        assert!(
            why.starts_with(&format!("{build} stay where Spotlight indexes them"))
                && why.contains("10 minutes of awake time, or after a restart")
                && !why.contains(TARGET),
            "{why}"
        );
        assert!(caches(&sys), "the caches new builds use are made");
        for mode in [Mode::Plan, Mode::Apply] {
            let rows = run_as(&sys, mode);
            assert_eq!(rows[0].outcome, Outcome::Person, "{mode:?}: {rows:?}");
            assert_eq!(rows[0].why.as_deref(), Some(why.as_str()));
            assert_eq!(rows[0].stood_aside, None);
        }

        let bare = Fake::root();
        bare.create_dir_all(Path::new("/Users//alice/Library/Caches"), 0o755)
            .unwrap();
        bare.file("/usr/bin/mdfind", b"").on(MDFIND, Ran::ok(""));
        let rows = run(&bare);
        assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
        assert!(caches(&bare));
    }

    /// The person's own project `target` that Spotlight indexes is a quiet note on the Rust
    /// in Spotlight row, with the folder named behind it: not Needs you, not a change, and
    /// the run that reads it finishes. aterm never renames or links it, whatever stands
    /// beside it.
    #[test]
    fn a_spotlit_project_folder_of_the_persons_is_a_note_and_never_touched() {
        let sys = with_a_spotlit_project(true);
        let items = alice_builds();
        for mode in [Mode::Plan, Mode::Apply] {
            let rows = engine::run(&items, mode, &sys, &context(), &mut NoJournal);
            assert_eq!(rows[0].outcome, Outcome::Ok, "{mode:?}: {rows:?}");
            assert_eq!(
                Reading::of(&rows[0]),
                Some(Reading::ProjectsIndexed { count: 1 })
            );
            let line = format!(
                "Spotlight indexes these project build folders of yours, which aterm leaves as they are: {TARGET}"
            );
            assert!(
                rows[0].to.as_deref().is_some_and(|to| to.ends_with(&line)),
                "{rows:?}"
            );
            let refs: Vec<&crate::model::ItemReport> = rows.iter().collect();
            let read = crate::init_observe::observation(
                aterm_types::macos_setup::Setting::Spotlight,
                &refs,
            );
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
        assert!(
            !sys.calls()
                .iter()
                .any(|c| c.starts_with("rename ") || c.contains("/bin/ln")),
            "{:#?}",
            sys.calls()
        );

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
            census(Ran::ok(
                "/Users//alice/work/demo/target/debug/incomplete.rmeta",
            )),
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
            Some(Reading::ProjectsIndexed { count: 1 })
        );
        // A clock that went back reads again.
        sys.now -= 10;
        engine::run(&items, Mode::Plan, &sys, &context(), &mut NoJournal);
        assert_eq!(searches(&sys), 3);
    }
}
