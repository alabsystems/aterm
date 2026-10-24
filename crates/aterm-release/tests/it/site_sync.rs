// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `ship site` end to end over the fixtures.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex, OnceLock};

use aterm_uds::spawnfd::{Child, Command};

use crate::site::pages::{self, Difference};
use crate::site::{
    Disagreement, Env, Failure, Guard, Launch, Options, Outcome, Ran, Refusal, Stop, World,
};
use crate::template::Tree;

const SLUG: &str = "alabsystems/aterm";
const PIN: &str = "1.97.1";
const TODAY: &str = "2026-08-29";
const ORIGIN: &str = "https://alab.systems";
const DMG63: &str =
    "https://github.com/alabsystems/aterm/releases/download/v0.63.0/aterm-0.63.0.dmg";
const DMG64: &str =
    "https://github.com/alabsystems/aterm/releases/download/v0.64.0/aterm-0.64.0.dmg";
const LINUX63: &str = "https://github.com/alabsystems/aterm/releases/download/v0.63.0/aterm-0.63.0-linux-x86_64.tar.gz";
const GHOST: &str = "ffffffffffffffffffffffffffffffffffffffff";
const PAGES_WITH_PICKS: [&str; 2] = ["aterm.html", "terminal.html"];

fn fixture(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/site")
        .join(rel)
}

fn at_once<T: Send>(cases: impl IntoIterator<Item = T>, case: impl Fn(T) + Sync) {
    std::thread::scope(|scope| {
        let case = &case;
        let running: Vec<_> = cases
            .into_iter()
            .map(|each| scope.spawn(move || case(each)))
            .collect();
        for ran in running {
            if let Err(panic) = ran.join() {
                std::panic::resume_unwind(panic);
            }
        }
    });
}

fn installer() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(crate::site::INSTALLER_SOURCE)
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn toy_module() -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../aterm-wasm-abi/tests/toy/aterm_wasm_abi_toy.wasm");
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn custom_section(name: &str, payload: &[u8]) -> Vec<u8> {
    fn leb(mut n: usize, out: &mut Vec<u8>) {
        loop {
            let byte = (n & 0x7f) as u8;
            n >>= 7;
            if n == 0 {
                out.push(byte);
                return;
            }
            out.push(byte | 0x80);
        }
    }
    let mut body = Vec::new();
    leb(name.len(), &mut body);
    body.extend_from_slice(name.as_bytes());
    body.extend_from_slice(payload);
    let mut section = vec![0];
    leb(body.len(), &mut section);
    section.extend(body);
    section
}

fn optimised_mark() -> Vec<u8> {
    custom_section("fixture-wasm-opt", b"-O3")
}

fn installed_pair(term: &Path) -> (String, String, Vec<u8>) {
    let mut modules: Vec<String> = std::fs::read_dir(term)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("aterm_wasm.") && n.ends_with(".wasm"))
        .collect();
    assert_eq!(modules.len(), 1, "one pair: {modules:?}");
    let name = modules.remove(0);
    let hash = name["aterm_wasm.".len()..name.len() - ".wasm".len()].to_string();
    let js = read(&term.join(format!("aterm_wasm.{hash}.js")));
    let wasm = std::fs::read(term.join(&name)).unwrap();
    (hash, js, wasm)
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dest = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_tree(&e.path(), &dest);
        } else {
            std::fs::copy(e.path(), &dest).unwrap();
        }
    }
}

/// git, isolated from this machine's configuration.
#[derive(Clone)]
struct Git {
    env: Vec<(String, String)>,
}

impl Git {
    fn output(&self, dir: &Path, args: &[&str]) -> std::process::Output {
        crate::ledger::testing::git()
            .arg("-C")
            .arg(dir)
            .args(args)
            .envs(self.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .output()
            .unwrap()
    }

    fn ok(&self, dir: &Path, args: &[&str]) -> String {
        let o = self.output(dir, args);
        assert!(
            o.status.success(),
            "git {args:?} in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }

    fn commit_all(&self, dir: &Path, msg: &str) {
        self.ok(dir, &["add", "-A"]);
        self.ok(
            dir,
            &[
                "-c",
                "user.name=fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "-q",
                "-m",
                msg,
            ],
        );
    }
}

/// How the engine's stock build and `wasm-opt` misbehave.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum EngineFault {
    /// The toy module, built and optimised.
    #[default]
    None,
    /// The build exits non-zero.
    BuildFails,
    /// A signal ends the build: what it started never showed it ran.
    Killed,
    /// A module with no `aterm_abi` descriptor (a wasm-bindgen-era build, say).
    NoDescriptor,
    /// The builder's home survives in the module.
    LeaksHome,
    /// `wasm-opt` leaves the module as it was.
    OptimiseNothing,
}

/// A deploy as the site's `deploy.sh` would end.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Deploy {
    /// public/ goes live.
    #[default]
    Ok,
    /// Goes live, but the CDN still serves the old engine.js.
    Stale,
    /// Goes live, but the live engine names a commit no repo holds.
    Ghost,
    /// Firebase's expired login.
    AuthErr,
    /// No firebase-tools at all.
    NoCli,
    /// An unrelated failure.
    Fail,
    /// A failure whose words name no login problem.
    OddFail,
}

/// What the sync asked of the world.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Call {
    Github(String),
    Download(String),
    Live(String),
    Build {
        commit: String,
        pin: String,
        rustflags: Vec<String>,
        /// The clone as cargo names its sources: the resolved path of the one handed over.
        sources: PathBuf,
    },
    Optimise,
    Deploy(PathBuf),
    Firebase {
        account: Option<String>,
    },
}

/// How the fixture world starts the sync's children: as they are, except that a signal
/// (SIGTERM) can arrive while a chosen git subcommand runs — after which, as under the
/// release, nothing more starts.
#[derive(Default)]
struct Signals {
    during: Option<&'static str>,
    caught: Cell<Option<i32>>,
}

impl Launch for Signals {
    fn interrupted(&self) -> Option<i32> {
        self.caught.get()
    }

    fn spawn(&self, cmd: &mut Command) -> std::io::Result<(Child, Guard)> {
        if self.caught.get().is_some() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "not started: interrupted",
            ));
        }
        let child = cmd.spawn()?;
        let during = self.during.is_some_and(|sub| {
            cmd.get_program() == aterm_uds::os_path!("git") && cmd.get_args().any(|a| a == sub)
        });
        if during {
            self.caught.set(Some(15));
        }
        Ok((child, Box::new(())))
    }
}

struct Fake {
    releases: String,
    main_sha: Option<String>,
    github_dead: bool,
    /// The live site cannot be fetched at all (no network, a DNS failure).
    live_dead: bool,
    live: PathBuf,
    home: PathBuf,
    engine: EngineFault,
    deploy: Deploy,
    firebase_ok: bool,
    git: Git,
    signals: Rc<Signals>,
    calls: Vec<Call>,
}

impl Fake {
    fn builds(&self) -> Vec<(String, String)> {
        self.calls
            .iter()
            .filter_map(|c| match c {
                Call::Build { commit, pin, .. } => Some((commit.clone(), pin.clone())),
                _ => None,
            })
            .collect()
    }

    fn deploys(&self) -> Vec<PathBuf> {
        self.calls
            .iter()
            .filter_map(|c| match c {
                Call::Deploy(p) => Some(p.clone()),
                _ => None,
            })
            .collect()
    }

    fn count(&self, f: impl Fn(&Call) -> bool) -> usize {
        self.calls.iter().filter(|c| f(c)).count()
    }
}

impl World for Fake {
    fn launch(&self) -> Rc<dyn Launch> {
        self.signals.clone()
    }

    fn github(&mut self, path: &str, _paginate: bool) -> Result<String, String> {
        self.calls.push(Call::Github(path.into()));
        if self.github_dead {
            return Err("GitHub GET failed: HTTP 401: Bad credentials".into());
        }
        if path == format!("repos/{SLUG}/releases?per_page=100") {
            return Ok(self.releases.clone());
        }
        match (&self.main_sha, path == format!("repos/{SLUG}/commits/main")) {
            (Some(sha), true) => Ok(format!("{{\"sha\":\"{sha}\"}}")),
            _ => Err(format!("unexpected GitHub read {path}")),
        }
    }

    fn download(&mut self, url: &str, limit: u64) -> Result<Vec<u8>, String> {
        self.calls.push(Call::Download(url.into()));
        let path = url
            .strip_prefix("file://")
            .ok_or_else(|| format!("unreachable: {url}"))?;
        let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
        if bytes.len() as u64 > limit {
            return Err(format!("larger than {limit} bytes"));
        }
        Ok(bytes)
    }

