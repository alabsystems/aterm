// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A TEST COMPILE SHAPED INTO CALLS (`docs/DESIGN-adaptive-build-2026-10-24.md`, VERIFY-SHAPE).
//!
//! A test compile whose estimate claims the run's whole memory budget runs as a few calls in the
//! main lane, one after another, each an ordinary stage with its own memory claim, meter and
//! record ([`Part`]): the cuts, narrower selections the book measured whose every unit the whole
//! call builds too, and last the closing call, today's command over the run's selection, byte for
//! byte, which finds what the cuts built fresh.

use crate::memory::{Key, Records};
use crate::unitgraph::{Graph, Interner, UnitName};

/// WHAT ONE CALL OF A SHAPED TEST COMPILE SELECTS: its crates, and whether it builds their
/// lib-tests alone (`--lib`) or every target with `test = true` (`--tests`, today's).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Piece {
    /// The crates its `-p`s name, as the run's selection spells them
    /// ([`crate::Scope::spec_of`]), sorted, each once.
    pub crates: Vec<String>,
    /// `--lib` in place of `--tests`: one crate's lib-test and what it needs.
    pub lib: bool,
}

impl Piece {
    /// A piece over `crates`, sorted and each once.
    #[must_use]
    pub fn of(crates: &[String], lib: bool) -> Piece {
        let mut crates = crates.to_vec();
        crates.sort();
        crates.dedup();
        Piece { crates, lib }
    }

    /// Its `-p`s.
    #[must_use]
    pub fn args(&self) -> Vec<String> {
        self.crates
            .iter()
            .flat_map(|c| ["-p".to_owned(), c.clone()])
            .collect()
    }

    /// As a person reads it: `-p a -p b`, or `-p a --lib`.
    #[must_use]
    pub fn label(&self) -> String {
        let mut label = self.args().join(" ");
        if self.lib {
            label.push_str(" --lib");
        }
        label
    }
}

/// The variant a lib-test call's record is kept under ([`Part::variant`]).
pub const LIB_VARIANT: &str = "targets --lib";

/// What a call that follows others keeps its record after ([`Part::variant`]).
pub const AFTER: &str = "after ";

/// ONE CALL OF A SHAPED TEST COMPILE ([`crate::plan::StageSpec::part`]): call `index + 1` of
/// `of`, the last the closing call over the run's selection. A part of one call is the stage
/// unshaped, carrying why it could not be shaped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    /// What it builds: for the closing call, the run's selection.
    pub piece: Piece,
    /// Its place among the calls, from 0.
    pub index: usize,
    /// How many calls the stage makes.
    pub of: usize,
    /// The calls before it, in order: what it runs after, in the same build directory.
    pub after: Vec<Piece>,
    /// Why it claims what it does, or why the stage was not shaped, for its time line.
    pub said: String,
    /// The measured peak its claim was planned by: a cut's estimate, at least its heaviest unit;
    /// the closing call's record after the cuts, at least its heaviest unit, or with none, the
    /// peaks of the units no call before it builds summed. A run of it below a
    /// quarter of this compiled almost none of its work and keeps no record
    /// ([`crate::memory::Book::keep`]), even under a key that holds none. `None` where it claims
    /// the whole estimate, which a closing call after cuts does much less than, and unshaped.
    pub planned: Option<u64>,
    /// The cuts a run refused that this plan therefore did not take ([`refusal_key`]), on the
    /// closing call: its stage keeps each refusal again ([`crate::memory::Book::refuse`]), so a
    /// refusal its selection keeps reading is never the oldest row of a record that lets that one
    /// go past [`crate::memory::ROWS_KEPT`] rows.
    pub refused: Vec<Piece>,
}

impl Part {
    /// The stage unshaped: one call over `piece`, its time line saying `said`.
    #[must_use]
    pub const fn whole(piece: Piece, said: String) -> Part {
        Part {
            piece,
            index: 0,
            of: 1,
            after: Vec::new(),
            said,
            planned: None,
            refused: Vec::new(),
        }
    }

    /// Whether it is the closing call: today's command over the run's selection.
    #[must_use]
    pub const fn closing(&self) -> bool {
        self.index + 1 == self.of
    }

    /// Whether the stage was shaped at all: more calls than one.
    #[must_use]
    pub const fn shaped(&self) -> bool {
        self.of > 1
    }

    /// The variant its record is kept under ([`crate::memory::StageKey::of_stage`]): a lib-test
    /// call's [`LIB_VARIANT`]; one that follows other calls, `after <their labels>`, so its peak
    /// (the rest of the work: the calls before it built the units they share with it, its crates'
    /// dependencies too, whether or not they share a crate) never stands for the whole of it; and
    /// none for the first call, whose work is `verify -p <its crates>`'s, under that key.
    #[must_use]
    pub fn variant(&self) -> Option<String> {
        if self.piece.lib {
            return Some(LIB_VARIANT.to_owned());
        }
        if self.after.is_empty() {
            return None;
        }
        let labels: Vec<String> = self.after.iter().map(Piece::label).collect();
        Some(format!("{AFTER}{}", labels.join("; ")))
    }
}

/// The stage a test compile's per-unit peaks are kept under ([`unit_key`]): the book's, which
/// weighs a wider record elsewhere by them too ([`aterm_capacity::book::Records::unit_floor`]).
pub use aterm_capacity::book::UNIT_STAGE;

