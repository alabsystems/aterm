// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::items::defaults::tests::user;
use crate::port::spawns::Spawns;
use crate::port::{Fake, Live, Ran, Scratch};

const TOKEN: &str = "fixture_credential_token_only";

/// Where `secret` shows in what `live` spawned for the runs of `record`: the program,
/// argument, variable or standard input that holds it, or `None`.
fn exposed(record: &[(Cmd, Option<Vec<u8>>)], live: &Live, secret: &str) -> Option<String> {
    for (cmd, input) in record {
        let (program, args, env) = live.spawned(cmd).unwrap();
        if program.to_string_lossy().contains(secret) {
            return Some(format!("program of {}", cmd.display()));
        }
        if let Some(arg) = args.iter().find(|a| a.to_string_lossy().contains(secret)) {
            return Some(format!("argument {arg:?}"));
        }
        if let Some((key, _)) = env.iter().find(|(_, value)| value.contains(secret)) {
            return Some(format!("variable {key}"));
        }
        if input
            .as_deref()
            .is_some_and(|i| i.windows(secret.len()).any(|w| w == secret.as_bytes()))
        {
            return Some(format!("standard input of {}", cmd.display()));
        }
    }
    None
}

fn alice() -> User {
    user("alice", 501)
}

/// git's URL matching as [`scope`] reads it;
/// `the_url_matcher_agrees_with_what_git_runs_for_a_github_lookup` checks the hard spellings
/// against the lookups a real git runs a helper under each for.
#[test]
fn a_setting_applies_to_github_as_git_matches_its_url() {
    for (url, want) in [
        (None, Scope::Every),
        (Some("https://github.com"), Scope::Every),
        (Some("https://github.com/"), Scope::Every),
        (Some("https://GitHub.com"), Scope::Every),
        (Some("HTTPS://github.com"), Scope::Every),
        (Some("https://*.com"), Scope::Every),
        (Some("https://*.*"), Scope::Every),
        (Some("https://github.com:443"), Scope::Every),
        (Some("https://github.com:"), Scope::Every),
        (Some("https://%67ithub.com"), Scope::Every),
        (Some("https://github.com."), Scope::Every),
        (Some("https://GITHUB.COM."), Scope::Every),
        (Some("https://github.com.:443"), Scope::Every),
        (Some("HTTPS://github.com."), Scope::Every),
        (Some("https://github.com.:0443"), Scope::Every),
        (Some("https://github.com.:"), Scope::Every),
        (Some("https://*.*."), Scope::Every),
        (Some("https://%67ithub.com/"), Scope::Every),
        (Some("github.com"), Scope::Every),
        (Some("github.com/"), Scope::Every),
        (Some("github.com//"), Scope::Every),
        (Some("https://%67ithub.com//"), Scope::Every),
        (Some("://github.com"), Scope::Every),
        (Some("https://"), Scope::Every),
        (Some("://"), Scope::Every),
        (Some(""), Scope::Every),
        (Some("https://bot@github.com"), Scope::Part),
        (Some("https://bot:pw@github.com"), Scope::Part),
        (Some("https://github.com/owner"), Scope::Part),
        (Some("https://github.com?x"), Scope::Part),
        (Some("https://github.com./owner"), Scope::Part),
        (Some("https://github.com.:443/owner"), Scope::Part),
        (Some("https://*.com./owner"), Scope::Part),
        (Some("github.com/owner/repo.git"), Scope::Part),
        (Some("bot@github.com"), Scope::Part),
        (Some("https://bot@%67ithub.com"), Scope::Part),
        (Some("/owner"), Scope::Part),
        (Some("https:///owner"), Scope::Part),
        (Some("https://github.com//"), Scope::Part),
        (Some("HTTPS://GITHUB.COM.:443//"), Scope::Part),
        (Some("https://github.com///"), Scope::Part),
        (Some("http://github.com"), Scope::Never),
        (Some("ssh://github.com"), Scope::Never),
        (Some("https://github.com:8443"), Scope::Never),
        (Some("https://github.com:x"), Scope::Never),
        (Some("https://gist.github.com"), Scope::Never),
        (Some("https://github.com.example.invalid"), Scope::Never),
        (Some("https://github.co"), Scope::Never),
        (Some("https://github.com../owner"), Scope::Never),
        (Some("https://github.com%2E"), Scope::Never),
        (Some("https://."), Scope::Never),
        (Some("https://./owner"), Scope::Never),
        (Some("https://*"), Scope::Never),
        (Some("https://[::1]"), Scope::Never),
        (Some("example.invalid"), Scope::Never),
        (Some("github.com."), Scope::Never),
        (Some("github.com./owner"), Scope::Never),
        (Some("bot@github.com."), Scope::Never),
        (Some("GITHUB.COM"), Scope::Never),
        (Some("*.com"), Scope::Never),
        (Some("github.com:443"), Scope::Never),
        (Some("github.com:"), Scope::Never),
        (Some("://github.com."), Scope::Never),
        (Some("://GITHUB.COM"), Scope::Never),
        (Some("https://%47ITHUB.COM"), Scope::Never),
        (Some("HTTPS://%67ithub.com"), Scope::Never),
        (Some("https://%67ithub.com:443"), Scope::Never),
        (Some("https://%67ithub.com."), Scope::Never),
        (Some("https://*.%63om"), Scope::Never),
        (Some("https://%2a.com"), Scope::Never),
        (Some("https://github.com:443."), Scope::Never),
        (Some("HTTPS://"), Scope::Never),
        (Some("http://"), Scope::Never),
    ] {
        assert_eq!(scope(url), want, "{url:?}");
    }
}

/// `(key, value)` pairs as settings of the file `/f`.
fn listed(entries: &[(&str, Option<&str>)]) -> Vec<Setting> {
    entries
        .iter()
        .map(|(key, value)| Setting {
            origin: "file:/f".into(),
            key: (*key).into(),
            value: value.map(str::to_owned),
        })
        .collect()
}

#[test]
fn a_read_is_records_of_a_file_a_key_and_a_value() {
    assert_eq!(
        parse("file:/a\0credential.helper\nosxkeychain\0file:/b\0credential.usehttppath\0"),
        Some(vec![
            Setting {
                origin: "file:/a".into(),
                key: "credential.helper".into(),
                value: Some("osxkeychain".into()),
            },
            Setting {
                origin: "file:/b".into(),
                key: "credential.usehttppath".into(),
                value: None,
            },
        ])
    );
    assert_eq!(parse(""), Some(Vec::new()));
    assert!(parse("file:/a\0").is_none());
    assert!(parse("file:/a").is_none());
}

/// A conditional include's file resolves as git resolves it, each once. Negative control:
/// one this cannot resolve, and one nested past git's bound, are reasons, not skipped.
#[test]
fn a_conditional_include_resolves_as_git_resolves_it() {
    let who = alice();
    let include = |origin: &str, value: Option<&str>| Setting {
        origin: origin.into(),
        key: "includeif.gitdir:~/work/.path".into(),
        value: value.map(str::to_owned),
    };
    let (mut named, mut pending) = (BTreeSet::new(), Vec::new());
    let settings = [
        include("file:/Users//alice/.gitconfig", Some("~/work.inc")),
        include("file:/Users//alice/.config/git/config", Some("work.inc")),
        include("file:/etc/gitconfig", Some("/opt/shared.inc")),
        include("file:/Users//alice/.gitconfig", Some("~/work.inc")),
        listed(&[("credential.helper", Some("x"))]).remove(0),
    ];
    includes(&settings, &who, 1, &mut named, &mut pending).unwrap();
    assert_eq!(
        pending,
        [
            (PathBuf::from("/Users//alice/work.inc"), 1),
            (PathBuf::from("/Users//alice/.config/git/work.inc"), 1),
            (PathBuf::from("/opt/shared.inc"), 1),
        ]
    );
    for unresolvable in [
        include("file:/Users//alice/.gitconfig", Some("~other/work.inc")),
        include("file:/Users//alice/.gitconfig", Some("%(prefix)/work.inc")),
        include("command line:", Some("work.inc")),
        include("file:/Users//alice/.gitconfig", None),
    ] {
        let why = includes(&[unresolvable], &who, 1, &mut named, &mut pending).unwrap_err();
        assert!(why.contains("cannot resolve"), "{why}");
    }
    let deep = include("file:/Users//alice/.gitconfig", Some("/deep.inc"));
    assert!(
        includes(
            std::slice::from_ref(&deep),
            &who,
            INCLUDE_DEPTH,
            &mut named,
            &mut pending
        )
        .is_ok()
    );
    let why = includes(&[deep], &who, INCLUDE_DEPTH + 1, &mut named, &mut pending).unwrap_err();
    assert!(why.contains("deeper than git reads"), "{why}");
}

