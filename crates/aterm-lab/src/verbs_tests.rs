// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_header_names_the_login_the_lab_launched_with() {
    let token = "exec env CLAUDE_CODE_OAUTH_TOKEN=sk-ant-oat01-LAB claude\n";
    let plain = "exec env claude\n";
    let helper = r#"{"apiKeyHelper": "/lab/key.sh", "env": {}}"#;
    let none = r#"{"env": {}}"#;
    for (launcher, settings, stored, want) in [
        (token, helper, false, ClaudeLogin::OAuthToken),
        (token, none, true, ClaudeLogin::OAuthToken),
        (plain, helper, true, ClaudeLogin::ApiKeyHelper),
        (plain, none, true, ClaudeLogin::StoredCredentials),
        (plain, none, false, ClaudeLogin::NoLogin),
        (plain, "", false, ClaudeLogin::NoLogin),
        (plain, "{", true, ClaudeLogin::StoredCredentials),
        (
            plain,
            r#"{"apiKeyHelper": null}"#,
            false,
            ClaudeLogin::NoLogin,
        ),
        (
            plain,
            r#"{"apiKeyHelper": 7}"#,
            true,
            ClaudeLogin::StoredCredentials,
        ),
    ] {
        assert_eq!(
            claude_login(launcher, settings, stored),
            want,
            "{launcher} {settings} {stored}"
        );
    }
    let all = [
        ClaudeLogin::OAuthToken,
        ClaudeLogin::ApiKeyHelper,
        ClaudeLogin::StoredCredentials,
        ClaudeLogin::NoLogin,
    ];
    let phrases: std::collections::BTreeSet<&str> =
        all.iter().map(|login| login.phrase()).collect();
    assert_eq!(phrases.len(), all.len(), "each login a header tells apart");
}

/// Two tabs as a lab's `ls` lists them (measured 2026-10-25), the second one active.
const LS: &str = "0 0 s-514051f1c3c7e2adf6ba - alive /private/tmp/at.GQvN/proj meta=0 window=0 active=0 wfocus=1 program=zsh\n\
                  0 1 s-0f4a5235f1fe789003c4 s-514051f1c3c7e2adf6ba alive /private/tmp meta=0 window=0 active=1 wfocus=1 program=zsh\n";

#[test]
fn a_verb_names_its_tab_by_sid_and_reads_the_active_one_otherwise() {
    let words = |w: &[&str]| w.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    let mut a = words(&["@s-0f4a5235f1fe789003c4", "name", "what"]);
    assert_eq!(
        take_tab(&mut a).unwrap().as_deref(),
        Some("s-0f4a5235f1fe789003c4")
    );
    assert_eq!(a, words(&["name", "what"]), "the tab's word is taken out");
    let mut a = words(&["name", "what"]);
    assert_eq!(take_tab(&mut a).unwrap(), None, "no tab named");
    assert_eq!(a.len(), 2);
    for bad in [&["@"][..], &["@a b"], &["@../x", "name"]] {
        assert!(take_tab(&mut words(bad)).is_err(), "{bad:?}");
    }
    for later in [
        &["mention", "@ file picker"][..],
        &["mention", "@mentions"],
        &["@s-0f4a5235f1fe789003c4", "mention", "@mentions"],
    ] {
        let mut a = words(later);
        let took = take_tab(&mut a).unwrap();
        assert_eq!(
            took.is_some(),
            later[0].starts_with('@'),
            "only the first word names a tab: {later:?}"
        );
        assert_eq!(
            a.last().map(String::as_str),
            later.last().copied(),
            "a later `@` word is the verb's own: {later:?}"
        );
    }
    assert!(ls_lists(LS, "s-0f4a5235f1fe789003c4"));
    assert!(ls_lists(LS, "s-514051f1c3c7e2adf6ba"));
    assert!(
        !ls_lists(LS, "s-0f4a"),
        "a prefix is no tab, nor is a parent column"
    );
    assert_eq!(active_in(LS).as_deref(), Some("s-0f4a5235f1fe789003c4"));
    assert_eq!(
        active_in(&LS.replace("active=1", "active=0")),
        None,
        "none active"
    );
}

