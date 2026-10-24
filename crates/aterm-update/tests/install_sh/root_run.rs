// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A root run over an account's home (macOS's sudo keeps HOME).

use std::os::unix::fs::MetadataExt as _;

use crate::support::{Lib, Scratch, group_run, install_sh, q, read, s, system_path};

#[test]
fn a_root_run_acts_on_a_home_only_as_its_account() {
    let scratch = Scratch::new();
    let vars = ["HOME_ACCOUNT", "HOME_REFUSED"];
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
        "account_homes \"$(id -u)\" | grep -xF \"$(eval echo \"~$(id -un)\")\" >/dev/null && \
         [ -z \"$(account_homes 424242424)\" ] && [ \"$(account_homes 0 | wc -l)\" -ge 2 ]",
        &[],
    );
    let records = lib.call(
        "printf '%s\\n' 'name: alice' 'uid: 502' 'dir: /Users//alice' '' 'name: bob' 'uid: 502' \
         'dir: /Users//b o b' '' '' 'name: c' 'uid: 5020' 'dir: /Users//c' | dscache_homes 502 && \
         printf '%s\\n' 'name: nobody' 'uid: -2' 'dir: /var/empty' '' 'name: x' 'uid: 2' 'dir: /x' | \
         dscache_homes 4294967294 && \
         printf '%s\\n' 'ubuntu:x:1000:1000::/home/ubuntu:/bin/sh' 'dev:x:1000:1001::/home/d e v:/bin/sh' \
         'x:x:10000:1::/home/x:/bin/sh' | passwd_homes 1000",
        &[],
    );
    let named = |homes: &str| format!("( account_homes() {{ {homes}; }}; home_owner_named )");
    let its_own = lib.call(named("echo \"$HOME\""), &[]);
    let inside = lib.call(named("echo \"${HOME%/*}\""), &[]);
    let root = lib.call(named("echo /"), &[]);
    let unknown = lib.call(named(":"), &[]);
    let elsewhere = lib.call(named("echo \"$TMPDIR\""), &[]);
    let prefix = lib.call(named("mkdir -p \"${HOME%e}\" && echo \"${HOME%e}\""), &[]);
    let gone = lib.call(named("echo \"$TMPDIR/gone\""), &[]);
    let second = lib.call(named("echo \"$TMPDIR\"; echo \"$HOME\""), &[]);
    let slashes = lib.call(
        "( account_homes() { echo \"$HOME\"; }; HOME=\"/$HOME\"; home_owner_named )",
        &[],
    );
    let alias = lib.call(
        named("ln -s \"$HOME\" \"$TMPDIR/alias\" && echo \"$TMPDIR/alias\""),
        &[],
    );
    let file = lib.call(
        "( HOME=\"$TMPDIR/a-file\"; : >\"$HOME\"; account_homes() { echo \"$TMPDIR\"; }; \
         home_owner_named )",
        &[],
    );
    let nameless = lib.call(
        "( awk() { echo 4242; }; account_homes() { echo \"$HOME\"; }; home_owner_named )",
        &[],
    );
    let recs = lib.run(&scratch);
    let owner = std::fs::metadata(scratch.join("home")).expect("HOME").uid();
    assert_eq!(recs[call].rc, 0, "{}", recs[call].err);
    assert_eq!(recs[call].out.trim(), owner.to_string());
    assert_eq!(
        recs[records].out.lines().collect::<Vec<_>>(),
        [
            "/Users//alice",
            "/Users//b o b",
            "/var/empty",
            "/home/ubuntu",
            "/home/d e v"
        ]
    );
    for (i, rc) in [
        (real_home, 0),
        (lookup, 0),
        (records, 0),
        (its_own, 0),
        (inside, 0),
        (root, 0),
        (unknown, 0),
        (elsewhere, 1),
        (prefix, 1),
        (gone, 1),
        (second, 0),
        (slashes, 0),
        (alias, 0),
        (file, 0),
        (nameless, 1),
    ] {
        assert_eq!(recs[i].rc, rc, "call {i}: {}", recs[i].err);
    }
}

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

