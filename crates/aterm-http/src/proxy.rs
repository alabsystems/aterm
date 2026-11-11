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

/// A value that is not valid Unicode converts lossily (U+FFFD) rather than
/// reading as unset, which would hand the decision to the next variable and
/// possibly a direct connection. [`parse_proxy`] refuses U+FFFD; in `NO_PROXY`
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
/// [`Error::Invalid`], naming the variable and never its value, when the
/// variable that decides is set to something this client cannot use.
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

/// Forms: `*`, a bare host, a `.suffix`, and a port-restricted `host:port`.
/// Matching is case-insensitive and a suffix matches only on a label boundary
/// (`example.test` never exempts `notexample.test`). An IP literal (IPv4, bare
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
        let Some((entry_host, entry_port)) = split_no_proxy_entry(&entry) else {
            continue;
        };
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

fn parse_no_proxy_port(text: &str) -> Option<u16> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse::<u16>().ok().filter(|&p| p != 0)
}

/// The port follows the colon only when that colon is not an IPv6 literal's
/// own: a bare IPv6 literal (`2001:db8::5`) has no port, a bracketed one
/// (`[2001:db8::5]:8443`) ends at its bracket, and a malformed bracket/port
/// tail (`other.test:99999`, `[2001:db8::5]:bad`, `[2001:db8::5]x`) is
/// rejected as `None` rather than stripped into a wildcard over all ports.
fn split_no_proxy_entry(entry: &str) -> Option<(&str, Option<u16>)> {
    if let Some(rest) = entry.strip_prefix('[') {
        let (host, tail) = rest.split_once(']')?;
        if host.parse::<Ipv6Addr>().is_err() {
            return None;
        }
        if tail.is_empty() {
            return Some((host, None));
        }
        let port = tail.strip_prefix(':').and_then(parse_no_proxy_port)?;
        return Some((host, Some(port)));
    }
    if entry.parse::<Ipv6Addr>().is_ok() {
        return Some((entry, None));
    }
    match entry.split_once(':') {
        None => Some((entry, None)),
        Some((host, port)) => Some((host, Some(parse_no_proxy_port(port)?))),
    }
}

#[cfg(test)]
#[path = "proxy_tests.rs"]
mod tests;
