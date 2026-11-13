// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::engine::{self, NoJournal};
use crate::git_credential::script;
use crate::items::defaults::tests::{ctx, user};
use crate::model::{Mode, Outcome};
use crate::port::{Fake, GithubResponse, Stat};

const GIT: &str = crate::items::github::CLT_GIT;
const SOURCE: &str = "/Users//alice/.secrets/github";
const VALUE: &str = "fixture_secret_injected_only";
const OTHER: &str = "fixture_other_token_only";

fn stat(mode: u32, len: usize) -> Stat {
    Stat {
        uid: 501,
        gid: 20,
        mode,
        dir: false,
        symlink: false,
        len: len as u64,
    }
}
fn fixture() -> (Fake, Auth, Ctx) {
    let mut sys = Fake::root();
    sys.euid = 501;
    sys.stat_of(GIT, stat(0o755, 1));
    sys.file(SOURCE, VALUE.as_bytes())
        .stat_of(SOURCE, stat(0o600, VALUE.len()));
    let u = user("alice", 501);
    script::launcher(&sys, &u);
    let config = Github {
        account: "octocat".into(),
        owner: "octocat".into(),
        token_file: SOURCE.into(),
        token_key: None,
    };
    (
        sys,
        Auth {
            user: u.clone(),
            config,
            after: None,
        },
        ctx(&[u]),
    )
}
/// aterm's route over `auth`'s token file hands back `token`.
fn holds(sys: &Fake, auth: &Auth, token: &str) {
    let helper = auth.config.helper(sys, &auth.user).unwrap();
    script::hands_back(sys, &auth.user, &helper, token);
}
fn signs_in(sys: &Fake, u: &User, login: &str) {
    sys.on_github_api(
        u,
        "/user",
        Ok(GithubResponse {
            status: 200,
            headers: Vec::new(),
            body: format!("{{\"login\":\"{login}\"}}").into_bytes(),
        }),
    );
}
/// No call carries a token, none is gh's, and none asks the login keychain or writes git's
/// settings other than by replacing the keychain block.
fn clean(sys: &Fake) {
    let calls = sys.calls();
    assert!(
        calls.iter().all(|c| !c.contains(VALUE)
            && !c.contains(OTHER)
            && !c.contains("/gh")
            && !c.contains("approve")
            && !c.contains("osxkeychain")
            && !c.contains("--replace-all")
            && !c.contains("--global")),
        "{calls:?}"
    );
}

/// Nothing replaced in git's settings either.
fn unwritten(sys: &Fake) {
    clean(sys);
    assert!(
        sys.calls()
            .iter()
            .all(|c| !c.contains("--unset-all") && !c.contains("--add")),
        "{:?}",
        sys.calls()
    );
}

/// `text` read as init.toml and its `[github]` table checked.
fn checked(text: &[u8], user: &User) -> Result<Asked, String> {
    decode(text).and_then(|profile| asked(profile.github, user))
}

