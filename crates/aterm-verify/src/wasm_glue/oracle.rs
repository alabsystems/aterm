// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `wasm-glue` oracle lane holding toy goldens and `drive.mjs` against `wasm-bindgen` 0.2.108.
//! STOCK EXCEPTION: `rustup`'s `stable` carries the `wasm32` `std` that Trust lacks.

use aterm_uds::output::CommandOutput as _;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use aterm_uds::spawnfd::command;

use super::Flavor;
use super::toy::{STEM, flavor_name, golden_dir, toy_dir};
use crate::perf::{LaneRun, Status};

/// Pinned `wasm-bindgen` crate and CLI version.
pub const BINDGEN: &str = aterm_uds::host::pinned_version("wasm-bindgen");

/// The machine-wide tooling directory's name in aterm's user cache.
const TOOLING: &str = aterm_uds::host::TOOLING;

/// Machine-wide `tooling` cache directory under `home` and `xdg_cache`.
#[must_use]
pub fn tooling(home: Option<&Path>, xdg_cache: Option<&Path>) -> Option<PathBuf> {
    crate::user_cache(std::env::consts::OS, home, xdg_cache, TOOLING)
}

/// [`tooling`] for this process's `$HOME` and `$XDG_CACHE_HOME`.
#[must_use]
pub fn host_tooling() -> Option<PathBuf> {
    crate::host_user_cache(TOOLING)
}

/// Installation root for the pinned `wasm-bindgen` CLI under `tooling`.
#[must_use]
pub fn bindgen_root(tooling: &Path) -> PathBuf {
    tooling.join(format!("wasm-bindgen-{BINDGEN}"))
}

/// `targo --unverified` arguments to install the pinned `wasm-bindgen-cli`.
pub const BINDGEN_INSTALL: [&str; 6] = [
    "install",
    "wasm-bindgen-cli",
    "--version",
    BINDGEN,
    "--locked",
    "--force",
];

/// The version a `wasm-bindgen --version` stdout reports (`wasm-bindgen
/// 0.2.108`'s second field).
fn reported_version(stdout: &str) -> Option<&str> {
    stdout.lines().next()?.split_whitespace().nth(1)
}

/// Returns whether a `wasm-bindgen --version` stdout reports [`BINDGEN`] exactly.
#[must_use]
pub fn reports_pin(stdout: &str) -> bool {
    reported_version(stdout) == Some(BINDGEN)
}

/// Whether a successful `program --version`, with `PATH` set to `path_env`
/// when given, reports exactly the pin ([`reports_pin`]).
fn answers_pin(program: &Path, path_env: Option<&std::ffi::OsStr>) -> bool {
    let mut cmd = command(program);
    cmd.arg("--version");
    if let Some(p) = path_env {
        cmd.env("PATH", p);
    }
    cmd.output_owned()
        .is_ok_and(|o| o.status.success() && reports_pin(&String::from_utf8_lossy(&o.stdout)))
}

/// wasm-bindgen at exactly [`BINDGEN`]: on PATH (with rustup's directory
/// appended, where a `cargo install`ed one lands), else in this checkout's
/// `target/tooling`, else in the machine-wide `tooling` ([`host_tooling`]).
fn find_bindgen(
    root: &Path,
    tooling: Option<&Path>,
    path_env: Option<&std::ffi::OsStr>,
) -> Option<PathBuf> {
    let on_path = std::env::split_paths(path_env.unwrap_or_default())
        .map(|d| d.join("wasm-bindgen"))
        .find(|p| p.is_file());
    let cached = [
        Some(root.join("target/tooling")),
        tooling.map(Path::to_path_buf),
    ]
    .into_iter()
    .flatten()
    .map(|t| bindgen_root(&t).join("bin/wasm-bindgen"));
    on_path
        .into_iter()
        .chain(cached)
        .find(|p| answers_pin(p, path_env))
}

/// The wasm-bindgen CLI at exactly [`BINDGEN`] ([`find_bindgen`], with
/// `rustup`'s directory appended to PATH), or the skip naming `verify provision
/// --record`, which lays it in the machine-wide `tooling`.
fn bindgen_at(root: &Path, rustup: &Path) -> Result<PathBuf, LaneRun> {
    let mut path_dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    if let Some(dir) = rustup.parent() {
        path_dirs.push(dir.to_path_buf());
    }
    let path_env = std::env::join_paths(path_dirs).ok();
    let tooling = host_tooling();
    find_bindgen(root, tooling.as_deref(), path_env.as_deref())
        .ok_or_else(|| bindgen_skip(root, tooling.as_deref()))
}

