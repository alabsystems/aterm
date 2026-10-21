// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `verify glue <in.wasm> <out-dir> --flavor web|nodejs`: the JS glue
//! for a module built with aterm-wasm-abi's `#[export]`, in place of the
//! wasm-bindgen CLI (docs/AUDIT-third-party-2026-10-15.md, wasm-platform
//! WP.6).
//!
//! It reads the `aterm_abi` descriptor from the module cargo wrote, checks
//! every record against the module's real export and import signatures,
//! and writes, for `<stem>.wasm`:
//!
//! * `<stem>_bg.wasm`: the module less the descriptor section, the DWARF
//!   `.debug_*` sections (wasm-bindgen drops them too unless asked to keep
//!   them) and the linker's `__data_end`/`__heap_base` exports (run
//!   `wasm-opt` on this one, never before binding);
//! * `<stem>.js`: an ES module with `init`/`initSync` (`web`) or a
//!   CommonJS module that loads the wasm at `require` (`nodejs`);
//! * `<stem>.d.ts` (with the docs as JSDoc) and `<stem>_bg.wasm.d.ts`.
//!
//! A module without the section, with records of another version, or whose
//! exports disagree with its records is refused: nothing is written.
//!
//! [`glue`] is also a library entry: `ship site` (aterm-release) binds the
//! `/terminal` engine with it in process.

use std::path::{Path, PathBuf};

mod js;
mod model;
pub(crate) mod oracle;
mod templates;
pub(crate) mod toy;
mod wasm;
mod webgl;

pub use js::Flavor;
/// What `ship provision` acquires for the `wasm-glue` check's oracle row: the
/// pinned CLI, into the machine-wide tooling the row reads.
pub use oracle::{BINDGEN, BINDGEN_INSTALL, bindgen_root, reports_pin, tooling};

/// The pass the shipping pipeline applies to a bound `<stem>_bg.wasm`, never
/// before binding: `-O3` (speed, matching the cargo opt-3 build) with the
/// shipping feature flags. The `wasm` perf lane and `ship site`'s `/terminal`
/// engine both run it.
pub const WASM_OPT_FLAGS: [&str; 7] = [
    "-O3",
    "--enable-bulk-memory",
    "--enable-nontrapping-float-to-int",
    "--enable-sign-ext",
    "--enable-mutable-globals",
    "--enable-reference-types",
    "--enable-simd",
];

/// The files for `bytes`, a module named `stem`: (name, contents) pairs.
///
/// # Errors
/// A stem that is not one filename component, or a module the generator
/// refuses (no `aterm_abi` section, records of another version, exports that
/// disagree with the records).
pub fn glue(bytes: &[u8], stem: &str, flavor: Flavor) -> Result<Vec<(String, Vec<u8>)>, String> {
    if stem.is_empty() || Path::new(stem).file_name().and_then(|s| s.to_str()) != Some(stem) {
        return Err("the module stem must be one filename component".to_string());
    }
    let module = wasm::Module::parse(bytes)?;
    let iface = model::interface(&module)?;
    let g = js::generate(&iface, stem, flavor);
    let mut dropped: Vec<&str> = module
        .custom_names()
        .filter(|n| n.starts_with(".debug_"))
        .collect();
    dropped.push(aterm_wasm_abi::descriptor::SECTION);
    let bg = module.rewrite(&dropped, &model::DROPPED_EXPORTS);
    Ok(vec![
        (format!("{stem}.js"), g.js.into_bytes()),
        (format!("{stem}.d.ts"), g.dts.into_bytes()),
        (format!("{stem}_bg.wasm"), bg),
        (format!("{stem}_bg.wasm.d.ts"), g.bg_dts.into_bytes()),
    ])
}

