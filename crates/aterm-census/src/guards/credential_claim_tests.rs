// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::tests::{run, spots};
use super::*;

/// The entry recorded on 2026-11-12, its claim wrapped across lines.
const INCIDENT: &str = "# Known issues\n\
\n\
## ATERM-KI-1112-93 — secrets: the notary profile check runs notarytool\n\
\n\
Read: one case can still ask. Removing that last case rewires the release audit, and\n\
it cannot be validated without a release that reaches Apple, which needs the owner's release\n\
credential.\n\
Left: take notarytool out of the subscriber.\n";

/// THE INCIDENT'S BARE CLAIM IS REFUSED, at the line its possessive stands on.
#[test]
fn a_bare_claim_of_a_persons_credential_is_refused() {
    let found = run(&CREDENTIAL_CLAIM, &[(INDEX, INCIDENT)]);
    assert_eq!(spots(&found), ["docs/KNOWN_ISSUES.md:6"]);
    assert!(found[0].detail.contains("ATERM-KI-1112-93"), "{found:?}");
    assert!(found[0].detail.contains("aterm secrets ls --describe"));
    assert!(found[0].detail.contains("aterm secrets need"));
    // Saying the registry holds none, with no request filed, is no evidence: the need stands
    // unfiled.
    for bare in [
        "`aterm secrets ls --describe` shows the registry has no entry for it.",
        "Checked the registry: it holds no such key.",
    ] {
        let text = INCIDENT.replace("Left:", &format!("{bare}\nLeft:"));
        assert_eq!(
            spots(&run(&CREDENTIAL_CLAIM, &[(INDEX, &text)])),
            ["docs/KNOWN_ISSUES.md:6"],
            "{bare}"
        );
    }
    for claim in [
        // ATERM-KI-1112-94's Left line on 2026-11-12, beside -93's.
        "Left: republish the git bottle with `pack-bottle git … --upload` at the next build \
         number, which needs the owner's release credential, and let atpkg install it.",
        "Republishing it needs a person's GitHub token.",
        "Left: rotate it, which takes the owner\u{2019}s signing key.",
        "Blocked: the owner's login is needed.",
    ] {
        let text = format!("## ATERM-KI-1-1 — x\n\n{claim}\n");
        assert_eq!(
            spots(&run(&CREDENTIAL_CLAIM, &[(INDEX, &text)])),
            ["docs/KNOWN_ISSUES.md:3"],
            "{claim}"
        );
    }
}

/// NEGATIVE CONTROLS: the claim with the registry entry it checked, or the need request filed for
/// it, a rewording with no claim, and the same claim in another document pass.
#[test]
fn a_claim_that_names_the_registry_entry_or_the_need_request_passes() {
    for evidence in [
        "The registry holds `release-notary` and the release ring on the release machine.",
        "The registry holds none, so `aterm secrets need notary` is filed.",
        "Filed: `aterm secrets need gh:release`.",
    ] {
        let text = INCIDENT.replace("Left:", &format!("{evidence}\nLeft:"));
        assert!(
            run(&CREDENTIAL_CLAIM, &[(INDEX, &text)]).is_empty(),
            "{evidence}"
        );
    }
    let reworded = INCIDENT.replace(
        "which needs the owner's release\ncredential.",
        "which the release machine's next release exercises; no person or credential is \
         needed.",
    );
    assert!(run(&CREDENTIAL_CLAIM, &[(INDEX, &reworded)]).is_empty());
    assert!(run(&CREDENTIAL_CLAIM, &[("docs/RELEASING.md", INCIDENT)]).is_empty());
}

/// Evidence counts only in the entry it stands in: another entry's registry sentence does not
/// excuse a bare claim.
#[test]
fn evidence_counts_only_in_its_own_entry() {
    let text = format!(
        "{INCIDENT}\n## ATERM-KI-1112-94 — other\n\nThe registry holds `gh_access_token`.\n"
    );
    assert_eq!(
        spots(&run(&CREDENTIAL_CLAIM, &[(INDEX, &text)])),
        ["docs/KNOWN_ISSUES.md:6"]
    );
}

/// What is no claim: a possessive with no credential word, a credential word with no person's
/// possessive, quoted inline code, fenced code and HTML comments.
#[test]
fn quoted_code_and_other_possessives_are_no_claim() {
    for text in [
        "Deferred because a release is the owner's call.",
        "While a pane is rewound a person's keys reach no program.",
        "A person's arrow between one of aterm's keys and the next read is no touch.",
        "A person's removal of a Mac takes out the successors it admitted with no fleet node key.",
        "What is left is a person's own sign-in in a window.",
        "dev's login is not in the vault adoption record.",
        "The parser reads `NEEDS YOU is the owner's key` as no need.",
        "```\nneeds the owner's release credential\n```",
        "<!-- needs the owner's release credential -->",
        "    needs the owner's release credential (an indented example)",
    ] {
        let text = format!("## ATERM-KI-1-1 — x\n\n{text}\n");
        assert!(
            run(&CREDENTIAL_CLAIM, &[(INDEX, &text)]).is_empty(),
            "{text}"
        );
    }
}
