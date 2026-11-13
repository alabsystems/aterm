// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::os::unix::fs::PermissionsExt as _;

#[test]
fn publication_pin_contract_is_required_without_a_path_interpreter_probe() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let script = dir.path().join("pub");
    let helper = script.with_file_name("pub-python");
    for body in [
        "#!/bin/sh\nexec python3.14 \"$@\"\n",
        "#!/bin/sh\nprintf old-contract\\n\n",
        "#!/bin/sh\nprintf '%s\\n' publication-pinned-python-v1\nexit 1\n",
    ] {
        std::fs::write(&helper, body).unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(require_engine_pin(&script).is_err());
    }
    std::fs::write(&helper, "#!/bin/sh\n[ -z \"$PATH\" ] || exit 1\n[ \"$#:$1\" = '1:--capabilities' ] || exit 1\nprintf '%s\\n' publication-pinned-python-v1\n").unwrap();
    require_engine_pin(&script).unwrap();
    std::fs::remove_file(&helper).unwrap();
    assert!(require_engine_pin(&script).is_err());
}

/// R12: a content key names the held command directory by what it exposes, so two runs that
/// hold the same signed builds under different temporary directories have one PATH to hash.
/// NEGATIVE CONTROLS: another signed build, another directory elsewhere on the PATH, and
/// another order are each another PATH; a PATH without the held directory is unchanged.
#[test]
fn a_content_key_names_the_held_directory_by_its_signed_tools() {
    let path = |held: &str| std::ffi::OsString::from(format!("{held}:/opt/rustup/bin:/usr/bin"));
    let one = Path::new("/tmp/.ubd/atpkg-prerequisites-aaaa.noindex");
    let two = Path::new("/tmp/.ubd/atpkg-prerequisites-bbbb.noindex");
    let tools = "gitleaks=gitleaks@7:1f2e";
    let first = path_naming(&path(one.to_str().unwrap()), one, tools);
    assert_eq!(
        first,
        path_naming(&path(two.to_str().unwrap()), two, tools),
        "the same tools held by another run"
    );
    assert_eq!(
        first,
        "<signed prerequisites: gitleaks=gitleaks@7:1f2e>:/opt/rustup/bin:/usr/bin"
    );
    assert_ne!(
        first,
        path_naming(
            &path(one.to_str().unwrap()),
            one,
            "gitleaks=gitleaks@8:9a0b"
        )
    );
    let elsewhere = std::ffi::OsString::from(format!("{}:/opt/other/bin:/usr/bin", one.display()));
    assert_ne!(first, path_naming(&elsewhere, one, tools));
    let reordered = std::ffi::OsString::from(format!("/opt/rustup/bin:{}:/usr/bin", one.display()));
    assert_ne!(first, path_naming(&reordered, one, tools));
    let without = std::ffi::OsString::from("/opt/rustup/bin:/usr/bin");
    assert_eq!(path_naming(&without, one, tools), without);
}
