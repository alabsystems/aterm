// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Windows holder: its names, its waits and its answers, and a holder run in this process
//! over a scratch state directory (whose events no other aterm's holder shares), told on and
//! off as the verbs tell it. Whether Windows lists the power request (`powercfg /requests`)
//! needs an elevated prompt: the request's own calls answer for it here.

use super::*;
use std::sync::mpsc;
use std::time::Duration;

/// A wait for something that must happen: a hang detector, never a latency budget.
const MUST: Duration = Duration::from_secs(60);

fn at(since_boot: u64) -> Clocks {
    Clocks {
        since_boot,
        wall: 1_794_355_200 + since_boot,
    }
}

#[test]
fn a_state_directory_names_its_own_events_however_its_path_is_spelled() {
    let names = Names::of(Path::new(r"C:\Users\Someone\AppData\Local\aterm"));
    assert_eq!(
        names,
        Names::of(Path::new("c:/users/someone/appdata/local/ATERM/")),
        "case and separators fold as Windows compares paths"
    );
    assert_ne!(
        names,
        Names::of(Path::new(r"C:\Users\Someone\AppData\Local\aterm2")),
        "another state root, another holder"
    );
    for name in [&names.holder, &names.held, &names.changed] {
        assert!(name.starts_with(r"Local\aterm-acaffeine-"), "{name}");
        assert!(!name[6..].contains('\\'), "no further namespace: {name}");
    }
    assert_ne!(names.holder, names.held);
    assert_ne!(names.held, names.changed);
    assert_eq!(
        names.holder, r"Local\aterm-acaffeine-a59ec8ba2afa1b5d-holder",
        "the hash is the path's alone, the same in every process"
    );
}

#[test]
fn the_holder_waits_to_a_spans_end_and_with_no_deadline_forever() {
    let now = at(1_000);
    assert_eq!(wait_millis(Mode::Forever, now), None);
    assert_eq!(wait_millis(Mode::Off, now), Some(0));
    let hour = Mode::activate(Some(3_600), now);
    assert_eq!(wait_millis(hour, now), Some(3_600_000));
    assert_eq!(wait_millis(hour, at(4_599)), Some(1_000), "its last second");
    assert_eq!(wait_millis(hour, at(4_600)), Some(0), "over: off");
    let year = Mode::activate(Some(crate::model::MAX_SPAN), now);
    assert_eq!(
        wait_millis(year, now),
        Some(u32::MAX - 1),
        "a wait Windows takes, never its INFINITE"
    );
}

fn seen(held: bool, running: bool, pid: Option<u32>) -> Seen {
    Seen {
        held,
        running,
        pid,
        display: false,
    }
}

#[test]
fn status_says_on_only_while_the_request_is_held() {
    let now = at(1_000);
    let hour = Mode::activate(Some(3_600), now);
    assert_eq!(
        status_text(hour, now, seen(true, true, Some(4242))),
        format!(
            "acaffeine=on\nuntil={}\nholder=running\npid=4242\n",
            now.wall + 3_600
        )
    );
    assert_eq!(
        status_text(Mode::Forever, now, seen(true, true, None)),
        "acaffeine=on\nuntil=forever\nholder=running\n"
    );
    assert_eq!(
        status_text(Mode::Forever, now, seen(false, true, Some(7))),
        "acaffeine=off\nuntil=-\nholder=running\npid=7\n",
        "a holder that has not taken the request yet holds nothing"
    );
    assert_eq!(
        status_text(Mode::Forever, now, seen(false, false, Some(7))),
        "acaffeine=off\nuntil=-\nholder=stopped\n",
        "a saved on with no holder keeps nothing awake"
    );
    assert_eq!(
        status_text(Mode::Off, now, seen(true, true, Some(7))),
        "acaffeine=off\nuntil=-\nholder=running\npid=7\n"
    );
}

/// On a PC with Modern Standby the request keeps the display on too, and `status` says so
/// while it is on, never while it is off.
#[test]
fn status_says_when_the_display_is_kept_on() {
    let now = at(1_000);
    let display = Seen {
        display: true,
        ..seen(true, true, Some(7))
    };
    assert_eq!(
        status_text(Mode::Forever, now, display),
        "acaffeine=on\nuntil=forever\ndisplay=on\nholder=running\npid=7\n"
    );
    assert_eq!(
        status_text(Mode::Off, now, display),
        "acaffeine=off\nuntil=-\nholder=running\npid=7\n"
    );
    assert_eq!(
        status_text(
            Mode::Forever,
            now,
            Seen {
                held: false,
                ..display
            }
        ),
        "acaffeine=off\nuntil=-\nholder=running\npid=7\n",
        "nothing held keeps nothing on"
    );
}

