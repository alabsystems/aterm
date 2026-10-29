// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Git's effective HTTP authentication config, without network or the person's config.

use super::*;
use std::time::Duration;

const PUBLIC: &str = "https://github.com/alabsystems/aterm.git";
const PRIVATE: &str = "https://github.com/alabsystems/private.git";
const PRIVATE_INHERITED: &str = "https://github.com/alabsystems/inherited.git";
const OWNER_BOUNDARY: &str = "https://github.com/alabsystems-private/private.git";
const PRIVATE_AUTH: &str = "Authorization: Fixture private";

/// Query Git's actual URL-match precedence using only an owned global config, caller entries
/// and the policy the export engine receives. No Git transport is allowed; these are config
/// reads, never an on-wire authentication claim.
fn config_context(
    root: &Path,
    config: &Path,
    parent: &[(String, String)],
    reads: &[(String, std::ffi::OsString)],
) -> Command {
    let mut git = aterm_uds::fixture_git::git();
    git.env_clear()
        .current_dir(root)
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join("xdg"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", config)
        .env("GIT_CONFIG_COUNT", (parent.len() / 2).to_string())
        .env("GIT_ALLOW_PROTOCOL", "")
        .env("GIT_TERMINAL_PROMPT", "0")
        .envs(parent.iter().map(|(key, value)| (key, value)))
        .envs(reads.iter().map(|(key, value)| (key, value)))
        .stdin(Stdio::null());
    git
}

fn effective_headers(
    root: &Path,
    config: &Path,
    parent: &[(String, String)],
    reads: &[(String, std::ffi::OsString)],
    url: &str,
) -> String {
    matched_headers(config_context(root, config, parent, reads), url)
}

fn matched_headers(mut git: Command, url: &str) -> String {
    git.args(["config", "--get-urlmatch", "http.extraHeader", url]);
    let out = aterm_uds::deadline::output_within(&mut git, Duration::from_secs(60))
        .expect("the scratch config query ends");
    assert!(
        matches!(out.status.code(), Some(0 | 1)),
        "scratch Git config query: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn filled_credentials(mut git: Command, url: &str) -> std::process::Output {
    let dir = aterm_tempfile::tempdir().unwrap();
    let input = dir.path().join("credential-input");
    std::fs::write(&input, format!("url={url}\n\n")).unwrap();
    git.args(["credential", "fill"]);
    aterm_uds::deadline::output_until_from(
        &mut git,
        Stdio::from(std::fs::File::open(input).unwrap()),
        std::time::Instant::now() + Duration::from_secs(60),
    )
    .expect("the scratch credential helper ends")
}

/// Both inherited files and caller command-config can authenticate without a credential
/// helper. Public owner reads must have no effective Authorization header, including an entry
/// more specific than the owner, while a different owner keeps its exact authentication.
/// NEGATIVE CONTROL: the helper-only policy, or clearing broad http.extraHeader alone, leaves
/// a fixture Authorization header effective on at least one public URL.
#[test]
fn public_http_reads_refuse_inherited_headers_without_changing_private_authentication() {
    let mut observed = Vec::new();
    for (label, inherited, command_config) in [
        (
            "global",
            "[http]\n extraHeader = Authorization: Fixture global\n",
            None,
        ),
        (
            "host",
            "[http \"https://github.com\"]\n extraHeader = Authorization: Fixture host\n",
            None,
        ),
        (
            "repo-file",
            "[http \"https://github.com/alabsystems/aterm.git\"]\n extraHeader = Authorization: Fixture repo-file\n",
            None,
        ),
        (
            "repo-command",
            "[http]\n extraHeader = Authorization: Fixture global\n",
            Some((
                "http.https://github.com/alabsystems/aterm.git.extraHeader",
                "Authorization: Fixture repo-command",
            )),
        ),
        (
            "host-case",
            "[http \"https://GITHUB.COM\"]\n extraHeader = Authorization: Fixture host-case\n",
            None,
        ),
        (
            "default-port",
            "[http]\n extraHeader = Authorization: Fixture global\n[http \"https://GiTHub.COM:443/alabsystems/aterm.git\"]\n extraHeader = Authorization: Fixture default-port\n",
            None,
        ),
    ] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("home")).unwrap();
        std::fs::create_dir_all(root.join("xdg")).unwrap();
        let config = root.join("gitconfig");
        std::fs::write(
            &config,
            format!("{inherited}[http \"{PRIVATE}\"]\n extraHeader = {PRIVATE_AUTH}\n[http \"{OWNER_BOUNDARY}\"]\n extraHeader = Authorization: Fixture owner-boundary\n"),
        )
        .unwrap();
        let parent = command_config.map_or_else(Vec::new, |(key, value)| {
            vec![
                ("GIT_CONFIG_KEY_0".to_string(), key.to_owned()),
                ("GIT_CONFIG_VALUE_0".to_string(), value.to_owned()),
            ]
        });
        let private_urls = [PRIVATE, PRIVATE_INHERITED, OWNER_BOUNDARY];
        let before: Vec<_> = private_urls
            .iter()
            .map(|url| effective_headers(&root, &config, &parent, &[], url))
            .collect();
        assert!(before[0].contains(PRIVATE_AUTH), "{label}: private control");
        if label != "repo-file" {
            assert!(
                before[1].to_ascii_lowercase().contains("authorization:"),
                "{label}: a private origin inherits the broad header too"
            );
        }
        let public_before = effective_headers(&root, &config, &parent, &[], PUBLIC);
        assert!(
            public_before
                .to_ascii_lowercase()
                .contains("authorization:"),
            "{label}: the inherited setting can authenticate before the policy"
        );

        let count = usize::from(command_config.is_some()).to_string();
        let keys = public_header_keys(config_context(&root, &config, &parent, &[])).unwrap();
        let reads = public_reads(Some(std::ffi::OsStr::new(&count)), &keys, None).unwrap();
        let real = Real::new(&root, Some(&root), true, false).unwrap();
        let stage =
            real.dry_run_command(&root.join("engine"), "c0ffee", &root.join("clone"), &reads);
        let reads: Vec<(String, std::ffi::OsString)> = stage
            .get_envs()
            .filter_map(|(key, value)| {
                let key = key.to_str().filter(|key| key.starts_with("GIT_"))?;
                Some((key.to_owned(), value?.to_os_string()))
            })
            .collect();
        let private: Vec<_> = private_urls
            .iter()
            .map(|url| effective_headers(&root, &config, &parent, &reads, url))
            .collect();
        assert_eq!(private, before, "{label}: private-origin auth is preserved");
        observed.push((
            label,
            effective_headers(&root, &config, &parent, &reads, PUBLIC),
        ));
    }
    assert!(
        observed
            .iter()
            .all(|(_, value)| !value.to_ascii_lowercase().contains("authorization:")),
        "public reads retain inherited HTTP authentication capability: {observed:?}"
    );
}

