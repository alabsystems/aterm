// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::collections::HashMap;

#[derive(Default)]
struct FakeEnv(HashMap<String, OsString>);

impl FakeEnv {
    fn with(pairs: &[(&str, &str)]) -> Self {
        Self(
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), OsString::from(v)))
                .collect(),
        )
    }

    fn and_raw(mut self, name: &str, value: OsString) -> Self {
        self.0.insert(name.to_owned(), value);
        self
    }
}

impl EnvSource for FakeEnv {
    fn var_os(&self, name: &str) -> Option<OsString> {
        self.0.get(name).cloned()
    }
}

/// `before` and `after` around one sequence that is not valid Unicode: the
/// byte 0xFF on Unix, an unpaired surrogate on Windows.
#[cfg(unix)]
fn not_unicode(before: &str, after: &str) -> OsString {
    use std::os::unix::ffi::OsStringExt;
    let mut bytes = before.as_bytes().to_vec();
    bytes.push(0xFF);
    bytes.extend_from_slice(after.as_bytes());
    OsString::from_vec(bytes)
}

#[cfg(windows)]
fn not_unicode(before: &str, after: &str) -> OsString {
    use std::os::windows::ffi::OsStringExt;
    let mut wide: Vec<u16> = before.encode_utf16().collect();
    wide.push(0xD800);
    wide.extend(after.encode_utf16());
    OsString::from_wide(&wide)
}

fn uri(text: &str) -> Uri {
    Uri::parse(text).unwrap()
}

/// The host of the proxy `env` resolves for `target` under
/// [`ProxyMode::Environment`]; `None` for a direct connection.
fn proxy_host(env: &FakeEnv, target: &str) -> Option<String> {
    resolve(ProxyMode::Environment, &uri(target), env)
        .unwrap()
        .map(|proxy| proxy.uri().host().to_owned())
}

