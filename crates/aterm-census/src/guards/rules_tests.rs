// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::super::tests::{run, spots};
use super::*;

const HEADER: &str = "// SPDX-License-Identifier: Apache-2.0\n// Copyright 2026 Andrew Yates\n";

#[test]
fn the_engine_names_no_platform_in_shipped_code() {
    let src = "use std::fs::File;\nfn f() { let _ = std::thread::spawn(|| ()); }\n\
               // std::fs in prose\nconst S: &str = \"Metal\";\n#[link(name = \"m\")]\n\
               extern \"C\" {}\n#[cfg(test)]\nmod tests {\n    use std::fs;\n}\n";
    let found = run(
        &ENGINE_PLATFORM,
        &[
            ("crates/aterm-grid/src/lib.rs", src),
            ("crates/aterm-grid/src/grid/tests.rs", src),
            ("crates/aterm-scrollback/src/storage.rs", src),
            ("crates/aterm-effects/src/file_feed.rs", src),
            ("crates/aterm-gui/src/lib.rs", src),
            ("crates/aterm-aos/src/log.rs", src),
            ("crates/aterm-aos/tests/toy.rs", src),
        ],
    );
    assert_eq!(
        spots(&found),
        [
            "crates/aterm-aos/src/log.rs:1",
            "crates/aterm-aos/src/log.rs:2",
            "crates/aterm-aos/src/log.rs:2",
            "crates/aterm-aos/src/log.rs:4",
            "crates/aterm-aos/src/log.rs:5",
            "crates/aterm-grid/src/lib.rs:1",
            "crates/aterm-grid/src/lib.rs:2",
            "crates/aterm-grid/src/lib.rs:2",
            "crates/aterm-grid/src/lib.rs:4",
            "crates/aterm-grid/src/lib.rs:5",
            "crates/aterm-scrollback/src/storage.rs:1",
            "crates/aterm-scrollback/src/storage.rs:2",
            "crates/aterm-scrollback/src/storage.rs:2",
            "crates/aterm-scrollback/src/storage.rs:4",
            "crates/aterm-scrollback/src/storage.rs:5",
        ]
    );
}

/// Detects `thread::Builder`, grouped `std` imports, and path-qualified `std` module items.
#[test]
fn a_thread_builder_and_a_grouped_std_import_are_found() {
    let src = "use std::{fs, io::{self, Read}};\n\
               use std::{\n    collections::HashMap,\n    net::TcpStream as Tcp,\n    thread,\n};\n\
               fn f() { let _ = std::thread::Builder::new(); }\n\
               use std::{io, sync::Arc};\n\
               // use std::{fs};\n\
               #[cfg(test)]\nmod tests {\n    use std::{fs, thread};\n}\n\
               use std::thread::{Builder, spawn};\n\
               fn g() { std::thread::scope(|s| { s.spawn(|| ()); }); }\n\
               use std::net::*;\n\
               std::thread_local! { static N: u8 = 0; }\n";
    let found = run(&ENGINE_PLATFORM, &[("crates/aterm-grid/src/lib.rs", src)]);
    assert_eq!(
        found
            .iter()
            .map(|f| format!("{}:{}", f.line, f.detail))
            .collect::<Vec<_>>(),
        [
            "1:imports `std::fs` in a group (the filesystem)",
            "4:imports `std::net` in a group (a socket)",
            "4:names `TcpStream` (a socket)",
            "5:imports `std::thread` in a group (a thread)",
            "7:names `thread::Builder` (a thread)",
            "7:names `std::thread` (a thread)",
            "14:names `std::thread` (a thread)",
            "15:names `std::thread` (a thread)",
            "16:names `std::net` (a socket)",
        ]
    );
}

#[test]
fn an_engine_framework_named_in_a_literal_or_a_cfg_attr_link_is_found() {
    let src = "#[cfg_attr(target_os = \"macos\", link(name = \"AppKit\", kind = \"framework\"))]\n\
               let h = open_framework(c\"/System/Library/Frameworks/Metal.framework/Metal\");\n\
               #[cfg_attr(\n    unix,\n    link(name = \"m\")\n)]\n\
               fn hyperlink(name: &str) -> Link { link(name) }\nstruct MetalLayer;\n";
    let found = run(&ENGINE_PLATFORM, &[("crates/aterm-core/src/lib.rs", src)]);
    assert_eq!(
        found
            .iter()
            .map(|f| format!("{}:{}", f.line, f.detail))
            .collect::<Vec<_>>(),
        [
            "1:names `AppKit` (a platform framework)",
            "1:links a native library (`link(name = …)`)",
            "2:names `Metal` (a platform framework)",
            "5:links a native library (`link(name = …)`)",
        ]
    );
}

/// A module its parent declares test-only at top level is test code, and so is its
/// directory.
#[test]
fn a_module_the_parent_declares_test_only_is_test_code() {
    let threaded = "fn rig() { std::thread::scope(|s| { s.spawn(|| ()); }); }\n";
    let lib = "pub mod writer;\n#[cfg(test)]\nmod rig;\n\
               #[cfg(all(test, unix))]\n#[path = \"../fixtures/deep_rig.rs\"]\nmod deep;\n\
               #[cfg(test)]\nmod tests {\n    mod inner;\n}\n#[cfg(test)]\nmod gone;\n";
    let found = run(
        &ENGINE_PLATFORM,
        &[
            ("crates/aterm-restore/src/lib.rs", lib),
            ("crates/aterm-restore/src/rig.rs", threaded),
            ("crates/aterm-restore/src/rig/part.rs", threaded),
            ("crates/aterm-restore/fixtures/deep_rig.rs", threaded),
            ("crates/aterm-restore/src/writer.rs", threaded),
            ("crates/aterm-restore/src/inner.rs", threaded),
        ],
    );
    assert_eq!(
        spots(&found),
        [
            "crates/aterm-restore/src/inner.rs:1",
            "crates/aterm-restore/src/writer.rs:1",
        ],
        "{found:?}"
    );
}

#[test]
fn the_renderers_never_name_terminal() {
    let src = "use aterm_core::terminal::Terminal;\nfn draw(t: &Terminal) {}\n\
               fn make() -> Terminal {}\nfn ok(f: &TerminalFrame, s: TermSize) {}\n\
               // aterm_core::Terminal in prose\n";
    let found = run(
        &RENDERER_TERMINAL,
        &[
            ("crates/aterm-render/src/lib.rs", src),
            ("crates/aterm-render/src/raster_tests.rs", src),
        ],
    );
    assert_eq!(
        spots(&found),
        [
            "crates/aterm-render/src/lib.rs:1",
            "crates/aterm-render/src/lib.rs:2",
            "crates/aterm-render/src/lib.rs:3",
        ]
    );
}

#[test]
fn a_device_read_to_eof_is_found_in_code_only() {
    let src = "let k = std::fs::read(\"/dev/urandom\")?;\n\
               let s = fs::read_to_string( \"/dev/stdin\" );\n\
               // fs::read(\"/dev/random\") is the incident\n\
               let ok = fs::read(\"/etc/hosts\");\nlet m = \"fs::read(\\\"/dev/x\\\")\";\n";
    let found = run(&DEV_READ, &[("crates/x/tests/t.rs", src)]);
    assert_eq!(
        spots(&found),
        ["crates/x/tests/t.rs:1", "crates/x/tests/t.rs:2"]
    );
}

#[test]
fn an_input_probe_is_found_in_code_and_strings_not_prose() {
    let src = "// CGEventSourceFlagsState opened a connection\n\
               unsafe extern \"C\" { fn CGEventSourceSecondsSince(s: u32) -> f64; }\n\
               let sym = dlsym(c\"SLSInputModifierKeyState\");\n\
               let why = \"+[NSEvent modifierFlags]\";\n";
    let found = run(&INPUT_PROBE, &[("crates/aterm-gui/src/platform.rs", src)]);
    assert_eq!(
        spots(&found),
        [
            "crates/aterm-gui/src/platform.rs:2",
            "crates/aterm-gui/src/platform.rs:3",
            "crates/aterm-gui/src/platform.rs:4",
        ]
    );
}

#[test]
fn a_protected_folder_literal_is_found_in_gui_cli_and_amacs_host_shipped_code() {
    let src = "let a = home.join(\"Documents\");\nlet b = \"~/Downloads/x\";\n\
               let c = Path::new(\"/Volumes/Ext\");\nlet d = \"Library/CloudStorage\";\n\
               // ~/Desktop in prose\nlet e = DocumentsView::new();\n\
               const DATA: &str = \"/System/Volumes/Data\";\n\
               let f = \"/System/Volumes/Data/Volumes/Ext\";\n\
               #[cfg(test)]\nmod tests {\n    const P: &str = \"~/Documents\";\n}\n";
    let found = run(
        &PROTECTED_FOLDER,
        &[
            ("crates/aterm-gui/src/lib.rs", src),
            ("crates/aterm-gui/src/lib_tests.rs", src),
            ("crates/aterm-containment/src/consent.rs", src),
            ("crates/amacs-files/src/host.rs", src),
            ("crates/amacs/src/store.rs", src),
        ],
    );
    assert_eq!(
        spots(&found),
        [
            "crates/amacs-files/src/host.rs:1",
            "crates/amacs-files/src/host.rs:2",
            "crates/amacs-files/src/host.rs:3",
            "crates/amacs-files/src/host.rs:4",
            "crates/amacs-files/src/host.rs:8",
            "crates/aterm-gui/src/lib.rs:1",
            "crates/aterm-gui/src/lib.rs:2",
            "crates/aterm-gui/src/lib.rs:3",
            "crates/aterm-gui/src/lib.rs:4",
            "crates/aterm-gui/src/lib.rs:8",
        ]
    );
}

