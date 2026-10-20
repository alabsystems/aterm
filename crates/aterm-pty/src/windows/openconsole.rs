// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Launching an OpenConsole.exe as a pseudoconsole's host: a port of
//! microsoft/terminal's `src/winconpty/winconpty.cpp` (MIT), with the
//! process's console-host state and the Windows lookups (`WindowsApps`
//! packages, PE file versions) the policy in `crate::console_host::policy` is
//! handed. The state starts at the inbox host for an embedder that states no
//! choice; a window states aterm.toml's `conpty` ([`set_console_host`]), and
//! its absence is the bundled host, so every window tab of a shipped build
//! reaches the launch below. Run on Windows 11 26200 since 2026-10-17 (the
//! crate's `tests/windows_openconsole.rs`, and the bundled 1.25.2609.30003
//! measured against the inbox host, `docs/WINDOWS_PARAGON_DESIGN.md` §5.4);
//! notes marked as the snapshot's are the 2026-09-29 snapshot's.
//!
//! WHERE THE DEFAULT FINDS ONE: the `OpenConsole.exe` beside the running
//! executable (the pinned one `apps/aterm-win/build.ps1` lays there), else
//! none. WHERE `auto` FINDS ONE: that file, else the installed Windows
//! Terminal's, COPIED
//! into aterm's state directory. The snapshot recorded an unpackaged process
//! launching it inside `WindowsApps` getting ERROR_ACCESS_DENIED (for
//! `CreateProcessW` and .NET `Process.Start` alike, although the file ACL grants
//! BUILTIN\Users execute: only a process with package identity may), while
//! reading it is allowed and the copy keeps its valid Authenticode signature
//! and runs. A failure on the way (no file, a copy that fails, a host that
//! rejects its command line, dies before it speaks or writes no VT) falls back
//! to the inbox host for that tab and every later one under the same choice,
//! and is handed to the frontend's log once ([`take_console_host_fallback`]); a
//! host that only stays silent costs the one tab, until it has done so three
//! times in a row. A program that never connects to its console (a GUI image)
//! or exits before it does is not the host's failure ([`await_started`]).
//!
//! THE PORT relies on one ABI fact: the value `PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE`
//! takes is a pointer to `{ hSignal, hPtyReference, hConPtyProcess }`
//! (winconpty.h's `PseudoConsole`), the three slots the inbox HPCON has: this
//! crate's own disassembly of `kernelbase!ResizePseudoConsole` writes `[0]`, and
//! `ClosePseudoConsole` closes `[0]`, `[8]`, `[0x10]`. node-pty's conpty.dll and
//! Windows Terminal hand exactly such a struct to the stock attribute.

use std::ffi::{OsStr, c_void};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use aterm_types::MutexExt;

use super::cmdline::wide_nul;
use super::ffi::{self, HANDLE, Handle};
use crate::console_host::policy::{self, AutoSource, HostState, OpenConsoleExe, Origin};
use crate::{ConsoleHostChoice, ConsoleHostReport};

/// The process's console-host state. Starts at the inbox host: an embedder
/// that never states a choice (the CLI's passthrough session, this crate's own
/// tests) keeps the host every earlier build used; a window states its
/// aterm.toml `conpty` before its first tab.
static HOST: Mutex<HostState> = Mutex::new(HostState::new());

/// Set the console host every LATER spawn in this process uses (the window
/// calls it with aterm.toml's `conpty` at launch and on every reload). A
/// changed choice is resolved afresh on the next spawn; the same choice keeps
/// its resolution, a failure included.
pub fn set_console_host(choice: ConsoleHostChoice) {
    HOST.lock_or_recover().set_choice(choice);
}

/// The fallback to the Windows console host this process has not reported yet
/// — at most once per failure, for the frontend's log (this crate has none).
#[must_use]
pub fn take_console_host_fallback() -> Option<String> {
    HOST.lock_or_recover().take_unlogged()
}

/// The console host a new tab gets under the current choice, resolved now if
/// no spawn has resolved it yet. Resolving `auto` (never the default) can copy
/// Windows Terminal's OpenConsole.exe into aterm's state directory; it never
/// launches it — a host that fails to START is reported once a spawn has tried
/// it (or by [`probe_console_host`]).
#[must_use]
pub fn console_host_report() -> ConsoleHostReport {
    let mut host = HOST.lock_or_recover();
    host.set_guarded(super::console_guard::available());
    host.report(resolve)
}

