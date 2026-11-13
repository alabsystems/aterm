// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native qualification of the actual window guards after keeper and owner death.
//! This debug lab starts no memory cohort and makes no timing claim. Failed worlds are kept.

use std::fs::{File, OpenOptions};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_toolchain::named_lock::{self, Holder, Refused, Wait};
use aterm_uds::process::{self, Identity, ProcessPresence};
use aterm_uds::spawnfd::{self, Child, Stdio};

use crate::instance::{self, Instance, Launch, World};
use crate::{Fail, Res};

const HANG: Duration = Duration::from_secs(60);
const MEMORY: u64 = 2 * 1024 * 1024 * 1024;
const READY: &str = "owner.ready";
const REQUEST: &str = "crash-keeper.request";
const ACK: &str = "keeper.reaped";
const REQUEST_BYTES: &[u8] = b"aterm-lab-window-custody/crash-keeper/1\n";

struct Args {
    aterm: PathBuf,
    out: PathBuf,
    route: Route,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Route {
    Child,
    AppBand,
}

impl Route {
    fn name(self) -> &'static str {
        match self {
            Self::Child => "agent",
            Self::AppBand => "app",
        }
    }
    fn parse(word: &str) -> Result<Self, String> {
        match word {
            "agent" => Ok(Self::Child),
            "app" => Ok(Self::AppBand),
            _ => Err("window custody band must be agent or app".into()),
        }
    }
    fn band(self) -> crate::band::Band {
        match self {
            Self::Child => crate::band::Band::Agent,
            Self::AppBand => crate::band::Band::App,
        }
    }
    fn keeper_evidence(self) -> &'static str {
        match self {
            Self::Child => "owned-child-wait-sigkill",
            Self::AppBand => "sigkill-delivered-native-absent",
        }
    }
}

impl Args {
    fn parse(args: &[String], default: Route) -> Result<Self, Fail> {
        let mut args = args.to_vec();
        let route = instance::take_value(&mut args, "--band")?
            .map(|word| Route::parse(&word))
            .transpose()
            .map_err(Fail::usage)?
            .unwrap_or(default);
        let aterm = instance::take_value(&mut args, "--aterm")?
            .ok_or_else(|| Fail::usage("window-custody needs --aterm ABS --out DIR"))?;
        let out = instance::take_value(&mut args, "--out")?
            .ok_or_else(|| Fail::usage("window-custody needs --aterm ABS --out DIR"))?;
        if !args.is_empty() || !Path::new(&aterm).is_absolute() || !Path::new(&out).is_absolute() {
            return Err(Fail::usage(
                "window-custody needs only --aterm ABS --out ABS",
            ));
        }
        let aterm = std::fs::canonicalize(&aterm).map_err(|e| Fail::die(e.to_string()))?;
        let metadata = std::fs::metadata(&aterm).map_err(|e| Fail::die(e.to_string()))?;
        if !metadata.is_file() || metadata.mode() & 0o111 == 0 {
            return Err(Fail::die("--aterm must name an actual executable"));
        }
        Ok(Self {
            aterm,
            out: out.into(),
            route,
        })
    }
}

struct Receipt {
    world: PathBuf,
    socket: PathBuf,
    owner: Identity,
    keeper: Identity,
    gui: Identity,
    lane: (u64, u64),
    memory: crate::window_guards::MemoryObservation,
    route: Route,
    job: Option<String>,
}