/// [`keychain_block`] where aterm's helper has a token here.
fn block_of(home: &str, every: &[Setting], macos: bool) -> Option<KeychainBlock> {
    keychain_block(home, every, macos, &|| true)
}

/// The replacement of `block`, which is in one file.
fn only(block: &KeychainBlock) -> &Replacement {
    let [replacement] = block.replacements.as_slice() else {
        panic!("one file: {block:?}");
    };
    replacement
}

/// The keys `block`, which is in one file, removes and writes.
fn keys(block: &KeychainBlock) -> (Vec<&'static str>, Vec<&'static str>) {
    let replacement = only(block);
    (replacement.unset.clone(), replacement.write.clone())
}

/// `(file, key, value)` settings as git prints them.
fn at(entries: &[(&str, &str, Option<&str>)]) -> Vec<Setting> {
    entries
        .iter()
        .map(|(file, key, value)| Setting {
            origin: format!("file:{file}"),
            key: (*key).into(),
            value: value.map(str::to_owned),
        })
        .collect()
}

const GLOBAL: &str = "/Users//alice/.gitconfig";
const XDG: &str = "/Users//alice/.config/git/config";

/// The block an earlier `aterm init` wrote, there in `~/.gitconfig`, as git prints it.
const BLOCK: [(&str, &str, Option<&str>); 4] = [
    (GLOBAL, HELPER, Some("")),
    (GLOBAL, HELPER, Some(OSXKEYCHAIN)),
    (GLOBAL, USERNAME, Some("alice")),
    (GLOBAL, USE_PATH, Some("false")),
];

/// The vault's helper in the XDG file, after CLT's own keychain helper.
const VAULT: [(&str, &str, Option<&str>); 3] = [
    (script::CLT_CONFIG, "credential.helper", Some(OSXKEYCHAIN)),
    (XDG, HELPER, Some("")),
    (XDG, HELPER, Some("!f() { :; }; f")),
];

/// The keychain block `aterm init` wrote on a Mac is found by its settings alone: its keys
/// are removed, github.com's helper key gets aterm's route, and the revert removes every
/// key the replacement touches before it adds the block back in its order. It is found in
/// either file `git config --global` writes, and replaced there; a block in each goes in one
/// pass. A value of anyone else's under its keys, or the block in a third file, is not it,
/// and neither is the vault's own section in the XDG file. A keychain helper
/// is in effect for github.com when it is still in the list once git's resets apply,
/// however it is spelled; one a reset drops, or one for another host, is not.
#[test]
fn the_keychain_block_and_a_keychain_helper_in_effect_are_read_from_settings_alone() {
    let home = "/Users//alice";
    let mut written = VAULT.to_vec();
    written.extend(BLOCK);
    written.push((GLOBAL, GIST_HELPER, Some("!gh")));
    written.push((GLOBAL, "user.name", Some("Alice")));
    let block = block_of(home, &at(&written), true).expect("the block init wrote");
    assert_eq!(
        only(&block).file,
        Path::new(GLOBAL),
        "the vault's file stays"
    );
    assert_eq!(only(&block).unset, [HELPER, USERNAME, USE_PATH]);
    assert_eq!(
        only(&block).write,
        [HELPER],
        "someone else's gist helper stays"
    );
    assert_eq!(
        block.revert(),
        "git config --file /Users//alice/.gitconfig --unset-all credential.https://github.com.helper; \
         git config --file /Users//alice/.gitconfig --unset-all credential.https://github.com.username; \
         git config --file /Users//alice/.gitconfig --unset-all credential.https://github.com.usehttppath; \
         git config --file /Users//alice/.gitconfig --add credential.https://github.com.helper ''; \
         git config --file /Users//alice/.gitconfig --add credential.https://github.com.helper osxkeychain; \
         git config --file /Users//alice/.gitconfig --add credential.https://github.com.username alice; \
         git config --file /Users//alice/.gitconfig --add credential.https://github.com.usehttppath false"
    );
    assert_eq!(
        keychain_helper_in_effect(&at(&written)).as_deref(),
        Some("credential.https://github.com.helper in /Users//alice/.gitconfig")
    );
    // With no gist.github.com section, its helper key gets aterm's route too.
    let mut bare = VAULT.to_vec();
    bare.extend(BLOCK);
    let block = block_of(home, &at(&bare), true).expect("the block init wrote");
    assert_eq!(only(&block).write, [HELPER, GIST_HELPER]);
    assert!(
        block
            .revert()
            .contains("--unset-all credential.https://gist.github.com.helper"),
        "{}",
        block.revert()
    );
    // Negative controls: someone else's value under the block's keys, the block without the
    // keychain, or in another file.
    for (label, extra) in [
        ("another helper", (GLOBAL, HELPER, Some("!other"))),
        ("useHttpPath on", (GLOBAL, USE_PATH, Some("true"))),
        ("a key with no value", (GLOBAL, USERNAME, None)),
    ] {
        let mut theirs = written.clone();
        theirs.push(extra);
        assert_eq!(block_of(home, &at(&theirs), true), None, "{label}");
    }
    let unkeyed: Vec<_> = BLOCK
        .iter()
        .copied()
        .filter(|(_, _, value)| *value != Some(OSXKEYCHAIN))
        .collect();
    assert_eq!(block_of(home, &at(&unkeyed), true), None);
    // The other file `git config --global` writes: the block is found and replaced there,
    // and a section the first file holds nothing of is that file's to fill. A third file is
    // no file of init's, the person's own `$XDG_CONFIG_HOME` among them.
    let moved = |file| -> Vec<_> {
        BLOCK
            .iter()
            .map(|(_, key, value)| (file, *key, *value))
            .collect()
    };
    let mut xdg = moved(XDG);
    xdg.push((GLOBAL, GIST_HELPER, Some("!other")));
    let block = block_of(home, &at(&xdg), true).expect("the block in the XDG file");
    assert_eq!(only(&block).file, Path::new(XDG));
    assert_eq!(only(&block).write, [HELPER, GIST_HELPER]);
    assert!(
        block
            .revert()
            .contains(&format!("--file {XDG} --add {HELPER} osxkeychain"))
            && !block.revert().contains(GLOBAL),
        "{}",
        block.revert()
    );
    for file in ["/Users//alice/xdg/git/config", "/Users//alice/work.inc"] {
        assert_eq!(block_of(home, &at(&moved(file)), true), None, "{file}");
    }
    let mut both = moved(XDG);
    both.extend(BLOCK);
    let block = block_of(home, &at(&both), true).expect("a block in each file");
    let files: Vec<&Path> = block
        .replacements
        .iter()
        .map(|replacement| replacement.file.as_path())
        .collect();
    assert_eq!(files, [Path::new(XDG), Path::new(GLOBAL)], "git's order");
    assert!(
        block.revert().contains(&format!("--file {XDG} --add"))
            && block.revert().contains(&format!("--file {GLOBAL} --add")),
        "{}",
        block.revert()
    );
    // The vault's section in the XDG file is someone else's, as the file leaves it for both
    // hosts, and it is no reason to read a block into the other file.
    let agh = "!\"/Users//alice/.local/bin/agh\" auth git-credential";
    let vault = [
        (XDG, HELPER, Some("")),
        (XDG, HELPER, Some(agh)),
        (XDG, GIST_HELPER, Some("")),
        (XDG, GIST_HELPER, Some(agh)),
    ];
    assert_eq!(
        block_of(home, &at(&vault), true),
        None,
        "the vault's section"
    );
    assert_eq!(
        block_of(home, &at(&VAULT), true),
        None,
        "the vault's section"
    );
    let other_case = [(
        GLOBAL,
        "credential.https://GitHub.com.helper",
        Some(OSXKEYCHAIN),
    )];
    assert_eq!(
        block_of(home, &at(&other_case), true),
        None,
        "a key git's --unset-all of the block's own would not remove is not the block"
    );
    // Every generation of init wrote the reset first, so a section without it is a person's:
    // left as it is, whichever helper it holds, and still named.
    for (label, lone) in [
        ("a lone osxkeychain", (GLOBAL, HELPER, Some(OSXKEYCHAIN))),
        (
            "a lone gh helper",
            (
                GLOBAL,
                HELPER,
                Some("!/opt/homebrew/bin/gh auth git-credential"),
            ),
        ),
    ] {
        assert_eq!(block_of(home, &at(&[lone]), true), None, "{label}");
        assert!(keychain_helper_in_effect(&at(&[lone])).is_some(), "{label}");
    }
    let gist = [(GLOBAL, GIST_HELPER, Some("!gh auth git-credential"))];
    assert_eq!(block_of(home, &at(&gist), true), None, "a lone gist helper");
    let reset_last = [
        (GLOBAL, HELPER, Some(OSXKEYCHAIN)),
        (GLOBAL, HELPER, Some("")),
    ];
    assert_eq!(block_of(home, &at(&reset_last), true), None, "reset last");

    assert_eq!(keychain_helper_in_effect(&at(&VAULT)), None);
    for (label, helper) in [
        (
            "a later generic osxkeychain",
            ("credential.helper", "osxkeychain"),
        ),
        (
            "its path",
            (
                "credential.https://github.com.helper",
                "/opt/git/libexec/git-core/git-credential-osxkeychain",
            ),
        ),
        (
            "a shell command",
            ("credential.helper", "!git credential-osxkeychain"),
        ),
        (
            "an argument",
            ("credential.helper", "osxkeychain --stand-in"),
        ),
        (
            "another spelling of the host",
            ("credential.https://GitHub.com.helper", " osxkeychain "),
        ),
    ] {
        let mut with = VAULT.to_vec();
        with.push((GLOBAL, helper.0, Some(helper.1)));
        assert_eq!(
            keychain_helper_in_effect(&at(&with)).as_deref(),
            Some(format!("{} in {GLOBAL}", helper.0).as_str()),
            "{label}"
        );
    }
    let mut with = VAULT.to_vec();
    with.push((GLOBAL, GIST_HELPER, Some(OSXKEYCHAIN)));
    assert_eq!(keychain_helper_in_effect(&at(&with)), None, "another host");
    assert_eq!(
        keychain_helper_in_effect(&at(&VAULT[..1])).as_deref(),
        Some(format!("credential.helper in {}", script::CLT_CONFIG).as_str()),
        "CLT's own helper, with no reset after it"
    );
}

