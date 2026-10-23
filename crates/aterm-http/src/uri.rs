// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Absolute `http`/`https` URL parsing for configured HTTP endpoints.
//!
//! Rejects userinfo (`user:pass@host`), control characters, whitespace, empty hosts,
//! and invalid ports, and carries percent-encoded paths and queries through verbatim.

/// The scheme of an absolute URL this client can speak.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scheme {
    Http,
    Https,
}

impl Scheme {
    /// The port used when the URL does not name one.
    #[inline]
    #[must_use]
    pub const fn default_port(self) -> u16 {
        match self {
            Self::Http => 80,
            Self::Https => 443,
        }
    }

    /// The lowercase scheme token, without the `://`.
    #[inline]
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
        }
    }

    /// Return `true` when this scheme uses TLS (`https`).
    #[inline]
    #[must_use]
    pub const fn is_tls(self) -> bool {
        matches!(self, Self::Https)
    }

    /// Return `true` when `port` equals this scheme's default port.
    #[inline]
    #[must_use]
    pub const fn uses_default_port(self, port: u16) -> bool {
        port == self.default_port()
    }
}

/// Maximum accepted URL byte length (`8192`).
pub const MAX_URI_LEN: usize = 8192;

/// Return `true` when `len` is a valid non-empty URI length (`1..=8192`).
#[inline]
#[must_use]
pub const fn is_valid_uri_len(len: usize) -> bool {
    len >= 1 && len <= MAX_URI_LEN
}

/// Return `true` when `status` is a `2xx` HTTP success code (`200..=299`).
#[inline]
#[must_use]
pub const fn is_http_status_success(status: u16) -> bool {
    status >= 200 && status <= 299
}

/// Return `true` when `status` is a `3xx` HTTP redirect code (`300..=399`).
#[inline]
#[must_use]
pub const fn is_http_status_redirect(status: u16) -> bool {
    status >= 300 && status <= 399
}

/// Return `true` when `status` is a `4xx` HTTP client error code (`400..=499`).
#[inline]
#[must_use]
pub const fn is_http_status_client_error(status: u16) -> bool {
    status >= 400 && status <= 499
}

/// Return `true` when `status` is a `5xx` HTTP server error code (`500..=599`).
#[inline]
#[must_use]
pub const fn is_http_status_server_error(status: u16) -> bool {
    status >= 500 && status <= 599
}

/// Encode an HTTP endpoint summary (`host_hash`, `port`, `status`, `body_len`) into 16 LE bytes.
#[inline]
#[must_use]
pub const fn encode_http_endpoint_le(
    host_hash: u64,
    port: u16,
    status: u16,
    body_len: u32,
) -> [u8; 16] {
    [
        host_hash as u8,
        (host_hash >> 8) as u8,
        (host_hash >> 16) as u8,
        (host_hash >> 24) as u8,
        (host_hash >> 32) as u8,
        (host_hash >> 40) as u8,
        (host_hash >> 48) as u8,
        (host_hash >> 56) as u8,
        port as u8,
        (port >> 8) as u8,
        status as u8,
        (status >> 8) as u8,
        body_len as u8,
        (body_len >> 8) as u8,
        (body_len >> 16) as u8,
        (body_len >> 24) as u8,
    ]
}

/// Decode 16 LE bytes into an HTTP endpoint summary (`host_hash`, `port`, `status`, `body_len`).
#[inline]
#[must_use]
pub const fn decode_http_endpoint_le(bytes: [u8; 16]) -> (u64, u16, u16, u32) {
    let host_hash = (bytes[0] as u64)
        | ((bytes[1] as u64) << 8)
        | ((bytes[2] as u64) << 16)
        | ((bytes[3] as u64) << 24)
        | ((bytes[4] as u64) << 32)
        | ((bytes[5] as u64) << 40)
        | ((bytes[6] as u64) << 48)
        | ((bytes[7] as u64) << 56);
    let port = (bytes[8] as u16) | ((bytes[9] as u16) << 8);
    let status = (bytes[10] as u16) | ((bytes[11] as u16) << 8);
    let body_len = (bytes[12] as u32)
        | ((bytes[13] as u32) << 8)
        | ((bytes[14] as u32) << 16)
        | ((bytes[15] as u32) << 24);
    (host_hash, port, status, body_len)
}

/// A parsed absolute `http`/`https` URL.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Uri {
    scheme: Scheme,
    host: String,
    port: u16,
    path_and_query: String,
    explicit_port: bool,
    authority_raw: String,
}

