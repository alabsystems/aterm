// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native SDK source qualification in a clean OS process universe. The existing
//! inner IP and real-home guards stay intact; no consumer census is waived.

use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use aterm_agent::harness::codex_alab;
use aterm_json::{Value, json};

use crate::mac::{self, Vm};
use crate::mac_component::{capture, copy_image, fail, json_file, save, unavailable};
use crate::util::shell_quote as q;
use crate::{Fail, Res, stop};

const USAGE: &str = "mac codex-source-proof --aterm FILE --package DIR --record NEW_DIR [--image REF]\n\
  Run the debug SDK's existing native source component wholly inside a disposable SIP-on\n\
  macOS guest. Preserve its IP and real-home guards and require no pre-existing Codex process.\n\
  Copy and verify the exact host, lab, package metadata and SDK image; retain input hashes,\n\
  stdout, stderr, exit and the private host log before stopping/deleting this VM.\n\
  Fixed guest: 4 vCPUs, 8 GiB RAM, 60 GB disk, headless. No guest build, app installation,\n\
  Full Disk Access grant or host application launch. Debug component evidence only:\n\
  this does not certify shipping, enable runtime upgrade or prove work preservation.\n";
const ROOT: &str = "/tmp/.ubd/aterm-codex-source.noindex";
const CPUS: u32 = 4;
const MEMORY_MIB: u32 = 8192;
const DISK_GB: u32 = 60;
// Outer transport ceiling; the component retains its own per-step hang detectors.
const COMPONENT: Duration = Duration::from_secs(3600);

struct Ask {
    aterm: PathBuf,
    package: PathBuf,
    record: PathBuf,
    image: String,
}

fn parse(args: &[String]) -> Result<Ask, Fail> {
    let (mut aterm, mut package, mut record, mut image) = (None, None, None, None);
    let mut at = 0;
    while at < args.len() {
        let flag = &args[at];
        let value = args
            .get(at + 1)
            .ok_or_else(|| Fail::usage(format!("{flag} needs a value\n{USAGE}")))?;
        match flag.as_str() {
            "--aterm" if aterm.is_none() => {
                aterm = Some(PathBuf::from(crate::paths::from_caller(value)))
            }
            "--package" if package.is_none() => {
                package = Some(PathBuf::from(crate::paths::from_caller(value)))
            }
            "--record" if record.is_none() => {
                record = Some(PathBuf::from(crate::paths::from_caller(value)))
            }
            "--image" if image.is_none() && !value.is_empty() => image = Some(value.clone()),
            _ => {
                return Err(Fail::usage(format!(
                    "unexpected argument {flag:?}\n{USAGE}"
                )));
            }
        }
        at += 2;
    }
    Ok(Ask {
        aterm: aterm.ok_or_else(|| Fail::usage(USAGE))?,
        package: package.ok_or_else(|| Fail::usage(USAGE))?,
        record: record.ok_or_else(|| Fail::usage(USAGE))?,
        image: image.unwrap_or_else(|| {
            std::env::var("LAB_MAC_IMAGE").unwrap_or_else(|_| mac::IMAGE.into())
        }),
    })
}

/// Output identifies capture data only. Canonical ownership is rechecked in the guest.
fn world_from_output(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut worlds = text
        .lines()
        .filter_map(|line| line.strip_prefix("WORLD isolated_source_proof="));
    let world = worlds.next()?;
    if worlds.next().is_some() {
        return None;
    }
    let path = Path::new(world);
    let name = path.file_name()?.to_str()?;
    (matches!(path.parent()?.to_str()?, "/tmp/.ubd" | "/private/tmp/.ubd")
        && name.starts_with("atsp")
        && name.ends_with(".noindex")
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte)))
    .then(|| world.to_owned())
}

fn inputs(ask: &Ask, share: &Path) -> Result<Value, Fail> {
    let manifest = crate::util::read_regular_text(&ask.package.join(codex_alab::PACKAGE_JSON))
        .map_err(fail)?;
    if codex_alab::package_entrypoint(&manifest).as_deref() != Some(Path::new("bin/codex")) {
        return Err(Fail::usage("the macOS debug package must enter bin/codex"));
    }
    let sdk = ask.package.join("bin/codex");
    if !codex_alab::isolated_source_proof_candidate(&sdk).map_err(fail)? {
        return Err(Fail::usage(
            "package lacks the explicit debug source proof feature",
        ));
    }
    let package = share.join("candidate");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(package.join("bin"))
        .map_err(fail)?;
    let sdk = copy_image(&sdk, &package, "bin/codex")?;
    let mut metadata = Vec::new();
    for name in [codex_alab::PACKAGE_JSON, codex_alab::ALAB_BUILD_JSON] {
        let bytes = crate::util::read_regular_file(&ask.package.join(name)).map_err(fail)?;
        save(&package, name, &bytes)?;
        metadata.push(json!({"name":name,"bytes":bytes.len(),
            "sha256":crate::windows::sha256_file(&package.join(name))?}));
    }
    Ok(json!({"aterm":copy_image(&ask.aterm,share,"native-host")?,
        "lab":copy_image(&std::env::current_exe().map_err(fail)?,share,"aterm-lab")?,
        "sdk":sdk,"metadata":metadata,"image":ask.image,
        "guest":{"cpus":CPUS,"memory_mib":MEMORY_MIB,"disk_gb":DISK_GB,"tart_graphics":false},
        "component_transport_ceiling_seconds":COMPONENT.as_secs(),
        "guest_compilation":false,"host_app_execution":false,"qualification_only":true,
        "runtime_activation":false,"inner_home_and_ip_guards_preserved":true}))
}

