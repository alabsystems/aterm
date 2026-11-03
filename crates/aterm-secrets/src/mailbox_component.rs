// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Names for individual mailbox credentials, bound to an endpoint and login.
//! The names disclose neither the login nor a credential value.

/// The only registered configuration a component holder may inspect.
pub const SOURCE_ENTRY: &str = "ai-mailbox.toml";
const DOMAIN: &[u8] = b"aterm/mailbox-component/1\0";
const ENTRY_PREFIX: &str = "mailbox-field-";

#[path = "mailbox_component/source.rs"]
mod source;
pub use source::InlineSource;

/// A component is requested only when the consumer observed it was required.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Part {
    Password,
    SecondPassword,
    Totp,
}

impl Part {
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::SecondPassword => "second-password",
            Self::Totp => "totp",
        }
    }

    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "password" => Some(Self::Password),
            "second-password" => Some(Self::SecondPassword),
            "totp" => Some(Self::Totp),
            _ => None,
        }
    }

    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Password => "mailbox login password",
            Self::SecondPassword => "second mailbox password",
            Self::Totp => "mailbox TOTP seed",
        }
    }
}

/// One exact credential of one mailbox on one authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Component {
    identity: [u8; 32],
    part: Part,
}

impl Component {
    /// Bind a known mailbox login to its configured endpoint. Userinfo is not
    /// accepted: callers must use their already validated source authority.
    #[must_use]
    pub fn for_mailbox(endpoint: &str, login: &str, part: Part) -> Option<Self> {
        let authority = canonical_authority(endpoint)?;
        let login = login.trim();
        if login.is_empty()
            || login.len() > 320
            || login.chars().any(|c| c.is_control() || c.is_whitespace())
        {
            return None;
        }
        let mut keyed = DOMAIN.to_vec();
        keyed.extend_from_slice(authority.as_bytes());
        keyed.push(0);
        keyed.extend_from_slice(login.to_ascii_lowercase().as_bytes());
        Some(Self {
            identity: aterm_digest::Sha256::digest(&keyed),
            part,
        })
    }

    #[must_use]
    pub const fn part(self) -> Part {
        self.part
    }

    #[must_use]
    pub fn key(self) -> String {
        format!(
            "mailbox-field:{}/{}",
            aterm_codec::hex::encode_lower(&self.identity),
            self.part.word()
        )
    }

    #[must_use]
    pub fn entry(self) -> String {
        format!(
            "{ENTRY_PREFIX}{}.{}",
            aterm_codec::hex::encode_lower(&self.identity),
            self.part.word()
        )
    }

    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        let (digest, part) = word.strip_prefix("mailbox-field:")?.split_once('/')?;
        Self::from_parts(digest, part)
    }

    #[must_use]
    pub fn from_entry(name: &str) -> Option<Self> {
        let (digest, part) = name.strip_prefix(ENTRY_PREFIX)?.split_once('.')?;
        Self::from_parts(digest, part)
    }

    fn from_parts(digest: &str, part: &str) -> Option<Self> {
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return None;
        }
        let identity = aterm_codec::hex::decode_vec(digest.as_bytes())?
            .try_into()
            .ok()?;
        Some(Self {
            identity,
            part: Part::parse(part)?,
        })
    }
}

/// Canonical scheme, host and effective port. Paths, queries and fragments do
/// not identify an authentication authority; different protocols and ports do.
#[must_use]
pub fn canonical_authority(endpoint: &str) -> Option<String> {
    let (scheme, rest) = endpoint.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    let (transport, default_port) = match scheme.as_str() {
        "https" => ("https", 443),
        "http" => ("http", 80),
        "imaps" => ("https", 993),
        "imap" => ("http", 143),
        _ => return None,
    };
    let uri = aterm_http::Uri::parse(&format!("{transport}://{rest}"))?;
    let host = uri.host().trim_end_matches('.').to_ascii_lowercase();
    let host = host
        .parse::<std::net::IpAddr>()
        .map(|ip| ip.to_string())
        .unwrap_or(host);
    if host.is_empty() {
        return None;
    }
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host
    };
    let port = if uri.has_explicit_port() {
        uri.port()
    } else {
        default_port
    };
    Some(format!("{scheme}://{host}:{port}"))
}

