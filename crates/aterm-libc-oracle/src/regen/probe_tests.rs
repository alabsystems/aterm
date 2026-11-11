// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::cells::CELLS;
use crate::regen::decls::{Const, Field, Record};

fn decls() -> Decls {
    Decls {
        structs: vec![Record {
            name: "pthread_mutex_t".into(),
            repr: None,
            stripped: true,
            traits: vec![],
            fields: vec![Field {
                name: "sig".into(),
                ty: "c_long".into(),
            }],
        }],
        consts: vec![
            Const {
                name: "PTHREAD_MUTEX_INITIALIZER".into(),
                ty: "pthread_mutex_t".into(),
                expr: Some("_".into()),
                value: None,
            },
            Const {
                name: "AF_INET".into(),
                ty: "c_int".into(),
                expr: Some("2".into()),
                value: Some("2i32".into()),
            },
        ],
        ..Decls::default()
    }
}

#[test]
fn each_pass_measures_its_facts_one_per_line() {
    let pass1 = types_and_offsets(&decls());
    assert_eq!(
        pass1.source(),
        "#![allow(dead_code, non_camel_case_types)]\nuse libc::*;\n\
         const _: [(); 999999] = [(); ::core::mem::size_of::<libc::pthread_mutex_t>()];  // SZ pthread_mutex_t\n\
         const _: [(); 999999] = [(); ::core::mem::align_of::<libc::pthread_mutex_t>()];  // AL pthread_mutex_t\n\
         const _: [(); 999999] = [(); ::core::mem::offset_of!(libc::pthread_mutex_t, sig)];  // OF pthread_mutex_t.sig\n"
    );
    let sizes = BTreeMap::from([("pthread_mutex_t".to_string(), 2)]);
    let pass2 = constant_bytes(&decls(), &sizes).expect("sized");
    assert_eq!(pass2.keys.len(), 2, "two bytes; AF_INET has a value");
    assert!(pass2.source().contains(
        "const B_PTHREAD_MUTEX_INITIALIZER: [u8; 2] = unsafe { ::core::mem::transmute(libc::PTHREAD_MUTEX_INITIALIZER) };"
    ));
    assert!(constant_bytes(&decls(), &BTreeMap::new()).is_err());
    let pass3 = field_types(&decls());
    assert_eq!(
        pass3.keys.values().collect::<Vec<_>>(),
        [
            &Key::TypeSize("c_long".into()),
            &Key::TypeAlign("c_long".into())
        ]
    );
}

#[test]
fn the_compilers_reports_are_read_back_to_their_facts() {
    let e0308 = |line: u32, size: u32| {
        format!(
            r#"{{"reason":"compiler-message","message":{{"message":"mismatched types","code":{{"code":"E0308","explanation":"…"}},"level":"error","spans":[{{"file_name":"src/measure_mac.rs","line_start":{line},"is_primary":true,"label":"expected an array with a size of 999999, found one with a size of {size}"}},{{"file_name":"src/measure_mac.rs","line_start":{line},"is_primary":false,"label":"expected because of the type of the constant"}}],"children":[]}}}}"#
        )
    };
    let stdout = [
        e0308(3, 40),
        e0308(4, 8),
        e0308(4, 9).replace("measure_mac", "measure_linux"),
        r#"{"reason":"compiler-message","message":{"message":"aborting","code":null,"level":"error","spans":[]}}"#.into(),
        "   Compiling libc v0.2.186".into(),
    ]
    .join("\n");
    let outcome = Outcome::parse(false, Some(101), &stdout, "");
    let reported = reported(&outcome.errors, "measure_mac.rs");
    assert_eq!(reported, BTreeMap::from([(3, 40), (4, 8)]));
    let pass1 = types_and_offsets(&decls());
    let mut measured = Measured::default();
    assert_eq!(
        pass1.read(&reported),
        Err(vec![Key::Offset("pthread_mutex_t".into(), "sig".into())])
    );
    let all = BTreeMap::from([(3, 40), (4, 8), (5, 0)]);
    measured
        .record(pass1.read(&all).expect("all three"))
        .expect("records");
    assert_eq!(measured.size["pthread_mutex_t"], 40);
    assert_eq!(measured.align["pthread_mutex_t"], 8);
    assert_eq!(
        measured.offset[&("pthread_mutex_t".into(), "sig".into())],
        0
    );
    assert!(
        measured
            .record(BTreeMap::from([
                (Key::Size("extra".into()), 16),
                (Key::Byte("X".into(), 0), 256),
            ]))
            .is_err()
    );
    assert!(
        !measured.size.contains_key("extra"),
        "failed pass leaves Measured unchanged"
    );
}

#[test]
fn an_unmeasured_probe_shows_the_errors_that_are_not_measurements() {
    let measurement = r#"{"reason":"compiler-message","message":{"rendered":"error[E0308]: mismatched types","message":"mismatched types","code":{"code":"E0308"},"level":"error","spans":[{"file_name":"src/measure_mac.rs","line_start":3,"is_primary":true,"label":"expected an array with a size of 999999, found one with a size of 40"}]}}"#;
    let unresolved = r#"{"reason":"compiler-message","message":{"rendered":"error[E0425]: cannot find value `B_X` in this scope","message":"cannot find value `B_X` in this scope","code":{"code":"E0425"},"level":"error","spans":[{"file_name":"src/measure_mac.rs","line_start":4,"is_primary":true,"label":"not found in this scope"}]}}"#;
    let stderr = "error: could not compile `libc-reference` (lib) due to 2 previous errors";
    let both = Outcome::parse(
        false,
        Some(101),
        &[measurement, unresolved].join("\n"),
        stderr,
    );
    assert_eq!(
        unmeasured_because(&both),
        "error[E0425]: cannot find value `B_X` in this scope"
    );
    let only_measured = Outcome::parse(false, Some(101), measurement, stderr);
    assert_eq!(unmeasured_because(&only_measured), stderr);
}

#[test]
fn the_probe_root_gates_each_cell_module_on_its_own_cfg() {
    let lib = lib_rs(&CELLS[..1]);
    assert_eq!(
        lib,
        "#[cfg(all(target_os = \"macos\", target_arch = \"aarch64\"))]\nmod measure_mac;\n"
    );
}