/// GitHub CLI's helper, which hands back the token gh keeps in the login keychain, is a
/// keychain helper however a command runs it; the vault's `agh` helper and other gh verbs
/// are not. What `gh auth setup-git` wrote for an `aterm init` before 2026-10-22 (the reset
/// and GitHub CLI's helper, for github.com and for gist.github.com) is a keychain block on a
/// Mac, each section judged on its own. Negative controls: GitHub CLI's helper inside
/// someone else's command, beside someone else's helper, or off a Mac (where no `aterm init`
/// ran gh), is not the block.
#[test]
fn github_clis_helper_is_a_keychain_helper_and_its_setup_git_block_is_replaced() {
    const GH: &str = "!/opt/homebrew/bin/gh auth git-credential";
    const QUOTED: &str = "!\"/opt/homebrew/bin/gh\" auth git-credential";
    const WRAPPED: &str = "!f() { gh auth git-credential \"$@\"; }; f";
    for (label, value) in [
        ("as gh auth setup-git writes it", GH),
        ("its path quoted", QUOTED),
        ("on PATH", "!gh auth git-credential"),
        ("in a shell function", WRAPPED),
    ] {
        let mut with = VAULT.to_vec();
        with.push((GLOBAL, HELPER, Some(value)));
        assert_eq!(
            keychain_helper_in_effect(&at(&with)).as_deref(),
            Some(format!("{HELPER} in {GLOBAL}").as_str()),
            "{label}"
        );
    }
    for (label, value) in [
        (
            "the vault's agh",
            "!\"/Users//alice/.local/bin/agh\" auth git-credential",
        ),
        ("aterm's agh", "!aterm agh auth git-credential"),
        ("another gh verb", "!gh auth token"),
        (
            "a program named after gh",
            "!gh-stand-in auth git-credential",
        ),
    ] {
        let mut with = VAULT.to_vec();
        with.push((GLOBAL, HELPER, Some(value)));
        assert_eq!(keychain_helper_in_effect(&at(&with)), None, "{label}");
    }

    let home = "/Users//alice";
    let setup_git = [
        (GLOBAL, HELPER, Some("")),
        (GLOBAL, HELPER, Some(GH)),
        (GLOBAL, GIST_HELPER, Some("")),
        (GLOBAL, GIST_HELPER, Some(QUOTED)),
    ];
    let mut written = VAULT.to_vec();
    written.extend(setup_git);
    written.push((GLOBAL, "user.name", Some("Alice")));
    let block = block_of(home, &at(&written), true).expect("the block gh setup-git wrote");
    assert_eq!(
        only(&block).unset,
        [HELPER, USERNAME, USE_PATH, GIST_HELPER]
    );
    assert_eq!(only(&block).write, [HELPER, GIST_HELPER]);
    assert_eq!(
        block.revert(),
        "git config --file /Users//alice/.gitconfig --unset-all credential.https://github.com.helper; \
         git config --file /Users//alice/.gitconfig --unset-all credential.https://github.com.username; \
         git config --file /Users//alice/.gitconfig --unset-all credential.https://github.com.usehttppath; \
         git config --file /Users//alice/.gitconfig --unset-all credential.https://gist.github.com.helper; \
         git config --file /Users//alice/.gitconfig --add credential.https://github.com.helper ''; \
         git config --file /Users//alice/.gitconfig --add credential.https://github.com.helper '!/opt/homebrew/bin/gh auth git-credential'; \
         git config --file /Users//alice/.gitconfig --add credential.https://gist.github.com.helper ''; \
         git config --file /Users//alice/.gitconfig --add credential.https://gist.github.com.helper '!\"/opt/homebrew/bin/gh\" auth git-credential'"
    );
    assert_eq!(
        block_of(home, &at(&written), false),
        None,
        "off a Mac no aterm init ran gh"
    );
    // gist.github.com's half alone, where init's later block replaced github.com's: both
    // hosts get aterm's route, github.com's holding nothing in ~/.gitconfig.
    let mut gist = VAULT.to_vec();
    gist.extend(&setup_git[2..]);
    let block = block_of(home, &at(&gist), true).expect("gist.github.com's half");
    assert_eq!(keys(&block), (vec![GIST_HELPER], vec![HELPER, GIST_HELPER]));
    // Negative controls: GitHub CLI's helper in someone else's command is in effect but
    // not the block; beside someone else's helper, its section is someone else's and keeps
    // its helper key.
    let mut wrapped = VAULT.to_vec();
    wrapped.extend([(GLOBAL, HELPER, Some("")), (GLOBAL, HELPER, Some(WRAPPED))]);
    assert_eq!(block_of(home, &at(&wrapped), true), None);
    assert!(keychain_helper_in_effect(&at(&wrapped)).is_some());
    let mut beside = written.clone();
    beside.push((GLOBAL, GIST_HELPER, Some("!other")));
    let block = block_of(home, &at(&beside), true).expect("github.com's section");
    assert_eq!(
        keys(&block),
        (vec![HELPER, USERNAME, USE_PATH], vec![HELPER])
    );
}

/// Off a Mac, the block `aterm init` wrote from 2026-10-26 (the reset, `store`, an account
/// and `useHttpPath` off) is found whole and replaced like the Mac's; `~/.git-credentials`,
/// the person's file, is not read or touched. Negative controls: a lone `store`, the block
/// missing a line, and the block on a Mac (where init never wrote it) are someone else's.
#[test]
fn the_store_block_init_wrote_elsewhere_is_found_whole() {
    let home = "/Users//alice";
    let vault = [
        (XDG, HELPER, Some("")),
        (XDG, HELPER, Some("!f() { :; }; f")),
    ];
    let store = [
        (GLOBAL, HELPER, Some("")),
        (GLOBAL, HELPER, Some(STORE)),
        (GLOBAL, USERNAME, Some("alice")),
        (GLOBAL, USE_PATH, Some("false")),
    ];
    let mut written = vault.to_vec();
    written.extend(store);
    let block = block_of(home, &at(&written), false).expect("the block init wrote");
    assert_eq!(only(&block).unset, [HELPER, USERNAME, USE_PATH]);
    assert_eq!(only(&block).write, [HELPER, GIST_HELPER]);
    assert!(
        !block.revert().contains("git-credentials"),
        "{}",
        block.revert()
    );
    assert_eq!(block_of(home, &at(&written), true), None, "on a Mac");
    for (label, drop) in [("no reset", 0), ("no account", 2), ("useHttpPath unset", 3)] {
        let mut part = vault.to_vec();
        part.extend(
            store
                .iter()
                .enumerate()
                .filter(|(at, _)| *at != drop)
                .map(|(_, setting)| *setting),
        );
        assert_eq!(block_of(home, &at(&part), false), None, "{label}");
    }
    let mut lone = vault.to_vec();
    lone.push((GLOBAL, HELPER, Some(STORE)));
    assert_eq!(block_of(home, &at(&lone), false), None, "a lone store");
    let mut mac = vault.to_vec();
    mac.extend(BLOCK);
    assert_eq!(
        block_of(home, &at(&mac), false),
        None,
        "the Mac's block elsewhere"
    );
}

