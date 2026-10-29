// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::super::tests::{run, spots};
use super::*;

const UNLABELLED_FUZZ: &str = "#![no_main]\nfuzz_target!(|data: &[u8]| { let _ = data; });\n";

const NO_LABEL: &str = ", and no comment labels it `STOCK EXCEPTION` or `PUBLIC BOUNDARY`";

#[test]
fn stock_code_without_a_label_is_found() {
    let labelled_fuzz =
        format!("//! STOCK EXCEPTION: Trust ships no cargo-fuzz.\n{UNLABELLED_FUZZ}");
    let env = "fn f(cmd: &mut Cmd) {\n    cmd.env(\"RUSTUP_TOOLCHAIN\", \"stable\");\n}\n";
    let wrapped = "fn f(cmd: &mut Cmd) {\n    cmd.env(\n        \"RUSTUP_TOOLCHAIN\",\n        \
                   \"nightly-2026-01-01\",\n    );\n}\n";
    let argv = "fn f(cmd: &mut Cmd) {\n    cmd.args([\"+nightly\", \"fuzz\", \"run\"]);\n}\n";
    let flag = "fn f(cmd: &mut Cmd) {\n    cmd.args([\n        \"target\",\n        \"add\",\n        \
                \"--toolchain\",\n        \"stable\",\n    ]);\n}\n";
    let run_sub =
        "fn f(cmd: &mut Cmd) {\n    cmd.args([\"run\", \"stable\", \"cargo\", \"build\"]);\n}\n";
    let api = "fn f(rustup: &Path) -> Tc {\n    \
               crate::rustup::Toolchain::resolve(rustup, \"stable\")\n}\n";
    let api_rustup = "fn f(at: &Path) -> Status {\n    rustup::install(at, \"beta\")\n}\n";
    let first = "fn f(cmd: &mut Cmd) {\n    cmd.env(\"RUSTUP_TOOLCHAIN\", \"stable\");\n    \
                 cmd.args([\"+nightly\", \"fuzz\"]);\n}\n";
    let in_string = "fn f(cmd: &mut Cmd) {\n    cmd.env(\"RUSTUP_TOOLCHAIN\", \"beta\");\n    \
                     panic!(\"needs stock beta (STOCK EXCEPTION)\");\n}\n";
    let labelled = "// PUBLIC BOUNDARY: the compat slice builds on stable.\n\
                    fn f(cmd: &mut Cmd) {\n    cmd.env(\"RUSTUP_TOOLCHAIN\", \"stable\");\n}\n";
    let not_stock = "fn f(cmd: &mut Cmd, t: &str) {\n    \
                     cmd.env(\"RUSTUP_TOOLCHAIN\", t);\n    \
                     cmd.env(\"RUSTUP_TOOLCHAIN\", \"stablex\");\n    \
                     cmd.args([\"--toolchain\", t, \"stable\"]);\n    \
                     cmd.args([\"run\", \"--release\"]);\n    \
                     let n = a +stable_count;\n    \
                     // cargo +nightly fuzz run decode\n    \
                     let s = \"runs stable\";\n    \
                     let fix = \"rustup +stable target add x86_64-apple-darwin\";\n    \
                     let fix = \"rustup run stable cargo check\";\n    \
                     let row = rust_toolchain_row(Some(\"stable\"));\n    \
                     let up = channel_of(\"stable\");\n    \
                     let names = toolchain_names(&[\"stable\", \"beta\"]);\n    \
                     let say = format!(\"{}\", \"\\\"+nightly\\\"\");\n    \
                     let say = \"\\\"--toolchain\\\", \\\"stable\\\"\";\n    \
                     let doc = r#\"run it with \"+nightly\" or \"RUSTUP_TOOLCHAIN\", \"stable\"\"#;\n}\n\
                     #[cfg(test)]\nmod tests {\n    \
                     fn t() { run(&[\"+nightly\", \"metadata\"]); toolchain(\"stable\"); }\n}\n";
    let found = run(
        &STOCK_LABEL,
        &[
            (
                "crates/aterm-a/fuzz/fuzz_targets/decode.rs",
                UNLABELLED_FUZZ,
            ),
            (
                "crates/aterm-b/fuzz/fuzz_targets/decode.rs",
                labelled_fuzz.as_str(),
            ),
            ("crates/aterm-c/src/env.rs", env),
            ("crates/aterm-c/src/wrapped.rs", wrapped),
            ("crates/aterm-c/src/argv.rs", argv),
            ("crates/aterm-c/src/flag.rs", flag),
            ("crates/aterm-c/src/run_sub.rs", run_sub),
            ("crates/aterm-c/src/api.rs", api),
            ("crates/aterm-c/src/api_rustup.rs", api_rustup),
            ("crates/aterm-c/src/first.rs", first),
            ("crates/aterm-c/src/in_string.rs", in_string),
            ("crates/aterm-c/src/labelled.rs", labelled),
            ("crates/aterm-c/src/not_stock.rs", not_stock),
            ("crates/aterm-c/src/lane_tests.rs", env),
        ],
    );
    assert_eq!(
        spots(&found),
        [
            "crates/aterm-a/fuzz/fuzz_targets/decode.rs:0",
            "crates/aterm-c/src/api.rs:2",
            "crates/aterm-c/src/api_rustup.rs:2",
            "crates/aterm-c/src/argv.rs:2",
            "crates/aterm-c/src/env.rs:2",
            "crates/aterm-c/src/first.rs:2",
            "crates/aterm-c/src/flag.rs:5",
            "crates/aterm-c/src/in_string.rs:2",
            "crates/aterm-c/src/run_sub.rs:2",
            "crates/aterm-c/src/wrapped.rs:3",
        ]
    );
    let details: Vec<&str> = found.iter().map(|f| f.detail.as_str()).collect();
    assert_eq!(
        details[1..],
        [
            "crate::rustup::Toolchain::resolve(.., \"stable\")",
            "rustup::install(.., \"beta\")",
            "\"+nightly\"",
            "\"RUSTUP_TOOLCHAIN\", \"stable\"",
            "\"RUSTUP_TOOLCHAIN\", \"stable\"",
            "\"--toolchain\", \"stable\"",
            "\"RUSTUP_TOOLCHAIN\", \"beta\"",
            "\"run\", \"stable\"",
            "\"RUSTUP_TOOLCHAIN\", \"nightly-2026-01-01\"",
        ]
        .map(|seen| format!("selects a stock toolchain: `{seen}`{NO_LABEL}"))
    );
}

