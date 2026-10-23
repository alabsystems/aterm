// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the rm-breaker resolver against approval shapes and negative controls.

use std::path::Path;

use super::*;

const CWD: &str = "/Users/_owner/aterm";
const HOME: &str = "/Users/_owner";
const TMP: &str = "/var/folders/ab/xyz/T";

fn roots(cwd: &str) -> Vec<ScratchRoot> {
    [
        ScratchRoot::dir(Path::new("/private/tmp/claude-502")),
        ScratchRoot::dir(Path::new(TMP)),
        ScratchRoot::children_of(Path::new("/tmp")),
        ScratchRoot::children_of(Path::new("/private/tmp")),
        ScratchRoot::glob_under(Path::new(cwd), "target*"),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn resolve_in(cwd: &str, cmd: &str) -> Result<Vec<String>, String> {
    let roots = roots(cwd);
    let scope = RmScope {
        cwd: Path::new(cwd),
        home: Some(Path::new(HOME)),
        roots: &roots,
        probe: &super::super::probe::LiveProbe,
    };
    resolve_rm_line(cmd, &scope)
}

fn resolve(cmd: &str) -> Result<Vec<String>, String> {
    resolve_in(CWD, cmd)
}

fn approves(cmd: &str) -> Vec<String> {
    resolve(cmd).unwrap_or_else(|e| panic!("{cmd:?} should resolve: {e}"))
}

fn escalates(cmd: &str) -> String {
    match resolve(cmd) {
        Ok(t) => panic!("{cmd:?} must escalate, resolved to {t:?}"),
        Err(e) => e,
    }
}

#[test]
fn literal_chains_resolve_and_every_operand_is_judged() {
    assert_eq!(
        approves(
            "S=/private/tmp/claude-502/a; T=$S/b; rm -rf \"$T/c\" \"${S}/d\" ${S:?}/e 2>/dev/null"
        ),
        [
            "/private/tmp/claude-502/a/b/c",
            "/private/tmp/claude-502/a/d",
            "/private/tmp/claude-502/a/e"
        ]
    );
    assert!(escalates("S=/private/tmp/claude-502/a; rm -rf \"$S/x\" /etc/y").contains("/etc/y"));
    approves("S=/tmp/work/a && rm -rf \"$S\" # tidy up; rm -rf /");
    approves("export S=/tmp/work/a; rm -r -- \"$S/x\"");
    approves("S=/tmp/work/a\nrm -rf \"$S/x\"");
    approves("ls /tmp/work; S=/tmp/work/a; rm -f $S/x.log >/dev/null 2>&1");
}

/// An assignment (or an `export`) counts only where it surely runs, and a
/// command after `|` runs whatever the one before returned, so
/// `test -n "$D" |` proves nothing.
#[test]
fn a_conditional_or_scoped_assignment_is_unknown() {
    for assign in ["S=/tmp/w/a", "export S=/tmp/w/a"] {
        for cmd in [
            format!("test -d x || {assign}; rm -rf \"$S/y\""),
            format!("true && {assign}; rm -rf \"$S/y\""),
            format!("{assign} | cat; rm -rf \"$S/y\""),
            format!("{assign} & rm -rf \"$S/y\""),
        ] {
            assert!(escalates(&cmd).contains("$S"), "{cmd}");
        }
        approves(&format!("{assign}; rm -rf \"$S/y\""));
    }
    assert!(escalates("S=/tmp/w/a rm -rf \"$S/y\"").contains("assignment on the rm"));
    assert!(escalates("S=/tmp/w/a ls; rm -rf \"$S/y\"").contains("$S"));
    for cmd in [
        "D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX); test -n \"$D\" | rm -rf \"$D/x\"",
        "D=$(mktemp -d); test -n \"$D\" | rm -rf \"$D/x\"",
    ] {
        assert!(escalates(cmd).contains("removes /x"), "{cmd}");
    }
    approves("D=$(mktemp -d); test -n \"$D\" && rm -rf \"$D/x\"");
}

#[test]
fn where_an_operand_may_point() {
    assert!(escalates("S=/private/tmp/claude-502; rm -rf \"$S\"").contains("not strictly inside"));
    assert!(
        escalates("S=/private/tmp/claude-502; rm -rf \"$S\"/*").contains("not strictly inside")
    );
    assert!(escalates("rm -rf /tmp/*/x").contains("not strictly inside"));
    assert!(escalates("rm -rf /tmp/w").contains("not strictly inside"));
    assert!(escalates("S=/tmp/w/a; rm -rf \"$S/../..\"").contains(".."));
    assert!(escalates("S=/tmp/w/a; rm -rf \"$S/.git\"").contains(".git"));
    assert!(escalates("S=/tmp/w/a; rm -rf $S/.*").contains("can match .."));
    assert!(escalates("S=$HOME/x; rm -rf \"$S/y\"").contains("$S"));
    assert!(escalates("rm -rf \"$HOME/x\"").contains("$HOME"));
    assert!(escalates("rm -rf ~/x").contains("~"));
    assert!(escalates("rm -rf /").contains("filesystem root"));
    assert!(escalates("rm -rf /Users/_owner").contains("home"));
    assert!(escalates("rm -rf /Users/_owner/*").contains("home"));
    assert!(escalates("rm -rf /USERS/someone").contains("/Users"));
    assert!(escalates("rm -rf /t*/x").contains("glob at root"));
    assert!(escalates("rm -rf /tmp/w/a/.GIT/x").contains(".git"));
    assert!(escalates("rm -rf --no-preserve-root /tmp/w/a").contains("--no-preserve-root"));
    assert!(escalates("rm -W /tmp/w/a").contains("does not know: -W"));
    approves("rm -rf /tmp/w/a/.gitignore");
    approves("rm -fv --one-file-system /tmp/w/a");
    let cwd = "/private/tmp/claude-502/w/sub";
    assert!(
        resolve_in(cwd, "rm -rf /private/tmp/claude-502/w/sub/.")
            .unwrap_err()
            .contains("cwd or an ancestor")
    );
    assert!(
        resolve_in(cwd, "S=/private/tmp/claude-502/w; rm -rf \"$S\"")
            .unwrap_err()
            .contains("cwd or an ancestor")
    );
    assert!(
        resolve_in(cwd, "rm -rf /private/tmp/claude-502/W/SUB")
            .unwrap_err()
            .contains("cwd or an ancestor")
    );
    assert_eq!(
        resolve_in(cwd, "rm -rf /private/tmp/claude-502/w/sub/x").unwrap(),
        ["/private/tmp/claude-502/w/sub/x"]
    );
    approves("S=/tmp/w/a; rm -rf \"$S\"/*.o");
    assert_eq!(
        approves("rm -rf /Users/_owner/aterm/target/debug/x"),
        ["/Users/_owner/aterm/target/debug/x"]
    );
    approves("rm -rf /Users/_owner/aterm/target.noindex/x");
    assert!(escalates("rm -rf /Users/_owner/aterm/target").contains("not strictly inside"));
    assert!(escalates("rm -rf /Users/_owner/aterm/src/x").contains("not strictly inside"));
}

/// The Bash tool keeps its own working directory, which the worker moves between calls, so a
/// relative operand and `$PWD` escalate.
#[test]
fn a_relative_operand_and_pwd_escalate() {
    assert!(escalates("rm -rf target/debug/x").contains("relative rm operand"));
    assert!(escalates("rm -rf ./target/x").contains("relative rm operand"));
    assert!(escalates("rm -rf \"$PWD/target/x\"").contains("$PWD"));
    assert!(escalates("S=$PWD/target; rm -rf \"$S/x\"").contains("$S"));
    assert!(escalates("rm -rf .").contains("relative rm operand"));
    approves("rm -rf /Users/_owner/aterm/target/x");
}

/// The default macOS volume is case-insensitive (`Rm` runs `/bin/rm`), so an
/// rm head in any letter case is judged.
#[test]
fn an_rm_head_is_an_rm_in_any_case() {
    assert!(escalates("Rm -rf /etc/x").contains("/etc/x"));
    assert!(escalates("/BIN/RM -rf /etc/x").contains("/etc/x"));
    assert!(escalates("ls | xargs RM").contains("rm this resolver cannot see"));
    assert_eq!(approves("RM -rf /tmp/w/a/x"), approves("rm -rf /tmp/w/a/x"));
}

/// The constructs a straight-line reading cannot follow.
#[test]
fn what_the_resolver_will_not_follow() {
    let cases = [
        ("rm -rf \"$1\"", "positional"),
        ("set -- /tmp/w/a; rm -rf \"$1\"", "`set`"),
        ("S=/tmp/w/a; eval rm -rf \"$S\"", "`eval`"),
        ("S=/tmp/w/a; ls | xargs rm -rf", "cannot see run"),
        ("S=/tmp/w/a; sh -c 'rm -rf $S'", "cannot see run"),
        ("S=/tmp/w/a; sudo rm -rf \"$S/x\"", "cannot see run"),
        ("S=/tmp/w/a; echo $(rm -rf \"$S\")", "a quote inside"),
        ("S=/tmp/w/a; (rm -rf \"$S/x\")", "subshell"),
        ("S=/tmp/w/a; { rm -rf \"$S/x\"; }", "compound"),
        ("S=`echo /tmp/w/a`; rm -rf \"$S/x\"", "backtick"),
        ("S=/tmp/w/a; rm -rf \"${S:-/}\"", "does not follow"),
        ("S=$'/tmp/w/a'; rm -rf \"$S/x\"", "$'"),
        ("S=/tmp/w/a; read S; rm -rf \"$S/x\"", "`read`"),
        (
            "S=/tmp/w/a; printf -v S /; rm -rf \"$S/x\"",
            "$S is not known after `printf`",
        ),
        ("S=/tmp/w/a; rm -rf \"$S/x\" > log.txt", "redirect"),
        ("S=/tmp/w/a; rm -rf \"$S/x\" <<EOF\nEOF", "here-document"),
        (
            "S=/tmp/w/a; rm --no-preserve-root -rf \"$S/x\"",
            "no-preserve-root",
        ),
        ("S=/tmp/w/a; rm -W \"$S/x\"", "flag"),
        ("S=\"/tmp/w/a b\"; rm -rf $S/x", "split or globbed"),
        ("S=/tmp/w/a*; rm -rf $S/x", "split or globbed"),
        ("F=-rf; S=/tmp/w/a; rm $F \"$S\"", "expands to a flag"),
        ("S=/tmp/w/a; $CMD -rf \"$S\"", "named by an expansion"),
        ("S=/tmp/w/a; rm -rf", "no operand"),
        ("ls", "no rm"),
        ("rm -rf /tmp/w/{a,b}", "brace"),
        ("S=/tmp/w/a; rm -rf \"$S/x", "unterminated"),
    ];
    for (cmd, want) in cases {
        let e = escalates(cmd);
        assert!(e.contains(want), "{cmd:?}: {e:?} does not mention {want:?}");
    }
}

#[test]
fn scratch_root_patterns() {
    let r = ScratchRoot::glob_under(Path::new("/w/proj"), "target*").expect("root");
    let comps = |p: &str| abs_components(p).expect("absolute");
    assert!(r.holds(&comps("/w/proj/target")));
    assert!(r.holds(&comps("/w/proj/target.noindex/x")));
    assert!(!r.holds(&comps("/w/proj/src")));
    assert!(ScratchRoot::dir(Path::new("relative")).is_none());
    assert!(ScratchRoot::dir(Path::new("/a/../b")).is_none());
}

fn all_escalate(cases: &[(&str, &str)]) {
    for (cmd, want) in cases {
        let e = escalates(cmd);
        assert!(e.contains(want), "{cmd:?}: {e:?} does not mention {want:?}");
    }
}

/// Assert that `S=<scratch>/w; <middle>; rm -rf "$S/x"` keeps `S` across each middle command.
fn keeps(middles: &[&str]) {
    for middle in middles {
        let cmd = format!("S=/private/tmp/claude-502/w; {middle}; rm -rf \"$S/x\"");
        assert_eq!(
            resolve(&cmd),
            Ok(vec!["/private/tmp/claude-502/w/x".to_string()]),
            "{cmd}"
        );
    }
}

/// Assert that `S=<scratch>/w; <middle>; rm -rf "$S/x"` forgets `S` after each middle command.
fn forgets(middles: &[&str]) {
    for middle in middles {
        let cmd = format!("S=/private/tmp/claude-502/w; {middle}; rm -rf \"$S/x\"");
        assert!(escalates(&cmd).contains("$S is not known after"), "{cmd}");
    }
}

/// A failed `$(mktemp -d …)` leaves its variable empty, so every operand built
/// on one is judged as the line runs when it fails, unless the rm cannot run
/// with it empty.
#[test]
fn a_mktemp_that_fails_leaves_its_variable_empty() {
    all_escalate(&[
        (
            "D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX); rm -rf $D/etc",
            "removes /etc",
        ),
        (
            "D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX); rm -rf \"$D/x\"",
            "removes /x",
        ),
        (
            "T=$(mktemp -d /private/tmp/claude-502/w.XXXXXX); rm -rf $T/bin/y",
            "removes /bin/y",
        ),
        (
            "D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX); rm -rf \"$D/bin\" \"$D/usr\"",
            "removes /bin",
        ),
        (
            "D=$(mktemp -d /private/tmp/claude-502/w.XXXX); rm -rf \"$D/sub\"",
            "removes /sub",
        ),
        (
            "D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX); E=$D/etc; rm -rf \"$E\"",
            "removes /etc",
        ),
        (
            "S=/tmp/scratch; D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX); rm -rf \"$S/$D\"",
            "removes /tmp/scratch/",
        ),
        (
            "D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX); rm -rf \"$D\"/*",
            "glob behind a $(mktemp)",
        ),
        (
            "D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX) E=$(true) && rm -rf \"$D/x\"",
            "removes /x",
        ),
        (
            "D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX) || true; rm -rf \"$D/x\"",
            "removes /x",
        ),
        (
            "D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX) && true || rm -rf \"$D/x\"",
            "removes /x",
        ),
        (
            "D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX); true && rm -rf \"$D/x\"",
            "removes /x",
        ),
        (
            "export D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX) && rm -rf \"$D/x\"",
            "removes /x",
        ),
        (
            "D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX); rm -rf \"${D?}/x\"",
            "removes /x",
        ),
        (
            "D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX); E=$D/etc; rm -rf \"${E:?}\"",
            "removes /etc",
        ),
        (
            "D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX)/x; E=${D:?}/y; rm -rf \"$E\"",
            "removes /x/y",
        ),
        (
            "D=/tmp/w/a; E=${D:?} D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX); rm -rf \"$D/etc\"",
            "removes /etc",
        ),
        (
            "D=/tmp/w/a; D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX) : \"${D:?}\"; rm -rf \"$D/etc\"",
            "is not assigned a literal",
        ),
        (
            "D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX); true && : \"${D:?}\"; rm -rf \"$D/x\"",
            "removes /x",
        ),
        (
            "D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX); : \"${D:?}\" | cat; rm -rf \"$D/x\"",
            "removes /x",
        ),
        (
            "D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX) || exit 1 & rm -rf \"$D/x\"",
            "is not assigned a literal",
        ),
        (
            "D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX) || exit 1 && true; rm -rf \"$D/x\"",
            "removes /x",
        ),
        (
            "D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX); true || test -n \"$D\" || exit; rm -rf \"$D/x\"",
            "removes /x",
        ),
        (
            "D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX); test -n $D && rm -rf \"$D/x\"",
            "removes /x",
        ),
        (
            "D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX); [ -n \"$D/x\" ] && rm -rf \"$D/x\"",
            "removes /x",
        ),
    ]);
    assert_eq!(
        approves("D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX) && rm -rf \"$D/x\" \"$D\"/*.o"),
        [
            "/private/tmp/claude-502/<mktemp>/x",
            "/private/tmp/claude-502/<mktemp>/*.o"
        ]
    );
    approves("S=/tmp/scratch; D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX) && rm -rf \"$S/$D\"");
    approves("D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX) && rm -rf \"$D/x\" | cat");
    approves("D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX); rm -rf \"${D:?}/etc\"");
    approves("D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX); rm -rf \"$D/etc\" \"${D:?}\"");
    approves("D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX); E=${D:?}/etc; rm -rf \"$E\"");
    approves("D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX) E=${D:?}; rm -rf \"$D/etc\"");
    approves("D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX); : \"${D:?}\"; rm -rf \"$D/etc\"");
    approves("D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX) || exit 1; rm -rf \"$D/etc\"");
    approves("D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX) || exit; rm -rf \"$D/etc\"");
    approves(
        "D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX); test -n \"$D\" && rm -rf \"$D/etc\"",
    );
    approves("D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX); [ -d \"$D\" ] && rm -rf \"$D/etc\"");
    approves("D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX); rm -rf $D \"$D\"");
}

/// zsh computes what the lexer reads as text: `$=V`, `$~V`, `$^V`, `$+V`, a modifier or
/// subscript on a bare name (`"$S:h:h"`, `$S[1,0]`), a non-ASCII name, and `$[…]`.
#[test]
fn zsh_expansions_are_not_text() {
    for flag in ["=", "~", "^", "+"] {
        let cmd = format!("S=/tmp/ok; T='y /etc'; rm -rf /tmp/ok/${flag}T");
        assert!(escalates(&cmd).contains("zsh computes"), "{cmd}");
        let quoted = format!("S=/tmp/ok; rm -rf \"$S/${flag}T\"");
        assert!(escalates(&quoted).contains("zsh computes"), "{quoted}");
    }
    all_escalate(&[
        ("S=/tmp/w/a; rm -rf \"$S:h:h\"", "`$S:h`"),
        ("S=/tmp/w/a; rm -rf $S:h", "`$S:h`"),
        ("S=/tmp/w/a; rm -rf \"$S:A/x\"", "`$S:A`"),
        ("S=/tmp/w/a; rm -rf \"$S:t\"", "`$S:t`"),
        ("S=/tmp/w/a; rm -rf \"/tmp/w/b/$S:&\"", "`$S:&`"),
        ("S=/tmp/w/a/b/etc; rm -rf $S[1,0]/etc", "zsh subscript"),
        (
            "S=/tmp/w/a; A=x; echo $A[S=0]; rm -rf \"$S/x\"",
            "zsh subscript",
        ),
        ("é=/../../../etc; rm -rf /tmp/w/a$é", "`$é`"),
        ("S=/tmp/ok; rm -rf $S/a/$[S=0]; rm -rf $S/x", "arithmetic"),
        ("S=/tmp/ok; T=$=S; rm -rf \"$T/x\"", "$T is not assigned"),
    ]);
    assert_eq!(approves("S=/tmp/ok; rm -rf $S/a$.b"), ["/tmp/ok/a$.b"]);
    assert_eq!(approves("S=/tmp/ok; rm -rf \"$S/a$\""), ["/tmp/ok/a$"]);
    assert_eq!(approves("S=/tmp/w/a; rm -rf \"${S}:h\""), ["/tmp/w/a:h"]);
    #[cfg(unix)]
    assert_eq!(
        approves("S=/tmp/w/a; rm -rf \"$S: x\" $S:/c \"$S:1\""),
        ["/tmp/w/a: x", "/tmp/w/a:/c", "/tmp/w/a:1"]
    );
    approves("S=/tmp/w/a; echo \"$S: cleaning\"; rm -rf \"$S\"/*");
    approves("S=/tmp/w/a; rm -rf \"${S}/x\" $S/y");
}

/// A command the resolver does not model may assign a variable it names (zsh's
/// `print -v S /etc`, `zparseopts a:=S`), so that variable is unknown after it, and every
/// variable is after a word whose text is unknown.
#[test]
fn a_variable_another_command_names_is_unknown_after_it() {
    forgets(&[
        "print -rv S /etc",
        "N=S; print -v $N /etc",
        "print -v $(echo S) /etc",
        "print -v $USER /etc",
        "print -v $=N /etc",
    ]);
    approves("S=/tmp/w/a; mkdir -p \"$S/x\" && ls $S; rm -rf \"$S/y\"");
    approves("S=/tmp/w/a; T=/tmp/w/b; print -v T /etc; rm -rf \"$S/y\"");
}

/// A name the shell itself sets, ties or reads (and `cd`, which may fail or move `$PWD`) is
/// not a variable the line states, however its assignment is written.
#[test]
fn a_name_the_shell_maintains_is_not_the_lines() {
    all_escalate(&[
        (
            "_=/tmp/ok/a; echo /elsewhere/b >/dev/null; rm -rf $_",
            "_ is assigned",
        ),
        ("PWD=/tmp/ok; cd /usr; rm -rf $PWD/x", "PWD is assigned"),
        (
            "export OLDPWD=/tmp/ok; rm -rf $OLDPWD/x",
            "OLDPWD is assigned",
        ),
        ("HOME=/tmp/ok; S=$HOME/x; rm -rf \"$S\"", "HOME is assigned"),
        ("path=/tmp/evil; rm -rf /tmp/ok/x", "path is assigned"),
        ("PATH=/tmp/evil; rm -rf /tmp/ok/x", "PATH is assigned"),
        ("IFS=/; S=/tmp/ok/a; rm -rf $S/x", "IFS is assigned"),
        ("argv=/; rm -rf /tmp/ok/x", "argv is assigned"),
        ("RANDOM=1; rm -rf /tmp/ok/$RANDOM", "RANDOM is assigned"),
        ("status=/tmp/ok; rm -rf \"$status/x\"", "status is assigned"),
        (
            "BASH_ENV=/tmp/ok/x; rm -rf /tmp/ok/y",
            "BASH_ENV is assigned",
        ),
        ("PATH=/tmp/evil ls; rm -rf /tmp/ok/y", "PATH is assigned"),
        ("rm -rf /tmp/ok/$SECONDS", "$SECONDS is not assigned"),
        ("cd /no/such/dir && rm -rf \"$PWD/x\"", "$PWD"),
        ("cd /tmp/ok; rm -rf x", "relative rm operand"),
        (
            "cd /tmp/ok && rm -rf \"$OLDPWD/x\"",
            "$OLDPWD is not assigned",
        ),
        (
            "S=/tmp/w/a; S+=/../../../etc; rm -rf \"$S\"",
            "`S+=/../../../etc` assigns",
        ),
        (
            "S=/tmp/w/a; S[0]=/etc; rm -rf \"$S/x\"",
            "`S[0]=/etc` assigns",
        ),
        (
            "S=/tmp/w/a; S[1,-1]=/etc; rm -rf \"$S/x\"",
            "`S[1,-1]=/etc` assigns",
        ),
        ("export S+=/x; rm -rf /tmp/ok/y", "export"),
        (
            "export S=/tmp/w/a T=$S/b; rm -rf \"$T\"",
            "$T is not assigned",
        ),
        ("S=/tmp/w/a:~/x; rm -rf \"$S\"", "$S is not assigned"),
        ("S==ls; rm -rf \"/tmp/w/a/$S\"", "$S is not assigned"),
    ]);
    assert_eq!(approves("S_=/tmp/ok; rm -rf $S_/x"), ["/tmp/ok/x"]);
    assert_eq!(
        approves("PWD_OLD=/tmp/ok; rm -rf $PWD_OLD/x"),
        ["/tmp/ok/x"]
    );
    approves("LC_ALL=C sort /tmp/ok/f; S=/tmp/ok; rm -rf $S/y");
    assert_eq!(
        approves("TMPDIR=/etc; D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX) && rm -rf \"$D/x\""),
        ["/private/tmp/claude-502/<mktemp>/x"]
    );
    assert_eq!(
        approves("TMPDIR=$(mktemp -d) && rm -rf \"$TMPDIR/\""),
        ["<mktemp -d>/"]
    );
    assert_eq!(
        approves("TMPDIR=/tmp/w/t; rm -rf \"$TMPDIR/x\""),
        ["/tmp/w/t/x"]
    );
    approves("export S=/tmp/w/a; export T=$S/b; rm -rf \"$T\"");
    approves("S=/tmp/w/a T=$S/b; rm -rf \"$T\"");
    assert_eq!(approves("S=/tmp/w/a:b; rm -rf \"$S\""), ["/tmp/w/a:b"]);
    assert_eq!(approves("S=\"/tmp/w/a:~\"; rm -rf \"$S\""), ["/tmp/w/a:~"]);
}

/// bash runs a list ending in `&` in a background subshell, so an assignment
/// in it is gone afterwards (`S=/x && true & echo "$S"` prints nothing in
/// bash, `/x` in zsh).
#[test]
fn an_assignment_in_a_backgrounded_list_is_not_the_shells() {
    all_escalate(&[
        ("S=/tmp/w/a && true & rm -rf \"$S/x\"", "$S is not assigned"),
        ("S=/tmp/w/a || true & rm -rf \"$S/x\"", "$S is not assigned"),
    ]);
    approves("S=/tmp/w/a && true; rm -rf \"$S/x\"");
}

/// A directory of the test's own under the temp dir, removed on drop (`remove_dir_all` never
/// follows a link).
struct TempTree(aterm_tempfile::TempDir);

impl TempTree {
    fn new(tag: &str) -> Self {
        Self(
            aterm_tempfile::Builder::new()
                .prefix(&format!("aterm-rm-proof-{tag}-"))
                .tempdir()
                .expect("the test's temp dir"),
        )
    }

    /// The path as `std::env::temp_dir` spells it (on macOS through the
    /// `/var` link), no trailing `/`: for the link tests, which are Unix's.
    #[cfg(unix)]
    fn at(&self, rel: &str) -> String {
        let base = self
            .0
            .path()
            .to_string_lossy()
            .trim_end_matches('/')
            .to_string();
        if rel.is_empty() {
            base
        } else {
            format!("{base}/{rel}")
        }
    }
}

/// `cmd` against the live disk with `roots` as the only scratch roots.
#[cfg(unix)]
fn resolve_on_disk(roots: &[ScratchRoot], cwd: &str, cmd: &str) -> Result<Vec<String>, String> {
    let scope = RmScope {
        cwd: Path::new(cwd),
        home: Some(Path::new(HOME)),
        roots,
        probe: &super::super::probe::LiveProbe,
    };
    resolve_rm_line(cmd, &scope)
}

/// rm follows links in an operand's directories: `rm -rf "$S/out/x"` with
/// `out -> /etc` removes `/etc/x`.
#[cfg(unix)]
#[test]
fn an_rm_through_a_symbolic_link_is_judged_where_it_leads() {
    use std::os::unix::fs::symlink;
    let t = TempTree::new("link");
    std::fs::create_dir_all(t.at("p/real")).expect("dir");
    symlink("/etc", t.at("p/out")).expect("link");
    symlink(t.at("p/real"), t.at("p/inner")).expect("link");
    symlink("real", t.at("p/rel")).expect("link");
    symlink("../../..", t.at("p/up")).expect("link");
    symlink("/no/such/aterm/dir", t.at("p/nowhere")).expect("link");
    let roots: Vec<ScratchRoot> = ScratchRoot::dir(t.0.path()).into_iter().collect();
    let run = |cmd: &str| resolve_on_disk(&roots, CWD, cmd);
    let etc = std::fs::canonicalize("/etc")
        .expect("/etc")
        .to_string_lossy()
        .to_string();
    for (cmd, want) in [
        (
            format!("S={}; rm -rf \"$S/out/x\"", t.at("p")),
            format!("leads through a symbolic link to {etc}/x"),
        ),
        (
            format!("rm -rf {}/", t.at("p/out")),
            format!("symbolic link to {etc}"),
        ),
        (
            format!("rm -rf {}/.", t.at("p/out")),
            format!("symbolic link to {etc}"),
        ),
        (
            format!("rm -rf {}", t.at("p/up/etc")),
            "not strictly inside a scratch root".to_string(),
        ),
        (
            format!("rm -rf {}", t.at("p/nowhere/x")),
            "symbolic link to /no/such/aterm/dir/x".to_string(),
        ),
        (
            format!("rm -rf {}", t.at("p/*/x")),
            "can match a symbolic link".to_string(),
        ),
    ] {
        let e = run(&cmd).expect_err(&cmd);
        assert!(e.contains(&want), "{cmd}: {e} does not mention {want}");
    }
    symlink("missing/../../../../../../etc", t.at("p/climb")).expect("link");
    let e = run(&format!("rm -rf {}", t.at("p/climb/x"))).expect_err("climb");
    assert!(e.contains("climbs out of it"), "{e}");
    symlink("b", t.at("p/a")).expect("link");
    symlink("a", t.at("p/b")).expect("link");
    let e = run(&format!("rm -rf {}", t.at("p/a/x"))).expect_err("a cycle");
    assert!(e.contains("more than 32 symbolic links"), "{e}");
    std::fs::create_dir_all(t.at("p/shut/in")).expect("dir");
    let shut = |mode: u32| {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(t.at("p/shut"), std::fs::Permissions::from_mode(mode))
            .expect("chmod");
    };
    shut(0o000);
    let looked = run(&format!("rm -rf {}", t.at("p/shut/in/x")));
    let root_reads_it = std::fs::symlink_metadata(t.at("p/shut/in")).is_ok();
    shut(0o700);
    if !root_reads_it {
        let e = looked.expect_err("a directory the resolver cannot look into");
        assert!(e.contains("cannot be examined on the disk"), "{e}");
    }
    assert_eq!(
        run(&format!("rm -rf {}", t.at("p/out"))),
        Ok(vec![t.at("p/out")])
    );
    let inside = format!(
        "rm -rf {} {} {} {}/ {}",
        t.at("p/inner/x"),
        t.at("p/rel/x"),
        t.at("p/real/x"),
        t.at("p/inner"),
        t.at("p/real/*.o")
    );
    assert_eq!(run(&inside).map(|v| v.len()), Ok(5), "{inside}");
}

/// A root is judged where it leads, through links root owns only: a scratch root swapped for
/// a worker-made link holds nothing.
#[cfg(unix)]
#[test]
fn a_root_is_judged_where_it_leads_through_roots_links_only() {
    use std::os::unix::fs::symlink;
    let t = TempTree::new("root");
    std::fs::create_dir_all(t.at("real")).expect("dir");
    symlink("/usr", t.at("swapped")).expect("link");
    symlink(t.at("real"), t.at("mine")).expect("link");
    for (root, cmd) in [
        ("swapped", format!("rm -rf {}", t.at("swapped/local/x"))),
        ("mine", format!("rm -rf {}", t.at("mine/x"))),
    ] {
        let roots: Vec<ScratchRoot> = ScratchRoot::dir(Path::new(&t.at(root)))
            .into_iter()
            .collect();
        let e = resolve_on_disk(&roots, CWD, &cmd).expect_err(&cmd);
        assert!(
            e.contains("not strictly inside a scratch root"),
            "{cmd}: {e}"
        );
    }
    let roots: Vec<ScratchRoot> = ScratchRoot::dir(Path::new(&t.at("real")))
        .into_iter()
        .collect();
    let ok = format!("rm -rf {}", t.at("real/x"));
    assert_eq!(resolve_on_disk(&roots, CWD, &ok), Ok(vec![t.at("real/x")]));

    let tmp_roots: Vec<ScratchRoot> = [
        ScratchRoot::children_of(Path::new("/tmp")),
        ScratchRoot::children_of(Path::new("/private/tmp")),
    ]
    .into_iter()
    .flatten()
    .collect();
    let name = format!("aterm-rm-proof-absent-{}", std::process::id());
    let only_tmp: Vec<ScratchRoot> = ScratchRoot::children_of(Path::new("/tmp"))
        .into_iter()
        .collect();
    let cmd = format!("rm -rf /tmp/{name}/x");
    assert_eq!(
        resolve_on_disk(&only_tmp, CWD, &cmd),
        Ok(vec![format!("/tmp/{name}/x")])
    );
    if std::fs::read_link("/tmp").is_ok() {
        let cwd = format!("/tmp/{name}/w");
        let e = resolve_on_disk(&tmp_roots, &cwd, &format!("rm -rf /private/tmp/{name}/w"))
            .expect_err("the cwd, spelled through the link");
        assert!(e.contains("cwd or an ancestor"), "{e}");
    }
}

/// The fresh-directory rule: `mktemp -d` makes a new, uniquely named directory, so `"$D"` and
/// a path lexically under it remove nothing that predates the line, wherever `$TMPDIR`
/// points, given a recognised guard and every use of `$D` quoted; `"$D"` alone is proven
/// whatever else reads `$D` (`""` when `mktemp` failed).
#[test]
fn a_directory_the_line_just_made_is_proven_wherever_it_is() {
    for (cmd, want) in [
        ("D=$(mktemp -d) && rm -rf \"$D\"", vec!["<mktemp -d>"]),
        (
            "D=$(mktemp -d) && rm -rf \"$D/x\" \"$D\"/y/z",
            vec!["<mktemp -d>/x", "<mktemp -d>/y/z"],
        ),
        (
            "D=$(mktemp -d -t probe) || exit 1; rm -rf \"$D/x\"",
            vec!["<mktemp -d>/x"],
        ),
        ("D=$(mktemp -d); rm -rf \"${D:?}/x\"", vec!["<mktemp -d>/x"]),
        (
            "D=$(mktemp -d); : \"${D:?}\"; rm -rf \"$D/x\"",
            vec!["<mktemp -d>/x"],
        ),
        (
            "D=$(mktemp -d); test -n \"$D\" && rm -rf \"$D/x\"",
            vec!["<mktemp -d>/x"],
        ),
        (
            "D=$(mktemp -d); [ -d \"$D\" ] && rm -rf \"$D/x\"",
            vec!["<mktemp -d>/x"],
        ),
        (
            "D=$(mktemp -d w.XXXXXX) && rm -rf \"$D/x\"",
            vec!["<mktemp -d>/x"],
        ),
        (
            "D=$(mktemp -d /usr/local/w.XXXXXX) && rm -rf \"$D/x\"",
            vec!["/usr/local/<mktemp>/x"],
        ),
        (
            "d=$(mktemp -d) && ls \"$d\" && rm -rf \"$d/./x\"",
            vec!["<mktemp -d>/./x"],
        ),
        (
            "E=$(mktemp -d) || exit; D=\"$E\"; rm -rf \"$D/x\"",
            vec!["<mktemp -d>/x"],
        ),
        ("D=$(mktemp -d); rm -rf \"$D\"", vec!["<mktemp -d>"]),
        ("D=$(mktemp -d); ls $D; rm -rf \"$D\"", vec!["<mktemp -d>"]),
        (
            "D=$(mktemp -d) && cd $D && rm -rf \"$D\"",
            vec!["<mktemp -d>"],
        ),
        (
            "D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX); rm -rf \"$D\"",
            vec!["/private/tmp/claude-502/<mktemp>"],
        ),
    ] {
        assert_eq!(
            resolve(cmd),
            Ok(want.iter().map(|t| t.to_string()).collect()),
            "{cmd}"
        );
    }
    all_escalate(&[
        ("D=$(mktemp -d) && rm -rf $D/x", "must be double-quoted"),
        ("D=$(mktemp -d) && rm -rf ${D}/x", "must be double-quoted"),
        ("D=$(mktemp -d) && rm -rf $D", "must be double-quoted"),
        (
            "D=$(mktemp -d) && ls $D && rm -rf \"${D:?}/x\"",
            "must be double-quoted",
        ),
        (
            "D=$(mktemp -d) && echo \"$(ls ${D})\" && rm -rf \"$D/x\"",
            "must be double-quoted",
        ),
        (
            "D=$(mktemp -d) && echo \"$(ls $D)\" && rm -rf \"$D/x\"",
            "must be double-quoted",
        ),
        (
            "D=$(mktemp -d) && ls $D && rm -rf \"$D/x\"",
            "must be double-quoted",
        ),
        (
            "D=$(mktemp -d) || exit; X=$(ls $D); rm -rf \"$D/x\"",
            "must be double-quoted",
        ),
        (
            "D=$(mktemp -d) || exit; D=/etc; rm -rf \"$D/x\"",
            "/etc/x is not strictly inside",
        ),
        (
            "D=$(mktemp -d) && D=/etc && rm -rf \"$D/x\"",
            "$D is not assigned a literal",
        ),
        (
            "D=$(mktemp -d) || exit; D=\"$D/..\"; rm -rf \"$D\"",
            "proven only as \"$D\" or a path under it",
        ),
        ("D=$(mktemp -d) && rm -rf \"$D/../x\"", "a .. under it"),
        ("D=$(mktemp -d) && rm -rf \"$D\"/*", "a glob or a brace"),
        (
            "D=$(mktemp -d) && rm -rf \"$D\"x",
            "a sibling of the directory",
        ),
        (
            "D=$(mktemp -d) || exit; S=x; rm -rf \"$D/$S\"",
            "another expansion follows it",
        ),
        ("D=$(mktemp -d); rm -rf \"$D/x\"", "removes /x"),
        ("D=$(mktemp -d); rm -rf \"$D/\"", "the rm then removes /;"),
        ("D=$(mktemp -d); rm -rf \"$D/.\"", "the rm then removes /.;"),
        (
            "D=$(mktemp) && rm -rf \"$D/x\"",
            "$D is not assigned a literal",
        ),
        ("D=$(mktemp -d -p /x); rm -rf \"$D\"", "$D"),
        ("D=$(pwd); rm -rf \"$D/x\"", "$D"),
        (
            "S=/tmp/w; D=$(mktemp -d) && rm -rf \"$S/$D\"",
            "proven only as \"$D\" or a path under it",
        ),
    ]);
}

/// A variable the line does not assign is not proven, whatever this process holds for it.
#[test]
fn a_variable_from_the_environment_is_not_proven() {
    #[cfg(unix)]
    assert!(
        std::env::var_os("HOME").is_some(),
        "this process holds $HOME"
    );
    all_escalate(&[
        ("rm -rf \"$HOME/x\"", "$HOME in an rm operand"),
        (
            "S=$HOME/aterm/target/x; rm -rf \"$S\"",
            "$S is not assigned a literal",
        ),
        (
            "rm -rf \"$TMPDIR/x\"",
            "$TMPDIR is not assigned on this line (a value from the environment is not proven)",
        ),
        (
            "rm -rf \"/private/tmp/claude-502/$USER\"",
            "$USER is not assigned on this line",
        ),
        (
            "rm -rf \"$SCRATCH/x\"",
            "$SCRATCH is not assigned on this line",
        ),
        (
            "S=$SCRATCH; rm -rf \"$S/x\"",
            "$S is not assigned a literal",
        ),
    ]);
    assert_eq!(
        approves("SCRATCH=/private/tmp/claude-502/s; rm -rf \"$SCRATCH/x\""),
        ["/private/tmp/claude-502/s/x"]
    );
    assert_eq!(
        approves("U=me; rm -rf \"/private/tmp/claude-502/$U\""),
        ["/private/tmp/claude-502/me"]
    );
}

/// An assignment in front of a declaring builtin (`export`, `typeset`, `declare`, `readonly`,
/// `local`) is in force while zsh expands its words and kept by bash after it, so the names
/// it assigns are unknown after it and it guards nothing.
#[test]
fn an_assignment_in_front_of_a_declaring_builtin_is_unknown_after_it() {
    let s = "S=/private/tmp/claude-502/s";
    all_escalate(&[
        (
            &format!("{s}; S=/etc export T=$S; rm -rf \"$T/x\""),
            "$T is not assigned",
        ),
        (
            &format!("{s}; S=/etc export S; rm -rf \"$S/x\""),
            "$S is not assigned",
        ),
        (
            "D=$(mktemp -d); D=/x export E=\"${D:?}\"; rm -rf \"$D/etc\"",
            "$D is not assigned",
        ),
        (
            "D=$(mktemp -d); D=/x : \"${D:?}\"; rm -rf \"$D/etc\"",
            "$D is not assigned",
        ),
        (
            "D=$(mktemp -d); E=\"$D\"; D=/x export F=\"${D:?}\"; rm -rf \"$E/etc\"",
            "removes /etc",
        ),
        (
            &format!("{s}; S=/etc integer T; rm -rf \"$S/x\""),
            "`integer`",
        ),
        (&format!("{s}; S=/etc float T; rm -rf \"$S/x\""), "`float`"),
        (
            &format!("{s}; S=/etc typeset T; rm -rf \"$S/x\""),
            "`typeset`",
        ),
        (
            &format!("{s}; S=/etc declare T; rm -rf \"$S/x\""),
            "`declare`",
        ),
        (
            &format!("{s}; S=/etc readonly T; rm -rf \"$S/x\""),
            "`readonly`",
        ),
        (&format!("{s}; S=/etc local T; rm -rf \"$S/x\""), "`local`"),
    ]);
    assert_eq!(
        approves(&format!("{s}; export T=$S; rm -rf \"$T/x\"")),
        ["/private/tmp/claude-502/s/x"]
    );
    assert_eq!(
        approves("D=$(mktemp -d); export E=\"${D:?}\"; rm -rf \"$D/etc\""),
        ["<mktemp -d>/etc"]
    );
}

/// Which words name a variable (`Eval::forget_named`): a lone option cluster names only what
/// is glued after its first letter, and a path word with no arithmetic operator names nothing
/// (`[ -d "$d" ]`, `ls "$out"`).
#[test]
fn a_variable_named_only_by_an_option_letter_or_a_path_is_kept() {
    for (cmd, want) in [
        (
            "d=$(mktemp -d) && [ -d \"$d\" ] && rm -rf \"$d/tmp\"",
            "<mktemp -d>/tmp",
        ),
        (
            "d=/tmp/w/cache && [ -d \"$d\" ] && rm -rf \"$d\"/*",
            "/tmp/w/cache/*",
        ),
        (
            "d=$(mktemp -d) && ls -d \"$d\" && rm -rf \"$d/lib\"",
            "<mktemp -d>/lib",
        ),
        (
            "out=/private/tmp/claude-502/out; ls \"$out\"; rm -rf \"$out\"/*",
            "/private/tmp/claude-502/out/*",
        ),
        (
            "build=/tmp/proj/build; du -sh \"$build\"; rm -rf \"$build\"/*",
            "/tmp/proj/build/*",
        ),
        ("tmp=/tmp/w/t; ls $tmp; rm -rf \"$tmp/\"", "/tmp/w/t"),
        (
            "tmp=$(mktemp -d /tmp/tmp.XXXXXX) && ls \"$tmp\" && rm -rf \"$tmp/x\"",
            "<mktemp>/x",
        ),
        ("f=/tmp/w/f.txt; ls -f \"$f\"; rm -f \"$f\"", "/tmp/w/f.txt"),
        (
            "n=$(mktemp -d); test -n \"$n\" && rm -rf \"$n/x\"",
            "<mktemp -d>/x",
        ),
        (
            "e=$(mktemp -d); [ -e \"$e\" ] && rm -rf \"$e/x\"",
            "<mktemp -d>/x",
        ),
    ] {
        let got = resolve(cmd).unwrap_or_else(|e| panic!("{cmd}: {e}"));
        assert!(got[0].ends_with(want), "{cmd}: {got:?}");
    }
    forgets(&[
        "[ -t \"1/(S=5)\" ]",
        "test -t \"1/S--\"",
        "[ -t -S-- ]",
        "printf %d \"S=5\"",
        "stat -A S /etc",
        "print -v 'S[1]' x",
        "print -vS /etc",
        "print -rvS /etc",
        "printf -vS /etc",
    ]);
}

/// An unknown word forgets every variable only where it could name one or run code
/// (`Eval::forget_named`): double-quoted under a command that takes no word as a name or
/// arithmetic (`ls`, `echo`), it forgets nothing.
#[test]
fn an_unknown_word_forgets_only_where_it_can_name_a_variable() {
    let s = "S=/tmp/w/a";
    for middle in [
        "ls \"$TMPDIR\"",
        "echo \"at $(date)\"",
        "ls \"$UNSETX\"",
        "echo \"$S: cleaning\"",
        "echo \"$USER\"; ls \"$HOME/x\"",
    ] {
        let cmd = format!("{s}; {middle}; rm -rf \"$S\"/*");
        assert_eq!(resolve(&cmd), Ok(vec!["/tmp/w/a/*".to_string()]), "{cmd}");
    }
    forgets(&[
        "X='/tmp/*(e:S=/etc:)'; echo $~X",
        "X='/tmp/*(e:S=/etc:)'; echo hi < $~X",
        "X='/tmp/*(e:S=/etc:)'; true < $~X",
        "echo $X",
        "X='/tmp/*(e:S=/etc:)'; echo $X",
        "Y='S=/etc'; X='/tmp/*(e:eval $Y:)'; echo $X",
        "echo $(date)",
        "print \"$X\" S /etc",
        "printf %d \"$X\"",
        "[ -t \"$X\" ]",
    ]);
}

/// `SHELL_NAMES` keeps every name in zsh and bash whose assignment fails or does not read back.
#[cfg(unix)]
#[test]
fn every_name_whose_assignment_does_not_read_back_is_kept() {
    let mut lanes = Lanes::default();
    for (shell, probe, floors) in [
        (
            "zsh",
            "for n in ${(k)parameters}; do [[ $n =~ '^[A-Za-z_][A-Za-z0-9_]*$' ]] || continue; \
             ( eval \"$n=/x/y\" 2>/dev/null || exit 1; \
             [[ ${(P)n} == /x/y ]] || print -r -- \"= $n\"; exit 0 ) 2>/dev/null \
             || print -r -- $n; done",
            (20, 2),
        ),
        (
            "bash",
            "for n in $(compgen -v); do ( eval \"$n=/x/y\" 2>/dev/null || exit 1; \
             [ \"${!n}\" = /x/y ] || echo \"= $n\"; exit 0 ) 2>/dev/null || echo \"$n\"; done",
            (5, 2),
        ),
    ] {
        let Some((out, _)) = lanes.run(shell, probe) else {
            continue;
        };
        let lines: Vec<&str> = out.lines().collect();
        let read_back_otherwise = lines.iter().filter(|l| l.starts_with("= ")).count();
        let refused = lines.len() - read_back_otherwise;
        let kept: Vec<&str> = lines
            .iter()
            .map(|l| l.strip_prefix("= ").unwrap_or(l))
            .filter(|n| !shell_name(n))
            .collect();
        assert!(kept.is_empty(), "{shell}: not in SHELL_NAMES: {kept:?}");
        assert!(
            refused > floors.0 && read_back_otherwise > floors.1,
            "{shell}: the probe found the shell's own names \
             ({refused} refused, {read_back_otherwise} read back otherwise)"
        );
    }
}

/// The flags that keep `shell` from reading a startup file: zsh's `-f` skips
/// `~/.zshenv` and the rest (the caller clears the environment); bash reads
/// none for a script with `BASH_ENV` cleared.
fn no_startup(shell: &str) -> &'static [&'static str] {
    if shell == "zsh" { &["-f"] } else { &[] }
}

/// `line` in the [`aterm_spec::host::TOOLS`] shell `shell` installed at
/// `path`, in `dir`, no startup file read, stdin closed, under the one-minute
/// hang detector. The shell reads `line` from a file outside `dir`, not from
/// its command line: Windows bounds a command line (32,767 characters, and Git
/// for Windows' `bash.exe` passes about 8,000 of them on), which a test's batch
/// of lines outgrows.
fn run_shell(shell: &str, path: &Path, line: &str, dir: &Path) -> std::process::Output {
    let script = TempTree::new("script");
    let file = script.0.path().join("line.sh");
    std::fs::write(&file, line).expect("the line's file");
    aterm_uds::deadline::output_until(
        aterm_uds::spawnfd::command(path)
            .args(no_startup(shell))
            .arg(&file)
            .current_dir(dir)
            .env_clear()
            .env("PATH", "/usr/bin:/bin"),
        std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap_or_else(|e| panic!("{shell} within the hang detector: {e}"))
}

/// `line` in `shell`, in a temp dir of the test's own, no startup file read,
/// stdin closed: its stdout, trimmed.
fn in_shell(shell: &str, line: &str) -> String {
    let (out, _) = in_shell_both(shell, line);
    out.trim().to_string()
}

/// Multi-shell runner that executes each installed shell and reports missing shells on drop.
#[derive(Default)]
struct Lanes {
    absent: Vec<&'static str>,
}

impl Lanes {
    /// `line` in `shell` as [`in_shell_both`] runs it, or `None`, noted, where
    /// `shell` is found nowhere ([`aterm_spec::host::find_tool`]).
    fn run(&mut self, shell: &'static str, line: &str) -> Option<(String, String)> {
        if let Some(path) = aterm_spec::host::find_tool(shell) {
            return Some(in_shell_at(shell, &path, line));
        }
        if !self.absent.contains(&shell) {
            self.absent.push(shell);
        }
        None
    }

    /// Each of `lines` in `shell`, its stdout and its stderr, in order, or
    /// `None`, noted, where `shell` is not installed.
    fn run_each(&mut self, shell: &'static str, lines: &[String]) -> Option<Vec<(String, String)>> {
        let mut script = String::new();
        for line in lines {
            script.push_str("( eval '");
            script.push_str(&line.replace('\'', "'\\''"));
            script.push_str("' )\nprintf '\\036'; printf '\\036' >&2\n");
        }
        let (out, err) = self.run(shell, &script)?;
        let split = |all: &str| -> Vec<String> {
            let mut each: Vec<String> = all.split(LINE_END).map(str::to_string).collect();
            assert_eq!(
                each.pop().as_deref(),
                Some(""),
                "{shell}: past the last line: {all:?}"
            );
            each
        };
        let (outs, errs) = (split(&out), split(&err));
        assert!(
            outs.len() == lines.len() && errs.len() == lines.len(),
            "{shell}: {} lines, {} outputs, {} error streams:\n{out}\n{err}",
            lines.len(),
            outs.len(),
            errs.len()
        );
        Some(outs.into_iter().zip(errs).collect())
    }
}

/// What [`Lanes::run_each`] prints after each line, on both streams: an
/// ASCII record separator, which no line prints.
const LINE_END: char = '\u{1e}';

impl Drop for Lanes {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            aterm_spec::host::could_not_run_without(&self.absent);
        }
    }
}