/// A module its parent declares test-only (`#[cfg(test)] mod tests;`) is
/// test code whatever its name, and is not read; its shipped sibling is.
#[test]
fn a_protected_folder_literal_in_a_declared_test_module_is_test_code() {
    let parent = "#[cfg(test)]\nmod tests;\nmod shipped;\n";
    let literal = "const P: &str = \"/Volumes/aterm/aterm.app\";\n";
    let found = run(
        &PROTECTED_FOLDER,
        &[
            ("crates/aterm-gui/src/page.rs", parent),
            ("crates/aterm-gui/src/page/tests.rs", literal),
            ("crates/aterm-gui/src/page/shipped.rs", literal),
        ],
    );
    assert_eq!(spots(&found), ["crates/aterm-gui/src/page/shipped.rs:1"]);
}

/// A test module's file that a shipped `#[path]`, `include!` or `mod` also loads is read,
/// and so is what a shipped mount of it loads in turn; what only a test module loads stays
/// test code.
#[test]
fn a_test_module_a_shipped_line_also_loads_is_read() {
    let literal = "const P: &str = \"/Volumes/aterm/aterm.app\";\n";
    // The parent, three files that spell the literal, and `extra` over them.
    let found = |parent: &str, extra: &[(&str, &str)]| {
        let mut files: std::collections::BTreeMap<&str, &str> = [
            ("crates/aterm-gui/src/page/tests.rs", literal),
            ("crates/aterm-gui/src/page/tests/forge.rs", literal),
            ("crates/aterm-gui/src/page_rig_tests.rs", literal),
        ]
        .into_iter()
        .chain(extra.iter().copied())
        .collect();
        files.insert("crates/aterm-gui/src/page.rs", parent);
        let files: Vec<(&str, &str)> = files.into_iter().collect();
        spots(&run(&PROTECTED_FOLDER, &files))
    };
    let tests =
        "#[cfg(test)]\nmod tests;\n#[cfg(test)]\n#[path = \"page_rig_tests.rs\"]\nmod rig;\n";
    assert!(found(tests, &[]).is_empty(), "{:?}", found(tests, &[]));
    // What a test module mounts by `#[path]`, at any depth, is test code too.
    let nested = [
        (
            "crates/aterm-gui/src/page_rig_tests.rs",
            "#[path = \"page_rig_more.rs\"]\nmod more;\n",
        ),
        (
            "crates/aterm-gui/src/page_rig_more.rs",
            "const P: &str = \"/Volumes/aterm/aterm.app\";\n\
             #[path = \"page_rig_deep_tests.rs\"]\nmod deep;\n",
        ),
        ("crates/aterm-gui/src/page_rig_deep_tests.rs", literal),
    ];
    assert!(
        found(tests, &nested).is_empty(),
        "{:?}",
        found(tests, &nested)
    );
    for (mount, read) in [
        (
            "#[path = \"page/tests.rs\"]\nmod shipped;\n",
            "crates/aterm-gui/src/page/tests.rs:1",
        ),
        (
            "#[path = \"page/tests.rs\"] mod shipped;\n",
            "crates/aterm-gui/src/page/tests.rs:1",
        ),
        (
            "#[cfg_attr(unix, path = \"page/tests.rs\")]\nmod shipped;\n",
            "crates/aterm-gui/src/page/tests.rs:1",
        ),
        (
            "mod outer {\n    #[path = \"../tests/forge.rs\"]\n    mod shipped;\n}\n",
            "crates/aterm-gui/src/page/tests/forge.rs:1",
        ),
        (
            "#[path = \"page/tests/forge.rs\"]\nmod forge;\n",
            "crates/aterm-gui/src/page/tests/forge.rs:1",
        ),
        (
            "include!(\"page/tests.rs\");\n",
            "crates/aterm-gui/src/page/tests.rs:1",
        ),
        (
            "#[path = \"page_rig_tests.rs\"]\nmod shipped_rig;\n",
            "crates/aterm-gui/src/page_rig_tests.rs:1",
        ),
    ] {
        assert_eq!(found(&format!("{tests}{mount}"), &[]), [read], "{mount}");
    }
    // A shipped mount of a shipped test file ships what it declares.
    let chain = format!("{tests}#[path = \"page/tests/forge.rs\"]\nmod forge;\n");
    let forge = "mod deep;\n";
    let deep = ("crates/aterm-gui/src/page/tests/forge/deep.rs", literal);
    assert_eq!(
        found(
            &chain,
            &[("crates/aterm-gui/src/page/tests/forge.rs", forge), deep]
        ),
        ["crates/aterm-gui/src/page/tests/forge/deep.rs:1"]
    );
    // The test module's own child stays test code.
    let own = ("crates/aterm-gui/src/page/tests.rs", "mod forge;\n");
    assert!(found(tests, &[own]).is_empty());
}

/// The files of [`test_code_layout`] that ship.
const SHIPPED_IN_LAYOUT: [&str; 5] = [
    "host.rs",
    "test_binary.rs",
    "shipped_tests.rs",
    "both.rs",
    "cross.rs",
];

/// A crate whose `src` folder is `krate`/`src` (its root `lib.rs`, or `mod.rs` deeper) with
/// `body` in every file but the loaders: a plain shipped module, a shipped one whose name
/// opens with `test`, a `_tests.rs` an ungated `#[path]` mounts, a file both a test-only
/// inline module and a shipped one `include!`, and a module the root declares test-only that
/// another crate's shipped `#[path]` mounts; then a module the root declares test-only (which
/// a test target mounts too), a file only a test-only inline module `include!`s, a test-only
/// `mod` inside a shipped inline module, a `_tests.rs` only a test target mounts, one only an
/// example mounts, a `tests.rs` nothing loads, a test target and its `mod common;`, and a
/// test support file only a bench and a test-only mount load.
fn test_code_layout(krate: &str, src: &str, body: &str) -> Vec<(String, String)> {
    let root = if src == "src" { "lib.rs" } else { "mod.rs" };
    let up = "../".repeat(src.split('/').count());
    let loads = format!(
        "pub mod host;\npub mod test_binary;\n#[path = \"shipped_tests.rs\"]\nmod shipped;\n\
         #[cfg(test)]\nmod rig;\n#[cfg(test)]\n#[path = \"{up}tests/support/files.rs\"]\n\
         mod files;\n#[cfg(test)]\nmod cross;\n#[cfg(test)]\nmod checks {{\n    \
         include!(\"spliced.rs\");\n    include!(\"both.rs\");\n}}\nmod again {{\n    \
         include!(\"both.rs\");\n}}\nmod imp {{\n    #[cfg(test)]\n    mod deep;\n}}\n"
    );
    let name = krate.strip_prefix("crates/").unwrap_or(krate);
    let mounter = format!("#[path = \"../../{name}/{src}/cross.rs\"]\nmod cross;\n");
    let suite = format!(
        "#[path = \"../{src}/shared_tests.rs\"]\nmod shared;\n\
         #[path = \"../{src}/rig.rs\"]\nmod rig;\nmod common;\n{body}"
    );
    let mut files = vec![
        (format!("{krate}/{src}/{root}"), loads),
        (format!("{krate}/tests/suite.rs"), suite),
        (
            format!("{krate}/benches/drift.rs"),
            "#[path = \"../tests/support/files.rs\"]\nmod files;\n".to_string(),
        ),
        (
            format!("{krate}/examples/demo.rs"),
            format!("#[path = \"../{src}/demo_tests.rs\"]\nmod demo;\n"),
        ),
        ("crates/zz-mount/src/lib.rs".to_string(), mounter),
    ];
    for file in SHIPPED_IN_LAYOUT.into_iter().chain([
        "rig.rs",
        "spliced.rs",
        "imp/deep.rs",
        "shared_tests.rs",
        "demo_tests.rs",
        "old/tests.rs",
    ]) {
        files.push((format!("{krate}/{src}/{file}"), body.to_string()));
    }
    for file in ["tests/common/mod.rs", "tests/support/files.rs"] {
        files.push((format!("{krate}/{file}"), body.to_string()));
    }
    files
}

