// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Which machine row a source means. A source's machine matches a row by its node (the roster's
//! `node` principal, a replica's `fleet_node` transport id), then host, then label or alias, the
//! order `register::pick` adopts a Mac by. The rows it can match are the
//! registry's live ones and the ones this plan already wants, so a run over an empty registry
//! makes the rows repeated runs would. A name two rows answer is a finding: nothing is written
//! onto either and no row is born for it.
//!
//! A row of a source that lists a Mac's names together (`m36` with the alias `m104`) links those
//! names for every source of the run, whichever is read first ([`NameLinks`]), and the node and
//! host it states find a row for any of them. Every name a source states for a row is wanted on it,
//! as its label or an alias, so the name still answers once the source that linked it is gone, and
//! an import never retracts a name a sheet still says.
//!
//! A table can also move a name: it lists `m5` and `m7` apart, with the alias `m104` on `m7`, while a
//! row going by `m5` holds `m104`. What an earlier import of a source this run read wrote there
//! answers for nothing, since this same plan takes it back ([`Planner::gives_up`]), so `m7` keeps a
//! row of its own and the name moves in one plan. Held by the owner, disputed by a row's conflicted
//! label, or stated by a source that reaches the row by a node or host it holds or this plan writes
//! on it, it is a finding and nothing is folded into that row. A table row whose own label a row
//! holds as an alias still finds that row, which is how a renamed Mac's old name finds it.
//!
//! The roster moves a row's label the same way: one it gives another row later in its list waits for
//! that row to move off it, so a chain of renames settles in one plan. Two rows trading labels stay a
//! finding, since nothing shows which came first. A row that frees a name is written before the row
//! that takes it, so a run cut short leaves no name on two rows; rows trading aliases have no such
//! order, and the next run finishes what one cut short.
//!
//! A row holding another node than the one stated is another Mac's, whatever else it shares
//! (`register::pick`'s rule): a host finds it no row, and a name it answers is a finding.

use super::{
    Births, Coord, Dump, Finding, Kind, KnownMac, Label, Planner, ReadNow, Sheet, SheetKind, guard,
    json, keys, source_row,
};
use crate::{
    alab::{
        register::same_host,
        registry::{MACHINE, SOURCE},
    },
    typed::cell::{self, Field},
};
use aterm_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// The machine names this run's sources state together.
#[derive(Debug, Default)]
pub(super) struct NameLinks {
    links: BTreeMap<String, BTreeSet<String>>,
    /// Names two rows of one source state under different labels, or two sources state as an alias
    /// of different labels: such a name links nothing and resolves to nothing, because it may mean
    /// two machines.
    contested: BTreeSet<String>,
    /// The transport ids a source states beside each name.
    nodes: BTreeMap<String, BTreeSet<String>>,
    /// The mesh ids a source states beside each name: a row's `node`.
    principals: BTreeMap<String, BTreeSet<String>>,
    /// The hosts a source states beside each name.
    hosts: BTreeMap<String, BTreeSet<String>>,
    /// Each source's machine table.
    tables: Vec<Table>,
}

/// One source's machine table: every name each label's row states, and the names any row states
/// beside its label.
#[derive(Debug, Default)]
struct Table {
    rows: BTreeMap<String, BTreeSet<String>>,
    aliases: BTreeSet<String>,
}

/// One row of a source: its label, every name it states (the label among them), and the node and
/// host it states.
#[derive(Debug, Default)]
struct Stated {
    label: String,
    names: BTreeSet<String>,
    node: Option<String>,
    principal: Option<String>,
    host: Option<String>,
}

impl NameLinks {
    /// The links the machine tables of `sheets` and the roster state.
    pub(super) fn read(sheets: &[Sheet], macs: &[KnownMac]) -> Self {
        let mut links = Self::default();
        let roster: Vec<Stated> = macs.iter().filter_map(mac_names).collect();
        let mut alias_of: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for rows in sheets.iter().map(sheet_rows).chain([roster]) {
            links.table(&rows);
            for row in &rows {
                for name in row.names.iter().filter(|name| **name != row.label) {
                    alias_of
                        .entry(name.clone())
                        .or_default()
                        .insert(row.label.clone());
                }
            }
        }
        links.contested.extend(
            alias_of
                .into_iter()
                .filter(|(_, labels)| labels.len() > 1)
                .map(|(name, _)| name),
        );
        links
    }

    fn table(&mut self, rows: &[Stated]) {
        let mut labels: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        let mut table = Table::default();
        for row in rows {
            table
                .rows
                .entry(row.label.clone())
                .or_default()
                .extend(row.names.iter().cloned());
            table
                .aliases
                .extend(row.names.iter().filter(|name| **name != row.label).cloned());
            for name in &row.names {
                labels.entry(name).or_default().insert(&row.label);
                for other in row.names.iter().filter(|other| *other != name) {
                    self.links
                        .entry(name.clone())
                        .or_default()
                        .insert(other.clone());
                }
                if let Some(node) = &row.node {
                    self.nodes
                        .entry(name.clone())
                        .or_default()
                        .insert(node.clone());
                }
                if let Some(principal) = &row.principal {
                    self.principals
                        .entry(name.clone())
                        .or_default()
                        .insert(principal.clone());
                }
                if let Some(host) = &row.host {
                    self.hosts
                        .entry(name.clone())
                        .or_default()
                        .insert(host.clone());
                }
            }
        }
        self.contested.extend(
            labels
                .into_iter()
                .filter(|(_, labels)| labels.len() > 1)
                .map(|(name, _)| name.to_owned()),
        );
        self.tables.push(table);
    }

    fn contested(&self, name: &str) -> bool {
        self.contested.contains(name)
    }

    /// Whether a table states `alias` of another row and lists `label` as a row without it, and no
    /// source gives `alias` to two rows.
    fn disowns(&self, alias: &str, label: &str) -> bool {
        !self.contested(alias)
            && self.tables.iter().any(|table| {
                table.aliases.contains(alias)
                    && table
                        .rows
                        .get(label)
                        .is_some_and(|names| !names.contains(alias))
            })
    }

