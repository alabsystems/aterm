// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//! Native Trust driver for the independent pinned Windows reference workspaces.
mod emit;
mod facade052;
mod model;
mod oracle;
mod plants;
mod source;
use aterm_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{ExitCode, Output};

type Result<T> = std::result::Result<T, String>;
const VERSION: &str = "0.61.2";
const TRIPLES: [&str; 2] = ["x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc"];
/// The generator each emitted Rust file's seal line names ([`aterm_census::seal`]).
const GENERATOR: &str = "`aterm-win32-oracle gen`";
const HELP: &str = "\
aterm-win32-oracle — generated declaration closure and independent Windows ABI oracle

usage: targo --unverified run -p aterm-win32-oracle -- <gen|check|record|oracle|oracle-052> [options]

  --root <checkout>     checkout holding win32-oracle/ and crates/aterm-win32/ (default: the nearest such
                        ancestor of cwd)
  --upstream <dir>      not oracle-052: pinned registry windows-sys 0.61.2 source (default: $WINDOWS_SYS_SRC,
                        else the Cargo registry, fetched when missing)
  --out <dir>           gen only: write into a checkout-shaped output tree
  --target-dir <dir>    record/oracle/oracle-052 build dir (default: $ORACLE_TARGET_DIR, else
                        <root>/win32-oracle/target.noindex; for oracle-052,
                        <root>/crates/aterm-windows-sys-052/tests/oracle/target.noindex)
  --stock <toolchain>   oracle/oracle-052 only: stock rustup toolchain (default: stable)
  --no-format           gen only: skip trustfmt on emitted Rust

