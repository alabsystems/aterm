// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! ConPTY Win32 FFI against `kernel32`, `ntdll`, and `version.dll`. Handles are plain `isize` so
//! the session registry is `Send + Sync` without unsafe impls.

#![allow(non_snake_case, non_camel_case_types, clippy::upper_case_acronyms)]

use std::ffi::c_void;

/// A Win32 HANDLE as a plain pointer-sized integer (see module docs).
pub(crate) type HANDLE = isize;

/// The pseudo-handle sentinel `INVALID_HANDLE_VALUE` (-1).
pub(crate) const INVALID_HANDLE_VALUE: HANDLE = -1;

/// `PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE` — attaches an HPCON to a child.
pub(crate) const PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE: usize = 0x0002_0016;
/// `CreateProcessW` flag: `lpStartupInfo` is a `STARTUPINFOEXW`.
pub(crate) const EXTENDED_STARTUPINFO_PRESENT: u32 = 0x0008_0000;
/// `CreateProcessW` flag: the environment block is UTF-16.
pub(crate) const CREATE_UNICODE_ENVIRONMENT: u32 = 0x400;
/// `CreateProcessW` flag: create the primary thread suspended, so no child instruction
/// runs before confinement and `ResumeThread`.
pub(crate) const CREATE_SUSPENDED: u32 = 0x4;
/// Set with NULL handles to suppress Win8+ std-handle auto-duplication.
pub(crate) const STARTF_USESTDHANDLES: u32 = 0x100;
/// `GetLastError` after `ReadFile` when the pipe's write side is gone (EOF).
pub(crate) const ERROR_BROKEN_PIPE: u32 = 109;
/// `GetLastError` after a `WriteFile` that `CancelSynchronousIo` cancelled.
pub(crate) const ERROR_OPERATION_ABORTED: i32 = 995;
/// `WaitFor*Object(s)` results.
pub(crate) const WAIT_OBJECT_0: u32 = 0;
pub(crate) const WAIT_TIMEOUT: u32 = 258;
/// Infinite wait timeout.
pub(crate) const INFINITE: u32 = 0xFFFF_FFFF;
/// Job Object limit flag: kill every process in the job when the last job
/// handle closes (the orphan sweep).
pub(crate) const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x2000;
/// `JOBOBJECTINFOCLASS::JobObjectExtendedLimitInformation`.
pub(crate) const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS: i32 = 9;
/// Focus-boost class: preempts NORMAL background load without passing to children.
pub(crate) const ABOVE_NORMAL_PRIORITY_CLASS: u32 = 0x8000;
/// `NORMAL_PRIORITY_CLASS` — the blur restore class.
pub(crate) const NORMAL_PRIORITY_CLASS: u32 = 0x20;
/// `PROCESS_INFORMATION_CLASS::ProcessPowerThrottling` (SetProcessInformation).
pub(crate) const PROCESS_POWER_THROTTLING_CLASS: i32 = 4;
/// `PROCESS_POWER_THROTTLING_STATE.Version` — current (only) version.
pub(crate) const PROCESS_POWER_THROTTLING_CURRENT_VERSION: u32 = 1;
/// EcoQoS bit: set in `ControlMask` with a clear `StateMask` bit to force throttling off.
pub(crate) const PROCESS_POWER_THROTTLING_EXECUTION_SPEED: u32 = 0x1;
/// `OpenProcess` access right: change priority/QoS only (least privilege for
/// the conhost focus-boost handle).
pub(crate) const PROCESS_SET_INFORMATION: u32 = 0x0200;
/// `CreateToolhelp32Snapshot` flag: snapshot every process in the system.
pub(crate) const TH32CS_SNAPPROCESS: u32 = 0x2;
/// Restricts inheritable handles a child created with `bInheritHandles = TRUE` receives.
pub(crate) const PROC_THREAD_ATTRIBUTE_HANDLE_LIST: usize = 0x0002_0002;
/// `SetHandleInformation` mask/flag: the handle is inherited by children.
pub(crate) const HANDLE_FLAG_INHERIT: u32 = 0x1;
/// `DuplicateHandle` option: the duplicate keeps the source's access rights.
pub(crate) const DUPLICATE_SAME_ACCESS: u32 = 0x2;
/// `OBJECT_ATTRIBUTES.Attributes`: the opened handle is inheritable.
pub(crate) const OBJ_INHERIT: u32 = 0x2;
/// `OBJECT_ATTRIBUTES.Attributes`: case-insensitive object-name lookup.
pub(crate) const OBJ_CASE_INSENSITIVE: u32 = 0x40;
/// Access rights for the ConDrv opens (winconpty's own masks).
pub(crate) const GENERIC_ALL: u32 = 0x1000_0000;
pub(crate) const GENERIC_READ: u32 = 0x8000_0000;
pub(crate) const GENERIC_WRITE: u32 = 0x4000_0000;
pub(crate) const SYNCHRONIZE: u32 = 0x0010_0000;
/// `FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE`.
pub(crate) const FILE_SHARE_ALL: u32 = 0x7;
/// `NtOpenFile` option: synchronous I/O, not alertable (the `\Reference` open).
pub(crate) const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x20;
/// `GetPackagesByPackageFamily` / `GetPackagePathByFullName`: the size probe's
/// "buffer too small" answer (a Win32 error code, returned, not set).
pub(crate) const ERROR_INSUFFICIENT_BUFFER: i32 = 122;
/// `JOBOBJECTINFOCLASS::JobObjectBasicProcessIdList`.
pub(crate) const JOB_OBJECT_BASIC_PROCESS_ID_LIST_CLASS: i32 = 3;
/// `OpenProcess` access right: the image path and little else (the console-input
/// guard's runtime probe).
pub(crate) const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
/// `OpenProcess` access right: `TerminateProcess` (a tab's programs outside its
/// job, ended at its close).
pub(crate) const PROCESS_TERMINATE: u32 = 0x0001;
/// `PROCESSINFOCLASS::ProcessBasicInformation` (`NtQueryInformationProcess`).
pub(crate) const PROCESS_BASIC_INFORMATION_CLASS: u32 = 0;
/// `CreateFileW` disposition: open what exists (`CONIN$`, `CONOUT$`).
pub(crate) const OPEN_EXISTING: u32 = 3;
/// `INPUT_RECORD.EventType` of a `MENU_EVENT_RECORD`.
pub(crate) const MENU_EVENT: u16 = 0x0008;
/// How many process ids one job query reads ([`JOBOBJECT_BASIC_PROCESS_ID_LIST`]).
pub(crate) const JOB_PROCESS_IDS: usize = 128;
/// Console output mode: control characters act (the leave client's buffer).
pub(crate) const ENABLE_PROCESSED_OUTPUT: u32 = 0x0001;
/// Console output mode: VT sequences written are parsed, so a `?1049l` switches
/// the screen buffer.
pub(crate) const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;
/// Set when `ReadConsoleW` returns cooked lines; cleared by raw line editors.
pub(crate) const ENABLE_LINE_INPUT: u32 = 0x0002;