/// A [`PLAIN_CONSUMERS`] command assigns no variable, so its words name nothing
/// (`echo "OUT=$OUT"` keeps `OUT` known).
#[test]
fn a_plain_commands_words_name_nothing() {
    for (cmd, want) in [
        (
            "OUT=/tmp/w/out; echo \"OUT=$OUT\"; rm -rf \"$OUT\"/*",
            vec!["/tmp/w/out/*"],
        ),
        (
            "d=$(mktemp -d) && ls -ld \"$d\" && rm -rf \"$d/\"",
            vec!["<mktemp -d>/"],
        ),
        (
            "h=/tmp/w/h; du -sh \"$h\"; rm -rf \"$h/x\"",
            vec!["/tmp/w/h/x"],
        ),
        (
            "D=$(mktemp -d); OUT=/tmp/w/out; rm -rf \"$D\" \"$OUT\"/*",
            vec!["<mktemp -d>", "/tmp/w/out/*"],
        ),
        (
            "S=/tmp/w/a; grep -rn S=x \"$S\"; rm -rf \"$S/x\"",
            vec!["/tmp/w/a/x"],
        ),
        ("S=/tmp/w/a; echo S; rm -rf \"$S/x\"", vec!["/tmp/w/a/x"]),
        (
            "S=/tmp/w/a; A=S=5; echo A; rm -rf \"$S/x\"",
            vec!["/tmp/w/a/x"],
        ),
    ] {
        assert_eq!(
            resolve(cmd),
            Ok(want.iter().map(|t| t.to_string()).collect()),
            "{cmd}"
        );
    }
}

