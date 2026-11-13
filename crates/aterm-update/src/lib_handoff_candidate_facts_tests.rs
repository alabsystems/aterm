// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use aterm_update_core::handoff_policy::{PolicyFile, PolicyRead};

#[test]
fn an_unread_policy_refuses_the_preverification_as_a_passing_condition() {
    let unread = PolicyRead::Unread(
        "/x/Contents/Resources/aterm-handoff-policy.toml: unreadable: Too many open files \
         (os error 24)"
            .into(),
    );
    let refused = HandoffCandidateFacts::admitted(unread, Ok(bundle::Capabilities::default()))
        .expect_err("an unread policy is not a pass");
    assert!(is_passing_refusal(&refused), "{refused}");
    for policy in [
        PolicyRead::Absent,
        PolicyRead::Ignored("not TOML".into()),
        PolicyRead::Parsed(PolicyFile::default()),
    ] {
        assert_eq!(
            HandoffCandidateFacts::admitted(
                policy.clone(),
                Ok(bundle::Capabilities {
                    grant_chunks: true,
                    grant_empty: true,
                    grant_desk: true,
                    recipient_recovery: NativeRecipientRecovery::BoundJournal,
                })
            ),
            Ok(HandoffCandidateFacts {
                policy: policy.clone(),
                grant_chunks: true,
                grant_empty: true,
                grant_desk: true,
                recipient_recovery: NativeRecipientRecovery::BoundJournal,
            }),
            "{policy:?}"
        );
    }
}

#[test]
fn an_info_plist_read_the_system_refused_for_now_refuses_the_preverification() {
    let declared = "<plist><dict><key>ATermHandoffGrantChunks</key>\
                    <string>chunks1</string></dict></plist>";
    let plist = std::path::Path::new("/x/aterm.app/Contents/Info.plist");
    let read = |errno: i32| {
        bundle::Capabilities::judge(plist, Err(std::io::Error::from_raw_os_error(errno)))
    };
    for errno in [libc::EMFILE, libc::ENFILE, libc::ENOMEM, libc::EIO] {
        let refused = HandoffCandidateFacts::admitted(PolicyRead::Absent, read(errno))
            .expect_err("a moment's read of the Info.plist is not a pass");
        assert!(is_passing_refusal(&refused), "errno {errno}: {refused}");
    }
    assert_eq!(
        HandoffCandidateFacts::admitted(PolicyRead::Absent, read(libc::EACCES)),
        Ok(HandoffCandidateFacts {
            policy: PolicyRead::Absent,
            grant_chunks: false,
            grant_empty: false,
            grant_desk: false,
            recipient_recovery: NativeRecipientRecovery::Absent,
        })
    );
    assert_eq!(
        bundle::Capabilities::judge(plist, Ok(Some(declared.to_string()))),
        Ok(bundle::Capabilities {
            grant_chunks: true,
            ..Default::default()
        })
    );
    assert_eq!(
        bundle::Capabilities::judge(plist, Ok(None)),
        Ok(bundle::Capabilities::default())
    );
}

#[test]
fn capability_snapshots_follow_the_bundle_on_disk_without_changing_prior_reads() {
    let root = crate::paths::scratch_dir("grant-chunks");
    let contents = root.join("aterm.app/Contents");
    std::fs::create_dir_all(&contents).unwrap();
    let app = root.join("aterm.app");
    let grants = |read: Result<HandoffCandidateFacts, String>| {
        read.map(|facts| {
            (
                facts.grant_chunks,
                facts.grant_empty,
                facts.grant_desk,
                facts.recipient_recovery,
            )
        })
    };
    assert_eq!(
        grants(HandoffCandidateFacts::read(&app)),
        Ok((false, false, false, NativeRecipientRecovery::Absent))
    );
    std::fs::write(
        contents.join("Info.plist"),
        "<plist><dict><key>ATermHandoffGrantChunks</key><string>chunks1</string>\
         </dict></plist>",
    )
    .unwrap();
    let chunks_only = HandoffCandidateFacts::read(&app);
    std::fs::write(
        contents.join("Info.plist"),
        "<plist><dict><key>ATermHandoffGrantChunks</key><string>chunks1</string>\
         <key>ATermHandoffGrantEmpty</key><string>empty1</string></dict></plist>",
    )
    .unwrap();
    let both = HandoffCandidateFacts::read(&app);
    std::fs::write(
        contents.join("Info.plist"),
        "<plist><dict><key>ATermHandoffGrantChunks</key><string>chunks1</string>\
         <key>ATermHandoffGrantEmpty</key><string>empty1</string>\
         <key>ATermHandoffGrantDesk</key><string>desk1</string></dict></plist>",
    )
    .unwrap();
    let all = HandoffCandidateFacts::read(&app);
    std::fs::write(
        contents.join("Info.plist"),
        "<plist><dict><key>ATermNativeRecipientRecovery</key><string>recipients5</string></dict></plist>",
    ).unwrap();
    let legacy_reader = HandoffCandidateFacts::read(&app);
    std::fs::write(
        contents.join("Info.plist"),
        include_str!("../../../apps/aterm-mac/Info.plist"),
    )
    .unwrap();
    let current = HandoffCandidateFacts::read(&app);
    std::fs::write(contents.join("Info.plist"), "<plist><dict/></plist>").unwrap();
    let replaced = HandoffCandidateFacts::read(&app);
    assert_eq!(
        grants(chunks_only),
        Ok((true, false, false, NativeRecipientRecovery::Absent))
    );
    assert_eq!(
        grants(both),
        Ok((true, true, false, NativeRecipientRecovery::Absent))
    );
    assert_eq!(
        grants(all),
        Ok((true, true, true, NativeRecipientRecovery::Absent))
    );
    assert_eq!(
        grants(legacy_reader),
        Ok((false, false, false, NativeRecipientRecovery::LegacyJournal))
    );
    assert_eq!(
        grants(current),
        Ok((true, true, true, NativeRecipientRecovery::BoundJournal))
    );
    assert_eq!(
        grants(replaced),
        Ok((false, false, false, NativeRecipientRecovery::Absent))
    );
}
