// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fixed, non-powering observations of an explicitly held guest. No arbitrary guest commands.

use super::{Fail, HeldVm, Layout, Res, Ssh, guest, guest_identity_script, not_run, read_port};
use std::os::unix::process::ExitStatusExt as _;
use std::time::{Duration, Instant};

const WITHIN: Duration = Duration::from_secs(60);
const OUTPUT_CAP: usize = 128 * 1024;
const ROW_CAP: usize = 256;
const NAME_CAP: usize = 256;

#[derive(Clone, Copy)]
enum Phase {
    IdentityBefore,
    Snapshot,
    IdentityAfter,
}

enum CaptureFailure {
    Io(std::io::ErrorKind),
    Exit(std::process::ExitStatus),
    Stderr,
    Encoding,
    Identity,
    Schema,
}

fn write_failure(
    dir: &std::path::Path,
    summary: &str,
    raw: Option<(&[u8], &[u8])>,
) -> std::io::Result<()> {
    let write = |name, bytes: &[u8]| {
        if aterm_uds::private_file::write_private_new(dir, name, bytes, |_| Ok(()))? {
            Ok(())
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "failure artifact exists",
            ))
        }
    };
    if let Some((stdout, stderr)) = raw {
        if stdout.len() > OUTPUT_CAP || stderr.len() > OUTPUT_CAP {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "capture exceeds per-stream bound",
            ));
        }
        write("stdout.bin", stdout)?;
        write("stderr.bin", stderr)?;
    }
    write(
        "metadata.txt",
        format!(
            "{summary}\nraw_streams={}\npartial_transport_capture=unavailable\n",
            if raw.is_some() {
                "completed"
            } else {
                "unavailable"
            }
        )
        .as_bytes(),
    )
}

fn retain_failure(summary: &str, raw: Option<(&[u8], &[u8])>) -> String {
    let made = aterm_tempfile::Builder::new()
        .prefix("windows-observe-refusal-")
        .suffix(".noindex")
        .tempdir();
    let dir = match made {
        Ok(dir) => dir,
        Err(error) => return format!("evidence=unavailable evidence_io_kind={:?}", error.kind()),
    };
    // This path exists only for a refusal. Keep even partial evidence if a later write or
    // durability operation fails; a guard's Drop must not erase the diagnostic.
    let path = dir.keep();
    let written = (|| {
        crate::util::set_dir_mode_nofollow(&path, 0o700)?;
        aterm_uds::private_file::check_private_dir(&path)?;
        write_failure(&path, summary, raw)?;
        aterm_durable::Flush::Durable.dir(
            path.parent()
                .ok_or_else(|| std::io::Error::other("failure artifact has no parent"))?,
        )
    })();
    match written {
        Ok(()) => format!("evidence={path:?} evidence_complete=true"),
        Err(error) => format!(
            "evidence={path:?} evidence_complete=false evidence_io_kind={:?}",
            error.kind()
        ),
    }
}

fn refused(
    phase: Phase,
    cause: CaptureFailure,
    bytes: Option<(usize, usize)>,
    raw: Option<(&[u8], &[u8])>,
) -> Fail {
    let phase = match phase {
        Phase::IdentityBefore => "identity-before",
        Phase::Snapshot => "snapshot",
        Phase::IdentityAfter => "identity-after",
    };
    let cause = match cause {
        CaptureFailure::Io(kind) => format!("io_kind={kind:?}"),
        CaptureFailure::Exit(status) => {
            format!("ssh_exit={:?} signal={:?}", status.code(), status.signal())
        }
        CaptureFailure::Stderr => "ssh_exit=0 unexpected_stderr=true".into(),
        CaptureFailure::Encoding => "ssh_exit=0 invalid_utf8=true".into(),
        CaptureFailure::Identity => "ssh_exit=0 identity_mismatch=true".into(),
        CaptureFailure::Schema => "ssh_exit=0 invalid_schema=true".into(),
    };
    let counts = bytes.map_or_else(
        || "stdout_bytes=unknown stderr_bytes=unknown".into(),
        |(out, err)| format!("stdout_bytes={out} stderr_bytes={err}"),
    );
    let summary = format!("held observation phase={phase} {cause} {counts}");
    let evidence = retain_failure(&summary, raw);
    not_run(format!("{summary} {evidence}; held VM state preserved"))
}

