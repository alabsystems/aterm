// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! D-Bus server addresses (`transport:key=value,...` entries separated by `;`).

use crate::error::{Error, Result};

/// One address entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Address {
    pub transport: String,
    /// The keys and unescaped values, in order.
    pub params: Vec<(String, String)>,
}

impl Address {
    /// `unix:path=<path>`.
    #[must_use]
    pub fn unix_path(path: &str) -> Self {
        Self {
            transport: "unix".into(),
            params: vec![("path".into(), path.into())],
        }
    }

    /// The value of `key`, if present.
    #[inline]
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// The server GUID the address promises, which the handshake must match.
    #[inline]
    #[must_use]
    pub fn guid(&self) -> Option<&str> {
        self.get("guid")
    }
}

fn unescape(text: &str) -> Result<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        if b == b'%' {
            let pair = bytes.get(i.saturating_add(1)..i.saturating_add(3));
            let pair = pair.ok_or(Error::Invalid("truncated %-escape in an address"))?;
            let mut byte = [0u8; 1];
            if !aterm_codec::hex::decode_into(pair, &mut byte) {
                return Err(Error::Invalid("bad %-escape in an address"));
            }
            out.extend_from_slice(&byte);
            i = i.saturating_add(3);
        } else {
            out.push(b);
            i = i.saturating_add(1);
        }
    }
    String::from_utf8(out).map_err(|_| Error::Invalid("address value is not UTF-8"))
}

/// Parses an address list, or returns [`Error::Invalid`] for a malformed entry.
pub fn parse(list: &str) -> Result<Vec<Address>> {
    let mut out = Vec::new();
    for entry in list.split(';').filter(|e| !e.is_empty()) {
        let (transport, rest) = entry
            .split_once(':')
            .ok_or(Error::Invalid("address entry without a transport"))?;
        if transport.is_empty() {
            return Err(Error::Invalid("address entry without a transport"));
        }
        let mut params: Vec<(String, String)> = Vec::new();
        for pair in rest.split(',').filter(|p| !p.is_empty()) {
            let (key, value) = pair
                .split_once('=')
                .ok_or(Error::Invalid("address key without a value"))?;
            if key.is_empty() || params.iter().any(|(k, _)| k == key) {
                return Err(Error::Invalid("address key empty or repeated"));
            }
            params.push((key.to_owned(), unescape(value)?));
        }
        out.push(Address {
            transport: transport.to_owned(),
            params,
        });
    }
    if out.is_empty() {
        return Err(Error::Invalid("empty address"));
    }
    Ok(out)
}

/// Resolves session bus addresses, or returns [`Error::Invalid`] for a malformed or non-UTF-8
/// `DBUS_SESSION_BUS_ADDRESS` or [`Error::Io`] when none is configured.
pub fn session_bus() -> Result<Vec<Address>> {
    match std::env::var("DBUS_SESSION_BUS_ADDRESS") {
        Ok(list) if !list.is_empty() => return parse(&list),
        Ok(_) | Err(std::env::VarError::NotPresent) => {}
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err(Error::Invalid("session bus address is not UTF-8"));
        }
    }
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        let path = std::path::Path::new(&dir).join("bus");
        if let (true, Some(text)) = (path.exists(), path.to_str()) {
            return Ok(vec![Address::unix_path(text)]);
        }
    }
    Err(Error::Io(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "no session bus: DBUS_SESSION_BUS_ADDRESS is unset and $XDG_RUNTIME_DIR/bus does not exist",
    )))
}

/// Connects a unix-socket address by `deadline` (`None` waits as long as it takes).
///
/// # Errors
/// [`Error::Unsupported`] for another transport or a unix address without `path=`
/// (or, on Linux, `abstract=`); [`Error::Invalid`] when a unix address names more than one
/// location key or an empty socket name; [`Error::Timeout`] once `deadline` passes (on Linux a full
/// backlog holds the connect); [`Error::Io`] when the connect fails.
#[cfg(unix)]
pub fn connect(
    address: &Address,
    deadline: Option<std::time::Instant>,
) -> Result<aterm_uds::CtlStream> {
    if address.transport != "unix" {
        return Err(Error::Unsupported("a transport other than unix"));
    }
    let keys = u8::from(address.get("path").is_some())
        + u8::from(address.get("abstract").is_some())
        + u8::from(address.get("tmpdir").is_some())
        + u8::from(address.get("dir").is_some())
        + u8::from(address.get("runtime").is_some());
    if keys > 1 {
        return Err(Error::Invalid(
            "unix address names more than one socket key",
        ));
    }
    #[cfg(target_os = "linux")]
    if let Some(path) = address.get("path") {
        if path.is_empty() {
            return Err(Error::Invalid("empty unix socket address"));
        }
        return connect_until(path.as_bytes(), false, deadline);
    }
    #[cfg(not(target_os = "linux"))]
    if let Some(path) = address.get("path") {
        if path.is_empty() {
            return Err(Error::Invalid("empty unix socket address"));
        }
        // Darwin refuses a full backlog at once; only a spent deadline is left to check.
        crate::conn::time_left(deadline)?;
        return Ok(aterm_uds::CtlStream::connect(path)?);
    }
    #[cfg(target_os = "linux")]
    if let Some(name) = address.get("abstract") {
        if name.is_empty() {
            return Err(Error::Invalid("empty unix socket address"));
        }
        return connect_until(name.as_bytes(), true, deadline);
    }
    Err(Error::Unsupported(
        "a unix address without path= (abstract= is Linux-only)",
    ))
}

