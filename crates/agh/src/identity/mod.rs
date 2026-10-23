// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Identity token kinds, sources, profiles, credentials, and ambient-token inspection.

pub mod provision;

pub use aterm_http::proxy::{EnvSource, ProcessEnv};

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::Path;

use aterm_uds::secret::Secret;

use crate::policy::{Presence, Tier};

/// GitHub token kind classified by prefix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    /// `ghp_`.
    Classic,
    /// `github_pat_`.
    FineGrained,
    /// `gho_`.
    OAuth,
    /// `ghu_`.
    UserToServer,
    /// `ghs_`.
    Installation,
    /// Any other prefix.
    Unknown,
}

impl TokenKind {
    /// Classifies `token` by prefix.
    ///
    /// # Errors
    /// Returns `LoadError::Unsupported` for refresh tokens (`ghr_`).
    pub fn of(token: &[u8]) -> Result<TokenKind, LoadError> {
        let kind = if token.starts_with(b"ghr_") {
            return Err(LoadError::Unsupported(
                "a refresh token cannot authenticate API calls",
            ));
        } else if token.starts_with(b"ghp_") {
            TokenKind::Classic
        } else if token.starts_with(b"github_pat_") {
            TokenKind::FineGrained
        } else if token.starts_with(b"gho_") {
            TokenKind::OAuth
        } else if token.starts_with(b"ghu_") {
            TokenKind::UserToServer
        } else if token.starts_with(b"ghs_") {
            TokenKind::Installation
        } else {
            TokenKind::Unknown
        };
        Ok(kind)
    }

    /// `classic`, `fine_grained`, `oauth`, `user_to_server`, `installation`, or `unknown`.
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            TokenKind::Classic => "classic",
            TokenKind::FineGrained => "fine_grained",
            TokenKind::OAuth => "oauth",
            TokenKind::UserToServer => "user_to_server",
            TokenKind::Installation => "installation",
            TokenKind::Unknown => "unknown",
        }
    }
}

/// Token storage location for a [`Profile`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Source {
    /// A private token file.
    File(std::path::PathBuf),
    /// An environment variable.
    Env(String),
}

/// Named identity profile from `agh.toml`.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Profile {
    /// The profile's name.
    pub name: String,
    /// Where its token is.
    pub source: Source,
    /// Its tier.
    pub tier: Tier,
    /// The login its token is bound to.
    pub login: Option<String>,
    /// Whether it covers every repository of its owners.
    pub covers_all_repos: bool,
    /// Token fingerprint vouched for via `auth add --drafts`.
    pub drafts_fp: Option<String>,
}

impl Profile {
    /// Creates a profile with `login: None`, `covers_all_repos: false`, and `drafts_fp: None`.
    pub fn new(name: impl Into<String>, source: Source, tier: Tier) -> Profile {
        Profile {
            name: name.into(),
            source,
            tier,
            login: None,
            covers_all_repos: false,
            drafts_fp: None,
        }
    }

    /// The same profile with `login`.
    #[must_use]
    pub fn with_login(mut self, login: impl Into<String>) -> Profile {
        self.login = Some(login.into());
        self
    }

    /// The same profile with `covers_all_repos`.
    #[must_use]
    pub fn with_covers_all_repos(mut self, covers: bool) -> Profile {
        self.covers_all_repos = covers;
        self
    }

    /// The same profile with `drafts_fp`.
    #[must_use]
    pub fn with_drafts_fp(mut self, fingerprint: impl Into<String>) -> Profile {
        self.drafts_fp = Some(fingerprint.into());
        self
    }

    /// Returns whether `cred` is permitted to perform a `WriterRead`.
    #[must_use]
    pub fn drafts_vouched(&self, cred: &Credential) -> bool {
        cred.kind() != TokenKind::FineGrained
            || self.drafts_fp.as_deref() == Some(cred.fingerprint())
    }
}