/// Each of these guards skips test code by [`TestCode::holds`] alone, so each reads the same
/// files of [`test_code_layout`], the ones that ship ([`SHIPPED_IN_LAYOUT`]), and nothing
/// else, though a test target, an example and a bench load test code, a test-only inline
/// module splices a file in, and the names of `rig.rs` and `imp/deep.rs` say nothing of
/// tests. Each row is a guard, a crate in its scope with the `src` folder its scope reads, a
/// line it fails, and the files the guard needs beside them to read anything.
#[test]
fn every_guard_skips_the_same_test_code() {
    let enums = [
        (
            "crates/aterm-phase/src/reader.rs",
            "pub enum Program {\n    Claude,\n    Codex,\n    Generic,\n}\n",
        ),
        (
            "crates/aterm-agent/src/harness/upgrade.rs",
            "pub enum Agent {\n    #[default]\n    Claude,\n    Codex,\n}\n",
        ),
    ];
    type Row<'a> = (
        &'a Rule,
        &'a str,
        &'a str,
        &'a str,
        &'a [(&'a str, &'a str)],
    );
    let rows: &[Row<'_>] = &[
        (
            &PROTECTED_FOLDER,
            "crates/amacs-files",
            "src",
            "const P: &str = \"/Volumes/aterm/aterm.app\";\n",
            &[],
        ),
        (
            &super::super::debug_seam::DEBUG_SEAM,
            "crates/aterm-lab",
            "src",
            "fn f() {\n    let _ = std::env::var_os(\"ATERM_DEBUG_NETPLACE\");\n}\n",
            &[],
        ),
        (
            &super::super::doors::DEVTOOLS_DOOR,
            "crates/aterm-gui",
            "src",
            "impl App {\n    fn f(&mut self) {\n        self.interact_system_state(x);\n    \
             }\n}\n",
            &[],
        ),
        (
            &super::super::doors::DEVTOOLS_DOOR,
            "crates/aterm-cli",
            "src",
            "fn a() { run_devtools_change(c, g, f); }\n",
            &[],
        ),
        (
            &STORE_CURRENT,
            "crates/aterm-verify",
            "src",
            "fn a(p: &Path) -> PathBuf {\n    p.join(\"current\")\n}\n",
            &[],
        ),
        (
            &RUNLOOP_TIMER,
            "crates/aterm-window",
            "src",
            "fn f() {\n    let t = CFRunLoopTimerCreate(a, b);\n}\n",
            &[],
        ),
        (
            &super::super::stock_label::STOCK_LABEL,
            "crates/aterm-c",
            "src",
            "fn f(cmd: &mut Cmd) {\n    cmd.env(\"RUSTUP_TOOLCHAIN\", \"stable\");\n}\n",
            &[],
        ),
        (
            &RENDERER_TERMINAL,
            "crates/aterm-render",
            "src",
            "use aterm_core::terminal::Terminal;\n",
            &[],
        ),
        (
            &KEEPER_NO_MMAP,
            "crates/aterm-keeper",
            "src",
            "fn f() {\n    unsafe { libc::mmap(p, n, 1, 1, fd, 0) };\n}\n",
            &[],
        ),
        (
            &RESTORE_STAYS_LOCAL,
            "crates/aterm-restore",
            "src",
            "use std::net::TcpStream;\n",
            &[],
        ),
        (
            &SUPERVISE_FX,
            "crates/aterm-agent",
            "src/supervise",
            "fn f() {\n    let t = Instant::now();\n}\n",
            &[],
        ),
        (
            &super::super::grants::PROMPT_GRANT,
            "crates/aterm-gui",
            "src",
            "fn mint() {\n    let _ = Occupancy::Prompt.grant();\n}\n",
            &[],
        ),
        (
            &super::super::machines::MACHINES_ROOT_DOOR,
            "crates/aterm-cli",
            "src",
            "fn a() {\n    run(\"/usr/sbin/installer\", &[\"-pkg\", p]);\n}\n",
            &[],
        ),
        (
            &super::super::doors::FFI_DOOR,
            "crates/aterm-cli",
            "src",
            "extern \"system\" {\n    fn WinExec();\n}\n",
            &[],
        ),
        (
            &super::super::doors::HANDOFF_NAME,
            "crates/aterm-gui",
            "src",
            "fn s() {\n    let k = \"ATERM_HANDOFF_CLAIM\".to_string();\n}\n",
            &[],
        ),
        (
            &super::super::aos_step::AOS_STEP_CLOCK,
            "crates/aterm-aos",
            "src",
            "fn a() {\n    let t = Instant::now();\n}\n",
            &[],
        ),
        (
            &super::super::term_write::TERM_WRITE,
            "crates/aterm-gui",
            "src",
            "fn f(term: &Mutex<Terminal>) {\n    term.lock().unwrap().process(b\"x\");\n}\n",
            &[],
        ),
        (
            &super::super::agent_gate::AGENT_GATE,
            "crates/aterm-gui-x",
            "src",
            "fn f(a: Program) -> bool {\n    a == Program::Claude\n}\n",
            &enums,
        ),
        (
            &super::super::slog_inert::SLOG_INERT,
            "crates/aterm-slog",
            "src",
            "fn f(w: SinkWriter) {}\n",
            &[],
        ),
        (
            &ENGINE_PLATFORM,
            "crates/aterm-core",
            "src",
            "use std::fs::File;\n",
            &[],
        ),
        (
            &super::super::mint::LAUNCHER_MINT,
            "crates/aterm-pty",
            "src",
            "fn f() {\n    let a = Authority::root_authority();\n}\n",
            &[],
        ),
        (
            &super::super::github::GITHUB_AUTH_DOOR,
            "crates/agh",
            "src",
            "const H: &str = \"Authorization\";\n",
            &[],
        ),
        (
            &super::super::github::GH_SPAWN,
            "crates/aterm-pty",
            "src",
            "fn f() {\n    let c = Command::new(\"gh\");\n}\n",
            &[],
        ),
        (
            &super::super::github::AGH_FX,
            "crates/agh",
            "src",
            "fn f() {\n    let t = Instant::now();\n}\n",
            &[],
        ),
    ];
    let mut wrong = Vec::new();
    for (rule, krate, src, body, beside) in rows {
        let mut files = test_code_layout(krate, src, body);
        files.extend(
            beside
                .iter()
                .map(|(p, b)| ((*p).to_string(), (*b).to_string())),
        );
        let files: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, b)| (p.as_str(), b.as_str()))
            .collect();
        let read: std::collections::BTreeSet<String> = run(rule, &files)
            .into_iter()
            .map(|f| f.path)
            .filter(|p| p.starts_with(&format!("{krate}/")))
            .collect();
        let want: std::collections::BTreeSet<String> = SHIPPED_IN_LAYOUT
            .iter()
            .map(|f| format!("{krate}/{src}/{f}"))
            .collect();
        if read != want {
            wrong.push(format!("{} over {krate}: read {read:?}", rule.id));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// engine-platform reads test code as every guard does: a module its parent declares
/// test-only stays test code though a test target, a bench or an example mounts it too, and
/// is read once a shipped line mounts it.
#[test]
fn an_engine_module_a_test_target_also_mounts_stays_test_code() {
    let body = "use std::fs::File;\n";
    let read = |root: &str, mounter: Option<&str>| {
        let mut files = vec![
            ("crates/aterm-aos/src/lib.rs", root),
            ("crates/aterm-aos/src/host.rs", body),
            ("crates/aterm-aos/src/rig.rs", body),
        ];
        let mount = "#[path = \"../src/rig.rs\"]\nmod rig;\n";
        files.extend(mounter.map(|rel| (rel, mount)));
        let found: std::collections::BTreeSet<String> = run(&ENGINE_PLATFORM, &files)
            .into_iter()
            .map(|f| f.path)
            .collect();
        found.into_iter().collect::<Vec<_>>()
    };
    let declared = "pub mod host;\n#[cfg(test)]\nmod rig;\n";
    for mounter in [
        None,
        Some("crates/aterm-aos/tests/suite.rs"),
        Some("crates/aterm-aos/benches/replay.rs"),
        Some("crates/aterm-aos/examples/demo.rs"),
    ] {
        assert_eq!(
            read(declared, mounter),
            ["crates/aterm-aos/src/host.rs"],
            "{mounter:?}"
        );
    }
    let shipped = "pub mod host;\n#[cfg(test)]\nmod rig;\n#[path = \"rig.rs\"]\nmod again;\n";
    assert_eq!(
        read(shipped, None),
        [
            "crates/aterm-aos/src/host.rs",
            "crates/aterm-aos/src/rig.rs"
        ]
    );
}

#[test]
fn a_runloop_timer_lives_only_in_wake_timer() {
    let src = "let t = CFRunLoopTimerCreate(a, b);\n\
               let n = send(cls, sel!(scheduledTimerWithTimeInterval:target:selector:userInfo:repeats:));\n";
    let found = run(
        &RUNLOOP_TIMER,
        &[
            ("crates/aterm-objc/src/wake_timer.rs", src),
            ("crates/aterm-gui/src/app.rs", src),
            ("crates/aterm-gui/tests/t.rs", src),
            ("crates/aterm-window/src/platform_impl/macos/app.rs", src),
        ],
    );
    assert_eq!(
        spots(&found),
        [
            "crates/aterm-gui/src/app.rs:1",
            "crates/aterm-gui/src/app.rs:2",
            "crates/aterm-window/src/platform_impl/macos/app.rs:1",
            "crates/aterm-window/src/platform_impl/macos/app.rs:2",
        ]
    );
}

#[test]
fn the_keeper_links_no_gui_framework() {
    let src = "#[link(name = \"AppKit\", kind = \"framework\")]\nextern \"C\" {}\n\
               #[link(name = \"Security\", kind = \"framework\")]\n\
               // objc_msgSend in prose\nlet x = winit::event_loop::EventLoop::new();\n";
    let found = run(
        &KEEPER_GUI,
        &[
            ("crates/aterm-keeper/src/lib.rs", src),
            ("crates/aterm-gui/src/lib.rs", src),
        ],
    );
    assert_eq!(
        spots(&found),
        [
            "crates/aterm-keeper/src/lib.rs:1",
            "crates/aterm-keeper/src/lib.rs:5",
        ]
    );
}

#[test]
fn the_keeper_maps_no_region() {
    let src = "fn hold(fd: OwnedFd) { let len = fd_size(fd.as_fd()); }\n\
               let r = aterm_uds::shm::Region::map_readonly(fd.as_fd(), len);\n\
               // a comment may say mmap\n\
               unsafe { libc::mmap(p, n, 1, 1, fd, 0) };\n\
               let s = \"mmap in a literal\";\n\
               unsafe extern \"C\" { fn munmap(addr: *mut u8, len: usize) -> i32; }\n\
               #[cfg(test)]\nfn t() { let _ = mmap(16384); }\n";
    let found = run(
        &KEEPER_NO_MMAP,
        &[
            ("crates/aterm-keeper/src/server.rs", src),
            ("crates/aterm-keeper/src/core_companion_tests.rs", src),
            ("crates/aterm-keeper/tests/kernel_custody.rs", src),
            ("crates/aterm-uds/src/shm.rs", src),
        ],
    );
    assert_eq!(
        spots(&found),
        [
            "crates/aterm-keeper/src/server.rs:4",
            "crates/aterm-keeper/src/server.rs:6",
        ]
    );
}

#[test]
fn the_crash_restore_names_no_socket() {
    let src = "use std::net::TcpStream;\n\
               // a comment may say socket\n\
               let s = \"a UnixStream in a literal\";\n\
               fn f(s: aterm_uds::CtlStream) {}\n\
               let n = sockets_seen + 1;\n\
               #[cfg(test)]\nfn t() { let _ = std::os::unix::net::UnixStream::pair(); }\n";
    let found = run(
        &RESTORE_STAYS_LOCAL,
        &[
            ("crates/aterm-restore/src/recover.rs", src),
            ("crates/aterm-restore/src/recover_tests.rs", src),
            ("crates/aterm-restore/tests/engine_conformance.rs", src),
            ("crates/aterm-gui/src/crash_restore.rs", src),
        ],
    );
    assert_eq!(
        spots(&found),
        [
            "crates/aterm-restore/src/recover.rs:1",
            "crates/aterm-restore/src/recover.rs:1",
            "crates/aterm-restore/src/recover.rs:4",
        ]
    );
}

#[test]
fn no_off_host_crate_has_the_crash_restore_in_its_closure() {
    use crate::scan_set::test_fixtures::{disk_tree, workspace_manifests};
    let closure_of = |off_host: (&str, &str), session_deps: &str, label: &str| {
        let mut files = workspace_manifests(&[
            off_host,
            ("aterm-session", session_deps),
            ("aterm-uds", ""),
            (
                "aterm-restore",
                "[dependencies]\naterm-uds = { workspace = true }\n",
            ),
        ]);
        for dir in [
            "aterm-gui",
            "aterm-types",
            off_host.0,
            "aterm-session",
            "aterm-uds",
            "aterm-restore",
        ] {
            files.push((format!("crates/{dir}/src/lib.rs"), String::new()));
        }
        files.push(("Cargo.lock".to_string(), "version = 4\n".to_string()));
        let root = disk_tree("aterm-census-guards", label, &files);
        let corpus = super::super::Corpus::of(&[]);
        let tree = Tree::new(&root, &corpus, crate::guards::tests::NO_ENGINE);
        let mut found = Vec::new();
        restore_stays_local(&tree, &mut found);
        let _ = std::fs::remove_dir_all(&root);
        found
    };
    let closure = |link_deps: &str, session_deps: &str, label: &str| {
        closure_of(("aterm-link", link_deps), session_deps, label)
    };
    for off_host in ["aterm-net", "aterm-http", "aterm-gui-fabric"] {
        let found = closure_of(
            (
                off_host,
                "[dependencies]\naterm-restore = { workspace = true }\n",
            ),
            "",
            &format!("restore-local-{off_host}"),
        );
        assert_eq!(found.len(), 1, "{off_host}: {found:?}");
        assert_eq!(found[0].path, format!("crates/{off_host}/Cargo.toml"));
        let found = closure_of(
            (
                off_host,
                "[dependencies]\naterm-uds = { workspace = true }\n",
            ),
            "",
            &format!("restore-local-{off_host}-clean"),
        );
        assert!(found.is_empty(), "{off_host}: {found:?}");
    }
    let link_via_session =
        "[dependencies]\naterm-uds = { workspace = true }\naterm-session = { workspace = true }\n";
    let found = closure(link_via_session, "", "restore-local-clean");
    assert!(found.is_empty(), "{found:?}");
    let found = closure(
        "[dependencies]\naterm-restore = { workspace = true }\n",
        "",
        "restore-local-link",
    );
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].path, "crates/aterm-link/Cargo.toml");
    assert!(found[0].detail.contains("aterm-restore is in"), "{found:?}");
    let found = closure(
        link_via_session,
        "[target.'cfg(unix)'.dependencies]\naterm-restore = { workspace = true }\n",
        "restore-local-session",
    );
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].path, "crates/aterm-link/Cargo.toml");
    assert!(
        found[0]
            .detail
            .contains("through aterm-session -> aterm-restore"),
        "{found:?}"
    );
}

