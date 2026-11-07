// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One logical continuation of an existing member's CPU unit. These values are
//! bookkeeping, never a grant or proof that a compiler has joined its workers.
//! The caller authenticates the original live parent and its actual CPU slot.
use super::*;
use std::io;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Edge {
    pub parent: [u8; 16],
    pub child: [u8; 16],
    pub nonce: [u8; 16],
    pub cpu: u32,
}
impl Edge {
    pub(super) fn of(loan: trust_resource_controller::Loan, child: [u8; 16]) -> Self {
        Self {
            parent: loan.parent_attempt,
            child,
            nonce: loan.nonce,
            cpu: loan.cpu_slot,
        }
    }
    pub(super) fn validate(&self) -> io::Result<()> {
        if self.parent == [0; 16]
            || self.child == [0; 16]
            || self.nonce == [0; 16]
            || self.parent == self.child
        {
            return Err(refused("invalid parent continuation identity"));
        }
        Ok(())
    }
}

fn fields(edge: Edge, state: &str) -> aterm_json::Value {
    aterm_json::json!({"cpu_loan": {
        "state":state,"parent":aterm_codec::hex::encode_lower(&edge.parent),
        "child":aterm_codec::hex::encode_lower(&edge.child),
        "nonce":aterm_codec::hex::encode_lower(&edge.nonce),"cpu":edge.cpu
    }})
}
fn parent(job: &Job, edge: Edge) -> Binding {
    Binding {
        session: job.binding.session.clone(),
        attempt: edge.parent,
    }
}
pub(super) fn outgoing(job: &Job, member: [u8; 16]) -> Option<Edge> {
    job.loans.get(&member).and_then(RunState::edge)
}
pub(super) fn matches_parent(job: &mut Job, peer: Peer, edge: Edge) -> bool {
    let paired = job.compiler.as_ref().is_some_and(|compiler| {
        i32::try_from(peer.pid)
            .ok()
            .and_then(aterm_uds::process::LoadedImage::read)
            .is_some_and(|image| {
                image.matches_file(&compiler.execution().join("bin/trustc")) == Some(true)
            })
    });
    edge.validate().is_ok()
        && edge.parent != job.binding.attempt
        && paired
        && outgoing(job, edge.parent).is_none_or(|existing| existing == edge)
        && members::trusted_caller(job, peer)
        && job
            .members
            .get_mut(&edge.parent)
            .is_some_and(|slot| slot.loan_parent(peer, edge))
}
/// This is reached only at the lexical before-ARM boundary. Once the journal
/// was attempted, only this actual attempt's NoLaunch/closure may return it.
pub(super) fn intent(
    job: &mut Job,
    peer: Peer,
    edge: Edge,
    journal: &journal::Journal,
) -> io::Result<()> {
    if !matches_parent(job, peer, edge) {
        return Err(refused(
            "loan does not match the live original compiler member",
        ));
    }
    let binding = parent(job, edge);
    let state = job.loans.entry(edge.parent).or_default();
    let persist = || journal.patch(&binding, fields(edge, "pending"));
    if state.runnable() {
        state.intent(edge, persist)
    } else {
        state.confirm_intent(edge, persist)
    }
}
pub(super) fn lend(job: &mut Job, edge: Edge, journal: &journal::Journal) -> io::Result<()> {
    let binding = parent(job, edge);
    job.loans
        .get_mut(&edge.parent)
        .ok_or_else(|| refused("missing original loan intent"))?
        .lend(edge, || journal.patch(&binding, fields(edge, "loaned")))
}
pub(super) fn returned(
    job: &mut Job,
    edge: Edge,
    receipt: member::ReturnReceipt,
    journal: &journal::Journal,
) -> io::Result<()> {
    let binding = parent(job, edge);
    match job.loans.get_mut(&edge.parent) {
        // Configuration/authorization may have failed before intent was tried.
        None | Some(RunState::Runnable) if matches!(receipt, member::ReturnReceipt::NoArm) => {
            Ok(())
        }
        None | Some(RunState::Runnable) => {
            Err(refused("closed child lost its original loan intent"))
        }
        Some(state) => state.returned(edge, || journal.patch(&binding, fields(edge, "returned"))),
    }
}

#[derive(Default)]
pub(super) enum RunState {
    #[default]
    Runnable,
    LoanPending(Edge),
    Loaned(Edge),
}
impl RunState {
    pub(super) fn runnable(&self) -> bool {
        matches!(self, Self::Runnable)
    }
    pub(super) fn edge(&self) -> Option<Edge> {
        match self {
            Self::Runnable => None,
            Self::LoanPending(edge) | Self::Loaned(edge) => Some(*edge),
        }
    }
    /// Publish before any child ARM. Once persistence has been attempted its
    /// failure is ambiguous, so the parent remains parked even on Err.
    pub(super) fn intent(
        &mut self,
        edge: Edge,
        persist: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        edge.validate()?;
        if !self.runnable() {
            return Err(refused("parent already has an outstanding continuation"));
        }
        *self = Self::LoanPending(edge);
        persist()
    }
    /// Repeat only the SAME intent, preserving a failed write's exact identity.
    /// This success does not itself authorize ARM, BIND or GO.
    pub(super) fn confirm_intent(
        &self,
        edge: Edge,
        persist: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        if !matches!(self, Self::LoanPending(current) if *current == edge) {
            return Err(refused("parent continuation intent changed"));
        }
        persist()
    }
    /// After the actual child is bound, record the edge before its GO. An
    /// uncertain journal acknowledgment cannot make the parent runnable.
    pub(super) fn lend(
        &mut self,
        edge: Edge,
        persist: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        if !matches!(self, Self::LoanPending(current) | Self::Loaned(current) if *current == edge) {
            return Err(refused("parent continuation does not own this child"));
        }
        *self = Self::Loaned(edge);
        persist()
    }
    /// The caller must already hold positive NoChild or actual child-scope
    /// retirement. The durable return precedes releasing Finished to the caller.
    /// Observed process exit, a nonce, and transport EOF are never that proof.
    pub(super) fn returned(
        &mut self,
        edge: Edge,
        persist: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        if self.edge() != Some(edge) {
            return Err(refused("return differs from the original continuation"));
        }
        persist()?;
        *self = Self::Runnable;
        Ok(())
    }
}
fn refused(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

#[cfg(test)]
#[path = "resource_admission_parent_loan_tests.rs"]
mod tests;
