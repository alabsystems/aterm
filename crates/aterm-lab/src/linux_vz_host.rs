// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The vz lane's helper in the lab (docs/DESIGN-vz-guest-2026-10-23.md §5.1, slice V2):
//! [`build_host`] builds `aterm-vz-host` with the `targo` that built the lab, and
//! [`place_host`] copies it to `vz.noindex/host/<sha256>/` and signs the copy there ad hoc
//! with `crates/aterm-vz/aterm-vz-host.entitlements` (`com.apple.security.virtualization`
//! only), never in place in `target/`, then reclaims the copies no helper needs.
//!
//! `linux vz-host up|exec|status|down` drive one VM through the helper, for V2's measures:
//! V3's layers make the disk (a bake's, with its seed, where none is baked), the kernel and
//! the seed, V1's agent is the boot share, and [`aterm_vz::client::Lab`] opens the session
//! on the helper it finds or starts detached. `up` holds `run/bring-up.lock` (V2's stand-in
//! for V4's lane lock), opens its session first, and prepares the disks only when that
//! session found no VM ([`aterm_vz::client::Session::bring_up`]), so no disk is unlinked or
//! replaced under a running VM. `exec` joins the live helper and never starts, stops or
//! replaces one. V4 makes this the lane's driver.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Instant;

use aterm_uds::spawnfd::Stdio;
use aterm_vz::client::{ClientError, Detached, Lab, UpError, failed_words, not_run_words};
use aterm_vz::layers::{Base, Layers, Prepared, System, VM};
use aterm_vz::plan::{COMMAND_LINE, CPUS, Disk, MEMORY, Plan, ShareEntry, mac_for};
use aterm_vz::proto::{Attach, PROTO};
use aterm_vz::record::{DiskLock, Holder, Kernel, Paths, Record};

use crate::linux::{NOT_RUN, not_run};
use crate::pyjson::J;
use crate::{Fail, LAB, Res};

/// The helper's package and executable.
pub const HOST: &str = aterm_vz::sign::EXE;

/// The helper's own target directory in the checkout at `repo`, apart from targo's.
#[must_use]
pub fn target_dir(repo: &Path) -> PathBuf {
    repo.join("target").join("vz-host.noindex")
}

/// `targo`'s arguments for the helper's build in the checkout at `repo`: the dev profile,
/// whose debug assertions check every hand-written send's prototype against the
/// Objective-C runtime (`checked_msg`) at each boot.
#[must_use]
pub fn host_build_args(repo: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "--unverified",
        "build",
        "--locked",
        "--offline",
        "-p",
        "aterm-vz",
        "--bin",
        HOST,
        "--message-format",
        "json-render-diagnostics",
        "--manifest-path",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    args.push(repo.join("Cargo.toml").into_os_string());
    args.push("--target-dir".into());
    args.push(target_dir(repo).into_os_string());
    args
}

/// The helper's executable, as the build's artifact line names it.
#[must_use]
pub fn executable_of(json_lines: &str) -> Option<PathBuf> {
    json_lines.lines().find_map(|line| {
        let j = J::parse(line).ok()?;
        let target = j.get("target")?;
        let ours = j.get("reason")?.as_str() == Some("compiler-artifact")
            && target.get("name")?.as_str() == Some(HOST)
            && target
                .get("kind")?
                .as_arr()?
                .iter()
                .any(|k| k.as_str() == Some("bin"));
        if !ours {
            return None;
        }
        j.get("executable")?.as_str().map(PathBuf::from)
    })
}

/// Build the helper from the checkout at `repo` with the `targo` that built the lab.
///
/// # Errors
/// No `targo` is known, the build failed, or it named no executable.
pub fn build_host(repo: &Path) -> Result<PathBuf, Fail> {
    let targo = option_env!("CARGO")
        .ok_or_else(|| Fail::die("vz-host: the lab was built by no cargo it can name"))?;
    let t0 = Instant::now();
    let out = aterm_uds::spawnfd::command(targo)
        .args(host_build_args(repo))
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("CARGO_BUILD_TARGET_DIR")
        .env_remove("CARGO_BUILD_BUILD_DIR")
        .current_dir(repo)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| Fail::die(format!("vz-host: cannot run {targo}: {e}")))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let tail: Vec<&str> = err.lines().rev().take(12).collect();
        return Err(Fail::die(format!(
            "vz-host: the helper did not build ({targo} --unverified build -p aterm-vz --bin {HOST}):\n{}",
            tail.into_iter().rev().collect::<Vec<_>>().join("\n")
        )));
    }
    let built = executable_of(&String::from_utf8_lossy(&out.stdout))
        .ok_or_else(|| Fail::die("vz-host: the helper's build named no executable"))?;
    println!(
        "vz-host: built {} in {:.1} s",
        built.display(),
        t0.elapsed().as_secs_f64()
    );
    Ok(built)
}