/// The skip for a machine without the CLI: `verify provision --record`, which
/// lays it in the machine-wide `tooling`; where no `$HOME` names that, its
/// install into the checkout's `target/tooling`.
fn bindgen_skip(root: &Path, tooling: Option<&Path>) -> LaneRun {
    let install = |into: &Path| {
        format!(
            "cd / && targo --unverified {} --root '{}'",
            BINDGEN_INSTALL.join(" "),
            bindgen_root(into).display()
        )
    };
    match tooling {
        Some(t) => LaneRun::skip(
            format!(
                "wasm-bindgen {BINDGEN} is not on PATH, beside rustup, in target/tooling or in {}",
                t.display()
            ),
            "targo --unverified verify provision --record",
        ),
        None => LaneRun::skip(
            format!(
                "wasm-bindgen {BINDGEN} is not on PATH, beside rustup or in target/tooling, and no \
                 $HOME names the machine's tooling"
            ),
            install(&root.join("target/tooling")),
        ),
    }
}

/// What the twin binds in place of `export_runtime!()`: the console and
/// stack the panic hook reports through, and the two clocks the toy reads
/// through `host`, as wasm-bindgen imports.
const TWIN_HOST: &str = r#"mod host {
    use wasm_bindgen::prelude::*;

    #[wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(js_namespace = console, js_name = error)]
        fn console_error(s: &str);
        type Error;
        #[wasm_bindgen(constructor)]
        fn new() -> Error;
        #[wasm_bindgen(structural, method, getter)]
        fn stack(error: &Error) -> String;
        #[wasm_bindgen(js_namespace = Date, js_name = now)]
        pub fn date_now() -> f64;
        #[wasm_bindgen(js_namespace = performance, js_name = now)]
        pub fn performance_now() -> f64;
    }

    pub const CONSOLE: aterm_wasm_abi::panic_hook::Console = aterm_wasm_abi::panic_hook::Console {
        error: |s| console_error(s),
        stack: || Error::new().stack(),
    };
}"#;

/// The toy's import line and runtime call, which the twin replaces.
const TOY_USE: &str = "use aterm_wasm_abi::{export, host, panic_hook};";
const TOY_RUNTIME: &str = "aterm_wasm_abi::export_runtime!();";

/// Runs the `wasm-glue` oracle check over `root`, building in `work`.
pub(crate) fn gate(root: &Path, work: &Path, log: &mut String) -> LaneRun {
    let rustup = match super::toy::stable_wasm32() {
        Ok(rustup) => rustup,
        Err(skip) => return skip,
    };
    let bindgen = match bindgen_at(root, &rustup) {
        Ok(bindgen) => bindgen,
        Err(skip) => return skip,
    };
    let _ = writeln!(log, "  wasm-bindgen: {} ({BINDGEN})", bindgen.display());
    match run(root, &rustup, &bindgen, work, log) {
        Ok(findings) if findings.is_empty() => LaneRun::pass(),
        Ok(findings) => LaneRun {
            status: Status::Fail(findings),
            ..LaneRun::pass()
        },
        Err(e) => LaneRun::fail(e),
    }
}