/// With no init.toml, or one with no `[github]` table, init runs the first launch's own
/// repository stage; `enabled = false` turns it off; `account` (and `owner`) alone scope that
/// stage; a `token_file` brings the profile's own credential and repository items. A table
/// that cannot be used is refused without echoing what it holds.
#[test]
fn the_native_stage_is_the_default_and_init_toml_scopes_replaces_or_turns_it_off() {
    let (sys, _auth, context) = fixture();
    let ids = |sys: &Fake| -> Vec<String> {
        items(sys, &context, None)
            .unwrap()
            .iter()
            .map(|item| item.id())
            .collect()
    };
    let native: &[&str] = &["init.github.keychain@alice", "init.github.native@alice"];
    assert_eq!(ids(&sys), native);
    let u = &context.users[0];
    for (text, want) in [
        (&b"[github]\nenabled = false\n"[..], &[][..]),
        (b"[develop]\ncheckouts = []\n", native),
        (b"[github]\naccount = 'alice'\n", native),
        (
            b"[github]\naccount='octocat'\nowner='octocat'\ntoken_file='~/.secrets/github'\n",
            &[
                "init.github.keychain@alice",
                "init.github.auth@alice",
                "init.github.repositories@alice",
            ][..],
        ),
    ] {
        let (sys, _auth, _context) = fixture();
        let path = "/Users//alice/.config/aterm/init.toml";
        sys.file(path, text).stat_of(path, stat(0o600, text.len()));
        assert_eq!(ids(&sys), want, "{}", String::from_utf8_lossy(text));
    }
    match checked(b"[github]\naccount='alice'\nowner='team'\n", u).unwrap() {
        Asked::Scoped(config) => {
            assert_eq!(
                (config.account.as_str(), config.owner.as_str()),
                ("alice", "team")
            );
            assert_eq!(
                config.token_file,
                Path::new("/Users//alice/.secrets/gh_access_token")
            );
        }
        _ => panic!("account and owner alone scope the native stage"),
    }
    match checked(
        b"[github]\naccount='octocat'\nowner='octocat'\ntoken_file='~/.secrets/github'\n",
        u,
    )
    .unwrap()
    {
        Asked::Profile(config) => assert_eq!(config.token_file, Path::new(SOURCE)),
        _ => panic!("a token_file is the profile's own source"),
    }
    for text in [
        format!("[github]\naccount='octocat'\nowner='octocat'\ntoken='{VALUE}'\n"),
        format!("[github]\naccount='octocat'\nowner='octocat'\ntoken_file='{VALUE}\n"),
        "[github]\naccount='-bad'\nowner='octocat'\ntoken_file='~/.secrets/github'".into(),
        "[github]\naccount='octocat'\nowner='octocat'\ntoken_file='../source'".into(),
        "[github]\naccount='octocat'\nowner='octocat'\ntoken_file='~/.aterm/release-credentials.toml'".into(),
        "[github]\nowner='octocat'\n".into(),
        format!("[github]\naccount='octocat'\ntoken_key='{VALUE}'\n"),
    ] {
        let error = checked(text.as_bytes(), u)
            .err()
            .expect("invalid profile refused");
        assert!(
            !error.contains(VALUE),
            "configuration values must not appear in errors"
        );
    }
}

#[test]
fn tokens_are_raw_or_explicit_literal_env_assignments_without_evaluation() {
    assert_eq!(
        token(format!(" {VALUE}\n").as_bytes(), None)
            .unwrap()
            .bytes(),
        VALUE.as_bytes()
    );
    assert_eq!(
        token(
            format!("# note\nOTHER=unused\nexport GH_TOKEN = '{VALUE}'\n").as_bytes(),
            Some("GH_TOKEN")
        )
        .unwrap()
        .bytes(),
        VALUE.as_bytes()
    );
    for text in [
        "GH_TOKEN=$(touch /tmp/should-never-exist)",
        "GH_TOKEN=\"${OTHER}\"",
        "GH_TOKEN='first'\nGH_TOKEN='second'",
        "OTHER=token",
        "GH_TOKEN='line\nbreak'",
        "GH_TOKEN=token # comment",
    ] {
        assert!(token(text.as_bytes(), Some("GH_TOKEN")).is_err());
    }
    assert!(token(b"", None).is_err());
    assert!(token(b"first\nsecond", None).is_err());
}

