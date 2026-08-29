// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `HTTP_PROXY` / `HTTPS_PROXY` / `NO_PROXY` discovery, and the `CONNECT`
//! tunnel an `https` request needs to cross a proxy.
//!
//! The caller decides WHETHER a proxy may be consulted at all
//! ([`ProxyMode`]); this module only answers "given that it may, which one".
//! That split matters: the title-summary worker forces
//! [`ProxyMode::Direct`] for a loopback endpoint so that a stray `HTTPS_PROXY`
//! in the environment can never turn a local-only trust decision into
//! terminal-data or bearer-token egress to an unrelated host.
//!
//! One deliberate hardening over the usual convention: the lowercase
//! `http_proxy` variable is honoured (it is the older, more common spelling)
//! but lowercase `https_proxy` is read only when the uppercase form is absent,
//! and a proxy URL that is not an absolute `http://`/`https://` URL is IGNORED
//! rather than guessed at. A malformed proxy setting must not silently become
//! a direct connection to an attacker-chosen host.

use crate::uri::{Scheme, Uri};

/// Whether the environment's proxy configuration may be consulted.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ProxyMode {
    /// Honour `HTTP_PROXY`, `HTTPS_PROXY` and `NO_PROXY`.
    #[default]
    Environment,
    /// Connect directly, ignoring every proxy environment variable.
    Direct,
}

/// Reads process environment variables. Injected so the resolution rules can be
/// tested without mutating the process environment, which is shared state in a
/// threaded test binary.
pub trait EnvSource {
    /// The value of `name`, if set and non-empty after trimming.
    fn var(&self, name: &str) -> Option<String>;
}

/// The real process environment.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessEnv;

impl EnvSource for ProcessEnv {
    fn var(&self, name: &str) -> Option<String> {
        std::env::var(name)
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
    }
}

/// Resolve the proxy to use for `target`, or `None` for a direct connection.
#[must_use]
pub fn resolve<E: EnvSource>(mode: ProxyMode, target: &Uri, env: &E) -> Option<Uri> {
    if mode == ProxyMode::Direct {
        return None;
    }
    if no_proxy_matches(target, env.var("NO_PROXY").or_else(|| env.var("no_proxy"))) {
        return None;
    }
    let raw = match target.scheme() {
        Scheme::Https => env.var("HTTPS_PROXY").or_else(|| env.var("https_proxy")),
        Scheme::Http => env.var("HTTP_PROXY").or_else(|| env.var("http_proxy")),
    }?;
    // A proxy setting we cannot parse is IGNORED, never guessed at.
    Uri::parse(&raw)
}