/// The keychain helper the person's own git still asks for github.com is read from the
/// settings alone, wherever it is: in effect for every lookup, scoped to some, or in a file
/// a conditional include names. Negative controls: the vault's helper alone, and a scoped
/// setting that names no keychain helper, are none; no helper runs for any read.
#[test]
fn a_keychain_helper_in_effect_is_read_without_running_one() {
    let who = alice();
    let mac = || {
        let sys = Fake::default();
        sys.executable(crate::items::github::CLT_GIT, b"git");
        sys
    };
    let work = "/Users//alice/work.inc";
    let vault = script::printed(&[(XDG, HELPER, ""), (XDG, HELPER, "!f() { :; }; f")]);
    for (label, sys, found, reads) in [
        (
            "GitHub CLI's helper for gist.github.com alone",
            {
                let sys = mac();
                let mut text = vault.clone();
                text.push_str(&script::printed(&[
                    (GLOBAL, GIST_HELPER, ""),
                    (GLOBAL, GIST_HELPER, "!gh auth git-credential"),
                ]));
                sys.on(&script::helpers(&who), Ran::ok(&text));
                sys
            },
            Some(format!("{GIST_HELPER} in {GLOBAL}")),
            vec![script::helpers(&who)],
        ),
        (
            "aterm's helper for gist.github.com",
            {
                let sys = mac();
                let mut text = vault.clone();
                text.push_str(&script::printed(&[
                    (GLOBAL, GIST_HELPER, ""),
                    (GLOBAL, GIST_HELPER, "!aterm secrets git-credential"),
                ]));
                sys.on(&script::helpers(&who), Ran::ok(&text));
                sys
            },
            None,
            vec![script::helpers(&who)],
        ),
        (
            "an include git refuses to read is a reason, never an error",
            {
                let sys = mac();
                let mut text = vault.clone();
                text.push_str(&script::printed(&[(
                    GLOBAL,
                    "includeif.gitdir:~/work/.path",
                    "~/work.inc",
                )]));
                sys.on(&script::helpers(&who), Ran::ok(&text));
                sys.on(
                    &script::included(&who, work),
                    Ran::exit(128, "fatal: bad config line 1"),
                );
                sys
            },
            Some(format!(
                "{work}: cannot read git's credential settings (output withheld)"
            )),
            vec![script::helpers(&who), script::included(&who, work)],
        ),
        (
            "settings git cannot be run to read are a reason too",
            mac(),
            Some("cannot completely read git's credential settings".to_owned()),
            vec![script::helpers(&who)],
        ),
        (
            "the block init wrote",
            {
                let sys = mac();
                script::keychain_block(&sys, &who, "alice");
                sys
            },
            Some(format!("{HELPER} in {GLOBAL}")),
            vec![script::helpers(&who)],
        ),
        (
            "CLT's own helper alone",
            {
                let sys = mac();
                script::unconfigured(&sys, &who);
                sys
            },
            Some(format!("credential.helper in {}", script::CLT_CONFIG)),
            vec![script::helpers(&who)],
        ),
        (
            "a keychain helper in a file a conditional include names",
            {
                let sys = mac();
                let mut text = vault.clone();
                text.push_str(&script::printed(&[(
                    GLOBAL,
                    "includeif.gitdir:~/work/.path",
                    "~/work.inc",
                )]));
                sys.on(&script::helpers(&who), Ran::ok(&text));
                sys.on(
                    &script::included(&who, work),
                    Ran::ok(&script::printed(&[(
                        work,
                        "credential.helper",
                        "osxkeychain",
                    )])),
                );
                sys
            },
            Some(format!(
                "credential.helper in {work} (a file a conditional include names)"
            )),
            vec![script::helpers(&who), script::included(&who, work)],
        ),
        (
            "a keychain helper for a user",
            {
                let sys = mac();
                let mut text = vault.clone();
                text.push_str(&script::printed(&[(
                    XDG,
                    "credential.https://bot@github.com.helper",
                    "osxkeychain",
                )]));
                sys.on(&script::helpers(&who), Ran::ok(&text));
                sys
            },
            Some(format!(
                "credential.https://<user>@github.com.helper in {XDG}"
            )),
            vec![script::helpers(&who)],
        ),
        (
            "control: the vault's helper alone",
            {
                let sys = mac();
                script::alike(&sys, &who);
                sys
            },
            None,
            vec![script::helpers(&who)],
        ),
        (
            "control: a path-scoped account",
            {
                let sys = mac();
                let mut text = vault.clone();
                text.push_str(&script::printed(&[(
                    XDG,
                    "credential.https://github.com/alice.username",
                    "bot",
                )]));
                sys.on(&script::helpers(&who), Ran::ok(&text));
                sys
            },
            None,
            vec![script::helpers(&who)],
        ),
    ] {
        let git = Git::for_user(&sys, &who);
        assert_eq!(git.keychain_in_effect(&sys, &who), found, "{label}");
        assert_eq!(sys.calls(), reads, "{label}: no helper ran");
    }
}

/// aterm's route answers when its own helper hands back exactly the token, and says why
/// otherwise, naming no token; with no launcher there is no route and nothing runs. The
/// read-back is the one fill, through the route alone.
#[test]
fn lookups_ask_atermss_helper_alone_and_hand_back_only_the_token() {
    let who = alice();
    let token = Secret::new(TOKEN.as_bytes()).unwrap();
    let sys = Fake::default();
    let git = Git::for_user(&sys, &who);
    assert_eq!(Helper::vault(&sys, &who), None, "no launcher, no helper");
    let Lookups::Not(why) = git.lookups(&sys, &who, None, &token).unwrap() else {
        panic!("no route is not ready");
    };
    assert!(
        why.contains("/Users//alice/.local/bin/aterm") && why.contains("repair its installation"),
        "{why}"
    );
    assert!(sys.calls().is_empty(), "nothing ran");
    for (answer, ready) in [
        (Some(TOKEN), true),
        (Some("fixture_another_token"), false),
        (None, false),
    ] {
        let sys = Fake::default();
        script::launcher(&sys, &who);
        match answer {
            Some(held) => script::lookups_hold(&sys, &who, GITHUB, "alice", held),
            None => script::lookups_hold_nothing(&sys, &who),
        }
        let helper = Helper::vault(&sys, &who).expect("the launcher is there");
        let read = Git::for_user(&sys, &who)
            .lookups(&sys, &who, Some(&helper), &token)
            .unwrap();
        assert_eq!(read.ready(), ready, "{answer:?}: {read:?}");
        assert_eq!(sys.calls(), [script::fill(&who)]);
        if let Lookups::Not(why) = read {
            assert!(!why.contains("fixture"), "{why}");
        }
    }
    let sys = Fake::default();
    script::launcher(&sys, &who);
    sys.on(&script::fill(&who), Ran::ok("password=one\npassword=two\n"));
    let helper = Helper::vault(&sys, &who).unwrap();
    assert!(
        Git::for_user(&sys, &who)
            .lookups(&sys, &who, Some(&helper), &token)
            .is_err(),
        "two passwords are no answer"
    );
    let declared = PathBuf::from("/custom store/aterm");
    let record = aterm_types::installed_command::path(Path::new(&who.home));
    let bytes = aterm_types::installed_command::Selection::new(declared.clone())
        .unwrap()
        .encode()
        .unwrap();
    sys.file(record.to_str().unwrap(), &bytes).stat_of(
        record.to_str().unwrap(),
        crate::port::Stat {
            uid: who.uid,
            gid: who.gid,
            mode: 0o600,
            dir: false,
            symlink: false,
            len: bytes.len() as u64,
        },
    );
    sys.executable(declared.to_str().unwrap(), b"declared command");
    let helper = Helper::vault(&sys, &who).unwrap();
    script::hands_back(&sys, &who, &helper, TOKEN);
    assert!(
        git.lookups(&sys, &who, Some(&helper), &token)
            .unwrap()
            .ready()
    );
    assert!(sys.calls().last().unwrap().contains("/custom store/aterm"));
}