/// A GitHub that cannot vouch for the token stops the repositories, in a plan and an apply:
/// no inventory runs, no clone starts, and the keychain block still gives way, since that
/// waits for neither the token nor GitHub.
#[test]
fn configured_plan_waits_for_auth_before_inventory_and_failed_auth_blocks_clones() {
    for mode in [Mode::Plan, Mode::Apply] {
        let (sys, auth, context) = fixture();
        let u = auth.user.clone();
        let profile =
            b"[github]\naccount='octocat'\nowner='octocat'\ntoken_file='~/.secrets/github'\n";
        let path = "/Users//alice/.config/aterm/init.toml";
        sys.file(path, profile)
            .stat_of(path, stat(0o644, profile.len()));
        script::keychain_block(&sys, &u, "octocat");
        script::keychain_block(&sys, &u, "octocat");
        script::alike(&sys, &u);
        holds(&sys, &auth, VALUE);
        let helper = auth.config.helper(&sys, &u).unwrap();
        script::replaces_keychain_block_with(&sys, &u, &helper);
        sys.on_github_api(&u, "/user", Err("refused (output withheld)".into()));
        let rows = engine::run(
            &items(&sys, &context, None).unwrap(),
            mode,
            &sys,
            &context,
            &mut NoJournal,
        );
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows[0].outcome,
            if mode == Mode::Plan {
                Outcome::Planned
            } else {
                Outcome::Changed
            },
            "{rows:?}"
        );
        assert_eq!(rows[1].outcome, Outcome::Unknown, "{rows:?}");
        assert_eq!(rows[2].outcome, Outcome::Blocked, "{rows:?}");
        assert!(!aterm_json::to_string(&rows).unwrap().contains(VALUE));
        assert!(
            !sys.calls()
                .iter()
                .any(|c| c.contains("/repos") || c.contains(" clone "))
        );
        clean(&sys);
    }
}

/// The token file is the credential's one source: without it the row waits for it, and no
/// git or helper runs.
#[test]
fn a_missing_token_file_blocks_and_runs_nothing() {
    let (sys, mut auth, context) = fixture();
    auth.config.token_file = "/not-present".into();
    let Probe::Blocked { why } = auth.probe(&sys, &context) else {
        panic!("no token file is not ready");
    };
    assert!(why.contains("/not-present"), "{why}");
    assert!(
        sys.calls().iter().all(|c| !c.contains(GIT)),
        "{:?}",
        sys.calls()
    );
}

/// GitHub vouches for the token git hands back: another account's, a refusal and an answer
/// that is no account are never ready, and their output is withheld.
#[test]
fn the_token_identity_is_checked_by_github_and_all_failure_output_is_withheld() {
    for answer in [
        Ok(GithubResponse {
            status: 200,
            headers: Vec::new(),
            body: b"{\"login\":\"someone-else\"}".to_vec(),
        }),
        Err(format!("refused {VALUE}")),
        Ok(GithubResponse {
            status: 200,
            headers: Vec::new(),
            body: VALUE.as_bytes().to_vec(),
        }),
    ] {
        let (sys, auth, context) = fixture();
        let u = auth.user.clone();
        holds(&sys, &auth, VALUE);
        sys.on_github_api(&u, "/user", answer);
        let why = match auth.probe(&sys, &context) {
            Probe::Blocked { why } | Probe::Unusable { why } => why,
            other => panic!("{other:?}"),
        };
        assert!(!why.contains(VALUE), "{why}");
        assert!(auth_ready(&sys, &u, &auth.config).unwrap().is_none());
        unwritten(&sys);
    }
}

/// The row reads and writes nothing of git's settings: aterm's helper is asked once, for the
/// token file's token, and then GitHub. With no launcher there is no route: the row says so,
/// and neither is asked.
#[test]
fn the_auth_row_asks_atermss_helper_then_github_and_writes_nothing() {
    let (sys, auth, context) = fixture();
    let u = auth.user.clone();
    holds(&sys, &auth, VALUE);
    signs_in(&sys, &u, "octocat");
    assert!(matches!(auth.probe(&sys, &context), Probe::Ok { .. }));
    let calls = sys.calls();
    let token_file = "--token-file /Users//alice/.secrets/github";
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert!(
        calls[0].contains(token_file) && calls[0].ends_with("credential fill"),
        "{calls:?}"
    );
    assert_eq!(calls[1], "alice$ agh GET /user");
    unwritten(&sys);

    let mut bare = Fake::root();
    bare.euid = 501;
    bare.stat_of(GIT, stat(0o755, 1));
    bare.file(SOURCE, VALUE.as_bytes())
        .stat_of(SOURCE, stat(0o600, VALUE.len()));
    let Probe::Blocked { why } = auth.probe(&bare, &context) else {
        panic!("no launcher is no route");
    };
    assert!(why.contains(".local/bin/aterm"), "{why}");
    assert!(bare.calls().is_empty(), "{:?}", bare.calls());
}