/// [`PLAIN_CONSUMERS`] holds no command that can assign a variable it names or read a word
/// as arithmetic: each head below assigns `S` in zsh 5.9 or bash 3.2 (zsh modules per their
/// manuals) and is refused or forgets `S`; no builtin member assigns.
#[test]
fn a_command_that_can_assign_what_it_names_is_no_plain_consumer() {
    let assigners = [
        "stat -A S /etc",
        "zstat -A S /etc",
        "print -v S /etc",
        "printf -v S /etc",
        "zformat -f S /etc",
        "zstyle -s :c s S",
        "zparseopts a:=S",
        "strftime -s S %s 0",
        "sysread -o 1 S",
        "zselect -a S",
        "vared S",
        "getln S",
        "read S",
        "getopts ab S",
        "[ -t S ]",
        "test -t S",
        "printf %d S=5",
        "print -f %d S=5",
        "return S=5",
    ];
    for middle in assigners {
        let head = middle.split(' ').next().expect("a head");
        assert!(!PLAIN_CONSUMERS.contains(&head), "{head}");
        let cmd = format!("S=/tmp/w/a; {middle}; rm -rf \"$S/x\"");
        let e = escalates(&cmd);
        assert!(
            e.contains("$S is not known after") || e.contains(&format!("`{head}`")),
            "{cmd}: {e}"
        );
    }
    let mut lanes = Lanes::default();
    let (mut zsh, mut bash) = (Vec::new(), Vec::new());
    for member in PLAIN_CONSUMERS {
        zsh.push(format!(
            "S=/x/y; if [[ $(whence -w -- '{member}') == *': builtin' ]]; then \
             '{member}' S -v S -A S S=5 -t 'a[S=5]' >/dev/null 2>&1; fi; print -r -- \"$S\""
        ));
        bash.push(format!(
            "S=/x/y; if [ \"$(type -t -- '{member}')\" = builtin ]; then \
             '{member}' S -v S -A S S=5 -t 'a[S=5]' >/dev/null 2>&1; fi; echo \"$S\""
        ));
    }
    for (sh, lines) in [("zsh", zsh), ("bash", bash)] {
        let Some(read) = lanes.run_each(sh, &lines) else {
            continue;
        };
        for (member, (out, _)) in PLAIN_CONSUMERS.iter().zip(read) {
            assert_eq!(out.trim(), "/x/y", "{sh}: {member}");
        }
    }
}