/// The lab's stub-route launcher ([`crate::lab::launcher`]) and Claude Code's default
/// route's (`claude-login-proof`), as each writes them.
const STUB: &str = "#!/bin/sh\nclear\nexec env PATH=\"/r/shim:$PATH\" CLAUDE_CONFIG_DIR=/r/scratch/home/.claude \
                    ANTHROPIC_BASE_URL=http://127.0.0.1:57044 \\\n  HTTP_PROXY=http://127.0.0.1:57045 \\\n  /c/claude\n";
const DEFAULT: &str = "#!/bin/sh\nclear\nexec env -u CLAUDE_CONFIG_DIR -u ANTHROPIC_BASE_URL -u HTTP_PROXY \\\n  \
                       PATH=\"/r/shim:$PATH\" CLAUDE_SECURESTORAGE_CONFIG_DIR='/r/scratch/state/a b/lab-b' \
                       DISABLE_AUTOUPDATER=1 \\\n  '/c/claude'\n";
const DEAD: &str =
    r#"{"claudeAiOauth":{"accessToken":"","refreshToken":"","expiresAt":0,"scopes":[]}}"#;

#[test]
fn a_header_names_the_route_the_tab_was_launched_on() {
    assert_eq!(claude_route(STUB), ClaudeRoute::Stub);
    assert_eq!(claude_route(DEFAULT), ClaudeRoute::Default);
    assert_eq!(claude_route(""), ClaudeRoute::Stub, "nothing launched");
    assert_eq!(
        launcher_value(DEFAULT, "CLAUDE_SECURESTORAGE_CONFIG_DIR").as_deref(),
        Some("/r/scratch/state/a b/lab-b"),
        "a quoted value whole"
    );
    assert_eq!(
        launcher_value(STUB, "CLAUDE_CONFIG_DIR").as_deref(),
        Some("/r/scratch/home/.claude")
    );
    assert_eq!(
        launcher_value(DEFAULT, "CLAUDE_CONFIG_DIR"),
        None,
        "`-u CLAUDE_CONFIG_DIR` gives it no value"
    );
    assert!(stored_login_dead(DEAD));
    assert!(!stored_login_dead(
        r#"{"claudeAiOauth":{"accessToken":"sk-ant-oat01-x","refreshToken":"r","expiresAt":1999999999999}}"#
    ));
    assert!(!stored_login_dead("{}"), "no stored login at all");
    let none = r#"{"env": {}}"#;
    let live = r#"{"claudeAiOauth":{"accessToken":"t","expiresAt":1999999999999}}"#;
    let home = DEFAULT.replace(
        " CLAUDE_SECURESTORAGE_CONFIG_DIR='/r/scratch/state/a b/lab-b'",
        "",
    );
    let of = |route, login, stored| Launched {
        route,
        login,
        stored,
        config_dir: route == ClaudeRoute::Stub,
    };
    // `lab claude --own`'s: the stub's route with no `CLAUDE_CONFIG_DIR` and no proxy.
    let own = STUB
        .replace(" CLAUDE_CONFIG_DIR=/r/scratch/home/.claude", "")
        .replace("\\\n  HTTP_PROXY=http://127.0.0.1:57045 ", "")
        .replace("exec env ", "exec env -u HTTP_PROXY ");
    let (stub, default, stored) = (
        ClaudeRoute::Stub,
        ClaudeRoute::Default,
        ClaudeLogin::StoredCredentials,
    );
    let cases = [
        ("the stub's route", STUB, Some(DEAD), of(stub, stored, None)),
        (
            "the stub's route in the scratch HOME's own `.claude` (`--own`)",
            own.as_str(),
            Some(DEAD),
            Launched {
                config_dir: false,
                ..of(stub, stored, None)
            },
        ),
        (
            "the default route, a dead login in a named dir",
            DEFAULT,
            Some(DEAD),
            of(default, stored, Some((LoginDir::SecureStorage, true))),
        ),
        (
            "the default route, a live login",
            DEFAULT,
            Some(live),
            of(default, stored, Some((LoginDir::SecureStorage, false))),
        ),
        (
            "the default route, the scratch HOME's own `.claude`",
            home.as_str(),
            Some(DEAD),
            of(default, stored, Some((LoginDir::ScratchClaude, true))),
        ),
        (
            "the default route, no stored login",
            DEFAULT,
            None,
            of(default, ClaudeLogin::NoLogin, None),
        ),
    ];
    let mut sentences = std::collections::BTreeSet::new();
    for (case, launcher, creds, want) in cases {
        let got = launched(launcher, none, creds);
        assert_eq!(got, want, "{case}");
        let sentence = claude_launch(launcher, none, creds);
        assert_eq!(
            sentence,
            got.sentence(),
            "{case}: the header's is the decision's"
        );
        for key in ["CLAUDE_CONFIG_DIR", "ANTHROPIC_BASE_URL"] {
            assert!(sentence.contains(key), "{case}: names {key}");
        }
        assert_eq!(
            sentence.contains(aterm_agent::harness::accounts::LOGIN_ENV),
            got.stored
                .is_some_and(|(dir, _)| dir == LoginDir::SecureStorage),
            "{case}: names the login dir's variable where the launch set it"
        );
        sentences.insert(sentence);
    }
    assert_eq!(
        sentences.len(),
        cases.len(),
        "each launch a header tells apart"
    );
}

