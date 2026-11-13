// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_doctest_is_a_block_rustdoc_compiles_in_a_doc_comment() {
    for yes in [
        "/// ```\n/// f();\n/// ```\n",
        "    //! ```rust\n    //! ```\n",
        "/// ```no_run\n/// ```\n",
        "/// ```edition2021,should_panic\n/// ```\n",
        "/// ```compile_fail,E0308\n/// let x: u8 = \"\";\n/// ```\n",
        "/// ```ignore-wasm32\n/// ```\n",
        "/// ```custom_word\n/// ```\n",
        "/// ~~~\n/// f();\n/// ~~~\n",
        "/// ````\n/// f();\n/// ````\n",
        "/**\n * ```\n * f();\n * ```\n */\n",
        "/*!\nText.\n\n```\nf();\n```\n*/\n",
        "#![doc = include_str!(\"../README.md\")]\n",
        "#[cfg_attr(doc, doc = include_str!(\"x.md\"))]\n",
        "#[doc = concat!(\"a\", $b)]\n",
        "#[doc = \"```\"]\n",
        "/// ```text\n/// x\n/// ```\n/// ```\n/// f();\n/// ```\n",
        "/// Text.\n///\n///     let x = 1;\n",
        "///     let x = 1;\n",
        "/// * item\n///\n///       code_in_the_item();\n",
    ] {
        assert!(docs_hold_a_doctest(yes), "{yes:?}");
    }
    for no in [
        "/// ```text\n/// x\n/// ```\n",
        "/// ```sh\n/// ls\n/// ```\n/// after\n",
        "/// ```rust,ignore\n/// ```\n",
        "/// ```ignore\n/// ```\n",
        "/// ```notest\n/// ```\n",
        "// ```\n// f();\n// ```\n",
        "//// ```\n//// f();\n//// ```\n",
        "/*\n```\nf();\n```\n*/\n",
        "let fence = \"```\";\n",
        "#[doc = \"plain\"]\n",
        "#[doc(hidden)]\n#[cfg(doc)]\n",
        "/// ```inline``` code\n",
        "/// Text\n///     continued, not a block.\n",
        "/// 1. item\n///\n///    more of the item.\n",
        "/// ```text\n///     indented inside a fence\n/// ```\n",
        "    #[doc = \\\" A \\\"] pub struct InAString;\n",
    ] {
        assert!(!docs_hold_a_doctest(no), "{no:?}");
    }
}

#[test]
fn a_file_pulls_in_its_path_and_include_targets() {
    let text = "#[path = \"../../w/src/a.rs\"]\nmod a;\n  include!(\"b.rs\");\n\
                let s = \"include!(\\\"no\\\")\";\n";
    assert_eq!(
        pulled_in(text),
        Some(vec![
            Pull::Beside("../../w/src/a.rs".to_string()),
            Pull::Beside("b.rs".to_string())
        ])
    );
    assert_eq!(
        pulled_in("include!(concat!(\n    env!(\"CARGO_MANIFEST_DIR\"),\n    \"/src/t.rs\"\n));\n"),
        Some(vec![Pull::InCrate("/src/t.rs".to_string())])
    );
    assert_eq!(
        pulled_in("include!(concat!(env!(\"OUT_DIR\"), \"/g.rs\"));\n"),
        None
    );
}

#[test]
fn a_doctest_in_a_file_pulled_in_from_another_crate_counts() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let src = tmp.path().join("a/src");
    let other = tmp.path().join("b/src");
    std::fs::create_dir_all(&src).expect("mkdir");
    std::fs::create_dir_all(&other).expect("mkdir");
    std::fs::write(
        other.join("x.rs"),
        "/// ```\n/// f();\n/// ```\npub fn f() {}\n",
    )
    .expect("write");
    std::fs::write(src.join("lib.rs"), "//! No doctest here.\n").expect("write");
    assert!(!src_holds_a_doctest(&src));
    std::fs::write(
        src.join("lib.rs"),
        "#[path = \"../../b/src/x.rs\"]\nmod x;\n",
    )
    .expect("write");
    assert!(src_holds_a_doctest(&src));
    std::fs::write(
        src.join("lib.rs"),
        "include!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../b/src/x.rs\"));\n",
    )
    .expect("write");
    assert!(
        src_holds_a_doctest(&src),
        "a pull from the crate's directory is followed"
    );
    std::fs::write(src.join("lib.rs"), "include!(\"../../b/src/gone.rs\");\n").expect("write");
    assert!(src_holds_a_doctest(&src), "an unreadable pull fails wide");
}

#[test]
fn pulled_in_handles_raw_strings_cfg_attr_and_alternate_include_delimiters() {
    let text = "#[path = r\"a/b.rs\"]\nmod b;\n\
                #[cfg_attr(unix, path = r#\"c/d.rs\"#)]\nmod d;\n\
                include! [r\"e.rs\"];\n\
                include!{concat![env!(\"CARGO_MANIFEST_DIR\"), r\"/src/f.rs\"]}\n";
    assert_eq!(
        pulled_in(text),
        Some(vec![
            Pull::Beside("a/b.rs".to_string()),
            Pull::Beside("c/d.rs".to_string()),
            Pull::Beside("e.rs".to_string()),
            Pull::InCrate("/src/f.rs".to_string()),
        ])
    );
}

#[cfg(unix)]
#[test]
fn src_holds_a_doctest_follows_symlinked_subdirectories_and_handles_cycles() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let src = tmp.path().join("a/src");
    let ext = tmp.path().join("ext");
    std::fs::create_dir_all(&src).expect("mkdir");
    std::fs::create_dir_all(&ext).expect("mkdir");
    std::fs::write(src.join("lib.rs"), "pub fn plain() {}\n").expect("write");
    std::fs::write(ext.join("sub.rs"), "pub fn no_doc() {}\n").expect("write");
    std::os::unix::fs::symlink(&ext, src.join("linked")).expect("symlink");
    std::os::unix::fs::symlink(&src, ext.join("cycle")).expect("cycle symlink");
    assert!(!src_holds_a_doctest(&src));
    std::fs::write(
        ext.join("sub.rs"),
        "/// ```\n/// f();\n/// ```\npub fn f() {}\n",
    )
    .expect("write");
    assert!(src_holds_a_doctest(&src));
}