/// zsh's arithmetic reads a variable's value as arithmetic in turn (`A=S=5; printf %d A`
/// sets `S`), so a word it reads forgets what its variables' values name.
#[test]
fn a_value_zsh_arithmetic_reads_is_read_too() {
    forgets(&[
        "A=S=5; printf %d A",
        "A=B; B=S=5; [ -t A ]",
        "A=S=5; printf '%s %d' x 1/A",
        "printf %d HOSTX",
        "test -t 1/UNSETX",
        "A=S=5; print -f %d A",
        "A=S=5; print -v T -f %d A",
        "A=S=5; print -u 2 -f %d A",
        "A=S=5; return A",
        "A=S=5; printf -- %d A",
        "A=x/S; printf %d A",
    ]);
    for cmd in [
        "S=/tmp/w/a; printf '%s\\n' \"$S\"; rm -rf \"$S/x\"",
        "S=/tmp/w/a; A=S=5; printf %d /A; rm -rf \"$S/x\"",
        "S=/tmp/w/a; [ -d \"$S\" ] && rm -rf \"$S/x\"",
        "S=/tmp/w/a; [ -t 1 ] && rm -rf \"$S/x\"",
        "S=/tmp/w/a; A=S=5; [ -n A ] && rm -rf \"$S/x\"",
        "S=/tmp/w/a; printf %d 5; rm -rf \"$S/x\"",
    ] {
        assert_eq!(resolve(cmd), Ok(vec!["/tmp/w/a/x".to_string()]), "{cmd}");
    }
    let out = in_shell(
        "zsh",
        "A=S=5; S=/tmp/w/a; printf %d A >/dev/null 2>&1; B=S=6; printf %d /B >/dev/null \
         2>&1; echo \"$S\"",
    );
    assert_eq!(
        out, "5",
        "zsh reads A's value as arithmetic, and /B not at all"
    );
}

