// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The registry rows this build compiles in (docs/DESIGN-alab-lab-model-2026-11-01.md
//! §4.2–§4.4): the six providers by the provider words the importer shares
//! (`typed::keys::PROVIDERS`, `import.rs` `PROVIDERS`), the AI CLIs by their connector id words
//! (`aterm-agent` `ai/connector.rs`, plus `codex-alab`, which shares Codex's login), and each
//! provider's plans: a free row, and one row per subscription type the lab buys under a name
//! this build can state. Another build may compile fewer rows or more: a row reaches a registry
//! when a build that holds it runs the seed pass there.
//!
//! A seed writes only the columns the schema gives to a seed (`Writer::Seed`). A plan's `tier`,
//! `price_cents` and `currency` are the owner's columns, so no seed writes them: a plan's tier is
//! in its key and its name (`anthropic-max-20x`, "Claude Max 20x"), and its tier and price cells
//! are the owner's to write, which land in empty cells. Nominal values only: the reading the
//! provider shows is authoritative.
//!
//! A seed is an automatic write (`"a": true`, counted against the writer's quota) and goes only
//! into a cell a **complete** scan of the registry found absent: it never overwrites the owner,
//! an import or another build's seed, so a re-run, or a second Mac's run after repair, writes
//! nothing. Each row's cells go in one record, and each record carries a content-derived
//! request id (`typed::request_id`), so a retry returns its first receipt.

use super::registry::{CLI, PLAN, PROVIDER};
use crate::{
    Result,
    database::Change,
    typed::{
        Collection,
        cell::{self, CellKey, CellValue, Cells, Field, Projection},
        keys,
    },
};
use aterm_json::{Value, json};

/// One compiled provider row.
pub struct ProviderSeed {
    pub word: &'static str,
    pub name: &'static str,
    /// The canonical claims realms (an unpooled provider's realm is its word).
    pub realms: &'static [&'static str],
}

/// The six providers, one row per provider word.
pub const PROVIDERS: [ProviderSeed; 6] = [
    ProviderSeed {
        word: keys::ANTHROPIC,
        name: "Anthropic",
        realms: &["claude"],
    },
    ProviderSeed {
        word: keys::OPENAI,
        name: "OpenAI",
        realms: &["codex"],
    },
    ProviderSeed {
        word: keys::GOOGLE,
        name: "Google",
        realms: &["google"],
    },
    ProviderSeed {
        word: keys::META,
        name: "Meta",
        realms: &["meta"],
    },
    ProviderSeed {
        word: keys::KIMI,
        name: "Moonshot AI (Kimi)",
        realms: &["kimi:global", "kimi:mainland-cn"],
    },
    ProviderSeed {
        word: keys::XAI,
        name: "xAI",
        realms: &["grok"],
    },
];

/// One compiled CLI row.
pub struct CliSeed {
    /// The connector id word.
    pub id: &'static str,
    pub provider: &'static str,
    pub pool_realm: &'static str,
    pub support: &'static str,
    pub readings: &'static [&'static str],
    pub roster: Option<&'static str>,
}