/// A verb whose holder was started in its terminal's job says the holder ends with the
/// terminal; one started outside it, or as a child of a process in no job, says nothing.
#[test]
fn a_holder_left_in_the_terminals_job_is_said_to_end_with_it() {
    let note = launched_note(&Launched::InJob {
        pid: 7,
        why: "no ancestor outside this process's job was found".to_string(),
    })
    .expect("a note");
    assert!(note.contains("closing the terminal ends it"), "{note}");
    assert!(note.contains("no ancestor outside"), "{note}");
    assert_eq!(launched_note(&Launched::Child { pid: 7 }), None);
    assert_eq!(
        launched_note(&Launched::OutsideJob(aterm_pty::Detached::Under {
            pid: 7,
            parent: 8
        })),
        None
    );
}

#[test]
fn nothing_saved_is_off_and_a_span_is_off_past_its_end() {
    let dir = crate::test_scratch_dir("win-saved");
    let file = dir.path().join("acaffeine.state");
    let now = clocks().expect("clocks");
    assert_eq!(
        saved_mode(&file, now),
        Mode::Off,
        "no file: nobody turned it on"
    );
    state::save_at(&file, Mode::Forever, now).expect("saved");
    assert_eq!(saved_mode(&file, now), Mode::Forever);
    let minute = Mode::activate(Some(60), now);
    state::save_at(&file, minute, now).expect("saved");
    assert!(saved_mode(&file, now).is_on(now));
    let later = Clocks {
        since_boot: now.since_boot + 61,
        wall: now.wall + 61,
    };
    assert_eq!(saved_mode(&file, later), Mode::Off);
}

/// A span saved before a restart is read on the wall clock after it, through the reader every
/// verb and the holder use: the boot clock (`GetTickCount64`) starts again at a restart, so the
/// saved `ends` would revive a span that ended days ago, or give one with time left its whole
/// length again. The same boot reads `ends` as saved.
#[test]
fn a_span_saved_before_a_restart_is_read_on_the_wall_clock() {
    let dir = crate::test_scratch_dir("win-restart");
    let file = dir.path().join("acaffeine.state");
    let saved_at = Clocks {
        since_boot: 432_000,
        wall: 1_794_355_200,
    };
    let span = 28_800;
    state::save_at(&file, Mode::activate(Some(span), saved_at), saved_at).expect("saved");
    let after = |since_boot, later| Clocks {
        since_boot,
        wall: saved_at.wall + later,
    };
    assert_eq!(
        saved_mode(&file, after(600, 2 * 86_400)),
        Mode::Off,
        "ended two days ago"
    );
    assert_eq!(
        saved_mode(&file, after(600, 3_600)),
        Mode::Until {
            deadline: 600 + span - 3_600,
            span
        },
        "an hour of it gone: the rest"
    );
    let same_boot = Clocks {
        since_boot: saved_at.since_boot + 100,
        wall: saved_at.wall + 100,
    };
    assert_eq!(
        saved_mode(&file, same_boot),
        Mode::Until {
            deadline: saved_at.since_boot + span,
            span
        },
        "CONTROL: this boot's `ends`"
    );
}

/// A holder over a scratch state directory, on a thread: what it told, and how it ended.
struct Holder {
    told: mpsc::Receiver<bool>,
    ended: mpsc::Receiver<Served>,
}

impl Holder {
    fn start(file: &Path, names: &Names) -> Self {
        let (told_tx, told) = mpsc::channel();
        let (ended_tx, ended) = mpsc::channel();
        let (file, names) = (file.to_path_buf(), names.clone());
        std::thread::spawn(move || {
            let served = serve_at(&file, &names, |held| {
                let _ = told_tx.send(held);
            });
            let _ = ended_tx.send(served);
        });
        Self { told, ended }
    }

    fn told(&self) -> bool {
        self.told.recv_timeout(MUST).expect("the holder goes on")
    }

    fn ended(&self) -> Served {
        self.ended.recv_timeout(MUST).expect("the holder ends")
    }
}

