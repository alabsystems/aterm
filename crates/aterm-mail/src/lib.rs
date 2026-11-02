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
    pub(crate) fn plain(&self) -> Option<String> {
        match self {
            Self::Password { username, password } => {
                Some(format!("\0{username}\0{}", password.expose()))
            }
            Self::OAuth { .. } => None,
        }
    }
    pub(crate) fn xoauth2(&self) -> Option<String> {
        match self {
            Self::OAuth {
                username,
                access_token,
            } => Some(format!(
                "user={username}\x01auth=Bearer {}\x01\x01",
                access_token.expose()
            )),
            Self::Password { .. } => None,
        }
    }
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
        let (server, mode) = if let Some(host) = server.strip_prefix("starttls://") {
            (host, Some(Security::StartTls))
        } else if let Some(host) = server.strip_prefix("tls://") {
            (host, Some(Security::Tls))
        } else {
            (server, None)
        };
        let (host, port) = if server.starts_with('[') {
            let (host, suffix) = server.split_once(']').ok_or("Invalid server address")?;
            let port = if suffix.is_empty() {
                default_port
            } else {
                suffix
                    .strip_prefix(':')
                    .ok_or("Invalid server port")?
                    .parse::<u16>()
                    .map_err(|_| "Invalid server port")?
            };
            (&host[1..], port)
        } else if let Some((host, port)) = server.rsplit_once(':') {
            (
                host,
                port.parse::<u16>().map_err(|_| "Invalid server port")?,
            )
        } else {
            (server, default_port)
        };
        if host.is_empty()
            || port == 0
            || host
                .chars()
                .any(|c| c.is_control() || c.is_whitespace() || matches!(c, '/' | '?' | '#' | '@'))
        {
            return Err("Enter a mail server name and optional port".into());
        }
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
        assert_eq!(Endpoint::parse("[::1]:993", 993).unwrap().host, "::1");
        for invalid in ["", "host:0", "host:65536", "user@host", "host/path", "[::1"] {
            assert!(Endpoint::parse(invalid, 993).is_err(), "{invalid}");
        }
    }
}
