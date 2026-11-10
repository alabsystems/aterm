// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The source guards: rules a type, a visibility or a clippy lint cannot express, each a small
//! function over one shared, lexed corpus.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;

use crate::memo::{self, Kept, Rejudged, Verdict};
use crate::{CensusOutcome, Floor, Walk};

mod corpus;
pub(crate) use corpus::{
    Corpus, Line, LoadKind, Script, SourceFile, Tree, brace_close, joined, named_for_tests,
};

mod agent_gate;
mod aos_step;
mod bridge_fx;
mod build_path;
mod captures;
mod census_thread;
mod changelog_repeat;
mod closure_anchor;
mod conflict_marker;
mod debug_seam;
mod doc_sentence;
mod doors;
mod eager_drop;
mod export_includes;
mod generated;
mod github;
mod grants;
mod graph;
mod identity_read;
mod installer;
mod keeper_native;
mod keys;
mod lifeline;
mod m6_names;
mod machines;
mod mint;
mod mirror_rows;
mod mktemp;
mod release_test_state;
mod rules;
mod rustup_keg;
mod seam_caller;
mod seam_reset;
mod shown_term;
mod slog_inert;
mod stock_label;
mod term_write;
mod unmounted_test;
mod usage_read;
mod win_resources;

/// One violation: where, which rule, and what the rule saw there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    /// The rule's id ([`Rule::id`]).
    pub rule: &'static str,
    /// Repo-relative path, `/`-separated.
    pub path: String,
    /// 1-based line; 0 when the finding is about the file as a whole.
    pub line: usize,
    /// What was seen, in one line.
    pub detail: String,
}

/// One guard: its id, the defect it keeps out, and its check.
pub struct Rule {
    /// The id findings carry.
    pub id: &'static str,
    /// The defect, and why it matters, in one line.
    pub why: &'static str,
    /// The path prefixes the rule reads by name.
    scope: &'static [&'static str],
    check: fn(&Tree<'_>, &mut Vec<Finding>),
}

/// Every guard, in the order findings print.
pub const GUARDS: &[Rule] = &[
    rules::ENGINE_PLATFORM,
    rules::RENDERER_TERMINAL,
    rules::DEV_READ,
    debug_seam::DEBUG_SEAM,
    seam_caller::SEAM_CALLER,
    census_thread::CENSUS_THREAD,
    rules::INPUT_PROBE,
    graph::TEST_APPKIT_DOOR,
    graph::SHARED_APPLICATION,
    rules::PROTECTED_FOLDER,
    rules::RUNLOOP_TIMER,
    rules::KEEPER_GUI,
    rules::KEEPER_GUI_DEPS,
    rules::KEEPER_NO_MMAP,
    rules::RESTORE_STAYS_LOCAL,
    lifeline::HEADLESS_LIFELINE,
    rules::STD_COMMAND,
    rules::FLOCK_DOOR,
    rules::SUPERVISE_FX,
    bridge_fx::BRIDGE_FX,
    aos_step::AOS_STEP_CLOCK,
    m6_names::M6_NAMES,
    rules::STORE_CURRENT,
    keys::KEY_MATERIAL,
    captures::CAPTURE_PRIVACY,
    export_includes::EXPORT_INCLUDES,
    build_path::BUILD_PATH_VALUE,
    doors::DEVTOOLS_DOOR,
    machines::MACHINES_ROOT_DOOR,
    doors::FFI_DOOR,
    doors::BOOT_ID_DOOR,
    doors::HANDOFF_NAME,
    doors::CORE_SIGNAL,
    mint::LAUNCHER_MINT,
    grants::PROMPT_GRANT,
    mktemp::CHECKED_MKTEMP,
    rustup_keg::RUSTUP_KEG,
    installer::INSTALLER_NETWORK,
    agent_gate::AGENT_GATE,
    term_write::TERM_WRITE,
    usage_read::USAGE_READ_ON_TIMER,
    shown_term::SHOWN_TERM,
    slog_inert::SLOG_INERT,
    eager_drop::EAGER_DROP,
    seam_reset::SEAM_RESET,
    doc_sentence::DOC_SENTENCE,
    stock_label::STOCK_LABEL,
    github::GITHUB_AUTH_DOOR,
    github::GH_SPAWN,
    github::AGH_FX,
    closure_anchor::CLOSURE_ANCHOR,
    rules::LICENCE_HEADER,
    generated::GENERATED_SEAL,
    identity_read::IDENTITY_READ,
    conflict_marker::CONFLICT_MARKER,
    changelog_repeat::CHANGELOG_REPEAT,
    mirror_rows::MIRROR_ROWS,
    unmounted_test::UNMOUNTED_TEST,
    release_test_state::RELEASE_TEST_STATE,
    win_resources::WIN_RESOURCES,
];

