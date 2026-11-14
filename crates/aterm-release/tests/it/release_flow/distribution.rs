// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Step 1's proof of what the publish, cut and after-cut steps write with
//! ([`crate::release::Backend::distribution`]) against the fake world: a credential the last
//! steps need is refused before anything is built, a dry run says the same, and a private
//! qualification, which makes none of those writes, is not asked.

use super::{TIP, World, bump, mac};
use crate::provision::{Audited, Finding};
use crate::release::ReleaseOptions;

/// The site rows as the 0.199.0 host had them: the checkout there, the deployer's key not.
fn key_gap(asked: &str) -> Vec<Finding> {
    vec![
        Finding {
            label: "release.site",
            state: Audited::Pass,
        },
        Finding {
            label: "release.site-key",
            state: Audited::Gap {
                what: format!(
                    "the site's service-account key is unusable: cannot read \
                     /h/config/local/service-account.alab-site-deployer.json; {asked}"
                ),
                fix: "wait for the vault to deliver the key to \
                      /h/config/local/service-account.alab-site-deployer.json"
                    .into(),
                owner: false,
            },
        },
    ]
}

fn proved() -> Vec<Finding> {
    ["release.site", "release.site-key", "release.tap"]
        .into_iter()
        .map(|label| Finding {
            label,
            state: Audited::Pass,
        })
        .collect()
}

/// A CREDENTIAL THE AFTER-CUT STEP NEEDS REFUSES AT STEP 1. The 0.199.0 release found it had
/// no site deployer key at step 9, with the app public. Here the same gap refuses at 1/10:
/// nothing is bumped, staged, built or published, and the refusal carries the vault's request
/// and the remedy.
/// NEGATIVE CONTROL: the same world with every row proved releases; without the step-1 ask the
/// gap is met only where the after-cut step meets it.
#[test]
fn a_missing_distribution_credential_refuses_at_step_one() {
    let mut w = World::new();
    w.distribution = key_gap("asked of the lab's vault now (site-deployer-key: posted)");
    let (code, last) = w.run(bump());
    assert_eq!(code, 1, "{last}\n{:#?}", w.out);
    assert!(last.starts_with("REFUSED at 1/10 checks"), "{last}");
    for part in [
        "release.site-key",
        "asked of the lab's vault now",
        "wait for the vault to deliver the key",
    ] {
        assert!(last.contains(part), "{part}: {last}");
    }
    assert_eq!(w.distribution_dry, [false], "asked once, as a real run");
    assert!(w.journal().is_none(), "no judged candidate journaled");
    for call in ["stage", "publish"] {
        assert_eq!(w.count(call), 0, "{call}: {:?}", w.calls);
    }

    let mut ok = World::new();
    ok.distribution = proved();
    let (code, last) = ok.run(bump());
    assert_eq!(code, 0, "{last}\n{:#?}", ok.out);
    assert!(last.starts_with("RELEASED aterm 0.6.0"), "{last}");
    assert!(
        ok.distribution_dry.iter().all(|dry| !dry) && !ok.distribution_dry.is_empty(),
        "{:?}",
        ok.distribution_dry
    );
}

/// A DRY RUN REPORTS THE SAME GAP AND ASKS AS A DRY RUN, so the backend raises nothing.
#[test]
fn a_dry_run_reports_the_missing_distribution_credential() {
    let mut w = World::new();
    w.distribution = key_gap("a real run asks the lab's vault for it (no need is on file yet)");
    let (code, last) = w.run(ReleaseOptions {
        dry_run: true,
        ..bump()
    });
    assert_eq!(code, 1, "{last}\n{:#?}", w.out);
    assert!(last.contains("would refuse at 1/10 checks"), "{last}");
    assert!(
        w.out.iter().any(|line| line.contains("WOULD REFUSE")
            && line.contains("release.site-key")
            && line.contains("a real run asks the lab's vault for it")),
        "{:#?}",
        w.out
    );
    assert!(
        w.distribution_dry.iter().all(|dry| *dry) && !w.distribution_dry.is_empty(),
        "{:?}",
        w.distribution_dry
    );
    assert!(w.journal().is_none(), "a dry run writes no journal");
}

/// A PRIVATE QUALIFICATION MAKES NONE OF THOSE WRITES AND IS NOT ASKED: a Mac with no site
/// key still qualifies.
#[test]
fn a_private_qualification_is_not_asked_for_distribution_credentials() {
    let mut w = World::new();
    w.versions.insert(TIP.into(), "0.6.0".into());
    w.distribution = key_gap("asked of the lab's vault now");
    let (code, last) = w.run(ReleaseOptions {
        commit: Some(TIP.into()),
        qualify: true,
        ..mac()
    });
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert!(last.starts_with("QUALIFIED aterm 0.6.0"), "{last}");
    assert!(w.distribution_dry.is_empty(), "{:?}", w.distribution_dry);
}
