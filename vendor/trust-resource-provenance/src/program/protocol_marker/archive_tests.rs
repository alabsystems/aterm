//! Marker parsing DATA controls, not native SDK/loaded-image qualification.
use std::io::Write as _;

use super::tests::{expected, image};
use super::*;

#[test]
fn local_archive_profile_is_distinct_and_every_original_marker_byte_is_required() {
    let deadline = Instant::now() + std::time::Duration::from_secs(60);
    assert_eq!(Profile::BootstrapArchive.section(), b"__trust_barch\0\0\0");
    assert_eq!(Profile::BootstrapArchive.magic(), b"TRUSTBARCH000002");
    assert_eq!(
        Profile::BootstrapArchive.fields(),
        [2, 118, 119, 120, 121, 122]
    );
    for profile in [
        Profile::Resolver,
        Profile::Workload,
        Profile::WorkloadMember,
        Profile::PairedQualification,
        Profile::NativeJob,
        Profile::BootstrapFixed,
        Profile::BootstrapGraph,
        Profile::BootstrapArchive,
    ] {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(&image(profile)).unwrap();
        assert_eq!(
            require(
                &file,
                Profile::BootstrapArchive,
                &expected(Profile::BootstrapArchive),
                deadline
            )
            .is_ok(),
            matches!(profile, Profile::BootstrapArchive)
        );
    }
    for index in 0..64 {
        let mut bytes = image(Profile::BootstrapArchive);
        bytes[256 + index] ^= 1;
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(&bytes).unwrap();
        assert!(
            require(
                &file,
                Profile::BootstrapArchive,
                &expected(Profile::BootstrapArchive),
                deadline
            )
            .is_err(),
            "byte {index}"
        );
    }
}