/// A blocking connect to a path or abstract `name`, its wait on a full backlog ended at `deadline` by `SO_SNDTIMEO`.
#[cfg(target_os = "linux")]
// Skip: bottoms out at the socket(2), setsockopt(2) and connect(2) FFI calls.
#[cfg_attr(trust_verify, trust::skip)]
#[expect(
    unsafe_code,
    reason = "socket(2), setsockopt(2) and connect(2) on the one descriptor this function owns"
)]
fn connect_until(
    name: &[u8],
    abstract_name: bool,
    deadline: Option<std::time::Instant>,
) -> Result<aterm_uds::CtlStream> {
    use std::io::{Error as IoError, ErrorKind};
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};

    let mut addr = libc::sockaddr_un {
        sun_family: libc::AF_UNIX as libc::sa_family_t,
        sun_path: [0; 108],
    };
    if name.len() >= addr.sun_path.len() || (!abstract_name && name.contains(&0)) {
        return Err(Error::Io(IoError::new(
            ErrorKind::InvalidInput,
            "socket name too long for sun_path, or a path with a NUL",
        )));
    }
    let start = usize::from(abstract_name);
    for (slot, byte) in addr.sun_path.iter_mut().skip(start).zip(name) {
        *slot = libc::c_char::from_ne_bytes([*byte]);
    }
    let len =
        (std::mem::offset_of!(libc::sockaddr_un, sun_path) + name.len() + 1) as libc::socklen_t;
    // SAFETY: socket(2) reads no memory; the descriptor it returns is owned at once below.
    let raw = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
    if raw < 0 {
        return Err(Error::Io(IoError::last_os_error()));
    }
    // SAFETY: `raw` is the open descriptor socket(2) just returned, and nothing else owns it.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    loop {
        if let Some(left) = crate::conn::time_left(deadline)? {
            let micros = left.as_micros().max(1);
            let limit = libc::timeval {
                tv_sec: libc::time_t::try_from(micros / 1_000_000).unwrap_or(libc::time_t::MAX),
                tv_usec: libc::suseconds_t::try_from(micros % 1_000_000).unwrap_or_default(),
            };
            // SAFETY: `limit` is a live timeval of the length passed, and the call only reads it.
            let set = unsafe {
                libc::setsockopt(
                    fd.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_SNDTIMEO,
                    (&raw const limit).cast(),
                    size_of::<libc::timeval>() as libc::socklen_t,
                )
            };
            if set != 0 {
                return Err(Error::Io(IoError::last_os_error()));
            }
        }
        // SAFETY: `addr` is an initialized sockaddr_un; `len`, within it as `name` is shorter
        // than `sun_path`, covers the family, the name and its one NUL (a path's last byte, an
        // abstract name's first).
        if unsafe { libc::connect(fd.as_raw_fd(), (&raw const addr).cast(), len) } == 0 {
            return Ok(aterm_uds::CtlStream::from(fd));
        }
        let error = IoError::last_os_error();
        match error.kind() {
            ErrorKind::Interrupted => {}
            ErrorKind::WouldBlock => return Err(Error::Timeout),
            _ => return Err(Error::Io(error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_list_parses_in_order_with_escapes() {
        let list = parse("unix:path=/run/user/1000/at-spi/bus_0,guid=0f3b;unix:abstract=/tmp/dbus-%41b%2c;tcp:host=x").unwrap();
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].get("path"), Some("/run/user/1000/at-spi/bus_0"));
        assert_eq!(list[0].guid(), Some("0f3b"));
        assert_eq!(list[1].get("abstract"), Some("/tmp/dbus-Ab,"));
        assert_eq!(list[2].transport, "tcp");
        assert_eq!(
            parse("unix:path=/a;").unwrap(),
            vec![Address::unix_path("/a")]
        );
    }

    #[test]
    fn malformed_lists_are_refused() {
        for bad in [
            "",
            ";",
            "unix",
            ":path=/a",
            "unix:path",
            "unix:path=/a,path=/b",
            "unix:=x",
            "unix:path=%4",
            "unix:path=%zz",
        ] {
            assert!(matches!(parse(bad), Err(Error::Invalid(_))), "{bad}");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn an_abstract_name_connects() {
        use std::os::linux::net::SocketAddrExt as _;
        use std::os::unix::net::{SocketAddr, UnixListener};
        let name = format!("aterm-dbus-abstract-{}", std::process::id());
        let at = SocketAddr::from_abstract_name(name.as_bytes()).unwrap();
        let listener = UnixListener::bind_addr(&at).unwrap();
        let address = &parse(&format!("unix:abstract={name}")).unwrap()[0];
        connect(address, None).unwrap();
        listener.accept().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn only_client_unix_keys_connect() {
        for listen_only in [
            "unix:dir=/tmp",
            "unix:tmpdir=/tmp",
            "unix:runtime=yes",
            "tcp:host=localhost,port=1",
        ] {
            let address = &parse(listen_only).unwrap()[0];
            assert!(
                matches!(connect(address, None), Err(Error::Unsupported(_))),
                "{listen_only}"
            );
        }
        for invalid in [
            "unix:path=/a,abstract=b",
            "unix:path=/a,tmpdir=/tmp",
            "unix:path=",
        ] {
            let address = &parse(invalid).unwrap()[0];
            assert!(
                matches!(connect(address, None), Err(Error::Invalid(_))),
                "{invalid}"
            );
        }
    }
}