    fn live(&mut self, url: &str) -> Result<Vec<u8>, String> {
        self.calls.push(Call::Live(url.into()));
        if self.live_dead {
            return Err(format!("curl: (6) Could not resolve host for {url}"));
        }
        let path = url
            .strip_prefix(ORIGIN)
            .ok_or_else(|| format!("unreachable host: {url}"))?;
        let rel = match path.trim_start_matches('/') {
            "" => "index.html".to_string(),
            p if self.live.join(p).is_file() => p.to_string(),
            p => format!("{p}.html"),
        };
        std::fs::read(self.live.join(&rel)).map_err(|_| format!("404 {url}"))
    }

    fn build_module(
        &mut self,
        clone: &Path,
        pin: &str,
        rustflags: &[String],
        _out: &mut dyn std::io::Write,
    ) -> Result<Vec<u8>, String> {
        let head = self.git.output(clone, &["rev-parse", "HEAD"]);
        let commit = String::from_utf8_lossy(&head.stdout).trim().to_string();
        self.calls.push(Call::Build {
            commit: commit.clone(),
            pin: pin.into(),
            rustflags: rustflags.to_vec(),
            sources: clone.canonicalize().unwrap_or_else(|_| clone.to_path_buf()),
        });
        if !clone.join("crates/aterm-wasm").is_dir() {
            return Err(format!("no aterm checkout at {}", clone.display()));
        }
        if !self.git.ok(clone, &["status", "--porcelain"]).is_empty() {
            return Err("the clone has uncommitted changes".into());
        }
        let mut module = match self.engine {
            EngineFault::BuildFails => return Err("the wasm32 build exited 101".into()),
            EngineFault::Killed => {
                return Err(format!("{}: exited -1", crate::site::ENGINE_UNPROVEN));
            }
            EngineFault::NoDescriptor => b"\0asm\x01\0\0\0".to_vec(),
            _ => toy_module(),
        };
        module.extend(custom_section("fixture-commit", commit.as_bytes()));
        if self.engine == EngineFault::LeaksHome {
            let panic = format!("{}/crates/aterm-wasm/src/lib.rs", self.home.display());
            module.extend(custom_section("fixture-panic", panic.as_bytes()));
        }
        Ok(module)
    }

    fn optimise(&mut self, wasm: &Path, _out: &mut dyn std::io::Write) -> Result<(), String> {
        self.calls.push(Call::Optimise);
        if self.engine != EngineFault::OptimiseNothing {
            let mut bytes = std::fs::read(wasm).unwrap();
            bytes.extend(optimised_mark());
            std::fs::write(wasm, bytes).unwrap();
        }
        Ok(())
    }

    fn deploy(&mut self, site: &Path, _out: &mut dyn std::io::Write) -> Ran {
        self.calls.push(Call::Deploy(site.to_path_buf()));
        let fail = |log: &str| Ran {
            code: 1,
            log: log.into(),
        };
        match self.deploy {
            Deploy::AuthErr => {
                return fail(
                    "==> Deploying site…\nError: Authentication Error: Your credentials are no \
                     longer valid. Please run firebase login --reauth\n",
                );
            }
            Deploy::NoCli => {
                return fail(
                    "firebase-tools is not installed. Run:  npm install -g firebase-tools\n",
                );
            }
            Deploy::Fail => return fail("Error: HTTP Error: 500, Internal Server Error\n"),
            Deploy::OddFail => return fail("Error: An unexpected error has occurred.\n"),
            Deploy::Ok | Deploy::Stale | Deploy::Ghost => {}
        }
        let engine = self.live.join("assets/term/engine.js");
        let old_engine = std::fs::read(&engine).ok();
        let _ = std::fs::remove_dir_all(&self.live);
        copy_tree(&site.join("public"), &self.live);
        if let (Deploy::Stale, Some(old)) = (self.deploy, old_engine) {
            std::fs::write(&engine, old).unwrap();
        }
        if self.deploy == Deploy::Ghost {
            std::fs::write(
                self.live.join("assets/term/ENGINE_SOURCE"),
                format!("{GHOST}\n"),
            )
            .unwrap();
        }
        Ran {
            code: 0,
            log: "Done.\n".into(),
        }
    }

    fn firebase_refuses(
        &mut self,
        _site: &Path,
        account: Option<&str>,
        _key: Option<&Path>,
    ) -> Option<String> {
        self.calls.push(Call::Firebase {
            account: account.map(str::to_string),
        });
        (!self.firebase_ok)
            .then(|| "Error: Authentication Error: Your credentials are no longer valid.".into())
    }

    fn sleep(&mut self, _secs: u64) {}
}

struct Lab {
    _dir: aterm_tempfile::TempDir,
    root: PathBuf,
    git: Git,
    public_git: PathBuf,
    public_sha: String,
    generation: u32,
    site_root: PathBuf,
    site: PathBuf,
    origin: PathBuf,
    live: PathBuf,
}

struct Built {
    root: PathBuf,
    tree: Tree,
    public_sha: String,
    generation: u32,
}

impl Lab {
    fn new() -> Self {
        Self::at("site-repo", true)
    }

    fn at(site_root: &str, changelog: bool) -> Self {
        type Shapes = BTreeMap<(String, bool), Arc<OnceLock<Built>>>;
        static BUILT: Mutex<Shapes> = Mutex::new(BTreeMap::new());
        let shape = BUILT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry((site_root.to_string(), changelog))
            .or_default()
            .clone();
        let mut fresh = None;
        let built = shape.get_or_init(|| {
            let lab = Self::build(site_root, changelog);
            let built = Built {
                tree: Tree::read(&lab.root),
                root: lab.root.clone(),
                public_sha: lab.public_sha.clone(),
                generation: lab.generation,
            };
            fresh = Some(lab);
            built
        });
        if let Some(lab) = fresh {
            return lab;
        }
        let mut lab = Self::empty(site_root);
        built.tree.lay(&built.root, &lab.root);
        lab.public_sha = built.public_sha.clone();
        lab.generation = built.generation;
        lab
    }

    fn build(site_root: &str, changelog: bool) -> Self {
        let mut lab = Self::empty(site_root);
        std::fs::create_dir_all(lab.root.join("home")).unwrap();
        lab.publish(changelog);
        lab.make_site();
        let src = lab.root.join(format!("public-src-{}", lab.generation));
        for repo in [&src, &lab.public_git, &lab.site_root, &lab.origin] {
            lab.git
                .ok(repo, &["repack", "-adq", "-n", "--no-write-bitmap-index"]);
        }
        lab
    }

    fn empty(site_root: &str) -> Self {
        let dir = aterm_tempfile::Builder::new()
            .prefix("aterm-site-sync-")
            .tempdir()
            .unwrap();
        let root = dir.path().canonicalize().unwrap();
        let home = root.join("home");
        let git = Git {
            env: vec![
                ("HOME".into(), home.to_string_lossy().into_owned()),
                ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
                ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
                ("XDG_CONFIG_HOME".into(), "/dev/null".into()),
                ("GIT_TERMINAL_PROMPT".into(), "0".into()),
                ("GIT_TEMPLATE_DIR".into(), String::new()),
                ("GIT_CONFIG_COUNT".into(), "1".into()),
                ("GIT_CONFIG_KEY_0".into(), "maintenance.auto".into()),
                ("GIT_CONFIG_VALUE_0".into(), "false".into()),
                ("PATH".into(), crate::git_first_path().into()),
            ],
        };
        let site_root = root.join(site_root);
        Self {
            _dir: dir,
            git,
            public_git: root.join("public.git"),
            public_sha: String::new(),
            generation: 0,
            site: site_root.join("web/site"),
            origin: root.join("site-origin.git"),
            live: root.join("live"),
            site_root,
            root,
        }
    }

    fn publish(&mut self, changelog: bool) {
        self.publish_with(changelog, |_| {});
    }

    fn publish_with(&mut self, changelog: bool, edit: impl FnOnce(&Path)) {
        self.generation += 1;
        let src = self.root.join(format!("public-src-{}", self.generation));
        std::fs::create_dir_all(src.join("crates/aterm-wasm")).unwrap();
        std::fs::create_dir_all(src.join("tools")).unwrap();
        std::fs::copy(installer(), src.join(crate::site::INSTALLER_SOURCE)).unwrap();
        std::fs::write(
            src.join("crates/aterm-wasm/Cargo.toml"),
            format!(
                "[package]\nname = \"aterm-wasm\"\nversion = \"0.64.0\"\n# {}\n",
                self.generation
            ),
        )
        .unwrap();
        if changelog {
            std::fs::copy(fixture("CHANGELOG.md"), src.join("CHANGELOG.md")).unwrap();
        }
        edit(&src);
        self.git.ok(
            &self.root,
            &["init", "-q", "-b", "main", &src.to_string_lossy()],
        );
        self.git.commit_all(&src, "release: aterm-staging@fixture");
        let _ = std::fs::remove_dir_all(&self.public_git);
        self.git.ok(
            &self.root,
            &[
                "clone",
                "-q",
                "--bare",
                &src.to_string_lossy(),
                &self.public_git.to_string_lossy(),
            ],
        );
        self.public_sha = self.git.ok(&src, &["rev-parse", "HEAD"]);
    }