/// What a refusal row's variant starts with ([`refusal_key`]).
pub const REFUSED: &str = "shape refused ";

/// The most calls a shaped test compile makes before its closing one.
pub const MOST_CUTS: usize = 2;

/// The key unit `name`'s peak is kept under in checkout `checkout`: stage [`UNIT_STAGE`], its
/// package, its kind.
#[must_use]
pub fn unit_key(checkout: &str, name: &UnitName) -> Key {
    Key {
        stage: UNIT_STAGE.to_owned(),
        checkout: checkout.to_owned(),
        crates: Some(vec![name.package.clone()]),
        variant: Some(name.kind.clone()),
    }
}

/// The key of the row that says a cut `cut` of selection `whole` in `checkout` did not hold: a
/// run's closing call rebuilt what it built ([`fresh_check`]), so the planner never cuts it
/// again.
#[must_use]
pub fn refusal_key(checkout: &str, whole: &Piece, cut: &Piece) -> Key {
    Key {
        stage: "TestCompile".to_owned(),
        checkout: checkout.to_owned(),
        crates: Some(whole.crates.clone()),
        variant: Some(format!("{REFUSED}{}", cut.label())),
    }
}

/// What a planner reads: the run's book, and its plain builds' ([`crate::memory::plain_like`]),
/// the checkout it runs in, and the run's memory budget.
#[derive(Clone, Copy, Debug)]
pub struct Shaping<'a> {
    /// Verify's own record.
    pub records: &'a Records,
    /// The plain builds' record.
    pub plain: &'a Records,
    /// The run's checkout ([`crate::Ctx::root`]).
    pub checkout: &'a str,
    /// What the run's stages may claim together ([`crate::memory::Book::budget`]).
    pub budget: u64,
}

/// What a selection is estimated to hold at its peak, and where the figure was read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Estimate {
    /// Bytes.
    pub peak: u64,
    /// Where it was read, as its time line says it.
    pub from: String,
}

/// WHAT [`shape`] PLANNED FOR A TEST COMPILE.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Shaped {
    /// Its calls: the cuts and then the closing call, or one call claiming the whole estimate,
    /// saying why no cut holds.
    Calls(Vec<Call>),
    /// Not shaped: its estimate claims less than the budget, or nothing kept says what it holds
    /// (`None`). The stage claims at least what its estimate does ([`crate::plan::plan`]), which
    /// reads every checkout's record of it and the narrower selections it holds, where its own
    /// stage's lookup ([`crate::memory::need`]) reads its checkout's record first, however light.
    Whole(Option<Estimate>),
}

impl Shaped {
    /// Its calls; `None` when it was not shaped.
    #[must_use]
    pub fn calls(self) -> Option<Vec<Call>> {
        match self {
            Shaped::Calls(calls) => Some(calls),
            Shaped::Whole(_) => None,
        }
    }
}

/// One call of a planned test compile, and what it claims.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Call {
    /// The call.
    pub part: Part,
    /// What it claims, bytes ([`crate::plan::StageSpec::mem`]).
    pub mem: u64,
}

/// The most any checkout's record of `stage` over exactly `crates`, under `variant`, kept, and
/// the checkout that kept it.
fn most(
    records: &Records,
    stage: &str,
    crates: &[String],
    variant: Option<&str>,
) -> Option<(u64, String)> {
    records
        .rows()
        .iter()
        .filter(|r| {
            r.key.stage == stage
                && r.key.crates.as_deref() == Some(crates)
                && r.key.variant.as_deref() == variant
        })
        .filter_map(|r| {
            r.peaks
                .iter()
                .map(|p| p.bytes)
                .max()
                .map(|m| (m, r.key.checkout.clone()))
        })
        .max_by_key(|(m, _)| *m)
}

/// The test compile's record of `piece` kept under `variant`, as an estimate.
fn kept(book: &Shaping<'_>, piece: &Piece, variant: Option<&str>) -> Option<Estimate> {
    most(book.records, "TestCompile", &piece.crates, variant).map(|(peak, checkout)| Estimate {
        peak,
        from: format!(
            "the most `{}`{} kept in {checkout} ({})",
            piece.label(),
            variant.map_or_else(String::new, |v| format!(" ({v})")),
            aterm_capacity::size(peak)
        ),
    })
}

/// The units `graph` builds, summed by the most each one's record kept ([`unit_key`]): what they
/// would hold if every one compiled at once, an upper bound of what they hold together, which
/// counts the units a warm build directory has fresh too. So it stands in only where no record of
/// the selection is kept: measured 2026-10-29, S1 less aterm-gui held 13 GiB where its units
/// summed 21 GiB. `None` with none kept.
fn unit_sum(book: &Shaping<'_>, graph: &Graph) -> Option<Estimate> {
    named_unit_sum(book, graph.names().into_iter())
}