#[test]
fn the_capture_pii_guard_looks_for_the_account_home_and_a_scratch_home() {
    let top = aterm_tempfile::Builder::new()
        .prefix("lab-pii-")
        .tempdir()
        .unwrap();
    let real = fs::canonicalize(top.path()).unwrap();
    let (account, scratch) = (real.join("account"), real.join("scratch"));
    for d in [&account, &scratch] {
        fs::create_dir_all(d).unwrap();
    }
    let (a, s) = (account.display().to_string(), scratch.display().to_string());
    let mut p = crate::paths::Paths::resolve_with(Some(account.clone()), Some(scratch.into()));
    p.user = "lab-owner-name".into();
    let host = "lab-host-name";
    assert_eq!(
        pii_found(&p, host, &format!("cwd {a}/x")),
        std::slice::from_ref(&a)
    );
    assert_eq!(
        pii_found(&p, host, &format!("cwd {s}/x")),
        std::slice::from_ref(&s)
    );
    assert_eq!(
        pii_found(&p, host, "lab-owner-name@lab-host-name"),
        ["lab-owner-name", "lab-host-name"]
    );
    assert!(pii_found(&p, host, "cwd ~/x").is_empty());
    let mut own = crate::paths::Paths::resolve_with(Some(account.clone()), Some(account.into()));
    own.user = "lab-owner-name".into();
    assert!(pii_found(&own, host, &format!("cwd {s}/x")).is_empty());
}

#[test]
fn survey_takes_on_or_off_and_reread_after_it() {
    let w = |s: &str| -> Vec<String> { s.split_whitespace().map(str::to_string).collect() };
    for (words, want) in [
        ("on", (Some("1"), false)),
        ("off", (None, false)),
        ("on --reread", (Some("1"), true)),
        ("off --reread", (None, true)),
    ] {
        assert_eq!(survey_words(&w(words)).ok(), Some(want), "{words}");
    }
    for bad in [
        "",
        "yes",
        "--reread",
        "--reread on",
        "on --reread x",
        "on off",
    ] {
        let e = survey_words(&w(bad)).expect_err(bad);
        assert_eq!(e.msg.as_deref(), Some("survey: on|off [--reread]"), "{bad}");
    }
}

#[test]
fn harness_keys_are_set_in_their_table_and_replaced_when_there() {
    let base = "[update]\nenabled = false\n[harness]\nprobe_api = false\n\n[render]\nx = 1\n";
    let kv = |k: &str, v: &str| vec![(k.to_string(), v.to_string())];
    let t = harness_set(base, &kv("continue", "false"));
    assert_eq!(
        t,
        "[update]\nenabled = false\n[harness]\nprobe_api = false\ncontinue = false\n\n[render]\nx = 1\n"
    );
    let t = harness_set(&t, &kv("probe_api", "true"));
    assert!(
        t.contains("[harness]\nprobe_api = true\ncontinue = false\n"),
        "{t}"
    );
    assert_eq!(
        harness_set("a = 1\n", &kv("k", "\"v\"")),
        "a = 1\n\n[harness]\nk = \"v\"\n"
    );
}

