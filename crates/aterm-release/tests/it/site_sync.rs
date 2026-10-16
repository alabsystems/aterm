// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `ship site` end to end over the fixtures: the public repo cloned over `file://`, a
//! site checkout with a bare origin to push to, and a fixture [`World`] standing in for
//! `gh`, the release download host, the live site, the site's build and deploy scripts
//! and `firebase` — so nothing here reaches a network or the live site. Each test reads
//! what the sync left behind: the pages' parsed hrefs, notes and attributes, the site
//! repo's commits, and the typed calls the world saw.

use std::cell::Cell;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use aterm_uds::spawnfd::{Child, Command};

use crate::site::pages::{self, Difference};
use crate::site::{Disagreement, Env, Failure, Guard, Launch, Options, Outcome, Ran, World};

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

fn fixture(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/site")
        .join(rel)
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
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
        aterm_uds::spawnfd::command("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .envs(self.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .output()
            .unwrap()
    }

    /// stdout, trimmed; the test fails on a git that fails.
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

    /// A fixture commit of everything in `dir`.
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
    Gh(String),
    Download(String),
    Live(String),
    Build { commit: String, pin: Option<String> },
    Deploy(PathBuf),
    Firebase { account: Option<String> },
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
        let during = self
            .during
            .is_some_and(|sub| cmd.get_program() == "git" && cmd.get_args().any(|a| a == sub));
        if during {
            self.caught.set(Some(15));
        }
        Ok((child, Box::new(())))
    }
}

/// The fixture world: canned GitHub answers, a directory as the live site, the
/// engine build and the deploy as the site's scripts behave.
struct Fake {
    releases: String,
    main_sha: Option<String>,
    gh_dead: bool,
    live: PathBuf,
    deploy: Deploy,
    firebase_ok: bool,
    git: Git,
    signals: Rc<Signals>,
    calls: Vec<Call>,
}