/// A network git carries aterm's route last on the command line: the helper list
/// and the account reset, then aterm's helper by the launcher's path, the options a token
/// file names shell-quoted; with no helper, the resets alone. A helper that would ask in a
/// window is told not to, and the read-back names no account.
#[test]
fn the_route_resets_every_helper_then_names_atermss_own() {
    let who = alice();
    assert_eq!(
        script::fill(&who),
        "alice$ GIT_TERMINAL_PROMPT=0 GIT_ASKPASS= GCM_INTERACTIVE=never \
         /Library/Developer/CommandLineTools/usr/bin/git \
         -c credential.helper= -c credential.https://github.com.username= \
         -c 'credential.https://github.com.helper=!/Users//alice/.local/bin/aterm secrets git-credential' \
         credential fill"
    );
    let sys = Fake::default();
    script::launcher(&sys, &who);
    let helper = Helper::token_file(
        &sys,
        &who,
        "alice",
        Path::new("/Users//alice/my secrets/github.env"),
        Some("GH_TOKEN"),
    )
    .unwrap();
    assert_eq!(
        helper.value(),
        "!/Users//alice/.local/bin/aterm secrets git-credential --dev-user alice \
         --token-file '/Users//alice/my secrets/github.env' --token-key GH_TOKEN"
    );
    let bare = pinned(Cmd::new("/usr/bin/git"), None)
        .arg("fetch")
        .display();
    assert_eq!(
        bare,
        "/usr/bin/git -c credential.helper= \
         -c credential.https://github.com.username= fetch"
    );
    let mut dir = who.clone();
    dir.home = "/Users//alice/a dir".into();
    sys.executable("/Users//alice/a dir/.local/bin/aterm", b"launcher");
    assert_eq!(
        Helper::vault(&sys, &dir).unwrap().value(),
        "!'/Users//alice/a dir/.local/bin/aterm' secrets git-credential"
    );
    let custom = PathBuf::from("/custom store/aterm");
    let custom_bin = "/custom bin/aterm";
    sys.link(custom_bin, custom.to_str().unwrap());
    sys.executable(custom.to_str().unwrap(), b"installed command");
    let record = aterm_types::installed_command::path(Path::new(&who.home));
    let bytes = aterm_types::installed_command::Selection::new(custom)
        .unwrap()
        .encode()
        .unwrap();
    sys.file(record.to_str().unwrap(), &bytes).stat_of(
        record.to_str().unwrap(),
        crate::port::Stat {
            uid: who.uid,
            gid: who.gid,
            mode: 0o600,
            dir: false,
            symlink: false,
            len: bytes.len() as u64,
        },
    );
    assert_eq!(
        Helper::vault(&sys, &who).unwrap().value(),
        "!'/custom store/aterm' secrets git-credential",
        "the stable declaration wins over an existing legacy launcher and custom BIN link"
    );
}

#[test]
fn helper_requires_a_nonempty_executable_launcher() {
    let who = alice();
    let launcher = aterm_types::installed_command::legacy(Path::new(&who.home))[0].clone();
    let launcher = launcher.to_str().unwrap();
    for executable in [false, true] {
        for bytes in [b"".as_slice(), b"launcher".as_slice()] {
            let sys = Fake::default();
            if executable {
                sys.executable(launcher, bytes);
            } else {
                sys.file(launcher, bytes);
            }
            let admitted = executable && !bytes.is_empty();
            assert_eq!(Helper::vault(&sys, &who).is_some(), admitted);
            assert_eq!(
                Helper::token_file(&sys, &who, "alice", Path::new("/token"), None).is_some(),
                admitted
            );
        }
    }
    let sys = Fake::default();
    sys.link(launcher, launcher);
    assert!(Helper::vault(&sys, &who).is_none());
    assert!(sys.calls().is_empty(), "admission starts no command");
}

/// The launcher is a link into the installed app, so the file behind it decides. Negative
/// controls: a link to nothing and a link to a folder are no launcher; an executable file
/// and a link to the app's executable binary are.
#[test]
fn a_launcher_link_to_nothing_is_no_launcher() {
    let who = alice();
    let app = "/Applications/aterm.app/Contents/MacOS/aterm";
    let [launcher, store] = aterm_types::installed_command::legacy(Path::new(&who.home));
    let launcher = launcher.to_str().unwrap();
    let sys = Fake::default();
    sys.link(launcher, app);
    assert_eq!(Helper::vault(&sys, &who), None, "the app moved away");
    sys.executable(app, b"aterm");
    assert!(Helper::vault(&sys, &who).is_some(), "the app is there");

    let sys = Fake::default();
    sys.link(launcher, "/Applications");
    sys.stat_of(
        "/Applications",
        crate::port::Stat {
            uid: 0,
            gid: 0,
            mode: 0o755,
            dir: true,
            symlink: false,
            len: 0,
        },
    );
    assert_eq!(Helper::vault(&sys, &who), None, "a folder");

    let sys = Fake::default();
    script::launcher(&sys, &who);
    assert!(Helper::vault(&sys, &who).is_some(), "a plain file");

    let sys = Fake::root();
    sys.executable(store.to_str().unwrap(), b"legacy store command");
    assert!(
        Helper::vault(&sys, &who)
            .unwrap()
            .value()
            .contains(store.to_str().unwrap()),
        "an absent declaration can use the legacy store when BIN is absent"
    );
    script::launcher(&sys, &who);
    let record = aterm_types::installed_command::path(Path::new(&who.home));
    let declared = PathBuf::from("/custom store/aterm");
    let bytes = aterm_types::installed_command::Selection::new(declared.clone())
        .unwrap()
        .encode()
        .unwrap();
    let policy = crate::port::Stat {
        uid: who.uid,
        gid: who.gid,
        mode: 0o600,
        dir: false,
        symlink: false,
        len: bytes.len() as u64,
    };
    sys.file(record.to_str().unwrap(), &bytes)
        .stat_of(record.to_str().unwrap(), policy);
    assert_eq!(
        Helper::vault(&sys, &who),
        None,
        "declared missing target does not fall back"
    );
    sys.executable(declared.to_str().unwrap(), b"repaired command");
    assert!(
        Helper::vault(&sys, &who)
            .unwrap()
            .value()
            .contains("/custom store/aterm")
    );
    sys.stat_of(
        declared.to_str().unwrap(),
        crate::port::Stat {
            dir: true,
            ..policy
        },
    );
    assert_eq!(
        Helper::vault(&sys, &who),
        None,
        "a declared directory does not fall back"
    );
    sys.stat_of(
        declared.to_str().unwrap(),
        crate::port::Stat {
            dir: false,
            mode: 0o755,
            ..policy
        },
    );
    sys.stat_of(
        record.to_str().unwrap(),
        crate::port::Stat { uid: 0, ..policy },
    );
    assert_eq!(
        Helper::vault(&sys, &who),
        None,
        "root cannot borrow a foreign-owned Alice declaration"
    );
    sys.stat_of(record.to_str().unwrap(), policy);
    sys.file(
        "/var/root/.config/aterm/installed-command.json",
        b"unrelated root record",
    );
    assert!(
        Helper::vault(&sys, &who).is_some(),
        "root acting for Alice uses Alice's owned record"
    );
    sys.file(record.to_str().unwrap(), b"malformed");
    assert_eq!(
        Helper::vault(&sys, &who),
        None,
        "malformed never falls back"
    );
    assert!(
        sys.calls().is_empty(),
        "no arbitrary current executable or PATH program is probed"
    );
}

/// git tries askpass before it looks at `GIT_TERMINAL_PROMPT`, so a person's `core.askPass`
/// would answer a lookup the route has no helper for. The run's base keeps it off. Control:
/// the same run without the empty `GIT_ASKPASS` takes `/bin/echo`'s answer. `/bin/echo` echoes
/// git's prompt back and reads nothing, and no helper runs in either.
#[test]
fn a_persons_askpass_never_answers_a_pinned_lookup() {
    crate::items::github::need_native_git();
    let live = Live::new();
    let mut scratch = Scratch::new(&live, "git-askpass").unwrap();
    let home = scratch.path().to_owned();
    let mut who = user("fixture", live.euid());
    who.home = home.to_string_lossy().into_owned();
    live.write_file(
        &home.join(".gitconfig"),
        b"[core]\n\taskPass = /bin/echo\n",
        0o600,
    )
    .unwrap();
    let ask = |git: Cmd| {
        let git = git.env("GIT_CONFIG_NOSYSTEM", "1");
        live.run_complete_stdout_input(&pinned(git, None).args(["credential", "fill"]), REQUEST)
            .unwrap()
            .success()
    };
    assert!(!ask(crate::checkout::git(&live, &who, None)));
    let open = Cmd::new(crate::items::github::git_program_for_user(&live, &who))
        .as_user(&who)
        .env("GIT_TERMINAL_PROMPT", "0");
    assert!(ask(open), "control: askpass answers where it is left on");
    scratch.close().unwrap();
}