/// A line with a builtin that changes how later words read, or zsh's `integer`/`float`
/// (whose values are arithmetic), is not followed.
#[test]
fn a_builtin_that_changes_how_words_read_is_not_followed() {
    all_escalate(&[
        (
            "S=/tmp/w/a; A=S=5; integer T=A; rm -rf \"$S/x\"",
            "`integer`",
        ),
        ("S=/tmp/w/a; float T=1; rm -rf \"$S/x\"", "`float`"),
        ("setopt GLOB_SUBST; S=/tmp/w/a; rm -rf \"$S/x\"", "`setopt`"),
        ("unsetopt EQUALS; S=/tmp/w/a; rm -rf \"$S/x\"", "`unsetopt`"),
        ("emulate sh; S=/tmp/w/a; rm -rf \"$S/x\"", "`emulate`"),
        ("shopt -s extglob; S=/tmp/w/a; rm -rf \"$S/x\"", "`shopt`"),
        (
            "zmodload zsh/stat; S=/tmp/w/a; rm -rf \"$S/x\"",
            "`zmodload`",
        ),
    ]);
    approves("S=/tmp/w/a; rm -rf \"$S/x\"");
}

/// After `$NAME:`, every letter of zsh's modifier set reshapes the value, so each escalates.
#[test]
fn every_zsh_modifier_letter_is_not_text() {
    let letters = "aAcefFghlpPqQrsStuwWx&";
    for m in letters.chars() {
        for cmd in [
            format!("S=/tmp/w/a.b; rm -rf \"$S:{m}\""),
            format!("S=/tmp/w/a.b; rm -rf /tmp/w/c/$S:{m}/x"),
        ] {
            assert!(escalates(&cmd).contains("zsh computes"), "{cmd}");
        }
    }
    let probe: String = letters
        .chars()
        .map(|m| format!("(print -r -- \"{m} $S:{m}\") 2>/dev/null; "))
        .collect();
    let out = in_shell("zsh", &format!("S=/tmp/w/a.b; {probe}"));
    let differ: Vec<char> = out
        .lines()
        .filter_map(|l| {
            let (m, v) = l.split_once(' ')?;
            let m = m.chars().next()?;
            (v != format!("/tmp/w/a.b:{m}")).then_some(m)
        })
        .collect();
    assert!(differ.len() >= 10, "zsh reads the modifiers: {differ:?}");
    for m in differ {
        let cmd = format!("S=/tmp/w/a.b; rm -rf \"$S:{m}\"");
        assert!(escalates(&cmd).contains("zsh computes"), "{cmd}");
    }
}

