// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Observe the real issued SDK controller before any credential is Ready.
//! This retains no source or provider grant and cannot admit a backend.

use aterm_agent::harness::{
    codex_session_slot::Caller,
    codex_source_registry::{self as registry, FrontendController, Registry},
    native_act::Process,
};
use std::os::fd::{AsRawFd as _, BorrowedFd};

pub(super) struct Observation {
    pub caller: Caller,
    pub before_ready: bool,
    pub foreground_denied: bool,
    controller: FrontendController,
    process: Process,
}

impl Observation {
    pub fn capture(
        registry: &Registry,
        sid: &str,
        pid: u32,
        connection: u64,
        outcome: &aterm_json::Value,
        master: BorrowedFd<'_>,
    ) -> Result<Self, registry::Error> {
        let process = Process::observe(pid).ok_or(registry::Error::Unavailable)?;
        let caller = Caller {
            sid: sid.into(),
            pid,
            birth: process.birth,
        };
        let args = atpkg::caller_shell::process_args(pid).ok_or(registry::Error::Unavailable)?;
        let token = args
            .env_var(registry::LAUNCH_ENV)
            .ok_or(registry::Error::Denied)?;
        let peer = registry.reserved_peer(&caller, token)?;
        let connection = registry.connection(connection)?;
        let member = outcome["member"].as_str().ok_or(registry::Error::Denied)?;
        let operation = outcome["operation"]
            .as_str()
            .ok_or(registry::Error::Denied)?;
        // DATA from the reply identifies an allocation only. The real registry
        // independently checks its actual peer, connection and issued operation.
        let controller = registry.frontend_controller(&peer, &connection, member, operation)?;
        // Independent control precedes the first credential actor. Requiring
        // a Source census here would discard the actual controller precisely
        // when its intended pre-credential issuance succeeds.
        let before_ready = !controller.credential_construction_started()? && controller.current();
        // This SDK child has piped protocol I/O, while the isolated shell owns
        // the foreground PTY. Control membership must not imply foreground.
        // SAFETY: the host retains this borrowed PTY for the entire observation.
        let foreground = unsafe { libc::tcgetpgrp(master.as_raw_fd()) };
        let foreground_denied = foreground > 0
            && i64::from(foreground) != i64::from(pid)
            && controller.foreground(master).is_err()
            && controller.current();
        Ok(Self {
            caller,
            before_ready,
            foreground_denied,
            controller,
            process,
        })
    }

    pub fn current(&self) -> bool {
        self.controller.current()
    }

    pub fn retired_while_live(&self) -> bool {
        Process::observe(self.caller.pid).as_ref() == Some(&self.process)
            && self.controller.withdrawal().is_ok()
    }
}