/// The console host `choice` gives a new tab, with the host actually STARTED:
/// the resolved OpenConsole is launched headless on throwaway pipes with
/// `cmd.exe /d /c exit` as its client, must pass the spawn's health check
/// ([`await_started`]), and both are ended again. For `aterm doctor`, a
/// separate process that has spawned nothing. `inbox` starts nothing; the
/// default starts the bundled host when there is one.
#[must_use]
pub fn probe_console_host(choice: &ConsoleHostChoice) -> ConsoleHostReport {
    let resolved = policy::resolve_choice(choice, resolve).and_then(|exe| {
        let pipes = super::Pipes::new().map_err(|e| format!("pipes: {e}"))?;
        let host = launch(&exe, ffi::COORD { x: 80, y: 24 }, &pipes)
            .map_err(|why| policy::did_not_start(&why))?;
        let mut command = system_command("cmd.exe", "/d /c exit");
        let client = start_client(&mut host.slots(), &mut command)
            .map_err(|e| policy::did_not_start(&format!("its probe client did not start: {e}")))?;
        let started = await_started(&host, client.raw(), pipes.out_read.raw());
        // SAFETY: the probe client we just started, by its handle.
        unsafe { ffi::TerminateProcess(client.raw(), 1) };
        match started {
            Health::Started => Ok(exe),
            Health::NoClient => Err(policy::did_not_start(
                "its probe client exited before the host wrote anything",
            )),
            Health::Failed { why, .. } => Err(policy::did_not_start(&why)),
        }
    });
    policy::report_guarded(choice, &resolved, super::console_guard::available())
}

/// `"<SystemRoot>\System32\<exe>" <args>`, NUL-terminated: a system program by
/// its full path, never whatever PATH finds first under that name.
pub(crate) fn system_command(exe: &str, args: &str) -> Vec<u16> {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    let path = Path::new(&root).join("System32").join(exe);
    let mut line: Vec<u16> = vec![u16::from(b'"')];
    line.extend(path.as_os_str().encode_wide());
    line.push(u16::from(b'"'));
    line.push(u16::from(b' '));
    line.extend(args.encode_utf16());
    line.push(0);
    line
}

/// The OpenConsole a spawn of `program` should launch now, `None` for the
/// inbox host (`HostState::host_for_spawn`).
pub(crate) fn host_for_spawn(program: &OsStr) -> Option<OpenConsoleExe> {
    let mut host = HOST.lock_or_recover();
    host.set_guarded(super::console_guard::available());
    host.host_for_spawn(Path::new(program), resolve, Path::is_file)
}

/// `exe` failed to start for a spawn: every later spawn under this choice takes
/// the inbox host, and the log hears why once.
pub(crate) fn note_launch_failure(exe: &OpenConsoleExe, why: &str) {
    HOST.lock_or_recover().note_launch_failure(exe, why);
}

/// `exe` said nothing to a spawn's waiting client: that tab took the inbox
/// host, and only the third such silence in a row is kept as a failure
/// (`HostState::note_silent_start`).
pub(crate) fn note_silent_start(exe: &OpenConsoleExe, why: &str) {
    HOST.lock_or_recover().note_silent_start(exe, why);
}

/// `exe` answered a spawn's client.
pub(crate) fn note_started(exe: &OpenConsoleExe) {
    HOST.lock_or_recover().note_started(exe);
}

/// Whether `path` is a GUI-subsystem image (`aterm_types::pe_image`): a program
/// Windows starts with no console, which never connects to the pseudoconsole it
/// is given. A file that cannot be read as an image (none there, a script, the
/// Store's app execution aliases, which cannot be opened) answers `false`.
pub(crate) fn is_gui_image(path: &Path) -> bool {
    use aterm_types::pe_image::{IMAGE_SUBSYSTEM_WINDOWS_GUI, file_subsystem};
    file_subsystem(path) == Some(IMAGE_SUBSYSTEM_WINDOWS_GUI)
}

