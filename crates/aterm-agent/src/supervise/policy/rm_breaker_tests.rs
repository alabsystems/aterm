// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The rm-breaker resolver against the shapes the audit measured: the
//! approvals the synthesis names, and a negative control beside each rule.

use std::path::{Path, PathBuf};

use super::*;

const CWD: &str = "/Users//_owner/aterm";
const HOME: &str = "/Users//_owner";
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
    let tmp = PathBuf::from(TMP);
    let scope = RmScope {
        cwd: Path::new(cwd),
        home: Some(Path::new(HOME)),
        tmpdir: Some(&tmp),
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

#[test]
fn mktemp_resolves_under_tmpdir_and_may_not_glob() {
    assert_eq!(
        approves("D=$(mktemp -d); rm -rf \"$D\""),
        ["/var/folders/ab/xyz/T/<mktemp>"]
    );
    approves("D=$(mktemp -d -t probe); rm -rf \"$D/x\"");
    approves("D=$(mktemp -d /private/tmp/claude-502/w.XXXXXX); rm -rf \"$D\"");
    assert!(escalates("D=$(mktemp -d); rm -rf \"$D\"/*").contains("glob behind a $(mktemp)"));
    assert!(escalates("D=$(mktemp); rm -rf \"$D\"").contains("$D"));
    assert!(escalates("D=$(mktemp -d -p /x); rm -rf \"$D\"").contains("$D"));
    assert!(escalates("D=$(pwd); rm -rf \"$D/x\"").contains("$D"));
    approves("rm -rf \"$TMPDIR/probe\"");
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
        approves("rm -rf /Users//_owner/aterm/target/debug/x"),
        ["/Users//_owner/aterm/target/debug/x"]
    );
    approves("rm -rf /Users//_owner/aterm/target.noindex/x");
    assert!(escalates("rm -rf /Users//_owner/aterm/target").contains("not strictly inside"));
    assert!(escalates("rm -rf /Users//_owner/aterm/src/x").contains("not strictly inside"));
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
    approves("rm -rf /Users//_owner/aterm/target/x");
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
        ("S=/tmp/w/a; printf -v S /; rm -rf \"$S/x\"", "printf -v"),
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
