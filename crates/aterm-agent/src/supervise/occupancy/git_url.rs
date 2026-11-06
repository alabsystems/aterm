// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! AN OWNER SOURCE'S REMOTE, AS aterm WILL HAND IT TO git (`docs/DESIGN-harness-occupancy-2026-10-20.md`
//! §3.3; the slice-3 plan's §0 Q3 limits 1, 5 and 6).
//!
//! [`RemoteUrl::parse`] reads `https://`, `ssh://` and scp's `user@host:path`, and refuses
//! everything else: a password or token in the user info (never stripped and read:
//! [`UrlRefusal::Userinfo`]), a query or fragment, `ext::`, `git://`, `http://`, a repository on
//! this Mac ([`UrlRefusal::Local`]), a host, user or path starting with `-` (an option to git or
//! ssh), a port that is not a number, a percent escape, and any control, bidi or invisible
//! character (`hidden`). What it keeps is [`RemoteUrl::stored`]: the only spelling an argv, a
//! record or a journal line may carry. An https user (anonymous https reads none) is dropped from
//! it; an ssh or scp user is kept.
//!
//! A LOCAL URL IS NO REMOTE aterm READS: git reads a repository on this Mac in a child of the GUI,
//! with that repository's own config, and follows its `[include]` paths, its
//! `objects/info/alternates` and its `commondir` (each measured with git 2.56: an include's
//! `uploadpack.hideRefs` hid a ref from `ls-remote file://…`, a fetch read objects only its
//! alternates held, and `ls-remote` of a bare repository holding no ref listed its common dir's).
//! Each is a path no table named and no admission saw, so a read could open one under a
//! privacy-guarded folder and put a dialog on the owner's screen. A list of such redirects would
//! be a registry of git's features, so the transport is refused whole.
//!
//! Pure: nothing here resolves a name or opens a path.

use std::fmt;

/// The longest URL read, in bytes.
pub const URL_MAX: usize = 512;

/// The longest host, in bytes (a DNS name's).
const HOST_MAX: usize = 253;

/// The longest ssh or scp user, in bytes.
const USER_MAX: usize = 32;

/// Why a URL is not read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlRefusal {
    /// The user info carries a password or a token.
    Userinfo,
    /// It has a query (`?`) or a fragment (`#`).
    Query,
    /// Its transport is not one of `https`, `ssh` or scp's form.
    Scheme,
    /// A repository on this Mac (`file://` or an absolute path): git would read it with its own
    /// config's includes, alternates and common dir ([module docs](self)).
    Local,
    /// A host, user or path starts with `-`.
    Dash,
    /// A port that is not a number from 1 to 65535.
    Port,
    /// A control, bidi or invisible character (`hidden`), a space, or a percent escape.
    Control,
    /// Longer than [`URL_MAX`] bytes.
    Long,
    /// Not a URL of its transport's shape (no host, an empty or dotted path segment, a character
    /// its part does not take).
    Shape,
}

impl UrlRefusal {
    /// Every refusal.
    pub const ALL: [UrlRefusal; 9] = [
        UrlRefusal::Userinfo,
        UrlRefusal::Query,
        UrlRefusal::Scheme,
        UrlRefusal::Local,
        UrlRefusal::Dash,
        UrlRefusal::Port,
        UrlRefusal::Control,
        UrlRefusal::Long,
        UrlRefusal::Shape,
    ];

    /// Its word (`refused why=<word>`).
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            UrlRefusal::Userinfo => "userinfo",
            UrlRefusal::Query => "query",
            UrlRefusal::Scheme => "scheme",
            UrlRefusal::Local => "local",
            UrlRefusal::Dash => "dash",
            UrlRefusal::Port => "port",
            UrlRefusal::Control => "control",
            UrlRefusal::Long => "long",
            UrlRefusal::Shape => "shape",
        }
    }
}

/// How a read of the remote authenticates, as the owner's yes names it (S12).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMode {
    /// ssh with a key the owner's ssh-agent holds (no ssh config, no prompt).
    SshAgent,
    /// https with no credential at all.
    AnonymousHttps,
    /// The tests' repository on this Mac ([`RemoteUrl::local_for_tests`]).
    #[cfg(test)]
    Local,
    /// The tests' loopback listener ([`RemoteUrl::loopback_http_for_tests`]).
    #[cfg(test)]
    LoopbackHttp,
}

