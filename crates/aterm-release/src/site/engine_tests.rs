// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_flags_are_the_site_scripts_in_its_order() {
    let flags = rustflags(Path::new("/tmp/c lone"), Some(Path::new("/Users//b")));
    assert_eq!(
        flags,
        [
            "-C",
            "target-feature=+simd128",
            "--remap-path-prefix=/Users//b/.cargo=/cargo",
            "--remap-path-prefix=/tmp/c lone=/aterm",
            "--remap-path-prefix=/Users//b=/build",
        ]
    );
    assert_eq!(
        rustflags(Path::new("/c"), None)[2..],
        ["--remap-path-prefix=/c=/aterm"]
    );
    assert_eq!(usable_home(Some(Path::new("/"))), None);
    assert_eq!(usable_home(Some(Path::new("relative"))), None);
}

#[test]
fn a_path_anywhere_in_the_module_is_found() {
    let home = Path::new("/Users//b");
    let clone = Path::new("/var/x/clone");
    let mut module = b"\0asm\x01\0\0\0panicked at /aterm/src/lib.rs".to_vec();
    assert_eq!(leaked(&module, [home, clone]), None);
    module.extend_from_slice(b"/var/x/clone/crates/aterm-wasm/src/lib.rs");
    assert_eq!(leaked(&module, [home, clone]), Some(clone));
    assert_eq!(leaked(&module, [Path::new("")]), None);
}

#[test]
fn the_module_url_is_renamed_in_either_quote_and_a_glue_without_one_is_refused() {
    let js = "a = new URL(\"aterm_wasm_bg.wasm\", import.meta.url);\nb('aterm_wasm_bg.wasm');\n\
              c(\"aterm_wasm_bg.wasm');\n// aterm_wasm_bg.wasm stays in prose\n";
    assert_eq!(
        rename_module(js, "aterm_wasm.0123456789ab.wasm").unwrap(),
        "a = new URL(\"aterm_wasm.0123456789ab.wasm\", import.meta.url);\n\
         b('aterm_wasm.0123456789ab.wasm');\nc(\"aterm_wasm.0123456789ab.wasm');\n\
         // aterm_wasm_bg.wasm stays in prose\n"
    );
    assert_eq!(
        rename_module("'aterm_wasm_bg.wasm'aterm_wasm_bg.wasm'", "x.wasm").unwrap(),
        "'x.wasm'aterm_wasm_bg.wasm'"
    );
    let why = rename_module("load(`aterm_wasm_bg.wasm`)", "x.wasm").unwrap_err();
    assert!(why.contains("never names aterm_wasm_bg.wasm"), "{why}");
}

#[test]
fn the_pair_replaces_every_other_engine_file_and_nothing_else() {
    let keep = ["aterm_wasm.0123456789ab.js", "aterm_wasm.0123456789ab.wasm"];
    for name in [
        "aterm_wasm.5e1b202b6f21.js",
        "aterm_wasm.5e1b202b6f21.wasm",
        "aterm_wasm.js",
        "aterm_wasm_bg.wasm",
    ] {
        assert!(replaced(name, keep), "{name}");
    }
    for name in keep.into_iter().chain([
        "engine.js",
        "ENGINE_SOURCE",
        "boot.js",
        "canary.dd580a62d497.island.wasm",
        "aterm_wasm.d.ts",
        "aterm_wasm.wasm",
    ]) {
        assert!(!replaced(name, keep), "{name}");
    }
    assert_eq!(
        engine_js("0123456789ab"),
        "export * from \"./aterm_wasm.0123456789ab.js\";\n\
         export { default } from \"./aterm_wasm.0123456789ab.js\";\n"
    );
}

#[test]
fn installing_an_engine_preserves_files_outside_its_asset_schema() {
    let private = aterm_tempfile::Builder::new()
        .prefix("site-engine-schema-")
        .tempdir()
        .unwrap();
    let term = private.path();
    install(term, "111111111111", "old glue", b"old module").unwrap();
    for legacy in ["aterm_wasm.js", "aterm_wasm_bg.wasm"] {
        std::fs::write(term.join(legacy), b"legacy").unwrap();
    }
    let unowned = [
        "aterm_wasm.notes.js",
        "aterm_wasm.0123456789AB.js",
        "aterm_wasm.0123456789a.js",
        "aterm_wasm.0123456789abc.wasm",
        "aterm_wasm.0123456789az.js",
        "aterm_wasm.0123456789ab.js.map",
    ];
    for name in unowned {
        std::fs::write(term.join(name), b"private bytes").unwrap();
    }
    install(term, "222222222222", "new glue", b"new module").unwrap();
    for name in unowned {
        assert_eq!(
            std::fs::read(term.join(name)).ok(),
            Some(b"private bytes".to_vec()),
            "preserve {name}"
        );
    }
    for old in [
        "aterm_wasm.111111111111.js",
        "aterm_wasm.111111111111.wasm",
        "aterm_wasm.js",
        "aterm_wasm_bg.wasm",
    ] {
        assert!(!term.join(old).exists(), "retire {old}");
    }
    assert_eq!(
        std::fs::read(term.join("aterm_wasm.222222222222.js")).unwrap(),
        b"new glue"
    );
    assert_eq!(
        std::fs::read_to_string(term.join("engine.js")).unwrap(),
        engine_js("222222222222")
    );
}

