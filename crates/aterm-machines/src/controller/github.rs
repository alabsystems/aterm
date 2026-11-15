// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Host GitHub key registration and revocation ([`System::github_rest`], [`KEY_SCOPE`]), or, by
//! `[github] keys = "agh-inline"`, over agh's library ([`register_via`], [`revoke_via`]: machines
//! 5a). The machine user's token is the vault's `gh:fleet` entry, resolved on the one credential
//! path ([`super::credential`]); one this Mac lacks is filed with the lab and blocks the call.

use super::inventory::{Github, KeysVia};
use crate::github_pages;
use crate::port::{GithubResponse, RestMethod, Secret, System};
use agh::GithubBuilder;
use agh::identity::{InlineToken, ProcessEnv, Profile};
use agh::policy::{RouteRequest, RouteTarget, Router, Tier};
use agh::redact::Redactor;
use agh::retry::RetryPolicy;
use aterm_secrets::need::{GhProfile, Purpose, Want};
use serde::Serialize;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

/// OAuth scope required to list, add, and delete the account's SSH keys.
pub const KEY_SCOPE: &str = "admin:public_key";

/// The key listing, at GitHub's largest page.
const KEYS: &str = "/user/keys?per_page=100";
const KEYS_PER_PAGE: usize = 100;

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

/// The machine user `account`'s token: the vault's `gh:fleet` entry, else a need filed with the
/// lab and the key call blocked.
fn fleet_token(sys: &dyn System, account: &str) -> Result<Secret, String> {
    if !valid_login(account) {
        return Err(format!("{account:?} is not a GitHub login"));
    }
    super::credential::resolve(
        sys,
        &Want::Gh(GhProfile::Fleet),
        Purpose::MachinesKeys,
        true,
    )
    .map_err(|why| format!("{why}; host key calls are blocked"))
}

