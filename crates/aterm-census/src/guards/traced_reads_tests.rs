// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::super::tests::{run, spots};
use super::*;

/// A marked file's every read past the trace is found, line by line, and nothing else is: not
/// the trace's own readers, a look's answer or an opened file asked, a comment or a literal, test
/// code, a function that records its own look, or a file whose module doc does not carry the
/// marker (the unmarked file never names it; the other two name it outside the module doc).
///
/// NEGATIVE CONTROLS (2026-11-15), each run on its own and restored (the file touched after),
/// each failing the first `assert_eq!` with the right as written here:
/// - `recording_bodies` answering no body: the recording reader's two raw looks were found too,
///   left `[..., "crates/x/src/marked.rs:38", "crates/x/src/marked.rs:45",
///   "crates/x/src/marked.rs:47", "crates/x/src/marked.rs:50"]`.
/// - `marked` answering true for every file: the reads of the file whose marker is a plain
///   comment and of the file whose marker is an inner module's doc were found, left
///   `["crates/x/src/commented.rs:3", "crates/x/src/late.rs:2", "crates/x/src/marked.rs:8",
///   ...]` (the file that never names the marker is not opened).
/// - `LOOKED` emptied: each `is_file`/`is_dir` on a look's answer was found, left
///   `[..., "crates/x/src/marked.rs:9", ..., "crates/x/src/marked.rs:13", ...,
///   "crates/x/src/marked.rs:21", ..., "crates/x/src/marked.rs:40", ...]`.
#[test]
fn a_raw_read_in_a_traced_file_fails_traced_reads() {
    let marked = "//! Accounts.\n\
                  //!\n\
                  //! Traced by the accounts keeper.\n\
                  \n\
                  use std::fs;\n\
                  use std::path::Path;\n\
                  use aterm_uds::trace;\n\
                  fn a(p: &Path) -> bool { p.is_file() }\n\
                  fn b(p: &Path) -> bool { trace::fs::metadata(p).is_ok_and(|meta| meta.is_file()) }\n\
                  fn c(p: &Path) -> Option<String> { fs::read_to_string(p).ok() }\n\
                  fn d(p: &Path) -> Option<String> { aterm_uds::trace::fs::read_to_string(p).ok() }\n\
                  fn e(p: &Path) -> bool { std::fs::metadata(p).is_ok_and(|m| m.is_dir()) }\n\
                  fn g(p: &Path) -> std::io::Result<bool> { Ok(trace::fs::symlink_metadata(p)?.is_dir()) }\n\
                  fn h(p: &Path) -> bool { p.exists() || Path::exists(p) }\n\
                  fn i(p: &Path) -> bool { p.read_dir().is_ok() }\n\
                  fn j(p: &Path) -> bool { std::fs::File::open(p).is_ok() }\n\
                  fn k(p: &Path) -> bool { trace::fs::open(p).is_ok_and(|file| file.metadata().is_ok()) }\n\
                  fn l(dir: &Path) -> usize {\n    \
                      trace::fs::read_dir(dir).map_or(0, |entries| {\n        \
                          entries.iter().filter(|entry| {\n            \
                              entry.file_type().is_ok_and(|kind| kind.is_file())\n                \
                                  && entry.path().is_file()\n                \
                                  && entry\n                    \
                                      .metadata()\n                    \
                                      .is_ok()\n        \
                          }).count()\n    \
                      })\n\
                  }\n\
                  fn n(p: &Path) -> Vec<String> { vec![p.to_path_buf()].iter().filter_map(fs::read_to_string).collect() }\n\
                  fn o(p: &Path) -> bool { astream_sys::open_nonblocking(p, astream_sys::Links::Refuse).is_ok() }\n\
                  fn q(p: &Path) -> bool {\n    \
                      let mut options = std::fs::OpenOptions::new();\n    \
                      options.read(true);\n    \
                      options.open(p).is_ok()\n\
                  }\n\
                  // fs::read(p) and p.exists() in a comment read nothing\n\
                  const SAID: &str = \"p.exists() || std::fs::read(p)\";\n\
                  fn r(p: &Path) -> bool { p.canonicalize().is_ok() || fs::canonicalize(p).is_ok() }\n\
                  fn s(p: &Path) -> bool { trace::fs::canonicalize(p).is_ok() }\n\
                  fn t(meta: &std::fs::Metadata) -> bool { meta.file_type().is_file() }\n\
                  /// A reader that records its own look.\n\
                  fn u(p: &Path) -> std::io::Result<std::fs::File> {\n    \
                      trace::fs::opening(p);\n    \
                      let mut options = std::fs::OpenOptions::new();\n    \
                      options.read(true);\n    \
                      options.open(p).map_err(|e| {\n        \
                          if std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir()) { e } else { e }\n    \
                      })\n\
                  }\n\
                  fn v(p: &Path) -> bool { p.symlink_metadata().is_ok() }\n\
                  #[cfg(test)]\n\
                  mod tests {\n    \
                      fn w(p: &std::path::Path) -> bool { p.exists() && std::fs::read(p).is_ok() }\n\
                  }\n";
    let unmarked = "//! Something else.\n\
                    \n\
                    use std::path::Path;\n\
                    fn a(p: &Path) -> bool { p.exists() }\n";
    let commented = "// Traced by the accounts keeper.\n\
                     //! Not this file's doc.\n\
                     fn a(p: &std::path::Path) -> bool { p.exists() }\n";
    let late = "//! A doc.\n\
                fn a(p: &std::path::Path) -> bool { p.exists() }\n\
                mod inner {\n    \
                    //! Traced by the accounts keeper.\n\
                }\n";
    let found = run(
        &TRACED_READS,
        &[
            ("crates/x/src/marked.rs", marked),
            ("crates/x/src/unmarked.rs", unmarked),
            ("crates/x/src/commented.rs", commented),
            ("crates/x/src/late.rs", late),
        ],
    );
    assert_eq!(
        spots(&found),
        [
            "crates/x/src/marked.rs:8",
            "crates/x/src/marked.rs:10",
            "crates/x/src/marked.rs:12",
            "crates/x/src/marked.rs:14",
            "crates/x/src/marked.rs:15",
            "crates/x/src/marked.rs:16",
            "crates/x/src/marked.rs:22",
            "crates/x/src/marked.rs:24",
            "crates/x/src/marked.rs:29",
            "crates/x/src/marked.rs:30",
            "crates/x/src/marked.rs:33",
            "crates/x/src/marked.rs:38",
            "crates/x/src/marked.rs:50",
        ]
    );
    assert!(
        found.iter().all(|f| f.detail.contains("past the trace")),
        "{found:#?}"
    );
}

