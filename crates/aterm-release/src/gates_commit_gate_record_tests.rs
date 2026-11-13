// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// NEGATIVE CONTROLS: no component of the record may cross a judgment.
#[test]
fn fixture_and_policy_records_bind_every_input() {
    struct At(PathBuf);
    impl GitRunner for At {
        fn git(&self, args: &[&str]) -> Result<crate::ledger::RunOut> {
            assert_eq!(
                args,
                ["rev-parse", "--path-format=absolute", "--git-common-dir"]
            );
            Ok(crate::ledger::RunOut {
                status: 0,
                stdout: format!("{}\n", self.0.display()).into_bytes(),
                stderr: Vec::new(),
            })
        }
    }
    let dir = aterm_tempfile::tempdir().unwrap();
    let git = At(dir.path().to_path_buf());
    let expected = CommitGateRecord::new("source", "0.183.0", "seal".into(), "tags".into());
    assert!(expected.adopt(&git).is_err(), "missing is not passed");
    expected.save(&git).unwrap();
    expected.adopt(&git).unwrap();
    for changed in [
        CommitGateRecord::new("other", "0.183.0", "seal".into(), "tags".into()),
        CommitGateRecord::new("source", "0.184.0", "seal".into(), "tags".into()),
        CommitGateRecord::new("source", "0.183.0", "other".into(), "tags".into()),
        CommitGateRecord::new("source", "0.183.0", "seal".into(), "other".into()),
    ] {
        assert!(changed.adopt(&git).is_err(), "{changed:?}");
    }
    std::fs::write(CommitGateRecord::path(&git).unwrap(), "garbled").unwrap();
    assert!(expected.adopt(&git).is_err(), "garbled is not passed");
}