impl Session {
    /// Resolve `account`'s token and check that it signs in as `account`.
    fn open(sys: &dyn System, account: &str) -> Result<Self, String> {
        let token = fleet_token(sys, account)?;
        let session = Self { token };
        let (login, _) = session.login(sys)?;
        if !login.eq_ignore_ascii_case(account) {
            return Err(format!(
                "the gh:fleet token for {account} signs in as {login:?}; nothing was sent"
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
        line.push_str(&format!("; the token lacks the {scope} scope"));
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
/// the vault's `gh:fleet` token.
///
/// # Errors
/// One line (no token for `account`, the token is for another login, the API refused).
pub fn register(
    sys: &dyn System,
    account: &str,
    host_name: &str,
    keys: &[(String, String)],
) -> Result<Vec<Registered>, String> {
    register_each(sys, account, host_name, keys).into_result()
}

/// [`register`], each key's outcome kept.
fn register_each(
    sys: &dyn System,
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
    let opened = Session::open(sys, account)
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

/// Delete each key id from the machine user `account`, with the vault's `gh:fleet` token, and
/// read the account's keys back.
///
/// # Errors
/// One line per key the account still holds, or why the keys could not be listed.
pub fn revoke(sys: &dyn System, account: &str, ids: &[u64]) -> Result<Vec<String>, String> {
    let ids = dedup_ids(ids);
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let session = Session::open(sys, account)?;
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

/// The name of the profile the agh key path speaks as.
const FLEET: &str = "fleet";

/// The single-identity router of the agh key path (plan §17): the `gh:fleet` token inline. Each
/// refusal of its own ends `nothing was sent`.
fn agh_router(sys: &dyn System, github: &Github) -> Result<Router, String> {
    let account = &github.account;
    if github.keys_via() == KeysVia::Gh {
        return Err(format!(
            "keys = \"gh\" sends {account}'s key calls over the REST path, not agh; nothing was \
             sent"
        ));
    }
    let token = fleet_token(sys, account)?;
    let token = InlineToken::new(aterm_uds::secret::Secret::new(token.bytes())).map_err(|e| {
        format!(
            "the gh:fleet token for {account} cannot be used by agh ({}); nothing was sent",
            e.slug()
        )
    })?;
    // agh binds the profile to `account`: its first call is `GET /user`, and a token that signs
    // in as another login is refused before any key call.
    let profile = Profile::inline(FLEET, Tier::Dev, account.clone(), token)
        .map_err(|e| format!("{}; nothing was sent", Redactor::shapes(&e.why)))?;
    Ok(Router::single_identity(profile))
}

/// One agh client of the key path `github.keys_via()` names, the one identity write permit
/// its key writes share, and whose token its error lines speak of.
struct AghKeys {
    gh: agh::Github,
    permit: agh::Permit,
    /// The account whose `gh:fleet` token the client carries inline (5a).
    account: String,
}

impl AghKeys {
    /// Open the client; agh binds the inline profile to `github.account` (plan §17, As built).
    fn open(sys: &dyn System, github: &Github) -> Result<Self, String> {
        let router = agh_router(sys, github)?;
        let (transport, endpoints) = sys.github_transport();
        let gh = GithubBuilder::new(router)
            .transport(transport)
            .endpoints(endpoints)
            .env(Arc::new(ProcessEnv))
            .fx(sys.github_fx())
            .retry(RetryPolicy::STANDARD)
            .caller("aterm-machines")
            .rate_book(
                sys.agh_dirs()
                    .map_or_else(agh::rate::RateBook::none, |dirs| {
                        agh::rate::RateBook::at(dirs.rate())
                    }),
            )
            .build();
        let permit = gh
            .permit(&RouteRequest::write(RouteTarget::Identity))
            .map_err(|e| agh_line(&gh, "route /user/keys", &e))?;
        Ok(Self {
            gh,
            permit,
            account: github.account.clone(),
        })
    }

    /// One line for a key call's failure, as `agh_line`. A token agh refuses as
    /// unauthenticated, or one that lacks [`KEY_SCOPE`], is named as the `gh:fleet` token, never
    /// agh's own, which cannot see it: another login's token gets the REST path's text.
    fn line(&self, what: &str, e: &agh::Error) -> String {
        let account = &self.account;
        let line = match (&e.kind, &e.sub) {
            (agh::Kind::Unauthenticated, agh::Sub::LoginMismatch { got, .. }) => {
                format!("the gh:fleet token for {account} signs in as {got:?}; nothing was sent")
            }
            (agh::Kind::Unauthenticated, _) => format!(
                "{what}: {}; GitHub refuses the gh:fleet token for {account}",
                e.why
            ),
            _ if e.next.contains(KEY_SCOPE) => format!(
                "{what}: {}; the gh:fleet token for {account} lacks the {KEY_SCOPE} scope",
                e.why
            ),
            _ => return agh_line(&self.gh, what, e),
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
/// POST and the next host's POST included, and binds the token's login once. A failed
/// opening is not kept: the next call opens again. The REST path (`KeysVia::Gh`) opens its
/// session per call, unchanged.
pub(crate) struct KeyCalls<'s> {
    sys: &'s dyn System,
    /// The client opened for an account and a key path, and its permit.
    agh: RefCell<Option<(String, KeysVia, Rc<AghKeys>)>>,
}

impl<'s> KeyCalls<'s> {
    /// No client opened yet.
    pub(crate) fn new(sys: &'s dyn System) -> Self {
        Self {
            sys,
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
        let keys = Rc::new(AghKeys::open(self.sys, github)?);
        *self.agh.borrow_mut() = Some((github.account.clone(), via, Rc::clone(&keys)));
        Ok(keys)
    }

    /// [`register`] by the path `github.keys_via()` names, each key's outcome kept: `Gh` is
    /// the REST path, unchanged; `AghInline` the agh path (§17).
    pub(crate) fn register(
        &self,
        github: &Github,
        host_name: &str,
        keys: &[(String, String)],
    ) -> Registration {
        match github.keys_via() {
            KeysVia::Gh => register_each(self.sys, &github.account, host_name, keys),
            KeysVia::AghInline => self.register_agh(github, host_name, keys),
        }
    }

    /// [`revoke`] by the path `github.keys_via()` names, as `register`.
    ///
    /// # Errors
    ///
    /// As `revoke`, or the agh path's refusal.
    pub(crate) fn revoke(&self, github: &Github, ids: &[u64]) -> Result<Vec<String>, String> {
        match github.keys_via() {
            KeysVia::Gh => revoke(self.sys, &github.account, ids),
            KeysVia::AghInline => self.revoke_agh(github, ids),
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

/// [`register`] by the path `github.keys_via()` names: `Gh` is
/// `register(sys, &github.account, host_name, keys)`; `AghInline` is the agh path (§17).
///
/// # Errors
///
/// As `register`, or the agh path's refusal, one line naming the key call.
pub fn register_via(
    sys: &dyn System,
    github: &Github,
    host_name: &str,
    keys: &[(String, String)],
) -> Result<Vec<Registered>, String> {
    KeyCalls::new(sys)
        .register(github, host_name, keys)
        .into_result()
}

/// [`revoke`] by the path `github.keys_via()` names, as `register_via`.
///
/// # Errors
///
/// As `revoke`, or the agh path's refusal.
pub fn revoke_via(sys: &dyn System, github: &Github, ids: &[u64]) -> Result<Vec<String>, String> {
    KeyCalls::new(sys).revoke(github, ids)
}

#[cfg(test)]
#[path = "github_tests.rs"]
mod tests;