impl Uri {
    /// Parse an absolute `http` or `https` URL.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        // No control characters and no spaces: the string is about to become
        // part of a request line, and CR/LF there is header injection.
        if !is_valid_uri_len(text.len())
            || text
                .chars()
                .any(|c| c.is_control() || c == ' ' || c == '\t')
        {
            return None;
        }
        let (scheme, rest) = strip_prefix_ascii_ci(text, "https://")
            .map(|rest| (Scheme::Https, rest))
            .or_else(|| strip_prefix_ascii_ci(text, "http://").map(|rest| (Scheme::Http, rest)))?;
        let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let (authority, tail) = rest.split_at(authority_end);
        if authority.is_empty() || authority.contains('@') {
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
    #[inline]
    #[must_use]
    pub const fn scheme(&self) -> Scheme {
        self.scheme
    }

    /// Host, without brackets for an IPv6 literal.
    #[inline]
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Port — the scheme default when the URL did not name one.
    #[inline]
    #[must_use]
    pub const fn port(&self) -> u16 {
        self.port
    }

    /// Return `true` when the authority explicitly included `:port`.
    #[inline]
    #[must_use]
    pub const fn has_explicit_port(&self) -> bool {
        self.explicit_port
    }

    /// Return `true` when `self.port()` matches `self.scheme().default_port()`.
    #[inline]
    #[must_use]
    pub const fn uses_default_port(&self) -> bool {
        self.scheme.uses_default_port(self.port)
    }

    /// The origin-form request target (`/v1/chat/completions?x=1`).
    #[inline]
    #[must_use]
    pub fn path_and_query(&self) -> &str {
        &self.path_and_query
    }

    /// The path alone, without the query string.
    #[inline]
    #[must_use]
    pub fn path(&self) -> &str {
        self.path_and_query
            .split('?')
            .next()
            .unwrap_or(&self.path_and_query)
    }

    /// The authority substring exactly as written in the input URL.
    #[inline]
    #[must_use]
    pub fn authority_as_written(&self) -> &str {
        &self.authority_raw
    }

    /// The `Host` header value with default ports elided (RFC 9110 §7.2) and IPv6 literals bracketed.
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
}

#[inline]
pub(crate) fn strip_prefix_ascii_ci<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let (head, rest) = text.split_at_checked(prefix.len())?;
    head.eq_ignore_ascii_case(prefix).then_some(rest)
}

fn split_authority(authority: &str, scheme: Scheme) -> Option<(String, u16, bool)> {
    if let Some(rest) = authority.strip_prefix('[') {
        let close = rest.find(']')?;
        let host = &rest[..close];
        if host.is_empty() || host.parse::<std::net::Ipv6Addr>().is_err() {
            return None;
        }
        let after = &rest[close + 1..];
        let (port, explicit) = parse_port(after, scheme)?;
        return Some((host.to_owned(), port, explicit));
    }
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

fn parse_port(after: &str, scheme: Scheme) -> Option<(u16, bool)> {
    if after.is_empty() {
        return Some((scheme.default_port(), false));
    }
    let digits = after.strip_prefix(':')?;
    Some((parse_port_digits(digits)?, true))
}

fn parse_port_digits(text: &str) -> Option<u16> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse::<u16>().ok().filter(|&p| p != 0)
}

#[inline]
fn host_is_plausible(host: &str) -> bool {
    host.bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'%'))
}

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
    fn parses_a_loopback_and_a_remote_endpoint() {
        let u = Uri::parse("http://127.0.0.1:11434/api/chat").unwrap();
        assert_eq!(u.scheme(), Scheme::Http);
        assert_eq!(u.host(), "127.0.0.1");
        assert_eq!(u.port(), 11434);
        assert_eq!(u.path_and_query(), "/api/chat");
        assert_eq!(u.host_header(), "127.0.0.1:11434");

        let u = Uri::parse("https://llm.example.test/v1/chat/completions").unwrap();
        assert_eq!(u.scheme(), Scheme::Https);
        assert_eq!(u.port(), 443);
        assert_eq!(u.host_header(), "llm.example.test");
        assert_eq!(u.path_and_query(), "/v1/chat/completions");
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
        let u = Uri::parse("http://h/a%2Fb%20c?q=%41").unwrap();
        assert_eq!(u.path_and_query(), "/a%2Fb%20c?q=%41");
    }

    #[test]
    fn userinfo_is_rejected_rather_than_stripped() {
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
        assert_eq!(Uri::parse("HtTpS://h/p").unwrap().scheme(), Scheme::Https);
    }

    #[test]
    fn ipv6_literals_round_trip_through_brackets() {
        let u = Uri::parse("http://[::1]:8080/p").unwrap();
        assert_eq!(u.host(), "::1");
        assert_eq!(u.port(), 8080);
        assert_eq!(u.host_header(), "[::1]:8080");
        assert_eq!(u.authority(), "[::1]:8080");

        let u = Uri::parse("https://[2606:4700::1111]/p").unwrap();
        assert_eq!(u.host_header(), "[2606:4700::1111]");
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
        assert_eq!(u.path(), "/v1/x");
        assert_eq!(u.path_and_query(), "/v1/x?a=1");

        let u = Uri::parse("http://[::1]:8080/p").unwrap();
        assert_eq!(u.authority_as_written(), "[::1]:8080");
    }
}
