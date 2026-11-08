// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The compile account's reading of Cargo's messages, its output bytes, and its line and event.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{Account, Units, human, output_bytes, read};

/// One `compiler-artifact` message naming `files`, `fresh` as given (`None`: no `fresh` field).
fn artifact(name: &str, fresh: Option<bool>, files: &[&Path]) -> String {
    let mut message = aterm_json::json!({
        "reason": "compiler-artifact",
        "package_id": format!("path+file:///ws#{name}@0.1.0"),
        "target": {"name": name, "kind": ["lib"]},
        "filenames": files.iter().map(|f| f.display().to_string()).collect::<Vec<_>>(),
    });
    if let (Some(fresh), Some(object)) = (fresh, message.as_object_mut()) {
        object.insert("fresh".to_owned(), aterm_json::Value::from(fresh));
    }
    aterm_json::to_string(&message).expect("encoded")
}

/// A call's units are what its artifact messages say: `fresh: true` fresh, `fresh: false`
/// compiled, and an artifact with no `fresh`, a `fresh` that is not a boolean, or repeated fields
/// unknown, never either. Other messages, a repeated dispatch and a person's lines are no unit.
/// NEGATIVE CONTROL: read as the shaped compile reads freshness (`fresh` missing is `false`), the
/// fresh-less artifact counts as compiled and the unknown count falls to 1.
#[test]
fn units_are_fresh_compiled_or_unknown_and_never_guessed() {
    let none: [&Path; 0] = [];
    let stdout = [
        artifact("a", Some(true), &none),
        artifact("b", Some(false), &none),
        artifact("c", Some(true), &none),
        artifact("d", None, &none),
        r#"{"reason":"compiler-artifact","target":{"name":"e","kind":["lib"]},"fresh":"yes","filenames":[]}"#.to_owned(),
        r#"{"reason":"compiler-artifact","fresh":true,"fresh":false,"filenames":[]}"#.to_owned(),
        r#"{"reason":"compilation-unit-dispatched","mode":"build","mode":"check"}"#.to_owned(),
        r#"{"reason":"build-script-executed","package_id":"x"}"#.to_owned(),
        r#"{"reason":"compiler-message","message":{"rendered":"warning"}}"#.to_owned(),
        r#"{"reason":"build-finished","success":true}"#.to_owned(),
        "   Compiling a v0.1.0".to_owned(),
        "{not json".to_owned(),
    ]
    .join("\n");
    assert_eq!(
        read(&stdout).units,
        Units {
            fresh: 2,
            compiled: 1,
            unknown: 3,
        }
    );
    assert_eq!(read("").units, Units::default());
}

/// JSON spelling and unrelated values cannot decide whether an ambiguous message names an
/// artifact. Every decoded top-level artifact reason counts once as unknown, in either order.
#[test]
fn ambiguous_units_use_decoded_top_level_reasons() {
    for wire in [
        r#"{"reason":"compiler-\u0061rtifact","fresh":false,"fresh":true}"#,
        r#"{"re\u0061son":"compiler-artifact","fresh":false,"fresh":true}"#,
        r#"{"reason":"compilation-unit-dispatched","reason":"compiler-artifact"}"#,
        r#"{"reason":"compiler-artifact","reason":"compilation-unit-dispatched"}"#,
        r#"{"reason":"build-finished","re\u0061son":"compiler-\u0061rtifact"}"#,
    ] {
        let artifacts = read(wire);
        assert_eq!(
            artifacts.units,
            Units {
                unknown: 1,
                ..Units::default()
            },
            "{wire}"
        );
        assert!(artifacts.written.is_empty());
    }
    for wire in [
        r#"{"reason":"compilation-unit-dispatched","note":"compiler-artifact","mode":"build","mode":"check"}"#,
        r#"{"reason":"compilation-unit-dispatched","note":{"reason":"compiler-artifact","x":1,"x":2}}"#,
        r#"{"reason":"compilation-unit-dispatched","reason":"build-finished","note":"compiler-artifact"}"#,
    ] {
        let artifacts = read(wire);
        assert_eq!(artifacts.units, Units::default(), "{wire}");
        assert!(artifacts.written.is_empty());
    }
}

