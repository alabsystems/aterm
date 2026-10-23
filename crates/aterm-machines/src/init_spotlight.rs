// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Rustup minimal-profile and `rust-docs` cleanup to keep toolchains out of Spotlight.

use crate::items::{Ctx, Item};
use crate::model::{Probe, User};
use crate::port::{Cmd, System};
use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

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
        Box::new(NoDocs(rustup)),
        Box::new(BuildNoindex {
            user: user.clone(),
            home: home.to_path_buf(),
        }),
    ])
}

/// Keep aterm's cargo build cache, tooling cache and Trust toolchain builds in `.noindex` folders.
#[derive(Debug, Clone)]
struct BuildNoindex {
    user: User,
    home: PathBuf,
}

impl BuildNoindex {
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

    /// Cargo/Rust `target` or `build` directories under `self.home` indexed by Spotlight.
    fn spotlit_build_dirs(&self, sys: &dyn System) -> Result<Vec<PathBuf>, String> {
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
        let mut out = Vec::new();
        for cand in candidates {
            let Some(name) = cand.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if Self::dir_present(sys, &cand)?
                && (Self::present(sys, &cand.join("CACHEDIR.TAG"))?
                    || Self::present(sys, &cand.join("debug"))?
                    || Self::present(sys, &cand.join("release"))?)
            {
                let noindex = cand.with_file_name(format!("{name}.noindex"));
                if Self::present(sys, &noindex)? {
                    return Err(format!(
                        "both {} and {} exist; left unchanged",
                        cand.display(),
                        noindex.display()
                    ));
                }
                out.push(cand);
            }
        }
        Ok(out)
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
        let has_caches = match Self::dir_present(sys, &self.caches_parent()) {
            Ok(v) => v,
            Err(why) => return Probe::Unusable { why },
        };
        let has_store = match Self::dir_present(sys, &self.trust_store()) {
            Ok(v) => v,
            Err(why) => return Probe::Unusable { why },
        };
        let spotlit_builds = match self.spotlit_build_dirs(sys) {
            Ok(dirs) => dirs,
            Err(why) => return Probe::Unusable { why },
        };
        if !has_caches && !has_store && spotlit_builds.is_empty() {
            return Probe::NotApplicable {
                why: format!(
                    "neither {} nor {} exists",
                    self.caches_parent().display(),
                    self.trust_store().display()
                ),
            };
        }
        let mut missing = Vec::new();
        if has_caches {
            match Self::dir_present(sys, &self.cargo_build_noindex()) {
                Ok(true) => {}
                Ok(false) => missing.push("cargo-build.noindex absent".to_owned()),
                Err(why) => return Probe::Unusable { why },
            }
            match Self::dir_present(sys, &self.tooling_noindex()) {
                Ok(true) => {}
                Ok(false) => missing.push("tooling.noindex absent".to_owned()),
                Err(why) => return Probe::Unusable { why },
            }
        }
        match self.plain_trust_builds(sys) {
            Ok(builds) if !builds.is_empty() => {
                missing.push(format!(
                    "trust store build(s) {} indexed by Spotlight",
                    builds.join(", ")
                ));
            }
            Ok(_) => {}
            Err(why) => return Probe::Unusable { why },
        }
        if !spotlit_builds.is_empty() {
            let names: Vec<String> = spotlit_builds
                .iter()
                .map(|d| d.display().to_string())
                .collect();
            missing.push(format!(
                "build folder(s) {} indexed by Spotlight",
                names.join(", ")
            ));
        }
        if missing.is_empty() {
            Probe::Ok {
                now: "cargo-build.noindex, tooling.noindex, and trust store builds are .noindex"
                    .into(),
            }
        } else {
            Probe::Change {
                from: missing.join("; "),
                to: "cargo-build.noindex, tooling.noindex, and trust/<build>.noindex".into(),
            }
        }
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        // Admit both inventories before creating or moving any cache directory.
        let trust_builds = self.plain_trust_builds(sys)?;
        let spotlit_builds = self.spotlit_build_dirs(sys)?;
        if Self::dir_present(sys, &self.caches_parent())? {
            for dir in [self.cargo_build_noindex(), self.tooling_noindex()] {
                if !Self::dir_present(sys, &dir)? {
                    sys.create_dir_all(&dir, 0o755)
                        .map_err(|e| format!("{}: {e}", dir.display()))?;
                }
            }
        }
        let store = self.trust_store();
        for name in trust_builds {
            let plain = store.join(&name);
            let noindex_name = format!("{name}.noindex");
            let noindex = store.join(&noindex_name);
            sys.rename(&plain, &noindex)
                .map_err(|e| format!("rename {} -> {}: {e}", plain.display(), noindex.display()))?;
            let ln = Cmd::new("/bin/ln")
                .as_user(&self.user)
                .args(["-s", &noindex_name])
                .arg(&plain);
            let ran = sys.run(&ln).map_err(|e| format!("{}: {e}", ln.display()))?;
            if !ran.success() {
                return Err(format!("{}: {}", ln.display(), ran.describe()));
            }
        }
        for plain in spotlit_builds {
            let Some(name) = plain.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let noindex_name = format!("{name}.noindex");
            let noindex = plain.with_file_name(&noindex_name);
            if sys.stat(&noindex).is_err() {
                sys.rename(&plain, &noindex).map_err(|e| {
                    format!("rename {} -> {}: {e}", plain.display(), noindex.display())
                })?;
                let ln = Cmd::new("/bin/ln")
                    .as_user(&self.user)
                    .args(["-s", &noindex_name])
                    .arg(&plain);
                let ran = sys.run(&ln).map_err(|e| format!("{}: {e}", ln.display()))?;
                if !ran.success() {
                    return Err(format!("{}: {}", ln.display(), ran.describe()));
                }
            }
        }
        Ok(())
    }

