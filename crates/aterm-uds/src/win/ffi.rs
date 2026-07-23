// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Raw Win32 declarations for the AF_UNIX-over-winsock transport — the ONLY
//! FFI in this crate, kept in one auditable file. `ws2_32` (sockets),
//! `kernel32` (short-path fallback + pid liveness), `bcrypt` (CSPRNG). All
//! import libraries ship with every Windows SDK, so `#[link]` is clean on
//! stable with zero dependencies.

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
    pub(crate) fn CloseHandle(hObject: RawHandle) -> i32;
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
