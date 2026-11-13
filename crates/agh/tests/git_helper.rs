// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The git credential helper, `auth git-credential`, and its grants (WP7).
//!
//! Each test runs `cli::run` of `auth git-credential …` with `ctx.with_stdin(..)`. A grant is
//! written by the test as the JSON of §4.6.13, under the scratch `grants` directory, for its
//! own pid and `read_process_start` boot: the ancestry walk starts at the helper itself, which
//! here is the test process. The test context's clock is `common::SIM_START`.

use std::path::Path;

use agh::cli::{Captured, Ctx};
use agh::config::Dirs;
use agh::identity::MapEnv;
use agh_fake::FakeGithub;
use aterm_json::Value;

use crate::common;

const DEV_REQUEST: &str = "protocol=https\nhost=github.com\npath=fixture-dev/app.git\n\n";
const RELEASE_REQUEST: &str = "protocol=https\nhost=github.com\npath=alabsystems/aterm.git\n\n";
const QUIT: &str = "quit=1\n";

/// A scratch root, its `Dirs`, agh-fake (for the tokens it mints), and the agh.toml written.
struct World {
    root: aterm_tempfile::TempDir,
    dirs: Dirs,
    fake: FakeGithub,
}

impl World {
    /// The canonical agh.toml and both token files.
    fn canonical(label: &str) -> World {
        World::with_config(label, common::canonical_config)
    }

    /// The canonical agh.toml with a `reader` profile (`fixture-reader`, its own token file)
    /// heading `fixture-dev`'s read chain, `dev` still its writer.
    fn with_reader(label: &str) -> World {
        let world = World::with_config(label, |root| {
            let reader = format!(
                "[profiles.reader]\ntoken_file = \"{}/tokens/reader.token\"\ntier = \"dev\"\n\
                 login = \"fixture-reader\"\n\n",
                root.display()
            );
            let canonical = common::canonical_config(root);
            let owners = canonical.replacen(
                "name = \"fixture-dev\"\ntier = \"dev\"\nread = [\"dev\"]",
                "name = \"fixture-dev\"\ntier = \"dev\"\nread = [\"reader\", \"dev\"]",
                1,
            );
            assert_ne!(owners, canonical, "fixture-dev's read chain was rewritten");
            format!("{reader}{owners}")
        });
        let reader = agh_fake::token("reader");
        world.fake.add_token(&reader, "fixture-reader");
        common::write_token(&world.root.path().join("tokens"), "reader.token", &reader);
        world
    }

    fn with_config(label: &str, text: impl Fn(&Path) -> String) -> World {
        let root = common::scratch(label);
        let fake = FakeGithub::start();
        let dirs = Dirs::under(root.path());
        common::write_config(&dirs, &text(root.path()));
        common::canonical_tokens(root.path(), &fake, true, true);
        World { root, dirs, fake }
    }

    fn ctx(&self, env: MapEnv) -> (Ctx, Captured) {
        common::ctx(self.root.path(), &self.fake, env)
    }

    /// `auth git-credential {op}` with `request` on stdin: its exit and stdout, after
    /// asserting stderr is empty.
    fn helper(&self, env: MapEnv, op: &str, request: &[u8]) -> (u8, String) {
        let (ctx, captured) = self.ctx(env);
        let mut ctx = ctx.with_stdin(request.to_vec());
        let exit = common::run_cli(&mut ctx, &["auth", "git-credential", op]);
        assert_eq!(captured.stderr_text(), "", "the helper wrote to stderr");
        (exit, captured.stdout_text())
    }

    /// `auth git-credential get` of `request`: its stdout, after asserting exit 0.
    fn get(&self, env: MapEnv, request: &str) -> String {
        let (exit, out) = self.helper(env, "get", request.as_bytes());
        assert_eq!(exit, 0, "{out}");
        out
    }

    /// `whoami --git {url}`: its JSON, after asserting exit 0.
    fn whoami(&self, env: MapEnv, url: &str) -> Value {
        let (mut ctx, captured) = self.ctx(env);
        let exit = common::run_cli(&mut ctx, &["whoami", "--git", url]);
        assert_eq!(exit, 0, "{}", captured.stderr_text());
        common::stdout_json(&captured)
    }

    /// Writes a grant `{nonce}.json` for `(pid, start)` into the scratch `grants` directory
    /// (0700, the file 0600), as the runner would: its nonce.
    fn grant(&self, grant: &TestGrant) -> String {
        let parent = self
            .dirs
            .state
            .parent()
            .expect("the state directory's parent");
        std::fs::create_dir_all(parent).expect("the state directory's parent");
        aterm_uds::private_file::create_private_dir(&self.dirs.state).expect("the state");
        aterm_uds::private_file::create_private_dir(&self.dirs.grants()).expect("the grants");
        let nonce = aterm_uds::rand::hex_token::<16>().expect("a nonce");
        let body = aterm_json::json!({
            "expires_at": grant.expires_at,
            "pid": grant.pid,
            "profile": grant.profile,
            "slug": grant.slug,
            "start": grant.start,
            "v": 1,
        })
        .to_string();
        let path = self.dirs.grants().join(format!("{nonce}.json"));
        write_private(&path, body.as_bytes());
        nonce
    }

    fn token(&self, name: &str) -> String {
        agh_fake::token(name)
    }
}

/// A grant's fields, as the test writes them.
struct TestGrant {
    slug: &'static str,
    profile: &'static str,
    pid: u32,
    start: u64,
    expires_at: u64,
}

impl TestGrant {
    /// A grant for this process, live until ten minutes past the test context's clock.
    fn live(slug: &'static str, profile: &'static str) -> TestGrant {
        let pid = std::process::id();
        let start = aterm_uds::process::read_process_start(
            i32::try_from(pid).expect("a pid that fits an i32"),
        )
        .expect("this process's start")
        .boot
        .0;
        TestGrant {
            slug,
            profile,
            pid,
            start,
            expires_at: common::SIM_START + 600,
        }
    }
}

fn write_private(path: &Path, bytes: &[u8]) {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .and_then(|mut f| f.write_all(bytes))
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
}

fn credential(token: &str) -> String {
    format!("username=x-access-token\npassword={token}\n")
}

