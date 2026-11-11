// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
    aterm_uds::accept(
        &listener,
        aterm_uds::AcceptWait::Forever,
        aterm_uds::AcceptedMode::Blocking,
    )
    .unwrap();
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

/// Address parsing rejects empty parameter slots, invalid transport/key tokens, and raw
/// control bytes, and the `XDG_RUNTIME_DIR` fallback refuses relative, shared, or symlinked
/// paths.
/// NEGATIVE CONTROL: `filter(|p| !p.is_empty())` and `path.exists()` admit `unix:path=/a,,guid=1`,
/// relative `XDG_RUNTIME_DIR`, world-writable directories, and symlinked `bus` entries.
#[test]
fn private_runtime_dir_and_address_syntax_guards_hold() {
    for bad in [
        "unix:path=/a,,guid=0f3b",
        "unix:,path=/a",
        "unix:path=/a,",
        "un ix:path=/a",
        "unix:pa th=/a",
        "unix:path=/a\nb",
    ] {
        assert!(matches!(parse(bad), Err(Error::Invalid(_))), "{bad:?}");
    }
    assert!(private_runtime_bus(std::path::Path::new("relative-runtime")).is_none());
    let scratch = aterm_tempfile::Builder::new()
        .prefix("dbus-rt-")
        .tempdir()
        .unwrap();
    let bus = scratch.path().join("bus");
    std::fs::write(&bus, b"sentinel").unwrap();
    assert_eq!(private_runtime_bus(scratch.path()).as_deref(), bus.to_str());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let target = scratch.path().join("real-bus");
        std::fs::write(&target, b"sentinel").unwrap();
        std::fs::remove_file(&bus).unwrap();
        std::os::unix::fs::symlink(&target, &bus).unwrap();
        assert!(
            private_runtime_bus(scratch.path()).is_none(),
            "symlinked $XDG_RUNTIME_DIR/bus must be refused"
        );
        std::fs::remove_file(&bus).unwrap();
        std::fs::write(&bus, b"sentinel").unwrap();
        std::fs::set_permissions(scratch.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            private_runtime_bus(scratch.path()).is_none(),
            "world/group-readable XDG_RUNTIME_DIR must be refused"
        );
    }
}
