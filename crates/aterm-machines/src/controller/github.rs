// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Host GitHub key registration and revocation ([`login`], the token file
//! [`Paths::github_token`] names, [`System::github_rest`], [`KEY_SCOPE`]), or, by `[github] keys`,
//! over agh's library ([`register_via`], [`revoke_via`]: machines 5a and 5b). The machine user's
//! token is never kept in the login keychain (owner ruling 2026-11-03).

use super::inventory::{Github, KeysVia, Paths};
use crate::github_pages;
use crate::port::{GithubResponse, RestMethod, Secret, System};
use agh::GithubBuilder;
use agh::identity::{InlineToken, ProcessEnv, Profile};
use agh::policy::{RouteRequest, RouteTarget, Router, Tier};
use agh::redact::Redactor;
use agh::retry::RetryPolicy;
use serde::Serialize;
use std::cell::RefCell;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

/// OAuth scope required to list, add, and delete the account's SSH keys.
pub const KEY_SCOPE: &str = "admin:public_key";

/// The key listing, at GitHub's largest page.
const KEYS: &str = "/user/keys?per_page=100";
const KEYS_PER_PAGE: usize = 100;

/// The largest token file read.
const TOKEN_FILE_MAX: usize = 64 * 1024;

/// One key `POST /user/keys` registered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registered {
    /// The host user the key belongs to.
    pub user: String,
    /// GitHub's key id.
    pub id: u64,
}

/// A GitHub login: letters, digits and single hyphens, at most 39 characters.
fn valid_login(account: &str) -> bool {
    !account.is_empty()
        && account.len() <= 39
        && !account.starts_with('-')
        && account
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// An authenticated session as the machine user.
struct Session {
    token: Secret,
}

/// `account`'s token, read from `token_file` ([`Paths::github_token`]): what `aterm machines
/// github login` stored, one token, in a private regular file of this account's (0600) that is
/// itself no link.
fn stored_token(sys: &dyn System, token_file: &Path, account: &str) -> Result<Secret, String> {
    if !valid_login(account) {
        return Err(format!("{account:?} is not a GitHub login"));
    }
    crate::port::no_core_dumps();
    let mut bytes = sys
        .read_owned_file(token_file, sys.euid(), TOKEN_FILE_MAX, true)
        .map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                format!(
                    "no GitHub token for {account} is stored in {} (aterm machines github login \
                     stores one: a token of {account}'s with the {KEY_SCOPE} scope)",
                    token_file.display()
                )
            } else {
                format!(
                    "{} must be a private regular file of this account's (0600), not a link, at \
                     most 64 KiB (aterm machines github login replaces it)",
                    token_file.display()
                )
            }
        })?;
    let token = std::str::from_utf8(&bytes)
        .ok()
        .map(str::trim)
        .filter(|token| !token.is_empty() && !token.contains(char::is_whitespace))
        .map(|token| Secret::new(token.as_bytes()));
    aterm_uds::secret::scrub(&mut bytes);
    match token {
        Some(Ok(token)) => Ok(token),
        _ => Err(format!(
            "{} holds no single GitHub token (aterm machines github login replaces it)",
            token_file.display()
        )),
    }
}

impl Session {
    /// Read `account`'s token from `token_file` and check that it signs in as `account`.
    fn open(sys: &dyn System, token_file: &Path, account: &str) -> Result<Self, String> {
        let token = stored_token(sys, token_file, account)?;
        let session = Self { token };
        let (login, _) = session.login(sys)?;
        if !login.eq_ignore_ascii_case(account) {
            return Err(format!(
                "the stored GitHub token for {account} signs in as {login:?}; nothing was sent \
                 (aterm machines github login replaces it)"
            ));
        }
        Ok(session)
    }

    fn send(
        &self,
        sys: &dyn System,
        method: RestMethod,
        endpoint: &str,
        body: Option<&[u8]>,
    ) -> Result<GithubResponse, String> {
        sys.github_rest(&self.token, method, endpoint, body)
    }

    /// `GET /user`: the login the token signs in as, and the answer's headers.
    fn login(&self, sys: &dyn System) -> Result<(String, Vec<(String, String)>), String> {
        let answer = self.send(sys, RestMethod::Get, "/user", None)?;
        if answer.status != 200 {
            return Err(failure(RestMethod::Get, "/user", &answer));
        }
        let headers = answer.headers.clone();
        let (value, _) = github_pages::page(answer.status, answer.headers, &answer.body)?;
        let login = value["login"]
            .as_str()
            .filter(|login| valid_login(login))
            .ok_or("GitHub GET /user answered no login")?
            .to_owned();
        Ok((login, headers))
    }
}

/// The value of the header `name` (any case) in `headers`.
fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

/// A scope list header (`admin:public_key, read:public_key`).
fn scopes(value: &str) -> impl Iterator<Item = &str> {
    value.split(',').map(str::trim).filter(|s| !s.is_empty())
}

/// The scope the token lacks for an endpoint, when GitHub's answer says.
fn missing_scope(headers: &[(String, String)]) -> Option<&str> {
    let accepted = header(headers, "x-accepted-oauth-scopes")?;
    let held: Vec<&str> = header(headers, "x-oauth-scopes")
        .map(|held| scopes(held).collect())
        .unwrap_or_default();
    let mut accepted = scopes(accepted).peekable();
    let first = *accepted.peek()?;
    (!accepted.any(|scope| held.contains(&scope))).then_some(first)
}

/// The API's own explanation in an error body: the first `errors[].message`, else `message`.
fn api_message(body: &[u8]) -> Option<String> {
    let value: aterm_json::Value = aterm_json::from_slice(body).ok()?;
    value
        .pointer("/errors/0/message")
        .or_else(|| value.pointer("/message"))
        .and_then(|m| m.as_str())
        .map(str::to_owned)
}

/// One line for an answer that is not the one wanted.
fn failure(method: RestMethod, endpoint: &str, answer: &GithubResponse) -> String {
    let mut line = format!("GitHub {method} {endpoint}: HTTP {}", answer.status);
    if let Some(message) = api_message(&answer.body) {
        line.push_str(&format!(" ({message})"));
    }
    if let Some(scope) = missing_scope(&answer.headers) {
        line.push_str(&format!(
            "; the token lacks the {scope} scope (aterm machines github login stores one \
             that holds it)"
        ));
    }
    line
}