    fn make_site(&mut self) {
        for d in [&self.site_root, &self.origin, &self.live] {
            let _ = std::fs::remove_dir_all(d);
        }
        copy_tree(&fixture("public"), &self.site.join("public"));
        let deploy = self.site.join("deploy.sh");
        std::fs::write(&deploy, "#!/bin/sh\n# fixture; never run\nexit 64\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&deploy, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        self.git.ok(
            &self.root,
            &[
                "init",
                "-q",
                "-b",
                "main",
                &self.site_root.to_string_lossy(),
            ],
        );
        self.git.commit_all(&self.site_root, "site: fixture");
        self.git.ok(
            &self.root,
            &[
                "init",
                "-q",
                "--bare",
                "-b",
                "main",
                &self.origin.to_string_lossy(),
            ],
        );
        crate::ledger::testing::config(&self.origin, &[]);
        self.git.ok(
            &self.site_root,
            &["remote", "add", "origin", &self.origin.to_string_lossy()],
        );
        self.git
            .ok(&self.site_root, &["push", "-q", "-u", "origin", "main"]);
        copy_tree(&self.site.join("public"), &self.live);
    }

    fn env(&self) -> Env {
        Env {
            slug: SLUG.into(),
            commit: Some(self.public_sha.clone()),
            version: Some("0.64.0".into()),
            staging_commit: None,
            dry_run: false,
            clone_url: format!("file://{}", self.public_git.display()),
            site_dir: Some(self.site.clone()),
            no_deploy: false,
            site_url: ORIGIN.into(),
            download_base: "https://github.com".into(),
            verify_retries: 2,
            verify_sleep: 0,
            today: TODAY.into(),
            firebase_account: Some("owner@example.invalid".into()),
            firebase_sa_key: None,
            firebase_sa_key_in_site: None,
            author: Some(("Andrew Yates (pub)".into(), "pub@example.invalid".into())),
            subject_prefix: "site: ".into(),
            wasm_pin: Some(PIN.into()),
            home: Some(self.root.join("home")),
            masters: Vec::new(),
            now_unix: 1_790_000_000,
            git_env: self.git.env.clone(),
        }
    }

    fn fake(&self, releases: &str) -> Fake {
        let releases = if releases.starts_with(['[', '{']) {
            releases.to_string()
        } else {
            read(&fixture(releases))
        };
        Fake {
            releases,
            main_sha: None,
            github_dead: false,
            live_dead: false,
            live: self.live.clone(),
            home: self.root.join("home"),
            engine: EngineFault::None,
            deploy: Deploy::Ok,
            firebase_ok: true,
            git: self.git.clone(),
            signals: Rc::default(),
            calls: Vec::new(),
        }
    }

    fn sync(
        &self,
        opts: &Options,
        env: &Env,
        world: &mut Fake,
    ) -> (Result<Outcome, Failure>, String) {
        let mut out = Vec::new();
        let ended = crate::site::sync(opts, env, world, &mut out);
        let _ = out.flush();
        (ended, String::from_utf8_lossy(&out).into_owned())
    }

    fn run(&self, world: &mut Fake) -> (Result<Outcome, Failure>, String) {
        self.sync(&Options::default(), &self.env(), world)
    }

    fn page(&self, name: &str) -> String {
        read(&self.site.join("public").join(name))
    }

    fn head(&self) -> String {
        self.git.ok(&self.site_root, &["rev-parse", "HEAD"])
    }

    fn origin_main(&self) -> String {
        self.git.ok(&self.origin, &["rev-parse", "main"])
    }

    fn commits(&self) -> usize {
        self.git
            .ok(&self.site_root, &["rev-list", "--count", "HEAD"])
            .parse()
            .unwrap()
    }

    fn status(&self) -> String {
        self.git.ok(&self.site_root, &["status", "--porcelain"])
    }

    fn owed_marker(&self) -> PathBuf {
        self.site_root.join(".git").join(crate::site::OWED_MARK)
    }

    fn refuse_pushes(&self, refuse: bool) {
        if refuse {
            self.git
                .ok(&self.origin, &["config", "receive.maxInputSize", "1"]);
        } else {
            self.git
                .ok(&self.origin, &["config", "--unset", "receive.maxInputSize"]);
        }
    }
}

fn synced(ended: &Result<Outcome, Failure>, out: &str) -> bool {
    assert!(ended.is_ok(), "{ended:?}\n{out}");
    matches!(ended, Ok(Outcome::Synced { .. } | Outcome::NoDeploy))
}

fn deployed(ended: &Result<Outcome, Failure>, out: &str) {
    assert_eq!(ended, &Ok(Outcome::Synced { deployed: true }), "{out}");
}

fn failure(ended: Result<Outcome, Failure>, out: &str) -> Failure {
    match ended {
        Err(failed) => failed,
        other => panic!("expected a failure, got {other:?}\n{out}"),
    }
}

fn failed_child(ended: Result<Outcome, Failure>, out: &str) -> (&'static str, String) {
    match failure(ended, out) {
        Failure::Child { child, said } => (child, said),
        other => panic!("expected a child that failed, got {other:?}\n{out}"),
    }
}

fn refused(ended: Result<Outcome, Failure>, out: &str, kind: Refusal) -> String {
    match failure(ended, out) {
        Failure::Refused { kind: k, why } if k == kind => why,
        other => panic!("expected a {kind:?} refusal, got {other:?}\n{out}"),
    }
}