/// The holder takes the request while the mode is on, says so by its events and its pid, lets
/// a second holder of the same directory hold nothing, and lets go and exits when told the mode
/// is off. A holder started with the mode off holds nothing at all.
#[test]
fn a_holder_holds_while_the_mode_is_on_and_lets_go_when_told_off() {
    let dir = crate::test_scratch_dir("win-holder");
    let file = dir.path().join("acaffeine.state");
    let names = Names::of(dir.path());
    let now = clocks().expect("clocks");

    assert_eq!(
        serve_at(&file, &names, |_| panic!("nothing is on")),
        Served::Off,
        "nothing saved: nothing held"
    );
    assert!(!exists(&names.holder) && !exists(&names.held));
    assert_eq!(holder_pid(&file, &names), None);

    state::save_at(&file, Mode::Forever, now).expect("saved");
    let holder = Holder::start(&file, &names);
    assert!(holder.told(), "the request is taken");
    assert!(exists(&names.holder) && exists(&names.held));
    assert_eq!(holder_pid(&file, &names), Some(std::process::id()));
    assert_eq!(
        serve_at(&file, &names, |_| panic!("the second holds nothing")),
        Served::Another
    );
    assert!(exists(&names.held), "the second's exit took nothing down");

    // A change that leaves the mode on is read and held through: no release is told.
    state::save_at(&file, Mode::activate(Some(3_600), now), now).expect("saved");
    assert_eq!(tell_changed(&names), Tell::Told);
    state::save_at(&file, Mode::Off, now).expect("saved");
    assert_eq!(tell_changed(&names), Tell::Told);
    assert!(!holder.told(), "the request is released, and only now");
    assert_eq!(holder.ended(), Served::Off);
    assert!(!exists(&names.holder) && !exists(&names.held));
    assert_eq!(holder_pid(&file, &names), None, "no holder, no pid");
    assert!(!file.with_file_name(PID_FILE).exists());
}

/// NEGATIVE CONTROL for the telling: a holder that is not told stays held over a saved off
/// until it is told, so the release above is the telling's and not a timer's.
#[test]
fn a_holder_reads_the_mode_only_when_told() {
    let dir = crate::test_scratch_dir("win-told");
    let file = dir.path().join("acaffeine.state");
    let names = Names::of(dir.path());
    let now = clocks().expect("clocks");
    state::save_at(&file, Mode::Forever, now).expect("saved");
    let holder = Holder::start(&file, &names);
    assert!(holder.told());
    state::save_at(&file, Mode::Off, now).expect("saved");
    assert!(
        holder.told.try_recv().is_err() && exists(&names.held),
        "saved off, not told: still held"
    );
    assert_eq!(tell_changed(&names), Tell::Told);
    assert!(!holder.told());
    assert_eq!(holder.ended(), Served::Off);
}

/// A tell with no holder finds none; one whose change event this user may not set is refused
/// (a holder made by an elevated terminal with its token's default DACL), never taken as told
/// nor as none; and a holder's own events let this user tell it. NEGATIVE CONTROL: the event
/// made with SYSTEM's access alone is the refusal, so the told case is the DACL's doing.
#[test]
fn a_tell_says_whether_a_holder_was_told_none_runs_or_it_was_refused() {
    let dir = crate::test_scratch_dir("win-tell");
    let names = Names::of(dir.path());
    assert_eq!(tell_changed(&names), Tell::NoHolder);

    let system_only = Descriptor::from_sddl("D:(A;;GA;;;SY)").expect("a descriptor");
    let (refusing, was_there) =
        Event::create_with(&names.changed, Some(&system_only)).expect("made");
    assert!(!was_there);
    assert_eq!(
        tell_changed(&names),
        Tell::Refused,
        "this user may not set it"
    );
    drop(refusing);
    assert_eq!(tell_changed(&names), Tell::NoHolder, "gone with its handle");

    let (holders, _) = Event::create(&names.changed).expect("made");
    assert_eq!(tell_changed(&names), Tell::Told);
    assert_eq!(holders.wait(Some(0)), 0, "the tell set the holder's event");
}

