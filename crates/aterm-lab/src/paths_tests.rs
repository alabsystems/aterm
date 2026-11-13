// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The checkout is the nearest one holding the lab's crate above the package directory the
/// run names, else above the folder it started in. NEGATIVE CONTROL: a start inside a
/// tree without the lab's crate (a second checkout's sibling, a scratch folder) names no
/// checkout and gives way to the next.
#[test]
fn the_checkout_is_the_nearest_one_holding_the_lab_above_where_the_run_starts() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let (first, second) = (scratch.path().join("first"), scratch.path().join("second"));
    for checkout in [&first, &second] {
        std::fs::create_dir_all(checkout.join(CRATE_DIR)).unwrap();
        std::fs::write(checkout.join(CRATE_DIR).join("Cargo.toml"), "").unwrap();
    }
    let elsewhere = scratch.path().join("elsewhere/deep");
    std::fs::create_dir_all(&elsewhere).unwrap();
    assert_eq!(
        checkout_from([second.join(CRATE_DIR)]),
        Some(second.clone())
    );
    assert_eq!(
        checkout_from([first.join("crates/other/src")]),
        Some(first.clone())
    );
    assert_eq!(
        checkout_from([second.join(CRATE_DIR), first.clone()]),
        Some(second)
    );
    assert_eq!(
        checkout_from([elsewhere.clone(), first.clone()]),
        Some(first)
    );
    assert_eq!(checkout_from([elsewhere]), None);
    assert_eq!(
        checkout().join(CRATE_DIR),
        aterm_tempfile::manifest_dir(),
        "this run's checkout holds the package it runs"
    );
}

#[test]
fn forgets_agent_and_aterm_variables_and_keeps_the_rest() {
    for k in [
        "ATERM_SESSION_ID",
        "CLAUDE_CONFIG_DIR",
        "ANTHROPIC_API_KEY",
        "ATPKG_BIN",
        "AI_AGENT",
        "CLAUDECODE",
        "CODEX_HOME",
        "META_API_KEY",
        "MUSE_MODEL",
        "TBH_AUTH_BASE_URL",
    ] {
        assert!(forgotten(k), "{k}");
    }
    for k in [
        "http_proxy",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "all_proxy",
        "No_Proxy",
        "CURL_HOME",
    ] {
        assert!(forgotten(k), "{k}");
    }
    for k in [
        "LAB_ATERM",
        aterm_toolchain::waits::VAR,
        "HOME",
        "PATH",
        "AI_AGENTX",
        "TERM_PROGRAM",
        "LAB_MUSE",
        "METAL_DEVICE_WRAPPER_TYPE",
        "_PROXY",
        "MY1_PROXY",
        "PROXYX",
    ] {
        assert!(!forgotten(k), "{k}");
    }
}

#[test]
fn tags_are_short_plain_words() {
    assert!(valid_tag("lab"));
    assert!(valid_tag("nudge-12345"));
    assert!(!valid_tag(""));
    assert!(!valid_tag("a/b"));
    assert!(!valid_tag(&"x".repeat(25)));
}

#[test]
fn physical_resolves_the_existing_head_and_keeps_the_rest() {
    let tmp = std::env::temp_dir();
    let real = std::fs::canonicalize(&tmp).unwrap();
    assert_eq!(
        physical(&tmp.join("no-such-dir-lab/x")),
        real.join("no-such-dir-lab/x")
    );
    assert_eq!(home_path(Some(&tmp)), real.to_str().unwrap());
    for path in [None, Some(Path::new("/")), Some(Path::new("relative"))] {
        assert!(home_path(path).is_empty());
    }
    assert!(home_path(Some(&tmp.join("no-such-dir-lab/x"))).is_empty());
    use std::os::unix::ffi::OsStrExt as _;
    let owned = aterm_tempfile::tempdir().unwrap();
    let bytes = owned.path().join(std::ffi::OsStr::from_bytes(b"home-\xff"));
    assert!(
        home_path(Some(&bytes)).is_empty(),
        "a fence cannot use a lossy name"
    );
}

#[test]
fn the_guarded_homes_are_the_account_s_and_home_s_each_once() {
    assert_eq!(guarded("/Users//o", "/tmp/s"), ["/Users//o", "/tmp/s"]);
    assert_eq!(guarded("/Users//o", "/Users//o"), ["/Users//o"]);
    assert_eq!(guarded("", "/tmp/s"), ["/tmp/s"]);
    assert_eq!(guarded("/Users//o", ""), ["/Users//o"]);
    assert!(guarded("", "").is_empty());
}