/// The guards' outcome over one tree.
pub struct GuardReport {
    /// The `.rs` files read.
    pub files: usize,
    /// The scripts read ([`Script`]).
    pub scripts: usize,
    /// Every finding, in [`GUARDS`] order, then by path and line.
    pub findings: Vec<Finding>,
}

/// Run every guard over the checkout at `root`.
///
/// # Errors
/// The checkout's source cannot be walked or read.
pub fn run_guards(root: &Path, engine: &crate::EngineBaseline) -> Result<GuardReport, String> {
    guard_report(&Walk {
        root,
        engine,
        source: &std::sync::OnceLock::new(),
        run: None,
    })
}

/// Every guard over `walk`'s checkout: each rule a memo unit when a memo run keeps a record.
fn guard_report(walk: &Walk<'_>) -> Result<GuardReport, String> {
    let corpus = read_corpus(walk)?;
    let tree = Tree::new(walk.root, &corpus, walk.engine);
    let mut findings = scope_findings(&corpus, GUARDS);
    findings.extend(run_rules(&tree, GUARDS, walk));
    Ok(GuardReport {
        files: corpus.len(),
        scripts: corpus.scripts().len(),
        findings,
    })
}

/// The corpus of `walk`'s snapshot. With a memo run, its accessors record their questions in
/// the run's units, and the run judges those questions by this corpus's answers.
pub(crate) fn read_corpus(walk: &Walk<'_>) -> Result<Arc<Corpus>, String> {
    let owner = walk.run.map(|run| Arc::clone(&run.shared));
    let corpus = Arc::new(Corpus::read(walk.root, walk.snapshot(), owner)?);
    if let Some(run) = walk.run {
        let [paths, scripts, manifests] = corpus.facts();
        let shared = Arc::clone(&run.shared);
        let held = Arc::clone(&corpus);
        run.set_corpus(memo::CorpusNow {
            globals: [paths, scripts, manifests, memo::engine_digest(walk.engine)],
            test_code: Box::new(move |rel| {
                memo::loader(&shared, || held.test_code_now().holds(rel))
            }),
        });
    }
    Ok(corpus)
}

/// One finding per scope prefix of `rules` that no file of `corpus` lies under.
fn scope_findings(corpus: &Corpus, rules: &[Rule]) -> Vec<Finding> {
    let mut out = Vec::new();
    for rule in rules {
        for prefix in rule.scope {
            if corpus.under(&[prefix]).next().is_none() {
                out.push(Finding {
                    rule: rule.id,
                    path: (*prefix).to_string(),
                    line: 0,
                    detail: "no Rust file lies under this scope, so the rule reads nothing \
                             there (renamed? name its new path in the rule)"
                        .to_string(),
                });
            }
        }
    }
    out
}

/// `rules` over `tree`, each on its own thread (they share only the corpus,
/// whose lexing is computed once per file whichever rule asks first), each one unit of `walk`'s
/// memo run when one keeps a record ([`judge`]).
fn run_rules(tree: &Tree<'_>, rules: &[Rule], walk: &Walk<'_>) -> Vec<Finding> {
    memo::threads(|scope| {
        let running: Vec<_> = rules
            .iter()
            .map(|rule| {
                scope.spawn_named(format!("guard-{}", rule.id), 8 << 20, move || {
                    judge(rule, tree, walk)
                })
            })
            .collect();
        let mut all = Vec::new();
        for (handle, rule) in running.into_iter().zip(rules) {
            let joined = handle
                .map_err(|e| e.to_string())
                .and_then(|h| h.join().map_err(|_| "it panicked".to_string()));
            match joined {
                Ok(mut found) => {
                    found.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
                    all.extend(found);
                }
                Err(why) => all.push(Finding {
                    rule: rule.id,
                    path: String::new(),
                    line: 0,
                    detail: format!("the rule could not run: {why}"),
                }),
            }
        }
        all
    })
}

