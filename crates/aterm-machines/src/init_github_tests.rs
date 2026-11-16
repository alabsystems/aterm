// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::items::defaults::tests::{ctx, user};
use crate::port::{Fake, Stat};

const VALUE: &str = "fixture_secret_injected_only";

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

fn fixture() -> (Fake, Ctx) {
    let mut sys = Fake::root();
    sys.euid = 501;
    (sys, ctx(&[user("alice", 501)]))
}

/// `text` read as init.toml and its `[github]` table checked.
fn checked(text: &[u8]) -> Result<Asked, String> {
    decode(text).and_then(|profile| asked(profile.github))
}

/// With no init.toml, or one with no `[github]` table, init runs the first launch's own
/// repository stage; `enabled = false` turns it off; `account` (and `owner`) scope that stage.
/// The token is the vault's alone: a `token_file` or `token_key` is no longer a field, so a
/// table naming one is refused, as is any table that cannot be used, without echoing it.
#[test]
fn the_native_stage_is_the_default_and_init_toml_scopes_or_turns_it_off() {
    let (sys, context) = fixture();
    let ids = |sys: &Fake| -> Vec<String> {
        items(sys, &context, None, crate::model::Mode::Plan, &[])
            .unwrap()
            .iter()
            .map(|item| item.id())
            .collect()
    };
    // The default stage refreshes the checkouts already here after acquisition; an explicit
    // profile's checkouts are left as they are.
    let native: &[&str] = &[
        "init.github.keychain@alice",
        "init.github.native@alice",
        "init.github.refresh@alice",
    ];
    let scoped: &[&str] = &["init.github.keychain@alice", "init.github.native@alice"];
    assert_eq!(ids(&sys), native);
    for (text, want) in [
        (&b"[github]\nenabled = false\n"[..], &[][..]),
        (b"[develop]\ncheckouts = []\n", native),
        (b"[github]\naccount = 'alice'\n", scoped),
    ] {
        let (sys, _context) = fixture();
        let path = "/Users//alice/.config/aterm/init.toml";
        sys.file(path, text).stat_of(path, stat(0o600, text.len()));
        assert_eq!(ids(&sys), want, "{}", String::from_utf8_lossy(text));
    }
    match checked(b"[github]\naccount='alice'\nowner='team'\n").unwrap() {
        Asked::Scoped(config) => assert_eq!(
            (config.account.as_str(), config.owner.as_str()),
            ("alice", "team")
        ),
        _ => panic!("account and owner alone scope the native stage"),
    }
    for text in [
        format!("[github]\naccount='octocat'\nowner='octocat'\ntoken='{VALUE}'\n"),
        format!("[github]\naccount='octocat'\nowner='octocat'\ntoken_file='{VALUE}'\n"),
        "[github]\naccount='-bad'\nowner='octocat'".into(),
        "[github]\nowner='octocat'\n".into(),
        format!("[github]\naccount='octocat'\ntoken_key='{VALUE}'\n"),
    ] {
        let error = checked(text.as_bytes())
            .err()
            .expect("invalid profile refused");
        assert!(
            !error.contains(VALUE),
            "configuration values must not appear in errors"
        );
    }
}

#[test]
fn a_token_is_one_literal_without_evaluation() {
    assert_eq!(
        token(format!(" {VALUE}\n").as_bytes()).unwrap().bytes(),
        VALUE.as_bytes()
    );
    for text in [
        "GH_TOKEN=$(touch /tmp/should-never-exist)",
        "line\nbreak",
        "",
    ] {
        assert!(token(text.as_bytes()).is_err(), "{text:?}");
    }
}
