// Copyright (c) 2014 Alex Crichton
// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from wasm-bindgen-cli-support 0.2.108 src/js/mod.rs and
// src/js/binding.rs: the shape of the JS each export's binding writes (the
// argument lowering, the lifting of returns into fresh typed arrays and
// strings with their frees, the class wrap, constructor and
// `Symbol.dispose` alias, and the import shims), so a page written against
// wasm-bindgen's glue sees the same JS. Modified by the aterm project in
// 2026: it is written from aterm-wasm-abi's descriptor, every call is
// followed by the return slot's throw check, and the externref table is
// gone. See ../../LICENSE-MIT for the upstream license; Apache-2.0 is the
// workspace's.

use std::collections::BTreeMap;

use super::*;
use crate::wasm_glue::model::Host;
use crate::wasm_glue::webgl;

/// The GL import at `name` in the table.
fn gl(name: &str) -> Host {
    Host::Gl(
        webgl::IMPORTS
            .iter()
            .position(|g| g.name == name)
            .expect("a listed import"),
    )
}

/// A JS object argument is stored in the object table and crosses as its
/// index, typed in the `.d.ts` by the name its descriptor carries; the
/// `aterm_gl` imports bring the WebGL2 state and the table they read the
/// canvas from, and a module importing neither gets none of it.
#[test]
fn a_js_object_crosses_by_its_table_slot_and_gl_imports_bring_their_state() {
    let init = Func {
        kind: kind::FUNCTION,
        js_name: "init".into(),
        symbol: "init".into(),
        docs: String::new(),
        receiver: false,
        params: vec![("canvas".into(), Ty::JsObject("HTMLCanvasElement".into()))],
        ret: Ty::Unit,
    };
    let iface = Interface {
        classes: Vec::new(),
        functions: vec![init.clone()],
        imports: vec![Host::ObjectDrop, gl("context_create"), gl("make_current")],
        raw: BTreeMap::new(),
    };
    let g = generate(&iface, "m");
    assert!(
        g.js.contains("wasm.init(addHeapObject(canvas), 0);"),
        "{}",
        g.js
    );
    assert!(
        g.dts
            .contains("export function init(canvas: HTMLCanvasElement): void;")
    );
    for want in [
        "function addHeapObject(obj)",
        "dropObject(arg0);",
        "const glContexts = [null];",
        "canvas.getContext('webgl2', { antialias: false })",
        "make_current: function(arg0)",
        "cachedFloat32ArrayMemory0 = null;",
    ] {
        assert!(g.js.contains(want), "the glue lacks {want:?}");
    }
    let plain = Interface {
        imports: Vec::new(),
        functions: vec![Func {
            params: Vec::new(),
            ..init
        }],
        ..iface
    };
    let g = generate(&plain, "m");
    assert!(!g.js.contains("addHeapObject") && !g.js.contains("GLctx"));
}

/// The GL-lifetime fixture's glue: the generator on the imports
/// `gl-lifetimes.mjs` drives.
#[cfg(unix)]
fn lifetime_glue() -> String {
    let iface = Interface {
        classes: Vec::new(),
        functions: vec![Func {
            kind: kind::FUNCTION,
            js_name: "create_context".into(),
            symbol: "create_context".into(),
            docs: String::new(),
            receiver: false,
            params: vec![("canvas".into(), Ty::JsObject("HTMLCanvasElement".into()))],
            ret: Ty::U32,
        }],
        imports: std::iter::once(Host::ObjectDrop)
            .chain(
                [
                    "context_create",
                    "context_drop",
                    "make_current",
                    "glGetString",
                    "glCreateProgram",
                    "glDeleteProgram",
                    "glGetUniformLocation",
                    "glUniform1i",
                ]
                .into_iter()
                .map(gl),
            )
            .collect(),
        raw: BTreeMap::new(),
    };
    generate(&iface, "lifetime").js
}

/// Run `gl-lifetimes.mjs` on `js` in a headless page and return whether it held along with console output.
#[cfg(unix)]
fn drive_lifetimes(js: &str, keep: bool) -> (bool, String) {
    let dir = aterm_tempfile::Builder::new()
        .prefix("atv-gl-life-")
        .suffix(".noindex")
        .tempdir()
        .expect("exclusive fixture root");
    std::fs::write(dir.path().join("lifetime.js"), js).expect("owned generated glue");
    std::fs::write(
        dir.path().join("driver.mjs"),
        include_str!("../../browser/gl-lifetimes.mjs"),
    )
    .expect("owned driver");
    let mut page = crate::wasm_page::start_for_test(dir.path());
    let ran = page.run("driver.mjs", &aterm_json::json!({"glue": "./lifetime.js"}));
    let console = match &ran {
        Ok(ran) => ran.console.join("\n"),
        Err(refused) => refused.why().to_string(),
    };
    eprintln!("{console}");
    if ran.is_err() && keep {
        drop(page);
        eprintln!(
            "GL_LIFETIME_OWNED_FIXTURE_RETAINED: {}",
            dir.keep().display()
        );
    }
    (ran.is_ok(), console)
}

/// Generated imports retire only their context or program's owned strings and uniform locations.
#[cfg(unix)]
#[test]
fn context_strings_and_program_locations_retire_with_their_owner() {
    let (held, console) = drive_lifetimes(&lifetime_glue(), true);
    assert!(
        console.contains("GL_LIFETIME_ADMITTED_CONTROLS_REACHED"),
        "fixture controls not reached: {console}"
    );
    assert!(
        console.contains("GL_LIFETIME_RETIREMENT_CASES_REACHED"),
        "retirement cases not reached: {console}"
    );
    assert!(
        held && console.contains("GL_LIFETIME_ALL_RETIRED"),
        "GL_LIFETIME_RETIREMENT_MISSING: {console}"
    );
}

/// Glue that reaches the network on import or a real canvas GPU context in `context_create` fails the fixture fence.
#[cfg(unix)]
#[test]
fn glue_that_reaches_the_network_or_a_real_gpu_fails_the_gl_fixture() {
    let js = lifetime_glue();
    let real_canvas = "canvas.getContext('webgl2', { antialias: false })";
    assert!(js.contains(real_canvas), "{js}");
    for (planted, reached) in [
        (format!("void fetch;\n{js}"), "fetch"),
        (
            js.replace(
                real_canvas,
                "document.createElement('canvas').getContext('webgl2', { antialias: false })",
            ),
            "getContext",
        ),
    ] {
        let (held, console) = drive_lifetimes(&planted, false);
        assert!(
            !held && console.contains(&format!("GL_LIFETIME_FENCE: the glue reached {reached}")),
            "{reached}: {console}"
        );
    }
}
