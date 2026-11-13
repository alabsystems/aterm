// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Thread scheduling roles, changed by the worker that owns them.

/// What a thread's work is worth relative to the human at the keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    /// The human is watching this land right now: input egress, and anything else on the keystroke→glass path.
    Interactive,
    /// The human asked for it and is waiting, but not frame-by-frame: PTY drain and parse, reflow.
    Responsive,
    /// Useful work nobody is blocked on: font warming, encoding, log flushing, package/update probes.
    Background,
    /// Work with no deadline at all, which may run arbitrarily late: opportunistic disk sweeps and prefetch.
    Housekeeping,
}

#[cfg(target_os = "macos")]
const fn qos_class(role: Role) -> libc::qos_class_t {
    match role {
        Role::Interactive => libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE,
        Role::Responsive => libc::qos_class_t::QOS_CLASS_USER_INITIATED,
        Role::Background => libc::qos_class_t::QOS_CLASS_UTILITY,
        Role::Housekeeping => libc::qos_class_t::QOS_CLASS_BACKGROUND,
    }
}

/// How far `role` outranks the others: the order [`Role`] declares, highest first.
const fn rank(role: Role) -> u8 {
    match role {
        Role::Interactive => 3,
        Role::Responsive => 2,
        Role::Background => 1,
        Role::Housekeeping => 0,
    }
}

/// `role`, or the role a process is [`held`] at where `role` ranks above it
/// ([`hold_process_as_background_work`], [`hold_process_as_housekeeping`]).
fn capped(role: Role, held: Option<Role>) -> Role {
    match held {
        Some(at) if rank(role) > rank(at) => at,
        _ => role,
    }
}