#[test]
fn the_keepers_closure_holds_no_gui_crate() {
    use crate::scan_set::test_fixtures::{disk_tree, workspace_manifests};
    let mut files = workspace_manifests(&[
        (
            "aterm-keeper",
            "[dependencies]\naterm-uds = { workspace = true }\n",
        ),
        ("aterm-uds", "[dependencies]\nobjc-sys = \"0.3\"\n"),
    ]);
    for dir in ["aterm-gui", "aterm-types", "aterm-keeper", "aterm-uds"] {
        files.push((format!("crates/{dir}/src/lib.rs"), String::new()));
    }
    files.push(("Cargo.lock".to_string(), "version = 4\n".to_string()));
    let root = disk_tree("aterm-census-guards", "keeper-deps", &files);
    let corpus = super::super::Corpus::of(&[]);
    let tree = Tree::new(&root, &corpus, crate::guards::tests::NO_ENGINE);
    let mut found = Vec::new();
    keeper_gui_deps(&tree, &mut found);
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].path, "crates/aterm-uds/Cargo.toml");
    assert!(found[0].detail.contains("`objc-sys`"), "{found:?}");

    let mut files = workspace_manifests(&[
        (
            "aterm-keeper",
            "[dependencies]\naterm-uds = { workspace = true }\n",
        ),
        (
            "aterm-uds",
            "[dependencies]\naterm-render = { path = \"../aterm-render\" }\n",
        ),
        ("aterm-render", ""),
    ]);
    for dir in [
        "aterm-gui",
        "aterm-types",
        "aterm-keeper",
        "aterm-uds",
        "aterm-render",
    ] {
        files.push((format!("crates/{dir}/src/lib.rs"), String::new()));
    }
    files.push(("Cargo.lock".to_string(), "version = 4\n".to_string()));
    let root = disk_tree("aterm-census-guards", "keeper-gui-crate", &files);
    std::fs::write(
        root.join("crates/aterm-render/src/lib.rs"),
        "#[link(name = \"AppKit\", kind = \"framework\")]\nunsafe extern \"C\" {}\n",
    )
    .unwrap();
    let corpus = super::super::Corpus::at(&root).unwrap();
    let tree = Tree::new(&root, &corpus, crate::guards::tests::NO_ENGINE);
    let mut found = Vec::new();
    keeper_gui_deps(&tree, &mut found);
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].path, "crates/aterm-render/src/lib.rs");
    assert!(found[0].detail.contains("AppKit"), "{found:?}");

    let mut files = workspace_manifests(&[
        (
            "aterm-keeper",
            "[dependencies]\naterm-uds = { workspace = true }\n",
        ),
        (
            "aterm-uds",
            "[dependencies]\nwindowing = { package = \"winit\", version = \"0.30\" }\n",
        ),
    ]);
    for dir in ["aterm-gui", "aterm-types", "aterm-keeper", "aterm-uds"] {
        files.push((format!("crates/{dir}/src/lib.rs"), String::new()));
    }
    files.push(("Cargo.lock".to_string(), "version = 4\n".to_string()));
    let root = disk_tree("aterm-census-guards", "keeper-renamed-gui", &files);
    let tree = Tree::new(&root, &corpus, crate::guards::tests::NO_ENGINE);
    let mut found = Vec::new();
    keeper_gui_deps(&tree, &mut found);
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].path, "crates/aterm-uds/Cargo.toml");
    assert!(found[0].detail.contains("`winit`"), "{found:?}");
}

#[test]
fn keeper_framework_check_follows_source_instead_of_crate_names() {
    use crate::scan_set::test_fixtures::{disk_tree, workspace_manifests};
    for helper in ["aterm-objc", "native-helper"] {
        for framework in ["Foundation", "AppKit"] {
            let deps = format!(
                "[dependencies]\nhelper = {{ package = \"{helper}\", path = \"../{helper}\" }}\n"
            );
            let mut files = workspace_manifests(&[
                (
                    "aterm-keeper",
                    "[dependencies]\naterm-uds = { workspace = true }\n",
                ),
                ("aterm-uds", &deps),
                (helper, ""),
            ]);
            for name in [
                "aterm-gui",
                "aterm-types",
                "aterm-keeper",
                "aterm-uds",
                helper,
            ] {
                files.push((format!("crates/{name}/src/lib.rs"), String::new()));
            }
            let source = format!(
                "#[link(name = \"{framework}\", kind = \"framework\")]\nunsafe extern \"C\" {{}}\n"
            );
            files.push(("Cargo.lock".to_owned(), "version = 4\n".to_owned()));
            let root = disk_tree("aterm-census-guards", "keeper-framework-source", &files);
            let path = format!("crates/{helper}/src/lib.rs");
            std::fs::write(root.join(&path), source).unwrap();
            let corpus = super::super::Corpus::at(&root).unwrap();
            let tree = Tree::new(&root, &corpus, crate::guards::tests::NO_ENGINE);
            let mut found = Vec::new();
            keeper_gui_deps(&tree, &mut found);
            let _ = std::fs::remove_dir_all(&root);
            assert_eq!(
                found.len(),
                usize::from(framework == "AppKit"),
                "{helper} {framework}: {found:?}"
            );
            if let Some(finding) = found.first() {
                assert_eq!(finding.path, path);
            }
        }
    }
}

