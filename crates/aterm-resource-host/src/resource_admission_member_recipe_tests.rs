// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
use super::*;

#[test]
fn metadata_probe_requires_exact_argv_and_bounded_timeout() {
    for word in [b"--print=sysroot".as_slice(), b"--print=target-list"] {
        let words = vec![word.to_vec()];
        assert!(metadata(&words, &GnuTransport::None, Some(Duration::from_secs(8))).unwrap());
        for timeout in [
            None,
            Some(Duration::ZERO),
            Some(Duration::from_nanos(8_000_000_001)),
        ] {
            assert!(metadata(&words, &GnuTransport::None, timeout).is_err());
        }
        assert!(
            metadata(
                &words,
                &GnuTransport::NumericPipe {
                    read_target: 80,
                    write_target: 81
                },
                Some(Duration::from_secs(1)),
            )
            .is_err()
        );
        assert!(metadata(&words, &GnuTransport::None, Some(Duration::from_nanos(1))).unwrap());
        let mut extra = words;
        extra.push(b"--cfg=foreign".to_vec());
        assert!(!metadata(&extra, &GnuTransport::None, Some(Duration::from_nanos(1))).unwrap());
    }
    assert!(
        !metadata(
            &[b"--print=cfg".to_vec()],
            &GnuTransport::None,
            Some(Duration::from_nanos(1))
        )
        .unwrap()
    );
    assert!(metadata(&[b"-vV".to_vec()], &GnuTransport::None, None).unwrap());
}

#[test]
fn native_members_refuse_early_pager_and_opaque_argument_files() {
    for words in [
        vec![b"--explain".to_vec(), b"E0001".to_vec()],
        vec![b"--explain=E0001".to_vec()],
        vec![b"@arguments".to_vec()],
        vec![b"@shell:arguments".to_vec()],
    ] {
        assert!(no_early_execution(&words).is_err());
    }
    for words in [
        vec![b"-vV".to_vec()],
        vec![b"--extern".to_vec(), b"normal=/tmp/@file.rlib".to_vec()],
        vec![b"./--explain.rs".to_vec()],
    ] {
        assert!(no_early_execution(&words).is_ok());
    }
}
