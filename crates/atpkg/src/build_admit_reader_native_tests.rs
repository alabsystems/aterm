// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn helper_hint_cannot_substitute_for_the_actual_terminal_hop() {
    let helper = Path::new("/managed/atpkg");
    let shim = Path::new("/managed/bin/targo");
    let body = crate::platform::canonical_build_hop_shim(
        shim,
        Path::new("/managed/targo"),
        &crate::shim_env::ShimEnv::NONE,
        helper,
    )
    .unwrap();
    let text = String::from_utf8(body.body).unwrap();
    assert_eq!(hop_helper(shim, &text).unwrap(), helper);
    assert!(
        hop_helper(
            shim,
            "# atpkg cache helper \"/managed/atpkg\"\nexec /other/atpkg\n"
        )
        .is_err()
    );
    let wrong = text.replace("&& exec '/managed/atpkg'", "&& exec '/other/atpkg'");
    assert_ne!(wrong, text);
    assert!(hop_helper(shim, &wrong).is_err());
    assert!(hop_helper(shim, &format!("{text}exec /other/atpkg\n")).is_err());
    assert!(
        hop_helper(
            shim,
            &format!("# atpkg cache helper \"/other/atpkg\"\n{text}")
        )
        .is_err()
    );
    let bypass = text.replacen("#!/bin/sh\n", "#!/bin/sh\nexec /old/reader \"$@\"\n", 1);
    assert_ne!(bypass, text);
    assert!(
        hop_helper(shim, &bypass).is_err(),
        "an exact terminal hop cannot excuse an earlier exec"
    );
}

#[test]
fn real_query_outputs_require_schema_namespace_and_success_separately() {
    let root = aterm_tempfile::tempdir().unwrap();
    let ledger = aterm_capacity::Ledger::at(root.path().join("ledger"));
    let expected = super::super::capabilities_at(&ledger).unwrap();
    let run = |text: &str, code: &str| {
        let mut command = aterm_uds::spawnfd::command("/bin/sh");
        command.args([
            "-c",
            "printf '%s\\n' \"$1\"; exit \"$2\"",
            "reader-control",
            text,
            code,
        ]);
        aterm_uds::deadline::output_within(&mut command, Duration::from_secs(60)).unwrap()
    };
    let text = aterm_json::to_string(&expected).unwrap();
    assert!(answers(&run(&text, "0"), &expected));
    let failed = run(&text, "1");
    assert_eq!(failed.stdout, format!("{text}\n").as_bytes());
    assert!(
        !answers(&failed, &expected),
        "correct provenance cannot replace native success"
    );
    assert!(
        !answers(&run(super::super::super::HOP_CAPABILITY, "0"), &expected),
        "the old hop contract is not reader-schema support"
    );
    let mut foreign = expected.clone();
    foreign
        .as_object_mut()
        .unwrap()
        .insert("ledger".into(), aterm_json::Value::from("/another/ledger"));
    assert!(!answers(
        &run(&aterm_json::to_string(&foreign).unwrap(), "0"),
        &expected
    ));
    foreign = expected.clone();
    foreign.as_object_mut().unwrap().insert(
        "protocol".into(),
        aterm_json::Value::from("an older record grammar"),
    );
    assert!(!answers(
        &run(&aterm_json::to_string(&foreign).unwrap(), "0"),
        &expected
    ));
    foreign = expected.clone();
    foreign
        .as_object_mut()
        .unwrap()
        .insert("machine".into(), aterm_json::Value::from("another machine"));
    assert!(!answers(
        &run(&aterm_json::to_string(&foreign).unwrap(), "0"),
        &expected
    ));
    assert!(
        !ledger.dir().exists(),
        "query DATA never constructs a reservation"
    );
}

#[test]
fn legacy_capability_entry_never_receives_a_possible_target_word() {
    let root = aterm_tempfile::tempdir().unwrap();
    let target = root.path().join("target-entered");
    // Actual legacy two-arm front: known capability returns before target
    // dispatch. The selector is ignored, as in the old implementation.
    const OLD_ENTRY: &str = "if [ \"$#\" -eq 2 ] && [ \"$1\" = __build_admit ] && [ \"$2\" = --capabilities ]; then printf '%s\\n' aterm-build-admit-hop-v2; exit 0; fi; /usr/bin/touch \"$READER_TARGET\"";
    let command = || {
        let mut command = aterm_uds::spawnfd::command("/bin/sh");
        command
            .args(["-c", OLD_ENTRY, "legacy-front"])
            .env("READER_TARGET", &target);
        command
    };
    let mut safe = command();
    configure_query(&mut safe, &crate::shim_env::ShimEnv::NONE, Query::Reader);
    let output = aterm_uds::deadline::output_within(&mut safe, Duration::from_secs(60)).unwrap();
    assert!(output.status.success());
    assert_eq!(
        output.stdout,
        format!("{}\n", super::super::super::HOP_CAPABILITY).as_bytes()
    );
    assert!(!target.exists());
    let mut unsafe_probe = command();
    unsafe_probe.args(["__build_admit", "--retained-reservation-reader"]);
    let output =
        aterm_uds::deadline::output_within(&mut unsafe_probe, Duration::from_secs(60)).unwrap();
    assert!(output.status.success());
    assert!(
        target.exists(),
        "the target discriminator must actually fire on an unknown flag"
    );
}

