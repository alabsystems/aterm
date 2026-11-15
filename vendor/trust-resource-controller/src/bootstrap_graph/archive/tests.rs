use super::*;

fn file(path: &[u8], inode: u64) -> MetadataFile {
    MetadataFile {
        path: path.to_vec(),
        sha256: [7; 32],
        bytes: 128,
        device: 1,
        inode,
    }
}
pub(super) fn request() -> Request {
    Request {
        key: OperationKey {
            graph: super::super::tests::key(),
            sequence: 1,
            operation: [4; 16],
        },
        gnu: GnuIdentity::NumericPipe {
            read: FileIdentity {
                device: 1,
                inode: 1,
            },
            write: FileIdentity {
                device: 1,
                inode: 2,
            },
        },
        sdk_root: b"/sdk".to_vec(),
        driver: file(b"/sdk/bin/targo", 3),
        host: "fixture-machine".into(),
        boot: "fixture-boot".into(),
        source_root: b"/source".to_vec(),
        source_directory: FileIdentity {
            device: 1,
            inode: 4,
        },
        declarations: [
            file(b"/source/src/stage0", 5),
            file(b"/source/bootstrap/trust-stage0/seed-ledger.toml", 6),
            file(
                b"/source/bootstrap/trust-stage0/dist/2026-11-07/channel-rust-trust.toml",
                7,
            ),
        ],
        date: "2026-11-07".into(),
        version: "0.13.0-trust".into(),
        archives: (0..ARCHIVE_COUNT)
            .map(|index| ArchivePin {
                relative: relative_archive("2026-11-07", "0.13.0-trust", index).unwrap(),
                sha256: [index as u8; 32],
            })
            .collect(),
        contents_directory: FileIdentity {
            device: 1,
            inode: 8,
        },
        timeout_ns: MAX_WORK_NS,
    }
}

#[test]
fn closed_archive_request_has_one_distinct_native_argv_and_roundtrips() {
    let request = request();
    let bytes = request.encode().unwrap();
    assert!(Request::decode(&bytes).unwrap() == request);
    assert_eq!(request.arguments().unwrap(), [START_ARGUMENT]);
    assert!(
        request
            .hello()
            .unwrap()
            .windows(PURPOSE.len())
            .any(|value| value == PURPOSE.as_bytes())
    );
    assert!(super::super::object::Request::decode(&bytes).is_err());
    assert!(super::super::query::Request::decode(&bytes).is_err());
    assert_eq!(&PROTOCOL_MARKER[..16], b"TRUSTBARCH000002");
}

#[test]
fn source_and_original_contents_expectations_do_not_alias_or_select_other_paths() {
    for case in 0..9 {
        let mut request = request();
        match case {
            0 => request.contents_directory = request.source_directory,
            1 => request.contents_directory.inode = 0,
            2 => request.source_root = b"/source/../foreign".to_vec(),
            3 => request.driver.path = b"/sdk/bin/trustc".to_vec(),
            4 => request.declarations[0].path = b"/source/other".to_vec(),
            5 => request.declarations[1].path = b"/foreign/seed-ledger.toml".to_vec(),
            6 => {
                request.declarations[2].path =
                    b"/source/bootstrap/trust-stage0/dist/2026-11-08/channel-rust-trust.toml"
                        .to_vec()
            }
            7 => request.declarations[0].bytes = MAX_DECLARATION_BYTES + 1,
            8 => request.declarations[0].bytes = 0,
            _ => unreachable!(),
        }
        assert!(request.encode().is_err(), "case {case}");
    }
}

#[test]
fn archive_population_is_exact_ordered_and_native_with_no_arbitrary_component() {
    let original = request();
    let leaves = (0..ARCHIVE_COUNT)
        .map(|index| original.output_basename(index).unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(leaves.len(), ARCHIVE_COUNT);
    assert!(
        leaves
            .iter()
            .all(|leaf| !leaf.contains(&b'/') && leaf.ends_with(b".tar.xz"))
    );
    assert!(original.output_basename(ARCHIVE_COUNT).is_err());
    for case in 0..7 {
        let mut request = request();
        match case {
            0 => {
                request.archives.pop();
            }
            1 => request.archives.push(request.archives[0].clone()),
            2 => request.archives.swap(0, 1),
            3 => request.archives[0].relative = b"../../outside.tar.xz".to_vec(),
            4 => {
                request.archives[0].relative =
                    b"dist/2026-11-07/targo-0.13.0-trust-aarch64-unknown-linux-gnu.tar.xz".to_vec()
            }
            5 => request.date = "../escape".into(),
            6 => request.version = "../escape".into(),
            _ => unreachable!(),
        }
        assert!(request.encode().is_err(), "case {case}");
    }
}

#[test]
fn original_nonce_and_finite_work_bounds_are_mandatory() {
    for timeout in [0, MAX_WORK_NS + 1, u64::MAX] {
        let mut request = request();
        request.timeout_ns = timeout;
        assert!(request.encode().is_err());
    }
    for nonce in [[0; 16], [1; 16], [2; 16], [3; 16]] {
        let mut request = request();
        request.key.operation = nonce;
        assert!(request.encode().is_err());
    }
}

#[test]
fn every_request_byte_is_bound_and_partial_or_trailing_frames_refuse() {
    let original = request();
    let bytes = original.encode().unwrap();
    for cut in 0..bytes.len() {
        assert!(Request::decode(&bytes[..cut]).is_err());
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(Request::decode(&trailing).is_err());
    for case in 0..6 {
        let mut changed = original.clone();
        match case {
            0 => changed.archives[0].sha256[0] ^= 1,
            1 => changed.declarations[0].sha256[0] ^= 1,
            2 => changed.driver.sha256[0] ^= 1,
            3 => changed.contents_directory.inode += 1,
            4 => changed.timeout_ns -= 1,
            5 => changed.key.sequence += 1,
            _ => unreachable!(),
        }
        assert_ne!(
            changed.binding_preimage().unwrap(),
            original.binding_preimage().unwrap()
        );
    }
}

#[test]
fn prepared_data_must_match_distinct_archive_program_policy_and_operation() {
    let request = request();
    let prepared = MetadataPrepared {
        root_attempt: request.key.graph.query.binding.root_attempt,
        member_attempt: request.key.operation,
        root: request.sdk_root.clone(),
        policy: PREPARED_POLICY.into(),
        scope: PREPARED_SCOPE.into(),
        executable: request.driver.clone(),
        runtime: vec![],
        loader_paths: vec![],
        system_dependencies: vec![],
        // Actual host/boot identity is checked against the retained context by
        // the native owner; it is not a Rust target triple.
        host: "fixture-machine".into(),
        boot: "fixture-boot".into(),
    };
    request.validate_prepared(&prepared).unwrap();
    for case in 0..6 {
        let mut changed = prepared.clone();
        match case {
            0 => changed.policy = super::super::object::PREPARED_POLICY.into(),
            1 => changed.scope = "arbitrary-process".into(),
            2 => changed.executable.sha256[0] ^= 1,
            3 => changed.root = b"/foreign".to_vec(),
            4 => changed.member_attempt = [9; 16],
            5 => changed.root_attempt = [9; 16],
            _ => unreachable!(),
        }
        assert!(request.validate_prepared(&changed).is_err(), "case {case}");
    }
}
