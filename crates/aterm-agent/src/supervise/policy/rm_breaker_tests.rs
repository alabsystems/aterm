// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The rm-breaker resolver against the shapes the audit measured: the
//! approvals the synthesis names, and a negative control beside each rule.

use std::path::{Path, PathBuf};

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

/// The synthesis's named cases: the one approval and its four refusals.
#[test]
fn the_synthesis_cases() {
    assert_eq!(
        approves("S=/private/tmp/claude-502/x; rm -rf \"$S/t5\""),
        ["/private/tmp/claude-502/x/t5"]
    );
    assert!(escalates("rm -rf \"$UNSET/t5\"").contains("$UNSET is not assigned"));
    assert!(escalates("S=/usr; rm -rf \"$S/t5\"").contains("not strictly inside a scratch root"));
    assert!(escalates("cd /private/tmp/claude-502/x && rm -rf t5").contains("relative rm operand"));
    // The 2026-09-21 incident (docs/DESIGN-claude-harness-permission-hooks
    // §1), with its scratch variable set as the workflow set it.
    let incident = "S=/private/tmp/claude-502/s; for pair in \"t_mb 630604f8\" \
                    \"t_sv salvage/x\" \"t_om origin/main\"; do set -- $pair; rm -rf $S/$1; \
                    mkdir -p $S/$1; git archive $2 | tar -x -C $S/$1; done";
    assert!(escalates(incident).contains("compound command (`for`)"));
    // The measured 2.1.280 box's line (lv/cap-rm.txt).
    assert!(
        escalates("S=$PWD/tmp; for p in a b; do set -- $p; rm -rf $S/$1; done")
            .contains("compound command")
    );
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
    // One operand outside a root fails the line.
    assert!(escalates("S=/private/tmp/claude-502/a; rm -rf \"$S/x\" /etc/y").contains("/etc/y"));
    // `&&` after a definite assignment keeps it; a comment is not code.
    approves("S=/tmp/work/a && rm -rf \"$S\" # tidy up; rm -rf /");
    approves("export S=/tmp/work/a; rm -r -- \"$S/x\"");
    approves("S=/tmp/work/a\nrm -rf \"$S/x\"");
    approves("ls /tmp/work; S=/tmp/work/a; rm -f $S/x.log >/dev/null 2>&1");
}

/// An assignment counts only where it surely runs.
#[test]
fn a_conditional_or_scoped_assignment_is_unknown() {
    assert!(escalates("test -d x || S=/tmp/w/a; rm -rf \"$S/y\"").contains("$S"));
    assert!(escalates("true && S=/tmp/w/a; rm -rf \"$S/y\"").contains("$S"));
    assert!(escalates("S=/tmp/w/a | cat; rm -rf \"$S/y\"").contains("$S"));
    assert!(escalates("S=/tmp/w/a & rm -rf \"$S/y\"").contains("$S"));
    assert!(escalates("S=/tmp/w/a rm -rf \"$S/y\"").contains("assignment on the rm"));
    assert!(escalates("S=/tmp/w/a ls; rm -rf \"$S/y\"").contains("$S"));
    // Negative control: the same assignment, unconditional.
    approves("S=/tmp/w/a; rm -rf \"$S/y\"");
}

/// Was `mktemp_resolves_under_tmpdir_and_may_not_glob`, whose `$TMPDIR` —
/// for `$TMPDIR/probe` and for the directory `mktemp -d` makes — was the
/// SUPERVISOR's, which the worker's Bash tool need not have (module header,
/// "Variables from the environment"). Of its three approvals on that
/// `$TMPDIR`: `rm -rf "$TMPDIR/probe"` escalates; `D=$(mktemp -d -t probe);
/// rm -rf "$D/x"` escalates (unguarded: were `mktemp` to fail it removes
/// `/x`), and is proven behind `&&` by the fresh-directory rule, as
/// `<mktemp -d>/x` — where it is is not claimed; `rm -rf "$D"` after
/// `D=$(mktemp -d)` stays proven (`"$D"` alone is `""` when it fails), as
/// `<mktemp -d>`. A literal template still resolves where it says.
#[test]
fn mktemp_is_proven_by_its_template_or_as_a_fresh_directory() {
    for (cmd, why) in [
        ("D=$(mktemp -d -t probe); rm -rf \"$D/x\"", "removes /x"),
        (
            "rm -rf \"$TMPDIR/probe\"",
            "$TMPDIR is not assigned on this line (a value from the environment",
        ),
    ] {
        let e = escalates(cmd);
        assert!(e.contains(why), "{cmd}: {e}");
    }
    assert_eq!(
        approves("D=$(mktemp -d -t probe) && rm -rf \"$D/x\""),
        ["<mktemp -d>/x"]
    );
    assert_eq!(approves("D=$(mktemp -d); rm -rf \"$D\""), ["<mktemp -d>"]);
    assert_eq!(
        approves("D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX); rm -rf \"$D\""),
        ["/private/tmp/claude-502/<mktemp>"]
    );
    assert!(
        escalates("D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX); rm -rf \"$D\"/*")
            .contains("glob behind a $(mktemp)")
    );
    assert!(escalates("D=$(mktemp); rm -rf \"$D\"").contains("$D"));
    assert!(escalates("D=$(mktemp -d -p /x); rm -rf \"$D\"").contains("$D"));
    assert!(escalates("D=$(pwd); rm -rf \"$D/x\"").contains("$D"));
}