#[test]
fn unsafe_token_files_are_never_ready() {
    let (sys, auth, context) = fixture();
    sys.stat_of(SOURCE, stat(0o644, VALUE.len()));
    assert!(matches!(auth.probe(&sys, &context), Probe::Blocked { .. }));
    unwritten(&sys);
}

#[test]
fn git_lookups_that_already_hand_back_the_token_file_are_left_as_they_are() {
    let (sys, auth, context) = fixture();
    let u = auth.user.clone();
    holds(&sys, &auth, VALUE);
    signs_in(&sys, &u, "Octocat");
    let ready = auth_ready(&sys, &u, &auth.config).unwrap();
    assert!(ready.is_some_and(|token| token.bytes() == VALUE.as_bytes()));
    let rows = engine::run(
        &[Box::new(auth)],
        Mode::Apply,
        &sys,
        &context,
        &mut NoJournal,
    );
    assert_eq!(rows[0].outcome, Outcome::Ok, "{rows:?}");
    unwritten(&sys);
}

/// Lookups that hand back another token, or one GitHub signs in as another account, refuses
/// or cannot vouch for, are not ready, and nothing is written: aterm never stores a token for
/// git, in the keychain or anywhere else.
#[test]
fn git_lookups_no_one_vouches_for_are_not_ready_and_nothing_is_written() {
    let refused = || {
        Ok(GithubResponse {
            status: 401,
            headers: Vec::new(),
            body: Vec::new(),
        })
    };
    let cases: [(&str, Result<GithubResponse, String>, bool); 4] = [
        (
            OTHER,
            Ok(GithubResponse {
                status: 200,
                headers: Vec::new(),
                body: b"{\"login\":\"octocat\"}".to_vec(),
            }),
            false,
        ),
        (
            VALUE,
            Ok(GithubResponse {
                status: 200,
                headers: Vec::new(),
                body: b"{\"login\":\"someone-else\"}".to_vec(),
            }),
            false,
        ),
        (VALUE, refused(), false),
        (VALUE, Err("timed out (output withheld)".into()), true),
    ];
    for (held, answer, unusable) in cases {
        let (sys, auth, context) = fixture();
        let u = auth.user.clone();
        holds(&sys, &auth, held);
        sys.on_github_api(&u, "/user", answer);
        assert!(auth_ready(&sys, &u, &auth.config).unwrap().is_none());
        let probed = auth.probe(&sys, &context);
        if unusable {
            assert!(matches!(probed, Probe::Unusable { .. }), "{probed:?}");
        } else {
            assert!(matches!(probed, Probe::Blocked { .. }), "{probed:?}");
        }
        unwritten(&sys);
    }
    let (sys, auth, context) = fixture();
    let u = context.users[0].clone();
    holds(&sys, &auth, VALUE);
    sys.on_github_api(&u, "/user", Err("timed out (output withheld)".into()));
    let profile = b"[github]\naccount='octocat'\nowner='octocat'\ntoken_file='~/.secrets/github'\n";
    let path = "/Users//alice/.config/aterm/init.toml";
    sys.file(path, profile)
        .stat_of(path, stat(0o644, profile.len()));
    assert_eq!(
        aterm_checkouts(&sys, &context).unwrap(),
        Vec::<PathBuf>::new(),
        "a develop pass lists no checkouts rather than failing whole"
    );
    clean(&sys);
}

#[test]
fn without_clt_git_the_bootstrap_names_the_item_that_installs_it() {
    let (sys, auth, context) = fixture();
    let sys = {
        let mut bare = Fake::root();
        bare.euid = sys.euid;
        bare
    };
    let Probe::Unusable { why } = auth.probe(&sys, &context) else {
        panic!("no git is not ready");
    };
    assert!(why.contains("aterm init --only base.clt"), "{why}");
    assert!(
        !why.contains("xcode-select") && !why.contains("brew"),
        "{why}"
    );
}
