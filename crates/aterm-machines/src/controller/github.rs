// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Registering and revoking each host's GitHub key with the fleet's machine user (design §7).
//!
//! The machine user's token stays in `gh`'s keychain entry on the controller. Each command
//! reads it with `gh auth token --hostname github.com --user <account>` and hands it to each
//! `gh api` child as `GH_TOKEN` through [`Cmd::secret_env`], whose display masks it, so it is never
//! printed, logged or put in an error. `gh auth switch` is never run: it would change the
//! account of every other program on the controller that uses gh.
//!
//! What gh prints was measured with gh 2.92.0 on 2026-10-16, run as [`crate::port::Live`] runs
//! a child (cleared environment, fixed `PATH`, `HOME`, `/dev/null` on standard input), and the
//! tests' fixtures follow it, with made-up ids, keys and tokens:
//!
//! * `gh auth token --hostname github.com --user <login>`: exit 0, the token and a newline
//!   (`gho_` and 36 of `[A-Za-z0-9_]`; a fine-grained token is `github_pat_…`), nothing on
//!   standard error. gh prints a token GitHub no longer accepts just the same, so only
//!   `GET /user` catches that. For a login gh does not hold (`--user` matches exactly, case
//!   included): exit 1, nothing on standard output, and `no oauth token found for github.com
//!   account <login>` on standard error.
//! * `gh api user --jq .login`: exit 0, the login and a newline.
//! * `gh api user/keys --paginate --jq '.[] | [.id, .key] | @tsv'`: per key, `<id>`, a tab,
//!   `<type> <base64>` (GitHub keeps no comment) and a newline; the same lines whatever the
//!   page size; nothing at all for no keys.
//! * A `gh api` GitHub refuses: exit 1, `gh: <message> (HTTP <status>)` and a newline on
//!   standard error, and the API's JSON body on standard output as GitHub sent it, `--jq` not
//!   applied (compact for a 404 or a 422, indented with CRLF line ends for a 401).

use crate::port::{Cmd, System};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Where gh lives: Homebrew on Apple silicon, then Homebrew on Intel and gh's own pkg.
pub const GH_PATHS: [&str; 2] = ["/opt/homebrew/bin/gh", "/usr/local/bin/gh"];

/// The one GitHub host.
pub const GITHUB_HOST: &str = "github.com";

/// The bound on one gh call: a hung network is what it catches.
const GH_BUDGET: Duration = Duration::from_secs(60);

/// The OAuth scope listing, adding and deleting the account's SSH keys needs. `gh auth login`
/// does not ask for it by default, and GitHub answers a token without it `Not Found`.
pub const KEY_SCOPE: &str = "admin:public_key";

/// One key `POST /user/keys` registered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registered {
    /// The host user the key belongs to.
    pub user: String,
    /// GitHub's key id.
    pub id: u64,
}

/// The first gh of [`GH_PATHS`] that exists.
///
/// # Errors
/// One line when neither does.
pub fn gh_path(sys: &dyn System) -> Result<PathBuf, String> {
    GH_PATHS
        .iter()
        .map(Path::new)
        .find(|p| sys.stat(p).is_ok())
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            format!(
                "gh is not installed ({} or {}), and aterm does not install it: with one \
                 there, gh auth login as the machine user",
                GH_PATHS[0], GH_PATHS[1]
            )
        })
}

