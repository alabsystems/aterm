// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn capture() -> Capture {
    Capture {
        reference: "Homebrew 7.0.10".into(),
        columns: 80,
        provenance: "Offline synthetic test capture".into(),
        source_sha256: "a".repeat(64),
        source_commit: None,
        tables: Tables {
            top: "top \"# 🐈\n".into(),
            texts: BTreeMap::from([("alpha".into(), "quoted \"## text\t \n".into())]),
            aliases: BTreeMap::from([("a".into(), "alpha".into())]),
        },
    }
}

fn fields(value: &mut Value) -> &mut aterm_json::Map {
    value.as_object_mut().unwrap()
}

#[test]
fn aliases_require_distinct_names_and_an_explicit_canonical_target() {
    let good = capture();
    assert_eq!(Capture::parse(&good.json().unwrap()).unwrap(), good);
    for aliases in [
        json!([{"name":"a", "target":"missing"}]),
        json!([{"name":"alpha", "target":"alpha"}]),
        json!([{"name":"a", "target":"alpha"}, {"name":"a", "target":"alpha"}]),
        json!([{"name":"a", "target":"b"}, {"name":"b", "target":"alpha"}]),
        json!([{"name":"a", "target":"alpha", "stdout":"different"}]),
    ] {
        let mut value: Value = aterm_json::from_slice(&good.json().unwrap()).unwrap();
        fields(&mut value).insert("aliases".into(), aliases);
        assert!(Capture::parse(&aterm_json::to_vec(&value).unwrap()).is_err());
    }
    let mut value: Value = aterm_json::from_slice(&good.json().unwrap()).unwrap();
    fields(&mut value).insert(
        "canonical".into(),
        json!([{"name":"x", "stdout":"one"}, {"name":"x", "stdout":"two"}]),
    );
    assert!(Capture::parse(&aterm_json::to_vec(&value).unwrap()).is_err());
}

#[test]
fn owner_paths_are_rejected_without_rejecting_documented_brew_defaults() {
    for text in [
        "/Users//another/private",
        "/home/another/private",
        r"C:\Users\another\private",
        "/home/linuxbrewer/private",
        "file:///home/alice/private",
        "/home/linuxbrew/../alice/private",
    ] {
        let mut bad = capture();
        bad.tables.top = text.into();
        assert!(bad.validate().is_err());
    }
    let mut documented = capture();
    documented.tables.top =
        "/opt/homebrew and /home/linuxbrew/.linuxbrew; use ./home/ for scratch\n".into();
    documented.validate().unwrap();
}

#[test]
fn generation_cannot_replace_its_only_input() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let path = temp.path().join(OUTPUT);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let witness = capture().json().unwrap();
    std::fs::write(&path, &witness).unwrap();
    let options = Options {
        input: OUTPUT.into(),
        mode: Mode::Write,
    };
    assert!(run_with(temp.path(), &options, |_, _| Ok(())).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), witness);
}

#[cfg(unix)]
#[test]
fn an_escaping_output_parent_cannot_write_outside_the_checkout() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let outside = aterm_tempfile::tempdir().unwrap();
    put_capture(temp.path(), &capture());
    let parent = temp.path().join(OUTPUT).parent().unwrap().to_path_buf();
    std::fs::create_dir_all(parent.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(outside.path(), &parent).unwrap();
    let witness = outside.path().join("help_texts.rs");
    std::fs::write(&witness, b"outside witness").unwrap();
    let options = Options {
        input: INPUT.into(),
        mode: Mode::Write,
    };
    assert!(run_with(temp.path(), &options, |_, _| Ok(())).is_err());
    assert_eq!(std::fs::read(&witness).unwrap(), b"outside witness");
}

#[test]
fn literal_rendering_round_trips_quotes_unicode_controls_and_raw_fences() {
    let mut cases = vec![
        "trailing spaces  \n\t".to_owned(),
        "🐈 quote \"# end\n".into(),
        "windows\r\nline\r\n".into(),
        "nul\0esc\u{1b}slash\\".into(),
    ];
    cases.push(
        (1..=255)
            .map(|n| format!("\"{}", "#".repeat(n)))
            .collect::<Vec<_>>()
            .join(" "),
    );
    for text in cases {
        let mut value = capture();
        value.tables.top = text.clone();
        let source = render(&value, "test generator", &"a".repeat(64));
        assert_eq!(legacy::tables(&source).unwrap().top, text);
    }
}

fn put_capture(root: &Path, value: &Capture) {
    let input = root.join(INPUT);
    std::fs::create_dir_all(input.parent().unwrap()).unwrap();
    std::fs::write(input, value.json().unwrap()).unwrap();
}

#[test]
fn a_missing_output_regenerates_and_check_detects_changed_independent_input() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let root = temp.path();
    let mut input = capture();
    put_capture(root, &input);
    let options = Options {
        input: INPUT.into(),
        mode: Mode::Write,
    };
    assert!(run_with(root, &options, |_, _| Ok(())).unwrap().ok);
    let path = root.join(OUTPUT);
    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(matches!(seal::check(&saved), seal::Seal::Holds { .. }));
    assert_eq!(legacy::tables(&saved).unwrap(), input.tables);
    let check = Options {
        mode: Mode::Check,
        ..options
    };
    assert!(run_with(root, &check, |_, _| Ok(())).unwrap().ok);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), saved);
    input.tables.top.push_str("changed\n");
    put_capture(root, &input);
    assert!(!run_with(root, &check, |_, _| Ok(())).unwrap().ok);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), saved);
}

#[test]
fn invalid_input_and_formatter_failure_preserve_an_existing_output() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let root = temp.path();
    let path = root.join(OUTPUT);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"output witness").unwrap();
    let mut input = capture();
    input.tables.top = "/Users//another/private".into();
    put_capture(root, &input);
    let options = Options {
        input: INPUT.into(),
        mode: Mode::Write,
    };
    assert!(
        run_with(root, &options, |_, _| panic!(
            "invalid input cannot reach formatting"
        ))
        .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"output witness");
    put_capture(root, &capture());
    assert!(run_with(root, &options, |_, _| Err("formatter refused".into())).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"output witness");
}

#[test]
fn legacy_import_does_not_accept_a_newly_stamped_or_executable_payload() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let input = temp.path().join("not-f15.rs");
    let source = format!(
        "{}\nfn arbitrary() {{}}\n",
        seal::line("//", "tools/gen-abrew-help-texts.py")
    );
    std::fs::write(&input, seal::stamp(&source).unwrap()).unwrap();
    let destination = temp.path().join("capture.json");
    std::fs::write(&destination, b"snapshot witness").unwrap();
    let options = Options {
        input,
        mode: Mode::ImportLegacy {
            output: destination.clone(),
        },
    };
    assert!(
        run_with(temp.path(), &options, |_, _| panic!(
            "import executes no formatter"
        ))
        .is_err()
    );
    assert_eq!(std::fs::read(&destination).unwrap(), b"snapshot witness");
}
