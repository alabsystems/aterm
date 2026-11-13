// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_verb_lists_raises_and_ends_and_refuses_junk() {
    assert_eq!(parse("", false), Ok(Asked::List));
    assert!(list_line().contains(&format!(
        "{}:{}",
        notice::LOGIN_SIGN_IN.code,
        notice::LOGIN_SIGN_IN.id
    )));
    assert_eq!(
        parse("si", true),
        Ok(Asked::Raise {
            text: &notice::LOGIN_SIGN_IN,
            label: LABEL.to_string()
        })
    );
    assert_eq!(
        parse("login-sign-in-as pool-1", true),
        Ok(Asked::Raise {
            text: &notice::LOGIN_SIGN_IN_AS,
            label: "pool-1".to_string()
        })
    );
    assert_eq!(parse("end", true), Ok(Asked::End));
    assert_eq!(parse("back", true), Ok(Asked::Back));
    assert_eq!(parse("disk", true), Ok(Asked::Disk(DiskFullCause::Spent)));
    assert_eq!(
        parse("disk inodes", true),
        Ok(Asked::Disk(DiskFullCause::Inodes))
    );
    assert!(matches!(
        notes(&Asked::Disk(DiskFullCause::Spent), "s-1").as_slice(),
        [HostNote::DiskFull { .. }]
    ));
    assert!(parse("si", false).is_err());
    assert!(parse("end", false).is_err());
    assert!(parse("back", false).is_err());
    assert!(parse("disk", false).is_err());
    assert!(parse("disk full", true).is_err());
    assert!(parse("zz", true).is_err());
    assert!(parse("si a b", true).is_err());
    assert!(parse("si bad/label", true).is_err());
}

#[test]
fn each_notice_goes_the_way_the_harness_tells_it() {
    for text in notice::ALL {
        let asked = Asked::Raise {
            text,
            label: LABEL.to_string(),
        };
        let notes = notes(&asked, "s-1");
        assert_eq!(notes.len(), 1, "{}", text.id);
        let accounts = matches!(notes[0], HostNote::Accounts(_));
        assert_eq!(
            accounts,
            [
                notice::ACCOUNT_SIGNED_OUT.id,
                notice::RENEWAL_WRONG_ACCOUNT.id
            ]
            .contains(&text.id),
            "{}",
            text.id
        );
        if let HostNote::ResumeException {
            standing: Some(e), ..
        } = &notes[0]
        {
            assert_eq!(e.notice.as_ref().map(|n| n.id), Some(text.id));
            let did = e.notice.as_ref().map(|n| n.did.clone()).unwrap_or_default();
            assert!(!did.contains('{'), "{}: {did}", text.id);
            assert!(!e.action.contains('{'), "{}: {}", text.id, e.action);
            let did = format!("{did} {}", e.action);
            assert_eq!(
                text.whys.iter().any(|w| did.contains(w)),
                !text.whys.is_empty(),
                "{}: {did}",
                text.id
            );
            for other in notice::ALL.iter().flat_map(|n| n.whys) {
                if !text.whys.contains(other) {
                    assert!(!did.contains(other), "{}: {did}", text.id);
                }
            }
            let expect = e
                .notice
                .as_ref()
                .map(|n| n.expect.clone())
                .unwrap_or_default();
            assert_eq!(
                expect.contains("on Oct 23 at 9am"),
                text.expect_reset.is_some(),
                "{}: {expect}",
                text.id
            );
        }
    }
}
