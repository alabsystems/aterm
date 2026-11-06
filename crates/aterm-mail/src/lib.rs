// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Mail account identities and transports shared by the native tab and command line.
//! The UI owns selection and drafts; providers own remote message identities.

pub mod crypto;
pub mod human;
pub mod imap;
pub mod mime;
pub mod oauth;
pub mod proton;
pub mod proton_error;
pub mod session;
pub mod smtp;
mod wire;

use std::fmt;

/// A provider's stable folder identity, independent of its display name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Folder {
    pub id: String,
    pub name: String,
}

/// Message identity is local to its account and folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub id: String,
    pub from: String,
    pub to: String,
    pub subject: String,
    pub date: String,
    pub unread: bool,
    pub text: String,
}

/// User-authored outbound message. Bcc is an envelope recipient, never a header.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Draft {
    pub from: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: String,
    pub text: String,
}

/// A credential whose formatting never reveals its value.
#[derive(Clone)]
pub struct Secret(zeroize::Zeroizing<String>);

impl Secret {
    pub fn new(value: String) -> Self {
        Self(zeroize::Zeroizing::new(value))
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

/// Authentication accepted by standard mail servers.
#[derive(Clone, Debug)]
pub enum Authentication {
    Password {
        username: String,
        password: Secret,
    },
    OAuth {
        username: String,
        access_token: Secret,
    },
}

impl Authentication {
    pub(crate) fn plain(&self) -> Option<zeroize::Zeroizing<String>> {
        match self {
            Self::Password { username, password } => {
                let secret = password.expose();
                if username.is_empty()
                    || secret.is_empty()
                    || username.chars().any(char::is_control)
                    || secret.chars().any(char::is_control)
                {
                    return None;
                }
                let mut out = zeroize::Zeroizing::new(String::with_capacity(
                    1 + username.len() + 1 + secret.len(),
                ));
                out.push('\0');
                out.push_str(username);
                out.push('\0');
                out.push_str(secret);
                Some(out)
            }
            Self::OAuth { .. } => None,
        }
    }
    pub(crate) fn xoauth2(&self) -> Option<zeroize::Zeroizing<String>> {
        match self {
            Self::OAuth {
                username,
                access_token,
            } => {
                let token = access_token.expose();
                if username.is_empty()
                    || token.is_empty()
                    || username.chars().any(char::is_control)
                    || token.chars().any(char::is_control)
                {
                    return None;
                }
                let mut out = zeroize::Zeroizing::new(String::with_capacity(
                    5 + username.len() + 13 + token.len() + 2,
                ));
                out.push_str("user=");
                out.push_str(username);
                out.push_str("\x01auth=Bearer ");
                out.push_str(token);
                out.push_str("\x01\x01");
                Some(out)
            }
            Self::Password { .. } => None,
        }
    }
}

fn strip_scheme_ci<'a>(server: &'a str, prefix: &str) -> Option<&'a str> {
    server
        .get(..prefix.len())
        .filter(|head| head.eq_ignore_ascii_case(prefix))
        .map(|_| &server[prefix.len()..])
}

/// TLS is mandatory; STARTTLS never silently falls back to a plaintext session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Security {
    Tls,
    StartTls,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
    pub security: Security,
}