/// Unproved Git-normalized forms cannot silently bypass the exact-key overrides. These are
/// pure key fixtures; no Git context, config file, credential helper or transport is consulted.
#[test]
fn unproved_public_header_scopes_refuse_without_echoing_the_config() {
    for scope in [
        "https://github.com/%61labsystems/aterm.git",
        "https://github.com/alabsystems/../alabsystems/aterm.git",
        "https://*.com/alabsystems/aterm.git",
        "https://Fixture:QZP6WORD1@github.com/%61labsystems/aterm.git",
    ] {
        let key = format!("http.{scope}.extraheader");
        let why = public_reads(None, std::slice::from_ref(&key), None)
            .expect_err("unproved URL scope refuses");
        let shown = format!("{why:?}");
        assert!(!shown.contains(&key), "the scope is never echoed");
        assert!(!shown.contains("QZP6WORD1"), "userinfo is never echoed");
    }
    assert!(
        public_reads(
            None,
            &["http.https://example.invalid/private%20repo.extraheader".into()],
            None,
        )
        .is_ok()
    );
}

/// Git's inherited `-c` representation is applied after numbered command config. It must
/// preserve private authentication while leaving public headers and helpers refused.
#[test]
fn public_reads_override_inherited_git_parameters_and_preserve_private_authentication() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("home")).unwrap();
    std::fs::create_dir_all(root.join("xdg")).unwrap();
    let config = root.join("gitconfig");
    std::fs::write(&config, "").unwrap();
    let parameters = concat!(
        "'http.https://github.com/alabsystems/aterm.git.extraHeader=Authorization: Fixture public' ",
        "'http.https://fixture'\\''owner@github.com/alabsystems/aterm.git.extraHeader=Authorization: Fixture user' ",
        "'http.https://github.com/alabsystems/private.git.extraHeader=Authorization: Fixture caller'\\''s private' ",
        "'credential.https://github.com/alabsystems.helper=' ",
        "'credential.https://github.com/alabsystems.helper=!f() { echo username=fixture; echo password=parameters-public; }; f' ",
        "'credential.https://github.com/alabsystems.helper=!f() { echo username=fixture; echo password=parameters-private; }; f' ",
        "'url.https://example.invalid/caller/.insteadOf=fixture:'",
    );
    let parent = vec![("GIT_CONFIG_PARAMETERS".into(), parameters.into())];
    let keys = public_header_keys(config_context(&root, &config, &parent, &[])).unwrap();
    let reads = public_reads(None, &keys, Some(std::ffi::OsStr::new(parameters))).unwrap();
    let real = Real::new(&root, Some(&root), true, false).unwrap();
    let stage = real.dry_run_command(&root.join("engine"), "c0ffee", &root.join("clone"), &reads);
    let reads: Vec<(String, std::ffi::OsString)> = stage
        .get_envs()
        .filter_map(|(key, value)| {
            let key = key.to_str().filter(|key| key.starts_with("GIT_"))?;
            Some((key.to_owned(), value?.to_os_string()))
        })
        .collect();
    let fill = |reads: &[(String, std::ffi::OsString)], url: &str| {
        filled_credentials(config_context(&root, &config, &parent, reads), url)
    };
    let user_public = "https://fixture'owner@github.com/alabsystems/aterm.git";
    for url in [PUBLIC, user_public] {
        assert!(
            effective_headers(&root, &config, &parent, &[], url).contains("Authorization:"),
            "the inherited parameters authenticate before the policy"
        );
        assert!(fill(&[], url).status.success());
    }
    let private_before = effective_headers(&root, &config, &parent, &[], PRIVATE);
    assert!(private_before.contains("caller's private"));
    assert_eq!(
        effective_headers(&root, &config, &parent, &reads, PRIVATE),
        private_before,
        "the caller's quoted private header survives"
    );
    let private = fill(&reads, PRIVATE);
    assert!(
        private.status.success()
            && String::from_utf8_lossy(&private.stdout).contains("password=parameters-private"),
        "the caller's private helper survives"
    );
    let mut rewrite = config_context(&root, &config, &parent, &reads);
    rewrite.args(["ls-remote", "--get-url", "fixture:repo.git"]);
    let rewrite = aterm_uds::deadline::output_within(&mut rewrite, Duration::from_secs(60))
        .expect("the scratch rewrite query ends");
    assert!(rewrite.status.success());
    assert_eq!(
        String::from_utf8_lossy(&rewrite.stdout).trim(),
        "https://example.invalid/caller/repo.git",
        "the caller's unrelated transport rewrite survives"
    );
    let mut exposed = Vec::new();
    for url in [PUBLIC, user_public] {
        let header = effective_headers(&root, &config, &parent, &reads, url);
        let helper = fill(&reads, url);
        exposed.push((
            url,
            header.contains("Authorization:"),
            helper.status.success(),
        ));
    }
    assert!(
        exposed.iter().all(|(_, header, helper)| !header && !helper),
        "inherited Git parameters restore public authentication: {exposed:?}"
    );
}

