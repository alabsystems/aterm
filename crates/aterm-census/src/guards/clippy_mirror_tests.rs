// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::super::tests::{run, spots};
use super::*;

const WORKSPACE: &str = "disallowed-methods = [\n\
  { path = \"std::io::pipe\", reason = \"make a pipe with aterm_uds::pipe\" },\n\
  { path = \"libc::pipe\", reason = \"make a pipe with aterm_uds::pipe\" },\n\
  { path = \"libc::pipe2\", reason = \"make a pipe with aterm_uds::pipe\", allow-invalid = true },\n\
  { path = \"windows_sys::Win32::System::Threading::CreateProcessW\", reason = \"spawnfd\", allow-invalid = true },\n\
  { path = \"aterm_gui::main_entry\", reason = \"run_window\" },\n\
]\n\
disallowed-types = [\n\
  { path = \"std::process::Command\", reason = \"spawnfd::command\" },\n\
]\n";

const PIPE_STD: &str =
    "  { path = \"std::io::pipe\", reason = \"make a pipe with aterm_uds::pipe\" },\n";
const PIPE_LIBC: &str = "  { path = \"libc::pipe\", reason = \"make a pipe with aterm_uds::pipe\" },\n\
  { path = \"libc::pipe2\", reason = \"make a pipe with aterm_uds::pipe\", allow-invalid = true },\n";
const COMMAND: &str = "disallowed-types = [\n  { path = \"std::process::Command\", reason = \"spawnfd::command\" },\n]\n";
const MMAP: &str = "  { path = \"libc::mmap\", reason = \"the keeper maps no region\" },\n";

fn methods(rows: &[&str]) -> String {
    format!("disallowed-methods = [\n{}]\n{COMMAND}", rows.concat())
}

fn manifest(deps: &str) -> String {
    format!("[package]\nname = \"x\"\n\n[dependencies]\naterm-uds = {{ workspace = true }}\n{deps}")
}

/// The keeper's case: its file names libc for a law of its own, so every workspace libc row
/// holds there, and std's everywhere; a file that repeats them all is quiet, and the gui row,
/// of a crate the keeper neither names nor depends on, is not asked for.
#[test]
fn a_crate_file_leaves_out_no_workspace_row_of_std_or_a_crate_it_names() {
    let short = methods(&[MMAP]);
    let whole = methods(&[PIPE_LIBC, PIPE_STD, MMAP]);
    let found = run(
        &CLIPPY_MIRROR,
        &[
            ("clippy.toml", WORKSPACE),
            ("crates/aterm-keeper/Cargo.toml", &manifest("")),
            ("crates/aterm-keeper/clippy.toml", &short),
            ("crates/aterm-restore/Cargo.toml", &manifest("")),
            ("crates/aterm-restore/clippy.toml", &whole),
        ],
    );
    let details: Vec<&str> = found.iter().map(|f| f.detail.as_str()).collect();
    assert_eq!(
        spots(&found),
        [
            "crates/aterm-keeper/clippy.toml:1",
            "crates/aterm-keeper/clippy.toml:1",
            "crates/aterm-keeper/clippy.toml:1",
        ],
        "{details:#?}"
    );
    for (detail, path) in details
        .iter()
        .zip(["std::io::pipe", "libc::pipe", "libc::pipe2"])
    {
        assert!(
            detail.contains(&format!("disallowed-methods row `{path}`"))
                && detail.contains("crates/aterm-keeper"),
            "{detail}"
        );
    }
}

/// A crate whose package depends on libc (a macOS dev-dependency, as aterm-core's) holds the
/// workspace's libc rows though its file names none; one that neither names nor depends on libc
/// holds only std's.
#[test]
fn a_dependency_brings_its_crates_workspace_rows() {
    let std_only = methods(&[PIPE_STD]);
    let found = run(
        &CLIPPY_MIRROR,
        &[
            ("clippy.toml", WORKSPACE),
            (
                "crates/aterm-core/Cargo.toml",
                &manifest(
                    "\n[target.'cfg(target_os = \"macos\")'.dev-dependencies]\nlibc = { workspace = true }\n",
                ),
            ),
            ("crates/aterm-core/clippy.toml", &std_only),
            (
                "crates/aterm-diskwalk/Cargo.toml",
                &manifest("\n[target.'cfg(unix)'.dependencies.libc]\nworkspace = true\n"),
            ),
            ("crates/aterm-diskwalk/clippy.toml", &std_only),
            ("crates/aterm-aos/Cargo.toml", &manifest("")),
            ("crates/aterm-aos/clippy.toml", &std_only),
        ],
    );
    let rows: Vec<String> = found
        .iter()
        .map(|f| {
            let path = f.detail.split('`').nth(1).unwrap_or_default();
            format!("{}:{path}", f.path)
        })
        .collect();
    assert_eq!(
        rows,
        [
            "crates/aterm-core/clippy.toml:libc::pipe",
            "crates/aterm-core/clippy.toml:libc::pipe2",
            "crates/aterm-diskwalk/clippy.toml:libc::pipe",
            "crates/aterm-diskwalk/clippy.toml:libc::pipe2",
        ]
    );
}

