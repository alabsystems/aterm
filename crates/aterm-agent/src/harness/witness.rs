// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ROUTE A SESSION'S API REQUESTS TAKE, AS ITS LAUNCH NAMES IT, AND THE WITNESSES ON IT (v6
//! §14.17.5.4, slice 12c).
//!
//! * [`Target`]: the endpoint a probe reaches, a host, a port and whether TLS carries it.
//! * [`Base`]: a launch's base URL, its [`Target`] and the path under it: two gateways' paths on
//!   one host (`/v1/openai`, `/v1/anthropic`) are two routes a probe reaches alike.
//! * [`endpoint_of`] / [`endpoint_of_with_agy`]: the base URL a session's own launch names (Claude
//!   Code's route as its settings chain reads it, Muse Code's `--base-url` or
//!   `-c endpoint_transport.base_url`, agy's `GOOGLE_GEMINI_BASE_URL` or default
//!   `generativelanguage.googleapis.com` when `settings.json` and `--settings` select
//!   `"modelProvider": "gemini"` on its Gemini API key), or a named [`Endpoint::Gap`]
//!   where the launch names none, a proxy carries it, or the program's route is not read: never a
//!   guessed default.
//! * [`RouteKey`]: the program and the base URL, which a witness must share with the session it
//!   clears: a Muse Code's turn says nothing of an agy on the same host, nor a turn on one host
//!   or one path of another. It names no model: a witness clears per program and base URL, so an
//!   overload one model meets alone is cleared by another model's answer there, once per outage
//!   (`WallTrack::witnessed` leaves the ladder to try it again).
//! * [`Witnesses`]: the instance's book of the last turn each session finished with a real reply
//!   on its route. An outage wait reads the newest of another session's on its own route
//!   ([`Witnesses::witness`]); a turn finished after the wall's last try is the outage's clearing,
//!   seen ([`crate::supervise::policy::turn_end`]'s `outage_wait`).

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use aterm_phase::Program;

use super::netwatch::{API_HOST, API_PORT, Route, Source};

/// A launch URL larger than this is an unsupported route, never copied into a witness key.
pub const MAX_BASE_URL_BYTES: usize = 4096;

/// Where a probe reaches an agent's API: a host, its port, and whether TLS carries the requests
/// (`https`), so a measure is a verified handshake, or plain HTTP, so it is an HTTP answer.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Target {
    pub host: String,
    pub port: u16,
    pub tls: bool,
}

impl Target {
    /// Claude Code's default route: `api.anthropic.com:443`, over TLS.
    #[must_use]
    pub fn anthropic() -> Self {
        Self {
            host: API_HOST.to_string(),
            port: API_PORT,
            tls: true,
        }
    }

    /// agy's default Gemini API route: `generativelanguage.googleapis.com:443`, over TLS.
    #[must_use]
    pub fn gemini() -> Self {
        Self {
            host: AGY_GEMINI_HOST.to_string(),
            port: 443,
            tls: true,
        }
    }

    /// The endpoint a base URL names: `http://` or `https://`, a host (a bracketed IPv6 literal
    /// taken without its brackets and canonicalized), an optional port (else the scheme's), and
    /// any path after it,
    /// which a probe does not need. URL userinfo, query strings and fragments are unsupported:
    /// dropping them could merge distinct routes or disclose endpoint credentials. `None` also
    /// for an oversized URL, whitespace, another scheme, no host, or an invalid port or literal.
    #[must_use]
    pub fn of_url(url: &str) -> Option<Self> {
        let url = url.trim();
        if url.len() > MAX_BASE_URL_BYTES
            || url.chars().any(|c| c.is_whitespace() || c.is_control())
            || url.contains(['?', '#'])
        {
            return None;
        }
        let (tls, rest) = if let Some(rest) = strip_prefix_ci(url, "https://") {
            (true, rest)
        } else {
            (false, strip_prefix_ci(url, "http://")?)
        };
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
        if authority.contains('@') {
            return None;
        }
        let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
            let (host, after) = v6.split_once(']')?;
            let ip = host.parse::<std::net::Ipv6Addr>().ok()?;
            let port = if after.is_empty() {
                None
            } else {
                Some(after.strip_prefix(':')?)
            };
            (ip.to_string(), port)
        } else {
            match authority.rsplit_once(':') {
                Some((host, port)) if !host.contains(':') => {
                    (host.to_ascii_lowercase(), Some(port))
                }
                Some(_) => return None,
                None => (authority.to_ascii_lowercase(), None),
            }
        };
        if host.is_empty() {
            return None;
        }
        let port = match port {
            Some(p) => p.parse::<u16>().ok().filter(|p| *p > 0)?,
            None if tls => 443,
            None => 80,
        };
        Some(Self { host, port, tls })
    }

    /// The endpoint as a log line names it: `https://api.anthropic.com:443`.
    #[must_use]
    pub fn says(&self) -> String {
        let scheme = if self.tls { "https" } else { "http" };
        if self.host.contains(':') {
            format!("{scheme}://[{}]:{}", self.host, self.port)
        } else {
            format!("{scheme}://{}:{}", self.host, self.port)
        }
    }
}