/// Loaded profile credential holding a `Secret` token.
pub struct Credential {
    profile: String,
    tier: Tier,
    kind: TokenKind,
    fingerprint: String,
    secret: Secret,
    file_id: Option<(u64, u64)>,
}

impl Credential {
    /// The profile's name.
    #[must_use]
    pub fn profile(&self) -> &str {
        &self.profile
    }

    /// The profile's tier.
    #[must_use]
    pub fn tier(&self) -> Tier {
        self.tier
    }

    /// The token's kind.
    #[must_use]
    pub fn kind(&self) -> TokenKind {
        self.kind
    }

    /// The token's fingerprint.
    #[must_use]
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// The token file's `(dev, ino)` as opened; `None` for an env source.
    #[must_use]
    pub fn file_id(&self) -> Option<(u64, u64)> {
        self.file_id
    }

    /// Returns the raw token bytes for `door::AuthHeader` and the redactor.
    pub(crate) fn expose(&self) -> &[u8] {
        self.secret.expose()
    }

    /// A credential from bytes, for tests (no file, no shape check).
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn for_tests(profile: &str, tier: Tier, token: &[u8]) -> Credential {
        Credential {
            profile: profile.to_string(),
            tier,
            kind: TokenKind::of(token).unwrap_or(TokenKind::Unknown),
            fingerprint: fingerprint(token),
            secret: Secret::new(token),
            file_id: None,
        }
    }
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Credential({} {} {} {})",
            self.profile,
            self.tier.slug(),
            self.kind.slug(),
            self.fingerprint
        )
    }
}

/// Why a profile gave no credential.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadError {
    /// No file or no variable (or an empty variable).
    Missing,
    /// `PrivateFileError::NotPrivate`'s text.
    Loose(String),
    /// The file or its directory is a symlink.
    Symlink,
    /// Not one token of the right shape.
    Malformed(&'static str),
    /// A token that cannot authenticate API calls.
    Unsupported(&'static str),
    /// The file could not be read.
    Io(String),
    /// File credentials on a platform without unix permission checks.
    Unavailable(&'static str),
}

impl LoadError {
    /// Maps `Missing` and `Unavailable` to `Presence::Absent` and others to `Presence::Unusable`.
    #[must_use]
    pub fn presence(&self) -> Presence {
        match self {
            LoadError::Missing | LoadError::Unavailable(_) => Presence::Absent,
            other => Presence::Unusable(other.slug()),
        }
    }

    /// `missing`, `loose`, `symlink`, `malformed`, `unsupported`, `io`, `unavailable`.
    #[must_use]
    pub const fn slug(&self) -> &'static str {
        match self {
            LoadError::Missing => "missing",
            LoadError::Loose(_) => "loose",
            LoadError::Symlink => "symlink",
            LoadError::Malformed(_) => "malformed",
            LoadError::Unsupported(_) => "unsupported",
            LoadError::Io(_) => "io",
            LoadError::Unavailable(_) => "unavailable",
        }
    }

    /// Returns the actionable remedy string printed by `auth status` and `whoami`, if any.
    #[must_use]
    pub fn remedy(&self, profile: &Profile, home: &Path) -> Option<String> {
        match self {
            LoadError::Loose(_) => loose_remedy(profile, home),
            LoadError::Symlink => Some(match &profile.source {
                Source::File(file) => token_link_remedy(file, home),
                Source::Env(_) => FILE_LINK.to_string(),
            }),
            LoadError::Malformed(why) => Some(malformed_remedy(why, &profile.source)),
            LoadError::Unsupported(_) => Some(
                "provision a personal access token: a refresh token cannot authenticate API calls"
                    .to_string(),
            ),
            LoadError::Missing | LoadError::Io(_) | LoadError::Unavailable(_) => None,
        }
    }
}

/// The Symlink remedy when the file itself is the link.
const FILE_LINK: &str = "replace the link with the file itself";

/// The Symlink remedy for the token file `file`.
#[cfg(unix)]
fn token_link_remedy(file: &Path, home: &Path) -> String {
    match (
        file.parent(),
        file.file_name().and_then(std::ffi::OsStr::to_str),
    ) {
        (Some(dir), Some(name)) => symlink_remedy(dir, name, home, Held::Token),
        _ => FILE_LINK.to_string(),
    }
}

/// Off unix no token file is read, so none is a link of a directory.
#[cfg(not(unix))]
fn token_link_remedy(file: &Path, home: &Path) -> String {
    let _ = (file, home);
    FILE_LINK.to_string()
}

/// The Malformed remedy for a token_file that names no regular file.
const TOKEN_NOT_A_FILE: &str = "token_file must name a regular file";

/// Which file `read_private` refused, for remedy formatting.
#[cfg(unix)]
#[derive(Clone, Copy, Debug)]
pub(crate) enum Held<'a> {
    /// A profile's `token_file`.
    Token,
    /// `agh.toml` (or `--config` file), with `Dirs::config` when available.
    Config(Option<&'a Path>),
}

#[cfg(unix)]
impl Held<'_> {
    /// Returns whether `dir` is the default `agh` config directory.
    pub(crate) fn is_agh_dir(self, dir: &Path) -> bool {
        use std::os::unix::fs::MetadataExt;
        let Held::Config(Some(agh_dir)) = self else {
            return false;
        };
        let id = |path: &Path| {
            std::fs::metadata(path)
                .ok()
                .map(|meta| (meta.dev(), meta.ino()))
        };
        agh_dir == dir || id(agh_dir).is_some_and(|agh| Some(agh) == id(dir))
    }
}

