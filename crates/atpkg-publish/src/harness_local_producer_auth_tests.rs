// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::cell::RefCell;

use super::*;
use crate::artifact::tools::{Ran, RunError};

struct Recording {
    calls: RefCell<Vec<Run>>,
    answer: Ran,
}

impl Recording {
    fn new(code: i32, output: &str) -> Self {
        Self {
            calls: RefCell::new(Vec::new()),
            answer: Ran {
                code: Some(code),
                stdout: Vec::new(),
                stderr: output.as_bytes().to_vec(),
            },
        }
    }
}

impl Tools for Recording {
    fn run(&self, run: &Run) -> std::result::Result<Ran, RunError> {
        self.calls.borrow_mut().push(run.clone());
        Ok(self.answer.clone())
    }
    fn strip(&self, _: &Path, _: bool) -> std::result::Result<(), String> {
        panic!("fetch never strips")
    }
    fn resign(&self, _: &Path) -> std::result::Result<(), String> {
        panic!("fetch never signs")
    }
    fn same_unsigned(&self, _: &Path, _: &Path) -> std::result::Result<bool, String> {
        panic!("fetch never compares images")
    }
    fn thin(&self, _: &Path, _: &str, _: &Path) -> std::result::Result<(), String> {
        panic!("fetch never thins")
    }
    fn relocate(
        &self,
        _: &Path,
        _: &[PathBuf],
    ) -> std::result::Result<crate::artifact::relocate::Report, String> {
        panic!("fetch never relocates")
    }
    fn compressor(&self) -> &dyn crate::artifact::tarwrite::Compressor {
        panic!("fetch never compresses")
    }
}

fn config(home: &Path) -> Config {
    Config::parse(&[
        "claude-code-alab".into(),
        "--checkout".into(),
        home.join("source").display().to_string(),
        "--out".into(),
        home.join("out").display().to_string(),
        "--prefix".into(),
        home.join("pkg").display().to_string(),
        "--native".into(),
        "--unverified".into(),
    ])
    .expect("explicit fixture config")
}

/// A fixture of agh's helper: the `credential.helper` value of a retained `atpkg-publish`.
fn retained_helper() -> std::result::Result<String, String> {
    agh::git::HelperCommand {
        exe: "/retained/atpkg-publish".into(),
        args: vec!["agh".into()],
    }
    .config_value()
    .map_err(|e| e.to_string())
}

/// A vault-adopted machine under a scratch root: no agh.toml, and `~/.secrets/gh_access_token`
/// holding a token agh-fake answers `GET /user` for as `fixture-dev`, whose login no agh call
/// has recorded yet.
struct Vault {
    _root: aterm_tempfile::TempDir,
    dirs: agh::config::Dirs,
    fake: agh_fake::FakeGithub,
    base: agh::Github,
}

impl Vault {
    fn new() -> Self {
        let root = aterm_tempfile::Builder::new()
            .prefix("harness-vault-")
            .suffix(".noindex")
            .tempdir()
            .expect("scratch");
        let dirs = agh::config::Dirs::under(root.path());
        let fake = agh_fake::FakeGithub::start();
        let token = agh_fake::token("harness-dev");
        fake.add_token(&token, "fixture-dev");
        let secrets = dirs.home.join(".secrets");
        aterm_uds::private_file::create_private_dir(&dirs.home).expect("home");
        aterm_uds::private_file::create_private_dir(&secrets).expect("the vault");
        let file = agh::identity::vault::Slot::Dev.path(&dirs.home);
        let name = file.file_name().expect("the vault file").to_string_lossy();
        aterm_uds::private_file::write_private(&secrets, &name, token.as_bytes())
            .expect("the vault token");
        let env = std::sync::Arc::new(agh::identity::MapEnv::new());
        let ports = fake.ports();
        let base = agh::GithubBuilder::new(agh::policy::Router::empty())
            .env(env.clone())
            .transport(std::sync::Arc::new(agh::wire::http::HttpTransport::direct(
                env,
            )))
            .endpoints(agh::wire::Endpoints::loopback(
                ports.api,
                ports.uploads,
                ports.web,
                ports.cdn,
            ))
            .fx(std::sync::Arc::new(agh::Fx::simulated(1_760_000_000)))
            .caller("atpkg-publish")
            .build();
        Self {
            _root: root,
            dirs,
            fake,
            base,
        }
    }