/// A path word with `=`, `(`, `[`, `++` or `--` may be arithmetic that assigns (`x/S=5`,
/// `x/S++`), so it names its variables.
#[test]
fn a_path_word_with_an_arithmetic_operator_names_its_variables() {
    forgets(&[
        "zcompute 'x/S=5'",
        "zcompute 'x/(S)'",
        "zcompute 'x/a[S]'",
        "zcompute 'x/S++'",
        "zcompute 'x/S--'",
    ]);
    keeps(&["zcompute 'x/S'"]);
}

/// A double-quoted `$(…)` names nothing only under a [`PLAIN_CONSUMERS`] command: under
/// another its output may be a name a builtin assigns (`print -v "$(echo S)" /etc`).
#[test]
fn a_quoted_substitution_under_another_command_forgets() {
    forgets(&[
        "print -v \"$(echo S)\" /etc",
        "printf -v \"$(echo S)\" /etc",
        "[ -t \"$(echo S)\" ]",
        "stat -A \"$(echo S)\" /etc",
    ]);
    keeps(&["echo \"$(echo S)\""]);
}

/// A session cwd reached through a user's link keeps its `<cwd>/target*` root, resolved where
/// the cwd leads.
#[cfg(unix)]
#[test]
fn a_cwd_through_a_users_link_keeps_its_target_root() {
    use std::os::unix::fs::symlink;
    let t = TempTree::new("cwdlink");
    std::fs::create_dir_all(t.at("real/proj/target/debug")).expect("dir");
    symlink(t.at("real/proj"), t.at("proj")).expect("link");
    let cwd = t.at("proj");
    let roots: Vec<ScratchRoot> = ScratchRoot::glob_under(Path::new(&cwd), "target*")
        .into_iter()
        .collect();
    let run = |cmd: &str| resolve_on_disk(&roots, &cwd, cmd);
    assert_eq!(
        run(&format!("rm -rf {cwd}/target/debug/x")),
        Ok(vec![format!("{cwd}/target/debug/x")])
    );
    for cmd in [
        format!("rm -rf {cwd}/"),
        format!("rm -rf {}", t.at("real/proj")),
    ] {
        let e = run(&cmd).expect_err(&cmd);
        assert!(
            e.contains("cwd or an ancestor") || e.contains("not strictly inside"),
            "{cmd}: {e}"
        );
    }
    let e = run(&format!("rm -rf {cwd}/src/x")).expect_err("outside target");
    assert!(e.contains("not strictly inside"), "{e}");
}

