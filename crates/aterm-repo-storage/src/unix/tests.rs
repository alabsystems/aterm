// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::fs;
use std::io::Write as _;
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::time::Duration;

const NAME: &str = "pack-0123456789abcdef0123456789abcdef01234567.pack";
const OTHER_NAME: &str = "pack-0000000000000000000000000000000000000000.pack";

struct Fixture {
    _temp: aterm_tempfile::TempDir,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = aterm_tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap().join("common");
        fs::create_dir_all(root.join("objects/pack")).unwrap();
        fs::create_dir(root.join("refs")).unwrap();
        fs::write(root.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        Self { _temp: temp, root }
    }
    fn pack(&self, store: &str, contents: &[u8]) -> PathBuf {
        self.named_pack(store, NAME, contents)
    }
    fn named_pack(&self, store: &str, name: &str, contents: &[u8]) -> PathBuf {
        let repo = self.root.join(store);
        fs::create_dir_all(repo.join("objects/pack")).unwrap();
        fs::write(repo.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        let path = repo.join("objects/pack").join(name);
        fs::write(&path, contents).unwrap();
        path
    }
    fn pair(&self, contents: &[u8]) -> (PathBuf, PathBuf) {
        (self.pack("", contents), self.pack("modules/dep", contents))
    }
}

fn verifying() -> Limits {
    Limits {
        verify_io_bytes: 1024 * 1024,
        ..Limits::default()
    }
}

#[test]
fn metadata_only_never_reads_content_and_counts_distinct_inodes() {
    let fixture = Fixture::new();
    fixture.pair(b"same bytes");
    let mut reads = 0;
    let report = plan_with_hook(&fixture.root, &Limits::default(), &mut |p| {
        if matches!(p, Point::ReadChunk) {
            reads += 1;
        }
    })
    .unwrap();
    assert!(report.observation_complete, "{:?}", report.issues);
    assert_eq!(reads, 0);
    assert_eq!(report.verification_io_bytes, 0);
    assert_eq!(report.unique_pack_inodes, 2);
    assert_eq!(report.unique_logical_bytes, 20);
    assert_eq!(report.candidate_duplicate_logical_bytes, 10);
    assert_eq!(report.pairs.len(), 1);
    assert_eq!(report.pairs[0].state, PairState::Candidate);
    assert_eq!(report.physical_savings_bytes, None);
    assert!(!report.mutation_available);
}

#[test]
fn exact_paired_comparison_has_digest_and_correct_both_stream_accounting() {
    let fixture = Fixture::new();
    fixture.pair(b"abc");
    let mut limits = verifying();
    limits.verify_io_bytes = 8; // 6 data + 2 reserved EOF bytes
    let report = plan(&fixture.root, &limits).unwrap();
    assert!(report.observation_complete, "{:?}", report.issues);
    assert_eq!(report.pairs[0].state, PairState::ByteEqual);
    assert_eq!(
        report.pairs[0].sha256.as_deref(),
        Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
    );
    assert_eq!(report.verification_io_bytes, 6);
    assert_eq!(report.byte_equal_duplicate_logical_bytes, 3);
}

#[test]
fn same_filename_and_length_never_substitutes_for_byte_equality() {
    let fixture = Fixture::new();
    fixture.pack("", b"abc");
    fixture.pack("modules/dep", b"abd");
    let report = plan(&fixture.root, &verifying()).unwrap();
    assert_eq!(report.pairs[0].state, PairState::ByteMismatch);
    assert_eq!(report.byte_equal_duplicate_logical_bytes, 0);
    assert!(report.pairs[0].sha256.is_none());
    assert_eq!(report.verification_io_bytes, 6);
}

#[test]
fn insufficient_budget_defers_before_read_and_zero_keeps_empty_pack_unverified() {
    let fixture = Fixture::new();
    fixture.pair(b"abc");
    let mut limits = verifying();
    limits.verify_io_bytes = 7;
    let mut read = false;
    let report = plan_with_hook(&fixture.root, &limits, &mut |p| {
        if matches!(p, Point::ReadChunk) {
            read = true;
        }
    })
    .unwrap();
    assert!(!read);
    assert_eq!(report.verification_io_bytes, 0);
    assert_eq!(report.pairs[0].state, PairState::Deferred);
    let empty = Fixture::new();
    empty.pair(b"");
    assert_eq!(
        plan(&empty.root, &Limits::default()).unwrap().pairs[0].state,
        PairState::Candidate
    );
}

#[test]
fn hardlink_aliases_are_already_shared_not_counted_again() {
    let fixture = Fixture::new();
    let a = fixture.pack("", b"abc");
    let b = fixture.pack("modules/dep", b"abc");
    fs::remove_file(&b).unwrap();
    fs::hard_link(&a, &b).unwrap();
    let report = plan(&fixture.root, &verifying()).unwrap();
    assert_eq!(report.packs.len(), 2);
    assert_eq!(report.unique_pack_inodes, 1);
    assert_eq!(report.hardlink_alias_paths, 1);
    assert_eq!(report.unique_logical_bytes, 3);
    assert_eq!(report.candidate_duplicate_logical_bytes, 0);
    assert!(report.pairs.is_empty());
}

#[test]
fn differently_named_alias_does_not_hide_an_existing_candidate() {
    let fixture = Fixture::new();
    let (source, _) = fixture.pair(b"abc");
    fs::hard_link(&source, source.with_file_name(OTHER_NAME)).unwrap();
    for limits in [Limits::default(), verifying()] {
        let report = plan(&fixture.root, &limits).unwrap();
        assert!(report.observation_complete, "{:?}", report.issues);
        assert_eq!(report.unique_pack_inodes, 2);
        assert_eq!(report.hardlink_alias_paths, 1);
        assert_eq!(report.unique_logical_bytes, 6);
        assert_eq!(report.candidate_duplicate_logical_bytes, 3);
        assert_eq!(report.pairs.len(), 1);
        assert!(report.pairs[0].source.ends_with(NAME));
        let expected_equal = if limits.verify_io_bytes == 0 { 0 } else { 3 };
        assert_eq!(report.byte_equal_duplicate_logical_bytes, expected_equal);
        assert_eq!(report.verification_io_bytes, expected_equal * 2);
    }
}

#[test]
fn overlapping_alias_groups_keep_pair_evidence_without_double_counting_bytes() {
    let fixture = Fixture::new();
    let (a, b) = fixture.pair(b"abc");
    for path in [&a, &b] {
        fs::hard_link(path, path.with_file_name(OTHER_NAME)).unwrap();
    }
    let report = plan(&fixture.root, &verifying()).unwrap();
    assert!(report.observation_complete, "{:?}", report.issues);
    assert_eq!(report.unique_pack_inodes, 2);
    assert_eq!(report.hardlink_alias_paths, 2);
    assert_eq!(report.unique_logical_bytes, 6);
    assert_eq!(report.pairs.len(), 2);
    assert!(report.pairs.iter().all(|p| p.state == PairState::ByteEqual));
    assert_eq!(report.verification_io_bytes, 12);
    assert_eq!(report.candidate_duplicate_logical_bytes, 3);
    assert_eq!(report.byte_equal_duplicate_logical_bytes, 3);
}

#[test]
fn alias_triangle_preserves_each_path_witness_but_counts_only_two_duplicate_inodes() {
    const THIRD_NAME: &str = "pack-ffffffffffffffffffffffffffffffffffffffff.pack";
    for contents in [b"abc".as_slice(), b"".as_slice()] {
        let fixture = Fixture::new();
        let a = fixture.named_pack("", OTHER_NAME, contents);
        fs::hard_link(&a, a.with_file_name(THIRD_NAME)).unwrap();
        let b = fixture.named_pack("modules/one", OTHER_NAME, contents);
        fs::hard_link(&b, b.with_file_name(NAME)).unwrap();
        let c = fixture.pack("modules/two", contents);
        fs::hard_link(&c, c.with_file_name(THIRD_NAME)).unwrap();
        let report = plan(&fixture.root, &verifying()).unwrap();
        assert!(report.observation_complete, "{:?}", report.issues);
        assert_eq!(report.unique_pack_inodes, 3);
        assert_eq!(report.hardlink_alias_paths, 3);
        assert_eq!(report.unique_logical_bytes, contents.len() as u64 * 3);
        assert_eq!(report.pairs.len(), 3);
        assert!(report.pairs.iter().all(|p| p.state == PairState::ByteEqual));
        assert_eq!(report.verification_io_bytes, contents.len() as u64 * 6);
        assert_eq!(
            report.candidate_duplicate_logical_bytes,
            contents.len() as u64 * 2
        );
        assert_eq!(
            report.byte_equal_duplicate_logical_bytes,
            contents.len() as u64 * 2
        );
    }
}

fn shared_target_alias_fixture(other: &[u8]) -> Fixture {
    let fixture = Fixture::new();
    fixture.named_pack("", OTHER_NAME, b"abc");
    fixture.pack("modules/one", other);
    let target = fixture.pack("modules/two", b"abc");
    fs::hard_link(&target, target.with_file_name(OTHER_NAME)).unwrap();
    fixture
}

#[test]
fn shared_target_alias_counts_connected_inodes_and_only_successful_equalities() {
    for (other, equal_bytes, equal_pairs) in [(b"abc".as_slice(), 6, 2), (b"abd".as_slice(), 3, 1)]
    {
        let fixture = shared_target_alias_fixture(other);
        let report = plan(&fixture.root, &verifying()).unwrap();
        assert!(report.observation_complete, "{:?}", report.issues);
        assert_eq!(report.unique_pack_inodes, 3);
        assert_eq!(report.hardlink_alias_paths, 1);
        assert_eq!(report.unique_logical_bytes, 9);
        assert_eq!(report.pairs.len(), 2);
        assert_eq!(report.candidate_duplicate_logical_bytes, 6);
        assert_eq!(report.byte_equal_duplicate_logical_bytes, equal_bytes);
        assert_eq!(
            report
                .pairs
                .iter()
                .filter(|p| p.state == PairState::ByteEqual)
                .count(),
            equal_pairs
        );
        assert_eq!(report.verification_io_bytes, 12);
    }
}

#[test]
fn alias_graph_budget_and_final_refusal_never_promote_unverified_edges() {
    let fixture = shared_target_alias_fixture(b"abc");
    let limits = Limits {
        verify_io_bytes: 8,
        ..Limits::default()
    };
    let report = plan(&fixture.root, &limits).unwrap();
    assert!(report.observation_complete, "{:?}", report.issues);
    assert_eq!(report.candidate_duplicate_logical_bytes, 6);
    assert_eq!(report.byte_equal_duplicate_logical_bytes, 3);
    assert_eq!(report.verification_io_bytes, 6);
    assert_eq!(report.pairs[0].state, PairState::ByteEqual);
    assert_eq!(report.pairs[1].state, PairState::Deferred);

    let report = plan_with_hook(&fixture.root, &verifying(), &mut |point| {
        if matches!(point, Point::BeforeFinal) {
            fs::write(fixture.root.join("objects/pack").join(OTHER_NAME), b"abd").unwrap();
        }
    })
    .unwrap();
    assert!(!report.observation_complete);
    assert_eq!(report.byte_equal_duplicate_logical_bytes, 0);
    assert!(
        report
            .pairs
            .iter()
            .all(|p| p.state == PairState::Refused && p.sha256.is_none())
    );
}

#[test]
fn scope_includes_nested_and_linked_module_stores_but_never_checkout_files() {
    let fixture = Fixture::new();
    fixture.pack("", b"abc");
    fixture.pack("modules/group/dep", b"abc");
    fixture.pack("modules/group/dep/modules/inner", b"abc");
    fixture.pack("worktrees/private/modules/other", b"abc");
    fixture.pack("checkout/ignored", b"abc");
    let report = plan(&fixture.root, &Limits::default()).unwrap();
    assert!(report.observation_complete, "{:?}", report.issues);
    assert_eq!(report.unique_pack_inodes, 4);
    assert_eq!(report.candidate_duplicate_logical_bytes, 9);
    assert!(
        report
            .packs
            .iter()
            .all(|p| !p.relative_path.starts_with("checkout/"))
    );
}

#[test]
fn root_and_pack_symlinks_refuse_without_following() {
    let fixture = Fixture::new();
    let (a, b) = fixture.pair(b"abc");
    fs::remove_file(&b).unwrap();
    symlink(&a, &b).unwrap();
    let report = plan(&fixture.root, &verifying()).unwrap();
    assert!(!report.observation_complete);
    assert!(report.pairs.is_empty());
    let alias = fixture.root.with_file_name("alias");
    symlink(&fixture.root, &alias).unwrap();
    assert!(plan(&alias, &Limits::default()).is_err());
    assert!(plan(Path::new("relative/.git"), &Limits::default()).is_err());
    let ancestor_alias = fixture.root.with_file_name("ancestor");
    symlink(fixture.root.parent().unwrap(), &ancestor_alias).unwrap();
    assert!(plan(&ancestor_alias.join("common"), &Limits::default()).is_err());
}

#[test]
fn fifo_pack_is_rejected_without_waiting_for_a_writer() {
    let fixture = Fixture::new();
    let path = fixture.root.join("objects/pack").join(NAME);
    use std::os::unix::ffi::OsStrExt as _;
    let path_c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: the live C string names only the owned test fixture; no descriptor is borrowed.
    assert_eq!(unsafe { libc::mkfifo(path_c.as_ptr(), 0o600) }, 0);
    let report = plan(&fixture.root, &verifying()).unwrap();
    assert!(!report.observation_complete);
    assert_eq!(report.verification_io_bytes, 0);
}

#[test]
fn depth_admission_precedes_opening_an_overdepth_pack_directory() {
    let fixture = Fixture::new();
    let pack = fixture.root.join("objects/pack");
    fs::remove_dir(&pack).unwrap();
    let outside = fixture.root.with_file_name("outside-pack");
    fs::create_dir(&outside).unwrap();
    symlink(&outside, &pack).unwrap();
    let report = plan(
        &fixture.root,
        &Limits {
            max_depth: 1,
            ..Limits::default()
        },
    )
    .unwrap();
    assert!(!report.observation_complete);
    assert_eq!(
        report.issues.first().map(String::as_str),
        Some("directory depth limit reached")
    );
    assert!(report.packs.is_empty());
    assert_eq!(report.verification_io_bytes, 0);
}

#[test]
fn pack_directory_at_the_permitted_depth_is_still_observed() {
    let fixture = Fixture::new();
    fixture.pack("", b"abc");
    let report = plan(
        &fixture.root,
        &Limits {
            max_depth: 2,
            ..Limits::default()
        },
    )
    .unwrap();
    assert!(report.observation_complete, "{:?}", report.issues);
    assert_eq!(report.packs.len(), 1);
    assert_eq!(
        report.packs[0].relative_path,
        format!("objects/pack/{NAME}")
    );
    assert_eq!(report.verification_io_bytes, 0);
}

#[test]
fn sparse_pack_size_entry_depth_pack_count_and_elapsed_limits_are_not_empty_success() {
    let fixture = Fixture::new();
    let (a, _) = fixture.pair(b"abc");
    File::options()
        .write(true)
        .open(&a)
        .unwrap()
        .set_len(1024 * 1024)
        .unwrap();
    for limits in [
        Limits {
            max_pack_bytes: 3,
            ..Limits::default()
        },
        Limits {
            max_entries: 0,
            ..Limits::default()
        },
        Limits {
            max_depth: 1,
            ..Limits::default()
        },
        Limits {
            max_packs: 0,
            ..Limits::default()
        },
    ] {
        let report = plan(&fixture.root, &limits).unwrap();
        assert!(!report.observation_complete, "{limits:?}");
        assert!(!report.issues.is_empty());
    }
}

#[test]
fn root_admission_is_bounded_before_any_component_is_opened() {
    let too_deep = PathBuf::from(format!("/{}", vec!["nonexistent"; 129].join("/")));
    assert!(
        plan(&too_deep, &Limits::default())
            .unwrap_err()
            .to_string()
            .contains("root path/component limit")
    );
    let too_long = PathBuf::from(format!("/{}", "x".repeat(4096)));
    assert!(
        plan(&too_long, &Limits::default())
            .unwrap_err()
            .to_string()
            .contains("root path/component limit")
    );
    let fixture = Fixture::new();
    assert!(
        plan(
            &fixture.root,
            &Limits {
                max_elapsed: Duration::ZERO,
                ..Limits::default()
            }
        )
        .unwrap_err()
        .to_string()
        .contains("elapsed-work limit")
    );
}

#[test]
fn positive_budget_verifies_empty_files_without_reading_nonexistent_data() {
    let fixture = Fixture::new();
    fixture.pair(b"");
    let report = plan(
        &fixture.root,
        &Limits {
            verify_io_bytes: 2,
            ..Limits::default()
        },
    )
    .unwrap();
    assert!(report.observation_complete, "{:?}", report.issues);
    assert_eq!(report.pairs[0].state, PairState::ByteEqual);
    assert_eq!(
        report.pairs[0].sha256.as_deref(),
        Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
    );
    assert_eq!(report.verification_io_bytes, 0);
}

#[test]
fn advertised_root_replacement_before_final_check_refuses_complete() {
    let fixture = Fixture::new();
    fixture.pair(b"abc");
    let moved = fixture.root.with_file_name("old-common");
    let report = plan_with_hook(&fixture.root, &verifying(), &mut |point| {
        if matches!(point, Point::BeforeFinal) {
            fs::rename(&fixture.root, &moved).unwrap();
            fs::create_dir(&fixture.root).unwrap();
        }
    })
    .unwrap();
    assert!(!report.observation_complete);
    assert_eq!(report.pairs[0].state, PairState::Refused);
    assert_eq!(report.byte_equal_duplicate_logical_bytes, 0);
}

#[test]
fn midread_rewrite_and_shrink_cannot_keep_an_equal_verdict() {
    for shrink in [false, true] {
        let fixture = Fixture::new();
        let data = vec![b'a'; 65539];
        let (_, b) = fixture.pair(&data);
        let mut changed = false;
        let report = plan_with_hook(&fixture.root, &verifying(), &mut |point| {
            if matches!(point, Point::ReadChunk) && !changed {
                if shrink {
                    File::options()
                        .write(true)
                        .open(&b)
                        .unwrap()
                        .set_len(0)
                        .unwrap();
                } else {
                    fs::write(&b, vec![b'b'; data.len()]).unwrap();
                }
                changed = true;
            }
        })
        .unwrap();
        assert!(!report.observation_complete);
        assert_eq!(report.pairs[0].state, PairState::Refused);
        assert_eq!(report.byte_equal_duplicate_logical_bytes, 0);
        assert_eq!(
            report.verification_io_bytes,
            if shrink { 131075 } else { 131078 }
        );
    }
}

#[test]
fn path_replacement_after_equal_read_revokes_final_equality() {
    let fixture = Fixture::new();
    let (_, b) = fixture.pair(b"abc");
    let replacement = fixture.root.join("replacement");
    fs::write(&replacement, b"abc").unwrap();
    let report = plan_with_hook(&fixture.root, &verifying(), &mut |point| {
        if matches!(point, Point::BeforeFinal) {
            fs::rename(&replacement, &b).unwrap();
        }
    })
    .unwrap();
    assert!(!report.observation_complete);
    assert_eq!(report.pairs[0].state, PairState::Refused);
    assert!(report.pairs[0].sha256.is_none());
    assert_eq!(report.byte_equal_duplicate_logical_bytes, 0);
}

#[test]
fn midread_growth_refuses_and_counts_actual_lookahead_bytes() {
    let fixture = Fixture::new();
    let (_, b) = fixture.pair(b"abc");
    let mut changed = false;
    let report = plan_with_hook(&fixture.root, &verifying(), &mut |point| {
        if matches!(point, Point::ReadChunk) && !changed {
            File::options()
                .append(true)
                .open(&b)
                .unwrap()
                .write_all(b"x")
                .unwrap();
            changed = true;
        }
    })
    .unwrap();
    assert!(!report.observation_complete);
    assert_eq!(report.pairs[0].state, PairState::Refused);
    assert_eq!(report.verification_io_bytes, 7);
}

#[test]
fn final_membership_addition_and_removal_cannot_report_complete() {
    for remove in [false, true] {
        let fixture = Fixture::new();
        let (_, b) = fixture.pair(b"abc");
        let report = plan_with_hook(&fixture.root, &Limits::default(), &mut |point| {
            if matches!(point, Point::BeforeFinal) {
                if remove {
                    fs::remove_file(&b).unwrap();
                } else {
                    fs::write(b.with_file_name("new.idx"), b"index").unwrap();
                }
            }
        })
        .unwrap();
        assert!(!report.observation_complete);
        assert!(!report.issues.is_empty());
    }
}

#[test]
fn strict_pack_names_accept_both_git_hash_widths_and_refuse_extra_suffixes() {
    assert!(pack_name(NAME));
    assert!(pack_name(&format!("pack-{}.pack", "a".repeat(64))));
    for bad in [
        "pack-a.pack",
        "pack-0000000000000000000000000000000000000000.pack.tmp",
        "pack-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA.pack",
    ] {
        assert!(!pack_name(bad));
    }
    let fixture = Fixture::new();
    fs::write(fixture.root.join("objects/pack/pack-invalid.pack"), b"a").unwrap();
    assert!(
        !plan(&fixture.root, &Limits::default())
            .unwrap()
            .observation_complete
    );
}