/// The keeper's dependency closure through `Cargo.lock`: a transitive GUI package or a
/// missing lock entry is a finding.
#[test]
fn the_keepers_closure_is_walked_through_cargo_lock() {
    use crate::scan_set::test_fixtures::{disk_tree, workspace_manifests};
    let lock = "version = 4\n\n\
                [[package]]\nname = \"aterm-uds\"\nversion = \"0.1.0\"\n\
                dependencies = [\n \"helper\",\n]\n\n\
                [[package]]\nname = \"helper\"\nversion = \"1.0.0\"\n\
                source = \"registry+https://github.com/rust-lang/crates.io-index\"\n\
                dependencies = [\n \"log\",\n \"middle\",\n]\n\n\
                [[package]]\nname = \"middle\"\nversion = \"0.2.0\"\n\
                source = \"registry+https://github.com/rust-lang/crates.io-index\"\n\
                dependencies = [\n \"objc2-foundation 0.3.1\",\n]\n\n\
                [[package]]\nname = \"objc2-foundation\"\nversion = \"0.3.1\"\n\
                source = \"registry+https://github.com/rust-lang/crates.io-index\"\n\n\
                [[package]]\nname = \"objc2-foundation\"\nversion = \"0.2.2\"\n\
                source = \"registry+https://github.com/rust-lang/crates.io-index\"\n\n\
                [[package]]\nname = \"log\"\nversion = \"0.4.0\"\n\
                dependencies = [\n \"winit\",\n]\n\n\
                [[package]]\nname = \"winit\"\nversion = \"0.30.0\"\n\
                source = \"registry+https://github.com/rust-lang/crates.io-index\"\n";
    let tree_with = |name: &str, uds_deps: &str| {
        let mut files = workspace_manifests(&[
            (
                "aterm-keeper",
                "[dependencies]\naterm-uds = { workspace = true }\n",
            ),
            ("aterm-uds", uds_deps),
            ("aterm-log", "[dev-dependencies]\nwinit = \"0.30\"\n"),
        ]);
        for (path, contents) in files.iter_mut() {
            if path == "Cargo.toml" {
                contents.push_str("log = { path = \"crates/aterm-log\" }\n");
            }
        }
        for dir in [
            "aterm-gui",
            "aterm-types",
            "aterm-keeper",
            "aterm-uds",
            "aterm-log",
        ] {
            files.push((format!("crates/{dir}/src/lib.rs"), String::new()));
        }
        files.push(("Cargo.lock".to_string(), lock.to_string()));
        disk_tree("aterm-census-guards", name, &files)
    };
    let run_on = |root: &std::path::Path| {
        let corpus = super::super::Corpus::of(&[]);
        let tree = Tree::new(root, &corpus, crate::guards::tests::NO_ENGINE);
        let mut found = Vec::new();
        keeper_gui_deps(&tree, &mut found);
        let _ = std::fs::remove_dir_all(root);
        found
    };
    let found = run_on(&tree_with(
        "keeper-lock",
        "[dependencies]\nhelper = \"1\"\n",
    ));
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].path, "crates/aterm-uds/Cargo.toml");
    assert!(
        found[0]
            .detail
            .contains("reaches `objc2-foundation` through helper -> middle -> objc2-foundation"),
        "{found:?}"
    );
    let found = run_on(&tree_with(
        "keeper-lock-stale",
        "[dependencies]\nnot-locked = \"1\"\n",
    ));
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0]
            .detail
            .contains("`not-locked`, which Cargo.lock does not hold"),
        "{found:?}"
    );
    let hash = "1234567890123456789012345678901234567890";
    let root = tree_with(
        "keeper-git-closure",
        &format!(
            "[dependencies]\nhelper = {{ git = \"https://github.com/example/helper.git\", rev = \"{hash}\" }}\n"
        ),
    );
    let path = root.join("Cargo.lock");
    let locked = std::fs::read_to_string(&path).unwrap().replacen(
        "source = \"registry+https://github.com/rust-lang/crates.io-index\"",
        &format!("source = \"git+https://github.com/example/helper.git?rev={hash}#{hash}\""),
        1,
    );
    std::fs::write(path, locked).unwrap();
    let found = run_on(&root);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0]
            .detail
            .contains("reaches `objc2-foundation` through helper -> middle -> objc2-foundation"),
        "{found:?}"
    );
}

#[test]
fn the_std_command_alias_is_built_renamed_or_re_aliased_only_in_aterm_uds() {
    let src = "use aterm_uds::spawnfd::StdCommand;\n\
               fn a(c: &mut StdCommand) -> &mut StdCommand { c }\n\
               let c = StdCommand::new(\"ls\");\n\
               let d = aterm_uds::spawnfd::StdCommand :: new (\"ls\");\n\
               use aterm_uds::spawnfd::StdCommand as Cmd;\n\
               type Cmd2 = StdCommand;\n\
               type Cmd3 = ::aterm_uds :: spawnfd::StdCommand;\n\
               // StdCommand::new in prose\n\
               let s = \"StdCommand::new\";\n\
               let e = StdCommand::new_with(x);\n\
               #[cfg(test)]\nmod tests {\n    fn t() { let _ = StdCommand::new(\"x\"); }\n}\n";
    let found = run(
        &STD_COMMAND,
        &[
            ("crates/aterm-gui/src/x.rs", src),
            ("crates/aterm-uds/src/spawnfd.rs", src),
        ],
    );
    assert_eq!(
        spots(&found),
        [
            "crates/aterm-gui/src/x.rs:3",
            "crates/aterm-gui/src/x.rs:4",
            "crates/aterm-gui/src/x.rs:5",
            "crates/aterm-gui/src/x.rs:6",
            "crates/aterm-gui/src/x.rs:7",
            "crates/aterm-gui/src/x.rs:13",
        ],
        "{found:?}"
    );
}

/// Each door the lint cannot name, with the line it fires on.
#[test]
fn every_flock_door_the_lint_cannot_name_is_found() {
    let cases: &[(&str, &str, usize)] = &[
        (
            "a private extern flock",
            "unsafe extern \"C\" {\n    fn flock(fd: i32, op: i32) -> i32;\n}\n",
            2,
        ),
        (
            "flock under another name",
            "unsafe extern \"C\" {\n    #[link_name = \"flock\"]\n    fn take_it(fd: i32) -> i32;\n}\n",
            2,
        ),
        (
            "an open that takes the lock",
            "let fd = libc::open(p, libc::O_RDWR | libc::O_EXLOCK, 0o600);\n",
            1,
        ),
        (
            "an OFD lock",
            "unsafe { libc::fcntl(fd, libc::F_OFD_SETLKW, &lock) };\n",
            1,
        ),
        (
            "a raw SYS_flock",
            "libc::syscall(\n    libc::SYS_flock,\n    fd,\n)\n",
            2,
        ),
        (
            "a syscall by number",
            "unsafe { libc::syscall(\n    131, fd, 6) };\n",
            1,
        ),
    ];
    for (why, src, line) in cases {
        let found = run(&FLOCK_DOOR, &[("crates/aterm-gui/src/x.rs", src)]);
        assert_eq!(
            spots(&found),
            [format!("crates/aterm-gui/src/x.rs:{line}")],
            "MISS — {why}:\n{src}"
        );
    }
}

#[test]
fn doors_the_lint_sees_and_near_misses_are_silent() {
    let src = "// A private `fn flock(fd, op)` would be a finding.\n\
               let l = unsafe { libc::flock(fd, libc::LOCK_EX) };\nlet _ = file.try_lock(); // O_EXLOCKED\n\
               fn flock_probe() {}\nlet q = libc::fcntl(fd, libc::F_OFD_GETLK);\nlet _ = my_syscall(1);\n\
               #[link_name = \"flockfile\"]\nlet c = libc::syscall(libc::SYS_close_range, 3);\n\
               let s = \"fn flock(\";\n";
    assert!(run(&FLOCK_DOOR, &[("crates/aterm-gui/src/x.rs", src)]).is_empty());
    let libc = "pub const O_EXLOCK: c_int = 0x20;\npub const SYS_flock: c_int = 131;\n\
                unsafe extern \"C\" {\n    pub fn flock(fd: c_int, op: c_int) -> c_int;\n}\n";
    assert!(
        run(
            &FLOCK_DOOR,
            &[("crates/aterm-libc/src/darwin_aarch64.rs", libc)]
        )
        .is_empty()
    );
}

/// Each way the supervisor's code can read a clock or sleep past its door, with the line it
/// fires on.
#[test]
fn every_clock_read_past_the_supervisors_door_is_found() {
    let cases: &[(&str, &str, usize)] = &[
        ("a monotonic read", "let now = Instant::now();\n", 1),
        (
            "a qualified one",
            "fn f() {\n    let t = std::time::Instant::now();\n}\n",
            2,
        ),
        (
            "the monotonic clock passed as a fn item",
            "let at = *since.get_or_insert_with(Instant::now);\n",
            1,
        ),
        ("an elapsed", "if at.elapsed() < step {\n}\n", 1),
        (
            "an elapsed on its own chained line",
            "let ms = since\n    .elapsed()\n    .as_millis();\n",
            2,
        ),
        (
            "an elapsed by path",
            "let d = marks.map(Instant::elapsed);\n",
            1,
        ),
        ("a wall read", "let w = std::time::SystemTime::now();\n", 1),
        ("limit's wall reader", "let s = limit::unix_now();\n", 1),
        (
            "limit's wall reader passed as a fn item",
            "self.clock.map_or_else(limit::unix_now_ms, |s| s * 1000)\n",
            1,
        ),
        (
            "the journal's wall reader",
            "let age = unix_ms().saturating_sub(taken);\n",
            1,
        ),
        (
            "aterm_time's wall reader",
            "let secs = aterm_time::unix_secs().unwrap_or(0);\n",
            1,
        ),
        ("a sleep", "std::thread::sleep(BACKOFF);\n", 1),
        ("a sleep imported", "use std::thread::sleep;\n", 1),
        (
            "a sleep after an own-name import",
            "fn f() {\n    use std::thread; thread::sleep(D);\n}\n",
            2,
        ),
        (
            "a monotonic read after a multiline import closes",
            "fn f() {\n    use std::{\n        thread,\n    }; let _ = Instant::now();\n}\n",
            4,
        ),
        (
            "a renamed sleep in the second import on one line",
            "use std::fmt; use std::thread::sleep as nap;\n",
            1,
        ),
        (
            "a process read after an own-name import",
            "fn f() {\n    use std::process; let _ = process::id();\n}\n",
            2,
        ),
        (
            "a sleep until an instant",
            "std::thread::sleep_until(at);\n",
            1,
        ),
        ("a park with a timeout", "thread::park_timeout(d);\n", 1),
        (
            "a sleep imported in a group with its module, then called bare",
            "use std::thread::{self, sleep};\nfn f() {\n    sleep(BUSY_SINK_BACKOFF);\n}\n",
            1,
        ),
        (
            "a sleep imported alone in braces",
            "use std::thread::{sleep};\n",
            1,
        ),
        (
            "a sleep renamed in a nested group across lines",
            "use std::{\n    thread::{sleep as nap, spawn},\n    time::Duration,\n};\n",
            1,
        ),
        (
            "the thread module renamed",
            "use std::thread as t;\nfn f() {\n    t::sleep(D);\n}\n",
            1,
        ),
        ("the thread module globbed in", "use std::thread::*;\n", 1),
        (
            "the monotonic clock renamed",
            "use std::time::Instant as Mono;\nfn f() {\n    let t = Mono::now();\n}\n",
            1,
        ),
        (
            "the monotonic clock renamed in a group",
            "use std::time::{Duration, Instant as Mono};\n",
            1,
        ),
        (
            "the wall clock's type renamed",
            "use std::time::SystemTime as Wall;\n",
            1,
        ),
        (
            "the monotonic clock aliased as a type",
            "pub(crate) type Mono = std::time::Instant;\n",
            1,
        ),
        (
            "the journal's wall reader imported in a group, passed bare",
            "use super::journal::{json_str, unix_ms};\nfn f() {\n    \
             let t = x.unwrap_or_else(unix_ms);\n}\n",
            1,
        ),
        (
            "aterm_time's wall reader renamed",
            "use aterm_time::{unix_millis as now_ms};\n",
            1,
        ),
        (
            "this process's id",
            "let holder = format!(\"x:{}\", std::process::id());\n",
            1,
        ),
        (
            "a delivery taken",
            "match m.rx.try_recv() {\n    _ => {}\n}\n",
            1,
        ),
        (
            "a delivery awaited",
            "let got = m.rx.recv_timeout(slice);\n",
            1,
        ),
        (
            "the ledger read by its path",
            "let typed = approvals::typed_at(path, sid);\n",
            1,
        ),
        (
            "another reader of the ledger by its path",
            "open_wall = approvals::open_wall(path, sid);\n",
            1,
        ),
        ("the ledger cut", "approvals::bound(path);\n", 1),
        (
            "this process's id imported bare",
            "use std::process::{self, id};\n",
            1,
        ),
        (
            "a reader of the ledger by its path imported bare",
            "use super::approvals::{Ledger, typed_at};\n",
            1,
        ),
        (
            "the ledger's module renamed",
            "use super::approvals as ledger;\n",
            1,
        ),
        (
            "an inline sleep import inside a function body on one line",
            "fn f() { use std::thread::{self, sleep}; sleep(D); }\n",
            1,
        ),
        (
            "an inline type alias inside a function body on one line",
            "fn f() { type Mono = std::time::Instant; let _ = Mono::now(); }\n",
            1,
        ),
    ];
    for (why, src, line) in cases {
        let found = run(
            &SUPERVISE_FX,
            &[("crates/aterm-agent/src/supervise/run.rs", src)],
        );
        assert_eq!(
            spots(&found),
            [format!("crates/aterm-agent/src/supervise/run.rs:{line}")],
            "MISS — {why}:\n{src}"
        );
    }
}