#[test]
fn where_an_operand_may_point() {
    // The roots themselves, a glob at the root, `..`, `.git`, home, the cwd
    // and its ancestors, a glob that can match `..`.
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
    // Controls: past every critical path, and flags that only say how.
    approves("rm -rf /tmp/w/a/.gitignore");
    approves("rm -fv --one-file-system /tmp/w/a");
    // A scratch cwd is still not removable, nor its parent.
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
    // Negative controls: deeper paths in the same roots, and globs below them.
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

/// The Bash tool keeps a working directory of its own, which the worker
/// moves between calls; the supervisor knows only the session's launch
/// directory (lane B's review, 2026-09-23). So a relative operand and
/// `$PWD` escalate — whatever the launch directory would have made of them
/// — and an absolute spelling of the same path is the negative control.
#[test]
fn a_relative_operand_and_pwd_escalate() {
    assert!(escalates("rm -rf target/debug/x").contains("relative rm operand"));
    assert!(escalates("rm -rf ./target/x").contains("relative rm operand"));
    assert!(escalates("rm -rf \"$PWD/target/x\"").contains("$PWD"));
    assert!(escalates("S=$PWD/target; rm -rf \"$S/x\"").contains("$S"));
    assert!(escalates("rm -rf .").contains("relative rm operand"));
    approves("rm -rf /Users/_owner/aterm/target/x");
}

/// The default macOS volume is case-insensitive: `Rm` runs `/bin/rm`, so
/// an rm head is an rm in any letter case — judged, never passed over as
/// another program (negative control: the lower-case spelling resolves
/// the same).
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
        // Was `printf -v`, the resolver's own check, which duplicated the
        // classifier's with a weaker rule: the variable it names is unknown.
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
    assert_eq!(r.label(), "/w/proj/target*");
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

/// THE AUDIT OF 2026-09-26: the proven rule (`rm-breaker@v2`, approve-all
/// off, bypass on) APPROVED each of these, and each reaches `/etc`
/// (measured on this Mac's zsh 5.9 and bash 3.2 with `echo`, never `rm`):
/// a `$(mktemp -d)` that fails leaves `D` empty, so `"$D/etc"` is `/etc`;
/// zsh splits `$=S`, so `/tmp/x/$=S` with `S='y /etc'` is `/tmp/x/y` and
/// `/etc`; `$_` is the previous command's last argument — empty after
/// `_=/tmp/x` — so `"$_/etc"` is `/etc`. The fourth, a symbolic link out of
/// the scratch root, is `an_rm_through_a_symbolic_link_is_judged_where_it_leads`.
/// Negative controls: the same removals written so they are provable.
#[test]
fn the_audits_measured_approvals_escalate() {
    all_escalate(&[
        // The measured line, and with a literal template: the failed run.
        ("D=$(mktemp -d); rm -rf \"$D/etc\"", "removes /etc"),
        (
            "D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX); rm -rf \"$D/etc\"",
            "removes /etc",
        ),
        ("S='y /etc'; rm -rf /tmp/x/$=S", "`$=S`"),
        ("_=/tmp/x; rm -rf \"$_/etc\"", "_ is assigned"),
        (
            "rm -rf \"$_/etc\"",
            "$_ (a positional or special parameter)",
        ),
    ]);
    assert_eq!(
        approves("D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX) && rm -rf \"$D/etc\""),
        ["/private/tmp/claude-502/<mktemp>/etc"]
    );
    approves("S='y'; rm -rf /tmp/x/$S");
    approves("X=/tmp/x; rm -rf \"$X/etc\"");
}

/// A `$(mktemp -d …)` that FAILS leaves its variable empty (module header), so
/// every operand built on one is judged as the line runs when it fails —
/// the port of the 2026-09-24 audit's F1/F3 lines, each of which reached a
/// path the owner's ruling forbids — unless the rm cannot run with it
/// empty. Negative controls: each provable form.
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
        // An assignment exits with its LAST substitution's status (measured).
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
        // `export`'s status is its own; `${D?}` tests only unset (measured).
        (
            "export D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX) && rm -rf \"$D/x\"",
            "removes /x",
        ),
        (
            "D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX); rm -rf \"${D?}/x\"",
            "removes /x",
        ),
        // `${E:?}` guards E, and E is `/etc` when the mktemp fails; `${D:?}`
        // guards D, and D is `/x` when it fails.
        (
            "D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX); E=$D/etc; rm -rf \"${E:?}\"",
            "removes /etc",
        ),
        (
            "D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX)/x; E=${D:?}/y; rm -rf \"$E\"",
            "removes /x/y",
        ),
        // `${D:?}` is read where the shell expands it: `E=${D:?} D=…`
        // guards the OLD `D` (measured, both shells).
        (
            "D=/tmp/w/a; E=${D:?} D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX); rm -rf \"$D/etc\"",
            "removes /etc",
        ),
        (
            "D=/tmp/w/a; D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX) : \"${D:?}\"; rm -rf \"$D/etc\"",
            "is not assigned a literal",
        ),
        // A `${D:?}` that may not run guards nothing after it.
        (
            "D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX); true && : \"${D:?}\"; rm -rf \"$D/x\"",
            "removes /x",
        ),
        (
            "D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX); : \"${D:?}\" | cat; rm -rf \"$D/x\"",
            "removes /x",
        ),
        // `exit` that may not end the shell: backgrounded, or not the
        // whole right side of the `||`.
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
        // `test -n` with no operand is true (measured): an unquoted `$D`
        // proves nothing.
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
    // The variable alone is `""` when the run fails: nothing is removed.
    approves("D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX); rm -rf $D \"$D\"");
}

/// zsh computes what the lexer read as text (module header, measured):
/// `$=V` splits, `$~V` globs, `$^V` and `$+V` expand, a modifier or a
/// subscript on a bare name reshapes it (`"$S:h:h"` is `/tmp`, `$S[1,0]`
/// is empty), and a name in non-ASCII letters is a variable; `$[…]` is
/// arithmetic, which assigns. Negative controls: a `$` before a character
/// that opens no expansion, `${S}:h` (text in zsh too, measured), `${S}` and
/// a plain `$S`.
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
    // A `:` before anything but a modifier letter is text in zsh too
    // (measured: `"$S: x"`, `$S:/c`, `$S:1`), in an operand and in a word
    // another command is handed.
    assert_eq!(
        approves("S=/tmp/w/a; rm -rf \"$S: x\" $S:/c \"$S:1\""),
        ["/tmp/w/a: x", "/tmp/w/a:/c", "/tmp/w/a:1"]
    );
    approves("S=/tmp/w/a; echo \"$S: cleaning\"; rm -rf \"$S\"/*");
    approves("S=/tmp/w/a; rm -rf \"${S}/x\" $S/y");
}

/// A command the resolver does not model may assign a variable it names:
/// zsh's `print -v S /etc`, `zformat -f S /etc` set `S`, `zparseopts a:=S`
/// and `zstyle -s c s S` empty it (measured). So a variable a word names —
/// as the word expands — is unknown after the command, and every variable
/// is after a word whose text is not known. Negative controls: a command
/// that only USES the variable, and one naming another.
#[test]
fn a_variable_another_command_names_is_unknown_after_it() {
    all_escalate(&[
        (
            "S=/tmp/w/a; print -v S /etc; rm -rf \"$S/x\"",
            "$S is not known after",
        ),
        (
            "S=/tmp/w/a; print -rv S /etc; rm -rf \"$S/x\"",
            "$S is not known after",
        ),
        (
            "S=/tmp/w/a; zparseopts a:=S; rm -rf \"$S/etc\"",
            "$S is not known after",
        ),
        (
            "S=/tmp/w/a; zstyle -s :x nope S; rm -rf \"$S/etc\"",
            "$S is not known after",
        ),
        (
            "S=/tmp/w/a; zformat -f S /etc; rm -rf \"$S/x\"",
            "$S is not known after",
        ),
        // Named through a variable, a substitution, an unknown variable.
        (
            "S=/tmp/w/a; N=S; print -v $N /etc; rm -rf \"$S/x\"",
            "$S is not known after",
        ),
        (
            "S=/tmp/w/a; print -v $(echo S) /etc; rm -rf \"$S/x\"",
            "$S is not known after",
        ),
        (
            "S=/tmp/w/a; print -v $USER /etc; rm -rf \"$S/x\"",
            "$S is not known after",
        ),
        (
            "S=/tmp/w/a; print -v $=N /etc; rm -rf \"$S/x\"",
            "$S is not known after",
        ),
    ]);
    approves("S=/tmp/w/a; mkdir -p \"$S/x\" && ls $S; rm -rf \"$S/y\"");
    approves("S=/tmp/w/a; T=/tmp/w/b; print -v T /etc; rm -rf \"$S/y\"");
}

/// A name the shell itself sets, ties or reads is not a variable the line
/// states (module header; the 2026-09-24 audit's F2 lines, and `cd`, which
/// may fail or move `$PWD`): the line that assigns one is not followed,
/// however it assigns it — nor one that appends or assigns through a
/// subscript, nor a value with a `~` or `=` the shell expands. Negative
/// controls: names that only look like them, and the same values quoted.
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
        // `cd` moves `$PWD`, and may fail: the Bash tool's directory is
        // never the line's to know.
        ("cd /no/such/dir && rm -rf \"$PWD/x\"", "$PWD"),
        ("cd /tmp/ok; rm -rf x", "relative rm operand"),
        (
            "cd /tmp/ok && rm -rf \"$OLDPWD/x\"",
            "$OLDPWD is not assigned",
        ),
        // Appended to, or assigned through a subscript: `S+=/../../../etc`
        // makes `/tmp/w/a/../../../etc`, `S[0]=/etc` is `S=/etc` in bash.
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
        // `export` expands every word before it assigns any: `T` is `/b`
        // here (measured, both shells).
        (
            "export S=/tmp/w/a T=$S/b; rm -rf \"$T\"",
            "$T is not assigned",
        ),
        // `~` after a `:` is the home directory in both shells, `=ls` is
        // `/bin/ls` in zsh (measured).
        ("S=/tmp/w/a:~/x; rm -rf \"$S\"", "$S is not assigned"),
        ("S==ls; rm -rf \"/tmp/w/a/$S\"", "$S is not assigned"),
    ]);
    assert_eq!(approves("S_=/tmp/ok; rm -rf $S_/x"), ["/tmp/ok/x"]);
    assert_eq!(
        approves("PWD_OLD=/tmp/ok; rm -rf $PWD_OLD/x"),
        ["/tmp/ok/x"]
    );
    approves("LC_ALL=C sort /tmp/ok/f; S=/tmp/ok; rm -rf $S/y");
    // `TMPDIR` is no name the shell keeps: `mktemp` reads it, and the
    // directory it makes there is the line's own wherever that is.
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

