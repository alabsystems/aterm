// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;
use crate::scan_set::test_fixtures::synth_tree;

#[test]
fn a_tests_part_is_a_test_file_and_a_name_that_only_contains_it_is_not() {
    let test = |p: &str| is_test_file(Path::new(p));
    assert!(test(
        "crates/aterm-shell-integration/src/tests_pwsh_keys.rs"
    ));
    assert!(test("crates/aterm-pty/src/unix_tests.rs"));
    assert!(test("crates/aterm-pty/tests/smoke.rs"));
    assert!(!test("crates/aterm-pty/src/lib.rs"));
    assert!(!test("crates/aterm-pty/src/latests_seen.rs"));
    assert!(!test("crates/aterm-pty/src/tests_dir/lib.rs"));
}

#[test]
fn a_standalone_tests_module_is_a_test_file() {
    for path in [
        "tests.rs",
        "crates/aterm-agent/src/vault_join/automatic/tests.rs",
    ] {
        assert!(is_test_file(Path::new(path)), "{path}");
    }
    for path in [
        "test.rs",
        "crates/aterm-agent/src/contest.rs",
        "crates/aterm-agent/src/automatic.rs",
    ] {
        assert!(!is_test_file(Path::new(path)), "{path}");
    }
}

#[test]
fn a_test_gated_module_and_its_folder_are_test_code_and_nothing_else_is() {
    let lib = "#[cfg(test)]\n/// The sim.\nmod sim;\npub mod shipped;\n\
               #[cfg(test)]\npub mod both;\n#[cfg(not(test))]\nmod both;\n\
               #[cfg(test)]\n#[path = \"probe_impl.rs\"]\nmod probe;\n\
               #[cfg(any(test, debug_assertions))]\nmod seams;\n\
               #[cfg(test)]\nmod inner;\nmod outer {\n    mod inner;\n}\n";
    let shipped = "#[cfg(test)] pub(crate) mod fixture; // one line\nmod kept;\n";
    let files: Vec<(String, String)> = [
        ("crates/a/src/lib.rs", lib),
        ("crates/a/src/sim/mod.rs", "mod world;\n"),
        ("crates/a/src/sim/world.rs", ""),
        ("crates/a/src/simulate.rs", ""),
        ("crates/a/src/shipped.rs", shipped),
        ("crates/a/src/shipped/fixture.rs", ""),
        ("crates/a/src/shipped/kept.rs", ""),
        ("crates/a/src/probe_impl.rs", ""),
        ("crates/a/src/seams.rs", ""),
        ("crates/a/src/outer/inner.rs", ""),
        ("crates/a/src/inner.rs", ""),
        (
            "crates/a/src/both.rs",
            "mod child;\n#[cfg(test)]\nmod probe;\n",
        ),
        ("crates/a/src/both/child.rs", ""),
        ("crates/a/src/both/probe.rs", ""),
    ]
    .iter()
    .map(|(rel, text)| ((*rel).to_string(), (*text).to_string()))
    .collect();
    let root = synth_tree("aterm-census-gated", "gated", &files);
    let mut all = Vec::new();
    collect_rs_files(&root.join("crates"), &mut all).expect("every entry reads");
    let gated = test_gated_module_files(&all);
    let expected: std::collections::BTreeSet<PathBuf> = [
        "crates/a/src/sim/mod.rs",
        "crates/a/src/sim/world.rs",
        "crates/a/src/shipped/fixture.rs",
        "crates/a/src/probe_impl.rs",
        "crates/a/src/both/probe.rs",
        "crates/a/src/inner.rs",
    ]
    .iter()
    .map(|rel| root.join(rel))
    .collect();
    assert_eq!(gated, expected);
}

#[test]
fn complete_false_cfg_loaders_exclude_files_until_a_live_loader_rescues_them() {
    let lib = "#[cfg(\r\n all(test, unix)\n)]\n#[path = \"dead.rs\"]\nmod hidden;\n#[cfg(not(not(test)))]\n#[path = \"shared.rs\"]\nmod dead_shared;\n#[cfg(any(test, feature = \"x\"))]\n#[path = \"shared.rs\"]\nmod live_shared;\n";
    let lib = lib.replace("\r\n", "\n").replace('\n', "\r\n");
    let files = [
        ("crates/a/src/lib.rs".to_owned(), lib),
        ("crates/a/src/dead.rs".to_owned(), String::new()),
        ("crates/a/src/shared.rs".to_owned(), String::new()),
    ];
    let root = synth_tree("aterm-census-cfg-loaders", "complete", &files);
    let mut all = Vec::new();
    collect_rs_files(&root.join("crates"), &mut all).expect("fixture files");
    assert_eq!(
        test_gated_module_files(&all),
        std::collections::BTreeSet::from([root.join("crates/a/src/dead.rs")])
    );
    let declarations = mod_decls(
        "#[cfg(all(test, broken(,)))]\nmod unknown;\n#[cfg(not(test))]\nmod production;\n",
    );
    assert_eq!(declarations.len(), 2);
    assert!(declarations.iter().all(|decl| !decl.test_only));
}