fn named_unit_sum<'a>(
    book: &Shaping<'_>,
    names: impl Iterator<Item = &'a UnitName>,
) -> Option<Estimate> {
    let mut found: Vec<(u64, &UnitName)> = names
        .filter_map(|n| {
            most(
                book.records,
                UNIT_STAGE,
                std::slice::from_ref(&n.package),
                Some(&n.kind),
            )
            .map(|(peak, _)| (peak, n))
        })
        .collect();
    if found.is_empty() {
        return None;
    }
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)));
    let peak = found.iter().fold(0u64, |s, (p, _)| s.saturating_add(*p));
    let heaviest: Vec<String> = found
        .iter()
        .take(3)
        .map(|(p, n)| format!("{} {}", n.kind, aterm_capacity::size(*p)))
        .collect();
    Some(Estimate {
        peak,
        from: format!(
            "its {} heaviest units' peaks summed ({}{})",
            found.len(),
            heaviest.join(", "),
            if found.len() > 3 { ", …" } else { "" }
        ),
    })
}

/// The heaviest unit of `graph` that none of the calls before it builds (`built`, their units'
/// identities), by the most its record kept in any checkout ([`unit_key`]): the least a call over
/// `graph` holds when it compiles that unit, whatever its own record says (a run that found its
/// units fresh keeps a few MiB). `None` with none kept.
fn heaviest_unit(
    book: &Shaping<'_>,
    graph: &Graph,
    built: &std::collections::BTreeSet<u32>,
) -> Option<Estimate> {
    graph
        .units
        .iter()
        .filter(|u| !built.contains(&u.id))
        .filter_map(|u| u.name.as_ref())
        .filter_map(|n| {
            most(
                book.records,
                UNIT_STAGE,
                std::slice::from_ref(&n.package),
                Some(&n.kind),
            )
            .map(|(peak, _)| (peak, n))
        })
        .max_by(|a, b| a.0.cmp(&b.0).then_with(|| b.1.cmp(a.1)))
        .map(|(peak, n)| Estimate {
            peak,
            from: format!(
                "its heaviest unit no call before it builds (`{}`, {})",
                n.kind,
                aterm_capacity::size(peak)
            ),
        })
}

/// The largest of `estimates`.
fn largest(estimates: impl IntoIterator<Item = Option<Estimate>>) -> Option<Estimate> {
    estimates.into_iter().flatten().max_by_key(|e| e.peak)
}

/// What a tests piece holds by its records: the most of its own, in any checkout, and a plain
/// build's ([`crate::memory::plain_like`]); with neither kept, its units' peaks summed when its
/// graph is read ([`unit_sum`]).
fn of_piece(book: &Shaping<'_>, piece: &Piece, graph: Option<&Graph>) -> Option<Estimate> {
    let key = Key {
        stage: "TestCompile".to_owned(),
        checkout: book.checkout.to_owned(),
        crates: Some(piece.crates.clone()),
        variant: None,
    };
    let plain = crate::memory::plain_like(book.plain, &key).map(|l| Estimate {
        peak: l.peak,
        from: format!(
            "the most a plain `{}` kept ({})",
            l.wider.map_or_else(|| "targo test".to_owned(), |k| k.stage),
            aterm_capacity::size(l.peak)
        ),
    });
    largest([kept(book, piece, None), plain]).or_else(|| graph.and_then(|g| unit_sum(book, g)))
}

/// The narrower selections inside `whole` the test compile's record holds, each once, heaviest
/// first.
fn narrower(book: &Shaping<'_>, whole: &Piece) -> Vec<(Piece, Estimate)> {
    let mut seen: Vec<Piece> = Vec::new();
    for r in book.records.rows() {
        if r.key.stage != "TestCompile" || r.key.variant.is_some() {
            continue;
        }
        let Some(crates) = r.key.crates.as_deref() else {
            continue;
        };
        let inside = !crates.is_empty()
            && crates.len() < whole.crates.len()
            && crates.iter().all(|c| whole.crates.contains(c));
        let piece = Piece::of(crates, false);
        if inside && !seen.contains(&piece) {
            seen.push(piece);
        }
    }
    let mut out: Vec<(Piece, Estimate)> = seen
        .into_iter()
        .filter_map(|p| kept(book, &p, None).map(|e| (p, e)))
        .collect();
    out.sort_by(|a, b| {
        b.1.peak
            .cmp(&a.1.peak)
            .then_with(|| a.0.crates.cmp(&b.0.crates))
    });
    out
}

/// The narrower rows' cover: the most of disjoint narrower selections ([`narrower`]), heaviest
/// first, summed.
fn cover(rows: &[(Piece, Estimate)]) -> Option<Estimate> {
    let mut taken: Vec<&str> = Vec::new();
    let mut sum = 0u64;
    let mut said = Vec::new();
    for (p, e) in rows {
        if p.crates.iter().any(|c| taken.contains(&c.as_str())) {
            continue;
        }
        taken.extend(p.crates.iter().map(String::as_str));
        sum = sum.saturating_add(e.peak);
        said.push(format!("`{}` {}", p.label(), aterm_capacity::size(e.peak)));
    }
    (!said.is_empty()).then(|| Estimate {
        peak: sum,
        from: format!(
            "the narrower selections it holds, summed ({})",
            said.join(" + ")
        ),
    })
}

/// A piece as its label spells it (`-p a -p b`, `-p a --lib`).
fn parse_label(label: &str) -> Option<Piece> {
    let mut words = label.split_whitespace().peekable();
    let mut crates = Vec::new();
    let mut lib = false;
    while let Some(w) = words.next() {
        match w {
            "-p" => crates.push(words.next()?.to_owned()),
            "--lib" => lib = true,
            _ => return None,
        }
    }
    (!crates.is_empty()).then(|| Piece::of(&crates, lib))
}