#[test]
fn a_fake_account_home_is_guarded_beside_a_scratch_home() {
    let top = aterm_tempfile::Builder::new()
        .prefix("lab-homes-")
        .suffix(".noindex")
        .tempdir()
        .unwrap();
    let real = std::fs::canonicalize(top.path()).unwrap();
    let (account, scratch) = (top.path().join("account"), top.path().join("scratch"));
    for d in [&account, &scratch] {
        std::fs::create_dir_all(d).unwrap();
    }
    let (a, s) = (
        real.join("account").display().to_string(),
        real.join("scratch").display().to_string(),
    );
    let p = Paths::resolve_with(Some(account.clone()), Some(scratch.clone().into()));
    assert_eq!(
        (p.real_home.as_str(), p.caller_home.as_str()),
        (a.as_str(), s.as_str())
    );
    assert_eq!(p.guarded_homes(), [a.clone(), s.clone()]);
    assert_eq!(p.need_guarded_homes(), Ok(vec![a.clone(), s.clone()]));
    assert_eq!(p.store_home(), a);
    assert_eq!(p.need_home(), Ok(a.as_str()));
    assert!(p.scratch_home());
    let own = Paths::resolve_with(Some(account.clone()), Some(account.clone().into()));
    assert_eq!(own.guarded_homes(), std::slice::from_ref(&a));
    assert!(
        !own.scratch_home(),
        "HOME the account's own is no scratch home"
    );
    let env_only = Paths::resolve_with(None, Some(scratch.clone().into()));
    assert_eq!(env_only.guarded_homes(), std::slice::from_ref(&s));
    assert_eq!(env_only.store_home(), s);
    assert!(
        env_only.need_home().is_err(),
        "the jail refuses an unknown account home"
    );
    let gone = Paths::resolve_with(Some(real.join("no-such-home")), Some(scratch.into()));
    assert_eq!(gone.guarded_homes(), std::slice::from_ref(&s));
    let none = Paths::resolve_with(None, None);
    assert!(none.need_guarded_homes().is_err() && none.need_store_home().is_err());
}

#[test]
fn the_lab_s_own_paths_take_the_account_home_from_the_account_database() {
    let want = home_path(aterm_uds::account::home().as_deref());
    assert!(
        !want.is_empty(),
        "the account database names this account's home"
    );
    let p = paths();
    assert_eq!(p.real_home, want);
    assert!(p.guarded_homes().contains(&want));
    assert_eq!(p.store_home(), want);
    assert_eq!(
        p.caller_home,
        home_path(std::env::var_os("HOME").as_deref().map(Path::new))
    );
    assert_eq!(
        p.scratch_home(),
        !p.caller_home.is_empty() && p.caller_home != want
    );
    let env_only = Paths::resolve_with(None, std::env::var_os("HOME"));
    assert!(env_only.real_home.is_empty() && env_only.need_home().is_err());
}

#[test]
fn the_default_state_sits_outside_every_home_short_and_per_checkout() {
    let a = default_state(501, Path::new("/Users//o/Developer/aterm"));
    let b = default_state(501, Path::new("/Users//o/Developer/wt/aterm"));
    for d in [&a, &b] {
        assert!(
            d.starts_with("/private/tmp/.ubd/at.lab-501"),
            "{}",
            d.display()
        );
        assert!(!d.starts_with("/Users//o"));
        let key = d.file_name().unwrap().to_string_lossy();
        assert!(
            key.starts_with("aterm-") && key.len() == "aterm-".len() + 8,
            "{key}"
        );
    }
    assert_ne!(a, b, "two checkouts named aterm keep two state dirs");
    assert_eq!(a, default_state(501, Path::new("/Users//o/Developer/aterm")));
    assert!(checkout_key(Path::new("/x/my repo!")).starts_with("my_repo_-"));
    assert!(checkout_key(Path::new("/x/.hidden")).starts_with("lab.hidden-"));
    assert!(checkout_key(Path::new("/")).starts_with("lab-"));
    let long = default_state(
        u32::MAX,
        Path::new("/x/a-checkout-whose-name-runs-on-and-on"),
    );
    assert!(
        long.to_string_lossy()
            .starts_with("/private/tmp/.ubd/at.lab-4294967295/a-checkout-whose-")
    );
    assert_eq!(long.as_os_str().len(), 61, "{}", long.display());
    let p = paths();
    if std::env::var_os("LAB_STATE").is_none_or(|s| s.is_empty()) {
        assert!(p.state_default);
        assert_eq!(
            p.state,
            default_state(crate::sys::getuid(), &physical(&p.repo))
        );
        assert!(p.need_state_outside(&p.guarded_homes()).is_ok());
    }
}

struct Tree(crate::TestScratch);

fn tree(name: &str) -> Tree {
    Tree(crate::TestScratch::new(name))
}

fn mode(p: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::symlink_metadata(p).unwrap().permissions().mode() & 0o777
}

