// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Native custody of one per-session backend, independent of frontend images.
//! Only actual issued frontend members may receive its private attachment streams.
use super::*;
use std::net::Shutdown;

/// An independently issued frontend controller on its original connection.
/// This object alone is not foreground permission: delivery also requires the
/// live request's actual borrowed PTY through FrontendMember.
#[derive(Clone)]
pub struct ServiceFrontend(Arc<Frontend>);
struct Frontend {
    state: Weak<Mutex<State>>,
    controller: Arc<frontend_control::FrontendIdentity>,
    scope: crate::harness::account_work::Scope,
}
impl ServiceFrontend {
    /// Comparison-only identity of this issued controller. It retains no App
    /// request scope and supplies no currentness, foreground or SDK permission.
    pub fn identity(&self) -> ServiceIdentity {
        ServiceIdentity(Arc::clone(&self.0.controller))
    }
    /// Recheck the independent control allocation and actual request scope.
    ///
    /// # Errors
    /// The control allocation, process, original connection or scope ended.
    pub fn current(&self) -> Result<(), Error> {
        if self.0.scope.cancelled() {
            return Err(Error::Stale);
        }
        self.0.controller.check_current()
    }
    /// Compare actual native request owners, without establishing foreground.
    pub fn same_requester(&self, other: &Self) -> bool {
        self.0.controller.same_requester(&other.0.controller)
    }
    /// Only original connection closure permits disposal of replay custody.
    pub fn retired(&self) -> bool {
        self.0.controller.retired()
    }
}

/// Opaque native identity for comparing constructor intents. Never a grant or a
/// serialized principal; operations still require a current ServiceFrontend.
#[derive(Clone)]
pub struct ServiceIdentity(Arc<frontend_control::FrontendIdentity>);
impl ServiceIdentity {
    /// Compare original namespace, process incarnation, connection and allocation.
    pub fn same(&self, other: &Self) -> bool {
        self.0.same_requester(&other.0)
    }
}
impl Registry {
    /// Capture an independently issued control allocation, not a Source member.
    /// Delivery still requires the same caller's actual borrowed foreground PTY.
    ///
    /// # Errors
    /// The namespace, actual App operation or independent control is not current.
    pub fn service_frontend(
        &self,
        controller: &FrontendController,
        scope: &crate::harness::account_work::Scope,
    ) -> Result<ServiceFrontend, Error> {
        let identity = controller.identity();
        if !identity.in_namespace(&Arc::downgrade(&self.state)) {
            return Err(Error::Denied);
        }
        controller.check_current()?;
        {
            let state = lock(&self.state)?;
            if !state
                .app_lifetime
                .as_ref()
                .is_some_and(|lifetime| scope.cancellation().owned_by(lifetime))
            {
                return Err(Error::Denied);
            }
        }
        let frontend = ServiceFrontend(Arc::new(Frontend {
            state: Arc::downgrade(&self.state),
            controller: Arc::new(identity),
            scope: scope.clone(),
        }));
        frontend.current()?;
        Ok(frontend)
    }
}

/// A failed ownership transfer retains the original native child for settlement.
#[derive(Debug)]
pub struct OwnedServiceFailure {
    /// Refusal to transfer the original constructor/control ownership.
    pub reason: Error,
    /// The actual child, retained for explicit settlement or another transfer.
    pub worker: Box<OwnedWorker>,
}

/// Actual endpoint custody for one delivery, retained by the native request
/// owner before committing IO. Retrying this same object cannot send twice.
/// Its frontend endpoint can be taken only through its still-current service.
pub struct PreparedServiceAttachment {
    owner: Weak<()>,
    identity: Arc<()>,
    frontend: ServiceFrontend,
    backend_stream: Option<aterm_uds::CtlStream>,
    frontend_stream: Option<aterm_uds::CtlStream>,
    fence: Option<aterm_uds::CtlStream>,
    phase: DeliveryPhase,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum DeliveryPhase {
    Prepared,
    Sent,
    Failed,
}
struct Active {
    identity: Arc<()>,
    frontend: ServiceFrontend,
    fence: aterm_uds::CtlStream,
}

/// Keep this owner outside frontend processes and off the GUI thread. Dropping
/// a frontend or its pending delivery never drops this backend. Explicit stop
/// and final owner loss retain `OwnedWorker`'s actual group kill/reap contract.
pub struct OwnedService {
    worker: OwnedWorker,
    control: Arc<control::Control>,
    state: Weak<Mutex<State>>,
    sid: String,
    identity: Arc<()>,
    active: Option<Active>,
}
impl OwnedService {
    /// Observe the actual owned child before this service starts stopping it.
    /// Other actors may already have requested a stop; this supplies no cause.
    ///
    /// # Errors
    /// Original child custody or the non-reaping kernel observation is unavailable.
    #[cfg(debug_assertions)]
    pub fn observe_child_exit_status(
        &mut self,
    ) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.worker.observe_child_exit_status()
    }
    /// Debug observation of this actual owner after a frontend has withdrawn.
    /// The returned process is DATA, not an attachment or provider grant.
    ///
    /// # Errors
    /// Original child/control custody or actual SDK construction is not current.
    #[cfg(debug_assertions)]
    pub fn observe_owned_backend(&mut self) -> Result<Process, Error> {
        let process = self.worker.process()?;
        self.worker.check_control_binding()?;
        self.worker.check_sdk_construction()?;
        Ok(process)
    }
    pub(super) fn new(
        worker: OwnedWorker,
        control: Arc<control::Control>,
        state: Weak<Mutex<State>>,
        sid: String,
    ) -> Self {
        Self {
            worker,
            control,
            state,
            sid,
            identity: Arc::new(()),
            active: None,
        }
    }

