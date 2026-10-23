// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the voucher table, blob decoding, and live macOS process identity.

use super::*;

const LAUNCHD: u32 = 0x2601_5b11;
const ATERM_APP: u32 = 0x2201_1311;
const LINKED_PROBE: u32 = 0x2202_0201;
const UNSIGNED: u32 = 0;

fn blob(text: &str) -> [u8; BLOB_CAP] {
    let mut out = [0u8; BLOB_CAP];
    let whole = u32::try_from(BLOB_HEADER + text.len() + 1).expect("short");
    out[4..8].copy_from_slice(&whole.to_be_bytes());
    out[BLOB_HEADER..BLOB_HEADER + text.len()].copy_from_slice(text.as_bytes());
    out
}

#[test]
fn the_voucher_table_names_every_row() {
    use Voucher::*;
    let rows: [(Option<u32>, Option<u32>, Voucher); 19] = [
        (Some(LAUNCHD), Some(1), Apple),
        (Some(CS_VALID), Some(2), AppStore),
        (Some(CS_VALID), Some(3), Development),
        (Some(CS_VALID), Some(4), AppStore),
        (Some(CS_VALID), Some(5), Enterprise),
        (Some(ATERM_APP), Some(6), DeveloperId),
        (Some(CS_VALID), Some(7), AdHoc),
        (Some(CS_VALID | CS_ADHOC), Some(10), AdHoc),
        (Some(LINKED_PROBE), Some(10), AdHoc),
        (Some(CS_SIGNED | CS_ADHOC | CS_DEBUGGED), Some(10), AdHoc),
        (Some(CS_SIGNED), None, Unknown),
        (Some(UNSIGNED), None, Unsigned),
        (Some(UNSIGNED), Some(10), Unsigned),
        (Some(CS_VALID), Some(10), Unknown),
        (Some(CS_VALID), Some(0), Unknown),
        (Some(CS_VALID), Some(8), Unknown),
        (Some(CS_VALID), Some(11), Unknown),
        (None, Some(1), Unknown),
        (Some(LAUNCHD), None, Unknown),
    ];
    for (status, category, want) in rows {
        assert_eq!(voucher(status, category), want, "{status:x?} {category:?}");
    }
    let words = [
        Apple,
        AppStore,
        DeveloperId,
        Development,
        Enterprise,
        AdHoc,
        Unsigned,
        Unknown,
    ]
    .map(Voucher::word);
    assert_eq!(
        words,
        [
            "apple",
            "app_store",
            "developer_id",
            "development",
            "enterprise",
            "ad_hoc",
            "unsigned",
            "unknown"
        ]
    );
}

#[test]
fn apple_needs_the_platform_flag_and_category_one() {
    assert_eq!(
        voucher(Some(LAUNCHD & !CS_PLATFORM_BINARY), Some(1)),
        Voucher::Unknown
    );
    assert_eq!(voucher(Some(LAUNCHD), Some(10)), Voucher::Unknown);
    let liar = Identity {
        exe: "mds".into(),
        cs_status: Some(CS_VALID | CS_ADHOC),
        category: Some(10),
        signing_id: Some("com.apple.mds".into()),
        team: None,
    };
    assert_eq!(liar.voucher(), Voucher::AdHoc);
}

#[test]
fn a_blob_answer_decodes_to_its_text() {
    assert_eq!(
        blob_text(&blob("com.apple.xpc.launchd")).as_deref(),
        Some("com.apple.xpc.launchd")
    );
    assert_eq!(
        blob_text(&blob("A66A9P66Z7")).as_deref(),
        Some("A66A9P66Z7")
    );
    let mut tail = blob("q1");
    tail[11..].fill(0xA5);
    assert_eq!(blob_text(&tail).as_deref(), Some("q1"));
}

#[test]
fn a_blob_this_layout_does_not_read_decodes_to_none() {
    let mut raw = [0u8; BLOB_CAP];
    raw[..13].copy_from_slice(b"com.apple.mds");
    assert_eq!(blob_text(&raw), None, "no header");
    let mut wrong = blob("com.apple.mds");
    wrong[7] += 1;
    assert_eq!(blob_text(&wrong), None, "a length that is not the text's");
    let mut magic = blob("com.apple.mds");
    magic[0] = 0xFA;
    assert_eq!(blob_text(&magic), None, "a nonzero first word");
    let mut unended = [b'a'; BLOB_CAP];
    unended[..BLOB_HEADER].copy_from_slice(&[0, 0, 0, 0, 0, 0, 1, 1]);
    assert_eq!(blob_text(&unended), None, "no NUL");
    assert_eq!(blob_text(&blob("")), None, "empty");
    assert_eq!(blob_text(&[0, 0, 0, 0, 0, 0, 0]), None, "short");
    let mut latin = blob("caf\u{e9}");
    latin[BLOB_HEADER + 3] = 0xE9;
    latin[BLOB_HEADER + 4] = 0;
    latin[7] -= 1;
    assert_eq!(blob_text(&latin), None, "not UTF-8");
}

#[test]
fn a_team_id_is_read_for_a_developers_signature_and_kept_by_its_shape() {
    let reads = |id: &'static str| move || Some(id.to_owned());
    for category in 3..=6 {
        for id in ["A66A9P66Z7", "Q6L2SF6YDW"] {
            assert_eq!(team_of(Some(category), reads(id)).as_deref(), Some(id));
        }
    }
    for not in ["", "A66A9P66Z", "A66A9P66Z77", "a66a9p66z7", "A66A9P 6Z7"] {
        assert_eq!(team_of(Some(6), reads(not)), None, "{not:?}");
    }
    assert_eq!(team_of(Some(6), || None), None);
}

#[test]
fn no_other_category_reads_a_team_id() {
    for category in [None, Some(0), Some(1), Some(2), Some(7), Some(10)] {
        let read = move || -> Option<String> { panic!("category {category:?} read a team id") };
        assert_eq!(team_of(category, read), None);
    }
}

#[test]
fn pid_zero_has_no_identity() {
    assert_eq!(identity(0), None);
}

#[cfg(target_os = "macos")]
mod live {
    use super::*;

    #[test]
    fn this_process_reads_its_own_image_and_signature() {
        let me = identity(std::process::id()).expect("this process answers");
        let exe = std::env::current_exe().expect("this test binary");
        let base = exe.file_name().expect("a basename").to_string_lossy();
        assert_eq!(me.exe, base);
        let status = me.cs_status.expect("the kernel answers this status");
        assert_eq!(
            me.category.is_some(),
            status & (CS_SIGNED | CS_VALID) != 0,
            "{me:?}"
        );
        assert!(
            matches!(me.voucher(), Voucher::AdHoc | Voucher::Unsigned),
            "{me:?}"
        );
    }

    #[test]
    fn launchd_is_apples_by_its_signature() {
        let launchd = identity(1).expect("launchd answers");
        assert_eq!(launchd.voucher(), Voucher::Apple, "{launchd:?}");
        assert_eq!(launchd.exe, "launchd");
        assert_eq!(launchd.signing_id.as_deref(), Some("com.apple.xpc.launchd"));
        assert_eq!(launchd.team, None, "launchd carries no team id");
    }
}
