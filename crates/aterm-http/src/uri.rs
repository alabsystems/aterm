// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Absolute `http`/`https` URL parsing — the slice of RFC 3986 an HTTP client
//! actually needs, and no more.
//!
//! This is deliberately NOT a general URI type. It accepts exactly what the
//! title-summary worker can be pointed at (an absolute `http://` or `https://`
//! URL with an optional port and path/query) and rejects everything else, so a
//! malformed or hostile endpoint string fails at parse time rather than
//! becoming a surprising connection. In particular:
//!
//! * userinfo (`user:pass@host`) is REJECTED, not stripped — the endpoint
//!   policy in `title_summary` already requires credential-free URLs, and a
//!   parser that silently dropped credentials would make that check vacuous;
//! * a control character or space anywhere in the string is rejected, so a
//!   header cannot be smuggled through the request line;
//! * an empty host is rejected, as is a port that is not a `u16`.
//!
//! Percent-encoding is deliberately NOT decoded. The path and query are carried
//! through to the request line VERBATIM — re-encoding is what turns one
//! well-formed request into two (request smuggling), and a client that never
//! rewrites the target cannot introduce that class of bug.

/// The scheme of an absolute URL this client can speak.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scheme {
    /// Plaintext HTTP.
    Http,
    /// HTTP over TLS.
    Https,
}

impl Scheme {
    /// The port used when the URL does not name one.
    #[must_use]
    pub const fn default_port(self) -> u16 {
        match self {
            Self::Http => 80,
            Self::Https => 443,
        }
    }

    /// The lowercase scheme token, without the `://`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
        }
    }
}

/// A parsed absolute `http`/`https` URL.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Uri {
    scheme: Scheme,
    /// Host WITHOUT brackets for an IPv6 literal (`::1`, not `[::1]`) — the
    /// form both `ServerName` and `SocketAddr` want.
    host: String,
    port: u16,
    /// Origin-form request target: always begins with `/`.
    path_and_query: String,
    /// True when the authority named a port explicitly.
    explicit_port: bool,
    /// The authority EXACTLY as it appeared in the input, brackets and all.
    /// Kept so a caller can prove the parse agrees with a naive textual split
    /// of the same string — an endpoint the two disagree about is ambiguous,
    /// and an ambiguous endpoint should be refused rather than guessed at.
    authority_raw: String,
}

impl Uri {
    /// Parse an absolute `http`/`https` URL. `None` on anything this client
    /// will not speak — see the module docs for the exact rejections.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        // No control characters and no spaces: the string is about to become
        // part of a request line, and CR/LF there is header injection.
        if text.is_empty()
            || text.len() > 8192
            || text
                .chars()
                .any(|c| c.is_control() || c == ' ' || c == '\t')
        {
            return None;
        }
        let (scheme, rest) = strip_prefix_ascii_ci(text, "https://")
            .map(|rest| (Scheme::Https, rest))
            .or_else(|| strip_prefix_ascii_ci(text, "http://").map(|rest| (Scheme::Http, rest)))?;
        // The authority ends at the first `/`, `?` or `#`. A fragment is not
        // sent to the server, so it is split off and dropped here.
        let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let (authority, tail) = rest.split_at(authority_end);
        if authority.is_empty() || authority.contains('@') {
            // Empty authority, or userinfo — rejected, never stripped.
            return None;
        }
        let (host, port, explicit_port) = split_authority(authority, scheme)?;
        let path_and_query = origin_form(tail);
        Some(Self {
            scheme,
            host,
            port,
            path_and_query,
            explicit_port,
            authority_raw: authority.to_owned(),
        })
    }

    /// The URL's scheme.
    #[must_use]
    pub const fn scheme(&self) -> Scheme {
        self.scheme
    }

    /// Host, without brackets for an IPv6 literal.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Port — the scheme default when the URL did not name one.
    #[must_use]
    pub const fn port(&self) -> u16 {
        self.port
    }

    /// The origin-form request target (`/v1/chat/completions?x=1`).
    #[must_use]
    pub fn path_and_query(&self) -> &str {
        &self.path_and_query
    }

    /// The path alone, without the query string.
    #[must_use]
    pub fn path(&self) -> &str {
        self.path_and_query
            .split('?')
            .next()
            .unwrap_or(&self.path_and_query)
    }

    /// The authority EXACTLY as written in the source string.
    ///
    /// For agreement checks against a naive `split("://")` of the same input:
    /// if this differs from the textual split, the endpoint is ambiguous.
    #[must_use]
    pub fn authority_as_written(&self) -> &str {
        &self.authority_raw
    }

    /// The `Host` header value: the authority as the peer should see it, with
    /// the port elided when it is the scheme default (RFC 9110 §7.2) and an
    /// IPv6 literal re-bracketed.
    #[must_use]
    pub fn host_header(&self) -> String {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        if self.explicit_port && self.port != self.scheme.default_port() {
            format!("{host}:{}", self.port)
        } else {
            host
        }
    }

    /// `host:port` in the form a proxy `CONNECT` target and a DNS resolve want.
    #[must_use]
    pub fn authority(&self) -> String {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        format!("{host}:{}", self.port)
    }

    /// Whether the host is an IPv4/IPv6 loopback literal or `localhost`.
    ///
    /// Name-based: `localhost` is treated as loopback WITHOUT resolving it,
    /// which is the conservative direction for the one decision this feeds —
    /// whether to bypass a configured proxy. Sending loopback traffic direct
    /// can only keep it on this machine.
    #[must_use]
    pub fn host_is_loopback(&self) -> bool {
        host_is_loopback(&self.host)
    }
}

