// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

const DIR: &str = "crates/a";

const MANIFEST: &str = "[package]\nname = \"a\"\nversion = \"0.1.0\"\nedition = \"2024\"\n";

const LIB: &str = "pub mod services;\n\npub fn f() -> u8 {\n    1\n}\n";

const SERVICES: &str = r#"//! Services.

pub fn tester() -> &'static str {
    "tester"
}

#[cfg(test)]
mod tests {
    #[test]
    fn names_the_tester() {
        assert_eq!(super::tester(), "tester");
    }
}
"#;

/// A package's files before and after a change, by package-relative path.
#[derive(Clone)]
struct Change {
    before: BTreeMap<String, String>,
    now: BTreeMap<String, String>,
    /// Files of other packages, by repo-relative path, the same before and after.
    beside: BTreeMap<String, String>,
}

impl Change {
    /// The package with `files` beside its manifest, the same before and after.
    fn of(files: &[(&str, &str)]) -> Self {
        let mut before: BTreeMap<String, String> =
            [("Cargo.toml".to_string(), MANIFEST.to_string())].into();
        for (path, text) in files {
            before.insert((*path).to_string(), (*text).to_string());
        }
        Self {
            now: before.clone(),
            before,
            beside: BTreeMap::new(),
        }
    }

    /// The same, with another package's file at the repo-relative `path`.
    fn beside(mut self, path: &str, text: &str) -> Self {
        self.beside.insert(path.to_string(), text.to_string());
        self
    }

    /// The package of `lib.rs` and `services.rs` above, with `files` added or replaced.
    fn services(files: &[(&str, &str)]) -> Self {
        let mut all = vec![("src/lib.rs", LIB), ("src/services.rs", SERVICES)];
        all.extend_from_slice(files);
        Self::of(&all)
    }

    /// The same before, with `path` now `text` (`None` deletes it).
    fn with(mut self, path: &str, text: Option<&str>) -> Self {
        match text {
            Some(text) => self.now.insert(path.to_string(), text.to_string()),
            None => self.now.remove(path),
        };
        self
    }

    /// The same before, with `from` replaced by `to` in `path`, which must hold it.
    fn edit(self, path: &str, from: &str, to: &str) -> Self {
        let text = self.now.get(path).expect("the file").clone();
        assert!(text.contains(from), "{path} holds {from:?}");
        self.with(path, Some(&text.replacen(from, to, 1)))
    }

    /// The repo-relative paths that differ between the two versions.
    fn changed(&self) -> Vec<String> {
        self.before
            .keys()
            .chain(self.now.keys())
            .filter(|path| self.before.get(*path) != self.now.get(*path))
            .map(|path| join(DIR, path))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    fn judge(&self) -> Result<(), Doubt> {
        let changed = self.changed();
        assert!(!changed.is_empty(), "the fixture changes something");
        let changed: Vec<&str> = changed.iter().map(String::as_str).collect();
        confined(
            self,
            DIR,
            Touched {
                changed: &changed,
                ..Touched::default()
            },
        )
    }

    /// The judgment of this change beside one outside the package: `readers`, package-relative,
    /// read another package's changed files, and `elsewhere`, repo-relative, changed outside it.
    fn judge_reads(&self, readers: &[&str], elsewhere: &[&str]) -> Result<(), Doubt> {
        let changed = self.changed();
        let changed: Vec<&str> = changed.iter().map(String::as_str).collect();
        let readers: Vec<String> = readers.iter().map(|rel| join(DIR, rel)).collect();
        let readers: Vec<&str> = readers.iter().map(String::as_str).collect();
        assert!(
            !changed.is_empty() || !readers.is_empty(),
            "the package holds something of the change"
        );
        confined(
            self,
            DIR,
            Touched {
                changed: &changed,
                readers: &readers,
                elsewhere,
            },
        )
    }

    /// Where `rel` spells [`FOREIGN`] now, for [`observes`].
    fn spelling(&self, rel: &str) -> Spelling {
        self.spelling_of(rel, FOREIGN)
    }

    /// Where `rel` spells the repo-relative `path` now, for [`observes`].
    fn spelling_of(&self, rel: &str, path: &str) -> Spelling {
        let text = &self.now[rel];
        let at: Vec<usize> = text.match_indices(path).map(|(at, _)| at).collect();
        assert!(!at.is_empty(), "{rel} spells {path}");
        Spelling {
            file: join(DIR, rel),
            path: path.to_string(),
            at,
        }
    }
}

/// Another package's file, which the package's sources read or spell.
const FOREIGN: &str = "crates/t/src/x.rs";

impl Files for Change {
    fn under(&self, dir: &str) -> Vec<String> {
        self.now
            .keys()
            .map(|path| join(DIR, path))
            .filter(|path| within(dir, path).is_some())
            .collect()
    }

    fn now(&self, path: &str) -> Result<Option<String>, String> {
        Ok(within(DIR, path)
            .map_or_else(|| self.beside.get(path), |rel| self.now.get(rel))
            .cloned())
    }