/// `rule`'s findings over `tree`: the ones `walk`'s memo run kept for it while every question it
/// asked keeps its answer, else judged now as one unit of the run and kept. A rule that panics
/// keeps nothing, and one whose findings carry another rule's id is judged again every run.
fn judge(rule: &Rule, tree: &Tree<'_>, walk: &Walk<'_>) -> Vec<Finding> {
    let check = || {
        let mut found = Vec::new();
        (rule.check)(tree, &mut found);
        found
    };
    let Some(run) = walk.run else {
        return check();
    };
    let id = format!("guard:{}", rule.id);
    let why = match run.reusable(&id, walk.snapshot()) {
        Ok(record) => match &record.result {
            Verdict::Findings(kept) => {
                let found = kept
                    .iter()
                    .map(|k| Finding {
                        rule: rule.id,
                        path: k.path.clone(),
                        line: k.line,
                        detail: k.detail.clone(),
                    })
                    .collect();
                run.keep_reused(record);
                return found;
            }
            Verdict::Outcome(_) => Rejudged::Changed("its record is a census's".into()),
        },
        Err(why) => why,
    };
    let (judged, trace) = memo::unit(&run.shared, || {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(check))
    });
    match judged {
        Ok(found) => {
            let kept = found.iter().all(|f| f.rule == rule.id).then(|| {
                let kept = found
                    .iter()
                    .map(|f| Kept {
                        path: f.path.clone(),
                        line: f.line,
                        detail: f.detail.clone(),
                    })
                    .collect();
                (trace, Verdict::Findings(kept))
            });
            run.keep_judged(&id, why, kept);
            found
        }
        Err(panic) => {
            run.keep_judged(&id, why, None);
            std::panic::resume_unwind(panic)
        }
    }
}

/// The guards as a census: the transcript `aterm-census --guards` prints.
pub(crate) fn run_guard_census(walk: &Walk<'_>) -> CensusOutcome {
    const TITLE: &str = "guards";
    let (root, engine) = (walk.root, walk.engine);
    let ids: Vec<&str> = GUARDS.iter().map(|r| r.id).collect();
    let mut log = format!(
        "=== guards (source rules over crates/, apps/) ===\n    root: {}\n    \
         rules: {}\n",
        root.display(),
        ids.join(", ")
    );
    let report = match guard_report(walk) {
        Ok(report) => report,
        Err(why) => {
            return CensusOutcome::red(TITLE, log, format!("the tree could not be read: {why}"));
        }
    };
    let notes = captures::notes(root, engine);
    for note in &notes {
        let _ = writeln!(log, "    note: {note}");
    }
    let mut by_rule: BTreeMap<usize, Vec<&Finding>> = BTreeMap::new();
    for f in &report.findings {
        let at = GUARDS.iter().position(|r| r.id == f.rule).unwrap_or(0);
        by_rule.entry(at).or_default().push(f);
    }
    for (at, found) in &by_rule {
        for f in found {
            let _ = writeln!(
                log,
                "  ✗ FAIL [{}] {}:{}: {}",
                f.rule, f.path, f.line, f.detail
            );
        }
        let _ = writeln!(log, "      why: {}", GUARDS[*at].why);
    }
    if report.findings.is_empty() {
        let floors = vec![Floor::at_least(
            "Rust files the guards read",
            report.files,
            1500,
            "the walk lost most of the tree (a wrong root or a moved crates/), so every \
                 guard held vacuously",
        )];
        CensusOutcome::green(
            TITLE,
            log,
            format!(
                "{} rules over {} Rust files and {} scripts; no finding.{}",
                GUARDS.len(),
                report.files,
                report.scripts,
                notes.iter().map(|n| format!(" ({n})")).collect::<String>()
            ),
        )
        .with_floors(floors)
    } else {
        CensusOutcome::red(
            TITLE,
            log,
            format!(
                "{} finding(s) from {} rule(s).",
                report.findings.len(),
                by_rule.len()
            ),
        )
    }
}

pub(crate) const fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// How a needle must sit in its line.
#[derive(Clone, Copy)]
pub(crate) enum Bound {
    /// Anywhere.
    Sub,
    /// Not inside a longer identifier on either side.
    Word,
}