/// A LAUNCH'S BASE URL AS A ROUTE (v6 §14.17.5.4): the endpoint a probe reaches ([`Target`]) and
/// the path under it, which a probe does not need but a route does: a gateway that serves one
/// provider under `/v1/openai` and another under `/v1/anthropic` on one host answers one while
/// the other is out, so a witness on one path is none on the other.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Base {
    pub target: Target,
    /// The path without a trailing `/` (`""` for the root). URLs with userinfo, a query or a
    /// fragment are refused rather than collapsed into this key.
    pub path: String,
}

impl std::fmt::Debug for Base {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Base")
            .field("target", &self.target)
            .field(
                "path",
                &if self.path.is_empty() {
                    ""
                } else {
                    "<custom-path>"
                },
            )
            .finish()
    }
}

impl Base {
    /// Claude Code's default route: `https://api.anthropic.com`, at its root.
    #[must_use]
    pub fn anthropic() -> Self {
        Self {
            target: Target::anthropic(),
            path: String::new(),
        }
    }

    /// agy's default Gemini API route: `https://generativelanguage.googleapis.com`, at its root.
    #[must_use]
    pub fn gemini() -> Self {
        Self {
            target: Target::gemini(),
            path: String::new(),
        }
    }

    /// The route a base URL names ([`Target::of_url`] and its path); `None` where it names no
    /// endpoint.
    #[must_use]
    pub fn of_url(url: &str) -> Option<Self> {
        let target = Target::of_url(url)?;
        let url = url.trim();
        let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
        let after = rest.find(['/', '?', '#']).map_or("", |at| &rest[at..]);
        let path = if after.starts_with('/') {
            after
                .split(['?', '#'])
                .next()
                .unwrap_or_default()
                .trim_end_matches('/')
        } else {
            ""
        };
        Some(Self {
            target,
            path: path.to_string(),
        })
    }

    /// A safe route display. The opaque path stays private to the key because endpoint paths
    /// can carry credentials; a non-root route is shown only as `/<custom-path>`.
    #[must_use]
    pub fn says(&self) -> String {
        format!(
            "{}{}",
            self.target.says(),
            if self.path.is_empty() {
                ""
            } else {
                "/<custom-path>"
            }
        )
    }
}

fn strip_prefix_ci<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let head = text.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &text[prefix.len()..])
}

/// The route a session's API requests take, as far as its own launch names it ([`endpoint_of`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    /// The launch names it: a probe can reach its endpoint, and a witness on it is one.
    Named(Base),
    /// The launch names none, or the host cannot reproduce it: why, for the log. No probe reaches
    /// it and no witness clears it; its outage is read off the network path alone and tried
    /// blind, as before slice 12c.
    Gap(String),
}

/// agy's default Gemini API host (`generativelanguage.googleapis.com`).
pub const AGY_GEMINI_HOST: &str = "generativelanguage.googleapis.com";

/// agy's default Gemini API base URL (`https://generativelanguage.googleapis.com`).
pub const AGY_GEMINI_URL: &str = "https://generativelanguage.googleapis.com";

/// The variable agy reads a custom Gemini endpoint from (`GOOGLE_GEMINI_BASE_URL`), on its Gemini
/// API backend ([`AgyBackend::GeminiApi`], [`AGY_API_KEYS`]).
pub const AGY_BASE_URL: &str = "GOOGLE_GEMINI_BASE_URL";