/// zsh's `printf` reads its arguments as arithmetic only under a numeric conversion or a `*`
/// width or precision (`%d`, `%*s`, `%.*s`; not `%s`, `%b`, `%q`, `%c`, `%1$s`, `%%d`), and
/// only `-v NAME` assigns.
#[test]
fn a_printf_format_with_no_numeric_conversion_reads_no_arithmetic() {
    for (cmd, want) in [
        (
            "S=/private/tmp/claude-502/w; printf '%s\\n' done; rm -rf \"$S/x\"",
            "/private/tmp/claude-502/w/x",
        ),
        (
            "OUT=/private/tmp/claude-502/out; printf 'OUT=%s\\n' \"$OUT\"; rm -rf \"$OUT\"/*",
            "/private/tmp/claude-502/out/*",
        ),
        (
            "S=/tmp/w/a; printf '%s %b %c %q %5.3s %1$s %%d\\n' S=5 S a b c d; rm -rf \"$S/x\"",
            "/tmp/w/a/x",
        ),
        (
            "S=/tmp/w/a; printf -- 'S=%s\\n' ok; rm -rf \"$S/x\"",
            "/tmp/w/a/x",
        ),
        ("S=/tmp/w/a; printf S; rm -rf \"$S/x\"", "/tmp/w/a/x"),
        ("S=/tmp/w/a; printf - %d S=5; rm -rf \"$S/x\"", "/tmp/w/a/x"),
    ] {
        assert_eq!(resolve(cmd), Ok(vec![want.to_string()]), "{cmd}");
    }
    for middle in [
        "printf '%*s' S=5 x",
        "printf '%.*s' S=5 x",
        "printf '%s %d' x S=5",
        "printf '%ld' S=5",
        "printf '%n' S=5",
        "printf %d A",
        "printf -v S '%s' /etc",
        "printf -vS '%s' /etc",
        "E=; printf $E %d S=5",
        "E=; printf $E -v S /etc",
        "printf {-v,S} /etc",
        "printf %s* S=5",
        "printf -%d S=5",
        "printf -x%d S=5",
        "printf -%d A",
        "printf -v T -%d S=5",
        "printf -vT -vS x",
    ] {
        let cmd = format!("S=/tmp/w/a; A=S=5; {middle}; rm -rf \"$S/x\"");
        assert!(escalates(&cmd).contains("$S is not"), "{cmd}");
    }
    assert!(
        escalates("D=$(mktemp -d); printf -%d D=0; rm -rf \"$D\"").contains("$D is not"),
        "zsh makes D 0: the rm removes ./0"
    );
    let probe = "S=1; printf '%s %b %c %q %1$s %%d' S=5 S=5 S=5 S=5 >/dev/null 2>&1; \
                 printf 'S=%s' x >/dev/null; printf S=5 >/dev/null; print -r -- \"$S\"; \
                 (printf '%*s' S=5 x >/dev/null 2>&1; print -r -- \"$S\"); \
                 (printf '%.*s' S=6 x >/dev/null 2>&1; print -r -- \"$S\")";
    let out = in_shell("zsh", probe);
    assert_eq!(
        out, "1\n5\n6",
        "zsh's printf reads arithmetic only where it says"
    );
    let probe = "S=1; printf - %d S=5 >/dev/null 2>&1; print -r -- \"$S\"; \
                 printf -%d S=2 >/dev/null 2>&1; print -r -- \"$S\"; \
                 printf -x%d S=3 >/dev/null 2>&1; print -r -- \"$S\"; \
                 printf -v T -%d S=4 >/dev/null 2>&1; print -r -- \"$S $T\"";
    let out = in_shell("zsh", probe);
    assert_eq!(
        out, "1\n2\n3\n4 -4",
        "zsh's printf takes a `-` word other than `-v` as its format"
    );
}

/// Read-only external programs run from disk cannot assign the shell's variables and are [`PLAIN_CONSUMERS`].
#[test]
fn an_external_read_only_program_names_nothing() {
    keeps(&[
        "shasum \"$HOME/f\"",
        "jq -r .name \"$TMPDIR/p.json\"",
        "tr a b < \"$HOME/f\"",
        "awk -v S=1 '{print}' \"$S/f\"",
        "sed -n 's/S=//p' \"$HOME/f\"",
        "git -C \"$HOME/src\" log -1 --format=S=%s",
        "nl \"$(date)\"",
    ]);
    for middle in [
        "stat -A S /etc",
        "stat \"$HOME/f\"",
        "env \"$HOME/f\"",
        "xargs \"$HOME/f\"",
        "print \"$HOME/f\"",
        "sed -n 1p $HOME/f",
    ] {
        let head = middle.split(' ').next().expect("a head");
        assert!(head == "sed" || !PLAIN_CONSUMERS.contains(&head), "{head}");
        let cmd = format!("S=/private/tmp/claude-502/w; {middle}; rm -rf \"$S/x\"");
        assert!(escalates(&cmd).contains("$S is not"), "{cmd}");
    }
}

/// zsh's `{S}>file` stores the new descriptor's number in `S` under a builtin, even with a
/// blank before the `>`; an unquoted `{NAME}` word forgets `NAME` under every command.
#[test]
fn a_named_descriptor_redirect_assigns_its_name() {
    all_escalate(&[
        (
            "S=/private/tmp/claude-502/w; echo hi {S} >/dev/null; rm -rf \"$S/x\"",
            "$S is not known after `echo`",
        ),
        (
            "S=/private/tmp/claude-502/w; true {S}</dev/null; rm -rf \"$S/x\"",
            "$S is not known after `true`",
        ),
        (
            "S=/private/tmp/claude-502/w; : {S}</dev/null; rm -rf \"$S/x\"",
            "$S is not known after `:`",
        ),
        (
            "S=/private/tmp/claude-502/w; pwd {S}>/dev/null; rm -rf \"$S\"",
            "$S is not known after `pwd`",
        ),
        (
            "S=/private/tmp/claude-502/w; printf '%s' x {S}>/dev/null; rm -rf \"$S/x\"",
            "$S is not known after `printf`",
        ),
        (
            "D=$(mktemp -d) && : {D}>/dev/null && rm -rf \"$D/x\"",
            "$D is not known after `:`",
        ),
        (
            "D=$(mktemp -d); echo hi {D}>/dev/null; rm -rf \"$D\"",
            "$D is not known after `echo`",
        ),
    ]);
    keeps(&[
        "echo '{S}'>/dev/null",
        "echo \\{S}>/dev/null",
        "echo hi {T}>/dev/null",
    ]);
    let probe = "S=/x; T=/y; U=/z; V=/v; echo hi {S}>/dev/null; ls {T}>/dev/null; \
                 echo '{U}'>/dev/null; echo hi {V} >/dev/null; print -r -- \"$S $T $U $V\"";
    let out = in_shell("zsh", probe);
    let f: Vec<&str> = out.lines().last().unwrap_or("").split(' ').collect();
    assert!(
        f.len() == 4
            && [f[0], f[3]]
                .iter()
                .all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            && f[1..3] == ["/y", "/z"],
        "zsh stores a builtin's descriptor in S and V, not an external's in T: {out}"
    );
}

/// Brace expansion moves the words the rule reads: zsh expands `A=S=5; printf {%d,A}` to
/// `printf %d A`, and bash `printf {-vS,%s} /etc` to `printf -vS …`.
#[test]
fn a_brace_expanded_word_moves_the_words_the_rule_reads() {
    forgets(&[
        "A=S=5; printf {%d,A}",
        "A=S=5; printf -- {%d,A}",
        "A=S=5; printf {-%d,A}",
        "A=S=5; printf {%d,A} >/dev/null",
        "A=S=5; printf %{d,} A",
        "printf {-vS,%s} /etc",
        "printf -{vS,-} /etc",
        "printf -{vT,vS} /etc",
        "printf -v{S,} /etc",
        "A=S=5; [ {-t,A} ]",
        "A=S=5; test {-t,A}",
        "A=S=5; printf %d {A,1}",
    ]);
    keeps(&[
        "printf '{%s}\\n' x",
        "printf '%s\\n' {a,b}",
        "printf '%s\\n' S={5,6}",
        "printf \\{-vS,%s} /etc",
        "echo {-vS,%s} /etc",
    ]);
    let probe = "S=1; A=S=5; printf {%d,A} >/dev/null; print -r -- \"$S\"; \
                 S=1; A=S=6; [ {-t,A} ]; print -r -- \"$S\"; \
                 S=1; printf '%s' S={7,8} >/dev/null; print -r -- \"$S\"";
    let mut lanes = Lanes::default();
    if let Some((out, _)) = lanes.run("zsh", probe) {
        assert_eq!(
            out.trim(),
            "5\n6\n1",
            "zsh reads the words brace expansion made"
        );
    }
    let probe = "S=1; printf {-vS,%s} /etc; echo \"$S\"; T=1; printf -{vT,-} /x; echo \"$T\"";
    if let Some((out, _)) = lanes.run("bash", probe) {
        assert_eq!(
            out.trim(),
            "/etc\n/x",
            "bash's printf -v made by brace expansion"
        );
    }
}

/// A glob makes words from the working directory's names: beside a file `S=5`, zsh runs
/// `printf %d *` as arithmetic on `S=5`.
#[test]
fn a_globbed_word_moves_the_words_the_rule_reads() {
    forgets(&[
        "printf %d *",
        "printf %d S=?",
        "printf '%s %d' x S*",
        "[ -t * ]",
        "[ * ]",
        "test -t S=[5]",
        "printf * S=5",
        "printf -* S=5",
    ]);
    keeps(&[
        "printf '%s\\n' *.log",
        "printf %d '*'",
        "printf %d \\*",
        "ls *",
        "echo S=?",
    ]);
}