/// Whether `dir` is one of the directories agh never asks a person to narrow (design §6.2:
/// `$HOME`, `~/.config` and `~/.config/aterm`, and for agh.toml the relocated aterm config
/// directory and its parent).
#[cfg(unix)]
fn never_narrowed(dir: &Path, home: &Path, held: Held<'_>) -> bool {
    use std::os::unix::fs::MetadataExt;
    let id = |path: &Path| {
        std::fs::metadata(path)
            .ok()
            .map(|meta| (meta.dev(), meta.ino()))
    };
    let Some(dir_id) = id(dir) else {
        return false;
    };
    let mut kept = Vec::new();
    if !home.as_os_str().is_empty() {
        let config = home.join(".config");
        kept.extend([home.to_path_buf(), config.join("aterm"), config]);
    }
    if let Held::Config(Some(agh_dir)) = held {
        kept.extend(agh_dir.ancestors().skip(1).take(2).map(Path::to_path_buf));
    }
    kept.iter().any(|path| id(path) == Some(dir_id))
}

/// Formats the `Symlink` remedy for `name` inside `dir`.
#[cfg(unix)]
pub(crate) fn symlink_remedy(dir: &Path, name: &str, home: &Path, held: Held<'_>) -> String {
    use std::os::unix::fs::MetadataExt;
    let Some(link) = std::fs::symlink_metadata(dir)
        .ok()
        .filter(|meta| meta.file_type().is_symlink())
    else {
        return FILE_LINK.to_string();
    };
    let dir_text = tilde(dir, home);
    let real = std::fs::canonicalize(dir).ok();
    let target = real
        .as_ref()
        .map(|real| format!(" to {}", tilde(real, home)))
        .unwrap_or_default();
    let ours = aterm_uds::peer::our_uid();
    if link.uid() == ours {
        return match held {
            Held::Token => format!(
                "the directory {dir_text} is a link{target}: name the file by its real path in \
                 token_file, or make {dir_text} a real directory"
            ),
            Held::Config(_) => {
                format!("the directory {dir_text} is a link{target}: make it a real directory")
            }
        };
    }
    if held.is_agh_dir(dir) {
        let parent_ours = dir
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .and_then(|parent| std::fs::symlink_metadata(parent).ok())
            .is_some_and(|meta| meta.uid() == ours);
        let way = if parent_ours {
            "replace the link with a real directory of yours that only you can open"
        } else {
            "its owner, or root, must replace it with a real directory of yours"
        };
        return format!(
            "the directory {dir_text} is another user's link{target}, and agh reads {name} \
             only there: {way}"
        );
    }
    let real_ours = real
        .as_ref()
        .and_then(|real| std::fs::symlink_metadata(real).ok())
        .is_some_and(|meta| meta.uid() == ours);
    let way = match (held, real.filter(|_| real_ours)) {
        (Held::Token, Some(_)) => "name the file by its real path in token_file".to_string(),
        (Held::Token, None) => {
            "keep the token file in a directory of yours that only you can open, and name it in \
             token_file"
                .to_string()
        }
        (Held::Config(_), Some(real)) => format!(
            "pass --config {}",
            shell_word(&tilde(&real.join(name), home))
        ),
        (Held::Config(_), None) => format!(
            "keep {name} in a directory of yours that only you can open, and pass --config \
             with its path"
        ),
    };
    format!("the directory {dir_text} is another user's link{target}: {way}")
}

