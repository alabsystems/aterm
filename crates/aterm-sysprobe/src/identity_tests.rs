// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The voucher table and the blob decode on recorded answers, and (macOS)
//! the reader held to the kernel on the live machine. No process that runs on
//! every Mac carries a developer's signature, so no live test reaches
//! `CS_OPS_TEAMID`: its number (14) is held only by the probe that read
//! aterm's team id with it (`answers/sign/q1.c` and `q1-out-1.txt`).

use super::*;

/// Status flags the probes read (`answers/sign/q1-out-1.txt` under
/// `~/.cache/aterm-design-evidence/details-2026-10-20`): launchd (pid 1,
/// category 1), the installed aterm (category 6) and the `cc`-built probe
/// itself (linker-signed, category 10).
const LAUNCHD: u32 = 0x2601_5b11;
const ATERM_APP: u32 = 0x2201_1311;
const LINKED_PROBE: u32 = 0x2202_0201;
/// An unsigned process's: a `cc -arch x86_64` binary stripped by `codesign
/// --remove-signature` and run under Rosetta (m22, 2026-10-22). Its status
/// read `0x00000000`, and its `CS_OPS_VALIDATION_CATEGORY`, `CS_OPS_IDENTITY`
/// and `CS_OPS_TEAMID` were refused with `EINVAL`.
const UNSIGNED: u32 = 0;

/// An answer as xnu writes it: zeros, the whole length big-endian, the text
/// and its NUL, in a [`BLOB_CAP`]-byte buffer.
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
        // A signature gone invalid keeps its kind where the kernel still
        // answers the category (a debugged process), and is unknown where it
        // refuses it.
        (Some(CS_SIGNED | CS_ADHOC | CS_DEBUGGED), Some(10), AdHoc),
        (Some(CS_SIGNED), None, Unknown),
        // Unsigned is the status's alone: the kernel refuses the category.
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

/// The negative control: category 1 without the platform flag, and the
/// platform flag under another category, are not Apple's; nor is the liar,
/// a fresh binary signed ad hoc as `com.apple.mds` (category 10, RD §2.3
/// rule 3), whatever identifier it carries.
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
    // The kernel copies only the header and the text: the rest is the
    // buffer's.
    let mut tail = blob("q1");
    tail[11..].fill(0xA5);
    assert_eq!(blob_text(&tail).as_deref(), Some("q1"));
}

/// The negative control: a header that does not hold the answer's length
/// (text written from byte 0, a moved length), a nonzero first word, no NUL,
/// an empty text, a buffer shorter than the header, or text that is not
/// UTF-8 decode to nothing.
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
    // The length a NUL after the last byte would give (257), so only the
    // missing NUL refuses it.
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

/// The negative control: another category's team id is neither read nor
/// kept, whatever the kernel would answer.
#[test]
fn no_other_category_reads_a_team_id() {
    for category in [None, Some(0), Some(1), Some(2), Some(7), Some(10)] {
        let read = move || -> Option<String> { panic!("category {category:?} read a team id") };
        assert_eq!(team_of(category, read), None);
    }
}

/// Pid 0 is never read: `csops` would answer for the caller (RD §2.3 rule
/// 4). This holds the answer, not which guard gives it (`process_path`
/// refuses pid 0 too).
#[test]
fn pid_zero_has_no_identity() {
    assert_eq!(identity(0), None);
}

#[cfg(target_os = "macos")]
mod live {
    use super::*;

    /// A test binary is the linker's: signed ad hoc by it on arm64, and
    /// unsigned on x86_64, where it signs nothing (and the kernel refuses
    /// the category).
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

    /// Launchd (root) is read, not run: its voucher, identifier and image.
    #[test]
    fn launchd_is_apples_by_its_signature() {
        let launchd = identity(1).expect("launchd answers");
        assert_eq!(launchd.voucher(), Voucher::Apple, "{launchd:?}");
        assert_eq!(launchd.exe, "launchd");
        assert_eq!(launchd.signing_id.as_deref(), Some("com.apple.xpc.launchd"));
        assert_eq!(launchd.team, None, "launchd carries no team id");
    }
}