#[test]
fn folders_an_earlier_root_run_left_are_named_at_their_top() {
    let scratch = Scratch::new();
    let home = scratch.join("home");
    let tmp = scratch.join("tmp");
    let mut lib = Lib::new();
    lib.step(
        "case \"$(uname -s)\" in Darwin) ST=\"$HOME/Library/Application Support/aterm\" ;; \
         *) export XDG_STATE_HOME=\"$HOME/.state\"; ST=\"$XDG_STATE_HOME/aterm\" ;; esac\n\
         mine() { case \"$1\" in \"$HOME/.local\" | \"$HOME/.local/\"* | \"$ST\" | \
         \"$TMPDIR/outside\"*) return 1 ;; esac; }\n\
         mkdir -p \"$HOME/.local/bin\" \"$HOME/.local/lib/aterm/bin\" \"$HOME/.config/aterm\" \
         \"$ST/pkg\" \"$TMPDIR/outside/bin\"\n\
         BIN_DIR=\"$HOME/.local/bin\" STORE_DIR=\"$HOME/.local/lib/aterm/bin\"",
    );
    let named = lib.call(
        "foreign_home_folders && printf '%s\\n' \"${FOREIGN_FOLDERS[@]}\"",
        &[],
    );
    let note = lib.call("foreign_home_note 2>&1", &[]);
    let user = lib.call("id -un", &[]);
    let nameless = lib.call(
        "( id() { [ \"$1\" != -un ] && command id \"$@\"; }; foreign_home_note ) 2>&1",
        &[],
    );
    let spaced = lib.call(
        "( id() { if [ \"$1\" = -un ]; then echo 'john smith'; else command id \"$@\"; fi; }; \
         foreign_home_note ) 2>&1",
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
    let list =
        "foreign_home_folders && printf '%s\\n' ${FOREIGN_FOLDERS[@]+\"${FOREIGN_FOLDERS[@]}\"}";
    let not_my_home = lib.call(
        format!(
            "( HOME=/usr; BIN_DIR=/usr/bin; mine() {{ [[ \"$1\" == \"$HOME\" ]]; }}; {count} ) 2>&1"
        ),
        &[],
    );
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
    let linked = lib.call(
        format!(
            "( mkdir -p \"$TMPDIR/linked/completions\" && ln -s \"$TMPDIR/linked\" \"$HOME/.config/fish\" && \
             mine() {{ case \"$1\" in */fish/*) return 1 ;; esac; }}; {count} && \
             unwritable_remedy \"$HOME/.config/fish/completions\" ATERM_X ) 2>&1"
        ),
        &[],
    );
    let fresh = |name: &str, body: &str| {
        format!(
            "( HOME=\"$TMPDIR/{name}\"; mkdir -p \"$HOME\" \"$TMPDIR/{name}-target\"; \
             BIN_DIR=\"$HOME/.local/bin\" STORE_DIR=\"$HOME/.local/lib/aterm/bin\"; {body} ) 2>&1"
        )
    };
    let link_top = lib.call(
        fresh(
            "ltop",
            &format!(
                "ln -s \"$TMPDIR/ltop-target\" \"$HOME/.aterm\"; \
                 mine() {{ case \"$1\" in \"$HOME/.aterm\"*) return 1 ;; esac; }}; {count}"
            ),
        ),
        &[],
    );
    let above_link = lib.call(
        fresh(
            "above",
            &format!(
                "mkdir -p \"$HOME/.top\"; ln -s \"$TMPDIR/above-target\" \"$HOME/.top/lnk\"; \
                 BIN_DIR=\"$HOME/.top/lnk/bin\"; mine() {{ [[ \"$1\" != \"$HOME/.top\" ]]; }}; {list}"
            ),
        ),
        &[],
    );
    let man_page = lib.call(
        fresh(
            "man",
            &format!(
                "mkdir -p \"$HOME/.local/share/man/man1\"; : >\"$HOME/.local/share/man/man1/aterm.1\"; \
                 mine() {{ [[ \"$1\" != *.1 ]]; }}; {list}"
            ),
        ),
        &[],
    );
    let xdg_data = lib.call(
        fresh(
            "xdg",
            &format!(
                "XDG_DATA_HOME=\"$HOME/.xdg/data\"; mkdir -p \"$XDG_DATA_HOME/zsh/site-functions\" \
                 \"$HOME/.local/share/zsh/site-functions\"; \
                 mine() {{ case \"$1\" in */zsh) return 1 ;; esac; }}; {list}"
            ),
        ),
        &[],
    );
    let inside = lib.call(
        fresh(
            "inside",
            &format!(
                "mkdir -p \"$STORE_DIR\" \"$HOME/.aterm\"; mine() {{ return 0; }}; \
                 foreign_inside() {{ case \"$1\" in \"$STORE_DIR\") printf '%s\\n' \"$1/x/y\" \"$1/x\" \"$1/x\" ;; \
                 \"$HOME/.aterm\") echo \"$1/shell.d\" ;; esac; }}; {list}"
            ),
        ),
        &[],
    );
    let inside_linked = lib.call(
        fresh(
            "inlink",
            &format!(
                "ln -s \"$TMPDIR/inlink-target\" \"$HOME/.aterm\"; mine() {{ return 0; }}; \
                 foreign_inside() {{ echo \"$1/planted\"; }}; {count}"
            ),
        ),
        &[],
    );
    let real_inside = lib.call(
        "mkdir -p \"$TMPDIR/tree/a/b\" \"$TMPDIR/tree/c\" \"$TMPDIR/tree/x\"$'\\n'\"/etc\" && \
         : >\"$TMPDIR/tree/f\" && ln -s a \"$TMPDIR/tree/l\" && \
         ( id() { echo 424242424; }; foreign_inside \"$TMPDIR/tree\" ) | sort && \
         echo -- && foreign_inside \"$TMPDIR/tree\"",
        &[],
    );
    let recs = lib.run(&scratch);
    for i in [
        named,
        note,
        user,
        nameless,
        spaced,
        remedies,
        all,
        none,
        as_root,
        not_my_home,
        dotdot,
        bare_homes,
        linked,
        link_top,
        above_link,
        man_page,
        xdg_data,
        inside,
        inside_linked,
        real_inside,
    ] {
        assert_eq!(recs[i].rc, 0, "call {i}: {}", recs[i].err);
    }
    let macos = cfg!(target_os = "macos");
    let local = s(&home.join(".local")).to_string();
    let (state, state_top) = if macos {
        (
            home.join("Library/Application Support/aterm"),
            home.join("Library"),
        )
    } else {
        (home.join(".state/aterm"), home.join(".state"))
    };
    let (state, state_top) = (s(&state).to_string(), s(&state_top).to_string());
    assert_eq!(
        recs[named].out.lines().collect::<Vec<_>>(),
        [&local, &state]
    );
    let quoted = if state.contains(' ') {
        format!("'{state}'")
    } else {
        state.clone()
    };
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
    assert!(
        recs[spaced]
            .out
            .contains(&format!("sudo chown -R 'john smith' {local} ")),
        "{}",
        recs[spaced].out
    );
    assert_eq!(
        recs[remedies].out.lines().collect::<Vec<_>>(),
        [
            format!("{local} is not yours: the NOTE above gives it back"),
            "set ATERM_BIN_DIR to a writable dir".to_string(),
        ]
    );
    let config = s(&home.join(".config")).to_string();
    assert_eq!(
        recs[all].out.lines().collect::<Vec<_>>(),
        [&local, &config, &state_top]
    );
    assert_eq!(recs[none].out.trim(), "0");
    assert_eq!(
        recs[not_my_home].out.trim(),
        "0",
        "a HOME this account does not own"
    );
    assert_eq!(
        recs[dotdot].out.lines().collect::<Vec<_>>(),
        [&local, &config, &state_top],
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
    assert_eq!(recs[link_top].out.trim(), "0", "a link is no top");
    let t = |rel: &str| s(&tmp.join(rel)).to_string();
    assert_eq!(
        recs[above_link].out.lines().collect::<Vec<_>>(),
        [t("above/.top")],
        "a folder above a link"
    );
    assert_eq!(
        recs[man_page].out.lines().collect::<Vec<_>>(),
        [t("man/.local/share/man/man1/aterm.1")],
        "a page root wrote into the account's man1"
    );
    assert_eq!(
        recs[xdg_data].out.lines().collect::<Vec<_>>(),
        [t("xdg/.xdg/data/zsh")],
        "XDG_DATA_HOME's completions, not ~/.local/share's"
    );
    assert_eq!(
        recs[inside].out.lines().collect::<Vec<_>>(),
        [
            t("inside/.local/lib/aterm/bin/x"),
            t("inside/.aterm/shell.d")
        ],
        "inside the trees, the outermost, once"
    );
    assert_eq!(recs[inside_linked].out.trim(), "0", "a tree through a link");
    assert_eq!(
        recs[real_inside].out.lines().collect::<Vec<_>>(),
        [t("tree/a"), t("tree/c"), t("tree/f"), "--".to_string()],
        "the real find"
    );
    assert_eq!(
        recs[as_root].out.trim(),
        "0",
        "root changes anything: nothing to name"
    );
}

#[test]
fn a_folder_the_account_can_write_is_its_own() {
    let scratch = Scratch::new();
    let mut lib = Lib::new();
    let call = lib.call("mine \"$HOME\" && mine /tmp && ! mine /", &[]);
    let recs = lib.run(&scratch);
    assert_eq!(recs[call].rc, 0, "{}", recs[call].err);
}

#[test]
fn the_note_names_the_desktop_entry_and_icons_on_linux_alone() {
    let scratch = Scratch::new();
    let tmp = scratch.join("tmp");
    let mut lib = Lib::new();
    let on = |os: &str| {
        format!(
            "( HOME=\"$TMPDIR/{os}\"; BIN_DIR=\"$HOME/.local/bin\" STORE_DIR=\"$HOME/.local/lib/aterm/bin\"; \
             d=\"$HOME/.local/share\"; mkdir -p \"$d/icons/hicolor/256x256/apps\" \"$d/applications\"; \
             : >\"$d/icons/hicolor/256x256/apps/aterm.png\"; \
             uname() {{ if [ \"$1\" = -s ]; then echo {os}; else command uname \"$@\"; fi; }}; \
             mine() {{ case \"$1\" in */apps/aterm.png | */applications) return 1 ;; esac; }}; \
             foreign_home_folders && printf '%s\\n' ${{FOREIGN_FOLDERS[@]+\"${{FOREIGN_FOLDERS[@]}}\"}} )"
        )
    };
    let linux = lib.call(on("Linux"), &[]);
    let darwin = lib.call(on("Darwin"), &[]);
    let recs = lib.run(&scratch);
    let t = |rel: &str| s(&tmp.join(rel)).to_string();
    assert_eq!(recs[linux].rc, 0, "{}", recs[linux].err);
    assert_eq!(
        recs[linux].out.lines().collect::<Vec<_>>(),
        [
            t("Linux/.local/share/applications"),
            t("Linux/.local/share/icons/hicolor/256x256/apps/aterm.png")
        ]
    );
    assert_eq!(recs[darwin].rc, 0, "{}", recs[darwin].err);
    assert_eq!(recs[darwin].out, "", "macOS has neither");
}

#[test]
fn the_notes_command_is_one_word_per_path_in_every_shell() {
    let scratch = Scratch::new();
    let words = [
        "alice",
        "/a/b.c",
        "john smith",
        "it's",
        "a\\b",
        "",
        "$HOME",
        "/Users//x/Donn\u{e9}es",
        "a\tb",
    ];
    let mut lib = Lib::new();
    lib.env("LC_ALL", "C");
    let calls: Vec<usize> = words
        .iter()
        .map(|w| {
            lib.call(
                format!(
                    "w=$(printf '%s' {}); q=$(shell_word \"$w\"); printf '%s\\n' \"$q\"; \
                     eval \"[ \\\"\\$w\\\" = $q ]\"",
                    q(w)
                ),
                &[],
            )
        })
        .collect();
    let recs = lib.run(&scratch);
    let spelled: Vec<&str> = calls
        .iter()
        .map(|&i| recs[i].out.trim_end_matches('\n'))
        .collect();
    assert_eq!(
        spelled,
        [
            "alice",
            "/a/b.c",
            "'john smith'",
            "it\\'s",
            "a\\\\b",
            "''",
            "'$HOME'",
            "'/Users//x/Donn\u{e9}es'",
            "$'a\\tb'",
        ]
    );
    for (w, &i) in words.iter().zip(&calls) {
        assert_eq!(recs[i].rc, 0, "{w:?} does not read back: {}", recs[i].err);
    }
}

#[test]
fn root_runs_no_program_another_account_can_change() {
    let scratch = Scratch::new();
    let mut lib = Lib::new();
    lib.step(
        "mkdir -p \"$HOME/.local/bin\" && \
         printf '#!/bin/sh\\necho \"$0 $*\" >>\"$TMPDIR/ran\"\\necho aterm 0.1.0\\n' >\"$HOME/.local/bin/aterm\" && \
         chmod 755 \"$HOME/.local/bin/aterm\" && ln -s /bin/ls \"$TMPDIR/ls\" && \
         for f in /usr/bin/*; do if [ -L \"$f\" ] && [ -f \"$f\" ]; then SYSLINK=$f; break; fi; done",
    );
    let as_account = lib.call("may_run \"$HOME/.local/bin/aterm\"", &[]);
    lib.step("id() { if [ \"$1\" = -u ]; then echo 0; else command id \"$@\"; fi; }");
    let may = |p: &str| format!("if may_run {p}; then echo yes; else echo no; fi");
    let system = lib.call(may("/bin/ls"), &[]);
    let system_link = lib.call(may("\"$SYSLINK\""), &[]);
    let theirs = lib.call(may("\"$HOME/.local/bin/aterm\""), &[]);
    let their_link = lib.call(may("\"$TMPDIR/ls\""), &[]);
    let missing = lib.call(may("/nonexistent/aterm"), &[]);
    let folder = lib.call(may("/bin"), &[]);
    let verdict = lib.call(
        "bin_aterm_verdict \"$HOME/.local/bin/aterm\" \"$HOME/.local/lib/aterm/bin\"",
        &[],
    );
    let asked = lib.call(
        "first_line_bounded \"$HOME/.local/bin/aterm\" --version && \
         setup_aterm shell-setup \"$HOME/.local/bin\" \"$HOME/.local/bin\" && [ ! -e \"$TMPDIR/ran\" ]",
        &[],
    );
    let bundles = lib.call(
        "mkdir -p \"$TMPDIR/x.app/Contents\" && bundle_is_roots /bin && ! bundle_is_roots \"$TMPDIR/x.app\"",
        &[],
    );
    // Linux shows every link as lrwxrwxrwx (Ubuntu's /bin -> usr/bin): a root
    // link is root's whatever its mode, a folder every account can write is not.
    let modes = lib.call(
        "( ls_entry() { echo 'lrwxrwxrwx 1 0 0 7 Apr 22 2024 /bin -> usr/bin'; }; root_entry /bin ) && \
         ! ( ls_entry() { echo 'lrwxrwxrwx 1 501 0 7 Apr 22 2024 /bin -> usr/bin'; }; root_entry /bin ) && \
         ! ( ls_entry() { echo 'drwxrwxrwx 2 0 0 64 Apr 22 2024 /x'; }; root_entry /x ) && \
         ( ls_entry() { echo 'drwxrwxrwt 2 0 0 64 Apr 22 2024 /x'; }; root_entry /x )",
        &[],
    );
    let recs = lib.run(&scratch);
    assert_eq!(recs[as_account].rc, 0, "a run as the account runs anything");
    for (i, want) in [
        (system, "yes"),
        (system_link, "yes"),
        (theirs, "no"),
        (their_link, "no"),
        (missing, "no"),
        (folder, "no"),
    ] {
        assert_eq!(recs[i].out.trim(), want, "call {i}: {}", recs[i].err);
    }
    assert_eq!(
        recs[verdict].out.trim(),
        "foreign another account can change it, so root does not run it to ask what it is"
    );
    assert_eq!(recs[asked].rc, 0, "root ran it: {}", recs[asked].err);
    assert_eq!(recs[asked].out, "");
    assert_eq!(recs[bundles].rc, 0, "{}", recs[bundles].err);
    assert_eq!(recs[modes].rc, 0, "{}", recs[modes].err);
}

/// Under root, the folder the app is installed in is refused when it carries
/// an ACL, as a folder on a program's way is (entry_without_acl): through a
/// link, the folder it leads to. A run as the account installs where it can.
#[cfg(target_os = "macos")]
#[test]
fn under_root_an_install_folder_with_an_acl_is_refused() {
    let scratch = Scratch::new();
    let mut lib = Lib::new();
    lib.step(format!(
        "{FOREIGN_FIND}\n{FAKE_ROOT}\nFOREIGN_LIST=\"$TMPDIR/foreign\"; : >\"$FOREIGN_LIST\"\n\
         D=\"$TMPDIR/d\"; mkdir \"$D\" && chmod 755 \"$D\" && ln -s d \"$TMPDIR/dest-link\"\n\
         judged() {{\n\
         \tchmod -N \"$D\" && {{ [ -z \"$1\" ] || chmod +a \"$1\" \"$D\"; }} || return\n\
         \tlocal p\n\
         \tfor p in \"$D\" \"$TMPDIR/dest-link\"; do\n\
         \t\tif root_dest_acl_open \"$p\"; then echo refused; else echo installs; fi\n\
         \tdone\n\
         }}"
    ));
    let rows = [
        ("no ACL", "", "installs"),
        (
            "an account may add folders",
            "user:nobody allow add_subdirectory",
            "refused",
        ),
        ("everyone may read", "group:everyone allow list", "refused"),
        (
            "a deny entry alone",
            "group:everyone deny delete",
            "refused",
        ),
    ];
    let calls: Vec<usize> = rows
        .iter()
        .map(|(_, ace, _)| lib.call(format!("judged '{ace}'"), &[]))
        .collect();
    let as_account = lib.call(
        "( unset -f id; \
         chmod +a 'group:everyone allow add_subdirectory' \"$D\" && ! root_dest_acl_open \"$D\" )",
        &[],
    );
    // A deny-delete entry left on the folder would keep it when the scratch goes.
    let cleared = lib.call("chmod -N \"$D\"", &[]);
    let recs = lib.run(&scratch);
    assert_eq!(recs[cleared].rc, 0, "chmod -N: {}", recs[cleared].err);
    for ((label, _, want), i) in rows.iter().zip(calls) {
        assert_eq!(recs[i].rc, 0, "{label}: {}", recs[i].err);
        assert_eq!(
            recs[i].out.lines().collect::<Vec<_>>(),
            [*want, *want],
            "{label}: the folder, and a link to it"
        );
    }
    assert_eq!(
        recs[as_account].rc, 0,
        "a run as the account installs where it can: {}",
        recs[as_account].err
    );
}

/// A dry run makes nothing, so under root it judges an install folder still to
/// be made as `mkdir -p` would make it: by the entries the folder above it
/// passes on (acl_passed_on). Each verdict, one folder down, two down, and one
/// down spelled with a trailing slash, is the one the real run reaches on the
/// folder `mkdir -p` makes there; a listing that cannot be read refuses.
#[cfg(target_os = "macos")]
#[test]
fn under_root_a_dry_run_judges_an_install_folder_to_be_made_as_the_run_would() {
    let scratch = Scratch::new();
    let mut lib = Lib::new();
    lib.step(format!(
        "{FOREIGN_FIND}\n{FAKE_ROOT}\nFOREIGN_LIST=\"$TMPDIR/foreign\"; : >\"$FOREIGN_LIST\"\n\
         n=0\n\
         verdict() {{ if root_dest_acl_open \"$1\"; then echo refused; else echo installs; fi; }}\n\
         judged() {{\n\
         \tlocal p=\"$TMPDIR/p$((n += 1))\" d dry\n\
         \tmkdir \"$p\" && chmod 755 \"$p\" && {{ [ -z \"$1\" ] || chmod +a \"$1\" \"$p\"; }} || return\n\
         \tfor d in \"$p/one\" \"$p/two/deep\" \"$p/three/\"; do\n\
         \t\tDRY_RUN=1; dry=\"$(verdict \"$d\")\"; DRY_RUN=0\n\
         \t\tmkdir -p \"$d\" || return\n\
         \t\techo \"$dry $(verdict \"$d\")\"\n\
         \tdone\n\
         \tchmod -R -N \"$p\"\n\
         }}"
    ));
    let rows = [
        ("no ACL", "", ["installs"; 3]),
        (
            "an entry for the folder alone",
            "user:nobody allow add_subdirectory",
            ["installs"; 3],
        ),
        (
            "a deny entry for the folder alone",
            "group:everyone deny delete",
            ["installs"; 3],
        ),
        (
            "an entry folders inherit",
            "user:nobody allow add_subdirectory,directory_inherit",
            ["refused"; 3],
        ),
        (
            "an entry only the folder's own children inherit",
            "user:nobody allow list,directory_inherit,limit_inherit",
            ["refused", "installs", "refused"],
        ),
        (
            "an entry files inherit",
            "user:nobody allow add_file,file_inherit",
            ["installs"; 3],
        ),
        (
            "an entry for what is made in it alone",
            "user:nobody allow list,directory_inherit,only_inherit",
            ["refused"; 3],
        ),
    ];
    let calls: Vec<usize> = rows
        .iter()
        .map(|(_, ace, _)| lib.call(format!("judged '{ace}'"), &[]))
        .collect();
    let unread = lib.call(
        "( mkdir \"$TMPDIR/q\" && ls_entry() { return 1; } && DRY_RUN=1 && \
         root_dest_acl_open \"$TMPDIR/q/new\" )",
        &[],
    );
    let as_account = lib.call(
        "( unset -f id; DRY_RUN=1; mkdir \"$TMPDIR/inh\" && \
         chmod +a 'user:nobody allow list,directory_inherit' \"$TMPDIR/inh\" && \
         ! root_dest_acl_open \"$TMPDIR/inh/new\" )",
        &[],
    );
    let recs = lib.run(&scratch);
    for ((label, _, want), i) in rows.iter().zip(calls) {
        assert_eq!(recs[i].rc, 0, "{label}: {}", recs[i].err);
        let said: Vec<&str> = recs[i].out.lines().collect();
        let want: Vec<String> = want.iter().map(|w| format!("{w} {w}")).collect();
        assert_eq!(
            said, want,
            "{label}: the dry run's verdict and the real run's, one down, two down, one down/"
        );
    }
    assert_eq!(
        recs[unread].rc, 0,
        "an unread listing refuses: {}",
        recs[unread].err
    );
    assert_eq!(
        recs[as_account].rc, 0,
        "a run as the account is never refused: {}",
        recs[as_account].err
    );
}

/// `sh install.sh` starts bash in POSIX mode, where a command substitution keeps set -e and
/// `head`'s early exit from a long `find` would end a root walk with no answer. The script
/// leaves that mode first, so the walk names the tree; a walk that ends without an answer
/// refuses all the same, naming the path it walked.
#[test]
fn under_sh_a_root_walk_names_what_it_finds_and_one_that_ends_early_refuses() {
    let scratch = Scratch::new();
    let mut lib = Lib::new();
    lib.env("POSIXLY_CORRECT", "1");
    let mode = lib.call(
        "if shopt -oq posix; then echo posix; else echo bash; fi; \
         if shopt -q inherit_errexit 2>/dev/null; then echo inherits; else echo own; fi",
        &[],
    );
    let walk = lib.call(
        "d=\"$TMPDIR/many\"; mkdir \"$d\" && \
         for ((i = 0; i < 3000; i++)); do : >\"$d/an-entry-another-account-owns-$i\"; done && \
         o=\"$(root_tree_offender \"$d\")\" && printf '%s\\n' \"$o\" && [ -n \"$o\" ]",
        &[],
    );
    let ends_early = lib.call(
        "( root_tree_failing() { return 1; }; ROOT=\"$TMPDIR/co\"; \
         ! root_build_preflight && printf '%s\\n' \"$CARGO_RESOLVE_REASON\" )",
        &[],
    );
    let recs = lib.run(&scratch);
    assert_eq!(recs[mode].rc, 0, "{}", recs[mode].err);
    assert_eq!(recs[mode].out.lines().collect::<Vec<_>>(), ["bash", "own"]);
    let tmp = scratch.join("tmp");
    assert_eq!(recs[walk].rc, 0, "the walk answered: {}", recs[walk].err);
    assert_eq!(recs[walk].out.trim_end(), s(&tmp.join("many")));
    assert_eq!(recs[ends_early].rc, 0, "{}", recs[ends_early].err);
    assert!(
        recs[ends_early]
            .out
            .contains(&format!("and {} is such", s(&tmp.join("co")))),
        "{}",
        recs[ends_early].out
    );
}

#[test]
fn a_home_still_to_be_made_is_judged_by_the_folder_it_lands_in() {
    let scratch = Scratch::new();
    let mut lib = Lib::new();
    let anchors = lib.call(
        "( HOME=\"$HOME/not/yet\"; home_anchor; home_owner_uid; HOME=/no-such-folder/x; home_anchor; \
         HOME=; home_anchor )",
        &[],
    );
    let named = lib.call(
        "( h=\"$HOME\"; account_homes() { echo \"$h\"; }; HOME=\"$HOME/not/yet\"; home_owner_named )",
        &[],
    );
    let recs = lib.run(&scratch);
    let home = scratch.join("home");
    assert_eq!(recs[anchors].rc, 0, "{}", recs[anchors].err);
    assert_eq!(
        recs[anchors].out.lines().collect::<Vec<_>>(),
        [
            s(&home).to_string(),
            std::fs::metadata(&home).expect("HOME").uid().to_string(),
            "/".to_string(),
            "/".to_string()
        ]
    );
    assert_eq!(
        recs[named].rc, 0,
        "a HOME to be made in the account's own home is in it"
    );
}

#[test]
fn a_source_build_names_what_it_cannot_change_before_it_starts() {
    let scratch = Scratch::new();
    let tmp = scratch.join("tmp");
    let home = scratch.join("home");
    let mut lib = Lib::new();
    lib.step(
        "ROOT=\"$TMPDIR/co\"; mkdir -p \"$ROOT/target.noindex/release/deps\" \
         \"$ROOT/target.noindex/debug/deps\" \"$HOME/.cargo/registry/index\" \"$HOME/.cargo/bin\" \
         \"$HOME/.cargo/git/db\" && \
         ln -s target.noindex \"$ROOT/target\" && : >\"$HOME/.cargo/.package-cache\" && \
         : >\"$HOME/.cargo/.crates.toml\" && ln -s /usr/bin \"$HOME/linked-cargo\"",
    );
    let not_me = "id() { if [ \"$1\" = -u ]; then echo 424242424; else command id \"$@\"; fi; }";
    let user = lib.call("id -un", &[]);
    let clean = lib.call("source_build_preflight", &["CARGO_RESOLVE_REASON"]);
    let stale = lib.call(format!("( {not_me}; source_build_preflight )"), &[]);
    let reason = lib.call(
        format!("( {not_me}; source_build_preflight; echo \"$CARGO_RESOLVE_REASON\" )"),
        &[],
    );
    // What the walk reads: each folder the build writes in, never the rest (the
    // folders in cargo's home one level deep); git/ only when Cargo.lock names a
    // git source, or there is no Cargo.lock.
    let locks = [
        ("no Cargo.lock", "rm -f \"$ROOT/Cargo.lock\"", true),
        (
            "registry sources alone",
            "printf 'source = \"registry+https://example.invalid/index\"\\n' >\"$ROOT/Cargo.lock\"",
            false,
        ),
        (
            "a git source",
            "printf 'source = \"registry+https://example.invalid/index\"\\nsource = \"git+https://example.invalid/x#0\"\\n' \
             >\"$ROOT/Cargo.lock\"",
            true,
        ),
    ];
    let walked: Vec<usize> = locks
        .iter()
        .map(|(_, lock, _)| {
            lib.call(
                format!(
                    "( {not_me}; {lock}; rm -f \"$TMPDIR/walked\"; find() {{ if [ \"$2\" = -maxdepth ]; then \
                     echo \"depth$3 $1\"; else echo \"tree $1\"; fi >>\"$TMPDIR/walked\"; command find \"$@\"; }}; \
                     source_build_preflight; LC_ALL=C sort \"$TMPDIR/walked\" )"
                ),
                &[],
            )
        })
        .collect();
    let elsewhere = lib.call(
        format!("( {not_me}; ROOT=/nonexistent; CARGO_HOME=/usr/bin; source_build_preflight )"),
        &[],
    );
    // A link to a folder whose parent is not this account's names nothing.
    let linked_elsewhere = lib.call(
        format!(
            "( {not_me}; ROOT=/nonexistent; CARGO_HOME=\"$HOME/linked-cargo\"; source_build_preflight )"
        ),
        &[],
    );
    let recs = lib.run(&scratch);
    for i in [clean, elsewhere, linked_elsewhere] {
        assert_eq!(recs[i].rc, 0, "call {i}: {}", recs[i].err);
    }
    assert_eq!(recs[clean].var("CARGO_RESOLVE_REASON"), "");
    let (target, cargo) = (
        s(&tmp.join("co/target.noindex")).to_string(),
        s(&home.join(".cargo")).to_string(),
    );
    assert_eq!(recs[stale].rc, 1, "{}", recs[stale].err);
    let reason = &recs[reason].out;
    assert!(
        reason.contains(&format!(
            "sudo chown -R {} {target} {cargo}",
            recs[user].out.trim()
        )),
        "{reason}"
    );
    assert_eq!(
        reason.matches(&target).count(),
        2,
        "the physical target dir, where the build writes and in the chown: {reason}"
    );
    assert_eq!(
        reason.matches(s(&tmp.join("co/target"))).count(),
        2,
        "never the link: {reason}"
    );
    for ((label, _, git), i) in locks.iter().zip(walked) {
        let mut want = vec![format!("depth0 {cargo}"), format!("depth0 {target}")];
        if *git {
            want.push(format!("depth1 {cargo}/git"));
            want.push(format!("depth1 {cargo}/git/db"));
        }
        want.push(format!("depth1 {cargo}/registry"));
        want.push(format!("depth1 {cargo}/registry/index"));
        want.push(format!("tree {target}/release"));
        assert_eq!(recs[i].out.lines().collect::<Vec<_>>(), want, "{label}");
    }
}

#[test]
fn under_root_the_source_lane_runs_no_aterm_or_toolchain_another_account_can_change() {
    let scratch = Scratch::new();
    let tmp = scratch.join("tmp");
    let mut lib = Lib::new();
    lib.step(
        "ROOT=\"$TMPDIR/co\"; mkdir -p \"$ROOT\" \"$TMPDIR/bin\" \"$TMPDIR/both\" \"$TMPDIR/cargo-only\" \
         \"$TMPDIR/store/trust/1/bin\" \"$TMPDIR/cargo-home\" && \
         printf '[toolchain]\\nchannel = \"trust\"\\n' >\"$ROOT/rust-toolchain.toml\" && \
         for f in bin/aterm both/rustup both/cargo cargo-only/cargo store/trust/1/bin/targo; do \
         printf '#!/bin/sh\\necho \"%s $*\" >>\"$TMPDIR/ran\"\\n' \"$f\" >\"$TMPDIR/$f\" && chmod 755 \"$TMPDIR/$f\"; \
         done; export CARGO_HOME=\"$TMPDIR/cargo-home\"",
    );
    let find = |dirs: &str| {
        format!(
            "PATH=\"{dirs}:{}\" find_cargo_and_rustup \"$ROOT\"",
            system_path()
        )
    };
    let vars = ["CARGO_LANE", "CARGO_RESOLVE_REASON"];
    let as_account = lib.call(find("$TMPDIR/bin:$TMPDIR/cargo-only"), &vars);
    let ran_as_account = lib.call("cat \"$TMPDIR/ran\" && rm \"$TMPDIR/ran\"", &[]);
    lib.step("id() { if [ \"$1\" = -u ]; then echo 0; else command id \"$@\"; fi; }");
    let rows = [
        ("$TMPDIR/bin:$TMPDIR/both", "rustup", "both/rustup"),
        (
            "$TMPDIR/bin:$TMPDIR/cargo-only",
            "rustup",
            "cargo-only/cargo",
        ),
        (
            "$TMPDIR/bin:$TMPDIR/store/trust/1/bin",
            "store",
            "store/trust/1/bin/targo",
        ),
    ];
    let calls: Vec<usize> = rows
        .iter()
        .map(|(dirs, ..)| lib.call(find(dirs), &vars))
        .collect();
    let recs = lib.run(&scratch);
    assert_eq!(
        recs[ran_as_account].out,
        "bin/aterm pkg which targo\ncargo-only/cargo --version",
        "a run as the account asks the aterm and the cargo: {}",
        recs[as_account].var("CARGO_RESOLVE_REASON")
    );
    for ((dirs, lane, refused), i) in rows.iter().zip(calls) {
        let rec = &recs[i];
        assert_eq!(rec.rc, 1, "{dirs}: {}", rec.err);
        assert_eq!(rec.var("CARGO_LANE"), *lane, "{dirs}");
        let reason = rec.var("CARGO_RESOLVE_REASON");
        assert!(
            reason.contains(&format!(" {} ", s(&tmp.join(refused)))),
            "{dirs}: {reason}"
        );
    }
    assert!(
        !tmp.join("ran").exists(),
        "root ran {}",
        read(&tmp.join("ran"))
    );
}

#[test]
fn a_root_runs_bundle_gives_its_folders_group_write_alone() {
    let scratch = Scratch::new();
    let tmp = scratch.join("tmp");
    let mut lib = Lib::new();
    lib.step(
        "chown() { :; }; mk() { mkdir -p \"$1/Contents/MacOS\" \"$1/Contents/Resources\" && \
         printf '#!/bin/sh\\n' >\"$1/Contents/MacOS/aterm\" && chmod 775 \"$1/Contents/MacOS/aterm\" && \
         : >\"$1/Contents/Info.plist\" && : >\"$1/Contents/Resources/r\" && \
         chmod 664 \"$1/Contents/Resources/r\"; }; \
         mkdir -p \"$TMPDIR/admin\" \"$TMPDIR/own\" && chmod 775 \"$TMPDIR/admin\" && chmod 755 \"$TMPDIR/own\" && \
         mk \"$TMPDIR/admin/aterm.app.installing.x\" && mk \"$TMPDIR/own/aterm.app.installing.x\" && \
         mk \"$TMPDIR/own/aterm.app.installing.y\" && (umask 002; ln -s own \"$TMPDIR/own-link\")",
    );
    let modes = |dest: &str, stage: &str| {
        format!(
            "own_bundle_as_root \"$TMPDIR/{dest}/aterm.app.installing.{stage}\" \"$TMPDIR/{dest}\" && \
             (cd \"$TMPDIR/{dest}/aterm.app.installing.{stage}\" && find . -exec ls -nd {{}} + | \
             awk '{{print $NF, substr($1, 1, 10)}}' | sort)"
        )
    };
    let admin = lib.call(modes("admin", "x"), &[]);
    let own = lib.call(modes("own", "x"), &[]);
    // A link to that folder, which a umask of 002 (and Linux, always) makes
    // look group-writable: the folder decides.
    let linked = lib.call(modes("own-link", "y"), &[]);
    // The bundle takes this account's group: a scratch dir under /private/tmp
    // (root:wheel) would give it gid 0, which bundle_is_only lets write.
    let only = lib.call(
        "me=\"$(command id -u)\"; b=\"$TMPDIR/own/aterm.app.installing.x\"; \
         chgrp -R \"$(command id -g)\" \"$b\" && bundle_is_only \"$me\" \"$b\" && \
         chmod g+w \"$b/Contents/Info.plist\" && ! bundle_is_only \"$me\" \"$b\"",
        &[],
    );
    let entries = lib.call(
        "( ls_entry() { echo '-rwxrwxr-x 1 0 80 9 Apr 22 2024 /x'; }; ! root_entry /x ) && \
         ( ls_entry() { echo '-rwxrwxr-x 1 0 0 9 Apr 22 2024 /x'; }; root_entry /x ) && \
         ( ls_entry() { echo 'drwxrwxr-x 2 0 80 64 Apr 22 2024 /x'; }; root_entry /x ) && \
         ( ls_entry() { echo '-rwxr-xr-x 1 0 80 9 Apr 22 2024 /x'; }; root_entry /x )",
        &[],
    );
    let recs = lib.run(&scratch);
    let tree = |dir: &str| {
        [
            format!(". {dir}"),
            format!("./Contents {dir}"),
            "./Contents/Info.plist -rw-r--r--".to_string(),
            format!("./Contents/MacOS {dir}"),
            "./Contents/MacOS/aterm -rwxr-xr-x".to_string(),
            format!("./Contents/Resources {dir}"),
            "./Contents/Resources/r -rw-r--r--".to_string(),
        ]
    };
    assert_eq!(recs[admin].rc, 0, "{}", recs[admin].err);
    assert_eq!(
        recs[admin].out.lines().collect::<Vec<_>>(),
        tree("drwxrwxr-x"),
        "a folder its group can write: the bundle's folders, never its files"
    );
    for i in [own, linked] {
        assert_eq!(recs[i].rc, 0, "{}", recs[i].err);
        assert_eq!(recs[i].out.lines().collect::<Vec<_>>(), tree("drwxr-xr-x"));
    }
    let plist = tmp.join("own/aterm.app.installing.x/Contents/Info.plist");
    let group = std::fs::metadata(&plist).expect("Info.plist").gid();
    assert_eq!(
        recs[only].rc,
        if group == 0 { 1 } else { 0 },
        "group write is admitted for root's group alone"
    );
    assert_eq!(recs[entries].rc, 0, "{}", recs[entries].err);
}

/// `theirs <path>...` marks paths as another uid's, everything below them
/// included (a test cannot give a file away), in `$FOREIGN_LIST`; `find`'s
/// `! -user <uid>` asks for those alone, and the rest of each walk is find's.
/// Under `FAKE_ROOT`, a walk of folders all outside its `$SCRATCH` finds
/// nothing, as its `ls` shows them root's: a walk from the scratch up to `/`
/// (each `.cargo/` above a checkout) would otherwise judge the host's own
/// folders, such as a `~/.cargo` a umask of 002 left group-writable.
const FOREIGN_FIND: &str = r#"theirs() { for p; do printf '%s\n' "$p" >>"$FOREIGN_LIST"; done; }
theirs_at() {
	local f
	while IFS= read -r f; do
		[ -n "$f" ] || continue
		case "$1" in "$f" | "$f"/*) return 0 ;; esac
	done <"$FOREIGN_LIST"
	return 1
}
find() {
	local -a a=()
	local f inside=0
	if [ -n "${SCRATCH:-}" ]; then
		for f; do
			case "$f" in -H | -L | -P) continue ;; -* | '(' | '!') break ;; esac
			case "$f" in "$SCRATCH" | "$SCRATCH"/*) inside=1 ;; esac
		done
		[ "$inside" -eq 1 ] || return 0
	fi
	while [ $# -gt 0 ]; do
		if [ "$1" = '!' ] && [ "${2:-}" = -user ]; then
			a+=('(' -false)
			while IFS= read -r f; do
				[ -n "$f" ] && a+=(-o -path "$f" -o -path "$f/*")
			done <"$FOREIGN_LIST"
			a+=(')')
			shift 3
			continue
		fi
		a+=("$1")
		shift
	done
	command find "${a[@]}"
}"#;

/// Root, over a scratch whose every entry `ls` shows as root's but those
/// `theirs` marks, uid 424242's, below folders that are plain root ones.
const FAKE_ROOT: &str = r#"id() { if [ "$1" = -u ]; then echo 0; else command id "$@"; fi; }
SCRATCH="${TMPDIR%/tmp}"
chmod 755 "$SCRATCH" "$TMPDIR"
ls_entry() {
	local p="${!#}" t=- u=0
	case "$p" in
	"$SCRATCH" | "$SCRATCH"/*)
		theirs_at "$p" && u=424242
		command ls "$@" | awk -v u="$u" 'NR == 1 { $3 = u } { print }'
		;;
	*)
		if [ -L "$p" ]; then t=l; elif [ -d "$p" ]; then t=d; fi
		echo "${t}rwxr-xr-x 2 0 0 64 Apr 22 2024 $p"
		;;
	esac
}"#;

/// What a root source build reads, all root's: a checkout with a build in
/// target/, a `.cargo/config.toml` above it, cargo's home, a developer folder
/// with its SDK (DEVELOPER_DIR), and a store whose targo writes the PATH it ran
/// on to `$TMPDIR/build-path`, the git config variables it was given to
/// `$TMPDIR/build-git`, and the developer folder, SDK and toolchain xcrun would
/// take to `$TMPDIR/build-dev`.
const ROOT_LAYOUT: &str = r#"umask 022
mk() {
	W="$TMPDIR/$1"
	FOREIGN_LIST="$TMPDIR/$1.foreign"
	: >"$FOREIGN_LIST"
	mkdir -p "$W/co/src" "$W/co/target/release/deps" "$W/home/.cargo/registry/src/idx" \
		"$W/home/.cargo/bin" "$W/store/trust/1/bin" "$W/.cargo" "$W/their-bin" \
		"$W/dev/usr/bin" "$W/dev/SDKs/MacOSX.sdk"
	printf 'fn main() {}\n' >"$W/co/src/main.rs"
	: >"$W/co/target/release/deps/libx.rlib"
	printf '[build]\n' >"$W/.cargo/config.toml"
	printf '#!/bin/sh\necho "$PATH" >"$TMPDIR/build-path"\necho "${GIT_CONFIG_GLOBAL-unset} ${GIT_CONFIG_NOSYSTEM-unset}" >"$TMPDIR/build-git"\necho "${DEVELOPER_DIR-unset} ${SDKROOT-unset} ${TOOLCHAINS-unset}" >"$TMPDIR/build-dev"\n' \
		>"$W/store/trust/1/bin/targo"
	printf '#!/bin/sh\n' >"$W/their-bin/sccache"
	cp "$W/their-bin/sccache" "$W/their-bin/rustc"
	cp "$W/their-bin/sccache" "$W/dev/usr/bin/clang"
	chmod -R u=rwX,go=rX "$W" && chmod 755 "$W/store/trust/1/bin/targo" "$W/their-bin/"* "$W/dev/usr/bin/clang"
	theirs "$W/their-bin"
	ROOT="$W/co" HOME="$W/home" CARGO_LANE=store CARGO_BIN="$W/store/trust/1/bin/targo" DEVELOPER_DIR="$W/dev"
	unset CARGO_HOME RUSTC RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER SDKROOT TOOLCHAINS
}"#;

/// What a root build refuses: the path it names, relative to the row's folder
/// ("" for the folder itself, "/x" for a bare `x`), and whose it says that is.
type Refusal = Option<(&'static str, &'static str)>;

#[test]
fn under_root_a_source_build_reads_and_runs_nothing_another_account_can_change() {
    let scratch = Scratch::new();
    let tmp = scratch.join("tmp");
    let mut lib = Lib::new();
    lib.step(format!("{FOREIGN_FIND}\n{FAKE_ROOT}\n{ROOT_LAYOUT}"));
    // (label, setup, None for a build)
    let rows: [(&str, &str, Refusal); 29] = [
        ("root's alone", "", None),
        (
            "a file of theirs in the checkout",
            ": >\"$W/co/src/lib.rs\"; theirs \"$W/co/src/lib.rs\"",
            Some(("co/src/lib.rs", "checkout")),
        ),
        (
            "what target/ already holds",
            "theirs \"$W/co/target/release/deps/libx.rlib\"",
            Some(("co/target/release/deps/libx.rlib", "checkout")),
        ),
        (
            "an ancestor's .cargo/config.toml",
            "theirs \"$W/.cargo/config.toml\"",
            Some((".cargo/config.toml", "checkout")),
        ),
        (
            "cargo home's config.toml",
            "printf '[build]\\nrustc = \"x\"\\n' >\"$W/home/.cargo/config.toml\"; \
             theirs \"$W/home/.cargo/config.toml\"",
            Some(("home/.cargo/config.toml", "cargo home")),
        ),
        (
            "a cc in cargo home's bin/",
            ": >\"$W/home/.cargo/bin/cc\"; theirs \"$W/home/.cargo/bin/cc\"",
            Some(("home/.cargo/bin/cc", "cargo home")),
        ),
        (
            "a cargo home still to be made in their folder",
            "HOME=\"$W/their-home\"; mkdir -p \"$HOME\"; theirs \"$HOME\"",
            Some(("their-home", "cargo home")),
        ),
        (
            "a folder above the checkout its group can write",
            "chgrp \"$(command id -g)\" \"$W\" && chmod g+w \"$W\"",
            Some(("", "checkout")),
        ),
        (
            "a sticky folder above it every account can write",
            "chmod 1777 \"$W\"",
            Some(("", "checkout")),
        ),
        (
            "a folder in it its group can write",
            "chgrp \"$(command id -g)\" \"$W/co/src\" && chmod g+w \"$W/co/src\"",
            Some(("co/src", "checkout")),
        ),
        (
            "a link out of the checkout",
            "mkdir -p \"$W/elsewhere\" && ln -s \"$W/elsewhere\" \"$W/co/vendor\"",
            Some(("co/vendor", "checkout")),
        ),
        ("a link inside it", "ln -s src \"$W/co/alias\"", None),
        (
            "a link of theirs inside it",
            "ln -s src \"$W/co/alias\"; theirs \"$W/co/alias\"",
            cfg!(target_os = "macos").then_some(("co/alias", "checkout")),
        ),
        (
            "a link inside it by a path with ..",
            "ln -s ../co/src \"$W/co/up\"",
            None,
        ),
        (
            "a link back into it through a link of theirs",
            "mkdir -p \"$W/other\" && ln -s \"$W/co/src\" \"$W/other/hop\" && \
             ln -s \"$W/other/hop\" \"$W/co/vendor\" && theirs \"$W/other\"",
            Some(("co/vendor", "checkout")),
        ),
        (
            "a link back into it by a path through their folder",
            "mkdir -p \"$W/other/sub\" && ln -s \"$W/other/sub/../../co/src\" \"$W/co/vendor\" && \
             theirs \"$W/other\"",
            Some(("co/vendor", "checkout")),
        ),
        (
            "a link back into it by a relative path through their folder",
            "mkdir -p \"$W/other\" && ln -s \"$W/co/src\" \"$W/other/hop\" && \
             ln -s ../other/hop \"$W/co/vendor\" && theirs \"$W/other\"",
            Some(("co/vendor", "checkout")),
        ),
        (
            "a link back into it through a link of root's",
            "mkdir -p \"$W/sealed\" && ln -s \"$W/co/src\" \"$W/sealed/hop\" && \
             ln -s \"$W/sealed/hop\" \"$W/co/vendor\"",
            None,
        ),
        (
            "a link back into it through a root link that leads through their folder",
            "mkdir -p \"$W/sealed\" \"$W/other\" && ln -s \"$W/co\" \"$W/other/co\" && \
             ln -s \"$W/other/co\" \"$W/sealed/mid\" && ln -s \"$W/sealed/mid/src\" \"$W/co/vendor\" && \
             theirs \"$W/other\"",
            Some(("co/vendor", "checkout")),
        ),
        (
            "a link whose target ends in a newline",
            "ln -s \"$W/co/src\"$'\\n' \"$W/co/vendor\"",
            Some(("co/vendor", "checkout")),
        ),
        (
            "a worktree's common git dir of theirs",
            "mkdir -p \"$W/repo/.git/worktrees/co\" \"$W/repo/.git/objects\" && \
             printf 'gitdir: %s\\n' \"$W/repo/.git/worktrees/co\" >\"$W/co/.git\" && \
             printf '../..\\n' >\"$W/repo/.git/worktrees/co/commondir\" && theirs \"$W/repo/.git/objects\"",
            Some(("repo/.git/objects", "checkout")),
        ),
        (
            "a rustc wrapper of theirs",
            "export RUSTC_WRAPPER=\"$W/their-bin/sccache\"",
            Some(("their-bin/sccache", "toolchain")),
        ),
        (
            "a wrapper's bare name only their folder on PATH holds",
            "export PATH=\"$W/their-bin:$PATH\" RUSTC_WRAPPER=sccache",
            Some(("/sccache", "toolchain")),
        ),
        (
            "a wrapper of root's",
            "export RUSTC_WORKSPACE_WRAPPER=\"$W/store/trust/1/bin/targo\"",
            None,
        ),
        (
            "RUSTC of theirs on rustup's lane",
            "CARGO_LANE=rustup; export RUSTC=\"$W/their-bin/rustc\"",
            Some(("their-bin/rustc", "toolchain")),
        ),
        (
            "RUSTC of theirs on the store lane, which scrubs it",
            "export RUSTC=\"$W/their-bin/rustc\"",
            None,
        ),
        (
            "a program of theirs beside the lane's cargo",
            "printf '#!/bin/sh\\n' >\"$W/store/trust/1/bin/cc\"; chmod 755 \"$W/store/trust/1/bin/cc\"; \
             theirs \"$W/store/trust/1/bin/cc\"",
            Some(("store/trust/1/bin", "toolchain")),
        ),
        (
            "root's own wrapper by a bare name, in cargo home's bin/ on rustup's lane",
            "CARGO_LANE=rustup; printf '#!/bin/sh\\n' >\"$W/home/.cargo/bin/sccache\"; \
             chmod 755 \"$W/home/.cargo/bin/sccache\"; export RUSTC_WRAPPER=sccache",
            None,
        ),
        (
            "root's own wrapper by a bare name, beside the store's targo",
            "printf '#!/bin/sh\\n' >\"$W/store/trust/1/bin/sccache\"; \
             chmod 755 \"$W/store/trust/1/bin/sccache\"; export RUSTC_WRAPPER=sccache",
            None,
        ),
    ];
    let calls: Vec<usize> = rows
        .iter()
        .enumerate()
        .map(|(n, (_, setup, _))| {
            lib.call(
                format!(
                    "( mk row{n}; {setup}\n source_build_preflight; rc=$?; \
                     echo \"${{CARGO_RESOLVE_REASON:-}}\"; exit $rc )"
                ),
                &[],
            )
        })
        .collect();
    let built = lib.call(
        format!(
            "( mk build; PATH=\"$W/their-bin:{}\"; cargo_build_pinned \"$ROOT\" && cat \"$TMPDIR/build-path\" )",
            system_path()
        ),
        &[],
    );
    let git = "cat \"$TMPDIR/build-git\"";
    let built_git = lib.call(git, &[]);
    let account_git = lib.call(
        format!("( unset -f id; mk account; cargo_build_pinned \"$ROOT\" && {git} )"),
        &[],
    );
    let recs = lib.run(&scratch);
    for (n, ((label, _, want), i)) in rows.iter().zip(calls).enumerate() {
        let rec = &recs[i];
        let reason = rec.out.trim_end();
        let Some((rel, whose)) = want else {
            assert_eq!(rec.rc, 0, "{label}: {reason}{}", rec.err);
            continue;
        };
        assert_eq!(rec.rc, 1, "{label}: root builds it: {}", rec.err);
        let row = tmp.join(format!("row{n}"));
        let named = match *rel {
            "" => s(&row).to_string(),
            bare if bare.starts_with('/') => bare[1..].to_string(),
            rel => s(&row.join(rel)).to_string(),
        };
        assert!(
            reason.contains(&format!("and {named} ")) || reason.contains(&format!("and {named},")),
            "{label}: names {named}: {reason}"
        );
        assert!(
            reason.contains(&format!("whose {whose} it is")),
            "{label}: {reason}"
        );
    }
    assert_eq!(recs[built].rc, 0, "{}", recs[built].err);
    assert_eq!(
        recs[built].out.trim_end(),
        format!(
            "{}:{}",
            s(&tmp.join("build/store/trust/1/bin")),
            system_path()
        ),
        "root's build runs on PATH's root folders alone"
    );
    assert_eq!(
        recs[built_git].out, "/dev/null 1",
        "root's build reads no git config of HOME's or the system's"
    );
    assert_eq!(recs[account_git].rc, 0, "{}", recs[account_git].err);
    assert_eq!(
        recs[account_git].out, "unset unset",
        "the account's build reads its own"
    );
}

#[test]
fn under_root_git_over_the_checkout_is_roots_and_reads_no_config_of_homes() {
    let scratch = Scratch::new();
    let mut lib = Lib::new();
    lib.step(format!(
        "{FOREIGN_FIND}\n{FAKE_ROOT}\n{ROOT_LAYOUT}\n\
         mk git; mkdir -p \"$W/root-bin\" && for b in their-bin root-bin; do \
         printf '#!/bin/sh\\necho \"%s ${{GIT_CONFIG_GLOBAL-unset}} ${{GIT_CONFIG_NOSYSTEM-unset}} $*\"\\n' \"$b\" \
         >\"$W/$b/git\" && chmod 755 \"$W/$b/git\"; done; export PATH=\"$W/their-bin:$W/root-bin:{}\"",
        system_path()
    ));
    let as_root = lib.call("checkout_git -C \"$ROOT\" rev-parse HEAD", &["ROOT"]);
    let as_account = lib.call(
        "( unset -f id; checkout_git -C \"$ROOT\" rev-parse HEAD )",
        &[],
    );
    let recs = lib.run(&scratch);
    let root = recs[as_root].var("ROOT");
    assert_eq!(recs[as_root].rc, 0, "{}", recs[as_root].err);
    assert_eq!(
        recs[as_root].out,
        format!("root-bin /dev/null 1 -C {root} rev-parse HEAD"),
        "root runs the git in its own folder, reading no git config of HOME's or the system's"
    );
    assert_eq!(recs[as_account].rc, 0, "{}", recs[as_account].err);
    assert_eq!(
        recs[as_account].out,
        format!("their-bin unset unset -C {root} rev-parse HEAD"),
        "the account runs PATH's"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn under_root_an_install_folder_on_a_volume_that_ignores_owners_is_skipped_before_the_download() {
    let scratch = Scratch::new();
    let mut lib = Lib::new();
    lib.step(
        "installer_uid() { echo 0; }\n\
         df() { [ -e \"${!#}\" ] || return 1; printf 'Filesystem 512-blocks Used Available Capacity Mounted on\\n%s 1 1 1 1%% /Volumes/X\\n' \"$DEV\"; }\n\
         mount_table() { printf '%s\\n' \"$MOUNTS\"; }\n\
         M=$'/dev/disk3s1s1 on / (apfs, sealed, local, read-only, journaled)\\n\
         /dev/disk6s1 on /Volumes/USB (exfat, local, nodev, nosuid, noowners, noatime, nobrowse)\\n\
         /dev/disk6s10 on /Volumes/Back on Up (apfs, local, journaled)\\n\
         /dev/disk7s1 on /Volumes/Ext (apfs, local, nodev, nosuid, journaled, noowners)\\n\
         /dev/disk8s1 on /Volumes/noowners (apfs, local, journaled)\\n\
         //me@nas/share on /Volumes/share (smbfs, nodev, nosuid, mounted by me)'",
    );
    // (the device df names for the folder, whether the app half is skipped)
    let rows = [
        ("/dev/disk6s1", true),
        ("/dev/disk7s1", true),
        ("/dev/disk6s10", false),
        ("/dev/disk3s1s1", false),
        ("//me@nas/share", false),
        ("/dev/disk9s1", false),
        ("/dev/disk7", false),
        ("/dev/disk8s1", false),
        ("", false),
    ];
    let calls: Vec<usize> = rows
        .iter()
        .map(|(dev, _)| {
            lib.call(
                format!(
                    "( DEV='{dev}'; MOUNTS=\"$M\"; root_dest_keeps_no_owner \"$TMPDIR/apps/new\" )"
                ),
                &[],
            )
        })
        .collect();
    let account = lib.call(
        "( installer_uid() { echo 424242; }; DEV=/dev/disk6s1; MOUNTS=\"$M\"; \
         root_dest_keeps_no_owner \"$TMPDIR/apps\" )",
        &[],
    );
    // A table not read in time judges nothing, whatever it printed first.
    let late = lib.call(
        "( DEV=/dev/disk6s1; macos_mounts() { printf '%s\\n' \"$M\"; return 124; }; \
         root_dest_keeps_no_owner \"$TMPDIR/apps/new\" )",
        &[],
    );
    let recs = lib.run(&scratch);
    for ((dev, skips), call) in rows.iter().zip(calls) {
        assert_eq!(
            recs[call].rc,
            i32::from(!skips),
            "a folder on {dev:?}: {}",
            recs[call].err
        );
    }
    assert_eq!(recs[account].rc, 1, "the account's own run is not judged");
    assert_eq!(recs[late].rc, 1, "{}", recs[late].err);
}

#[cfg(target_os = "macos")]
#[test]
fn under_root_no_app_in_a_folder_another_account_can_change_backs_the_cli() {
    let scratch = Scratch::new();
    let mut lib = Lib::new();
    lib.step(format!(
        "{FOREIGN_FIND}\n{FAKE_ROOT}\nFOREIGN_LIST=\"$TMPDIR/foreign\"; : >\"$FOREIGN_LIST\"\n\
         umask 022; W=\"$TMPDIR/w\"; VERIFY_WAIT_SECS=5\n\
         app() {{ mkdir -p \"$W/$1/aterm.app/Contents/MacOS\" && \
         printf '#!/bin/sh\\necho aterm 9.9.9\\n' >\"$W/$1/aterm.app/Contents/MacOS/aterm\" && \
         chmod 755 \"$W/$1/aterm.app/Contents/MacOS/aterm\"; }}\n\
         app sealed && app admin && app theirs && theirs \"$W/theirs\" && \
         chgrp \"$(command id -g)\" \"$W/admin\" && chmod g+w \"$W/admin\" && mkdir -p \"$W/none\" \"$W/bin\" \"$W/tbin\" && \
         cp \"$W/sealed/aterm.app/Contents/MacOS/aterm\" \"$W/tbin/aterm\" && theirs \"$W/tbin\"\n\
         bundle_dirs() {{ local d; for d in $DIRS; do printf '%s\\n' \"$W/$d\"; done; }}"
    ));
    // (where find_bundle_cli looks, the folder root_bundle_unsealed names)
    let rows = [
        ("DIRS=sealed; root_bundle_unsealed", ""),
        ("DIRS=admin; root_bundle_unsealed", "/admin"),
        ("DIRS='none admin'; root_bundle_unsealed", "/admin"),
        ("DIRS='admin sealed'; root_bundle_unsealed", ""),
        ("DIRS='theirs admin'; root_bundle_unsealed", "/theirs"),
        (
            "DIRS=admin/new; root_bundle_unsealed \"$W/admin/new\"",
            "/admin",
        ),
        (
            "DIRS=sealed/new; root_bundle_unsealed \"$W/sealed/new\"",
            "",
        ),
        (
            "DIRS=sealed/no/new; root_bundle_unsealed \"$W/sealed/no/new\"",
            "/sealed/no/new",
        ),
        (
            "installer_uid() { echo 424242; }; DIRS=admin; root_bundle_unsealed",
            "",
        ),
    ];
    let calls: Vec<usize> = rows
        .iter()
        .map(|(code, _)| lib.call(format!("( {code} )"), &["W"]))
        .collect();
    let drivers = [
        (
            "DIRS=admin; BIN_DIR=\"$W/bin\"",
            "skip no installed `aterm` to drive it",
        ),
        (
            "DIRS=sealed; BIN_DIR=\"$W/bin\"",
            "run {w}/sealed/aterm.app/Contents/MacOS/aterm",
        ),
        (
            "DIRS=sealed; BIN_DIR=\"$W/tbin\"",
            "skip another account can change {w}/tbin/aterm ",
        ),
    ];
    let driven: Vec<usize> = drivers
        .iter()
        .map(|(setup, _)| lib.call(format!("( {setup}; toolset_driver )"), &["W"]))
        .collect();
    let refusal = lib.call("root_bundle_refusal /x 1; root_bundle_refusal /x", &[]);
    let recs = lib.run(&scratch);
    for ((code, named), call) in rows.iter().zip(calls) {
        let w = recs[call].var("W");
        let want = if named.is_empty() {
            String::new()
        } else {
            format!("{w}{named}")
        };
        assert_eq!(recs[call].out, want, "{code}: {}", recs[call].err);
    }
    for ((setup, says), call) in drivers.iter().zip(driven) {
        let w = recs[call].var("W");
        let want = says.replace("{w}", w);
        assert!(
            recs[call].out.starts_with(&want),
            "{setup}: {:?} does not begin {want:?}: {}",
            recs[call].out,
            recs[call].err
        );
    }
    let lines: Vec<&str> = recs[refusal].out.lines().collect();
    assert_eq!(lines.len(), 2, "{}", recs[refusal].out);
    assert!(
        lines[0].starts_with("root runs no aterm another account can change, and /x, ")
            && lines[0].contains(" — install the CLI as your account")
            && !lines[1].contains(" — install"),
        "the remedy only when asked: {lines:?}"
    );
}

#[test]
fn under_root_a_build_takes_no_name_a_link_another_account_can_redirect() {
    let scratch = Scratch::new();
    let mut lib = Lib::new();
    lib.step(format!(
        "{FOREIGN_FIND}\n{FAKE_ROOT}\nFOREIGN_LIST=\"$TMPDIR/foreign\"; : >\"$FOREIGN_LIST\"\n\
         umask 022; W=\"$TMPDIR/w\"; mkdir -p \"$W/other/d\" \"$W/sealed/d\" && theirs \"$W/other\" && \
         printf '#!/bin/sh\\n' >\"$W/sealed/real\" && chmod 755 \"$W/sealed/real\" && \
         ln -s \"$W/sealed/real\" \"$W/other/hop\" && ln -s \"$W/other/x\" \"$W/sealed/dl\" && \
         : >\"$W/sealed/theirs\" && theirs \"$W/sealed/theirs\" && \
         ln -s \"$W/sealed\" \"$W/other/back\" && ln -s \"$W/other/back\" \"$W/sealed/mid\" && \
         ln -s \"$W/other/evil\" \"$W/sealed/nl\"$'\\n' && mkdir \"$W/sealed/pbin\" && \
         cp \"$W/sealed/real\" \"$W/sealed/pbin/tool\" && ln -s \"$W/sealed/loop\" \"$W/sealed/loop\" && \
         ln -s \"$W/other/lb\" \"$W/sealed/la\" && ln -s \"$W/sealed/la\" \"$W/other/lb\" && \
         ln -s \"$W/sealed/real\" \"$W/sealed/tl\" && theirs \"$W/sealed/tl\" && \
         ln -s \"$W/sealed/gone\" \"$W/sealed/dg\" && mkdir -p \"$W/sealed/x/y\" \"$W/sealed/x\"$'\\n'/y && \
         {{ [[ \"$OSTYPE\" != darwin* ]] || {{ ln -s \"$W/sealed/real\" \"$W/sealed/al\" && \
         chmod -h +a 'everyone allow delete' \"$W/sealed/al\"; }}; }}\n\
         bin() {{ mkdir -p \"$W/$1\" && ln -s \"$2\" \"$W/$1/cc\" && root_bin_dir \"$W/$1\"; }}"
    ));
    // (what the bin folder's cc leads to, whether root takes programs from it)
    let rows = [
        ("$W/sealed/real", true),
        ("$W/sealed/cc", true),
        // A folder that is not there yet can be made for another account: a
        // disk image any account attaches gets one in /Volumes.
        ("$W/sealed/new/cc", false),
        ("$W/sealed/d", true),
        ("$W/other/cc", false),
        ("$W/other/new/cc", false),
        ("$W/other/d", false),
        ("$W/other/hop", false),
        ("$W/other/d/../../sealed/real", false),
        ("$W/sealed/dl/cc", false),
        ("$W/sealed/theirs", false),
        // A root link on the way, not at its end, that leads through their
        // folder (back to root's).
        ("$W/sealed/mid/real", false),
        // A target the shell would read without its newline.
        ("$W/sealed/nl\"$'\\n'\"", false),
        // A loop of root's alone leads nowhere the kernel goes; one through
        // their folder is theirs to break.
        ("$W/sealed/loop", true),
        ("$W/sealed/la", false),
        // Their link in root's folder leads where root's folder says: no
        // account rewrites a link in place, and on Linux none removes it but
        // through its folder. On macOS its owner, or an ACL on it, can.
        ("$W/sealed/tl", !cfg!(target_os = "macos")),
        ("$W/sealed/al", !cfg!(target_os = "macos")),
    ];
    let calls: Vec<usize> = rows
        .iter()
        .enumerate()
        .map(|(n, (to, _))| lib.call(format!("bin b{n} \"{to}\""), &[]))
        .collect();
    let by_mid = lib.call(
        "root_bin_dir \"$W/sealed/pbin\" && ! root_bin_dir \"$W/sealed/mid/pbin\" && \
         [ \"$(folder_failing sealed_entry \"$W/sealed/no/pbin\")\" = \"$W/sealed/no/pbin\" ] && \
         may_run \"$W/sealed/real\" && ! may_run \"$W/sealed/real\"$'\\n'",
        &[],
    );
    // A folder still to be made is judged by the one that would hold it, and
    // refused below a name still to be made, or at a link that leads nowhere.
    let to_make = lib.call(
        "[ -z \"$(root_tree_failing \"$W/sealed/new\")\" ] && \
         [ \"$(root_tree_failing \"$W/sealed/no/new\")\" = \"$W/sealed/no/new\" ] && \
         [ \"$(root_tree_failing \"$W/sealed/dg\")\" = \"$W/sealed/gone\" ]",
        &[],
    );
    // A relative way is taken from the working folder the kernel uses, whose
    // name may end in a newline that a command substitution drops.
    let nl_cwd = lib.call(
        "( cd \"$W/sealed/x\"$'\\n' && [ -z \"$(cd \"$W/sealed/x\" && folder_failing sealed_entry y)\" ] && \
         [ \"$(folder_failing sealed_entry y)\" = y ] )",
        &[],
    );
    // A real ACL, on a program in a folder PATH names by a link to it.
    let acl_by_link = cfg!(target_os = "macos").then(|| {
        lib.call(
            "mkdir \"$W/abin\" && cp \"$W/sealed/real\" \"$W/abin/tool\" && ln -s \"$W/abin\" \"$W/abinl\" && \
             root_bin_dir \"$W/abinl\" && chmod +a 'everyone allow write' \"$W/abin/tool\" && \
             ! root_bin_dir \"$W/abinl\"",
            &[],
        )
    });
    let recs = lib.run(&scratch);
    for ((to, takes), call) in rows.iter().zip(calls) {
        assert_eq!(
            recs[call].rc,
            i32::from(!takes),
            "a cc that leads to {to}: {}",
            recs[call].err
        );
    }
    assert_eq!(recs[to_make].rc, 0, "{}", recs[to_make].err);
    assert_eq!(
        recs[nl_cwd].rc, 0,
        "a working folder named with a newline refuses: {}",
        recs[nl_cwd].err
    );
    if let Some(call) = acl_by_link {
        assert_eq!(
            recs[call].rc, 0,
            "a folder named by a link has its own entries' ACLs read: {}",
            recs[call].err
        );
    }
    assert_eq!(
        recs[by_mid].rc, 0,
        "a folder of root's is refused by a way to it through their folder, one with no way to it \
         is named, and a name with a newline is not taken for the one without: {}",
        recs[by_mid].err
    );
}

/// What `under_root_a_walk_through_a_filesystem_whose_owners_say_nothing_refuses` walks:
/// folders, a way of 42 links in a mounted and in a plain folder (each led to by a bin
/// folder's `cc`), folders the first `folder_ids` call swaps for others while `SWAP` names
/// one, and a mountinfo fixture read by the real reader (`kernel_linux_mounts`) on both
/// platforms. `$W/roots` lists each mount as `<dev> <statfs type> <folder>` (`-` for a type
/// stat cannot read), and `folder_ids` and `folder_types` answer from it as the host's stat
/// would; off it, `folder_ids` reads the host on macOS and gives Linux's root device, which is
/// FUSE here.
const FOREIGN_MOUNTS: &str = r#"FOREIGN_LIST="$TMPDIR/foreign"; : >"$FOREIGN_LIST"
umask 022; W="$TMPDIR/w"; ABC="$W/a b"$'\t'"c\\d"
mkdir -p "$W/vol/d" "$W/net/d" "$W/ok/d" "$W/fl/d" "$W/tree/sub/d" "$W/bad/d" "$W/stack/d" \
	"$W/tree2/at on it" "$W/tree3/sub" "$W/pbin" "$W/obin" "$W/sib/d" "$W/sib (old)" "$W/dn/d" \
	"$W/swap/d" "$W/swap2/d" "$W/cbin" "$W/ocbin" "$W/rb/d" "$W/loc/d" "$W/fm" "$ABC/d" \
	"$W/dno/d" "$W/xt/sub" "$W/sys/d" "$W/lt/sub" "$W/xg" "$W/tt/sub" && ln -s "$W/tt" "$W/tl"
printf '#!/bin/sh\n' >"$W/pbin/cc" && chmod 755 "$W/pbin/cc"
cp "$W/pbin/cc" "$W/obin/cc" && cp "$W/pbin/cc" "$W/fm/prog" && cp "$W/pbin/cc" "$W/fm/other"
i=0
while [ "$i" -lt 42 ]; do
	ln -s "l$((i + 1))" "$W/vol/l$i" && ln -s "l$((i + 1))" "$W/ok/l$i"
	i=$((i + 1))
done
ln -s "$W/vol/l0" "$W/cbin/cc" && ln -s "$W/ok/l0" "$W/ocbin/cc"
moved() { [ -z "${SWAP:-}" ] || [ -e "$W/$SWAP.old" ] || { mv "$W/$SWAP" "$W/$SWAP.old" && mkdir -p "$W/$SWAP/d"; }; }
esc() { local p="${1//\\/\\134}"; p="${p// /\\040}"; printf '%s' "${p//$'\t'/\\011}"; }
eval "kernel_$(declare -f linux_mounts)"
: >"$W/empty"
printf '%s\n' "21 1 0:31 / / rw - fuse.fuse-overlayfs fuse-overlayfs rw,user_id=0" \
	"22 21 0:31 /x $(esc "$W/rb") rw - fuse.fuse-overlayfs fuse-overlayfs rw" \
	"23 21 0:40 / $(esc "$W/vol") rw,nosuid shared:5 - fuse.sshfs u@h:/ rw,user_id=1000" \
	"24 21 0:41 / $(esc "$W/net") rw - nfs4 h:/x rw" \
	"25 21 8:1 / $(esc "$W/ok") rw - ext4 /dev/sda1 rw" \
	"26 21 0:42 / $(esc "$W/tree/sub") rw - fuse.sshfs u@h:/ rw" \
	"27 21 0:43 / $(esc "$W/bad") rw - fuse.weird x rw" \
	"28 21 0:44 / $(esc "$W/pbin/cc") rw - fuse.sshfs x rw" \
	"29 21 0:45 / $(esc "$ABC") rw - 9p x rw" \
	"30 21 0:46 / $(esc "$W/fm/prog") rw - fuse.sshfs x rw" \
	"31 21 0:47 / $(esc "$W/loc") rw - newfs x rw" "32 21 0:48 / $(esc "$W/lt/sub") rw - newfs x rw" \
	"33 21 0:49 / $(esc "$W/tl/sub") rw - fuse.sshfs x rw" >"$W/mountinfo"
printf '%s\n' "0:31 65735546 $W/rb" "0:40 65735546 $W/vol" "0:41 6969 $W/net" "8:1 ef53 $W/ok" \
	"0:42 65735546 $W/tree/sub" "0:43 - $W/bad" "0:44 65735546 $W/pbin/cc" "0:45 1021997 $ABC" \
	"0:46 65735546 $W/fm/prog" "0:47 ef53 $W/loc" "0:48 ef53 $W/lt/sub" "0:49 65735546 $W/tl/sub" \
	>"$W/roots"
fixture_mount() {
	local dev hex r hit=""
	while IFS=' ' read -r dev hex r; do
		[ -e "$r" ] && in_folder "$1" "$r" && hit="$dev $hex $r"
	done <"$W/roots"
	[ -z "$hit" ] || printf '%s\n' "$hit"
}
folder_ids() {
	local p m dev root ino s=0
	moved
	for p; do
		if [ ! -e "$p" ]; then
			echo "stat: $p: No such file or directory" >&2
			s=1
			continue
		fi
		m="$(fixture_mount "$p")"
		if [ -n "$m" ]; then
			dev="${m%% *}"
			root="${m#* * }"
			ino=2
			[ "$p" -ef "$root" ] || ino=9
			echo "$dev $ino"
		elif [[ "$OSTYPE" == darwin* ]]; then
			command stat -f '%d %i' -- "$p" || s=1
		elif [ "$p" -ef / ]; then
			echo "0:31 2"
		else
			echo "0:31 9"
		fi
	done
	return "$s"
}
folder_types() {
	local p m s=0
	for p; do
		m="$(fixture_mount "$p")"
		m="${m#* }"
		m="${m%% *}"
		if [ ! -e "$p" ] || [ "$m" = - ]; then s=1; else echo "${m:-65735546}"; fi
	done
	return "$s"
}"#;

/// The Mac's mount table for that walk: the fixture's macOS-only mounts, then the host's own.
/// The lines naming `$W/hm/x on $W/sys`, `$W/hm/y on $W/obin/cc` and `$W/hm/z on /usr/bin`
/// each mount a folder whose name holds ` on `. Each `/dev/disk9s<n>` has the device
/// `$W/nodes` gives it: `$W/gr` is a graft that shows Preboot's device with no mount of its
/// own, `$W/un` a volume no line names, `$W/fs` a FUSE volume over a local one, `$W/uf` an
/// account's disk that a forged local line names too, and `$W/lf` a local volume with a tree.
const FOREIGN_MACOS: &str = r#"mkdir -p "$W/gr/d" "$W/un/d" "$W/lf/d" "$W/lf/t/x" "$W/fs/d" "$W/uf/d" &&
	ln -s x "$W/lf/t/l"
printf '%s\n' "1:50 - /System/Volumes/Data$W/fl" "1:51 - $W/stack" \
	"1:52 - $W/tree2/at on it" "1:53 - /System/Volumes/Data$W/tree3/sub" "1:54 - $W/sib (old)" \
	"1:55 - $W/dn" "1:56 - $W/dno" "1:57 - $W/xt/sub" "1:59 - $W/un" "1:60 - $W/lf" \
	"$(command stat -f %d /System/Volumes/Preboot) - $W/gr" "1:62 - $W/uf" "1:64 - $W/fs" \
	>>"$W/roots"
n=1
for d in 0:40 8:1 1:50 0:42 1:71 1:51 1:52 1:53 0:44 1:54 0:45 0:46 1:73 1:74 1:75 1:60 1:76 1:77 \
	1:78 1:62 1:79; do
	echo "/dev/disk9s$n $d" >>"$W/nodes"
	n=$((n + 1))
done
disk_devices() {
	local p d s=0
	for p; do
		d="$(awk -v p="$p" '$1 == p { print $2 }' "$W/nodes")"
		if [ -n "$d" ]; then echo "$d"; else command stat -f %r -- "$p" || s=1; fi
	done
	return "$s"
}
macos_mounts() {
	printf '%s\n' "/dev/disk9s1 on $W/vol (apfs, local, nodev, nosuid, journaled, mounted by someone)" \
		"//u@h/s on $W/net (smbfs, nodev, nosuid)" "/dev/disk9s2 on $W/ok (apfs, local, journaled)" \
		"/dev/disk9s3 on /System/Volumes/Data$W/fl (apfs, local, nodev, nosuid, mounted by someone)" \
		"/dev/disk9s4 on $W/tree/sub (apfs, local, nodev, nosuid, mounted by someone)" \
		"/dev/disk9s5 on $W/stack (apfs, local, journaled)" \
		"/dev/disk9s6 on $W/stack (apfs, local, nodev, nosuid, mounted by someone)" \
		"/dev/disk9s7 on $W/tree2/at on it (apfs, local, nodev, nosuid, mounted by someone)" \
		"/dev/disk9s8 on /System/Volumes/Data$W/tree3/sub (apfs, local, nodev, nosuid, mounted by someone)" \
		"/dev/disk9s9 on $W/pbin/cc (apfs, local, nodev, nosuid, mounted by someone)" \
		"/dev/disk9s10 on $W/sib (old) (apfs, local, nodev, nosuid, mounted by someone)" \
		"x on y on $W/dn (macfuse, nodev, nosuid, synchronous, mounted by someone)" \
		"x on on $W/dno (macfuse, nodev, nosuid, synchronous, mounted by someone)" \
		"u@h:/x on on $W/xt/sub (nfs, nodev, nosuid)" \
		"/dev/disk9s11 on $ABC (apfs, local, nodev, nosuid, mounted by someone)" \
		"/dev/disk9s12 on $W/fm/prog (apfs, local, nodev, nosuid, mounted by someone)" \
		"/dev/disk9s13 on $W/hm/x on $W/sys (apfs, local, nodev, nosuid, mounted by someone)" \
		"/dev/disk9s14 on $W/hm/y on $W/obin/cc (apfs, local, nodev, nosuid, mounted by someone)" \
		"/dev/disk9s15 on $W/hm/z on /usr/bin (apfs, local, nodev, nosuid, mounted by someone)" \
		"$W/net/g on $W/xg/gone (macfuse, nodev, nosuid, mounted by someone)" \
		"/dev/disk9s16 on $W/lf (apfs, local, journaled)" \
		"/dev/disk9s19 on $W/lt/sub (apfs, local, journaled)" \
		"/dev/disk9s20 on $W/hm/q (apfs, local, journaled)" "/dev/disk9s20 on $W/u" \
		"f (apfs, local, nodev, nosuid, mounted by someone)" \
		"/dev/disk9s21 on $W/fs (apfs, local, journaled)" \
		"x on $W/fs (macfuse, nodev, nosuid, mounted by someone)" \
		"/dev/disk1 x on $W/xj (macfuse, nodev, nosuid, mounted by someone)"
	/sbin/mount
}
no_table() { macos_mounts() { return 1; }; }"#;

/// Linux's for that walk: the fixture, read by the real reader.
const FOREIGN_LINUX: &str = r#"linux_mounts() { kernel_linux_mounts "$W/mountinfo"; }
no_table() { linux_mounts() { return 1; }; }"#;

/// A walk proves nothing through a filesystem whose owners and modes do not say who may
/// change it: on Linux a network or FUSE one (but for the one / is on), on macOS a volume that
/// is not local or that an account mounted. A way into one by any spelling, a tree or a bin
/// folder with one in it, a way of more than 40 links through one, a mount table unread when
/// it is needed, and a folder swapped by the time the walk ends each refuse. Only what the walk
/// crosses is looked at: on macOS no mount's folder is stat'd, and on Linux none outside the
/// tree, since one whose server stopped answering would hang the look. On macOS a volume the
/// walk crosses passes only from a disk that a local mount no account made names.
#[test]
fn under_root_a_walk_through_a_filesystem_whose_owners_say_nothing_refuses() {
    let scratch = Scratch::new();
    let mut lib = Lib::new();
    let table = if cfg!(target_os = "macos") {
        FOREIGN_MACOS
    } else {
        FOREIGN_LINUX
    };
    lib.step(format!(
        "{FOREIGN_FIND}\n{FAKE_ROOT}\n{FOREIGN_MOUNTS}\n{table}"
    ));
    // (what is walked, what it names)
    let mut rows = vec![
        ("folder_failing sealed_entry \"$W/vol/d\"", "$W/vol"),
        ("folder_failing sealed_entry \"$W/net/d\"", "$W/net"),
        ("folder_failing sealed_entry \"$W/ok/d\"", ""),
        ("folder_failing sealed_entry \"$W/vol\"", "$W/vol"),
        ("folder_failing sealed_entry \"$ABC/d\"", "$ABC"),
        ("folder_failing sealed_entry \"/$W/vol/d\"", "$W/vol"),
        (
            "folder_failing sealed_entry \"/System/Volumes/Data$W/vol/d\"",
            if cfg!(target_os = "macos") {
                "/System/Volumes/Data$W/vol"
            } else {
                "/System/Volumes/Data$W/vol/d"
            },
        ),
        ("root_entries_offender \"$W/tree\"", "$W/tree/sub"),
        ("root_entries_offender \"/$W/tree\"", "$W/tree/sub"),
        // A mount at the tree itself is the end of the walk to it.
        ("root_tree_failing \"$W/vol\"", "$W/vol"),
        ("root_entries_offender \"$W/ok\"", ""),
        // A local mount in a tree.
        ("root_entries_offender \"$W/lt\"", ""),
        ("no_table; root_entries_offender \"$W/ok\"", "$W/ok"),
        ("foreign_on_way \"$W/ok\" \"$W/gone\"", "$W/gone"),
        // A folder whose own folder the walk did not pass is looked at too.
        ("foreign_on_way \"$W/vol\"", "$W/vol"),
        // A folder root takes programs from by name, with one mounted on a
        // file in it (Linux mounts on a file too), by any spelling.
        ("root_bin_dir \"$W/pbin\" || echo refused", "refused"),
        ("root_bin_dir \"/$W/pbin\" || echo refused", "refused"),
        ("root_bin_dir \"$W/obin\" || echo refused", ""),
        // A way of more than 40 links leads the kernel nowhere, but through a
        // mount its account can swap it for one that leads somewhere.
        ("root_bin_dir \"$W/cbin\" || echo refused", "refused"),
        ("root_bin_dir \"$W/ocbin\" || echo refused", ""),
        ("may_run \"$W/fm/prog\" || echo refused", "refused"),
        ("may_run \"$W/fm/other\" || echo refused", ""),
        // The folder judged is another by the first look after the walk.
        (
            "SWAP=swap; folder_failing sealed_entry \"$W/swap/d\"",
            "$W/swap",
        ),
        // macOS's /dev/fd fails a stat now and then under load: then the
        // descriptors' own stat decides, alike or not.
        (
            "dev_fd_alike() { : >\"$W/asked\"; return 1; }; folder_failing sealed_entry \"$W/ok/d\"; \
             [ -e \"$W/asked\" ] || echo unasked",
            "",
        ),
        (
            "dev_fd_alike() { return 1; }; SWAP=swap2; folder_failing sealed_entry \"$W/swap2/d\"",
            "$W/swap2",
        ),
        // The reader of Linux's table, on its text.
        (
            "kernel_linux_mounts \"$W/mountinfo\" | grep -Fxc \"$ABC\"",
            "1",
        ),
        (
            "kernel_linux_mounts \"$W/mountinfo\" | grep -Fxc \"$W/vol\"",
            "1",
        ),
        (
            "kernel_linux_mounts \"$W/mountinfo\" | grep -Fxc \"$W/ok\"",
            "0",
        ),
        (
            "kernel_linux_mounts \"$W/mountinfo\" | grep -Fxc \"$W/rb\"",
            "0",
        ),
        ("kernel_linux_mounts \"$W/mountinfo\" | grep -Fxc /", "0"),
        ("kernel_linux_mounts \"$W/empty\" || echo unread", "unread"),
    ];
    if cfg!(target_os = "linux") {
        rows.extend([
            ("folder_failing sealed_entry \"$W/bad/d\"", "$W/bad"),
            // The filesystem / is on, FUSE here, and another mount of its device.
            ("folder_failing sealed_entry \"$W/rb/d\"", ""),
            // A type the reader does not know, which stat reads as a disk's.
            ("folder_failing sealed_entry \"$W/loc/d\"", ""),
            // A walk asks statfs of each folder on it and reads no table.
            (
                "linux_mounts() { : >\"$W/read\"; return 1; }; folder_failing sealed_entry \"$W/vol/d\"; \
                 [ ! -e \"$W/read\" ] || echo read",
                "$W/vol",
            ),
            ("no_table; folder_failing sealed_entry \"$W/ok/d\"", ""),
            // A mount's folder is placed by its text: a link that leads into
            // the tree from outside it is not the tree.
            ("root_entries_offender \"$W/tt\"", ""),
            // A tree's check asks statfs of no mount outside it.
            (
                ": >\"$W/typed\"; eval \"typed_$(declare -f folder_types)\"; \
                 folder_types() { printf '%s\\n' \"$@\" >>\"$W/typed\"; typed_folder_types \"$@\"; }; \
                 root_entries_offender \"$W/tree\" >/dev/null; grep -Fxc \"$W/net\" \"$W/typed\"",
                "0",
            ),
        ]);
    } else {
        rows.extend([
            ("folder_failing sealed_entry \"$W/fl/d\"", "$W/fl"),
            ("folder_failing sealed_entry \"$W/stack/d\"", "$W/stack"),
            ("folder_failing sealed_entry \"$W/sib/d\"", ""),
            ("folder_failing sealed_entry \"$W/dn/d\"", "$W/dn"),
            // A device name that ends in ` on`.
            ("folder_failing sealed_entry \"$W/dno/d\"", "$W/dno"),
            ("root_entries_offender \"$W/xt\"", "$W/xt/sub"),
            // A mount in a tree that root may not stat.
            ("root_entries_offender \"$W/xg\"", "$W/xg/gone"),
            ("root_entries_offender \"$W/tree2\"", "$W/tree2/at on it"),
            (
                "root_entries_offender \"$W/tree3\"",
                "/System/Volumes/Data$W/tree3/sub",
            ),
            // A folder only a mount's name spells is no volume.
            ("folder_failing sealed_entry \"$W/sys/d\"", ""),
            ("folder_failing sealed_entry /usr/bin", ""),
            // A walk that crosses no volume reads no table.
            (
                "macos_mounts() { : >\"$W/read\"; }; folder_failing sealed_entry \"$W/obin\"; \
                 [ ! -e \"$W/read\" ] || echo read",
                "",
            ),
            ("no_table; folder_failing sealed_entry \"$W/ok/d\"", "$W/ok"),
            // A table cut off is not read, whatever it printed first.
            (
                "eval \"base_$(declare -f macos_mounts)\"; macos_mounts() { base_macos_mounts; return 124; }; \
                 folder_failing sealed_entry \"$W/ok/d\"",
                "$W/ok",
            ),
            (
                "eval \"base_$(declare -f macos_mounts)\"; macos_mounts() { base_macos_mounts; return 124; }; \
                 root_entries_offender \"$W/xt\"",
                "$W/xt",
            ),
            // A local volume passes, and a graft shows the device of the
            // volume it is from; a volume no line names, one put over a local
            // one, and an account's disk a forged local line names too each
            // refuse.
            ("folder_failing sealed_entry \"$W/lf/d\"", ""),
            ("folder_failing sealed_entry \"$W/gr/d\"", ""),
            ("folder_failing sealed_entry \"$W/un/d\"", "$W/un"),
            ("folder_failing sealed_entry \"$W/fs/d\"", "$W/fs"),
            ("folder_failing sealed_entry \"$W/uf/d\"", "$W/uf"),
            // A walk across a volume stats no mount's folder and no line's
            // source that is no disk's.
            (
                ": >\"$W/ids\"; eval \"logged_$(declare -f folder_ids)\"; \
                 folder_ids() { printf '%s\\n' \"$@\" >>\"$W/ids\"; logged_folder_ids \"$@\"; }; \
                 eval \"logged_$(declare -f disk_devices)\"; \
                 disk_devices() { printf '%s\\n' \"$@\" >>\"$W/ids\"; logged_disk_devices \"$@\"; }; \
                 folder_failing sealed_entry \"$W/lf/d\" >/dev/null; \
                 grep -Fxc -e \"$W/net\" -e \"$W/net/g\" -e \"$W/vol\" -e \"$W/fs\" \"$W/ids\"",
                "0",
            ),
            // A disk whose node cannot be read refuses a volume, with no
            // word from the shell, and a line that only starts like a node's
            // is no disk's.
            (
                "eval \"base_$(declare -f macos_mounts)\"; macos_mounts() { base_macos_mounts; \
                 echo \"/dev/disk9s99 on $W/gone2 (apfs, local, journaled)\"; }; \
                 folder_failing sealed_entry \"$W/lf/d\" 2>&1",
                "$W/lf",
            ),
            // The kernel's own devfs at /dev passes, but not one a line says
            // an account mounted there.
            ("sealed_end /dev/null", "/dev/null"),
            (
                "eval \"base_$(declare -f macos_mounts)\"; macos_mounts() { base_macos_mounts; \
                 echo \"devfs on /dev (devfs, local, nobrowse, mounted by someone)\"; }; \
                 sealed_end /dev/null || echo refused",
                "refused",
            ),
            (
                "eval \"base_$(declare -f folder_ids)\"; \
                 folder_ids() { [ \"$1\" != /dev ] || return 1; base_folder_ids \"$@\"; }; \
                 sealed_end /dev/null 2>&1 || echo refused",
                "refused",
            ),
            // A link in a tree on a volume reads no table.
            (
                "macos_mounts() { : >\"$W/read\"; }; sealed_end \"$W/lf/t/l\" \"$W/lf/t\" >/dev/null || \
                 echo refused; [ ! -e \"$W/read\" ] || echo read",
                "",
            ),
            // APFS's names ignore case; on a volume whose names do not, the
            // variant is not there to walk.
            (
                "if [ -d \"$TMPDIR/W\" ]; then folder_failing sealed_entry \"$TMPDIR/W/vol/d\"; \
                 else echo \"$TMPDIR/W/vol\"; fi",
                "$TMPDIR/W/vol",
            ),
        ]);
    }
    let calls: Vec<usize> = rows
        .iter()
        .map(|(walk, named)| {
            lib.call(
                format!("( o=\"$({walk})\"; [ \"$o\" = \"{named}\" ] || {{ echo \"named [$o]\"; exit 1; }} )"),
                &[],
            )
        })
        .collect();
    let recs = lib.run(&scratch);
    for ((walk, named), call) in rows.iter().zip(calls) {
        assert_eq!(
            recs[call].rc, 0,
            "{walk} names [{named}]: {}{}",
            recs[call].out, recs[call].err
        );
    }
}

/// macOS's mount table is what a run of `/sbin/mount` printed, with its status.
/// A run cut off ends the read at the cut-off even while something it started
/// outside its process group (as a program in a file system's call that does
/// not heed SIGKILL lives on) still holds what it printed to.
#[cfg(target_os = "macos")]
#[test]
fn a_mount_table_read_cut_off_ends_at_the_cut_off() {
    let scratch = Scratch::new();
    let mut lib = Lib::new();
    // The fake sleep advances the reader's clock only once the escaped holder is ready, so a real
    // minute in `bounded` never races the outer one-minute hang detector. The holder acknowledges
    // the release written after the read returns, then SIGKILLs itself; it expires after 120
    // seconds without one.
    lib.step("VERIFY_WAIT_SECS=60; LINE='/dev/disk1s1 on / (apfs, local, journaled)'");
    let rows = [
        (
            "mount_table() { echo \"$LINE\"; }; macos_mounts",
            0,
            "/dev/disk1s1 on / (apfs, local, journaled)",
        ),
        (
            "mount_table() { echo \"$LINE\"; return 3; }; macos_mounts >/dev/null",
            3,
            "",
        ),
        (
            r#"(
                mount_table() {
                    echo "$LINE"
                    /usr/bin/perl -e '
                        use POSIX;
                        POSIX::setsid() >= 0 or die "setsid: $!";
                        sub mark {
                            my ($path, $pid) = @_;
                            open(my $f, ">", $path) or die "mark: $!";
                            print $f $pid;
                            close($f) or die "close: $!";
                        }
                        $SIG{ALRM} = sub { mark($ARGV[3], $$); kill 9, $$; die "self kill"; };
                        alarm 120;
                        mark($ARGV[0], $$);
                        until (-e $ARGV[1]) { select(undef, undef, undef, 0.02); }
                        mark($ARGV[2], $$);
                        kill 9, $$;
                        die "self kill";
                    ' "$TMPDIR/holder" "$TMPDIR/release" "$TMPDIR/released" "$TMPDIR/expired" &
                    /bin/sleep 120
                }
                t="$(
                    sleep() {
                        if [ -s "$TMPDIR/holder" ]; then
                            SECONDS=$((SECONDS + VERIFY_WAIT_SECS))
                        else
                            /bin/sleep "$@"
                        fi
                    }
                    macos_mounts 2>/dev/null
                )"; s=$?
                holder_deadline=$((SECONDS + 60))
                until [ -s "$TMPDIR/holder" ]; do
                    [ "$SECONDS" -lt "$holder_deadline" ] || { echo 'no holder' >&2; exit 1; }
                    sleep 0.1
                done
                holder="$(cat "$TMPDIR/holder")"
                case "$holder" in ''|*[!0-9]*) echo 'invalid holder' >&2; exit 1 ;; esac
                : >"$TMPDIR/release"
                holder_deadline=$((SECONDS + 60))
                until [ -s "$TMPDIR/released" ] || [ -s "$TMPDIR/expired" ]; do
                    [ "$SECONDS" -lt "$holder_deadline" ] || { echo 'no release acknowledgment' >&2; exit 1; }
                    sleep 0.1
                done
                [ ! -s "$TMPDIR/expired" ] && [ "$(cat "$TMPDIR/released")" = "$holder" ] || {
                    echo 'holder expired before the reader released it' >&2; exit 1;
                }
                holder_deadline=$((SECONDS + 60))
                while kill -0 "$holder" 2>/dev/null; do
                    [ "$SECONDS" -lt "$holder_deadline" ] || { echo 'holder still present' >&2; exit 1; }
                    sleep 0.1
                done
                printf '%s' "$s"
            )"#,
            0,
            "124",
        ),
    ];
    let calls: Vec<usize> = rows.iter().map(|(code, ..)| lib.call(*code, &[])).collect();
    let recs = lib.run(&scratch);
    for ((code, rc, out), call) in rows.iter().zip(calls) {
        assert_eq!(
            (recs[call].rc, recs[call].out.as_str()),
            (*rc, *out),
            "{code}: {}",
            recs[call].err
        );
    }
}

/// statfs's network and FUSE types are the ones the updater's rule names.
#[test]
fn a_root_walks_remote_filesystem_types_are_the_updaters() {
    use aterm_uds::shared_write::{REMOTE_FILESYSTEMS, remote_filesystem};
    let scratch = Scratch::new();
    let mut lib = Lib::new();
    let magics: Vec<u32> = REMOTE_FILESYSTEMS
        .iter()
        .copied()
        .chain([
            0xEF53,
            0x0102_1994,
            0x9123_683E,
            0x5846_5342,
            0x794C_7630,
            0x9FA0,
            0,
        ])
        .collect();
    let calls: Vec<usize> = magics
        .iter()
        .map(|magic| lib.call(format!("remote_filesystem_type {magic:x}"), &[]))
        .collect();
    let unread: Vec<usize> = ["''", "0x6969", "6969x"]
        .iter()
        .map(|t| lib.call(format!("remote_filesystem_type {t}"), &[]))
        .collect();
    let recs = lib.run(&scratch);
    for (magic, call) in magics.iter().zip(calls) {
        assert_eq!(
            recs[call].rc == 0,
            remote_filesystem(*magic),
            "{magic:#x}: {}",
            recs[call].err
        );
    }
    for call in unread {
        assert_eq!(
            recs[call].rc, 0,
            "a type stat did not read counts as remote"
        );
    }
}

/// The developer folder and SDK a root build pins, or what its refusal names
/// and the remedy it gives, each relative to the row's folder.
type Pinned = Result<(&'static str, &'static str), (&'static str, &'static str)>;

/// On macOS a root build pins the developer folder and SDK it judged, and
/// refuses one another account can change, naming the remedy by where the
/// folder came from. Linux runs no developer folder, but runs the same
/// judgment here, under the bash it has (5.x, where an unset variable stops
/// the run even inside a condition), with getfacl for the ACL.
#[test]
fn under_root_cc_runs_only_from_a_developer_folder_and_sdk_root_alone_can_change() {
    let scratch = Scratch::new();
    let mut lib = Lib::new();
    // What an environment says root judged is never taken for it.
    lib.env("ROOT_DEVELOPER_DIR", "/nonexistent");
    lib.env("ROOT_SDKROOT", "/nonexistent");
    // acl_on's ACL grants no write, which on Linux would show in the group bits
    // (its mask) for the mode walk to find before the ACL walk.
    lib.step(format!(
        "{FOREIGN_FIND}\n{FAKE_ROOT}\n{ROOT_LAYOUT}\n\
         picks() {{ unset DEVELOPER_DIR; \
         xcode_dirs() {{ printf '%s\\n' \"$W/select\" \"$W/xcode\" \"$W/clt\"; }}; }}\n\
         acl_on() {{ if [[ \"$OSTYPE\" == darwin* ]]; then chmod +a 'everyone allow add_file' \"$1\"; \
         else setfacl -m u:nobody:r-x \"$1\"; fi; }}"
    ));
    let (developer, sdk_root, select) = ("unset DEVELOPER_DIR", "unset SDKROOT", "xcode-select -s");
    // (label, setup, what the build pins or the refusal names)
    let rows: [(&str, &str, Pinned); 17] = [
        (
            "root's",
            "export TOOLCHAINS=x",
            Ok(("dev", "dev/SDKs/MacOSX.sdk")),
        ),
        (
            "a program of theirs in it",
            "theirs \"$W/dev/usr/bin/clang\"",
            Err(("dev/usr/bin/clang", developer)),
        ),
        (
            "a link of theirs to a program of root's inside it",
            "ln -s clang \"$W/dev/usr/bin/cc\"; theirs \"$W/dev/usr/bin/cc\"",
            if cfg!(target_os = "macos") {
                Err(("dev/usr/bin/cc", developer))
            } else {
                Ok(("dev", "dev/SDKs/MacOSX.sdk"))
            },
        ),
        (
            "an ACL in it",
            "acl_on \"$W/dev/usr/bin\"",
            Err(("dev", developer)),
        ),
        (
            "a folder above it their group can write",
            "mkdir \"$W/g\" && mv \"$W/dev\" \"$W/g/\" && chgrp \"$(command id -g)\" \"$W/g\" && \
             chmod g+w \"$W/g\" && DEVELOPER_DIR=\"$W/g/dev\"",
            Err(("g", developer)),
        ),
        (
            "an SDK of theirs",
            "mkdir \"$W/sdk\" && theirs \"$W/sdk\" && export SDKROOT=\"$W/sdk\"",
            Err(("sdk", sdk_root)),
        ),
        (
            "an SDK of root's holding a file of theirs",
            "mkdir \"$W/sdk\" && : >\"$W/sdk/lib\" && theirs \"$W/sdk/lib\" && export SDKROOT=\"$W/sdk\"",
            Err(("sdk/lib", sdk_root)),
        ),
        (
            "an SDK of root's outside it",
            "mkdir \"$W/sdk\" && export SDKROOT=\"$W/sdk\"",
            Ok(("dev", "sdk")),
        ),
        (
            "no SDK",
            "rm -r \"$W/dev/SDKs\"",
            Err((
                "dev/Platforms/MacOSX.platform/Developer/SDKs/MacOSX.sdk",
                developer,
            )),
        ),
        (
            "an Xcode.app",
            "mkdir -p \"$W/x/Xcode.app/Contents/Developer/Platforms/MacOSX.platform/Developer/SDKs/MacOSX.sdk\" && \
             DEVELOPER_DIR=\"$W/x/Xcode.app\"",
            Ok((
                "x/Xcode.app/Contents/Developer",
                "x/Xcode.app/Contents/Developer/Platforms/MacOSX.platform/Developer/SDKs/MacOSX.sdk",
            )),
        ),
        (
            "a library of theirs beside an Xcode.app's developer folder",
            "mkdir -p \"$W/x/Xcode.app/Contents/Developer/SDKs/MacOSX.sdk\" \
             \"$W/x/Xcode.app/Contents/SharedFrameworks\" && \
             : >\"$W/x/Xcode.app/Contents/SharedFrameworks/lib\" && \
             theirs \"$W/x/Xcode.app/Contents/SharedFrameworks/lib\" && DEVELOPER_DIR=\"$W/x/Xcode.app\"",
            Err(("x/Xcode.app/Contents/SharedFrameworks/lib", developer)),
        ),
        (
            "a library of theirs beside a developer folder in a bundle not named .app",
            "mkdir -p \"$W/opt/xc16/Contents/Developer/SDKs/MacOSX.sdk\" \
             \"$W/opt/xc16/Contents/SharedFrameworks\" \"$W/x\" && \
             : >\"$W/opt/xc16/Contents/SharedFrameworks/lib\" && \
             theirs \"$W/opt/xc16/Contents/SharedFrameworks/lib\" && \
             ln -s \"$W/opt/xc16\" \"$W/x/Xcode16.app\" && DEVELOPER_DIR=\"$W/x/Xcode16.app\"",
            Err(("opt/xc16/Contents/SharedFrameworks/lib", developer)),
        ),
        (
            "the one xcode-select chose",
            "picks; ln -s \"$W/dev\" \"$W/select\"",
            Ok(("dev", "dev/SDKs/MacOSX.sdk")),
        ),
        (
            "the one xcode-select chose, by a way through their folder",
            "picks; mkdir \"$W/other\" && ln -s \"$W/dev\" \"$W/other/dev\" && theirs \"$W/other\" && \
             ln -s \"$W/other/dev\" \"$W/select\"",
            Err(("other", select)),
        ),
        (
            "with none chosen, Xcode's",
            "picks; mv \"$W/dev\" \"$W/xcode\"",
            Ok(("xcode", "xcode/SDKs/MacOSX.sdk")),
        ),
        (
            "with none chosen and no Xcode, the Command Line Tools'",
            "picks; mv \"$W/dev\" \"$W/clt\"",
            Ok(("clt", "clt/SDKs/MacOSX.sdk")),
        ),
        (
            "with none at all",
            "picks; rm -r \"$W/dev\"",
            Err(("clt", select)),
        ),
    ];
    let macos = cfg!(target_os = "macos");
    // On macOS the pre-flight judges and the build pins; elsewhere the judgment
    // runs alone.
    let run = if macos {
        "source_build_preflight || { echo \"$CARGO_RESOLVE_REASON\"; exit 1; }; \
         cargo_build_pinned \"$ROOT\" && cat \"$TMPDIR/build-dev\""
    } else {
        "root_xcode_judged || { echo \"$CARGO_RESOLVE_REASON\"; exit 1; }; \
         echo \"$ROOT_DEVELOPER_DIR $ROOT_SDKROOT\""
    };
    let calls: Vec<usize> = rows
        .iter()
        .enumerate()
        .map(|(n, (_, setup, _))| lib.call(format!("( mk row{n}; {setup}\n {run} )"), &[]))
        .collect();
    let by_name = lib.call(
        "( mk by-name; export SDKROOT=macosx; ! root_xcode_judged && \
         [[ \"$CARGO_RESOLVE_REASON\" == *SDKROOT=macosx* ]] )",
        &[],
    );
    // A build the pre-flight did not judge judges the folder itself.
    let unjudged = macos.then(|| {
        lib.call(
            "( mk unjudged; theirs \"$W/dev/usr/bin/clang\"; rm -f \"$TMPDIR/build-dev\"; \
             ! cargo_build_pinned \"$ROOT\" && [ ! -e \"$TMPDIR/build-dev\" ] )",
            &[],
        )
    });
    let git = macos.then(|| {
        lib.call(
            format!(
                "( mk git; mkdir \"$W/root-bin\" && \
                 printf '#!/bin/sh\\necho \"$DEVELOPER_DIR $SDKROOT ${{TOOLCHAINS-unset}}\"\\n' >\"$W/root-bin/git\" && \
                 chmod 755 \"$W/root-bin/git\" && export PATH=\"$W/root-bin:{}\" TOOLCHAINS=x && checkout_git )",
                system_path()
            ),
            &[],
        )
    });
    let recs = lib.run(&scratch);
    let tmp = scratch.join("tmp");
    for (n, ((label, _, want), call)) in rows.iter().zip(calls).enumerate() {
        let row = tmp.join(format!("row{n}"));
        let w = s(&row);
        match want {
            Ok((dev, sdk)) => {
                assert_eq!(
                    recs[call].rc, 0,
                    "{label}: {}{}",
                    recs[call].out, recs[call].err
                );
                let toolchains = if macos { " unset" } else { "" };
                assert_eq!(
                    recs[call].out.trim_end(),
                    format!("{w}/{dev} {w}/{sdk}{toolchains}"),
                    "{label}: the build pins what root judged"
                );
            }
            Err((named, remedy)) => {
                assert_eq!(recs[call].rc, 1, "{label}: {}", recs[call].err);
                assert!(
                    recs[call].out.starts_with(&format!(
                        "root runs no compiler another account can change, and {w}/{named} is such "
                    )) && recs[call].out.contains(remedy),
                    "{label}: {}",
                    recs[call].out
                );
            }
        }
    }
    assert_eq!(
        recs[by_name].rc, 0,
        "an SDK named, not its folder, is refused in its own words: {}",
        recs[by_name].err
    );
    if let (Some(unjudged), Some(git)) = (unjudged, git) {
        assert_eq!(recs[unjudged].rc, 0, "{}", recs[unjudged].err);
        assert_eq!(recs[git].rc, 0, "{}", recs[git].err);
        let git_dir = tmp.join("git");
        let w = s(&git_dir);
        assert_eq!(
            recs[git].out,
            format!("{w}/dev {w}/dev/SDKs/MacOSX.sdk unset"),
            "root's git runs in the developer folder it judged"
        );
    }
}

#[test]
fn under_root_temporary_folders_go_in_tmp_whatever_tmpdir_says() {
    let scratch = Scratch::new();
    let mut lib = Lib::new();
    let root = lib.call(
        "( T=\"$TMPDIR\"; installer_uid() { echo 0; }; root_metadata_path && \
         [[ \"$T\" != /tmp && \"$TMPDIR\" == /tmp && \"$(env | grep '^TMPDIR=')\" == TMPDIR=/tmp ]] )",
        &[],
    );
    let account = lib.call(
        "( T=\"$TMPDIR\"; installer_uid() { echo 424242; }; root_metadata_path && [[ \"$TMPDIR\" == \"$T\" ]] )",
        &[],
    );
    let recs = lib.run(&scratch);
    assert_eq!(
        recs[root].rc, 0,
        "root's temporary folders go in /tmp, for root and what it runs: {}",
        recs[root].err
    );
    assert_eq!(
        recs[account].rc, 0,
        "the account keeps its own TMPDIR: {}",
        recs[account].err
    );
}

/// A Linux root run without getfacl can read no ACL, so each file and folder it checks
/// fails as one another account could change; it says so once, naming where it looked,
/// and goes on, since the signed binary's verifier reads ACLs itself. A getfacl that runs
/// is taken, one that does not is passed over, and a run as the account or on macOS says
/// nothing.
#[test]
fn under_root_on_linux_a_missing_getfacl_is_noted_once_and_stops_nothing() {
    let scratch = Scratch::new();
    let mut lib = Lib::new();
    // Root's TMPDIR becomes /tmp, so the file's name is fixed before the run.
    lib.step("PLAIN=\"$TMPDIR/plain\"; : >\"$PLAIN\"");
    let rows = [
        (
            "none that runs",
            "0",
            "Linux",
            "\"$PLAIN\" /nonexistent/getfacl",
            true,
        ),
        ("one that runs", "0", "Linux", "\"$PLAIN\" /bin/sh", false),
        (
            "the account",
            "424242",
            "Linux",
            "/nonexistent/getfacl",
            false,
        ),
        ("macOS", "0", "Darwin", "/nonexistent/getfacl", false),
    ];
    let calls: Vec<usize> = rows
        .iter()
        .map(|(_, uid, os, tools, _)| {
            lib.call(
                format!(
                    "( installer_uid() {{ echo {uid}; }}; uname() {{ echo {os}; }}; \
                     linux_acl_tools() {{ printf '%s\\n' {tools}; }}; root_metadata_path )"
                ),
                &[],
            )
        })
        .collect();
    let recs = lib.run(&scratch);
    let plain = s(&scratch.join("tmp/plain")).to_string();
    for ((label, _, _, _, noted), i) in rows.iter().zip(calls) {
        assert_eq!(recs[i].rc, 0, "{label}: the run goes on: {}", recs[i].err);
        let notes: Vec<&str> = recs[i]
            .err
            .lines()
            .filter(|l| l.contains("getfacl"))
            .collect();
        if *noted {
            assert_eq!(notes.len(), 1, "{label}: {}", recs[i].err);
            assert!(
                notes[0].starts_with("install.sh: NOTE: ")
                    && notes[0].contains(&format!("({plain} /nonexistent/getfacl)")),
                "{label}: a note naming where root looked: {}",
                notes[0]
            );
        } else {
            assert!(notes.is_empty(), "{label}: {}", recs[i].err);
        }
    }
}

#[test]
fn under_root_a_build_keeps_the_os_folders_root_runs_its_own_tools_from() {
    let scratch = Scratch::new();
    let mut lib = Lib::new();
    // The host's own folders as they are: macOS's sealed /usr/bin and /usr/sbin
    // hold _uucp's cu and uucico, and a build without them has no cc or git.
    let host = lib.call(
        "( installer_uid() { echo 0; }; ROOT_LOOKUP_PATH=\"$ROOT_SYSTEM_PATH\"; root_build_path )",
        &[],
    );
    lib.step(format!(
        "{FOREIGN_FIND}\n{FAKE_ROOT}\nFOREIGN_LIST=\"$TMPDIR/foreign\"; : >\"$FOREIGN_LIST\"\n\
         umask 022; W=\"$TMPDIR/w\"; mkdir -p \"$W/os\" \"$W/o\" && for d in os o; do \
         printf '#!/bin/sh\\n' >\"$W/$d/cu\" && chmod 755 \"$W/$d/cu\"; theirs \"$W/$d\"; done"
    ));
    let unlisted = lib.call("root_bin_dir \"$W/os\"", &[]);
    let listed = lib.call(
        "( ROOT_SYSTEM_PATH=\"$W/os:$ROOT_SYSTEM_PATH\"; root_bin_dir \"$W/os\" && root_bin_dir \"$W/os//\" && \
         ! root_bin_dir \"$W/o\" && ROOT_LOOKUP_PATH=\"$W/o:$W/os/:$W/os\" root_build_path )",
        &["W"],
    );
    let recs = lib.run(&scratch);
    assert_eq!(recs[host].rc, 0, "{}", recs[host].err);
    let fixed: Vec<&str> = ["/usr/bin", "/bin", "/usr/sbin", "/sbin"]
        .into_iter()
        .filter(|d| std::path::Path::new(d).is_dir())
        .collect();
    let kept: Vec<&str> = recs[host].out.split(':').collect();
    assert!(
        fixed.iter().all(|d| kept.contains(d)),
        "a root build keeps each OS folder this host has ({fixed:?}), whatever system account's \
         files it holds: {}",
        recs[host].out
    );
    assert_eq!(
        recs[unlisted].rc, 1,
        "a folder another account owns is refused"
    );
    assert_eq!(recs[listed].rc, 0, "{}", recs[listed].err);
    let w = recs[listed].var("W");
    assert_eq!(
        recs[listed].out,
        format!("{w}/os/:{w}/os"),
        "the same folder is root's word once it is an OS folder, however its slashes are spelled, \
         and a folder whose name only begins one is not"
    );
}

#[test]
fn root_build_authority_includes_acls_and_the_folders_program_names_pass_through() {
    let scratch = Scratch::new();
    let mut lib = Lib::new();
    lib.step(format!("{FOREIGN_FIND}\n{FAKE_ROOT}\n{ROOT_LAYOUT}"));
    // Only listings are injected: the private tree and the production preflight,
    // executable lookup and PATH filtering remain real. BSD's @ can hide +;
    // Linux needs a successful ACL query, not mode-only ls's missing +.
    lib.step(
        r#"eval "$(declare -f ls_entry | sed '1s/ls_entry/base_ls_entry/')"
uname() { if [ "$1" = -s ]; then echo "$PLATFORM"; else command uname "$@"; fi; }
# Each platform's mount table, one local filesystem at /, and a disk's type
# for each folder statfs reads on Linux.
macos_mounts() { echo '/dev/disk1s1 on / (apfs, local, journaled)'; }
linux_mounts() { :; }
folder_types() { local p; for p; do echo ef53; done; }
ls_entry() {
	local p="${!#}" arg flags="" within=0 listing
	local -a args=()
	for arg; do
		case "$arg" in
		--*) args+=("$arg") ;;
		-*) flags+="$arg"; args+=("${arg//e/}") ;;
		*) args+=("$arg") ;;
		esac
	done
	[[ "$p" != "${ACL_AT:-}" && "$p" != "${ACL_UNREADABLE:-}" ]] || within=1
	if [[ "$flags" != *d* ]]; then
		case "${ACL_AT:-}" in "$p"/*) within=1 ;; esac
		case "${ACL_UNREADABLE:-}" in "$p"/*) within=1 ;; esac
	fi
	if [[ "$flags" == *l* && "$within" == 1 && -n "${ACL_UNREADABLE:-}" ]]; then return 1; fi
	listing="$(base_ls_entry "${args[@]}")" || return
	if [[ "$within" == 1 && -n "${ACL_AT:-}" ]]; then
		if [[ "$PLATFORM" == Darwin ]]; then
			printf '%s\n' "$listing"
			[[ "$flags" != *e* ]] || echo ' 0: user:other allow write,append,add_file,writesecurity'
		else
			printf '%s\n' "$listing" | awk '/^[-bcdlps]/ && !marked { $1 = $1 "+"; marked = 1 } { print }'
		fi
	else
		printf '%s\n' "$listing"
	fi
}
linux_acl_query() {
	local p="${!#}" within=0
	[[ "$p" == / ]] || p="${p%/}"
	[[ -z "${ACL_QUERY_UNAVAILABLE:-}" ]] || return 1
	[[ "$p" != "${ACL_AT:-}" && "$p" != "${ACL_UNREADABLE:-}" ]] || within=1
	case "${ACL_AT:-}" in "$p"/*) within=1 ;; esac
	case "${ACL_UNREADABLE:-}" in "$p"/*) within=1 ;; esac
	[[ "$within" != 1 || -z "${ACL_UNREADABLE:-}" ]] || return 1
	printf '%s\n' user::rwx group::r-x other::r-x
	[[ -z "${ACL_DEFAULT:-}" ]] || echo 'default:user::rwx'
	[[ "$within" != 1 || -z "${ACL_AT:-}" ]] || echo 'user:424242:rwx'
}"#,
    );
    let rows = [
        ("plain", "", true),
        ("source-acl", "ACL_AT=\"$W/co/src/main.rs\"", false),
        ("ancestor-acl", "ACL_AT=\"$W\"", false),
        ("config-acl", "ACL_AT=\"$W/.cargo/config.toml\"", false),
        (
            "cargo-acl",
            ": >\"$W/home/.cargo/config.toml\"; ACL_AT=\"$W/home/.cargo/config.toml\"",
            false,
        ),
        ("tool-acl", "ACL_AT=\"$W/store/trust/1/bin/targo\"", false),
        (
            "unreadable-tree-acl",
            "ACL_UNREADABLE=\"$W/co/src/main.rs\"",
            false,
        ),
        (
            "unreadable-tool-acl",
            "ACL_UNREADABLE=\"$W/store/trust/1/bin/targo\"",
            false,
        ),
    ];
    let mut checks = Vec::new();
    for platform in ["Darwin", "Linux"] {
        for (label, setup, builds) in rows {
            let call = lib.call(
                format!(
                    "( mk {platform}-{label}; PLATFORM={platform}; unset ACL_AT ACL_UNREADABLE; \
                     {setup}\n source_build_preflight )"
                ),
                &[],
            );
            checks.push((format!("{platform} {label}"), call, builds));
        }
    }
    // These controls do not race: the folder's authority alone is enough to
    // refuse it before another account can populate or replace a program.
    let path = lib.call(
        format!(
            r#"( mk path; PLATFORM=Darwin; unset ACL_AT ACL_UNREADABLE
mkdir -p "$W/unsafe" "$W/safe" "$W/alias-bin"
chgrp "$(command id -g)" "$W/unsafe"
chmod 775 "$W/unsafe"
printf '#!/bin/sh\n' >"$W/unsafe/cc"
chmod 755 "$W/unsafe/cc"
ln -s "$W/unsafe/cc" "$W/alias-bin/cc"
! root_bin_dir "$W/unsafe" && ! root_bin_dir "$W/alias-bin" &&
! may_run "$W/unsafe/cc" && ! may_run "$W/alias-bin/cc" &&
root_bin_dir "$W/safe" &&
PATH="$W/unsafe:$W/alias-bin:$W/safe:{}" root_build_path )"#,
            system_path()
        ),
        &[],
    );
    let unavailable = lib.call(
        "( mk unknown-acl; PLATFORM=Linux; unset ACL_AT ACL_UNREADABLE; \
         ACL_QUERY_UNAVAILABLE=1; ! source_build_preflight )",
        &[],
    );
    let default_acl = lib.call(
        "( mk default-acl; PLATFORM=Linux; unset ACL_AT ACL_UNREADABLE; \
         ACL_DEFAULT=1; ! source_build_preflight )",
        &[],
    );
    let identity = lib.call(
        r#"( . "$1"
id() { : >"$TMPDIR/id-ran"; echo 424242; }
[[ "$(installer_uid)" == "$EUID" && ! -e "$TMPDIR/id-ran" ]] )"#,
        &[],
    );
    let metadata = lib.call(
        r#"( mk metadata; PLATFORM=Darwin; unset ACL_AT ACL_UNREADABLE
installer_uid() { echo 0; }
unset -f id uname
mkdir -p "$W/inherited-bin"
for tool in id awk; do
printf '#!/bin/sh\necho touched >>"$TMPDIR/metadata-ran"\necho 424242\n' >"$W/inherited-bin/$tool"
chmod 755 "$W/inherited-bin/$tool"
done
cd "$W"
original="inherited-bin:$PATH"
PATH="$original"
root_metadata_path && [[ "$ROOT_LOOKUP_PATH" == "$original" ]] &&
found="$(lookup_program id)" && [[ "$found" == "$W/inherited-bin/id" ]] &&
may_run "$found" && cd "$W/co" && [[ "$found" -ef "$W/inherited-bin/id" ]] &&
id -u >/dev/null && awk 'BEGIN { exit 0 }' && [[ ! -e "$TMPDIR/metadata-ran" ]] )"#,
        &[],
    );
    let functions = lib.call(
        format!(
            r#"( mk functions; mkdir -p "$W/empty"; ln -s empty "$W/link"
uname() {{ : >"$TMPDIR/function-ran"; }}
find() {{ : >"$TMPDIR/function-ran"; }}
awk() {{ : >"$TMPDIR/function-ran"; }}
readlink() {{ : >"$TMPDIR/function-ran"; }}
discard_inherited_functions
PATH="{}"
[[ "$(builtin type -t uname)" == file && "$(builtin type -t find)" == file &&
   "$(builtin type -t awk)" == file && "$(builtin type -t readlink)" == file &&
   "$(builtin type -t discard_inherited_functions)" == function ]] &&
uname -s >/dev/null && find "$W/empty" -type f && awk 'BEGIN {{ exit 0 }}' &&
[[ "$(readlink "$W/link")" == empty && ! -e "$TMPDIR/function-ran" ]] )"#,
            system_path()
        ),
        &[],
    );
    let relative_home = lib.call(
        r#"( mk relative-home; PLATFORM=Darwin; unset ACL_AT ACL_UNREADABLE
printf '#!/bin/sh\nexit 99\n' >"$W/home/.cargo/bin/rustup"
cp "$W/home/.cargo/bin/rustup" "$W/home/.cargo/bin/cargo"
chmod 755 "$W/home/.cargo/bin/"*
cd "$W"
CARGO_HOME=../home/.cargo
find_cargo_and_rustup "$ROOT" && [[ "$CARGO_LANE" == rustup &&
    "$CARGO_HOME" == "$ROOT/../home/.cargo" &&
    "$RUSTUP_BIN" == "$CARGO_HOME/bin/rustup" && "$CARGO_BIN" == "$CARGO_HOME/bin/cargo" ]] &&
cd "$ROOT" && [[ -f "$CARGO_BIN" && -f "$RUSTUP_BIN" ]] )"#,
        &[],
    );
    let recs = lib.run(&scratch);
    for (label, call, builds) in checks {
        assert_eq!(
            recs[call].rc,
            i32::from(!builds),
            "{label}: {}{}",
            recs[call].out,
            recs[call].err
        );
    }
    assert_eq!(recs[path].rc, 0, "{}", recs[path].err);
    for (name, call) in [
        ("unknown ACL capability", unavailable),
        ("default ACL authority", default_acl),
        ("readonly process identity", identity),
        ("trusted metadata PATH with original lookup", metadata),
        ("inherited command functions", functions),
        ("relative cargo-home discovery and execution", relative_home),
    ] {
        assert_eq!(recs[call].rc, 0, "{name}: {}", recs[call].err);
    }
    assert_ne!(gid(&scratch.join("tmp/path/unsafe")), 0);
    assert_eq!(
        recs[path].out,
        format!("{}:{}", s(&scratch.join("tmp/path/safe")), system_path()),
        "PATH keeps the sealed folder, excluding both writable ancestry and its file alias"
    );
}

#[test]
fn a_source_build_asks_only_the_folders_new_downloads_land_in() {
    let scratch = Scratch::new();
    let tmp = scratch.join("tmp");
    let mut lib = Lib::new();
    lib.step(format!(
        "{FOREIGN_FIND}\numask 022\n\
         mk() {{ W=\"$TMPDIR/$1\"; FOREIGN_LIST=\"$TMPDIR/$1.foreign\"; : >\"$FOREIGN_LIST\"; \
         ROOT=\"$W/co\" CARGO_HOME=\"$W/cargo\"; c=\"$W/cargo\"; \
         mkdir -p \"$ROOT/target/release\" \"$c/registry/src/idx/cfg-if-1.0.0/src\" \"$c/registry/cache/idx\" \
         \"$c/registry/index/idx/.cache/cf\" \"$c/git/checkouts/x-1/abc/src\" \"$c/git/db/x-1\" && \
         : >\"$c/registry/src/idx/cfg-if-1.0.0/src/lib.rs\" && : >\"$c/registry/cache/idx/cfg-if-1.0.0.crate\" && \
         : >\"$c/registry/index/idx/.cache/cf/cfg-if\" && : >\"$c/.package-cache\" && \
         : >\"$c/.package-cache-mutate\" && : >\"$c/.global-cache\" && \
         printf 'source = \"git+https://example.invalid/x#0\"\\n' >\"$ROOT/Cargo.lock\"; }}"
    ));
    let user = lib.call("id -un", &[]);
    let rows: [(&str, &str, bool); 7] = [
        (
            "crate, index and lock files of root's",
            "theirs \"$c/registry/src/idx/cfg-if-1.0.0/src/lib.rs\" \"$c/registry/cache/idx/cfg-if-1.0.0.crate\" \
             \"$c/registry/index/idx/.cache\" \"$c/.package-cache\" \"$c/.package-cache-mutate\" \"$c/.global-cache\"",
            true,
        ),
        (
            "an extracted crate's folder",
            "theirs \"$c/registry/src/idx/cfg-if-1.0.0\"",
            true,
        ),
        (
            "a per-index folder of root's",
            "theirs \"$c/registry/src/idx\"",
            false,
        ),
        ("registry/index", "theirs \"$c/registry/index\"", false),
        (
            "a git checkout's folder",
            "theirs \"$c/git/checkouts/x-1\"",
            false,
        ),
        (
            "a folder deep in a git checkout",
            "theirs \"$c/git/checkouts/x-1/abc/src\"",
            true,
        ),
        (
            "a git checkout's folder, with no git source in the lock",
            "printf 'source = \"registry+https://example.invalid/index\"\\n' >\"$ROOT/Cargo.lock\"; \
             theirs \"$c/git/checkouts/x-1\"",
            true,
        ),
    ];
    let calls: Vec<usize> = rows
        .iter()
        .enumerate()
        .map(|(n, (_, setup, _))| {
            lib.call(
                format!(
                    "( mk row{n}; {setup}; source_build_preflight; rc=$?; \
                     echo \"${{CARGO_RESOLVE_REASON:-}}\"; exit $rc )"
                ),
                &[],
            )
        })
        .collect();
    let recs = lib.run(&scratch);
    let user = recs[user].out.trim();
    for (n, ((label, _, builds), i)) in rows.iter().zip(calls).enumerate() {
        let rec = &recs[i];
        if *builds {
            assert_eq!(rec.rc, 0, "{label}: {}", rec.out);
            continue;
        }
        assert_eq!(rec.rc, 1, "{label}: {}", rec.err);
        let cargo = tmp.join(format!("row{n}/cargo"));
        assert!(
            rec.out
                .contains(&format!("sudo chown -R {user} {}", s(&cargo))),
            "{label}: {}",
            rec.out
        );
    }
}

#[test]
fn under_root_an_aterm_root_does_not_ask_is_the_one_the_refusal_names() {
    let scratch = Scratch::new();
    let tmp = scratch.join("tmp");
    let mut lib = Lib::new();
    lib.step(format!(
        "{FOREIGN_FIND}\numask 022; FOREIGN_LIST=\"$TMPDIR/foreign\"; : >\"$FOREIGN_LIST\"\n\
         t=\"$TMPDIR/theirs\"; mkdir -p \"$TMPDIR/co\" \"$t/bin\" \"$t/pkg/store/trust/7/bin\" && \
         printf '[toolchain]\\nchannel = \"trust\"\\n' >\"$TMPDIR/co/rust-toolchain.toml\" && \
         printf '#!/bin/sh\\necho \"aterm $*\" >>\"$TMPDIR/ran\"\\necho \"targo → %s/targo — managed 7 — pinned by index 1\"\\n' \
         \"$t/pkg/store/trust/7/bin\" >\"$t/bin/aterm\" && \
         printf '#!/bin/sh\\necho \"targo $*\" >>\"$TMPDIR/ran\"\\n' >\"$t/pkg/store/trust/7/bin/targo\" && \
         chmod 755 \"$t/bin/aterm\" \"$t/pkg/store/trust/7/bin/targo\" && theirs \"$t\"\n\
         export PATH=\"$t/bin:{}\"; unset CARGO_HOME",
        system_path()
    ));
    let vars = ["CARGO_LANE", "CARGO_RESOLVE_REASON"];
    let as_account = lib.call("find_cargo_and_rustup \"$TMPDIR/co\"", &vars);
    let ran = lib.call("cat \"$TMPDIR/ran\" && rm \"$TMPDIR/ran\"", &[]);
    lib.step(FAKE_ROOT);
    let as_root = lib.call("find_cargo_and_rustup \"$TMPDIR/co\"", &vars);
    let remedy = lib.call("gate_trust_remedy", &[]);
    let ran_as_root = lib.call("[ ! -e \"$TMPDIR/ran\" ]", &[]);
    let recs = lib.run(&scratch);
    assert_eq!(recs[as_account].rc, 0, "{}", recs[as_account].var(vars[1]));
    assert_eq!(recs[as_account].var(vars[0]), "store");
    assert_eq!(recs[ran].out, "aterm pkg which targo");
    let rec = &recs[as_root];
    let reason = rec.var(vars[1]);
    assert_eq!(rec.rc, 1, "{reason}");
    let aterm = s(&tmp.join("theirs/bin/aterm")).to_string();
    assert!(
        reason.contains(&format!("and {aterm} is one")) && reason.contains("whose toolchain it is"),
        "{reason}"
    );
    for said in [reason, recs[remedy].out.as_str()] {
        assert!(!said.contains("aterm pkg install trust"), "{said}");
    }
    assert_eq!(
        recs[ran_as_root].rc,
        0,
        "root ran {}",
        read(&tmp.join("ran"))
    );
}

/// Under root, the copy goes into a folder made inside the staging folder once
/// that one is root's, 0700, empty and (on macOS) stripped of the ACL an
/// inheritable entry on DEST gave it. A staging folder an account entered
/// (an entry named with a newline), opened (mode 0777), took (another uid's)
/// or replaced with a link fails the staging, and nothing is left in DEST;
/// the strip leaves the folder a link leads to as it was.
#[test]
fn a_root_run_copies_the_app_where_only_root_can_reach_it() {
    let scratch = Scratch::new();
    let tmp = scratch.join("tmp");
    let mut lib = Lib::new();
    lib.step(
        "id() { if [ \"$1\" = -u ]; then echo 0; else command id \"$@\"; fi; }\n\
         log() { printf '%s\\n' \"$*\" >>\"$TMPDIR/log\"; }\n\
         ls_entry() { command ls \"$@\" | awk -v u=\"${STAGE_UID:-0}\" 'NR == 1 && NF >= 9 { $3 = u } { print }'; }\n\
         mode_of() {\n\
         \tif [ \"$(uname -s)\" = Darwin ]; then\n\
         \t\tcommand ls -led \"$1\" | awk 'NR == 1 { m = substr($1, 1, 10) } NR > 1 && /^ *[0-9]+: / { a = \"+\" } END { printf \"%s%s\", m, a }'\n\
         \telse\n\
         \t\tcommand ls -nd \"$1\" | awk '{ m = substr($1, 1, 10); if (substr($1, 11, 1) == \"+\") m = m \"+\"; printf \"%s\", m }'\n\
         \tfi\n\
         }\n\
         ditto() {\n\
         \tlog \"ditto $2\"\n\
         \tlog \"into: $(mode_of \"${2%/*}\")\"\n\
         \tfor e in \"$DEST\"/* \"$DEST\"/.[!.]*; do\n\
         \t\t[ -e \"$e\" ] && log \"in dest: $(mode_of \"$e\") ${e##*/}\"\n\
         \tdone\n\
         \tmkdir -p \"$2/Contents/MacOS\" && : >\"$2/Contents/MacOS/aterm\" && [ -z \"${DITTO_FAILS:-}\" ]\n\
         }\n\
         chown() { log \"chown $*\"; [ -z \"${CHOWN_FAILS:-}\" ]; }\n\
         mv() { log \"mv $*\"; command mv \"$@\"; }\n\
         mktemp() {\n\
         \tlocal d\n\
         \td=\"$(command mktemp \"$@\")\" || return\n\
         \t[ -z \"${ENTERED:-}\" ] || mkdir \"$d\"/$'\\n'\n\
         \t[ -z \"${OPENED:-}\" ] || command chmod 777 \"$d\"\n\
         \t[ -z \"${LINKED:-}\" ] || { command rmdir \"$d\" && ln -s \"$LINKED\" \"$d\"; } || return\n\
         \tprintf '%s\\n' \"$d\"\n\
         }\n\
         mkdir -p \"$TMPDIR/ok\" \"$TMPDIR/copy-fails\" \"$TMPDIR/chown-fails\" \"$TMPDIR/entered\" \\\n\
         \t\"$TMPDIR/opened\" \"$TMPDIR/taken\" \"$TMPDIR/linked\" \"$TMPDIR/theirs\"\n\
         [ \"$(uname -s)\" != Darwin ] || {\n\
         \t/bin/chmod +a \"user:nobody allow list,search,add_file,add_subdirectory,file_inherit,directory_inherit\" \"$TMPDIR/ok\" &&\n\
         \t\t/bin/chmod +a \"user:nobody allow list\" \"$TMPDIR/theirs\"\n\
         }",
    );
    let staged = lib.call(
        "DEST=\"$TMPDIR/ok\"; stage_bundle /nonexistent/aterm.app \"$DEST\" && swap_staged_bundle \"$DEST\" && \
         ls -A \"$DEST\"",
        &["STAGE", "STAGE_DIR"],
    );
    let log = lib.call("cat \"$TMPDIR/log\" && rm \"$TMPDIR/log\"", &[]);
    let failed = |name: &str, set: &str| {
        format!(
            "DEST=\"$TMPDIR/{name}\"; {set} stage_bundle /nonexistent/aterm.app \"$DEST\"; rc=$?; drop_stage; \
             ls -A \"$DEST\"; [ \"$rc\" -ne 0 ]"
        )
    };
    let copy_fails = lib.call(failed("copy-fails", "DITTO_FAILS=1"), &[]);
    let chown_fails = lib.call(failed("chown-fails", "CHOWN_FAILS=1"), &[]);
    let entered = lib.call(failed("entered", "ENTERED=1"), &[]);
    let opened = lib.call(failed("opened", "OPENED=1"), &[]);
    let taken = lib.call(failed("taken", "STAGE_UID=424242"), &[]);
    let linked = lib.call(failed("linked", "LINKED=\"$TMPDIR/theirs\""), &[]);
    let theirs = lib.call(
        "[ -d \"$TMPDIR/theirs\" ] && if [ \"$(uname -s)\" = Darwin ]; then \
         /bin/ls -led \"$TMPDIR/theirs\" | { grep -c '^ [0-9]*: ' || true; }; else echo 0; fi",
        &[],
    );
    let recs = lib.run(&scratch);
    assert_eq!(recs[staged].rc, 0, "{}", recs[staged].err);
    assert_eq!(
        recs[staged].out, "aterm.app",
        "the staging folder went with the swap"
    );
    assert_eq!(
        [recs[staged].var("STAGE"), recs[staged].var("STAGE_DIR")],
        ["", ""]
    );
    let log: Vec<&str> = recs[log].out.lines().collect();
    let dest = s(&tmp.join("ok")).to_string();
    let copied = log
        .first()
        .and_then(|l| l.strip_prefix("ditto "))
        .unwrap_or_else(|| panic!("{log:?}"));
    let (name, copy) = copied
        .strip_suffix("/aterm.app")
        .and_then(|folder| folder.strip_prefix(&format!("{dest}/")))
        .and_then(|rel| rel.split_once('/'))
        .unwrap_or_else(|| panic!("ditto copies into a folder of a folder in {dest}: {log:?}"));
    assert!(name.starts_with("aterm.app.installing."), "{log:?}");
    assert!(copy.starts_with("copy.") && !copy.contains('/'), "{log:?}");
    assert_eq!(
        log.get(1..),
        Some(
            &[
                "into: drwx------",
                format!("in dest: drwx------ {name}").as_str(),
                format!("chown -R 0:{} {copied}", gid(&tmp.join("ok"))).as_str(),
                format!("mv {copied} {dest}/aterm.app").as_str(),
            ][..]
        ),
        "while ditto copies, {dest} holds the root-only folder alone, it and the folder the copy \
         goes in with no ACL its inheritable entry would give them, and the copy is root's before \
         it leaves it"
    );
    for i in [copy_fails, chown_fails, entered, opened, taken, linked] {
        assert_eq!(recs[i].rc, 0, "a failed staging says so: {}", recs[i].err);
        assert_eq!(recs[i].out, "", "drop_stage leaves nothing behind");
    }
    assert_eq!(recs[theirs].rc, 0, "{}", recs[theirs].err);
    assert_eq!(
        recs[theirs].out.trim_end(),
        if cfg!(target_os = "macos") { "1" } else { "0" },
        "the strip never reaches through a link put in the staging folder's place"
    );
}