/// The calls `pieces` make in order, each after every earlier one.
fn in_order(pieces: &[Piece]) -> Vec<Part> {
    pieces
        .iter()
        .enumerate()
        .map(|(i, p)| Part {
            piece: p.clone(),
            index: i,
            of: pieces.len(),
            after: pieces[..i].to_vec(),
            said: String::new(),
            planned: None,
            refused: Vec::new(),
        })
        .collect()
}

/// The last shaped run of `whole`'s calls summed: its closing call's record (`after …`) and each
/// cut's, under the keys they were kept by. `None` unless every call's record is kept.
fn last_shaped(book: &Shaping<'_>, whole: &Piece) -> Option<Estimate> {
    let row = book
        .records
        .rows()
        .iter()
        .filter(|r| {
            r.key.stage == "TestCompile"
                && r.key.crates.as_deref() == Some(&whole.crates[..])
                && r.key
                    .variant
                    .as_deref()
                    .is_some_and(|v| v.starts_with(AFTER))
        })
        .max_by_key(|r| r.peaks.iter().map(|p| p.at_ms).max())?;
    let after = row.key.variant.as_deref()?.strip_prefix(AFTER)?;
    let mut pieces: Vec<Piece> = after.split("; ").map(parse_label).collect::<Option<_>>()?;
    pieces.push(whole.clone());
    let mut sum = 0u64;
    for part in in_order(&pieces) {
        let e = kept(book, &part.piece, part.variant().as_deref())?;
        sum = sum.saturating_add(e.peak);
    }
    Some(Estimate {
        peak: sum,
        from: format!(
            "its last shaped run's calls summed ({} calls, after {after})",
            pieces.len()
        ),
    })
}

/// What names `spec`'s package in a unit graph ([`crate::scope::spec_name`]).
fn package(spec: &str) -> &str {
    crate::scope::spec_name(spec).unwrap_or(spec)
}

/// One planning of a test compile ([`shape`]): what it reads, and why the cuts it tried did not
/// hold.
struct Planner<'a, 'p> {
    book: &'a Shaping<'a>,
    whole: Piece,
    interner: Interner,
    probe: &'p mut dyn FnMut(&Piece) -> Result<String, String>,
    why_not: Vec<String>,
    /// The cuts it did not take because a run refused them ([`Part::refused`]).
    refused: Vec<Piece>,
}

impl Planner<'_, '_> {
    fn read(&mut self, piece: &Piece) -> Result<Graph, String> {
        (self.probe)(piece).and_then(|t| Graph::read(&t, &mut self.interner))
    }

    /// What `piece` holds, called after `after`: a lib-test call by its own record, else its
    /// units' peaks; a call after others by its record after them, else by the most of the same
    /// crates called first, which builds more; a first call by [`of_piece`].
    fn estimate(&self, piece: &Piece, after: &[Piece], graph: &Graph) -> Option<Estimate> {
        let mut order = after.to_vec();
        order.push(piece.clone());
        let part = in_order(&order).pop()?;
        if piece.lib {
            kept(self.book, piece, Some(LIB_VARIANT)).or_else(|| unit_sum(self.book, graph))
        } else if part.after.is_empty() {
            of_piece(self.book, piece, Some(graph))
        } else {
            kept(self.book, piece, part.variant().as_deref())
                .or_else(|| of_piece(self.book, piece, Some(graph)))
        }
    }

    /// `piece` as a cut called after `after`, when it holds: its unit graph builds only units the
    /// whole call builds, no run refused it, and its estimate, at least its heaviest unit no call
    /// before it builds ([`heaviest_unit`], excluding `built`), claims less than the budget. Its
    /// estimate and its unit graph; `None`, saying why, otherwise.
    fn cut(
        &mut self,
        piece: &Piece,
        after: &[Piece],
        built: &std::collections::BTreeSet<u32>,
        whole_graph: &Graph,
    ) -> Option<(Estimate, Graph)> {
        let refused = refusal_key(self.book.checkout, &self.whole, piece);
        if self.book.records.rows().iter().any(|r| r.key == refused) {
            if !self.refused.contains(piece) {
                self.refused.push(piece.clone());
            }
            self.why_not.push(format!(
                "`{}` was refused after a run whose closing call rebuilt what it built",
                piece.label()
            ));
            return None;
        }
        let graph = match self.read(piece) {
            Ok(g) => g,
            Err(e) => {
                self.why_not.push(format!(
                    "`{}`'s unit graph could not be read: {e}",
                    piece.label()
                ));
                return None;
            }
        };
        let outside = graph.outside(whole_graph);
        if outside > 0 {
            self.why_not.push(format!(
                "`{}` builds {outside} of its {} units unlike the whole call",
                piece.label(),
                graph.ids().len()
            ));
            return None;
        }
        let heaviest = heaviest_unit(self.book, &graph, built);
        match self
            .estimate(piece, after, &graph)
            .and_then(|e| largest([Some(e), heaviest]))
        {
            Some(e) if aterm_capacity::peak_claim(e.peak) < self.book.budget => Some((e, graph)),
            Some(e) => {
                self.why_not.push(format!(
                    "`{}` claims {}, not under the budget",
                    piece.label(),
                    aterm_capacity::size(aterm_capacity::peak_claim(e.peak))
                ));
                None
            }
            None => {
                self.why_not
                    .push(format!("nothing is kept of what `{}` holds", piece.label()));
                None
            }
        }
    }

