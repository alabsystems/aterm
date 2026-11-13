// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[cfg(unix)]
#[test]
fn verification_allowance_admits_scheduled_local_work() {
    let deadline = Instant::now() - Duration::from_secs(31) + verify_timeout_for(0);
    assert!(
        deadline > Instant::now(),
        "required local work remains admissible"
    );
    let program = aterm_uds::deadline::stand_in(Path::new("/bin/echo"));
    let mut command = aterm_uds::spawnfd::command(&program);
    command.env_clear().arg("ready");
    let output = output_until(&mut command, deadline).expect("native local work completes");
    assert_eq!(output.stdout, b"ready\n");
    assert!(developer_id_verdict(Ok(output)).is_ok());

    let (input, held) = std::os::unix::net::UnixStream::pair().unwrap();
    let program = aterm_uds::deadline::stand_in(Path::new("/bin/cat"));
    let mut blocked = aterm_uds::spawnfd::command(&program);
    blocked.env_clear();
    let spent = Instant::now() - Duration::from_secs(61) + verify_timeout_for(0);
    let result = developer_id_verdict(aterm_uds::deadline::output_until_from(
        &mut blocked,
        aterm_uds::spawnfd::Stdio::from(std::os::fd::OwnedFd::from(input)),
        spent,
    ));
    drop(held);
    let error = result.unwrap_err();
    assert!(matches!(error, CodesignError::TimedOut));
    assert!(!error.is_verdict());
}

/// The requirement is Apple's Developer ID shape with the team spliced in verbatim.
#[test]
fn the_requirement_pins_anchor_chain_and_team() {
    assert_eq!(
        developer_id_requirement("Q6L2SF6YDW").unwrap(),
        "=anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists \
         and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate \
         leaf[subject.OU] = \"Q6L2SF6YDW\""
    );
}

/// A team that could rewrite the requirement, or is empty, is refused before any spawn.
#[test]
fn a_team_that_is_not_alphanumeric_is_refused_before_any_spawn() {
    for bad in ["", "Q6L2\"SF6YD", "ABC DEF", "ÄBCDEFGHIJ", "A\"or\"B"] {
        assert!(
            matches!(
                verify_developer_id(Path::new("/nonexistent"), bad, false),
                Err(CodesignError::InvalidTeam(_))
            ),
            "{bad:?}"
        );
    }
}

#[cfg(not(target_os = "macos"))]
#[test]
fn off_macos_there_is_no_anchor_to_check() {
    assert!(matches!(
        verify_developer_id(Path::new("/bin/sh"), "Q6L2SF6YDW", false),
        Err(CodesignError::Unsupported)
    ));
}

/// Exit 3, and exit 1 naming a signature fault, are verdicts; exit 1 for an unreadable
/// file or internal failure, any other exit, a signal and a failure to run are not.
#[test]
fn only_a_signature_fault_is_a_verdict() {
    let refused = |code: Option<i32>, stderr: &str| CodesignError::Refused {
        code,
        stderr: stderr.to_string(),
    };
    for stderr in [
        "bin/claude: code object is not signed at all\n",
        "bin/claude: invalid signature (code or signature have been modified)\n\
         In architecture: arm64\n",
        "x: main executable failed strict validation",
        "x: object file format unrecognized, invalid, or unsuitable\n",
        "aterm.app: a sealed resource is missing or invalid\n",
        "aterm.app: unsealed contents present in the bundle root\n",
        "A.app: invalid Info.plist (plist or signature have been modified)\n",
        "A.app: invalid resource directory (directory or signature have been modified)\n\
         In subcomponent: /A.app/Contents/Helpers/h\n",
        "aterm.app: bundle format unrecognized, invalid, or unsuitable\n",
        "A.app: invalid destination for symbolic link in bundle\n\
         file modified: /A.app/Contents/Resources/h: Permission denied\n",
    ] {
        assert!(refused(Some(1), stderr).is_verdict(), "{stderr:?}");
        let verified = Path::new(stderr.split(": ").next().unwrap_or_default());
        assert!(
            !refused(Some(1), stderr).judged_nothing_at(verified),
            "{stderr:?}"
        );
    }
    assert!(
        refused(
            Some(3),
            "test-requirement: code failed to satisfy specified code requirement(s)\n"
        )
        .is_verdict()
    );
    for stderr in [
        "bin/claude: Permission denied\n",
        "bin/claude: No such file or directory\n",
        "bin/claude: internal error in Code Signing subsystem\n",
        "bin/claude: the code cannot be read by the verifier (file system permissions etc.)",
        "",
    ] {
        assert!(!refused(Some(1), stderr).is_verdict(), "{stderr:?}");
    }
    assert!(!refused(Some(2), "code object is not signed at all").is_verdict());
    assert!(!refused(None, "code object is not signed at all").is_verdict());
    assert!(!CodesignError::TimedOut.is_verdict());
    assert!(!CodesignError::Unsupported.is_verdict());
    assert!(!CodesignError::InvalidTeam(String::new()).is_verdict());
}

