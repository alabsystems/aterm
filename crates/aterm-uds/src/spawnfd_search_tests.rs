// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`Search`]: the candidates glibc's `execvp` tries, in its order.

use super::Search;
use std::ffi::{CStr, OsStr};

fn candidates(program: &CStr, path: Option<&str>) -> Vec<String> {
    Search::new(program, path.map(OsStr::new), false)
        .expect("a search")
        .candidates
        .iter()
        .map(|c| c.to_str().expect("UTF-8").to_owned())
        .collect()
}

#[test]
fn a_bare_name_is_joined_to_each_entry_and_an_empty_entry_is_the_name_itself() {
    assert_eq!(
        candidates(c"tool", Some("/usr/bin::rel/dir:/bin")),
        ["/usr/bin/tool", "tool", "rel/dir/tool", "/bin/tool"],
    );
    assert_eq!(candidates(c"tool", Some("")), ["tool"]);
    assert_eq!(candidates(c"tool", Some("/a:")), ["/a/tool", "tool"]);
}

#[test]
fn no_path_searches_glibcs_default_and_a_name_with_a_slash_is_its_own_candidate() {
    assert_eq!(candidates(c"tool", None), ["/bin/tool", "/usr/bin/tool"]);
    assert_eq!(candidates(c"./tool", Some("/usr/bin")), ["./tool"]);
    assert_eq!(candidates(c"/opt/x/tool", None), ["/opt/x/tool"]);
}

#[test]
fn an_empty_name_is_refused_before_any_exec_and_a_long_one_is_searched() {
    let refused = Search::new(c"", Some(OsStr::new("/bin")), false).expect_err("refused");
    assert_eq!(refused.raw_os_error(), Some(libc::ENOENT));
    let long = "x".repeat(256);
    let name = std::ffi::CString::new(long.clone()).expect("no NUL");
    assert_eq!(
        candidates(&name, Some("/bin:")),
        [format!("/bin/{long}"), long],
        "glibc refuses no name for its length: each exec answers it"
    );
}

#[test]
fn an_entry_too_long_for_glibcs_buffer_is_passed_over_for_the_name_itself() {
    let long = format!("/{}", "d".repeat(4095));
    let path = format!("/a:{long}:/b");
    assert_eq!(
        candidates(c"tool", Some(&path)),
        ["/a/tool", "tool", "/b/tool"],
        "past a long entry that is not the last, glibc's walk reads an empty one"
    );
    let path = format!("/a:{long}");
    assert_eq!(candidates(c"tool", Some(&path)), ["/a/tool"]);
    let path = format!("{long}:");
    assert_eq!(candidates(c"tool", Some(&path)), ["tool", "tool"]);
    let shorter = format!("/{}", "d".repeat(4094));
    let path = format!("{shorter}:/b");
    assert_eq!(
        candidates(c"tool", Some(&path)),
        [format!("{shorter}/tool"), "/b/tool".to_owned()],
        "an entry of PATH_MAX - 1 bytes fits glibc's buffer"
    );
}

#[test]
fn only_a_native_image_skips_the_shell() {
    let search = |native| {
        Search::new(c"tool", Some(OsStr::new("/bin")), native)
            .expect("a search")
            .script
    };
    assert!(search(false));
    assert!(!search(true));
}
