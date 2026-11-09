// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Shared test helpers: scratch directories, canonical configs, fake/scripted clients, and CLI.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use agh::cli::{self, Captured, Ctx};
use agh::config;
#[cfg(unix)]
use agh::config::Dirs;
use agh::fx::Fx;
#[cfg(unix)]
use agh::git::HelperCommand;
use agh::identity::MapEnv;
use agh::policy::Router;
#[cfg(unix)]
use agh::policy::{Named, Permit, RouteRequest};
use agh::retry::RetryPolicy;
use agh::wire::http::HttpTransport;
use agh::wire::{Endpoints, Transport};
use agh::{Github, GithubBuilder};
use agh_fake::FakeGithub;
#[cfg(unix)]
use agh_fake::RepoSpec;

/// The hang detector for anything that must happen.
pub const HANG: Duration = Duration::from_secs(60);

/// The simulated clock's start, in Unix seconds: every test client's `Fx::simulated`.
pub const SIM_START: u64 = 1_760_000_000;

/// `text` as a repository name, which the test knows to be one.
pub fn slug(text: &str) -> agh::Slug {
    agh::Slug::parse(text).expect("a slug")
}

/// A run's stdout, one JSON value (its trailing newline trimmed).
pub fn stdout_json(captured: &Captured) -> aterm_json::Value {
    aterm_json::from_str(captured.stdout_text().trim_end()).expect("JSON on stdout")
}

/// The error object of a failed run's error JSON, the second of its stderr lines.
pub fn error_json(stderr: &str) -> aterm_json::Value {
    let line = stderr.lines().nth(1).expect("two stderr lines");
    let value: aterm_json::Value = aterm_json::from_str(line).expect("the error JSON");
    value.get("error").cloned().expect("an error object")
}

/// A fresh 0700 scratch directory.
pub fn scratch(label: &str) -> aterm_tempfile::TempDir {
    let end = label.floor_char_boundary(24);
    let prefix = format!("agh-{}-", &label[..end]);
    aterm_tempfile::Builder::new()
        .prefix(&prefix)
        .suffix(".noindex")
        .tempdir()
        .expect("a scratch directory")
}

/// Creates `dir` (`0700`) and writes `{token}\n` to `dir/name` (`0600`).
#[cfg(unix)]
pub fn write_token(dir: &Path, name: &str, token: &str) -> PathBuf {
    aterm_uds::private_file::create_private_dir(dir).expect("the token directory");
    setup_file(&dir.join(name), format!("{token}\n").as_bytes());
    dir.join(name)
}

// Writes `path` (`0600`) without `F_FULLFSYNC` to avoid volume-wide flush contention in tests.
#[cfg(unix)]
fn setup_file(path: &Path, bytes: &[u8]) {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .and_then(|mut f| f.write_all(bytes))
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
}

/// Creates `dirs.config` (`0700`) and writes `text` to `agh.toml` (`0600`).
#[cfg(unix)]
pub fn write_config(dirs: &Dirs, text: &str) {
    if let Some(parent) = dirs.config.parent() {
        std::fs::create_dir_all(parent).expect("the config directory's parent");
    }
    aterm_uds::private_file::create_private_dir(&dirs.config).expect("the config directory");
    setup_file(&dirs.config.join("agh.toml"), text.as_bytes());
}

/// Renders `fixtures/canonical.toml` with `{root}` escaped for TOML basic strings.
pub fn canonical_config(root: &Path) -> String {
    let root = root
        .display()
        .to_string()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    include_str!("fixtures/canonical.toml").replace("{root}", &root)
}

/// Registers canonical dev/release tokens with `fake` and writes requested token files.
#[cfg(unix)]
pub fn canonical_tokens(root: &Path, fake: &FakeGithub, dev: bool, release: bool) {
    let dev_token = agh_fake::token("dev");
    let release_token = agh_fake::token("release");
    fake.add_token(&dev_token, "fixture-dev-bot");
    fake.add_token(&release_token, "fixture-release-bot");
    if dev {
        write_token(&root.join("tokens"), "dev.token", &dev_token);
    }
    if release {
        write_token(&root.join("secrets"), "release.token", &release_token);
    }
}