impl AuthMode {
    /// Its words.
    #[must_use]
    pub const fn words(self) -> &'static str {
        match self {
            AuthMode::SshAgent => "ssh-agent",
            AuthMode::AnonymousHttps => "anonymous https",
            #[cfg(test)]
            AuthMode::Local => "local",
            #[cfg(test)]
            AuthMode::LoopbackHttp => "loopback http",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Transport {
    Https,
    Ssh,
    Scp,
    #[cfg(test)]
    File,
    #[cfg(test)]
    Local,
    #[cfg(test)]
    LoopbackHttp,
}

/// A REMOTE aterm MAY READ ([module docs](self)). Its only spellings are [`Self::stored`] and
/// its parts; nothing it was read from is kept.
#[derive(Clone, PartialEq, Eq)]
pub struct RemoteUrl {
    transport: Transport,
    user: Option<String>,
    host: String,
    port: Option<u16>,
    path: String,
}

impl RemoteUrl {
    /// THE REMOTE `text` NAMES.
    /// # Errors
    /// [`UrlRefusal`]: the first reason it is not read.
    pub fn parse(text: &str) -> Result<Self, UrlRefusal> {
        if text.len() > URL_MAX {
            return Err(UrlRefusal::Long);
        }
        if text.chars().any(hidden) {
            return Err(UrlRefusal::Control);
        }
        if text.contains("::") {
            return Err(UrlRefusal::Scheme);
        }
        if text.chars().any(|c| c.is_whitespace() || c == '%') {
            return Err(UrlRefusal::Control);
        }
        if text.contains('?') || text.contains('#') {
            return Err(UrlRefusal::Query);
        }
        if let Some(rest) = text.strip_prefix("https://") {
            let (user, host, port, path) = authority_and_path(rest)?;
            if let Some(user) = user {
                https_user(&user)?;
            }
            return Ok(Self {
                transport: Transport::Https,
                user: None,
                host,
                port,
                path,
            });
        }
        if let Some(rest) = text.strip_prefix("ssh://") {
            let (user, host, port, path) = authority_and_path(rest)?;
            return Ok(Self {
                transport: Transport::Ssh,
                user: user.as_deref().map(ssh_user).transpose()?,
                host,
                port,
                path,
            });
        }
        if text.starts_with("file://") || text.starts_with('/') {
            return Err(UrlRefusal::Local);
        }
        if text.contains("://") {
            return Err(UrlRefusal::Scheme);
        }
        scp(text)
    }

    /// The tests' repository on this Mac (`file:///…` or `/…`), which [`Self::parse`] refuses
    /// [`UrlRefusal::Local`]: compiled into this crate's own tests and nowhere else, so no shipped
    /// read runs git in a repository on this Mac.
    /// # Errors
    /// [`UrlRefusal::Scheme`] for anything [`Self::parse`] does not refuse as local, and a local
    /// path's own refusal.
    #[cfg(test)]
    pub(crate) fn local_for_tests(text: &str) -> Result<Self, UrlRefusal> {
        if Self::parse(text) != Err(UrlRefusal::Local) {
            return Err(UrlRefusal::Scheme);
        }
        let (transport, rest) = match text.strip_prefix("file://") {
            Some(rest) => (Transport::File, rest),
            None => (Transport::Local, text),
        };
        let path = rest.strip_prefix('/').ok_or(UrlRefusal::Shape)?;
        Ok(Self {
            transport,
            user: None,
            host: String::new(),
            port: None,
            path: segments(path)?,
        })
    }

    /// [`Self::parse`] for the tests' plain-HTTP loopback listener alone (`http://127.0.0.1:<port>/…`):
    /// compiled into this crate's own tests and nowhere else, so no shipped read speaks HTTP.
    /// # Errors
    /// [`UrlRefusal::Scheme`] for anything but a loopback `http://` URL [`Self::parse`] would
    /// read as https.
    #[cfg(test)]
    pub(crate) fn loopback_http_for_tests(text: &str) -> Result<Self, UrlRefusal> {
        let rest = text
            .strip_prefix("http://127.0.0.1:")
            .ok_or(UrlRefusal::Scheme)?;
        let read = Self::parse(&format!("https://127.0.0.1:{rest}"))?;
        Ok(Self {
            transport: Transport::LoopbackHttp,
            ..read
        })
    }

    /// THE ONE SPELLING aterm keeps, journals and hands to git: no password, no token, no https
    /// user, the host in lower case.
    #[must_use]
    pub fn stored(&self) -> String {
        let user = self
            .user
            .as_deref()
            .map_or_else(String::new, |u| format!("{u}@"));
        let port = self.port.map_or_else(String::new, |p| format!(":{p}"));
        match self.transport {
            Transport::Https => format!("https://{}{port}/{}", self.host, self.path),
            #[cfg(test)]
            Transport::LoopbackHttp => format!("http://{}{port}/{}", self.host, self.path),
            Transport::Ssh => format!("ssh://{user}{}{port}/{}", self.host, self.path),
            Transport::Scp => format!("{user}{}:{}", self.host, self.path),
            #[cfg(test)]
            Transport::File => format!("file:///{}", self.path),
            #[cfg(test)]
            Transport::Local => format!("/{}", self.path),
        }
    }

    /// The host, in lower case (empty for the tests' repository on this Mac).
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The path on its host, without its leading `/`.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// How a read authenticates.
    #[must_use]
    pub const fn auth(&self) -> AuthMode {
        match self.transport {
            Transport::Https => AuthMode::AnonymousHttps,
            Transport::Ssh | Transport::Scp => AuthMode::SshAgent,
            #[cfg(test)]
            Transport::File | Transport::Local => AuthMode::Local,
            #[cfg(test)]
            Transport::LoopbackHttp => AuthMode::LoopbackHttp,
        }
    }
}

impl fmt::Debug for RemoteUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RemoteUrl({})", self.stored())
    }
}

