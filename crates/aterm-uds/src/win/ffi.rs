// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Raw Win32 declarations for the AF_UNIX-over-winsock transport and process
//! inspection (`ws2_32`, `kernel32`, `advapi32`, `ntdll`, `wtsapi32`, `bcrypt`).
//!
//! Process image and PEB parameter reads go through `aterm_sysprobe::peb`; the
//! remaining 64-bit structures used by `crate::holders` are declared here and
//! layout-tested below.

#![allow(non_snake_case, reason = "Win32 API names are camel-case by contract")]

/// Winsock `SOCKET`: an opaque `UINT_PTR`.
pub(crate) type RawSocket = usize;
/// Win32 `HANDLE` as returned by `OpenProcess` (`null` == 0 on failure).
pub(crate) type RawHandle = isize;

pub(crate) const INVALID_SOCKET: RawSocket = RawSocket::MAX;
pub(crate) const SOCKET_ERROR: i32 = -1;

/// `AF_UNIX` address family (afunix.sys, Windows 10 1803+).
pub(crate) const AF_UNIX: i32 = 1;
pub(crate) const SOCK_STREAM: i32 = 1;

// `shutdown(2)` directions.
pub(crate) const SD_RECEIVE: i32 = 0;
pub(crate) const SD_SEND: i32 = 1;
pub(crate) const SD_BOTH: i32 = 2;

/// `recv`/`send` after a local `shutdown` of that direction.
pub(crate) const WSAESHUTDOWN: i32 = 10058;
/// A non-blocking `recv`/`send` with no data/space right now. Expected once
/// `WSAEventSelect` flips the socket non-blocking; the caller parks on the
/// readiness event and retries.
pub(crate) const WSAEWOULDBLOCK: i32 = 10035;

/// `WSAEVENT` — an opaque manual-reset event `HANDLE`.
pub(crate) type WsaEvent = isize;

/// `WSACreateEvent` failure sentinel (`NULL`).
pub(crate) const WSA_INVALID_EVENT: WsaEvent = 0;
/// `WSAWaitForMultipleEvents` results.
pub(crate) const WSA_WAIT_FAILED: u32 = 0xFFFF_FFFF;
pub(crate) const WSA_WAIT_TIMEOUT: u32 = 258;
/// `dwTimeout` sentinel: wait with no deadline.
pub(crate) const WSA_INFINITE: u32 = 0xFFFF_FFFF;

/// `afunix.h`: `SIO_AF_UNIX_GETPEERPID` = `_WSAIOR(IOC_VENDOR, 256)` —
/// `IOC_OUT` (0x4000_0000) | `IOC_VENDOR` (0x1800_0000) | 256. Answers the
/// connected peer's pid as afunix recorded it at connect (a `ULONG`).
pub(crate) const SIO_AF_UNIX_GETPEERPID: u32 = 0x5800_0100;

// `WSAEventSelect` network-event bits.
pub(crate) const FD_READ: i32 = 0x01;
pub(crate) const FD_WRITE: i32 = 0x02;
pub(crate) const FD_CLOSE: i32 = 0x20;

/// `sockaddr_un`: family + a NUL-terminated path of at most 107 bytes.
#[repr(C)]
pub(crate) struct SockaddrUn {
    pub sun_family: u16,
    pub sun_path: [u8; 108],
}

/// `WSANETWORKEVENTS`: the pending network-event bitmask plus a per-event
/// error array. Only `lNetworkEvents` is read; the errors surface anyway on
/// the following `recv`/`send`.
#[repr(C)]
pub(crate) struct WsaNetworkEvents {
    pub lNetworkEvents: i32,
    pub _iErrorCode: [i32; 10],
}

/// `FILETIME`: 100 ns ticks since 1601-01-01 UTC, split in two halves.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(crate) struct FileTime {
    pub dwLowDateTime: u32,
    pub dwHighDateTime: u32,
}

impl FileTime {
    /// The two halves as one tick count.
    pub(crate) fn ticks(self) -> u64 {
        (u64::from(self.dwHighDateTime) << 32) | u64::from(self.dwLowDateTime)
    }
}

/// Opaque, oversized stand-in for `WSADATA` (408 bytes on x64); `WSAStartup`
/// only writes into it and we never read it back.
#[repr(C)]
pub(crate) struct WsaData {
    pub _opaque: [u8; 512],
}