/// An empty environment.
pub fn env() -> MapEnv {
    MapEnv::new()
}

/// Parses and validates `text` into a [`Router`], panicking on any fault.
pub fn router(text: &str, home: &Path) -> Router {
    let parsed = config::parse(text).unwrap_or_else(|f| panic!("{}: {}", f.rule, f.what));
    let validated = config::validate(&parsed, home, &config::file_id).unwrap_or_else(|faults| {
        let faults: Vec<String> = faults
            .iter()
            .map(|f| format!("{}: {}", f.rule, f.what))
            .collect();
        panic!("{}", faults.join("; "))
    });
    Router::from_validated(validated)
}

/// The loopback endpoints of `fake`.
pub fn endpoints(fake: &FakeGithub) -> Endpoints {
    let ports = fake.ports();
    Endpoints::loopback(ports.api, ports.uploads, ports.web, ports.cdn)
}

/// Builds a test [`Github`] client over `fake`'s loopback endpoints.
pub fn github(router: Router, fake: &FakeGithub) -> Github {
    let transport = Arc::new(HttpTransport::direct(Arc::new(env())));
    github_over(router, transport, endpoints(fake))
}

/// As `github`, over any transport and endpoints (a `ScriptedTransport`).
pub fn github_over(router: Router, transport: Arc<dyn Transport>, endpoints: Endpoints) -> Github {
    GithubBuilder::new(router)
        .transport(transport)
        .endpoints(endpoints)
        .env(Arc::new(env()))
        .fx(Arc::new(Fx::simulated(SIM_START)))
        .caller("test")
        .retry(RetryPolicy::NONE)
        .build()
}

/// `http://127.0.0.1:{api}`: the base of a scripted api origin's URLs.
pub fn base(api: u16) -> String {
    format!("http://127.0.0.1:{api}")
}

/// Owned header pairs.
pub fn headers(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(n, v)| ((*n).to_string(), (*v).to_string()))
        .collect()
}

/// Loopback endpoints from `api` (`api` to `api + 3`), which a scripted transport never dials.
pub fn scripted_endpoints(api: u16) -> Endpoints {
    Endpoints::loopback(api, api + 1, api + 2, api + 3)
}

/// Builds an anonymous [`Github`] client over a scripted `transport`.
pub fn scripted(transport: &Arc<agh::wire::scripted::ScriptedTransport>, api: u16) -> Github {
    scripted_with_retry(transport, api, RetryPolicy::NONE)
}

/// Builds an anonymous [`Github`] client over a scripted `transport` with `retry`.
pub fn scripted_with_retry(
    transport: &Arc<agh::wire::scripted::ScriptedTransport>,
    api: u16,
    retry: RetryPolicy,
) -> Github {
    GithubBuilder::new(Router::empty())
        .transport(transport.clone())
        .endpoints(scripted_endpoints(api))
        .env(Arc::new(env()))
        .fx(Arc::new(Fx::simulated(SIM_START)))
        .caller("test")
        .retry(retry)
        .build()
}

/// `path`'s mode set to `mode`.
#[cfg(unix)]
pub fn chmod(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
}

/// A fresh path for a tape in `dir` that does not exist yet: `{dir}/{label}.atape`.
pub fn tape_path(dir: &Path, label: &str) -> PathBuf {
    dir.join(format!("{label}.atape"))
}

/// `Ctx::for_tests(root, Arc::new(HttpTransport::direct(env)), endpoints(fake), env)`, with
/// this process's vault adoption `GET /user` bounded by `HANG`: a fake must answer it.
pub fn ctx(root: &Path, fake: &FakeGithub, env: MapEnv) -> (Ctx, Captured) {
    #[cfg(unix)]
    agh::github::users::set_learn_deadlines_for_test(HANG);
    let transport = Arc::new(HttpTransport::direct(Arc::new(env.clone())));
    Ctx::for_tests(root, transport, endpoints(fake), env)
}

/// `cli::run` of `args`.
pub fn run_cli(ctx: &mut Ctx, args: &[&str]) -> u8 {
    let args: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
    cli::run(&args, ctx)
}