/// A glob or brace word starting with `/` expands only to absolute paths, so the rule keeps
/// proving past one.
#[test]
fn an_absolute_glob_moves_no_word_the_rule_reads() {
    keeps(&[
        "stat -f %z \"$S\"/*.bin",
        "stat -f %z $S/{a,b}.bin",
        "printf %d /tmp/w/*",
        "[ -e /tmp/w/*.log ]",
        "test -t 1 /tmp/w/*",
    ]);
    forgets(&[
        "stat -f %z ./*.bin",
        "T='/tmp/w -vS'; printf %d \"$T\"/*",
        "printf %d *",
    ]);
}

/// A `-` word zsh takes as `printf`'s format is read as a format (bash refuses it as an
/// unknown option).
#[test]
fn a_dash_format_with_no_numeric_conversion_reads_no_arithmetic() {
    keeps(&[
        "printf '--- %s ---\\n' done",
        "printf -- '--- %s ---\\n' done",
        "printf -%s S=5",
        "printf -v T '--- %s' S=5",
    ]);
    forgets(&["printf '--- %d ---\\n' S=5"]);
    let probe = "S=1; printf '--- %s ---' S=5 >/dev/null 2>&1; printf -%s S=6 >/dev/null 2>&1; \
                 printf '--- %d ---' S=7 >/dev/null 2>&1; echo \"$S\"";
    let mut lanes = Lanes::default();
    if let Some((out, _)) = lanes.run("zsh", probe) {
        assert_eq!(out.trim(), "7", "zsh evaluates only the numeric `-` format");
    }
    if let Some((out, _)) = lanes.run("bash", probe) {
        assert_eq!(
            out.trim(),
            "1",
            "bash assigns nothing for an unknown option"
        );
    }
}

/// Under `EXTENDED_GLOB`, `^`, `#` and `~` glob too: beside files `-t`, `-vS`
/// and `S=5`, `[ ^-vS ]` is `[ -t S=5 ]`, and `rm -rf <root>/^x` removes every
/// name but `x`.
#[test]
fn an_extended_glob_word_moves_the_words_the_rule_reads() {
    forgets(&[
        "[ ^-vS ]",
        "test ^x",
        "[ x~y ]",
        "printf ^%s S=5",
        "printf %s~x S=5",
    ]);
    for cmd in [
        "rm -rf /private/tmp/claude-502/^x",
        "rm -rf /private/tmp/claude-502/x#",
    ] {
        let e = escalates(cmd);
        assert!(
            e.contains("not strictly inside a scratch root"),
            "{cmd}: {e}"
        );
    }
    for middle in ["[ -d ~/w ]", "printf '%s\\n' ^x", "echo ^-vS", "[ '^-vS' ]"] {
        let cmd = format!("S=/private/tmp/claude-502/w; {middle}; rm -rf \"$S/x\"");
        assert_eq!(
            resolve(&cmd),
            Ok(vec!["/private/tmp/claude-502/w/x".to_string()]),
            "{cmd}"
        );
    }
    assert_eq!(
        resolve("rm -rf /private/tmp/claude-502/w/^x"),
        Ok(vec!["/private/tmp/claude-502/w/^x".to_string()])
    );
    assert_eq!(
        resolve("S=\"/private/tmp/claude-502/a:~\"; rm -rf \"$S\""),
        Ok(vec!["/private/tmp/claude-502/a:~".to_string()])
    );
}

/// Past `printf`'s non-arithmetic as-written format, a double-quoted word is one word of
/// data.
#[test]
fn a_quoted_word_printf_reads_as_data_names_nothing() {
    keeps(&[
        "printf '%s\\n' \"$x\"",
        "printf '%s: %s\\n' \"$x\" \"$(date)\"",
        "printf -- '%s\\n' \"$x\"",
        "printf -v T '%s' \"$x\"",
        "printf '%s\\n' \"$S\" \"${x}\"",
    ]);
    forgets(&[
        "printf \"$f\" x",
        "printf -- \"$x\"",
        "printf -v \"$x\" '%s' y",
        "printf \"$x\" '%s' y",
        "printf '%s\\n' $x",
        "printf '%d\\n' \"$x\"",
        "printf '%s %d\\n' a \"$x\"",
        "print \"$x\"",
        "[ -t \"$x\" ]",
        "printf '%s\\n' \"$x\" >\"$y\"",
    ]);
}

/// A string or file operand of `[`/`test` names nothing ([`test_operands`]); checked in
/// zsh 5.9 and bash 3.2 where installed.
#[test]
fn a_quoted_test_operand_names_nothing() {
    for middle in [
        "[ -n \"$x\" ];",
        "[ -z \"$x\" ];",
        "[ \"$x\" = y ];",
        "[ -d \"$HOME\" ] &&",
        "[ -n \"$1\" ] &&",
        "test -n \"$x\";",
        "test \"$x\";",
        "[ \"$x\" ];",
        "[ ! \"$x\" ];",
        "[ ! -f \"$x\" ];",
        "[ \"$x\" != \"$y\" ];",
        "[ \"$x\" -eq 1 ];",
        "[ -d \"$HOME/x\" ] &&",
        "[ -e \"$x\"/y ] ||",
    ] {
        let cmd = format!("S=/private/tmp/claude-502/w; {middle} rm -rf \"$S/x\"");
        assert_eq!(
            resolve(&cmd),
            Ok(vec!["/private/tmp/claude-502/w/x".to_string()]),
            "{cmd}"
        );
    }
    for middle in [
        "[ -t \"$x\" ];",
        "test -t \"$x\";",
        "[ -v \"$x\" ];",
        "[ -R \"$x\" ];",
        "[ -n \"$x\"* ];",
        "[ -n {\"$x\",a} ];",
        "[ -n $x ];",
        "[ \"$op\" \"$x\" ];",
        "[ \"$x\" -n ];",
        "[ \"$x\" -a \"$y\" ];",
        "[ \"$x\" = y -o \"$z\" ];",
        "[ -n x ] >\"$x\";",
        "[ -n \"$x\" ;",
        "[ \"$@\" ] &&",
        "[ -n \"$@\" ] &&",
        "[ -z \"$@\" ];",
        "[ \"$x$@\" ];",
    ] {
        let cmd = format!("S=/private/tmp/claude-502/w; {middle} rm -rf \"$S/x\"");
        assert!(escalates(&cmd).contains("$S is not known after"), "{cmd}");
    }
    assert!(
        escalates("S=/private/tmp/claude-502/w; [[ -n \"$x\" ]]; rm -rf \"$S/x\"")
            .contains("compound command")
    );
    let values = [
        "-t",
        "S=5",
        "-v",
        "-vS",
        "a[$(echo RAN_$((6*7)) >&2)]",
        "path[$(echo RAN_$((6*7)) >&2)]",
        "(",
        ")",
        "!",
        "=",
        "-a",
        "-o",
        "-t S=5",
        "1/(S=5)",
    ];
    let forms = [
        "[ -n \"$x\" ]",
        "[ -z \"$x\" ]",
        "[ \"$x\" ]",
        "[ ! \"$x\" ]",
        "[ ! -n \"$x\" ]",
        "[ -d \"$x\" ]",
        "[ \"$x\" = \"$y\" ]",
        "[ \"$x\" != \"$y\" ]",
        "[ \"$x\" -eq \"$y\" ]",
        "test \"$x\" = \"$y\"",
        "[ -n \"$1\" ]",
    ];
    let mut lines = Vec::new();
    for x in values {
        for y in [values[0], x] {
            for form in forms {
                lines.push(format!(
                    "S=/x/y; x='{x}'; y='{y}'; set -- '{x}'; {form}; echo \"$S\""
                ));
            }
        }
    }
    let mut lanes = Lanes::default();
    for sh in ["zsh", "bash"] {
        let Some(read) = lanes.run_each(sh, &lines) else {
            continue;
        };
        for (line, (out, err)) in lines.iter().zip(read) {
            assert_eq!(out.trim(), "/x/y", "{sh}: {line}");
            assert!(!err.contains("RAN_42"), "{sh}: {line}");
        }
    }
}

/// `line` in `shell` as [`in_shell`] runs it: its stdout and its stderr. A
/// shell found nowhere ends the test COULD NOT RUN, naming where one comes from.
fn in_shell_both(shell: &str, line: &str) -> (String, String) {
    in_shell_at(shell, &aterm_spec::host::need_tool(shell), line)
}

/// `line` in `shell`, installed at `path`, as [`in_shell_both`] runs it.
fn in_shell_at(shell: &str, path: &Path, line: &str) -> (String, String) {
    let t = TempTree::new("shell");
    let out = run_shell(shell, path, line, t.0.path());
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// zsh subscripts a special parameter like a name, as arithmetic: after `A=S=0`,
/// `echo "$#[A]"` sets `S` to 0.
#[test]
fn a_subscript_on_a_special_parameter_escalates() {
    for sub in [
        "\"$#[A]\"",
        "\"$?[A]\"",
        "\"$@[A]\"",
        "\"$![A]\"",
        "\"$-[A]\"",
        "$*[A]",
        "\"$1[A]\"",
        "\"$#A[A]\"",
    ] {
        let cmd = format!("A=S=0; S=/private/tmp/claude-502/w; echo {sub}; rm -rf \"$S/x\"");
        assert!(
            escalates(&cmd).contains("subscript on a special parameter"),
            "{cmd}"
        );
    }
    keeps(&["echo \"$#\" \"$?\" \"$1\"", "printf '%s\\n' \"$#\""]);
}

/// `printf`'s format is read after its Unicode escapes are decoded: one that
/// decodes to `%d` is a numeric conversion.
#[test]
fn a_printf_format_is_read_after_its_unicode_escapes() {
    for middle in ["printf 'BSu2713 %sBSn' \"$x\"", "printf 'BSu0025s' \"$x\""] {
        let cmd =
            format!("S=/private/tmp/claude-502/w; {middle}; rm -rf \"$S/x\"").replace("BS", "\\");
        assert_eq!(
            resolve(&cmd),
            Ok(vec!["/private/tmp/claude-502/w/x".to_string()]),
            "{cmd}"
        );
    }
    for middle in ["printf 'BSu0025d' \"$x\"", "printf '%BSu0064' \"$x\""] {
        let cmd =
            format!("S=/private/tmp/claude-502/w; {middle}; rm -rf \"$S/x\"").replace("BS", "\\");
        assert!(escalates(&cmd).contains("$S is not known after"), "{cmd}");
    }
}