gen writes crates/aterm-win32, crates/aterm-windows-sys's feature list and the win32-oracle
conformance and recorder crates from the pinned registry source and Cargo's consumer/feature
graph; check regenerates into scratch and reports DRIFT, never writing; record rewrites
win32-oracle/recorded.json from the host's answers, then runs gen; oracle compiles the
conformance crate, compares the facade's import tables with upstream's and requires every
planted fault to be caught, on both Windows triples; oracle-052 holds
crates/aterm-windows-sys-052 to registry windows-sys 0.52.0. The reference workspaces have no
[patch] table. The driver, graph and host recorder use Trust; oracle and oracle-052 are a STOCK
EXCEPTION: stock Cargo/RUSTC compile Windows std, and no Windows program runs.
`targo --unverified verify cells` runs both with a Windows cell.
Exit: 0 pass, 1 finding, 2 usage, 3 could not run; no proof or Windows-runtime claim.
";
#[derive(Clone)]
struct Tools {
    targo: PathBuf,
    trustfmt: PathBuf,
    stock: Option<(PathBuf, PathBuf)>,
    nm: PathBuf,
}
fn read(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}
fn which(name: &str) -> Result<PathBuf> {
    if Path::new(name).is_absolute() {
        return Ok(name.into());
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|p| p.join(name))
        .find(|p| p.is_file())
        .and_then(|p| p.canonicalize().ok())
        .ok_or_else(|| format!("{name} absent from PATH"))
}
fn run_status(program: &Path, args: &[String], cwd: &Path, rustc: Option<&Path>) -> Result<Output> {
    let mut command = aterm_uds::spawnfd::command(program);
    command.args(args).current_dir(cwd);
    if let Some(rustc) = rustc {
        command.env("RUSTC", rustc);
    }
    command
        .output()
        .map_err(|e| format!("{} could not start: {e}", program.display()))
}
fn run(program: &Path, args: &[&str], cwd: &Path, rustc: Option<&Path>) -> Result<Output> {
    let args: Vec<_> = args.iter().map(|s| (*s).to_string()).collect();
    let out = run_status(program, &args, cwd, rustc)?;
    if !out.status.success() {
        return Err(format!(
            "{} {:?} failed:\n{}{}",
            program.display(),
            args,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(out)
}
fn find_root(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|p| {
            p.join("win32-oracle/Cargo.toml").is_file()
                && p.join("crates/aterm-win32/Cargo.toml").is_file()
        })
        .map(Path::to_path_buf)
}
fn upstream() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("WINDOWS_SYS_SRC") {
        return Ok(p.into());
    }
    let home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cargo")))
        .ok_or("no Cargo registry location")?;
    let dir = home.join("registry/src");
    let mut hits = vec![];
    for e in
        std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}; use --upstream", dir.display()))?
    {
        let path = e
            .map_err(|e| e.to_string())?
            .path()
            .join(format!("windows-sys-{VERSION}"));
        if path.join("Cargo.toml").is_file() {
            hits.push(path)
        }
    }
    hits.sort();
    hits.into_iter()
        .next()
        .ok_or("no cached windows-sys 0.61.2; use --upstream".into())
}
/// Populate `$CARGO_HOME/registry/src/…/windows-sys-0.61.2` via `win32-oracle/Cargo.toml`.
fn fetch_reference(root: &Path) -> Result<()> {
    let manifest = root.join("win32-oracle/Cargo.toml");
    let manifest = manifest.to_str().ok_or("checkout path is not UTF-8")?;
    run(
        &which("targo")?,
        &["fetch", "--locked", "--manifest-path", manifest],
        root,
        None,
    )
    .map(drop)
}
/// Locate `rustup` on `PATH`, `$CARGO_HOME/bin`, or `~/.cargo/bin`.
fn rustup() -> Result<PathBuf> {
    which("rustup").or_else(|missing| {
        std::env::var_os("CARGO_HOME")
            .map(PathBuf::from)
            .into_iter()
            .chain(std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cargo")))
            .map(|home| home.join("bin/rustup"))
            .find(|p| p.is_file())
            .ok_or(missing)
    })
}
fn tools(root: &Path, stock: Option<&str>) -> Result<Tools> {
    for name in [
        "RUSTFLAGS",
        "RUSTDOCFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
        "CARGO_ENCODED_RUSTDOCFLAGS",
        "CARGO_BUILD_TARGET",
    ] {
        if std::env::var_os(name).is_some() {
            return Err(format!(
                "ambient {name} would replace the declared compiler/config cell"
            ));
        }
    }
    let mut out = Tools {
        targo: which("targo")?,
        trustfmt: which("trustfmt")?,
        stock: None,
        nm: PathBuf::from("nm"),
    };
    if let Some(name) = stock {
        let rustup = rustup()?;
        let mut paths = vec![];
        for driver in ["cargo", "rustc"] {
            let answer = run(&rustup, &["which", "--toolchain", name, driver], root, None)?;
            let p = PathBuf::from(
                String::from_utf8(answer.stdout)
                    .map_err(|e| e.to_string())?
                    .trim(),
            );
            if !p.is_absolute() || !p.is_file() {
                return Err(format!(
                    "stock {driver} did not resolve to an absolute executable"
                ));
            }
            paths.push(p);
        }
        out.stock = Some((paths.remove(0), paths.remove(0)));
    }
    out.nm = match std::env::var_os("NM") {
        Some(nm) => PathBuf::from(nm),
        None => which("llvm-nm")
            .ok()
            .or_else(|| {
                out.stock
                    .as_ref()
                    .and_then(|(_, rustc)| toolchain_llvm_nm(rustc))
            })
            .or_else(|| which("nm").ok())
            .unwrap_or_else(|| PathBuf::from("nm")),
    };
    Ok(out)
}

/// Locate `<sysroot>/lib/rustlib/<host>/bin/llvm-nm` from the stock toolchain's `llvm-tools`.
fn toolchain_llvm_nm(rustc: &Path) -> Option<PathBuf> {
    let rustlib = rustc.parent()?.parent()?.join("lib").join("rustlib");
    let name = if cfg!(windows) {
        "llvm-nm.exe"
    } else {
        "llvm-nm"
    };
    let mut found: Vec<PathBuf> = std::fs::read_dir(rustlib)
        .ok()?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path().join("bin").join(name))
        .filter(|candidate| candidate.is_file())
        .collect();
    found.sort();
    found.into_iter().next()
}
fn formatted(
    root: &Path,
    tools: &Tools,
    outputs: &BTreeMap<PathBuf, String>,
    out: &Path,
    format: bool,
) -> Result<()> {
    let mut paths = vec![];
    for (rel, text) in outputs {
        let p = out.join(rel);
        std::fs::create_dir_all(p.parent().ok_or("output has no parent")?)
            .map_err(|e| e.to_string())?;
        if p.extension().is_some_and(|e| e == "rs") {
            std::fs::write(&p, with_seal_line(rel, text)?).map_err(|e| e.to_string())?;
            paths.push(p)
        } else {
            std::fs::write(&p, text).map_err(|e| e.to_string())?;
        }
    }
    if format && !paths.is_empty() {
        let mut args = vec![
            "--edition".into(),
            "2024".into(),
            "--config".into(),
            "skip_children=true".into(),
        ];
        args.extend(paths.iter().map(|p| p.to_string_lossy().into_owned()));
        let answer = run_status(&tools.trustfmt, &args, root, None)?;
        if !answer.status.success() {
            return Err(format!(
                "trustfmt failed: {}{}",
                String::from_utf8_lossy(&answer.stdout),
                String::from_utf8_lossy(&answer.stderr)
            ));
        }
    }
    for p in &paths {
        let sealed =
            aterm_census::seal::stamp(&read(p)?).map_err(|e| format!("{}: {e}", p.display()))?;
        std::fs::write(p, sealed).map_err(|e| e.to_string())?;
    }
    Ok(())
}
/// `text` with the unstamped seal line ([`aterm_census::seal::line`]) after its SPDX line;
/// [`formatted`] stamps it once the file is in its final form.
fn with_seal_line(rel: &Path, text: &str) -> Result<String> {
    let at = text
        .find("SPDX-License-Identifier:")
        .and_then(|spdx| text[spdx..].find('\n').map(|end| spdx + end + 1))
        .ok_or_else(|| format!("{}: no SPDX line to put the seal under", rel.display()))?;
    Ok(format!(
        "{}{}\n{}",
        &text[..at],
        aterm_census::seal::line("//", GENERATOR),
        &text[at..]
    ))
}
/// Returns whether `path` is a hand-written file under `crates/aterm-win32/src`.
fn hand_written(root: &Path, path: &Path) -> bool {
    let src = root.join("crates/aterm-win32/src");
    ["lib.rs"]
        .iter()
        .chain(model::HAND_WRITTEN.iter())
        .any(|f| path == src.join(f))
}
fn generate(
    root: &Path,
    model: &model::Model,
    tools: &Tools,
    out: &Path,
    format: bool,
) -> Result<()> {
    let outputs = model.outputs(root)?;
    if out == root {
        for path in model::rust_files(&root.join("crates/aterm-win32/src"))? {
            let rel = path.strip_prefix(root).map_err(|e| e.to_string())?;
            if !hand_written(root, &path) && !outputs.contains_key(rel) {
                std::fs::remove_file(&path).map_err(|e| e.to_string())?;
            }
        }
    }
    formatted(root, tools, &outputs, out, format)
}
fn check(root: &Path, model: &model::Model, tools: &Tools) -> Result<u8> {
    let outputs = model.outputs(root)?;
    let tmp = aterm_tempfile::tempdir().map_err(|e| e.to_string())?;
    formatted(root, tools, &outputs, tmp.path(), true)?;
    let mut drift = vec![];
    for path in outputs.keys() {
        let want = read(&tmp.path().join(path))?;
        if read(&root.join(path)).ok().as_deref() != Some(&want) {
            drift.push(path.display().to_string())
        }
    }
    for p in model::rust_files(&root.join("crates/aterm-win32/src"))? {
        let rel = p.strip_prefix(root).map_err(|e| e.to_string())?;
        if !hand_written(root, &p) && !outputs.contains_key(rel) {
            drift.push(format!("{} (no longer generated)", rel.display()))
        }
    }
    if drift.is_empty() {
        println!("check: generated files match the harvest");
        Ok(0)
    } else {
        for p in drift {
            println!("DRIFT {p}")
        }
        Ok(1)
    }
}
fn parse_record(tsv: &str) -> Result<Value> {
    let mut items = aterm_json::Map::new();
    for line in tsv.lines() {
        let parts: Vec<_> = line.split('\t').collect();
        let key = parts.get(1).ok_or("record row lacks key")?;
        match parts.first().copied() {
            Some("layout") if parts.len() == 4 => {
                let size = parts[2].parse::<u64>().map_err(|e| e.to_string())?;
                let align = parts[3].parse::<u64>().map_err(|e| e.to_string())?;
                if items
                    .insert(
                        (*key).into(),
                        aterm_json::json!({"size":size,"align":align,"offsets":{}}),
                    )
                    .is_some()
                {
                    return Err("duplicate layout record".into());
                }
            }
            Some("offset") if parts.len() == 4 => {
                let offsets = items
                    .get_mut(*key)
                    .and_then(Value::as_object_mut)
                    .and_then(|v| v.get_mut("offsets"))
                    .and_then(Value::as_object_mut)
                    .ok_or("offset precedes layout")?;
                let offset = parts[3].parse::<u64>().map_err(|e| e.to_string())?;
                if offsets
                    .insert(parts[2].into(), Value::from(offset))
                    .is_some()
                {
                    return Err("duplicate offset".into());
                }
            }
            Some("value" | "cstr") if parts.len() == 3 => {
                let k = parts[0];
                if items
                    .insert((*key).into(), aterm_json::json!({k:parts[2]}))
                    .is_some()
                {
                    return Err("duplicate recorded value".into());
                }
            }
            _ => return Err(format!("unparsed record row {line:?}")),
        }
    }
    Ok(Value::Object(items))
}
fn record(root: &Path, model: &model::Model, tools: &Tools, target: &Path) -> Result<()> {
    formatted(root, tools, &model.recorder_outputs(root)?, root, true)?;
    let target = target.to_str().ok_or("non-UTF8 target path")?;
    let answer = run(
        &tools.targo,
        &[
            "--unverified",
            "run",
            "--locked",
            "--offline",
            "-q",
            "-p",
            "aterm-win32-record",
            "--target-dir",
            target,
        ],
        &root.join("win32-oracle"),
        None,
    )?;
    let version = run(&which("trustc")?, &["-vV"], root, None)?;
    let host = String::from_utf8(version.stdout)
        .map_err(|e| e.to_string())?
        .lines()
        .find_map(|l| l.strip_prefix("host: "))
        .ok_or("trustc reports no host")?
        .to_string();
    let data = aterm_json::json!({"windows-sys":VERSION,"host":host,"items":parse_record(&String::from_utf8(answer.stdout).map_err(|e|e.to_string())?)?});
    emit::recorded_tests(&model.up, &model.variants(), &data)?;
    let text = pretty_json(&data)?;
    std::fs::write(root.join("win32-oracle/recorded.json"), text + "\n")
        .map_err(|e| e.to_string())?;
    generate(root, model, tools, root, true)
}