#[repr(C)]
pub(crate) struct JOBOBJECT_BASIC_PROCESS_ID_LIST {
    pub NumberOfAssignedProcesses: u32,
    pub NumberOfProcessIdsInList: u32,
    pub ProcessIdList: [usize; JOB_PROCESS_IDS],
}

/// `EventType` followed by the 16-byte 4-aligned event union (`Event[0]` is `dwCommandId`).
#[repr(C)]
pub(crate) struct INPUT_RECORD {
    pub EventType: u16,
    pub Event: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(crate) struct SMALL_RECT {
    pub Left: i16,
    pub Top: i16,
    pub Right: i16,
    pub Bottom: i16,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(crate) struct CONSOLE_SCREEN_BUFFER_INFO {
    pub dwSize: [i16; 2],
    pub dwCursorPosition: [i16; 2],
    pub wAttributes: u16,
    pub srWindow: SMALL_RECT,
    pub dwMaximumWindowSize: [i16; 2],
}

/// Counted (non-NUL-terminated) UTF-16 string with byte lengths.
#[repr(C)]
pub(crate) struct UNICODE_STRING {
    pub Length: u16,
    pub MaximumLength: u16,
    pub Buffer: *mut u16,
}

#[repr(C)]
pub(crate) struct OBJECT_ATTRIBUTES {
    pub Length: u32,
    pub RootDirectory: HANDLE,
    pub ObjectName: *mut UNICODE_STRING,
    pub Attributes: u32,
    pub SecurityDescriptor: *mut c_void,
    pub SecurityQualityOfService: *mut c_void,
}

#[repr(C)]
pub(crate) struct IO_STATUS_BLOCK {
    pub Status: isize,
    pub Information: usize,
}

#[repr(C)]
pub(crate) struct VS_FIXEDFILEINFO {
    pub dwSignature: u32,
    pub dwStrucVersion: u32,
    pub dwFileVersionMS: u32,
    pub dwFileVersionLS: u32,
    pub dwProductVersionMS: u32,
    pub dwProductVersionLS: u32,
    pub dwFileFlagsMask: u32,
    pub dwFileFlags: u32,
    pub dwFileOS: u32,
    pub dwFileType: u32,
    pub dwFileSubtype: u32,
    pub dwFileDateMS: u32,
    pub dwFileDateLS: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct COORD {
    pub x: i16,
    pub y: i16,
}

#[repr(C)]
pub(crate) struct STARTUPINFOW {
    pub cb: u32,
    pub lpReserved: *mut u16,
    pub lpDesktop: *mut u16,
    pub lpTitle: *mut u16,
    pub dwX: u32,
    pub dwY: u32,
    pub dwXSize: u32,
    pub dwYSize: u32,
    pub dwXCountChars: u32,
    pub dwYCountChars: u32,
    pub dwFillAttribute: u32,
    pub dwFlags: u32,
    pub wShowWindow: u16,
    pub cbReserved2: u16,
    pub lpReserved2: *mut u8,
    pub hStdInput: HANDLE,
    pub hStdOutput: HANDLE,
    pub hStdError: HANDLE,
}

#[repr(C)]
pub(crate) struct STARTUPINFOEXW {
    pub StartupInfo: STARTUPINFOW,
    pub lpAttributeList: *mut c_void,
}

#[repr(C)]
pub(crate) struct PROCESS_INFORMATION {
    pub hProcess: HANDLE,
    pub hThread: HANDLE,
    pub dwProcessId: u32,
    pub dwThreadId: u32,
}

#[repr(C)]
pub(crate) struct JOBOBJECT_BASIC_LIMIT_INFORMATION {
    pub PerProcessUserTimeLimit: i64,
    pub PerJobUserTimeLimit: i64,
    pub LimitFlags: u32,
    pub MinimumWorkingSetSize: usize,
    pub MaximumWorkingSetSize: usize,
    pub ActiveProcessLimit: u32,
    pub Affinity: usize,
    pub PriorityClass: u32,
    pub SchedulingClass: u32,
}

#[repr(C)]
pub(crate) struct IO_COUNTERS {
    pub ReadOperationCount: u64,
    pub WriteOperationCount: u64,
    pub OtherOperationCount: u64,
    pub ReadTransferCount: u64,
    pub WriteTransferCount: u64,
    pub OtherTransferCount: u64,
}

#[repr(C)]
pub(crate) struct JOBOBJECT_EXTENDED_LIMIT_INFORMATION {
    pub BasicLimitInformation: JOBOBJECT_BASIC_LIMIT_INFORMATION,
    pub IoInfo: IO_COUNTERS,
    pub ProcessMemoryLimit: usize,
    pub JobMemoryLimit: usize,
    pub PeakProcessMemoryUsed: usize,
    pub PeakJobMemoryUsed: usize,
}

#[repr(C)]
pub(crate) struct PROCESS_POWER_THROTTLING_STATE {
    pub Version: u32,
    pub ControlMask: u32,
    pub StateMask: u32,
}

#[repr(C)]
pub(crate) struct PROCESSENTRY32W {
    pub dwSize: u32,
    pub cntUsage: u32,
    pub th32ProcessID: u32,
    pub th32DefaultHeapID: usize,
    pub th32ModuleID: u32,
    pub cntThreads: u32,
    pub th32ParentProcessID: u32,
    pub pcPriClassBase: i32,
    pub dwFlags: u32,
    pub szExeFile: [u16; 260],
}

#[repr(C)]
#[derive(Default)]
pub(crate) struct PROCESS_BASIC_INFORMATION {
    pub ExitStatus: isize,
    pub PebBaseAddress: usize,
    pub AffinityMask: usize,
    pub BasePriority: isize,
    pub UniqueProcessId: usize,
    pub InheritedFromUniqueProcessId: usize,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(crate) struct FILETIME {
    pub dwLowDateTime: u32,
    pub dwHighDateTime: u32,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    pub(crate) fn CreatePipe(
        hReadPipe: *mut HANDLE,
        hWritePipe: *mut HANDLE,
        lpPipeAttributes: *mut c_void,
        nSize: u32,
    ) -> i32;
    pub(crate) fn ReadFile(
        hFile: HANDLE,
        lpBuffer: *mut u8,
        nNumberOfBytesToRead: u32,
        lpNumberOfBytesRead: *mut u32,
        lpOverlapped: *mut c_void,
    ) -> i32;
    pub(crate) fn WriteFile(
        hFile: HANDLE,
        lpBuffer: *const u8,
        nNumberOfBytesToWrite: u32,
        lpNumberOfBytesWritten: *mut u32,
        lpOverlapped: *mut c_void,
    ) -> i32;
    pub(crate) fn CloseHandle(hObject: HANDLE) -> i32;
    pub(crate) fn CancelIoEx(hFile: HANDLE, lpOverlapped: *mut c_void) -> i32;
    pub(crate) fn CreatePseudoConsole(
        size: COORD,
        hInput: HANDLE,
        hOutput: HANDLE,
        dwFlags: u32,
        phPC: *mut HANDLE,
    ) -> i32;
    pub(crate) fn ResizePseudoConsole(hPC: HANDLE, size: COORD) -> i32;
    pub(crate) fn ClosePseudoConsole(hPC: HANDLE);
    pub(crate) fn InitializeProcThreadAttributeList(
        lpAttributeList: *mut c_void,
        dwAttributeCount: u32,
        dwFlags: u32,
        lpSize: *mut usize,
    ) -> i32;
    pub(crate) fn UpdateProcThreadAttribute(
        lpAttributeList: *mut c_void,
        dwFlags: u32,
        Attribute: usize,
        lpValue: *mut c_void,
        cbSize: usize,
        lpPreviousValue: *mut c_void,
        lpReturnSize: *mut usize,
    ) -> i32;
    pub(crate) fn DeleteProcThreadAttributeList(lpAttributeList: *mut c_void);
    pub(crate) fn CreateProcessW(
        lpApplicationName: *const u16,
        lpCommandLine: *mut u16,
        lpProcessAttributes: *mut c_void,
        lpThreadAttributes: *mut c_void,
        bInheritHandles: i32,
        dwCreationFlags: u32,
        lpEnvironment: *mut c_void,
        lpCurrentDirectory: *const u16,
        lpStartupInfo: *const STARTUPINFOW,
        lpProcessInformation: *mut PROCESS_INFORMATION,
    ) -> i32;
    pub(crate) fn ResumeThread(hThread: HANDLE) -> u32;
    pub(crate) fn TerminateProcess(hProcess: HANDLE, uExitCode: u32) -> i32;
    pub(crate) fn WaitForSingleObject(hHandle: HANDLE, dwMilliseconds: u32) -> u32;
    pub(crate) fn WaitForMultipleObjects(
        nCount: u32,
        lpHandles: *const HANDLE,
        bWaitAll: i32,
        dwMilliseconds: u32,
    ) -> u32;
    pub(crate) fn GetExitCodeProcess(hProcess: HANDLE, lpExitCode: *mut u32) -> i32;
    pub(crate) fn CreateEventW(
        lpEventAttributes: *mut c_void,
        bManualReset: i32,
        bInitialState: i32,
        lpName: *const u16,
    ) -> HANDLE;
    pub(crate) fn SetEvent(hEvent: HANDLE) -> i32;
    pub(crate) fn CreateJobObjectW(lpJobAttributes: *mut c_void, lpName: *const u16) -> HANDLE;
    pub(crate) fn SetInformationJobObject(
        hJob: HANDLE,
        JobObjectInformationClass: i32,
        lpJobObjectInformation: *mut c_void,
        cbJobObjectInformationLength: u32,
    ) -> i32;
    pub(crate) fn AssignProcessToJobObject(hJob: HANDLE, hProcess: HANDLE) -> i32;
    pub(crate) fn TerminateJobObject(hJob: HANDLE, uExitCode: u32) -> i32;
    pub(crate) fn SetPriorityClass(hProcess: HANDLE, dwPriorityClass: u32) -> i32;
    pub(crate) fn SetProcessInformation(
        hProcess: HANDLE,
        ProcessInformationClass: i32,
        ProcessInformation: *mut c_void,
        ProcessInformationSize: u32,
    ) -> i32;
    pub(crate) fn OpenProcess(
        dwDesiredAccess: u32,
        bInheritHandle: i32,
        dwProcessId: u32,
    ) -> HANDLE;
    pub(crate) fn GetCurrentProcessId() -> u32;
    /// A process's creation, exit, kernel and user times; the console guard's
    /// tree walk reads the creation time to tell a child from a pid reuse.
    pub(crate) fn GetProcessTimes(
        hProcess: HANDLE,
        lpCreationTime: *mut FILETIME,
        lpExitTime: *mut FILETIME,
        lpKernelTime: *mut FILETIME,
        lpUserTime: *mut FILETIME,
    ) -> i32;
    /// `SetConsoleCtrlHandler(NULL, FALSE)` restores inherited Ctrl+C processing.
    pub(crate) fn SetConsoleCtrlHandler(HandlerRoutine: *const c_void, Add: i32) -> i32;
    /// The pid behind a process HANDLE: the DefTerm adoption path's only way to learn
    /// the client's pid, since a handoff carries a handle, not a pid. 0 on failure.
    pub(crate) fn GetProcessId(hProcess: HANDLE) -> u32;
    pub(crate) fn CreateToolhelp32Snapshot(dwFlags: u32, th32ProcessID: u32) -> HANDLE;
    pub(crate) fn Process32FirstW(hSnapshot: HANDLE, lppe: *mut PROCESSENTRY32W) -> i32;
    pub(crate) fn Process32NextW(hSnapshot: HANDLE, lppe: *mut PROCESSENTRY32W) -> i32;
    pub(crate) fn SearchPathW(
        lpPath: *const u16,
        lpFileName: *const u16,
        lpExtension: *const u16,
        nBufferLength: u32,
        lpBuffer: *mut u16,
        lpFilePart: *mut *mut u16,
    ) -> u32;
    pub(crate) fn SetHandleInformation(hObject: HANDLE, dwMask: u32, dwFlags: u32) -> i32;
    pub(crate) fn DuplicateHandle(
        hSourceProcessHandle: HANDLE,
        hSourceHandle: HANDLE,
        hTargetProcessHandle: HANDLE,
        lpTargetHandle: *mut HANDLE,
        dwDesiredAccess: u32,
        bInheritHandle: i32,
        dwOptions: u32,
    ) -> i32;
    pub(crate) fn GetCurrentProcess() -> HANDLE;
    /// The calling thread's pseudo-handle; `DuplicateHandle` turns it into a
    /// real one another thread can use.
    pub(crate) fn GetCurrentThread() -> HANDLE;
    /// Cancel the synchronous I/O `hThread` is blocked in (the handle needs
    /// `THREAD_TERMINATE`); 0 with `ERROR_NOT_FOUND` when it is in none.
    pub(crate) fn CancelSynchronousIo(hThread: HANDLE) -> i32;
    /// Bytes waiting in a pipe, WITHOUT consuming them (the OpenConsole health
    /// check reads the host's first output this way and leaves it for the
    /// session reader).
    pub(crate) fn PeekNamedPipe(
        hNamedPipe: HANDLE,
        lpBuffer: *mut u8,
        nBufferSize: u32,
        lpBytesRead: *mut u32,
        lpTotalBytesAvail: *mut u32,
        lpBytesLeftThisMessage: *mut u32,
    ) -> i32;
    /// Returns a Win32 error code (0 = success).
    pub(crate) fn GetPackagesByPackageFamily(
        packageFamilyName: *const u16,
        count: *mut u32,
        packageFullNames: *mut *mut u16,
        bufferLength: *mut u32,
        buffer: *mut u16,
    ) -> i32;
    /// Returns a Win32 error code (0 = success).
    pub(crate) fn GetPackagePathByFullName(
        packageFullName: *const u16,
        pathLength: *mut u32,
        path: *mut u16,
    ) -> i32;
    /// Whether the process behind a handle is in `JobHandle` (`Result` nonzero).
    pub(crate) fn IsProcessInJob(ProcessHandle: HANDLE, JobHandle: HANDLE, Result: *mut i32)
    -> i32;
    pub(crate) fn QueryInformationJobObject(
        hJob: HANDLE,
        JobObjectInformationClass: i32,
        lpJobObjectInformation: *mut c_void,
        cbJobObjectInformationLength: u32,
        lpReturnLength: *mut u32,
    ) -> i32;
    pub(crate) fn QueryFullProcessImageNameW(
        hProcess: HANDLE,
        dwFlags: u32,
        lpExeName: *mut u16,
        lpdwSize: *mut u32,
    ) -> i32;
    pub(crate) fn CreateFileW(
        lpFileName: *const u16,
        dwDesiredAccess: u32,
        dwShareMode: u32,
        lpSecurityAttributes: *mut c_void,
        dwCreationDisposition: u32,
        dwFlagsAndAttributes: u32,
        hTemplateFile: HANDLE,
    ) -> HANDLE;
    pub(crate) fn WriteConsoleInputW(
        hConsoleInput: HANDLE,
        lpBuffer: *const INPUT_RECORD,
        nLength: u32,
        lpNumberOfEventsWritten: *mut u32,
    ) -> i32;
    pub(crate) fn GetConsoleScreenBufferInfo(
        hConsoleOutput: HANDLE,
        lpConsoleScreenBufferInfo: *mut CONSOLE_SCREEN_BUFFER_INFO,
    ) -> i32;
    /// Read and set a console OUTPUT buffer's mode: the leave client touches no
    /// input handle.
    pub(crate) fn GetConsoleMode(hConsoleHandle: HANDLE, lpMode: *mut u32) -> i32;
    pub(crate) fn SetConsoleMode(hConsoleHandle: HANDLE, dwMode: u32) -> i32;
    pub(crate) fn WriteConsoleW(
        hConsoleOutput: HANDLE,
        lpBuffer: *const u16,
        nNumberOfCharsToWrite: u32,
        lpNumberOfCharsWritten: *mut u32,
        lpReserved: *mut c_void,
    ) -> i32;
}

#[link(name = "ntdll")]
unsafe extern "system" {
    /// Opens `\Device\ConDrv\Server` and its `\Reference`; returns an NTSTATUS (negative = failure).
    pub(crate) fn NtOpenFile(
        FileHandle: *mut HANDLE,
        DesiredAccess: u32,
        ObjectAttributes: *mut OBJECT_ATTRIBUTES,
        IoStatusBlock: *mut IO_STATUS_BLOCK,
        ShareAccess: u32,
        OpenOptions: u32,
    ) -> i32;
    /// Queries process information (such as [`PROCESS_BASIC_INFORMATION`]); returns an NTSTATUS.
    pub(crate) fn NtQueryInformationProcess(
        ProcessHandle: HANDLE,
        ProcessInformationClass: u32,
        ProcessInformation: *mut c_void,
        ProcessInformationLength: u32,
        ReturnLength: *mut u32,
    ) -> i32;
}

#[link(name = "version")]
unsafe extern "system" {
    pub(crate) fn GetFileVersionInfoSizeW(lptstrFilename: *const u16, lpdwHandle: *mut u32) -> u32;
    pub(crate) fn GetFileVersionInfoW(
        lptstrFilename: *const u16,
        dwHandle: u32,
        dwLen: u32,
        lpData: *mut c_void,
    ) -> i32;
    pub(crate) fn VerQueryValueW(
        pBlock: *const c_void,
        lpSubBlock: *const u16,
        lplpBuffer: *mut *mut c_void,
        puLen: *mut u32,
    ) -> i32;
}

/// RAII guard around a raw kernel HANDLE that closes on drop unless released via [`Handle::into_raw`].
pub(crate) struct Handle(HANDLE);

impl Handle {
    /// Takes ownership of `raw` (0 and `INVALID_HANDLE_VALUE` are ignored on drop).
    pub(crate) fn new(raw: HANDLE) -> Self {
        Self(raw)
    }

    /// The raw handle, still owned by the guard.
    pub(crate) fn raw(&self) -> HANDLE {
        self.0
    }

    /// Releases ownership without closing the handle.
    pub(crate) fn into_raw(mut self) -> HANDLE {
        std::mem::replace(&mut self.0, 0)
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        if self.0 != 0 && self.0 != INVALID_HANDLE_VALUE {
            // SAFETY: this guard solely owns the live handle (into_raw swaps in 0,
            // so a released handle is never double-closed).
            unsafe { CloseHandle(self.0) };
        }
    }
}
