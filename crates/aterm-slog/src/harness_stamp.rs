// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE STAMP EVERY ROW OF A HARNESS'S RECORD STREAM CARRIES (D30): `holder=<the claim's
//! holder>`, `src=harness`, `conf=claim`, `by=<the tab's own session>`, `hseq=<the record's
//! seq>` where the row stands for one record, and `fp=<its digest>` where the record's body went
//! to the tab's Out ring (HX-15h). One type builds every kind of row (a mark, a
//! node's open, close and amend) so no writer of a stream row can leave one of the four out:
//! aterm-gui's stream arm and the turn tracker's stream source ([`crate::TurnTracker::stream_turn`])
//! both write through it.

use crate::field::{FieldError, Fields, Key};
use crate::log::{Amend, Close, Mark, Open};
use crate::vocab::{Actor, Conf, Kind, ShortId, Src};
use crate::words::Out;

/// Who wrote a stream row, and which record it stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HarnessStamp {
    /// The claim's holder (`holder=`).
    pub holder: ShortId,
    /// The tab's own session (`by=`).
    pub by: Actor,
    /// The record's `seq` (`hseq=`), where the row stands for one record.
    pub hseq: Option<u64>,
    /// The record's digest (`fp=`, 40 hex characters), where its body went to the Out ring.
    pub fp: Option<ShortId>,
}

impl HarnessStamp {
    /// The stamp of `holder` (none for a holder that is no log ID) and `by`, for record `hseq`.
    #[must_use]
    pub fn new(holder: &str, by: Actor, hseq: Option<u64>) -> Option<HarnessStamp> {
        Some(HarnessStamp {
            holder: ShortId::new(holder)?,
            by,
            hseq,
            fp: None,
        })
    }

    /// The stamp of a record whose body went to the Out ring under the digest `fp` (none for a
    /// digest that is no log ID).
    #[must_use]
    pub fn with_fp(self, fp: &str) -> Option<HarnessStamp> {
        Some(HarnessStamp {
            fp: Some(ShortId::new(fp)?),
            ..self
        })
    }

    /// `f` with `holder=` and, for a record, `hseq=` (and `fp=` for one in the Out ring) added.
    ///
    /// # Errors
    ///
    /// [`FieldError`] when `f` has no room for them.
    pub fn fields(&self, f: Fields) -> Result<Fields, FieldError> {
        let mut f = f.id(Key::Holder, self.holder.as_str())?;
        if let Some(seq) = self.hseq {
            f = f.u(Key::Hseq, seq)?;
        }
        match self.fp {
            Some(fp) => f.id(Key::Fp, fp.as_str()),
            None => Ok(f),
        }
    }

    /// A mark of `kind` with `f`, stamped.
    ///
    /// # Errors
    ///
    /// [`FieldError`] when `f` has no room for the stamp's fields.
    pub fn mark(&self, kind: Kind, f: Fields) -> Result<Mark, FieldError> {
        Ok(Mark::new(kind, Src::Harness, Conf::Claim)
            .by(self.by)
            .fields(self.fields(f)?))
    }

    /// An open of `kind` with `f`, stamped (its parent is the caller's).
    ///
    /// # Errors
    ///
    /// [`FieldError`] when `f` has no room for the stamp's fields.
    pub fn open(&self, kind: Kind, f: Fields) -> Result<Open, FieldError> {
        Ok(Open::new(kind, Src::Harness, Conf::Claim)
            .by(self.by)
            .fields(self.fields(f)?))
    }

    /// A close `out` with `f`, stamped.
    ///
    /// # Errors
    ///
    /// [`FieldError`] when `f` has no room for the stamp's fields.
    pub fn close(&self, out: Out, f: Fields) -> Result<Close, FieldError> {
        Ok(Close::new(out, Src::Harness, Conf::Claim)
            .by(self.by)
            .fields(self.fields(f)?))
    }

    /// An amend of a node of `kind` with `f`, stamped.
    ///
    /// # Errors
    ///
    /// [`FieldError`] when `f` has no room for the stamp's fields.
    pub fn amend(&self, kind: Kind, f: Fields) -> Result<Amend, FieldError> {
        Ok(Amend::new(kind, Src::Harness, Conf::Claim, self.fields(f)?).by(self.by))
    }
}
