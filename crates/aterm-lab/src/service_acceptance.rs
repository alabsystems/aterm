// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Physical server acceptance custody. This intentionally uses the real login's service
//! label; it never claims that a scratch HOME isolates launchd. The old GUI stays running.

use crate::{Fail, Res};
use aterm_agent::service_activation::NativeBackend;
use aterm_json::Value;
use aterm_secrets::{
    Error, Result,
    client::{self, Paths},
    service::{self, Job},
    service_acceptance::{self, Environment},
    service_repair::{Backend, HealthRequirement, Observed, Unit},
};
use aterm_update::service_artifact::{SignedRelease, Store};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[path = "service_acceptance_refusal.rs"]
mod refusal;

const USAGE: &str = "service-acceptance begin ID SOURCE_APP EVIDENCE_DIR CTL_EXECUTABLE | restore ID CTL_EXECUTABLE (real unpinned user server; debug lab only)";
fn error(e: impl std::fmt::Display) -> Error {
    Error::new(e.to_string())
}
fn read(path: &Path) -> Result<Vec<u8>> {
    aterm_secrets::fsx::read_owned(path, 16 * 1024 * 1024)?
        .ok_or_else(|| error("missing acceptance evidence"))
}
fn run(program: &Path, args: &[&str]) -> Result<String> {
    let mut command = aterm_uds::spawnfd::command(program);
    command.args(args);
    let output =
        aterm_uds::deadline::output_within(&mut command, Duration::from_secs(60)).map_err(error)?;
    if !output.status.success() {
        return Err(error(format!(
            "{} failed: {}",
            program.display(),
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    String::from_utf8(output.stdout).map_err(error)
}
fn hold_clear(status: &str, lease: &str) -> Result<()> {
    let field = |name: &str| {
        status
            .split_whitespace()
            .find_map(|word| word.strip_prefix(name))
    };
    if !status.starts_with("OK ")
        || field("hold=") != Some("0")
        || field("hand=") != Some("-")
        || field("hold_advisory=") != Some("-")
        || field("hold_by=") != Some("-")
        || lease.trim() != "OK lease none"
    {
        return Err(error(
            "service acceptance is held or has an active drive lease",
        ));
    }
    Ok(())
}
struct Live<B: Backend = NativeBackend> {
    paths: Paths,
    ctl: PathBuf,
    baseline: Option<PathBuf>,
    qualified: Option<B>,
}
impl<B: Backend> Live<B> {
    fn install_qualified(&mut self, unit: &Unit) -> Result<bool> {
        if self.baseline.as_deref() != Some(Path::new(&unit.program)) {
            return Ok(false);
        }
        self.qualified
            .as_mut()
            .ok_or_else(|| error("qualified baseline backend is absent"))?
            .install(Job::Server, unit)?;
        Ok(true)
    }

    fn prove_qualified(&mut self, unit: &Unit, health: &HealthRequirement) -> Result<bool> {
        if self.baseline.as_deref() != Some(Path::new(&unit.program)) {
            return Ok(false);
        }
        self.qualified
            .as_mut()
            .ok_or_else(|| error("qualified baseline backend is absent"))?
            .prove(Job::Server, unit, health)?;
        Ok(true)
    }
}
impl<B: Backend> Environment for Live<B> {
    fn guard(&mut self) -> Result<()> {
        if !aterm_secrets::swarm::enabled_in_home(&self.paths.home)? {
            return Err(error("sharing is Off"));
        }
        let status = run(&self.ctl, &["ctl", "@self", "status"])?;
        let lease = run(&self.ctl, &["ctl", "@self", "lease", "status"])?;
        hold_clear(&status, &lease)
    }
    fn observe(&mut self) -> Result<Observed> {
        service::repair_observe(&self.paths.home, Job::Server)
    }
    fn install(&mut self, unit: &Unit) -> Result<()> {
        self.guard()?;
        if self.install_qualified(unit)? {
            return Ok(());
        }
        // Legacy restoration retains nonce rotation without claiming a nonce-bound proof.
        aterm_secrets::service_health::prepare_server(&self.paths)?;
        service::repair_install(&self.paths.home, Job::Server, unit)
    }
    fn prove(&mut self, unit: &Unit, health: &HealthRequirement) -> Result<()> {
        if self.prove_qualified(unit, health)? {
            return Ok(());
        }
        let (_, url) =
            aterm_secrets::service_health::server_address(&self.paths, &unit.args, &health.vault)?;
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let proof = (|| {
                service::repair_prove_image(&self.paths.home, Job::Server, unit)?;
                aterm_secrets::service_health::server(&url, health)?;
                service::repair_prove_image(&self.paths.home, Job::Server, unit)
            })();
            match proof {
                Ok(()) => return Ok(()),
                Err(e) if Instant::now() >= deadline => return Err(e),
                Err(_) => std::thread::sleep(Duration::from_millis(200)),
            }
        }
    }
}
fn health(paths: &Paths) -> Result<HealthRequirement> {
    let config = client::config(paths)?.ok_or_else(|| error("no configured vault"))?;
    if !client::materialization_ready(paths)? {
        return Err(error("local vault is not materialized"));
    }
    let frontier = match client::status(paths)?.membership {
        client::Membership::In { seq, .. } => seq,
        _ => return Err(error("not a vault member")),
    };
    let records = client::load_records(paths)?
        .frontier()
        .iter()
        .map(|id| aterm_codec::hex::encode_lower(id))
        .collect::<Vec<_>>();
    if records.is_empty() {
        return Err(error("no signed membership frontier"));
    }
    Ok(HealthRequirement {
        vault: config.name,
        signer: aterm_codec::hex::encode_lower(
            &config.signer.ok_or_else(|| error("vault is unsigned"))?,
        ),
        records,
        frontier,
    })
}
fn begin(id: &str, source: &Path, evidence: &Path, ctl: &Path) -> Result<Value> {
    if !(16..=128).contains(&id.len())
        || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(error("invalid acceptance id"));
    }
    let paths = Paths::live()?;
    let mut live: Live = Live {
        paths: paths.clone(),
        ctl: ctl.into(),
        baseline: None,
        qualified: None,
    };
    live.guard()?;
    let root = paths.home.join(".local/share/aterm/service-images");
    let signed = SignedRelease {
        manifest: String::from_utf8(read(&evidence.join("aterm-appcast.toml"))?).map_err(error)?,
        manifest_signature: read(&evidence.join("aterm-appcast.toml.sig"))?,
        roster: read(&evidence.join("aterm-machines.toml"))?,
        roster_signature: read(&evidence.join("aterm-machines.toml.sig"))?,
    };
    let now =
        i64::try_from(aterm_time::unix_millis().ok_or_else(|| error("clock unavailable"))? / 1000)
            .map_err(error)?;
    let store = Store::new(root).map_err(error)?;
    let receipt = store
        .materialize(
            &signed,
            source,
            &read(&evidence.join("aterm-service-qualification.json"))?,
            now,
            0,
        )
        .map_err(error)?;
    // The source has now passed the same signed-release and native-image checks. Use its
    // ordinary staging command to retain authority.json under the production lock.
    let source_program = source.join("Contents/MacOS/aterm");
    run(
        &source_program,
        &[
            "secrets",
            "service-repair",
            "stage",
            "--evidence",
            evidence
                .to_str()
                .ok_or_else(|| error("non-UTF8 evidence path"))?,
            "--image",
            source
                .to_str()
                .ok_or_else(|| error("non-UTF8 image path"))?,
        ],
    )?;
    let run_dir = paths
        .home
        .join(".local/share/aterm/service-acceptance")
        .join(id);
    if run_dir.try_exists()? {
        return Err(error("acceptance baseline directory already exists"));
    }
    aterm_secrets::fsx::ensure_private_dir(&run_dir)?;
    let baseline = run_dir.join("baseline.app");
    run(
        Path::new("/usr/bin/ditto"),
        &[
            "--rsrc",
            "--extattr",
            source
                .to_str()
                .ok_or_else(|| error("non-UTF8 image path"))?,
            baseline
                .to_str()
                .ok_or_else(|| error("non-UTF8 baseline path"))?,
        ],
    )?;
    // Re-materialization re-verifies every baseline byte and native signature without
    // replacing the existing content-addressed candidate.
    let baseline_receipt = store
        .materialize(
            &signed,
            &baseline,
            &read(&evidence.join("aterm-service-qualification.json"))?,
            now,
            0,
        )
        .map_err(error)?;
    if baseline_receipt.build == 0 || baseline_receipt != receipt {
        return Err(error(
            "qualified baseline differs from the same authenticated signed image",
        ));
    }
    let program = baseline.join("Contents/MacOS/aterm");
    live.baseline = Some(program.clone());
    live.qualified = Some(NativeBackend::new(
        paths.clone(),
        None,
        baseline_receipt.build,
    ));
    let expected = health(&paths)?;
    service_acceptance::begin(
        &paths,
        id,
        &program,
        &store.executable(&receipt).map_err(error)?,
        &receipt.service.image_sha256,
        &receipt.service.executable_sha256,
        expected,
        &mut live,
    )
}

/// Set up or restore a real user server, retaining exact durable custody throughout.
/// # Errors
/// Unsupported options, unavailable policy evidence, signed-image verification, or custody failure.
pub fn cmd(args: &[String]) -> Res {
    if args.first().is_some_and(|verb| verb == "reject-verified") {
        return refusal::cmd(&args[1..]);
    }
    let result = match args {
        [verb, id, source, evidence, ctl] if verb == "begin" => {
            begin(id, Path::new(source), Path::new(evidence), Path::new(ctl))
        }
        [verb, id, ctl] if verb == "restore" => {
            let paths = Paths::live().map_err(|e| Fail::die(e.to_string()))?;
            service_acceptance::restore(
                &paths,
                id,
                &mut Live::<NativeBackend> {
                    paths: paths.clone(),
                    ctl: ctl.into(),
                    baseline: None,
                    qualified: None,
                },
            )
        }
        _ => return Err(Fail::usage(USAGE)),
    }
    .map_err(|e| Fail::die(e.to_string()))?;
    println!(
        "{}",
        aterm_json::to_string(&result).map_err(|e| Fail::die(e.to_string()))?
    );
    Ok(0)
}

#[cfg(test)]
mod tests {
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
}