/// The role this process is held at, if a hold holds it ([`hold_process_as_background_work`],
/// [`hold_process_as_housekeeping`]).
fn held() -> Option<Role> {
    #[cfg(target_os = "macos")]
    {
        hold::held()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// Hold this whole process as background work (the utility class), thread by thread, where a
/// launchd job of `ProcessType` `Standard` clamped it: the calling thread at once, every thread
/// the process starts from here on as it starts, each role a thread declares (`qos::set_self`)
/// and each child started on a role's behalf (`qos::command`) capped there. Unlike the clamp, the hold
/// leaves a lift ([`aterm_uds::qos::lift`]) its effect, so the one thread a waiter waits on (the
/// fleet book's lock holder, a forking thread) runs at the default class while it holds what is
/// waited on. `aterm node serve` calls it first, before it starts a thread, and the converge
/// pass's runner (`aterm init --converge`) as its pass starts: a thread started earlier keeps
/// its class. A process is held once: the first hold stands. A no-op off macOS.
pub fn hold_process_as_background_work() {
    #[cfg(target_os = "macos")]
    hold::install(Role::Background);
}

/// [`hold_process_as_background_work`], at the background class (housekeeping) in place of
/// utility: the priority (4) a launchd job of `ProcessType` `Background` ran every thread at, so
/// its work costs what it cost there, but a lift still lifts. The vault subscriber's converge
/// runner (`aterm init --converge --trigger subscriber`) holds itself so as its pass starts,
/// having left that job's band. Measured on macOS 27.0, one fixed piece of CPU work (1.8 G
/// instructions) in children of private-label `Background` jobs, twelve rounds side by side: in
/// the band 111-194 mJ and 0.9-2.1 s of CPU, out of it at the background class 112-174 mJ and
/// 1.1-2.1 s round for round, and at utility 864-1348 mJ in 0.49-0.66 s. A no-op off macOS.
pub fn hold_process_as_housekeeping() {
    #[cfg(target_os = "macos")]
    hold::install(Role::Housekeeping);
}

/// Holding every thread of a process at one role's class as it starts.
#[cfg(target_os = "macos")]
mod hold {
    use std::ffi::c_void;
    use std::sync::{Once, OnceLock};

    use super::{Role, qos_class};

    /// `pthread_introspection_hook_t` (`<pthread/introspection.h>`).
    type Hook = unsafe extern "C" fn(
        event: libc::c_uint,
        thread: libc::pthread_t,
        addr: *mut c_void,
        size: usize,
    );

    /// `PTHREAD_INTROSPECTION_THREAD_START` (`<pthread/introspection.h>`): the event a thread
    /// gets on itself as it starts, before its start routine runs.
    const THREAD_START: libc::c_uint = 2;

    unsafe extern "C" {
        fn pthread_introspection_hook_install(hook: Option<Hook>) -> Option<Hook>;
    }

    /// The hook that held this process's thread events before [`start_held`] (none in aterm, a
    /// sanitizer's where one runs), set once [`start_held`] is installed: every event is handed
    /// on to it, as `pthread_introspection_hook_install` keeps one hook alone.
    static CHAINED: OnceLock<Option<Hook>> = OnceLock::new();

    /// The role this process is held at, set by the first [`install`] before its hook is.
    static AT: OnceLock<Role> = OnceLock::new();

    /// The role [`install`] holds this process at, once it has run.
    pub(super) fn held() -> Option<Role> {
        AT.get().copied()
    }

    /// Hold this process at `role` (the first install's role stands), install [`start_held`]
    /// once, and put the calling thread at the held class. Darwin's `pthread_create` gives a new
    /// thread the default class whatever class its creator runs at (measured on macOS 27.0: a
    /// thread started by one at background or utility read 0x15), so the hook sets each one's
    /// class on that thread as it starts.
    pub(super) fn install(role: Role) {
        let at = *AT.get_or_init(|| role);
        static INSTALL: Once = Once::new();
        INSTALL.call_once(|| {
            // SAFETY: installs a plain function that lives as long as the process and takes
            // nothing from this frame; it hands each event to the hook it replaced once
            // `CHAINED` holds that hook (an event in the instant before is not handed on).
            let chained = unsafe { pthread_introspection_hook_install(Some(start_held)) };
            let _ = CHAINED.set(chained);
        });
        // SAFETY: sets this thread's own class; takes no pointers and mutates no shared state.
        unsafe { libc::pthread_set_qos_class_self_np(qos_class(at), 0) };
    }

    /// [`install`]'s hook: a starting thread's class set to the held role's ([`AT`]) on that
    /// thread, then every event handed to the hook it replaced ([`CHAINED`]). It may run where a
    /// thread's state is not whole (its end, its teardown), so it reads two atomics and makes one
    /// call, for a start alone. A dispatch worker's item still runs at the class its queue gives
    /// it, which libdispatch sets on that thread for each item.
    unsafe extern "C" fn start_held(
        event: libc::c_uint,
        thread: libc::pthread_t,
        addr: *mut c_void,
        size: usize,
    ) {
        if event == THREAD_START
            && let Some(&at) = AT.get()
        {
            // SAFETY: a start's event runs on the starting thread itself, whose own class this
            // sets; it takes no pointers and mutates no shared state.
            unsafe { libc::pthread_set_qos_class_self_np(qos_class(at), 0) };
        }
        if let Some(Some(chained)) = CHAINED.get() {
            // SAFETY: `chained` is the hook the install returned, a function of this signature
            // that lives as long as the process, given this event's own arguments.
            unsafe { chained(event, thread, addr, size) };
        }
    }
}

/// Declare the calling thread's role, [`capped`] in a held process.
#[inline]
pub(crate) fn set_self(role: Role) {
    let role = capped(role, held());
    #[cfg(target_os = "macos")]
    {
        fork_at_default::install();
        // SAFETY: sets this thread's own QoS class. Takes no pointers and
        // mutates no shared state, so there is nothing for another thread to
        // observe.
        unsafe {
            libc::pthread_set_qos_class_self_np(qos_class(role), 0);
        }
    }
    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    if let Some(slice) = linux_slice_ns(role) {
        let _ = set_self_slice(slice);
    }
    #[cfg(not(any(
        target_os = "macos",
        all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64")
        )
    )))]
    let _ = role;
}

/// One role transition, observed on the thread that made it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RoleObservation {
    pub(crate) requested: Role,
    pub(crate) observed: Option<Role>,
    pub(crate) class: Option<u32>,
    pub(crate) set_errno: Option<i32>,
    pub(crate) read_errno: Option<i32>,
}

/// Set and immediately read this thread's native role, retaining native errors: `role`
/// [`capped`] as [`set_self`] caps it, which is the role it reports requested.
pub(crate) fn set_self_observed(role: Role) -> RoleObservation {
    let role = capped(role, held());
    #[cfg(target_os = "macos")]
    {
        fork_at_default::install();
        // SAFETY: both calls name the calling thread. The setter takes no
        // pointers; the getter writes into live, correctly typed locals.
        let (set, read, class) = unsafe {
            let set = libc::pthread_set_qos_class_self_np(qos_class(role), 0);
            let (mut class, mut relative) = (0, 0);
            let read = pthread_get_qos_class_np(
                pthread_self() as libc::pthread_t,
                &mut class,
                &mut relative,
            );
            (set, read, class)
        };
        let observed = (read == 0)
            .then(|| {
                [
                    Role::Interactive,
                    Role::Responsive,
                    Role::Background,
                    Role::Housekeeping,
                ]
                .into_iter()
                .find(|candidate| qos_class(*candidate) as u32 == class)
            })
            .flatten();
        RoleObservation {
            requested: role,
            observed,
            class: (read == 0).then_some(class),
            set_errno: Some(set),
            read_errno: Some(read),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        set_self(role);
        RoleObservation {
            requested: role,
            observed: None,
            class: None,
            set_errno: None,
            read_errno: None,
        }
    }
}

/// Spawn the named worker thread `name`, declaring its initial `role` before its body runs, on that thread.
pub(crate) fn spawn_lane_thread<F, T>(
    name: impl Into<String>,
    role: Role,
    f: F,
) -> std::io::Result<std::thread::JoinHandle<T>>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            set_self(role);
            f()
        })
}