fn guest(vm: &Vm, ask: &Ask, inputs: &Value) -> Res {
    let mut setup = format!(
        "set -eu; umask 077; mkdir -p /tmp/.ubd; test ! -L /tmp/.ubd; \
         test \"$(cd /tmp/.ubd && /bin/pwd -P)\" = /private/tmp/.ubd; mkdir {ROOT}; \
         test \"$(/usr/bin/stat -f %u {ROOT})\" = \"$(id -u)\"; \
         test \"$(/usr/bin/stat -f %Lp {ROOT})\" = 700; \
         cp {share}/native-host {ROOT}/native-host; cp {share}/aterm-lab {ROOT}/aterm-lab; \
         cp -R {share}/candidate {ROOT}/candidate; chmod 700 {ROOT}/native-host {ROOT}/aterm-lab {ROOT}/candidate/bin/codex; ",
        share = q(mac::GUEST_SHARE),
    );
    let hashes = [
        ("native-host", inputs["aterm"]["sha256"].as_str()),
        ("aterm-lab", inputs["lab"]["sha256"].as_str()),
        ("candidate/bin/codex", inputs["sdk"]["sha256"].as_str()),
    ];
    for (name, hash) in hashes {
        setup.push_str(&format!(
            "test \"$(/usr/bin/shasum -a 256 {ROOT}/{name} | /usr/bin/cut -d ' ' -f 1)\" = {}; ",
            q(hash.expect("hashed input")),
        ));
    }
    for metadata in inputs["metadata"].as_array().expect("hashed metadata") {
        let name = metadata["name"].as_str().expect("metadata name");
        let path = q(&format!("{ROOT}/candidate/{name}"));
        setup.push_str(&format!(
            "test \"$(/usr/bin/shasum -a 256 {path} | /usr/bin/cut -d ' ' -f 1)\" = {}; ",
            q(metadata["sha256"].as_str().expect("metadata hash")),
        ));
    }
    let setup = vm.exec_complete(&setup, mac::STEP).map_err(Fail::die)?;
    save(&ask.record, "setup.stdout", &setup.stdout)?;
    save(&ask.record, "setup.stderr", &setup.stderr)?;
    if !setup.status.success() {
        return Err(Fail::die(format!(
            "guest input verification exited {}",
            setup.status
        )));
    }
    let os = vm
        .exec_complete("/usr/bin/sw_vers; /usr/bin/uname -m", mac::STEP)
        .map_err(Fail::die)?;
    save(&ask.record, "guest-os.txt", &os.stdout)?;
    if !os.status.success() {
        return Err(Fail::die("guest OS identity unavailable"));
    }
    let census = vm
        .exec_complete("/bin/ps -A -o ucomm=", mac::STEP)
        .map_err(Fail::die)?;
    save(&ask.record, "consumer-census.before", &census.stdout)?;
    if !census.status.success()
        || String::from_utf8_lossy(&census.stdout)
            .lines()
            .any(|name| name.to_ascii_lowercase().contains("codex"))
    {
        return Err(Fail::die(
            "guest has an unavailable or non-empty Codex consumer census",
        ));
    }
    let command = format!(
        "umask 077; cd {ROOT}; ulimit -n 65536; export TMPDIR=/tmp; \
         ./aterm-lab codex-source-proof --aterm {ROOT}/native-host --package {ROOT}/candidate --keep \
         > {ROOT}/stdout 2> {ROOT}/stderr; code=$?; printf '%s\\n' \"$code\" > {ROOT}/exit"
    );
    let ran = vm.exec_complete(&command, COMPONENT);
    let ended = ran.as_ref().is_ok_and(|out| out.status.success());
    let (stdout, text) = capture(
        vm,
        &ask.record,
        "stdout",
        &format!("/bin/cat {ROOT}/stdout"),
    );
    let (stderr, _) = capture(
        vm,
        &ask.record,
        "stderr",
        &format!("/bin/cat {ROOT}/stderr"),
    );
    let (exit, code) = capture(vm, &ask.record, "exit", &format!("/bin/cat {ROOT}/exit"));
    let code = code
        .as_deref()
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
        .and_then(|text| text.trim().parse::<i32>().ok());
    let world = text.as_deref().and_then(world_from_output);
    let host_log = if let Some(world) = &world {
        let canonical = world
            .strip_prefix("/tmp/")
            .map_or_else(|| world.clone(), |tail| format!("/private/tmp/{tail}"));
        let command = format!(
            "set -eu; test ! -L {world}; test -d {world}; \
             test \"$(cd {world} && /bin/pwd -P)\" = {canonical}; \
             test \"$(/usr/bin/stat -f %u {world})\" = \"$(id -u)\"; \
             test \"$(/usr/bin/stat -f %Lp {world})\" = 700; \
             test ! -L {world}/source-host.log; /bin/cat {world}/source-host.log",
            world = q(world),
            canonical = q(&canonical),
        );
        capture(vm, &ask.record, "source-host.log", &command).0
    } else {
        unavailable("no unique private source world in complete driver output")
    };
    let complete = [&stdout, &stderr, &exit, &host_log]
        .iter()
        .all(|entry| entry["complete"].as_bool() == Some(true));
    json_file(
        &ask.record,
        "capture.json",
        &json!({"stdout":stdout,"stderr":stderr,
        "exit":exit,"source_host_log":host_log,"world":world,"complete":complete,
        "capture_attempted_before_vm_drop":true,"credential_documents_exported":false,
        "log_finality_verified":false,"child_settlement_inferred_from_capture":false}),
    )?;
    json_file(
        &ask.record,
        "result.json",
        &json!({"component_exit":code,
        "command_ended":ended,"capture_complete":complete,"execution_error":ran.as_ref().err(),
        "qualification_only":true,"shipping_certified":false,"runtime_enabled":false,
        "automatic_update_proven":false,"work_preservation_proven":false}),
    )?;
    println!(
        "mac codex-source-proof: exit={code:?} capture_complete={complete}; evidence={}",
        ask.record.display()
    );
    Ok(if ended && code == Some(0) && complete {
        0
    } else {
        1
    })
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
    let scratch = aterm_tempfile::Builder::new()
        .prefix("mac-codex-")
        .suffix(".noindex")
        .tempdir()
        .map_err(fail)?;
    let share = scratch.path().join("share");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&share)
        .map_err(fail)?;
    let inputs = inputs(&ask, &share)?;
    json_file(&ask.record, "inputs.json", &inputs)?;
    let run = || -> Res {
        let tart = mac::tart_to_run()?;
        let base = mac::prepared_base(
            &tart,
            &ask.image,
            false,
            scratch.path(),
            &share,
            &mut || Ok(()),
        )?;
        let need = crate::vm_admit::Guest {
            lane: "mac-codex-source-proof",
            vm: "mac-codex-source-proof",
            memory: u64::from(MEMORY_MIB) << 20,
            cpus: CPUS,
        };
        let mut admitted = crate::vm_admit::admit(&need, true)?;
        let mut vm = Vm::clone_from(&tart, &base, &ask.record, &share, false)?;
        json_file(
            &ask.record,
            "vm.json",
            &json!({"name":vm.name,"prepared_base":base,
            "resource_claim_held_through_teardown":true,"tart_log":"tart-run.log"}),
        )?;
        let mut shape = aterm_uds::spawnfd::command(&tart);
        shape.args([
            "set",
            &vm.name,
            "--cpu",
            &CPUS.to_string(),
            "--memory",
            &MEMORY_MIB.to_string(),
            "--disk-size",
            &DISK_GB.to_string(),
        ]);
        let shaped = aterm_uds::deadline::output_within(&mut shape, mac::STEP).map_err(fail)?;
        if !shaped.status.success() {
            return Err(Fail::die(format!("tart set: {}", shaped.status)));
        }
        vm.graphics = false;
        mac::boot_sip_on(&mut vm, &base)?;
        if let Some(pid) = vm.pid() {
            admitted.credit("mac-codex-source-proof", &[pid]);
        }
        let result = guest(&vm, &ask, &inputs);
        drop(vm);
        drop(admitted);
        result
    };
    let result = run();
    if let Err(why) = &result {
        json_file(
            &ask.record,
            "error.json",
            &json!({"code":why.code,"message":why.msg,
            "signal":why.stopped,"qualification_only":true,"runtime_enabled":false}),
        )?;
    }
    result
}

#[cfg(test)]
#[path = "mac_codex_source_proof_tests.rs"]
mod tests;