#[test]
fn a_release_sync_rewrites_rebuilds_commits_pushes_deploys_and_verifies() {
    let lab = Lab::new();
    let mut world = lab.fake("releases-fat.json");
    let (ended, out) = lab.run(&mut world);
    deployed(&ended, &out);

    for name in ["index.html", "aterm.html"] {
        let page = lab.page(name);
        assert_eq!(pages::hrefs(name, &page), [DMG63], "{name}");
        assert_eq!(
            pages::note_cells(&page),
            ["<code>aterm-0.63.0.dmg</code> &middot; <code>1.07&nbsp;GB</code>"],
            "{name}: the name and the Finder's decimal size, no version cell"
        );
    }
    let terminal = lab.page("terminal.html");
    assert_eq!(pages::hrefs("terminal.html", &terminal), [DMG63]);
    assert_eq!(pages::title_versions(&terminal), ["0.63.0"]);
    assert_eq!(
        pages::attribute_values(&terminal, "href")
            .iter()
            .filter(|h| *h == "/register")
            .count(),
        1,
        "the Register button is left alone"
    );
    for name in ["aterm.html", "terminal.html"] {
        assert_eq!(
            pages::attribute_values(&lab.page(name), "data-href-linux-x86_64"),
            [LINUX63]
        );
    }
    assert!(pages::attribute_values(&lab.page("index.html"), "data-href-linux-x86_64").is_empty());
    let aterm = lab.page("aterm.html");
    assert!(aterm.contains("<a href=\"/releases\">release notes</a>"));
    assert_eq!(lab.page("install.sh"), read(&installer()));
    for name in PAGES_WITH_PICKS {
        for attr in ["data-install-linux-x86_64", "data-install-linux-aarch64"] {
            assert!(
                pages::attribute_values(&lab.page(name), attr).is_empty(),
                "{name} {attr}"
            );
        }
    }
    assert!(
        out.contains("== linux-x86_64 install: no signed executable — the page offers no command"),
        "{out}"
    );

    assert_eq!(world.builds(), [(lab.public_sha.clone(), PIN.to_string())]);
    let home = lab.root.join("home");
    let Some(Call::Build {
        rustflags, sources, ..
    }) = world.calls.iter().find(|c| matches!(c, Call::Build { .. }))
    else {
        unreachable!()
    };
    assert_eq!(rustflags[..2], ["-C", "target-feature=+simd128"]);
    assert!(
        rustflags.contains(&format!("--remap-path-prefix={}=/aterm", sources.display())),
        "the clone remap names {} as cargo does: {rustflags:?}",
        sources.display()
    );
    assert_eq!(
        rustflags.last().unwrap(),
        &format!("--remap-path-prefix={}=/build", home.display())
    );
    assert_eq!(world.count(|c| *c == Call::Optimise), 1);
    let term = lab.site.join("public/assets/term");
    assert_eq!(read(&term.join("ENGINE_SOURCE")).trim(), lab.public_sha);
    let (h, js, wasm) = installed_pair(&term);
    assert_eq!(
        h,
        aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(&wasm))[..12]
    );
    assert!(
        wasm.ends_with(&optimised_mark()),
        "the shipped module is the optimised one"
    );
    assert!(wasm.windows(40).any(|w| w == lab.public_sha.as_bytes()));
    let mut built = toy_module();
    built.extend(custom_section("fixture-commit", lab.public_sha.as_bytes()));
    let generated = aterm_verify::wasm_glue::glue(&built, "aterm_wasm").unwrap();
    let (_, generated) = generated
        .into_iter()
        .find(|(name, _)| name == "aterm_wasm.js")
        .unwrap();
    let generated = String::from_utf8(generated).unwrap();
    assert_eq!(generated.matches("\"aterm_wasm_bg.wasm\"").count(), 1);
    assert_eq!(
        js,
        generated.replace(
            "\"aterm_wasm_bg.wasm\"",
            &format!("\"aterm_wasm.{h}.wasm\"")
        )
    );
    assert_eq!(
        read(&term.join("engine.js")),
        crate::site::engine::engine_js(&h)
    );
    assert!(
        !term.join("aterm_wasm.000000000000.js").exists(),
        "the old pair is gone"
    );

    assert_eq!(
        lab.page("releases.html"),
        read(&fixture("releases.golden.html"))
    );
    assert_eq!(lab.page("assets/releases.css"), crate::site::notes::CSS);
    assert_eq!(
        crate::site::sitemap::lastmods(&lab.page("sitemap.xml")),
        [
            (format!("{ORIGIN}/"), TODAY.to_string()),
            (format!("{ORIGIN}/aterm"), TODAY.to_string()),
            (format!("{ORIGIN}/terminal"), TODAY.to_string()),
            (format!("{ORIGIN}/mission"), "2026-08-27".to_string()),
            (format!("{ORIGIN}/releases"), TODAY.to_string()),
        ]
    );

    assert_eq!(lab.commits(), 2);
    assert_eq!(lab.head(), lab.origin_main());
    assert_eq!(
        lab.git
            .ok(&lab.site_root, &["log", "-1", "--format=%an <%ae>"]),
        "Andrew Yates (pub) <pub@example.invalid>"
    );
    assert_eq!(
        lab.git.ok(&lab.site_root, &["log", "-1", "--format=%s"]),
        format!(
            "site: alab.systems — aterm 0.64.0 published: /terminal engine @ {}, download → \
             aterm-0.63.0.dmg (v0.64.0 not cut yet), /releases",
            &lab.public_sha[..12]
        )
    );
    assert_eq!(
        lab.git.ok(&lab.site_root, &["log", "-1", "--format=%b"]),
        format!(
            "Synced by aterm's `ship site` from github.com/{SLUG} @ {}.\nDownload: {DMG63} \
             (1.07 GB, release v0.63.0). v0.64.0 has no release yet; the release's after-cut \
             step re-runs `ship site --latest`. Linux: {LINUX63} (release v0.63.0). Release \
             notes: /releases.",
            lab.public_sha
        )
    );
    assert!(
        out.contains(
            "== WARNING: aterm-0.63.0.dmg is 1.07 GB — not the lean image; the site links it \
             as-is. Ship its successor from current main."
        ),
        "{out}"
    );
    let w = "web/site/public";
    let mut committed: Vec<String> = lab
        .git
        .ok(
            &lab.site_root,
            &["show", "--name-only", "--format=", "HEAD"],
        )
        .lines()
        .map(str::to_string)
        .collect();
    committed.sort();
    let mut want: Vec<String> = [
        "assets/releases.css".to_string(),
        "assets/term/ENGINE_SOURCE".into(),
        "assets/term/aterm_wasm.000000000000.js".into(),
        "assets/term/aterm_wasm.000000000000.wasm".into(),
        format!("assets/term/aterm_wasm.{h}.js"),
        format!("assets/term/aterm_wasm.{h}.wasm"),
        "assets/term/engine.js".into(),
        "aterm.html".into(),
        "index.html".into(),
        "install.sh".into(),
        "releases.html".into(),
        "sitemap.xml".into(),
        "terminal.html".into(),
    ]
    .iter()
    .map(|f| format!("{w}/{f}"))
    .collect();
    want.sort();
    assert_eq!(committed, want);
    assert_eq!(lab.status(), "");
    assert_eq!(world.deploys(), std::slice::from_ref(&lab.site));
    assert!(!lab.owed_marker().exists(), "shipped: nothing is owed");
    assert!(
        world
            .calls
            .iter()
            .all(|c| !matches!(c, Call::Github(p) if !p.starts_with("repos/")))
    );
    assert!(world.calls.iter().all(|c| match c {
        Call::Live(u) => u.starts_with(ORIGIN),
        Call::Download(u) => u.starts_with("file://") || u.starts_with("https://github.com/"),
        _ => true,
    }));

    world.calls.clear();
    let (ended, out) = lab.run(&mut world);
    assert_eq!(ended, Ok(Outcome::Synced { deployed: false }), "{out}");
    assert!(world.builds().is_empty() && world.deploys().is_empty());
    assert_eq!(lab.commits(), 2);
    assert!(
        world.count(|c| matches!(c, Call::Live(_))) > 0,
        "the live site is still checked"
    );
}

#[test]
fn the_next_release_moves_every_page_and_back() {
    let lab = Lab::new();
    let mut world = lab.fake("releases-lean.json");
    let (ended, out) = lab.run(&mut world);
    deployed(&ended, &out);
    for name in ["index.html", "aterm.html"] {
        let page = lab.page(name);
        assert_eq!(pages::hrefs(name, &page), [DMG64], "{name}");
        assert_eq!(
            pages::note_cells(&page),
            ["<code>aterm-0.64.0.dmg</code> &middot; <code>31&nbsp;MB</code>"]
        );
    }
    let terminal = lab.page("terminal.html");
    assert_eq!(pages::hrefs("terminal.html", &terminal), [DMG64]);
    assert_eq!(pages::title_versions(&terminal), ["0.64.0"]);
    for name in ["aterm.html", "terminal.html"] {
        assert!(pages::attribute_values(&lab.page(name), "data-href-linux-x86_64").is_empty());
    }
    assert_eq!(
        lab.git.ok(&lab.site_root, &["log", "-1", "--format=%s"]),
        format!(
            "site: alab.systems — aterm 0.64.0 published: /terminal engine @ {}, download → \
             aterm-0.64.0.dmg, /releases",
            &lab.public_sha[..12]
        ),
        "the promoted version IS the newest release: no catch-up words"
    );
    let body = lab.git.ok(&lab.site_root, &["log", "-1", "--format=%b"]);
    assert!(
        !body.contains("has no release yet") && !body.contains("Linux:"),
        "{body}"
    );
    assert!(!out.contains("WARNING"), "{out}");
    let mut world = lab.fake("releases-fat.json");
    let (ended, out) = lab.run(&mut world);
    deployed(&ended, &out);
    assert_eq!(pages::hrefs("index.html", &lab.page("index.html")), [DMG63]);
    assert_eq!(
        pages::hrefs("terminal.html", &lab.page("terminal.html")),
        [DMG63]
    );
    assert_eq!(
        pages::note_cells(&lab.page("aterm.html")),
        ["<code>aterm-0.63.0.dmg</code> &middot; <code>1.07&nbsp;GB</code>"]
    );
}

#[test]
fn release_bodies_render_releases_when_the_public_tree_has_no_changelog() {
    let lab = Lab::at("site-repo", false);
    let mut world = lab.fake("releases-fat.json");
    let (ended, out) = lab.run(&mut world);
    deployed(&ended, &out);
    assert_eq!(
        lab.page("releases.html"),
        read(&fixture("releases-bodies.golden.html"))
    );
}

#[test]
fn a_dry_run_plans_and_changes_nothing() {
    let lab = Lab::new();
    let before = lab.head();
    for (opts, env) in [
        (
            Options {
                dry_run: true,
                ..Options::default()
            },
            lab.env(),
        ),
        (
            Options::default(),
            Env {
                dry_run: true,
                ..lab.env()
            },
        ),
    ] {
        let mut world = lab.fake("releases-fat.json");
        let (ended, out) = lab.sync(&opts, &env, &mut world);
        assert_eq!(
            ended,
            Ok(Outcome::Planned {
                files: [
                    "public/index.html",
                    "public/aterm.html",
                    "public/terminal.html",
                    "public/releases.html",
                    "public/assets/releases.css",
                    "public/install.sh",
                    "public/sitemap.xml",
                ]
                .map(String::from)
                .to_vec(),
                engine: true,
            }),
            "{out}"
        );
        assert!(world.builds().is_empty() && world.deploys().is_empty());
        assert_eq!(lab.status(), "");
        assert_eq!(lab.head(), before);
        assert!(!lab.site.join("public/releases.html").exists());
        assert!(!lab.site.join("public/install.sh").exists());
        assert!(!lab.site.join("public/assets/term/ENGINE_SOURCE").exists());
    }
}