fn granted(nonce: &str) -> MapEnv {
    MapEnv::new().with("AGH_GIT_GRANT", nonce)
}

#[test]
fn a_dev_request_answers_the_dev_token() {
    let world = World::canonical("helper-dev");
    assert_eq!(
        world.get(MapEnv::new(), DEV_REQUEST),
        credential(&world.token("dev"))
    );
}

#[test]
fn a_pathless_request_quits() {
    let world = World::canonical("helper-pathless");
    assert_eq!(
        world.get(MapEnv::new(), "protocol=https\nhost=github.com\n\n"),
        QUIT
    );
}

#[test]
fn another_host_quits() {
    let world = World::canonical("helper-host");
    for request in [
        "protocol=https\nhost=example.com\npath=fixture-dev/app.git\n\n",
        "protocol=http\nhost=github.com\npath=fixture-dev/app.git\n\n",
        "protocol=https\nhost=github.com:8443\npath=fixture-dev/app.git\n\n",
    ] {
        assert_eq!(world.get(MapEnv::new(), request), QUIT, "{request:?}");
    }
}

#[test]
fn a_path_that_names_no_repository_quits() {
    let world = World::canonical("helper-path");
    for path in [
        "fixture-dev/app/extra.git",
        "fixture-dev",
        "%66ixture-dev/app.git",
    ] {
        let request = format!("protocol=https\nhost=github.com\npath={path}\n\n");
        assert_eq!(world.get(MapEnv::new(), &request), QUIT, "{path}");
    }
}

#[cfg(debug_assertions)]
#[test]
fn a_loopback_host_answers_in_a_debug_build() {
    let world = World::canonical("helper-loopback");
    let request = "protocol=http\nhost=127.0.0.1:4321\npath=fixture-dev/app.git\n\n";
    let pointed = MapEnv::new().with("AGH_GIT_LOOPBACK", "127.0.0.1:4321");
    assert_eq!(world.get(pointed, request), credential(&world.token("dev")));
    assert_eq!(world.get(MapEnv::new(), request), QUIT);
    let elsewhere = MapEnv::new().with("AGH_GIT_LOOPBACK", "127.0.0.1:4322");
    assert_eq!(world.get(elsewhere, request), QUIT);
}

#[test]
fn a_dev_request_never_answers_a_release_grant() {
    let world = World::canonical("helper-b5");
    let nonce = world.grant(&TestGrant::live("fixture-dev/app", "release"));
    let out = world.get(granted(&nonce), DEV_REQUEST);
    assert_eq!(out, credential(&world.token("dev")));
    assert!(!out.contains(&world.token("release")));
    let who = world.whoami(granted(&nonce), "https://github.com/fixture-dev/app.git");
    assert_eq!(who["profile"], Value::from("dev"), "{who}");
    assert_eq!(who["tier"], Value::from("dev"), "{who}");
    assert_eq!(who["grant"], Value::Null, "{who}");
}

#[test]
fn a_dev_grant_answers_only_the_writer() {
    let world = World::with_reader("helper-writer");
    let url = "https://github.com/fixture-dev/app.git";
    assert_eq!(
        world.get(MapEnv::new(), DEV_REQUEST),
        credential(&world.token("reader"))
    );
    let writer = world.grant(&TestGrant::live("fixture-dev/app", "dev"));
    assert_eq!(
        world.get(granted(&writer), DEV_REQUEST),
        credential(&world.token("dev"))
    );
    let who = world.whoami(granted(&writer), url);
    assert_eq!(who["grant"], Value::from("live"), "{who}");
    assert_eq!(who["profile"], Value::from("dev"), "{who}");
    let reader = world.grant(&TestGrant::live("fixture-dev/app", "reader"));
    assert_eq!(
        world.get(granted(&reader), DEV_REQUEST),
        credential(&world.token("reader"))
    );
    let who = world.whoami(granted(&reader), url);
    assert_eq!(who["grant"], Value::Null, "{who}");
    assert_eq!(who["profile"], Value::from("reader"), "{who}");
}

#[test]
fn a_writer_grant_that_cannot_answer_quits() {
    let world = World::with_reader("helper-rule5");
    std::fs::remove_file(world.root.path().join("tokens").join("dev.token"))
        .expect("the dev token file");
    let writer = world.grant(&TestGrant::live("fixture-dev/app", "dev"));
    let out = world.get(granted(&writer), DEV_REQUEST);
    assert_eq!(out, QUIT);
    assert!(!out.contains(&world.token("reader")));
    let who = world.whoami(granted(&writer), "https://github.com/fixture-dev/app.git");
    assert_eq!(who["answer"], Value::from("quit"), "{who}");
    assert!(
        who["why"].as_str().is_some_and(|why| why.contains("dev")),
        "{who}"
    );
    assert_eq!(
        world.get(MapEnv::new(), DEV_REQUEST),
        credential(&world.token("reader"))
    );
}

#[test]
fn a_release_request_needs_a_live_grant() {
    let world = World::canonical("helper-release");
    let release = credential(&world.token("release"));
    assert_eq!(world.get(MapEnv::new(), RELEASE_REQUEST), QUIT);
    let live = TestGrant::live("alabsystems/aterm", "release");
    let nonce = world.grant(&live);
    assert_eq!(world.get(granted(&nonce), RELEASE_REQUEST), release);
    let later = world.grant(&TestGrant {
        start: live.start + 1,
        ..TestGrant::live("alabsystems/aterm", "release")
    });
    assert_eq!(world.get(granted(&later), RELEASE_REQUEST), QUIT);
    let expired = world.grant(&TestGrant {
        expires_at: common::SIM_START - 1,
        ..TestGrant::live("alabsystems/aterm", "release")
    });
    assert_eq!(world.get(granted(&expired), RELEASE_REQUEST), QUIT);
    let other = "protocol=https\nhost=github.com\npath=alabsystems/other.git\n\n";
    assert_eq!(world.get(granted(&nonce), other), QUIT);
    assert_eq!(world.get(granted("not-a-nonce"), RELEASE_REQUEST), QUIT);
    assert_eq!(
        world.get(granted(&nonce.to_uppercase()), RELEASE_REQUEST),
        QUIT
    );
}

