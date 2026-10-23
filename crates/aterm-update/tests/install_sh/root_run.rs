// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A root run over an account's home (macOS's sudo keeps HOME): the installer runs itself
//! again as the account that typed sudo, and over anyone else's home, or piped, says how to
//! run it instead. Nothing here runs as root or reaches the real sudo: euid 0 is an
//! argument, and the one call that would exec sudo is replaced.

use std::os::unix::fs::MetadataExt as _;

use crate::support::{Lib, Scratch, install_sh, s, system_path};

/// (euid, $SUDO_UID, HOME's owner) -> (the uid to run as, refused): only root over the home
/// of the account that typed sudo runs as it; root over any other account's home is refused;
/// anything else (not root, root's own home, an owner that cannot be read) is left alone.
/// $SUDO_UID rides `-u "#<uid>"`, so only digits name an account.
#[test]
fn a_root_run_acts_on_a_home_only_as_its_account() {
    let scratch = Scratch::new();
    let vars = ["HOME_ACCOUNT", "HOME_REFUSED"];
    // euid, $SUDO_UID, HOME's owner -> HOME_ACCOUNT, HOME_REFUSED
    let rows = [
        ["501", "", "501", "", "0"],
        ["501", "502", "501", "", "0"],
        ["0", "501", "501", "501", "0"],
        ["0", "501", "502", "", "1"],
        ["0", "", "501", "", "1"],
        ["0", "0", "501", "", "1"],
        ["0", "", "0", "", "0"],
        ["0", "501", "0", "", "0"],
        ["0", "501", "", "", "0"],
        ["0", "5x", "501", "", "1"],
        ["0", "501;x", "501", "", "1"],
        ["0", "5x", "5x", "", "1"],
    ];
    let mut lib = Lib::new();
    let calls: Vec<usize> = rows
        .iter()
        .map(|[euid, sudo, owner, ..]| {
            lib.call(format!("home_account '{euid}' '{sudo}' '{owner}'"), &vars)
        })
        .collect();
    let recs = lib.run(&scratch);
    for (row, i) in rows.iter().zip(calls) {
        let rec = &recs[i];
        assert_eq!(rec.rc, 0, "{row:?}: {}", rec.err);
        assert_eq!([rec.var(vars[0]), rec.var(vars[1])], row[3..], "{row:?}");
    }
}

/// The owner the decision reads is HOME's, as the kernel reports it.
#[test]
fn the_home_owner_is_homes_uid() {
    let scratch = Scratch::new();
    let mut lib = Lib::new();
    let call = lib.call("home_owner_uid", &[]);
    let recs = lib.run(&scratch);
    let owner = std::fs::metadata(scratch.join("home")).expect("HOME").uid();
    assert_eq!(recs[call].rc, 0, "{}", recs[call].err);
    assert_eq!(recs[call].out.trim(), owner.to_string());
}

/// The run as the account is this file again, by its absolute path, with the same
/// arguments verbatim, over the same HOME, with the root run's PATH. sudo keeps the
/// settings it reads (ATERM_*, XDG_*, CARGO_HOME, RUSTUP_HOME, curl's proxy and CA
/// variables) by name, so none of their values is in argv, and nothing else. A piped run
/// (no file) or another account's home execs nothing and says to run it without sudo.
#[test]
fn a_root_run_execs_itself_as_the_account_or_says_why_not() {
    let scratch = Scratch::new();
    let log = scratch.join("tmp/as-account");
    let mut lib = Lib::new();
    lib.step(
        "exec_as_account() { printf '%s\\n' \"$@\" > \"$TMPDIR/as-account\"; }\n\
         account_test() { return 0; }\n\
         export ATERM_REPO_SLUG=example/aterm CARGO_HOME=/c RUSTUP_HOME=/r \\
         XDG_CONFIG_HOME=/x OTHER_SETTING=kept-out CARGO_TARGET_DIR=/t \\
         https_proxy=http://u:pw@proxy:3128 CURL_CA_BUNDLE=/ca.pem",
    );
    let ran = lib.call(
        "( HOME_ACCOUNT=501; HOME_REFUSED=0; run_as_home_account --version 1.2.3 'a b' )",
        &[],
    );
    let argv = lib.call(
        "cat \"$TMPDIR/as-account\" && rm \"$TMPDIR/as-account\"",
        &[],
    );
    let piped = lib.call(
        "( self_on_disk() { return 1; }; HOME_ACCOUNT=501; HOME_REFUSED=0; run_as_home_account )",
        &[],
    );
    let refused = lib.call(
        "( HOME_ACCOUNT=; HOME_REFUSED=1; run_as_home_account --dry-run )",
        &[],
    );
    let recs = lib.run(&scratch);
    assert_eq!(recs[ran].rc, 0, "{}", recs[ran].err);
    assert!(
        recs[ran]
            .out
            .contains("install.sh: running as uid 501, not root: HOME ("),
        "{}",
        recs[ran].out
    );
    let script = install_sh();
    let home = scratch.join("home");
    let argv: Vec<&str> = recs[argv].out.lines().collect();
    let mut kept: Vec<&str> = argv.get(1).map_or(vec![], |list| list.split(',').collect());
    kept.sort_unstable();
    assert_eq!(
        kept,
        [
            "ATERM_REPO_SLUG",
            "CARGO_HOME",
            "CURL_CA_BUNDLE",
            "RUSTUP_HOME",
            "XDG_CONFIG_HOME",
            "https_proxy"
        ],
        "{argv:?}"
    );
    let want = [
        "/usr/bin/env",
        &format!("HOME={}", s(&home)),
        &format!("PATH={}", system_path()),
        "/bin/bash",
        s(&script),
        "--version",
        "1.2.3",
        "a b",
    ];
    assert_eq!(argv.first(), Some(&"501"), "{argv:?}");
    assert_eq!(argv.get(2..), Some(&want[..]), "{argv:?}");
    let owner = std::fs::metadata(&home).expect("HOME").uid();
    for (rec, uid) in [
        (&recs[piped], "501".to_string()),
        (&recs[refused], owner.to_string()),
    ] {
        assert_eq!(rec.rc, 1, "{}", rec.out);
        assert!(
            rec.err.contains(&format!(
                "install.sh: not as root: HOME ({}) is uid {uid}'s, and root would leave files \
                 there that its own aterm cannot change — run it as that account, without sudo",
                s(&home)
            )),
            "{}",
            rec.err
        );
    }
    assert!(!log.exists(), "a refusal execs nothing");
}

