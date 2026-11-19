// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A single SD-72 native-reader qualification in a disposable macOS guest.

use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};

use aterm_json::{Value, json};
use aterm_uds::spawnfd::Stdio;

use crate::mac::{self, Vm};
use crate::mac_component::{copy_image, fail, json_file, save};
use crate::self_cpu_qualification::{Bound, Proof, validate};
use crate::util::shell_quote as q;
use crate::{Fail, Res, stop};

const FIXTURE: &str = "aterm-self-cpu-fixture";
const ROOT: &str = "/tmp/.ubd/aterm-self-cpu.noindex";
const CPUS: u32 = 4;
const MEMORY_MIB: u32 = 8192;
const DISK_GB: u32 = 60;
const SHAPE: mac::Shape = mac::Shape {
    cpus: CPUS,
    memory_mib: MEMORY_MIB,
    disk_gb: DISK_GB,
};
const MAX_BUILD_OUTPUT: u64 = 16 * 1024 * 1024;
const USAGE: &str = "mac self-cpu --record NEW_DIR [--image REF]\n\
  Build the dev-only actual-reader fixture and copy its exact bytes into one disposable\n\
  SIP-on macOS guest (4 vCPUs, 8 GiB, disk reported in the 60 GB bucket, headless).\n\
  Actual disk capacity is below 61 decimal GB; no exact 60 GB capacity is assumed.\n\
  The fixture refuses workload\n\
  unless kern.hv_vmm_present proves a VM. A guarded child runs one five-second named\n\
  worker; its supervisor collects owning wait4 accounting after that child exits.\n\
  Retain build/provenance, complete child output, saved exit accounting and raw cuts.\n\
  Exit 0 only when published worker CPU proves at least 80% of saved exit-accounted\n\
  CPU with qualified conversion rounding; 3 unavailable, 1 failed.\n";

struct Ask {
    record: PathBuf,
    image: String,
}

fn parse(args: &[String]) -> Result<Ask, Fail> {
    let (mut record, mut image) = (None, None);
    let mut words = args.chunks_exact(2);
    for pair in &mut words {
        match pair[0].as_str() {
            "--record" if record.is_none() && !pair[1].is_empty() => {
                record = Some(PathBuf::from(crate::paths::from_caller(&pair[1])));
            }
            "--image" if image.is_none() && !pair[1].is_empty() => image = Some(pair[1].clone()),
            _ => return Err(Fail::usage(USAGE)),
        }
    }
    if !words.remainder().is_empty() {
        return Err(Fail::usage(USAGE));
    }
    Ok(Ask {
        record: record.ok_or_else(|| Fail::usage(USAGE))?,
        image: image.unwrap_or_else(|| mac::IMAGE.into()),
    })
}

fn fixture_artifact(json_lines: &str) -> Result<PathBuf, Fail> {
    let mut paths = json_lines
        .lines()
        .filter_map(|line| aterm_json::from_str::<Value>(line).ok())
        .filter(|v| {
            v["reason"].as_str() == Some("compiler-artifact")
                && v["target"]["name"].as_str() == Some(FIXTURE)
        })
        .filter_map(|v| v["executable"].as_str().map(PathBuf::from));
    let path = paths
        .next()
        .ok_or_else(|| Fail::die("the build emitted no self-CPU fixture executable"))?;
    if paths.next().is_some() || !path.is_absolute() || !path.is_file() {
        return Err(Fail::die(
            "the build did not name one exact fixture executable",
        ));
    }
    Ok(path)
}

fn build_fixture(record: &Path) -> Result<PathBuf, Fail> {
    let args = [
        "--unverified",
        "build",
        "--profile",
        "test",
        "-p",
        "aterm-lab",
        "--bin",
        FIXTURE,
        "--message-format=json-render-diagnostics",
    ];
    let stdout = record.join("build.stdout.jsonl");
    let stderr = record.join("build.stderr.log");
    let out = crate::util::create_new_regular_file(&stdout, 0o600).map_err(fail)?;
    let err = crate::util::create_new_regular_file(&stderr, 0o600).map_err(fail)?;
    let claim = crate::util::claim_build(&args, None, &crate::paths::paths().repo).map_err(fail)?;
    let status = crate::util::status_claimed(
        crate::util::targo(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::from(out))
            .stderr(Stdio::from(err)),
        claim,
    )
    .map_err(fail)?;
    json_file(
        record,
        "build.json",
        &json!({"exit":status.code(),"success":status.success(),
        "stdout":"build.stdout.jsonl","stderr":"build.stderr.log","profile":"test"}),
    )?;
    if !status.success() {
        return Err(Fail::die(
            "self-CPU fixture build failed; retained complete build logs",
        ));
    }
    if std::fs::metadata(&stdout).map_err(fail)?.len() > MAX_BUILD_OUTPUT {
        return Err(Fail::die("fixture build output exceeds its parse bound"));
    }
    fixture_artifact(&std::fs::read_to_string(&stdout).map_err(fail)?)
}