    fn ready(
        &mut self,
        frontend: &ServiceFrontend,
        foreground: &FrontendMember<'_>,
    ) -> Result<(), Error> {
        frontend.current()?;
        if !Weak::ptr_eq(&self.state, &frontend.0.state) || self.sid != foreground.caller().sid {
            return Err(Error::Denied);
        }
        frontend.0.controller.check_foreground(foreground)?;
        if self.worker.process()?.pid == foreground.caller().pid {
            return Err(Error::Denied);
        }
        self.worker.check_control_binding().inspect_err(|_error| {
            #[cfg(debug_assertions)]
            eprintln!("Retained backend admission: control={_error:?}");
        })?;
        self.worker.check_sdk_construction().inspect_err(|_error| {
            #[cfg(debug_assertions)]
            eprintln!("Retained backend admission: sdk={_error:?}");
        })?;
        frontend.0.controller.check_foreground(foreground)?;
        frontend.current()
    }

    /// Allocate a fresh private stream under actual frontend and backend checks.
    /// Store the returned owner with the accepted native request before commit;
    /// never recreate it merely because an SDK response waiter was cancelled.
    ///
    /// # Errors
    /// The original backend or frontend admission is unavailable/stale, SDK or
    /// physical construction is incomplete, or private socket creation fails.
    pub fn prepare_attachment(
        &mut self,
        frontend: ServiceFrontend,
        foreground: &FrontendMember<'_>,
    ) -> Result<PreparedServiceAttachment, Error> {
        self.ready(&frontend, foreground)?;
        let (backend_stream, frontend_stream) =
            aterm_uds::kept_out(aterm_uds::CtlStream::pair).map_err(|_| Error::Unavailable)?;
        let fence =
            aterm_uds::kept_out(|| frontend_stream.try_clone()).map_err(|_| Error::Unavailable)?;
        self.ready(&frontend, foreground)?;
        Ok(PreparedServiceAttachment {
            owner: Arc::downgrade(&self.identity),
            identity: Arc::new(()),
            frontend,
            backend_stream: Some(backend_stream),
            frontend_stream: Some(frontend_stream),
            fence: Some(fence),
            phase: DeliveryPhase::Prepared,
        })
    }

    fn owns(&self, attachment: &PreparedServiceAttachment) -> Result<(), Error> {
        if !Weak::ptr_eq(&attachment.owner, &Arc::downgrade(&self.identity)) {
            return Err(Error::Denied);
        }
        Ok(())
    }

