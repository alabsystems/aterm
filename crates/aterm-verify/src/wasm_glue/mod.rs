// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `verify glue`: the ES module and TypeScript glue generator for `aterm_abi` wasm modules.

use std::path::{Path, PathBuf};

mod js;
mod model;
pub(crate) mod oracle;
mod templates;
pub(crate) mod toy;
mod wasm;
mod webgl;

/// What `verify provision --record` lays for the `wasm-glue` check's oracle row.
pub use oracle::{BINDGEN, BINDGEN_INSTALL, bindgen_root, reports_pin, tooling};

/// The pass the shipping pipeline applies to a bound `<stem>_bg.wasm`, never before binding.
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
/// A stem that is not one filename component, or a module the generator refuses.
pub fn glue(bytes: &[u8], stem: &str) -> Result<Vec<(String, Vec<u8>)>, String> {
    if stem.is_empty() || Path::new(stem).file_name().and_then(|s| s.to_str()) != Some(stem) {
        return Err("the module stem must be one filename component".to_string());
    }
    let module = wasm::Module::parse(bytes)?;
    let iface = model::interface(&module)?;
    let g = js::generate(&iface, stem);
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
pub(crate) fn bind(input: &Path, out_dir: &Path) -> Result<Vec<PathBuf>, String> {
    let bytes =
        std::fs::read(input).map_err(|e| format!("could not read {}: {e}", input.display()))?;
    let stem = input
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| format!("{}: no file name", input.display()))?;
    let files = glue(&bytes, stem).map_err(|e| format!("{}: {e}", input.display()))?;
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

const USAGE: &str = "usage: targo --unverified verify glue <in.wasm> <out-dir> [--flavor web]\n\
       targo --unverified verify glue --record-toy      (re-record the toy's goldens and fixture)\n\
       targo --unverified verify glue --record-replay   (re-record the CPU module's boundary answers)";

/// `verify glue …`, answering its exit code.
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
            crate::perf::Status::Skip { why, fix }
            | crate::perf::Status::CouldNotRun { why, fix } => {
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
    let (input, out) = match parse(args) {
        Ok(paths) => paths,
        Err(why) => {
            if let Some(why) = why {
                eprintln!("glue: {why}");
            }
            eprintln!("{USAGE}");
            return crate::exit::USAGE;
        }
    };
    match bind(Path::new(input), Path::new(out)) {
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

/// The generator's own arguments: the input and the output dir.
fn parse(args: &[String]) -> Result<(&String, &String), Option<String>> {
    let mut positional = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--flavor" => match it.next().map(String::as_str) {
                Some("web") => {}
                Some("nodejs") => {
                    return Err(Some(
                        "the nodejs flavor is gone: the glue is an ES module, and its drivers \
                         run in a browser"
                            .to_string(),
                    ));
                }
                _ => return Err(None),
            },
            _ => positional.push(a),
        }
    }
    match positional.as_slice() {
        [input, out] => Ok((*input, *out)),
        _ => Err(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::perf::workspace_root;

    #[test]
    fn the_toy_module_binds_to_its_goldens() {
        let root = workspace_root();
        let bytes = std::fs::read(toy::fixture(&root)).unwrap();
        let files = glue(&bytes, toy::STEM).unwrap();
        assert_eq!(files.len(), 4);
        for (name, contents) in files.iter().filter(|(n, _)| toy::is_text(n)) {
            let path = toy::golden_dir(&root).join(name);
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

    #[test]
    fn the_cli_takes_web_and_refuses_the_nodejs_flavor() {
        let args = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        let plain = args(&["m.wasm", "out"]);
        assert_eq!(
            parse(&plain).map(|(i, o)| (i.as_str(), o.as_str())),
            Ok(("m.wasm", "out"))
        );
        let named = args(&["m.wasm", "--flavor", "web", "out"]);
        assert_eq!(
            parse(&named).map(|(i, o)| (i.as_str(), o.as_str())),
            Ok(("m.wasm", "out"))
        );
        let gone = args(&["m.wasm", "out", "--flavor", "nodejs"]);
        assert!(matches!(parse(&gone), Err(Some(why)) if why.contains("nodejs flavor is gone")));
        assert_eq!(
            parse(&args(&["m.wasm", "out", "--flavor", "cjs"])),
            Err(None)
        );
        assert_eq!(parse(&args(&["m.wasm"])), Err(None));
    }

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
        let files = glue(&bytes, toy::STEM).unwrap();
        let (_, bg) = files.iter().find(|(n, _)| n.ends_with("_bg.wasm")).unwrap();
        let names: Vec<String> = wasm::Module::parse(bg)
            .unwrap()
            .custom_names()
            .map(str::to_string)
            .collect();
        assert_eq!(names, ["producers"]);
    }

    fn refusal(bytes: &[u8]) -> String {
        glue(bytes, "m").expect_err("refused")
    }

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

    #[test]
    fn descriptor_names_cannot_inject_javascript_or_shadow_its_runtime() {
        use aterm_tempfile::TempDir;
        let valid = std::fs::read(toy::fixture(&workspace_root())).unwrap();
        assert!(glue(&valid, toy::STEM).is_ok());
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
            if bind(&input, &output).is_ok() || output.exists() {
                accepted.push(new);
            }
        }
        assert!(accepted.is_empty(), "unsafe names accepted: {accepted:?}");
    }

    /// Class names come from Rust structs, with no js_name override. A
    /// forged class must not enter the generator's private `$` locals.
    #[cfg(unix)]
    #[test]
    fn a_class_cannot_shadow_generated_argument_or_result_bindings() {
        let bytes = std::fs::read(toy::fixture(&workspace_root())).unwrap();
        run_bound(&[(
            bytes,
            "normal_classes",
            "const c = new m.Counter(2n); assert.equal(c.value, 2n); c.free();",
        )]);
        for name in ["arg$0", "ret$", "ptr$0", "len$0", "char$0"] {
            let renamed = with_renamed_record("*", &[("Counter", name)]);
            assert!(
                glue(&renamed, "classes").is_err(),
                "class {name} was admitted into the wrapper-local namespace"
            );
        }
    }

    /// Bind each `(module, stem, check)` into a directory of its own, then,
    /// in one browser, load each one's glue by its init and run its `check`
    /// (JavaScript with the module as `m` and a small `assert`): every check
    /// must complete without a throw.
    #[cfg(unix)]
    fn run_bound(cases: &[(Vec<u8>, &str, &str)]) {
        let scratch = aterm_tempfile::TempDir::new().unwrap();
        let mut modules = Vec::new();
        for (i, (bytes, stem, check)) in cases.iter().enumerate() {
            let dir = scratch.path().join(format!("case{i}"));
            std::fs::create_dir_all(&dir).unwrap();
            for (name, contents) in glue(bytes, stem).unwrap() {
                std::fs::write(dir.join(name), contents).unwrap();
            }
            let glue_url = format!("./{}", js::url_component(&format!("{stem}.js")));
            let module = format!(
                "const assert = {{ equal(got, want) {{ if (!Object.is(got, want)) \
                 throw new Error(`got ${{String(got)}}, want ${{String(want)}}`) }} }};\n\
                 export async function run() {{\n  const m = await import({});\n  \
                 await m.default();\n  {check}\n}}\n",
                aterm_json::to_string(&glue_url).unwrap()
            );
            std::fs::write(dir.join("check.mjs"), module).unwrap();
            modules.push(format!("case{i}/check.mjs"));
        }
        let mut page = crate::wasm_page::start_for_test(scratch.path());
        for (module, (_, stem, _)) in modules.iter().zip(cases) {
            if let Err(refused) = page.run(module, &aterm_json::json!({})) {
                panic!("generated glue for {stem:?} failed: {}", refused.why());
            }
        }
    }

    /// Legal Rust names can escape to the same JS name or shadow a helper,
    /// a host global or a class the wrapper itself reads. Drive the real
    /// wasm, rather than pinning the spelling of the generator's chosen
    /// aliases.
    #[cfg(unix)]
    #[test]
    fn colliding_parameter_names_keep_their_values_in_generated_glue() {
        run_bound(&[
            (
                with_renamed_record("reserved", &[("function", "wasm"), ("this", "_wasm")]),
                "params",
                "assert.equal(m.reserved(1, 2), 12);",
            ),
            (
                with_renamed_record("reserved", &[("this", "_this"), ("function", "this")]),
                "params",
                "assert.equal(m.reserved(1, 2), 12);",
            ),
            (
                with_renamed_record("echo_opt_u64", &[("v", "BigInt")]),
                "params",
                "assert.equal(m.echo_opt_u64(5n), 5n); assert.equal(m.echo_opt_u64(), undefined);",
            ),
            (
                with_renamed_record("counter_find", &[("needle", "Snapshot")]),
                "params",
                "const c = new m.Counter(3n); c.note('abc'); \
                 assert.equal(c.find('b').value, 3n); assert.equal(c.find('z'), undefined);",
            ),
            (
                with_renamed_record("utf8_len", &[("text", "passStringToWasm0")]),
                "params",
                "assert.equal(m.utf8_len('😀'), 4);",
            ),
            (
                with_renamed_record("echo_str", &[("v", "WASM_VECTOR_LEN")]),
                "params",
                "assert.equal(m.echo_str('text'), 'text');",
            ),
        ]);
    }

    /// A filename is data in the loader, including quote/template/comment
    /// delimiters. It must load the sibling module with that exact name.
    #[cfg(unix)]
    #[test]
    fn a_quoted_module_filename_is_loaded_literally() {
        let bytes = std::fs::read(toy::fixture(&workspace_root())).unwrap();
        run_bound(&[(
            bytes,
            "quote'`${globalThis.injected = 1}#?%",
            "assert.equal(m.echo_u8(7), 7); assert.equal(globalThis.injected, undefined);",
        )]);
    }

    /// NEGATIVE CONTROL for [`run_bound`]: a check that fails in the page
    /// fails the test, so a browser that ran nothing cannot pass one.
    #[cfg(unix)]
    #[test]
    #[should_panic(expected = "got 7, want 8")]
    fn a_failing_check_in_the_page_fails() {
        let bytes = std::fs::read(toy::fixture(&workspace_root())).unwrap();
        run_bound(&[(bytes, "control", "assert.equal(m.echo_u8(7), 8);")]);
    }

    #[test]
    fn an_export_without_a_record_is_refused() {
        assert!(glue(&with_records(|_| true), toy::STEM).is_ok());
        let e = refusal(&with_records(|r| r.symbol != "echo_u16s"));
        assert!(
            e.contains("export `echo_u16s` has no descriptor record"),
            "{e}"
        );
    }

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
        let wrong = wasm::tests::sample(&[("aterm_abi", &record("g", tag::U32))]);
        let e = refusal(&wrong);
        assert!(
            e.contains("export `g` has signature (i32, i64) -> (f64)"),
            "{e}"
        );
        let foreign = wasm::tests::sample(&[("aterm_abi", &record("h", tag::UNIT))]);
        let e = refusal(&foreign);
        assert!(e.contains("imports `env.f`"), "{e}");
    }
}