/// A `gh api` run carrying `token`.
#[must_use]
pub fn api_cmd(gh: &Path, token: &str) -> Cmd {
    Cmd::new(gh)
        .secret_env("GH_TOKEN", token)
        .arg("api")
        .budget(GH_BUDGET)
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

/// An authenticated session as the machine user. Holds the token, so it has no `Debug`.
struct Session {
    gh: PathBuf,
    token: String,
}

impl Session {
    /// Read `account`'s token from gh and check that it signs in as `account`.
    fn open(sys: &dyn System, account: &str) -> Result<Self, String> {
        if !valid_login(account) {
            return Err(format!("{account:?} is not a GitHub login"));
        }
        let gh = gh_path(sys)?;
        let cmd = Cmd::new(&gh)
            .args([
                "auth",
                "token",
                "--hostname",
                GITHUB_HOST,
                "--user",
                account,
            ])
            .budget(GH_BUDGET);
        // Only standard error is ever quoted: standard output is the token.
        let ran = sys.run(&cmd).map_err(|e| format!("gh auth token: {e}"))?;
        if !ran.success() {
            // gh matches `--user` exactly: a login it holds as `fleetbot` is not found as
            // `FleetBot`, though GitHub reads both as one account.
            return Err(format!(
                "gh holds no token for {account} on {GITHUB_HOST} (gh matches the login \
                 exactly, case included, as gh auth status lists it; or run gh auth login \
                 --hostname {GITHUB_HOST} --scopes {KEY_SCOPE} and sign in as {account}): {}",
                ran.describe()
            ));
        }
        let token = ran.out().trim().to_owned();
        if token.is_empty() || token.contains(char::is_whitespace) {
            return Err(format!("gh auth token printed no token for {account}"));
        }
        let session = Self { gh, token };
        let login = session.call(sys, &["user", "--jq", ".login"])?;
        let login = login.trim();
        if !login.eq_ignore_ascii_case(account) {
            return Err(format!(
                "gh's token for {account} signs in as {login:?}; nothing was sent (gh auth \
                 switch is never run)"
            ));
        }
        Ok(session)
    }

    /// `gh api <args>`: standard output on success, else one line from gh's standard error
    /// and the API's message.
    fn call(&self, sys: &dyn System, args: &[&str]) -> Result<String, String> {
        let cmd = api_cmd(&self.gh, &self.token).args(args);
        let what = format!("gh api {}", endpoint(args));
        match sys.run(&cmd) {
            Ok(ran) if ran.success() => Ok(ran.out()),
            Ok(ran) => Err(format!("{what}: {}", failure(&ran))),
            Err(e) => Err(format!("{what}: {e}")),
        }
    }
}

/// The endpoint among `gh api`'s arguments: the first word that is neither a flag nor the
/// method `--method` takes.
fn endpoint<'a>(args: &[&'a str]) -> &'a str {
    let mut words = args.iter().copied();
    while let Some(word) = words.next() {
        if word == "--method" || word == "-X" {
            words.next();
        } else if !word.starts_with('-') {
            return word;
        }
    }
    ""
}

/// One line for a failed `gh api`: how it ended, the API's message when gh's status line does
/// not already carry it, and any line gh printed about the token's scopes. gh's status line is
/// the body's `message` (`gh: Not Found (HTTP 404)`), so only a message it leaves out (a 422's
/// `errors[].message`) is added. gh writes its scope suggestion (`gh auth refresh … -s
/// <scope>`) after the status line, which [`Ran::describe`] alone would drop.
fn failure(ran: &crate::port::Ran) -> String {
    let mut line = ran.describe();
    if let Some(message) = api_message(&ran.out()).filter(|m| !line.contains(m.as_str())) {
        line.push_str(&format!(" ({message})"));
    }
    let err = ran.err();
    for hint in err.lines().skip(1).filter(|l| l.contains("scope")) {
        line.push_str("; ");
        line.push_str(hint.trim().trim_start_matches("gh: "));
    }
    line
}

/// The API's own explanation in an error body gh printed: the first `errors[].message`, else
/// `message`.
fn api_message(body: &str) -> Option<String> {
    let value: aterm_json::Value = aterm_json::from_str(body.trim()).ok()?;
    value
        .pointer("/errors/0/message")
        .or_else(|| value.pointer("/message"))
        .and_then(|m| m.as_str())
        .map(str::to_owned)
}