/// The variables that give agy a Gemini API key, the backend [`AGY_BASE_URL`] and
/// [`AGY_GEMINI_URL`] serve when `settings.json` sets `"modelProvider": "gemini"`: with none, or
/// without that setting, agy calls its Google sign-in's route (`cloudcode-pa.googleapis.com`),
/// which no variable here names.
pub const AGY_API_KEYS: [&str; 2] = ["GEMINI_API_KEY", "GOOGLE_API_KEY"];

/// The variable that turns agy to Vertex AI, whose endpoint (`GOOGLE_VERTEX_BASE_URL`, a region's
/// host) is not read.
pub const AGY_VERTEX: &str = "GOOGLE_GENAI_USE_VERTEXAI";

/// The variable Google's Go GenAI SDK reads ahead of [`AGY_VERTEX`] to turn agy to Vertex AI /
/// Gemini Enterprise (`false` overrides [`AGY_VERTEX`]).
pub const AGY_ENTERPRISE: &str = "GOOGLE_GENAI_USE_ENTERPRISE";

/// The variable that turns agy to an Enterprise LLM Gateway ahead of its Gemini API key and Google
/// sign-in; its gateway discovery endpoint is not read.
pub const AGY_GATEWAY_URL: &str = "AGY_LLM_GATEWAY_URL";

/// The variable that names a Claude Code route of its own's endpoint.
pub const CLAUDE_BASE_URL: &str = "ANTHROPIC_BASE_URL";

/// The option Muse Code reads its provider's endpoint from (`--base-url URL`, `--base-url=URL`).
pub const MUSE_BASE_URL: &str = "--base-url";

/// The proxy variables that carry an `https` request, in the order a client reads them.
pub const HTTPS_PROXY_KEYS: [&str; 4] = ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"];

/// The proxy variables that carry an `http` request, in the order a client reads them.
pub const HTTP_PROXY_KEYS: [&str; 4] = ["HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"];

/// The variables that list the hosts no proxy carries.
pub const NO_PROXY_KEYS: [&str; 2] = ["NO_PROXY", "no_proxy"];

/// The value of `key` in `env` (`KEY=value` entries; the first wins), `None` when unset or blank.
fn var<'a>(env: &'a [String], key: &str) -> Option<&'a str> {
    env.iter()
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v.trim())
        .filter(|v| !v.is_empty())
}