/// The image path of the process behind `process`, read and nothing else: for
/// a program started through an app execution alias, the packaged program it
/// named.
fn image_of(process: HANDLE) -> Option<PathBuf> {
    let mut buf = vec![0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: a live process handle, a buffer of `len` characters, an in/out length.
    let ok = unsafe { ffi::QueryFullProcessImageNameW(process, 0, buf.as_mut_ptr(), &mut len) };
    (ok != 0).then(|| PathBuf::from(String::from_utf16_lossy(&buf[..len as usize])))
}

/// The console host `choice` gives a new tab, resolved now WITHOUT making it
/// this process's choice, copying anything or starting it: `--show-config`'s
/// `console_host:` row, a print-and-exit report with no side effect. Under
/// `auto` with no bundled host it names the copy of Windows Terminal's
/// OpenConsole.exe a first tab makes ([`console_host_report`] resolves the
/// process's own choice; [`probe_console_host`] starts the host).
#[must_use]
pub fn preview_console_host(choice: &ConsoleHostChoice) -> ConsoleHostReport {
    policy::report_guarded(
        choice,
        &policy::resolve_choice(choice, |choice| resolve_as(choice, false)),
        super::console_guard::available(),
    )
}

/// Resolve an opted-in choice to the OpenConsole it names, or say why the
/// inbox host. Reached through `policy::resolve_choice`, which answers the
/// inbox choice itself.
fn resolve(choice: &ConsoleHostChoice) -> Result<OpenConsoleExe, String> {
    resolve_as(choice, true)
}

/// [`resolve`], copying Windows Terminal's OpenConsole.exe out of its package
/// only when `copy` (a preview names the copy without making it).
fn resolve_as(choice: &ConsoleHostChoice, copy: bool) -> Result<OpenConsoleExe, String> {
    let exe_dir = || {
        std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(Path::to_path_buf))
    };
    match choice {
        ConsoleHostChoice::Inbox => Err(policy::INBOX_REASON.to_string()),
        ConsoleHostChoice::Path(path) => policy::configured(path, Path::is_file, file_version),
        ConsoleHostChoice::Bundled => {
            let path = policy::bundled_source(exe_dir().as_deref(), Path::is_file)?;
            Ok(OpenConsoleExe {
                version: file_version(&path),
                path,
                origin: Origin::Bundled,
            })
        }
        ConsoleHostChoice::Auto => {
            let exe_dir = exe_dir();
            let source = policy::auto_source(
                exe_dir.as_deref(),
                Path::is_file,
                windows_terminal_openconsole,
            )?;
            match source {
                AutoSource::Bundled(path) => Ok(OpenConsoleExe {
                    version: file_version(&path),
                    path,
                    origin: Origin::Bundled,
                }),
                AutoSource::WindowsTerminal { package, source } => {
                    let root = aterm_types::dirs::state_dir().ok_or_else(|| {
                        "no state directory to copy Windows Terminal's OpenConsole.exe into \
                         (LOCALAPPDATA is unset)"
                            .to_string()
                    })?;
                    if !copy {
                        return Ok(OpenConsoleExe {
                            version: file_version(&source),
                            path: root.join("conpty").join(&package).join("OpenConsole.exe"),
                            origin: Origin::WindowsTerminal(package),
                        });
                    }
                    let path =
                        policy::copy_out(&source, &root.join("conpty"), &package).map_err(|e| {
                            format!(
                                "copying Windows Terminal's OpenConsole.exe out of {} failed: {e}",
                                source.display()
                            )
                        })?;
                    Ok(OpenConsoleExe {
                        version: file_version(&path),
                        path,
                        origin: Origin::WindowsTerminal(package),
                    })
                }
            }
        }
    }
}

/// The Windows Terminal package families, stable first.
const WINDOWS_TERMINAL_FAMILIES: &[&str] = &[
    "Microsoft.WindowsTerminal_8wekyb3d8bbwe",
    "Microsoft.WindowsTerminalPreview_8wekyb3d8bbwe",
    "Microsoft.WindowsTerminalCanary_8wekyb3d8bbwe",
];

/// The first installed Windows Terminal package (stable, then Preview, then
/// Canary) whose folder holds OpenConsole.exe, as (full name, file), asked of
/// the package manager (appmodel.h), never by listing `WindowsApps`, which an
/// ordinary user cannot.
fn windows_terminal_openconsole() -> Option<(String, PathBuf)> {
    WINDOWS_TERMINAL_FAMILIES.iter().find_map(|family| {
        packages_of(family).into_iter().find_map(|full_name| {
            let file = package_root(&full_name)?.join("OpenConsole.exe");
            file.is_file().then_some((full_name, file))
        })
    })
}

/// Every installed package of `family`, by full name (empty on any failure).
fn packages_of(family: &str) -> Vec<String> {
    let family_w = wide_nul(OsStr::new(family));
    let mut count = 0u32;
    let mut chars = 0u32;
    // SAFETY: the documented size probe — NULL buffers, valid out-params.
    let rc = unsafe {
        ffi::GetPackagesByPackageFamily(
            family_w.as_ptr(),
            &mut count,
            std::ptr::null_mut(),
            &mut chars,
            std::ptr::null_mut(),
        )
    };
    if (rc != 0 && rc != ffi::ERROR_INSUFFICIENT_BUFFER) || count == 0 {
        return Vec::new();
    }
    let mut names: Vec<*mut u16> = vec![std::ptr::null_mut(); count as usize];
    let mut buf: Vec<u16> = vec![0; chars as usize];
    // SAFETY: both buffers are sized from the probe above.
    let rc = unsafe {
        ffi::GetPackagesByPackageFamily(
            family_w.as_ptr(),
            &mut count,
            names.as_mut_ptr(),
            &mut chars,
            buf.as_mut_ptr(),
        )
    };
    if rc != 0 {
        return Vec::new();
    }
    let base = buf.as_ptr() as usize;
    let end = base + buf.len() * 2;
    names
        .iter()
        .take(count as usize)
        .filter_map(|&p| {
            // Each name points INTO `buf`; read it back as an index so a
            // pointer the API never wrote cannot be followed.
            let at = p as usize;
            if at < base || at >= end {
                return None;
            }
            let start = (at - base) / 2;
            let len = buf[start..].iter().position(|&c| c == 0)?;
            Some(String::from_utf16_lossy(&buf[start..start + len]))
        })
        .collect()
}