/// The refusal message for an environment that must not resolve.
fn refusal(env: &FakeEnv, target: &str) -> String {
    match resolve(ProxyMode::Environment, &uri(target), env) {
        Err(Error::Invalid(message)) => message,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn direct_mode_ignores_every_proxy_variable() {
    let env = FakeEnv::with(&[
        ("HTTPS_PROXY", "http://evil.test:8080"),
        ("HTTP_PROXY", "socks5://evil.test:1080"),
        ("ALL_PROXY", "http://evil.test:8080"),
    ]);
    for target in ["https://api.test/v1", "http://127.0.0.1:11434/api"] {
        assert!(
            resolve(ProxyMode::Direct, &uri(target), &env)
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn scheme_selects_the_variable() {
    let env = FakeEnv::with(&[
        ("HTTP_PROXY", "http://plain.test:1"),
        ("HTTPS_PROXY", "http://secure.test:2"),
    ]);
    assert_eq!(
        proxy_host(&env, "http://a.test/").as_deref(),
        Some("plain.test")
    );
    assert_eq!(
        proxy_host(&env, "https://a.test/").as_deref(),
        Some("secure.test")
    );
}

#[test]
fn uppercase_wins_over_lowercase_and_lowercase_is_a_fallback() {
    let env = FakeEnv::with(&[
        ("HTTP_PROXY", "http://upper.test:1"),
        ("http_proxy", "http://lower.test:1"),
    ]);
    assert_eq!(
        proxy_host(&env, "http://a.test/").as_deref(),
        Some("upper.test")
    );
    let env = FakeEnv::with(&[("http_proxy", "http://lower.test:1")]);
    assert_eq!(
        proxy_host(&env, "http://a.test/").as_deref(),
        Some("lower.test")
    );
}

#[test]
fn all_proxy_is_the_fallback_after_the_schemes_own_variable() {
    for name in ["ALL_PROXY", "all_proxy"] {
        let env = FakeEnv::with(&[(name, "http://all.test:3128")]);
        for target in ["https://a.test/", "http://a.test/"] {
            assert_eq!(
                proxy_host(&env, target).as_deref(),
                Some("all.test"),
                "{name} must proxy {target}"
            );
        }
    }
    let env = FakeEnv::with(&[
        ("ALL_PROXY", "http://all.test:3128"),
        ("all_proxy", "http://lower-all.test:3128"),
        ("https_proxy", "http://secure.test:3128"),
    ]);
    assert_eq!(
        proxy_host(&env, "https://a.test/").as_deref(),
        Some("secure.test")
    );
    assert_eq!(
        proxy_host(&env, "http://a.test/").as_deref(),
        Some("all.test")
    );
    let env = FakeEnv::with(&[
        ("ALL_PROXY", "http://all.test:3128"),
        ("NO_PROXY", "a.test"),
    ]);
    assert_eq!(proxy_host(&env, "https://a.test/"), None);
}

#[test]
fn userinfo_becomes_a_basic_credential_and_leaves_the_proxy_uri() {
    for (raw, credential) in [
        ("http://alice:s3cret@p.test:3128", "YWxpY2U6czNjcmV0"),
        ("http://alice:p%40ss@p.test:3128", "YWxpY2U6cEBzcw=="),
        ("http://alice:p@ss@p.test:3128/", "YWxpY2U6cEBzcw=="),
        ("http://alice@p.test:3128", "YWxpY2U6"),
    ] {
        let env = FakeEnv::with(&[("HTTPS_PROXY", raw)]);
        let proxy = resolve(ProxyMode::Environment, &uri("https://a.test/"), &env)
            .unwrap()
            .unwrap_or_else(|| panic!("{raw} must resolve"));
        assert_eq!(proxy.basic_auth(), Some(credential), "{raw}");
        assert_eq!(proxy.uri().authority(), "p.test:3128", "{raw}");
        assert!(
            !format!("{proxy:?}").contains(credential),
            "Debug must not print the credential: {proxy:?}"
        );
    }
    let env = FakeEnv::with(&[("HTTPS_PROXY", "http://p.test:3128")]);
    let proxy = resolve(ProxyMode::Environment, &uri("https://a.test/"), &env)
        .unwrap()
        .unwrap();
    assert_eq!(proxy.basic_auth(), None);
}

#[test]
fn an_unusable_proxy_setting_is_an_error_naming_the_variable_not_a_direct_connection() {
    for bad in [
        "evil.test:8080",
        "socks5://evil.test:1080",
        "not a url",
        "/x",
        "http://",
        "http://@p.test:3128",
        "http://a%3Ab:pw@p.test:3128",
        "http://alice:pw@",
        "http://p.test:99999",
    ] {
        for name in ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"] {
            let env = FakeEnv::with(&[(name, bad)]);
            let message = refusal(&env, "https://a.test/");
            assert!(message.contains(name), "{name}={bad}: {message}");
        }
    }
    let env = FakeEnv::with(&[
        ("HTTPS_PROXY", "proxy.test:3128"),
        ("https_proxy", "http://proxy.test:3128"),
    ]);
    assert!(refusal(&env, "https://a.test/").contains("HTTPS_PROXY"));
}

#[cfg(any(unix, windows))]
#[test]
fn a_value_that_is_not_valid_unicode_fails_closed_naming_its_variable() {
    for name in ["HTTPS_PROXY", "https_proxy", "ALL_PROXY"] {
        for value in [
            not_unicode("http://p.test", ":3128"),
            not_unicode("http://p.test:3128/", ""),
            not_unicode("http://alice:pw", "@p.test:3128"),
            not_unicode("", ""),
        ] {
            let env = FakeEnv::with(&[("all_proxy", "http://fallback.test:3128")])
                .and_raw(name, value.clone());
            let message = refusal(&env, "https://a.test/");
            assert!(message.contains(name), "{name}={value:?}: {message}");
        }
    }
    let env = FakeEnv::with(&[("HTTPS_PROXY", "http://p.test:3128")])
        .and_raw("NO_PROXY", not_unicode("a.test,", ".b.test"));
    assert_eq!(proxy_host(&env, "https://a.test/"), None);
    assert_eq!(
        proxy_host(&env, "https://b.test/").as_deref(),
        Some("p.test")
    );
}

#[test]
fn a_credential_escape_that_is_not_utf8_is_refused_not_altered() {
    for bad in [
        "http://alice:%FF@p.test:3128",
        "http://alice:pw%C3@p.test:3128",
        "http://%FF:pw@p.test:3128",
        "http://alice:%EF%BF%BD@p.test:3128",
    ] {
        let env = FakeEnv::with(&[("HTTPS_PROXY", bad)]);
        let message = refusal(&env, "https://a.test/");
        assert!(message.contains("HTTPS_PROXY"), "{bad}: {message}");
    }
    let env = FakeEnv::with(&[("HTTPS_PROXY", "http://alice:%C3%A9@p.test:3128")]);
    let proxy = resolve(ProxyMode::Environment, &uri("https://a.test/"), &env)
        .unwrap()
        .unwrap();
    assert_eq!(proxy.basic_auth(), Some("YWxpY2U6w6k="));
}

#[test]
fn https_proxy_urls_are_refused_not_spoken_to_in_plaintext() {
    for (name, target) in [
        ("HTTPS_PROXY", "https://a.test/"),
        ("HTTP_PROXY", "http://a.test/"),
        ("ALL_PROXY", "https://a.test/"),
    ] {
        let env = FakeEnv::with(&[(name, "https://p.test:443")]);
        let message = refusal(&env, target);
        assert!(message.contains(name), "{name}: {message}");
    }
}

#[test]
fn a_refusal_never_echoes_the_value() {
    for bad in [
        "socks5://alice:s3cret@p.test:1080",
        "https://alice:s3cret@p.test:443",
        "http://alice:s3cret@p.test:0",
        "alice:s3cret@p.test:3128",
    ] {
        let env = FakeEnv::with(&[("HTTPS_PROXY", bad)]);
        let message = refusal(&env, "https://a.test/");
        assert!(!message.contains("s3cret"), "{message}");
        assert!(!message.contains("p.test"), "{message}");
    }
}

#[test]
fn no_proxy_star_exempts_everything() {
    let env = FakeEnv::with(&[("HTTPS_PROXY", "http://p.test:1"), ("NO_PROXY", "*")]);
    assert_eq!(proxy_host(&env, "https://a.test/"), None);
}

#[test]
fn no_proxy_matches_on_a_label_boundary_only() {
    let env = FakeEnv::with(&[
        ("HTTPS_PROXY", "http://p.test:1"),
        ("NO_PROXY", "example.test"),
    ]);
    assert_eq!(proxy_host(&env, "https://example.test/"), None);
    assert_eq!(proxy_host(&env, "https://api.example.test/"), None);
    assert!(proxy_host(&env, "https://notexample.test/").is_some());
}

#[test]
fn no_proxy_honours_a_leading_dot_and_a_port_restriction() {
    let env = FakeEnv::with(&[
        ("HTTPS_PROXY", "http://p.test:1"),
        ("NO_PROXY", ".example.test, other.test:8443"),
    ]);
    assert_eq!(proxy_host(&env, "https://a.example.test/"), None);
    assert_eq!(proxy_host(&env, "https://other.test:8443/"), None);
    assert!(proxy_host(&env, "https://other.test/").is_some());
    for bad in [
        "other.test:99999",
        "other.test:0",
        "other.test:abc",
        "other.test:",
    ] {
        let env = FakeEnv::with(&[("HTTPS_PROXY", "http://p.test:1"), ("NO_PROXY", bad)]);
        assert!(
            proxy_host(&env, "https://other.test/").is_some(),
            "NO_PROXY={bad} must not exempt all ports of other.test"
        );
    }
}

#[test]
fn no_proxy_is_case_insensitive() {
    let env = FakeEnv::with(&[
        ("HTTPS_PROXY", "http://p.test:1"),
        ("NO_PROXY", "EXAMPLE.TEST"),
    ]);
    assert_eq!(proxy_host(&env, "https://Example.Test/"), None);
}

#[test]
fn no_proxy_matches_ip_literals_as_addresses() {
    for (entry, target) in [
        ("2001:db8::5", "https://[2001:db8::5]/v1"),
        ("[2001:db8::5]", "https://[2001:db8::5]/v1"),
        ("[2001:db8::5]:8443", "https://[2001:db8::5]:8443/v1"),
        ("2001:DB8:0:0:0:0:0:5", "https://[2001:db8::5]/v1"),
        ("10.0.0.7", "https://10.0.0.7/v1"),
        ("10.0.0.7:8443", "https://10.0.0.7:8443/v1"),
    ] {
        let env = FakeEnv::with(&[("HTTPS_PROXY", "http://p.test:1"), ("NO_PROXY", entry)]);
        assert_eq!(
            proxy_host(&env, target),
            None,
            "NO_PROXY={entry} must exempt {target}"
        );
    }
    for (entry, target) in [
        ("[2001:db8::5]:8443", "https://[2001:db8::5]/v1"),
        ("[2001:db8::5]:99999", "https://[2001:db8::5]/v1"),
        ("[2001:db8::5]:abc", "https://[2001:db8::5]/v1"),
        ("[2001:db8::5]garbage", "https://[2001:db8::5]/v1"),
        ("0.0.7", "https://10.0.0.7/v1"),
        ("::5", "https://[2001:db8::5]/v1"),
    ] {
        let env = FakeEnv::with(&[("HTTPS_PROXY", "http://p.test:1"), ("NO_PROXY", entry)]);
        assert!(
            proxy_host(&env, target).is_some(),
            "NO_PROXY={entry} must not exempt {target}"
        );
    }
}

#[test]
fn absent_configuration_is_a_direct_connection() {
    assert_eq!(proxy_host(&FakeEnv::default(), "https://a.test/"), None);
}