/// The holder's events are made with this user's own entry, giving exactly what a verb asks
/// (to learn one exists, wait on it and set it), beside SYSTEM's and the Administrators': not
/// the token's default DACL, which an elevated holder's would be.
#[test]
fn a_holders_events_give_this_user_what_a_verb_asks_whatever_the_holders_elevation() {
    let sid = this_user_sid().expect("this user's SID");
    assert!(sid.starts_with("S-1-"), "{sid}");
    assert_eq!(
        event_sddl(&sid),
        format!("D:(A;;GA;;;SY)(A;;GA;;;BA)(A;;0x100003;;;{sid})")
    );
    let dir = crate::test_scratch_dir("win-dacl");
    let names = Names::of(dir.path());
    let (_event, _) = Event::create(&names.held).expect("made");
    // Its owner (this user, who made it) may read its DACL, which no holder asks for.
    let reader = Event::open_or_why(&names.held, acl::READ_CONTROL).expect("its owner reads it");
    let entries = acl::allowed(reader.0);
    let user = acl::sid(&sid);
    let mine: Vec<u32> = entries
        .iter()
        .filter(|(entry_sid, _)| acl::equal(entry_sid, &user))
        .map(|&(_, mask)| mask)
        .collect();
    assert_eq!(mine, [USER_EVENT_RIGHTS], "{entries:?}");
    assert_eq!(
        entries.len(),
        3,
        "SYSTEM, the Administrators and this user: {entries:?}"
    );
}

/// Reading an event's DACL back, for the test above.
mod acl {
    #![allow(non_snake_case, reason = "Win32 names are kept verbatim")]

    use super::HANDLE;
    use std::ffi::c_void;

    /// `DACL_SECURITY_INFORMATION`.
    const DACL_SECURITY_INFORMATION: u32 = 0x4;
    /// `READ_CONTROL`.
    pub(super) const READ_CONTROL: u32 = 0x0002_0000;
    /// `ACCESS_ALLOWED_ACE_TYPE`.
    const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;

    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn GetKernelObjectSecurity(
            Handle: HANDLE,
            RequestedInformation: u32,
            pSecurityDescriptor: *mut c_void,
            nLength: u32,
            lpnLengthNeeded: *mut u32,
        ) -> i32;
        fn GetSecurityDescriptorDacl(
            pSecurityDescriptor: *const c_void,
            lpbDaclPresent: *mut i32,
            pDacl: *mut *mut Acl,
            lpbDaclDefaulted: *mut i32,
        ) -> i32;
        fn GetAce(pAcl: *const Acl, dwAceIndex: u32, pAce: *mut *mut u8) -> i32;
        fn EqualSid(pSid1: *const c_void, pSid2: *const c_void) -> i32;
        fn GetLengthSid(pSid: *const c_void) -> u32;
        fn ConvertStringSidToSidW(StringSid: *const u16, Sid: *mut *mut c_void) -> i32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LocalFree(hMem: *mut c_void) -> *mut c_void;
    }

    /// `ACL` (`winnt.h`).
    #[repr(C)]
    pub(super) struct Acl {
        revision: u8,
        sbz1: u8,
        size: u16,
        count: u16,
        sbz2: u16,
    }

    /// The SID `text` names, as bytes.
    pub(super) fn sid(text: &str) -> Vec<u8> {
        let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
        let mut sid = std::ptr::null_mut();
        // SAFETY: a NUL-terminated SID string and an out-param the call fills with a
        // LocalAlloc'd SID, copied and freed below.
        assert_ne!(
            unsafe { ConvertStringSidToSidW(wide.as_ptr(), &raw mut sid) },
            0
        );
        // SAFETY: the SID just made, its length read from it, then freed once.
        unsafe {
            let bytes =
                std::slice::from_raw_parts(sid.cast::<u8>(), GetLengthSid(sid) as usize).to_vec();
            LocalFree(sid);
            bytes
        }
    }

    /// Whether two SIDs' bytes name one SID.
    pub(super) fn equal(a: &[u8], b: &[u8]) -> bool {
        // SAFETY: two SIDs' whole bytes.
        unsafe { EqualSid(a.as_ptr().cast(), b.as_ptr().cast()) != 0 }
    }

    /// Each access-allowed entry of the DACL of the object behind `handle`: its SID's bytes
    /// and its mask.
    pub(super) fn allowed(handle: HANDLE) -> Vec<(Vec<u8>, u32)> {
        let mut needed = 0_u32;
        // SAFETY: a size probe with no buffer.
        let _ = unsafe {
            GetKernelObjectSecurity(
                handle,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                0,
                &raw mut needed,
            )
        };
        let mut buf = vec![0_u64; (needed as usize).div_ceil(8)];
        // SAFETY: a writable, 8-aligned buffer of the size the probe asked for.
        assert_ne!(
            unsafe {
                GetKernelObjectSecurity(
                    handle,
                    DACL_SECURITY_INFORMATION,
                    buf.as_mut_ptr().cast(),
                    needed,
                    &raw mut needed,
                )
            },
            0,
            "the event's DACL reads"
        );
        let (mut present, mut defaulted) = (0_i32, 0_i32);
        let mut dacl: *mut Acl = std::ptr::null_mut();
        // SAFETY: the descriptor just read, and out-params.
        assert_ne!(
            unsafe {
                GetSecurityDescriptorDacl(
                    buf.as_ptr().cast(),
                    &raw mut present,
                    &raw mut dacl,
                    &raw mut defaulted,
                )
            },
            0
        );
        assert!(present != 0 && !dacl.is_null(), "a DACL is present");
        // SAFETY: the DACL inside the descriptor read above, which `buf` keeps alive.
        let count = unsafe { (*dacl).count };
        (0..u32::from(count))
            .filter_map(|index| {
                let mut ace: *mut u8 = std::ptr::null_mut();
                // SAFETY: an index below the DACL's count, and an out-param.
                assert_ne!(unsafe { GetAce(dacl, index, &raw mut ace) }, 0);
                // SAFETY: an ACE in the DACL: a 4-byte header (type, flags, size), then for an
                // access-allowed entry its mask and its SID.
                unsafe {
                    if *ace != ACCESS_ALLOWED_ACE_TYPE {
                        return None;
                    }
                    let mask = ace.add(4).cast::<u32>().read_unaligned();
                    let sid = ace.add(8).cast::<c_void>();
                    let len = GetLengthSid(sid) as usize;
                    Some((
                        std::slice::from_raw_parts(sid.cast::<u8>(), len).to_vec(),
                        mask,
                    ))
                }
            })
            .collect()
    }
}