impl Receipt {
    fn text(&self) -> String {
        format!(
            "aterm-lab-window-custody/3\nworld={}\nsocket={}\nowner={}\nkeeper={}\ngui={}\nlane-dev={}\nlane-ino={}\nmemory-claim={}\nmemory-dev={}\nmemory-ino={}\nmemory-bytes={}\nmemory-pid={}\nmemory-start={}\nmemory-sequence={}\nroute={}\njob={}\n",
            self.world.display(),
            self.socket.display(),
            self.owner.token(),
            self.keeper.token(),
            self.gui.token(),
            self.lane.0,
            self.lane.1,
            self.memory.claim,
            self.memory.device,
            self.memory.inode,
            self.memory.bytes,
            self.memory.pid,
            self.memory.start_ms,
            self.memory.sequence,
            self.route.name(),
            self.job.as_deref().unwrap_or("none")
        )
    }
    fn parse(bytes: &[u8]) -> Result<Self, String> {
        let text = std::str::from_utf8(bytes).map_err(|e| e.to_string())?;
        let lines: Vec<_> = text.lines().collect();
        if lines.len() != 17 || lines[0] != "aterm-lab-window-custody/3" {
            return Err("invalid custody receipt envelope".into());
        }
        let field = |index: usize, name: &str| {
            lines[index]
                .strip_prefix(name)
                .ok_or_else(|| format!("missing custody field {name}"))
        };
        let identity = |index, name| {
            Identity::parse(field(index, name)?)
                .ok_or_else(|| "invalid native process identity".to_string())
        };
        let number = |index, name| -> Result<u64, String> {
            field(index, name)?
                .parse::<u64>()
                .map_err(|why| why.to_string())
        };
        let receipt = Self {
            world: field(1, "world=")?.into(),
            socket: field(2, "socket=")?.into(),
            owner: identity(3, "owner=")?,
            keeper: identity(4, "keeper=")?,
            gui: identity(5, "gui=")?,
            lane: (
                field(6, "lane-dev=")?
                    .parse::<u64>()
                    .map_err(|e| e.to_string())?,
                field(7, "lane-ino=")?
                    .parse::<u64>()
                    .map_err(|e| e.to_string())?,
            ),
            memory: crate::window_guards::MemoryObservation {
                claim: field(8, "memory-claim=")?.to_owned(),
                device: number(9, "memory-dev=")?,
                inode: number(10, "memory-ino=")?,
                bytes: number(11, "memory-bytes=")?,
                pid: u32::try_from(number(12, "memory-pid=")?).map_err(|why| why.to_string())?,
                start_ms: number(13, "memory-start=")?,
                sequence: number(14, "memory-sequence=")?,
            },
            route: Route::parse(field(15, "route=")?)?,
            job: match field(16, "job=")? {
                "none" => None,
                job => Some(job.to_owned()),
            },
        };
        if receipt.text() != text {
            return Err("noncanonical custody receipt".into());
        }
        receipt.validate()?;
        Ok(receipt)
    }

    fn validate(&self) -> Result<(), String> {
        if !self.world.is_absolute()
            || self.socket != self.world.join("run/a.sock")
            || self.owner.pid == self.keeper.pid
            || self.owner.pid == self.gui.pid
            || self.keeper.pid == self.gui.pid
            || self.memory.pid != u32::try_from(self.owner.pid).map_err(|why| why.to_string())?
            || self.memory.bytes != MEMORY
            || self.memory.claim
                != format!(
                    "{}-{}-{}.claim",
                    self.memory.start_ms, self.memory.pid, self.memory.sequence
                )
            || match self.route {
                Route::Child => self.job.is_some(),
                Route::AppBand => {
                    self.job.as_deref()
                        != Some(crate::band::label_for(&self.world, self.memory.pid).as_str())
                }
            }
        {
            return Err(
                "custody receipt has substituted process, route, job or original memory identity"
                    .into(),
            );
        }
        crate::window_guards::WindowGuards::arguments(&[3, 4], Some(&self.memory.claim))?;
        Ok(())
    }

    fn keeper_ack(&self) -> String {
        format!(
            "aterm-lab-window-keeper-cut/1\nkeeper={}\nroute={}\nevidence={}\njob={}\n",
            self.keeper.token(),
            self.route.name(),
            self.route.keeper_evidence(),
            self.job.as_deref().unwrap_or("none")
        )
    }
}

fn identity(pid: i32) -> Result<Identity, String> {
    process::read_identity(pid).ok_or_else(|| format!("no actual native identity for pid {pid}"))
}

