// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::port::Fake;

/// Made up; never a real token.
const TOKEN: &str = "fixture_machine_user_token_only";

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

/// A controller signed in to the scratch vault home, holding no `gh:fleet` token yet.
fn fake() -> Fake {
    let sys = Fake::default();
    super::super::credential::tests::in_vault(&sys, FLEET, None);
    sys
}

/// `bytes` as the vault's `gh:fleet` token, in this account's store.
fn stored(sys: &Fake, bytes: &str) {
    super::super::credential::tests::in_vault(sys, FLEET, Some(bytes.as_bytes()));
}

/// The `gh:fleet` entry.
const FLEET: &str = aterm_secrets::need::catalog::FLEET_GH_TOKEN_ENTRY;

/// A controller whose vault holds a `gh:fleet` token for `fleetbot` that signs in as `login`.
fn controller(login: &str) -> Fake {
    let sys = Fake::default();
    stored(&sys, &format!("{TOKEN}\n"));
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

/// No `gh:fleet` token, one others can read, or one that holds no single token blocks the key
/// call, which files the want with the lab and asks nobody. None of them sends anything, and
/// none runs git.
#[test]
fn no_usable_fleet_token_blocks_the_key_call_and_sends_nothing() {
    let sys = fake();
    let err = revoke(&sys, "fleetbot", &[1]).unwrap_err();
    assert!(
        err.contains("no usable gh:fleet") && err.contains("asked the lab's Macs"),
        "{err}"
    );
    assert!(sys.calls().is_empty(), "{:?}", sys.calls());
    for (bytes, unreadable, why) in [
        (TOKEN, true, "cannot be read here"),
        ("", false, "no single value"),
        ("first second", false, "no single value"),
    ] {
        let sys = Fake::default();
        stored(&sys, bytes);
        if unreadable {
            sys.vault_unreadable(std::io::ErrorKind::PermissionDenied);
        }
        let err = revoke(&sys, "fleetbot", &[1]).unwrap_err();
        assert!(err.contains(why), "{err}");
        assert!(err.contains("host key calls are blocked"), "{err}");
        assert!(!err.contains(TOKEN), "{err}");
        assert!(sys.calls().is_empty(), "{:?}", sys.calls());
    }
}

#[test]
fn a_token_github_no_longer_accepts_stops_at_the_login_check() {
    let sys = Fake::default();
    stored(&sys, TOKEN);
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
fn an_account_that_is_not_a_login_is_refused_before_anything_is_read() {
    let sys = Fake::default();
    stored(&sys, TOKEN);
    let err = revoke(&sys, "--user=x", &[1]).unwrap_err();
    assert!(err.contains("not a GitHub login"), "{err}");
    assert!(sys.calls().is_empty());
}
