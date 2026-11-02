// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::time::{Duration, Instant};

use aterm_phase::Program;

use super::*;

fn words(s: &str) -> Vec<String> {
    s.split_whitespace().map(str::to_string).collect()
}

fn stub(port: u16) -> Target {
    Target {
        host: "127.0.0.1".to_string(),
        port,
        tls: false,
    }
}

/// The stub's base URL at its root.
fn base(port: u16) -> Base {
    Base {
        target: stub(port),
        path: String::new(),
    }
}

/// The lab's agy key (`GEMINI_API_KEY`), which makes `GOOGLE_GEMINI_BASE_URL` its route.
const KEYED: &str = "GEMINI_API_KEY=lab-key";

/// The lab's launches as their scripts write them: a proxy for everything but loopback, the stub's
/// base URL (agy's variable, Muse Code's option).
fn lab_env(extra: &[&str]) -> Vec<String> {
    [
        "PATH=/usr/bin",
        "HTTPS_PROXY=http://127.0.0.1:9",
        "https_proxy=http://127.0.0.1:9",
        "HTTP_PROXY=http://127.0.0.1:9",
        "ALL_PROXY=http://127.0.0.1:9",
        "NO_PROXY=127.0.0.1,localhost",
    ]
    .iter()
    .chain(extra)
    .map(|s| (*s).to_string())
    .collect()
}

fn unread_claude() -> super::super::netwatch::Route {
    panic!("the Claude Code route is asked only for Claude Code")
}

#[test]
fn a_base_url_names_its_endpoint() {
    for (url, want) in [
        (
            "https://api.meta.ai/v1",
            Some(Target {
                host: "api.meta.ai".into(),
                port: 443,
                tls: true,
            }),
        ),
        ("http://127.0.0.1:61234", Some(stub(61234))),
        (
            "HTTP://LocalHost:8080/v1/",
            Some(Target {
                host: "localhost".into(),
                port: 8080,
                tls: false,
            }),
        ),
        (
            "http://[::1]:7000/x",
            Some(Target {
                host: "::1".into(),
                port: 7000,
                tls: false,
            }),
        ),
        ("https://user:pw@gw.example.test", None),
        ("ftp://example.test", None),
        ("https://", None),
        ("http://h:0", None),
        ("http://h:99999", None),
    ] {
        assert_eq!(Target::of_url(url), want, "{url}");
    }
    assert_eq!(Target::anthropic().says(), "https://api.anthropic.com:443");
    assert_eq!(stub(5).says(), "http://127.0.0.1:5");
}

/// A supported base URL's route keeps its path (no trailing `/`), so two gateway paths on
/// one host are two routes; the root's path is empty however it is written. NEGATIVE CONTROL
/// (run): with `Base::of_url` keeping no path, the two gateway paths read as one route and the
/// test fails.
#[test]
fn a_base_url_keeps_its_path_as_part_of_its_route() {
    let gw = |path: &str| Base {
        target: Target {
            host: "gw.example.test".into(),
            port: 443,
            tls: true,
        },
        path: path.to_string(),
    };
    for (url, want) in [
        ("https://gw.example.test/v1/openai", Some(gw("/v1/openai"))),
        ("https://gw.example.test/v1/openai/", Some(gw("/v1/openai"))),
        (
            "https://gw.example.test/v1/anthropic",
            Some(gw("/v1/anthropic")),
        ),
        ("https://gw.example.test", Some(gw(""))),
        ("https://gw.example.test/", Some(gw(""))),
        ("https://gw.example.test?route=/v1", None),
        ("ftp://gw.example.test/v1", None),
    ] {
        assert_eq!(Base::of_url(url), want, "{url}");
    }
    let env = lab_env(&[]);
    let route = |url: &str| {
        RouteKey::of(
            Program::Muse,
            &endpoint_of(
                Program::Muse,
                &words(&format!("muse --base-url {url}")),
                Some(&env),
                unread_claude,
            ),
        )
    };
    let openai = route("http://127.0.0.1:4100/v1/openai");
    let anthropic = route("http://127.0.0.1:4100/v1/anthropic");
    assert!(openai.is_some() && anthropic.is_some());
    assert_ne!(openai, anthropic, "two paths on one host are two routes");
    assert_eq!(
        openai.as_ref().map(|k| &k.base.target),
        anthropic.as_ref().map(|k| &k.base.target),
        "one endpoint for the probe"
    );
    let mut book = Witnesses::default();
    let t = Instant::now();
    book.answered(anthropic.clone().unwrap(), "s-anthropic", t);
    assert_eq!(
        book.witness(openai.as_ref().unwrap(), "s-openai"),
        None,
        "an answer on one gateway path witnesses nothing on another"
    );
    assert_eq!(
        route("http://127.0.0.1:4100/v1/openai/"),
        openai,
        "a trailing slash names the same route"
    );
    assert_eq!(
        gw("/v1/openai").says(),
        "https://gw.example.test:443/<custom-path>"
    );
}