#[test]
fn quoted_or_commented_module_examples_cannot_remove_shipped_sources() {
    let lib = r###"const EXAMPLE: &str = r#"
#[cfg(test)]
mod live;
"#;
/*
#[cfg(test)]
mod commented;
*/
include!("live.rs");
include!("commented.rs");
"###;
    let files = [
        ("crates/a/src/lib.rs".to_owned(), lib.to_owned()),
        ("crates/a/src/live.rs".to_owned(), String::new()),
        ("crates/a/src/commented.rs".to_owned(), String::new()),
    ];
    let root = synth_tree("aterm-census-quoted-mod", "quoted", &files);
    let mut all = Vec::new();
    collect_rs_files(&root.join("crates"), &mut all).expect("every entry reads");
    assert!(test_gated_module_files(&all).is_empty());
}

fn empty_files(name: &str, files: &[&str]) -> crate::tree::memory::Tree {
    let files: Vec<_> = files
        .iter()
        .map(|rel| ((*rel).to_string(), String::new()))
        .collect();
    synth_tree("aterm-census-walk", name, &files)
}

#[test]
fn the_rs_walk_skips_target_and_dot_directories() {
    let root = empty_files(
        "skips",
        &[
            "crates/a/src/lib.rs",
            "crates/a/src/m/x.rs",
            "crates/a/src/notes.md",
            "crates/a/target/debug/build/out.rs",
            "crates/a/.claude/worktrees/w/src/lib.rs",
            "crates/a/src/.scratch/x.rs",
        ],
    );
    let mut files = Vec::new();
    collect_rs_files(&root.join("crates"), &mut files).expect("every entry reads");
    files.sort();
    assert_eq!(
        files,
        [
            root.join("crates/a/src/lib.rs"),
            root.join("crates/a/src/m/x.rs")
        ]
    );
}

#[test]
fn the_rs_walk_stops_where_it_cannot_read() {
    use std::io::ErrorKind;
    let walk = |fault: fn(&crate::tree::memory::Tree)| {
        let root = empty_files("unread", &["crates/a/src/lib.rs", "crates/a/src/m/x.rs"]);
        fault(&root);
        let mut files = Vec::new();
        collect_rs_files(&root.join("crates"), &mut files)
            .map(|()| files.len())
            .map_err(|e| e.kind())
    };
    assert_eq!(walk(|_| {}), Ok(2), "read whole");
    assert_eq!(
        walk(|root| root.unreadable("crates/a/src/m/x.rs")),
        Err(ErrorKind::Other)
    );
    assert_eq!(
        walk(|root| root.unlistable("crates/a/src/m")),
        Err(ErrorKind::PermissionDenied)
    );
}

#[test]
fn run_censuses_keeps_the_given_order_and_turns_a_panic_red() {
    fn clean(_: &Walk<'_>) -> CensusOutcome {
        CensusOutcome::green("a", String::new(), "clean")
    }
    fn broken(_: &Walk<'_>) -> CensusOutcome {
        panic!("a walker bug")
    }
    let a = Census {
        name: "a",
        about: "",
        run: clean,
    };
    let b = Census {
        name: "b",
        about: "",
        run: broken,
    };
    let (out, report) = run_censuses(Path::new("."), &[&b, &a], &Err(String::new()), None);
    assert!(report.is_none(), "no memo, no report");
    assert!(
        !out[0].ok && out[0].log.contains("b census"),
        "{}",
        out[0].log
    );
    assert!(out[1].ok, "{}", out[1].log);
}

#[test]
fn run_censuses_turns_a_missed_floor_red() {
    fn thin(_: &Walk<'_>) -> CensusOutcome {
        CensusOutcome::green("thin", "=== thin ===\n".to_string(), "1 site").with_floors(vec![
            Floor::at_least("sites read", 1, 100, "the walk went blind"),
            Floor::at_least("files read", 3, 1, "no files"),
        ])
    }
    fn red(_: &Walk<'_>) -> CensusOutcome {
        CensusOutcome::red("red", String::new(), "1 violation").with_floors(vec![Floor::at_least(
            "sites read",
            0,
            100,
            "the walk went blind",
        )])
    }
    let (a, b) = (
        Census {
            name: "thin",
            about: "",
            run: thin,
        },
        Census {
            name: "red",
            about: "",
            run: red,
        },
    );
    let walk = Walk {
        root: Path::new("."),
        engine: &Err(String::new()),
        source: &OnceLock::from(std::sync::Arc::new(memo::Snapshot::empty())),
        run: None,
    };
    assert!(thin(&walk).ok, "the bare walk holds no floors");
    let (out, _) = run_censuses(Path::new("."), &[&a, &b], &Err(String::new()), None);
    assert!(!out[0].ok, "{}", out[0].log);
    assert!(
        out[0].log.contains("[floor] sites read") && !out[0].log.contains("files read"),
        "only the missed floor is named:\n{}",
        out[0].log
    );
    assert!(
        out[1].verdict.ends_with("1 violation"),
        "{}",
        out[1].verdict
    );
}