    /// `routed_helper` for `slug` over this machine, learning through agh-fake when `learn`,
    /// else reading the record alone (`agh::routing::view`), and how often it named the helper.
    fn route(&self, slug: &str, learn: bool) -> (std::result::Result<String, String>, usize) {
        self.route_with(slug, &|dirs| {
            if learn {
                agh::routing::adopt(&self.base, dirs)
            } else {
                agh::routing::view(dirs, &agh::identity::MapEnv::new())
            }
        })
    }

    fn route_with(
        &self,
        slug: &str,
        learn: &dyn Fn(&agh::config::Dirs) -> agh::Result<agh::routing::Configured>,
    ) -> (std::result::Result<String, String>, usize) {
        let slug = agh::Slug::parse(slug).expect("the slug");
        let asked = std::cell::Cell::new(0);
        let env = agh::identity::MapEnv::new();
        let routed = routed_helper(&self.dirs, &env, &slug, learn, &|| {
            asked.set(asked.get() + 1);
            retained_helper()
        });
        (routed, asked.get())
    }

    fn user_calls(&self) -> usize {
        self.fake
            .seen()
            .iter()
            .filter(|seen| seen.path_and_query == "/user")
            .count()
    }
}

/// On a vault machine whose token's login agh has not recorded, the fetch learns it first (one
/// `GET /user`, as any routed agh call would) and then names agh's helper, which reads only the
/// record and now answers the staging repository as `dev`; a second fetch learns nothing more.
/// Without the learn step the helper is never named, and the refusal says agh routes the
/// repository with no identity (the unrecorded token routes nothing): that view-only first half
/// is the negative control for the learn step, `view` in place of `adopt`.
#[cfg(unix)]
#[test]
fn a_vault_login_agh_has_not_recorded_is_learned_before_the_fetch_names_the_helper() {
    let vault = Vault::new();
    let (unlearned, asked) = vault.route("fixture-dev/claude-code-alab", false);
    let why = unlearned.expect_err("an unrecorded login refuses");
    assert_eq!(asked, 0, "a refused route names no helper");
    assert!(
        why.contains("agh routes fixture-dev/claude-code-alab with no identity"),
        "{why}"
    );
    assert_eq!(vault.user_calls(), 0, "a view learns nothing");
    let (learned, asked) = vault.route("fixture-dev/claude-code-alab", true);
    assert_eq!(
        learned,
        retained_helper(),
        "the learned route names the helper"
    );
    assert_eq!(asked, 1);
    assert_eq!(vault.user_calls(), 1, "one GET /user learns the login");
    let (recorded, asked) = vault.route("fixture-dev/claude-code-alab", false);
    assert_eq!(recorded, retained_helper(), "the helper reads the record");
    assert_eq!(asked, 1);
    let (again, _) = vault.route("fixture-dev/claude-code-alab", true);
    assert_eq!(again, retained_helper());
    assert_eq!(
        vault.user_calls(),
        1,
        "a recorded login is not learned again"
    );
}

/// A login learned live whose record the helper cannot read (the record could not be written,
/// or was removed since) refuses with agh's own reason and next step, and names no helper: the
/// helper reads only the record, so git would end on its `quit=1`. NEGATIVE CONTROL: without
/// `require_answer` in `routed_helper`, the helper is named for a fetch it will not answer.
#[cfg(unix)]
#[test]
fn a_learned_login_the_helper_cannot_read_refuses_with_agh_s_reason() {
    let vault = Vault::new();
    let (refused, asked) = vault.route_with("fixture-dev/claude-code-alab", &|dirs| {
        let learned = agh::routing::adopt(&vault.base, dirs);
        let record = dirs.config.join(agh::identity::vault::RECORD_FILE);
        std::fs::remove_file(&record).expect("the record adoption wrote");
        learned
    });
    let why = refused.expect_err("an unreadable record refuses");
    assert!(
        why.contains(
            "dev's login is not in the vault adoption record, which git's credential helper reads"
        ),
        "{why}"
    );
    assert!(why.contains("; next: "), "{why}");
    assert_eq!(asked, 0);
    assert_eq!(vault.user_calls(), 1, "the login was learned live");
}