/// Unsupported URL fields cannot collapse distinct routes or print credentials. A private
/// path remains part of the key while its log display reveals no opaque path bytes.
#[test]
fn unsupported_route_fields_refuse_and_path_credentials_are_not_displayed() {
    let secret = "endpoint-SECRET";
    let env = lab_env(&[]);
    for url in [
        format!("https://{secret}@gw.example.test/v1"),
        format!("https://gw.example.test/v1?route={secret}"),
        format!("https://gw.example.test/v1#{secret}"),
        "http://[::1]extra/v1".to_string(),
        "http://unbracketed::1/v1".to_string(),
        "https://gw.example.test/\r\nsecret".to_string(),
        format!("https://gw.example.test/{}", "x".repeat(MAX_BASE_URL_BYTES)),
    ] {
        assert_eq!(Target::of_url(&url), None);
        assert_eq!(Base::of_url(&url), None);
        let endpoint = endpoint_of(
            Program::Muse,
            &["muse".to_string(), "--base-url".to_string(), url],
            Some(&env),
            unread_claude,
        );
        assert!(matches!(endpoint, Endpoint::Gap(_)));
        assert!(!format!("{endpoint:?}").contains(secret));
    }
    let first = Base::of_url(&format!("https://gw.example.test/{secret}/one")).unwrap();
    let second = Base::of_url(&format!("https://gw.example.test/{secret}/two")).unwrap();
    assert_ne!(first, second, "different paths cannot share witnesses");
    assert_eq!(
        first.says(),
        second.says(),
        "display is not the authority key"
    );
    assert!(!first.says().contains(secret));
    assert!(
        !format!("{first:?}").contains(secret),
        "Debug is also redacted"
    );
}

/// A day with arbitrarily many sessions still has a fixed-size book. Capacity drops only new
/// evidence; existing witnesses update, and natural expiry admits new evidence again.
#[test]
fn the_witness_book_is_bounded_without_replacing_live_evidence() {
    let route = RouteKey {
        program: Program::Muse,
        base: base(4100),
    };
    let now = Instant::now();
    let mut book = Witnesses::default();
    for n in 0..MAX_WITNESSES {
        book.answered(route.clone(), &format!("s-{n}"), now);
    }
    book.answered(route.clone(), "s-overflow", now);
    assert_eq!(book.last.len(), MAX_WITNESSES);
    assert!(
        !book
            .last
            .contains_key(&(route.clone(), "s-overflow".to_string()))
    );
    book.answered(route.clone(), "s-0", now + Duration::from_secs(2));
    assert_eq!(
        book.witness(&route, "s-reader"),
        Some(now + Duration::from_secs(2))
    );
    book.answered(route.clone(), "s-overflow", now + WITNESS_KEPT);
    assert_eq!(book.last.len(), 2, "expiry frees only expired records");
    assert!(book.last.contains_key(&(route, "s-overflow".to_string())));
}