    fn revert(&self) -> Option<String> {
        Some(format!(
            "restore plain directories under {}",
            self.trust_store().display()
        ))
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

/// No toolchain rustup installed carries `rust-docs`.
#[derive(Debug)]
struct NoDocs(Rustup);

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
            Ok(docs) => Probe::Change {
                from: format!("rust-docs installed in {}", docs.join(", ")),
                to: "removed".into(),
            },
        }
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
        let items: Vec<Box<dyn Item>> =
            vec![Box::new(Profile(rustup.clone())), Box::new(NoDocs(rustup))];
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
    fn build_noindex_creates_cache_dirs_and_noindexes_trust_store_builds() {
        let alice = user("alice", 501);
        let item = BuildNoindex {
            user: alice,
            home: "/Users//alice".into(),
        };
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
        let ln = "alice$ /bin/ln -s 15698.noindex '/Users//alice/Library/Application Support/aterm/pkg/store/trust/15698'";
        sys.on(ln, Ran::ok(""));
        let items: Vec<Box<dyn Item>> = vec![Box::new(item)];
        let planned = engine::run(&items, Mode::Plan, &sys, &context(), &mut NoJournal);
        assert_eq!(planned[0].outcome, Outcome::Planned, "{planned:?}");
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
        assert!(!sys.calls().contains(&ln.to_owned()));
    }