/// The offset just past a macro's opening delimiter `(`, `[`, or `{`, paired with its closer.
pub(crate) fn open_delim(code: &str, at: usize) -> Option<(usize, &'static str)> {
    token(code, at, "(")
        .map(|p| (p, ")"))
        .or_else(|| token(code, at, "[").map(|p| (p, "]")))
        .or_else(|| token(code, at, "{").map(|p| (p, "}")))
}

/// The offset just past `tok`, when it is the next thing after blanks from `at`.
pub(crate) fn token(code: &str, at: usize, tok: &str) -> Option<usize> {
    let rest = code.get(at..)?;
    let start = at + (rest.len() - rest.trim_start().len());
    code[start..].starts_with(tok).then_some(start + tok.len())
}

/// The string literal that is the next thing after blanks from `at` (the CODE view keeps
/// only a one-line literal's two quotes): its text, and the offset just past it.
pub(crate) fn literal<'a>(code: &str, text: &'a str, at: usize) -> Option<(&'a str, usize)> {
    let open = token(code, at, "\"")?;
    let close = open + code[open..].find('"')?;
    Some((text.get(open..close)?, close + 1))
}

/// The byte offsets where `needle` occurs in `hay` under `bound`.
pub(crate) fn hits<'a>(
    hay: &'a str,
    needle: &'a str,
    bound: Bound,
) -> impl Iterator<Item = usize> + 'a {
    let h = hay.as_bytes();
    hay.match_indices(needle)
        .map(|(at, _)| at)
        .filter(move |&at| {
            matches!(bound, Bound::Sub)
                || ((at == 0 || !is_word(h[at - 1]))
                    && h.get(at + needle.len()).is_none_or(|&b| !is_word(b)))
        })
}

/// Does `hay` hold `needle` under `bound`?
pub(crate) fn has(hay: &str, needle: &str, bound: Bound) -> bool {
    hits(hay, needle, bound).next().is_some()
}

/// `s` after `prefix`, past any spaces and tabs before it.
pub(crate) fn after_ws<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    s.trim_start_matches([' ', '\t']).strip_prefix(prefix)
}