#[test]
fn canonical_shim_exports_are_replayed_before_the_query_selector() {
    let environment = crate::shim_env::ShimEnv::admit(&[
        format!("{}=/foreign/ledger", super::super::super::LEDGER_ENV),
        format!("{QUERY_ENV}=inherited-selector"),
    ])
    .unwrap();
    let shim = Path::new("/managed/bin/targo");
    let generated = crate::platform::canonical_build_hop_shim(
        shim,
        Path::new("/managed/targo"),
        &environment,
        Path::new("/managed/atpkg"),
    )
    .unwrap();
    let text = String::from_utf8(generated.body).unwrap();
    hop_helper(shim, &text).unwrap();
    let actual = crate::platform::parse_sh_shim_env(&text);
    assert_eq!(actual, environment);
    let run = |query| {
        let mut command = aterm_uds::spawnfd::command("/bin/sh");
        command.args(["-c", "printf '%s\\n' \"$ATERM_BUILD_ADMIT_LEDGER\" \"${ATERM_BUILD_ADMIT_READER_QUERY-unset}\"", "query-environment"]);
        configure_query(&mut command, &actual, query);
        aterm_uds::deadline::output_within(&mut command, Duration::from_secs(60)).unwrap()
    };
    let hop = run(Query::Hop);
    assert!(hop.status.success());
    assert_eq!(hop.stdout, b"/foreign/ledger\nunset\n");
    let reader = run(Query::Reader);
    assert!(reader.status.success());
    assert_eq!(
        reader.stdout,
        format!("/foreign/ledger\n{QUERY}\n").as_bytes()
    );
}

#[test]
fn mixed_managed_readers_cannot_borrow_the_other_hops_support() {
    let root = aterm_tempfile::tempdir().unwrap();
    let ledger = aterm_capacity::Ledger::at(root.path().join("ledger"));
    let expected = super::super::capabilities_at(&ledger).unwrap();
    let text = aterm_json::to_string(&expected).unwrap();
    let mut foreign = expected.clone();
    foreign
        .as_object_mut()
        .unwrap()
        .insert("ledger".into(), aterm_json::Value::from("/foreign/ledger"));
    let foreign = aterm_json::to_string(&foreign).unwrap();
    // Distinct actual native helper bodies: an older reader ignores the
    // selector; a current reader reports its effective namespace.
    let run = |old: bool, environment: &crate::shim_env::ShimEnv| {
        let body = if old {
            "printf '%s\\n' aterm-build-admit-hop-v2"
        } else {
            "if [ \"${ATERM_BUILD_ADMIT_LEDGER-}\" = /foreign/ledger ]; then printf '%s\\n' \"$2\"; else printf '%s\\n' \"$1\"; fi"
        };
        let mut command = aterm_uds::spawnfd::command("/bin/sh");
        command.args(["-c", body, "mixed-reader", &text, &foreign]);
        configure_query(&mut command, environment, Query::Reader);
        aterm_uds::deadline::output_within(&mut command, Duration::from_secs(60)).unwrap()
    };
    let normal = crate::shim_env::ShimEnv::admit(&[format!(
        "{}={}",
        super::super::super::LEDGER_ENV,
        ledger.dir().display()
    )])
    .unwrap();
    let foreign_env = crate::shim_env::ShimEnv::admit(&[format!(
        "{}=/foreign/ledger",
        super::super::super::LEDGER_ENV
    )])
    .unwrap();
    let targo = run(false, &normal);
    assert!(answers(&targo, &expected));
    let both_current = run(false, &normal);
    assert!(answers(&targo, &expected) && answers(&both_current, &expected));
    let old_tippy = run(true, &normal);
    assert!(old_tippy.status.success());
    assert!(!answers(&old_tippy, &expected));
    let foreign_tippy = run(false, &foreign_env);
    assert_eq!(foreign_tippy.stdout, format!("{foreign}\n").as_bytes());
    assert!(!answers(&foreign_tippy, &expected));
    assert!(!ledger.dir().exists());
}