/// Copy `built` to `vz/host/<sha256>/` and sign it there ([`aterm_vz::sign::place`],
/// [`aterm_vz::sign::codesign`]), then reclaim every other copy no helper needs: the one the
/// record at `paths` names is kept, so is one whose helper was started and has not yet proved
/// it ran, and so is one a live helper of any state directory runs from (it holds the copy's
/// `ran` marker, [`aterm_vz::sign::InUse`]).
///
/// # Errors
/// The copy or its signing failed.
pub fn place_host(built: &Path, vz: &Path, paths: &Paths) -> Result<aterm_vz::sign::Placed, Fail> {
    let host = vz.join("host");
    let placed = aterm_vz::sign::place(built, &host, &mut |copy, ents| {
        aterm_vz::sign::codesign(copy, ents)
    })
    .map_err(|why| Fail::die(format!("vz-host: {why}")))?;
    println!(
        "vz-host: {} ({})",
        placed.exe.display(),
        if placed.fresh {
            "signed and placed"
        } else {
            "already there"
        }
    );
    let record = Record::read(&paths.record());
    for gone in aterm_vz::sign::reclaim(&host, &placed.sha256, record.as_ref()) {
        println!("vz-host: reclaimed {}", gone.display());
    }
    Ok(placed)
}

/// `linux vz-host`'s flags.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opts {
    /// The verb: `place` (the default), `up`, `exec`, `status` or `down`.
    pub verb: String,
    /// The VM's state directory, else this user's `vz.noindex`.
    pub state: Option<PathBuf>,
    /// The tooling cache the pins are read from, else this user's.
    pub tooling: Option<PathBuf>,
    /// `up --keep`: the VM stays up once the session closes.
    pub keep: bool,
    /// `up --unsigned`: start the helper as built, without the entitlement (the negative
    /// control).
    pub unsigned: bool,
    /// `up --cpus`.
    pub cpus: u32,
    /// `up --memory-gib`.
    pub memory_bytes: u64,
    /// `exec -- <argv>`.
    pub argv: Vec<String>,
}