/// bash runs a list that ends in `&` in a background subshell, so an
/// assignment in it is gone after it (measured: `S=/x && true & echo "$S"`
/// prints nothing in bash, `/x` in zsh) — the line never assigned `S` in
/// the shell the rm runs in. Negative control: the same list ended by `;`.
#[test]
fn an_assignment_in_a_backgrounded_list_is_not_the_shells() {
    all_escalate(&[
        ("S=/tmp/w/a && true & rm -rf \"$S/x\"", "$S is not assigned"),
        ("S=/tmp/w/a || true & rm -rf \"$S/x\"", "$S is not assigned"),
    ]);
    approves("S=/tmp/w/a && true; rm -rf \"$S/x\"");
}

/// A directory of the test's own under the process's temp dir, removed
/// (links as links: `remove_dir_all` never follows one) when dropped.
/// Nothing outside it is written; the resolver only DECIDES, nothing runs.
struct TempTree(PathBuf);

impl TempTree {
    fn new(tag: &str) -> Self {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "aterm-rm-proof-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the test's temp dir");
        Self(dir)
    }

    /// The path as `std::env::temp_dir` spells it (on macOS through the
    /// `/var` link), no trailing `/`.
    fn at(&self, rel: &str) -> String {
        let base = self.0.to_string_lossy().trim_end_matches('/').to_string();
        if rel.is_empty() {
            base
        } else {
            format!("{base}/{rel}")
        }
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `cmd` against the live disk with `roots` as the only scratch roots.
fn resolve_on_disk(roots: &[ScratchRoot], cwd: &str, cmd: &str) -> Result<Vec<String>, String> {
    let scope = RmScope {
        cwd: Path::new(cwd),
        home: Some(Path::new(HOME)),
        roots,
    };
    resolve_rm_line(cmd, &scope)
}

/// THE AUDIT'S FOURTH (2026-09-26): `S=/private/tmp/zzprobe; rm -rf
/// "$S/out/x"` with `out -> /etc` was approved — the resolver was lexical —
/// and rm follows a link in an operand's directories, so it removes
/// `/etc/x`. Built here with real links in a temp dir: a link out of the
/// root, the last component followed by a trailing `/` or `/.`, a relative
/// link that climbs out, a link to nowhere outside, a glob in a directory
/// (it may match a link). Negative controls: the link itself (rm removes the
/// link, never where it leads), a link that stays inside, absolute or
/// relative, a real directory, a glob in the last component.
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
    let roots: Vec<ScratchRoot> = ScratchRoot::dir(&t.0).into_iter().collect();
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
    // A link whose target climbs out of a directory that does not exist:
    // not judged lexically (the kernel would stop at the missing one, and a
    // directory made there later would change where it leads).
    symlink("missing/../../../../../../etc", t.at("p/climb")).expect("link");
    let e = run(&format!("rm -rf {}", t.at("p/climb/x"))).expect_err("climb");
    assert!(e.contains("climbs out of it"), "{e}");
    // Links that never end.
    symlink("b", t.at("p/a")).expect("link");
    symlink("a", t.at("p/b")).expect("link");
    let e = run(&format!("rm -rf {}", t.at("p/a/x"))).expect_err("a cycle");
    assert!(e.contains("more than 32 symbolic links"), "{e}");
    // A directory the disk will not let the resolver look into (not as root,
    // who reads every directory).
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

/// A root is judged where it leads too, and only through links root owns:
/// a scratch root swapped for a link the worker made holds nothing on the
/// disk (were it followed, a root re-pointed at `/usr` would approve
/// `/usr/local/x`). `/tmp/*` is `/private/tmp/*` on macOS, so a path spelled
/// through `/tmp` is inside it on the disk, and the session cwd spelled
/// through `/tmp` is the cwd however the operand spells it. Negative
/// control: the same removal under a real directory as the root.
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

/// THE FRESH-DIRECTORY RULE (module header, "A directory the line itself
/// just made"; the coordinator's ruling of 2026-09-26): `mktemp -d` makes a
/// new, uniquely named directory, so `"$D"` and a path lexically under it
/// remove nothing that predates the line — wherever `$TMPDIR` points, which
/// is neither read nor claimed (`<mktemp -d>`). Proven: any template (bare,
/// `-t`, relative, absolute — even outside every root), behind each guard
/// 72794ea13 recognises, every use of `$D` quoted. Negative controls, each
/// escalated: `$D` unquoted in the operand, `${D}` unquoted, `$D` unquoted
/// anywhere else on the line (a word, a `$(…)`), `D` reassigned between the
/// `mktemp` and the rm, `..` under it, a glob, text glued to its name (a
/// sibling), another variable under it, no guard (the failed-`mktemp`
/// path), a `mktemp` of a FILE (no `-d`), and the directory's name inside
/// another path.
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
        // Unguarded, the variable alone: `""` when the run fails.
        ("D=$(mktemp -d); rm -rf \"$D\"", vec!["<mktemp -d>"]),
        (
            "D=$(mktemp -d); ls \"$D\"; rm -rf \"$D\"",
            vec!["<mktemp -d>"],
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
        (
            "D=$(mktemp -d) && ls $D && rm -rf \"$D/x\"",
            "must be double-quoted",
        ),
        (
            "D=$(mktemp -d) || exit; X=$(ls $D); rm -rf \"$D/x\"",
            "must be double-quoted",
        ),
        // Reassigned between the `mktemp` and the rm: surely (to `/etc`), or
        // maybe (behind `&&`, so unknown).
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
        // Under it, even by a `/` alone: `/` when the run fails.
        ("D=$(mktemp -d); rm -rf \"$D/\"", "the rm then removes /;"),
        ("D=$(mktemp -d); rm -rf \"$D/.\"", "the rm then removes /.;"),
        (
            "D=$(mktemp) && rm -rf \"$D/x\"",
            "$D is not assigned a literal",
        ),
        (
            "S=/tmp/w; D=$(mktemp -d) && rm -rf \"$S/$D\"",
            "proven only as \"$D\" or a path under it",
        ),
    ]);
}

/// VARIABLES FROM THE ENVIRONMENT (module header; the coordinator's ruling
/// of 2026-09-26, which deleted the reader of Claude Code's `exec`
/// environment): a variable the line does not assign is not proven, directly
/// or through an assignment — whatever this process (the supervisor, here)
/// holds for it. Negative control: the same lines assigning it.
#[test]
fn a_variable_from_the_environment_is_not_proven() {
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

/// An assignment in front of a command (module header): zsh expands
/// `export`'s own words with it in force and bash keeps it after `export` —
/// measured below with `echo` in both shells, where installed — so the
/// names such a command assigns are unknown after it, and it guards nothing
/// (`D=/x export F="${D:?}"` tests the prefix in zsh, not the `D` a copy
/// made before still holds). The same for the other declaring builtins
/// (`typeset`, `declare`, `readonly`, `local`, and zsh's `integer`
/// and `float`, are refused outright). Negative controls: the same commands
/// without the prefix.
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
        // A copy of `D` made before: the guard tested the prefix.
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
    // The two shells' readings, with `echo`: each form lands on `/etc` in
    // at least one of them.
    let run = |shell: &str, line: &str| -> Option<String> {
        let out = std::process::Command::new(shell)
            .args(no_startup(shell, line))
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .output()
            .ok()?;
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    for (line, what) in [
        (format!("{s}; S=/etc export T=$S; echo \"$T\""), "T"),
        (format!("{s}; S=/etc export S; echo \"$S\""), "S"),
    ] {
        let readings: Vec<String> = ["/bin/zsh", "/bin/bash"]
            .iter()
            .filter(|sh| Path::new(sh).exists())
            .filter_map(|sh| run(sh, &line))
            .collect();
        if readings.len() == 2 {
            assert!(readings.iter().any(|r| r == "/etc"), "{what}: {readings:?}");
            assert!(
                readings.iter().any(|r| r != "/etc"),
                "the shells disagree: {readings:?}"
            );
        }
    }
}

/// Which words name a variable (`Eval::forget_named`; the review of
/// 2026-09-26): a lone option cluster names only what is glued after its
/// first letter, and a path word with no arithmetic operator names nothing —
/// so `[ -d "$d" ]`, `ls -f "$f"`, `ls "$out"` (whose value ends in `/out`),
/// `du -sh "$build"` leave `d`, `f`, `out`, `build` known, as upstream
/// proved. Negative controls, each still forgotten: arithmetic zsh
/// evaluates (`[ -t "1/(S=5)" ]`, `test -t "1/S--"`, `[ -t -S-- ]`,
/// `printf %d "S=5"`), a name after an option (`stat -A S`), a subscript
/// (`print -v 'S[1]'`), and a name glued to its option (`print -vS`,
/// `print -rvS`, bash's `printf -vS`).
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
    let s = "S=/tmp/w";
    for middle in [
        "[ -t \"1/(S=5)\" ]",
        "test -t \"1/S--\"",
        "[ -t -S-- ]",
        "printf %d \"S=5\"",
        "stat -A S /etc",
        "print -v 'S[1]' x",
        "print -vS /etc",
        "print -rvS /etc",
        "printf -vS /etc",
    ] {
        let cmd = format!("{s}; {middle}; rm -rf \"$S/x\"");
        assert!(escalates(&cmd).contains("$S is not known after"), "{cmd}");
    }
}

