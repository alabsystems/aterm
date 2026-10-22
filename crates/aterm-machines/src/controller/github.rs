// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Registering and revoking each host's GitHub key with the fleet's machine user (design §7).
//!
//! No gh (docs/DESIGN-init-system-2026-10-20.md S14, §6.1 row 22). The machine user's token is
//! a login-keychain entry on the controller that `aterm machines github login` files ([`login`])
//! through CLT git's osxkeychain helper (`git credential approve`, the token on standard input
//! alone; [`crate::git_credential`]) under the host [`TOKEN_HOST`], not `github.com`, so git's
//! own github.com lookups (a clone, a fetch) never hand it out. Each command reads it back with
//! `git credential fill`, checks with `GET /user` that it signs in as the account, and talks to
//! GitHub's REST API through [`System::github_rest`] (aterm-http): the token is an
//! `Authorization` header this process builds, never an argument, a variable, a log line or
//! an error.
//!
//! What GitHub answers (REST version 2022-11-28), which the tests' fixtures follow with
//! made-up ids, keys and tokens:
//!
//! * `GET /user`: 200 and the account's `login`; 401 `Bad credentials` for a token it no longer
//!   accepts. A classic token's answers carry `X-OAuth-Scopes`, the scopes it holds; a
//!   fine-grained token's carry none.
//! * `GET /user/keys?per_page=100`: 200 and a JSON array of `{id, key, …}`, `key` being
//!   `<type> <base64>` (GitHub keeps no comment), paged by `Link`.
//! * `POST /user/keys` with `{"title", "key"}`: 201 and the new key's `id`; 422 `Validation
//!   Failed` with `errors[].message` for a key it refuses.
//! * `DELETE /user/keys/{id}`: 204.
//! * A token without [`KEY_SCOPE`]: 404 `Not Found` on each of those, with
//!   `X-Accepted-OAuth-Scopes` naming the scopes the endpoint takes.

use crate::git_credential::Git;
use crate::github_pages;
use crate::port::{GithubResponse, RestMethod, Secret, System};
use serde::Serialize;

/// The host the machine user's token is filed under in the login keychain: the API's, which
/// no git remote names.
pub const TOKEN_HOST: &str = "api.github.com";

/// The OAuth scope listing, adding and deleting the account's SSH keys needs (a classic
/// token's; a fine-grained token needs read and write of "Git SSH keys"). GitHub answers a
/// token without it `Not Found`.
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

/// The git `account`'s token is stored and read through, once `account` is a login and that
/// git is installed.
fn keychain(sys: &dyn System, account: &str) -> Result<Git, String> {
    if !valid_login(account) {
        return Err(format!("{account:?} is not a GitHub login"));
    }
    let git = Git::keychain();
    if !git.installed(sys) {
        return Err(
            "the machine user's GitHub token is kept through Apple's Command Line Tools git, \
             which is not installed (aterm init --only base.clt installs them)"
                .into(),
        );
    }
    Ok(git)
}

/// An authenticated session as the machine user. Holds the token, so it has no `Debug`.
struct Session {
    token: Secret,
}