#[test]
fn an_unsized_dmg_is_linked_by_name_alone() {
    let lab = Lab::new();
    let no_size = r#"[{"tag_name":"v0.63.0","prerelease":false,"draft":false,"assets":[
        {"name":"aterm-0.63.0.dmg","size":null},{"name":"aterm-0.63.0-mac.zip"},
        {"name":"aterm-appcast.toml"},{"name":"aterm-appcast.toml.sig"}]}]"#;
    let mut world = lab.fake(no_size);
    let (ended, out) = lab.run(&mut world);
    deployed(&ended, &out);
    for name in ["index.html", "aterm.html"] {
        assert_eq!(
            pages::note_cells(&lab.page(name)),
            ["<code>aterm-0.63.0.dmg</code>"],
            "{name}"
        );
    }
}

#[test]
fn no_usable_site_checkout_touches_nothing() {
    let lab = Lab::new();
    let mut world = lab.fake("releases-fat.json");
    let gone = Env {
        site_dir: Some(lab.root.join("no-such-site")),
        ..lab.env()
    };
    let (ended, out) = lab.sync(&Options::default(), &gone, &mut world);
    match ended {
        Ok(Outcome::NoCheckout(why)) => assert!(why.contains("site checkout not found"), "{why}"),
        other => panic!("{other:?}\n{out}"),
    }
    std::fs::remove_file(lab.site.join("deploy.sh")).unwrap();
    let (ended, out) = lab.run(&mut world);
    match &ended {
        Ok(Outcome::NoCheckout(why)) => {
            assert!(why.contains("has no executable deploy.sh"), "{why}");
            assert!(!why.contains("not found"), "{why}");
        }
        other => panic!("{other:?}\n{out}"),
    }
    assert_eq!(crate::site::report(&ended), 3);
    assert!(world.calls.is_empty(), "{:?}", world.calls);
    assert_eq!(lab.status(), "D web/site/deploy.sh");
    let (ended, _) = lab.sync(
        &Options {
            skip: true,
            ..Options::default()
        },
        &lab.env(),
        &mut world,
    );
    assert_eq!(ended, Ok(Outcome::Skipped));
    assert!(world.calls.is_empty());
}

#[test]
fn a_missing_firebase_login_commits_and_pushes_but_never_hides_a_real_failure() {
    at_once(
        [
            (Deploy::AuthErr, true, false),
            (Deploy::NoCli, true, false),
            (Deploy::Fail, true, true),
            (Deploy::OddFail, false, false),
            (Deploy::OddFail, true, true),
        ],
        a_missing_firebase_login_case,
    );
}

fn a_missing_firebase_login_case((deploy, firebase_ok, want_failure): (Deploy, bool, bool)) {
    let lab = Lab::new();
    let mut world = lab.fake("releases-fat.json");
    world.deploy = deploy;
    world.firebase_ok = firebase_ok;
    let (ended, out) = lab.run(&mut world);
    if want_failure {
        assert_eq!(failed_child(ended, &out).0, "deploy.sh", "{deploy:?}");
        assert_eq!(
            world.count(|c| *c
                == Call::Firebase {
                    account: Some("owner@example.invalid".into())
                }),
            1,
            "{deploy:?}: the login was asked, with deploy.sh's account"
        );
    } else {
        assert_eq!(ended, Ok(Outcome::NotDeployed), "{deploy:?}\n{out}");
        assert_eq!(
            out.lines().last(),
            Some(
                format!(
                    "SITE NOT DEPLOYED — committed and pushed; deploy later with: (cd {} && \
                     ./deploy.sh)",
                    lab.site.display()
                )
                .as_str()
            )
        );
        assert!(
            lab.owed_marker().is_file(),
            "{deploy:?}: the commit stays owed"
        );
    }
    assert_eq!(
        lab.head(),
        lab.origin_main(),
        "{deploy:?}: pushed either way"
    );
    assert_eq!(
        world.count(|c| matches!(c, Call::Live(_))),
        0,
        "{deploy:?}: no live claim"
    );
}

#[test]
fn no_deploy_commits_and_pushes_only() {
    let lab = Lab::new();
    let mut world = lab.fake("releases-fat.json");
    let opts = Options {
        no_deploy: true,
        ..Options::default()
    };
    for run in 0..2 {
        let (ended, out) = lab.sync(&opts, &lab.env(), &mut world);
        assert_eq!(ended, Ok(Outcome::NoDeploy), "run {run}\n{out}");
        assert!(world.deploys().is_empty());
        assert_eq!(world.count(|c| matches!(c, Call::Live(_))), 0);
        assert_eq!(lab.head(), lab.origin_main());
    }
    let env = Env {
        no_deploy: true,
        ..lab.env()
    };
    let (ended, _) = lab.sync(&Options::default(), &env, &mut world);
    assert_eq!(ended, Ok(Outcome::NoDeploy));
    assert!(world.deploys().is_empty());
}

#[test]
fn an_interrupt_starts_nothing_further() {
    let lab = Lab::new();
    let mut world = lab.fake("releases-fat.json");
    world.signals = Rc::new(Signals {
        during: Some("push"),
        ..Signals::default()
    });
    let (ended, out) = lab.run(&mut world);
    assert_eq!(
        failure(ended, &out),
        Failure::Interrupted {
            signal: 15,
            step: Stop::Before("the deploy")
        }
    );
    assert!(world.deploys().is_empty());
    assert_eq!(
        world.count(|c| matches!(c, Call::Live(_) | Call::Firebase { .. })),
        0
    );
    assert_eq!(
        lab.head(),
        lab.origin_main(),
        "the push it arrived in finished"
    );
    assert!(lab.owed_marker().is_file(), "the commit stays owed");
    let mut world = lab.fake("releases-fat.json");
    let (ended, out) = lab.run(&mut world);
    deployed(&ended, &out);
    assert_eq!(world.deploys().len(), 1);

    let lab = Lab::new();
    let before = lab.head();
    let mut world = lab.fake("releases-fat.json");
    world.signals = Rc::new(Signals {
        during: Some("clone"),
        ..Signals::default()
    });
    let (ended, out) = lab.run(&mut world);
    match failure(ended, &out) {
        Failure::Interrupted {
            signal: 15,
            step: Stop::During(stopped),
        } => assert!(
            matches!(*stopped, Failure::Child { .. }),
            "a git it never started: {stopped:?}"
        ),
        other => panic!("{other:?}"),
    }
    assert!(world.builds().is_empty() && world.deploys().is_empty());
    assert_eq!(
        world.count(|c| matches!(c, Call::Github(_) | Call::Download(_))),
        0
    );
    assert_eq!((lab.head(), lab.status()), (before, String::new()));
}

#[test]
fn an_interrupted_syncs_own_files_are_put_back_and_a_persons_are_refused() {
    let lab = Lab::new();
    let in_flight = lab.site_root.join(".git").join(crate::site::IN_FLIGHT_MARK);
    let mut world = lab.fake("releases-fat.json");
    world.signals = Rc::new(Signals {
        during: Some("add"),
        ..Signals::default()
    });
    let (ended, out) = lab.run(&mut world);
    assert!(
        matches!(
            failure(ended, &out),
            Failure::Interrupted { signal: 15, .. }
        ),
        "{out}"
    );
    assert_eq!(lab.commits(), 1, "stopped before its commit");
    assert!(!lab.status().is_empty(), "its writes are left");
    assert!(in_flight.is_file(), "and it is in flight");
    let left = lab.status();

    std::fs::write(lab.owed_marker(), "0123456789\n").unwrap();
    let mut world = lab.fake("releases-fat.json");
    let _ = refused(lab.run(&mut world).0, "", Refusal::DirtyOwed);
    assert_eq!(lab.status(), left);
    std::fs::remove_file(lab.owed_marker()).unwrap();
    let dry = Options {
        dry_run: true,
        ..Options::default()
    };
    let (ended, out) = lab.sync(&dry, &lab.env(), &mut world);
    let _ = refused(ended, &out, Refusal::DirtyDryRun);
    assert_eq!(lab.status(), left);

    let mut world = lab.fake("releases-fat.json");
    let (ended, out) = lab.run(&mut world);
    deployed(&ended, &out);
    assert_eq!(lab.commits(), 2, "one sync commit, of what this run wrote");
    assert_eq!(lab.status(), "");
    assert!(!in_flight.exists(), "nothing in flight once committed");

    let path = lab.site.join("public/index.html");
    std::fs::write(&path, read(&path) + "<!-- wip -->\n").unwrap();
    let mut world = lab.fake("releases-fat.json");
    let _ = refused(lab.run(&mut world).0, "", Refusal::DirtyOwned);
    assert!(read(&path).ends_with("<!-- wip -->\n"));
}

