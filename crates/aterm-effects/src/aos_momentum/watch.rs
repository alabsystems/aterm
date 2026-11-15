// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The online watcher's judgement of one retired log epoch of the glow
//! ([`crate::aos_effect::Segment`] over [`MomentumEngine`]).

use super::MomentumEngine;

/// One retired log epoch of the glow, with the code that ran it, for [`Segment::judge`].
pub type Segment = crate::aos_effect::Segment<MomentumEngine>;

/// What [`Segment::judge`] found.
pub type SegmentJudgement = crate::aos_effect::SegmentJudgement<MomentumEngine>;
