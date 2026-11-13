// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const ASN: &str = "ASN:0x0-0x895895";
const PID: u32 = 82104;

fn specimen() -> Bundle {
    let path = PathBuf::from("/owned/with spaces/aterm (dev).app");
    Bundle {
        executable: path.join("Contents/MacOS/aterm"),
        path,
        id: "com.aterm.aterm.dev".into(),
    }
}

fn info(bundle: &Bundle) -> String {
    format!(
        "\"aterm (dev)\" {ASN}:\n    bundleID={}\n    bundle path={}\n    executable path={}\n    pid = {PID} token=[sess=100020 pid={PID} uid:501,501,501] type=\"Foreground\"\n    checkin time = 84 seconds ago\n",
        aterm_json::to_string(&bundle.id).unwrap(),
        aterm_json::to_string(bundle.path.to_str().unwrap()).unwrap(),
        aterm_json::to_string(bundle.executable.to_str().unwrap()).unwrap(),
    )
}

#[test]
fn bundle_shape_and_canonical_executable_stay_inside_the_same_app() {
    for raw in [
        "/raw/aterm",
        "/owned/aterm.app/aterm",
        "/owned/aterm.app/Contents/aterm",
        "/owned/aterm.app/Contents/MacOS/other",
        "/owned/aterm.not-app/Contents/MacOS/aterm",
    ] {
        assert!(app_root(Path::new(raw)).is_err(), "{raw}");
    }
    let scratch = aterm_tempfile::Builder::new()
        .prefix("peer-reopen-")
        .suffix(".noindex")
        .tempdir()
        .unwrap();
    let executable = scratch.path().join("with spaces.app/Contents/MacOS/aterm");
    std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
    std::fs::write(&executable, b"never executed").unwrap();
    let (path, canonical) = canonical_paths(&executable).unwrap();
    assert_eq!(canonical, std::fs::canonicalize(&executable).unwrap());
    assert_eq!(canonical, path.join("Contents/MacOS/aterm"));

    #[cfg(unix)]
    {
        let other = scratch.path().join("other.app/Contents/MacOS/aterm");
        std::fs::create_dir_all(other.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&executable, &other).unwrap();
        assert!(
            canonical_paths(&other).is_err(),
            "even another app is an escape"
        );
    }
}

#[test]
fn metadata_requires_the_app_type_executable_and_public_identifier() {
    let good = r#"{"CFBundlePackageType":"APPL","CFBundleExecutable":"aterm","CFBundleIdentifier":"com.aterm.aterm.dev"}"#;
    let bundle = specimen();
    assert_eq!(
        metadata(bundle.path.clone(), bundle.executable.clone(), good)
            .unwrap()
            .id,
        bundle.id
    );
    for bad in [
        "{}".into(),
        good.replace("APPL", "BNDL"),
        good.replace("\"aterm\"", "\"other\""),
        good.replace("\"com.aterm.aterm.dev\"", "null"),
        good.replace("com.aterm.aterm.dev", ""),
        good.replace("com.aterm.aterm.dev", "com.exämple"),
        good.replace("com.aterm.aterm.dev", "com.other id=x"),
    ] {
        assert!(metadata(bundle.path.clone(), bundle.executable.clone(), &bad).is_err());
    }
}

#[test]
fn find_requires_one_real_asn() {
    assert_eq!(single_asn(&format!("  {ASN}\n")).unwrap(), ASN);
    assert_eq!(single_asn(&format!("{ASN}:\n")).unwrap(), ASN);
    for (annotated, expected) in [
        ("ASN:0x0-0x895895-\"aterm_(dev)\":", ASN),
        (
            "ASN:0x0-0x87e87e-\"aterm-repair-view-test-profile\":",
            "ASN:0x0-0x87e87e",
        ),
        ("ASN:0x0-0x895895-\"a different display label\":", ASN),
        (
            "ASN:0x0-0x895895-\"display \\\"quote\\\" and \\\\slash\":",
            ASN,
        ),
    ] {
        assert_eq!(single_asn(annotated).unwrap(), expected);
    }
    for bad in [
        "".into(),
        "[ NULL ]".into(),
        "com.aterm.aterm.dev".into(),
        "ASN:0x0-0x0".into(),
        "ASN:0x0-0xNO".into(),
        "ASN:0x0-0x895895::".into(),
        format!("{ASN}\n{ASN}"),
        format!("ERROR {ASN}"),
        format!("{ASN}-\"one\": {ASN}-\"two\":"),
        format!("{ASN}-\"one\":\n{ASN}"),
        format!("{ASN}-\"unterminated:"),
        format!("{ASN}-\"missing terminator\""),
        format!("{ASN}-\"one\": trailing"),
        format!("{ASN}-\"one\"::"),
        format!("{ASN}:-\"one\":"),
        format!("{ASN}-\"one\" :"),
        format!("{ASN}-[ NULL ]:"),
        "ASN:0x0-0xNO-\"aterm_(dev)\":".into(),
    ] {
        assert!(single_asn(&bad).is_err(), "{bad}");
    }
}