#[test]
fn an_interrupted_root_run_leaves_no_staging_folder_behind() {
    let scratch = Scratch::new();
    let mut lib = Lib::new();
    lib.step(
        "umask 022; id() { if [ \"$1\" = -u ]; then echo 0; else command id \"$@\"; fi; }\n\
         ls_entry() { command ls \"$@\" | awk 'NR == 1 && NF >= 9 { $3 = 0 } { print }'; }\n\
         ditto() { mkdir -p \"$2/Contents/MacOS\" && : >\"$2/Contents/MacOS/aterm\"; }\n\
         chown() { :; }\n\
         CONTAINER_KIND=zip\n\
         staged() { DEST=\"$TMPDIR/$1\"; TMP=\"$TMPDIR/$1.tmp\"; MNT=\"$TMP/mnt\"; OLD=\"\"; \
         mkdir -p \"$DEST\" \"$MNT/aterm.app\" && stage_bundle \"$MNT/aterm.app\" \"$DEST\" && ls -A \"$DEST\"; }",
    );
    // Interrupted after the copy, before the swap; and after the swap set the
    // installed bundle aside, before it renamed the copy in.
    let before_swap = lib.call(
        "staged before && echo -- && ( install_app_cleanup ); ls -A \"$DEST\"; [ ! -e \"$TMP\" ]",
        &[],
    );
    let mid_swap = lib.call(
        "mkdir -p \"$TMPDIR/mid/aterm.app\" && : >\"$TMPDIR/mid/aterm.app/installed\" && staged mid && \
         OLD=\"$DEST/.aterm.app.old.x\" && mv \"$DEST/aterm.app\" \"$OLD\" && echo -- && \
         ( install_app_cleanup ); ls -A \"$DEST\"; echo +; ls -A \"$DEST/aterm.app\"",
        &[],
    );
    // The set-aside bundle is back before the DMG's detach, the one step of the
    // cleanup a Ctrl-C can still cut.
    let back_first = lib.call(
        "mkdir -p \"$TMPDIR/dmg/aterm.app\" && staged dmg >/dev/null && OLD=\"$DEST/.aterm.app.old.x\" && \
         mv \"$DEST/aterm.app\" \"$OLD\" && ( CONTAINER_KIND=dmg; \
         hdiutil() { [ ! -d \"$DEST/aterm.app\" ] || echo back >\"$TMPDIR/at-detach\"; }; install_app_cleanup ); \
         cat \"$TMPDIR/at-detach\"",
        &[],
    );
    let recs = lib.run(&scratch);
    assert_eq!(
        (recs[back_first].rc, recs[back_first].out.as_str()),
        (0, "back"),
        "{}",
        recs[back_first].err
    );
    for (label, i, kept) in [
        ("before the swap", before_swap, &[][..]),
        ("mid-swap", mid_swap, &["aterm.app", "+", "installed"][..]),
    ] {
        let rec = &recs[i];
        assert_eq!(rec.rc, 0, "{label}: {}", rec.err);
        let (staged, left) = rec
            .out
            .split_once("--")
            .unwrap_or_else(|| panic!("{label}: {}", rec.out));
        assert!(
            staged
                .lines()
                .any(|e| e.starts_with("aterm.app.installing.")),
            "{label}: the run staged in a folder of its own: {staged}"
        );
        let left: Vec<&str> = left.trim_start_matches('\n').lines().collect();
        assert_eq!(
            left, kept,
            "{label}: the EXIT trap leaves no staging folder, and puts back what the swap set aside"
        );
    }
}

