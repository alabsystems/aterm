// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;
use aterm_types::scheme_import::{self, MAX_IMPORT_BYTES};

/// An answer whose stdout cannot be written still says its stderr, then why the stdout was
/// lost, and exits 1. NEGATIVE CONTROL: a written stdout keeps the outcome's own code and
/// adds nothing to its stderr.
#[test]
fn an_unwritten_answer_still_says_its_stderr() {
    let outcome = Outcome {
        stdout: "a list\n".into(),
        stderr: "aterm theme: a warning\n".into(),
        code: 3,
    };
    for writes in [false, true] {
        let out = std::cell::RefCell::new(String::new());
        let err = std::cell::RefCell::new(String::new());
        let code = delivered(
            &outcome,
            &|text| {
                if !writes {
                    return Err(std::io::Error::other("the disk is full"));
                }
                out.borrow_mut().push_str(text);
                Ok(())
            },
            &|text| err.borrow_mut().push_str(text),
        );
        if writes {
            assert_eq!((code, out.take()), (3, "a list\n".to_owned()));
            assert_eq!(err.take(), "aterm theme: a warning\n");
        } else {
            assert_eq!(code, 1);
            assert_eq!(
                err.take(),
                "aterm theme: a warning\n\
                 aterm theme: the answer was not written (the disk is full)\n"
            );
        }
    }
}

struct ScratchDir(aterm_tempfile::TempDir);

impl std::ops::Deref for ScratchDir {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        self.0.path()
    }
}

impl AsRef<Path> for ScratchDir {
    fn as_ref(&self) -> &Path {
        self.0.path()
    }
}

fn scratch(tag: &str) -> ScratchDir {
    ScratchDir(
        aterm_tempfile::Builder::new()
            .prefix(&format!("aterm-cli-theme-{tag}-"))
            .tempdir()
            .expect("scratch dir"),
    )
}

fn args(list: &[&str]) -> Vec<OsString> {
    list.iter().map(OsString::from).collect()
}

fn never_lists(_: &ThemeListRequest) -> String {
    panic!("only `list` lists")
}

fn echo(request: &ThemeListRequest) -> String {
    format!("{request:?}")
}

/// A test host: the listing is `.0`, the catalog is the built-ins, and
/// the classifier is the window's one dark/light rule.
pub(crate) struct Host(pub(crate) fn(&ThemeListRequest) -> String);

impl ThemeHost for Host {
    fn list(&self, request: &ThemeListRequest) -> String {
        (self.0)(request)
    }

    fn lookup(&self, id: &str) -> Result<aterm_types::scheme::CatalogTheme, String> {
        aterm_types::scheme::CatalogTheme::builtin(id)
            .ok_or_else(|| "no built-in theme by that name".to_string())
    }

    fn measure(&self, background: aterm_types::Rgb) -> Appearance {
        if aterm_messages::ink::bg_is_light([background.r, background.g, background.b]) {
            Appearance::Light
        } else {
            Appearance::Dark
        }
    }
}

pub(crate) const NEVER_LISTS: Host = Host(never_lists);
const ECHO: Host = Host(echo);

fn listed(argv: &[&str]) -> Outcome {
    run(&args(argv), None, &ECHO)
}

#[test]
fn list_passes_its_filter_and_format_to_the_listing() {
    let plain = listed(&["list"]);
    assert_eq!(plain.code, 0, "{plain:?}");
    let expect = |appearance, format| format!("{:?}", ThemeListRequest { appearance, format });
    assert_eq!(plain.stdout, expect(None, ThemeListFormat::Text));
    for argv in [
        vec!["list", "--appearance", "light", "--format", "tsv"],
        vec!["list", "--format=tsv", "--appearance=light"],
    ] {
        let out = listed(&argv);
        assert_eq!(out.code, 0, "{argv:?}: {out:?}");
        assert_eq!(
            out.stdout,
            expect(Some(Appearance::Light), ThemeListFormat::Tsv),
            "{argv:?}"
        );
    }
    let dark = listed(&["list", "--appearance", "dark", "--format", "text"]);
    assert_eq!(
        dark.stdout,
        expect(Some(Appearance::Dark), ThemeListFormat::Text)
    );
}