/// A header hidden behind a Git-directory condition in the source can become active in the
/// engine's scratch repository. Discovery must refuse that context-dependent configuration.
#[test]
fn public_header_discovery_refuses_conditional_includes_and_keeps_plain_includes() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    for name in ["home", "xdg", "engine"] {
        std::fs::create_dir_all(root.join(name)).unwrap();
    }
    let config = root.join("gitconfig");
    let included = root.join("ordinary-config");
    let conditional = root.join("conditional-config");
    let ordinary_public = "https://github.com/alabsystems/other.git";
    std::fs::write(
        &included,
        format!(
            "[http \"{PRIVATE}\"]\n extraHeader = {PRIVATE_AUTH}\n\
             [http \"{ordinary_public}\"]\n extraHeader = Authorization: Fixture ordinary\n\
             [credential \"https://github.com/alabsystems\"]\n \
             helper = \"!f() {{ echo username=fixture; echo password=ordinary-private; }}; f\"\n"
        ),
    )
    .unwrap();
    let plain = format!("[include]\n path = {}\n", included.display());
    std::fs::write(&config, &plain).unwrap();
    for cwd in [&root, &root.join("engine")] {
        let mut git = config_context(&root, &config, &[], &[]);
        git.current_dir(cwd).args(["init", "--quiet"]);
        let out = aterm_uds::deadline::output_within(&mut git, Duration::from_secs(60))
            .expect("the scratch repository initializes");
        assert!(out.status.success());
    }
    let keys = public_header_keys(config_context(&root, &config, &[], &[]))
        .expect("ordinary unconditional includes remain supported");
    let reads = public_reads(None, &keys, None).unwrap();
    assert!(
        effective_headers(&root, &config, &[], &[], ordinary_public).contains("Fixture ordinary")
    );
    assert!(
        effective_headers(&root, &config, &[], &reads, ordinary_public)
            .trim()
            .is_empty()
    );
    assert_eq!(
        effective_headers(&root, &config, &[], &reads, PRIVATE),
        effective_headers(&root, &config, &[], &[], PRIVATE),
        "an ordinary include's private header survives"
    );
    let private = filled_credentials(config_context(&root, &config, &[], &reads), PRIVATE);
    assert!(
        private.status.success()
            && String::from_utf8_lossy(&private.stdout).contains("password=ordinary-private"),
        "an ordinary include's private helper survives"
    );
    std::fs::write(
        &conditional,
        format!("[http \"{PUBLIC}\"]\n extraHeader = Authorization: Fixture conditional\n"),
    )
    .unwrap();
    let engine_gitdir = root.join("engine/.git");
    std::fs::write(
        &config,
        format!(
            "{plain}[includeIf \"gitdir:{}\"]\n path = {}\n",
            engine_gitdir.display(),
            conditional.display()
        ),
    )
    .unwrap();
    assert!(
        effective_headers(&root, &config, &[], &[], PUBLIC)
            .trim()
            .is_empty()
    );
    let mut engine = config_context(&root, &config, &[], &reads);
    engine.current_dir(root.join("engine"));
    assert!(
        matched_headers(engine, PUBLIC).contains("Fixture conditional"),
        "a source-only key scan cannot clear the header that the engine will inherit"
    );
    let why = public_header_keys(config_context(&root, &config, &[], &[]))
        .expect_err("a conditional include must refuse before future repository reads");
    let shown = format!("{why:?}");
    assert!(!shown.contains(&engine_gitdir.display().to_string()));
    assert!(!shown.contains("Fixture conditional"));
}

