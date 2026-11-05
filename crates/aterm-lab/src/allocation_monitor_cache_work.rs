// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Real prepared cold/warm costs through the original window lab owner.

use super::*;
use aterm_capacity::{
    cache_work::{Availability, Collection, Summary},
    epoch_recipe::{RootOutput, RootRecipe},
    epoch_target::{TargetLease, TargetLoan},
    host_protocol::Generation,
};
use atpkg_publish::{
    artifact::tools::HostTools,
    testkit::harness::{self, BIN, Fixture},
};
use std::{
    collections::BTreeSet,
    ffi::OsStr,
    io::Read as _,
    os::unix::fs::{DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _},
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn check(ok: bool, why: &str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(io::Error::other(why).into())
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Artifact {
    inode: [u64; 2],
    length: u64,
    modified: [i64; 2],
    bytes: Vec<u8>,
}

fn artifact(path: &Path) -> Result<Artifact> {
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    let before = file.metadata()?;
    check(
        before.is_file() && before.len() <= 64 << 20,
        "unbounded or nonregular compiler artifact",
    )?;
    let mut bytes = Vec::new();
    file.by_ref().take((64 << 20) + 1).read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    let stamp = |m: &fs::Metadata| {
        (
            m.dev(),
            m.ino(),
            m.len(),
            m.mtime(),
            m.mtime_nsec(),
            m.ctime(),
            m.ctime_nsec(),
        )
    };
    check(
        stamp(&before) == stamp(&after) && bytes.len() as u64 == before.len(),
        "compiler artifact changed during observation",
    )?;
    Ok(Artifact {
        inode: [before.dev(), before.ino()],
        length: before.len(),
        modified: [before.mtime(), before.mtime_nsec()],
        bytes,
    })
}

fn dependency_artifact(stdout: &[u8], target: &Path) -> Result<PathBuf> {
    let mut found = BTreeSet::new();
    for line in std::str::from_utf8(stdout)?.lines() {
        let Ok(frame) = aterm_json::from_str::<aterm_json::Value>(line) else {
            continue;
        };
        if frame["reason"].as_str() != Some("compiler-artifact")
            || frame["target"]["name"].as_str() != Some("unchanged_probe")
        {
            continue;
        }
        let names = frame["filenames"]
            .as_array()
            .ok_or("dependency filenames absent")?;
        for name in names {
            let path = PathBuf::from(name.as_str().ok_or("non-string dependency filename")?);
            if path.extension() == Some(OsStr::new("rlib")) {
                check(
                    path.starts_with(target),
                    "dependency artifact escaped original target",
                )?;
                found.insert(path);
            }
        }
    }
    check(
        found.len() == 1,
        "actual Cargo JSON did not select one unchanged dependency",
    )?;
    Ok(found.into_iter().next().unwrap())
}

fn executable_artifact(stdout: &[u8], target: &Path) -> Result<PathBuf> {
    let mut found = BTreeSet::new();
    for line in std::str::from_utf8(stdout)?.lines() {
        let Ok(frame) = aterm_json::from_str::<aterm_json::Value>(line) else {
            continue;
        };
        if frame["reason"].as_str() == Some("compiler-artifact")
            && frame["target"]["name"].as_str() == Some(BIN)
        {
            let path = PathBuf::from(
                frame["executable"]
                    .as_str()
                    .ok_or("actual executable absent")?,
            );
            check(
                path.starts_with(target),
                "actual executable escaped original target",
            )?;
            found.insert(path);
        }
    }
    check(
        found.len() == 1,
        "actual Cargo JSON did not select one emitted harness",
    )?;
    Ok(found.into_iter().next().unwrap())
}

fn execute(path: &Path, version: &str) -> Result<()> {
    let mut command = spawnfd::command(path);
    let output = aterm_uds::deadline::output_within(&mut command, HANG)?;
    check(
        output.status.success()
            && output.stdout == format!("native-harness-ok {version}\n").as_bytes(),
        "actual emitted harness execution failed",
    )
}

fn complete(collection: &Collection) -> Result<Summary> {
    let Collection::Report(report) = collection else {
        return Err(format!("actual report unavailable: {collection:?}").into());
    };
    check(
        report.availability() == Availability::Complete
            && report.scope() == aterm_capacity::cache_work::SCOPE
            && report.denominators().observed_units > 0
            && report.denominators().attempts_started > 0
            && report.totals().is_some_and(|work| work.metadata_reads > 0),
        "actual original run did not contain complete measured input costs",
    )?;
    Ok(report.summary())
}

struct Builds<'a> {
    world: &'a World,
    targo: &'a Path,
    namespace: &'a Namespace,
    pool: RetainedPool,
}

impl Builds<'_> {
    fn compile(
        &self,
        engine: &mut Engine,
        phase: &str,
        recipe: RootRecipe,
        manifest: &Path,
        target: &Path,
        loan: TargetLoan,
    ) -> Result<(EpochId, RootOutput)> {
        let held = admit_project(
            manifest.parent().ok_or("manifest parent absent")?,
            self.namespace,
        )?;
        let mut caps = Vec::new();
        for _ in 0..2 {
            caps.push(
                self.pool
                    .try_acquire(CpuOwner::current(None))?
                    .ok_or("original CPU units unavailable")?,
            );
        }
        let transport = self
            .world
            .dir
            .join(format!("cache-work-{phase}-transport.noindex"));
        let accepted = engine.owner.submit_epoch_with_recipes(
            held,
            caps,
            EpochConfig {
                helper: self.targo.to_owned(),
                directory: transport.clone(),
                epoch: aterm_uds::rand::hex_token::<16>()?,
                roots: 1,
            },
            vec![recipe],
        )?;
        accepted.result?;
        let id = accepted.id;
        let deadline = Instant::now() + HANG;
        loop {
            engine.live(2)?;
            let status = engine.owner.epoch_status(&id)?;
            check(
                !status.faulted && status.uncertain_units == 0,
                "configured epoch faulted before Opened",
            )?;
            if status.opened {
                break;
            }
            check(Instant::now() < deadline, "configured helper did not open")?;
            engine.owner.wait(deadline)?;
        }
        check(
            matches!(engine.owner.epoch_take_root_output(&id,0), Err(error) if error.kind()==io::ErrorKind::WouldBlock),
            "unclosed epoch exposed output",
        )?;
        engine
            .owner
            .epoch_spawn_root_with_target(&id, 0, manifest, target, loan)?;
        engine.owner.epoch_close(&id)?;
        let deadline = Instant::now() + HANG;
        let mut previous_settled = 0;
        loop {
            engine.live(2)?;
            let status = engine.owner.epoch_status(&id)?;
            check(
                status.settled_units <= previous_settled + 1,
                "one owner tick settled more than one original unit",
            )?;
            previous_settled = status.settled_units;
            check(
                !status.faulted && status.uncertain_units == 0,
                "configured epoch lost conclusive closure",
            )?;
            if status.settled_units == 2 {
                check(
                    status.retired
                        && status.helper_exited
                        && status.control_eof
                        && status.endpoint_cleaned
                        && status.remaining_units == 0
                        && status.failed_roots == 0,
                    "CPU settlement preceded actual closure",
                )?;
                break;
            }
            check(
                Instant::now() < deadline,
                "configured epoch failed to settle",
            )?;
            engine.owner.wait(deadline)?;
        }
        let outcome = engine
            .owner
            .epoch_root_outcome(&id, 0)?
            .ok_or("closed root outcome absent")?;
        check(
            outcome.success && outcome.code == Some(0),
            "actual configured build failed",
        )?;
        check(
            matches!(fs::symlink_metadata(transport.join("credits.fifo")), Err(error) if error.kind()==io::ErrorKind::NotFound),
            "closed original transport remained named",
        )?;
        check(
            self.pool
                .snapshot()?
                .iter()
                .all(|slot| matches!(slot, RetainedSlotState::Available { .. })),
            "closed epoch did not restore original CPU inventory",
        )?;
        let output = engine
            .owner
            .epoch_take_root_output(&id, 0)?
            .ok_or("closed original output absent")?;
        check(
            engine.owner.epoch_take_root_output(&id, 0)?.is_none(),
            "original output was duplicable",
        )?;
        check(
            output.outcome().success && output.cancellation().is_none(),
            "configured output was ineligible",
        )?;
        Ok((id, output))
    }
}