/// A word the resolver cannot compute forgets every variable only where it
/// could name one or run code (`Eval::forget_named`; the review of
/// 2026-09-26): a double-quoted unknown variable or `$(…)` handed to a
/// command that takes no word as a name or as arithmetic (`ls`, `echo`, …)
/// forgets nothing. Negative controls, each still forgetting: a zsh form (a
/// glob qualifier in `$~X` runs code: `X='/tmp/*(e:S=/etc:)'; echo $~X` sets
/// `S`, measured), an unquoted unknown variable or `$(…)`, an unquoted value
/// holding a glob (the same under `GLOB_SUBST`), and a quoted unknown word
/// handed to `print`, `printf` or `[`.
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
    for middle in [
        "X='/tmp/*(e:S=/etc:)'; echo $~X",
        // A builtin's redirect is globbed in this shell, qualifier and all
        // (`echo hi < $~X` and `true < $~X` set `S`; `ls < $~X` does not —
        // measured, `zsh -f`): the redirect's words are read too.
        "X='/tmp/*(e:S=/etc:)'; echo hi < $~X",
        "X='/tmp/*(e:S=/etc:)'; true < $~X",
        "echo $X",
        "X='/tmp/*(e:S=/etc:)'; echo $X",
        // The value names no `S`; its qualifier's code would, under
        // `GLOB_SUBST`: an unquoted value with a glob forgets all.
        "Y='S=/etc'; X='/tmp/*(e:eval $Y:)'; echo $X",
        "echo $(date)",
        "print \"$X\" S /etc",
        "printf %d \"$X\"",
        "[ -t \"$X\" ]",
    ] {
        let cmd = format!("{s}; {middle}; rm -rf \"$S/x\"");
        assert!(escalates(&cmd).contains("$S is not known after"), "{cmd}");
    }
}

/// The assignment rule's `surely` for `export` and the pipe's `made` (the
/// review of 2026-09-26: both survived mutation with every test green). An
/// `export` that may not run assigns nothing the line can rely on, and a
/// command after `|` runs whatever the one before it returned, so `test -n
/// "$D" |` proves nothing. Negative controls: the unconditional `export`,
/// and `test -n "$D" &&`.
#[test]
fn a_conditional_export_and_a_pipe_prove_nothing() {
    all_escalate(&[
        ("test -d x || export S=/tmp/w/a; rm -rf \"$S/y\"", "$S"),
        ("true && export S=/tmp/w/a; rm -rf \"$S/y\"", "$S"),
        ("export S=/tmp/w/a | cat; rm -rf \"$S/y\"", "$S"),
        ("export S=/tmp/w/a & rm -rf \"$S/y\"", "$S"),
        (
            "D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX); test -n \"$D\" | rm -rf \"$D/x\"",
            "removes /x",
        ),
        (
            "D=$(mktemp -d); test -n \"$D\" | rm -rf \"$D/x\"",
            "removes /x",
        ),
    ]);
    approves("export S=/tmp/w/a; rm -rf \"$S/y\"");
    approves("D=$(mktemp -d); test -n \"$D\" && rm -rf \"$D/x\"");
}

/// SHELL_NAMES keeps every name whose assignment does not read back — the
/// shell keeps it itself, or refuses it — measured here in zsh and bash,
/// where installed: each name the shell reports (`${(k)parameters}`,
/// `compgen -v`) is assigned `/x/y` in a subshell and read back.
#[cfg(unix)]
#[test]
fn every_name_whose_assignment_does_not_read_back_is_kept() {
    let probes = [
        (
            "/bin/zsh",
            "for n in ${(k)parameters}; do [[ $n =~ '^[A-Za-z_][A-Za-z0-9_]*$' ]] || continue; \
             ( eval \"$n=/x/y\" 2>/dev/null; [[ ${(P)n} == /x/y ]] ) 2>/dev/null || print -r -- $n; \
             done",
        ),
        (
            "/bin/bash",
            "for n in $(compgen -v); do ( eval \"$n=/x/y\" 2>/dev/null; \
             [ \"${!n}\" = /x/y ] ) 2>/dev/null || echo \"$n\"; done",
        ),
    ];
    let mut measured = 0;
    for (shell, probe) in probes {
        if !Path::new(shell).exists() {
            continue;
        }
        let out = std::process::Command::new(shell)
            .args(no_startup(shell, probe))
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .output()
            .expect("the shell runs");
        let kept: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .filter(|n| !shell_name(n))
            .collect();
        assert!(kept.is_empty(), "{shell}: not in SHELL_NAMES: {kept:?}");
        measured += String::from_utf8_lossy(&out.stdout).lines().count();
    }
    if Path::new("/bin/zsh").exists() {
        assert!(
            measured > 20,
            "the probe found the shell's own names ({measured})"
        );
    }
}

/// `shell -c line` with no startup file of the user's read: zsh's `-f`
/// (`NO_RCS`: `~/.zshenv` and the rest are skipped; the environment is
/// cleared by the caller), bash's `-c` reads none (`BASH_ENV` is cleared).
fn no_startup<'a>(shell: &str, line: &'a str) -> Vec<&'a str> {
    if shell.ends_with("zsh") {
        vec!["-f", "-c", line]
    } else {
        vec!["-c", line]
    }
}

/// `line` in `shell` when it is installed, in a temp dir of the test's own,
/// no startup file read, stdin closed: its stdout, trimmed.
fn in_shell(shell: &str, line: &str) -> Option<String> {
    if !Path::new(shell).exists() {
        return None;
    }
    let t = TempTree::new("shell");
    let out = std::process::Command::new(shell)
        .args(no_startup(shell, line))
        .current_dir(&t.0)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// THE ROUND-2 REVIEW (2026-09-26, medium): the forgetting that closed
/// `print -v S` also forgot every variable a PLAIN command's words named,
/// so lines the lexical resolver proved escalated — `echo "OUT=$OUT"` names
/// `OUT`. A [`PLAIN_CONSUMERS`] command assigns no variable, so its words
/// name nothing. Negative controls: the same words under a command that can
/// assign or evaluate them still forget (`a_command_that_can_assign_…`).
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

/// With [`PLAIN_CONSUMERS`]' words naming nothing, its membership is a
/// soundness law: a command that CAN assign a variable it names, or read a
/// word as arithmetic, is not in it, and forgets. Each head below assigns
/// `S` from a word in zsh 5.9 or bash 3.2 (the zsh modules' by their
/// manuals: `zsh/stat`, `zsh/datetime`, `zsh/system`, `zsh/zselect`), and
/// each is refused or forgets `S`. Measured in the shells: no builtin
/// member of [`PLAIN_CONSUMERS`] assigns a variable its words name.
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
    // Each builtin member, handed words a builtin could take as a name, an
    // option's name or arithmetic, leaves `S` as it was. External members
    // cannot touch the shell's variables; they are not run.
    for member in PLAIN_CONSUMERS {
        let zsh = format!(
            "S=/x/y; if [[ $(whence -w -- '{member}') == *': builtin' ]]; then \
             '{member}' S -v S -A S S=5 -t 'a[S=5]' >/dev/null 2>&1; fi; print -r -- \"$S\""
        );
        let bash = format!(
            "S=/x/y; if [ \"$(type -t -- '{member}')\" = builtin ]; then \
             '{member}' S -v S -A S S=5 -t 'a[S=5]' >/dev/null 2>&1; fi; echo \"$S\""
        );
        for (sh, line) in [("/bin/zsh", zsh), ("/bin/bash", bash)] {
            if let Some(out) = in_shell(sh, &line) {
                assert_eq!(out, "/x/y", "{sh}: {member}");
            }
        }
    }
}