/// A Ctrl-C while install_app's EXIT trap cleans up never cuts it short: the
/// first one after a refusal, a second one in a run a Ctrl-C ended (which still
/// dies of it), and one that cuts a wedged DMG detach, which goes on to the
/// -force fallback and to the download's removal, which a Ctrl-C does not cut
/// either. Before, the run ended at the step the Ctrl-C killed and left
/// the staging copy, the set-aside old copy and the download behind. A
/// set-aside copy the cleanup could not put back stays, and it says where, and
/// so does an image no detach could release, which rm never walks into.
#[test]
fn a_ctrl_c_never_cuts_the_cleanup_short() {
    let scratch = Scratch::new();
    let mut lib = Lib::new();
    lib.env("INSTALL_SH", s(&install_sh()));
    lib.step(
        "mkdir -p \"$TMPDIR/bin\" && cat >\"$TMPDIR/bin/rm\" <<'EOF'
#!/bin/sh
for last; do :; done
[ \"$last\" != \"$CUT\" ] || { echo cut >>\"$TMPDIR/cut-$ROW\"; kill -s INT 0; }
exec /bin/rm \"$@\"
EOF
cat >\"$TMPDIR/bin/hdiutil\" <<'EOF'
#!/bin/sh
echo \"$3\" >>\"$TMPDIR/hdiutil-$ROW\"
[ \"$ROW\" != mounted ] || exit 1
[ \"$3\" = -force ] && exit 0
kill -s INT 0
exec /bin/sleep 30
EOF
chmod +x \"$TMPDIR/bin/rm\" \"$TMPDIR/bin/hdiutil\" && cat >\"$TMPDIR/cleanup.bash\" <<'EOF'
ATERM_INSTALL_LIBRARY_ONLY=1
. \"$INSTALL_SH\"
# A bracket in TMP, as a TMPDIR can have, which a glob would not match.
DEST=\"$TMPDIR/$1\" TMP=\"$TMPDIR/$1.[t]mp\" CONTAINER_KIND=zip STAGE_DIR=\"\"
MNT=\"$TMP/mnt\" STAGE=\"$DEST/aterm.app.installing.x\" OLD=\"$DEST/.aterm.app.old.x\"
mkdir -p \"$DEST/aterm.app\" \"$STAGE\" \"$OLD\" \"$MNT\"
# rm sends the Ctrl-C as it starts on CUT.
export ROW=$1 CUT=\"$STAGE\"
PATH=\"$TMPDIR/bin:$PATH\"
trap install_app_cleanup EXIT
case \"$1\" in
refusal) exit 1 ;;
second) kill -s INT 0 ;;
detach) CUT=\"$TMP\" CONTAINER_KIND=dmg; sleep() { :; }; exit 1 ;;
restore) CUT=\"\"; command rmdir \"$DEST/aterm.app\"; mv() { return 1; }; exit 1 ;;
mounted) CUT=\"\" CONTAINER_KIND=dmg; sleep() { :; }; mkdir \"$MNT/aterm.app\"; : >\"$TMP/aterm.dmg\"; exit 1 ;;
esac
EOF",
    );
    let tags = ["refusal", "second", "detach", "restore", "mounted"];
    let calls: Vec<usize> = tags
        .iter()
        .map(|tag| {
            lib.call(
                format!(
                    "echo \"$({})\" $(ls -A \"$TMPDIR/{tag}\") $([ ! -e \"$TMPDIR/{tag}.[t]mp\" ] || echo tmp) \
                     $(ls -A \"$TMPDIR/{tag}.[t]mp\" 2>/dev/null) $(cat \"$TMPDIR/hdiutil-{tag}\" \"$TMPDIR/cut-{tag}\" 2>/dev/null)",
                    group_run(&format!("\"$TMPDIR/cleanup.bash\" {tag}"))
                ),
                &[],
            )
        })
        .collect();
    let recs = lib.run(&scratch);
    // `cut`: the rm stand-in sent its Ctrl-C.
    for (tag, call, want) in [
        ("refusal", calls[0], "exit 1 aterm.app cut"),
        ("second", calls[1], "signal 2 aterm.app cut"),
        ("detach", calls[2], "exit 1 aterm.app -quiet -force cut"),
        ("restore", calls[3], "exit 1 .aterm.app.old.x"),
        (
            "mounted",
            calls[4],
            "exit 1 aterm.app tmp mnt -quiet -force",
        ),
    ] {
        assert_eq!(recs[call].out, want, "{tag}: {}", recs[call].err);
    }
    assert!(
        recs[calls[3]]
            .err
            .contains("could not put the previous aterm.app back"),
        "{}",
        recs[calls[3]].err
    );
    assert!(
        recs[calls[4]]
            .err
            .contains("the disk image is still mounted at"),
        "{}",
        recs[calls[4]].err
    );
}