impl Session {
    /// Read `account`'s token from the login keychain and check that it signs in as
    /// `account`.
    fn open(sys: &dyn System, account: &str) -> Result<Self, String> {
        let git = keychain(sys, account)?;
        let Some(token) = git.fill(sys, None, TOKEN_HOST, account)? else {
            return Err(format!(
                "the login keychain holds no GitHub token for {account} (aterm machines github \
                 login stores one: a token of {account}'s with the {KEY_SCOPE} scope)"
            ));
        };
        let session = Self { token };
        let (login, _) = session.login(sys)?;
        if !login.eq_ignore_ascii_case(account) {
            return Err(format!(
                "the login keychain's GitHub token for {account} signs in as {login:?}; nothing \
                 was sent (aterm machines github login replaces it)"
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

/// The scope the token lacks for an endpoint, when GitHub's answer says: the first of the
/// endpoint's `X-Accepted-OAuth-Scopes` when the token's `X-OAuth-Scopes` holds none of them.
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

/// One line for an answer that is not the one wanted: the status, the API's message, and the
/// scope the token lacks when GitHub names it.
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
    let values = github_pages::all(KEYS, KEYS_PER_PAGE, |endpoint| {
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

/// Register every `github.key@<user>` public key in `report` that the machine user
/// `account` does not hold yet, titled `aterm-machines:<host_name>:<user>`.
///
/// `keys` are `(user, public key line)`. A key the account already holds (the same type and
/// body, whatever its comment) is not sent again; its id is returned so the controller
/// records it. When some keys fail, the error names them; the ones that landed are found by
/// the listing on the next run.
///
/// # Errors
/// One line (no token for `account`, the token is for another login, the API refused).
pub fn register(
    sys: &dyn System,
    account: &str,
    host_name: &str,
    keys: &[(String, String)],
) -> Result<Vec<Registered>, String> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let mut wanted = Vec::with_capacity(keys.len());
    for (user, line) in keys {
        let (kind, body) = crate::items::github::key_material(line)
            .ok_or_else(|| format!("{user}'s key is not an OpenSSH public key"))?;
        wanted.push((user, kind, body));
    }
    let session = Session::open(sys, account)?;
    let held = listed(sys, &session)?;
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
        let new = aterm_json::to_vec(&NewKey {
            title: &title,
            key: &key,
        })
        .map_err(|e| format!("{user}: {e}"))?;
        let posted = session.send(sys, RestMethod::Post, "/user/keys", Some(&new));
        match posted {
            Ok(answer) if answer.status == 201 => {
                let id = aterm_json::from_slice::<aterm_json::Value>(&answer.body)
                    .ok()
                    .and_then(|value| value["id"].as_u64());
                match id {
                    Some(id) => done.push(Registered {
                        user: user.clone(),
                        id,
                    }),
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
    if failed.is_empty() {
        Ok(done)
    } else {
        Err(failed.join("; "))
    }
}

/// Delete each key id from the machine user `account`, and read the account's keys back.
/// Returns each deleted key as `<type> <base64>`: the keys the account held under those ids,
/// so the controller can refuse to register them again.
///
/// A key is revoked when the listing after the deletes no longer holds it. A `DELETE`'s own
/// answer cannot say so: GitHub answers `Not Found (HTTP 404)` both for a key already gone and
/// for a token without [`KEY_SCOPE`], and a revocation that reported a live key gone would
/// leave an unenrolled Mac its access.
///
/// # Errors
/// One line per key the account still holds, or why the keys could not be listed.
pub fn revoke(sys: &dyn System, account: &str, ids: &[u64]) -> Result<Vec<String>, String> {
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
        Ok(deleted)
    } else {
        Err(failed.join("; "))
    }
}

/// Store `token` as the machine user `account`'s (`aterm machines github login`), once
/// GitHub confirms it signs in as `account` and, for a classic token (whose answers list its
/// scopes), that it holds [`KEY_SCOPE`]; then read it back from the login keychain.
///
/// # Errors
/// One line (not a login, no CLT git, GitHub refused the token, it is another account's or
/// lacks the scope, or the keychain did not hand it back). Nothing is stored on an error
/// before the store, and no error carries the token.
pub fn login(sys: &dyn System, account: &str, token: Secret) -> Result<(), String> {
    let git = keychain(sys, account)?;
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
    git.store(sys, None, TOKEN_HOST, account, &session.token)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_credential::script;
    use crate::port::{Fake, Stat};

    /// Made up; never a real token.
    const TOKEN: &str = "fixture_machine_user_token_only";

    /// The key the VM's admin made (see items::github's fixtures).
    const ALICE_KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIJpFPtSPTOuLIj6oqQTj8ndNDVNHFp4renP47+wxFYao aterm-machines:lab-07:alice";

    /// GitHub's own example ed25519 key shape, as `GET /user/keys` returns keys: type and
    /// body, no comment.
    const BOB_BODY: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl";

    /// Two more made-up ed25519 bodies, for keys the account holds for other hosts.
    const OTHER_BODIES: [&str; 2] = [
        "AAAAC3NzaC1lZDI1NTE5AAAAIEf/eQTWeJTgxWM6+tuR3GYdddTaYJfSbQ+2Vr/R0C/7",
        "AAAAC3NzaC1lZDI1NTE5AAAAIKClQs4JkZ6qk49IjNe40mEc2+O0ppqqJHbHKh7uv2yZ",
    ];

    /// The body GitHub sent for `GET user/keys/1` (a key id the account does not hold),
    /// captured byte for byte: compact, no newline. Nothing reads its `documentation_url`.
    const NOT_FOUND_BODY: &str = r#"{"message":"Not Found","documentation_url":"https://docs.github.com/rest/users/keys#get-a-public-ssh-key-for-the-authenticated-user","status":"404"}"#;

    /// The body GitHub sent for `GET user` with a made-up token, captured byte for byte:
    /// indented, CRLF line ends.
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

    /// CLT git, for the keychain reads.
    fn git(sys: &Fake) {
        sys.stat_of(
            crate::items::github::CLT_GIT,
            Stat {
                uid: 0,
                gid: 0,
                mode: 0o755,
                dir: false,
                symlink: false,
                len: 1,
            },
        );
    }

    /// A controller whose login keychain holds a token for `fleetbot` that signs in as
    /// `login`.
    fn controller(login: &str) -> Fake {
        let sys = Fake::default();
        git(&sys);
        script::holds(&sys, None, TOKEN_HOST, "fleetbot", TOKEN);
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

    /// No call carries the token, and none is gh.
    fn clean(sys: &Fake) {
        let calls = sys.calls();
        assert!(
            calls
                .iter()
                .all(|c| !c.contains(TOKEN) && !c.contains("/gh ") && !c.contains("GH_TOKEN")),
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
        let done = register(&sys, "fleetbot", "lab-07", &keys).unwrap();
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
        // The token came from the keychain, through the one pinned helper.
        assert_eq!(calls[0], script::credential(None, "fill"));
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
            let done = register(&sys, "fleetbot", "lab-07", &bob);
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
            "fleetbot",
            "lab-07",
            &[("alice".into(), ALICE_KEY.into())],
        )
        .unwrap_err();
        assert!(err.contains("signs in as \"someone-else\""), "{err}");
        assert!(sys.calls().iter().all(|c| !c.contains("/user/keys")));
        clean(&sys);
    }

    #[test]
    fn no_token_in_the_keychain_names_the_verb_that_stores_one_and_sends_nothing() {
        let sys = Fake::default();
        git(&sys);
        script::holds_nothing(&sys, None);
        let err = revoke(&sys, "fleetbot", &[1]).unwrap_err();
        assert!(err.contains("holds no GitHub token for fleetbot"), "{err}");
        assert!(err.contains("aterm machines github login"), "{err}");
        assert_eq!(sys.calls(), [script::credential(None, "fill")]);
        // Without CLT git the keychain is not read at all, and the remedy is init's item.
        let sys = Fake::default();
        let err = revoke(&sys, "fleetbot", &[1]).unwrap_err();
        assert!(err.contains("aterm init --only base.clt"), "{err}");
        assert!(sys.calls().is_empty());
    }

    #[test]
    fn a_token_github_no_longer_accepts_stops_at_the_login_check() {
        // The keychain still holds it; only `GET /user` says GitHub refuses it.
        let sys = Fake::default();
        git(&sys);
        script::holds(&sys, None, TOKEN_HOST, "fleetbot", TOKEN);
        sys.on_github_rest(
            RestMethod::Get,
            "/user",
            answer(401, &[], BAD_CREDENTIALS_BODY),
        );
        let err = register(
            &sys,
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
        // GitHub's reference shows no body for this 422: the `errors[]` entry is made up, in
        // the layout of the measured 422 of `GET search/repositories?q=`.
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
        // Before: …105 is held and …106 is already gone. After the delete: neither.
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
            revoke(&sys, "fleetbot", &[100_000_105, 100_000_106]),
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
            let err = revoke(&sys, "fleetbot", &[100_000_107]).unwrap_err();
            assert!(
                err.starts_with(
                    "key 100000107: still registered (GitHub DELETE /user/keys/100000107: HTTP \
                     404 (Not Found)"
                ),
                "{err}"
            );
            // Negative control: a token that holds the scope gets no scope hint.
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
        let err = revoke(&sys, "fleetbot", &[7]).unwrap_err();
        assert!(
            err.starts_with("GitHub GET /user/keys: HTTP 404 (Not Found); the token lacks the admin:public_key scope"),
            "{err}"
        );
        assert!(sys.calls().iter().all(|c| !c.contains("DELETE")));
    }

    #[test]
    fn a_listing_entry_out_of_shape_stops_the_revocation() {
        // An entry the listing cannot read could hide a key still held, which a revocation
        // would then report gone.
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
        let err = revoke(&sys, "fleetbot", &[100_000_107]).unwrap_err();
        assert!(err.contains("not an id and an SSH key"), "{err}");
        assert!(sys.calls().iter().all(|c| !c.contains("DELETE")));
    }

    #[test]
    fn nothing_to_do_reads_no_token() {
        let sys = Fake::default();
        assert_eq!(register(&sys, "fleetbot", "lab-07", &[]), Ok(Vec::new()));
        assert_eq!(revoke(&sys, "fleetbot", &[]), Ok(Vec::new()));
        assert!(sys.calls().is_empty());
    }

    #[test]
    fn an_account_that_is_not_a_login_is_refused_before_git_runs() {
        let sys = Fake::default();
        git(&sys);
        let err = revoke(&sys, "--user=x", &[1]).unwrap_err();
        assert!(err.contains("not a GitHub login"), "{err}");
        let err = login(&sys, "a\nb", Secret::new(TOKEN.as_bytes()).unwrap()).unwrap_err();
        assert!(err.contains("not a GitHub login"), "{err}");
        assert!(sys.calls().is_empty());
    }

    /// `aterm machines github login` stores the token only once GitHub signs it in as the
    /// account with the key scope, and reads it back. Negative controls: another account's
    /// token, a classic token without the scope, and one GitHub refuses each store nothing.
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
            // A fine-grained token lists no scopes: GitHub's answers on use are the check.
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
            git(&sys);
            sys.on_github_rest(RestMethod::Get, "/user", reply);
            let approve = script::approves(&sys, None);
            script::holds(&sys, None, TOKEN_HOST, "fleetbot", TOKEN);
            let stored = login(&sys, "fleetbot", Secret::new(TOKEN.as_bytes()).unwrap());
            let calls = sys.calls();
            match refused {
                None => {
                    stored.unwrap();
                    assert_eq!(
                        calls,
                        [
                            "github GET /user".to_owned(),
                            approve,
                            script::credential(None, "fill")
                        ]
                    );
                }
                Some(why) => {
                    let err = stored.unwrap_err();
                    assert!(err.contains(why), "{err}");
                    assert_eq!(calls, ["github GET /user"], "nothing was stored");
                }
            }
            clean(&sys);
        }
    }

    #[test]
    fn a_store_the_keychain_does_not_hand_back_fails() {
        let sys = Fake::default();
        git(&sys);
        sys.on_github_rest(
            RestMethod::Get,
            "/user",
            answer(200, &[], r#"{"login":"fleetbot"}"#),
        );
        script::approves(&sys, None);
        script::holds(
            &sys,
            None,
            TOKEN_HOST,
            "fleetbot",
            "fixture_some_other_value",
        );
        let err = login(&sys, "fleetbot", Secret::new(TOKEN.as_bytes()).unwrap()).unwrap_err();
        assert!(err.contains("did not hand back"), "{err}");
        assert!(
            !err.contains(TOKEN) && !err.contains("fixture_some_other_value"),
            "{err}"
        );
    }
}