/// A release-tier staging owner refuses before any helper is named: agh's helper answers a
/// release-tier repository only under a grant its own git runner writes, and the fetch writes
/// none. NEGATIVE CONTROL: without that check, `require_answer` refuses with a false reason (a
/// login missing from the record).
#[cfg(unix)]
#[test]
fn a_release_tier_fetch_refuses_and_names_no_helper() {
    let vault = Vault::new();
    let (refused, asked) = vault.route("alabsystems/claude-code-alab", true);
    let why = refused.expect_err("release tier refuses");
    assert!(
        why.contains("alabsystems/claude-code-alab is release tier"),
        "{why}"
    );
    assert_eq!(asked, 0);
}

/// The fetch's environment, as the helper sees it, lacks each name the fetch removes, so a
/// profile whose `token_env` names one is absent to the check as it is to the helper.
#[test]
fn the_fetch_environment_hides_each_removed_name() {
    let removed = source_git_removals();
    for name in [
        "GIT_DIR",
        "GIT_CONFIG_PARAMETERS",
        "ATPKG_PUBLISH_GIT_TOKEN",
    ] {
        assert!(removed.iter().any(|entry| entry == name), "{name}");
    }
    let env = FetchEnv {
        removed: vec!["PATH".into()],
    };
    assert_eq!(agh::identity::EnvSource::var_os(&env, "PATH"), None);
    let kept = FetchEnv {
        removed: Vec::new(),
    };
    assert_eq!(
        agh::identity::EnvSource::var_os(&kept, "PATH"),
        agh::identity::EnvSource::var_os(&agh::identity::ProcessEnv, "PATH")
    );
}

/// The variables the source fetch sets: git and SSH hygiene, nothing else.
const SET: [&str; 11] = [
    "GIT_TRACE",
    "GIT_TRACE2",
    "GIT_TRACE2_EVENT",
    "GIT_TRACE2_PERF",
    "GIT_TRACE_CURL",
    "GIT_TRACE_REDACT",
    "GIT_TERMINAL_PROMPT",
    "GIT_ASKPASS",
    "SSH_ASKPASS",
    "SSH_ASKPASS_REQUIRE",
    "GIT_SSH_COMMAND",
];

