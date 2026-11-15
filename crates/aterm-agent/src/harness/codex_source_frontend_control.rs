// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Native membership for backend request admission, independent of OAuth grants.
use super::*;
use std::os::fd::{AsRawFd as _, BorrowedFd};

pub(super) struct Issued {
    caller: slot::Caller,
    pub(super) process: Process,
    pub(super) connection: Connection,
    operation: String,
    pub(super) closed: bool,
}

impl Issued {
    pub(super) fn bound_to(
        &self,
        caller: &slot::Caller,
        process: &Process,
        connection: &Connection,
    ) -> bool {
        !self.closed
            && self.caller == *caller
            && self.process == *process
            && self.connection.same(connection)
            && connection.is_live()
    }
}

/// An issued SDK control member on its actual authenticated connection.
/// Credential loading and provider readiness do not establish this control role.
/// It supplies no Source, assignment, provider-use or backend attachment grant.
/// Separate foreground and independently constructed backend checks still apply.
pub struct FrontendController {
    state: Weak<Mutex<State>>,
    caller: slot::Caller,
    process: Process,
    connection: Connection,
    member: String,
    operation: String,
}

/// Native evidence that this exact control allocation was withdrawn. This is
/// not evidence that a backend process, root, model request or tool has ended.
pub struct ControllerWithdrawal<'a> {
    _controller: &'a FrontendController,
}

/// An authenticated SDK controller observed as the actual foreground process of the
/// server's borrowed PTY. A background backend's own SDK membership cannot
/// substitute for its frontend. This borrows the live request's descriptor;
/// it neither duplicates the PTY nor keeps a closed terminal alive.
pub struct FrontendMember<'a> {
    member: &'a FrontendController,
    master: BorrowedFd<'a>,
}

pub(super) struct FrontendIdentity {
    state: Weak<Mutex<State>>,
    caller: slot::Caller,
    process: Process,
    connection: Connection,
    member: String,
    operation: String,
}

impl Registry {
    /// Issue control on its own live transport, independently of Source members.
    /// The actual peer and App scope are required; this grants no provider use.
    pub fn open_frontend_controller(
        &self,
        peer: &Peer,
        connection: &Connection,
        operation: &str,
        scope: &super::super::account_work::Scope,
    ) -> Result<MemberAllocation, Error> {
        if !hex(operation, 32) || !peer.current() || !connection.is_live() || scope.cancelled() {
            return Err(Error::Denied);
        }
        self.reap_gone();
        let mut state = lock(&self.state)?;
        if !state
            .app_lifetime
            .as_ref()
            .is_some_and(|lifetime| scope.cancellation().owned_by(lifetime))
            || scope.cancelled()
            || !connection.is_live()
        {
            return Err(Error::Denied);
        }
        if let Some((member, issued)) = state
            .controllers
            .iter()
            .find(|(_, issued)| issued.caller == peer.caller && issued.operation == operation)
        {
            if issued.closed
                || issued.process != peer.process
                || !issued.connection.same(connection)
            {
                return Err(Error::Stale);
            }
            return Ok(MemberAllocation {
                member: member.clone(),
                operation: operation.into(),
            });
        }
        if state.controllers.len() >= MAX_MEMBERS {
            return Err(Error::Busy);
        }
        let member = aterm_uds::rand::hex_token::<16>().map_err(|_| Error::Unavailable)?;
        if state.controllers.contains_key(&member) {
            return Err(Error::Unavailable);
        }
        state.controllers.insert(
            member.clone(),
            Issued {
                caller: peer.caller.clone(),
                process: peer.process.clone(),
                connection: connection.clone(),
                operation: operation.into(),
                closed: false,
            },
        );
        Ok(MemberAllocation {
            member,
            operation: operation.into(),
        })
    }

    /// Withdraw only this caller's exact control allocation. Repeating closure
    /// on the original transport acknowledges the same withdrawal, not root exit.
    pub fn close_frontend_controller(
        &self,
        peer: &Peer,
        connection: &Connection,
        member: &str,
        operation: &str,
    ) -> Result<(), Error> {
        if !peer.current() || !connection.is_live() {
            return Err(Error::Stale);
        }
        let mut state = lock(&self.state)?;
        let issued = state.controllers.get_mut(member).ok_or(Error::Stale)?;
        if issued.caller != peer.caller
            || issued.process != peer.process
            || issued.operation != operation
            || !issued.connection.same(connection)
        {
            return Err(Error::Denied);
        }
        issued.closed = true;
        Ok(())
    }

