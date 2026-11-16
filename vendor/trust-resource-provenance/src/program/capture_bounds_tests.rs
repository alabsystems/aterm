// SPDX-License-Identifier: Apache-2.0 OR MIT
// Copyright 2026 Andrew Yates

//! Actual file metadata/capture bounds. Fixture Mach-O bytes are never run.
use super::*;

#[test]
fn runtime_count_and_remaining_bytes_refuse_before_candidate_read_population() {
    let fixture = Fixture::new(&[]);
    for (remaining_files, remaining_bytes, reason) in [
        (0, MAX_IMAGE, "runtime population"),
        (1, 0, "runtime byte population"),
    ] {
        let mut directories = BTreeMap::new();
        let error = file_bounded(
            &mut directories,
            &fixture.program,
            true,
            &fixture.root,
            deadline(),
            remaining_files,
            remaining_bytes,
        )
        .err()
        .expect("actual candidate must fit both remaining budgets");
        assert!(error.to_string().contains(reason), "{error}");
        assert!(directories.len() <= MAX_DIRECTORIES);
    }
    fixture.open().unwrap().validate(deadline()).unwrap();
}

#[test]
fn full_runtime_population_still_allows_repeated_existing_edges_and_refuses_new_files() {
    let names = (0..MAX_RUNTIME)
        .map(|n| format!("runtime-{n}.dylib"))
        .collect::<Vec<_>>();
    let fixture = Fixture::new(&["@rpath/runtime-0.dylib"]);
    for (n, name) in names.iter().enumerate() {
        let next = names.get(n + 1).map(|name| format!("@rpath/{name}"));
        let loads = next.as_deref().map(|path| vec![path]).unwrap_or_default();
        fixture.runtime(name, &loads);
    }
    let pin = fixture.open().unwrap();
    assert_eq!(pin.runtime.len(), MAX_RUNTIME);
    assert!(
        pin.directories.len() + pin.runtime.len() + 1
            <= HeldProgram::capture_descriptor_bound().unwrap()
    );
    drop(pin);
    // A repeated original leaf at the end of a full population creates no new
    // file owner and must remain accepted; a genuine additional file refuses.
    fs::remove_file(fixture.root.join("lib").join(names.last().unwrap())).unwrap();
    fixture.runtime(names.last().unwrap(), &["@rpath/runtime-0.dylib"]);
    assert_eq!(fixture.open().unwrap().runtime.len(), MAX_RUNTIME);
    fixture.runtime("extra.dylib", &[]);
    fs::remove_file(fixture.root.join("lib").join(names.last().unwrap())).unwrap();
    fixture.runtime(names.last().unwrap(), &["@rpath/extra.dylib"]);
    let error = fixture
        .open()
        .err()
        .expect("new runtime exceeds the actual population bound");
    assert!(error.to_string().contains("runtime population"), "{error}");
}