/// zsh's arithmetic reads a variable's VALUE as arithmetic in turn: `A=S=5;
/// printf %d A` sets `S` (measured below, where zsh is installed). So a
/// word an arithmetic command reads forgets what its variables' values
/// name, and a name whose value the line does not know (the environment's)
/// forgets every variable. Negative controls: a word that starts with `/`
/// (the parse stops there: `printf %d /A` reads nothing, measured), the
/// format of `printf`, a word `[` does not read as arithmetic, a plain
/// command.
#[test]
fn a_value_zsh_arithmetic_reads_is_read_too() {
    all_escalate(&[
        (
            "S=/tmp/w/a; A=S=5; printf %d A; rm -rf \"$S/x\"",
            "$S is not known after",
        ),
        (
            "S=/tmp/w/a; A=B; B=S=5; [ -t A ]; rm -rf \"$S/x\"",
            "$S is not known after",
        ),
        (
            "S=/tmp/w/a; A=S=5; printf '%s %d' x 1/A; rm -rf \"$S/x\"",
            "$S is not known after",
        ),
        (
            "S=/tmp/w/a; printf %d HOSTX; rm -rf \"$S/x\"",
            "$S is not known after",
        ),
        (
            "S=/tmp/w/a; test -t 1/UNSETX; rm -rf \"$S/x\"",
            "$S is not known after",
        ),
        (
            "S=/tmp/w/a; A=S=5; print -f %d A; rm -rf \"$S/x\"",
            "$S is not known after",
        ),
        // `print`'s options that take a word do not end its options.
        (
            "S=/tmp/w/a; A=S=5; print -v T -f %d A; rm -rf \"$S/x\"",
            "$S is not known after",
        ),
        (
            "S=/tmp/w/a; A=S=5; print -u 2 -f %d A; rm -rf \"$S/x\"",
            "$S is not known after",
        ),
        (
            "S=/tmp/w/a; A=S=5; return A; rm -rf \"$S/x\"",
            "$S is not known after",
        ),
        (
            "S=/tmp/w/a; A=S=5; printf -- %d A; rm -rf \"$S/x\"",
            "$S is not known after",
        ),
        // The value, not the name, is what reads `S`.
        (
            "S=/tmp/w/a; A=x/S; printf %d A; rm -rf \"$S/x\"",
            "$S is not known after",
        ),
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
    if let Some(out) = in_shell(
        "/bin/zsh",
        "A=S=5; S=/tmp/w/a; printf %d A >/dev/null 2>&1; B=S=6; printf %d /B >/dev/null \
         2>&1; echo \"$S\"",
    ) {
        assert_eq!(
            out, "5",
            "zsh reads A's value as arithmetic, and /B not at all"
        );
    }
    if let Some(out) = in_shell(
        "/bin/zsh",
        "A=S=5; S=/tmp/w/a; print -u 2 -f %d A 2>/dev/null; echo \"$S\"",
    ) {
        assert_eq!(out, "5", "zsh's print -f follows `-u 2`");
    }
}

/// The builtins that change how every later word reads, and zsh's
/// `integer`/`float`, whose values are arithmetic (`integer T=A` reads
/// `A`'s value): a line with one is not followed. Negative control: the
/// line without it.
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

/// The measured mutation survivors of the round-2 review (2026-09-26):
/// each test below fails with the rule it names weakened.
///
/// zsh's modifiers: after `$NAME:`, every letter of zsh's documented set
/// reshapes the value (`"$S:s/w/etc/"` is `/tmp/etc/a`, `$S:P` resolves
/// links, `$S:a`, `$S:r` — measured), so each is read as an expansion the
/// resolver does not compute. The letters are spelled here, not read off
/// [`ZSH_MODIFIERS`]; and where zsh is installed, every letter whose
/// reading differs from the text is checked to escalate.
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
    all_escalate(&[
        ("S=/tmp/w/a; rm -rf \"$S:s/w/etc/\"", "`$S:s`"),
        ("S=/tmp/w/a; rm -rf \"$S:P/x\"", "`$S:P`"),
        ("S=/tmp/w/a.b; rm -rf \"$S:r\"", "`$S:r`"),
        ("S=/tmp/w/a; rm -rf \"$S:a/x\"", "`$S:a`"),
    ]);
    // zsh's own reading of each letter, where it differs from the text
    // (each in a subshell: a modifier zsh rejects ends only that one).
    let probe: String = letters
        .chars()
        .map(|m| format!("(print -r -- \"{m} $S:{m}\") 2>/dev/null; "))
        .collect();
    if let Some(out) = in_shell("/bin/zsh", &format!("S=/tmp/w/a.b; {probe}")) {
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
}

/// A path word names nothing only when no arithmetic can assign in it: a
/// `=`, `(`, `[` or `++` in it (as `--`) makes it a word a command the
/// resolver does not model may evaluate (`x/S=5`, `x/(S=5)`, `x/a[S=5]`,
/// `x/S++` each assign `S` as arithmetic). Negative control: a plain path.
#[test]
fn a_path_word_with_an_arithmetic_operator_names_its_variables() {
    for word in ["x/S=5", "x/(S)", "x/a[S]", "x/S++", "x/S--"] {
        let cmd = format!("S=/tmp/w/a; zcompute '{word}'; rm -rf \"$S/x\"");
        assert!(escalates(&cmd).contains("$S is not known after"), "{cmd}");
    }
    approves("S=/tmp/w/a; zcompute 'x/S'; rm -rf \"$S/x\"");
}

/// A double-quoted `$(…)` names nothing only under a [`PLAIN_CONSUMERS`]
/// command: under any other its output may be the name a builtin assigns
/// (`print -v "$(echo S)" /etc` sets `S`). Negative control: the same under
/// `echo`.
#[test]
fn a_quoted_substitution_under_another_command_forgets() {
    for middle in [
        "print -v \"$(echo S)\" /etc",
        "printf -v \"$(echo S)\" /etc",
        "[ -t \"$(echo S)\" ]",
        "stat -A \"$(echo S)\" /etc",
    ] {
        let cmd = format!("S=/tmp/w/a; {middle}; rm -rf \"$S/x\"");
        assert!(escalates(&cmd).contains("$S is not known after"), "{cmd}");
    }
    approves("S=/tmp/w/a; echo \"$(echo S)\"; rm -rf \"$S/x\"");
}

/// OWNER DECISION D9 (b96c9f035): the retired `rm_breaker = false` leaves
/// the rule no scratch root, and then nothing is proven — the fresh-
/// directory rule, which asks no root, included (the round-2 challenge: it
/// would have lifted the owner's limit). Negative control: the same lines
/// with the roots.
#[test]
fn nothing_is_proven_with_no_scratch_root() {
    let none: Vec<ScratchRoot> = Vec::new();
    for cmd in [
        "D=$(mktemp -d) && rm -rf \"$D/x\"",
        "D=$(mktemp -d); rm -rf \"$D\"",
        "D=$(mktemp -d w.XXXXXX) || exit 1; rm -rf \"$D/x\"",
    ] {
        let e = resolve_on_disk(&none, CWD, cmd).expect_err(cmd);
        assert!(e.contains("withheld"), "{cmd}: {e}");
        assert!(resolve(cmd).is_ok(), "{cmd}");
    }
    let e = resolve_on_disk(&none, CWD, "S=/tmp/w/a; rm -rf \"$S/x\"").expect_err("literal");
    assert!(e.contains("not strictly inside"), "{e}");
}

/// THE ROUND-2 REVIEW (low): a session cwd reached through a link of the
/// user's dropped the whole `<cwd>/target*` root on the disk, so nothing
/// under the build directory proved. That root is resolved where the cwd
/// leads, as the cwd check is. Negative controls: a fixed root through the
/// same link is still no root (`a_root_is_judged_where_it_leads…`), and the
/// cwd itself, spelled either way, is still refused.
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

/// THE PORT'S REVIEW (2026-09-27, medium): zsh's `printf` reads its
/// arguments as arithmetic only under a numeric conversion or a `*` width
/// or precision (measured below: `%s`, `%b`, `%q`, `%c`, `%1$s`, `%%d` and a
/// format with no conversion leave `S` after `printf FMT S=5`; `%d`, `%*s`
/// and `%.*s` set it), and its format names nothing (only `-v NAME`
/// assigns) — so `printf '%s\n' done` and `printf 'OUT=%s\n' "$OUT"`, which
/// the lexical resolver proved, prove again. Negative controls: a numeric
/// conversion anywhere in the format, a length modifier or a conversion
/// this reading does not know, `-v`, and a format whose word the shell may
/// split, remove, glob or brace-expand into an option (`$E` empty, `{-v,S}`,
/// `%s*`), each still forgetting `S`.
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
        // A lone `-` is the format (zsh leaves `S`, measured below).
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
        // THE FINAL CHECK (2026-09-27, high): zsh's printf knows only
        // `-v`; any other `-` word is its FORMAT, so `-%d` reads `S=5`.
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
    if let Some(out) = in_shell("/bin/zsh", probe) {
        assert_eq!(
            out, "1\n5\n6",
            "zsh's printf reads arithmetic only where it says"
        );
    }
    let probe = "S=1; printf - %d S=5 >/dev/null 2>&1; print -r -- \"$S\"; \
                 printf -%d S=2 >/dev/null 2>&1; print -r -- \"$S\"; \
                 printf -x%d S=3 >/dev/null 2>&1; print -r -- \"$S\"; \
                 printf -v T -%d S=4 >/dev/null 2>&1; print -r -- \"$S $T\"";
    if let Some(out) = in_shell("/bin/zsh", probe) {
        assert_eq!(
            out, "1\n2\n3\n4 -4",
            "zsh's printf takes a `-` word other than `-v` as its format"
        );
    }
}

/// THE PORT'S REVIEW (2026-09-27, medium): a program run from the disk
/// cannot assign the shell's variables, so its words name nothing — the
/// read-only external programs the classifier accepts are
/// [`PLAIN_CONSUMERS`], and lines the lexical resolver proved
/// (`shasum "$HOME/f"`, `jq … "$TMPDIR/p.json"`, `awk -v S=1`) prove again.
/// Each one added here is measured to be no builtin in zsh 5.9 (`zsh -f`)
/// or bash 3.2 where they are installed. Negative controls: `stat` (zsh's
/// `zsh/stat` builtin assigns with `-A`), `env` and `xargs` (they run a
/// command), `print`, and an unquoted unknown word, which the shell itself
/// may glob (a qualifier runs code) whatever the program.
#[test]
fn an_external_read_only_program_names_nothing() {
    let externals = [
        "git",
        "egrep",
        "fgrep",
        "mdfind",
        "sed",
        "awk",
        "tr",
        "jq",
        "comm",
        "nl",
        "md5",
        "shasum",
        "sha256sum",
        "column",
        "paste",
        "seq",
        "strings",
        "fold",
        "expr",
        "uname",
        "whoami",
        "id",
        "sw_vers",
        "hostname",
        "df",
        "uptime",
        "sysctl",
        "ps",
        "lsof",
        "pgrep",
        "sleep",
    ];
    for name in externals {
        assert!(PLAIN_CONSUMERS.contains(&name), "{name}");
        let zsh = format!("whence -w -- '{name}'");
        if let Some(out) = in_shell("/bin/zsh", &zsh) {
            assert!(
                out.ends_with(": command") || out.ends_with(": none"),
                "zsh: {out}"
            );
        }
        let bash = format!("type -t -- '{name}'");
        if let Some(out) = in_shell("/bin/bash", &bash) {
            assert!(out == "file" || out.is_empty(), "bash: {name} is {out}");
        }
    }
    for middle in [
        "shasum \"$HOME/f\"",
        "jq -r .name \"$TMPDIR/p.json\"",
        "tr a b < \"$HOME/f\"",
        "awk -v S=1 '{print}' \"$S/f\"",
        "sed -n 's/S=//p' \"$HOME/f\"",
        "git -C \"$HOME/src\" log -1 --format=S=%s",
        "nl \"$(date)\"",
    ] {
        let cmd = format!("S=/private/tmp/claude-502/w; {middle}; rm -rf \"$S/x\"");
        assert_eq!(
            resolve(&cmd),
            Ok(vec!["/private/tmp/claude-502/w/x".to_string()]),
            "{cmd}"
        );
    }
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

/// THE PORT'S REVIEW (2026-09-27, low): the operand `"$D"` alone is the
/// fresh directory, or `""` when the `mktemp` failed — which removes
/// nothing — whatever another command did with an unquoted `$D`: a split or
/// globbed `$D` elsewhere changes no word of the rm's. So only a path
/// UNDER `$D` asks every use to be double-quoted. Negative controls: a
/// path under an unquoted-elsewhere `$D`, and an unquoted rm operand.
#[test]
fn a_quoted_fresh_directory_alone_is_proven_whatever_else_reads_it() {
    for cmd in [
        "D=$(mktemp -d); ls $D; rm -rf \"$D\"",
        "D=$(mktemp -d) && cd $D && rm -rf \"$D\"",
    ] {
        assert_eq!(resolve(cmd), Ok(vec!["<mktemp -d>".to_string()]), "{cmd}");
    }
    all_escalate(&[
        (
            "D=$(mktemp -d) && ls $D && rm -rf \"$D/x\"",
            "must be double-quoted",
        ),
        ("D=$(mktemp -d) && rm -rf $D", "must be double-quoted"),
        (
            "D=$(mktemp -d) && ls $D && rm -rf \"${D:?}/x\"",
            "must be double-quoted",
        ),
        // Inside `$(…)` quoting is not read, braces or not.
        (
            "D=$(mktemp -d) && echo \"$(ls ${D})\" && rm -rf \"$D/x\"",
            "must be double-quoted",
        ),
        (
            "D=$(mktemp -d) && echo \"$(ls $D)\" && rm -rf \"$D/x\"",
            "must be double-quoted",
        ),
    ]);
}

/// THE PORT'S REVIEW (2026-09-27, low): a variable the line assigned a
/// literal, then forgot after a command that may assign it, is reported as
/// that — not as "not assigned a literal on this line", which the owner
/// reading the badge sees to be false. Negative controls: a variable the
/// line never assigns, and one it assigns only where the assignment may not
/// run, keep their own reasons.
#[test]
fn a_forgotten_variable_names_the_command_that_forgot_it() {
    for (middle, head) in [
        ("print -v S /etc", "print"),
        ("A=S=5; printf %d A", "printf"),
        ("[ -t \"$X\" ]", "["),
        ("X='/tmp/*(e:S=/etc:)'; echo $~X", "echo"),
    ] {
        let cmd = format!("S=/tmp/w/a; {middle}; rm -rf \"$S/x\"");
        let e = escalates(&cmd);
        assert!(
            e.starts_with(&format!("$S is not known after `{head}`")),
            "{cmd}: {e}"
        );
    }
    assert!(escalates("rm -rf \"$UNSETX/x\"").contains("$UNSETX is not assigned on this line"));
    assert!(
        escalates("test -d x || S=/tmp/w/a; rm -rf \"$S/x\"")
            .contains("$S is not assigned a literal on this line")
    );
}

/// THE FINAL CHECK (2026-09-27, high): zsh's `{S}>file` opens a descriptor
/// and stores its number in `S` — in the shell itself under a builtin
/// (`echo`, `:`, `pwd`, `printf`), even with a blank before the `>`
/// (measured below) — so `rm -rf "$S/x"` removed a directory named for
/// that number (`11/x` measured; it depends on the descriptors open) in
/// the Bash tool's working directory. An unquoted `{NAME}` word forgets
/// `NAME` under every command, a plain one's and `printf`'s format's
/// included. Negative controls: a quoted or escaped brace (zsh leaves `S`,
/// measured), and another name.
#[test]
fn a_named_descriptor_redirect_assigns_its_name() {
    all_escalate(&[
        (
            "S=/private/tmp/claude-502/w; echo hi {S}>/dev/null; rm -rf \"$S/x\"",
            "$S is not known after `echo`",
        ),
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
    for cmd in [
        "S=/private/tmp/claude-502/w; echo '{S}'>/dev/null; rm -rf \"$S/x\"",
        "S=/private/tmp/claude-502/w; echo \\{S}>/dev/null; rm -rf \"$S/x\"",
        "S=/private/tmp/claude-502/w; echo hi {T}>/dev/null; rm -rf \"$S/x\"",
    ] {
        assert_eq!(
            resolve(cmd),
            Ok(vec!["/private/tmp/claude-502/w/x".to_string()]),
            "{cmd}"
        );
    }
    let probe = "S=/x; T=/y; U=/z; V=/v; echo hi {S}>/dev/null; ls {T}>/dev/null; \
                 echo '{U}'>/dev/null; echo hi {V} >/dev/null; print -r -- \"$S $T $U $V\"";
    if let Some(out) = in_shell("/bin/zsh", probe) {
        // The descriptor is a new one: `echo` still prints to stdout.
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
}

/// THE RECHECK (2026-09-27, high): the positions the rule reads — `printf`'s
/// options, format and arguments, `-t`'s operand — are the words as
/// WRITTEN, and brace expansion moves them. zsh expands `A=S=5; printf
/// {%d,A}` to `printf %d A` and sets `S`; bash expands `printf {-vS,%s}
/// /etc` and `printf -{vS,-} /etc` to `printf -vS …` and sets `S=/etc`; zsh
/// runs `[ {-t,A} ]` as `[ -t A ]` (measured below). Each was proven, so the
/// rm removed `5/x` or `/etc/x`. A word with an unquoted `{` now forgets
/// every variable under a command that reads its words, unless it is a
/// data word past `printf`'s as-written format that reads no arithmetic.
/// Negative controls: a quoted or escaped brace, brace-expanded data after
/// a `%s` format, and a plain command's brace word.
#[test]
fn a_brace_expanded_word_moves_the_words_the_rule_reads() {
    for middle in [
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
    ] {
        let cmd = format!("S=/private/tmp/claude-502/w; {middle}; rm -rf \"$S/x\"");
        assert!(escalates(&cmd).contains("$S is not known after"), "{cmd}");
    }
    for middle in [
        "printf '{%s}\\n' x",
        "printf '%s\\n' {a,b}",
        "printf '%s\\n' S={5,6}",
        "printf \\{-vS,%s} /etc",
        "echo {-vS,%s} /etc",
    ] {
        let cmd = format!("S=/private/tmp/claude-502/w; {middle}; rm -rf \"$S/x\"");
        assert_eq!(
            resolve(&cmd),
            Ok(vec!["/private/tmp/claude-502/w/x".to_string()]),
            "{cmd}"
        );
    }
    let probe = "S=1; A=S=5; printf {%d,A} >/dev/null; print -r -- \"$S\"; \
                 S=1; A=S=6; [ {-t,A} ]; print -r -- \"$S\"; \
                 S=1; printf '%s' S={7,8} >/dev/null; print -r -- \"$S\"";
    if let Some(out) = in_shell("/bin/zsh", probe) {
        assert_eq!(out, "5\n6\n1", "zsh reads the words brace expansion made");
    }
    let probe = "S=1; printf {-vS,%s} /etc; echo \"$S\"; T=1; printf -{vT,-} /x; echo \"$T\"";
    if let Some(out) = in_shell("/bin/bash", probe) {
        assert_eq!(out, "/etc\n/x", "bash's printf -v made by brace expansion");
    }
}

/// THE RECHECK (2026-09-27, high): a glob is a word the shell makes other
/// words of, from the names in the working directory. With a file named
/// `S=5` there, zsh runs `printf %d *`, `printf %d S=?` and `[ -t * ]` as
/// arithmetic on `S=5` and sets `S`, and with files `-t` and `S=5`, `[ * ]`
/// is `[ -t S=5 ]` (measured, zsh 5.9 -f, in a scratch directory holding
/// those files). Every version proved them. A word with an unquoted glob
/// now forgets every variable where a brace word does
/// (`a_brace_expanded_word_moves_the_words_the_rule_reads`). Negative
/// controls: a quoted glob, globbed data after a `%s` format, a plain
/// command's glob.
#[test]
fn a_globbed_word_moves_the_words_the_rule_reads() {
    for middle in [
        "printf %d *",
        "printf %d S=?",
        "printf '%s %d' x S*",
        "[ -t * ]",
        "[ * ]",
        "test -t S=[5]",
        "printf * S=5",
        "printf -* S=5",
    ] {
        let cmd = format!("S=/private/tmp/claude-502/w; {middle}; rm -rf \"$S/x\"");
        assert!(escalates(&cmd).contains("$S is not known after"), "{cmd}");
    }
    for middle in [
        "printf '%s\\n' *.log",
        "printf %d '*'",
        "printf %d \\*",
        "ls *",
        "echo S=?",
    ] {
        let cmd = format!("S=/private/tmp/claude-502/w; {middle}; rm -rf \"$S/x\"");
        assert_eq!(
            resolve(&cmd),
            Ok(vec!["/private/tmp/claude-502/w/x".to_string()]),
            "{cmd}"
        );
    }
}

/// A glob or brace word that starts with `/` expands to absolute paths
/// only — no option, no name, no arithmetic (`printf %d /tmp/w/*` and `[
/// /tmp/w/* ]` beside files `-t`, `-vS` and `S=5` there leave `S`, and zsh
/// runs nothing on no match: measured, zsh 5.9 -f and bash 3.2) — so the
/// rule keeps proving past one, as upstream did: `stat -f %z "$S"/*.bin`.
/// Negative controls: a relative glob, and a value with a blank, which
/// bash splits.
#[test]
fn an_absolute_glob_moves_no_word_the_rule_reads() {
    for middle in [
        "stat -f %z \"$S\"/*.bin",
        "stat -f %z $S/{a,b}.bin",
        "printf %d /tmp/w/*",
        "[ -e /tmp/w/*.log ]",
        "test -t 1 /tmp/w/*",
    ] {
        let cmd = format!("S=/private/tmp/claude-502/w; {middle}; rm -rf \"$S/x\"");
        assert_eq!(
            resolve(&cmd),
            Ok(vec!["/private/tmp/claude-502/w/x".to_string()]),
            "{cmd}"
        );
    }
    for middle in [
        "stat -f %z ./*.bin",
        "T='/tmp/w -vS'; printf %d \"$T\"/*",
        "printf %d *",
    ] {
        let cmd = format!("S=/private/tmp/claude-502/w; {middle}; rm -rf \"$S/x\"");
        assert!(escalates(&cmd).contains("$S is not known after"), "{cmd}");
    }
}

/// THE RECHECK (2026-09-27, low): a `-` word zsh takes as `printf`'s
/// format was read as numeric whatever it said, so `printf '--- %s ---\n'
/// done` forgot every variable. It is read as a format now: zsh evaluates
/// nothing under `--- %s ---` or `-%s`, and bash refuses either as an
/// unknown option, assigning nothing (measured below). Negative controls:
/// a numeric `-` format, and `-vS`, which bash reads as `-v S`.
#[test]
fn a_dash_format_with_no_numeric_conversion_reads_no_arithmetic() {
    for middle in [
        "printf '--- %s ---\\n' done",
        "printf -- '--- %s ---\\n' done",
        "printf -%s S=5",
        "printf -v T '--- %s' S=5",
    ] {
        let cmd = format!("S=/private/tmp/claude-502/w; {middle}; rm -rf \"$S/x\"");
        assert_eq!(
            resolve(&cmd),
            Ok(vec!["/private/tmp/claude-502/w/x".to_string()]),
            "{cmd}"
        );
    }
    for middle in [
        "printf '--- %d ---\\n' S=5",
        "printf -x%d S=5",
        "printf -vS '%s' /etc",
        "printf -vT -vS x",
    ] {
        let cmd = format!("S=/private/tmp/claude-502/w; {middle}; rm -rf \"$S/x\"");
        assert!(escalates(&cmd).contains("$S is not known after"), "{cmd}");
    }
    let probe = "S=1; printf '--- %s ---' S=5 >/dev/null 2>&1; printf -%s S=6 >/dev/null 2>&1; \
                 printf '--- %d ---' S=7 >/dev/null 2>&1; echo \"$S\"";
    if let Some(out) = in_shell("/bin/zsh", probe) {
        assert_eq!(out, "7", "zsh evaluates only the numeric `-` format");
    }
    if let Some(out) = in_shell("/bin/bash", probe) {
        assert_eq!(out, "1", "bash assigns nothing for an unknown option");
    }
}

/// THE FOURTH CHECK (2026-09-27, medium): under `EXTENDED_GLOB` — a user's
/// `setopt` rides into Claude Code's shell snapshot — `^`, `#` and `~` glob
/// too, and the rule knew `*?[` only. Beside files `-t`, `-vS` and `S=5`,
/// `[ ^-vS ]` is `[ -t S=5 ]`, which sets `S`, and `printf ^%s S=5` is
/// `printf %d S=5` beside a file `%d` (measured below); `rm -rf
/// <root>/^x` removes every name in the root but `x`. Each was proven.
/// Negative controls: a leading `~`, extended-glob data after a `%s`
/// format, a plain command's word, a quoted `^`, an extended glob below a
/// directory inside the root, and a quoted `~` in a value (zsh's `~` only
/// excludes names from what the pattern before it matches: `a~b` is at
/// most `a`).
#[test]
fn an_extended_glob_word_moves_the_words_the_rule_reads() {
    for middle in [
        "[ ^-vS ]",
        "test ^x",
        "[ x~y ]",
        "printf ^%s S=5",
        "printf %s~x S=5",
    ] {
        let cmd = format!("S=/private/tmp/claude-502/w; {middle}; rm -rf \"$S/x\"");
        assert!(escalates(&cmd).contains("$S is not known after"), "{cmd}");
    }
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
    if !Path::new("/bin/zsh").exists() {
        return;
    }
    for (files, probe, want) in [
        (
            &["-t", "-vS", "S=5"][..],
            "setopt extendedglob; S=1; [ ^-vS ] 2>/dev/null; echo \"$S\"",
            "5",
        ),
        (
            &["%d"][..],
            "setopt extendedglob; S=1; printf ^%s S=7 >/dev/null 2>&1; echo \"$S\"",
            "7",
        ),
    ] {
        let t = TempTree::new("extglob");
        for f in files {
            std::fs::write(t.0.join(f), "").expect("a probe file");
        }
        let out = std::process::Command::new("/bin/zsh")
            .args(no_startup("/bin/zsh", probe))
            .current_dir(&t.0)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .stdin(std::process::Stdio::null())
            .output()
            .expect("zsh runs");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            want,
            "{probe} beside {files:?}"
        );
    }
}

/// THE FOURTH CHECK (2026-09-27, medium, a regression round 3 made): a
/// double-quoted word this rule cannot read forgot every variable under
/// any command but a plain one, so `printf '%s\n' "$x"` before the rm
/// escalated where upstream proved it. Past `printf`'s as-written format
/// that reads no arithmetic a quoted word is one word of data: its value
/// is no option (the options stand before the format), no name, and no
/// arithmetic (zsh evaluates an argument only under a numeric conversion).
/// Negative controls: the same word as the format, as `-v`'s name, before
/// the format, unquoted, under a numeric format, and under other commands.
#[test]
fn a_quoted_word_printf_reads_as_data_names_nothing() {
    for middle in [
        "printf '%s\\n' \"$x\"",
        "printf '%s: %s\\n' \"$x\" \"$(date)\"",
        "printf -- '%s\\n' \"$x\"",
        "printf -v T '%s' \"$x\"",
        "printf '%s\\n' \"$S\" \"${x}\"",
    ] {
        let cmd = format!("S=/private/tmp/claude-502/w; {middle}; rm -rf \"$S/x\"");
        assert_eq!(
            resolve(&cmd),
            Ok(vec!["/private/tmp/claude-502/w/x".to_string()]),
            "{cmd}"
        );
    }
    for middle in [
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
    ] {
        let cmd = format!("S=/private/tmp/claude-502/w; {middle}; rm -rf \"$S/x\"");
        assert!(escalates(&cmd).contains("$S is not known after"), "{cmd}");
    }
}

/// THE FIFTH CHECK (2026-09-28, usability): since round 3 a `[`/`test` with
/// a double-quoted word the line does not assign forgot every variable,
/// so `S=<scratch>; [ -n "$x" ]; rm -rf "$S/x"` and `[ -d "$HOME" ] && rm
/// -rf "$S/x"` escalated, though upstream pressed them. A string or file
/// operand names nothing ([`test_operands`]): measured, zsh 5.9 and bash
/// 3.2 (below, where they are installed), no value of the word assigns or
/// runs anything there. Negative controls: `-t`'s operand (arithmetic),
/// `-v`'s (a name whose subscript zsh evaluates), a word the shell may
/// glob or brace-expand, an unquoted one, an operator from a variable,
/// four words or more, a redirect's target, and `[[`, a compound command.
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
    // The measurement: each value in each form leaves `S` as it was and
    // runs nothing (the marker is computed, so an error quoting a value
    // cannot print it).
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
    for x in values {
        for y in [values[0], x] {
            for form in forms {
                let line = format!("S=/x/y; x='{x}'; y='{y}'; set -- '{x}'; {form}; echo \"$S\"");
                for sh in ["/bin/zsh", "/bin/bash"] {
                    if let Some((out, err)) = in_shell_both(sh, &line) {
                        assert_eq!(out.trim(), "/x/y", "{sh}: {line}");
                        assert!(!err.contains("RAN_42"), "{sh}: {line}");
                    }
                }
            }
        }
    }
}

/// `line` in `shell` as [`in_shell`] runs it: its stdout and its stderr.
fn in_shell_both(shell: &str, line: &str) -> Option<(String, String)> {
    if !Path::new(shell).exists() {
        return None;
    }
    let t = TempTree::new("shell");
    let out = std::process::Command::new(shell)
        .args(no_startup(shell, line))
        .current_dir(&t.0)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    Some((
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    ))
}

/// THE FIFTH CHECK, reviewed (2026-09-28, high): zsh subscripts a special
/// parameter as it does a name, and the subscript is arithmetic, which
/// assigns: `A=S=0; S=<scratch>; echo "$#[A]"; rm -rf "$S/x"` removes
/// `0/x` in the working directory (measured, zsh 5.9 -f; also `$?[A]`,
/// `$@[A]`, `$![A]`, `$-[A]`, `$*[A]`, `$1[A]` and `$#A[A]`). Negative
/// controls: the special parameters without a subscript.
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
    for cmd in [
        "S=/private/tmp/claude-502/w; echo \"$#\" \"$?\" \"$1\"; rm -rf \"$S/x\"",
        "S=/private/tmp/claude-502/w; printf '%s\\n' \"$#\"; rm -rf \"$S/x\"",
    ] {
        assert_eq!(
            resolve(cmd),
            Ok(vec!["/private/tmp/claude-502/w/x".to_string()]),
            "{cmd}"
        );
    }
}

/// A Unicode escape in `printf`'s format is decoded before the conversions
/// are read (2026-09-28 review, usability): one that decodes to no numeric
/// conversion reads its arguments as data, as upstream d41bdb4f8 pressed
/// (`printf '<check mark> %s\n' "$x"`); one that decodes to `%d` does not
/// (`x=S=0; printf <u0025d> "$x"` sets `S`, measured, zsh 5.9 -f). `BS`
/// stands for a backslash.
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