/// Whether a bare host string names the loopback interface.
#[must_use]
pub fn host_is_loopback(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    if let Ok(v4) = host.parse::<std::net::Ipv4Addr>() {
        return v4.is_loopback();
    }
    if let Ok(v6) = host.parse::<std::net::Ipv6Addr>() {
        return v6.is_loopback();
    }
    false
}

/// ASCII-case-insensitive `strip_prefix` (schemes are case-insensitive).
fn strip_prefix_ascii_ci<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let head = text.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &text[prefix.len()..])
}

/// Split `host[:port]`, handling the `[v6]:port` bracket form.
fn split_authority(authority: &str, scheme: Scheme) -> Option<(String, u16, bool)> {
    if let Some(rest) = authority.strip_prefix('[') {
        // IPv6 literal: the host runs to the closing bracket.
        let close = rest.find(']')?;
        let host = &rest[..close];
        if host.is_empty() || host.parse::<std::net::Ipv6Addr>().is_err() {
            return None;
        }
        let after = &rest[close + 1..];
        let (port, explicit) = parse_port(after, scheme)?;
        return Some((host.to_owned(), port, explicit));
    }
    // A bare `:` count above one is a malformed authority (an unbracketed IPv6
    // literal), not a host with a port.
    let mut parts = authority.split(':');
    let host = parts.next()?;
    let port_text = parts.next();
    if parts.next().is_some() || host.is_empty() {
        return None;
    }
    if !host_is_plausible(host) {
        return None;
    }
    let (port, explicit) = match port_text {
        None => (scheme.default_port(), false),
        Some(text) => (parse_port_digits(text)?, true),
    };
    Some((host.to_owned(), port, explicit))
}

/// Parse the `":port"` (or empty) tail after an IPv6 literal's `]`.
fn parse_port(after: &str, scheme: Scheme) -> Option<(u16, bool)> {
    if after.is_empty() {
        return Some((scheme.default_port(), false));
    }
    let digits = after.strip_prefix(':')?;
    Some((parse_port_digits(digits)?, true))
}

/// A port must be non-empty ASCII digits fitting a non-zero `u16`.
fn parse_port_digits(text: &str) -> Option<u16> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse::<u16>().ok().filter(|&p| p != 0)
}

/// Reject a host containing a character that has no business in a `Host`
/// header or an SNI name.
fn host_is_plausible(host: &str) -> bool {
    host.bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'%'))
}