/// The AI CLIs: the connector table's rows (`claude`, `codex`, `agy`, `muse`, `grok`, `kimi`)
/// and `codex-alab`, which runs on Codex's pool. Where a pooled CLI's figures come from is what
/// the harness reads today: Claude Code's status line and `/usage`, Codex's rollouts, Kimi's and
/// agy's `/usage` output.
pub const CLIS: [CliSeed; 7] = [
    CliSeed {
        id: "claude",
        provider: keys::ANTHROPIC,
        pool_realm: "claude",
        support: "pooled",
        readings: &["statusline", "cli-output"],
        roster: Some("accounts.toml"),
    },
    CliSeed {
        id: "codex",
        provider: keys::OPENAI,
        pool_realm: "codex",
        support: "pooled",
        readings: &["rollout"],
        roster: Some("accounts-codex.toml"),
    },
    CliSeed {
        id: "codex-alab",
        provider: keys::OPENAI,
        pool_realm: "codex",
        support: "pooled",
        readings: &["rollout"],
        roster: Some("accounts-codex.toml"),
    },
    CliSeed {
        id: "agy",
        provider: keys::GOOGLE,
        pool_realm: "none",
        support: "quota-only",
        readings: &["cli-output"],
        roster: None,
    },
    CliSeed {
        id: "muse",
        provider: keys::META,
        pool_realm: "none",
        support: "launch-only",
        readings: &[],
        roster: None,
    },
    CliSeed {
        id: "grok",
        provider: keys::XAI,
        pool_realm: "grok",
        support: "pooled",
        readings: &[],
        roster: Some("accounts-grok.toml"),
    },
    CliSeed {
        id: "kimi",
        provider: keys::KIMI,
        pool_realm: "kimi:global",
        support: "pooled",
        readings: &["cli-output"],
        roster: Some("accounts-kimi.toml"),
    },
];

/// One compiled plan row.
pub struct PlanSeed {
    /// `<provider>-<slug>`: the slug names the tier, and never changes once a build shipped it.
    pub pk: &'static str,
    pub provider: &'static str,
    /// The plan's name with its tier, as the provider sells it.
    pub name: &'static str,
    /// How it is billed: `free` for a row with no subscription, none where this build does not
    /// know.
    pub period: Option<&'static str>,
    /// The windows its limits are read in, each in percent; none where the plan's are unknown.
    pub windows: &'static [&'static str],
    /// `current`, or `legacy` for a key kept only so that what names it still reads.
    pub status: &'static str,
}

impl PlanSeed {
    /// A current plan whose billing period and limit windows this build does not know.
    const fn new(pk: &'static str, provider: &'static str, name: &'static str) -> Self {
        Self {
            pk,
            provider,
            name,
            period: None,
            windows: &[],
            status: "current",
        }
    }
    /// Billed by the month.
    const fn monthly(mut self) -> Self {
        self.period = Some("month");
        self
    }
    /// The provider's free use: an account with no subscription.
    const fn free(mut self) -> Self {
        self.period = Some("free");
        self
    }
    /// Its limits are read as a five-hour and a seven-day window.
    const fn session_and_week(mut self) -> Self {
        self.windows = &["5h", "7d"];
        self
    }
    /// Kept for what names it; an account's plan is one of its provider's other rows.
    const fn legacy(mut self) -> Self {
        self.status = "legacy";
        self
    }
}

