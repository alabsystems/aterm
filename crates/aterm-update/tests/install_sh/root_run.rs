// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A root run over an account's home (macOS's sudo keeps HOME).

use std::os::unix::fs::MetadataExt as _;

use crate::support::{Lib, Scratch, install_sh, q, read, s, system_path};

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
    let recs = lib.run(&scratch);
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
/// target/, a `.cargo/config.toml` above it, cargo's home, and a store whose
/// targo writes the PATH it ran on to `$TMPDIR/build-path` and the git config
/// variables it was given to `$TMPDIR/build-git`.
const ROOT_LAYOUT: &str = r#"umask 022
mk() {
	W="$TMPDIR/$1"
	FOREIGN_LIST="$TMPDIR/$1.foreign"
	: >"$FOREIGN_LIST"
	mkdir -p "$W/co/src" "$W/co/target/release/deps" "$W/home/.cargo/registry/src/idx" \
		"$W/home/.cargo/bin" "$W/store/trust/1/bin" "$W/.cargo" "$W/their-bin"
	printf 'fn main() {}\n' >"$W/co/src/main.rs"
	: >"$W/co/target/release/deps/libx.rlib"
	printf '[build]\n' >"$W/.cargo/config.toml"
	printf '#!/bin/sh\necho "$PATH" >"$TMPDIR/build-path"\necho "${GIT_CONFIG_GLOBAL-unset} ${GIT_CONFIG_NOSYSTEM-unset}" >"$TMPDIR/build-git"\n' \
		>"$W/store/trust/1/bin/targo"
	printf '#!/bin/sh\n' >"$W/their-bin/sccache"
	cp "$W/their-bin/sccache" "$W/their-bin/rustc"
	chmod -R u=rwX,go=rX "$W" && chmod 755 "$W/store/trust/1/bin/targo" "$W/their-bin/"*
	theirs "$W/their-bin"
	ROOT="$W/co" HOME="$W/home" CARGO_LANE=store CARGO_BIN="$W/store/trust/1/bin/targo"
	unset CARGO_HOME RUSTC RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER
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
    let rows: [(&str, &str, Refusal); 21] = [
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
         ditto() {\n\
         \tlog \"ditto $2\"\n\
         \tlog \"into: $(command ls -nd \"${2%/*}\" | awk '{print $1}')\"\n\
         \tfor e in \"$DEST\"/* \"$DEST\"/.[!.]*; do\n\
         \t\t[ -e \"$e\" ] && log \"in dest: $(command ls -nd \"$e\" | awk '{print $1}') ${e##*/}\"\n\
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
    let recs = lib.run(&scratch);
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
