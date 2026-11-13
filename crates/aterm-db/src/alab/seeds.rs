// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The registry rows every build compiles in (docs/DESIGN-alab-lab-model-2026-11-01.md
//! §4.2–§4.4): the six providers by the provider words the importer shares
//! (`typed::keys::PROVIDERS`, `import.rs` `PROVIDERS`), the AI CLIs by their connector id words
//! (`aterm-agent` `ai/connector.rs`, plus `codex-alab`, which shares Codex's login), and the
//! plan words the CLIs themselves report (Claude Code's organization types, Codex's
//! `plan_type`). Nominal values only, never a price: the owner annotates, and the reading the
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
    /// `<provider>-<slug>`.
    pub pk: &'static str,
    pub provider: &'static str,
    pub name: &'static str,
    pub period: Option<&'static str>,
    /// The windows its limits are read in, each in percent; none where the plan's are unknown.
    pub windows: &'static [&'static str],
}

/// The plan words the CLIs report: Claude Code's organization types and Codex's `plan_type`.
pub const PLANS: [PlanSeed; 7] = [
    PlanSeed {
        pk: "anthropic-max",
        provider: keys::ANTHROPIC,
        name: "Claude Max",
        period: Some("month"),
        windows: &["5h", "7d"],
    },
    PlanSeed {
        pk: "anthropic-pro",
        provider: keys::ANTHROPIC,
        name: "Claude Pro",
        period: Some("month"),
        windows: &["5h", "7d"],
    },
    PlanSeed {
        pk: "anthropic-team",
        provider: keys::ANTHROPIC,
        name: "Claude Team",
        period: None,
        windows: &[],
    },
    PlanSeed {
        pk: "anthropic-enterprise",
        provider: keys::ANTHROPIC,
        name: "Claude Enterprise",
        period: None,
        windows: &[],
    },
    PlanSeed {
        pk: "openai-plus",
        provider: keys::OPENAI,
        name: "ChatGPT Plus",
        period: Some("month"),
        windows: &["5h", "7d"],
    },
    PlanSeed {
        pk: "openai-pro",
        provider: keys::OPENAI,
        name: "ChatGPT Pro",
        period: Some("month"),
        windows: &["5h", "7d"],
    },
    PlanSeed {
        pk: "openai-prolite",
        provider: keys::OPENAI,
        name: "ChatGPT Pro Lite",
        period: Some("month"),
        windows: &["5h", "7d"],
    },
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
            scalar(p.pk, "status", json!("current")),
            scalar(p.pk, "row", json!("live")),
        ];
        if let Some(period) = p.period {
            cells.push(scalar(p.pk, "period", json!(period)));
        }
        rows.push((&PLAN, cells));
    }
    rows
}

/// Whether every build compiles a seed row keyed `pk` in `collection` (a provider, CLI or plan
/// row): a row the registry holds by construction, which a seed pass writes wherever it is
/// absent, so an owner write may name it before any pass has run here
/// ([`crate::att::Compiled::seeded`]).
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

/// The seed rows a complete scan of `cells` (the registry's projected cells, past-quota
/// records left out) calls for: every seeded cell absent there, written as automatic, one entry
/// per row so a row's cells go in one record. Empty when every seed is held, so a re-run writes
/// nothing.
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
        for seed in seeds.into_iter().filter(|seed| !held(projection, seed)) {
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