fn scene(
    inst: &Instance,
    engine: &mut Engine,
    out: &Path,
    attachment: u64,
    generation: Generation,
    id: &EpochId,
    summary: &Summary,
) -> Result<(u64, u64, u64)> {
    let read = engine
        .owner
        .observer()
        .read(None)
        .map_err(|error| format!("cache-work observation: {error:?}"))?;
    let snapshot = &read.snapshot;
    check(
        snapshot.generation == generation && !snapshot.closed && snapshot.jobs.is_empty(),
        "reopened monitor changed original source or ordinary drain",
    )?;
    let epoch = snapshot
        .epochs
        .iter()
        .find(|epoch| epoch.sequence == id.sequence())
        .ok_or("actual measured epoch missing")?;
    check(
        epoch.completed_roots == 1
            && epoch.settled_units == 2
            && epoch.remaining_units == 0
            && epoch.cache_work.len() == 1
            && epoch.cache_work[0].root == 0
            && epoch.cache_work[0].summary.as_ref() == Some(summary)
            && epoch.cache_work[0].unavailable.is_none(),
        "monitor lost exact original ticket/root/run costs",
    )?;
    let recent: Vec<_> = read
        .events
        .iter()
        .map(RecordSummary::from)
        .filter(|record| matches!(record.kind, EventKind::Finished { .. } | EventKind::Failed))
        .collect();
    check(
        recent.len() == 2
            && recent.iter().all(|record| {
                record.kind
                    == EventKind::Finished {
                        succeeded: true,
                        cancelled: false,
                    }
            }),
        "reopened scene lost the two original ordinary completions",
    )?;
    let expected = Scene {
        snapshot,
        recent: &recent,
    };
    let (view, index, text) = observe(inst, &expected, engine, 2, attachment)?;
    crate::util::write_fresh(&out.join("owner.snapshot.json"), snapshot.encode()?)?;
    crate::util::write_fresh(&out.join("owner.completions.txt"), format!("{recent:#?}\n"))?;
    crate::util::write_fresh(&out.join("allocation.text.txt"), text)?;
    super::paint::capture(
        inst,
        view,
        out,
        engine,
        &expected,
        attachment,
        id.sequence(),
    )?;
    Ok((view, index, snapshot.revision))
}

