// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Kernel mapping fixture kept outside the engine library's unsafe-free source.
#![cfg(unix)]

use aterm_restore::store::{RegionStore, RegionView, StoreError};

/// A session journaled into a shared mapping recovers from a read-only mapping of its custody descriptor.
#[cfg(unix)]
#[test]
fn a_session_journaled_into_shared_memory_recovers_from_its_custody_descriptor() {
    use aterm_core::terminal::Terminal;
    use aterm_restore::layout::{Geometry, REGION_LEN};
    use aterm_restore::recover::{RecoverPolicy, Recovery, recover};
    use aterm_restore::writer::{JournalWriter, KeyframeInput, WriterIdentity};
    use std::os::fd::AsFd as _;

    const CONFIG: u64 = 0xC0FF;
    // SAFETY: this fixture keeps custody local without reopening or resizing it; all
    // writes use JournalWriter, which drops before the only read-only mapping below.
    let (region, custody) =
        unsafe { aterm_uds::shm::Region::create(REGION_LEN) }.expect("a region");
    let mut writer = JournalWriter::create(
        region,
        Geometry::v1(),
        &WriterIdentity {
            pid: 1,
            birth_us: 2,
            local_id: 3,
            build: 4,
            sid: "s-shm",
        },
    )
    .expect("created");
    let mut term = Terminal::new(6, 30);
    term.process(b"before the keyframe\r\n");
    let checkpoint = term.checkpoint_journal(16).expect("ground");
    writer
        .write_keyframe(&KeyframeInput {
            checkpoint: &checkpoint,
            fence: term.history_fence(),
            resize_lifetime: term.resize_lifetime(),
            resize_ordinal: term.resize_ordinal(),
            bridge_lines: 16,
            config_fp: CONFIG,
            integration_on: term.shell_integration_posture()
                == aterm_core::terminal::ShellIntegrationPosture::On,
        })
        .expect("keyframe");
    let mut gathered = 0u64;
    for slice in [&b"\x1b[1mbold\x1b[0m after\r\n"[..], b"$ \xe2\x9c\x93"] {
        gathered += slice.len() as u64;
        writer.set_gathered(gathered).expect("gathered");
        term.process(slice);
        writer.append_slice(slice).expect("appended");
    }
    drop(writer);
    // SAFETY: drop(writer) above removed this private object's only writable mapping.
    // The retained custody is read-only and stays open until after `dead` is dropped.
    let dead = unsafe { aterm_uds::shm::Region::map_readonly(custody.as_fd(), REGION_LEN) }
        .expect("mapped read-only");
    assert_eq!(
        RegionStore::store_u64_release(&dead, 0, 1),
        Err(StoreError::ReadOnly)
    );
    assert_eq!(
        RegionStore::write(&dead, 0, b"x"),
        Err(StoreError::ReadOnly)
    );
    assert_eq!(
        RegionView::load_u64_acquire(&dead, 4),
        Err(StoreError::Misaligned { at: 4 })
    );
    let policy = RecoverPolicy {
        tail: true,
        config_fp: CONFIG,
        scratch_nonce: [0xA5; 32],
    };
    let Recovery::Restored(restored) = recover(&dead, policy, Terminal::new) else {
        panic!("the region restores a screen");
    };
    assert!(restored.exact, "{:?}", restored.losses);
    assert_eq!(restored.replayed_records, 2);
    assert!(restored.checkpoint.holder.seed_untrusted);
    let visible = |c| {
        Terminal::from_checkpoint(c)
            .checkpoint_journal(0)
            .expect("ground")
            .without_seed_lineage()
    };
    assert_eq!(
        visible(&restored.checkpoint),
        visible(&term.checkpoint_journal(0).expect("ground"))
    );
    assert_eq!(restored.header.sid, "s-shm");
}
