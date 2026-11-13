// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `verify glue`: the ES module and TypeScript glue generator for `aterm_abi` wasm modules.

use std::path::{Path, PathBuf};

mod js;
mod model;
mod templates;
pub(crate) mod toy;
mod wasm;
mod webgl;

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
    let bytes = crate::read_regular(input)
        .map_err(|e| format!("could not read {}: {e}", input.display()))?;
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
        crate::write_fresh(&path, contents)
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
#[path = "mod_tests.rs"]
mod tests;
