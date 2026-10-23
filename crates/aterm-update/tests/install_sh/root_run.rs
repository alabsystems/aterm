// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A root run over an account's home (macOS's sudo keeps HOME): the installer runs itself
//! again as the account that typed sudo, and over anyone else's home, or piped, says how to
//! run it instead. Nothing here runs as root or reaches the real sudo: euid 0 is an
//! argument, and the one call that would exec sudo is replaced.

use std::os::unix::fs::MetadataExt as _;

use crate::support::{Lib, Scratch, install_sh, s, system_path};

/// (euid, $SUDO_UID, HOME's owner, whether HOME is in that account's own home) -> (the uid
/// to run as, refused): only root over the home of the account that typed sudo runs as it;
/// root over any other account's home, or a folder in it, is refused; anything else (not
/// root, root's own home, an owner that cannot be read, one no account here has, or a HOME
/// outside its owner's own home: a CI container's) is left alone.
/// $SUDO_UID rides `-u "#<uid>"`, so only digits name an account.
#[test]
fn a_root_run_acts_on_a_home_only_as_its_account() {
    let scratch = Scratch::new();
    let vars = ["HOME_ACCOUNT", "HOME_REFUSED"];
    // euid, $SUDO_UID, HOME's owner, that owner is an account -> HOME_ACCOUNT, HOME_REFUSED
    let rows = [
        ["501", "", "501", "1", "", "0"],
        ["501", "502", "501", "1", "", "0"],
        ["0", "501", "501", "1", "501", "0"],
        ["0", "501", "502", "1", "", "1"],
        ["0", "", "501", "1", "", "1"],
        ["0", "0", "501", "1", "", "1"],
        ["0", "", "0", "1", "", "0"],
        ["0", "501", "0", "1", "", "0"],
        ["0", "501", "", "1", "", "0"],
        ["0", "5x", "501", "1", "", "1"],
        ["0", "501;x", "501", "1", "", "1"],
        ["0", "5x", "5x", "1", "", "1"],
        ["0", "", "1001", "0", "", "0"],
        ["0", "501", "1001", "0", "", "0"],
        ["0", "501", "501", "0", "501", "0"],
    ];
    let mut lib = Lib::new();
    let calls: Vec<usize> = rows
        .iter()
        .map(|[euid, sudo, owner, named, ..]| {
            lib.call(
                format!("home_account '{euid}' '{sudo}' '{owner}' '{named}'"),
                &vars,
            )
        })
        .collect();
    let recs = lib.run(&scratch);
    for (row, i) in rows.iter().zip(calls) {
        let rec = &recs[i];
        assert_eq!(rec.rc, 0, "{row:?}: {}", rec.err);
        assert_eq!([rec.var(vars[0]), rec.var(vars[1])], row[4..], "{row:?}");
    }
}