/// A package's install folder (`C:\Program Files\WindowsApps\<full name>`).
fn package_root(full_name: &str) -> Option<PathBuf> {
    let name_w = wide_nul(OsStr::new(full_name));
    let mut len = 0u32;
    // SAFETY: the documented size probe — NULL buffer, valid length out-param.
    let rc =
        unsafe { ffi::GetPackagePathByFullName(name_w.as_ptr(), &mut len, std::ptr::null_mut()) };
    if rc != ffi::ERROR_INSUFFICIENT_BUFFER || len == 0 {
        return None;
    }
    let mut path = vec![0u16; len as usize];
    // SAFETY: `path` holds the `len` characters the probe asked for.
    let rc = unsafe { ffi::GetPackagePathByFullName(name_w.as_ptr(), &mut len, path.as_mut_ptr()) };
    if rc != 0 {
        return None;
    }
    let end = path.iter().position(|&c| c == 0).unwrap_or(path.len());
    Some(PathBuf::from(String::from_utf16_lossy(&path[..end])))
}

/// A PE file's numeric version (`a.b.c.d`, from `VS_FIXEDFILEINFO`).
fn file_version(path: &Path) -> Option<String> {
    let path_w = wide_nul(path.as_os_str());
    let mut ignored = 0u32;
    // SAFETY: NUL-terminated path, valid out-param.
    let size = unsafe { ffi::GetFileVersionInfoSizeW(path_w.as_ptr(), &mut ignored) };
    if size == 0 {
        return None;
    }
    let mut block = vec![0u8; size as usize];
    // SAFETY: `block` holds the `size` bytes the size query asked for.
    if unsafe { ffi::GetFileVersionInfoW(path_w.as_ptr(), 0, size, block.as_mut_ptr().cast()) } == 0
    {
        return None;
    }
    let root = wide_nul(OsStr::new("\\"));
    let mut info: *mut c_void = std::ptr::null_mut();
    let mut len = 0u32;
    // SAFETY: `block` is the version resource just read; the answer points into it.
    let ok =
        unsafe { ffi::VerQueryValueW(block.as_ptr().cast(), root.as_ptr(), &mut info, &mut len) };
    if ok == 0 || info.is_null() || (len as usize) < std::mem::size_of::<ffi::VS_FIXEDFILEINFO>() {
        return None;
    }
    // SAFETY: VerQueryValueW("\\") points at a VS_FIXEDFILEINFO inside `block`,
    // which outlives this read; `read_unaligned` makes no alignment claim.
    let fixed = unsafe { std::ptr::read_unaligned(info.cast::<ffi::VS_FIXEDFILEINFO>()) };
    let (ms, ls) = (fixed.dwFileVersionMS, fixed.dwFileVersionLS);
    Some(format!(
        "{}.{}.{}.{}",
        ms >> 16,
        ms & 0xffff,
        ls >> 16,
        ls & 0xffff
    ))
}

/// A running OpenConsole: the three handles that ARE its pseudoconsole (the
/// attribute value's slots, in winconpty's order). Dropping it closes them in
/// winconpty's `_ClosePseudoConsoleMembers` order (the signal pipe first,
/// which makes the host tear its session down, then the reference that kept it
/// alive, then the process handle) and never waits: the fields are declared
/// in that order, and Rust drops fields in declaration order.
pub(crate) struct OpenConsoleHost {
    pub(crate) signal: Handle,
    pub(crate) reference: Handle,
    pub(crate) process: Handle,
}

impl OpenConsoleHost {
    /// The attribute value's three slots, still owned by this host.
    pub(crate) fn slots(&self) -> [HANDLE; 3] {
        [self.signal.raw(), self.reference.raw(), self.process.raw()]
    }

    /// Hand the three handles over (the session owns them from here).
    pub(crate) fn into_raw(self) -> [HANDLE; 3] {
        let OpenConsoleHost {
            signal,
            reference,
            process,
        } = self;
        [signal.into_raw(), reference.into_raw(), process.into_raw()]
    }
}