/// The door's own reads, same-named fields, prose, test code, home files and code outside
/// the supervisor are silent.
#[test]
fn the_door_and_the_clocks_homes_are_silent() {
    let src = "let now = self.fx.mono();\nlet d = self.fx.since(at);\nself.fx.sleep(pause);\n\
               let w = fx.wall_ms() + self.fx.stamp_ms();\n\
               // Instant::now() and t.elapsed() in prose\n\
               let s = \"thread::sleep(1)\";\nlet unix_ms = d.unix_ms;\nsettle.unix_ms = unix_ms;\n\
               fn wall_reset_at(unix_ms: i64, elapsed: Duration) {}\nlet x = r.elapsed;\n\
               #[cfg(test)]\nmod tests {\n    fn t() { let _ = Instant::now(); std::thread::sleep(D); }\n}\n";
    let limit = "pub fn unix_now() -> i64 {\n    \
                 aterm_time::unix_secs().map_or(0, |s| s as i64)\n}\n\
                 pub fn unix_now_ms() -> i64 {\n    \
                 aterm_time::unix_millis().map_or(0, |m| m as i64)\n}\n";
    let journal = "pub fn unix_ms() -> i64 {\n    \
                   aterm_time::unix_millis().map_or(0, |m| m as i64)\n}\n";
    let found = run(
        &SUPERVISE_FX,
        &[
            ("crates/aterm-agent/src/supervise/run.rs", src),
            ("crates/aterm-agent/src/supervise/limit.rs", limit),
            ("crates/aterm-agent/src/supervise/journal.rs", journal),
        ],
    );
    assert!(found.is_empty(), "{:?}", spots(&found));
    let doors = "match self.fx.try_recv_mail(&m.rx) {\n    _ => {}\n}\n\
                 let got = self.fx.recv_mail_timeout(&m.rx, slice);\n\
                 let typed = approvals::typed_at_in(&body, sid);\n\
                 let w = approvals::open_wall_in(&body, sid);\nself.fx.bound(path);\n";
    let holder = "pub(super) fn default_holder() -> String {\n    \
                  format!(\"aterm-supervise:{}\", std::process::id())\n}\n";
    let found = run(
        &SUPERVISE_FX,
        &[
            ("crates/aterm-agent/src/supervise/run.rs", doors),
            ("crates/aterm-agent/src/supervise/claim.rs", holder),
        ],
    );
    assert!(found.is_empty(), "{:?}", spots(&found));
    let elsewhere = "fn output_within() {\n    let _ = std::process::id();\n}\n\
                     fn default_holder() {\n    let _ = rx.try_recv();\n}\n";
    assert_eq!(
        spots(&run(
            &SUPERVISE_FX,
            &[("crates/aterm-agent/src/supervise/run.rs", elsewhere)]
        )),
        [
            "crates/aterm-agent/src/supervise/run.rs:2",
            "crates/aterm-agent/src/supervise/run.rs:5",
        ]
    );
    let bypass = "let now = Instant::now();\nstd::thread::sleep(D);\nlet w = limit::unix_now();\n";
    assert!(
        run(
            &SUPERVISE_FX,
            &[
                ("crates/aterm-agent/src/supervise/fx.rs", bypass),
                ("crates/aterm-agent/src/supervise/tape/codec.rs", bypass),
                ("crates/aterm-agent/src/supervise/transport.rs", bypass),
                ("crates/aterm-agent/src/supervise/mail.rs", bypass),
                ("crates/aterm-agent/src/supervise/run_tests.rs", bypass),
                ("crates/aterm-agent/src/harness/upgrade_drive.rs", bypass),
                ("crates/aterm-gui/src/harness_host.rs", bypass),
            ]
        )
        .is_empty()
    );
    assert_eq!(
        spots(&run(
            &SUPERVISE_FX,
            &[(
                "crates/aterm-agent/src/supervise/policy/turn_end.rs",
                bypass
            )]
        )),
        [
            "crates/aterm-agent/src/supervise/policy/turn_end.rs:1",
            "crates/aterm-agent/src/supervise/policy/turn_end.rs:2",
            "crates/aterm-agent/src/supervise/policy/turn_end.rs:3",
        ]
    );
}

/// A wall reader's home is its own `fn` in its own file for the wall-clock read alone.
#[test]
fn a_wall_readers_home_is_its_own_fn_in_its_own_file_for_the_wall_alone() {
    let limit = "pub fn unix_now() -> i64 {\n    \
                 aterm_time::unix_secs().map_or(0, |s| s as i64)\n}\n\
                 pub fn unix_now_ms() -> i64 {\n    std::thread::sleep(D);\n    \
                 let _ = std::time::Instant::now();\n    \
                 aterm_time::unix_millis().map_or(0, |m| m as i64)\n}\n";
    let journal = "pub fn unix_ms() -> i64 {\n    \
                   let _ = std::time::SystemTime::now();\n    \
                   aterm_time::unix_millis().map_or(0, |m| m as i64)\n}\n";
    let elsewhere = "impl Waiter {\n    fn unix_ms(&self) -> i64 {\n        \
                     std::thread::sleep(Duration::from_secs(5));\n        \
                     let _ = std::time::Instant::now();\n        \
                     aterm_time::unix_millis().map_or(0, |m| m as i64)\n    }\n}\n";
    assert_eq!(
        spots(&run(
            &SUPERVISE_FX,
            &[
                ("crates/aterm-agent/src/supervise/limit.rs", limit),
                ("crates/aterm-agent/src/supervise/journal.rs", journal),
                ("crates/aterm-agent/src/supervise/probe2.rs", elsewhere),
                ("crates/aterm-agent/src/supervise/policy/limit.rs", journal),
            ]
        )),
        [
            "crates/aterm-agent/src/supervise/limit.rs:5",
            "crates/aterm-agent/src/supervise/limit.rs:6",
            "crates/aterm-agent/src/supervise/policy/limit.rs:2",
            "crates/aterm-agent/src/supervise/policy/limit.rs:3",
            "crates/aterm-agent/src/supervise/probe2.rs:3",
            "crates/aterm-agent/src/supervise/probe2.rs:4",
            "crates/aterm-agent/src/supervise/probe2.rs:5",
        ]
    );
    let holder = "pub(super) fn default_holder() -> String {\n    \
                  format!(\"aterm-supervise:{}\", std::process::id())\n}\n\
                  fn default_holder_too() {\n    let _ = std::process::id();\n}\n";
    let wrong = "fn default_holder() {\n    let _ = rx.try_recv();\n}\n";
    assert_eq!(
        spots(&run(
            &SUPERVISE_FX,
            &[
                ("crates/aterm-agent/src/supervise/claim.rs", holder),
                ("crates/aterm-agent/src/supervise/run.rs", holder),
                ("crates/aterm-agent/src/supervise/escalate.rs", wrong),
            ]
        )),
        [
            "crates/aterm-agent/src/supervise/claim.rs:5",
            "crates/aterm-agent/src/supervise/escalate.rs:2",
            "crates/aterm-agent/src/supervise/run.rs:2",
            "crates/aterm-agent/src/supervise/run.rs:5",
        ]
    );
    let mixed_wall = "fn unix_now() {\n    \
                      let _ = SystemTime::now(); thread::sleep(D);\n    \
                      let _ = SystemTime::now(); let _ = process::id();\n    \
                      let _ = aterm_time::unix_secs(); let _ = rx.recv_timeout(D);\n    \
                      let _ = SystemTime::now(); let _ = aterm_time::unix_secs();\n}\n";
    let mixed_holder = "fn default_holder() {\n    \
                        let _ = process::id(); let _ = rx.try_recv();\n}\n";
    assert_eq!(
        spots(&run(
            &SUPERVISE_FX,
            &[
                ("crates/aterm-agent/src/supervise/limit.rs", mixed_wall),
                ("crates/aterm-agent/src/supervise/claim.rs", mixed_holder),
            ]
        )),
        [
            "crates/aterm-agent/src/supervise/claim.rs:2",
            "crates/aterm-agent/src/supervise/limit.rs:2",
            "crates/aterm-agent/src/supervise/limit.rs:3",
            "crates/aterm-agent/src/supervise/limit.rs:4",
        ]
    );
}