/// The owner the decision reads is HOME's, as the kernel reports it. HOME is an account's
/// only when it is that account's own home (getpwnam's) or a folder in it: the test user's
/// real home is, and (a stand-in answering the lookup) a folder outside the owner's home is
/// not, as a CI container's HOME owned by a uid some account of the image shares is not. A
/// name with no home, or a home of `/`, counts as the account's, so the run refuses rather
/// than writes as root.
#[test]
fn the_home_owner_is_homes_uid() {
    let scratch = Scratch::new();
    let mut lib = Lib::new();
    let call = lib.call("home_owner_uid", &[]);
    let real_home = lib.call(
        "( HOME=\"$(eval echo \"~$(id -un)\")\"; home_owner_named )",
        &[],
    );
    let lookup = lib.call(
        "[ \"$(account_home \"$(id -un)\")\" = \"$(eval echo \"~$(id -un)\")\" ] && \
         ! account_home 'a b' && ! account_home 'x;y' && ! account_home -x && \
         ! account_home no-such-account-zz9",
        &[],
    );
    let named = |home: &str| format!("( account_home() {{ {home}; }}; home_owner_named )");
    let its_own = lib.call(named("echo \"$HOME\""), &[]);
    let inside = lib.call(named("echo \"${HOME%/*}\""), &[]);
    let root = lib.call(named("echo /"), &[]);
    let unknown = lib.call(named("return 1"), &[]);
    let elsewhere = lib.call(named("echo \"$TMPDIR\""), &[]);
    let prefix = lib.call(named("mkdir -p \"${HOME%e}\" && echo \"${HOME%e}\""), &[]);
    let gone = lib.call(named("echo \"$TMPDIR/gone\""), &[]);
    let recs = lib.run(&scratch);
    let owner = std::fs::metadata(scratch.join("home")).expect("HOME").uid();
    assert_eq!(recs[call].rc, 0, "{}", recs[call].err);
    assert_eq!(recs[call].out.trim(), owner.to_string());
    for (i, rc) in [
        (real_home, 0),
        (lookup, 0),
        (its_own, 0),
        (inside, 0),
        (root, 0),
        (unknown, 0),
        (elsewhere, 1),
        (prefix, 1),
        (gone, 1),
    ] {
        assert_eq!(recs[i].rc, rc, "call {i}: {}", recs[i].err);
    }
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

/// What an earlier install under sudo left root's is named once per tree, at the outermost
/// folder on each of aterm's paths below HOME that is not the account's, so one chown gives
/// the tree back; a folder outside HOME is never named, a root run names none, and a skip
/// into a named tree points at the NOTE instead of at the setting.
#[test]
fn folders_an_earlier_root_run_left_are_named_at_their_top() {
    let scratch = Scratch::new();
    let home = scratch.join("home");
    let mut lib = Lib::new();
    lib.step(
        "mine() { case \"$1\" in \"$HOME/.local\" | \"$HOME/.local/\"* | \
         \"$HOME/Library/Application Support/aterm\" | \"$TMPDIR/outside\"*) return 1 ;; esac; }\n\
         mkdir -p \"$HOME/.local/bin\" \"$HOME/.local/lib/aterm/bin\" \"$HOME/.config/aterm\" \
         \"$HOME/Library/Application Support/aterm/pkg\" \"$TMPDIR/outside/bin\"\n\
         BIN_DIR=\"$HOME/.local/bin\" STORE_DIR=\"$HOME/.local/lib/aterm/bin\"",
    );
    let named = lib.call(
        "foreign_home_folders && printf '%s\\n' \"${FOREIGN_FOLDERS[@]}\"",
        &[],
    );
    let note = lib.call("foreign_home_note 2>&1", &[]);
    let user = lib.call("id -un", &[]);
    // A uid with no name here is named by its number.
    let nameless = lib.call(
        "( id() { [ \"$1\" != -un ] && command id \"$@\"; }; foreign_home_note ) 2>&1",
        &[],
    );
    let remedies = lib.call(
        "unwritable_remedy \"$STORE_DIR\" ATERM_STORE_DIR && unwritable_remedy \"$TMPDIR/outside/bin\" ATERM_BIN_DIR",
        &[],
    );
    let all = lib.call(
        "( BIN_DIR=\"$TMPDIR/outside/bin\"; mine() { [[ \"$1\" == \"$HOME\" ]]; }; foreign_home_folders && \
         printf '%s\\n' \"${FOREIGN_FOLDERS[@]}\" )",
        &[],
    );
    let none = lib.call(
        "( mine() { return 0; }; foreign_home_folders && foreign_home_note && \
         echo \"${#FOREIGN_FOLDERS[@]}\" ) 2>&1",
        &[],
    );
    let as_root = lib.call(
        "( id() { echo 0; }; mine() { [[ \"$1\" == \"$HOME\" ]]; }; foreign_home_folders && \
         foreign_home_note && \
         echo \"${#FOREIGN_FOLDERS[@]}\" ) 2>&1",
        &[],
    );
    let count = "foreign_home_folders && foreign_home_note && echo \"${#FOREIGN_FOLDERS[@]}\"";
    // sudo -u keeps the invoker's HOME: its folders are its owner's.
    let not_my_home = lib.call(format!("( mine() {{ return 1; }}; {count} ) 2>&1"), &[]);
    // A `..` climbs out of HOME as written; HOME empty or `/` contains every path.
    let dotdot = lib.call(
        "( BIN_DIR=\"$HOME/../tmp/outside/bin\"; mine() { [[ \"$1\" == \"$HOME\" ]]; }; \
         foreign_home_folders && printf '%s\\n' \"${FOREIGN_FOLDERS[@]}\" )",
        &[],
    );
    let bare_homes = lib.call(
        format!(
            "( HOME=/; mine() {{ [[ \"$1\" == / ]]; }}; {count} ) 2>&1 && \
             ( HOME=; mine() {{ [[ -z \"$1\" ]]; }}; {count} ) 2>&1"
        ),
        &[],
    );
    // chown -R of a symlink changes the link: a link ends HOME's own folders.
    let linked = lib.call(
        format!(
            "( mkdir -p \"$TMPDIR/linked/completions\" && ln -s \"$TMPDIR/linked\" \"$HOME/.config/fish\" && \
             mine() {{ case \"$1\" in */fish/*) return 1 ;; esac; }}; {count} && \
             unwritable_remedy \"$HOME/.config/fish/completions\" ATERM_X ) 2>&1"
        ),
        &[],
    );
    let recs = lib.run(&scratch);
    for i in [
        named,
        note,
        user,
        nameless,
        remedies,
        all,
        none,
        as_root,
        not_my_home,
        dotdot,
        bare_homes,
        linked,
    ] {
        assert_eq!(recs[i].rc, 0, "{}", recs[i].err);
    }
    let local = s(&home.join(".local")).to_string();
    let state = s(&home.join("Library/Application Support/aterm")).to_string();
    assert_eq!(
        recs[named].out.lines().collect::<Vec<_>>(),
        [&local, &state]
    );
    let quoted = state.replace(' ', "\\ ");
    assert_eq!(
        recs[note].out.trim_end(),
        format!(
            "install.sh: NOTE: not yours, so aterm cannot change what is in them: {local}, \
             {state} (an install under sudo makes such folders root's) — give them back with: \
             sudo chown -R {} {local} {quoted}",
            recs[user].out.trim()
        )
    );
    assert!(
        recs[nameless].out.contains(&format!(
            "sudo chown -R {} {local} ",
            std::fs::metadata(&home).expect("HOME").uid()
        )),
        "{}",
        recs[nameless].out
    );
    assert_eq!(
        recs[remedies].out.lines().collect::<Vec<_>>(),
        [
            format!("{local} is not yours: the NOTE above gives it back"),
            "set ATERM_BIN_DIR to a writable dir".to_string(),
        ]
    );
    let config = s(&home.join(".config")).to_string();
    let library = s(&home.join("Library")).to_string();
    assert_eq!(
        recs[all].out.lines().collect::<Vec<_>>(),
        [&local, &config, &library]
    );
    assert_eq!(recs[none].out.trim(), "0");
    assert_eq!(recs[not_my_home].out.trim(), "0", "another account's HOME");
    assert_eq!(
        recs[dotdot].out.lines().collect::<Vec<_>>(),
        [&local, &config, &library],
        "a `..` path is never walked"
    );
    assert_eq!(
        recs[bare_homes].out.lines().collect::<Vec<_>>(),
        ["0", "0"],
        "HOME / and empty"
    );
    assert_eq!(
        recs[linked].out.lines().collect::<Vec<_>>(),
        ["0", "set ATERM_X to a writable dir"],
        "a folder through a link"
    );
    assert_eq!(
        recs[as_root].out.trim(),
        "0",
        "root changes anything: nothing to name"
    );
}

/// The real `mine`: a folder this account owns or can write (root's 1777 /tmp) is its own,
/// so the NOTE never names one a run can already write; root's / is not.
#[test]
fn a_folder_the_account_can_write_is_its_own() {
    let scratch = Scratch::new();
    let mut lib = Lib::new();
    let call = lib.call("mine \"$HOME\" && mine /tmp && ! mine /", &[]);
    let recs = lib.run(&scratch);
    assert_eq!(recs[call].rc, 0, "{}", recs[call].err);
}