fn gid(path: &std::path::Path) -> u32 {
    std::fs::metadata(path).expect("a folder").gid()
}

/// A bundle of this account's, in its own group (never gid 0, which
/// bundle_is_only lets write), with plain modes whatever the umask.
const BUNDLE: &str = "B=\"$TMPDIR/b/aterm.app\"; mkdir -p \"$B/Contents/MacOS\" && \
                      printf '#!/bin/sh\\n' >\"$B/Contents/MacOS/aterm\" && : >\"$B/Contents/Info.plist\" && \
                      chmod -R u=rwX,go=rX \"$TMPDIR/b\" && chmod 755 \"$B/Contents/MacOS/aterm\" && \
                      chgrp -R \"$(id -g)\" \"$TMPDIR/b\"; ME=\"$(id -u)\"";

#[test]
fn a_bundle_every_account_can_write_in_is_reinstalled() {
    let scratch = Scratch::new();
    let mut lib = Lib::new();
    lib.step(BUNDLE);
    let clean = lib.call("bundle_is_only \"$ME\" \"$B\"", &[]);
    let file = lib.call(
        "( chmod 646 \"$B/Contents/Info.plist\" && ! bundle_is_only \"$ME\" \"$B\"; rc=$?; \
         chmod 644 \"$B/Contents/Info.plist\"; exit $rc )",
        &[],
    );
    let folder = lib.call(
        "( chmod 757 \"$B/Contents/MacOS\" && ! bundle_is_only \"$ME\" \"$B\"; rc=$?; \
         chmod 755 \"$B/Contents/MacOS\"; exit $rc )",
        &[],
    );
    let sticky = lib.call(
        "( chmod 1777 \"$B/Contents\" && ! bundle_is_only \"$ME\" \"$B\"; rc=$?; \
         chmod 755 \"$B/Contents\"; exit $rc )",
        &[],
    );
    let recs = lib.run(&scratch);
    assert_ne!(
        gid(&scratch.join("tmp/b")),
        0,
        "the bundle's group is not root's"
    );
    for (label, i) in [
        ("plain", clean),
        ("a file every account can write", file),
        ("a folder every account can write", folder),
        ("a sticky one", sticky),
    ] {
        assert_eq!(recs[i].rc, 0, "{label}: {}", recs[i].err);
    }
}