/// A package whose `[[bin]]` names a root Cargo would not discover.
const PACKAGE_D: &str = "[package]\nname = \"aterm-d\"\nedition = \"2024\"\n\n\
                         [[bin]]\nname = \"aterm-d-windowed\"\npath = \"src/windowed.rs\"\n";

/// A test suite under `autotests = false`, and two tests declared by name alone.
const PACKAGE_E: &str = "[package]\nname = \"aterm-e\"\nautotests = false\n\n\
                         [[test]]\nname = \"suite\"\npath = \"tests/suite.rs\"\n\n\
                         [[test]]\nname = \"alone\"\n\n[[test]]\nname = \"nested\"\n";

/// Binary discovery off: the one binary named after the package roots at `src/main.rs`, and
/// one named otherwise does not.
const PACKAGE_I: &str = "[package]\nname = \"aterm-i\"\nautobins = false\n\n\
                         [[bin]]\nname = \"aterm-i\"\n";
const PACKAGE_J: &str = "[package]\nname = \"aterm-j\"\nautobins = false\n\n\
                         [[bin]]\nname = \"tool\"\n";

/// A library rooted at the file Cargo also discovers as the package's binary.
const PACKAGE_K: &str = "[package]\nname = \"aterm-k\"\n\n[lib]\npath = \"src/main.rs\"\n";

/// A `fn main` whose work is Windows-only, and which only reports anywhere else.
const STUB: &str =
    "fn main() {\n    #[cfg(windows)]\n    win::run();\n    eprintln!(\"Windows only\");\n}\n";

/// A target root gated whole to Linux.
const WHOLE: &str =
    "//! A live bus test.\n\n#![cfg(target_os = \"linux\")]\n\n#[test]\nfn t() {}\n";