/// What one token is, as the Malformed remedies say it.
const ONE_TOKEN: &str = "one token: 20-255 printable ASCII bytes and at most one trailing newline";

/// `aterm_uds::private_file`'s Malformed text for a file longer than the cap.
const TOO_LONG: &str = "longer than such a file can be";

/// The Malformed remedy for `why`: the token's shape for a value of the wrong shape or
/// size, and a regular file for a path that names none.
fn malformed_remedy(why: &str, source: &Source) -> String {
    match source {
        Source::Env(_) => format!("the variable holds {ONE_TOKEN}"),
        Source::File(_) if why == SHAPE || why == TOO_LONG => {
            format!("the token file holds {ONE_TOKEN}")
        }
        Source::File(_) if why == NO_DIRECTORY => {
            "token_file must name a regular file in a directory, by a UTF-8 name".to_string()
        }
        Source::File(_) => TOKEN_NOT_A_FILE.to_string(),
    }
}

/// Formats `path` relative to `home` using `~`.
pub(crate) fn tilde(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) if !home.as_os_str().is_empty() && rest.as_os_str().is_empty() => "~".into(),
        Ok(rest) if !home.as_os_str().is_empty() => format!("~/{}", rest.display()),
        _ => path.display().to_string(),
    }
}

/// `text` as one word of a shell command a person pastes: `~` and a word whose every byte is
/// one of `[A-Za-z0-9_./+-]` (after a leading `~/`) unchanged, else single-quoted with each
/// `'` written `'\''`.
#[cfg(unix)]
fn shell_word(text: &str) -> String {
    if text == "~" {
        return text.to_string();
    }
    let (head, rest) = match text.strip_prefix("~/") {
        Some(rest) => ("~/", rest),
        None => ("", text),
    };
    let plain = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'/' | b'+' | b'-');
    if !rest.is_empty() && rest.bytes().all(plain) {
        return text.to_string();
    }
    format!("{head}'{}'", rest.replace('\'', "'\\''"))
}

/// The Loose remedy: the chmods that make the token file and its directory private, their
/// inverse, and the way that touches no folder of the person's.
#[cfg(unix)]
fn loose_remedy(profile: &Profile, home: &Path) -> Option<String> {
    let Source::File(file) = &profile.source else {
        return None;
    };
    let dir = file.parent()?;
    let file_text = shell_word(&tilde(file, home));
    let provision = format!(
        "agh auth add {} --tier {} --stdin < {file_text}",
        profile.name,
        profile.tier.slug()
    );
    match private_fix(dir, file, home, Held::Token) {
        PrivateFix::Fix(chmods) => Some(format!(
            "{chmods}; or, touching no folder of yours: {provision}"
        )),
        PrivateFix::NotAFile => Some(TOKEN_NOT_A_FILE.to_string()),
        PrivateFix::NoChmod | PrivateFix::Unseen => {
            Some(format!("touching no folder of yours: {provision}"))
        }
    }
}