/// `gh auth setup-git` writes its section for anyone, so that shape gives way only where
/// aterm's helper has a token here; left in place, it is still named. A literal block never
/// asks: the ruling is about it. Beside a gh half with no token, its own section still goes.
#[test]
fn a_gh_section_gives_way_only_where_atermss_helper_has_a_token() {
    const GH: &str = "!/opt/homebrew/bin/gh auth git-credential";
    let home = "/Users//alice";
    let setup_git = [
        (GLOBAL, HELPER, Some("")),
        (GLOBAL, HELPER, Some(GH)),
        (GLOBAL, GIST_HELPER, Some("")),
        (GLOBAL, GIST_HELPER, Some(GH)),
    ];
    let every = at(&setup_git);
    assert_eq!(keychain_block(home, &every, true, &|| false), None);
    assert!(keychain_helper_in_effect(&every).is_some());
    let block = keychain_block(home, &every, true, &|| true).expect("the token is here");
    assert_eq!(only(&block).write, [HELPER, GIST_HELPER]);

    let asked = std::cell::Cell::new(false);
    let unasked = || {
        asked.set(true);
        false
    };
    assert!(keychain_block(home, &at(&BLOCK), true, &unasked).is_some());
    assert!(!asked.get(), "a literal block reads no token");

    let mut beside = BLOCK.to_vec();
    beside.extend(&setup_git[2..]);
    let block = keychain_block(home, &at(&beside), true, &|| false).expect("osxkeychain");
    assert_eq!(only(&block).unset, [HELPER, USERNAME, USE_PATH]);
    assert_eq!(only(&block).write, [HELPER], "the gh half stays");

    // The same word in the other file `gh auth setup-git` writes, and each file on its own:
    // the block in one file does not carry a gh section in the other with it.
    let xdg: Vec<_> = setup_git
        .iter()
        .map(|(_, key, value)| (XDG, *key, *value))
        .collect();
    assert_eq!(keychain_block(home, &at(&xdg), true, &|| false), None);
    let block = keychain_block(home, &at(&xdg), true, &|| true).expect("the token is here");
    assert_eq!(only(&block).file, Path::new(XDG));
    let mut apart = xdg;
    apart.extend(BLOCK);
    let block = keychain_block(home, &at(&apart), true, &|| false).expect("osxkeychain");
    assert_eq!(only(&block).file, Path::new(GLOBAL), "the gh file stays");
}

/// A shell stand-in for a keychain helper, by its name alone: `sh` runs the file, which
/// writes each operation it is asked to `ran` in `home` and answers nothing. No keychain is
/// read; the name makes [`keychain_helper`] count it.
fn keychain_stand_in(live: &Live, home: &Path) -> String {
    let dir = home.join("stand-in");
    std::fs::create_dir_all(&dir).unwrap();
    let script = dir.join("git-credential-osxkeychain");
    let ran = home.join("ran");
    live.write_file(
        &script,
        format!("echo \"$1\" >> '{}'\n", ran.display()).as_bytes(),
        0o600,
    )
    .unwrap();
    format!("!sh {}", script.display())
}

/// Whether the stand-in [`keychain_stand_in`] wrote ran since the last call, which clears
/// its record.
fn stand_in_ran(home: &Path) -> bool {
    let ran = home.join("ran");
    let held = std::fs::read_to_string(&ran).is_ok_and(|text| !text.is_empty());
    let _ = std::fs::remove_file(&ran);
    held
}

/// Run `revert`, a block's journaled commands, with this host's native git and no system
/// file.
fn put_back(live: &Live, who: &User, revert: &str) {
    let commands = revert.replace(
        "git config",
        &format!("{} config", crate::items::github::need_native_git()),
    );
    let ran = live
        .run_complete_stdout(
            &Cmd::new("/bin/sh")
                .as_user(who)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .args(["-c", &commands]),
        )
        .unwrap();
    assert!(ran.success(), "{ran:?}");
}

/// A real native git in a scratch HOME (no system file read): aterm's route keeps every helper
/// a file names from running, a keychain stand-in among them, and asks its own alone, which
/// answers. The keychain block an earlier `aterm init` wrote on this host to `~/.gitconfig`
/// ([`script::written_helper`]: `osxkeychain`'s on a Mac, `store`'s elsewhere), beside the
/// person's own sections, is found from the settings alone and replaced: its three settings
/// go, github.com and gist.github.com get the reset and aterm's helper, every other section
/// stays, and the person's own git then asks aterm's helper and not the generic keychain
/// stand-in an XDG file names (in CLT's system file's place, which is read before it). The
/// revert puts the block back, from a replacement cut short too. The token git hands back
/// is compared here, never handed to git: no program, argument, variable or standard input
/// the route spawned holds it. Negative controls: the stand-in runs for the person's own git
/// before the replacement; a helper of someone else's under the block's keys keeps the file
/// as it is (and on a Mac, `osxkeychain` still in effect is reported; `store` is no
/// keychain); and a run that does hand git the token is caught. The block's literal helper
/// is only ever read here, never run: no unrouted lookup runs while it is in the file.
#[test]
fn the_keychain_block_gives_way_to_atermss_route_and_no_keychain_helper_runs() {
    crate::items::github::need_native_git();
    let live = Live::new();
    let mut scratch = Scratch::new(&live, "git-keychain-block").unwrap();
    let home = scratch.path().to_owned();
    let mut who = user("fixture", live.euid());
    who.home = home.to_string_lossy().into_owned();
    std::fs::create_dir_all(home.join(".config/git")).unwrap();
    let stand_in = keychain_stand_in(&live, &home);
    live.write_file(
        &home.join(".config/git/config"),
        format!("[credential]\n\thelper = \"{stand_in}\"\n").as_bytes(),
        0o600,
    )
    .unwrap();
    // aterm's stand-in reads its answer from a file: a value in its `-c` would be a value in
    // the run's arguments, which is what `exposed` looks for.
    let answer = home.join("answer");
    live.write_file(
        &answer,
        b"username=alice\npassword=dummy_vault_value\n",
        0o600,
    )
    .unwrap();
    let ours = Helper::stand_in(&format!(
        "!f() {{ test \"$1\" = get || return 0; cat '{}'; }}; f",
        answer.display()
    ));
    let git = Git::isolated();
    let vault = Secret::new(b"dummy_vault_value").unwrap();
    let gitconfig = home.join(".gitconfig");
    let fill = |input: &[u8]| {
        let ran = live
            .run_complete_stdout_input(
                &git.cmd(Some(&who))
                    .arg("-C")
                    .arg(&home)
                    .args(["credential", "fill"]),
                input,
            )
            .unwrap();
        ran.success() && ran.out().contains("password=dummy_vault_value")
    };

    // The Command Line Tools' shape: a generic helper alone, no reset in any file to lean
    // on. The pin clears it, for github.com and for any other host, where no helper answers
    // and the lookup ends at the disabled prompt.
    let spawns = Spawns::new(&live);
    assert_eq!(
        git.lookups(&spawns, &who, Some(&ours), &vault).unwrap(),
        Lookups::Ready
    );
    assert!(!stand_in_ran(&home), "the pin left the generic helper");
    let elsewhere = b"protocol=https\nhost=gitlab.example\n\n";
    for route in [Some(&ours), None] {
        let ran = live
            .run_complete_stdout_input(
                &pinned(git.cmd(Some(&who)).arg("-C").arg(&home), route)
                    .args(["credential", "fill"]),
                elsewhere,
            )
            .unwrap();
        assert!(!ran.success(), "{route:?}");
        assert!(!stand_in_ran(&home), "{route:?}: the generic helper ran");
    }
    assert!(!fill(elsewhere));
    assert!(stand_in_ran(&home), "control: the person's own git runs it");

    // The route, before anything is replaced: the stand-in named for github.com after a
    // reset, never run.
    live.write_file(
        &gitconfig,
        format!("[credential \"https://github.com\"]\n\thelper =\n\thelper = \"{stand_in}\"\n")
            .as_bytes(),
        0o600,
    )
    .unwrap();
    assert!(git.keychain_in_effect(&live, &who).is_some());
    let spawns = Spawns::new(&live);
    assert_eq!(
        git.lookups(&spawns, &who, Some(&ours), &vault).unwrap(),
        Lookups::Ready
    );
    assert!(!stand_in_ran(&home), "the route ran a keychain stand-in");
    let record = spawns.record();
    assert_eq!(record.len(), 1, "one fill: {record:?}");
    assert_eq!(exposed(&record, &live, "dummy_vault_value"), None);
    let handed = vec![(
        git.cmd(Some(&who))
            .env("GH_TOKEN", "dummy_vault_value")
            .args(["--version"]),
        None,
    )];
    assert_eq!(
        exposed(&handed, &live, "dummy_vault_value"),
        Some("variable GH_TOKEN".into())
    );
    assert!(!fill(b"protocol=https\nhost=github.com\n\n"));
    assert!(stand_in_ran(&home), "control: the person's own git runs it");

    // The block init wrote on this host, beside the person's own sections.
    let written = format!("helper = {}\n", script::written_helper(&live));
    let block = format!(
        "[credential \"https://github.com\"]\n\thelper = \n\t{written}\tusername = alice\n\tuseHttpPath = false\n"
    );
    let theirs = "[user]\n\tname = Alice\n[url \"https://alabsystems@github.com/alabsystems/\"]\n\tpushInsteadOf = https://github.com/alabsystems/\n";
    live.write_file(&gitconfig, format!("{block}{theirs}").as_bytes(), 0o600)
        .unwrap();
    let found = git
        .keychain_block(&live, &who, &|| true)
        .unwrap()
        .expect("the block init wrote is found");
    assert_eq!(
        keys(&found),
        (vec![HELPER, USERNAME, USE_PATH], vec![HELPER, GIST_HELPER])
    );
    let revert = found.revert().to_owned();
    git.replace_keychain_block(&live, &who, &found, &ours, &|| Ok(()))
        .unwrap();
    let after = std::fs::read_to_string(&gitconfig).unwrap();
    assert!(
        !after.contains(&written) && !after.contains("useHttpPath") && !after.contains("username"),
        "{after}"
    );
    for kept in [
        "name = Alice",
        "pushInsteadOf",
        "[credential \"https://gist.github.com\"]",
    ] {
        assert!(after.contains(kept), "{kept}: {after}");
    }
    assert_eq!(git.keychain_block(&live, &who, &|| true).unwrap(), None);
    assert_eq!(git.keychain_in_effect(&live, &who), None);
    for (label, input) in [
        ("host-wide", &b"protocol=https\nhost=github.com\n\n"[..]),
        ("a clone's", b"url=https://github.com/owner/repo.git\n\n"),
    ] {
        assert!(
            fill(input),
            "{label}: aterm's helper answers the person's own git"
        );
        assert!(
            !stand_in_ran(&home),
            "{label}: the generic stand-in never ran"
        );
    }

    // The revert, run by the same git, puts the block back; so it does from a replacement
    // cut short after its first removal.
    put_back(&live, &who, &revert);
    assert_eq!(
        git.keychain_block(&live, &who, &|| true).unwrap().as_ref(),
        Some(&found)
    );
    let cut = live
        .run_complete_stdout(
            &git.cmd(Some(&who))
                .args(["config", "--file"])
                .arg(&gitconfig)
                .args(["--unset-all", HELPER]),
        )
        .unwrap();
    assert!(cut.success(), "{cut:?}");
    put_back(&live, &who, &revert);
    assert_eq!(
        git.keychain_block(&live, &who, &|| true).unwrap().as_ref(),
        Some(&found)
    );

    // Negative control: someone else's helper beside the block's keeps the file as it is.
    let foreign = format!("{block}\thelper = !other-stand-in\n{theirs}");
    live.write_file(&gitconfig, foreign.as_bytes(), 0o600)
        .unwrap();
    assert_eq!(git.keychain_block(&live, &who, &|| true).unwrap(), None);
    assert_eq!(
        git.keychain_in_effect(&live, &who).is_some(),
        live.macos(),
        "on a Mac the keychain helper is still in effect, and reported"
    );
    assert_eq!(std::fs::read_to_string(&gitconfig).unwrap(), foreign);
    scratch.close().unwrap();
}