/// The keys the machine user holds: `(id, key type, base64 body)`, every page read.
fn listed(sys: &dyn System, session: &Session) -> Result<Vec<(u64, String, String)>, String> {
    let values = github_pages::all(github_pages::API, KEYS, KEYS_PER_PAGE, |endpoint| {
        let answer = session.send(sys, RestMethod::Get, endpoint, None)?;
        if answer.status != 200 {
            return Err(failure(RestMethod::Get, "/user/keys", &answer));
        }
        github_pages::page(answer.status, answer.headers, &answer.body)
    })?;
    values
        .iter()
        .map(|value| {
            let id = value["id"].as_u64().filter(|id| *id != 0);
            let key = value["key"]
                .as_str()
                .and_then(crate::items::github::key_material);
            id.zip(key)
                .map(|(id, (kind, body))| (id, kind, body))
                .ok_or_else(|| {
                    "GitHub GET /user/keys listed an entry that is not an id and an SSH key".into()
                })
        })
        .collect()
}

/// The body of `POST /user/keys`.
#[derive(Serialize)]
struct NewKey<'a> {
    title: &'a str,
    key: &'a str,
}

/// What one registration did: each key registered or found held (`done`), and one line per
/// key that was not, or the one line that stopped them all (`failed`). A key GitHub created
/// is in `done` whatever became of the others, so its id can be recorded and later revoked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Registration {
    /// The keys the account holds now, by host user.
    pub(crate) done: Vec<Registered>,
    /// Why each other key is not registered.
    pub(crate) failed: Vec<String>,
}

impl Registration {
    /// One line that stopped every key.
    fn stopped(why: String) -> Self {
        Self {
            done: Vec::new(),
            failed: vec![why],
        }
    }

    /// `register`'s answer: `done`, or every failure joined by `; ` when any key failed.
    ///
    /// # Errors
    /// The failures, joined.
    pub(crate) fn into_result(self) -> Result<Vec<Registered>, String> {
        if self.failed.is_empty() {
            Ok(self.done)
        } else {
            Err(self.failed.join("; "))
        }
    }
}

/// Register each `github.key@<user>` public key in `report` not yet held by `account`, with
/// the token `token_file` holds.
///
/// # Errors
/// One line (no token for `account`, the token is for another login, the API refused).
pub fn register(
    sys: &dyn System,
    token_file: &Path,
    account: &str,
    host_name: &str,
    keys: &[(String, String)],
) -> Result<Vec<Registered>, String> {
    register_each(sys, token_file, account, host_name, keys).into_result()
}

/// [`register`], each key's outcome kept.
fn register_each(
    sys: &dyn System,
    token_file: &Path,
    account: &str,
    host_name: &str,
    keys: &[(String, String)],
) -> Registration {
    if keys.is_empty() {
        return Registration::default();
    }
    let wanted = match wanted(keys) {
        Ok(wanted) => wanted,
        Err(why) => return Registration::stopped(why),
    };
    let opened = Session::open(sys, token_file, account)
        .and_then(|session| listed(sys, &session).map(|held| (session, held)));
    let (session, mut held) = match opened {
        Ok(opened) => opened,
        Err(why) => return Registration::stopped(why),
    };
    let mut done = Vec::new();
    let mut failed = Vec::new();
    for (user, kind, body) in wanted {
        if let Some((id, ..)) = held.iter().find(|(_, k, b)| *k == kind && *b == body) {
            done.push(Registered {
                user: user.clone(),
                id: *id,
            });
            continue;
        }
        let title = crate::items::github::key_title(host_name, user);
        let key = format!("{kind} {body}");
        let new = match aterm_json::to_vec(&NewKey {
            title: &title,
            key: &key,
        }) {
            Ok(new) => new,
            Err(e) => {
                failed.push(format!("{user}: {e}"));
                continue;
            }
        };
        let posted = session.send(sys, RestMethod::Post, "/user/keys", Some(&new));
        match posted {
            Ok(answer) if answer.status == 201 => {
                let id = aterm_json::from_slice::<aterm_json::Value>(&answer.body)
                    .ok()
                    .and_then(|value| value["id"].as_u64())
                    .filter(|id| *id != 0);
                match id {
                    Some(id) => {
                        held.push((id, kind, body));
                        done.push(Registered {
                            user: user.clone(),
                            id,
                        });
                    }
                    None => failed.push(format!("{user}: GitHub returned no key id")),
                }
            }
            Ok(answer) => failed.push(format!(
                "{user}: {}",
                failure(RestMethod::Post, "/user/keys", &answer)
            )),
            Err(why) => failed.push(format!("{user}: {why}")),
        }
    }
    Registration { done, failed }
}

fn dedup_ids(ids: &[u64]) -> Vec<u64> {
    let mut unique = Vec::with_capacity(ids.len());
    for &id in ids {
        if !unique.contains(&id) {
            unique.push(id);
        }
    }
    unique
}

/// Delete each key id from the machine user `account`, with the token `token_file` holds, and
/// read the account's keys back.
///
/// # Errors
/// One line per key the account still holds, or why the keys could not be listed.
pub fn revoke(
    sys: &dyn System,
    token_file: &Path,
    account: &str,
    ids: &[u64],
) -> Result<Vec<String>, String> {
    let ids = dedup_ids(ids);
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let session = Session::open(sys, token_file, account)?;
    let listing = listed(sys, &session)?;
    let before: Vec<u64> = listing.iter().map(|(id, ..)| *id).collect();
    let deleted: Vec<String> = listing
        .iter()
        .filter(|(id, ..)| ids.contains(id))
        .map(|(_, kind, body)| format!("{kind} {body}"))
        .collect();
    let mut answers: Vec<(u64, String)> = Vec::new();
    for id in ids.iter().copied().filter(|id| before.contains(id)) {
        let path = format!("/user/keys/{id}");
        let answer = match session.send(sys, RestMethod::Delete, &path, None) {
            Ok(answer) if answer.status == 204 => continue,
            Ok(answer) => failure(RestMethod::Delete, &path, &answer),
            Err(e) => e,
        };
        answers.push((id, answer));
    }
    let after: Vec<u64> = listed(sys, &session)?
        .into_iter()
        .map(|(id, ..)| id)
        .collect();
    judged(&ids, &after, &answers)?;
    Ok(deleted)
}