#[test]
fn an_interrupted_sync_beside_a_staged_file_it_does_not_own_is_refused() {
    let lab = Lab::new();
    let mut world = lab.fake("releases-fat.json");
    world.signals = Rc::new(Signals {
        during: Some("add"),
        ..Signals::default()
    });
    let (ended, out) = lab.run(&mut world);
    assert!(
        matches!(
            failure(ended, &out),
            Failure::Interrupted { signal: 15, .. }
        ),
        "{out}"
    );
    std::fs::write(lab.site_root.join("NOTES.md"), "a person's\n").unwrap();
    lab.git.ok(&lab.site_root, &["add", "NOTES.md"]);

    let mut world = lab.fake("releases-fat.json");
    let (ended, out) = lab.run(&mut world);
    let _ = refused(ended, &out, Refusal::StagedUnrelated);
    assert!(world.builds().is_empty() && world.deploys().is_empty());
    assert_eq!(lab.commits(), 1, "nothing committed");
    assert_eq!(
        lab.git
            .ok(&lab.site_root, &["diff", "--cached", "--name-only"]),
        "NOTES.md",
        "theirs is left staged, and only theirs"
    );
}

#[test]
fn a_live_site_that_cannot_be_fetched_is_unchecked_never_synced() {
    let lab = Lab::new();
    let mut world = lab.fake("releases-fat.json");
    world.live_dead = true;
    let (ended, out) = lab.run(&mut world);
    assert_eq!(
        ended,
        Ok(Outcome::LiveUnchecked { deployed: true }),
        "{out}"
    );
    assert_eq!(world.deploys().len(), 1);
    assert!(world.count(|c| matches!(c, Call::Live(_))) > 0);
    assert_eq!(crate::site::report(&ended), 0);
    let after = crate::release_steps::site_outcome(&ended, Vec::new()).expect("a note");
    assert!(!after.site_deferred);
    assert!(
        after.note.contains("could NOT be verified live"),
        "{after:?}"
    );
    let (ended, out) = lab.run(&mut world);
    assert_eq!(
        ended,
        Ok(Outcome::LiveUnchecked { deployed: false }),
        "{out}"
    );
}

#[test]
fn a_live_site_that_disagrees_after_the_deploy_is_named() {
    let lab = Lab::new();
    let mut world = lab.fake("releases-fat.json");
    world.deploy = Deploy::Stale;
    let (ended, out) = lab.run(&mut world);
    match &ended {
        Ok(Outcome::LiveDisagrees {
            deployed: true,
            why,
        }) => {
            assert!(
                why.contains(&Disagreement::Page(Difference::EngineDiffers)),
                "{why:?}"
            );
        }
        other => panic!("{other:?}\n{out}"),
    }
    assert_eq!(crate::site::report(&ended), 4);
    assert_eq!(world.deploys().len(), 1);

    let lab = Lab::new();
    let mut world = lab.fake("releases-fat.json");
    world.deploy = Deploy::Ghost;
    let (ended, out) = lab.run(&mut world);
    assert_eq!(
        ended,
        Ok(Outcome::LiveDisagrees {
            deployed: true,
            why: vec![Disagreement::EngineNotCommitted {
                served: GHOST.into(),
                committed: lab.public_sha.clone(),
            }],
        }),
        "{out}"
    );
}

#[test]
fn an_engine_the_site_repo_does_not_hold_is_never_deployed() {
    let lab = Lab::new();
    std::fs::write(
        lab.site.join(".gitignore"),
        "public/assets/term/aterm_wasm.*\npublic/assets/term/ENGINE_SOURCE\n",
    )
    .unwrap();
    lab.git
        .commit_all(&lab.site_root, "site: someone ignored the engine");
    lab.git
        .ok(&lab.site_root, &["push", "-q", "origin", "main"]);
    let mut world = lab.fake("releases-fat.json");
    let (ended, out) = lab.run(&mut world);
    let (child, said) = failed_child(ended, &out);
    assert_eq!(
        child, "git add",
        "refused at the staging, in git's words: {said}"
    );
    assert!(said.contains("public/assets/term/ENGINE_SOURCE"), "{said}");
    assert!(world.deploys().is_empty());

    let mut lab = Lab::new();
    let mut world = lab.fake("releases-fat.json");
    let (ended, out) = lab.run(&mut world);
    deployed(&ended, &out);
    let first = lab.public_sha.clone();
    assert_eq!(
        lab.git.ok(
            &lab.site_root,
            &["show", "HEAD:web/site/public/assets/term/ENGINE_SOURCE"]
        ),
        first,
        "the ordinary path commits the marker"
    );
    lab.git.ok(
        &lab.site,
        &[
            "update-index",
            "--assume-unchanged",
            "public/assets/term/ENGINE_SOURCE",
        ],
    );
    lab.publish(true);
    assert_ne!(lab.public_sha, first);
    world.calls.clear();
    let (ended, out) = lab.run(&mut world);
    let why = refused(ended, &out, Refusal::EngineNotInGit);
    assert!(
        why.contains(&lab.public_sha) && why.contains(&first),
        "the engine on disk and the one committed, by sha: {why}"
    );
    assert!(world.deploys().is_empty());
}

#[test]
fn a_page_out_of_shape_or_dirty_is_refused_before_anything_is_written() {
    for (page, from, to, rule) in [
        (
            "index.html",
            "class=\"download\"",
            "class=\"dl\"",
            "index.html: expected exactly one match for the download anchor, found 0",
        ),
        (
            "terminal.html",
            "aterm v0.61.0&nbsp;",
            "aterm (dev) ",
            "terminal.html: expected exactly one match for the header title version",
        ),
    ] {
        let lab = Lab::new();
        let path = lab.site.join("public").join(page);
        std::fs::write(&path, read(&path).replace(from, to)).unwrap();
        lab.git
            .commit_all(&lab.site_root, "site: someone edited a page");
        let before = lab.head();
        let mut world = lab.fake("releases-fat.json");
        let (ended, out) = lab.run(&mut world);
        let why = refused(ended, &out, Refusal::Page);
        assert!(why.starts_with(rule.split(':').next().unwrap()), "{why}");
        assert_eq!(lab.head(), before);
        assert!(world.builds().is_empty() && world.deploys().is_empty());
        assert_eq!(lab.status(), "");
    }
    let lab = Lab::new();
    let path = lab.site.join("public/aterm.html");
    std::fs::write(&path, read(&path) + "<!-- wip -->\n").unwrap();
    let mut world = lab.fake("releases-fat.json");
    let (ended, out) = lab.run(&mut world);
    let why = refused(ended, &out, Refusal::DirtyOwned);
    assert!(
        why.contains("public/aterm.html"),
        "names the dirty file: {why}"
    );
    assert!(
        world.calls.is_empty(),
        "refused before GitHub was asked: {:?}",
        world.calls
    );
}

#[test]
fn a_stream_with_nothing_installable_keeps_the_previous_link() {
    let lab = Lab::new();
    let before = lab.head();
    let nodmg = r#"[{"tag_name":"v0.65.0","prerelease":false,"draft":false,"assets":[
        {"name":"aterm.dmg","size":31457280},{"name":"aterm-0.65.0-mac.zip","size":28219832}]}]"#;
    let mut world = lab.fake(nodmg);
    let (ended, out) = lab.run(&mut world);
    let _ = refused(ended, &out, Refusal::Release);
    assert!(world.builds().is_empty() && world.deploys().is_empty());
    assert_eq!((lab.head(), lab.status()), (before, String::new()));
    assert_eq!(
        pages::hrefs("index.html", &lab.page("index.html")),
        ["https://github.com/alabsystems/aterm/releases/download/v0.61.0/aterm-0.61.0.dmg"]
    );
}

#[test]
fn the_commit_is_the_promoted_one_and_github_is_read_through_its_api_only() {
    let lab = Lab::new();
    for (commit, fetched) in [
        (Some("0000000000000000000000000000000000000000"), true),
        (Some("abc"), false),
        (None, false),
    ] {
        let mut world = lab.fake("releases-fat.json");
        let env = Env {
            commit: commit.map(str::to_string),
            ..lab.env()
        };
        let (ended, out) = lab.sync(&Options::default(), &env, &mut world);
        if fetched {
            assert_eq!(failed_child(ended, &out).0, "git fetch", "{commit:?}");
        } else {
            let _ = refused(ended, &out, Refusal::Commit);
        }
    }
    let latest = Options {
        latest: true,
        no_deploy: true,
        ..Options::default()
    };
    let env = Env {
        commit: None,
        ..lab.env()
    };
    let mut world = lab.fake("releases-fat.json");
    world.main_sha = Some(lab.public_sha.clone());
    let (ended, out) = lab.sync(&latest, &env, &mut world);
    assert!(synced(&ended, &out));
    assert_eq!(
        world.calls[0],
        Call::Github(format!("repos/{SLUG}/commits/main"))
    );
    assert_eq!(
        read(&lab.site.join("public/assets/term/ENGINE_SOURCE")).trim(),
        lab.public_sha
    );

    let lab = Lab::new();
    let mut world = lab.fake("releases-fat.json");
    world.github_dead = true;
    let (ended, out) = lab.sync(&latest, &env, &mut world);
    assert_eq!(failed_child(ended, &out).0, "github", "main's tip");
    let (ended, out) = lab.sync(
        &Options {
            no_deploy: true,
            ..Options::default()
        },
        &lab.env(),
        &mut world,
    );
    assert_eq!(failed_child(ended, &out).0, "github", "the release list");
    assert!(
        world.calls.iter().all(|c| matches!(c, Call::Github(_))),
        "{:?}",
        world.calls
    );
}

