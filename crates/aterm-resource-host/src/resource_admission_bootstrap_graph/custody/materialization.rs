// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Establish actual native workspace custody before configuration capture.
use super::*;

pub(crate) struct MaterializationTransfer {
    request: wire::MaterializationPreparation,
    context: trust_resource_controller::ContextReply,
    peer: Peer,
    active: Arc<AtomicBool>,
    deadline: Instant,
    contents_directory: Option<trust_resource_controller::FileIdentity>,
}
impl MaterializationTransfer {
    pub(crate) fn context(&self) -> &trust_resource_controller::ContextReply {
        &self.context
    }
    pub(crate) fn deadline(&self) -> Instant {
        self.deadline
    }
    /// Actual retained directory observation, never a new resource owner.
    pub(crate) fn contents_directory(&self) -> io::Result<trust_resource_controller::FileIdentity> {
        time(self.deadline)?;
        self.contents_directory
            .ok_or_else(|| refused("materialization has no actual contents observation"))
    }
}

impl Preparing {
    pub(crate) fn materialization_transfer(
        &mut self,
        request: wire::MaterializationPreparation,
    ) -> io::Result<MaterializationTransfer> {
        request.validate()?;
        if !self.submitted
            || !self.source_submitted
            || self.materialization_submitted
            || self.configuration_budget.remaining_files() != wire::configuration::MAX_FILES
            || self.output_submitted
            || request.key != self.request.key
            || !self.active.load(Ordering::Acquire)
        {
            return Err(refused(
                "materialization preparation is foreign, repeated or out of order",
            ));
        }
        self.materialization_submitted = true;
        let deadline = self
            .configuration_anchor
            .checked_add(Duration::from_nanos(request.input_remaining_ns))
            .ok_or_else(|| refused("materialization input endpoint overflow"))?
            .min(self.deadline);
        time(deadline)?;
        self.deadline = self.deadline.min(deadline);
        Ok(MaterializationTransfer {
            request,
            context: self.context.clone(),
            peer: self.peer,
            active: Arc::clone(&self.active),
            deadline,
            contents_directory: None,
        })
    }
}

impl Owner {
    pub(in crate::actor) fn bootstrap_graph_materialization_begin(
        &mut self,
        mut token: MaterializationTransfer,
    ) -> io::Result<MaterializationTransfer> {
        self.refresh_bootstrap_query(token.peer, &token.request.key)?;
        let context = self.bootstrap_graph_context(token.peer, &token.request.key)?;
        if context != token.context {
            return Err(refused(
                "Bootstrap context changed before materialization preparation",
            ));
        }
        let root = self
            .jobs
            .get_mut(&(
                token.request.key.query.binding.session.clone(),
                token.request.key.query.binding.root_attempt,
            ))
            .and_then(|job| job.bootstrap_graph.as_mut())
            .ok_or_else(|| refused("materialization has no original Root"))?;
        let root_deadline = root.deadline();
        let deadline = token.deadline.min(root_deadline);
        time(deadline)?;
        root.sdk.validate()?;
        root.gnu.check()?;
        root.queries.enrollment_allowed(&token.request.key)?;
        let attempt = root
            .inputs
            .attempt
            .as_mut()
            .ok_or_else(|| refused("materialization has no original enrollment"))?;
        if attempt.request.key != token.request.key
            || !Arc::ptr_eq(&attempt.active, &token.active)
            || !attempt.active.load(Ordering::Acquire)
            || attempt.source.is_none()
            || attempt.materialization_requested.is_some()
            || attempt.workspace.is_some()
            || attempt.configuration_requested.is_some()
            || attempt.output_requested.is_some()
        {
            return Err(refused(
                "materialization is foreign, unprepared or repeated",
            ));
        }
        let expected = attempt
            .configuration_anchor
            .checked_add(Duration::from_nanos(token.request.input_remaining_ns))
            .ok_or_else(|| refused("materialization input endpoint overflow"))?
            .min(root_deadline);
        if expected != token.deadline {
            return Err(refused(
                "materialization endpoint is not its original bound",
            ));
        }
        attempt
            .source
            .as_ref()
            .expect("accepted source")
            .check_current(deadline)?;
        attempt.materialization_requested = Some(token.request.clone());
        let binding = attempt.binding.clone();
        let workspace =
            self.journal
                .materialization_workspace(&binding, token.request.key.graph, deadline)?;
        let attempt = self
            .jobs
            .get_mut(&(
                token.request.key.query.binding.session.clone(),
                token.request.key.query.binding.root_attempt,
            ))
            .and_then(|job| job.bootstrap_graph.as_mut())
            .and_then(|root| root.inputs.attempt.as_mut());
        let Some(attempt) = attempt else {
            std::mem::forget(workspace);
            return Err(refused(
                "fresh workspace lost its original preparation slot",
            ));
        };
        attempt.workspace = Some(workspace);
        attempt
            .workspace
            .as_ref()
            .expect("same actual workspace")
            .check_current()?;
        attempt
            .source
            .as_ref()
            .expect("same actual source")
            .check_current(deadline)?;
        token.contents_directory = Some(observe_contents(
            attempt.workspace.as_ref().expect("same actual workspace"),
            attempt.source.as_ref().expect("same actual source"),
            deadline,
        )?);
        time(deadline)?;
        Ok(token)
    }
}

fn observe_contents(
    workspace: &atpkg::private_input::MaterializationWorkspace,
    source: &OriginalSource,
    deadline: Instant,
) -> io::Result<trust_resource_controller::FileIdentity> {
    let result = (|| {
        time(deadline)?;
        source.check_current(deadline)?;
        workspace.with_contents_directory(|original| {
            let metadata =
                spawnfd::descriptor_metadata(std::os::fd::AsRawFd::as_raw_fd(&original))?;
            let observed = trust_resource_controller::FileIdentity {
                device: metadata.dev(),
                inode: metadata.ino(),
            };
            if observed.inode == 0 || observed == source.request.source_directory {
                return Err(refused("materialization contents aliases original source"));
            }
            time(deadline)?;
            Ok(observed)
        })
    })();
    // Run the original source bookend even if metadata or workspace checks fail.
    let final_check = source.check_current(deadline).and_then(|()| time(deadline));
    if result.is_err() || final_check.is_err() {
        workspace.quarantine();
    }
    let observed = result?;
    final_check?;
    Ok(observed)
}

#[cfg(test)]
#[path = "materialization/tests.rs"]
mod tests;