/// Each key's user, type and base64 body, or the first key that is not an OpenSSH public key.
fn wanted(keys: &[(String, String)]) -> Result<Vec<(&String, String, String)>, String> {
    keys.iter()
        .map(|(user, line)| {
            crate::items::github::key_material(line)
                .map(|(kind, body)| (user, kind, body))
                .ok_or_else(|| format!("{user}'s key is not an OpenSSH public key"))
        })
        .collect()
}

/// A revocation judged by the listing read after it (`after`), never by a DELETE's answer:
/// one line per id still listed, carrying its DELETE's failure (`answers`) where one was kept.
fn judged(ids: &[u64], after: &[u64], answers: &[(u64, String)]) -> Result<(), String> {
    let failed: Vec<String> = ids
        .iter()
        .filter(|id| after.contains(id))
        .map(|id| {
            let answer = answers
                .iter()
                .find(|(a, _)| a == id)
                .map_or("DELETE answered success", |(_, why)| why.as_str());
            format!("key {id}: still registered ({answer})")
        })
        .collect();
    if failed.is_empty() {
        Ok(())
    } else {
        Err(failed.join("; "))
    }
}

/// Validate `token` against `account` and [`KEY_SCOPE`], then store it in `token_file`
/// ([`Paths::github_token`], 0600), never the login keychain.
///
/// # Errors
/// One line (not a login, the token is empty or holds whitespace, GitHub refused it, it is
/// another account's or lacks the scope, the file could not be written, or it did not hand the
/// token back). Nothing is stored on an error before the write, and no error carries the token.
pub fn login(
    sys: &dyn System,
    token_file: &Path,
    account: &str,
    token: Secret,
) -> Result<(), String> {
    if !valid_login(account) {
        return Err(format!(
            "{account:?} is not a GitHub login; nothing was stored"
        ));
    }
    if token.bytes().is_empty() || token.bytes().iter().any(u8::is_ascii_whitespace) {
        return Err("the token is not one word; nothing was stored".into());
    }
    let session = Session { token };
    let (login, headers) = session
        .login(sys)
        .map_err(|why| format!("{why}; nothing was stored"))?;
    if !login.eq_ignore_ascii_case(account) {
        return Err(format!(
            "the token signs in as {login:?}, not {account}; nothing was stored"
        ));
    }
    if let Some(held) = header(&headers, "x-oauth-scopes")
        && !scopes(held).any(|scope| scope == KEY_SCOPE)
    {
        return Err(format!(
            "the token lacks the {KEY_SCOPE} scope that adding and deleting {account}'s SSH \
             keys needs; nothing was stored"
        ));
    }
    if let Some(keys) = token_file.parent() {
        sys.create_dir_all(keys, 0o700)
            .map_err(|e| format!("{}: {e}; nothing was stored", keys.display()))?;
    }
    sys.write_file(token_file, session.token.bytes(), 0o600)
        .map_err(|e| format!("{}: {e}", token_file.display()))?;
    let stored = stored_token(sys, token_file, account)?;
    if stored.bytes() != session.token.bytes() {
        return Err(format!(
            "{} did not hand the token back as it was written",
            token_file.display()
        ));
    }
    Ok(())
}

/// The name of the profile the agh key path speaks as, inline (5a) or from agh.toml (5b).
const FLEET: &str = "fleet";

/// The single-identity router of the agh key path `github.keys_via()` names (plan §17): the
/// token file's token inline (`AghInline`), or agh.toml's `fleet` profile (`AghFleet`). Each
/// refusal of its own ends `nothing was sent`; the token file's read refuses with
/// `stored_token`'s texts, unchanged (body 3).
fn agh_router(sys: &dyn System, token_file: &Path, github: &Github) -> Result<Router, String> {
    let account = &github.account;
    match github.keys_via() {
        KeysVia::Gh => Err(format!(
            "keys = \"gh\" sends {account}'s key calls over the REST path, not agh; nothing was \
             sent"
        )),
        KeysVia::AghInline => {
            let token = stored_token(sys, token_file, account)?;
            let token =
                InlineToken::new(aterm_uds::secret::Secret::new(token.bytes())).map_err(|e| {
                    format!(
                        "the stored token for {account} cannot be used by agh ({}); nothing was \
                         sent",
                        e.slug()
                    )
                })?;
            // agh binds the profile to `account`: its first call is `GET /user`, and a token
            // that signs in as another login is refused before any key call.
            let profile = Profile::inline(FLEET, Tier::Dev, account.clone(), token)
                .map_err(|e| format!("{}; nothing was sent", Redactor::shapes(&e.why)))?;
            Ok(Router::single_identity(profile))
        }
        KeysVia::AghFleet => {
            if !valid_login(account) {
                return Err(format!(
                    "{account:?} is not a GitHub login; nothing was sent"
                ));
            }
            let dirs = sys.agh_dirs().ok_or(
                "agh's directories cannot be resolved; keys = \"agh-fleet\" reads agh.toml's \
                 fleet profile; nothing was sent",
            )?;
            let validated = agh::config::load(&dirs)
                .map_err(|e| {
                    format!(
                        "agh.toml is invalid: {}; nothing was sent",
                        Redactor::shapes(&e.why)
                    )
                })?
                .ok_or_else(|| {
                    format!(
                        "agh.toml is missing ({}); keys = \"agh-fleet\" reads its fleet profile; \
                         nothing was sent",
                        dirs.config_file().display()
                    )
                })?;
            let profile = Router::from_validated(validated)
                .profile(FLEET)
                .cloned()
                .ok_or(
                    "agh.toml has no fleet profile: agh auth add fleet adds the machine user's \
                     token; nothing was sent",
                )?;
            match profile.login.as_deref() {
                Some(login) if login.eq_ignore_ascii_case(account) => {}
                bound => {
                    return Err(format!(
                        "agh.toml's fleet profile is bound to {}, not {account}; nothing was sent",
                        bound.unwrap_or("no login")
                    ));
                }
            }
            Ok(Router::single_identity(profile))
        }
    }
}