fn checked_output(stdout: &[u8], success: bool) -> Result<Bound, String> {
    let proof = aterm_json::from_slice::<Proof>(stdout).map_err(|e| e.to_string())?;
    let bound = validate(&proof)?;
    if !success {
        return Err("the guest fixture did not exit successfully".into());
    }
    Ok(bound)
}

fn qualify(vm: &Vm, ask: &Ask, fixture: &Value) -> Result<(i32, Value), Fail> {
    let sha = fixture["sha256"]
        .as_str()
        .ok_or_else(|| Fail::die("fixture has no hash"))?;
    let setup = format!(
        "set -eu; umask 077; mkdir -p /tmp/.ubd; test ! -L /tmp/.ubd; \
         test \"$(cd /tmp/.ubd && /bin/pwd -P)\" = /private/tmp/.ubd; mkdir {ROOT}; \
         test \"$(/usr/bin/stat -f %u {ROOT})\" = \"$(id -u)\"; \
         test \"$(/usr/bin/stat -f %Lp {ROOT})\" = 700; \
         cp {share}/{FIXTURE} {ROOT}/{FIXTURE}; chmod 700 {ROOT}/{FIXTURE}; \
         test \"$(/usr/bin/shasum -a 256 {ROOT}/{FIXTURE} | /usr/bin/cut -d ' ' -f 1)\" = {sha}; \
         /usr/bin/sw_vers; /usr/bin/uname -m; /usr/bin/csrutil status; \
         /usr/sbin/sysctl -n kern.hv_vmm_present",
        share = q(mac::GUEST_SHARE),
        sha = q(sha),
    );
    let setup_out = vm.exec_output(&setup, mac::STEP).map_err(fail)?;
    save(&ask.record, "setup.stdout", &setup_out.stdout)?;
    save(&ask.record, "setup.stderr", &setup_out.stderr)?;
    if !setup_out.status.success() {
        return Err(Fail::die(format!(
            "guest fixture setup failed: {}",
            setup_out.status
        )));
    }
    let captured = vm.exec_output(&format!("exec {ROOT}/{FIXTURE}"), mac::ROW);
    let out = match captured {
        Ok(out) => out,
        Err(why) => {
            json_file(
                &ask.record,
                "result.json",
                &json!({"complete":false,"qualification":false,"transport_error":why}),
            )?;
            return Err(Fail::die(
                "guest fixture output was unavailable or incomplete",
            ));
        }
    };
    save(&ask.record, "fixture.stdout.json", &out.stdout)?;
    save(&ask.record, "fixture.stderr", &out.stderr)?;
    if let Ok(proof) = aterm_json::from_slice::<Proof>(&out.stdout) {
        if proof.child_stdout.len() as u64 <= crate::self_cpu_qualification::MAX_CHILD_OUTPUT
            && proof.child_stderr.len() as u64 <= crate::self_cpu_qualification::MAX_CHILD_OUTPUT
        {
            save(&ask.record, "child.stdout.json", &proof.child_stdout)?;
            save(&ask.record, "child.stderr", &proof.child_stderr)?;
        }
    }
    let checked = checked_output(&out.stdout, out.status.success());
    let passed = checked.is_ok();
    let code = if passed {
        0
    } else if out.status.code() == Some(mac::NOT_RUN) {
        mac::NOT_RUN
    } else {
        1
    };
    Ok((
        code,
        json!({"complete":true,"exit":out.status.code(),
        "qualification":passed,"checked_bound":checked.as_ref().ok(),"failure":checked.as_ref().err(),
        "fixture_sha256":sha,"guest_only_workload":true,"shipping_certified":false,
        "comparison":"published_worker_vs_saved_exit_accounting"}),
    ))
}