/// Authority of a configured endpoint. IMAP URLs may carry userinfo; strip it
/// only within their authority, then validate the remaining endpoint normally.
/// Component construction itself remains strict and never accepts userinfo.
#[must_use]
pub fn configured_authority(endpoint: &str) -> Option<String> {
    let endpoint = endpoint.trim();
    let (scheme, rest) = endpoint.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("imap") && !scheme.eq_ignore_ascii_case("imaps") {
        return canonical_authority(endpoint);
    }
    if endpoint
        .chars()
        .any(|c| c.is_control() || c.is_whitespace() || c == '\\')
    {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = match authority.rsplit_once('@') {
        Some((userinfo, host)) if !userinfo.is_empty() => host,
        Some(_) => return None,
        None => authority,
    };
    canonical_authority(&format!("{scheme}://{host}"))
}

/// IMAP's explicit URL login takes precedence over the configured fallback.
#[must_use]
pub fn configured_login(endpoint: &str, fallback: &str) -> Option<String> {
    configured_authority(endpoint)?;
    let (scheme, rest) = endpoint.trim().split_once("://")?;
    if scheme.eq_ignore_ascii_case("imap") || scheme.eq_ignore_ascii_case("imaps") {
        let authority = rest.split(['/', '?', '#']).next()?;
        if let Some((userinfo, _)) = authority.rsplit_once('@') {
            let user = userinfo.split_once(':').map_or(userinfo, |(user, _)| user);
            let user = aterm_codec::pct::decode(user);
            return (!user.trim().is_empty()).then_some(user);
        }
    }
    Some(fallback.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identities_bind_authority_and_login_without_disclosing_them() {
        assert_eq!(
            canonical_authority("imaps://[2001:0db8:0:0:0:0:0:1]/INBOX"),
            canonical_authority("imaps://[2001:db8::1]:993/other")
        );
        assert_eq!(
            configured_authority("imaps://owner:fixture@[2001:db8::1]/INBOX"),
            canonical_authority("imaps://[2001:db8::1]:993")
        );
        assert_eq!(
            configured_authority("imaps://embedded-c@gmail.com@imap.gmail.com/Archive"),
            canonical_authority("imaps://imap.gmail.com:993")
        );
        assert_eq!(
            configured_login(
                "imaps://embedded-c@gmail.com@imap.gmail.com/Archive",
                "fallback@example.test"
            )
            .as_deref(),
            Some("embedded-c@gmail.com")
        );
        assert_eq!(
            configured_login(
                "imaps://owner:p@ss@imap.example.test/INBOX",
                "fallback@example.test"
            )
            .as_deref(),
            Some("owner")
        );
        assert!(
            configured_login(
                "imaps://:fixture@imap.example.test/INBOX",
                "fallback@example.test"
            )
            .is_none()
        );
        assert!(configured_authority("imaps://@imap.example.test/INBOX").is_none());
        assert!(configured_authority("https://owner:fixture@mail.example").is_none());
        assert!(configured_authority("imaps://owner:fixture@bad host/").is_none());
        let one = Component::for_mailbox(
            "HTTPS://MAIL.EXAMPLE.:443/inbox?x=1",
            " User@Example ",
            Part::Password,
        )
        .unwrap();
        let same = Component::for_mailbox(
            "https://mail.example/elsewhere",
            "user@example",
            Part::Password,
        )
        .unwrap();
        assert_eq!(one, same);
        for (source, login) in [
            ("https://other.example", "user@example"),
            ("https://mail.example:444", "user@example"),
            ("imaps://mail.example:443", "user@example"),
            ("https://mail.example", "other@example"),
        ] {
            assert_ne!(
                one,
                Component::for_mailbox(source, login, Part::Password).unwrap()
            );
        }
        assert!(!one.key().contains("user") && !one.entry().contains("example"));
        assert!(!format!("{one:?}").contains("user@example"));
        assert_eq!(Component::parse(&one.key()), Some(one));
        assert_eq!(Component::from_entry(&one.entry()), Some(one));
        for part in [Part::Password, Part::SecondPassword, Part::Totp] {
            let component =
                Component::for_mailbox("https://mail.example", "user@example", part).unwrap();
            assert_eq!(Component::parse(&component.key()), Some(component));
            assert_eq!(Component::from_entry(&component.entry()), Some(component));
        }
    }

    #[test]
    fn unknown_or_ambiguous_identities_and_noncanonical_keys_are_refused() {
        for source in [
            "mail.example",
            "ftp://mail.example",
            "https://user:password@mail.example",
            "https://",
            "https://mail.example:0",
        ] {
            assert!(Component::for_mailbox(source, "user@example", Part::Password).is_none());
        }
        for login in ["", " ", "user\n@example", "two users"] {
            assert!(
                Component::for_mailbox("https://mail.example", login, Part::Password).is_none()
            );
        }
        for word in [
            "mailbox-field:00/password",
            "mailbox-field:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff/unknown",
            "mailbox-field:FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF/password",
        ] {
            assert!(Component::parse(word).is_none());
        }
    }
}