/// The keys the machine user holds: `(id, key type, base64 body)`. One `<id>\t<type> <base64>`
/// line per key from `--jq`, which applies to each page `--paginate` fetches.
fn listed(sys: &dyn System, session: &Session) -> Result<Vec<(u64, String, String)>, String> {
    let out = session.call(
        sys,
        &[
            "user/keys",
            "--paginate",
            "--jq",
            ".[] | [.id, .key] | @tsv",
        ],
    )?;
    let mut keys = Vec::new();
    for line in out.lines().filter(|l| !l.trim().is_empty()) {
        let parsed = line.split_once('\t').and_then(|(id, key)| {
            let (kind, body) = crate::items::github::key_material(key)?;
            Some((id.trim().parse::<u64>().ok()?, kind, body))
        });
        keys.push(parsed.ok_or_else(|| format!("gh api user/keys printed {line:?}"))?);
    }
    Ok(keys)
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
        let posted = session.call(
            sys,
            &[
                "--method",
                "POST",
                "user/keys",
                "-f",
                &format!("title={title}"),
                "-f",
                &format!("key={kind} {body}"),
                "--jq",
                ".id",
            ],
        );
        match posted.map(|out| out.trim().parse::<u64>()) {
            Ok(Ok(id)) => done.push(Registered {
                user: user.clone(),
                id,
            }),
            Ok(Err(_)) => failed.push(format!("{user}: GitHub returned no key id")),
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
    let held = |sys: &dyn System| -> Result<Vec<u64>, String> {
        Ok(listed(sys, &session)?
            .into_iter()
            .map(|(id, ..)| id)
            .collect())
    };
    let listing = listed(sys, &session)?;
    let before: Vec<u64> = listing.iter().map(|(id, ..)| *id).collect();
    let deleted: Vec<String> = listing
        .iter()
        .filter(|(id, ..)| ids.contains(id))
        .map(|(_, kind, body)| format!("{kind} {body}"))
        .collect();
    let mut answers: Vec<(u64, String)> = Vec::new();
    for id in ids.iter().copied().filter(|id| before.contains(id)) {
        let path = format!("user/keys/{id}");
        let cmd = api_cmd(&session.gh, &session.token).args(["--method", "DELETE", &path]);
        let answer = match sys.run(&cmd) {
            Ok(ran) if ran.success() => continue,
            Ok(ran) => failure(&ran),
            Err(e) => e.to_string(),
        };
        answers.push((id, answer));
    }
    let after = held(sys)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::port::{Fake, Ran, Stat};

    /// Made up, in the measured shape: `gho_` and 36 of `[A-Za-z0-9_]`.
    const TOKEN: &str = "gho_ThisIsNotARealToken0123456789abcdefg"; // gitleaks:allow

    const GH: &str = "/opt/homebrew/bin/gh";

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

    /// A `gh api` GitHub refused, laid out as gh prints one: exit 1, the API's body on
    /// standard output, gh's own lines on standard error.
    fn refused(body: &str, stderr: &str) -> Ran {
        Ran {
            status: Some(1),
            stdout: body.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    fn link() -> Stat {
        Stat {
            uid: 501,
            gid: 80,
            mode: 0o755,
            dir: false,
            symlink: true,
            len: 26,
        }
    }

    /// A controller with gh at Homebrew's path, a token for `fleetbot`, signing in as
    /// `login`.
    fn controller(login: &str) -> Fake {
        let sys = Fake::default();
        sys.stat_of(GH, link());
        sys.on(
            &format!("{GH} auth token --hostname github.com --user fleetbot"),
            Ran::ok(&format!("{TOKEN}\n")),
        );
        sys.on(
            &format!("GH_TOKEN=\u{2026} {GH} api user --jq .login"),
            Ran::ok(&format!("{login}\n")),
        );
        sys
    }

    fn list_key() -> String {
        format!("GH_TOKEN=\u{2026} {GH} api user/keys --paginate --jq '.[] | [.id, .key] | @tsv'")
    }

    fn post_key(user: &str, body: &str) -> String {
        format!(
            "GH_TOKEN=\u{2026} {GH} api --method POST user/keys -f title=aterm-machines:lab-07:{user} \
             -f 'key=ssh-ed25519 {body}' --jq .id"
        )
    }

    #[test]
    fn every_api_call_carries_the_token_in_its_environment_only() {
        let cmd = api_cmd(Path::new(GH), TOKEN).args(["user"]);
        assert_eq!(cmd.env, [("GH_TOKEN".to_owned(), TOKEN.to_owned())]);
        assert!(cmd.args.iter().all(|a| a.to_string_lossy() != TOKEN));
        assert!(!cmd.display().contains(TOKEN));
    }

    #[test]
    fn a_new_key_is_posted_and_one_already_held_is_only_recorded() {
        let sys = controller("fleetbot");
        sys.on(
            &list_key(),
            Ran::ok(&format!(
                "100000101\tssh-ed25519 {}\n100000102\tssh-ed25519 {BOB_BODY}\n\
                 100000103\tssh-ed25519 {}\n",
                OTHER_BODIES[0], OTHER_BODIES[1]
            )),
        );
        let alice_body = ALICE_KEY.split(' ').nth(1).unwrap();
        sys.on(&post_key("alice", alice_body), Ran::ok("100000104\n"));
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
        assert_eq!(
            calls.iter().filter(|c| c.contains("--method POST")).count(),
            1
        );
        assert!(calls.iter().all(|c| !c.contains(TOKEN)), "{calls:?}");
        assert!(calls.iter().all(|c| !c.contains("auth switch")));
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
        assert!(!err.contains(TOKEN));
        assert!(sys.calls().iter().all(|c| !c.contains("user/keys")));
    }

    #[test]
    fn no_token_for_the_account_is_one_line_that_never_quotes_standard_output() {
        let sys = Fake::default();
        sys.stat_of(GH, link());
        // gh's measured answer (exit 1, this standard error), plus standard output it never
        // printed, to show that standard output is never quoted.
        sys.on(
            &format!("{GH} auth token --hostname github.com --user fleetbot"),
            Ran {
                status: Some(1),
                stdout: b"partial-secret".to_vec(),
                stderr: b"no oauth token found for github.com account fleetbot\n".to_vec(),
            },
        );
        let err = revoke(&sys, "fleetbot", &[1]).unwrap_err();
        assert!(err.contains("gh holds no token for fleetbot"), "{err}");
        assert!(
            err.ends_with("exit 1: no oauth token found for github.com account fleetbot"),
            "{err}"
        );
        assert!(!err.contains("partial-secret"), "{err}");
        assert_eq!(sys.calls().len(), 1);
    }

    #[test]
    fn a_token_github_no_longer_accepts_stops_at_the_login_check() {
        // gh prints a stored token GitHub refuses as it prints any other (exit 0); `GET /user`
        // is what answers 401.
        let sys = Fake::default();
        sys.stat_of(GH, link());
        sys.on(
            &format!("{GH} auth token --hostname github.com --user fleetbot"),
            Ran::ok(&format!("{TOKEN}\n")),
        );
        sys.on(
            &format!("GH_TOKEN=\u{2026} {GH} api user --jq .login"),
            refused(BAD_CREDENTIALS_BODY, "gh: Bad credentials (HTTP 401)\n"),
        );
        let err = register(
            &sys,
            "fleetbot",
            "lab-07",
            &[("alice".into(), ALICE_KEY.into())],
        )
        .unwrap_err();
        assert!(err.starts_with("gh api user: exit 1"), "{err}");
        assert!(err.contains("HTTP 401"), "{err}");
        assert_eq!(err.matches("Bad credentials").count(), 1, "{err}");
        assert!(!err.contains(TOKEN), "{err}");
        assert!(sys.calls().iter().all(|c| !c.contains("user/keys")));
    }

    #[test]
    fn a_refused_post_names_the_apis_reason() {
        let sys = controller("fleetbot");
        sys.on(&list_key(), Ran::ok(""));
        let alice_body = ALICE_KEY.split(' ').nth(1).unwrap();
        // No POST was sent to capture this, and GitHub's reference shows no body for this 422:
        // the `errors[]` entry is made up, in the layout of the measured 422 of `GET
        // search/repositories?q=` (compact body on standard output with `--jq` not applied,
        // `gh: Validation Failed (HTTP 422)` on standard error).
        sys.on(
            &post_key("alice", alice_body),
            refused(
                r#"{"message":"Validation Failed","errors":[{"resource":"PublicKey","code":"custom","field":"key","message":"key is already in use"}],"documentation_url":"https://docs.github.com/rest/users/keys#create-a-public-ssh-key-for-the-authenticated-user","status":"422"}"#,
                "gh: Validation Failed (HTTP 422)\n",
            ),
        );
        let err = register(
            &sys,
            "fleetbot",
            "lab-07",
            &[("alice".into(), ALICE_KEY.into())],
        )
        .unwrap_err();
        assert!(err.contains("HTTP 422"), "{err}");
        assert!(err.contains("key is already in use"), "{err}");
    }

    fn delete_key(id: u64) -> String {
        format!("GH_TOKEN=\u{2026} {GH} api --method DELETE user/keys/{id}")
    }

    /// What gh prints to standard error for a 404 from a token that lacks the scope: the
    /// measured status line, then its scope suggestion. The suggestion is gh 2.92.0's format
    /// string (`This API operation needs the %[1]q scope. To request it, run:  gh auth refresh
    /// -h %[2]s -s %[1]s`, after `gh: `), read from its binary, not a capture: every token on
    /// the controller it was measured on holds the scope. gh fills in the first scope of the
    /// answer's `X-Accepted-OAuth-Scopes` the token lacks, measured for `GET user/keys` and
    /// `GET user/keys/1` as `admin:public_key, read:public_key, write:public_key`.
    const NO_SCOPE: &str = "gh: Not Found (HTTP 404)\ngh: This API operation needs the \"admin:public_key\" scope. To request it, run:  gh auth refresh -h github.com -s admin:public_key\n";

    #[test]
    fn a_key_is_revoked_when_the_listing_no_longer_holds_it() {
        let sys = controller("fleetbot");
        // Before: …105 is held and …106 is already gone. After the delete: neither.
        sys.on(
            &list_key(),
            Ran::ok(&format!(
                "100000101\tssh-ed25519 {}\n100000105\tssh-ed25519 {BOB_BODY}\n",
                OTHER_BODIES[0]
            )),
        );
        sys.on(
            &list_key(),
            Ran::ok(&format!("100000101\tssh-ed25519 {}\n", OTHER_BODIES[0])),
        );
        sys.on(&delete_key(100_000_105), Ran::ok(""));
        assert_eq!(
            revoke(&sys, "fleetbot", &[100_000_105, 100_000_106]),
            Ok(vec![format!("ssh-ed25519 {BOB_BODY}")])
        );
        let calls = sys.calls();
        assert!(calls.contains(&delete_key(100_000_105)), "{calls:?}");
        assert!(
            !calls.contains(&delete_key(100_000_106)),
            "a key not held is not deleted"
        );
    }

    #[test]
    fn a_404_from_a_token_without_the_scope_is_not_a_key_already_gone() {
        let sys = controller("fleetbot");
        sys.on(
            &list_key(),
            Ran::ok(&format!("100000107\tssh-ed25519 {BOB_BODY}\n")),
        );
        sys.on(&delete_key(100_000_107), refused(NOT_FOUND_BODY, NO_SCOPE));
        let err = revoke(&sys, "fleetbot", &[100_000_107]).unwrap_err();
        assert!(err.starts_with("key 100000107: still registered"), "{err}");
        assert!(err.contains("HTTP 404"), "{err}");
        assert_eq!(err.matches("Not Found").count(), 1, "{err}");
        assert!(err.contains("-s admin:public_key"), "{err}");
    }

    #[test]
    fn a_listing_the_token_may_not_read_names_the_endpoint_and_the_missing_scope() {
        let sys = controller("fleetbot");
        sys.on(&list_key(), refused(NOT_FOUND_BODY, NO_SCOPE));
        let err = revoke(&sys, "fleetbot", &[7]).unwrap_err();
        assert!(err.starts_with("gh api user/keys: exit 1"), "{err}");
        assert!(err.contains("admin:public_key"), "{err}");
        assert!(sys.calls().iter().all(|c| !c.contains("DELETE")));
        assert_eq!(
            endpoint(&["--method", "POST", "user/keys", "-f", "title=x"]),
            "user/keys"
        );
    }

    #[test]
    fn a_listing_line_out_of_the_measured_shape_stops_the_revocation() {
        // A line the listing cannot read could hide a key still held, which a revocation
        // would then report gone.
        let sys = controller("fleetbot");
        sys.on(
            &list_key(),
            Ran::ok(&format!(
                "100000107\tssh-ed25519 {BOB_BODY}\nssh-ed25519 {}\n",
                OTHER_BODIES[1]
            )),
        );
        let err = revoke(&sys, "fleetbot", &[100_000_107]).unwrap_err();
        assert!(err.starts_with("gh api user/keys printed"), "{err}");
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
    fn gh_is_found_at_either_path_or_named_missing() {
        let sys = Fake::default();
        assert!(gh_path(&sys).unwrap_err().contains("gh is not installed"));
        sys.stat_of("/usr/local/bin/gh", link());
        assert_eq!(gh_path(&sys).unwrap(), PathBuf::from("/usr/local/bin/gh"));
        sys.stat_of(GH, link());
        assert_eq!(gh_path(&sys).unwrap(), PathBuf::from(GH));
    }

    #[test]
    fn an_account_that_is_not_a_login_is_refused_before_gh_runs() {
        let sys = Fake::default();
        sys.stat_of(GH, link());
        let err = revoke(&sys, "--user=x", &[1]).unwrap_err();
        assert!(err.contains("not a GitHub login"), "{err}");
        assert!(sys.calls().is_empty());
    }
}