/// Each provider's plans: a free row, and one row per tier the lab buys, the tier in the key and
/// the name.
///
/// - `anthropic-max` is the word Claude Code reports for either Max tier (`max` names no tier),
///   and the only Max row earlier builds seeded. It stays, as `legacy`, so an account or an
///   observation that names it still reads; `anthropic-max-5x` and `anthropic-max-20x` are the
///   plans.
/// - `openai-prolite` is Codex's `plan_type` word, seeded as it was. The other ChatGPT rows hold
///   every `plan_type` a Codex login's claims name and the harness keeps (`aterm-agent`
///   `harness::journal::PLANS`), so a plan a Mac observed names a row (`fleet::fold`,
///   `<provider>-<word>`): Go is billed by the month; Team, Business (OpenAI's later name for
///   Team, which a login's claims may carry either way), Enterprise and Edu have no period or
///   window this build knows.
/// - Moonshot AI and Meta have only their free row: no code here names a paid plan either
///   sells (the harness reads Muse Code's `tierName` as the provider sends it and holds no name
///   of its own), and a seed states no name this build cannot confirm. A paid row of theirs is
///   the owner's to write.
/// - A free row's name is the provider's product and `Free`: the registry's name for an account
///   with no subscription, whatever the provider calls it.
pub const PLANS: [PlanSeed; 24] = [
    PlanSeed::new("anthropic-free", keys::ANTHROPIC, "Claude Free").free(),
    PlanSeed::new("anthropic-pro", keys::ANTHROPIC, "Claude Pro")
        .monthly()
        .session_and_week(),
    PlanSeed::new("anthropic-max-5x", keys::ANTHROPIC, "Claude Max 5x")
        .monthly()
        .session_and_week(),
    PlanSeed::new("anthropic-max-20x", keys::ANTHROPIC, "Claude Max 20x")
        .monthly()
        .session_and_week(),
    PlanSeed::new("anthropic-max", keys::ANTHROPIC, "Claude Max")
        .monthly()
        .session_and_week()
        .legacy(),
    PlanSeed::new("anthropic-team", keys::ANTHROPIC, "Claude Team"),
    PlanSeed::new("anthropic-enterprise", keys::ANTHROPIC, "Claude Enterprise"),
    PlanSeed::new("openai-free", keys::OPENAI, "ChatGPT Free").free(),
    PlanSeed::new("openai-plus", keys::OPENAI, "ChatGPT Plus")
        .monthly()
        .session_and_week(),
    PlanSeed::new("openai-pro", keys::OPENAI, "ChatGPT Pro")
        .monthly()
        .session_and_week(),
    PlanSeed::new("openai-prolite", keys::OPENAI, "ChatGPT Pro Lite")
        .monthly()
        .session_and_week(),
    PlanSeed::new("openai-go", keys::OPENAI, "ChatGPT Go").monthly(),
    PlanSeed::new("openai-team", keys::OPENAI, "ChatGPT Team"),
    PlanSeed::new("openai-business", keys::OPENAI, "ChatGPT Business"),
    PlanSeed::new("openai-enterprise", keys::OPENAI, "ChatGPT Enterprise"),
    PlanSeed::new("openai-edu", keys::OPENAI, "ChatGPT Edu"),
    PlanSeed::new("google-free", keys::GOOGLE, "Google AI Free").free(),
    PlanSeed::new("google-ai-pro", keys::GOOGLE, "Google AI Pro").monthly(),
    PlanSeed::new("google-ai-ultra", keys::GOOGLE, "Google AI Ultra").monthly(),
    PlanSeed::new("meta-free", keys::META, "Muse Code Free").free(),
    PlanSeed::new("kimi-free", keys::KIMI, "Kimi Free").free(),
    PlanSeed::new("xai-free", keys::XAI, "Grok Free").free(),
    PlanSeed::new("xai-supergrok", keys::XAI, "SuperGrok").monthly(),
    PlanSeed::new("xai-supergrok-heavy", keys::XAI, "SuperGrok Heavy").monthly(),
];

/// One seeded cell: its key parts and value.
struct Seed {
    pk: &'static str,
    col: &'static str,
    elem: Option<&'static str>,
    x: Value,
}

fn scalar(pk: &'static str, col: &'static str, x: Value) -> Seed {
    Seed {
        pk,
        col,
        elem: None,
        x,
    }
}

/// Every seeded row, as (collection, its cells), in a fixed order.
fn rows() -> Vec<(&'static Collection, Vec<Seed>)> {
    let mut rows = Vec::new();
    for p in &PROVIDERS {
        rows.push((
            &PROVIDER,
            vec![
                scalar(p.word, "name", json!(p.name)),
                scalar(p.word, "realms", json!(p.realms)),
                scalar(p.word, "status", json!("active")),
                scalar(p.word, "row", json!("live")),
            ],
        ));
    }
    for c in &CLIS {
        let mut cells = vec![
            scalar(c.id, "provider", json!(c.provider)),
            scalar(c.id, "pool_realm", json!(c.pool_realm)),
            scalar(c.id, "support", json!(c.support)),
        ];
        if let Some(roster) = c.roster {
            cells.push(scalar(c.id, "roster", json!(roster)));
        }
        for reading in c.readings {
            cells.push(Seed {
                pk: c.id,
                col: "readings",
                elem: Some(reading),
                x: Value::Bool(true),
            });
        }
        cells.push(scalar(c.id, "row", json!("live")));
        rows.push((&CLI, cells));
    }
    for p in &PLANS {
        let limits: Vec<Value> = p
            .windows
            .iter()
            .map(|w| json!({"w": w, "unit": "pct"}))
            .collect();
        let mut cells = vec![
            scalar(p.pk, "provider", json!(p.provider)),
            scalar(p.pk, "name", json!(p.name)),
            scalar(p.pk, "limits", Value::Array(limits)),
            scalar(p.pk, "status", json!(p.status)),
            scalar(p.pk, "row", json!("live")),
        ];
        if let Some(period) = p.period {
            cells.push(scalar(p.pk, "period", json!(period)));
        }
        rows.push((&PLAN, cells));
    }
    rows
}