/// A live, unexpired grant for the right repository whose spawner is a live process outside
/// the helper's ancestry (here a child of the helper's own process) is quit: the helper
/// climbs from itself, and a matching pid and start alone never answer. NEGATIVE CONTROL: the
/// same grant written for this process answers the release writer's token.
#[test]
fn a_grant_from_outside_the_helper_s_ancestry_is_quit() {
    use aterm_uds::spawnfd::Stdio;
    let world = World::canonical("helper-outside");
    let mine = world.grant(&TestGrant::live("alabsystems/aterm", "release"));
    assert_eq!(
        world.get(granted(&mine), RELEASE_REQUEST),
        credential(&world.token("release"))
    );
    let cat = aterm_uds::deadline::stand_in(Path::new("/bin/cat"));
    let mut child = aterm_uds::spawnfd::command(&cat)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("the child");
    let stdin = aterm_uds::deadline::cat_echoes(&mut child);
    let pid = child.id();
    let start = aterm_uds::process::read_process_start(i32::try_from(pid).expect("an i32 pid"))
        .map(|start| start.boot.0);
    let answered = start.map(|start| {
        let outside = world.grant(&TestGrant {
            pid,
            start,
            ..TestGrant::live("alabsystems/aterm", "release")
        });
        world.get(granted(&outside), RELEASE_REQUEST)
    });
    drop(stdin);
    let _ = child.wait();
    assert_eq!(answered.as_deref(), Some(QUIT), "the live child's start");
}

/// A dev token file at 0644 under `legacy_loose = true`: `config::load` (no `AGH_GIT_LOOSE`)
/// leaves the profile strict, so the token does not load and the helper quits;
/// `AGH_GIT_LOOSE=1` reads the file through `load_for_host`, which honours the flag.
#[test]
fn agh_git_loose_reads_a_loose_token_file() {
    let world = World::with_config("helper-loose", |root| {
        let canonical = common::canonical_config(root);
        let loose = canonical.replacen(
            "login = \"fixture-dev-bot\"\n",
            "login = \"fixture-dev-bot\"\nlegacy_loose = true\n",
            1,
        );
        assert_ne!(loose, canonical, "the dev profile was marked legacy_loose");
        loose
    });
    common::chmod(&world.root.path().join("tokens").join("dev.token"), 0o644);
    assert_eq!(world.get(MapEnv::new(), DEV_REQUEST), QUIT);
    assert_eq!(
        world.get(MapEnv::new().with("AGH_GIT_LOOSE", "1"), DEV_REQUEST),
        credential(&world.token("dev"))
    );
}

/// The loose world beside `vault = true` (no vault file): a `ci` profile (L10 keeps `dev` out
/// of an agh.toml that joins the vault) marked `legacy_loose = true`, its token file 0644.
/// Under `AGH_GIT_LOOSE=1` the helper routes by `load_for_host` alone, the loader of the
/// router that set the variable, so the loose file answers; without it, adoption's strict
/// judgement leaves `ci` unusable and the helper quits.
#[test]
fn agh_git_loose_reads_a_loose_token_file_beside_vault_true() {
    let world = World::with_config("helper-loose-vault", |root| {
        format!(
            "vault = true\n\n[profiles.ci]\ntoken_file = \"{}/tokens/ci.token\"\ntier = \"dev\"\n\
             login = \"fixture-dev-bot\"\nlegacy_loose = true\n\n[[owners]]\nname = \"fixture-dev\"\n\
             tier = \"dev\"\nread = [\"ci\"]\nwrite = \"ci\"\n",
            root.display()
        )
    });
    let ci = world.token("ci");
    let file = common::write_token(&world.root.path().join("tokens"), "ci.token", &ci);
    common::chmod(&file, 0o644);
    let loose = || MapEnv::new().with("AGH_GIT_LOOSE", "1");
    assert_eq!(world.get(loose(), DEV_REQUEST), credential(&ci));
    assert_eq!(world.get(MapEnv::new(), DEV_REQUEST), QUIT);
    let who = world.whoami(loose(), "https://github.com/fixture-dev/app.git");
    assert_eq!(who["answer"], Value::from("credential"), "{who}");
    assert_eq!(who["profile"], Value::from("ci"), "{who}");
}

/// `whoami --git` explains the helper, which sees none of `--for`, `--op`, `--verify`, `--as`
/// or `--anonymous`: each is Usage, never weighed silently.
#[test]
fn whoami_git_refuses_flags_the_helper_never_sees() {
    let world = World::canonical("helper-whoami-flags");
    let url = "https://github.com/fixture-dev/app.git";
    for argv in [
        vec!["--as", "release", "whoami", "--git", url],
        vec!["--anonymous", "whoami", "--git", url],
        vec!["whoami", "--git", url, "--verify"],
        vec!["whoami", "--git", url, "--for", "fixture-dev/app"],
    ] {
        let (mut ctx, captured) = world.ctx(MapEnv::new());
        assert_eq!(
            common::run_cli(&mut ctx, &argv),
            2,
            "{argv:?}: {}",
            captured.stdout_text()
        );
        assert_eq!(captured.stdout_text(), "", "{argv:?}");
    }
}

#[test]
fn a_malformed_request_quits() {
    let world = World::canonical("helper-malformed");
    let nul = b"protocol=https\nhost=github.com\0\npath=fixture-dev/app.git\n\n";
    let (exit, out) = world.helper(MapEnv::new(), "get", nul);
    assert_eq!((exit, out.as_str()), (0, QUIT));
    let long = format!(
        "protocol=https\nhost=github.com\npath=fixture-dev/app.git\nextra={}\n\n",
        "a".repeat(70 * 1024)
    );
    let (exit, out) = world.helper(MapEnv::new(), "get", long.as_bytes());
    assert_eq!((exit, out.as_str()), (0, QUIT));
}

