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
        }
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

    fn judge(&self) -> Result<(), Doubt> {
        let changed: Vec<String> = self
            .before
            .keys()
            .chain(self.now.keys())
            .filter(|path| self.before.get(*path) != self.now.get(*path))
            .map(|path| join(DIR, path))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        assert!(!changed.is_empty(), "the fixture changes something");
        let changed: Vec<&str> = changed.iter().map(String::as_str).collect();
        confined(self, DIR, &changed)
    }
}

impl Files for Change {
    fn under(&self, dir: &str) -> Vec<String> {
        self.now
            .keys()
            .map(|path| join(DIR, path))
            .filter(|path| within(dir, path).is_some())
            .collect()
    }

    fn now(&self, path: &str) -> Result<Option<String>, String> {
        Ok(within(DIR, path).and_then(|rel| self.now.get(rel).cloned()))
    }

    fn before(&self, path: &str) -> Result<Option<String>, String> {
        Ok(within(DIR, path).and_then(|rel| self.before.get(rel).cloned()))
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
        .edit("src/services.rs", "\"tester\");", "\"probe\");")
        .edit("src/services.rs", "//! Services.", "//! The services.");
    assert_eq!(change.judge(), shipped("src/services.rs"));
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

#[test]
fn files_of_other_targets_and_no_source_keep_the_cone() {
    let base = Change::services(&[
        ("tests/it.rs", "#[test]\nfn it() {}\n"),
        ("benches/b.rs", "fn main() {}\n"),
        ("examples/e.rs", "fn main() {}\n"),
        (
            "build.rs",
            "fn main() {\n    println!(\"cargo:rerun-if-changed=build.rs\");\n}\n",
        ),
        ("README.md", "# a\n"),
    ]);
    for (path, want) in [
        (
            "benches/b.rs",
            Doubt::OtherTarget(join(DIR, "benches/b.rs")),
        ),
        (
            "examples/e.rs",
            Doubt::OtherTarget(join(DIR, "examples/e.rs")),
        ),
        ("build.rs", Doubt::OtherTarget(join(DIR, "build.rs"))),
        ("README.md", Doubt::NotRust(join(DIR, "README.md"))),
        ("Cargo.toml", Doubt::NotRust(join(DIR, "Cargo.toml"))),
    ] {
        let text = format!("{}\n# changed\n", base.now[path]);
        assert_eq!(
            base.clone().with(path, Some(&text)).judge(),
            Err(want),
            "{path}"
        );
    }
    let custom = format!("{MANIFEST}\n[[test]]\nname = \"inner\"\npath = \"src/inner_test.rs\"\n");
    let change = Change::services(&[
        ("Cargo.toml", &custom),
        ("src/inner_test.rs", "fn main() {}\n"),
    ])
    .with("src/inner_test.rs", Some("fn main() { let _ = 1; }\n"));
    assert_eq!(change.judge(), Ok(()));
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