#[link(name = "ws2_32")]
unsafe extern "system" {
    pub(crate) fn WSAStartup(wVersionRequested: u16, lpWSAData: *mut WsaData) -> i32;
    pub(crate) fn WSAGetLastError() -> i32;
    pub(crate) fn socket(af: i32, ty: i32, protocol: i32) -> RawSocket;
    pub(crate) fn bind(s: RawSocket, name: *const SockaddrUn, namelen: i32) -> i32;
    pub(crate) fn listen(s: RawSocket, backlog: i32) -> i32;
    pub(crate) fn accept(s: RawSocket, addr: *mut SockaddrUn, addrlen: *mut i32) -> RawSocket;
    pub(crate) fn connect(s: RawSocket, name: *const SockaddrUn, namelen: i32) -> i32;
    pub(crate) fn recv(s: RawSocket, buf: *mut u8, len: i32, flags: i32) -> i32;
    pub(crate) fn send(s: RawSocket, buf: *const u8, len: i32, flags: i32) -> i32;
    /// `lpBuffers` is an `LPWSABUF`: std guarantees an `IoSlice` is laid out
    /// as a `WSABUF`, and a send only reads the array. No overlapped I/O.
    pub(crate) fn WSASend(
        s: RawSocket,
        lpBuffers: *const std::io::IoSlice<'_>,
        dwBufferCount: u32,
        lpNumberOfBytesSent: *mut u32,
        dwFlags: u32,
        lpOverlapped: *mut core::ffi::c_void,
        lpCompletionRoutine: *mut core::ffi::c_void,
    ) -> i32;
    pub(crate) fn shutdown(s: RawSocket, how: i32) -> i32;
    pub(crate) fn closesocket(s: RawSocket) -> i32;
    pub(crate) fn WSACreateEvent() -> WsaEvent;
    pub(crate) fn WSACloseEvent(hEvent: WsaEvent) -> i32;
    pub(crate) fn WSASetEvent(hEvent: WsaEvent) -> i32;
    pub(crate) fn WSAEventSelect(s: RawSocket, hEventObject: WsaEvent, lNetworkEvents: i32) -> i32;
    pub(crate) fn WSAEnumNetworkEvents(
        s: RawSocket,
        hEventObject: WsaEvent,
        lpNetworkEvents: *mut WsaNetworkEvents,
    ) -> i32;
    pub(crate) fn WSAIoctl(
        s: RawSocket,
        dwIoControlCode: u32,
        lpvInBuffer: *mut core::ffi::c_void,
        cbInBuffer: u32,
        lpvOutBuffer: *mut core::ffi::c_void,
        cbOutBuffer: u32,
        lpcbBytesReturned: *mut u32,
        lpOverlapped: *mut core::ffi::c_void,
        lpCompletionRoutine: *mut core::ffi::c_void,
    ) -> i32;
    pub(crate) fn WSAWaitForMultipleEvents(
        cEvents: u32,
        lphEvents: *const WsaEvent,
        fWaitAll: i32,
        dwTimeout: u32,
        fAlertable: i32,
    ) -> u32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    pub(crate) fn GetLastError() -> u32;
    /// Clears the thread's error before a call whose failure only the error
    /// tells apart (`GetFileType`'s `FILE_TYPE_UNKNOWN`).
    pub(crate) fn SetLastError(dwErrCode: u32);
    pub(crate) fn GetShortPathNameW(
        lpszLongPath: *const u16,
        lpszShortPath: *mut u16,
        cchBuffer: u32,
    ) -> u32;
    pub(crate) fn OpenProcess(
        dwDesiredAccess: u32,
        bInheritHandle: i32,
        dwProcessId: u32,
    ) -> RawHandle;
    pub(crate) fn GetExitCodeProcess(hProcess: RawHandle, lpExitCode: *mut u32) -> i32;
    /// `WAIT_TIMEOUT` at `dwMilliseconds` 0: the process has not exited.
    pub(crate) fn WaitForSingleObject(hHandle: RawHandle, dwMilliseconds: u32) -> u32;
    /// A process's creation, exit, kernel and user times (`crate::process`).
    pub(crate) fn GetProcessTimes(
        hProcess: RawHandle,
        lpCreationTime: *mut FileTime,
        lpExitTime: *mut FileTime,
        lpKernelTime: *mut FileTime,
        lpUserTime: *mut FileTime,
    ) -> i32;
    pub(crate) fn CloseHandle(hObject: RawHandle) -> i32;
    /// The pids attached to this process's console; 0 when it has none
    /// (`crate::spawn::have_console`).
    pub(crate) fn GetConsoleProcessList(lpdwProcessList: *mut u32, dwProcessCount: u32) -> u32;
    /// This process's pseudo-handle (`crate::elevation`).
    pub(crate) fn GetCurrentProcess() -> RawHandle;
    /// One of this machine's names (`crate::process`: its DNS host name, its
    /// NetBIOS name), by `COMPUTER_NAME_FORMAT` (`name_type`). `size` is the
    /// buffer's length in UTF-16 units, NUL included; on success it is set to
    /// the name's length without the NUL.
    pub(crate) fn GetComputerNameExW(name_type: u32, buffer: *mut u16, size: *mut u32) -> i32;
    /// psapi's `EnumProcessModulesEx`, exported by kernel32 under this name.
    pub(crate) fn K32EnumProcessModulesEx(
        hProcess: RawHandle,
        lphModule: *mut usize,
        cb: u32,
        lpcbNeeded: *mut u32,
        dwFilterFlag: u32,
    ) -> i32;
    /// psapi's `GetModuleFileNameExW`, exported by kernel32 under this name.
    pub(crate) fn K32GetModuleFileNameExW(
        hProcess: RawHandle,
        hModule: usize,
        lpFilename: *mut u16,
        nSize: u32,
    ) -> u32;
    /// psapi's `GetMappedFileNameW`, exported by kernel32 under this name: the
    /// kernel's name (`\Device\HarddiskVolume3\…`) of the file mapped at
    /// `lpv` in `hProcess` — the file the section was made from, its path
    /// as the open resolved it (a junction or link followed), unlike the
    /// loader's record of a module, which keeps the path as it was asked for.
    /// The characters copied, 0 on failure.
    pub(crate) fn K32GetMappedFileNameW(
        hProcess: RawHandle,
        lpv: *const core::ffi::c_void,
        lpFilename: *mut u16,
        nSize: u32,
    ) -> u32;
    pub(crate) fn DuplicateHandle(
        hSourceProcessHandle: RawHandle,
        hSourceHandle: RawHandle,
        hTargetProcessHandle: RawHandle,
        lpTargetHandle: *mut RawHandle,
        dwDesiredAccess: u32,
        bInheritHandle: i32,
        dwOptions: u32,
    ) -> i32;
    /// What kind of object a file handle names (`FILE_TYPE_DISK` for a file or
    /// directory on a volume), answered from its device, never from its name.
    pub(crate) fn GetFileType(hFile: RawHandle) -> u32;
    pub(crate) fn GetFinalPathNameByHandleW(
        hFile: RawHandle,
        lpszFilePath: *mut u16,
        cchFilePath: u32,
        dwFlags: u32,
    ) -> u32;
    /// Maps a module into this process (the scan's tests: a child that maps a
    /// module from under a directory, `DONT_RESOLVE_DLL_REFERENCES` so none of
    /// its code runs). 0 on failure.
    #[cfg(test)]
    pub(crate) fn LoadLibraryExW(
        lpLibFileName: *const u16,
        hFile: RawHandle,
        dwFlags: u32,
    ) -> isize;
    /// Waits for a change under the directory `hDirectory` (the scan's tests: a
    /// child that blocks in a synchronous watch, holding its file object's lock).
    #[cfg(test)]
    pub(crate) fn ReadDirectoryChangesW(
        hDirectory: RawHandle,
        lpBuffer: *mut core::ffi::c_void,
        nBufferLength: u32,
        bWatchSubtree: i32,
        dwNotifyFilter: u32,
        lpBytesReturned: *mut u32,
        lpOverlapped: *mut core::ffi::c_void,
        lpCompletionRoutine: *mut core::ffi::c_void,
    ) -> i32;
}