    fn before(&self, path: &str) -> Result<Option<String>, String> {
        Ok(within(DIR, path)
            .map_or_else(|| self.beside.get(path), |rel| self.before.get(rel))
            .cloned())
    }
}

fn shipped(rel: &str) -> Result<(), Doubt> {
    Err(Doubt::Shipped(join(DIR, rel)))
}

/// The measured case (atpkg's `abrew/services.rs`, 2026-11-09): string literals inside the test
/// module change. CONTROL: the same literal in the shipped function is a dependent-visible change.
#[test]
fn a_change_inside_a_test_module_is_confined() {
    let tests = Change::services(&[]).edit(
        "src/services.rs",
        "assert_eq!(super::tester(), \"tester\");",
        "assert_eq!(super::tester(), \"tester\", \"the tester\");\n        assert!(!super::tester().is_empty());",
    );
    assert_eq!(tests.judge(), Ok(()));
    let shipped_literal =
        Change::services(&[]).edit("src/services.rs", "    \"tester\"\n", "    \"probe\"\n");
    assert_eq!(shipped_literal.judge(), shipped("src/services.rs"));
}

/// A seam both builds compile reaches dependents. CONTROL: the same item under `cfg(test)` alone
/// does not.
#[test]
fn a_change_inside_a_debug_assertions_seam_is_not_confined() {
    for (cfg, confined) in [
        ("any(test, debug_assertions)", false),
        ("any(test, feature = \"probe\")", false),
        ("not(test)", false),
        ("test", true),
        ("all(test, unix)", true),
    ] {
        let seam = format!(
            "pub fn tester() -> &'static str {{\n    \"tester\"\n}}\n\n#[cfg({cfg})]\npub fn seam() -> u8 {{\n    1\n}}\n"
        );
        let change = Change::services(&[("src/services.rs", &seam)]).edit(
            "src/services.rs",
            "    1\n",
            "    2\n",
        );
        let want = if confined {
            Ok(())
        } else {
            shipped("src/services.rs")
        };
        assert_eq!(change.judge(), want, "{cfg}");
    }
}

#[test]
fn a_change_that_also_touches_a_shipped_line_is_not_confined() {
    let change = Change::services(&[])
        .edit("src/services.rs", "    \"tester\"\n", "    \"probe\"\n")
        .edit("src/services.rs", "//! Services.", "//! The services.");
    assert_eq!(change.judge(), shipped("src/services.rs"));
    // A doc comment no macro reads is no shipped line: beside a test hunk it stays confined.
    let doc = Change::services(&[])
        .edit("src/services.rs", "\"tester\");", "\"probe\");")
        .edit("src/services.rs", "//! Services.", "//! The services.");
    assert_eq!(doc.judge(), Ok(()));
    // Another file of the change that ships keeps the whole change unconfined.
    let two = Change::services(&[])
        .edit("src/services.rs", "\"tester\");", "\"probe\");")
        .edit("src/lib.rs", "    1\n", "    2\n");
    assert_eq!(two.judge(), shipped("src/lib.rs"));
}

const MOUNTED: &str = "#[cfg(test)]\n#[path = \"lib_tests.rs\"]\nmod tests;\n";

const LIB_TESTS: &str = "use super::*;\n\n#[test]\nfn one() {\n    assert_eq!(f(), 1);\n}\n";

/// A file only a `cfg(test)` declaration mounts, by `#[path]`, holds nothing a build without tests
/// reads, whatever changes in it. CONTROL: a shipped declaration of the same file, by `#[path]` or
/// by default, or one whose path a `cfg_attr` may set, makes its every token count.
#[test]
fn a_file_mounted_only_under_cfg_test_is_confined() {
    let lib = format!("{LIB}\n{MOUNTED}");
    let any_text = |lib: &str| {
        Change::services(&[("src/lib.rs", lib), ("src/lib_tests.rs", LIB_TESTS)]).with(
            "src/lib_tests.rs",
            Some("pub fn anything() -> u8 {\n    7\n}\n"),
        )
    };
    assert_eq!(any_text(&lib).judge(), Ok(()));
    for shipped_mount in [
        "#[path = \"lib_tests.rs\"]\nmod helpers;\n",
        "#[path = \"../src/lib_tests.rs\"]\nmod helpers;\n",
        "mod lib_tests;\n",
        // A file system that folds case opens the file by either spelling.
        "#[path = \"Lib_Tests.rs\"]\nmod helpers;\n",
        "mod Lib_Tests;\n",
        "#[cfg_attr(unix, path = \"elsewhere.rs\")]\nmod plat;\n",
        "macro_rules! mount { ($name:ident) => { mod $name; }; }\n",
    ] {
        let lib = format!("{lib}{shipped_mount}");
        assert_eq!(
            any_text(&lib).judge(),
            shipped("src/lib_tests.rs"),
            "{shipped_mount}"
        );
    }
    // A mount a test module holds, at any depth, is a test's too.
    let nested = format!(
        "{LIB}\n#[cfg(test)]\nmod tests {{\n    #[path = \"lib_tests.rs\"]\n    mod deep;\n}}\n"
    );
    assert_eq!(any_text(&nested).judge(), Ok(()));
    // Adding a test file and its `cfg(test)` declaration together is confined; the declaration
    // added outside `cfg(test)` is not.
    let added = Change::services(&[])
        .with("src/lib.rs", Some(&lib))
        .with("src/lib_tests.rs", Some(LIB_TESTS));
    assert_eq!(added.judge(), Ok(()));
    let added_shipped = Change::services(&[])
        .with(
            "src/lib.rs",
            Some(&format!("{LIB}\n#[path = \"lib_tests.rs\"]\nmod tests;\n")),
        )
        .with("src/lib_tests.rs", Some(LIB_TESTS));
    assert_eq!(added_shipped.judge(), shipped("src/lib.rs"));
}

/// NEGATIVE CONTROL: a dependent-visible change hidden next to a test-only hunk widens again,
/// wherever it hides.
#[test]
fn a_planted_change_next_to_a_test_hunk_is_not_confined() {
    let test_hunk = || Change::services(&[]).edit("src/services.rs", "\"tester\");", "\"probe\");");
    assert_eq!(test_hunk().judge(), Ok(()));
    for (from, to) in [
        // On the line that closes the test module.
        ("    }\n}\n", "    }\n} pub fn planted() {}\n"),
        // After the module, with a brace in a string inside it to end a naive scan early.
        (
            "    }\n}\n",
            "    }\n    const B: &str = \"}\";\n}\npub fn planted() {}\n",
        ),
        // The module's `cfg` weakened to one a build without tests can satisfy.
        ("#[cfg(test)]", "#[cfg(any(test, feature = \"probe\"))]"),
        ("#[cfg(test)]", "#[cfg_attr(test, cfg(test))]"),
        // An item slipped between the `cfg` and the module takes the `cfg` from it.
        ("#[cfg(test)]\n", "#[cfg(test)]\npub fn planted() {}\n"),
    ] {
        let change = test_hunk().edit("src/services.rs", from, to);
        assert_eq!(change.judge(), shipped("src/services.rs"), "{to}");
    }
    // CONTROL: a macro on the module itself, outer or inner, runs only once its `cfg` kept it,
    // which no build without tests does.
    for (from, to) in [
        (
            "#[cfg(test)]\nmod tests",
            "#[cfg(test)]\n#[some_macro]\nmod tests",
        ),
        (
            "#[cfg(test)]\nmod tests",
            "#[some_macro]\n#[cfg(test)]\nmod tests",
        ),
        ("mod tests {\n", "mod tests {\n    #![some_macro]\n"),
    ] {
        assert_eq!(
            test_hunk().edit("src/services.rs", from, to).judge(),
            Ok(()),
            "{to}"
        );
    }
}

/// An attribute macro around a test item receives it before its `cfg` is read.
#[test]
fn a_test_item_an_attribute_macro_may_rewrite_is_not_confined() {
    let wrapped = "#[some_macro]\npub mod m {\n    #[cfg(test)]\n    fn helper() -> u8 {\n        1\n    }\n}\n";
    let change = Change::services(&[("src/services.rs", wrapped)]).edit(
        "src/services.rs",
        "        1\n",
        "        2\n",
    );
    assert_eq!(change.judge(), shipped("src/services.rs"));
    // CONTROL: an inert attribute reads on.
    let inert = wrapped.replace("#[some_macro]", "#[allow(dead_code)]");
    let change = Change::services(&[("src/services.rs", &inert)]).edit(
        "src/services.rs",
        "        1\n",
        "        2\n",
    );
    assert_eq!(change.judge(), Ok(()));
}

/// A file no declaration here places may be a module a macro from another crate declares: its
/// shipped tokens count. CONTROL: a change inside its test module is confined.
#[test]
fn a_file_no_declaration_places_is_judged_by_its_tokens() {
    let lib = format!("{LIB}elsewhere::declare!(hidden);\n");
    let base = Change::services(&[("src/lib.rs", &lib), ("src/hidden.rs", SERVICES)]);
    let test_hunk = base
        .clone()
        .edit("src/hidden.rs", "\"tester\");", "\"probe\");");
    assert_eq!(test_hunk.judge(), Ok(()));
    let shipped_hunk = base
        .clone()
        .edit("src/hidden.rs", "    \"tester\"\n", "    \"probe\"\n");
    assert_eq!(shipped_hunk.judge(), shipped("src/hidden.rs"));
    // A new file is all new tokens, whatever holds them.
    let added = Change::services(&[("src/lib.rs", &lib)]).with("src/hidden.rs", Some(SERVICES));
    assert_eq!(added.judge(), shipped("src/hidden.rs"));
}

/// `column!` and a panic's location count characters: a test hunk that keeps the bytes before
/// shipped code on its line but not their characters moves that code. CONTROL: the same hunk on a
/// line no shipped code follows is confined.
#[test]
fn a_test_hunk_that_moves_shipped_code_along_its_line_is_not_confined() {
    let shared = "#[cfg(test)] const T: &str = \"ab\"; pub fn at() -> u32 { column!() }\n";
    let narrower = Change::services(&[("src/services.rs", shared)]).edit(
        "src/services.rs",
        "\"ab\"",
        "\"\u{e9}\"",
    );
    assert_eq!(narrower.judge(), shipped("src/services.rs"));
    let own_line = "pub fn at() -> u32 { column!() }\n#[cfg(test)] const T: &str = \"ab\";\n";
    let alone = Change::services(&[("src/services.rs", own_line)]).edit(
        "src/services.rs",
        "\"ab\"",
        "\"\u{e9}\"",
    );
    assert_eq!(alone.judge(), Ok(()));
}

/// Shipped tokens keep their place: a test hunk that moves later shipped code moves what
/// `line!` and a panic's location read.
#[test]
fn a_test_hunk_that_moves_shipped_code_is_not_confined() {
    let middle =
        "#[cfg(test)]\nmod tests {\n    fn t() {}\n}\n\npub fn after() -> u32 {\n    line!()\n}\n";
    let base = Change::services(&[("src/services.rs", middle)]);
    let longer = base.clone().edit(
        "src/services.rs",
        "    fn t() {}\n",
        "    fn t() {}\n    fn u() {}\n",
    );
    assert_eq!(longer.judge(), shipped("src/services.rs"));
    let same_lines = base.edit("src/services.rs", "fn t() {}", "fn renamed() {}");
    assert_eq!(same_lines.judge(), Ok(()));
}

/// A build script reads what it declares and what its literals name. CONTROL: one that declares
/// only itself reads nothing of the sources.
#[test]
fn a_build_script_that_may_read_the_file_keeps_the_cone() {
    let test_hunk = |files: &[(&str, &str)]| {
        Change::services(files).edit("src/services.rs", "\"tester\");", "\"probe\");")
    };
    let script = |body: &str| format!("fn main() {{\n{body}\n}}\n");
    let own = script("    println!(\"cargo:rerun-if-changed=build.rs\");");
    assert_eq!(test_hunk(&[("build.rs", &own)]).judge(), Ok(()));
    let doubt = Err(Doubt::BuildInput(join(DIR, "src/services.rs")));
    for body in [
        "    println!(\"cargo:rerun-if-changed=src\");",
        "    println!(\"cargo::rerun-if-changed=src/services.rs\");",
        "    println!(\"cargo:rerun-if-changed={}\", path());",
        "    println!(\"cargo:rustc-cfg=probe\");",
        "    println!(\"cargo:rerun-if-changed=build.rs\");\n    let _ = std::fs::read(\"src/services.rs\");",
        "    println!(\"cargo:rerun-if-changed=build.rs\");\n    walk(format!(\"{}/src\", dir()));",
        "    println!(\"cargo:rerun-if-changed=build.rs\");\n    let _ = include_str!(\"src/services.rs\");",
        // A walk of the package from where Cargo runs the script names no file.
        "    println!(\"cargo:rerun-if-changed=build.rs\");\n    walk(std::env::var(\"CARGO_MANIFEST_DIR\").unwrap());",
        "    println!(\"cargo:rerun-if-changed=build.rs\");\n    walk(std::env::current_dir().unwrap());",
    ] {
        assert_eq!(
            test_hunk(&[("build.rs", &script(body))]).judge(),
            doubt,
            "{body}"
        );
    }
    let mounts = format!("#[path = \"src/services.rs\"]\nmod services;\n{own}");
    assert_eq!(test_hunk(&[("build.rs", &mounts)]).judge(), doubt);
    let helper = format!("mod helper;\n{own}");
    let declared = "pub fn emit() {\n    println!(\"cargo:rerun-if-changed=src\");\n}\n";
    assert_eq!(
        test_hunk(&[("build.rs", &helper), ("helper.rs", declared)]).judge(),
        doubt
    );
    // A helper the reader cannot find is a doubt too.
    assert_eq!(test_hunk(&[("build.rs", &helper)]).judge(), doubt);
    // A build-dependency's code may declare inputs no literal here shows.
    let with_deps =
        format!("{MANIFEST}\n[build-dependencies]\nhelper = {{ path = \"../helper\" }}\n");
    assert_eq!(
        test_hunk(&[("build.rs", &own), ("Cargo.toml", &with_deps)]).judge(),
        doubt
    );
    let target_deps =
        format!("{MANIFEST}\n[target.'cfg(unix)'.build-dependencies]\nhelper = \"1\"\n");
    assert_eq!(
        test_hunk(&[("build.rs", &own), ("Cargo.toml", &target_deps)]).judge(),
        doubt
    );
    // `build = false` leaves a `build.rs` unbuilt; `build = "other.rs"` builds that one.
    let off = format!("{MANIFEST}build = false\n");
    let undeclared = script("    println!(\"cargo:rustc-cfg=probe\");");
    assert_eq!(
        test_hunk(&[("build.rs", &undeclared), ("Cargo.toml", &off)]).judge(),
        Ok(())
    );
    let other = format!("{MANIFEST}build = \"other.rs\"\n");
    assert_eq!(
        test_hunk(&[("other.rs", &undeclared), ("Cargo.toml", &other)]).judge(),
        doubt
    );
}

/// What shipped code may embed or names keeps the cone. CONTROL: the same under `cfg(test)`, or
/// what the build script wrote, does not.
#[test]
fn a_file_shipped_code_embeds_or_names_is_not_confined() {
    let with_lib = |extra: &str| {
        Change::services(&[("src/lib.rs", &format!("{LIB}{extra}"))]).edit(
            "src/services.rs",
            "\"tester\");",
            "\"probe\");",
        )
    };
    let services = join(DIR, "src/services.rs");
    for (extra, want) in [
        (
            "pub const S: &str = include_str!(\"services.rs\");\n",
            Err(Doubt::Embedded(services.clone())),
        ),
        (
            "pub const S: &[u8] = include_bytes!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/src/services.rs\"));\n",
            Err(Doubt::Embedded(services.clone())),
        ),
        (
            "macro_rules! embed { ($p:literal) => { include_bytes!($p) }; }\n",
            Err(Doubt::Embedded(services.clone())),
        ),
        (
            "pub const S: &str = \"src/services.rs\";\n",
            Err(Doubt::Named(services.clone())),
        ),
        (
            "#[cfg(test)]\nconst S: &str = include_str!(\"services.rs\");\n",
            Ok(()),
        ),
        (
            "#[cfg(test)]\nconst S: &str = \"src/services.rs\";\n",
            Ok(()),
        ),
        (
            "include!(concat!(env!(\"OUT_DIR\"), \"/generated.rs\"));\n",
            Ok(()),
        ),
    ] {
        assert_eq!(with_lib(extra).judge(), want, "{extra}");
    }
    // An `include!` of a test-mounted file brings its tokens into the build.
    let tests = format!("{MOUNTED}include!(\"lib_tests.rs\");\n");
    let change = Change::services(&[
        ("src/lib.rs", &format!("{LIB}{tests}")),
        ("src/lib_tests.rs", LIB_TESTS),
    ])
    .with("src/lib_tests.rs", Some("pub fn anything() {}\n"));
    assert_eq!(change.judge(), shipped("src/lib_tests.rs"));
}

/// A bench or example root, and what such a root alone mounts, compiles into no target a
/// dependent links: a code change there is confined, as a test's is. Cargo discovers
/// `benches/<x>.rs`, `benches/<x>/main.rs` and the same under `examples/` while `autobenches` and
/// `autoexamples` hold, and takes a `[[bench]]` or `[[example]]` by its path or, with none, by its
/// name. CONTROL: the build script is another target, a README is no source, a bench file the
/// library also mounts or embeds ships, a file no root mounts is unplaced, and with discovery off
/// a bench or example no entry names is unplaced too.
#[test]
fn a_bench_or_example_only_its_targets_compile_is_confined_and_the_build_script_is_not() {
    let main = "fn main() {}\n";
    let coded = |change: Change, path: &str| {
        change
            .edit(path, "fn main() {}", "fn main() { let _ = 1; }")
            .judge()
    };
    let with = |files: &[(&str, &str)]| Change::services(files);
    for (files, path) in [
        (vec![("benches/b.rs", main)], "benches/b.rs"),
        (vec![("benches/x/main.rs", main)], "benches/x/main.rs"),
        (vec![("examples/e.rs", main)], "examples/e.rs"),
        (vec![("examples/x/main.rs", main)], "examples/x/main.rs"),
        (
            vec![
                (
                    "examples/e.rs",
                    "#[path = \"../src/codegen.rs\"]\nmod codegen;\nfn main() {}\n",
                ),
                ("src/codegen.rs", main),
            ],
            "src/codegen.rs",
        ),
        (
            vec![
                ("benches/b.rs", "mod support;\nfn main() {}\n"),
                ("benches/support/mod.rs", main),
            ],
            "benches/support/mod.rs",
        ),
    ] {
        assert_eq!(coded(with(&files), path), Ok(()), "{path}");
    }
    // With discovery off, a `[[bench]]` or `[[example]]` by name, or by path, is a root.
    for (manifest, path) in [
        (
            "autobenches = false\n\n[[bench]]\nname = \"x\"\n",
            "benches/x.rs",
        ),
        (
            "autobenches = false\n\n[[bench]]\nname = \"x\"\n",
            "benches/x/main.rs",
        ),
        (
            "autoexamples = false\n\n[[example]]\nname = \"x\"\n",
            "examples/x.rs",
        ),
        (
            "autobenches = false\n\n[[bench]]\nname = \"b\"\npath = \"src/bench.rs\"\n",
            "src/bench.rs",
        ),
    ] {
        let manifest = format!("{MANIFEST}{manifest}");
        let change = with(&[("Cargo.toml", &manifest), (path, main)]);
        assert_eq!(coded(change, path), Ok(()), "{manifest} {path}");
    }
    // A `[[test]]` root outside `tests/` is a test's.
    let custom = format!("{MANIFEST}\n[[test]]\nname = \"inner\"\npath = \"src/inner_test.rs\"\n");
    let change = with(&[("Cargo.toml", &custom), ("src/inner_test.rs", main)]);
    assert_eq!(coded(change, "src/inner_test.rs"), Ok(()));

    let script = "fn main() {\n    println!(\"cargo:rerun-if-changed=build.rs\");\n}\n";
    let base = with(&[
        ("benches/b.rs", main),
        ("build.rs", script),
        ("README.md", "# a\n"),
    ]);
    for (path, want) in [
        ("build.rs", Doubt::OtherTarget(join(DIR, "build.rs"))),
        ("README.md", Doubt::NotRust(join(DIR, "README.md"))),
    ] {
        let text = format!("{}\n# changed\n", base.now[path]);
        assert_eq!(
            base.clone().with(path, Some(&text)).judge(),
            Err(want),
            "{path}"
        );
    }
    let bench_shipped = |extra: &str| {
        with(&[
            ("benches/b.rs", main),
            ("src/lib.rs", &format!("{LIB}{extra}")),
        ])
    };
    assert_eq!(
        coded(
            bench_shipped("#[path = \"../benches/b.rs\"]\nmod b;\n"),
            "benches/b.rs"
        ),
        shipped("benches/b.rs")
    );
    assert_eq!(
        coded(
            bench_shipped("pub const B: &str = include_str!(\"../benches/b.rs\");\n"),
            "benches/b.rs"
        ),
        Err(Doubt::Embedded(join(DIR, "benches/b.rs")))
    );
    assert_eq!(
        coded(
            with(&[("benches/common/helper.rs", main)]),
            "benches/common/helper.rs"
        ),
        shipped("benches/common/helper.rs")
    );
    for (manifest, path) in [
        ("autobenches = false\n", "benches/b.rs"),
        (
            "autobenches = false\n\n[[bench]]\nname = \"y\"\n",
            "benches/b.rs",
        ),
        ("autoexamples = false\n", "examples/e.rs"),
    ] {
        let manifest = format!("{MANIFEST}{manifest}");
        let change = with(&[("Cargo.toml", &manifest), (path, main)]);
        assert_eq!(coded(change, path), shipped(path), "{manifest} {path}");
    }
}

/// A source that reads another package's changed file, by a mount, an include or any literal,
/// is judged where it compiles: in code only the package's test, bench or example targets
/// compile, no build of a dependent reads it, and the package is confined. CONTROL: the same
/// reader in the library, in a file no declaration places, in a bench file the library also
/// mounts, or in the build script keeps the cone, and so does a build script that may read the
/// changed file, by a declared path, a literal spelled in pieces, or a build-dependency's code.
#[test]
fn a_reader_only_tests_benches_or_examples_compile_is_confined() {
    let mount = "#[path = \"../../t/src/x.rs\"]\nmod x;\n";
    let embed = "const S: &str = include_str!(\"../../t/src/x.rs\");\n";
    let reads = |files: &[(&str, &str)], reader: &str| {
        Change::services(files)
            .beside(FOREIGN, "pub fn x() {}\n")
            .judge_reads(&[reader], &[FOREIGN])
    };
    for (files, reader) in [
        (
            vec![("benches/b.rs", format!("{mount}fn main() {{}}\n"))],
            "benches/b.rs",
        ),
        (
            vec![("benches/b.rs", format!("{embed}fn main() {{}}\n"))],
            "benches/b.rs",
        ),
        (
            vec![("examples/e.rs", format!("{mount}fn main() {{}}\n"))],
            "examples/e.rs",
        ),
        (vec![("tests/it.rs", mount.to_string())], "tests/it.rs"),
        (
            vec![
                ("benches/b.rs", "mod support;\nfn main() {}\n".to_string()),
                ("benches/support/mod.rs", mount.to_string()),
            ],
            "benches/support/mod.rs",
        ),
        (
            vec![
                ("src/lib.rs", format!("{LIB}{MOUNTED}")),
                ("src/lib_tests.rs", embed.to_string()),
            ],
            "src/lib_tests.rs",
        ),
    ] {
        let files: Vec<(&str, &str)> = files.iter().map(|(p, t)| (*p, t.as_str())).collect();
        assert_eq!(reads(&files, reader), Ok(()), "{reader}");
    }
    // A reader that also changed is judged both ways.
    let edited = Change::services(&[("benches/b.rs", &format!("{mount}fn main() {{}}\n"))]).edit(
        "benches/b.rs",
        "fn main() {}",
        "fn main() { let _ = 1; }",
    );
    assert_eq!(edited.judge_reads(&["benches/b.rs"], &[FOREIGN]), Ok(()));

    let read_where = |rel: &str| Err(Doubt::Reads(join(DIR, rel)));
    assert_eq!(
        reads(&[("src/lib.rs", &format!("{LIB}{mount}"))], "src/lib.rs"),
        read_where("src/lib.rs")
    );
    assert_eq!(
        reads(
            &[("src/services.rs", &format!("{SERVICES}{embed}"))],
            "src/services.rs"
        ),
        read_where("src/services.rs")
    );
    assert_eq!(
        reads(&[("src/stray.rs", mount)], "src/stray.rs"),
        read_where("src/stray.rs")
    );
    let both = format!("{mount}fn main() {{}}\n");
    assert_eq!(
        reads(
            &[
                ("benches/b.rs", &both),
                (
                    "src/lib.rs",
                    &format!("{LIB}#[path = \"../benches/b.rs\"]\nmod b;\n")
                ),
            ],
            "benches/b.rs"
        ),
        read_where("benches/b.rs")
    );
    let unlexed = format!("{mount}fn main() {{ \"open }}\n");
    assert_eq!(
        reads(&[("benches/b.rs", &unlexed)], "benches/b.rs"),
        Err(Doubt::Unread(join(DIR, "benches/b.rs")))
    );

    // What the build script may read reaches every dependent, whichever file reads it.
    let bench = ("benches/b.rs", both.as_str());
    let script = |body: &str| {
        format!("fn main() {{\n    println!(\"cargo:rerun-if-changed=build.rs\");\n{body}}}\n")
    };
    let quiet = script("");
    assert_eq!(
        reads(&[bench, ("build.rs", &quiet)], "benches/b.rs"),
        Ok(())
    );
    let input = |path: &str| Err(Doubt::BuildInput(path.to_string()));
    // A `..` names the folder that holds every package.
    let up = script("    let _ = std::fs::read(std::path::Path::new(\"..\").join(name()));\n");
    assert_eq!(
        reads(&[bench, ("build.rs", &up)], "benches/b.rs"),
        input(&join(DIR, "benches/b.rs"))
    );
    let foreign = input(FOREIGN);
    for body in [
        "    println!(\"cargo:rerun-if-changed=../t/src/x.rs\");\n",
        "    let _ = std::fs::read(workspace().join(\"t/src/x.rs\"));\n",
        "    let _ = include_str!(\"../t/src/x.rs\");\n",
    ] {
        assert_eq!(
            reads(&[bench, ("build.rs", &script(body))], "benches/b.rs"),
            foreign,
            "{body}"
        );
    }
    let mounting = format!("#[path = \"../t/src/x.rs\"]\nmod x;\n{quiet}");
    assert_eq!(
        reads(&[bench, ("build.rs", &mounting)], "benches/b.rs"),
        foreign
    );
    // The script itself, or a bench the manifest makes the script, reads as the script.
    assert_eq!(
        reads(&[("build.rs", &mounting)], "build.rs"),
        input(&join(DIR, "build.rs"))
    );
    let as_script = format!("{MANIFEST}build = \"benches/b.rs\"\n");
    assert_eq!(
        reads(&[bench, ("Cargo.toml", &as_script)], "benches/b.rs"),
        input(&join(DIR, "benches/b.rs"))
    );
    // A build-dependency's code may read what no literal of the script shows.
    let with_deps =
        format!("{MANIFEST}\n[build-dependencies]\nhelper = {{ path = \"../helper\" }}\n");
    assert_eq!(
        reads(
            &[bench, ("build.rs", &quiet), ("Cargo.toml", &with_deps)],
            "benches/b.rs"
        ),
        input(&join(DIR, "benches/b.rs"))
    );
    // The same holds for a package whose only change is its own: a script that may read another
    // package's changed file keeps the cone.
    let own = Change::services(&[(
        "build.rs",
        &script("    println!(\"cargo:rerun-if-changed=../t/src/x.rs\");\n"),
    )])
    .edit("src/services.rs", "\"tester\");", "\"probe\");");
    assert_eq!(own.judge(), Ok(()), "alone, nothing else changed");
    assert_eq!(own.judge_reads(&[], &[FOREIGN]), foreign);
}

/// A member that spells another's changed path observes it by where the spelling sits: in test
/// code, a doc comment no macro reads, or a file only its test, bench or example targets compile,
/// only its own rows can see it (`Tests`); in a plain comment no macro reads, nothing reads it
/// (`Nothing`); in code its library, binaries or build script compile, a carried comment, a
/// comment in a file the library `include!`s, a comment in a file shipped code embeds by
/// `include_str!`, a file it cannot place or read, or a file its build script may read, a
/// dependent's test may run the code that reads it (`Linked`). Several spellings observe as the
/// most of them. CONTROL: the same plain comment in the library itself, or in the same module
/// when nothing embeds it, observes nothing.
#[test]
fn a_namer_observes_by_where_it_spells_the_path() {
    let spelled = format!("\"{FOREIGN}\"");
    let constant = format!("pub const P: &str = {spelled};\n");
    let observed = |files: &[(&str, &str)], rel: &str| {
        let change = Change::services(files);
        let by_path = observes(&change, DIR, &[change.spelling(rel)]);
        assert_eq!(by_path.len(), 1, "{by_path:?}");
        by_path[FOREIGN]
    };
    let lib = |extra: &str| format!("{LIB}{extra}");
    let tested = format!("#[cfg(test)]\nmod tests {{\n    const P: &str = {spelled};\n}}\n");
    let seam = format!("#[cfg(any(test, debug_assertions))]\npub const P: &str = {spelled};\n");
    let script = format!(
        "fn main() {{\n    println!(\"cargo:rerun-if-changed=build.rs\");\n    let _ = {spelled};\n}}\n"
    );
    let quiet = "fn main() {\n    println!(\"cargo:rerun-if-changed=build.rs\");\n}\n";
    let main = |body: &str| format!("{body}fn main() {{}}\n");
    for (files, rel, want) in [
        (
            vec![("tests/it.rs", constant.clone())],
            "tests/it.rs",
            Observed::Tests,
        ),
        (
            vec![("src/services.rs", format!("{SERVICES}{tested}"))],
            "src/services.rs",
            Observed::Tests,
        ),
        (
            vec![(
                "src/lib.rs",
                lib(&format!("/// See {FOREIGN}.\npub fn g() {{}}\n")),
            )],
            "src/lib.rs",
            Observed::Tests,
        ),
        (
            vec![("benches/b.rs", main(&constant))],
            "benches/b.rs",
            Observed::Tests,
        ),
        (
            vec![("examples/e.rs", main(&constant))],
            "examples/e.rs",
            Observed::Tests,
        ),
        (
            vec![("src/lib.rs", lib(&format!("// {FOREIGN}\n")))],
            "src/lib.rs",
            Observed::Nothing,
        ),
        (
            vec![("src/lib.rs", lib(&format!("/* {FOREIGN} */\n")))],
            "src/lib.rs",
            Observed::Nothing,
        ),
        (
            vec![("src/lib.rs", lib(&constant))],
            "src/lib.rs",
            Observed::Linked,
        ),
        (
            vec![("src/lib.rs", lib(&seam))],
            "src/lib.rs",
            Observed::Linked,
        ),
        (
            vec![("build.rs", script.clone())],
            "build.rs",
            Observed::Linked,
        ),
        (
            vec![("src/lib.rs", lib(&format!("m!(/* {FOREIGN} */);\n")))],
            "src/lib.rs",
            Observed::Linked,
        ),
        (
            vec![
                ("src/lib.rs", lib("#[x::y]\nmod r;\n")),
                ("src/r.rs", format!("/// See {FOREIGN}.\npub fn g() {{}}\n")),
            ],
            "src/r.rs",
            Observed::Linked,
        ),
        (
            vec![
                ("src/lib.rs", lib("include!(\"inc.rs\");\n")),
                ("src/inc.rs", format!("// {FOREIGN}\n")),
            ],
            "src/inc.rs",
            Observed::Linked,
        ),
        // A shipped module that embeds itself holds its own plain comment as data.
        (
            vec![
                ("src/lib.rs", lib("pub mod watched;\n")),
                (
                    "src/watched.rs",
                    format!("// {FOREIGN}\npub const SELF: &str = include_str!(\"watched.rs\");\n"),
                ),
            ],
            "src/watched.rs",
            Observed::Linked,
        ),
        // CONTROL: the same module that embeds nothing observes nothing.
        (
            vec![
                ("src/lib.rs", lib("pub mod watched;\n")),
                (
                    "src/watched.rs",
                    format!("// {FOREIGN}\npub const SELF: &str = \"\";\n"),
                ),
            ],
            "src/watched.rs",
            Observed::Nothing,
        ),
        (
            vec![("src/stray.rs", format!("// {FOREIGN}\n"))],
            "src/stray.rs",
            Observed::Linked,
        ),
        (
            vec![("src/stray.rs", constant.clone())],
            "src/stray.rs",
            Observed::Linked,
        ),
        (
            vec![("tests/it.rs", format!("{constant}fn open() {{ \"\n"))],
            "tests/it.rs",
            Observed::Linked,
        ),
        (
            vec![
                ("tests/it.rs", constant.clone()),
                (
                    "build.rs",
                    quiet.replace(
                        "build.rs\");",
                        "build.rs\");\n    let _ = include_str!(\"tests/it.rs\");",
                    ),
                ),
            ],
            "tests/it.rs",
            Observed::Linked,
        ),
        (
            vec![
                ("tests/it.rs", constant.clone()),
                ("build.rs", quiet.to_string()),
                (
                    "Cargo.toml",
                    format!("{MANIFEST}\n[build-dependencies]\nhelper = \"1\"\n"),
                ),
            ],
            "tests/it.rs",
            Observed::Linked,
        ),
        // CONTROL: the same with no build-dependency, and with a script that reads none of it.
        (
            vec![
                ("tests/it.rs", constant.clone()),
                ("build.rs", quiet.to_string()),
            ],
            "tests/it.rs",
            Observed::Tests,
        ),
        // Several spellings observe as the most of them.
        (
            vec![("src/lib.rs", lib(&format!("// {FOREIGN}\n{tested}")))],
            "src/lib.rs",
            Observed::Tests,
        ),
        (
            vec![("src/lib.rs", lib(&format!("{tested}{constant}")))],
            "src/lib.rs",
            Observed::Linked,
        ),
    ] {
        let files: Vec<(&str, &str)> = files.iter().map(|(p, t)| (*p, t.as_str())).collect();
        assert_eq!(observed(&files, rel), want, "{rel}: {files:?}");
    }
    // A manifest that does not parse observes as linked.
    let broken = Change::services(&[("Cargo.toml", "[package\n"), ("tests/it.rs", &constant)]);
    assert_eq!(
        observes(&broken, DIR, &[broken.spelling("tests/it.rs")]),
        BTreeMap::from([(FOREIGN.to_string(), Observed::Linked)])
    );
    // Each changed path observes as the most of its own spellings, one file's or several's: the
    // library spells one, its tests the other, and a plain comment a third. CONTROL: the same
    // spellings of one path observe as the library's.
    let other = "crates/t/src/y.rs";
    let quiet = "crates/t/src/z.rs";
    let paths = Change::services(&[(
        "src/lib.rs",
        &lib(&format!(
            "{constant}// {quiet}\n#[cfg(test)]\nmod tests {{\n    const Q: &str = \"{other}\";\n}}\n"
        )),
    )]);
    let spellings = [
        paths.spelling_of("src/lib.rs", FOREIGN),
        paths.spelling_of("src/lib.rs", other),
        paths.spelling_of("src/lib.rs", quiet),
    ];
    assert_eq!(
        observes(&paths, DIR, &spellings),
        BTreeMap::from([
            (FOREIGN.to_string(), Observed::Linked),
            (other.to_string(), Observed::Tests),
            (quiet.to_string(), Observed::Nothing),
        ])
    );
    let one: Vec<Spelling> = spellings
        .iter()
        .cloned()
        .map(|spelling| Spelling {
            path: FOREIGN.to_string(),
            ..spelling
        })
        .collect();
    assert_eq!(
        observes(&paths, DIR, &one),
        BTreeMap::from([(FOREIGN.to_string(), Observed::Linked)])
    );
    assert!(Observed::Nothing < Observed::Tests && Observed::Tests < Observed::Linked);
}

/// Integration targets and their modules are not linked into dependent crates. Negative
/// controls mount the same file as shipped tokens, embed its bytes, or let the build script
/// read it: its location must never hide any of those observations.
#[test]
fn integration_tests_are_confined_only_when_no_shipped_reader_observes_them() {
    let base = Change::services(&[
        ("tests/suite.rs", "mod cases;\n"),
        (
            "tests/cases/mod.rs",
            "pub fn value() -> u8 { 1 }\n#[test]\nfn it() {}\n",
        ),
    ]);
    let changed = |base: Change| base.edit("tests/cases/mod.rs", "{ 1 }", "{ 2 }");
    let observing = |path: &str, text: &str| {
        let mut observed = base.clone().with(path, Some(text));
        observed.before = observed.now.clone();
        observed
    };
    assert_eq!(changed(base.clone()).judge(), Ok(()));
    for reader in [
        "#[path = \"../tests/cases/mod.rs\"] mod cases;\n",
        "include!(\"../tests/cases/mod.rs\");\n",
        "const DATA: &str = include_str!(\"../tests/cases/mod.rs\");\n",
        "const PATH: &str = \"tests/cases/mod.rs\";\n",
    ] {
        assert!(
            changed(observing("src/lib.rs", reader)).judge().is_err(),
            "shipped reader {reader} must keep dependents"
        );
    }
    let build = "fn main() { println!(\"cargo::rerun-if-changed=tests\"); }\n";
    assert!(changed(observing("build.rs", build)).judge().is_err());
    // A test path can also be a binary root, even outside src/: both builds must be read.
    let dual = format!("{MANIFEST}\n[[bin]]\nname='dual'\npath='tests/cases/mod.rs'\n");
    let mut dual_base = base.clone();
    dual_base.before.insert("Cargo.toml".into(), dual.clone());
    dual_base.now.insert("Cargo.toml".into(), dual);
    assert_eq!(changed(dual_base).judge(), shipped("tests/cases/mod.rs"));
    // A file not reached by a test root remains unknown, rather than being trusted by folder.
    let orphan = Change::services(&[("tests/cases/orphan.rs", "fn orphan() {}\n")]).edit(
        "tests/cases/orphan.rs",
        "fn orphan() {}",
        "fn orphan() { panic!(); }",
    );
    assert_eq!(orphan.judge(), shipped("tests/cases/orphan.rs"));
}

/// With discovery disabled, folder placement is no evidence that Cargo owns a file as a test.
/// An explicit test root restores that evidence, including for the modules it mounts.
#[test]
fn disabled_test_discovery_requires_an_explicit_root() {
    let manifest = format!("{MANIFEST}autotests = false\n");
    let base = Change::services(&[
        ("Cargo.toml", &manifest),
        ("tests/suite.rs", "mod cases;\n"),
        ("tests/cases/mod.rs", "pub fn value() -> u8 { 1 }\n"),
    ]);
    assert_eq!(
        base.clone()
            .edit("tests/cases/mod.rs", "{ 1 }", "{ 2 }")
            .judge(),
        shipped("tests/cases/mod.rs")
    );
    let explicit = format!("{manifest}\n[[test]]\nname='suite'\npath='tests/suite.rs'\n");
    let mut declared = base;
    declared
        .before
        .insert("Cargo.toml".into(), explicit.clone());
    declared.now.insert("Cargo.toml".into(), explicit);
    assert_eq!(
        declared
            .edit("tests/cases/mod.rs", "{ 1 }", "{ 2 }")
            .judge(),
        Ok(())
    );
}

/// A deleted file that only tests mounted is confined; a deleted shipped one is not.
#[test]
fn a_deleted_file_is_judged_where_it_was() {
    let lib = format!("{LIB}\n{MOUNTED}");
    let gone = Change::services(&[("src/lib.rs", &lib), ("src/lib_tests.rs", LIB_TESTS)])
        .with("src/lib.rs", Some(LIB))
        .with("src/lib_tests.rs", None);
    assert_eq!(gone.judge(), Ok(()));
    let shipped_gone = Change::services(&[]).with("src/services.rs", None);
    assert_eq!(shipped_gone.judge(), shipped("src/services.rs"));
}

/// A file the reader cannot lex is a doubt where a build without tests may read it.
#[test]
fn a_shipped_file_that_does_not_lex_is_a_doubt() {
    let broken = Change::services(&[]).edit("src/services.rs", "\"tester\");", "\"tester);");
    assert_eq!(
        broken.judge(),
        Err(Doubt::Unread(join(DIR, "src/services.rs")))
    );
    let unreadable_manifest = Change::services(&[("Cargo.toml", "[package\n")]).edit(
        "src/services.rs",
        "\"tester\");",
        "\"probe\");",
    );
    assert_eq!(
        unreadable_manifest.judge(),
        Err(Doubt::Unread(join(DIR, "Cargo.toml")))
    );
}

#[test]
fn binaries_ship_their_module_trees() {
    let bin = "mod cli;\nfn main() {}\n";
    let cli = "pub fn run() {}\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn runs() {}\n}\n";
    for root in ["src/main.rs", "src/bin/tool.rs", "src/bin/tool/main.rs"] {
        let folder = parent(root);
        let cli_path = join(folder, "cli.rs");
        let change = Change::of(&[(root, bin), (cli_path.as_str(), cli)]);
        assert_eq!(
            change
                .clone()
                .edit(&cli_path, "fn runs() {}", "fn runs_again() {}")
                .judge(),
            Ok(()),
            "{root}"
        );
        assert_eq!(
            change
                .edit(&cli_path, "pub fn run() {}", "pub fn run() { }")
                .judge(),
            shipped(&cli_path),
            "{root}"
        );
    }
}

/// A plain comment is no token: editing one in shipped code is confined. CONTROL: the same edit to
/// a shipped literal is not.
#[test]
fn a_plain_comment_edit_is_confined() {
    let commented = SERVICES.replace("    \"tester\"\n", "    // The name.\n    \"tester\"\n");
    let base = Change::services(&[("src/services.rs", &commented)]);
    let edit = |from, to| base.clone().edit("src/services.rs", from, to).judge();
    assert_eq!(edit("// The name.", "// The tester's name."), Ok(()));
    assert_eq!(edit("// The name.", "/* The name. */"), Ok(()));
    assert_eq!(
        edit("    \"tester\"\n", "    \"probe\"\n"),
        shipped("src/services.rs")
    );
}

/// A doc comment no macro reads reaches no dependent's build: on a `pub fn`, a file's `//!`, or a
/// plain comment that becomes a doc. CONTROL: a doc that grows a line moves the code after it.
#[test]
fn a_doc_comment_no_macro_reads_is_confined() {
    let documented = SERVICES.replace(
        "pub fn tester",
        "/// The tester.\n#[must_use]\npub fn tester",
    );
    let base = Change::services(&[("src/services.rs", &documented)]);
    let edit = |from, to| base.clone().edit("src/services.rs", from, to).judge();
    assert_eq!(edit("/// The tester.", "/// The one tester."), Ok(()));
    assert_eq!(edit("//! Services.", "//! What serves."), Ok(()));
    assert_eq!(edit("/// The tester.", "// The tester."), Ok(()));
    assert_eq!(
        edit("/// The tester.", "/// The tester.\n/// More."),
        shipped("src/services.rs")
    );
}

/// A `// SAFETY:` comment rewritten in place is confined: only the member's own lint reads it, and
/// a member judged alone keeps its lint. CONTROL: deleting its line moves the code after it.
#[test]
fn a_safety_comment_rewritten_in_place_is_confined() {
    let unsafe_fn = "pub fn read(p: *const u8) -> u8 {\n    // SAFETY: the caller passes a live byte.\n    unsafe { *p }\n}\n";
    let base = Change::services(&[("src/services.rs", unsafe_fn)]);
    let rewritten = base
        .clone()
        .edit("src/services.rs", "// SAFETY: the caller", "// the caller");
    assert_eq!(rewritten.judge(), Ok(()));
    let deleted = base.edit(
        "src/services.rs",
        "    // SAFETY: the caller passes a live byte.\n",
        "",
    );
    assert_eq!(deleted.judge(), shipped("src/services.rs"));
}

fn carried(rel: &str) -> Result<(), Doubt> {
    Err(Doubt::Carried(join(DIR, rel)))
}

/// A comment a macro may read keeps the cone, one case per carrier. CONTROL: each without its
/// carrier is confined.
#[test]
fn a_comment_a_macro_may_read_keeps_the_cone() {
    for (with, without) in [
        (
            "/// A record.\n#[derive(Debug)]\npub struct R;\n",
            "/// A record.\n#[must_use]\npub struct R;\n",
        ),
        (
            "#[derive(Debug)]\npub struct R {\n    /// A record.\n    pub a: u8,\n}\n",
            "pub struct R {\n    /// A record.\n    pub a: u8,\n}\n",
        ),
        (
            "macro_rules! r {\n    () => {\n        // A record.\n    };\n}\n",
            "pub fn r() {\n    // A record.\n}\n",
        ),
        (
            "aterm_spec::ty_model! {\n    // A record.\n    model R {}\n}\n",
            "pub mod r {\n    // A record.\n    pub struct R;\n}\n",
        ),
        (
            "/// A record.\n#[cfg_attr(any(test, debug_assertions), aterm_spec_anchor::refines(m))]\npub fn r() {}\n",
            "/// A record.\n#[cfg_attr(trust_verify, trust::skip)]\npub fn r() {}\n",
        ),
        (
            "/// A record.\n#[doc = \"More.\"]\npub fn r() {}\n",
            "/// A record.\n#[doc(alias = \"rec\")]\npub fn r() {}\n",
        ),
    ] {
        for (text, want) in [(with, carried("src/services.rs")), (without, Ok(()))] {
            let change = Change::services(&[("src/services.rs", text)]).edit(
                "src/services.rs",
                "// A record.",
                "// The record.",
            );
            assert_eq!(change.judge(), want, "{text}");
        }
    }
    // A file a declaration a macro may read mounts, or that no declaration places.
    let doc_edit = |change: Change, path: &str| {
        change
            .edit(path, "//! Services.", "//! The services.")
            .judge()
    };
    let mounted = "#[some::wrap]\npub mod services;\n\npub fn f() -> u8 {\n    1\n}\n";
    assert_eq!(
        doc_edit(
            Change::services(&[("src/lib.rs", mounted)]),
            "src/services.rs"
        ),
        carried("src/services.rs")
    );
    let unplaced = format!("{LIB}elsewhere::declare!(hidden);\n");
    assert_eq!(
        doc_edit(
            Change::services(&[("src/lib.rs", &unplaced), ("src/hidden.rs", SERVICES)]),
            "src/hidden.rs"
        ),
        carried("src/hidden.rs")
    );
    // A file a test-only declaration placed before and none places now: a macro may declare it.
    let tested = format!("{LIB}#[cfg(test)]\nmod x;\n");
    assert_eq!(
        doc_edit(
            Change::services(&[("src/lib.rs", &tested), ("src/x.rs", SERVICES)])
                .with("src/lib.rs", Some(LIB)),
            "src/x.rs"
        ),
        carried("src/x.rs")
    );
    // A declaration whose path this reader cannot settle, and a plain one in a file a carried
    // declaration mounts. (Neither path literal names the file, which would be [`Doubt::Named`].)
    let unsettled = "#[cfg_attr(unix, path = \"elsewhere.rs\")]\npub mod services;\n\npub fn f() -> u8 {\n    1\n}\n";
    assert_eq!(
        doc_edit(
            Change::services(&[("src/lib.rs", unsettled)]),
            "src/services.rs"
        ),
        carried("src/services.rs")
    );
    let outer = "#[some::wrap]\npub mod outer;\n\npub fn f() -> u8 {\n    1\n}\n";
    assert_eq!(
        doc_edit(
            Change::services(&[
                ("src/lib.rs", outer),
                ("src/outer.rs", "pub mod services;\n")
            ]),
            "src/services.rs"
        ),
        carried("src/services.rs")
    );
    assert_eq!(doc_edit(Change::services(&[]), "src/services.rs"), Ok(()));
}

/// A comment is carried where a carrier of either version holds it: a doc that becomes a plain
/// comment before a derive leaves the derive's input in the old version only, and the reverse in
/// the new one only. CONTROL: the same flips before an inert attribute are confined.
#[test]
fn a_comment_either_versions_carrier_holds_keeps_the_cone() {
    let item = |attr: &str| format!("/// A record.\n#[{attr}]\npub struct R;\n");
    let field = "#[derive(Debug)]\npub struct R {\n    /// A record.\n    pub a: u8,\n}\n";
    for (from, to) in [("/// A", "// A"), ("// A", "/// A")] {
        let flip = |text: &str| {
            let text = text.replace("/// A", from);
            Change::services(&[("src/services.rs", &text)])
                .edit("src/services.rs", from, to)
                .judge()
        };
        assert_eq!(
            flip(&item("derive(Debug)")),
            carried("src/services.rs"),
            "{from}"
        );
        assert_eq!(flip(field), carried("src/services.rs"), "{from}");
        assert_eq!(flip(&item("must_use")), Ok(()), "{from}");
    }
}

/// A Trust `clean { … }` island is Lean to the compiler: a span in it that lexes as a Rust comment
/// is code there, so its edit keeps the cone. CONTROL: the same text in a Rust function body is
/// a comment.
#[test]
fn an_islands_comment_shaped_text_keeps_the_cone() {
    let lib = "#[cfg(clean_islands)]\n#[rustfmt::skip]\n#[path = \"x_island.rs\"]\nmod x_island;\n\npub fn f() -> u8 {\n    1\n}\n";
    let text = "    -- glob src/*.rs\n    def v (x : UInt64) : UInt64 := x + 1\n    -- end */\n";
    for (island, want) in [
        (format!("clean {{\n{text}}}\n"), carried("src/x_island.rs")),
        (format!("pub fn g() {{\n{text}}}\n"), Ok(())),
    ] {
        let change = Change::of(&[("src/lib.rs", lib), ("src/x_island.rs", &island)]).edit(
            "src/x_island.rs",
            "x + 1",
            "x + 2",
        );
        assert_eq!(change.judge(), want, "{island}");
    }
}

/// The tokens of a file an `include!` reads keep the cone when its comments change. CONTROL: a
/// change inside its test module is confined.
#[test]
fn an_included_files_comment_edit_keeps_the_cone() {
    let lib = "include!(\"services.rs\");\n\npub fn f() -> u8 {\n    1\n}\n";
    let base = Change::services(&[("src/lib.rs", lib)]);
    assert_eq!(
        base.clone()
            .edit("src/services.rs", "//! Services.", "//! The services.")
            .judge(),
        Err(Doubt::Included(join(DIR, "src/services.rs")))
    );
    assert_eq!(
        base.edit("src/services.rs", "\"tester\");", "\"probe\");")
            .judge(),
        Ok(())
    );
}

/// A member's own manifest whose TOML values are the same before and after is confined. CONTROL:
/// a value that changes, a shipped literal that names it, or a build script that may read it
/// keeps the cone.
#[test]
fn a_member_manifest_whose_values_are_the_same_is_confined() {
    let manifest = |files: &[(&str, &str)], to: &str| {
        Change::services(files).with("Cargo.toml", Some(to)).judge()
    };
    let commented = format!("# The package.\n{MANIFEST}");
    let respelled = MANIFEST.replace("name = \"a\"", "name = 'a'");
    assert_eq!(manifest(&[], &commented), Ok(()));
    assert_eq!(manifest(&[], &respelled), Ok(()));
    let doubt = |doubt: fn(String) -> Doubt| Err(doubt(join(DIR, "Cargo.toml")));
    assert_eq!(
        manifest(&[], &MANIFEST.replace("0.1.0", "0.2.0")),
        doubt(Doubt::NotRust)
    );
    let named = format!("{LIB}pub const M: &str = \"Cargo.toml\";\n");
    assert_eq!(
        manifest(&[("src/lib.rs", &named)], &commented),
        doubt(Doubt::Named)
    );
    let walker = "fn main() {\n    println!(\"cargo:rerun-if-changed=build.rs\");\n    walk(std::env::var(\"CARGO_MANIFEST_DIR\").unwrap());\n}\n";
    assert_eq!(
        manifest(&[("build.rs", walker)], &commented),
        doubt(Doubt::BuildInput)
    );
    // Another file's manifest is no member's own.
    let other =
        Change::services(&[("README.toml", "a = 1\n")]).with("README.toml", Some("# x\na = 1\n"));
    assert_eq!(other.judge(), Err(Doubt::NotRust(join(DIR, "README.toml"))));
}
