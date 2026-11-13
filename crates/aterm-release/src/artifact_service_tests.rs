// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[cfg(unix)]
#[test]
fn the_ordinary_service_producer_remains_observable() {
    let root = aterm_tempfile::tempdir().unwrap();
    let executable = root.path().join("A&B <quoted>.app/Contents/MacOS/aterm");
    let text = aterm_secrets::service::plist_text(
        "systems.alab.aterm-node",
        &executable,
        &["node".into(), "serve".into()],
        &root.path().join("Library/Logs/aterm-secrets/service.log"),
    );
    assert_eq!(program(&text).unwrap(), executable);
}

#[test]
fn a_working_directory_does_not_rebase_an_absolute_executable() {
    let wrap = |body: &str| format!("<plist><dict>{body}</dict></plist>");
    for selection in [
        "<key>Program</key><string>/artifact/bin/aterm</string>",
        "<key>ProgramArguments</key><array><string>/artifact/bin/aterm</string><string>serve</string></array>",
    ] {
        let directory = "<key>WorkingDirectory</key><string>/another/directory</string>";
        assert_eq!(
            program(&wrap(&format!("{selection}{directory}"))).unwrap(),
            PathBuf::from("/artifact/bin/aterm")
        );
        for value in ["<true/>", "<string>relative</string>", "<string/>"] {
            assert!(
                program(&wrap(&format!(
                    "{selection}<key>WorkingDirectory</key>{value}"
                )))
                .is_err()
            );
        }
    }
    assert!(program(&wrap(
        "<key>Program</key><string>relative</string><key>WorkingDirectory</key><string>/artifact</string>"
    ))
    .is_err());
}

#[test]
fn ambiguous_or_unsupported_selections_are_unknown() {
    let executable = std::env::temp_dir().join("artifact-image");
    let escaped = aterm_codec::xml::escape(&executable.display().to_string());
    let selection = format!("<key>Program</key><string>{escaped}</string>");
    let wrap = |body: &str| format!("<plist><dict>{body}</dict></plist>");
    assert_eq!(program(&wrap(&selection)).unwrap(), executable);
    for body in [
        format!("{selection}{selection}"),
        format!("<!-- {selection} -->{selection}"),
        format!("{selection}<key>EnvironmentVariables</key><dict>{selection}</dict>"),
        format!("{selection}<key>RootDirectory</key><string>/other</string>"),
        format!("{selection}<key>BundleProgram</key><string>other</string>"),
        format!("{selection}<key>ProgramArguments</key><string>wrong type</string>"),
        "<key>Program</key><string>/bad&unknown;entity</string>".to_owned(),
    ] {
        assert!(
            program(&wrap(&body)).is_err(),
            "unsupported selection accepted"
        );
    }
}