#[test]
fn whoami_git_explains_without_the_token() {
    let world = World::canonical("helper-whoami");
    let (mut ctx, captured) = world.ctx(MapEnv::new());
    let exit = common::run_cli(
        &mut ctx,
        &["whoami", "--git", "https://github.com/fixture-dev/app.git"],
    );
    assert_eq!(exit, 0, "{}", captured.stderr_text());
    assert_eq!(
        captured.stdout_text().trim_end(),
        r#"{"answer":"credential","grant":null,"profile":"dev","repo":"fixture-dev/app","tier":"dev","why":null}"#
    );
    let (mut ctx, captured) = world.ctx(MapEnv::new());
    let exit = common::run_cli(
        &mut ctx,
        &["whoami", "--git", "https://github.com/alabsystems/aterm"],
    );
    assert_eq!(exit, 0, "{}", captured.stderr_text());
    let who = common::stdout_json(&captured);
    assert_eq!(who["answer"], Value::from("quit"), "{who}");
    assert_eq!(who["tier"], Value::from("release"), "{who}");
    assert_eq!(who["profile"], Value::Null, "{who}");
    assert!(
        who["why"]
            .as_str()
            .is_some_and(|why| why.contains("alabsystems/aterm")),
        "{who}"
    );
    let stdout = captured.stdout_text();
    for name in ["dev", "release"] {
        assert!(!stdout.contains(&world.token(name)), "{stdout}");
    }
    let (mut ctx, captured) = world.ctx(MapEnv::new());
    assert_eq!(
        common::run_cli(&mut ctx, &["whoami", "--git", "ftp://github.com/a/b"]),
        2,
        "{}",
        captured.stderr_text()
    );
}

/// A credential-bearing Git URL is exactly `OWNER/REPO[.git]`: query, fragment and backslash
/// syntax cannot be folded into a repository name. The helper must quit without a credential,
/// and the explanatory verb must reject the same spellings.
#[test]
fn git_urls_reject_query_fragment_and_backslash_suffixes() {
    let world = World::canonical("helper-url-suffixes");
    for suffix in ["?download=1", "#fragment", "\\other"] {
        let request =
            format!("protocol=https\nhost=github.com\npath=fixture-dev/app.git{suffix}\n\n");
        assert_eq!(world.get(MapEnv::new(), &request), QUIT, "{suffix:?}");

        let url = format!("https://github.com/fixture-dev/app.git{suffix}");
        let (mut ctx, captured) = world.ctx(MapEnv::new());
        assert_eq!(
            common::run_cli(&mut ctx, &["whoami", "--git", &url]),
            2,
            "{suffix:?}: {}",
            captured.stderr_text()
        );
    }
}

#[test]
fn repo_clone_clones_through_the_runner() {
    let world = World::canonical("helper-clone");
    let bare = crate::git_runner::bare_repo(world.root.path(), "app");
    // A second commit, so a clone of depth 1 cuts history and writes `.git/shallow`.
    let tree = crate::git_runner::host_git(&bare, &["rev-parse", "main^{tree}"]);
    let first = crate::git_runner::host_git(&bare, &["rev-parse", "main"]);
    let head =
        crate::git_runner::host_git(&bare, &["commit-tree", &tree, "-p", &first, "-m", "two"]);
    crate::git_runner::host_git(&bare, &["update-ref", "refs/heads/main", &head]);
    let attest = format!("refs/attest/soundness/{head}/fixture-host");
    let record = crate::git_runner::host_git(
        &bare,
        &["commit-tree", &tree, "-p", &head, "-m", "evidence"],
    );
    crate::git_runner::host_git(&bare, &["update-ref", &attest, &record]);
    let clone = |dir: &Path, extra: &[&str]| {
        let (ctx, captured) = world.ctx(crate::git_runner::git_env());
        let mut ctx = ctx
            .with_git_helper(common::helper())
            .with_git_remote("fixture-dev/app", &bare);
        let dir_text = dir.display().to_string();
        let mut args = vec!["repo", "clone", "fixture-dev/app", dir_text.as_str()];
        args.extend_from_slice(extra);
        let exit = common::run_cli(&mut ctx, &args);
        (exit, captured)
    };
    let full = world.root.path().join("full");
    let (exit, captured) = clone(&full, &["--meta"]);
    assert_eq!(exit, 0, "{}", captured.stderr_text());
    assert_eq!(captured.stdout_text(), "");
    assert_eq!(
        crate::git_runner::host_git(&full, &["rev-parse", "HEAD"]),
        head
    );
    let stderr = captured.stderr_text();
    let receipt = stderr
        .lines()
        .find(|line| line.starts_with("agh: ok"))
        .unwrap_or_else(|| panic!("a receipt: {stderr}"));
    assert!(receipt.split(' ').any(|pair| pair == "as=dev"), "{receipt}");
    assert_eq!(
        receipt
            .split(' ')
            .filter(|pair| pair.starts_with("as="))
            .count(),
        1,
        "{receipt}"
    );
    // Under -R, DIR is the one positional left.
    let typed = world.root.path().join("typed");
    let (ctx, captured) = world.ctx(crate::git_runner::git_env());
    let mut ctx = ctx
        .with_git_helper(common::helper())
        .with_git_remote("fixture-dev/app", &bare);
    let typed_text = typed.display().to_string();
    let exit = common::run_cli(
        &mut ctx,
        &[
            "repo",
            "clone",
            "-R",
            "fixture-dev/app",
            typed_text.as_str(),
        ],
    );
    assert_eq!(exit, 0, "{}", captured.stderr_text());
    assert_eq!(
        crate::git_runner::host_git(&typed, &["rev-parse", "HEAD"]),
        head
    );
    // The run speaks as agh.toml routes it, so --anonymous and --as are Usage, and nothing
    // is cloned.
    for flags in [&["--anonymous"][..], &["--as", "dev"][..]] {
        let refused = world.root.path().join("refused");
        let (exit, captured) = clone(&refused, flags);
        assert_eq!(exit, 2, "{flags:?}: {}", captured.stderr_text());
        assert!(!refused.exists(), "{flags:?}");
    }
    let shallow = world.root.path().join("shallow");
    let (exit, captured) = clone(&shallow, &["--depth", "1"]);
    assert_eq!(exit, 0, "{}", captured.stderr_text());
    assert!(shallow.join(".git").join("shallow").is_file());
    assert_eq!(
        crate::git_runner::host_git(&full, &["for-each-ref", "--format=%(refname)", &attest]),
        "",
        "an ordinary clone does not carry custom evidence refs"
    );
    let mirror = world.root.path().join("mirror.git");
    let (exit, captured) = clone(&mirror, &["--mirror"]);
    assert_eq!(exit, 0, "{}", captured.stderr_text());
    assert_eq!(
        crate::git_runner::host_git(&mirror, &["rev-parse", "--is-bare-repository"]),
        "true"
    );
    assert_eq!(
        crate::git_runner::host_git(&mirror, &["rev-parse", &attest]),
        record
    );
    assert_eq!(
        crate::git_runner::host_git(&mirror, &["rev-parse", &format!("{attest}^")]),
        head
    );
    assert_eq!(
        crate::git_runner::host_git(&mirror, &["rev-parse", "main^"]),
        first
    );
    assert!(!mirror.join("shallow").exists());
    let (exit, captured) = clone(&mirror, &["--mirror"]);
    assert_eq!(exit, 2, "{}", captured.stderr_text());
    let refused = world.root.path().join("shallow-mirror.git");
    let before = world.fake.seen().len();
    let (exit, captured) = clone(&refused, &["--mirror", "--depth", "1"]);
    assert_eq!(exit, 2, "{}", captured.stderr_text());
    assert!(!refused.exists());
    assert_eq!(
        world.fake.seen().len(),
        before,
        "invalid modes are refused before routing"
    );
    let busy = world.root.path().join("busy");
    std::fs::create_dir_all(&busy).expect("the busy directory");
    std::fs::write(busy.join("kept"), b"kept").expect("a file in it");
    let (exit, captured) = clone(&busy, &[]);
    assert_eq!(exit, 2, "{}", captured.stderr_text());
    assert_eq!(std::fs::read(busy.join("kept")).expect("kept"), b"kept");
}