fn body(src: &str) -> Vec<String> {
    src.lines().map(|l| l.to_string()).collect()
}

#[test]
fn parse_fn_def_recognises_qualified_definitions_not_fn_bounds() {
    assert_eq!(
        parse_fn_def("    pub(crate) fn resize_panes_scoped(&mut self) {"),
        Some((4, "resize_panes_scoped".to_string()))
    );
    assert_eq!(
        parse_fn_def("fn cross_resize(term: &Arc) -> String {"),
        Some((0, "cross_resize".to_string()))
    );
    assert!(parse_fn_def("    where F: Fn(u16) -> bool,").is_none());
    assert!(parse_fn_def("        let f = default_fn(x);").is_none());
}

#[test]
fn a_bodyless_declaration_does_not_swallow_the_fns_after_it() {
    let src = "\
extern \"system\" {
    fn GetDC(hwnd: isize) -> isize;
    fn ReleaseDC(
        hwnd: isize,
        dc: isize,
    ) -> i32;
}

pub trait Present {
    fn present(&mut self, rects: &[u32; 4]) -> bool;
}

impl Present for Gdi {
    fn present(&mut self, rects: &[u32; 4]) -> bool {
        let dc = unsafe { GetDC(self.hwnd) };
        blit(dc, rects)
    }
}

fn blit(dc: isize, rects: &[u32; 4]) -> bool {
    term_lock(&TERM).resize(rects[0], rects[1]);
    dc != 0
}

fn table() -> [f32; 256] {
    [0.0; 256]
}
";
    let mut fns = Vec::new();
    parse_source_fns(src, "x.rs", &mut fns);
    let spans: Vec<(&str, &str)> = fns
        .iter()
        .map(|f| (f.name.as_str(), f.span.as_str()))
        .collect();
    assert_eq!(
        spans,
        [
            ("present", "x.rs:14"),
            ("blit", "x.rs:20"),
            ("table", "x.rs:25")
        ],
        "each defined fn is its own item; no declaration is one"
    );
    assert_eq!(fns[0].body.len(), 4);
    assert!(fns[1].body.iter().any(|l| l.contains("term_lock(")));
    assert_eq!(fns[2].body.len(), 3);
}

#[test]
fn term_hop_flags_synchronous_guarded_resize_both_idioms() {
    let (hz, _) = term_hop_calls(&body("    let p = term_lock(term).resize(rows, cols);"));
    assert_eq!(hz.len(), 1, "chained term_lock(..).resize( must flag");
    assert_eq!(hz[0].method, "resize");
    let (hz, _) = term_hop_calls(&body(
        "    let mut term = term_lock(&s.term);\n    term.resize(sub_rows, sub_cols);",
    ));
    assert_eq!(hz.len(), 1, "guard.resize( must flag");
}

#[test]
fn term_hop_passes_the_offloaded_shape_and_bounded_neighbours() {
    let (hz, off) = term_hop_calls(&body(
        "    let mut term = term_lock(&s.term);\n    \
         term.resize_offloading_scrollback(sub_rows, sub_cols)",
    ));
    assert!(hz.is_empty(), "resize_offloading_scrollback must not flag");
    assert_eq!(off, 1, "offload boundary should be counted");
    let (hz, off) = term_hop_calls(&body("    term_lock(&s.term).set_cell_pixel_size(w, h);"));
    assert!(hz.is_empty());
    assert_eq!(off, 0);
}

#[test]
fn term_hop_ignores_vec_and_local_terminal_resize() {
    let (hz, _) = term_hop_calls(&body("    self.born_scratch.resize(rows * cols, None);"));
    assert!(hz.is_empty());
    let (hz, _) = term_hop_calls(&body(
        "    let mut term = Terminal::from_checkpoint(cp, host);\n    \
         term.resize(rows, cols);",
    ));
    assert!(hz.is_empty(), "local Terminal::resize must not flag");
    assert!(
        guard_vars(&body(
            "    let mut term = Terminal::from_checkpoint(cp, host);"
        ))
        .is_empty()
    );
}