/// What a run of this test binary as a stand-in is ([`stand_in`]): `launcher` or `holder`.
const STAND_IN: &str = "ATERM_ACAFFEINE_TEST_STAND_IN";
/// The stand-in holder's state file.
const STAND_IN_STATE: &str = "ATERM_ACAFFEINE_TEST_STATE";
/// The kill-on-close job the stand-in launcher joins, by name.
const STAND_IN_JOB: &str = "ATERM_ACAFFEINE_TEST_JOB";
/// How the stand-in launcher starts its holder: `outside` (as `on` does) or `in-job` (as a
/// plain child, the negative control).
const STAND_IN_HOW: &str = "ATERM_ACAFFEINE_TEST_HOW";
/// Where the stand-in launcher writes the holder's pid.
const STAND_IN_RESULT: &str = "ATERM_ACAFFEINE_TEST_RESULT";
/// This test, as the stand-ins run it.
const STAND_IN_TEST: &str = "win::tests::stand_in";

/// Not a check of its own: the body this test binary runs as a stand-in, in a process the job
/// tests below start, and nothing in any other run. The launcher joins the named kill-on-close
/// job, as a shell in an aterm tab is in one, and starts a holder as `aterm acaffeine on` does
/// (this binary again, as the holder stand-in); the holder serves the named state file.
#[test]
fn stand_in() {
    let var = |name| std::env::var_os(name).unwrap_or_else(|| panic!("{name:?} set"));
    match std::env::var(STAND_IN).as_deref() {
        Ok("holder") => {
            let file = PathBuf::from(var(STAND_IN_STATE));
            let names = Names::of(file.parent().expect("a directory"));
            assert_eq!(serve_at(&file, &names, |_| {}), Served::Off);
        }
        Ok("launcher") => {
            job::join(&var(STAND_IN_JOB));
            assert!(aterm_pty::in_a_job(), "the launcher runs in the job");
            let exe = std::env::current_exe().expect("this test binary");
            let args: Vec<OsString> = ["--exact", STAND_IN_TEST, "--nocapture"]
                .into_iter()
                .map(OsString::from)
                .collect();
            let env = [(OsString::from(STAND_IN), OsString::from("holder"))];
            let pid = match std::env::var(STAND_IN_HOW).as_deref() {
                Ok("outside") => match start_holder_with(&exe, &args, &env).expect("started") {
                    Launched::OutsideJob(aterm_pty::Detached::Under { pid, .. }) => pid,
                    other => panic!("started outside the job by a parent outside it: {other:?}"),
                },
                Ok("in-job") => {
                    spawn_child(&exe, &args, &env, &holder_directory()).expect("started")
                }
                other => panic!("{STAND_IN_HOW}: {other:?}"),
            };
            std::fs::write(var(STAND_IN_RESULT), format!("{pid}\n")).expect("the pid written");
        }
        _ => {}
    }
}

