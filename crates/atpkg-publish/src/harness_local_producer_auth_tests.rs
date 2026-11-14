// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::cell::{Cell, RefCell};

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
fn retained_command() -> std::result::Result<String, String> {
    agh::git::HelperCommand {
        exe: "/retained/atpkg-publish".into(),
        args: vec!["agh".into()],
    }
    .config_value()
    .map_err(|e| e.to_string())
}

/// That helper as a fetch mounts it for a profile whose token file is read strictly.
fn retained_helper() -> std::result::Result<AghHelper, String> {
    retained_command().map(|command| AghHelper {
        command,
        loose: false,
    })
}

/// [`retained_helper`] as [`fetch_source`] asks for it, whatever list it is judged with.
fn mounted(_removed: &[String]) -> std::result::Result<AghHelper, String> {
    retained_helper()
}

/// The staging repository every routing test fetches.
const STAGING: &str = "fixture-dev/claude-code-alab";

/// A machine under a scratch root, with agh-fake standing in for GitHub behind `base` (the
/// production client's shape before routing). [`Machine::vault`] is a vault-adopted one: no
/// agh.toml, and `~/.secrets/gh_access_token` holding a token agh-fake answers `GET /user` for
/// as `fixture-dev`, whose login no agh call has recorded yet.
struct Machine {
    root: aterm_tempfile::TempDir,
    dirs: agh::config::Dirs,
    fake: agh_fake::FakeGithub,
    base: agh::Github,
}