#[test]
fn guard_vars_captures_the_term_lock_whole_binding() {
    let g = guard_vars(&body("        let t = crate::term_lock_whole(term);"));
    assert!(
        g.contains("t"),
        "crate::term_lock_whole( binding yields `t`"
    );
    let g = guard_vars(&body("        let mut t = term_lock_whole(term);"));
    assert!(g.contains("t"), "bare term_lock_whole( binding yields `t`");
    let (hz, _) = term_hop_calls(&body(
        "    let p = crate::term_lock_whole(term).resize(rows, cols);",
    ));
    assert_eq!(hz.len(), 1, "chained term_lock_whole(..).resize( must flag");
    let g = guard_vars(&body(
        "        let t = crate::term_lock_whole_when(term, |_| on == ReadOn::Control);",
    ));
    assert!(
        g.contains("t"),
        "crate::term_lock_whole_when( binding yields `t`"
    );
    let g = guard_vars(&body(
        "        let mut t = term_lock_whole_when(term, waits);",
    ));
    assert!(
        g.contains("t"),
        "bare term_lock_whole_when( binding yields `t`"
    );
    let (hz, _) = term_hop_calls(&body(
        "    let p = crate::term_lock_whole_when(term, |_| true).resize(rows, cols);",
    ));
    assert_eq!(
        hz.len(),
        1,
        "chained term_lock_whole_when(..).resize( must flag"
    );
}

#[test]
fn any_match_tries_exactly_the_starts_match_indices_yields() {
    for (hay, needle) in [
        ("aaaa", "aa"),
        ("tttterm term", "term"),
        ("ééx é", "éx"),
        ("ReadOn::Control, ReadOn::Committed)", "ReadOn::C"),
        ("ab", ""),
        ("", "x"),
    ] {
        let mut tried = Vec::new();
        assert!(!any_match(hay, needle, |at| {
            tried.push(at);
            false
        }));
        let starts: Vec<usize> = hay.match_indices(needle).map(|(at, _)| at).collect();
        assert_eq!(tried, starts, "{needle:?} in {hay:?}");
        assert_eq!(
            mentions(hay, needle),
            hay.contains(needle),
            "{needle:?} in {hay:?}"
        );
    }
    let mut tried = Vec::new();
    assert!(any_match("f(a(b(", "(", |at| {
        tried.push(at);
        at == 3
    }));
    assert_eq!(tried, [1, 3], "the scan stops at the first start that fits");
}

#[test]
fn guard_vars_captures_the_term_lock_ui_binding() {
    let g = guard_vars(&body(
        "            let mut term = crate::term_lock_ui(&front_terminal.term, ui_waiting, site);",
    ));
    assert!(
        g.contains("term"),
        "crate::term_lock_ui( binding yields `term`"
    );
    let g = guard_vars(&body(
        "        let mut t = term_lock_ui(&term, &ui_waiting, site);",
    ));
    assert!(g.contains("t"), "bare term_lock_ui( binding yields `t`");
    let (hz, _) = term_hop_calls(&body(
        "    let mut term = crate::term_lock_ui(&s.term, w, site);\n    term.resize(r, c);",
    ));
    assert_eq!(
        hz.len(),
        1,
        "resize on a term_lock_ui-bound guard must flag"
    );
    assert_eq!(hz[0].method, "resize");
    let (hz, _) = term_hop_calls(&body(
        "    crate::term_lock_ui(&s.term, w, site).resize(r, c);",
    ));
    assert_eq!(hz.len(), 1, "chained term_lock_ui(..).resize( must flag");
}