#[test]
fn local_read_admission_needs_no_identity_or_request_and_preserves_source() {
    let world = World::canonical("helper-local-admission");
    let source = crate::git_runner::bare_repo(world.root.path(), "source with spaces ");
    crate::git_runner::host_git(
        &source,
        &[
            "config",
            "remote.upstream.pushurl",
            "NEVER-PUSH-TO-UPSTREAM",
        ],
    );
    // A local check must not load routing or credential values, even when the
    // CLI would refuse this routing document for an actual network operation.
    common::write_config(&world.dirs, "[not valid TOML");
    let before = std::fs::read(source.join("config")).unwrap();
    let head = std::fs::read(source.join("HEAD")).unwrap();
    let refs = crate::git_runner::host_git(&source, &["for-each-ref"]);
    let (mut ctx, captured) = world.ctx(
        crate::git_runner::git_env()
            .with("GIT_DIR", "/unrelated")
            .with("GIT_CONFIG_COUNT", "1"),
    );
    let path = source.display().to_string();
    let exit = common::run_cli(
        &mut ctx,
        &["repo", "check-local", "alabsystems/aterm", &path],
    );
    assert_eq!(exit, 0, "{}", captured.stderr_text());
    let report = common::stdout_json(&captured);
    let directory = std::fs::canonicalize(&source)
        .unwrap()
        .display()
        .to_string();
    assert_eq!(
        report,
        aterm_json::json!({
            "schema": 1, "mode": "local-read", "repository": "alabsystems/aterm",
            "directory": directory, "common_dir": directory,
            "admitted": true, "github_requests": 0, "source_mutated": false,
        })
    );
    assert_eq!(std::fs::read(source.join("config")).unwrap(), before);
    assert_eq!(std::fs::read(source.join("HEAD")).unwrap(), head);
    assert_eq!(
        crate::git_runner::host_git(&source, &["for-each-ref"]),
        refs
    );
    assert!(world.fake.seen().is_empty());
    assert!(!world.dirs.grants().exists());
}

#[test]
fn local_read_admission_refuses_hooks_and_enclosing_repositories() {
    let world = World::canonical("helper-local-admission-refusals");
    let source = crate::git_runner::bare_repo(world.root.path(), "source");
    let invoke = |directory: &Path| {
        let (mut ctx, captured) = world.ctx(crate::git_runner::git_env());
        let path = directory.display().to_string();
        let exit = common::run_cli(&mut ctx, &["repo", "check-local", "fixture-dev/app", &path]);
        (exit, captured)
    };
    let empty = source.join("not-a-repository");
    std::fs::create_dir(&empty).unwrap();
    assert_ne!(
        invoke(&empty).0,
        0,
        "the parent's repository is never admitted"
    );
    crate::git_runner::host_git(&source, &["config", "hook.hostile.command", "false"]);
    let config = std::fs::read(source.join("config")).unwrap();
    let (exit, captured) = invoke(&source);
    assert_ne!(exit, 0);
    assert!(captured.stderr_text().contains("git_config"));
    assert_eq!(std::fs::read(source.join("config")).unwrap(), config);
    assert!(world.fake.seen().is_empty());
    assert!(!world.dirs.grants().exists());
}

#[test]
fn repo_fetch_accepts_exact_anonymous_reads_and_refuses_unsafe_names() {
    let world = World::canonical("helper-exact-fetch");
    let bare = crate::git_runner::bare_repo(world.root.path(), "app");
    let sha = crate::git_runner::host_git(&bare, &["rev-parse", "main"]);
    let checkout = world.root.path().join("receiver.git");
    std::fs::create_dir(&checkout).unwrap();
    crate::git_runner::host_git(&checkout, &["init", "--quiet", "--bare"]);
    let head = std::fs::read(checkout.join("HEAD")).unwrap();
    let config = std::fs::read(checkout.join("config")).unwrap();
    std::fs::write(checkout.join("FETCH_HEAD"), b"retained\n").unwrap();
    let invoke = |source: &str, into: &str, extra: &[&str]| {
        let (ctx, captured) = world.ctx(crate::git_runner::git_env());
        let mut ctx = ctx
            .with_git_helper(common::helper())
            .with_git_remote("alabsystems/aterm", &bare);
        let path = checkout.display().to_string();
        let mut args = vec![
            "repo",
            "fetch",
            "alabsystems/aterm",
            path.as_str(),
            "--ref",
            source,
            "--into",
            into,
        ];
        args.extend_from_slice(extra);
        let exit = common::run_cli(&mut ctx, &args);
        (exit, captured)
    };
    let (exit, captured) = invoke(&sha, "refs/agh/release-tree", &["--anonymous", "--meta"]);
    assert_eq!(exit, 0, "{}", captured.stderr_text());
    assert!(captured.stderr_text().contains("as=anonymous"));
    assert_eq!(
        crate::git_runner::host_git(&checkout, &["rev-parse", "refs/agh/release-tree"]),
        sha
    );
    assert_eq!(std::fs::read(checkout.join("HEAD")).unwrap(), head);
    assert_eq!(std::fs::read(checkout.join("config")).unwrap(), config);
    assert_eq!(
        std::fs::read(checkout.join("FETCH_HEAD")).unwrap(),
        b"retained\n"
    );
    for (source, into, extra) in [
        ("main", "refs/agh/read", &[][..]),
        ("refs/heads/*", "refs/agh/read", &[][..]),
        (sha.as_str(), "refs/heads/main", &[][..]),
        (sha.as_str(), "refs/agh/read", &["--as", "release"][..]),
    ] {
        let (exit, captured) = invoke(source, into, extra);
        assert_eq!(exit, 2, "{}", captured.stderr_text());
    }
    assert!(
        world.fake.seen().is_empty(),
        "the local fixture needs no HTTP or credential probe"
    );
}