/// A Windows image's `fn main`, and its fallback everywhere else.
const WINDOWED: &str = "#[cfg(all(windows, not(test)))]\nfn main() {\n    front_door::main()\n}\n\n\
                        #[cfg(all(not(windows), not(test)))]\nfn main() -> std::process::ExitCode {\n    \
                        eprintln!(\"aterm-d-windowed is the Windows image; run aterm-d\");\n    \
                        std::process::ExitCode::from(2)\n}\n";

/// A Linux agent's entry, and the seam that serves in its place everywhere else.
const AGENT: &str = "#[cfg(target_os = \"linux\")]\npub fn main() -> ExitCode {\n    linux::run()\n}\n\n\
                     #[cfg(not(target_os = \"linux\"))]\npub fn main() -> ExitCode {\n    \
                     eprintln!(\"the seam\");\n    ExitCode::from(2)\n}\n";

#[test]
fn a_target_with_no_work_on_trusts_host_is_found() {
    let whole_labelled = format!("//! STOCK EXCEPTION: Linux only.\n{WHOLE}");
    let bench = "#[path = \"support.rs\"]\nmod support;\n\n#[cfg(target_os = \"linux\")]\n\
                 mod live {\n    pub fn run() {}\n}\n\nfn main() {\n    \
                 #[cfg(target_os = \"linux\")]\n    live::run();\n    \
                 #[cfg(not(target_os = \"linux\"))]\n    eprintln!(\"requires Linux\");\n}\n";
    let harness = "#[path = \"support.rs\"]\nmod support;\n\n\
                   fn main() -> std::process::ExitCode {\n    \
                   support::run(\"rows\", &[(\"row\", ())], |row| {\n        \
                   #[cfg(target_os = \"linux\")]\n        let ran = match row {\n            \
                   () => live::run(),\n        };\n        \
                   #[cfg(target_os = \"linux\")]\n        match ran {\n            \
                   Ok(detail) => support::Outcome::Pass(detail),\n            \
                   Err(why) => support::Outcome::CouldNotRun(why),\n        }\n        \
                   #[cfg(not(target_os = \"linux\"))]\n        {\n            let _ = row;\n            \
                   support::Outcome::CouldNotRun(\"requires Linux\".into())\n        }\n    })\n}\n";
    let per_os_main = "#[cfg(windows)]\nfn main() {}\n\n#[cfg(not(windows))]\nfn main() {\n    \
                       eprintln!(\"Windows only\");\n}\n";
    let wrapped = "fn main() {\n    let ran = {\n        \
                   #[cfg(all(windows, debug_assertions))]\n        {\n            1\n        }\n        \
                   #[cfg(not(all(\n            windows,\n            debug_assertions,\n        )))]\n        \
                   {\n            0\n        }\n    };\n    let _ = ran;\n}\n";
    let client = "fn main() -> std::process::ExitCode {\n    #[cfg(windows)]\n    \
                  if let Some(code) = serve() {\n        return code;\n    }\n    \
                  std::process::ExitCode::from(64)\n}\n";
    let visible = "pub(crate) fn main() {\n    #[cfg(windows)]\n    win::run();\n    \
                   #[cfg(not(windows))]\n    eprintln!(\"Windows only\");\n}\n";
    let typed = "fn main() {\n    #[cfg(windows)]\n    let r: Result<u8, String> = win::run();\n    \
                 #[cfg(not(windows))]\n    eprintln!(\"Windows only\");\n}\n";
    let feature_gated = "#![cfg(all(target_os = \"linux\", feature = \"x\"))]\nfn main() {}\n";
    let lane_bus = "fn main() {\n    #[cfg(unix)]\n    {\n        run();\n    }\n    \
                    #[cfg(not(unix))]\n    {\n        eprintln!(\"unix only\");\n    }\n}\n";
    let portable_main = "fn main() {\n    #[cfg(not(target_os = \"macos\"))]\n    \
                         eprintln!(\"macOS only\");\n    run(&[]);\n}\n\n\
                         fn run(args: &[u8]) {\n    #[cfg(windows)]\n    windows(args);\n    \
                         #[cfg(not(windows))]\n    let _ = args;\n}\n";
    let portable_arms = "fn main() {\n    #[cfg(windows)]\n    let _ = attach_console();\n    \
                         #[cfg(not(windows))]\n    let _ = ();\n    serve();\n}\n";
    let unix_arm = "pub fn main() -> ExitCode {\n    #[cfg(windows)]\n    {\n        win::run()\n    }\n    \
                    #[cfg(not(windows))]\n    {\n        unix::serve()\n    }\n}\n";
    let stub_elsewhere = "#[cfg(windows)]\nfn main() -> ExitCode {\n    eprintln!(\"unix only\");\n    \
                          ExitCode::FAILURE\n}\n\n#[cfg(not(windows))]\nfn main() -> ExitCode {\n    \
                          serve()\n}\n";
    let featured = "fn main() {\n    #[cfg(windows)]\n    win::run();\n    \
                    #[cfg(feature = \"x\")]\n    other();\n    eprintln!(\"Windows only\");\n}\n";
    let turbofish = "fn main() {\n    #[cfg(windows)]\n    win::run();\n    \
                     let n = parse::<u8>(\"1\");\n}\n";
    let or_arg = "fn main() {\n    #[cfg(windows)]\n    win::run();\n    check(a || b);\n}\n";
    let module = "#![cfg(target_os = \"linux\")]\npub fn f() {}\n";
    let open = "#![cfg(any(target_os = \"linux\", feature = \"x\"))]\n#[test]\nfn t() {}\n";
    let unix = "#![cfg(unix)]\n#[test]\nfn t() {}\n";
    let nested = "mod w {\n    #![cfg(windows)]\n    pub fn f() {}\n}\n\n\
                  mod lane {\n    pub fn main() {\n        \
                  #[cfg(not(target_os = \"linux\"))]\n        eprintln!(\"x\");\n    }\n}\n\n\
                  fn main() {\n    lane::main();\n}\n";
    let in_string = "const S: &str = \"#![cfg(windows)]\";\nfn main() {\n    \
                     let s = \"#[cfg(not(windows))]\";\n}\n";
    let suite =
        "mod live;\n#[path = \"gated.rs\"]\n#[cfg(unix)]\nmod renamed;\n#[test]\nfn t() {}\n";
    let result_arms = "#[cfg(windows)]\nfn main() -> ExitCode {\n    win::run()\n}\n\n\
                       #[cfg(not(windows))]\nfn main() -> Result<(), String> {\n    \
                       serve()?;\n    Ok(())\n}\n";
    let result_stub = "fn main() -> Result<(), String> {\n    #[cfg(windows)]\n    \
                       return win::run();\n    #[cfg(not(windows))]\n    \
                       Err(\"Windows only\".into())\n}\n";
    let result_swap = "#[cfg(windows)]\nfn main() -> Result<(), String> {\n    win::run()\n}\n\n\
                       #[cfg(not(windows))]\nfn main() -> Result<(), String> {\n    \
                       Err(\"Windows only\".into())\n}\n";
    let gated_static = "fn main() {\n    #[cfg(windows)]\n    \
                        static TABLE: Map<u8, u8> = build();\n    \
                        eprintln!(\"Windows only\");\n}\n";
    let closure_server = "fn main() {\n    #[cfg(windows)]\n    attach_console();\n    \
                          server::serve(|req| Reply::Ok(req));\n}\n";
    let late_harness = "use support::{harness, Outcome};\nuse std::process::ExitCode;\n\
                        use std::time::{Duration, Instant};\nuse std::collections::BTreeMap;\n\n\
                        fn main() {\n    harness(\"rows\", |row| {\n        #[cfg(windows)]\n        \
                        win::run(row);\n        Outcome::Skip\n    });\n}\n";
    let inner_gate = "fn main() {\n    #![cfg(windows)]\n    win::init();\n    win::run();\n}\n\n\
                      #[cfg(not(windows))]\nfn main() {\n    eprintln!(\"Windows only\");\n}\n";
    let inner_host = "fn main() {\n    #![cfg(unix)]\n    serve();\n}\n\n\
                      #[cfg(not(unix))]\nfn main() {\n    eprintln!(\"unix only\");\n}\n";
    let nested_inner =
        "fn main() {\n    serve();\n    mod w {\n        #![cfg(windows)]\n    }\n}\n";
    let found = run(
        &STOCK_LABEL,
        &[
            ("crates/aterm-d/Cargo.toml", PACKAGE_D),
            ("crates/aterm-d/tests/live_bus.rs", WHOLE),
            (
                "crates/aterm-d/tests/live_labelled.rs",
                whole_labelled.as_str(),
            ),
            ("crates/aterm-d/benches/accept.rs", bench),
            ("crates/aterm-d/benches/harness.rs", harness),
            ("crates/aterm-d/benches/per_os_main.rs", per_os_main),
            ("crates/aterm-d/benches/wrapped.rs", wrapped),
            ("crates/aterm-d/src/bin/client.rs", client),
            ("crates/aterm-d/src/bin/visible.rs", visible),
            ("crates/aterm-d/src/bin/typed.rs", typed),
            ("crates/aterm-d/src/bin/agent.rs", AGENT),
            ("crates/aterm-d/src/lib.rs", AGENT),
            ("crates/aterm-d/src/windowed.rs", WINDOWED),
            ("crates/aterm-d/src/unlisted.rs", WINDOWED),
            ("crates/aterm-d/examples/feature_gated.rs", feature_gated),
            ("crates/aterm-d/src/bin/lane-bus/main.rs", lane_bus),
            ("crates/aterm-d/src/main.rs", portable_main),
            ("crates/aterm-d/src/bin/portable.rs", portable_arms),
            ("crates/aterm-d/src/bin/unix_arm.rs", unix_arm),
            ("crates/aterm-d/src/bin/keys.rs", stub_elsewhere),
            ("crates/aterm-d/src/bin/featured.rs", featured),
            ("crates/aterm-d/src/bin/turbofish.rs", turbofish),
            ("crates/aterm-d/src/bin/or_arg.rs", or_arg),
            ("crates/aterm-d/src/linux.rs", module),
            ("crates/aterm-d/src/console/tests/win.rs", WHOLE),
            ("crates/aterm-d/tests/open.rs", open),
            ("crates/aterm-d/tests/unix.rs", unix),
            ("crates/aterm-d/examples/nested.rs", nested),
            ("crates/aterm-d/benches/in_string.rs", in_string),
            ("crates/aterm-d/src/bin/result_arms.rs", result_arms),
            ("crates/aterm-d/src/bin/result_stub.rs", result_stub),
            ("crates/aterm-d/src/bin/result_swap.rs", result_swap),
            ("crates/aterm-d/src/bin/gated_static.rs", gated_static),
            ("crates/aterm-d/src/bin/closure_server.rs", closure_server),
            ("crates/aterm-d/benches/late_harness.rs", late_harness),
            ("crates/aterm-d/src/bin/inner_gate.rs", inner_gate),
            ("crates/aterm-d/src/bin/inner_host.rs", inner_host),
            ("crates/aterm-d/src/bin/nested_inner.rs", nested_inner),
            ("crates/aterm-d/src/bin/dirbin/main.rs", STUB),
            ("crates/aterm-d/src/bin/dirbin/helper.rs", WHOLE),
            ("crates/aterm-d/tests/dirtest/main.rs", STUB),
            ("crates/aterm-d/tests/dirtest/support.rs", WHOLE),
            ("crates/aterm-d/benches/dirbench/main.rs", STUB),
            ("crates/aterm-d/examples/direx/main.rs", STUB),
            ("crates/aterm-e/Cargo.toml", PACKAGE_E),
            ("crates/aterm-e/tests/suite.rs", suite),
            ("crates/aterm-e/tests/live.rs", WHOLE),
            ("crates/aterm-e/tests/gated.rs", WHOLE),
            ("crates/aterm-e/tests/alone.rs", WHOLE),
            ("crates/aterm-e/tests/nested/main.rs", STUB),
            ("crates/aterm-f/Cargo.toml", "[workspace]\nmembers = []\n"),
            ("crates/aterm-f/tests/live.rs", WHOLE),
            ("crates/aterm-g/tests/live.rs", WHOLE),
            (
                "crates/aterm-h/Cargo.toml",
                "[package\nname = \"aterm-h\"\n",
            ),
            ("crates/aterm-i/Cargo.toml", PACKAGE_I),
            ("crates/aterm-i/src/main.rs", WHOLE),
            ("crates/aterm-j/Cargo.toml", PACKAGE_J),
            ("crates/aterm-j/src/main.rs", WHOLE),
            ("crates/aterm-j/src/bin/other.rs", WHOLE),
            ("crates/aterm-k/Cargo.toml", PACKAGE_K),
            ("crates/aterm-k/src/main.rs", STUB),
        ],
    );
    assert_eq!(
        spots(&found),
        [
            "crates/aterm-d/benches/accept.rs:10",
            "crates/aterm-d/benches/dirbench/main.rs:2",
            "crates/aterm-d/benches/harness.rs:6",
            "crates/aterm-d/benches/late_harness.rs:8",
            "crates/aterm-d/benches/per_os_main.rs:1",
            "crates/aterm-d/benches/wrapped.rs:3",
            "crates/aterm-d/examples/direx/main.rs:2",
            "crates/aterm-d/examples/feature_gated.rs:1",
            "crates/aterm-d/src/bin/agent.rs:1",
            "crates/aterm-d/src/bin/client.rs:2",
            "crates/aterm-d/src/bin/dirbin/main.rs:2",
            "crates/aterm-d/src/bin/gated_static.rs:2",
            "crates/aterm-d/src/bin/inner_gate.rs:2",
            "crates/aterm-d/src/bin/result_stub.rs:2",
            "crates/aterm-d/src/bin/result_swap.rs:1",
            "crates/aterm-d/src/bin/typed.rs:2",
            "crates/aterm-d/src/bin/visible.rs:2",
            "crates/aterm-d/src/windowed.rs:1",
            "crates/aterm-d/tests/dirtest/main.rs:2",
            "crates/aterm-d/tests/live_bus.rs:3",
            "crates/aterm-e/tests/alone.rs:3",
            "crates/aterm-e/tests/nested/main.rs:2",
            "crates/aterm-h/Cargo.toml:0",
            "crates/aterm-i/src/main.rs:3",
            "crates/aterm-k/src/main.rs:2",
        ]
    );
    let detail = |path: &str| {
        found
            .iter()
            .find(|f| f.path.ends_with(path))
            .map_or("", |f| f.detail.as_str())
    };
    let fallback = |pred: &str| {
        format!(
            "a Cargo target whose `fn main` only reports on Trust's host: its work is gated to \
             `{pred}`, which Trust ships no std for{NO_LABEL}"
        )
    };
    assert_eq!(
        detail("benches/wrapped.rs"),
        fallback("all(windows, debug_assertions)")
    );
    assert_eq!(
        detail("src/windowed.rs"),
        fallback("all(windows, not(test))")
    );
    assert_eq!(detail("result_swap.rs"), fallback("windows"));
    assert_eq!(
        detail("aterm-d/tests/live_bus.rs"),
        format!(
            "a Cargo target that builds only for `target_os = \"linux\"`, which Trust ships no \
             std for{NO_LABEL}"
        )
    );
    let manifest = detail("aterm-h/Cargo.toml");
    assert!(manifest.starts_with("unreadable manifest"), "{manifest}");
}