fn synth_support_files() -> Vec<(String, String)> {
    let m = "// COST: UNBOUNDED";
    vec![
        (
            "crates/aterm-grid/src/grid/reflow.rs".to_string(),
            format!("{m}(scrollback-width-reflow)\npub fn resize_with_reflow_mode() {{}}\n"),
        ),
        (
            "crates/aterm-grid/src/grid/scrollback_reflow.rs".to_string(),
            format!(
                "{m}(ring+tiered-history-lines)\npub fn take_scrollback_lines() {{}}\n\
                 {m}(session-history-cells)\npub fn reflow_scrollback_lines() {{}}\n"
            ),
        ),
        (
            "crates/aterm-core/src/terminal/callback_setters.rs".to_string(),
            "pub fn resize_offloading_scrollback() {}\n\
             pub fn finish_resize_offload() {}\n\
             pub fn abort_resize_offload() {}\n"
                .to_string(),
        ),
        (
            "crates/aterm-grid/src/grid/scrollback_offload.rs".to_string(),
            "pub fn reattach_reflowed_scrollback() {}\n".to_string(),
        ),
        (
            "crates/aterm-control/src/lib.rs".to_string(),
            "// stub\n".to_string(),
        ),
        (
            "crates/aterm-diskwalk/src/walk.rs".to_string(),
            format!(
                "{m}(entries)\npub fn diskwalk_slice() {{}}\n\
                 {m}(entries)\npub fn diskwalk_size_full() {{}}\n"
            ),
        ),
        (
            "crates/aterm-gui/src/control_input.rs".to_string(),
            "fn input_if_fenced_observed() {\n    \
             let terminal = term_lock_whole_when(term, Lane::SERVED, |_| waits);\n}\n"
                .to_string(),
        ),
        (
            "crates/aterm-gui/src/control_query.rs".to_string(),
            "fn search_terminal() {\n    \
             let t = term_lock_whole_when(term, Lane::SERVED, |_| on == ReadOn::Control);\n}\n"
                .to_string(),
        ),
        (
            "crates/aterm-gui/src/control_lanes.rs".to_string(),
            "fn serve_here() {\n    cmd_text(term);\n}\n\
             fn cmd_text() {\n    let t = crate::term_lock_whole(term);\n}\n"
                .to_string(),
        ),
        (
            "crates/aterm-gui/src/task_launch.rs".to_string(),
            "fn run_worker_launch() {}\n".to_string(),
        ),
    ]
}

fn synth_manifests() -> Vec<(String, String)> {
    let mut files = crate::scan_set::test_fixtures::workspace_manifests(&[
        ("aterm-core", ""),
        ("aterm-grid", ""),
        ("aterm-diskwalk", ""),
    ]);
    for stub in [
        "crates/aterm-types/src/lib.rs",
        "crates/aterm-core/src/lib.rs",
        "crates/aterm-grid/src/lib.rs",
        "crates/aterm-diskwalk/src/lib.rs",
    ] {
        files.push((stub.to_string(), "// stub\n".to_string()));
    }
    files
}

fn synth_base() -> Vec<(String, String)> {
    let mut files = synth_manifests();
    files.extend(synth_support_files());
    files
}

fn synth_gui(extra: &str) -> String {
    format!(
        "fn new_events() {{}}\n\
         fn resumed() {{}}\n\
         fn user_event() {{\n    window_event();\n}}\n\
         fn window_event() {{\n    resize_helper();\n}}\n\
         fn about_to_wait() {{}}\n\
         fn flush_pending_resize() {{}}\n\
         {extra}"
    )
}

#[test]
fn synthetic_offloaded_tree_is_green() {
    let mut files = synth_base();
    let gui = synth_gui(
        "fn resize_helper() {\n    \
         let pending = term_lock(&s.term).resize_offloading_scrollback(rows, cols);\n}\n",
    );
    files.push(("crates/aterm-gui/src/main.rs".to_string(), gui));
    let root = synth_tree("aterm-census", "green", &files);
    let out = run_mainloop_census(&root);
    assert!(out.ok, "expected GREEN, got:\n{}", out.log);
    assert!(
        out.log.contains("main-loop census: GREEN"),
        "log:\n{}",
        out.log
    );
    assert!(
        out.log
            .contains("the DERIVED GUI-process closure (5 workspace crates"),
        "the transcript must state the derived marker-sweep scope; log:\n{}",
        out.log
    );
    assert!(
        out.log.contains("0 out-of-closure marker(s) reported"),
        "log:\n{}",
        out.log
    );
}

#[test]
fn synthetic_prefix_resize_shape_is_red_with_path() {
    let mut files = synth_base();
    let gui = synth_gui(
        "fn resize_helper() {\n    \
         let mut term = term_lock(&s.term);\n    \
         term.resize(sub_rows, sub_cols);\n}\n\
         fn unrelated_offload() {\n    \
         term_lock(&s.term).finish_resize_offload(reflowed);\n}\n",
    );
    files.push(("crates/aterm-gui/src/main.rs".to_string(), gui));
    let root = synth_tree("aterm-census", "red-ob5", &files);
    let out = run_mainloop_census(&root);
    assert!(!out.ok, "expected RED, got:\n{}", out.log);
    assert!(out.log.contains("[OB-5]"), "log:\n{}", out.log);
    assert!(
        out.log.contains("window_event -> resize_helper"),
        "the diagnostic must print the root -> hazard path; log:\n{}",
        out.log
    );
    assert!(
        out.log.contains("term.resize(sub_rows, sub_cols);"),
        "the diagnostic must quote the offending line; log:\n{}",
        out.log
    );
    assert!(
        out.log.contains("HOW TO REPAIR"),
        "the diagnostic must carry the repair options; log:\n{}",
        out.log
    );
    assert!(
        out.log.contains("main-loop census: FAILED — "),
        "the verdict line must name the census; log:\n{}",
        out.log
    );
}