#[test]
fn repo_batch_fetch_requires_expected_ids_and_keeps_anonymous_authority_explicit() {
    let world = World::canonical("helper-batch-fetch");
    let bare = crate::git_runner::bare_repo(world.root.path(), "app");
    let sha = crate::git_runner::host_git(&bare, &["rev-parse", "main"]);
    let receiver = world.root.path().join("receiver with spaces.git");
    std::fs::create_dir(&receiver).unwrap();
    crate::git_runner::host_git(&receiver, &["init", "--quiet", "--bare"]);
    let file = world.root.path().join("refs with spaces.json");
    let invoke = |extra: &[&str]| {
        let (ctx, captured) = world.ctx(crate::git_runner::git_env());
        let mut ctx = ctx
            .with_git_helper(common::helper())
            .with_git_remote("alabsystems/aterm", &bare);
        let receiver = receiver.to_string_lossy();
        let file = file.to_string_lossy();
        let mut args = vec![
            "repo",
            "fetch",
            "alabsystems/aterm",
            receiver.as_ref(),
            "--input",
            file.as_ref(),
            "--anonymous",
            "--meta",
        ];
        args.extend_from_slice(extra);
        let exit = common::run_cli(&mut ctx, &args);
        (exit, captured)
    };
    std::fs::write(
        &file,
        format!(
            r#"[{{"source":"refs/heads/main","into":"refs/agh/main","expected_oid":"{sha}"}}]"#
        ),
    )
    .unwrap();
    let (exit, captured) = invoke(&[]);
    assert_eq!(exit, 0, "{}", captured.stderr_text());
    assert!(captured.stderr_text().contains("as=anonymous"));
    assert_eq!(
        crate::git_runner::host_git(&receiver, &["rev-parse", "refs/agh/main"]),
        sha
    );
    let before = crate::git_runner::host_git(&receiver, &["for-each-ref"]);
    for extra in [
        &["--ref", "refs/heads/main"][..],
        &["--into", "refs/agh/other"][..],
        &["--as", "release"][..],
    ] {
        let (exit, _) = invoke(extra);
        assert_eq!(exit, 2);
    }
    for body in [
        "[]".to_string(),
        format!(
            r#"[{{"source":"refs/heads/main","into":"refs/heads/main","expected_oid":"{sha}"}}]"#
        ),
        format!(
            r#"[{{"source":"refs/heads/main","into":"refs/agh/other","expected_oid":"{sha}","extra":true}}]"#
        ),
    ] {
        std::fs::write(&file, body).unwrap();
        assert_ne!(invoke(&[]).0, 0);
        assert_eq!(
            crate::git_runner::host_git(&receiver, &["for-each-ref"]),
            before
        );
    }
    assert!(
        world.fake.seen().is_empty(),
        "anonymous local fixture reads do not judge or use credentials"
    );
}