/// An https fetch mounts one helper, agh's (the retained executable followed by `agh auth
/// git-credential`), scoped to `https://github.com` with `credential.useHttpPath=true`, after
/// a reset of every other one; and no token is in its argv or its environment: it sets only
/// the hygiene variables and removes every ambient token selector. NEGATIVE CONTROL: a token
/// put back into the fetch's environment fails the set of variables.
#[test]
fn https_fetch_mounts_agh_s_helper_alone_and_carries_no_token() {
    let dir = aterm_tempfile::TempDir::new().expect("scratch");
    let config = config(dir.path());
    let tools = Recording::new(0, "");
    fetch_source(
        &tools,
        &config,
        "https://github.com/fixture/claude-code-alab.git",
        &retained_helper,
    )
    .expect("recorded fetch");
    let calls = tools.calls.borrow();
    let [run] = calls.as_slice() else {
        panic!("fetch must run exactly once")
    };
    let args = run.argv();
    assert_eq!(args[..2], ["-c", "credential.helper="]);
    let helpers: Vec<&String> = args.iter().filter(|arg| arg.contains("helper=")).collect();
    assert_eq!(
        helpers,
        [
            "credential.helper=",
            "credential.https://github.com.helper=!\"/retained/atpkg-publish\" \"agh\" auth \
             git-credential"
        ],
        "{args:?}"
    );
    let at = |word: &str| {
        args.iter()
            .position(|arg| arg == word)
            .unwrap_or_else(|| panic!("no {word} in {args:?}"))
    };
    assert_eq!(at(helpers[1]), 3);
    assert!(at("credential.useHttpPath=true") < at("fetch"), "{args:?}");
    for arg in &args {
        assert_eq!(agh::redact::Redactor::shapes(arg), *arg, "a token in argv");
        assert!(!arg.contains("ATPKG_PUBLISH_GIT_TOKEN"), "{arg}");
    }
    let mut set: Vec<&str> = run.env.iter().map(|(name, _)| name.as_str()).collect();
    set.sort_unstable();
    set.dedup();
    let mut want = SET.to_vec();
    want.sort_unstable();
    assert_eq!(set, want, "the fetch sets hygiene variables alone");
    for (name, value) in &run.env {
        let value = value.to_string_lossy();
        assert_eq!(agh::redact::Redactor::shapes(&value), value, "{name}");
    }
    assert_eq!(run.env_of("GIT_TERMINAL_PROMPT"), Some(&"0".into()));
    for name in ["GIT_TRACE2", "GIT_TRACE2_EVENT", "GIT_TRACE2_PERF"] {
        assert_eq!(run.env_of(name), Some(&"0".into()));
    }
    assert_eq!(run.env_of("GIT_TRACE_REDACT"), Some(&"1".into()));
    assert_eq!(run.env_of("GIT_ASKPASS"), Some(&"/usr/bin/false".into()));
    for name in [
        "ATPKG_PUBLISH_GIT_TOKEN",
        "GH_TOKEN",
        "GITHUB_TOKEN",
        "GIT_CURL_VERBOSE",
        "SHELLOPTS",
    ] {
        assert!(run.env_remove.iter().any(|entry| entry == name), "{name}");
    }
    assert_eq!(run.cwd.as_ref(), Some(&config.checkout));
    assert!(run.deadline.is_some());
}

/// A helper that cannot be named starts no git (there is no token fallback), and a failed
/// fetch's text is redacted of every token shape.
#[test]
fn an_unnamed_helper_starts_no_git_and_fetch_failures_redact_token_shapes() {
    let dir = aterm_tempfile::TempDir::new().expect("scratch");
    let config = config(dir.path());
    let tools = Recording::new(0, "");
    let unnamed = || -> std::result::Result<String, String> {
        Err("recorded image custody is unavailable".into())
    };
    let refused = fetch_source(
        &tools,
        &config,
        "https://github.com/fixture/claude-code-alab.git",
        &unnamed,
    )
    .expect_err("no helper, no fetch")
    .to_string();
    assert!(
        refused.contains("local source credential: recorded image custody is unavailable"),
        "{refused}"
    );
    assert!(tools.calls.borrow().is_empty());
    let secret = ["github_", "pat_failed_fetch_fixture"].concat();
    let failed = Recording::new(128, &format!("remote diagnostic {secret}"));
    let error = fetch_source(
        &failed,
        &config,
        "https://github.com/fixture/claude-code-alab.git",
        &retained_helper,
    )
    .expect_err("refused fetch")
    .to_string();
    assert!(!error.contains(&secret), "{error}");
    assert!(error.contains("remote diagnostic [redacted]"), "{error}");
    assert_eq!(failed.calls.borrow().len(), 1);
}

#[test]
fn ssh_fetch_keeps_batch_mode_and_mounts_no_helper() {
    let dir = aterm_tempfile::TempDir::new().expect("scratch");
    let config = config(dir.path());
    let tools = Recording::new(0, "");
    let never = || -> std::result::Result<String, String> {
        panic!("an SSH fetch never names a credential helper")
    };
    fetch_source(
        &tools,
        &config,
        "git@github.com:fixture/claude-code-alab.git",
        &never,
    )
    .expect("recorded SSH fetch");
    let calls = tools.calls.borrow();
    let [run] = calls.as_slice() else {
        panic!("fetch must run exactly once")
    };
    assert_eq!(
        run.env_of("GIT_SSH_COMMAND"),
        Some(&"ssh -oBatchMode=yes -oNumberOfPasswordPrompts=0".into())
    );
    let args = run.argv();
    assert_eq!(args[..2], ["-c", "credential.helper="]);
    assert!(
        !args
            .iter()
            .any(|arg| arg.contains("helper=!") || arg.contains("useHttpPath")),
        "{args:?}"
    );
}