/// One agh client of the key path `github.keys_via()` names, the one identity write permit
/// its key writes share, and whose token its error lines speak of.
struct AghKeys {
    gh: agh::Github,
    permit: agh::Permit,
    /// The account, on the inline path (5a), whose token is the token file's.
    inline: Option<String>,
}

impl AghKeys {
    /// Open the client. Under `AghFleet`, `GET /user` checks that the token agh.toml's profile
    /// reads signs in as `github.account` before any key call, as the REST path's session and
    /// the inline path's login binding check theirs: agh binds only an inline profile's login
    /// (plan §17, As built).
    fn open(sys: &dyn System, token_file: &Path, github: &Github) -> Result<Self, String> {
        let router = agh_router(sys, token_file, github)?;
        let (transport, endpoints) = sys.github_transport();
        let gh = GithubBuilder::new(router)
            .transport(transport)
            .endpoints(endpoints)
            .env(Arc::new(ProcessEnv))
            .fx(sys.github_fx())
            .retry(RetryPolicy::STANDARD)
            .caller("aterm-machines")
            .build();
        let account = &github.account;
        let via = github.keys_via();
        if via == KeysVia::AghFleet {
            let me = gh
                .users()
                .me(FLEET)
                .map_err(|e| agh_line(&gh, "GET /user", &e))?;
            if !me.login.eq_ignore_ascii_case(account) {
                return Err(gh.redactor().apply(&format!(
                    "agh.toml's fleet token signs in as {}, not {account}; nothing was sent",
                    me.login
                )));
            }
        }
        let permit = gh
            .permit(&RouteRequest::write(RouteTarget::Identity))
            .map_err(|e| agh_line(&gh, "route /user/keys", &e))?;
        let inline = (via == KeysVia::AghInline).then(|| account.clone());
        Ok(Self { gh, permit, inline })
    }

    /// One line for a key call's failure, as `agh_line`. On the inline path a token agh
    /// refuses as unauthenticated (another login's, or one GitHub answers 401) names the verb
    /// that replaces the token file's token, never agh's own, which cannot see it: another
    /// login's token gets the REST path's text. A token GitHub says lacks `admin:public_key`
    /// keeps agh's scope hint and also names that verb, which stores the re-issued token.
    fn line(&self, what: &str, e: &agh::Error) -> String {
        let Some(account) = self.inline.as_deref() else {
            return agh_line(&self.gh, what, e);
        };
        let line = if e.kind == agh::Kind::Unauthenticated {
            match &e.sub {
                agh::Sub::LoginMismatch { got, .. } => format!(
                    "the stored GitHub token for {account} signs in as {got:?}; nothing was \
                     sent (aterm machines github login replaces it)"
                ),
                _ => format!(
                    "{what}: {}; aterm machines github login replaces the stored token for \
                     {account} (a token of {account}'s with the {KEY_SCOPE} scope)",
                    e.why
                ),
            }
        } else if e.next.contains(KEY_SCOPE) {
            format!(
                "{what}: {}; {}; aterm machines github login stores the re-issued token for \
                 {account}",
                e.why, e.next
            )
        } else {
            return agh_line(&self.gh, what, e);
        };
        self.gh.redactor().apply(&line)
    }

    /// The keys the client's identity holds: `(id, key type, base64 body)`, every page read.
    fn listed(&self) -> Result<Vec<(u64, String, String)>, String> {
        let keys = self
            .gh
            .users()
            .keys()
            .list()
            .map_err(|e| self.line("GET /user/keys", &e))?;
        keys.into_iter()
            .map(|held| {
                if held.id == 0 {
                    return Err("GET /user/keys: GitHub listed key id 0".into());
                }
                crate::items::github::key_material(&held.key)
                    .map(|(kind, body)| (held.id, kind, body))
                    .ok_or_else(|| {
                        format!(
                            "GET /user/keys: key {} is not an OpenSSH public key",
                            held.id
                        )
                    })
            })
            .collect()
    }
}

/// One line for an agh call's failure, `{what}: {why}; {next}`, through the client's redactor.
/// A 404 whose scope headers show the token lacks `admin:public_key` names that scope in its
/// next (design §16.7).
fn agh_line(gh: &agh::Github, what: &str, e: &agh::Error) -> String {
    let line = if e.next.is_empty() {
        format!("{what}: {}", e.why)
    } else {
        format!("{what}: {}; {}", e.why, e.next)
    };
    gh.redactor().apply(&line)
}

/// The key calls of one controller command (plan §17, As built). Over agh it opens one client
/// and one identity write permit at its first call and shares them with every call after, so
/// agh paces all of the command's key writes 1 s apart (§4.6.5), a rotation's DELETE after its
/// POST and the next host's POST included, and binds the inline token's login once. A failed
/// opening is not kept: the next call opens again. The REST path (`KeysVia::Gh`) opens its
/// session per call, unchanged.
pub(crate) struct KeyCalls<'s> {
    sys: &'s dyn System,
    /// The machine user's token file ([`Paths::github_token`]).
    token_file: PathBuf,
    /// The client opened for an account and a key path, and its permit.
    agh: RefCell<Option<(String, KeysVia, Rc<AghKeys>)>>,
}

impl<'s> KeyCalls<'s> {
    /// No client opened yet; the token from the controller's token file.
    pub(crate) fn new(sys: &'s dyn System, paths: &Paths) -> Self {
        Self {
            sys,
            token_file: paths.github_token(),
            agh: RefCell::new(None),
        }
    }