/// Whether `no_proxy` (a `NO_PROXY` list: hosts, `.suffix`es, `host:port`s, or `*`) names `target`.
#[must_use]
pub fn no_proxy_names(no_proxy: &str, target: &Target) -> bool {
    no_proxy.split(',').map(str::trim).any(|entry| {
        if entry == "*" {
            return true;
        }
        if let Some(v6) = entry.strip_prefix('[') {
            let Some((host, after)) = v6.split_once(']') else {
                return false;
            };
            let Ok(ip) = host.parse::<std::net::Ipv6Addr>() else {
                return false;
            };
            let port = if after.is_empty() {
                None
            } else {
                let Some(port) = after
                    .strip_prefix(':')
                    .and_then(|p| p.parse::<u16>().ok())
                    .filter(|p| *p > 0)
                else {
                    return false;
                };
                Some(port)
            };
            return port.is_none_or(|p| p == target.port) && target.host == ip.to_string();
        }
        let (host, port) = match entry.rsplit_once(':') {
            Some((h, p)) if !h.contains(':') => {
                let Some(port) = (!p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
                    .then(|| p.parse::<u16>().ok().filter(|p| *p > 0))
                    .flatten()
                else {
                    return false;
                };
                (h, Some(port))
            }
            Some(_) => {
                return entry
                    .parse::<std::net::Ipv6Addr>()
                    .is_ok_and(|ip| target.host == ip.to_string());
            }
            None => (entry, None),
        };
        if port.is_some_and(|p| p != target.port) {
            return false;
        }
        let host = host.strip_prefix("*.").unwrap_or(host);
        if host.contains('*') {
            return false;
        }
        let bare = host.strip_prefix('.').unwrap_or(host).to_ascii_lowercase();
        if bare.is_empty() || bare.starts_with('.') {
            return false;
        }
        target.host == bare || target.host.ends_with(&format!(".{bare}"))
    })
}

/// The endpoint `base` (a base URL a launch names under `what`) gives, unless a proxy in `env`
/// carries its requests (the probe would measure the direct route, which the agent does not take).
fn named(base: &str, what: &str, env: &[String]) -> Endpoint {
    let Some(base) = Base::of_url(base) else {
        return Endpoint::Gap(format!("{what} is not a supported http(s) base URL"));
    };
    let target = &base.target;
    let keys = if target.tls {
        HTTPS_PROXY_KEYS
    } else {
        HTTP_PROXY_KEYS
    };
    if let Some(key) = keys.iter().find(|k| var(env, k).is_some()) {
        let lists: Vec<&str> = NO_PROXY_KEYS.iter().filter_map(|k| var(env, k)).collect();
        let spared =
            !lists.is_empty() && lists.into_iter().all(|list| no_proxy_names(list, target));
        if !spared {
            return Endpoint::Gap(format!("{key} carries {what}"));
        }
    }
    Endpoint::Named(base)
}

/// The `ANTHROPIC_BASE_URL` a Claude Code launch's route variables (`env`) name, where it is the
/// only route variable besides the proxies and `NO_PROXY` ([`named`] judges those).
fn claude_base(env: &[String]) -> Option<&str> {
    let others = env
        .iter()
        .filter_map(|kv| kv.split_once('='))
        .any(|(key, value)| {
            key != CLAUDE_BASE_URL
                && !HTTPS_PROXY_KEYS.contains(&key)
                && !HTTP_PROXY_KEYS.contains(&key)
                && !NO_PROXY_KEYS.contains(&key)
                && super::netwatch::names_a_route(key, value)
        });
    var(env, CLAUDE_BASE_URL).filter(|_| !others)
}

/// Go's `strconv.ParseBool` grammar, as Google's Go GenAI SDK inside `agy` parses boolean
/// environment variables.
fn parse_go_bool(value: &str) -> Option<bool> {
    match value {
        "1" | "t" | "T" | "true" | "TRUE" | "True" => Some(true),
        "0" | "f" | "F" | "false" | "FALSE" | "False" => Some(false),
        _ => None,
    }
}

/// Why an `agy` launch's Vertex / Enterprise flags leave its Gemini API route unread, following
/// Google's Go GenAI SDK (`GOOGLE_GENAI_USE_ENTERPRISE` overrides `GOOGLE_GENAI_USE_VERTEXAI`).
pub(super) fn agy_vertex_reason(env: &[String]) -> Option<String> {
    if let Some(v) = var(env, AGY_ENTERPRISE) {
        return match parse_go_bool(v) {
            Some(true) => Some(format!(
                "agy's launch sets {AGY_ENTERPRISE}: its Vertex AI endpoint is not read"
            )),
            Some(false) => None,
            None => Some(format!(
                "agy's launch sets {AGY_ENTERPRISE} to a non-boolean value, so its backend is not \
                 read"
            )),
        };
    }
    let v = var(env, AGY_VERTEX)?;
    match parse_go_bool(v) {
        Some(true) => Some(format!(
            "agy's launch sets {AGY_VERTEX}: its Vertex AI endpoint is not read"
        )),
        Some(false) => None,
        None => Some(format!(
            "agy's launch sets {AGY_VERTEX} to a non-boolean value, so its backend is not read"
        )),
    }
}

/// What `agy`'s `settings.json` (and `--settings`) says of its model backend ([`agy_backend`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgyBackend {
    /// `settings.json` and any `--settings` session override select `"modelProvider": "gemini"`:
    /// `agy` speaks to the Gemini API on [`AGY_API_KEYS`].
    GeminiApi,
    /// Settled for the process: `settings.json` and `--settings` omit `"modelProvider"`, name
    /// another provider (Google sign-in, gateway), pass a relative or unparseable `--settings`
    /// override, its config directory cannot be resolved, or a settings file lies inside a
    /// file-provider domain.
    Other(String),
    /// `settings.json` or an absolute `--settings` file could not be read or parsed this time (as
    /// one mid-write cannot): read again at the next ask.
    Unread(String),
}

enum OverrideChoice {
    GeminiApi,
    Other(String),
    Unset,
}

pub(super) fn parse_settings_object(text: &str) -> Option<aterm_json::Map> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    match aterm_json::from_str::<aterm_json::Value>(text).ok()? {
        aterm_json::Value::Object(map) => Some(map),
        _ => None,
    }
}