/// Each program's endpoint is read from its own launch: Muse Code's `--base-url` (either form, the
/// last before `--`), agy's `GOOGLE_GEMINI_BASE_URL`, both through the lab's proxy, which spares
/// loopback; Claude Code's from its route.
#[test]
fn each_programs_endpoint_is_read_from_its_own_launch() {
    let env = lab_env(&["GOOGLE_GEMINI_BASE_URL=http://127.0.0.1:4100", KEYED]);
    assert_eq!(
        endpoint_of(
            Program::Muse,
            &words("muse --base-url http://127.0.0.1:4100 --trust-workspace"),
            Some(&env),
            unread_claude,
        ),
        Endpoint::Named(base(4100))
    );
    assert_eq!(
        endpoint_of(
            Program::Muse,
            &words(
                "muse --base-url=http://127.0.0.1:1 -c x=1 --base-url=http://127.0.0.1:4101 -- --base-url http://127.0.0.1:9"
            ),
            Some(&env),
            unread_claude,
        ),
        Endpoint::Named(base(4101))
    );
    assert_eq!(
        endpoint_of(Program::Agy, &words("agy"), Some(&env), unread_claude),
        Endpoint::Named(base(4100))
    );
    let google_key = lab_env(&[
        "GOOGLE_GEMINI_BASE_URL=http://127.0.0.1:4100",
        "GOOGLE_API_KEY=lab-key",
        "GOOGLE_GENAI_USE_VERTEXAI=false",
    ]);
    assert_eq!(
        endpoint_of(
            Program::Agy,
            &words("agy"),
            Some(&google_key),
            unread_claude
        ),
        Endpoint::Named(base(4100)),
        "either key variable; Vertex turned off"
    );
    assert_eq!(
        endpoint_of(
            Program::Agy,
            &words("agy"),
            Some(&[KEYED.to_string()]),
            unread_claude,
        ),
        Endpoint::Named(Base::gemini()),
        "keyed agy on modelProvider gemini with no GOOGLE_GEMINI_BASE_URL reaches generativelanguage.googleapis.com"
    );
    assert_eq!(
        endpoint_of(
            Program::Agy,
            &words("agy"),
            Some(&[
                KEYED.to_string(),
                "GOOGLE_GENAI_USE_VERTEXAI=true".to_string(),
                "GOOGLE_GENAI_USE_ENTERPRISE=false".to_string(),
            ]),
            unread_claude,
        ),
        Endpoint::Named(Base::gemini()),
        "GOOGLE_GENAI_USE_ENTERPRISE=false overrides GOOGLE_GENAI_USE_VERTEXAI=true"
    );
    assert_eq!(
        endpoint_of(Program::Claude, &words("claude"), None, || {
            super::super::netwatch::Route::Default
        }),
        Endpoint::Named(Base::anthropic())
    );
    // Claude Code on a route of its own: the ANTHROPIC_BASE_URL its launch's route variables name.
    let own = lab_env(&["ANTHROPIC_BASE_URL=http://127.0.0.1:4100"]);
    assert_eq!(
        endpoint_of(Program::Claude, &words("claude"), Some(&own), || {
            super::super::netwatch::Route::Custom("ANTHROPIC_BASE_URL in settings.json".into())
        }),
        Endpoint::Named(base(4100))
    );
}