#[link(name = "ntdll")]
unsafe extern "system" {
    /// One class of facts about any object, read through a handle: here its
    /// kernel name (`ObjectNameInformation`, a [`UnicodeString`] followed by
    /// its characters), for a file handle no file name query answers. Returns
    /// an NTSTATUS.
    pub(crate) fn NtQueryObject(
        Handle: RawHandle,
        ObjectInformationClass: u32,
        ObjectInformation: *mut core::ffi::c_void,
        ObjectInformationLength: u32,
        ReturnLength: *mut u32,
    ) -> i32;
    /// One class of system-wide facts: here every open handle of every process
    /// (`SystemExtendedHandleInformation`). Returns an NTSTATUS.
    pub(crate) fn NtQuerySystemInformation(
        SystemInformationClass: u32,
        SystemInformation: *mut core::ffi::c_void,
        SystemInformationLength: u32,
        ReturnLength: *mut u32,
    ) -> i32;
}

#[link(name = "wtsapi32")]
unsafe extern "system" {
    /// Every process of every session with its owner's SID, read without
    /// opening any of them (the session manager answers).
    pub(crate) fn WTSEnumerateProcessesExW(
        hServer: RawHandle,
        pLevel: *mut u32,
        SessionId: u32,
        ppProcessInfo: *mut *mut WtsProcessInfoExW,
        pCount: *mut u32,
    ) -> i32;
    pub(crate) fn WTSFreeMemoryExW(
        WTSTypeClass: u32,
        pMemory: *mut core::ffi::c_void,
        NumberOfEntries: u32,
    ) -> i32;
}