#[test]
fn an_item_ends_where_rust_ends_it() {
    for (code, item) in [
        (
            "let x = match y { A => 1, B => 2 };\nnext();",
            "let x = match y { A => 1, B => 2 };",
        ),
        (
            "let r: Result<u8, E> = f();\nnext();",
            "let r: Result<u8, E> = f();",
        ),
        (
            "let x = if a { 1 } else { 2 };\nnext();",
            "let x = if a { 1 } else { 2 };",
        ),
        (
            "if a { b(); } else { c(); }\nnext();",
            "if a { b(); } else { c(); }",
        ),
        ("{ work() }\n#[cfg(x)]\n{ 0 }", "{ work() }"),
        ("{ a }.b();\nnext();", "{ a }.b();"),
        ("tail(1)\n}", "tail(1)\n"),
        ("A => work(),\nB => 0,", "A => work(),"),
        (
            "#[allow(x)]\nlet a = f(1, [2]);\nnext();",
            "#[allow(x)]\nlet a = f(1, [2]);",
        ),
        (
            "fn main() -> T {\n    body();\n}\n\nfn other() {}",
            "fn main() -> T {\n    body();\n}",
        ),
        (
            "fn main() -> Result<(), String> {\n    serve()?;\n    Ok(())\n}\n\nfn other() {}",
            "fn main() -> Result<(), String> {\n    serve()?;\n    Ok(())\n}",
        ),
        (
            "static T: Map<u8, u8> = build();\nnext();",
            "static T: Map<u8, u8> = build();",
        ),
        (
            "A if f::<u8, u8>() => work(),\nB => 0,",
            "A if f::<u8, u8>() => work(),",
        ),
        ("lets_go! { a }\nnext();", "lets_go! { a }"),
    ] {
        assert_eq!(&code[..item_end(code, 0)], item, "{code}");
    }
    // An item inside brackets, from the attribute that opens it.
    for (code, item) in [
        ("[#[cfg(x)] a(), b()]", "#[cfg(x)] a(),"),
        ("f(#[cfg(x)] a, b)", "#[cfg(x)] a,"),
        ("S { #[cfg(x)] a: 1, b: 2 }", "#[cfg(x)] a: 1,"),
        ("S { #[cfg(x)] a, b }", "#[cfg(x)] a,"),
        ("{ #[cfg(x)] f::<A, B>(); g(); }", "#[cfg(x)] f::<A, B>();"),
        (
            "{ #[cfg(x)] let f = |a, b| a; g(); }",
            "#[cfg(x)] let f = |a, b| a;",
        ),
    ] {
        let from = code.find('#').unwrap_or_default();
        assert_eq!(&code[from..item_end(code, from)], item, "{code}");
    }
}