/// A probe that cannot name the route is a named gap, never a guessed default: no `--base-url`, no
/// `GOOGLE_GEMINI_BASE_URL`, an environment that could not be read, a proxy that carries the
/// requests, Claude Code on a custom route, and a program whose route is not read.
#[test]
fn a_route_no_launch_names_is_a_named_gap() {
    let gap = |e: Endpoint, says: &str| match e {
        Endpoint::Gap(why) => assert!(why.contains(says), "{why}"),
        other => panic!("{other:?}, not a gap naming {says}"),
    };
    let env = lab_env(&[]);
    gap(
        endpoint_of(
            Program::Muse,
            &words("muse --trust-workspace"),
            Some(&env),
            unread_claude,
        ),
        "names no --base-url",
    );
    gap(
        endpoint_of(Program::Agy, &words("agy"), Some(&env), unread_claude),
        "names no Gemini API key",
    );
    gap(
        endpoint_of(
            Program::Agy,
            &words("agy"),
            Some(&lab_env(&[KEYED])),
            unread_claude,
        ),
        "HTTPS_PROXY carries agy's Gemini API endpoint",
    );
    gap(
        endpoint_of(Program::Agy, &words("agy"), None, unread_claude),
        "could not be read",
    );
    // agy's base URL is its route on a Gemini API key alone: signed in with Google (no key) or
    // turned to Vertex AI, it calls a route the variable does not name. NEGATIVE CONTROL (run):
    // with `endpoint_of` naming agy's base URL whatever its backend, both read as named.
    gap(
        endpoint_of(
            Program::Agy,
            &words("agy"),
            Some(&lab_env(&["GOOGLE_GEMINI_BASE_URL=http://127.0.0.1:4100"])),
            unread_claude,
        ),
        "names no Gemini API key",
    );
    gap(
        endpoint_of(
            Program::Agy,
            &words("agy"),
            Some(&lab_env(&[
                "GOOGLE_GEMINI_BASE_URL=http://127.0.0.1:4100",
                KEYED,
                "GOOGLE_GENAI_USE_VERTEXAI=True",
            ])),
            unread_claude,
        ),
        "Vertex AI endpoint is not read",
    );
    gap(
        endpoint_of(
            Program::Agy,
            &words("agy"),
            Some(&lab_env(&[
                "GOOGLE_GEMINI_BASE_URL=http://127.0.0.1:4100",
                KEYED,
                "GOOGLE_GENAI_USE_VERTEXAI=false",
                "GOOGLE_GENAI_USE_ENTERPRISE=1",
            ])),
            unread_claude,
        ),
        "GOOGLE_GENAI_USE_ENTERPRISE: its Vertex AI endpoint is not read",
    );
    gap(
        endpoint_of(
            Program::Agy,
            &words("agy"),
            Some(&lab_env(&[
                "GOOGLE_GEMINI_BASE_URL=http://127.0.0.1:4100",
                KEYED,
                "GOOGLE_GENAI_USE_VERTEXAI=maybe",
            ])),
            unread_claude,
        ),
        "non-boolean",
    );
    let proxied = lab_env(&["GOOGLE_GEMINI_BASE_URL=https://gw.example.test/v1", KEYED]);
    gap(
        endpoint_of(Program::Agy, &words("agy"), Some(&proxied), unread_claude),
        "HTTPS_PROXY carries GOOGLE_GEMINI_BASE_URL",
    );
    gap(
        endpoint_of(
            Program::Muse,
            &words("muse --base-url ftp://x"),
            Some(&env),
            unread_claude,
        ),
        "not a supported http(s) base URL",
    );
    gap(
        endpoint_of(Program::Claude, &words("claude"), None, || {
            super::super::netwatch::Route::Custom(
                "ANTHROPIC_BASE_URL in the agent's environment".into(),
            )
        }),
        "ANTHROPIC_BASE_URL",
    );
    let custom =
        || super::super::netwatch::Route::Custom("ANTHROPIC_BASE_URL in settings.json".into());
    for (extra, says) in [
        (
            &[
                "ANTHROPIC_BASE_URL=http://127.0.0.1:4100",
                "CLAUDE_CODE_USE_BEDROCK=1",
            ][..],
            "ANTHROPIC_BASE_URL in settings.json",
        ),
        (
            &[
                "ANTHROPIC_BASE_URL=http://127.0.0.1:4100",
                "ANTHROPIC_UNIX_SOCKET=/tmp/s",
            ][..],
            "ANTHROPIC_BASE_URL in settings.json",
        ),
        (
            &["ANTHROPIC_BASE_URL=https://gw.example.test"][..],
            "HTTPS_PROXY carries ANTHROPIC_BASE_URL",
        ),
        (&[][..], "ANTHROPIC_BASE_URL in settings.json"),
    ] {
        gap(
            endpoint_of(
                Program::Claude,
                &words("claude"),
                Some(&lab_env(extra)),
                custom,
            ),
            says,
        );
    }
    for p in [
        Program::Codex,
        Program::Grok,
        Program::Kimi,
        Program::Generic,
    ] {
        gap(
            endpoint_of(p, &words("x"), Some(&env), unread_claude),
            "route is not read",
        );
    }
}

#[test]
fn no_proxy_spares_the_hosts_it_names() {
    let t = Target {
        host: "api.example.test".into(),
        port: 443,
        tls: true,
    };
    for (list, spared) in [
        ("*", true),
        ("example.test", true),
        (".example.test", true),
        ("*.example.test", true),
        ("api.example.test:443", true),
        ("api.example.test:8443", false),
        ("other.test, localhost", false),
        ("xample.test", false),
        ("", false),
    ] {
        assert_eq!(no_proxy_names(list, &t), spared, "{list:?}");
    }
}

/// A witness is the newest answer of ANOTHER session on the SAME route: the session's own, another
/// program's on the same endpoint, and the same program's on another endpoint are none.
#[test]
fn a_witness_is_another_sessions_answer_on_the_same_route() {
    let t0 = Instant::now();
    let at = |s| t0 + Duration::from_secs(s);
    let muse = RouteKey {
        program: Program::Muse,
        base: base(4100),
    };
    let agy = RouteKey {
        program: Program::Agy,
        base: base(4100),
    };
    let elsewhere = RouteKey {
        program: Program::Muse,
        base: base(4200),
    };
    let mut book = Witnesses::default();
    book.answered(muse.clone(), "s-me", at(50));
    book.answered(agy, "s-agy", at(60));
    book.answered(elsewhere, "s-far", at(70));
    assert_eq!(book.witness(&muse, "s-me"), None, "only others' answers");
    book.answered(muse.clone(), "s-mate", at(40));
    book.answered(muse.clone(), "s-mate", at(30));
    assert_eq!(book.witness(&muse, "s-me"), Some(at(40)), "the newest kept");
    book.answered(muse.clone(), "s-other", at(45));
    assert_eq!(book.witness(&muse, "s-me"), Some(at(45)));
    assert_eq!(book.witness(&muse, "s-mate"), Some(at(50)));
    book.answered(muse.clone(), "s-late", at(45) + WITNESS_KEPT);
    assert_eq!(
        book.witness(&muse, "s-late"),
        Some(at(50)),
        "an answer older than the book keeps is dropped"
    );
    assert_eq!(
        book.last.len(),
        4,
        "each write drops every answer a day old: s-me, s-agy, s-far, s-late are kept"
    );
    assert_eq!(
        RouteKey::of(Program::Muse, &Endpoint::Gap("x".into())),
        None,
        "a gap takes no witness"
    );
    assert_eq!(muse.says(), "muse http://127.0.0.1:4100");
}