#[cfg(target_os = "macos")]
mod fork_at_default {
    use std::cell::Cell;

    unsafe extern "C" {
        fn pthread_atfork(
            prepare: Option<extern "C" fn()>,
            parent: Option<extern "C" fn()>,
            child: Option<extern "C" fn()>,
        ) -> libc::c_int;
    }

    thread_local! {
        /// What [`lift`] raised this thread from, for the fork in flight on it; `None` when it
        /// raised nothing.
        static LIFTED_FROM: Cell<Option<aterm_uds::qos::Lifted>> = const { Cell::new(None) };
    }

    /// Bracket every fork in the process with [`lift`] and [`restore`].
    pub(super) fn install() {
        static INSTALLED: std::sync::Once = std::sync::Once::new();
        INSTALLED.call_once(|| {
            // SAFETY: registers two `extern "C"` functions that live as long as the process; neither unwinds (they cannot panic).
            unsafe { pthread_atfork(Some(lift), Some(restore), None) };
        });
    }

    /// The prepare handler: on the forking thread, before `fork(2)`.
    extern "C" fn lift() {
        LIFTED_FROM.set(aterm_uds::qos::lift());
    }

    /// The parent handler: on the forking thread, once `fork(2)` has returned.
    extern "C" fn restore() {
        if let Some(lifted) = LIFTED_FROM.take() {
            aterm_uds::qos::restore(lifted);
        }
    }
}

/// The EEVDF slice request for `role`, in ns, or `None` for the kernel default.
#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ),
    test
))]
const fn linux_slice_ns(role: Role) -> Option<u64> {
    match role {
        Role::Interactive => Some(500_000),
        Role::Responsive => Some(1_000_000),
        Role::Background | Role::Housekeeping => None,
    }
}

/// `struct sched_attr` (`include/uapi/linux/sched/types.h`, VER1 — 56 bytes).
#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ),
    test
))]
#[repr(C)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct SchedAttr {
    size: u32,
    sched_policy: u32,
    sched_flags: u64,
    sched_nice: i32,
    sched_priority: u32,
    sched_runtime: u64,
    sched_deadline: u64,
    sched_period: u64,
    sched_util_min: u32,
    sched_util_max: u32,
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const SYS_SCHED_SETATTR: libc::c_long = 314;
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const SYS_SCHED_SETATTR: libc::c_long = 274;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const SYS_SCHED_GETATTR: libc::c_long = 315;
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const SYS_SCHED_GETATTR: libc::c_long = 275;
/// `SCHED_FLAG_RESET_ON_FORK`.
#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ),
    test
))]
const SCHED_FLAG_RESET_ON_FORK: u64 = 0x01;

/// The fair-class policies (`SCHED_OTHER`, `SCHED_BATCH`, `SCHED_IDLE`) — the
/// only ones whose `sched_runtime` is an EEVDF slice request.
#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ),
    test
))]
const FAIR_POLICIES: [u32; 3] = [0, 3, 5];

/// The `sched_attr` that asks for `slice_ns` while changing nothing else about
/// `current`: its policy, nice and priority are carried over.
#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ),
    test
))]
fn slice_request(current: &SchedAttr, slice_ns: u64) -> Option<SchedAttr> {
    (FAIR_POLICIES.contains(&current.sched_policy) && current.sched_nice >= 0).then(|| SchedAttr {
        size: std::mem::size_of::<SchedAttr>() as u32,
        sched_policy: current.sched_policy,
        sched_flags: SCHED_FLAG_RESET_ON_FORK,
        sched_nice: current.sched_nice,
        sched_priority: current.sched_priority,
        sched_runtime: slice_ns,
        ..SchedAttr::default()
    })
}