#[test]
fn list_refuses_what_it_does_not_take() {
    for argv in [
        vec!["list", "--json"],
        vec!["list", "extra"],
        vec!["list", "--appearance", "blue"],
        vec!["list", "--appearance"],
        vec!["list", "--format", "csv"],
        vec!["list", "--format="],
        vec!["list", "--help=extra"],
        vec!["list", "--appearance", "dark", "--appearance", "light"],
        vec!["list", "--format", "tsv", "--format", "text"],
    ] {
        let out = run(&args(&argv), None, &NEVER_LISTS);
        assert_eq!(out.code, 2, "{argv:?}: {out:?}");
        assert!(out.stderr.contains(USAGE), "{argv:?}");
        assert!(out.stdout.is_empty(), "{argv:?}");
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt as _;
        let bad = OsString::from_vec(vec![0xff]);
        let out = run(
            &[OsString::from("list"), OsString::from("--appearance"), bad],
            None,
            &NEVER_LISTS,
        );
        assert_eq!(out.code, 2, "{out:?}");
        assert!(out.stderr.contains("is not UTF-8"), "{}", out.stderr);
    }
}

#[test]
fn list_themes_is_the_theme_list_spelling() {
    let rest = args(&["--format", "tsv"]);
    let aliased = list_themes_alias("list-themes", &rest).expect("the alias");
    assert_eq!(aliased, args(&["list", "--format", "tsv"]));
    assert_eq!(
        run(&aliased, None, &ECHO).stdout,
        listed(&["list", "--format", "tsv"]).stdout
    );
    for other in ["theme", "list", "list-fonts", ""] {
        assert!(list_themes_alias(other, &rest).is_none(), "{other:?}");
    }
}

const KITTY: &str = "## name: Harbour\nforeground #c0caf5\nbackground #1a1b26\n\
                     url_color #73daca\ncolor1 #f7768e\n";

#[test]
fn import_writes_a_native_theme_that_reads_back() {
    let root = scratch("import");
    let source = root.join("harbour.conf");
    std::fs::write(&source, KITTY).unwrap();
    let themes = root.join("themes");
    let out = run(
        &args(&["import", source.to_str().unwrap()]),
        Some(&themes),
        &NEVER_LISTS,
    );
    assert_eq!(out.code, 0, "{out:?}");
    assert!(out.stdout.contains("url_color"), "{}", out.stdout);
    let written = std::fs::read_to_string(themes.join("harbour.conf")).unwrap();
    let parsed = scheme::parse_theme_str(&written).unwrap();
    let converted = scheme_import::convert(scheme_import::ImportFormat::Kitty, KITTY, None)
        .unwrap()
        .scheme;
    assert_eq!(parsed.scheme, converted);
    assert_eq!(parsed.scheme.name, "Harbour");
    let entries: Vec<_> = std::fs::read_dir(&themes).unwrap().collect();
    assert_eq!(entries.len(), 1);
    let _ = std::fs::remove_dir_all(&root);
}

fn printable(text: &str) -> bool {
    !text.chars().any(|c| c.is_control() && c != '\n')
}