/// Normalize the post-authority tail into an origin-form request target: a
/// missing path becomes `/`, and a fragment is dropped (never sent).
fn origin_form(tail: &str) -> String {
    let tail = tail.split('#').next().unwrap_or("");
    if tail.is_empty() {
        return "/".to_owned();
    }
    if tail.starts_with('?') {
        return format!("/{tail}");
    }
    tail.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_shapes_the_worker_is_pointed_at() {
        let u = Uri::parse("http://127.0.0.1:11434/api/chat").unwrap();
        assert_eq!(u.scheme(), Scheme::Http);
        assert_eq!(u.host(), "127.0.0.1");
        assert_eq!(u.port(), 11434);
        assert_eq!(u.path_and_query(), "/api/chat");
        assert_eq!(u.host_header(), "127.0.0.1:11434");
        assert!(u.host_is_loopback());

        let u = Uri::parse("https://llm.example.test/v1/chat/completions").unwrap();
        assert_eq!(u.scheme(), Scheme::Https);
        assert_eq!(u.port(), 443);
        // Default port is elided from the Host header.
        assert_eq!(u.host_header(), "llm.example.test");
        assert_eq!(u.path_and_query(), "/v1/chat/completions");
        assert!(!u.host_is_loopback());
    }

    #[test]
    fn missing_path_becomes_root_and_a_bare_query_keeps_it() {
        assert_eq!(Uri::parse("http://h").unwrap().path_and_query(), "/");
        assert_eq!(Uri::parse("http://h/").unwrap().path_and_query(), "/");
        assert_eq!(
            Uri::parse("http://h?a=1").unwrap().path_and_query(),
            "/?a=1"
        );
        assert_eq!(
            Uri::parse("http://h/p?a=1").unwrap().path_and_query(),
            "/p?a=1"
        );
    }

    #[test]
    fn a_fragment_is_dropped_and_never_reaches_the_request_line() {
        let u = Uri::parse("http://h/p?a=1#secret").unwrap();
        assert_eq!(u.path_and_query(), "/p?a=1");
    }

    #[test]
    fn percent_encoding_is_carried_through_verbatim() {
        // Re-encoding a target is how one request becomes two. The bytes the
        // caller configured are the bytes on the wire.
        let u = Uri::parse("http://h/a%2Fb%20c?q=%41").unwrap();
        assert_eq!(u.path_and_query(), "/a%2Fb%20c?q=%41");
    }

    #[test]
    fn userinfo_is_rejected_rather_than_stripped() {
        // Silently dropping credentials would make the endpoint policy's
        // credential-free check vacuous.
        assert!(Uri::parse("http://user:pass@host/p").is_none());
        assert!(Uri::parse("https://token@host/p").is_none());
    }

    #[test]
    fn control_characters_and_spaces_cannot_smuggle_a_header() {
        assert!(Uri::parse("http://h/p\r\nX-Evil: 1").is_none());
        assert!(Uri::parse("http://h/p q").is_none());
        assert!(Uri::parse("http://h\n/p").is_none());
        assert!(Uri::parse("http://h/p\0").is_none());
    }

    #[test]
    fn only_http_and_https_are_accepted() {
        for bad in [
            "ftp://h/p",
            "file:///etc/passwd",
            "/relative/path",
            "h/p",
            "",
            "ws://h/p",
        ] {
            assert!(Uri::parse(bad).is_none(), "{bad} must not parse");
        }
        // The scheme itself is case-insensitive.
        assert_eq!(Uri::parse("HtTpS://h/p").unwrap().scheme(), Scheme::Https);
    }

    #[test]
    fn ipv6_literals_round_trip_through_brackets() {
        let u = Uri::parse("http://[::1]:8080/p").unwrap();
        // Host is stored UNBRACKETED (what ServerName and SocketAddr want)...
        assert_eq!(u.host(), "::1");
        assert_eq!(u.port(), 8080);
        // ...and re-bracketed for the Host header and the CONNECT target.
        assert_eq!(u.host_header(), "[::1]:8080");
        assert_eq!(u.authority(), "[::1]:8080");
        assert!(u.host_is_loopback());

        let u = Uri::parse("https://[2606:4700::1111]/p").unwrap();
        assert_eq!(u.host_header(), "[2606:4700::1111]");
        assert!(!u.host_is_loopback());
    }

    #[test]
    fn malformed_authorities_are_rejected() {
        for bad in [
            "http://",
            "http:///p",
            "http://h:/p",
            "http://h:0/p",
            "http://h:99999/p",
            "http://h:abc/p",
            "http://::1/p",  // unbracketed IPv6
            "http://[::1/p", // unclosed bracket
            "http://[zz]/p", // not an IPv6 address
        ] {
            assert!(Uri::parse(bad).is_none(), "{bad} must not parse");
        }
    }

    #[test]
    fn the_raw_authority_and_bare_path_are_available_for_agreement_checks() {
        let u = Uri::parse("http://127.0.0.1:11434/api/chat").unwrap();
        assert_eq!(u.authority_as_written(), "127.0.0.1:11434");
        assert_eq!(u.path(), "/api/chat");

        let u = Uri::parse("https://api.test/v1/x?a=1").unwrap();
        assert_eq!(u.authority_as_written(), "api.test");
        // `path` drops the query; `path_and_query` keeps it.
        assert_eq!(u.path(), "/v1/x");
        assert_eq!(u.path_and_query(), "/v1/x?a=1");

        // Brackets are preserved verbatim, because the check compares against
        // the caller's own textual split of the same input.
        let u = Uri::parse("http://[::1]:8080/p").unwrap();
        assert_eq!(u.authority_as_written(), "[::1]:8080");
    }

    #[test]
    fn loopback_detection_covers_the_forms_a_local_daemon_is_named_by() {
        for yes in [
            "http://127.0.0.1:11434/",
            "http://127.9.9.9/",
            "http://localhost:11434/",
            "http://LocalHost/",
            "http://[::1]/",
        ] {
            assert!(Uri::parse(yes).unwrap().host_is_loopback(), "{yes}");
        }
        for no in [
            "http://10.0.0.1/",
            "http://example.test/",
            "http://[2606:4700::1111]/",
            // Not loopback: a name that merely CONTAINS localhost.
            "http://localhost.evil.test/",
        ] {
            assert!(!Uri::parse(no).unwrap().host_is_loopback(), "{no}");
        }
    }
}