/// `WTS_PROCESS_INFO_EXW`: one process of a `WTSEnumerateProcessesExW` listing
/// at level 1.
#[repr(C)]
pub(crate) struct WtsProcessInfoExW {
    pub SessionId: u32,
    pub ProcessId: u32,
    pub pProcessName: *mut u16,
    /// The owner's SID, inside the listing's own allocation; null when the
    /// session manager could not read it.
    pub pUserSid: *mut core::ffi::c_void,
    pub NumberOfThreads: u32,
    pub HandleCount: u32,
    pub PagefileUsage: u32,
    pub PeakPagefileUsage: u32,
    pub WorkingSetSize: u32,
    pub PeakWorkingSetSize: u32,
    pub UserTime: i64,
    pub KernelTime: i64,
}

/// `UNICODE_STRING` (64-bit layout): a counted UTF-16 string, as
/// `NtQueryObject(ObjectNameInformation)` answers a name (`Length` in bytes,
/// `Buffer` pointing into the caller's own buffer, after this header).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(crate) struct UnicodeString {
    pub Length: u16,
    pub MaximumLength: u16,
    pub Buffer: usize,
}

/// `SYSTEM_HANDLE_TABLE_ENTRY_INFO_EX`: one open handle of one process
/// (`SystemExtendedHandleInformation`); the listing is a two-word header
/// (`NumberOfHandles`, a reserved word) and then these.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(crate) struct SystemHandleEntryEx {
    pub Object: usize,
    pub UniqueProcessId: usize,
    pub HandleValue: usize,
    pub GrantedAccess: u32,
    pub CreatorBackTraceIndex: u16,
    pub ObjectTypeIndex: u16,
    pub HandleAttributes: u32,
    pub Reserved: u32,
}

#[link(name = "advapi32")]
unsafe extern "system" {
    pub(crate) fn OpenProcessToken(
        ProcessHandle: RawHandle,
        DesiredAccess: u32,
        TokenHandle: *mut RawHandle,
    ) -> i32;
    pub(crate) fn GetTokenInformation(
        TokenHandle: RawHandle,
        TokenInformationClass: u32,
        TokenInformation: *mut core::ffi::c_void,
        TokenInformationLength: u32,
        ReturnLength: *mut u32,
    ) -> i32;
    pub(crate) fn EqualSid(pSid1: *const core::ffi::c_void, pSid2: *const core::ffi::c_void)
    -> i32;
    pub(crate) fn IsValidSid(pSid: *const core::ffi::c_void) -> i32;
    /// A security descriptor from its SDDL text (the scan's tests: a child
    /// that refuses the scan's reads of it), allocated with `LocalAlloc`.
    #[cfg(test)]
    pub(crate) fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
        StringSecurityDescriptor: *const u16,
        StringSDRevision: u32,
        SecurityDescriptor: *mut *mut core::ffi::c_void,
        SecurityDescriptorSize: *mut u32,
    ) -> i32;
    /// Replaces part of an object's security (the scan's tests: the DACL of
    /// the child's own process object).
    #[cfg(test)]
    pub(crate) fn SetKernelObjectSecurity(
        Handle: RawHandle,
        SecurityInformation: u32,
        SecurityDescriptor: *const core::ffi::c_void,
    ) -> i32;
}

#[link(name = "bcrypt")]
unsafe extern "system" {
    pub(crate) fn BCryptGenRandom(
        hAlgorithm: *mut core::ffi::c_void,
        pbBuffer: *mut u8,
        cbBuffer: u32,
        dwFlags: u32,
    ) -> i32;
}

/// The hand-declared structures the path-user scan reads have the sizes and
/// field offsets of their SDK definitions on a 64-bit Windows.
#[cfg(all(test, target_pointer_width = "64"))]
mod layout_tests {
    use super::{SystemHandleEntryEx, UnicodeString, WtsProcessInfoExW};
    use std::mem::{offset_of, size_of};

    #[test]
    fn the_scan_structures_have_their_sdk_layouts() {
        assert_eq!(size_of::<WtsProcessInfoExW>(), 64);
        assert_eq!(offset_of!(WtsProcessInfoExW, ProcessId), 4);
        assert_eq!(offset_of!(WtsProcessInfoExW, pUserSid), 16);
        assert_eq!(offset_of!(WtsProcessInfoExW, UserTime), 48);
        assert_eq!(size_of::<SystemHandleEntryEx>(), 40);
        assert_eq!(offset_of!(SystemHandleEntryEx, UniqueProcessId), 8);
        assert_eq!(offset_of!(SystemHandleEntryEx, HandleValue), 16);
        assert_eq!(offset_of!(SystemHandleEntryEx, ObjectTypeIndex), 30);
        assert_eq!(size_of::<UnicodeString>(), 16);
        assert_eq!(offset_of!(UnicodeString, Buffer), 8);
    }
}