#[test]
fn a_site_dir_with_spaces_syncs() {
    let lab = Lab::at("site repo", true);
    let mut world = lab.fake("releases-fat.json");
    let (ended, out) = lab.sync(
        &Options {
            dry_run: true,
            ..Options::default()
        },
        &lab.env(),
        &mut world,
    );
    assert!(
        matches!(ended, Ok(Outcome::Planned { .. })),
        "{ended:?}\n{out}"
    );
    assert_eq!(lab.status(), "");
    let (ended, out) = lab.run(&mut world);
    deployed(&ended, &out);
    assert_eq!(world.deploys(), std::slice::from_ref(&lab.site));
    assert_eq!(lab.head(), lab.origin_main());
}

#[test]
fn the_engine_is_built_on_the_pin_and_says_which() {
    let lab = Lab::new();
    let mut world = lab.fake("releases-fat.json");
    let (ended, out) = lab.run(&mut world);
    deployed(&ended, &out);
    assert_eq!(world.builds(), [(lab.public_sha.clone(), PIN.to_string())]);
    assert!(out.contains(&format!("on the wasm32 pin {PIN}")), "{out}");

    let lab = Lab::new();
    let mut world = lab.fake("releases-fat.json");
    let env = Env {
        wasm_pin: None,
        ..lab.env()
    };
    let (ended, out) = lab.sync(&Options::default(), &env, &mut world);
    deployed(&ended, &out);
    assert_eq!(
        world.builds(),
        [(lab.public_sha.clone(), "stable".to_string())]
    );
    assert!(out.contains("on rustup's stable"), "{out}");
    assert!(!out.contains("wasm32 pin"), "{out}");
}

#[test]
fn an_engine_that_cannot_ship_fails_the_sync_and_writes_nothing() {
    for (fault, said) in [
        (EngineFault::BuildFails, "the wasm32 build exited 101"),
        (
            EngineFault::NoDescriptor,
            "verify glue's generator refused aterm_wasm.wasm",
        ),
        (EngineFault::LeaksHome, "survived the path remapping"),
        (
            EngineFault::OptimiseNothing,
            "the engine never ships pre-opt",
        ),
    ] {
        let lab = Lab::new();
        let before = lab.head();
        let mut world = lab.fake("releases-fat.json");
        world.engine = fault;
        let (ended, out) = lab.run(&mut world);
        let (child, why) = failed_child(ended, &out);
        assert_eq!(child, "the /terminal engine build", "{fault:?}: {why}");
        assert!(
            why.starts_with("the /terminal engine build failed: "),
            "{fault:?}: {why}"
        );
        assert!(why.contains(said), "{fault:?}: {why}");
        assert_eq!(world.builds().len(), 1, "{fault:?}");
        assert_eq!(
            (lab.head(), lab.status()),
            (before, String::new()),
            "{fault:?}"
        );
        let term = lab.site.join("public/assets/term");
        assert!(
            term.join("aterm_wasm.000000000000.wasm").is_file(),
            "{fault:?}"
        );
        assert!(!term.join("ENGINE_SOURCE").exists(), "{fault:?}");
        assert!(world.deploys().is_empty(), "{fault:?}");
    }
}

/// AN ENGINE BUILD A SIGNAL ENDED KEEPS ITS SCRATCH: what it started never showed it ran,
/// so the sync fails naming the scratch it keeps, stamped for a later sync to remove; one
/// that exited on its own takes its scratch with it.
#[test]
fn an_engine_build_a_signal_ended_keeps_its_scratch() {
    let scratch_of = |world: &Fake| {
        world
            .calls
            .iter()
            .find_map(|c| match c {
                Call::Build { sources, .. } => sources.parent().map(Path::to_path_buf),
                _ => None,
            })
            .expect("the engine was built")
    };
    let lab = Lab::new();
    let mut world = lab.fake("releases-fat.json");
    world.engine = EngineFault::Killed;
    let (ended, out) = lab.run(&mut world);
    let (child, why) = failed_child(ended, &out);
    assert_eq!(child, "the /terminal engine build", "{why}");
    let kept = scratch_of(&world);
    let named = why
        .split("; ")
        .nth(1)
        .and_then(|rest| rest.split(" stays while").next())
        .unwrap_or_else(|| panic!("no kept scratch named: {why}"));
    assert_eq!(
        std::fs::canonicalize(named).ok(),
        Some(kept.clone()),
        "{why}"
    );
    assert!(kept.join("clone").is_dir(), "{why}");
    assert!(
        kept.join(aterm_uds::deadline::EXEC_STAMP).is_file(),
        "{why}"
    );
    std::fs::remove_dir_all(&kept).unwrap();

    let mut world = lab.fake("releases-fat.json");
    world.engine = EngineFault::BuildFails;
    let (ended, out) = lab.run(&mut world);
    let (_, why) = failed_child(ended, &out);
    assert!(!why.contains(" stays while"), "{why}");
    assert!(!scratch_of(&world).exists(), "{why}");
}

#[test]
fn the_promote_time_run_defers_to_the_cut() {
    let lab = Lab::new();
    let before = lab.head();
    let engine = Env {
        staging_commit: Some("1111111111111111111111111111111111111111".into()),
        ..lab.env()
    };
    for dry_run in [false, true] {
        let mut world = lab.fake("releases-fat.json");
        let opts = Options {
            dry_run,
            ..Options::default()
        };
        let (ended, out) = lab.sync(&opts, &engine, &mut world);
        assert_eq!(ended, Ok(Outcome::Deferred), "{out}");
        assert!(world.calls.is_empty(), "{:?}", world.calls);
        assert_eq!((lab.head(), lab.status()), (before.clone(), String::new()));
    }
    let mut world = lab.fake("releases-fat.json");
    let latest = Options {
        latest: true,
        ..Options::default()
    };
    let (ended, out) = lab.sync(&latest, &engine, &mut world);
    deployed(&ended, &out);
    assert_eq!(world.builds().len(), 1);

    let lab = Lab::new();
    let mut world = lab.fake("releases-fat.json");
    let (ended, out) = lab.run(&mut world);
    deployed(&ended, &out);
}

#[test]
fn a_failed_push_deploys_nothing_and_the_next_run_finishes_the_job() {
    let lab = Lab::new();
    lab.refuse_pushes(true);
    let mut world = lab.fake("releases-fat.json");
    let (ended, out) = lab.run(&mut world);
    assert_eq!(failed_child(ended, &out).0, "git push");
    assert!(world.deploys().is_empty());
    assert_eq!(world.count(|c| matches!(c, Call::Live(_))), 0);
    assert_eq!(lab.commits(), 2);
    assert_eq!(
        lab.git.ok(&lab.origin, &["rev-list", "--count", "main"]),
        "1",
        "origin has nothing new"
    );
    lab.refuse_pushes(false);
    world.calls.clear();
    let (ended, out) = lab.run(&mut world);
    deployed(&ended, &out);
    assert_eq!(lab.head(), lab.origin_main());
    assert_eq!(world.deploys().len(), 1);

    std::fs::write(lab.site.join("NOTES.md"), "local notes\n").unwrap();
    lab.git
        .commit_all(&lab.site_root, "site: local notes, not the sync's");
    world.calls.clear();
    let (ended, out) = lab.run(&mut world);
    assert_eq!(ended, Ok(Outcome::Synced { deployed: false }), "{out}");
    assert_eq!(
        lab.git.ok(
            &lab.site_root,
            &["rev-list", "--count", "origin/main..main"]
        ),
        "1"
    );
    assert!(world.deploys().is_empty());

    let lab = Lab::new();
    lab.refuse_pushes(true);
    let mut world = lab.fake("releases-fat.json");
    let opts = Options {
        no_deploy: true,
        ..Options::default()
    };
    let (ended, out) = lab.sync(&opts, &lab.env(), &mut world);
    assert_eq!(failed_child(ended, &out).0, "git push");
}

