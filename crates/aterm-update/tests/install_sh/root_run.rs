// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A root run over an account's home (macOS's sudo keeps HOME).

use std::os::unix::fs::MetadataExt as _;

use crate::support::{Lib, Scratch, install_sh, q, s, system_path};

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
    lib.step("ROOT=\"$TMPDIR/co\"; mkdir -p \"$ROOT/target/release\" \"$HOME/.cargo/registry\"");
    let user = lib.call("id -un", &[]);
    let clean = lib.call("source_build_preflight", &[]);
    let stale = lib.call(
        "( id() { if [ \"$1\" = -u ]; then echo 424242424; else command id \"$@\"; fi; }; \
         source_build_preflight ) 2>&1",
        &[],
    );
    let elsewhere = lib.call(
        "( id() { if [ \"$1\" = -u ]; then echo 424242424; else command id \"$@\"; fi; }; \
         ROOT=/nonexistent; CARGO_HOME=/usr/bin; source_build_preflight )",
        &[],
    );
    let as_root = lib.call(
        "( id() { if [ \"$1\" = -u ]; then echo 0; else command id \"$@\"; fi; }; source_build_preflight )",
        &[],
    );
    let recs = lib.run(&scratch);
    for i in [clean, elsewhere, as_root] {
        assert_eq!(recs[i].rc, 0, "call {i}: {}", recs[i].err);
    }
    let (target, cargo) = (
        s(&tmp.join("co/target")).to_string(),
        s(&home.join(".cargo")).to_string(),
    );
    assert_eq!(recs[stale].rc, 1);
    assert_eq!(
        recs[stale].out.trim_end(),
        format!(
            "install.sh: the source build would fail: not yours, so cargo cannot change what is in \
             {target} {cargo} (an install under sudo makes such folders root's) — give them back \
             with: sudo chown -R {} {target} {cargo}",
            recs[user].out.trim()
        )
    );
}