fn pretty_json(value: &Value) -> Result<String> {
    fn write(value: &Value, depth: usize, out: &mut String) -> Result<()> {
        let pad = " ".repeat(depth + 1);
        let close_pad = " ".repeat(depth);
        match value {
            Value::Object(fields) if !fields.is_empty() => {
                out.push_str("{\n");
                for (i, (key, value)) in fields.iter().enumerate() {
                    out.push_str(&pad);
                    out.push_str(&aterm_json::to_string(key).map_err(|e| e.to_string())?);
                    out.push_str(": ");
                    write(value, depth + 1, out)?;
                    if i + 1 != fields.len() {
                        out.push(',');
                    }
                    out.push('\n');
                }
                out.push_str(&close_pad);
                out.push('}');
            }
            Value::Array(items) if !items.is_empty() => {
                out.push_str("[\n");
                for (i, value) in items.iter().enumerate() {
                    out.push_str(&pad);
                    write(value, depth + 1, out)?;
                    if i + 1 != items.len() {
                        out.push(',');
                    }
                    out.push('\n');
                }
                out.push_str(&close_pad);
                out.push(']');
            }
            _ => out.push_str(&aterm_json::to_string(value).map_err(|e| e.to_string())?),
        }
        Ok(())
    }
    let mut out = String::new();
    write(value, 0, &mut out)?;
    Ok(out)
}