#[test]
fn a_failed_pull_and_its_remedy_end_in_a_deploy_and_an_owed_commit_is_shipped_once() {
    let latest = Options {
        latest: true,
        ..Options::default()
    };
    let lab = Lab::new();
    let origin = lab.origin.to_string_lossy().into_owned();
    let nowhere = lab
        .root
        .join("no-such-origin.git")
        .to_string_lossy()
        .into_owned();
    lab.git
        .ok(&lab.site_root, &["remote", "set-url", "origin", &nowhere]);
    let mut world = lab.fake("releases-fat.json");
    let (ended, out) = lab.run(&mut world);
    let (child, why) = failed_child(ended, &out);
    assert_eq!(child, "git pull");
    assert!(
        why.contains(&format!(
            "Resolve the pull by hand, and do not push: (cd {} && git pull --rebase origin main)",
            lab.site_root.display()
        )),
        "{why}"
    );
    assert!(world.deploys().is_empty());
    let git_dir = lab.site_root.join(".git");
    assert!(!git_dir.join("rebase-merge").exists() && !git_dir.join("rebase-apply").exists());
    lab.git
        .ok(&lab.site_root, &["remote", "set-url", "origin", &origin]);
    lab.git.ok(
        &lab.site_root,
        &["pull", "-q", "--rebase", "origin", "main"],
    );
    let mut env = lab.env();
    env.commit = None;
    world.main_sha = Some(lab.public_sha.clone());
    world.calls.clear();
    let (ended, out) = lab.sync(&latest, &env, &mut world);
    deployed(&ended, &out);
    assert_eq!(lab.head(), lab.origin_main());

    let lab = Lab::new();
    lab.git
        .ok(&lab.site_root, &["remote", "set-url", "origin", &nowhere]);
    let mut world = lab.fake("releases-fat.json");
    let (ended, out) = lab.run(&mut world);
    let _ = failure(ended, &out);
    let origin = lab.origin.to_string_lossy().into_owned();
    lab.git
        .ok(&lab.site_root, &["remote", "set-url", "origin", &origin]);
    lab.git.ok(
        &lab.site_root,
        &["pull", "-q", "--rebase", "origin", "main"],
    );
    lab.git
        .ok(&lab.site_root, &["push", "-q", "origin", "main"]);
    assert_eq!(lab.head(), lab.origin_main(), "nothing is left unpushed");
    world.calls.clear();
    let (ended, out) = lab.run(&mut world);
    deployed(&ended, &out);
    assert_eq!(world.deploys().len(), 1, "the owed commit is deployed");
    assert!(!lab.owed_marker().exists());
    assert_eq!(
        lab.git
            .ok(&lab.site_root, &["status", "--porcelain", "--ignored"]),
        ""
    );
    world.calls.clear();
    let (ended, out) = lab.run(&mut world);
    assert_eq!(ended, Ok(Outcome::Synced { deployed: false }), "{out}");
    assert!(world.deploys().is_empty());
    std::fs::write(lab.live.join("assets/term/engine.js"), "stale\n").unwrap();
    let (ended, out) = lab.run(&mut world);
    assert!(
        matches!(
            &ended,
            Ok(Outcome::LiveDisagrees {
                deployed: false,
                ..
            })
        ),
        "{ended:?}\n{out}"
    );
    assert!(world.deploys().is_empty());
}

#[test]
fn an_undeployed_sync_commit_is_deployed_by_the_next_run() {
    at_once([Deploy::Ok, Deploy::AuthErr], |first| {
        let lab = Lab::new();
        let mut world = lab.fake("releases-fat.json");
        world.deploy = first;
        let opts = Options {
            no_deploy: first == Deploy::Ok,
            ..Options::default()
        };
        let (ended, out) = lab.sync(&opts, &lab.env(), &mut world);
        assert!(ended.is_ok(), "{ended:?}\n{out}");
        assert!(lab.owed_marker().is_file(), "{first:?}");
        world.deploy = Deploy::Ok;
        world.calls.clear();
        let (ended, out) = lab.run(&mut world);
        deployed(&ended, &out);
        assert_eq!(world.deploys().len(), 1, "{first:?}");
        assert!(!lab.owed_marker().exists());
    });
}

#[test]
fn the_intel_mark_and_the_toolchain_facts_reach_the_committed_pages() {
    let lab = Lab::new();
    let dl = lab.root.join("dl");
    let lay = |repo: &str, tag: &str, file: &str| {
        let dir = dl
            .join("alabsystems")
            .join(repo)
            .join("releases/download")
            .join(tag);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::copy(fixture(&format!("toolchain/{file}")), dir.join(file)).unwrap();
    };
    lay("aterm", "atpkg-index-50", "index.toml");
    lay("ay", "atpkg-ay-8256", "pkg-ay-8256.toml");
    lay("trust", "atpkg-trust-9192", "pkg-trust-9192.toml");
    lay(
        "aterm",
        "atpkg-claude-2026092601",
        "pkg-claude-2026092601.toml",
    );
    let mut releases: aterm_json::Value =
        aterm_json::from_str(&read(&fixture("releases-fat.json"))).unwrap();
    let list = releases.as_array_mut().unwrap();
    for r in list.iter_mut() {
        if r.get("tag_name").and_then(aterm_json::Value::as_str) == Some("v0.63.0") {
            let fields = r.as_object_mut().unwrap();
            let assets = fields.get_mut("assets").unwrap().as_array_mut().unwrap();
            assets.push(
                aterm_json::from_str(r#"{"name":"aterm-0.63.0-x86_64-probe.txt","size":900}"#)
                    .unwrap(),
            );
        }
    }
    list.push(
        aterm_json::from_str(
            r#"{"tag_name":"atpkg-index-50","prerelease":true,"draft":false,"assets":[{"name":"index.toml","size":1}]}"#,
        )
        .unwrap(),
    );
    let mut world = lab.fake(&aterm_json::to_string(&releases).unwrap());
    let env = Env {
        download_base: format!("file://{}", dl.display()),
        ..lab.env()
    };
    let (ended, out) = lab.sync(&Options::default(), &env, &mut world);
    deployed(&ended, &out);
    let probe = "https://github.com/alabsystems/aterm/releases/download/v0.63.0/aterm-0.63.0-x86_64-probe.txt";
    for name in ["aterm.html", "terminal.html"] {
        let page = lab.page(name);
        assert_eq!(
            pages::attribute_values(&page, "data-intel-proof"),
            [probe],
            "{name}"
        );
        assert_eq!(
            pages::attribute_values(&page, "data-toolchain-x86_64"),
            ["partial"],
            "{name}"
        );
        assert_eq!(
            pages::attribute_values(&page, "data-toolchain-aarch64"),
            ["full"],
            "{name}"
        );
    }

    let lab = Lab::new();
    let mut world = lab.fake("releases-fat.json");
    let (ended, out) = lab.run(&mut world);
    deployed(&ended, &out);
    for name in ["aterm.html", "terminal.html"] {
        let page = lab.page(name);
        for attr in [
            "data-intel-proof",
            "data-toolchain-x86_64",
            "data-toolchain-aarch64",
        ] {
            assert!(
                pages::attribute_values(&page, attr).is_empty(),
                "{name} {attr}"
            );
        }
    }
}

#[test]
fn an_installer_the_site_cannot_serve_is_refused_before_anything_is_written() {
    for (label, edit, words) in [
        (
            "no installer",
            (|src: &Path| std::fs::remove_file(src.join("tools/install.sh")).unwrap()) as fn(&Path),
            "the public commit has no readable tools/install.sh to serve as /install.sh",
        ),
        (
            "another channel",
            |src: &Path| {
                let path = src.join("tools/install.sh");
                let text = std::fs::read_to_string(&path).unwrap().replace(
                    "OFFICIAL_REPO_SLUG=\"alabsystems/aterm\"",
                    "OFFICIAL_REPO_SLUG=\"someone-else/aterm\"",
                );
                std::fs::write(&path, text).unwrap();
            },
            "does not declare OFFICIAL_REPO_SLUG=\"alabsystems/aterm\"",
        ),
    ] {
        let mut lab = Lab::new();
        lab.publish_with(true, edit);
        let before = lab.head();
        let mut world = lab.fake("releases-fat.json");
        let (ended, out) = lab.run(&mut world);
        let why = refused(ended, &out, Refusal::Installer);
        assert!(why.contains(words), "{label}: {why}");
        assert!(
            world.builds().is_empty() && world.deploys().is_empty(),
            "{label}"
        );
        assert_eq!(
            (lab.head(), lab.status()),
            (before, String::new()),
            "{label}"
        );
        assert!(!lab.site.join("public/install.sh").exists(), "{label}");
    }
}

#[test]
fn a_live_installer_that_is_missing_or_stale_is_named() {
    let lab = Lab::new();
    let mut world = lab.fake("releases-fat.json");
    let (ended, out) = lab.run(&mut world);
    deployed(&ended, &out);
    assert_eq!(read(&lab.live.join("install.sh")), read(&installer()));
    for (stale, want) in [
        (None, Difference::InstallerNotFetched),
        (
            Some("#!/bin/sh\necho stale\n"),
            Difference::InstallerDiffers,
        ),
    ] {
        let live = lab.live.join("install.sh");
        match stale {
            None => std::fs::remove_file(&live).unwrap(),
            Some(text) => std::fs::write(&live, text).unwrap(),
        }
        world.calls.clear();
        let (ended, out) = lab.run(&mut world);
        assert_eq!(
            ended,
            Ok(Outcome::LiveDisagrees {
                deployed: false,
                why: vec![Disagreement::Page(want.clone())],
            }),
            "{out}"
        );
        assert!(world.deploys().is_empty());
    }
}