#[test]
fn a_harness_value_is_written_as_toml() {
    let parses = |t: &str| aterm_toml::from_str::<aterm_toml::Value>(t).is_ok();
    let base = "[update]\nenabled = false\n";
    let v = toml_value("none").unwrap();
    assert_eq!(v, "\"none\"");
    let t = harness_set(base, &[("approve".to_string(), v)]);
    assert!(t.ends_with("[harness]\napprove = \"none\"\n"), "{t}");
    assert!(parses(&t), "{t}");
    let raw = harness_set(base, &[("approve".to_string(), "none".to_string())]);
    assert!(!parses(&raw), "the unquoted word is no TOML: {raw}");
    for typed in ["false", "3", "1.5", "\"safe\"", "'safe'", "[\"a\", \"b\"]"] {
        assert_eq!(toml_value(typed).as_deref(), Ok(typed), "{typed}");
    }
    assert_eq!(toml_value("a\"b\\c").unwrap(), r#""a\"b\\c""#);
    assert!(parses(&format!("k = {}\n", toml_value("a\"b\\c").unwrap())));
    for bad in ["[1,", "\"open", "{a = }"] {
        assert!(toml_value(bad).is_err(), "{bad}");
    }
    let two = toml_value("true\n[update]\nenabled = true").unwrap();
    assert_eq!(two, r#""true\u000A[update]\u000Aenabled = true""#);
    let t = harness_set(base, &[("approve".to_string(), two)]);
    assert!(parses(&t), "{t}");
    assert!(t.contains("[update]\nenabled = false\n"), "{t}");
    assert!(!t.contains("\nenabled = true"), "{t}");
}

#[test]
fn a_journal_record_prints_as_one_aligned_line() {
    let l = journal_line(
        r#"{"t":1792091823464,"kind":"dismissed","phase":"survey","line":"DISMISSED survey seq=1460"}"#,
    );
    assert!(
        l.ends_with(".464 dismissed  survey       DISMISSED survey seq=1460"),
        "{l}"
    );
    assert_eq!(journal_line("not json"), "not json");
}

#[test]
fn headers_takes_the_on200_and_on429_blocks_alone() {
    assert_eq!(rate_blocks(None).unwrap().py(), "{}");
    assert_eq!(
        rate_blocks(Some(r#"{"on200":{"a":"1"},"on429":{}}"#))
            .unwrap()
            .py(),
        r#"{"on200": {"a": "1"}, "on429": {}}"#
    );
    for bad in [r#"{"x-codex-a":"1"}"#, r#"{"on200":"1"}"#, "[1]", "nope"] {
        assert!(rate_blocks(Some(bad)).is_err(), "{bad}");
    }
}

#[test]
fn both_headers_verbs_writes_stand_in_the_one_file() {
    let d = crate::TestScratch::new("headers");
    let p = d.join("headers.json");
    let read = || {
        J::parse(&std::fs::read_to_string(&p).unwrap())
            .unwrap()
            .py()
    };
    let flat = r#"{"x-codex-primary-used-percent":"76.0"}"#;
    write_headers(&d, J::parse(r#"{"on429":{"a":"1"}}"#).unwrap(), true).unwrap();
    write_headers(&d, J::parse(flat).unwrap(), false).unwrap();
    let both = read();
    atomic_write(&p, flat.as_bytes()).unwrap();
    let over = read();
    assert_eq!(
        both,
        r#"{"on429": {"a": "1"}, "x-codex-primary-used-percent": "76.0"}"#
    );
    assert_eq!(over, r#"{"x-codex-primary-used-percent": "76.0"}"#);
}

#[test]
fn a_custom_mode_is_named_what_the_stub_serves_from_its_file() {
    assert_eq!(custom_mode_name("fable-noreset").unwrap(), "fable-noreset");
    assert_eq!(custom_mode_name("v1..v2").unwrap(), "v1..v2");
    let refused = |m: &str| custom_mode_name(m).unwrap_err().msg.unwrap_or_default();
    for m in ["ok", "limit", "tool", "bg", "slow", "rate-limit"] {
        assert!(refused(m).contains("is a built-in mode"), "{m}");
    }
    assert!(refused("refresh").contains("token refresh"));
    for m in [
        "",
        ".",
        "..",
        "a b",
        "custom:x",
        "limit:five_hour:1",
        "x/../y",
    ] {
        assert!(refused(m).contains("letters, digits"), "{m}");
    }
    let pids = ["4242".to_string()];
    assert_eq!(
        refusal_of(
            "2026-10-29 19:45:00.123-0700 0x1 Error 0 0 kernel: Sandbox: Antigravity Helper (Plugin)(4242) deny(1) file-read-data /x",
            &pids,
        )
        .as_deref(),
        Some("19:45:00.123 Antigravity Helper (Plugin)(4242) deny(1) file-read-data /x")
    );
    assert_eq!(
        refusal_of(
            "2026-10-29 19:45:00.123-0700 0x1 Error 0 0 kernel: Sandbox: foo(bar)(4242) deny(1) mach-lookup x",
            &pids,
        )
        .as_deref(),
        Some("19:45:00.123 foo(bar)(4242) deny(1) mach-lookup x")
    );
    assert!(
        refusal_of(
            "2026-10-29 19:45:00.123-0700 0x1 Error 0 0 kernel: Sandbox: 2 duplicate reports for node(4242) deny(1) file-read-data /x",
            &pids,
        )
        .is_none()
    );
}

#[test]
fn versions_order_by_their_numbers() {
    assert!(version_key("2.1.300") > version_key("2.1.299"));
    assert!(version_key("2.1.300-rc.1") > version_key("2.1.299"));
    assert_eq!(version_key("x.99.0"), Vec::<u64>::new());
    assert!(version_key("99.0.0") > version_key("2.1.300"));
    assert!(
        semver_like("2.1.999")
            && semver_like("2.1.300-rc.1")
            && semver_like("2.1.300+build.5")
            && !semver_like("2.1.3x")
            && !semver_like("2.1.3-")
            && !semver_like("2.1.3+")
            && !semver_like("2.1a.3")
            && !semver_like("x.1.2")
            && !semver_like("2.1")
    );
}

/// The live upgrade starts a newer build once before moving a session onto it, through a probe
/// that pins the twin's launch, and pins only a twin atpkg would have laid over a build that is
/// an image to it. A twin it refuses is a build that never starts, so nothing moves (the lab's
/// hand-written twin did that from fd3b937ea1 on). The two refusals are the controls.
#[test]
fn a_start_probe_pins_the_twin_the_lab_lays_over_its_build() {
    use std::os::unix::fs::PermissionsExt as _;
    let top = aterm_tempfile::Builder::new()
        .prefix("lab-twin-")
        .tempdir()
        .unwrap();
    let pkg = fs::canonicalize(top.path()).unwrap().join("pkg");
    let (twin, build) = (
        pkg.join("agents/claude"),
        pkg.join("store/claude/lab-1/bin/claude"),
    );
    let lay = |path: &Path, text: &str| {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    };
    let forwarder = atpkg::platform::ProbeForwarder {
        executable: std::env::current_exe().unwrap(),
        parent_pid: std::process::id(),
    };
    let pinned = |text: &str| atpkg::platform::probe_shim(&twin, text, &forwarder).is_some();
    let laid = String::from_utf8(atpkg_twin(&pkg, &twin, &build).unwrap()).unwrap();
    lay(&twin, &laid);
    lay(&build, &crashing_build("99.0.0"));
    assert!(pinned(&laid), "the twin the lab lays: {laid}");
    let hand = format!(
        "#!/bin/sh\n# atpkg shim — exec so the tool authenticates at its real path.\n# aterm-lab twin: a newer managed build\nexport DISABLE_AUTOUPDATER='1'\nexec '{}' \"$@\"\n",
        build.display()
    );
    assert!(!pinned(&hand), "a twin atpkg would not lay");
    lay(&build, "#!/bin/sh\necho '99.0.0 (Claude Code)'\n");
    assert!(!pinned(&laid), "a build the probe reads as another shim");
}