/// Build, bind and judge the twin; the findings, or why nothing was judged.
fn run(
    root: &Path,
    rustup: &Path,
    bindgen: &Path,
    work: &Path,
    log: &mut String,
) -> Result<Vec<String>, String> {
    let toy = toy_dir(root);
    let twin = twin_source(&read(&toy.join("src/lib.rs"))?)?;
    let staged = work.join("twin");
    let manifest = super::toy::stage(root, &staged)?;
    std::fs::write(&manifest, twin_manifest(&read(&manifest)?)?)
        .map_err(|e| format!("could not write the twin's manifest: {e}"))?;
    std::fs::write(staged.join("src/lib.rs"), twin)
        .map_err(|e| format!("could not write the twin's source: {e}"))?;
    let module = super::toy::build_staged(
        rustup,
        &manifest,
        &work.join("target"),
        "the wasm-bindgen twin",
        log,
    )?;

    let glue = work.join("glue");
    if glue.exists() {
        std::fs::remove_dir_all(&glue)
            .map_err(|e| format!("could not clear {}: {e}", glue.display()))?;
    }
    for flavor in [Flavor::Nodejs, Flavor::Web] {
        let name = flavor_name(flavor);
        let out = command(bindgen)
            .args(["--target", name, "--out-dir"])
            .arg(glue.join(name))
            .arg(&module)
            .output_owned()
            .map_err(|e| format!("could not run {}: {e}", bindgen.display()))?;
        if !out.status.success() {
            return Err(format!(
                "wasm-bindgen --target {name} exited {:?}:\n{}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr).trim_end()
            ));
        }
    }

    let (passed, stderr) = super::toy::drive(&toy.join("drive.mjs"), &glue)?;
    let mut findings = judge_drive(passed, &stderr);
    if findings.is_empty() {
        let _ = writeln!(
            log,
            "  wasm-bindgen {BINDGEN} meets every drive.mjs check but the STRICTER ones, \
             and fails `another class` in both flavors"
        );
    }

    let before = findings.len();
    for flavor in [Flavor::Nodejs, Flavor::Web] {
        let name = flavor_name(flavor);
        let golden = read(&golden_dir(root, flavor).join(format!("{STEM}.d.ts")))?;
        let theirs = declarations(&read(&glue.join(name).join(format!("{STEM}.d.ts")))?);
        findings.extend(judge_declarations(name, &golden, &theirs));
    }
    if findings.len() == before {
        let _ = writeln!(
            log,
            "  the golden .d.ts declarations are wasm-bindgen {BINDGEN}'s, parameter names \
             included; a renamed parameter is caught in both flavors"
        );
    }
    Ok(findings)
}

fn read(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("could not read {}: {e}", path.display()))
}

/// The twin's source: the toy's, with `#[export]` spelled `#[wasm_bindgen]`
/// and `export_runtime!()` replaced by wasm-bindgen imports ([`TWIN_HOST`]).
///
/// # Errors
/// The toy no longer has a line the rewrite needs.
fn twin_source(toy: &str) -> Result<String, String> {
    for needle in [TOY_USE, TOY_RUNTIME, "#[export]", "#[export("] {
        if !toy.contains(needle) {
            return Err(format!(
                "the toy no longer has `{needle}`, which the wasm-bindgen twin rewrites"
            ));
        }
    }
    Ok(toy
        .replacen(
            TOY_USE,
            "use aterm_wasm_abi::panic_hook;\nuse wasm_bindgen::prelude::*;",
            1,
        )
        .replacen(TOY_RUNTIME, TWIN_HOST, 1)
        .replace("#[export(", "#[wasm_bindgen(")
        .replace("#[export]", "#[wasm_bindgen]"))
}

/// The staged toy manifest with its `[dependencies]` table replaced: the
/// pinned wasm-bindgen, and aterm-wasm-abi without the export layer.
///
/// # Errors
/// The manifest has no `[dependencies]` table to replace.
fn twin_manifest(staged: &str) -> Result<String, String> {
    let mut out = String::new();
    let (mut inside, mut replaced) = (false, false);
    for line in staged.lines() {
        if line.starts_with('[') {
            inside = line.trim_end() == "[dependencies]";
            if inside {
                replaced = true;
                let _ = write!(
                    out,
                    "[dependencies]\nwasm-bindgen = \"={BINDGEN}\"\n\
                     aterm-wasm-abi = {{ path = \"crates/aterm-wasm-abi\" }}\n\n"
                );
                continue;
            }
        }
        if !inside {
            out.push_str(line);
            out.push('\n');
        }
    }
    if replaced {
        Ok(out)
    } else {
        Err("the staged toy manifest has no [dependencies] table for the twin to replace".into())
    }
}