/// The `[[writers]]` row the M2 tests grant: `fixture-caller` on `alabsystems/aterm`.
#[cfg(unix)]
pub const WRITERS: &str =
    "\n[[writers]]\ncaller = \"fixture-caller\"\nrepos = [\"alabsystems/aterm\"]\n";

/// The canonical config plus `extra`, written as agh.toml under `Dirs::under(root)`, and the
/// router `config::load_for_host` reads back (origin File, so grants count).
#[cfg(unix)]
pub fn file_router(root: &Path, extra: &str) -> (Dirs, Router) {
    let dirs = Dirs::under(root);
    write_config(&dirs, &format!("{}{extra}", canonical_config(root)));
    let validated = config::load_for_host(&dirs)
        .unwrap_or_else(|e| panic!("agh.toml did not load: {}", e.why))
        .unwrap_or_else(|| panic!("no agh.toml at {}", dirs.config.display()));
    (dirs, Router::from_validated(validated))
}

/// `github(router, fake)` with `journal(dirs.clone())`.
#[cfg(unix)]
pub fn writer(router: Router, fake: &FakeGithub, dirs: &Dirs) -> Github {
    GithubBuilder::new(router)
        .transport(Arc::new(HttpTransport::direct(Arc::new(env()))))
        .endpoints(endpoints(fake))
        .env(Arc::new(env()))
        .fx(Arc::new(Fx::simulated(SIM_START)))
        .caller("test")
        .retry(RetryPolicy::NONE)
        .journal(dirs.clone())
        .build()
}

/// A permit for `req` from `gh`'s router and `gh.presence`, or a panic naming the refusal.
/// Its body is `gh.router().permit(req, &|p| gh.presence(p))`, panicking with the error's why:
/// `Router::permit`, never `Github::permit`, so a test that mints its permit here needs no
/// client-side binding.
#[cfg(unix)]
pub fn permit(gh: &Github, req: &RouteRequest) -> Permit {
    gh.router()
        .permit(req, &|p| gh.presence(p))
        .unwrap_or_else(|e| panic!("no permit: {}", e.why))
}

/// `Named::Literal` of `slug`, granted to `fixture-caller`.
#[cfg(unix)]
pub fn granted(gh: &Github, slug: &str) -> Named {
    let slug = agh::Slug::parse(slug).unwrap_or_else(|e| panic!("not a slug: {}", e.why));
    let target = gh
        .router()
        .grant("fixture-caller", &slug)
        .unwrap_or_else(|e| panic!("no grant: {}", e.why));
    Named::Literal(target)
}

/// The default branch's head in `canonical_repos`: the sha M1's fixtures already use.
#[cfg(unix)]
pub const MAIN_SHA: &str = "0123456789abcdef0123456789abcdef01234567";

/// `fixture-dev/app` (private, push for `fixture-dev-bot`) and `alabsystems/aterm` (public,
/// push for `fixture-release-bot`) on `fake`, each with branch `main` at `MAIN_SHA`, and their
/// ids.
#[cfg(unix)]
pub fn canonical_repos(fake: &FakeGithub) -> (u64, u64) {
    let dev = fake.add_repo(
        "fixture-dev/app",
        RepoSpec::private().push("fixture-dev-bot"),
    );
    let public = fake.add_repo(
        "alabsystems/aterm",
        RepoSpec::public().push("fixture-release-bot"),
    );
    fake.set_branch("fixture-dev/app", "main", MAIN_SHA);
    fake.set_branch("alabsystems/aterm", "main", MAIN_SHA);
    (dev, public)
}

/// The agh binary as the git helper: the package's `agh` ([`aterm_tempfile::bin_exe`]), with no
/// words before `auth git-credential`.
#[cfg(unix)]
pub fn helper() -> HelperCommand {
    HelperCommand {
        exe: aterm_tempfile::bin_exe("agh"),
        args: Vec::new(),
    }
}

/// `bytes` written to `dir/name`, for an upload; its path.
#[cfg(unix)]
pub fn file(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    path
}