fn agy_override_choice(object: &aterm_json::Map, label: &str) -> OverrideChoice {
    match object.get("modelProvider") {
        None => OverrideChoice::Unset,
        Some(aterm_json::Value::Null) => OverrideChoice::Other(format!(
            "agy's {label} clears modelProvider, so it uses Google sign-in rather than Gemini API"
        )),
        Some(aterm_json::Value::String(p)) if p == "gemini" => OverrideChoice::GeminiApi,
        Some(aterm_json::Value::String(p)) if p.is_empty() => OverrideChoice::Other(format!(
            "agy's {label} clears modelProvider, so it uses Google sign-in rather than Gemini API"
        )),
        Some(aterm_json::Value::String(_)) => OverrideChoice::Other(format!(
            "agy's {label} sets modelProvider to another backend (not \"gemini\")"
        )),
        Some(_) => OverrideChoice::Other(format!("agy's {label} sets a non-string modelProvider")),
    }
}

fn agy_settings_file_backend(settings: &Source) -> AgyBackend {
    match settings {
        Source::Missing => AgyBackend::Other(
            "agy's settings.json is absent, so it uses Google sign-in rather than Gemini API"
                .to_string(),
        ),
        Source::Unreadable => AgyBackend::Unread("agy's settings.json could not be read".into()),
        Source::Text(text) => {
            let Some(object) = parse_settings_object(text) else {
                return AgyBackend::Unread("agy's settings.json could not be parsed".to_string());
            };
            match object.get("modelProvider") {
                None | Some(aterm_json::Value::Null) => AgyBackend::Other(
                    "agy's settings.json omits modelProvider, so it uses Google sign-in rather \
                     than Gemini API"
                        .to_string(),
                ),
                Some(aterm_json::Value::String(p)) if p == "gemini" => AgyBackend::GeminiApi,
                Some(aterm_json::Value::String(_)) => AgyBackend::Other(
                    "agy's settings.json sets modelProvider to another backend (not \"gemini\")"
                        .to_string(),
                ),
                Some(_) => AgyBackend::Other(
                    "agy's settings.json sets a non-string modelProvider".to_string(),
                ),
            }
        }
    }
}

/// [`agy_backend`] with an optional session `--settings` file document (`override_file`) when
/// `argv`'s last `--settings` flag names an absolute file path rather than an inline JSON object.
#[must_use]
pub fn agy_backend_with_override(
    argv: &[String],
    settings: &Source,
    override_file: Option<&Source>,
) -> AgyBackend {
    match super::upgrade_agy::settings_override(argv) {
        None => agy_settings_file_backend(settings),
        Some(None) => AgyBackend::Other("agy's launch passes --settings with no value".to_string()),
        Some(Some(raw)) => {
            let trimmed = raw.strip_prefix('\u{feff}').unwrap_or(raw).trim_start();
            if trimmed.starts_with('{') {
                let Some(object) = parse_settings_object(trimmed) else {
                    return AgyBackend::Other(
                        "agy's --settings inline value is not a valid JSON object".to_string(),
                    );
                };
                match agy_override_choice(&object, "--settings") {
                    OverrideChoice::GeminiApi => AgyBackend::GeminiApi,
                    OverrideChoice::Other(why) => AgyBackend::Other(why),
                    OverrideChoice::Unset => agy_settings_file_backend(settings),
                }
            } else {
                if raw.is_empty() {
                    return AgyBackend::Other(
                        "agy's launch passes an empty --settings file path".to_string(),
                    );
                }
                match override_file {
                    None if !Path::new(raw).is_absolute() => AgyBackend::Other(
                        "agy's launch passes a relative --settings file path, whose session \
                         override is not read"
                            .to_string(),
                    ),
                    None => AgyBackend::Other(
                        "agy's launch passes a --settings file path, whose session override is \
                         not read"
                            .to_string(),
                    ),
                    Some(Source::Missing) => {
                        AgyBackend::Other("agy's --settings file is absent".to_string())
                    }
                    Some(Source::Unreadable) => {
                        AgyBackend::Unread("agy's --settings file could not be read".to_string())
                    }
                    Some(Source::Text(text)) => {
                        let Some(object) = parse_settings_object(text) else {
                            return AgyBackend::Unread(
                                "agy's --settings file could not be parsed".to_string(),
                            );
                        };
                        match agy_override_choice(&object, "--settings file") {
                            OverrideChoice::GeminiApi => AgyBackend::GeminiApi,
                            OverrideChoice::Other(why) => AgyBackend::Other(why),
                            OverrideChoice::Unset => agy_settings_file_backend(settings),
                        }
                    }
                }
            }
        }
    }
}