    /// `name` and every name linked to it through names no source contests.
    pub(super) fn of(&self, name: &str) -> BTreeSet<String> {
        let mut names = BTreeSet::from([name.to_owned()]);
        let mut todo = vec![name.to_owned()];
        while let Some(next) = todo.pop() {
            for linked in self.links.get(&next).into_iter().flatten() {
                if !self.contested(linked) && names.insert(linked.clone()) {
                    todo.push(linked.clone());
                }
            }
        }
        names
    }

    /// What sources state beside any of `names`, from `map`.
    fn beside(
        names: &BTreeSet<String>,
        map: &BTreeMap<String, BTreeSet<String>>,
    ) -> BTreeSet<String> {
        names
            .iter()
            .filter_map(|name| map.get(name))
            .flatten()
            .cloned()
            .collect()
    }
}

/// Every canonical name `row` goes by, and each alias. Raw fixtures retain label candidates.
fn row_names(row: &cell::Row) -> BTreeSet<&str> {
    let mut names: BTreeSet<&str> = row
        .fields
        .get("aliases")
        .map(|field| field.members().into_iter().collect())
        .unwrap_or_default();
    if let Some(canonical) = row.claims.get("label") {
        names.extend(canonical.as_ref().and_then(Value::as_str));
    } else if let Some(field) = row.fields.get("label") {
        names.extend(field.candidates_x().into_iter().filter_map(Value::as_str));
    }
    names
}

/// An `aliases` cell's item as a machine name, a dotted spelling (`m27.2`) read as its stored one.
pub(super) fn alias_name(part: &str) -> Option<String> {
    Label::parse(part)
        .or_else(|_| Label::parse(&part.replace('.', "-")))
        .ok()
        .map(|label| label.canonical())
}

/// What each machine row of a sheet states; nothing for any other table. A cell or a row the
/// replica holds concurrent values for states nothing.
fn sheet_rows(sheet: &Sheet) -> Vec<Stated> {
    let separators: &[char] = match sheet.kind {
        SheetKind::Computers => &[',', ';', ' '],
        SheetKind::FleetDb(Dump::Computers) => &['\n', ',', ';'],
        _ => return Vec::new(),
    };
    let Some(label) = sheet.column(|h| matches!(h, "id" | "machine" | "label")) else {
        return Vec::new();
    };
    let aliases = sheet.column(|h| h == "aliases");
    let node = sheet.column(|h| h == "node_id");
    let host = sheet.column(|h| h == "hostname");
    let mut rows = Vec::new();
    for (index, cells) in sheet.rows.iter().enumerate() {
        let row = index + 2;
        if sheet.conflicted.contains(&(row, usize::MAX)) {
            continue;
        }
        let Some(stated) = cells
            .get(label)
            .and_then(|cell| Label::parse(cell.trim()).ok())
        else {
            continue;
        };
        let raw = cells.get(label).map_or("", |cell| cell.trim());
        let cell = |col: Option<usize>| {
            col.filter(|&col| !sheet.conflicted.contains(&(row, col)))
                .and_then(|col| cells.get(col))
        };
        let stated = stated.canonical();
        let mut names = BTreeSet::from([stated.clone()]);
        names.extend(
            cell(aliases)
                .into_iter()
                .flat_map(|cell| cell.split(separators))
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .filter_map(alias_name),
        );
        rows.push(Stated {
            label: stated,
            names,
            node: cell(node).and_then(|cell| node_word(cell)),
            principal: cell(node).and_then(|cell| principal_word(cell)),
            host: cell(host).and_then(|cell| host_word(cell, raw)),
        });
    }
    rows
}

fn mac_names(mac: &KnownMac) -> Option<Stated> {
    let names: BTreeSet<String> = mac
        .label
        .iter()
        .chain(&mac.aliases)
        .map(Label::canonical)
        .collect();
    let label = mac
        .label
        .as_ref()
        .map(Label::canonical)
        .or_else(|| names.first().cloned())?;
    Some(Stated {
        label,
        names,
        ..Stated::default()
    })
}

/// What a machine name resolves to.
enum Named {
    Row(String),
    /// No row yet, and the roster knows the Mac: it is born with what the roster says.
    Known(KnownMac),
    Free,
    Clash(Kind),
}

/// The one element of a set of candidates, none, or the clash `kind` says two are.
fn sole(candidates: BTreeSet<String>, kind: Kind) -> Result<Option<String>, Kind> {
    if candidates.len() > 1 {
        return Err(kind);
    }
    Ok(candidates.into_iter().next())
}

