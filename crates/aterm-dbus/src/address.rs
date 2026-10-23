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

/// Connects a unix-socket address.
///
/// # Errors
/// [`Error::Unsupported`] for another transport or a unix address without `path=`
/// (or, on Linux, `abstract=`); [`Error::Io`] when the connect fails.
#[cfg(unix)]
pub fn connect(address: &Address) -> Result<aterm_uds::CtlStream> {
    if address.transport != "unix" {
        return Err(Error::Unsupported("a transport other than unix"));
    }
    if let Some(path) = address.get("path") {
        return Ok(aterm_uds::CtlStream::connect(path)?);
    }
    #[cfg(target_os = "linux")]
    if let Some(name) = address.get("abstract") {
        use std::os::linux::net::SocketAddrExt as _;
        let addr = std::os::unix::net::SocketAddr::from_abstract_name(name.as_bytes())?;
        return Ok(aterm_uds::CtlStream::connect_addr(&addr)?);
    }
    Err(Error::Unsupported(
        "a unix address without path= (abstract= is Linux-only)",
    ))
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
                matches!(connect(address), Err(Error::Unsupported(_))),
                "{listen_only}"
            );
        }
    }
}
