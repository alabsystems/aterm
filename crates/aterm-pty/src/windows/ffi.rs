// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The ConPTY backend's FFI: direct `extern "system"` declarations against kernel32 (std
//! already links it), ntdll (`NtOpenFile`, the ConDrv opens the OpenConsole launch needs)
//! and version.dll (an OpenConsole's file version, for the host report), all three system
//! DLLs. Handles are plain `isize`, so the session registry is `Send + Sync` without unsafe
//! impls; struct layouts are transcribed from the Windows SDK.

// Win32 ABI names are kept verbatim (PascalCase fields, SCREAMING struct names)
// so they can be checked against the SDK headers line by line.
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
/// `STARTUPINFOW.dwFlags`: honor the `hStd*` fields. Set with NULL handles to suppress
/// the Win8+ std-handle auto-duplication (see the spawn).
pub(crate) const STARTF_USESTDHANDLES: u32 = 0x100;
/// `GetLastError` after `ReadFile` when the pipe's write side is gone (EOF).
pub(crate) const ERROR_BROKEN_PIPE: u32 = 109;
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
/// `ABOVE_NORMAL_PRIORITY_CLASS`, the focus-boost class: the shell's line editor
/// preempts NORMAL background load, while its children still start at NORMAL (a child
/// inherits only an IDLE/BELOW_NORMAL class), so a build it launches cannot starve the
/// terminal.
pub(crate) const ABOVE_NORMAL_PRIORITY_CLASS: u32 = 0x8000;
/// `NORMAL_PRIORITY_CLASS` — the blur restore class.
pub(crate) const NORMAL_PRIORITY_CLASS: u32 = 0x20;
/// `PROCESS_INFORMATION_CLASS::ProcessPowerThrottling` (SetProcessInformation).
pub(crate) const PROCESS_POWER_THROTTLING_CLASS: i32 = 4;
/// `PROCESS_POWER_THROTTLING_STATE.Version` — current (only) version.
pub(crate) const PROCESS_POWER_THROTTLING_CURRENT_VERSION: u32 = 1;
/// Power-throttling bit (EcoQoS / efficiency cores): set in `ControlMask` with a clear
/// `StateMask` bit to force throttling off; `ControlMask = 0` is system-managed.
pub(crate) const PROCESS_POWER_THROTTLING_EXECUTION_SPEED: u32 = 0x1;
/// `OpenProcess` access right: change priority/QoS only (least privilege for
/// the conhost focus-boost handle).
pub(crate) const PROCESS_SET_INFORMATION: u32 = 0x0200;
/// `CreateToolhelp32Snapshot` flag: snapshot every process in the system.
pub(crate) const TH32CS_SNAPPROCESS: u32 = 0x2;
/// `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` — the ONLY inheritable handles a child
/// created with `bInheritHandles = TRUE` receives (the OpenConsole launch hands
/// it exactly four).
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

/// `UNICODE_STRING` — the counted (NOT NUL-terminated) name `NtOpenFile` takes.
/// Lengths are in BYTES.
#[repr(C)]
pub(crate) struct UNICODE_STRING {
    pub Length: u16,
    pub MaximumLength: u16,
    pub Buffer: *mut u16,
}

/// `OBJECT_ATTRIBUTES` — the object name, the directory it is relative to, and
/// the handle attributes (`OBJ_*`).
#[repr(C)]
pub(crate) struct OBJECT_ATTRIBUTES {
    pub Length: u32,
    pub RootDirectory: HANDLE,
    pub ObjectName: *mut UNICODE_STRING,
    pub Attributes: u32,
    pub SecurityDescriptor: *mut c_void,
    pub SecurityQualityOfService: *mut c_void,
}

/// `IO_STATUS_BLOCK` (64-bit layout: the NTSTATUS/PVOID union is pointer-sized).
#[repr(C)]
pub(crate) struct IO_STATUS_BLOCK {
    pub Status: isize,
    pub Information: usize,
}

/// `VS_FIXEDFILEINFO` — the numeric version block of a PE version resource.
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

/// `COORD` — ConPTY size in character cells.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct COORD {
    pub x: i16,
    pub y: i16,
}

/// `STARTUPINFOW` (64-bit layout). Only `cb` and `dwFlags` are set: the ConPTY pipes
/// travel in the attribute list, not `hStd*`.
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