/// Every name a line calls: the identifier before each `(` (spaces between allowed), whether it
/// is a free call, a path's last segment or a method.
pub(crate) fn calls(code: &str) -> Vec<&str> {
    let b = code.as_bytes();
    let mut out = Vec::new();
    for (at, _) in code.match_indices('(') {
        let mut end = at;
        while end > 0 && matches!(b[end - 1], b' ' | b'\t') {
            end -= 1;
        }
        let mut start = end;
        while start > 0 && is_word(b[start - 1]) {
            start -= 1;
        }
        while start < end && b[start].is_ascii_digit() {
            start += 1;
        }
        if start < end {
            out.push(&code[start..end]);
        }
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// No engine baseline: what a test's tree is judged with unless it says otherwise.
    pub(crate) const NO_ENGINE: &crate::EngineBaseline = &Err(String::new());

    /// The rule's findings over `files`, with no checkout behind them.
    pub(crate) fn run(rule: &Rule, files: &[(&str, &str)]) -> Vec<Finding> {
        let corpus = Corpus::of(files);
        let tree = Tree::new(
            Path::new("/nonexistent-aterm-census-root"),
            &corpus,
            NO_ENGINE,
        );
        let walk = Walk {
            root: tree.root,
            engine: NO_ENGINE,
            source: &std::sync::OnceLock::from(Arc::new(memo::Snapshot::empty())),
            run: None,
        };
        run_rules(&tree, std::slice::from_ref(rule), &walk)
    }

    /// Each finding as `path:line`.
    pub(crate) fn spots(found: &[Finding]) -> Vec<String> {
        found
            .iter()
            .map(|f| format!("{}:{}", f.path, f.line))
            .collect()
    }

    #[test]
    fn calls_are_the_identifiers_before_each_paren() {
        assert_eq!(
            calls("let x = a::b(c.d (1), Some(e)) + f[0](g);"),
            ["b", "d", "Some"]
        );
        assert!(calls("(1, 2)").is_empty());
    }

    #[test]
    fn word_hits_respect_identifier_edges() {
        assert!(has("use libc::c_int;", "libc", Bound::Word));
        assert!(!has("use aterm_libc::x;", "libc", Bound::Word));
        assert!(!has("MetalLayer", "Metal", Bound::Word));
        assert!(has("MetalLayer", "Metal", Bound::Sub));
    }

    #[test]
    fn shipped_lines_drop_test_items_in_both_views() {
        let file = SourceFile::outside(
            "crates/x/src/lib.rs".to_string(),
            "fn a() {}\n#[cfg(test)]\nmod tests {\n    fn b() {}\n}\nfn c() {}\n",
        );
        let kept: Vec<usize> = file.shipped_lines().map(|l| l.no).collect();
        assert_eq!(kept, [1, 6]);
    }

    #[test]
    fn a_panicking_rule_is_one_finding_not_a_crash() {
        fn boom(_: &Tree<'_>, _: &mut Vec<Finding>) {
            panic!("planted");
        }
        let rule = Rule {
            id: "boom",
            why: "planted",
            scope: &[],
            check: boom,
        };
        let found = run(&rule, &[]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].rule, "boom");
    }

    /// The files of `corpus` that are test code nothing ships ([`rules::TestCode::holds`]).
    fn held(corpus: &Corpus) -> Vec<String> {
        let test_code = corpus.test_code();
        let held: std::collections::BTreeSet<String> = corpus
            .under(&[""])
            .filter(|f| test_code.holds(&f.rel))
            .map(|f| f.rel.clone())
            .collect();
        held.into_iter().collect()
    }

    /// A file is test code when its parent declares it under a test `cfg`,
    /// by its own name, its `mod.rs` or a `#[path]`, or when a test-only file
    /// declares it.
    #[test]
    fn a_module_its_parent_gates_on_test_is_test_code() {
        let corpus = Corpus::of(&[
            (
                "crates/x/src/lib.rs",
                "#[cfg(test)]\nmod a;\n#[cfg(all(test, unix))]\n#[path = \"rig_conformance.rs\"]\n\
                 pub(crate) mod rig;\n#[cfg(not(test))]\nmod live;\nmod plain;\n",
            ),
            ("crates/x/src/a.rs", "mod b;\n"),
            ("crates/x/src/a/b/mod.rs", ""),
            ("crates/x/src/rig_conformance.rs", ""),
            ("crates/x/src/live.rs", ""),
            (
                "crates/x/src/plain.rs",
                "#[cfg(test)]\nmod tests;\nmod inner;\n",
            ),
            ("crates/x/src/plain/tests.rs", ""),
            ("crates/x/src/plain/inner.rs", ""),
        ]);
        assert_eq!(
            held(&corpus),
            [
                "crates/x/src/a.rs",
                "crates/x/src/a/b/mod.rs",
                "crates/x/src/plain/tests.rs",
                "crates/x/src/rig_conformance.rs",
            ]
        );
    }

    /// A file a shipped line also loads ships, and so does what it declares; a file only a
    /// test-only file loads stays test code, and so does one a test-only inline module
    /// `include!`s. Each load resolves as rustc resolves it: an attribute on the `mod`'s own
    /// line, a `cfg_attr` path, an inline module's directory, an `include!` literal.
    #[test]
    fn a_test_module_a_shipped_line_also_loads_ships() {
        let corpus = Corpus::of(&[
            (
                "crates/x/src/lib.rs",
                "#[cfg(test)]\nmod a;\n#[cfg(test)]\nmod b;\n#[path = \"b.rs\"] mod b_again;\n\
                 mod outer {\n    #[cfg_attr(unix, path = \"o.rs\")]\n    mod inner;\n    \
                 mod plain;\n}\ninclude!(\"inc.rs\");\n#[cfg(test)]\nmod c {\n    \
                 include!(\"c_inc.rs\");\n}\n",
            ),
            ("crates/x/src/a.rs", "mod a_child;\n"),
            ("crates/x/src/a/a_child.rs", ""),
            ("crates/x/src/b.rs", "mod b_child;\n"),
            ("crates/x/src/b/b_child.rs", ""),
            ("crates/x/src/outer/o.rs", ""),
            ("crates/x/src/outer/plain.rs", ""),
            ("crates/x/src/inc.rs", ""),
            ("crates/x/src/c_inc.rs", ""),
        ]);
        assert_eq!(
            held(&corpus),
            [
                "crates/x/src/a.rs",
                "crates/x/src/a/a_child.rs",
                "crates/x/src/c_inc.rs",
            ]
        );
        let lib = corpus.get("crates/x/src/lib.rs").map(SourceFile::loads);
        let loads: Vec<(Vec<String>, bool)> = lib
            .unwrap_or_default()
            .iter()
            .map(|l| (l.files.clone(), l.gated))
            .collect();
        let s = |p: &str| p.to_string();
        assert_eq!(
            loads,
            [
                (
                    vec![s("crates/x/src/a.rs"), s("crates/x/src/a/mod.rs")],
                    true
                ),
                (
                    vec![s("crates/x/src/b.rs"), s("crates/x/src/b/mod.rs")],
                    true
                ),
                (vec![s("crates/x/src/b.rs")], false),
                (vec![s("crates/x/src/outer/o.rs")], false),
                (
                    vec![
                        s("crates/x/src/outer/plain.rs"),
                        s("crates/x/src/outer/plain/mod.rs")
                    ],
                    false
                ),
                (vec![s("crates/x/src/inc.rs")], false),
                (vec![s("crates/x/src/c_inc.rs")], true),
            ]
        );
    }

    /// The corpus fails closed: a source directory its `.rs` walk may not list
    /// refuses the read, never a corpus of what it could read.
    #[test]
    fn a_source_directory_it_cannot_list_refuses_the_corpus() {
        let read = |fault: fn(&crate::tree::memory::Tree)| {
            let root = crate::scan_set::test_fixtures::synth_tree(
                "aterm-census-guards",
                "unlisted",
                &[("crates/a/src/notes.md".to_string(), String::new())],
            );
            fault(&root);
            Corpus::at(&root).map(|corpus| corpus.len())
        };
        assert_eq!(read(|_| {}), Ok(0));
        assert!(read(|root| root.unlistable("crates/a/src")).is_err());
    }

    /// The walk enters dot directories for paths (never `.git`, nor one
    /// `.gitignore` keeps out by directory), and reads scripts only outside
    /// them.
    #[test]
    fn the_walk_lists_dot_directories_but_reads_no_script_there() {
        use crate::scan_set::test_fixtures::disk_tree;
        let files: Vec<(String, String)> = [
            (
                ".gitignore",
                "debug\n.claude/\n/.trust-cache/\n/publish/.out/\n**/mutants.out*/\n\
                 !vendor/**/debug/\n",
            ),
            ("tools/install.sh", "#!/bin/sh\n"),
            (".cargo/config.toml", ""),
            (".cargo/machine.key", ""),
            (".config/hooks/stop.sh", "#!/bin/sh\n"),
            (".claude/worktrees/w/.cargo/machine.key", ""),
            (".trust-cache/f.json", ""),
            ("crates/a/.trust-cache/f.json", ""),
            ("publish/.out/x.sh", "#!/bin/sh\n"),
            (".git/hooks/pre-commit.sh", "#!/bin/sh\n"),
            ("target/debug/x.sh", "#!/bin/sh\n"),
            (
                "crates/a/src/lib.rs",
                "// SPDX-License-Identifier: Apache-2.0\n",
            ),
        ]
        .iter()
        .map(|(p, c)| ((*p).to_string(), (*c).to_string()))
        .collect();
        let root = disk_tree("aterm-census-guards", "walk-dot-dirs", &files);
        let corpus = Corpus::at(&root);
        let _ = std::fs::remove_dir_all(&root);
        let corpus = corpus.expect("the synth tree reads");
        assert_eq!(
            corpus.paths(),
            [
                ".cargo/config.toml",
                ".cargo/machine.key",
                ".config/hooks/stop.sh",
                ".gitignore",
                "crates/a/.trust-cache/f.json",
                "crates/a/src/lib.rs",
                "tools/install.sh",
            ]
        );
        let scripts: Vec<&str> = corpus.scripts().iter().map(|s| s.rel.as_str()).collect();
        assert_eq!(scripts, ["tools/install.sh"]);
    }

    #[test]
    fn a_scope_no_file_lies_under_is_a_finding() {
        fn nothing(_: &Tree<'_>, _: &mut Vec<Finding>) {}
        let rule = Rule {
            id: "scoped",
            why: "planted",
            scope: &["crates/aterm-grid/", "crates/aterm-renamed/"],
            check: nothing,
        };
        let corpus = Corpus::of(&[("crates/aterm-grid/src/lib.rs", "")]);
        let found = scope_findings(&corpus, std::slice::from_ref(&rule));
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].path, "crates/aterm-renamed/");
    }
}
