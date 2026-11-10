// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Private frontdoors recover actual existing destination roles. Correlation
//! replay precedes work admission; metadata alone cannot mint either role.
use super::*;
fn copy_report<'a>(report: &ResourceReport<'a>) -> ResourceReport<'a> {
    ResourceReport {
        operation: report.operation,
        member: report.member,
        registration: report.registration,
        boundary: report.boundary,
        assignment_generation: report.assignment_generation,
        credential_generation: report.credential_generation,
        digest: report.digest,
        account: report.account,
        user: report.user,
        auth: report.auth,
    }
}
impl Registry {
    pub fn prepare_registered_target_resources(
        &self,
        caller: &slot::Caller,
        connection: &Connection,
        assignment: &str,
        context: &str,
        report: ResourceReport<'_>,
    ) -> Result<ResourcePreparation, Error> {
        if let Some(receipt) =
            self.replay_target_resources(caller, assignment, context, copy_report(&report))?
        {
            return Ok(ResourcePreparation::Ready(receipt));
        }
        let peer = self.registered_work_peer(caller)?;
        let role = self.installed_target(&peer, assignment, Some(context))?;
        self.prepare_target_resources(&peer, connection, &role, report)
    }
    pub fn prepare_registered_target_ack(
        &self,
        caller: &slot::Caller,
        assignment: &str,
        context: &str,
        receipt: &str,
    ) -> Result<AckPreparation, Error> {
        if let Some(applied) = self.replay_target_ack(caller, assignment, context, receipt)? {
            return Ok(AckPreparation::Applied(applied));
        }
        let peer = self.registered_work_peer(caller)?;
        self.prepare_target_ack(&peer, assignment, context, receipt)
    }
}