#[test]
fn a_whole_frame_read_the_event_loop_reaches_is_red_ob23() {
    let tree = |name: &str, read: &str| {
        let mut files = synth_base();
        let gui = synth_gui(&format!(
            "fn resize_helper() {{\n    \
             let pending = term_lock(&s.term).resize_offloading_scrollback(r, c);\n    \
             claude_rows_now();\n    \
             input_if_fenced_observed();\n    \
             search_terminal();\n    \
             serve_here();\n}}\n\
             fn claude_rows_now() {{\n    let t = {read}(&entry.term);\n}}\n"
        ));
        files.push(("crates/aterm-gui/src/main.rs".to_string(), gui));
        run_mainloop_census(&synth_tree("aterm-census", name, &files))
    };
    let out = tree("red-ob23", "crate::term_lock_whole");
    assert!(!out.ok, "expected RED, got:\n{}", out.log);
    assert!(
        out.log
            .contains("PATH:  window_event -> resize_helper -> claude_rows_now"),
        "log:\n{}",
        out.log
    );
    assert!(
        out.verdict
            .contains("(0 unbounded-work site(s), 1 frame wait(s))"),
        "{}",
        out.log
    );
    let out = tree("green-ob23", "crate::term_lock");
    assert!(out.ok, "expected GREEN, got:\n{}", out.log);
}

#[test]
fn a_waiting_read_on_the_event_loop_hands_a_gate_is_red_ob23() {
    let tree = |name: &str, on: &str| {
        let mut files = synth_base();
        let gui = synth_gui(&format!(
            "fn resize_helper() {{\n    \
             let pending = term_lock(&s.term).resize_offloading_scrollback(r, c);\n    \
             claude_rows_now();\n}}\n\
             fn claude_rows_now() {{\n    \
             input_if_fenced_observed(term, ctx, ev, &fence, crate::{on}, None);\n}}\n"
        ));
        files.push(("crates/aterm-gui/src/main.rs".to_string(), gui));
        run_mainloop_census(&synth_tree("aterm-census", name, &files))
    };
    for (name, on) in [
        ("red-ob23-control", "ReadOn::Control"),
        ("red-ob23-committed", "ReadOn::Committed"),
    ] {
        let out = tree(name, on);
        assert!(!out.ok, "expected RED for {on}, got:\n{}", out.log);
        assert!(
            out.log
                .contains("PATH:  window_event -> resize_helper -> claude_rows_now"),
            "log:\n{}",
            out.log
        );
    }
    let out = tree("green-ob23-loop", "ReadOn::EventLoop");
    assert!(out.ok, "expected GREEN, got:\n{}", out.log);
    let gui = synth_gui(
        "fn resize_helper() {\n    \
         let pending = term_lock(&s.term).resize_offloading_scrollback(r, c);\n    \
         lane(on);\n}\n\
         fn lane(on: ReadOn) -> bool {\n    \
         let waits = on == ReadOn::Control;\n    \
         match on {\n        \
         ReadOn::Committed => waits,\n        \
         ReadOn::Control | ReadOn::EventLoop => !waits,\n    }\n}\n",
    );
    let mut files = synth_base();
    files.push(("crates/aterm-gui/src/main.rs".to_string(), gui));
    let out = run_mainloop_census(&synth_tree("aterm-census", "green-ob23-arms", &files));
    assert!(
        out.ok,
        "a pattern or a comparison hands no read: {}",
        out.log
    );
}

#[test]
fn a_gate_that_no_longer_waits_is_red_ob23() {
    let files: Vec<(String, String)> = synth_base()
        .into_iter()
        .map(|(rel, text)| {
            if rel.ends_with("control_query.rs") {
                (
                    rel,
                    "fn search_terminal() {\n    let t = term_lock(term);\n}\n".into(),
                )
            } else {
                (rel, text)
            }
        })
        .chain([(
            "crates/aterm-gui/src/main.rs".to_string(),
            synth_gui(
                "fn resize_helper() {\n    \
                 let pending = term_lock(&s.term).resize_offloading_scrollback(r, c);\n}\n",
            ),
        )])
        .collect();
    let out = run_mainloop_census(&synth_tree("aterm-census", "red-ob23-gate", &files));
    assert!(!out.ok, "expected RED, got:\n{}", out.log);
    assert!(
        out.log
            .contains("whole-frame gate `search_terminal` no longer takes a whole-frame read"),
        "log:\n{}",
        out.log
    );
}
#[test]
fn cfg_predicate_entails_unshipped_only_when_it_really_does() {
    for yes in [
        "#[cfg(test)]",
        "#[cfg(all(test, unix))]",
        "#[cfg(all(test, any(windows, target_os = \"linux\")))]",
        "#[cfg(all(unix, all(test, debug_assertions)))]",
    ] {
        assert!(cfg_attr_never_ships(yes), "`{yes}` must be masked");
    }
    for no in [
        "#[cfg(any(test, unix))]",
        "#[cfg(not(test))]",
        "#[cfg(all(not(test), unix))]",
        "#[cfg(feature = \"test\")]",
        "#[cfg(target_os = \"macos\")]",
        "#[cfg(all())]",
        "#[test]",
        "#[derive(Debug)]",
        "let x = 1;",
    ] {
        assert!(!cfg_attr_never_ships(no), "`{no}` must NOT be masked");
    }
}