/// `STARTUPINFOEXW` — `STARTUPINFOW` plus the proc-thread attribute list that
/// carries `PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE`.
#[repr(C)]
pub(crate) struct STARTUPINFOEXW {
    pub StartupInfo: STARTUPINFOW,
    pub lpAttributeList: *mut c_void,
}

/// `PROCESS_INFORMATION` — the child's process/thread handles + ids.
#[repr(C)]
pub(crate) struct PROCESS_INFORMATION {
    pub hProcess: HANDLE,
    pub hThread: HANDLE,
    pub dwProcessId: u32,
    pub dwThreadId: u32,
}

/// `JOBOBJECT_BASIC_LIMIT_INFORMATION`.
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

/// `IO_COUNTERS`.
#[repr(C)]
pub(crate) struct IO_COUNTERS {
    pub ReadOperationCount: u64,
    pub WriteOperationCount: u64,
    pub OtherOperationCount: u64,
    pub ReadTransferCount: u64,
    pub WriteTransferCount: u64,
    pub OtherTransferCount: u64,
}

/// `JOBOBJECT_EXTENDED_LIMIT_INFORMATION`; only `LimitFlags` is set here.
#[repr(C)]
pub(crate) struct JOBOBJECT_EXTENDED_LIMIT_INFORMATION {
    pub BasicLimitInformation: JOBOBJECT_BASIC_LIMIT_INFORMATION,
    pub IoInfo: IO_COUNTERS,
    pub ProcessMemoryLimit: usize,
    pub JobMemoryLimit: usize,
    pub PeakProcessMemoryUsed: usize,
    pub PeakJobMemoryUsed: usize,
}

/// `PROCESS_POWER_THROTTLING_STATE` — the SetProcessInformation payload for
/// the EcoQoS opt-in/out (see the `PROCESS_POWER_THROTTLING_*` consts).
#[repr(C)]
pub(crate) struct PROCESS_POWER_THROTTLING_STATE {
    pub Version: u32,
    pub ControlMask: u32,
    pub StateMask: u32,
}

/// `PROCESSENTRY32W` (Toolhelp), for the spawn-time conhost discovery walk.
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
    /// `SetConsoleCtrlHandler(NULL, FALSE)` restores normal CTRL+C processing for this
    /// process, the state a child inherits at `CreateProcessW`: the only public switch
    /// for the "Ctrl+C ignored" flag `CREATE_NEW_PROCESS_GROUP` sets.
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
    /// appmodel.h: the full names of every installed package of a family.
    /// Returns a Win32 error code (0 = success).
    pub(crate) fn GetPackagesByPackageFamily(
        packageFamilyName: *const u16,
        count: *mut u32,
        packageFullNames: *mut *mut u16,
        bufferLength: *mut u32,
        buffer: *mut u16,
    ) -> i32;
    /// appmodel.h: the install folder of one package. Returns a Win32 error
    /// code (0 = success).
    pub(crate) fn GetPackagePathByFullName(
        packageFullName: *const u16,
        pathLength: *mut u32,
        path: *mut u16,
    ) -> i32;
}

#[link(name = "ntdll")]
unsafe extern "system" {
    /// Opens `\Device\ConDrv\Server` and its `\Reference` for the OpenConsole
    /// launch — the two handles winconpty opens the same way. Returns an
    /// NTSTATUS (negative = failure).
    pub(crate) fn NtOpenFile(
        FileHandle: *mut HANDLE,
        DesiredAccess: u32,
        ObjectAttributes: *mut OBJECT_ATTRIBUTES,
        IoStatusBlock: *mut IO_STATUS_BLOCK,
        ShareAccess: u32,
        OpenOptions: u32,
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

/// RAII guard around a raw kernel HANDLE: closes on drop unless released via
/// [`Handle::into_raw`], so every early return in the spawn closes exactly the handles
/// it created.
pub(crate) struct Handle(HANDLE);

impl Handle {
    /// Take ownership of `raw` (0 / `INVALID_HANDLE_VALUE` are tolerated as
    /// "nothing to close").
    pub(crate) fn new(raw: HANDLE) -> Self {
        Self(raw)
    }

    /// The raw handle, still owned by the guard.
    pub(crate) fn raw(&self) -> HANDLE {
        self.0
    }

    /// Release ownership without closing (the success path hands the handle to
    /// the session registry, whose `Drop` then owns the close).
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