/// A CHARACTER NO OWNER-WRITTEN VALUE aterm READS MAY HOLD, as a line or an alert would not show
/// it as written: one a terminal acts on or that reorders the line
/// ([`aterm_codec::tty::is_active`]), any of the General_Category Cc, Cf, Co, Cs or Cn (a
/// control, a format character such as a tag or an interlinear annotation, private use, a
/// surrogate, unassigned), a line or paragraph separator, and every Default_Ignorable_Code_Point
/// ([`default_ignorable`]: the variation selectors, the combining grapheme joiner and the Hangul
/// fillers among them).
pub(crate) fn hidden(c: char) -> bool {
    use aterm_grapheme::GeneralCategory as G;
    aterm_codec::tty::is_active(c)
        || default_ignorable(c)
        || matches!(
            aterm_grapheme::general_category(c),
            G::Control
                | G::Format
                | G::PrivateUse
                | G::Surrogate
                | G::Unassigned
                | G::LineSeparator
                | G::ParagraphSeparator
        )
}

/// Unicode 16.0's Default_Ignorable_Code_Point (DerivedCoreProperties.txt), whole: what a renderer
/// shows nothing for unless it can draw it, whatever its category (a mark, a letter, a format
/// character or one not yet assigned).
pub(crate) const fn default_ignorable(c: char) -> bool {
    matches!(
        c,
        '\u{ad}'
            | '\u{34f}'
            | '\u{61c}'
            | '\u{115f}'..='\u{1160}'
            | '\u{17b4}'..='\u{17b5}'
            | '\u{180b}'..='\u{180f}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{206f}'
            | '\u{3164}'
            | '\u{fe00}'..='\u{fe0f}'
            | '\u{feff}'
            | '\u{ffa0}'
            | '\u{fff0}'..='\u{fff8}'
            | '\u{1bca0}'..='\u{1bca3}'
            | '\u{1d173}'..='\u{1d17a}'
            | '\u{e0000}'..='\u{e0fff}'
    )
}

type Authority = (Option<String>, String, Option<u16>, String);

/// `[user[:secret]@]host[:port]/path` after a scheme.
fn authority_and_path(rest: &str) -> Result<Authority, UrlRefusal> {
    let (authority, path) = rest.split_once('/').ok_or(UrlRefusal::Shape)?;
    let (user, hostport) = match authority.rsplit_once('@') {
        Some((user, hostport)) => (Some(user), hostport),
        None => (None, authority),
    };
    if let Some(user) = user
        && (user.contains(':') || user.contains('@'))
    {
        if super::fault::armed(super::fault::Fault::StripUserinfo) {
            let (host, port) = host_and_port(hostport)?;
            return Ok((None, host, port, segments(path)?));
        }
        return Err(UrlRefusal::Userinfo);
    }
    let (host, port) = host_and_port(hostport)?;
    Ok((user.map(str::to_string), host, port, segments(path)?))
}

