// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Dev-only SD-72 workload: the kernel must identify this process's machine as a VM first.

#[path = "../self_cpu_qualification.rs"]
mod qualification;

use aterm_uds::spawnfd::{Child, Stdio};
use std::io::{Read as _, Write as _};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const HANG: Duration = Duration::from_secs(60);

struct Worker {
    start: Option<Sender<()>>,
    release: Option<Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        drop(self.start.take());
        drop(self.release.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn receive<T>(channel: &Receiver<T>, what: &str) -> Result<T, String> {
    channel
        .recv_timeout(HANG)
        .map_err(|error| format!("{what}: {error}"))
}

fn qualify_child() -> Result<qualification::ChildProof, String> {
    // This precedes worker creation, every reader call and every instruction of CPU load.
    qualification::require_guest(aterm_sysprobe::hv_vmm_present()).map_err(str::to_owned)?;
    let (ready_tx, ready_rx) = mpsc::channel();
    let (start_tx, start_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let thread = std::thread::Builder::new()
        .name(qualification::WORKER.into())
        .spawn(move || {
            if ready_tx.send(()).is_err() || start_rx.recv().is_err() {
                return;
            }
            let started = Instant::now();
            let mut n = 0_u64;
            let mut word = 1_u64;
            while started.elapsed() < Duration::from_nanos(qualification::WORK_NS) {
                word = std::hint::black_box(
                    word.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1),
                );
                n = n.saturating_add(1);
            }
            let wall_ns = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
            let _ = done_tx.send((wall_ns, n));
            // Keep the same native thread alive and named through the second reader cut.
            let _ = release_rx.recv();
        })
        .map_err(|error| error.to_string())?;
    let mut worker = Worker {
        start: Some(start_tx),
        release: Some(release_tx),
        thread: Some(thread),
    };
    receive(&ready_rx, "worker readiness")?;
    let epoch = Instant::now();
    let before =
        qualification::Cut::from(aterm_sysprobe::self_cpu::read(epoch).map_err(|e| e.to_string())?);
    worker
        .start
        .take()
        .ok_or("worker start disappeared")?
        .send(())
        .map_err(|e| e.to_string())?;
    let (work_wall_ns, work_iterations) = receive(&done_rx, "worker completion")?;
    let after =
        qualification::Cut::from(aterm_sysprobe::self_cpu::read(epoch).map_err(|e| e.to_string())?);
    let proof = qualification::ChildProof {
        schema: 2,
        kernel_vm: true,
        worker: qualification::WORKER.into(),
        work_wall_ns,
        work_iterations,
        before,
        after,
    };
    drop(worker.release.take());
    worker
        .thread
        .take()
        .ok_or("worker thread disappeared")?
        .join()
        .map_err(|_| "worker panicked")?;
    Ok(proof)
}

struct OwnedChild(Option<Child>);

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            if !child.is_reaped() {
                let _ = child.kill();
                let _ = child.wait_with_rusage();
            }
        }
    }
}

fn private_output(path: &Path) -> Result<std::fs::File, String> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(path).map_err(|e| e.to_string())
}

fn complete_file(path: &Path) -> Result<Vec<u8>, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() > qualification::MAX_CHILD_OUTPUT {
        return Err("guest-child output exceeded its complete-file bound".into());
    }
    let mut bytes = Vec::new();
    file.take(qualification::MAX_CHILD_OUTPUT + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 != metadata.len() {
        return Err("guest-child output changed after exit".into());
    }
    Ok(bytes)
}

#[cfg(target_os = "macos")]
fn kernel_timebase() -> Option<(u32, u32)> {
    aterm_sysprobe::procs::kernel_timebase()
}
#[cfg(not(target_os = "macos"))]
fn kernel_timebase() -> Option<(u32, u32)> {
    None
}