/// Whether this build compiles a seed row keyed `pk` in `collection` (a provider, CLI or plan
/// row): a row this build's seed pass writes wherever it is absent, so an owner write made on
/// this build may name it before any pass has run here ([`crate::att::Compiled::seeded`]). An
/// earlier build may compile no such row.
pub fn compiled(collection: &str, pk: &str) -> bool {
    rows()
        .iter()
        .any(|(c, seeds)| c.name == collection && seeds.iter().any(|seed| seed.pk == pk))
}

/// Whether a projected cell already holds something: a value, a conflict, or for a set element
/// any candidate at all. A seed never writes over it.
fn held(projection: &Projection, seed: &Seed) -> bool {
    let Some(field) = projection
        .rows
        .get(seed.pk)
        .and_then(|row| row.fields.get(seed.col))
    else {
        return false;
    };
    match (seed.elem, field) {
        (Some(elem), Field::Set(elements)) => elements.contains_key(elem),
        (Some(_), _) => true,
        (None, _) => true,
    }
}

/// Whether `seed`, a cell of `collection`, names a provider row `providers` holds that is not
/// live: the owner retired it, and a seed of a live CLI or plan naming it would be refused
/// (`typed::check::names_not_live`), and with it every later pass that plans it again.
fn names_ended_provider(collection: &Collection, seed: &Seed, providers: &Projection) -> bool {
    collection.column(seed.col).is_some_and(
        |column| matches!(column.ty, crate::typed::Ty::Ref(target) if target == PROVIDER.name),
    ) && seed
        .x
        .as_str()
        .and_then(|word| providers.rows.get(word))
        .is_some_and(|row| !row.live())
}

/// The seed rows a complete scan of `cells` (the registry's projected cells, past-quota
/// records left out) calls for: every seeded cell absent there, written as automatic, one entry
/// per row so a row's cells go in one record. Empty when every seed is held, so a re-run writes
/// nothing. A CLI's or plan's `provider` that names a provider the owner retired is left out
/// ([`names_ended_provider`]); a later pass fills it once the provider is live again.
///
/// # Errors
/// A compiled seed breaks its column's type (a bug the tests catch).
pub fn rows_for(cells: &Cells) -> Result<Vec<Vec<Change>>> {
    let providers = cell::project(&PROVIDER, cells);
    let clis = cell::project(&CLI, cells);
    let plans = cell::project(&PLAN, cells);
    let mut out = Vec::new();
    for (collection, seeds) in rows() {
        let projection = match collection.name {
            "provider" => &providers,
            "cli" => &clis,
            _ => &plans,
        };
        let mut row = Vec::new();
        for seed in seeds.into_iter().filter(|seed| {
            !held(projection, seed) && !names_ended_provider(collection, seed, &providers)
        }) {
            let key = CellKey {
                pk: seed.pk,
                col: seed.col,
                elem: seed.elem,
            };
            row.push(cell::change(
                collection,
                key,
                &CellValue::automatic(collection.version, seed.x),
            )?);
        }
        if !row.is_empty() {
            out.push(row);
        }
    }
    Ok(out)
}

#[cfg(all(test, unix))]
#[path = "seeds_tests.rs"]
mod tests;
