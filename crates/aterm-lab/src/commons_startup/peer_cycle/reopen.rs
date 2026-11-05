// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Admit an owned app bundle and the exact running registration before a reopen request.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use aterm_json::Value;

use super::HANG;

const MAX_PLIST_BYTES: u64 = 1024 * 1024;

#[derive(Debug)]
pub(super) struct Bundle {
    pub path: PathBuf,
    pub executable: PathBuf,
    pub id: String,
}

fn app_root(executable: &Path) -> Result<&Path, String> {
    let macos = executable.parent().ok_or("missing app executable parent")?;
    let contents = macos.parent().ok_or("missing app Contents directory")?;
    let app = contents.parent().ok_or("missing app bundle directory")?;
    if executable.file_name() != Some(OsStr::new("aterm"))
        || macos.file_name() != Some(OsStr::new("MacOS"))
        || contents.file_name() != Some(OsStr::new("Contents"))
        || app.extension() != Some(OsStr::new("app"))
    {
        return Err("peer details require an .app/Contents/MacOS/aterm executable".into());
    }
    Ok(app)
}

fn canonical_paths(executable: &Path) -> Result<(PathBuf, PathBuf), String> {
    let root = app_root(executable)?;
    let path = std::fs::canonicalize(root).map_err(|error| format!("resolve app: {error}"))?;
    let executable = std::fs::canonicalize(executable)
        .map_err(|error| format!("resolve app executable: {error}"))?;
    if !executable.is_file()
        || app_root(&executable)? != path.as_path()
        || executable != path.join("Contents/MacOS/aterm")
    {
        return Err("app executable escapes its canonical bundle".into());
    }
    Ok((path, executable))
}

fn metadata(path: PathBuf, executable: PathBuf, body: &str) -> Result<Bundle, String> {
    let value: Value = aterm_json::from_str(body)
        .map_err(|error| format!("decode app Info.plist JSON: {error}"))?;
    if value.get("CFBundlePackageType").and_then(Value::as_str) != Some("APPL")
        || value.get("CFBundleExecutable").and_then(Value::as_str) != Some("aterm")
    {
        return Err("app metadata must identify an APPL bundle with executable aterm".into());
    }
    let id = value
        .get("CFBundleIdentifier")
        .and_then(Value::as_str)
        .filter(|id| {
            !id.is_empty()
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
        })
        .ok_or("app metadata has no supported bundle identifier")?;
    Ok(Bundle {
        path,
        executable,
        id: id.into(),
    })
}

