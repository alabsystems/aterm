// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The kickoff names the module and its arguments as JSON literals, so a
/// quote in either is data, and settles into `window.atermRun`.
#[test]
fn the_kickoff_names_its_module_and_arguments_as_literals() {
    let k = kickoff("http://127.0.0.1:1/a'b\".mjs", &json!({"x": "</script>"})).unwrap();
    assert!(
        k.contains(r#"import("http://127.0.0.1:1/a'b\".mjs")"#),
        "{k}"
    );
    assert!(k.contains(r#"m.run({"x":"</script>"})"#), "{k}");
    assert!(
        k.contains("window.atermRun = ") && k.contains(&format!("{SETTLED:?}")),
        "{k}"
    );
}

#[test]
fn a_drivers_lines_read_as_text_and_other_kinds_keep_theirs() {
    assert_eq!(
        console_lines(vec![
            "console.error: wasm-smoke: ok".into(),
            "console.log: GL_LIFETIME_ALL_RETIRED".into(),
            "console.warn: careful".into(),
            "uncaught: Error: x".into(),
        ]),
        [
            "wasm-smoke: ok",
            "GL_LIFETIME_ALL_RETIRED",
            "console.warn: careful",
            "uncaught: Error: x"
        ]
    );
}

/// A real browser: a module's answer comes back by value with its
/// console; one that throws is a finding naming the throw, with what it
/// printed first, and so is one that answers after a timer of its threw
/// or a rejection it left went unhandled (node's process failed on
/// both); and the tab before it leaves nothing for the next.
#[test]
fn a_module_answers_by_value_and_a_throw_is_a_finding() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("atv-page-")
        .suffix(".noindex")
        .tempdir()
        .expect("scratch");
    std::fs::write(
        dir.path().join("ok.mjs"),
        "export async function run(args) { console.error('from', args.who); \
         await new Promise((r) => setTimeout(r, 0)); \
         return { sum: args.a + 1, gc: typeof gc, isolated: crossOriginIsolated } }\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("throws.mjs"),
        "export async function run() { console.log('before'); throw new Error('planted') }\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("timer.mjs"),
        "export async function run() { setTimeout(() => { throw new Error('a timer threw') }, 0); \
         await new Promise((r) => setTimeout(r, 0)); return 1 }\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("dangling.mjs"),
        "export async function run() { \
         const seen = new Promise((r) => addEventListener('unhandledrejection', \
         () => setTimeout(r, 0), { once: true })); \
         Promise.reject(new Error('nobody handles this')); await seen; return 1 }\n",
    )
    .unwrap();
    let mut chrome = crate::wasm_page::start_for_test(dir.path());
    let ok = chrome
        .run("ok.mjs", &json!({"a": 41, "who": "rust"}))
        .expect("an answer");
    assert_eq!(
        ok.value,
        json!({"sum": 42, "gc": "function", "isolated": true})
    );
    assert_eq!(ok.console, ["from rust"]);
    let threw = chrome.run("throws.mjs", &json!({})).expect_err("a throw");
    let Refused::Failed(why) = threw else {
        panic!("a throw is a finding: {threw:?}")
    };
    assert!(
        why.contains("Error: planted") && why.contains("before"),
        "{why}"
    );
    for (module, thrown) in [
        ("timer.mjs", "a timer threw"),
        ("dangling.mjs", "nobody handles this"),
    ] {
        let threw = chrome.run(module, &json!({})).expect_err(module);
        let Refused::Failed(why) = threw else {
            panic!("{module}: a throw outside run is a finding: {threw:?}")
        };
        assert!(
            why.contains("threw outside its run") && why.contains(thrown),
            "{module}: {why}"
        );
    }
    let missing = chrome
        .run("missing.mjs", &json!({}))
        .expect_err("no module");
    assert!(matches!(missing, Refused::Failed(_)), "{missing:?}");
    assert!(chrome.product().contains("Chrome"), "{}", chrome.product());
    let again = chrome
        .run("ok.mjs", &json!({"a": 0, "who": "again"}))
        .unwrap();
    assert_eq!(again.console, ["from again"]);
    chrome.running.0.kill().expect("the test's own browser");
    let _ = chrome.running.0.wait();
    let dead = chrome
        .run("ok.mjs", &json!({"a": 0, "who": "dead"}))
        .expect_err("no browser");
    assert!(matches!(dead, Refused::Failed(_)), "{dead:?}");
}