/// A marked file's every raw read of the wall clock is found, whatever path names it and inside
/// a function that records its own look too, and nothing else is: not `aterm_time`'s counted
/// readers, the trace's `Now`, a monotonic deadline, a `SystemTime` named as a type or its epoch,
/// a comment, a literal or test code.
///
/// NEGATIVE CONTROLS (2026-11-16), each run on its own and restored (the file touched after):
/// - `clock_reads` answering nothing failed the first `assert_eq!`, left `[]`;
/// - the clock's reads filtered by `recording_bodies` as the file system's are failed it, left
///   `["crates/x/src/clock.rs:3", "crates/x/src/clock.rs:4"]`.
#[test]
fn a_raw_wall_clock_read_in_a_traced_file_fails_traced_reads() {
    let clock = "//! Traced by the accounts keeper.\n\
                 use std::time::{Instant, SystemTime, UNIX_EPOCH};\n\
                 fn a() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() }\n\
                 fn b() -> u64 { std::time::SystemTime::now()\n    \
                     .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() }\n\
                 fn c() -> Option<u64> { aterm_time::unix_secs() }\n\
                 fn d(now: aterm_uds::trace::Now) -> bool { now.passed(5) }\n\
                 fn e() -> Instant { Instant::now() + std::time::Duration::from_secs(5) }\n\
                 fn f(p: &std::path::Path) -> SystemTime {\n    \
                     aterm_uds::trace::fs::opening(p); time::SystemTime::now()\n\
                 }\n\
                 fn g(at: SystemTime) -> bool { at > SystemTime::UNIX_EPOCH }\n\
                 // SystemTime::now() in a comment reads nothing\n\
                 const SAID: &str = \"SystemTime::now()\";\n\
                 #[cfg(test)]\n\
                 mod tests {\n    \
                     fn w() -> std::time::SystemTime { std::time::SystemTime::now() }\n\
                 }\n";
    let found = run(&TRACED_READS, &[("crates/x/src/clock.rs", clock)]);
    assert_eq!(
        spots(&found),
        [
            "crates/x/src/clock.rs:3",
            "crates/x/src/clock.rs:4",
            "crates/x/src/clock.rs:10",
        ]
    );
    assert!(
        found
            .iter()
            .all(|f| f.detail.contains("reads the wall clock past the trace")),
        "{found:#?}"
    );
}

/// The receiver a method is judged by is its last name, a call's or a binding's, across a
/// line break and past a `?`.
#[test]
fn a_receiver_is_its_last_name_across_lines_and_a_question_mark() {
    let code = "entry\n    .metadata()";
    assert_eq!(receiver(code, code.find('.').unwrap()), Some("entry"));
    let code = "trace::fs::metadata(p)?.is_dir()";
    assert_eq!(receiver(code, code.rfind('.').unwrap()), Some("metadata"));
    let code = "items[0].is_file()";
    assert_eq!(receiver(code, code.find('.').unwrap()), None);
}