impl Planner<'_> {
    /// The live rows and this plan's rows that hold one of `names`, as a label or an alias.
    fn rows_named(&self, names: &BTreeSet<String>) -> BTreeSet<String> {
        let mut hits: BTreeSet<String> = self.current[MACHINE.name]
            .rows
            .iter()
            .filter(|(_, row)| row.live())
            .filter(|(pk, row)| {
                // A conflicted label answers for each candidate, as `register::birth_label` reads it.
                row.fields.get("label").is_some_and(|field| {
                    field
                        .candidates_x()
                        .into_iter()
                        .filter_map(Value::as_str)
                        .any(|label| names.contains(label))
                }) || row.fields.get("aliases").is_some_and(|field| {
                    field
                        .members()
                        .iter()
                        .any(|m| names.contains(*m) && !self.gives_up(pk, row, m))
                })
            })
            .map(|(pk, _)| pk.clone())
            .collect();
        for (pk, row) in self.rows.get(MACHINE.name).into_iter().flatten() {
            let label = row.values.get("label").and_then(|want| want.x.as_str());
            if label.is_some_and(|label| names.contains(label))
                || row
                    .elements
                    .get("aliases")
                    .is_some_and(|set| set.keys().any(|alias| names.contains(alias)))
            {
                hits.insert(pk.clone());
            }
        }
        hits
    }

    /// Whether a table lists a name `row` goes by as a row without `alias`, and states `alias` of
    /// another ([`NameLinks::disowns`]). A table's row lists its own label, so `alias` itself is no
    /// such name.
    fn disowned(&self, row: &cell::Row, alias: &str) -> bool {
        row_names(row)
            .into_iter()
            .any(|name| self.links.disowns(alias, name))
    }

    /// Whether a source row stating `alias` reaches `pk` by what else it states (a transport id, a
    /// mesh id the row holds as its `node` or was born under, or a host), or a known Mac going by
    /// `alias` does: `pk` keeps being told the name, so it cannot move. With `wanted`, the node cells
    /// and hosts this plan wants for `pk` reach it too.
    fn reached_stating(&self, pk: &str, row: &cell::Row, alias: &str, wanted: bool) -> bool {
        let names = BTreeSet::from([alias.to_owned()]);
        let want = self
            .row_get(MACHINE.name, pk)
            .filter(|_| wanted)
            .map(|want| &want.values);
        let held = |column: &str, node: &str| {
            row.get(column).and_then(Value::as_str) == Some(node)
                || want
                    .and_then(|values| values.get(column))
                    .is_some_and(|want| want.x == json!(node))
        };
        let hosts: Vec<&str> = row
            .get("hosts")
            .and_then(Value::as_array)
            .into_iter()
            .chain(
                want.and_then(|values| values.get("hosts"))
                    .and_then(|want| want.x.as_array()),
            )
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let on_host = |host: &str| hosts.iter().any(|mine| same_host(mine, host));
        NameLinks::beside(&names, &self.links.nodes)
            .iter()
            .any(|node| held("fleet_node", node))
            || NameLinks::beside(&names, &self.links.principals)
                .iter()
                .any(|node| held("node", node) || self.row_under_key(node).as_deref() == Some(pk))
            || NameLinks::beside(&names, &self.links.hosts)
                .iter()
                .any(|host| on_host(host))
            || self.known(&names).iter().any(|mac| {
                valid_node(mac).is_some_and(|node| {
                    held("node", &node) || self.row_under_key(&node).as_deref() == Some(pk)
                }) || valid_transport(mac).is_some_and(|node| held("fleet_node", &node))
                    || valid_hosts(mac).into_iter().any(on_host)
            })
    }

    /// Whether this plan takes `alias` off `row`: a table disowns it, an earlier import of a source
    /// this run reads as it is wrote it (what an edited replica's earlier import wrote is only
    /// reported, so it stays), no source still tells the row the name (by what the row holds, or what
    /// an earlier pass came to want for it: `names_reached`), and the row's label is settled. It
    /// answers for nothing, so the row the table states it of takes it in the same plan.
    fn gives_up(&self, pk: &str, row: &cell::Row, alias: &str) -> bool {
        matches!(row.fields.get("label"), None | Some(Field::Value(_)))
            && self.disowned(row, alias)
            && !Self::linked_to(&self.links, row, alias)
            && !self
                .names_reached
                .get(pk)
                .is_some_and(|names| names.contains(alias))
            && !self.reached_stating(pk, row, alias, false)
            && matches!(row.fields.get("aliases"), Some(Field::Set(elements))
            if elements.get(alias).is_some_and(|field| {
                source_row(field, &self.current[SOURCE.name])
                    .is_some_and(|(key, wrote)| ReadNow::of(self.input).reads_as_written(key, wrote))
            }))
    }

    /// The names this pass gives up that the node or hosts it came to want for their row reach
    /// after all: a source states them beside what the row will hold, so `plan` keeps them in its
    /// next pass. Only names this pass gives up, so each pass keeps more.
    pub(super) fn reached_later(&self) -> BTreeMap<String, BTreeSet<String>> {
        let mut later: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (pk, row) in &self.current[MACHINE.name].rows {
            let Some(aliases) = row.fields.get("aliases").filter(|_| row.live()) else {
                continue;
            };
            for alias in aliases.members() {
                if self.gives_up(pk, row, alias) && self.reached_stating(pk, row, alias, true) {
                    later
                        .entry(pk.clone())
                        .or_default()
                        .insert(alias.to_owned());
                }
            }
        }
        later
    }

    /// Put every row that gives a name up in the plan, which is where `finish` takes it back,
    /// whichever rows the sources' other rows found, and write it before any row taking that name. A
    /// name a table disowns that its row cannot give up stays, as `names_kept`, with the finding.
    pub(super) fn names_given_up(&mut self) {
        let mut given = Vec::new();
        let mut kept: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (pk, row) in &self.current[MACHINE.name].rows {
            let Some(aliases) = row.fields.get("aliases").filter(|_| row.live()) else {
                continue;
            };
            for alias in aliases.members() {
                if self.gives_up(pk, row, alias) {
                    given.push((pk.clone(), alias.to_owned()));
                } else if self.disowned(row, alias) {
                    kept.entry(pk.clone()).or_default().insert(alias.to_owned());
                }
            }
        }
        self.names_kept = kept;
        let table = self.rows.entry(MACHINE.name).or_default();
        for (giver, alias) in given {
            table.entry(giver.clone()).or_default();
            for (pk, want) in table.iter() {
                let as_alias = want
                    .elements
                    .get("aliases")
                    .is_some_and(|names| names.contains_key(&alias));
                let as_label = want
                    .values
                    .get("label")
                    .is_some_and(|label| label.x.as_str() == Some(alias.as_str()));
                if as_alias || as_label {
                    self.frees_first.push((giver.clone(), pk.clone()));
                }
            }
        }
    }

    /// Whether `pk` answers `names` only through an alias a table disowns and this plan does not take
    /// back: what the table says of the name is a finding, and nothing folds into that row.
    fn holds_disowned(&self, pk: &str, names: &BTreeSet<String>) -> bool {
        let Some(row) = self.current[MACHINE.name].rows.get(pk) else {
            return false;
        };
        let by_label = row.fields.get("label").is_some_and(|field| {
            field
                .candidates_x()
                .into_iter()
                .filter_map(Value::as_str)
                .any(|label| names.contains(label))
        });
        !by_label
            && row.fields.get("aliases").is_some_and(|field| {
                field
                    .members()
                    .iter()
                    .any(|m| names.contains(*m) && self.disowned(row, m))
            })
    }

    /// The live rows and this plan's rows whose node cell `column` holds `node`: one, or a clash any
    /// writer could make, since it is an owner cell. A Mac has two: `node`, the key-derived
    /// principal it publishes itself and the roster derives, and `fleet_node`, the older transport
    /// id a replica's `node_id` states.
    fn rows_on_node(&self, column: &str, node: &str) -> BTreeSet<String> {
        let mut hits: BTreeSet<String> = self.current[MACHINE.name]
            .rows
            .iter()
            .filter(|(_, row)| row.live() && row.get(column).and_then(Value::as_str) == Some(node))
            .map(|(pk, _)| pk.clone())
            .collect();
        for (pk, row) in self.rows.get(MACHINE.name).into_iter().flatten() {
            if row
                .values
                .get(column)
                .is_some_and(|want| want.x == json!(node))
            {
                hits.insert(pk.clone());
            }
        }
        hits
    }

    /// The live rows and this plan's rows holding one of `hosts`, with or without `.local`.
    fn rows_on_host(&self, hosts: &[&str]) -> BTreeSet<String> {
        let holds = |list: Option<&Value>| {
            list.and_then(Value::as_array).is_some_and(|list| {
                list.iter()
                    .filter_map(Value::as_str)
                    .any(|held| hosts.iter().any(|host| same_host(held, host)))
            })
        };
        let mut hits: BTreeSet<String> = self.current[MACHINE.name]
            .rows
            .iter()
            .filter(|(_, row)| row.live() && holds(row.get("hosts")))
            .map(|(pk, _)| pk.clone())
            .collect();
        for (pk, row) in self.rows.get(MACHINE.name).into_iter().flatten() {
            if holds(row.values.get("hosts").map(|want| &want.x)) {
                hits.insert(pk.clone());
            }
        }
        hits
    }

    /// The node cell `column` of `pk`: what this plan wants, else what it holds.
    fn node_cell(&self, pk: &str, column: &str) -> Option<String> {
        self.row_get(MACHINE.name, pk)
            .and_then(|row| row.values.get(column))
            .map(|want| &want.x)
            .or_else(|| self.current[MACHINE.name].rows.get(pk)?.get(column))
            .and_then(Value::as_str)
            .map(str::to_owned)
    }

    /// Whether `pk` holds a transport id other than those stated, or a principal other than this
    /// one, which makes it another Mac's. Nothing stated says nothing.
    fn other_mac(&self, pk: &str, transports: &BTreeSet<String>, principal: Option<&str>) -> bool {
        let transport = self.node_cell(pk, "fleet_node");
        let other_transport =
            !transports.is_empty() && transport.is_some_and(|held| !transports.contains(&held));
        let other_principal = principal
            .is_some_and(|node| self.node_cell(pk, "node").is_some_and(|held| held != node));
        other_transport || other_principal
    }

    /// The one mesh id sources state beside `names`: two say nothing of which Mac it is.
    fn stated_principal(&self, names: &BTreeSet<String>) -> Option<String> {
        let stated = NameLinks::beside(names, &self.links.principals);
        (stated.len() == 1)
            .then(|| stated.into_iter().next())
            .flatten()
    }

    /// The row that what sources state beside `names` finds, by node, then host, then the names
    /// themselves. A name answered by a row another Mac's node holds is a clash, never a match.
    fn found(
        &self,
        names: &BTreeSet<String>,
        mut transports: BTreeSet<String>,
        principal: Option<&str>,
    ) -> Result<Option<String>, Kind> {
        let stated = self.stated_principal(names);
        let principal = principal.or(stated.as_deref());
        if let Some(node) = principal
            && let Some(pk) = sole(self.rows_on_node("node", node), Kind::NodeAmbiguous)?
        {
            return Ok(Some(pk));
        }
        transports.extend(NameLinks::beside(names, &self.links.nodes));
        let on_node: BTreeSet<String> = transports
            .iter()
            .flat_map(|node| self.rows_on_node("fleet_node", node))
            .collect();
        if let Some(pk) = sole(on_node, Kind::NodeAmbiguous)? {
            if self.other_mac(&pk, &transports, principal) {
                return Err(Kind::LabelAmbiguous);
            }
            return Ok(Some(pk));
        }
        let stated = NameLinks::beside(names, &self.links.hosts);
        let hosts: Vec<&str> = stated.iter().map(String::as_str).collect();
        let mut on_host = self.rows_on_host(&hosts);
        on_host.retain(|pk| !self.other_mac(pk, &transports, principal));
        if let Some(pk) = sole(on_host, Kind::HostAmbiguous)? {
            return Ok(Some(pk));
        }
        let named = self.rows_named(names);
        if named
            .iter()
            .any(|pk| self.other_mac(pk, &transports, principal) || self.holds_disowned(pk, names))
        {
            return Err(Kind::LabelAmbiguous);
        }
        sole(named, Kind::LabelAmbiguous)
    }

    /// Whether the roster's `label` would land on `pk`: one this plan already wants for it stays, as
    /// does a label an import of another source or the owner wrote, and the roster only reports it
    /// (`decide`).
    fn label_lands(&self, pk: &str, label: &str) -> bool {
        let wanted = self
            .row_get(MACHINE.name, pk)
            .and_then(|row| row.values.get("label"));
        if wanted.is_some_and(|want| want.x != json!(label)) {
            return false;
        }
        let Some(field) = self.current[MACHINE.name]
            .rows
            .get(pk)
            .and_then(|row| row.fields.get("label"))
        else {
            return true;
        };
        match field {
            Field::Value(candidate) if candidate.value.x == json!(label) => true,
            Field::Value(candidate) if candidate.value.x.is_null() => candidate.value.s.is_some(),
            Field::Value(_) => {
                source_row(field, &self.current[SOURCE.name]).is_some_and(|(_, wrote)| {
                    wrote.get("kind").and_then(Value::as_str) == Some("swarm-roster")
                })
            }
            Field::Set(_) | Field::Conflict { .. } => false,
        }
    }

    /// The label `pk` will hold: what this plan wants and can write, else what it holds.
    fn label_of(&self, pk: &str) -> Option<String> {
        self.row_get(MACHINE.name, pk)
            .and_then(|row| row.values.get("label"))
            .and_then(|want| want.x.as_str())
            .filter(|label| self.label_lands(pk, label))
            .or_else(|| {
                self.current[MACHINE.name]
                    .rows
                    .get(pk)?
                    .get("label")?
                    .as_str()
            })
            .map(str::to_owned)
    }

    /// Whether a row other than `pk` answers `name`, or a source says `name` means two machines.
    fn name_clash(&self, pk: &str, name: &str) -> bool {
        self.links.contested(name)
            || self
                .rows_named(&BTreeSet::from([name.to_owned()]))
                .iter()
                .any(|hit| hit != pk)
    }

    /// Want `alias` as another name of `pk`; its own label needs none. A name another row answers
    /// is a finding, never a second holder, and an element the row already holds stays wanted:
    /// retracting it would erase what shows the two rows are one Mac.
    pub(super) fn alias_element(&mut self, pk: &str, alias: &str, at: &Coord) {
        self.stated.insert(alias.to_owned());
        if self.label_of(pk).as_deref() == Some(alias) {
            return;
        }
        if self.name_clash(pk, alias) {
            self.findings.insert(Finding {
                kind: Kind::LabelAmbiguous,
                collection: Some(MACHINE.name),
                pk: Some(pk.to_owned()),
                column: Some("aliases".to_owned()),
                at: vec![at.clone()],
            });
            let held = self.current[MACHINE.name]
                .rows
                .get(pk)
                .and_then(|row| row.fields.get("aliases"))
                .is_some_and(|field| field.has_member(alias));
            if !held {
                return;
            }
        }
        self.element(&MACHINE, pk, "aliases", alias, at);
    }

    /// The known Macs any of `names` names, by a label or an alias.
    fn known(&self, names: &BTreeSet<String>) -> Vec<&KnownMac> {
        self.input
            .macs
            .iter()
            .filter(|mac| {
                mac.label
                    .iter()
                    .chain(&mac.aliases)
                    .any(|name| names.contains(&name.canonical()))
            })
            .collect()
    }

    /// A live row or this plan's row born under this principal's own key, however it is labelled
    /// now: the roster keys a Mac's row by its node, and only the Mac itself writes the `node` cell.
    fn row_under_key(&self, node: &str) -> Option<String> {
        let rows = &self.current[MACHINE.name].rows;
        let planned = self.rows.get(MACHINE.name);
        let answer = std::cell::RefCell::new(None);
        keys::planned_machine_pk(&format!("n-{node}"), |pk| {
            let wanted = planned.is_some_and(|table| table.contains_key(pk));
            let row = rows.get(pk);
            if wanted || row.is_some_and(|row| row.live()) {
                answer.borrow_mut().get_or_insert_with(|| pk.to_owned());
            }
            wanted || row.is_some()
        });
        answer.into_inner()
    }

    /// The row a known Mac already has: by its node principal or its transport id (naming the cell),
    /// under its own key, then a host, then what its names find ([`Self::found`]). `Err` names the
    /// clash when two rows answer, or a name links it to another known Mac: no row is the Mac's
    /// until the owner keeps one.
    fn known_found(&self, mac: &KnownMac) -> Result<Option<(String, Option<&'static str>)>, Kind> {
        let principal = valid_node(mac);
        if let Some(node) = &principal
            && let Some(pk) = sole(self.rows_on_node("node", node), Kind::NodeAmbiguous)?
        {
            return Ok(Some((pk, Some("node"))));
        }
        let transport = valid_transport(mac);
        if let Some(node) = &transport
            && let Some(pk) = sole(self.rows_on_node("fleet_node", node), Kind::NodeAmbiguous)?
        {
            if self.other_mac(&pk, &BTreeSet::new(), principal.as_deref()) {
                return Err(Kind::NodeAmbiguous);
            }
            return Ok(Some((pk, Some("fleet_node"))));
        }
        if let Some(pk) = principal
            .as_deref()
            .and_then(|node| self.row_under_key(node))
        {
            return Ok(Some((pk, None)));
        }
        let transports: BTreeSet<String> = transport.into_iter().collect();
        let mut on_host = self.rows_on_host(&valid_hosts(mac));
        on_host.retain(|pk| !self.other_mac(pk, &transports, principal.as_deref()));
        if let Some(pk) = sole(on_host, Kind::HostAmbiguous)? {
            return Ok(Some((pk, None)));
        }
        if mac
            .label
            .as_ref()
            .is_some_and(|label| self.links.contested(&label.canonical()))
        {
            return Err(Kind::LabelAmbiguous);
        }
        let mut names = BTreeSet::new();
        for name in mac.label.iter().chain(&mac.aliases) {
            let stored = name.canonical();
            if !self.links.contested(&stored) {
                names.extend(self.links.of(&stored));
            }
        }
        if self.known(&names).iter().any(|other| *other != mac) {
            return Err(Kind::LabelAmbiguous);
        }
        let found = self.found(&names, transports, principal.as_deref())?;
        Ok(found.map(|pk| (pk, None)))
    }

    /// What the machine name `stored` resolves to, with no side effect.
    fn named(&self, stored: &str) -> Named {
        if self.links.contested(stored) {
            return Named::Clash(Kind::LabelAmbiguous);
        }
        let names = self.links.of(stored);
        match self.known(&names).as_slice() {
            [] => match self.found(&names, BTreeSet::new(), None) {
                Ok(Some(pk)) => Named::Row(pk),
                Ok(None) => Named::Free,
                Err(kind) => Named::Clash(kind),
            },
            [mac] => match self.known_found(mac) {
                Ok(Some((pk, _))) => Named::Row(pk),
                Ok(None) => Named::Known((*mac).clone()),
                Err(kind) => Named::Clash(kind),
            },
            _ => Named::Clash(Kind::LabelAmbiguous),
        }
    }

    /// The row a machine name means: a row answering it or a name linked to it; else the known Mac
    /// it names, born as the roster says (a source names it now); else a planned placeholder keyed
    /// by the label
    /// ([`keys::planned_machine_pk`]: a retired row holding that key leaves it, and the placeholder
    /// takes the first free `<label>/<n>`, as `add machine` keys it, so the old row's history stays
    /// apart). `None` when two rows answer, or births are off and nothing matched.
    pub(super) fn machine_for(&mut self, label: Label, at: &Coord) -> Option<String> {
        let stored = label.canonical();
        self.stated.insert(stored.clone());
        let pk = match self.named(&stored) {
            Named::Clash(kind) => {
                self.find(kind, Some(MACHINE.name), None, vec![at.clone()]);
                return None;
            }
            Named::Row(pk) => pk,
            Named::Known(mac) => self.known_row(&mac, true)?,
            Named::Free if self.input.births != Births::None => {
                let rows = &self.current[MACHINE.name].rows;
                let pk = keys::planned_machine_pk(&stored, |pk| rows.contains_key(pk))?;
                self.want(&MACHINE, &pk, "label", json!(stored), at);
                self.want(&MACHINE, &pk, "status", json!("planned"), at);
                pk
            }
            Named::Free => return None,
        };
        self.alias_element(&pk, &stored, at);
        Some(pk)
    }

    /// The rows a source's machine row finds by the node and the host it states: `Ok(None)` for
    /// neither, or a row found, by those alone.
    fn stated_row(
        &self,
        label: &str,
        node: Option<&str>,
        host: Option<&str>,
    ) -> Result<Option<String>, Kind> {
        let principal = self.stated_principal(&self.links.of(label));
        if let Some(mesh) = &principal
            && let Some(pk) = sole(self.rows_on_node("node", mesh), Kind::NodeAmbiguous)?
        {
            return Ok(Some(pk));
        }
        let on_node = node.map_or_else(BTreeSet::new, |node| self.rows_on_node("fleet_node", node));
        if let Some(pk) = sole(on_node, Kind::NodeAmbiguous)? {
            return Ok(Some(pk));
        }
        let transports: BTreeSet<String> = node.map(str::to_owned).into_iter().collect();
        let mut on_host = host.map_or_else(BTreeSet::new, |host| self.rows_on_host(&[host]));
        on_host.retain(|pk| !self.other_mac(pk, &transports, principal.as_deref()));
        sole(on_host, Kind::HostAmbiguous)
    }

    /// The row a source's machine row means: by the fleet node and the host it states, then by its
    /// label ([`Self::machine_for`]). `None` when two rows answer, or births are off and nothing
    /// matched.
    pub(super) fn machine_at(
        &mut self,
        label: Label,
        node: Option<&str>,
        host: Option<&str>,
        at: &Coord,
    ) -> Option<String> {
        match self.stated_row(&label.canonical(), node, host) {
            Err(kind) => {
                self.find(kind, Some(MACHINE.name), None, vec![at.clone()]);
                None
            }
            Ok(None) => self.machine_for(label, at),
            Ok(Some(pk)) => {
                let stored = label.canonical();
                self.stated.insert(stored.clone());
                self.alias_element(&pk, &stored, at);
                Some(pk)
            }
        }
    }

    /// The machine key a label names, with no side effect: what [`Self::machine_for`] resolves
    /// to, for comparing a lower source's machines with the ones a higher source named.
    pub(super) fn machine_key(&self, label: Label) -> Option<String> {
        let stored = label.canonical();
        let rows = &self.current[MACHINE.name].rows;
        match self.named(&stored) {
            Named::Row(pk) => Some(pk),
            Named::Known(mac) => known_birth(&mac, |pk| rows.contains_key(pk)),
            Named::Free => keys::planned_machine_pk(&stored, |pk| rows.contains_key(pk)),
            Named::Clash(_) => None,
        }
    }

    /// [`Self::machine_key`] for a source's row that states a node and a host, which come first.
    pub(super) fn machine_key_at(
        &self,
        label: Label,
        node: Option<&str>,
        host: Option<&str>,
    ) -> Option<String> {
        match self.stated_row(&label.canonical(), node, host) {
            Ok(Some(pk)) => Some(pk),
            Ok(None) => self.machine_key(label),
            Err(_) => None,
        }
    }

    /// The row a source that only compares its machine means ([`Self::machine_key`]). A name a
    /// row answers stays that row's while this source says it, as any other source's would.
    pub(super) fn machine_compared(&mut self, label: Label, at: &Coord) -> Option<String> {
        let stored = label.canonical();
        let pk = self.machine_key(label)?;
        if self.rows_named(&self.links.of(&stored)).contains(&pk) {
            self.stated.insert(stored.clone());
            self.alias_element(&pk, &stored, at);
        }
        Some(pk)
    }

    /// The `hosts` cell to want of `pk` for a source stating `host`: what the row holds when it
    /// answers that host in any spelling, so a register's fuller list is not rewritten; `None` when
    /// another row answers it, a finding, since a Mac with that host could then adopt neither row.
    pub(super) fn hosts_cell(&mut self, pk: &str, host: &str, at: &Coord) -> Option<Value> {
        let held = self
            .row_get(MACHINE.name, pk)
            .and_then(|row| row.values.get("hosts"))
            .map(|want| &want.x)
            .or_else(|| self.current[MACHINE.name].rows.get(pk)?.get("hosts"))
            .filter(|list| {
                list.as_array().is_some_and(|list| {
                    list.iter()
                        .filter_map(Value::as_str)
                        .any(|held| same_host(held, host))
                })
            })
            .cloned();
        if held.is_some() {
            return held;
        }
        if self.rows_on_host(&[host]).iter().any(|hit| hit != pk) {
            self.findings.insert(Finding {
                kind: Kind::HostAmbiguous,
                collection: Some(MACHINE.name),
                pk: Some(pk.to_owned()),
                column: Some("hosts".to_owned()),
                at: vec![at.clone()],
            });
            return None;
        }
        Some(json!([host]))
    }

    /// The row of a known Mac: by node, then host, then its label or an alias; else, where this run
    /// births it ([`Births`]: `named` is that a source of the run names it), a new row keyed by its
    /// node (or its label); else none, which `adopt` reports (`unadopted`). Its values come from the
    /// roster, so they carry the roster's coordinate, whichever sheet label led here. Its `node`
    /// cell is the Mac's own to publish (`register`), so the roster's word for it matches a row and
    /// writes nothing.
    ///
    /// A row born here is `planned`, which the Mac can adopt (`register::pick` adopts only that by
    /// name), and only a row its Mac published its principal on is `active` on the roster's word.
    fn known_row(&mut self, mac: &KnownMac, named: bool) -> Option<String> {
        let label = mac.label.as_ref().map(Label::canonical);
        let place = self
            .input
            .macs
            .iter()
            .position(|known| known == mac)
            .unwrap_or(0);
        let at = Coord {
            sheet: super::ROSTER,
            source: self.roster.0.clone(),
            row: place + 1,
            col: 0,
        };
        let found = match self.known_found(mac) {
            Ok(found) => found,
            Err(kind) => {
                self.findings.insert(Finding {
                    kind,
                    collection: Some(MACHINE.name),
                    pk: None,
                    column: (kind == Kind::NodeAmbiguous).then(|| "node".to_owned()),
                    at: vec![at],
                });
                return None;
            }
        };
        if let Some((pk, Some(column))) = &found {
            self.findings.insert(Finding {
                kind: Kind::AdoptedByNode,
                collection: Some(MACHINE.name),
                pk: Some(pk.clone()),
                column: Some((*column).to_owned()),
                at: Vec::new(),
            });
        }
        let status = match &found {
            None => Some("planned"),
            Some((pk, Some("node"))) => {
                self.joined.insert(pk.clone());
                Some("active")
            }
            // A row found any other way, `adopt`'s included, waits for its Mac as it is:
            // `register::pick` adopts by name only a `planned` row.
            Some(_) => None,
        };
        let found = found.map(|(pk, _)| pk);
        let born = match self.input.births {
            Births::None => false,
            Births::Named => named,
            Births::Listed => true,
        };
        let pk = match found {
            Some(pk) => pk,
            None if born => {
                let rows = &self.current[MACHINE.name].rows;
                known_birth(mac, |pk| rows.contains_key(pk))?
            }
            // An import says nothing of a seat none of its sources names: the seat is no
            // defect of theirs, and most such seats never register, so the count would stand
            // on every Mac for good. `adopt --findings` is the reader that lists them.
            None if self.input.births != Births::None => return None,
            None => {
                self.findings.insert(Finding {
                    kind: Kind::Unadopted,
                    collection: Some(MACHINE.name),
                    pk: None,
                    column: label.map(|l| format!("label {l}")),
                    at: Vec::new(),
                });
                return None;
            }
        };
        if let Some(label) = &label {
            self.known_label(&pk, label, &at);
        }
        if let Some(status) = status {
            self.want(&MACHINE, &pk, "status", json!(status), &at);
        }
        let hosts = valid_hosts(mac);
        if !hosts.is_empty() {
            self.want(&MACHINE, &pk, "hosts", json!(hosts), &at);
        }
        for alias in &mac.aliases {
            self.alias_element(&pk, &alias.canonical(), &at);
        }
        Some(pk)
    }

    /// Whether this plan moves `pk` off the label `label`: it wants another that lands, and keeps no
    /// alias of the old one.
    fn leaves(&self, pk: &str, label: &str) -> bool {
        let Some(want) = self.row_get(MACHINE.name, pk) else {
            return false;
        };
        let moves = want
            .values
            .get("label")
            .and_then(|want| want.x.as_str())
            .is_some_and(|to| to != label && self.label_lands(pk, to));
        let keeps = want
            .elements
            .get("aliases")
            .is_some_and(|set| set.contains_key(label))
            || self.current[MACHINE.name]
                .rows
                .get(pk)
                .and_then(|row| row.fields.get("aliases"))
                .is_some_and(|field| field.has_member(label));
        moves && !keeps
    }

    /// Whether a row other than `pk` keeps answering the roster's `label`, or a source says it means
    /// two machines ([`Self::name_clash`], but a row this plan moves off it is no holder).
    fn label_clash(&self, pk: &str, label: &str) -> bool {
        self.links.contested(label)
            || self
                .rows_named(&BTreeSet::from([label.to_owned()]))
                .iter()
                .any(|hit| hit != pk && !self.leaves(hit, label))
    }

    /// Want the roster's `label` on `pk`, or wait for the row that holds it to move
    /// ([`Self::known_macs`]).
    fn known_label(&mut self, pk: &str, label: &str, at: &Coord) {
        if self.label_clash(pk, label) {
            self.label_waits
                .push((pk.to_owned(), label.to_owned(), at.clone()));
            return;
        }
        self.land_label(pk, label, at);
    }

    /// Want the roster's `label` on `pk`, which no other row keeps: any row answering it now moves
    /// off it in this plan, and is written first. Where the roster replaces its own earlier label and
    /// a sheet this run still says the old one, the old one stays the row's as an alias, or the next
    /// run would birth a row for it. Where the row's label stays against the roster's, the roster's
    /// name is wanted as an alias, so it still answers this row.
    fn land_label(&mut self, pk: &str, label: &str, at: &Coord) {
        for hit in self.rows_named(&BTreeSet::from([label.to_owned()])) {
            self.frees_first.push((hit, pk.to_owned()));
        }
        let replaced = self.current[MACHINE.name].rows.get(pk).and_then(|row| {
            let field = row.fields.get("label")?;
            let (_, wrote) = source_row(field, &self.current[SOURCE.name])?;
            (wrote.get("kind").and_then(Value::as_str) == Some("swarm-roster"))
                .then(|| row.get("label").and_then(Value::as_str))?
                .filter(|held| *held != label && self.stated.contains(*held))
                .map(str::to_owned)
        });
        self.want(&MACHINE, pk, "label", json!(label), at);
        if !self.label_lands(pk, label) && self.label_of(pk).as_deref() != Some(label) {
            self.element(&MACHINE, pk, "aliases", label, at);
        }
        if let Some(old) = replaced {
            self.alias_element(pk, &old, at);
        }
    }

    pub(super) fn known_macs(&mut self) {
        // The sheets were read first, so a Mac one of them named has its row in this plan.
        for mac in self.input.macs {
            if mac.label.is_some() || valid_node(mac).is_some() || valid_transport(mac).is_some() {
                self.known_row(mac, false);
            }
        }
        // Each pass lands what the last one's moves freed. A label another row still holds at the end
        // is a finding: that row stays, or the two trade labels.
        let mut waits = std::mem::take(&mut self.label_waits);
        loop {
            let before = waits.len();
            waits.retain(|(pk, label, at)| {
                let waiting = self.label_clash(pk, label);
                if !waiting {
                    self.land_label(pk, label, at);
                }
                waiting
            });
            if waits.len() == before {
                break;
            }
        }
        for (pk, _, at) in waits {
            self.findings.insert(Finding {
                kind: Kind::LabelAmbiguous,
                collection: Some(MACHINE.name),
                pk: Some(pk),
                column: Some("label".to_owned()),
                at: vec![at],
            });
        }
    }

    /// Whether a source of this run states `name` beside one of the names `row` already goes by. Such
    /// a name is not retracted when a clash left it unwanted: it is what shows two rows are one Mac.
    pub(super) fn linked_to(links: &NameLinks, row: &cell::Row, name: &str) -> bool {
        links.of(name).iter().any(|linked| {
            linked != name
                && (row.get("label").and_then(Value::as_str) == Some(linked.as_str())
                    || row
                        .fields
                        .get("aliases")
                        .is_some_and(|field| field.has_member(linked)))
        })
    }

    /// Whether `pk`'s status is `planned` now: what this plan wants, else what it holds.
    pub(super) fn status_planned(&self, pk: &str) -> bool {
        let wanted = self
            .row_get(MACHINE.name, pk)
            .and_then(|row| row.values.get("status"))
            .map(|want| &want.x);
        let status = wanted.or_else(|| self.current[MACHINE.name].rows.get(pk)?.get("status"));
        status == Some(&json!("planned"))
    }
}