/// The fetch's own configuration, run by a real git against a user config whose helpers (one
/// for every URL, one scoped to `https://github.com`) answer a planted credential to any
/// request, path or none, and which turns `useHttpPath` off: git asks agh's helper alone, and
/// hands it the repository's path. Without the fetch's words the planted credential answers,
/// so the user's helpers are live. NEGATIVE CONTROL: without the `credential.helper=` reset,
/// the user's helpers come first and the planted credential answers.
#[test]
fn a_helper_the_users_config_names_is_never_asked() {
    let dir = aterm_tempfile::TempDir::new().expect("scratch");
    let config = config(dir.path());
    // Run by `/bin/sh` itself, so no new executable starts: it answers every `get`.
    let planted = dir.path().join("planted-helper.sh");
    std::fs::write(
        &planted,
        "test \"$1\" = get || exit 0\ncat >/dev/null\n\
         printf 'username=intruder\\npassword=planted\\n'\n",
    )
    .expect("planted");
    let user = dir.path().join("gitconfig");
    std::fs::write(
        &user,
        format!(
            "[credential]\n\tuseHttpPath = false\n\thelper = !/bin/sh '{0}'\n\
             [credential \"https://github.com\"]\n\thelper = !/bin/sh '{0}'\n",
            planted.display()
        ),
    )
    .expect("the user's config");
    // A stand-in for agh's helper: it answers, and names the path git sent it.
    let stand_in = || -> std::result::Result<String, String> {
        agh::git::HelperCommand {
            exe: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                "while IFS= read -r line; do case \"$line\" in path=*) path=${line#path=} ;; \
                 esac; done; printf 'username=agh-fixture\\npassword=agh-fixture-%s\\n' \
                 \"$path\""
                    .into(),
                "sh".into(),
            ],
        }
        .config_value()
        .map_err(|e| e.to_string())
    };
    let tools = Recording::new(0, "");
    fetch_source(
        &tools,
        &config,
        "https://github.com/fixture/claude-code-alab.git",
        &stand_in,
    )
    .expect("recorded fetch");
    let calls = tools.calls.borrow();
    let [run] = calls.as_slice() else {
        panic!("fetch must run exactly once")
    };
    let fetch = run
        .args
        .iter()
        .position(|arg| arg == "fetch")
        .expect("the fetch verb");
    let request = dir.path().join("request");
    std::fs::write(
        &request,
        "protocol=https\nhost=github.com\npath=fixture/claude-code-alab.git\n\n",
    )
    .expect("the request");
    let fill = |words: &[std::ffi::OsString]| {
        let mut git = crate::testkit::git();
        for name in &run.env_remove {
            git.env_remove(name);
        }
        for (name, value) in &run.env {
            git.env(name, value);
        }
        git.env("GIT_CONFIG_GLOBAL", &user)
            .args(words)
            .args(["credential", "fill"]);
        let stdin = std::fs::File::open(&request).expect("the request");
        // A hang detector, not a budget: git answers from its helpers at once.
        let hang = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let out = aterm_uds::deadline::output_until_from(&mut git, stdin.into(), hang)
            .expect("git credential fill");
        assert!(out.status.success(), "{out:?}");
        String::from_utf8(out.stdout).expect("UTF-8 answer")
    };
    let answered = fill(&run.args[..fetch]);
    assert!(
        answered.contains("username=agh-fixture\n")
            && answered.contains("password=agh-fixture-fixture/claude-code-alab.git\n")
            && !answered.contains("intruder"),
        "{answered}"
    );
    let unreset = fill(&[]);
    assert!(unreset.contains("username=intruder\n"), "{unreset}");
}
