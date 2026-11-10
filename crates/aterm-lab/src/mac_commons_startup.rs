// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Run the existing guarded ordinary-startup component wholly inside a disposable macOS guest.
//! The borrowed app never runs on the host; transfer launches no additional host lab process.
//! Evidence capture finishes or records its
//! unavailability before Vm's stop/delete teardown; it is no child-settlement proof.

use std::io::Write as _;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};

use crate::mac::{self, Vm};
use crate::util::shell_quote as q;
use crate::{Fail, Res, stop};
use aterm_json::{Value, json};

const USAGE: &str = "mac commons-startup --aterm FILE --record NEW_DIR [--image REF]\n\
  Copy the explicit debug executable and this lab into one disposable SIP-on guest and run\n\
  `commons-startup FILE NEW_OUT --log-backed` there, with its existing private homes and guarded\n\
  loopback attachment. No app runs on the host, no guest build or system install, no PF change.\n\
  Fixed guest: 4 vCPUs, 8 GiB RAM, 60 GB disk, no Tart window. The component opens its GUI inside\n\
  the guest. NEW_DIR retains hashes, stdout/stderr/exit, output and all kept profiles, including\n\
  failures. Each capture is complete up to 16 MiB or explicitly unavailable. Exit follows the\n\
  component; capture completeness is recorded separately. This is synthetic-prior-approval\n\
  component evidence, never a production LAN or default-fleet qualification.\n";
const ROOT: &str = "/tmp/.ubd/aterm-commons-startup.noindex";
const CPUS: u32 = 4;
const MEMORY_MIB: u32 = 8192;
const DISK_GB: u32 = 60;
/// Outer transport/containment ceiling for the whole multi-stage component, whose individual
/// startup, repair and cleanup waits have their own hang detectors. This is no latency budget.
const COMPONENT: std::time::Duration = std::time::Duration::from_secs(3600);

struct Ask {
    aterm: PathBuf,
    record: PathBuf,
    image: String,
}

fn parse(args: &[String]) -> Result<Ask, Fail> {
    let (mut aterm, mut record) = (None, None);
    let mut image = std::env::var("LAB_MAC_IMAGE").unwrap_or_else(|_| mac::IMAGE.into());
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
            "--record" if record.is_none() => {
                record = Some(PathBuf::from(crate::paths::from_caller(value)))
            }
            "--image" => image.clone_from(value),
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
        record: record.ok_or_else(|| Fail::usage(USAGE))?,
        image,
    })
}

fn fail(error: impl std::fmt::Display) -> Fail {
    Fail::die(error.to_string())
}

/// Diagnostic files are published without clobbering; a failed write remains labelled partial.
fn save(record: &Path, name: &str, bytes: &[u8]) -> Result<(), Fail> {
    let partial = record.join(format!("{name}.partial"));
    let mut file = crate::util::create_new_regular_file(&partial, 0o600).map_err(fail)?;
    file.write_all(bytes).map_err(fail)?;
    drop(file);
    std::fs::hard_link(&partial, record.join(name)).map_err(fail)?;
    std::fs::remove_file(partial).map_err(fail)
}

fn json_file(record: &Path, name: &str, value: &Value) -> Result<(), Fail> {
    save(record, name, &aterm_json::to_vec(value).map_err(fail)?)
}

fn copy_image(source: &Path, share: &Path, name: &str) -> Result<Value, Fail> {
    let source = source.canonicalize().map_err(fail)?;
    if !std::fs::metadata(&source).map_err(fail)?.is_file() {
        return Err(Fail::usage(format!(
            "{} is not an executable file",
            source.display()
        )));
    }
    let mut input = std::fs::File::open(&source).map_err(fail)?;
    let dest = share.join(name);
    let mut output = crate::util::create_new_regular_file(&dest, 0o700).map_err(fail)?;
    std::io::copy(&mut input, &mut output).map_err(fail)?;
    drop(output);
    Ok(json!({"borrowed_path":source.display().to_string(),
        "bytes":std::fs::metadata(&dest).map_err(fail)?.len(),
        "sha256":crate::windows::sha256_file(&dest)?,"additional_host_launch":false}))
}

fn unavailable(why: impl std::fmt::Display) -> Value {
    json!({"complete":false,"unavailable":why.to_string()})
}