fn valid_node(mac: &KnownMac) -> Option<String> {
    let node = mac.node.as_deref()?.trim();
    (node.len() == 16 && node.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| node.to_ascii_lowercase())
}

fn valid_transport(mac: &KnownMac) -> Option<String> {
    node_word(mac.fleet_node.as_deref()?)
}

fn valid_hosts(mac: &KnownMac) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    for host in &mac.hosts {
        let h = host.trim();
        if !h.is_empty() && guard::text(h, 64).is_ok() && !out.contains(&h) {
            out.push(h);
            if out.len() == 4 {
                break;
            }
        }
    }
    out
}

/// The key a known Mac with no live row is born under: the planned key
/// ([`keys::planned_machine_pk`]) of its node principal (`n-<node>`), else of its label. Either
/// skips a key `held` by a row: a retired row the node or the label once named keeps its
/// history, and the Mac named again gets a live row of its own.
fn known_birth(mac: &KnownMac, held: impl Fn(&str) -> bool) -> Option<String> {
    match (valid_node(mac), &mac.label) {
        (Some(node), _) => keys::planned_machine_pk(&format!("n-{node}"), held),
        (None, Some(label)) => keys::planned_machine_pk(&label.canonical(), held),
        (None, None) => None,
    }
}

/// A transport id in its stored spelling.
fn node_word(raw: &str) -> Option<String> {
    let node = raw.trim().to_ascii_lowercase();
    aterm_codec::hex::is_lower(&node, 32).then_some(node)
}