/// THE PURE HALF OF [`agy_backend_on_disk`]: what `argv` and `<config>/settings.json` (`settings`)
/// say of `agy`'s model backend. Measured on `agy 1.4.12`: `agy` uses `GEMINI_API_KEY` /
/// `GOOGLE_API_KEY` and `GOOGLE_GEMINI_BASE_URL` only when `settings.json` (overlaid with
/// `--settings`) sets `"modelProvider": "gemini"`.
#[must_use]
pub fn agy_backend(argv: &[String], settings: &Source) -> AgyBackend {
    agy_backend_with_override(argv, settings, None)
}

/// Reads `<config>/settings.json` and any `--settings` session override for an `agy` launch
/// (`argv`, `env`, and the host's `host_home` as both fallback home and file-provider fence)
/// through the bounded regular-file reader
/// ([`crate::supervise::policy::claude_settings::read_settings_in`]).
#[must_use]
pub fn agy_backend_on_disk(
    argv: &[String],
    env: &[String],
    host_home: Option<&Path>,
) -> AgyBackend {
    agy_backend_in_dir(argv, env, None, host_home)
}

/// [`agy_backend_on_disk`] resolving a relative `--settings` file path against `cwd` when `cwd`
/// is an absolute directory.
#[must_use]
pub fn agy_backend_in_dir(
    argv: &[String],
    env: &[String],
    cwd: Option<&Path>,
    host_home: Option<&Path>,
) -> AgyBackend {
    let mut override_source = None;
    match super::upgrade_agy::settings_override(argv) {
        None => {}
        Some(None) => return agy_backend_with_override(argv, &Source::Missing, None),
        Some(Some(raw)) => {
            let trimmed = raw.strip_prefix('\u{feff}').unwrap_or(raw).trim_start();
            if trimmed.starts_with('{') {
                let Some(object) = parse_settings_object(trimmed) else {
                    return agy_backend_with_override(argv, &Source::Missing, None);
                };
                if !matches!(
                    agy_override_choice(&object, "--settings"),
                    OverrideChoice::Unset
                ) {
                    return agy_backend_with_override(argv, &Source::Missing, None);
                }
            } else {
                if raw.is_empty() {
                    return agy_backend_with_override(argv, &Source::Missing, None);
                }
                let raw_path = Path::new(raw);
                let resolved = if raw_path.is_absolute() {
                    crate::supervise::policy::git_config::normalise(raw_path)
                } else if let Some(dir) = cwd.filter(|d| d.is_absolute()) {
                    crate::supervise::policy::git_config::normalise(&dir.join(raw_path))
                } else {
                    return agy_backend_with_override(argv, &Source::Missing, None);
                };
                match crate::supervise::policy::claude_settings::read_settings_in(
                    host_home, &resolved,
                ) {
                    Ok(None) => {
                        return agy_backend_with_override(
                            argv,
                            &Source::Missing,
                            Some(&Source::Missing),
                        );
                    }
                    Err(why) if why.contains("file-provider domain") => {
                        return AgyBackend::Other(format!("agy's --settings file {why}"));
                    }
                    Err(_) => {
                        return agy_backend_with_override(
                            argv,
                            &Source::Missing,
                            Some(&Source::Unreadable),
                        );
                    }
                    Ok(Some(text)) => {
                        let falls_through = parse_settings_object(&text).is_some_and(|object| {
                            matches!(
                                agy_override_choice(&object, "--settings file"),
                                OverrideChoice::Unset
                            )
                        });
                        let src = Source::Text(text);
                        if !falls_through {
                            return agy_backend_with_override(argv, &Source::Missing, Some(&src));
                        }
                        override_source = Some(src);
                    }
                }
            }
        }
    }
    let Some(config) =
        super::upgrade_agy::config_dir_or(argv, var(env, super::upgrade_agy::HOME_ENV), host_home)
    else {
        return AgyBackend::Other("agy's config directory could not be resolved".to_string());
    };
    let path = config.join(super::upgrade_agy::SETTINGS_JSON);
    match crate::supervise::policy::claude_settings::read_settings_in(host_home, &path) {
        Ok(None) => agy_backend_with_override(argv, &Source::Missing, override_source.as_ref()),
        Ok(Some(text)) => {
            agy_backend_with_override(argv, &Source::Text(text), override_source.as_ref())
        }
        Err(why) if why.contains("file-provider domain") => {
            AgyBackend::Other(format!("agy's settings.json {why}"))
        }
        Err(_) => agy_backend_with_override(argv, &Source::Unreadable, override_source.as_ref()),
    }
}