/// A signal, and exit 1 whose first line names only a failure to read or run on the
/// verified path, judged nothing.
#[test]
fn only_a_known_machine_fault_or_a_signal_judged_nothing() {
    let refused = |code: Option<i32>, stderr: &str| CodesignError::Refused {
        code,
        stderr: stderr.to_string(),
    };
    assert!(refused(None, "").judged_nothing_at(Path::new("bin/claude")));
    for stderr in [
        "bin/claude: Permission denied\n",
        "\n  bin/claude: Permission denied  \n",
        "bin/claude: internal error in Code Signing subsystem\n",
        "bin/claude: the code cannot be read by the verifier (file system permissions etc.)",
    ] {
        assert!(
            refused(Some(1), stderr).judged_nothing_at(Path::new("bin/claude")),
            "{stderr:?}"
        );
    }
    let progress = [
        (
            "/x/N1.app",
            "--prepared:/x/N1.app/Contents/Helpers/h\nX: Permission denied\n\
             --validated:/x/N1.app/Contents/Helpers/h\nX: Permission denied\n\
             /x/N1.app: nested code is modified or invalid\n"
                .to_string(),
        ),
        (
            "/x/C.app",
            "--prepared:/x/C.app/Contents/Helpers/h: Permission denied\n\
             --validated:/x/C.app/Contents/Helpers/h: Permission denied\n\
             /x/C.app: nested code is modified or invalid\n"
                .to_string(),
        ),
        (
            "/x/A.app",
            format!(
                "/x/A.app: Permission denied\n{}/x/A.app: nested code is modified or \
                 invalid\n",
                "--prepared:/x/A.app/Contents/Helpers/h\n".repeat(STDERR_KEEP / 32)
            ),
        ),
    ];
    for (verified, stderr) in &progress {
        assert!(
            !refused(Some(1), stderr).judged_nothing_at(Path::new(verified)),
            "{stderr:.300}"
        );
    }
    for (code, verified, stderr) in [
        (
            Some(1),
            "A.app",
            "A.app: nested code is modified or invalid\n",
        ),
        (Some(1), "A.app", ""),
        (Some(1), "/x/A.app", "/x/Other.app: Permission denied\n"),
        (Some(1), "A.app", "A.app: No such file or directory\n"),
        (
            Some(1),
            "A.app",
            "A.app: invalid destination for symbolic link in bundle\n\
             file modified: /A.app/Contents/Resources/h: Permission denied\n",
        ),
        (
            Some(1),
            "A.app",
            "A.app: a verdict nobody has measured yet\n\
             In subcomponent: /A.app/Contents/Helpers/h: Permission denied\n",
        ),
        (Some(1), "A.app", "A.app: unPermission denied\n"),
        (
            Some(1),
            "A.app",
            "A.app: Permission denied\nA.app: code object is not signed at all\n",
        ),
        (Some(2), "bin/claude", "bin/claude: Permission denied\n"),
        (
            Some(3),
            "A.app",
            "code failed to satisfy specified code requirement(s)\n",
        ),
    ] {
        assert!(
            !refused(code, stderr).judged_nothing_at(Path::new(verified)),
            "{code:?} {stderr:?}"
        );
    }
    assert!(!CodesignError::TimedOut.judged_nothing_at(Path::new("A.app")));
}

/// Exit 1 for a file codesign cannot read is not a verdict (the bytes verify once
/// readable); exit 1 for an unsigned file is.
#[cfg(target_os = "macos")]
#[test]
fn an_unreadable_file_is_not_a_verdict_and_an_unsigned_one_is() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = crate::test_scratch_dir("codesign-verdict");
    let file = dir.path().join("thin");
    let mut thin = 0xCFFA_EDFEu32.to_be_bytes().to_vec();
    thin.extend_from_slice(&[0u8; 60]);
    std::fs::write(&file, &thin).unwrap();
    let unsigned = verify_developer_id(&file, "Q6L2SF6YDW", false).unwrap_err();
    assert!(unsigned.is_verdict(), "{unsigned}");

    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o000)).unwrap();
    let premise = std::fs::read(&file).is_err();
    let unreadable = verify_developer_id(&file, "Q6L2SF6YDW", false).unwrap_err();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
    if premise {
        assert!(
            matches!(unreadable, CodesignError::Refused { code: Some(1), .. }),
            "{unreadable:?}"
        );
        assert!(!unreadable.is_verdict(), "{unreadable}");
        assert!(unreadable.judged_nothing_at(&file), "{unreadable}");
    }
    assert!(!unsigned.judged_nothing_at(&file), "{unsigned}");
}

/// A small file keeps a minute, a 238 MB binary gets 173 s, nothing more than the cap.
#[test]
fn the_verify_deadline_scales_with_the_file_and_is_capped() {
    assert_eq!(verify_timeout_for(0), VERIFY_TIMEOUT);
    assert_eq!(verify_timeout_for(64 * 1024), VERIFY_TIMEOUT);
    assert_eq!(
        verify_timeout_for(238_223_808),
        Duration::from_secs(60 + 113)
    );
    assert!(verify_timeout_for(238_223_808) > VERIFY_TIMEOUT);
    assert_eq!(verify_timeout_for(u64::MAX), VERIFY_TIMEOUT_MAX);
    assert_eq!(
        verify_timeout_for(10 * 1024 * 1024 * 1024),
        VERIFY_TIMEOUT_MAX
    );
}

/// Apple's own binaries carry no Developer ID: the requirement refuses them (exit 3, or 1
/// where a platform binary has no leaf OU), so the team pin does work.
#[cfg(target_os = "macos")]
#[test]
fn a_validly_signed_binary_of_another_signer_is_refused() {
    aterm_uds::deadline::require_trust_service();
    match verify_developer_id(Path::new("/bin/ls"), "Q6L2SF6YDW", false) {
        Err(CodesignError::Refused { code, .. }) => {
            assert!(matches!(code, Some(1 | 3)), "{code:?}");
        }
        other => panic!("{other:?}"),
    }
}