// CIM returns cumulative counters, not rates. Select properties at the provider, and emit
// only this fixed schema. Neither exception text nor a process command line leaves the guest.
const QUERY: &str = r"$ErrorActionPreference='Stop'; $ProgressPreference='SilentlyContinue'; $phase='birth'; try {
function Counter($p, $k) { $v=$p.$k; if ($null -eq $v) { '-' } else { ([UInt64]$v).ToString([Globalization.CultureInfo]::InvariantCulture) } }
$start=[DateTime]::UtcNow.ToFileTimeUtc();
$phase='cim';
$rows=@(Get-CimInstance -ClassName Win32_Process -Property Name,ProcessId,CreationDate,ParentProcessId,KernelModeTime,UserModeTime,ReadTransferCount,WriteTransferCount -OperationTimeoutSec 20 | Sort-Object ProcessId);
$phase='count';
if ($rows.Count -gt 256) { [Console]::Out.WriteLine(('observation-failure version=1 phase=count rows={0} row_cap=256' -f $rows.Count)); exit 125 };
$lines=@(); foreach ($p in $rows) {
 $phase='name';
 $bytes=[Text.Encoding]::UTF8.GetBytes([string]$p.Name); if ($bytes.Length -eq 0 -or $bytes.Length -gt 256) { [Console]::Out.WriteLine(('observation-failure version=1 phase=name name_bytes={0} name_cap=256' -f $bytes.Length)); exit 125 };
 $name=[BitConverter]::ToString($bytes).Replace('-','').ToLowerInvariant();
 $phase='birth';
 $birth=if ($null -eq $p.CreationDate) { '-' } else { $p.CreationDate.ToUniversalTime().ToFileTimeUtc().ToString([Globalization.CultureInfo]::InvariantCulture) };
 $phase='counters';
 $lines+=('process name_hex={0} pid={1} birth={2} parent={3} kernel={4} user={5} read={6} write={7}' -f $name,(Counter $p 'ProcessId'),$birth,(Counter $p 'ParentProcessId'),(Counter $p 'KernelModeTime'),(Counter $p 'UserModeTime'),(Counter $p 'ReadTransferCount'),(Counter $p 'WriteTransferCount'));
};
$phase='birth';
$end=[DateTime]::UtcNow.ToFileTimeUtc();
'snapshot version=1 start={0} end={1} rows={2} observer={3}' -f $start,$end,$rows.Count,$PID;
$lines;
} catch {
 [Console]::Out.WriteLine(('observation-failure version=1 phase={0} hresult={1} category={2}' -f $phase,([Int32]$_.Exception.HResult).ToString([Globalization.CultureInfo]::InvariantCulture),([Int32]$_.CategoryInfo.Category).ToString([Globalization.CultureInfo]::InvariantCulture)));
 exit 125
}";

#[derive(Debug, PartialEq, Eq)]
struct Process {
    name: String,
    pid: u32,
    birth: Option<u64>,
    parent: Option<u32>,
    kernel: Option<u64>,
    user: Option<u64>,
    read: Option<u64>,
    write: Option<u64>,
}

#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    start: u64,
    end: u64,
    observer: u32,
    processes: Vec<Process>,
}