    /// The cuts to try, heaviest first by their records: each narrower selection the record
    /// holds, its complement in the whole, and the whole less its heaviest crate.
    fn candidates(&self, rows: &[(Piece, Estimate)], whole_graph: &Graph) -> Vec<Piece> {
        let whole = &self.whole;
        let mut out: Vec<Piece> = Vec::new();
        let mut push = |p: Piece| {
            if !p.crates.is_empty() && p != *whole && !out.contains(&p) {
                out.push(p);
            }
        };
        let less = |gone: &[String]| {
            let rest: Vec<String> = whole
                .crates
                .iter()
                .filter(|c| !gone.contains(c))
                .cloned()
                .collect();
            Piece::of(&rest, false)
        };
        for (p, _) in rows {
            push(p.clone());
            push(less(&p.crates));
        }
        let weight = |c: &String| {
            kept(self.book, &Piece::of(std::slice::from_ref(c), false), None)
                .or_else(|| {
                    named_unit_sum(
                        self.book,
                        whole_graph
                            .names()
                            .into_iter()
                            .filter(|n| n.package == package(c)),
                    )
                })
                .map_or(0, |e| e.peak)
        };
        if whole.crates.len() > 1
            && let Some(heaviest) = whole
                .crates
                .iter()
                .filter(|c| weight(c) > 0)
                .max_by_key(|c| weight(c))
        {
            push(less(std::slice::from_ref(heaviest)));
        }
        let mut ranked: Vec<(Piece, u64)> = out
            .into_iter()
            .map(|p| {
                let e = of_piece(self.book, &p, None).map_or(0, |e| e.peak);
                (p, e)
            })
            .collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.crates.cmp(&b.0.crates)));
        ranked.into_iter().map(|(p, _)| p).collect()
    }
}