/// Parse `linux vz-host`'s arguments.
///
/// # Errors
/// A usage error, in words.
pub fn parse(rest: &[String]) -> Result<Opts, String> {
    let mut o = Opts {
        verb: "place".into(),
        state: None,
        tooling: None,
        keep: false,
        unsigned: false,
        cpus: CPUS,
        memory_bytes: MEMORY,
        argv: Vec::new(),
    };
    let mut it = rest.iter();
    if let Some(verb) = rest.first().filter(|a| !a.starts_with('-')) {
        o.verb.clone_from(verb);
        it.next();
    }
    if !matches!(o.verb.as_str(), "place" | "up" | "exec" | "status" | "down") {
        return Err(format!("unknown verb '{}'", o.verb));
    }
    while let Some(arg) = it.next() {
        let mut value = |name: &str| {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match arg.as_str() {
            "--state" => o.state = Some(PathBuf::from(value("--state")?)),
            "--tooling" => o.tooling = Some(PathBuf::from(value("--tooling")?)),
            "--keep" if o.verb == "up" => o.keep = true,
            "--unsigned" if o.verb == "up" => o.unsigned = true,
            "--cpus" if o.verb == "up" => {
                o.cpus = value("--cpus")?
                    .parse()
                    .map_err(|_| "--cpus takes a count".to_string())?;
            }
            "--memory-gib" if o.verb == "up" => {
                let gib: u64 = value("--memory-gib")?
                    .parse()
                    .map_err(|_| "--memory-gib takes a count".to_string())?;
                o.memory_bytes = gib << 30;
            }
            "--" if o.verb == "exec" => {
                o.argv = it.by_ref().cloned().collect();
            }
            other => return Err(format!("'{other}' is not a flag of vz-host {}", o.verb)),
        }
    }
    if o.verb == "exec" && o.argv.is_empty() {
        return Err("vz-host exec needs a command after --".into());
    }
    Ok(o)
}

/// The checkout, this user's `vz.noindex`, and the VM's run files.
fn places(o: &Opts) -> Result<(PathBuf, PathBuf, Paths), Fail> {
    let repo = crate::paths::physical(&crate::paths::paths().repo);
    let vz = aterm_uds::host::host_user_cache("vz")
        .ok_or_else(|| Fail::die("vz-host: no HOME, so no vz.noindex"))?;
    let state = o.state.clone().unwrap_or_else(|| vz.clone());
    Ok((repo, vz, Paths::at(state)))
}

/// What a client error ends the run with: the helper `exe`'s refusals in the lab's words.
fn client_fail(
    e: ClientError,
    exe: &Path,
    lab_replaced: Option<&aterm_vz::client::Replaced>,
) -> Fail {
    match e {
        ClientError::Failed { error, tail } => {
            print_tail(&tail);
            Fail::exit(NOT_RUN, failed_words(&error, exe, lab_replaced))
        }
        ClientError::Lost { why, tail } => {
            print_tail(&tail);
            Fail::exit(NOT_RUN, not_run_words(&why))
        }
        ClientError::Hung(words) => Fail::exit(NOT_RUN, words),
        ClientError::NoHelper(why) => Fail::exit(
            NOT_RUN,
            not_run_words(&format!("{why} ({LAB} linux vz-host up first)")),
        ),
        other => Fail::die(format!("vz-host: {other}")),
    }
}

fn print_tail(tail: &str) {
    if !tail.is_empty() {
        println!("vz-host: the console's last lines:");
        for line in tail.lines() {
            println!("  | {line}");
        }
    }
}

fn layer_fail(e: aterm_vz::Error) -> Fail {
    match e {
        // The floor's words are NOT RUN already (`space::below_bring_up`).
        aterm_vz::Error::Floor(why) => Fail::exit(NOT_RUN, not_run_words(&why)),
        aterm_vz::Error::Input(why) => not_run(why),
        aterm_vz::Error::Io(why) => Fail::die(format!("vz-host: {why}")),
    }
}

/// The `virtualization` hardware row's gate (§13), read as `kern.hv_support` by sysctl with
/// no framework loaded ([`aterm_sysprobe::hv_support`]): COULD NOT RUN in the row's words
/// where this Mac has no hypervisor, or the read failed.
fn virtualization_gate(hv_support: Option<bool>) -> Result<(), Fail> {
    let row = aterm_uds::host::tool("virtualization");
    match hv_support {
        Some(true) => Ok(()),
        Some(false) => Err(Fail::exit(
            NOT_RUN,
            format!("COULD NOT RUN: {} (kern.hv_support is 0)", row.refusal()),
        )),
        None => Err(Fail::exit(
            NOT_RUN,
            format!(
                "COULD NOT RUN: kern.hv_support did not read, so whether this host has {} is \
                 unknown",
                row.needs_label()
            ),
        )),
    }
}

/// `up`'s hold on the state's bring-ups (V2's stand-in for V4's lane lock, §4): one `up` at
/// a time prepares disks and boots, so two never race a `*.partial.img` one of them booted on.
/// Another's is NOT RUN, naming the lock; nothing waits.
fn bring_up_lock(paths: &Paths) -> Result<DiskLock, Fail> {
    let path = paths.run_dir().join("bring-up.lock");
    std::fs::create_dir_all(paths.run_dir())
        .map_err(|e| Fail::die(format!("vz-host: {}: {e}", paths.run_dir().display())))?;
    match DiskLock::try_take(&path) {
        Ok(Some(lock)) => Ok(lock),
        Ok(None) => Err(Fail::exit(
            NOT_RUN,
            format!(
                "NOT RUN — another `vz-host up` holds {}: it is bringing this VM up",
                path.display()
            ),
        )),
        Err(e) => Err(Fail::die(format!("vz-host: {}: {e}", path.display()))),
    }
}

/// The `Attach` of an `exec` on the live helper `record` names: its helper, and the agent
/// its guest said hello with. Before that hello (the VM is booting) there is no agent to
/// name, and a session naming another would be refused: NOT RUN, and nothing is stopped.
fn exec_attach(record: &Record, repo: &Path) -> Result<Attach, Fail> {
    let agent = record.agent_sha256.clone().ok_or_else(|| {
        Fail::exit(
            NOT_RUN,
            format!(
                "NOT RUN — helper {}'s guest has not said hello yet (its VM is booting): run \
                 exec once `{LAB} linux vz-host up` says ready",
                record.pid
            ),
        )
    })?;
    Ok(Attach {
        proto: PROTO,
        helper_sha256: record.helper_sha256.clone(),
        agent_sha256: agent,
        shares: shares(repo),
        keep: false,
    })
}

/// The `aterm` share of a run from the checkout at `repo`: `repo`, read-only.
fn shares(repo: &Path) -> Vec<ShareEntry> {
    vec![ShareEntry {
        name: "repo".into(),
        path: repo.to_path_buf(),
    }]
}

/// `vz-host up`: the virtualization gate, the bring-up lock, the helper placed (or,
/// `--unsigned`, as built) and the agent placed; then a session, and only when it found no VM
/// the kernel laid, the disks prepared and the boot; then a wait for the agent's `Ready`.
fn cmd_up(o: &Opts) -> Res {
    virtualization_gate(aterm_sysprobe::hv_support())?;
    let (repo, vz, paths) = places(o)?;
    let _bringing_up = bring_up_lock(&paths)?;
    let built = build_host(&repo)?;
    let (exe, helper_sha256) = if o.unsigned {
        let sha = aterm_vz::sign::helper_sha256(&built).map_err(Fail::die)?;
        println!("vz-host: --unsigned: the helper runs as built, without its entitlement");
        (built, sha)
    } else {
        let placed = place_host(&built, &vz, &paths)?;
        (placed.exe, placed.sha256)
    };
    let agent = crate::linux_vz::build_agent(&repo, &vz)?;
    let boot_share = agent
        .path
        .parent()
        .ok_or_else(|| Fail::die("vz-host: the agent has no directory"))?
        .to_path_buf();
    let attach = Attach {
        proto: PROTO,
        helper_sha256,
        agent_sha256: agent.sha256.clone(),
        shares: shares(&repo),
        keep: o.keep,
    };
    let mut lab = Lab::new(paths.clone(), Detached::new(exe.clone()));
    let t0 = Instant::now();
    let session = lab.session(&attach);
    let mut session = session.map_err(|e| client_fail(e, &exe, lab.replaced.as_ref()))?;
    let pid = Record::read(&paths.record()).map_or(0, |r| r.pid);
    println!(
        "vz-host: session on helper {pid} in {:.3} s; the VM is {}",
        t0.elapsed().as_secs_f64(),
        session.phase.name()
    );
    // Only for a session that found no VM: a running VM keeps the disks it booted on.
    let prepare = || -> Result<Plan, Fail> {
        let lookup = |tool| {
            let mut l = aterm_uds::host::Lookup::host(tool);
            if let Some(t) = &o.tooling {
                l.tooling = Some(t.clone());
            }
            l
        };
        let layers = Layers::at(paths.root());
        let t0 = Instant::now();
        let (vmlinuz, initrd) =
            aterm_vz::pins::kernel_files(&lookup(aterm_vz::pins::kernel_tool()))
                .map_err(layer_fail)?;
        let kernel = aterm_vz::kernel::lay(
            &layers.kernel_dir(),
            &vmlinuz,
            &initrd,
            &aterm_vz::kernel::Want::PINNED,
        )
        .map_err(layer_fail)?;
        let base = Base::pinned(&lookup(aterm_vz::pins::root_tool())).map_err(layer_fail)?;
        let user = aterm_vz::seed::User::host()
            .ok_or_else(|| Fail::die("vz-host: this user has no name"))?;
        let seed = aterm_vz::seed::Seed::new(&user).map_err(not_run)?;
        let prepared = layers
            .prepare(&System, &base, &seed.fingerprint(), &mut |s| {
                println!("vz-host: {s}");
            })
            .map_err(layer_fail)?;
        let disks = match prepared {
            Prepared::Live { disk, .. } => vec![Disk {
                path: disk,
                read_only: false,
            }],
            Prepared::Bake(partial) => {
                let iso = layers.seed_iso(&System, &seed).map_err(layer_fail)?;
                println!(
                    "vz-host: no baked disk: this boot is a bake's, on {} with the seed {}",
                    partial.path().display(),
                    iso.display()
                );
                vec![
                    Disk {
                        path: partial.path().to_path_buf(),
                        read_only: false,
                    },
                    Disk {
                        path: iso,
                        read_only: true,
                    },
                ]
            }
        };
        println!(
            "vz-host: kernel, base and disks ready in {:.2} s",
            t0.elapsed().as_secs_f64()
        );
        Ok(Plan {
            name: VM.into(),
            cpus: o.cpus,
            memory_bytes: o.memory_bytes,
            kernel: kernel.image,
            initrd: kernel.initrd,
            command_line: COMMAND_LINE.into(),
            disks,
            mac: mac_for(VM),
            boot_share,
            shares: shares(&repo),
            rosetta: true,
            console_log: paths.console_log(),
            agent_sha256: agent.sha256.clone(),
        })
    };
    let t0 = Instant::now();
    let hello = session
        .bring_up(prepare, &mut |p| {
            println!("vz-host: {:7.2} s {p}", t0.elapsed().as_secs_f64());
        })
        .map_err(|e| match e {
            UpError::Prepare(fail) => fail,
            UpError::Client(e) => client_fail(e, &exe, lab.replaced.as_ref()),
        })?;
    println!(
        "vz-host: ready {:.2} s after the session: kernel {}, boot {}, the agent's hello at guest uptime {} ms",
        t0.elapsed().as_secs_f64(),
        hello.kernel,
        hello.boot_id,
        hello.uptime_ms
    );
    let keep = session.keep;
    session.detach();
    println!(
        "vz-host: detached; keep {}",
        if keep {
            "set: the VM stays up"
        } else {
            "unset: the helper powers the guest off and exits"
        }
    );
    Ok(0)
}

/// The live helper at `paths`, or NOT RUN.
fn live(paths: &Paths) -> Result<Record, Fail> {
    match aterm_vz::record::holder(paths, &Kernel) {
        Holder::Live(r) => Ok(r),
        _ => Err(not_run(format!(
            "no helper runs the VM at {} ({LAB} linux vz-host up first)",
            paths.root().display()
        ))),
    }
}

/// `vz-host exec -- <argv>`: a session joined on the live helper ([`Lab::join`]: nothing is
/// started, stopped or replaced), a command connection, the command run as the lane user,
/// and its output.
fn cmd_exec(o: &Opts) -> Res {
    let (repo, vz, paths) = places(o)?;
    let record = live(&paths)?;
    let attach = exec_attach(&record, &repo)?;
    // The copy it runs from, for the run's words; `join` never starts a helper.
    let exe = vz
        .join("host")
        .join(&record.helper_sha256)
        .join(aterm_vz::sign::EXE);
    let mut lab = Lab::new(paths, Detached::new(exe.clone()));
    let mut session = lab.join(&attach).map_err(|e| client_fail(e, &exe, None))?;
    session
        .wait_ready(&mut |p| println!("vz-host: {p}"))
        .map_err(|e| client_fail(e, &exe, None))?;
    let t0 = Instant::now();
    let (id, fd) = session
        .connect(aterm_vz_agent::COMMAND_PORT)
        .map_err(|e| client_fail(e, &exe, None))?;
    let connected = t0.elapsed();
    let argv: Vec<&str> = o.argv.iter().map(String::as_str).collect();
    let exec = aterm_vz::agent::exec_of(
        &argv,
        "/",
        &[(
            "PATH",
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        )],
    );
    let ran = aterm_vz::agent::run(fd, &exec, aterm_vz::detect::SPAN);
    let _ = session.release(id);
    session.detach();
    let out = ran.map_err(|why| Fail::die(format!("vz-host: {why}")))?;
    use std::io::Write as _;
    let _ = std::io::stdout().write_all(&out.out);
    let _ = std::io::stderr().write_all(&out.err);
    println!(
        "vz-host: connection {id} in {:.1} ms; the command ended {:?} {:.3} s after it",
        connected.as_secs_f64() * 1000.0,
        out.exit,
        t0.elapsed().as_secs_f64()
    );
    Ok(match out.exit {
        aterm_vz_agent::wire::Exit::Exited(code) => code,
        _ => 1,
    })
}

/// `vz-host status`: the live helper's state; nothing is started.
fn cmd_status(o: &Opts) -> Res {
    let (_repo, _vz, paths) = places(o)?;
    let mut lab = Lab::new(paths.clone(), Detached::new(PathBuf::new()));
    match lab
        .status()
        .map_err(|e| client_fail(e, Path::new(""), None))?
    {
        None => println!("vz-host: no helper runs at {}", paths.root().display()),
        Some(s) => {
            println!(
                "vz-host: helper {} (unique id {}), {}; keep {}; {} session(s); boot {}; beat {}; free {}",
                s.pid,
                s.uniq,
                s.phase.name(),
                s.keep,
                s.sessions,
                if s.boot_id.is_empty() {
                    "-"
                } else {
                    &s.boot_id
                },
                s.beat_ms_ago
                    .map_or_else(|| "-".to_string(), |ms| format!("{ms} ms ago")),
                s.free_bytes
                    .map_or_else(|| "-".to_string(), aterm_vz::space::gib),
            );
            print_tail(&s.console_tail);
        }
    }
    Ok(0)
}

/// `vz-host down`: stop the live helper's VM and wait for the helper to end.
fn cmd_down(o: &Opts) -> Res {
    let (_repo, _vz, paths) = places(o)?;
    let pid = Record::read(&paths.record()).map(|r| r.pid);
    let mut lab = Lab::new(paths, Detached::new(PathBuf::new()));
    let t0 = Instant::now();
    lab.down()
        .map_err(|e| client_fail(e, Path::new(""), None))?;
    match pid {
        Some(pid) => println!(
            "vz-host: helper {pid} ended {:.2} s after Stop",
            t0.elapsed().as_secs_f64()
        ),
        None => println!("vz-host: no helper ran"),
    }
    Ok(0)
}

/// `linux vz-host …`.
///
/// # Errors
/// A usage error, or a step that failed.
pub fn cmd_vz_host(rest: &[String]) -> Res {
    let o = parse(rest)
        .map_err(|why| Fail::usage(format!("linux vz-host: {why} ({LAB} linux help)")))?;
    match o.verb.as_str() {
        "up" => cmd_up(&o),
        "exec" => cmd_exec(&o),
        "status" => cmd_status(&o),
        "down" => cmd_down(&o),
        _ => {
            let (repo, vz, paths) = places(&o)?;
            let built = build_host(&repo)?;
            let placed = place_host(&built, &vz, &paths)?;
            println!("vz-host: sha256 {}", placed.sha256);
            Ok(0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn the_build_is_the_helpers_bin_in_the_dev_profile_into_its_own_target_dir() {
        let args = host_build_args(Path::new("/src/aterm"));
        let text: Vec<String> = args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            text,
            [
                "--unverified",
                "build",
                "--locked",
                "--offline",
                "-p",
                "aterm-vz",
                "--bin",
                "aterm-vz-host",
                "--message-format",
                "json-render-diagnostics",
                "--manifest-path",
                "/src/aterm/Cargo.toml",
                "--target-dir",
                "/src/aterm/target/vz-host.noindex",
            ]
        );
        assert!(
            !text.iter().any(|a| a == "--release"),
            "debug assertions check every send"
        );
    }

    #[test]
    fn the_executable_is_read_from_the_bins_artifact_line_only() {
        let lines = [
            r#"{"reason":"compiler-artifact","target":{"name":"aterm_vz","kind":["lib"]},"filenames":["/t/libaterm_vz.rlib"],"executable":null}"#,
            r#"{"reason":"compiler-artifact","target":{"name":"aterm-vz-host","kind":["bin"]},"filenames":["/t/debug/aterm-vz-host"],"executable":"/t/debug/aterm-vz-host"}"#,
            r#"{"reason":"build-finished","success":true}"#,
        ]
        .join("\n");
        assert_eq!(
            executable_of(&lines),
            Some(PathBuf::from("/t/debug/aterm-vz-host"))
        );
        assert_eq!(
            executable_of(&lines.replace("aterm-vz-host", "other")),
            None
        );
        assert_eq!(executable_of("not json"), None);
    }

    #[test]
    fn the_virtualization_gate_reads_kern_hv_support_in_the_rows_words() {
        assert!(virtualization_gate(Some(true)).is_ok());
        for (hv, words) in [
            (
                Some(false),
                "COULD NOT RUN: this host lacks Apple silicon with Virtualization.framework, and \
                 no command adds it (kern.hv_support is 0)",
            ),
            (
                None,
                "COULD NOT RUN: kern.hv_support did not read, so whether this host has Apple \
                 silicon with Virtualization.framework is unknown",
            ),
        ] {
            let fail = virtualization_gate(hv).expect_err("gated");
            assert_eq!((fail.code, fail.msg.as_deref()), (NOT_RUN, Some(words)));
        }
    }

    #[test]
    fn one_up_at_a_time_holds_the_bring_up_lock() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::at(dir.path());
        let first = bring_up_lock(&paths).expect("free");
        let fail = bring_up_lock(&paths).expect_err("held");
        assert_eq!(fail.code, NOT_RUN);
        let words = fail.msg.unwrap_or_default();
        assert!(
            words.starts_with("NOT RUN — another `vz-host up` holds ")
                && words.contains("run/bring-up.lock"),
            "{words}"
        );
        // The negative control: let go, it is free again.
        drop(first);
        assert!(bring_up_lock(&paths).is_ok());
    }

    #[test]
    fn an_exec_names_the_guests_agent_and_refuses_before_its_hello() {
        let mut record = Record {
            pid: 4242,
            uniq: 77,
            helper_sha256: "a1".repeat(32),
            proto: PROTO,
            live_disk: "/vz/live/aterm-linux.img".into(),
            started: 1_792_000_000,
            boot_id: None,
            agent_sha256: None,
        };
        let fail = exec_attach(&record, Path::new("/src/aterm")).expect_err("booting");
        assert_eq!(fail.code, NOT_RUN);
        let words = fail.msg.unwrap_or_default();
        assert!(
            words.starts_with("NOT RUN — helper 4242's guest has not said hello yet"),
            "{words}"
        );
        // The negative control: once the guest said hello, the exec names its agent.
        record.agent_sha256 = Some("cd".repeat(32));
        let attach = exec_attach(&record, Path::new("/src/aterm")).unwrap();
        assert_eq!(
            (
                attach.helper_sha256.as_str(),
                attach.agent_sha256.as_str(),
                attach.keep
            ),
            ("a1".repeat(32).as_str(), "cd".repeat(32).as_str(), false)
        );
    }

    #[test]
    fn a_floors_words_and_a_lost_guests_say_not_run_once() {
        let floor = "NOT RUN — the volume holding aterm-linux.img, /v, has 19.0 GiB free";
        let fail = layer_fail(aterm_vz::Error::Floor(floor.into()));
        assert_eq!((fail.code, fail.msg.as_deref()), (NOT_RUN, Some(floor)));
        let lost = "NOT RUN — the boot hung before the agent";
        let fail = client_fail(
            ClientError::Lost {
                why: lost.into(),
                tail: String::new(),
            },
            Path::new(""),
            None,
        );
        assert_eq!(fail.msg.as_deref(), Some(lost));
        // The negative control: words without it gain it.
        let fail = client_fail(
            ClientError::Lost {
                why: "the VM was stopped by a Stop (down)".into(),
                tail: String::new(),
            },
            Path::new(""),
            None,
        );
        assert_eq!(
            fail.msg.as_deref(),
            Some("NOT RUN — the VM was stopped by a Stop (down)")
        );
    }

    #[test]
    fn the_verbs_take_their_own_flags_only() {
        let o = parse(&[]).unwrap();
        assert_eq!(o.verb, "place");
        let o = parse(&args(&[
            "up",
            "--state",
            "/s",
            "--tooling",
            "/t",
            "--keep",
            "--cpus",
            "4",
            "--memory-gib",
            "4",
        ]))
        .unwrap();
        assert_eq!(
            (
                o.verb.as_str(),
                o.state,
                o.tooling,
                o.keep,
                o.unsigned,
                o.cpus,
                o.memory_bytes
            ),
            (
                "up",
                Some("/s".into()),
                Some("/t".into()),
                true,
                false,
                4,
                4 << 30
            )
        );
        assert!(parse(&args(&["up", "--unsigned"])).unwrap().unsigned);
        let o = parse(&args(&[
            "exec", "--state", "/s", "--", "uname", "-r", "--keep",
        ]))
        .unwrap();
        assert_eq!(
            o.argv,
            ["uname", "-r", "--keep"],
            "everything after -- is the command"
        );
        for (bad, why) in [
            (&["exec"][..], "needs a command"),
            (&["status", "--keep"][..], "not a flag of vz-host status"),
            (&["down", "--unsigned"][..], "not a flag of vz-host down"),
            (&["up", "--cpus"][..], "needs a value"),
            (&["up", "--cpus", "x"][..], "takes a count"),
            (&["reboot"][..], "unknown verb"),
        ] {
            let e = parse(&args(bad)).unwrap_err();
            assert!(e.contains(why), "{bad:?}: {e}");
        }
    }
}
