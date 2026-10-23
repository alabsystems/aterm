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
use agh::identity::MapEnv;
use agh::policy::Router;
use agh::retry::RetryPolicy;
use agh::wire::http::HttpTransport;
use agh::wire::{Endpoints, Transport};
use agh::{Github, GithubBuilder};
use agh_fake::FakeGithub;

/// The hang detector for anything that must happen.
pub const HANG: Duration = Duration::from_secs(60);

const SIM_START: u64 = 1_760_000_000;

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
    aterm_tempfile::Builder::new()
        .prefix(&format!("agh-{label}-"))
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
    github_over(Router::empty(), transport.clone(), scripted_endpoints(api))
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

/// `Ctx::for_tests(root, Arc::new(HttpTransport::direct(env)), endpoints(fake), env)`.
pub fn ctx(root: &Path, fake: &FakeGithub, env: MapEnv) -> (Ctx, Captured) {
    let transport = Arc::new(HttpTransport::direct(Arc::new(env.clone())));
    Ctx::for_tests(root, transport, endpoints(fake), env)
}

/// `cli::run` of `args`.
pub fn run_cli(ctx: &mut Ctx, args: &[&str]) -> u8 {
    let args: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
    cli::run(&args, ctx)
}