/// THE CALLS A TEST COMPILE OVER `selection` MAKES (VERIFY-SHAPE), by `book`, reading each call's
/// unit graph through `probe` (its `--unit-graph`, resolved and never built). [`Shaped::Whole`],
/// with its estimate, when the stage is not shaped: its estimate claims less than the budget, or
/// nothing is known of it. The estimate is the largest of its own record in any checkout, a plain
/// build's ([`crate::memory::plain_like`]), the cover of the narrower selections it holds, and its
/// last shaped run's calls summed; with none of those kept, its units' peaks summed
/// ([`unit_sum`]), its unit graph read only then.
///
/// A cut is a narrower selection the record holds, its complement in `selection`, or `selection`
/// less its heaviest crate, heaviest first, at most [`MOST_CUTS`] of them over no crate twice;
/// and a lib-test call (`-p c --lib`, where `c`'s lib is tested) before a first cut over `c`
/// alone, or before a one-crate selection's closing call. A cut is taken only when its unit graph
/// builds only units the whole call builds ([`Graph::within`]), no run refused it
/// ([`refusal_key`]), and its estimate claims less than the budget. Every call claims at least
/// the heaviest unit no call before it builds ([`heaviest_unit`]), whatever its own record says.
/// The closing call is the whole selection after every cut, claiming by the larger of its record
/// after them and that unit; with no record after them, by the peaks of the units no call before
/// it builds summed ([`unit_sum`]'s bound over its own work, which the units the cuts built,
/// fresh for it, do not swell), where that claims less than the whole estimate; otherwise by the
/// whole estimate (at least the budget, so it runs with no other stage that claims memory). With
/// no cut that holds, one call, saying why.
#[must_use]
pub fn shape(
    selection: &[String],
    book: &Shaping<'_>,
    probe: &mut dyn FnMut(&Piece) -> Result<String, String>,
) -> Shaped {
    let whole = Piece::of(selection, false);
    if whole.crates.is_empty() {
        return Shaped::Whole(None);
    }
    let mut planner = Planner {
        book,
        whole: whole.clone(),
        interner: Interner::default(),
        probe,
        why_not: Vec::new(),
        refused: Vec::new(),
    };
    let rows = narrower(book, &whole);
    let by_rows = largest([
        of_piece(book, &whole, None),
        cover(&rows),
        last_shaped(book, &whole),
    ]);
    let triggers =
        |e: Option<&Estimate>| e.is_some_and(|e| aterm_capacity::peak_claim(e.peak) >= book.budget);
    let units_kept = book.records.rows().iter().any(|r| {
        r.key.stage == UNIT_STAGE
            && r.key.crates.as_ref().is_some_and(|c| {
                c.iter()
                    .any(|p| whole.crates.iter().any(|w| package(w) == p))
            })
    });
    if !triggers(by_rows.as_ref()) && (by_rows.is_some() || !units_kept) {
        return Shaped::Whole(by_rows);
    }
    let whole_graph = planner.read(&whole);
    let estimate = by_rows.or_else(|| whole_graph.as_ref().ok().and_then(|g| unit_sum(book, g)));
    if !triggers(estimate.as_ref()) {
        return Shaped::Whole(estimate);
    }
    let Some(estimate) = estimate else {
        return Shaped::Whole(None);
    };
    let whole_claim = aterm_capacity::peak_claim(estimate.peak);
    let alone = |why: String| {
        vec![Call {
            part: Part::whole(
                whole.clone(),
                format!(
                    "by {} and a fifth more, at least the run's {} budget; not shaped: {why}",
                    estimate.from,
                    aterm_capacity::size(book.budget)
                ),
            ),
            mem: whole_claim,
        }]
    };
    let whole_graph = match whole_graph {
        Ok(g) => g,
        Err(e) => return Shaped::Calls(alone(format!("its unit graph could not be read: {e}"))),
    };

    let mut cuts: Vec<(Piece, Estimate, Graph)> = Vec::new();
    let mut cut_built = std::collections::BTreeSet::new();
    for p in planner.candidates(&rows, &whole_graph) {
        if cuts.len() >= MOST_CUTS {
            break;
        }
        if cuts
            .iter()
            .any(|(c, _, _)| c.crates.iter().any(|k| p.crates.contains(k)))
        {
            continue;
        }
        let after: Vec<Piece> = cuts.iter().map(|(c, _, _)| c.clone()).collect();
        if let Some((e, graph)) = planner.cut(&p, &after, &cut_built, &whole_graph) {
            cut_built.extend(graph.ids());
            cuts.push((p, e, graph));
        }
    }
    let one_crate = match cuts.first() {
        Some((p, _, _)) if p.crates.len() == 1 => Some(p.clone()),
        None if whole.crates.len() == 1 => Some(whole.clone()),
        _ => None,
    };
    if let Some(target) = one_crate
        && whole_graph.tests_lib_of(package(&target.crates[0]))
    {
        let lib = Piece::of(&target.crates, true);
        let empty = std::collections::BTreeSet::new();
        if let Some((e, lib_graph)) = planner.cut(&lib, &[], &empty, &whole_graph) {
            if target != whole {
                cuts.truncate(1);
                let after = [lib.clone()];
                let graph = &cuts[0].2;
                let floor = heaviest_unit(book, graph, &lib_graph.ids());
                if let Some(e) = planner
                    .estimate(&target, &after, graph)
                    .and_then(|e| largest([Some(e), floor]))
                {
                    cuts[0].1 = e;
                }
            }
            cuts.insert(0, (lib, e, lib_graph));
        }
    }
    if cuts.is_empty() {
        let mut why = std::mem::take(&mut planner.why_not);
        if why.is_empty() {
            why.push("no narrower selection of it, and no lib-test call, is known".to_owned());
        }
        why.truncate(3);
        let mut calls = alone(format!("no cut of it holds: {}", why.join("; ")));
        calls[0].part.refused = std::mem::take(&mut planner.refused);
        return Shaped::Calls(calls);
    }

    let mut pieces: Vec<Piece> = cuts.iter().map(|(p, _, _)| p.clone()).collect();
    pieces.push(whole.clone());
    let n = pieces.len();
    let mut built = std::collections::BTreeSet::new();
    let mut floors = Vec::with_capacity(cuts.len());
    for (_, _, graph) in &cuts {
        floors.push(heaviest_unit(book, graph, &built));
        built.extend(graph.ids());
    }
    let closing_floor = heaviest_unit(book, &whole_graph, &built);
    let closing_units = named_unit_sum(
        book,
        whole_graph
            .units
            .iter()
            .filter(|u| !built.contains(&u.id))
            .filter_map(|u| u.name.as_ref())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter(),
    )
    .filter(|e| aterm_capacity::peak_claim(e.peak) < whole_claim);
    let calls = in_order(&pieces)
        .into_iter()
        .map(|mut part| {
            let i = part.index;
            let whole_said = |why: &str| {
                format!(
                    "call {n} of {n}, the whole selection after the cuts, by {} and a fifth more, \
                     at least the run's {} budget: {why}",
                    estimate.from,
                    aterm_capacity::size(book.budget)
                )
            };
            let (mem, said, planned) = if let Some((_, e, graph)) = cuts.get(i) {
                let e = largest([Some(e.clone()), floors[i].clone()]).unwrap_or_else(|| e.clone());
                (
                    aterm_capacity::peak_claim(e.peak),
                    format!(
                        "call {} of {n} (`{}`), by {} and a fifth more; its {} units are units \
                         the whole call builds",
                        i + 1,
                        part.piece.label(),
                        e.from,
                        graph.ids().len()
                    ),
                    Some(e.peak),
                )
            } else {
                match (
                    kept(book, &whole, part.variant().as_deref()),
                    closing_floor.clone(),
                ) {
                    (Some(e), Some(floor)) => {
                        let peak = e.peak.max(floor.peak);
                        (
                            aterm_capacity::peak_claim(peak),
                            format!(
                                "call {n} of {n}, the whole selection after the cuts, by the \
                                 larger of {} and {}, and a fifth more",
                                e.from, floor.from
                            ),
                            Some(peak),
                        )
                    }
                    (None, _) => match &closing_units {
                        Some(e) => (
                            aterm_capacity::peak_claim(e.peak),
                            format!(
                                "call {n} of {n}, the whole selection after the cuts, by the \
                                 units no call before it builds, {} and a fifth more: no run of \
                                 it after them is kept",
                                e.from
                            ),
                            Some(e.peak),
                        ),
                        None => (
                            whole_claim,
                            whole_said("no run of it after them is kept"),
                            None,
                        ),
                    },
                    (Some(e), None) => (
                        whole_claim,
                        whole_said(&format!(
                            "no unit only it builds is kept, which would bound {} from below",
                            e.from
                        )),
                        None,
                    ),
                }
            };
            part.said = said;
            part.planned = planned;
            if part.closing() {
                part.refused.clone_from(&planner.refused);
            }
            Call { part, mem }
        })
        .collect();
    Shaped::Calls(calls)
}