/// Bind `input` into `out_dir`; the files written.
pub(crate) fn bind(input: &Path, out_dir: &Path, flavor: Flavor) -> Result<Vec<PathBuf>, String> {
    let bytes =
        std::fs::read(input).map_err(|e| format!("could not read {}: {e}", input.display()))?;
    let stem = input
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| format!("{}: no file name", input.display()))?;
    let files = glue(&bytes, stem, flavor).map_err(|e| format!("{}: {e}", input.display()))?;
    std::fs::create_dir_all(out_dir)
        .map_err(|e| format!("could not create {}: {e}", out_dir.display()))?;
    let mut written = Vec::new();
    for (name, contents) in files {
        let path = out_dir.join(name);
        std::fs::write(&path, contents)
            .map_err(|e| format!("could not write {}: {e}", path.display()))?;
        written.push(path);
    }
    Ok(written)
}

pub(crate) fn parse_flavor(s: &str) -> Option<Flavor> {
    match s {
        "web" => Some(Flavor::Web),
        "nodejs" => Some(Flavor::Nodejs),
        _ => None,
    }
}

const USAGE: &str = "usage: targo --unverified verify glue <in.wasm> <out-dir> --flavor web|nodejs\n\
       targo --unverified verify glue --record-toy      (re-record the toy's goldens and fixture)\n\
       targo --unverified verify glue --record-replay   (re-record the CPU module's boundary answers)";

/// `verify glue …`, answering its exit code: the generator over one module;
/// `--record-toy`, the `wasm-glue` check's goldens and fixture rewritten; or
/// `--record-replay`, the `wasm-smoke` check's CPU replay answers rewritten.
#[must_use]
pub fn run(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return crate::exit::PASS;
    }
    if args == ["--record-toy"] || args == ["--record-replay"] {
        let root = crate::perf::workspace_root();
        let mut log = String::new();
        let run = if args == ["--record-toy"] {
            toy::gate(&root, &root.join("target/wasm-glue-toy"), true, &mut log)
        } else {
            crate::perf::wasm::record_replay(&mut log)
        };
        eprint!("{log}");
        return match run.status {
            crate::perf::Status::Pass => crate::exit::PASS,
            crate::perf::Status::Skip { why, fix } => {
                eprintln!("glue: could not record — {why} (fix: {fix})");
                crate::exit::COULD_NOT_RUN
            }
            crate::perf::Status::Fail(findings) => {
                for f in findings {
                    eprintln!("glue: {f}");
                }
                crate::exit::FAILED
            }
            _ => crate::exit::FAILED,
        };
    }
    let Some((input, out, flavor)) = parse(args) else {
        eprintln!("{USAGE}");
        return crate::exit::USAGE;
    };
    match bind(Path::new(input), Path::new(out), flavor) {
        Ok(files) => {
            for f in files {
                eprintln!("  wrote {}", f.display());
            }
            crate::exit::PASS
        }
        Err(e) => {
            eprintln!("glue: {e}");
            crate::exit::FAILED
        }
    }
}

