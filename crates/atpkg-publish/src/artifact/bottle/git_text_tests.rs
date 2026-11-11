// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn script_literals_relocate_and_unrecognized_uses_refuse() {
    let prefix = "@@HOMEBREW_CELLAR@@/git/2.56.0";
    assert_eq!(
        literals(&format!("our $GIT = \"{prefix}/bin/git\";\n"), prefix, true).unwrap(),
        "our $GIT = \"$ENV{ATERM_GIT_ROOT}/bin/git\";\n"
    );
    assert_eq!(
        literals(&format!("root='{prefix}/share/gitweb'\n"), prefix, false).unwrap(),
        "root=\"${ATERM_GIT_ROOT}/share/gitweb\"\n"
    );
    assert!(literals(&format!("root={prefix}/share/gitweb\n"), prefix, false).is_err());
    assert!(literals(&format!("root='{prefix}/$unsafe'\n"), prefix, false).is_err());
    assert!(literals(&format!("our $root='{prefix}/@unsafe';\n"), prefix, true).is_err());
}

#[test]
fn formula_revision_is_part_of_the_relocated_prefix() {
    let prefix = "@@HOMEBREW_CELLAR@@/git/2.56.0";
    let text = format!("root='{prefix}_2/share/locale'\n");
    let actual = bottled_prefixes(&text, prefix).unwrap();
    assert_eq!(actual, [format!("{prefix}_2")]);
    assert_eq!(
        literals(&text, &actual[0], false).unwrap(),
        "root=\"${ATERM_GIT_ROOT}/share/locale\"\n"
    );
    assert!(bottled_prefixes(&format!("root='{prefix}1/share'"), prefix).is_err());
    assert!(bottled_prefixes(&format!("root='{prefix}_bad/share'"), prefix).is_err());
}

#[cfg(target_os = "macos")]
#[test]
fn relocated_helpers_find_their_data_after_the_package_moves() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let stage = temp.path().join("stage");
    std::fs::create_dir_all(stage.join("libexec/git-core")).unwrap();
    std::fs::create_dir_all(stage.join("share/perl5")).unwrap();
    std::fs::write(
        stage.join("share/perl5/PackageProbe.pm"),
        "package PackageProbe; sub value { 'loaded package module' } 1;\n",
    )
    .unwrap();
    std::fs::write(stage.join("libexec/git-core/probe"),
        "#!/usr/bin/perl\nuse strict; use warnings;\nuse lib ('@@HOMEBREW_CELLAR@@/git/2.56.0/share/perl5');\nuse PackageProbe; print PackageProbe::value();\n").unwrap();
    std::fs::write(
        stage.join("libexec/git-core/shell-probe"),
        "#!/bin/sh\nroot='@@HOMEBREW_CELLAR@@/git/2.56.0/share/gitweb'\nprintf '%s' \"$root\"\n",
    )
    .unwrap();
    rewrite(&stage, "2.56.0").unwrap();
    let moved = temp.path().join("moved Git package");
    std::fs::rename(&stage, &moved).unwrap();
    let output = aterm_uds::spawnfd::command("/usr/bin/perl")
        .arg(moved.join("libexec/git-core/probe"))
        .env_remove("ATERM_GIT_ROOT")
        .env_remove("PERL5LIB")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"loaded package module");
    let output = aterm_uds::spawnfd::command("/bin/sh")
        .arg(moved.join("libexec/git-core/shell-probe"))
        .env("ATERM_GIT_ROOT", &moved)
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        moved.join("share/gitweb").to_string_lossy()
    );
}