fn words<'a>(line: &'a str, prefix: &str, keys: &[&str]) -> Result<Vec<&'a str>, String> {
    let mut words = line.split(' ');
    if words.next() != Some(prefix) {
        return Err("unexpected observation record".into());
    }
    let values = keys
        .iter()
        .map(|key| {
            let word = words.next().ok_or("missing observation field")?;
            word.strip_prefix(*key)
                .and_then(|tail| tail.strip_prefix('='))
                .filter(|value| !value.is_empty())
                .ok_or("unexpected observation field")
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(str::to_string)?;
    if words.next().is_some() {
        return Err("extra observation field".into());
    }
    Ok(values)
}

fn number(value: &str) -> Result<u64, String> {
    if !value.bytes().all(|byte| byte.is_ascii_digit())
        || (value.len() > 1 && value.starts_with('0'))
    {
        return Err("invalid observation integer".into());
    }
    value
        .parse()
        .map_err(|_| "observation integer overflow".into())
}

fn optional(value: &str) -> Result<Option<u64>, String> {
    if value == "-" {
        Ok(None)
    } else {
        number(value).map(Some)
    }
}

fn name(value: &str) -> Result<String, String> {
    if value.is_empty()
        || value.len() > NAME_CAP * 2
        || !value.len().is_multiple_of(2)
        || !aterm_codec::hex::is_lower(value, value.len())
    {
        return Err("invalid observation name encoding".into());
    }
    let bytes = aterm_codec::hex::decode_vec(value.as_bytes())
        .ok_or("invalid observation name encoding")?;
    let name = String::from_utf8(bytes).map_err(|_| "observation name is not UTF-8")?;
    if name
        .chars()
        .any(|ch| ch.is_control() || matches!(ch, '/' | '\\'))
    {
        return Err("observation name is not a process basename".into());
    }
    Ok(name)
}

fn parse(text: &str) -> Result<Snapshot, String> {
    if text.len() > OUTPUT_CAP || !text.ends_with('\n') {
        return Err("incomplete or oversized observation".into());
    }
    let mut lines = text.lines();
    let header = words(
        lines.next().ok_or("missing observation header")?,
        "snapshot",
        &["version", "start", "end", "rows", "observer"],
    )?;
    if header[0] != "1" {
        return Err("unsupported observation version".into());
    }
    let start = number(header[1])?;
    let end = number(header[2])?;
    let count = usize::try_from(number(header[3])?).map_err(|_| "observation count overflow")?;
    let observer = u32::try_from(number(header[4])?).map_err(|_| "observer ID overflow")?;
    if start == 0 || end < start || count > ROW_CAP {
        return Err("invalid observation window or count".into());
    }
    let mut processes = Vec::new();
    for line in lines {
        if processes.len() >= count {
            return Err("extra observation row".into());
        }
        let values = words(
            line,
            "process",
            &[
                "name_hex", "pid", "birth", "parent", "kernel", "user", "read", "write",
            ],
        )?;
        let pid = u32::try_from(number(values[1])?).map_err(|_| "process ID overflow")?;
        if processes.iter().any(|process: &Process| process.pid == pid) {
            return Err("duplicate observation process".into());
        }
        let birth = optional(values[2])?;
        if birth.is_some_and(|birth| birth == 0 || birth > end) {
            return Err("invalid process birth".into());
        }
        processes.push(Process {
            name: name(values[0])?,
            pid,
            birth,
            parent: optional(values[3])?
                .map(u32::try_from)
                .transpose()
                .map_err(|_| "parent ID overflow")?,
            kernel: optional(values[4])?,
            user: optional(values[5])?,
            read: optional(values[6])?,
            write: optional(values[7])?,
        });
    }
    if processes.len() != count {
        return Err("missing observation rows".into());
    }
    if observer == 0 || !processes.iter().any(|process| process.pid == observer) {
        return Err("missing observation process identity".into());
    }
    Ok(Snapshot {
        start,
        end,
        observer,
        processes,
    })
}

fn validate_args(args: &[String]) -> Result<(), Fail> {
    if args.is_empty() {
        Ok(())
    } else {
        Err(Fail::usage(
            "windows attach <custody> observe takes no arguments",
        ))
    }
}

/// Strict existing-host trust only. Unlike the mutative lane, observation cannot enroll a key.
fn capture(ssh: &Ssh, script: &str, phase: Phase, deadline: Instant) -> Result<String, Fail> {
    if Instant::now() >= deadline {
        return Err(refused(
            phase,
            CaptureFailure::Io(std::io::ErrorKind::TimedOut),
            None,
            None,
        ));
    }
    let mut options = ssh.options_for(true);
    options.extend([
        "-p".into(),
        ssh.port.to_string(),
        format!("{}@127.0.0.1", ssh.user),
        guest::powershell_command(script),
    ]);
    let mut command = aterm_uds::spawnfd::command("/usr/bin/ssh");
    command.args(options);
    let output = aterm_uds::deadline::output_with_input_until(
        &mut command,
        aterm_uds::secret::Secret::new(&[]),
        deadline,
        OUTPUT_CAP,
    )
    .map_err(|error| refused(phase, CaptureFailure::Io(error.kind()), None, None))?;
    let bytes = Some((output.stdout.len(), output.stderr.len()));
    let raw = Some((output.stdout.as_slice(), output.stderr.as_slice()));
    if !output.status.success() {
        return Err(refused(
            phase,
            CaptureFailure::Exit(output.status),
            bytes,
            raw,
        ));
    }
    if !output.stderr.is_empty() {
        return Err(refused(phase, CaptureFailure::Stderr, bytes, raw));
    }
    std::str::from_utf8(&output.stdout)
        .map(str::to_owned)
        .map_err(|_| refused(phase, CaptureFailure::Encoding, bytes, raw))
}

fn identity(ssh: &Ssh, held: &HeldVm, phase: Phase, deadline: Instant) -> Result<(), Fail> {
    let text = capture(
        ssh,
        &guest_identity_script(&held.plan, false),
        phase,
        deadline,
    )?;
    if text.trim() == "lane-identity-ok" {
        Ok(())
    } else {
        Err(refused(
            phase,
            CaptureFailure::Identity,
            Some((text.len(), 0)),
            Some((text.as_bytes(), b"")),
        ))
    }
}

pub(super) fn run(held: &HeldVm, args: &[String]) -> Res {
    validate_args(args)?;
    let deadline = Instant::now() + WITHIN;
    let layout = Layout::find()?;
    held.check(&layout)?;
    let port = read_port(&layout)
        .ok_or_else(|| not_run("held observation has no recorded SSH port; preserved"))?;
    if !layout.key().is_file() || !layout.known_hosts().is_file() {
        return Err(not_run(
            "held observation requires existing SSH identity and host trust; preserved",
        ));
    }
    let ssh = Ssh::new(&layout, port).runner();
    identity(&ssh, held, Phase::IdentityBefore, deadline)?;
    held.check(&layout)?;
    let text = capture(&ssh, QUERY, Phase::Snapshot, deadline)?;
    let snapshot = parse(&text).map_err(|_| {
        refused(
            Phase::Snapshot,
            CaptureFailure::Schema,
            Some((text.len(), 0)),
            Some((text.as_bytes(), b"")),
        )
    })?;
    held.check(&layout)?;
    identity(&ssh, held, Phase::IdentityAfter, deadline)?;
    held.check(&layout)?;
    if Instant::now() >= deadline {
        return Err(not_run("held observation deadline ended; preserved"));
    }
    println!(
        "windows-observe scope=vm_unattributed_process_facts provider=Win32_Process cpu_unit=100ns io_unit=bytes birth_and_window_unit=windows_filetime_100ns unknown=- non_atomic=true current_job_attribution=unknown health_verdict=none"
    );
    println!(
        "snapshot version=1 start={} end={} rows={} observer={}",
        snapshot.start,
        snapshot.end,
        snapshot.processes.len(),
        snapshot.observer
    );
    for p in snapshot.processes {
        let value = |value: Option<u64>| value.map_or_else(|| "-".into(), |v| v.to_string());
        println!(
            "process name={:?} pid={} birth={} parent={} kernel={} user={} read={} write={}",
            p.name,
            p.pid,
            value(p.birth),
            value(p.parent.map(u64::from)),
            value(p.kernel),
            value(p.user),
            value(p.read),
            value(p.write)
        );
    }
    Ok(0)
}

#[cfg(test)]
#[path = "windows_observe_tests.rs"]
mod tests;