/// The generator's own arguments: the input, the output dir, the flavor.
fn parse(args: &[String]) -> Option<(&String, &String, Flavor)> {
    let mut positional = Vec::new();
    let mut flavor = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--flavor" => flavor = Some(it.next().and_then(|f| parse_flavor(f))?),
            _ => positional.push(a),
        }
    }
    match (flavor, positional.as_slice()) {
        (Some(flavor), [input, out]) => Some((*input, *out, flavor)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::perf::workspace_root;

    /// The committed toy module, bound in both flavors, is its goldens byte
    /// for byte: the generator's output on a real module, natively.
    /// The `wasm-glue` check holds a fresh build of the toy to the same
    /// goldens and runs the glue under node.
    #[test]
    fn the_toy_module_binds_to_its_goldens() {
        let root = workspace_root();
        let bytes = std::fs::read(toy::fixture(&root)).unwrap();
        for flavor in [Flavor::Web, Flavor::Nodejs] {
            let files = glue(&bytes, toy::STEM, flavor).unwrap();
            assert_eq!(files.len(), 4);
            for (name, contents) in files.iter().filter(|(n, _)| toy::is_text(n)) {
                let path = toy::golden_dir(&root, flavor).join(name);
                let want = std::fs::read_to_string(&path).unwrap();
                let got = String::from_utf8_lossy(contents);
                assert!(
                    got == want,
                    "{}: {}",
                    path.display(),
                    toy::first_difference(&got, &want)
                );
            }
            let (_, bg) = files.iter().find(|(n, _)| n.ends_with("_bg.wasm")).unwrap();
            let m = wasm::Module::parse(bg).unwrap();
            assert!(m.custom(aterm_wasm_abi::descriptor::SECTION).is_empty());
            assert!(!m.exports.iter().any(|e| e.name == "__heap_base"));
        }
    }

    /// The bound module carries no DWARF, as wasm-bindgen's does not; the
    /// other custom sections stay (the control: `producers` survives).
    #[test]
    fn the_bound_module_drops_dwarf_and_keeps_other_custom_sections() {
        let mut bytes = std::fs::read(toy::fixture(&workspace_root())).unwrap();
        for name in [".debug_info", "producers", ".debug_line"] {
            let mut payload = Vec::new();
            wasm::leb(&mut payload, name.len() as u32);
            payload.extend(name.as_bytes());
            payload.extend(b"payload");
            bytes.push(0);
            wasm::leb(&mut bytes, payload.len() as u32);
            bytes.extend(payload);
        }
        let files = glue(&bytes, toy::STEM, Flavor::Web).unwrap();
        let (_, bg) = files.iter().find(|(n, _)| n.ends_with("_bg.wasm")).unwrap();
        let names: Vec<String> = wasm::Module::parse(bg)
            .unwrap()
            .custom_names()
            .map(str::to_string)
            .collect();
        assert_eq!(names, ["producers"]);
    }

    fn refusal(bytes: &[u8]) -> String {
        glue(bytes, "m", Flavor::Web).expect_err("refused")
    }

    /// NEGATIVE CONTROLS on the real module: what the generator must refuse,
    /// it refuses, and says why.
    #[test]
    fn a_module_the_descriptor_does_not_describe_is_refused() {
        let bytes = std::fs::read(toy::fixture(&workspace_root())).unwrap();
        let m = wasm::Module::parse(&bytes).unwrap();

        let stripped = m.rewrite(&[aterm_wasm_abi::descriptor::SECTION], &[]);
        assert!(refusal(&stripped).contains("no `aterm_abi` custom section"));

        let mut old = bytes.clone();
        let at = old
            .windows(5)
            .position(|w| {
                w[..4] == aterm_wasm_abi::descriptor::MAGIC
                    && w[4] == aterm_wasm_abi::descriptor::VERSION
            })
            .unwrap();
        old[at + 4] += 1;
        assert!(refusal(&old).contains("descriptor version"));

        let missing = m.rewrite(&[], &["echo_u8"]);
        assert!(refusal(&missing).contains("names export `echo_u8`, which the module lacks"));
        let no_alloc = m.rewrite(&[], &["aterm_alloc"]);
        assert!(refusal(&no_alloc).contains("does not export `aterm_alloc`"));
    }

    /// The fixture with its descriptor section rebuilt from the records
    /// `keep` accepts.
    fn with_records(keep: impl Fn(&aterm_wasm_abi::descriptor::Record) -> bool) -> Vec<u8> {
        use aterm_wasm_abi::descriptor::{self as d, HEADER_LEN};
        let bytes = std::fs::read(toy::fixture(&workspace_root())).unwrap();
        let m = wasm::Module::parse(&bytes).unwrap();
        let mut kept: Vec<u8> = Vec::new();
        for section in m.custom(d::SECTION) {
            let mut at = 0;
            while at < section.len() {
                let len = u32::from_le_bytes(section[at + 5..at + 9].try_into().unwrap());
                let record = &section[at..at + HEADER_LEN + len as usize];
                if keep(&d::decode(record).unwrap()[0]) {
                    kept.extend(record);
                }
                at += record.len();
            }
        }
        let mut payload = Vec::new();
        wasm::leb(&mut payload, d::SECTION.len() as u32);
        payload.extend(d::SECTION.as_bytes());
        payload.extend(kept);
        let mut out = m.rewrite(&[d::SECTION], &[]);
        out.push(0);
        wasm::leb(&mut out, payload.len() as u32);
        out.extend(payload);
        out
    }

    /// Change a length-prefixed name in one real descriptor without changing
    /// its export's signature or code. Everything outside that record stays
    /// byte-for-byte the fixture's.
    fn with_renamed_record(symbol: &str, renames: &[(&str, &str)]) -> Vec<u8> {
        use aterm_wasm_abi::descriptor::{self as d, HEADER_LEN};
        let bytes = std::fs::read(toy::fixture(&workspace_root())).unwrap();
        let m = wasm::Module::parse(&bytes).unwrap();
        let mut records = Vec::new();
        let mut changed = false;
        for section in m.custom(d::SECTION) {
            let mut at = 0;
            while at < section.len() {
                let len = u32::from_le_bytes(section[at + 5..at + 9].try_into().unwrap());
                let raw = &section[at..at + HEADER_LEN + len as usize];
                if symbol == "*" || d::decode(raw).unwrap()[0].symbol == symbol {
                    let mut body = raw[HEADER_LEN..].to_vec();
                    for (old, new) in renames {
                        let mut before = (old.len() as u32).to_le_bytes().to_vec();
                        before.extend(old.as_bytes());
                        let mut after = (new.len() as u32).to_le_bytes().to_vec();
                        after.extend(new.as_bytes());
                        let hits: Vec<_> = body
                            .windows(before.len())
                            .enumerate()
                            .filter_map(|(i, word)| (word == before).then_some(i))
                            .collect();
                        if symbol == "*" {
                            for i in hits.into_iter().rev() {
                                body.splice(i..i + before.len(), after.iter().copied());
                            }
                            // A class reference in a type has a one-byte
                            // name length, unlike the record's string fields.
                            let mut before = vec![u8::try_from(old.len()).unwrap()];
                            before.extend(old.as_bytes());
                            let mut after = vec![u8::try_from(new.len()).unwrap()];
                            after.extend(new.as_bytes());
                            while let Some(i) = body.windows(before.len()).position(|s| s == before)
                            {
                                body.splice(i..i + before.len(), after.iter().copied());
                            }
                        } else {
                            assert!(!hits.is_empty(), "name {old:?} was not in {symbol}");
                            let i = hits[0];
                            body.splice(i..i + before.len(), after.iter().copied());
                        }
                    }
                    records.extend(d::MAGIC);
                    records.push(d::VERSION);
                    records.extend((body.len() as u32).to_le_bytes());
                    records.extend(body);
                    changed = true;
                } else {
                    records.extend(raw);
                }
                at += raw.len();
            }
        }
        assert!(changed, "no record for {symbol}");
        let mut payload = Vec::new();
        wasm::leb(&mut payload, d::SECTION.len() as u32);
        payload.extend(d::SECTION.as_bytes());
        payload.extend(records);
        let mut out = m.rewrite(&[d::SECTION], &[]);
        out.push(0);
        wasm::leb(&mut out, payload.len() as u32);
        out.extend(payload);
        out
    }

    /// Metadata is not JavaScript source: malformed identifiers must fail
    /// before the output directory is created, even when the wasm signature
    /// is genuine and unchanged.
    #[test]
    fn descriptor_names_cannot_inject_javascript_or_shadow_its_runtime() {
        use aterm_tempfile::TempDir;
        let valid = std::fs::read(toy::fixture(&workspace_root())).unwrap();
        assert!(glue(&valid, toy::STEM, Flavor::Web).is_ok());
        let cases = [
            ("echo_string", "echo_string", "echo-string"),
            ("utf8_len", "text", "v) { globalThis.injected = 1; } //"),
            ("counter_note", "note", "x() {} static injected() {} x"),
            ("echo_string", "echo_string", "TextDecoder"),
        ];
        let scratch = TempDir::new().unwrap();
        let mut accepted = Vec::new();
        for (i, (symbol, old, new)) in cases.into_iter().enumerate() {
            let bytes = with_renamed_record(symbol, &[(old, new)]);
            let input = scratch.path().join(format!("case{i}.wasm"));
            let output = scratch.path().join(format!("out{i}"));
            std::fs::write(&input, bytes).unwrap();
            if bind(&input, &output, Flavor::Web).is_ok() || output.exists() {
                accepted.push(new);
            }
        }
        assert!(accepted.is_empty(), "unsafe names accepted: {accepted:?}");
    }

    /// Class names come from Rust structs, with no js_name override. A
    /// forged class must not enter the generator's private `$` locals.
    #[test]
    fn a_class_cannot_shadow_generated_argument_or_result_bindings() {
        let bytes = std::fs::read(toy::fixture(&workspace_root())).unwrap();
        run_bound_node(
            &bytes,
            "normal_classes",
            "const c = new m.Counter(2n); assert.equal(c.value, 2n); c.free();",
        );
        for name in ["arg$0", "ret$", "ptr$0", "len$0", "char$0"] {
            let renamed = with_renamed_record("*", &[("Counter", name)]);
            assert!(
                glue(&renamed, "classes", Flavor::Web).is_err(),
                "class {name} was admitted into the wrapper-local namespace"
            );
        }
    }

    fn run_bound_node_flavor(bytes: &[u8], stem: &str, flavor: Flavor, check: &str) {
        let scratch = aterm_tempfile::TempDir::new().unwrap();
        for (name, contents) in glue(bytes, stem, flavor).unwrap() {
            std::fs::write(scratch.path().join(name), contents).unwrap();
        }
        let module = scratch.path().join(format!("{stem}.js"));
        let mut script = String::from("const assert = require('node:assert/strict'); ");
        match flavor {
            Flavor::Nodejs => {
                script.push_str(&format!("const m = require(process.argv[1]); {check}"))
            }
            Flavor::Web => {
                std::fs::write(scratch.path().join("package.json"), r#"{"type":"module"}"#)
                    .unwrap();
                script.push_str(&format!(
                    "const fs = require('node:fs'); const u = require('node:url'); \
                     globalThis.fetch = async (url) => new Response(fs.readFileSync(url), \
                     {{ headers: {{ 'Content-Type': 'application/wasm' }} }}); \
                     (async () => {{ const m = await import(u.pathToFileURL(process.argv[1])); \
                     await m.default(); {check} }})().catch(e => {{ console.error(e); process.exitCode = 1; }});"
                ));
            }
        }
        // A machine without node cannot run the glue: that is the machine's
        // answer, never a finding, so it panics with verify's COULD NOT RUN
        // sentinel.
        let result = aterm_uds::spawnfd::command("node")
            .args(["-e", &script])
            .arg(module)
            .output()
            .unwrap_or_else(|e| {
                panic!(
                    "{}: node could not start ({e}); the generated glue runs under node; {}",
                    crate::libtest::COULD_NOT_RUN_SENTINEL,
                    aterm_uds::host::tool("node").remedy()
                )
            });
        assert!(
            result.status.success(),
            "generated glue failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }

    fn run_bound_node(bytes: &[u8], stem: &str, check: &str) {
        run_bound_node_flavor(bytes, stem, Flavor::Nodejs, check);
    }

    /// Legal Rust names can escape to the same JS name or shadow a helper,
    /// a host global or a class the wrapper itself reads. Drive the real
    /// wasm, rather than pinning the spelling of the generator's chosen
    /// aliases.
    #[test]
    fn colliding_parameter_names_keep_their_values_in_generated_glue() {
        run_bound_node(
            &with_renamed_record("reserved", &[("function", "wasm"), ("this", "_wasm")]),
            "params",
            "assert.equal(m.reserved(1, 2), 12);",
        );
        run_bound_node(
            &with_renamed_record("reserved", &[("this", "_this"), ("function", "this")]),
            "params",
            "assert.equal(m.reserved(1, 2), 12);",
        );
        run_bound_node(
            &with_renamed_record("echo_opt_u64", &[("v", "BigInt")]),
            "params",
            "assert.equal(m.echo_opt_u64(5n), 5n); assert.equal(m.echo_opt_u64(), undefined);",
        );
        run_bound_node(
            &with_renamed_record("counter_find", &[("needle", "Snapshot")]),
            "params",
            "const c = new m.Counter(3n); c.note('abc'); \
             assert.equal(c.find('b').value, 3n); assert.equal(c.find('z'), undefined);",
        );
        run_bound_node(
            &with_renamed_record("utf8_len", &[("text", "passStringToWasm0")]),
            "params",
            "assert.equal(m.utf8_len('😀'), 4);",
        );
        run_bound_node(
            &with_renamed_record("echo_str", &[("v", "WASM_VECTOR_LEN")]),
            "params",
            "assert.equal(m.echo_str('text'), 'text');",
        );
    }

    /// A filename is data in the loader, including quote/template/comment
    /// delimiters. It must load the sibling module with that exact name.
    #[test]
    fn a_quoted_module_filename_is_loaded_literally() {
        let bytes = std::fs::read(toy::fixture(&workspace_root())).unwrap();
        for flavor in [Flavor::Nodejs, Flavor::Web] {
            run_bound_node_flavor(
                &bytes,
                "quote'`${globalThis.injected = 1}#?%",
                flavor,
                "assert.equal(m.echo_u8(7), 7); assert.equal(globalThis.injected, undefined);",
            );
        }
    }

    /// An export no record describes would vanish from the glue: refused.
    /// The same surgery keeping every record binds, so the refusal is the
    /// missing record's.
    #[test]
    fn an_export_without_a_record_is_refused() {
        assert!(glue(&with_records(|_| true), toy::STEM, Flavor::Web).is_ok());
        let e = refusal(&with_records(|r| r.symbol != "echo_u16s"));
        assert!(
            e.contains("export `echo_u16s` has no descriptor record"),
            "{e}"
        );
    }

    /// A record whose lowering is not the export's signature, and an import
    /// the glue does not know, on a module built by hand.
    #[test]
    fn signatures_and_imports_are_checked_against_the_module() {
        use aterm_wasm_abi::descriptor::{MAGIC, VERSION, kind, receiver, tag};
        fn record(name: &str, ret: u8) -> Vec<u8> {
            let mut body = vec![kind::FUNCTION];
            for s in ["", name, name, ""] {
                body.extend((s.len() as u32).to_le_bytes());
                body.extend(s.as_bytes());
            }
            body.extend([receiver::NONE, 0, ret]);
            let mut r = MAGIC.to_vec();
            r.push(VERSION);
            r.extend((body.len() as u32).to_le_bytes());
            r.extend(body);
            r
        }
        // `g` is (i32, i64) -> f64; describe it as `fn g() -> u32`.
        let wrong = wasm::tests::sample(&[("aterm_abi", &record("g", tag::U32))]);
        let e = refusal(&wrong);
        assert!(
            e.contains("export `g` has signature (i32, i64) -> (f64)"),
            "{e}"
        );
        // `h` is () -> (): right, so the next check speaks: the import.
        let foreign = wasm::tests::sample(&[("aterm_abi", &record("h", tag::UNIT))]);
        let e = refusal(&foreign);
        assert!(e.contains("imports `env.f`"), "{e}");
    }
}
