// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Actual supplementary bytes, with a ledger independent of fungible FIFO data.

use super::{Epoch, refused};
use crate::cohort::Cohort;
use aterm_uds::spawnfd::BirthKey;
use std::{
    fmt,
    io::{self, Read as _, Write as _},
    sync::Arc,
};

/// One actual byte removed from the original paid GNU epoch. This value cannot
/// be cloned or constructed from data. Drop never writes a byte or clears its
/// original epoch's independent unresolved-borrower entry.
#[derive(Debug)]
pub struct Loan {
    origin: Arc<()>,
    id: u64,
}

#[derive(Debug)]
pub(crate) enum State {
    Unused,
    Launching,
    NoChild(BirthKey),
    Unbound(BirthKey),
    Bound { birth: BirthKey, cohort: Cohort },
    Closed,
    Returning,
    Unknown,
}

#[derive(Debug)]
struct Receipt {
    origin: Arc<()>,
    id: u64,
}

/// Actual lexical non-use, or persisted cancellation of this exact native
/// attempt. There is no constructor from a failed status, key or observation.
#[derive(Debug)]
pub struct NoLaunch(Receipt);

/// The same original child was actually reaped and its registered subgroup
/// durably retired. This excludes escaped descendants. The Host must retain an
/// actual matching native no-fork/no-escape target witness before that child GO;
/// this receipt alone does not establish whole-descendant or root closure.
#[derive(Debug)]
pub struct RetiredSubgroup(Receipt);

/// A one-use original native bookend for returning one specific loan.
#[derive(Debug)]
pub enum Closure {
    /// The exact loan was unused or its actual no-child attempt was cancelled.
    NoLaunch(NoLaunch),
    /// Its exact owning child and durable registered subgroup have retired.
    Retired(RetiredSubgroup),
}

impl Closure {
    fn receipt(&self) -> &Receipt {
        match self {
            Self::NoLaunch(proof) => &proof.0,
            Self::Retired(proof) => &proof.0,
        }
    }
}

/// A refused return retains its original receipt until a native write is
/// attempted. An attempted or ambiguous write never makes a retryable receipt.
#[derive(Debug)]
pub struct ReturnError {
    error: io::Error,
    proof: Option<Box<Closure>>,
}

impl ReturnError {
    pub(crate) fn refused(error: io::Error, proof: Closure) -> Self {
        Self {
            error,
            proof: Some(Box::new(proof)),
        }
    }
    /// Inspect the refusal without inferring whether a byte was returned.
    #[must_use]
    pub const fn error(&self) -> &io::Error {
        &self.error
    }

    /// Recover the original receipt only when no native return was attempted.
    #[must_use]
    pub fn into_parts(self) -> (io::Error, Option<Closure>) {
        (self.error, self.proof.map(|proof| *proof))
    }
}

impl fmt::Display for ReturnError {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(out, "original GNU loan return refused: {}", self.error)
    }
}
impl std::error::Error for ReturnError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