/// A mesh id in its stored spelling: a row's `node`, which a replica spells `n-<16 hex>` or bare.
fn principal_word(raw: &str) -> Option<String> {
    let node = raw.trim().to_ascii_lowercase();
    let mesh = node.strip_prefix("n-").unwrap_or(&node);
    aterm_codec::hex::is_lower(mesh, 16).then(|| mesh.to_owned())
}

/// The transport id a replica row states for matching `fleet_node`: a concurrent or malformed cell
/// states none.
pub(super) fn stated_node(raw: Option<(String, Coord, bool)>) -> Option<String> {
    let (node, _, conflicted) = raw?;
    node_word(&node).filter(|_| !conflicted)
}

/// Whether a replica's `hostname` says anything beyond fleet_db's `<label>.local` default.
pub(super) fn host_given(host: &str, label: &str) -> bool {
    host.strip_suffix(".local").is_none_or(|stem| {
        stem.bytes().any(|b| b.is_ascii_uppercase()) || !stem.eq_ignore_ascii_case(label)
    })
}

/// A host a row states for matching: fleet_db's default or an unreadable cell states none.
fn host_word(raw: &str, label: &str) -> Option<String> {
    let host = raw.trim();
    let readable = !host.is_empty() && guard::text(host, 64).is_ok() && !host.contains('@');
    (host_given(host, label) && readable).then(|| host.to_owned())
}

/// The host a replica row states for matching: a concurrent cell or fleet_db's default states none.
pub(super) fn stated_host(raw: Option<(String, Coord, bool)>, label: &str) -> Option<String> {
    let (host, _, conflicted) = raw?;
    host_word(&host, label).filter(|_| !conflicted)
}

#[cfg(test)]
mod tests;