fn same_process(original: Identity, what: &str) -> Result<(), String> {
    if process::read_identity(original.pid) == Some(original) {
        Ok(())
    } else {
        Err(format!(
            "{what}: original native process identity is no longer present"
        ))
    }
}

fn record_process_groups(receipt: &Receipt, out: &Path) -> Result<(), String> {
    let groups = |token: Identity| -> Result<(i32, i32), String> {
        same_process(token, "process group observation")?;
        // SAFETY: positive, freshly validated owned process IDs; these calls only read
        // kernel group/session identifiers. Recheck native identity after both reads.
        #[allow(unsafe_code)]
        let pair = unsafe { (libc::getpgid(token.pid), libc::getsid(token.pid)) };
        same_process(token, "process group observation")?;
        if pair.0 <= 0 || pair.1 <= 0 {
            return Err(format!(
                "native process group/session is unavailable for {}: pgid={}, sid={}",
                token.token(),
                pair.0,
                pair.1
            ));
        }
        Ok(pair)
    };
    let keeper = groups(receipt.keeper)?;
    let gui = groups(receipt.gui)?;
    if gui != (receipt.gui.pid, receipt.gui.pid) || gui.0 == keeper.0 {
        return Err("guarded GUI does not own a separate native session/process group".into());
    }
    aterm_uds::private_file::write_private(
        out,
        "original-process-groups",
        format!(
            "keeper={}\nkeeper-pgid={}\nkeeper-sid={}\ngui={}\ngui-pgid={}\ngui-sid={}\n",
            receipt.keeper.token(),
            keeper.0,
            keeper.1,
            receipt.gui.token(),
            gui.0,
            gui.1
        )
        .as_bytes(),
    )
    .map_err(|why| why.to_string())
}

fn lane_path() -> Result<PathBuf, String> {
    aterm_capacity::Ledger::user()
        .map(|ledger| ledger.dir().join("lab-window.lock"))
        .ok_or_else(|| "no physical account capacity ledger".into())
}

fn lane_file() -> Result<File, String> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(lane_path()?)
        .map_err(|e| e.to_string())
}

fn file_identity(file: &File) -> Result<(u64, u64), String> {
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() {
        return Err("window lane is not a regular file".into());
    }
    Ok((metadata.dev(), metadata.ino()))
}

fn unchanged_lane(pinned: &File, expected: (u64, u64)) -> Result<(), String> {
    if file_identity(pinned)? != expected || file_identity(&lane_file()?)? != expected {
        return Err("the original account window lane inode changed".into());
    }
    Ok(())
}

/// Observe the same actual machine ledger and its reservation arithmetic. This
/// creates no claim and supplies no synthetic capacity to the machine.
fn memory_accounting(receipt: &Receipt, out: &Path, phase: &str, held: bool) -> Result<(), String> {
    let ledger = aterm_capacity::Ledger::user().ok_or("original memory ledger unavailable")?;
    aterm_uds::private_file::check_private_dir(ledger.dir()).map_err(|why| why.to_string())?;
    let path = ledger.dir().join(&receipt.memory.claim);
    match std::fs::symlink_metadata(&path) {
        Ok(metadata)
            if metadata.is_file()
                && (metadata.dev(), metadata.ino())
                    == (receipt.memory.device, receipt.memory.inode) => {}
        Err(why) if !held && why.kind() == std::io::ErrorKind::NotFound => {}
        other => return Err(format!("original memory inode changed: {other:?}")),
    }
    let records = ledger.list().map_err(|why| why.to_string())?;
    let reservations = aterm_capacity::reserved(&records, None, &[]);
    let matches: Vec<_> = records
        .iter()
        .enumerate()
        .filter(|(_, record)| {
            record.pid == receipt.memory.pid
                && record.start_ms == receipt.memory.start_ms
                && record.seq == receipt.memory.sequence
        })
        .collect();
    let reserved = if held {
        if matches.len() != 1 {
            return Err("original held memory claim missing or duplicated".into());
        }
        let (index, record) = matches[0];
        if record.accounting != aterm_capacity::Accounting::Detached
            || record.held.is_none()
            || record.need.mem != receipt.memory.bytes
            || record.need.covers
            || record.need.quiet.is_some()
            || reservations[index].mem != receipt.memory.bytes
        {
            return Err(format!(
                "original detached memory reservation was reduced: {record:?}, {:?}",
                reservations[index]
            ));
        }
        reservations[index].mem
    } else {
        if !matches.is_empty() {
            return Err("original memory still counted after final guard close".into());
        }
        0
    };
    aterm_uds::private_file::write_private(out, phase, format!(
        "claim={}\noriginal_pid={}\noriginal_bytes={}\naccounting={}\nactual_reserved_bytes={reserved}\nledger_records={}\n",
        receipt.memory.claim, receipt.memory.pid, receipt.memory.bytes,
        if held { "Detached" } else { "not-counted-after-final-close" }, records.len()
    ).as_bytes()).map_err(|why| why.to_string())
}