/// One artifact a call's compile reported (`compiler-artifact`): the files it names, whether
/// cargo found it fresh or built it, its package and whether it is a test build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Artifact {
    /// Its `filenames`.
    pub filenames: Vec<String>,
    /// Its `fresh`.
    pub fresh: bool,
    /// Its package's name, read from its `package_id` ([`package_name`]).
    pub package: Option<String>,
    /// Its `profile.test`: a build of a test, which cargo makes only for a selected package (one of
    /// the workspace's own).
    pub test: bool,
}

/// The messages of a compile's `--message-format=json…` output whose `reason` is `reason`, in
/// order.
fn messages<'a>(output: &'a str, reason: &'a str) -> impl Iterator<Item = aterm_json::Value> + 'a {
    let quoted = format!("\"{reason}\"");
    output
        .lines()
        .filter(move |l| l.starts_with('{') && l.contains(&quoted))
        .filter_map(|l| aterm_json::from_str::<aterm_json::Value>(l).ok())
        .filter(move |v| v.get("reason").and_then(aterm_json::Value::as_str) == Some(reason))
}

/// The artifacts a compile's `--message-format=json…` output reports, in order; lines that are
/// not a `compiler-artifact` message are not read.
#[must_use]
pub fn artifacts(output: &str) -> Vec<Artifact> {
    messages(output, "compiler-artifact")
        .map(|v| Artifact {
            filenames: v
                .get("filenames")
                .and_then(aterm_json::Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(aterm_json::Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            fresh: v
                .get("fresh")
                .and_then(aterm_json::Value::as_bool)
                .unwrap_or(false),
            package: v
                .get("package_id")
                .and_then(aterm_json::Value::as_str)
                .and_then(package_name),
            test: v
                .get("profile")
                .and_then(|p| p.get("test"))
                .and_then(aterm_json::Value::as_bool)
                .unwrap_or(false),
        })
        .collect()
}

/// The `out_dir` of each build script a compile's `--message-format=json…` output says it ran
/// (`build-script-executed`), fresh or run again: the folder of that run's unit.
#[must_use]
pub fn script_runs(output: &str) -> Vec<String> {
    messages(output, "build-script-executed")
        .filter_map(|v| {
            v.get("out_dir")
                .and_then(aterm_json::Value::as_str)
                .map(str::to_owned)
        })
        .collect()
}

/// The package a cargo `package_id` names: `<source>#<name>@<version>`, `<source>#<version>` (a
/// source whose last path segment is the name) or, before cargo 1.77, `<name> <version>
/// (<source>)`; `None` for any other form.
#[must_use]
pub fn package_name(id: &str) -> Option<String> {
    if let Some((name, rest)) = id.split_once(' ') {
        return (!name.is_empty() && rest.contains(" (")).then(|| name.to_owned());
    }
    let (source, fragment) = id.rsplit_once('#')?;
    let name = match fragment.split_once('@') {
        Some((name, _)) => name,
        None => source
            .split_once('?')
            .map_or(source, |(path, _)| path)
            .rsplit('/')
            .next()?,
    };
    (!name.is_empty() && !name.contains(':')).then(|| name.to_owned())
}

/// A cut that passed, the files of the artifacts it built (not fresh), and when it began.
pub type Built = (
    Piece,
    std::collections::BTreeSet<String>,
    std::time::SystemTime,
);

/// What a run's shaped test compile has done so far ([`crate::Ctx::shape_run`]): the files each
/// cut built, what each call's time line says of them, and the units of the build directory
/// every call reported as its outputs.
#[derive(Debug, Default)]
pub struct Run {
    /// Each cut that passed ([`Built`]).
    pub built: Vec<Built>,
    /// What the fresh check said for each call, by its index.
    pub said: std::collections::BTreeMap<usize, String>,
    /// The units of the main lane's build directory each call's artifacts are outputs of, fresh
    /// or built, and the packages the calls built tests of ([`crate::disk::note_used`]): what a
    /// release run that passes keeps as the record the cache's bound reads
    /// ([`crate::disk::keep_used`], `atpkg::build_cache::stale::USED`).
    pub used: atpkg::build_cache::stale::Used,
}

/// THE FRESH CHECK: whether what the cuts `built` built came back fresh in a later call's
/// `artifacts`, as the unit-graph proof says it must. A cut is refused when the call rebuilt any
/// artifact naming a file it built (cargo found it stale: the proof missed what makes it another
/// unit), unless `changed` finds a source the call rebuilt from that changed after the cut began
/// ([`changed_since`]: an edit under the run, which says nothing of the proof); or, for the closing
/// call, when a file it built is in no artifact of the call's (the whole call does not build it).
/// Each refused cut and why; and a line for the time line.
#[must_use]
pub fn fresh_check(
    built: &[Built],
    artifacts: &[Artifact],
    closing: bool,
    changed: &dyn Fn(std::time::SystemTime) -> Option<String>,
) -> (Vec<(Piece, String)>, String) {
    let mut refused = Vec::new();
    let mut fresh = Vec::new();
    let mut edited = Vec::new();
    for (cut, files, began) in built {
        if files.is_empty() {
            continue;
        }
        let names = |a: &Artifact| a.filenames.iter().any(|f| files.contains(f));
        let rebuilt = artifacts.iter().filter(|a| !a.fresh && names(a)).count();
        let reported: std::collections::BTreeSet<&String> =
            artifacts.iter().flat_map(|a| a.filenames.iter()).collect();
        let missing = if closing {
            files.iter().filter(|f| !reported.contains(f)).count()
        } else {
            0
        };
        let edit = if rebuilt > 0 { changed(*began) } else { None };
        if let Some(source) = edit {
            edited.push(format!(
                "`{}` (it rebuilt {rebuilt} of its artifacts, and `{source}` changed after it \
                 began)",
                cut.label()
            ));
        } else if rebuilt > 0 {
            refused.push((
                cut.clone(),
                format!(
                    "it rebuilt {rebuilt} of the artifacts `{}` built",
                    cut.label()
                ),
            ));
        } else if missing > 0 {
            refused.push((
                cut.clone(),
                format!(
                    "{missing} of the {} files `{}` built are in none of its artifacts",
                    files.len(),
                    cut.label()
                ),
            ));
        } else if closing || artifacts.iter().any(names) {
            let n = files.len();
            fresh.push(format!(
                "`{}`'s {n} file{}",
                cut.label(),
                if n == 1 { "" } else { "s" }
            ));
        }
    }
    let mut said = Vec::new();
    if !fresh.is_empty() {
        said.push(format!("shape: fresh here: {}", fresh.join(", ")));
    }
    if !edited.is_empty() {
        said.push(format!(
            "shape: not refused, its sources changed under the run: {}",
            edited.join(", ")
        ));
    }
    for (cut, why) in &refused {
        said.push(format!(
            "shape: `{}` refused: {why}; no later run cuts it",
            cut.label()
        ));
    }
    (refused, said.join("; "))
}

/// Bound a dep-info file read even if it grows after `symlink_metadata`.
const DEP_INFO_BYTES: u64 = 16 << 20;

/// The dep-info file cargo keeps beside an artifact's `file` (`deps/libfoo-1a2b.rlib`,
/// `deps/foo-1a2b`): `deps/foo-1a2b.d`, rustc's list of the sources it compiled the unit from.
/// `None` where there is none.
fn dep_info_of(file: &std::path::Path) -> Option<std::path::PathBuf> {
    let name = file.file_name()?.to_str()?;
    let stem = [
        ".rlib", ".rmeta", ".dylib", ".so", ".a", ".dll", ".exe", ".wasm",
    ]
    .iter()
    .find_map(|ext| name.strip_suffix(ext))
    .unwrap_or(name);
    [stem.strip_prefix("lib"), Some(stem)]
        .into_iter()
        .flatten()
        .map(|s| file.with_file_name(format!("{s}.d")))
        .find(|d| std::fs::symlink_metadata(d).is_ok_and(|m| m.file_type().is_file()))
}

/// The sources a dep-info file's first rule lists (`out: a.rs b\ c.rs`), unescaped.
fn dep_info_sources(text: &str) -> Vec<String> {
    let Some((_, deps)) = text.lines().find_map(|l| l.split_once(": ")) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut word = String::new();
    let mut chars = deps.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => word.extend(chars.next()),
            ' ' => {
                if !word.is_empty() {
                    out.push(std::mem::take(&mut word));
                }
            }
            c => word.push(c),
        }
    }
    if !word.is_empty() {
        out.push(word);
    }
    out
}