/// [`endpoint_of_with_agy`] assuming [`AgyBackend::GeminiApi`] (`settings.json` already known to
/// select `"modelProvider": "gemini"`). Live hosts call [`endpoint_of_with_agy`] with
/// [`agy_backend_on_disk`].
#[must_use]
pub fn endpoint_of(
    program: Program,
    argv: &[String],
    env: Option<&[String]>,
    claude: impl FnOnce() -> Route,
) -> Endpoint {
    endpoint_of_with_agy(program, argv, env, claude, || AgyBackend::GeminiApi)
}

/// THE ENDPOINT A SESSION'S OWN LAUNCH NAMES (v6 §14.17.5.4): read from the agent's `argv` and
/// exec environment `env` (`None`: it could not be read), never guessed. Claude Code's is its
/// route as the settings chain reads it (`claude`, asked only for Claude Code): the default route
/// is `api.anthropic.com`; a route of its own is the `ANTHROPIC_BASE_URL` its launch's route
/// variables name (`env`, [`super::netwatch::launch_env`]) where no other route variable (Bedrock,
/// Vertex, Foundry, a Unix socket, `CLAUDE_CODE_PROXY…`) is set, and otherwise the route's own
/// reason as a gap. Muse Code's is its `--base-url` (or `-c endpoint_transport.base_url` when no
/// `-c endpoint_transport` proxy or custom CA bundle overrides transport). agy's (`agy`, asked
/// only for `agy` once `env` is read) requires `settings.json` overlaid with `--settings` to
/// select `"modelProvider": "gemini"` ([`AgyBackend::GeminiApi`]), a Gemini API key
/// ([`AGY_API_KEYS`]), and Vertex AI off ([`AGY_ENTERPRISE`] overriding [`AGY_VERTEX`]): then
/// `GOOGLE_GEMINI_BASE_URL` names a custom endpoint and its absence names [`AGY_GEMINI_URL`]
/// (`generativelanguage.googleapis.com`). Signed in with Google (no `"modelProvider": "gemini"` or
/// no key) or on Vertex, agy calls a route those variables do not name. Each is a gap where a
/// proxy carries it or the program's route is not read. codex-alab carries its own Autopilot and
/// Grok Build and Kimi Code their own forks, so no route of theirs is read.
#[must_use]
pub fn endpoint_of_with_agy(
    program: Program,
    argv: &[String],
    env: Option<&[String]>,
    claude: impl FnOnce() -> Route,
    agy: impl FnOnce() -> AgyBackend,
) -> Endpoint {
    let unread = |who: &str| Endpoint::Gap(format!("{who}'s environment could not be read"));
    match program {
        Program::Claude => match claude() {
            Route::Default => Endpoint::Named(Base::anthropic()),
            Route::Custom(why) => match env.and_then(|env| claude_base(env).map(|b| (env, b))) {
                Some((env, base)) => named(base, CLAUDE_BASE_URL, env),
                None => Endpoint::Gap(why),
            },
        },
        Program::Muse => {
            let Some(env) = env else {
                return unread("Muse Code");
            };
            match super::upgrade_muse::launch_endpoint(argv, env) {
                super::upgrade_muse::LaunchEndpoint::BaseUrl { url, source } => {
                    named(&url, source, env)
                }
                super::upgrade_muse::LaunchEndpoint::Gap(why) => Endpoint::Gap(why),
            }
        }
        Program::Agy => {
            let Some(env) = env else {
                return unread("agy");
            };
            if var(env, AGY_GATEWAY_URL).is_some() {
                return Endpoint::Gap(format!(
                    "agy's launch sets {AGY_GATEWAY_URL}, whose Enterprise LLM Gateway route is \
                     not read"
                ));
            }
            match agy() {
                AgyBackend::Unread(why) | AgyBackend::Other(why) => return Endpoint::Gap(why),
                AgyBackend::GeminiApi => {}
            }
            if let Some(why) = agy_vertex_reason(env) {
                return Endpoint::Gap(why);
            }
            let keyed = AGY_API_KEYS.iter().any(|k| var(env, k).is_some());
            if !keyed {
                return Endpoint::Gap(format!(
                    "agy's launch names no Gemini API key ({}), so it is not on its Gemini API \
                     route: its Google sign-in's endpoint is not read",
                    AGY_API_KEYS.join(", ")
                ));
            }
            match var(env, AGY_BASE_URL) {
                Some(base) => named(base, AGY_BASE_URL, env),
                None => named(AGY_GEMINI_URL, "agy's Gemini API endpoint", env),
            }
        }
        Program::Codex | Program::Grok | Program::Kimi | Program::Generic => {
            Endpoint::Gap(format!("{}'s route is not read", program.product()))
        }
    }
}