/// Imports that keep a clock module or type under its own name are silent.
#[test]
fn an_import_that_keeps_a_clocks_name_is_silent() {
    let src = "use std::thread;\nuse std::time::{Duration, Instant};\n\
               use std::time::SystemTime;\nuse std::fmt::Write as _;\n\
               use std::thread::sleep as _;\n\
               use super::approvals::{self, Ledger, typed_at_in};\n\
               use super::journal::{Journal, json_str, unix_ms_of};\n\
               use std::{\n    process,\n    thread::{self, spawn},\n};\n\
               use aterm_phase::prompt::fixtures as f;\n\
               use std::os::unix::fs::OpenOptionsExt;\n\
               type Clocked = fn() -> Instant;\ntype Mark = (Instant, u64);\n\
               pub(crate) type Stamp = super::policy::sleeps::SleepStamp;\n\
               #[cfg(test)]\nmod tests {\n    use std::thread::{self, sleep};\n    \
               use std::time::Instant as Mono;\n}\n";
    let found = run(
        &SUPERVISE_FX,
        &[("crates/aterm-agent/src/supervise/run.rs", src)],
    );
    assert!(found.is_empty(), "{:?}", spots(&found));
}

/// Each look at the machine made past a derived door is a finding.
#[test]
fn a_look_at_the_machine_past_a_derived_door_is_named() {
    let cases = [
        ("a folder listed", "let e = std::fs::read_dir(&dir)?;\n"),
        ("a file read through fs", "let b = fs::read(&p)?;\n"),
        ("a file opened by its type", "let f = File::open(&p)?;\n"),
        (
            "a file opened with options",
            "let f = std::fs::OpenOptions::new().append(true).open(p)?;\n",
        ),
        (
            "a path resolved",
            "let r = std::fs::canonicalize(p).ok();\n",
        ),
        ("a path resolved as a method", "let r = p.canonicalize();\n"),
        (
            "a link examined",
            "let m = std::fs::symlink_metadata(&p);\n",
        ),
        ("a link read", "let to = std::fs::read_link(&p);\n"),
        ("a metadata read", "let m = p.metadata()?;\n"),
        ("an existence asked", "if p.exists() {\n}\n"),
        ("a directory asked", "if p.is_dir() {\n}\n"),
        ("a file asked", "if p.is_file() {\n}\n"),
        (
            "a child spawned",
            "let mut c = aterm_uds::spawnfd::command(\"lsof\");\n",
        ),
        (
            "a process read through the kernel",
            "let a = atpkg::caller_shell::process_args(pid);\n",
        ),
        (
            "the kernel asked",
            "let cwd = aterm_sysprobe::process_cwd(pid);\n",
        ),
        (
            "this process's environment",
            "let p = std::env::var(\"PATH\");\n",
        ),
        (
            "the passwd database",
            "let h = atpkg::platform::account_home();\n",
        ),
        ("the zone read", "let o = limit::tz_offset_s();\n"),
        ("a zone's offset", "let o = limit::offset_at(Some(z), t);\n"),
        (
            "a named zone's offset",
            "let o = super::limit::zone_offset_at(z, t);\n",
        ),
        (
            "the local zone's offset passed as a fn item",
            "let at = limit::reset_at_with(&s, n, l, &limit::local_offset_at);\n",
        ),
        (
            "the injected zone reader read past its door",
            "let at = limit::reset_at_with(&s, n, l, &self.zone_offset);\n",
        ),
        (
            "the injected local reader called",
            "let o = (self.local_offset_at)(at);\n",
        ),
        ("a small file read bare", "let t = read_small(&p, 4096);\n"),
        (
            "a file opened by the footer's reader",
            "let f = crate::harness::footer::open_regular(&p)?;\n",
        ),
        (
            "the transcripts read",
            "ctx.shell_cwds = crate::harness::footer::shell_cwds(&claude, cwd);\n",
        ),
    ];
    for (why, src) in cases {
        let found = run(
            &SUPERVISE_FX,
            &[("crates/aterm-agent/src/supervise/approval_loop.rs", src)],
        );
        assert_eq!(
            spots(&found),
            ["crates/aterm-agent/src/supervise/approval_loop.rs:1"],
            "MISS — {why}:\n{src}"
        );
    }
}

/// Derived doors, home functions, and look-alike identifiers are silent.
#[test]
fn the_derived_doors_and_their_homes_are_silent() {
    let doors = "ctx.shell_cwds = ctx.probe.shell_cwds(w, cwd);\n\
                 let real = ctx.probe.real_components(comps);\n\
                 let o = self.fx.zone_offset(live, z, t);\n\
                 let o = self.fx.local_offset_at(live, t);\n\
                 let o = self.fx.tz_offset();\n\
                 let s = stamp(l.now_ms, l.tz_offset_s, true);\n\
                 file: Option<std::fs::File>,\n\
                 use crate::harness::footer::read_small;\n\
                 use std::fs;\n\
                 fn shell_cwds(&self, worker: &WorkerEnv, cwd: &Path) -> Vec<PathBuf>;\n\
                 let started_ms = thread.is_none();\n\
                 zone_offset: limit::zone_offset_at,\n\
                 // std::fs::read_dir(p) in prose\n\
                 let s = \"p.exists()\";\n\
                 #[cfg(test)]\nmod tests {\n    fn t() { let _ = std::fs::read(p); }\n}\n";
    assert_eq!(
        spots(&run(
            &SUPERVISE_FX,
            &[("crates/aterm-agent/src/supervise/approval_loop.rs", doors)]
        )),
        ["crates/aterm-agent/src/supervise/approval_loop.rs:12"]
    );
    let homes = "fn read(worker: &WorkerEnv) {\n    let t = read_small(p, 9);\n}\n\
                 fn parse(text: &str) {\n    let t = read_small(p, 9);\n}\n";
    assert_eq!(
        spots(&run(
            &SUPERVISE_FX,
            &[
                (
                    "crates/aterm-agent/src/supervise/policy/shell_startup.rs",
                    homes
                ),
                ("crates/aterm-agent/src/supervise/policy/question.rs", homes),
            ]
        )),
        [
            "crates/aterm-agent/src/supervise/policy/question.rs:2",
            "crates/aterm-agent/src/supervise/policy/question.rs:5",
            "crates/aterm-agent/src/supervise/policy/shell_startup.rs:5",
        ]
    );
    let live = "let e = std::fs::read_dir(&dir);\n\
                let a = atpkg::caller_shell::process_args(group);\n\
                let r = std::fs::canonicalize(p);\n";
    assert!(
        run(
            &SUPERVISE_FX,
            &[(
                "crates/aterm-agent/src/supervise/policy/probe_live.rs",
                live
            )]
        )
        .is_empty()
    );
    let clock = "fn read(worker: &WorkerEnv) {\n    let n = Instant::now();\n}\n";
    assert_eq!(
        spots(&run(
            &SUPERVISE_FX,
            &[(
                "crates/aterm-agent/src/supervise/policy/shell_startup.rs",
                clock
            )]
        )),
        ["crates/aterm-agent/src/supervise/policy/shell_startup.rs:2"]
    );
}

/// Stand-ins for the files that hold the policy's live bodies.
fn live_body_files() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "crates/aterm-agent/src/supervise/policy/shell_startup.rs",
            "pub fn read(worker: &WorkerEnv, launch: &Path) -> Result<S, String> {\n    \
             let s = snapshots(&claude)?;\n}\n\
             fn snapshots(claude: &Path) -> Result<Vec<PathBuf>, String> {\n    \
             let e = std::fs::read_dir(claude);\n}\n\
             pub fn tool_kinds(worker: &WorkerEnv) -> Vec<Kind> {\n    Vec::new()\n}\n",
        ),
        (
            "crates/aterm-agent/src/supervise/policy/claude_settings.rs",
            "pub fn owner_asks(worker: &WorkerEnv, launch: &Path) -> Vec<OwnerAsk> {\n    \
             for (s, v) in parsed(worker, launch) {}\n}\n\
             pub(crate) fn parsed(worker: &WorkerEnv, launch: &Path) -> Vec<Doc> {\n    \
             let t = read_settings_in(None, p);\n}\n\
             fn read_settings_in(fence: Option<&Path>, path: &Path) -> Option<String> {\n    \
             crate::harness::footer::read_small(path, 9)\n}\n\
             pub fn rule_tool(rule: &str) -> Option<&str> {\n    None\n}\n",
        ),
        (
            "crates/aterm-agent/src/supervise/policy/git_config.rs",
            "impl WorkerEnv {\n    pub fn raw_git_view(\n        &self,\n        dir: &Path,\n    \
             ) -> Result<GitView, String> {\n        self.view_at(dir)\n    }\n    \
             fn view_at(&self, dir: &Path) -> Result<GitView, String> {\n        \
             let o = aterm_uds::spawnfd::command(\"git\");\n    }\n    \
             pub fn git_settings_env(&self, launch: &Path) -> Result<(), String> {\n        \
             self.settings_env_where(launch)\n    }\n    \
             pub fn settings_env_where(&self, launch: &Path) -> Result<(), String> {\n        \
             for d in super::claude_settings::parsed(self, launch) {}\n    }\n    \
             pub fn var(&self, key: &str) -> Option<&str> {\n        None\n    }\n}\n",
        ),
        (
            "crates/aterm-agent/src/supervise/limit.rs",
            "pub fn zone_offset_at(zone: &str, unix: i64) -> Option<i64> {\n    \
             let t = std::fs::read(zone);\n}\n\
             fn offset_in(zone_offset_at: &dyn Fn(&str) -> Option<i64>) -> i64 {\n    \
             zone_offset_at(\"z\").unwrap_or(0)\n}\n\
             pub fn reset_at_with(zone_offset_at: &dyn Fn(&str) -> Option<i64>) -> i64 {\n    \
             offset_in(zone_offset_at)\n}\n\
             fn read_offset(zone: &str, unix: i64) -> Option<i64> {\n    \
             let t = std::fs::read(zone);\n}\n\
             pub fn rogue_reset(unix: i64) -> i64 {\n    \
             read_offset(\"z\", unix).unwrap_or(0)\n}\n",
        ),
    ]
}