/// Request `slice_ns` for the calling thread (tid 0): read its attributes, change
/// only the slice ([`slice_request`]), write them back.
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
fn set_self_slice(slice_ns: u64) -> std::io::Result<()> {
    let current = sched_attr_of_self()?;
    let Some(attr) = slice_request(&current, slice_ns) else {
        return Ok(());
    };
    // SAFETY: `sched_setattr(0, &attr, 0)` reads one correctly sized, live `sched_attr` and changes only the calling thread's scheduling hint.
    let rc = unsafe {
        libc::syscall(
            SYS_SCHED_SETATTR,
            0 as libc::c_long,
            &raw const attr,
            0 as libc::c_long,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// The calling thread's `sched_attr`, read back from the kernel.
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
fn sched_attr_of_self() -> std::io::Result<SchedAttr> {
    let mut attr = SchedAttr::default();
    // SAFETY: `sched_getattr(0, &mut attr, size, 0)` writes at most `size` bytes into the live, correctly sized local.
    let rc = unsafe {
        libc::syscall(
            SYS_SCHED_GETATTR,
            0 as libc::c_long,
            &raw mut attr,
            std::mem::size_of::<SchedAttr>() as libc::c_long,
            0 as libc::c_long,
        )
    };
    if rc == 0 {
        Ok(attr)
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// The slice (ns) and flags the calling thread runs with: the Linux twin of `class_of_self`.
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    test
))]
pub(crate) fn slice_of_self() -> Option<(u64, u64)> {
    sched_attr_of_self()
        .ok()
        .map(|attr| (attr.sched_runtime, attr.sched_flags))
}

/// macOS's QoS launcher — the one `aterm-verify` wraps compile-only children in.
const TASKPOLICY: &str = aterm_uds::os_path!("taskpolicy");

/// The `taskpolicy -c` clamp for a child spawned on behalf of `role`, or `None`
/// when the child keeps the tier it inherits.
const fn child_clamp(role: Role) -> Option<&'static str> {
    match role {
        Role::Interactive | Role::Responsive => None,
        Role::Background => Some("utility"),
        Role::Housekeeping => Some("background"),
    }
}

/// A `Command` for `program`, run on behalf of `role`'s work ([`capped`] in a held process): behind `taskpolicy -c <clamp>` whenever that work ranks below the inherited default, so the child and everything it launches yield to the program being typed into (under `taskpolicy -c utility` a child and its own child both read pri 20, against 31 bare).
pub(crate) fn command(
    role: Role,
    program: impl AsRef<std::path::Path>,
) -> aterm_uds::spawnfd::Command {
    command_with(
        capped(role, held()),
        program.as_ref(),
        std::path::Path::new(TASKPOLICY),
    )
}

/// THE way the GUI starts the co-located `atpkg`: a [`Role::Background`] [`command`], so a package pass — parsing, verifying, unpacking — never runs in the band of the program being typed into.
pub(crate) fn atpkg_command(atpkg: impl AsRef<std::path::Path>) -> aterm_uds::spawnfd::Command {
    command(Role::Background, atpkg)
}

/// The package lane's pass child: [`atpkg_command`], except for the FIRST RUN a person waits on (`waited`: the store's first install of the ALab toolset still ahead, `crate::pass_is_waited_on`): told to wait for the toolset, with stubs on PATH blocking until it lands.
pub(crate) fn atpkg_pass_command(
    atpkg: impl AsRef<std::path::Path>,
    waited: bool,
) -> aterm_uds::spawnfd::Command {
    command(
        if waited {
            Role::Responsive
        } else {
            Role::Background
        },
        atpkg,
    )
}

/// [`command`] over an explicit launcher, so the wrapping decision is testable on
/// every Unix against a stand-in.
fn command_with(
    role: Role,
    program: &std::path::Path,
    taskpolicy: &std::path::Path,
) -> aterm_uds::spawnfd::Command {
    match child_clamp(role) {
        Some(clamp)
            if program.is_absolute()
                && is_executable_file(program)
                && is_executable_file(taskpolicy) =>
        {
            let mut wrapped = aterm_uds::spawnfd::command(taskpolicy);
            wrapped.arg("-c").arg(clamp).arg(program);
            wrapped
        }
        _ => aterm_uds::spawnfd::command(program),
    }
}

fn is_executable_file(path: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::metadata(path)
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        false
    }
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn pthread_self() -> *mut std::ffi::c_void;
    fn pthread_override_qos_class_start_np(
        thread: libc::pthread_t,
        class: libc::qos_class_t,
        relative_priority: libc::c_int,
    ) -> *mut std::ffi::c_void;
    fn pthread_override_qos_class_end_np(over: *mut std::ffi::c_void) -> libc::c_int;
}