/// Remedy classification for a file refused by `read_private` as not private.
#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PrivateFix {
    /// Shell commands that make the path private.
    Fix(String),
    /// Path is not a regular file.
    NotAFile,
    /// Refusal cannot be lifted by a user `chmod`.
    NoChmod,
    /// No loose mode bit observed (e.g. ACL or vanished file).
    Unseen,
}

/// Returns the `chmod` remedy string for `file` in `dir`, if any.
#[cfg(unix)]
pub(crate) fn chmod_remedy(dir: &Path, file: &Path, home: &Path) -> Option<String> {
    match private_fix(dir, file, home, Held::Config(None)) {
        PrivateFix::Fix(fix) => Some(fix),
        PrivateFix::NotAFile => Some(format!(
            "{} must be a regular file",
            shell_word(&tilde(file, home))
        )),
        PrivateFix::NoChmod | PrivateFix::Unseen => None,
    }
}

/// Computes the [`PrivateFix`] for `file` in `dir`.
#[cfg(unix)]
pub(crate) fn private_fix(dir: &Path, file: &Path, home: &Path, held: Held<'_>) -> PrivateFix {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let dir_meta = std::fs::symlink_metadata(dir).ok();
    let file_meta = std::fs::symlink_metadata(file).ok();
    if file_meta.as_ref().is_some_and(|meta| {
        let kind = meta.file_type();
        !kind.is_file() && !kind.is_symlink()
    }) {
        return PrivateFix::NotAFile;
    }
    let ours = aterm_uds::peer::our_uid();
    if dir_meta.as_ref().is_some_and(|meta| meta.uid() != ours) {
        return PrivateFix::NoChmod;
    }
    let mode_of = |meta: &std::fs::Metadata| meta.permissions().mode() & 0o7777;
    let loose = |mode: &u32| mode & 0o077 != 0;
    let dir_mode = dir_meta.as_ref().map(mode_of).filter(loose);
    if dir_mode.is_some() && never_narrowed(dir, home, held) {
        return PrivateFix::NoChmod;
    }
    let (dir_text, file_text) = (
        shell_word(&tilde(dir, home)),
        shell_word(&tilde(file, home)),
    );
    let mut fix = Vec::new();
    let mut inverse = Vec::new();
    if let Some(mode) = dir_mode {
        fix.push(format!("chmod 700 {dir_text}"));
        inverse.push(format!("chmod {mode:o} {dir_text}"));
    }
    let mut then = None;
    match &file_meta {
        Some(meta) if meta.file_type().is_file() && (meta.uid() != ours || meta.nlink() > 1) => {
            let copy = shell_word(&format!("{}.new", tilde(file, home)));
            then = Some(format!(
                "replace {file_text} with a private copy of your own: cp {file_text} {copy} \
                 && chmod 600 {copy} && mv {copy} {file_text}"
            ));
        }
        Some(meta) if meta.file_type().is_file() => {
            if let Some(mode) = Some(mode_of(meta)).filter(loose) {
                fix.push(format!("chmod 600 {file_text}"));
                inverse.push(format!("chmod {mode:o} {file_text}"));
            }
        }
        Some(_) => {
            then = Some(format!("replace the link {file_text} with the file itself"));
        }
        None => {}
    }
    let chmods = (!fix.is_empty())
        .then(|| format!("{} (inverse: {})", fix.join(" && "), inverse.join(" && ")));
    match (chmods, then) {
        (Some(chmods), Some(then)) => PrivateFix::Fix(format!("{chmods}; then {then}")),
        (Some(fix), None) | (None, Some(fix)) => PrivateFix::Fix(fix),
        (None, None) => PrivateFix::Unseen,
    }
}