    /// The agh client for `github`'s account and key path, opened at the first call.
    fn agh(&self, github: &Github) -> Result<Rc<AghKeys>, String> {
        let via = github.keys_via();
        if let Some((account, held, keys)) = &*self.agh.borrow()
            && *account == github.account
            && *held == via
        {
            return Ok(Rc::clone(keys));
        }
        let keys = Rc::new(AghKeys::open(self.sys, &self.token_file, github)?);
        *self.agh.borrow_mut() = Some((github.account.clone(), via, Rc::clone(&keys)));
        Ok(keys)
    }

    /// [`register`] by the path `github.keys_via()` names, each key's outcome kept: `Gh` is
    /// the REST path, unchanged; `AghInline` and `AghFleet` the agh path (§17).
    pub(crate) fn register(
        &self,
        github: &Github,
        host_name: &str,
        keys: &[(String, String)],
    ) -> Registration {
        match github.keys_via() {
            KeysVia::Gh => {
                register_each(self.sys, &self.token_file, &github.account, host_name, keys)
            }
            KeysVia::AghInline | KeysVia::AghFleet => self.register_agh(github, host_name, keys),
        }
    }

    /// [`revoke`] by the path `github.keys_via()` names, as `register`.
    ///
    /// # Errors
    ///
    /// As `revoke`, or the agh path's refusal.
    pub(crate) fn revoke(&self, github: &Github, ids: &[u64]) -> Result<Vec<String>, String> {
        match github.keys_via() {
            KeysVia::Gh => revoke(self.sys, &self.token_file, &github.account, ids),
            KeysVia::AghInline | KeysVia::AghFleet => self.revoke_agh(github, ids),
        }
    }

    /// [`register`] over agh: the same algorithm, its writes paced 1 s apart by agh.
    fn register_agh(
        &self,
        github: &Github,
        host_name: &str,
        keys: &[(String, String)],
    ) -> Registration {
        if keys.is_empty() {
            return Registration::default();
        }
        let wanted = match wanted(keys) {
            Ok(wanted) => wanted,
            Err(why) => return Registration::stopped(why),
        };
        let opened = self
            .agh(github)
            .and_then(|agh| agh.listed().map(|held| (agh, held)));
        let (agh, mut held) = match opened {
            Ok(opened) => opened,
            Err(why) => return Registration::stopped(why),
        };
        let calls = agh.gh.users().keys();
        let mut done = Vec::new();
        let mut failed = Vec::new();
        for (user, kind, body) in wanted {
            if let Some((id, ..)) = held.iter().find(|(_, k, b)| *k == kind && *b == body) {
                done.push(Registered {
                    user: user.clone(),
                    id: *id,
                });
                continue;
            }
            let title = crate::items::github::key_title(host_name, user);
            match calls.add(&agh.permit, &title, &format!("{kind} {body}")) {
                Ok(agh::Ensured { object, .. }) if object.id != 0 => {
                    held.push((object.id, kind, body));
                    done.push(Registered {
                        user: user.clone(),
                        id: object.id,
                    });
                }
                Ok(_) => failed.push(format!(
                    "{user}: POST /user/keys: GitHub returned no key id"
                )),
                Err(e) => failed.push(format!("{user}: {}", agh.line("POST /user/keys", &e))),
            }
        }
        Registration { done, failed }
    }

    /// [`revoke`] over agh: each listed id deleted, then the revocation judged by a second
    /// listing.
    fn revoke_agh(&self, github: &Github, ids: &[u64]) -> Result<Vec<String>, String> {
        let ids = dedup_ids(ids);
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let agh = self.agh(github)?;
        let listing = agh.listed()?;
        let before: Vec<u64> = listing.iter().map(|(id, ..)| *id).collect();
        let deleted: Vec<String> = listing
            .iter()
            .filter(|(id, ..)| ids.contains(id))
            .map(|(_, kind, body)| format!("{kind} {body}"))
            .collect();
        let calls = agh.gh.users().keys();
        let mut answers: Vec<(u64, String)> = Vec::new();
        for id in ids.iter().copied().filter(|id| before.contains(id)) {
            if let Err(e) = calls.delete(&agh.permit, id) {
                answers.push((id, agh.line(&format!("DELETE /user/keys/{id}"), &e)));
            }
        }
        let after: Vec<u64> = agh.listed()?.into_iter().map(|(id, ..)| id).collect();
        judged(&ids, &after, &answers)?;
        Ok(deleted)
    }
}

/// [`register`] by the path `github.keys_via()` names, with the token file `paths` names:
/// `Gh` is `register(sys, &paths.github_token(), &github.account, host_name, keys)`;
/// `AghInline` and `AghFleet` are the agh path (§17).
///
/// # Errors
///
/// As `register`, or the agh path's refusal, one line naming the key call.
pub fn register_via(
    sys: &dyn System,
    paths: &Paths,
    github: &Github,
    host_name: &str,
    keys: &[(String, String)],
) -> Result<Vec<Registered>, String> {
    KeyCalls::new(sys, paths)
        .register(github, host_name, keys)
        .into_result()
}