fn host_and_port(text: &str) -> Result<(String, Option<u16>), UrlRefusal> {
    let (host, port) = match text.split_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (text, None),
    };
    let host = hostname(host)?;
    let port = match port {
        None => None,
        Some(digits) => {
            let canonical = (1..=5).contains(&digits.len())
                && digits.bytes().all(|b| b.is_ascii_digit())
                && !digits.starts_with('0');
            let n = digits
                .parse::<u16>()
                .ok()
                .filter(|_| canonical)
                .ok_or(UrlRefusal::Port)?;
            Some(n)
        }
    };
    Ok((host, port))
}

fn hostname(host: &str) -> Result<String, UrlRefusal> {
    if host.starts_with('-') {
        return Err(UrlRefusal::Dash);
    }
    let shaped = !host.is_empty()
        && host.len() <= HOST_MAX
        && !host.starts_with('.')
        && !host.ends_with('.')
        && !host.contains("..")
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-');
    if !shaped {
        return Err(UrlRefusal::Shape);
    }
    Ok(host.to_ascii_lowercase())
}

/// An https user: one with no secret is dropped (anonymous https reads none); one that is a
/// token is refused, never dropped and read.
fn https_user(user: &str) -> Result<(), UrlRefusal> {
    if token_shaped(user) {
        return Err(UrlRefusal::Userinfo);
    }
    ssh_user(user).map(|_| ())
}

/// A user name as an https forge's token takes it: a forge's token prefix, or longer than any
/// user name aterm keeps.
fn token_shaped(user: &str) -> bool {
    const PREFIXES: [&str; 10] = [
        "ghp_",
        "gho_",
        "ghu_",
        "ghs_",
        "ghr_",
        "github_pat_",
        "glpat-",
        "x-access-token",
        "x-oauth",
        "oauth2",
    ];
    let folded = user.to_ascii_lowercase();
    folded.len() > USER_MAX || PREFIXES.iter().any(|p| folded.starts_with(p))
}

fn ssh_user(user: &str) -> Result<String, UrlRefusal> {
    if user.starts_with('-') {
        return Err(UrlRefusal::Dash);
    }
    let shaped = (1..=USER_MAX).contains(&user.len())
        && user
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if !shaped {
        return Err(UrlRefusal::Shape);
    }
    Ok(user.to_string())
}

/// A path's segments, without the leading `/`: non-empty, none `.` or `..`, the first not
/// starting with `-`, each of `A-Z a-z 0-9 . _ - ~ +` (so no `@` or `:` hides user info in one).
/// One trailing `/` is dropped; scp's absolute form (`host:/srv/r.git`) is refused, as its
/// first segment is empty (`ssh://host/srv/r.git` names it).
fn segments(path: &str) -> Result<String, UrlRefusal> {
    if path.starts_with('-') {
        return Err(UrlRefusal::Dash);
    }
    let path = path.strip_suffix('/').unwrap_or(path);
    let shaped = !path.is_empty()
        && path.split('/').all(|seg| {
            !seg.is_empty()
                && seg != "."
                && seg != ".."
                && seg.bytes().all(|b| {
                    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'~' | b'+')
                })
        });
    if !shaped {
        return Err(UrlRefusal::Shape);
    }
    Ok(path.to_string())
}

/// scp's `[user@]host:path`.
fn scp(text: &str) -> Result<RemoteUrl, UrlRefusal> {
    let (head, path) = text.split_once(':').ok_or(UrlRefusal::Scheme)?;
    if head.starts_with('-') {
        return Err(UrlRefusal::Dash);
    }
    if path.contains('@') {
        return Err(UrlRefusal::Userinfo);
    }
    let (user, host) = match head.split_once('@') {
        Some((user, host)) => (Some(ssh_user(user)?), host),
        None => (None, head),
    };
    if host.contains('/') {
        return Err(UrlRefusal::Scheme);
    }
    Ok(RemoteUrl {
        transport: Transport::Scp,
        user,
        host: hostname(host)?,
        port: None,
        path: segments(path)?,
    })
}

#[cfg(test)]
#[path = "git_url_tests.rs"]
mod tests;