    /// Fence the predecessor, then send exactly one endpoint on the original
    /// constructor-bound socket. This synchronous one-byte SCM_RIGHTS commit
    /// has no cancellation point between sending and recording its outcome.
    /// An uncertain/failed attempt is terminal for this delivery object, while
    /// the backend and its original accepted operations remain owned.
    ///
    /// # Errors
    /// Admission changed, the object is foreign/retired/failed, fencing fails,
    /// or the original socket cannot accept the endpoint. No frontend endpoint
    /// is exposed before this succeeds and the admissions are rechecked.
    pub fn commit_attachment(
        &mut self,
        attachment: &mut PreparedServiceAttachment,
        foreground: &FrontendMember<'_>,
    ) -> Result<(), Error> {
        self.owns(attachment)?;
        self.ready(&attachment.frontend, foreground)?;
        match attachment.phase {
            DeliveryPhase::Sent => {
                return self.current_attachment(attachment);
            }
            DeliveryPhase::Failed => return Err(Error::Stale),
            DeliveryPhase::Prepared => {}
        }
        self.detach_frontend()?;
        let backend = attachment.backend_stream.as_ref().ok_or(Error::Stale)?;
        match self.control.send_attachment(backend) {
            Ok(()) => {}
            // Control returns Busy only before sending any bytes or rights.
            // Retain this exact prepared endpoint for the same delivery owner.
            Err(Error::Busy) => return Err(Error::Busy),
            Err(error) => {
                attachment.phase = DeliveryPhase::Failed;
                #[cfg(debug_assertions)]
                eprintln!("Retained native delivery: backend-receipt={error:?}");
                return Err(error);
            }
        }
        drop(attachment.backend_stream.take());
        // Record physical custody before any fallible metadata recheck. Busy
        // after this point retries admission, never the already received right.
        self.active = Some(Active {
            identity: attachment.identity.clone(),
            frontend: attachment.frontend.clone(),
            fence: attachment.fence.take().ok_or(Error::Stale)?,
        });
        attachment.phase = DeliveryPhase::Sent;
        match self.ready(&attachment.frontend, foreground) {
            Ok(()) => Ok(()),
            Err(Error::Busy) => Err(Error::Busy),
            Err(error) => {
                attachment.phase = DeliveryPhase::Failed;
                self.detach_frontend()?;
                Err(error)
            }
        }
    }

    fn current_attachment(&self, attachment: &PreparedServiceAttachment) -> Result<(), Error> {
        if !self
            .active
            .as_ref()
            .is_some_and(|active| Arc::ptr_eq(&active.identity, &attachment.identity))
        {
            return Err(Error::Stale);
        }
        Ok(())
    }

    /// Take the actual frontend endpoint once, after successful commit and a
    /// final check of this exact delivery, original child and live permissions.
    /// The native callback request owner must retain it across cancelled waits.
    ///
    /// # Errors
    /// This is not the current committed delivery, admission ended, or its
    /// unique endpoint was already transferred.
    pub fn take_frontend(
        &mut self,
        attachment: &mut PreparedServiceAttachment,
        foreground: &FrontendMember<'_>,
    ) -> Result<aterm_uds::CtlStream, Error> {
        self.owns(attachment)?;
        if attachment.phase != DeliveryPhase::Sent {
            return Err(Error::Stale);
        }
        self.current_attachment(attachment)?;
        self.ready(&attachment.frontend, foreground)?;
        attachment.frontend_stream.take().ok_or(Error::Stale)
    }

    /// Fence an attachment whose actual original frontend permission ended.
    /// The backend remains available for a newly admitted frontend.
    ///
    /// # Errors
    /// The kernel refuses to shut down the original frontend socket.
    pub fn reconcile_frontend(&mut self) -> Result<(), Error> {
        match self.active.as_ref().map(|active| active.frontend.current()) {
            Some(Err(Error::Busy)) => Err(Error::Busy),
            Some(Err(_error)) => {
                #[cfg(debug_assertions)]
                eprintln!("Retained native owner: frontend-invalidated={_error:?}");
                self.detach_frontend()
            }
            _ => Ok(()),
        }
    }

    /// Fence the current frontend, leaving original backend work untouched.
    ///
    /// # Errors
    /// The kernel refuses shutdown; the actual fence remains owned for retry.
    pub fn detach_frontend(&mut self) -> Result<(), Error> {
        if let Some(active) = &self.active {
            #[cfg(debug_assertions)]
            eprintln!("Retained native owner: frontend-fence-shutdown");
            match active.fence.shutdown(Shutdown::Both) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotConnected => {}
                Err(_) => return Err(Error::Unavailable),
            }
        }
        self.active.take();
        Ok(())
    }

    /// Stop this actual owned backend cohort after fencing its frontend.
    ///
    /// # Errors
    /// Fencing or original group settlement is incomplete; child custody stays
    /// here for retry, including a post-reap unresolved group-presence outcome.
    pub fn stop(&mut self) -> std::io::Result<()> {
        #[cfg(debug_assertions)]
        eprintln!("Retained native owner: cohort-stop");
        self.detach_frontend()
            .map_err(|_| std::io::Error::other("frontend fencing is unavailable"))?;
        self.worker.stop()
    }
}

impl Drop for OwnedService {
    fn drop(&mut self) {
        let _ = self.detach_frontend();
        // OwnedWorker retains actual group settlement, including unresolved reap.
    }
}

#[cfg(test)]
#[path = "codex_source_owned_service_admission_tests.rs"]
mod tests;