/// `drive.mjs`'s run on wasm-bindgen's glue, judged: it must fail, and only
/// the checks it marks STRICTER ([`stricter`]), `another class` among them in
/// both flavors.
fn judge_drive(passed: bool, stderr: &str) -> Vec<String> {
    let mut findings = Vec::new();
    let fails: Vec<&str> = stderr
        .lines()
        .filter(|l| l.starts_with("wasm-glue: FAIL "))
        .collect();
    let counted = stderr.lines().any(|l| {
        l.strip_prefix("wasm-glue: ")
            .and_then(|rest| rest.strip_suffix(" checks FAILED"))
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    });
    if passed || !counted {
        let lines: Vec<&str> = stderr.lines().collect();
        let tail = lines[lines.len().saturating_sub(20)..].join("\n");
        findings.push(format!(
            "drive.mjs did not end in the STRICTER checks' failures on wasm-bindgen's glue:\n{tail}"
        ));
    }
    let unexpected: Vec<&str> = fails.iter().copied().filter(|l| !stricter(l)).collect();
    if !unexpected.is_empty() {
        findings.push(format!(
            "checks wasm-bindgen's glue fails beyond the STRICTER ones:\n{}",
            unexpected.join("\n")
        ));
    }
    for flavor in ["nodejs", "web"] {
        let mark = format!("wasm-glue: FAIL {flavor} classes another class:");
        if !fails.iter().any(|l| l.starts_with(&mark)) {
            findings.push(format!(
                "NEGATIVE CONTROL: `another class` held on {flavor}: not wasm-bindgen's glue, \
                 or a vacuous run"
            ));
        }
    }
    findings
}

/// A `drive.mjs` failure line for a check it marks STRICTER than
/// wasm-bindgen: `another class` in a `classes` section, or a misuse throw.
fn stricter(line: &str) -> bool {
    let Some((section, rest)) = line
        .strip_prefix("wasm-glue: FAIL ")
        .and_then(|l| l.split_once(' '))
    else {
        return false;
    };
    section.bytes().all(|b| b.is_ascii_lowercase())
        && (rest.starts_with("classes another class:") || rest.starts_with("misuse throws "))
}

/// Compare the toy's generated .d.ts shape, not general TypeScript: each
/// declaration occupies one line, and an exported class or interface has
/// its opening brace on the header and a closing brace on its own line.
fn declarations(dts: &str) -> Vec<String> {
    let (mut found, mut owner) = (Vec::new(), None);
    for line in dts.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('/') || line.starts_with('*') {
            continue;
        }
        if line == "}" {
            owner = None;
        } else if let Some(header) = line.strip_suffix(" {").filter(|header| {
            header.starts_with("export class ") || header.starts_with("export interface ")
        }) {
            owner = Some(header);
            if header != "export interface InitOutput" {
                found.push(header.to_string());
            }
        } else if owner != Some("export interface InitOutput") {
            found.push(match owner {
                Some(header) => format!("{header} :: {line}"),
                None => line.to_string(),
            });
        }
    }
    found.sort_unstable();
    found
}

/// The entries of sorted `a` that sorted `b` lacks, counting repeats.
fn missing_from<'a>(a: &'a [String], b: &[String]) -> Vec<&'a str> {
    let (mut out, mut j) = (Vec::new(), 0);
    for x in a {
        while j < b.len() && b[j] < *x {
            j += 1;
        }
        if j < b.len() && b[j] == *x {
            j += 1;
        } else {
            out.push(x.as_str());
        }
    }
    out
}