fn control(receipt: &Receipt, command: &str) -> Result<String, String> {
    let socket = receipt.socket.to_str().ok_or("non-UTF8 private socket")?;
    let mut client = aterm_agent::RelayClient::connect_local_beside(socket)
        .map_err(|e| e.to_string())?
        .bounded(HANG);
    let reply = client
        .request_line(command)
        .map_err(|e| format!("{command}: {e}"))?;
    if !reply.starts_with("OK") {
        return Err(format!("{command}: {reply}"));
    }
    Ok(reply)
}

fn challenge(receipt: &Receipt, size: &str, phase: &str, out: &Path) -> Result<(), String> {
    same_process(receipt.gui, phase)?;
    // Resize acknowledges an actual GUI main-thread dispatch, rather than just a saved PID
    // or a control worker's metadata answer. Each phase opens a fresh authenticated socket.
    let resized = control(receipt, &format!("resize px {size}"))?;
    let version = control(receipt, "version")?;
    same_process(receipt.gui, phase)?;
    aterm_uds::private_file::write_private(out, phase, format!("{resized}\n{version}\n").as_bytes())
        .map_err(|e| e.to_string())
}

fn pause(deadline: Instant, what: &str) -> Result<(), String> {
    if Instant::now() >= deadline {
        return Err(format!("one-minute hang detector: {what}"));
    }
    crate::stop::sleep(Duration::from_millis(20)).map_err(|e| format!("interrupted: {e:?}"))
}

fn read_record(out: &Path, name: &str) -> Result<Option<Vec<u8>>, String> {
    match std::fs::symlink_metadata(out.join(name)) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
        Ok(_) => aterm_uds::private_file::read_private(out, name, 8192)
            .map(Some)
            .map_err(|e| e.to_string()),
    }
}

fn quit(receipt: &Receipt) -> Result<(), String> {
    same_process(receipt.gui, "authenticated quit")?;
    let _ = control(receipt, "invoke Quit")?;
    if process::process_presence(receipt.gui.pid) == ProcessPresence::Present {
        // A confirmation is legitimate only after Quit. A GUI that already ended needs no
        // answer; absence is established separately with a kernel probe below.
        let _ = control(receipt, "confirm yes");
    }
    Ok(())
}

struct OwnedHelper {
    child: Child,
    receipt: Option<Receipt>,
    job: Option<OwnedJob>,
}

struct OwnedJob {
    world: PathBuf,
    owner: u32,
    label: String,
}

impl OwnedJob {
    fn remove(&self) -> Result<(), String> {
        crate::band::remove_owned_for_qualification(&self.world, self.owner, &self.label)
    }
}