impl Epoch {
    pub(crate) fn try_loan(&mut self) -> io::Result<Option<Loan>> {
        self.check()?;
        let transport = self.transport.as_mut().expect("checked original transport");
        if transport.poisoned || transport.burned != 0 || transport.retired {
            return Err(refused(
                "original GNU transport cannot lend after retirement or poison",
            ));
        }
        if transport.supplementary == 0 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "one CPU has no supplementary GNU unit",
            ));
        }
        // Even forged replacement bytes cannot fund more native borrowers than
        // the original paid supplementary population. Ordinary cooperative GNU
        // users remain subject to the existing fungible-byte transport contract.
        if self.loans.len() >= transport.supplementary {
            return Ok(None);
        }
        let id = self
            .next_loan
            .checked_add(1)
            .ok_or_else(|| refused("original GNU loan identity exhausted"))?;
        self.next_loan = id;
        // Insert the restrictive entry before removing an actual byte. A native
        // failure or unwind can only retain capacity, never silently refund it.
        self.loans.insert(id, State::Unknown);
        let mut byte = [0];
        match transport.control.read(&mut byte) {
            Ok(1) if byte[0] == b'+' => {
                *self.loans.get_mut(&id).expect("reserved original loan") = State::Unused;
                Ok(Some(Loan {
                    origin: Arc::clone(&self.loan_origin),
                    id,
                }))
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                self.loans.remove(&id);
                Ok(None)
            }
            Err(error) => Err(error),
            Ok(_) => {
                transport.poisoned = true;
                Err(refused(
                    "original GNU loan encountered an alien byte or closed FIFO",
                ))
            }
        }
    }

    pub(crate) fn loan_state(&self, loan: &Loan) -> io::Result<&State> {
        self.check()?;
        if !Arc::ptr_eq(&self.loan_origin, &loan.origin) {
            return Err(refused("GNU loan belongs to a different original epoch"));
        }
        self.loans
            .get(&loan.id)
            .ok_or_else(|| refused("GNU loan no longer has unresolved custody"))
    }

    pub(crate) fn set_loan_state(&mut self, loan: &Loan, state: State) -> io::Result<()> {
        self.loan_state(loan)?;
        *self.loans.get_mut(&loan.id).expect("checked original loan") = state;
        Ok(())
    }

    pub(crate) fn close_unused(&mut self, loan: &Loan) -> io::Result<Closure> {
        if !matches!(self.loan_state(loan)?, State::Unused) {
            return Err(refused("GNU loan has already attempted a native launch"));
        }
        self.close_no_launch(loan)
    }

    pub(crate) fn close_no_launch(&mut self, loan: &Loan) -> io::Result<Closure> {
        self.set_loan_state(loan, State::Closed)?;
        Ok(Closure::NoLaunch(NoLaunch(self.loan_receipt(loan))))
    }

    pub(crate) fn close_retired(&mut self, loan: &Loan) -> io::Result<RetiredSubgroup> {
        self.set_loan_state(loan, State::Closed)?;
        Ok(RetiredSubgroup(self.loan_receipt(loan)))
    }

    fn loan_receipt(&self, loan: &Loan) -> Receipt {
        Receipt {
            origin: Arc::clone(&self.loan_origin),
            id: loan.id,
        }
    }

    pub(crate) fn return_loan(
        &mut self,
        loan: &mut Loan,
        proof: Closure,
    ) -> Result<(), ReturnError> {
        let checked = (|| {
            if !matches!(self.loan_state(loan)?, State::Closed)
                || !Arc::ptr_eq(&self.loan_origin, &proof.receipt().origin)
                || loan.id != proof.receipt().id
            {
                return Err(refused(
                    "GNU return needs the same original loan's native closure",
                ));
            }
            let transport = self.transport.as_ref().expect("checked original transport");
            if transport.poisoned || transport.burned != 0 || transport.retired {
                return Err(refused("original GNU transport cannot accept a return"));
            }
            Ok(())
        })();
        if let Err(error) = checked {
            return Err(ReturnError {
                error,
                proof: Some(Box::new(proof)),
            });
        }
        // No retry after reaching this state, including EINTR or an ambiguous
        // native write. Keep the independent entry even if a byte appeared.
        *self.loans.get_mut(&loan.id).expect("checked original loan") = State::Returning;
        let transport = self.transport.as_mut().expect("checked original transport");
        match transport.control.write(b"+") {
            Ok(1) => {
                if let Err(error) = transport.check() {
                    transport.poisoned = true;
                    return Err(ReturnError { error, proof: None });
                }
                self.loans.remove(&loan.id);
                Ok(())
            }
            result => {
                transport.poisoned = true;
                let error = result
                    .err()
                    .unwrap_or_else(|| refused("original GNU byte return was short"));
                Err(ReturnError { error, proof: None })
            }
        }
    }
}

#[cfg(test)]
#[path = "retained_gnu_loan_tests.rs"]
mod tests;