/// How long a host has to write its first byte once its first client started.
/// The snapshot recorded OpenConsole writing `CSI 1 t CSI c` about 27 ms after
/// the launch when the client starts at once, and a host that rejects its
/// command line exiting in about 20 ms having written nothing.
const HEALTH_WINDOW: Duration = Duration::from_millis(250);

/// Launch `exe` as the host of a new pseudoconsole of `size` on `pipes`:
/// winconpty's `_CreatePseudoConsole`, minus the token and WOW64 branches and
/// minus its driver-load retry (a failure here falls back to the inbox host
/// instead). The host gets duplicates of the pipes' host ends; the caller
/// keeps and closes its own. Whether the host WORKS is only known once a
/// client has connected ([`await_started`]).
pub(crate) fn launch(
    exe: &OpenConsoleExe,
    size: ffi::COORD,
    pipes: &super::Pipes,
) -> Result<OpenConsoleHost, String> {
    let (in_read, out_write) = (pipes.in_read.raw(), pipes.out_write.raw());
    let exe = exe.path.as_path();
    // `\Device\ConDrv\Server`, inheritable (DeviceHandle::CreateServerHandle).
    let server = Handle::new(nt_open(
        r"\Device\ConDrv\Server",
        0,
        ffi::GENERIC_ALL,
        ffi::OBJ_CASE_INSENSITIVE | ffi::OBJ_INHERIT,
        0,
    )?);
    // `\Reference` under it, NOT inheritable (DeviceHandle::CreateClientHandle):
    // the client gets its connection through the attribute, never by inheritance.
    let reference = Handle::new(nt_open(
        r"\Reference",
        server.raw(),
        ffi::GENERIC_READ | ffi::GENERIC_WRITE | ffi::SYNCHRONIZE,
        ffi::OBJ_CASE_INSENSITIVE,
        ffi::FILE_SYNCHRONOUS_IO_NONALERT,
    )?);
    let (mut sig_host, mut sig_ours): (HANDLE, HANDLE) = (0, 0);
    // SAFETY: valid out-params, default attributes and size.
    if unsafe { ffi::CreatePipe(&mut sig_host, &mut sig_ours, std::ptr::null_mut(), 0) } == 0 {
        return Err(format!("signal pipe: {}", io::Error::last_os_error()));
    }
    let sig_host = Handle::new(sig_host);
    let signal = Handle::new(sig_ours);
    // SAFETY: our own live handle.
    if unsafe {
        ffi::SetHandleInformation(
            sig_host.raw(),
            ffi::HANDLE_FLAG_INHERIT,
            ffi::HANDLE_FLAG_INHERIT,
        )
    } == 0
    {
        return Err(format!("signal pipe: {}", io::Error::last_os_error()));
    }
    let host_in = Handle::new(dup_inheritable(in_read)?);
    let host_out = Handle::new(dup_inheritable(out_write)?);

    let mut cmdline = host_command_line(exe, size, sig_host.raw(), server.raw());
    let exe_w = wide_nul(exe.as_os_str());
    let mut inherited: [HANDLE; 4] = [server.raw(), host_in.raw(), host_out.raw(), sig_host.raw()];
    let mut attr = super::AttrList::new(1).map_err(|e| format!("attribute list: {e}"))?;
    // SAFETY: an initialized one-slot list; the handle array outlives CreateProcessW.
    let updated = unsafe {
        ffi::UpdateProcThreadAttribute(
            attr.ptr(),
            0,
            ffi::PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
            inherited.as_mut_ptr().cast(),
            std::mem::size_of_val(&inherited),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if updated == 0 {
        return Err(format!("handle list: {}", io::Error::last_os_error()));
    }
    // SAFETY: a zeroed STARTUPINFOEXW is valid; the fields set below are all it needs.
    let mut si: ffi::STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    si.StartupInfo.cb = std::mem::size_of::<ffi::STARTUPINFOEXW>() as u32;
    si.StartupInfo.hStdInput = host_in.raw();
    si.StartupInfo.hStdOutput = host_out.raw();
    si.StartupInfo.hStdError = host_out.raw();
    si.StartupInfo.dwFlags = ffi::STARTF_USESTDHANDLES;
    si.lpAttributeList = attr.ptr();
    // SAFETY: POD out-memory.
    let mut pi: ffi::PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: NUL-terminated buffers and the attribute list live across the call.
    let created = unsafe {
        ffi::CreateProcessW(
            exe_w.as_ptr(),
            cmdline.as_mut_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            1, // inherit — only the four listed handles
            ffi::EXTENDED_STARTUPINFO_PRESENT,
            std::ptr::null_mut(),
            std::ptr::null(),
            &si.StartupInfo,
            &mut pi,
        )
    };
    if created == 0 {
        return Err(format!("CreateProcessW: {}", io::Error::last_os_error()));
    }
    drop(Handle::new(pi.hThread));
    // The host holds its own copies of the server handle, the pipe ends and
    // the signal pipe's far end now; ours go (winconpty's unique_handles do the same).
    drop((server, host_in, host_out, sig_host));
    Ok(OpenConsoleHost {
        signal,
        reference,
        process: Handle::new(pi.hProcess),
    })
}

/// What the health check found ([`await_started`]).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Health {
    /// The host wrote its first VT byte: it works.
    Started,
    /// The client never connects (a GUI image) or exited before the host said
    /// anything (a console program whose loader failed): nothing the host did,
    /// and nothing to retry. The session goes on in this host, and its waiter
    /// reports the program's exit.
    NoClient,
    /// The host failed: it exited or wrote something other than VT (`sticky`:
    /// it will not work for a later tab either), or said nothing within
    /// [`HEALTH_WINDOW`] while it and its client both ran.
    Failed { why: String, sticky: bool },
}

/// The health check: once `client` has been started on `host`, the host must
/// write its first byte within [`HEALTH_WINDOW`]. The snapshot recorded
/// (2026-09-29) OpenConsole writing NOTHING before a client connects (with the
/// shell still suspended, 250 ms passed in silence), and on the connect writing
/// `CSI 1 t CSI c` and holding the client until that DA1 is answered (3 s when
/// it is not). A console-subsystem client connects as its process starts,
/// before any code of its own runs, so until this byte such a client has run
/// nothing of its own: a host that fails here leaves nothing behind that a
/// retry on the inbox host would run twice. That holds for nothing else. A
/// GUI-subsystem image never connects (Windows connects console images only),
/// so its host never speaks while the program runs freely, and a client that
/// exits before it connects leaves the host nothing to answer: both are
/// [`Health::NoClient`], never a failure that would end the program and run it
/// again. On failure the host is terminated.
pub(crate) fn await_started(host: &OpenConsoleHost, client: HANDLE, out_read: HANDLE) -> Health {
    // The image the client really runs, an app execution alias's target
    // included (a spawn checks the program it names before it picks a host).
    if image_of(client).is_some_and(|image| is_gui_image(&image)) {
        return Health::NoClient;
    }
    let health = await_first_output(host.process.raw(), client, out_read, HEALTH_WINDOW);
    if let Health::Failed { .. } = health {
        // SAFETY: the host we launched, by its handle; a dead one ignores the call.
        unsafe { ffi::TerminateProcess(host.process.raw(), 1) };
    }
    health
}

/// Start `command_line` (NUL-terminated) as a client of the pseudoconsole whose
/// three slots are `slots`, with its stdio bound to it: the second-client lane
/// the alternate-screen resync and the doctor's probe use. Returns the
/// client's process handle.
pub(crate) fn start_client(
    slots: &mut [HANDLE; 3],
    command_line: &mut [u16],
) -> io::Result<Handle> {
    let mut attr = super::AttrList::new(1)?;
    // SAFETY: initialized one-slot list; `slots` outlives CreateProcessW.
    let updated = unsafe {
        ffi::UpdateProcThreadAttribute(
            attr.ptr(),
            0,
            ffi::PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
            slots.as_mut_ptr().cast(),
            std::mem::size_of::<isize>(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if updated == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a zeroed STARTUPINFOEXW is valid; cb, flags and the list are set.
    let mut si: ffi::STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    si.StartupInfo.cb = std::mem::size_of::<ffi::STARTUPINFOEXW>() as u32;
    // NULL std handles claimed on purpose, as the shell's spawn does: the
    // client's stdio then binds to the pseudoconsole, never to ours.
    si.StartupInfo.dwFlags = ffi::STARTF_USESTDHANDLES;
    si.lpAttributeList = attr.ptr();
    // SAFETY: POD out-memory.
    let mut pi: ffi::PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: the command line is NUL-terminated and, like the list, outlives the call.
    let created = unsafe {
        ffi::CreateProcessW(
            std::ptr::null(),
            command_line.as_mut_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
            ffi::EXTENDED_STARTUPINFO_PRESENT,
            std::ptr::null_mut(),
            std::ptr::null(),
            &si.StartupInfo,
            &mut pi,
        )
    };
    if created == 0 {
        return Err(io::Error::last_os_error());
    }
    drop(Handle::new(pi.hThread));
    Ok(Handle::new(pi.hProcess))
}

/// OpenConsole's command line: the quoted path, then `policy::host_arguments`
/// (the five stable arguments), NUL-terminated.
fn host_command_line(exe: &Path, size: ffi::COORD, signal: HANDLE, server: HANDLE) -> Vec<u16> {
    let mut line: Vec<u16> = vec![u16::from(b'"')];
    line.extend(exe.as_os_str().encode_wide());
    line.push(u16::from(b'"'));
    let rest = policy::host_arguments(size.x, size.y, signal as usize, server as usize);
    line.extend(rest.encode_utf16());
    line.push(0);
    line
}

/// Wait for the host's first output byte (left in the pipe for the session
/// reader). The host fails when it exits first or speaks something other than
/// VT (a console host's first byte is the ESC of its `CSI 1 t`, while a program
/// that is not one, a wrong `conpty` path, writes an error message into the
/// pipe it was handed as stdout), and when it says nothing within `window`
/// while its client still runs. A client that exits before the host has said
/// anything, the host still running, is [`Health::NoClient`]: it never
/// connected. Both exited is the host's failure: a host that dies takes its
/// waiting client with it.
fn await_first_output(host: HANDLE, client: HANDLE, out_read: HANDLE, window: Duration) -> Health {
    let start = Instant::now();
    let failed = |why: String, sticky: bool| Health::Failed { why, sticky };
    let exited = |process: HANDLE| {
        // SAFETY: a handle the caller holds open across this wait.
        unsafe { ffi::WaitForSingleObject(process, 0) == ffi::WAIT_OBJECT_0 }
    };
    loop {
        // Asked before the peek, so output the host wrote before the client
        // went is seen as the start it is.
        let client_gone = exited(client);
        let mut first = [0u8; 1];
        let (mut got, mut avail) = (0u32, 0u32);
        // SAFETY: a peek of our own live pipe end into a 1-byte local; nothing
        // is consumed.
        let peeked = unsafe {
            ffi::PeekNamedPipe(
                out_read,
                first.as_mut_ptr(),
                1,
                &mut got,
                &mut avail,
                std::ptr::null_mut(),
            )
        };
        if peeked != 0 && got == 1 && first[0] != 0x1b {
            let why = format!(
                "its first output byte was 0x{:02x}, not the ESC of a console host's VT",
                first[0]
            );
            return failed(why, true);
        }
        if exited(host) {
            let mut code = 0u32;
            // SAFETY: valid handle + out-param.
            unsafe { ffi::GetExitCodeProcess(host, &mut code) };
            let why = format!(
                "it exited with code 0x{code:08x} after {} ms",
                start.elapsed().as_millis()
            );
            return failed(why, true);
        }
        if peeked != 0 && avail > 0 {
            return Health::Started;
        }
        if client_gone {
            return Health::NoClient;
        }
        if start.elapsed() >= window {
            return failed(
                format!("it wrote nothing in {} ms", window.as_millis()),
                false,
            );
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// `NtOpenFile` of an NT object path, relative to `root` when non-zero.
fn nt_open(
    name: &str,
    root: HANDLE,
    access: u32,
    attributes: u32,
    options: u32,
) -> Result<HANDLE, String> {
    let mut name_w: Vec<u16> = name.encode_utf16().collect();
    let bytes = (name_w.len() * 2) as u16;
    let mut us = ffi::UNICODE_STRING {
        Length: bytes,
        MaximumLength: bytes,
        Buffer: name_w.as_mut_ptr(),
    };
    let mut oa = ffi::OBJECT_ATTRIBUTES {
        Length: std::mem::size_of::<ffi::OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: root,
        ObjectName: &mut us,
        Attributes: attributes,
        SecurityDescriptor: std::ptr::null_mut(),
        SecurityQualityOfService: std::ptr::null_mut(),
    };
    let mut iosb = ffi::IO_STATUS_BLOCK {
        Status: 0,
        Information: 0,
    };
    let mut handle: HANDLE = 0;
    // SAFETY: every pointer is a live local for the duration of the call.
    let status = unsafe {
        ffi::NtOpenFile(
            &mut handle,
            access,
            &mut oa,
            &mut iosb,
            ffi::FILE_SHARE_ALL,
            options,
        )
    };
    if status < 0 {
        return Err(format!(
            "NtOpenFile({name}) NTSTATUS 0x{:08x}",
            status as u32
        ));
    }
    Ok(handle)
}

/// An inheritable duplicate of one of our own handles.
fn dup_inheritable(h: HANDLE) -> Result<HANDLE, String> {
    let mut out: HANDLE = 0;
    // SAFETY: duplicating our own live handle within our own process.
    let ok = unsafe {
        ffi::DuplicateHandle(
            ffi::GetCurrentProcess(),
            h,
            ffi::GetCurrentProcess(),
            &mut out,
            0,
            1,
            ffi::DUPLICATE_SAME_ACCESS,
        )
    };
    if ok == 0 {
        return Err(format!("DuplicateHandle: {}", io::Error::last_os_error()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The full command line: the path quoted, then the five stable arguments,
    // NUL-terminated.
    #[test]
    fn the_host_command_line_quotes_the_path() {
        let line = host_command_line(
            Path::new(r"C:\Program Files\aterm\OpenConsole.exe"),
            ffi::COORD { x: 120, y: 30 },
            0x2c4,
            0x1a8,
        );
        assert_eq!(line.last(), Some(&0), "NUL-terminated");
        assert_eq!(
            String::from_utf16_lossy(&line[..line.len() - 1]),
            "\"C:\\Program Files\\aterm\\OpenConsole.exe\" --headless --width 120 --height 30 \
             --signal 0x2c4 --server 0x1a8"
        );
    }

    // kernelbase reads the attribute value as three pointer-sized slots at
    // offsets 0, 8 and 16: the layout `OpenConsoleHost::into_raw` hands over.
    #[test]
    fn the_pseudoconsole_slots_are_three_pointers() {
        let slots: [HANDLE; 3] = [1, 2, 3];
        assert_eq!(std::mem::size_of_val(&slots), 24);
        let base = slots.as_ptr() as usize;
        assert_eq!(std::ptr::addr_of!(slots[1]) as usize - base, 8);
        assert_eq!(std::ptr::addr_of!(slots[2]) as usize - base, 16);
    }

    /// An event standing in for a process handle: signalled is exited.
    fn process_standin(exited: bool) -> Handle {
        // SAFETY: NULL attributes and name; manual-reset, so a signal stays.
        let event = unsafe {
            ffi::CreateEventW(std::ptr::null_mut(), 1, i32::from(exited), std::ptr::null())
        };
        assert_ne!(event, 0);
        Handle::new(event)
    }

    /// Pipes whose output end holds `bytes`, as a host's first write left them.
    fn output_holding(bytes: &[u8]) -> super::super::Pipes {
        let pipes = super::super::Pipes::new().expect("pipes");
        if !bytes.is_empty() {
            let mut wrote = 0u32;
            // SAFETY: our own live pipe end and a buffer of the stated length.
            let ok = unsafe {
                ffi::WriteFile(
                    pipes.out_write.raw(),
                    bytes.as_ptr(),
                    bytes.len() as u32,
                    &mut wrote,
                    std::ptr::null_mut(),
                )
            };
            assert!(ok != 0 && wrote as usize == bytes.len());
        }
        pipes
    }

    // The check tells the host's failures from a client that never connected.
    // A host that speaks VT started; one that exits, or writes anything else,
    // failed for good; one silent while its client waits failed for this tab
    // only. A client gone with the host still up and silent never connected
    // (its loader failed): no failure, no retry. Both gone is the host's.
    #[test]
    fn the_health_check_blames_the_host_only_for_what_the_host_did() {
        let window = Duration::from_millis(30);
        let check = |host_exited: bool, client_exited: bool, out: &[u8]| {
            let (host, client) = (process_standin(host_exited), process_standin(client_exited));
            let pipes = output_holding(out);
            await_first_output(host.raw(), client.raw(), pipes.out_read.raw(), window)
        };
        assert_eq!(check(false, false, b"\x1b[1t"), Health::Started);
        assert_eq!(
            check(false, true, b"\x1b[1t"),
            Health::Started,
            "it answered before the client went"
        );
        assert_eq!(check(false, true, b""), Health::NoClient);
        assert!(
            matches!(check(false, false, b""), Health::Failed { ref why, sticky: false } if why == "it wrote nothing in 30 ms")
        );
        assert!(matches!(
            check(true, false, b""),
            Health::Failed { sticky: true, .. }
        ));
        assert!(matches!(
            check(true, true, b""),
            Health::Failed { sticky: true, .. }
        ));
        assert!(
            matches!(check(false, false, b"usage"), Health::Failed { ref why, sticky: true } if why.contains("0x75"))
        );
    }

    // The subsystem of real images: a console program and a windowed one.
    #[test]
    fn a_gui_image_is_told_from_a_console_one() {
        let root = std::path::PathBuf::from(std::env::var_os("SystemRoot").expect("SystemRoot"));
        assert!(!is_gui_image(&root.join("System32").join("cmd.exe")));
        assert!(is_gui_image(&root.join("explorer.exe")));
        assert!(!is_gui_image(&root.join("no-such-program.exe")));
        assert!(!is_gui_image(
            &root
                .join("System32")
                .join("drivers")
                .join("etc")
                .join("hosts")
        ));
    }

    #[test]
    fn a_system_command_names_its_program_by_full_path() {
        let line = system_command("cmd.exe", "/d /c exit");
        let text = String::from_utf16_lossy(&line[..line.len() - 1]);
        assert!(text.starts_with('"'), "{text}");
        assert!(
            text.to_ascii_lowercase()
                .ends_with("\\system32\\cmd.exe\" /d /c exit"),
            "{text}"
        );
    }
}