#[test]
fn top_level_comma_split_ignores_nesting_and_string_literals() {
    assert_eq!(
        split_top_level_commas("test, any(windows, target_os = \"linux\")"),
        vec!["test", "any(windows, target_os = \"linux\")"]
    );
    assert_eq!(
        split_top_level_commas("target_os = \"a,b\", unix"),
        vec!["target_os = \"a,b\"", "unix"]
    );
    assert!(split_top_level_commas("  ").is_empty());
}

#[test]
fn conjunctive_cfg_test_helper_is_masked_not_graphed() {
    let mut files = synth_base();
    let gui = synth_gui(
        "fn resize_helper() {\n    \
         compose();\n}\n\
         fn compose() {}\n\
         fn unrelated_offload() {\n    \
         term_lock(&s.term).finish_resize_offload(reflowed);\n}\n\
         #[cfg(all(test, any(windows, target_os = \"linux\")))]\n\
         mod strip_band_landing_tests {\n    \
         fn compose() {\n        \
         let mut term = term_lock(&s.term);\n        \
         term.resize(rows, cols);\n    }\n}\n",
    );
    files.push(("crates/aterm-gui/src/main.rs".to_string(), gui));
    let root = synth_tree("aterm-census", "mask-cfg-all-test", &files);
    let out = run_mainloop_census(&root);
    assert!(
        out.ok,
        "a `cfg(all(test, ..))` helper never enters the shipped GUI \
         process and must not fail OB-5:\n{}",
        out.log
    );
}

#[test]
fn a_disjunctive_cfg_test_gate_is_not_masked_and_stays_red() {
    let mut files = synth_base();
    let gui = synth_gui(
        "fn resize_helper() {\n    \
         compose();\n}\n\
         fn unrelated_offload() {\n    \
         term_lock(&s.term).finish_resize_offload(reflowed);\n}\n\
         #[cfg(any(test, unix))]\n\
         mod maybe_shipped {\n    \
         fn compose() {\n        \
         let mut term = term_lock(&s.term);\n        \
         term.resize(rows, cols);\n    }\n}\n",
    );
    files.push(("crates/aterm-gui/src/main.rs".to_string(), gui));
    let root = synth_tree("aterm-census", "no-mask-any-test", &files);
    let out = run_mainloop_census(&root);
    assert!(
        !out.ok,
        "`any(test, unix)` compiles on a shipped unix build — the hazard \
         under it must still fail OB-5:\n{}",
        out.log
    );
    assert!(out.log.contains("[OB-5]"), "log:\n{}", out.log);
}

#[test]
fn synthetic_direct_sink_call_is_red_ob6() {
    let mut files = synth_base();
    let gui = synth_gui(
        "fn resize_helper() {\n    \
         let lines = reflow_scrollback_lines(&lines, cols);\n}\n\
         fn unrelated_offload() {\n    \
         term_lock(&s.term).finish_resize_offload(reflowed);\n}\n",
    );
    files.push(("crates/aterm-gui/src/main.rs".to_string(), gui));
    let root = synth_tree("aterm-census", "red-ob6", &files);
    let out = run_mainloop_census(&root);
    assert!(!out.ok, "expected RED, got:\n{}", out.log);
    assert!(out.log.contains("[OB-6]"), "log:\n{}", out.log);
    assert!(
        out.log.contains("reflow_scrollback_lines"),
        "log:\n{}",
        out.log
    );
}

#[test]
fn synthetic_missing_boundary_def_is_red_ob3_and_missing_offload_is_red_ob4() {
    let files: Vec<(String, String)> = synth_base()
        .into_iter()
        .filter(|(rel, _)| !rel.ends_with("callback_setters.rs"))
        .chain([(
            "crates/aterm-gui/src/main.rs".to_string(),
            synth_gui("fn resize_helper() {}\n"),
        )])
        .collect();
    let root = synth_tree("aterm-census", "red-ob3-ob4", &files);
    let out = run_mainloop_census(&root);
    assert!(!out.ok, "expected RED, got:\n{}", out.log);
    assert!(out.log.contains("[OB-3]"), "log:\n{}", out.log);
    assert!(out.log.contains("[OB-4]"), "log:\n{}", out.log);
}

