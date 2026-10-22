// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The helpers every test module shares: scratch directories, the canonical config, clients
//! over agh-fake or a scripted transport, and CLI contexts.

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

/// The simulated clock's start, in Unix seconds.
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

/// `create_private_dir(dir)` (its parent must exist), then `write_private(dir, name, token +
/// "\n")`.
#[cfg(unix)]
pub fn write_token(dir: &Path, name: &str, token: &str) -> PathBuf {
    aterm_uds::private_file::create_private_dir(dir).expect("the token directory");
    aterm_uds::private_file::write_private(dir, name, format!("{token}\n").as_bytes())
        .expect("the token file");
    dir.join(name)
}

/// `create_dir_all(dirs.config.parent())`, `create_private_dir(&dirs.config)`, then
/// `write_private(&dirs.config, "agh.toml", text)`.
#[cfg(unix)]
pub fn write_config(dirs: &Dirs, text: &str) {
    if let Some(parent) = dirs.config.parent() {
        std::fs::create_dir_all(parent).expect("the config directory's parent");
    }
    aterm_uds::private_file::create_private_dir(&dirs.config).expect("the config directory");
    aterm_uds::private_file::write_private(&dirs.config, "agh.toml", text.as_bytes())
        .expect("agh.toml");
}

/// The canonical config: `fixtures/canonical.toml` with `{root}` filled. The fixture holds
/// `{root}` inside TOML basic strings, so the root's `\` and `"` are escaped first: a Windows
/// root (`C:\Temp\…`) would otherwise read as TOML escapes.
pub fn canonical_config(root: &Path) -> String {
    let root = root
        .display()
        .to_string()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    include_str!("fixtures/canonical.toml").replace("{root}", &root)
}

/// Register the dev and release tokens with `fake` (logins `fixture-dev-bot` and
/// `fixture-release-bot`), and write each requested token file where the canonical config
/// names it.
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

/// `config::parse` and `config::validate` (with `config::file_id`) of `text`; panics on a
/// fault.
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

/// A client over `HttpTransport::direct`, `endpoints(fake)`, `env()`, `Fx::simulated`, caller
/// `test`, `RetryPolicy::NONE`.
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

/// An anonymous client (no agh.toml) over a scripted `transport`, at
/// `scripted_endpoints(api)`.
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