#[test]
fn exact_registration_keeps_spaces_and_escaped_path_characters() {
    let mut bundle = specimen();
    assert!(registered(&info(&bundle), ASN, PID, &bundle).is_ok());
    bundle.path = PathBuf::from("/owned/with \"quotes\" and \\slashes/aterm (dev).app");
    bundle.executable = bundle.path.join("Contents/MacOS/aterm");
    assert!(registered(&info(&bundle), ASN, PID, &bundle).is_ok());
}

#[test]
fn informational_front_state_does_not_change_the_header_identity() {
    let bundle = specimen();
    let plain = info(&bundle);
    assert!(registered(&plain, ASN, PID, &bundle).is_ok());
    let actual = "\"aterm (dev)\" ASN:0x0-0x8a48a4: (in front) ";
    assert_eq!(info_header(actual).unwrap(), (0, 0x8a48a4));
    let actual_body = plain.replacen(plain.lines().next().unwrap(), actual, 1);
    assert!(registered(&actual_body, "ASN:0x0-0x8a48a4", PID, &bundle).is_ok());
    assert!(registered(&actual_body, ASN, PID, &bundle).is_err());
    let with_front_state = plain.replacen(&format!("{ASN}:"), &format!("{ASN}: (in front) "), 1);
    assert!(registered(&with_front_state, ASN, PID, &bundle).is_ok());
    let name_with_identity_text =
        "\"name \\\"quote\\\" ASN:display\" ASN:0x0-0x895895: (status ASN:ignored)";
    assert_eq!(info_header(name_with_identity_text).unwrap(), (0, 0x895895));
    for bad in [
        "\"aterm (dev)\" ASN:0x0-0xNO: (in front)",
        "\"aterm (dev)\" ASN:0x0-0x8a48a4 (in front)",
        "\"aterm (dev)\" ASN:0x0-0x8a48a4:: (in front)",
    ] {
        assert!(info_header(bad).is_err());
    }
    assert!(
        registered(
            &format!("{with_front_state}{with_front_state}"),
            ASN,
            PID,
            &bundle
        )
        .is_err()
    );
}

#[test]
fn registration_rejects_changed_missing_duplicate_and_unsupported_fields() {
    let bundle = specimen();
    let good = info(&bundle);
    for bad in [
        good.replace("com.aterm.aterm.dev", "com.other"),
        good.replace("with spaces", "other app"),
        good.replace("Contents/MacOS/aterm", "Contents/MacOS/other"),
        good.replace("pid = 82104", "pid = 82105"),
        good.replace(ASN, "ASN:0x0-0x1234"),
        good.replace("bundleID=\"com.aterm.aterm.dev\"", "bundleID=[ NULL ]"),
        good.replace(
            "bundleID=\"com.aterm.aterm.dev\"",
            "bundleID=\"com.aterm.aterm.dev\" extra",
        ),
        good.replace("with spaces", "with \\U1234 escape"),
        good.lines().skip(1).collect::<Vec<_>>().join("\n"),
        format!("{good}{good}"),
        "[ NULL ]\n".into(),
    ] {
        assert!(registered(&bad, ASN, PID, &bundle).is_err(), "{bad}");
    }
    for prefix in ["bundleID=", "bundle path=", "executable path=", "pid = "] {
        let matching = good
            .lines()
            .find(|line| line.trim_start().starts_with(prefix))
            .unwrap();
        let missing = good
            .lines()
            .filter(|line| !line.trim_start().starts_with(prefix))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            registered(&missing, ASN, PID, &bundle).is_err(),
            "missing {prefix}"
        );
        let duplicate = format!("{good}{matching}\n");
        assert!(
            registered(&duplicate, ASN, PID, &bundle).is_err(),
            "duplicate {prefix}"
        );
    }
}