/// A worker raised to [`Role::Responsive`] until this drops. The owner retains the worker's
/// unjoined handle until the override ends.
pub(crate) struct Boost {
    #[cfg(target_os = "macos")]
    over: Option<std::ptr::NonNull<std::ffi::c_void>>,
}

// SAFETY: the override is an opaque libSystem object that any thread may end (`pthread_override_qos_class_end_np`'s contract), and only the one owner of this value ever ends it.
#[cfg(target_os = "macos")]
unsafe impl Send for Boost {}

impl Boost {
    /// Raise `thread`, a worker not yet joined, for the lifetime of a demand.
    pub(crate) fn start(thread: Option<&std::thread::JoinHandle<()>>) -> Boost {
        #[cfg(target_os = "macos")]
        {
            use std::os::unix::thread::JoinHandleExt as _;
            let over = thread.and_then(|thread| {
                let pthread = thread.as_pthread_t() as libc::pthread_t;
                // SAFETY: the borrowed handle is not joined, so `pthread` names a thread whose handle stays valid across the call.
                std::ptr::NonNull::new(unsafe {
                    pthread_override_qos_class_start_np(pthread, qos_class(Role::Responsive), 0)
                })
            });
            Boost { over }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = thread;
            Boost {}
        }
    }

    /// Whether an override was started.
    #[cfg(target_os = "macos")]
    pub(crate) fn armed(&self) -> bool {
        self.over.is_some()
    }
}

#[cfg(target_os = "macos")]
impl Drop for Boost {
    fn drop(&mut self) {
        if let Some(over) = self.over.take() {
            // SAFETY: `over` came from a successful start and is ended once.
            unsafe { pthread_override_qos_class_end_np(over.as_ptr()) };
        }
    }
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    #[cfg(test)]
    fn qos_class_self() -> libc::c_uint;
    fn pthread_get_qos_class_np(
        thread: libc::pthread_t,
        class: *mut libc::c_uint,
        relative_priority: *mut libc::c_int,
    ) -> libc::c_int;
}

/// The class the calling thread is actually running at.
#[cfg(all(test, target_os = "macos"))]
pub(crate) fn class_of_self() -> u32 {
    // SAFETY: reads the calling thread's own class. Takes no pointers and mutates no shared state.
    unsafe { qos_class_self() }
}

/// The class another thread has requested, named by its handle, or `None` if the platform declined to report one.
#[cfg(all(test, target_os = "macos"))]
pub(crate) fn class_of<T>(thread: &std::thread::JoinHandle<T>) -> Option<u32> {
    aterm_uds::qos::requested_class_of(thread).map(|(class, _)| class)
}

/// The class [`set_self`] puts a thread at, to compare against one read back off the running thread.
#[cfg(all(test, target_os = "macos"))]
pub(crate) fn class_for(role: Role) -> u32 {
    qos_class(role) as u32
}

/// The longest timer [`sleep_until`] asks for within [`COALESCE_MARGIN`] of its deadline.
pub(crate) const PRECISE_SLICE: std::time::Duration = std::time::Duration::from_millis(2);

/// How far before its deadline [`sleep_until`] stops asking for one long timer and takes [`PRECISE_SLICE`]s: more than the coalescing measured below.
pub(crate) const COALESCE_MARGIN: std::time::Duration = std::time::Duration::from_millis(150);

/// Sleep until `deadline`, never asking for a timer that ends within [`COALESCE_MARGIN`] of it longer than [`PRECISE_SLICE`].
pub(crate) fn sleep_until(deadline: std::time::Instant) {
    sleep_until_with(deadline, std::time::Instant::now, std::thread::sleep);
}

/// [`sleep_until`] over an injected clock and sleeper.
fn sleep_until_with(
    deadline: std::time::Instant,
    mut now: impl FnMut() -> std::time::Instant,
    mut sleep: impl FnMut(std::time::Duration),
) {
    loop {
        let left = deadline.saturating_duration_since(now());
        if left.is_zero() {
            return;
        }
        sleep(if left > COALESCE_MARGIN {
            left - COALESCE_MARGIN
        } else {
            left.min(PRECISE_SLICE)
        });
    }
}

/// The class a worker that declares no role runs at — what every silent `spawn` in this crate gets, and what a deleted `set_self` line leaves behind.
#[cfg(all(test, target_os = "macos"))]
pub(crate) const UNDECLARED_CLASS: u32 = libc::qos_class_t::QOS_CLASS_DEFAULT as u32;

#[cfg(test)]
#[path = "qos_tests.rs"]
mod tests;