/// `git config --global` writes `~/.config/git/config` where that file is there and
/// `~/.gitconfig` is not. A real native git in such a scratch HOME (no system file read) writes
/// the block that way, as an earlier `aterm init` did on this host
/// ([`script::written_helper`]). It is found in that file and replaced
/// there, no `~/.gitconfig` appears, the person's own git then asks aterm's helper, and the
/// revert puts it back. A block in each file goes in one pass and comes back with one revert.
/// Negative control: the vault's section in that file is no block, and stays byte for byte
/// beside a block replaced in `~/.gitconfig`. The block's literal helper is only ever read
/// here, never run: the one unrouted lookup runs with no file naming it.
#[test]
fn a_block_in_the_xdg_file_alone_gives_way_in_that_file() {
    crate::items::github::need_native_git();
    let live = Live::new();
    let mut scratch = Scratch::new(&live, "git-keychain-xdg").unwrap();
    let home = scratch.path().to_owned();
    let mut who = user("fixture", live.euid());
    who.home = home.to_string_lossy().into_owned();
    std::fs::create_dir_all(home.join(".config/git")).unwrap();
    let xdg = home.join(".config/git/config");
    let gitconfig = home.join(".gitconfig");
    let text = |file: &Path| std::fs::read_to_string(file).unwrap();
    live.write_file(&xdg, b"", 0o600).unwrap();
    let answer = home.join("answer");
    live.write_file(
        &answer,
        b"username=alice\npassword=dummy_vault_value\n",
        0o600,
    )
    .unwrap();
    let ours = Helper::stand_in(&format!(
        "!f() {{ test \"$1\" = get || return 0; cat '{}'; }}; f",
        answer.display()
    ));
    let git = Git::isolated();
    let block = || git.keychain_block(&live, &who, &|| true).unwrap();
    let helper = script::written_helper(&live);
    let written = format!("helper = {helper}\n");

    for (key, value) in [
        (HELPER, ""),
        (HELPER, helper),
        (USERNAME, "alice"),
        (USE_PATH, "false"),
    ] {
        let ran = live
            .run_complete_stdout(
                &git.cmd(Some(&who))
                    .args(["config", "--global", "--add", key, value]),
            )
            .unwrap();
        assert!(ran.success(), "{ran:?}");
    }
    assert!(text(&xdg).contains(&written), "{}", text(&xdg));
    assert!(!gitconfig.exists(), "--global wrote the other file");

    let found = block().expect("the block in the XDG file");
    assert_eq!(only(&found).file, xdg);
    assert_eq!(
        keys(&found),
        (vec![HELPER, USERNAME, USE_PATH], vec![HELPER, GIST_HELPER])
    );
    git.replace_keychain_block(&live, &who, &found, &ours, &|| Ok(()))
        .unwrap();
    let after = text(&xdg);
    assert!(
        !after.contains(&written)
            && !after.contains("usehttppath")
            && !after.contains("username")
            && after.contains("[credential \"https://gist.github.com\"]"),
        "{after}"
    );
    assert!(!gitconfig.exists(), "the replacement made another file");
    assert_eq!(block(), None);
    assert_eq!(git.keychain_in_effect(&live, &who), None);
    let ran = live
        .run_complete_stdout_input(
            &git.cmd(Some(&who))
                .arg("-C")
                .arg(&home)
                .args(["credential", "fill"]),
            REQUEST,
        )
        .unwrap();
    assert!(
        ran.success() && ran.out().contains("password=dummy_vault_value"),
        "aterm's helper answers the person's own git"
    );
    put_back(&live, &who, found.revert());
    assert_eq!(block().as_ref(), Some(&found));
    assert!(!gitconfig.exists(), "the revert made another file");

    live.write_file(&gitconfig, text(&xdg).as_bytes(), 0o600)
        .unwrap();
    let both = block().expect("a block in each file");
    assert_eq!(both.replacements.len(), 2, "{both:?}");
    git.replace_keychain_block(&live, &who, &both, &ours, &|| Ok(()))
        .unwrap();
    assert!(!text(&xdg).contains(&written), "{}", text(&xdg));
    assert!(!text(&gitconfig).contains(&written), "{}", text(&gitconfig));
    assert_eq!(block(), None);
    put_back(&live, &who, both.revert());
    assert_eq!(block().as_ref(), Some(&both));

    let keychain = text(&gitconfig);
    let vault = "[credential \"https://github.com\"]\n\thelper =\n\thelper = \"!\\\"/stand-in/agh\\\" auth git-credential\"\n[credential \"https://gist.github.com\"]\n\thelper =\n\thelper = \"!\\\"/stand-in/agh\\\" auth git-credential\"\n";
    live.write_file(&xdg, vault.as_bytes(), 0o600).unwrap();
    std::fs::remove_file(&gitconfig).unwrap();
    assert_eq!(block(), None, "the vault's section is someone else's");
    live.write_file(&gitconfig, keychain.as_bytes(), 0o600)
        .unwrap();
    let beside = block().expect("the block in ~/.gitconfig");
    assert_eq!(only(&beside).file, gitconfig);
    git.replace_keychain_block(&live, &who, &beside, &ours, &|| Ok(()))
        .unwrap();
    assert_eq!(text(&xdg), vault);
    assert!(!text(&gitconfig).contains(&written), "{}", text(&gitconfig));
    scratch.close().unwrap();
}

