// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm lan`, hidden (docs/DESIGN-lan-broker-2026-11-05.md): `serve --source <bundle>`, the
//! root LAN broker its LaunchDaemon runs; `status`, the broker's hello for `aterm doctor` and
//! the lab lane; and `fdpass <host> <port> <seconds>`, the `ProxyCommand` of every `ssh` the
//! machines controller runs, which connects through the broker and passes the connection to
//! `ssh` ([`aterm_machines::controller::lan_proxy`]).

use std::process::ExitCode;

/// The verb's first word.
pub(crate) const VERB: &str = "lan";

const USAGE: &str = "usage: aterm lan serve --source <app bundle> | aterm lan status | aterm lan \
                     fdpass <host> <port> <seconds>";

/// `aterm lan <args>`.
pub(crate) fn run(args: &[std::ffi::OsString]) -> ExitCode {
    match args {
        [verb, flag, source] if verb == "serve" && flag == "--source" => {
            serve(&source.to_string_lossy())
        }
        [verb] if verb == "status" => status(),
        [verb, host, port, seconds] if verb == "fdpass" => fdpass(
            &host.to_string_lossy(),
            &port.to_string_lossy(),
            &seconds.to_string_lossy(),
        ),
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

/// The broker, as root on macOS until its process ends.
fn serve(source: &str) -> ExitCode {
    if !cfg!(target_os = "macos") {
        eprintln!("aterm lan serve: macOS only (no Local Network privacy gates aterm elsewhere)");
        return ExitCode::from(2);
    }
    let opts = aterm_lan::Opts {
        source: source.into(),
        socket: aterm_uds::lan::socket_path(),
    };
    match aterm_lan::serve(opts) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("aterm lan serve: {error}");
            ExitCode::FAILURE
        }
    }
}

/// The connection `ssh`'s `ProxyCommand` asks for, passed over standard output; on failure the
/// line `ssh` would have printed, on standard error, which `ssh` shows as its own.
fn fdpass(host: &str, port: &str, seconds: &str) -> ExitCode {
    match aterm_machines::controller::lan_proxy::serve(host, port, seconds) {
        Ok(()) => ExitCode::SUCCESS,
        Err(line) => {
            eprintln!("{line}");
            ExitCode::FAILURE
        }
    }
}

/// One line: the serving broker's protocol, build and socket, or why there is none.
fn status() -> ExitCode {
    let socket = aterm_uds::lan::socket_path();
    match aterm_uds::lan::broker::hello() {
        // No broker: this process served itself under macOS's allowance (§5's direct path).
        Ok(aterm_uds::lan::BrokerHello {
            direct: Some(allowed),
            ..
        }) => {
            println!(
                "the LAN broker is not installed: until setup's Mac password installs it, aterm \
                 makes its own LAN connections as {allowed} (socket={})",
                socket.display()
            );
            ExitCode::FAILURE
        }
        Ok(hello) => {
            println!(
                "broker protocol={} build={} socket={}",
                hello.protocol,
                hello.build,
                socket.display()
            );
            ExitCode::SUCCESS
        }
        Err(why) => {
            println!("{why} (socket={})", socket.display());
            ExitCode::FAILURE
        }
    }
}
