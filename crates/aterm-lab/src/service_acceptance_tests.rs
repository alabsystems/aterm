// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_secrets::service_repair::Request;

#[derive(Default)]
struct Qualified {
    installs: usize,
    proofs: usize,
    fail_install: bool,
    fail_proof: bool,
}
impl Backend for Qualified {
    fn authorize(&mut self, _: &Request) -> Result<()> {
        unreachable!("routing test never authorizes")
    }
    fn observe(&mut self, _: Job) -> Result<Observed> {
        unreachable!("routing test never observes")
    }
    fn verify_candidate(&mut self, _: &Request) -> Result<()> {
        unreachable!("routing test never verifies")
    }
    fn install(&mut self, job: Job, _: &Unit) -> Result<()> {
        assert_eq!(job, Job::Server);
        self.installs += 1;
        if self.fail_install {
            Err(error("qualified install refused"))
        } else {
            Ok(())
        }
    }
    fn prove(&mut self, job: Job, _: &Unit, _: &HealthRequirement) -> Result<()> {
        assert_eq!(job, Job::Server);
        self.proofs += 1;
        if self.fail_proof {
            Err(error("qualified proof refused"))
        } else {
            Ok(())
        }
    }
}
fn unit() -> Unit {
    Unit {
        program: "/qualified/baseline.app/Contents/MacOS/aterm".into(),
        args: vec!["secrets".into(), "serve".into()],
        definition: "unused by routing control".into(),
        image: "a".repeat(64),
        executable_sha256: "b".repeat(64),
    }
}
fn expected_health() -> HealthRequirement {
    HealthRequirement {
        vault: "team".into(),
        signer: "c".repeat(64),
        records: vec!["d".repeat(64)],
        frontier: 17,
    }
}
fn routed(qualified: Option<Qualified>) -> Live<Qualified> {
    Live {
        paths: Paths::for_home(Path::new("/__never_read_legacy_service__")),
        ctl: "/never-run".into(),
        baseline: Some(PathBuf::from(unit().program)),
        qualified,
    }
}

#[test]
fn qualified_baseline_install_and_proof_delegate_without_legacy_fallback() {
    let mut success = routed(Some(Qualified::default()));
    assert!(success.install_qualified(&unit()).unwrap());
    success.prove(&unit(), &expected_health()).unwrap();
    assert_eq!(success.qualified.as_ref().unwrap().installs, 1);
    assert_eq!(success.qualified.as_ref().unwrap().proofs, 1);
    let mut refused = routed(Some(Qualified {
        fail_install: true,
        fail_proof: true,
        ..Qualified::default()
    }));
    assert_eq!(
        refused.install_qualified(&unit()).unwrap_err().to_string(),
        "qualified install refused"
    );
    // The full Environment::prove path returns the delegated error before any legacy
    // configuration, process or endpoint access. Its paths are deliberately unusable.
    assert_eq!(
        refused
            .prove(&unit(), &expected_health())
            .unwrap_err()
            .to_string(),
        "qualified proof refused"
    );
    assert_eq!(refused.qualified.as_ref().unwrap().proofs, 1);
}

#[test]
fn qualified_baseline_missing_backend_refuses_and_legacy_is_not_routed() {
    let mut absent = routed(None);
    assert!(absent.install_qualified(&unit()).is_err());
    assert!(absent.prove(&unit(), &expected_health()).is_err());
    let mut live = routed(Some(Qualified::default()));
    let mut legacy = unit();
    legacy.program = "/original.app/Contents/MacOS/aterm".into();
    assert!(!live.install_qualified(&legacy).unwrap());
    assert!(!live.prove_qualified(&legacy, &expected_health()).unwrap());
    assert_eq!(live.qualified.as_ref().unwrap().installs, 0);
    assert_eq!(live.qualified.as_ref().unwrap().proofs, 0);
}

#[test]
fn physical_acceptance_requires_clear_current_hold_and_lease() {
    let clear = "OK hold=0 hand=- hold_advisory=- hold_by=-";
    assert!(hold_clear(clear, "OK lease none\n").is_ok());
    assert!(hold_clear(&clear.replace("hold=0", "hold=1"), "OK lease none").is_err());
    assert!(
        hold_clear(
            &clear.replace("hand=-", "hand=lease:another"),
            "OK lease none"
        )
        .is_err()
    );
    assert!(hold_clear(clear, "OK lease holder=another").is_err());
    assert!(hold_clear("OK hold=0", "OK lease none").is_err());
}