#[test]
fn synthetic_missing_root_is_red_ob2() {
    let mut files = synth_base();
    files.push((
        "crates/aterm-gui/src/main.rs".to_string(),
        "fn new_events() {}\n\
         fn resumed() {}\n\
         fn user_event() {}\n\
         fn window_event() {}\n\
         fn about_to_wait() {}\n\
         fn some_offload() {\n    \
         term_lock(&s.term).resize_offloading_scrollback(r, c);\n}\n"
            .to_string(),
    ));
    let root = synth_tree("aterm-census", "red-ob2", &files);
    let out = run_mainloop_census(&root);
    assert!(!out.ok, "expected RED, got:\n{}", out.log);
    assert!(out.log.contains("[OB-2]"), "log:\n{}", out.log);
    assert!(
        out.log.contains("flush_pending_resize"),
        "log:\n{}",
        out.log
    );
}

#[test]
fn out_of_closure_marker_is_not_swept_and_is_reported() {
    let m = "// COST: UNBOUNDED";
    let mut files = synth_base();
    files.push((
        "crates/aterm-gui/src/main.rs".to_string(),
        synth_gui(
            "fn resize_helper() {\n    \
             term_lock(&s.term).resize_offloading_scrollback(rows, cols);\n}\n",
        ),
    ));
    files.push((
        "crates/aterm-clitool/src/main.rs".to_string(),
        format!("{m}(whole-archive-dump)\npub fn dump_everything() {{}}\n"),
    ));
    let root = synth_tree("aterm-census", "out-of-closure", &files);
    let out = run_mainloop_census(&root);
    assert!(
        out.ok,
        "an out-of-closure marker must NOT enter OB-1 coherence; log:\n{}",
        out.log
    );
    assert!(
        out.log.contains("NOTE [OB-1 scope]")
            && out.log.contains("dump_everything")
            && out.log.contains("crates/aterm-clitool/src/main.rs:1"),
        "the out-of-closure marker must be REPORTED with its span; log:\n{}",
        out.log
    );
    assert!(
        out.log.contains("1 out-of-closure marker(s) reported"),
        "log:\n{}",
        out.log
    );
}

#[test]
fn out_of_closure_marker_cannot_satisfy_the_registry() {
    let m = "// COST: UNBOUNDED";
    let mut files: Vec<(String, String)> = synth_base()
        .into_iter()
        .map(|(rel, contents)| {
            if rel.ends_with("grid/reflow.rs") {
                (rel, "pub fn resize_with_reflow_mode() {}\n".to_string())
            } else {
                (rel, contents)
            }
        })
        .collect();
    files.push((
        "crates/aterm-gui/src/main.rs".to_string(),
        synth_gui(
            "fn resize_helper() {\n    \
             term_lock(&s.term).resize_offloading_scrollback(rows, cols);\n}\n",
        ),
    ));
    files.push((
        "crates/aterm-clitool/src/lib.rs".to_string(),
        format!("{m}(scrollback-width-reflow)\npub fn resize_with_reflow_mode() {{}}\n"),
    ));
    let root = synth_tree("aterm-census", "moved-marker", &files);
    let out = run_mainloop_census(&root);
    assert!(!out.ok, "expected RED, got:\n{}", out.log);
    assert!(
        out.log.contains("[OB-1]")
            && out.log.contains("has LOST its")
            && out.log.contains("resize_with_reflow_mode"),
        "the registry must not be satisfiable from outside the closure; log:\n{}",
        out.log
    );
    assert!(
        out.log.contains("NOTE [OB-1 scope]"),
        "the stray out-of-closure marker must still be reported; log:\n{}",
        out.log
    );
}

#[test]
fn underivable_workspace_fails_the_mainloop_census_closed() {
    let files: Vec<(String, String)> = synth_base()
        .into_iter()
        .filter(|(rel, _)| rel != "Cargo.toml")
        .chain([(
            "crates/aterm-gui/src/main.rs".to_string(),
            synth_gui("fn resize_helper() {}\n"),
        )])
        .collect();
    let root = synth_tree("aterm-census", "underivable", &files);
    let out = run_mainloop_census(&root);
    assert!(!out.ok, "expected RED, got:\n{}", out.log);
    assert!(
        out.log.contains("SCAN-SET DERIVATION FAILED"),
        "log:\n{}",
        out.log
    );
}