/// The deadline runner retains a bounded tail. A key list that fills it cannot prove that
/// earlier public headers or inactive conditional directives were absent.
#[test]
fn public_header_discovery_refuses_a_key_list_that_fills_the_capture_limit() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    for name in ["home", "xdg"] {
        std::fs::create_dir_all(root.join(name)).unwrap();
    }
    let config = root.join("gitconfig");
    let padding = "p".repeat(aterm_uds::deadline::OUTPUT_KEEP + 4096);
    std::fs::write(
        &config,
        format!(
            "[http \"{PUBLIC}\"]\n extraHeader = Authorization: Fixture earlier-public\n\
             [includeIf \"gitdir:{}/inactive/.git\"]\n path = never-read\n\
             [http \"https://example.invalid/{padding}\"]\n extraHeader = X-Fixture: tail\n",
            root.display()
        ),
    )
    .unwrap();
    assert!(effective_headers(&root, &config, &[], &[], PUBLIC).contains("Fixture earlier-public"));
    let discovered = public_header_keys(config_context(&root, &config, &[], &[]));
    if let Ok(keys) = &discovered {
        assert!(!keys.iter().any(|key| key.starts_with("includeif.")));
        let reads = public_reads(None, keys, None).unwrap();
        assert!(
            effective_headers(&root, &config, &[], &reads, PUBLIC)
                .contains("Fixture earlier-public"),
            "the retained tail cannot supply the earlier public key's exact reset"
        );
    }
    assert!(
        discovered.is_err(),
        "a saturated key-list capture must refuse rather than accept its incomplete tail"
    );
    let shown = format!("{:?}", discovered.unwrap_err());
    assert!(!shown.contains(&root.display().to_string()));
    assert!(!shown.contains("Fixture earlier-public"));
}