#[test]
fn a_call_is_work_unless_it_reports_constructs_converts_or_exits() {
    // `gated`: the text gated code starts with, which runs to the end.
    for (code, gated, work) in [
        ("eprintln!(\"no\");", None, false),
        ("ExitCode::from(2)", None, false),
        ("std::process::exit(3)", None, false),
        ("Outcome::CouldNotRun(\"no\".into())", None, false),
        ("let s = 1.to_string();", None, false),
        ("if (a) { return (1); }", None, false),
        (
            "harness(\"rows\", |row| Outcome::Skip(row))",
            Some("Outcome"),
            false,
        ),
        ("harness(move || Outcome::Skip)", Some("Outcome"), false),
        ("harness(\"rows\", |row| serve(row))", Some("serve"), true),
        ("harness(\"rows\", |row| Outcome::Skip(row))", None, true),
        (
            "eprintln!(\"gated\"); serve(|req| Reply::Ok(req))",
            Some("eprintln"),
            true,
        ),
        ("harness(\"rows\", Outcome::Skip)", Some("Outcome"), true),
        (
            "harness(|row| Outcome::Skip(row), \"rows\")",
            Some("Outcome"),
            false,
        ),
        ("let b = a::<B> > (c);", None, false),
        ("serve()", None, true),
        ("check(a || b)", None, true),
        ("x.collect::<Vec<_>>()", None, true),
        ("let n = parse::<u8>(\"1\");", None, true),
    ] {
        let gates: Vec<Range<usize>> = gated
            .and_then(|text| code.find(text))
            .map(|at| at..code.len())
            .into_iter()
            .collect();
        assert_eq!(works(code, &gates), work, "{code}");
    }
}

#[test]
fn a_predicate_is_false_on_trusts_host_only_when_the_host_fixes_every_name_it_turns_on() {
    for (pred, holds) in [
        ("target_os = \"linux\"", Some(false)),
        ("target_os = \"macos\"", Some(true)),
        ("windows", Some(false)),
        ("unix", Some(true)),
        ("not(windows)", Some(true)),
        ("not(unix)", Some(false)),
        ("all(windows, debug_assertions)", Some(false)),
        ("all(unix, debug_assertions)", None),
        (
            "any(target_os = \"linux\", target_os = \"macos\")",
            Some(true),
        ),
        ("any(target_os = \"linux\", feature = \"x\")", None),
        (
            "all(target_arch = \"wasm32\", target_os = \"unknown\")",
            Some(false),
        ),
        ("target_arch = \"x86_64\"", Some(false)),
        ("target_family = \"wasm\"", Some(false)),
        ("target_vendor = \"apple\"", Some(true)),
        ("all()", Some(true)),
        ("any()", Some(false)),
        ("test", None),
        ("target_os = linux", None),
    ] {
        assert_eq!(on_trust(pred), holds, "{pred}");
    }
}