impl Endpoint {
    /// Parse a server name with an optional port and explicit TLS mode.
    /// Standard submission/IMAP upgrade ports require STARTTLS automatically.
    pub fn parse(server: &str, default_port: u16) -> Result<Self, String> {
        let server = server.trim();
        let (server, mode) = if let Some(host) = strip_scheme_ci(server, "starttls://") {
            (host, Some(Security::StartTls))
        } else if let Some(host) = strip_scheme_ci(server, "tls://") {
            (host, Some(Security::Tls))
        } else {
            (server, None)
        };
        let (host, port) = if server.starts_with('[') {
            let (host, suffix) = server.split_once(']').ok_or("Invalid server address")?;
            let ipv6 = &host[1..];
            if ipv6.parse::<std::net::Ipv6Addr>().is_err() {
                return Err("Invalid server address".into());
            }
            let port = if suffix.is_empty() {
                default_port
            } else {
                suffix
                    .strip_prefix(':')
                    .ok_or("Invalid server port")?
                    .parse::<u16>()
                    .map_err(|_| "Invalid server port")?
            };
            if port == 0 {
                return Err("Enter a mail server name and optional port".into());
            }
            (ipv6, port)
        } else {
            let (host, port) = if let Some((host, port)) = server.rsplit_once(':') {
                (
                    host,
                    port.parse::<u16>().map_err(|_| "Invalid server port")?,
                )
            } else {
                (server, default_port)
            };
            if host.is_empty()
                || port == 0
                || host.chars().any(|c| {
                    c.is_control()
                        || c.is_whitespace()
                        || matches!(c, '/' | '?' | '#' | '@' | ':' | '[' | ']')
                })
            {
                return Err("Enter a mail server name and optional port".into());
            }
            (host, port)
        };
        Ok(Self {
            host: host.into(),
            port,
            security: mode.unwrap_or(if matches!(port, 143 | 587) {
                Security::StartTls
            } else {
                Security::Tls
            }),
        })
    }
}

/// Remote operations used identically by every account in the mail tab.
pub trait Mailbox: Send {
    fn folders(&mut self) -> Result<Vec<Folder>, String>;
    fn messages(&mut self, folder: &str, limit: usize) -> Result<Vec<Message>, String>;
    fn read(&mut self, folder: &str, id: &str) -> Result<Message, String>;
    fn mark_read(&mut self, folder: &str, id: &str, read: bool) -> Result<(), String>;
}

#[cfg(test)]
mod endpoint_tests {
    use super::*;
    #[test]
    fn explicit_and_standard_transport_modes() {
        assert_eq!(
            Endpoint::parse("mail.example:587", 465).unwrap().security,
            Security::StartTls
        );
        assert_eq!(
            Endpoint::parse("tls://mail.example:587", 465)
                .unwrap()
                .security,
            Security::Tls
        );
        assert_eq!(
            Endpoint::parse("TLS://mail.example:587", 465)
                .unwrap()
                .security,
            Security::Tls
        );
        assert_eq!(
            Endpoint::parse("STARTTLS://mail.example:993", 993)
                .unwrap()
                .security,
            Security::StartTls
        );
        assert_eq!(Endpoint::parse("[::1]:993", 993).unwrap().host, "::1");
        for invalid in ["", "host:0", "host:65536", "user@host", "host/path", "[::1"] {
            assert!(Endpoint::parse(invalid, 993).is_err(), "{invalid}");
        }
    }

    /// NEGATIVE CONTROL: without rejecting `:` in unbracketed hosts, `2001:db8::587` splits at its
    /// final `:` into host `"2001:db8:"` on port `587` (`StartTls`), and without control-character
    /// checks in `Authentication::{plain, xoauth2}`, `\0` or `\x01` splits SASL credential frames.
    #[test]
    fn unbracketed_ipv6_non_ipv6_brackets_and_sasl_control_delimiters_fail_closed() {
        for invalid in [
            "::1",
            "2001:db8::587",
            "[not-ipv6]:993",
            "[::1]:0",
            "smtp://mail.example:587",
            "host]name:993",
        ] {
            assert!(Endpoint::parse(invalid, 993).is_err(), "{invalid}");
        }

        for (user, secret) in [
            ("", "pw"),
            ("me@example.test", ""),
            ("admin\0me@example.test", "pw"),
            ("me@example.test", "pw\0extra"),
            ("me@example.test\r\n", "pw"),
        ] {
            let pw = Authentication::Password {
                username: user.into(),
                password: Secret::new(secret.into()),
            };
            assert!(pw.plain().is_none(), "{user:?} / {secret:?}");
        }

        for (user, token) in [
            ("", "tok"),
            ("me@example.test", ""),
            ("me@example.test\x01auth=Bearer other", "tok"),
            ("me@example.test", "tok\x01extra"),
            ("me@example.test", "tok\r\n"),
        ] {
            let oauth = Authentication::OAuth {
                username: user.into(),
                access_token: Secret::new(token.into()),
            };
            assert!(oauth.xoauth2().is_none(), "{user:?}");
        }
    }
}