#[test]
fn repo_fetch_main_keeps_work_and_refs_and_records_rewinds() {
    let world = World::with_reader("helper-fetch-main");
    let bare = crate::git_runner::bare_repo(world.root.path(), "app");
    let checkout = world.root.path().join("checkout");
    let invoke = |verb: &str, extra: &[&str]| {
        let (ctx, captured) = world.ctx(crate::git_runner::git_env());
        let mut ctx = ctx
            .with_git_helper(common::helper())
            .with_git_remote("fixture-dev/app", &bare);
        let path = checkout.display().to_string();
        let mut args = vec!["repo", verb, "fixture-dev/app", path.as_str()];
        args.extend_from_slice(extra);
        let exit = common::run_cli(&mut ctx, &args);
        (exit, captured)
    };
    let (exit, captured) = invoke("clone", &[]);
    assert_eq!(exit, 0, "{}", captured.stderr_text());
    let first = crate::git_runner::host_git(&bare, &["rev-parse", "main"]);
    let tree = crate::git_runner::host_git(&bare, &["rev-parse", "main^{tree}"]);
    crate::git_runner::host_git(&checkout, &["update-ref", "refs/stash", &first]);
    crate::git_runner::host_git(&checkout, &["tag", "local-tag", &first]);
    let refs = || {
        crate::git_runner::host_git(
            &checkout,
            &["for-each-ref", "--format=%(refname) %(objectname)"],
        )
    };
    let before = refs();
    let config_before = std::fs::read(checkout.join(".git/config")).expect("local config");
    let fetch_head = checkout.join(".git/FETCH_HEAD");
    let marker = format!("{first}\t\tbranch 'main' of fixture\n");
    std::fs::write(&fetch_head, &marker).expect("old FETCH_HEAD");
    std::fs::write(checkout.join("local-work"), b"keep this").expect("uncommitted work");
    let second =
        crate::git_runner::host_git(&bare, &["commit-tree", &tree, "-p", &first, "-m", "two"]);
    crate::git_runner::host_git(&bare, &["update-ref", "refs/heads/main", &second]);
    crate::git_runner::host_git(&bare, &["tag", "remote-tag", &second]);
    let (exit, captured) = invoke("fetch-main", &["--meta"]);
    assert_eq!(exit, 0, "{}", captured.stderr_text());
    assert_eq!(captured.stdout_text(), "");
    assert!(captured.stderr_text().contains("as=reader"));
    assert!(captured.stderr_text().contains("ref=refs/agh/fetched-main"));
    assert_eq!(
        crate::git_runner::host_git(&checkout, &["rev-parse", "refs/agh/fetched-main"]),
        second
    );
    // A remote rewind refreshes only the cache; its previous tip stays recoverable.
    crate::git_runner::host_git(&bare, &["update-ref", "refs/heads/main", &first]);
    let (exit, captured) = invoke("fetch-main", &[]);
    assert_eq!(exit, 0, "{}", captured.stderr_text());
    assert_eq!(
        crate::git_runner::host_git(&checkout, &["rev-parse", "refs/agh/fetched-main"]),
        first
    );
    let log = crate::git_runner::host_git(
        &checkout,
        &["reflog", "show", "--format=%H", "refs/agh/fetched-main"],
    );
    assert_eq!(
        log.lines().collect::<Vec<_>>(),
        vec![first.as_str(), second.as_str()]
    );
    let kept = refs()
        .lines()
        .filter(|line| !line.starts_with("refs/agh/"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(kept, before, "branches, tags, remotes and stash stay fixed");
    assert_eq!(
        std::fs::read_to_string(fetch_head).expect("FETCH_HEAD"),
        marker
    );
    assert_eq!(
        std::fs::read(checkout.join("local-work")).expect("local work"),
        b"keep this"
    );
    assert_eq!(
        crate::git_runner::host_git(&checkout, &["rev-parse", "HEAD"]),
        first
    );
    assert_eq!(
        std::fs::read(checkout.join(".git/config")).expect("local config"),
        config_before
    );
    // -R leaves DIR as the one positional, just as for cloning.
    let (ctx, captured) = world.ctx(crate::git_runner::git_env());
    let mut ctx = ctx
        .with_git_helper(common::helper())
        .with_git_remote("fixture-dev/app", &bare);
    let path = checkout.display().to_string();
    assert_eq!(
        common::run_cli(
            &mut ctx,
            &["repo", "fetch-main", "-R", "fixture-dev/app", &path]
        ),
        0,
        "{}",
        captured.stderr_text()
    );
    let seen = world.fake.seen().len();
    for flags in [
        &["--anonymous"][..],
        &["--as", "dev"][..],
        &["--into", "refs/stash"][..],
    ] {
        let (exit, captured) = invoke("fetch-main", flags);
        assert_eq!(exit, 2, "{}", captured.stderr_text());
    }
    let absent = world.root.path().join("absent");
    let absent = absent.display().to_string();
    for args in [
        vec!["repo", "fetch-main", "fixture-dev/app"],
        vec!["repo", "fetch-main", "fixture-dev/app", absent.as_str()],
    ] {
        let (mut ctx, captured) = world.ctx(crate::git_runner::git_env());
        assert_eq!(
            common::run_cli(&mut ctx, &args),
            2,
            "{}",
            captured.stderr_text()
        );
    }
    assert_eq!(
        world.fake.seen().len(),
        seen,
        "invalid modes and destinations never route"
    );
}

/// A scratch root with no agh.toml (or `config`, when given) and the vault's dev token, whose
/// login `fixture-dev` the vault's record already binds, as the runner's own call records it
/// (design §6.10): the root, its `Dirs`, agh-fake and the token.
fn vault_world(label: &str, config: Option<&str>) -> (World, String) {
    let root = common::scratch(label);
    let fake = FakeGithub::start();
    let dirs = Dirs::under(root.path());
    let token = agh_fake::token("vault-dev");
    fake.add_token(&token, "fixture-dev");
    let path = agh::identity::vault::Slot::Dev.path(&dirs.home);
    std::fs::create_dir_all(&dirs.home).expect("the home");
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .expect("the vault file's name");
    common::write_token(path.parent().expect("the vault directory"), name, &token);
    let fingerprint = agh::identity::fingerprint(token.as_bytes());
    agh::identity::vault::record_for_tests(
        &dirs,
        &[(
            agh::identity::vault::Slot::Dev,
            fingerprint.as_str(),
            "fixture-dev",
        )],
    )
    .expect("the vault's record");
    if let Some(text) = config {
        std::fs::write(dirs.config.join("agh.toml"), text).expect("agh.toml");
        common::chmod(&dirs.config.join("agh.toml"), 0o600);
    }
    (World { root, dirs, fake }, token)
}

/// The helper routes as the runner's client does: with no agh.toml, or one that says
/// `vault = true`, the vault's adopted rows route, so a request for the dev token's owner
/// answers that token, and `whoami --git` names `dev`, as `whoami --for` does.
#[test]
fn a_vault_routed_request_answers_the_vault_token() {
    for (label, config) in [
        ("helper-vault-bare", None),
        ("helper-vault-joined", Some("vault = true\n")),
    ] {
        let (world, token) = vault_world(label, config);
        assert_eq!(
            world.get(MapEnv::new(), DEV_REQUEST),
            credential(&token),
            "{label}"
        );
        let who = world.whoami(MapEnv::new(), "https://github.com/fixture-dev/app.git");
        assert_eq!(who["answer"], Value::from("credential"), "{label}: {who}");
        assert_eq!(who["profile"], Value::from("dev"), "{label}: {who}");
        assert_eq!(who["tier"], Value::from("dev"), "{label}: {who}");
        assert!(!who.to_string().contains(&token), "{label}");
    }
}

/// A vault token the record binds to no login adopts nothing for the helper, which asks
/// GitHub nothing: it quits.
#[test]
fn an_unrecorded_vault_token_answers_nothing() {
    let (world, _token) = vault_world("helper-vault-unrecorded", None);
    std::fs::remove_file(world.dirs.config.join(agh::identity::vault::RECORD_FILE))
        .expect("the record");
    assert_eq!(world.get(MapEnv::new(), DEV_REQUEST), QUIT);
    assert!(world.fake.seen().is_empty(), "the helper asked GitHub");
}

/// An invalid agh.toml: the helper quits silently, and `whoami --git`, the verb that
/// explains, fails with the config's own refusal as every routed verb does.
#[test]
fn an_invalid_agh_toml_quits_the_helper_and_fails_whoami_git() {
    let world = World::with_config("helper-invalid", |_| "[profiles.dev\n".to_string());
    assert_eq!(world.get(MapEnv::new(), DEV_REQUEST), QUIT);
    let (mut ctx, captured) = world.ctx(MapEnv::new());
    let exit = common::run_cli(
        &mut ctx,
        &["whoami", "--git", "https://github.com/fixture-dev/app.git"],
    );
    assert_ne!(exit, 0, "{}", captured.stdout_text());
    assert_eq!(captured.stdout_text(), "");
}

/// A stdin that records whether anything read it.
struct Watched(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl std::io::Read for Watched {
    fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(0)
    }
}

/// Rule 1 holds before the read: a helper typed at a terminal answers `quit=1` at once, and
/// never waits for the person to end the input.
#[test]
fn a_terminal_stdin_is_never_read() {
    let world = World::canonical("helper-terminal-unread");
    let (ctx, captured) = world.ctx(MapEnv::new());
    let mut ctx = ctx.with_stdin_terminal(true);
    let read = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    ctx.stdin = Box::new(Watched(std::sync::Arc::clone(&read)));
    let exit = common::run_cli(&mut ctx, &["auth", "git-credential", "get"]);
    assert_eq!((exit, captured.stdout_text().as_str()), (0, QUIT));
    assert_eq!(captured.stderr_text(), "");
    assert!(
        !read.load(std::sync::atomic::Ordering::SeqCst),
        "the terminal was read"
    );
}

/// Rule 2 before the read: `store` and `erase`, which git sends with the credential it used,
/// answer `quit=1` without reading stdin, so the token they carry is never held in a buffer.
/// An op agh does not know (`approve`) also answers `quit=1` unread.
#[test]
fn store_and_erase_are_never_read() {
    let world = World::canonical("helper-store-unread");
    for op in ["store", "erase", "approve"] {
        let (mut ctx, captured) = world.ctx(MapEnv::new());
        let read = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        ctx.stdin = Box::new(Watched(std::sync::Arc::clone(&read)));
        let exit = common::run_cli(&mut ctx, &["auth", "git-credential", op]);
        assert_eq!((exit, captured.stdout_text().as_str()), (0, QUIT), "{op}");
        assert_eq!(captured.stderr_text(), "", "{op}");
        assert!(
            !read.load(std::sync::atomic::Ordering::SeqCst),
            "{op}'s stdin was read"
        );
    }
}

#[test]
fn authenticated_exact_ref_reads_use_no_rest_and_do_not_return_prefix_matches() {
    let world = World::canonical("helper-private-refs");
    let bare = crate::git_runner::bare_repo(world.root.path(), "app");
    let oid = crate::git_runner::host_git(&bare, &["rev-parse", "HEAD"]);
    crate::git_runner::host_git(&bare, &["update-ref", "refs/aterm/judged", &oid]);
    crate::git_runner::host_git(&bare, &["update-ref", "refs/aterm/judged-other", &oid]);
    let (ctx, captured) = world.ctx(crate::git_runner::git_env());
    let mut ctx = ctx
        .with_git_helper(common::helper())
        .with_git_remote("fixture-dev/app", &bare);
    world.fake.clear_seen();
    assert_eq!(
        common::run_cli(
            &mut ctx,
            &[
                "repo",
                "refs",
                "fixture-dev/app",
                "--ref",
                "refs/aterm/judged"
            ]
        ),
        0,
        "{}",
        captured.stderr_text()
    );
    assert_eq!(
        common::stdout_json(&captured),
        aterm_json::json!([{"ref":"refs/aterm/judged", "oid":oid.trim()}])
    );
    assert!(
        world.fake.seen().is_empty(),
        "native authenticated ref reads spend no REST quota"
    );
    for flag in [&["--anonymous"][..], &["--as", "dev"][..]] {
        let mut args = flag.to_vec();
        args.extend(["repo", "refs", "fixture-dev/app"]);
        assert_eq!(
            common::run_cli(&mut ctx, &args),
            2,
            "a native read must not ignore identity flags"
        );
    }
    assert!(world.fake.seen().is_empty());
}

#[test]
fn public_refs_include_peeled_tags_without_rest_or_writer_identity() {
    let world = World::canonical("helper-public-refs");
    let bare = crate::git_runner::bare_repo(world.root.path(), "app");
    crate::git_runner::host_git(
        &bare,
        &[
            "-c",
            "user.name=Andrew Yates",
            "-c",
            "user.email=test@example.com",
            "tag",
            "-a",
            "v1",
            "-m",
            "tag",
        ],
    );
    let tag_oid = crate::git_runner::host_git(&bare, &["rev-parse", "refs/tags/v1"]);
    let packed = (0..2000)
        .map(|i| format!("{tag_oid} refs/tags/history-{i}\n"))
        .collect::<String>();
    std::fs::write(bare.join("packed-refs"), packed).unwrap();
    let (ctx, captured) = world.ctx(crate::git_runner::git_env());
    let mut ctx = ctx
        .with_git_helper(common::helper())
        .with_git_remote("alabsystems/aterm", &bare);
    assert_eq!(
        common::run_cli(
            &mut ctx,
            &[
                "--anonymous",
                "--no-anonymous-api",
                "repo",
                "public-refs",
                "alabsystems/aterm",
                "--meta"
            ]
        ),
        0,
        "{}",
        captured.stderr_text()
    );
    let text = captured.stdout_text();
    assert!(text.len() > 64 * 1024);
    assert!(text.contains("refs/tags/history-0^{}"));
    assert!(text.contains("refs/tags/history-1999^{}"));
    assert!(text.contains("refs/tags/v1^{}"), "{text}");
    assert!(captured.stderr_text().contains("as=anonymous"));
    assert!(world.fake.seen().is_empty());
    // Negative control: this verb never silently adopts a routed writer.
    assert_eq!(
        common::run_cli(&mut ctx, &["repo", "public-refs", "alabsystems/aterm"]),
        2
    );
}