    /// Observe an independently issued control allocation, never a Source member.
    /// A signed-out or policy-denied UI still needs native control. This fact
    /// grants no provider use, and cannot satisfy backend SDK construction.
    ///
    /// # Errors
    /// The peer, operation, member or connection is stale, retired or foreign.
    /// Kernel observation runs outside State.
    pub fn frontend_controller(
        &self,
        peer: &Peer,
        connection: &Connection,
        member: &str,
        operation: &str,
    ) -> Result<FrontendController, Error> {
        member_key(&peer.caller, member)?;
        if !peer.current() || !connection.is_live() {
            return Err(Error::Stale);
        }
        let proof = FrontendController {
            state: Arc::downgrade(&self.state),
            caller: peer.caller.clone(),
            process: peer.process.clone(),
            connection: connection.clone(),
            member: member.into(),
            operation: operation.into(),
        };
        {
            let state = lock(&self.state)?;
            proof.record(&state)?;
        }
        proof.check_current()?;
        Ok(proof)
    }
}

impl FrontendController {
    /// Debug qualification observation of actual credential allocation history.
    /// A controller can exist before any credential actor does. This reads its
    /// own registry without allocating a Source actor or asserting Ready/Closed.
    ///
    /// # Errors
    /// The original controller/process changed or metadata is unavailable.
    #[cfg(any(test, debug_assertions))]
    pub fn credential_construction_started(&self) -> Result<bool, Error> {
        if !self.connection.is_live()
            || Process::observe(self.caller.pid).as_ref() != Some(&self.process)
        {
            return Err(Error::Stale);
        }
        let started = {
            let state = self.state.upgrade().ok_or(Error::Stale)?;
            let state = lock(&state)?;
            self.record(&state)?;
            state.actors.contains_key(&actor_key(&self.caller))
                || state
                    .members
                    .values()
                    .any(|member| member.caller == self.caller)
        };
        self.check_current()?;
        Ok(started)
    }

    pub(super) fn identity(&self) -> FrontendIdentity {
        FrontendIdentity {
            state: self.state.clone(),
            caller: self.caller.clone(),
            process: self.process.clone(),
            connection: self.connection.clone(),
            member: self.member.clone(),
            operation: self.operation.clone(),
        }
    }
    /// Observe explicit withdrawal or the original connection's revocation.
    /// A failed liveness check alone never constructs this witness.
    ///
    /// # Errors
    /// The allocation is still live, metadata is contended/unavailable, or its
    /// original binding cannot be authenticated. None proves withdrawal.
    pub fn withdrawal(&self) -> Result<ControllerWithdrawal<'_>, Error> {
        let withdrawn = || ControllerWithdrawal { _controller: self };
        if !self.connection.is_live() {
            return Ok(withdrawn());
        }
        let state = self.state.upgrade().ok_or(Error::Stale)?;
        let state = lock(&state)?;
        let issued = state.controllers.get(&self.member).ok_or(Error::Stale)?;
        if issued.caller != self.caller
            || issued.process != self.process
            || issued.operation != self.operation
            || !issued.connection.same(&self.connection)
        {
            return Err(Error::Stale);
        }
        if issued.closed || !self.connection.is_live() {
            Ok(withdrawn())
        } else {
            Err(Error::Busy)
        }
    }

    /// Bind this member to the server's actual session PTY for a frontend request.
    /// No supervisor claim or autonomous task is required.
    ///
    /// # Errors
    /// The member is no longer current, or the kernel's foreground process
    /// group does not name this exact admitted caller.
    pub fn foreground<'a>(&'a self, master: BorrowedFd<'a>) -> Result<FrontendMember<'a>, Error> {
        let frontend = FrontendMember {
            member: self,
            master,
        };
        frontend.check_current()?;
        Ok(frontend)
    }

    /// Recheck the actual process incarnation and the same issued allocation.
    /// EOF, controller withdrawal or image replacement invalidates this observation.
    /// Credential readiness changes neither grant nor revoke this control role.
    pub fn current(&self) -> bool {
        self.check_current().is_ok()
    }

    /// Preserve transient registry contention separately from ended permission.
    /// A negative boolean predicate must never be used as an EOF observation.
    ///
    /// # Errors
    /// Busy means metadata is contended; Stale means the actual process,
    /// original connection or issued control allocation changed or ended.
    pub fn check_current(&self) -> Result<(), Error> {
        if !self.connection.is_live()
            || Process::observe(self.caller.pid).as_ref() != Some(&self.process)
        {
            return Err(Error::Stale);
        }
        let state = self.state.upgrade().ok_or(Error::Stale)?;
        let state = lock(&state)?;
        self.record(&state)
    }

    /// The native-observed caller; these identity fields alone confer no grant.
    pub fn caller(&self) -> &slot::Caller {
        &self.caller
    }

    fn record(&self, state: &State) -> Result<(), Error> {
        let issued = state.controllers.get(&self.member).ok_or(Error::Stale)?;
        if issued.caller != self.caller
            || issued.process != self.process
            || issued.closed
            || issued.operation != self.operation
            || !issued.connection.same(&self.connection)
            || !self.connection.is_live()
        {
            return Err(Error::Stale);
        }
        Ok(())
    }
}