/// Off unix no token file is read, so no file is loose.
#[cfg(not(unix))]
fn loose_remedy(profile: &Profile, home: &Path) -> Option<String> {
    let _ = (profile, home);
    None
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Missing => f.write_str("there is no such file or variable"),
            LoadError::Loose(why) => f.write_str(why),
            LoadError::Malformed(why)
            | LoadError::Unsupported(why)
            | LoadError::Unavailable(why) => f.write_str(why),
            LoadError::Symlink => f.write_str("it or its directory is a symlink"),
            LoadError::Io(e) => write!(f, "it could not be read: {e}"),
        }
    }
}

/// Loads `profile`'s token under strict permission and shape rules.
///
/// # Errors
/// Returns why the profile gave no credential.
pub(crate) fn load(profile: &Profile, env: &dyn EnvSource) -> Result<Credential, LoadError> {
    match &profile.source {
        Source::File(path) => load_file(profile, path),
        Source::Env(var) => load_env(profile, env, var),
    }
}

/// What a token that is not one token of the right shape is refused as.
const SHAPE: &str = "a token is 20-255 printable ASCII bytes and at most one trailing newline";

/// What a token file path with no directory or a non-UTF-8 name is refused as.
const NO_DIRECTORY: &str = "a token file needs a directory and a UTF-8 name";

/// A file source, on unix: `read_private_into` a stack buffer, which is scrubbed whatever the
/// outcome.
#[cfg(unix)]
fn load_file(profile: &Profile, path: &Path) -> Result<Credential, LoadError> {
    use aterm_uds::private_file::{PrivateFileError, read_private_into};
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty());
    let name = path.file_name().and_then(std::ffi::OsStr::to_str);
    let (Some(dir), Some(name)) = (dir, name) else {
        return Err(LoadError::Malformed(NO_DIRECTORY));
    };
    let mut buf = [0u8; crate::config::TOKEN_MAX];
    let loaded = match read_private_into(dir, name, crate::config::TOKEN_MAX, &mut buf) {
        Ok(read) => credential_from(profile, &buf[..read.len], Some((read.dev, read.ino))),
        Err(PrivateFileError::Missing) => Err(LoadError::Missing),
        Err(PrivateFileError::NotPrivate(why)) => Err(LoadError::Loose(why.to_string())),
        Err(PrivateFileError::Symlink) => Err(LoadError::Symlink),
        Err(PrivateFileError::Malformed(why)) => Err(LoadError::Malformed(why)),
        Err(PrivateFileError::Io(e)) => Err(LoadError::Io(e.to_string())),
    };
    aterm_uds::secret::scrub(&mut buf);
    loaded
}

/// A file source, off unix: no permission check, so no file credential.
#[cfg(not(unix))]
fn load_file(profile: &Profile, path: &Path) -> Result<Credential, LoadError> {
    let _ = (profile, path);
    Err(LoadError::Unavailable(
        "file credentials need unix permission checks",
    ))
}

/// An env source: unset or empty is Missing; the value is checked as a file's bytes are, and
/// the copy read from the environment is scrubbed.
fn load_env(profile: &Profile, env: &dyn EnvSource, var: &str) -> Result<Credential, LoadError> {
    let Some(value) = env.var_os(var) else {
        return Err(LoadError::Missing);
    };
    if value.is_empty() {
        return Err(LoadError::Missing);
    }
    let value = match value.into_string() {
        Ok(value) => value,
        Err(raw) => {
            let mut bytes = raw.into_encoded_bytes();
            aterm_uds::secret::scrub(&mut bytes);
            return Err(LoadError::Malformed("the variable is not UTF-8"));
        }
    };
    let mut bytes = value.into_bytes();
    let loaded = credential_from(profile, &bytes, None);
    aterm_uds::secret::scrub(&mut bytes);
    loaded
}

/// Strips at most one trailing `\r\n` or `\n` from `raw`.
pub(crate) fn trim_newline(raw: &[u8]) -> &[u8] {
    raw.strip_suffix(b"\r\n")
        .or_else(|| raw.strip_suffix(b"\n"))
        .unwrap_or(raw)
}