impl Drop for OwnedHelper {
    fn drop(&mut self) {
        if let Some(receipt) = &self.receipt
            && process::read_identity(receipt.gui.pid) == Some(receipt.gui)
        {
            let _ = quit(receipt);
        }
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let end = Instant::now() + HANG;
            while matches!(self.child.try_wait(), Ok(None)) && Instant::now() < end {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        if let Some(job) = self.job.take() {
            // A helper SIGKILL bypasses Instance::Drop. This locator was captured from
            // the parent's original private World and actual child, before readiness.
            if let Err(why) = job.remove() {
                eprintln!("aterm-lab: owned qualification job cleanup: {why}");
            }
        }
    }
}

/// Qualify native GUI guard custody through owned keeper and owner SIGKILL/reap.
/// # Errors
/// Invalid inputs or any missing actual process, control, inode, lock or closure evidence.
pub fn cmd(args: &[String]) -> Res {
    let args = Args::parse(args, Route::Child)?;
    qualify(&args).map(|()| 0).map_err(Fail::die)
}

/// Exercise the authenticated launchd FD/ACK/GO route into an actual GUI.
/// This is startup/dispatch qualification, separate from the two-route crash oracle.
pub fn app_band(args: &[String]) -> Res {
    let mut args = args.to_vec();
    let cut_keeper = args
        .iter()
        .position(|word| word == "--cut-keeper")
        .map(|at| args.remove(at))
        .is_some();
    let args = Args::parse(&args, Route::AppBand)?;
    if args.route != Route::AppBand {
        return Err(Fail::usage("window-handoff-app requires the app band"));
    }
    run_app_band(&args, cut_keeper)
        .map(|()| 0)
        .map_err(Fail::die)
}

fn run_app_band(args: &Args, cut_keeper: bool) -> Result<(), String> {
    aterm_uds::private_file::create_private_dir(&args.out).map_err(|e| e.to_string())?;
    if std::fs::read_dir(&args.out)
        .map_err(|e| e.to_string())?
        .next()
        .is_some()
    {
        return Err("--out must be a fresh empty private directory".into());
    }
    let mut world = prepare_world(&args.out)?;
    let mut instance = Instance::start_retained_functional_in(
        &world,
        Launch {
            aterm: args.aterm.clone(),
            headless: false,
            args: Vec::new(),
            env: world.env(),
            cwd: world.home.clone(),
            sandbox: None,
            reroute: false,
        },
        MEMORY,
        crate::band::Band::App,
    )?;
    instance
        .await_socket(HANG)
        .map_err(|e| format!("actual app-band GUI startup: {e:?}"))?;
    let receipt = Receipt {
        world: world.dir.clone(),
        socket: instance.sock.clone(),
        owner: identity(i32::try_from(std::process::id()).map_err(|e| e.to_string())?)?,
        keeper: identity(instance.pid)?,
        gui: identity(
            instance
                .aterm_pid()
                .ok_or("app-band keeper has no actual GUI child")?,
        )?,
        lane: file_identity(&lane_file()?)?,
        memory: instance.memory_for_qualification()?,
        route: Route::AppBand,
        job: instance.qualification_job_label().map(str::to_owned),
    };
    receipt.validate()?;
    record_process_groups(&receipt, &args.out)?;
    challenge(
        &receipt,
        "920 680",
        "app-band-authenticated-handoff",
        &args.out,
    )?;
    memory_accounting(&receipt, &args.out, "memory-app-live", true)?;
    if cut_keeper {
        instance.stop()?;
    } else {
        quit(&receipt)?;
    }
    let deadline = Instant::now() + HANG;
    while process::process_presence(receipt.gui.pid) != ProcessPresence::Absent {
        pause(
            deadline,
            "app-band actual GUI exit after authenticated Quit",
        )?;
    }
    if !cut_keeper {
        instance.stop()?;
    }
    let deadline = Instant::now() + HANG;
    while process::process_presence(receipt.keeper.pid) != ProcessPresence::Absent {
        pause(
            deadline,
            "app-band actual keeper absence after normal closure",
        )?;
    }
    drop(instance);
    memory_accounting(&receipt, &args.out, "memory-app-released", false)?;
    aterm_uds::private_file::write_private(&args.out, "passed", format!("{}actual-gui-dispatch=yes\nnormal-end={}\ngui-and-keeper-absent=yes\ncrash-custody-qualified=no\n", receipt.text(), if cut_keeper { "keeper-lifeline" } else { "authenticated-quit" }).as_bytes()).map_err(|e| e.to_string())?;
    world.release();
    println!(
        "PASS: authenticated app-band FD/ACK/GO reached actual GUI dispatch and completed normal closure; {}",
        args.out.display()
    );
    Ok(())
}

fn qualify(args: &Args) -> Result<(), String> {
    aterm_uds::private_file::create_private_dir(&args.out).map_err(|e| e.to_string())?;
    if std::fs::read_dir(&args.out)
        .map_err(|e| e.to_string())?
        .next()
        .is_some()
    {
        return Err("--out must be a fresh empty private directory".into());
    }
    let out = std::fs::canonicalize(&args.out).map_err(|e| e.to_string())?;
    // The independent controller owns the world/job locator before the helper can
    // start a window, so a malformed/missing readiness record cannot lose cleanup.
    let mut world = prepare_world(&out)?;
    let log = crate::util::create_stdio_file(&out.join("owner.log")).map_err(|e| e.to_string())?;
    let child = spawnfd::command(std::env::current_exe().map_err(|e| e.to_string())?)
        .args(["window-custody-owner", "--aterm"])
        .arg(&args.aterm)
        .arg("--out")
        .arg(&out)
        .args(["--band", args.route.name()])
        .stdin(Stdio::null())
        .stdout(log.try_clone().map_err(|e| e.to_string())?)
        .stderr(log)
        .spawn()
        .map_err(|e| e.to_string())?;
    let job = (args.route == Route::AppBand).then(|| OwnedJob {
        world: world.dir.clone(),
        owner: child.id(),
        label: crate::band::label_for(&world.dir, child.id()),
    });
    let mut helper = OwnedHelper {
        child,
        receipt: None,
        job,
    };
    let owner_pid = i32::try_from(helper.child.id()).map_err(|e| e.to_string())?;
    let owner_identity = identity(owner_pid)?;
    let deadline = Instant::now() + HANG;
    let receipt = loop {
        if let Some(status) = helper.child.try_wait().map_err(|e| e.to_string())? {
            return Err(format!("owner ended before readiness: {status}"));
        }
        if let Some(bytes) = read_record(&out, READY)? {
            break Receipt::parse(&bytes)?;
        }
        pause(deadline, "actual owner/keeper/GUI readiness")?;
    };
    if receipt.owner != owner_identity
        || receipt.owner.pid != owner_pid
        || receipt.route != args.route
        || receipt.world != world.dir
    {
        return Err("receipt did not come from the original owned helper identity".into());
    }
    helper.receipt = Some(receipt);
    let receipt = helper.receipt.as_ref().ok_or("missing original receipt")?;
    same_process(receipt.owner, "ready owner")?;
    same_process(receipt.keeper, "ready keeper")?;
    let pinned_lane = lane_file()?; // An independent, unheld observer, never window_lane().
    unchanged_lane(&pinned_lane, receipt.lane)?;
    challenge(receipt, "920 680", "before-crashes", &out)?;
    record_process_groups(receipt, &out)?;
    memory_accounting(receipt, &out, "memory-before-crashes", true)?;
    aterm_uds::private_file::write_private(&out, REQUEST, REQUEST_BYTES)
        .map_err(|e| e.to_string())?;
    let deadline = Instant::now() + HANG;
    loop {
        if let Some(status) = helper.child.try_wait().map_err(|e| e.to_string())? {
            return Err(format!("owner ended before actual keeper reap: {status}"));
        }
        if let Some(bytes) = read_record(&out, ACK)? {
            if bytes != receipt.keeper_ack().as_bytes() {
                return Err("foreign keeper reap acknowledgement".into());
            }
            break;
        }
        pause(
            deadline,
            "owned keeper crash/native absence acknowledgement",
        )?;
    }
    if process::process_presence(receipt.keeper.pid) != ProcessPresence::Absent {
        return Err("the acknowledged keeper was not actually reaped".into());
    }
    if let Some(job) = &helper.job {
        // Observe the real one-use restart retiring its own job while the GUI must
        // remain alive. Explicit bootout here could hide the lifecycle under test.
        crate::band::await_owned_absent_for_qualification(&job.world, job.owner, &job.label)?;
        aterm_uds::private_file::write_private(
            &out,
            "job-automatically-absent",
            format!(
                "job={}\nexplicit-bootout-before-observation=no\n",
                job.label
            )
            .as_bytes(),
        )
        .map_err(|why| why.to_string())?;
    }
    challenge(receipt, "940 700", "after-keeper-reap", &out)?;
    memory_accounting(receipt, &out, "memory-after-keeper", true)?;
    same_process(receipt.owner, "owner before SIGKILL")?;
    if helper
        .child
        .try_wait()
        .map_err(|e| e.to_string())?
        .is_some()
    {
        return Err("owner ended before its controlled crash".into());
    }
    helper.child.kill().map_err(|e| e.to_string())?;
    let deadline = Instant::now() + HANG;
    let status = loop {
        if let Some(status) = helper.child.try_wait().map_err(|e| e.to_string())? {
            break status;
        }
        pause(deadline, "owned owner SIGKILL/reap")?;
    };
    if status.signal() != Some(libc::SIGKILL) {
        return Err(format!(
            "owner was not killed by the actual owned Child: {status}"
        ));
    }
    challenge(receipt, "960 720", "after-owner-reap", &out)?;
    memory_accounting(receipt, &out, "memory-after-owner", true)?;
    unchanged_lane(&pinned_lane, receipt.lane)?;
    let me = Holder::me(
        "lab window-custody independent probe",
        &std::env::current_dir().map_err(|e| e.to_string())?,
    );
    match named_lock::take(&lane_path()?, &me, Wait::No, &mut |_, _| {}, &|| false) {
        Err(Refused::Held(_)) => {}
        Ok(unexpected) => {
            drop(unexpected);
            return Err(
                "lane was free while the original live GUI should own its sole custody".into(),
            );
        }
        Err(error) => return Err(format!("independent lane probe: {error:?}")),
    }
    challenge(receipt, "980 740", "gui-only-custody", &out)?;
    quit(receipt)?;
    let deadline = Instant::now() + HANG;
    while process::process_presence(receipt.gui.pid) != ProcessPresence::Absent {
        pause(
            deadline,
            "original native GUI exit/reap after authenticated Quit",
        )?;
    }
    let deadline = Instant::now() + HANG;
    let reacquired = loop {
        unchanged_lane(&pinned_lane, receipt.lane)?;
        match named_lock::take(&lane_path()?, &me, Wait::No, &mut |_, _| {}, &|| false) {
            Ok(held) => break held,
            Err(Refused::Held(_)) => pause(deadline, "the GUI guard's final close")?,
            Err(error) => return Err(format!("final lane reacquisition: {error:?}")),
        }
    };
    let metadata = reacquired.metadata().map_err(|e| e.to_string())?;
    if (metadata.dev(), metadata.ino()) != receipt.lane {
        return Err("reacquired a different account window lane".into());
    }
    unchanged_lane(&pinned_lane, receipt.lane)?;
    memory_accounting(receipt, &out, "memory-after-gui", false)?;
    if let Some(job) = &helper.job {
        job.remove()?;
    }
    aterm_uds::private_file::write_private(&out, "passed", format!("{}owner-signal=SIGKILL\nkeeper-evidence={}\nkeeper-native-absent=yes\ngui-fresh-after-both=yes\nindependent-lane-held=yes\ngui-absent-before-reacquire=yes\noriginal-memory-full-after-owner=yes\noriginal-memory-released-after-gui=yes\nowned-job-absent={}\n", receipt.text(), receipt.route.keeper_evidence(), if receipt.route == Route::AppBand { "yes" } else { "not-applicable" }).as_bytes()).map_err(|e| e.to_string())?;
    helper.job = None;
    world.release();
    println!(
        "PASS: actual GUI retained original window lane after owned keeper and owner SIGKILL/reap; same inode reacquired only after authenticated Quit and native absence; evidence {}",
        out.display()
    );
    Ok(())
}

/// Original owner side of the native qualification; started only as an actual owned child.
/// # Errors
/// A private fixture, admission, actual native identity, controlled crash or acknowledgement fails.
pub fn owner(args: &[String]) -> Res {
    let args = Args::parse(args, Route::Child)?;
    run_owner(&args).map(|()| 0).map_err(Fail::die)
}

fn run_owner(args: &Args) -> Result<(), String> {
    aterm_uds::private_file::check_private_dir(&args.out).map_err(|e| e.to_string())?;
    let bytes = read_record(&args.out, "world.txt")?
        .ok_or("original controller did not prepare its world")?;
    let path = std::str::from_utf8(&bytes)
        .map_err(|why| why.to_string())?
        .strip_suffix('\n')
        .ok_or("invalid original world locator")?;
    let world = World::open_at(Path::new(path))?;
    let mut instance = Instance::start_retained_functional_in(
        &world,
        Launch {
            aterm: args.aterm.clone(),
            headless: false,
            args: vec![
                "--columns".into(),
                "100".into(),
                "--lines".into(),
                "30".into(),
            ],
            env: world.env(),
            cwd: world.home.clone(),
            sandbox: None,
            reroute: false,
        },
        MEMORY,
        args.route.band(),
    )?;
    instance
        .await_socket(HANG)
        .map_err(|e| format!("actual GUI startup: {e:?}"))?;
    let receipt = Receipt {
        world: world.dir.clone(),
        socket: instance.sock.clone(),
        owner: identity(i32::try_from(std::process::id()).map_err(|e| e.to_string())?)?,
        keeper: identity(instance.pid)?,
        gui: identity(
            instance
                .aterm_pid()
                .ok_or("owned keeper has no actual GUI child")?,
        )?,
        lane: file_identity(&lane_file()?)?,
        memory: instance.memory_for_qualification()?,
        route: args.route,
        job: instance.qualification_job_label().map(str::to_owned),
    };
    receipt.validate()?;
    challenge(&receipt, "900 660", "owner-initial-dispatch", &args.out)?;
    aterm_uds::private_file::write_private(&args.out, READY, receipt.text().as_bytes())
        .map_err(|e| e.to_string())?;
    let deadline = Instant::now() + HANG;
    loop {
        if let Some(bytes) = read_record(&args.out, REQUEST)? {
            if bytes != REQUEST_BYTES {
                return Err("foreign keeper crash request".into());
            }
            break;
        }
        pause(deadline, "original controller's keeper crash request")?;
    }
    instance.crash_keeper_for_qualification(receipt.keeper)?;
    aterm_uds::private_file::write_private(&args.out, ACK, receipt.keeper_ack().as_bytes())
        .map_err(|e| e.to_string())?;
    let deadline = Instant::now() + HANG;
    loop {
        pause(deadline, "original controller's actual owner SIGKILL")?;
    }
}

fn prepare_world(out: &Path) -> Result<World, String> {
    let mut world = World::new(instance::CONFIG_OFF)?;
    world.keep();
    crate::util::set_dir_mode_nofollow(&world.cfg, 0o700).map_err(|why| why.to_string())?;
    aterm_uds::private_file::write_private(&world.dir, "lab-world", b"aterm-lab-world/1\n")
        .map_err(|e| e.to_string())?;
    aterm_uds::private_file::write_private(
        out,
        "world.txt",
        format!("{}\n", world.dir.display()).as_bytes(),
    )
    .map_err(|e| e.to_string())?;
    Ok(world)
}

#[cfg(test)]
#[path = "window_custody_tests.rs"]
mod tests;
