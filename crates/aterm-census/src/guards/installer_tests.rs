// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::super::tests::{run, spots};
use super::*;

const TRANSPORT: &str = "web_curl() {\n\tcurl --proto '=https' --proto-redir '=https' \"$@\"\n}\n";

#[test]
fn the_installer_fetches_only_through_its_transport() {
    let clean = format!(
        "#!/bin/sh\n{TRANSPORT}if ! web_curl -fsSL \"$url\"; then exit 1; fi\n\
         # a comment may say curl or gh without running them\n"
    );
    assert!(run(&INSTALLER_NETWORK, &[(INSTALLER, &clean)]).is_empty());
    let broken = format!(
        "#!/bin/sh\n{TRANSPORT}curl -fsSL \"$url\"\nif ! curl -L x; then :; fi\n\
         head=$(gh release view)\nx=https://api.github.com/repos\nsign | gh api y\n"
    );
    let found = run(
        &INSTALLER_NETWORK,
        &[(INSTALLER, &broken), ("tools/other.sh", &broken)],
    );
    assert_eq!(
        spots(&found),
        [
            "tools/install.sh:5",
            "tools/install.sh:6",
            "tools/install.sh:7",
            "tools/install.sh:8",
            "tools/install.sh:9",
        ],
        "only the installer is fenced: {found:?}"
    );
    let lost = run(&INSTALLER_NETWORK, &[(INSTALLER, "#!/bin/sh\nwget x\n")]);
    assert_eq!(spots(&lost), ["tools/install.sh:0"]);
}

/// A walk that reads no installer lost the scripts: other scripts do not
/// stand in for it.
#[test]
fn a_walk_that_misses_the_installer_is_a_finding() {
    let clean = format!("#!/bin/sh\n{TRANSPORT}");
    for files in [&[][..], &[("tools/other.sh", clean.as_str())][..]] {
        let found = run(&INSTALLER_NETWORK, files);
        assert!(
            found.len() == 1 && found[0].detail.starts_with("read no tools/install.sh"),
            "{found:?}"
        );
    }
    assert!(run(&INSTALLER_NETWORK, &[(INSTALLER, &clean)]).is_empty());
}
