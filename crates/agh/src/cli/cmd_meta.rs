// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! agh's own verbs (design §15): `help`, `schema` and `version`.

use super::args::Invocation;
use super::verbs::Noun;
use super::{Answer, Ctx, Outcome, Receipt, help, schema as schema_doc};
use crate::error::Result;

/// The outcome of a local answer.
fn local(inv: &Invocation, answer: Answer) -> Outcome {
    Outcome {
        answer,
        receipt: Receipt::new(&inv.verb_name()),
    }
}

/// `agh help [WORD…]`: the manual, a page, or the verbs under a word.
///
/// # Errors
///
/// `help::render`'s usage error.
pub(crate) fn help(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let _ = ctx;
    let page = help::render(&inv.positionals)?;
    Ok(local(inv, Answer::Bytes(page.into_bytes())))
}

/// `agh schema [WHAT]`.
///
/// # Errors
///
/// `schema::schema`'s usage error.
pub(crate) fn schema(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let _ = ctx;
    let value = schema_doc::schema(inv.positional(0))?;
    Ok(local(
        inv,
        Answer::Object {
            noun: Noun::Meta,
            value,
        },
    ))
}

/// `agh version`: `agh {VERSION} (api {API_VERSION})`.
///
/// # Errors
///
/// None: it answers from this binary.
pub(crate) fn version(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let _ = ctx;
    let line = format!("agh {} (api {})\n", crate::VERSION, crate::API_VERSION);
    Ok(local(inv, Answer::Bytes(line.into_bytes())))
}