/// A row repeated in other words (here without its `allow-invalid`) is named on its own line;
/// a row in the wrong list is left out of the right one.
#[test]
fn a_reworded_or_misplaced_row_is_not_a_repeat() {
    let reworded = format!(
        "disallowed-methods = [\n{PIPE_STD}  {{ path = \"libc::pipe\", reason = \"make a pipe with aterm_uds::pipe\" }},\n  \
         {{ path = \"libc::pipe2\", reason = \"make a pipe with aterm_uds::pipe\" }},\n  \
         {{ path = \"std::process::Command\", reason = \"spawnfd::command\" }},\n]\n"
    );
    let found = run(
        &CLIPPY_MIRROR,
        &[
            ("clippy.toml", WORKSPACE),
            ("crates/aterm-slog/Cargo.toml", &manifest("")),
            ("crates/aterm-slog/clippy.toml", &reworded),
        ],
    );
    assert_eq!(
        spots(&found),
        [
            "crates/aterm-slog/clippy.toml:0",
            "crates/aterm-slog/clippy.toml:4"
        ],
        "{found:#?}"
    );
    assert!(
        found[0]
            .detail
            .contains("leaves out the workspace's disallowed-types row `std::process::Command`")
    );
    assert!(
        found[1]
            .detail
            .contains("words the workspace's disallowed-methods row `libc::pipe2`")
    );
}

/// A file that lists no row opts out whole (aterm-libc's); a file that is no package's nearest
/// (a `CLIPPY_CONF_DIR` folder below a crate) is not a crate's file; packages nearest the root
/// lint by the workspace's rows themselves; and `.clippy.toml` is a crate file as clippy reads it.
#[test]
fn only_a_crate_file_with_rows_is_held_to_the_workspace() {
    let found = run(
        &CLIPPY_MIRROR,
        &[
            ("clippy.toml", WORKSPACE),
            (
                "crates/aterm-libc/Cargo.toml",
                &manifest("libc = \"0.2\"\n"),
            ),
            (
                "crates/aterm-libc/clippy.toml",
                "# Empty: clippy's defaults.\n",
            ),
            ("crates/aterm-time/Cargo.toml", &manifest("")),
            ("crates/aterm-time/wasm32/clippy.toml", &methods(&[])),
            ("crates/aterm-gui/Cargo.toml", &manifest("libc = \"0.2\"\n")),
            ("crates/aterm-dot/Cargo.toml", &manifest("")),
            ("crates/aterm-dot/.clippy.toml", &methods(&[])),
        ],
    );
    assert_eq!(
        spots(&found),
        ["crates/aterm-dot/.clippy.toml:1"],
        "{found:#?}"
    );
    assert!(found[0].detail.contains("`std::io::pipe`"));
}

/// An entry written over several lines, or a list on one line, is one this rule cannot read, in
/// the workspace's file and a crate's alike.
#[test]
fn an_entry_off_one_line_is_named() {
    let split = format!(
        "disallowed-methods = [\n{PIPE_STD}  {{ path = \"libc::mmap\",\n    reason = \"no region\" }},\n]\n{COMMAND}"
    );
    let found = run(
        &CLIPPY_MIRROR,
        &[
            (
                "clippy.toml",
                &format!("{WORKSPACE}disallowed-macros = [ {{ path = \"std::dbg\" }} ]\n"),
            ),
            ("crates/aterm-keeper/Cargo.toml", &manifest("")),
            ("crates/aterm-keeper/clippy.toml", &split),
        ],
    );
    assert_eq!(
        spots(&found),
        [
            "clippy.toml:11",
            "crates/aterm-keeper/clippy.toml:3",
            "crates/aterm-keeper/clippy.toml:4",
        ],
        "{found:#?}"
    );
    assert!(found.iter().all(|f| f.detail.contains("reads only as one")));
}

#[test]
fn dependencies_read_every_table_as_code_spells_the_crate() {
    let deps = dependencies(
        "[package]\nname = \"x\"\n\n[dependencies]\naterm-uds = { workspace = true }\n\
         windows-sys.workspace = true\n# libc = \"0.2\"\n\n[features]\nring = []\n\n\
         [build-dependencies.cc]\nversion = \"1\"\n\n[target.'cfg(windows)'.dev-dependencies]\n\
         \"aterm-objc\" = { path = \"../aterm-objc\" }\n",
    );
    assert_eq!(
        deps.into_iter().collect::<Vec<_>>(),
        ["aterm_objc", "aterm_uds", "cc", "windows_sys"]
    );
}