/// Read only the canonical owned bundle's public metadata; never install or register it.
pub(super) fn bundle(executable: &Path) -> Result<Bundle, String> {
    let (path, executable) = canonical_paths(executable)?;
    let info = path.join("Contents/Info.plist");
    if std::fs::canonicalize(&info).map_err(|error| format!("resolve Info.plist: {error}"))? != info
    {
        return Err("Info.plist escapes its canonical bundle".into());
    }
    let size = std::fs::symlink_metadata(&info)
        .map_err(|error| format!("read Info.plist metadata: {error}"))?;
    if !size.is_file() || size.len() > MAX_PLIST_BYTES {
        return Err("app Info.plist is not a bounded regular file".into());
    }
    let mut command = aterm_uds::spawnfd::command("/usr/bin/plutil");
    command.args(["-convert", "json", "-o", "-"]).arg(&info);
    let output = aterm_uds::deadline::output_within(&mut command, HANG)
        .map_err(|error| format!("read app Info.plist: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "plutil could not read app Info.plist: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let body = std::str::from_utf8(&output.stdout)
        .map_err(|error| format!("app Info.plist JSON is not UTF-8: {error}"))?;
    metadata(path, executable, body)
}

fn asn_key(value: &str) -> Option<(u64, u64)> {
    let value = value.strip_suffix(':').unwrap_or(value);
    let (high, low) = value.strip_prefix("ASN:0x")?.split_once("-0x")?;
    let number = |part: &str| {
        (!part.is_empty() && part.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .then(|| u64::from_str_radix(part, 16).ok())
            .flatten()
    };
    let low = number(low)?;
    (low != 0).then_some((number(high)?, low))
}

/// A find result must identify exactly one running registration, never a name or bundle ID.
pub(super) fn single_asn(body: &str) -> Result<String, String> {
    let record = body.trim();
    if record.is_empty() {
        return Err("no registered app matches this bundle".into());
    }
    let value = if let Some(at) = record.find("-\"") {
        let (asn, label) = record.split_at(at);
        let label = label
            .strip_prefix('-')
            .and_then(|label| label.strip_suffix(':'))
            .filter(|label| label.ends_with('"'))
            .ok_or("invalid annotated LaunchServices find record")?;
        // The annotation is display metadata, never a process or bundle identity.
        let _: String =
            aterm_json::from_str(label).map_err(|_| "invalid LaunchServices display annotation")?;
        asn
    } else {
        record.strip_suffix(':').unwrap_or(record)
    };
    if value.ends_with(':') || asn_key(value).is_none() {
        return Err("expected exactly one LaunchServices ASN".into());
    }
    Ok(value.into())
}

fn once<T>(slot: &mut Option<T>, value: T) -> Result<(), String> {
    if slot.replace(value).is_some() {
        return Err("duplicate LaunchServices metadata field".into());
    }
    Ok(())
}

fn quoted(value: &str) -> Result<String, String> {
    aterm_json::from_str(value)
        .map_err(|error| format!("unsupported LaunchServices quoted value: {error}"))
}

fn info_header(value: &str) -> Result<(u64, u64), String> {
    let mut escaped = false;
    let mut end = None;
    for (index, byte) in value.bytes().enumerate().skip(1) {
        if escaped {
            escaped = false;
        } else if byte == b'\\' {
            escaped = true;
        } else if byte == b'"' {
            end = Some(index + 1);
            break;
        }
    }
    let end = end.ok_or("invalid LaunchServices application name")?;
    if quoted(&value[..end])?.is_empty() {
        return Err("empty LaunchServices application name".into());
    }
    let token = value[end..]
        .strip_prefix(" ASN:")
        .and_then(|suffix| suffix.split_ascii_whitespace().next())
        .filter(|token| token.ends_with(':'))
        .ok_or("invalid LaunchServices info header")?;
    // Anything after this token describes UI state, not identity or authority.
    asn_key(&format!("ASN:{token}")).ok_or_else(|| "invalid LaunchServices info header ASN".into())
}

/// Admit the saved-text shape of one exact registration; unsupported quoting fails closed.
pub(super) fn registered(body: &str, asn: &str, pid: u32, bundle: &Bundle) -> Result<(), String> {
    let expected_asn = asn_key(asn).ok_or("invalid expected LaunchServices ASN")?;
    let expected_path = bundle.path.to_str().ok_or("owned app path is not UTF-8")?;
    let expected_executable = bundle
        .executable
        .to_str()
        .ok_or("owned app executable path is not UTF-8")?;
    let (mut header, mut id, mut path, mut executable, mut found_pid) =
        (None, None, None, None, None);
    for line in body.lines() {
        let value = line.trim();
        if value.is_empty() {
            continue;
        }
        if value.starts_with('"') {
            once(&mut header, info_header(value)?)?;
        } else if let Some(value) = value.strip_prefix("bundleID=") {
            once(&mut id, quoted(value)?)?;
        } else if let Some(value) = value.strip_prefix("bundle path=") {
            once(&mut path, quoted(value)?)?;
        } else if let Some(value) = value.strip_prefix("executable path=") {
            once(&mut executable, quoted(value)?)?;
        } else if let Some(value) = value.strip_prefix("pid = ") {
            let digits = value
                .split_ascii_whitespace()
                .next()
                .filter(|value| {
                    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
                })
                .ok_or("invalid LaunchServices process ID")?;
            once(
                &mut found_pid,
                digits
                    .parse::<u32>()
                    .map_err(|_| "invalid LaunchServices process ID")?,
            )?;
        } else if !line.starts_with(char::is_whitespace) {
            return Err("unrecognized LaunchServices info record".into());
        }
    }
    if pid == 0
        || header != Some(expected_asn)
        || found_pid != Some(pid)
        || id.as_deref() != Some(bundle.id.as_str())
        || path.as_deref() != Some(expected_path)
        || executable.as_deref() != Some(expected_executable)
    {
        return Err("LaunchServices registration differs from the owned app instance".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
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
        let with_front_state =
            plain.replacen(&format!("{ASN}:"), &format!("{ASN}: (in front) "), 1);
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
}