#[test]
#[cfg(unix)]
fn replacing_the_engine_pointer_does_not_write_through_a_link() {
    let private = aterm_tempfile::Builder::new()
        .prefix("site-engine-pointer-")
        .tempdir()
        .unwrap();
    let term = private.path().join("term");
    install(&term, "111111111111", "old glue", b"old module").unwrap();
    let note = private.path().join("private-note");
    std::fs::write(&note, b"private bytes").unwrap();
    std::fs::remove_file(term.join("engine.js")).unwrap();
    std::os::unix::fs::symlink(&note, term.join("engine.js")).unwrap();
    let installed = install(&term, "222222222222", "new glue", b"new module");
    assert_eq!(
        std::fs::read(&note).unwrap(),
        b"private bytes",
        "the pointer's link target stays untouched"
    );
    installed.unwrap();
    assert!(
        std::fs::symlink_metadata(term.join("engine.js"))
            .unwrap()
            .file_type()
            .is_file()
    );
    assert_eq!(
        std::fs::read_to_string(term.join("engine.js")).unwrap(),
        engine_js("222222222222")
    );
}

#[test]
fn a_pointer_stage_collision_keeps_the_old_engine_and_the_collision() {
    let private = aterm_tempfile::Builder::new()
        .prefix("site-engine-stage-")
        .tempdir()
        .unwrap();
    let term = private.path();
    install(term, "111111111111", "old glue", b"old module").unwrap();
    let before = std::fs::read(term.join("engine.js")).unwrap();
    let stage = term.join(format!(".engine.js.tmp.{}", std::process::id()));
    std::fs::write(&stage, b"private stage bytes").unwrap();
    let installed = install(term, "222222222222", "new glue", b"new module");
    assert_eq!(
        std::fs::read(&stage).ok(),
        Some(b"private stage bytes".to_vec()),
        "a collision stays untouched"
    );
    assert_eq!(
        std::fs::read(term.join("engine.js")).unwrap(),
        before,
        "a failed pointer stage leaves the old engine"
    );
    assert!(installed.is_err());
    assert_eq!(
        std::fs::read(term.join("aterm_wasm.111111111111.js")).unwrap(),
        b"old glue"
    );
    assert_eq!(
        std::fs::read(term.join("aterm_wasm.111111111111.wasm")).unwrap(),
        b"old module"
    );
}

#[test]
fn an_existing_immutable_url_cannot_acquire_different_glue() {
    let private = aterm_tempfile::Builder::new()
        .prefix("site-engine-immutable-")
        .tempdir()
        .unwrap();
    let term = private.path();
    install(term, "111111111111", "old glue", b"same module").unwrap();
    install(term, "111111111111", "old glue", b"same module").unwrap();
    let before = std::fs::read(term.join("engine.js")).unwrap();
    let installed = install(term, "111111111111", "different glue", b"same module");
    assert_eq!(
        std::fs::read(term.join("aterm_wasm.111111111111.js")).unwrap(),
        b"old glue",
        "an immutable URL keeps its original bytes"
    );
    assert_eq!(
        std::fs::read(term.join("aterm_wasm.111111111111.wasm")).unwrap(),
        b"same module"
    );
    assert_eq!(std::fs::read(term.join("engine.js")).unwrap(), before);
    assert!(installed.is_err());
}

#[test]
#[cfg(unix)]
fn an_existing_immutable_asset_link_is_refused_without_writing_its_target() {
    let private = aterm_tempfile::Builder::new()
        .prefix("site-engine-asset-link-")
        .tempdir()
        .unwrap();
    let term = private.path().join("term");
    install(&term, "111111111111", "old glue", b"old module").unwrap();
    let before = std::fs::read(term.join("engine.js")).unwrap();
    let note = private.path().join("private-note");
    std::fs::write(&note, b"private bytes").unwrap();
    std::os::unix::fs::symlink(&note, term.join("aterm_wasm.222222222222.js")).unwrap();
    let installed = install(&term, "222222222222", "new glue", b"new module");
    assert_eq!(
        std::fs::read(&note).unwrap(),
        b"private bytes",
        "an asset link target stays untouched"
    );
    assert_eq!(std::fs::read(term.join("engine.js")).unwrap(), before);
    assert!(installed.is_err());
}