/// Trims one trailing newline from `raw` and validates the 20–255 printable-ASCII token shape.
///
/// # Errors
/// Returns `LoadError::Malformed` if the token fails the shape check.
pub(crate) fn token_bytes(raw: &[u8]) -> Result<&[u8], LoadError> {
    let token = trim_newline(raw);
    let printable = token.iter().all(|b| (0x21..=0x7e).contains(b));
    if !(20..=255).contains(&token.len()) || !printable {
        return Err(LoadError::Malformed(SHAPE));
    }
    Ok(token)
}

/// One token read whole: `token_bytes`, then the kind and the fingerprint.
fn credential_from(
    profile: &Profile,
    raw: &[u8],
    file_id: Option<(u64, u64)>,
) -> Result<Credential, LoadError> {
    let token = token_bytes(raw)?;
    let kind = TokenKind::of(token)?;
    Ok(Credential {
        profile: profile.name.clone(),
        tier: profile.tier,
        kind,
        fingerprint: fingerprint(token),
        secret: Secret::new(token),
        file_id,
    })
}

/// Exposes [`load`] for integration tests.
///
/// # Errors
/// Returns why the profile gave no credential.
#[cfg(any(test, debug_assertions))]
pub fn load_for_tests(profile: &Profile, env: &dyn EnvSource) -> Result<Credential, LoadError> {
    load(profile, env)
}

/// Evaluates [`load`] and returns only its [`Presence`].
#[must_use]
pub fn presence(profile: &Profile, env: &dyn EnvSource) -> Presence {
    match load(profile, env) {
        Ok(_) => Presence::Present,
        Err(e) => e.presence(),
    }
}

/// Returns the first 12 hex characters of `sha256("agh/fp/v1\0" || token)`.
#[must_use]
pub fn fingerprint(token: &[u8]) -> String {
    let mut hasher = aterm_digest::Sha256::new();
    hasher.update(b"agh/fp/v1\0");
    hasher.update(token);
    let digest = hasher.finalize();
    aterm_codec::hex::encode_lower(&digest[..6])
}

/// An ambient token, reported and never used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ambient {
    /// The variable it is in.
    pub var: &'static str,
    /// Its fingerprint.
    pub fingerprint: String,
    /// Its kind; `None` for a token no API call accepts (`ghr_`).
    pub kind: Option<TokenKind>,
}

/// The variables an ambient token is read from, in order.
const AMBIENT_VARS: [&str; 2] = ["GH_TOKEN", "GITHUB_TOKEN"];

/// Inspects `GH_TOKEN` and `GITHUB_TOKEN` when set and non-empty.
#[must_use]
pub fn ambient(env: &dyn EnvSource) -> Vec<Ambient> {
    AMBIENT_VARS
        .iter()
        .filter_map(|var| {
            let mut raw = env.var_os(var)?.into_encoded_bytes();
            let bytes = trim_newline(&raw);
            let ambient = (!bytes.is_empty()).then(|| Ambient {
                var,
                fingerprint: fingerprint(bytes),
                kind: TokenKind::of(bytes).ok(),
            });
            aterm_uds::secret::scrub(&mut raw);
            ambient
        })
        .collect()
}

/// In-memory environment map for tests.
#[derive(Clone, Debug, Default)]
pub struct MapEnv {
    vars: BTreeMap<String, OsString>,
}

impl MapEnv {
    /// An empty environment.
    #[must_use]
    pub fn new() -> MapEnv {
        MapEnv::default()
    }

    /// The same environment with `name` set to `value`.
    #[must_use]
    pub fn with(mut self, name: &str, value: &str) -> MapEnv {
        self.vars.insert(name.to_string(), OsString::from(value));
        self
    }
}

impl EnvSource for MapEnv {
    fn var_os(&self, name: &str) -> Option<OsString> {
        self.vars.get(name).cloned()
    }
}