/// Before anything runs as the account, the account itself is asked whether it can read
/// the file and write each folder an ATERM_*_DIR names, at the folder `mkdir -p` would
/// build on (the nearest that exists; the working directory for a relative path with
/// none). A no is a refusal that execs nothing and names the way that works.
#[test]
fn a_root_run_asks_the_account_before_it_runs_as_it() {
    let scratch = Scratch::new();
    let tmp = scratch.join("tmp");
    let script = install_sh();
    let mut lib = Lib::new();
    lib.step(
        "exec_as_account() { : > \"$TMPDIR/as-account\"; }\n\
         account_test() { printf '%s %s\\n' \"$2\" \"$3\" >> \"$TMPDIR/asked\"; [[ \"$2 $3\" != \"${DENY:-}\" ]]; }\n\
         mkdir -p \"$TMPDIR/sys\"",
    );
    let as_root = "HOME_ACCOUNT=501; HOME_REFUSED=0; \
                   export ATERM_BIN_DIR=\"$TMPDIR/sys/new/bin\" ATERM_STORE_DIR=rel/store";
    let asked = lib.call(
        format!(
            "( {as_root}; run_as_home_account ) > /dev/null && cat \"$TMPDIR/asked\" && \
             rm \"$TMPDIR/asked\" \"$TMPDIR/as-account\""
        ),
        &[],
    );
    let unreadable = lib.call(
        format!(
            "( {as_root}; DENY='-r {}'; run_as_home_account )",
            s(&script)
        ),
        &[],
    );
    let unwritable = lib.call(
        format!("( {as_root}; DENY=\"-w $TMPDIR/sys\"; run_as_home_account )"),
        &[],
    );
    let recs = lib.run(&scratch);
    assert_eq!(recs[asked].rc, 0, "{}", recs[asked].err);
    let script_line = format!("-r {}", s(&script));
    let sys_line = format!("-w {}", s(&tmp.join("sys")));
    assert_eq!(
        recs[asked].out.lines().collect::<Vec<_>>(),
        [script_line.as_str(), sys_line.as_str(), "-w ."]
    );
    let why = format!(
        "install.sh: not as root: HOME ({}) is uid 501's, and root would leave files there \
         that its own aterm cannot change, and uid 501 cannot",
        s(&scratch.join("home"))
    );
    for (rec, tail) in [
        (
            &recs[unreadable],
            format!(
                "read {} — run a copy it can read as that account, without sudo",
                s(&script)
            ),
        ),
        (
            &recs[unwritable],
            format!(
                "write ATERM_BIN_DIR ({}) — run it with sudo -H to install there as root, or \
                 name a folder that account can write",
                s(&tmp.join("sys/new/bin"))
            ),
        ),
    ] {
        assert_eq!(rec.rc, 1, "{}", rec.out);
        assert!(rec.err.contains(&format!("{why} {tail}")), "{}", rec.err);
    }
    assert!(!tmp.join("as-account").exists(), "a refusal execs nothing");
}