/// One flavor's golden .d.ts against wasm-bindgen's `theirs` declarations,
/// and the NEGATIVE CONTROL: the golden with its first `(v: ` parameter
/// renamed must not match.
fn judge_declarations(flavor: &str, golden: &str, theirs: &[String]) -> Vec<String> {
    let mut findings = Vec::new();
    let ours = declarations(golden);
    if ours != theirs {
        let mut diff = String::new();
        for line in missing_from(theirs, &ours) {
            let _ = writeln!(diff, "< {line}");
        }
        for line in missing_from(&ours, theirs) {
            let _ = writeln!(diff, "> {line}");
        }
        findings.push(format!(
            "the {flavor} golden .d.ts declares otherwise than wasm-bindgen \
             (< wasm-bindgen, > golden):\n{}",
            diff.trim_end()
        ));
    }
    let planted = golden.replacen("(v: ", "(planted: ", 1);
    if planted == golden {
        findings.push(format!(
            "NEGATIVE CONTROL: no `(v: ` in the {flavor} golden .d.ts to plant in"
        ));
    } else if declarations(&planted) == theirs {
        findings.push(format!(
            "NEGATIVE CONTROL: a {flavor} .d.ts with a parameter renamed matched wasm-bindgen's"
        ));
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::perf::workspace_root;

    /// The twin rewrites every export attribute and the runtime, and refuses
    /// a toy that lost a line it rewrites.
    #[test]
    fn the_twin_spells_every_export_as_wasm_bindgen() {
        let toy = std::fs::read_to_string(toy_dir(&workspace_root()).join("src/lib.rs")).unwrap();
        let twin = twin_source(&toy).unwrap();
        assert!(!twin.contains("#[export"), "an export attribute survived");
        assert!(!twin.contains(TOY_RUNTIME) && twin.contains("pub fn date_now() -> f64;"));
        assert_eq!(
            twin.matches("#[wasm_bindgen").count(),
            toy.matches("#[export").count() + TWIN_HOST.matches("#[wasm_bindgen").count()
        );
        let err = twin_source(&toy.replace(TOY_RUNTIME, "")).unwrap_err();
        assert!(err.contains("export_runtime"), "{err}");
    }

    /// Only the `[dependencies]` table changes, and a manifest without one is
    /// refused.
    #[test]
    fn the_twin_manifest_replaces_only_the_dependencies() {
        let staged = "[package]\nname = \"t\"\n\n[dependencies]\naterm-wasm-abi = { path = \
                      \"crates/aterm-wasm-abi\", features = [\"export\"] }\n\n[workspace]\n\
                      members = []\n";
        let twin = twin_manifest(staged).unwrap();
        assert!(twin.starts_with("[package]\nname = \"t\"\n\n[dependencies]\n"));
        assert!(twin.contains(&format!("wasm-bindgen = \"={BINDGEN}\"")));
        assert!(!twin.contains("\"export\""));
        assert!(twin.ends_with("[workspace]\nmembers = []\n"));
        assert!(twin_manifest("[package]\nname = \"t\"\n").is_err());
    }

    const RUN: &str = "wasm-glue: nodejs scalars: 40 checks\n\
                       wasm-glue: FAIL nodejs classes another class: no throw\n\
                       wasm-glue: FAIL nodejs misuse throws 100k: leaked\n\
                       wasm-glue: FAIL web classes another class: no throw\n\
                       wasm-glue: 3 checks FAILED\n";

    /// wasm-bindgen's run passes only when it fails the STRICTER checks and
    /// nothing else, `another class` in both flavors.
    #[test]
    fn the_drive_verdict_wants_exactly_the_stricter_failures() {
        assert_eq!(judge_drive(false, RUN), Vec::<String>::new());
        let held = RUN.replace("wasm-glue: FAIL web classes another class: no throw\n", "");
        let found = judge_drive(false, &held);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("held on web"), "{found:?}");
        let extra = RUN.replace(
            "wasm-glue: 3",
            "wasm-glue: FAIL nodejs scalars u32 max is unsigned: -1\nwasm-glue: 4",
        );
        assert!(judge_drive(false, &extra)[0].contains("u32 max"));
        let passed = "wasm-glue: every check held\n";
        assert_eq!(judge_drive(true, passed).len(), 3);
        let died = RUN.replace("wasm-glue: 3 checks FAILED\n", "TypeError: boom\n");
        assert!(judge_drive(false, &died)[0].contains("TypeError: boom"));
    }

    const DTS: &str = "/* tslint:disable */\n\
                       export function echo_u8(v: number): number;\n\
                       export class Counter {\n  free(): void;\n  add(by: bigint): bigint;\n  \
                       readonly value: number;\n}\n\
                       export interface InitOutput {\n  readonly echo_u8: (a: number) => number;\n}\n";

    #[test]
    fn declarations_keep_owners_and_properties_but_drop_comments_and_raw_interface() {
        assert_eq!(
            declarations(DTS),
            [
                "export class Counter",
                "export class Counter :: add(by: bigint): bigint;",
                "export class Counter :: free(): void;",
                "export class Counter :: readonly value: number;",
                "export function echo_u8(v: number): number;",
            ]
        );
    }

    /// Member order is immaterial; class identity, member ownership,
    /// properties and parameter names matter.
    #[test]
    fn a_golden_matches_only_wasm_bindgens_declarations() {
        let theirs = declarations(DTS);
        assert_eq!(
            judge_declarations("web", DTS, &theirs),
            Vec::<String>::new()
        );
        let reordered = DTS.replace(
            "  free(): void;\n  add(by: bigint): bigint;\n  readonly value: number;",
            "  readonly value: number;\n  add(by: bigint): bigint;\n  free(): void;",
        );
        assert_ne!(reordered, DTS);
        assert_eq!(
            judge_declarations("web", &reordered, &theirs),
            Vec::<String>::new()
        );
        let moved = DTS.replace("  add(by: bigint): bigint;\n", "")
            + "export class Snapshot {\n  add(by: bigint): bigint;\n}\n";
        let with_snapshot = format!("{DTS}export class Snapshot {{\n}}\n");
        let with_snapshot_declarations = declarations(&with_snapshot);
        let mut missed = Vec::new();
        for (change, golden, expected) in [
            (
                "renamed class",
                DTS.replace("class Counter", "class WrongCounter"),
                &theirs,
            ),
            ("moved member", moved, &with_snapshot_declarations),
            (
                "changed readonly type",
                DTS.replace("readonly value: number;", "readonly value: string;"),
                &theirs,
            ),
            (
                "removed readonly property",
                DTS.replace("  readonly value: number;\n", ""),
                &theirs,
            ),
        ] {
            assert_ne!(golden, DTS);
            let found = judge_declarations("web", &golden, expected);
            if found.is_empty() {
                missed.push(change);
            } else {
                assert_eq!(found.len(), 1, "{change}: {found:?}");
                assert!(
                    found[0].contains("declares otherwise"),
                    "{change}: {found:?}"
                );
            }
        }
        assert!(
            missed.is_empty(),
            "undetected declaration changes: {missed:?}"
        );
        let renamed = DTS.replace("(by: ", "(n: ");
        let found = judge_declarations("web", &renamed, &theirs);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0].contains("< export class Counter :: add(by: bigint): bigint;"),
            "{}",
            found[0]
        );
        assert!(
            found[0].contains("> export class Counter :: add(n: bigint): bigint;"),
            "{}",
            found[0]
        );
        let bare = DTS.replace("(v: ", "(x: ");
        let found = judge_declarations("web", &bare, &declarations(&bare));
        assert_eq!(found.len(), 1);
        assert!(found[0].contains("no `(v: `"), "{found:?}");
        let comment = format!("// (v: x\n{DTS}");
        let found = judge_declarations("web", &comment, &theirs);
        assert!(found[0].contains("matched wasm-bindgen's"), "{found:?}");
    }

    #[test]
    fn missing_from_counts_repeats() {
        let s = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        assert_eq!(
            missing_from(&s(&["a", "a", "b"]), &s(&["a", "c"])),
            ["a", "b"]
        );
        assert!(missing_from(&s(&["a"]), &s(&["a", "b"])).is_empty());
    }

    /// Only a wasm-bindgen that answers exactly the pin is found, in the
    /// checkout's `target/tooling` or the machine's `tooling`: a version the
    /// pin is a prefix of, and one that does not answer, are not.
    #[cfg(unix)]
    #[test]
    fn only_a_bindgen_answering_exactly_the_pin_is_found() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = aterm_tempfile::Builder::new()
            .prefix("atv-wasm-")
            .tempdir()
            .expect("scratch");
        let checkout = dir.path().join("checkout");
        let machine = dir.path().join("tooling");
        let no_path = Some(std::ffi::OsStr::new(""));
        let installs_into = |skip: LaneRun, tooling: &Path| match skip.status {
            Status::Skip { fix, .. } => {
                fix.contains(&format!("--root '{}'", bindgen_root(tooling).display()))
            }
            _ => false,
        };
        assert!(matches!(
            bindgen_skip(&checkout, Some(&machine)).status,
            Status::Skip { fix, .. } if fix == "targo --unverified verify provision --record"
        ));
        assert!(installs_into(
            bindgen_skip(&checkout, None),
            &checkout.join("target/tooling")
        ));
        for tooling in [checkout.join("target/tooling"), machine.clone()] {
            let cache = bindgen_root(&tooling).join("bin/wasm-bindgen");
            std::fs::create_dir_all(cache.parent().expect("parent")).expect("mkdir");
            for (body, found) in [
                (format!("echo 'wasm-bindgen {BINDGEN}0'"), false),
                ("exit 1".to_string(), false),
                (format!("echo 'wasm-bindgen {BINDGEN}'"), true),
            ] {
                let _ = std::fs::remove_file(&cache);
                std::fs::write(&cache, format!("#!/bin/sh\n{body}\n")).expect("stub");
                std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o755))
                    .expect("chmod");
                assert_eq!(
                    find_bindgen(&checkout, Some(&machine), no_path),
                    found.then(|| cache.clone()),
                    "{}: {body}",
                    tooling.display()
                );
            }
            if tooling != machine {
                std::fs::remove_file(&cache).expect("rm");
            }
        }
        assert_eq!(find_bindgen(&checkout, None, no_path), None);
    }
}