/// A real native git in a scratch HOME whose `~/.gitconfig` holds what `gh auth setup-git`
/// wrote for an `aterm init` before 2026-10-22 (GitHub CLI's helper for github.com and
/// gist.github.com, each after the empty reset), beside the person's `[user]`: the settings
/// read finds both sections as the block and asks no helper (the spawn record holds that
/// read alone, and the gh the block names does not exist, so a run of it could only fail);
/// the replacement leaves aterm's helper for both hosts and the person's `[user]`, and
/// aterm's helper then answers. Negative control: GitHub CLI's helper inside a command of
/// someone else's is in effect and not the block, and the file stays as it is. Only on a
/// Mac: no `aterm init` ran gh elsewhere, so off a Mac those sections are a person's own,
/// as the settings tests above hold.
#[cfg(target_os = "macos")]
#[test]
fn the_block_gh_setup_git_wrote_gives_way_to_atermss_route() {
    let live = Live::new();
    if !live.macos() {
        aterm_uds::host::could_not_run(
            "not a Mac: only an `aterm init` on a Mac ran `gh auth setup-git`, whose block \
             this replaces; elsewhere its sections are a person's own",
        );
    }
    crate::items::github::need_native_git();
    let mut scratch = Scratch::new(&live, "git-gh-block").unwrap();
    let home = scratch.path().to_owned();
    let mut who = user("fixture", live.euid());
    who.home = home.to_string_lossy().into_owned();
    let gh = home.join("absent/bin/gh");
    let setup_git = format!(
        "[credential \"https://github.com\"]\n\thelper = \n\thelper = !{gh} auth git-credential\n\
         [credential \"https://gist.github.com\"]\n\thelper = \n\thelper = !\\\"{gh}\\\" auth git-credential\n",
        gh = gh.display()
    );
    let theirs = "[user]\n\tname = Alice\n";
    let gitconfig = home.join(".gitconfig");
    live.write_file(&gitconfig, format!("{setup_git}{theirs}").as_bytes(), 0o600)
        .unwrap();
    let git = Git::isolated();
    let ours = Helper::stand_in(
        "!f() { test \"$1\" = get || return 0; echo username=alice; echo password=dummy_vault_value; }; f",
    );
    let vault = Secret::new(b"dummy_vault_value").unwrap();

    let spawns = Spawns::new(&live);
    let found = git
        .keychain_block(&spawns, &who, &|| true)
        .unwrap()
        .expect("the block gh setup-git wrote is found");
    assert_eq!(
        only(&found).unset,
        [HELPER, USERNAME, USE_PATH, GIST_HELPER]
    );
    assert_eq!(only(&found).write, [HELPER, GIST_HELPER]);
    assert_eq!(
        spawns.record().len(),
        1,
        "the settings read alone: no helper ran"
    );
    git.replace_keychain_block(&live, &who, &found, &ours, &|| Ok(()))
        .unwrap();
    let after = std::fs::read_to_string(&gitconfig).unwrap();
    assert!(!after.contains("auth git-credential"), "{after}");
    assert_eq!(after.matches("dummy_vault_value").count(), 2, "{after}");
    assert!(after.contains("name = Alice"), "{after}");
    assert_eq!(git.keychain_in_effect(&live, &who), None);
    assert_eq!(
        git.lookups(&live, &who, Some(&ours), &vault).unwrap(),
        Lookups::Ready
    );

    // Negative control: GitHub CLI's helper in someone else's command.
    let wrapped = format!(
        "[credential \"https://github.com\"]\n\thelper = \n\thelper = \"!f() {{ {gh} auth git-credential \\\"$@\\\"; }}; f\"\n{theirs}",
        gh = gh.display()
    );
    live.write_file(&gitconfig, wrapped.as_bytes(), 0o600)
        .unwrap();
    let spawns = Spawns::new(&live);
    assert_eq!(git.keychain_block(&spawns, &who, &|| true).unwrap(), None);
    assert!(git.keychain_in_effect(&spawns, &who).is_some());
    assert_eq!(
        spawns.record().len(),
        2,
        "two settings reads: no helper ran"
    );
    assert_eq!(std::fs::read_to_string(&gitconfig).unwrap(), wrapped);
    scratch.close().unwrap();
}

/// [`scope`] and [`Git::keychain_in_effect`] agree with a real native git (no system file read,
/// a scratch HOME) about which lookups a `credential.<url>.helper` reaches: a keychain
/// stand-in under each hard spelling of the table above runs for both a host-wide lookup and
/// a clone's that names an account and a path (`Every`), for one of them (`Part`), or for
/// neither (`Never`), and the stand-in is a keychain helper in effect exactly when it runs
/// for one. A stand-in in a file a conditional include names (on the repository's
/// directory, or on its remote) runs for a lookup in that repository and is in effect too.
/// Negative controls: the spellings that name another host, scheme or port, and an include
/// that holds no credential setting.
#[test]
fn the_url_matcher_agrees_with_what_git_runs_for_a_github_lookup() {
    crate::items::github::need_native_git();
    let live = Live::new();
    let mut scratch = Scratch::new(&live, "git-url-matcher").unwrap();
    let home = scratch.path().to_owned();
    let mut who = user("fixture", live.euid());
    who.home = home.to_string_lossy().into_owned();
    let stand_in = keychain_stand_in(&live, &home);
    let git = Git::isolated();
    let gitconfig = home.join(".gitconfig");
    let repo = home.join("repo");
    let ask = |dir: &Path, input: &[u8]| {
        live.run_complete_stdout_input(
            &git.cmd(Some(&who))
                .arg("-C")
                .arg(dir)
                .args(["credential", "fill"]),
            input,
        )
        .unwrap();
        stand_in_ran(&home)
    };
    for url in [
        "https://github.com",
        "https://GitHub.com",
        "https://*.com",
        "https://github.com:443",
        "https://github.com.",
        "https://%67ithub.com",
        "github.com",
        "",
        "https://bot@github.com",
        "https://github.com/owner",
        "https://github.com./owner",
        "github.com/owner/repo.git",
        "bot@github.com",
        "https://github.com//",
        "http://github.com",
        "https://github.com:8443",
        "https://gist.github.com",
        "https://github.com../owner",
        "github.com.",
        "GITHUB.COM",
        "github.com:443",
        "https://%67ithub.com.",
        "HTTPS://%67ithub.com",
    ] {
        live.write_file(
            &gitconfig,
            format!("[credential \"{url}\"]\n\thelper = \"{stand_in}\"\n").as_bytes(),
            0o600,
        )
        .unwrap();
        let host_wide = ask(&home, b"protocol=https\nhost=github.com\n\n");
        let clone = ask(&home, b"url=https://bot@github.com/owner/repo.git\n\n");
        let git_runs = match (host_wide, clone) {
            (true, true) => Scope::Every,
            (false, false) => Scope::Never,
            _ => Scope::Part,
        };
        assert_eq!(scope(Some(url)), git_runs, "{url:?}");
        // `scope` speaks of github.com; gist.github.com's section is named for its own host.
        assert_eq!(
            git.keychain_in_effect(&live, &who).is_some(),
            git_runs != Scope::Never || url == "https://gist.github.com",
            "{url:?}"
        );
    }

    let made = live
        .run_complete_stdout(&git.cmd(Some(&who)).arg("init").arg("-q").arg(&repo))
        .unwrap();
    assert!(made.success(), "{made:?}");
    let remote = live
        .run_complete_stdout(&git.cmd(Some(&who)).arg("-C").arg(&repo).args([
            "remote",
            "add",
            "origin",
            "https://github.com/owner/repo.git",
        ]))
        .unwrap();
    assert!(remote.success(), "{remote:?}");
    let include = home.join("github.inc");
    let helper = format!("[credential]\n\thelper = \"{stand_in}\"\n");
    for (label, condition, included, runs) in [
        (
            "on the repository's directory",
            "[includeIf \"gitdir:~/repo/\"]\n\tpath = ~/github.inc\n",
            helper.as_str(),
            true,
        ),
        (
            "on its remote",
            "[includeIf \"hasconfig:remote.*.url:https://github.com/**\"]\n\tpath = github.inc\n",
            helper.as_str(),
            true,
        ),
        (
            "control: an include holding no credential setting",
            "[includeIf \"gitdir:~/repo/\"]\n\tpath = ~/github.inc\n",
            "[user]\n\temail = bot@example.invalid\n",
            false,
        ),
    ] {
        live.write_file(&gitconfig, condition.as_bytes(), 0o600)
            .unwrap();
        live.write_file(&include, included.as_bytes(), 0o600)
            .unwrap();
        assert!(
            !ask(&home, b"protocol=https\nhost=github.com\n\n"),
            "{label}: outside the repository"
        );
        assert_eq!(
            ask(&repo, b"url=https://github.com/owner/repo.git\n\n"),
            runs,
            "{label}"
        );
        let found = git.keychain_in_effect(&live, &who);
        assert_eq!(found.is_some(), runs, "{label}: {found:?}");
    }
    scratch.close().unwrap();
}