#[test]
fn the_default_state_is_made_private_and_a_squatted_root_is_refused() {
    use std::os::unix::fs::PermissionsExt as _;
    let t = tree("private");
    let uid = crate::sys::getuid();
    let mut p = Paths::resolve_with(None, None);
    p.state_relative = None;
    p.state_default = true;
    p.state = t.0.join("root/key");
    p.make_default_state().unwrap();
    assert_eq!((mode(&t.0.join("root")), mode(&p.state)), (0o700, 0o700));
    std::fs::set_permissions(t.0.join("root"), std::fs::Permissions::from_mode(0o755)).unwrap();
    p.make_default_state().unwrap();
    assert_eq!(mode(&t.0.join("root")), 0o700, "a looser root is narrowed");
    std::fs::create_dir_all(t.0.join("elsewhere")).unwrap();
    std::os::unix::fs::symlink(t.0.join("elsewhere"), t.0.join("linked")).unwrap();
    p.state = t.0.join("linked/key");
    let err = p.make_default_state().unwrap_err();
    assert!(
        err.contains("not a directory") && err.contains("set LAB_STATE"),
        "{err}"
    );
    assert!(
        !t.0.join("elsewhere/key").exists(),
        "nothing made through the link"
    );
    std::fs::write(t.0.join("file"), "x").unwrap();
    p.state = t.0.join("file/key");
    assert!(
        p.make_default_state()
            .unwrap_err()
            .contains("not a directory")
    );
    let err = private_dir(&t.0.join("root"), uid.wrapping_add(1)).unwrap_err();
    assert!(err.contains("belongs to uid"), "{err}");
    p.state_default = false;
    p.state = t.0.join("linked/key");
    assert!(
        p.make_default_state().is_ok(),
        "a named LAB_STATE is not touched"
    );
}

#[test]
fn the_state_a_verb_makes_things_in_is_made_private_first() {
    let t = tree("need");
    let mut p = Paths::resolve_with(None, None);
    p.state_relative = None;
    p.state_default = true;
    std::fs::create_dir_all(t.0.join("elsewhere")).unwrap();
    std::os::unix::fs::symlink(t.0.join("elsewhere"), t.0.join("linked")).unwrap();
    p.state = t.0.join("linked/key");
    let err = p.need_state().unwrap_err();
    assert!(err.contains("not a directory"), "{err}");
    let results = t.0.join("linked/key/results");
    assert!(
        !t.0.join("elsewhere/key").exists() && !results.exists(),
        "nothing made through the link"
    );
    p.state = t.0.join("root/key");
    assert!(p.need_state_outside(&[]).is_ok());
    assert!(!t.0.join("root").exists(), "a check makes nothing");
    assert_eq!(p.need_state().unwrap(), t.0.join("root/key"));
    assert_eq!((mode(&t.0.join("root")), mode(&p.state)), (0o700, 0o700));
    p.state_default = false;
    p.state = t.0.join("named/key");
    assert_eq!(p.need_state().unwrap(), t.0.join("named/key"));
    assert!(
        !t.0.join("named").exists(),
        "a named LAB_STATE is not made here"
    );
    p.state_relative = Some("rel".into());
    assert!(p.need_state().unwrap_err().contains("is relative"));
}

#[test]
fn a_state_dir_inside_a_home_is_refused_and_one_outside_goes_through() {
    let t = tree("inside");
    let (account, scratch) = (t.0.join("account"), t.0.join("scratch-home"));
    for h in [&account, &scratch] {
        std::fs::create_dir_all(h.join("lab")).unwrap();
    }
    std::os::unix::fs::symlink(account.join("lab"), t.0.join("leads-in")).unwrap();
    let mut p = Paths::resolve_with(Some(account.clone()), Some(scratch.clone().into()));
    p.state_relative = None;
    p.state_default = false;
    let homes = p.guarded_homes();
    for (state, home) in [
        (account.join("lab/state"), &account),
        (scratch.join("lab"), &scratch),
        (account.clone(), &account),
        (t.0.join("leads-in/state"), &account),
    ] {
        p.state = state;
        let err = p.need_state_outside(&homes).unwrap_err();
        assert!(
            err.contains(&format!("sits inside the home {}", home.display()))
                && err.contains("/private/tmp/.ubd/at.lab-"),
            "{err}"
        );
    }
    for state in [t.0.join("outside"), t.0.join("account2/state")] {
        p.state = state.clone();
        assert_eq!(p.need_state_outside(&homes), Ok(state.as_path()));
    }
    p.state = PathBuf::from("/");
    assert!(p.need_state_outside(&homes).is_err());
    p.state = t.0.to_path_buf();
    assert!(
        p.need_state_outside(&homes)
            .unwrap_err()
            .contains("encloses the home")
    );
    p.state = state_root(crate::sys::getuid());
    assert!(
        p.need_state_outside(&homes)
            .unwrap_err()
            .contains("encloses")
    );
}