#[cfg(target_os = "macos")]
#[test]
fn a_root_runs_bundle_keeps_no_acl_and_one_with_an_acl_is_reinstalled() {
    let scratch = Scratch::new();
    let mut lib = Lib::new();
    lib.step(BUNDLE);
    let aces = "/bin/ls -leRA \"$TMPDIR/dest/aterm.app\" | grep -c '^ [0-9]*: ' || true";
    let clean = lib.call("bundle_is_only \"$ME\" \"$B\"", &[]);
    let file = lib.call(
        "( chmod +a 'group:everyone allow write,append' \"$B/Contents/MacOS/aterm\" && \
         ! bundle_is_only \"$ME\" \"$B\"; rc=$?; chmod -N \"$B/Contents/MacOS/aterm\"; exit $rc )",
        &[],
    );
    let folder = lib.call(
        "( chmod +a 'group:everyone allow add_file' \"$B/Contents/MacOS\" && \
         ! bundle_is_only \"$ME\" \"$B\"; rc=$?; chmod -N \"$B/Contents/MacOS\"; exit $rc )",
        &[],
    );
    // What a destination's inheritable ACE gives every entry ditto makes in
    // it, and a link with an ACL of its own, as an archive can carry one.
    let inherited = lib.call(
        format!(
            "mkdir -p \"$TMPDIR/dest\" && \
             chmod +a 'group:everyone allow write,append,add_file,file_inherit,directory_inherit' \"$TMPDIR/dest\" && \
             ditto \"$B\" \"$TMPDIR/dest/aterm.app\" && ln -s MacOS/aterm \"$TMPDIR/dest/aterm.app/Contents/link\" && \
             chmod -h +a 'group:everyone allow write' \"$TMPDIR/dest/aterm.app/Contents/link\" && {aces}"
        ),
        &[],
    );
    let stripped = lib.call(
        format!(
            "chown() {{ :; }}; own_bundle_as_root \"$TMPDIR/dest/aterm.app\" \"$TMPDIR/dest\" && {aces}; \
             bundle_is_only \"$ME\" \"$TMPDIR/dest/aterm.app\""
        ),
        &[],
    );
    let recs = lib.run(&scratch);
    for (label, i) in [
        ("plain", clean),
        ("a file with an ACL", file),
        ("a folder with one", folder),
    ] {
        assert_eq!(recs[i].rc, 0, "{label}: {}", recs[i].err);
    }
    assert_eq!(recs[inherited].rc, 0, "{}", recs[inherited].err);
    let before: u32 = recs[inherited].out.trim().parse().expect("a count");
    assert!(before > 0, "the copy inherits the ACE");
    assert_eq!(recs[stripped].rc, 0, "{}", recs[stripped].err);
    assert_eq!(
        recs[stripped].out.trim(),
        "0",
        "own_bundle_as_root strips every ACL, a link's own among them"
    );
}