/// THE ROUTE A WITNESS MUST SHARE: the program and the base URL its launch names (its endpoint and
/// the path under it, [`Base`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RouteKey {
    pub program: Program,
    pub base: Base,
}

impl RouteKey {
    /// The key of a session of `program` whose endpoint is `endpoint`: none for a gap.
    #[must_use]
    pub fn of(program: Program, endpoint: &Endpoint) -> Option<Self> {
        match endpoint {
            Endpoint::Named(base) => Some(Self {
                program,
                base: base.clone(),
            }),
            Endpoint::Gap(_) => None,
        }
    }

    /// The route as a log line names it: `claude https://api.anthropic.com:443`.
    #[must_use]
    pub fn says(&self) -> String {
        format!("{} {}", self.program.name(), self.base.says())
    }
}

/// How long a session's last answer is kept: an outage wait reads only the answers newer than its
/// wall's last try, so an older one can never clear anything a live wait stands on.
pub const WITNESS_KEPT: Duration = Duration::from_secs(24 * 60 * 60);

/// Hard bound on retained session/route answers. At capacity a new answer is omitted until an
/// older record expires; already-booked routes keep their normal witnesses.
pub const MAX_WITNESSES: usize = 4096;

/// THE INSTANCE'S BOOK OF ANSWERED TURNS: for each session and its route, when its last turn
/// finished with a real reply ([`Self::answered`]). The newest of another session's on a route is
/// that route's witness ([`Self::witness`]). It costs nothing while no tab waits: a write at each
/// answered turn end, a read only at an outage wall's points. A session that closed keeps its
/// last answer, as true of its route as any (a wall that began after it is not cleared by it), until
/// it is older than [`WITNESS_KEPT`]: each write drops every such answer, and [`MAX_WITNESSES`]
/// bounds the count even if many sessions answer within a day.
#[derive(Debug, Clone, Default)]
pub struct Witnesses {
    last: HashMap<(RouteKey, String), Instant>,
}

impl Witnesses {
    /// Session `sid`'s turn on route `key` finished with a real reply at `at`: kept where it is the
    /// session's newest, and every answer older than [`WITNESS_KEPT`] dropped.
    pub fn answered(&mut self, key: RouteKey, sid: &str, at: Instant) {
        self.last
            .retain(|_, t| at.saturating_duration_since(*t) < WITNESS_KEPT);
        let key = (key, sid.to_string());
        if self.last.len() >= MAX_WITNESSES && !self.last.contains_key(&key) {
            return;
        }
        let slot = self.last.entry(key).or_insert(at);
        if *slot < at {
            *slot = at;
        }
    }

    /// The newest answer on route `key` of any session but `sid`: the witness `sid`'s outage wait
    /// reads. Another program's, or another endpoint's, is never one.
    #[must_use]
    pub fn witness(&self, key: &RouteKey, sid: &str) -> Option<Instant> {
        self.last
            .iter()
            .filter(|((k, s), _)| k == key && s != sid)
            .map(|(_, at)| *at)
            .max()
    }
}

/// The words a loop's journal marks a resume typed on a witness with ([`witness_note`]), which a
/// reader of the journal (the forever lab's judge) finds it by.
pub const WITNESS_MARK: &str = " witness: ";

/// THE JOURNAL'S NOTE OF A RESUME TYPED ON A WITNESS (v6 §14.17.5.4): at point `seq` under `rule`,
/// on another session's turn on the route that finished at `at` (as the journal stamps it).
#[must_use]
pub fn witness_note(seq: u64, rule: &str, at: &str) -> String {
    format!(
        "NOTE seq={seq} rule={rule}{WITNESS_MARK}another session's turn on this route finished at \
         {at} since the wall's last try, so the outage has cleared"
    )
}

#[cfg(test)]
#[path = "witness_tests.rs"]
mod tests;