/// [`revoke`] by the path `github.keys_via()` names, as `register_via`.
///
/// # Errors
///
/// As `revoke`, or the agh path's refusal.
pub fn revoke_via(
    sys: &dyn System,
    paths: &Paths,
    github: &Github,
    ids: &[u64],
) -> Result<Vec<String>, String> {
    KeyCalls::new(sys, paths).revoke(github, ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::port::{Fake, Stat};

    /// Made up; never a real token.
    const TOKEN: &str = "fixture_machine_user_token_only";

    /// The controller's token file in these tests.
    const TOKEN_FILE: &str = "/controller/keys/github_token";

    /// The key the VM's admin made (see items::github's fixtures).
    const ALICE_KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIJpFPtSPTOuLIj6oqQTj8ndNDVNHFp4renP47+wxFYao aterm-machines:lab-07:alice";

    /// GitHub's own example ed25519 key shape, as `GET /user/keys` returns keys.
    const BOB_BODY: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl";

    /// Two more made-up ed25519 bodies, for keys the account holds for other hosts.
    const OTHER_BODIES: [&str; 2] = [
        "AAAAC3NzaC1lZDI1NTE5AAAAIEf/eQTWeJTgxWM6+tuR3GYdddTaYJfSbQ+2Vr/R0C/7",
        "AAAAC3NzaC1lZDI1NTE5AAAAIKClQs4JkZ6qk49IjNe40mEc2+O0ppqqJHbHKh7uv2yZ",
    ];

    /// GitHub `404 Not Found` body for `GET user/keys/1`.
    const NOT_FOUND_BODY: &str = r#"{"message":"Not Found","documentation_url":"https://docs.github.com/rest/users/keys#get-a-public-ssh-key-for-the-authenticated-user","status":"404"}"#;

    /// The body GitHub sent for `GET user` with a made-up token, captured byte for byte.
    const BAD_CREDENTIALS_BODY: &str = "{\r\n  \"message\": \"Bad credentials\",\r\n  \"documentation_url\": \"https://docs.github.com/rest\",\r\n  \"status\": \"401\"\r\n}";

    /// What GitHub names for the key endpoints in `X-Accepted-OAuth-Scopes`.
    const ACCEPTED: (&str, &str) = (
        "X-Accepted-OAuth-Scopes",
        "admin:public_key, read:public_key, write:public_key",
    );

    const FIRST_PAGE: &str = "/user/keys?per_page=100&page=1";

    fn answer(status: u16, headers: &[(&str, &str)], body: &str) -> Result<GithubResponse, String> {
        Ok(GithubResponse {
            status,
            headers: headers
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
            body: body.as_bytes().to_vec(),
        })
    }

    fn keys(held: &[(u64, &str)]) -> String {
        let keys: Vec<_> = held
            .iter()
            .map(|(id, body)| aterm_json::json!({"id": id, "key": format!("ssh-ed25519 {body}")}))
            .collect();
        aterm_json::to_string(&keys).unwrap()
    }

    /// The listing's next answer: the account holds `held`, on one page.
    fn lists(sys: &Fake, held: &[(u64, &str)]) {
        sys.on_github_rest(RestMethod::Get, FIRST_PAGE, answer(200, &[], &keys(held)));
    }

    fn token_file() -> &'static Path {
        Path::new(TOKEN_FILE)
    }

    /// `bytes` in the controller's token file, with `mode`, owned by this account.
    fn stored(sys: &Fake, bytes: &str, mode: u32) {
        sys.file(TOKEN_FILE, bytes.as_bytes()).stat_of(
            TOKEN_FILE,
            Stat {
                uid: sys.euid,
                gid: 0,
                mode,
                dir: false,
                symlink: false,
                len: bytes.len() as u64,
            },
        );
    }

    /// A controller whose token file holds a token for `fleetbot` that signs in as `login`.
    fn controller(login: &str) -> Fake {
        let sys = Fake::default();
        stored(&sys, &format!("{TOKEN}\n"), 0o600);
        sys.on_github_rest(
            RestMethod::Get,
            "/user",
            answer(200, &[], &format!(r#"{{"login":"{login}"}}"#)),
        );
        sys
    }

    fn posted(user: &str, body: &str) -> String {
        format!(
            r#"github POST /user/keys {{"title":"aterm-machines:lab-07:{user}","key":"ssh-ed25519 {body}"}}"#
        )
    }

    /// No call carries the token, none is gh, and none runs git: the login keychain is never
    /// asked.
    fn clean(sys: &Fake) {
        let calls = sys.calls();
        assert!(
            calls.iter().all(|c| !c.contains(TOKEN)
                && !c.contains("/gh ")
                && !c.contains("GH_TOKEN")
                && !c.contains("/git ")
                && !c.contains("credential")
                && !c.contains("osxkeychain")),
            "{calls:?}"
        );
    }

    #[test]
    fn a_new_key_is_posted_and_one_already_held_is_only_recorded() {
        let sys = controller("fleetbot");
        lists(
            &sys,
            &[
                (100_000_101, OTHER_BODIES[0]),
                (100_000_102, BOB_BODY),
                (100_000_103, OTHER_BODIES[1]),
            ],
        );
        sys.on_github_rest(
            RestMethod::Post,
            "/user/keys",
            answer(201, &[], r#"{"id":100000104,"key":"ssh-ed25519 x"}"#),
        );
        let keys = [
            ("alice".to_owned(), ALICE_KEY.to_owned()),
            (
                "bob".to_owned(),
                format!("ssh-ed25519 {BOB_BODY} aterm-machines:lab-07:bob"),
            ),
        ];
        let done = register(&sys, token_file(), "fleetbot", "lab-07", &keys).unwrap();
        assert_eq!(
            done,
            [
                Registered {
                    user: "alice".into(),
                    id: 100_000_104
                },
                Registered {
                    user: "bob".into(),
                    id: 100_000_102
                },
            ]
        );
        let calls = sys.calls();
        let alice_body = ALICE_KEY.split(' ').nth(1).unwrap();
        assert!(calls.contains(&posted("alice", alice_body)), "{calls:?}");
        assert_eq!(
            calls
                .iter()
                .filter(|c| c.starts_with("github POST"))
                .count(),
            1
        );
        assert_eq!(calls[0], "github GET /user");
        clean(&sys);
    }

    #[test]
    fn a_key_held_on_a_later_page_is_found_and_a_page_cut_short_sends_nothing() {
        let next = format!("<https://api.github.com{KEYS}&page=2>; rel=\"next\"");
        let first: Vec<(u64, &str)> = (1..=100).map(|id| (id, OTHER_BODIES[0])).collect();
        for cut in [false, true] {
            let sys = controller("fleetbot");
            sys.on_github_rest(
                RestMethod::Get,
                FIRST_PAGE,
                answer(200, &[("Link", &next)], &keys(&first)),
            );
            sys.on_github_rest(
                RestMethod::Get,
                "/user/keys?per_page=100&page=2",
                if cut {
                    Err("GitHub GET /user/keys?per_page=100&page=2: the body was cut short".into())
                } else {
                    answer(200, &[], &keys(&[(101, BOB_BODY)]))
                },
            );
            let bob = [(
                "bob".to_owned(),
                format!("ssh-ed25519 {BOB_BODY} aterm-machines:lab-07:bob"),
            )];
            let done = register(&sys, token_file(), "fleetbot", "lab-07", &bob);
            if cut {
                assert!(done.unwrap_err().contains("cut short"));
            } else {
                assert_eq!(
                    done.unwrap(),
                    [Registered {
                        user: "bob".into(),
                        id: 101
                    }]
                );
            }
            assert!(!sys.calls().iter().any(|c| c.starts_with("github POST")));
        }
    }

    #[test]
    fn a_token_for_another_login_sends_nothing() {
        let sys = controller("someone-else");
        let err = register(
            &sys,
            token_file(),
            "fleetbot",
            "lab-07",
            &[("alice".into(), ALICE_KEY.into())],
        )
        .unwrap_err();
        assert!(err.contains("signs in as \"someone-else\""), "{err}");
        assert!(sys.calls().iter().all(|c| !c.contains("/user/keys")));
        clean(&sys);
    }

    /// No token file names the verb that stores one; a token file others can read, or one that
    /// holds no single token, is refused and names the verb that replaces it. None of them
    /// sends anything, and none runs git.
    #[test]
    fn no_usable_token_file_names_the_verb_that_stores_one_and_sends_nothing() {
        let sys = Fake::default();
        let err = revoke(&sys, token_file(), "fleetbot", &[1]).unwrap_err();
        assert!(
            err.contains("no GitHub token for fleetbot is stored"),
            "{err}"
        );
        assert!(err.contains("aterm machines github login"), "{err}");
        assert!(sys.calls().is_empty(), "{:?}", sys.calls());
        for (bytes, mode, why) in [
            (TOKEN, 0o644, "private regular file"),
            ("", 0o600, "no single GitHub token"),
            ("first second", 0o600, "no single GitHub token"),
        ] {
            let sys = Fake::default();
            stored(&sys, bytes, mode);
            let err = revoke(&sys, token_file(), "fleetbot", &[1]).unwrap_err();
            assert!(err.contains(why), "{err}");
            assert!(
                err.contains("aterm machines github login replaces it"),
                "{err}"
            );
            assert!(!err.contains(TOKEN), "{err}");
            assert!(sys.calls().is_empty(), "{:?}", sys.calls());
        }
    }

    #[test]
    fn a_token_github_no_longer_accepts_stops_at_the_login_check() {
        let sys = Fake::default();
        stored(&sys, TOKEN, 0o600);
        sys.on_github_rest(
            RestMethod::Get,
            "/user",
            answer(401, &[], BAD_CREDENTIALS_BODY),
        );
        let err = register(
            &sys,
            token_file(),
            "fleetbot",
            "lab-07",
            &[("alice".into(), ALICE_KEY.into())],
        )
        .unwrap_err();
        assert_eq!(err, "GitHub GET /user: HTTP 401 (Bad credentials)");
        assert!(sys.calls().iter().all(|c| !c.contains("/user/keys")));
        clean(&sys);
    }

    #[test]
    fn a_refused_post_names_the_apis_reason() {
        let sys = controller("fleetbot");
        lists(&sys, &[]);
        sys.on_github_rest(
            RestMethod::Post,
            "/user/keys",
            answer(
                422,
                &[],
                r#"{"message":"Validation Failed","errors":[{"resource":"PublicKey","code":"custom","field":"key","message":"key is already in use"}],"documentation_url":"https://docs.github.com/rest/users/keys#create-a-public-ssh-key-for-the-authenticated-user","status":"422"}"#,
            ),
        );
        let err = register(
            &sys,
            token_file(),
            "fleetbot",
            "lab-07",
            &[("alice".into(), ALICE_KEY.into())],
        )
        .unwrap_err();
        assert_eq!(
            err,
            "alice: GitHub POST /user/keys: HTTP 422 (key is already in use)"
        );
    }

    #[test]
    fn a_key_is_revoked_when_the_listing_no_longer_holds_it() {
        let sys = controller("fleetbot");
        lists(
            &sys,
            &[(100_000_101, OTHER_BODIES[0]), (100_000_105, BOB_BODY)],
        );
        lists(&sys, &[(100_000_101, OTHER_BODIES[0])]);
        sys.on_github_rest(
            RestMethod::Delete,
            "/user/keys/100000105",
            answer(204, &[], ""),
        );
        assert_eq!(
            revoke(&sys, token_file(), "fleetbot", &[100_000_105, 100_000_106]),
            Ok(vec![format!("ssh-ed25519 {BOB_BODY}")])
        );
        let calls = sys.calls();
        assert!(calls.contains(&"github DELETE /user/keys/100000105".to_owned()));
        assert!(
            !calls.contains(&"github DELETE /user/keys/100000106".to_owned()),
            "a key not held is not deleted"
        );
        clean(&sys);
    }

    #[test]
    fn a_404_from_a_token_without_the_scope_is_not_a_key_already_gone() {
        for (held, hint) in [("repo, read:org", true), ("admin:public_key", false)] {
            let sys = controller("fleetbot");
            lists(&sys, &[(100_000_107, BOB_BODY)]);
            sys.on_github_rest(
                RestMethod::Delete,
                "/user/keys/100000107",
                answer(404, &[ACCEPTED, ("X-OAuth-Scopes", held)], NOT_FOUND_BODY),
            );
            let err = revoke(&sys, token_file(), "fleetbot", &[100_000_107]).unwrap_err();
            assert!(
                err.starts_with(
                    "key 100000107: still registered (GitHub DELETE /user/keys/100000107: HTTP \
                     404 (Not Found)"
                ),
                "{err}"
            );
            assert_eq!(
                err.contains("the token lacks the admin:public_key scope"),
                hint,
                "{err}"
            );
        }
    }

    #[test]
    fn a_listing_the_token_may_not_read_names_the_missing_scope() {
        let sys = controller("fleetbot");
        sys.on_github_rest(
            RestMethod::Get,
            FIRST_PAGE,
            answer(404, &[ACCEPTED, ("X-OAuth-Scopes", "repo")], NOT_FOUND_BODY),
        );
        let err = revoke(&sys, token_file(), "fleetbot", &[7]).unwrap_err();
        assert!(
            err.starts_with("GitHub GET /user/keys: HTTP 404 (Not Found); the token lacks the admin:public_key scope"),
            "{err}"
        );
        assert!(sys.calls().iter().all(|c| !c.contains("DELETE")));
    }

    #[test]
    fn a_listing_entry_out_of_shape_stops_the_revocation() {
        let sys = controller("fleetbot");
        sys.on_github_rest(
            RestMethod::Get,
            FIRST_PAGE,
            answer(
                200,
                &[],
                &format!(
                    r#"[{{"id":100000107,"key":"ssh-ed25519 {BOB_BODY}"}},{{"key":"ssh-ed25519 {}"}}]"#,
                    OTHER_BODIES[1]
                ),
            ),
        );
        let err = revoke(&sys, token_file(), "fleetbot", &[100_000_107]).unwrap_err();
        assert!(err.contains("not an id and an SSH key"), "{err}");
        assert!(sys.calls().iter().all(|c| !c.contains("DELETE")));
    }

    #[test]
    fn nothing_to_do_reads_no_token() {
        let sys = Fake::default();
        assert_eq!(
            register(&sys, token_file(), "fleetbot", "lab-07", &[]),
            Ok(Vec::new())
        );
        assert_eq!(revoke(&sys, token_file(), "fleetbot", &[]), Ok(Vec::new()));
        assert!(sys.calls().is_empty());
    }

    #[test]
    fn an_account_that_is_not_a_login_is_refused_before_anything_is_read() {
        let sys = Fake::default();
        stored(&sys, TOKEN, 0o600);
        let err = revoke(&sys, token_file(), "--user=x", &[1]).unwrap_err();
        assert!(err.contains("not a GitHub login"), "{err}");
        let err = login(
            &sys,
            token_file(),
            "a\nb",
            Secret::new(TOKEN.as_bytes()).unwrap(),
        )
        .unwrap_err();
        assert!(err.contains("not a GitHub login"), "{err}");
        assert!(sys.calls().is_empty());
    }

    /// A token that is empty or holds whitespace is no token: nothing is stored or asked.
    #[test]
    fn a_token_that_is_not_one_word_stores_nothing_and_asks_nothing() {
        for token in [&b"a b"[..], b"", b"a\nb"] {
            let sys = Fake::default();
            let err =
                login(&sys, token_file(), "fleetbot", Secret::new(token).unwrap()).unwrap_err();
            assert!(err.contains("not one word"), "{err}");
            assert!(sys.calls().is_empty(), "{:?}", sys.calls());
            assert_eq!(sys.written(&token_file().to_string_lossy()), None);
        }
    }

    /// `aterm machines github login` stores the token in the controller's token file (0600) only
    /// once GitHub signs it in as the account with the key scope, and reads it back; git and the
    /// login keychain are never asked. Negative controls: another account's token, a classic
    /// token without the scope, and one GitHub refuses each store nothing.
    #[test]
    fn login_stores_a_token_only_after_github_vouches_for_it() {
        let user = |login: &str| format!(r#"{{"login":"{login}"}}"#);
        let cases: [(Result<GithubResponse, String>, Option<&str>); 5] = [
            (
                answer(
                    200,
                    &[("X-OAuth-Scopes", "repo, admin:public_key")],
                    &user("FleetBot"),
                ),
                None,
            ),
            (answer(200, &[], &user("fleetbot")), None),
            (
                answer(
                    200,
                    &[("X-OAuth-Scopes", "admin:public_key")],
                    &user("someone-else"),
                ),
                Some("signs in as \"someone-else\", not fleetbot; nothing was stored"),
            ),
            (
                answer(
                    200,
                    &[("X-OAuth-Scopes", "repo, read:public_key")],
                    &user("fleetbot"),
                ),
                Some("lacks the admin:public_key scope"),
            ),
            (
                answer(401, &[], BAD_CREDENTIALS_BODY),
                Some("GitHub GET /user: HTTP 401 (Bad credentials); nothing was stored"),
            ),
        ];
        for (reply, refused) in cases {
            let sys = Fake::default();
            sys.on_github_rest(RestMethod::Get, "/user", reply);
            let result = login(
                &sys,
                token_file(),
                "fleetbot",
                Secret::new(TOKEN.as_bytes()).unwrap(),
            );
            let calls = sys.calls();
            match refused {
                None => {
                    result.unwrap();
                    assert_eq!(
                        calls,
                        [
                            "github GET /user".to_owned(),
                            "mkdir /controller/keys 700".to_owned(),
                            format!("write {TOKEN_FILE} 600"),
                        ]
                    );
                    assert_eq!(sys.mode_of(TOKEN_FILE), Some(0o600));
                    assert_eq!(
                        stored_token(&sys, token_file(), "fleetbot")
                            .unwrap()
                            .bytes(),
                        TOKEN.as_bytes()
                    );
                }
                Some(why) => {
                    let err = result.unwrap_err();
                    assert!(err.contains(why), "{err}");
                    assert_eq!(calls, ["github GET /user"], "nothing was stored");
                    assert!(sys.written(TOKEN_FILE).is_none());
                }
            }
            clean(&sys);
        }
    }

    /// A token file the read-back cannot read as a private file of this account's fails the
    /// login, and the error carries no token.
    #[test]
    fn a_token_file_that_does_not_read_back_fails() {
        let sys = Fake::default();
        sys.on_github_rest(
            RestMethod::Get,
            "/user",
            answer(200, &[], r#"{"login":"fleetbot"}"#),
        );
        sys.stat_of(
            TOKEN_FILE,
            Stat {
                uid: 0,
                gid: 0,
                mode: 0o644,
                dir: false,
                symlink: false,
                len: TOKEN.len() as u64,
            },
        );
        let err = login(
            &sys,
            token_file(),
            "fleetbot",
            Secret::new(TOKEN.as_bytes()).unwrap(),
        )
        .unwrap_err();
        assert!(err.contains("private regular file"), "{err}");
        assert!(!err.contains(TOKEN), "{err}");
        clean(&sys);
    }
}