fn capture(vm: &Vm, record: &Path, name: &str, command: &str) -> (Value, Option<Vec<u8>>) {
    match vm.exec_complete(command, mac::STEP) {
        Ok(out) if out.status.success() => match save(record, name, &out.stdout) {
            Ok(()) => (
                json!({"complete":true,"file":name,"bytes":out.stdout.len()}),
                Some(out.stdout),
            ),
            Err(why) => (
                json!({"complete":false,"unavailable":{
                    "code":why.code,"message":why.msg,"signal":why.stopped,
                }}),
                None,
            ),
        },
        Ok(out) => (
            unavailable(format!(
                "guest capture exited {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            )),
            None,
        ),
        Err(why) => (unavailable(why), None),
    }
}

/// Only the exact canonical temporary profiles reported by this component may be exported.
/// The guest additionally checks directory kind, ownership, mode and canonical identity.
fn profile(path: &str) -> bool {
    let path = Path::new(path);
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    matches!(
        path.parent().and_then(Path::to_str),
        Some("/private/tmp/.ubd" | "/tmp/.ubd")
    ) && name.starts_with("at")
        && name.ends_with(".noindex")
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

fn retain(vm: &Vm, record: &Path, command_ended: bool) -> (Value, Option<i32>) {
    let read = |name: &str| format!("/bin/cat {ROOT}/{name}");
    let (stdout, text) = capture(vm, record, "stdout", &read("stdout"));
    let (stderr, _) = capture(vm, record, "stderr", &read("stderr"));
    let (exit, code) = capture(vm, record, "exit", &read("exit"));
    let code = code
        .as_deref()
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
        .and_then(|text| text.trim().parse::<i32>().ok());
    let (out, _) = capture(
        vm,
        record,
        "output.tar",
        &format!("test ! -L {ROOT}/out && test -d {ROOT}/out && /usr/bin/tar -cf - -C {ROOT} out"),
    );
    let (before, binding) = capture(vm, record, "before.json", &read("out/before.json"));
    let binding = binding
        .as_deref()
        .and_then(|bytes| aterm_json::from_slice::<Value>(bytes).ok());
    let mut profiles = Vec::new();
    if let Some(text) = &text {
        for line in String::from_utf8_lossy(text).lines() {
            if let Some(path) = line.strip_prefix("commons-startup profile=")
                && !profiles.iter().any(|seen| seen == path)
            {
                profiles.push(path.to_owned());
            }
        }
    }
    let source = binding
        .as_ref()
        .and_then(|value| value["source_profile"].as_str());
    let recipient = binding
        .as_ref()
        .and_then(|value| value["recipient_profile"].as_str());
    let bound =
        matches!((source, recipient), (Some(source), Some(recipient)) if source != recipient);
    let roles = if bound {
        json!({"source":source,"recipient":recipient,
            "source_reported":profiles.iter().any(|path| Some(path.as_str()) == source),
            "recipient_reported":profiles.iter().any(|path| Some(path.as_str()) == recipient)})
    } else {
        unavailable("before.json does not supply distinct source and recipient profile bindings")
    };
    let restart = if bound
        && profiles.first().map(String::as_str) == source
        && profiles.get(1).map(String::as_str) == recipient
    {
        profiles.get(2).map_or_else(
            || unavailable("no distinct restarted launch profile was reported"),
            |path| json!({"reported_profile":path,"binding":"distinct third driver-reported launch directory"}),
        )
    } else {
        unavailable("the source/recipient reports do not match before.json")
    };
    let mut custody = Vec::new();
    for (index, path) in profiles.iter().enumerate() {
        let role = if bound && Some(path.as_str()) == source {
            "source"
        } else if bound && Some(path.as_str()) == recipient {
            "recipient"
        } else if bound
            && index == 2
            && profiles.first().map(String::as_str) == source
            && profiles.get(1).map(String::as_str) == recipient
        {
            "restarted_launch"
        } else {
            "additional_launch"
        };
        let evidence = if profile(path) {
            let command = format!(
                "set -eu; p={}; test ! -L \"$p\"; test -d \"$p\"; \
                     test \"$(cd \"$p\" && /bin/pwd -P)\" = \"$p\"; \
                     test \"$(/usr/bin/stat -f %u \"$p\")\" = \"$(id -u)\"; \
                     test \"$(/usr/bin/stat -f %Lp \"$p\")\" = 700; /usr/bin/tar -cf - -C \"$p\" .",
                q(path),
            );
            let (capture, _) = capture(vm, record, &format!("profile-{index}.tar"), &command);
            json!({"role":role,"profile":path,"capture":capture})
        } else {
            unavailable("the reported profile is outside the owned temporary-profile shape")
        };
        custody.push(evidence);
    }
    if profiles.is_empty() {
        custody.push(unavailable(
            "no kept profile paths were reported in complete stdout",
        ));
    }
    (
        json!({"complete_stdout_limit_bytes":aterm_uds::deadline::MAX_COMPLETE_STDOUT_BYTES,
        "command_ended":command_ended,
        "capture_scope":if command_ended {"after component command ended"} else {"snapshots of still-mutable guest files; component command is unsettled"},
        "log_finality_verified":false,
        "stdout":stdout,"stderr":stderr,"exit":exit,"output":out,"profiles":custody,
        "before":before,"profile_bindings":roles,"restarted_launch":restart,
        "profile_capture_atomic":false,"child_settlement":"not inferred from archival; consult the component's explicit cleanup receipts"}),
        code,
    )
}

fn guest(vm: &Vm, ask: &Ask, inputs: &Value) -> Res {
    let setup = format!(
        "set -eu; umask 077; mkdir -p /tmp/.ubd; test ! -L /tmp/.ubd; \
         test \"$(cd /tmp/.ubd && /bin/pwd -P)\" = /private/tmp/.ubd; mkdir {ROOT}; \
         test \"$(/usr/bin/stat -f %u {ROOT})\" = \"$(id -u)\"; \
         test \"$(/usr/bin/stat -f %Lp {ROOT})\" = 700; \
         cp {share}/aterm {ROOT}/aterm; cp {share}/aterm-lab {ROOT}/aterm-lab; chmod 700 {ROOT}/aterm {ROOT}/aterm-lab; \
         test \"$(/usr/bin/shasum -a 256 {ROOT}/aterm | /usr/bin/cut -d ' ' -f 1)\" = {app}; \
         test \"$(/usr/bin/shasum -a 256 {ROOT}/aterm-lab | /usr/bin/cut -d ' ' -f 1)\" = {lab}",
        share = q(mac::GUEST_SHARE),
        app = q(inputs["aterm"]["sha256"].as_str().expect("hashed app")),
        lab = q(inputs["lab"]["sha256"].as_str().expect("hashed lab")),
    );
    let setup = vm.exec_complete(&setup, mac::STEP);
    if !setup.as_ref().is_ok_and(|out| out.status.success()) {
        let reason = match setup {
            Ok(out) => {
                save(&ask.record, "setup.stdout", &out.stdout)?;
                save(&ask.record, "setup.stderr", &out.stderr)?;
                format!("guest setup exited {}", out.status)
            }
            Err(why) => why,
        };
        let missing = unavailable("the component did not start");
        json_file(
            &ask.record,
            "capture.json",
            &json!({"setup":unavailable(&reason),
            "stdout":missing,"stderr":missing,"exit":missing,"output":missing,
            "profiles":[missing,missing]}),
        )?;
        return Err(Fail::die(reason));
    }
    // The component itself supplies private roots and explicit unrelated-lane settings; no
    // release-shaped fresh-HOME application or unguarded Commons attachment runs here.
    let command = format!(
        "umask 077; cd {ROOT}; ./aterm-lab commons-startup {ROOT}/aterm {ROOT}/out --log-backed \
         > {ROOT}/stdout 2> {ROOT}/stderr; code=$?; printf '%s\\n' \"$code\" > {ROOT}/exit"
    );
    let ran = vm.exec_complete(&command, COMPONENT);
    let ended = ran.as_ref().is_ok_and(|out| out.status.success());
    // Do this even when the command times out or fails, while this owner still holds the VM.
    let (captures, code) = retain(vm, &ask.record, ended);
    json_file(&ask.record, "capture.json", &captures)?;
    let result = json!({"component_exit":code,"command_ended":ended,
        "execution_error":ran.as_ref().err(),"loopback_component_only":true,
        "default_fleet_or_physical_lan_measured":false,"capture_attempted_before_vm_drop":true,
        "child_settlement_verified":false});
    json_file(&ask.record, "result.json", &result)?;
    println!(
        "mac commons-startup: exit={code:?}; evidence={}",
        ask.record.display()
    );
    Ok(if ended && code == Some(0) { 0 } else { 1 })
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
        .prefix("mac-commons-")
        .suffix(".noindex")
        .tempdir()
        .map_err(fail)?;
    let share = scratch.path().join("share");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&share)
        .map_err(fail)?;
    let inputs = json!({"aterm":copy_image(&ask.aterm,&share,"aterm")?,
        "lab":copy_image(&std::env::current_exe().map_err(fail)?,&share,"aterm-lab")?,
        "image":ask.image,"guest":{"cpus":CPUS,"memory_mib":MEMORY_MIB,"disk_gb":DISK_GB,"tart_graphics":false},
        "component_transport_ceiling_seconds":COMPONENT.as_secs(),
        "guest_compilation":false,"host_app_execution":false,"pf_changes":false});
    json_file(&ask.record, "inputs.json", &inputs)?;
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
        lane: "mac-commons-startup",
        vm: "mac-commons-startup",
        memory: u64::from(MEMORY_MIB) << 20,
        cpus: CPUS,
    };
    let mut admitted = crate::vm_admit::admit(&need, true)?;
    let mut vm = Vm::clone_from(&tart, &base, &ask.record, &share, false)?;
    json_file(
        &ask.record,
        "vm.json",
        &json!({"name":vm.name,"prepared_base":base,
        "tart_log":"tart-run.log","resource_claim_held_through_teardown":true}),
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
        save(&ask.record, "shape.stdout", &shaped.stdout)?;
        save(&ask.record, "shape.stderr", &shaped.stderr)?;
        return Err(Fail::die(format!("tart set: {}", shaped.status)));
    }
    vm.graphics = false;
    mac::boot_sip_on(&mut vm, &base)?;
    if let Some(pid) = vm.pid() {
        admitted.credit("mac-commons-startup", &[pid]);
    }
    let result = guest(&vm, &ask, &inputs);
    drop(vm); // Stop/delete this clone while its resource admission remains held.
    drop(admitted);
    result
}