fn finish_qualification(
    record: &Path,
    work: Result<(i32, Value), Fail>,
    cleanup: impl FnOnce() -> Result<(), Fail>,
) -> Res {
    let observed_red = work.as_ref().is_ok_and(|(code, _)| *code == 1);
    let work = work.and_then(|(code, report)| {
        json_file(
            record,
            "fixture-observation.json",
            &json!({"complete":false,"scope":"guest fixture before checked VM retirement",
                "vm_retirement_confirmed":false,"guest_result":&report}),
        )?;
        Ok((code, report))
    });
    let (code, report) = mac::after_vm_cleanup(work, cleanup).map_err(|mut error| {
        if observed_red {
            error.code = 1;
        }
        error
    })?;
    json_file(record, "result.json", &report)?;
    println!(
        "mac self-cpu: {}; evidence={}",
        if code == 0 { "PASS" } else { "NOT QUALIFIED" },
        record.display()
    );
    Ok(code)
}

pub(crate) fn cmd(args: &[String]) -> Res {
    if args == ["--help"] || args == ["help"] || args == ["-h"] {
        print!("{USAGE}");
        return Ok(0);
    }
    let mut ask = parse(args)?;
    stop::install();
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&ask.record)
        .map_err(fail)?;
    ask.record = ask.record.canonicalize().map_err(fail)?;
    aterm_uds::private_file::check_private_dir(&ask.record).map_err(fail)?;
    let run = || -> Res {
        let fixture = build_fixture(&ask.record)?;
        let mut scratch = mac::VmScratch::new(
            aterm_tempfile::Builder::new()
                .prefix("mac-self-cpu-")
                .suffix(".noindex")
                .tempdir()
                .map_err(fail)?,
            &ask.record,
        )?;
        let share = scratch.path().join("share");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&share)
            .map_err(fail)?;
        let input = copy_image(&fixture, &share, FIXTURE)?;
        json_file(
            &ask.record,
            "inputs.json",
            &json!({"fixture":input,"image":ask.image,
            "guest":{"cpus":CPUS,"memory_mib":MEMORY_MIB,"disk_policy":SHAPE.disk_interval()?,"graphics":false},
            "host_workload":false,"guest_kernel_guard":"kern.hv_vmm_present=1"}),
        )?;
        let tart = mac::tart_to_run()?;
        let need = crate::vm_admit::Guest {
            lane: "mac-self-cpu",
            vm: "mac-self-cpu",
            memory: u64::from(MEMORY_MIB) << 20,
            cpus: CPUS,
        };
        let mut admitted = crate::vm_admit::admit(&need, true)?;
        let reservation = mac::retain_admission(&mut admitted)?;
        let base = mac::prepared_base_with_admission(
            &tart,
            &ask.image,
            &mac::Prep {
                shape: Some(SHAPE),
                ..mac::Prep::default()
            },
            &ask.record,
            &share,
            &reservation,
            &mut || Ok(()),
        )?;
        let mut vm =
            Vm::clone_from_recorded(&tart, &base, &ask.record, &share, false, &ask.record)?;
        vm.shape = Some(SHAPE);
        vm.graphics = false;
        let work = (|| -> Result<(i32, Value), Fail> {
            vm.hold_reserved_admission(&reservation)?;
            mac::boot_sip_on(&mut vm, &base)?;
            let config = aterm_uds::deadline::output_with_input_until(
                aterm_uds::spawnfd::command(&tart).args(["get", &vm.name, "--format", "json"]),
                aterm_uds::secret::Secret::new(&[]),
                std::time::Instant::now() + mac::STEP,
                aterm_uds::deadline::MAX_COMPLETE_STDOUT_BYTES,
            )
            .map_err(fail)?;
            save(&ask.record, "vm-config.json", &config.stdout)?;
            save(&ask.record, "vm-config.stderr", &config.stderr)?;
            if !config.status.success() {
                return Err(Fail::die("guest configuration could not be recorded"));
            }
            json_file(
                &ask.record,
                "vm.json",
                &json!({"name":vm.name,"prepared_base":base,
                "resource_claim_held_through_teardown":true,"tart_log":"tart-run.log"}),
            )?;
            qualify(&vm, &ask, &input)
        })();
        let result = finish_qualification(&ask.record, work, || vm.finish().map(|_| ()))?;
        scratch.discard_after_finish();
        Ok(result)
    };
    let result = run();
    if let Err(why) = &result {
        json_file(
            &ask.record,
            "error.json",
            &json!({"code":why.code,"message":why.msg,"signal":why.stopped,
            "qualification":false,"host_workload":false}),
        )?;
    }
    result
}

#[cfg(test)]
#[path = "mac_self_cpu_tests.rs"]
mod tests;