    #[test]
    fn build_noindex_discovers_and_noindexes_spotlit_repo_targets() {
        let alice = user("alice", 501);
        let item = BuildNoindex {
            user: alice,
            home: "/Users//alice".into(),
        };
        let sys = Fake::root();
        sys.create_dir_all(
            Path::new("/Users//alice/Library/Caches/aterm/cargo-build.noindex"),
            0o755,
        )
        .unwrap();
        sys.create_dir_all(
            Path::new("/Users//alice/Library/Caches/aterm/tooling.noindex"),
            0o755,
        )
        .unwrap();
        sys.create_dir_all(Path::new("/Users//alice/work/demo/target/debug"), 0o755)
            .unwrap();
        sys.file("/Users//alice/work/demo/target/CACHEDIR.TAG", b"Signature:");
        sys.file("/usr/bin/mdfind", b"");
        let mdfind =
            "alice$ /usr/bin/mdfind -onlyin /Users//alice -0 'kMDItemFSName == \"*.rmeta\"'";
        sys.on(
            mdfind,
            Ran::ok("/Users//alice/work/demo/target/debug/deps/libdemo.rmeta\0"),
        );
        let items: Vec<Box<dyn Item>> = vec![Box::new(item)];
        let planned = engine::run(&items, Mode::Plan, &sys, &context(), &mut NoJournal);
        assert_eq!(planned[0].outcome, Outcome::Planned, "{planned:?}");
        assert!(
            planned[0]
                .from
                .as_deref()
                .is_some_and(|f| f.contains("/Users//alice/work/demo/target")),
            "{planned:?}"
        );

        let kept = "/Users//alice/work/demo/target/retained";
        sys.file(kept, b"existing build bytes");
        sys.create_dir_all(Path::new("/Users//alice/work/demo/target.noindex"), 0o755)
            .unwrap();
        let after = engine::run(&items, Mode::Plan, &sys, &context(), &mut NoJournal);
        assert_eq!(after[0].outcome, Outcome::Unknown, "{after:?}");
        assert!(items[0].apply(&sys, &context()).is_err());
        assert_eq!(
            sys.read_file(Path::new(kept)).unwrap(),
            b"existing build bytes"
        );
        assert!(
            sys.stat(Path::new("/Users//alice/work/demo/target"))
                .unwrap()
                .dir
        );
        assert!(!sys.calls().iter().any(|call| call.contains("/bin/ln")));

        let census = |reply| {
            let sys = Fake::root();
            sys.create_dir_all(Path::new("/Users//alice/Library/Caches"), 0o755)
                .unwrap();
            sys.file("/usr/bin/mdfind", b"").on(mdfind, reply);
            sys
        };
        for reply in [
            Ran::exit(1, "census failed"),
            Ran::ok("/Users//alice/work/demo/target/debug/incomplete.rmeta"),
        ] {
            let unknown = census(reply);
            assert!(matches!(
                items[0].probe(&unknown, &context()),
                Probe::Unusable { .. }
            ));
            assert!(items[0].apply(&unknown, &context()).is_err());
            assert!(
                unknown
                    .stat(Path::new(
                        "/Users//alice/Library/Caches/aterm/cargo-build.noindex"
                    ))
                    .is_err()
            );
        }
        let incomplete = census(Ran::ok(""));
        incomplete.refuse_complete_stdout(mdfind, "complete capture unavailable");
        assert!(matches!(
            items[0].probe(&incomplete, &context()),
            Probe::Unusable { .. }
        ));
        assert!(items[0].apply(&incomplete, &context()).is_err());
        assert!(
            incomplete
                .stat(Path::new(
                    "/Users//alice/Library/Caches/aterm/cargo-build.noindex"
                ))
                .is_err()
        );

        let missing = Fake::root();
        missing
            .create_dir_all(Path::new("/Users//alice/Library/Caches"), 0o755)
            .unwrap();
        assert!(matches!(
            items[0].probe(&missing, &context()),
            Probe::Unusable { .. }
        ));
        assert!(items[0].apply(&missing, &context()).is_err());
        assert!(
            missing
                .stat(Path::new(
                    "/Users//alice/Library/Caches/aterm/cargo-build.noindex"
                ))
                .is_err()
        );

        let empty = census(Ran::ok(""));
        for name in ["cargo-build.noindex", "tooling.noindex"] {
            empty
                .create_dir_all(
                    &Path::new("/Users//alice/Library/Caches/aterm").join(name),
                    0o755,
                )
                .unwrap();
        }
        assert!(matches!(
            items[0].probe(&empty, &context()),
            Probe::Ok { .. }
        ));
    }
}