/// The original window lab's scene the cache-work run reopens: its world and output folder, the
/// toolchain it builds with, the peer's namespace and instance, and the attachment and generation
/// the original scenes observed.
pub(super) struct Original<'a> {
    pub(super) world: &'a World,
    pub(super) out: &'a Path,
    pub(super) targo: &'a Path,
    pub(super) sdk_bin: &'a Path,
    pub(super) namespace: &'a Namespace,
    pub(super) inst: &'a Instance,
    pub(super) attachment: u64,
    pub(super) generation: Generation,
}

pub(super) fn run(original: &Original<'_>, engine: &mut Engine) -> std::result::Result<(), String> {
    run_checked(original, engine).map_err(|error| format!("real cache-work scene: {error}"))
}

fn run_checked(original: &Original<'_>, engine: &mut Engine) -> Result<()> {
    let &Original {
        world,
        out,
        targo,
        sdk_bin,
        namespace,
        inst,
        attachment,
        generation,
    } = original;
    let root = world.dir.join("cache-work-fixture.noindex");
    fs::DirBuilder::new().mode(0o700).create(&root)?;
    let Fixture {
        ctx,
        host: _host,
        checkout,
        request: _request,
        plan,
        mut prepared,
        counters,
    } = harness::prepare(&root, sdk_bin)?;
    check(
        plan.command.program.as_path() == targo && counters.read()? == [0, 0, 0],
        "prepared driver or original counters changed",
    )?;
    let target = PathBuf::from(
        plan.command
            .env_of("CARGO_TARGET_DIR")
            .ok_or("prepared target absent")?,
    );
    let target_metadata = fs::symlink_metadata(&target)?;
    let target_identity = [target_metadata.dev(), target_metadata.ino()];
    let manifest = checkout.join("Cargo.toml");
    let recipe = prepared.recipe(0)?;
    let collector = prepared.cache_work_collector(0)?;
    let version = plan
        .harness
        .as_ref()
        .ok_or("harness version absent")?
        .version
        .clone();
    let cold_dir = out.join("cold-cache-work");
    let warm_dir = out.join("warm-cache-work");
    let warm_logs = root.join("warm-cache-work.noindex");
    for dir in [&cold_dir, &warm_dir, &warm_logs] {
        fs::DirBuilder::new().mode(0o700).create(dir)?;
    }
    let builds = Builds {
        world,
        targo,
        namespace,
        pool: RetainedPool::create_private(
            &world.dir.join("cache-work-cpu.noindex"),
            NonZeroU32::new(2).unwrap(),
        )?,
    };
    let (cold_id, output) = builds.compile(
        engine,
        "cold",
        recipe.clone(),
        &manifest,
        &target,
        prepared.target_loan()?,
    )?;
    let completion = prepared.finish_root_output_with_cache_work(&ctx, &HostTools, output)?;
    let cold = complete(&completion.cache_work)?;
    engine
        .owner
        .epoch_record_cache_work(&cold_id, 0, &completion.cache_work)?;
    let built = completion.built?;
    check(
        counters.read()? == [1, 1, 1],
        "cold scripts did not each execute once",
    )?;
    let stdout = fs::read(built.logs.join("build.stdout"))?;
    let dependency = dependency_artifact(&stdout, &target)?;
    let original_dependency = artifact(&dependency)?;
    let compiler_binary = executable_artifact(&stdout, &target)?;
    let original_compiler_binary = artifact(&compiler_binary)?;
    let binary = built.dest.join("bin").join(BIN);
    let original_binary = artifact(&binary)?;
    let receipt = fs::read(built.dest.join("local-build.json"))?;
    execute(&binary, &version)?;
    execute(&compiler_binary, &version)?;
    require(
        inst.ctl(&["resize", "px", "1400", "1800"]),
        "resize cost scene",
    )?;
    require(
        inst.ctl(&["open", "app", "allocation"]),
        "reopen Allocation on original attachment",
    )?;
    let (view, index, cold_revision) = scene(
        inst, engine, &cold_dir, attachment, generation, &cold_id, &cold,
    )?;

    let mut lease = TargetLease::acquire(&target)?;
    let (warm_id, output) =
        builds.compile(engine, "warm", recipe, &manifest, &target, lease.loan()?)?;
    let bytes = output.read(64 << 20)?;
    check(
        bytes.outcome.success && bytes.cancellation.is_none(),
        "actual warm output failed",
    )?;
    check(
        matches!(lease.loan(), Err(error) if error.kind() == io::ErrorKind::WouldBlock),
        "original output bytes lost their one-use target loan",
    )?;
    let collection = collector.collect(&HostTools, &bytes, &warm_logs);
    let warm = complete(&collection)?;
    check(
        warm.run_id != cold.run_id,
        "cold and warm reused an analysis run identity",
    )?;
    engine
        .owner
        .epoch_record_cache_work(&warm_id, 0, &collection)?;
    check(
        counters.read()? == [1, 1, 1]
            && artifact(&dependency)? == original_dependency
            && artifact(&compiler_binary)? == original_compiler_binary
            && artifact(&binary)? == original_binary
            && fs::read(built.dest.join("local-build.json"))? == receipt,
        "warm build reran a script or changed dependency/publication bytes",
    )?;
    let current_target = fs::symlink_metadata(&target)?;
    check(
        [current_target.dev(), current_target.ino()] == target_identity,
        "warm build replaced original target directory",
    )?;
    check(
        dependency_artifact(&bytes.stdout, &target)? == dependency,
        "warm actual Cargo JSON selected another dependency",
    )?;
    check(
        executable_artifact(&bytes.stdout, &target)? == compiler_binary,
        "warm actual Cargo JSON selected another executable",
    )?;
    execute(&compiler_binary, &version)?;
    crate::util::write_fresh(&warm_dir.join("root.stdout"), &bytes.stdout)?;
    crate::util::write_fresh(&warm_dir.join("root.stderr"), &bytes.stderr)?;
    let (next_view, next_index, warm_revision) = scene(
        inst, engine, &warm_dir, attachment, generation, &warm_id, &warm,
    )?;
    check(
        next_view == view && next_index == index && warm_revision > cold_revision,
        "cold/warm observation changed view or failed to advance original source",
    )?;
    drop(bytes);
    drop(lease.loan()?);
    drop(lease);
    drop(TargetLease::acquire(&target)?);
    check(
        engine.owner.is_empty() && Ledger::at(namespace.fence()).list()?.is_empty(),
        "real cold/warm work did not drain original owner and ledger",
    )?;
    crate::util::write_fresh(
        &out.join("cache-work.result.txt"),
        format!(
            "PASS\nattachment={attachment}\nattachments=1\ncold_epoch={}\nwarm_epoch={}\ncold_run={}\nwarm_run={}\nscope={}\ncold_revision={cold_revision}\nwarm_revision={warm_revision}\ncounters=1,1,1\nsame_target=true\nunchanged_dependency=true\nunchanged_publication=true\ncold={cold:?}\nwarm={warm:?}\n",
            cold_id.sequence(),
            warm_id.sequence(),
            cold.run_id,
            warm.run_id,
            aterm_capacity::cache_work::SCOPE
        ),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actual_artifact_selection_rejects_missing_ambiguous_and_foreign_paths() {
        let target = Path::new("/original/target");
        let valid = br#"{"reason":"compiler-artifact","target":{"name":"unchanged_probe"},"filenames":["/original/target/libprobe.rlib"]}
{"reason":"compiler-artifact","target":{"name":"claude-alab"},"executable":"/original/target/claude-alab"}
"#;
        assert_eq!(
            dependency_artifact(valid, target).unwrap(),
            target.join("libprobe.rlib")
        );
        assert_eq!(
            executable_artifact(valid, target).unwrap(),
            target.join("claude-alab")
        );
        assert!(dependency_artifact(b"", target).is_err());
        assert!(executable_artifact(b"", target).is_err());
        let foreign = String::from_utf8(valid.to_vec())
            .unwrap()
            .replace("/original/target/", "/original/target-foreign/");
        assert!(dependency_artifact(foreign.as_bytes(), target).is_err());
        assert!(executable_artifact(foreign.as_bytes(), target).is_err());
        let ambiguous = format!(
            "{}{}",
            String::from_utf8_lossy(valid),
            String::from_utf8_lossy(valid)
                .replace("libprobe.rlib", "other.rlib")
                .replace("target/claude-alab", "target/other")
        );
        assert!(dependency_artifact(ambiguous.as_bytes(), target).is_err());
        assert!(executable_artifact(ambiguous.as_bytes(), target).is_err());
    }
}