/// agy's `settings.json` decides whether an API key in its environment is its active backend:
/// `"modelProvider": "gemini"` selects Gemini API (default `generativelanguage.googleapis.com` or
/// `GOOGLE_GEMINI_BASE_URL`); an absent `settings.json`, omitted `modelProvider` (Google sign-in),
/// another provider (`"gateway"`), or `--settings` on `argv` is a settled gap even when
/// `GEMINI_API_KEY` sits in the environment; a truncated or unreadable `settings.json` is unread so
/// the host re-reads it. NEGATIVE CONTROL (run): with `endpoint_of_with_agy` ignoring `agy()`, a
/// signed-in agy with `GEMINI_API_KEY` in its environment reads as `Named(Base::gemini())` and this
/// test fails.
#[test]
fn agy_settings_gate_whether_an_api_key_in_env_is_its_route() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let home = dir.path().join("home");
    let cfg = home.join(".gemini").join("antigravity-cli");
    std::fs::create_dir_all(&cfg).expect("cfg dir");
    let settings = cfg.join("settings.json");
    let env = vec![format!("HOME={}", home.display()), KEYED.to_string()];
    let route = |argv: &str| {
        let argv = words(argv);
        endpoint_of_with_agy(Program::Agy, &argv, Some(&env), unread_claude, || {
            agy_backend_on_disk(&argv, &env, None)
        })
    };

    // Absent settings.json -> Google sign-in, NOT Gemini API, despite GEMINI_API_KEY in env.
    assert!(
        matches!(route("agy"), Endpoint::Gap(ref why) if why.contains("settings.json is absent")),
        "{:?}",
        route("agy")
    );

    // Present settings.json without modelProvider -> Google sign-in.
    std::fs::write(&settings, "{\"theme\":\"dark\"}\n").expect("write sign-in settings");
    assert!(
        matches!(route("agy"), Endpoint::Gap(ref why) if why.contains("omits modelProvider")),
        "{:?}",
        route("agy")
    );

    // Non-gemini modelProvider -> settled gap, without echoing arbitrary values.
    std::fs::write(&settings, "{\"modelProvider\":\"gateway-SECRET\"}\n").expect("write gateway");
    let gw = route("agy");
    assert!(
        matches!(gw, Endpoint::Gap(ref why) if why.contains("another backend") && !why.contains("SECRET")),
        "{gw:?}"
    );

    // Empty / mid-write settings.json -> Unread (not settled as omitted!).
    std::fs::write(&settings, "").expect("truncate settings");
    assert_eq!(
        agy_backend_on_disk(&words("agy"), &env, None),
        AgyBackend::Unread("agy's settings.json could not be parsed".to_string())
    );

    // modelProvider: "gemini" -> Named(Base::gemini()), unless --settings overrides argv.
    std::fs::write(&settings, "\u{feff}{\"modelProvider\":\"gemini\"}\n").expect("write gemini");
    assert_eq!(route("agy"), Endpoint::Named(Base::gemini()));
    assert!(
        matches!(
            route("agy --settings={}"),
            Endpoint::Gap(ref why) if why.contains("--settings")
        ),
        "session-only --settings override is not guessed"
    );

    // File-provider fence: never opened, settled as Other rather than Unread.
    let cloud = home
        .join("Library")
        .join("CloudStorage")
        .join("OneDrive")
        .join("gem");
    std::fs::create_dir_all(cloud.join("antigravity-cli")).expect("cloud dir");
    std::fs::write(
        cloud.join("antigravity-cli").join("settings.json"),
        "{\"modelProvider\":\"gemini\"}\n",
    )
    .expect("cloud settings");
    let fenced_argv = vec![
        "agy".to_string(),
        format!("--gemini_dir={}", cloud.display()),
    ];
    assert!(
        matches!(
            agy_backend_on_disk(&fenced_argv, &env, Some(&home)),
            AgyBackend::Other(ref why) if why.contains("file-provider domain")
        ),
        "fenced settings.json is settled without opening"
    );
}