/// Whether `NO_PROXY` exempts `target`.
///
/// Supports the conventional forms: `*` (everything), a bare host, a
/// `.suffix`, and a `host:port` restricted entry. Matching is
/// case-insensitive, and a suffix entry matches only on a LABEL boundary, so
/// `example.test` never exempts `notexample.test`.
fn no_proxy_matches(target: &Uri, value: Option<String>) -> bool {
    let Some(value) = value else {
        return false;
    };
    let host = target.host().to_ascii_lowercase();
    for entry in value.split(',') {
        let entry = entry.trim().to_ascii_lowercase();
        if entry.is_empty() {
            continue;
        }
        if entry == "*" {
            return true;
        }
        // `host:port` entries only exempt that exact port.
        let (entry_host, entry_port) = match entry.rsplit_once(':') {
            Some((h, p)) if p.bytes().all(|b| b.is_ascii_digit()) && !p.is_empty() => {
                (h.to_owned(), p.parse::<u16>().ok())
            }
            _ => (entry.clone(), None),
        };
        if entry_port.is_some_and(|port| port != target.port()) {
            continue;
        }
        let entry_host = entry_host.trim_start_matches('.');
        if entry_host.is_empty() {
            continue;
        }
        if host == entry_host
            || (host.len() > entry_host.len()
                && host.ends_with(entry_host)
                && host.as_bytes()[host.len() - entry_host.len() - 1] == b'.')
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[derive(Default)]
    struct FakeEnv(HashMap<String, String>);

    impl FakeEnv {
        fn with(pairs: &[(&str, &str)]) -> Self {
            Self(
                pairs
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                    .collect(),
            )
        }
    }

    impl EnvSource for FakeEnv {
        fn var(&self, name: &str) -> Option<String> {
            self.0.get(name).cloned().filter(|v| !v.is_empty())
        }
    }

    fn uri(text: &str) -> Uri {
        Uri::parse(text).unwrap()
    }

    #[test]
    fn direct_mode_ignores_every_proxy_variable() {
        // THE loopback guarantee: a stray HTTPS_PROXY must never redirect
        // terminal context or a bearer token off this machine.
        let env = FakeEnv::with(&[
            ("HTTPS_PROXY", "http://evil.test:8080"),
            ("HTTP_PROXY", "http://evil.test:8080"),
        ]);
        assert!(resolve(ProxyMode::Direct, &uri("https://api.test/v1"), &env).is_none());
        assert!(resolve(ProxyMode::Direct, &uri("http://127.0.0.1:11434/api"), &env).is_none());
    }

    #[test]
    fn scheme_selects_the_variable() {
        let env = FakeEnv::with(&[
            ("HTTP_PROXY", "http://plain.test:1"),
            ("HTTPS_PROXY", "http://secure.test:2"),
        ]);
        let p = resolve(ProxyMode::Environment, &uri("http://a.test/"), &env).unwrap();
        assert_eq!(p.host(), "plain.test");
        let p = resolve(ProxyMode::Environment, &uri("https://a.test/"), &env).unwrap();
        assert_eq!(p.host(), "secure.test");
    }

    #[test]
    fn uppercase_wins_over_lowercase_and_lowercase_is_a_fallback() {
        let env = FakeEnv::with(&[
            ("HTTP_PROXY", "http://upper.test:1"),
            ("http_proxy", "http://lower.test:1"),
        ]);
        assert_eq!(
            resolve(ProxyMode::Environment, &uri("http://a.test/"), &env)
                .unwrap()
                .host(),
            "upper.test"
        );
        let env = FakeEnv::with(&[("http_proxy", "http://lower.test:1")]);
        assert_eq!(
            resolve(ProxyMode::Environment, &uri("http://a.test/"), &env)
                .unwrap()
                .host(),
            "lower.test"
        );
    }

    #[test]
    fn a_malformed_proxy_url_is_ignored_rather_than_guessed_at() {
        for bad in [
            "evil.test:8080",
            "socks5://evil.test:1080",
            "not a url",
            "/x",
        ] {
            let env = FakeEnv::with(&[("HTTPS_PROXY", bad)]);
            assert!(
                resolve(ProxyMode::Environment, &uri("https://a.test/"), &env).is_none(),
                "{bad} must not resolve to a proxy"
            );
        }
    }

    #[test]
    fn no_proxy_star_exempts_everything() {
        let env = FakeEnv::with(&[("HTTPS_PROXY", "http://p.test:1"), ("NO_PROXY", "*")]);
        assert!(resolve(ProxyMode::Environment, &uri("https://a.test/"), &env).is_none());
    }

    #[test]
    fn no_proxy_matches_on_a_label_boundary_only() {
        let env = FakeEnv::with(&[
            ("HTTPS_PROXY", "http://p.test:1"),
            ("NO_PROXY", "example.test"),
        ]);
        // Exact host and a true subdomain are exempt...
        assert!(resolve(ProxyMode::Environment, &uri("https://example.test/"), &env).is_none());
        assert!(
            resolve(
                ProxyMode::Environment,
                &uri("https://api.example.test/"),
                &env
            )
            .is_none()
        );
        // ...but a host that merely ENDS with the string is not.
        assert!(
            resolve(
                ProxyMode::Environment,
                &uri("https://notexample.test/"),
                &env
            )
            .is_some()
        );
    }

    #[test]
    fn no_proxy_honours_a_leading_dot_and_a_port_restriction() {
        let env = FakeEnv::with(&[
            ("HTTPS_PROXY", "http://p.test:1"),
            ("NO_PROXY", ".example.test, other.test:8443"),
        ]);
        assert!(
            resolve(
                ProxyMode::Environment,
                &uri("https://a.example.test/"),
                &env
            )
            .is_none()
        );
        // Port-restricted entry matches only that port.
        assert!(
            resolve(
                ProxyMode::Environment,
                &uri("https://other.test:8443/"),
                &env
            )
            .is_none()
        );
        assert!(resolve(ProxyMode::Environment, &uri("https://other.test/"), &env).is_some());
    }

    #[test]
    fn no_proxy_is_case_insensitive() {
        let env = FakeEnv::with(&[
            ("HTTPS_PROXY", "http://p.test:1"),
            ("NO_PROXY", "EXAMPLE.TEST"),
        ]);
        assert!(resolve(ProxyMode::Environment, &uri("https://Example.Test/"), &env).is_none());
    }

    #[test]
    fn absent_configuration_is_a_direct_connection() {
        let env = FakeEnv::default();
        assert!(resolve(ProxyMode::Environment, &uri("https://a.test/"), &env).is_none());
    }
}