/// THE TAB'S CLOSE: a holder started by a process in a kill-on-close job (a shell in an aterm
/// tab; here a stand-in launcher that joined one) is in no part of that job and is not below
/// the launcher, and it keeps holding when the job is closed, which ends everything the job
/// holds; told off, it exits on its own.
#[test]
fn a_holder_started_in_a_kill_on_close_job_outlives_the_jobs_close() {
    job::case(true);
}

/// NEGATIVE CONTROL: a holder started as a plain child of the launcher is in the job, and the
/// job's close ends it, so the case above survives by being started outside the job.
#[test]
fn a_holder_left_in_a_kill_on_close_job_ends_with_its_close() {
    job::case(false);
}

/// The job tests' Windows calls and their run.
mod job {
    #![allow(non_snake_case, reason = "Win32 names are kept verbatim")]

    use super::*;
    use std::ffi::{OsStr, c_void};
    use std::os::windows::ffi::OsStrExt as _;

    /// `JOB_OBJECT_ASSIGN_PROCESS`.
    const JOB_OBJECT_ASSIGN_PROCESS: u32 = 0x0001;
    /// `JobObjectExtendedLimitInformation`.
    const EXTENDED_LIMITS: i32 = 9;
    /// `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`.
    const KILL_ON_JOB_CLOSE: u32 = 0x2000;
    /// `PROCESS_QUERY_LIMITED_INFORMATION`.
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    /// `PROCESS_TERMINATE`.
    const PROCESS_TERMINATE: u32 = 0x0001;
    /// `WAIT_TIMEOUT`.
    const WAIT_TIMEOUT: u32 = 258;

    /// `JOBOBJECT_BASIC_LIMIT_INFORMATION`.
    #[repr(C)]
    #[derive(Default)]
    struct BasicLimits {
        per_process_user_time: i64,
        per_job_user_time: i64,
        flags: u32,
        min_working_set: usize,
        max_working_set: usize,
        active_processes: u32,
        affinity: usize,
        priority: u32,
        scheduling: u32,
    }

    /// `JOBOBJECT_EXTENDED_LIMIT_INFORMATION`.
    #[repr(C)]
    #[derive(Default)]
    struct ExtendedLimits {
        basic: BasicLimits,
        io: [u64; 6],
        process_memory: usize,
        job_memory: usize,
        peak_process_memory: usize,
        peak_job_memory: usize,
    }
    #[cfg(target_pointer_width = "64")]
    const _: () = assert!(std::mem::size_of::<ExtendedLimits>() == 144);