#[test]
fn skipped_key_names_reach_the_terminal_escaped() {
    let root = scratch("skipped-escaped");
    for (file, key, listed) in [
        ("evil.conf", "\x1b]0;PWNED\x07", "\\u{1b}]0;pwned\\u{7}"),
        ("plain.conf", "future_key", "future_key"),
    ] {
        let source = root.join(file);
        std::fs::write(
            &source,
            format!(
                "name = Evil\nschema = 999\nbackground = #000000\n\
                 foreground = #ffffff\n{key} = 1\n"
            ),
        )
        .unwrap();
        let path = source.to_str().unwrap();
        let checked = run(&args(&["check", path]), None, &NEVER_LISTS);
        let imported = run(
            &args(&["import", path, "--replace"]),
            Some(&root.join("themes")),
            &NEVER_LISTS,
        );
        for out in [&checked, &imported] {
            let text = format!("{}{}", out.stdout, out.stderr);
            assert!(printable(&text), "{file}: {text:?}");
            assert!(text.contains(listed), "{file}: {text:?}");
        }
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_existing_theme_is_kept_unless_replace() {
    let root = scratch("replace");
    let source = root.join("harbour.conf");
    std::fs::write(&source, KITTY).unwrap();
    let themes = root.join("themes");
    std::fs::create_dir_all(&themes).unwrap();
    let dest = themes.join("mine.conf");
    std::fs::write(&dest, "old").unwrap();
    let src = source.to_str().unwrap();
    let refused = run(
        &args(&["import", src, "--name", "mine"]),
        Some(&themes),
        &NEVER_LISTS,
    );
    assert_eq!(refused.code, 1, "{refused:?}");
    assert!(refused.stderr.contains("--replace"));
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), "old");
    let replaced = run(
        &args(&["import", src, "--name=mine", "--replace"]),
        Some(&themes),
        &NEVER_LISTS,
    );
    assert_eq!(replaced.code, 0, "{replaced:?}");
    let parsed = scheme::parse_theme_str(&std::fs::read_to_string(&dest).unwrap()).unwrap();
    assert_eq!(parsed.scheme.name, "mine");
    let literal_prefix = run(
        &args(&["import", src, "--name=--name=mine"]),
        Some(&themes),
        &NEVER_LISTS,
    );
    assert_eq!(literal_prefix.code, 0, "{literal_prefix:?}");
    assert_eq!(
        scheme::parse_theme_str(&std::fs::read_to_string(themes.join("--name=mine.conf")).unwrap())
            .unwrap()
            .scheme
            .name,
        "--name=mine"
    );
    assert_eq!(std::fs::read_dir(&themes).unwrap().count(), 2);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn urls_builtin_names_and_bad_ids_are_refused_before_reading() {
    let root = scratch("refuse");
    let themes = root.join("themes");
    for argv in [
        vec!["import", "https://example.com/theme.conf"],
        vec!["import", "/nonexistent/Nord.conf"],
        vec!["import", "/nonexistent/x.conf", "--name", "default"],
        vec!["import", "/nonexistent/x.conf", "--name", "dark:x"],
        vec!["import", "/nonexistent/x.conf", "--name", "a/b"],
        vec!["import", "/nonexistent/x.conf", "--name="],
        vec![
            "import",
            "/nonexistent/x.conf",
            "--name",
            "a",
            "--name",
            "b",
        ],
        vec!["import"],
        vec!["import", "a", "b"],
        vec!["import", "a", "--bogus"],
        vec!["frobnicate"],
        vec![],
    ] {
        let out = run(&args(&argv), Some(&themes), &NEVER_LISTS);
        assert_eq!(out.code, 2, "{argv:?}: {out:?}");
        assert!(out.stderr.contains(USAGE), "{argv:?}");
    }
    assert!(!themes.exists(), "a refusal must write nothing");
    let out = run(
        &args(&["import", "/nonexistent/x.conf"]),
        Some(&themes),
        &NEVER_LISTS,
    );
    assert_eq!(out.code, 1, "{out:?}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn oversize_and_unknown_files_fail_and_write_nothing() {
    let root = scratch("oversize");
    let themes = root.join("themes");
    let big = root.join("big.conf");
    std::fs::write(&big, vec![b'\n'; MAX_IMPORT_BYTES + 1]).unwrap();
    let out = run(
        &args(&["import", big.to_str().unwrap()]),
        Some(&themes),
        &NEVER_LISTS,
    );
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stderr.contains("at most"), "{}", out.stderr);
    let prose = root.join("prose.txt");
    std::fs::write(&prose, "hello there\n").unwrap();
    let out = run(
        &args(&["import", prose.to_str().unwrap()]),
        Some(&themes),
        &NEVER_LISTS,
    );
    assert_eq!(out.code, 1, "{out:?}");
    assert!(!themes.exists());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn help_answers_on_stdout() {
    for argv in [
        vec!["help"],
        vec!["--help"],
        vec!["import", "--help"],
        vec!["list", "--help"],
    ] {
        let out = run(&args(&argv), None, &NEVER_LISTS);
        assert_eq!(out.code, 0);
        assert!(out.stdout.contains("aterm theme import"), "{argv:?}");
        assert!(out.stdout.contains("aterm theme list"), "{argv:?}");
    }
}

/// Non-UTF-8 `--name=<bytes>` and unknown `-...` flags are refused as usage errors (exit 2)
/// instead of falling through to the positional file slot.
/// NEGATIVE CONTROL: matching only `arg.to_str()` treats `--name=\xff` as a file path and
/// exits 1 on `open` instead of 2 with `USAGE`.
#[cfg(unix)]
#[test]
fn non_utf8_flags_are_refused_as_usage_errors() {
    use std::os::unix::ffi::OsStringExt as _;

    let root = scratch("non-utf8-flags");
    let themes = root.join("themes");
    let bad_inline_name = vec![
        OsString::from("import"),
        OsString::from("/nonexistent/x.conf"),
        OsString::from_vec(b"--name=\xff".to_vec()),
    ];
    let out = run(&bad_inline_name, Some(&themes), &NEVER_LISTS);
    assert_eq!(out.code, 2, "{out:?}");
    assert!(out.stderr.contains("--name must be UTF-8"), "{out:?}");
    assert!(out.stderr.contains(USAGE), "{out:?}");

    let bad_flag = vec![
        OsString::from("import"),
        OsString::from_vec(b"--bogus\xff".to_vec()),
    ];
    let out = run(&bad_flag, Some(&themes), &NEVER_LISTS);
    assert_eq!(out.code, 2, "{out:?}");
    assert!(out.stderr.contains("unknown option"), "{out:?}");
    assert!(out.stderr.contains(USAGE), "{out:?}");
    assert!(!themes.exists());
    let _ = std::fs::remove_dir_all(&root);
}