impl Fake {
    fn builds(&self) -> Vec<(String, Option<String>)> {
        self.calls
            .iter()
            .filter_map(|c| match c {
                Call::Build { commit, pin } => Some((commit.clone(), pin.clone())),
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

    fn gh_api(&mut self, path: &str, _paginate: bool) -> Result<String, String> {
        self.calls.push(Call::Gh(path.into()));
        if self.gh_dead {
            return Err("gh: not logged in".into());
        }
        if path == format!("repos/{SLUG}/releases?per_page=100") {
            return Ok(self.releases.clone());
        }
        match (&self.main_sha, path == format!("repos/{SLUG}/commits/main")) {
            (Some(sha), true) => Ok(format!("{{\"sha\":\"{sha}\"}}")),
            _ => Err(format!("unexpected gh api {path}")),
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
        let path = url
            .strip_prefix(ORIGIN)
            .ok_or_else(|| format!("unreachable host: {url}"))?;
        // Firebase Hosting: a file as itself, else its clean-URL .html form
        let rel = match path.trim_start_matches('/') {
            "" => "index.html".to_string(),
            p if self.live.join(p).is_file() => p.to_string(),
            p => format!("{p}.html"),
        };
        std::fs::read(self.live.join(&rel)).map_err(|_| format!("404 {url}"))
    }

    fn build_engine(
        &mut self,
        script: &Path,
        clone: &Path,
        pin: Option<&str>,
        _out: &mut dyn std::io::Write,
    ) -> Result<(), String> {
        // asked: recorded before anything can fail, so no build asked is ever missed
        let head = self.git.output(clone, &["rev-parse", "HEAD"]);
        let commit = String::from_utf8_lossy(&head.stdout).trim().to_string();
        self.calls.push(Call::Build {
            commit: commit.clone(),
            pin: pin.map(str::to_string),
        });
        // the site script's contract: a clean checkout of aterm in, one hash-named pair
        // and an engine.js pointing at it out
        if !clone.join("crates/aterm-wasm").is_dir() || commit.len() < 12 {
            return Err(format!("no aterm checkout at {}", clone.display()));
        }
        if !self.git.ok(clone, &["status", "--porcelain"]).is_empty() {
            return Err("refusing: uncommitted changes".into());
        }
        let h = &commit[..12];
        let dest = script.parent().unwrap().join("public/assets/term");
        for e in std::fs::read_dir(&dest).unwrap() {
            let name = e.unwrap().file_name().to_string_lossy().into_owned();
            if name.starts_with("aterm_wasm.") {
                std::fs::remove_file(dest.join(name)).unwrap();
            }
        }
        std::fs::write(
            dest.join(format!("aterm_wasm.{h}.js")),
            format!("glue {commit}\n"),
        )
        .unwrap();
        std::fs::write(
            dest.join(format!("aterm_wasm.{h}.wasm")),
            format!("wasm {commit}\n"),
        )
        .unwrap();
        std::fs::write(
            dest.join("engine.js"),
            format!("export * from \"./aterm_wasm.{h}.js\";\n"),
        )
        .unwrap();
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

/// A promote's public repo and a site checkout with its origin, in one scratch dir.
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

impl Lab {
    fn new() -> Self {
        Self::at("site-repo", true)
    }

    /// The site repo at `<scratch>/<site_root>`; the public tree with a CHANGELOG.md or not.
    fn at(site_root: &str, changelog: bool) -> Self {
        let dir = aterm_tempfile::Builder::new()
            .prefix("aterm-site-sync-")
            .tempdir()
            .unwrap();
        let root = dir.path().canonicalize().unwrap();
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let git = Git {
            env: vec![
                ("HOME".into(), home.to_string_lossy().into_owned()),
                ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
                ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
                ("GIT_TERMINAL_PROMPT".into(), "0".into()),
            ],
        };
        let site_root = root.join(site_root);
        let mut lab = Self {
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
        };
        lab.publish(changelog);
        lab.make_site();
        lab
    }

    /// A promote: a new public commit (CHANGELOG.md or not), as the only commit of a
    /// fresh bare repo.
    fn publish(&mut self, changelog: bool) {
        self.generation += 1;
        let src = self.root.join(format!("public-src-{}", self.generation));
        std::fs::create_dir_all(src.join("crates/aterm-wasm")).unwrap();
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

    /// The site checkout (the fixture's public/ and its two scripts, which the fixture
    /// world stands in for and nothing runs), pushed to a fresh bare origin; the live
    /// site serves the same public/.
    fn make_site(&mut self) {
        for d in [&self.site_root, &self.origin, &self.live] {
            let _ = std::fs::remove_dir_all(d);
        }
        copy_tree(&fixture("public"), &self.site.join("public"));
        for script in ["build-terminal-wasm.sh", "deploy.sh"] {
            let path = self.site.join(script);
            // never run: the fixture world builds and deploys (and the build script's
            // text says it reads the pin, as the site's does)
            std::fs::write(
                &path,
                "#!/bin/sh\n# fixture; reads TERMINAL_WASM_TOOLCHAIN\nexit 64\n",
            )
            .unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
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
        self.git.ok(
            &self.site_root,
            &["remote", "add", "origin", &self.origin.to_string_lossy()],
        );
        self.git
            .ok(&self.site_root, &["push", "-q", "-u", "origin", "main"]);
        copy_tree(&self.site.join("public"), &self.live);
    }

    /// What the release's after-cut step hands the sync, for this lab.
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
            masters: Vec::new(),
            now_unix: 1_790_000_000,
            git_env: self.git.env.clone(),
        }
    }

    /// The world, serving `releases` (a fixture file's name, or JSON).
    fn fake(&self, releases: &str) -> Fake {
        let releases = if releases.starts_with(['[', '{']) {
            releases.to_string()
        } else {
            read(&fixture(releases))
        };
        Fake {
            releases,
            main_sha: None,
            gh_dead: false,
            live: self.live.clone(),
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

    /// The default run: the env as given, no flags.
    fn run(&self, world: &mut Fake) -> (Result<Outcome, Failure>, String) {
        self.sync(&Options::default(), &self.env(), world)
    }

    fn page(&self, name: &str) -> String {
        read(&self.site.join("public").join(name))
    }

    fn head(&self) -> String {
        self.git.ok(&self.site_root, &["rev-parse", "HEAD"])
    }

    /// origin's main, as a fetch sees it now.
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

    /// The site origin refuses every push (a protected branch, a lost credential).
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

fn failure(ended: Result<Outcome, Failure>, out: &str) -> String {
    match ended {
        Err(Failure(why)) => why,
        other => panic!("expected a failure, got {other:?}\n{out}"),
    }
}

/// TODAY'S RELEASE: the newest stable release's versioned DMG is what the buttons name,
/// at its true size; the engine is rebuilt from the promoted commit on the wasm32 pin;
/// `/releases` and the sitemap follow; ONE commit of exactly the owned files is pushed,
/// deployed once and verified live. A re-run then has nothing to do.
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
    // the Linux tarball of the newest release that carries one, on /aterm and /terminal only
    for name in ["aterm.html", "terminal.html"] {
        assert_eq!(
            pages::attribute_values(&lab.page(name), "data-href-linux-x86_64"),
            [LINUX63]
        );
    }
    assert!(pages::attribute_values(&lab.page("index.html"), "data-href-linux-x86_64").is_empty());
    let aterm = lab.page("aterm.html");
    assert!(aterm.contains("<a href=\"/releases\">release notes</a>"));

    // the engine: rebuilt once, from the promoted commit, on the wasm32 pin
    assert_eq!(
        world.builds(),
        [(lab.public_sha.clone(), Some(PIN.to_string()))]
    );
    let term = lab.site.join("public/assets/term");
    assert_eq!(read(&term.join("ENGINE_SOURCE")).trim(), lab.public_sha);
    assert!(
        read(&term.join("engine.js")).contains(&format!("aterm_wasm.{}.js", &lab.public_sha[..12]))
    );
    assert!(
        !term.join("aterm_wasm.000000000000.js").exists(),
        "the old pair is gone"
    );

    // /releases: the reviewed bytes; the stylesheet; the sitemap — today for each page that
    // changed and for /terminal's rebuilt engine, /releases added, the rest untouched
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

    // one commit, of exactly the owned files that changed, by the configured identity,
    // saying the promoted version is not cut yet; pushed; nothing left over
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
    let h = &lab.public_sha[..12];
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
    // GitHub was only read: the world has no write, and every read was a gh api GET
    assert!(
        world
            .calls
            .iter()
            .all(|c| !matches!(c, Call::Gh(p) if !p.starts_with("repos/")))
    );
    // nothing reached past the live site and the download host
    assert!(world.calls.iter().all(|c| match c {
        Call::Live(u) => u.starts_with(ORIGIN),
        Call::Download(u) => u.starts_with("file://") || u.starts_with("https://github.com/"),
        _ => true,
    }));

    // a re-run: ENGINE_SOURCE skips the rebuild, nothing to commit, no deploy, still verified
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

/// THE NEXT RELEASE: the lean cut's DMG, 31 MB in decimal units; no Linux tarball in the
/// whole history removes the attribute rather than deriving one; and flipping back to
/// the older list rewrites every page again.
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

/// The public snapshot exports no CHANGELOG.md: `/releases` is rendered from the stable
/// release bodies, the cutter's asset legend dropped — the reviewed bytes.
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

/// A DRY RUN plans and changes nothing — the same plan a real run would carry out, the
/// engine rebuild included — whether asked by flag or by `PUB_DRY_RUN`.
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
        assert!(!lab.site.join("public/assets/term/ENGINE_SOURCE").exists());
    }
}

/// A DMG the API gave no size for is still the download; the page names it alone,
/// never a guessed size.
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

/// NO USABLE SITE CHECKOUT is exit 3 having touched nothing — a missing directory, or one
/// without its deploy script, which is named; `--skip-site` is always a clean no-op.
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

/// A MISSING OR EXPIRED FIREBASE LOGIN is not a failure of the release: committed and
/// pushed, not deployed, and owed. An unrelated deploy failure is not laundered into
/// one: the login is asked (with the account deploy.sh pins), and only a login that
/// cannot list projects either makes an unrecognised failure a login one.
#[test]
fn a_missing_firebase_login_commits_and_pushes_but_never_hides_a_real_failure() {
    for (deploy, firebase_ok, want_failure) in [
        (Deploy::AuthErr, true, false),
        (Deploy::NoCli, true, false),
        (Deploy::Fail, true, true),
        (Deploy::OddFail, false, false),
        (Deploy::OddFail, true, true),
    ] {
        let lab = Lab::new();
        let mut world = lab.fake("releases-fat.json");
        world.deploy = deploy;
        world.firebase_ok = firebase_ok;
        let (ended, out) = lab.run(&mut world);
        if want_failure {
            let why = failure(ended, &out);
            assert!(why.contains("deploy.sh failed (rc 1)"), "{deploy:?}: {why}");
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
}

/// `--no-deploy` (or SITE_NO_DEPLOY=1) commits and pushes only, and says so — never
/// "already shown"; a re-run with nothing to commit stays off the live site rather than
/// calling the undeployed site wrong.
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

/// AN INTERRUPT STARTS NOTHING FURTHER: a SIGTERM that arrives while the push runs lets
/// the push finish, and then no deploy, login probe or live check starts — the sync
/// says so, the commit is owed, and the next run deploys it. One that arrives during
/// the clone stops the sync there, said as an interrupt whatever the step it stopped
/// said, with nothing built, committed or deployed.
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
        "interrupted (signal 15) before the deploy — nothing further was started"
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
    let why = failure(ended, &out);
    assert!(
        why.starts_with(
            "interrupted (signal 15) — nothing further was started; the step it \
                         stopped said: "
        ),
        "{why}"
    );
    assert!(world.builds().is_empty() && world.deploys().is_empty());
    assert_eq!(
        world.count(|c| matches!(c, Call::Gh(_) | Call::Download(_))),
        0
    );
    assert_eq!((lab.head(), lab.status()), (before, String::new()));
}

/// DEPLOYED, BUT THE LIVE SITE DISAGREES: exit 4, naming what differs — a CDN that still
/// serves the old engine, or a live engine built from a commit the site repo never
/// committed (any clean-checkout deploy would silently roll it back).
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

/// WHAT IS DEPLOYED MUST BE WHAT IS COMMITTED: an engine the site repo ignores is refused
/// at the commit (git's own words), and one git cannot see (the marker assume-unchanged)
/// by comparing the marker on disk with the committed one — both before any deploy.
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
    let why = failure(ended, &out);
    assert!(
        why.contains("ignored by one of your .gitignore files"),
        "{why}"
    );
    assert!(why.contains("public/assets/term/ENGINE_SOURCE"), "{why}");
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
    let why = failure(ended, &out);
    assert!(
        why.contains("deploying would publish an engine that is not in git"),
        "{why}"
    );
    assert!(
        why.contains(&lab.public_sha) && why.contains(&first),
        "{why}"
    );
    assert!(world.deploys().is_empty());
}

/// A PAGE THAT LOST A RULE'S SHAPE fails loudly, before the engine rebuild makes the first
/// write — so the checkout is left exactly as it was; and edits to an owned page that
/// are not committed are refused, never swept into the release commit.
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
        let why = failure(ended, &out);
        assert!(why.contains(rule), "{why}");
        assert_eq!(lab.head(), before);
        assert!(world.builds().is_empty() && world.deploys().is_empty());
        assert_eq!(lab.status(), "");
    }
    let lab = Lab::new();
    let path = lab.site.join("public/aterm.html");
    std::fs::write(&path, read(&path) + "<!-- wip -->\n").unwrap();
    let mut world = lab.fake("releases-fat.json");
    let (ended, out) = lab.run(&mut world);
    let why = failure(ended, &out);
    assert!(
        why.contains("uncommitted changes in files this sync rewrites"),
        "{why}"
    );
    assert!(
        world.calls.is_empty(),
        "refused before GitHub was asked: {:?}",
        world.calls
    );
}

/// A RELEASE STREAM WITH NO VERSIONED DMG ANYWHERE is refused before anything is written:
/// the site keeps its previous link.
#[test]
fn a_stream_with_nothing_installable_keeps_the_previous_link() {
    let lab = Lab::new();
    let before = lab.head();
    let nodmg = r#"[{"tag_name":"v0.65.0","prerelease":false,"draft":false,"assets":[
        {"name":"aterm.dmg","size":31457280},{"name":"aterm-0.65.0-mac.zip","size":28219832}]}]"#;
    let mut world = lab.fake(nodmg);
    let (ended, out) = lab.run(&mut world);
    let why = failure(ended, &out);
    assert!(
        why.contains("no stable release carries its versioned DMG"),
        "{why}"
    );
    assert!(world.builds().is_empty() && world.deploys().is_empty());
    assert_eq!((lab.head(), lab.status()), (before, String::new()));
    assert_eq!(
        pages::hrefs("index.html", &lab.page("index.html")),
        ["https://github.com/alabsystems/aterm/releases/download/v0.61.0/aterm-0.61.0.dmg"]
    );
}

/// THE COMMIT IS THE PROMOTED ONE: an unknown or short sha, or none, is refused; `--latest`
/// asks the authenticated gh for main's tip; a gh that cannot answer stops both GitHub
/// reads, naming gh, with nothing fetched around it.
#[test]
fn the_commit_is_the_promoted_one_and_github_is_read_through_gh_only() {
    let lab = Lab::new();
    for (commit, words) in [
        (
            Some("0000000000000000000000000000000000000000"),
            "is not on alabsystems/aterm",
        ),
        (Some("abc"), "must be a full hex commit sha"),
        (None, "PUB_PUBLIC_COMMIT is required"),
    ] {
        let mut world = lab.fake("releases-fat.json");
        let env = Env {
            commit: commit.map(str::to_string),
            ..lab.env()
        };
        let (ended, out) = lab.sync(&Options::default(), &env, &mut world);
        let why = failure(ended, &out);
        assert!(why.contains(words), "{commit:?}: {why}");
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
        Call::Gh(format!("repos/{SLUG}/commits/main"))
    );
    assert_eq!(
        read(&lab.site.join("public/assets/term/ENGINE_SOURCE")).trim(),
        lab.public_sha
    );

    let lab = Lab::new();
    let mut world = lab.fake("releases-fat.json");
    world.gh_dead = true;
    let (ended, out) = lab.sync(&latest, &env, &mut world);
    assert!(
        failure(ended, &out)
            .contains("could not read the tip of alabsystems/aterm main through gh")
    );
    let (ended, out) = lab.sync(
        &Options {
            no_deploy: true,
            ..Options::default()
        },
        &lab.env(),
        &mut world,
    );
    assert!(
        failure(ended, &out).contains("could not list releases for alabsystems/aterm through gh")
    );
    assert!(
        world.calls.iter().all(|c| matches!(c, Call::Gh(_))),
        "{:?}",
        world.calls
    );
}

/// A site checkout whose path holds spaces plans and syncs like any other.
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

/// THE ENGINE'S COMPILER: the wasm32 pin is handed to the site's build, and the transcript
/// never claims a pin the build did not use — a site script that does not read it yet
/// is said to build on its own default; with no pin, nothing is handed.
#[test]
fn the_engine_is_handed_the_pin_and_never_claims_one_it_did_not_use() {
    let lab = Lab::new();
    std::fs::write(
        lab.site.join("build-terminal-wasm.sh"),
        "#!/bin/sh\n# from before the pin\nexit 64\n",
    )
    .unwrap();
    lab.git
        .commit_all(&lab.site_root, "site: a build script from before the pin");
    lab.git
        .ok(&lab.site_root, &["push", "-q", "origin", "main"]);
    let mut world = lab.fake("releases-fat.json");
    let (ended, out) = lab.run(&mut world);
    deployed(&ended, &out);
    assert_eq!(
        world.builds(),
        [(lab.public_sha.clone(), Some(PIN.to_string()))]
    );
    assert!(out.contains(&format!("NOT the wasm32 pin {PIN}")), "{out}");
    assert!(!out.contains(&format!("on the wasm32 pin {PIN}")), "{out}");

    let lab = Lab::new();
    let mut world = lab.fake("releases-fat.json");
    let env = Env {
        wasm_pin: None,
        ..lab.env()
    };
    let (ended, out) = lab.sync(&Options::default(), &env, &mut world);
    deployed(&ended, &out);
    assert_eq!(world.builds(), [(lab.public_sha.clone(), None)]);
}

/// THE PROMOTE-TIME RUN DEFERS TO THE CUT: the engine's hook carries PUB_STAGING_COMMIT and
/// no `--latest`, before the version has a release — nothing is asked, built, committed or
/// deployed (a dry run too). `--latest` beside the same facts syncs, and so does a hand
/// run naming a commit without them.
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

/// ORIGIN HAS IT BEFORE ANYTHING IS DEPLOYED: a refused push or a failed pull stops the run
/// before deploy.sh with the sync commit kept here, and the next run finishes the job —
/// pushing the leftover and deploying it. Local commits touching nothing of the sync's
/// are the site repo's own: neither pushed nor deployed on their account.
#[test]
fn a_failed_push_deploys_nothing_and_the_next_run_finishes_the_job() {
    let lab = Lab::new();
    lab.refuse_pushes(true);
    let mut world = lab.fake("releases-fat.json");
    let (ended, out) = lab.run(&mut world);
    let why = failure(ended, &out);
    assert!(
        why.contains("push to origin main failed (above) — the site commit is on THIS MACHINE ONLY and NOTHING WAS DEPLOYED"),
        "{why}"
    );
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

    // a local commit of the site repo's own: not pushed, nothing deployed on its account
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

    // --no-deploy promises the push too, so a refused push fails it as well
    let lab = Lab::new();
    lab.refuse_pushes(true);
    let mut world = lab.fake("releases-fat.json");
    let opts = Options {
        no_deploy: true,
        ..Options::default()
    };
    let (ended, out) = lab.sync(&opts, &lab.env(), &mut world);
    assert!(failure(ended, &out).contains("push to origin main failed"));
}

/// A FAILED PULL is as fatal and as early, leaves no rebase behind, and its remedy leads to
/// a deploy: following it (a pull by hand, no push) the re-run pushes and deploys. An
/// operator who pushes by hand anyway is still deployed — the commit is owed until a
/// deploy ships it — and once shipped nothing is owed: a live site that then differs is
/// reported, never redeployed on a guess.
#[test]
fn a_failed_pull_and_its_remedy_end_in_a_deploy_and_an_owed_commit_is_shipped_once() {
    let latest = Options {
        latest: true,
        ..Options::default()
    };
    // the remedy as said: pull by hand, re-run
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
    let why = failure(ended, &out);
    assert!(
        why.contains("pull --rebase from origin main failed (above)"),
        "{why}"
    );
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

    // the operator who pushes by hand anyway
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
    // shipped: nothing owed, no second deploy
    world.calls.clear();
    let (ended, out) = lab.run(&mut world);
    assert_eq!(ended, Ok(Outcome::Synced { deployed: false }), "{out}");
    assert!(world.deploys().is_empty());
    // nothing owed and the live site differs: reported, not redeployed
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

/// The other ways a pushed sync commit goes undeployed — `--no-deploy`, no Firebase login —
/// leave it owed, and the next plain run deploys it.
#[test]
fn an_undeployed_sync_commit_is_deployed_by_the_next_run() {
    for first in [Deploy::Ok, Deploy::AuthErr] {
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
    }
}

/// THE INTEL MARK: the chosen release's record of running its x86_64 slice, and how much
/// of the pinned toolset the index publishes per Mac architecture (read from the download
/// host), reach the committed `/aterm` and `/terminal` anchors and are compared live. With
/// no index release the facts are unknown and the pages carry no level — never a stale one.
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