/// A call's output bytes are the regular files each artifact it did not report fresh names, one
/// metadata read each: a path named twice, and a hard link of a file already counted (an uplifted
/// binary beside its `deps/` copy), count once; a directory (`.dSYM`), a symlink and a file that is
/// gone add nothing; a fresh artifact's files add nothing; and nothing under a directory is read.
/// NEGATIVE CONTROLS: counted by path alone, the hard link adds its 300 bytes again; read through
/// symlinks, the symlink adds the 50 bytes of the file it names.
#[test]
fn output_bytes_are_the_files_a_call_wrote_once_each_and_never_a_walk() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let at = |name: &str| dir.path().join(name);
    std::fs::write(at("liba.rlib"), [0_u8; 300]).expect("rlib");
    std::fs::write(at("a.rmeta"), [0_u8; 20]).expect("rmeta");
    std::fs::write(at("fresh.rlib"), [0_u8; 4000]).expect("fresh");
    std::fs::create_dir_all(at("bin.dSYM/Contents")).expect("dsym");
    std::fs::write(at("bin.dSYM/Contents/dwarf"), [0_u8; 9000]).expect("dwarf");
    #[cfg(unix)]
    {
        std::fs::hard_link(at("liba.rlib"), at("uplifted.rlib")).expect("hard link");
        std::fs::write(at("elsewhere.rlib"), [0_u8; 50]).expect("symlink target");
        std::os::unix::fs::symlink(at("elsewhere.rlib"), at("link.rlib")).expect("symlink");
    }
    let written: Vec<PathBuf> = [
        "liba.rlib",
        "a.rmeta",
        "liba.rlib",
        "uplifted.rlib",
        "link.rlib",
        "bin.dSYM",
        "gone.rlib",
    ]
    .iter()
    .map(|n| at(n))
    .collect();
    let refs: Vec<&Path> = written.iter().map(PathBuf::as_path).collect();
    let stdout = [
        artifact("a", Some(false), &refs[..3]),
        artifact("b", None, &refs[3..]),
        artifact("fresh", Some(true), &[at("fresh.rlib").as_path()]),
    ]
    .join("\n");
    let read = read(&stdout);
    assert_eq!(
        read.written, written,
        "every file a non-fresh artifact names"
    );
    assert_eq!(output_bytes(&read.written), 320);
    assert_eq!(output_bytes(&[]), 0);
}

/// A person's lines stay and Cargo's messages go: what a build script printed, a JSON line that is
/// no compiler message, and the line order are kept.
#[test]
fn human_keeps_every_line_but_cargos_messages() {
    let none: [&Path; 0] = [];
    let stdout = format!(
        "first\n{}\n{{\"reason\":\"build-finished\",\"success\":true}}\n{{\"note\":1}}\nlast\n",
        artifact("a", Some(true), &none)
    );
    assert_eq!(human(&stdout), "first\n{\"note\":1}\nlast\n");
    assert_eq!(human(""), "");
}

/// The line and the event carry the same numbers, and what was not read is said as unread or
/// unreported (`null` in the event), never as 0. NEGATIVE CONTROL: an account with every reading
/// gives every field a number.
#[test]
fn the_compile_line_and_event_agree_and_never_print_an_unread_zero() {
    let usage = crate::exec::TreeUsage {
        cpu: Duration::from_millis(45_250),
        runnable: Duration::from_secs(60),
        starved: Duration::from_millis(1_500),
        waited: Duration::from_millis(250),
        wall: Duration::from_secs(12),
        peak_bytes: 3 << 30,
        processes: 9,
    };
    let full = Account {
        step: "test compile".to_owned(),
        call: "targo test -p a --no-run".to_owned(),
        units: Some(Units {
            fresh: 120,
            compiled: 3,
            unknown: 1,
        }),
        usage: Some(usage),
        wall: Duration::from_secs(12),
        output_bytes: Some(5 << 20),
    };
    let line = full.line();
    // The wall and the sizes read as the run's formatters write them everywhere.
    let wall = format!("{} wall", crate::report::duration(12.0));
    let peak = format!("{} peak", aterm_capacity::size(3 << 30));
    let out = format!("{} out", aterm_capacity::size(5 << 20));
    for said in [
        "compile test compile/targo test -p a --no-run: ",
        "120 fresh / 3 compiled / 1 unknown",
        "45.2 CPU-s",
        "1.5 starved-s",
        &wall,
        &peak,
        &out,
    ] {
        assert!(line.contains(said), "{said:?} in {line}");
    }
    let event = |a: &Account| -> aterm_json::Value {
        let e = a.event();
        aterm_json::from_str(e.strip_prefix("verify-event: ").expect("prefixed")).expect("json")
    };
    let e = event(&full);
    assert_eq!(e["schema"].as_u64(), Some(1));
    assert_eq!(e["event"].as_str(), Some("compile-finished"));
    assert_eq!(e["step"].as_str(), Some("test compile"));
    assert_eq!(e["units"]["fresh"].as_u64(), Some(120));
    assert_eq!(e["units"]["compiled"].as_u64(), Some(3));
    assert_eq!(e["units"]["unknown"].as_u64(), Some(1));
    assert_eq!(e["cpu_s"].as_f64(), Some(45.25));
    assert_eq!(e["starved_s"].as_f64(), Some(1.5));
    assert_eq!(e["waited_s"].as_f64(), Some(0.25));
    assert_eq!(e["wall_s"].as_f64(), Some(12.0));
    assert_eq!(e["peak_bytes"].as_u64(), Some(3 << 30));
    assert_eq!(e["output_bytes"].as_u64(), Some(5 << 20));

    let unread = Account::of("lint", "tippy -p a", None, None, Duration::from_secs(2));
    let line = unread.line();
    for said in [
        "units unreported",
        "CPU-s unread",
        "starved-s unread",
        "peak unread",
        "out unread",
    ] {
        assert!(line.contains(said), "{said:?} in {line}");
    }
    assert!(!line.contains(" 0 "), "{line}");
    let e = event(&unread);
    for field in [
        "units",
        "cpu_s",
        "starved_s",
        "waited_s",
        "peak_bytes",
        "output_bytes",
    ] {
        assert!(e[field].is_null(), "{field}: {e}");
    }
    assert_eq!(e["wall_s"].as_f64(), Some(2.0));
}