/// A source one of `artifacts` that cargo rebuilt was compiled from, modified at or after
/// `since`, by its dep-info ([`dep_info_of`]): the first found, as the dep-info names it. Only a
/// source it names relative to the workspace root `root` counts, which is how rustc is handed a
/// workspace member's sources: an absolute one is a registry crate's or a build script's output
/// (`OUT_DIR`), which a cut's own build writes. `None` where no rebuilt artifact's sources
/// changed, or none could be read.
#[must_use]
pub fn changed_since(
    root: &std::path::Path,
    artifacts: &[Artifact],
    since: std::time::SystemTime,
) -> Option<String> {
    artifacts
        .iter()
        .filter(|a| !a.fresh)
        .flat_map(|a| a.filenames.iter())
        .filter_map(|f| dep_info_of(std::path::Path::new(f)))
        .filter_map(|d| crate::runs::read_bounded_regular_text(&d, DEP_INFO_BYTES).ok())
        .flat_map(|text| dep_info_sources(&text))
        .filter(|s| {
            let p = std::path::Path::new(s);
            p.is_relative()
                && !p
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
        })
        .find(|s| {
            std::fs::symlink_metadata(root.join(s))
                .ok()
                .filter(|m| m.file_type().is_file())
                .and_then(|m| m.modified().ok())
                .is_some_and(|m| m >= since)
        })
}

#[cfg(test)]
#[path = "shape_tests.rs"]
pub(crate) mod tests;