impl FrontendMember<'_> {
    /// Recheck both observations around the kernel foreground read, outside
    /// registry metadata locks. This supplies no source or backend attachment.
    pub fn current(&self) -> bool {
        self.check_current().is_ok()
    }

    /// Check actual foreground while preserving transient metadata contention.
    ///
    /// # Errors
    /// The controller is busy/stale, or the original PTY's foreground process
    /// group no longer names this actual caller.
    pub fn check_current(&self) -> Result<(), Error> {
        self.member.check_current()?;
        // SAFETY: the native request retains this borrowed PTY descriptor.
        let foreground = unsafe { libc::tcgetpgrp(self.master.as_raw_fd()) };
        if i64::from(foreground) != i64::from(self.member.caller.pid) {
            return Err(Error::Denied);
        }
        self.member.check_current()
    }

    /// The same actual caller whose SDK allocation and foreground were checked.
    pub fn caller(&self) -> &slot::Caller {
        self.member.caller()
    }

    pub(super) fn admits(&self, state: &Weak<Mutex<State>>, sid: &str) -> bool {
        Weak::ptr_eq(state, &self.member.state) && self.caller().sid == sid && self.current()
    }

    pub(super) fn identity(&self) -> FrontendIdentity {
        self.member.identity()
    }
}

impl FrontendIdentity {
    pub(super) fn check_current(&self) -> Result<(), Error> {
        FrontendController {
            state: self.state.clone(),
            caller: self.caller.clone(),
            process: self.process.clone(),
            connection: self.connection.clone(),
            member: self.member.clone(),
            operation: self.operation.clone(),
        }
        .check_current()
    }
    pub(super) fn retired(&self) -> bool {
        !self.connection.is_live()
    }
    pub(super) fn in_namespace(&self, state: &Weak<Mutex<State>>) -> bool {
        Weak::ptr_eq(&self.state, state)
    }
    pub(super) fn same_requester(&self, other: &Self) -> bool {
        self.in_namespace(&other.state)
            && self.caller == other.caller
            && self.process == other.process
            && self.connection.same(&other.connection)
            && self.member == other.member
            && self.operation == other.operation
    }
    pub(super) fn check_foreground(&self, frontend: &FrontendMember<'_>) -> Result<(), Error> {
        if !Weak::ptr_eq(&self.state, &frontend.member.state)
            || self.caller != frontend.member.caller
            || self.process != frontend.member.process
            || !self.connection.same(&frontend.member.connection)
            || self.member != frontend.member.member
            || self.operation != frontend.member.operation
        {
            return Err(Error::Denied);
        }
        frontend.check_current()
    }
}

impl std::fmt::Debug for FrontendController {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("NativeSdkFrontendController(redacted)")
    }
}

impl std::fmt::Debug for FrontendMember<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ForegroundNativeSdkMember(redacted)")
    }
}