impl Machine {
    fn new() -> Self {
        let root = aterm_tempfile::Builder::new()
            .prefix("harness-vault-")
            .suffix(".noindex")
            .tempdir()
            .expect("scratch");
        let dirs = agh::config::Dirs::under(root.path());
        aterm_uds::private_file::create_private_dir(&dirs.home).expect("home");
        let fake = agh_fake::FakeGithub::start();
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
            root,
            dirs,
            fake,
            base,
        }
    }

    fn vault() -> Self {
        let machine = Self::new();
        let token = agh_fake::token("harness-dev");
        machine.fake.add_token(&token, "fixture-dev");
        let secrets = machine.dirs.home.join(".secrets");
        aterm_uds::private_file::create_private_dir(&secrets).expect("the vault");
        let file = agh::identity::vault::Slot::Dev.path(&machine.dirs.home);
        let name = file.file_name().expect("the vault file").to_string_lossy();
        aterm_uds::private_file::write_private(&secrets, &name, token.as_bytes())
            .expect("the vault token");
        machine
    }

    /// `text`, `{root}` filled with the scratch root, as this machine's private agh.toml.
    fn agh_toml(&self, text: &str) {
        let parent = self.dirs.config.parent().expect("the config's parent");
        std::fs::create_dir_all(parent).expect("the config's parent");
        aterm_uds::private_file::create_private_dir(&self.dirs.config).expect("the config");
        let file = self.dirs.config_file();
        let name = file.file_name().expect("agh.toml").to_string_lossy();
        let text = text.replace("{root}", &self.root.path().display().to_string());
        aterm_uds::private_file::write_private(&self.dirs.config, &name, text.as_bytes())
            .expect("agh.toml");
    }

    /// The production wiring under `agh_helper` ([`fetch_helper`]) for [`STAGING`] over this
    /// machine, in `inner` holding `names` (so a fetch removes [`source_git_removals_from`]
    /// them), and how often it asked `helper()`.
    fn fetch(
        &self,
        inner: &agh::identity::MapEnv,
        names: &[&str],
        helper: &dyn Fn() -> std::result::Result<String, String>,
    ) -> (std::result::Result<AghHelper, String>, usize) {
        let slug = agh::Slug::parse(STAGING).expect("the slug");
        let asked = Cell::new(0);
        let fetched = fetch_helper(
            &self.dirs,
            &self.base,
            inner,
            &source_git_removals_from(names.iter().map(OsString::from)),
            &slug,
            &|| {
                asked.set(asked.get() + 1);
                helper()
            },
        );
        (fetched, asked.get())
    }

    /// `routed_helper` for `slug` over this machine with `learn` in place of the production
    /// learn step, in an empty environment, and how often it asked the helper.
    fn route_with(
        &self,
        slug: &str,
        learn: &dyn Fn(&agh::config::Dirs) -> agh::Result<agh::routing::Configured>,
    ) -> (std::result::Result<AghHelper, String>, usize) {
        let slug = agh::Slug::parse(slug).expect("the slug");
        let asked = Cell::new(0);
        let env = agh::identity::MapEnv::new();
        let routed = routed_helper(&self.dirs, &env, &slug, learn, &|| {
            asked.set(asked.get() + 1);
            retained_command()
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

    /// Whether agh's vault adoption record exists: what a learned login writes.
    fn recorded(&self) -> bool {
        self.dirs
            .config
            .join(agh::identity::vault::RECORD_FILE)
            .try_exists()
            .expect("the record's directory")
    }
}

/// On a vault machine whose token's login agh has not recorded, the production wiring
/// (`fetch_helper`, the whole of `agh_helper` but its process inputs) learns it first (one
/// `GET /user`, as any routed agh call would) and then names agh's helper, which reads only the
/// record and now answers the staging repository as `dev`; a second fetch learns nothing more.
/// The recorded view alone, `routing::view_for_host` in place of the learn step, refuses: agh
/// routes the repository with no identity (the unrecorded token routes nothing). NEGATIVE
/// CONTROL: `view_for_host` in place of `adopt_for_host` in `fetch_helper` fails the second
/// half.
#[cfg(unix)]
#[test]
fn a_vault_login_agh_has_not_recorded_is_learned_before_the_fetch_names_the_helper() {
    let vault = Machine::vault();
    let (unlearned, _) = vault.route_with(STAGING, &|dirs| {
        agh::routing::view_for_host(dirs, &agh::identity::MapEnv::new())
    });
    let why = unlearned.expect_err("an unrecorded login refuses");
    assert!(
        why.contains("agh routes fixture-dev/claude-code-alab with no identity"),
        "{why}"
    );
    assert_eq!(vault.user_calls(), 0, "a view learns nothing");
    assert!(!vault.recorded(), "a view records nothing");
    let none = agh::identity::MapEnv::new();
    let (learned, asked) = vault.fetch(&none, &[], &retained_command);
    assert_eq!(
        learned,
        retained_helper(),
        "the learned route names the helper"
    );
    assert_eq!(asked, 1);
    assert_eq!(vault.user_calls(), 1, "one GET /user learns the login");
    assert!(vault.recorded(), "the learned login is recorded");
    let (recorded, _) = vault.route_with(STAGING, &|dirs| {
        agh::routing::view_for_host(dirs, &agh::identity::MapEnv::new())
    });
    assert_eq!(recorded, retained_helper(), "the helper reads the record");
    let (again, _) = vault.fetch(&none, &[], &retained_command);
    assert_eq!(again, retained_helper());
    assert_eq!(
        vault.user_calls(),
        1,
        "a recorded login is not learned again"
    );
}

/// A login learned live whose record the helper cannot read (the record could not be written,
/// or was removed since) refuses with agh's own reason and next step: the helper reads only the
/// record, so git would end on its `quit=1`. NEGATIVE CONTROL: without `require_answer` in
/// `routed_helper`, the helper is returned for a fetch it will not answer.
#[cfg(unix)]
#[test]
fn a_learned_login_the_helper_cannot_read_refuses_with_agh_s_reason() {
    let vault = Machine::vault();
    let (refused, asked) = vault.route_with(STAGING, &|dirs| {
        let learned = agh::routing::adopt_for_host(&vault.base, dirs);
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
    assert_eq!(asked, 1, "the helper is named before any request");
    assert_eq!(vault.user_calls(), 1, "the login was learned live");
}

/// A release-tier staging owner refuses by the recorded routing alone, before any request or
/// record: agh's helper answers a release-tier repository only under a grant its own git
/// runner writes, and the fetch writes none, so a `GET /user` would learn nothing it could use.
/// NEGATIVE CONTROL: the tier judged from the learned routing instead (after `learn`) sends the
/// `GET /user` and writes the record, failing both counts.
#[cfg(unix)]
#[test]
fn a_release_tier_fetch_refuses_before_any_request_or_record() {
    let vault = Machine::vault();
    let slug = agh::Slug::parse("alabsystems/claude-code-alab").expect("the slug");
    let env = agh::identity::MapEnv::new();
    let refused = fetch_helper(
        &vault.dirs,
        &vault.base,
        &env,
        &source_git_removals_from(std::iter::empty()),
        &slug,
        &retained_command,
    );
    let why = refused.expect_err("release tier refuses");
    assert!(
        why.contains("alabsystems/claude-code-alab is release tier"),
        "{why}"
    );
    assert_eq!(vault.user_calls(), 0, "nothing is learned for a refusal");
    assert!(!vault.recorded(), "nothing is recorded for a refusal");
}

/// A helper that cannot be named (a frontend whose recorded image custody is unavailable)
/// refuses with its own reason before any request or record. NEGATIVE CONTROL: `helper()`
/// asked after the learn step sends the `GET /user` and writes the record first.
#[cfg(unix)]
#[test]
fn an_unnamed_helper_refuses_before_any_request_or_record() {
    let vault = Machine::vault();
    let unnamed = || -> std::result::Result<String, String> {
        Err("recorded image custody is unavailable".into())
    };
    let (refused, asked) = vault.fetch(&agh::identity::MapEnv::new(), &[], &unnamed);
    assert_eq!(
        refused,
        Err("recorded image custody is unavailable".to_string())
    );
    assert_eq!(asked, 1);
    assert_eq!(vault.user_calls(), 0, "nothing is learned for a refusal");
    assert!(!vault.recorded(), "nothing is recorded for a refusal");
}

/// When GitHub cannot be reached to learn the vault token's login, the refusal names that
/// failure (agh's adoption note: the token, and why it was not adopted), so an outage does not
/// read as a machine agh routes with no identity by its configuration. NEGATIVE CONTROL:
/// without the note in `routed_helper`'s refusal, only "with no identity" is left.
#[cfg(unix)]
#[test]
fn a_login_github_cannot_be_reached_for_is_named_in_the_refusal() {
    let vault = Machine::vault();
    vault
        .fake
        .fault_always(agh_fake::Match::api("/user"), agh_fake::Fault::Close);
    let (refused, _) = vault.fetch(&agh::identity::MapEnv::new(), &[], &retained_command);
    let why = refused.expect_err("an unlearned login refuses");
    assert!(
        why.contains("agh routes fixture-dev/claude-code-alab with no identity"),
        "{why}"
    );
    assert!(why.contains("agh: note vault=dev"), "{why}");
    assert!(why.contains("was not adopted"), "{why}");
    assert!(vault.user_calls() >= 1, "GitHub was asked");
    assert!(!vault.recorded(), "nothing was learned to record");
}

/// The agh.toml rows of an `env` profile, `{var}` its variable, which alone reads the staging
/// owner.
const ENV_PROFILE: &str = "[profiles.ci]\nenv = \"{var}\"\ntier = \"dev\"\n\n\
     [[owners]]\nname = \"fixture-dev\"\ntier = \"dev\"\nread = [\"ci\"]\n";

/// An agh.toml profile whose `env` names a variable the fetch removes (`GH_TOKEN` always, a
/// `GH_*` name the environment holds) is absent to the check, as it is to the helper git
/// starts, so the fetch refuses; one whose variable the fetch keeps answers. The check judges
/// the fetch's own environment, the caller's `inner` without the fetch's removals.
/// NEGATIVE CONTROL: `inner` itself in place of `FetchEnv` (or a `FetchEnv` that removes
/// nothing) answers the `GH_*` profiles; `ProcessEnv` in place of `inner` refuses the kept one.
#[cfg(unix)]
#[test]
fn an_agh_toml_profile_is_judged_in_the_fetch_s_own_environment() {
    let token = agh_fake::token("harness-ci");
    for (var, answers) in [
        ("GH_TOKEN", false),
        ("GH_FIXTURE_TOKEN", false),
        ("HARNESS_FIXTURE_TOKEN", true),
    ] {
        let machine = Machine::new();
        machine.agh_toml(&ENV_PROFILE.replace("{var}", var));
        let inner = agh::identity::MapEnv::new().with(var, &token);
        let (fetched, _) = machine.fetch(&inner, &[var], &retained_command);
        if answers {
            assert_eq!(fetched, retained_helper(), "{var}");
        } else {
            let why = fetched.expect_err("a removed variable reads as absent");
            assert!(
                why.contains("no profile in fixture-dev's read chain is present"),
                "{var}: {why}"
            );
        }
        assert_eq!(machine.user_calls(), 0, "agh.toml alone routes");
    }
}

/// The agh.toml of a `legacy_loose` profile (design §6.6, M2 only) whose token file has a
/// group and other read bit, which only the library host's loader accepts.
const LOOSE_PROFILE: &str = "[profiles.ci]\ntoken_file = \"{root}/tokens/ci.token\"\n\
     tier = \"dev\"\nlegacy_loose = true\n\n\
     [[owners]]\nname = \"fixture-dev\"\ntier = \"dev\"\nread = [\"ci\"]\n";

/// A `legacy_loose` profile is routed and checked loose, as atpkg-publish's agh host reads
/// it (`AghHost::configured_client` routes by `adopt_for_host`), and the helper comes back
/// marked loose, so the fetch sets `AGH_GIT_LOOSE=1` for the helper git starts. The strict
/// routing every agh verb uses (`routing::adopt`) refuses the same machine. The token file's
/// mode is the loose part; its directory is made private, so no umask the runner has (`002`
/// leaves a group write bit, which even the loose loader refuses) decides the test.
/// NEGATIVE CONTROL: `adopt` in place of `adopt_for_host` in `fetch_helper`, or `loose` false
/// to `require_answer`, refuses the first half.
#[cfg(unix)]
#[test]
fn a_legacy_loose_profile_is_routed_and_checked_loose_as_the_host_reads_it() {
    use std::os::unix::fs::PermissionsExt as _;
    let machine = Machine::new();
    machine.agh_toml(LOOSE_PROFILE);
    let tokens = machine.root.path().join("tokens");
    aterm_uds::private_file::create_private_dir(&tokens).expect("the token directory");
    let file = tokens.join("ci.token");
    std::fs::write(&file, agh_fake::token("harness-loose")).expect("the token file");
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    let (fetched, _) = machine.fetch(&agh::identity::MapEnv::new(), &[], &retained_command);
    assert_eq!(
        fetched,
        retained_command().map(|command| AghHelper {
            command,
            loose: true
        })
    );
    let (strict, _) = machine.route_with(STAGING, &|dirs| agh::routing::adopt(&machine.base, dirs));
    let why = strict.expect_err("the strict routing refuses a loose file");
    assert!(
        why.contains("ci in fixture-dev's read chain is unusable"),
        "{why}"
    );
}

/// The fetch's environment, as the helper sees it, lacks each name the fetch removes, so a
/// profile whose `env` names one is absent to the check as it is to the helper; every other
/// name reads as the caller's environment has it.
#[test]
fn the_fetch_environment_hides_each_removed_name() {
    let removed = source_git_removals_from(["GIT_SSL_NO_VERIFY", "GH_FIXTURE"].map(OsString::from));
    for name in [
        "GIT_DIR",
        "GIT_CONFIG_PARAMETERS",
        "ATPKG_PUBLISH_GIT_TOKEN",
        "GH_TOKEN",
        "GIT_SSL_NO_VERIFY",
        "GH_FIXTURE",
    ] {
        assert!(removed.iter().any(|entry| entry == name), "{name}");
    }
    let inner = agh::identity::MapEnv::new()
        .with("PATH", "/fixture/bin")
        .with("HOME", "/fixture/home");
    let removed = ["PATH".to_string()];
    let env = FetchEnv {
        inner: &inner,
        removed: &removed,
    };
    assert_eq!(agh::identity::EnvSource::var_os(&env, "PATH"), None);
    assert_eq!(
        agh::identity::EnvSource::var_os(&env, "HOME"),
        Some("/fixture/home".into())
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

/// Each variable `run` sets, once, sorted.
fn set_names(run: &Run) -> Vec<&str> {
    let mut set: Vec<&str> = run.env.iter().map(|(name, _)| name.as_str()).collect();
    set.sort_unstable();
    set.dedup();
    set
}

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
        &mounted,
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
    for pin in ["http.sslVerify=true", "http.followRedirects=false"] {
        assert_eq!(args[at(pin) - 1], "-c", "{args:?}");
        assert!(at(pin) < at("fetch"), "{args:?}");
    }
    for arg in &args {
        assert_eq!(agh::redact::Redactor::shapes(arg), *arg, "a token in argv");
        assert!(!arg.contains("ATPKG_PUBLISH_GIT_TOKEN"), "{arg}");
    }
    let mut want = SET.to_vec();
    want.sort_unstable();
    assert_eq!(
        set_names(run),
        want,
        "the fetch sets hygiene variables alone"
    );
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

/// The helper is judged with the very list of names the fetch's git removes: one list, so a
/// profile whose `env` names a variable the git drops (a `GH_*` name the environment holds,
/// say) is absent to the check as it is to the helper git starts. NEGATIVE CONTROL: the helper
/// handed another list (`helper(&[])` in `fetch_source`) fails this test.
#[test]
fn the_helper_is_judged_with_the_list_the_fetch_removes() {
    let dir = aterm_tempfile::TempDir::new().expect("scratch");
    let config = config(dir.path());
    let tools = Recording::new(0, "");
    let judged = RefCell::new(None);
    fetch_source(
        &tools,
        &config,
        "https://github.com/fixture/claude-code-alab.git",
        &|removed| {
            *judged.borrow_mut() = Some(removed.to_vec());
            retained_helper()
        },
    )
    .expect("recorded fetch");
    let calls = tools.calls.borrow();
    let [run] = calls.as_slice() else {
        panic!("fetch must run exactly once")
    };
    let judged = judged.into_inner().expect("the helper was judged");
    assert!(!judged.is_empty(), "the fetch removes ambient selectors");
    assert_eq!(judged, run.env_remove);
}

/// A helper that answers as a `legacy_loose` profile is mounted with `AGH_GIT_LOOSE=1` in the
/// fetch's environment, set after its removals, so agh's helper reads that profile's token
/// file loose, as the check judged it; a strict one's fetch sets the hygiene variables alone
/// (above). NEGATIVE CONTROL: without the variable for a loose helper, this test fails.
#[test]
fn a_loose_helper_is_told_so_in_the_fetch_s_environment() {
    let dir = aterm_tempfile::TempDir::new().expect("scratch");
    let config = config(dir.path());
    let tools = Recording::new(0, "");
    let loose = |_: &[String]| {
        retained_command().map(|command| AghHelper {
            command,
            loose: true,
        })
    };
    fetch_source(
        &tools,
        &config,
        "https://github.com/fixture/claude-code-alab.git",
        &loose,
    )
    .expect("recorded fetch");
    let calls = tools.calls.borrow();
    let [run] = calls.as_slice() else {
        panic!("fetch must run exactly once")
    };
    assert_eq!(run.env_of(agh::git::LOOSE_VAR), Some(&"1".into()));
    let mut want = SET.to_vec();
    want.push(agh::git::LOOSE_VAR);
    want.sort_unstable();
    assert_eq!(set_names(run), want);
}

/// A helper that cannot be named starts no git (there is no token fallback), and a failed
/// fetch's text is redacted of every token shape.
#[test]
fn an_unnamed_helper_starts_no_git_and_fetch_failures_redact_token_shapes() {
    let dir = aterm_tempfile::TempDir::new().expect("scratch");
    let config = config(dir.path());
    let tools = Recording::new(0, "");
    let unnamed = |_: &[String]| -> std::result::Result<AghHelper, String> {
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
        &mounted,
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
    let never = |_: &[String]| -> std::result::Result<AghHelper, String> {
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
    assert_eq!(args[2..6], GIT_TLS_PINS, "{args:?}");
}

/// The repository URL every fetch test fetches.
const ORIGIN: &str = "https://github.com/fixture/claude-code-alab.git";

/// The fetch's own configuration, read by a real git as git's HTTP transport reads it (by URL
/// match against the origin, `config --get-urlmatch`), in a checkout whose own config, the
/// user's and the system's each turn TLS verification off and follow every redirect: unscoped,
/// scoped to `https://github.com`, and scoped to the repository's exact URL (with and without a
/// trailing slash), the most specific keys git can match there. git takes agh's pins
/// (verification on, no redirect) over all of them, so the credential agh's helper answers
/// never goes to an unverified or redirected peer. NEGATIVE CONTROLS: without the fetch's words
/// git reads the configs' values, so they are live; with the unscoped pins alone
/// (`GIT_TLS_PINS` without `git_tls_pins_at`) the scoped keys still win.
#[test]
fn the_fetch_pins_tls_over_any_config_that_turns_it_off() {
    const HANG_DETECTOR: std::time::Duration = std::time::Duration::from_secs(60);
    let dir = aterm_tempfile::TempDir::new().expect("scratch");
    let config = config(dir.path());
    let off = "\tsslVerify = false\n\tfollowRedirects = true\n";
    let user = dir.path().join("gitconfig");
    std::fs::write(
        &user,
        format!("[http]\n{off}[http \"https://github.com\"]\n{off}[http \"{ORIGIN}\"]\n{off}"),
    )
    .expect("the user's config");
    let system = dir.path().join("system-gitconfig");
    std::fs::write(&system, format!("[http \"{ORIGIN}/\"]\n{off}")).expect("the system's");
    let local = |args: &[&str]| {
        let mut git = crate::testkit::git();
        git.args(args);
        // A hang detector, not a budget: each is one local repository write.
        let out =
            aterm_uds::deadline::output_until(&mut git, std::time::Instant::now() + HANG_DETECTOR)
                .expect("fixture git");
        assert!(out.status.success(), "{out:?}");
    };
    let checkout = config.checkout.to_str().expect("a UTF-8 checkout");
    local(&["init", "--quiet", checkout]);
    for (key, value) in [("sslVerify", "false"), ("followRedirects", "true")] {
        local(&[
            "-C",
            checkout,
            "config",
            &format!("http.{ORIGIN}.{key}"),
            value,
        ]);
    }
    let tools = Recording::new(0, "");
    fetch_source(&tools, &config, ORIGIN, &mounted).expect("recorded fetch");
    let calls = tools.calls.borrow();
    let [run] = calls.as_slice() else {
        panic!("fetch must run exactly once")
    };
    let fetch = run
        .args
        .iter()
        .position(|arg| arg == "fetch")
        .expect("the fetch verb");
    let read = |words: &[std::ffi::OsString], key: &str| {
        let mut git = crate::testkit::git();
        for name in &run.env_remove {
            git.env_remove(name);
        }
        for (name, value) in &run.env {
            git.env(name, value);
        }
        git.env_remove("GIT_CONFIG_NOSYSTEM")
            .env("GIT_CONFIG_SYSTEM", &system)
            .env("GIT_CONFIG_GLOBAL", &user)
            .current_dir(&config.checkout)
            .args(words)
            .args(["config", "--get-urlmatch", key, ORIGIN]);
        // A hang detector, not a budget: git reads its config at once.
        let out =
            aterm_uds::deadline::output_until(&mut git, std::time::Instant::now() + HANG_DETECTOR)
                .expect("git config");
        assert!(out.status.success(), "{out:?}");
        String::from_utf8(out.stdout)
            .expect("UTF-8 value")
            .trim_end()
            .to_string()
    };
    let words = &run.args[..fetch];
    assert_eq!(read(words, "http.sslVerify"), "true");
    assert_eq!(read(words, "http.followRedirects"), "false");
    assert_eq!(read(&[], "http.sslVerify"), "false");
    assert_eq!(read(&[], "http.followRedirects"), "true");
    let unscoped: Vec<std::ffi::OsString> = words
        .chunks(2)
        .filter(|pair| {
            assert_eq!(pair[0], "-c", "{words:?}");
            !pair[1].to_string_lossy().starts_with("http.https://")
        })
        .flatten()
        .cloned()
        .collect();
    assert_eq!(unscoped.len() + 4, words.len(), "{words:?}");
    assert_eq!(read(&unscoped, "http.sslVerify"), "false");
    assert_eq!(read(&unscoped, "http.followRedirects"), "true");
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
    let stand_in = |_: &[String]| -> std::result::Result<AghHelper, String> {
        let command = agh::git::HelperCommand {
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
        .map_err(|e| e.to_string())?;
        Ok(AghHelper {
            command,
            loose: false,
        })
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

/// A fetch whose new main moves a populated submodule's gitlink fetches `origin` alone, though
/// git's default (`fetch.recurseSubmodules=on-demand`) would start a fetch in that submodule,
/// which inherits agh's helper and the unscoped pins through `GIT_CONFIG_PARAMETERS` but not
/// the pins at `origin`, and asks the helper for a repository [`routed_helper`] never judged.
/// The fetch's own words and environment, run by a real git against a loopback smart-HTTP
/// origin, bring the new main, and the submodule's remote, which no fetch could reach, is never
/// asked. NEGATIVE CONTROL: without `--recurse-submodules=no` git starts the submodule's fetch,
/// which fails, and the fetch with it.
#[test]
fn the_fetch_never_starts_a_fetch_in_a_populated_submodule() {
    const HANG_DETECTOR: std::time::Duration = std::time::Duration::from_secs(60);
    const READER: &str = "fixture-reader";
    let dir = aterm_tempfile::TempDir::new().expect("scratch");
    let config = config(dir.path());
    let fixture = |cwd: &Path, args: &[&str]| {
        let mut git = crate::testkit::git();
        git.current_dir(cwd)
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "protocol.file.allow=always",
            ])
            .args(args);
        // A hang detector, not a budget: each is one local repository read or write.
        let out =
            aterm_uds::deadline::output_until(&mut git, std::time::Instant::now() + HANG_DETECTOR)
                .expect("fixture git");
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8(out.stdout)
            .expect("UTF-8 output")
            .trim_end()
            .to_string()
    };
    let utf8 = |path: &Path| path.to_str().expect("a UTF-8 scratch").to_string();
    let sub = dir.path().join("sub");
    let upstream = dir.path().join("upstream");
    for repo in [&sub, &upstream] {
        std::fs::create_dir(repo).expect("a repository");
        fixture(repo, &["init", "--quiet", "--initial-branch=main"]);
        std::fs::write(repo.join("README"), "fixture\n").expect("a file");
        fixture(repo, &["add", "README"]);
    }
    fixture(&sub, &["commit", "--quiet", "-m", "sub"]);
    fixture(
        &upstream,
        &["submodule", "add", "--quiet", &utf8(&sub), "sub"],
    );
    fixture(
        &upstream,
        &["commit", "--quiet", "-m", "with its submodule"],
    );
    fixture(
        dir.path(),
        &[
            "clone",
            "--quiet",
            "--recurse-submodules",
            &utf8(&upstream),
            &utf8(&config.checkout),
        ],
    );
    let nowhere = format!(
        "file://{}",
        dir.path().join("no-such-submodule.git").display()
    );
    fixture(
        &config.checkout.join("sub"),
        &["remote", "set-url", "origin", &nowhere],
    );
    let moved = format!("160000,{},sub", "1".repeat(40));
    fixture(&upstream, &["update-index", "--cacheinfo", &moved]);
    fixture(
        &upstream,
        &["commit", "--quiet", "-m", "moves its submodule"],
    );
    let main = fixture(&upstream, &["rev-parse", "HEAD"]);
    let backend = agh_fake::git_backend::GitHttpBackend::start(
        "fixture/claude-code-alab",
        &upstream,
        READER,
        "fixture-writer",
    )
    .expect("the loopback origin");
    fixture(
        &config.checkout,
        &[
            "remote",
            "set-url",
            "origin",
            &format!(
                "http://x-access-token:{READER}@127.0.0.1:{}/fixture/claude-code-alab.git",
                backend.port()
            ),
        ],
    );
    let tools = Recording::new(0, "");
    fetch_source(&tools, &config, ORIGIN, &mounted).expect("recorded fetch");
    let calls = tools.calls.borrow();
    let [run] = calls.as_slice() else {
        panic!("fetch must run exactly once")
    };
    let mut git = crate::testkit::git();
    for name in &run.env_remove {
        git.env_remove(name);
    }
    for (name, value) in &run.env {
        git.env(name, value);
    }
    git.current_dir(&config.checkout).args(&run.args);
    // A hang detector, not a budget: one loopback fetch of one commit.
    let out =
        aterm_uds::deadline::output_until(&mut git, std::time::Instant::now() + HANG_DETECTOR)
            .expect("the fetch");
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{said}");
    assert!(!said.contains("submodule"), "{said}");
    assert_eq!(
        fixture(&config.checkout, &["rev-parse", "refs/remotes/origin/main"]),
        main
    );
}
