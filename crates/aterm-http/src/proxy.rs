// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `HTTP_PROXY` / `HTTPS_PROXY` / `ALL_PROXY` / `NO_PROXY` discovery.

use std::ffi::OsString;
use std::net::{IpAddr, Ipv6Addr};

use crate::client::Error;
use crate::uri::{Scheme, Uri, strip_prefix_ascii_ci};

/// Whether the environment's proxy configuration may be consulted.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ProxyMode {
    /// Honour `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY` and `NO_PROXY`.
    #[default]
    Environment,
    /// Connect directly, ignoring every proxy environment variable.
    Direct,
}

/// Reads process environment variables.
pub trait EnvSource {
    /// The raw value of `name`, if set.
    fn var_os(&self, name: &str) -> Option<OsString>;
}

/// The real process environment.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessEnv;

impl EnvSource for ProcessEnv {
    fn var_os(&self, name: &str) -> Option<OsString> {
        std::env::var_os(name)
    }
}

/// that entry matches no host and the others still apply.
fn env_text<E: EnvSource>(env: &E, name: &str) -> Option<String> {
    let value = env.var_os(name)?;
    let value = value.to_string_lossy();
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

/// A proxy this client speaks through.
#[derive(Clone)]
pub struct ProxyTarget {
    uri: Uri,
    basic_auth: Option<String>,
}

impl ProxyTarget {
    /// The proxy's `http://` URL without its userinfo; only its host and port are used.
    #[inline]
    #[must_use]
    pub const fn uri(&self) -> &Uri {
        &self.uri
    }

    /// The base64 of `user:password` for `Proxy-Authorization: Basic`.
    #[inline]
    #[must_use]
    pub fn basic_auth(&self) -> Option<&str> {
        self.basic_auth.as_deref()
    }
}

impl std::fmt::Debug for ProxyTarget {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProxyTarget")
            .field("uri", &self.uri)
            .field(
                "basic_auth",
                &self.basic_auth.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

/// Resolve the proxy to use for `target`: `Ok(None)` for a direct connection.
/// # Errors
/// [`Error::Invalid`].
pub fn resolve<E: EnvSource>(
    mode: ProxyMode,
    target: &Uri,
    env: &E,
) -> Result<Option<ProxyTarget>, Error> {
    if mode == ProxyMode::Direct {
        return Ok(None);
    }
    let no_proxy = env_text(env, "NO_PROXY").or_else(|| env_text(env, "no_proxy"));
    if no_proxy_matches(target, no_proxy) {
        return Ok(None);
    }
    let for_scheme = match target.scheme() {
        Scheme::Https => ["HTTPS_PROXY", "https_proxy"],
        Scheme::Http => ["HTTP_PROXY", "http_proxy"],
    };
    let Some((name, raw)) = for_scheme
        .into_iter()
        .chain(["ALL_PROXY", "all_proxy"])
        .find_map(|name| env_text(env, name).map(|raw| (name, raw)))
    else {
        return Ok(None);
    };
    parse_proxy(name, &raw).map(Some)
}

/// Parse the value of the proxy variable `name`.
pub(crate) fn parse_proxy(name: &str, raw: &str) -> Result<ProxyTarget, Error> {
    let unusable = || {
        Error::Invalid(format!(
            "{name} is set but is not a usable http:// proxy URL"
        ))
    };
    if raw.len() > 8192
        || raw
            .chars()
            .any(|c| c.is_control() || c == ' ' || c == char::REPLACEMENT_CHARACTER)
    {
        return Err(unusable());
    }
    let Some(rest) = strip_prefix_ascii_ci(raw, "http://") else {
        return Err(if strip_prefix_ascii_ci(raw, "https://").is_some() {
            Error::Invalid(format!(
                "{name} names an https:// proxy, which this client does not support; use an http:// proxy URL"
            ))
        } else {
            unusable()
        });
    };
    let (authority, tail) = rest
        .find(['/', '?', '#'])
        .and_then(|end| rest.split_at_checked(end))
        .unwrap_or((rest, ""));
    let (basic_auth, host_port) = match authority.rsplit_once('@') {
        None => (None, authority),
        Some((userinfo, host_port)) => {
            let (user, password) = userinfo.split_once(':').unwrap_or((userinfo, ""));
            let user = aterm_codec::pct::decode(user);
            let password = aterm_codec::pct::decode(password);
            if userinfo.is_empty()
                || user.contains(':')
                || user.contains(char::REPLACEMENT_CHARACTER)
                || password.contains(char::REPLACEMENT_CHARACTER)
            {
                return Err(unusable());
            }
            let credential = aterm_codec::base64::encode(format!("{user}:{password}").as_bytes())
                .map_err(|_| unusable())?;
            (Some(credential), host_port)
        }
    };
    let uri = Uri::parse(&format!("http://{host_port}{tail}")).ok_or_else(unusable)?;
    Ok(ProxyTarget { uri, basic_auth })
}

/// or bracketed IPv6) matches the same address and nothing else.
fn no_proxy_matches(target: &Uri, value: Option<String>) -> bool {
    let Some(value) = value else {
        return false;
    };
    let host = target.host().to_ascii_lowercase();
    let host_ip = host.parse::<IpAddr>().ok();
    for entry in value.split(',') {
        let entry = entry.trim().to_ascii_lowercase();
        if entry.is_empty() {
            continue;
        }
        if entry == "*" {
            return true;
        }
        let (entry_host, entry_port) = split_no_proxy_entry(&entry);
        if entry_port.is_some_and(|port| port != target.port()) {
            continue;
        }
        let entry_host = entry_host.trim_start_matches('.');
        if entry_host.is_empty() {
            continue;
        }
        let matched = match (host_ip, entry_host.parse::<IpAddr>().ok()) {
            (Some(host_ip), Some(entry_ip)) => host_ip == entry_ip,
            (Some(_), None) | (None, Some(_)) => false,
            (None, None) => {
                host == entry_host
                    || (host.len() > entry_host.len()
                        && host.ends_with(entry_host)
                        && host.as_bytes()[host.len() - entry_host.len() - 1] == b'.')
            }
        };
        if matched {
            return true;
        }
    }
    false
}

/// (`[2001:db8::5]:8443`) ends at its bracket.
fn split_no_proxy_entry(entry: &str) -> (&str, Option<u16>) {
    let (host, port) = if let Some((host, tail)) = entry
        .strip_prefix('[')
        .and_then(|rest| rest.split_once(']'))
    {
        (host, tail.strip_prefix(':'))
    } else if entry.parse::<Ipv6Addr>().is_ok() {
        (entry, None)
    } else {
        match entry.rsplit_once(':') {
            Some((host, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => {
                (host, Some(port))
            }
            _ => (entry, None),
        }
    };
    (host, port.and_then(|port| port.parse::<u16>().ok()))
}

#[cfg(test)]
mod tests {
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
}