fn start() -> Result<u8> {
    let mut args = std::env::args().skip(1);
    let Some(verb) = args.next() else {
        print!("{HELP}");
        return Ok(2);
    };
    if ["-h", "--help"].contains(&verb.as_str()) {
        print!("{HELP}");
        return Ok(0);
    }
    if !["gen", "check", "record", "oracle", "oracle-052"].contains(&verb.as_str()) {
        eprintln!("unknown verb {verb}\n{HELP}");
        return Ok(2);
    }
    let mut root = None;
    let mut up = None;
    let mut out = None;
    let mut target = None;
    let mut stock = "stable".to_string();
    let mut format = true;
    let options = (|| -> Result<bool> {
        while let Some(arg) = args.next() {
            let value = |args: &mut std::iter::Skip<std::env::Args>| {
                args.next().ok_or_else(|| format!("{arg} needs a value"))
            };
            match arg.as_str() {
                "--root" => root = Some(PathBuf::from(value(&mut args)?)),
                "--upstream" if verb != "oracle-052" => {
                    up = Some(PathBuf::from(value(&mut args)?));
                }
                "--out" if verb == "gen" => out = Some(PathBuf::from(value(&mut args)?)),
                "--target-dir" => target = Some(PathBuf::from(value(&mut args)?)),
                "--stock" if verb.starts_with("oracle") => stock = value(&mut args)?,
                "--no-format" if verb == "gen" => format = false,
                "--help" | "-h" => {
                    return Ok(true);
                }
                _ => return Err(format!("unknown argument {arg}")),
            }
        }
        Ok(false)
    })();
    match options {
        Ok(true) => {
            print!("{HELP}");
            return Ok(0);
        }
        Ok(false) => {}
        Err(error) => {
            eprintln!("{error}\n{HELP}");
            return Ok(2);
        }
    }
    let root = root
        .or_else(|| std::env::current_dir().ok().and_then(|p| find_root(&p)))
        .ok_or("cannot locate checkout; use --root")?
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let target = target.or_else(|| std::env::var_os("ORACLE_TARGET_DIR").map(PathBuf::from));
    if verb == "oracle-052" {
        let tools = tools(&root, Some(stock.as_str()))?;
        let target =
            target.unwrap_or_else(|| root.join(facade052::WORKSPACE).join("target.noindex"));
        return facade052::gate(&root, &tools, &target);
    }
    let up = match up {
        Some(p) => p,
        None => match upstream() {
            Ok(p) => p,
            Err(_) => {
                fetch_reference(&root)?;
                upstream()?
            }
        },
    };
    let manifest: aterm_toml::Value =
        aterm_toml::from_str(&read(&up.join("Cargo.toml"))?).map_err(|e| e.to_string())?;
    if manifest
        .get("package")
        .and_then(|p| p.get("name"))
        .and_then(aterm_toml::Value::as_str)
        != Some("windows-sys")
        || manifest
            .get("package")
            .and_then(|p| p.get("version"))
            .and_then(aterm_toml::Value::as_str)
            != Some(VERSION)
    {
        return Err("upstream source is not pinned windows-sys 0.61.2".into());
    }
    let tools = tools(&root, (verb == "oracle").then_some(stock.as_str()))?;
    let model = model::Model::load(&root, &up, &tools)?;
    println!("{}", model.summary());
    for note in &model.notes {
        println!("note: {note}")
    }
    let target = target.unwrap_or_else(|| root.join("win32-oracle/target.noindex"));
    match verb.as_str() {
        "gen" => {
            generate(
                &root,
                &model,
                &tools,
                out.as_deref().unwrap_or(&root),
                format,
            )?;
            Ok(0)
        }
        "check" => check(&root, &model, &tools),
        "record" => {
            record(&root, &model, &tools, &target)?;
            Ok(0)
        }
        "oracle" => oracle::gate(&root, &model, &tools, &target),
        _ => unreachable!(),
    }
}
fn main() -> ExitCode {
    match start() {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("win32 oracle could not run: {error}");
            ExitCode::from(3)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recorded_stream_is_strict_and_keeps_all_answer_kinds() {
        let r =
            parse_record("layout\tA\t8\t4\noffset\tA\tx\t0\nvalue\tB\t-1\ncstr\tC\t1.2\n").unwrap();
        assert_eq!(r["A"]["offsets"]["x"].as_u64(), Some(0));
        assert_eq!(r["B"]["value"].as_str(), Some("-1"));
        assert_eq!(r["C"]["cstr"].as_str(), Some("1.2"));
        for bad in ["garbage", "offset\tA\tx\t0", "value\tB\t1\nvalue\tB\t2"] {
            assert!(parse_record(bad).is_err(), "{bad}")
        }
    }

    #[test]
    fn the_stock_toolchains_llvm_nm_is_its_llvm_tools_one() {
        let sysroot = aterm_tempfile::tempdir().expect("scratch sysroot");
        let rustc = sysroot.path().join("bin").join("rustc");
        std::fs::create_dir_all(rustc.parent().unwrap()).unwrap();
        std::fs::write(&rustc, b"").unwrap();
        let rustlib = sysroot.path().join("lib").join("rustlib");
        std::fs::create_dir_all(rustlib.join("x86_64-pc-windows-msvc").join("lib")).unwrap();
        assert_eq!(toolchain_llvm_nm(&rustc), None, "no llvm-tools component");
        let name = if cfg!(windows) {
            "llvm-nm.exe"
        } else {
            "llvm-nm"
        };
        let host_bin = rustlib.join("aarch64-unknown-linux-gnu").join("bin");
        std::fs::create_dir_all(&host_bin).unwrap();
        std::fs::write(host_bin.join(name), b"").unwrap();
        assert_eq!(toolchain_llvm_nm(&rustc), Some(host_bin.join(name)));
    }
}