fn supervise() -> Result<qualification::Proof, String> {
    qualification::require_guest(aterm_sysprobe::hv_vmm_present()).map_err(str::to_owned)?;
    let (numer, denom) = kernel_timebase().ok_or("kernel_timebase_unavailable")?;
    let rounding =
        qualification::terminal_rounding(numer, denom).ok_or("kernel_timebase_unqualified")?;
    let scratch = aterm_tempfile::Builder::new()
        .prefix("self-cpu-child-")
        .suffix(".noindex")
        .tempdir()
        .map_err(|e| e.to_string())?;
    let stdout = scratch.path().join("child.stdout");
    let stderr = scratch.path().join("child.stderr");
    let mut child = OwnedChild(Some(
        aterm_uds::spawnfd::command(std::env::current_exe().map_err(|e| e.to_string())?)
            .arg("--child")
            .stdin(Stdio::null())
            .stdout(Stdio::from(private_output(&stdout)?))
            .stderr(Stdio::from(private_output(&stderr)?))
            .spawn()
            .map_err(|e| e.to_string())?,
    ));
    let native = child.0.as_mut().ok_or("guest-child custody missing")?;
    let pid = native.id();
    let usage = native.wait_with_rusage().map_err(|e| e.to_string())?;
    let out = complete_file(&stdout);
    let err = complete_file(&stderr);
    let complete = out.is_ok() && err.is_ok();
    let capture_error = out.as_ref().err().or(err.as_ref().err()).cloned();
    let terminal = qualification::TerminalOracle {
        source: qualification::TERMINAL_SOURCE.into(),
        pid,
        exit_code: usage.status.code(),
        user_ns: u64::try_from(usage.user.as_nanos()).map_err(|_| "exit user CPU overflow")?,
        system_ns: u64::try_from(usage.system.as_nanos())
            .map_err(|_| "exit system CPU overflow")?,
        timebase_numer: numer,
        timebase_denom: denom,
        rounding_upper_ns: rounding,
    };
    let mut proof = qualification::Proof {
        schema: 2,
        kernel_vm: true,
        terminal: Some(terminal),
        child_output_complete: complete,
        child_stdout: out.unwrap_or_default(),
        child_stderr: err.unwrap_or_default(),
        bound: None,
        qualification_error: capture_error,
    };
    match qualification::exit_accounting(&proof) {
        Ok(bound) => proof.bound = Some(bound),
        Err(why) => {
            if proof.qualification_error.is_none() {
                proof.qualification_error = Some(why);
            }
        }
    }
    Ok(proof)
}

fn emit(value: &impl serde::Serialize, limit: u64) -> Result<(), String> {
    let bytes = aterm_json::to_vec(value).map_err(|e| e.to_string())?;
    if bytes.len() as u64 >= limit {
        return Err("guest protocol exceeded its output bound".into());
    }
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(&bytes)
        .and_then(|()| stdout.write_all(b"\n"))
        .map_err(|e| e.to_string())
}

fn run() -> Result<bool, String> {
    // Both roles refuse physical/unknown hosts before process or worker creation.
    qualification::require_guest(aterm_sysprobe::hv_vmm_present()).map_err(str::to_owned)?;
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args == ["--child"] {
        emit(&qualify_child()?, qualification::MAX_CHILD_OUTPUT)?;
        Ok(true)
    } else if args.is_empty() {
        let proof = supervise()?;
        emit(&proof, 16 * qualification::MAX_CHILD_OUTPUT)?;
        Ok(qualification::validate(&proof).is_ok())
    } else {
        Err("unknown guest fixture mode".into())
    }
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(true) => std::process::ExitCode::SUCCESS,
        Ok(false) => std::process::ExitCode::FAILURE,
        Err(why) => {
            eprintln!("self CPU fixture NOT RUN or failed: {why}");
            std::process::ExitCode::from(
                if matches!(
                    why.as_str(),
                    "physical_host_refused"
                        | "kernel_vm_evidence_unavailable"
                        | "kernel_timebase_unavailable"
                        | "kernel_timebase_unqualified"
                ) {
                    3
                } else {
                    1
                },
            )
        }
    }
}