/// Calling or importing a probe's live body from above its door is flagged.
#[test]
fn a_probes_live_body_called_above_its_door_is_named() {
    let lp = "let s = crate::supervise::policy::shell_startup::read(w, cwd);\n\
              let a = crate::supervise::policy::claude_settings::owner_asks(w, cwd);\n\
              let w = crate::supervise::policy::LiveProbe.worker_proc(group, &sid);\n\
              ctx.shell_cwds = crate::harness::footer::shell_cwds(&claude, cwd);\n";
    let policy = "worker.git_settings_env(&ctx.cwd)?;\n\
                  let v = worker.raw_git_view(dir, &|_| false)?;\n\
                  let t = LiveProbe.hidden_twin(comps)?;\n\
                  let d = super::claude_settings::parsed(worker, launch);\n";
    let te = "let r = super::limit::rogue_reset(now);\n";
    let imports = "use super::policy::shell_startup::read;\n\
                   use super::policy::{\n    \
                   claude_settings::{OwnerAsk, owner_asks},\n    \
                   probe::PolicyProbe,\n};\n";
    let mut files = live_body_files();
    files.extend([
        ("crates/aterm-agent/src/supervise/approval_loop.rs", lp),
        (
            "crates/aterm-agent/src/supervise/policy/approval.rs",
            policy,
        ),
        ("crates/aterm-agent/src/supervise/turn_end_loop.rs", te),
        ("crates/aterm-agent/src/supervise/run.rs", imports),
    ]);
    assert_eq!(
        spots(&run(&SUPERVISE_FX, &files)),
        [
            "crates/aterm-agent/src/supervise/approval_loop.rs:1",
            "crates/aterm-agent/src/supervise/approval_loop.rs:2",
            "crates/aterm-agent/src/supervise/approval_loop.rs:3",
            "crates/aterm-agent/src/supervise/approval_loop.rs:4",
            "crates/aterm-agent/src/supervise/policy/approval.rs:1",
            "crates/aterm-agent/src/supervise/policy/approval.rs:2",
            "crates/aterm-agent/src/supervise/policy/approval.rs:3",
            "crates/aterm-agent/src/supervise/policy/approval.rs:4",
            "crates/aterm-agent/src/supervise/run.rs:1",
            "crates/aterm-agent/src/supervise/run.rs:2",
            "crates/aterm-agent/src/supervise/turn_end_loop.rs:1",
        ]
    );
}

/// Calls on the probe, pure helpers in live-body files, injected readers and internal
/// live-body chains are silent.
#[test]
fn the_probe_and_its_live_bodies_own_files_are_silent() {
    let policy = "ctx.probe.settings_env(worker, &ctx.cwd)?;\n\
                  let v = ctx.probe.git_view(worker, dir, &writes)?;\n\
                  let s = ctx.probe.shell_startup(w, cwd);\n\
                  let a = ctx.probe.owner_asks(w, cwd);\n\
                  let k = super::shell_startup::tool_kinds(&w);\n\
                  let r = super::claude_settings::rule_tool(rule);\n\
                  let e = worker.var(\"HOME\");\n\
                  let p = self.fx.policy_probe();\n\
                  let at = super::limit::reset_at_with(&|z| self.fx.zone_offset(z));\n\
                  use super::shell_startup::ShellStartup;\n\
                  fn read(&self) -> u8 {\n    0\n}\n";
    let door = "pub struct LiveProbe;\n\
                #[derive(Clone)]\npub(crate) struct TapedProbe {\n    live: LiveProbe,\n}\n";
    let arm = "impl PolicyProbe for LiveProbe {\n    \
               fn shell_startup(&self, w: &WorkerEnv, l: &Path) -> R {\n        \
               super::shell_startup::read(w, l)\n    }\n    \
               fn settings_env(&self, w: &WorkerEnv, l: &Path) -> Result<(), String> {\n        \
               w.git_settings_env(l)\n    }\n}\n";
    let mut files = live_body_files();
    files.extend([
        (
            "crates/aterm-agent/src/supervise/policy/approval.rs",
            policy,
        ),
        ("crates/aterm-agent/src/supervise/policy/probe.rs", door),
        ("crates/aterm-agent/src/supervise/policy/probe_live.rs", arm),
    ]);
    let found = run(&SUPERVISE_FX, &files);
    assert!(found.is_empty(), "{:?}", spots(&found));
}

/// By-path ledger readers are detected by their `_in` companion convention.
#[test]
fn the_ledgers_readers_by_path_are_found_by_their_convention() {
    let ledger = "pub fn awaited_try_in(body: &str) -> u8 {\n    0\n}\n\
                  pub fn awaited_try(path: &Path) -> u8 {\n    \
                  awaited_try_in(&std::fs::read_to_string(path).unwrap_or_default())\n}\n\
                  pub fn rogue(path: &Path) {\n    let _ = std::fs::read(path);\n}\n";
    let lp = "let t = approvals::awaited_try(path, sid);\n\
              let u = approvals::awaited_try_in(&body, sid);\n\
              let v = super::approvals::awaited_try(path, sid);\n\
              use super::approvals::{awaited_try_in, awaited_try};\n\
              use super::approvals::awaited_try_in;\n";
    assert_eq!(
        spots(&run(
            &SUPERVISE_FX,
            &[
                ("crates/aterm-agent/src/supervise/approvals.rs", ledger),
                ("crates/aterm-agent/src/supervise/turn_end_loop.rs", lp),
            ]
        )),
        [
            "crates/aterm-agent/src/supervise/approvals.rs:8",
            "crates/aterm-agent/src/supervise/turn_end_loop.rs:1",
            "crates/aterm-agent/src/supervise/turn_end_loop.rs:3",
            "crates/aterm-agent/src/supervise/turn_end_loop.rs:4",
        ]
    );
}

#[test]
fn a_store_current_path_is_resolved_within_six_lines() {
    let src = "fn b(p: &Path) -> PathBuf {\n    let c = p.join(\"store/trust/current/bin\");\n    \
               std::fs::canonicalize(c).unwrap()\n}\n\
               fn d(p: &Path) -> PathBuf {\n    physical(&p.join(\"current\"))\n}\n\
               fn e(p: &Path) -> PathBuf {\n    p.join(\"currently\").join(\"a/current_x\")\n}\n\
               // p.join(\"current\") in prose\n\
               #[cfg(test)]\nmod tests {\n    fn t(p: &Path) { p.join(\"current\"); }\n}\n\
               fn a(p: &Path) -> PathBuf {\n    p.join(\"current\")\n}\n\
               fn c() -> PathBuf {\n    PathBuf::from(\"/opt/store/x/current\")\n}\n";
    let found = run(
        &STORE_CURRENT,
        &[
            ("crates/aterm-verify/src/lib.rs", src),
            ("crates/atpkg/src/store.rs", src),
            ("crates/aterm-verify/tests/t.rs", src),
            ("crates/aterm-spec/src/verify_tests.rs", src),
        ],
    );
    assert_eq!(
        spots(&found),
        [
            "crates/aterm-verify/src/lib.rs:17",
            "crates/aterm-verify/src/lib.rs:20",
        ]
    );
}

#[test]
fn a_header_needs_both_lines_in_either_order() {
    let found = run(
        &LICENCE_HEADER,
        &[
            ("crates/a/src/spdx_first.rs", HEADER),
            (
                "crates/a/src/copyright_first.rs",
                "// Copyright 2026 Andrew Yates\n// SPDX-License-Identifier: MIT\n",
            ),
            ("crates/a/src/bare.rs", "fn main() {}\n"),
            (
                "crates/a/src/late.rs",
                "\n\n\n\n\n\n\n\n// SPDX-License-Identifier: Apache-2.0\n// Copyright\n",
            ),
            ("vendor/indexmap/src/lib.rs", "fn main() {}\n"),
        ],
    );
    assert_eq!(
        spots(&found),
        ["crates/a/src/bare.rs:1", "crates/a/src/late.rs:1"]
    );
}

#[test]
fn a_derived_head_says_what_aterm_changed() {
    let derived = |notice: &str| {
        format!(
            "{HEADER}//\n// Derived from x11rb 0.13.2 src/properties.rs{notice}\n\nfn f() {{}}\n"
        )
    };
    let bare = derived(".");
    let modified = derived(", modified by the aterm project in 2026.");
    let partial = derived("; the rest of the file is aterm's.");
    let generated = format!("{HEADER}// @generated by `x gen`\n// Derived from windows-sys\n");
    let module_doc = format!("{HEADER}\n//! Offsets derived from the model's runs.\n");
    let found = run(
        &LICENCE_HEADER,
        &[
            ("crates/a/src/bare.rs", bare.as_str()),
            ("crates/a/src/modified.rs", modified.as_str()),
            ("crates/a/src/partial.rs", partial.as_str()),
            ("crates/a/src/generated.rs", generated.as_str()),
            ("crates/a/src/module_doc.rs", module_doc.as_str()),
        ],
    );
    assert_eq!(spots(&found), ["crates/a/src/bare.rs:1"]);
}