    /// `PROCESS_BASIC_INFORMATION`.
    #[repr(C)]
    #[derive(Default)]
    struct BasicInformation {
        exit_status: isize,
        peb: usize,
        affinity: usize,
        priority: isize,
        pid: usize,
        parent: usize,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateJobObjectW(lpJobAttributes: *const c_void, lpName: *const u16) -> HANDLE;
        fn OpenJobObjectW(dwDesiredAccess: u32, bInheritHandle: i32, lpName: *const u16) -> HANDLE;
        fn SetInformationJobObject(
            hJob: HANDLE,
            JobObjectInformationClass: i32,
            lpJobObjectInformation: *const c_void,
            cbJobObjectInformationLength: u32,
        ) -> i32;
        fn AssignProcessToJobObject(hJob: HANDLE, hProcess: HANDLE) -> i32;
        fn IsProcessInJob(ProcessHandle: HANDLE, JobHandle: HANDLE, Result: *mut i32) -> i32;
        fn OpenProcess(dwDesiredAccess: u32, bInheritHandle: i32, dwProcessId: u32) -> HANDLE;
        fn TerminateProcess(hProcess: HANDLE, uExitCode: u32) -> i32;
        fn GetExitCodeProcess(hProcess: HANDLE, lpExitCode: *mut u32) -> i32;
        fn QueryFullProcessImageNameW(
            hProcess: HANDLE,
            dwFlags: u32,
            lpExeName: *mut u16,
            lpdwSize: *mut u32,
        ) -> i32;
    }

    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtQueryInformationProcess(
            ProcessHandle: HANDLE,
            ProcessInformationClass: u32,
            ProcessInformation: *mut c_void,
            ProcessInformationLength: u32,
            ReturnLength: *mut u32,
        ) -> i32;
    }

    fn wide(text: &OsStr) -> Vec<u16> {
        text.encode_wide().chain(Some(0)).collect()
    }

    /// A handle this test owns; closed on drop.
    struct Owned(HANDLE);

    impl Drop for Owned {
        fn drop(&mut self) {
            if self.0 != 0 {
                // SAFETY: the handle this owns, closed once.
                let _ = unsafe { CloseHandle(self.0) };
            }
        }
    }

    /// The stand-in launcher's half: join the job named `name`, which the test made.
    pub(super) fn join(name: &OsStr) {
        let name = wide(name);
        // SAFETY: a NUL-terminated name; the handle is owned below.
        let job = Owned(unsafe { OpenJobObjectW(JOB_OBJECT_ASSIGN_PROCESS, 0, name.as_ptr()) });
        assert_ne!(job.0, 0, "the test's job opens");
        // SAFETY: the job just opened, and this process's pseudo handle.
        let joined = unsafe { AssignProcessToJobObject(job.0, GetCurrentProcess()) };
        assert_ne!(joined, 0, "{}", std::io::Error::last_os_error());
    }

    /// A holder stand-in this test started (through its launcher), ended on drop if it still
    /// runs: a test that fails leaves no holder behind.
    struct Started {
        process: Owned,
    }

    impl Started {
        /// The holder stand-in `pid` names, once vouched for: this test binary's image.
        fn open(pid: u32) -> Self {
            let access = SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE;
            // SAFETY: plain OpenProcess; the handle is owned by the guard.
            let process = Owned(unsafe { OpenProcess(access, 0, pid) });
            assert_ne!(process.0, 0, "the holder {pid} runs");
            let mut buf = vec![0_u16; 32_768];
            let mut len = buf.len() as u32;
            // SAFETY: the process handle above, a buffer of `len` characters, in/out length.
            let read =
                unsafe { QueryFullProcessImageNameW(process.0, 0, buf.as_mut_ptr(), &raw mut len) };
            assert_ne!(read, 0);
            let image = PathBuf::from(String::from_utf16_lossy(&buf[..len as usize]));
            let ours = std::env::current_exe().expect("this test binary");
            assert!(
                image.as_os_str().eq_ignore_ascii_case(ours.as_os_str()),
                "pid {pid} is this test binary: {}",
                image.display()
            );
            Self { process }
        }

        fn running(&self) -> bool {
            // SAFETY: the process handle this guard holds.
            unsafe { WaitForSingleObject(self.process.0, 0) == WAIT_TIMEOUT }
        }

        /// Its exit code once it has ended (a hang detector, never a budget).
        fn ended(&self) -> u32 {
            let millis = u32::try_from(MUST.as_millis()).unwrap_or(u32::MAX);
            // SAFETY: the process handle this guard holds.
            let waited = unsafe { WaitForSingleObject(self.process.0, millis) };
            assert_eq!(waited, 0, "the holder ends");
            let mut code = 0_u32;
            // SAFETY: the process handle this guard holds, and an out-param.
            assert_ne!(
                unsafe { GetExitCodeProcess(self.process.0, &raw mut code) },
                0
            );
            code
        }

        fn in_job(&self, job: HANDLE) -> bool {
            let mut held = 0_i32;
            // SAFETY: the process handle this guard holds, the test's job, an out-param.
            assert_ne!(
                unsafe { IsProcessInJob(self.process.0, job, &raw mut held) },
                0
            );
            held != 0
        }

        /// The pid of its parent, as the kernel keeps it.
        fn parent(&self) -> u32 {
            let mut info = BasicInformation::default();
            // SAFETY: the process handle this guard holds and an out-buffer of its size.
            let status = unsafe {
                NtQueryInformationProcess(
                    self.process.0,
                    0,
                    (&raw mut info).cast(),
                    std::mem::size_of::<BasicInformation>() as u32,
                    std::ptr::null_mut(),
                )
            };
            assert!(status >= 0, "{status:#x}");
            u32::try_from(info.parent).expect("a pid")
        }
    }

    impl Drop for Started {
        fn drop(&mut self) {
            if self.running() {
                // SAFETY: the holder stand-in this test started and holds.
                let _ = unsafe { TerminateProcess(self.process.0, 1) };
            }
        }
    }

    /// Wait until `ready` holds: a hang detector, never a budget.
    fn until(what: &str, mut ready: impl FnMut() -> bool) {
        let deadline = std::time::Instant::now() + MUST;
        while !ready() {
            assert!(std::time::Instant::now() < deadline, "{what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// One job case: a launcher in a kill-on-close job starts a holder `outside` it (as
    /// `aterm acaffeine on` does) or as its plain child, and the job is closed.
    pub(super) fn case(outside: bool) {
        let dir = crate::test_scratch_dir(if outside { "win-job-out" } else { "win-job-in" });
        let file = dir.path().join("acaffeine.state");
        let names = Names::of(dir.path());
        let result = dir.path().join("launched");
        state::save_at(&file, Mode::Forever, clocks().expect("clocks")).expect("saved");

        let job_name = OsString::from(format!(
            r"Local\aterm-acaffeine-test-job-{}-{}",
            std::process::id(),
            if outside { "out" } else { "in" }
        ));
        let job_wide = wide(&job_name);
        // SAFETY: no attributes and a NUL-terminated name; the handle is owned below.
        let mut job = Owned(unsafe { CreateJobObjectW(std::ptr::null(), job_wide.as_ptr()) });
        assert_ne!(job.0, 0, "{}", std::io::Error::last_os_error());
        let mut limits = ExtendedLimits::default();
        limits.basic.flags = KILL_ON_JOB_CLOSE;
        // SAFETY: the job just made, and a limit record of its size.
        let limited = unsafe {
            SetInformationJobObject(
                job.0,
                EXTENDED_LIMITS,
                (&raw const limits).cast(),
                std::mem::size_of::<ExtendedLimits>() as u32,
            )
        };
        assert_ne!(limited, 0, "{}", std::io::Error::last_os_error());

        let exe = std::env::current_exe().expect("this test binary");
        let mut launcher = aterm_uds::spawnfd::command(&exe);
        launcher
            .args(["--exact", STAND_IN_TEST, "--nocapture"])
            .env(STAND_IN, "launcher")
            .env(STAND_IN_STATE, &file)
            .env(STAND_IN_JOB, &job_name)
            .env(STAND_IN_HOW, if outside { "outside" } else { "in-job" })
            .env(STAND_IN_RESULT, &result)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let mut launcher = launcher.spawn().expect("the launcher starts");
        let deadline = std::time::Instant::now() + MUST;
        let status = loop {
            if let Some(status) = launcher.try_wait().expect("the launcher's state") {
                break status;
            }
            assert!(std::time::Instant::now() < deadline, "the launcher ends");
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(status.success(), "the launcher passed: {status}");
        let pid: u32 = std::fs::read_to_string(&result)
            .expect("the launcher wrote the holder's pid")
            .trim()
            .parse()
            .expect("a pid");
        let holder = Started::open(pid);
        until("the holder holds", || exists(&names.held));
        assert_eq!(holder_pid(&file, &names), Some(pid));

        assert_eq!(holder.in_job(job.0), !outside, "in the job");
        if outside {
            assert_eq!(
                holder.parent(),
                std::process::id(),
                "under the launcher's parent outside the job, below no launcher"
            );
        }

        // The tab closes: the job's last handle goes, and the job ends all it holds.
        drop(std::mem::replace(&mut job, Owned(0)));
        if !outside {
            let _ = holder.ended();
            return;
        }
        assert!(holder.running(), "the job's close left the holder");
        assert!(exists(&names.held), "and its request");
        state::save_at(&file, Mode::Off, clocks().expect("clocks")).expect("saved");
        assert_eq!(tell_changed(&names), Tell::Told);
        assert_eq!(holder.ended(), 0, "told off, it exits on its own");
        assert!(!exists(&names.held));
    }
}
